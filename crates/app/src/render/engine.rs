#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChunkState {
    Pending,
    Rendering,
    Rendered,
    Dirty,
}

#[derive(Clone, Debug)]
struct CacheChunk {
    start: u64,
    end: u64,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RenderPhase {
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
    active: Option<ActiveChunk>,
    cache_dir: PathBuf,
    audio_path: PathBuf,
    next_generation: u64,
    cache_encoder_failures: Vec<u8>,
    settings: RenderSpec,
    fps: f64,
    canvas_size: [u32; 2],
    audio_mix: Option<JoinHandle<Result<()>>>,
    transcode: Option<Child>,
    transcode_used_vt_fast_path: bool,
    disable_vt_transcode_fast_path: bool,
    error: Option<String>,
}

impl RenderJob {
    fn new(settings: RenderSpec, fps: f64, project: &Project) -> Result<Self> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temp_dir = std::env::temp_dir();
        let cache_dir = temp_dir.join(format!("kama-render-{stamp}-{}", std::process::id()));
        let audio_path = temp_dir.join(format!(
            "kama-render-audio-{stamp}-{}.wav",
            std::process::id()
        ));
        fs::create_dir_all(&cache_dir)
            .with_context(|| format!("create {}", cache_dir.display()))?;
        let chunk_frames = (fps * cache_chunk_seconds()).round().max(1.0) as u64;
        let mut chunks = Vec::new();
        let mut start = settings.begin_frame;
        let mut index = 0usize;
        while start <= settings.end_frame {
            let end = (start + chunk_frames - 1).min(settings.end_frame);
            chunks.push(CacheChunk {
                start,
                end,
                path: cache_dir.join(format!("chunk-{index:06}.mov")),
                state: ChunkState::Pending,
                signature: 0,
                generation: 0,
            });
            start = end.saturating_add(1);
            index += 1;
        }
        let cache_encoder_failures = vec![0; chunks.len()];
        Ok(Self {
            phase: RenderPhase::Rendering,
            chunks,
            active: None,
            cache_dir,
            audio_path,
            next_generation: 1,
            cache_encoder_failures,
            settings,
            fps,
            canvas_size: project.active_settings().canvas_size,
            audio_mix: None,
            transcode: None,
            transcode_used_vt_fast_path: false,
            disable_vt_transcode_fast_path: false,
            error: None,
        })
    }

