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
    runtime::{cpu_surface, gpu_surface, shared_surface, video::PresentationTexture},
    timeline::{TimelineDocument, TimelineState},
};

const PROTOCOL: u16 = 2;
const MAX_LINE_BYTES: usize = 64 * 1024 * 1024;

fn deadline_after(duration: Duration) -> Instant {
    Instant::now()
        .checked_add(duration)
        .unwrap_or_else(Instant::now)
}

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
    captured_frame: Option<cpu_surface::CpuCaptureDescriptor>,
    cpu_slots: Vec<cpu_surface::CpuSurfaceDescriptor>,
    force_cpu: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct FrameTransport {
    seek_revision: u64,
    playhead: f32,
    playing: bool,
    scrubbing: bool,
    preview_size: [u32; 2],
    render_scale: f32,
    render_cache: Option<RenderCachePreview>,
    captured_frame: Option<cpu_surface::CpuCaptureDescriptor>,
    cpu_slots: Vec<cpu_surface::CpuSurfaceDescriptor>,
    force_cpu: bool,
}

impl FrameTransport {
    fn apply(self, snapshot: &mut Snapshot) {
        snapshot.seek_revision = self.seek_revision;
        snapshot.playhead = self.playhead;
        snapshot.playing = self.playing;
        snapshot.scrubbing = self.scrubbing;
        snapshot.preview_size = self.preview_size;
        snapshot.render_scale = self.render_scale;
        snapshot.render_cache = self.render_cache;
        snapshot.captured_frame = self.captured_frame;
        snapshot.cpu_slots = self.cpu_slots;
        snapshot.force_cpu = self.force_cpu;
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Request {
    Hello {
        protocol: u16,
        parent_pid: i32,
        metal_registry_id: Option<u64>,
        #[serde(default)]
        gpu_identity: Option<gpu_surface::GpuIdentity>,
        cpu_session: cpu_surface::CpuSurfaceSessionDescriptor,
        plugin_paths: String,
    },
    Render {
        generation: u64,
        snapshot: Box<Snapshot>,
    },
    RenderCached {
        generation: u64,
        revision: u64,
        transport: FrameTransport,
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
        descriptor: FrameSurfaceDescriptor,
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

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "descriptor", rename_all = "snake_case")]
enum FrameSurfaceDescriptor {
    Metal(shared_surface::SharedSurfaceDescriptor),
    Gpu(gpu_surface::Descriptor),
    Cpu(cpu_surface::CpuSurfaceDescriptor),
}

enum PreviewSurface {
    Metal(shared_surface::SharedSurface),
    Native(Box<gpu_surface::SharedSurface>),
}

impl PreviewSurface {
    fn texture(&self) -> &wgpu::Texture {
        match self {
            Self::Metal(surface) => surface.texture(),
            Self::Native(surface) => surface.texture(),
        }
    }

    fn dimensions(&self) -> [u32; 2] {
        match self {
            Self::Metal(surface) => [surface.descriptor().width, surface.descriptor().height],
            Self::Native(surface) => [surface.descriptor().width, surface.descriptor().height],
        }
    }

    fn channel(&self) -> &str {
        match self {
            Self::Metal(surface) => &surface.descriptor().channel,
            Self::Native(surface) => &surface.descriptor().channel,
        }
    }

    fn publish(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<FrameSurfaceDescriptor> {
        match self {
            Self::Metal(surface) => {
                device
                    .poll(wgpu::PollType::Wait {
                        submission_index: None,
                        timeout: Some(Duration::from_secs(5)),
                    })
                    .context("wait for child GPU render")?;
                Ok(FrameSurfaceDescriptor::Metal(surface.descriptor().clone()))
            }
            Self::Native(surface) => {
                surface.publish(device, queue)?;
                Ok(FrameSurfaceDescriptor::Gpu(surface.descriptor().clone()))
            }
        }
    }
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
        channel: Option<String>,
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
    cpu_surfaces: Arc<cpu_surface::CpuSurfaceSession>,
    force_cpu: Arc<std::sync::atomic::AtomicBool>,
}

struct ChildGuard {
    child: Option<Child>,
    cpu_surfaces: Option<Arc<cpu_surface::CpuSurfaceSession>>,
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let cpu_surfaces = self.cpu_surfaces.take();
            let _ = child.kill();
            thread::spawn(move || {
                let _ = child.wait();
                drop(cpu_surfaces);
            });
        }
    }
}

impl Process {
    fn spawn(
        plugin_paths: &str,
        parent_pid: i32,
        metal_registry_id: Option<u64>,
        device: &Arc<wgpu::Device>,
        queue: &Arc<wgpu::Queue>,
        force_cpu: bool,
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
            .env("KAMA_VIDEO_WORKER_TEST_CHILD", "1")
            .env(
                "KAMA_VIDEO_FORCE_CPU_SURFACE_TEST_CHILD",
                if force_cpu { "1" } else { "0" },
            );
        let child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("spawn video worker")?;
        let mut child_guard = ChildGuard {
            child: Some(child),
            cpu_surfaces: None,
        };
        let child = child_guard
            .child
            .as_mut()
            .context("video worker process unavailable")?;
        let pid = child.id();
        let cpu_surfaces = Arc::new(cpu_surface::CpuSurfaceSession::create(
            parent_pid,
            i32::try_from(pid).unwrap_or(i32::MAX),
        )?);
        child_guard.cpu_surfaces = Some(Arc::clone(&cpu_surfaces));
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
            .spawn(move || drain_diagnostics(stderr, &diagnostics_output))?;
        let metrics_output = Arc::downgrade(&metrics);
        thread::Builder::new()
            .name(format!("kama-video-metrics-{pid}"))
            .spawn(move || sample_process_metrics(pid, &metrics_output))?;
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
            .spawn(move || read_responses(stdout, &wire_tx, cfg!(all(test, debug_assertions))))?;
        let importer = Arc::clone(device);
        let importer_queue = Arc::clone(queue);
        let importer_cpu_surfaces = Arc::clone(&cpu_surfaces);
        let importer_force_cpu = Arc::new(std::sync::atomic::AtomicBool::new(force_cpu));
        let importer_force_cpu_response = Arc::clone(&importer_force_cpu);
        let importer_releases = Arc::clone(&releases);
        thread::Builder::new()
            .name(format!("kama-video-import-{pid}"))
            .spawn(move || {
                import_responses(
                    &wire_rx,
                    &response_tx,
                    &importer,
                    &importer_queue,
                    &importer_cpu_surfaces,
                    &importer_force_cpu_response,
                    &importer_releases,
                    pid,
                );
            })?;
        request_tx
            .try_send(Request::Hello {
                protocol: PROTOCOL,
                parent_pid,
                metal_registry_id,
                gpu_identity: gpu_surface::identity(device).ok(),
                cpu_session: cpu_surfaces.descriptor(),
                plugin_paths: plugin_paths.to_owned(),
            })
            .map_err(|error| anyhow::anyhow!("send video worker handshake: {error}"))?;
        Ok(Self {
            child: child_guard.child.take(),
            requests: request_tx,
            responses: response_rx,
            pid,
            reader: Some(reader),
            writer: Some(writer),
            metrics,
            diagnostics,
            diagnostics_reader: Some(diagnostics_reader),
            releases,
            cpu_surfaces,
            force_cpu: importer_force_cpu,
        })
    }

