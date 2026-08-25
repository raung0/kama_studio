mod atlas;
mod types;
pub use atlas::{AtlasEntry, AtlasFull};
use std::sync::Arc;
pub use types::*;

use anyhow::{bail, Context, Result};
use bytemuck::{Pod, Zeroable};
use winit::{dpi::PhysicalSize, window::Window};

use crate::atlas::TextureAtlas;

const TILE_SIZE: u32 = 16;
const INITIAL_COMMAND_CAPACITY: usize = 16_384;
const INITIAL_VERTEX_CAPACITY: usize = 65_536;
const MAX_TILE_REFERENCES: usize = 1_048_576;
pub const MAX_EXTERNAL_TEXTURES: usize = 8;

macro_rules! dispatch {
    ($encoder:expr, $label:expr, $pipeline:expr, $bind_group:expr, $method:ident($($arg:expr),*)) => {{
        let mut pass = $encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some($label),
            timestamp_writes: None,
        });
        pass.set_pipeline(&$pipeline);
        pass.set_bind_group(0, $bind_group, &[]);
        pass.$method($($arg),*);
    }};
}

macro_rules! storage_buffers {
    ($device:expr; $($name:ident: $ty:ty, $label:literal, $count:expr, $usage:expr;)*) => {
        $(let $name = storage_buffer::<$ty>($device, $label, $count, $usage);)*
    };
}

macro_rules! texture_views {
    ($device:expr; $($texture:ident, $view:ident, $label:literal, $width:expr, $height:expr, $mips:expr, $usage:expr;)*) => {
        $(
            let $texture = rgba16_texture($device, $label, $width, $height, $mips, $usage);
            let $view = $texture.create_view(&wgpu::TextureViewDescriptor::default());
        )*
    };
}

macro_rules! atlas_registration {
    ($name:ident, $id:ident, $atlas:ident, $entries:ident) => {
        pub fn $name(&mut self, width: u32, height: u32, pixels: &[u8]) -> Result<$id> {
            register_atlas(
                &mut self.$atlas,
                &mut self.$entries,
                &self.queue,
                width,
                height,
                pixels,
            )
            .map($id)
        }
    };
}

#[repr(u32)]
#[derive(Clone, Copy, Debug)]
pub enum TextureKind {
    None = 0,
    Glyph = 1,
    Icon = 2,
    User = 3,
}

#[derive(Clone, Copy, Debug)]
pub struct ResolvedTexture {
    kind: u32,
    uv: [f32; 4],
    revision: u32,
}

