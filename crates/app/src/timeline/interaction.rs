use super::*;

impl TimelineState {
    pub fn pointer_pressed(
        &mut self,
        snapshot: &LayoutSnapshot,
        point: [f32; 2],
        button: MouseButton,
        modifiers: ModifiersState,
    ) -> bool {
        self.cursor = point;
        let Some((stack, layout)) = Self::active_layout(snapshot, point) else {
            self.context_menu = None;
            return false;
        };
        self.focused_stack = Some(stack);
        self.snap_times.clear();

        if button == MouseButton::Left {
            if let Some(editor) = &self.keyframe_value_editor {
                if Self::keyframe_value_set_rect(layout, editor).contains(point) {
                    self.commit_keyframe_value_editor();
                    return true;
                }
                if Self::keyframe_value_editor_rect(layout, editor).contains(point) {
                    return true;
                }
                self.keyframe_value_editor = None;
                return true;
            }
            if let Some(editor) = &self.mixer_exact {
                if Self::mixer_exact_rect(layout, editor).contains(point) {
                    return true;
                }
                self.mixer_exact = None;
                return true;
            }
        }
        if button == MouseButton::Left && self.context_click(layout, point) {
            return true;
        }
        if button == MouseButton::Middle {
            self.context_menu = None;
            self.drag = Some(Drag::Pan {
                start: point,
                scroll_time: self.scroll_time,
                scroll_y: self.scroll_y,
            });
            return true;
        }
        if button == MouseButton::Right {
            self.context_menu = None;
            if let Some(track) = self.header_track_at(layout, point) {
                if self.tracks[track].kind == TrackKind::Audio {
                    for parameter in [MixerParameter::Volume, MixerParameter::Pan] {
                        if self
                            .mixer_knob_rect(layout, track, parameter)
                            .contains(point)
                        {
                            self.context_menu = Some(ContextMenu {
                                stack,
                                point: [point[0] - layout.rect.x, point[1] - layout.rect.y],
                                kind: ContextKind::Mixer {
                                    track: self.tracks[track].id,
                                    parameter,
                                },
                            });
                            return true;
                        }
                    }
                }
                self.context_menu = Some(ContextMenu {
                    stack,
                    point: [point[0] - layout.rect.x, point[1] - layout.rect.y],
                    kind: ContextKind::Track {
                        id: self.tracks[track].id,
                        kind: self.tracks[track].kind,
                    },
                });
                return true;
            }
            if layout.body.contains(point) {
                if let Some((lane, time)) = self.keyframe_at(layout, point) {
                    if !self.keyframe_is_selected(&lane, time) {
                        self.select_keyframe(lane, time, false);
                    }
                    self.context_menu = Some(ContextMenu {
                        stack,
                        point: [point[0] - layout.rect.x, point[1] - layout.rect.y],
                        kind: ContextKind::Keyframe,
                    });
                    return true;
                }
                let hit = self
                    .clip_at(layout, point)
                    .map(|(index, _)| self.clips[index].id);
                if let Some(id) = hit {
                    if !self.selected.contains(&id) {
                        self.selected_track = None;
                        self.select_clip(id, modifiers.shift_key());
                    }
                }
                self.drag = Some(Drag::BoxSelect {
                    start: point,
                    current: point,
                    additive: modifiers.shift_key(),
                    stack,
                    hit,
                });
                return true;
            }
            return true;
        }
        if button != MouseButton::Left {
            return true;
        }

        if self.rename.is_some() {
            self.commit_rename();
        }
        let toolbar_action = if Self::transport_button_rect(layout, 0).contains(point) {
            Some(TimelineAction::JumpTimelineStart)
        } else if Self::transport_button_rect(layout, 1).contains(point) {
            Some(TimelineAction::TogglePlayback)
        } else if Self::transport_button_rect(layout, 2).contains(point) {
            Some(TimelineAction::JumpTimelineEnd)
        } else if Self::transport_button_rect(layout, 3).contains(point) {
            Some(TimelineAction::ToggleEndBehavior)
        } else if layout.frame_snap_button.contains(point) {
            Some(TimelineAction::ToggleFrameSnap)
        } else if layout.grid_snap_button.contains(point) {
            Some(TimelineAction::ToggleGridSnap)
        } else if layout.clip_snap_button.contains(point) {
            Some(TimelineAction::ToggleClipSnap)
        } else if layout.playhead_snap_button.contains(point) {
            Some(TimelineAction::TogglePlayheadSnap)
        } else if layout.razor_tool_button.contains(point) {
            Some(TimelineAction::ToggleRazorTool)
        } else if layout.follow_playhead_button.contains(point) {
            Some(TimelineAction::ToggleFollowPlayhead)
        } else {
            None
        };
        if let Some(action) = toolbar_action {
            self.pending_action = Some(action);
            return true;
        }
        if layout.overview_body.contains(point) {
            let window = self.overview_window(layout);
            let left = (point[0] - window.x).abs();
            let right = (point[0] - window.right()).abs();
            let part = if left <= 7.0 && left <= right {
                OverviewPart::Left
            } else if right <= 7.0 {
                OverviewPart::Right
            } else {
                if !window.contains(point) {
                    let visible = self.visible_duration(layout);
                    let body = layout.overview_body;
                    let time = ((point[0] - body.x) / body.width.max(1.0)).clamp(0.0, 1.0) as f64
                        * self.overview_duration(layout);
                    self.scroll_time = (time - visible * 0.5).max(0.0);
                }
                OverviewPart::Body
            };
            self.drag = Some(Drag::Overview {
                part,
                start_x: point[0],
                scroll_time: self.scroll_time,
                pixels_per_second: self.pixels_per_second,
            });
            return true;
        }
        if let Some(track) = self.header_track_at(layout, point) {
            let track_id = self.tracks[track].id;
            if self
                .keyframe_track_toggle_rect(layout, track)
                .contains(point)
            {
                if !self.keyframe_lanes_for_track(track).is_empty() {
                    let was_expanded = self.expanded_keyframe_tracks.contains(&track_id);
                    self.keyframe_track_expansion
                        .entry(track_id)
                        .or_insert(was_expanded as u8 as f32);
                    if !self.expanded_keyframe_tracks.insert(track_id) {
                        self.expanded_keyframe_tracks.remove(&track_id);
                    }
                    self.scroll_y = self.clamp_scroll(self.scroll_y, layout);
                }
                return true;
            }
            for (start, _, rect) in self.keyframe_property_rects(layout, track, true) {
                if Self::keyframe_lane_toggle_rect(rect).contains(point) {
                    let target = self.keyframe_lanes_for_track(track)[start].id.group.clone();
                    let was_expanded = self.expanded_keyframe_lanes.contains(&target);
                    self.keyframe_lane_expansion
                        .entry(target.clone())
                        .or_insert(was_expanded as u8 as f32);
                    if !self.expanded_keyframe_lanes.insert(target.clone()) {
                        self.expanded_keyframe_lanes.remove(&target);
                    }
                    self.scroll_y = self.clamp_scroll(self.scroll_y, layout);
                    return true;
                }
            }
            self.selected_track = Some(track_id);
            if !modifiers.shift_key() {
                self.selected.clear();
                self.primary_selected = None;
            }
            let local_x = point[0] - layout.rect.x;
            let row_y = self.track_y(layout, track);
            let local_y = point[1] - row_y;
            if self.tracks[track].kind == TrackKind::Audio {
                for parameter in [MixerParameter::Volume, MixerParameter::Pan] {
                    let rect = self.mixer_knob_rect(layout, track, parameter);
                    if rect.contains(point) {
                        let id = self.tracks[track].id;
                        let reset = self
                            .mixer_knobs
                            .get_mut(&(id, parameter))
                            .and_then(|knob| knob.pointer_pressed(rect, point));
                        if let Some(value) = reset {
                            self.set_track_mix(id, parameter, value as f32);
                        }
                        return true;
                    }
                }
            }
            if local_x < TRACK_HEADER_PAD + 1.0 + TRACK_HANDLE_W {
                let heights = self
                    .tracks
                    .iter()
                    .enumerate()
                    .map(|(index, track)| (track.id, self.display_track_height(index)))
                    .collect();
                self.drag = Some(Drag::Track {
                    id: self.tracks[track].id,
                    grab_y: local_y,
                    current_y: point[1],
                    origin_y: layout.rect.y,
                    heights,
                });
            } else if self
                .header_button_rect(layout, track, false)
                .contains(point)
            {
                self.pending_action = Some(TimelineAction::ToggleTrackMute(self.tracks[track].id));
            } else if self.header_button_rect(layout, track, true).contains(point) {
                self.pending_action = Some(TimelineAction::ToggleTrackSolo(self.tracks[track].id));
            } else if self.header_name_rect(layout, track).contains(point) {
                let id = self.tracks[track].id;
                let now = Instant::now();
                if self
                    .last_track_click
                    .is_some_and(|(last, at)| last == id && now.duration_since(at) <= DOUBLE_CLICK)
                {
                    self.pending_action = Some(TimelineAction::RenameTrack(id));
                    self.last_track_click = None;
                } else {
                    self.last_track_click = Some((id, now));
                }
            }
            return true;
        }
        if layout.corner.contains(point) {
            return true;
        }
        if layout.ruler.contains(point) {
            self.set_dragged_playhead(layout, self.time_at(layout, point[0]));
            self.drag = Some(Drag::Playhead);
            return true;
        }
        if !layout.body.contains(point) {
            return true;
        }

        if let Some(easing) = self.keyframe_easing_at(layout, point) {
            self.drag = Some(Drag::KeyframeEase(easing));
            return true;
        }
        if let Some((lane, key_time)) = self.keyframe_at(layout, point) {
            self.select_keyframe(lane, key_time, modifiers.shift_key());
            let points = self.begin_keyframe_drag(layout, point);
            if !points.is_empty() {
                self.drag = Some(Drag::Keyframe {
                    points,
                    start: point,
                });
            }
            return true;
        }
        if let Some((index, rect)) = self.clip_at(layout, point) {
            let left = point[0] - rect.x <= EDGE_W;
            let right = rect.right() - point[0] <= EDGE_W;
            let (id, anchor_start, anchor_track, edge_origin) = {
                let clip = &self.clips[index];
                (
                    clip.id,
                    clip.start,
                    clip.track,
                    ClipEdgeOrigin {
                        start: clip.start,
                        duration: clip.duration,
                        source_offset: clip.source_offset,
                        speed: clip.speed,
                    },
                )
            };
            if !modifiers.shift_key() && !self.selected.contains(&id) {
                self.selected_keyframes.clear();
            }
            if self.tool == TimelineTool::Razor {
                let raw = self.time_at(layout, point[0]);
                let snapped = self.insertion_snap_time(layout, raw);
                let time = if self.frame_snap {
                    (snapped * self.frame_rate).round() / self.frame_rate
                } else {
                    snapped
                };
                self.pending_action = Some(TimelineAction::CutClipAt { clip: id, time });
                return true;
            }
            if rect.width >= 30.0 && (left || right) {
                if !self.selected.contains(&id) {
                    self.selected_track = None;
                    self.select_clip(id, modifiers.shift_key());
                } else {
                    self.primary_selected = Some(id);
                }
                self.drag = Some(Drag::ClipEdge {
                    id,
                    left,
                    rate_stretch: modifiers.shift_key(),
                    origin: edge_origin,
                });
                return true;
            }

            self.selected_track = None;
            let shift_toggle_on_click =
                (modifiers.shift_key() && self.selected.contains(&id)).then_some(id);
            let collapse_selection_on_click = (!modifiers.shift_key() && !modifiers.alt_key())
                .then(|| self.multi_selection_click_target(id))
                .flatten();
            if shift_toggle_on_click.is_none() {
                self.select_clip(id, modifiers.shift_key());
            } else {
                self.primary_selected = Some(id);
            }
            if modifiers.alt_key() {
                self.duplicate_selection_for_drag();
            }
            let shift_adjust = modifiers.shift_key() && !modifiers.alt_key();
            let grabbed_is_audio =
                self.clips
                    .iter()
                    .find(|clip| clip.id == id)
                    .is_some_and(|clip| {
                        clip.source.is_audio()
                            || self
                                .tracks
                                .iter()
                                .find(|track| track.id == clip.track)
                                .is_some_and(|track| track.kind == TrackKind::Audio)
                    });
            if !self.selected.is_empty() {
                let mut origins = Vec::with_capacity(self.selected.len());
                let mut snap_points = vec![self.playhead];
                for (index, clip) in self.clips.iter().enumerate() {
                    if self.selected.contains(&clip.id) {
                        origins.push(ClipOrigin {
                            index,
                            id: clip.id,
                            start: clip.start,
                            duration: clip.duration,
                            track: clip.track,
                            source_offset: clip.source_offset,
                            opacity: clip.opacity,
                            volume: clip.volume,
                        });
                    } else if self.clip_snap {
                        snap_points.extend([clip.start, clip.end()]);
                    }
                }
                snap_points.sort_unstable_by(f32::total_cmp);
                snap_points.dedup_by(|a, b| a.total_cmp(b).is_eq());
                self.drag = Some(Drag::Clips {
                    anchor_track,
                    anchor_start,
                    start: point,
                    origins,
                    snap_points,
                    keyframes: self.begin_keyframe_drag(layout, point),
                    shift_adjust: shift_adjust.then_some(ShiftClipAdjustAxis::Pending),
                    shift_adjust_audio: grabbed_is_audio,
                    shift_adjust_anchor: id,
                    duplicated: modifiers.alt_key(),
                    shift_toggle_on_click,
                    collapse_selection_on_click,
                    preview_tracks: Vec::new(),
                });
            }
        } else {
            if !modifiers.shift_key() {
                self.selected.clear();
                self.primary_selected = None;
                self.selected_track = None;
                self.selected_keyframes.clear();
            }
            self.set_playhead(self.time_at(layout, point[0]));
        }
        true
    }

