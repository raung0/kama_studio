static NEXT_CACHE_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum ChunkState {
    Pending,
    Rendering,
    Rendered,
    Dirty,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CacheChunk {
    start: u64,
    end: u64,
    source_start: u64,
    source_end: u64,
    path: PathBuf,
    state: ChunkState,
    signature: u64,
    generation: u64,
}

struct ActiveChunk {
    index: usize,
    start_frame: u64,
    end_frame: u64,
    next_frame: u64,
    encoder: Option<CacheEncoder>,
    temp_path: PathBuf,
    signature_at_start: u64,
    input_format: ExportPixelFormat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RenderPhase {
    Idle,
    Rendering,
    Paused,
    Transcoding,
    Done,
    Error,
}

struct RenderJob {
    phase: RenderPhase,
    chunks: Vec<CacheChunk>,
    cached_chunks: Vec<CacheChunk>,
    shared_cache: Arc<Mutex<Vec<CacheChunk>>>,
    active: Option<ActiveChunk>,
    cache_dir: PathBuf,
    audio_path: PathBuf,
    next_generation: u64,
    cache_encoder_failures: Vec<u8>,
    settings: RenderSpec,
    fps: f64,
    canvas_size: [u32; 2],
    audio_mix: Option<JoinHandle<Result<()>>>,
    transcode: Option<JoinHandle<Result<()>>>,
    transcode_cancel: Arc<AtomicBool>,
    error: Option<String>,
}

impl RenderJob {
    #[cfg(test)]
    fn new(
        settings: RenderSpec,
        fps: f64,
        project: &Project,
        timeline: &TimelineState,
        shared_cache: Arc<Mutex<Vec<CacheChunk>>>,
    ) -> Result<Self> {
        let cache_dir = allocate_cache_dir()?;
        Self::new_in_cache_dir(settings, fps, project, timeline, shared_cache, cache_dir)
    }

    fn new_in_cache_dir(
        settings: RenderSpec,
        fps: f64,
        project: &Project,
        timeline: &TimelineState,
        shared_cache: Arc<Mutex<Vec<CacheChunk>>>,
        cache_dir: PathBuf,
    ) -> Result<Self> {
        let audio_path = cache_dir.join("audio.wav");
        fs::create_dir_all(&cache_dir)
            .with_context(|| format!("create {}", cache_dir.display()))?;
        let cached_chunks = shared_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let mut job = Self {
            phase: RenderPhase::Rendering,
            chunks: Vec::new(),
            cached_chunks,
            shared_cache,
            active: None,
            cache_dir,
            audio_path,
            next_generation: 1,
            cache_encoder_failures: Vec::new(),
            settings,
            fps,
            canvas_size: project.active_settings().canvas_size,
            audio_mix: None,
            transcode: None,
            transcode_cancel: Arc::new(AtomicBool::new(false)),
            error: None,
        };
        job.rebuild_chunks(project, timeline);
        Ok(job)
    }

    fn update_live_end(&mut self, requested_end: u64, project: &Project, timeline: &TimelineState) {
        if matches!(
            self.phase,
            RenderPhase::Transcoding | RenderPhase::Done | RenderPhase::Error | RenderPhase::Idle
        ) {
            return;
        }
        let new_end = requested_end.max(self.settings.begin_frame);
        if new_end == self.settings.end_frame {
            return;
        }
        self.abort_active();
        self.settings.end_frame = new_end;
        self.rebuild_chunks(project, timeline);
    }

    fn pause(&mut self) {
        if self.phase == RenderPhase::Rendering {
            self.phase = RenderPhase::Paused;
        }
    }
    fn resume(&mut self) {
        if self.phase == RenderPhase::Paused {
            self.phase = RenderPhase::Rendering;
        }
    }

    fn stop(&mut self) {
        self.abort_active();
        self.transcode_cancel.store(true, Ordering::Release);
        if let Some(task) = self.transcode.take() {
            let _ = task.join();
        }
        self.transcode_cancel.store(false, Ordering::Release);
        if let Some(task) = self.audio_mix.take() {
            let _ = task.join();
        }
        let _ = fs::remove_file(&self.audio_path);
        let _ = fs::remove_file(self.transcode_output_path());
        self.phase = RenderPhase::Idle;
    }

    fn rebuild_chunks(&mut self, project: &Project, timeline: &TimelineState) {
        self.invalidate_if_edited(project, timeline);
        let chunk_frames =
            super::nonnegative_frame((self.fps * cache_chunk_seconds()).round().max(1.0));
        let mut chunks = Vec::new();
        let mut start = self.settings.begin_frame;
        loop {
            if let Some(cached) = self
                .cached_chunks
                .iter()
                .filter(|chunk| {
                    chunk.state == ChunkState::Rendered
                        && chunk.start <= start
                        && chunk.end >= start
                })
                .max_by_key(|chunk| (chunk.end, chunk.generation))
            {
                let mut chunk = cached.clone();
                chunk.start = start;
                chunk.end = chunk.end.min(self.settings.end_frame);
                chunks.push(chunk);
            } else {
                #[expect(
                    clippy::arithmetic_side_effects,
                    reason = "chunk_frames is converted from a duration clamped to at least one frame"
                )]
                let mut end = start
                    .saturating_add(
                        chunk_frames
                            .saturating_sub(1)
                            .saturating_sub(start % chunk_frames),
                    )
                    .min(self.settings.end_frame);
                if let Some(next) = self
                    .cached_chunks
                    .iter()
                    .filter(|chunk| chunk.state == ChunkState::Rendered && chunk.start > start)
                    .map(|chunk| chunk.start)
                    .min()
                {
                    end = end.min(next.saturating_sub(1));
                }
                chunks.push(CacheChunk {
                    start,
                    end,
                    source_start: start,
                    source_end: end,
                    path: self.cache_dir.join(format!("frames-{start}-{end}.mov")),
                    state: ChunkState::Pending,
                    signature: 0,
                    generation: 0,
                });
            }
            let end = chunks.last().map_or(start, |chunk| chunk.end);
            if end >= self.settings.end_frame {
                break;
            }
            start = end.saturating_add(1);
        }
        self.cache_encoder_failures = vec![0; chunks.len()];
        self.chunks = chunks;
    }

    fn restart(
        &mut self,
        settings: RenderSpec,
        project: &Project,
        timeline: &TimelineState,
        live_end_frame: &AtomicU64,
    ) {
        self.stop();
        live_end_frame.store(settings.end_frame, Ordering::Release);
        self.settings = settings;
        self.rebuild_chunks(project, timeline);
        self.error = None;
        self.phase = RenderPhase::Rendering;
    }

    fn abort_active(&mut self) {
        if let Some(active) = self.active.take() {
            drop(active.encoder);
            let _ = fs::remove_file(active.temp_path);
            if let Some(chunk) = self.chunks.get_mut(active.index)
                && chunk.state == ChunkState::Rendering
            {
                chunk.state = if chunk.signature != 0 || active.next_frame > chunk.start {
                    ChunkState::Dirty
                } else {
                    ChunkState::Pending
                };
            }
        }
    }

    fn invalidate_if_edited(&mut self, project: &Project, timeline: &TimelineState) {
        let cache_is_frozen = matches!(self.phase, RenderPhase::Transcoding | RenderPhase::Done);
        let active_changed = !cache_is_frozen
            && self.active.as_ref().is_some_and(|active| {
                range_signature(
                    project,
                    timeline,
                    active.start_frame,
                    active.end_frame,
                    self.fps,
                ) != active.signature_at_start
            });
        if active_changed {
            self.abort_active();
        }

        for chunk in self.chunks.iter_mut().chain(&mut self.cached_chunks) {
            if chunk.signature == 0 || chunk.state == ChunkState::Rendering {
                continue;
            }
            let now = range_signature(
                project,
                timeline,
                chunk.source_start,
                chunk.source_end,
                self.fps,
            );
            chunk.state = if now == chunk.signature && chunk.path.exists() {
                ChunkState::Rendered
            } else {
                ChunkState::Dirty
            };
        }
    }

    fn next_chunk(&self) -> Option<usize> {
        self.chunks
            .iter()
            .position(|chunk| chunk.state == ChunkState::Dirty)
            .or_else(|| {
                self.chunks
                    .iter()
                    .position(|chunk| chunk.state == ChunkState::Pending)
            })
    }

    fn start_chunk(
        &mut self,
        index: usize,
        project: &Project,
        timeline: &TimelineState,
    ) -> Result<()> {
        let chunk = self
            .chunks
            .get_mut(index)
            .context("cache chunk disappeared")?;
        let start_frame = chunk.start;
        let end_frame = chunk.end;
        let temp_path = chunk.path.with_extension("partial.mov");
        let _ = fs::remove_file(&temp_path);
        let encoder = CacheEncoder::new(
            &temp_path,
            project.active_settings().canvas_size,
            self.fps,
            render_encoder_threads(self.settings.background),
        )?;
        let signature = range_signature(project, timeline, start_frame, end_frame, self.fps);
        chunk.state = ChunkState::Rendering;
        self.active = Some(ActiveChunk {
            index,
            start_frame,
            end_frame,
            next_frame: start_frame,
            encoder: Some(encoder),
            temp_path,
            signature_at_start: signature,
            input_format: ExportPixelFormat::Yuva444p10Le,
        });
        Ok(())
    }

    fn finish_active(&mut self, project: &Project, timeline: &TimelineState) -> Result<()> {
        let Some(mut active) = self.active.take() else {
            return Ok(());
        };
        let encoder = active
            .encoder
            .take()
            .context("ProRes cache encoder disappeared")?;
        if let Err(error) = encoder.finish() {
            return self.schedule_cache_retry(
                active.index,
                &active.temp_path,
                &format!("ProRes cache encoder failed: {error:#}"),
            );
        }

        if range_signature(
            project,
            timeline,
            active.start_frame,
            active.end_frame,
            self.fps,
        ) != active.signature_at_start
        {
            let _ = fs::remove_file(&active.temp_path);
            self.chunks
                .get_mut(active.index)
                .context("active cache chunk disappeared")?
                .state = ChunkState::Dirty;
            return Ok(());
        }

        let expected_frames = active
            .end_frame
            .checked_sub(active.start_frame)
            .and_then(|count| count.checked_add(1))
            .context("invalid cache frame range")?;
        if let Err(error) =
            validate_cache_chunk(&active.temp_path, self.canvas_size, expected_frames)
        {
            return self.schedule_cache_retry(
                active.index,
                &active.temp_path,
                &format!("completed ProRes chunk validation failed: {error:#}"),
            );
        }

        let generation = self.next_generation;
        let committed_path = self
            .cache_dir
            .join(format!("chunk-{:06}-g{generation:016}.mov", active.index));

        replace_file(&active.temp_path, &committed_path)
            .with_context(|| format!("commit ProRes cache chunk {}", committed_path.display()))?;

        let chunk = self
            .chunks
            .get_mut(active.index)
            .context("active cache chunk disappeared")?;
        chunk.path = committed_path;
        chunk.source_start = chunk.start;
        chunk.source_end = chunk.end;
        chunk.signature = active.signature_at_start;
        chunk.generation = generation;
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        chunk.state = ChunkState::Rendered;
        self.cached_chunks.push(chunk.clone());
        self.shared_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone_from(&self.cached_chunks);
        *self
            .cache_encoder_failures
            .get_mut(active.index)
            .context("cache failure counter disappeared")? = 0;
        Ok(())
    }

    fn retry_active_after_pipe_failure(&mut self, write_error: Error) -> Result<()> {
        let Some(active) = self.active.take() else {
            return Err(write_error).context("cache encoder failed with no active chunk");
        };
        let index = active.index;
        let temp_path = active.temp_path.clone();
        drop(active.encoder);
        self.schedule_cache_retry(
            index,
            &temp_path,
            &format!("cache encoder write failed: {write_error}"),
        )
    }

    fn schedule_cache_retry(
        &mut self,
        index: usize,
        temp_path: &std::path::Path,
        reason: &str,
    ) -> Result<()> {
        let _ = fs::remove_file(temp_path);
        let chunk = self
            .chunks
            .get_mut(index)
            .context("cache retry chunk disappeared")?;

        chunk.state = if chunk.signature != 0 && chunk.path.exists() {
            ChunkState::Dirty
        } else {
            ChunkState::Pending
        };

        let failures = self
            .cache_encoder_failures
            .get_mut(index)
            .context("cache failure counter disappeared")?;
        *failures = failures.saturating_add(1);
        if *failures <= 3 {
            return Ok(());
        }

        anyhow::bail!(
            "ProRes cache encoder repeatedly failed for frames {}..{}; all previously committed cache chunks were preserved. Last error: {}",
            chunk.start,
            chunk.end,
            reason
        )
    }

    fn begin_transcode(
        &mut self,
        project: &Project,
        timeline: &TimelineState,
        plugins: &PluginRegistry,
    ) -> Result<()> {
        for chunk in &self.chunks {
            if chunk.state != ChunkState::Rendered || chunk.signature == 0 || !chunk.path.exists() {
                anyhow::bail!("render cache chunk {} is not valid", chunk.path.display());
            }
        }

        if let Some(parent) = self
            .settings
            .output
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }

        self.phase = RenderPhase::Transcoding;
        if self.settings.preset.include_audio {
            let project = project.clone();
            let timeline = timeline.document();
            let plugins = plugins.clone();
            let start = self.settings.begin_frame as f64 / self.fps;
            let end = self.settings.end_frame.saturating_add(1) as f64 / self.fps;
            let sample_rate = self.settings.preset.sample_rate;
            let audio_path = self.audio_path.clone();
            let _ = fs::remove_file(&audio_path);
            self.audio_mix = Some(std::thread::spawn(move || {
                render_audio_wav(
                    &project,
                    &plugins,
                    &timeline,
                    start as f32,
                    end as f32,
                    sample_rate,
                    &audio_path,
                )
            }));
        } else {
            self.spawn_final_transcode()?;
        }
        Ok(())
    }

    fn spawn_final_transcode(&mut self) -> Result<()> {
        if !self.settings.overwrite && self.settings.output.exists() {
            anyhow::bail!("output already exists: {}", self.settings.output.display());
        }
        let output = self.transcode_output_path();
        let _ = fs::remove_file(&output);
        let chunks = self
            .chunks
            .iter()
            .map(|chunk| {
                Ok(super::transcoder::CacheSegment {
                    path: chunk.path.clone(),
                    skip_frames: chunk
                        .start
                        .checked_sub(chunk.source_start)
                        .context("invalid cache source range")?,
                    frame_count: chunk
                        .end
                        .checked_sub(chunk.start)
                        .and_then(|count| count.checked_add(1))
                        .context("invalid cache frame range")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let audio = self
            .settings
            .preset
            .include_audio
            .then(|| self.audio_path.clone());
        let settings = self.settings.clone();
        let canvas_size = self.canvas_size;
        let fps = self.fps;
        self.transcode_cancel.store(false, Ordering::Release);
        let cancelled = Arc::clone(&self.transcode_cancel);
        self.transcode = Some(thread::spawn(move || {
            super::transcoder::transcode(
                &chunks,
                audio.as_deref(),
                &output,
                &settings,
                canvas_size,
                fps,
                &cancelled,
            )
        }));
        Ok(())
    }

    fn poll_transcode(&mut self) -> Result<()> {
        if self
            .audio_mix
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished)
        {
            let task = self
                .audio_mix
                .take()
                .context("finished audio task disappeared")?;
            task.join()
                .map_err(|_| anyhow::anyhow!("offline audio render thread panicked"))??;
            self.spawn_final_transcode()?;
        }
        if self.audio_mix.is_some() {
            return Ok(());
        }
        if self
            .transcode
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished)
        {
            let result = self
                .transcode
                .take()
                .context("finished transcode disappeared")?
                .join()
                .map_err(|_| anyhow::anyhow!("transcode thread panicked"))?;
            let _ = fs::remove_file(&self.audio_path);
            let temporary = self.transcode_output_path();
            result?;
            if !self.settings.overwrite && self.settings.output.exists() {
                let _ = fs::remove_file(&temporary);
                anyhow::bail!(
                    "output appeared during render: {}",
                    self.settings.output.display()
                );
            }
            replace_file(&temporary, &self.settings.output)
                .with_context(|| format!("commit render {}", self.settings.output.display()))?;
            self.phase = RenderPhase::Done;
        }
        Ok(())
    }

    fn transcode_output_path(&self) -> PathBuf {
        let stem = self
            .settings
            .output
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("render");
        let extension = self
            .settings
            .output
            .extension()
            .and_then(|value| value.to_str());
        let suffix = extension.map_or_else(
            || format!(".{stem}.kama-partial-{}", std::process::id()),
            |extension| format!(".{stem}.kama-partial-{}.{}", std::process::id(), extension),
        );
        self.settings.output.with_file_name(suffix)
    }

    fn timeline_ranges(&self) -> Vec<RenderCacheRange> {
        fn push_merged(ranges: &mut Vec<RenderCacheRange>, next: RenderCacheRange) {
            if let Some(last) = ranges.last_mut()
                && last.state == next.state
                && (last.end - next.start).abs() <= 0.000_1
            {
                last.end = last.end.max(next.end);
                return;
            }
            ranges.push(next);
        }

        let mut ranges = Vec::new();
        for (index, chunk) in self.chunks.iter().enumerate() {
            let start = chunk.start as f32 / self.fps as f32;
            let end = chunk.end.saturating_add(1) as f32 / self.fps as f32;
            match chunk.state {
                ChunkState::Rendered => push_merged(
                    &mut ranges,
                    RenderCacheRange {
                        start,
                        end,
                        state: RenderCacheState::Rendered,
                    },
                ),
                ChunkState::Dirty => push_merged(
                    &mut ranges,
                    RenderCacheRange {
                        start,
                        end,
                        state: RenderCacheState::Dirty,
                    },
                ),
                ChunkState::Rendering => {
                    let done_end = self
                        .active
                        .as_ref()
                        .filter(|active| active.index == index)
                        .map_or(start, |active| {
                            active.next_frame.min(chunk.end.saturating_add(1)) as f32
                                / self.fps as f32
                        });
                    if done_end > start {
                        push_merged(
                            &mut ranges,
                            RenderCacheRange {
                                start,
                                end: done_end,
                                state: RenderCacheState::Rendering,
                            },
                        );
                    }
                    if chunk.signature != 0 && done_end < end {
                        push_merged(
                            &mut ranges,
                            RenderCacheRange {
                                start: done_end,
                                end,
                                state: RenderCacheState::Dirty,
                            },
                        );
                    }
                }
                ChunkState::Pending => {}
            }
        }
        ranges
    }

    fn preview_ranges(&self) -> Vec<CachePreviewRange> {
        self.cached_chunks
            .iter()
            .rev()
            .filter(|chunk| {
                chunk.state == ChunkState::Rendered && chunk.generation != 0 && chunk.path.exists()
            })
            .map(|chunk| CachePreviewRange {
                start_frame: chunk.start,
                end_frame: chunk.end,
                source_start_frame: chunk.source_start,
                path: chunk.path.clone(),
                generation: chunk.generation,
            })
            .collect()
    }
}

impl Drop for RenderJob {
    fn drop(&mut self) {
        self.abort_active();
        self.transcode_cancel.store(true, Ordering::Release);
        if let Some(task) = self.transcode.take() {
            let _ = task.join();
        }
        let _ = fs::remove_file(self.transcode_output_path());
        if let Some(task) = self.audio_mix.take() {
            let _ = task.join();
        }
        let _ = fs::remove_file(&self.audio_path);

        if let Ok(entries) = fs::read_dir(&self.cache_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.contains(".partial."))
                {
                    let _ = fs::remove_file(path);
                }
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CachePreviewRange {
    start_frame: u64,
    end_frame: u64,
    source_start_frame: u64,
    path: PathBuf,
    generation: u64,
}

#[derive(Clone, Debug)]
struct RenderWorkerStatus {
    phase: RenderPhase,
    ranges: Vec<RenderCacheRange>,
    preview_ranges: Vec<CachePreviewRange>,
    cached_chunks: Vec<CacheChunk>,
    error: Option<String>,
    update_revision: u64,
}

impl RenderWorkerStatus {
    fn from_job(job: &RenderJob, update_revision: u64) -> Self {
        Self {
            phase: job.phase,
            ranges: job.timeline_ranges(),
            preview_ranges: job.preview_ranges(),
            cached_chunks: job.cached_chunks.clone(),
            error: job.error.clone(),
            update_revision,
        }
    }
}

#[derive(Serialize, Deserialize)]
enum RenderWorkerCommand {
    Pause,
    Resume,
    Stop,
    Restart(RenderSpec),
    SetEndFrame(u64),
    SetBackground(bool),
    SetEditing(bool),
    SetInteractive(bool),
    Update {
        revision: u64,
        project: Project,
        worker_state: crate::project::WorkerProjectState,
        video_changed: bool,
    },
    Cancel,
    #[cfg(debug_assertions)]
    Crash,
}

#[derive(Serialize, Deserialize)]
struct RenderProcessStart {
    settings: RenderSpec,
    edit_revision: u64,
    editing: bool,
    interactive: bool,
    project: Project,
    worker_state: crate::project::WorkerProjectState,
    timeline: TimelineDocument,
    plugin_paths: String,
    cache_dir: PathBuf,
    cached_chunks: Vec<CacheChunk>,
    expected_gpu: Option<u64>,
}

#[derive(Serialize, Deserialize)]
enum RenderProcessMessage {
    Start(Box<RenderProcessStart>),
    Command(RenderWorkerCommand),
}

#[derive(Serialize, Deserialize)]
enum RenderProcessResponse {
    Ready { pid: u32 },
    Status(RenderStatusWire),
    Error { message: String },
}

#[derive(Serialize, Deserialize)]
struct RenderStatusWire {
    phase: RenderPhase,
    ranges: Vec<RenderRangeWire>,
    preview_ranges: Vec<CachePreviewRange>,
    cached_chunks: Vec<CacheChunk>,
    error: Option<String>,
    update_revision: u64,
}

#[derive(Serialize, Deserialize)]
struct RenderRangeWire {
    start: f32,
    end: f32,
    state: RenderRangeStateWire,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
enum RenderRangeStateWire {
    Rendered,
    Rendering,
    Dirty,
}

impl From<RenderWorkerStatus> for RenderStatusWire {
    fn from(status: RenderWorkerStatus) -> Self {
        Self {
            phase: status.phase,
            ranges: status
                .ranges
                .into_iter()
                .map(|range| RenderRangeWire {
                    start: range.start,
                    end: range.end,
                    state: match range.state {
                        RenderCacheState::Rendered => RenderRangeStateWire::Rendered,
                        RenderCacheState::Rendering => RenderRangeStateWire::Rendering,
                        RenderCacheState::Dirty => RenderRangeStateWire::Dirty,
                    },
                })
                .collect(),
            preview_ranges: status.preview_ranges,
            cached_chunks: status.cached_chunks,
            error: status.error,
            update_revision: status.update_revision,
        }
    }
}

impl From<RenderStatusWire> for RenderWorkerStatus {
    fn from(status: RenderStatusWire) -> Self {
        Self {
            phase: status.phase,
            ranges: status
                .ranges
                .into_iter()
                .map(|range| RenderCacheRange {
                    start: range.start,
                    end: range.end,
                    state: match range.state {
                        RenderRangeStateWire::Rendered => RenderCacheState::Rendered,
                        RenderRangeStateWire::Rendering => RenderCacheState::Rendering,
                        RenderRangeStateWire::Dirty => RenderCacheState::Dirty,
                    },
                })
                .collect(),
            preview_ranges: status.preview_ranges,
            cached_chunks: status.cached_chunks,
            error: status.error,
            update_revision: status.update_revision,
        }
    }
}

struct RenderTask {
    settings: RenderSpec,
    command_tx: SyncSender<RenderProcessMessage>,
    latest_status: Arc<Mutex<Option<RenderWorkerStatus>>>,
    last_response_at: Arc<Mutex<Instant>>,
    process: Option<std::process::Child>,
    diagnostics: Arc<Mutex<VecDeque<String>>>,
    queued_commands: VecDeque<RenderWorkerCommand>,
    pid: u32,
    cache_dir: PathBuf,
    status: RenderWorkerStatus,
    last_sent_revision: u64,
    last_seen_revision: u64,
    last_content_signature: u64,
    last_video_signature: u64,
    pending_video_revision: Option<u64>,
    last_editing: bool,
    last_interactive: bool,
    reported_error: Option<String>,
    status_version: u64,
    canvas_size: [u32; 2],
    fps: f64,
    composition: CompositionId,
}

impl RenderTask {
    #[allow(clippy::too_many_arguments)]
    fn spawn(
        settings: RenderSpec,
        renderer: &Renderer,
        project: &Project,
        timeline: &TimelineState,
        _plugins: &PluginRegistry,
        shared_cache: &Arc<Mutex<Vec<CacheChunk>>>,
        interaction: (u64, bool, bool),
    ) -> Result<Self> {
        let (edit_revision, editing, interactive) = interaction;
        let composition = project.active_composition;
        let fps = project.active_settings().frame_rate.max(1.0);
        let canvas_size = project.active_settings().canvas_size;
        let device = renderer.device_handle();
        let worker_document = timeline.document();
        let last_content_signature = render_content_signature(project, &worker_document);
        let last_video_signature = video_content_signature(project, &worker_document);
        let cache_dir = allocate_cache_dir()?;
        let cached_chunks = shared_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let start = RenderProcessStart {
            settings: settings.clone(),
            edit_revision,
            editing,
            interactive,
            project: render_worker_project_snapshot(project, worker_document.clone()),
            worker_state: crate::project::WorkerProjectState::capture(project),
            timeline: worker_document,
            plugin_paths: crate::preferences::load_plugin_paths(),
            cache_dir: cache_dir.clone(),
            cached_chunks: cached_chunks.clone(),
            expected_gpu: crate::runtime::shared_surface::metal_registry_id(&device).ok(),
        };
        let mut child_command = std::process::Command::new(
            std::env::current_exe().context("find application executable for render worker")?,
        );
        #[cfg(test)]
        child_command
            .args([
                "--exact",
                "render::engine::tests::render_worker_child_entry",
                "--quiet",
            ])
            .env("KAMA_RENDER_WORKER_TEST_CHILD", "1");
        #[cfg(not(test))]
        child_command.arg("--kama-worker=render");
        let child = child_command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("spawn isolated render worker")?;
        let mut child = RenderChildGuard::new(child);
        let pid = child.id();
        let child_stdin = child
            .child_mut()
            .stdin
            .take()
            .context("render worker stdin missing")?;
        let child_stdout = child
            .child_mut()
            .stdout
            .take()
            .context("render worker stdout missing")?;
        let child_stderr = child
            .child_mut()
            .stderr
            .take()
            .context("render worker stderr missing")?;
        let (command_tx, command_rx) = mpsc::sync_channel(8);
        thread::Builder::new()
            .name(format!("kama-render-write-{pid}"))
            .spawn(move || render_process_writer(child_stdin, command_rx))
            .context("start render worker command writer")?;
        let latest_status = Arc::new(Mutex::new(None));
        let reader_status = Arc::clone(&latest_status);
        let reader_cache = Arc::clone(shared_cache);
        let last_response_at = Arc::new(Mutex::new(Instant::now()));
        let reader_last_response_at = Arc::clone(&last_response_at);
        let diagnostics = Arc::new(Mutex::new(VecDeque::with_capacity(32)));
        let reader_diagnostics = Arc::clone(&diagnostics);
        thread::Builder::new()
            .name(format!("kama-render-read-{pid}"))
            .spawn(move || {
                render_process_reader(
                    child_stdout,
                    &reader_status,
                    &reader_cache,
                    &reader_diagnostics,
                    &reader_last_response_at,
                );
            })
            .context("start render worker response reader")?;
        let stderr_diagnostics = Arc::clone(&diagnostics);
        thread::Builder::new()
            .name(format!("kama-render-stderr-{pid}"))
            .spawn(move || render_process_stderr(child_stderr, &stderr_diagnostics))
            .context("start render worker diagnostic reader")?;
        command_tx
            .try_send(RenderProcessMessage::Start(Box::new(start)))
            .map_err(|error| anyhow::anyhow!("queue render worker startup: {error}"))?;
        let child = child.into_child();
        let initial_status = RenderWorkerStatus {
            phase: RenderPhase::Rendering,
            ranges: Vec::new(),
            preview_ranges: cached_chunks
                .iter()
                .filter(|chunk| chunk.state == ChunkState::Rendered && chunk.path.exists())
                .map(|chunk| CachePreviewRange {
                    start_frame: chunk.start,
                    end_frame: chunk.end,
                    source_start_frame: chunk.source_start,
                    path: chunk.path.clone(),
                    generation: chunk.generation,
                })
                .collect(),
            cached_chunks,
            error: None,
            update_revision: edit_revision,
        };
        Ok(Self {
            settings,
            command_tx,
            latest_status,
            last_response_at,
            process: Some(child),
            diagnostics,
            queued_commands: VecDeque::with_capacity(16),
            pid,
            cache_dir,
            status: initial_status,
            last_sent_revision: edit_revision,
            last_seen_revision: edit_revision,
            last_content_signature,
            last_video_signature,
            pending_video_revision: None,
            last_editing: editing,
            last_interactive: interactive,
            reported_error: None,
            status_version: 0,
            canvas_size,
            fps,
            composition,
        })
    }

    fn poll(&mut self) {
        self.flush_commands();
        let next_status = self
            .latest_status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(status) = next_status.filter(|_| self.process.is_some()) {
            self.status = status;
            if self
                .pending_video_revision
                .is_some_and(|revision| revision == self.status.update_revision)
            {
                self.pending_video_revision = None;
            }
            self.status_version = self.status_version.wrapping_add(1);
        }
        if let Some(process) = self.process.as_mut() {
            match process.try_wait() {
                Ok(Some(exit)) if self.status.phase != RenderPhase::Done => {
                    self.process = None;
                    self.status.phase = RenderPhase::Error;
                    let protocol_error = self.status.error.take();
                    let diagnostics = self
                        .diagnostics
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("\n");
                    let diagnostic_summary = if diagnostics.is_empty() {
                        "no child diagnostics were emitted".to_owned()
                    } else {
                        diagnostics
                    };
                    self.status.error = Some(match protocol_error {
                        Some(error) => format!(
                            "{error}; render worker pid {} exited {exit}; {diagnostic_summary}",
                            self.pid
                        ),
                        None => format!(
                            "render worker pid {} exited {exit}; {diagnostic_summary}",
                            self.pid
                        ),
                    });
                    cleanup_render_artifacts_async(
                        self.cache_dir.clone(),
                        self.settings.output.clone(),
                        self.pid,
                    );
                }
                Ok(Some(_)) => self.process = None,
                Ok(None) => {}
                Err(error) => {
                    self.status.phase = RenderPhase::Error;
                    self.status.error =
                        Some(format!("inspect render worker pid {}: {error}", self.pid));
                }
            }
        }
        if self.process.is_some()
            && render_worker_timed_out(
                *self
                    .last_response_at
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                Instant::now(),
            )
        {
            let diagnostic_tail = self
                .diagnostics
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");
            let error = format!(
                "render worker pid {} is unresponsive (no protocol response for 30 seconds){}",
                self.pid,
                if diagnostic_tail.is_empty() {
                    String::new()
                } else {
                    format!("; recent diagnostics: {diagnostic_tail}")
                }
            );
            self.status.phase = RenderPhase::Error;
            self.status.error = Some(error);
            if let Some(mut process) = self.process.take() {
                let _ = process.kill();
                let cache_dir = self.cache_dir.clone();
                let output = self.settings.output.clone();
                let pid = self.pid;
                let _ = thread::Builder::new()
                    .name(format!("kama-render-watchdog-reap-{pid}"))
                    .spawn(move || {
                        let _ = process.wait();
                        cleanup_render_artifacts(&cache_dir, &output, pid);
                    });
            }
        }
        if self.status.phase == RenderPhase::Error
            && let Some(error) = self.status.error.as_ref()
            && self.reported_error.as_ref() != Some(error)
        {
            crate::messages::error("Render worker", error.clone());
            self.reported_error = Some(error.clone());
        }
    }

    fn update_if_edited(
        &mut self,
        edit_revision: u64,
        project: &Project,
        timeline: &TimelineState,
    ) {
        if edit_revision == self.last_seen_revision || self.has_pending_update() {
            return;
        }
        let document = timeline.document();
        let signature = render_content_signature(project, &document);

        if signature == self.last_content_signature {
            self.last_seen_revision = edit_revision;
            return;
        }
        let video_signature = video_content_signature(project, &document);
        let video_changed = video_signature != self.last_video_signature;
        let snapshot = render_worker_project_snapshot(project, document);
        if self.send_command(RenderWorkerCommand::Update {
            revision: edit_revision,
            project: snapshot,
            worker_state: crate::project::WorkerProjectState::capture(project),
            video_changed,
        }) {
            self.last_seen_revision = edit_revision;
            self.last_content_signature = signature;
            self.last_video_signature = video_signature;
            self.last_sent_revision = edit_revision;
        }
        if self.last_sent_revision == edit_revision && video_changed {
            self.pending_video_revision = Some(edit_revision);
        }
    }

    fn update_editing(&mut self, editing: bool) {
        if self.last_editing == editing {
            return;
        }
        if self.send_command(RenderWorkerCommand::SetEditing(editing)) {
            self.last_editing = editing;
        }
    }

    fn update_interactive(&mut self, interactive: bool) {
        if self.last_interactive == interactive {
            return;
        }
        if self.send_command(RenderWorkerCommand::SetInteractive(interactive)) {
            self.last_interactive = interactive;
        }
    }

    fn has_pending_update(&self) -> bool {
        self.status.phase != RenderPhase::Error
            && self.last_sent_revision != self.status.update_revision
    }

    fn has_pending_video_update(&self) -> bool {
        self.status.phase != RenderPhase::Error && self.pending_video_revision.is_some()
    }

    fn pause(&mut self) {
        if self.send_command(RenderWorkerCommand::Pause)
            && self.status.phase == RenderPhase::Rendering
        {
            self.status.phase = RenderPhase::Paused;
        }
    }

    fn resume(&mut self) {
        if self.send_command(RenderWorkerCommand::Resume)
            && self.status.phase == RenderPhase::Paused
        {
            self.status.phase = RenderPhase::Rendering;
        }
    }

    fn restart(&mut self, settings: RenderSpec) {
        if self.send_command(RenderWorkerCommand::Restart(settings.clone())) {
            self.settings = settings;
            self.status.phase = RenderPhase::Rendering;
            self.status.error = None;
            self.reported_error = None;
        }
    }

    fn can_restart(&self, project: &Project) -> bool {
        self.process.is_some()
            && self.status.phase != RenderPhase::Error
            && project.active_composition == self.composition
            && self.canvas_size == project.active_settings().canvas_size
            && (self.fps - project.active_settings().frame_rate.max(1.0)).abs() <= f64::EPSILON
    }

    fn set_background(&mut self, background: bool) {
        if self.send_command(RenderWorkerCommand::SetBackground(background)) {
            self.settings.background = background;
        }
    }

    fn set_end_frame(&mut self, end_frame: u64) {
        let end_frame = end_frame.max(self.settings.begin_frame);
        if self.send_command(RenderWorkerCommand::SetEndFrame(end_frame)) {
            self.settings.end_frame = end_frame;
        }
    }

    fn cancel(&mut self) {
        let _ = self.send_command(RenderWorkerCommand::Cancel);
    }

    fn stop(&mut self) {
        if self.send_command(RenderWorkerCommand::Stop) {
            self.status.phase = RenderPhase::Idle;
        }
    }

    fn send_command(&mut self, command: RenderWorkerCommand) -> bool {
        if !self.queued_commands.is_empty() {
            return self.queue_command(command);
        }
        match self
            .command_tx
            .try_send(RenderProcessMessage::Command(command))
        {
            Ok(()) => true,
            Err(mpsc::TrySendError::Full(RenderProcessMessage::Command(command))) => {
                self.queue_command(command)
            }
            Err(
                mpsc::TrySendError::Full(RenderProcessMessage::Start(_))
                | mpsc::TrySendError::Disconnected(_),
            ) => false,
        }
    }

    fn queue_command(&mut self, command: RenderWorkerCommand) -> bool {
        if let Some(index) = self
            .queued_commands
            .iter()
            .rposition(|queued| same_coalescing_class(queued, &command))
            && let Some(queued) = self.queued_commands.get_mut(index)
        {
            *queued = command;
            return true;
        }
        if self.queued_commands.len() < 16 {
            self.queued_commands.push_back(command);
            true
        } else {
            false
        }
    }

    fn flush_commands(&mut self) {
        while let Some(command) = self.queued_commands.pop_front() {
            match self
                .command_tx
                .try_send(RenderProcessMessage::Command(command))
            {
                Ok(()) => {}
                Err(mpsc::TrySendError::Full(RenderProcessMessage::Command(command))) => {
                    self.queued_commands.push_front(command);
                    break;
                }
                Err(
                    mpsc::TrySendError::Full(RenderProcessMessage::Start(_))
                    | mpsc::TrySendError::Disconnected(_),
                ) => {
                    self.queued_commands.clear();
                    break;
                }
            }
        }
    }
}

fn render_worker_timed_out(last_response_at: Instant, now: Instant) -> bool {
    now.saturating_duration_since(last_response_at) >= Duration::from_secs(30)
}

const fn same_coalescing_class(left: &RenderWorkerCommand, right: &RenderWorkerCommand) -> bool {
    matches!(
        (left, right),
        (
            RenderWorkerCommand::Pause | RenderWorkerCommand::Resume,
            RenderWorkerCommand::Pause | RenderWorkerCommand::Resume
        ) | (
            RenderWorkerCommand::Restart(_),
            RenderWorkerCommand::Restart(_)
        ) | (
            RenderWorkerCommand::SetBackground(_),
            RenderWorkerCommand::SetBackground(_)
        ) | (
            RenderWorkerCommand::SetEditing(_),
            RenderWorkerCommand::SetEditing(_)
        ) | (
            RenderWorkerCommand::SetInteractive(_),
            RenderWorkerCommand::SetInteractive(_)
        ) | (
            RenderWorkerCommand::SetEndFrame(_),
            RenderWorkerCommand::SetEndFrame(_)
        ) | (
            RenderWorkerCommand::Update { .. },
            RenderWorkerCommand::Update { .. }
        )
    )
}

impl Drop for RenderTask {
    fn drop(&mut self) {
        self.cancel();
        if let Some(mut process) = self.process.take() {
            let _ = process.kill();
            let cache_dir = self.cache_dir.clone();
            let output = self.settings.output.clone();
            let pid = self.pid;
            let _ = thread::Builder::new()
                .name(format!("kama-render-reap-{}", self.pid))
                .spawn(move || {
                    let _ = process.wait();
                    cleanup_render_artifacts(&cache_dir, &output, pid);
                });
        }
    }
}

struct RenderChildGuard {
    child: Option<std::process::Child>,
}

impl RenderChildGuard {
    const fn new(child: std::process::Child) -> Self {
        Self { child: Some(child) }
    }

    #[expect(
        clippy::expect_used,
        reason = "the child guard owns its child until into_child transfers it"
    )]
    const fn child_mut(&mut self) -> &mut std::process::Child {
        self.child.as_mut().expect("guarded render child exists")
    }

    #[expect(
        clippy::expect_used,
        reason = "the child guard owns its child until into_child transfers it"
    )]
    fn id(&self) -> u32 {
        self.child
            .as_ref()
            .expect("guarded render child exists")
            .id()
    }

    #[expect(
        clippy::expect_used,
        reason = "the child guard owns its child until this one-time transfer"
    )]
    fn into_child(mut self) -> std::process::Child {
        self.child.take().expect("guarded render child exists")
    }
}

