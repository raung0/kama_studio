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

use anyhow::{Context, Error, Result};
use kama_ui::{ExternalTextureId, Renderer};

use crate::{
    clip_graph_cache,
    effects::{
        resolved_node_input_cached, EffectRuntime, GpuValue, ImageBinding, ImageGraphIndex,
        PipelineInstance, ValueEvaluator,
    },
    embedded_vfs, messages,
    plugin::{GeneratorBackend, GeneratorDefinition, PluginRegistry},
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
    runtime::wasm::{WasmRenderRequest, WasmRuntime},
    timeline::{Clip, TimelineState, TrackKind},
};

use super::{
    decode_pool::VideoDecoderPool,
    export_readback::{ExportReadbacks, ExportRgba16Args, ExportYuvBatchArgs},
    preload::{upcoming_video_preloads, VIDEO_CLIP_PRELOAD_LIMIT},
};

#[derive(Clone, Debug)]
pub(crate) struct RenderCachePreview {
    pub path: PathBuf,
    pub local_time: f64,
    pub generation: u64,
    pub frame: u64,
}

pub(crate) const GRAPH_GENERATOR_VARIANT_CAPACITY: usize = 4;

#[derive(Debug)]
pub(crate) struct GraphGeneratorVariants<T> {
    pub(crate) variants: crate::app_shared::BoundedCache<u64, T>,
}

impl<T> Default for GraphGeneratorVariants<T> {
    fn default() -> Self {
        Self {
            variants: Default::default(),
        }
    }
}

impl<T: Clone> GraphGeneratorVariants<T> {
    pub(crate) fn get(&mut self, key: u64) -> Option<T> {
        self.variants.get(&key).cloned()
    }

    fn latest(&self) -> Option<T> {
        self.variants.latest().cloned()
    }

    pub(crate) fn insert(&mut self, key: u64, value: T) {
        self.variants.insert(key, value);
        self.variants
            .trim(GRAPH_GENERATOR_VARIANT_CAPACITY, usize::MAX, |_| 0);
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SourceGeometry {
    pub(crate) size: (u32, u32),
    pub(crate) position_offset: [f32; 2],
}

#[derive(Clone, Copy)]
pub(crate) struct PreviewOutput<'a> {
    pub(crate) texture: Option<ExternalTextureId>,
    pub(crate) source_geometry: &'a HashMap<u32, SourceGeometry>,
}

impl SourceGeometry {
    pub(crate) fn canvas(width: u32, height: u32) -> Self {
        Self {
            size: (width, height),
            position_offset: [0.0, 0.0],
        }
    }
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

pub(crate) struct FrameRenderer {
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
    wasm: Option<WasmRuntime>,
    last_signature: Option<u64>,
    waiting_for_video: bool,
}

impl Default for FrameRenderer {
    fn default() -> Self {
        Self {
            texture: None,
            presentation: None,
            gpu: None,
            image_cache: HashMap::new(),
            image_gpu_cache: HashMap::new(),
            model_gpu: None,
            video_decoders: VideoDecoderPool::default(),
            export_video_decoders: HashMap::new(),
            export_decode_epoch: 0,
            video_gpu_cache: HashMap::new(),
            render_cache_decoder: None,
            render_cache_gpu: None,
            export_readbacks: ExportReadbacks::default(),
            generator_gpu_cache: HashMap::new(),
            graph_generator_gpu_cache: HashMap::new(),
            generator_worker: None,
            source_geometry: HashMap::new(),
            wasm: WasmRuntime::new()
                .map_err(|error| {
                    messages::error("WASM generator", format!("runtime unavailable: {error:#}"));
                    error
                })
                .ok(),
            last_signature: None,
            waiting_for_video: false,
        }
    }
}

impl FrameRenderer {
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

    pub(crate) fn new(
        renderer: &Renderer,
        effects: &EffectRuntime,
        plugins: &PluginRegistry,
    ) -> Self {
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

    pub(crate) fn preview_output(&self) -> PreviewOutput<'_> {
        PreviewOutput {
            texture: self.texture,
            source_geometry: &self.source_geometry,
        }
    }

    pub(crate) fn is_waiting_for_video(&self) -> bool {
        self.waiting_for_video
    }