    fn update_live_end(&mut self, requested_end: u64, project: &Project, timeline: &TimelineState) {
        if matches!(
            self.phase,
            RenderPhase::Transcoding | RenderPhase::Done | RenderPhase::Error
        ) {
            return;
        }
        let new_end = requested_end.max(self.settings.begin_frame);
        if new_end == self.settings.end_frame {
            return;
        }
        let old_end = self.settings.end_frame;
        self.settings.end_frame = new_end;

        let chunk_frames = (self.fps * cache_chunk_seconds()).round().max(1.0) as u64;
        if new_end < old_end {
            if self
                .active
                .as_ref()
                .is_some_and(|active| active.start_frame > new_end)
            {
                self.abort_active();
            }
            let keep = self
                .chunks
                .iter()
                .rposition(|chunk| chunk.start <= new_end)
                .map_or(0, |index| index + 1);
            self.chunks.truncate(keep);
            self.cache_encoder_failures.truncate(keep);
            if let Some(chunk) = self.chunks.last_mut() {
                if chunk.end != new_end {
                    chunk.end = new_end;
                    if chunk.state == ChunkState::Rendered {
                        chunk.state = ChunkState::Dirty;
                    }
                }
            }
            if let Some(active) = self.active.as_mut() {
                if active.start_frame <= new_end && active.end_frame > new_end {
                    active.end_frame = new_end.max(active.next_frame.saturating_sub(1));
                    active.signature_at_start = range_signature(
                        project,
                        timeline,
                        active.start_frame,
                        active.end_frame,
                        self.fps,
                    );
                }
            }
        } else {
            if let Some(last) = self.chunks.last_mut() {
                let natural_end = last
                    .start
                    .saturating_add(chunk_frames.saturating_sub(1))
                    .min(new_end);
                if last.end < natural_end {
                    last.end = natural_end;
                    if last.state == ChunkState::Rendered {
                        last.state = ChunkState::Dirty;
                    }
                }
            }
            let mut start = self
                .chunks
                .last()
                .map_or(self.settings.begin_frame, |chunk| {
                    chunk.end.saturating_add(1)
                });
            while start <= new_end {
                let end = start
                    .saturating_add(chunk_frames.saturating_sub(1))
                    .min(new_end);
                let index = self.chunks.len();
                self.chunks.push(CacheChunk {
                    start,
                    end,
                    path: self.cache_dir.join(format!("chunk-{index:06}.mov")),
                    state: ChunkState::Pending,
                    signature: 0,
                    generation: 0,
                });
                self.cache_encoder_failures.push(0);
                start = end.saturating_add(1);
            }
            if let Some(active) = self.active.as_mut() {
                if active.end_frame == old_end {
                    active.end_frame = active
                        .start_frame
                        .saturating_add(chunk_frames.saturating_sub(1))
                        .min(new_end);
                    active.signature_at_start = range_signature(
                        project,
                        timeline,
                        active.start_frame,
                        active.end_frame,
                        self.fps,
                    );
                }
            }
        }
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

    fn restart(&mut self, settings: RenderSpec) {
        self.abort_active();
        if let Some(mut child) = self.transcode.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(task) = self.audio_mix.take() {
            let _ = task.join();
        }
        let _ = fs::remove_file(&self.audio_path);
        let _ = fs::remove_file(self.transcode_output_path());

        let chunk_frames = (self.fps * cache_chunk_seconds()).round().max(1.0) as u64;
        let mut previous = std::mem::take(&mut self.chunks);
        let mut chunks = Vec::new();
        let mut start = settings.begin_frame;
        while start <= settings.end_frame {
            let end = start
                .saturating_add(chunk_frames.saturating_sub(1))
                .min(settings.end_frame);
            if let Some(index) = previous
                .iter()
                .position(|chunk| chunk.start == start && chunk.end == end)
            {
                chunks.push(previous.swap_remove(index));
            } else {
                let index = chunks.len();
                chunks.push(CacheChunk {
                    start,
                    end,
                    path: self.cache_dir.join(format!("chunk-{index:06}.mov")),
                    state: ChunkState::Pending,
                    signature: 0,
                    generation: 0,
                });
            }
            start = end.saturating_add(1);
        }
        self.cache_encoder_failures = vec![0; chunks.len()];
        self.chunks = chunks;
        self.settings = settings;
        self.error = None;
        self.transcode_used_vt_fast_path = false;
        self.disable_vt_transcode_fast_path = false;
        self.phase = RenderPhase::Rendering;
    }

    fn abort_active(&mut self) {
        if let Some(active) = self.active.take() {
            drop(active.encoder);
            let _ = fs::remove_file(active.temp_path);
            if let Some(chunk) = self.chunks.get_mut(active.index) {
                if chunk.state == ChunkState::Rendering {
                    chunk.state = if chunk.signature != 0 || active.next_frame > chunk.start {
                        ChunkState::Dirty
                    } else {
                        ChunkState::Pending
                    };
                }
            }
        }
    }

    fn invalidate_if_edited(&mut self, project: &Project, timeline: &TimelineState) {
        if matches!(self.phase, RenderPhase::Error | RenderPhase::Idle) {
            return;
        }

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

        for chunk in &mut self.chunks {
            if chunk.signature == 0 || chunk.state == ChunkState::Rendering {
                continue;
            }
            let now = range_signature(project, timeline, chunk.start, chunk.end, self.fps);
            chunk.state = if now == chunk.signature {
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
        let chunk = &self.chunks[index];
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
        self.chunks[index].state = ChunkState::Rendering;
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
                format!("ProRes cache encoder failed: {error:#}"),
            );
        }

        let expected_frames = active.end_frame.saturating_sub(active.start_frame) + 1;
        if let Err(error) =
            validate_cache_chunk(&active.temp_path, self.canvas_size, expected_frames)
        {
            return self.schedule_cache_retry(
                active.index,
                &active.temp_path,
                format!("completed ProRes chunk validation failed: {error:#}"),
            );
        }

        let generation = self.next_generation;
        let committed_path = self
            .cache_dir
            .join(format!("chunk-{:06}-g{generation:016}.mov", active.index));

        replace_file(&active.temp_path, &committed_path)
            .with_context(|| format!("commit ProRes cache chunk {}", committed_path.display()))?;

        let chunk = &mut self.chunks[active.index];
        chunk.path = committed_path;
        chunk.signature = range_signature(project, timeline, chunk.start, chunk.end, self.fps);
        chunk.generation = generation;
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        chunk.state = ChunkState::Rendered;
        self.cache_encoder_failures[active.index] = 0;
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
            format!("cache encoder write failed: {write_error}"),
        )
    }

    fn schedule_cache_retry(
        &mut self,
        index: usize,
        temp_path: &std::path::Path,
        reason: String,
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

        let failures = &mut self.cache_encoder_failures[index];
        *failures = failures.saturating_add(1);
        if *failures <= 3 {
            return Ok(());
        }

        anyhow::bail!(
            "ProRes cache encoder repeatedly failed for frames {}..{}; all previously committed cache chunks were preserved. Last error: {}",
            chunk.start, chunk.end, reason
        )
    }

