use std::{
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result};
use windows::Win32::{
    Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle, GENERIC_ALL, HANDLE},
    Graphics::Direct3D12::*,
    System::Threading::{GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE},
};

use super::{CpuSurfaceSessionDescriptor, Descriptor, GpuIdentity};

static NEXT_CHANNEL: AtomicU64 = AtomicU64::new(1);

pub fn identity(device: &wgpu::Device) -> Result<GpuIdentity> {
    let hal =
        unsafe { device.as_hal::<wgpu::hal::api::Dx12>() }.context("preview device is not DX12")?;
    let luid = unsafe { hal.raw_device().GetAdapterLuid() };
    let mut bytes = [0u8; 8];
    bytes[..4].copy_from_slice(&luid.LowPart.to_le_bytes());
    bytes[4..].copy_from_slice(&luid.HighPart.to_le_bytes());
    Ok(GpuIdentity::Dx12 { luid: bytes })
}

fn wrap(device: &wgpu::Device, resource: ID3D12Resource, size: u64) -> wgpu::Buffer {
    unsafe {
        let buffer = wgpu::hal::dx12::Device::buffer_from_raw(resource, size);
        device.create_buffer_from_hal::<wgpu::hal::api::Dx12>(
            buffer,
            &wgpu::BufferDescriptor {
                label: Some("Kama DX12 shared preview buffer"),
                size,
                usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            },
        )
    }
}

fn copy(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    buffer: &wgpu::Buffer,
    texture: &wgpu::Texture,
    descriptor: &Descriptor,
    importing: bool,
) -> Result<()> {
    let mut encoder = super::copy_encoder(device, texture, importing);
    let layout = wgpu::TexelCopyBufferLayout {
        offset: 0,
        bytes_per_row: Some(descriptor.stride),
        rows_per_image: Some(descriptor.height),
    };
    let buffer_copy = wgpu::TexelCopyBufferInfo { buffer, layout };
    let texture_copy = wgpu::TexelCopyTextureInfo {
        texture,
        mip_level: 0,
        origin: wgpu::Origin3d::ZERO,
        aspect: wgpu::TextureAspect::All,
    };
    let extent = wgpu::Extent3d {
        width: descriptor.width,
        height: descriptor.height,
        depth_or_array_layers: 1,
    };
    if importing {
        encoder.copy_buffer_to_texture(buffer_copy, texture_copy, extent);
    } else {
        encoder.copy_texture_to_buffer(texture_copy, buffer_copy, extent);
    }
    encoder.transition_resources(
        std::iter::once(wgpu::BufferTransition {
            buffer,
            state: wgpu::BufferUses::empty(),
        }),
        std::iter::empty(),
    );
    super::wait(device, queue.submit([encoder.finish()]))
}

pub struct SharedSurface {
    _handle: OwnedHandle,
    buffer: wgpu::Buffer,
    texture: wgpu::Texture,
    descriptor: Descriptor,
}

impl SharedSurface {
    pub fn allocate(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        session: &CpuSurfaceSessionDescriptor,
    ) -> Result<Self> {
        let gpu = identity(device)?;
        let (stride, size) = super::device_layout(device, width, height)?;
        let hal = unsafe { device.as_hal::<wgpu::hal::api::Dx12>() }
            .context("preview device is not DX12")?;
        let heap = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_DEFAULT,
            CreationNodeMask: 1,
            VisibleNodeMask: 1,
            ..Default::default()
        };
        let description = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Width: size,
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            ..Default::default()
        };
        let mut resource: Option<ID3D12Resource> = None;
        unsafe {
            hal.raw_device().CreateCommittedResource(
                &heap,
                D3D12_HEAP_FLAG_SHARED,
                &description,
                D3D12_RESOURCE_STATE_COMMON,
                None,
                &mut resource,
            )
        }
        .context("allocate DX12 shared preview buffer")?;
        let resource = resource.context("DX12 returned no shared buffer")?;
        let handle = unsafe {
            hal.raw_device()
                .CreateSharedHandle(&resource, None, GENERIC_ALL.0, None)
        }
        .context("create DX12 shared preview handle")?;
        let handle = unsafe { OwnedHandle::from_raw_handle(handle.0) };
        let descriptor = Descriptor {
            channel: format!(
                "gpu-{}-{}",
                std::process::id(),
                NEXT_CHANNEL.fetch_add(1, Ordering::Relaxed)
            ),
            width,
            height,
            stride,
            allocation_size: size,
            memory_type: 0,
            handle: handle.as_raw_handle() as usize as u64,
            gpu,
            session_token: session.session_token,
            exporter_pid: session.exporter_pid,
            importer_pid: session.parent_pid,
        };
        Ok(Self {
            _handle: handle,
            buffer: wrap(device, resource, size),
            texture: super::texture(device, width, height),
            descriptor,
        })
    }

    pub const fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }
    pub const fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    pub fn publish(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> Result<()> {
        copy(
            device,
            queue,
            &self.buffer,
            &self.texture,
            &self.descriptor,
            false,
        )
    }
}

pub fn receive(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    descriptor: &Descriptor,
    _: &CpuSurfaceSessionDescriptor,
) -> Result<wgpu::Texture> {
    let source = unsafe {
        OpenProcess(
            PROCESS_DUP_HANDLE,
            false,
            u32::try_from(descriptor.exporter_pid)?,
        )
    }
    .context("open supervised DX12 exporter")?;
    let source = unsafe { OwnedHandle::from_raw_handle(source.0) };
    let value = usize::try_from(descriptor.handle).context("DX12 shared handle is out of range")?;
    anyhow::ensure!(
        value != 0 && value != usize::MAX,
        "invalid DX12 shared handle"
    );
    let mut handle = HANDLE::default();
    unsafe {
        DuplicateHandle(
            HANDLE(source.as_raw_handle()),
            HANDLE(value as *mut _),
            GetCurrentProcess(),
            &mut handle,
            0,
            false,
            DUPLICATE_SAME_ACCESS,
        )
    }
    .context("duplicate DX12 preview handle")?;
    let handle = unsafe { OwnedHandle::from_raw_handle(handle.0) };
    let hal =
        unsafe { device.as_hal::<wgpu::hal::api::Dx12>() }.context("preview device is not DX12")?;
    let mut resource: Option<ID3D12Resource> = None;
    unsafe {
        hal.raw_device()
            .OpenSharedHandle(HANDLE(handle.as_raw_handle()), &mut resource)
    }
    .context("open DX12 shared preview buffer")?;
    let resource = resource.context("DX12 returned no imported preview buffer")?;
    let description = unsafe { resource.GetDesc() };
    anyhow::ensure!(
        description.Dimension == D3D12_RESOURCE_DIMENSION_BUFFER
            && description.Width == descriptor.allocation_size,
        "DX12 preview buffer layout mismatch"
    );
    let buffer = wrap(device, resource, descriptor.allocation_size);
    let texture = super::texture(device, descriptor.width, descriptor.height);
    copy(device, queue, &buffer, &texture, descriptor, true)?;
    Ok(texture)
}
