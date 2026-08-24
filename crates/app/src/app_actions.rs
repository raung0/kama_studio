use super::*;

impl EditorApp {
    pub(super) fn handle_inspector_action(&mut self, action: InspectorAction) {
        self.command_queue.push(EditorCommand::inspector(action));
    }

    pub(super) fn execute_inspector_action(&mut self, action: InspectorAction) {
        match action {
            InspectorAction::ChoosePipeline(anchor) => {
                self.palette.pending_open = Some((
                    PaletteKind::PipelineAssignment(self.editor.timeline.selected_pipeline_kind()),
                    Some(anchor),
                ));
            }
            InspectorAction::ChooseFont(anchor) => {
                self.palette.font_options = self.gui.font_families();
                self.palette.pending_open = Some((PaletteKind::FontFamily, Some(anchor)));
            }
            InspectorAction::CreatePipeline => {
                if !self.editor.timeline.can_assign_pipeline() {
                    return;
                }
                let id = self
                    .editor
                    .project
                    .create_pipeline_kind(self.editor.timeline.selected_pipeline_kind());
                self.editor.timeline.set_selected_pipeline(Some(id));
                self.sync_effect_runtime();
            }
            InspectorAction::AddEffect => {
                let kind = self.editor.timeline.selected_pipeline_kind();
                if self.selected_or_create_pipeline(kind).is_none() {
                    return;
                }
                self.effects.rebuild(&self.editor.project.pipelines);
                self.palette.pending_open = Some((
                    PaletteKind::AddEffect {
                        audio: kind == PipelineKind::Audio,
                    },
                    Some(Rect::new(self.cursor[0], self.cursor[1], 1.0, 1.0)),
                ));
            }
            InspectorAction::MoveEffect(node, direction) => {
                if let Some(pipeline) = self
                    .editor
                    .timeline
                    .selected_pipeline()
                    .and_then(|instance| instance.pipeline)
                {
                    if self
                        .editor
                        .project
                        .move_pipeline_node(pipeline, node, direction)
                    {
                        self.sync_effect_runtime();
                    }
                }
            }
            InspectorAction::RemoveEffect(node) => {
                if let Some(pipeline) = self
                    .editor
                    .timeline
                    .selected_pipeline()
                    .and_then(|instance| instance.pipeline)
                {
                    if self.editor.project.remove_pipeline_node(pipeline, node) {
                        self.editor
                            .timeline
                            .reconcile_pipeline_overrides(&self.editor.project);
                        self.sync_effect_runtime();
                    }
                }
            }
            InspectorAction::MakeIndependent => {
                if let Some(pipeline) = self
                    .editor
                    .timeline
                    .selected_pipeline()
                    .and_then(|instance| instance.pipeline)
                {
                    if let Some(duplicate) = self.editor.project.duplicate_pipeline(pipeline) {
                        self.editor
                            .timeline
                            .set_selected_pipeline_preserving_overrides(duplicate);
                        self.sync_effect_runtime();
                    }
                }
            }
            InspectorAction::OpenGraph => {
                self.pipeline_graph.follow_selection();
                self.open_panel(PanelKind::Pipeline);
            }
        }
    }

    pub(super) fn handle_timeline_action(&mut self, action: TimelineAction) {
        self.set_history_gesture_label(action.history_label());
        self.command_queue.push(EditorCommand::timeline(action));
    }

    pub(super) fn execute_timeline_action(&mut self, action: TimelineAction) {
        match action {
            TimelineAction::InsertVideoClip { track, time } => {
                self.palette.pending_open = Some((
                    PaletteKind::VideoClip { track, time },
                    Some(Rect::new(self.cursor[0], self.cursor[1], 1.0, 1.0)),
                ));
            }
            TimelineAction::InsertEffectClip { track, time } => {
                self.palette.pending_open = Some((
                    PaletteKind::EffectClip { track, time },
                    Some(Rect::new(self.cursor[0], self.cursor[1], 1.0, 1.0)),
                ));
            }
            TimelineAction::AddSelectionToComposition => {
                self.open_modal(Modal::Composition(NewCompositionDialog::new(
                    NewCompositionMode::FromSelection,
                )));
            }
            TimelineAction::SpeedDuration => {
                if self.editor.timeline.has_selection() {
                    self.open_modal(Modal::SpeedDuration(SpeedDurationDialog::new(
                        &self.editor.timeline,
                        &self.editor.project,
                    )));
                }
            }
            TimelineAction::ReplaceSelectedClips => {
                let Some((min_video_tracks, min_audio_tracks)) = self
                    .editor
                    .timeline
                    .selected_media_track_requirements(&self.editor.project)
                else {
                    return;
                };
                if !self
                    .editor
                    .timeline
                    .has_compatible_replacement_media(&self.editor.project)
                {
                    return;
                }
                self.palette.replacement_excluded_media = self.editor.timeline.selected_media_ids();
                self.palette.pending_open = Some((
                    PaletteKind::ReplaceSelectedClips {
                        min_video_tracks,
                        min_audio_tracks,
                    },
                    Some(Rect::new(self.cursor[0], self.cursor[1], 1.0, 1.0)),
                ));
            }
            action => {
                if self.editor.timeline.apply_action(action, &self.snapshot) {
                    self.playback.invalidate();
                }
            }
        }
    }