impl ResolvedTexture {
    const fn none() -> Self {
        Self {
            kind: TextureKind::None as u32,
            uv: [0.0, 0.0, 1.0, 1.0],
            revision: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct GpuVertex {
    pub position: [f32; 2],
}

fn crop_uv(atlas: [f32; 4], crop: [f32; 4]) -> [f32; 4] {
    let width = atlas[2] - atlas[0];
    let height = atlas[3] - atlas[1];
    [
        width.mul_add(crop[0], atlas[0]),
        height.mul_add(crop[1], atlas[1]),
        width.mul_add(crop[2], atlas[0]),
        height.mul_add(crop[3], atlas[1]),
    ]
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct DrawCommand {
    rect: [f32; 4],
    fill_color: [f32; 4],
    border_color: [f32; 4],
    params: [f32; 4],
    reveal_color: [f32; 4],
    fill_uv: [f32; 4],
    border_uv: [f32; 4],
    clip_rect_0: [f32; 4],
    clip_rect_1: [f32; 4],
    clip_rect_2: [f32; 4],
    clip_rect_3: [f32; 4],
    clip_radii: [f32; 4],
    texture_and_id: [u32; 4],
    shape_data: [u32; 4],
}

impl DrawCommand {
    fn base(id: u64, rect: Rect, clips: &[ClipShape], color: Color, opacity: f32) -> Self {
        let mut clip_rects = [[0.0; 4]; 4];
        let mut clip_radii = [0.0; 4];
        let clips = &clips[clips.len().saturating_sub(4)..];
        for (index, clip) in clips.iter().enumerate() {
            clip_rects[index] = clip.rect.as_array();
            clip_radii[index] = clip.radius;
        }
        Self {
            rect: rect.as_array(),
            fill_color: color.to_array(),
            border_color: Color::TRANSPARENT.to_array(),
            params: [0.0, 0.0, opacity, 0.0],
            reveal_color: Color::TRANSPARENT.to_array(),
            fill_uv: [0.0, 0.0, 1.0, 1.0],
            border_uv: [0.0, 0.0, 1.0, 1.0],
            clip_rect_0: clip_rects[0],
            clip_rect_1: clip_rects[1],
            clip_rect_2: clip_rects[2],
            clip_rect_3: clip_rects[3],
            clip_radii,
            texture_and_id: [
                TextureKind::None as u32,
                TextureKind::None as u32,
                id as u32,
                (id >> 32) as u32,
            ],
            shape_data: [0, 0, 0, clips.len() as u32],
        }
    }

    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn rounded_block(
        id: u64,
        rect: Rect,
        clips: &[ClipShape],
        fill_color: Color,
        border_color: Color,
        radius: f32,
        border_width: f32,
        fill: ResolvedTexture,
        border: ResolvedTexture,
        opacity: f32,
        reveal_strength: f32,
        reveal_color: Color,
        texture_uv: [f32; 4],
        texture_mode: u32,
        texture_rotation: f32,
    ) -> Self {
        let mut command = Self::base(id, rect, clips, fill_color, opacity);
        command.border_color = border_color.to_array();
        command.params = [radius, border_width, opacity, reveal_strength];
        command.reveal_color = reveal_color.to_array();
        command.fill_uv = crop_uv(fill.uv, texture_uv);
        command.border_uv = border.uv;
        command.texture_and_id = [
            fill.kind,
            border.kind,
            (id as u32) ^ fill.revision.rotate_left(7),
            ((id >> 32) as u32) ^ border.revision.rotate_left(13),
        ];
        command.shape_data[1] = texture_rotation.to_bits();
        command.shape_data[2] = texture_mode;
        command
    }

    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn mesh(
        id: u64,
        rect: Rect,
        clips: &[ClipShape],
        color: Color,
        fill: ResolvedTexture,
        vertex_offset: u32,
        vertex_count: u32,
        geometry_hash: u64,
        opacity: f32,
    ) -> Self {
        let mut command = Self::base(id, rect, clips, color, opacity);
        command.fill_uv = fill.uv;
        command.texture_and_id = [
            fill.kind,
            TextureKind::None as u32,
            (id as u32) ^ fill.revision.rotate_left(7) ^ geometry_hash as u32,
            ((id >> 32) as u32) ^ fill.revision.rotate_left(13) ^ (geometry_hash >> 32) as u32,
        ];
        command.shape_data = [1, vertex_offset, vertex_count, command.shape_data[3]];
        command
    }

    #[must_use]
    pub fn backdrop_blur(
        id: u64,
        rect: Rect,
        clips: &[ClipShape],
        radius: f32,
        tint: Color,
        opacity: f32,
    ) -> Self {
        let mut command = Self::base(id, rect, clips, tint, opacity);
        command.params = [radius, 0.0, opacity, 0.0];
        command.shape_data[0] = 2;
        command
    }

    #[must_use]
    pub fn glyph(
        id: u64,
        rect: Rect,
        clips: &[ClipShape],
        color: Color,
        uv: [f32; 4],
        kind: TextureKind,
        opacity: f32,
    ) -> Self {
        let mut command = Self::base(id, rect, clips, color, opacity);
        command.fill_uv = uv;
        command.texture_and_id[0] = kind as u32;
        command
    }

    fn tile_bounds(
        &self,
        width: u32,
        height: u32,
        tile_x_count: u32,
        tile_y_count: u32,
    ) -> Option<(u32, u32, u32, u32)> {
        let min_x = self.rect[0].floor().max(0.0) as u32;
        let min_y = self.rect[1].floor().max(0.0) as u32;
        let max_x = (self.rect[0] + self.rect[2]).ceil().min(width as f32) as u32;
        let max_y = (self.rect[1] + self.rect[3]).ceil().min(height as f32) as u32;
        if min_x >= max_x || min_y >= max_y {
            return None;
        }

        let min_tile_x = (min_x / TILE_SIZE).min(tile_x_count.saturating_sub(1));
        let min_tile_y = (min_y / TILE_SIZE).min(tile_y_count.saturating_sub(1));
        let max_tile_x = ((max_x - 1) / TILE_SIZE).min(tile_x_count.saturating_sub(1));
        let max_tile_y = ((max_y - 1) / TILE_SIZE).min(tile_y_count.saturating_sub(1));
        Some((min_tile_x, min_tile_y, max_tile_x, max_tile_y))
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FrameUniform {
    width: u32,
    height: u32,
    tile_x_count: u32,
    tile_y_count: u32,
    tile_size: u32,
    mouse_x: f32,
    mouse_y: f32,
    reveal_radius: f32,
}

#[derive(Clone, Copy, Debug)]
struct ManagedEntry {
    atlas: AtlasEntry,
    revision: u32,
}

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    config: wgpu::SurfaceConfiguration,

    scan_pipeline: wgpu::ComputePipeline,
    paint_pipeline: wgpu::ComputePipeline,
    overlay_pipeline: wgpu::ComputePipeline,
    blur_pipeline: wgpu::ComputePipeline,
    present_pipeline: wgpu::RenderPipeline,

    frame_buffer: wgpu::Buffer,
    command_buffer: wgpu::Buffer,
    overlay_command_buffer: wgpu::Buffer,
    overlay_tile_indices_buffer: wgpu::Buffer,
    vertex_buffer: wgpu::Buffer,
    size: SizeResources,
    sampler: wgpu::Sampler,

    glyph_atlas: TextureAtlas,
    icon_atlas: TextureAtlas,
    user_atlas: TextureAtlas,
    user_entries: Vec<ManagedEntry>,
    icon_entries: Vec<ManagedEntry>,

    _dummy_external_texture: wgpu::Texture,
    dummy_external_view: wgpu::TextureView,
    external_views: Vec<wgpu::TextureView>,
    external_revisions: Vec<u32>,

    compute_bind_group: Option<wgpu::BindGroup>,
    paint_bind_group: Option<wgpu::BindGroup>,
    overlay_bind_group: Option<wgpu::BindGroup>,
    blur_bind_groups: Vec<wgpu::BindGroup>,
    present_bind_group: Option<wgpu::BindGroup>,
    scale_factor: f32,
}

impl Renderer {
    pub async fn new(window: Arc<Window>) -> Result<Self> {
        let size = window.inner_size();
        let scale_factor = window.scale_factor() as f32;
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = instance
            .create_surface(window)
            .context("create wgpu surface")?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
                apply_limit_buckets: false,
            })
            .await
            .context("no suitable graphics adapter")?;
        let adapter_features = adapter.features();
        let mut required_features = wgpu::Features::TEXTURE_FORMAT_16BIT_NORM
            | wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES;
        if !adapter_features.contains(required_features) {
            anyhow::bail!(
                "graphics adapter does not support the texture-format features required for native high-bit-depth video"
            );
        }
        let r16uint_features = adapter.get_texture_format_features(wgpu::TextureFormat::R16Uint);
        if !r16uint_features
            .allowed_usages
            .contains(wgpu::TextureUsages::STORAGE_BINDING)
        {
            anyhow::bail!("graphics adapter does not support R16Uint storage textures");
        }
        let _ = format_args!(
            "gpu: enabled native adapter texture-format features for high-bit-depth video"
        );

        if cfg!(all(target_os = "macos", target_arch = "aarch64"))
            && adapter_features.contains(wgpu::Features::MAPPABLE_PRIMARY_BUFFERS)
        {
            required_features |= wgpu::Features::MAPPABLE_PRIMARY_BUFFERS;
            let _ =
                format_args!("gpu: enabled MAPPABLE_PRIMARY_BUFFERS for zero-copy export readback");
        }

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("block-ui device"),
                required_features,
                required_limits: wgpu::Limits::default(),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
            })
            .await
            .context("request wgpu device")?;
        let device = Arc::new(device);
        let queue = Arc::new(queue);

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .unwrap_or(caps.formats[0]);
        let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Fifo) {
            wgpu::PresentMode::Fifo
        } else {
            caps.present_modes[0]
        };
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let compute_shader = shader_module(
            &device,
            "block-ui compute shaders",
            include_str!("shader.wgsl"),
        );
        let blur_shader = shader_module(
            &device,
            "block-ui downsample/upsample blur shader",
            include_str!("blur.wgsl"),
        );
        let present_shader = shader_module(
            &device,
            "block-ui present shader",
            include_str!("present.wgsl"),
        );
        let scan_pipeline = compute_pipeline(&device, "cs_scan", &compute_shader);
        let paint_pipeline = compute_pipeline(&device, "cs_paint", &compute_shader);
        let overlay_pipeline = compute_pipeline(&device, "cs_overlay", &compute_shader);
        let blur_pipeline = compute_pipeline(&device, "cs_blur", &blur_shader);

        let present_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("block-ui fullscreen triangle"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &present_shader,
                entry_point: Some("vs_present"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &present_shader,
                entry_point: Some("fs_present"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let frame_buffer = gpu_buffer(
            &device,
            "frame constants",
            std::mem::size_of::<FrameUniform>() as u64,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        storage_buffers! { &device;
            command_buffer: DrawCommand, "base shape commands", INITIAL_COMMAND_CAPACITY, wgpu::BufferUsages::COPY_DST;
            overlay_command_buffer: DrawCommand, "overlay shape commands", INITIAL_COMMAND_CAPACITY, wgpu::BufferUsages::COPY_DST;
            overlay_tile_indices_buffer: u32, "overlay tile command indices", MAX_TILE_REFERENCES, wgpu::BufferUsages::COPY_DST;
            vertex_buffer: GpuVertex, "custom triangle vertices", INITIAL_VERTEX_CAPACITY, wgpu::BufferUsages::COPY_DST;
        }
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("block-ui atlas sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let glyph_atlas = TextureAtlas::new(
            &device,
            "glyph atlas",
            2048,
            2048,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            4,
        );
        let mut icon_atlas = TextureAtlas::new(
            &device,
            "icon atlas",
            1024,
            1024,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            4,
        );
        let user_atlas = TextureAtlas::new(
            &device,
            "user texture atlas",
            2048,
            2048,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            4,
        );
        let icon_pixels = make_builtin_icon(128);
        let icon_entry = icon_atlas.upload(&queue, 128, 128, &icon_pixels)?;

        let dummy_external_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("dummy external texture"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &dummy_external_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[255, 255, 255, 255],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let dummy_external_view =
            dummy_external_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let size_resources = create_size_resources(&device, &queue, config.width, config.height);
        let mut renderer = Self {
            surface,
            device,
            queue,
            config,
            scan_pipeline,
            paint_pipeline,
            overlay_pipeline,
            blur_pipeline,
            present_pipeline,
            frame_buffer,
            command_buffer,
            overlay_command_buffer,
            overlay_tile_indices_buffer,
            vertex_buffer,
            size: size_resources,
            sampler,
            glyph_atlas,
            icon_atlas,
            user_atlas,
            user_entries: Vec::new(),
            icon_entries: vec![ManagedEntry {
                atlas: icon_entry,
                revision: 0,
            }],
            _dummy_external_texture: dummy_external_texture,
            dummy_external_view,
            external_views: Vec::new(),
            external_revisions: Vec::new(),
            compute_bind_group: None,
            paint_bind_group: None,
            overlay_bind_group: None,
            blur_bind_groups: Vec::new(),
            present_bind_group: None,
            scale_factor: scale_factor.max(0.25),
        };
        renderer.rebuild_bind_groups();
        Ok(renderer)
    }

    pub fn logical_width(&self) -> f32 {
        self.config.width as f32 / self.scale_factor
    }
    pub fn logical_height(&self) -> f32 {
        self.config.height as f32 / self.scale_factor
    }
    pub const fn scale_factor(&self) -> f32 {
        self.scale_factor
    }
    pub const fn builtin_icon(&self) -> IconId {
        IconId(0)
    }

    pub const fn set_scale_factor(&mut self, scale_factor: f64) {
        let scale_factor = scale_factor as f32;
        self.scale_factor = if scale_factor.is_finite() {
            scale_factor.max(0.25)
        } else {
            1.0
        };
    }

    pub fn resize(&mut self, size: PhysicalSize<u32>, scale_factor: f64) {
        self.set_scale_factor(scale_factor);
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.config.width = size.width;
        self.config.height = size.height;
        self.surface.configure(&self.device, &self.config);
        self.size = create_size_resources(&self.device, &self.queue, size.width, size.height);
        self.rebuild_bind_groups();
    }

    atlas_registration!(register_texture_rgba8, TextureId, user_atlas, user_entries);

    pub fn update_texture_rgba8(&mut self, id: TextureId, pixels: &[u8]) -> Result<()> {
        let entry = self
            .user_entries
            .get_mut(id.0 as usize)
            .context("invalid user atlas TextureId")?;
        self.user_atlas.update(&self.queue, entry.atlas, pixels)?;
        entry.revision = entry.revision.wrapping_add(1);
        Ok(())
    }

    pub fn device(&self) -> &wgpu::Device {
        self.device.as_ref()
    }

    pub fn queue(&self) -> &wgpu::Queue {
        self.queue.as_ref()
    }

    pub fn device_handle(&self) -> Arc<wgpu::Device> {
        Arc::clone(&self.device)
    }

    pub fn queue_handle(&self) -> Arc<wgpu::Queue> {
        Arc::clone(&self.queue)
    }

    atlas_registration!(register_icon_rgba8, IconId, icon_atlas, icon_entries);

    pub fn register_external_texture(
        &mut self,
        view: wgpu::TextureView,
    ) -> Result<ExternalTextureId> {
        if self.external_views.len() >= MAX_EXTERNAL_TEXTURES {
            bail!("supports at most {MAX_EXTERNAL_TEXTURES} external TextureViews");
        }
        let id = ExternalTextureId(self.external_views.len() as u32);
        self.external_views.push(view);
        self.external_revisions.push(0);
        self.rebuild_bind_groups();
        Ok(id)
    }

    pub fn replace_external_texture(
        &mut self,
        id: ExternalTextureId,
        view: wgpu::TextureView,
    ) -> Result<()> {
        let slot = self
            .external_views
            .get_mut(id.0 as usize)
            .context("invalid external TextureView ID")?;
        *slot = view;
        let revision = self
            .external_revisions
            .get_mut(id.0 as usize)
            .context("invalid external TextureView ID")?;
        *revision = revision.wrapping_add(1);
        self.rebuild_bind_groups();
        Ok(())
    }

    pub fn invalidate_external_texture(&mut self, id: ExternalTextureId) -> Result<()> {
        let revision = self
            .external_revisions
            .get_mut(id.0 as usize)
            .context("invalid external TextureView ID")?;
        *revision = revision.wrapping_add(1);
        Ok(())
    }

    pub fn upload_glyph(&mut self, width: u32, height: u32, rgba: &[u8]) -> Result<AtlasEntry> {
        self.glyph_atlas.upload(&self.queue, width, height, rgba)
    }

    pub const fn reset_glyph_atlas(&mut self) {
        self.glyph_atlas.reset();
    }

    pub fn resolve_texture(&self, texture: Option<TextureSource>) -> ResolvedTexture {
        match texture {
            None => ResolvedTexture::none(),
            Some(TextureSource::Atlas(id)) => {
                resolve_atlas(&self.user_entries, id.0, TextureKind::User)
            }
            Some(TextureSource::Icon(id)) => {
                resolve_atlas(&self.icon_entries, id.0, TextureKind::Icon)
            }
            Some(TextureSource::External(id)) if (id.0 as usize) < self.external_views.len() => {
                ResolvedTexture {
                    kind: 4 + id.0,
                    uv: [0.0, 0.0, 1.0, 1.0],
                    revision: self.external_revisions[id.0 as usize],
                }
            }
            Some(TextureSource::External(_)) => ResolvedTexture::none(),
        }
    }

    pub fn render(
        &mut self,
        base_commands: &[DrawCommand],
        overlay_commands: &[DrawCommand],
        vertices: &[GpuVertex],
        cursor_logical: [f32; 2],
    ) -> Result<()> {
        let resized = grow_storage_buffer::<DrawCommand>(
            &self.device,
            &mut self.command_buffer,
            base_commands.len(),
            "base shape commands",
        )? | grow_storage_buffer::<DrawCommand>(
            &self.device,
            &mut self.overlay_command_buffer,
            overlay_commands.len(),
            "overlay shape commands",
        )? | grow_storage_buffer::<GpuVertex>(
            &self.device,
            &mut self.vertex_buffer,
            vertices.len(),
            "custom triangle vertices",
        )?;
        if resized {
            self.rebuild_bind_groups();
        }

        let base_bins = self.build_tile_bins(base_commands)?;
        let overlay_bins = if overlay_commands.is_empty() {
            None
        } else {
            Some(self.build_tile_bins(overlay_commands)?)
        };
        let needs_blur = overlay_commands
            .iter()
            .any(|command| command.shape_data[0] == 2);

        let output = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(output)
            | wgpu::CurrentSurfaceTexture::Suboptimal(output) => output,
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                match self.surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(output)
                    | wgpu::CurrentSurfaceTexture::Suboptimal(output) => output,
                    wgpu::CurrentSurfaceTexture::Timeout
                    | wgpu::CurrentSurfaceTexture::Occluded => return Ok(()),
                    wgpu::CurrentSurfaceTexture::Outdated
                    | wgpu::CurrentSurfaceTexture::Lost
                    | wgpu::CurrentSurfaceTexture::Validation => {
                        bail!("failed to reacquire surface texture")
                    }
                }
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(())
            }
            wgpu::CurrentSurfaceTexture::Lost => bail!("surface lost"),
            wgpu::CurrentSurfaceTexture::Validation => bail!("surface validation error"),
        };
        let output_view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let uniform = FrameUniform {
            width: self.config.width,
            height: self.config.height,
            tile_x_count: self.size.tile_x_count,
            tile_y_count: self.size.tile_y_count,
            tile_size: TILE_SIZE,
            mouse_x: cursor_logical[0] * self.scale_factor,
            mouse_y: cursor_logical[1] * self.scale_factor,
            reveal_radius: 144.0 * self.scale_factor,
        };
        self.queue
            .write_buffer(&self.frame_buffer, 0, bytemuck::bytes_of(&uniform));
        for (buffer, bytes) in [
            (
                &self.size.scan_args_buffer,
                bytemuck::cast_slice(&[0u32, 1, 1]),
            ),
            (&self.command_buffer, bytemuck::cast_slice(base_commands)),
            (
                &self.size.tile_offset_buffer,
                bytemuck::cast_slice(&base_bins.offsets),
            ),
            (
                &self.size.tile_index_buffer,
                bytemuck::cast_slice(&base_bins.indices),
            ),
            (
                &self.overlay_command_buffer,
                bytemuck::cast_slice(overlay_commands),
            ),
            (&self.vertex_buffer, bytemuck::cast_slice(vertices)),
        ] {
            write_bytes(&self.queue, buffer, bytes);
        }
        if let Some(bins) = overlay_bins.as_ref() {
            for (buffer, bytes) in [
                (
                    &self.size.overlay_tile_offsets_buffer,
                    bytemuck::cast_slice(&bins.offsets),
                ),
                (
                    &self.overlay_tile_indices_buffer,
                    bytemuck::cast_slice(&bins.indices),
                ),
                (
                    &self.size.overlay_active_tiles_buffer,
                    bytemuck::cast_slice(&bins.active_tiles),
                ),
            ] {
                write_bytes(&self.queue, buffer, bytes);
            }
        }

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("UI frame"),
            });
        let tile_groups = (self.size.tile_x_count * self.size.tile_y_count).div_ceil(64);
        dispatch!(
            encoder,
            "Base cache: find dirty tiles",
            self.scan_pipeline,
            self.compute_bind_group
                .as_ref()
                .expect("compute bind group"),
            dispatch_workgroups(tile_groups, 1, 1)
        );
        dispatch!(
            encoder,
            "Base cache: paint dirty tiles",
            self.paint_pipeline,
            self.paint_bind_group.as_ref().expect("paint bind group"),
            dispatch_workgroups_indirect(&self.size.scan_args_buffer, 0)
        );

        if needs_blur {
            let steps = [
                ("Blur: full → half", &self.blur_bind_groups[0], 2),
                ("Blur: half → quarter", &self.blur_bind_groups[1], 4),
                ("Blur: quarter → eighth", &self.blur_bind_groups[2], 8),
                ("Blur: eighth → quarter", &self.blur_bind_groups[3], 4),
                ("Blur: quarter → half", &self.blur_bind_groups[4], 2),
                ("Blur: half → full", &self.blur_bind_groups[5], 1),
            ];
            for (label, bind_group, divisor) in steps {
                let width = self.config.width.div_ceil(divisor).max(1);
                let height = self.config.height.div_ceil(divisor).max(1);
                dispatch!(
                    encoder,
                    label,
                    self.blur_pipeline,
                    bind_group,
                    dispatch_workgroups(width.div_ceil(16), height.div_ceil(16), 1)
                );
            }
        }

        encoder.copy_texture_to_texture(
            image_copy(&self.size.ui_cache_texture),
            image_copy(&self.size.final_cache_texture),
            wgpu::Extent3d {
                width: self.config.width,
                height: self.config.height,
                depth_or_array_layers: 1,
            },
        );

        if let Some(bins) = overlay_bins
            .as_ref()
            .filter(|bins| !bins.active_tiles.is_empty())
        {
            dispatch!(
                encoder,
                "Overlay tiles",
                self.overlay_pipeline,
                self.overlay_bind_group
                    .as_ref()
                    .expect("overlay bind group"),
                dispatch_workgroups(bins.active_tiles.len() as u32, 1, 1)
            );
        }

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Present"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &output_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.present_pipeline);
            pass.set_bind_group(
                0,
                self.present_bind_group
                    .as_ref()
                    .expect("present bind group"),
                &[],
            );
            pass.draw(0..3, 0..1);
        }

