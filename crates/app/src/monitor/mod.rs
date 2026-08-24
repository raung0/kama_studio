use std::{cell::RefCell, collections::HashMap};

use crate::{
    assets::{AppIcon, Icons},
    effects::{GpuValue, PipelineInstance},
    gradient::{
        colors_from_values, colors_to_values, insert_midpoint, inserted_color,
        normalized_midpoints, remove_midpoint,
    },
    i18n,
    panels::GraphMonitorSelection,
    playback::{
        generator_content_bounds, tight_generator_source_geometry, PreviewOutput, SourceGeometry,
    },
    plugin::{EffectRole, GeneratorDefinition, InputType, MonitorHandleMode, PluginRegistry},
    project::{GeneratorSource, Project, VisualSource},
    runtime::wasm::{plugin_parameter_hash, WasmRuntime},
    theme,
    timeline::{Clip, TimelineState, TrackKind},
};
use kama_ui::{
    components::{ComboBox, ComboBoxOpenDirection, ToggleButton},
    Color, IconId, Rect, Size,
};
use winit::keyboard::ModifiersState;

labeled_enum! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    enum PreviewResolution {
        Full => "Full",
        Half => "1/2",
        Quarter => "1/4",
        Eighth => "1/8",
    }
}

impl PreviewResolution {
    fn divisor(self) -> u32 {
        match self {
            Self::Full => 1,
            Self::Half => 2,
            Self::Quarter => 4,
            Self::Eighth => 8,
        }
    }

    fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|candidate| *candidate == self)
            .unwrap_or(0)
    }
}

#[derive(Clone, Copy, Debug)]
enum TransformGizmoHandle {
    Move,
    Scale(usize),
    Anchor,
}

#[derive(Clone, Debug)]
struct TransformGizmoDrag {
    handle: TransformGizmoHandle,
    preview: Rect,
    start: [f32; 2],
    position: [f32; 2],
    position_offset: [f32; 2],
    scale: [f32; 2],
    anchor: [f32; 2],
    rotation: f32,
    keep_position_on_scale: bool,
    canvas_size: [f32; 2],
    source_size: [f32; 2],
    screen_x: [f32; 3],
    screen_y: [f32; 3],
    group: Option<TransformGizmoGroupDrag>,
    snap: SnapSession,
}

#[derive(Clone, Debug)]
struct TransformGizmoGroupDrag {
    reference_clip_id: u32,
    members: Vec<TransformGizmoGroupMember>,
}

#[derive(Clone, Copy, Debug)]
struct TransformGizmoGroupMember {
    clip_id: u32,
    time: f64,
    position: [f32; 2],
    position_offset: [f32; 2],
    scale: [f32; 2],
}

#[derive(Clone, Copy)]
struct GizmoScaleChange {
    scale: [f32; 2],
    position: [f32; 2],
    pivot: [f32; 2],
    factor: [f32; 2],
}

fn gizmo_scale_change(
    drag: &mut TransformGizmoDrag,
    index: usize,
    point: [f32; 2],
    modifiers: ModifiersState,
) -> Option<GizmoScaleChange> {
    let canvas = drag.canvas_size;
    let source_size = drag.source_size;
    let effective_position = [
        drag.position[0] + drag.position_offset[0],
        drag.position[1] + drag.position_offset[1],
    ];
    let correction = drag.snap.snap([point[0]; 3], [point[1]; 3], 8.0);
    let cursor = screen_to_project(
        drag.preview,
        [point[0] + correction[0], point[1] + correction[1]],
        canvas,
    );
    let corners = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
    let corner = [
        corners[index][0] * source_size[0],
        corners[index][1] * source_size[1],
    ];

    let pivot_source = if modifiers.control_key() || drag.keep_position_on_scale {
        [source_size[0] * 0.5, source_size[1] * 0.5]
    } else {
        let opposite = corners[(index + 2) % 4];
        [opposite[0] * source_size[0], opposite[1] * source_size[1]]
    };
    let pivot = transform_source_point(
        pivot_source,
        canvas,
        source_size,
        effective_position,
        drag.scale,
        drag.anchor,
        drag.rotation,
    );
    let delta = rotate([cursor[0] - pivot[0], cursor[1] - pivot[1]], -drag.rotation);
    let source_delta = [corner[0] - pivot_source[0], corner[1] - pivot_source[1]];
    if source_delta.iter().any(|value| value.abs() <= 0.000_001) {
        return None;
    }
    let mut scale = [delta[0] / source_delta[0], delta[1] / source_delta[1]];
    if modifiers.shift_key() {
        let factors = [
            scale[0] / safe_scale(drag.scale[0]),
            scale[1] / safe_scale(drag.scale[1]),
        ];
        let factor = if (factors[0] - 1.0).abs() >= (factors[1] - 1.0).abs() {
            factors[0]
        } else {
            factors[1]
        };
        scale = [drag.scale[0] * factor, drag.scale[1] * factor];
    }
    scale.iter_mut().for_each(|value| {
        if value.is_finite() && value.abs() < 0.01 {
            *value = value.signum() * 0.01;
        }
    });
    scale.iter().all(|value| value.is_finite()).then(|| {
        let moved_pivot = transform_source_point(
            pivot_source,
            canvas,
            source_size,
            effective_position,
            scale,
            drag.anchor,
            drag.rotation,
        );
        GizmoScaleChange {
            scale,
            position: [
                drag.position[0] + (pivot[0] - moved_pivot[0]) / canvas[0],
                drag.position[1] + (pivot[1] - moved_pivot[1]) / canvas[1],
            ],
            pivot,
            factor: [
                scale[0] / safe_scale(drag.scale[0]),
                scale[1] / safe_scale(drag.scale[1]),
            ],
        }
    })
}

#[derive(Clone, Copy, Debug)]
struct SnapLock {
    target: f32,
    feature: usize,
}

#[derive(Clone, Debug, Default)]
struct SnapTargets {
    x: Vec<f32>,
    y: Vec<f32>,
}

#[derive(Clone, Debug, Default)]
struct SnapSession {
    targets: SnapTargets,
    x_lock: Option<SnapLock>,
    y_lock: Option<SnapLock>,
}

impl SnapSession {
    fn snap(&mut self, x: [f32; 3], y: [f32; 3], tolerance: f32) -> [f32; 2] {
        [
            snap_axis(x, &self.targets.x, tolerance, &mut self.x_lock),
            snap_axis(y, &self.targets.y, tolerance, &mut self.y_lock),
        ]
    }
}

#[derive(Clone, Copy, Debug)]
struct TransformGizmoGeometry {
    corners: [[f32; 2]; 4],
    anchor: Option<[f32; 2]>,
}

#[derive(Clone, Copy, Debug)]
struct PenPointHandle {
    index: usize,
    point: [f32; 2],
}

#[derive(Clone, Debug)]
enum PenEditTarget {
    Clip {
        input: String,
    },
    Graph {
        pipeline: u64,
        node: u64,
        input: String,
        time: f64,
        follows_clip: bool,
    },
}

impl PenEditTarget {
    fn follows_clip(&self) -> bool {
        matches!(
            self,
            Self::Clip { .. }
                | Self::Graph {
                    follows_clip: true,
                    ..
                }
        )
    }

    fn points(&self, project: &Project, timeline: &TimelineState) -> Option<Vec<[f32; 2]>> {
        let value = match self {
            Self::Clip { input } => timeline.generator_host_value(input),
            Self::Graph {
                pipeline,
                node,
                input,
                time,
                ..
            } => project.pipeline_node_host_value(*pipeline, *node, input, *time),
        }?;
        match value {
            crate::project::HostValue::Vec2Array(points) => Some(points),
            _ => None,
        }
    }

    fn set_points(
        &self,
        project: &mut Project,
        timeline: &mut TimelineState,
        points: Vec<[f32; 2]>,
    ) {
        self.set_host_value(
            project,
            timeline,
            self.input(),
            crate::project::HostValue::Vec2Array(points),
        );
    }

    fn input(&self) -> &str {
        match self {
            Self::Clip { input } | Self::Graph { input, .. } => input,
        }
    }

    fn host_value(
        &self,
        project: &Project,
        timeline: &TimelineState,
        input: &str,
    ) -> Option<crate::project::HostValue> {
        match self {
            Self::Clip { .. } => timeline.generator_host_value(input),
            Self::Graph {
                pipeline,
                node,
                time,
                ..
            } => project.pipeline_node_host_value(*pipeline, *node, input, *time),
        }
    }

    fn set_host_value(
        &self,
        project: &mut Project,
        timeline: &mut TimelineState,
        input: &str,
        value: crate::project::HostValue,
    ) {
        match self {
            Self::Clip { .. } => timeline.set_generator_host_value(input, value),
            Self::Graph {
                pipeline,
                node,
                time,
                ..
            } => {
                project.set_pipeline_node_host_value(*pipeline, *node, input, *time, value);
            }
        }
    }
}

fn pen_gradient_colors(
    target: &PenEditTarget,
    input: &str,
    project: &Project,
    timeline: &TimelineState,
    count: usize,
) -> Vec<[f32; 4]> {
    let values = target
        .host_value(project, timeline, input)
        .and_then(|value| match value {
            crate::project::HostValue::F32List(values) => Some(values),
            _ => None,
        })
        .unwrap_or_default();
    colors_from_values(&values, count)
}

fn set_pen_gradient_colors(
    target: &PenEditTarget,
    input: &str,
    project: &mut Project,
    timeline: &mut TimelineState,
    colors: &[[f32; 4]],
) {
    target.set_host_value(
        project,
        timeline,
        input,
        crate::project::HostValue::F32List(colors_to_values(colors)),
    );
}

fn pen_gradient_midpoints(
    target: &PenEditTarget,
    input: &str,
    project: &Project,
    timeline: &TimelineState,
    point_count: usize,
) -> Vec<f32> {
    let values = target
        .host_value(project, timeline, input)
        .and_then(|value| match value {
            crate::project::HostValue::F32List(values) => Some(values),
            _ => None,
        })
        .unwrap_or_default();
    normalized_midpoints(&values, point_count)
}

fn set_pen_gradient_midpoints(
    target: &PenEditTarget,
    input: &str,
    project: &mut Project,
    timeline: &mut TimelineState,
    midpoints: Vec<f32>,
) {
    target.set_host_value(
        project,
        timeline,
        input,
        crate::project::HostValue::F32List(midpoints),
    );
}

#[derive(Clone, Debug)]
struct PenToolDrag {
    target: PenEditTarget,
    index: usize,
    preview: Rect,
    render_size: [u32; 2],
    source_geometry: SourceGeometry,
    source_origin: [f32; 2],
    source_scale: [f32; 2],
    snap: SnapSession,
}

#[derive(Clone, Copy, Debug)]
struct GradientMidpointHandle {
    segment: usize,
    point: [f32; 2],
    start: [f32; 2],
    end: [f32; 2],
}

#[derive(Clone, Debug)]
struct GradientMidpointDrag {
    target: PenEditTarget,
    input: String,
    segment: usize,
    start: [f32; 2],
    end: [f32; 2],
    point_count: usize,
    snap: SnapSession,
}

#[derive(Clone, Debug)]
enum GeneratorVec2EditTarget {
    Clip {
        input: String,
    },
    LocalEffect {
        node: u64,
        input: String,
        follows_clip: bool,
    },
    Graph {
        pipeline: u64,
        node: u64,
        input: String,
        follows_clip: bool,
    },
}

impl GeneratorVec2EditTarget {
    fn follows_clip(&self) -> bool {
        matches!(
            self,
            Self::Clip { .. }
                | Self::LocalEffect {
                    follows_clip: true,
                    ..
                }
                | Self::Graph {
                    follows_clip: true,
                    ..
                }
        )
    }

