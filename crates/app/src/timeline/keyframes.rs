use super::*;

impl TimelineState {
    fn source_row_mut(&mut self, track: u32, row: usize) -> Option<&mut LayerPropertyRow> {
        self.edit.track_mut(track)?.property_rows.get_mut(row)
    }

    fn target_pipeline_mut(&mut self, owner: KeyframeOwner) -> Option<&mut PipelineInstance> {
        match owner {
            KeyframeOwner::Track(id) => self.edit.track_mut(id)?.pipeline.as_mut(),
            KeyframeOwner::SourceRow { track, row } => {
                self.source_row_mut(track, row).map(|row| &mut row.pipeline)
            }
        }
    }

    fn target_composite_mut(&mut self, owner: KeyframeOwner) -> Option<&mut LayerComposite> {
        match owner {
            KeyframeOwner::Track(id) => Some(&mut self.edit.track_mut(id)?.composite),
            KeyframeOwner::SourceRow { track, row } => self
                .source_row_mut(track, row)
                .map(|row| &mut row.composite),
        }
    }

    pub(super) fn keyframe_binding_mut(
        &mut self,
        target: &KeyframeBindingTarget,
    ) -> Option<&mut Binding> {
        let property = &target.property;
        match property {
            KeyframeProperty::Opacity => {
                Some(&mut self.target_composite_mut(target.owner)?.opacity)
            }
            KeyframeProperty::BlendMode => {
                Some(&mut self.target_composite_mut(target.owner)?.blend_mode)
            }
            KeyframeProperty::AlphaBlendMode => {
                Some(&mut self.target_composite_mut(target.owner)?.alpha_blend_mode)
            }
            KeyframeProperty::Volume => match target.owner {
                KeyframeOwner::Track(id) => Some(&mut self.edit.track_mut(id)?.volume),
                KeyframeOwner::SourceRow { .. } => None,
            },
            KeyframeProperty::Pan => match target.owner {
                KeyframeOwner::Track(id) => Some(&mut self.edit.track_mut(id)?.pan),
                KeyframeOwner::SourceRow { .. } => None,
            },
            KeyframeProperty::Local { node, input } => self
                .target_pipeline_mut(target.owner)?
                .local_nodes
                .iter_mut()
                .find(|item| item.id == *node)?
                .inputs
                .get_mut(input),
            KeyframeProperty::Override { node, input } => self
                .target_pipeline_mut(target.owner)?
                .overrides
                .get_mut(*node, input),
            KeyframeProperty::Generator(input) => {
                let KeyframeOwner::SourceRow { track, row } = target.owner else {
                    return None;
                };
                let VisualSource::Generator(generator) =
                    &mut self.source_row_mut(track, row)?.source
                else {
                    return None;
                };
                generator.host_binding_mut(input)?.gpu_mut()
            }
            KeyframeProperty::Model3d(input) => match target.owner {
                KeyframeOwner::SourceRow { track, row } => {
                    self.source_row_mut(track, row)?.model3d.binding_mut(input)
                }
                KeyframeOwner::Track(_) => None,
            },
            KeyframeProperty::LocalHost { .. } | KeyframeProperty::GeneratorHost(_) => None,
        }
    }

    pub(super) fn keyframe_host_binding_mut(
        &mut self,
        target: &KeyframeBindingTarget,
    ) -> Option<&mut HostBinding> {
        match &target.property {
            KeyframeProperty::LocalHost { node, input } => self
                .target_pipeline_mut(target.owner)?
                .local_nodes
                .iter_mut()
                .find(|item| item.id == *node)?
                .host_inputs
                .get_mut(input),
            KeyframeProperty::GeneratorHost(input) => {
                let KeyframeOwner::SourceRow { track, row } = target.owner else {
                    return None;
                };
                let VisualSource::Generator(generator) =
                    &mut self.source_row_mut(track, row)?.source
                else {
                    return None;
                };
                generator.host_binding_mut(input)
            }
            _ => None,
        }
    }