        self.queue.submit(Some(encoder.finish()));
        self.queue.present(output);
        Ok(())
    }

    fn build_tile_bins(&self, commands: &[DrawCommand]) -> Result<TileBins> {
        let tile_count = (self.size.tile_x_count * self.size.tile_y_count) as usize;
        let mut counts = vec![0u32; tile_count];

        for command in commands {
            self.for_each_tile(command, |tile| counts[tile] += 1);
        }

        let mut offsets = Vec::with_capacity(tile_count + 1);
        let mut reference_count = 0u32;
        offsets.push(reference_count);
        for &count in &counts {
            reference_count = reference_count
                .checked_add(count)
                .context("tile reference count overflow")?;
            offsets.push(reference_count);
        }
        let reference_count = reference_count as usize;
        if reference_count > MAX_TILE_REFERENCES {
            bail!(
                "frame generated {reference_count} tile references; limit is {MAX_TILE_REFERENCES}"
            );
        }

        let active_tiles = counts
            .iter()
            .enumerate()
            .filter_map(|(tile, &count)| (count != 0).then_some(tile as u32))
            .collect();
        let mut cursors = offsets[..tile_count].to_vec();
        let mut indices = vec![0u32; reference_count];
        for (command_index, command) in commands.iter().enumerate() {
            self.for_each_tile(command, |tile| {
                indices[cursors[tile] as usize] = command_index as u32;
                cursors[tile] += 1;
            });
        }

        Ok(TileBins {
            offsets,
            indices,
            active_tiles,
        })
    }

    fn for_each_tile(&self, command: &DrawCommand, mut f: impl FnMut(usize)) {
        let Some((min_x, min_y, max_x, max_y)) = command.tile_bounds(
            self.config.width,
            self.config.height,
            self.size.tile_x_count,
            self.size.tile_y_count,
        ) else {
            return;
        };
        for tile_y in min_y..=max_y {
            for tile_x in min_x..=max_x {
                f((tile_y * self.size.tile_x_count + tile_x) as usize);
            }
        }
    }

    fn rebuild_bind_groups(&mut self) {
        self.compute_bind_group = Some(make_bind_group(
            &self.device,
            "block-ui scan bind group",
            &self.scan_pipeline.get_bind_group_layout(0),
            &[
                buffer_entry(0, &self.frame_buffer),
                buffer_entry(1, &self.command_buffer),
                buffer_entry(3, &self.size.tile_offset_buffer),
                buffer_entry(4, &self.size.tile_index_buffer),
                buffer_entry(6, &self.size.previous_hash_buffer),
                buffer_entry(7, &self.size.dirty_tile_buffer),
                buffer_entry(8, &self.size.scan_args_buffer),
            ],
        ));

        let mut paint_entries = vec![
            buffer_entry(0, &self.frame_buffer),
            buffer_entry(1, &self.command_buffer),
            buffer_entry(3, &self.size.tile_offset_buffer),
            buffer_entry(4, &self.size.tile_index_buffer),
            buffer_entry(7, &self.size.dirty_tile_buffer),
            texture_entry(9, &self.size.ui_cache_view),
            sampler_entry(10, &self.sampler),
            texture_entry(11, self.glyph_atlas.view()),
            texture_entry(12, self.icon_atlas.view()),
            texture_entry(13, self.user_atlas.view()),
        ];
        add_external_entries(
            &mut paint_entries,
            &self.external_views,
            &self.dummy_external_view,
            14,
        );
        paint_entries.push(buffer_entry(22, &self.vertex_buffer));
        self.paint_bind_group = Some(make_bind_group(
            &self.device,
            "block-ui paint bind group",
            &self.paint_pipeline.get_bind_group_layout(0),
            &paint_entries,
        ));

        let mut overlay_entries = vec![
            buffer_entry(0, &self.frame_buffer),
            buffer_entry(1, &self.overlay_command_buffer),
            texture_entry(2, &self.size.ui_cache_view),
            texture_entry(9, &self.size.final_cache_view),
            sampler_entry(10, &self.sampler),
            texture_entry(11, self.glyph_atlas.view()),
            texture_entry(12, self.icon_atlas.view()),
            texture_entry(13, self.user_atlas.view()),
        ];
        add_external_entries(
            &mut overlay_entries,
            &self.external_views,
            &self.dummy_external_view,
            14,
        );
        overlay_entries.extend([
            buffer_entry(22, &self.vertex_buffer),
            buffer_entry(23, &self.size.overlay_tile_offsets_buffer),
            buffer_entry(24, &self.overlay_tile_indices_buffer),
            buffer_entry(25, &self.size.overlay_active_tiles_buffer),
            texture_entry(26, &self.size.blurred_cache_view),
        ]);
        let overlay_layout = self.overlay_pipeline.get_bind_group_layout(0);
        self.overlay_bind_group = Some(make_bind_group(
            &self.device,
            "block-ui overlay bind group",
            &overlay_layout,
            &overlay_entries,
        ));

        let blur_layout = self.blur_pipeline.get_bind_group_layout(0);
        let size = &self.size;
        self.blur_bind_groups = [
            (&size.ui_cache_view, &size.blur_scratch_view),
            (&size.blur_scratch_view, &size.blurred_cache_quarter_view),
            (&size.blurred_cache_quarter_view, &size.blur_eighth_view),
            (&size.blur_eighth_view, &size.blurred_cache_quarter_view),
            (&size.blurred_cache_quarter_view, &size.blur_scratch_view),
            (&size.blur_scratch_view, &size.blurred_cache_view),
        ]
        .map(|(source, destination)| {
            make_bind_group(
                &self.device,
                "block-ui blur bind group",
                &blur_layout,
                &[
                    texture_entry(0, source),
                    sampler_entry(1, &self.sampler),
                    texture_entry(2, destination),
                ],
            )
        })
        .into();

        let present_layout = self.present_pipeline.get_bind_group_layout(0);
        self.present_bind_group = Some(make_bind_group(
            &self.device,
            "block-ui present bind group",
            &present_layout,
            &[texture_entry(0, &self.size.final_cache_view)],
        ));
    }
}

