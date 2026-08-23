use std::{
    cell::RefCell,
    collections::{hash_map::DefaultHasher, HashMap},
    hash::{Hash, Hasher},
    num::NonZeroU64,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use anyhow::{bail, Context, Result};
use bytemuck::{Pod, Zeroable};
use ffmpeg_next::frame::Video as AvVideoFrame;
use wgpu::util::DeviceExt;

use crate::{
    clip_graph_cache,
    effects::{
        resolved_node_input_cached, CompiledStage, EffectNode, EffectRuntime, GpuValue,
        NodeExecution, PipelineInstance, ValueEvalContext, ValueEvaluator,
    },
    messages,
    plugin::{GeneratorBackend, GeneratorDefinition, PluginRegistry},
    project::{AlphaBlendMode, BlendMode, HostBinding, HostValue},
    shader_codegen::{
        build_fused_pointwise_shader, build_generator_shader, build_standalone_shader,
    },
};

const GPU_FRAME_POOL_CAPACITY: usize = 8;
const GPU_FRAME_POOL_MAX_BYTES: u64 = 512 * 1024 * 1024;
const UNIFORM_UPLOAD_CHUNK_BYTES: u64 = 256 * 1024;
const BIND_GROUP_CACHE_CAPACITY: usize = 64;

static NEXT_GPU_SURFACE_ID: AtomicU64 = AtomicU64::new(1);

struct UniformUploadChunk {
    buffer: Arc<wgpu::Buffer>,
    capacity: u64,
}

struct UniformAllocation {
    buffer: Arc<wgpu::Buffer>,
    chunk_index: usize,
    offset: u64,
    size: NonZeroU64,
}

struct UniformUploadArena {
    chunks: Vec<UniformUploadChunk>,
    chunk_index: usize,
    offset: u64,
    alignment: u64,
}

impl UniformUploadArena {
    fn new(device: &wgpu::Device) -> Self {
        Self {
            chunks: Vec::new(),
            chunk_index: 0,
            offset: 0,
            alignment: u64::from(device.limits().min_uniform_buffer_offset_alignment).max(1),
        }
    }

    fn begin_submission(&mut self) {
        self.chunk_index = 0;
        self.offset = 0;
    }

    fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        label: &str,
        bytes: &[u8],
    ) -> UniformAllocation {
        debug_assert!(!bytes.is_empty());
        let size = bytes.len() as u64;
        loop {
            if self.chunk_index == self.chunks.len() {
                let required = align_up(size, self.alignment);
                let capacity = UNIFORM_UPLOAD_CHUNK_BYTES.max(required.next_power_of_two());
                let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size: capacity,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                self.chunks.push(UniformUploadChunk {
                    buffer: Arc::new(buffer),
                    capacity,
                });
            }

            let offset = align_up(self.offset, self.alignment);
            let chunk = &self.chunks[self.chunk_index];
            if offset.saturating_add(size) <= chunk.capacity {
                queue.write_buffer(chunk.buffer.as_ref(), offset, bytes);
                self.offset = offset + size;
                return UniformAllocation {
                    buffer: Arc::clone(&chunk.buffer),
                    chunk_index: self.chunk_index,
                    offset,
                    size: NonZeroU64::new(size).expect("uniform uploads are non-empty"),
                };
            }

            self.chunk_index += 1;
            self.offset = 0;
        }
    }
}

fn align_up(value: u64, alignment: u64) -> u64 {
    value.div_ceil(alignment) * alignment
}

#[derive(Clone)]
pub struct CpuFrame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<f32>,
}

impl CpuFrame {
    pub fn from_pixels(width: u32, height: u32, pixels: Vec<f32>) -> Self {
        debug_assert_eq!(pixels.len(), width as usize * height as usize * 4);
        Self {
            width,
            height,
            pixels,
        }
    }

    pub fn transparent(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0.0; width as usize * height as usize * 4],
        }
    }

    pub fn rgba(&self, x: u32, y: u32) -> [f32; 4] {
        let index = ((y * self.width + x) * 4) as usize;
        [
            self.pixels[index],
            self.pixels[index + 1],
            self.pixels[index + 2],
            self.pixels[index + 3],
        ]
    }

    pub fn set_rgba(&mut self, x: u32, y: u32, value: [f32; 4]) {
        let index = ((y * self.width + x) * 4) as usize;
        self.pixels[index..index + 4].copy_from_slice(&value);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeVideoLayout {
    Yuv420p,
    Nv12,

    P010,
    Yuv422p,

    P210,
    Yuv444p,

    Ayuv,
}

impl NativeVideoLayout {
    pub(crate) fn chroma_width(self, width: u32) -> u32 {
        match self {
            Self::Yuv420p | Self::Nv12 | Self::P010 | Self::Yuv422p | Self::P210 => {
                width.div_ceil(2)
            }
            Self::Yuv444p | Self::Ayuv => width,
        }
    }

    pub(crate) fn chroma_height(self, height: u32) -> u32 {
        match self {
            Self::Yuv420p | Self::Nv12 | Self::P010 => height.div_ceil(2),
            Self::Yuv422p | Self::P210 | Self::Yuv444p | Self::Ayuv => height,
        }
    }

    fn interleaved_uv(self) -> bool {
        matches!(self, Self::Nv12 | Self::P010 | Self::P210)
    }

    fn packed_ayuv(self) -> bool {
        self == Self::Ayuv
    }
}

#[derive(Clone)]
pub enum VideoFramePixels {
    Rgba16(Vec<u8>),

    NativeYuv {
        layout: NativeVideoLayout,
        bit_depth: u32,

        frame: Arc<AvVideoFrame>,
        has_alpha: bool,
    },
}

#[derive(Clone)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub source_width: u32,
    pub source_height: u32,
    pub fit_width: u32,
    pub fit_height: u32,
    pub pixels: VideoFramePixels,

    pub transfer: u32,
    pub bt2020_primaries: bool,

    pub yuv_matrix: u32,
    pub full_range: bool,
}

impl VideoFrame {
    pub fn from_rgba16(
        width: u32,
        height: u32,
        fit_width: u32,
        fit_height: u32,
        pixels: Vec<u8>,
        transfer: u32,
        bt2020_primaries: bool,
    ) -> Self {
        debug_assert_eq!(pixels.len(), fit_width as usize * fit_height as usize * 8);
        Self {
            width,
            height,
            source_width: fit_width,
            source_height: fit_height,
            fit_width,
            fit_height,
            pixels: VideoFramePixels::Rgba16(pixels),
            transfer,
            bt2020_primaries,
            yuv_matrix: 1,
            full_range: true,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn from_native_yuv(
        width: u32,
        height: u32,
        source_width: u32,
        source_height: u32,
        fit_width: u32,
        fit_height: u32,
        layout: NativeVideoLayout,
        bit_depth: u32,
        frame: Arc<AvVideoFrame>,
        has_alpha: bool,
        transfer: u32,
        bt2020_primaries: bool,
        yuv_matrix: u32,
        full_range: bool,
    ) -> Self {
        Self {
            width,
            height,
            source_width,
            source_height,
            fit_width,
            fit_height,
            pixels: VideoFramePixels::NativeYuv {
                layout,
                bit_depth,
                frame,
                has_alpha,
            },
            transfer,
            bt2020_primaries,
            yuv_matrix,
            full_range,
        }
    }

    pub fn byte_len(&self) -> usize {
        match &self.pixels {
            VideoFramePixels::Rgba16(pixels) => pixels.len(),
            VideoFramePixels::NativeYuv {
                layout,
                frame,
                has_alpha,
                ..
            } => native_frame_byte_len(*layout, frame, *has_alpha),
        }
    }

    pub fn native_layout(&self) -> Option<NativeVideoLayout> {
        match &self.pixels {
            VideoFramePixels::NativeYuv { layout, .. } => Some(*layout),
            VideoFramePixels::Rgba16(_) => None,
        }
    }

    fn native_bit_depth(&self) -> Option<u32> {
        match &self.pixels {
            VideoFramePixels::NativeYuv { bit_depth, .. } => Some(*bit_depth),
            VideoFramePixels::Rgba16(_) => None,
        }
    }

    fn has_alpha(&self) -> bool {
        matches!(
            &self.pixels,
            VideoFramePixels::NativeYuv {
                layout: NativeVideoLayout::Ayuv,
                ..
            } | VideoFramePixels::NativeYuv {
                has_alpha: true,
                ..
            }
        )
    }
}

struct PackedVideoUploadSurface {
    source_width: u32,
    source_height: u32,
    source: wgpu::Texture,
    output: GpuFrame,
    color_uniform: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

struct NativeVideoUploadSurface {
    layout: NativeVideoLayout,
    bit_depth: u32,
    source_width: u32,
    source_height: u32,
    y: wgpu::Texture,
    u_or_uv: wgpu::Texture,
    v: wgpu::Texture,
    alpha: wgpu::Texture,
    output: GpuFrame,
    color_uniform: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

enum VideoUploadSurfaceInner {
    Packed(Box<PackedVideoUploadSurface>),
    NativeYuv(Box<NativeVideoUploadSurface>),
}

pub struct VideoUploadSurface {
    inner: VideoUploadSurfaceInner,
}

impl VideoUploadSurface {
    pub fn frame(&self) -> GpuFrame {
        match &self.inner {
            VideoUploadSurfaceInner::Packed(surface) => surface.output.clone(),
            VideoUploadSurfaceInner::NativeYuv(surface) => surface.output.clone(),
        }
    }

    pub fn matches(&self, frame: &VideoFrame) -> bool {
        match (&self.inner, frame.native_layout()) {
            (VideoUploadSurfaceInner::Packed(surface), None) => {
                surface.source_width == frame.source_width
                    && surface.source_height == frame.source_height
                    && surface.output.width == frame.width
                    && surface.output.height == frame.height
            }
            (VideoUploadSurfaceInner::NativeYuv(surface), Some(layout)) => {
                surface.layout == layout
                    && Some(surface.bit_depth) == frame.native_bit_depth()
                    && surface.source_width == frame.source_width
                    && surface.source_height == frame.source_height
                    && surface.output.width == frame.width
                    && surface.output.height == frame.height
            }
            _ => false,
        }
    }
}

pub struct GpuFrame {
    surface_id: u64,
    texture: Arc<wgpu::Texture>,
    view: Arc<wgpu::TextureView>,
    format: wgpu::TextureFormat,
    pub width: u32,
    pub height: u32,
}

impl Clone for GpuFrame {
    fn clone(&self) -> Self {
        Self {
            surface_id: self.surface_id,
            texture: Arc::clone(&self.texture),
            view: Arc::clone(&self.view),
            format: self.format,
            width: self.width,
            height: self.height,
        }
    }
}

impl GpuFrame {
    pub(crate) fn shares_surface(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.texture, &other.texture)
    }

    pub(crate) fn new(device: &wgpu::Device, width: u32, height: u32, label: &str) -> Self {
        Self::new_with_format(
            device,
            width,
            height,
            label,
            wgpu::TextureFormat::Rgba16Float,
        )
    }

    fn new_with_format(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        label: &str,
        format: wgpu::TextureFormat,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC
                | (if format == wgpu::TextureFormat::Rgba16Float {
                    wgpu::TextureUsages::RENDER_ATTACHMENT
                } else {
                    wgpu::TextureUsages::empty()
                }),
            view_formats: &[],
        });
        let texture = Arc::new(texture);
        let view = Arc::new(texture.create_view(&wgpu::TextureViewDescriptor::default()));
        Self {
            surface_id: NEXT_GPU_SURFACE_ID.fetch_add(1, Ordering::Relaxed),
            texture,
            view,
            format,
            width: width.max(1),
            height: height.max(1),
        }
    }

    pub(crate) fn view(&self) -> &wgpu::TextureView {
        self.view.as_ref()
    }
}

pub struct PresentationTexture {
    texture: wgpu::Texture,
    storage_view: wgpu::TextureView,
    pub width: u32,
    pub height: u32,
}

impl PresentationTexture {
    pub fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("kama monitor presentation"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
            view_formats: &[],
        });
        let storage_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            texture,
            storage_view,
            width: width.max(1),
            height: height.max(1),
        }
    }

    fn storage_view(&self) -> &wgpu::TextureView {
        &self.storage_view
    }

    pub fn external_view(&self) -> wgpu::TextureView {
        self.texture
            .create_view(&wgpu::TextureViewDescriptor::default())
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct VideoColorUniform {
    transfer: u32,
    bt2020_primaries: u32,
    has_alpha: u32,
    native_layout: u32,
    source_width: u32,
    source_height: u32,
    fit_width: u32,
    fit_height: u32,
    yuv_matrix: u32,
    full_range: u32,
    bit_depth: u32,
    _padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vec4Uniform {
    value: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct EffectRuntimeUniform {
    output_source_size: [u32; 4],
    times: [f32; 4],
    frame: [u32; 4],
}

#[derive(Clone, Copy, Debug)]
pub struct EffectEvalContext {
    pub timeline_time: f64,
    pub local_time: f64,
    pub frame_index: u64,
    pub frame_rate: f64,
}

impl EffectEvalContext {
    pub fn keyframe_time(self) -> f64 {
        self.timeline_time
    }

    pub(crate) fn value_context(self) -> ValueEvalContext {
        ValueEvalContext {
            timeline_time: self.timeline_time,
            local_time: self.local_time,
            frame_index: self.frame_index,
            frame_rate: self.frame_rate,
        }
    }
}

pub struct EffectInputs<'graph, 'values> {
    pub instance: Option<&'graph PipelineInstance>,
    pub plugins: &'graph PluginRegistry,
    pub context: EffectEvalContext,

    render_scale: f32,
    values: &'values RefCell<ValueEvaluator<'graph>>,
}

impl<'graph, 'values> EffectInputs<'graph, 'values> {
    pub fn new(
        instance: Option<&'graph PipelineInstance>,
        plugins: &'graph PluginRegistry,
        context: EffectEvalContext,
        values: &'values RefCell<ValueEvaluator<'graph>>,
        render_scale: f32,
    ) -> Self {
        Self {
            instance,
            plugins,
            context,
            render_scale: render_scale.max(0.000_001),
            values,
        }
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum BindGroupCacheKey {
    Generator {
        pipeline: usize,
        output: u64,
        params_chunk: usize,
        params_size: u64,
    },
    Source {
        pipeline: usize,
        output: u64,
        params_chunk: usize,
        params_size: u64,
        runtime_chunk: usize,
        runtime_size: u64,
    },
    Unary {
        pipeline: usize,
        input: u64,
        output: u64,
        params_chunk: usize,
        params_size: u64,
        runtime_chunk: usize,
        runtime_size: u64,
    },
    UnaryUniform {
        pipeline: usize,
        input: u64,
        output: u64,
        params_chunk: usize,
        params_size: u64,
    },
    Binary {
        pipeline: usize,
        first: u64,
        second: u64,
        output: u64,
        params_chunk: usize,
        params_size: u64,
        runtime_chunk: usize,
        runtime_size: u64,
    },
    BinaryUniform {
        pipeline: usize,
        first: u64,
        second: u64,
        output: u64,
        params_chunk: usize,
        params_size: u64,
    },
    Composite {
        destination: u64,
        source: u64,
        output: u64,
        params_chunk: usize,
        params_size: u64,
    },
}

pub struct VideoGpuRuntime {
    clear: wgpu::ComputePipeline,
    solid: wgpu::ComputePipeline,
    gaussian_blur: wgpu::ComputePipeline,
    bloom_combine: wgpu::ComputePipeline,
    upload_video: wgpu::ComputePipeline,
    upload_yuv_video: wgpu::ComputePipeline,
    composite: wgpu::ComputePipeline,
    present: wgpu::ComputePipeline,
    export_rgba16: wgpu::ComputePipeline,
    export_ayuv64: wgpu::ComputePipeline,
    export_yuva10: wgpu::ComputePipeline,
    export_nv12_buffer: wgpu::ComputePipeline,
    export_p010_buffer: wgpu::ComputePipeline,
    export_p210_buffer: wgpu::ComputePipeline,
    export_ayuv64_buffer: wgpu::ComputePipeline,
    export_yuva10_buffer: wgpu::ComputePipeline,
    programs: HashMap<u64, Arc<wgpu::ComputePipeline>>,
    generator_programs: HashMap<String, Arc<wgpu::ComputePipeline>>,
    shader_programs: HashMap<u64, Arc<wgpu::ComputePipeline>>,

    frame_pool: HashMap<(u32, u32), Vec<GpuFrame>>,
    frame_pool_len: usize,
    frame_pool_bytes: u64,
    uniform_uploads: UniformUploadArena,
    parameter_scratch: Vec<[f32; 4]>,
    bind_group_cache: HashMap<BindGroupCacheKey, Arc<wgpu::BindGroup>>,
}

pub struct GeneratorRenderArgs<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub encoder: &'a mut wgpu::CommandEncoder,
    pub generator: &'a GeneratorDefinition,
    pub parameters: &'a std::collections::BTreeMap<String, HostBinding>,
    pub time: f64,
    pub size: [u32; 2],
    pub render_scale: f32,
}

pub struct PresentationArgs<'a> {
    pub device: &'a wgpu::Device,
    pub encoder: &'a mut wgpu::CommandEncoder,
    pub input: &'a GpuFrame,
    pub output: &'a PresentationTexture,
}

pub struct EffectRenderArgs<'a, 'graph, 'values> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub encoder: &'a mut wgpu::CommandEncoder,
    pub effect: &'a EffectInputs<'graph, 'values>,
}

pub struct ExportPassArgs<'a> {
    pub device: &'a wgpu::Device,
    pub encoder: &'a mut wgpu::CommandEncoder,
    pub input: &'a GpuFrame,
}

pub struct CompositeArgs<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub encoder: &'a mut wgpu::CommandEncoder,
    pub destination: GpuFrame,
    pub source: GpuFrame,
    pub opacity: f32,
    pub mode: BlendMode,
    pub alpha_mode: AlphaBlendMode,
}