    pub(super) fn append_keyframe_lanes<S: ScalarKeyframeBinding + ?Sized>(
        lanes: &mut Vec<KeyframeLane>,
        target: KeyframeBindingTarget,
        group_owner: KeyframeGroupOwner,
        label: String,
        binding: &S,
    ) {
        for component in 0..binding.scalar_count() {
            let keys = binding.scalar_keys(component);
            if keys.is_empty() {
                continue;
            }
            let points = keys
                .into_iter()
                .map(|key| KeyframeLanePoint {
                    time: key.time,
                    value: key.value as f64,
                    interpolation: key.interpolation,
                    ease_in: key.ease_in,
                    ease_out: key.ease_out,
                    custom_ease_in: key.custom_ease_in,
                    custom_ease_out: key.custom_ease_out,
                })
                .collect::<Vec<_>>();
            lanes.push(KeyframeLane {
                id: KeyframeLaneId {
                    target: target.clone(),
                    group: KeyframeLaneGroup {
                        owner: group_owner.clone(),
                        property: target.property.clone(),
                    },
                    component,
                },
                label: label.clone(),
                value_range: keyframe_points_range(&points),
                points,
            });
        }
    }

    pub(super) fn append_pipeline_keyframe_lanes(
        lanes: &mut Vec<KeyframeLane>,
        instance: &PipelineInstance,
        owner: &str,
        target_owner: KeyframeOwner,
        group_owner: KeyframeGroupOwner,
    ) {
        let mut local = HashSet::new();
        for node in &instance.local_nodes {
            for (input, base) in &node.inputs {
                local.insert((node.id, input.clone()));
                let (property, binding) =
                    if let Some(binding) = instance.overrides.get(node.id, input) {
                        (
                            KeyframeProperty::Override {
                                node: node.id,
                                input: input.clone(),
                            },
                            binding,
                        )
                    } else {
                        (
                            KeyframeProperty::Local {
                                node: node.id,
                                input: input.clone(),
                            },
                            base,
                        )
                    };
                let node_label = node
                    .node_type
                    .strip_prefix("builtin.")
                    .unwrap_or(&node.node_type);
                Self::append_keyframe_lanes(
                    lanes,
                    KeyframeBindingTarget::new(target_owner, property),
                    group_owner.clone(),
                    format!("{owner} -> {node_label} -> {input}"),
                    binding,
                );
            }
            for (input, binding) in &node.host_inputs {
                let target = KeyframeBindingTarget::new(
                    target_owner,
                    KeyframeProperty::LocalHost {
                        node: node.id,
                        input: input.clone(),
                    },
                );
                let node_label = node
                    .node_type
                    .strip_prefix("builtin.")
                    .unwrap_or(&node.node_type);
                Self::append_keyframe_lanes(
                    lanes,
                    target,
                    group_owner.clone(),
                    format!("{owner} -> {node_label} -> {input}"),
                    binding,
                );
            }
        }
        for (node, input, binding) in instance.overrides.iter() {
            if local.contains(&(node, input.to_owned())) {
                continue;
            }
            let target = KeyframeBindingTarget::new(
                target_owner,
                KeyframeProperty::Override {
                    node,
                    input: input.to_owned(),
                },
            );
            Self::append_keyframe_lanes(
                lanes,
                target,
                group_owner.clone(),
                format!("{owner} -> {node} -> {input}"),
                binding,
            );
        }
    }

    fn append_property_lanes(
        lanes: &mut Vec<KeyframeLane>,
        pipeline: &PipelineInstance,
        composite: &LayerComposite,
        model3d: &Model3dClipTransform,
        label: &str,
        owner: KeyframeOwner,
        group: KeyframeGroupOwner,
    ) {
        for (property, suffix, binding) in [
            (KeyframeProperty::Opacity, "Opacity", &composite.opacity),
            (
                KeyframeProperty::BlendMode,
                "Blend Mode",
                &composite.blend_mode,
            ),
            (
                KeyframeProperty::AlphaBlendMode,
                "Alpha Blend",
                &composite.alpha_blend_mode,
            ),
        ] {
            Self::append_keyframe_lanes(
                lanes,
                KeyframeBindingTarget::new(owner, property),
                group.clone(),
                format!("{label} -> {suffix}"),
                binding,
            );
        }
        Self::append_pipeline_keyframe_lanes(lanes, pipeline, label, owner, group.clone());
        for (input, suffix, binding) in [
            ("size", "3D Size", &model3d.size),
            ("position", "3D Position", &model3d.position),
            ("rotation", "3D Rotation", &model3d.rotation),
            ("scale", "3D Scale", &model3d.scale),
        ] {
            Self::append_keyframe_lanes(
                lanes,
                KeyframeBindingTarget::new(owner, KeyframeProperty::Model3d(input.into())),
                group.clone(),
                format!("{label} -> {suffix}"),
                binding,
            );
        }
    }

