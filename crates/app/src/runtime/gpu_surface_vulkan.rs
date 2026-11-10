use std::{
    io::{Read, Write},
    mem,
    os::{
        fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd},
        unix::net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use ash::{khr::external_memory_fd, vk};

use super::{CpuSurfaceSessionDescriptor, Descriptor, GpuIdentity};

static NEXT_CHANNEL: AtomicU64 = AtomicU64::new(1);

pub fn identity(device: &wgpu::Device) -> Result<GpuIdentity> {
    let hal = unsafe { device.as_hal::<wgpu::hal::api::Vulkan>() }
        .context("preview device is not Vulkan")?;
    anyhow::ensure!(
        hal.enabled_device_extensions()
            .contains(&external_memory_fd::NAME),
        "Vulkan external-memory FD extension is unavailable"
    );
    let mut id = vk::PhysicalDeviceIDProperties::default();
    let mut properties = vk::PhysicalDeviceProperties2::default().push_next(&mut id);
    unsafe {
        hal.shared_instance()
            .raw_instance()
            .get_physical_device_properties2(hal.raw_physical_device(), &mut properties);
    };
    Ok(GpuIdentity::Vulkan {
        device: id.device_uuid,
        driver: id.driver_uuid,
    })
}

struct Buffer {
    device: wgpu::Device,
    raw: ash::Device,
    handle: vk::Buffer,
    memory: vk::DeviceMemory,
}

impl Drop for Buffer {
    fn drop(&mut self) {
        unsafe {
            self.raw.destroy_buffer(self.handle, None);
            if self.memory != vk::DeviceMemory::null() {
                self.raw.free_memory(self.memory, None);
            }
        }
    }
}

fn buffer(
    device: &wgpu::Device,
    size: u64,
    import: Option<(&Descriptor, OwnedFd)>,
) -> Result<(Buffer, u64, u32)> {
    let hal = unsafe { device.as_hal::<wgpu::hal::api::Vulkan>() }
        .context("preview device is not Vulkan")?;
    let instance = hal.shared_instance().raw_instance();
    let physical = hal.raw_physical_device();
    let external = vk::PhysicalDeviceExternalBufferInfo::default()
        .usage(vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST)
        .handle_type(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
    let mut properties = vk::ExternalBufferProperties::default();
    unsafe {
        instance.get_physical_device_external_buffer_properties(
            physical,
            &external,
            &mut properties,
        );
    };
    anyhow::ensure!(
        properties
            .external_memory_properties
            .external_memory_features
            .contains(
                vk::ExternalMemoryFeatureFlags::EXPORTABLE
                    | vk::ExternalMemoryFeatureFlags::IMPORTABLE
            ),
        "Vulkan device cannot share transfer buffers"
    );
    let mut info = vk::ExternalMemoryBufferCreateInfo::default()
        .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
    let create = vk::BufferCreateInfo::default()
        .size(
            import
                .as_ref()
                .map_or(size, |(descriptor, _)| descriptor.allocation_size),
        )
        .usage(vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .push_next(&mut info);
    let raw = hal.raw_device().clone();
    let handle =
        unsafe { raw.create_buffer(&create, None) }.context("create external Vulkan buffer")?;
    let mut result = Buffer {
        device: device.clone(),
        raw,
        handle,
        memory: vk::DeviceMemory::null(),
    };
    let mut requirements = unsafe { result.raw.get_buffer_memory_requirements(handle) };
    if import.is_none() && requirements.size != size {
        let allocation_size = requirements.size;
        unsafe { result.raw.destroy_buffer(result.handle, None) };
        result.handle = vk::Buffer::null();
        let create = create.size(allocation_size);
        result.handle = unsafe { result.raw.create_buffer(&create, None) }
            .context("create aligned external Vulkan buffer")?;
        requirements = unsafe { result.raw.get_buffer_memory_requirements(result.handle) };
        anyhow::ensure!(
            requirements.size == allocation_size,
            "external Vulkan buffer allocation cannot be fully initialized"
        );
    }
    anyhow::ensure!(
        requirements.size <= 512 * 1024 * 1024,
        "external Vulkan allocation exceeds 512 MiB"
    );
    let handle = result.handle;
    let memory_properties = unsafe { instance.get_physical_device_memory_properties(physical) };
    let memory_type = if let Some((descriptor, _)) = &import {
        anyhow::ensure!(
            descriptor.allocation_size == requirements.size
                && descriptor.memory_type < memory_properties.memory_type_count
                && requirements.memory_type_bits & (1 << descriptor.memory_type) != 0,
            "incompatible Vulkan external allocation"
        );
        descriptor.memory_type
    } else {
        memory_properties
            .memory_types_as_slice()
            .iter()
            .enumerate()
            .find(|(index, memory)| {
                requirements.memory_type_bits & (1 << index) != 0
                    && memory
                        .property_flags
                        .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
            })
            .map(|(index, _)| index as u32)
            .context("no device-local external buffer memory")?
    };
    let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().buffer(handle);
    let mut export = vk::ExportMemoryAllocateInfo::default()
        .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
    let mut import_info = vk::ImportMemoryFdInfoKHR::default();
    let mut allocation = vk::MemoryAllocateInfo::default()
        .allocation_size(requirements.size)
        .memory_type_index(memory_type)
        .push_next(&mut dedicated);
    if let Some((_, fd)) = &import {
        import_info = import_info
            .handle_type(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD)
            .fd(fd.as_raw_fd());
        allocation = allocation.push_next(&mut import_info);
    } else {
        allocation = allocation.push_next(&mut export);
    }
    result.memory = unsafe { result.raw.allocate_memory(&allocation, None) }
        .context("allocate external Vulkan memory")?;
    if let Some((_, fd)) = import {
        let _ = fd.into_raw_fd();
    }
    unsafe { result.raw.bind_buffer_memory(handle, result.memory, 0) }
        .context("bind external Vulkan memory")?;
    Ok((result, requirements.size, memory_type))
}

fn copy(
    buffer: &Arc<Buffer>,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    descriptor: &Descriptor,
    importing: bool,
    acquire: bool,
) -> Result<()> {
    let device = &buffer.device;
    let hal = unsafe { device.as_hal::<wgpu::hal::api::Vulkan>() }
        .context("preview device is not Vulkan")?;
    let family = hal.queue_family_index();
    let image = {
        let image = unsafe { texture.as_hal::<wgpu::hal::api::Vulkan>() }
            .context("preview texture is not Vulkan")?;
        unsafe { image.raw_handle() }
    };
    let transitions = super::copy_encoder(device, texture, importing);
    queue.submit([transitions.finish()]);
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("Kama native Vulkan shared-buffer copy"),
    });
    unsafe {
        encoder.as_hal_mut::<wgpu::hal::api::Vulkan, _, _>(|encoder| -> Result<()> {
            let command = encoder
                .context("Vulkan preview encoder unavailable")?
                .raw_handle();
            let access = if importing {
                vk::AccessFlags::TRANSFER_READ
            } else {
                vk::AccessFlags::TRANSFER_WRITE
            };
            if acquire {
                let barrier = vk::BufferMemoryBarrier::default()
                    .src_queue_family_index(vk::QUEUE_FAMILY_EXTERNAL)
                    .dst_queue_family_index(family)
                    .src_access_mask(vk::AccessFlags::empty())
                    .dst_access_mask(access)
                    .buffer(buffer.handle)
                    .offset(0)
                    .size(vk::WHOLE_SIZE);
                buffer.raw.cmd_pipeline_barrier(
                    command,
                    vk::PipelineStageFlags::TOP_OF_PIPE,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[barrier],
                    &[],
                );
            }
            if !importing {
                buffer
                    .raw
                    .cmd_fill_buffer(command, buffer.handle, 0, vk::WHOLE_SIZE, 0);
                let barrier = vk::BufferMemoryBarrier::default()
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                    .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                    .buffer(buffer.handle)
                    .offset(0)
                    .size(vk::WHOLE_SIZE);
                buffer.raw.cmd_pipeline_barrier(
                    command,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[barrier],
                    &[],
                );
            }
            let region = vk::BufferImageCopy::default()
                .buffer_row_length(descriptor.stride / 8)
                .buffer_image_height(descriptor.height)
                .image_subresource(
                    vk::ImageSubresourceLayers::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .layer_count(1),
                )
                .image_extent(vk::Extent3D {
                    width: descriptor.width,
                    height: descriptor.height,
                    depth: 1,
                });
            if importing {
                buffer.raw.cmd_copy_buffer_to_image(
                    command,
                    buffer.handle,
                    image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[region],
                );
            } else {
                buffer.raw.cmd_copy_image_to_buffer(
                    command,
                    image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    buffer.handle,
                    &[region],
                );
            }
            let barrier = vk::BufferMemoryBarrier::default()
                .src_queue_family_index(family)
                .dst_queue_family_index(vk::QUEUE_FAMILY_EXTERNAL)
                .src_access_mask(access)
                .dst_access_mask(vk::AccessFlags::empty())
                .buffer(buffer.handle)
                .offset(0)
                .size(vk::WHOLE_SIZE);
            buffer.raw.cmd_pipeline_barrier(
                command,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::empty(),
                &[],
                &[barrier],
                &[],
            );
            Ok(())
        })
    }?;
    let resources = (Arc::clone(buffer), texture.clone());
    let submission = queue.submit([encoder.finish()]);
    queue.on_submitted_work_done(move || drop(resources));
    super::wait(device, submission)
}

struct Endpoint {
    stop: Arc<AtomicBool>,
    wake: UnixStream,
    thread: Option<thread::JoinHandle<()>>,
    path: PathBuf,
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.wake.write_all(&[1]);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

fn peer(stream: &UnixStream, expected_pid: i32) -> Result<()> {
    let mut credential: libc::ucred = unsafe { mem::zeroed() };
    let mut length = mem::size_of::<libc::ucred>() as libc::socklen_t;
    let status = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credential).cast(),
            &raw mut length,
        )
    };
    anyhow::ensure!(
        status == 0
            && credential.pid == expected_pid
            && credential.uid == unsafe { libc::geteuid() },
        "GPU surface socket peer mismatch"
    );
    Ok(())
}

