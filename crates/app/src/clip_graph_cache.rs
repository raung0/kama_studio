use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        mpsc::{self, SyncSender, TrySendError},
    },
    thread,
};

use anyhow::{Context, Result, ensure};
use flate2::{Compression, read::GzDecoder, write::GzEncoder};

use crate::{
    file_io::{commit_if_absent, temporary_path},
    runtime::video::CpuFrame,
};

const FRAME_MAGIC: &[u8; 8] = b"KCGF\0\0\0\x01";
const MAX_FRAME_BYTES: usize = 512 * 1024 * 1024;
const MAX_TEXT_BYTES: usize = 16 * 1024 * 1024;
const WRITE_QUEUE_CAPACITY: usize = 2;

enum CacheWrite {
    Frame {
        key: u64,
        frame: Arc<CpuFrame>,
    },
    Text {
        namespace: &'static str,
        key: u64,
        text: String,
    },
}

fn cache_root() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Library/Caches/kama/clip-graphs")
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("LOCALAPPDATA")
            .or_else(|| std::env::var_os("APPDATA"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("kama/cache/clip-graphs")
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .map(|home| home.join(".cache"))
            })
            .unwrap_or_else(|| PathBuf::from("."))
            .join("kama/clip-graphs")
    }
}

fn frame_path(key: u64) -> PathBuf {
    cache_root()
        .join("frames")
        .join(format!("{key:016x}.rgba32f.gz"))
}

fn text_path(namespace: &str, key: u64) -> PathBuf {
    cache_root()
        .join(namespace)
        .join(format!("{key:016x}.txt.gz"))
}

fn writer() -> Option<&'static SyncSender<CacheWrite>> {
    static WRITER: OnceLock<Option<SyncSender<CacheWrite>>> = OnceLock::new();
    WRITER
        .get_or_init(|| {
            let (tx, rx) = mpsc::sync_channel(WRITE_QUEUE_CAPACITY);
            match thread::Builder::new()
                .name("kama-clip-graph-cache".into())
                .spawn(move || {
                    while let Ok(write) = rx.recv() {
                        let result = match write {
                            CacheWrite::Frame { key, frame } => write_frame(key, &frame),
                            CacheWrite::Text {
                                namespace,
                                key,
                                text,
                            } => write_text(namespace, key, &text),
                        };
                        let _ = result;
                    }
                }) {
                Ok(_) => Some(tx),
                Err(_) => None,
            }
        })
        .as_ref()
}

fn enqueue(write: CacheWrite) {
    let Some(writer) = writer() else {
        return;
    };
    match writer.try_send(write) {
        Ok(()) | Err(TrySendError::Full(_)) => {}
        Err(TrySendError::Disconnected(_)) => {}
    }
}

pub(crate) fn store_frame_async(key: u64, frame: Arc<CpuFrame>) {
    let expected = (frame.width as usize)
        .checked_mul(frame.height as usize)
        .and_then(|pixels| pixels.checked_mul(4));
    if expected == Some(frame.pixels.len())
        && frame.pixels.len() <= MAX_FRAME_BYTES / std::mem::size_of::<f32>()
    {
        enqueue(CacheWrite::Frame { key, frame });
    }
}

pub(crate) fn load_frame(key: u64) -> Option<CpuFrame> {
    let path = frame_path(key);
    let result = read_frame(&path);
    if result.is_err() && path.exists() {
        let _ = fs::remove_file(path);
    }
    result.ok()
}

pub(crate) fn store_text_async(namespace: &'static str, key: u64, text: String) {
    if text.len() <= MAX_TEXT_BYTES {
        enqueue(CacheWrite::Text {
            namespace,
            key,
            text,
        });
    }
}

pub(crate) fn load_text(namespace: &'static str, key: u64) -> Option<String> {
    let path = text_path(namespace, key);
    let result = read_text(&path);
    if result.is_err() && path.exists() {
        let _ = fs::remove_file(path);
    }
    result.ok()
}

fn read_frame(path: &Path) -> Result<CpuFrame> {
    let file = File::open(path)?;
    let mut decoder = GzDecoder::new(file);
    let mut magic = [0u8; 8];
    decoder.read_exact(&mut magic)?;
    ensure!(
        &magic == FRAME_MAGIC,
        "unsupported clip graph frame cache version"
    );

    let width = read_u32(&mut decoder)?;
    let height = read_u32(&mut decoder)?;
    let pixel_len = usize::try_from(read_u64(&mut decoder)?)
        .context("clip graph frame cache pixel count overflow")?;
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .context("clip graph frame cache dimensions overflow")?;
    let byte_len = pixel_len
        .checked_mul(std::mem::size_of::<f32>())
        .context("clip graph frame cache byte count overflow")?;
    ensure!(
        pixel_len == expected && byte_len <= MAX_FRAME_BYTES,
        "invalid clip graph frame cache dimensions"
    );

    let mut pixels = vec![0.0f32; pixel_len];
    decoder.read_exact(bytemuck::cast_slice_mut(&mut pixels))?;
    Ok(CpuFrame::from_pixels(width, height, pixels))
}

fn write_frame(key: u64, frame: &CpuFrame) -> Result<()> {
    write_compressed(&frame_path(key), |encoder| {
        encoder.write_all(FRAME_MAGIC)?;
        encoder.write_all(&frame.width.to_le_bytes())?;
        encoder.write_all(&frame.height.to_le_bytes())?;
        encoder.write_all(&(frame.pixels.len() as u64).to_le_bytes())?;
        encoder.write_all(bytemuck::cast_slice(&frame.pixels))?;
        Ok(())
    })
}

fn read_text(path: &Path) -> Result<String> {
    let file = File::open(path)?;
    let decoder = GzDecoder::new(file);
    let mut limited = decoder.take((MAX_TEXT_BYTES + 1) as u64);
    let mut text = String::new();
    limited.read_to_string(&mut text)?;
    ensure!(
        text.len() <= MAX_TEXT_BYTES,
        "clip graph text cache exceeds size limit"
    );
    Ok(text)
}

fn write_text(namespace: &str, key: u64, text: &str) -> Result<()> {
    write_compressed(&text_path(namespace, key), |encoder| {
        encoder.write_all(text.as_bytes())?;
        Ok(())
    })
}

fn write_compressed(
    path: &Path,
    write: impl FnOnce(&mut GzEncoder<File>) -> Result<()>,
) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    fs::create_dir_all(parent)?;
    let temporary = temporary_path(path);
    let result = (|| {
        let file = File::create(&temporary)?;
        let mut encoder = GzEncoder::new(file, Compression::fast());
        write(&mut encoder)?;
        encoder.finish()?.sync_all()?;
        commit_if_absent(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn read_u32(reader: &mut impl Read) -> Result<u32> {
    let mut bytes = [0u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64(reader: &mut impl Read) -> Result<u64> {
    let mut bytes = [0u8; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}