    pub(super) fn build_keyframe_lanes_for_track(&self, index: usize) -> Vec<KeyframeLane> {
        let Some(track) = self.tracks.get(index) else {
            return Vec::new();
        };
        let mut lanes = Vec::new();
        let track_group = KeyframeGroupOwner::Target(KeyframeOwner::Track(track.id));
        Self::append_keyframe_lanes(
            &mut lanes,
            KeyframeBindingTarget::new(KeyframeOwner::Track(track.id), KeyframeProperty::Opacity),
            track_group.clone(),
            "Track -> Opacity".into(),
            &track.composite.opacity,
        );
        Self::append_keyframe_lanes(
            &mut lanes,
            KeyframeBindingTarget::new(KeyframeOwner::Track(track.id), KeyframeProperty::BlendMode),
            track_group.clone(),
            "Track -> Blend Mode".into(),
            &track.composite.blend_mode,
        );
        Self::append_keyframe_lanes(
            &mut lanes,
            KeyframeBindingTarget::new(
                KeyframeOwner::Track(track.id),
                KeyframeProperty::AlphaBlendMode,
            ),
            track_group.clone(),
            "Track -> Alpha Blend".into(),
            &track.composite.alpha_blend_mode,
        );
        if track.kind == TrackKind::Audio {
            Self::append_keyframe_lanes(
                &mut lanes,
                KeyframeBindingTarget::new(
                    KeyframeOwner::Track(track.id),
                    KeyframeProperty::Volume,
                ),
                track_group.clone(),
                "Track -> Volume".into(),
                &track.volume,
            );
            Self::append_keyframe_lanes(
                &mut lanes,
                KeyframeBindingTarget::new(KeyframeOwner::Track(track.id), KeyframeProperty::Pan),
                track_group.clone(),
                "Track -> Pan".into(),
                &track.pan,
            );
        }
        if let Some(instance) = &track.pipeline {
            Self::append_pipeline_keyframe_lanes(
                &mut lanes,
                instance,
                "Track",
                KeyframeOwner::Track(track.id),
                track_group,
            );
        }
        for (row_index, row) in track.property_rows.iter().enumerate().filter(|(_, row)| {
            self.clips
                .iter()
                .any(|clip| clip.track == track.id && row.matches(clip))
        }) {
            let owner = KeyframeOwner::SourceRow {
                track: track.id,
                row: row_index,
            };
            let group = KeyframeGroupOwner::Target(owner);
            let label = self
                .clips
                .iter()
                .find(|clip| clip.track == track.id && row.matches(clip))
                .map(|clip| clip.name.as_str())
                .unwrap_or("Source");
            Self::append_property_lanes(
                &mut lanes,
                &row.pipeline,
                &row.composite,
                &row.model3d,
                label,
                owner,
                group.clone(),
            );
            if let VisualSource::Generator(generator) = &row.source {
                for (input, binding) in generator.parameters() {
                    if let Some(gpu) = binding.gpu() {
                        Self::append_keyframe_lanes(
                            &mut lanes,
                            KeyframeBindingTarget::new(
                                owner,
                                KeyframeProperty::Generator(input.clone()),
                            ),
                            group.clone(),
                            format!("{label} -> {input}"),
                            gpu,
                        );
                    } else {
                        Self::append_keyframe_lanes(
                            &mut lanes,
                            KeyframeBindingTarget::new(
                                owner,
                                KeyframeProperty::GeneratorHost(input.clone()),
                            ),
                            group.clone(),
                            format!("{label} -> {input}"),
                            binding,
                        );
                    }
                }
            }
        }

        let mut group_order = HashMap::new();
        for lane in &lanes {
            let next = group_order.len();
            group_order.entry(lane.id.group.clone()).or_insert(next);
        }
        lanes.sort_by_key(|lane| *group_order.get(&lane.id.group).unwrap());
        let mut seen_lanes = HashSet::with_capacity(lanes.len());
        lanes.retain(|lane| seen_lanes.insert(lane.id.clone()));

        lanes
    }

