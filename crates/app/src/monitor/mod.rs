use std::{
    cell::RefCell,
    collections::{hash_map::DefaultHasher, BTreeMap, HashMap, HashSet},
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
        Arc, Condvar, Mutex,
    },
    thread,
};

use crate::{
    assets::{AppIcon, Icons},
    clip_graph_cache,
    effects::{
        resolved_node_input_cached, EffectRuntime, GpuValue, ImageBinding, ImageGraphIndex,
        PipelineInstance, ValueEvaluator,
    },
    embedded_vfs,
    gradient::{
        colors_from_values, colors_to_values, insert_midpoint, inserted_color,
        normalized_midpoints, remove_midpoint,
    },
    messages,
    panels::GraphMonitorSelection,
    plugin::{
        EffectRole, GeneratorBackend, GeneratorDefinition, InputType, MonitorHandleMode,
        PluginRegistry,
    },
    project::{
        CompositionId, CompositionSettings, GeneratorSource, HostBinding, MediaKind, Project,
        ProjectBackground, VisualSource,
    },
    runtime::media::{ExportVideoDecoder, VideoDecoder, SCRUB_PREVIEW_FPS},
    runtime::video::{
        CompositeArgs, CpuFrame, EffectEvalContext, EffectInputs, EffectRenderArgs, ExportPassArgs,
        GeneratorRenderArgs, GpuFrame, PresentationArgs, PresentationTexture, VideoFrame,
        VideoGpuRuntime, VideoUploadSurface,
    },
    runtime::wasm::{plugin_parameter_hash, WasmRenderRequest, WasmRuntime},
    theme,
    timeline::{Clip, TimelineState, TrackKind},
};
use anyhow::{Context, Error, Result};
use kama_ui::{
    components::{ComboBox, ComboBoxOpenDirection, ToggleButton},
    Color, ExternalTextureId, IconId, Rect, Renderer, Size,
};
use winit::keyboard::ModifiersState;

mod decode_pool;
mod export_readback;

use decode_pool::VideoDecoderPool;
use export_readback::ExportReadbacks;
pub(crate) use export_readback::{ExportPixelFormat, ExportRgba16Args, ExportYuvBatchArgs};

const VIDEO_CLIP_PRELOAD_SECONDS: f32 = 3.0;
const VIDEO_DECODER_POOL_CAPACITY: usize = 8;

#[derive(Clone, Debug)]
pub(crate) struct RenderCachePreview {
    pub path: PathBuf,
    pub local_time: f64,
    pub generation: u64,
    pub frame: u64,
}

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

const GRAPH_GENERATOR_VARIANT_CAPACITY: usize = 4;

#[derive(Debug)]
struct GraphGeneratorVariants<T> {
    variants: crate::app_shared::BoundedCache<u64, T>,
}

impl<T> Default for GraphGeneratorVariants<T> {
    fn default() -> Self {
        Self {
            variants: Default::default(),
        }
    }
}

impl<T: Clone> GraphGeneratorVariants<T> {
    fn get(&mut self, key: u64) -> Option<T> {
        self.variants.get(&key).cloned()
    }

    fn latest(&self) -> Option<T> {
        self.variants.latest().cloned()
    }

    fn insert(&mut self, key: u64, value: T) {
        self.variants.insert(key, value);
        self.variants
            .trim(GRAPH_GENERATOR_VARIANT_CAPACITY, usize::MAX, |_| 0);
    }
}

#[derive(Clone, Copy, Debug)]
struct SourceGeometry {
    size: (u32, u32),
    position_offset: [f32; 2],
}

impl SourceGeometry {
    fn canvas(width: u32, height: u32) -> Self {
        Self {
            size: (width, height),
            position_offset: [0.0, 0.0],
        }
    }
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
    pub icons: Icons,
}

pub(crate) struct MonitorPointerContext<'a> {
    pub modifiers: ModifiersState,
    pub project: &'a mut Project,
    pub plugins: &'a PluginRegistry,
    pub graph_selection: Option<GraphMonitorSelection>,
    pub timeline: &'a mut TimelineState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum GeneratorWorkerSlot {
    Clip(u64),
    Graph { pipeline: u64, node: u64 },
}

enum GeneratorWorkerKind {
    Plugin {
        definition: Box<GeneratorDefinition>,
        parameters: BTreeMap<String, HostBinding>,
        parameter_time: f64,
        local_time: f64,
        scale: f32,
        render_origin: [f32; 2],
        tight_bounds: bool,
    },
    Wasm {
        module: PathBuf,
        entry: String,
        parameters: BTreeMap<String, HostBinding>,
        parameter_time: f64,
        local_time: f64,
        scale: f32,
    },
}

struct WasmGeneratorRender<'a> {
    module: &'a Path,
    entry: &'a str,
    parameters: &'a BTreeMap<String, HostBinding>,
    size: [u32; 2],
    render_origin: [f32; 2],
    tight_bounds: bool,
    times: [f64; 2],
    memory_cache_key: u64,
    error_context: &'a str,
}

struct GeneratorWorkerJob {
    slot: GeneratorWorkerSlot,
    key: u64,
    epoch: u64,
    width: u32,
    height: u32,
    kind: GeneratorWorkerKind,
}

struct GeneratorWorkerResult {
    slot: GeneratorWorkerSlot,
    key: u64,
    epoch: u64,
    result: Result<GpuFrame>,
}

struct GeneratorWorkerShared {
    jobs: Mutex<HashMap<GeneratorWorkerSlot, GeneratorWorkerJob>>,
    wake: Condvar,
    closed: AtomicBool,
    epoch: AtomicU64,
}

struct GeneratorWorker {
    shared: Arc<GeneratorWorkerShared>,
    responses: Receiver<GeneratorWorkerResult>,
    pending: HashMap<GeneratorWorkerSlot, u64>,
}

impl GeneratorWorker {
    fn new(device: Arc<wgpu::Device>, queue: Arc<wgpu::Queue>) -> Option<Self> {
        let shared = Arc::new(GeneratorWorkerShared {
            jobs: Mutex::new(HashMap::new()),
            wake: Condvar::new(),
            closed: AtomicBool::new(false),
            epoch: AtomicU64::new(0),
        });
        let worker_shared = Arc::clone(&shared);
        let (response_tx, responses) = mpsc::channel();
        if thread::Builder::new()
            .name("kama-generator-worker".into())
            .spawn(move || generator_worker_loop(device, queue, worker_shared, response_tx))
            .is_err()
        {
            return None;
        }
        Some(Self {
            shared,
            responses,
            pending: HashMap::new(),
        })
    }

    fn request(&mut self, mut job: GeneratorWorkerJob) {
        if self.pending.get(&job.slot) == Some(&job.key) {
            return;
        }
        self.pending.insert(job.slot, job.key);
        job.epoch = self.shared.epoch.load(Ordering::Acquire);
        let mut jobs = self
            .shared
            .jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        jobs.insert(job.slot, job);
        drop(jobs);
        self.shared.wake.notify_one();
    }

    fn drain(&mut self) -> Vec<GeneratorWorkerResult> {
        let mut completed = Vec::new();
        loop {
            match self.responses.try_recv() {
                Ok(result) => {
                    if result.epoch != self.shared.epoch.load(Ordering::Acquire) {
                        continue;
                    }
                    if self.pending.get(&result.slot) == Some(&result.key) {
                        self.pending.remove(&result.slot);
                    }
                    completed.push(result);
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }
        completed
    }

    fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    fn clear(&mut self) {
        self.shared.epoch.fetch_add(1, Ordering::AcqRel);
        self.pending.clear();
        self.shared
            .jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        while self.responses.try_recv().is_ok() {}
    }
}

impl Drop for GeneratorWorker {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::Release);
        self.shared.wake.notify_all();
    }
}

fn generator_worker_loop(
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    shared: Arc<GeneratorWorkerShared>,
    responses: Sender<GeneratorWorkerResult>,
) {
    let mut gpu = VideoGpuRuntime::new(device.as_ref());
    let mut wasm = WasmRuntime::new().ok();
    loop {
        let job = {
            let mut jobs = shared
                .jobs
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            while jobs.is_empty() && !shared.closed.load(Ordering::Acquire) {
                jobs = shared
                    .wake
                    .wait(jobs)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
            if shared.closed.load(Ordering::Acquire) {
                return;
            }
            let Some(slot) = jobs.keys().next().copied() else {
                continue;
            };
            jobs.remove(&slot).expect("generator worker job exists")
        };

        let result = (|| -> Result<GpuFrame> {
            match &job.kind {
                GeneratorWorkerKind::Plugin {
                    definition,
                    parameters,
                    parameter_time,
                    local_time,
                    scale,
                    render_origin,
                    tight_bounds,
                } => match definition.backend {
                    GeneratorBackend::Gpu => {
                        let mut encoder =
                            device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                                label: Some("kama background generator encoder"),
                            });
                        gpu.begin_submission();
                        let frame = gpu.render_generator(GeneratorRenderArgs {
                            device: device.as_ref(),
                            queue: queue.as_ref(),
                            encoder: &mut encoder,
                            generator: definition,
                            parameters,
                            time: *parameter_time,
                            size: [job.width, job.height],
                            render_scale: *scale,
                        })?;
                        queue.submit(Some(encoder.finish()));
                        Ok(frame)
                    }
                    GeneratorBackend::Wasm => {
                        let (module, entry) = definition
                            .wasm_export()
                            .context("WASM generator module missing")?;
                        let runtime = wasm.as_mut().context("WASM runtime unavailable")?;
                        let cpu = cached_wasm_frame(
                            runtime,
                            job.key,
                            WasmRenderRequest {
                                module_path: module,
                                entry,
                                parameters,
                                size: [job.width, job.height],
                                render_scale: *scale,
                                render_origin: *render_origin,
                                tight_bounds: *tight_bounds,
                                parameter_time: *parameter_time,
                                local_time: *local_time,
                            },
                        )?;
                        Ok(gpu.upload(device.as_ref(), queue.as_ref(), cpu.as_ref()))
                    }
                },
                GeneratorWorkerKind::Wasm {
                    module,
                    entry,
                    parameters,
                    parameter_time,
                    local_time,
                    scale,
                } => {
                    let runtime = wasm.as_mut().context("WASM runtime unavailable")?;
                    let cpu = cached_wasm_frame(
                        runtime,
                        job.key,
                        WasmRenderRequest {
                            module_path: module,
                            entry,
                            parameters,
                            size: [job.width, job.height],
                            render_scale: *scale,
                            render_origin: [0.0, 0.0],
                            tight_bounds: false,
                            parameter_time: *parameter_time,
                            local_time: *local_time,
                        },
                    )?;
                    Ok(gpu.upload(device.as_ref(), queue.as_ref(), cpu.as_ref()))
                }
            }
        })();
        if responses
            .send(GeneratorWorkerResult {
                slot: job.slot,
                key: job.key,
                epoch: job.epoch,
                result,
            })
            .is_err()
        {
            return;
        }
    }
}

fn cached_wasm_frame(
    runtime: &mut WasmRuntime,
    key: u64,
    request: WasmRenderRequest<'_>,
) -> Result<Arc<CpuFrame>> {
    let disk_key = persistent_wasm_graph_frame_key(key, request.module_path, request.entry);
    if let Some(frame) = clip_graph_cache::load_frame(disk_key) {
        return Ok(Arc::new(frame));
    }
    let frame = Arc::new(runtime.render(request)?);
    clip_graph_cache::store_frame_async(disk_key, Arc::clone(&frame));
    Ok(frame)
}

#[derive(Clone, Copy)]
struct SourceRenderTiming {
    timeline_fps: f64,
    local_time: f64,
    keyframe_time: f64,
    source_step_seconds: f64,
}

struct RenderContext<'a> {
    gpu: &'a mut VideoGpuRuntime,
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    encoder: &'a mut wgpu::CommandEncoder,
    project: &'a Project,
    effects: &'a EffectRuntime,
    plugins: &'a PluginRegistry,

    render_scale: f32,
    scrubbing: bool,
    blocking_decode: bool,
}