fn register_atlas(
    atlas: &mut TextureAtlas,
    entries: &mut Vec<ManagedEntry>,
    queue: &wgpu::Queue,
    width: u32,
    height: u32,
    pixels: &[u8],
) -> Result<u32> {
    let entry = atlas.upload(queue, width, height, pixels)?;
    let id = entries.len() as u32;
    entries.push(ManagedEntry {
        atlas: entry,
        revision: 0,
    });
    Ok(id)
}

fn resolve_atlas(entries: &[ManagedEntry], id: u32, kind: TextureKind) -> ResolvedTexture {
    entries
        .get(id as usize)
        .map_or_else(ResolvedTexture::none, |entry| ResolvedTexture {
            kind: kind as u32,
            uv: entry.atlas.uv,
            revision: entry.revision,
        })
}

fn write_bytes(queue: &wgpu::Queue, buffer: &wgpu::Buffer, bytes: &[u8]) {
    if !bytes.is_empty() {
        queue.write_buffer(buffer, 0, bytes);
    }
}

const fn image_copy(texture: &wgpu::Texture) -> wgpu::TexelCopyTextureInfo<'_> {
    wgpu::TexelCopyTextureInfo {
        texture,
        mip_level: 0,
        origin: wgpu::Origin3d::ZERO,
        aspect: wgpu::TextureAspect::All,
    }
}

