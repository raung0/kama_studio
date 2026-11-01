use std::{
    collections::{HashMap, VecDeque},
    hash::{Hash, Hasher},
    io::{BufRead, BufReader, BufWriter, Read, Write},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{
    effects::EffectRuntime,
    messages,
    playback::{FrameRenderer, PreviewOutput, RenderCachePreview, SourceGeometry},
    plugin::PluginRegistry,
    project::{Project, WorkerProjectState},
    runtime::{shared_surface, video::PresentationTexture},
    timeline::{TimelineDocument, TimelineState},
};

const PROTOCOL: u16 = 1;
const MAX_LINE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Snapshot {
    project: Project,
    worker_state: WorkerProjectState,
    timeline: TimelineDocument,
    revision: u64,
    seek_revision: u64,
    playhead: f32,
    playing: bool,
    scrubbing: bool,
    preview_size: [u32; 2],
    render_scale: f32,
    render_cache: Option<RenderCachePreview>,
    captured_frame: Option<([u32; 2], Vec<u8>)>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Request {
    Hello {
        protocol: u16,
        parent_pid: i32,
        metal_registry_id: u64,
        plugin_paths: String,
    },
    Render {
        generation: u64,
        snapshot: Snapshot,
    },
    Release {
        channel: String,
    },
    Invalidate,
    Shutdown,
    #[cfg(debug_assertions)]
    Crash,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireResponse {
    Ready {
        protocol: u16,
        pid: u32,
    },
    Frame {
        generation: u64,
        revision: u64,
        seek_revision: u64,
        playhead: f32,
        pending: bool,
        descriptor: shared_surface::SharedSurfaceDescriptor,
        source_geometry: HashMap<String, SourceGeometry>,
    },
    Error {
        message: String,
    },
    Diagnostic {
        generation: u64,
        error: bool,
        source: String,
        message: String,
    },
}

enum Response {
    Ready {
        pid: u32,
    },
    Frame {
        generation: u64,
        revision: u64,
        seek_revision: u64,
        playhead: f32,
        pending: bool,
        channel: String,
        texture: wgpu::Texture,
        source_geometry: HashMap<u32, SourceGeometry>,
    },
    Error(String),
    Diagnostic {
        generation: u64,
        error: bool,
        source: String,
        message: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoWorkerStatus {
    Starting,
    Ready { pid: u32 },
    Crashed,
    Unresponsive,
    Restarting,
    StartupFailed,
    Unsupported,
}

struct Process {
    child: Option<Child>,
    requests: mpsc::SyncSender<Request>,
    responses: mpsc::Receiver<Response>,
    pid: u32,
    reader: Option<thread::JoinHandle<()>>,
    writer: Option<thread::JoinHandle<()>>,
    metrics: Arc<Mutex<Option<(f32, u64)>>>,
    diagnostics: Arc<Mutex<VecDeque<String>>>,
    diagnostics_reader: Option<thread::JoinHandle<()>>,
    releases: Arc<Mutex<VecDeque<String>>>,
}

struct ChildGuard(Option<Child>);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
}

impl Process {
    fn spawn(
        plugin_paths: &str,
        parent_pid: i32,
        metal_registry_id: u64,
        device: Arc<wgpu::Device>,
    ) -> Result<Self> {
        let executable = std::env::current_exe().context("locate video worker executable")?;
        let mut command = Command::new(executable);
        #[cfg(not(all(test, debug_assertions)))]
        command.arg("--kama-worker=video");
        #[cfg(all(test, debug_assertions))]
        command
            .arg("--exact")
            .arg("video_worker::tests::video_worker_child_entry")
            .arg("--quiet")
            .arg("--nocapture")
            .env("KAMA_VIDEO_WORKER_TEST_CHILD", "1");
        let child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("spawn video worker")?;
        let mut child_guard = ChildGuard(Some(child));
        let child = child_guard
            .0
            .as_mut()
            .context("video worker process unavailable")?;
        let pid = child.id();
        let stdin = child
            .stdin
            .take()
            .context("video worker stdin unavailable")?;
        let stdout = child
            .stdout
            .take()
            .context("video worker stdout unavailable")?;
        let stderr = child
            .stderr
            .take()
            .context("video worker stderr unavailable")?;
        let (request_tx, request_rx) = mpsc::sync_channel(8);
        let (wire_tx, wire_rx) = mpsc::sync_channel(4);
        let (response_tx, response_rx) = mpsc::sync_channel(66);
        let metrics = Arc::new(Mutex::new(None));
        let diagnostics = Arc::new(Mutex::new(VecDeque::new()));
        let releases = Arc::new(Mutex::new(VecDeque::with_capacity(3)));
        let diagnostics_output = Arc::clone(&diagnostics);
        let diagnostics_reader = thread::Builder::new()
            .name(format!("kama-video-stderr-{pid}"))
            .spawn(move || drain_diagnostics(stderr, diagnostics_output))?;
        let metrics_output = Arc::downgrade(&metrics);
        thread::Builder::new()
            .name(format!("kama-video-metrics-{pid}"))
            .spawn(move || sample_process_metrics(pid, metrics_output))?;
        let writer_diagnostics = Arc::clone(&diagnostics);
        let writer = thread::Builder::new()
            .name(format!("kama-video-stdin-{pid}"))
            .spawn(move || {
                if let Err(error) = write_requests(stdin, request_rx)
                    && let Ok(mut diagnostics) = writer_diagnostics.lock()
                {
                    diagnostics.push_back(format!("parent request writer: {error:#}"));
                    while diagnostics.len() > 32 {
                        diagnostics.pop_front();
                    }
                }
            })?;
        let reader = thread::Builder::new()
            .name(format!("kama-video-stdout-{pid}"))
            .spawn(move || read_responses(stdout, wire_tx, cfg!(all(test, debug_assertions))))?;
        let importer = Arc::clone(&device);
        thread::Builder::new()
            .name(format!("kama-video-import-{pid}"))
            .spawn(move || import_responses(wire_rx, response_tx, importer, pid))?;
        request_tx
            .try_send(Request::Hello {
                protocol: PROTOCOL,
                parent_pid,
                metal_registry_id,
                plugin_paths: plugin_paths.to_owned(),
            })
            .map_err(|error| anyhow::anyhow!("send video worker handshake: {error}"))?;
        Ok(Self {
            child: child_guard.0.take(),
            requests: request_tx,
            responses: response_rx,
            pid,
            reader: Some(reader),
            writer: Some(writer),
            metrics,
            diagnostics,
            diagnostics_reader: Some(diagnostics_reader),
            releases,
        })
    }

    fn stop(&mut self) {
        let _ = self.requests.try_send(Request::Shutdown);
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            thread::spawn(move || {
                let _ = child.wait();
            });
        }
        self.reader.take();
        self.writer.take();
        self.diagnostics_reader.take();
    }
}

fn drain_diagnostics(stderr: impl std::io::Read, output: Arc<Mutex<VecDeque<String>>>) {
    let mut reader = BufReader::new(stderr);
    loop {
        let mut bytes = Vec::with_capacity(256);
        let mut byte = [0_u8; 1];
        let mut truncated = false;
        loop {
            match reader.read(&mut byte) {
                Ok(0) if bytes.is_empty() => return,
                Ok(0) => break,
                Ok(_) if byte[0] == b'\n' => break,
                Err(_) => return,
                Ok(_) if bytes.len() < 1024 => bytes.push(byte[0]),
                Ok(_) => truncated = true,
            }
        }
        let mut line = String::from_utf8_lossy(&bytes).into_owned();
        if truncated {
            line.push_str(" [truncated]");
        }
        if let Ok(mut output) = output.lock() {
            output.push_back(line);
            while output.len() > 32 {
                output.pop_front();
            }
        }
    }
}

fn sample_process_metrics(pid: u32, output: std::sync::Weak<Mutex<Option<(f32, u64)>>>) {
    #[cfg(unix)]
    loop {
        let Some(output) = output.upgrade() else {
            break;
        };
        let result = Command::new("ps")
            .env("LC_ALL", "C")
            .args(["-o", "%cpu=", "-o", "rss=", "-p", &pid.to_string()])
            .output();
        let Ok(result) = result else { break };
        if !result.status.success() {
            break;
        }
        let output_text = String::from_utf8_lossy(&result.stdout);
        let mut fields = output_text.split_whitespace().map(str::to_owned);
        if let (Some(cpu), Some(rss)) = (fields.next(), fields.next())
            && let (Ok(cpu), Ok(rss)) = (cpu.parse::<f32>(), rss.parse::<u64>())
            && let Ok(mut metrics) = output.lock()
        {
            *metrics = Some((cpu, rss.saturating_mul(1024)));
        }
        drop(output);
        thread::sleep(Duration::from_secs(1));
    }
    #[cfg(not(unix))]
    let _ = (pid, output);
}

impl Drop for Process {
    fn drop(&mut self) {
        self.stop();
    }
}

fn write_requests(stdin: impl Write, requests: mpsc::Receiver<Request>) -> Result<()> {
    let mut writer = BufWriter::new(stdin);
    for request in requests {
        serde_json::to_writer(&mut writer, &request).context("serialize video worker request")?;
        writer
            .write_all(b"\n")
            .context("write video worker request delimiter")?;
        writer.flush().context("flush video worker request")?;
        if matches!(request, Request::Shutdown) {
            break;
        }
    }
    Ok(())
}

fn queue_release(releases: Arc<Mutex<VecDeque<String>>>, channel: String) {
    if let Ok(mut releases) = releases.lock()
        && !releases.contains(&channel)
        && releases.len() < 3
    {
        releases.push_back(channel);
    }
}

fn frame_matches_current_request(
    frame_generation: u64,
    frame_revision: u64,
    frame_seek_revision: u64,
    playhead: f32,
    generation: u64,
    key: Option<(u64, u64, i64, [u32; 2])>,
    scrubbing: bool,
) -> bool {
    frame_generation == generation
        && key.is_some_and(|key| {
            key.0 == frame_revision
                && if scrubbing {
                    frame_seek_revision <= key.1
                } else {
                    key.1 == frame_seek_revision
                }
        })
        && playhead.is_finite()
}

fn processing_identity(
    key: (u64, u64, i64, [u32; 2]),
    scrubbing: bool,
) -> (u64, Option<u64>, [u32; 2]) {
    (key.0, (!scrubbing).then_some(key.1), key.3)
}

fn advance_processing_generation(
    previous: Option<(u64, u64, i64, [u32; 2])>,
    desired: (u64, u64, i64, [u32; 2]),
    generation: u64,
    scrubbing: bool,
) -> u64 {
    if previous.is_none_or(|previous| {
        processing_identity(previous, scrubbing) != processing_identity(desired, scrubbing)
    }) {
        generation.wrapping_add(1)
    } else {
        generation
    }
}

fn read_line(reader: &mut impl BufRead) -> std::io::Result<Option<String>> {
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
            };
        }
        let segment_len = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if bytes.len().saturating_add(segment_len) > MAX_LINE_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "video worker response exceeded protocol limit",
            ));
        }
        let has_newline = available.get(segment_len - 1) == Some(&b'\n');
        bytes.extend_from_slice(&available[..segment_len]);
        reader.consume(segment_len);
        if has_newline {
            return Ok(Some(String::from_utf8_lossy(&bytes).into_owned()));
        }
    }
}