    pub(crate) fn clear_frame_caches(&mut self) {
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
        request: WasmGeneratorRender<'_>,
        render_scale: f32,
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
        let scale = render_scale;
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

    pub(crate) fn sync_compiled_effects(
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

    pub(crate) fn precompile_wasm(&mut self, path: &std::path::Path) -> Result<()> {
        if let Some(runtime) = &mut self.wasm {
            runtime.precompile(path)?;
        }
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
                wgpu::TexelCopyTextureInfo {
                    texture: &surface.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &surface.buffer,
                    layout: wgpu::TexelCopyBufferLayout {
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
                        let _ = device.poll(wgpu::PollType::Poll);
                        std::thread::sleep(std::time::Duration::from_micros(250));
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        anyhow::bail!("export readback callback dropped");
                    }
                }
            };
            map_result.map_err(|error| anyhow::anyhow!("map export frame: {error}"))?;
            let mapped = slice
                .get_mapped_range()
                .map_err(|error| anyhow::anyhow!("get mapped export frame: {error}"))?;
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
                        let _ = device.poll(wgpu::PollType::Wait {
                            submission_index: Some(submission),
                            timeout: None,
                        });
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
                    let mapped = match slice.get_mapped_range() {
                        Ok(mapped) => mapped,
                        Err(error) => {
                            map_error.get_or_insert_with(|| error.to_string());
                            surface.buffer.unmap();
                            continue;
                        }
                    };
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

    fn begin_export_decode_frame(&mut self) {
        self.export_decode_epoch = self.export_decode_epoch.wrapping_add(1).max(1);
    }

    fn finish_export_decode_frame(&mut self) {
        let epoch = self.export_decode_epoch;

        self.export_video_decoders
            .retain(|_, (_, _, last_used)| *last_used == epoch);
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
                    render.render_scale,
                    [preview_width, preview_height],
                );
                if let Some(cached) = self.cached_generator_frame(clip_id, cache_key) {
                    return Ok(cached);
                }

                if !render.blocking_decode {
                    let preview_scale = render.render_scale;
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
                            render.render_scale,
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
                    render.render_scale,
                    [preview_width, preview_height],
                );
                if let Some(cached) = self.cached_generator_frame(clip_id, cache_key) {
                    return Ok(cached);
                }
                if !render.blocking_decode {
                    let preview_scale = render.render_scale;
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
                    render.render_scale,
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
        let wasm_scale = render.render_scale;
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

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn refresh_preview(
        &mut self,
        renderer: &mut Renderer,
        project: &Project,
        timeline: &TimelineState,
        effects: &EffectRuntime,
        plugins: &PluginRegistry,
        render_cache: Option<&RenderCachePreview>,
        preview_size: [u32; 2],
        render_scale: f32,
        captured_frame: Option<([u32; 2], &[u8])>,
    ) -> Result<()> {
        let [preview_width, preview_height] = preview_size;
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
        if let Some((_, pixels)) = captured_frame {
            let mut hasher = DefaultHasher::new();
            signature.hash(&mut hasher);
            0x0043_4150_5455_5245_u64.hash(&mut hasher);
            pixels.len().hash(&mut hasher);
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

        let mut gpu = self.gpu.take().expect("preview GPU initialized");
        let result = (|| -> Result<()> {
            let device = renderer.device();
            let queue = renderer.queue();
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("kama playback render graph"),
            });
            gpu.begin_submission();
            let frame = if let Some(([capture_width, capture_height], pixels)) = captured_frame {
                let captured = VideoFrame::from_rgba16(
                    preview_width,
                    preview_height,
                    capture_width.max(1),
                    capture_height.max(1),
                    pixels.to_vec(),
                    1,
                    false,
                );
                let captured = Arc::new(captured);
                let surface = gpu.video_upload_surface(device, captured.as_ref());
                let _ = gpu.upload_video_into(queue, &mut encoder, &surface, captured.as_ref());
                surface.frame()
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
                    render_scale,
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
                .expect("preview presentation initialized");
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

    fn preload_upcoming_videos(
        &mut self,
        project: &Project,
        timeline: &TimelineState,
        width: u32,
        height: u32,
    ) {
        let targets = upcoming_video_preloads(
            project,
            timeline.tracks(),
            timeline.clips(),
            project.active_settings().frame_rate,
            project.active_settings().canvas_size,
            [width, height],
            timeline.playhead(),
        );
        let mut warmed_tracks = HashSet::new();
        let mut warmed_clips = HashSet::new();
        let mut warmed = 0usize;
        for target in targets {
            if !warmed_tracks.insert((target.track_scope, target.track))
                || !warmed_clips.insert(target.clip_key)
            {
                continue;
            }
            self.video_decoders
                .get(target.clip_key, &target.path, false)
                .preload(
                    target.source_time,
                    target.source_fps,
                    target.source_step_seconds,
                    target.width,
                    target.height,
                );
            warmed += 1;
            if warmed >= VIDEO_CLIP_PRELOAD_LIMIT {
                break;
            }
        }
    }

    pub(crate) fn invalidate(&mut self) {
        self.last_signature = None;
    }

    pub(crate) fn clear_media_caches(&mut self) {
        self.clear_frame_caches();
        self.video_decoders.clear();
        self.export_video_decoders.clear();
        self.last_signature = None;
    }

    pub(crate) fn clear_caches(&mut self) {
        self.clear_media_caches();
        if let Some(wasm) = &mut self.wasm {
            wasm.clear();
        }
    }
}

pub(crate) fn local_node_evaluation_order(instance: &PipelineInstance) -> Vec<usize> {
    ImageGraphIndex::new(&instance.local_nodes).stack_evaluation_order(&instance.local_output)
}

pub(crate) fn quantize_composition_time(time: f32, frame_rate: f64) -> f32 {
    let fps = frame_rate.max(1.0);
    ((time.max(0.0) as f64 * fps).floor() / fps) as f32
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

pub(crate) fn generator_render_cache_key(
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

pub(crate) fn tight_generator_source_geometry(
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

pub(super) fn scaled_source_geometry(
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

pub(crate) fn generator_content_bounds(
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

pub(super) fn scoped_clip_id(scope: u64, clip: u32) -> u64 {
    let mut hasher = DefaultHasher::new();
    scope.hash(&mut hasher);
    clip.hash(&mut hasher);
    hasher.finish() | (1u64 << 63)
}

pub(super) fn nested_cache_scope(parent_clip_key: u64, composition: CompositionId) -> u64 {
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