    pub fn pointer_moved(
        &mut self,
        snapshot: &LayoutSnapshot,
        point: [f32; 2],
        modifiers: ModifiersState,
        project: &Project,
    ) -> bool {
        if let Some((&(track, parameter), knob)) = self
            .mixer_knobs
            .iter_mut()
            .find(|(_, knob)| knob.is_dragging())
        {
            let changed = knob.pointer_moved(point);
            if let Some(value) = changed {
                self.set_track_mix(track, parameter, value as f32);
            }
            self.cursor = point;
            return true;
        }
        self.cursor = point;
        let Some(mut drag) = self.drag.take() else {
            return false;
        };
        let Some(layout) = self.focused_layout(snapshot) else {
            self.drag = Some(drag);
            return false;
        };
        match &mut drag {
            Drag::Pan {
                start,
                scroll_time,
                scroll_y,
            } => {
                self.scroll_time = (*scroll_time
                    - (point[0] - start[0]) as f64 / self.pixels_per_second as f64)
                    .max(0.0);
                self.scroll_y = self.clamp_scroll(*scroll_y - (point[1] - start[1]), layout);
            }
            Drag::BoxSelect { current, .. } => *current = point,
            Drag::Playhead => {
                let x = point[0].clamp(layout.body.x, layout.body.right());
                self.set_dragged_playhead(layout, self.time_at(layout, x));
            }
            Drag::ClipEdge {
                id,
                left,
                rate_stretch,
                origin,
            } => {
                *rate_stretch = modifiers.shift_key();
                self.resize_clip(layout, *id, *left, point[0], *rate_stretch, *origin);
            }
            Drag::Clips {
                anchor_track,
                anchor_start,
                start,
                origins,
                snap_points,
                keyframes,
                shift_adjust,
                shift_adjust_audio,
                shift_adjust_anchor,
                duplicated: _,
                shift_toggle_on_click: _,
                collapse_selection_on_click: _,
                preview_tracks,
            } => {
                let dx = point[0] - start[0];
                let dy = point[1] - start[1];
                if let Some(axis) = shift_adjust {
                    if matches!(*axis, ShiftClipAdjustAxis::Pending)
                        && dx.abs().max(dy.abs()) >= CLIP_DRAG_THRESHOLD_PX
                    {
                        *axis = if dx.abs() >= dy.abs() {
                            ShiftClipAdjustAxis::Horizontal
                        } else {
                            ShiftClipAdjustAxis::Vertical
                        };
                    }

                    match *axis {
                        ShiftClipAdjustAxis::Pending => {}
                        ShiftClipAdjustAxis::Horizontal => {
                            let mut offset_delta = -dx / self.pixels_per_second;

                            if self.clip_snap {
                                if let Some(origin) = origins
                                    .iter()
                                    .find(|origin| origin.id == *shift_adjust_anchor)
                                {
                                    if let Some(clip) = self
                                        .clips
                                        .get(origin.index)
                                        .filter(|clip| clip.id == origin.id)
                                    {
                                        let speed = clip.speed.max(0.01);
                                        let raw_offset =
                                            origin.source_offset + offset_delta * speed;
                                        let snap_distance =
                                            SNAP_PX / self.pixels_per_second * speed;
                                        let mut snap_targets = vec![0.0];
                                        if let Some(source_duration) =
                                            clip_source_duration(project, &clip.source)
                                        {
                                            snap_targets.push(
                                                source_duration - origin.duration.max(0.0) * speed,
                                            );
                                        }
                                        if let Some(target) =
                                            snap_targets.into_iter().min_by(|left, right| {
                                                (raw_offset - *left)
                                                    .abs()
                                                    .total_cmp(&(raw_offset - *right).abs())
                                            })
                                        {
                                            if (raw_offset - target).abs() <= snap_distance {
                                                offset_delta =
                                                    (target - origin.source_offset) / speed;
                                            }
                                        }
                                    }
                                }
                            }

                            for origin in origins.iter() {
                                let Some(clip) = self
                                    .clips
                                    .get_mut(origin.index)
                                    .filter(|clip| clip.id == origin.id)
                                else {
                                    continue;
                                };
                                clip.source_offset =
                                    origin.source_offset + offset_delta * clip.speed.max(0.01);
                            }
                        }
                        ShiftClipAdjustAxis::Vertical => {
                            let level_delta = -dy / 200.0;
                            for origin in origins.iter() {
                                let Some(clip) = self
                                    .clips
                                    .get(origin.index)
                                    .filter(|clip| clip.id == origin.id)
                                else {
                                    continue;
                                };
                                let clip_is_audio = clip.source.is_audio()
                                    || self
                                        .tracks
                                        .iter()
                                        .find(|track| track.id == clip.track)
                                        .is_some_and(|track| track.kind == TrackKind::Audio);
                                if clip_is_audio != *shift_adjust_audio {
                                    continue;
                                }
                                let Some(clip) = self
                                    .clips
                                    .get_mut(origin.index)
                                    .filter(|clip| clip.id == origin.id)
                                else {
                                    continue;
                                };
                                if clip_is_audio {
                                    clip.volume = (origin.volume + level_delta).clamp(0.0, 1.0);
                                } else {
                                    clip.opacity = (origin.opacity + level_delta).clamp(0.0, 1.0);
                                }
                            }
                        }
                    }
                } else {
                    let delta_time = self.move_clips(
                        layout,
                        ClipMoveAnchor {
                            track: *anchor_track,
                            time: *anchor_start,
                            pointer: *start,
                        },
                        origins,
                        snap_points,
                        preview_tracks,
                        point,
                    );
                    self.move_keyframes_horizontal(keyframes, delta_time);
                }
            }
            Drag::Overview {
                part,
                start_x,
                scroll_time,
                pixels_per_second,
            } => {
                self.move_overview(
                    layout,
                    *part,
                    point[0] - *start_x,
                    *scroll_time,
                    *pixels_per_second,
                );
            }
            Drag::Keyframe { points, start } => {
                let mut dx = point[0] - start[0];
                let mut dy = point[1] - start[1];
                let can_move_vertically = points.iter().any(|key| key.vertical);

                if can_move_vertically && dx.abs().max(dy.abs()) >= CLIP_DRAG_THRESHOLD_PX {
                    if dx.abs() >= dy.abs() * KEYFRAME_AXIS_LOCK_RATIO {
                        dy = 0.0;
                    } else if dy.abs() >= dx.abs() * KEYFRAME_AXIS_LOCK_RATIO {
                        dx = 0.0;
                    }
                }

                let delta_time = dx as f64 / self.pixels_per_second as f64;
                let delta_y = can_move_vertically.then_some(dy);
                self.move_keyframes(points, delta_time, delta_y, true);
            }
            Drag::KeyframeEase(easing) => {
                self.update_keyframe_easing_drag(layout, easing, point);
            }
            Drag::Track {
                id,
                current_y,
                heights,
                ..
            } => {
                let target = self.track_target_cached(layout, point[1], heights);
                self.reorder_track_cached(*id, target, heights);
                *current_y = point[1];
            }
        }
        self.drag = Some(drag);
        true
    }