impl RenderContext<'_> {
    fn transparent(&mut self, width: u32, height: u32) -> GpuFrame {
        self.gpu
            .transparent(self.device, self.encoder, width, height)
    }

    fn solid(&mut self, width: u32, height: u32, color: [f32; 4]) -> GpuFrame {
        self.gpu
            .solid(self.device, self.queue, self.encoder, width, height, color)
    }

    fn upload(&mut self, frame: &CpuFrame) -> GpuFrame {
        self.gpu.upload(self.device, self.queue, frame)
    }

    fn video_surface(&self, frame: &VideoFrame) -> VideoUploadSurface {
        self.gpu.video_upload_surface(self.device, frame)
    }

    fn upload_video_into(&mut self, surface: &VideoUploadSurface, frame: &VideoFrame) -> bool {
        self.gpu
            .upload_video_into(self.queue, self.encoder, surface, frame)
    }

    fn render_generator(
        &mut self,
        generator: &GeneratorDefinition,
        parameters: &BTreeMap<String, HostBinding>,
        time: f64,
        width: u32,
        height: u32,
    ) -> Result<GpuFrame> {
        self.gpu.render_generator(GeneratorRenderArgs {
            device: self.device,
            queue: self.queue,
            encoder: self.encoder,
            generator,
            parameters,
            time,
            size: [width, height],
            render_scale: self.render_scale,
        })
    }

    fn recycle(&mut self, frame: GpuFrame) {
        self.gpu.recycle_frame(frame);
    }

    fn composite(
        &mut self,
        destination: GpuFrame,
        source: GpuFrame,
        opacity: f32,
        mode: crate::project::BlendMode,
        alpha_mode: crate::project::AlphaBlendMode,
    ) -> GpuFrame {
        self.gpu.composite(CompositeArgs {
            device: self.device,
            queue: self.queue,
            encoder: self.encoder,
            destination,
            source,
            opacity,
            mode,
            alpha_mode,
        })
    }

    fn apply_source(
        &mut self,
        width: u32,
        height: u32,
        node: &crate::effects::EffectNode,
        effect: &EffectInputs<'_, '_>,
    ) -> GpuFrame {
        self.gpu.apply_source_node(
            EffectRenderArgs {
                device: self.device,
                queue: self.queue,
                encoder: self.encoder,
                effect,
            },
            width,
            height,
            node,
        )
    }

    fn apply_local(
        &mut self,
        input: GpuFrame,
        node: &crate::effects::EffectNode,
        effect: &EffectInputs<'_, '_>,
    ) -> GpuFrame {
        self.gpu.apply_local_node(
            EffectRenderArgs {
                device: self.device,
                queue: self.queue,
                encoder: self.encoder,
                effect,
            },
            input,
            node,
        )
    }

    fn apply_local_sized(
        &mut self,
        input: GpuFrame,
        node: &crate::effects::EffectNode,
        effect: &EffectInputs<'_, '_>,
        size: [u32; 2],
    ) -> GpuFrame {
        self.gpu.apply_local_node_sized(
            EffectRenderArgs {
                device: self.device,
                queue: self.queue,
                encoder: self.encoder,
                effect,
            },
            input,
            node,
            size,
        )
    }

    fn apply_binary(
        &mut self,
        first: GpuFrame,
        second: GpuFrame,
        node: &crate::effects::EffectNode,
        effect: &EffectInputs<'_, '_>,
    ) -> GpuFrame {
        self.gpu.apply_binary_node(
            EffectRenderArgs {
                device: self.device,
                queue: self.queue,
                encoder: self.encoder,
                effect,
            },
            first,
            second,
            node,
        )
    }

    fn apply_stage(
        &mut self,
        input: GpuFrame,
        stage: &crate::effects::CompiledStage,
        nodes: &[&crate::effects::EffectNode],
        effect: &EffectInputs<'_, '_>,
    ) -> GpuFrame {
        self.gpu.apply_compiled_stage(
            EffectRenderArgs {
                device: self.device,
                queue: self.queue,
                encoder: self.encoder,
                effect,
            },
            input,
            stage,
            nodes,
        )
    }
}

struct TimelineRender<'a> {
    tracks: &'a [crate::timeline::Track],
    clips: &'a [Clip],
    settings: &'a CompositionSettings,
    scope: u64,
    output_size: [u32; 2],
    time: f32,
    depth: usize,
    record_source_geometry: bool,
}