fn read_responses(
    stdout: impl std::io::Read,
    responses: mpsc::SyncSender<WireResponse>,
    allow_test_harness_output: bool,
) {
    let mut reader = BufReader::new(stdout);
    loop {
        let line = match read_line(&mut reader) {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(error) => {
                let _ = responses.send(WireResponse::Error {
                    message: format!("read video worker protocol: {error}"),
                });
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str(&line) {
            Ok(response) => {
                if responses.send(response).is_err() {
                    break;
                }
            }
            Err(error) => {
                if allow_test_harness_output {
                    continue;
                }
                let excerpt = line.chars().take(512).collect::<String>();
                let _ = responses.send(WireResponse::Error {
                    message: format!("decode video worker response: {error}; prefix={excerpt}"),
                });
                break;
            }
        }
    }
}

fn import_responses(
    responses: mpsc::Receiver<WireResponse>,
    output: mpsc::SyncSender<Response>,
    device: Arc<wgpu::Device>,
    pid: u32,
) {
    let mut imported = HashMap::<String, wgpu::Texture>::new();
    while let Ok(response) = responses.recv() {
        let response = match response {
            WireResponse::Ready { protocol, pid } if protocol == PROTOCOL => {
                Response::Ready { pid }
            }
            WireResponse::Frame {
                generation,
                revision,
                seek_revision,
                playhead,
                pending,
                descriptor,
                source_geometry,
            } => {
                let source_geometry = source_geometry
                    .into_iter()
                    .filter_map(|(id, geometry)| id.parse().ok().map(|id| (id, geometry)))
                    .collect();
                let channel = descriptor.channel.clone();
                let texture = if let Some(texture) = imported.get(&channel) {
                    Ok(texture.clone())
                } else {
                    shared_surface::ImportedSurface::receive(
                        &device,
                        &descriptor,
                        i32::try_from(pid).unwrap_or(i32::MAX),
                        Duration::from_secs(2),
                    )
                    .map(|surface| surface.into_texture())
                };
                match texture {
                    Ok(texture) => {
                        imported.insert(channel.clone(), texture.clone());
                        while imported.len() > 3 {
                            let evict = imported
                                .keys()
                                .find(|candidate| **candidate != channel)
                                .cloned();
                            if let Some(evict) = evict {
                                imported.remove(&evict);
                            } else {
                                break;
                            }
                        }
                        Response::Frame {
                            generation,
                            revision,
                            seek_revision,
                            playhead,
                            pending,
                            channel,
                            texture,
                            source_geometry,
                        }
                    }
                    Err(error) => Response::Error(format!("import video frame: {error:#}")),
                }
            }
            WireResponse::Error { message } => Response::Error(message),
            WireResponse::Diagnostic {
                generation,
                error,
                source,
                message,
            } => Response::Diagnostic {
                generation,
                error,
                source,
                message,
            },
            WireResponse::Ready { .. } => Response::Error("video worker protocol mismatch".into()),
        };
        if output.send(response).is_err() {
            break;
        }
    }
}

pub struct VideoPlayback {
    process: Option<Process>,
    last_pid: Option<u32>,
    plugin_paths: String,
    status: VideoWorkerStatus,
    registry_id: u64,
    generation: u64,
    key: Option<(u64, u64, i64, [u32; 2])>,
    pending: Option<(u64, Snapshot)>,
    retry_pending: bool,
    request_pending: bool,
    texture_ids: HashMap<usize, kama_ui::ExternalTextureId>,
    active_renderer_key: Option<usize>,
    texture: Option<wgpu::Texture>,
    channel: Option<String>,
    source_geometry: HashMap<u32, SourceGeometry>,
    startup_deadline: Instant,
    request_started: Option<Instant>,
    live_edit_revision: u64,
    last_live_snapshot: Option<Instant>,
    waiting_for_video: bool,
    scrubbing: bool,
    invalidate_pending: bool,
    deferred_releases: VecDeque<String>,
    #[cfg(debug_assertions)]
    crash_requested: bool,
}

impl VideoPlayback {
    pub fn new(plugin_paths: &str, device: Arc<wgpu::Device>) -> Self {
        let registry_id = match shared_surface::metal_registry_id(&device) {
            Ok(id) => id,
            Err(error) => {
                messages::error(
                    "Video worker",
                    format!("unsupported GPU isolation: {error:#}"),
                );
                return Self::unavailable(plugin_paths, VideoWorkerStatus::Unsupported);
            }
        };
        let process = Self::spawn(plugin_paths, registry_id, device);
        let status = if process.is_ok() {
            VideoWorkerStatus::Starting
        } else {
            VideoWorkerStatus::StartupFailed
        };
        if let Err(error) = &process {
            messages::error("Video worker", format!("startup failed: {error:#}"));
        }
        let last_pid = process.as_ref().ok().map(|process| process.pid);
        Self {
            process: process.ok(),
            last_pid,
            plugin_paths: plugin_paths.to_owned(),
            status,
            registry_id,
            generation: 0,
            key: None,
            pending: None,
            retry_pending: false,
            request_pending: false,
            texture_ids: HashMap::new(),
            active_renderer_key: None,
            texture: None,
            channel: None,
            source_geometry: HashMap::new(),
            startup_deadline: Instant::now() + Duration::from_secs(15),
            request_started: None,
            live_edit_revision: 0,
            last_live_snapshot: None,
            waiting_for_video: false,
            scrubbing: false,
            invalidate_pending: false,
            deferred_releases: VecDeque::new(),
            #[cfg(debug_assertions)]
            crash_requested: false,
        }
    }

    fn unavailable(plugin_paths: &str, status: VideoWorkerStatus) -> Self {
        Self {
            process: None,
            last_pid: None,
            plugin_paths: plugin_paths.to_owned(),
            status,
            registry_id: 0,
            generation: 0,
            key: None,
            pending: None,
            retry_pending: false,
            request_pending: false,
            texture_ids: HashMap::new(),
            active_renderer_key: None,
            texture: None,
            channel: None,
            source_geometry: HashMap::new(),
            startup_deadline: Instant::now(),
            request_started: None,
            live_edit_revision: 0,
            last_live_snapshot: None,
            waiting_for_video: false,
            scrubbing: false,
            invalidate_pending: false,
            deferred_releases: VecDeque::new(),
            #[cfg(debug_assertions)]
            crash_requested: false,
        }
    }

    fn spawn(plugin_paths: &str, registry_id: u64, device: Arc<wgpu::Device>) -> Result<Process> {
        Process::spawn(
            plugin_paths,
            i32::try_from(std::process::id()).unwrap_or(i32::MAX),
            registry_id,
            device,
        )
    }

    pub fn status(&self) -> VideoWorkerStatus {
        self.status
    }

    pub const fn pid(&self) -> Option<u32> {
        self.last_pid
    }

    pub fn is_waiting_for_video(&self) -> bool {
        self.process.is_some()
            && (self.waiting_for_video
                || self.request_pending
                || self.pending.is_some()
                || matches!(
                    self.status,
                    VideoWorkerStatus::Starting | VideoWorkerStatus::Restarting
                ))
    }

    pub fn invalidate(&mut self) {
        if self.scrubbing {
            return;
        }
        self.key = None;
        self.generation = self.generation.wrapping_add(1);
        self.waiting_for_video = false;
        self.retry_pending = false;
        self.invalidate_pending = true;
    }

    pub fn resource_metrics(&self) -> Option<(f32, u64)> {
        self.process
            .as_ref()
            .and_then(|process| process.metrics.lock().ok().and_then(|metrics| *metrics))
    }

    #[cfg(debug_assertions)]
    pub fn crash_for_test(&mut self) {
        self.crash_requested = matches!(self.status, VideoWorkerStatus::Ready { .. });
    }

    pub fn restart(&mut self, device: Arc<wgpu::Device>) {
        if !matches!(
            self.status,
            VideoWorkerStatus::Crashed
                | VideoWorkerStatus::Unresponsive
                | VideoWorkerStatus::StartupFailed
                | VideoWorkerStatus::Unsupported
        ) {
            return;
        }
        #[cfg(debug_assertions)]
        {
            self.crash_requested = false;
        }
        self.stop();
        self.generation = self.generation.wrapping_add(1);
        self.key = None;
        self.pending = None;
        self.retry_pending = false;
        self.request_pending = false;
        self.invalidate_pending = false;
        self.deferred_releases.clear();
        #[cfg(not(target_os = "macos"))]
        {
            self.status = VideoWorkerStatus::Unsupported;
            return;
        }
        #[cfg(target_os = "macos")]
        match shared_surface::metal_registry_id(&device) {
            Ok(registry_id) => self.registry_id = registry_id,
            Err(error) => {
                self.status = VideoWorkerStatus::Unsupported;
                messages::error(
                    "Video worker",
                    format!("GPU isolation is unavailable after window switch: {error:#}"),
                );
                return;
            }
        }
        match Self::spawn(&self.plugin_paths, self.registry_id, device) {
            Ok(process) => {
                self.last_pid = Some(process.pid);
                self.process = Some(process);
                self.status = VideoWorkerStatus::Restarting;
                self.startup_deadline = Instant::now() + Duration::from_secs(15);
            }
            Err(error) => {
                self.status = VideoWorkerStatus::StartupFailed;
                messages::error("Video worker", format!("restart failed: {error:#}"));
            }
        }
    }

    fn stop(&mut self) {
        if let Some(mut process) = self.process.take() {
            process.stop();
        }
        self.texture = None;
        self.channel = None;
        self.pending = None;
        self.retry_pending = false;
        self.active_renderer_key = None;
        self.source_geometry.clear();
    }

    fn queue_snapshot(&mut self, key: (u64, u64, i64, [u32; 2]), snapshot: Snapshot) {
        self.pending = Some((self.generation, snapshot));
        self.key = Some(key);
        self.retry_pending = false;
    }

    fn should_queue_snapshot(&self, key: (u64, u64, i64, [u32; 2])) -> bool {
        self.key != Some(key) || self.retry_pending
    }

    pub fn sync(
        &mut self,
        renderer: &mut kama_ui::Renderer,
        project: &Project,
        timeline: &TimelineState,
        revision: u64,
        preview_size: [u32; 2],
        render_scale: f32,
        render_cache: Option<&RenderCachePreview>,
        captured_frame: Option<([u32; 2], &[u8])>,
        live_editing: bool,
    ) -> Result<bool> {
        let previous_status = self.status;
        if live_editing
            && self
                .last_live_snapshot
                .is_none_or(|last| last.elapsed() >= Duration::from_millis(50))
        {
            self.live_edit_revision = self.live_edit_revision.wrapping_add(1);
            self.last_live_snapshot = Some(Instant::now());
        } else if !live_editing {
            self.last_live_snapshot = None;
        }
        let frame =
            (f64::from(timeline.playhead()) * project.active_settings().frame_rate.max(1.0)) as i64;
        let mut content_hasher = std::collections::hash_map::DefaultHasher::new();
        revision.hash(&mut content_hasher);
        self.live_edit_revision.hash(&mut content_hasher);
        timeline.is_playing().hash(&mut content_hasher);
        timeline.is_scrubbing().hash(&mut content_hasher);
        render_scale.to_bits().hash(&mut content_hasher);
        if let Some(cache) = render_cache {
            cache.path.hash(&mut content_hasher);
            cache.generation.hash(&mut content_hasher);
        }
        if let Some((size, pixels)) = captured_frame {
            size.hash(&mut content_hasher);
            pixels.hash(&mut content_hasher);
        }
        let content_revision = content_hasher.finish();
        let key = (
            content_revision,
            timeline.seek_revision(),
            frame,
            preview_size,
        );
        self.generation =
            advance_processing_generation(self.key, key, self.generation, timeline.is_scrubbing());
        self.scrubbing = timeline.is_scrubbing();
        if self.should_queue_snapshot(key) {
            let mut snapshot_project = project.clone();
            snapshot_project.sync_active_timeline(timeline.document());
            let snapshot = Snapshot {
                worker_state: WorkerProjectState::capture(&snapshot_project),
                project: snapshot_project,
                timeline: timeline.document(),
                revision: content_revision,
                seek_revision: timeline.seek_revision(),
                playhead: timeline.playhead(),
                playing: timeline.is_playing(),
                scrubbing: timeline.is_scrubbing(),
                preview_size,
                render_scale,
                render_cache: render_cache.cloned(),
                captured_frame: captured_frame.map(|(size, pixels)| (size, pixels.to_vec())),
            };
            self.queue_snapshot(key, snapshot);
        }
        self.poll(renderer);
        self.present_current(renderer)?;
        let Some(process) = self.process.as_mut() else {
            return Ok(matches!(
                previous_status,
                VideoWorkerStatus::Ready { .. }
                    | VideoWorkerStatus::Starting
                    | VideoWorkerStatus::Restarting
            ) && matches!(
                self.status,
                VideoWorkerStatus::Crashed | VideoWorkerStatus::Unresponsive
            ));
        };
        let mut release_disconnected = false;
        if let Ok(mut releases) = process.releases.lock() {
            while let Some(channel) = releases.front().cloned() {
                match process.requests.try_send(Request::Release { channel }) {
                    Ok(()) => {
                        releases.pop_front();
                    }
                    Err(mpsc::TrySendError::Full(_)) => break,
                    Err(mpsc::TrySendError::Disconnected(_)) => {
                        release_disconnected = true;
                        break;
                    }
                }
            }
        }
        if release_disconnected {
            self.fail(VideoWorkerStatus::Crashed);
            return Ok(true);
        }
        #[cfg(debug_assertions)]
        if self.crash_requested && process.requests.try_send(Request::Crash).is_ok() {
            self.crash_requested = false;
        }
        if self.invalidate_pending {
            match process.requests.try_send(Request::Invalidate) {
                Ok(()) => self.invalidate_pending = false,
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    self.fail(VideoWorkerStatus::Crashed);
                    return Ok(true);
                }
                Err(mpsc::TrySendError::Full(_)) => return Ok(false),
            }
        }
        if !self.request_pending
            && let Some((generation, snapshot)) = self.pending.take()
        {
            match process.requests.try_send(Request::Render {
                generation,
                snapshot,
            }) {
                Ok(()) => {
                    self.request_pending = true;
                    self.request_started = Some(Instant::now());
                    self.retry_pending = false;
                }
                Err(mpsc::TrySendError::Full(Request::Render {
                    generation,
                    snapshot,
                })) => {
                    self.pending = Some((generation, snapshot));
                }
                Err(mpsc::TrySendError::Disconnected(_)) => self.fail(VideoWorkerStatus::Crashed),
                Err(mpsc::TrySendError::Full(_)) => {}
            }
        }
        Ok(matches!(
            previous_status,
            VideoWorkerStatus::Ready { .. }
                | VideoWorkerStatus::Starting
                | VideoWorkerStatus::Restarting
        ) && matches!(
            self.status,
            VideoWorkerStatus::Crashed | VideoWorkerStatus::Unresponsive
        ))
    }

    fn poll(&mut self, renderer: &mut kama_ui::Renderer) {
        let Some(process) = self.process.as_ref() else {
            return;
        };
        while let Ok(response) = process.responses.try_recv() {
            match response {
                Response::Ready { pid } => {
                    if self
                        .process
                        .as_ref()
                        .is_some_and(|process| process.pid == pid)
                    {
                        self.status = VideoWorkerStatus::Ready { pid };
                    } else {
                        self.fail(VideoWorkerStatus::Crashed);
                        return;
                    }
                }
                Response::Frame {
                    generation,
                    revision,
                    seek_revision,
                    playhead,
                    pending,
                    channel,
                    texture,
                    source_geometry,
                } if frame_matches_current_request(
                    generation,
                    revision,
                    seek_revision,
                    playhead,
                    self.generation,
                    self.key,
                    self.scrubbing,
                ) =>
                {
                    self.request_pending = false;
                    self.request_started = None;
                    let renderer_key = renderer.registration_id();
                    let view = texture.create_view(&Default::default());
                    let id = match self.texture_ids.get(&renderer_key).copied() {
                        Some(id) => renderer.replace_external_texture(id, view).map(|()| id),
                        None => renderer.register_external_texture(view),
                    };
                    match id {
                        Ok(id) => {
                            self.texture_ids.insert(renderer_key, id);
                            self.active_renderer_key = Some(renderer_key);
                            let old_channel = self.channel.replace(channel);
                            let old_texture = self.texture.replace(texture);
                            self.source_geometry = source_geometry;
                            self.waiting_for_video = pending;
                            self.retry_pending = pending;
                            self.status = match self.status {
                                VideoWorkerStatus::Ready { pid } => {
                                    VideoWorkerStatus::Ready { pid }
                                }
                                other => other,
                            };
                            if let (Some(channel), Some(old_texture)) = (old_channel, old_texture) {
                                let releases = self
                                    .process
                                    .as_ref()
                                    .map(|process| Arc::clone(&process.releases));
                                renderer.queue().on_submitted_work_done(move || {
                                    drop(old_texture);
                                    if let Some(releases) = releases {
                                        queue_release(releases, channel);
                                    }
                                });
                            }
                        }
                        Err(error) => {
                            if let Some(process) = self.process.as_ref() {
                                queue_release(Arc::clone(&process.releases), channel);
                            }
                            messages::error(
                                "Video worker",
                                format!("register preview texture: {error:#}"),
                            );
                        }
                    }
                }
                Response::Frame { channel, .. } => {
                    self.request_pending = false;
                    self.request_started = None;
                    if let Some(process) = self.process.as_ref() {
                        queue_release(Arc::clone(&process.releases), channel);
                    }
                }
                Response::Error(message) => {
                    self.request_pending = false;
                    self.request_started = None;
                    if matches!(
                        message.as_str(),
                        "preview source is not ready" | "video frame slots are full"
                    ) {
                        self.waiting_for_video = true;
                        self.retry_pending = true;
                    } else {
                        messages::error("Video worker", message);
                    }
                }
                Response::Diagnostic {
                    generation,
                    error,
                    source,
                    message,
                } => {
                    let source = format!(
                        "Video worker {} gen {generation}: {source}",
                        self.process.as_ref().map_or(0, |p| p.pid)
                    );
                    if error {
                        messages::error(source, message);
                    } else {
                        messages::warning(source, message);
                    }
                }
            }
        }
        let exit_result = self
            .process
            .as_mut()
            .and_then(|process| process.child.as_mut().map(Child::try_wait));
        if let Some(Err(error)) = exit_result {
            self.fail_with_detail(
                VideoWorkerStatus::Crashed,
                format!("could not inspect child exit: {error}"),
            );
        } else if let Some(Ok(Some(status))) = exit_result {
            self.fail_with_detail(
                VideoWorkerStatus::Crashed,
                format!(
                    "process {} exited with {status}",
                    self.process.as_ref().map_or(0, |process| process.pid)
                ),
            );
        } else if matches!(
            self.status,
            VideoWorkerStatus::Starting | VideoWorkerStatus::Restarting
        ) && Instant::now() >= self.startup_deadline
        {
            self.fail(VideoWorkerStatus::Unresponsive);
        } else if self
            .request_started
            .is_some_and(|started| started.elapsed() >= Duration::from_secs(15))
        {
            self.fail(VideoWorkerStatus::Unresponsive);
        }
    }

    fn fail(&mut self, status: VideoWorkerStatus) {
        self.fail_with_detail(status, String::new());
    }

    fn fail_with_detail(&mut self, status: VideoWorkerStatus, detail: String) {
        if !matches!(
            self.status,
            VideoWorkerStatus::Crashed
                | VideoWorkerStatus::Unresponsive
                | VideoWorkerStatus::StartupFailed
        ) {
            let diagnostic = self
                .process
                .as_ref()
                .and_then(|process| process.diagnostics.lock().ok())
                .and_then(|lines| lines.back().cloned());
            let diagnostic = diagnostic.map_or_else(String::new, |line| format!("; {line}"));
            messages::error(
                "Video worker",
                format!(
                    "worker {} stopped: {status:?}{detail}{diagnostic}",
                    self.process.as_ref().map_or(0, |process| process.pid)
                ),
            );
        }
        self.status = status;
        self.stop();
    }

    fn present_current(&mut self, renderer: &mut kama_ui::Renderer) -> Result<()> {
        let key = renderer.registration_id();
        if self.active_renderer_key == Some(key) || self.texture.is_none() {
            return Ok(());
        }
        if let Some(texture) = &self.texture {
            let view = texture.create_view(&Default::default());
            let id = if let Some(id) = self.texture_ids.get(&key).copied() {
                renderer.replace_external_texture(id, view)?;
                id
            } else {
                renderer.register_external_texture(view)?
            };
            self.texture_ids.insert(key, id);
            self.active_renderer_key = Some(key);
        }
        Ok(())
    }

    pub fn preview_output(&self) -> PreviewOutput<'_> {
        PreviewOutput {
            texture: self
                .active_renderer_key
                .and_then(|key| self.texture_ids.get(&key).copied()),
            source_geometry: &self.source_geometry,
        }
    }
}