impl Drop for RenderChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_render_worker(
    mut job: RenderJob,
    mut update_revision: u64,
    mut editing: bool,
    mut interactive: bool,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    project: &mut Project,
    timeline: &mut TimelineState,
    effects: &mut EffectRuntime,
    plugins: &PluginRegistry,
    command_rx: &Receiver<RenderWorkerCommand>,
    status_tx: &SyncSender<RenderWorkerStatus>,
    frame_renderer: &mut FrameRenderer,
    target_composition: CompositionId,
    live_end_frame: &Arc<AtomicU64>,
) {
    let mut last_status_at = Instant::now()
        .checked_sub(Duration::from_millis(100))
        .unwrap_or_else(Instant::now);
    let mut last_status_phase = job.phase;

    let _ = status_tx.try_send(RenderWorkerStatus::from_job(&job, update_revision));
    loop {
        let mut cancelled = false;
        loop {
            let command = match command_rx.try_recv() {
                Ok(command) => command,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return,
            };
            match command {
                RenderWorkerCommand::Pause => job.pause(),
                RenderWorkerCommand::Resume => job.resume(),
                RenderWorkerCommand::Stop => job.stop(),
                RenderWorkerCommand::Restart(settings) => {
                    job.restart(settings, project, timeline, live_end_frame);
                }
                RenderWorkerCommand::SetBackground(background) => {
                    job.settings.background = background;
                }
                RenderWorkerCommand::SetEditing(value) => {
                    editing = value;
                }
                RenderWorkerCommand::SetInteractive(value) => {
                    interactive = value;
                }
                RenderWorkerCommand::SetEndFrame(value) => {
                    live_end_frame.store(value, Ordering::Release);
                }
                RenderWorkerCommand::Update {
                    revision,
                    project: mut updated_project,
                    worker_state,
                    video_changed,
                } => {
                    if let Err(error) = worker_state.restore(&mut updated_project) {
                        job.phase = RenderPhase::Error;
                        job.error = Some(format!("restore render update: {error:#}"));
                        update_revision = revision;
                        continue;
                    }
                    if updated_project.set_active_composition(target_composition) {
                        let target_document = updated_project.active_composition().timeline.clone();
                        *project = updated_project;
                        timeline.load_document(target_document);
                        effects.rebuild(&project.pipelines);
                        if video_changed {
                            job.invalidate_if_edited(project, timeline);
                        }
                    }
                    update_revision = revision;
                }
                RenderWorkerCommand::Cancel => {
                    cancelled = true;
                    break;
                }
                #[cfg(debug_assertions)]
                RenderWorkerCommand::Crash => std::process::abort(),
            }
        }
        if cancelled {
            return;
        }

        job.update_live_end(live_end_frame.load(Ordering::Acquire), project, timeline);

        let cached_chunk_count = job.cached_chunks.len();
        let step = (|| -> Result<()> {
            match job.phase {
                RenderPhase::Transcoding => {
                    job.poll_transcode()?;
                    return Ok(());
                }
                RenderPhase::Rendering => {}
                RenderPhase::Paused
                | RenderPhase::Done
                | RenderPhase::Error
                | RenderPhase::Idle => {
                    return Ok(());
                }
            }

            if job.active.is_none() {
                if let Some(index) = job.next_chunk() {
                    job.start_chunk(index, project, timeline)?;
                } else {
                    if editing {
                        return Ok(());
                    }
                    if job.settings.transcode {
                        job.begin_transcode(project, timeline, plugins)?;
                    } else {
                        job.phase = RenderPhase::Done;
                    }
                    return Ok(());
                }
            }

            let (frame, done) = {
                let active = job.active.as_ref().context("render chunk disappeared")?;
                (active.next_frame, active.next_frame > active.end_frame)
            };
            if done {
                job.finish_active(project, timeline)?;
                return Ok(());
            }

            let (batch_end, input_format) = {
                let active = job.active.as_ref().context("render chunk disappeared")?;
                let batch_frames = render_export_batch_frames(job.canvas_size) as u64;
                (
                    active
                        .next_frame
                        .saturating_add(batch_frames.saturating_sub(1))
                        .min(active.end_frame),
                    active.input_format,
                )
            };
            let timeline_times = (frame..=batch_end)
                .map(|batch_frame| (batch_frame as f64 / job.fps) as f32)
                .collect::<Vec<_>>();
            let write_result = {
                let _decode_priority = prioritize_offline_export();
                let active = job
                    .active
                    .as_mut()
                    .context("render chunk disappeared before frame batch")?;
                let encoder = active
                    .encoder
                    .as_mut()
                    .context("ProRes cache encoder closed")?;
                frame_renderer.render_export_yuv_batch_to_writer_on(ExportYuvBatchArgs {
                    device,
                    queue,
                    project,
                    timeline,
                    runtime: (effects, plugins),
                    timeline_times: &timeline_times,
                    first_frame: frame,
                    live_end_frame,
                    format: input_format,
                    writer: encoder,
                })?
            };
            let (write_error, rendered_frames) = write_result;
            if let Some(error) = write_error {
                job.retry_active_after_pipe_failure(error)?;
                return Ok(());
            }
            let finished = {
                let active = job
                    .active
                    .as_mut()
                    .context("render chunk disappeared after write")?;
                active.next_frame = frame.saturating_add(rendered_frames as u64);
                let requested_end = live_end_frame.load(Ordering::Acquire);
                active.next_frame > requested_end || active.next_frame > active.end_frame
            };
            if finished {
                job.finish_active(project, timeline)?;
            }
            Ok(())
        })();

        if let Err(error) = step {
            job.abort_active();
            job.phase = RenderPhase::Error;
            job.error = Some(format!("{error:#}"));
        }
        let phase_changed = job.phase != last_status_phase;
        let cache_committed = job.cached_chunks.len() != cached_chunk_count;
        if phase_changed || cache_committed {
            if status_tx
                .send(RenderWorkerStatus::from_job(&job, update_revision))
                .is_err()
            {
                return;
            }
            last_status_at = Instant::now();
            last_status_phase = job.phase;
        } else if last_status_at.elapsed() >= Duration::from_millis(33) {
            match status_tx.try_send(RenderWorkerStatus::from_job(&job, update_revision)) {
                Ok(()) => {
                    last_status_at = Instant::now();
                    last_status_phase = job.phase;
                }
                Err(TrySendError::Full(_)) => {
                    last_status_at = Instant::now();
                }
                Err(TrySendError::Disconnected(_)) => return,
            }
        }
        match job.phase {
            RenderPhase::Error | RenderPhase::Idle | RenderPhase::Done => {
                thread::sleep(Duration::from_millis(40));
            }
            RenderPhase::Paused => thread::sleep(Duration::from_millis(12)),
            RenderPhase::Transcoding => thread::sleep(Duration::from_millis(20)),
            RenderPhase::Rendering if job.settings.background && interactive => {
                thread::yield_now();
            }
            RenderPhase::Rendering => {}
        }
    }
}