    fn stop(&mut self) {
        let _ = self.requests.try_send(Request::Shutdown);
        if let Some(mut child) = self.child.take() {
            let cpu_surfaces = Arc::clone(&self.cpu_surfaces);
            let _ = child.kill();
            thread::spawn(move || {
                let _ = child.wait();
                drop(cpu_surfaces);
            });
        }
        self.reader.take();
        self.writer.take();
        self.diagnostics_reader.take();
    }
}

fn drain_diagnostics(stderr: impl std::io::Read, output: &Arc<Mutex<VecDeque<String>>>) {
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

fn sample_process_metrics(pid: u32, output: &std::sync::Weak<Mutex<Option<(f32, u64)>>>) {
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
    let mut cached_project = None;
    for request in requests {
        let request = match request {
            Request::Render {
                generation,
                snapshot,
            } if cached_project
                == Some((snapshot.revision, snapshot.project.active_composition)) =>
            {
                Request::RenderCached {
                    generation,
                    revision: snapshot.revision,
                    transport: FrameTransport {
                        seek_revision: snapshot.seek_revision,
                        playhead: snapshot.playhead,
                        playing: snapshot.playing,
                        scrubbing: snapshot.scrubbing,
                        preview_size: snapshot.preview_size,
                        render_scale: snapshot.render_scale,
                        render_cache: snapshot.render_cache,
                        captured_frame: snapshot.captured_frame,
                        cpu_slots: snapshot.cpu_slots,
                        force_cpu: snapshot.force_cpu,
                    },
                }
            }
            Request::Render {
                generation,
                snapshot,
            } => {
                cached_project = Some((snapshot.revision, snapshot.project.active_composition));
                Request::Render {
                    generation,
                    snapshot,
                }
            }
            Request::Invalidate => {
                cached_project = None;
                Request::Invalidate
            }
            other => other,
        };
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

fn queue_release(releases: &Arc<Mutex<VecDeque<String>>>, channel: String) {
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

fn transport_readiness_matches(
    ready: Option<(u64, bool, bool, u64)>,
    current: (u64, bool, bool, u64),
) -> bool {
    ready == Some(current)
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
            .map_or(available.len(), |index| index.saturating_add(1));
        if bytes.len().saturating_add(segment_len) > MAX_LINE_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "video worker response exceeded protocol limit",
            ));
        }
        let has_newline = segment_len
            .checked_sub(1)
            .and_then(|index| available.get(index))
            == Some(&b'\n');
        let segment = available.get(..segment_len).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "video worker response segment is out of bounds",
            )
        })?;
        bytes.extend_from_slice(segment);
        reader.consume(segment_len);
        if has_newline {
            return Ok(Some(String::from_utf8_lossy(&bytes).into_owned()));
        }
    }
}