    pub(super) fn keyframe_lanes_for_track(&self, index: usize) -> &[KeyframeLane] {
        self.keyframe_lane_snapshot
            .get(index)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub(super) fn keyframe_property_groups(
        &self,
        index: usize,
    ) -> impl Iterator<Item = (usize, usize)> + '_ {
        let lanes = self.keyframe_lanes_for_track(index);
        let mut start = 0;
        std::iter::from_fn(move || {
            if start >= lanes.len() {
                return None;
            }
            let group_start = start;
            let target = &lanes[group_start].id.group;
            start += 1;
            while start < lanes.len() && lanes[start].id.group == *target {
                start += 1;
            }
            Some((group_start, start))
        })
    }

    pub(super) fn keyframe_property_open_amount(&self, id: &KeyframeLaneId) -> f32 {
        self.keyframe_lane_expansion
            .get(&id.group)
            .copied()
            .unwrap_or_else(|| self.expanded_keyframe_lanes.contains(&id.group) as u8 as f32)
    }

    pub(super) fn keyframe_property_is_expanded(&self, id: &KeyframeLaneId) -> bool {
        self.expanded_keyframe_lanes.contains(&id.group)
    }

    pub(super) fn keyframe_property_graph_is_settled(&self, id: &KeyframeLaneId) -> bool {
        if !self.keyframe_property_is_expanded(id) {
            return false;
        }
        (self.keyframe_property_open_amount(id) - 1.0).abs() <= 0.001
    }

    pub(super) fn keyframe_property_height(&self, lane: &KeyframeLane) -> f32 {
        let open = self.keyframe_property_open_amount(&lane.id);
        KEYFRAME_LANE_H + (KEYFRAME_CURVE_H - KEYFRAME_LANE_H) * open
    }

    pub(super) fn keyframe_rows_height(&self, index: usize) -> f32 {
        let Some(track) = self.tracks.get(index) else {
            return 0.0;
        };
        if let Some(height) = self.keyframe_row_heights.get(&track.id) {
            return *height;
        }
        let open = self.keyframe_track_open_amount(track.id);
        if open <= 0.001 {
            return 0.0;
        }
        self.keyframe_property_groups(index)
            .map(|(start, _)| {
                self.keyframe_property_height(&self.keyframe_lanes_for_track(index)[start])
            })
            .sum::<f32>()
            * open
    }

    pub(super) fn display_track_height(&self, index: usize) -> f32 {
        self.tracks[index].height + self.keyframe_rows_height(index)
    }

    pub(super) fn keyframe_property_rects(
        &self,
        layout: TimelineLayout,
        index: usize,
        header: bool,
    ) -> Vec<(usize, usize, Rect)> {
        let lanes = self.keyframe_lanes_for_track(index);
        let track_open = self.keyframe_track_open_amount(self.tracks[index].id);
        if lanes.is_empty() || track_open <= 0.001 {
            return Vec::new();
        }
        let x = if header {
            layout.header_body.x
        } else {
            layout.body.x
        };
        let width = if header {
            layout.header_body.width
        } else {
            layout.body.width
        };
        let mut y = self.track_y(layout, index) + self.tracks[index].height;
        self.keyframe_property_groups(index)
            .map(|(start, end)| {
                let height = self.keyframe_property_height(&lanes[start]) * track_open;
                let rect = Rect::new(x, y, width, height);
                y += height;
                (start, end, rect)
            })
            .collect()
    }

    pub(super) fn keyframe_track_toggle_rect(&self, layout: TimelineLayout, index: usize) -> Rect {
        let header = self.track_header_rect(layout, index);
        Rect::new(header.x + 3.0, header.bottom() - 19.0, 18.0, 18.0)
    }

    pub(super) fn keyframe_lane_toggle_rect(rect: Rect) -> Rect {
        Rect::new(
            rect.x + 4.0,
            rect.y + 3.0,
            18.0,
            18.0_f32.min((rect.height - 6.0).max(1.0)),
        )
    }

