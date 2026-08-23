use std::sync::atomic::AtomicU64;

use anyhow::Result;

use crate::{
    effects::EffectRuntime,
    plugin::PluginRegistry,
    project::Project,
    runtime::video::{ExportPassArgs, GpuFrame, VideoGpuRuntime},
    timeline::TimelineState,
};

pub(super) struct ExportReadbackSurface {
    pub(super) texture: wgpu::Texture,
    pub(super) view: wgpu::TextureView,
    pub(super) buffer: wgpu::Buffer,
    width: u32,
    height: u32,
    pub(super) row_bytes: u64,
    pub(super) padded_row_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExportPixelFormat {
    
    Nv12,
    
    P010Le,
    
    P210Le,
    
    Ayuv64Le,
    
    Yuva444p10Le,
}

pub(crate) struct ExportRgba16Args<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub project: &'a Project,
    pub timeline: &'a TimelineState,
    pub runtime: (&'a EffectRuntime, &'a PluginRegistry),
    pub timeline_time: f32,
}

pub(crate) struct ExportYuvBatchArgs<'a, W: std::io::Write> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub project: &'a Project,
    pub timeline: &'a TimelineState,
    pub runtime: (&'a EffectRuntime, &'a PluginRegistry),
    pub timeline_times: &'a [f32],
    pub first_frame: u64,
    pub live_end_frame: &'a AtomicU64,
    pub format: ExportPixelFormat,
    pub writer: &'a mut W,
}

impl ExportPixelFormat {
    pub(crate) fn ffmpeg_name(self) -> &'static str {
        match self {
            Self::Nv12 => "nv12",
            Self::P010Le => "p010le",
            Self::P210Le => "p210le",
            Self::Ayuv64Le => "ayuv64le",
            Self::Yuva444p10Le => "yuva444p10le",
        }
    }

    fn texture_format(self) -> wgpu::TextureFormat {
        match self {
            
            Self::Nv12 | Self::P010Le | Self::P210Le => wgpu::TextureFormat::R16Uint,
            Self::Ayuv64Le => wgpu::TextureFormat::Rgba16Uint,
            Self::Yuva444p10Le => wgpu::TextureFormat::R16Uint,
        }
    }

    fn plane_count(self) -> usize {
        match self {
            
            Self::Nv12 | Self::P010Le => 1,
            Self::P210Le => 2,
            Self::Ayuv64Le => 1,
            Self::Yuva444p10Le => 4,
        }
    }

    fn row_bytes(self, width: u32) -> u64 {
        let width = u64::from(width);
        match self {
            
            
            Self::Nv12 => width * 3 / 2,
            Self::P010Le => width * 3,
            Self::P210Le => width * 2,
            Self::Ayuv64Le => width * 8,
            Self::Yuva444p10Le => width * 2,
        }
    }

    fn direct_buffer_only(self) -> bool {
        matches!(self, Self::Nv12 | Self::P010Le | Self::P210Le)
    }
}




pub(super) struct EncodeReadbackSurface {
    textures: Vec<wgpu::Texture>,
    views: Vec<wgpu::TextureView>,
    pub(super) buffer: wgpu::Buffer,
    format: ExportPixelFormat,
    width: u32,
    height: u32,
    row_bytes: u64,
    padded_row_bytes: u64,
    plane_stride: u64,
    direct_mapped_storage: bool,
}

impl EncodeReadbackSurface {
    pub(super) fn new(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        format: ExportPixelFormat,
    ) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        let texture_format = format.texture_format();
        let row_bytes = format.row_bytes(width);
        let exact_plane_stride = row_bytes * u64::from(height);
        let exact_size = exact_plane_stride * format.plane_count() as u64;
        let storage_limit = u64::from(device.limits().max_storage_buffer_binding_size);
        let direct_mapped_storage = device
            .features()
            .contains(wgpu::Features::MAPPABLE_PRIMARY_BUFFERS)
            && exact_size <= storage_limit
            && (format != ExportPixelFormat::Yuva444p10Le || width.is_multiple_of(2))
            && (format != ExportPixelFormat::P210Le || width.is_multiple_of(2))
            && (format != ExportPixelFormat::P010Le
                || (width.is_multiple_of(2) && height.is_multiple_of(2)))
            && (format != ExportPixelFormat::Nv12
                || (width.is_multiple_of(4) && height.is_multiple_of(2)));