const fn cache_chunk_seconds() -> f64 {
    DEFAULT_CACHE_CHUNK_SECONDS
}

fn allocate_cache_dir() -> Result<PathBuf> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let cache_id = NEXT_CACHE_ID.fetch_add(1, Ordering::Relaxed);
    let cache_dir = std::env::temp_dir().join(format!(
        "kama-render-{stamp}-{}-{cache_id}",
        std::process::id()
    ));
    fs::create_dir_all(&cache_dir).with_context(|| format!("create {}", cache_dir.display()))?;
    Ok(cache_dir)
}

fn validate_cache_chunk(path: &Path, expected_size: [u32; 2], expected_frames: u64) -> Result<()> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("stat completed cache chunk {}", path.display()))?;
    if metadata.len() < 1024 {
        anyhow::bail!(
            "completed cache chunk is suspiciously small ({} bytes)",
            metadata.len()
        );
    }

    crate::runtime::media::init_ffmpeg()?;
    let input = ffmpeg_next::format::input(path)
        .with_context(|| format!("open completed cache chunk {}", path.display()))?;
    let (codec_id, width, height, frame_count) = {
        let stream = input
            .streams()
            .best(ffmpeg_next::media::Type::Video)
            .context("completed cache chunk has no video stream")?;
        let codec_id = stream.parameters().id();
        let frame_count = stream.frames();
        let decoder = ffmpeg_next::codec::context::Context::from_parameters(stream.parameters())?
            .decoder()
            .video()?;
        (codec_id, decoder.width(), decoder.height(), frame_count)
    };
    anyhow::ensure!(
        codec_id == ffmpeg_next::codec::Id::PRORES,
        "completed cache chunk is not ProRes"
    );
    anyhow::ensure!(
        [width, height] == [expected_size[0].max(1), expected_size[1].max(1)],
        "completed cache chunk has size {}x{}, expected {}x{}",
        width,
        height,
        expected_size[0].max(1),
        expected_size[1].max(1)
    );
    anyhow::ensure!(
        u64::try_from(frame_count).ok() == Some(expected_frames),
        "completed cache chunk has {frame_count} frames, expected {expected_frames}"
    );
    Ok(())
}

