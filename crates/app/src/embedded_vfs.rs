use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use anyhow::{Context, Result};
use rust_embed::RustEmbed;
use vfs::{EmbeddedFS, VfsPath};

pub(crate) const BUILTIN_PLUGIN_ROOT: &str = "builtin:/plugins";
const BUILTIN_PLUGIN_PREFIX: &str = "builtin:/plugins/";

#[derive(Debug, RustEmbed)]
#[folder = "$OUT_DIR/embedded-plugins"]
struct BuiltinPlugins;

fn root() -> &'static VfsPath {
    static ROOT: OnceLock<VfsPath> = OnceLock::new();
    ROOT.get_or_init(|| VfsPath::new(EmbeddedFS::<BuiltinPlugins>::new()))
}

fn normalize(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn embedded_path(path: &Path) -> Result<Option<VfsPath>> {
    let normalized = normalize(path);
    let Some(relative) = normalized.strip_prefix(BUILTIN_PLUGIN_PREFIX) else {
        return Ok(None);
    };
    Ok(Some(root().join(relative).with_context(|| {
        format!("resolve embedded path {normalized}")
    })?))
}

fn virtual_path(path: &VfsPath) -> PathBuf {
    PathBuf::from(format!("{BUILTIN_PLUGIN_ROOT}{}", path.as_str()))
}

pub(crate) fn read(path: &Path) -> Result<Option<Vec<u8>>> {
    let Some(path) = embedded_path(path)? else {
        return Ok(None);
    };
    if !path.is_file().context("stat embedded plugin file")? {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    path.open_file()
        .context("open embedded plugin file")?
        .read_to_end(&mut bytes)
        .context("read embedded plugin file")?;
    Ok(Some(bytes))
}

pub(crate) fn read_to_string(path: &Path) -> Result<Option<String>> {
    let Some(path) = embedded_path(path)? else {
        return Ok(None);
    };
    if !path.is_file().context("stat embedded plugin file")? {
        return Ok(None);
    }
    Ok(Some(
        path.read_to_string()
            .context("read embedded plugin file as UTF-8")?,
    ))
}

pub(crate) fn fingerprint(path: &Path) -> Option<u64> {
    let bytes = read(path).ok().flatten()?;
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    Some(hash)
}

pub(crate) fn plugin_manifests(path: &Path) -> Result<Option<Vec<PathBuf>>> {
    if normalize(path) != BUILTIN_PLUGIN_ROOT {
        return Ok(None);
    }

    let root = root();
    let mut manifests = Vec::new();
    let direct_manifest = root
        .join("plugin.toml")
        .context("resolve embedded plugin manifest")?;
    if direct_manifest
        .is_file()
        .context("stat embedded plugin manifest")?
    {
        manifests.push(virtual_path(&direct_manifest));
    }
    for child in root.read_dir().context("read embedded plugin directory")? {
        if !child.is_dir().context("stat embedded plugin directory")? {
            continue;
        }
        let manifest = child
            .join("plugin.toml")
            .context("resolve embedded plugin manifest")?;
        if manifest
            .is_file()
            .context("stat embedded plugin manifest")?
        {
            manifests.push(virtual_path(&manifest));
        }
    }
    Ok(Some(manifests))
}