    pub fn pointer_released(
        &mut self,
        snapshot: &LayoutSnapshot,
        point: [f32; 2],
        button: MouseButton,
        modifiers: ModifiersState,
        project: &Project,
    ) -> bool {
        if button == MouseButton::Left {
            if let Some(knob) = self
                .mixer_knobs
                .values_mut()
                .find(|knob| knob.is_dragging())
            {
                knob.pointer_released();
                return true;
            }
        }
        let relevant = matches!(
            (&self.drag, button),
            (Some(Drag::Pan { .. }), MouseButton::Middle)
                | (Some(Drag::BoxSelect { .. }), MouseButton::Right)
                | (
                    Some(
                        Drag::Clips { .. }
                            | Drag::ClipEdge { .. }
                            | Drag::Playhead
                            | Drag::Overview { .. }
                            | Drag::Keyframe { .. }
                            | Drag::KeyframeEase(_)
                            | Drag::Track { .. }
                    ),
                    MouseButton::Left
                )
        );
        if !relevant {
            return false;
        }
        match self.drag.take() {
            Some(Drag::BoxSelect {
                start,
                current,
                additive,
                stack,
                hit,
            }) => {
                if let Some(layout) = self.focused_layout(snapshot) {
                    let dx = current[0] - start[0];
                    let dy = current[1] - start[1];
                    if dx * dx + dy * dy > 16.0 {
                        let area = normalized_rect(start, current);
                        let hits: Vec<_> = self
                            .clips
                            .iter()
                            .filter(|clip| intersects(self.clip_rect(layout, clip), area))
                            .map(|clip| clip.id)
                            .collect();
                        let mut keyframe_hits = Vec::new();
                        for track_index in 0..self.tracks.len() {
                            let lanes = self.keyframe_lanes_for_track(track_index);
                            for (lane_start, lane_end, rect) in
                                self.keyframe_property_rects(layout, track_index, false)
                            {
                                if !intersects(rect, area) {
                                    continue;
                                }
                                let expanded =
                                    self.keyframe_property_graph_is_settled(&lanes[lane_start].id);
                                let axis = expanded.then(|| {
                                    keyframe_property_axis_range(
                                        &lanes[lane_start..lane_end],
                                        rect.height,
                                    )
                                });
                                for lane in &lanes[lane_start..lane_end] {
                                    for key in &lane.points {
                                        let key_point = [
                                            self.time_x(layout, key.time as f32),
                                            keyframe_value_y(rect, axis, key.value),
                                        ];
                                        if area.contains(key_point) {
                                            keyframe_hits.push(SelectedKeyframe {
                                                lane: lane.id.clone(),
                                                time: key.time,
                                            });
                                        }
                                    }
                                }
                            }
                        }
                        if !additive {
                            self.selected.clear();
                            self.primary_selected = None;
                            self.selected_keyframes.clear();
                        }
                        let primary_hit = hits.first().copied();
                        for id in hits {
                            let grouped = self.grouped_ids(id);
                            self.selected.extend(grouped);
                        }
                        if let Some(id) = primary_hit {
                            self.primary_selected = Some(id);
                        }
                        for hit in keyframe_hits {
                            if !self.keyframe_is_selected(&hit.lane, hit.time) {
                                self.selected_keyframes.push(hit);
                            }
                        }
                    } else {
                        let kind = if hit.is_some() {
                            ContextKind::Selection
                        } else {
                            {
                                let track = self.track_at(layout, start[1]);
                                ContextKind::Empty {
                                    time: self.time_at(layout, start[0]),
                                    kind: track.and_then(|index| {
                                        self.tracks.get(index).map(|track| track.kind)
                                    }),
                                    track,
                                }
                            }
                        };
                        self.context_menu = Some(ContextMenu {
                            stack,
                            point: [start[0] - layout.rect.x, start[1] - layout.rect.y],
                            kind,
                        });
                    }
                }
            }
            Some(Drag::Track {
                id,
                grab_y,
                current_y,
                ..
            }) => {
                if let Some(layout) = self.focused_layout(snapshot) {
                    if let Some(index) = self.track_index(id) {
                        let offset = current_y - grab_y - self.track_base_y(layout, index);
                        if offset.abs() > 0.25 {
                            self.track_offsets.insert(id, offset);
                        }
                    }
                }
            }
            Some(Drag::Clips {
                start,
                shift_toggle_on_click: Some(id),
                duplicated: false,
                ..
            }) if {
                let dx = point[0] - start[0];
                let dy = point[1] - start[1];
                dx * dx + dy * dy < CLIP_DRAG_THRESHOLD_PX * CLIP_DRAG_THRESHOLD_PX
            } =>
            {
                self.select_clip(id, true);
            }
            Some(Drag::Clips {
                start,
                collapse_selection_on_click: Some(id),
                ..
            }) if {
                let dx = point[0] - start[0];
                let dy = point[1] - start[1];
                dx * dx + dy * dy < CLIP_DRAG_THRESHOLD_PX * CLIP_DRAG_THRESHOLD_PX
            } =>
            {
                self.selected_track = None;
                self.replace_selection_with_clip(id);
            }
            drag => {
                let moved = match &drag {
                    Some(Drag::Clips { origins, .. }) => origins
                        .iter()
                        .map(|origin| (origin.id, origin.track))
                        .collect::<Vec<_>>(),
                    _ => Vec::new(),
                };
                self.drag = drag;
                self.pointer_moved(snapshot, point, modifiers, project);
                self.drag = None;
                let moved_any = !moved.is_empty();
                for (id, previous_track) in moved {
                    let _ = self.ensure_property_row_for_moved_clip(id, previous_track);
                }
                if moved_any {
                    self.prune_unused_property_rows();
                }
            }
        }
        self.snap_times.clear();
        true
    }