impl VideoGpuRuntime {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            clear: compute_pipeline(device, "kama clear", CLEAR_WGSL, ComputeLayout::ClearRgba32),
            solid: compute_pipeline(
                device,
                "kama solid fill",
                SOLID_WGSL,
                ComputeLayout::GeneratorRgba32WithParams,
            ),
            gaussian_blur: compute_pipeline(
                device,
                "kama gaussian blur",
                GAUSSIAN_BLUR_WGSL,
                ComputeLayout::UnaryRgba32WithUniform,
            ),
            bloom_combine: compute_pipeline(
                device,
                "kama bloom combine",
                BLOOM_COMBINE_WGSL,
                ComputeLayout::BinaryRgba32WithUniform,
            ),
            upload_video: video_upload_pipeline(
                device,
                "kama packed video upload",
                VIDEO_UPLOAD_HEADER_WGSL,
                VIDEO_UPLOAD_MAIN_WGSL,
                ComputeLayout::UploadRgba16,
            ),
            upload_yuv_video: video_upload_pipeline(
                device,
                "kama native YUV video upload",
                YUV_VIDEO_UPLOAD_HEADER_WGSL,
                YUV_VIDEO_UPLOAD_MAIN_WGSL,
                ComputeLayout::UploadYuv420,
            ),
            composite: compute_pipeline(
                device,
                "kama layer compositor",
                COMPOSITE_WGSL,
                ComputeLayout::CompositeRgba32,
            ),
            present: compute_pipeline(
                device,
                "kama monitor conversion",
                PRESENT_WGSL,
                ComputeLayout::PresentRgba16,
            ),
            export_rgba16: compute_pipeline(
                device,
                "kama export rgba16 conversion",
                &export_shader(EXPORT_RGBA16_WGSL, false),
                ComputeLayout::ExportRgba16,
            ),
            export_ayuv64: compute_pipeline(
                device,
                "kama export AYUV64 conversion",
                &export_shader(EXPORT_AYUV64_WGSL, true),
                ComputeLayout::ExportAyuv64,
            ),
            export_yuva10: compute_pipeline(
                device,
                "kama export YUVA444P10 conversion",
                &export_shader(EXPORT_YUVA10_WGSL, true),
                ComputeLayout::ExportYuva10,
            ),
            export_nv12_buffer: compute_pipeline(
                device,
                "kama export NV12 direct buffer conversion",
                &export_buffer_shader(EXPORT_NV12_BUFFER_WGSL, EXPORT_8BIT_YUV_ENCODER_WGSL),
                ComputeLayout::ExportNv12Buffer,
            ),
            export_p010_buffer: compute_pipeline(
                device,
                "kama export P010 direct buffer conversion",
                &export_buffer_shader(EXPORT_P010_BUFFER_WGSL, EXPORT_10BIT_YUV_ENCODER_WGSL),
                ComputeLayout::ExportP010Buffer,
            ),
            export_p210_buffer: compute_pipeline(
                device,
                "kama export P210 direct buffer conversion",
                &export_buffer_shader(EXPORT_P210_BUFFER_WGSL, EXPORT_10BIT_YUV_ENCODER_WGSL),
                ComputeLayout::ExportP210Buffer,
            ),
            export_ayuv64_buffer: compute_pipeline(
                device,
                "kama export AYUV64 direct buffer conversion",
                &export_buffer_shader(EXPORT_AYUV64_BUFFER_WGSL, ""),
                ComputeLayout::ExportAyuv64Buffer,
            ),
            export_yuva10_buffer: compute_pipeline(
                device,
                "kama export YUVA444P10 direct buffer conversion",
                &export_buffer_shader(EXPORT_YUVA10_BUFFER_WGSL, EXPORT_10BIT_YUVA_ENCODER_WGSL),
                ComputeLayout::ExportYuva10Buffer,
            ),
            programs: HashMap::new(),
            generator_programs: HashMap::new(),
            shader_programs: HashMap::new(),
            frame_pool: HashMap::new(),
            frame_pool_len: 0,
            frame_pool_bytes: 0,
            uniform_uploads: UniformUploadArena::new(device),
            parameter_scratch: Vec::new(),
            bind_group_cache: HashMap::new(),
        }
    }

    pub fn begin_submission(&mut self) {
        self.uniform_uploads.begin_submission();
    }

    fn upload_uniform(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        label: &str,
        bytes: &[u8],
    ) -> UniformAllocation {
        self.uniform_uploads.upload(device, queue, label, bytes)
    }

    fn take_frame(
        &mut self,
        device: &wgpu::Device,
        width: u32,
        height: u32,
        label: &str,
    ) -> GpuFrame {
        let width = width.max(1);
        let height = height.max(1);
        if let Some(bucket) = self.frame_pool.get_mut(&(width, height)) {
            if let Some(frame) = bucket.pop() {
                self.frame_pool_len = self.frame_pool_len.saturating_sub(1);
                self.frame_pool_bytes = self
                    .frame_pool_bytes
                    .saturating_sub(width as u64 * height as u64 * 8);
                return frame;
            }
        }
        GpuFrame::new(device, width, height, label)
    }

    pub(crate) fn recycle_frame(&mut self, frame: GpuFrame) {
        if frame.format != wgpu::TextureFormat::Rgba16Float {
            return;
        }

        let frame_bytes = frame.width as u64 * frame.height as u64 * 8;
        if Arc::strong_count(&frame.texture) == 1
            && self.frame_pool_len < GPU_FRAME_POOL_CAPACITY
            && self.frame_pool_bytes.saturating_add(frame_bytes) <= GPU_FRAME_POOL_MAX_BYTES
        {
            self.frame_pool
                .entry((frame.width, frame.height))
                .or_default()
                .push(frame);
            self.frame_pool_len += 1;
            self.frame_pool_bytes += frame_bytes;
        }
    }

    pub fn retain_working_size(&mut self, width: u32, height: u32) {
        let key = (width.max(1), height.max(1));
        if self.frame_pool.keys().any(|bucket_key| *bucket_key != key) {
            self.bind_group_cache.clear();
        }
        self.frame_pool.retain(|bucket_key, _| *bucket_key == key);
        self.frame_pool_len = self.frame_pool.values().map(Vec::len).sum();
        self.frame_pool_bytes = self
            .frame_pool
            .iter()
            .map(|(&(width, height), frames)| {
                width as u64 * height as u64 * 8 * frames.len() as u64
            })
            .sum();
    }

    pub fn prewarm(
        &mut self,
        device: &wgpu::Device,
        effects: &EffectRuntime,
        plugins: &PluginRegistry,
    ) {
        for pipeline in effects.compiled_pipelines() {
            for stage in &pipeline.stages {
                if matches!(
                    stage.fragment.execution,
                    NodeExecution::CpuWasm
                        | NodeExecution::GeneratorGpu
                        | NodeExecution::GeneratorCpu
                ) {
                    continue;
                }
                if let Err(error) = self.ensure_program(device, stage, plugins) {
                    messages::error("Effect Pipeline", format!("prewarm failed: {error:#}"));
                }
            }
        }
        for generator in plugins
            .generators()
            .filter(|generator| generator.backend == GeneratorBackend::Gpu)
        {
            if let Err(error) = self.ensure_generator_program(device, generator) {
                messages::error(
                    "GPU generator",
                    format!("{} prewarm failed: {error:#}", generator.key),
                );
            }
        }
    }

    pub fn transparent(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        width: u32,
        height: u32,
    ) -> GpuFrame {
        let output = self.take_frame(device, width, height, "kama transparent RGBA16F frame");
        let layout = self.clear.get_bind_group_layout(0);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kama clear bind group"),
            layout: &layout,
            entries: &[storage_entry(0, output.view())],
        });
        dispatch(
            encoder,
            &self.clear,
            &bind_group,
            output.width,
            output.height,
        );
        output
    }

    pub fn solid(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        width: u32,
        height: u32,
        color: [f32; 4],
    ) -> GpuFrame {
        let output = self.take_frame(device, width, height, "kama solid RGBA16F frame");
        let alpha = color[3].clamp(0.0, 1.0);
        let params = self.upload_uniform(
            device,
            queue,
            "kama solid fill parameters",
            bytemuck::cast_slice(&[[color[0] * alpha, color[1] * alpha, color[2] * alpha, alpha]]),
        );
        let layout = self.solid.get_bind_group_layout(0);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kama solid fill"),
            layout: &layout,
            entries: &[
                storage_entry(0, output.view()),
                dynamic_uniform_entry(1, &params),
            ],
        });
        dispatch_dynamic(
            encoder,
            &self.solid,
            &bind_group,
            &[dynamic_offset(&params)],
            output.width,
            output.height,
        );
        output
    }

    pub fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        frame: &CpuFrame,
    ) -> GpuFrame {
        let output = GpuFrame::new_with_format(
            device,
            frame.width,
            frame.height,
            "kama uploaded RGBA32F CPU source",
            wgpu::TextureFormat::Rgba32Float,
        );
        self.upload_into(queue, &output, frame);
        output
    }

    pub fn upload_into(&self, queue: &wgpu::Queue, output: &GpuFrame, frame: &CpuFrame) -> bool {
        if output.width != frame.width
            || output.height != frame.height
            || output.format != wgpu::TextureFormat::Rgba32Float
        {
            return false;
        }
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &output.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&frame.pixels),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(frame.width * 16),
                rows_per_image: Some(frame.height),
            },
            wgpu::Extent3d {
                width: frame.width,
                height: frame.height,
                depth_or_array_layers: 1,
            },
        );
        true
    }

    pub fn video_upload_surface(
        &self,
        device: &wgpu::Device,
        frame: &VideoFrame,
    ) -> VideoUploadSurface {
        match frame.native_layout() {
            Some(layout) => {
                let bit_depth = frame.native_bit_depth().unwrap_or(8);
                let chroma_width = if layout.packed_ayuv() {
                    1
                } else {
                    layout.chroma_width(frame.source_width.max(1))
                };
                let chroma_height = if layout.packed_ayuv() {
                    1
                } else {
                    layout.chroma_height(frame.source_height.max(1))
                };
                let planar_format = if bit_depth > 8 {
                    wgpu::TextureFormat::R16Unorm
                } else {
                    wgpu::TextureFormat::R8Unorm
                };
                let y = video_staging_texture(
                    device,
                    "kama native video Y plane",
                    frame.source_width,
                    frame.source_height,
                    if layout.packed_ayuv() {
                        if bit_depth > 8 {
                            wgpu::TextureFormat::Rgba16Unorm
                        } else {
                            wgpu::TextureFormat::Rgba8Unorm
                        }
                    } else {
                        planar_format
                    },
                );
                let u_or_uv = video_staging_texture(
                    device,
                    "kama native video U/UV plane",
                    chroma_width,
                    chroma_height,
                    if layout.interleaved_uv() {
                        if bit_depth > 8 {
                            wgpu::TextureFormat::Rg16Unorm
                        } else {
                            wgpu::TextureFormat::Rg8Unorm
                        }
                    } else {
                        planar_format
                    },
                );
                let v = video_staging_texture(
                    device,
                    "kama native video V plane",
                    chroma_width,
                    chroma_height,
                    planar_format,
                );
                let alpha = video_staging_texture(
                    device,
                    "kama native video alpha plane",
                    frame.source_width,
                    frame.source_height,
                    planar_format,
                );
                let output = GpuFrame::new(
                    device,
                    frame.width,
                    frame.height,
                    "kama native decoded video RGBA16F",
                );
                let color_uniform =
                    video_color_buffer(device, frame, "kama native video color metadata");
                let layout_desc = self.upload_yuv_video.get_bind_group_layout(0);
                let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("kama native YUV upload bind group"),
                    layout: &layout_desc,
                    entries: &[
                        texture_entry(0, &y.create_view(&wgpu::TextureViewDescriptor::default())),
                        texture_entry(
                            1,
                            &u_or_uv.create_view(&wgpu::TextureViewDescriptor::default()),
                        ),
                        texture_entry(2, &v.create_view(&wgpu::TextureViewDescriptor::default())),
                        texture_entry(
                            3,
                            &alpha.create_view(&wgpu::TextureViewDescriptor::default()),
                        ),
                        storage_entry(4, output.view()),
                        buffer_entry(5, &color_uniform),
                    ],
                });
                VideoUploadSurface {
                    inner: VideoUploadSurfaceInner::NativeYuv(Box::new(NativeVideoUploadSurface {
                        layout,
                        bit_depth,
                        source_width: frame.source_width,
                        source_height: frame.source_height,
                        y,
                        u_or_uv,
                        v,
                        alpha,
                        output,
                        color_uniform,
                        bind_group,
                    })),
                }
            }
            None => {
                let source = video_staging_texture(
                    device,
                    "kama packed RGBA16 video staging",
                    frame.source_width,
                    frame.source_height,
                    wgpu::TextureFormat::Rgba16Uint,
                );
                let output = GpuFrame::new(
                    device,
                    frame.width.max(1),
                    frame.height.max(1),
                    "kama decoded video RGBA16F",
                );
                let color_uniform =
                    video_color_buffer(device, frame, "kama decoded video color metadata");
                let layout = self.upload_video.get_bind_group_layout(0);
                let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("kama packed video upload bind group"),
                    layout: &layout,
                    entries: &[
                        texture_entry(
                            0,
                            &source.create_view(&wgpu::TextureViewDescriptor::default()),
                        ),
                        storage_entry(1, output.view()),
                        buffer_entry(2, &color_uniform),
                    ],
                });
                VideoUploadSurface {
                    inner: VideoUploadSurfaceInner::Packed(Box::new(PackedVideoUploadSurface {
                        source_width: frame.source_width,
                        source_height: frame.source_height,
                        source,
                        output,
                        color_uniform,
                        bind_group,
                    })),
                }
            }
        }
    }

    pub fn upload_video_into(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        surface: &VideoUploadSurface,
        frame: &VideoFrame,
    ) -> bool {
        if !surface.matches(frame) {
            return false;
        }
        match (&surface.inner, &frame.pixels) {
            (VideoUploadSurfaceInner::Packed(surface), VideoFramePixels::Rgba16(pixels)) => {
                queue.write_buffer(
                    &surface.color_uniform,
                    0,
                    bytemuck::bytes_of(&video_color_uniform(frame)),
                );
                queue.write_texture(
                    wgpu::ImageCopyTexture {
                        texture: &surface.source,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    pixels,
                    wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some(frame.source_width * 8),
                        rows_per_image: Some(frame.source_height),
                    },
                    wgpu::Extent3d {
                        width: frame.source_width,
                        height: frame.source_height,
                        depth_or_array_layers: 1,
                    },
                );
                dispatch(
                    encoder,
                    &self.upload_video,
                    &surface.bind_group,
                    frame.width,
                    frame.height,
                );
                true
            }
            (
                VideoUploadSurfaceInner::NativeYuv(surface),
                VideoFramePixels::NativeYuv {
                    layout,
                    bit_depth,
                    frame: av_frame,
                    has_alpha,
                },
            ) if *layout == surface.layout && *bit_depth == surface.bit_depth => {
                queue.write_buffer(
                    &surface.color_uniform,
                    0,
                    bytemuck::bytes_of(&video_color_uniform(frame)),
                );
                let bytes_per_sample = if *bit_depth > 8 { 2 } else { 1 };
                write_av_plane(
                    queue,
                    &surface.y,
                    av_frame,
                    0,
                    frame.source_width,
                    frame.source_height,
                    bytes_per_sample * if layout.packed_ayuv() { 4 } else { 1 },
                );
                if layout.packed_ayuv() {
                    dispatch(
                        encoder,
                        &self.upload_yuv_video,
                        &surface.bind_group,
                        frame.width,
                        frame.height,
                    );
                    return true;
                }
                let chroma_width = layout.chroma_width(frame.source_width);
                let chroma_height = layout.chroma_height(frame.source_height);
                write_av_plane(
                    queue,
                    &surface.u_or_uv,
                    av_frame,
                    1,
                    chroma_width,
                    chroma_height,
                    bytes_per_sample * if layout.interleaved_uv() { 2 } else { 1 },
                );
                if !layout.interleaved_uv() {
                    write_av_plane(
                        queue,
                        &surface.v,
                        av_frame,
                        2,
                        chroma_width,
                        chroma_height,
                        bytes_per_sample,
                    );
                }
                if *has_alpha {
                    write_av_plane(
                        queue,
                        &surface.alpha,
                        av_frame,
                        3,
                        frame.source_width,
                        frame.source_height,
                        bytes_per_sample,
                    );
                }
                dispatch(
                    encoder,
                    &self.upload_yuv_video,
                    &surface.bind_group,
                    frame.width,
                    frame.height,
                );
                true
            }
            _ => false,
        }
    }

    pub fn render_generator(&mut self, args: GeneratorRenderArgs<'_>) -> Result<GpuFrame> {
        let GeneratorRenderArgs {
            device,
            queue,
            encoder,
            generator,
            parameters,
            time,
            size,
            render_scale,
        } = args;
        if generator.backend != GeneratorBackend::Gpu {
            bail!("generator {} is not GPU-backed", generator.key);
        }
        self.ensure_generator_program(device, generator)?;
        let pipeline = self
            .generator_programs
            .get(&generator.key)
            .cloned()
            .context("GPU generator program disappeared after compilation")?;
        generator_parameters(
            &mut self.parameter_scratch,
            generator,
            parameters,
            time,
            render_scale,
        );
        let parameter_buffer = self.uniform_uploads.upload(
            device,
            queue,
            "kama GPU generator parameters",
            bytemuck::cast_slice(&self.parameter_scratch),
        );
        let output = self.take_frame(
            device,
            size[0],
            size[1],
            "kama GPU generator RGBA16F output",
        );
        let bind_group = cached_dispatch_bind_group(
            &mut self.bind_group_cache,
            device,
            &pipeline,
            true,
            BindGroupCacheKey::Generator {
                pipeline: pipeline_identity(&pipeline),
                output: output.surface_id,
                params_chunk: parameter_buffer.chunk_index,
                params_size: parameter_buffer.size.get(),
            },
            &[
                storage_entry(0, output.view()),
                dynamic_uniform_entry(1, &parameter_buffer),
            ],
            "kama GPU generator bind group",
        );
        dispatch_dynamic(
            encoder,
            &pipeline,
            &bind_group,
            &[dynamic_offset(&parameter_buffer)],
            output.width,
            output.height,
        );
        Ok(output)
    }

    pub fn apply_local_node(
        &mut self,
        args: EffectRenderArgs<'_, '_, '_>,
        input: GpuFrame,
        node: &EffectNode,
    ) -> GpuFrame {
        let EffectRenderArgs {
            device,
            queue,
            encoder,
            effect,
        } = args;
        let input_size = [input.width, input.height];
        let output_size = padding_output_size(
            node,
            effect,
            input_size,
            device.limits().max_texture_dimension_2d,
        )
        .unwrap_or(input_size);
        self.apply_local_node_sized(
            EffectRenderArgs {
                device,
                queue,
                encoder,
                effect,
            },
            input,
            node,
            output_size,
        )
    }

    pub fn apply_local_node_sized(
        &mut self,
        args: EffectRenderArgs<'_, '_, '_>,
        input: GpuFrame,
        node: &EffectNode,
        output_size: [u32; 2],
    ) -> GpuFrame {
        let EffectRenderArgs {
            device,
            queue,
            encoder,
            effect,
        } = args;
        let fake_stage = stage_for_single_node(node);
        self.apply_compiled_stage_sized(
            device,
            queue,
            encoder,
            input,
            (&fake_stage, &[node]),
            effect,
            output_size,
        )
    }

    pub fn apply_source_node(
        &mut self,
        args: EffectRenderArgs<'_, '_, '_>,
        width: u32,
        height: u32,
        node: &EffectNode,
    ) -> GpuFrame {
        let EffectRenderArgs {
            device,
            queue,
            encoder,
            effect,
        } = args;
        let stage = stage_for_single_node(node);
        if let Err(error) = self.ensure_program(device, &stage, effect.plugins) {
            messages::error("Effect Pipeline", format!("source node skipped: {error:#}"));
            return self.transparent(device, encoder, width, height);
        }
        let Some(pipeline) = self.programs.get(&stage.fragment.key).cloned() else {
            return self.transparent(device, encoder, width, height);
        };
        let (parameter_buffer, runtime_buffer) = effect_uniform_buffers(
            &mut self.uniform_uploads,
            &mut self.parameter_scratch,
            device,
            queue,
            &[node],
            effect,
            [width, height],
            [width, height],
        );
        let output = self.take_frame(device, width, height, "kama source effect output");
        dispatch_source(
            &mut self.bind_group_cache,
            device,
            encoder,
            &pipeline,
            &output,
            &parameter_buffer,
            &runtime_buffer,
            "kama source effect output",
        );
        output
    }

    pub fn apply_binary_node(
        &mut self,
        args: EffectRenderArgs<'_, '_, '_>,
        first: GpuFrame,
        second: GpuFrame,
        node: &EffectNode,
    ) -> GpuFrame {
        let EffectRenderArgs {
            device,
            queue,
            encoder,
            effect,
        } = args;
        let stage = stage_for_single_node(node);
        if let Err(error) = self.ensure_program(device, &stage, effect.plugins) {
            messages::error("Effect Pipeline", format!("binary node skipped: {error:#}"));
            self.recycle_frame(second);
            return first;
        }
        let Some(pipeline) = self.programs.get(&stage.fragment.key).cloned() else {
            self.recycle_frame(second);
            return first;
        };
        let size = [first.width, first.height];
        let (parameter_buffer, runtime_buffer) = effect_uniform_buffers(
            &mut self.uniform_uploads,
            &mut self.parameter_scratch,
            device,
            queue,
            &[node],
            effect,
            size,
            size,
        );
        let output = self.take_frame(
            device,
            first.width,
            first.height,
            "kama binary effect output",
        );
        dispatch_binary(
            &mut self.bind_group_cache,
            device,
            encoder,
            &pipeline,
            [&first, &second],
            &output,
            [&parameter_buffer, &runtime_buffer],
            "kama binary effect output",
        );
        self.recycle_frame(first);
        self.recycle_frame(second);
        output
    }

    pub fn apply_compiled_stage(
        &mut self,
        args: EffectRenderArgs<'_, '_, '_>,
        input: GpuFrame,
        stage: &CompiledStage,
        nodes: &[&EffectNode],
    ) -> GpuFrame {
        let EffectRenderArgs {
            device,
            queue,
            encoder,
            effect,
        } = args;
        let input_size = [input.width, input.height];
        let output_size = nodes
            .first()
            .filter(|_| nodes.len() == 1)
            .and_then(|node| {
                padding_output_size(
                    node,
                    effect,
                    input_size,
                    device.limits().max_texture_dimension_2d,
                )
            })
            .unwrap_or(input_size);
        self.apply_compiled_stage_sized(
            device,
            queue,
            encoder,
            input,
            (stage, nodes),
            effect,
            output_size,
        )
    }

    fn apply_compiled_stage_sized(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        input: GpuFrame,
        stage_nodes: (&CompiledStage, &[&EffectNode]),
        effect: &EffectInputs<'_, '_>,
        output_size: [u32; 2],
    ) -> GpuFrame {
        let (stage, nodes) = stage_nodes;
        if stage.fragment.execution == NodeExecution::CpuWasm {
            return input;
        }
        if output_size == [input.width, input.height] && nodes.len() == 1 {
            match nodes[0].node_type.as_str() {
                "builtin.blur" => {
                    return self.apply_builtin_gaussian_blur(
                        device, queue, encoder, input, nodes[0], effect,
                    );
                }
                "builtin.bloom" => {
                    return self
                        .apply_builtin_bloom(device, queue, encoder, input, nodes[0], effect);
                }
                _ => {}
            }
        }
        if let Err(error) = self.ensure_program(device, stage, effect.plugins) {
            messages::error("Effect Pipeline", format!("stage skipped: {error:#}"));
            return input;
        }
        let Some(pipeline) = self.programs.get(&stage.fragment.key).cloned() else {
            return input;
        };

        let (parameter_buffer, runtime_buffer) = effect_uniform_buffers(
            &mut self.uniform_uploads,
            &mut self.parameter_scratch,
            device,
            queue,
            nodes,
            effect,
            output_size,
            [input.width, input.height],
        );
        let output = self.take_frame(
            device,
            output_size[0],
            output_size[1],
            "kama Effect Pipeline output",
        );
        dispatch_unary(
            &mut self.bind_group_cache,
            device,
            encoder,
            &pipeline,
            &input,
            &output,
            [&parameter_buffer, &runtime_buffer],
            "kama Effect Pipeline output",
        );
        self.recycle_frame(input);
        output
    }

    fn gaussian_blur(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        input: &GpuFrame,
        radius: f32,
        threshold: Option<[f32; 2]>,
        label: &str,
    ) -> (GpuFrame, GpuFrame) {
        let horizontal = self.take_frame(device, input.width, input.height, &format!("{label} h"));
        let output = self.take_frame(device, input.width, input.height, &format!("{label} v"));
        let sigma = (radius / 3.0).max(0.35);
        for (source, target, axis, threshold) in [
            (input, &horizontal, [1.0, 0.0], threshold),
            (&horizontal, &output, [0.0, 1.0], None),
        ] {
            let params = gaussian_blur_params(
                &mut self.uniform_uploads,
                device,
                queue,
                axis,
                radius,
                sigma,
                threshold,
            );
            dispatch_unary_uniform(
                &mut self.bind_group_cache,
                device,
                encoder,
                &self.gaussian_blur,
                source,
                target,
                &params,
                label,
            );
        }
        (horizontal, output)
    }

    fn apply_builtin_gaussian_blur(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        input: GpuFrame,
        node: &EffectNode,
        effect: &EffectInputs<'_, '_>,
    ) -> GpuFrame {
        let enabled = node_value(node, effect, "enabled")
            .and_then(GpuValue::bool)
            .unwrap_or(true);
        let radius = node_value(node, effect, "radius")
            .and_then(GpuValue::f32)
            .unwrap_or(4.0)
            .clamp(0.0, 64.0);
        if !enabled || radius <= 0.001 {
            return input;
        }
        let (horizontal, output) = self.gaussian_blur(
            device,
            queue,
            encoder,
            &input,
            radius,
            None,
            "kama gaussian blur",
        );
        self.recycle_frame(input);
        self.recycle_frame(horizontal);
        output
    }

    fn apply_builtin_bloom(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        input: GpuFrame,
        node: &EffectNode,
        effect: &EffectInputs<'_, '_>,
    ) -> GpuFrame {
        let enabled = node_value(node, effect, "enabled")
            .and_then(GpuValue::bool)
            .unwrap_or(true);
        let intensity = node_value(node, effect, "intensity")
            .and_then(GpuValue::f32)
            .unwrap_or(0.5)
            .max(0.0);
        if !enabled || intensity <= 0.001 {
            return input;
        }
        let threshold = node_value(node, effect, "threshold")
            .and_then(GpuValue::f32)
            .unwrap_or(0.8)
            .max(0.0);
        let radius = node_value(node, effect, "radius")
            .and_then(GpuValue::f32)
            .unwrap_or(2.0)
            .clamp(0.0, 64.0);
        if radius <= 0.001 {
            return input;
        }

        let knee = (threshold * 0.2 + 0.04).max(0.08);
        let (horizontal, glow) = self.gaussian_blur(
            device,
            queue,
            encoder,
            &input,
            radius,
            Some([threshold, knee]),
            "kama bloom blur",
        );
        let output = self.take_frame(device, input.width, input.height, "kama bloom output");
        let combine_params = self.upload_uniform(
            device,
            queue,
            "kama bloom combine params",
            bytemuck::cast_slice(&[[intensity, 0.0, 0.0, 0.0]]),
        );
        dispatch_binary_uniform(
            &mut self.bind_group_cache,
            device,
            encoder,
            &self.bloom_combine,
            [&input, &glow],
            &output,
            &combine_params,
            "kama bloom combine",
        );
        self.recycle_frame(input);
        self.recycle_frame(horizontal);
        self.recycle_frame(glow);
        output
    }

    fn ensure_program(
        &mut self,
        device: &wgpu::Device,
        stage: &CompiledStage,
        plugins: &PluginRegistry,
    ) -> Result<()> {
        let key = stage.fragment.key;
        if self.programs.contains_key(&key) {
            return Ok(());
        }
        let disk_key = persistent_stage_shader_key(stage, plugins);
        let build_source = || -> Result<String> {
            match stage.fragment.execution {
                NodeExecution::PointwiseGpu => {
                    build_fused_pointwise_shader(&stage.fragment.node_types, plugins)
                }
                NodeExecution::SpatialGpu | NodeExecution::KernelGpu => {
                    let [node_type] = stage.fragment.node_types.as_slice() else {
                        bail!("standalone GPU stage must contain exactly one node");
                    };
                    build_standalone_shader(node_type, plugins)
                }
                NodeExecution::CpuWasm
                | NodeExecution::GeneratorGpu
                | NodeExecution::GeneratorCpu => {
                    bail!("non-GPU fragment reached GPU shader compiler")
                }
            }
        };
        let (source, cache_miss) = match clip_graph_cache::load_text("effect-wgsl", disk_key) {
            Some(cached) if cached_shader_matches_working_format(&cached) => (cached, false),
            Some(_) => (build_source()?, true),
            None => (build_source()?, true),
        };
        let image_inputs = stage
            .fragment
            .node_types
            .first()
            .and_then(|node_type| plugins.effect(node_type))
            .map_or(1, |effect| effect.image_inputs.len());
        let layout = match image_inputs {
            0 => ComputeLayout::SourceRgba32WithParamsAndRuntime,
            1 => ComputeLayout::UnaryRgba32WithParamsAndRuntime,
            2 => ComputeLayout::BinaryRgba32WithParamsAndRuntime,
            _ => unreachable!("validated plugin image input count"),
        };
        let pipeline = self.cached_pipeline(
            device,
            "kama compiled Effect Pipeline fragment",
            &source,
            layout,
        )?;
        self.programs.insert(key, pipeline);
        if cache_miss {
            clip_graph_cache::store_text_async("effect-wgsl", disk_key, source);
        }
        Ok(())
    }

    fn ensure_generator_program(
        &mut self,
        device: &wgpu::Device,
        generator: &GeneratorDefinition,
    ) -> Result<()> {
        if self.generator_programs.contains_key(&generator.key) {
            return Ok(());
        }
        let disk_key = persistent_generator_shader_key(generator);
        let (source, cache_miss) = match clip_graph_cache::load_text("generator-wgsl", disk_key) {
            Some(cached) if cached_shader_matches_working_format(&cached) => (cached, false),
            Some(_) => (build_generator_shader(generator)?, true),
            None => (build_generator_shader(generator)?, true),
        };
        let pipeline = self.cached_pipeline(
            device,
            "kama compiled GPU generator",
            &source,
            ComputeLayout::GeneratorRgba32WithParams,
        )?;
        self.generator_programs
            .insert(generator.key.clone(), pipeline);
        if cache_miss {
            clip_graph_cache::store_text_async("generator-wgsl", disk_key, source);
        }
        Ok(())
    }

    fn cached_pipeline(
        &mut self,
        device: &wgpu::Device,
        label: &str,
        source: &str,
        layout: ComputeLayout,
    ) -> Result<Arc<wgpu::ComputePipeline>> {
        let key = shader_program_key(source, layout);
        if let Some(pipeline) = self.shader_programs.get(&key) {
            return Ok(Arc::clone(pipeline));
        }
        let pipeline = Arc::new(try_compute_pipeline(device, label, source, layout)?);
        self.shader_programs.insert(key, Arc::clone(&pipeline));
        Ok(pipeline)
    }

    pub fn composite(&mut self, args: CompositeArgs<'_>) -> GpuFrame {
        let CompositeArgs {
            device,
            queue,
            encoder,
            destination,
            source,
            opacity,
            mode,
            alpha_mode,
        } = args;
        let opacity = opacity.clamp(0.0, 1.0);
        if opacity <= 0.0 {
            self.recycle_frame(source);
            return destination;
        }
        let output = self.take_frame(
            device,
            destination.width,
            destination.height,
            "kama composite output",
        );
        let uniform = Vec4Uniform {
            value: [
                opacity,
                blend_mode_index(mode) as f32,
                alpha_blend_mode_index(alpha_mode) as f32,
                0.0,
            ],
        };
        let params = self.upload_uniform(
            device,
            queue,
            "kama compositor parameters",
            bytemuck::bytes_of(&uniform),
        );
        let bind_group = cached_dispatch_bind_group(
            &mut self.bind_group_cache,
            device,
            &self.composite,
            true,
            BindGroupCacheKey::Composite {
                destination: destination.surface_id,
                source: source.surface_id,
                output: output.surface_id,
                params_chunk: params.chunk_index,
                params_size: params.size.get(),
            },
            &[
                texture_entry(0, destination.view()),
                texture_entry(1, source.view()),
                storage_entry(2, output.view()),
                dynamic_uniform_entry(3, &params),
            ],
            "kama compositor bind group",
        );
        dispatch_dynamic(
            encoder,
            &self.composite,
            &bind_group,
            &[dynamic_offset(&params)],
            output.width,
            output.height,
        );
        self.recycle_frame(destination);
        self.recycle_frame(source);
        output
    }

    pub fn export_rgba16_into(&self, args: ExportPassArgs<'_>, output: &wgpu::TextureView) {
        let ExportPassArgs {
            device,
            encoder,
            input,
        } = args;
        let layout = self.export_rgba16.get_bind_group_layout(0);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kama export rgba16 bind group"),
            layout: &layout,
            entries: &[texture_entry(0, input.view()), storage_entry(1, output)],
        });
        dispatch(
            encoder,
            &self.export_rgba16,
            &bind_group,
            input.width,
            input.height,
        );
    }

    pub fn export_ayuv64_into(&self, args: ExportPassArgs<'_>, output: &wgpu::TextureView) {
        let ExportPassArgs {
            device,
            encoder,
            input,
        } = args;
        let layout = self.export_ayuv64.get_bind_group_layout(0);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kama export AYUV64 bind group"),
            layout: &layout,
            entries: &[texture_entry(0, input.view()), storage_entry(1, output)],
        });
        dispatch(
            encoder,
            &self.export_ayuv64,
            &bind_group,
            input.width,
            input.height,
        );
    }

    pub fn export_yuva10_into(&self, args: ExportPassArgs<'_>, outputs: [&wgpu::TextureView; 4]) {
        let ExportPassArgs {
            device,
            encoder,
            input,
        } = args;
        let layout = self.export_yuva10.get_bind_group_layout(0);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kama export YUVA444P10 bind group"),
            layout: &layout,
            entries: &[
                texture_entry(0, input.view()),
                storage_entry(1, outputs[0]),
                storage_entry(2, outputs[1]),
                storage_entry(3, outputs[2]),
                storage_entry(4, outputs[3]),
            ],
        });
        dispatch(
            encoder,
            &self.export_yuva10,
            &bind_group,
            input.width,
            input.height,
        );
    }

    fn export_to_buffer(
        &self,
        args: ExportPassArgs<'_>,
        output: &wgpu::Buffer,
        pipeline: &wgpu::ComputePipeline,
        label: &str,
        size: [u32; 2],
    ) {
        let layout = pipeline.get_bind_group_layout(0);
        let bind_group = args.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &layout,
            entries: &[texture_entry(0, args.input.view()), buffer_entry(1, output)],
        });
        dispatch(args.encoder, pipeline, &bind_group, size[0], size[1]);
    }

    pub fn export_nv12_to_buffer(&self, args: ExportPassArgs<'_>, output: &wgpu::Buffer) {
        let size = [args.input.width.div_ceil(4), args.input.height.div_ceil(2)];
        self.export_to_buffer(
            args,
            output,
            &self.export_nv12_buffer,
            "kama export NV12 direct-buffer bind group",
            size,
        );
    }

    pub fn export_p010_to_buffer(&self, args: ExportPassArgs<'_>, output: &wgpu::Buffer) {
        let size = [args.input.width.div_ceil(2), args.input.height.div_ceil(2)];
        self.export_to_buffer(
            args,
            output,
            &self.export_p010_buffer,
            "kama export P010 direct-buffer bind group",
            size,
        );
    }

    pub fn export_p210_to_buffer(&self, args: ExportPassArgs<'_>, output: &wgpu::Buffer) {
        let size = [args.input.width.div_ceil(2), args.input.height];
        self.export_to_buffer(
            args,
            output,
            &self.export_p210_buffer,
            "kama export P210 direct-buffer bind group",
            size,
        );
    }

    pub fn export_ayuv64_to_buffer(&self, args: ExportPassArgs<'_>, output: &wgpu::Buffer) {
        let size = [args.input.width, args.input.height];
        self.export_to_buffer(
            args,
            output,
            &self.export_ayuv64_buffer,
            "kama export AYUV64 direct-buffer bind group",
            size,
        );
    }

    pub fn export_yuva10_to_buffer(&self, args: ExportPassArgs<'_>, output: &wgpu::Buffer) {
        let size = [args.input.width.div_ceil(2), args.input.height];
        self.export_to_buffer(
            args,
            output,
            &self.export_yuva10_buffer,
            "kama export YUVA444P10 direct-buffer bind group",
            size,
        );
    }

    pub fn present(&self, args: PresentationArgs<'_>) {
        let PresentationArgs {
            device,
            encoder,
            input,
            output,
        } = args;
        debug_assert_eq!((input.width, input.height), (output.width, output.height));
        let storage_view = output.storage_view();
        let layout = self.present.get_bind_group_layout(0);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kama monitor conversion bind group"),
            layout: &layout,
            entries: &[
                texture_entry(0, input.view()),
                storage_entry(1, storage_view),
            ],
        });
        dispatch(
            encoder,
            &self.present,
            &bind_group,
            input.width,
            input.height,
        );
    }
}

