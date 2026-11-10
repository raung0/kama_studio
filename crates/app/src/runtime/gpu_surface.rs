use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::cpu_surface::CpuSurfaceSessionDescriptor;

#[cfg(target_os = "linux")]
#[path = "gpu_surface_vulkan.rs"]
mod platform;
#[cfg(target_os = "windows")]
#[path = "gpu_surface_dx12.rs"]
mod platform;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum GpuIdentity {
    Vulkan { device: [u8; 16], driver: [u8; 16] },
    Dx12 { luid: [u8; 8] },
}

impl GpuIdentity {
    pub const fn backend(&self) -> wgpu::Backends {
        match self {
            Self::Vulkan { .. } => wgpu::Backends::VULKAN,
            Self::Dx12 { .. } => wgpu::Backends::DX12,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Descriptor {
    pub channel: String,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub allocation_size: u64,
    pub memory_type: u32,
    pub handle: u64,
    pub gpu: GpuIdentity,
    pub session_token: [u8; 32],
    pub exporter_pid: i32,
    pub importer_pid: i32,
}

impl Descriptor {
    fn validate(&self, session: &CpuSurfaceSessionDescriptor) -> Result<()> {
        let expected_processes = (session.exporter_pid, session.parent_pid);
        anyhow::ensure!(
            self.session_token == session.session_token
                && (self.exporter_pid, self.importer_pid) == expected_processes,
            "GPU surface does not belong to the supervised worker session"
        );
        anyhow::ensure!(
            self.channel.starts_with("gpu-")
                && self.channel.len() <= 64
                && self
                    .channel
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
            "invalid GPU surface channel"
        );
        let (stride, size) = layout(self.width, self.height)?;
        anyhow::ensure!(
            self.stride == stride
                && self.allocation_size >= size
                && self.allocation_size <= 512 * 1024 * 1024,
            "invalid GPU surface allocation layout"
        );
        Ok(())
    }
}

pub fn identity(device: &wgpu::Device) -> Result<GpuIdentity> {
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    return platform::identity(device);
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        let _ = device;
        anyhow::bail!("native GPU sharing is unavailable on this platform")
    }
}

fn layout(width: u32, height: u32) -> Result<(u32, u64)> {
    anyhow::ensure!(
        (1..=16_384).contains(&width) && (1..=16_384).contains(&height),
        "invalid GPU surface dimensions"
    );
    let stride = width
        .checked_mul(8)
        .context("GPU surface stride overflow")?
        .checked_add(255)
        .context("GPU surface stride alignment overflow")?
        & !255;
    let size = u64::from(stride)
        .checked_mul(u64::from(height))
        .context("GPU surface size overflow")?;
    anyhow::ensure!(size <= 256 * 1024 * 1024, "GPU surface exceeds 256 MiB");
    Ok((stride, size))
}

fn device_layout(device: &wgpu::Device, width: u32, height: u32) -> Result<(u32, u64)> {
    let limits = device.limits();
    anyhow::ensure!(
        width <= limits.max_texture_dimension_2d && height <= limits.max_texture_dimension_2d,
        "GPU surface dimensions exceed device limits"
    );
    let (stride, size) = layout(width, height)?;
    anyhow::ensure!(
        size <= limits.max_buffer_size,
        "GPU surface exceeds device buffer limits"
    );
    Ok((stride, size))
}

fn texture(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Kama GPU-only cross-process preview"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn copy_encoder(
    device: &wgpu::Device,
    texture: &wgpu::Texture,
    importing: bool,
) -> wgpu::CommandEncoder {
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("Kama cross-process GPU copy"),
    });
    if importing {
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Initialize private GPU preview"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
    }
    encoder.transition_resources(
        std::iter::empty(),
        std::iter::once(wgpu::TextureTransition {
            texture,
            selector: None,
            state: if importing {
                wgpu::TextureUses::COPY_DST
            } else {
                wgpu::TextureUses::COPY_SRC
            },
        }),
    );
    encoder
}

fn wait(device: &wgpu::Device, submission: wgpu::SubmissionIndex) -> Result<()> {
    device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: Some(Duration::from_secs(5)),
        })
        .context("wait for cross-process GPU copy")?;
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
pub use platform::SharedSurface;

#[cfg(any(target_os = "linux", target_os = "windows"))]
pub fn receive(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    descriptor: &Descriptor,
    session: &CpuSurfaceSessionDescriptor,
) -> Result<wgpu::Texture> {
    descriptor.validate(session)?;
    device_layout(device, descriptor.width, descriptor.height)?;
    anyhow::ensure!(
        identity(device)? == descriptor.gpu,
        "GPU surface adapter mismatch"
    );
    platform::receive(device, queue, descriptor, session)
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub enum SharedSurface {}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
impl SharedSurface {
    pub fn allocate(
        _: &wgpu::Device,
        _: u32,
        _: u32,
        _: &CpuSurfaceSessionDescriptor,
    ) -> Result<Self> {
        anyhow::bail!("native GPU sharing is unavailable on this platform")
    }

    pub fn descriptor(&self) -> &Descriptor {
        match *self {}
    }
    pub fn texture(&self) -> &wgpu::Texture {
        match *self {}
    }
    pub fn publish(&mut self, _: &wgpu::Device, _: &wgpu::Queue) -> Result<()> {
        match *self {}
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub fn receive(
    _: &wgpu::Device,
    _: &wgpu::Queue,
    _: &Descriptor,
    _: &CpuSurfaceSessionDescriptor,
) -> Result<wgpu::Texture> {
    anyhow::bail!("native GPU sharing is unavailable on this platform")
}