fn shader_module(device: &wgpu::Device, label: &str, source: &'static str) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    })
}

fn compute_pipeline(
    device: &wgpu::Device,
    entry_point: &'static str,
    module: &wgpu::ShaderModule,
) -> wgpu::ComputePipeline {
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(entry_point),
        layout: None,
        module,
        entry_point: Some(entry_point),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: None,
    })
}

fn gpu_buffer(
    device: &wgpu::Device,
    label: &str,
    size: u64,
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: size.max(4),
        usage,
        mapped_at_creation: false,
    })
}

fn grow_storage_buffer<T>(
    device: &wgpu::Device,
    buffer: &mut wgpu::Buffer,
    needed: usize,
    label: &str,
) -> Result<bool> {
    let item_size = std::mem::size_of::<T>();
    if needed <= buffer.size() as usize / item_size {
        return Ok(false);
    }
    let max = device.limits().max_storage_buffer_binding_size as usize / item_size;
    if needed > max {
        bail!("{needed} {label} exceed the device limit of {max}");
    }
    let next = needed.checked_next_power_of_two().unwrap_or(max).min(max);
    *buffer = storage_buffer::<T>(device, label, next, wgpu::BufferUsages::COPY_DST);
    Ok(true)
}

fn storage_buffer<T>(
    device: &wgpu::Device,
    label: &str,
    count: usize,
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    gpu_buffer(
        device,
        label,
        (count * std::mem::size_of::<T>()) as u64,
        wgpu::BufferUsages::STORAGE | usage,
    )
}

