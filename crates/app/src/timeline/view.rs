use super::*;

impl TimelineState {
    pub fn build(
        &self,
        ctx: &mut ui::BuildCtx,
        stack: StackId,
        content: Rect,
        icons: Icons,
        project: &Project,
        waveform_textures: &WaveformTextures,
    ) {
        let local = TimelineLayout::new(Rect::new(0.0, 0.0, content.width, content.height));
        ui::ui!(ctx, {
            Block {
                id: @format("timeline {}", stack.0);
                fill: theme::timeline_bg();
                width: Size::Fill;
                height: Size::Fill;

                @rust {
                    self.build_track_rows(ctx, local, stack);
                    self.build_grid(ctx, local, stack);
                    let dragged = self.dragged_track();
                    let visible_start = self.scroll_time as f32;
                    let visible_end = (self.scroll_time + self.visible_duration(local)) as f32;



                    let overscan = ((visible_end - visible_start) * 0.05).max(0.25);
                    for clip in &self.clips {
                        if clip.end() < visible_start - overscan
                            || clip.start > visible_end + overscan
                            || clip.track_index(&self.tracks) == dragged
                        {
                            continue;
                        }
                        self.build_clip(
                            ctx,
                            local,
                            stack,
                            clip,
                            project,
                            waveform_textures,
                        );
                    }
                    self.build_ruler(ctx, local, stack);
                    self.build_headers(ctx, local, stack, icons);
                    self.build_dragged_track(
                        ctx,
                        local,
                        stack,
                        icons,
                        project,
                        waveform_textures,
                    );
                    self.build_overview(ctx, local, stack, icons);
                    self.build_overlays(ctx, local, stack, [content.x, content.y]);

                    self.build_transport(ctx, local, stack, icons);
                    self.build_context_menu(ctx, local, stack, [content.x, content.y], icons, project);
                    self.build_mixer_exact(ctx, local, stack);
                    self.build_keyframe_value_editor(ctx, local, stack);
                }
            }
        });
    }

    fn dragged_track(&self) -> Option<usize> {
        match &self.drag {
            Some(Drag::Track { id, .. }) => self.track_index(*id),
            _ => None,
        }
    }