fn send_fd(stream: &UnixStream, fd: &OwnedFd) -> Result<()> {
    let mut byte = [1u8];
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let space = unsafe { libc::CMSG_SPACE(mem::size_of::<i32>() as u32) } as usize;
    let mut control = vec![0usize; space.div_ceil(mem::size_of::<usize>())];
    let mut message: libc::msghdr = unsafe { mem::zeroed() };
    message.msg_iov = &raw mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = space;
    unsafe {
        let header = libc::CMSG_FIRSTHDR(&raw const message);
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(mem::size_of::<i32>() as u32) as usize;
        let raw_fd = fd.as_raw_fd();
        std::ptr::copy_nonoverlapping(
            (&raw const raw_fd).cast::<u8>(),
            libc::CMSG_DATA(header),
            mem::size_of::<i32>(),
        );
        anyhow::ensure!(
            libc::sendmsg(stream.as_raw_fd(), &raw const message, libc::MSG_NOSIGNAL) == 1,
            "send external GPU memory FD failed"
        );
    }
    Ok(())
}

fn receive_fd(stream: &UnixStream) -> Result<OwnedFd> {
    let mut byte = [0u8];
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let space = unsafe { libc::CMSG_SPACE(mem::size_of::<i32>() as u32) } as usize;
    let mut control = vec![0usize; space.div_ceil(mem::size_of::<usize>())];
    let mut message: libc::msghdr = unsafe { mem::zeroed() };
    message.msg_iov = &raw mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = space;
    unsafe {
        let count = libc::recvmsg(stream.as_raw_fd(), &raw mut message, libc::MSG_CMSG_CLOEXEC);
        let mut header = libc::CMSG_FIRSTHDR(&raw const message);
        let mut descriptors = Vec::new();
        while !header.is_null() {
            if (*header).cmsg_level == libc::SOL_SOCKET && (*header).cmsg_type == libc::SCM_RIGHTS {
                let length = (*header)
                    .cmsg_len
                    .saturating_sub(libc::CMSG_LEN(0) as usize);
                let bytes = std::slice::from_raw_parts(libc::CMSG_DATA(header), length);
                for bytes in bytes.as_chunks::<{ mem::size_of::<i32>() }>().0 {
                    let value = i32::from_ne_bytes(*bytes);
                    descriptors.push(OwnedFd::from_raw_fd(value));
                }
            }
            header = libc::CMSG_NXTHDR(&raw const message, header);
        }
        anyhow::ensure!(
            count == 1
                && descriptors.len() == 1
                && byte == [1]
                && message.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) == 0,
            "invalid GPU memory FD message"
        );
        descriptors.pop().context("GPU memory FD is missing")
    }
}