fn buffer_entry(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: buffer.as_entire_binding(),
    }
}

const fn texture_entry(binding: u32, view: &wgpu::TextureView) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: wgpu::BindingResource::TextureView(view),
    }
}

const fn sampler_entry(binding: u32, sampler: &wgpu::Sampler) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: wgpu::BindingResource::Sampler(sampler),
    }
}

fn add_external_entries<'a>(
    entries: &mut Vec<wgpu::BindGroupEntry<'a>>,
    views: &'a [wgpu::TextureView],
    dummy: &'a wgpu::TextureView,
    first_binding: u32,
) {
    entries.extend((0..MAX_EXTERNAL_TEXTURES).map(|slot| {
        texture_entry(
            first_binding + slot as u32,
            views.get(slot).unwrap_or(dummy),
        )
    }));
}

fn make_bind_group(
    device: &wgpu::Device,
    label: &str,
    layout: &wgpu::BindGroupLayout,
    entries: &[wgpu::BindGroupEntry<'_>],
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout,
        entries,
    })
}

struct TileBins {
    offsets: Vec<u32>,
    indices: Vec<u32>,
    active_tiles: Vec<u32>,
}

struct SizeResources {
    tile_x_count: u32,
    tile_y_count: u32,
    tile_offset_buffer: wgpu::Buffer,
    tile_index_buffer: wgpu::Buffer,
    previous_hash_buffer: wgpu::Buffer,
    dirty_tile_buffer: wgpu::Buffer,
    scan_args_buffer: wgpu::Buffer,
    overlay_tile_offsets_buffer: wgpu::Buffer,
    overlay_active_tiles_buffer: wgpu::Buffer,
    ui_cache_texture: wgpu::Texture,
    ui_cache_view: wgpu::TextureView,
    final_cache_texture: wgpu::Texture,
    final_cache_view: wgpu::TextureView,
    blur_scratch_view: wgpu::TextureView,
    blur_eighth_view: wgpu::TextureView,
    blurred_cache_quarter_view: wgpu::TextureView,
    blurred_cache_view: wgpu::TextureView,
}

