use std::{
    collections::HashSet,
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{Descriptor, Format, SearchPaths, runtime::inspect};
use crate::{
    file_io::{app_data_dir, atomic_write_json, read_json},
    messages,
};

#[derive(Default, Serialize, Deserialize)]
struct Cache {
    version: u32,
    architecture: String,
    entries: Vec<CacheEntry>,
}

#[derive(Serialize, Deserialize)]
struct CacheEntry {
    path: PathBuf,
    format: Format,
    stamp: u128,
    plugins: Vec<Descriptor>,
    error: Option<String>,
}

fn cache_path() -> PathBuf {
    app_data_dir().join("native-plugin-cache.json")
}

fn stamp(path: &Path) -> u128 {
    let metadata = fs::symlink_metadata(path).ok();
    let own = metadata
        .as_ref()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |time| time.as_nanos());
    if !metadata.is_some_and(|metadata| metadata.is_dir()) {
        return own;
    }
    fs::read_dir(path)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| stamp(&entry.path()))
        .fold(own, u128::max)
}

fn candidates(
    root: &Path,
    format: Format,
    seen: &mut HashSet<PathBuf>,
    output: &mut Vec<(PathBuf, Format)>,
) {
    let Ok(path) = root.canonicalize() else {
        return;
    };
    if !seen.insert(path.clone()) {
        return;
    }
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    let matches = match format {
        Format::Vst3 => extension.eq_ignore_ascii_case("vst3"),
        Format::Clap => extension.eq_ignore_ascii_case("clap"),
        Format::Lv2 => extension.eq_ignore_ascii_case("lv2"),
        Format::Vst2 => {
            #[cfg(target_os = "macos")]
            {
                extension.eq_ignore_ascii_case("vst")
            }
            #[cfg(target_os = "windows")]
            {
                extension.eq_ignore_ascii_case("dll")
            }
            #[cfg(not(any(target_os = "windows", target_os = "macos")))]
            {
                extension.eq_ignore_ascii_case("so")
            }
        }
    };
    if matches {
        output.push((path, format));
    } else if path.is_dir() {
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                candidates(&entry.path(), format, seen, output);
            }
        }
    }
}

pub fn discover(paths: &SearchPaths) -> Vec<Descriptor> {
    scan(paths, false)
}

pub fn rescan(paths: &SearchPaths) -> Vec<Descriptor> {
    scan(paths, true)
}

fn scan(paths: &SearchPaths, force: bool) -> Vec<Descriptor> {
    let mut cache = if force {
        Cache::default()
    } else {
        read_json::<Cache>(&cache_path()).unwrap_or_default()
    };
    if cache.version != 2 || cache.architecture != std::env::consts::ARCH {
        cache = Cache {
            version: 2,
            architecture: std::env::consts::ARCH.into(),
            entries: Vec::new(),
        };
    }
    let mut files = Vec::new();
    for format in Format::ALL {
        let mut seen = HashSet::new();
        for root in paths.directories(format) {
            candidates(&root, format, &mut seen, &mut files);
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    for (path, format) in &files {
        let stamp = stamp(path);
        if cache
            .entries
            .iter()
            .any(|entry| &entry.path == path && entry.format == *format && entry.stamp == stamp)
        {
            continue;
        }
        cache
            .entries
            .retain(|entry| &entry.path != path || entry.format != *format);
        let result = scan_child(path, *format);
        let (plugins, error) = match result {
            Ok(plugins) => (plugins, None),
            Err(error) => (Vec::new(), Some(format!("{error:#}"))),
        };
        cache.entries.push(CacheEntry {
            path: path.clone(),
            format: *format,
            stamp,
            plugins,
            error,
        });
        let _ = atomic_write_json(&cache_path(), &cache);
    }
    cache.entries.retain(|entry| {
        files
            .iter()
            .any(|(path, format)| path == &entry.path && format == &entry.format)
    });
    let _ = atomic_write_json(&cache_path(), &cache);
    let mut plugins = Vec::new();
    let mut keys = HashSet::new();
    for entry in &cache.entries {
        if !files
            .iter()
            .any(|(path, format)| path == &entry.path && format == &entry.format)
        {
            continue;
        }
        if let Some(error) = &entry.error {
            messages::error(
                "Plugin scan",
                format!(
                    "{} plugin {} failed: {error}",
                    entry.format.label(),
                    entry.path.display()
                ),
            );
        }
        for plugin in &entry.plugins {
            if keys.insert(plugin.key()) {
                plugins.push(plugin.clone());
            }
        }
    }
    plugins
}

fn scan_child(path: &Path, format: Format) -> Result<Vec<Descriptor>> {
    let result_path = app_data_dir().join(format!("plugin-scan-{}.json", std::process::id()));
    let diagnostic_path = result_path.with_extension("log");
    fs::create_dir_all(app_data_dir()).context("create plugin scanner directory")?;
    let result = scan_child_inner(path, format, &result_path, &diagnostic_path);
    let diagnostics = if result.is_err() {
        let mut text = String::new();
        if let Ok(mut file) = fs::File::open(&diagnostic_path) {
            let _ = file
                .seek(SeekFrom::End(-8192))
                .or_else(|_| file.seek(SeekFrom::Start(0)));
            let mut bytes = Vec::new();
            if file.take(8192).read_to_end(&mut bytes).is_ok() {
                text = String::from_utf8_lossy(&bytes).trim().to_owned();
            }
        }
        text
    } else {
        String::new()
    };
    let _ = fs::remove_file(&result_path);
    let _ = fs::remove_file(&diagnostic_path);
    if diagnostics.is_empty() {
        result
    } else {
        result.with_context(|| diagnostics)
    }
}

fn scan_child_inner(
    path: &Path,
    format: Format,
    result_path: &Path,
    diagnostic_path: &Path,
) -> Result<Vec<Descriptor>> {
    let _ = fs::remove_file(&result_path);
    let mut child = Command::new(std::env::current_exe()?)
        .arg("--kama-worker=plugin-scan")
        .arg(path)
        .arg(format.label())
        .arg(&result_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(fs::File::create(diagnostic_path).context("create plugin scanner diagnostic log")?)
        .spawn()
        .context("start plugin scanner")?;
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            anyhow::ensure!(
                status.success(),
                "scanner crashed or rejected plugin ({status})"
            );
            let result =
                read_json::<Vec<Descriptor>>(&result_path).context("read scanner metadata");
            let _ = fs::remove_file(&result_path);
            return result;
        }
        if start.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("scanner timed out; quarantined until plugin changes");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

pub fn run_scanner() -> Result<()> {
    let mut arguments = std::env::args_os()
        .skip_while(|argument| argument != "--kama-worker=plugin-scan")
        .skip(1);
    let path = PathBuf::from(arguments.next().context("missing plugin path")?);
    let format = arguments.next().context("missing plugin format")?;
    let format = Format::ALL
        .into_iter()
        .find(|candidate| format == candidate.label())
        .context("unknown plugin format")?;
    let result = PathBuf::from(arguments.next().context("missing scanner result path")?);
    atomic_write_json(&result, &inspect(&path, format)?)
}