fn video_staging_texture(
    device: &wgpu::Device,
    label: &str,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

fn video_color_buffer(device: &wgpu::Device, frame: &VideoFrame, label: &str) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents: bytemuck::bytes_of(&video_color_uniform(frame)),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    })
}

fn video_color_uniform(frame: &VideoFrame) -> VideoColorUniform {
    VideoColorUniform {
        transfer: frame.transfer,
        bt2020_primaries: u32::from(frame.bt2020_primaries),
        has_alpha: u32::from(frame.has_alpha()),
        native_layout: match frame.native_layout() {
            None => 0,
            Some(NativeVideoLayout::Yuv420p) => 1,
            Some(NativeVideoLayout::Nv12) => 2,
            Some(NativeVideoLayout::Yuv422p) => 3,
            Some(NativeVideoLayout::Yuv444p) => 4,
            Some(NativeVideoLayout::Ayuv) => 5,
            Some(NativeVideoLayout::P010) => 6,
            Some(NativeVideoLayout::P210) => 7,
        },
        source_width: frame.source_width,
        source_height: frame.source_height,
        fit_width: frame.fit_width,
        fit_height: frame.fit_height,
        yuv_matrix: frame.yuv_matrix,
        full_range: u32::from(frame.full_range),
        bit_depth: frame.native_bit_depth().unwrap_or(16),
        _padding: 0,
    }
}