    fn set_value(&self, project: &mut Project, timeline: &mut TimelineState, value: [f32; 2]) {
        let value = GpuValue::Vec2(value);
        match self {
            Self::Clip { input } => timeline.set_generator_value(input, value),
            Self::LocalEffect { node, input, .. } => {
                timeline.set_selected_local_node_value(*node, input, value);
            }
            Self::Graph {
                pipeline,
                node,
                input,
                follows_clip,
            } => {
                if *follows_clip {
                    timeline.set_pipeline_input_value(project, *node, input, value);
                } else {
                    project.set_pipeline_node_value(*pipeline, *node, input, value);
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct GeneratorSizeTransformDrag {
    clip_id: u32,
    time: f64,
    position: [f32; 2],
    position_offset: [f32; 2],
    scale: [f32; 2],
    anchor: [f32; 2],
    rotation: f32,
}

#[derive(Clone, Debug)]
struct GeneratorVec2Drag {
    target: GeneratorVec2EditTarget,
    mode: MonitorHandleMode,
    handle: usize,
    preview: Rect,
    render_size: [u32; 2],
    source_geometry: SourceGeometry,
    center: [f32; 2],
    parameter_scale: [f32; 2],
    value: [f32; 2],
    min: f32,
    max: f32,
    resize_transform: Option<GeneratorSizeTransformDrag>,
    snap: SnapSession,
}

#[derive(Clone, Debug)]
struct GeneratorVec2HandleSet {
    target: GeneratorVec2EditTarget,
    mode: MonitorHandleMode,
    points: Vec<PenPointHandle>,
    lines: Vec<[usize; 2]>,
    preview: Rect,
    render_size: [u32; 2],
    source_geometry: SourceGeometry,
    center: [f32; 2],
    parameter_scale: [f32; 2],
    value: [f32; 2],
    min: f32,
    max: f32,
    resize_transform: bool,
}

type MonitorSourceOverlay = (Vec<[f32; 2]>, Vec<[usize; 2]>);

#[derive(Clone, Debug)]
struct PluginHandleDrag {
    target: GeneratorVec2EditTarget,
    preview: Rect,
    render_size: [u32; 2],
    source_geometry: SourceGeometry,
    base: [f32; 2],
    snap: SnapSession,
}

#[derive(Clone, Debug)]
struct PluginPointHandle {
    point: PenPointHandle,
    target: GeneratorVec2EditTarget,
    base: [f32; 2],
}

#[derive(Clone, Debug)]
struct PluginHandleSet {
    handles: Vec<PluginPointHandle>,
    lines: Vec<[usize; 2]>,
    preview: Rect,
    render_size: [u32; 2],
    source_geometry: SourceGeometry,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MonitorAction {
    CaptureFrame,
    CaptureTemporaryFrame,
}

pub(crate) struct MonitorBuildContext<'a> {
    pub project: &'a Project,
    pub timeline: &'a TimelineState,
    pub plugins: &'a PluginRegistry,
    pub graph_selection: Option<GraphMonitorSelection>,
    pub output: PreviewOutput<'a>,
    pub icons: Icons,
}

pub(crate) struct MonitorPointerContext<'a> {
    pub modifiers: ModifiersState,
    pub project: &'a mut Project,
    pub plugins: &'a PluginRegistry,
    pub graph_selection: Option<GraphMonitorSelection>,
    pub timeline: &'a mut TimelineState,
    pub source_geometry: &'a HashMap<u32, SourceGeometry>,
}

default_state! {
    pub struct MonitorState {
        monitor_wasm: RefCell<Option<WasmRuntime>> = RefCell::new(WasmRuntime::new().ok()),
        gizmo_drag: Option<TransformGizmoDrag>,
        pen_drag: Option<PenToolDrag>,
        gradient_midpoint_drag: Option<GradientMidpointDrag>,
        generator_vec2_drag: Option<GeneratorVec2Drag>,
        plugin_handle_drag: Option<PluginHandleDrag>,
        pen_tool: bool,
        selected_pen_point: Option<usize>,
        view_pan_drag: Option<([f32; 2], [f32; 2])>,
        view_pan: [f32; 2],
        view_zoom: f32 = 1.0,
        preview_resolution: PreviewResolution = PreviewResolution::Full,
        preview_combo: ComboBox = ComboBox::new(PreviewResolution::Full.index())
            .open_direction(ComboBoxOpenDirection::Up),
        viewport_snap: bool = true,
        clip_snap: bool = true,
        master_muted: bool,
        captured_frame: Option<([u32; 2], Vec<u8>)>,
        show_captured_frame: bool,
        pending_action: Option<MonitorAction>,
    }
}

impl MonitorState {
    pub(crate) fn preview_render_size(&self, project: &Project) -> [u32; 2] {
        let (width, height) = self.preview_dimensions(project);
        [width, height]
    }

    pub(crate) fn preview_render_scale(&self, project: &Project) -> f32 {
        self.preview_scale(project)
    }

    pub(crate) fn captured_preview(&self) -> Option<([u32; 2], &[u8])> {
        self.show_captured_frame
            .then_some(self.captured_frame.as_ref())
            .flatten()
            .map(|(size, pixels)| (*size, pixels.as_slice()))
    }

    pub(crate) fn clear_captured_frame(&mut self) {
        self.captured_frame = None;
        self.show_captured_frame = false;
    }

    pub fn tick(&mut self, dt: f32) {
        self.preview_combo.tick(dt);
    }

    pub fn toggle_pen_tool(&mut self) {
        self.pen_tool = !self.pen_tool;
        self.pen_drag = None;
        self.gradient_midpoint_drag = None;
        if !self.pen_tool {
            self.selected_pen_point = None;
        }
    }

    pub fn zoom_to_fit(&mut self) {
        self.view_zoom = 1.0;
        self.view_pan = [0.0, 0.0];
        self.view_pan_drag = None;
    }

    pub fn cycle_hover_selection(
        &mut self,
        rect: Rect,
        point: [f32; 2],
        project: &Project,
        timeline: &mut TimelineState,
        source_geometry: &HashMap<u32, SourceGeometry>,
        direction: i32,
    ) -> bool {
        let preview = self.preview_rect(rect, project);
        if !preview.contains(point) {
            return false;
        }
        let (width, height) = self.preview_dimensions(project);
        let candidates = monitor_clips_at(preview, point, timeline, width, height, source_geometry);
        if candidates.len() < 2 {
            return false;
        }
        let current_id = timeline.selected_clip().map(|clip| clip.id);
        let current =
            current_id.and_then(|id| candidates.iter().position(|candidate| *candidate == id));
        let len = candidates.len() as i32;
        let next = current.map_or_else(
            || {
                if direction < 0 {
                    candidates.len() - 1
                } else {
                    0
                }
            },
            |index| (index as i32 + direction).rem_euclid(len) as usize,
        );
        timeline.select_clip_by_id(candidates[next], false)
    }

    fn preview_rect(&self, rect: Rect, project: &Project) -> Rect {
        monitor_preview_rect(
            rect,
            project.active_settings().canvas_size[0],
            project.active_settings().canvas_size[1],
            self.view_pan,
            self.view_zoom,
        )
    }

    pub fn is_animating(&self) -> bool {
        self.preview_combo.is_animating()
    }

    fn preview_dimensions(&self, project: &Project) -> (u32, u32) {
        let divisor = self.preview_resolution.divisor();
        (
            project.active_settings().canvas_size[0]
                .max(1)
                .div_ceil(divisor),
            project.active_settings().canvas_size[1]
                .max(1)
                .div_ceil(divisor),
        )
    }

    fn preview_scale(&self, project: &Project) -> f32 {
        let (width, height) = self.preview_dimensions(project);
        let sx = width as f32 / project.active_settings().canvas_size[0].max(1) as f32;
        let sy = height as f32 / project.active_settings().canvas_size[1].max(1) as f32;
        sx.min(sy)
    }

    fn set_preview_resolution(&mut self, resolution: PreviewResolution) {
        self.preview_combo.set_selected(resolution.index());
        if self.preview_resolution == resolution {
            return;
        }
        self.preview_resolution = resolution;
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn master_muted(&self) -> bool {
        self.master_muted
    }

    pub(crate) fn take_action(&mut self) -> Option<MonitorAction> {
        self.pending_action.take()
    }

    pub(crate) fn set_captured_frame(&mut self, size: [u32; 2], pixels: Vec<u8>) {
        self.captured_frame = Some((size, pixels));
    }

    pub fn build(&self, ctx: &mut kama_ui::BuildCtx, rect: Rect, view: MonitorBuildContext<'_>) {
        let MonitorBuildContext {
            project,
            timeline,
            plugins,
            graph_selection,
            output,
            icons,
        } = view;
        let source_geometry = output.source_geometry;
        let chevron = icons.get(AppIcon::Chevron);
        let local_rect = Rect::new(0.0, 0.0, rect.width, rect.height);
        let preview = self.preview_rect(local_rect, project);
        let preview_width = preview.width;
        let preview_height = preview.height;
        let (render_width, render_height) = self.preview_dimensions(project);
        let texture = output.texture;

        kama_ui::ui!(ctx, {
            Block {
                id: "monitor-root";
                width: Size::Fill;
                height: Size::Fill;
                fill: theme::timeline_bg();

                Block {
                    id: "monitor-stage";
                    bounds: (0.0, 0.0, local_rect.width, local_rect.height);

                    @rust {
                        let stage = monitor_stage_rect(local_rect);
                        let mut frame = ctx.new()
                            .id("monitor-frame")
                            .bounds((
                                preview.x - stage.x,
                                preview.y - stage.y,
                                preview_width,
                                preview_height,
                            ))
                            .fill(if texture.is_some() { Color::WHITE } else { Color::BLACK });
                        if let Some(texture) = texture {
                            frame = frame.fill_texture(texture);
                        }
                        frame.build();
                    }
                }
            }
        });

        let combo = monitor_resolution_combo_rect(local_rect);
        let option_names = PreviewResolution::ALL
            .iter()
            .map(|resolution| {
                let divisor = resolution.divisor();
                let width = project.active_settings().canvas_size[0]
                    .max(1)
                    .div_ceil(divisor);
                let height = project.active_settings().canvas_size[1]
                    .max(1)
                    .div_ceil(divisor);
                format!("{}  {}×{}", resolution.label(), width, height)
            })
            .collect::<Vec<_>>();
        let options = option_names.iter().map(String::as_str).collect::<Vec<_>>();
        self.preview_combo.build(
            ctx,
            "monitor-preview-resolution",
            combo,
            &options,
            chevron,
            crate::widgets::component_style(),
        );
        for (id, rect, icon, active, enabled, tooltip) in [
            (
                "monitor-viewport-snap",
                monitor_snap_button_rect(local_rect, false),
                AppIcon::ViewportSnap,
                self.viewport_snap,
                true,
                &i18n::text("monitor-viewport-snap"),
            ),
            (
                "monitor-clip-snap",
                monitor_snap_button_rect(local_rect, true),
                AppIcon::MonitorClipSnap,
                self.clip_snap,
                true,
                &i18n::text("monitor-clip-snap"),
            ),
            (
                "monitor-pen-tool",
                monitor_pen_button_rect(local_rect),
                AppIcon::Pen,
                self.pen_tool,
                true,
                &i18n::text("monitor-pen-tool"),
            ),
            (
                "monitor-master-mute",
                monitor_mute_button_rect(local_rect),
                AppIcon::MasterMute,
                self.master_muted,
                true,
                &i18n::text("monitor-master-mute"),
            ),
            (
                "monitor-capture-frame",
                monitor_capture_button_rect(local_rect, 0),
                AppIcon::CaptureFrame,
                false,
                true,
                &i18n::text("monitor-capture-frame"),
            ),
            (
                "monitor-capture-temp",
                monitor_capture_button_rect(local_rect, 1),
                AppIcon::CaptureTemp,
                self.captured_frame.is_some(),
                true,
                &i18n::text("monitor-temporary-capture"),
            ),
            (
                "monitor-show-capture",
                monitor_capture_button_rect(local_rect, 2),
                AppIcon::ShowCapture,
                self.show_captured_frame,
                self.captured_frame.is_some(),
                &i18n::text("monitor-show-capture"),
            ),
        ] {
            monitor_icon_toggle(ctx, id, rect, icons.get(icon), active, enabled, tooltip);
        }

        let handle_clip = monitor_stage_rect(local_rect);
        ctx.with_clip(handle_clip, |ctx| {
            let graph_transform_selected = graph_selection.is_some_and(|selection| {
                graph_selection_is_transform(timeline, plugins, selection)
            });
            if graph_selection.is_none() || graph_transform_selected {
                if let Some(geometry) = transform_gizmo_geometry(
                    preview,
                    timeline,
                    render_width,
                    render_height,
                    source_geometry,
                ) {
                    draw_transform_gizmo(ctx, geometry);
                }
            }
            let edit = MonitorEditView {
                preview,
                plugins,
                graph_selection,
                render_size: [render_width, render_height],
                source_geometry,
                monitor_wasm: &self.monitor_wasm,
            }
            .context(project, timeline);
            if let Some((handles, lines)) = selected_generator_pen_handles(edit) {
                draw_pen_tool_handles(ctx, handles, lines, self.selected_pen_point);
            }
            if let Some(handles) = selected_gradient_midpoint_handles(edit) {
                draw_gradient_midpoint_handles(ctx, handles);
            }
            if let Some(handles) = selected_plugin_handles(edit) {
                draw_plugin_handles(ctx, &handles);
            }
            if let Some(handles) = selected_generator_vec2_handles(edit) {
                draw_generator_vec2_handles(ctx, &handles);
            }
            let snap = self
                .gizmo_drag
                .as_ref()
                .map(|drag| &drag.snap)
                .or_else(|| self.gradient_midpoint_drag.as_ref().map(|drag| &drag.snap))
                .or_else(|| self.pen_drag.as_ref().map(|drag| &drag.snap))
                .or_else(|| self.generator_vec2_drag.as_ref().map(|drag| &drag.snap))
                .or_else(|| self.plugin_handle_drag.as_ref().map(|drag| &drag.snap));
            if let Some(snap) = snap {
                draw_snap_guides(
                    ctx,
                    preview,
                    snap.x_lock.map(|lock| (lock.target - rect.x, lock.feature)),
                    snap.y_lock.map(|lock| (lock.target - rect.y, lock.feature)),
                );
            }
        });
    }

    pub fn close_popups(&mut self) {
        self.preview_combo.close();
    }

    pub fn popup_contains(&self, rect: Rect, point: [f32; 2]) -> bool {
        self.preview_combo.popup_contains(
            monitor_resolution_combo_rect(rect),
            point,
            PreviewResolution::ALL.len(),
        )
    }

    pub fn scroll_popup(&self, rect: Rect, point: [f32; 2], delta: [f32; 2]) -> bool {
        self.preview_combo.scroll(
            monitor_resolution_combo_rect(rect),
            point,
            delta,
            PreviewResolution::ALL.len(),
        )
    }

    pub fn scroll(
        &mut self,
        rect: Rect,
        point: [f32; 2],
        delta: [f32; 2],
        modifiers: ModifiersState,
        project: &Project,
    ) -> bool {
        if !monitor_stage_rect(rect).contains(point) {
            return false;
        }
        if modifiers.control_key() || modifiers.super_key() {
            return self.zoom_at(rect, point, (delta[1] * 0.0025).exp(), project);
        }
        self.view_pan[0] += delta[0];
        self.view_pan[1] += delta[1];
        true
    }

    pub fn pinch_zoom(
        &mut self,
        rect: Rect,
        point: [f32; 2],
        delta: f64,
        project: &Project,
    ) -> bool {
        if !monitor_stage_rect(rect).contains(point) {
            return false;
        }
        if !delta.is_finite() || delta.abs() <= f64::EPSILON {
            return true;
        }
        self.zoom_at(rect, point, (delta as f32).exp(), project)
    }

    pub fn pointer_middle_pressed(&mut self, rect: Rect, point: [f32; 2]) -> bool {
        if !monitor_stage_rect(rect).contains(point) {
            return false;
        }
        self.view_pan_drag = Some((point, self.view_pan));
        true
    }

    pub fn pointer_middle_released(&mut self) -> bool {
        self.view_pan_drag.take().is_some()
    }

    fn zoom_at(&mut self, rect: Rect, point: [f32; 2], factor: f32, project: &Project) -> bool {
        self.set_zoom_at(
            rect,
            point,
            self.view_zoom * factor.clamp(0.5, 2.0),
            project,
        )
    }

    fn set_zoom_at(&mut self, rect: Rect, point: [f32; 2], zoom: f32, project: &Project) -> bool {
        if !monitor_stage_rect(rect).contains(point) {
            return false;
        }
        let fit = monitor_fit_preview_rect(
            rect,
            project.active_settings().canvas_size[0],
            project.active_settings().canvas_size[1],
        );
        let before = self.preview_rect(rect, project);
        let uv = [
            (point[0] - before.x) / before.width.max(1.0),
            (point[1] - before.y) / before.height.max(1.0),
        ];
        self.view_zoom = zoom.clamp(MONITOR_MIN_ZOOM, MONITOR_MAX_ZOOM);
        let width = fit.width * self.view_zoom;
        let height = fit.height * self.view_zoom;
        self.view_pan = [
            point[0] - uv[0] * width - (fit.x + (fit.width - width) * 0.5),
            point[1] - uv[1] * height - (fit.y + (fit.height - height) * 0.5),
        ];
        true
    }

    pub fn delete_selected_pen_point(
        &mut self,
        rect: Rect,
        project: &mut Project,
        timeline: &mut TimelineState,
        plugins: &PluginRegistry,
        graph_selection: Option<GraphMonitorSelection>,
        source_geometry: &HashMap<u32, SourceGeometry>,
    ) -> bool {
        if !self.pen_tool {
            return false;
        }
        let Some(index) = self.selected_pen_point else {
            return false;
        };
        let (render_width, render_height) = self.preview_dimensions(project);
        let view = MonitorEditView {
            preview: self.preview_rect(rect, project),
            plugins,
            graph_selection,
            render_size: [render_width, render_height],
            source_geometry,
            monitor_wasm: &self.monitor_wasm,
        };
        let Some(mut setup) = pen_edit_setup(view.context(project, timeline)) else {
            self.selected_pen_point = None;
            return true;
        };
        if index >= setup.points.len() {
            self.selected_pen_point = None;
            return true;
        }
        let minimum = if setup.closed { 3 } else { 1 };
        if setup.points.len() <= minimum {
            return true;
        }
        let old_point_count = setup.points.len();
        let mut gradient_colors = setup.colors_input.as_deref().map(|input| {
            pen_gradient_colors(&setup.target, input, project, timeline, old_point_count)
        });
        let mut gradient_midpoints = setup.midpoints_input.as_deref().map(|input| {
            pen_gradient_midpoints(&setup.target, input, project, timeline, old_point_count)
        });
        setup.points.remove(index);
        let remaining = setup.points.len();
        setup.target.set_points(project, timeline, setup.points);
        if let Some(colors) = gradient_colors.as_mut() {
            if index < colors.len() {
                colors.remove(index);
            }
            set_pen_gradient_colors(
                &setup.target,
                setup.colors_input.as_deref().expect("colors input exists"),
                project,
                timeline,
                colors,
            );
        }
        if let Some(midpoints) = gradient_midpoints.as_mut() {
            remove_midpoint(midpoints, index, old_point_count);
            set_pen_gradient_midpoints(
                &setup.target,
                setup
                    .midpoints_input
                    .as_deref()
                    .expect("midpoints input exists"),
                project,
                timeline,
                midpoints.clone(),
            );
        }
        self.pen_drag = None;
        self.selected_pen_point = (remaining > 0).then_some(index.min(remaining - 1));
        true
    }

    pub fn pointer_pressed(
        &mut self,
        rect: Rect,
        point: [f32; 2],
        input: MonitorPointerContext<'_>,
    ) -> bool {
        let MonitorPointerContext {
            modifiers,
            project,
            plugins,
            graph_selection,
            timeline,
            source_geometry,
        } = input;
        let combo = monitor_resolution_combo_rect(rect);
        if let Some(index) =
            self.preview_combo
                .option_at(combo, point, PreviewResolution::ALL.len())
        {
            self.preview_combo.select(index, true);
            if let Some(resolution) = PreviewResolution::ALL.get(index).copied() {
                self.set_preview_resolution(resolution);
            }
            return true;
        }
        if monitor_snap_button_rect(rect, false).contains(point) {
            self.viewport_snap = !self.viewport_snap;
            return true;
        }
        if monitor_snap_button_rect(rect, true).contains(point) {
            self.clip_snap = !self.clip_snap;
            return true;
        }
        if monitor_pen_button_rect(rect).contains(point) {
            self.toggle_pen_tool();
            return true;
        }
        if monitor_mute_button_rect(rect).contains(point) {
            self.master_muted = !self.master_muted;
            return true;
        }
        if monitor_capture_button_rect(rect, 0).contains(point) {
            self.pending_action = Some(MonitorAction::CaptureFrame);
            return true;
        }
        if monitor_capture_button_rect(rect, 1).contains(point) {
            self.pending_action = Some(MonitorAction::CaptureTemporaryFrame);
            return true;
        }
        if monitor_capture_button_rect(rect, 2).contains(point) {
            if self.captured_frame.is_some() {
                self.show_captured_frame = !self.show_captured_frame;
            }
            return true;
        }
        if combo.contains(point) {
            self.preview_combo.toggle();
            return true;
        }
        self.preview_combo.close();

        let preview = self.preview_rect(rect, project);
        let (preview_width, preview_height) = self.preview_dimensions(project);
        let edit_view = MonitorEditView {
            preview,
            plugins,
            graph_selection,
            render_size: [preview_width, preview_height],
            source_geometry,
            monitor_wasm: &self.monitor_wasm,
        };
        let handle_snap = SnapSession {
            targets: monitor_snap_targets(
                preview,
                timeline,
                preview_width,
                preview_height,
                self.viewport_snap,
                self.clip_snap,
                source_geometry,
            ),
            ..SnapSession::default()
        };
        let graph_transform_selected = graph_selection
            .is_some_and(|selection| graph_selection_is_transform(timeline, plugins, selection));
        let allow_transform_gizmo = graph_selection.is_none() || graph_transform_selected;

        if monitor_stage_rect(rect).contains(point) {
            if handle_selected_gradient_midpoint_press(
                point,
                edit_view.context(project, timeline),
                handle_snap.clone(),
                &mut self.gradient_midpoint_drag,
            ) {
                self.gizmo_drag = None;
                self.pen_drag = None;
                self.generator_vec2_drag = None;
                self.plugin_handle_drag = None;
                return true;
            }
            if handle_selected_plugin_handle_press(
                point,
                edit_view.context(project, timeline),
                handle_snap.clone(),
                &mut self.plugin_handle_drag,
            ) {
                self.gizmo_drag = None;
                self.pen_drag = None;
                self.generator_vec2_drag = None;
                return true;
            }
            if handle_selected_generator_vec2_press(
                point,
                edit_view.context(project, timeline),
                handle_snap.clone(),
                &mut self.generator_vec2_drag,
            ) {
                self.gizmo_drag = None;
                self.pen_drag = None;
                self.plugin_handle_drag = None;
                return true;
            }

            if handle_selected_generator_pen_press(
                point,
                modifiers,
                edit_view,
                self.pen_tool,
                project,
                timeline,
                handle_snap.clone(),
                &mut self.pen_drag,
                &mut self.selected_pen_point,
            ) {
                self.gizmo_drag = None;
                self.generator_vec2_drag = None;
                self.plugin_handle_drag = None;
                return true;
            }

            if self.pen_tool {
                self.gizmo_drag = None;
                self.generator_vec2_drag = None;
                self.plugin_handle_drag = None;
                return true;
            }
            if allow_transform_gizmo {
                if let Some(geometry) = transform_gizmo_geometry(
                    preview,
                    timeline,
                    preview_width,
                    preview_height,
                    source_geometry,
                ) {
                    if let Some(handle) = gizmo_handle_at(point, geometry) {
                        return self.begin_gizmo_drag(
                            handle,
                            preview,
                            point,
                            [preview_width, preview_height],
                            timeline,
                            plugins,
                            source_geometry,
                        );
                    }
                }
            }
        }
        if !preview.contains(point) {
            if rect.contains(point) && !modifiers.shift_key() {
                timeline.clear_selection();
            }
            self.gizmo_drag = None;
            self.pen_drag = None;
            self.generator_vec2_drag = None;
            self.plugin_handle_drag = None;
            return rect.contains(point);
        }

        if let Some(id) = monitor_clip_at(
            preview,
            point,
            timeline,
            preview_width,
            preview_height,
            source_geometry,
        ) {
            timeline.select_clip_by_id(id, modifiers.shift_key());
            if !modifiers.shift_key() {
                if handle_selected_generator_pen_press(
                    point,
                    modifiers,
                    edit_view,
                    self.pen_tool,
                    project,
                    timeline,
                    handle_snap.clone(),
                    &mut self.pen_drag,
                    &mut self.selected_pen_point,
                ) {
                    self.gizmo_drag = None;
                    self.plugin_handle_drag = None;
                    return true;
                }
                if allow_transform_gizmo {
                    if let Some(geometry) = transform_gizmo_geometry(
                        preview,
                        timeline,
                        preview_width,
                        preview_height,
                        source_geometry,
                    ) {
                        if point_in_quad(point, geometry.corners) {
                            return self.begin_gizmo_drag(
                                TransformGizmoHandle::Move,
                                preview,
                                point,
                                [preview_width, preview_height],
                                timeline,
                                plugins,
                                source_geometry,
                            );
                        }
                    }
                }
            }
            return true;
        }

        if !modifiers.shift_key() {
            timeline.clear_selection();
        }
        self.gizmo_drag = None;
        self.pen_drag = None;
        self.generator_vec2_drag = None;
        self.plugin_handle_drag = None;
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn begin_gizmo_drag(
        &mut self,
        handle: TransformGizmoHandle,
        preview: Rect,
        point: [f32; 2],
        preview_size: [u32; 2],
        timeline: &TimelineState,
        plugins: &PluginRegistry,
        source_geometry: &HashMap<u32, SourceGeometry>,
    ) -> bool {
        let [preview_width, preview_height] = preview_size;
        let Some(reference_clip) = selected_monitor_transform_clips(timeline)
            .into_iter()
            .next()
            .or_else(|| timeline.selected_clip())
        else {
            return false;
        };
        let reference_source_geometry = clip_source_geometry(
            source_geometry,
            reference_clip.id,
            preview_width,
            preview_height,
        );
        let reference_state = clip_transform_state(
            timeline.clip_property_pipeline(reference_clip),
            timeline.playhead(),
            reference_source_geometry.position_offset,
        );
        let position = [
            reference_state.position[0] - reference_source_geometry.position_offset[0],
            reference_state.position[1] - reference_source_geometry.position_offset[1],
        ];
        let scale = reference_state.scale;
        let anchor = reference_state.anchor;
        let rotation = reference_state.rotation;
        let geometry = transform_gizmo_geometry(
            preview,
            timeline,
            preview_width,
            preview_height,
            source_geometry,
        );
        let (screen_x, screen_y) =
            geometry.map_or(([point[0]; 3], [point[1]; 3]), geometry_features);
        let selected = selected_transform_clips(timeline);
        let group = (selected.len() > 1).then(|| TransformGizmoGroupDrag {
            reference_clip_id: reference_clip.id,
            members: selected
                .into_iter()
                .map(|clip| {
                    let source_geometry = clip_source_geometry(
                        source_geometry,
                        clip.id,
                        preview_width,
                        preview_height,
                    );
                    let time = transform_group_sample_time(clip, timeline.playhead());
                    let state = clip_transform_state(
                        timeline.clip_property_pipeline(clip),
                        time,
                        source_geometry.position_offset,
                    );
                    TransformGizmoGroupMember {
                        clip_id: clip.id,
                        time: time as f64,
                        position: [
                            state.position[0] - source_geometry.position_offset[0],
                            state.position[1] - source_geometry.position_offset[1],
                        ],
                        position_offset: source_geometry.position_offset,
                        scale: state.scale,
                    }
                })
                .collect(),
        });
        let snap = SnapSession {
            targets: monitor_snap_targets(
                preview,
                timeline,
                preview_width,
                preview_height,
                self.viewport_snap,
                self.clip_snap,
                source_geometry,
            ),
            ..SnapSession::default()
        };
        let keep_position_on_scale = match &reference_clip.source {
            VisualSource::Generator(GeneratorSource::Plugin { generator_type, .. }) => {
                generator_type == "builtin.shape"
                    || plugins
                        .generator(generator_type)
                        .is_some_and(|definition| definition.bounds.is_some())
            }
            _ => false,
        };
        let source_geometry = reference_source_geometry;
        self.gizmo_drag = Some(TransformGizmoDrag {
            handle,
            preview,
            start: point,
            position,
            position_offset: source_geometry.position_offset,
            scale,
            anchor,
            rotation,
            keep_position_on_scale,
            canvas_size: [preview_width as f32, preview_height as f32],
            source_size: [
                source_geometry.size.0.max(1) as f32,
                source_geometry.size.1.max(1) as f32,
            ],
            screen_x,
            screen_y,
            group,
            snap,
        });
        true
    }

    pub fn pointer_moved(
        &mut self,
        point: [f32; 2],
        modifiers: ModifiersState,
        project: &mut Project,
        _plugins: &PluginRegistry,
        timeline: &mut TimelineState,
    ) -> bool {
        if let Some((start, pan)) = self.view_pan_drag {
            self.view_pan = [pan[0] + point[0] - start[0], pan[1] + point[1] - start[1]];
            return true;
        }
        if let Some(mut drag) = self.plugin_handle_drag.take() {
            let correction = drag.snap.snap([point[0]; 3], [point[1]; 3], 8.0);
            let snapped_point = [point[0] + correction[0], point[1] + correction[1]];
            let Some(source) = drag_source_point(
                drag.preview,
                snapped_point,
                drag.render_size,
                drag.source_geometry,
                drag.target.follows_clip(),
                timeline,
            ) else {
                self.plugin_handle_drag = None;
                return false;
            };
            drag.target.set_value(
                project,
                timeline,
                [source[0] - drag.base[0], source[1] - drag.base[1]],
            );
            self.plugin_handle_drag = Some(drag);
            return true;
        }
        if let Some(mut drag) = self.generator_vec2_drag.take() {
            let correction = drag.snap.snap([point[0]; 3], [point[1]; 3], 8.0);
            let snapped_point = [point[0] + correction[0], point[1] + correction[1]];
            let Some(source) = drag_source_point(
                drag.preview,
                snapped_point,
                drag.render_size,
                drag.source_geometry,
                drag.target.follows_clip(),
                timeline,
            ) else {
                self.generator_vec2_drag = None;
                return false;
            };
            let extent = [
                (source[0] - drag.center[0]).abs() / drag.parameter_scale[0].max(0.000_001),
                (source[1] - drag.center[1]).abs() / drag.parameter_scale[1].max(0.000_001),
            ];
            let mut next = drag.value;
            let mut next_position = None;
            match drag.mode {
                MonitorHandleMode::Size => {
                    if let Some(resize) = drag.resize_transform {
                        let (value, position) =
                            generator_size_transform_value(&drag, resize, snapped_point, modifiers);
                        next = value;
                        next_position = Some((resize, position));
                    } else {
                        next = [extent[0] * 2.0, extent[1] * 2.0];
                    }
                }
                MonitorHandleMode::Radius => match drag.handle {
                    0 | 1 => next[0] = extent[0],
                    2 | 3 => next[1] = extent[1],
                    _ => {}
                },
                MonitorHandleMode::Points => return false,
            }
            for value in &mut next {
                *value = value.clamp(drag.min, drag.max);
            }
            drag.target.set_value(project, timeline, next);
            if let Some((resize, position)) = next_position {
                timeline.set_clip_transform_value_at(
                    resize.clip_id,
                    resize.time,
                    "position",
                    GpuValue::Vec2(position),
                );
            }
            self.generator_vec2_drag = Some(drag);
            return true;
        }
        if let Some(mut drag) = self.gradient_midpoint_drag.take() {
            let delta = [drag.end[0] - drag.start[0], drag.end[1] - drag.start[1]];
            let length_sq = delta[0] * delta[0] + delta[1] * delta[1];
            if length_sq <= 1.0e-6 {
                self.gradient_midpoint_drag = Some(drag);
                return true;
            }

            let raw_midpoint = (((point[0] - drag.start[0]) * delta[0]
                + (point[1] - drag.start[1]) * delta[1])
                / length_sq)
                .clamp(0.01, 0.99);
            let projected = [
                drag.start[0] + delta[0] * raw_midpoint,
                drag.start[1] + delta[1] * raw_midpoint,
            ];
            let _ = drag.snap.snap([projected[0]; 3], [projected[1]; 3], 8.0);

            let x_midpoint = drag.snap.x_lock.and_then(|lock| {
                (delta[0].abs() > 1.0e-6)
                    .then_some((lock.target - drag.start[0]) / delta[0])
                    .filter(|value| (0.01..=0.99).contains(value))
            });
            let y_midpoint = drag.snap.y_lock.and_then(|lock| {
                (delta[1].abs() > 1.0e-6)
                    .then_some((lock.target - drag.start[1]) / delta[1])
                    .filter(|value| (0.01..=0.99).contains(value))
            });
            let midpoint = match (x_midpoint, y_midpoint) {
                (Some(x), Some(y)) => {
                    if (x - raw_midpoint).abs() <= (y - raw_midpoint).abs() {
                        drag.snap.y_lock = None;
                        x
                    } else {
                        drag.snap.x_lock = None;
                        y
                    }
                }
                (Some(x), None) => x,
                (None, Some(y)) => y,
                (None, None) => raw_midpoint,
            };
            let mut midpoints = pen_gradient_midpoints(
                &drag.target,
                &drag.input,
                project,
                timeline,
                drag.point_count,
            );
            if let Some(value) = midpoints.get_mut(drag.segment) {
                *value = midpoint;
                set_pen_gradient_midpoints(&drag.target, &drag.input, project, timeline, midpoints);
            }
            self.gradient_midpoint_drag = Some(drag);
            return true;
        }
        if let Some(mut drag) = self.pen_drag.take() {
            let correction = drag.snap.snap([point[0]; 3], [point[1]; 3], 8.0);
            let snapped_point = [point[0] + correction[0], point[1] + correction[1]];
            let Some(mut source) = drag_source_point(
                drag.preview,
                snapped_point,
                drag.render_size,
                drag.source_geometry,
                drag.target.follows_clip(),
                timeline,
            ) else {
                self.pen_drag = None;
                return false;
            };

            source[0] = source[0] / drag.source_scale[0].max(0.000_001) + drag.source_origin[0];
            source[1] = source[1] / drag.source_scale[1].max(0.000_001) + drag.source_origin[1];

            let Some(mut points) = drag.target.points(project, timeline) else {
                self.pen_drag = None;
                return false;
            };
            let Some(value) = points.get_mut(drag.index) else {
                self.pen_drag = None;
                return false;
            };
            *value = source;
            drag.target.set_points(project, timeline, points);
            self.pen_drag = Some(drag);
            return true;
        }
        let Some(mut drag) = self.gizmo_drag.clone() else {
            return false;
        };
        if let Some(group) = drag
            .group
            .clone()
            .filter(|_| !matches!(drag.handle, TransformGizmoHandle::Anchor))
        {
            match drag.handle {
                TransformGizmoHandle::Move => {
                    let screen_dx = point[0] - drag.start[0];
                    let screen_dy = point[1] - drag.start[1];
                    let correction = drag.snap.snap(
                        drag.screen_x.map(|value| value + screen_dx),
                        drag.screen_y.map(|value| value + screen_dy),
                        8.0,
                    );
                    let delta = [
                        (screen_dx + correction[0]) / drag.preview.width.max(1.0),
                        (screen_dy + correction[1]) / drag.preview.height.max(1.0),
                    ];
                    for member in &group.members {
                        timeline.set_clip_transform_value_at(
                            member.clip_id,
                            member.time,
                            "position",
                            GpuValue::Vec2([
                                member.position[0] + delta[0],
                                member.position[1] + delta[1],
                            ]),
                        );
                    }
                }
                TransformGizmoHandle::Scale(index) => {
                    let canvas = drag.canvas_size;
                    if let Some(change) = gizmo_scale_change(&mut drag, index, point, modifiers) {
                        for member in &group.members {
                            if member.clip_id == group.reference_clip_id {
                                if !drag.keep_position_on_scale {
                                    timeline.set_clip_transform_value_at(
                                        member.clip_id,
                                        member.time,
                                        "position",
                                        GpuValue::Vec2(change.position),
                                    );
                                }
                                timeline.set_clip_transform_value_at(
                                    member.clip_id,
                                    member.time,
                                    "scale",
                                    GpuValue::Vec2(change.scale),
                                );
                                continue;
                            }

                            let effective = [
                                (member.position[0] + member.position_offset[0]) * canvas[0],
                                (member.position[1] + member.position_offset[1]) * canvas[1],
                            ];
                            let relative = rotate(
                                [
                                    effective[0] - change.pivot[0],
                                    effective[1] - change.pivot[1],
                                ],
                                -drag.rotation,
                            );
                            let moved_relative = rotate(
                                [
                                    relative[0] * change.factor[0],
                                    relative[1] * change.factor[1],
                                ],
                                drag.rotation,
                            );
                            let moved = [
                                change.pivot[0] + moved_relative[0],
                                change.pivot[1] + moved_relative[1],
                            ];
                            timeline.set_clip_transform_value_at(
                                member.clip_id,
                                member.time,
                                "position",
                                GpuValue::Vec2([
                                    moved[0] / canvas[0].max(1.0) - member.position_offset[0],
                                    moved[1] / canvas[1].max(1.0) - member.position_offset[1],
                                ]),
                            );
                            timeline.set_clip_transform_value_at(
                                member.clip_id,
                                member.time,
                                "scale",
                                GpuValue::Vec2([
                                    member.scale[0] * change.factor[0],
                                    member.scale[1] * change.factor[1],
                                ]),
                            );
                        }
                    }
                }
                TransformGizmoHandle::Anchor => unreachable!(),
            }
            self.gizmo_drag = Some(drag);
            return true;
        }
        let canvas = drag.canvas_size;
        let source_size = drag.source_size;
        let effective_position = [
            drag.position[0] + drag.position_offset[0],
            drag.position[1] + drag.position_offset[1],
        ];
        match drag.handle {
            TransformGizmoHandle::Move => {
                let screen_dx = point[0] - drag.start[0];
                let screen_dy = point[1] - drag.start[1];
                let correction = drag.snap.snap(
                    drag.screen_x.map(|value| value + screen_dx),
                    drag.screen_y.map(|value| value + screen_dy),
                    8.0,
                );
                let dx = (screen_dx + correction[0]) / drag.preview.width.max(1.0);
                let dy = (screen_dy + correction[1]) / drag.preview.height.max(1.0);
                timeline.set_transform_value(
                    "position",
                    GpuValue::Vec2([drag.position[0] + dx, drag.position[1] + dy]),
                );
            }
            TransformGizmoHandle::Anchor => {
                let cursor = screen_to_project(drag.preview, point, canvas);
                let source = inverse_transform_source_point(
                    cursor,
                    canvas,
                    source_size,
                    effective_position,
                    drag.scale,
                    drag.anchor,
                    drag.rotation,
                );
                timeline.set_transform_value(
                    "anchor",
                    GpuValue::Vec2([
                        source[0] / source_size[0].max(1.0),
                        source[1] / source_size[1].max(1.0),
                    ]),
                );
            }
            TransformGizmoHandle::Scale(index) => {
                if let Some(change) = gizmo_scale_change(&mut drag, index, point, modifiers) {
                    if !drag.keep_position_on_scale {
                        timeline.set_transform_value("position", GpuValue::Vec2(change.position));
                    }
                    timeline.set_transform_value("scale", GpuValue::Vec2(change.scale));
                }
            }
        }
        self.gizmo_drag = Some(drag);
        true
    }

    pub fn pointer_released(&mut self) -> bool {
        let pen = self.pen_drag.take().is_some();
        let gradient_midpoint = self.gradient_midpoint_drag.take().is_some();
        let generator = self.generator_vec2_drag.take().is_some();
        let plugin_handle = self.plugin_handle_drag.take().is_some();
        self.gizmo_drag.take().is_some() || pen || gradient_midpoint || generator || plugin_handle
    }
}

#[derive(Clone, Copy)]
struct MonitorChromeRects {
    fit_stage: Rect,
    combo: Rect,
    frame_snap: Rect,
    clip_snap: Rect,
    pen: Rect,
    mute: Rect,
    capture: [Rect; 3],
}

fn monitor_chrome_rects(rect: Rect) -> MonitorChromeRects {
    let vertical = kama_ui::layout::column(
        rect,
        &[
            kama_ui::layout::Item::height(0.0),
            kama_ui::layout::Item::fill(),
            kama_ui::layout::Item::height(6.0),
            kama_ui::layout::Item::height(32.0),
            kama_ui::layout::Item::height(0.0),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
        None,
    );
    let fit_stage = vertical[1];
    let status = vertical[3];
    let combo_w = 172.0_f32.min((status.width - 8.0).max(1.0));
    let parts = kama_ui::layout::row(
        status,
        &[
            kama_ui::layout::Item::width(4.0),
            kama_ui::layout::Item::new(Size::Pixels(combo_w), Size::Pixels(28.0)),
            kama_ui::layout::Item::width(6.0),
            kama_ui::layout::Item::new(Size::Pixels(28.0), Size::Pixels(28.0)),
            kama_ui::layout::Item::width(4.0),
            kama_ui::layout::Item::new(Size::Pixels(28.0), Size::Pixels(28.0)),
            kama_ui::layout::Item::width(6.0),
            kama_ui::layout::Item::new(Size::Pixels(28.0), Size::Pixels(28.0)),
            kama_ui::layout::Item::width(6.0),
            kama_ui::layout::Item::new(Size::Pixels(28.0), Size::Pixels(28.0)),
            kama_ui::layout::Item::width(6.0),
            kama_ui::layout::Item::new(Size::Pixels(28.0), Size::Pixels(28.0)),
            kama_ui::layout::Item::width(4.0),
            kama_ui::layout::Item::new(Size::Pixels(28.0), Size::Pixels(28.0)),
            kama_ui::layout::Item::width(4.0),
            kama_ui::layout::Item::new(Size::Pixels(28.0), Size::Pixels(28.0)),
            kama_ui::layout::Item::fill(),
        ],
        0.0,
        0.0,
        kama_ui::Align::Center,
    );
    MonitorChromeRects {
        fit_stage,
        combo: parts[1],
        frame_snap: parts[3],
        clip_snap: parts[5],
        pen: parts[7],
        mute: parts[9],
        capture: [parts[11], parts[13], parts[15]],
    }
}

fn monitor_resolution_combo_rect(rect: Rect) -> Rect {
    monitor_chrome_rects(rect).combo
}

fn monitor_icon_toggle(
    ctx: &mut kama_ui::BuildCtx,
    id: &str,
    rect: Rect,
    icon: IconId,
    active: bool,
    enabled: bool,
    tooltip: &str,
) {
    let style = crate::widgets::component_style();
    if enabled {
        ToggleButton::build(ctx, id, rect, "", active, style);
    } else {
        kama_ui::ui!(ctx, {
            Rect(("monitor-disabled-control", id), rect) {
                fill: style.control;
                border: 1;
                border_color: style.border;
                border_radius: style.radius_md;
            }
        });
    }
    kama_ui::ui!(ctx, {
        Block {
            id: @format("{}-icon", id);
            bounds: (rect.x, rect.y, rect.width, rect.height);
            content_centered;

            Icon {
                id: @format("{}-glyph", id);
                icon!: icon;
                color!: if enabled { theme::toggle_icon_color(active) } else { theme::popup_dim() };
                width: Size::Pixels(16.0);
                height: Size::Pixels(16.0);
            }
        }
        @if enabled {
            Rect(("monitor-control-tooltip", id), rect) {
                interactive;
                tooltip: tooltip;
            }
        }
    });
}

fn monitor_snap_button_rect(rect: Rect, clip: bool) -> Rect {
    let layout = monitor_chrome_rects(rect);
    if clip {
        layout.clip_snap
    } else {
        layout.frame_snap
    }
}

fn monitor_pen_button_rect(rect: Rect) -> Rect {
    monitor_chrome_rects(rect).pen
}

fn monitor_mute_button_rect(rect: Rect) -> Rect {
    monitor_chrome_rects(rect).mute
}

fn monitor_capture_button_rect(rect: Rect, index: usize) -> Rect {
    monitor_chrome_rects(rect).capture[index]
}

const MONITOR_MIN_ZOOM: f32 = 0.05;
const MONITOR_MAX_ZOOM: f32 = 16.0;

fn monitor_stage_rect(rect: Rect) -> Rect {
    rect
}

fn monitor_fit_preview_rect(rect: Rect, canvas_width: u32, canvas_height: u32) -> Rect {
    const FIT_PADDING: f32 = 8.0;

    let stage = monitor_chrome_rects(rect).fit_stage;
    let fit_area = Rect::new(
        stage.x + FIT_PADDING,
        stage.y + FIT_PADDING,
        (stage.width - FIT_PADDING * 2.0).max(1.0),
        (stage.height - FIT_PADDING).max(1.0),
    );
    let aspect = canvas_width.max(1) as f32 / canvas_height.max(1) as f32;
    let mut width = fit_area.width;
    let mut height = width / aspect;
    if height > fit_area.height {
        height = fit_area.height;
        width = height * aspect;
    }
    Rect::new(
        fit_area.x + (fit_area.width - width) * 0.5,
        fit_area.y + (fit_area.height - height) * 0.5,
        width.max(1.0),
        height.max(1.0),
    )
}

fn monitor_preview_rect(
    rect: Rect,
    canvas_width: u32,
    canvas_height: u32,
    pan: [f32; 2],
    zoom: f32,
) -> Rect {
    let fit = monitor_fit_preview_rect(rect, canvas_width, canvas_height);
    let zoom = zoom.clamp(MONITOR_MIN_ZOOM, MONITOR_MAX_ZOOM);
    let width = fit.width * zoom;
    let height = fit.height * zoom;
    Rect::new(
        fit.x + (fit.width - width) * 0.5 + pan[0],
        fit.y + (fit.height - height) * 0.5 + pan[1],
        width.max(1.0),
        height.max(1.0),
    )
}

fn clip_source_geometry(
    source_geometry: &HashMap<u32, SourceGeometry>,
    clip_id: u32,
    render_width: u32,
    render_height: u32,
) -> SourceGeometry {
    source_geometry
        .get(&clip_id)
        .copied()
        .unwrap_or_else(|| SourceGeometry::canvas(render_width, render_height))
}

#[derive(Clone, Copy, Debug)]
struct ClipTransformState {
    position: [f32; 2],
    scale: [f32; 2],
    anchor: [f32; 2],
    rotation: f32,
}

#[derive(Clone, Copy, Debug)]
struct ClipTransformSpace {
    canvas: [f32; 2],
    source_size: [f32; 2],
    transform: ClipTransformState,
}

impl ClipTransformSpace {
    fn new(
        pipeline: &PipelineInstance,
        timeline_time: f32,
        render_width: u32,
        render_height: u32,
        source_geometry: SourceGeometry,
    ) -> Self {
        Self {
            canvas: [render_width.max(1) as f32, render_height.max(1) as f32],
            source_size: [
                source_geometry.size.0.max(1) as f32,
                source_geometry.size.1.max(1) as f32,
            ],
            transform: clip_transform_state(
                pipeline,
                timeline_time,
                source_geometry.position_offset,
            ),
        }
    }

    fn source_to_project(self, source: [f32; 2]) -> [f32; 2] {
        transform_source_point(
            source,
            self.canvas,
            self.source_size,
            self.transform.position,
            self.transform.scale,
            self.transform.anchor,
            self.transform.rotation,
        )
    }

    fn project_to_source(self, projected: [f32; 2]) -> [f32; 2] {
        inverse_transform_source_point(
            projected,
            self.canvas,
            self.source_size,
            self.transform.position,
            self.transform.scale,
            self.transform.anchor,
            self.transform.rotation,
        )
    }
}

fn clip_transform_state(
    pipeline: &PipelineInstance,
    timeline_time: f32,
    position_offset: [f32; 2],
) -> ClipTransformState {
    let keyframe_time = timeline_time as f64;
    let transform = pipeline.transform();
    let value = |name: &str| {
        transform
            .and_then(|transform| transform.inputs.get(name))
            .and_then(|binding| binding.evaluate(keyframe_time))
    };
    let mut position = value("position")
        .and_then(GpuValue::vec2)
        .unwrap_or([0.5, 0.5]);
    position[0] += position_offset[0];
    position[1] += position_offset[1];
    ClipTransformState {
        position,
        scale: value("scale")
            .and_then(GpuValue::vec2)
            .unwrap_or([1.0, 1.0]),
        anchor: value("anchor")
            .and_then(GpuValue::vec2)
            .unwrap_or([0.5, 0.5]),
        rotation: value("rotation").and_then(GpuValue::f32).unwrap_or(0.0),
    }
}

fn transform_gizmo_geometry(
    preview: Rect,
    timeline: &TimelineState,
    render_width: u32,
    render_height: u32,
    source_geometry: &HashMap<u32, SourceGeometry>,
) -> Option<TransformGizmoGeometry> {
    let clip = selected_monitor_transform_clips(timeline)
        .into_iter()
        .next()
        .or_else(|| timeline.selected_clip())?;
    timeline.clip_property_pipeline(clip).transform()?;
    Some(transform_gizmo_geometry_for_clip(
        preview,
        timeline.clip_property_pipeline(clip),
        timeline.playhead(),
        render_width,
        render_height,
        clip_source_geometry(source_geometry, clip.id, render_width, render_height),
    ))
}

fn selected_transform_clips(timeline: &TimelineState) -> Vec<&Clip> {
    let reference = timeline.selected_clip().map(|clip| clip.id);
    let mut selected = timeline
        .clips()
        .iter()
        .filter(|clip| {
            timeline.is_clip_selected(clip.id)
                && clip.source.is_renderable_visual()
                && timeline.clip_property_pipeline(clip).transform().is_some()
        })
        .collect::<Vec<_>>();
    selected.sort_by_key(|clip| clip.id != reference.unwrap_or(u32::MAX));
    selected
}

fn selected_monitor_transform_clips(timeline: &TimelineState) -> Vec<&Clip> {
    let time = timeline.playhead();
    let has_video_solo = timeline
        .tracks()
        .iter()
        .any(|track| track.kind != TrackKind::Audio && track.solo);
    selected_transform_clips(timeline)
        .into_iter()
        .filter(|clip| {
            time >= clip.start
                && time < clip.end()
                && timeline
                    .tracks()
                    .iter()
                    .find(|track| track.id == clip.track)
                    .is_some_and(|track| {
                        track.kind != TrackKind::Audio
                            && !track.muted
                            && (!has_video_solo || track.solo)
                    })
        })
        .collect()
}

fn transform_group_sample_time(clip: &Clip, playhead: f32) -> f32 {
    if playhead >= clip.start && playhead < clip.end() {
        return playhead;
    }
    clip.start
}

fn transform_gizmo_geometry_for_clip(
    preview: Rect,
    pipeline: &PipelineInstance,
    timeline_time: f32,
    render_width: u32,
    render_height: u32,
    source_geometry: SourceGeometry,
) -> TransformGizmoGeometry {
    let space = ClipTransformSpace::new(
        pipeline,
        timeline_time,
        render_width,
        render_height,
        source_geometry,
    );
    let source_corners = [
        [0.0, 0.0],
        [space.source_size[0], 0.0],
        space.source_size,
        [0.0, space.source_size[1]],
    ];
    let corners = source_corners
        .map(|source| project_to_screen(preview, space.source_to_project(source), space.canvas));
    let anchor_source = [
        space.transform.anchor[0] * space.source_size[0],
        space.transform.anchor[1] * space.source_size[1],
    ];
    TransformGizmoGeometry {
        corners,
        anchor: Some(project_to_screen(
            preview,
            space.source_to_project(anchor_source),
            space.canvas,
        )),
    }
}

fn geometry_features(geometry: TransformGizmoGeometry) -> ([f32; 3], [f32; 3]) {
    let left = geometry
        .corners
        .iter()
        .map(|point| point[0])
        .fold(f32::INFINITY, f32::min);
    let right = geometry
        .corners
        .iter()
        .map(|point| point[0])
        .fold(f32::NEG_INFINITY, f32::max);
    let top = geometry
        .corners
        .iter()
        .map(|point| point[1])
        .fold(f32::INFINITY, f32::min);
    let bottom = geometry
        .corners
        .iter()
        .map(|point| point[1])
        .fold(f32::NEG_INFINITY, f32::max);
    (
        [left, (left + right) * 0.5, right],
        [top, (top + bottom) * 0.5, bottom],
    )
}

fn monitor_snap_targets(
    preview: Rect,
    timeline: &TimelineState,
    render_width: u32,
    render_height: u32,
    viewport_snap: bool,
    clip_snap: bool,
    source_geometry: &HashMap<u32, SourceGeometry>,
) -> SnapTargets {
    let mut targets = SnapTargets::default();
    if viewport_snap {
        targets
            .x
            .extend([preview.x, preview.x + preview.width * 0.5, preview.right()]);
        targets.y.extend([
            preview.y,
            preview.y + preview.height * 0.5,
            preview.bottom(),
        ]);
    }
    if clip_snap {
        let time = timeline.playhead();
        for clip in timeline.clips().iter().filter(|clip| {
            !timeline.is_clip_selected(clip.id)
                && time >= clip.start
                && time < clip.end()
                && clip.source.is_renderable_visual()
                && timeline
                    .tracks()
                    .iter()
                    .find(|track| track.id == clip.track)
                    .is_some_and(|track| track.kind != TrackKind::Audio && !track.muted)
        }) {
            let geometry = transform_gizmo_geometry_for_clip(
                preview,
                timeline.clip_property_pipeline(clip),
                time,
                render_width,
                render_height,
                clip_source_geometry(source_geometry, clip.id, render_width, render_height),
            );
            let (x, y) = geometry_features(geometry);
            targets.x.extend(x);
            targets.y.extend(y);
        }
    }
    targets.x.sort_unstable_by(f32::total_cmp);
    targets.x.dedup_by(|a, b| (*a - *b).abs() < 0.01);
    targets.y.sort_unstable_by(f32::total_cmp);
    targets.y.dedup_by(|a, b| (*a - *b).abs() < 0.01);
    targets
}

fn snap_axis(
    features: [f32; 3],
    targets: &[f32],
    tolerance: f32,
    lock: &mut Option<SnapLock>,
) -> f32 {
    if let Some(locked) = *lock {
        let distance = locked.target - features[locked.feature];
        if distance.abs() <= tolerance * 1.75 {
            return distance;
        }
        *lock = None;
    }
    let best = features
        .iter()
        .enumerate()
        .flat_map(|(feature, value)| {
            targets
                .iter()
                .map(move |target| (feature, *target, *target - *value))
        })
        .min_by(|a, b| a.2.abs().total_cmp(&b.2.abs()));
    let Some((feature, target, distance)) = best.filter(|best| best.2.abs() <= tolerance) else {
        return 0.0;
    };
    *lock = Some(SnapLock { target, feature });
    distance
}

fn transform_source_point(
    source: [f32; 2],
    canvas: [f32; 2],
    source_size: [f32; 2],
    position: [f32; 2],
    scale: [f32; 2],
    anchor: [f32; 2],
    rotation: f32,
) -> [f32; 2] {
    let source_center = [source_size[0] * 0.5, source_size[1] * 0.5];
    let placed_center = [position[0] * canvas[0], position[1] * canvas[1]];
    let anchor_source = [anchor[0] * source_size[0], anchor[1] * source_size[1]];
    let scaled_anchor = [
        placed_center[0] + (anchor_source[0] - source_center[0]) * scale[0],
        placed_center[1] + (anchor_source[1] - source_center[1]) * scale[1],
    ];
    let scaled = [
        placed_center[0] + (source[0] - source_center[0]) * scale[0],
        placed_center[1] + (source[1] - source_center[1]) * scale[1],
    ];
    let rotated = rotate(
        [scaled[0] - scaled_anchor[0], scaled[1] - scaled_anchor[1]],
        rotation,
    );
    [scaled_anchor[0] + rotated[0], scaled_anchor[1] + rotated[1]]
}

fn inverse_transform_source_point(
    projected: [f32; 2],
    canvas: [f32; 2],
    source_size: [f32; 2],
    position: [f32; 2],
    scale: [f32; 2],
    anchor: [f32; 2],
    rotation: f32,
) -> [f32; 2] {
    let source_center = [source_size[0] * 0.5, source_size[1] * 0.5];
    let placed_center = [position[0] * canvas[0], position[1] * canvas[1]];
    let anchor_source = [anchor[0] * source_size[0], anchor[1] * source_size[1]];
    let scaled_anchor = [
        placed_center[0] + (anchor_source[0] - source_center[0]) * scale[0],
        placed_center[1] + (anchor_source[1] - source_center[1]) * scale[1],
    ];
    let unrotated = rotate(
        [
            projected[0] - scaled_anchor[0],
            projected[1] - scaled_anchor[1],
        ],
        -rotation,
    );
    let scaled = [
        scaled_anchor[0] + unrotated[0],
        scaled_anchor[1] + unrotated[1],
    ];
    [
        source_center[0] + (scaled[0] - placed_center[0]) / safe_scale(scale[0]),
        source_center[1] + (scaled[1] - placed_center[1]) / safe_scale(scale[1]),
    ]
}

fn gizmo_handle_at(
    point: [f32; 2],
    geometry: TransformGizmoGeometry,
) -> Option<TransformGizmoHandle> {
    if geometry
        .anchor
        .is_some_and(|anchor| distance_sq(point, anchor) <= 9.0 * 9.0)
    {
        return Some(TransformGizmoHandle::Anchor);
    }
    if let Some(index) = geometry
        .corners
        .iter()
        .position(|corner| distance_sq(point, *corner) <= 9.0 * 9.0)
    {
        return Some(TransformGizmoHandle::Scale(index));
    }
    point_in_quad(point, geometry.corners).then_some(TransformGizmoHandle::Move)
}

fn monitor_clips_at(
    preview: Rect,
    point: [f32; 2],
    timeline: &TimelineState,
    render_width: u32,
    render_height: u32,
    source_geometry: &HashMap<u32, SourceGeometry>,
) -> Vec<u32> {
    let time = timeline.playhead();
    let has_video_solo = timeline
        .tracks()
        .iter()
        .any(|track| track.kind != TrackKind::Audio && track.solo);
    let mut hits = Vec::new();

    for track in timeline.tracks() {
        if matches!(track.kind, TrackKind::Audio | TrackKind::Effect)
            || track.muted
            || (has_video_solo && !track.solo)
        {
            continue;
        }
        for clip in timeline.clips().iter().rev() {
            if clip.track != track.id || time < clip.start || time >= clip.end() {
                continue;
            }
            if !clip.source.is_renderable_visual() {
                continue;
            }
            let geometry = transform_gizmo_geometry_for_clip(
                preview,
                timeline.clip_property_pipeline(clip),
                time,
                render_width,
                render_height,
                clip_source_geometry(source_geometry, clip.id, render_width, render_height),
            );
            if point_in_quad(point, geometry.corners) {
                hits.push(clip.id);
            }
        }
    }
    hits
}

fn monitor_clip_at(
    preview: Rect,
    point: [f32; 2],
    timeline: &TimelineState,
    render_width: u32,
    render_height: u32,
    source_geometry: &HashMap<u32, SourceGeometry>,
) -> Option<u32> {
    monitor_clips_at(
        preview,
        point,
        timeline,
        render_width,
        render_height,
        source_geometry,
    )
    .into_iter()
    .next()
}

fn draw_snap_guides(
    ctx: &mut kama_ui::BuildCtx,
    preview: Rect,
    x_lock: Option<(f32, usize)>,
    y_lock: Option<(f32, usize)>,
) {
    let guide = Color::rgb8(0x42, 0xd9, 0xff);
    let shadow = Color::rgba8(0x00, 0x00, 0x00, 0xb0);
    if let Some((x, feature)) = x_lock {
        for (index, (width, color)) in [(3.0, shadow), (1.0, guide)].into_iter().enumerate() {
            kama_ui::ui!(ctx, {
                Rect(
                    ("monitor-snap-guide-x", index),
                    Rect::new(x - width * 0.5, preview.y, width, preview.height),
                ) {
                    fill: color;
                }
            });
        }
        draw_snap_badge(ctx, [x + 5.0, preview.y + 5.0], "X", feature);
    }
    if let Some((y, feature)) = y_lock {
        for (index, (height, color)) in [(3.0, shadow), (1.0, guide)].into_iter().enumerate() {
            kama_ui::ui!(ctx, {
                Rect(
                    ("monitor-snap-guide-y", index),
                    Rect::new(preview.x, y - height * 0.5, preview.width, height),
                ) {
                    fill: color;
                }
            });
        }
        draw_snap_badge(ctx, [preview.x + 5.0, y + 5.0], "Y", feature);
    }
}

fn draw_snap_badge(
    ctx: &mut kama_ui::BuildCtx,
    point: [f32; 2],
    axis: &'static str,
    feature: usize,
) {
    let feature = match (axis, feature) {
        ("X", 0) => "left",
        ("X", 1) => "center",
        ("X", 2) => "right",
        ("Y", 0) => "top",
        ("Y", 1) => "center",
        ("Y", 2) => "bottom",
        _ => "edge",
    };
    kama_ui::ui!(ctx, {
        Rect(("monitor-snap-badge", axis), Rect::new(point[0], point[1], 58.0, 17.0)) {
            fill: Color::rgba8(0x09, 0x22, 0x2a, 0xe8); border: 1; border_color: Color::rgb8(0x42, 0xd9, 0xff);
            border_radius: 3.0; font_size: 8.0; text_color: Color::WHITE; text_centered;
            text: format!("{feature} → {axis}");
        }
    });
}

#[derive(Clone, Copy)]
struct MonitorEditView<'a> {
    preview: Rect,
    plugins: &'a PluginRegistry,
    graph_selection: Option<GraphMonitorSelection>,
    render_size: [u32; 2],
    source_geometry: &'a HashMap<u32, SourceGeometry>,
    monitor_wasm: &'a RefCell<Option<WasmRuntime>>,
}

impl<'a> MonitorEditView<'a> {
    fn context<'b>(
        self,
        project: &'b Project,
        timeline: &'b TimelineState,
    ) -> MonitorEditContext<'a, 'b> {
        MonitorEditContext {
            view: self,
            project,
            timeline,
        }
    }
}

#[derive(Clone, Copy)]
struct MonitorEditContext<'view, 'model> {
    view: MonitorEditView<'view>,
    project: &'model Project,
    timeline: &'model TimelineState,
}

fn graph_generator_coordinate_scale(
    project: &Project,
    render_width: u32,
    render_height: u32,
) -> [f32; 2] {
    let canvas = project.active_settings().canvas_size;
    let scale = (render_width.max(1) as f32 / canvas[0].max(1) as f32)
        .min(render_height.max(1) as f32 / canvas[1].max(1) as f32)
        .max(0.000_001);
    [scale, scale]
}

#[allow(clippy::too_many_arguments)]
fn generator_vec2_handle_set(
    edit: MonitorEditContext<'_, '_>,
    target: GeneratorVec2EditTarget,
    input: &crate::plugin::PluginInput,
    value: [f32; 2],
    parameter_scale: [f32; 2],
    geometry: SourceGeometry,
    clip: Option<&Clip>,
    source_points: MonitorSourceOverlay,
) -> Option<GeneratorVec2HandleSet> {
    let [render_width, render_height] = edit.view.render_size;
    let preview = edit.view.preview;
    let timeline_time = edit.timeline.playhead();
    let mode = input.monitor_handle?;
    let center = [
        geometry.size.0.max(1) as f32 * 0.5,
        geometry.size.1.max(1) as f32 * 0.5,
    ];
    let (source_points, lines) = source_points;
    let points = source_points
        .into_iter()
        .enumerate()
        .map(|(index, source)| PenPointHandle {
            index,
            point: clip.map_or_else(
                || {
                    project_to_screen(
                        preview,
                        source,
                        [render_width.max(1) as f32, render_height.max(1) as f32],
                    )
                },
                |clip| {
                    selected_clip_source_to_screen(
                        preview,
                        clip,
                        render_width,
                        render_height,
                        geometry,
                        source,
                        timeline_time,
                    )
                },
            ),
        })
        .collect();
    Some(GeneratorVec2HandleSet {
        target,
        mode,
        points,
        lines,
        preview,
        render_size: [render_width, render_height],
        source_geometry: geometry,
        center,
        parameter_scale,
        value,
        min: input.min.unwrap_or(0.0),
        max: input.max.unwrap_or(f32::INFINITY),
        resize_transform: input.monitor_resize_transform,
    })
}

fn selected_generator_vec2_handles(
    edit: MonitorEditContext<'_, '_>,
) -> Option<GeneratorVec2HandleSet> {
    let MonitorEditContext {
        view,
        project,
        timeline,
    } = edit;
    let MonitorEditView {
        plugins,
        graph_selection,
        render_size: [render_width, render_height],
        source_geometry,
        ..
    } = view;
    let timeline_time = timeline.playhead();
    if let Some(selection) = graph_selection {
        let (pipeline_id, node_id, follows_clip) = shared_graph_selection(selection)?;
        let pipeline = project.pipeline(pipeline_id)?;
        let node = pipeline.node(node_id)?;
        let definition = plugins.generator(&node.node_type)?;
        let time = timeline_time as f64;
        let value_for = |name: &str| {
            if follows_clip {
                timeline.pipeline_input_value(project, node_id, name)
            } else {
                node.inputs
                    .get(name)
                    .and_then(|binding| binding.evaluate(time))
            }
        };
        let input = definition.inputs.iter().find(|input| {
            matches!(
                input.monitor_handle,
                Some(MonitorHandleMode::Size | MonitorHandleMode::Radius)
            ) && input.is_visible_with(value_for)
        })?;
        let value = value_for(&input.id)?.vec2()?;
        let clip = graph_selection_clip(timeline, selection);
        let geometry = clip
            .map(|clip| clip_source_geometry(source_geometry, clip.id, render_width, render_height))
            .unwrap_or_else(|| SourceGeometry::canvas(render_width, render_height));
        let resolved = definition
            .inputs
            .iter()
            .filter_map(|definition| {
                value_for(&definition.id).map(|value| {
                    (
                        plugin_parameter_hash(&definition.id),
                        crate::project::HostValue::Gpu(value),
                    )
                })
            })
            .collect();
        let source_points = generator_vec2_overlay(
            edit,
            definition,
            &input.id,
            resolved,
            [geometry.size.0 as f32, geometry.size.1 as f32],
            time,
        )?;
        return generator_vec2_handle_set(
            edit,
            GeneratorVec2EditTarget::Graph {
                pipeline: pipeline_id,
                node: node_id,
                input: input.id.clone(),
                follows_clip: follows_clip && clip.is_some(),
            },
            input,
            value,
            graph_generator_coordinate_scale(project, render_width, render_height),
            geometry,
            clip,
            source_points,
        );
    }

    let mut clip = timeline.selected_clip()?.clone();
    clip.pipeline = timeline.clip_property_pipeline(&clip).clone();
    let GeneratorSource::Plugin {
        generator_type,
        parameters,
    } = timeline.selected_generator()?
    else {
        return None;
    };
    let definition = plugins.generator(generator_type)?;
    let time = timeline_time as f64;
    let value_for = |name: &str| {
        parameters
            .get(name)
            .and_then(|binding| match binding.evaluate(time)? {
                crate::project::HostValue::Gpu(value) => Some(value),
                _ => None,
            })
    };
    let input = definition.inputs.iter().find(|input| {
        matches!(
            input.monitor_handle,
            Some(MonitorHandleMode::Size | MonitorHandleMode::Radius)
        ) && input.is_visible_with(value_for)
    })?;
    let value = value_for(&input.id)?.vec2()?;
    let geometry = clip_source_geometry(source_geometry, clip.id, render_width, render_height);
    let parameter_scale = match input.monitor_handle? {
        MonitorHandleMode::Size => [
            geometry.size.0.max(1) as f32 / value[0].max(0.000_001),
            geometry.size.1.max(1) as f32 / value[1].max(0.000_001),
        ],
        MonitorHandleMode::Radius => [
            geometry.size.0.max(1) as f32 / (value[0] * 2.0).max(0.000_001),
            geometry.size.1.max(1) as f32 / (value[1] * 2.0).max(0.000_001),
        ],
        MonitorHandleMode::Points => return None,
    };
    let resolved = parameters
        .iter()
        .filter_map(|(name, binding)| {
            binding
                .evaluate(time)
                .map(|value| (plugin_parameter_hash(name), value))
        })
        .collect();
    let source_points = generator_vec2_overlay(
        edit,
        definition,
        &input.id,
        resolved,
        [geometry.size.0 as f32, geometry.size.1 as f32],
        time,
    )?;
    generator_vec2_handle_set(
        edit,
        GeneratorVec2EditTarget::Clip {
            input: input.id.clone(),
        },
        input,
        value,
        parameter_scale,
        geometry,
        Some(&clip),
        source_points,
    )
}

fn generator_vec2_overlay(
    edit: MonitorEditContext<'_, '_>,
    definition: &GeneratorDefinition,
    input: &str,
    parameters: HashMap<u32, crate::project::HostValue>,
    size: [f32; 2],
    time: f64,
) -> Option<MonitorSourceOverlay> {
    let module = definition
        .monitor_module
        .as_ref()
        .or(definition.module.as_ref())?;
    let entry = definition.monitor_entry.as_deref()?;
    let overlay = edit
        .view
        .monitor_wasm
        .borrow_mut()
        .as_mut()?
        .monitor_overlay(module, entry, parameters, size, time)
        .ok()?;
    let target = plugin_parameter_hash(input);
    let positions = overlay
        .handles
        .iter()
        .filter(|handle| handle.target == target && handle.element == -1)
        .map(|handle| handle.position)
        .collect::<Vec<_>>();
    (!positions.is_empty()).then_some((positions, overlay.lines))
}

fn selected_plugin_handles(edit: MonitorEditContext<'_, '_>) -> Option<PluginHandleSet> {
    let selection = edit.view.graph_selection?;
    let [render_width, render_height] = edit.view.render_size;
    let time = edit.timeline.playhead() as f64;

    let (definition, parameters, clip, owner) = match selection {
        GraphMonitorSelection::Local { node } => {
            let instance = edit.timeline.selected_pipeline()?;
            let effect = instance
                .local_nodes
                .iter()
                .find(|candidate| candidate.id == node)?;
            let definition = edit.view.plugins.effect(&effect.node_type)?;
            let clip = edit.timeline.selected_clip().filter(|clip| {
                edit.timeline
                    .clip_property_pipeline(clip)
                    .local_nodes
                    .iter()
                    .any(|candidate| candidate.id == node)
            });
            let follows_clip = clip.is_some();
            let parameters = effect
                .inputs
                .iter()
                .filter_map(|(input, binding)| {
                    binding.evaluate(time).map(|value| {
                        (
                            plugin_parameter_hash(input),
                            crate::project::HostValue::Gpu(value),
                        )
                    })
                })
                .collect();
            (definition, parameters, clip, (None, node, follows_clip))
        }
        GraphMonitorSelection::Shared {
            pipeline,
            node,
            follows_clip,
        } => {
            let effect = edit.project.pipeline(pipeline)?.node(node)?;
            let definition = edit.view.plugins.effect(&effect.node_type)?;
            let clip = graph_selection_clip(edit.timeline, selection);
            let parameters = definition
                .inputs
                .iter()
                .filter_map(|input| {
                    let value = if follows_clip {
                        edit.timeline
                            .pipeline_input_value(edit.project, node, &input.id)
                    } else {
                        effect
                            .inputs
                            .get(&input.id)
                            .and_then(|binding| binding.evaluate(time))
                    };
                    value.map(|value| {
                        (
                            plugin_parameter_hash(&input.id),
                            crate::project::HostValue::Gpu(value),
                        )
                    })
                })
                .collect();
            (
                definition,
                parameters,
                clip,
                (Some(pipeline), node, follows_clip && clip.is_some()),
            )
        }
    };

    let clip_state = clip.map(|clip| {
        let mut state = clip.clone();
        state.pipeline = edit.timeline.clip_property_pipeline(clip).clone();
        state
    });
    let clip = clip_state.as_ref();

    let monitor = definition.monitor.as_ref()?;
    let overlay_size = clip
        .map(|clip| {
            let geometry = clip_source_geometry(
                edit.view.source_geometry,
                clip.id,
                render_width,
                render_height,
            );
            [geometry.size.0 as f32, geometry.size.1 as f32]
        })
        .unwrap_or([render_width as f32, render_height as f32]);
    let overlay = edit
        .view
        .monitor_wasm
        .borrow_mut()
        .as_mut()?
        .monitor_overlay(
            &monitor.module,
            &monitor.entry,
            parameters,
            overlay_size,
            time,
        )
        .ok()?;
    let targets = overlay
        .handles
        .iter()
        .map(|handle| {
            (handle.element == -1)
                .then(|| {
                    definition
                        .inputs
                        .iter()
                        .find(|input| plugin_parameter_hash(&input.id) == handle.target)
                        .map(|input| match owner {
                            (Some(pipeline), node, follows_clip) => {
                                GeneratorVec2EditTarget::Graph {
                                    pipeline,
                                    node,
                                    input: input.id.clone(),
                                    follows_clip,
                                }
                            }
                            (None, node, follows_clip) => GeneratorVec2EditTarget::LocalEffect {
                                node,
                                input: input.id.clone(),
                                follows_clip,
                            },
                        })
                })
                .flatten()
        })
        .collect::<Option<Vec<_>>>()?;

    let geometry = clip
        .map(|clip| {
            clip_source_geometry(
                edit.view.source_geometry,
                clip.id,
                render_width,
                render_height,
            )
        })
        .unwrap_or_else(|| SourceGeometry::canvas(render_width, render_height));
    let handles = overlay
        .handles
        .iter()
        .zip(targets)
        .enumerate()
        .map(|(index, (handle, target))| {
            let source = handle.position;
            let point = clip.map_or_else(
                || {
                    project_to_screen(
                        edit.view.preview,
                        source,
                        [render_width.max(1) as f32, render_height.max(1) as f32],
                    )
                },
                |clip| {
                    selected_clip_source_to_screen(
                        edit.view.preview,
                        clip,
                        render_width,
                        render_height,
                        geometry,
                        source,
                        edit.timeline.playhead(),
                    )
                },
            );
            PluginPointHandle {
                point: PenPointHandle { index, point },
                target,
                base: handle.origin,
            }
        })
        .collect();
    Some(PluginHandleSet {
        handles,
        lines: overlay.lines,
        preview: edit.view.preview,
        render_size: [render_width, render_height],
        source_geometry: geometry,
    })
}

fn handle_selected_plugin_handle_press(
    point: [f32; 2],
    edit: MonitorEditContext<'_, '_>,
    snap: SnapSession,
    drag: &mut Option<PluginHandleDrag>,
) -> bool {
    let Some(handles) = selected_plugin_handles(edit) else {
        return false;
    };
    let Some(handle) = handles
        .handles
        .iter()
        .find(|handle| distance_sq(point, handle.point.point) <= 12.0 * 12.0)
    else {
        return false;
    };
    *drag = Some(PluginHandleDrag {
        target: handle.target.clone(),
        preview: handles.preview,
        render_size: handles.render_size,
        source_geometry: handles.source_geometry,
        base: handle.base,
        snap,
    });
    true
}

fn handle_selected_generator_vec2_press(
    point: [f32; 2],
    edit: MonitorEditContext<'_, '_>,
    snap: SnapSession,
    drag: &mut Option<GeneratorVec2Drag>,
) -> bool {
    let Some(handles) = selected_generator_vec2_handles(edit) else {
        return false;
    };
    let Some(handle) = handles
        .points
        .iter()
        .find(|handle| distance_sq(point, handle.point) <= 11.0 * 11.0)
    else {
        return false;
    };
    let resize_transform = shape_size_transform_drag(edit, &handles);
    *drag = Some(GeneratorVec2Drag {
        target: handles.target,
        mode: handles.mode,
        handle: handle.index,
        preview: handles.preview,
        render_size: handles.render_size,
        source_geometry: handles.source_geometry,
        center: handles.center,
        parameter_scale: handles.parameter_scale,
        value: handles.value,
        min: handles.min,
        max: handles.max,
        resize_transform,
        snap,
    });
    true
}

fn shape_size_transform_drag(
    edit: MonitorEditContext<'_, '_>,
    handles: &GeneratorVec2HandleSet,
) -> Option<GeneratorSizeTransformDrag> {
    if handles.mode != MonitorHandleMode::Size || !handles.resize_transform {
        return None;
    }
    let GeneratorVec2EditTarget::Clip { .. } = &handles.target else {
        return None;
    };
    let clip = edit.timeline.selected_clip()?;
    edit.timeline.clip_property_pipeline(clip).transform()?;
    let state = clip_transform_state(
        edit.timeline.clip_property_pipeline(clip),
        edit.timeline.playhead(),
        handles.source_geometry.position_offset,
    );
    Some(GeneratorSizeTransformDrag {
        clip_id: clip.id,
        time: edit.timeline.playhead() as f64,
        position: [
            state.position[0] - handles.source_geometry.position_offset[0],
            state.position[1] - handles.source_geometry.position_offset[1],
        ],
        position_offset: handles.source_geometry.position_offset,
        scale: state.scale,
        anchor: state.anchor,
        rotation: state.rotation,
    })
}

fn generator_size_transform_value(
    drag: &GeneratorVec2Drag,
    resize: GeneratorSizeTransformDrag,
    point: [f32; 2],
    modifiers: ModifiersState,
) -> ([f32; 2], [f32; 2]) {
    let canvas = [
        drag.render_size[0].max(1) as f32,
        drag.render_size[1].max(1) as f32,
    ];
    let source_size = [
        drag.source_geometry.size.0.max(1) as f32,
        drag.source_geometry.size.1.max(1) as f32,
    ];
    let corner_uvs = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
    let corner_uv = corner_uvs[drag.handle.min(3)];
    let pivot_uv = if modifiers.control_key() {
        [0.5, 0.5]
    } else {
        corner_uvs[(drag.handle.min(3) + 2) % 4]
    };
    let pivot_source = [pivot_uv[0] * source_size[0], pivot_uv[1] * source_size[1]];
    let effective_position = [
        resize.position[0] + resize.position_offset[0],
        resize.position[1] + resize.position_offset[1],
    ];
    let fixed_pivot = transform_source_point(
        pivot_source,
        canvas,
        source_size,
        effective_position,
        resize.scale,
        resize.anchor,
        resize.rotation,
    );
    let cursor = screen_to_project(drag.preview, point, canvas);
    let local_delta = rotate(
        [cursor[0] - fixed_pivot[0], cursor[1] - fixed_pivot[1]],
        -resize.rotation,
    );
    let direction = [corner_uv[0] - pivot_uv[0], corner_uv[1] - pivot_uv[1]];
    let mut next = drag.value;
    for axis in 0..2 {
        let denominator = direction[axis] * safe_scale(resize.scale[axis]);
        if denominator.abs() > 0.000_001 {
            let next_source_size = local_delta[axis] / denominator;
            next[axis] = next_source_size / drag.parameter_scale[axis].max(0.000_001);
        }
    }
    if modifiers.shift_key() {
        next = uniform_vec2_resize(drag.value, next, drag.min, drag.max);
    } else {
        for value in &mut next {
            *value = value.clamp(drag.min, drag.max);
        }
    }

    let next_source_size = [
        next[0] * drag.parameter_scale[0].max(0.000_001),
        next[1] * drag.parameter_scale[1].max(0.000_001),
    ];
    let next_pivot_source = [
        pivot_uv[0] * next_source_size[0],
        pivot_uv[1] * next_source_size[1],
    ];
    let moved_pivot = transform_source_point(
        next_pivot_source,
        canvas,
        next_source_size,
        effective_position,
        resize.scale,
        resize.anchor,
        resize.rotation,
    );
    let position = [
        resize.position[0] + (fixed_pivot[0] - moved_pivot[0]) / canvas[0],
        resize.position[1] + (fixed_pivot[1] - moved_pivot[1]) / canvas[1],
    ];
    (next, position)
}

fn uniform_vec2_resize(start: [f32; 2], candidate: [f32; 2], min: f32, max: f32) -> [f32; 2] {
    let factor_for = |axis: usize| candidate[axis] / start[axis].max(0.000_001);
    let x_factor = factor_for(0);
    let y_factor = factor_for(1);
    let mut factor = if (x_factor - 1.0).abs() >= (y_factor - 1.0).abs() {
        x_factor
    } else {
        y_factor
    };
    let min_factor = (min / start[0].max(0.000_001)).max(min / start[1].max(0.000_001));
    let max_factor = (max / start[0].max(0.000_001)).min(max / start[1].max(0.000_001));
    factor = factor.clamp(min_factor, max_factor);
    [start[0] * factor, start[1] * factor]
}

#[derive(Clone, Debug)]
struct PenEditSetup {
    target: PenEditTarget,
    points: Vec<[f32; 2]>,
    pen_tool: bool,
    closed: bool,
    lines: Vec<[usize; 2]>,
    colors_input: Option<String>,
    midpoints_input: Option<String>,
    clip: Option<Clip>,
    preview: Rect,
    render_size: [u32; 2],
    source_geometry: SourceGeometry,
    source_origin: [f32; 2],
    source_scale: [f32; 2],
    timeline_time: f32,
}

fn generator_point_overlay(
    edit: MonitorEditContext<'_, '_>,
    definition: &GeneratorDefinition,
    input: &str,
    parameters: HashMap<u32, crate::project::HostValue>,
    size: [f32; 2],
    time: f64,
) -> Option<MonitorSourceOverlay> {
    let module = definition.module.as_ref()?;
    let entry = definition.monitor_entry.as_deref()?;
    let overlay = edit
        .view
        .monitor_wasm
        .borrow_mut()
        .as_mut()?
        .monitor_overlay(module, entry, parameters, size, time)
        .ok()?;
    let target = plugin_parameter_hash(input);
    if overlay
        .handles
        .iter()
        .any(|handle| handle.target != target || handle.element < 0)
    {
        return None;
    }
    let mut indexed = overlay
        .handles
        .iter()
        .map(|handle| (handle.element as usize, handle.position))
        .collect::<Vec<_>>();
    indexed.sort_unstable_by_key(|(index, _)| *index);
    if indexed
        .iter()
        .enumerate()
        .any(|(expected, (actual, _))| expected != *actual)
    {
        return None;
    }
    Some((
        indexed.into_iter().map(|(_, point)| point).collect(),
        overlay.lines,
    ))
}

impl PenEditSetup {
    fn has_gradient_stops(&self) -> bool {
        self.colors_input.is_some() || self.midpoints_input.is_some()
    }

    fn source_to_screen(&self, source: [f32; 2]) -> [f32; 2] {
        let source = [
            (source[0] - self.source_origin[0]) * self.source_scale[0],
            (source[1] - self.source_origin[1]) * self.source_scale[1],
        ];
        if self.target.follows_clip() {
            let clip = self
                .clip
                .as_ref()
                .expect("clip-space pen edit must have a clip");
            selected_clip_source_to_screen(
                self.preview,
                clip,
                self.render_size[0],
                self.render_size[1],
                self.source_geometry,
                source,
                self.timeline_time,
            )
        } else {
            project_to_screen(
                self.preview,
                source,
                [
                    self.render_size[0].max(1) as f32,
                    self.render_size[1].max(1) as f32,
                ],
            )
        }
    }

    fn screen_to_source(&self, point: [f32; 2]) -> [f32; 2] {
        let mut source = if self.target.follows_clip() {
            let clip = self
                .clip
                .as_ref()
                .expect("clip-space pen edit must have a clip");
            screen_to_selected_clip_source_point(
                self.preview,
                point,
                clip,
                self.render_size[0],
                self.render_size[1],
                self.source_geometry,
                self.timeline_time,
            )
        } else {
            screen_to_project(
                self.preview,
                point,
                [
                    self.render_size[0].max(1) as f32,
                    self.render_size[1].max(1) as f32,
                ],
            )
        };
        source[0] = source[0] / self.source_scale[0].max(0.000_001) + self.source_origin[0];
        source[1] = source[1] / self.source_scale[1].max(0.000_001) + self.source_origin[1];
        source
    }

    fn handles(&self) -> Vec<PenPointHandle> {
        self.points
            .iter()
            .copied()
            .enumerate()
            .map(|(index, source)| PenPointHandle {
                index,
                point: self.source_to_screen(source),
            })
            .collect()
    }

    fn drag(&self, index: usize, snap: SnapSession) -> PenToolDrag {
        PenToolDrag {
            target: self.target.clone(),
            index,
            preview: self.preview,
            render_size: self.render_size,
            source_geometry: self.source_geometry,
            source_origin: self.source_origin,
            source_scale: self.source_scale,
            snap,
        }
    }
}

fn pen_edit_setup(edit: MonitorEditContext<'_, '_>) -> Option<PenEditSetup> {
    let MonitorEditContext {
        view,
        project,
        timeline,
    } = edit;
    let MonitorEditView {
        preview,
        plugins,
        graph_selection,
        render_size: [render_width, render_height],
        source_geometry,
        ..
    } = view;
    let timeline_time = timeline.playhead();
    if let Some(selection) = graph_selection {
        let (pipeline, node, follows_clip) = shared_graph_selection(selection)?;
        let GraphGeneratorPenInput {
            input,
            time,
            pen_tool,
            closed,
            colors_input,
            midpoints_input,
        } = graph_generator_pen_input(project, selection, timeline, plugins)?;
        let clip = graph_selection_clip(timeline, selection).cloned();
        let geometry = clip
            .as_ref()
            .map(|clip| clip_source_geometry(source_geometry, clip.id, render_width, render_height))
            .unwrap_or_else(|| SourceGeometry::canvas(render_width, render_height));
        let graph_node = project.pipeline(pipeline)?.node(node)?;
        let definition = plugins.generator(&graph_node.node_type)?;
        let parameters = graph_node
            .inputs
            .iter()
            .filter_map(|(name, binding)| {
                binding.evaluate(time).map(|value| {
                    (
                        plugin_parameter_hash(name),
                        crate::project::HostValue::Gpu(value),
                    )
                })
            })
            .chain(graph_node.host_inputs.iter().filter_map(|(name, binding)| {
                binding
                    .evaluate(time)
                    .map(|value| (plugin_parameter_hash(name), value))
            }))
            .collect();
        let (points, lines) = generator_point_overlay(
            edit,
            definition,
            &input,
            parameters,
            [geometry.size.0 as f32, geometry.size.1 as f32],
            time,
        )?;
        return Some(PenEditSetup {
            target: PenEditTarget::Graph {
                pipeline,
                node,
                input,
                time,
                follows_clip: follows_clip && clip.is_some(),
            },
            points,
            pen_tool,
            closed,
            lines,
            colors_input,
            midpoints_input,
            clip,
            preview,
            render_size: [render_width, render_height],
            source_geometry: geometry,
            source_origin: [0.0, 0.0],
            source_scale: graph_generator_coordinate_scale(project, render_width, render_height),
            timeline_time,
        });
    }

    let mut clip = timeline.selected_clip()?.clone();
    if let Some(row) = timeline
        .tracks()
        .iter()
        .find(|track| track.id == clip.track)
        .and_then(|track| track.property_row(&clip.source, clip.source_instance))
    {
        clip.source = row.source.clone();
        clip.pipeline = row.pipeline.clone();
        clip.composite = row.composite.clone();
        clip.model3d = row.model3d.clone();
    }
    let cached_geometry =
        || clip_source_geometry(source_geometry, clip.id, render_width, render_height);
    let geometry = tight_generator_source_geometry(
        &clip.source,
        timeline_time as f64,
        plugins,
        project.active_settings().canvas_size,
        render_width,
        render_height,
    )
    .unwrap_or_else(cached_geometry);
    let (input, source_origin, pen_tool, closed, colors_input, midpoints_input) =
        selected_clip_pen_input(&clip, timeline_time, plugins)?;
    let VisualSource::Generator(GeneratorSource::Plugin {
        generator_type,
        parameters,
    }) = &clip.source
    else {
        return None;
    };
    let definition = plugins.generator(generator_type)?;
    let resolved = parameters
        .iter()
        .filter_map(|(name, binding)| {
            binding
                .evaluate(timeline_time as f64)
                .map(|value| (plugin_parameter_hash(name), value))
        })
        .collect();
    let (points, lines) = generator_point_overlay(
        edit,
        definition,
        &input,
        resolved,
        [geometry.size.0 as f32, geometry.size.1 as f32],
        timeline_time as f64,
    )?;
    let source_scale = selected_clip_pen_scale(
        &clip,
        timeline_time,
        plugins,
        geometry.size,
        project.active_settings().canvas_size,
    );
    Some(PenEditSetup {
        target: PenEditTarget::Clip { input },
        points,
        pen_tool,
        closed,
        lines,
        colors_input,
        midpoints_input,
        clip: Some(clip),
        preview,
        render_size: [render_width, render_height],
        source_geometry: geometry,
        source_origin,
        source_scale,
        timeline_time,
    })
}

fn selected_generator_pen_handles(
    edit: MonitorEditContext<'_, '_>,
) -> Option<(Vec<PenPointHandle>, Vec<[usize; 2]>)> {
    pen_edit_setup(edit).map(|setup| (setup.handles(), setup.lines))
}

fn selected_gradient_midpoint_handles(
    edit: MonitorEditContext<'_, '_>,
) -> Option<Vec<GradientMidpointHandle>> {
    let setup = pen_edit_setup(edit)?;
    let input = setup.midpoints_input.as_deref()?;
    if setup.points.len() < 2 {
        return None;
    }
    let points = setup.handles();
    let midpoints = pen_gradient_midpoints(
        &setup.target,
        input,
        edit.project,
        edit.timeline,
        setup.points.len(),
    );
    Some(
        points
            .windows(2)
            .enumerate()
            .map(|(segment, pair)| {
                let start = pair[0].point;
                let end = pair[1].point;
                let midpoint = midpoints.get(segment).copied().unwrap_or(0.5);
                GradientMidpointHandle {
                    segment,
                    point: [
                        start[0] + (end[0] - start[0]) * midpoint,
                        start[1] + (end[1] - start[1]) * midpoint,
                    ],
                    start,
                    end,
                }
            })
            .collect(),
    )
}

fn handle_selected_gradient_midpoint_press(
    point: [f32; 2],
    edit: MonitorEditContext<'_, '_>,
    snap: SnapSession,
    drag: &mut Option<GradientMidpointDrag>,
) -> bool {
    let Some(setup) = pen_edit_setup(edit) else {
        return false;
    };
    let Some(input) = setup.midpoints_input.clone() else {
        return false;
    };
    let Some(handles) = selected_gradient_midpoint_handles(edit) else {
        return false;
    };
    let Some(handle) = handles
        .into_iter()
        .find(|handle| distance_sq(point, handle.point) <= 9.0 * 9.0)
    else {
        return false;
    };
    *drag = Some(GradientMidpointDrag {
        target: setup.target,
        input,
        segment: handle.segment,
        start: handle.start,
        end: handle.end,
        point_count: setup.points.len(),
        snap,
    });
    true
}

#[allow(clippy::too_many_arguments)]
fn handle_selected_generator_pen_press(
    point: [f32; 2],
    modifiers: ModifiersState,
    view: MonitorEditView<'_>,
    pen_tool: bool,
    project: &mut Project,
    timeline: &mut TimelineState,
    snap: SnapSession,
    drag: &mut Option<PenToolDrag>,
    selected: &mut Option<usize>,
) -> bool {
    let Some(mut setup) = pen_edit_setup(view.context(project, timeline)) else {
        return false;
    };
    let handles = setup.handles();

    if let Some(handle) = handles
        .iter()
        .find(|handle| distance_sq(point, handle.point) <= 10.0 * 10.0)
        .copied()
    {
        *selected = Some(handle.index);
        let can_remove = if setup.closed {
            setup.points.len() > 3
        } else {
            setup.points.len() > 1
        };
        if modifiers.alt_key() && pen_tool && setup.pen_tool && can_remove {
            let old_point_count = setup.points.len();
            let mut gradient_colors = setup.colors_input.as_deref().map(|input| {
                pen_gradient_colors(&setup.target, input, project, timeline, old_point_count)
            });
            let mut gradient_midpoints = setup.midpoints_input.as_deref().map(|input| {
                pen_gradient_midpoints(&setup.target, input, project, timeline, old_point_count)
            });
            setup.points.remove(handle.index);
            setup.target.set_points(project, timeline, setup.points);
            if let Some(colors) = gradient_colors.as_mut() {
                if handle.index < colors.len() {
                    colors.remove(handle.index);
                }
                set_pen_gradient_colors(
                    &setup.target,
                    setup.colors_input.as_deref().expect("colors input exists"),
                    project,
                    timeline,
                    colors,
                );
            }
            if let Some(midpoints) = gradient_midpoints.as_mut() {
                remove_midpoint(midpoints, handle.index, old_point_count);
                set_pen_gradient_midpoints(
                    &setup.target,
                    setup
                        .midpoints_input
                        .as_deref()
                        .expect("midpoints input exists"),
                    project,
                    timeline,
                    midpoints.clone(),
                );
            }
            *drag = None;
            *selected = None;
            return true;
        }
        *drag = Some(setup.drag(handle.index, snap.clone()));
        return true;
    }

    if !pen_tool || !setup.pen_tool {
        return false;
    }
    let gradient_stops = setup.has_gradient_stops();
    let (index, projected_unselected) = if gradient_stops {
        gradient_pen_insert_target(*selected, &handles, point)
    } else {
        (pen_insert_index(*selected, setup.points.len()), None)
    };

    let old_point_count = setup.points.len();
    let mut gradient_colors = setup
        .colors_input
        .as_deref()
        .map(|input| pen_gradient_colors(&setup.target, input, project, timeline, old_point_count));
    let mut gradient_midpoints = setup.midpoints_input.as_deref().map(|input| {
        pen_gradient_midpoints(&setup.target, input, project, timeline, old_point_count)
    });
    if gradient_stops && !setup.points.is_empty() {
        if index == setup.points.len() {
            let endpoint = *setup
                .points
                .last()
                .expect("gradient has at least one point");
            setup.points.push(endpoint);
        } else {
            let projected = projected_unselected.unwrap_or_else(|| {
                project_point_to_segment(point, handles[index - 1].point, handles[index].point)
            });
            setup
                .points
                .insert(index, setup.screen_to_source(projected));
        }
    } else {
        setup.points.insert(index, setup.screen_to_source(point));
    }
    setup
        .target
        .set_points(project, timeline, setup.points.clone());
    if let Some(colors) = gradient_colors.as_mut() {
        let color = inserted_color(colors, index);
        colors.insert(index, color);
        set_pen_gradient_colors(
            &setup.target,
            setup.colors_input.as_deref().expect("colors input exists"),
            project,
            timeline,
            colors,
        );
    }
    if let Some(midpoints) = gradient_midpoints.as_mut() {
        insert_midpoint(midpoints, index, old_point_count);
        set_pen_gradient_midpoints(
            &setup.target,
            setup
                .midpoints_input
                .as_deref()
                .expect("midpoints input exists"),
            project,
            timeline,
            midpoints.clone(),
        );
    }

    let next_drag = setup.drag(index, snap);
    *selected = Some(index);
    *drag = Some(next_drag);
    true
}

fn shared_graph_selection(selection: GraphMonitorSelection) -> Option<(u64, u64, bool)> {
    match selection {
        GraphMonitorSelection::Shared {
            pipeline,
            node,
            follows_clip,
        } => Some((pipeline, node, follows_clip)),
        GraphMonitorSelection::Local { .. } => None,
    }
}

fn graph_selection_is_transform(
    timeline: &TimelineState,
    plugins: &PluginRegistry,
    selection: GraphMonitorSelection,
) -> bool {
    let GraphMonitorSelection::Local { node } = selection else {
        return false;
    };
    timeline
        .selected_pipeline()
        .and_then(|instance| {
            instance
                .local_nodes
                .iter()
                .find(|candidate| candidate.id == node)
        })
        .and_then(|node| plugins.effect(&node.node_type))
        .is_some_and(|definition| definition.role == Some(EffectRole::VisualTransform))
}

fn graph_selection_clip(
    timeline: &TimelineState,
    selection: GraphMonitorSelection,
) -> Option<&Clip> {
    let (pipeline, _, follows_clip) = shared_graph_selection(selection)?;
    if !follows_clip {
        return None;
    }
    timeline
        .selected_clip()
        .filter(|clip| timeline.clip_property_pipeline(clip).pipeline == Some(pipeline))
}

struct GraphGeneratorPenInput {
    input: String,
    time: f64,
    pen_tool: bool,
    closed: bool,
    colors_input: Option<String>,
    midpoints_input: Option<String>,
}

fn graph_generator_pen_input(
    project: &Project,
    selection: GraphMonitorSelection,
    timeline: &TimelineState,
    plugins: &PluginRegistry,
) -> Option<GraphGeneratorPenInput> {
    let (pipeline, node_id, _) = shared_graph_selection(selection)?;
    let node = project.pipeline(pipeline)?.node(node_id)?;
    let definition = plugins.generator(&node.node_type)?;
    let input = definition.inputs.iter().find(|input| {
        input.ty == InputType::Vec2Array && input.monitor_handle == Some(MonitorHandleMode::Points)
    })?;
    let time = timeline.playhead() as f64;
    Some(GraphGeneratorPenInput {
        input: input.id.clone(),
        time,
        pen_tool: input.pen_tool,
        closed: input.pen_closed,
        colors_input: input.monitor_colors.clone(),
        midpoints_input: input.monitor_midpoints.clone(),
    })
}

type SelectedClipPenInput = (String, [f32; 2], bool, bool, Option<String>, Option<String>);

fn selected_clip_pen_input(
    clip: &Clip,
    timeline_time: f32,
    plugins: &PluginRegistry,
) -> Option<SelectedClipPenInput> {
    let VisualSource::Generator(GeneratorSource::Plugin {
        generator_type,
        parameters,
    }) = &clip.source
    else {
        return None;
    };
    let definition = plugins.generator(generator_type)?;
    let input = definition.inputs.iter().find(|input| {
        input.ty == InputType::Vec2Array && input.monitor_handle == Some(MonitorHandleMode::Points)
    })?;
    let time = timeline_time as f64;
    let origin = generator_content_bounds(definition, parameters, time)
        .map(|(x, y, _, _)| [x, y])
        .unwrap_or([0.0, 0.0]);
    Some((
        input.id.clone(),
        origin,
        input.pen_tool,
        input.pen_closed,
        input.monitor_colors.clone(),
        input.monitor_midpoints.clone(),
    ))
}

fn selected_clip_pen_scale(
    clip: &Clip,
    timeline_time: f32,
    plugins: &PluginRegistry,
    source_dimensions: (u32, u32),
    canvas_size: [u32; 2],
) -> [f32; 2] {
    let VisualSource::Generator(GeneratorSource::Plugin {
        generator_type,
        parameters,
    }) = &clip.source
    else {
        return [1.0, 1.0];
    };
    let Some(definition) = plugins.generator(generator_type) else {
        return [1.0, 1.0];
    };
    if definition.bounds.is_none() {
        return [
            source_dimensions.0.max(1) as f32 / canvas_size[0].max(1) as f32,
            source_dimensions.1.max(1) as f32 / canvas_size[1].max(1) as f32,
        ];
    }
    let time = timeline_time as f64;
    let Some((_, _, width, height)) = generator_content_bounds(definition, parameters, time) else {
        return [1.0, 1.0];
    };
    [
        source_dimensions.0.max(1) as f32 / width.max(1) as f32,
        source_dimensions.1.max(1) as f32 / height.max(1) as f32,
    ]
}

fn selected_clip_source_to_screen(
    preview: Rect,
    clip: &Clip,
    render_width: u32,
    render_height: u32,
    source_geometry: SourceGeometry,
    source: [f32; 2],
    timeline_time: f32,
) -> [f32; 2] {
    let space = ClipTransformSpace::new(
        &clip.pipeline,
        timeline_time,
        render_width,
        render_height,
        source_geometry,
    );
    project_to_screen(preview, space.source_to_project(source), space.canvas)
}

#[allow(clippy::too_many_arguments)]
fn screen_to_selected_clip_source_point(
    preview: Rect,
    point: [f32; 2],
    clip: &Clip,
    render_width: u32,
    render_height: u32,
    source_geometry: SourceGeometry,
    timeline_time: f32,
) -> [f32; 2] {
    let space = ClipTransformSpace::new(
        &clip.pipeline,
        timeline_time,
        render_width,
        render_height,
        source_geometry,
    );
    space.project_to_source(screen_to_project(preview, point, space.canvas))
}

fn pen_insert_index(selected: Option<usize>, point_count: usize) -> usize {
    selected
        .filter(|index| *index < point_count)
        .map(|index| index + 1)
        .unwrap_or(point_count)
}

fn gradient_pen_insert_target(
    selected: Option<usize>,
    handles: &[PenPointHandle],
    point: [f32; 2],
) -> (usize, Option<[f32; 2]>) {
    if let Some(index) = selected.filter(|index| *index < handles.len()) {
        return (index + 1, None);
    }

    const SEGMENT_HIT_RADIUS: f32 = 14.0;
    let mut best: Option<(f32, usize, [f32; 2])> = None;
    for index in 0..handles.len().saturating_sub(1) {
        let a = handles[index].point;
        let b = handles[index + 1].point;
        let delta = [b[0] - a[0], b[1] - a[1]];
        let length_sq = delta[0] * delta[0] + delta[1] * delta[1];
        if length_sq <= 1.0e-6 {
            continue;
        }
        let t = ((point[0] - a[0]) * delta[0] + (point[1] - a[1]) * delta[1]) / length_sq;
        if !(0.0..=1.0).contains(&t) {
            continue;
        }
        let projected = [a[0] + delta[0] * t, a[1] + delta[1] * t];
        let dx = point[0] - projected[0];
        let dy = point[1] - projected[1];
        let distance_sq = dx * dx + dy * dy;
        if distance_sq > SEGMENT_HIT_RADIUS * SEGMENT_HIT_RADIUS {
            continue;
        }
        if best
            .as_ref()
            .is_none_or(|(best_distance, _, _)| distance_sq < *best_distance)
        {
            best = Some((distance_sq, index + 1, projected));
        }
    }
    best.map(|(_, index, projected)| (index, Some(projected)))
        .unwrap_or((handles.len(), None))
}

fn project_point_to_segment(point: [f32; 2], a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    let delta = [b[0] - a[0], b[1] - a[1]];
    let length_sq = delta[0] * delta[0] + delta[1] * delta[1];
    let t = if length_sq <= 1.0e-6 {
        0.0
    } else {
        (((point[0] - a[0]) * delta[0] + (point[1] - a[1]) * delta[1]) / length_sq).clamp(0.0, 1.0)
    };
    [a[0] + delta[0] * t, a[1] + delta[1] * t]
}

fn draw_gradient_midpoint_handles(
    ctx: &mut kama_ui::BuildCtx,
    handles: Vec<GradientMidpointHandle>,
) {
    let accent = Color::rgb8(0x42, 0xd9, 0xff);
    for handle in handles {
        let point = handle.point;
        kama_ui::ui!(ctx, {
            Rect(
                ("monitor-gradient-midpoint", handle.segment),
                Rect::new(point[0] - 4.0, point[1] - 4.0, 8.0, 8.0),
            ) {
                fill: Color::rgb8(0x18, 0x1b, 0x20);
                border: 2;
                border_color: accent;
                border_radius: 1.5;
                interactive;
            }
        });
    }
}

fn draw_pen_tool_handles(
    ctx: &mut kama_ui::BuildCtx,
    handles: Vec<PenPointHandle>,
    lines: Vec<[usize; 2]>,
    selected: Option<usize>,
) {
    draw_monitor_handle_set(
        ctx,
        &handles,
        &lines,
        (
            "monitor-pen-handle",
            10_000,
            Color::rgb8(0x42, 0xd9, 0xff),
            10.0,
            5.0,
        ),
        selected,
        |handle| (handle.index, handle.point),
    );
}

fn draw_plugin_handles(ctx: &mut kama_ui::BuildCtx, handles: &PluginHandleSet) {
    draw_monitor_handle_set(
        ctx,
        &handles.handles,
        &handles.lines,
        (
            "monitor-plugin-handle",
            12_300,
            Color::rgb8(0x72, 0xe0, 0xa0),
            11.0,
            5.5,
        ),
        None,
        |handle| (handle.point.index, handle.point.point),
    );
}

fn draw_generator_vec2_handles(ctx: &mut kama_ui::BuildCtx, handles: &GeneratorVec2HandleSet) {
    draw_monitor_handle_set(
        ctx,
        &handles.points,
        &handles.lines,
        (
            "monitor-generator-vec2-handle",
            12_000,
            Color::rgb8(0x42, 0xd9, 0xff),
            10.0,
            2.0,
        ),
        None,
        |handle| (handle.index, handle.point),
    );
}

fn draw_monitor_handle_set<T>(
    ctx: &mut kama_ui::BuildCtx,
    handles: &[T],
    lines: &[[usize; 2]],
    style: (&'static str, usize, Color, f32, f32),
    selected: Option<usize>,
    point: impl Fn(&T) -> (usize, [f32; 2]),
) {
    let (key, line_id, accent, size, radius) = style;
    let shadow = Color::rgba8(0, 0, 0, 0x90);
    for (index, [start, end]) in lines.iter().copied().enumerate() {
        let Some((a, b)) = handles.get(start).zip(handles.get(end)) else {
            continue;
        };
        let (_, a) = point(a);
        let (_, b) = point(b);
        draw_gizmo_line(ctx, line_id + index * 2, a, b, 3.0, shadow);
        draw_gizmo_line(ctx, line_id + index * 2 + 1, a, b, 1.25, accent);
    }
    for handle in handles {
        let (index, point) = point(handle);
        let selected = selected == Some(index);
        let size = size + if selected { 2.0 } else { 0.0 };
        kama_ui::ui!(ctx, {
            Rect((key, index), Rect::new(point[0] - size * 0.5, point[1] - size * 0.5, size, size)) {
                fill: if selected { accent } else { Color::WHITE };
                border: 2; border_color: if selected { Color::WHITE } else { accent };
                border_radius: if selected { size * 0.5 } else { radius }; interactive;
            }
        });
    }
}

fn draw_transform_gizmo(ctx: &mut kama_ui::BuildCtx, geometry: TransformGizmoGeometry) {
    let accent = Color::rgb8(0xf0, 0xa2, 0x15);
    let shadow = Color::rgba8(0x00, 0x00, 0x00, 0xa0);
    for edge in 0..4 {
        let a = geometry.corners[edge];
        let b = geometry.corners[(edge + 1) % 4];
        draw_gizmo_line(ctx, edge * 2, a, b, 3.5, shadow);
        draw_gizmo_line(ctx, edge * 2 + 1, a, b, 1.7, accent);
    }
    for (index, point) in geometry.corners.into_iter().enumerate() {
        kama_ui::ui!(ctx, {
            Rect(("monitor-transform-handle", index), Rect::new(point[0] - 5.0, point[1] - 5.0, 10.0, 10.0)) {
                fill: Color::rgb8(0xf4, 0xf4, 0xf4); border: 2; border_color: accent; border_radius: 2.0; interactive;
            }
        });
    }
    if let Some(pivot) = geometry.anchor {
        kama_ui::ui!(ctx, {
            Rect("monitor-transform-pivot-outer", Rect::new(pivot[0] - 7.0, pivot[1] - 7.0, 14.0, 14.0)) {
                fill: Color::rgba8(0x00, 0x00, 0x00, 0x80); border: 2; border_color: Color::WHITE;
                border_radius: 7.0; interactive;
            }
            Rect("monitor-transform-pivot-inner", Rect::new(pivot[0] - 2.0, pivot[1] - 2.0, 4.0, 4.0)) {
                fill: accent; border_radius: 2.0;
            }
        });
    }
}

fn draw_gizmo_line(
    ctx: &mut kama_ui::BuildCtx,
    id: usize,
    a: [f32; 2],
    b: [f32; 2],
    width: f32,
    color: Color,
) {
    let min_x = a[0].min(b[0]) - width;
    let min_y = a[1].min(b[1]) - width;
    let max_x = a[0].max(b[0]) + width;
    let max_y = a[1].max(b[1]) + width;
    let bounds = Rect::new(
        min_x,
        min_y,
        (max_x - min_x).max(1.0),
        (max_y - min_y).max(1.0),
    );
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let length = (dx * dx + dy * dy).sqrt().max(0.0001);
    let nx = -dy / length * width * 0.5;
    let ny = dx / length * width * 0.5;
    let points = [
        [a[0] + nx - bounds.x, a[1] + ny - bounds.y],
        [b[0] + nx - bounds.x, b[1] + ny - bounds.y],
        [a[0] - nx - bounds.x, a[1] - ny - bounds.y],
        [a[0] - nx - bounds.x, a[1] - ny - bounds.y],
        [b[0] + nx - bounds.x, b[1] + ny - bounds.y],
        [b[0] - nx - bounds.x, b[1] - ny - bounds.y],
    ];
    kama_ui::ui!(ctx, {
        Rect(("monitor-transform-line", id), bounds) {
            fill: color;
            vertices: points.to_vec();
        }
    });
}

fn project_to_screen(preview: Rect, point: [f32; 2], size: [f32; 2]) -> [f32; 2] {
    [
        preview.x + point[0] / size[0].max(1.0) * preview.width,
        preview.y + point[1] / size[1].max(1.0) * preview.height,
    ]
}

fn screen_to_project(preview: Rect, point: [f32; 2], size: [f32; 2]) -> [f32; 2] {
    [
        (point[0] - preview.x) / preview.width.max(1.0) * size[0],
        (point[1] - preview.y) / preview.height.max(1.0) * size[1],
    ]
}

fn drag_source_point(
    preview: Rect,
    point: [f32; 2],
    render_size: [u32; 2],
    source_geometry: SourceGeometry,
    follows_clip: bool,
    timeline: &TimelineState,
) -> Option<[f32; 2]> {
    if follows_clip {
        let mut clip = timeline.selected_clip()?.clone();
        clip.pipeline = timeline.clip_property_pipeline(&clip).clone();
        Some(screen_to_selected_clip_source_point(
            preview,
            point,
            &clip,
            render_size[0],
            render_size[1],
            source_geometry,
            timeline.playhead(),
        ))
    } else {
        Some(screen_to_project(
            preview,
            point,
            [render_size[0].max(1) as f32, render_size[1].max(1) as f32],
        ))
    }
}

fn rotate(value: [f32; 2], degrees: f32) -> [f32; 2] {
    let radians = degrees.to_radians();
    let (sin, cos) = radians.sin_cos();
    [
        value[0] * cos - value[1] * sin,
        value[0] * sin + value[1] * cos,
    ]
}

fn safe_scale(value: f32) -> f32 {
    if value.abs() < 0.000001 {
        if value.is_sign_negative() {
            -0.000001
        } else {
            0.000001
        }
    } else {
        value
    }
}

fn distance_sq(a: [f32; 2], b: [f32; 2]) -> f32 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    dx * dx + dy * dy
}

fn point_in_quad(point: [f32; 2], corners: [[f32; 2]; 4]) -> bool {
    let mut sign = 0.0f32;
    for index in 0..4 {
        let a = corners[index];
        let b = corners[(index + 1) % 4];
        let cross = (b[0] - a[0]) * (point[1] - a[1]) - (b[1] - a[1]) * (point[0] - a[0]);
        if cross.abs() <= 0.001 {
            continue;
        }
        if sign == 0.0 {
            sign = cross.signum();
        } else if cross.signum() != sign {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod snap_tests {
    use super::*;
    use std::collections::BTreeMap;

    use crate::{
        effects::{EffectNode, ImageBinding, NodeExecution, SocketRef},
        playback::{
            generator_render_cache_key, local_node_evaluation_order, quantize_composition_time,
            GraphGeneratorVariants, GRAPH_GENERATOR_VARIANT_CAPACITY,
        },
        project::HostBinding,
    };

    fn local_test_node(id: u64, image_inputs: BTreeMap<String, ImageBinding>) -> EffectNode {
        EffectNode {
            id,
            node_type: format!("test.{id}"),
            execution: NodeExecution::SpatialGpu,
            ui_position: None,
            image_inputs,
            stack_input: Some("image".into()),
            inputs: BTreeMap::new(),
            host_inputs: BTreeMap::new(),
            dynamic_image_inputs: None,
        }
    }

    #[test]
    fn local_graph_evaluation_follows_dependencies_and_skips_disconnected_nodes() {
        let mut instance = PipelineInstance::effect_default();
        instance.local_nodes = vec![
            local_test_node(
                1,
                BTreeMap::from([(
                    "image".into(),
                    ImageBinding::Node(SocketRef {
                        node: 2,
                        output: "image".into(),
                    }),
                )]),
            ),
            local_test_node(
                2,
                BTreeMap::from([("image".into(), ImageBinding::PipelineInput)]),
            ),
            local_test_node(
                3,
                BTreeMap::from([("image".into(), ImageBinding::PipelineInput)]),
            ),
        ];
        instance.local_output = ImageBinding::Node(SocketRef {
            node: 1,
            output: "image".into(),
        });

        assert_eq!(local_node_evaluation_order(&instance), vec![1, 0]);
    }

    #[test]
    fn nested_composition_time_is_quantized_to_child_frame_rate() {
        let frame = 1.0 / 12.0;
        assert_eq!(quantize_composition_time(0.0, 12.0), 0.0);
        assert!((quantize_composition_time(0.082, 12.0) - 0.0).abs() < 1.0e-6);
        assert!((quantize_composition_time(0.084, 12.0) - frame).abs() < 1.0e-5);
        assert!((quantize_composition_time(0.124, 12.0) - frame).abs() < 1.0e-5);
    }

    #[test]
    fn nested_composition_time_never_uses_parent_subframes() {
        let child_fps = 24.0;
        let parent_times = [10.0 / 60.0, 11.0 / 60.0, 13.0 / 60.0];
        let sampled = parent_times.map(|time| quantize_composition_time(time, child_fps));
        assert_eq!(sampled[0], sampled[1]);
        assert!(sampled[2] > sampled[1]);
    }

    #[test]
    fn static_generator_cache_key_ignores_playhead_time() {
        let parameters = std::collections::BTreeMap::from([(
            "size".to_string(),
            HostBinding::Constant(crate::project::HostValue::Gpu(GpuValue::F32(42.0))),
        )]);
        let at_start = generator_render_cache_key(
            "test.generator",
            &parameters,
            0.0,
            0.0,
            false,
            1.0,
            [1920, 1080],
        );
        let later = generator_render_cache_key(
            "test.generator",
            &parameters,
            120.0,
            120.0,
            false,
            1.0,
            [1920, 1080],
        );
        assert_eq!(at_start, later);
    }

    #[test]
    fn graph_generator_cache_keeps_clip_override_variants() {
        let mut variants = GraphGeneratorVariants::default();
        variants.insert(11, "clip-a");
        variants.insert(22, "clip-b");

        assert_eq!(variants.get(11), Some("clip-a"));
        assert_eq!(variants.get(22), Some("clip-b"));
        assert_eq!(variants.get(11), Some("clip-a"));
        assert_eq!(variants.get(22), Some("clip-b"));
    }

    #[test]
    fn graph_generator_cache_bounds_animated_variants() {
        let mut variants = GraphGeneratorVariants::default();
        for key in 0..(GRAPH_GENERATOR_VARIANT_CAPACITY as u64 + 2) {
            variants.insert(key, key);
        }

        assert_eq!(variants.variants.len(), GRAPH_GENERATOR_VARIANT_CAPACITY);
        assert_eq!(variants.get(0), None);
        assert_eq!(
            variants.get(GRAPH_GENERATOR_VARIANT_CAPACITY as u64 + 1),
            Some(GRAPH_GENERATOR_VARIANT_CAPACITY as u64 + 1)
        );
    }

    #[test]
    fn time_dependent_generator_cache_key_tracks_time() {
        let parameters = std::collections::BTreeMap::new();
        let at_start = generator_render_cache_key(
            "test.generator",
            &parameters,
            0.0,
            0.0,
            true,
            1.0,
            [1920, 1080],
        );
        let later = generator_render_cache_key(
            "test.generator",
            &parameters,
            1.0,
            1.0,
            true,
            1.0,
            [1920, 1080],
        );
        assert_ne!(at_start, later);
    }

    #[test]
    fn pen_insert_index_is_after_selection_or_at_end() {
        assert_eq!(pen_insert_index(Some(0), 3), 1);
        assert_eq!(pen_insert_index(Some(1), 3), 2);
        assert_eq!(pen_insert_index(Some(2), 3), 3);
        assert_eq!(pen_insert_index(None, 3), 3);
        assert_eq!(pen_insert_index(Some(99), 3), 3);
    }

    #[test]
    fn gradient_click_between_unselected_stops_inserts_on_that_segment() {
        let handles = [
            PenPointHandle {
                index: 0,
                point: [10.0, 20.0],
            },
            PenPointHandle {
                index: 1,
                point: [110.0, 20.0],
            },
            PenPointHandle {
                index: 2,
                point: [210.0, 20.0],
            },
        ];
        let (index, projected) = gradient_pen_insert_target(None, &handles, [62.0, 25.0]);
        assert_eq!(index, 1);
        assert_eq!(projected, Some([62.0, 20.0]));
    }

    #[test]
    fn gradient_click_away_from_segments_appends_without_moving_existing_stops() {
        let handles = [
            PenPointHandle {
                index: 0,
                point: [10.0, 20.0],
            },
            PenPointHandle {
                index: 1,
                point: [110.0, 20.0],
            },
        ];
        assert_eq!(
            gradient_pen_insert_target(None, &handles, [60.0, 80.0]),
            (2, None)
        );
    }

    #[test]
    fn inserted_gradient_color_follows_logical_index() {
        let colors = [[1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]];
        assert_eq!(inserted_color(&colors, 1), [0.5, 0.0, 0.5, 1.0]);
        assert_eq!(inserted_color(&colors, 2), colors[1]);
    }

    #[test]
    fn snap_lock_stays_on_target_until_release_tolerance() {
        let mut lock = None;
        assert_eq!(
            snap_axis([48.0, 60.0, 72.0], &[50.0, 51.0], 4.0, &mut lock),
            2.0
        );
        assert_eq!(lock.map(|lock| lock.target), Some(50.0));
        assert_eq!(
            snap_axis([49.0, 61.0, 73.0], &[50.0, 51.0], 4.0, &mut lock),
            1.0
        );
        assert_eq!(lock.map(|lock| lock.target), Some(50.0));
        assert_eq!(
            snap_axis([60.0, 72.0, 84.0], &[50.0, 51.0], 4.0, &mut lock),
            0.0
        );
    }
}
