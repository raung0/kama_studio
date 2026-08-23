use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result};

pub(crate) fn app_data_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join("Library/Application Support"));
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".local/share"))
        });
    base.unwrap_or_else(|| PathBuf::from(".")).join("kama")
}

pub(crate) fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    fs::read(path)
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok())
}

pub(crate) fn atomic_write_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create directory {}", parent.display()))?;
    }
    let data = serde_json::to_vec_pretty(value).context("serialize JSON")?;
    atomic_write(path, &data)
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = temporary_path(path);
    let result = (|| {
        let mut file = File::create(&temporary)
            .with_context(|| format!("create temporary file {}", temporary.display()))?;
        file.write_all(bytes)
            .with_context(|| format!("write temporary file {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("sync temporary file {}", temporary.display()))?;
        replace_file(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub(crate) fn replace_file(temporary: &Path, destination: &Path) -> Result<()> {
    replace_file_impl(temporary, destination)
}

pub(crate) fn commit_if_absent(temporary: &Path, destination: &Path) -> Result<()> {
    if destination.exists() {
        let _ = fs::remove_file(temporary);
        return Ok(());
    }
    match fs::rename(temporary, destination) {
        Ok(()) => Ok(()),
        Err(_error) if destination.exists() => {
            let _ = fs::remove_file(temporary);
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
fn replace_file_impl(temporary: &Path, destination: &Path) -> Result<()> {
    fs::rename(temporary, destination).with_context(|| format!("replace {}", destination.display()))
}

#[cfg(not(unix))]
fn replace_file_impl(temporary: &Path, destination: &Path) -> Result<()> {
    if !destination.exists() {
        return fs::rename(temporary, destination)
            .with_context(|| format!("commit {}", destination.display()));
    }
    let backup = temporary_path(destination);
    let _ = fs::remove_file(&backup);
    fs::rename(destination, &backup)
        .with_context(|| format!("stage previous {}", destination.display()))?;
    if let Err(error) = fs::rename(temporary, destination) {
        let _ = fs::rename(&backup, destination);
        return Err(error).with_context(|| format!("replace {}", destination.display()));
    }
    let _ = fs::remove_file(backup);
    Ok(())
}

pub(crate) fn temporary_path(path: &Path) -> PathBuf {
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(
        ".tmp-{}-{}",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    PathBuf::from(temporary)
}