fn native_frame_byte_len(
    layout: NativeVideoLayout,
    frame: &AvVideoFrame,
    has_alpha: bool,
) -> usize {
    let planes = match layout {
        NativeVideoLayout::Ayuv => 1,
        NativeVideoLayout::Nv12 | NativeVideoLayout::P010 | NativeVideoLayout::P210 => 2,
        NativeVideoLayout::Yuv420p | NativeVideoLayout::Yuv422p | NativeVideoLayout::Yuv444p => {
            if has_alpha {
                4
            } else {
                3
            }
        }
    };
    (0..planes).map(|plane| frame.data(plane).len()).sum()
}

fn write_av_plane(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    frame: &AvVideoFrame,
    plane: usize,
    width: u32,
    height: u32,
    bytes_per_pixel: u32,
) {
    if width == 0 || height == 0 {
        return;
    }
    let stride = frame.stride(plane) as u32;
    debug_assert!(stride >= width.saturating_mul(bytes_per_pixel));
    queue.write_texture(
        wgpu::ImageCopyTexture {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        frame.data(plane),
        wgpu::ImageDataLayout {
            offset: 0,

            bytes_per_row: Some(stride),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
}

const CLIP_GRAPH_SHADER_CACHE_VERSION: &str = "kama-clip-graph-wgsl-v3-rgba16f";

fn cached_shader_matches_working_format(source: &str) -> bool {
    source.contains("texture_storage_2d<rgba16float")
        && !source.contains("texture_storage_2d<rgba32float")
}

fn persistent_stage_shader_key(stage: &CompiledStage, plugins: &PluginRegistry) -> u64 {
    let mut hasher = DefaultHasher::new();
    CLIP_GRAPH_SHADER_CACHE_VERSION.hash(&mut hasher);
    stage.fragment.key.hash(&mut hasher);
    stage.fragment.execution.hash(&mut hasher);
    for node_type in &stage.fragment.node_types {
        node_type.hash(&mut hasher);
        if let Some(definition) = plugins.effect(node_type) {
            definition.source.hash(&mut hasher);
            definition.entry.hash(&mut hasher);
            definition.uses.hash(&mut hasher);
            for image in &definition.image_inputs {
                image.id.hash(&mut hasher);
            }
            for input in &definition.inputs {
                input.id.hash(&mut hasher);
                (input.ty as u8).hash(&mut hasher);
                format!("{:?}", input.default).hash(&mut hasher);
                input.options.hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

fn persistent_generator_shader_key(generator: &GeneratorDefinition) -> u64 {
    let mut hasher = DefaultHasher::new();
    CLIP_GRAPH_SHADER_CACHE_VERSION.hash(&mut hasher);
    generator.key.hash(&mut hasher);
    generator.source.hash(&mut hasher);
    generator.entry.hash(&mut hasher);
    generator.uses_time.hash(&mut hasher);
    for input in &generator.inputs {
        input.id.hash(&mut hasher);
        (input.ty as u8).hash(&mut hasher);
        format!("{:?}", input.default).hash(&mut hasher);
        input.options.hash(&mut hasher);
    }
    hasher.finish()
}

fn stage_for_single_node(node: &EffectNode) -> CompiledStage {
    use crate::effects::CompiledFragment;
    use std::sync::Arc;
    let mut hasher = DefaultHasher::new();
    node.node_type.hash(&mut hasher);
    node.execution.hash(&mut hasher);
    let key = hasher.finish();
    CompiledStage {
        fragment: Arc::new(CompiledFragment {
            key,
            execution: node.execution,
            node_types: vec![node.node_type.clone()],
        }),
        node_ids: vec![node.id],
    }
}

fn effect_uniform_buffers(
    uploads: &mut UniformUploadArena,
    parameter_scratch: &mut Vec<[f32; 4]>,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    nodes: &[&EffectNode],
    effect: &EffectInputs<'_, '_>,
    output_size: [u32; 2],
    source_size: [u32; 2],
) -> (UniformAllocation, UniformAllocation) {
    let context = effect.context;
    effect_parameters(parameter_scratch, nodes, effect);
    let parameter_buffer = uploads.upload(
        device,
        queue,
        "kama effect parameters",
        bytemuck::cast_slice(parameter_scratch),
    );
    let runtime = EffectRuntimeUniform {
        output_source_size: [
            output_size[0],
            output_size[1],
            source_size[0],
            source_size[1],
        ],
        times: [
            context.timeline_time as f32,
            context.local_time as f32,
            0.0,
            0.0,
        ],
        frame: [
            context.frame_index as u32,
            (context.frame_index >> 32) as u32,
            0,
            0,
        ],
    };
    let runtime_buffer = uploads.upload(
        device,
        queue,
        "kama effect runtime context",
        bytemuck::bytes_of(&runtime),
    );
    (parameter_buffer, runtime_buffer)
}

fn effect_parameters(
    values: &mut Vec<[f32; 4]>,
    nodes: &[&EffectNode],
    inputs: &EffectInputs<'_, '_>,
) {
    values.clear();
    for node in nodes {
        let Some(effect) = inputs.plugins.effect(&node.node_type) else {
            continue;
        };
        values.push(pack_gpu_value(
            node_value(node, inputs, "enabled").unwrap_or(GpuValue::Bool(true)),
        ));
        for input in &effect.inputs {
            if let Some(value) = node_value(node, inputs, &input.id) {
                values.push(pack_gpu_value(value));
            } else if let Ok(value) = input.ty.default_gpu(&input.default) {
                values.push(pack_gpu_value(value));
            } else {
                values.push([0.0; 4]);
            }
        }
    }
    if values.is_empty() {
        values.push([0.0; 4]);
    }
}

fn generator_parameters(
    values: &mut Vec<[f32; 4]>,
    generator: &GeneratorDefinition,
    parameters: &std::collections::BTreeMap<String, HostBinding>,
    time: f64,
    render_scale: f32,
) {
    values.clear();
    for input in &generator.inputs {
        let resolved = parameters
            .get(&input.id)
            .and_then(|binding| binding.evaluate(time))
            .and_then(|value| match value {
                HostValue::Gpu(value) => Some(value),
                _ => None,
            })
            .or_else(|| input.ty.default_gpu(&input.default).ok());
        let resolved = resolved.map(|value| {
            if input.suffix.trim() == "px" {
                scale_pixel_value(value, render_scale)
            } else {
                value
            }
        });
        values.push(resolved.map(pack_gpu_value).unwrap_or([0.0; 4]));
    }
    if values.is_empty() {
        values.push([0.0; 4]);
    }
}

fn pack_gpu_value(value: GpuValue) -> [f32; 4] {
    match value {
        GpuValue::F32(value) => [value, 0.0, 0.0, 0.0],
        GpuValue::I32(value) => [value as f32, 0.0, 0.0, 0.0],
        GpuValue::U32(value) | GpuValue::Enum(value) => [value as f32, 0.0, 0.0, 0.0],
        GpuValue::Bool(value) => [if value { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0],
        GpuValue::Vec2(value) => [value[0], value[1], 0.0, 0.0],
        GpuValue::Vec3(value) => [value[0], value[1], value[2], 0.0],
        GpuValue::Vec4(value) | GpuValue::Color(value) => value,
    }
}

fn padding_output_size(
    node: &EffectNode,
    effect: &EffectInputs<'_, '_>,
    source: [u32; 2],
    max_dimension: u32,
) -> Option<[u32; 2]> {
    if node.node_type != "builtin.padding" {
        return None;
    }
    if node_value(node, effect, "enabled")
        .and_then(GpuValue::bool)
        .is_some_and(|enabled| !enabled)
    {
        return None;
    }
    let edges = node_value(node, effect, "edges")
        .and_then(GpuValue::enum_index)
        .unwrap_or(0);
    let thickness = node_value(node, effect, "thickness")
        .and_then(GpuValue::vec2)
        .unwrap_or([0.0, 0.0]);
    let horizontal = thickness[0].round().max(0.0) as u32;
    let vertical = thickness[1].round().max(0.0) as u32;
    let (left, top, right, bottom) = match edges {
        1 => (horizontal, 0, horizontal, 0),
        2 => (0, vertical, 0, vertical),
        3 => (horizontal, 0, 0, 0),
        4 => (0, 0, horizontal, 0),
        5 => (0, vertical, 0, 0),
        6 => (0, 0, 0, vertical),
        _ => (horizontal, vertical, horizontal, vertical),
    };
    Some([
        source[0]
            .saturating_add(left)
            .saturating_add(right)
            .clamp(1, max_dimension),
        source[1]
            .saturating_add(top)
            .saturating_add(bottom)
            .clamp(1, max_dimension),
    ])
}

fn node_value(node: &EffectNode, effect: &EffectInputs<'_, '_>, input: &str) -> Option<GpuValue> {
    let mut values = effect.values.borrow_mut();
    let value = resolved_node_input_cached(node, effect.instance, input, &mut values)?;
    let pixel_input = effect
        .plugins
        .effect(&node.node_type)
        .and_then(|definition| {
            definition
                .inputs
                .iter()
                .find(|candidate| candidate.id == input)
        })
        .is_some_and(|definition| definition.suffix.trim() == "px");
    if !pixel_input || effect.render_scale == 1.0 {
        return Some(value);
    }
    Some(scale_pixel_value(value, effect.render_scale))
}

fn scale_pixel_value(value: GpuValue, render_scale: f32) -> GpuValue {
    let scale = render_scale.max(0.000_001);
    match value {
        GpuValue::F32(value) => GpuValue::F32(value * scale),
        GpuValue::I32(value) => GpuValue::I32((value as f32 * scale).round() as i32),
        GpuValue::Vec2(value) => GpuValue::Vec2([value[0] * scale, value[1] * scale]),
        GpuValue::Vec3(value) => {
            GpuValue::Vec3([value[0] * scale, value[1] * scale, value[2] * scale])
        }
        GpuValue::Vec4(value) => GpuValue::Vec4([
            value[0] * scale,
            value[1] * scale,
            value[2] * scale,
            value[3] * scale,
        ]),
        other => other,
    }
}

fn cached_dispatch_bind_group(
    cache: &mut HashMap<BindGroupCacheKey, Arc<wgpu::BindGroup>>,
    device: &wgpu::Device,
    pipeline: &wgpu::ComputePipeline,
    cacheable: bool,
    key: BindGroupCacheKey,
    entries: &[wgpu::BindGroupEntry<'_>],
    label: &str,
) -> Arc<wgpu::BindGroup> {
    if !cacheable {
        let layout = pipeline.get_bind_group_layout(0);
        return Arc::new(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &layout,
            entries,
        }));
    }
    if let Some(group) = cache.get(&key) {
        return Arc::clone(group);
    }
    if cache.len() >= BIND_GROUP_CACHE_CAPACITY {
        cache.clear();
    }
    let layout = pipeline.get_bind_group_layout(0);
    let group = Arc::new(device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout: &layout,
        entries,
    }));
    cache.insert(key, Arc::clone(&group));
    group
}

fn dispatch_unary(
    cache: &mut HashMap<BindGroupCacheKey, Arc<wgpu::BindGroup>>,
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    pipeline: &wgpu::ComputePipeline,
    input: &GpuFrame,
    output: &GpuFrame,
    buffers: [&UniformAllocation; 2],
    label: &str,
) {
    let group = cached_dispatch_bind_group(
        cache,
        device,
        pipeline,
        input.format == wgpu::TextureFormat::Rgba16Float,
        BindGroupCacheKey::Unary {
            pipeline: pipeline_identity(pipeline),
            input: input.surface_id,
            output: output.surface_id,
            params_chunk: buffers[0].chunk_index,
            params_size: buffers[0].size.get(),
            runtime_chunk: buffers[1].chunk_index,
            runtime_size: buffers[1].size.get(),
        },
        &[
            texture_entry(0, input.view()),
            storage_entry(1, output.view()),
            dynamic_uniform_entry(2, buffers[0]),
            dynamic_uniform_entry(3, buffers[1]),
        ],
        label,
    );
    dispatch_dynamic(
        encoder,
        pipeline,
        &group,
        &[dynamic_offset(buffers[0]), dynamic_offset(buffers[1])],
        output.width,
        output.height,
    );
}

fn dispatch_unary_uniform(
    cache: &mut HashMap<BindGroupCacheKey, Arc<wgpu::BindGroup>>,
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    pipeline: &wgpu::ComputePipeline,
    input: &GpuFrame,
    output: &GpuFrame,
    params: &UniformAllocation,
    label: &str,
) {
    let group = cached_dispatch_bind_group(
        cache,
        device,
        pipeline,
        input.format == wgpu::TextureFormat::Rgba16Float,
        BindGroupCacheKey::UnaryUniform {
            pipeline: pipeline_identity(pipeline),
            input: input.surface_id,
            output: output.surface_id,
            params_chunk: params.chunk_index,
            params_size: params.size.get(),
        },
        &[
            texture_entry(0, input.view()),
            storage_entry(1, output.view()),
            dynamic_uniform_entry(2, params),
        ],
        label,
    );
    dispatch_dynamic(
        encoder,
        pipeline,
        &group,
        &[dynamic_offset(params)],
        output.width,
        output.height,
    );
}

fn dispatch_binary_uniform(
    cache: &mut HashMap<BindGroupCacheKey, Arc<wgpu::BindGroup>>,
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    pipeline: &wgpu::ComputePipeline,
    inputs: [&GpuFrame; 2],
    output: &GpuFrame,
    params: &UniformAllocation,
    label: &str,
) {
    let group = cached_dispatch_bind_group(
        cache,
        device,
        pipeline,
        inputs
            .iter()
            .all(|frame| frame.format == wgpu::TextureFormat::Rgba16Float),
        BindGroupCacheKey::BinaryUniform {
            pipeline: pipeline_identity(pipeline),
            first: inputs[0].surface_id,
            second: inputs[1].surface_id,
            output: output.surface_id,
            params_chunk: params.chunk_index,
            params_size: params.size.get(),
        },
        &[
            texture_entry(0, inputs[0].view()),
            texture_entry(1, inputs[1].view()),
            storage_entry(2, output.view()),
            dynamic_uniform_entry(3, params),
        ],
        label,
    );
    dispatch_dynamic(
        encoder,
        pipeline,
        &group,
        &[dynamic_offset(params)],
        output.width,
        output.height,
    );
}

fn dispatch_source(
    cache: &mut HashMap<BindGroupCacheKey, Arc<wgpu::BindGroup>>,
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    pipeline: &wgpu::ComputePipeline,
    output: &GpuFrame,
    params: &UniformAllocation,
    runtime: &UniformAllocation,
    label: &str,
) {
    let group = cached_dispatch_bind_group(
        cache,
        device,
        pipeline,
        true,
        BindGroupCacheKey::Source {
            pipeline: pipeline_identity(pipeline),
            output: output.surface_id,
            params_chunk: params.chunk_index,
            params_size: params.size.get(),
            runtime_chunk: runtime.chunk_index,
            runtime_size: runtime.size.get(),
        },
        &[
            storage_entry(0, output.view()),
            dynamic_uniform_entry(1, params),
            dynamic_uniform_entry(2, runtime),
        ],
        label,
    );
    dispatch_dynamic(
        encoder,
        pipeline,
        &group,
        &[dynamic_offset(params), dynamic_offset(runtime)],
        output.width,
        output.height,
    );
}

fn dispatch_binary(
    cache: &mut HashMap<BindGroupCacheKey, Arc<wgpu::BindGroup>>,
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    pipeline: &wgpu::ComputePipeline,
    inputs: [&GpuFrame; 2],
    output: &GpuFrame,
    buffers: [&UniformAllocation; 2],
    label: &str,
) {
    let group = cached_dispatch_bind_group(
        cache,
        device,
        pipeline,
        inputs
            .iter()
            .all(|frame| frame.format == wgpu::TextureFormat::Rgba16Float),
        BindGroupCacheKey::Binary {
            pipeline: pipeline_identity(pipeline),
            first: inputs[0].surface_id,
            second: inputs[1].surface_id,
            output: output.surface_id,
            params_chunk: buffers[0].chunk_index,
            params_size: buffers[0].size.get(),
            runtime_chunk: buffers[1].chunk_index,
            runtime_size: buffers[1].size.get(),
        },
        &[
            texture_entry(0, inputs[0].view()),
            texture_entry(1, inputs[1].view()),
            storage_entry(2, output.view()),
            dynamic_uniform_entry(3, buffers[0]),
            dynamic_uniform_entry(4, buffers[1]),
        ],
        label,
    );
    dispatch_dynamic(
        encoder,
        pipeline,
        &group,
        &[dynamic_offset(buffers[0]), dynamic_offset(buffers[1])],
        output.width,
        output.height,
    );
}

fn gaussian_blur_params(
    uploads: &mut UniformUploadArena,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    direction: [f32; 2],
    radius: f32,
    sigma: f32,
    prefilter: Option<[f32; 2]>,
) -> UniformAllocation {
    let [threshold, knee] = prefilter.unwrap_or([0.0, 0.0]);
    let data = [
        [direction[0], direction[1], radius, sigma],
        [
            if prefilter.is_some() { 1.0 } else { 0.0 },
            threshold,
            knee,
            0.0,
        ],
    ];
    uploads.upload(
        device,
        queue,
        "kama gaussian blur params",
        bytemuck::cast_slice(&data),
    )
}

fn dispatch_dynamic(
    encoder: &mut wgpu::CommandEncoder,
    pipeline: &wgpu::ComputePipeline,
    bind_group: &wgpu::BindGroup,
    dynamic_offsets: &[wgpu::DynamicOffset],
    width: u32,
    height: u32,
) {
    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
        label: None,
        timestamp_writes: None,
    });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, bind_group, dynamic_offsets);
    pass.dispatch_workgroups(width.div_ceil(8), height.div_ceil(8), 1);
}

fn dispatch(
    encoder: &mut wgpu::CommandEncoder,
    pipeline: &wgpu::ComputePipeline,
    bind_group: &wgpu::BindGroup,
    width: u32,
    height: u32,
) {
    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
        label: Some("kama video compute pass"),
        timestamp_writes: None,
    });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, bind_group, &[]);
    pass.dispatch_workgroups(width.div_ceil(8), height.div_ceil(8), 1);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum ComputeLayout {
    ClearRgba32,
    UploadRgba16,
    UploadYuv420,
    UnaryRgba32WithUniform,
    BinaryRgba32WithUniform,
    SourceRgba32WithParamsAndRuntime,
    UnaryRgba32WithParamsAndRuntime,
    BinaryRgba32WithParamsAndRuntime,
    GeneratorRgba32WithParams,
    CompositeRgba32,
    PresentRgba16,
    ExportRgba16,
    ExportAyuv64,
    ExportYuva10,
    ExportNv12Buffer,
    ExportP010Buffer,
    ExportP210Buffer,
    ExportAyuv64Buffer,
    ExportYuva10Buffer,
}