fn endpoint(path: PathBuf, fd: OwnedFd, descriptor: &Descriptor) -> Result<Endpoint> {
    let (wake, wake_reader) = UnixStream::pair()?;
    wake.set_nonblocking(true)?;
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = Arc::clone(&stop);
    let listener = UnixListener::bind(&path).context("bind GPU surface socket")?;
    let mut endpoint = Endpoint {
        stop,
        wake,
        thread: None,
        path,
    };
    listener.set_nonblocking(true)?;
    let token = descriptor.session_token;
    let pid = descriptor.importer_pid;
    endpoint.thread = Some(
        thread::Builder::new()
            .name("kama-gpu-memory-fd".into())
            .spawn(move || {
                while !stopping.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let result = (|| -> Result<()> {
                                peer(&stream, pid)?;
                                stream.set_read_timeout(Some(Duration::from_millis(200)))?;
                                stream.set_write_timeout(Some(Duration::from_millis(200)))?;
                                let mut received = [0u8; 32];
                                stream.read_exact(&mut received)?;
                                anyhow::ensure!(
                                    received == token,
                                    "GPU surface session token mismatch"
                                );
                                send_fd(&stream, &fd)
                            })();
                            let _ = result;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            let mut descriptors = [
                                libc::pollfd {
                                    fd: listener.as_raw_fd(),
                                    events: libc::POLLIN,
                                    revents: 0,
                                },
                                libc::pollfd {
                                    fd: wake_reader.as_raw_fd(),
                                    events: libc::POLLIN,
                                    revents: 0,
                                },
                            ];
                            let status = unsafe { libc::poll(descriptors.as_mut_ptr(), 2, -1) };
                            if status < 0
                                && std::io::Error::last_os_error().kind()
                                    != std::io::ErrorKind::Interrupted
                            {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            })?,
    );
    Ok(endpoint)
}

pub struct SharedSurface {
    _endpoint: Endpoint,
    buffer: Arc<Buffer>,
    texture: wgpu::Texture,
    descriptor: Descriptor,
    published: bool,
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
        let (buffer, allocation_size, memory_type) = buffer(device, size, None)?;
        let hal = unsafe { device.as_hal::<wgpu::hal::api::Vulkan>() }
            .context("preview device is not Vulkan")?;
        let extension =
            external_memory_fd::Device::new(hal.shared_instance().raw_instance(), hal.raw_device());
        let info = vk::MemoryGetFdInfoKHR::default()
            .memory(buffer.memory)
            .handle_type(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
        let fd = unsafe {
            OwnedFd::from_raw_fd(
                extension
                    .get_memory_fd(&info)
                    .context("export Vulkan memory FD")?,
            )
        };
        let descriptor = Descriptor {
            channel: format!(
                "gpu-{}-{}",
                std::process::id(),
                NEXT_CHANNEL.fetch_add(1, Ordering::Relaxed)
            ),
            width,
            height,
            stride,
            allocation_size,
            memory_type,
            handle: 0,
            gpu,
            session_token: session.session_token,
            exporter_pid: session.exporter_pid,
            importer_pid: session.parent_pid,
        };
        let endpoint = endpoint(session.directory.join(&descriptor.channel), fd, &descriptor)?;
        Ok(Self {
            _endpoint: endpoint,
            buffer: Arc::new(buffer),
            texture: super::texture(device, width, height),
            descriptor,
            published: false,
        })
    }

    pub const fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }
    pub const fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    pub fn publish(&mut self, _: &wgpu::Device, queue: &wgpu::Queue) -> Result<()> {
        copy(
            &self.buffer,
            queue,
            &self.texture,
            &self.descriptor,
            false,
            self.published,
        )?;
        self.published = true;
        Ok(())
    }
}

pub fn receive(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    descriptor: &Descriptor,
    session: &CpuSurfaceSessionDescriptor,
) -> Result<wgpu::Texture> {
    let mut stream = UnixStream::connect(session.directory.join(&descriptor.channel))
        .context("connect GPU surface socket")?;
    peer(&stream, descriptor.exporter_pid)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(&descriptor.session_token)?;
    let fd = receive_fd(&stream)?;
    let (_, size) = super::layout(descriptor.width, descriptor.height)?;
    let (buffer, _, _) = buffer(device, size, Some((descriptor, fd)))?;
    let buffer = Arc::new(buffer);
    let texture = super::texture(device, descriptor.width, descriptor.height);
    copy(&buffer, queue, &texture, descriptor, true, true)?;
    Ok(texture)
}