        if format.direct_buffer_only() && !direct_mapped_storage {
            panic!(
                "{} export requires MAPPABLE_PRIMARY_BUFFERS and compatible 4:2:x dimensions",
                format.ffmpeg_name()
            );
        }
        let mut textures = Vec::new();
        let mut views = Vec::new();
        let (padded_row_bytes, plane_stride, usage) = if direct_mapped_storage {
            (
                row_bytes,
                exact_plane_stride,
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::MAP_READ,
            )
        } else {
            textures.reserve(format.plane_count());
            views.reserve(format.plane_count());
            for plane in 0..format.plane_count() {
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(match format {
                        ExportPixelFormat::Nv12 => "kama export NV12 plane",
                        ExportPixelFormat::P010Le => "kama export P010 plane",
                        ExportPixelFormat::P210Le => "kama export P210 plane",
                        ExportPixelFormat::Ayuv64Le => "kama export AYUV64",
                        ExportPixelFormat::Yuva444p10Le => "kama export YUVA10 plane",
                    }),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: texture_format,
                    usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                });
                let view = texture.create_view(&wgpu::TextureViewDescriptor {
                    label: Some(if plane == 0 {
                        "kama export YUV primary view"
                    } else {
                        "kama export YUV plane view"
                    }),
                    ..Default::default()
                });
                textures.push(texture);
                views.push(view);
            }
            let alignment = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as u64;
            let padded = row_bytes.div_ceil(alignment) * alignment;
            (
                padded,
                padded * u64::from(height),
                wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            )
        };
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(if direct_mapped_storage {
                "kama direct encoder YUV buffer"
            } else {
                "kama encoder-native YUV readback"
            }),
            size: plane_stride * format.plane_count() as u64,
            usage,
            mapped_at_creation: false,
        });
        Self {
            textures,
            views,
            buffer,
            format,
            width,
            height,
            row_bytes,
            padded_row_bytes,
            plane_stride,
            direct_mapped_storage,
        }
    }

    pub(super) fn matches(&self, width: u32, height: u32, format: ExportPixelFormat) -> bool {
        self.width == width.max(1) && self.height == height.max(1) && self.format == format
    }

    pub(super) fn encode_gpu_conversion(
        &self,
        gpu: &VideoGpuRuntime,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        frame: &GpuFrame,
    ) {
        if self.direct_mapped_storage {
            match self.format {
                ExportPixelFormat::Nv12 => {
                    gpu.export_nv12_to_buffer(
                        ExportPassArgs {
                            device,
                            encoder,
                            input: frame,
                        },
                        &self.buffer,
                    );
                }
                ExportPixelFormat::P010Le => {
                    gpu.export_p010_to_buffer(
                        ExportPassArgs {
                            device,
                            encoder,
                            input: frame,
                        },
                        &self.buffer,
                    );
                }
                ExportPixelFormat::P210Le => {
                    gpu.export_p210_to_buffer(
                        ExportPassArgs {
                            device,
                            encoder,
                            input: frame,
                        },
                        &self.buffer,
                    );
                }
                ExportPixelFormat::Ayuv64Le => {
                    gpu.export_ayuv64_to_buffer(
                        ExportPassArgs {
                            device,
                            encoder,
                            input: frame,
                        },
                        &self.buffer,
                    );
                }
                ExportPixelFormat::Yuva444p10Le => {
                    gpu.export_yuva10_to_buffer(
                        ExportPassArgs {
                            device,
                            encoder,
                            input: frame,
                        },
                        &self.buffer,
                    );
                }
            }
            return;
        }
        match self.format {
            ExportPixelFormat::Nv12 | ExportPixelFormat::P010Le | ExportPixelFormat::P210Le => {
                unreachable!("4:2:x packed export is direct-buffer-only")
            }
            ExportPixelFormat::Ayuv64Le => {
                gpu.export_ayuv64_into(
                    ExportPassArgs {
                        device,
                        encoder,
                        input: frame,
                    },
                    &self.views[0],
                );
            }
            ExportPixelFormat::Yuva444p10Le => {
                gpu.export_yuva10_into(
                    ExportPassArgs {
                        device,
                        encoder,
                        input: frame,
                    },
                    [
                        &self.views[0],
                        &self.views[1],
                        &self.views[2],
                        &self.views[3],
                    ],
                );
            }
        }
    }

    pub(super) fn copy_to_buffer(&self, encoder: &mut wgpu::CommandEncoder) {
        if self.direct_mapped_storage {
            return;
        }
        for (plane, texture) in self.textures.iter().enumerate() {
            encoder.copy_texture_to_buffer(
                wgpu::ImageCopyTexture {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::ImageCopyBuffer {
                    buffer: &self.buffer,
                    layout: wgpu::ImageDataLayout {
                        offset: self.plane_stride * plane as u64,
                        bytes_per_row: Some(self.padded_row_bytes as u32),
                        rows_per_image: Some(self.height),
                    },
                },
                wgpu::Extent3d {
                    width: self.width,
                    height: self.height,
                    depth_or_array_layers: 1,
                },
            );
        }
    }

    pub(super) fn write_mapped<W: std::io::Write>(
        &self,
        mapped: &[u8],
        writer: &mut W,
    ) -> Result<()> {
        if self.direct_mapped_storage {
            writer.write_all(mapped)?;
            return Ok(());
        }
        let row_bytes = self.row_bytes as usize;
        let padded = self.padded_row_bytes as usize;
        let plane_stride = self.plane_stride as usize;
        for plane in 0..self.format.plane_count() {
            let base = plane * plane_stride;
            if row_bytes == padded {
                writer.write_all(&mapped[base..base + row_bytes * self.height as usize])?;
            } else {
                for y in 0..self.height as usize {
                    let start = base + y * padded;
                    writer.write_all(&mapped[start..start + row_bytes])?;
                }
            }
        }
        Ok(())
    }
}