    pub(super) fn insert_media_asset_at(
        &mut self,
        asset: MediaAsset,
        stream: MediaStream,
        track: u32,
        time: f32,
    ) -> bool {
        match stream {
            MediaStream::All if matches!(asset.kind, MediaKind::WasmPlugin) => {
                let plugin_id = asset
                    .path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or("WASM Generator")
                    .to_string();
                self.plugins
                    .visual_pipeline_instance()
                    .is_ok_and(|pipeline| {
                        self.editor
                            .timeline
                            .insert_wasm_clip_at(track, time, asset.path, plugin_id, pipeline)
                    })
            }
            MediaStream::All if matches!(asset.kind, MediaKind::Video) => self
                .plugins
                .visual_pipeline_instance()
                .is_ok_and(|pipeline| {
                    self.editor.timeline.insert_av_media_clip_at(
                        (track, time),
                        asset.id,
                        asset.name,
                        asset.has_audio,
                        asset.duration,
                        pipeline,
                    )
                }),
            MediaStream::All | MediaStream::Video(_) => self
                .plugins
                .visual_pipeline_instance()
                .is_ok_and(|pipeline| {
                    self.editor.timeline.insert_media_clip_at(
                        (track, time),
                        asset.id,
                        asset.name,
                        false,
                        asset.duration,
                        pipeline,
                    )
                }),
            MediaStream::Audio(_) => self.editor.timeline.insert_media_clip_at(
                (track, time),
                asset.id,
                if matches!(asset.kind, MediaKind::Video) {
                    format!("{} - Audio", asset.name)
                } else {
                    asset.name
                },
                true,
                asset.duration,
                effects::PipelineInstance::effect_default(),
            ),
        }
    }

    pub(super) fn insert_media_drag_items(
        &mut self,
        items: &[MediaDragItem],
        anchor_track: u32,
        time: f32,
    ) -> bool {
        let initialize_end = self.editor.timeline.end_time.is_none();
        let first_new_clip = self.editor.timeline.clips().len();
        let existing_clips = self
            .editor
            .timeline
            .clips()
            .iter()
            .map(|clip| clip.id)
            .collect::<std::collections::HashSet<_>>();
        let all_media = items
            .iter()
            .filter_map(|item| match item {
                MediaDragItem::Media {
                    media,
                    stream: MediaStream::All,
                } => Some(*media),
                _ => None,
            })
            .collect::<std::collections::HashSet<_>>();
        let mut cursor = MediaInsertionCursor::new(&mut self.editor.timeline, anchor_track, time);
        let mut inserted_any = false;

        for item in items {
            match *item {
                MediaDragItem::Media { media, stream } => {
                    if stream != MediaStream::All && all_media.contains(&media) {
                        continue;
                    }
                    let Some(asset) = self.editor.project.media(media).cloned() else {
                        continue;
                    };
                    let duration =
                        asset.duration.unwrap_or(5.0).clamp(0.1, 24.0 * 60.0 * 60.0) as f32;
                    if stream == MediaStream::All {
                        let media_first_clip = self.editor.timeline.clips().len();
                        let (video_count, audio_count) = if asset.tracks.is_empty() {
                            (
                                usize::from(!matches!(
                                    asset.kind,
                                    MediaKind::Audio | MediaKind::WasmPlugin
                                )),
                                usize::from(
                                    matches!(asset.kind, MediaKind::Audio)
                                        || matches!(asset.kind, MediaKind::Video)
                                            && asset.has_audio,
                                ),
                            )
                        } else {
                            (
                                asset
                                    .tracks
                                    .iter()
                                    .filter(|track| {
                                        track.kind == crate::project::MediaTrackKind::Video
                                    })
                                    .count(),
                                asset
                                    .tracks
                                    .iter()
                                    .filter(|track| {
                                        track.kind == crate::project::MediaTrackKind::Audio
                                    })
                                    .count(),
                            )
                        };
                        if matches!(asset.kind, MediaKind::WasmPlugin) {
                            let (videos, _) = cursor.tracks(&mut self.editor.timeline, 1, 0);
                            inserted_any |= self.insert_media_asset_at(
                                asset.clone(),
                                MediaStream::All,
                                videos[0],
                                cursor.time,
                            );
                        } else {
                            let (videos, audios) =
                                cursor.tracks(&mut self.editor.timeline, video_count, audio_count);
                            for (index, track) in videos.into_iter().enumerate() {
                                inserted_any |= self.insert_media_asset_at(
                                    asset.clone(),
                                    MediaStream::Video(index),
                                    track,
                                    cursor.time,
                                );
                            }
                            for (index, track) in audios.into_iter().enumerate() {
                                inserted_any |= self.insert_media_asset_at(
                                    asset.clone(),
                                    MediaStream::Audio(index),
                                    track,
                                    cursor.time,
                                );
                            }
                        }
                        let media_clip_ids = self.editor.timeline.clips()[media_first_clip..]
                            .iter()
                            .map(|clip| clip.id)
                            .collect::<Vec<_>>();
                        self.editor.timeline.group_clip_ids(&media_clip_ids);
                        cursor.advance(duration);
                    } else {
                        let (videos, audios) = match stream {
                            MediaStream::Video(_) => cursor.tracks(&mut self.editor.timeline, 1, 0),
                            MediaStream::Audio(_) => cursor.tracks(&mut self.editor.timeline, 0, 1),
                            MediaStream::All => unreachable!(),
                        };
                        let target = match stream {
                            MediaStream::Video(_) => videos[0],
                            MediaStream::Audio(_) => audios[0],
                            MediaStream::All => unreachable!(),
                        };
                        if self.insert_media_asset_at(asset, stream, target, cursor.time) {
                            inserted_any = true;
                            cursor.advance(duration);
                        }
                    }
                }
                MediaDragItem::Composition {
                    composition,
                    stream,
                } => {
                    let parent = self.editor.project.active_composition;
                    if !self
                        .editor
                        .project
                        .can_reference_composition(parent, composition)
                    {
                        continue;
                    }
                    let Some(source) = self.editor.project.composition(composition).cloned() else {
                        continue;
                    };
                    let duration = self
                        .editor
                        .project
                        .composition_duration(composition)
                        .unwrap_or(5.0);
                    let has_audio = self.editor.project.composition_has_audio(composition);
                    let (videos, audios) = match stream {
                        MediaStream::All => {
                            cursor.tracks(&mut self.editor.timeline, 1, usize::from(has_audio))
                        }
                        MediaStream::Video(_) => cursor.tracks(&mut self.editor.timeline, 1, 0),
                        MediaStream::Audio(_) => cursor.tracks(&mut self.editor.timeline, 0, 1),
                    };
                    let visual_pipeline =
                        if matches!(stream, MediaStream::All | MediaStream::Video(_)) {
                            match self.plugins.visual_pipeline_instance() {
                                Ok(p) => p,
                                Err(_) => continue,
                            }
                        } else {
                            effects::PipelineInstance::effect_default()
                        };
                    let inserted = match stream {
                        MediaStream::All => self.editor.timeline.insert_av_composition_clip_at(
                            (videos[0], cursor.time),
                            composition,
                            source.name,
                            has_audio,
                            duration,
                            visual_pipeline,
                        ),
                        MediaStream::Video(_) => self.editor.timeline.insert_composition_clip_at(
                            (videos[0], cursor.time),
                            composition,
                            source.name,
                            false,
                            duration,
                            visual_pipeline,
                        ),
                        MediaStream::Audio(_) => self.editor.timeline.insert_composition_clip_at(
                            (audios[0], cursor.time),
                            composition,
                            source.name,
                            true,
                            duration,
                            visual_pipeline,
                        ),
                    };
                    if inserted {
                        inserted_any = true;
                        cursor.advance(duration);
                    }
                }
            }
        }

        if inserted_any {
            if initialize_end {
                self.editor
                    .timeline
                    .set_initial_end_from_clips_since(first_new_clip);
            }
            let inserted = self
                .editor
                .timeline
                .clips()
                .iter()
                .filter(|clip| !existing_clips.contains(&clip.id))
                .map(|clip| clip.id)
                .collect::<Vec<_>>();
            self.editor.timeline.select_clip_ids(&inserted);
        }
        inserted_any
    }

