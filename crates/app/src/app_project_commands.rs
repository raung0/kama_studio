use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::Result;

use crate::{
    app_events::remember_recent_project,
    app_shared::{missing_project_media, sanitize_file_name},
    model3d,
    project::{MediaKind, Project},
    project_io, EditorApp, MissingMediaDialog, Modal, PendingDiscardAction,
    MEDIA_PRESENCE_CHECK_INTERVAL,
};

impl EditorApp {
    pub(super) fn new_project_unchecked(&mut self) {
        self.editor.project = Project::new();
        self.editor.project_path = None;
        self.next_media_presence_check = Instant::now() + MEDIA_PRESENCE_CHECK_INTERVAL;
        self.reset_project_runtime("New project");
        self.mark_document_saved();
        self.update_window_title();
    }

    pub(super) fn reset_project_runtime(&mut self, history_label: &str) {
        self.editor
            .timeline
            .load_document(self.editor.project.active_composition().timeline.clone());
        self.editor
            .timeline
            .ensure_composition_visual_pipelines(&self.plugins);
        self.audio.clear();
        self.media.clear_selection();
        self.waveform_textures.clear();
        self.editor
            .history
            .reset(&self.editor.project, &self.editor.timeline, history_label);
        self.editor.history_gesture = None;
        self.effects.rebuild(&self.editor.project.pipelines);
        self.playback.clear_caches();
        self.monitor.clear_captured_frame();
        self.playback
            .sync_compiled_effects(&self.renderer, &self.effects, &self.plugins);
        self.sync_inactive_windows_after_project_reset();
        self.request_redraw_all();
    }

    pub(super) fn open_project_dialog(&mut self) {
        self.request_discard_action(PendingDiscardAction::OpenProjectDialog);
    }

    pub(super) fn open_project_dialog_unchecked(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Kama Project", &["kama"])
            .pick_file()
        else {
            return;
        };
        let _ = self.load_project_unchecked(&path);
    }

    pub(super) fn load_project(&mut self, path: &Path) {
        self.request_discard_action(PendingDiscardAction::LoadProject(path.to_path_buf()));
    }

    pub(super) fn load_project_unchecked(&mut self, path: &Path) -> Result<()> {
        let project = project_io::load(path)?;
        let missing = missing_project_media(&project);
        if missing.is_empty() {
            self.finish_project_load(path.to_path_buf(), project);
        } else {
            self.open_modal(Modal::MissingMedia(MissingMediaDialog::new(
                path.to_path_buf(),
                project,
                missing,
            )));
        }
        Ok(())
    }

    pub(super) fn confirm_missing_media_load(&mut self, dialog: &mut MissingMediaDialog) {
        let Some(mut pending) = dialog.take() else {
            return;
        };
        let missing_ids = pending
            .missing
            .iter()
            .map(|(media, _)| *media)
            .collect::<HashSet<_>>();
        pending.project.remove_media(&missing_ids);
        self.finish_project_load(pending.path, pending.project);
    }

    pub(super) fn finish_project_load(&mut self, path: PathBuf, mut project: Project) {
        project.reconcile_plugin_metadata(&self.plugins);
        self.editor.project = project;
        self.reset_project_runtime("Project opened");
        self.warm_project_scrub_thumbnails();
        self.waveform_textures.queue_missing(&self.editor.project);
        self.editor
            .timeline
            .reconcile_pipeline_overrides(&self.editor.project);
        self.editor.project_path = Some(path.clone());
        self.next_media_presence_check = Instant::now() + MEDIA_PRESENCE_CHECK_INTERVAL;
        for asset in &self.editor.project.media {
            if matches!(asset.kind, MediaKind::WasmPlugin) {
                let _ = self.playback.precompile_wasm(&asset.path);
            }
        }
        self.mark_document_saved();
        remember_recent_project(&path);
        #[cfg(target_os = "macos")]
        self.native_menu.refresh_recent_projects();
        self.update_window_title();
    }

    pub(super) fn save_project(&mut self) {
        let path = self.editor.project_path.clone();
        if let Some(path) = path {
            let _ = self.save_project_to(&path);
        } else {
            self.save_project_as();
        }
    }

    pub(super) fn save_project_as(&mut self) {
        let suggested = format!("{}.kama", sanitize_file_name(&self.editor.project.name));
        let Some(mut path) = rfd::FileDialog::new()
            .add_filter("Kama Project", &["kama"])
            .set_file_name(suggested)
            .save_file()
        else {
            return;
        };
        if !path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("kama"))
        {
            path.set_extension("kama");
        }
        let _ = self.save_project_to(&path);
    }

    pub(super) fn save_project_to(&mut self, path: &Path) -> Result<()> {
        self.editor
            .project
            .sync_active_timeline(self.editor.timeline.document());
        if let Some(name) = path.file_stem().and_then(|name| name.to_str()) {
            self.editor.project.name = name.to_string();
        }
        project_io::save(&self.editor.project, path)?;
        self.editor.project_path = Some(path.to_path_buf());
        self.mark_document_saved();
        remember_recent_project(path);
        #[cfg(target_os = "macos")]
        self.native_menu.refresh_recent_projects();
        self.update_window_title();
        Ok(())
    }

    pub(super) fn import_media_dialog(&mut self) {
        let mut extensions = [
            "png", "jpg", "jpeg", "webp", "gif", "bmp", "tif", "tiff", "tga", "mp4", "mov", "mkv",
            "webm", "avi", "m4v", "wav", "mp3", "flac", "aac", "ogg", "m4a", "wasm",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
        extensions.extend(model3d::supported_extensions());
        extensions.sort();
        extensions.dedup();
        let Some(paths) = rfd::FileDialog::new()
            .add_filter("Media", &extensions)
            .pick_files()
        else {
            return;
        };
        for path in paths {
            let _ = self.import_path(path);
        }
    }
}