fn shader_program_key(source: &str, layout: ComputeLayout) -> u64 {
    let mut hasher = DefaultHasher::new();
    layout.hash(&mut hasher);
    source.hash(&mut hasher);
    hasher.finish()
}

fn video_upload_pipeline(
    device: &wgpu::Device,
    label: &str,
    header: &str,
    main: &str,
    layout: ComputeLayout,
) -> wgpu::ComputePipeline {
    let source = [
        VIDEO_COLOR_INFO_WGSL,
        header,
        VIDEO_COLOR_CONVERSION_WGSL,
        main,
    ]
    .concat();
    compute_pipeline(device, label, &source, layout)
}

fn try_compute_pipeline(
    device: &wgpu::Device,
    label: &str,
    source: &str,
    layout_kind: ComputeLayout,
) -> Result<wgpu::ComputePipeline> {
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let pipeline = compute_pipeline(device, label, source, layout_kind);
    if let Some(error) = pollster::block_on(device.pop_error_scope()) {
        bail!("{label} validation failed: {error}");
    }
    Ok(pipeline)
}

fn compute_pipeline(
    device: &wgpu::Device,
    label: &str,
    source: &str,
    layout_kind: ComputeLayout,
) -> wgpu::ComputePipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });

    let entries = match layout_kind {
        ComputeLayout::ClearRgba32 => vec![storage_texture_layout_entry(
            0,
            wgpu::TextureFormat::Rgba16Float,
        )],
        ComputeLayout::UploadRgba16 => vec![
            uint_texture_layout_entry(0),
            storage_texture_layout_entry(1, wgpu::TextureFormat::Rgba16Float),
            uniform_layout_entry(2),
        ],
        ComputeLayout::UploadYuv420 => vec![
            sampled_texture_layout_entry(0),
            sampled_texture_layout_entry(1),
            sampled_texture_layout_entry(2),
            sampled_texture_layout_entry(3),
            storage_texture_layout_entry(4, wgpu::TextureFormat::Rgba16Float),
            uniform_layout_entry(5),
        ],
        ComputeLayout::UnaryRgba32WithUniform => vec![
            sampled_texture_layout_entry(0),
            storage_texture_layout_entry(1, wgpu::TextureFormat::Rgba16Float),
            dynamic_uniform_layout_entry(2),
        ],
        ComputeLayout::BinaryRgba32WithUniform => vec![
            sampled_texture_layout_entry(0),
            sampled_texture_layout_entry(1),
            storage_texture_layout_entry(2, wgpu::TextureFormat::Rgba16Float),
            dynamic_uniform_layout_entry(3),
        ],
        ComputeLayout::SourceRgba32WithParamsAndRuntime => vec![
            storage_texture_layout_entry(0, wgpu::TextureFormat::Rgba16Float),
            dynamic_uniform_layout_entry(1),
            dynamic_uniform_layout_entry(2),
        ],
        ComputeLayout::UnaryRgba32WithParamsAndRuntime => vec![
            sampled_texture_layout_entry(0),
            storage_texture_layout_entry(1, wgpu::TextureFormat::Rgba16Float),
            dynamic_uniform_layout_entry(2),
            dynamic_uniform_layout_entry(3),
        ],
        ComputeLayout::BinaryRgba32WithParamsAndRuntime => vec![
            sampled_texture_layout_entry(0),
            sampled_texture_layout_entry(1),
            storage_texture_layout_entry(2, wgpu::TextureFormat::Rgba16Float),
            dynamic_uniform_layout_entry(3),
            dynamic_uniform_layout_entry(4),
        ],
        ComputeLayout::GeneratorRgba32WithParams => vec![
            storage_texture_layout_entry(0, wgpu::TextureFormat::Rgba16Float),
            dynamic_uniform_layout_entry(1),
        ],
        ComputeLayout::CompositeRgba32 => vec![
            sampled_texture_layout_entry(0),
            sampled_texture_layout_entry(1),
            storage_texture_layout_entry(2, wgpu::TextureFormat::Rgba16Float),
            dynamic_uniform_layout_entry(3),
        ],
        ComputeLayout::PresentRgba16 => vec![
            sampled_texture_layout_entry(0),
            storage_texture_layout_entry(1, wgpu::TextureFormat::Rgba16Float),
        ],
        ComputeLayout::ExportRgba16 | ComputeLayout::ExportAyuv64 => vec![
            sampled_texture_layout_entry(0),
            storage_texture_layout_entry(1, wgpu::TextureFormat::Rgba16Uint),
        ],
        ComputeLayout::ExportYuva10 => vec![
            sampled_texture_layout_entry(0),
            storage_texture_layout_entry(1, wgpu::TextureFormat::R16Uint),
            storage_texture_layout_entry(2, wgpu::TextureFormat::R16Uint),
            storage_texture_layout_entry(3, wgpu::TextureFormat::R16Uint),
            storage_texture_layout_entry(4, wgpu::TextureFormat::R16Uint),
        ],
        ComputeLayout::ExportNv12Buffer
        | ComputeLayout::ExportP010Buffer
        | ComputeLayout::ExportP210Buffer
        | ComputeLayout::ExportAyuv64Buffer
        | ComputeLayout::ExportYuva10Buffer => vec![
            sampled_texture_layout_entry(0),
            storage_buffer_layout_entry(1),
        ],
    };
    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &entries,
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[&bind_group_layout],
        push_constant_ranges: &[],
    });

    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: Some(&pipeline_layout),
        module: &module,
        entry_point: "main",
        compilation_options: wgpu::PipelineCompilationOptions::default(),
    })
}