fn render_export_batch_frames(canvas_size: [u32; 2]) -> usize {
    let pixels = u64::from(canvas_size[0].max(1)).saturating_mul(u64::from(canvas_size[1].max(1)));

    if pixels >= 3_840u64 * 2_160 {
        3
    } else if pixels >= 2_560u64 * 1_440 {
        5
    } else {
        6
    }
}

fn render_encoder_threads(background: bool) -> usize {
    let cores = thread::available_parallelism().map_or(4, std::num::NonZero::get);
    if background {
        cores.saturating_sub(1).max(2)
    } else {
        cores.max(1)
    }
}

fn render_process_writer(
    mut stdin: std::process::ChildStdin,
    commands: Receiver<RenderProcessMessage>,
) {
    for command in commands {
        if serde_json::to_writer(&mut stdin, &command).is_err()
            || stdin.write_all(b"\n").is_err()
            || stdin.flush().is_err()
        {
            break;
        }
    }
}

fn render_process_command_reader<R: std::io::BufRead>(
    mut reader: R,
    commands: &SyncSender<RenderWorkerCommand>,
) {
    loop {
        let mut bytes = Vec::new();
        let read = read_bounded_line(&mut reader, &mut bytes, 64 * 1024 * 1024);
        let command = match read {
            Ok(Some(false)) => match serde_json::from_slice::<RenderProcessMessage>(&bytes) {
                Ok(RenderProcessMessage::Command(command)) => command,
                Ok(RenderProcessMessage::Start(_)) | Err(_) => RenderWorkerCommand::Cancel,
            },
            Ok(Some(true) | None) | Err(_) => RenderWorkerCommand::Cancel,
        };
        let cancel = matches!(command, RenderWorkerCommand::Cancel);
        if commands.send(command).is_err() || cancel {
            return;
        }
    }
}

fn render_process_reader(
    stdout: std::process::ChildStdout,
    latest_status: &Arc<Mutex<Option<RenderWorkerStatus>>>,
    shared_cache: &Arc<Mutex<Vec<CacheChunk>>>,
    diagnostics: &Arc<Mutex<VecDeque<String>>>,
    last_response_at: &Arc<Mutex<Instant>>,
) {
    let mut reader = std::io::BufReader::new(stdout);
    loop {
        let mut bytes = Vec::new();
        match read_bounded_line(&mut reader, &mut bytes, 64 * 1024 * 1024) {
            Ok(Some(false)) => {}
            Ok(Some(true)) => {
                let status = error_status_from_cache(
                    "render worker protocol line exceeded the 64 MiB limit".into(),
                    latest_status,
                    shared_cache,
                );
                *latest_status
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(status);
                break;
            }
            Ok(None) => break,
            Err(error) => {
                push_render_diagnostic(diagnostics, format!("read worker protocol: {error}"));
                break;
            }
        }
        let line = String::from_utf8_lossy(&bytes);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        #[cfg(test)]
        let protocol_line = if line.starts_with('{') {
            line
        } else if is_render_test_harness_output(line) {
            continue;
        } else {
            line
        };
        #[cfg(not(test))]
        let protocol_line = line;
        let response = match serde_json::from_str::<RenderProcessResponse>(protocol_line) {
            Ok(response) => response,
            #[cfg(test)]
            Err(_) if is_render_test_harness_output(line.trim()) => continue,
            Err(error) => {
                let status = error_status_from_cache(
                    format!("decode render worker response: {error}"),
                    latest_status,
                    shared_cache,
                );
                *latest_status
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(status);
                break;
            }
        };
        *last_response_at
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Instant::now();
        match response {
            RenderProcessResponse::Ready { pid } => {
                push_render_diagnostic(diagnostics, format!("worker ready pid={pid}"));
            }
            RenderProcessResponse::Status(status) => {
                shared_cache
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone_from(&status.cached_chunks);
                let mut latest = latest_status
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                *latest = Some(status.into());
            }
            RenderProcessResponse::Error { message } => {
                let status = error_status_from_cache(message, latest_status, shared_cache);
                *latest_status
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(status);
            }
        }
    }
}

fn error_status_from_cache(
    error: String,
    latest_status: &Mutex<Option<RenderWorkerStatus>>,
    shared_cache: &Mutex<Vec<CacheChunk>>,
) -> RenderWorkerStatus {
    let previous = latest_status
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let cached_chunks = shared_cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let preview_ranges = previous
        .as_ref()
        .map(|status| status.preview_ranges.clone())
        .filter(|ranges| !ranges.is_empty())
        .unwrap_or_else(|| {
            cached_chunks
                .iter()
                .filter(|chunk| chunk.state == ChunkState::Rendered && chunk.path.exists())
                .map(|chunk| CachePreviewRange {
                    start_frame: chunk.start,
                    end_frame: chunk.end,
                    source_start_frame: chunk.source_start,
                    path: chunk.path.clone(),
                    generation: chunk.generation,
                })
                .collect()
        });
    RenderWorkerStatus {
        phase: RenderPhase::Error,
        ranges: previous
            .as_ref()
            .map_or_else(Vec::new, |status| status.ranges.clone()),
        preview_ranges,
        cached_chunks,
        error: Some(error),
        update_revision: previous.as_ref().map_or(0, |status| status.update_revision),
    }
}

fn render_process_stderr(
    stderr: std::process::ChildStderr,
    diagnostics: &Arc<Mutex<VecDeque<String>>>,
) {
    let mut reader = std::io::BufReader::new(stderr);
    loop {
        let mut bytes = Vec::new();
        match read_bounded_line(&mut reader, &mut bytes, 4 * 1024) {
            Ok(Some(truncated)) => {
                let mut line = String::from_utf8_lossy(&bytes).into_owned();
                if truncated {
                    line.push_str(" [line truncated at 4 KiB]");
                }
                push_render_diagnostic(diagnostics, line);
            }
            Ok(None) => break,
            Err(error) => {
                push_render_diagnostic(diagnostics, format!("read worker stderr: {error}"));
                break;
            }
        }
    }
}

fn read_bounded_line<R: std::io::BufRead>(
    reader: &mut R,
    output: &mut Vec<u8>,
    max_bytes: usize,
) -> std::io::Result<Option<bool>> {
    let mut truncated = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok((!output.is_empty() || truncated).then_some(truncated));
        }
        let consumed = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index.saturating_add(1));
        let payload_len = if available.get(consumed.saturating_sub(1)) == Some(&b'\n') {
            consumed.saturating_sub(1)
        } else {
            consumed
        };
        let remaining = max_bytes.saturating_sub(output.len());
        let copied = payload_len.min(remaining);
        let Some(payload) = available.get(..copied) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "render worker response segment is out of bounds",
            ));
        };
        output.extend_from_slice(payload);
        truncated |= copied < payload_len;
        let has_newline = available
            .get(consumed.saturating_sub(1))
            .is_some_and(|byte| *byte == b'\n');
        reader.consume(consumed);
        if has_newline {
            return Ok(Some(truncated));
        }
    }
}

fn push_render_diagnostic(diagnostics: &Mutex<VecDeque<String>>, message: String) {
    let mut diagnostics = diagnostics
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if diagnostics.len() == 32 {
        diagnostics.pop_front();
    }
    diagnostics.push_back(message);
}

#[cfg(test)]
fn is_render_test_harness_output(line: &str) -> bool {
    line.starts_with("running ")
        || line.starts_with("test ")
        || line.starts_with("failures:")
        || line.starts_with("test result:")
}

