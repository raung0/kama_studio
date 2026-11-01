use std::{
    fs::{self, OpenOptions},
    hash::{Hash, Hasher},
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering, fence},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use memmap2::MmapMut;
use serde::{Deserialize, Serialize};

pub const SLOT_COUNT: usize = 3;
const MAX_DIMENSION: u32 = 16_384;
const MAX_SLOT_BYTES: u64 = 256 * 1024 * 1024;
const BYTES_PER_PIXEL: u64 = 8;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CpuSurfaceSessionDescriptor {
    pub directory: PathBuf,
    pub session_token: [u8; 32],
    pub parent_pid: i32,
    pub exporter_pid: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CpuSurfaceDescriptor {
    pub session_token: [u8; 32],
    pub slot_id: u8,
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub length: u64,
    pub parent_pid: i32,
    pub exporter_pid: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CpuCaptureDescriptor {
    pub session_token: [u8; 32],
    pub file_id: u64,
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub length: u64,
    pub parent_pid: i32,
    pub exporter_pid: i32,
}

struct MappedSlot {
    descriptor: CpuSurfaceDescriptor,
    map: MmapMut,
}

struct MappedCapture {
    identity: (usize, usize, [u32; 2], u64),
    descriptor: CpuCaptureDescriptor,
    _map: MmapMut,
}

pub struct CpuSurfaceSession {
    descriptor: CpuSurfaceSessionDescriptor,
    slots: Mutex<Vec<MappedSlot>>,
    captures: Mutex<Vec<MappedCapture>>,
    next_capture_id: AtomicU64,
}

impl CpuSurfaceSession {
    pub fn create(parent_pid: i32, exporter_pid: i32) -> Result<Self> {
        anyhow::ensure!(
            parent_pid > 1 && exporter_pid > 1,
            "invalid CPU surface process IDs"
        );
        let token = new_session_token(parent_pid, exporter_pid);
        let directory = std::env::temp_dir().join(format!(
            "kama-video-cpu-{}-{}",
            parent_pid,
            token
                .iter()
                .take(6)
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = fs::DirBuilder::new();
            builder
                .mode(0o700)
                .create(&directory)
                .with_context(|| format!("create CPU surface session {}", directory.display()))?;
        }
        #[cfg(not(unix))]
        {
            fs::create_dir(&directory)
                .with_context(|| format!("create CPU surface session {}", directory.display()))?;
        }
        Ok(Self {
            descriptor: CpuSurfaceSessionDescriptor {
                directory,
                session_token: token,
                parent_pid,
                exporter_pid,
            },
            slots: Mutex::new(Vec::new()),
            captures: Mutex::new(Vec::new()),
            next_capture_id: AtomicU64::new(1),
        })
    }

    pub fn descriptor(&self) -> CpuSurfaceSessionDescriptor {
        self.descriptor.clone()
    }

    pub fn prepare(&self, size: [u32; 2]) -> Result<Vec<CpuSurfaceDescriptor>> {
        let [width, height] = size;
        validate_dimensions(width, height)?;
        let stride = width
            .checked_mul(BYTES_PER_PIXEL as u32)
            .context("CPU surface row stride overflow")?;
        let length = u64::from(stride)
            .checked_mul(u64::from(height))
            .context("CPU surface length overflow")?;
        anyhow::ensure!(
            length <= MAX_SLOT_BYTES,
            "CPU surface exceeds the 256 MiB slot limit"
        );

        let mut slots = self
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slots.len() == SLOT_COUNT
            && slots
                .iter()
                .all(|slot| slot.descriptor.width == width && slot.descriptor.height == height)
        {
            return Ok(slots.iter().map(|slot| slot.descriptor.clone()).collect());
        }
        slots.clear();
        for slot_id in 0..SLOT_COUNT {
            let path = self
                .descriptor
                .directory
                .join(format!("slot-{slot_id}.rgba16"));
            let _ = fs::remove_file(&path);
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
                .with_context(|| format!("create CPU surface slot {}", path.display()))?;
            file.set_len(length)
                .with_context(|| format!("size CPU surface slot {}", path.display()))?;
            let map = unsafe { MmapMut::map_mut(&file) }
                .with_context(|| format!("map CPU surface slot {}", path.display()))?;
            let descriptor = CpuSurfaceDescriptor {
                session_token: self.descriptor.session_token,
                slot_id: u8::try_from(slot_id).context("CPU surface slot ID overflow")?,
                path,
                width,
                height,
                stride,
                length,
                parent_pid: self.descriptor.parent_pid,
                exporter_pid: self.descriptor.exporter_pid,
            };
            slots.push(MappedSlot { descriptor, map });
        }
        Ok(slots.iter().map(|slot| slot.descriptor.clone()).collect())
    }

    pub fn read_frame(
        &self,
        frame: &CpuSurfaceDescriptor,
        expected_exporter_pid: u32,
    ) -> Result<Vec<u8>> {
        frame.validate()?;
        anyhow::ensure!(
            frame.session_token == self.descriptor.session_token,
            "CPU surface session token mismatch"
        );
        anyhow::ensure!(
            frame.parent_pid == self.descriptor.parent_pid,
            "CPU surface parent PID mismatch"
        );
        anyhow::ensure!(
            frame.exporter_pid == i32::try_from(expected_exporter_pid).unwrap_or(i32::MAX),
            "CPU surface exporter PID mismatch"
        );
        let slots = self
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = slots
            .get(usize::from(frame.slot_id))
            .context("unknown CPU surface slot")?;
        anyhow::ensure!(
            slot.descriptor == *frame,
            "CPU surface slot identity or dimensions changed"
        );
        fence(Ordering::Acquire);
        Ok(slot.map[..usize::try_from(frame.length)?].to_vec())
    }

    pub fn cache_capture(
        &self,
        size: [u32; 2],
        pixels: &[u8],
        identity_generation: u64,
        preserve: Option<&CpuCaptureDescriptor>,
    ) -> Result<CpuCaptureDescriptor> {
        let [width, height] = size;
        validate_dimensions(width, height)?;
        let stride = width
            .checked_mul(BYTES_PER_PIXEL as u32)
            .context("CPU capture row stride overflow")?;
        let length = u64::from(stride)
            .checked_mul(u64::from(height))
            .context("CPU capture length overflow")?;
        anyhow::ensure!(
            length <= MAX_SLOT_BYTES,
            "captured frame exceeds the 256 MiB limit"
        );
        anyhow::ensure!(
            pixels.len() == usize::try_from(length)?,
            "captured RGBA16 frame length mismatch"
        );
        let identity = (
            pixels.as_ptr() as usize,
            pixels.len(),
            size,
            identity_generation,
        );
        let mut captures = self
            .captures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(capture) = captures.iter().find(|capture| capture.identity == identity) {
            let descriptor = capture.descriptor.clone();
            prune_captures(&mut captures, preserve, Some(&descriptor));
            return Ok(descriptor);
        }
        prune_captures(&mut captures, preserve, None);
        let file_id = self.next_capture_id.fetch_add(1, Ordering::Relaxed);
        let path = self
            .descriptor
            .directory
            .join(format!("capture-{file_id}.rgba16"));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| format!("create captured frame backing file {}", path.display()))?;
        file.set_len(length)
            .context("size captured frame backing file")?;
        let mut map = unsafe { MmapMut::map_mut(&file) }
            .with_context(|| format!("map captured frame backing file {}", path.display()))?;
        map.copy_from_slice(pixels);
        fence(Ordering::Release);
        let descriptor = CpuCaptureDescriptor {
            session_token: self.descriptor.session_token,
            file_id,
            path,
            width,
            height,
            stride,
            length,
            parent_pid: self.descriptor.parent_pid,
            exporter_pid: self.descriptor.exporter_pid,
        };
        captures.push(MappedCapture {
            identity,
            descriptor: descriptor.clone(),
            _map: map,
        });
        Ok(descriptor)
    }

    pub fn retain_captures(&self, keep: &[CpuCaptureDescriptor]) {
        let mut captures = self
            .captures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        captures.retain(|capture| {
            if keep.contains(&capture.descriptor) {
                true
            } else {
                let _ = fs::remove_file(&capture.descriptor.path);
                false
            }
        });
    }
}

impl CpuSurfaceDescriptor {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.slot_id < SLOT_COUNT as u8,
            "invalid CPU surface slot ID"
        );
        anyhow::ensure!(
            self.parent_pid > 1 && self.exporter_pid > 1,
            "invalid CPU surface PID"
        );
        validate_dimensions(self.width, self.height)?;
        let min_stride = self
            .width
            .checked_mul(BYTES_PER_PIXEL as u32)
            .context("CPU surface stride overflow")?;
        anyhow::ensure!(
            self.stride >= min_stride,
            "CPU surface row stride is too small"
        );
        let expected_length = u64::from(self.stride)
            .checked_mul(u64::from(self.height))
            .context("CPU surface length overflow")?;
        anyhow::ensure!(
            self.length == expected_length && self.length <= MAX_SLOT_BYTES,
            "invalid CPU surface length"
        );
        anyhow::ensure!(
            self.path.file_name().and_then(|name| name.to_str())
                == Some(format!("slot-{}.rgba16", self.slot_id).as_str()),
            "CPU surface path does not identify its slot"
        );
        Ok(())
    }
}