fn sampled_texture_layout_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn uint_texture_layout_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Uint,
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn storage_texture_layout_entry(
    binding: u32,
    format: wgpu::TextureFormat,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format,
            view_dimension: wgpu::TextureViewDimension::D2,
        },
        count: None,
    }
}

fn storage_buffer_layout_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: false },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn dynamic_uniform_layout_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    let mut entry = uniform_layout_entry(binding);
    if let wgpu::BindingType::Buffer {
        has_dynamic_offset, ..
    } = &mut entry.ty
    {
        *has_dynamic_offset = true;
    }
    entry
}

fn uniform_layout_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn texture_entry(binding: u32, view: &wgpu::TextureView) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: wgpu::BindingResource::TextureView(view),
    }
}

fn storage_entry(binding: u32, view: &wgpu::TextureView) -> wgpu::BindGroupEntry<'_> {
    texture_entry(binding, view)
}

fn buffer_entry(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: buffer.as_entire_binding(),
    }
}

fn dynamic_uniform_entry(binding: u32, allocation: &UniformAllocation) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
            buffer: allocation.buffer.as_ref(),
            offset: 0,
            size: Some(allocation.size),
        }),
    }
}

fn dynamic_offset(allocation: &UniformAllocation) -> wgpu::DynamicOffset {
    u32::try_from(allocation.offset)
        .expect("uniform upload offset exceeds WebGPU dynamic offset range")
}

fn pipeline_identity(pipeline: &wgpu::ComputePipeline) -> usize {
    pipeline as *const wgpu::ComputePipeline as usize
}

fn blend_mode_index(mode: BlendMode) -> u32 {
    match mode {
        BlendMode::Normal => 0,
        BlendMode::Add => 1,
        BlendMode::Subtract => 2,
        BlendMode::Multiply => 3,
        BlendMode::Screen => 4,
        BlendMode::Overlay => 5,
        BlendMode::Difference => 6,
        BlendMode::Darken => 7,
        BlendMode::Lighten => 8,
        BlendMode::ColorDodge => 9,
        BlendMode::ColorBurn => 10,
        BlendMode::HardLight => 11,
        BlendMode::SoftLight => 12,
        BlendMode::Exclusion => 13,
        BlendMode::LinearBurn => 14,
        BlendMode::Divide => 15,
    }
}

fn alpha_blend_mode_index(mode: AlphaBlendMode) -> u32 {
    match mode {
        AlphaBlendMode::SourceOver => 0,
        AlphaBlendMode::PreserveDestination => 1,
        AlphaBlendMode::Replace => 2,
        AlphaBlendMode::Add => 3,
        AlphaBlendMode::Subtract => 4,
        AlphaBlendMode::Multiply => 5,
        AlphaBlendMode::Min => 6,
        AlphaBlendMode::Max => 7,
    }
}

fn export_shader(body: &str, yuv: bool) -> String {
    let prelude = r#"
fn linear_to_bt709(value: f32) -> f32 {
    let v = max(value, 0.0);
    return select(1.099 * pow(v, 0.45) - 0.099, v * 4.5, v < 0.018);
}
"#;
    let yuv = if yuv {
        r#"
fn bt709_yuv(rgb: vec3<f32>) -> vec3<f32> {
    let y = dot(rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    let cb = (rgb.b - y) / 1.8556;
    let cr = (rgb.r - y) / 1.5748;
    return vec3<f32>(y, cb, cr);
}

fn unpremul_bt709(premul: vec4<f32>) -> vec3<f32> {
    let alpha = clamp(premul.a, 0.0, 1.0);
    let inv_alpha = select(0.0, 1.0 / alpha, alpha > 0.000001);
    let straight = clamp(premul.rgb * inv_alpha, vec3<f32>(0.0), vec3<f32>(1.0));
    return vec3<f32>(
        clamp(linear_to_bt709(straight.r), 0.0, 1.0),
        clamp(linear_to_bt709(straight.g), 0.0, 1.0),
        clamp(linear_to_bt709(straight.b), 0.0, 1.0),
    );
}
"#
    } else {
        ""
    };
    format!("{prelude}{yuv}{body}")
}

fn export_buffer_shader(body: &str, encoder: &str) -> String {
    export_shader(&format!("{EXPORT_BUFFER_HEADER_WGSL}{encoder}{body}"), true)
}

const EXPORT_BUFFER_HEADER_WGSL: &str = r#"
@group(0) @binding(0) var input_texture: texture_2d<f32>;
struct OutputWords { words: array<u32>, }
@group(0) @binding(1) var<storage, read_write> output: OutputWords;
"#;

const EXPORT_8BIT_YUV_ENCODER_WGSL: &str = r#"
fn encoded_yuv(pixel: vec2<u32>) -> vec3<u32> {
    let dimensions = textureDimensions(input_texture);
    let clamped = min(pixel, dimensions - vec2<u32>(1u));
    let premul = textureLoad(input_texture, vec2<i32>(clamped), 0);
    let yuv = bt709_yuv(unpremul_bt709(premul));
    return vec3<u32>(round(clamp(
        vec3<f32>(16.0, 128.0, 128.0) + vec3<f32>(219.0, 224.0, 224.0) * yuv,
        vec3<f32>(0.0), vec3<f32>(255.0)
    )));
}
"#;

const EXPORT_10BIT_YUV_ENCODER_WGSL: &str = r#"
fn encoded_yuv(pixel: vec2<u32>) -> vec3<u32> {
    let dimensions = textureDimensions(input_texture);
    let clamped = min(pixel, dimensions - vec2<u32>(1u));
    let premul = textureLoad(input_texture, vec2<i32>(clamped), 0);
    let yuv = bt709_yuv(unpremul_bt709(premul));
    return vec3<u32>(round(clamp(
        vec3<f32>(64.0, 512.0, 512.0) + vec3<f32>(876.0, 896.0, 896.0) * yuv,
        vec3<f32>(0.0), vec3<f32>(1023.0)
    ))) << vec3<u32>(6u);
}
"#;

const EXPORT_10BIT_YUVA_ENCODER_WGSL: &str = r#"
fn encode_pixel(pixel: vec2<u32>) -> vec4<u32> {
    let premul = textureLoad(input_texture, vec2<i32>(pixel), 0);
    let yuv = bt709_yuv(unpremul_bt709(premul));
    let encoded = vec3<u32>(round(clamp(
        vec3<f32>(64.0, 512.0, 512.0) + vec3<f32>(876.0, 896.0, 896.0) * yuv,
        vec3<f32>(0.0), vec3<f32>(1023.0)
    )));
    return vec4<u32>(encoded, u32(round(clamp(premul.a, 0.0, 1.0) * 1023.0)));
}
"#;

const EXPORT_RGBA16_WGSL: &str = r#"
@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var output_texture: texture_storage_2d<rgba16uint, write>;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dimensions = textureDimensions(output_texture);
    if (gid.x >= dimensions.x || gid.y >= dimensions.y) { return; }
    let premul = textureLoad(input_texture, vec2<i32>(gid.xy), 0);
    let alpha = clamp(premul.a, 0.0, 1.0);
    let inv_alpha = select(0.0, 1.0 / alpha, alpha > 0.000001);
    let straight = premul.rgb * inv_alpha;
    let encoded = vec4<f32>(
        clamp(linear_to_bt709(straight.r), 0.0, 1.0),
        clamp(linear_to_bt709(straight.g), 0.0, 1.0),
        clamp(linear_to_bt709(straight.b), 0.0, 1.0),
        alpha,
    );
    textureStore(output_texture, vec2<i32>(gid.xy), vec4<u32>(round(encoded * 65535.0)));
}
"#;