impl ExportReadbackSurface {
    pub(super) fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("kama export RGBA16"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Uint,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let row_bytes = width as u64 * 8;
        let alignment = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as u64;
        let padded_row_bytes = row_bytes.div_ceil(alignment) * alignment;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kama export readback"),
            size: padded_row_bytes * height as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        Self {
            texture,
            view,
            buffer,
            width,
            height,
            row_bytes,
            padded_row_bytes,
        }
    }

    pub(super) fn matches(&self, width: u32, height: u32) -> bool {
        self.width == width.max(1) && self.height == height.max(1)
    }
}




#[derive(Default)]
pub(super) struct ExportReadbacks {
    rgba: Option<ExportReadbackSurface>,
    encode: Vec<EncodeReadbackSurface>,
}

impl ExportReadbacks {
    pub(super) fn ensure_rgba(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        if self
            .rgba
            .as_ref()
            .is_none_or(|surface| !surface.matches(width, height))
        {
            self.rgba = Some(ExportReadbackSurface::new(device, width, height));
        }
    }

    pub(super) fn rgba(&self) -> &ExportReadbackSurface {
        self.rgba
            .as_ref()
            .expect("export readback surface initialized")
    }

    pub(super) fn ensure_encode_batch(
        &mut self,
        device: &wgpu::Device,
        count: usize,
        width: u32,
        height: u32,
        format: ExportPixelFormat,
    ) {
        while self.encode.len() < count {
            self.encode
                .push(EncodeReadbackSurface::new(device, width, height, format));
        }
        for surface in self.encode.iter_mut().take(count) {
            if !surface.matches(width, height, format) {
                *surface = EncodeReadbackSurface::new(device, width, height, format);
            }
        }
    }

    pub(super) fn encode(&self, index: usize) -> &EncodeReadbackSurface {
        &self.encode[index]
    }
}