pub fn write_frame(
    session: &CpuSurfaceSessionDescriptor,
    slot: &CpuSurfaceDescriptor,
    pixels: &[u8],
    exporter_pid: u32,
) -> Result<()> {
    slot.validate()?;
    anyhow::ensure!(
        slot.session_token == session.session_token,
        "CPU surface session token mismatch"
    );
    anyhow::ensure!(
        slot.parent_pid == session.parent_pid && slot.exporter_pid == session.exporter_pid,
        "CPU surface session identity mismatch"
    );
    anyhow::ensure!(
        slot.exporter_pid == i32::try_from(exporter_pid).unwrap_or(i32::MAX),
        "CPU surface writer PID mismatch"
    );
    anyhow::ensure!(
        slot.path.parent() == Some(session.directory.as_path()),
        "CPU surface file is outside its session directory"
    );
    let metadata = fs::symlink_metadata(&slot.path).context("inspect CPU surface backing file")?;
    anyhow::ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "invalid CPU surface backing file"
    );
    let compact_row = usize::try_from(u64::from(slot.width) * BYTES_PER_PIXEL)?;
    anyhow::ensure!(
        pixels.len()
            == compact_row
                .checked_mul(slot.height as usize)
                .context("CPU frame size overflow")?,
        "CPU frame pixel length mismatch"
    );
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&slot.path)
        .with_context(|| format!("open parent-owned CPU surface slot {}", slot.path.display()))?;
    anyhow::ensure!(
        file.metadata()?.len() == slot.length,
        "CPU surface backing file size changed"
    );
    let mut map = unsafe { MmapMut::map_mut(&file) }
        .with_context(|| format!("map parent-owned CPU surface slot {}", slot.path.display()))?;
    let stride = usize::try_from(slot.stride)?;
    for (source, destination) in pixels
        .chunks_exact(compact_row)
        .zip(map.chunks_exact_mut(stride))
    {
        destination[..compact_row].copy_from_slice(source);
    }
    fence(Ordering::Release);
    Ok(())
}