const EXPORT_AYUV64_WGSL: &str = r#"
@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var output_texture: texture_storage_2d<rgba16uint, write>;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dimensions = textureDimensions(output_texture);
    if (gid.x >= dimensions.x || gid.y >= dimensions.y) { return; }
    let premul = textureLoad(input_texture, vec2<i32>(gid.xy), 0);
    let alpha = clamp(premul.a, 0.0, 1.0);
    let rgb = unpremul_bt709(premul);
    let yuv = bt709_yuv(rgb);
    let y = clamp((16.0 + 219.0 * yuv.x) * 256.0, 0.0, 65535.0);
    let u = clamp((128.0 + 224.0 * yuv.y) * 256.0, 0.0, 65535.0);
    let v = clamp((128.0 + 224.0 * yuv.z) * 256.0, 0.0, 65535.0);
    textureStore(output_texture, vec2<i32>(gid.xy), vec4<u32>(
        u32(round(alpha * 65535.0)), u32(round(y)), u32(round(u)), u32(round(v))
    ));
}
"#;

const EXPORT_YUVA10_WGSL: &str = r#"
@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var y_texture: texture_storage_2d<r16uint, write>;
@group(0) @binding(2) var u_texture: texture_storage_2d<r16uint, write>;
@group(0) @binding(3) var v_texture: texture_storage_2d<r16uint, write>;
@group(0) @binding(4) var a_texture: texture_storage_2d<r16uint, write>;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dimensions = textureDimensions(y_texture);
    if (gid.x >= dimensions.x || gid.y >= dimensions.y) { return; }
    let premul = textureLoad(input_texture, vec2<i32>(gid.xy), 0);
    let alpha = clamp(premul.a, 0.0, 1.0);
    let rgb = unpremul_bt709(premul);
    let yuv = bt709_yuv(rgb);
    let y = u32(round(clamp(64.0 + 876.0 * yuv.x, 0.0, 1023.0)));
    let u = u32(round(clamp(512.0 + 896.0 * yuv.y, 0.0, 1023.0)));
    let v = u32(round(clamp(512.0 + 896.0 * yuv.z, 0.0, 1023.0)));
    let a = u32(round(alpha * 1023.0));
    textureStore(y_texture, vec2<i32>(gid.xy), vec4<u32>(y, 0u, 0u, 0u));
    textureStore(u_texture, vec2<i32>(gid.xy), vec4<u32>(u, 0u, 0u, 0u));
    textureStore(v_texture, vec2<i32>(gid.xy), vec4<u32>(v, 0u, 0u, 0u));
    textureStore(a_texture, vec2<i32>(gid.xy), vec4<u32>(a, 0u, 0u, 0u));
}
"#;

const EXPORT_NV12_BUFFER_WGSL: &str = r#"
fn pack4(a: u32, b: u32, c: u32, d: u32) -> u32 {
    return a | (b << 8u) | (c << 16u) | (d << 24u);
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dimensions = textureDimensions(input_texture);
    let word_width = dimensions.x / 4u;
    let chroma_height = dimensions.y / 2u;
    if (gid.x >= word_width || gid.y >= chroma_height) { return; }

    let x = gid.x * 4u;
    let y0 = gid.y * 2u;
    let y1 = y0 + 1u;
    let a0 = encoded_yuv(vec2<u32>(x + 0u, y0));
    let a1 = encoded_yuv(vec2<u32>(x + 1u, y0));
    let a2 = encoded_yuv(vec2<u32>(x + 2u, y0));
    let a3 = encoded_yuv(vec2<u32>(x + 3u, y0));
    let b0 = encoded_yuv(vec2<u32>(x + 0u, y1));
    let b1 = encoded_yuv(vec2<u32>(x + 1u, y1));
    let b2 = encoded_yuv(vec2<u32>(x + 2u, y1));
    let b3 = encoded_yuv(vec2<u32>(x + 3u, y1));

    output.words[y0 * word_width + gid.x] = pack4(a0.x, a1.x, a2.x, a3.x);
    output.words[y1 * word_width + gid.x] = pack4(b0.x, b1.x, b2.x, b3.x);

    let u0 = (a0.y + a1.y + b0.y + b1.y + 2u) / 4u;
    let v0 = (a0.z + a1.z + b0.z + b1.z + 2u) / 4u;
    let u1 = (a2.y + a3.y + b2.y + b3.y + 2u) / 4u;
    let v1 = (a2.z + a3.z + b2.z + b3.z + 2u) / 4u;
    let y_words = word_width * dimensions.y;
    output.words[y_words + gid.y * word_width + gid.x] = pack4(u0, v0, u1, v1);
}
"#;

const EXPORT_P010_BUFFER_WGSL: &str = r#"
@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dimensions = textureDimensions(input_texture);
    let pair_width = dimensions.x / 2u;
    let pair_height = dimensions.y / 2u;
    if (gid.x >= pair_width || gid.y >= pair_height) { return; }

    let x0 = gid.x * 2u;
    let y0 = gid.y * 2u;
    let p00 = encoded_yuv(vec2<u32>(x0, y0));
    let p10 = encoded_yuv(vec2<u32>(x0 + 1u, y0));
    let p01 = encoded_yuv(vec2<u32>(x0, y0 + 1u));
    let p11 = encoded_yuv(vec2<u32>(x0 + 1u, y0 + 1u));
    let row_words = pair_width;
    output.words[y0 * row_words + gid.x] = p00.x | (p10.x << 16u);
    output.words[(y0 + 1u) * row_words + gid.x] = p01.x | (p11.x << 16u);

    let u = (((p00.y >> 6u) + (p10.y >> 6u) + (p01.y >> 6u) + (p11.y >> 6u) + 2u) / 4u) << 6u;
    let v = (((p00.z >> 6u) + (p10.z >> 6u) + (p01.z >> 6u) + (p11.z >> 6u) + 2u) / 4u) << 6u;
    let y_words = row_words * dimensions.y;
    output.words[y_words + gid.y * row_words + gid.x] = u | (v << 16u);
}
"#;

const EXPORT_P210_BUFFER_WGSL: &str = r#"
@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dimensions = textureDimensions(input_texture);
    let pair_width = (dimensions.x + 1u) / 2u;
    if (gid.x >= pair_width || gid.y >= dimensions.y) { return; }

    let x0 = gid.x * 2u;
    let x1 = min(x0 + 1u, dimensions.x - 1u);
    let yuv0 = encoded_yuv(vec2<u32>(x0, gid.y));
    let yuv1 = encoded_yuv(vec2<u32>(x1, gid.y));
    let pair_index = gid.y * pair_width + gid.x;
    let y_words = pair_width * dimensions.y;

    output.words[pair_index] = yuv0.x | (yuv1.x << 16u);
    let u = ((yuv0.y >> 6u) + (yuv1.y >> 6u) + 1u) / 2u << 6u;
    let v = ((yuv0.z >> 6u) + (yuv1.z >> 6u) + 1u) / 2u << 6u;
    output.words[y_words + pair_index] = u | (v << 16u);
}
"#;
const EXPORT_AYUV64_BUFFER_WGSL: &str = r#"
@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dimensions = textureDimensions(input_texture);
    if (gid.x >= dimensions.x || gid.y >= dimensions.y) { return; }
    let premul = textureLoad(input_texture, vec2<i32>(gid.xy), 0);
    let alpha = clamp(premul.a, 0.0, 1.0);
    let rgb = unpremul_bt709(premul);
    let yuv = bt709_yuv(rgb);
    let a = u32(round(alpha * 65535.0));
    let y = u32(round(clamp((16.0 + 219.0 * yuv.x) * 256.0, 0.0, 65535.0)));
    let u = u32(round(clamp((128.0 + 224.0 * yuv.y) * 256.0, 0.0, 65535.0)));
    let v = u32(round(clamp((128.0 + 224.0 * yuv.z) * 256.0, 0.0, 65535.0)));
    let pixel = gid.y * dimensions.x + gid.x;
    output.words[pixel * 2u] = a | (y << 16u);
    output.words[pixel * 2u + 1u] = u | (v << 16u);
}
"#;

const EXPORT_YUVA10_BUFFER_WGSL: &str = r#"
@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dimensions = textureDimensions(input_texture);
    let pair_width = dimensions.x / 2u;
    if (gid.x >= pair_width || gid.y >= dimensions.y) { return; }
    let x = gid.x * 2u;
    let first = encode_pixel(vec2<u32>(x, gid.y));
    let second = encode_pixel(vec2<u32>(x + 1u, gid.y));
    let pair_index = gid.y * pair_width + gid.x;
    let plane_words = (dimensions.x * dimensions.y) / 2u;
    output.words[pair_index] = first.x | (second.x << 16u);
    output.words[plane_words + pair_index] = first.y | (second.y << 16u);
    output.words[plane_words * 2u + pair_index] = first.z | (second.z << 16u);
    output.words[plane_words * 3u + pair_index] = first.w | (second.w << 16u);
}
"#;

#[cfg(test)]
#[test]
fn export_shaders_are_valid_wgsl() {
    let shaders = [
        export_shader(EXPORT_RGBA16_WGSL, false),
        export_shader(EXPORT_AYUV64_WGSL, true),
        export_shader(EXPORT_YUVA10_WGSL, true),
        export_buffer_shader(EXPORT_NV12_BUFFER_WGSL, EXPORT_8BIT_YUV_ENCODER_WGSL),
        export_buffer_shader(EXPORT_P010_BUFFER_WGSL, EXPORT_10BIT_YUV_ENCODER_WGSL),
        export_buffer_shader(EXPORT_P210_BUFFER_WGSL, EXPORT_10BIT_YUV_ENCODER_WGSL),
        export_buffer_shader(EXPORT_AYUV64_BUFFER_WGSL, ""),
        export_buffer_shader(EXPORT_YUVA10_BUFFER_WGSL, EXPORT_10BIT_YUVA_ENCODER_WGSL),
    ];
    shaders.iter().for_each(|shader| {
        naga::front::wgsl::parse_str(shader).expect("export shader should parse");
    });
}

const VIDEO_COLOR_INFO_WGSL: &str = r#"
struct ColorInfo {
    transfer: u32,
    bt2020_primaries: u32,
    has_alpha: u32,
    native_layout: u32,
    source_width: u32,
    source_height: u32,
    fit_width: u32,
    fit_height: u32,
    yuv_matrix: u32,
    full_range: u32,
    bit_depth: u32,
    _padding: u32,
}
"#;

const VIDEO_COLOR_CONVERSION_WGSL: &str = r#"fn decode_channel(value: f32, transfer: u32) -> f32 {
    if (transfer == 0u) { return value; }
    if (transfer == 1u) {
        return select(value / 12.92, pow((value + 0.055) / 1.055, 2.4), value > 0.04045);
    }
    if (transfer == 2u) { return pow(max(value, 0.0), 2.2); }
    if (transfer == 3u) { return pow(max(value, 0.0), 2.8); }
    if (transfer == 4u) {
        let m1 = 2610.0 / 16384.0;
        let m2 = 2523.0 / 32.0;
        let c1 = 3424.0 / 4096.0;
        let c2 = 2413.0 / 128.0;
        let c3 = 2392.0 / 128.0;
        let p = pow(max(value, 0.0), 1.0 / m2);
        let nits = pow(max(p - c1, 0.0) / max(c2 - c3 * p, 0.000001), 1.0 / m1) * 10000.0;
        return nits / 100.0;
    }
    if (transfer == 5u) {
        let a = 0.17883277;
        let b = 0.28466892;
        let c = 0.5599107;
        return select((value * value) / 3.0, (exp((value - c) / a) + b) / 12.0, value > 0.5);
    }
    return select(value / 4.5, pow((value + 0.099) / 1.099, 1.0 / 0.45), value >= 0.081);
}

fn to_working_rgb(encoded: vec3<f32>) -> vec3<f32> {
    var linear = vec3<f32>(
        decode_channel(encoded.r, color_info.transfer),
        decode_channel(encoded.g, color_info.transfer),
        decode_channel(encoded.b, color_info.transfer),
    );
    if (color_info.bt2020_primaries != 0u) {
        let c = linear;
        linear = vec3<f32>(
            1.660491 * c.r - 0.587641 * c.g - 0.072850 * c.b,
            -0.124550 * c.r + 1.132900 * c.g - 0.008349 * c.b,
            -0.018151 * c.r - 0.100579 * c.g + 1.118730 * c.b,
        );
    }
    return linear;
}

"#;

const YUV_VIDEO_UPLOAD_HEADER_WGSL: &str = r#"
@group(0) @binding(0) var y_texture: texture_2d<f32>;
@group(0) @binding(1) var u_or_uv_texture: texture_2d<f32>;
@group(0) @binding(2) var v_texture: texture_2d<f32>;
@group(0) @binding(3) var alpha_texture: texture_2d<f32>;
@group(0) @binding(4) var output_texture: texture_storage_2d<rgba16float, write>;
@group(0) @binding(5) var<uniform> color_info: ColorInfo;

"#;

const YUV_VIDEO_UPLOAD_MAIN_WGSL: &str = r#"fn native_code(raw: f32) -> f32 {
    if (color_info.bit_depth <= 8u) { return raw; }
    let max_code = f32((1u << color_info.bit_depth) - 1u);
    if (color_info.native_layout == 6u || color_info.native_layout == 7u) {
        let shift = 16u - color_info.bit_depth;
        return raw * (65535.0 / (max_code * f32(1u << shift)));
    }
    return raw * (65535.0 / max_code);
}