    fn begin_transcode(
        &mut self,
        project: &Project,
        timeline: &TimelineState,
        plugins: &PluginRegistry,
    ) -> Result<()> {
        let list_path = self.cache_dir.join("concat.txt");
        let mut list = BufWriter::new(File::create(&list_path)?);
        for chunk in &self.chunks {
            if chunk.state != ChunkState::Rendered || chunk.signature == 0 || !chunk.path.exists() {
                anyhow::bail!("render cache chunk {} is not valid", chunk.path.display());
            }
            let escaped = chunk
                .path
                .to_string_lossy()
                .replace('\\', "\\\\")
                .replace('\'', "'\\''");
            writeln!(list, "file '{escaped}'")?;
        }
        drop(list);

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
            let end = (self.settings.end_frame + 1) as f64 / self.fps;
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
        let list_path = self.cache_dir.join("concat.txt");
        let audio_path = self.audio_path.clone();
        if !self.settings.overwrite && self.settings.output.exists() {
            anyhow::bail!("output already exists: {}", self.settings.output.display());
        }
        let transcode_output = self.transcode_output_path();
        let _ = fs::remove_file(&transcode_output);
        let target_size = self.settings.preset.resolution.dimensions(self.canvas_size);
        let use_vt_fast_path = !self.disable_vt_transcode_fast_path
            && final_videotoolbox_pipeline_usable(self.settings.preset.video_codec);
        let mut command = Command::new(external_tool("ffmpeg"));
        command
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-y");
        if use_vt_fast_path {
            command.args([
                "-hwaccel",
                "videotoolbox",
                "-hwaccel_output_format",
                "videotoolbox_vld",
            ]);
        }
        command
            .arg("-f")
            .arg("concat")
            .arg("-safe")
            .arg("0")
            .arg("-i")
            .arg(&list_path);
        if self.settings.preset.include_audio {
            command.arg("-i").arg(&audio_path);
        }
        let needs_scale = target_size != self.canvas_size;
        if use_vt_fast_path {
            command.arg("-vf").arg(format!(
                "scale_vt=w={}:h={}:color_matrix=bt709:color_primaries=bt709:color_transfer=bt709",
                target_size[0], target_size[1]
            ));
        } else if needs_scale {
            command.arg("-vf").arg(format!(
                "scale={}:{}:flags=lanczos",
                target_size[0], target_size[1]
            ));
        }

        configure_target_video_encoder(
            &mut command,
            self.settings.preset.video_codec,
            self.settings.preset.quality,
            needs_scale,
            use_vt_fast_path,
        );
        if self.settings.preset.include_audio {
            match self.settings.preset.audio_codec {
                AudioCodec::Aac => {
                    command.arg("-c:a").arg("aac").arg("-b:a").arg(format!(
                        "{}k",
                        self.settings.preset.audio_bitrate_kbps.max(64)
                    ));
                }
                AudioCodec::Opus => {
                    command.arg("-c:a").arg("libopus").arg("-b:a").arg(format!(
                        "{}k",
                        self.settings.preset.audio_bitrate_kbps.max(64)
                    ));
                }
                AudioCodec::Flac => {
                    command.arg("-c:a").arg("flac");
                }
                AudioCodec::Pcm => {
                    command.arg("-c:a").arg("pcm_s24le");
                }
            }
            command.arg("-shortest");
        } else {
            command.arg("-an");
        }
        let frame_count = self
            .settings
            .end_frame
            .saturating_sub(self.settings.begin_frame)
            .saturating_add(1);
        let duration = frame_count as f64 / self.fps.max(1.0);
        command
            .arg("-frames:v")
            .arg(frame_count.to_string())
            .arg("-t")
            .arg(format!("{duration:.9}"));
        command
            .arg(&transcode_output)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        self.transcode_used_vt_fast_path = use_vt_fast_path;
        self.transcode = Some(command.spawn().context("start final ffmpeg transcode")?);
        Ok(())
    }

    fn poll_transcode(&mut self) -> Result<()> {
        if self
            .audio_mix
            .as_ref()
            .is_some_and(|task| task.is_finished())
        {
            let task = self
                .audio_mix
                .take()
                .expect("finished audio task disappeared");
            task.join()
                .map_err(|_| anyhow::anyhow!("offline audio render thread panicked"))??;
            self.spawn_final_transcode()?;
        }
        if self.audio_mix.is_some() {
            return Ok(());
        }
        let status = match self.transcode.as_mut() {
            Some(child) => child.try_wait()?,
            None => None,
        };
        if let Some(status) = status {
            self.transcode = None;
            let _ = fs::remove_file(&self.audio_path);
            let temporary = self.transcode_output_path();
            if status.success() {
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
            } else if self.transcode_used_vt_fast_path {
                let _ = fs::remove_file(&temporary);
                self.transcode_used_vt_fast_path = false;
                self.disable_vt_transcode_fast_path = true;
                self.spawn_final_transcode()?;
            } else {
                let _ = fs::remove_file(&temporary);
                self.phase = RenderPhase::Error;
                self.error = Some(format!("final transcode exited with {status}"));
            }
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
            if let Some(last) = ranges.last_mut() {
                if last.state == next.state && (last.end - next.start).abs() <= 0.000_1 {
                    last.end = last.end.max(next.end);
                    return;
                }
            }
            ranges.push(next);
        }

        let mut ranges = Vec::new();
        for (index, chunk) in self.chunks.iter().enumerate() {
            let start = chunk.start as f32 / self.fps as f32;
            let end = (chunk.end + 1) as f32 / self.fps as f32;
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
                    if let Some(active) =
                        self.active.as_ref().filter(|active| active.index == index)
                    {
                        let done_end =
                            active.next_frame.min(chunk.end + 1) as f32 / self.fps as f32;
                        if done_end > start {
                            push_merged(
                                &mut ranges,
                                RenderCacheRange {
                                    start,
                                    end: done_end,
                                    state: RenderCacheState::Rendered,
                                },
                            );
                        }
                        if done_end < end && chunk.signature != 0 {
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
                }
                ChunkState::Pending => {}
            }
        }
        ranges
    }

