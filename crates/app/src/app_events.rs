use std::path::{Path, PathBuf};

use crate::file_io::{app_data_dir, atomic_write_json, read_json};

#[cfg(target_os = "macos")]
use muda::MenuEvent;

pub(super) enum AppEvent {
    Interrupt,
    #[cfg(target_os = "macos")]
    Menu(MenuEvent),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum FileCommand {
    NewProject,
    Save,
    SaveAs,
    Load,
    LoadRecent(PathBuf),
    ImportMedia,
    Exit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum LayoutCommand {
    Save,
    SaveNamed(String),
    Load(PathBuf),
    Delete(PathBuf),
    RestoreDefault,
}

#[derive(Clone, Debug)]
pub(super) struct SavedLayout {
    pub(super) name: String,
    pub(super) path: PathBuf,
}

pub(super) fn layout_data_dir() -> PathBuf {
    app_data_dir().join("layouts")
}

pub(super) fn recent_projects_path() -> PathBuf {
    app_data_dir().join("recent-projects.json")
}

pub(super) fn recent_projects() -> Vec<PathBuf> {
    read_json::<Vec<PathBuf>>(&recent_projects_path())
        .unwrap_or_default()
        .into_iter()
        .filter(|path| path.exists())
        .take(10)
        .collect()
}

pub(super) fn remember_recent_project(path: &Path) {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut recent = recent_projects();
    recent.retain(|candidate| candidate != &path);
    recent.insert(0, path);
    recent.truncate(10);
    let _ = atomic_write_json(&recent_projects_path(), &recent);
}

pub(super) fn saved_layouts() -> Vec<SavedLayout> {
    let mut layouts = std::fs::read_dir(layout_data_dir())
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension().and_then(|extension| extension.to_str()) == Some("kama-layout")).then(
                || SavedLayout {
                    name: path
                        .file_stem()
                        .and_then(|name| name.to_str())
                        .unwrap_or("Layout")
                        .to_string(),
                    path,
                },
            )
        })
        .collect::<Vec<_>>();
    layouts.sort_by_key(|layout| layout.name.to_lowercase());
    layouts
}