    pub(super) fn keyframes_at(
        &self,
        layout: TimelineLayout,
        point: [f32; 2],
    ) -> Vec<(KeyframeLaneId, f64)> {
        let mut hits = Vec::new();
        for index in 0..self.tracks.len() {
            let lanes = self.keyframe_lanes_for_track(index);
            for (start, end, rect) in self.keyframe_property_rects(layout, index, false) {
                if !rect.contains(point) {
                    continue;
                }
                let expanded = self.keyframe_property_graph_is_settled(&lanes[start].id);
                let axis_range =
                    expanded.then(|| keyframe_property_axis_range(&lanes[start..end], rect.height));
                for lane in &lanes[start..end] {
                    if let Some(hit) = lane.points.iter().min_by(|a, b| {
                        let a_x = self.time_x(layout, a.time as f32);
                        let b_x = self.time_x(layout, b.time as f32);
                        let a_y = keyframe_value_y(rect, axis_range, a.value);
                        let b_y = keyframe_value_y(rect, axis_range, b.value);
                        let a_distance = (a_x - point[0]).hypot(a_y - point[1]);
                        let b_distance = (b_x - point[0]).hypot(b_y - point[1]);
                        a_distance.total_cmp(&b_distance)
                    }) {
                        let x = self.time_x(layout, hit.time as f32);
                        let y = keyframe_value_y(rect, axis_range, hit.value);
                        if (x - point[0]).hypot(y - point[1]) <= 7.0 {
                            hits.push((lane.id.clone(), hit.time));
                        }
                    }
                }
            }
        }
        hits
    }

    pub(super) fn keyframe_at(
        &self,
        layout: TimelineLayout,
        point: [f32; 2],
    ) -> Option<(KeyframeLaneId, f64)> {
        self.keyframes_at(layout, point).into_iter().next()
    }

    pub(super) fn keyframe_easing_at(
        &self,
        layout: TimelineLayout,
        point: [f32; 2],
    ) -> Option<KeyframeEaseDrag> {
        for index in 0..self.tracks.len() {
            let lanes = self.keyframe_lanes_for_track(index);
            for (start, end, rect) in self.keyframe_property_rects(layout, index, false) {
                if !rect.contains(point)
                    || !self.keyframe_property_graph_is_settled(&lanes[start].id)
                {
                    continue;
                }
                let axis_range = keyframe_property_axis_range(&lanes[start..end], rect.height);
                for lane in &lanes[start..end] {
                    for pair in lane.points.windows(2) {
                        let a = pair[0];
                        let b = pair[1];
                        if a.interpolation == Interpolation::Step || b.time <= a.time {
                            continue;
                        }
                        let (out, incoming) = keyframe_easing(a, b);
                        let (out_pos, in_pos) = keyframe_control_positions(
                            self,
                            layout,
                            rect,
                            Some(axis_range),
                            a,
                            b,
                            out,
                            incoming,
                        );
                        if self.keyframe_is_selected(&lane.id, a.time)
                            && (out_pos[0] - point[0]).hypot(out_pos[1] - point[1]) <= 7.0
                        {
                            return Some(KeyframeEaseDrag {
                                kind: KeyframeEaseDragKind::Control {
                                    lane: lane.id.clone(),
                                    key_time: a.time,
                                    incoming: false,
                                },
                                rect,
                                axis_range,
                                left_time: a.time,
                                right_time: b.time,
                                left_value: a.value,
                                right_value: b.value,
                            });
                        }
                        if self.keyframe_is_selected(&lane.id, b.time)
                            && (in_pos[0] - point[0]).hypot(in_pos[1] - point[1]) <= 7.0
                        {
                            return Some(KeyframeEaseDrag {
                                kind: KeyframeEaseDragKind::Control {
                                    lane: lane.id.clone(),
                                    key_time: b.time,
                                    incoming: true,
                                },
                                rect,
                                axis_range,
                                left_time: a.time,
                                right_time: b.time,
                                left_value: a.value,
                                right_value: b.value,
                            });
                        }
                        let mid_mix = keyframe_segment_amount(a, b, 0.5) as f64;
                        let mid_time = a.time + (b.time - a.time) * 0.5;
                        let mid_value = a.value + (b.value - a.value) * mid_mix;
                        let mid = [
                            self.time_x(layout, mid_time as f32),
                            keyframe_value_y(rect, Some(axis_range), mid_value),
                        ];
                        if (mid[0] - point[0]).hypot(mid[1] - point[1]) <= 7.0 {
                            return Some(KeyframeEaseDrag {
                                kind: KeyframeEaseDragKind::Midpoint {
                                    lane: lane.id.clone(),
                                    left_time: a.time,
                                    right_time: b.time,
                                },
                                rect,
                                axis_range,
                                left_time: a.time,
                                right_time: b.time,
                                left_value: a.value,
                                right_value: b.value,
                            });
                        }
                    }
                }
            }
        }
        None
    }