    fn preview_ranges(&self) -> Vec<CachePreviewRange> {
        self.chunks
            .iter()
            .filter(|chunk| {
                chunk.state == ChunkState::Rendered && chunk.generation != 0 && chunk.path.exists()
            })
            .map(|chunk| CachePreviewRange {
                start_frame: chunk.start,
                end_frame: chunk.end,
                source_start_frame: chunk.start,
                path: chunk.path.clone(),
                generation: chunk.generation,
            })
            .collect()
    }
}

impl Drop for RenderJob {
    fn drop(&mut self) {
        self.abort_active();
        if let Some(mut child) = self.transcode.take() {
            let _ = child.kill();
            let _ = child.wait();
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

#[derive(Clone, Debug)]
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
    error: Option<String>,
    update_revision: u64,
}

impl RenderWorkerStatus {
    fn from_job(job: &RenderJob, update_revision: u64) -> Self {
        Self {
            phase: job.phase,
            ranges: job.timeline_ranges(),
            preview_ranges: job.preview_ranges(),
            error: job.error.clone(),
            update_revision,
        }
    }
}

enum RenderWorkerCommand {
    Pause,
    Resume,
    Restart(RenderSpec),
    SetBackground(bool),
    SetEditing(bool),
    SetInteractive(bool),
    Update {
        revision: u64,
        project: Project,
        video_changed: bool,
    },
    Cancel,
}

struct RenderTask {
    settings: RenderSpec,
    command_tx: Sender<RenderWorkerCommand>,
    status_rx: Receiver<RenderWorkerStatus>,
    worker: Option<JoinHandle<()>>,
    cache_dir: PathBuf,
    status: RenderWorkerStatus,
    last_sent_revision: u64,
    last_seen_revision: u64,
    last_content_signature: u64,
    last_video_signature: u64,
    pending_video_revision: Option<u64>,
    last_editing: bool,
    last_interactive: bool,
    status_version: u64,
    canvas_size: [u32; 2],
    fps: f64,
    composition: CompositionId,
    live_end_frame: Arc<AtomicU64>,
}

impl RenderTask {
    fn spawn(
        settings: RenderSpec,
        renderer: &Renderer,
        project: &Project,
        timeline: &TimelineState,
        plugins: &PluginRegistry,
        interaction: (u64, bool, bool),
    ) -> Result<Self> {
        let (edit_revision, editing, interactive) = interaction;
        let composition = project.active_composition;
        let fps = project.active_settings().frame_rate.max(1.0);
        let canvas_size = project.active_settings().canvas_size;
        let job = RenderJob::new(settings.clone(), fps, project)?;
        let cache_dir = job.cache_dir.clone();
        let initial_status = RenderWorkerStatus::from_job(&job, edit_revision);
        let device = renderer.device_handle();
        let queue = renderer.queue_handle();
        let worker_document = timeline.document();
        let last_content_signature = render_content_signature(project, &worker_document);
        let last_video_signature = video_content_signature(project, &worker_document);
        let mut worker_project = render_worker_project_snapshot(project, worker_document.clone());
        let worker_plugins = plugins.clone();
        let (command_tx, command_rx) = mpsc::channel();
        let (status_tx, status_rx) = mpsc::sync_channel(1);
        let live_end_frame = Arc::new(AtomicU64::new(settings.end_frame));
        let worker_live_end_frame = Arc::clone(&live_end_frame);
        let worker = thread::Builder::new()
            .name("kama-render".into())
            .spawn(move || {
                let mut worker_timeline = TimelineState::from_document(worker_document);
                let mut effects = EffectRuntime::default();
                effects.rebuild(&worker_project.pipelines);
                let mut frame_renderer =
                    FrameRenderer::new_export_worker(&device, &effects, &worker_plugins);
                run_render_worker(
                    job,
                    edit_revision,
                    editing,
                    interactive,
                    device,
                    queue,
                    &mut worker_project,
                    &mut worker_timeline,
                    &mut effects,
                    &worker_plugins,
                    command_rx,
                    status_tx,
                    &mut frame_renderer,
                    composition,
                    worker_live_end_frame,
                );
            })
            .context("start render worker thread")?;
        Ok(Self {
            settings,
            command_tx,
            status_rx,
            worker: Some(worker),
            cache_dir,
            status: initial_status,
            last_sent_revision: edit_revision,
            last_seen_revision: edit_revision,
            last_content_signature,
            last_video_signature,
            pending_video_revision: None,
            last_editing: editing,
            last_interactive: interactive,
            status_version: 0,
            canvas_size,
            fps,
            composition,
            live_end_frame,
        })
    }

    fn poll(&mut self) {
        while let Ok(status) = self.status_rx.try_recv() {
            self.status = status;
            if self
                .pending_video_revision
                .is_some_and(|revision| revision == self.status.update_revision)
            {
                self.pending_video_revision = None;
            }
            self.status_version = self.status_version.wrapping_add(1);
        }
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.is_finished())
        {
            if let Some(worker) = self.worker.take() {
                if worker.join().is_err() && self.status.phase != RenderPhase::Error {
                    self.status.phase = RenderPhase::Error;
                    self.status.error = Some("render worker thread panicked".into());
                }
            }
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
        self.last_seen_revision = edit_revision;
        let document = timeline.document();
        let signature = render_content_signature(project, &document);

        if signature == self.last_content_signature {
            return;
        }
        let video_signature = video_content_signature(project, &document);
        let video_changed = video_signature != self.last_video_signature;
        self.last_content_signature = signature;
        self.last_video_signature = video_signature;
        self.last_sent_revision = edit_revision;
        let snapshot = render_worker_project_snapshot(project, document);
        if self
            .command_tx
            .send(RenderWorkerCommand::Update {
                revision: edit_revision,
                project: snapshot,
                video_changed,
            })
            .is_ok()
            && video_changed
        {
            self.pending_video_revision = Some(edit_revision);
        }
    }

    fn update_editing(&mut self, editing: bool) {
        if self.last_editing == editing {
            return;
        }
        self.last_editing = editing;
        let _ = self
            .command_tx
            .send(RenderWorkerCommand::SetEditing(editing));
    }

    fn update_interactive(&mut self, interactive: bool) {
        if self.last_interactive == interactive {
            return;
        }
        self.last_interactive = interactive;
        let _ = self
            .command_tx
            .send(RenderWorkerCommand::SetInteractive(interactive));
    }

    fn has_pending_update(&self) -> bool {
        self.status.phase != RenderPhase::Error
            && self.last_sent_revision != self.status.update_revision
    }

    fn has_pending_video_update(&self) -> bool {
        self.status.phase != RenderPhase::Error && self.pending_video_revision.is_some()
    }

    fn pause(&mut self) {
        let _ = self.command_tx.send(RenderWorkerCommand::Pause);
        if self.status.phase == RenderPhase::Rendering {
            self.status.phase = RenderPhase::Paused;
        }
    }

    fn resume(&mut self) {
        let _ = self.command_tx.send(RenderWorkerCommand::Resume);
        if self.status.phase == RenderPhase::Paused {
            self.status.phase = RenderPhase::Rendering;
        }
    }

    fn restart(&mut self, settings: RenderSpec) {
        self.live_end_frame
            .store(settings.end_frame, Ordering::Release);
        self.settings = settings.clone();
        if self
            .command_tx
            .send(RenderWorkerCommand::Restart(settings))
            .is_ok()
        {
            self.status.phase = RenderPhase::Rendering;
            self.status.error = None;
        }
    }

    fn can_restart(&self, project: &Project) -> bool {
        self.worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
            && project.active_composition == self.composition
            && self.canvas_size == project.active_settings().canvas_size
            && (self.fps - project.active_settings().frame_rate.max(1.0)).abs() <= f64::EPSILON
    }

    fn set_background(&mut self, background: bool) {
        self.settings.background = background;
        let _ = self
            .command_tx
            .send(RenderWorkerCommand::SetBackground(background));
    }

    fn set_end_frame(&mut self, end_frame: u64) {
        let end_frame = end_frame.max(self.settings.begin_frame);
        self.settings.end_frame = end_frame;

        self.live_end_frame.store(end_frame, Ordering::Release);
    }

    fn cancel(&mut self) {
        let _ = self.command_tx.send(RenderWorkerCommand::Cancel);
    }
}

impl Drop for RenderTask {
    fn drop(&mut self) {
        self.cancel();

        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_render_worker(
    mut job: RenderJob,
    mut update_revision: u64,
    mut editing: bool,
    mut interactive: bool,
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    project: &mut Project,
    timeline: &mut TimelineState,
    effects: &mut EffectRuntime,
    plugins: &PluginRegistry,
    command_rx: Receiver<RenderWorkerCommand>,
    status_tx: SyncSender<RenderWorkerStatus>,
    frame_renderer: &mut FrameRenderer,
    target_composition: CompositionId,
    live_end_frame: Arc<AtomicU64>,
) {
    let mut last_status_at = Instant::now() - Duration::from_millis(100);
    let mut last_status_phase = job.phase;

    let _ = status_tx.try_send(RenderWorkerStatus::from_job(&job, update_revision));
    loop {
        let mut cancelled = false;
        while let Ok(command) = command_rx.try_recv() {
            match command {
                RenderWorkerCommand::Pause => job.pause(),
                RenderWorkerCommand::Resume => job.resume(),
                RenderWorkerCommand::Restart(settings) => job.restart(settings),
                RenderWorkerCommand::SetBackground(background) => {
                    job.settings.background = background;
                }
                RenderWorkerCommand::SetEditing(value) => {
                    editing = value;
                }
                RenderWorkerCommand::SetInteractive(value) => {
                    interactive = value;
                }
                RenderWorkerCommand::Update {
                    revision,
                    project: mut updated_project,
                    video_changed,
                } => {
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
            }
        }
        if cancelled {
            return;
        }

        job.update_live_end(live_end_frame.load(Ordering::Acquire), project, timeline);

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
                    device: &device,
                    queue: &queue,
                    project,
                    timeline,
                    runtime: (effects, plugins),
                    timeline_times: &timeline_times,
                    first_frame: frame,
                    live_end_frame: &live_end_frame,
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
        if phase_changed || last_status_at.elapsed() >= Duration::from_millis(33) {
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
            RenderPhase::Error => return,
            RenderPhase::Done => thread::sleep(Duration::from_millis(40)),
            RenderPhase::Paused => thread::sleep(Duration::from_millis(12)),
            RenderPhase::Transcoding => thread::sleep(Duration::from_millis(20)),
            RenderPhase::Rendering if job.settings.background && interactive => {
                thread::yield_now();
            }
            _ => {}
        }
    }
}

fn cache_chunk_seconds() -> f64 {
    DEFAULT_CACHE_CHUNK_SECONDS
}

fn external_tool(name: &str) -> PathBuf {
    let executable_name = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(directory) = current_exe.parent() {
            let bundled = directory.join(&executable_name);
            if bundled.is_file() {
                return bundled;
            }
        }
    }
    PathBuf::from(executable_name)
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
        frame_count >= 0 && frame_count as u64 == expected_frames,
        "completed cache chunk has {frame_count} frames, expected {expected_frames}"
    );
    Ok(())
}

fn render_export_batch_frames(canvas_size: [u32; 2]) -> usize {
    let pixels = u64::from(canvas_size[0].max(1)) * u64::from(canvas_size[1].max(1));

    if pixels >= 3_840u64 * 2_160 {
        3
    } else if pixels >= 2_560u64 * 1_440 {
        5
    } else {
        6
    }
}

fn render_encoder_threads(background: bool) -> usize {
    let cores = thread::available_parallelism().map_or(4, |value| value.get());
    if background {
        cores.saturating_sub(1).max(2)
    } else {
        cores.max(1)
    }
}

fn configure_prores_encoder(
    command: &mut Command,
    allow_hardware: bool,
    software_threads: Option<usize>,
) -> bool {
    if allow_hardware && prores_videotoolbox_usable() {
        command.args([
            "-c:v",
            "prores_videotoolbox",
            "-profile:v",
            "4444",
            "-allow_sw",
            "0",
            "-prio_speed",
            "1",
            "-pix_fmt",
            "ayuv64le",
        ]);
        true
    } else {
        command.args([
            "-c:v",
            "prores_ks",
            "-profile:v",
            "4444",
            "-alpha_bits",
            "16",
        ]);
        if let Some(threads) = software_threads {
            command
                .arg("-threads")
                .arg(threads.to_string())
                .args(["-thread_type", "slice+frame"]);
        }
        command.args(["-pix_fmt", "yuva444p10le"]);
        false
    }
}

fn prores_videotoolbox_usable() -> bool {
    static USABLE: OnceLock<bool> = OnceLock::new();

    *USABLE.get_or_init(|| {
        probe_prores_videotoolbox("color=c=black@0.0:s=16x16:r=1:d=1", "ayuv64le", "4444")
    })
}

fn probe_prores_videotoolbox(input: &str, pixel_format: &str, profile: &str) -> bool {
    if !cfg!(target_os = "macos") || !ffmpeg_encoder_available("prores_videotoolbox") {
        return false;
    }
    Command::new(external_tool("ffmpeg"))
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            input,
            "-frames:v",
            "1",
            "-vf",
            &format!("format={pixel_format}"),
            "-c:v",
            "prores_videotoolbox",
            "-profile:v",
            profile,
            "-allow_sw",
            "0",
            "-f",
            "null",
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn ffmpeg_encoder_available(name: &str) -> bool {
    static ENCODERS: OnceLock<HashSet<String>> = OnceLock::new();
    ENCODERS
        .get_or_init(|| {
            let Ok(output) = Command::new(external_tool("ffmpeg"))
                .args(["-hide_banner", "-encoders"])
                .output()
            else {
                return HashSet::new();
            };
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| {
                    let mut fields = line.split_whitespace();
                    let flags = fields.next()?;
                    let name = fields.next()?;
                    (flags.starts_with('V') || flags.starts_with('.')).then(|| name.to_string())
                })
                .collect()
        })
        .contains(name)
}

fn ffmpeg_filter_available(name: &str) -> bool {
    static FILTERS: OnceLock<HashSet<String>> = OnceLock::new();
    FILTERS
        .get_or_init(|| {
            let Ok(output) = Command::new(external_tool("ffmpeg"))
                .args(["-hide_banner", "-filters"])
                .output()
            else {
                return HashSet::new();
            };
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| {
                    let mut fields = line.split_whitespace();
                    let flags = fields.next()?;
                    let name = fields.next()?;
                    (flags.len() >= 3 && !name.starts_with('=')).then(|| name.to_string())
                })
                .collect()
        })
        .contains(name)
}

fn final_videotoolbox_pipeline_usable(codec: VideoCodec) -> bool {
    if !cfg!(all(target_os = "macos", target_arch = "aarch64"))
        || !ffmpeg_filter_available("scale_vt")
    {
        return false;
    }
    match codec {
        VideoCodec::H264 => ffmpeg_encoder_available("h264_videotoolbox"),
        VideoCodec::H265 => ffmpeg_encoder_available("hevc_videotoolbox"),
        _ => false,
    }
}

struct H26xEncoder {
    videotoolbox: &'static str,
    nvenc: &'static str,
    software: &'static str,
    videotoolbox_pix_fmt: &'static str,
    nvenc_pix_fmt: &'static str,
    software_pix_fmt: &'static str,
}

fn configure_h26x_encoder(
    command: &mut Command,
    encoder: H26xEncoder,
    quality: u8,
    hardware_frames: bool,
) {
    if cfg!(all(target_os = "macos", target_arch = "aarch64"))
        && ffmpeg_encoder_available(encoder.videotoolbox)
    {
        let vt_quality = (100i32 - i32::from(quality) * 2).clamp(1, 100);
        command
            .arg("-c:v")
            .arg(encoder.videotoolbox)
            .arg("-q:v")
            .arg(vt_quality.to_string())
            .arg("-prio_speed")
            .arg("1");
        if !hardware_frames {
            command.arg("-pix_fmt").arg(encoder.videotoolbox_pix_fmt);
        }
    } else if ffmpeg_encoder_available(encoder.nvenc) {
        command
            .arg("-c:v")
            .arg(encoder.nvenc)
            .arg("-preset")
            .arg("p4")
            .arg("-cq")
            .arg(quality.to_string())
            .arg("-pix_fmt")
            .arg(encoder.nvenc_pix_fmt);
    } else {
        command
            .arg("-c:v")
            .arg(encoder.software)
            .arg("-preset")
            .arg("veryfast")
            .arg("-threads")
            .arg(render_encoder_threads(false).to_string())
            .arg("-crf")
            .arg(quality.to_string())
            .arg("-pix_fmt")
            .arg(encoder.software_pix_fmt);
    }
}

fn configure_target_video_encoder(
    command: &mut Command,
    codec: VideoCodec,
    quality: u8,
    _needs_scale: bool,
    hardware_frames: bool,
) {
    let quality = quality.clamp(0, 51);
    match codec {
        VideoCodec::H264 => configure_h26x_encoder(
            command,
            H26xEncoder {
                videotoolbox: "h264_videotoolbox",
                nvenc: "h264_nvenc",
                software: "libx264",
                videotoolbox_pix_fmt: "yuv420p",
                nvenc_pix_fmt: "yuv420p",
                software_pix_fmt: "yuv420p",
            },
            quality,
            hardware_frames,
        ),
        VideoCodec::H265 => configure_h26x_encoder(
            command,
            H26xEncoder {
                videotoolbox: "hevc_videotoolbox",
                nvenc: "hevc_nvenc",
                software: "libx265",
                videotoolbox_pix_fmt: "p010le",
                nvenc_pix_fmt: "p010le",
                software_pix_fmt: "yuv420p10le",
            },
            quality,
            hardware_frames,
        ),
        VideoCodec::ProRes4444 => {
            configure_prores_encoder(command, true, None);
        }
        VideoCodec::Vp9 => {
            command
                .arg("-c:v")
                .arg("libvpx-vp9")
                .arg("-b:v")
                .arg("0")
                .arg("-crf")
                .arg(quality.to_string())
                .arg("-threads")
                .arg(render_encoder_threads(false).to_string())
                .args([
                    "-row-mt",
                    "1",
                    "-tile-columns",
                    "2",
                    "-frame-parallel",
                    "1",
                    "-cpu-used",
                    "4",
                    "-pix_fmt",
                    "yuva420p",
                ]);
        }
        VideoCodec::Gif => {
            command.arg("-c:v").arg("gif").arg("-pix_fmt").arg("pal8");
        }
        VideoCodec::Ffv1 => {
            command
                .arg("-c:v")
                .arg("ffv1")
                .arg("-level")
                .arg("3")
                .arg("-coder")
                .arg("rice")
                .arg("-context")
                .arg("0")
                .arg("-slicecrc")
                .arg("0")
                .arg("-threads")
                .arg(render_encoder_threads(false).to_string())
                .arg("-slices")
                .arg("16")
                .arg("-pix_fmt")
                .arg("gbrap16le");
        }
    }
}

fn render_content_signature(project: &Project, timeline: &TimelineDocument) -> u64 {
    let mut document = timeline.clone();

    document.view = Default::default();
    let mut snapshot = render_worker_project_snapshot(project, document);
    for composition in &mut snapshot.compositions {
        composition.timeline.view = Default::default();
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
        document.view = Default::default();
        document.end_time = None;
        document.end_behavior = Default::default();
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
    let end = (end_frame + 1) as f32 / fps as f32;
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

    let mut media_ids = HashSet::new();
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
            .filter(|time| **time >= clip.start as f64 && **time < clip.end() as f64)
            .copied()
            .collect::<Vec<_>>();
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
        let source_state = row.map(|row| &row.source).unwrap_or(&clip.source);
        hash_visual_source(project, source_state, &source_times, &mut h, 0);
        hash_pipeline_instance(
            project,
            row.map(|row| &row.pipeline).unwrap_or(&clip.pipeline),
            &active_times,
            &mut h,
        );
        hash_composite(
            row.map(|row| &row.composite).unwrap_or(&clip.composite),
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
            hash_composition_source(project, *id, times, h, depth + 1);
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
    let mut media_ids = HashSet::new();
    for track in visual_tracks {
        let active_clips = timeline
            .clips
            .iter()
            .filter(|clip| {
                clip.track == track.id
                    && times
                        .iter()
                        .any(|time| *time >= clip.start as f64 && *time < clip.end() as f64)
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
                .filter(|time| **time >= clip.start as f64 && **time < clip.end() as f64)
                .copied()
                .collect::<Vec<_>>();
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
            let source_state = row.map(|row| &row.source).unwrap_or(&clip.source);
            hash_visual_source(project, source_state, &source_times, h, depth);
            hash_pipeline_instance(
                project,
                row.map(|row| &row.pipeline).unwrap_or(&clip.pipeline),
                &active_times,
                h,
            );
            hash_composite(
                row.map(|row| &row.composite).unwrap_or(&clip.composite),
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

    if let Some(id) = instance.pipeline {
        if let Some(pipeline) = project.pipeline(id) {
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

pub(super) struct RenderSession {
    task: Option<RenderTask>,
    cache_dirs: Vec<PathBuf>,
    timeline_status_version: u64,
}

impl Default for RenderSession {
    fn default() -> Self {
        Self {
            task: None,
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
        for path in self.cache_dirs.drain(..) {
            let _ = fs::remove_dir_all(path);
        }
    }
}

impl RenderSession {
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
        if let Some(mut task) = self.task.take() {
            task.cancel();
            drop(task);
        }
        // Keep committed cache files for the lifetime of the application. Cancelling a
        // render stops work; it must not destroy frames which are still reusable.
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
            task.restart(settings);
            self.timeline_status_version = u64::MAX;
            return Ok(());
        }

        let task = RenderTask::spawn(settings, renderer, project, timeline, plugins, interaction)?;
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
        match RenderTask::spawn(
            settings,
            renderer,
            &target_project,
            &target_timeline,
            plugins,
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
        let frame = (timeline_time.max(0.0) as f64 * fps.max(1.0)).floor() as u64;
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
            local_time: (frame - range.source_start_frame) as f64 / fps.max(1.0),
            generation: range.generation,
            frame,
        })
    }
}

use std::{
    collections::{hash_map::DefaultHasher, HashSet},
    fs::{self, File},
    hash::{Hash, Hasher},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender, SyncSender, TrySendError},
        Arc, OnceLock,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Error, Result};
use kama_ui::Renderer;
use serde::Serialize;

use super::{
    cache_encoder::CacheEncoder, AudioCodec, RenderSpec, VideoCodec, DEFAULT_CACHE_CHUNK_SECONDS,
};
use crate::{
    audio::render_audio_wav,
    effects::{Binding, EffectRuntime, PipelineInstance},
    file_io::replace_file,
    playback::{ExportPixelFormat, ExportYuvBatchArgs, FrameRenderer, RenderCachePreview},
    plugin::PluginRegistry,
    project::{CompositionId, GeneratorSource, HostBinding, LayerComposite, Project, VisualSource},
    runtime::media::prioritize_offline_export,
    timeline::{RenderCacheRange, RenderCacheState, TimelineDocument, TimelineState, TrackKind},
};