fn cleanup_partial_files(cache_dir: &Path) {
    let Ok(entries) = fs::read_dir(cache_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.contains(".partial."))
        {
            let _ = fs::remove_file(path);
        }
    }
}

fn cleanup_render_artifacts(cache_dir: &Path, output: &Path, pid: u32) {
    cleanup_partial_files(cache_dir);
    let stem = output
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("render");
    let extension = output.extension().and_then(|value| value.to_str());
    let partial_name = extension.map_or_else(
        || format!(".{stem}.kama-partial-{pid}"),
        |extension| format!(".{stem}.kama-partial-{pid}.{extension}"),
    );
    let _ = fs::remove_file(output.with_file_name(partial_name));
}

fn cleanup_render_artifacts_async(cache_dir: PathBuf, output: PathBuf, pid: u32) {
    let _ = thread::Builder::new()
        .name("kama-render-partial-cleanup".into())
        .spawn(move || cleanup_render_artifacts(&cache_dir, &output, pid));
}

fn read_render_process_input<R: std::io::BufRead + Send + 'static>(
    mut input: R,
) -> Result<(RenderProcessStart, Receiver<RenderWorkerCommand>)> {
    let mut start_line = Vec::new();
    match read_bounded_line(&mut input, &mut start_line, 64 * 1024 * 1024)? {
        Some(false) => {}
        Some(true) => anyhow::bail!("render worker startup line exceeded 64 MiB"),
        None => anyhow::bail!("render worker input closed before startup"),
    }
    let start = match serde_json::from_slice::<RenderProcessMessage>(&start_line)
        .context("decode render worker startup")?
    {
        RenderProcessMessage::Start(start) => *start,
        RenderProcessMessage::Command(_) => anyhow::bail!("render worker expected startup first"),
    };
    let (command_tx, command_rx) = mpsc::sync_channel(32);
    thread::Builder::new()
        .name(format!("kama-render-command-read-{}", std::process::id()))
        .spawn(move || render_process_command_reader(input, &command_tx))
        .context("start render worker command reader")?;
    Ok((start, command_rx))
}

pub(super) fn run_render_process_worker() -> Result<()> {
    let (start, command_rx) = read_render_process_input(std::io::BufReader::new(std::io::stdin()))?;
    let startup = (|| -> Result<_> {
        let (instance, adapter, device, queue) = create_render_device(start.expected_gpu)?;
        let mut project = start.project;
        start.worker_state.restore(&mut project)?;
        let timeline = TimelineState::from_document(start.timeline);
        let mut plugins = PluginRegistry::load_default(&start.plugin_paths)?;
        plugins.validate_gpu(&device);
        let mut effects = EffectRuntime::default();
        effects.rebuild(&project.pipelines);
        let frame_renderer = FrameRenderer::new_export_worker(&device, &effects, &plugins);
        let shared_cache = Arc::new(Mutex::new(start.cached_chunks));
        let job = RenderJob::new_in_cache_dir(
            start.settings,
            project.active_settings().frame_rate.max(1.0),
            &project,
            &timeline,
            shared_cache,
            start.cache_dir,
        )?;
        Ok((
            instance,
            adapter,
            device,
            queue,
            project,
            timeline,
            plugins,
            effects,
            frame_renderer,
            job,
        ))
    })();
    let (
        instance,
        adapter,
        device,
        queue,
        mut project,
        mut timeline,
        plugins,
        mut effects,
        mut frame_renderer,
        job,
    ) = match startup {
        Ok(startup) => startup,
        Err(error) => {
            write_render_process_response(&RenderProcessResponse::Error {
                message: format!("initialize render worker: {error:#}"),
            })?;
            return Err(error);
        }
    };
    write_render_process_response(&RenderProcessResponse::Ready {
        pid: std::process::id(),
    })?;
    let (status_tx, status_rx) = mpsc::sync_channel::<RenderWorkerStatus>(1);
    thread::Builder::new()
        .name(format!("kama-render-status-write-{}", std::process::id()))
        .spawn(move || {
            for status in status_rx {
                if write_render_process_response(&RenderProcessResponse::Status(status.into()))
                    .is_err()
                {
                    break;
                }
            }
        })
        .context("start render worker status writer")?;
    let target_composition = project.active_composition;
    let live_end_frame = Arc::new(AtomicU64::new(job.settings.end_frame));
    run_render_worker(
        job,
        start.edit_revision,
        start.editing,
        start.interactive,
        &device,
        &queue,
        &mut project,
        &mut timeline,
        &mut effects,
        &plugins,
        &command_rx,
        &status_tx,
        &mut frame_renderer,
        target_composition,
        &live_end_frame,
    );
    drop(status_tx);
    drop((adapter, instance));
    Ok(())
}

pub(super) fn create_render_device(
    expected_gpu: Option<u64>,
) -> Result<(wgpu::Instance, wgpu::Adapter, wgpu::Device, wgpu::Queue)> {
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = if cfg!(target_os = "macos") {
        wgpu::Backends::METAL
    } else {
        wgpu::Backends::PRIMARY
    };
    let instance = wgpu::Instance::new(descriptor);
    let backends = if cfg!(target_os = "macos") {
        wgpu::Backends::METAL
    } else {
        wgpu::Backends::PRIMARY
    };
    let features = wgpu::Features::TEXTURE_FORMAT_16BIT_NORM
        | wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES;
    let mut fallback = None;
    for adapter in pollster::block_on(instance.enumerate_adapters(backends)) {
        if !adapter.features().contains(features)
            || !adapter
                .get_texture_format_features(wgpu::TextureFormat::R16Uint)
                .allowed_usages
                .contains(wgpu::TextureUsages::STORAGE_BINDING)
        {
            continue;
        }
        let device_descriptor = wgpu::DeviceDescriptor {
            label: Some("kama isolated render device"),
            required_features: features,
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
        };
        let Ok((device, queue)) = pollster::block_on(adapter.request_device(&device_descriptor))
        else {
            continue;
        };
        if expected_gpu.is_none_or(|expected| {
            crate::runtime::shared_surface::metal_registry_id(&device).ok() == Some(expected)
        }) {
            return Ok((instance, adapter, device, queue));
        }
        if expected_gpu.is_none() {
            fallback.get_or_insert((adapter, device, queue));
        }
    }
    if let Some((adapter, device, queue)) = fallback {
        return Ok((instance, adapter, device, queue));
    }
    if expected_gpu.is_some() {
        anyhow::bail!("the render worker could not reacquire the GPU used by the editor")
    }
    anyhow::bail!("no GPU supports render export's required texture formats")
}

