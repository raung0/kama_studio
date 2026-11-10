#[cfg(not(target_os = "macos"))]
use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[cfg(target_os = "macos")]
const RGBA16_FLOAT_FOURCC: u32 = 0x5247_6841;
#[cfg(target_os = "macos")]
const MAX_SHARED_DIMENSION: u32 = 16_384;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SharedSurfaceDescriptor {
    pub channel: String,
    pub session_token: [u8; 32],
    pub width: u32,
    pub height: u32,
    pub pixel_format: u32,
    pub metal_registry_id: u64,
    pub importer_pid: i32,
    pub exporter_pid: i32,
}

#[cfg(target_os = "macos")]
impl SharedSurfaceDescriptor {
    fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            !self.channel.is_empty() && self.channel.len() < 192 && self.channel.is_ascii(),
            "invalid shared-surface channel"
        );
        anyhow::ensure!(
            (1..=MAX_SHARED_DIMENSION).contains(&self.width)
                && (1..=MAX_SHARED_DIMENSION).contains(&self.height),
            "invalid shared-surface dimensions"
        );
        anyhow::ensure!(
            self.pixel_format == RGBA16_FLOAT_FOURCC,
            "unsupported shared-surface pixel format"
        );
        anyhow::ensure!(self.importer_pid > 1, "invalid shared-surface importer PID");
        anyhow::ensure!(self.exporter_pid > 1, "invalid shared-surface exporter PID");
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::{
        ffi::{CStr, CString, c_char, c_void},
        ptr,
        time::Duration,
    };

    use anyhow::{Context, Result};
    use objc2::{rc::Retained, runtime::ProtocolObject};
    use objc2_metal::{MTLDevice, MTLTexture, MTLTextureType};

    use super::SharedSurfaceDescriptor;

    const CHANNEL_CAPACITY: usize = 192;
    const METAL_TEXTURE_USAGE: wgpu::TextureUsages = wgpu::TextureUsages::TEXTURE_BINDING
        .union(wgpu::TextureUsages::STORAGE_BINDING)
        .union(wgpu::TextureUsages::RENDER_ATTACHMENT)
        .union(wgpu::TextureUsages::COPY_SRC);

    #[repr(C)]
    struct NativeSurfaceInfo {
        channel: [c_char; CHANNEL_CAPACITY],
        token: [u8; 32],
        width: u32,
        height: u32,
        pixel_format: u32,
        registry_id: u64,
        importer_pid: i32,
        exporter_pid: i32,
    }

    const _: () = assert!(std::mem::size_of::<NativeSurfaceInfo>() == 256);

    unsafe extern "C" {
        fn kama_shared_surface_create(
            metal_device: *mut c_void,
            width: u32,
            height: u32,
            peer_pid: i32,
            info: *mut NativeSurfaceInfo,
            surface_out: *mut *mut c_void,
            texture_out: *mut *mut c_void,
        ) -> i32;
        fn kama_shared_surface_receive(
            metal_device: *mut c_void,
            info: *const NativeSurfaceInfo,
            timeout_ms: u32,
            surface_out: *mut *mut c_void,
            texture_out: *mut *mut c_void,
        ) -> i32;
        fn kama_shared_surface_release(surface: *mut c_void);
        fn kama_shared_surface_release_texture(texture: *mut c_void);
    }

    struct NativeSurface(*mut c_void);

    impl Drop for NativeSurface {
        fn drop(&mut self) {
            unsafe { kama_shared_surface_release(self.0) };
        }
    }

    pub struct SharedSurface {
        texture: Option<wgpu::Texture>,
        _native: NativeSurface,
        descriptor: SharedSurfaceDescriptor,
    }

    impl SharedSurface {
        pub fn allocate(
            device: &wgpu::Device,
            width: u32,
            height: u32,
            parent_pid: i32,
        ) -> Result<Self> {
            anyhow::ensure!(
                (1..=super::MAX_SHARED_DIMENSION).contains(&width)
                    && (1..=super::MAX_SHARED_DIMENSION).contains(&height),
                "shared-surface dimensions must be in 1..={}",
                super::MAX_SHARED_DIMENSION
            );
            anyhow::ensure!(parent_pid > 1, "invalid parent PID");

            let metal_device = metal_device_pointer(device)?;
            let mut info = NativeSurfaceInfo {
                channel: [0; CHANNEL_CAPACITY],
                token: [0; 32],
                width: 0,
                height: 0,
                pixel_format: 0,
                registry_id: 0,
                importer_pid: 0,
                exporter_pid: 0,
            };
            let mut native = ptr::null_mut();
            let mut raw_texture = ptr::null_mut();
            let status = unsafe {
                kama_shared_surface_create(
                    metal_device,
                    width,
                    height,
                    parent_pid,
                    &raw mut info,
                    &raw mut native,
                    &raw mut raw_texture,
                )
            };
            anyhow::ensure!(
                status == 0,
                "create IOSurface/Mach endpoint failed: {status}"
            );
            let native = NativeSurface(native);
            let texture = match unsafe {
                wrap_texture(
                    device,
                    raw_texture,
                    width,
                    height,
                    wgpu::TextureUses::UNINITIALIZED,
                )
            } {
                Ok(texture) => texture,
                Err(error) => {
                    // `wrap_texture` consumes this retain only after accepting a non-null pointer.
                    unsafe { kama_shared_surface_release_texture(raw_texture) };
                    return Err(error);
                }
            };
            let descriptor = descriptor_from_native(&info)?;
            Ok(Self {
                texture: Some(texture),
                _native: native,
                descriptor,
            })
        }

        pub const fn descriptor(&self) -> &SharedSurfaceDescriptor {
            &self.descriptor
        }

        #[expect(
            clippy::expect_used,
            reason = "the texture is present until ownership is explicitly transferred"
        )]
        pub const fn texture(&self) -> &wgpu::Texture {
            self.texture
                .as_ref()
                .expect("surface texture remains owned")
        }
    }

    impl Drop for SharedSurface {
        fn drop(&mut self) {
            // The IOSurface may back this texture, so release the HAL wrapper before its native owner.
            drop(self.texture.take());
        }
    }

    pub struct ImportedSurface {
        texture: Option<wgpu::Texture>,
        _native: NativeSurface,
    }

    impl ImportedSurface {
        pub fn receive(
            device: &wgpu::Device,
            descriptor: &SharedSurfaceDescriptor,
            expected_exporter_pid: i32,
            timeout: Duration,
        ) -> Result<Self> {
            descriptor.validate()?;
            anyhow::ensure!(
                descriptor.exporter_pid == expected_exporter_pid,
                "shared-surface exporter PID does not match the supervised child"
            );
            anyhow::ensure!(
                !timeout.is_zero() && timeout <= Duration::from_secs(2),
                "shared-surface receive timeout must be at most two seconds"
            );
            let metal_device = metal_device_pointer(device)?;
            let info = native_info_from_descriptor(descriptor)?;
            let mut native = ptr::null_mut();
            let mut raw_texture = ptr::null_mut();
            let timeout_ms = u32::try_from(timeout.as_millis())
                .unwrap_or(2_000)
                .clamp(1, 2_000);
            let status = unsafe {
                kama_shared_surface_receive(
                    metal_device,
                    &raw const info,
                    timeout_ms,
                    &raw mut native,
                    &raw mut raw_texture,
                )
            };
            anyhow::ensure!(status == 0, "receive/import IOSurface failed: {status}");
            let native = NativeSurface(native);
            let texture = match unsafe {
                wrap_texture(
                    device,
                    raw_texture,
                    descriptor.width,
                    descriptor.height,
                    wgpu::TextureUses::STORAGE_WRITE_ONLY,
                )
            } {
                Ok(texture) => texture,
                Err(error) => {
                    unsafe { kama_shared_surface_release_texture(raw_texture) };
                    return Err(error);
                }
            };
            Ok(Self {
                texture: Some(texture),
                _native: native,
            })
        }

        #[expect(
            clippy::expect_used,
            reason = "the imported texture is present until ownership is explicitly transferred"
        )]
        pub const fn texture(&self) -> &wgpu::Texture {
            self.texture
                .as_ref()
                .expect("imported texture remains owned")
        }

        #[expect(
            clippy::expect_used,
            reason = "receive constructs this value with a texture and transfers it exactly once"
        )]
        pub fn into_texture(mut self) -> wgpu::Texture {
            self.texture.take().expect("imported texture remains owned")
        }
    }

    impl Drop for ImportedSurface {
        fn drop(&mut self) {
            drop(self.texture.take());
        }
    }

    fn descriptor_from_native(info: &NativeSurfaceInfo) -> Result<SharedSurfaceDescriptor> {
        let channel = unsafe { CStr::from_ptr(info.channel.as_ptr()) }
            .to_str()
            .context("native shared-surface channel is not UTF-8")?
            .to_owned();
        let descriptor = SharedSurfaceDescriptor {
            channel,
            session_token: info.token,
            width: info.width,
            height: info.height,
            pixel_format: info.pixel_format,
            metal_registry_id: info.registry_id,
            importer_pid: info.importer_pid,
            exporter_pid: info.exporter_pid,
        };
        descriptor.validate()?;
        Ok(descriptor)
    }

    fn native_info_from_descriptor(
        descriptor: &SharedSurfaceDescriptor,
    ) -> Result<NativeSurfaceInfo> {
        let channel = CString::new(descriptor.channel.as_str()).context("channel contains NUL")?;
        let bytes = channel.as_bytes_with_nul();
        anyhow::ensure!(
            bytes.len() <= CHANNEL_CAPACITY,
            "shared-surface channel is too long"
        );
        let mut native_channel = [0; CHANNEL_CAPACITY];
        for (target, source) in native_channel.iter_mut().zip(bytes) {
            *target = c_char::try_from(*source).context("channel byte exceeds c_char")?;
        }
        Ok(NativeSurfaceInfo {
            channel: native_channel,
            token: descriptor.session_token,
            width: descriptor.width,
            height: descriptor.height,
            pixel_format: descriptor.pixel_format,
            registry_id: descriptor.metal_registry_id,
            importer_pid: descriptor.importer_pid,
            exporter_pid: descriptor.exporter_pid,
        })
    }

    fn metal_device_pointer(device: &wgpu::Device) -> Result<*mut c_void> {
        // SAFETY: the device is only inspected for its backend-native Metal device handle.
        let hal = unsafe { device.as_hal::<wgpu::hal::api::Metal>() }
            .context("wgpu device is not backed by Metal")?;
        let pointer = Retained::as_ptr(hal.raw_device()) as *mut c_void;
        anyhow::ensure!(!pointer.is_null(), "wgpu Metal device handle is null");
        Ok(pointer)
    }

    pub fn metal_registry_id(device: &wgpu::Device) -> Result<u64> {
        let hal = unsafe { device.as_hal::<wgpu::hal::api::Metal>() }
            .context("wgpu device is not backed by Metal")?;
        Ok(hal.raw_device().registryID())
    }

    unsafe fn wrap_texture(
        device: &wgpu::Device,
        raw_texture: *mut c_void,
        width: u32,
        height: u32,
        initial_state: wgpu::TextureUses,
    ) -> Result<wgpu::Texture> {
        // SAFETY: the bridge returns a retained Metal texture for this device and these validated dimensions/usages.
        let retained =
            unsafe { Retained::<ProtocolObject<dyn MTLTexture>>::from_raw(raw_texture.cast()) }
                .context("native bridge returned a null MTLTexture")?;
        // SAFETY: the retained native object and descriptor metadata satisfy wgpu-hal's Metal texture import requirements.
        let hal_texture = unsafe {
            wgpu::hal::metal::Device::texture_from_raw(
                retained,
                wgpu::TextureFormat::Rgba16Float,
                MTLTextureType::Type2D,
                1,
                1,
                wgpu::hal::CopyExtent {
                    width,
                    height,
                    depth: 1,
                },
                None,
            )
        };
        // SAFETY: the imported HAL texture is on this device; storage-write initial state preserves producer contents on receive.
        Ok(unsafe {
            device.create_texture_from_hal::<wgpu::hal::api::Metal>(
                hal_texture,
                &wgpu::TextureDescriptor {
                    label: Some("Kama cross-process RGBA16Float IOSurface"),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba16Float,
                    usage: METAL_TEXTURE_USAGE,
                    view_formats: &[],
                },
                initial_state,
            )
        })
    }

    pub use ImportedSurface as PlatformImportedSurface;
    pub use SharedSurface as PlatformSharedSurface;

    #[cfg(test)]
    mod tests {
        use std::{
            io::{BufRead, Write},
            process::{Command, Stdio},
            thread,
            time::{Duration, Instant},
        };

        use super::{ImportedSurface, SharedSurface};

        fn metal_device() -> (wgpu::Instance, wgpu::Device, wgpu::Queue) {
            let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
            descriptor.backends = wgpu::Backends::METAL;
            let instance = wgpu::Instance::new(descriptor);
            let adapter = pollster::block_on(
                instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
            )
            .expect("Metal adapter available");
            let (device, queue) =
                pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                    .expect("request Metal device");
            (instance, device, queue)
        }

        fn wait_bounded(
            child: &mut std::process::Child,
            timeout: Duration,
        ) -> std::process::ExitStatus {
            let deadline = Instant::now()
                .checked_add(timeout)
                .unwrap_or_else(Instant::now);
            loop {
                if let Some(status) = child.try_wait().expect("poll subprocess") {
                    return status;
                }
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("GPU surface subprocess exceeded {timeout:?}");
                }
                thread::sleep(Duration::from_millis(5));
            }
        }

        #[test]
        fn shared_surface_child_entry() {
            if std::env::var_os("KAMA_SHARED_SURFACE_CHILD_TEST").is_none() {
                return;
            }
            let mut encoded_descriptor = String::new();
            std::io::stdin()
                .lock()
                .read_line(&mut encoded_descriptor)
                .expect("read descriptor from parent");
            let descriptor = serde_json::from_str::<super::super::SharedSurfaceDescriptor>(
                encoded_descriptor.trim(),
            )
            .expect("decode parent descriptor");
            let (_instance, device, queue) = metal_device();

            let mut bad_session = descriptor.clone();
            bad_session.session_token[0] ^= 0x80;
            assert!(
                ImportedSurface::receive(
                    &device,
                    &bad_session,
                    descriptor.exporter_pid,
                    Duration::from_millis(150),
                )
                .is_err()
            );

            let imported = ImportedSurface::receive(
                &device,
                &descriptor,
                descriptor.exporter_pid,
                Duration::from_secs(1),
            )
            .expect("authenticate and import IOSurface from parent");
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("cross-process IOSurface test readback"),
                size: 1_024,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut encoder =
                device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
            encoder.copy_texture_to_buffer(
                imported.texture().as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(256),
                        rows_per_image: Some(4),
                    },
                },
                wgpu::Extent3d {
                    width: 4,
                    height: 4,
                    depth_or_array_layers: 1,
                },
            );
            let submission = queue.submit([encoder.finish()]);
            let slice = buffer.slice(..);
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            slice.map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
            device
                .poll(wgpu::PollType::Wait {
                    submission_index: Some(submission),
                    timeout: Some(Duration::from_secs(5)),
                })
                .expect("bounded GPU readback wait");
            rx.recv_timeout(Duration::from_secs(1))
                .expect("readback callback")
                .expect("map imported pixels");
            let mapped = slice.get_mapped_range().expect("mapped GPU readback");
            let expected = [0, 52, 0, 56, 0, 58, 0, 60];
            for row in mapped.as_chunks::<256>().0.iter().take(4) {
                for pixel in row.as_chunks::<8>().0.iter().take(4) {
                    assert_eq!(pixel, &expected);
                }
            }
            println!("child verified all imported RGBA16Float pixels");
        }

        #[test]
        fn iosurface_mach_port_roundtrip_preserves_pixels_and_cleans_up() {
            let (_instance, device, queue) = metal_device();
            let executable = std::env::current_exe().expect("locate test executable");
            let mut child = Command::new(executable)
                .arg("--exact")
                .arg("runtime::shared_surface::platform::tests::shared_surface_child_entry")
                .arg("--nocapture")
                .env("KAMA_SHARED_SURFACE_CHILD_TEST", "1")
                .stdin(Stdio::piped())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .expect("spawn independent GPU consumer process");
            let child_pid = i32::try_from(child.id()).expect("child PID fits pid_t");
            let surface = SharedSurface::allocate(&device, 4, 4, child_pid)
                .expect("allocate IOSurface and authenticated Mach channel");

            let view = surface
                .texture()
                .create_view(&wgpu::TextureViewDescriptor::default());
            let mut encoder =
                device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
            {
                let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("deterministic shared-surface clear"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: 0.25,
                                g: 0.5,
                                b: 0.75,
                                a: 1.0,
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                });
            }
            let submission = queue.submit([encoder.finish()]);
            device
                .poll(wgpu::PollType::Wait {
                    submission_index: Some(submission),
                    timeout: Some(Duration::from_secs(5)),
                })
                .expect("producer clear completes before descriptor publication");

            let mut stdin = child.stdin.take().expect("child stdin pipe");
            serde_json::to_writer(&mut stdin, surface.descriptor()).expect("write descriptor");
            stdin.write_all(b"\n").expect("terminate descriptor line");
            drop(stdin);
            let status = wait_bounded(&mut child, Duration::from_secs(15));
            assert!(status.success(), "consumer process failed with {status}");

            let mut stale = surface.descriptor().clone();
            drop(surface);
            stale.importer_pid = i32::try_from(std::process::id()).unwrap();
            let lookup_started = Instant::now();
            assert!(
                ImportedSurface::receive(
                    &device,
                    &stale,
                    stale.exporter_pid,
                    Duration::from_millis(500),
                )
                .is_err(),
                "released service must no longer be importable"
            );
            assert!(
                lookup_started.elapsed() < Duration::from_millis(200),
                "released bootstrap service should fail lookup, not time out against a live endpoint"
            );
        }
    }
}