pub(super) fn run_video_worker() -> Result<()> {
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let Some(line) = read_line(&mut reader)? else {
        anyhow::bail!("video worker handshake missing");
    };
    let Request::Hello {
        protocol,
        parent_pid,
        metal_registry_id,
        plugin_paths,
    } = serde_json::from_str(&line).context("decode video worker handshake")?
    else {
        anyhow::bail!("video worker handshake expected");
    };
    anyhow::ensure!(protocol == PROTOCOL, "unsupported video worker protocol");
    let (instance, adapter, device, queue) = create_matching_metal_device(metal_registry_id)?;
    let mut plugins = PluginRegistry::load_default(&plugin_paths)?;
    plugins.validate_gpu(&device);
    let mut effects = EffectRuntime::default();
    let mut renderer = FrameRenderer::new_export_worker(&device, &effects, &plugins);
    let mut effects_revision = None;
    write_response(&WireResponse::Ready {
        protocol: PROTOCOL,
        pid: std::process::id(),
    })?;
    let mut surfaces = HashMap::<String, shared_surface::SharedSurface>::new();
    let mut free_surfaces = VecDeque::<String>::new();
    while let Some(line) = read_line(&mut reader)? {
        let request: Request =
            serde_json::from_str(&line).context("decode video worker request")?;
        match request {
            Request::Render {
                generation,
                snapshot,
            } => {
                let mut project = snapshot.project;
                if let Err(error) = snapshot.worker_state.restore(&mut project) {
                    write_response(&WireResponse::Error {
                        message: format!("restore video worker project state: {error:#}"),
                    })?;
                    continue;
                }
                if effects_revision != Some(snapshot.revision) {
                    effects.rebuild(&project.pipelines);
                    renderer.sync_compiled_effects_on(&device, &effects, &plugins);
                    effects_revision = Some(snapshot.revision);
                }
                let mut timeline = TimelineState::default();
                timeline.load_document(snapshot.timeline.clone());
                timeline.set_worker_transport(snapshot.playhead, snapshot.playing);
                let [width, height] = snapshot.preview_size;
                let dimensions = [width.max(1), height.max(1)];
                while let Some(channel) = free_surfaces.pop_front() {
                    if surfaces.get(&channel).is_some_and(|surface| {
                        surface.descriptor().width != dimensions[0]
                            || surface.descriptor().height != dimensions[1]
                    }) {
                        surfaces.remove(&channel);
                    } else {
                        free_surfaces.push_front(channel);
                        break;
                    }
                }
                let reusable = free_surfaces.pop_front();
                let channel = if let Some(channel) = reusable {
                    channel
                } else if surfaces.len() < 2 {
                    let surface = match shared_surface::SharedSurface::allocate(
                        &device,
                        dimensions[0],
                        dimensions[1],
                        parent_pid,
                    ) {
                        Ok(surface) => surface,
                        Err(error) => {
                            write_response(&WireResponse::Error {
                                message: format!("allocate preview surface: {error:#}"),
                            })?;
                            continue;
                        }
                    };
                    let channel = surface.descriptor().channel.clone();
                    surfaces.insert(channel.clone(), surface);
                    channel
                } else {
                    write_response(&WireResponse::Error {
                        message: "video frame slots are full".into(),
                    })?;
                    continue;
                };
                let surface = surfaces
                    .get(&channel)
                    .context("reusable frame slot disappeared")?;
                let output = PresentationTexture::from_texture(
                    surface.texture().clone(),
                    width.max(1),
                    height.max(1),
                );
                let rendered = renderer.refresh_preview_on(
                    &device,
                    &queue,
                    &project,
                    &timeline,
                    &effects,
                    &plugins,
                    snapshot.render_cache.as_ref(),
                    [width.max(1), height.max(1)],
                    snapshot.render_scale,
                    snapshot.scrubbing,
                    snapshot
                        .captured_frame
                        .as_ref()
                        .map(|(size, pixels)| (*size, pixels.as_slice())),
                    &output,
                );
                let rendered = match rendered {
                    Ok(rendered) => rendered,
                    Err(error) => {
                        free_surfaces.push_back(channel.clone());
                        write_response(&WireResponse::Error {
                            message: format!("render preview: {error:#}"),
                        })?;
                        continue;
                    }
                };
                if !rendered {
                    free_surfaces.push_back(channel.clone());
                    write_response(&WireResponse::Error {
                        message: "preview source is not ready".into(),
                    })?;
                    continue;
                }
                device
                    .poll(wgpu::PollType::Wait {
                        submission_index: None,
                        timeout: Some(Duration::from_secs(5)),
                    })
                    .context("wait for child GPU render")?;
                let descriptor = surface.descriptor().clone();
                let source_geometry = renderer
                    .source_geometry()
                    .iter()
                    .map(|(id, geometry)| (id.to_string(), geometry.clone()))
                    .collect();
                for (error, source, message) in messages::drain_for_worker()
                    .into_iter()
                    .take(64)
                    .map(|(severity, source, message)| {
                        (
                            severity == messages::MessageSeverity::Error,
                            source,
                            message,
                        )
                    })
                {
                    write_response(&WireResponse::Diagnostic {
                        generation,
                        error,
                        source,
                        message,
                    })?;
                }
                write_response(&WireResponse::Frame {
                    generation,
                    revision: snapshot.revision,
                    seek_revision: snapshot.seek_revision,
                    playhead: snapshot.playhead,
                    pending: renderer.is_waiting_for_video(),
                    descriptor,
                    source_geometry,
                })?;
            }
            Request::Release { channel } => {
                if surfaces.contains_key(&channel) && !free_surfaces.contains(&channel) {
                    free_surfaces.push_back(channel);
                }
            }
            Request::Invalidate => {
                effects_revision = None;
                renderer.clear_caches();
            }
            Request::Shutdown => break,
            #[cfg(debug_assertions)]
            Request::Crash => std::process::abort(),
            Request::Hello { .. } => write_response(&WireResponse::Error {
                message: "duplicate video handshake".into(),
            })?,
        }
    }
    drop(surfaces);
    drop((adapter, instance));
    Ok(())
}