    pub fn scroll(
        &mut self,
        snapshot: &LayoutSnapshot,
        point: [f32; 2],
        delta: [f32; 2],
        modifiers: ModifiersState,
    ) -> bool {
        let Some((stack, layout)) = Self::active_layout(snapshot, point) else {
            return false;
        };
        self.focused_stack = Some(stack);
        let vertical = delta[1];
        let horizontal = delta[0];
        let amount = if vertical.abs() >= horizontal.abs() {
            vertical
        } else {
            horizontal
        };
        if modifiers.control_key() {
            let change = amount.signum() * 4.0;
            if modifiers.shift_key() {
                if let Some(track) = self
                    .clips
                    .iter()
                    .find(|clip| self.selected.contains(&clip.id))
                    .and_then(|clip| clip.track_index(&self.tracks))
                    .or_else(|| self.track_at(layout, point[1]))
                {
                    self.tracks[track].height = (self.tracks[track].height + change).max(TRACK_MIN);
                }
            } else {
                for track in &mut self.tracks {
                    track.height = (track.height + change).max(TRACK_MIN);
                }
            }
            self.scroll_y = self.clamp_scroll(self.scroll_y, layout);
        } else if modifiers.shift_key() && modifiers.alt_key() {
            self.scroll_y = self.clamp_scroll(self.scroll_y - amount, layout);
        } else if modifiers.shift_key() {
            self.scroll_time =
                (self.scroll_time - amount as f64 / self.pixels_per_second as f64).max(0.0);
        } else if point[0] < layout.body.x
            && point[1] >= layout.body.y
            && point[1] < layout.body.bottom()
        {
            self.scroll_y = self.clamp_scroll(self.scroll_y - amount, layout);
        } else if horizontal.abs() > vertical.abs() {
            self.scroll_time =
                (self.scroll_time - horizontal as f64 / self.pixels_per_second as f64).max(0.0);
        } else if vertical.abs() > 0.01
            && (layout.body.contains(point) || layout.ruler.contains(point))
        {
            let pixels_per_second =
                (self.pixels_per_second * (vertical * 0.0025).exp()).min(MAX_PIXELS_PER_SECOND);
            if pixels_per_second.is_finite()
                && pixels_per_second > 0.0
                && (layout.body.width / pixels_per_second).is_finite()
            {
                self.pixels_per_second = pixels_per_second;
                self.scroll_time = (self.playhead as f64
                    - layout.body.width as f64 / pixels_per_second as f64 * 0.5)
                    .max(0.0);
            }
        }
        true
    }