fn create_size_resources(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    width: u32,
    height: u32,
) -> SizeResources {
    let tile_x_count = width.div_ceil(TILE_SIZE);
    let tile_y_count = height.div_ceil(TILE_SIZE);
    let tile_count = (tile_x_count * tile_y_count).max(1);
    storage_buffers! { device;
        tile_offset_buffer: u32, "tile command offsets", tile_count as usize + 1, wgpu::BufferUsages::COPY_DST;
        tile_index_buffer: u32, "tile command indices", MAX_TILE_REFERENCES, wgpu::BufferUsages::COPY_DST;
        previous_hash_buffer: u32, "previous tile hashes", tile_count as usize, wgpu::BufferUsages::COPY_DST;
    }
    queue.write_buffer(
        &previous_hash_buffer,
        0,
        bytemuck::cast_slice(&vec![u32::MAX; tile_count as usize]),
    );
    storage_buffers! { device;
        dirty_tile_buffer: u32, "dirty tile work indices", tile_count as usize, wgpu::BufferUsages::empty();
        scan_args_buffer: u32, "dirty-tile indirect dispatch args", 3, wgpu::BufferUsages::INDIRECT | wgpu::BufferUsages::COPY_DST;
        overlay_tile_offsets_buffer: u32, "overlay tile command offsets", tile_count as usize + 1, wgpu::BufferUsages::COPY_DST;
        overlay_active_tiles_buffer: u32, "overlay active tile indices", tile_count as usize, wgpu::BufferUsages::COPY_DST;
    }
    let storage_texture_usage =
        wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING;
    texture_views! { device;
        ui_cache_texture, ui_cache_view, "persistent ui cache", width.max(1), height.max(1), 1, storage_texture_usage | wgpu::TextureUsages::COPY_SRC;
        final_cache_texture, final_cache_view, "final UI cache with overlays", width.max(1), height.max(1), 1, storage_texture_usage | wgpu::TextureUsages::COPY_DST;
        blur_scratch_texture, blur_scratch_view, "half-resolution popup blur scratch texture", width.max(1).div_ceil(2), height.max(1).div_ceil(2), 1, storage_texture_usage;
        blur_eighth_texture, blur_eighth_view, "eighth-resolution popup blur scratch texture", width.max(1).div_ceil(8), height.max(1).div_ceil(8), 1, storage_texture_usage;
    }

    let blurred_cache_texture = rgba16_texture(
        device,
        "full-resolution smooth popup backdrop with quarter mip scratch",
        width.max(4),
        height.max(4),
        3,
        storage_texture_usage,
    );
    let blurred_cache_quarter_view = mip_view(
        &blurred_cache_texture,
        "quarter-resolution popup blur scratch mip",
        2,
    );
    let blurred_cache_view = mip_view(
        &blurred_cache_texture,
        "full-resolution popup blurred backdrop",
        0,
    );

    SizeResources {
        tile_x_count,
        tile_y_count,
        tile_offset_buffer,
        tile_index_buffer,
        previous_hash_buffer,
        dirty_tile_buffer,
        scan_args_buffer,
        overlay_tile_offsets_buffer,
        overlay_active_tiles_buffer,
        ui_cache_texture,
        ui_cache_view,
        final_cache_texture,
        final_cache_view,
        blur_scratch_view,
        blur_eighth_view,
        blurred_cache_quarter_view,
        blurred_cache_view,
    }
}

fn rgba16_texture(
    device: &wgpu::Device,
    label: &str,
    width: u32,
    height: u32,
    mip_level_count: u32,
    usage: wgpu::TextureUsages,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage,
        view_formats: &[],
    })
}

fn mip_view(texture: &wgpu::Texture, label: &str, base_mip_level: u32) -> wgpu::TextureView {
    texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some(label),
        format: None,
        dimension: Some(wgpu::TextureViewDimension::D2),
        aspect: wgpu::TextureAspect::All,
        base_mip_level,
        mip_level_count: Some(1),
        base_array_layer: 0,
        array_layer_count: Some(1),
        usage: None,
    })
}

fn make_builtin_icon(size: u32) -> Vec<u8> {
    let mut pixels = vec![0u8; (size * size * 4) as usize];
    let center = (size as f32 - 1.0) * 0.5;
    let radius = size as f32 * 0.375;
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let edge = radius - dx.hypot(dy);
            let alpha = ((edge + 0.5).clamp(0.0, 1.0) * 255.0) as u8;
            let i = ((y * size + x) * 4) as usize;
            pixels[i] = 255;
            pixels[i + 1] = 255;
            pixels[i + 2] = 255;
            pixels[i + 3] = alpha;
        }
    }
    pixels
}