pub fn validate_session(
    session: &CpuSurfaceSessionDescriptor,
    expected_parent_pid: i32,
) -> Result<()> {
    anyhow::ensure!(
        session.parent_pid == expected_parent_pid,
        "CPU surface parent PID mismatch"
    );
    anyhow::ensure!(session.exporter_pid > 1, "invalid CPU surface exporter PID");
    anyhow::ensure!(
        session.directory.is_absolute(),
        "CPU surface session directory must be absolute"
    );
    let metadata = fs::symlink_metadata(&session.directory)
        .context("inspect CPU surface session directory")?;
    anyhow::ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "invalid CPU surface session directory"
    );
    Ok(())
}

pub fn read_capture(
    session: &CpuSurfaceSessionDescriptor,
    capture: &CpuCaptureDescriptor,
    expected_exporter_pid: u32,
) -> Result<Vec<u8>> {
    validate_session(session, capture.parent_pid)?;
    validate_dimensions(capture.width, capture.height)?;
    anyhow::ensure!(
        capture.session_token == session.session_token,
        "captured frame session token mismatch"
    );
    anyhow::ensure!(
        capture.exporter_pid == session.exporter_pid,
        "captured frame worker PID mismatch"
    );
    anyhow::ensure!(
        capture.exporter_pid == i32::try_from(expected_exporter_pid).unwrap_or(i32::MAX),
        "captured frame exporter PID mismatch"
    );
    anyhow::ensure!(
        capture.path.parent() == Some(session.directory.as_path()),
        "captured frame is outside its session directory"
    );
    let expected_name = format!("capture-{}.rgba16", capture.file_id);
    anyhow::ensure!(
        capture.path.file_name().and_then(|name| name.to_str()) == Some(expected_name.as_str()),
        "captured frame path identity mismatch"
    );
    let stride = capture
        .width
        .checked_mul(BYTES_PER_PIXEL as u32)
        .context("captured frame row overflow")?;
    anyhow::ensure!(capture.stride == stride, "captured frame stride mismatch");
    let length = u64::from(stride)
        .checked_mul(u64::from(capture.height))
        .context("captured frame size overflow")?;
    anyhow::ensure!(
        capture.length == length && length <= MAX_SLOT_BYTES,
        "captured frame length is invalid"
    );
    let metadata =
        fs::symlink_metadata(&capture.path).context("inspect captured frame backing file")?;
    anyhow::ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() == length,
        "captured frame backing file is invalid"
    );
    fs::read(&capture.path).context("read parent-owned captured frame")
}