    fn cycle_hover_target(&mut self, layout: TimelineLayout, direction: i32) -> bool {
        if !layout.body.contains(self.cursor) {
            return false;
        }
        let mut targets = self
            .keyframes_at(layout, self.cursor)
            .into_iter()
            .map(|(lane, time)| TimelineHoverTarget::Keyframe(lane, time))
            .collect::<Vec<_>>();
        targets.extend(
            self.clips_at(layout, self.cursor)
                .into_iter()
                .map(|(index, _)| TimelineHoverTarget::Clip(self.clips[index].id)),
        );
        if targets.len() < 2 {
            return false;
        }
        let current = targets.iter().position(|target| match target {
            TimelineHoverTarget::Keyframe(lane, time) => self.keyframe_is_selected(lane, *time),
            TimelineHoverTarget::Clip(id) => self.primary_selected == Some(*id),
        });
        let len = targets.len() as i32;
        let next = current.map_or_else(
            || if direction < 0 { targets.len() - 1 } else { 0 },
            |index| (index as i32 + direction).rem_euclid(len) as usize,
        );
        match targets[next].clone() {
            TimelineHoverTarget::Keyframe(lane, time) => {
                self.selected.clear();
                self.primary_selected = None;
                self.selected_track = None;
                self.selected_keyframes.clear();
                self.select_keyframe(lane, time, false);
            }
            TimelineHoverTarget::Clip(id) => {
                self.selected_keyframes.clear();
                self.select_clip(id, false);
            }
        }
        true
    }