default_state! {
    pub struct MonitorState {
        texture: Option<ExternalTextureId>,
        presentation: Option<PresentationTexture>,
        gpu: Option<VideoGpuRuntime>,
        image_cache: HashMap<PathBuf, Arc<CpuFrame>>,
        image_gpu_cache: HashMap<PathBuf, GpuFrame>,
        model_gpu: Option<crate::model3d::ModelGpuRuntime>,
        video_decoders: VideoDecoderPool,
        export_video_decoders: HashMap<u64, (PathBuf, ExportVideoDecoder, u64)>,
        export_decode_epoch: u64,
        video_gpu_cache: HashMap<u64, (PathBuf, Arc<VideoFrame>, VideoUploadSurface)>,
        render_cache_decoder: Option<(PathBuf, u64, VideoDecoder)>,
        render_cache_gpu: Option<(PathBuf, u64, Arc<VideoFrame>, VideoUploadSurface)>,
        export_readbacks: ExportReadbacks,
        generator_gpu_cache: HashMap<u64, (u64, GpuFrame)>,
        graph_generator_gpu_cache: HashMap<(u64, u64), GraphGeneratorVariants<GpuFrame>>,
        generator_worker: Option<GeneratorWorker>,
        source_geometry: HashMap<u32, SourceGeometry>,
        wasm: Option<WasmRuntime> = WasmRuntime::new()
            .map_err(|error| {
                messages::error("WASM generator", format!("runtime unavailable: {error:#}"));
                error
            })
            .ok(),
        monitor_wasm: RefCell<Option<WasmRuntime>> = RefCell::new(WasmRuntime::new().ok()),
        last_signature: Option<u64>,
        waiting_for_video: bool,
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
    fn new_with_device(
        device: &wgpu::Device,
        effects: &EffectRuntime,
        plugins: &PluginRegistry,
    ) -> Self {
        let mut state = Self::default();
        let mut gpu = VideoGpuRuntime::new(device);
        gpu.prewarm(device, effects, plugins);
        state.gpu = Some(gpu);
        if let Some(runtime) = &mut state.wasm {
            let mut seen_modules = HashSet::new();
            for generator in plugins.generators() {
                let Some((module, _)) = generator.wasm_export() else {
                    continue;
                };
                if !seen_modules.insert(module.to_path_buf()) {
                    continue;
                }
                if let Err(error) = runtime.precompile(module) {
                    messages::error(
                        "WASM generator",
                        format!(
                            "module precompile failed for {}: {error:#}",
                            module.display()
                        ),
                    );
                }
            }
        }
        state
    }

    pub fn new(renderer: &Renderer, effects: &EffectRuntime, plugins: &PluginRegistry) -> Self {
        let mut state = Self::new_with_device(renderer.device(), effects, plugins);
        state.generator_worker =
            GeneratorWorker::new(renderer.device_handle(), renderer.queue_handle());
        state
    }

    pub(crate) fn new_export_worker(
        device: &wgpu::Device,
        effects: &EffectRuntime,
        plugins: &PluginRegistry,
    ) -> Self {
        Self::new_with_device(device, effects, plugins)
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
        direction: i32,
    ) -> bool {
        let preview = self.preview_rect(rect, project);
        if !preview.contains(point) {
            return false;
        }
        let (width, height) = self.preview_dimensions(project);
        let candidates = monitor_clips_at(
            preview,
            point,
            timeline,
            width,
            height,
            &self.source_geometry,
        );
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
        self.clear_frame_caches();
        self.last_signature = None;
    }

    fn clear_frame_caches(&mut self) {
        self.presentation = None;
        self.image_cache.clear();
        self.image_gpu_cache.clear();
        self.model_gpu = None;
        self.video_gpu_cache.clear();
        self.render_cache_decoder = None;
        self.render_cache_gpu = None;
        self.generator_gpu_cache.clear();
        self.graph_generator_gpu_cache.clear();
        if let Some(worker) = &mut self.generator_worker {
            worker.clear();
        }
        self.source_geometry.clear();
    }

    fn cached_generator_frame(&self, clip_id: u64, key: u64) -> Option<GpuFrame> {
        self.generator_gpu_cache
            .get(&clip_id)
            .and_then(|(cached_key, frame)| (*cached_key == key).then(|| frame.clone()))
    }

    fn cache_generator_frame(&mut self, clip_id: u64, key: u64, frame: GpuFrame) -> GpuFrame {
        self.generator_gpu_cache
            .insert(clip_id, (key, frame.clone()));
        frame
    }

    fn last_generator_frame(&self, clip_id: u64) -> Option<GpuFrame> {
        self.generator_gpu_cache
            .get(&clip_id)
            .map(|(_, frame)| frame.clone())
    }

    fn poll_generator_worker(&mut self) -> bool {
        let Some(worker) = &mut self.generator_worker else {
            return false;
        };
        let completed = worker.drain();
        let changed = !completed.is_empty();
        for completed in completed {
            match completed.result {
                Ok(frame) => match completed.slot {
                    GeneratorWorkerSlot::Clip(clip) => {
                        self.generator_gpu_cache
                            .insert(clip, (completed.key, frame));
                    }
                    GeneratorWorkerSlot::Graph { pipeline, node } => {
                        self.graph_generator_gpu_cache
                            .entry((pipeline, node))
                            .or_default()
                            .insert(completed.key, frame);
                    }
                },
                Err(error) => {
                    messages::error("Generator", format!("{error:#}"));
                }
            }
        }
        changed
    }

    fn render_wasm_generator(
        &mut self,
        project: &Project,
        request: WasmGeneratorRender<'_>,
    ) -> Arc<CpuFrame> {
        let WasmGeneratorRender {
            module,
            entry,
            parameters,
            size: [width, height],
            render_origin,
            tight_bounds,
            times: [parameter_time, local_time],
            memory_cache_key,
            error_context,
        } = request;
        let scale = self.preview_scale(project);
        let Some(runtime) = &mut self.wasm else {
            return Arc::new(placeholder_frame("WASM runtime unavailable", width, height));
        };
        cached_wasm_frame(
            runtime,
            memory_cache_key,
            WasmRenderRequest {
                module_path: module,
                entry,
                parameters,
                size: [width, height],
                render_scale: scale,
                render_origin,
                tight_bounds,
                parameter_time,
                local_time,
            },
        )
        .unwrap_or_else(|error| {
            messages::error(
                "WASM generator",
                format!("{error_context} failed: {error:#}"),
            );
            Arc::new(placeholder_frame("WASM generator error", width, height))
        })
    }

    pub fn sync_compiled_effects(
        &mut self,
        renderer: &Renderer,
        effects: &EffectRuntime,
        plugins: &PluginRegistry,
    ) {
        if self.gpu.is_none() {
            self.gpu = Some(VideoGpuRuntime::new(renderer.device()));
        }
        if let Some(gpu) = &mut self.gpu {
            gpu.prewarm(renderer.device(), effects, plugins);
        }

        self.graph_generator_gpu_cache.clear();
        self.last_signature = None;
    }

    pub fn precompile_wasm(&mut self, path: &std::path::Path) -> Result<()> {
        if let Some(runtime) = &mut self.wasm {
            runtime.precompile(path)?;
        }
        Ok(())
    }

    pub fn refresh(
        &mut self,
        renderer: &mut Renderer,
        project: &Project,
        timeline: &TimelineState,
        effects: &EffectRuntime,
        plugins: &PluginRegistry,
        render_cache: Option<&RenderCachePreview>,
    ) -> Result<()> {
        let top_level_clip_ids = timeline
            .clips()
            .iter()
            .map(|clip| clip.id)
            .collect::<HashSet<_>>();
        let live_cache_clip_ids = live_cache_clip_ids(project, timeline);

        self.video_decoders.begin_frame();
        self.video_gpu_cache
            .retain(|clip, _| live_cache_clip_ids.contains(clip));
        self.generator_gpu_cache
            .retain(|clip, _| live_cache_clip_ids.contains(clip));

        self.source_geometry
            .retain(|clip, _| top_level_clip_ids.contains(clip));

        let mut decoded_frame_ready = false;
        for decoder in self.video_decoders.iter_mut() {
            decoded_frame_ready |= decoder.poll_completed();
        }
        if let Some((_, _, decoder)) = &mut self.render_cache_decoder {
            decoded_frame_ready |= decoder.poll_completed();
        }
        decoded_frame_ready |= self.poll_generator_worker();
        if decoded_frame_ready {
            self.last_signature = None;
        }
        let (preview_width, preview_height) = self.preview_dimensions(project);
        if timeline.is_playing() && !timeline.is_scrubbing() {
            self.preload_upcoming_videos(project, timeline, preview_width, preview_height);
        }
        if self.presentation.as_ref().is_some_and(|presentation| {
            presentation.width != preview_width || presentation.height != preview_height
        }) {
            self.clear_frame_caches();
        }
        if let Some(gpu) = &mut self.gpu {
            gpu.retain_working_size(preview_width, preview_height);
        }
        let mut signature = render_signature(project, timeline, preview_width, preview_height);
        if self.show_captured_frame {
            let mut hasher = DefaultHasher::new();
            signature.hash(&mut hasher);
            0x0043_4150_5455_5245_u64.hash(&mut hasher);
            self.captured_frame
                .as_ref()
                .map(|(_, pixels)| pixels.len())
                .hash(&mut hasher);
            signature = hasher.finish();
        }
        if let Some(cache) = render_cache {
            let mut hasher = DefaultHasher::new();
            signature.hash(&mut hasher);
            cache.path.hash(&mut hasher);
            cache.generation.hash(&mut hasher);
            cache.frame.hash(&mut hasher);
            signature = hasher.finish();
        }
        if self.last_signature == Some(signature) && self.texture.is_some() {
            return Ok(());
        }
        self.waiting_for_video = false;

        let cached_render_frame = if let Some(cache) = render_cache {
            match self.request_render_cache_frame(
                cache,
                project.active_settings().frame_rate.max(1.0),
                preview_width,
                preview_height,
            )? {
                Some(frame) => Some(frame),
                None => {
                    self.waiting_for_video = true;
                    self.last_signature = None;
                    return Ok(());
                }
            }
        } else {
            self.render_cache_decoder = None;
            self.render_cache_gpu = None;
            None
        };

        if self.gpu.is_none() {
            self.gpu = Some(VideoGpuRuntime::new(renderer.device()));
        }
        if self.presentation.is_none() {
            let presentation =
                PresentationTexture::new(renderer.device(), preview_width, preview_height);
            if let Some(texture) = self.texture {
                renderer.replace_external_texture(texture, presentation.external_view())?;
            } else {
                let texture = renderer.register_external_texture(presentation.external_view())?;
                self.texture = Some(texture);
            }
            self.presentation = Some(presentation);
        }

        let mut gpu = self.gpu.take().expect("monitor GPU initialized");
        let result = (|| -> Result<()> {
            let device = renderer.device();
            let queue = renderer.queue();
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("kama monitor render graph"),
            });
            gpu.begin_submission();
            let frame = if self.show_captured_frame {
                if let Some(([capture_width, capture_height], pixels)) =
                    self.captured_frame.as_ref()
                {
                    let captured = VideoFrame::from_rgba16(
                        preview_width,
                        preview_height,
                        (*capture_width).max(1),
                        (*capture_height).max(1),
                        pixels.clone(),
                        1,
                        false,
                    );
                    let captured = Arc::new(captured);
                    let surface = gpu.video_upload_surface(device, captured.as_ref());
                    let _ = gpu.upload_video_into(queue, &mut encoder, &surface, captured.as_ref());
                    surface.frame()
                } else {
                    gpu.transparent(device, &mut encoder, preview_width, preview_height)
                }
            } else if let (Some(cache), Some(cached)) = (render_cache, cached_render_frame.as_ref())
            {
                self.upload_render_cache_frame(&gpu, device, queue, &mut encoder, cache, cached)
            } else {
                let mut render = RenderContext {
                    gpu: &mut gpu,
                    device,
                    queue,
                    encoder: &mut encoder,
                    project,
                    effects,
                    plugins,
                    render_scale: self.preview_scale(project),
                    scrubbing: timeline.is_scrubbing(),
                    blocking_decode: false,
                };
                self.render_project(
                    &mut render,
                    timeline,
                    [preview_width, preview_height],
                    timeline.playhead(),
                )?
            };
            let presentation = self
                .presentation
                .as_ref()
                .expect("monitor presentation initialized");
            gpu.present(PresentationArgs {
                device,
                encoder: &mut encoder,
                input: &frame,
                output: presentation,
            });
            gpu.recycle_frame(frame);
            queue.submit(Some(encoder.finish()));
            Ok(())
        })();
        self.gpu = Some(gpu);
        result?;

        if let Some(texture) = self.texture {
            renderer.invalidate_external_texture(texture)?;
        }
        self.last_signature = Some(signature);
        Ok(())
    }

    pub(crate) fn render_export_rgba16_on(
        &mut self,
        args: ExportRgba16Args<'_>,
    ) -> Result<Vec<u8>> {
        self.with_export_rgba16_mapped_on(args, |mapped, row_bytes, padded_row_bytes, height| {
            let output_len = row_bytes * height;
            let mut output = vec![0u8; output_len];
            if row_bytes == padded_row_bytes {
                output.copy_from_slice(&mapped[..output_len]);
            } else {
                for y in 0..height {
                    let source = &mapped[y * padded_row_bytes..y * padded_row_bytes + row_bytes];
                    let target = &mut output[y * row_bytes..(y + 1) * row_bytes];
                    target.copy_from_slice(source);
                }
            }
            output
        })
    }

    fn with_export_rgba16_mapped_on<R>(
        &mut self,
        args: ExportRgba16Args<'_>,
        consume: impl FnOnce(&[u8], usize, usize, usize) -> R,
    ) -> Result<R> {
        let ExportRgba16Args {
            device,
            queue,
            project,
            timeline,
            runtime,
            timeline_time,
        } = args;
        let (effects, plugins) = runtime;
        let width = project.active_settings().canvas_size[0].max(1);
        let height = project.active_settings().canvas_size[1].max(1);
        if self.gpu.is_none() {
            self.gpu = Some(VideoGpuRuntime::new(device));
        }
        let mut gpu = self.gpu.take().expect("export GPU initialized");
        let result = (|| -> Result<R> {
            self.export_readbacks.ensure_rgba(device, width, height);
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("kama export frame"),
            });
            gpu.begin_submission();
            let mut render = RenderContext {
                gpu: &mut gpu,
                device,
                queue,
                encoder: &mut encoder,
                project,
                effects,
                plugins,
                render_scale: 1.0,
                scrubbing: false,
                blocking_decode: true,
            };
            self.begin_export_decode_frame();
            let frame =
                self.render_project(&mut render, timeline, [width, height], timeline_time)?;
            self.finish_export_decode_frame();
            let surface = self.export_readbacks.rgba();

            gpu.export_rgba16_into(
                ExportPassArgs {
                    device,
                    encoder: &mut encoder,
                    input: &frame,
                },
                &surface.view,
            );
            encoder.copy_texture_to_buffer(
                wgpu::ImageCopyTexture {
                    texture: &surface.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::ImageCopyBuffer {
                    buffer: &surface.buffer,
                    layout: wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some(surface.padded_row_bytes as u32),
                        rows_per_image: Some(height),
                    },
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
            queue.submit(Some(encoder.finish()));

            let slice = surface.buffer.slice(..);
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            slice.map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });

            let map_result = loop {
                match rx.try_recv() {
                    Ok(result) => break result,
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        device.poll(wgpu::Maintain::Poll);
                        std::thread::sleep(std::time::Duration::from_micros(250));
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        anyhow::bail!("export readback callback dropped");
                    }
                }
            };
            map_result.map_err(|error| anyhow::anyhow!("map export frame: {error}"))?;
            let mapped = slice.get_mapped_range();
            let consumed = consume(
                &mapped,
                surface.row_bytes as usize,
                surface.padded_row_bytes as usize,
                height as usize,
            );
            drop(mapped);
            surface.buffer.unmap();
            gpu.recycle_frame(frame);
            Ok(consumed)
        })();
        self.gpu = Some(gpu);
        result
    }

    pub(crate) fn render_export_yuv_batch_to_writer_on<W: std::io::Write>(
        &mut self,
        args: ExportYuvBatchArgs<'_, W>,
    ) -> Result<(Option<Error>, usize)> {
        let ExportYuvBatchArgs {
            device,
            queue,
            project,
            timeline,
            runtime,
            timeline_times,
            first_frame,
            live_end_frame,
            format,
            writer,
        } = args;
        if timeline_times.is_empty() {
            return Ok((None, 0));
        }
        let (effects, plugins) = runtime;
        let width = project.active_settings().canvas_size[0].max(1);
        let height = project.active_settings().canvas_size[1].max(1);
        self.export_readbacks.ensure_encode_batch(
            device,
            timeline_times.len(),
            width,
            height,
            format,
        );
        if self.gpu.is_none() {
            self.gpu = Some(VideoGpuRuntime::new(device));
        }
        let mut gpu = self.gpu.take().expect("export GPU initialized");
        let result = (|| -> Result<(Option<Error>, usize)> {
            let mut receivers = Vec::with_capacity(timeline_times.len());
            for (index, &timeline_time) in timeline_times.iter().enumerate() {
                if first_frame.saturating_add(index as u64) > live_end_frame.load(Ordering::Acquire)
                {
                    break;
                }
                let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("kama pipelined export frame"),
                });
                gpu.begin_submission();
                self.begin_export_decode_frame();
                let frame = {
                    let mut render = RenderContext {
                        gpu: &mut gpu,
                        device,
                        queue,
                        encoder: &mut encoder,
                        project,
                        effects,
                        plugins,
                        render_scale: 1.0,
                        scrubbing: false,
                        blocking_decode: true,
                    };
                    self.render_project(&mut render, timeline, [width, height], timeline_time)?
                };
                self.finish_export_decode_frame();
                let surface = self.export_readbacks.encode(index);
                surface.encode_gpu_conversion(&gpu, device, &mut encoder, &frame);
                surface.copy_to_buffer(&mut encoder);
                let submission = queue.submit(Some(encoder.finish()));
                let slice = surface.buffer.slice(..);
                let (tx, rx) = std::sync::mpsc::sync_channel(1);
                slice.map_async(wgpu::MapMode::Read, move |result| {
                    let _ = tx.send(result);
                });
                receivers.push((rx, submission));
                gpu.recycle_frame(frame);
            }

            let rendered_count = receivers.len();
            let mut write_error = None;
            let mut map_error = None;
            for (index, (rx, submission)) in receivers.into_iter().enumerate() {
                let mapped_ok = match rx.try_recv() {
                    Ok(Ok(())) => true,
                    Ok(Err(error)) => {
                        map_error.get_or_insert_with(|| error.to_string());
                        false
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        map_error
                            .get_or_insert_with(|| "export readback callback dropped".to_string());
                        false
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        let _ = device.poll(wgpu::Maintain::WaitForSubmissionIndex(submission));
                        match rx.recv() {
                            Ok(Ok(())) => true,
                            Ok(Err(error)) => {
                                map_error.get_or_insert_with(|| error.to_string());
                                false
                            }
                            Err(_) => {
                                map_error.get_or_insert_with(|| {
                                    "export readback callback dropped".to_string()
                                });
                                false
                            }
                        }
                    }
                };
                let surface = self.export_readbacks.encode(index);
                if mapped_ok {
                    let slice = surface.buffer.slice(..);
                    let mapped = slice.get_mapped_range();
                    if write_error.is_none() {
                        if let Err(error) = surface.write_mapped(&mapped, writer) {
                            write_error = Some(error);
                        }
                    }
                    drop(mapped);
                    surface.buffer.unmap();
                }
            }
            if let Some(error) = map_error {
                anyhow::bail!("map encoder-native export frame: {error}");
            }
            Ok((write_error, rendered_count))
        })();
        self.gpu = Some(gpu);
        result
    }

    fn request_render_cache_frame(
        &mut self,
        cache: &RenderCachePreview,
        fps: f64,
        width: u32,
        height: u32,
    ) -> Result<Option<Arc<VideoFrame>>> {
        let replace = self
            .render_cache_decoder
            .as_ref()
            .is_none_or(|(path, generation, _)| {
                path != &cache.path || *generation != cache.generation
            });
        if replace {
            self.render_cache_decoder = Some((
                cache.path.clone(),
                cache.generation,
                VideoDecoder::new(cache.path.clone()),
            ));
            self.render_cache_gpu = None;
        }
        let (_, _, decoder) = self
            .render_cache_decoder
            .as_mut()
            .context("render cache decoder disappeared")?;
        let (frame, pending) = decoder.frame(
            cache.local_time,
            fps,
            1.0 / fps.max(1.0),
            width,
            height,
            false,
        );
        if let Some(frame) = frame {
            return Ok(Some(frame));
        }
        if pending {
            if let Some((path, generation, frame, _)) = &self.render_cache_gpu {
                if path == &cache.path && *generation == cache.generation {
                    return Ok(Some(Arc::clone(frame)));
                }
            }
            return Ok(None);
        }
        Ok(None)
    }

    fn upload_render_cache_frame(
        &mut self,
        gpu: &VideoGpuRuntime,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        cache: &RenderCachePreview,
        frame: &Arc<VideoFrame>,
    ) -> GpuFrame {
        if let Some((path, generation, cached_frame, surface)) = &mut self.render_cache_gpu {
            if path == &cache.path
                && *generation == cache.generation
                && surface.matches(frame.as_ref())
            {
                if Arc::ptr_eq(cached_frame, frame) {
                    return surface.frame();
                }
                if gpu.upload_video_into(queue, encoder, surface, frame.as_ref()) {
                    *cached_frame = Arc::clone(frame);
                    return surface.frame();
                }
            }
        }

        let surface = gpu.video_upload_surface(device, frame.as_ref());
        let _ = gpu.upload_video_into(queue, encoder, &surface, frame.as_ref());
        let uploaded = surface.frame();
        self.render_cache_gpu = Some((
            cache.path.clone(),
            cache.generation,
            Arc::clone(frame),
            surface,
        ));
        uploaded
    }

    fn preload_upcoming_videos(
        &mut self,
        project: &Project,
        timeline: &TimelineState,
        width: u32,
        height: u32,
    ) {
        let playhead = timeline.playhead();
        let preload_end = playhead + VIDEO_CLIP_PRELOAD_SECONDS;
        let mut upcoming = timeline
            .clips()
            .iter()
            .filter(|clip| {
                clip.start >= playhead
                    && clip.start <= preload_end
                    && timeline
                        .tracks()
                        .iter()
                        .find(|track| track.id == clip.track)
                        .is_some_and(|track| track.kind != TrackKind::Audio && !track.muted)
            })
            .collect::<Vec<_>>();
        upcoming.sort_unstable_by(|left, right| left.start.total_cmp(&right.start));

        let mut warmed_tracks = HashSet::new();
        for clip in upcoming {
            if warmed_tracks.contains(&clip.track) {
                continue;
            }
            let VisualSource::Media(media) = &clip.source else {
                continue;
            };
            let Some(asset) = project
                .media(*media)
                .filter(|asset| asset.kind == MediaKind::Video)
            else {
                continue;
            };
            warmed_tracks.insert(clip.track);
            self.video_decoders
                .get(u64::from(clip.id), &asset.path, false)
                .preload(
                    clip.looped_source_time(clip.start, project),
                    asset
                        .frame_rate
                        .unwrap_or(project.active_settings().frame_rate)
                        .max(1.0),
                    clip.speed.max(0.01) as f64 / project.active_settings().frame_rate.max(1.0),
                    width,
                    height,
                );
        }
    }

    pub fn invalidate(&mut self) {
        self.last_signature = None;
    }

    pub(crate) fn master_muted(&self) -> bool {
        self.master_muted
    }

    pub(crate) fn take_action(&mut self) -> Option<MonitorAction> {
        self.pending_action.take()
    }

    pub(crate) fn set_captured_frame(&mut self, size: [u32; 2], pixels: Vec<u8>) {
        self.captured_frame = Some((size, pixels));
        self.last_signature = None;
    }

    pub fn is_waiting_for_video(&self) -> bool {
        self.waiting_for_video
    }

    fn begin_export_decode_frame(&mut self) {
        self.export_decode_epoch = self.export_decode_epoch.wrapping_add(1).max(1);
    }

    fn finish_export_decode_frame(&mut self) {
        let epoch = self.export_decode_epoch;

        self.export_video_decoders
            .retain(|_, (_, _, last_used)| *last_used == epoch);
    }

    pub fn clear_media_caches(&mut self) {
        self.clear_frame_caches();
        self.video_decoders.clear();
        self.export_video_decoders.clear();
        self.last_signature = None;
    }

    pub fn clear_caches(&mut self) {
        self.clear_media_caches();
        self.captured_frame = None;
        self.show_captured_frame = false;
        if let Some(wasm) = &mut self.wasm {
            wasm.clear();
        }
    }

    pub fn build(&self, ctx: &mut kama_ui::BuildCtx, rect: Rect, view: MonitorBuildContext<'_>) {
        let MonitorBuildContext {
            project,
            timeline,
            plugins,
            graph_selection,
            icons,
        } = view;
        let chevron = icons.get(AppIcon::Chevron);
        let local_rect = Rect::new(0.0, 0.0, rect.width, rect.height);
        let preview = self.preview_rect(local_rect, project);
        let preview_width = preview.width;
        let preview_height = preview.height;
        let (render_width, render_height) = self.preview_dimensions(project);
        let texture = self.texture;

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
                "Viewport Snap",
            ),
            (
                "monitor-clip-snap",
                monitor_snap_button_rect(local_rect, true),
                AppIcon::MonitorClipSnap,
                self.clip_snap,
                true,
                "Clip Snap",
            ),
            (
                "monitor-pen-tool",
                monitor_pen_button_rect(local_rect),
                AppIcon::Pen,
                self.pen_tool,
                true,
                "Pen tool",
            ),
            (
                "monitor-master-mute",
                monitor_mute_button_rect(local_rect),
                AppIcon::MasterMute,
                self.master_muted,
                true,
                "Master Mute",
            ),
            (
                "monitor-capture-frame",
                monitor_capture_button_rect(local_rect, 0),
                AppIcon::CaptureFrame,
                false,
                true,
                "Capture frame to Media",
            ),
            (
                "monitor-capture-temp",
                monitor_capture_button_rect(local_rect, 1),
                AppIcon::CaptureTemp,
                self.captured_frame.is_some(),
                true,
                "Temporary frame capture",
            ),
            (
                "monitor-show-capture",
                monitor_capture_button_rect(local_rect, 2),
                AppIcon::ShowCapture,
                self.show_captured_frame,
                self.captured_frame.is_some(),
                "Show captured frame",
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
                    &self.source_geometry,
                ) {
                    draw_transform_gizmo(ctx, geometry);
                }
            }
            let edit = MonitorEditView {
                preview,
                plugins,
                graph_selection,
                render_size: [render_width, render_height],
                source_geometry: &self.source_geometry,
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
            source_geometry: &self.source_geometry,
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
                self.last_signature = None;
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
            source_geometry: &self.source_geometry,
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
                &self.source_geometry,
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
                    &self.source_geometry,
                ) {
                    if let Some(handle) = gizmo_handle_at(point, geometry) {
                        return self.begin_gizmo_drag(
                            handle,
                            preview,
                            point,
                            [preview_width, preview_height],
                            timeline,
                            plugins,
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
            &self.source_geometry,
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
                        &self.source_geometry,
                    ) {
                        if point_in_quad(point, geometry.corners) {
                            return self.begin_gizmo_drag(
                                TransformGizmoHandle::Move,
                                preview,
                                point,
                                [preview_width, preview_height],
                                timeline,
                                plugins,
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

    fn begin_gizmo_drag(
        &mut self,
        handle: TransformGizmoHandle,
        preview: Rect,
        point: [f32; 2],
        preview_size: [u32; 2],
        timeline: &TimelineState,
        plugins: &PluginRegistry,
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
            &self.source_geometry,
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
            &self.source_geometry,
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
                        &self.source_geometry,
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
                &self.source_geometry,
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

    fn render_project(
        &mut self,
        render: &mut RenderContext<'_>,
        timeline: &TimelineState,
        output_size: [u32; 2],
        time: f32,
    ) -> Result<GpuFrame> {
        let project = render.project;
        self.render_timeline_layers(
            render,
            TimelineRender {
                tracks: timeline.tracks(),
                clips: timeline.clips(),
                settings: project.active_settings(),
                scope: project.active_composition,
                output_size,
                time,
                depth: 0,
                record_source_geometry: true,
            },
        )
    }

    fn render_composition_document(
        &mut self,
        render: &mut RenderContext<'_>,
        composition_id: CompositionId,
        cache_scope: u64,
        output_size: [u32; 2],
        time: f32,
        depth: usize,
    ) -> Result<GpuFrame> {
        if depth >= 16 {
            return Ok(render.transparent(output_size[0], output_size[1]));
        }
        let Some(composition) = render.project.composition(composition_id) else {
            return Ok(render.transparent(output_size[0], output_size[1]));
        };
        self.render_timeline_layers(
            render,
            TimelineRender {
                tracks: &composition.timeline.tracks,
                clips: &composition.timeline.clips,
                settings: &composition.settings,
                scope: cache_scope,
                output_size,
                time: quantize_composition_time(time, composition.settings.frame_rate),
                depth,
                record_source_geometry: false,
            },
        )
    }

    fn render_timeline_layers(
        &mut self,
        render: &mut RenderContext<'_>,
        timeline: TimelineRender<'_>,
    ) -> Result<GpuFrame> {
        let TimelineRender {
            tracks,
            clips,
            settings,
            scope,
            output_size: [preview_width, preview_height],
            time: timeline_time,
            depth,
            record_source_geometry,
        } = timeline;
        let mut accumulated = match settings.background {
            ProjectBackground::Transparent => render.transparent(preview_width, preview_height),
            ProjectBackground::Solid { color } => {
                render.solid(preview_width, preview_height, color)
            }
        };
        let frame_index =
            (timeline_time.max(0.0) as f64 * settings.frame_rate.max(1.0)).floor() as u64;
        let has_video_solo = tracks
            .iter()
            .any(|track| track.kind != TrackKind::Audio && track.solo);

        let mut active_clips_by_track: HashMap<u32, Vec<&Clip>> = HashMap::new();
        for clip in clips {
            if timeline_time >= clip.start && timeline_time < clip.end() {
                active_clips_by_track
                    .entry(clip.track)
                    .or_default()
                    .push(clip);
            }
        }
        for track in tracks.iter().rev() {
            if track.kind == TrackKind::Audio || track.muted || (has_video_solo && !track.solo) {
                continue;
            }
            let active_clips = active_clips_by_track
                .get(&track.id)
                .map(Vec::as_slice)
                .unwrap_or(&[]);

            if track.kind == TrackKind::Effect {
                for &clip in active_clips {
                    if !clip.source.is_effect_input() {
                        continue;
                    }
                    let row = track.property_row(&clip.source, clip.source_instance);
                    let composite = row.map(|row| &row.composite).unwrap_or(&clip.composite);
                    let pipeline = row.map(|row| &row.pipeline).unwrap_or(&clip.pipeline);
                    let clip_opacity =
                        composite.opacity(timeline_time as f64) * clip.opacity.clamp(0.0, 1.0);
                    if clip_opacity <= 0.0 && render.blocking_decode {
                        continue;
                    }
                    let original = accumulated.clone();
                    let local_time = clip.local_time(timeline_time);
                    let processed = self.evaluate_pipeline(
                        render,
                        accumulated,
                        pipeline,
                        EffectEvalContext {
                            timeline_time: timeline_time as f64,
                            local_time,
                            frame_index,
                            frame_rate: settings.frame_rate,
                        },
                        None,
                        [0.0, 0.0],
                    );
                    accumulated = render.composite(
                        original,
                        processed,
                        clip_opacity,
                        composite.blend_mode(timeline_time as f64),
                        composite.alpha_blend_mode(timeline_time as f64),
                    );
                }
                continue;
            }

            let track_opacity = track.composite.opacity(timeline_time as f64);
            if track_opacity <= 0.0 && render.blocking_decode {
                continue;
            }
            let track_mode = track.composite.blend_mode(timeline_time as f64);
            let track_alpha_mode = track.composite.alpha_blend_mode(timeline_time as f64);
            if active_clips.is_empty() && track.pipeline.is_none() {
                continue;
            }

            let mut track_frame: Option<GpuFrame> = None;
            for &clip in active_clips {
                let row = track.property_row(&clip.source, clip.source_instance);
                let source_state = row.map(|row| &row.source).unwrap_or(&clip.source);
                let composite = row.map(|row| &row.composite).unwrap_or(&clip.composite);
                let pipeline = row.map(|row| &row.pipeline).unwrap_or(&clip.pipeline);
                let model3d = row.map(|row| &row.model3d).unwrap_or(&clip.model3d);
                let clip_opacity =
                    composite.opacity(timeline_time as f64) * clip.opacity.clamp(0.0, 1.0);
                if clip_opacity <= 0.0 && render.blocking_decode {
                    continue;
                }
                let clip_mode = composite.blend_mode(timeline_time as f64);
                let clip_alpha_mode = composite.alpha_blend_mode(timeline_time as f64);
                let local_time = clip.local_time(timeline_time);
                if clip.source.is_effect_input() {
                    let base = track_frame
                        .take()
                        .unwrap_or_else(|| render.transparent(preview_width, preview_height));
                    let original = base.clone();
                    let processed = self.evaluate_pipeline(
                        render,
                        base,
                        pipeline,
                        EffectEvalContext {
                            timeline_time: timeline_time as f64,
                            local_time,
                            frame_index,
                            frame_rate: settings.frame_rate,
                        },
                        None,
                        [0.0, 0.0],
                    );
                    track_frame = Some(render.composite(
                        original,
                        processed,
                        clip_opacity,
                        clip_mode,
                        clip_alpha_mode,
                    ));
                    continue;
                }
                if !clip.source.is_renderable_visual() {
                    continue;
                }
                let source_time = if matches!(
                    &clip.source,
                    VisualSource::Media(_) | VisualSource::Composition(_)
                ) {
                    clip.looped_source_time(timeline_time, render.project)
                } else {
                    local_time
                };
                let source_geometry = self.source_render_geometry(
                    render.project,
                    source_state,
                    timeline_time as f64,
                    render.plugins,
                    settings.canvas_size,
                    [preview_width, preview_height],
                );
                let source_dimensions = source_geometry.size;
                if record_source_geometry {
                    self.source_geometry.insert(clip.id, source_geometry);
                }
                let cache_clip_id = if record_source_geometry {
                    u64::from(clip.id)
                } else {
                    scoped_clip_id(scope, clip.id)
                };
                let source = self.render_source(
                    render,
                    cache_clip_id,
                    source_state,
                    model3d,
                    SourceRenderTiming {
                        timeline_fps: settings.frame_rate,
                        local_time: source_time,
                        keyframe_time: timeline_time as f64,
                        source_step_seconds: clip.speed.max(0.01) as f64
                            / settings.frame_rate.max(1.0),
                    },
                    [source_dimensions.0, source_dimensions.1],
                    depth + 1,
                )?;
                let processed = self.evaluate_pipeline(
                    render,
                    source,
                    pipeline,
                    EffectEvalContext {
                        timeline_time: timeline_time as f64,
                        local_time,
                        frame_index,
                        frame_rate: settings.frame_rate,
                    },
                    Some([preview_width, preview_height]),
                    source_geometry.position_offset,
                );

                if track_frame.is_none()
                    && processed.width == preview_width
                    && processed.height == preview_height
                    && clip_opacity >= 1.0
                    && matches!(clip_mode, crate::project::BlendMode::Normal)
                    && matches!(clip_alpha_mode, crate::project::AlphaBlendMode::SourceOver)
                {
                    track_frame = Some(processed);
                } else {
                    let base = track_frame
                        .take()
                        .unwrap_or_else(|| render.transparent(preview_width, preview_height));
                    track_frame = Some(render.composite(
                        base,
                        processed,
                        clip_opacity,
                        clip_mode,
                        clip_alpha_mode,
                    ));
                }
            }

            let mut track_frame = match track_frame {
                Some(frame) => frame,
                None if track.pipeline.is_none() => continue,
                None => render.transparent(preview_width, preview_height),
            };
            if let Some(pipeline) = &track.pipeline {
                track_frame = self.evaluate_pipeline(
                    render,
                    track_frame,
                    pipeline,
                    EffectEvalContext {
                        timeline_time: timeline_time as f64,
                        local_time: timeline_time as f64,
                        frame_index,
                        frame_rate: settings.frame_rate,
                    },
                    None,
                    [0.0, 0.0],
                );
            }
            accumulated = render.composite(
                accumulated,
                track_frame,
                track_opacity,
                track_mode,
                track_alpha_mode,
            );
        }
        Ok(accumulated)
    }

    fn source_render_geometry(
        &mut self,
        project: &Project,
        source: &VisualSource,
        keyframe_time: f64,
        plugins: &PluginRegistry,
        canvas_size: [u32; 2],
        preview_size: [u32; 2],
    ) -> SourceGeometry {
        let [preview_width, preview_height] = preview_size;
        if let Some(geometry) = tight_generator_source_geometry(
            source,
            keyframe_time,
            plugins,
            canvas_size,
            preview_width,
            preview_height,
        ) {
            return geometry;
        }

        let dimensions = match source {
            VisualSource::Media(id) => project.media(*id).and_then(|asset| match asset.kind {
                MediaKind::Image { width, height } => Some((width, height)),
                MediaKind::Video => asset.video_width.zip(asset.video_height),
                _ => None,
            }),
            VisualSource::Composition(id) => project.composition(*id).map(|composition| {
                (
                    composition.settings.canvas_size[0],
                    composition.settings.canvas_size[1],
                )
            }),
            VisualSource::Generator(GeneratorSource::Plugin {
                generator_type,
                parameters,
            }) if generator_type == "builtin.text" => {
                let text = generator_string(parameters, "text", keyframe_time).unwrap_or_default();
                let family = generator_string(parameters, "font_family", keyframe_time)
                    .filter(|family| !family.is_empty());
                let font_size =
                    generator_f32(parameters, "font_size", keyframe_time).unwrap_or(72.0);
                self.wasm.as_mut().map(|runtime| {
                    let [width, height] =
                        runtime.measure_text(&text, family.as_deref(), font_size, 1.0);
                    (width, height)
                })
            }
            VisualSource::Generator(GeneratorSource::Plugin {
                generator_type,
                parameters,
            }) if generator_type == "builtin.shape" => {
                if generator_u32(parameters, "shape_type", keyframe_time).unwrap_or(0) == 1 {
                    generator_vec2(parameters, "radius", keyframe_time).map(|value| {
                        (
                            (value[0] * 2.0).max(1.0) as u32,
                            (value[1] * 2.0).max(1.0) as u32,
                        )
                    })
                } else {
                    generator_vec2(parameters, "size", keyframe_time)
                        .map(|value| (value[0].max(1.0) as u32, value[1].max(1.0) as u32))
                }
            }
            _ => None,
        };
        dimensions
            .filter(|(width, height)| *width > 0 && *height > 0)
            .map(|dimensions| {
                scaled_source_geometry(
                    dimensions,
                    [0.0, 0.0],
                    canvas_size,
                    preview_width,
                    preview_height,
                )
            })
            .unwrap_or_else(|| SourceGeometry::canvas(preview_width, preview_height))
    }

    #[allow(clippy::too_many_arguments)]
    fn render_source(
        &mut self,
        render: &mut RenderContext<'_>,
        clip_id: u64,
        source: &VisualSource,
        model3d: &crate::timeline::Model3dClipTransform,
        timing: SourceRenderTiming,
        output_size: [u32; 2],
        depth: usize,
    ) -> Result<GpuFrame> {
        let SourceRenderTiming {
            timeline_fps,
            local_time,
            keyframe_time,
            source_step_seconds,
        } = timing;
        let [preview_width, preview_height] = output_size;
        let cpu = match source {
            VisualSource::Media(id) => {
                let asset = render.project.media(*id).context("missing media asset")?;
                match asset.kind {
                    MediaKind::Video => {
                        let fps = asset.frame_rate.unwrap_or(timeline_fps).max(1.0);
                        let (frame, pending) = if render.blocking_decode {
                            let export_decode_epoch = self.export_decode_epoch;
                            if !self.export_video_decoders.contains_key(&clip_id) {
                                let reusable = self.export_video_decoders.iter().find_map(
                                    |(&other_clip, (path, _, last_used))| {
                                        (other_clip != clip_id
                                            && path.as_path() == asset.path.as_path()
                                            && *last_used != export_decode_epoch
                                            && last_used.saturating_add(1) == export_decode_epoch)
                                            .then_some(other_clip)
                                    },
                                );
                                if let Some(other_clip) = reusable {
                                    if let Some((path, decoder, _)) =
                                        self.export_video_decoders.remove(&other_clip)
                                    {
                                        self.export_video_decoders
                                            .insert(clip_id, (path, decoder, export_decode_epoch));
                                    }
                                }
                            }
                            let export_entry = self
                                .export_video_decoders
                                .entry(clip_id)
                                .or_insert_with(|| {
                                    (
                                        asset.path.clone(),
                                        ExportVideoDecoder::new(asset.path.clone()),
                                        export_decode_epoch,
                                    )
                                });
                            if export_entry.0.as_path() != asset.path.as_path() {
                                *export_entry = (
                                    asset.path.clone(),
                                    ExportVideoDecoder::new(asset.path.clone()),
                                    export_decode_epoch,
                                );
                            }
                            export_entry.2 = export_decode_epoch;
                            let decoded = export_entry.1.frame(
                                local_time,
                                fps,
                                preview_width,
                                preview_height,
                            );
                            (Some(decoded?), false)
                        } else {
                            self.video_decoders.get(clip_id, &asset.path, true).frame(
                                local_time,
                                fps,
                                source_step_seconds,
                                preview_width,
                                preview_height,
                                render.scrubbing,
                            )
                        };
                        if pending {
                            self.waiting_for_video = true;
                            if let Some((cached_path, _, surface)) =
                                self.video_gpu_cache.get(&clip_id)
                            {
                                if cached_path.as_path() == asset.path.as_path() {
                                    return Ok(surface.frame());
                                }
                            }
                        }
                        if let Some(frame) = frame {
                            let uploaded = if let Some((cached_path, cached_frame, surface)) =
                                self.video_gpu_cache.get_mut(&clip_id)
                            {
                                if cached_path.as_path() == asset.path.as_path()
                                    && surface.matches(frame.as_ref())
                                {
                                    if Arc::ptr_eq(cached_frame, &frame) {
                                        Some(surface.frame())
                                    } else if render.upload_video_into(surface, frame.as_ref()) {
                                        *cached_frame = Arc::clone(&frame);
                                        Some(surface.frame())
                                    } else {
                                        None
                                    }
                                } else {
                                    None
                                }
                            } else {
                                None
                            };
                            let uploaded = if let Some(uploaded) = uploaded {
                                uploaded
                            } else {
                                let surface = render.video_surface(frame.as_ref());
                                let _ = render.upload_video_into(&surface, frame.as_ref());
                                let uploaded = surface.frame();
                                self.video_gpu_cache.insert(
                                    clip_id,
                                    (asset.path.clone(), Arc::clone(&frame), surface),
                                );
                                uploaded
                            };
                            return Ok(uploaded);
                        }
                        placeholder_frame(
                            &format!("Video: {}", asset.name),
                            preview_width,
                            preview_height,
                        )
                    }
                    MediaKind::Image { .. } => {
                        if let Some(cached) = self.image_gpu_cache.get(&asset.path) {
                            return Ok(cached.clone());
                        }
                        let decoded = if let Some(cached) = self.image_cache.get(&asset.path) {
                            Arc::clone(cached)
                        } else {
                            let decoded = Arc::new(match image::open(&asset.path) {
                                Ok(image) => frame_from_image(image, preview_width, preview_height),
                                Err(_) => {
                                    placeholder_frame(&asset.name, preview_width, preview_height)
                                }
                            });
                            self.image_cache
                                .insert(asset.path.clone(), Arc::clone(&decoded));
                            decoded
                        };
                        let uploaded = render.upload(decoded.as_ref());
                        self.image_gpu_cache
                            .insert(asset.path.clone(), uploaded.clone());
                        return Ok(uploaded);
                    }
                    MediaKind::Model3d => {
                        let vector = |binding: &crate::effects::Binding, fallback: [f32; 3]| {
                            binding
                                .evaluate(keyframe_time)
                                .and_then(|value| match value {
                                    GpuValue::Vec3(value) => Some(value),
                                    _ => None,
                                })
                                .unwrap_or(fallback)
                        };
                        let size = vector(&model3d.size, [2.0, 2.0, 2.0]);
                        let position = vector(&model3d.position, [0.0, 0.0, 0.0]);
                        let rotation = vector(&model3d.rotation, [0.0, 0.0, 0.0]);
                        let scale = vector(&model3d.scale, [1.0, 1.0, 1.0]);
                        let runtime = self.model_gpu.get_or_insert_with(|| {
                            crate::model3d::ModelGpuRuntime::new(render.device)
                        });
                        return match runtime.render(
                            render.device,
                            render.queue,
                            render.encoder,
                            &asset.path,
                            preview_width,
                            preview_height,
                            size,
                            scale,
                            rotation,
                            position,
                            model3d.shading,
                        ) {
                            Ok(frame) => Ok(frame),
                            Err(_) => Ok(render.upload(&placeholder_frame(
                                &asset.name,
                                preview_width,
                                preview_height,
                            ))),
                        };
                    }
                    _ => placeholder_frame(&asset.name, preview_width, preview_height),
                }
            }
            VisualSource::Composition(composition) => {
                return self.render_composition_document(
                    render,
                    *composition,
                    nested_cache_scope(clip_id, *composition),
                    [preview_width, preview_height],
                    local_time.max(0.0) as f32,
                    depth,
                );
            }
            VisualSource::Generator(GeneratorSource::Plugin {
                generator_type,
                parameters,
            }) => {
                let Some(definition) = render.plugins.generator(generator_type) else {
                    return Ok(render.upload(&placeholder_frame(
                        generator_type,
                        preview_width,
                        preview_height,
                    )));
                };
                let cache_key = generator_render_cache_key(
                    &definition.key,
                    parameters,
                    keyframe_time,
                    local_time,
                    definition.uses_time,
                    self.preview_scale(render.project),
                    [preview_width, preview_height],
                );
                if let Some(cached) = self.cached_generator_frame(clip_id, cache_key) {
                    return Ok(cached);
                }

                if !render.blocking_decode {
                    let preview_scale = self.preview_scale(render.project);
                    let previous = self.last_generator_frame(clip_id);
                    if let Some(worker) = &mut self.generator_worker {
                        let render_origin =
                            generator_content_bounds(definition, parameters, keyframe_time)
                                .map(|(x, y, _, _)| [x, y])
                                .unwrap_or([0.0, 0.0]);
                        worker.request(GeneratorWorkerJob {
                            slot: GeneratorWorkerSlot::Clip(clip_id),
                            key: cache_key,
                            epoch: 0,
                            width: preview_width,
                            height: preview_height,
                            kind: GeneratorWorkerKind::Plugin {
                                definition: Box::new(definition.clone()),
                                parameters: parameters.clone(),
                                parameter_time: keyframe_time,
                                local_time,
                                scale: preview_scale,
                                render_origin,
                                tight_bounds: definition.bounds.is_some(),
                            },
                        });
                        self.waiting_for_video = true;
                        if let Some(previous) = previous {
                            return Ok(previous);
                        }
                        return Ok(render.transparent(preview_width, preview_height));
                    }
                }

                let generated = match definition.backend {
                    GeneratorBackend::Gpu => match render.render_generator(
                        definition,
                        parameters,
                        keyframe_time,
                        preview_width,
                        preview_height,
                    ) {
                        Ok(frame) => frame,
                        Err(error) => {
                            messages::error(
                                "GPU generator",
                                format!("{} failed: {error:#}", definition.key),
                            );
                            render.upload(&placeholder_frame(
                                &definition.name,
                                preview_width,
                                preview_height,
                            ))
                        }
                    },
                    GeneratorBackend::Wasm => {
                        let Some((module, entry)) = definition.wasm_export() else {
                            return Ok(render.upload(&placeholder_frame(
                                "WASM generator module missing",
                                preview_width,
                                preview_height,
                            )));
                        };
                        let render_origin =
                            generator_content_bounds(definition, parameters, keyframe_time)
                                .map(|(x, y, _, _)| [x, y])
                                .unwrap_or([0.0, 0.0]);
                        let cpu = self.render_wasm_generator(
                            render.project,
                            WasmGeneratorRender {
                                module,
                                entry,
                                parameters,
                                size: [preview_width, preview_height],
                                render_origin,
                                tight_bounds: definition.bounds.is_some(),
                                times: [keyframe_time, local_time],
                                memory_cache_key: cache_key,
                                error_context: &definition.key,
                            },
                        );
                        render.upload(cpu.as_ref())
                    }
                };

                return Ok(self.cache_generator_frame(clip_id, cache_key, generated));
            }
            VisualSource::Generator(GeneratorSource::Wasm {
                module,
                entry,
                parameters,
                ..
            }) => {
                let cache_id = format!("wasm:{}:{entry}", module.display());
                let cache_key = generator_render_cache_key(
                    &cache_id,
                    parameters,
                    keyframe_time,
                    local_time,
                    true,
                    self.preview_scale(render.project),
                    [preview_width, preview_height],
                );
                if let Some(cached) = self.cached_generator_frame(clip_id, cache_key) {
                    return Ok(cached);
                }
                if !render.blocking_decode {
                    let preview_scale = self.preview_scale(render.project);
                    let previous = self.last_generator_frame(clip_id);
                    if let Some(worker) = &mut self.generator_worker {
                        worker.request(GeneratorWorkerJob {
                            slot: GeneratorWorkerSlot::Clip(clip_id),
                            key: cache_key,
                            epoch: 0,
                            width: preview_width,
                            height: preview_height,
                            kind: GeneratorWorkerKind::Wasm {
                                module: module.clone(),
                                entry: entry.clone(),
                                parameters: parameters.clone(),
                                parameter_time: keyframe_time,
                                local_time,
                                scale: preview_scale,
                            },
                        });
                        self.waiting_for_video = true;
                        if let Some(previous) = previous {
                            return Ok(previous);
                        }
                        return Ok(render.transparent(preview_width, preview_height));
                    }
                }
                let cpu = self.render_wasm_generator(
                    render.project,
                    WasmGeneratorRender {
                        module,
                        entry,
                        parameters,
                        size: [preview_width, preview_height],
                        render_origin: [0.0, 0.0],
                        tight_bounds: false,
                        times: [keyframe_time, local_time],
                        memory_cache_key: cache_key,
                        error_context: &cache_id,
                    },
                );
                let generated = render.upload(cpu.as_ref());
                return Ok(self.cache_generator_frame(clip_id, cache_key, generated));
            }
            VisualSource::EffectInput | VisualSource::Audio(_) | VisualSource::AudioPlaceholder => {
                CpuFrame::transparent(preview_width, preview_height)
            }
        };
        Ok(render.upload(&cpu))
    }

    fn evaluate_shared_graph(
        &mut self,
        render: &mut RenderContext<'_>,
        input: GpuFrame,
        pipeline: &crate::effects::EffectPipeline,
        instance: &PipelineInstance,
        context: EffectEvalContext,
    ) -> GpuFrame {
        struct Eval<'a, 'render> {
            render: &'a mut RenderContext<'render>,
            wasm: &'a mut Option<WasmRuntime>,
            graph_generator_cache: &'a mut HashMap<(u64, u64), GraphGeneratorVariants<GpuFrame>>,
            generator_worker: &'a mut Option<GeneratorWorker>,
            wasm_scale: f32,
            context: EffectEvalContext,
            pipeline_visiting: HashSet<u64>,
        }

        #[derive(Clone, Copy)]
        struct ResolveView<'graph, 'values> {
            pipeline: &'graph crate::effects::EffectPipeline,
            instance: Option<&'graph PipelineInstance>,
            project: &'graph Project,
            plugins: &'graph PluginRegistry,
            values: &'values RefCell<ValueEvaluator<'graph>>,
            nodes: &'values HashMap<u64, &'graph crate::effects::EffectNode>,
        }

        fn resolve_binding(
            binding: &ImageBinding,
            input: &GpuFrame,
            view: ResolveView<'_, '_>,
            eval: &mut Eval<'_, '_>,
            cache: &mut HashMap<u64, GpuFrame>,
            visiting: &mut HashSet<u64>,
        ) -> GpuFrame {
            match binding {
                ImageBinding::Disconnected => eval.render.transparent(input.width, input.height),
                ImageBinding::PipelineInput => input.clone(),
                ImageBinding::Node(socket) => {
                    resolve_node(socket.node, input, view, eval, cache, visiting)
                }
            }
        }

        fn resolve_node(
            node_id: u64,
            input: &GpuFrame,
            view: ResolveView<'_, '_>,
            eval: &mut Eval<'_, '_>,
            cache: &mut HashMap<u64, GpuFrame>,
            visiting: &mut HashSet<u64>,
        ) -> GpuFrame {
            let ResolveView {
                pipeline,
                instance,
                project,
                plugins,
                nodes,
                ..
            } = view;
            if let Some(frame) = cache.get(&node_id) {
                return frame.clone();
            }
            if !visiting.insert(node_id) {
                return eval.render.transparent(input.width, input.height);
            }
            let Some(&node) = nodes.get(&node_id) else {
                visiting.remove(&node_id);
                return eval.render.transparent(input.width, input.height);
            };

            if node.node_type == crate::effects::PIPELINE_NODE_TYPE {
                let source_binding = node
                    .stack_input
                    .as_ref()
                    .and_then(|name| node.image_inputs.get(name))
                    .unwrap_or(&ImageBinding::PipelineInput);
                let source = resolve_binding(source_binding, input, view, eval, cache, visiting);
                let enabled = resolved_graph_node_value(node, instance, view.values, "enabled")
                    .and_then(GpuValue::bool)
                    .unwrap_or(true);
                let target = enabled
                    .then(|| {
                        resolved_graph_node_value(node, instance, view.values, "pipeline")
                            .and_then(GpuValue::enum_index)
                            .and_then(|index| {
                                project.pipeline_node_target_index(pipeline.id, index)
                            })
                    })
                    .flatten();
                let output = if let Some(target) = target {
                    if eval.pipeline_visiting.insert(target.id) {
                        let mut nested_cache = HashMap::new();
                        let mut nested_visiting = HashSet::new();
                        let nested_values = RefCell::new(ValueEvaluator::new(
                            &target.value_nodes,
                            eval.context.value_context(),
                        ));
                        let nested_nodes =
                            target.nodes.iter().map(|node| (node.id, node)).collect();
                        let nested_view = ResolveView {
                            pipeline: target,
                            instance: None,
                            project,
                            plugins,
                            values: &nested_values,
                            nodes: &nested_nodes,
                        };
                        let output = resolve_binding(
                            &target.output,
                            &source,
                            nested_view,
                            eval,
                            &mut nested_cache,
                            &mut nested_visiting,
                        );
                        for frame in nested_cache.into_values() {
                            if !frame.shares_surface(&output) {
                                eval.render.recycle(frame);
                            }
                        }
                        eval.pipeline_visiting.remove(&target.id);
                        output
                    } else {
                        source
                    }
                } else {
                    source
                };
                visiting.remove(&node_id);
                cache.insert(node_id, output.clone());
                return output;
            }

            if let Some(generator) = plugins.generator(&node.node_type) {
                let parameters = resolved_graph_generator_parameters(
                    node,
                    instance,
                    view.values,
                    eval.context.keyframe_time(),
                );
                let render_key = generator_render_cache_key(
                    &generator.key,
                    &parameters,
                    eval.context.keyframe_time(),
                    eval.context.local_time,
                    generator.uses_time,
                    eval.wasm_scale,
                    [input.width, input.height],
                );
                let slot = (pipeline.id, node.id);
                if let Some(frame) = eval
                    .graph_generator_cache
                    .get_mut(&slot)
                    .and_then(|variants| variants.get(render_key))
                {
                    visiting.remove(&node_id);
                    cache.insert(node_id, frame.clone());
                    return frame;
                }
                if let Some(worker) = eval.generator_worker.as_mut() {
                    let previous = eval
                        .graph_generator_cache
                        .get(&slot)
                        .and_then(|variants| variants.latest());
                    worker.request(GeneratorWorkerJob {
                        slot: GeneratorWorkerSlot::Graph {
                            pipeline: pipeline.id,
                            node: node.id,
                        },
                        key: render_key,
                        epoch: 0,
                        width: input.width,
                        height: input.height,
                        kind: GeneratorWorkerKind::Plugin {
                            definition: Box::new(generator.clone()),
                            parameters,
                            parameter_time: eval.context.keyframe_time(),
                            local_time: eval.context.local_time,
                            scale: eval.wasm_scale,
                            render_origin: [0.0, 0.0],
                            tight_bounds: false,
                        },
                    });
                    let output = previous
                        .unwrap_or_else(|| eval.render.transparent(input.width, input.height));
                    visiting.remove(&node_id);
                    cache.insert(node_id, output.clone());
                    return output;
                }
                let output = match generator.backend {
                    GeneratorBackend::Gpu => eval
                        .render
                        .gpu
                        .render_generator(GeneratorRenderArgs {
                            device: eval.render.device,
                            queue: eval.render.queue,
                            encoder: eval.render.encoder,
                            generator,
                            parameters: &parameters,
                            time: eval.context.keyframe_time(),
                            size: [input.width, input.height],
                            render_scale: eval.render.render_scale,
                        })
                        .unwrap_or_else(|error| {
                            messages::error(
                                "GPU generator",
                                format!("{} failed: {error:#}", generator.key),
                            );
                            eval.render.transparent(input.width, input.height)
                        }),
                    GeneratorBackend::Wasm => {
                        let cpu = generator
                            .wasm_export()
                            .and_then(|(module, entry)| {
                                eval.wasm.as_mut().map(|runtime| {
                                    runtime
                                        .render(WasmRenderRequest {
                                            module_path: module,
                                            entry,
                                            parameters: &parameters,
                                            size: [input.width, input.height],
                                            render_scale: eval.wasm_scale,
                                            render_origin: [0.0, 0.0],
                                            tight_bounds: false,
                                            parameter_time: eval.context.keyframe_time(),
                                            local_time: eval.context.local_time,
                                        })
                                        .unwrap_or_else(|error| {
                                            messages::error(
                                                "WASM generator",
                                                format!("{} failed: {error:#}", generator.key),
                                            );
                                            placeholder_frame(
                                                "WASM generator error",
                                                input.width,
                                                input.height,
                                            )
                                        })
                                })
                            })
                            .unwrap_or_else(|| {
                                placeholder_frame(
                                    "WASM generator unavailable",
                                    input.width,
                                    input.height,
                                )
                            });
                        eval.render.upload(&cpu)
                    }
                };
                eval.graph_generator_cache
                    .entry(slot)
                    .or_default()
                    .insert(render_key, output.clone());
                visiting.remove(&node_id);
                cache.insert(node_id, output.clone());
                return output;
            }

            let image_input_names = node.image_input_names();
            let mut frames = image_input_names
                .iter()
                .map(|name| {
                    resolve_binding(
                        node.image_inputs
                            .get(name)
                            .unwrap_or(&ImageBinding::Disconnected),
                        input,
                        view,
                        eval,
                        cache,
                        visiting,
                    )
                })
                .collect::<Vec<_>>();
            let effect = EffectInputs::new(
                instance,
                plugins,
                eval.context,
                view.values,
                eval.render.render_scale,
            );
            let output = match frames.len() {
                0 => eval
                    .render
                    .apply_source(input.width, input.height, node, &effect),
                1 if node.dynamic_image_inputs.is_some() => frames.remove(0),
                1 => eval.render.apply_local(frames.remove(0), node, &effect),
                2 if node.dynamic_image_inputs.is_none() => {
                    let first = frames.remove(0);
                    let second = frames.remove(0);
                    eval.render.apply_binary(first, second, node, &effect)
                }
                _ if node.dynamic_image_inputs.is_some() => {
                    let mut iter = frames.into_iter();
                    let mut output = iter
                        .next()
                        .unwrap_or_else(|| eval.render.transparent(input.width, input.height));
                    for next in iter {
                        output = eval.render.apply_binary(output, next, node, &effect);
                    }
                    output
                }
                _ => {
                    for frame in frames {
                        eval.render.recycle(frame);
                    }
                    eval.render.transparent(input.width, input.height)
                }
            };
            visiting.remove(&node_id);
            cache.insert(node_id, output.clone());
            output
        }

        let project = render.project;
        let plugins = render.plugins;
        let wasm_scale = self.preview_scale(project);
        let mut eval = Eval {
            render,
            wasm: &mut self.wasm,
            graph_generator_cache: &mut self.graph_generator_gpu_cache,
            generator_worker: &mut self.generator_worker,
            wasm_scale,
            context,
            pipeline_visiting: HashSet::from([pipeline.id]),
        };
        let mut cache = HashMap::new();
        let mut visiting = HashSet::new();
        let values = RefCell::new(ValueEvaluator::new(
            &pipeline.value_nodes,
            context.value_context(),
        ));
        let nodes = pipeline.nodes.iter().map(|node| (node.id, node)).collect();
        let view = ResolveView {
            pipeline,
            instance: Some(instance),
            project,
            plugins,
            values: &values,
            nodes: &nodes,
        };
        let output = resolve_binding(
            &pipeline.output,
            &input,
            view,
            &mut eval,
            &mut cache,
            &mut visiting,
        );
        let generator_pending = eval
            .generator_worker
            .as_ref()
            .is_some_and(GeneratorWorker::has_pending);
        for frame in cache.into_values() {
            if !frame.shares_surface(&output) {
                eval.render.recycle(frame);
            }
        }
        if !input.shares_surface(&output) {
            eval.render.recycle(input);
        }
        drop(eval);
        self.waiting_for_video |= generator_pending;
        output
    }

    fn evaluate_pipeline(
        &mut self,
        render: &mut RenderContext<'_>,
        mut frame: GpuFrame,
        instance: &PipelineInstance,
        context: EffectEvalContext,
        local_output_size: Option<[u32; 2]>,
        local_position_offset: [f32; 2],
    ) -> GpuFrame {
        if let Some(id) = instance.pipeline {
            if let Some(pipeline) = render.project.pipeline(id) {
                let values = RefCell::new(ValueEvaluator::new(
                    &pipeline.value_nodes,
                    context.value_context(),
                ));
                let effect = EffectInputs::new(
                    Some(instance),
                    render.plugins,
                    context,
                    &values,
                    render.render_scale,
                );
                if pipeline.nodes.iter().any(|node| {
                    node.image_inputs.len() != 1
                        || node.dynamic_image_inputs.is_some()
                        || node.node_type == crate::effects::PIPELINE_NODE_TYPE
                        || matches!(
                            node.execution,
                            crate::effects::NodeExecution::GeneratorGpu
                                | crate::effects::NodeExecution::GeneratorCpu
                        )
                }) {
                    frame = self.evaluate_shared_graph(render, frame, pipeline, instance, context);
                } else {
                    match &pipeline.output {
                        crate::effects::ImageBinding::Disconnected => {
                            frame = render.transparent(frame.width, frame.height);
                        }
                        crate::effects::ImageBinding::PipelineInput => {}
                        crate::effects::ImageBinding::Node(_) => {
                            if let Some(compiled) = render.effects.compiled(id) {
                                for stage in &compiled.stages {
                                    let nodes: Vec<_> = stage
                                        .node_ids
                                        .iter()
                                        .filter_map(|node_id| {
                                            pipeline.nodes.iter().find(|node| node.id == *node_id)
                                        })
                                        .collect();
                                    frame = render.apply_stage(frame, stage, &nodes, &effect);
                                }
                            } else {
                                for node in pipeline.main_path() {
                                    frame = render.apply_local(frame, node, &effect);
                                }
                            }
                        }
                    }
                }
            }
        }

        let local_input = frame;
        let local_values = RefCell::new(ValueEvaluator::new(&[], context.value_context()));
        let local_effect = EffectInputs::new(
            Some(instance),
            render.plugins,
            context,
            &local_values,
            render.render_scale,
        );
        let local_width = local_input.width;
        let local_height = local_input.height;
        let mut local_outputs = HashMap::<u64, GpuFrame>::new();
        for node_index in local_node_evaluation_order(instance) {
            let node = &instance.local_nodes[node_index];
            let input = match node
                .stack_image_input()
                .map(|(_, binding)| binding)
                .unwrap_or(&crate::effects::ImageBinding::Disconnected)
            {
                crate::effects::ImageBinding::Disconnected => {
                    render.transparent(local_width, local_height)
                }
                crate::effects::ImageBinding::PipelineInput => local_input.clone(),
                crate::effects::ImageBinding::Node(socket) => local_outputs
                    .get(&socket.node)
                    .cloned()
                    .unwrap_or_else(|| render.transparent(local_width, local_height)),
            };
            let output = if node.id == crate::effects::LOCAL_TRANSFORM_NODE_ID {
                let adjusted;
                let transform = if local_position_offset != [0.0, 0.0] {
                    adjusted = transform_with_position_offset(
                        node,
                        context.keyframe_time(),
                        local_position_offset,
                    );
                    &adjusted
                } else {
                    node
                };
                render.apply_local_sized(
                    input,
                    transform,
                    &local_effect,
                    local_output_size.unwrap_or([local_width, local_height]),
                )
            } else {
                render.apply_local(input, node, &local_effect)
            };
            local_outputs.insert(node.id, output);
        }
        let output = match &instance.local_output {
            crate::effects::ImageBinding::Disconnected => {
                let [width, height] = local_output_size.unwrap_or([local_width, local_height]);
                render.transparent(width, height)
            }
            crate::effects::ImageBinding::PipelineInput if local_output_size.is_some() => {
                let [width, height] = local_output_size.unwrap();
                if width == local_width && height == local_height {
                    local_input.clone()
                } else if let Some(transform) = instance.transform() {
                    let adjusted;
                    let transform = if local_position_offset != [0.0, 0.0] {
                        adjusted = transform_with_position_offset(
                            transform,
                            context.keyframe_time(),
                            local_position_offset,
                        );
                        &adjusted
                    } else {
                        transform
                    };
                    render.apply_local_sized(
                        local_input.clone(),
                        transform,
                        &local_effect,
                        [width, height],
                    )
                } else {
                    local_input.clone()
                }
            }
            crate::effects::ImageBinding::PipelineInput => local_input.clone(),
            crate::effects::ImageBinding::Node(socket) => {
                local_outputs.get(&socket.node).cloned().unwrap_or_else(|| {
                    let [width, height] = local_output_size.unwrap_or([local_width, local_height]);
                    render.transparent(width, height)
                })
            }
        };
        for frame in local_outputs.into_values() {
            if !frame.shares_surface(&output) {
                render.recycle(frame);
            }
        }
        if !local_input.shares_surface(&output) {
            render.recycle(local_input);
        }
        output
    }
}

fn local_node_evaluation_order(instance: &PipelineInstance) -> Vec<usize> {
    ImageGraphIndex::new(&instance.local_nodes).stack_evaluation_order(&instance.local_output)
}

fn quantize_composition_time(time: f32, frame_rate: f64) -> f32 {
    let fps = frame_rate.max(1.0);
    ((time.max(0.0) as f64 * fps).floor() / fps) as f32
}

#[derive(Clone, Copy)]
struct MonitorChromeLayout {
    fit_stage: Rect,
    combo: Rect,
    frame_snap: Rect,
    clip_snap: Rect,
    pen: Rect,
    mute: Rect,
    capture: [Rect; 3],
}

fn monitor_chrome_layout(rect: Rect) -> MonitorChromeLayout {
    let vertical = crate::ui_layout::column(
        rect,
        &[
            crate::ui_layout::Item::height(0.0),
            crate::ui_layout::Item::fill(),
            crate::ui_layout::Item::height(6.0),
            crate::ui_layout::Item::height(32.0),
            crate::ui_layout::Item::height(0.0),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
        None,
    );
    let fit_stage = vertical[1];
    let status = vertical[3];
    let combo_w = 172.0_f32.min((status.width - 8.0).max(1.0));
    let parts = crate::ui_layout::row(
        status,
        &[
            crate::ui_layout::Item::width(4.0),
            crate::ui_layout::Item::new(Size::Pixels(combo_w), Size::Pixels(28.0)),
            crate::ui_layout::Item::width(6.0),
            crate::ui_layout::Item::new(Size::Pixels(28.0), Size::Pixels(28.0)),
            crate::ui_layout::Item::width(4.0),
            crate::ui_layout::Item::new(Size::Pixels(28.0), Size::Pixels(28.0)),
            crate::ui_layout::Item::width(6.0),
            crate::ui_layout::Item::new(Size::Pixels(28.0), Size::Pixels(28.0)),
            crate::ui_layout::Item::width(6.0),
            crate::ui_layout::Item::new(Size::Pixels(28.0), Size::Pixels(28.0)),
            crate::ui_layout::Item::width(6.0),
            crate::ui_layout::Item::new(Size::Pixels(28.0), Size::Pixels(28.0)),
            crate::ui_layout::Item::width(4.0),
            crate::ui_layout::Item::new(Size::Pixels(28.0), Size::Pixels(28.0)),
            crate::ui_layout::Item::width(4.0),
            crate::ui_layout::Item::new(Size::Pixels(28.0), Size::Pixels(28.0)),
            crate::ui_layout::Item::fill(),
        ],
        0.0,
        0.0,
        kama_ui::Align::Center,
    );
    MonitorChromeLayout {
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
    monitor_chrome_layout(rect).combo
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
    let layout = monitor_chrome_layout(rect);
    if clip {
        layout.clip_snap
    } else {
        layout.frame_snap
    }
}

fn monitor_pen_button_rect(rect: Rect) -> Rect {
    monitor_chrome_layout(rect).pen
}

fn monitor_mute_button_rect(rect: Rect) -> Rect {
    monitor_chrome_layout(rect).mute
}

fn monitor_capture_button_rect(rect: Rect, index: usize) -> Rect {
    monitor_chrome_layout(rect).capture[index]
}

const MONITOR_MIN_ZOOM: f32 = 0.05;
const MONITOR_MAX_ZOOM: f32 = 16.0;

fn monitor_stage_rect(rect: Rect) -> Rect {
    rect
}

fn monitor_fit_preview_rect(rect: Rect, canvas_width: u32, canvas_height: u32) -> Rect {
    const FIT_PADDING: f32 = 8.0;

    let stage = monitor_chrome_layout(rect).fit_stage;
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

fn persistent_wasm_graph_frame_key(memory_key: u64, module: &Path, entry: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    "kama-wasm-graph-frame-v2".hash(&mut hasher);
    memory_key.hash(&mut hasher);
    module.hash(&mut hasher);
    entry.hash(&mut hasher);
    if let Some(fingerprint) = embedded_vfs::fingerprint(module) {
        fingerprint.hash(&mut hasher);
    } else if let Ok(metadata) = std::fs::metadata(module) {
        metadata.len().hash(&mut hasher);
        if let Ok(modified) = metadata.modified() {
            if let Ok(since_epoch) = modified.duration_since(std::time::UNIX_EPOCH) {
                since_epoch.as_secs().hash(&mut hasher);
                since_epoch.subsec_nanos().hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

fn generator_render_cache_key(
    generator_type: &str,
    parameters: &std::collections::BTreeMap<String, crate::project::HostBinding>,
    keyframe_time: f64,
    local_time: f64,
    uses_time: bool,
    render_scale: f32,
    preview_size: [u32; 2],
) -> u64 {
    let [preview_width, preview_height] = preview_size;
    let mut hasher = DefaultHasher::new();
    generator_type.hash(&mut hasher);
    preview_width.hash(&mut hasher);
    preview_height.hash(&mut hasher);
    render_scale.to_bits().hash(&mut hasher);
    for (name, binding) in parameters {
        name.hash(&mut hasher);
        hash_generator_value(binding.evaluate(keyframe_time).as_ref(), &mut hasher);
    }
    if uses_time {
        local_time.to_bits().hash(&mut hasher);
    }
    hasher.finish()
}

fn hash_generator_value(value: Option<&crate::project::HostValue>, hasher: &mut impl Hasher) {
    use crate::project::HostValue;

    match value {
        None => 0u8.hash(hasher),
        Some(HostValue::Vec2Array(value)) => {
            1u8.hash(hasher);
            value.len().hash(hasher);
            for point in value {
                point[0].to_bits().hash(hasher);
                point[1].to_bits().hash(hasher);
            }
        }
        Some(HostValue::F32List(value)) => {
            2u8.hash(hasher);
            value.len().hash(hasher);
            for component in value {
                component.to_bits().hash(hasher);
            }
        }
        Some(HostValue::String(value)) => {
            3u8.hash(hasher);
            value.hash(hasher);
        }
        Some(HostValue::Bytes(value)) => {
            4u8.hash(hasher);
            value.hash(hasher);
        }
        Some(HostValue::Gpu(value)) => {
            5u8.hash(hasher);
            hash_gpu_value(*value, hasher);
        }
    }
}

fn hash_gpu_value(value: GpuValue, hasher: &mut impl Hasher) {
    match value {
        GpuValue::F32(value) => {
            0u8.hash(hasher);
            value.to_bits().hash(hasher);
        }
        GpuValue::I32(value) => {
            1u8.hash(hasher);
            value.hash(hasher);
        }
        GpuValue::U32(value) => {
            2u8.hash(hasher);
            value.hash(hasher);
        }
        GpuValue::Bool(value) => {
            3u8.hash(hasher);
            value.hash(hasher);
        }
        GpuValue::Vec2(value) => {
            4u8.hash(hasher);
            for component in value {
                component.to_bits().hash(hasher);
            }
        }
        GpuValue::Vec3(value) => {
            5u8.hash(hasher);
            for component in value {
                component.to_bits().hash(hasher);
            }
        }
        GpuValue::Vec4(value) => {
            6u8.hash(hasher);
            for component in value {
                component.to_bits().hash(hasher);
            }
        }
        GpuValue::Color(value) => {
            7u8.hash(hasher);
            for component in value {
                component.to_bits().hash(hasher);
            }
        }
        GpuValue::Enum(value) => {
            8u8.hash(hasher);
            value.hash(hasher);
        }
    }
}

fn render_signature(
    project: &Project,
    timeline: &TimelineState,
    preview_width: u32,
    preview_height: u32,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    let preview_fps = if timeline.is_scrubbing() {
        SCRUB_PREVIEW_FPS
    } else {
        project.active_settings().frame_rate.max(1.0)
    };
    let preview_frame = (timeline.playhead().max(0.0) as f64 * preview_fps).floor() as i64;
    preview_frame.hash(&mut hasher);
    timeline.is_scrubbing().hash(&mut hasher);
    preview_width.hash(&mut hasher);
    preview_height.hash(&mut hasher);
    project.active_settings().canvas_size[0].hash(&mut hasher);
    project.active_settings().canvas_size[1].hash(&mut hasher);
    project
        .active_settings()
        .frame_rate
        .to_bits()
        .hash(&mut hasher);
    match project.active_settings().background {
        ProjectBackground::Transparent => 0u8.hash(&mut hasher),
        ProjectBackground::Solid { color } => {
            1u8.hash(&mut hasher);
            for channel in color {
                channel.to_bits().hash(&mut hasher);
            }
        }
    }

    project.media.len().hash(&mut hasher);
    timeline.tracks().len().hash(&mut hasher);
    timeline.clips().len().hash(&mut hasher);
    for pipeline in &project.pipelines {
        pipeline.id.hash(&mut hasher);
        pipeline.revision.hash(&mut hasher);
    }
    hasher.finish()
}

fn generator_string(
    parameters: &BTreeMap<String, HostBinding>,
    input: &str,
    time: f64,
) -> Option<String> {
    match generator_parameter(parameters, input, time)? {
        crate::project::HostValue::String(value) => Some(value),
        _ => None,
    }
}

fn generator_parameter(
    parameters: &BTreeMap<String, HostBinding>,
    input: &str,
    time: f64,
) -> Option<crate::project::HostValue> {
    parameters.get(input)?.evaluate(time)
}

fn generator_f32(
    parameters: &BTreeMap<String, HostBinding>,
    input: &str,
    time: f64,
) -> Option<f32> {
    match generator_parameter(parameters, input, time)? {
        crate::project::HostValue::Gpu(GpuValue::F32(value)) => Some(value),
        _ => None,
    }
}

fn generator_u32(
    parameters: &BTreeMap<String, HostBinding>,
    input: &str,
    time: f64,
) -> Option<u32> {
    match generator_parameter(parameters, input, time)? {
        crate::project::HostValue::Gpu(GpuValue::U32(value) | GpuValue::Enum(value)) => Some(value),
        _ => None,
    }
}

fn generator_vec2(
    parameters: &BTreeMap<String, HostBinding>,
    input: &str,
    time: f64,
) -> Option<[f32; 2]> {
    match generator_parameter(parameters, input, time)? {
        crate::project::HostValue::Gpu(GpuValue::Vec2(value)) => Some(value),
        _ => None,
    }
}

fn generator_vec2_array(
    parameters: &BTreeMap<String, HostBinding>,
    input: &str,
    time: f64,
) -> Option<Vec<[f32; 2]>> {
    match generator_parameter(parameters, input, time)? {
        crate::project::HostValue::Vec2Array(value) => Some(value),
        _ => None,
    }
}

fn tight_generator_source_geometry(
    source: &VisualSource,
    local_time: f64,
    plugins: &PluginRegistry,
    canvas_size: [u32; 2],
    preview_width: u32,
    preview_height: u32,
) -> Option<SourceGeometry> {
    let VisualSource::Generator(GeneratorSource::Plugin {
        generator_type,
        parameters,
    }) = source
    else {
        return None;
    };
    let definition = plugins.generator(generator_type)?;
    let (x, y, width, height) = generator_content_bounds(definition, parameters, local_time)?;
    let canvas = [canvas_size[0].max(1) as f32, canvas_size[1].max(1) as f32];
    let position_offset = [
        (x + width as f32 * 0.5) / canvas[0] - 0.5,
        (y + height as f32 * 0.5) / canvas[1] - 0.5,
    ];
    Some(scaled_source_geometry(
        (width, height),
        position_offset,
        canvas_size,
        preview_width,
        preview_height,
    ))
}

fn scaled_source_geometry(
    dimensions: (u32, u32),
    position_offset: [f32; 2],
    canvas_size: [u32; 2],
    preview_width: u32,
    preview_height: u32,
) -> SourceGeometry {
    let (width, height) = dimensions;
    if width == 0 || height == 0 {
        return SourceGeometry::canvas(preview_width, preview_height);
    }
    let output_scale = (preview_width as f32 / canvas_size[0].max(1) as f32)
        .min(preview_height as f32 / canvas_size[1].max(1) as f32)
        .max(0.000_001);
    let scale = output_scale
        .min(preview_width as f32 / width as f32)
        .min(preview_height as f32 / height as f32);
    SourceGeometry {
        size: (
            (width as f32 * scale).round().max(1.0) as u32,
            (height as f32 * scale).round().max(1.0) as u32,
        ),
        position_offset,
    }
}

fn transform_with_position_offset(
    node: &crate::effects::EffectNode,
    keyframe_time: f64,
    offset: [f32; 2],
) -> crate::effects::EffectNode {
    let mut node = node.clone();
    let position = node
        .inputs
        .get("position")
        .and_then(|binding| binding.evaluate(keyframe_time))
        .and_then(GpuValue::vec2)
        .unwrap_or([0.5, 0.5]);
    node.inputs.insert(
        "position".into(),
        crate::effects::Binding::Constant(GpuValue::Vec2([
            position[0] + offset[0],
            position[1] + offset[1],
        ])),
    );
    node
}

fn generator_content_bounds(
    definition: &GeneratorDefinition,
    parameters: &BTreeMap<String, HostBinding>,
    time: f64,
) -> Option<(f32, f32, u32, u32)> {
    let bounds = definition.bounds.as_ref()?;
    let points = generator_vec2_array(parameters, &bounds.points_input, time)?;
    if points.len() < 2 {
        return None;
    }
    let scalar_padding = |input: &str| {
        generator_parameter(parameters, input, time)
            .and_then(|value| match value {
                crate::project::HostValue::Gpu(value) => {
                    value.numeric(None).map(|value| value as f32)
                }
                _ => None,
            })
            .unwrap_or(0.0)
            .max(0.0)
    };
    let padding = bounds
        .padding_input
        .as_deref()
        .into_iter()
        .chain(bounds.padding_inputs.iter().map(String::as_str))
        .map(scalar_padding)
        .sum::<f32>()
        .ceil()
        .max(1.0)
        + 1.0;
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for point in &points {
        min_x = min_x.min(point[0]);
        min_y = min_y.min(point[1]);
        max_x = max_x.max(point[0]);
        max_y = max_y.max(point[1]);
    }
    if !min_x.is_finite() || !min_y.is_finite() || !max_x.is_finite() || !max_y.is_finite() {
        return None;
    }
    let min_x = min_x - padding;
    let min_y = min_y - padding;
    let width = (max_x - min_x + padding).ceil().max(1.0) as u32;
    let height = (max_y - min_y + padding).ceil().max(1.0) as u32;
    Some((min_x, min_y, width, height))
}

fn resolved_graph_node_value(
    node: &crate::effects::EffectNode,
    instance: Option<&PipelineInstance>,
    values: &RefCell<ValueEvaluator<'_>>,
    input: &str,
) -> Option<GpuValue> {
    let mut values = values.borrow_mut();
    resolved_node_input_cached(node, instance, input, &mut values)
}

fn resolved_graph_generator_parameters(
    node: &crate::effects::EffectNode,
    instance: Option<&PipelineInstance>,
    values: &RefCell<ValueEvaluator<'_>>,
    time: f64,
) -> BTreeMap<String, HostBinding> {
    let mut parameters = node
        .host_inputs
        .iter()
        .filter_map(|(name, binding)| {
            binding
                .evaluate(time)
                .map(|value| (name.clone(), HostBinding::Constant(value)))
        })
        .collect::<BTreeMap<_, _>>();
    for name in node.inputs.keys() {
        if let Some(value) = resolved_graph_node_value(node, instance, values, name) {
            parameters.insert(
                name.clone(),
                HostBinding::Constant(crate::project::HostValue::Gpu(value)),
            );
        }
    }
    parameters
}

fn scoped_clip_id(scope: u64, clip: u32) -> u64 {
    let mut hasher = DefaultHasher::new();
    scope.hash(&mut hasher);
    clip.hash(&mut hasher);
    hasher.finish() | (1u64 << 63)
}

fn nested_cache_scope(parent_clip_key: u64, composition: CompositionId) -> u64 {
    let mut hasher = DefaultHasher::new();
    parent_clip_key.hash(&mut hasher);
    composition.hash(&mut hasher);
    hasher.finish()
}

fn live_cache_clip_ids(project: &Project, timeline: &TimelineState) -> HashSet<u64> {
    let mut ids = timeline
        .clips()
        .iter()
        .map(|clip| u64::from(clip.id))
        .collect::<HashSet<_>>();
    let mut active_path = HashSet::new();
    active_path.insert(project.active_composition);
    collect_nested_cache_clip_ids(
        project,
        timeline.clips(),
        None,
        0,
        &mut active_path,
        &mut ids,
    );
    ids
}

fn collect_nested_cache_clip_ids(
    project: &Project,
    clips: &[Clip],
    scope: Option<u64>,
    depth: usize,
    active_path: &mut HashSet<CompositionId>,
    ids: &mut HashSet<u64>,
) {
    if depth >= 16 {
        return;
    }
    for clip in clips {
        let VisualSource::Composition(composition_id) = &clip.source else {
            continue;
        };
        let Some(composition) = project.composition(*composition_id) else {
            continue;
        };
        if !active_path.insert(composition.id) {
            continue;
        }
        let parent_clip_key = scope
            .map(|scope| scoped_clip_id(scope, clip.id))
            .unwrap_or(u64::from(clip.id));
        let child_scope = nested_cache_scope(parent_clip_key, composition.id);
        for child in &composition.timeline.clips {
            ids.insert(scoped_clip_id(child_scope, child.id));
        }
        collect_nested_cache_clip_ids(
            project,
            &composition.timeline.clips,
            Some(child_scope),
            depth + 1,
            active_path,
            ids,
        );
        active_path.remove(&composition.id);
    }
}

fn frame_from_image(
    image: image::DynamicImage,
    preview_width: u32,
    preview_height: u32,
) -> CpuFrame {
    let image = image.resize(
        preview_width,
        preview_height,
        image::imageops::FilterType::Triangle,
    );
    let rgba = image.to_rgba32f();
    let (width, height) = rgba.dimensions();
    let mut frame = CpuFrame::transparent(width, height);
    for (x, y, pixel) in rgba.enumerate_pixels() {
        let alpha = pixel[3].clamp(0.0, 1.0);
        let rgb = [
            srgb_to_linear(pixel[0].max(0.0)),
            srgb_to_linear(pixel[1].max(0.0)),
            srgb_to_linear(pixel[2].max(0.0)),
        ];
        frame.set_rgba(
            x,
            y,
            [rgb[0] * alpha, rgb[1] * alpha, rgb[2] * alpha, alpha],
        );
    }
    frame
}

fn placeholder_frame(label: &str, preview_width: u32, preview_height: u32) -> CpuFrame {
    let mut frame = CpuFrame::transparent(preview_width, preview_height);
    let hash = label
        .bytes()
        .fold(0u32, |hash, byte| hash.wrapping_mul(16777619) ^ byte as u32);
    let tint = [
        0.08 + ((hash & 0xff) as f32 / 255.0) * 0.10,
        0.08 + (((hash >> 8) & 0xff) as f32 / 255.0) * 0.10,
        0.08 + (((hash >> 16) & 0xff) as f32 / 255.0) * 0.10,
        1.0,
    ];
    for y in 0..preview_height {
        for x in 0..preview_width {
            let checker = ((x / 32 + y / 32) & 1) as f32 * 0.035;
            frame.set_rgba(
                x,
                y,
                [tint[0] + checker, tint[1] + checker, tint[2] + checker, 1.0],
            );
        }
    }
    frame
}

fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod snap_tests {
    use super::*;
    use crate::effects::{EffectNode, NodeExecution, SocketRef};

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