    fn visible_keyframe_points<'a>(
        &self,
        layout: TimelineLayout,
        points: &'a [KeyframeLanePoint],
    ) -> &'a [KeyframeLanePoint] {
        if points.len() <= 2 {
            return points;
        }
        let visible_start = self.scroll_time;
        let visible_end = visible_start + self.visible_duration(layout);
        let first_visible = points.partition_point(|point| point.time < visible_start);
        let first = first_visible.saturating_sub(1);
        let after_visible = points.partition_point(|point| point.time <= visible_end);
        let end = (after_visible + 1).min(points.len());
        &points[first..end.max(first + 1)]
    }

    fn build_keyframe_body_lanes(
        &self,
        ctx: &mut ui::BuildCtx,
        layout: TimelineLayout,
        stack: StackId,
        index: usize,
    ) {
        let lanes = self.keyframe_lanes_for_track(index);
        for (property_index, (start, end, rect)) in self
            .keyframe_property_rects(layout, index, false)
            .into_iter()
            .enumerate()
        {
            if rect.y > layout.body.bottom() {
                break;
            }
            if rect.bottom() < layout.body.y {
                continue;
            }
            let property = &lanes[start..end];
            let property_id = &property[0].id;
            let open_amount = self.keyframe_property_open_amount(property_id);
            let graph_settled = self.keyframe_property_graph_is_settled(property_id);
            let axis_range =
                (open_amount > 0.001).then(|| keyframe_property_axis_range(property, rect.height));

            ui::ui!(ctx, {
                Rect(("keyframe-property-bg", stack.0, self.tracks[index].id, property_index), rect) {
                    fill: if property_index % 2 == 0 { theme::timeline_bg_alt() } else { theme::timeline_bg() };
                    border: 1;
                    border_color: theme::timeline_line();
                }
            });

            for (component_offset, lane) in property.iter().enumerate() {
                let color = keyframe_property_color(&lane.id);
                let visible_points = self.visible_keyframe_points(layout, &lane.points);
                if !visible_points.is_empty() {
                    let mut vertices = Vec::new();
                    if axis_range.is_none() {
                        let y = rect.height * 0.5;
                        push_line_vertices(&mut vertices, [0.0, y], [rect.width, y], 1.5);
                    } else {
                        let first = visible_points[0];
                        let first_x = self.time_x(layout, first.time as f32) - rect.x;
                        let first_y = keyframe_value_y(rect, axis_range, first.value) - rect.y;
                        push_line_vertices(&mut vertices, [0.0, first_y], [first_x, first_y], 1.5);

                        for pair in visible_points.windows(2) {
                            let a = pair[0];
                            let b = pair[1];
                            let span = b.time - a.time;
                            if span <= 0.0 {
                                continue;
                            }
                            if a.interpolation == Interpolation::Step {
                                let x0 = self.time_x(layout, a.time as f32);
                                let x1 = self.time_x(layout, b.time as f32);
                                let y0 = keyframe_value_y(rect, axis_range, a.value);
                                let y1 = keyframe_value_y(rect, axis_range, b.value);
                                push_line_vertices(
                                    &mut vertices,
                                    [x0 - rect.x, y0 - rect.y],
                                    [x1 - rect.x, y0 - rect.y],
                                    1.5,
                                );
                                push_line_vertices(
                                    &mut vertices,
                                    [x1 - rect.x, y0 - rect.y],
                                    [x1 - rect.x, y1 - rect.y],
                                    1.5,
                                );
                            } else {
                                let start = [
                                    self.time_x(layout, a.time as f32) - rect.x,
                                    keyframe_value_y(rect, axis_range, a.value) - rect.y,
                                ];
                                let end = [
                                    self.time_x(layout, b.time as f32) - rect.x,
                                    keyframe_value_y(rect, axis_range, b.value) - rect.y,
                                ];
                                push_keyframe_curve_vertices(&mut vertices, a, b, start, end, 1.5);
                            }
                        }

                        let last = *visible_points.last().unwrap();
                        let last_x = self.time_x(layout, last.time as f32) - rect.x;
                        let last_y = keyframe_value_y(rect, axis_range, last.value) - rect.y;
                        push_line_vertices(
                            &mut vertices,
                            [last_x, last_y],
                            [rect.width, last_y],
                            1.5,
                        );
                    }

                    if !vertices.is_empty() {
                        ui::ui!(ctx, {
                            Rect(("keyframe-curve", stack.0, self.tracks[index].id, property_index, component_offset), rect) {
                                fill: color;
                                vertices: vertices;
                            }
                        });
                    }
                }

                if axis_range.is_some() {
                    let mut midpoint_vertices = Vec::new();
                    let mut control_vertices = Vec::new();
                    let mut control_line_vertices = Vec::new();
                    for pair in visible_points.windows(2) {
                        let a = pair[0];
                        let b = pair[1];
                        if a.interpolation == Interpolation::Step || b.time <= a.time {
                            continue;
                        }
                        let mid_t = 0.5f32;
                        let mid_mix = keyframe_segment_amount(a, b, mid_t) as f64;
                        let mid_time = a.time + (b.time - a.time) * 0.5;
                        let mid_value = a.value + (b.value - a.value) * mid_mix;
                        let mx = self.time_x(layout, mid_time as f32);
                        let my = keyframe_value_y(rect, axis_range, mid_value);
                        push_diamond_vertices(
                            &mut midpoint_vertices,
                            [mx - rect.x, my - rect.y],
                            3.5,
                        );

                        let selected_a = self.keyframe_is_selected(&lane.id, a.time);
                        let selected_b = self.keyframe_is_selected(&lane.id, b.time);
                        if selected_a || selected_b {
                            let (out, incoming) = keyframe_easing(a, b);
                            let (out_pos, in_pos) = keyframe_control_positions(
                                self, layout, rect, axis_range, a, b, out, incoming,
                            );
                            let ax = self.time_x(layout, a.time as f32);
                            let ay = keyframe_value_y(rect, axis_range, a.value);
                            let bx = self.time_x(layout, b.time as f32);
                            let by = keyframe_value_y(rect, axis_range, b.value);
                            if selected_a {
                                push_line_vertices(
                                    &mut control_line_vertices,
                                    [ax - rect.x, ay - rect.y],
                                    [out_pos[0] - rect.x, out_pos[1] - rect.y],
                                    1.0,
                                );
                                push_diamond_vertices(
                                    &mut control_vertices,
                                    [out_pos[0] - rect.x, out_pos[1] - rect.y],
                                    4.0,
                                );
                            }
                            if selected_b {
                                push_line_vertices(
                                    &mut control_line_vertices,
                                    [bx - rect.x, by - rect.y],
                                    [in_pos[0] - rect.x, in_pos[1] - rect.y],
                                    1.0,
                                );
                                push_diamond_vertices(
                                    &mut control_vertices,
                                    [in_pos[0] - rect.x, in_pos[1] - rect.y],
                                    4.0,
                                );
                            }
                        }
                    }
                    if !control_line_vertices.is_empty() {
                        ui::ui!(ctx, {
                            Rect(("keyframe-ease-lines", stack.0, self.tracks[index].id, property_index, component_offset), rect) {
                                fill: color;
                                vertices: control_line_vertices;
                            }
                        });
                    }
                    if !midpoint_vertices.is_empty() {
                        ui::ui!(ctx, {
                            Rect(("keyframe-ease-midpoints", stack.0, self.tracks[index].id, property_index, component_offset), rect) {
                                fill: color;
                                vertices: midpoint_vertices;
                            }
                        });
                    }
                    if !control_vertices.is_empty() {
                        ui::ui!(ctx, {
                            Rect(("keyframe-ease-controls", stack.0, self.tracks[index].id, property_index, component_offset), rect) {
                                fill: theme::timeline_text();
                                vertices: control_vertices;
                            }
                        });
                    }
                }

                let mut selected_vertices = Vec::new();
                let mut key_vertices = Vec::new();
                for point in visible_points {
                    let x = self.time_x(layout, point.time as f32);
                    let y = keyframe_value_y(rect, axis_range, point.value);
                    if self.keyframe_is_selected(&lane.id, point.time) {
                        push_diamond_vertices(
                            &mut selected_vertices,
                            [x - rect.x, y - rect.y],
                            7.0,
                        );
                    }
                    push_diamond_vertices(&mut key_vertices, [x - rect.x, y - rect.y], 4.5);
                }
                if !selected_vertices.is_empty() {
                    ui::ui!(ctx, {
                        Rect(("keyframe-selected-points", stack.0, self.tracks[index].id, property_index, component_offset), rect) {
                            fill: theme::timeline_text();
                            vertices: selected_vertices;
                        }
                    });
                }
                if !key_vertices.is_empty() {
                    ui::ui!(ctx, {
                        Rect(("keyframe-points", stack.0, self.tracks[index].id, property_index, component_offset), rect) {
                            fill: color;
                            vertices: key_vertices;
                        }
                    });
                }
            }

            if graph_settled {
                let (minimum, maximum) =
                    axis_range.expect("expanded keyframe property has an axis range");
                let middle = (minimum + maximum) * 0.5;
                let labels = [
                    (format_keyframe_value(maximum as f32), rect.y + 8.0),
                    (
                        format_keyframe_value(middle as f32),
                        rect.y + rect.height * 0.5,
                    ),
                    (format_keyframe_value(minimum as f32), rect.bottom() - 8.0),
                ];
                for (label_index, (text, y)) in labels.into_iter().enumerate() {
                    ui::ui!(ctx, {
                        Rect(("keyframe-range-label", stack.0, self.tracks[index].id, property_index, label_index), Rect::new(rect.x + 4.0, y - 8.0, 62.0, 16.0)) {
                            fill: theme::timeline_bg();
                            font_size: 8.5;
                            text_color: theme::timeline_muted();
                            padding: 2.0;
                            text: text;
                        }
                    });
                }
            }
        }
    }

    pub(super) fn keyframe_track_open_amount(&self, track: u32) -> f32 {
        self.keyframe_track_expansion
            .get(&track)
            .copied()
            .unwrap_or_else(|| self.expanded_keyframe_tracks.contains(&track) as u8 as f32)
    }

    fn build_keyframe_header_lanes(
        &self,
        ctx: &mut ui::BuildCtx,
        layout: TimelineLayout,
        stack: StackId,
        index: usize,
        icons: Icons,
    ) {
        let lanes = self.keyframe_lanes_for_track(index);
        for (property_index, (start, end, rect)) in self
            .keyframe_property_rects(layout, index, true)
            .into_iter()
            .enumerate()
        {
            if rect.y > layout.header_body.bottom() {
                break;
            }
            if rect.bottom() < layout.header_body.y {
                continue;
            }
            let property = &lanes[start..end];
            let lane = &property[0];
            let open_amount = self.keyframe_property_open_amount(&lane.id);
            let toggle = Self::keyframe_lane_toggle_rect(rect);
            ui::ui!(ctx, {
                Rect(("keyframe-header-bg", stack.0, self.tracks[index].id, property_index), rect) {
                    fill: theme::timeline_header();
                    border: 1;
                    border_color: theme::timeline_line();
                }
                Block {
                    id: @format("keyframe-header-chevron-{}-{}-{}", stack.0, self.tracks[index].id, property_index);
                    bounds: (toggle.x, toggle.y, toggle.width, toggle.height);
                    content_centered;

                    Icon {
                        id: @format("keyframe-header-chevron-icon-{}-{}-{}", stack.0, self.tracks[index].id, property_index);
                        icon!: icons.get(AppIcon::Chevron);
                        color!: theme::timeline_muted();
                        texture_rotation: std::f32::consts::FRAC_PI_2 * open_amount;
                        width: Size::Pixels(12.0);
                        height: Size::Pixels(12.0);
                    }
                }
                Rect(
                    ("keyframe-header-label", stack.0, self.tracks[index].id, property_index),
                    Rect::new(
                        toggle.right() + 3.0,
                        rect.y + 3.0,
                        (rect.width - toggle.width - 14.0).max(1.0),
                        (KEYFRAME_LANE_H - 3.0).max(1.0),
                    ),
                ) {
                    font_size: 9.5;
                    text_color: theme::timeline_text();
                    text_vertical_align: Align::Start;
                    text: lane.label.as_str();
                }
            });
            if open_amount > 0.001 {
                let mut seen = HashSet::new();
                let components = property
                    .iter()
                    .filter(|lane| seen.insert(lane.id.component))
                    .collect::<Vec<_>>();
                for (component_offset, lane) in components.iter().enumerate() {
                    let row_y = rect.y + KEYFRAME_LANE_H + 4.0 + component_offset as f32 * 18.0;
                    if row_y + 16.0 > rect.bottom() {
                        break;
                    }
                    let component_label =
                        keyframe_component_label(lane.id.component, components.len());
                    ui::ui!(ctx, {
                        Rect(
                            ("keyframe-header-component", stack.0, self.tracks[index].id, property_index, component_offset),
                            Rect::new(toggle.right() + 5.0, row_y + 4.0, 7.0, 7.0),
                        ) {
                            fill: keyframe_property_color(&lane.id);
                            border_radius: 3.5;
                        }
                        Rect(
                            ("keyframe-header-component-label", stack.0, self.tracks[index].id, property_index, component_offset),
                            Rect::new(
                                toggle.right() + 18.0,
                                row_y,
                                (rect.width - toggle.width - 29.0).max(1.0),
                                16.0,
                            ),
                        ) {
                            font_size: 9.0;
                            text_color: theme::timeline_muted();
                            text_vertical_align: Align::Center;
                            text: component_label;
                        }
                    });
                }
            }
        }
    }

    fn build_track_row(
        &self,
        ctx: &mut ui::BuildCtx,
        layout: TimelineLayout,
        stack: StackId,
        index: usize,
    ) {
        let track = &self.tracks[index];
        let y = self.track_y(layout, index);
        if y + self.display_track_height(index) < layout.body.y || y > layout.body.bottom() {
            return;
        }
        let rect = self.track_row_rect(layout, index);
        ui::ui!(ctx, {
            Rect(("timeline-track", stack.0, track.id), rect) {
                fill: if index.is_multiple_of(2) { theme::timeline_bg() } else { theme::timeline_bg_alt() };
                border: 1;
                border_color: theme::timeline_line();
            }
        });
        for (range_index, range) in self.render_cache_ranges.iter().enumerate() {
            let x0 = self.time_x(layout, range.start).max(rect.x);
            let x1 = self.time_x(layout, range.end).min(rect.right());
            if x1 <= x0 {
                continue;
            }
            let fill = match range.state {
                RenderCacheState::Rendered => Color::rgba8(0x12, 0x63, 0x35, 0x2a),
                RenderCacheState::Dirty => Color::rgba8(0xd2, 0x78, 0x16, 0x34),
            };
            ui::ui!(ctx, {
                Rect(
                    ("timeline-render-cache", stack.0, track.id, range_index),
                    Rect::new(x0, rect.y + 1.0, x1 - x0, (rect.height - 2.0).max(1.0)),
                ) {
                    fill: fill;
                }
            });
        }
        if let Some((start, end)) = self.render_output_range {
            for (edge, time) in [("start", start), ("end", end)] {
                let x = self.time_x(layout, time);
                if x >= rect.x && x <= rect.right() {
                    ui::ui!(ctx, {
                        Rect(
                            ("timeline-render-boundary", stack.0, track.id, edge),
                            Rect::new(x - 0.5, rect.y, 1.0, rect.height),
                        ) {
                            fill: theme::accent();
                        }
                    });
                }
            }
        }
        if let Some(end) = self.end_time {
            self.build_after_end(
                ctx,
                ("timeline-track-after-end", stack.0, track.id),
                rect,
                self.time_x(layout, end),
            );
        }
        self.build_keyframe_body_lanes(ctx, layout, stack, index);
    }

    fn build_track_rows(&self, ctx: &mut ui::BuildCtx, layout: TimelineLayout, stack: StackId) {
        ui::ui!(ctx, {
            Rect(("timeline-body", stack.0), layout.body) {
                fill: theme::timeline_bg();
            }
        });
        if let Some(end) = self.end_time {
            self.build_after_end(
                ctx,
                ("timeline-body-after-end", stack.0),
                layout.body,
                self.time_x(layout, end),
            );
        }
        let dragged = self.dragged_track();
        for (index, _) in self.tracks.iter().enumerate() {
            if Some(index) != dragged {
                self.build_track_row(ctx, layout, stack, index);
            }
        }
    }

    fn build_grid(&self, ctx: &mut ui::BuildCtx, layout: TimelineLayout, stack: StackId) {
        ui::ui!(ctx, {
            @for (index, x, _) in timeline_ticks(self.scroll_time, self.pixels_per_second, layout.body.width) {
                Rect(("timeline-grid", stack.0, index), Rect { x: layout.body.x + x, width: 1.0, ..layout.body }) {
                    fill: theme::timeline_grid();
                }
            }
        });
    }

    fn build_clip(
        &self,
        ctx: &mut ui::BuildCtx,
        layout: TimelineLayout,
        stack: StackId,
        clip: &Clip,
        project: &Project,
        waveform_textures: &WaveformTextures,
    ) {
        let rect = self.clip_rect(layout, clip);
        if !intersects(rect, layout.body) {
            return;
        }
        let selection = self.selection_levels.get(&clip.id).copied().unwrap_or(0.0);
        if selection > 0.0 {
            ui::ui!(ctx, {
                Rect(("timeline-clip-selection", stack.0, clip.id), rect.inset(-2.0)) {
                    fill: Color::TRANSPARENT;
                    border: 2;
                    border_color: Color::rgba(theme::accent().r, theme::accent().g, theme::accent().b, selection);
                    border_radius: RADIUS_LG;
                }
            });
        }
        let waveforms = clip_waveforms(clip, rect, project, waveform_textures);
        let mut loop_markers = Vec::new();
        if let Some(source_duration) = clip_source_duration(project, &clip.source) {
            let speed = clip.speed.max(0.01);
            let source_offset = clip.source_offset.rem_euclid(source_duration);
            let first = (source_duration - source_offset) / speed;
            let period = source_duration / speed;
            let mut local_time = first;
            let mut marker_index = 0usize;
            while local_time < clip.duration - 1.0e-6 && marker_index < 4096 {
                let x = local_time / clip.duration.max(MIN_CLIP) * rect.width;
                if rect.x + x >= layout.body.x - 4.0 && rect.x + x <= layout.body.right() + 4.0 {
                    loop_markers.push((marker_index, x));
                }
                marker_index += 1;
                local_time += period;
            }
        }
        let visible_clip = rect.intersect(layout.body);
        let title_x = (layout.body.x - rect.x + 7.0)
            .max(7.0)
            .min((rect.width - 7.0).max(7.0));
        let title_width = (rect.width - title_x - 7.0).max(1.0);
        ui::ui!(ctx, {
            Block {
                id: @format("timeline-clip {} {}", stack.0, clip.id);
                bounds: (rect.x, rect.y, rect.width, rect.height);
                fill: clip.color.color();
                border_radius: RADIUS_MD;
                cursor: CursorShape::Pointer;

                @for (index, waveform) in waveforms.iter().enumerate() {
                    Rect(("clip-waveform", clip.id, index), waveform.rect) {
                        fill_texture: waveform.texture;
                        texture_uv: waveform.uv;
                        texture_mode: waveform.mode;
                    }
                }

                @if visible_clip.width >= 15.0 {
                    Block {
                        id: @format("clip-title {}", clip.id);
                        bounds: (title_x, 4.0, title_width, 16.0);
                        font_size: 10.0;
                        text_color: theme::timeline_text();
                        text: clip.name.clone();
                    }
                }

                @for (marker_index, marker_x) in loop_markers.iter().copied() {
                    Rect(
                        ("clip-loop-top", clip.id, marker_index),
                        Rect::new(marker_x - 4.0, 0.0, 8.0, 5.0),
                    ) {
                        fill: theme::timeline_bg();
                        vertices: vec![[0.0, 0.0], [8.0, 0.0], [4.0, 5.0]];
                    }
                    Rect(
                        ("clip-loop-bottom", clip.id, marker_index),
                        Rect::new(marker_x - 4.0, rect.height - 5.0, 8.0, 5.0),
                    ) {
                        fill: theme::timeline_bg();
                        vertices: vec![[0.0, 5.0], [8.0, 5.0], [4.0, 0.0]];
                    }
                }

                @if rect.width >= 30.0 {
                    @for left in [true, false] {
                        Block {
                            id: @format("clip-edge {} {}", clip.id, left);
                            bounds: (
                                if left { 0.0 } else { rect.width - EDGE_W },
                                0.0,
                                EDGE_W,
                                rect.height,
                            );
                            fill: Color::rgba8(0, 0, 0, 0x28);
                            cursor: CursorShape::EwResize;
                        }
                    }
                }
            }
        });
    }

    fn build_ruler(&self, ctx: &mut ui::BuildCtx, layout: TimelineLayout, stack: StackId) {
        ui::ui!(ctx, {
            Rect(("timeline-ruler", stack.0), layout.ruler) {
                fill: theme::timeline_header(); border: 1; border_color: theme::timeline_line();
            }
        });
        for (range_index, range) in self.render_cache_ranges.iter().enumerate() {
            let x0 = self.time_x(layout, range.start).max(layout.ruler.x);
            let x1 = self.time_x(layout, range.end).min(layout.ruler.right());
            if x1 <= x0 {
                continue;
            }
            let fill = match range.state {
                RenderCacheState::Rendered => Color::rgba8(0x0d, 0x59, 0x2d, 0x32),
                RenderCacheState::Dirty => Color::rgba8(0xd2, 0x78, 0x16, 0x3c),
            };
            ui::ui!(ctx, {
                Rect(
                    ("timeline-ruler-render-cache", stack.0, range_index),
                    Rect::new(x0, 1.0, x1 - x0, (RULER_H - 2.0).max(1.0)),
                ) {
                    fill: fill;
                }
            });
        }
        if let Some((start, end)) = self.render_output_range {
            for (edge, time) in [("start", start), ("end", end)] {
                let x = self.time_x(layout, time);
                if x >= layout.ruler.x && x <= layout.ruler.right() {
                    ui::ui!(ctx, {
                        Rect(
                            ("timeline-ruler-render-boundary", stack.0, edge),
                            Rect::new(x - 0.5, 0.0, 1.0, RULER_H),
                        ) {
                            fill: theme::accent();
                        }
                    });
                }
            }
        }
        if let Some(end) = self.end_time {
            self.build_after_end(
                ctx,
                ("timeline-ruler-after-end", stack.0),
                layout.ruler,
                self.time_x(layout, end),
            );
        }
        let step = tick_step(self.pixels_per_second);
        for (index, x, time) in
            timeline_ticks(self.scroll_time, self.pixels_per_second, layout.body.width)
        {
            let x = layout.body.x + x;
            ui::ui!(ctx, {
                Rect(
                    ("timeline-tick", stack.0, index),
                    Rect::new(x, RULER_H - 7.0, 1.0, 7.0),
                ) {
                    fill: theme::timeline_muted();
                }
                Rect(("timeline-time", stack.0, index), Rect::new(x + 4.0, 2.0, 60.0, 16.0)) {
                    font_size: 9.0;
                    monospace;
                    text_color: theme::timeline_muted();
                    text: format_time(time, step);
                }
            });
        }
    }

    fn build_header(
        &self,
        ctx: &mut ui::BuildCtx,
        layout: TimelineLayout,
        stack: StackId,
        index: usize,
        icons: Icons,
    ) {
        let track = &self.tracks[index];
        let number = self.tracks[..=index]
            .iter()
            .filter(|candidate| candidate.kind == track.kind)
            .count();
        let header = self.track_header_rect(layout, index);
        if self.track_y(layout, index) + self.display_track_height(index) < layout.header_body.y
            || header.y > layout.header_body.bottom()
        {
            return;
        }
        let active = self
            .clips
            .iter()
            .any(|clip| clip.track == track.id && self.selected.contains(&clip.id));
        let dragging = self.dragged_track() == Some(index);
        let muted = self.track_is_muted(index);
        let rename = self
            .rename
            .as_ref()
            .filter(|rename| rename.track == track.id);
        ui::ui!(ctx, {
            Row {
                id: @format("timeline-header {} {}", stack.0, track.id);
                bounds: (header.x, header.y, header.width, header.height);
                padding: TRACK_HEADER_PAD;
                fill: if active { theme::timeline_header_active() } else { theme::timeline_header() };
                border: 1;
                border_color: theme::timeline_line();
                gap: TRACK_HEADER_GAP;

                Column {
                    width: Size::Pixels(TRACK_HANDLE_W);
                    height: Size::Fill;

                    Block {
                        id: @format("track-handle {}", track.id);
                        width: Size::Fill;
                        height: Size::Pixels(TRACK_TOP_H);
                        content_centered;

                        Icon {
                            id: @format("track-handle-icon {}", track.id);
                            icon!: icons.get(AppIcon::GripVertical);
                            color!: if dragging { theme::accent() } else { theme::timeline_muted() };
                            width: Size::Pixels(16.0);
                            height: Size::Pixels(16.0);
                        }
                    }
                }

                Column {
                    width: Size::Fill;
                    height: Size::Fill;
                    gap: TRACK_HEADER_GAP;

                    Row {
                        width: Size::Fill;
                        height: Size::Pixels(TRACK_TOP_H);
                        gap: TRACK_HEADER_GAP;

                        Block {
                            id: @format("track-number {}", track.id);
                            width: Size::Pixels(TRACK_LABEL_W);
                            height: Size::Fill;
                            font_size: 9.0;
                            text_color: theme::timeline_muted();
                            text: format!(
                                "{}{number}",
                                match track.kind { TrackKind::Video => 'V', TrackKind::Audio => 'A', TrackKind::Effect => 'E' }
                            );
                        }

                        @if rename.is_some() {
                            Block {
                                id: @format("track-rename {}", track.id);
                                width: Size::Pixels(TRACK_NAME_W);
                                height: Size::Fill;
                                fill: theme::timeline_bg();
                                border: 1;
                                border_color: theme::accent();
                                border_radius: RADIUS_SM;
                                padding: 3.0;
                                font_size: 10.0;
                                text_color: theme::timeline_text();
                                text: format!("{}│", rename.unwrap().value);
                            }
                        } @else {
                            Block {
                                id: @format("track-name {}", track.id);
                                width: Size::Pixels(TRACK_NAME_W);
                                height: Size::Fill;
                                font_size: 10.0;
                                text_color: theme::timeline_text();
                                text: track.name.clone();
                            }
                        }

                        HSpacer {}

                        @for (solo, label, on) in [(false, "M", muted), (true, "S", track.solo)] {
                            Block {
                                id: @format("track-toggle {} {}", track.id, solo);
                                width: Size::Pixels(TRACK_BUTTON_W);
                                height: Size::Fill;
                                fill: if on { theme::accent() } else { theme::timeline_header_active() };
                                animate_fill;
                                border: 1;
                                border_color: if on { theme::accent() } else { theme::timeline_line() };
                                border_radius: RADIUS_SM;
                                font_size: 9.0;
                                text_color: if on { theme::timeline_bg() } else { theme::timeline_text() };
                                text_centered;
                                text: label;
                                reveal;
                                tooltip: if solo { "Solo track" } else { "Mute track" };
                            }
                        }
                    }

                    @if track.kind == TrackKind::Audio {
                        @let levels = self.audio_levels.get(&track.id).copied().unwrap_or([0.0_f32, 0.0_f32]);

                        Row {
                            width: Size::Fill;
                            height: Size::Fill;

                            HSpacer {}
                            Column {
                                width: Size::Pixels(TRACK_VU_W);
                                height: Size::Fill;
                                gap: 3.0;

                                @for (channel, level) in levels.into_iter().enumerate() {
                                    Block {
                                        id: @format("vu-bg {} {}", track.id, channel);
                                        width: Size::Fill;
                                        height: Size::Pixels(5.0);
                                        fill: theme::timeline_bg();

                                        Block {
                                            id: @format("vu {} {}", track.id, channel);
                                            fill: if level > 0.82 { theme::accent() } else { AUDIO_A };
                                            width: Size::Pixels(TRACK_VU_W * level);
                                            height: Size::Fill;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });
        if track.kind == TrackKind::Audio {
            for (parameter, label) in [(MixerParameter::Volume, "V"), (MixerParameter::Pan, "P")] {
                if let Some(knob) = self.mixer_knobs.get(&(track.id, parameter)) {
                    let (label_rect, rect) = self.mixer_control_rects(layout, index, parameter);
                    ui::ui!(ctx, {
                        Rect(("track-mixer-label", track.id, parameter), label_rect) {
                            font_size: 8.0;
                            text_color: if self.mixer_has_keyframe(track.id, parameter) { theme::accent() } else { theme::timeline_muted() };
                            text_centered;
                            text: label;
                        }
                    });
                    knob.build(
                        ctx,
                        format!("track-mixer-{}-{parameter:?}", track.id),
                        rect,
                        crate::widgets::component_style(),
                    );
                }
            }
        }

        let keyframe_lanes = self.keyframe_lanes_for_track(index);
        if !keyframe_lanes.is_empty() {
            let keyframe_toggle = self.keyframe_track_toggle_rect(layout, index);
            let keyframe_open_amount = self.keyframe_track_open_amount(track.id);
            ui::ui!(ctx, {
                Block {
                    id: @format("track-keyframe-chevron-{}-{}", stack.0, track.id);
                    bounds: (keyframe_toggle.x, keyframe_toggle.y, keyframe_toggle.width, keyframe_toggle.height);
                    content_centered;
                    interactive;
                    tooltip: i18n::text("timeline-show-keyframes");

                    Icon {
                        id: @format("track-keyframe-chevron-icon-{}-{}", stack.0, track.id);
                        icon!: icons.get(AppIcon::Chevron);
                        color!: theme::timeline_text();
                        texture_rotation: std::f32::consts::FRAC_PI_2 * keyframe_open_amount;
                        width: Size::Pixels(12.0);
                        height: Size::Pixels(12.0);
                    }
                }
            });
        }
        self.build_keyframe_header_lanes(ctx, layout, stack, index, icons);

        if dragging {
            let drag_border = header.inset(-2.0);
            ui::ui!(ctx, {
                Block {
                    id: @format("timeline-header-drag-border {} {}", stack.0, track.id);
                    bounds: (drag_border.x, drag_border.y, drag_border.width, drag_border.height);
                    border: 2;
                    border_color: theme::accent();
                }
            });
        }
    }

    fn build_headers(
        &self,
        ctx: &mut ui::BuildCtx,
        layout: TimelineLayout,
        stack: StackId,
        icons: Icons,
    ) {
        ui::ui!(ctx, {
            Block {
                id: @format("timeline-header-bed {}", stack.0);
                bounds: (
                    layout.header_body.x,
                    layout.header_body.y,
                    layout.header_body.width,
                    layout.header_body.height,
                );
                fill: theme::timeline_header();
                border: 1;
                border_color: theme::timeline_line();
            }
        });
        let dragged = self.dragged_track();
        for (index, _) in self.tracks.iter().enumerate() {
            if Some(index) != dragged {
                self.build_header(ctx, layout, stack, index, icons);
            }
        }
    }

    fn build_transport(
        &self,
        ctx: &mut ui::BuildCtx,
        layout: TimelineLayout,
        stack: StackId,
        icons: Icons,
    ) {
        let sticky = layout.corner;
        let (controls, _) = Self::transport_parts(layout);
        ui::ui!(ctx, {
            Block {
                id: @format("timeline-corner {}", stack.0);
                bounds: (sticky.x, sticky.y, sticky.width, sticky.height);
                fill: theme::timeline_header();
                border: 1;
                border_color: theme::timeline_line();
            }
        });
        ui::ui!(ctx, {
            Row {
                id: @format("timeline-controls {}", stack.0);
                bounds: (controls.x, controls.y, controls.width, controls.height);
                gap: TRANSPORT_BUTTON_GAP;

                HSpacer {
                    width: Size::Pixels(4.0);
                }
                Block {
                    id: @format("timeline-timecode {}", stack.0);
                    width: Size::Fit;
                    height: Size::Fill;
                    font_size: 11.0;
                    monospace;
                    no_wrap;
                    text_color: theme::timeline_text();
                    text: format_timecode(self.playhead, self.frame_rate);
                }
                HSpacer {}

                @for (index, icon) in [
                    AppIcon::SkipStart,
                    if self.playing { AppIcon::Pause } else { AppIcon::Play },
                    AppIcon::SkipEnd,
                ]
                .into_iter()
                .enumerate() {
                    Block {
                        id: @format("timeline-jump {} {}", stack.0, index);
                        fill: theme::timeline_header_active();
                        border: 1;
                        border_color: theme::timeline_line();
                        border_radius: RADIUS_SM;
                        width: Size::Pixels(TRANSPORT_BUTTON_W);
                        height: Size::Fill;
                        reveal;
                        tooltip: match index {
                            0 => i18n::text("timeline-jump-start"),
                            1 if self.playing => i18n::text("timeline-pause"),
                            1 => i18n::text("timeline-play"),
                            _ => i18n::text("timeline-jump-end"),
                        };
                        content_centered;

                        Icon {
                            id: @format("timeline-jump-icon {} {}", stack.0, index);
                            icon!: icons.get(icon);
                            color!: theme::timeline_text();
                            width: Size::Pixels(16.0);
                            height: Size::Pixels(16.0);
                        }
                    }
                }

                Block {
                    id: @format("timeline-end-behavior {}", stack.0);
                    fill: if self.end_behavior == EndBehavior::Restart { theme::accent() } else { theme::timeline_header_active() };
                    border: 1;
                    border_color: theme::timeline_line();
                    border_radius: RADIUS_SM;
                    width: Size::Pixels(TRANSPORT_BUTTON_W);
                    height: Size::Fill;
                    reveal;
                    interactive;
                    tooltip: if self.end_behavior == EndBehavior::Restart {
                        i18n::text("timeline-end-restart")
                    } else {
                        i18n::text("timeline-end-stop")
                    };
                    content_centered;
                    font_size: 10.0;
                    text_color: if self.end_behavior == EndBehavior::Restart { theme::timeline_bg() } else { theme::timeline_text() };
                    text: if self.end_behavior == EndBehavior::Restart { "R" } else { "S" };
                    text_align: Align::Center;
                }

                HSpacer {
                    width: Size::Pixels(1.0);
                }
            }
        });
    }

    fn build_dragged_track(
        &self,
        ctx: &mut ui::BuildCtx,
        layout: TimelineLayout,
        stack: StackId,
        icons: Icons,
        project: &Project,
        waveform_textures: &WaveformTextures,
    ) {
        let Some(index) = self.dragged_track() else {
            return;
        };
        let track = &self.tracks[index];
        self.build_track_row(ctx, layout, stack, index);
        let y = self.track_y(layout, index);
        for (index, x, _) in
            timeline_ticks(self.scroll_time, self.pixels_per_second, layout.body.width)
        {
            ui::ui!(ctx, {
                Rect(
                    ("timeline-drag-grid", stack.0, track.id, index),
                    Rect::new(layout.body.x + x, y, 1.0, track.height),
                ) {
                    fill: theme::timeline_grid();
                }
            });
        }
        for clip in self.clips.iter().filter(|clip| clip.track == track.id) {
            self.build_clip(ctx, layout, stack, clip, project, waveform_textures);
        }
        self.build_header(ctx, layout, stack, index, icons);
    }

    fn build_overview(
        &self,
        ctx: &mut ui::BuildCtx,
        layout: TimelineLayout,
        stack: StackId,
        icons: Icons,
    ) {
        let body = layout.overview_body;
        let total = self.overview_duration(layout);
        for (id, rect) in [
            ("timeline-overview", layout.overview),
            ("timeline-overview-left", layout.overview_header),
        ] {
            ui::ui!(ctx, {
                Rect((id, stack.0), rect) {
                    fill: theme::timeline_header();
                    border: 1;
                    border_color: theme::timeline_line();
                }
            });
        }
        ui::ui!(ctx, {
            Rect(("timeline-tool-separator", stack.0), layout.tool_separator) {
                fill: theme::timeline_line();
            }
        });
        for (id, label, rect, active, icon) in [
            (
                "timeline-frame-snap",
                &i18n::text("timeline-snap-frames"),
                layout.frame_snap_button,
                self.frame_snap,
                AppIcon::SnapFrame,
            ),
            (
                "timeline-grid-snap",
                &i18n::text("timeline-snap-grid"),
                layout.grid_snap_button,
                self.grid_snap,
                AppIcon::SnapGrid,
            ),
            (
                "timeline-clip-snap",
                &i18n::text("timeline-snap-clips"),
                layout.clip_snap_button,
                self.clip_snap,
                AppIcon::SnapClips,
            ),
            (
                "timeline-playhead-snap",
                &i18n::text("timeline-snap-playhead"),
                layout.playhead_snap_button,
                self.playhead_snap,
                AppIcon::SnapPlayhead,
            ),
            (
                "timeline-follow-playhead",
                &i18n::text("timeline-follow-playhead"),
                layout.follow_playhead_button,
                self.follow_playhead,
                AppIcon::FollowPlayhead,
            ),
            (
                "timeline-razor-tool",
                &i18n::text("timeline-razor-tool"),
                layout.razor_tool_button,
                self.tool == TimelineTool::Razor,
                AppIcon::ClipCut,
            ),
        ] {
            timeline_icon_toggle(
                ctx,
                &format!("{} {}", id, stack.0),
                rect,
                icons.get(icon),
                active,
                label,
            );
        }
        ui::ui!(ctx, {
            Rect(("timeline-overview-body", stack.0), body) {
                fill: theme::timeline_bg_alt();
            }
        });
        if let Some(end) = self.end_time {
            self.build_after_end(
                ctx,
                ("overview-after-end", stack.0),
                body,
                body.x + (end as f64 / total * body.width as f64) as f32,
            );
        }
        let lane_h = ((body.height - 8.0) / self.tracks.len().max(1) as f32).max(2.0);
        let batch_count = (body.width / OVERVIEW_BATCH_W).ceil().max(1.0) as usize;
        let mut overview_clips: HashMap<u32, Vec<&Clip>> = HashMap::new();
        for clip in &self.clips {
            overview_clips.entry(clip.track).or_default().push(clip);
        }
        for (track_index, track) in self.tracks.iter().enumerate() {
            let colors = match track.kind {
                TrackKind::Video => [VIDEO_A, VIDEO_B],
                TrackKind::Audio => [AUDIO_A, AUDIO_B],
                TrackKind::Effect => [Color::rgb8(0x8a, 0x58, 0x91), Color::rgb8(0x72, 0x46, 0x7a)],
            };
            let mut batches: Vec<[Vec<[f32; 2]>; 2]> =
                (0..batch_count).map(|_| [Vec::new(), Vec::new()]).collect();
            for &clip in overview_clips
                .get(&track.id)
                .map(Vec::as_slice)
                .unwrap_or(&[])
            {
                let x = body.x + (clip.start as f64 / total * body.width as f64) as f32;
                let right = x + (clip.duration as f64 / total * body.width as f64).max(1.0) as f32;
                let first = ((x - body.x) / OVERVIEW_BATCH_W).floor().max(0.0) as usize;
                let last = ((right - body.x) / OVERVIEW_BATCH_W).floor().max(0.0) as usize;
                let first = first.min(batch_count - 1);
                let last = last.min(batch_count - 1);
                let color = usize::from(clip.color.color() == colors[1]);
                for (batch, vertices) in batches.iter_mut().enumerate().take(last + 1).skip(first) {
                    let batch_x = body.x + batch as f32 * OVERVIEW_BATCH_W;
                    let left = x.max(batch_x) - batch_x;
                    let right = right.min(batch_x + OVERVIEW_BATCH_W) - batch_x;
                    if right > left {
                        push_rect_vertices(
                            &mut vertices[color],
                            Rect::new(left, 0.0, right - left, (lane_h - 1.0).max(1.0)),
                        );
                    }
                }
            }
            let y = body.y + 4.0 + track_index as f32 * lane_h;
            for (batch, colors_vertices) in batches.into_iter().enumerate() {
                let x = body.x + batch as f32 * OVERVIEW_BATCH_W;
                let width = OVERVIEW_BATCH_W.min(body.right() - x);
                for (color, vertices) in colors_vertices.into_iter().enumerate() {
                    if !vertices.is_empty() {
                        ui::ui!(ctx, {
                            Rect(("overview-clips", stack.0, track.id, batch, color), Rect::new(x, y, width, (lane_h - 1.0).max(1.0))) {
                                fill: colors[color];
                                vertices: vertices;
                            }
                        });
                    }
                }
            }
        }
        let playhead_x = body.x + (self.playhead as f64 / total * body.width as f64) as f32;
        ui::ui!(ctx, {
            Rect(
                ("overview-playhead", stack.0),
                Rect::new(
                    playhead_x,
                    body.y + 2.0,
                    1.0,
                    (body.height - 4.0).max(1.0),
                ),
            ) {
                fill: PLAYHEAD;
            }
        });
        let window = self.overview_window(layout);
        ui::ui!(ctx, {
            Rect(("overview-window", stack.0), window) {
                fill: Color::rgba(
                    theme::accent().r,
                    theme::accent().g,
                    theme::accent().b,
                    0x22 as f32 / 255.0,
                );
                border: 2;
                border_color: theme::accent();
                border_radius: RADIUS_SM;
            }
        });
        for (left, x) in [(true, window.x - 2.5), (false, window.right() - 2.5)] {
            ui::ui!(ctx, {
                Rect(
                    ("overview-handle", stack.0, left),
                    Rect::new(x, window.y, 5.0, window.height),
                ) {
                    fill: Color::rgba8(0, 0, 0, 0);
                }
            });
        }
    }

    fn build_overlays(
        &self,
        ctx: &mut ui::BuildCtx,
        layout: TimelineLayout,
        stack: StackId,
        origin: [f32; 2],
    ) {
        for time in &self.snap_times {
            let x = self.time_x(layout, *time);
            ui::ui!(ctx, {
                Rect(
                    ("timeline-snap", stack.0, (*time * 1000.0) as i64),
                    Rect::new(x, RULER_H, 1.0, layout.body.height),
                ) {
                    fill: SNAP;
                }
            });
        }
        let playhead_x = self.time_x(layout, self.playhead);
        if playhead_x >= layout.body.x && playhead_x <= layout.body.right() {
            ui::ui!(ctx, {
                Rect(
                    ("timeline-playhead", stack.0),
                    Rect::new(playhead_x, 0.0, 1.5, layout.overview.y),
                ) {
                    fill: PLAYHEAD;
                }
            });
        }
        if self.focused_stack == Some(stack) {
            if let Some(Drag::BoxSelect { start, current, .. }) = &self.drag {
                let rect = normalized_rect(*start, *current);
                let local = Rect {
                    x: rect.x - origin[0],
                    y: rect.y - origin[1],
                    ..rect
                };
                ui::ui!(ctx, {
                    Rect(("timeline-box-select", stack.0), local) {
                        fill: Color::rgba8(0xf0, 0xa2, 0x15, 0x18);
                        border: 1;
                        border_color: theme::accent();
                    }
                });
            }
        }
    }

    fn build_context_menu(
        &self,
        ctx: &mut ui::BuildCtx,
        layout: TimelineLayout,
        stack: StackId,
        origin: [f32; 2],
        icons: Icons,
        project: &Project,
    ) {
        let Some(menu) = self.context_menu.filter(|menu| menu.stack == stack) else {
            return;
        };
        let items = context_items(menu.kind, self.has_compatible_replacement_media(project));
        let rect = Self::context_rect(layout, menu, items.len());
        let cursor = [self.cursor[0] - origin[0], self.cursor[1] - origin[1]];
        widgets::build_context_menu(
            ctx,
            &format!("timeline-{}", stack.0),
            rect,
            cursor,
            &items,
            icons,
        );
    }

    fn build_mixer_exact(&self, ctx: &mut ui::BuildCtx, layout: TimelineLayout, stack: StackId) {
        let Some(editor) = self
            .mixer_exact
            .as_ref()
            .filter(|editor| editor.stack == stack)
        else {
            return;
        };
        let popup = Self::mixer_exact_rect(layout, editor);
        let label = match editor.parameter {
            MixerParameter::Volume => "Volume exact value (%)",
            MixerParameter::Pan => "Pan exact value (%)",
        };
        let rows = crate::ui_layout::column(
            popup,
            &[
                crate::ui_layout::Item::height(4.0),
                crate::ui_layout::Item::height(18.0),
                crate::ui_layout::Item::height(3.0),
                crate::ui_layout::Item::height(26.0),
                crate::ui_layout::Item::fill(),
            ],
            0.0,
            0.0,
            ui::Align::Start,
            None,
        );
        let label_rect = crate::ui_layout::row(
            rows[1],
            &[
                crate::ui_layout::Item::width(7.0),
                crate::ui_layout::Item::fill(),
                crate::ui_layout::Item::width(7.0),
            ],
            0.0,
            0.0,
            ui::Align::Start,
        )[1];
        let value_rect = crate::ui_layout::row(
            rows[3],
            &[
                crate::ui_layout::Item::width(7.0),
                crate::ui_layout::Item::fill(),
                crate::ui_layout::Item::width(7.0),
            ],
            0.0,
            0.0,
            ui::Align::Start,
        )[1];
        ui::ui!(ctx, {
            Rect(("mixer-exact", stack.0), popup) {
                overlay;
                backdrop_blur: 22.0;
                backdrop_tint: theme::popup_tint();
                fill: theme::floating_bg();
                border: 1;
                border_color: theme::accent();
                border_radius: RADIUS_MD;
            }
            Rect(("mixer-exact-label", stack.0), label_rect) {
                overlay;
                font_size: 9.0;
                text_color: theme::popup_muted();
                text: label;
            }
            Rect(("mixer-exact-value", stack.0), value_rect) {
                overlay;
                fill: theme::timeline_bg();
                border: 1;
                border_color: theme::accent();
                border_radius: RADIUS_SM;
                padding: 6.0;
                font_size: 10.5;
                text_color: theme::timeline_text();
                text: format!("{}│", editor.value);
            }
        });
    }

    fn build_keyframe_value_editor(
        &self,
        ctx: &mut ui::BuildCtx,
        layout: TimelineLayout,
        stack: StackId,
    ) {
        let Some(editor) = self
            .keyframe_value_editor
            .as_ref()
            .filter(|editor| editor.stack == stack)
        else {
            return;
        };
        let popup = Self::keyframe_value_editor_rect(layout, editor);
        let field = Rect::new(popup.x + 10.0, popup.y + 28.0, popup.width - 20.0, 24.0);
        let set = Self::keyframe_value_set_rect(layout, editor);
        ui::ui!(ctx, {
            Rect(("keyframe-value-editor", stack.0), popup) {
                overlay;
                backdrop_blur: 22.0;
                backdrop_tint: theme::popup_tint();
                fill: theme::floating_bg();
                border: 1;
                border_color: theme::accent();
                border_radius: RADIUS_MD;
            }
            Rect(("keyframe-value-title", stack.0), Rect::new(popup.x + 10.0, popup.y + 6.0, popup.width - 20.0, 18.0)) {
                overlay;
                font_size: 9.5;
                text_color: theme::popup_muted();
                text: i18n::text("timeline-edit-keyframe-value");
            }
            Rect(("keyframe-value-field", stack.0), field) {
                overlay;
                fill: theme::timeline_bg();
                border: 1;
                border_color: theme::accent();
                border_radius: RADIUS_SM;
                padding: 6.0;
                font_size: 10.5;
                text_color: theme::timeline_text();
                text: format!("{}│", editor.value);
            }
            Rect(("keyframe-value-set", stack.0), set) {
                overlay;
                fill: theme::accent();
                border_radius: RADIUS_SM;
                font_size: 9.5;
                text_color: theme::timeline_bg();
                text_centered;
                text: i18n::text("timeline-set");
            }
        });
    }
}