    pub(super) fn handle_media_action(&mut self, action: MediaAction) {
        match action {
            MediaAction::None | MediaAction::BeginDrag { .. } => {}
            MediaAction::NewComposition => self
                .command_queue
                .push(EditorCommand::Action(PaletteAction::NewComposition)),
            MediaAction::DuplicateComposition(composition) => {
                self.set_history_gesture_label("Duplicate composition");
                self.editor
                    .project
                    .sync_active_timeline(self.editor.timeline.document());
                if let Some(duplicate) = self.editor.project.duplicate_composition(composition) {
                    self.media.select_composition(duplicate);
                }
            }
            MediaAction::RenameComposition(composition) => {
                if let Some(name) = self
                    .editor
                    .project
                    .composition(composition)
                    .map(|composition| composition.name.clone())
                {
                    self.open_modal(Modal::Composition(NewCompositionDialog::rename(
                        composition,
                        &name,
                    )));
                }
            }
            MediaAction::DeleteComposition(composition) => {
                self.set_history_gesture_label("Delete composition");
                self.editor
                    .project
                    .sync_active_timeline(self.editor.timeline.document());
                if !self.editor.project.remove_composition(composition) {
                    return;
                }
                self.editor.timeline.load_history_document(
                    self.editor.project.active_composition().timeline.clone(),
                );
                self.editor
                    .timeline
                    .ensure_composition_visual_pipelines(&self.plugins);
                self.editor
                    .timeline
                    .discard_composition_from_clipboard(composition);
                self.editor
                    .timeline
                    .reconcile_pipeline_overrides(&self.editor.project);
                self.media.clear_selection();
                self.audio.clear();
                self.playback.invalidate();
                self.render_panel.sync_timeline_ranges(
                    &mut self.editor.timeline,
                    self.editor.project.active_composition,
                    self.editor.project.active_settings().frame_rate,
                );
            }
            MediaAction::Import => self
                .command_queue
                .push(EditorCommand::Action(PaletteAction::ImportMedia)),
            MediaAction::ImportClipboard => self
                .command_queue
                .push(EditorCommand::Action(PaletteAction::ImportClipboard)),
            MediaAction::InsertSelected { items } => {
                self.set_history_gesture_label("Insert media at playhead");
                let target = self
                    .editor
                    .timeline
                    .insert_target(&self.snapshot, self.cursor)
                    .map(|(track, time, _)| (track, time));
                if target
                    .is_some_and(|(track, time)| self.insert_media_drag_items(&items, track, time))
                {
                    self.media.clear_selection();
                    self.playback.invalidate();
                }
            }
            MediaAction::ReplaceSelectedMedia { media } => {
                let Some(path) = rfd::FileDialog::new()
                    .add_filter(
                        "Media",
                        &[
                            "png", "jpg", "jpeg", "webp", "gif", "bmp", "tif", "tiff", "tga",
                            "mp4", "mov", "mkv", "webm", "avi", "m4v", "wav", "mp3", "flac", "aac",
                            "ogg", "m4a", "wasm",
                        ],
                    )
                    .pick_file()
                else {
                    return;
                };
                self.set_history_gesture_label("Replace media");
                match self.editor.project.replace_media(media, path.clone()) {
                    Ok(()) => {
                        self.waveform_textures.clear();
                        self.waveform_textures.queue_missing(&self.editor.project);
                        self.warm_project_scrub_thumbnails();
                        self.playback.clear_media_caches();
                        if path
                            .extension()
                            .and_then(|extension| extension.to_str())
                            .is_some_and(|extension| extension.eq_ignore_ascii_case("wasm"))
                        {
                            if let Err(error) = self.playback.precompile_wasm(&path) {
                                messages::warning(
                                    "Replace media",
                                    format!("CPU/WASM plugin precompile failed: {error:#}"),
                                );
                            }
                        }
                        self.audio.clear();
                        self.playback.invalidate();
                    }
                    Err(error) => messages::warning("Replace media", format!("{error:#}")),
                }
            }
            MediaAction::RemoveSelected { media } => {
                self.set_history_gesture_label("Remove media");
                let media = media.into_iter().collect::<std::collections::HashSet<_>>();
                self.editor
                    .project
                    .sync_active_timeline(self.editor.timeline.document());
                if self.editor.project.remove_media(&media) == 0 {
                    return;
                }
                self.warm_project_scrub_thumbnails();
                let document = self.editor.project.active_composition().timeline.clone();
                self.editor.timeline.load_history_document(document);
                self.editor.timeline.discard_media_from_clipboard(&media);
                self.editor
                    .timeline
                    .reconcile_pipeline_overrides(&self.editor.project);
                self.media.clear_selection();
                self.audio.clear();
                self.playback.invalidate();
                self.render_panel.sync_timeline_ranges(
                    &mut self.editor.timeline,
                    self.editor.project.active_composition,
                    self.editor.project.active_settings().frame_rate,
                );
            }
        }
    }