fn yuv_to_rgb(y_code: f32, u_code: f32, v_code: f32) -> vec3<f32> {
    let bit_depth = max(color_info.bit_depth, 8u);
    let max_code = f32((1u << bit_depth) - 1u);
    let center_code = f32(1u << (bit_depth - 1u));
    var y = y_code;
    var cb = u_code - center_code / max_code;
    var cr = v_code - center_code / max_code;
    if (color_info.full_range == 0u) {
        let scale = f32(1u << (bit_depth - 8u));
        y = (y_code * max_code - 16.0 * scale) / (219.0 * scale);
        cb = (u_code * max_code - 128.0 * scale) / (224.0 * scale);
        cr = (v_code * max_code - 128.0 * scale) / (224.0 * scale);
    }
    if (color_info.yuv_matrix == 2u) {
        return vec3<f32>(
            y + 1.4746 * cr,
            y - 0.164553 * cb - 0.571353 * cr,
            y + 1.8814 * cb,
        );
    }
    if (color_info.yuv_matrix == 0u) {
        return vec3<f32>(
            y + 1.4020 * cr,
            y - 0.344136 * cb - 0.714136 * cr,
            y + 1.7720 * cb,
        );
    }
    return vec3<f32>(
        y + 1.5748 * cr,
        y - 0.187324 * cb - 0.468124 * cr,
        y + 1.8556 * cb,
    );
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let out_dim = textureDimensions(output_texture);
    if (gid.x >= out_dim.x || gid.y >= out_dim.y) { return; }

    let offset_x = (out_dim.x - color_info.fit_width) / 2u;
    let offset_y = (out_dim.y - color_info.fit_height) / 2u;
    if (gid.x < offset_x || gid.y < offset_y
        || gid.x >= offset_x + color_info.fit_width
        || gid.y >= offset_y + color_info.fit_height) {
        textureStore(output_texture, vec2<i32>(gid.xy), vec4<f32>(0.0));
        return;
    }

    let local = gid.xy - vec2<u32>(offset_x, offset_y);
    let source = vec2<u32>(
        min((local.x * color_info.source_width) / max(color_info.fit_width, 1u), color_info.source_width - 1u),
        min((local.y * color_info.source_height) / max(color_info.fit_height, 1u), color_info.source_height - 1u),
    );
    var chroma = source / 2u;
    if (color_info.native_layout == 3u || color_info.native_layout == 7u) {
        chroma = vec2<u32>(source.x / 2u, source.y);
    } else if (color_info.native_layout == 4u) {
        chroma = source;
    }

    var y: f32;
    var u: f32;
    var v: f32;
    var alpha = 1.0;
    if (color_info.native_layout == 5u) {
        let ayuv = textureLoad(y_texture, vec2<i32>(source), 0);
        alpha = clamp(native_code(ayuv.r), 0.0, 1.0);
        y = native_code(ayuv.g);
        u = native_code(ayuv.b);
        v = native_code(ayuv.a);
    } else {
        y = native_code(textureLoad(y_texture, vec2<i32>(source), 0).r);
        if (color_info.native_layout == 2u || color_info.native_layout == 6u || color_info.native_layout == 7u) {
            let uv = textureLoad(u_or_uv_texture, vec2<i32>(chroma), 0).rg;
            u = native_code(uv.r);
            v = native_code(uv.g);
        } else {
            u = native_code(textureLoad(u_or_uv_texture, vec2<i32>(chroma), 0).r);
            v = native_code(textureLoad(v_texture, vec2<i32>(chroma), 0).r);
        }
        if (color_info.has_alpha != 0u) {
            alpha = clamp(native_code(textureLoad(alpha_texture, vec2<i32>(source), 0).r), 0.0, 1.0);
        }
    }

    let encoded = clamp(yuv_to_rgb(y, u, v), vec3<f32>(0.0), vec3<f32>(1.0));
    let linear = to_working_rgb(encoded);
    textureStore(output_texture, vec2<i32>(gid.xy), vec4<f32>(linear * alpha, alpha));
}
"#;

const VIDEO_UPLOAD_HEADER_WGSL: &str = r#"
@group(0) @binding(0) var source_texture: texture_2d<u32>;
@group(0) @binding(1) var output_texture: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var<uniform> color_info: ColorInfo;

"#;

const VIDEO_UPLOAD_MAIN_WGSL: &str = r#"@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let out_dim = textureDimensions(output_texture);
    if (gid.x >= out_dim.x || gid.y >= out_dim.y) { return; }

    let offset = (out_dim - vec2<u32>(color_info.fit_width, color_info.fit_height)) / 2u;
    if (any(gid.xy < offset) || any(gid.xy >= offset + vec2<u32>(color_info.fit_width, color_info.fit_height))) {
        textureStore(output_texture, vec2<i32>(gid.xy), vec4<f32>(0.0));
        return;
    }

    let local = gid.xy - offset;
    let source_dimensions = vec2<u32>(color_info.source_width, color_info.source_height);
    let fit_dimensions = vec2<u32>(
        max(color_info.fit_width, 1u),
        max(color_info.fit_height, 1u),
    );
    let source = min(
        (local * source_dimensions) / fit_dimensions,
        source_dimensions - vec2<u32>(1u),
    );
    let encoded = vec4<f32>(textureLoad(source_texture, vec2<i32>(source), 0)) / 65535.0;
    let alpha = encoded.a;
    let linear = to_working_rgb(encoded.rgb);
    textureStore(output_texture, vec2<i32>(gid.xy), vec4<f32>(linear * alpha, alpha));
}
"#;

const GAUSSIAN_BLUR_WGSL: &str = r#"
@group(0) @binding(0) var source_tex: texture_2d<f32>;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba16float, write>;

struct BlurParams {
    direction_radius: vec4<f32>,
    prefilter: vec4<f32>,
}

@group(0) @binding(2) var<uniform> params: BlurParams;

fn bloom_prefilter(color: vec4<f32>) -> vec4<f32> {
    if params.prefilter.x < 0.5 {
        return color;
    }
    let threshold = params.prefilter.y;
    let knee = max(params.prefilter.z, 0.0001);
    let luma = dot(color.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    return color * smoothstep(threshold - knee, threshold + knee, luma);
}

fn load_clamped(pixel: vec2<i32>, size: vec2<i32>) -> vec4<f32> {
    return bloom_prefilter(textureLoad(source_tex, clamp(pixel, vec2<i32>(0), size - vec2<i32>(1)), 0));
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let size_u = textureDimensions(output_tex);
    if gid.x >= size_u.x || gid.y >= size_u.y {
        return;
    }
    let size = vec2<i32>(size_u);
    let pixel = vec2<i32>(gid.xy);
    let direction = vec2<i32>(round(params.direction_radius.xy));
    let radius = clamp(params.direction_radius.z, 0.0, 64.0);
    let sigma = max(params.direction_radius.w, 0.05);

    var sum = load_clamped(pixel, size);
    var weight_sum = 1.0;
    for (var tap: i32 = 1; tap <= 64; tap = tap + 1) {
        let distance = f32(tap);
        let coverage = clamp(radius + 1.0 - distance, 0.0, 1.0);
        if coverage <= 0.0 {
            continue;
        }
        let gaussian = exp(-0.5 * (distance * distance) / (sigma * sigma));
        let weight = gaussian * coverage;
        let offset = direction * tap;
        sum += load_clamped(pixel + offset, size) * weight;
        sum += load_clamped(pixel - offset, size) * weight;
        weight_sum += 2.0 * weight;
    }
    textureStore(output_tex, pixel, sum / max(weight_sum, 0.000001));
}
"#;

const BLOOM_COMBINE_WGSL: &str = r#"
@group(0) @binding(0) var source_tex: texture_2d<f32>;
@group(0) @binding(1) var glow_tex: texture_2d<f32>;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba16float, write>;
@group(0) @binding(3) var<uniform> params: vec4<f32>;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let size = textureDimensions(output_tex);
    if gid.x >= size.x || gid.y >= size.y {
        return;
    }
    let pixel = vec2<i32>(gid.xy);
    let source = textureLoad(source_tex, pixel, 0);
    let glow = textureLoad(glow_tex, pixel, 0);
    let intensity = max(params.x, 0.0);
    let rgb = source.rgb + glow.rgb * intensity;
    let alpha = clamp(source.a + glow.a * intensity, 0.0, 1.0);
    textureStore(output_tex, pixel, vec4<f32>(rgb, alpha));
}
"#;

const CLEAR_WGSL: &str = r#"
@group(0) @binding(0) var output_texture: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dimensions = textureDimensions(output_texture);
    if (gid.x >= dimensions.x || gid.y >= dimensions.y) { return; }
    textureStore(output_texture, vec2<i32>(gid.xy), vec4<f32>(0.0));
}
"#;

const SOLID_WGSL: &str = r#"
struct Params { value: vec4<f32>, }
@group(0) @binding(0) var output_texture: texture_storage_2d<rgba16float, write>;
@group(0) @binding(1) var<uniform> params: Params;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dimensions = textureDimensions(output_texture);
    if (gid.x >= dimensions.x || gid.y >= dimensions.y) { return; }
    textureStore(output_texture, vec2<i32>(gid.xy), params.value);
}
"#;

const COMPOSITE_WGSL: &str = r#"
struct Params { value: vec4<f32>, }
@group(0) @binding(0) var destination_texture: texture_2d<f32>;
@group(0) @binding(1) var source_texture: texture_2d<f32>;
@group(0) @binding(2) var output_texture: texture_storage_2d<rgba16float, write>;
@group(0) @binding(3) var<uniform> params: Params;

fn linear_to_srgb_blend(value: f32) -> f32 {
    let sign = select(-1.0, 1.0, value >= 0.0);
    let v = abs(value);
    let encoded = select(12.92 * v, 1.055 * pow(v, 1.0 / 2.4) - 0.055, v > 0.0031308);
    return sign * encoded;
}

fn srgb_to_linear_blend(value: f32) -> f32 {
    let sign = select(-1.0, 1.0, value >= 0.0);
    let v = abs(value);
    let linear = select(v / 12.92, pow((v + 0.055) / 1.055, 2.4), v > 0.04045);
    return sign * linear;
}

fn blend_channel(dst: f32, src: f32, mode: u32) -> f32 {
    switch mode {
        case 1u: { return src + dst; }
        case 2u: { return dst - src; }
        case 3u: { return src * dst; }
        case 4u: { return 1.0 - (1.0 - src) * (1.0 - dst); }
        case 5u: { return select(1.0 - 2.0 * (1.0 - src) * (1.0 - dst), 2.0 * src * dst, dst <= 0.5); }
        case 6u: { return abs(dst - src); }
        case 7u: { return min(src, dst); }
        case 8u: { return max(src, dst); }
        case 9u: { return select(min(1.0, dst / max(1.0 - src, 0.000001)), 1.0, src >= 0.999999); }
        case 10u: { return select(1.0 - min(1.0, (1.0 - dst) / max(src, 0.000001)), 0.0, src <= 0.000001); }
        case 11u: { return select(1.0 - 2.0 * (1.0 - src) * (1.0 - dst), 2.0 * src * dst, src <= 0.5); }
        case 12u: {
            let d = select(sqrt(max(dst, 0.0)), ((16.0 * dst - 12.0) * dst + 4.0) * dst, dst <= 0.25);
            return select(dst + (2.0 * src - 1.0) * (d - dst), dst - (1.0 - 2.0 * src) * dst * (1.0 - dst), src <= 0.5);
        }
        case 13u: { return src + dst - 2.0 * src * dst; }
        case 14u: { return max(0.0, src + dst - 1.0); }
        case 15u: { return select(min(1.0, dst / max(src, 0.000001)), 1.0, src <= 0.000001 && dst > 0.0); }
        default: { return src; }
    }
}

fn blend_alpha(dst: f32, src: f32, mode: u32) -> f32 {
    switch mode {
        case 1u: { return dst; }
        case 2u: { return src; }
        case 3u: { return clamp(dst + src, 0.0, 1.0); }
        case 4u: { return clamp(dst - src, 0.0, 1.0); }
        case 5u: { return dst * src; }
        case 6u: { return min(dst, src); }
        case 7u: { return max(dst, src); }
        default: { return src + dst * (1.0 - src); }
    }
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dimensions = textureDimensions(destination_texture);
    if (gid.x >= dimensions.x || gid.y >= dimensions.y) { return; }
    let p = vec2<i32>(gid.xy);
    let dst = textureLoad(destination_texture, p, 0);
    let source_dimensions = textureDimensions(source_texture);
    let source_offset = (vec2<i32>(dimensions) - vec2<i32>(source_dimensions)) / 2;
    let source_p = p - source_offset;
    let source_in_bounds = source_p.x >= 0 && source_p.y >= 0
        && source_p.x < i32(source_dimensions.x) && source_p.y < i32(source_dimensions.y);
    if (!source_in_bounds) {
        textureStore(output_texture, p, dst);
        return;
    }
    let source = textureLoad(source_texture, source_p, 0);
    let opacity = clamp(params.value.x, 0.0, 1.0);
    let src = vec4<f32>(source.rgb * opacity, source.a * opacity);
    let sa = clamp(src.a, 0.0, 1.0);
    let da = clamp(dst.a, 0.0, 1.0);
    let cs = src.rgb / max(sa, 0.000001);
    let cd = select(vec3<f32>(0.0), dst.rgb / max(da, 0.000001), da > 0.000001);
    let mode = u32(params.value.y + 0.5);
    let linear_src = clamp(cs, vec3<f32>(0.0), vec3<f32>(1.0));
    let linear_dst = clamp(cd, vec3<f32>(0.0), vec3<f32>(1.0));
    let blend_src = vec3<f32>(
        linear_to_srgb_blend(linear_src.r),
        linear_to_srgb_blend(linear_src.g),
        linear_to_srgb_blend(linear_src.b)
    );
    let blend_dst = vec3<f32>(
        linear_to_srgb_blend(linear_dst.r),
        linear_to_srgb_blend(linear_dst.g),
        linear_to_srgb_blend(linear_dst.b)
    );
    let blended_srgb = vec3<f32>(
        blend_channel(blend_dst.r, blend_src.r, mode),
        blend_channel(blend_dst.g, blend_src.g, mode),
        blend_channel(blend_dst.b, blend_src.b, mode)
    );
    let blended = vec3<f32>(
        srgb_to_linear_blend(blended_srgb.r),
        srgb_to_linear_blend(blended_srgb.g),
        srgb_to_linear_blend(blended_srgb.b)
    );
    let coverage_alpha = sa + da - sa * da;
    let coverage_rgb =
        src.rgb * (1.0 - da) + dst.rgb * (1.0 - sa) + blended * (sa * da);
    let alpha_mode = u32(params.value.z + 0.5);
    let out_alpha = clamp(blend_alpha(da, sa, alpha_mode), 0.0, 1.0);
    let straight_rgb = select(
        vec3<f32>(0.0),
        coverage_rgb / max(coverage_alpha, 0.000001),
        coverage_alpha > 0.000001
    );
    let out_rgb = straight_rgb * out_alpha;
    textureStore(output_texture, p, vec4<f32>(out_rgb, out_alpha));
}
"#;

const PRESENT_WGSL: &str = r#"
@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var output_texture: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dimensions = textureDimensions(input_texture);
    if (gid.x >= dimensions.x || gid.y >= dimensions.y) { return; }
    let p = vec2<i32>(gid.xy);
    let premultiplied = textureLoad(input_texture, p, 0);
    let alpha = clamp(premultiplied.a, 0.0, 1.0);
    let straight_linear = select(
        vec3<f32>(0.0),
        premultiplied.rgb / max(alpha, 0.000001),
        alpha > 0.000001
    );
    textureStore(output_texture, p, vec4<f32>(straight_linear, alpha));
}
"#;