fn read_responses(
    stdout: impl std::io::Read,
    responses: &mpsc::SyncSender<WireResponse>,
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

#[expect(
    clippy::too_many_arguments,
    reason = "the importer needs the supervised process resources and channels to transfer each frame"
)]
fn import_responses(
    responses: &mpsc::Receiver<WireResponse>,
    output: &mpsc::SyncSender<Response>,
    device: &Arc<wgpu::Device>,
    queue: &Arc<wgpu::Queue>,
    cpu_surfaces: &Arc<cpu_surface::CpuSurfaceSession>,
    force_cpu: &Arc<std::sync::atomic::AtomicBool>,
    releases: &Arc<Mutex<VecDeque<String>>>,
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
                let (channel, texture) = match descriptor {
                    FrameSurfaceDescriptor::Gpu(descriptor) => {
                        let channel = descriptor.channel.clone();
                        let texture = gpu_surface::receive(
                            device,
                            queue,
                            &descriptor,
                            &cpu_surfaces.descriptor(),
                        );
                        queue_release(releases, channel);
                        (None, texture)
                    }
                    FrameSurfaceDescriptor::Metal(descriptor) => {
                        let channel = descriptor.channel.clone();
                        let texture = imported.get(&channel).map_or_else(
                            || {
                                shared_surface::ImportedSurface::receive(
                                    device,
                                    &descriptor,
                                    i32::try_from(pid).unwrap_or(i32::MAX),
                                    Duration::from_secs(2),
                                )
                                .map(shared_surface::ImportedSurface::into_texture)
                            },
                            |texture| Ok(texture.clone()),
                        );
                        (Some(channel), texture)
                    }
                    FrameSurfaceDescriptor::Cpu(descriptor) => {
                        force_cpu.store(true, std::sync::atomic::Ordering::Release);
                        let texture = cpu_surfaces.read_frame(&descriptor, pid).map(|pixels| {
                            let texture = device.create_texture(&wgpu::TextureDescriptor {
                                label: Some("Kama imported CPU preview frame"),
                                size: wgpu::Extent3d {
                                    width: descriptor.width,
                                    height: descriptor.height,
                                    depth_or_array_layers: 1,
                                },
                                mip_level_count: 1,
                                sample_count: 1,
                                dimension: wgpu::TextureDimension::D2,
                                format: wgpu::TextureFormat::Rgba16Float,
                                usage: wgpu::TextureUsages::TEXTURE_BINDING
                                    | wgpu::TextureUsages::STORAGE_BINDING
                                    | wgpu::TextureUsages::COPY_DST
                                    | wgpu::TextureUsages::COPY_SRC,
                                view_formats: &[],
                            });
                            queue.write_texture(
                                wgpu::TexelCopyTextureInfo {
                                    texture: &texture,
                                    mip_level: 0,
                                    origin: wgpu::Origin3d::ZERO,
                                    aspect: wgpu::TextureAspect::All,
                                },
                                &pixels,
                                wgpu::TexelCopyBufferLayout {
                                    offset: 0,
                                    bytes_per_row: Some(descriptor.stride),
                                    rows_per_image: Some(descriptor.height),
                                },
                                wgpu::Extent3d {
                                    width: descriptor.width,
                                    height: descriptor.height,
                                    depth_or_array_layers: 1,
                                },
                            );
                            texture
                        });
                        (None, texture)
                    }
                };
                match texture {
                    Ok(texture) => {
                        if let Some(channel) = &channel {
                            imported.insert(channel.clone(), texture.clone());
                            while imported.len() > 3 {
                                let evict = imported
                                    .keys()
                                    .find(|candidate| **candidate != *channel)
                                    .cloned();
                                if let Some(evict) = evict {
                                    imported.remove(&evict);
                                } else {
                                    break;
                                }
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
                    Err(error) => {
                        if let Some(channel) = channel {
                            queue_release(releases, channel);
                        }
                        force_cpu.store(true, std::sync::atomic::Ordering::Release);
                        Response::Error(format!("import video frame: {error:#}"))
                    }
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

#[expect(
    clippy::struct_excessive_bools,
    reason = "worker transport, request lifecycle, and readiness are independent states"
)]
pub struct VideoPlayback {
    process: Option<Process>,
    last_pid: Option<u32>,
    plugin_paths: String,
    status: VideoWorkerStatus,
    registry_id: Option<u64>,
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
    current_capture: Option<cpu_surface::CpuCaptureDescriptor>,
    inflight_capture: Option<cpu_surface::CpuCaptureDescriptor>,
    capture_identity: Option<(usize, usize, [u32; 2])>,
    capture_epoch: u64,
    transport_key: Option<(u64, bool, bool)>,
    readiness_revision: Option<u64>,
    readiness_editing: bool,
    ready_transport: Option<(u64, bool, bool, u64)>,
    #[cfg(debug_assertions)]
    crash_requested: bool,
}

impl VideoPlayback {
    pub fn new(plugin_paths: &str, device: &Arc<wgpu::Device>, queue: &Arc<wgpu::Queue>) -> Self {
        let registry_id = shared_surface::metal_registry_id(device).ok();
        let force_cpu = std::env::var_os("KAMA_VIDEO_FORCE_CPU_SURFACE_TEST_CHILD")
            .is_some_and(|value| value == "1");
        let process = Self::spawn(plugin_paths, registry_id, device, queue, force_cpu);
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
            startup_deadline: deadline_after(Duration::from_secs(15)),
            request_started: None,
            live_edit_revision: 0,
            last_live_snapshot: None,
            waiting_for_video: false,
            scrubbing: false,
            invalidate_pending: false,
            deferred_releases: VecDeque::new(),
            current_capture: None,
            inflight_capture: None,
            capture_identity: None,
            capture_epoch: 0,
            transport_key: None,
            readiness_revision: None,
            readiness_editing: false,
            ready_transport: None,
            #[cfg(debug_assertions)]
            crash_requested: false,
        }
    }

    pub(crate) fn playback_ready(
        &self,
        timeline: &TimelineState,
        revision: u64,
        editing: bool,
    ) -> bool {
        if !timeline.is_playing() || timeline.is_scrubbing() {
            return true;
        }
        if self.transport_key
            != Some((
                timeline.seek_revision(),
                timeline.is_playing(),
                timeline.is_scrubbing(),
            ))
        {
            return false;
        }
        self.readiness_revision == Some(revision)
            && self.readiness_editing == editing
            && transport_readiness_matches(
                self.ready_transport,
                (
                    timeline.seek_revision(),
                    timeline.is_playing(),
                    timeline.is_scrubbing(),
                    revision,
                ),
            )
            && self.texture.is_some()
            && matches!(self.status, VideoWorkerStatus::Ready { .. })
    }

    #[cfg(test)]
    fn unavailable(plugin_paths: &str, status: VideoWorkerStatus) -> Self {
        Self {
            process: None,
            last_pid: None,
            plugin_paths: plugin_paths.to_owned(),
            status,
            registry_id: None,
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
            current_capture: None,
            inflight_capture: None,
            capture_identity: None,
            capture_epoch: 0,
            transport_key: None,
            readiness_revision: None,
            readiness_editing: false,
            ready_transport: None,
            #[cfg(debug_assertions)]
            crash_requested: false,
        }
    }

    fn spawn(
        plugin_paths: &str,
        registry_id: Option<u64>,
        device: &Arc<wgpu::Device>,
        queue: &Arc<wgpu::Queue>,
        force_cpu: bool,
    ) -> Result<Process> {
        Process::spawn(
            plugin_paths,
            i32::try_from(std::process::id()).unwrap_or(i32::MAX),
            registry_id,
            device,
            queue,
            force_cpu,
        )
    }

    pub const fn status(&self) -> VideoWorkerStatus {
        self.status
    }

    pub const fn pid(&self) -> Option<u32> {
        self.last_pid
    }

    pub const fn is_waiting_for_video(&self) -> bool {
        self.process.is_some()
            && (self.waiting_for_video
                || self.request_pending
                || self.pending.is_some()
                || matches!(
                    self.status,
                    VideoWorkerStatus::Starting | VideoWorkerStatus::Restarting
                ))
    }

    pub const fn invalidate(&mut self) {
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
    pub const fn crash_for_test(&mut self) {
        self.crash_requested = matches!(self.status, VideoWorkerStatus::Ready { .. });
    }

    pub fn restart(&mut self, device: &Arc<wgpu::Device>, queue: &Arc<wgpu::Queue>) {
        if !matches!(
            self.status,
            VideoWorkerStatus::Crashed
                | VideoWorkerStatus::Unresponsive
                | VideoWorkerStatus::StartupFailed
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
        self.current_capture = None;
        self.inflight_capture = None;
        self.capture_identity = None;
        self.capture_epoch = self.capture_epoch.wrapping_add(1);
        self.request_pending = false;
        self.invalidate_pending = false;
        self.deferred_releases.clear();
        self.registry_id = shared_surface::metal_registry_id(device).ok();
        let force_cpu = std::env::var_os("KAMA_VIDEO_FORCE_CPU_SURFACE_TEST_CHILD")
            .is_some_and(|value| value == "1");
        match Self::spawn(
            &self.plugin_paths,
            self.registry_id,
            device,
            queue,
            force_cpu,
        ) {
            Ok(process) => {
                self.last_pid = Some(process.pid);
                self.process = Some(process);
                self.status = VideoWorkerStatus::Restarting;
                self.startup_deadline = deadline_after(Duration::from_secs(15));
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
        self.ready_transport = None;
    }

    fn queue_snapshot(&mut self, key: (u64, u64, i64, [u32; 2]), snapshot: Snapshot) {
        self.pending = Some((self.generation, snapshot));
        self.key = Some(key);
        self.retry_pending = false;
    }

    fn should_queue_snapshot(&self, key: (u64, u64, i64, [u32; 2])) -> bool {
        self.key != Some(key) || self.retry_pending
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the arguments form one complete snapshot request, including its optional cache and capture inputs"
    )]
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
        self.transport_key = Some((
            timeline.seek_revision(),
            timeline.is_playing(),
            timeline.is_scrubbing(),
        ));
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
        self.readiness_revision = Some(revision);
        self.readiness_editing = live_editing;
        let frame =
            (f64::from(timeline.playhead()) * project.active_settings().frame_rate.max(1.0)) as i64;
        let capture_identity =
            captured_frame.map(|(size, pixels)| (pixels.as_ptr() as usize, pixels.len(), size));
        if capture_identity != self.capture_identity {
            self.capture_epoch = self.capture_epoch.wrapping_add(1);
            self.capture_identity = capture_identity;
        }
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
            (pixels.as_ptr() as usize).hash(&mut content_hasher);
            pixels.len().hash(&mut content_hasher);
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
            let captured_frame = match (captured_frame, self.process.as_ref()) {
                (Some((size, pixels)), Some(process)) => Some(process.cpu_surfaces.cache_capture(
                    size,
                    pixels,
                    self.capture_epoch,
                    self.inflight_capture.as_ref(),
                )?),
                _ => None,
            };
            self.current_capture.clone_from(&captured_frame);
            self.retain_capture_files();
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
                captured_frame,
                cpu_slots: Vec::new(),
                force_cpu: false,
            };
            self.queue_snapshot(key, snapshot);
            self.retain_capture_files();
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
            && let Some((generation, mut snapshot)) = self.pending.take()
        {
            snapshot.cpu_slots = process.cpu_surfaces.prepare([
                snapshot.preview_size[0].max(1),
                snapshot.preview_size[1].max(1),
            ])?;
            snapshot.force_cpu = process.force_cpu.load(std::sync::atomic::Ordering::Acquire);
            let sent_capture = snapshot.captured_frame.clone();
            match process.requests.try_send(Request::Render {
                generation,
                snapshot: Box::new(snapshot),
            }) {
                Ok(()) => {
                    self.request_pending = true;
                    self.request_started = Some(Instant::now());
                    self.inflight_capture = sent_capture;
                    self.retry_pending = false;
                }
                Err(mpsc::TrySendError::Full(Request::Render {
                    generation,
                    snapshot,
                })) => {
                    self.pending = Some((generation, *snapshot));
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
                    self.inflight_capture = None;
                    self.retain_capture_files();
                    let renderer_key = renderer.registration_id();
                    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                    let id = match self.texture_ids.get(&renderer_key).copied() {
                        Some(id) => renderer.replace_external_texture(id, view).map(|()| id),
                        None => renderer.register_external_texture(view),
                    };
                    match id {
                        Ok(id) => {
                            self.texture_ids.insert(renderer_key, id);
                            self.active_renderer_key = Some(renderer_key);
                            let old_channel = std::mem::replace(&mut self.channel, channel);
                            let old_texture = self.texture.replace(texture);
                            self.source_geometry = source_geometry;
                            self.waiting_for_video = pending;
                            self.retry_pending = pending;
                            if !pending {
                                self.ready_transport = Some((
                                    seek_revision,
                                    self.transport_key.is_some_and(|key| key.1),
                                    self.transport_key.is_some_and(|key| key.2),
                                    self.readiness_revision.unwrap_or_default(),
                                ));
                            }
                            self.status = match self.status {
                                VideoWorkerStatus::Ready { pid } => {
                                    VideoWorkerStatus::Ready { pid }
                                }
                                other => other,
                            };
                            if let Some(old_texture) = old_texture {
                                let releases = self
                                    .process
                                    .as_ref()
                                    .map(|process| Arc::clone(&process.releases));
                                renderer.queue().on_submitted_work_done(move || {
                                    drop(old_texture);
                                    if let (Some(channel), Some(releases)) = (old_channel, releases)
                                    {
                                        queue_release(&releases, channel);
                                    }
                                });
                            }
                        }
                        Err(error) => {
                            if let (Some(process), Some(channel)) = (self.process.as_ref(), channel)
                            {
                                queue_release(&process.releases, channel);
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
                    self.inflight_capture = None;
                    self.retain_capture_files();
                    if let (Some(channel), Some(process)) = (channel, self.process.as_ref()) {
                        queue_release(&process.releases, channel);
                    }
                }
                Response::Error(message) => {
                    self.request_pending = false;
                    self.request_started = None;
                    self.inflight_capture = None;
                    self.retain_capture_files();
                    if message.starts_with("import video frame:")
                        && let Some(process) = self.process.as_ref()
                    {
                        process
                            .force_cpu
                            .store(true, std::sync::atomic::Ordering::Release);
                        self.retry_pending = true;
                        messages::warning(
                            "Video worker",
                            format!("retrying preview through shared CPU memory: {message}"),
                        );
                        continue;
                    }
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
        let exit_detail = exit_result.and_then(|result| match result {
            Err(error) => Some(format!("could not inspect child exit: {error}")),
            Ok(Some(status)) => Some(format!(
                "process {} exited with {status}",
                self.process.as_ref().map_or(0, |process| process.pid)
            )),
            Ok(None) => None,
        });
        if let Some(detail) = exit_detail {
            self.fail_with_detail(VideoWorkerStatus::Crashed, &detail);
        } else if (matches!(
            self.status,
            VideoWorkerStatus::Starting | VideoWorkerStatus::Restarting
        ) && Instant::now() >= self.startup_deadline)
            || self
                .request_started
                .is_some_and(|started| started.elapsed() >= Duration::from_secs(15))
        {
            self.fail(VideoWorkerStatus::Unresponsive);
        }
    }

    fn fail(&mut self, status: VideoWorkerStatus) {
        self.fail_with_detail(status, "");
    }

    fn fail_with_detail(&mut self, status: VideoWorkerStatus, detail: &str) {
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

    fn retain_capture_files(&self) {
        if let Some(process) = self.process.as_ref() {
            let keep = self
                .current_capture
                .iter()
                .chain(self.inflight_capture.iter())
                .chain(
                    self.pending
                        .iter()
                        .filter_map(|(_, snapshot)| snapshot.captured_frame.as_ref()),
                )
                .cloned()
                .collect::<Vec<_>>();
            process.cpu_surfaces.retain_captures(&keep);
        }
    }

    fn present_current(&mut self, renderer: &mut kama_ui::Renderer) -> Result<()> {
        let key = renderer.registration_id();
        if self.active_renderer_key == Some(key) || self.texture.is_none() {
            return Ok(());
        }
        if let Some(texture) = &self.texture {
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
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

pub fn run_video_worker() -> Result<()> {
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let Some(line) = read_line(&mut reader)? else {
        anyhow::bail!("video worker handshake missing");
    };
    let Request::Hello {
        protocol,
        parent_pid,
        metal_registry_id,
        gpu_identity,
        cpu_session,
        plugin_paths,
    } = serde_json::from_str(&line).context("decode video worker handshake")?
    else {
        anyhow::bail!("video worker handshake expected");
    };
    anyhow::ensure!(protocol == PROTOCOL, "unsupported video worker protocol");
    cpu_surface::validate_session(&cpu_session, parent_pid)?;
    anyhow::ensure!(
        cpu_session.exporter_pid == i32::try_from(std::process::id()).unwrap_or(i32::MAX),
        "CPU surface session is not owned by this video worker"
    );
    let (instance, adapter, device, queue) = if let Some(registry_id) = metal_registry_id {
        create_matching_metal_device(registry_id)?
    } else if let Some(identity) = gpu_identity.as_ref() {
        match create_matching_gpu_device(identity) {
            Ok(device) => device,
            Err(error) => {
                messages::warning(
                    "Video worker",
                    format!("GPU sharing unavailable; using CPU transfer: {error:#}"),
                );
                crate::render::create_processing_device(None)?
            }
        }
    } else {
        crate::render::create_processing_device(None)?
    };
    let native_gpu_sharing = gpu_identity
        .as_ref()
        .is_some_and(|expected| gpu_surface::identity(&device).as_ref().ok() == Some(expected));
    let mut plugins = PluginRegistry::load_default(&plugin_paths)?;
    plugins.validate_gpu(&device);
    let mut effects = EffectRuntime::default();
    let mut frame_renderer = FrameRenderer::new_preview_worker(
        Arc::new(device.clone()),
        Arc::new(queue.clone()),
        &effects,
        &plugins,
    );
    #[cfg(test)]
    let force_cpu_test_child = std::env::var_os("KAMA_VIDEO_FORCE_CPU_SURFACE_TEST_CHILD")
        .is_some_and(|value| value == "1");
    #[cfg(not(test))]
    let force_cpu_test_child = false;
    let mut effects_revision = None;
    write_response(&WireResponse::Ready {
        protocol: PROTOCOL,
        pid: std::process::id(),
    })?;
    let mut surfaces = HashMap::<String, PreviewSurface>::new();
    let mut free_surfaces = VecDeque::<String>::new();
    let mut cached_snapshot: Option<Snapshot> = None;
    while let Some(line) = read_line(&mut reader)? {
        let request: Request =
            serde_json::from_str(&line).context("decode video worker request")?;
        let request = match request {
            Request::Render {
                generation,
                snapshot,
            } => {
                cached_snapshot = Some(*snapshot.clone());
                Request::Render {
                    generation,
                    snapshot,
                }
            }
            Request::RenderCached {
                generation,
                revision,
                transport,
            } => {
                let Some(mut snapshot) = cached_snapshot
                    .as_ref()
                    .filter(|snapshot| snapshot.revision == revision)
                    .cloned()
                else {
                    write_response(&WireResponse::Error {
                        message: "video frame references an unavailable project revision".into(),
                    })?;
                    continue;
                };
                transport.apply(&mut snapshot);
                Request::Render {
                    generation,
                    snapshot: Box::new(snapshot),
                }
            }
            other => other,
        };
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
                    frame_renderer.sync_compiled_effects_on(&device, &effects, &plugins);
                    effects_revision = Some(snapshot.revision);
                }
                let mut timeline = TimelineState::default();
                timeline.load_document(snapshot.timeline.clone());
                timeline.set_worker_transport(snapshot.playhead, snapshot.playing);
                let captured_frame = if let Some(capture) = snapshot.captured_frame.as_ref() {
                    match cpu_surface::read_capture(&cpu_session, capture, std::process::id()) {
                        Ok(pixels) => Some(((capture.width, capture.height), pixels)),
                        Err(error) => {
                            write_response(&WireResponse::Error {
                                message: format!("read parent-owned capture frame: {error:#}"),
                            })?;
                            continue;
                        }
                    }
                } else {
                    None
                };
                let [width, height] = snapshot.preview_size;
                let dimensions = [width.max(1), height.max(1)];
                let mut channel = None;
                let mut gpu_surface_error = None;
                let mut cpu_slot = None;
                let output_texture = if !snapshot.force_cpu
                    && !force_cpu_test_child
                    && (metal_registry_id.is_some() || native_gpu_sharing)
                {
                    while let Some(candidate) = free_surfaces.pop_front() {
                        if surfaces
                            .get(&candidate)
                            .is_some_and(|surface| surface.dimensions() != dimensions)
                        {
                            surfaces.remove(&candidate);
                        } else {
                            free_surfaces.push_front(candidate);
                            break;
                        }
                    }
                    let reusable = free_surfaces.pop_front();
                    if let Some(candidate) = reusable {
                        channel = Some(candidate.clone());
                        surfaces
                            .get(&candidate)
                            .map(|surface| surface.texture().clone())
                    } else if surfaces.len() < 2 {
                        let allocation = if metal_registry_id.is_some() {
                            shared_surface::SharedSurface::allocate(
                                &device,
                                dimensions[0],
                                dimensions[1],
                                parent_pid,
                            )
                            .map(PreviewSurface::Metal)
                        } else {
                            gpu_surface::SharedSurface::allocate(
                                &device,
                                dimensions[0],
                                dimensions[1],
                                &cpu_session,
                            )
                            .map(Box::new)
                            .map(PreviewSurface::Native)
                        };
                        match allocation {
                            Ok(surface) => {
                                let candidate = surface.channel().to_owned();
                                let texture = surface.texture().clone();
                                surfaces.insert(candidate.clone(), surface);
                                channel = Some(candidate);
                                Some(texture)
                            }
                            Err(error) => {
                                gpu_surface_error =
                                    Some(format!("allocate preview surface: {error:#}"));
                                None
                            }
                        }
                    } else {
                        gpu_surface_error = Some("GPU preview surface slots are full".into());
                        None
                    }
                } else {
                    None
                };
                let output_texture = if let Some(texture) = output_texture {
                    texture
                } else {
                    if let Some(error) = gpu_surface_error {
                        messages::warning(
                            "Video worker",
                            format!("falling back to CPU preview transfer: {error}"),
                        );
                    }
                    let Some(slot) = snapshot
                        .cpu_slots
                        .get(usize::try_from(
                            generation
                                .checked_rem(u64::try_from(cpu_surface::SLOT_COUNT)?)
                                .context("CPU surface slot count is zero")?,
                        )?)
                        .cloned()
                    else {
                        write_response(&WireResponse::Error {
                            message: "CPU preview slots are unavailable".into(),
                        })?;
                        continue;
                    };
                    slot.validate()?;
                    anyhow::ensure!(
                        slot.width == dimensions[0] && slot.height == dimensions[1],
                        "CPU preview slot dimensions do not match the frame"
                    );
                    cpu_slot = Some(slot);
                    device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("Kama private CPU-transfer preview target"),
                        size: wgpu::Extent3d {
                            width: dimensions[0],
                            height: dimensions[1],
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::Rgba16Float,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING
                            | wgpu::TextureUsages::STORAGE_BINDING
                            | wgpu::TextureUsages::COPY_SRC,
                        view_formats: &[],
                    })
                };
                let output = PresentationTexture::from_texture(
                    output_texture.clone(),
                    dimensions[0],
                    dimensions[1],
                );
                let render_result = frame_renderer.refresh_preview_on(
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
                    captured_frame
                        .as_ref()
                        .map(|((capture_width, capture_height), pixels)| {
                            ([*capture_width, *capture_height], pixels.as_slice())
                        }),
                    &output,
                );
                let has_rendered = match render_result {
                    Ok(has_rendered) => has_rendered,
                    Err(error) => {
                        if let Some(channel) = channel {
                            free_surfaces.push_back(channel);
                        }
                        write_response(&WireResponse::Error {
                            message: format!("render preview: {error:#}"),
                        })?;
                        continue;
                    }
                };
                if !has_rendered {
                    if let Some(channel) = channel {
                        free_surfaces.push_back(channel);
                    }
                    write_response(&WireResponse::Error {
                        message: "preview source is not ready".into(),
                    })?;
                    continue;
                }
                let descriptor = if let Some(channel) = channel {
                    match surfaces
                        .get_mut(&channel)
                        .context("rendered GPU surface disappeared")?
                        .publish(&device, &queue)
                    {
                        Ok(descriptor) => descriptor,
                        Err(error) => {
                            surfaces.remove(&channel);
                            write_response(&WireResponse::Error {
                                message: format!(
                                    "import video frame: publish GPU surface failed: {error:#}"
                                ),
                            })?;
                            continue;
                        }
                    }
                } else {
                    let slot = cpu_slot.context("CPU preview slot disappeared")?;
                    let pixels = cpu_surface::readback_rgba16(
                        &device,
                        &queue,
                        &output_texture,
                        dimensions[0],
                        dimensions[1],
                    )?;
                    cpu_surface::write_frame(&cpu_session, &slot, &pixels, std::process::id())?;
                    FrameSurfaceDescriptor::Cpu(slot)
                };
                let source_geometry = frame_renderer
                    .source_geometry()
                    .iter()
                    .map(|(id, geometry)| (id.to_string(), *geometry))
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
                    pending: frame_renderer.is_waiting_for_video(),
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
                cached_snapshot = None;
                effects_revision = None;
                frame_renderer.clear_caches();
            }
            Request::Shutdown => break,
            #[cfg(debug_assertions)]
            Request::Crash => std::process::abort(),
            Request::Hello { .. } => write_response(&WireResponse::Error {
                message: "duplicate video handshake".into(),
            })?,
            Request::RenderCached { .. } => anyhow::bail!("unresolved cached video frame request"),
        }
    }
    drop(surfaces);
    drop((adapter, instance));
    Ok(())
}

fn create_matching_gpu_device(
    expected: &gpu_surface::GpuIdentity,
) -> Result<(wgpu::Instance, wgpu::Adapter, wgpu::Device, wgpu::Queue)> {
    let backend = expected.backend();
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = backend;
    let instance = wgpu::Instance::new(descriptor);
    for adapter in pollster::block_on(instance.enumerate_adapters(backend)) {
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
            label: Some("kama isolated shared-GPU video device"),
            required_features: features,
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
        };
        let Ok((device, queue)) = pollster::block_on(adapter.request_device(&descriptor)) else {
            continue;
        };
        if gpu_surface::identity(&device).as_ref().ok() == Some(expected) {
            return Ok((instance, adapter, device, queue));
        }
    }
    anyhow::bail!("no sharing-capable adapter matches the editor GPU")
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

    use super::{FrameSurfaceDescriptor, SourceGeometry, WireResponse};

    #[test]
    fn frame_transport_reuses_project_until_explicit_invalidation() {
        use super::{Request, Snapshot};
        use crate::{
            project::{Project, WorkerProjectState},
            timeline::TimelineState,
        };

        let mut project = Project::new();
        project.name = "Retained processing document".into();
        let snapshot = |playhead| Snapshot {
            worker_state: WorkerProjectState::capture(&project),
            project: project.clone(),
            timeline: TimelineState::default().document(),
            revision: 41,
            seek_revision: 3,
            playhead,
            playing: true,
            scrubbing: false,
            preview_size: [640, 360],
            render_scale: 0.5,
            render_cache: None,
            captured_frame: None,
            cpu_slots: Vec::new(),
            force_cpu: false,
        };
        let (tx, rx) = std::sync::mpsc::sync_channel(4);
        tx.send(Request::Render {
            generation: 1,
            snapshot: Box::new(snapshot(1.25)),
        })
        .unwrap();
        tx.send(Request::Render {
            generation: 1,
            snapshot: Box::new(snapshot(1.5)),
        })
        .unwrap();
        tx.send(Request::Invalidate).unwrap();
        tx.send(Request::Render {
            generation: 2,
            snapshot: Box::new(snapshot(2.0)),
        })
        .unwrap();
        drop(tx);
        let mut output = Vec::new();
        super::write_requests(&mut output, rx).unwrap();
        let mut requests = output
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice::<Request>(line).unwrap());
        match requests.next().unwrap() {
            Request::Render { snapshot, .. } => {
                assert_eq!(snapshot.project.name, "Retained processing document");
            }
            _ => panic!("first frame must transfer its processing document"),
        }
        match requests.next().unwrap() {
            Request::RenderCached {
                revision,
                transport,
                ..
            } => {
                assert_eq!(revision, 41);
                assert_eq!(transport.playhead, 1.5);
                assert_eq!(transport.preview_size, [640, 360]);
            }
            _ => panic!("advancing a frame must not retransmit the processing document"),
        }
        assert!(matches!(requests.next(), Some(Request::Invalidate)));
        match requests.next().unwrap() {
            Request::Render { snapshot, .. } => assert_eq!(snapshot.playhead, 2.0),
            _ => panic!(
                "invalidation must refresh the processing document even at the same revision"
            ),
        }
        assert!(requests.next().is_none());
    }

    #[test]
    fn frame_response_round_trips_numeric_source_ids() {
        let response = WireResponse::Frame {
            generation: 1,
            revision: 2,
            seek_revision: 3,
            playhead: 1.5,
            pending: true,
            descriptor: FrameSurfaceDescriptor::Metal(
                crate::runtime::shared_surface::SharedSurfaceDescriptor {
                    channel: "test".into(),
                    session_token: [0; 32],
                    width: 2,
                    height: 2,
                    pixel_format: 0,
                    metal_registry_id: 0,
                    importer_pid: 1,
                    exporter_pid: 2,
                },
            ),
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
    fn cached_video_readiness_is_invalidated_by_seek_and_project_revision() {
        let ready = Some((14, true, false, 27));
        assert!(super::transport_readiness_matches(
            ready,
            (14, true, false, 27)
        ));
        assert!(!super::transport_readiness_matches(
            ready,
            (15, true, false, 27)
        ));
        assert!(!super::transport_readiness_matches(
            ready,
            (14, true, false, 28)
        ));
    }

    #[test]
    fn video_worker_child_entry() {
        if std::env::var_os("KAMA_VIDEO_WORKER_TEST_CHILD").is_some() {
            super::run_video_worker().expect("video worker child protocol exits cleanly");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn forced_cpu_surface_worker_transfers_distinct_cross_process_frames() {
        use std::{sync::Arc, time::Duration};

        use super::{Process, Request, Response, Snapshot};
        use crate::{
            project::{Project, ProjectBackground, WorkerProjectState},
            runtime::shared_surface,
            timeline::TimelineState,
        };

        let mut instance_descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        instance_descriptor.backends = wgpu::Backends::METAL;
        let instance = wgpu::Instance::new(instance_descriptor);
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .expect("Metal adapter available for CPU transfer fallback");
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .expect("request parent GPU for CPU upload");
        let device = Arc::new(device);
        let queue = Arc::new(queue);
        let registry_id = shared_surface::metal_registry_id(&device).expect("Metal device ID");
        let process = Process::spawn(
            "",
            i32::try_from(std::process::id()).unwrap(),
            Some(registry_id),
            &device,
            &queue,
            true,
        )
        .expect("spawn forced-CPU video worker subprocess");
        assert!(matches!(
            process.responses.recv_timeout(Duration::from_secs(15)),
            Ok(Response::Ready { pid }) if pid == process.pid
        ));

        let cpu_slots = process.cpu_surfaces.prepare([4, 4]).unwrap();
        let timeline = TimelineState::default();
        let mut project = Project::new();
        project.active_composition_mut().settings.canvas_size = [4, 4];
        project.active_composition_mut().settings.background = ProjectBackground::Solid {
            color: [0.9, 0.1, 0.2, 1.0],
        };
        let snapshot =
            |revision, project: &Project, seek_revision, preview_size, cpu_slots| Snapshot {
                worker_state: WorkerProjectState::capture(project),
                project: project.clone(),
                timeline: timeline.document(),
                revision,
                seek_revision,
                playhead: 0.0,
                playing: false,
                scrubbing: false,
                preview_size,
                render_scale: 1.0,
                render_cache: None,
                captured_frame: None,
                cpu_slots,
                force_cpu: true,
            };
        process
            .requests
            .send(Request::Render {
                generation: 1,
                snapshot: Box::new(snapshot(1, &project, 1, [4, 4], cpu_slots)),
            })
            .unwrap();
        let first_texture = match process
            .responses
            .recv_timeout(Duration::from_secs(20))
            .unwrap()
        {
            Response::Frame {
                channel: None,
                texture,
                ..
            } => texture,
            Response::Frame {
                channel: Some(channel),
                ..
            } => panic!("forced CPU frame used GPU surface {channel}"),
            Response::Error(error) => panic!("CPU preview failed: {error}"),
            _ => panic!("expected CPU frame response"),
        };
        let first_pixels =
            crate::runtime::cpu_surface::readback_rgba16(&device, &queue, &first_texture, 4, 4)
                .unwrap();
        drop(first_texture);

        project.active_composition_mut().settings.background = ProjectBackground::Solid {
            color: [0.25, 0.5, 0.75, 1.0],
        };
        let resized_slots = process.cpu_surfaces.prepare([8, 4]).unwrap();
        process
            .requests
            .send(Request::Render {
                generation: 2,
                snapshot: Box::new(snapshot(2, &project, 2, [8, 4], resized_slots)),
            })
            .unwrap();
        let second_texture = match process
            .responses
            .recv_timeout(Duration::from_secs(20))
            .unwrap()
        {
            Response::Frame {
                channel: None,
                texture,
                ..
            } => texture,
            Response::Frame {
                channel: Some(channel),
                ..
            } => panic!("CPU fallback switched to GPU surface {channel}"),
            Response::Error(error) => panic!("second CPU preview failed: {error}"),
            _ => panic!("expected second CPU frame response"),
        };
        let second_pixels =
            crate::runtime::cpu_surface::readback_rgba16(&device, &queue, &second_texture, 8, 4)
                .unwrap();
        let expected_blue = [0x00, 0x34, 0x00, 0x38, 0x00, 0x3a, 0x00, 0x3c];
        assert!(
            first_pixels
                .as_chunks::<8>()
                .0
                .iter()
                .all(|pixel| pixel != &expected_blue)
        );
        assert!(
            second_pixels
                .as_chunks::<8>()
                .0
                .iter()
                .all(|pixel| pixel == &expected_blue)
        );

        drop(second_texture);
        let _ = process.requests.try_send(Request::Shutdown);
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
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
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
        let device = Arc::new(device);
        let queue = Arc::new(queue);
        let registry_id = shared_surface::metal_registry_id(&device).expect("Metal device ID");
        let process = Process::spawn(
            &plugin_paths,
            i32::try_from(std::process::id()).unwrap(),
            Some(registry_id),
            &device,
            &queue,
            false,
        )
        .expect("spawn production video worker");
        let startup_deadline = super::deadline_after(Duration::from_secs(20));
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
                cpu_slots: Vec::new(),
                force_cpu: false,
            };
            let mut request = Request::Render {
                generation,
                snapshot: Box::new(snapshot),
            };
            if generation == 1 {
                let encoded = serde_json::to_vec(&request).expect("serialize production request");
                request = serde_json::from_slice(&encoded).expect("decode production request");
                let Request::Render { snapshot, .. } = &request else {
                    panic!("render request must retain its snapshot");
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
                        if let Some(channel) = channel {
                            let _ = process.requests.try_send(Request::Release { channel });
                        }
                    }
                    Ok(Response::Error(error)) if error == "preview source is not ready" => {
                        not_ready_count += 1;
                        if not_ready_count == 1 || not_ready_count.is_multiple_of(10) {
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
                    Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() < deadline => {}
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        let sample_path = "/private/tmp/opencode/pumpkin-video-test-child.sample";
                        let parent_sample_path =
                            "/private/tmp/opencode/pumpkin-video-test-parent.sample";
                        let sample = Command::new("sample")
                            .args([&process.pid.to_string(), "1", "-file", sample_path])
                            .output()
                            .map_or_else(
                                |error| format!("sample failed: {error}"),
                                |_| format!("sample saved to {sample_path}"),
                            );
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
                    .map_or_else(
                        |error| format!("sample failed: {error}"),
                        |_| format!("sample saved to {sample_path}"),
                    );
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
        if let Some(channel) = initial_channel {
            process
                .requests
                .try_send(Request::Release { channel })
                .expect("release initial reproduction frame");
        }
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
                cpu_slots: Vec::new(),
                force_cpu: false,
            };
            process
                .requests
                .send(Request::Render {
                    generation,
                    snapshot: Box::new(snapshot),
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
                        if let Some(channel) = channel {
                            process
                                .requests
                                .send(Request::Release { channel })
                                .expect("release scrub frame");
                        }
                        break;
                    }
                    Ok(Response::Frame {
                        channel, texture, ..
                    }) => {
                        drop(texture);
                        if let Some(channel) = channel {
                            let _ = process.requests.try_send(Request::Release { channel });
                        }
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
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
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
                .as_chunks::<8>()
                .0
                .iter()
                .any(|pixel| pixel.iter().take(6).any(|byte| *byte != 0)),
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
            cpu_slots: Vec::new(),
            force_cpu: false,
        };
        let mut playback = VideoPlayback::unavailable("", VideoWorkerStatus::StartupFailed);
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
        let mut playback = VideoPlayback::unavailable("", VideoWorkerStatus::StartupFailed);
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
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .expect("Metal adapter available for subprocess integration test");
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .expect("request parent Metal device");
        let device = Arc::new(device);
        let queue = Arc::new(queue);
        let registry_id = shared_surface::metal_registry_id(&device).expect("Metal registry ID");
        let process = Process::spawn(
            "",
            i32::try_from(std::process::id()).unwrap(),
            Some(registry_id),
            &device,
            &queue,
            false,
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
                snapshot: Box::new(Snapshot {
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
                    cpu_slots: Vec::new(),
                    force_cpu: false,
                }),
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
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
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
        for row in mapped.as_chunks::<256>().0.iter().take(4) {
            for pixel in row.as_chunks::<8>().0.iter().take(4) {
                assert_eq!(pixel, &expected);
            }
        }
        drop(mapped);
        drop(texture);
        if let Some(channel) = channel {
            let _ = process.requests.try_send(Request::Release { channel });
        }

        process
            .requests
            .try_send(Request::Render {
                generation: 2,
                snapshot: Box::new(Snapshot {
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
                    cpu_slots: Vec::new(),
                    force_cpu: false,
                }),
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
        if let Some(channel) = resized_channel.as_ref() {
            let _ = process.requests.try_send(Request::Release {
                channel: channel.clone(),
            });
        }
        process
            .requests
            .try_send(Request::Render {
                generation: 2,
                snapshot: Box::new(Snapshot {
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
                    cpu_slots: Vec::new(),
                    force_cpu: false,
                }),
            })
            .expect("render next playback frame into a released surface");
        match process.responses.recv_timeout(Duration::from_secs(20)) {
            Ok(Response::Frame {
                channel, playhead, ..
            }) => {
                assert_eq!(channel, resized_channel, "reuse the released GPU surface");
                assert_eq!(playhead, 0.5);
                if let Some(channel) = channel {
                    process
                        .requests
                        .try_send(Request::Release { channel })
                        .unwrap();
                }
            }
            Ok(Response::Error(error)) => panic!("next playback frame failed: {error}"),
            _ => panic!("expected next playback frame"),
        }

        let deadline = super::deadline_after(Duration::from_secs(3));
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
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .unwrap();
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
        let device = Arc::new(device);
        let queue = Arc::new(queue);
        let mut playback = super::VideoPlayback::new("", &device, &queue);
        let original = playback
            .pid()
            .expect("worker has a PID immediately after spawning");

        playback.fail(super::VideoWorkerStatus::Unresponsive);
        assert_eq!(playback.status(), super::VideoWorkerStatus::Unresponsive);
        assert_eq!(playback.pid(), Some(original));
        assert!(playback.process.is_none());

        playback.restart(&device, &queue);
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
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .expect("Metal adapter available for subprocess crash test");
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .expect("request parent Metal device");
        let device = Arc::new(device);
        let queue = Arc::new(queue);
        let registry_id = shared_surface::metal_registry_id(&device).expect("Metal registry ID");
        let mut process = Process::spawn(
            "",
            i32::try_from(std::process::id()).unwrap(),
            Some(registry_id),
            &device,
            &queue,
            false,
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

        let deadline = super::deadline_after(Duration::from_secs(5));
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