    pub(super) fn update_keyframe_easing_drag(
        &mut self,
        layout: TimelineLayout,
        drag: &KeyframeEaseDrag,
        point: [f32; 2],
    ) {
        let span = (drag.right_time - drag.left_time).max(1.0e-9);
        let time = f64::from(self.time_at(layout, point[0]));
        let x = ((time - drag.left_time) / span).clamp(0.001, 0.999) as f32;
        let value = keyframe_value_at_y(drag.rect, drag.axis_range, point[1]);
        let delta = drag.right_value - drag.left_value;
        let y = if delta.abs() <= 1.0e-9 {
            0.5
        } else {
            ((value - drag.left_value) / delta).clamp(-4.0, 4.0) as f32
        };
        match &drag.kind {
            KeyframeEaseDragKind::Control {
                lane,
                key_time,
                incoming,
            } => {
                let handle = if *incoming {
                    EasingHandle {
                        x: 1.0 - x,
                        y: 1.0 - y,
                    }
                } else {
                    EasingHandle { x, y }
                };
                self.edit_keyframe_lane_easing(lane, *key_time, *incoming, handle);
            }
            KeyframeEaseDragKind::Midpoint {
                lane,
                left_time,
                right_time,
            } => {
                let out = EasingHandle {
                    x: (x * (2.0 / 3.0)).clamp(0.001, 0.999),
                    y: y * (2.0 / 3.0),
                };
                let incoming = EasingHandle {
                    x: ((1.0 - x) * (2.0 / 3.0)).clamp(0.001, 0.999),
                    y: (1.0 - y) * (2.0 / 3.0),
                };
                self.edit_keyframe_lane_easing(lane, *left_time, false, out);
                self.edit_keyframe_lane_easing(lane, *right_time, true, incoming);
            }
        }
    }

    pub(super) fn keyframe_editor_mut(
        &mut self,
        target: &KeyframeBindingTarget,
    ) -> Option<&mut dyn ScalarKeyframeBinding> {
        if target.is_host() {
            self.keyframe_host_binding_mut(target)
                .map(|binding| binding as &mut dyn ScalarKeyframeBinding)
        } else {
            self.keyframe_binding_mut(target)
                .map(|binding| binding as &mut dyn ScalarKeyframeBinding)
        }
    }

    pub(super) fn edit_keyframe_lane_key(
        &mut self,
        lane: &KeyframeLaneId,
        time: f64,
        next_time: Option<f64>,
        next_value: Option<f32>,
        interpolation: Option<Interpolation>,
    ) -> bool {
        self.keyframe_editor_mut(&lane.target)
            .is_some_and(|editor| {
                editor.edit_scalar_key(lane.component, time, next_time, next_value, interpolation)
            })
    }

    pub(super) fn edit_keyframe_lane_easing(
        &mut self,
        lane: &KeyframeLaneId,
        time: f64,
        incoming: bool,
        handle: EasingHandle,
    ) -> bool {
        self.keyframe_editor_mut(&lane.target)
            .is_some_and(|editor| {
                editor.edit_scalar_key_easing(lane.component, time, incoming, handle)
            })
    }

    pub(super) fn remove_keyframe_lane_key(&mut self, lane: &KeyframeLaneId, time: f64) -> bool {
        self.keyframe_editor_mut(&lane.target)
            .is_some_and(|editor| editor.remove_scalar_key(lane.component, time))
    }

    pub(super) fn keyframe_is_selected(&self, lane: &KeyframeLaneId, time: f64) -> bool {
        self.selected_keyframes.iter().any(|selected| {
            selected.lane == *lane && (selected.time - time).abs() <= 1.0 / 24_000.0
        })
    }