    pub(super) fn remove_missing_media_files(&mut self) {
        let now = Instant::now();
        if now < self.next_media_presence_check {
            return;
        }
        self.next_media_presence_check = now + MEDIA_PRESENCE_CHECK_INTERVAL;
        if self.editor.history_gesture.is_some() {
            return;
        }

        let missing = missing_project_media(&self.editor.project);
        if missing.is_empty() {
            return;
        }
        let missing_ids = missing
            .iter()
            .map(|(media, _)| *media)
            .collect::<HashSet<_>>();
        let missing_paths = missing
            .iter()
            .map(|(_, path)| path.display().to_string())
            .collect::<Vec<_>>();
        let before = self
            .editor
            .history
            .capture(&self.editor.project, &self.editor.timeline);

        self.editor
            .project
            .sync_active_timeline(self.editor.timeline.document());
        if self.editor.project.remove_media(&missing_ids) == 0 {
            return;
        }
        self.editor
            .timeline
            .load_history_document(self.editor.project.active_composition().timeline.clone());
        self.editor
            .timeline
            .ensure_composition_visual_pipelines(&self.plugins);
        self.editor
            .timeline
            .discard_media_from_clipboard(&missing_ids);
        self.editor
            .timeline
            .reconcile_pipeline_overrides(&self.editor.project);
        self.media.clear_selection();
        self.audio.clear();
        self.waveform_textures.clear();
        self.waveform_textures.queue_missing(&self.editor.project);
        self.warm_project_scrub_thumbnails();
        self.playback.clear_caches();
        self.monitor.clear_captured_frame();
        self.playback.invalidate();
        self.render_panel.sync_timeline_ranges(
            &mut self.editor.timeline,
            self.editor.project.active_composition,
            self.editor.project.active_settings().frame_rate,
        );
        self.editor.history.record_after(
            "Remove missing media",
            before,
            &self.editor.project,
            &self.editor.timeline,
            false,
        );
        messages::warning(
            "Missing media",
            format!(
                "Removed missing media and clips using it:\n{}",
                missing_paths.join("\n")
            ),
        );
    }