fn prune_captures(
    captures: &mut Vec<MappedCapture>,
    preserve: Option<&CpuCaptureDescriptor>,
    also_keep: Option<&CpuCaptureDescriptor>,
) {
    captures.retain(|capture| {
        if preserve.is_some_and(|descriptor| descriptor == &capture.descriptor)
            || also_keep.is_some_and(|descriptor| descriptor == &capture.descriptor)
        {
            true
        } else {
            let _ = fs::remove_file(&capture.descriptor.path);
            false
        }
    });
}

pub fn readback_rgba16(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
) -> Result<Vec<u8>> {
    validate_dimensions(width, height)?;
    anyhow::ensure!(
        texture.format() == wgpu::TextureFormat::Rgba16Float,
        "CPU preview readback requires RGBA16Float"
    );
    let row_bytes = u64::from(width) * BYTES_PER_PIXEL;
    let padded = row_bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as u64)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as u64;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Kama CPU preview readback"),
        size: padded * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("Kama CPU preview readback"),
    });
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(u32::try_from(padded)?),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    let submission = queue.submit([encoder.finish()]);
    let slice = buffer.slice(..);
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: Some(std::time::Duration::from_secs(5)),
        })
        .context("wait for CPU preview GPU readback")?;
    receiver
        .recv_timeout(std::time::Duration::from_secs(5))
        .context("CPU preview readback callback timed out")??;
    let mapped = slice
        .get_mapped_range()
        .context("access mapped CPU preview readback")?;
    let mut pixels = Vec::with_capacity(usize::try_from(row_bytes * u64::from(height))?);
    for row in mapped
        .chunks_exact(usize::try_from(padded)?)
        .take(height as usize)
    {
        pixels.extend_from_slice(&row[..usize::try_from(row_bytes)?]);
    }
    drop(mapped);
    buffer.unmap();
    Ok(pixels)
}