    pub fn handle_key(
        &mut self,
        snapshot: &LayoutSnapshot,
        event: &KeyEvent,
        _modifiers: ModifiersState,
    ) -> bool {
        if self.focused_stack.is_none() || event.state != ElementState::Pressed {
            return false;
        }
        if self.keyframe_value_editor.is_none()
            && self.mixer_exact.is_none()
            && self.rename.is_none()
        {
            if let Some(layout) = self.focused_layout(snapshot) {
                match event.logical_key {
                    Key::Named(NamedKey::ArrowUp) if self.cycle_hover_target(layout, -1) => {
                        return true;
                    }
                    Key::Named(NamedKey::ArrowDown) if self.cycle_hover_target(layout, 1) => {
                        return true;
                    }
                    _ => {}
                }
            }
        }
        if self.keyframe_value_editor.is_some() {
            match &event.logical_key {
                Key::Named(NamedKey::Escape) => self.keyframe_value_editor = None,
                Key::Named(NamedKey::Enter) => self.commit_keyframe_value_editor(),
                Key::Named(NamedKey::Backspace) => {
                    if let Some(editor) = &mut self.keyframe_value_editor {
                        if editor.replace_on_input {
                            editor.value.clear();
                            editor.replace_on_input = false;
                        } else {
                            editor.value.pop();
                        }
                    }
                }
                _ => {
                    if let Some(text) = event.text.as_deref().filter(|text| {
                        text.chars()
                            .all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E'))
                    }) {
                        if let Some(editor) = &mut self.keyframe_value_editor {
                            if editor.replace_on_input {
                                editor.value.clear();
                                editor.replace_on_input = false;
                            }
                            editor.value.push_str(text);
                        }
                    }
                }
            }
            return true;
        }
        if self.mixer_exact.is_some() {
            match &event.logical_key {
                Key::Named(NamedKey::Escape) => self.mixer_exact = None,
                Key::Named(NamedKey::Enter) => self.commit_mixer_exact(),
                Key::Named(NamedKey::Backspace) => {
                    if let Some(editor) = &mut self.mixer_exact {
                        if editor.replace_on_input {
                            editor.value.clear();
                            editor.replace_on_input = false;
                        } else {
                            editor.value.pop();
                        }
                    }
                }
                _ => {
                    if let Some(text) = event.text.as_deref().filter(|text| {
                        text.chars()
                            .all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+'))
                    }) {
                        if let Some(editor) = &mut self.mixer_exact {
                            if editor.replace_on_input {
                                editor.value.clear();
                                editor.replace_on_input = false;
                            }
                            editor.value.push_str(text);
                        }
                    }
                }
            }
            return true;
        }
        if self.rename.is_some() {
            match &event.logical_key {
                Key::Named(NamedKey::Escape) => self.rename = None,
                Key::Named(NamedKey::Enter) => self.commit_rename(),
                Key::Named(NamedKey::Backspace) => {
                    if let Some(rename) = self.rename.as_mut() {
                        rename.value.pop();
                    }
                }
                _ => {
                    if let Some(text) = event.text.as_deref() {
                        if text.chars().all(|character| !character.is_control()) {
                            if let Some(rename) = self.rename.as_mut() {
                                rename.value.push_str(text);
                            }
                        }
                    }
                }
            }
            return true;
        }
        if matches!(event.logical_key, Key::Named(NamedKey::Escape))
            && self.context_menu.take().is_some()
        {
            return true;
        }
        false
    }