    pub(super) fn clear_editor_selection(&mut self) {
        self.editor.timeline.clear_selection();
        self.media.clear_selection();
        self.pipeline_graph.clear_selection();
        self.playback.invalidate();
    }

    pub(super) fn switch_composition(&mut self, composition: CompositionId) {
        if self.editor.project.active_composition == composition
            || self.editor.project.composition(composition).is_none()
        {
            return;
        }
        self.editor
            .project
            .sync_active_timeline(self.editor.timeline.document());
        if !self.editor.project.set_active_composition(composition) {
            return;
        }
        let document = self.editor.project.active_composition().timeline.clone();
        self.editor
            .timeline
            .load_document_preserving_clipboard(document);
        self.editor
            .timeline
            .ensure_composition_visual_pipelines(&self.plugins);
        self.editor
            .timeline
            .reconcile_pipeline_overrides(&self.editor.project);
        self.audio.clear();
        self.playback.invalidate();
        self.render_panel.sync_timeline_ranges(
            &mut self.editor.timeline,
            self.editor.project.active_composition,
            self.editor.project.active_settings().frame_rate,
        );
    }

    pub(super) fn confirm_new_composition_dialog(&mut self, dialog: &NewCompositionDialog) {
        let raw_name = dialog.editor.text().trim().to_string();
        let before = self
            .editor
            .history
            .capture(&self.editor.project, &self.editor.timeline);

        if let NewCompositionMode::Rename(composition) = dialog.mode {
            if raw_name.is_empty() {
                return;
            }
            self.editor
                .project
                .sync_active_timeline(self.editor.timeline.document());
            if !self
                .editor
                .project
                .rename_composition(composition, raw_name)
            {
                return;
            }
            self.editor
                .timeline
                .load_history_document(self.editor.project.active_composition().timeline.clone());
            self.editor.history.record_after(
                "Rename composition",
                before,
                &self.editor.project,
                &self.editor.timeline,
                false,
            );
            return;
        }

        let name = if raw_name.is_empty() {
            format!("Composition {}", self.editor.project.next_composition_id)
        } else {
            raw_name
        };
        let inherited_settings = self.editor.project.active_settings().clone();

        match dialog.mode {
            NewCompositionMode::Blank => {
                let composition = self.editor.project.create_composition(name);
                if let Some(created) = self.editor.project.composition_mut(composition) {
                    created.settings = inherited_settings;
                }
                self.switch_composition(composition);
            }
            NewCompositionMode::FromSelection => {
                let visual_pipeline = match self.plugins.visual_pipeline_instance() {
                    Ok(pipeline) => pipeline,
                    Err(_) => {
                        return;
                    }
                };
                let Some(extraction) = self.editor.timeline.extract_selection_for_composition()
                else {
                    return;
                };
                let composition = self.editor.project.create_composition(name.clone());
                if let Some(created) = self.editor.project.composition_mut(composition) {
                    created.settings = inherited_settings;
                    created.settings.background = project::ProjectBackground::Transparent;
                    created.timeline = extraction.timeline.clone();
                }
                self.editor.timeline.insert_composition_reference(
                    composition,
                    &name,
                    &extraction,
                    visual_pipeline,
                );
                self.playback.invalidate();
                self.audio.clear();
            }
            NewCompositionMode::Rename(_) => unreachable!(),
        }

        self.editor.history.record_after(
            "Create composition",
            before,
            &self.editor.project,
            &self.editor.timeline,
            false,
        );
    }

    pub(super) fn confirm_speed_duration_dialog(&mut self, dialog: &SpeedDurationDialog) {
        let Some(value) = dialog.value() else {
            return;
        };
        let before = self
            .editor
            .history
            .capture(&self.editor.project, &self.editor.timeline);
        self.editor
            .timeline
            .apply_speed_duration(&self.editor.project, dialog.mode, value);
        self.playback.invalidate();
        self.editor.history.record_after(
            "Speed / Duration",
            before,
            &self.editor.project,
            &self.editor.timeline,
            false,
        );
    }

    pub(super) fn graph_pipeline(&self) -> Option<u64> {
        self.pipeline_graph
            .target_pipeline(&self.editor.project, &self.editor.timeline)
    }

    pub(super) fn selected_or_create_pipeline(&mut self, kind: PipelineKind) -> Option<u64> {
        if !self.editor.timeline.can_assign_pipeline() {
            return None;
        }
        if let Some(id) = self
            .editor
            .timeline
            .selected_pipeline()
            .and_then(|instance| instance.pipeline)
            .filter(|id| {
                self.editor
                    .project
                    .pipeline(*id)
                    .is_some_and(|pipeline| pipeline.kind == kind)
            })
        {
            return Some(id);
        }
        let id = self.editor.project.create_pipeline_kind(kind);
        self.editor.timeline.set_selected_pipeline(Some(id));
        Some(id)
    }

    pub(super) fn sync_effect_runtime(&mut self) {
        self.effects.rebuild(&self.editor.project.pipelines);
        self.playback
            .sync_compiled_effects(&self.renderer, &self.effects, &self.plugins);
    }