#[cfg(target_os = "macos")]
#[allow(unused_imports)]
pub use platform::{
    PlatformImportedSurface as ImportedSurface, PlatformSharedSurface as SharedSurface,
};

#[cfg(target_os = "macos")]
pub fn metal_registry_id(device: &wgpu::Device) -> Result<u64> {
    platform::metal_registry_id(device)
}

#[cfg(not(target_os = "macos"))]
pub enum SharedSurface {}

#[cfg(not(target_os = "macos"))]
impl SharedSurface {
    pub fn allocate(
        _device: &wgpu::Device,
        _width: u32,
        _height: u32,
        _parent_pid: i32,
    ) -> Result<Self> {
        anyhow::bail!("cross-process IOSurface sharing is only supported on macOS Metal")
    }

    pub const fn descriptor(&self) -> &SharedSurfaceDescriptor {
        match *self {}
    }

    pub const fn texture(&self) -> &wgpu::Texture {
        match *self {}
    }
}

#[cfg(not(target_os = "macos"))]
pub fn metal_registry_id(_device: &wgpu::Device) -> Result<u64> {
    anyhow::bail!("shared GPU video workers require macOS Metal IOSurface support")
}

#[cfg(not(target_os = "macos"))]
pub enum ImportedSurface {}

#[cfg(not(target_os = "macos"))]
impl ImportedSurface {
    pub fn receive(
        _device: &wgpu::Device,
        _descriptor: &SharedSurfaceDescriptor,
        _expected_exporter_pid: i32,
        _timeout: Duration,
    ) -> Result<Self> {
        anyhow::bail!("cross-process IOSurface sharing is only supported on macOS Metal")
    }

    pub fn into_texture(self) -> wgpu::Texture {
        match self {}
    }
}