fn validate_dimensions(width: u32, height: u32) -> Result<()> {
    anyhow::ensure!(
        (1..=MAX_DIMENSION).contains(&width) && (1..=MAX_DIMENSION).contains(&height),
        "invalid CPU surface dimensions"
    );
    let bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(BYTES_PER_PIXEL))
        .context("CPU surface dimensions overflow")?;
    anyhow::ensure!(
        bytes <= MAX_SLOT_BYTES,
        "CPU surface exceeds the 256 MiB slot limit"
    );
    Ok(())
}

fn new_session_token(parent_pid: i32, exporter_pid: i32) -> [u8; 32] {
    static NEXT_TOKEN: AtomicU64 = AtomicU64::new(0);
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut token = [0_u8; 32];
    for index in 0..4_u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (
            stamp,
            parent_pid,
            exporter_pid,
            NEXT_TOKEN.fetch_add(1, Ordering::Relaxed),
            index,
        )
            .hash(&mut hasher);
        token[index as usize * 8..index as usize * 8 + 8]
            .copy_from_slice(&hasher.finish().to_le_bytes());
    }
    token
}

impl Drop for CpuSurfaceSession {
    fn drop(&mut self) {
        if let Ok(slots) = self.slots.get_mut() {
            slots.clear();
        }
        if let Ok(captures) = self.captures.get_mut() {
            captures.clear();
        }
        let _ = fs::remove_dir_all(&self.descriptor.directory);
    }
}

#[cfg(test)]
mod tests {
    use super::{CpuSurfaceSession, SLOT_COUNT, read_capture, write_frame};

    #[test]
    fn parent_owned_slots_are_bounded_validated_and_resize_without_stale_identity() {
        let pid = i32::try_from(std::process::id()).unwrap();
        let session = CpuSurfaceSession::create(pid, pid).unwrap();
        let first = session.prepare([2, 3]).unwrap();
        assert_eq!(first.len(), SLOT_COUNT);
        assert!(
            first
                .iter()
                .all(|slot| slot.stride == 16 && slot.length == 48)
        );
        let pixels = (0_u8..48).collect::<Vec<_>>();
        write_frame(
            &session.descriptor(),
            &first[1],
            &pixels,
            std::process::id(),
        )
        .unwrap();
        assert_eq!(
            session.read_frame(&first[1], std::process::id()).unwrap(),
            pixels
        );

        let resized = session.prepare([4, 2]).unwrap();
        assert_eq!(resized.len(), SLOT_COUNT);
        assert!(
            resized
                .iter()
                .all(|slot| slot.stride == 32 && slot.length == 64)
        );
        assert!(session.read_frame(&first[1], std::process::id()).is_err());
    }

    #[test]
    fn cpu_surface_validation_rejects_oversized_dimensions_before_allocation() {
        let pid = i32::try_from(std::process::id()).unwrap();
        let session = CpuSurfaceSession::create(pid, pid).unwrap();
        assert!(session.prepare([8_192, 8_192]).is_err());
        assert!(session.prepare([0, 12]).is_err());
    }

    #[test]
    fn clearing_and_replacing_a_capture_invalidates_pointer_cache_identity() {
        let pid = i32::try_from(std::process::id()).unwrap();
        let session = CpuSurfaceSession::create(pid, pid).unwrap();
        let mut pixels = vec![0x35; 2 * 2 * 8];
        let first = session.cache_capture([2, 2], &pixels, 1, None).unwrap();
        session.retain_captures(&[]);
        pixels.fill(0xc2);
        let second = session.cache_capture([2, 2], &pixels, 3, None).unwrap();

        assert_ne!(first.file_id, second.file_id);
        assert!(!first.path.exists());
        assert_eq!(
            read_capture(&session.descriptor(), &second, std::process::id()).unwrap(),
            pixels
        );
    }
}