    pub(super) fn select_keyframe(&mut self, lane: KeyframeLaneId, time: f64, additive: bool) {
        if additive {
            if let Some(index) = self.selected_keyframes.iter().position(|selected| {
                selected.lane == lane && (selected.time - time).abs() <= 1.0 / 24_000.0
            }) {
                self.selected_keyframes.remove(index);
            } else {
                self.selected_keyframes
                    .push(SelectedKeyframe { lane, time });
            }
        } else if !self.keyframe_is_selected(&lane, time) {
            self.selected_keyframes.clear();
            self.selected_keyframes
                .push(SelectedKeyframe { lane, time });
        }
    }

    pub(super) fn keyframe_lane_geometry(
        &self,
        layout: TimelineLayout,
        id: &KeyframeLaneId,
    ) -> Option<(KeyframeLane, Rect)> {
        for index in 0..self.tracks.len() {
            let lanes = self.keyframe_lanes_for_track(index);
            for (start, end, rect) in self.keyframe_property_rects(layout, index, false) {
                if let Some(lane) = lanes[start..end].iter().find(|lane| lane.id == *id) {
                    return Some((lane.clone(), rect));
                }
            }
        }
        None
    }

    pub(super) fn keyframe_property_axis_for_lane(
        &self,
        layout: TimelineLayout,
        id: &KeyframeLaneId,
    ) -> Option<((f64, f64), Rect)> {
        for index in 0..self.tracks.len() {
            let lanes = self.keyframe_lanes_for_track(index);
            for (start, end, rect) in self.keyframe_property_rects(layout, index, false) {
                if lanes[start..end].iter().any(|lane| lane.id == *id) {
                    return Some((
                        keyframe_property_axis_range(&lanes[start..end], rect.height),
                        rect,
                    ));
                }
            }
        }
        None
    }

    pub(super) fn begin_keyframe_drag(
        &self,
        layout: TimelineLayout,
        _start: [f32; 2],
    ) -> Vec<KeyframeDragPoint> {
        self.selected_keyframes
            .iter()
            .filter_map(|selected| {
                let (lane, rect) = self.keyframe_lane_geometry(layout, &selected.lane)?;
                let point = lane
                    .points
                    .iter()
                    .find(|point| (point.time - selected.time).abs() <= 1.0 / 24_000.0)?;
                let expanded = self.keyframe_property_graph_is_settled(&lane.id);
                let ((minimum, maximum), _) = self
                    .keyframe_property_axis_for_lane(layout, &lane.id)
                    .unwrap_or((keyframe_axis_range(&lane, rect.height), rect));
                let value_per_pixel = ((maximum - minimum) / rect.height.max(1.0) as f64) as f32;
                Some(KeyframeDragPoint {
                    lane: selected.lane.clone(),
                    origin_time: selected.time,
                    current_time: selected.time,
                    origin_value: point.value as f32,
                    value_per_pixel,
                    vertical: expanded,
                })
            })
            .collect()
    }

    pub(super) fn set_selected_keyframe_interpolation(&mut self, interpolation: Interpolation) {
        let selected = self.selected_keyframes.clone();
        for key in selected {
            self.edit_keyframe_lane_key(&key.lane, key.time, None, None, Some(interpolation));
        }
    }

    pub(super) fn delete_selected_keyframes(&mut self) {
        let selected = std::mem::take(&mut self.selected_keyframes);
        for key in selected {
            self.remove_keyframe_lane_key(&key.lane, key.time);
        }
    }

    pub(super) fn set_selected_keyframe_value(&mut self, value: f32) {
        let selected = self.selected_keyframes.clone();
        for key in selected {
            self.edit_keyframe_lane_key(&key.lane, key.time, None, Some(value), None);
        }
    }

    pub(super) fn selected_keyframe_value(&self) -> Option<f32> {
        let selected = self.selected_keyframes.first()?;
        for index in 0..self.tracks.len() {
            for lane in self.keyframe_lanes_for_track(index) {
                if lane.id != selected.lane {
                    continue;
                }
                return lane
                    .points
                    .iter()
                    .find(|point| (point.time - selected.time).abs() <= 1.0 / 24_000.0)
                    .map(|point| point.value as f32);
            }
        }
        None
    }
}