fn create_matching_metal_device(
    expected_registry_id: u64,
) -> Result<(wgpu::Instance, wgpu::Adapter, wgpu::Device, wgpu::Queue)> {
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = wgpu::Backends::METAL;
    let instance = wgpu::Instance::new(descriptor);
    for adapter in pollster::block_on(instance.enumerate_adapters(wgpu::Backends::METAL)) {
        let features = wgpu::Features::TEXTURE_FORMAT_16BIT_NORM
            | wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES;
        if !adapter.features().contains(features)
            || !adapter
                .get_texture_format_features(wgpu::TextureFormat::R16Uint)
                .allowed_usages
                .contains(wgpu::TextureUsages::STORAGE_BINDING)
        {
            continue;
        }
        let descriptor = wgpu::DeviceDescriptor {
            label: Some("kama isolated video device"),
            required_features: features,
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
        };
        let Ok((device, queue)) = pollster::block_on(adapter.request_device(&descriptor)) else {
            continue;
        };
        if shared_surface::metal_registry_id(&device).ok() == Some(expected_registry_id) {
            return Ok((instance, adapter, device, queue));
        }
    }
    anyhow::bail!("no Metal adapter matches the parent GPU registry ID")
}

fn write_response(response: &WireResponse) -> Result<()> {
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();
    serde_json::to_writer(&mut writer, response)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{SourceGeometry, WireResponse};

    #[test]
    fn frame_response_round_trips_numeric_source_ids() {
        let response = WireResponse::Frame {
            generation: 1,
            revision: 2,
            seek_revision: 3,
            playhead: 1.5,
            pending: true,
            descriptor: crate::runtime::shared_surface::SharedSurfaceDescriptor {
                channel: "test".into(),
                session_token: [0; 32],
                width: 2,
                height: 2,
                pixel_format: 0,
                metal_registry_id: 0,
                importer_pid: 1,
                exporter_pid: 2,
            },
            source_geometry: HashMap::from([("42".to_owned(), SourceGeometry::canvas(2, 2))]),
        };

        let encoded = serde_json::to_string(&response).unwrap();
        let decoded: WireResponse = serde_json::from_str(&encoded).unwrap();
        let WireResponse::Frame {
            pending,
            source_geometry,
            ..
        } = decoded
        else {
            panic!("frame response expected");
        };
        assert!(pending);
        assert!(source_geometry.contains_key("42"));
    }

    #[test]
    fn video_worker_child_entry() {
        if std::env::var_os("KAMA_VIDEO_WORKER_TEST_CHILD").is_some() {
            super::run_video_worker().expect("video worker child protocol exits cleanly");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn project_video_preview_reproduction() {
        use std::{
            path::Path,
            process::Command,
            sync::Arc,
            sync::mpsc,
            time::{Duration, Instant},
        };

        use super::{Process, Request, Response, Snapshot};
        use crate::{
            plugin::PluginRegistry, project::Project, runtime::shared_surface,
            timeline::TimelineState,
        };

        let Ok(project_path) = std::env::var("KAMA_VIDEO_REPRO_PROJECT") else {
            return;
        };
        let path = Path::new(&project_path);
        let mut project = Project::load(path).expect("load reproduction project");
        for asset in &project.media {
            assert!(
                !asset.path.as_os_str().is_empty(),
                "loaded media '{}' has an empty runtime path",
                asset.name
            );
            assert!(
                asset.path.is_file(),
                "loaded media '{}' is missing at {}",
                asset.name,
                asset.path.display()
            );
        }
        let plugin_paths = crate::preferences::load_plugin_paths();
        let _plugins = PluginRegistry::load_default(&plugin_paths)
            .expect("load plugins with application preference paths");
        let mut timeline = TimelineState::default();
        timeline.load_document(project.active_composition().timeline.clone());
        project.sync_active_timeline(timeline.document());
        eprintln!(
            "project={} media={:?} timeline_clips={:?} playhead={}",
            project.name,
            project
                .media
                .iter()
                .map(|asset| (&asset.path, asset.kind))
                .collect::<Vec<_>>(),
            timeline
                .clips()
                .iter()
                .map(|clip| (&clip.source, clip.start, clip.duration))
                .collect::<Vec<_>>(),
            timeline.playhead(),
        );

        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = wgpu::Backends::METAL;
        let instance = wgpu::Instance::new(descriptor);
        let adapter = pollster::block_on(instance.request_adapter(&Default::default()))
            .expect("Metal adapter available for project reproduction");
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("pumpkin preview verification"),
            required_features: wgpu::Features::TEXTURE_FORMAT_16BIT_NORM
                | wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES,
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
        }))
        .expect("request preview GPU device");
        let registry_id = shared_surface::metal_registry_id(&device).expect("Metal device ID");
        let process = Process::spawn(
            &plugin_paths,
            i32::try_from(std::process::id()).unwrap(),
            registry_id,
            Arc::new(device.clone()),
        )
        .expect("spawn production video worker");
        let startup_deadline = Instant::now() + Duration::from_secs(20);
        loop {
            match process.responses.recv_timeout(Duration::from_secs(1)) {
                Ok(Response::Ready { pid }) if pid == process.pid => break,
                Ok(Response::Ready { pid }) => panic!("worker reported unexpected pid {pid}"),
                Ok(Response::Diagnostic {
                    error,
                    source,
                    message,
                    ..
                }) => {
                    eprintln!("worker startup {source} error={error}: {message}");
                }
                Ok(Response::Error(error)) => panic!("worker startup failed: {error}"),
                Ok(Response::Frame { .. }) => panic!("worker rendered before project request"),
                Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() < startup_deadline => {}
                Err(error) => panic!("worker did not become ready: {error}"),
            }
        }

        let frame_size = [640, 360];
        let timeout = std::env::var("KAMA_VIDEO_REPRO_TIMEOUT_SECS")
            .ok()
            .and_then(|seconds| seconds.parse().ok())
            .map_or(Duration::from_secs(75), Duration::from_secs);
        let deadline = Instant::now() + timeout;
        let mut generation = 0_u64;
        let mut not_ready_count = 0_u64;
        let frame = 'render: loop {
            generation = generation.wrapping_add(1);
            let snapshot = Snapshot {
                worker_state: super::WorkerProjectState::capture(&project),
                project: project.clone(),
                timeline: timeline.document(),
                revision: 3,
                seek_revision: timeline.seek_revision(),
                playhead: timeline.playhead(),
                playing: timeline.is_playing(),
                scrubbing: timeline.is_scrubbing(),
                preview_size: frame_size,
                render_scale: 1.0,
                render_cache: None,
                captured_frame: None,
            };
            let mut request = Request::Render {
                generation,
                snapshot,
            };
            if generation == 1 {
                let encoded = serde_json::to_vec(&request).expect("serialize production request");
                request = serde_json::from_slice(&encoded).expect("decode production request");
                let Request::Render { snapshot, .. } = &request else {
                    unreachable!();
                };
                let mut restored = snapshot.project.clone();
                snapshot
                    .worker_state
                    .clone()
                    .restore(&mut restored)
                    .expect("restore worker runtime project state");
                assert_eq!(restored.active_composition, project.active_composition);
                for asset in &restored.media {
                    let source = project.media(asset.id).expect("source project media");
                    assert_eq!(asset.path, source.path, "worker path for {}", asset.name);
                    assert!(
                        asset.path.is_file(),
                        "worker media path {}",
                        asset.path.display()
                    );
                }
            }
            process
                .requests
                .send(request)
                .expect("send bounded project preview request");
            loop {
                match process.responses.recv_timeout(Duration::from_secs(2)) {
                    Ok(Response::Frame {
                        generation: response_generation,
                        channel,
                        texture,
                        ..
                    }) if response_generation == generation => break 'render (channel, texture),
                    Ok(Response::Frame {
                        channel, texture, ..
                    }) => {
                        drop(texture);
                        let _ = process.requests.try_send(Request::Release { channel });
                    }
                    Ok(Response::Error(error)) if error == "preview source is not ready" => {
                        not_ready_count += 1;
                        if not_ready_count == 1 || not_ready_count % 10 == 0 {
                            eprintln!("preview source not ready, retries={not_ready_count}");
                        }
                        break;
                    }
                    Ok(Response::Error(error)) => panic!("project preview error: {error}"),
                    Ok(Response::Diagnostic {
                        error,
                        source,
                        message,
                        ..
                    }) => {
                        eprintln!("worker {source} error={error}: {message}");
                    }
                    Ok(Response::Ready { .. }) => panic!("unexpected worker handshake response"),
                    Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() < deadline => continue,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        let sample_path = "/private/tmp/opencode/pumpkin-video-test-child.sample";
                        let parent_sample_path =
                            "/private/tmp/opencode/pumpkin-video-test-parent.sample";
                        let sample = Command::new("sample")
                            .args([&process.pid.to_string(), "1", "-file", sample_path])
                            .output()
                            .map(|_| format!("sample saved to {sample_path}"))
                            .unwrap_or_else(|error| format!("sample failed: {error}"));
                        let _ = Command::new("sample")
                            .args([
                                &std::process::id().to_string(),
                                "1",
                                "-file",
                                parent_sample_path,
                            ])
                            .output();
                        panic!(
                            "no preview frame in {:?}; worker pid={} diagnostics={:?}; parent sample={parent_sample_path}\n{sample}",
                            timeout,
                            process.pid,
                            process
                                .diagnostics
                                .lock()
                                .unwrap()
                                .iter()
                                .collect::<Vec<_>>()
                        );
                    }
                    Err(error) => panic!("worker response channel failed: {error}"),
                }
            }
            if Instant::now() >= deadline {
                let sample_path = "/private/tmp/opencode/pumpkin-video-test-child.sample";
                let parent_sample_path = "/private/tmp/opencode/pumpkin-video-test-parent.sample";
                let sample = Command::new("sample")
                    .args([&process.pid.to_string(), "1", "-file", sample_path])
                    .output()
                    .map(|_| format!("sample saved to {sample_path}"))
                    .unwrap_or_else(|error| format!("sample failed: {error}"));
                let _ = Command::new("sample")
                    .args([
                        &std::process::id().to_string(),
                        "1",
                        "-file",
                        parent_sample_path,
                    ])
                    .output();
                panic!(
                    "preview remained not-ready until deadline {:?}; worker pid={} diagnostics={:?}; parent sample={parent_sample_path}\n{sample}",
                    timeout,
                    process.pid,
                    process
                        .diagnostics
                        .lock()
                        .unwrap()
                        .iter()
                        .collect::<Vec<_>>()
                );
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let (initial_channel, texture) = frame;
        process
            .requests
            .try_send(Request::Release {
                channel: initial_channel,
            })
            .expect("release initial reproduction frame");
        let mut scrub_frames = 0;
        for seek_step in 1..=5 {
            timeline.seek_relative(0.25);
            generation = generation.wrapping_add(1);
            let snapshot = Snapshot {
                worker_state: super::WorkerProjectState::capture(&project),
                project: project.clone(),
                timeline: timeline.document(),
                revision: 3,
                seek_revision: timeline.seek_revision(),
                playhead: timeline.playhead(),
                playing: false,
                scrubbing: true,
                preview_size: frame_size,
                render_scale: 1.0,
                render_cache: None,
                captured_frame: None,
            };
            process
                .requests
                .send(Request::Render {
                    generation,
                    snapshot,
                })
                .expect("send active scrub snapshot");
            let expected_playhead = timeline.playhead();
            let expected_seek_revision = timeline.seek_revision();
            loop {
                match process.responses.recv_timeout(Duration::from_secs(5)) {
                    Ok(Response::Frame {
                        generation: response_generation,
                        seek_revision,
                        playhead,
                        channel,
                        texture,
                        ..
                    }) if response_generation == generation => {
                        assert_eq!(seek_revision, expected_seek_revision);
                        assert_eq!(playhead, expected_playhead);
                        scrub_frames += 1;
                        drop(texture);
                        process
                            .requests
                            .send(Request::Release { channel })
                            .expect("release scrub frame");
                        break;
                    }
                    Ok(Response::Frame {
                        channel, texture, ..
                    }) => {
                        drop(texture);
                        let _ = process.requests.try_send(Request::Release { channel });
                    }
                    Ok(Response::Error(error)) => panic!("scrub preview error: {error}"),
                    Ok(Response::Diagnostic { .. }) => {}
                    Ok(Response::Ready { .. }) => panic!("unexpected worker handshake response"),
                    Err(error) => {
                        panic!("worker did not render held-drag scrub {seek_step}: {error}")
                    }
                }
            }
        }
        assert_eq!(scrub_frames, 5, "scrub frames must arrive before release");
        let row_bytes = frame_size[0] * 8;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pumpkin rendered pixel verification"),
            size: u64::from(row_bytes) * u64::from(frame_size[1]),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row_bytes),
                    rows_per_image: Some(frame_size[1]),
                },
            },
            wgpu::Extent3d {
                width: frame_size[0],
                height: frame_size[1],
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);
        let slice = buffer.slice(..);
        let (sender, receiver) = mpsc::sync_channel(1);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(Duration::from_secs(5)),
            })
            .expect("bounded preview pixel readback");
        receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("readback callback")
            .expect("map preview pixels");
        let pixels = slice.get_mapped_range().expect("mapped preview pixels");
        assert!(
            pixels
                .chunks_exact(8)
                .any(|pixel| pixel[..6].iter().any(|byte| *byte != 0)),
            "project preview rendered entirely black"
        );
        drop(pixels);
        drop(texture);
    }

    #[test]
    fn playback_advances_keep_inflight_frame_but_explicit_seek_invalidates_it() {
        let initial = (23, 8, 42, [1280, 720]);
        let first_generation = super::advance_processing_generation(None, initial, 0, false);
        let latest_playback_frame = (23, 8, 45, [1280, 720]);
        let playback_generation = super::advance_processing_generation(
            Some(initial),
            latest_playback_frame,
            first_generation,
            false,
        );
        assert_eq!(playback_generation, first_generation);
        assert!(super::frame_matches_current_request(
            first_generation,
            23,
            8,
            1.75,
            playback_generation,
            Some(latest_playback_frame),
            false,
        ));

        let seek = (23, 9, 46, [1280, 720]);
        let seek_generation = super::advance_processing_generation(
            Some(latest_playback_frame),
            seek,
            playback_generation,
            false,
        );
        assert_ne!(seek_generation, playback_generation);
        assert!(!super::frame_matches_current_request(
            first_generation,
            23,
            8,
            1.75,
            seek_generation,
            Some(seek),
            false,
        ));
    }

    #[test]
    fn scrub_keeps_one_inflight_generation_and_retries_latest_pending_snapshot() {
        use super::{Snapshot, VideoPlayback, VideoWorkerStatus};
        use crate::{project::Project, timeline::TimelineState};

        let first_scrub = (23, 100, 42, [1280, 720]);
        let generation = super::advance_processing_generation(None, first_scrub, 8, true);
        let next_scrub = (23, 101, 43, [1280, 720]);
        let next_generation =
            super::advance_processing_generation(Some(first_scrub), next_scrub, generation, true);
        let latest_scrub = (23, 104, 47, [1280, 720]);
        let latest_generation = super::advance_processing_generation(
            Some(next_scrub),
            latest_scrub,
            next_generation,
            true,
        );

        assert_eq!(latest_generation, generation);
        assert!(super::frame_matches_current_request(
            generation,
            23,
            100,
            1.75,
            latest_generation,
            Some(latest_scrub),
            true,
        ));

        let released = (24, 104, 47, [1280, 720]);
        let released_generation = super::advance_processing_generation(
            Some(latest_scrub),
            released,
            latest_generation,
            false,
        );
        assert_ne!(released_generation, latest_generation);
        assert!(!super::frame_matches_current_request(
            generation,
            23,
            100,
            1.75,
            released_generation,
            Some(released),
            false,
        ));

        let key = (23, 104, 47, [1280, 720]);
        let timeline = TimelineState::default();
        let project = Project::new();
        let snapshot = Snapshot {
            worker_state: super::WorkerProjectState::capture(&project),
            project,
            timeline: timeline.document(),
            revision: 23,
            seek_revision: 104,
            playhead: 2.9375,
            playing: false,
            scrubbing: true,
            preview_size: [1280, 720],
            render_scale: 1.0,
            render_cache: None,
            captured_frame: None,
        };
        let mut playback = VideoPlayback::unavailable("", VideoWorkerStatus::Unsupported);
        playback.generation = latest_generation;
        playback.key = Some(key);
        playback.retry_pending = true;
        assert!(playback.should_queue_snapshot(key));
        playback.queue_snapshot(key, snapshot);
        assert_eq!(playback.generation, latest_generation);
        assert!(!playback.retry_pending);
        let (queued_generation, queued_snapshot) = playback.pending.as_ref().unwrap();
        assert_eq!(*queued_generation, latest_generation);
        assert_eq!(queued_snapshot.playhead, 2.9375);
        assert_eq!(queued_snapshot.seek_revision, 104);
    }

    #[test]
    fn timeline_drag_invalidation_does_not_cancel_live_scrub_preview() {
        use super::{VideoPlayback, VideoWorkerStatus};

        let key = (23, 100, 42, [640, 360]);
        let mut playback = VideoPlayback::unavailable("", VideoWorkerStatus::Unsupported);
        playback.scrubbing = true;
        playback.generation = 9;
        playback.key = Some(key);
        playback.waiting_for_video = true;
        playback.retry_pending = true;

        playback.invalidate();

        assert_eq!(playback.generation, 9);
        assert_eq!(playback.key, Some(key));
        assert!(playback.waiting_for_video);
        assert!(playback.retry_pending);
        assert!(!playback.invalidate_pending);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn child_renders_and_shares_exact_rgba16float_preview_pixels() {
        use std::{
            sync::mpsc,
            thread,
            time::{Duration, Instant},
        };

        use super::{Process, Request, Response, Snapshot};
        use crate::{
            project::{Project, ProjectBackground},
            runtime::shared_surface,
            timeline::TimelineState,
        };
        use std::sync::Arc;

        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = wgpu::Backends::METAL;
        let instance = wgpu::Instance::new(descriptor);
        let adapter = pollster::block_on(instance.request_adapter(&Default::default()))
            .expect("Metal adapter available for subprocess integration test");
        let (device, queue) = pollster::block_on(adapter.request_device(&Default::default()))
            .expect("request parent Metal device");
        let device = Arc::new(device);
        let registry_id = shared_surface::metal_registry_id(&device).expect("Metal registry ID");
        let process = Process::spawn(
            "",
            i32::try_from(std::process::id()).unwrap(),
            registry_id,
            Arc::clone(&device),
        )
        .expect("spawn the production video worker protocol");
        assert!(matches!(
            process.responses.recv_timeout(Duration::from_secs(15)),
            Ok(Response::Ready { pid }) if pid == process.pid
        ));

        let mut project = Project::new();
        project.active_composition_mut().settings.canvas_size = [2, 2];
        project.active_composition_mut().settings.background = ProjectBackground::Solid {
            color: [0.9, 0.1, 0.2, 1.0],
        };
        let selected_composition = project.create_composition("Worker preview target");
        let selected = project.composition_mut(selected_composition).unwrap();
        selected.settings.canvas_size = [4, 4];
        selected.settings.background = ProjectBackground::Solid {
            color: [0.25, 0.5, 0.75, 1.0],
        };
        assert!(project.set_active_composition(selected_composition));
        let timeline = TimelineState::default();
        process
            .requests
            .try_send(Request::Render {
                generation: 1,
                snapshot: Snapshot {
                    worker_state: super::WorkerProjectState::capture(&project),
                    project: project.clone(),
                    timeline: timeline.document(),
                    revision: 7,
                    seek_revision: 3,
                    playhead: 0.0,
                    playing: false,
                    scrubbing: false,
                    preview_size: [4, 4],
                    render_scale: 1.0,
                    render_cache: None,
                    captured_frame: None,
                },
            })
            .expect("submit one bounded preview request");
        let (generation, revision, seek_revision, playhead, channel, texture) = match process
            .responses
            .recv_timeout(Duration::from_secs(20))
            .expect("child render and authenticated surface import complete")
        {
            Response::Frame {
                generation,
                revision,
                seek_revision,
                playhead,
                channel,
                texture,
                ..
            } => (
                generation,
                revision,
                seek_revision,
                playhead,
                channel,
                texture,
            ),
            Response::Error(error) => panic!("child preview failed: {error}"),
            Response::Diagnostic { message, .. } => {
                panic!("child emitted diagnostic before frame: {message}")
            }
            Response::Ready { .. } => panic!("unexpected second worker handshake"),
        };
        assert_eq!(
            (generation, revision, seek_revision, playhead),
            (1, 7, 3, 0.0)
        );

        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("video worker subprocess readback"),
            size: 1_024,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(4),
                },
            },
            wgpu::Extent3d {
                width: 4,
                height: 4,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);
        let slice = buffer.slice(..);
        let (tx, rx) = mpsc::sync_channel(1);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(Duration::from_secs(5)),
            })
            .expect("bounded parent GPU readback");
        rx.recv_timeout(Duration::from_secs(1))
            .expect("readback callback")
            .expect("map shared pixels");
        let mapped = slice.get_mapped_range().expect("mapped pixel buffer");
        let expected = [0x00, 0x34, 0x00, 0x38, 0x00, 0x3a, 0x00, 0x3c];
        for row in mapped.chunks_exact(256).take(4) {
            for pixel in row[..32].chunks_exact(8) {
                assert_eq!(pixel, expected);
            }
        }
        drop(mapped);
        drop(texture);
        let _ = process.requests.try_send(Request::Release { channel });

        process
            .requests
            .try_send(Request::Render {
                generation: 2,
                snapshot: Snapshot {
                    worker_state: super::WorkerProjectState::capture(&project),
                    project: project.clone(),
                    timeline: timeline.document(),
                    revision: 8,
                    seek_revision: 4,
                    playhead: 0.25,
                    playing: true,
                    scrubbing: false,
                    preview_size: [8, 4],
                    render_scale: 1.0,
                    render_cache: None,
                    captured_frame: None,
                },
            })
            .expect("submit resized preview request after releasing prior slot");
        let (resized_channel, resized_texture) = match process
            .responses
            .recv_timeout(Duration::from_secs(20))
            .expect("child resize render and import complete")
        {
            Response::Frame {
                generation: 2,
                channel,
                texture,
                ..
            } => (channel, texture),
            Response::Error(error) => panic!("resized child preview failed: {error}"),
            _ => panic!("unexpected response to resized frame request"),
        };
        assert_eq!((resized_texture.width(), resized_texture.height()), (8, 4));
        drop(resized_texture);
        let _ = process.requests.try_send(Request::Release {
            channel: resized_channel.clone(),
        });
        process
            .requests
            .try_send(Request::Render {
                generation: 2,
                snapshot: Snapshot {
                    worker_state: super::WorkerProjectState::capture(&project),
                    project: project.clone(),
                    timeline: timeline.document(),
                    revision: 8,
                    seek_revision: 4,
                    playhead: 0.5,
                    playing: true,
                    scrubbing: false,
                    preview_size: [8, 4],
                    render_scale: 1.0,
                    render_cache: None,
                    captured_frame: None,
                },
            })
            .expect("render next playback frame into a released surface");
        match process.responses.recv_timeout(Duration::from_secs(20)) {
            Ok(Response::Frame {
                channel, playhead, ..
            }) => {
                assert_eq!(channel, resized_channel, "reuse the released GPU surface");
                assert_eq!(playhead, 0.5);
                process
                    .requests
                    .try_send(Request::Release { channel })
                    .unwrap();
            }
            Ok(Response::Error(error)) => panic!("next playback frame failed: {error}"),
            _ => panic!("expected next playback frame"),
        }

        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if process.requests.try_send(Request::Shutdown).is_ok() {
                break;
            }
            assert!(Instant::now() < deadline, "bounded worker shutdown queue");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(all(target_os = "macos", debug_assertions))]
    #[test]
    fn video_worker_pid_is_retained_after_timeout_and_replaced_on_restart() {
        use std::sync::Arc;

        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = wgpu::Backends::METAL;
        let instance = wgpu::Instance::new(descriptor);
        let adapter = pollster::block_on(instance.request_adapter(&Default::default())).unwrap();
        let (device, _) = pollster::block_on(adapter.request_device(&Default::default())).unwrap();
        let device = Arc::new(device);
        let mut playback = super::VideoPlayback::new("", Arc::clone(&device));
        let original = playback
            .pid()
            .expect("worker has a PID immediately after spawning");

        playback.fail(super::VideoWorkerStatus::Unresponsive);
        assert_eq!(playback.status(), super::VideoWorkerStatus::Unresponsive);
        assert_eq!(playback.pid(), Some(original));
        assert!(playback.process.is_none());

        playback.restart(device);
        let replacement = playback.pid().expect("replacement worker has a PID");
        assert_ne!(replacement, original);
        assert_eq!(playback.status(), super::VideoWorkerStatus::Restarting);
    }

    #[cfg(all(target_os = "macos", debug_assertions))]
    #[test]
    fn crashed_video_worker_exits_without_an_automatic_replacement() {
        use std::{
            sync::Arc,
            thread,
            time::{Duration, Instant},
        };

        use super::{Process, Request, Response};
        use crate::runtime::shared_surface;

        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = wgpu::Backends::METAL;
        let instance = wgpu::Instance::new(descriptor);
        let adapter = pollster::block_on(instance.request_adapter(&Default::default()))
            .expect("Metal adapter available for subprocess crash test");
        let (device, _queue) = pollster::block_on(adapter.request_device(&Default::default()))
            .expect("request parent Metal device");
        let device = Arc::new(device);
        let registry_id = shared_surface::metal_registry_id(&device).expect("Metal registry ID");
        let mut process = Process::spawn(
            "",
            i32::try_from(std::process::id()).unwrap(),
            registry_id,
            Arc::clone(&device),
        )
        .expect("spawn video worker");
        assert!(matches!(
            process.responses.recv_timeout(Duration::from_secs(15)),
            Ok(Response::Ready { pid }) if pid == process.pid
        ));
        process
            .requests
            .try_send(Request::Crash)
            .expect("send developer crash request");

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if process
                .child
                .as_mut()
                .and_then(|child| child.try_wait().ok().flatten())
                .is_some()
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "video worker crash detection deadline"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            process
                .child
                .as_mut()
                .unwrap()
                .try_wait()
                .unwrap()
                .is_some()
        );
    }
}