    pub(super) fn begin_rename(&mut self, track: u32) {
        if let Some(track) = self.tracks.iter().find(|candidate| candidate.id == track) {
            self.rename = Some(RenameState {
                track: track.id,
                value: track.name.clone(),
            });
        }
    }

    fn commit_rename(&mut self) {
        let Some(rename) = self.rename.take() else {
            return;
        };
        let value = rename.value.trim();
        if value.is_empty() {
            return;
        }
        if let Some(track) = self
            .tracks
            .iter_mut()
            .find(|track| track.id == rename.track)
        {
            track.name = value.to_string();
        }
    }

    pub(super) fn execute_context(&mut self, kind: ContextKind, point: [f32; 2], index: usize) {
        let Some(command) = context_items(kind).get(index).map(|item| item.3) else {
            return;
        };
        match command {
            ContextCommand::CopySelection => {
                self.pending_action = Some(TimelineAction::CopySelection)
            }
            ContextCommand::CutSelection => {
                self.pending_action = Some(TimelineAction::CutSelection)
            }
            ContextCommand::Paste => self.pending_action = Some(TimelineAction::Paste),
            ContextCommand::Group => self.pending_action = Some(TimelineAction::GroupSelection),
            ContextCommand::Ungroup => self.pending_action = Some(TimelineAction::UngroupSelection),
            ContextCommand::CloseGap => self.pending_action = Some(TimelineAction::CloseGap),
            ContextCommand::SpeedDuration => {
                self.pending_action = Some(TimelineAction::SpeedDuration)
            }
            ContextCommand::ReplaceSelectedClips => {
                self.pending_action = Some(TimelineAction::ReplaceSelectedClips)
            }
            ContextCommand::DeleteSelection => {
                self.pending_action = Some(TimelineAction::DeleteSelection)
            }
            ContextCommand::EditKeyframeValue => {
                if let (Some(stack), Some(value)) =
                    (self.focused_stack, self.selected_keyframe_value())
                {
                    self.keyframe_value_editor = Some(KeyframeValueEditor {
                        stack,
                        point,
                        value: format_keyframe_value(value),
                        replace_on_input: true,
                    });
                }
            }
            ContextCommand::SetKeyframeInterpolation(interpolation) => {
                self.set_selected_keyframe_interpolation(interpolation);
            }
            ContextCommand::DeleteKeyframes => self.delete_selected_keyframes(),
            ContextCommand::AddSelectionToComposition => {
                self.pending_action = Some(TimelineAction::AddSelectionToComposition)
            }
            ContextCommand::SetEnd => self.pending_action = Some(TimelineAction::SetEnd),
            ContextCommand::InsertVideoHere => {
                let target = match kind {
                    ContextKind::Track { id, .. } => Some((id, self.playhead)),
                    ContextKind::Empty { time, track, .. } => track
                        .and_then(|index| self.tracks.get(index))
                        .map(|track| (track.id, time)),
                    ContextKind::Selection | ContextKind::Mixer { .. } | ContextKind::Keyframe => {
                        None
                    }
                };
                if let Some((track, time)) = target {
                    self.pending_action = Some(TimelineAction::InsertVideoClip { track, time });
                }
            }
            ContextCommand::InsertVideoFirst => {
                let ContextKind::Empty { time, .. } = kind else {
                    return;
                };
                if let Some(track) = self
                    .tracks
                    .iter()
                    .find(|track| track.kind == TrackKind::Video)
                    .map(|track| track.id)
                {
                    self.pending_action = Some(TimelineAction::InsertVideoClip { track, time });
                }
            }
            ContextCommand::InsertAudio => {
                if let ContextKind::Empty { time, track, .. } = kind {
                    self.pending_action = Some(TimelineAction::InsertAudio { time, near: track });
                }
            }
            ContextCommand::InsertEffectHere => {
                let target = match kind {
                    ContextKind::Track { id, .. } => Some((id, self.playhead)),
                    ContextKind::Empty { time, track, .. } => track
                        .and_then(|index| self.tracks.get(index))
                        .map(|track| (track.id, time)),
                    ContextKind::Selection | ContextKind::Mixer { .. } | ContextKind::Keyframe => {
                        None
                    }
                };
                if let Some((track, time)) = target {
                    self.pending_action = Some(TimelineAction::InsertEffectClip { track, time });
                }
            }
            ContextCommand::RenameTrack => {
                if let ContextKind::Track { id, .. } = kind {
                    self.pending_action = Some(TimelineAction::RenameTrack(id));
                }
            }
            ContextCommand::DeleteTrack => {
                if let ContextKind::Track { id, .. } = kind {
                    self.pending_action = Some(TimelineAction::DeleteTrack(id));
                }
            }
            ContextCommand::AddTrack(track_kind) => {
                let near = match kind {
                    ContextKind::Empty { track, .. } => track,
                    ContextKind::Track { id, .. } => self.track_index(id),
                    ContextKind::Selection | ContextKind::Mixer { .. } | ContextKind::Keyframe => {
                        None
                    }
                };
                self.pending_action = Some(TimelineAction::AddTrack {
                    kind: track_kind,
                    near,
                });
            }
            ContextCommand::SetExactMixer => {
                if let ContextKind::Mixer { track, parameter } = kind {
                    self.pending_action = Some(TimelineAction::BeginMixerExact {
                        point,
                        track,
                        parameter,
                    });
                }
            }
            ContextCommand::ToggleMixerKeyframe => {
                if let ContextKind::Mixer { track, parameter } = kind {
                    self.pending_action =
                        Some(TimelineAction::ToggleMixerKeyframe { track, parameter });
                }
            }
        }
    }
}