    pub(super) fn edit_shared_graph_input(
        &mut self,
        node: u64,
        input: &str,
        edit_project: impl FnOnce(&mut Project, u64) -> bool,
        edit_instance: impl FnOnce(&mut TimelineState, &mut Project) -> bool,
    ) {
        let pipeline = self.graph_pipeline();
        let structural = pipeline.is_some_and(|pipeline| {
            self.editor
                .project
                .pipeline(pipeline)
                .and_then(|pipeline| pipeline.node(node))
                .and_then(|node| node.dynamic_image_inputs.as_ref())
                .is_some_and(|dynamic| dynamic.count_input == input)
        });
        let changed = if self.pipeline_graph.is_pinned() {
            pipeline.is_some_and(|pipeline| edit_project(&mut self.editor.project, pipeline))
        } else {
            edit_instance(&mut self.editor.timeline, &mut self.editor.project)
        };
        if changed {
            self.editor
                .timeline
                .reconcile_pipeline_overrides(&self.editor.project);
            if structural {
                self.sync_effect_runtime();
            }
            self.playback.invalidate();
        }
    }

    pub(super) fn handle_pipeline_graph_action(&mut self, action: PipelineGraphAction) {
        self.command_queue
            .push(EditorCommand::pipeline_graph(action));
    }

    pub(super) fn remove_graph_target(&mut self, target: GraphNodeTarget) -> (bool, bool) {
        match target {
            GraphNodeTarget::Local(node) => {
                (false, self.editor.timeline.remove_selected_local_node(node))
            }
            GraphNodeTarget::Shared(node) => (
                self.graph_pipeline().is_some_and(|pipeline| {
                    self.editor.project.remove_pipeline_node(pipeline, node)
                }),
                false,
            ),
            GraphNodeTarget::Value(node) => (
                self.graph_pipeline()
                    .is_some_and(|pipeline| self.editor.project.remove_value_node(pipeline, node)),
                false,
            ),
            GraphNodeTarget::Input | GraphNodeTarget::Output => (false, false),
        }
    }

    pub(super) fn move_graph_node(&mut self, target: GraphNodeTarget, position: [f32; 2]) {
        match target {
            GraphNodeTarget::Input | GraphNodeTarget::Output => {
                let input = matches!(target, GraphNodeTarget::Input);
                if self.pipeline_graph.is_pinned() {
                    if let Some(pipeline) = self.graph_pipeline() {
                        let _ = self
                            .editor
                            .project
                            .set_pipeline_endpoint_position(pipeline, input, position);
                    }
                } else {
                    let _ = self
                        .editor
                        .timeline
                        .set_selected_endpoint_position(input, position);
                }
            }
            GraphNodeTarget::Local(node) => {
                let _ = self
                    .editor
                    .timeline
                    .set_selected_local_node_position(node, position);
            }
            GraphNodeTarget::Shared(node) => {
                if let Some(pipeline) = self.graph_pipeline() {
                    let _ = self
                        .editor
                        .project
                        .set_pipeline_node_position(pipeline, node, position);
                }
            }
            GraphNodeTarget::Value(node) => {
                if let Some(pipeline) = self.graph_pipeline() {
                    let _ = self
                        .editor
                        .project
                        .set_value_node_position(pipeline, node, position);
                }
            }
        }
    }