fn write_render_process_response(response: &RenderProcessResponse) -> Result<()> {
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    serde_json::to_writer(&mut stdout, response)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

fn render_content_signature(project: &Project, timeline: &TimelineDocument) -> u64 {
    let mut document = timeline.clone();

    document.view = TimelineViewState::default();
    let mut snapshot = render_worker_project_snapshot(project, document);
    for composition in &mut snapshot.compositions {
        composition.timeline.view = TimelineViewState::default();
    }
    let mut hasher = DefaultHasher::new();
    hash_json(&snapshot, &mut hasher);
    hasher.finish()
}

fn video_content_signature(project: &Project, timeline: &TimelineDocument) -> u64 {
    let mut snapshot = render_worker_project_snapshot(project, timeline.clone());

    snapshot.name.clear();
    snapshot.next_media_id = 0;
    snapshot.next_pipeline_id = 0;
    snapshot.next_node_id = 0;
    snapshot.next_composition_id = 0;

    let mut visual_media = HashSet::new();
    for composition in &mut snapshot.compositions {
        composition.name.clear();
        let document = &mut composition.timeline;
        document.view = TimelineViewState::default();
        document.end_time = None;
        document.end_behavior = EndBehavior::default();
        document.next_group = 0;
        document.next_track = 0;
        document.next_clip = 0;

        let audio_tracks = document
            .tracks
            .iter()
            .filter(|track| track.kind == TrackKind::Audio)
            .map(|track| track.id)
            .collect::<HashSet<_>>();
        document
            .clips
            .retain(|clip| !audio_tracks.contains(&clip.track));
        document
            .tracks
            .retain(|track| track.kind != TrackKind::Audio);

        for track in &mut document.tracks {
            track.name.clear();
            track.height = 0.0;
            track.volume = Binding::Constant(crate::effects::GpuValue::F32(1.0));
            track.pan = Binding::Constant(crate::effects::GpuValue::F32(0.0));
            if let Some(instance) = &mut track.pipeline {
                instance.ui_input_position = None;
                instance.ui_output_position = None;
                for node in &mut instance.local_nodes {
                    node.ui_position = None;
                }
            }
        }
        for clip in &mut document.clips {
            clip.name.clear();
            clip.group = None;
            clip.color = crate::timeline::ClipColor::VideoA;
            clip.fade_in = 0.0;
            clip.fade_out = 0.0;
            if let VisualSource::Media(id) = &clip.source {
                visual_media.insert(*id);
            }
            clip.pipeline.ui_input_position = None;
            clip.pipeline.ui_output_position = None;
            for node in &mut clip.pipeline.local_nodes {
                node.ui_position = None;
            }
        }
    }

    snapshot
        .pipelines
        .retain(|pipeline| pipeline.kind == crate::effects::PipelineKind::Video);
    for pipeline in &mut snapshot.pipelines {
        pipeline.name.clear();
        pipeline.revision = 0;
        pipeline.ui_input_position = None;
        pipeline.ui_output_position = None;
        for node in &mut pipeline.nodes {
            node.ui_position = None;
        }
        for node in &mut pipeline.value_nodes {
            node.ui_position = None;
        }
    }

    snapshot
        .media
        .retain(|asset| visual_media.contains(&asset.id));
    for asset in &mut snapshot.media {
        asset.name.clear();
        asset.has_audio = false;
        asset.waveform = None;
        asset
            .tracks
            .retain(|track| track.kind == crate::project::MediaTrackKind::Video);
    }

    let mut hasher = DefaultHasher::new();
    hash_json(&snapshot, &mut hasher);
    hasher.finish()
}

fn render_worker_project_snapshot(project: &Project, timeline: TimelineDocument) -> Project {
    let mut snapshot = project.clone();

    for asset in &mut snapshot.media {
        asset.waveform = None;
    }
    snapshot.sync_active_timeline(timeline);
    snapshot
}

fn range_signature(
    project: &Project,
    timeline: &TimelineState,
    start_frame: u64,
    end_frame: u64,
    fps: f64,
) -> u64 {
    let start = start_frame as f32 / fps as f32;
    let end = end_frame.saturating_add(1) as f32 / fps as f32;
    let sample_times = (start_frame..=end_frame)
        .map(|frame| frame as f64 / fps.max(1.0))
        .collect::<Vec<_>>();
    let mut h = DefaultHasher::new();
    hash_json(project.active_settings(), &mut h);

    let visual_track_ids = timeline
        .tracks()
        .iter()
        .filter(|track| track.kind != TrackKind::Audio)
        .map(|track| track.id)
        .collect::<HashSet<_>>();
    let overlapping = timeline
        .clips()
        .iter()
        .filter(|clip| {
            clip.start < end && clip.end() > start && visual_track_ids.contains(&clip.track)
        })
        .collect::<Vec<_>>();
    let active_tracks = overlapping
        .iter()
        .map(|clip| clip.track)
        .collect::<HashSet<_>>();

    for track in timeline
        .tracks()
        .iter()
        .filter(|track| track.kind != TrackKind::Audio && track.solo)
    {
        track.id.hash(&mut h);
    }

    let mut media_ids = BTreeSet::new();
    for track in timeline.tracks() {
        if track.kind == TrackKind::Audio || !active_tracks.contains(&track.id) {
            continue;
        }
        track.id.hash(&mut h);
        match track.kind {
            TrackKind::Video => 0u8,
            TrackKind::Audio => 1u8,
            TrackKind::Effect => 2u8,
        }
        .hash(&mut h);
        track.muted.hash(&mut h);
        track.solo.hash(&mut h);

        if track.kind == TrackKind::Video {
            hash_composite(&track.composite, &sample_times, &mut h);
            if let Some(instance) = &track.pipeline {
                hash_pipeline_instance(project, instance, &sample_times, &mut h);
            }
        }
    }

    for clip in overlapping {
        clip.track.hash(&mut h);
        clip.start.to_bits().hash(&mut h);

        if matches!(
            &clip.source,
            VisualSource::Media(_) | VisualSource::Composition(_)
        ) {
            clip.speed.to_bits().hash(&mut h);
            clip.source_offset.to_bits().hash(&mut h);
        }

        let active_times = sample_times
            .iter()
            .filter(|time| **time >= f64::from(clip.start) && **time < f64::from(clip.end()))
            .copied()
            .collect::<Vec<_>>();
        active_times.len().hash(&mut h);
        let source_times = if matches!(&clip.source, VisualSource::Composition(_)) {
            active_times
                .iter()
                .map(|time| clip.looped_source_time(*time as f32, project))
                .collect::<Vec<_>>()
        } else {
            active_times.clone()
        };
        let row = timeline.property_row(clip.track, &clip.source, clip.source_instance);
        clip.opacity.to_bits().hash(&mut h);
        let source_state = row.map_or(&clip.source, |row| &row.source);
        hash_visual_source(project, source_state, &source_times, &mut h, 0);
        hash_pipeline_instance(
            project,
            row.map_or(&clip.pipeline, |row| &row.pipeline),
            &active_times,
            &mut h,
        );
        hash_composite(
            row.map_or(&clip.composite, |row| &row.composite),
            &active_times,
            &mut h,
        );
        if let VisualSource::Media(id) = &clip.source {
            media_ids.insert(*id);
        }
    }

    for id in media_ids {
        if let Some(asset) = project.media(id) {
            asset.id.hash(&mut h);
            asset.path.hash(&mut h);
            hash_json(&asset.kind, &mut h);
            hash_json(&asset.duration, &mut h);
            hash_json(&asset.frame_rate, &mut h);
            asset.video_width.hash(&mut h);
            asset.video_height.hash(&mut h);
        }
    }
    h.finish()
}

fn hash_json<T: Serialize>(value: &T, h: &mut DefaultHasher) {
    if let Ok(bytes) = serde_json::to_vec(value) {
        bytes.hash(h);
    }
}

fn hash_binding_samples(binding: &Binding, times: &[f64], h: &mut DefaultHasher) {
    match binding {
        Binding::Constant(value) => hash_json(value, h),
        Binding::Connection(socket) => hash_json(socket, h),
        Binding::Keyframes(_) | Binding::Components(_) => {
            for &time in times {
                hash_json(&binding.evaluate(time), h);
            }
        }
    }
}

fn hash_composite(composite: &LayerComposite, times: &[f64], h: &mut DefaultHasher) {
    hash_binding_samples(&composite.opacity, times, h);
    hash_binding_samples(&composite.blend_mode, times, h);
    hash_binding_samples(&composite.alpha_blend_mode, times, h);
}

fn hash_host_binding_samples(binding: &HostBinding, times: &[f64], h: &mut DefaultHasher) {
    match binding {
        HostBinding::Constant(value) => hash_json(value, h),
        HostBinding::Gpu(binding) => hash_binding_samples(binding, times, h),
        HostBinding::Keyframes(_) | HostBinding::Components(_) => {
            for &time in times {
                hash_json(&binding.evaluate(time), h);
            }
        }
    }
}

fn hash_visual_source(
    project: &Project,
    source: &VisualSource,
    times: &[f64],
    h: &mut DefaultHasher,
    depth: usize,
) {
    match source {
        VisualSource::Media(id) => {
            0u8.hash(h);
            id.hash(h);
        }
        VisualSource::Audio(id) => {
            1u8.hash(h);
            id.hash(h);
        }
        VisualSource::Generator(GeneratorSource::Plugin {
            generator_type,
            parameters,
        }) => {
            2u8.hash(h);
            generator_type.hash(h);
            for (name, binding) in parameters {
                name.hash(h);
                hash_host_binding_samples(binding, times, h);
            }
        }
        VisualSource::Generator(GeneratorSource::Wasm {
            plugin_id,
            module,
            entry,
            parameters,
        }) => {
            3u8.hash(h);
            plugin_id.hash(h);
            module.hash(h);
            entry.hash(h);
            for (name, binding) in parameters {
                name.hash(h);
                hash_host_binding_samples(binding, times, h);
            }
        }
        VisualSource::EffectInput => 4u8.hash(h),
        VisualSource::AudioPlaceholder => 5u8.hash(h),
        VisualSource::Composition(id) => {
            6u8.hash(h);
            id.hash(h);
            hash_composition_source(project, *id, times, h, depth.saturating_add(1));
        }
    }
}

fn hash_composition_source(
    project: &Project,
    composition_id: u64,
    times: &[f64],
    h: &mut DefaultHasher,
    depth: usize,
) {
    if depth >= 16 || times.is_empty() {
        return;
    }
    let Some(composition) = project.composition(composition_id) else {
        return;
    };
    hash_json(&composition.settings, h);
    let timeline = &composition.timeline;
    let visual_tracks = timeline
        .tracks
        .iter()
        .filter(|track| track.kind != TrackKind::Audio)
        .collect::<Vec<_>>();
    for track in &visual_tracks {
        if track.solo {
            track.id.hash(h);
        }
    }
    let has_solo = visual_tracks.iter().any(|track| track.solo);
    let mut media_ids = BTreeSet::new();
    for track in visual_tracks {
        let active_clips = timeline
            .clips
            .iter()
            .filter(|clip| {
                clip.track == track.id
                    && times
                        .iter()
                        .any(|time| *time >= f64::from(clip.start) && *time < f64::from(clip.end()))
            })
            .collect::<Vec<_>>();
        if active_clips.is_empty() {
            continue;
        }
        track.id.hash(h);
        track.muted.hash(h);
        track.solo.hash(h);
        if has_solo && !track.solo {
            continue;
        }
        hash_composite(&track.composite, times, h);
        if let Some(instance) = &track.pipeline {
            hash_pipeline_instance(project, instance, times, h);
        }
        for clip in active_clips {
            clip.track.hash(h);
            clip.start.to_bits().hash(h);
            if matches!(
                &clip.source,
                VisualSource::Media(_) | VisualSource::Composition(_)
            ) {
                clip.speed.to_bits().hash(h);
                clip.source_offset.to_bits().hash(h);
            }
            let active_times = times
                .iter()
                .filter(|time| **time >= f64::from(clip.start) && **time < f64::from(clip.end()))
                .copied()
                .collect::<Vec<_>>();
            active_times.len().hash(h);
            let source_times = if matches!(&clip.source, VisualSource::Composition(_)) {
                active_times
                    .iter()
                    .map(|time| clip.looped_source_time(*time as f32, project))
                    .collect::<Vec<_>>()
            } else {
                active_times.clone()
            };
            let row = timeline.property_row(clip.track, &clip.source, clip.source_instance);
            clip.opacity.to_bits().hash(h);
            let source_state = row.map_or(&clip.source, |row| &row.source);
            hash_visual_source(project, source_state, &source_times, h, depth);
            hash_pipeline_instance(
                project,
                row.map_or(&clip.pipeline, |row| &row.pipeline),
                &active_times,
                h,
            );
            hash_composite(
                row.map_or(&clip.composite, |row| &row.composite),
                &active_times,
                h,
            );
            if let VisualSource::Media(id) = &clip.source {
                media_ids.insert(*id);
            }
        }
    }
    for id in media_ids {
        if let Some(asset) = project.media(id) {
            asset.id.hash(h);
            asset.path.hash(h);
            hash_json(&asset.kind, h);
            hash_json(&asset.duration, h);
            hash_json(&asset.frame_rate, h);
            asset.video_width.hash(h);
            asset.video_height.hash(h);
        }
    }
}

fn hash_pipeline_instance(
    project: &Project,
    instance: &PipelineInstance,
    times: &[f64],
    h: &mut DefaultHasher,
) {
    hash_json(&instance.local_output, h);
    hash_effect_nodes(&instance.local_nodes, Some(instance), times, h);
    instance.pipeline.hash(h);

    if let Some(id) = instance.pipeline
        && let Some(pipeline) = project.pipeline(id)
    {
        hash_json(&pipeline.kind, h);
        hash_json(&pipeline.output, h);
        for node in pipeline.main_path() {
            hash_effect_node(node, Some(instance), times, h);
        }
        for value_node in &pipeline.value_nodes {
            value_node.id.hash(h);
            hash_json(&value_node.kind, h);
            hash_json(&value_node.value, h);
            for (name, binding) in &value_node.inputs {
                name.hash(h);
                hash_binding_samples(binding, times, h);
            }
            if value_node.kind.is_runtime_source() {
                for time in times {
                    time.to_bits().hash(h);
                }
            }
        }
    }
}

fn hash_effect_nodes(
    nodes: &[crate::effects::EffectNode],
    instance: Option<&PipelineInstance>,
    times: &[f64],
    h: &mut DefaultHasher,
) {
    for node in nodes {
        hash_effect_node(node, instance, times, h);
    }
}

fn hash_effect_node(
    node: &crate::effects::EffectNode,
    instance: Option<&PipelineInstance>,
    times: &[f64],
    h: &mut DefaultHasher,
) {
    node.id.hash(h);
    node.node_type.hash(h);
    hash_json(&node.execution, h);
    hash_json(&node.image_inputs, h);
    hash_json(&node.dynamic_image_inputs, h);
    for (name, binding) in &node.host_inputs {
        name.hash(h);
        hash_host_binding_samples(binding, times, h);
    }
    for (name, default_binding) in &node.inputs {
        name.hash(h);
        let binding = instance
            .and_then(|instance| instance.overrides.get(node.id, name))
            .unwrap_or(default_binding);
        hash_binding_samples(binding, times, h);
    }
}

struct CompositionCache {
    composition: CompositionId,
    canvas_size: [u32; 2],
    fps: f64,
    chunks: Arc<Mutex<Vec<CacheChunk>>>,
}

pub(super) struct RenderSession {
    task: Option<RenderTask>,
    caches: Vec<CompositionCache>,
    cache_dirs: Vec<PathBuf>,
    timeline_status_version: u64,
}

impl Default for RenderSession {
    fn default() -> Self {
        Self {
            task: None,
            caches: Vec::new(),
            cache_dirs: Vec::new(),
            timeline_status_version: u64::MAX,
        }
    }
}

impl Drop for RenderSession {
    fn drop(&mut self) {
        if let Some(mut task) = self.task.take() {
            task.cancel();
            drop(task);
        }
        let paths = std::mem::take(&mut self.cache_dirs);
        let _ = thread::Builder::new()
            .name("kama-render-cache-cleanup".into())
            .spawn(move || {
                for path in paths {
                    let _ = fs::remove_dir_all(path);
                }
            });
    }
}

impl RenderSession {
    fn cache_for(&mut self, project: &Project) -> Arc<Mutex<Vec<CacheChunk>>> {
        let settings = project.active_settings();
        let fps = settings.frame_rate.max(1.0);
        let composition = project.active_composition;
        if let Some(cache) = self.caches.iter().find(|cache| {
            cache.composition == composition
                && cache.canvas_size == settings.canvas_size
                && (cache.fps - fps).abs() <= f64::EPSILON
        }) {
            return Arc::clone(&cache.chunks);
        }
        let chunks = Arc::new(Mutex::new(Vec::new()));
        self.caches.push(CompositionCache {
            composition: project.active_composition,
            canvas_size: settings.canvas_size,
            fps,
            chunks: Arc::clone(&chunks),
        });
        chunks
    }

    pub(super) fn phase(&self) -> RenderPhase {
        self.task
            .as_ref()
            .map_or(RenderPhase::Idle, |task| task.status.phase)
    }

    pub(super) fn is_active(&self) -> bool {
        matches!(
            self.phase(),
            RenderPhase::Rendering | RenderPhase::Paused | RenderPhase::Transcoding
        )
    }

    pub(super) fn has_pending_update(&self) -> bool {
        self.task
            .as_ref()
            .is_some_and(RenderTask::has_pending_update)
    }

    pub(super) fn pause(&mut self) {
        if let Some(task) = self.task.as_mut() {
            task.pause();
        }
    }

    pub(super) fn resume(&mut self) {
        if let Some(task) = self.task.as_mut() {
            task.resume();
        }
    }

    pub(super) fn set_background(&mut self, background: bool) {
        if let Some(task) = self.task.as_mut() {
            task.set_background(background);
        }
    }

    pub(super) fn cancel(&mut self) {
        if let Some(task) = self.task.as_mut() {
            task.stop();
        }
        self.timeline_status_version = u64::MAX;
    }

    pub(super) fn start(
        &mut self,
        settings: RenderSpec,
        renderer: &Renderer,
        project: &Project,
        timeline: &TimelineState,
        plugins: &PluginRegistry,
        interaction: (u64, bool, bool),
    ) -> Result<()> {
        if let Some(task) = self.task.as_mut().filter(|task| task.can_restart(project)) {
            task.update_if_edited(interaction.0, project, timeline);
            task.restart(settings);
            self.timeline_status_version = u64::MAX;
            return Ok(());
        }

        drop(self.task.take());
        let cache = self.cache_for(project);
        let task = RenderTask::spawn(
            settings,
            renderer,
            project,
            timeline,
            plugins,
            &cache,
            interaction,
        )?;
        if !self.cache_dirs.contains(&task.cache_dir) {
            self.cache_dirs.push(task.cache_dir.clone());
        }
        self.task = Some(task);
        self.timeline_status_version = u64::MAX;
        Ok(())
    }

    pub(super) fn update(
        &mut self,
        live_end_frame: u64,
        renderer: &Renderer,
        project: &Project,
        timeline: &TimelineState,
        plugins: &PluginRegistry,
        interaction: (u64, bool, bool),
    ) {
        let (edit_revision, editing, interactive) = interaction;
        let restart_for_format_change = self.task.as_ref().is_some_and(|task| {
            let Some(target) = project.composition(task.composition) else {
                return false;
            };
            matches!(
                task.status.phase,
                RenderPhase::Rendering | RenderPhase::Paused
            ) && (task.canvas_size != target.settings.canvas_size
                || (task.fps - target.settings.frame_rate.max(1.0)).abs() > f64::EPSILON)
        });
        if restart_for_format_change {
            self.restart_for_format_change(renderer, project, timeline, plugins, interaction);
        }

        if let Some(task) = self.task.as_mut() {
            if matches!(
                task.status.phase,
                RenderPhase::Rendering | RenderPhase::Paused
            ) {
                let live_end = live_end_frame.max(task.settings.begin_frame);
                if live_end != task.settings.end_frame {
                    task.set_end_frame(live_end);
                }
            }
            task.update_editing(editing);
            task.update_interactive(interactive);
            task.update_if_edited(edit_revision, project, timeline);
            task.poll();
        }
    }

    fn restart_for_format_change(
        &mut self,
        renderer: &Renderer,
        project: &Project,
        timeline: &TimelineState,
        plugins: &PluginRegistry,
        interaction: (u64, bool, bool),
    ) {
        let Some(task) = self.task.as_mut() else {
            return;
        };
        let settings = task.settings.clone();
        let target_composition = task.composition;
        task.cancel();

        let mut target_project = render_worker_project_snapshot(project, timeline.document());
        let target_timeline = if target_project.set_active_composition(target_composition) {
            TimelineState::from_document(target_project.active_composition().timeline.clone())
        } else {
            TimelineState::from_document(timeline.document())
        };
        drop(self.task.take());
        let cache = self.cache_for(&target_project);
        match RenderTask::spawn(
            settings,
            renderer,
            &target_project,
            &target_timeline,
            plugins,
            &cache,
            interaction,
        ) {
            Ok(replacement) => {
                if !self.cache_dirs.contains(&replacement.cache_dir) {
                    self.cache_dirs.push(replacement.cache_dir.clone());
                }
                self.task = Some(replacement);
                self.timeline_status_version = u64::MAX;
            }
            Err(_) => self.task = None,
        }
    }

    pub(super) fn sync_timeline_ranges(
        &mut self,
        timeline: &mut TimelineState,
        active_composition: CompositionId,
    ) {
        let version = self
            .task
            .as_ref()
            .map_or(u64::MAX, |task| task.status_version);
        let target_matches = self
            .task
            .as_ref()
            .is_some_and(|task| task.composition == active_composition);
        let effective_version = version ^ active_composition.rotate_left(13);
        if effective_version == self.timeline_status_version {
            return;
        }
        self.timeline_status_version = effective_version;
        timeline.set_render_cache_ranges(if target_matches {
            self.task
                .as_ref()
                .map_or_else(Vec::new, |task| task.status.ranges.clone())
        } else {
            Vec::new()
        });
    }

    pub(super) fn cached_preview(
        &self,
        timeline_time: f32,
        fps: f64,
        active_composition: CompositionId,
    ) -> Option<RenderCachePreview> {
        let frame =
            super::nonnegative_frame((f64::from(timeline_time.max(0.0)) * fps.max(1.0)).floor());
        let task = self.task.as_ref()?;
        if task.composition != active_composition || task.has_pending_video_update() {
            return None;
        }
        let range = task
            .status
            .preview_ranges
            .iter()
            .find(|range| frame >= range.start_frame && frame <= range.end_frame)?;
        Some(RenderCachePreview {
            path: range.path.clone(),
            local_time: frame.saturating_sub(range.source_start_frame) as f64 / fps.max(1.0),
            generation: range.generation,
            frame,
        })
    }
}

use std::{
    collections::{BTreeSet, HashSet, VecDeque, hash_map::DefaultHasher},
    fs,
    hash::{Hash, Hasher},
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Error, Result};
use kama_ui::Renderer;
use serde::{Deserialize, Serialize};

use super::{DEFAULT_CACHE_CHUNK_SECONDS, RenderSpec, cache_encoder::CacheEncoder};
use crate::{
    audio::render_audio_wav,
    effects::{Binding, EffectRuntime, PipelineInstance},
    file_io::replace_file,
    playback::{ExportPixelFormat, ExportYuvBatchArgs, FrameRenderer, RenderCachePreview},
    plugin::PluginRegistry,
    project::{CompositionId, GeneratorSource, HostBinding, LayerComposite, Project, VisualSource},
    runtime::media::prioritize_offline_export,
    timeline::{
        EndBehavior, RenderCacheRange, RenderCacheState, TimelineDocument, TimelineState,
        TimelineViewState, TrackKind,
    },
};

#[cfg(test)]
mod tests {
    use std::{io::Write, process::Command};

    use super::*;
    use crate::project::{MediaAsset, MediaKind};

    #[test]
    fn render_worker_child_entry() {
        if std::env::var_os("KAMA_RENDER_WORKER_TEST_CHILD").is_some() {
            super::run_render_process_worker().expect("render worker child protocol exits cleanly");
        }
    }

    #[test]
    fn protocol_errors_retain_committed_cache_and_preview_ranges() {
        let cache_path = std::env::temp_dir().join(format!(
            "kama-render-cache-error-{}-{}.mov",
            std::process::id(),
            NEXT_CACHE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&cache_path, b"committed preview").unwrap();
        let chunk = CacheChunk {
            start: 4,
            end: 9,
            source_start: 4,
            source_end: 9,
            path: cache_path.clone(),
            state: ChunkState::Rendered,
            signature: 87,
            generation: 2,
        };
        let latest_status = Mutex::new(None);
        let shared_cache = Mutex::new(vec![chunk]);

        let status = error_status_from_cache(
            "malformed worker response".into(),
            &latest_status,
            &shared_cache,
        );

        assert_eq!(status.phase, RenderPhase::Error);
        assert_eq!(status.cached_chunks.len(), 1);
        assert_eq!(status.cached_chunks[0].path, cache_path);
        assert_eq!(status.preview_ranges.len(), 1);
        assert_eq!(
            (
                status.preview_ranges[0].start_frame,
                status.preview_ranges[0].end_frame
            ),
            (4, 9)
        );
        fs::remove_file(cache_path).unwrap();
    }

    #[test]
    fn command_pipe_eof_requests_child_cancellation() {
        let (commands, receiver) = mpsc::sync_channel(1);
        render_process_command_reader(std::io::Cursor::new(&b""[..]), &commands);

        assert!(matches!(
            receiver.try_recv(),
            Ok(RenderWorkerCommand::Cancel)
        ));
    }

    #[test]
    fn startup_handoff_preserves_buffered_commands_in_order() {
        let fixture = Fixture::new();
        let start = RenderProcessStart {
            settings: fixture.job.settings.clone(),
            edit_revision: 17,
            editing: false,
            interactive: false,
            project: fixture.project.clone(),
            worker_state: crate::project::WorkerProjectState::capture(&fixture.project),
            timeline: fixture.timeline.document(),
            plugin_paths: String::new(),
            cache_dir: fixture.job.cache_dir.clone(),
            cached_chunks: Vec::new(),
            expected_gpu: None,
        };
        let mut input = Vec::new();
        for message in [
            RenderProcessMessage::Start(Box::new(start)),
            RenderProcessMessage::Command(RenderWorkerCommand::Pause),
            RenderProcessMessage::Command(RenderWorkerCommand::SetEndFrame(47)),
            RenderProcessMessage::Command(RenderWorkerCommand::Resume),
        ] {
            serde_json::to_writer(&mut input, &message).unwrap();
            input.push(b'\n');
        }
        let reader = std::io::BufReader::with_capacity(input.len(), std::io::Cursor::new(input));

        let (_, commands) = read_render_process_input(reader).unwrap();

        assert!(matches!(
            commands.recv_timeout(Duration::from_secs(1)),
            Ok(RenderWorkerCommand::Pause)
        ));
        assert!(matches!(
            commands.recv_timeout(Duration::from_secs(1)),
            Ok(RenderWorkerCommand::SetEndFrame(47))
        ));
        assert!(matches!(
            commands.recv_timeout(Duration::from_secs(1)),
            Ok(RenderWorkerCommand::Resume)
        ));
        assert!(matches!(
            commands.recv_timeout(Duration::from_secs(1)),
            Ok(RenderWorkerCommand::Cancel)
        ));
    }

    #[test]
    fn bounded_line_reader_discards_oversized_tail_and_recovers_at_next_line() {
        let mut reader = std::io::Cursor::new(b"abcdef\nok\n");
        let mut line = Vec::new();

        assert_eq!(
            read_bounded_line(&mut reader, &mut line, 3).unwrap(),
            Some(true)
        );
        assert_eq!(line, b"abc");
        line.clear();
        assert_eq!(
            read_bounded_line(&mut reader, &mut line, 3).unwrap(),
            Some(false)
        );
        assert_eq!(line, b"ok");
    }

    #[test]
    fn watchdog_times_out_after_30_seconds_without_protocol_activity() {
        let now = Instant::now();
        assert!(!render_worker_timed_out(
            now.checked_sub(Duration::from_secs(29))
                .unwrap_or_else(Instant::now),
            now
        ));
        assert!(render_worker_timed_out(
            now.checked_sub(Duration::from_secs(30))
                .unwrap_or_else(Instant::now),
            now
        ));
    }

    #[cfg(all(target_os = "macos", debug_assertions))]
    #[test]
    fn isolated_worker_transcodes_real_cache_and_crash_preserves_committed_chunk() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kama-render-process-test-{}-{unique}",
            std::process::id()
        ));
        let cache_dir = root.join("cache");
        let output = root.join("render.mp4");
        fs::create_dir_all(&root).unwrap();

        let source = root.join("red-then-blue.y4m");
        let mut file = fs::File::create(&source).unwrap();
        file.write_all(b"YUV4MPEG2 W16 H16 F2:1 Ip A1:1 C420mpeg2 XCOLORRANGE=LIMITED\n")
            .unwrap();
        for (luma, cb, cr) in [(81, 90, 240), (41, 240, 110)] {
            file.write_all(b"FRAME\n").unwrap();
            file.write_all(&[luma; 256]).unwrap();
            file.write_all(&[cb; 64]).unwrap();
            file.write_all(&[cr; 64]).unwrap();
        }
        drop(file);
        let mut project = Project::new();
        project.active_composition_mut().settings.canvas_size = [16, 16];
        project.active_composition_mut().settings.frame_rate = 2.0;
        project.active_composition_mut().settings.background =
            crate::project::ProjectBackground::Solid {
                color: [0.85, 0.12, 0.2, 1.0],
            };
        let media = project.import_media(&source).unwrap();
        let mut timeline =
            TimelineState::from_document(project.active_composition().timeline.clone());
        let track = timeline.tracks()[0].id;
        assert!(timeline.insert_media_clip_at(
            (track, 0.0),
            media,
            "red-then-blue.y4m".into(),
            false,
            Some(1.0),
            PipelineInstance::effect_default(),
        ));
        let mut preset = super::super::built_in_presets().remove(0);
        preset.include_audio = false;
        let settings = RenderSpec {
            preset,
            output: output.clone(),
            overwrite: true,
            begin_frame: 0,
            end_frame: 1,
            background: true,
            transcode: true,
        };
        let start = RenderProcessStart {
            settings,
            edit_revision: 7,
            editing: false,
            interactive: false,
            project: render_worker_project_snapshot(&project, timeline.document()),
            worker_state: crate::project::WorkerProjectState::capture(&project),
            timeline: timeline.document(),
            plugin_paths: crate::preferences::load_plugin_paths(),
            cache_dir: cache_dir.clone(),
            cached_chunks: Vec::new(),
            expected_gpu: None,
        };
        let mut child_guard = RenderChildGuard::new(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "render::engine::tests::render_worker_child_entry",
                    "--quiet",
                ])
                .env("KAMA_RENDER_WORKER_TEST_CHILD", "1")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn real render worker test process"),
        );
        let child = child_guard.child_mut();
        let pid = child.id();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let (command_tx, command_rx) = mpsc::sync_channel(8);
        thread::spawn(move || render_process_writer(stdin, command_rx));
        let latest_status = Arc::new(Mutex::new(None));
        let shared_cache = Arc::new(Mutex::new(Vec::new()));
        let diagnostics = Arc::new(Mutex::new(VecDeque::new()));
        {
            let latest_status = Arc::clone(&latest_status);
            let shared_cache = Arc::clone(&shared_cache);
            let diagnostics = Arc::clone(&diagnostics);
            let last_response_at = Arc::new(Mutex::new(Instant::now()));
            thread::spawn(move || {
                render_process_reader(
                    stdout,
                    &latest_status,
                    &shared_cache,
                    &diagnostics,
                    &last_response_at,
                );
            });
        }
        {
            let diagnostics = Arc::clone(&diagnostics);
            thread::spawn(move || render_process_stderr(stderr, &diagnostics));
        }
        command_tx
            .send(RenderProcessMessage::Start(Box::new(start)))
            .unwrap();

        let deadline = Instant::now()
            .checked_add(Duration::from_secs(90))
            .unwrap_or_else(Instant::now);
        let completed = loop {
            let latest = latest_status.lock().unwrap().take();
            if let Some(status) = latest {
                assert_ne!(status.phase, RenderPhase::Error, "{:?}", status.error);
                if status.phase == RenderPhase::Done {
                    break status;
                }
            }
            assert!(Instant::now() < deadline, "render child did not finish");
            assert!(
                child.try_wait().unwrap().is_none(),
                "render worker exited early"
            );
            thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(completed.cached_chunks.len(), 1);
        let committed_cache = completed.cached_chunks[0].path.clone();
        assert!(committed_cache.is_file());
        assert!(output.is_file());

        let mut input = ffmpeg_next::format::input(&output).unwrap();
        let video = input
            .streams()
            .best(ffmpeg_next::media::Type::Video)
            .unwrap();
        let stream_index = video.index();
        let mut decoder = ffmpeg_next::codec::context::Context::from_parameters(video.parameters())
            .unwrap()
            .decoder()
            .video()
            .unwrap();
        let mut decoded_frame = ffmpeg_next::frame::Video::empty();
        let mut chroma = Vec::new();
        for (stream, packet) in input.packets() {
            if stream.index() != stream_index {
                continue;
            }
            decoder.send_packet(&packet).unwrap();
            while decoder.receive_frame(&mut decoded_frame).is_ok() {
                assert_eq!(decoded_frame.format(), ffmpeg_next::format::Pixel::YUV420P);
                chroma.push([decoded_frame.data(1)[0], decoded_frame.data(2)[0]]);
            }
        }
        decoder.send_eof().unwrap();
        while decoder.receive_frame(&mut decoded_frame).is_ok() {
            assert_eq!(decoded_frame.format(), ffmpeg_next::format::Pixel::YUV420P);
            chroma.push([decoded_frame.data(1)[0], decoded_frame.data(2)[0]]);
        }
        assert_eq!(chroma.len(), 2);
        assert!(
            i16::from(chroma[0][1]) - i16::from(chroma[0][0]) > 40,
            "first frame must be red, got {chroma:?}"
        );
        assert!(
            i16::from(chroma[1][0]) - i16::from(chroma[1][1]) > 40,
            "second frame must be blue, got {chroma:?}"
        );
        drop(decoder);
        drop(input);

        command_tx
            .send(RenderProcessMessage::Command(RenderWorkerCommand::Crash))
            .unwrap();
        let crash_deadline = Instant::now() + Duration::from_secs(15);
        let exit = loop {
            if let Some(exit) = child.try_wait().unwrap() {
                break exit;
            }
            assert!(
                Instant::now() < crash_deadline,
                "crash command was not delivered"
            );
            thread::sleep(Duration::from_millis(10));
        };
        assert!(!exit.success(), "test crash must terminate only the child");
        cleanup_render_artifacts(&cache_dir, &output, pid);
        assert!(
            committed_cache.is_file(),
            "committed preview cache survives child crash"
        );
        validate_cache_chunk(&committed_cache, [16, 16], 2).unwrap();

        fs::remove_dir_all(root).unwrap();
    }

    struct Fixture {
        project: Project,
        timeline: TimelineState,
        job: RenderJob,
    }

    impl Fixture {
        fn new() -> Self {
            let mut project = Project::new();
            project.active_composition_mut().settings.canvas_size = [16, 16];
            project.active_composition_mut().settings.frame_rate = 1.0;
            let mut timeline =
                TimelineState::from_document(project.active_composition().timeline.clone());
            let track = timeline.tracks()[0].id;
            for id in 1..=10 {
                project.media.push(MediaAsset {
                    id,
                    name: format!("still-{id}"),
                    path: PathBuf::from(format!("still-{id}.png")),
                    absolute_path: PathBuf::new(),
                    relative_path: PathBuf::new(),
                    kind: MediaKind::Image {
                        width: 16,
                        height: 16,
                    },
                    duration: None,
                    frame_rate: None,
                    video_width: None,
                    video_height: None,
                    has_audio: false,
                    tracks: Vec::new(),
                    waveform: None,
                    legacy_model: None,
                });
                let (start, duration) = match id {
                    9 => (8.0, 8.0),
                    10 => (16.0, 8.0),
                    _ => (id.saturating_sub(1) as f32, 1.0),
                };
                assert!(timeline.insert_media_clip_at(
                    (track, start),
                    id,
                    format!("still-{id}"),
                    false,
                    Some(duration),
                    PipelineInstance::effect_default(),
                ));
            }
            let settings = RenderSpec {
                preset: super::super::built_in_presets().remove(0),
                output: PathBuf::from("unused-render-test.mp4"),
                overwrite: true,
                begin_frame: 0,
                end_frame: 23,
                background: true,
                transcode: false,
            };
            let mut job = RenderJob::new(
                settings,
                1.0,
                &project,
                &timeline,
                Arc::new(Mutex::new(Vec::new())),
            )
            .unwrap();
            job.settings.output = job.cache_dir.join("output.mp4");
            Self {
                project,
                timeline,
                job,
            }
        }

        fn commit_chunk(&mut self, index: usize) {
            self.job
                .start_chunk(index, &self.project, &self.timeline)
                .unwrap();
            let active = self.job.active.as_mut().unwrap();
            let pixels = vec![0; 16 * 16 * 8];
            for _ in active.start_frame..=active.end_frame {
                active.encoder.as_mut().unwrap().write_all(&pixels).unwrap();
            }
            active.next_frame = active.end_frame.saturating_add(1);
            self.job
                .finish_active(&self.project, &self.timeline)
                .unwrap();
        }

        fn commit_all(&mut self) {
            while let Some(index) = self.job.next_chunk() {
                self.commit_chunk(index);
            }
            self.job.phase = RenderPhase::Done;
        }

        fn restart(&mut self, begin: u64, end: u64) {
            let mut settings = self.job.settings.clone();
            settings.begin_frame = begin;
            settings.end_frame = end;
            let live_end_frame = AtomicU64::new(self.job.settings.end_frame);
            self.job
                .restart(settings, &self.project, &self.timeline, &live_end_frame);
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.job.stop();
            let _ = fs::remove_dir_all(&self.job.cache_dir);
        }
    }

    #[test]
    fn restarting_render_keeps_requested_range_after_live_end_update() {
        for end_frame in [7, 39] {
            let mut fixture = Fixture::new();
            fixture.job.phase = RenderPhase::Done;
            let live_end_frame = AtomicU64::new(23);
            let mut settings = fixture.job.settings.clone();
            settings.begin_frame = 3;
            settings.end_frame = end_frame;

            fixture.job.restart(
                settings,
                &fixture.project,
                &fixture.timeline,
                &live_end_frame,
            );
            fixture.job.update_live_end(
                live_end_frame.load(Ordering::Acquire),
                &fixture.project,
                &fixture.timeline,
            );

            assert_eq!(fixture.job.settings.end_frame, end_frame);
            assert_eq!(fixture.job.chunks.first().unwrap().start, 3);
            assert_eq!(fixture.job.chunks.last().unwrap().end, end_frame);
        }
    }

    #[test]
    fn repeated_render_of_unchanged_multi_source_chunks_schedules_no_frames() {
        let mut fixture = Fixture::new();
        fixture.commit_all();
        let paths = fixture
            .job
            .chunks
            .iter()
            .map(|chunk| chunk.path.clone())
            .collect::<Vec<_>>();

        for _ in 0..16 {
            fixture.restart(0, 23);
            assert_eq!(fixture.job.next_chunk(), None);
            assert_eq!(
                fixture
                    .job
                    .chunks
                    .iter()
                    .map(|chunk| chunk.path.clone())
                    .collect::<Vec<_>>(),
                paths
            );
        }
    }

    #[test]
    fn a_local_visual_edit_only_schedules_its_chunk() {
        let mut fixture = Fixture::new();
        fixture.commit_all();
        let mut document = fixture.timeline.document();
        document
            .clips
            .iter_mut()
            .find(|clip| clip.start.to_bits() == 8.0f32.to_bits())
            .unwrap()
            .opacity = 0.35;
        fixture.timeline = TimelineState::from_document(document);

        fixture
            .job
            .invalidate_if_edited(&fixture.project, &fixture.timeline);

        assert_eq!(
            fixture
                .job
                .chunks
                .iter()
                .map(|chunk| chunk.state)
                .collect::<Vec<_>>(),
            vec![
                ChunkState::Rendered,
                ChunkState::Dirty,
                ChunkState::Rendered
            ]
        );
        fixture.commit_chunk(1);
        assert_eq!(fixture.job.next_chunk(), None);
    }

    #[test]
    fn trimming_a_constant_clip_invalidates_the_affected_chunk() {
        let mut fixture = Fixture::new();
        fixture.commit_all();
        let mut document = fixture.timeline.document();
        document
            .clips
            .iter_mut()
            .find(|clip| clip.start.to_bits() == 8.0f32.to_bits())
            .unwrap()
            .duration = 3.0;
        fixture.timeline = TimelineState::from_document(document);

        fixture
            .job
            .invalidate_if_edited(&fixture.project, &fixture.timeline);

        assert_eq!(fixture.job.next_chunk(), Some(1));
        assert_eq!(fixture.job.chunks[2].state, ChunkState::Rendered);
    }

    #[test]
    fn stopping_preserves_committed_preview_and_restart_only_schedules_missing_frames() {
        let mut fixture = Fixture::new();
        fixture.commit_chunk(0);
        let committed = fixture.job.chunks[0].path.clone();
        fixture
            .job
            .start_chunk(1, &fixture.project, &fixture.timeline)
            .unwrap();

        fixture.job.stop();

        assert_eq!(fixture.job.phase, RenderPhase::Idle);
        assert!(fixture.job.active.is_none());
        assert_eq!(fixture.job.preview_ranges()[0].path, committed);
        fixture.restart(0, 23);
        assert_eq!(fixture.job.next_chunk(), Some(1));
        assert_eq!(fixture.job.chunks[0].path, committed);
    }

    #[test]
    fn edits_while_stopped_invalidate_cached_preview() {
        let mut fixture = Fixture::new();
        fixture.commit_chunk(0);
        fixture.job.stop();
        fixture.project.media[0].path = PathBuf::from("replacement.png");

        fixture
            .job
            .invalidate_if_edited(&fixture.project, &fixture.timeline);

        assert!(fixture.job.preview_ranges().is_empty());
    }

    #[test]
    fn narrowing_then_restoring_the_range_reuses_all_committed_frames() {
        let mut fixture = Fixture::new();
        fixture.commit_all();
        let first_path = fixture.job.chunks[0].path.clone();

        fixture.restart(3, 11);

        assert_eq!(fixture.job.next_chunk(), None);
        assert_eq!(
            (fixture.job.chunks[0].start, fixture.job.chunks[0].end),
            (3, 7)
        );
        assert_eq!(fixture.job.chunks[0].source_start, 0);
        assert_eq!(fixture.job.chunks[0].path, first_path);
        fixture.restart(0, 23);
        assert_eq!(fixture.job.next_chunk(), None);
        assert_eq!(fixture.job.chunks.len(), 3);
    }

    #[test]
    fn extending_the_range_only_schedules_uncached_frames() {
        let mut fixture = Fixture::new();
        fixture.restart(0, 10);
        fixture.commit_all();

        fixture.restart(0, 23);

        let missing = fixture
            .job
            .chunks
            .iter()
            .filter(|chunk| chunk.state != ChunkState::Rendered)
            .map(|chunk| (chunk.start, chunk.end))
            .collect::<Vec<_>>();
        assert_eq!(missing, vec![(11, 15), (16, 23)]);
    }

    #[test]
    fn replacing_the_worker_keeps_the_committed_cache() {
        let mut fixture = Fixture::new();
        fixture.commit_all();
        let mut replacement = RenderJob::new(
            fixture.job.settings.clone(),
            1.0,
            &fixture.project,
            &fixture.timeline,
            Arc::clone(&fixture.job.shared_cache),
        )
        .unwrap();

        assert_eq!(replacement.next_chunk(), None);
        assert_eq!(replacement.preview_ranges().len(), 3);
        replacement.stop();
        fs::remove_dir_all(&replacement.cache_dir).unwrap();
    }

    #[test]
    fn missing_cache_files_are_scheduled_again() {
        let mut fixture = Fixture::new();
        fixture.commit_all();
        fs::remove_file(&fixture.job.chunks[1].path).unwrap();

        fixture.restart(0, 23);

        assert_eq!(fixture.job.next_chunk(), Some(1));
        assert_eq!(fixture.job.chunks[0].state, ChunkState::Rendered);
        assert_eq!(fixture.job.chunks[2].state, ChunkState::Rendered);
    }

    #[test]
    fn changing_export_settings_does_not_schedule_visual_processing() {
        let mut fixture = Fixture::new();
        fixture.commit_all();
        let mut settings = fixture.job.settings.clone();
        settings.preset.video_codec = super::super::VideoCodec::Ffv1;
        settings.preset.resolution = super::super::RenderResolution::Hd;
        settings.preset.quality = 37;
        settings.transcode = true;
        settings.output = fixture.job.cache_dir.join("different-output.mkv");

        let live_end_frame = AtomicU64::new(23);
        fixture.job.restart(
            settings,
            &fixture.project,
            &fixture.timeline,
            &live_end_frame,
        );

        assert_eq!(fixture.job.next_chunk(), None);
    }

    #[test]
    fn in_progress_frames_are_visible_without_exposing_an_uncommitted_cache() {
        let mut fixture = Fixture::new();
        fixture.commit_chunk(0);
        fixture
            .job
            .start_chunk(1, &fixture.project, &fixture.timeline)
            .unwrap();
        fixture.job.active.as_mut().unwrap().next_frame = 12;

        let ranges = fixture.job.timeline_ranges();

        assert_eq!(ranges.len(), 2);
        assert_eq!((ranges[0].start, ranges[0].end), (0.0, 8.0));
        assert_eq!(ranges[0].state, RenderCacheState::Rendered);
        assert_eq!((ranges[1].start, ranges[1].end), (8.0, 12.0));
        assert_eq!(ranges[1].state, RenderCacheState::Rendering);
        assert_eq!(fixture.job.preview_ranges().len(), 1);
    }

    #[test]
    fn nested_compositions_with_multiple_sources_have_stable_cache_signatures() {
        let mut fixture = Fixture::new();
        let child = fixture.project.create_composition("Nested");
        fixture.project.composition_mut(child).unwrap().timeline = fixture.timeline.document();
        let mut document = fixture.timeline.document();
        document.clips.truncate(1);
        document.clips[0].source = VisualSource::Composition(child);
        document.clips[0].duration = 24.0;
        fixture.timeline = TimelineState::from_document(document);
        fixture.commit_all();

        for _ in 0..16 {
            fixture.restart(0, 23);
            assert_eq!(fixture.job.next_chunk(), None);
        }
    }

    #[test]
    fn cancelling_the_session_stops_work_without_losing_cached_playback() {
        let mut fixture = Fixture::new();
        fixture.commit_all();
        let (task, command_rx) = task_for_fixture(&fixture);
        let mut session = RenderSession::default();
        session.task = Some(task);

        session.cancel();

        assert!(matches!(
            command_rx.try_recv(),
            Ok(RenderProcessMessage::Command(RenderWorkerCommand::Stop))
        ));
        assert_eq!(session.phase(), RenderPhase::Idle);
        let preview = session
            .cached_preview(3.0, 1.0, fixture.project.active_composition)
            .unwrap();
        assert_eq!(preview.path, fixture.job.chunks[0].path);
        assert_eq!(preview.local_time, 3.0);
    }

    #[test]
    fn delayed_status_cannot_revive_a_terminated_render_worker() {
        let fixture = Fixture::new();
        let (mut task, _commands) = task_for_fixture(&fixture);
        task.status.phase = RenderPhase::Error;
        task.status.error = Some("render worker is unresponsive".into());
        let mut delayed = task.status.clone();
        delayed.phase = RenderPhase::Rendering;
        delayed.error = None;
        *task.latest_status.lock().unwrap() = Some(delayed);

        task.poll();

        assert_eq!(task.status.phase, RenderPhase::Error);
        assert_eq!(
            task.status.error.as_deref(),
            Some("render worker is unresponsive")
        );
    }

    fn task_for_fixture(fixture: &Fixture) -> (RenderTask, Receiver<RenderProcessMessage>) {
        let (command_tx, command_rx) = mpsc::sync_channel(8);
        let task = RenderTask {
            settings: fixture.job.settings.clone(),
            command_tx,
            latest_status: Arc::new(Mutex::new(None)),
            last_response_at: Arc::new(Mutex::new(Instant::now())),
            process: None,
            diagnostics: Arc::new(Mutex::new(VecDeque::new())),
            queued_commands: VecDeque::new(),
            pid: 0,
            cache_dir: fixture.job.cache_dir.clone(),
            status: RenderWorkerStatus::from_job(&fixture.job, 0),
            last_sent_revision: 0,
            last_seen_revision: 0,
            last_content_signature: 0,
            last_video_signature: 0,
            pending_video_revision: None,
            last_editing: false,
            last_interactive: false,
            reported_error: None,
            status_version: 0,
            canvas_size: [16, 16],
            fps: 1.0,
            composition: fixture.project.active_composition,
        };
        (task, command_rx)
    }
}