    pub(super) fn execute_pipeline_graph_action(&mut self, action: PipelineGraphAction) {
        let mut graph_changed = false;
        let mut local_changed = false;
        match action {
            PipelineGraphAction::None => return,
            PipelineGraphAction::SelectPipeline(pipeline) => {
                if let Some(pipeline) = pipeline {
                    self.pipeline_graph.open_pipeline(pipeline);
                } else {
                    self.pipeline_graph.follow_selection();
                }
                return;
            }
            PipelineGraphAction::Create => {
                self.palette.pending_open = Some((PaletteKind::NewPipeline, None));
                return;
            }
            PipelineGraphAction::RemovePipeline(id) => {
                if let Some(remaps) = self.editor.project.remove_pipeline(id) {
                    self.editor.timeline.clear_pipeline_references(id);
                    self.editor
                        .timeline
                        .remap_pipeline_selector_overrides(&remaps);
                    self.pipeline_graph.follow_selection();
                    graph_changed = true;
                    self.playback.invalidate();
                }
            }
            PipelineGraphAction::InsertNode => {
                let pipeline = if let Some(id) = self.graph_pipeline() {
                    id
                } else {
                    let kind = self
                        .pipeline_graph
                        .graph_kind(&self.editor.project, &self.editor.timeline);
                    let id = self.editor.project.create_pipeline_kind(kind);
                    if self.editor.timeline.can_assign_pipeline() {
                        self.editor.timeline.set_selected_pipeline(Some(id));
                        self.pipeline_graph.follow_selection();
                    } else {
                        self.pipeline_graph.open_pipeline(id);
                    }
                    self.sync_effect_runtime();
                    id
                };
                let anchor = self
                    .focused_panel()
                    .and_then(|(stack, _)| self.snapshot.stack(stack))
                    .map(|layout| {
                        let point = if layout.content.contains(self.cursor) {
                            self.cursor
                        } else {
                            [
                                layout.content.x + layout.content.width * 0.5,
                                layout.content.y + layout.content.height * 0.5,
                            ]
                        };
                        (
                            self.pipeline_graph
                                .insertion_position(layout.content, point),
                            Rect::new(point[0], point[1], 1.0, 1.0),
                        )
                    });
                if let Some((position, anchor)) = anchor {
                    self.palette.pending_open =
                        Some((PaletteKind::NodeInsert { pipeline, position }, Some(anchor)));
                }
                return;
            }
            PipelineGraphAction::Remove(target) => {
                (graph_changed, local_changed) = self.remove_graph_target(target);
            }
            PipelineGraphAction::RemoveMany(targets) => {
                for target in targets {
                    let changed = self.remove_graph_target(target);
                    graph_changed |= changed.0;
                    local_changed |= changed.1;
                }
            }
            PipelineGraphAction::DeleteWire(wire) => match wire {
                panels::GraphWire::LocalImage {
                    destination: Some(node),
                    ..
                } => {
                    local_changed = self.editor.timeline.disconnect_selected_local_image(node);
                }
                panels::GraphWire::LocalImage {
                    destination: None, ..
                } => {
                    local_changed = self.editor.timeline.disconnect_selected_local_output();
                }
                panels::GraphWire::Image {
                    destination: Some(node),
                    input: Some(input),
                    ..
                } => {
                    if let Some(pipeline) = self.graph_pipeline() {
                        graph_changed = self
                            .editor
                            .project
                            .disconnect_pipeline_image_input(pipeline, node, &input);
                    }
                }
                panels::GraphWire::Image {
                    destination: None, ..
                } => {
                    if let Some(pipeline) = self.graph_pipeline() {
                        graph_changed = self.editor.project.disconnect_pipeline_output(pipeline);
                    }
                }
                panels::GraphWire::Image {
                    destination: Some(_),
                    input: None,
                    ..
                } => {
                    debug_assert!(false, "node-bound image wires must name their input socket");
                }
                panels::GraphWire::Value {
                    destination, input, ..
                } => {
                    if let Some(pipeline) = self.graph_pipeline() {
                        graph_changed = self.editor.project.disconnect_pipeline_value(
                            pipeline,
                            destination,
                            &input,
                        );
                    }
                }
            },
            PipelineGraphAction::MoveNode { target, position } => {
                self.move_graph_node(target, position);
                return;
            }
            PipelineGraphAction::MoveNodes(nodes) => {
                for (target, position) in nodes {
                    self.move_graph_node(target, position);
                }
                return;
            }
            PipelineGraphAction::ConnectLocalImage { node, source } => {
                local_changed = self
                    .editor
                    .timeline
                    .connect_selected_local_image(node, source);
            }
            PipelineGraphAction::SetLocalOutput { source } => {
                local_changed = self.editor.timeline.set_selected_local_output(source);
            }
            PipelineGraphAction::ConnectSharedBoundary {
                source,
                destination,
            } => {
                if let Some(pipeline) = self.graph_pipeline() {
                    graph_changed = self.editor.project.set_pipeline_output(pipeline, source);
                    local_changed = if let Some(node) = destination {
                        self.editor
                            .timeline
                            .connect_selected_local_image(node, None)
                    } else {
                        self.editor.timeline.set_selected_local_output(None)
                    };
                }
            }
            PipelineGraphAction::ConnectImage {
                node,
                input,
                source,
            } => {
                if let Some(pipeline) = self.graph_pipeline() {
                    graph_changed = self
                        .editor
                        .project
                        .connect_pipeline_image_input(pipeline, node, &input, source);
                }
            }
            PipelineGraphAction::SetOutput { source } => {
                if let Some(pipeline) = self.graph_pipeline() {
                    graph_changed = self.editor.project.set_pipeline_output(pipeline, source);
                }
            }
            PipelineGraphAction::ConnectValue {
                node,
                input,
                source,
            } => {
                if let Some(pipeline) = self.graph_pipeline() {
                    graph_changed = self
                        .editor
                        .project
                        .connect_pipeline_value(pipeline, node, &input, source);
                }
            }
            PipelineGraphAction::SetValueComponent {
                node,
                component,
                value,
                linked,
            } => {
                if let Some(pipeline) = self.graph_pipeline() {
                    if self
                        .editor
                        .project
                        .set_value_node_component(pipeline, node, component, value, linked)
                    {
                        self.playback.invalidate();
                    }
                }
                return;
            }
            PipelineGraphAction::SetEffectComponent {
                target,
                input,
                component,
                value,
                linked,
            } => {
                match target {
                    GraphNodeTarget::Local(node) => {
                        if self.editor.timeline.set_selected_local_node_component(
                            node, &input, component, value, linked,
                        ) {
                            self.playback.invalidate();
                        }
                    }
                    GraphNodeTarget::Shared(node) => {
                        self.edit_shared_graph_input(
                            node,
                            &input,
                            |project, pipeline| {
                                project.set_pipeline_node_component(
                                    pipeline, node, &input, component, value, linked,
                                )
                            },
                            |timeline, project| {
                                timeline.set_pipeline_input_component(
                                    project, node, &input, component, value, linked,
                                )
                            },
                        );
                    }
                    GraphNodeTarget::Value(node) => {
                        if let Some(pipeline) = self.graph_pipeline() {
                            if self.editor.project.set_value_node_input_component(
                                pipeline, node, &input, component, value, linked,
                            ) {
                                self.playback.invalidate();
                            }
                        }
                    }
                    GraphNodeTarget::Input => {
                        if let Some(current) = self.editor.timeline.generator_value(&input) {
                            if let Some(next) = current.with_component(component, value, linked) {
                                self.editor.timeline.set_generator_value(&input, next);
                                self.playback.invalidate();
                            }
                        }
                    }
                    GraphNodeTarget::Output => {}
                }
                return;
            }
            PipelineGraphAction::SetEffectValue {
                target,
                input,
                value,
            } => {
                match target {
                    GraphNodeTarget::Local(node) => {
                        if self
                            .editor
                            .timeline
                            .set_selected_local_node_value(node, &input, value)
                        {
                            self.playback.invalidate();
                        }
                    }
                    GraphNodeTarget::Shared(node) => {
                        self.edit_shared_graph_input(
                            node,
                            &input,
                            |project, pipeline| {
                                project.set_pipeline_node_value(pipeline, node, &input, value)
                            },
                            |timeline, project| {
                                timeline.set_pipeline_input_value(project, node, &input, value)
                            },
                        );
                    }
                    GraphNodeTarget::Value(node) => {
                        if let Some(pipeline) = self.graph_pipeline() {
                            if self
                                .editor
                                .project
                                .set_value_node_input_value(pipeline, node, &input, value)
                            {
                                self.playback.invalidate();
                            }
                        }
                    }
                    GraphNodeTarget::Input => {
                        self.editor.timeline.set_generator_value(&input, value);
                        self.playback.invalidate();
                    }
                    GraphNodeTarget::Output => {}
                }
                return;
            }
            PipelineGraphAction::SetHostValue {
                target,
                input,
                value,
            } => {
                match target {
                    GraphNodeTarget::Local(node) => {
                        if self
                            .editor
                            .timeline
                            .set_selected_local_node_host_value(node, &input, value)
                        {
                            self.playback.invalidate();
                        }
                    }
                    GraphNodeTarget::Shared(node) => {
                        if let Some(pipeline) = self.graph_pipeline() {
                            if self.editor.project.set_pipeline_node_host_value(
                                pipeline,
                                node,
                                &input,
                                self.editor.timeline.selected_keyframe_time(),
                                value,
                            ) {
                                self.playback.invalidate();
                            }
                        }
                    }
                    GraphNodeTarget::Input => {
                        self.editor.timeline.set_generator_host_value(&input, value);
                        self.playback.invalidate();
                    }
                    GraphNodeTarget::Value(_) | GraphNodeTarget::Output => {}
                }
                return;
            }
            PipelineGraphAction::ToggleHostKeyframe { target, input } => {
                match target {
                    GraphNodeTarget::Local(node) => {
                        if self
                            .editor
                            .timeline
                            .toggle_selected_local_node_host_keyframe(node, &input)
                        {
                            self.playback.invalidate();
                        }
                    }
                    GraphNodeTarget::Shared(node) => {
                        if let Some(pipeline) = self.graph_pipeline() {
                            if self.editor.project.toggle_pipeline_node_host_keyframe(
                                pipeline,
                                node,
                                &input,
                                self.editor.timeline.selected_keyframe_time(),
                            ) {
                                self.playback.invalidate();
                            }
                        }
                    }
                    GraphNodeTarget::Input => {
                        self.editor.timeline.toggle_generator_keyframe(&input);
                        self.playback.invalidate();
                    }
                    GraphNodeTarget::Value(_) | GraphNodeTarget::Output => {}
                }
                return;
            }
            PipelineGraphAction::SetValueNodeValue { node, value } => {
                if let Some(pipeline) = self.graph_pipeline() {
                    if self
                        .editor
                        .project
                        .set_value_node_value(pipeline, node, value)
                    {
                        self.playback.invalidate();
                    }
                }
                return;
            }
            PipelineGraphAction::MakeInputUnique { node, input } => {
                if self.editor.timeline.make_pipeline_input_unique(
                    &self.editor.project,
                    node,
                    &input,
                ) {
                    self.playback.invalidate();
                }
                return;
            }
            PipelineGraphAction::UseSharedInput { node, input } => {
                if self.editor.timeline.use_shared_pipeline_input(node, &input) {
                    self.playback.invalidate();
                }
                return;
            }
            PipelineGraphAction::InsertNodeOnWire {
                node,
                source,
                destination,
                destination_input,
            } => {
                if let Some(pipeline) = self.graph_pipeline() {
                    graph_changed = self.editor.project.insert_pipeline_node_on_wire(
                        pipeline,
                        node,
                        source,
                        destination,
                        destination_input.as_deref(),
                    );
                }
            }
        }
        if graph_changed {
            self.editor
                .timeline
                .reconcile_pipeline_overrides(&self.editor.project);
            self.sync_effect_runtime();
        }
        if graph_changed || local_changed {
            self.playback.invalidate();
        }
    }
}
