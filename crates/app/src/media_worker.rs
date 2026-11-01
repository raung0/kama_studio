use std::{
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{project::MediaAsset, runtime::media::AvMediaProbe};
use kama_editor_core::document::WaveformData;

const MAX_MESSAGE_BYTES: u64 = 64 * 1024 * 1024;
static PROBE_JOB: Mutex<Option<Session>> = Mutex::new(None);
static ANALYSIS_JOB: Mutex<Option<Session>> = Mutex::new(None);
static THUMBNAIL_JOB: Mutex<Option<Session>> = Mutex::new(None);
static CAPTURE_JOB: Mutex<Option<Session>> = Mutex::new(None);

fn rgba16_len(size: [u32; 2]) -> Result<u64> {
    u64::from(size[0])
        .checked_mul(u64::from(size[1]))
        .and_then(|pixels| pixels.checked_mul(8))
        .context("RGBA16 image size overflow")
}

pub struct CapturedFrame {
    pub size: [u32; 2],
    pub pixels: Vec<u8>,
    pub saved_path: Option<PathBuf>,
}

#[derive(Serialize, Deserialize)]
enum Request {
    Capture {
        project: crate::project::Project,
        worker_state: crate::project::WorkerProjectState,
        timeline: crate::timeline::TimelineDocument,
        playhead: f32,
        plugin_paths: String,
        output: PathBuf,
        saved_path: Option<PathBuf>,
    },
    Probe(PathBuf),
    Analyze {
        asset: MediaAsset,
        path: PathBuf,
    },
    Thumbnail {
        path: PathBuf,
        time: f64,
        fps: f64,
        size: [u32; 2],
    },
    #[cfg(all(test, debug_assertions))]
    Crash,
    #[cfg(all(test, debug_assertions))]
    Hang,
}

#[derive(Serialize, Deserialize)]
enum Response {
    Captured {
        size: [u32; 2],
        saved_path: Option<PathBuf>,
    },
    Probe(AvMediaProbe),
    Analysis(Option<WaveformData>),
    Thumbnail(Option<ThumbnailFrame>),
}

#[derive(Serialize, Deserialize)]
struct ThumbnailFrame {
    width: u32,
    height: u32,
    source_size: [u32; 2],
    fit_size: [u32; 2],
    pixels: Vec<u8>,
    transfer: u32,
    bt2020_primaries: bool,
}

impl ThumbnailFrame {
    fn from_frame(frame: &crate::runtime::video::VideoFrame) -> Result<Self> {
        let crate::runtime::video::VideoFramePixels::Rgba16(pixels) = &frame.pixels else {
            anyhow::bail!("thumbnail decoder did not return CPU pixels");
        };
        Ok(Self {
            width: frame.width,
            height: frame.height,
            source_size: [frame.source_width, frame.source_height],
            fit_size: [frame.fit_width, frame.fit_height],
            pixels: pixels.clone(),
            transfer: frame.transfer,
            bt2020_primaries: frame.bt2020_primaries,
        })
    }

    fn into_frame(self) -> Result<crate::runtime::video::VideoFrame> {
        let length = rgba16_len(self.fit_size)?;
        anyhow::ensure!(
            length / 8 <= 1024 * 1024 && u64::try_from(self.pixels.len())? == length,
            "invalid thumbnail pixel dimensions"
        );
        let mut frame = crate::runtime::video::VideoFrame::from_rgba16(
            self.width,
            self.height,
            self.fit_size[0],
            self.fit_size[1],
            self.pixels,
            self.transfer,
            self.bt2020_primaries,
        );
        frame.source_width = self.source_size[0];
        frame.source_height = self.source_size[1];
        Ok(frame)
    }
}

#[derive(Serialize, Deserialize)]
struct Reply {
    pid: u32,
    result: std::result::Result<Response, String>,
}

struct Process(Option<Child>);

struct Cleanup {
    path: PathBuf,
    keep: bool,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
}

pub fn probe(path: &Path) -> Result<AvMediaProbe> {
    match execute(Request::Probe(path.to_owned()), Duration::from_secs(20))? {
        Response::Probe(probe) => Ok(probe),
        _ => anyhow::bail!("unexpected media probe response"),
    }
}

pub fn analyze(asset: &MediaAsset) -> Result<Option<WaveformData>> {
    let mut snapshot = asset.clone();
    snapshot.waveform = None;
    match execute(
        Request::Analyze {
            asset: snapshot,
            path: asset.path.clone(),
        },
        Duration::from_secs(300),
    )? {
        Response::Analysis(waveform) => Ok(waveform),
        _ => anyhow::bail!("unexpected media analysis response"),
    }
}

pub fn thumbnail(
    path: &Path,
    time: f64,
    fps: f64,
    size: [u32; 2],
) -> Result<Option<Arc<crate::runtime::video::VideoFrame>>> {
    match execute(
        Request::Thumbnail {
            path: path.to_owned(),
            time,
            fps,
            size,
        },
        Duration::from_secs(20),
    )? {
        Response::Thumbnail(frame) => frame
            .map(ThumbnailFrame::into_frame)
            .transpose()
            .map(|frame| frame.map(Arc::new)),
        _ => anyhow::bail!("unexpected thumbnail response"),
    }
}

fn execute(request: Request, timeout: Duration) -> Result<Response> {
    let pool = match &request {
        Request::Probe(_) => &PROBE_JOB,
        Request::Analyze { .. } => &ANALYSIS_JOB,
        Request::Thumbnail { .. } => &THUMBNAIL_JOB,
        Request::Capture { .. } => &CAPTURE_JOB,
        #[cfg(all(test, debug_assertions))]
        Request::Crash | Request::Hang => &PROBE_JOB,
    };
    let mut session = if matches!(&request, Request::Capture { .. }) {
        match pool.try_lock() {
            Ok(session) => session,
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                anyhow::bail!("a capture worker job is already running")
            }
        }
    } else {
        pool.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    if session.is_none() {
        *session = Some(Session::spawn()?);
    }
    let result = session
        .as_mut()
        .context("media session unavailable")?
        .execute(request, timeout);
    if result.is_err() {
        session.take();
    }
    result
}

pub fn capture(
    project: crate::project::Project,
    timeline: crate::timeline::TimelineDocument,
    playhead: f32,
    plugin_paths: String,
    saved_path: Option<PathBuf>,
) -> Result<CapturedFrame> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let output =
        std::env::temp_dir().join(format!("kama-capture-{}-{stamp}.raw", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&output)?;
    let _cleanup = Cleanup {
        path: output.clone(),
        keep: false,
    };
    drop(file);
    let mut saved_cleanup = if let Some(path) = &saved_path {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .with_context(|| format!("reserve capture {}", path.display()))?;
        Some(Cleanup {
            path: path.clone(),
            keep: false,
        })
    } else {
        None
    };
    let worker_state = crate::project::WorkerProjectState::capture(&project);
    let expected_size = project
        .active_settings()
        .canvas_size
        .map(|value| value.max(1));
    let response = execute(
        Request::Capture {
            project,
            worker_state,
            timeline,
            playhead,
            plugin_paths,
            output: output.clone(),
            saved_path: saved_path.clone(),
        },
        Duration::from_secs(60),
    )?;
    let Response::Captured {
        size,
        saved_path: returned_path,
    } = response
    else {
        anyhow::bail!("unexpected frame capture response");
    };
    anyhow::ensure!(
        size == expected_size && returned_path == saved_path,
        "frame capture response differs from request"
    );
    let expected = rgba16_len(size)?;
    anyhow::ensure!(
        expected <= 256 * 1024 * 1024 && std::fs::metadata(&output)?.len() == expected,
        "invalid captured frame size"
    );
    let pixels = std::fs::read(&output)?;
    if let Some(cleanup) = &mut saved_cleanup {
        cleanup.keep = true;
    }
    Ok(CapturedFrame {
        size,
        pixels,
        saved_path,
    })
}

struct Session {
    process: Process,
    requests: mpsc::SyncSender<Request>,
    responses: mpsc::Receiver<Result<Reply>>,
    diagnostics: Arc<Mutex<Vec<u8>>>,
    pid: u32,
}

impl Session {
    fn spawn() -> Result<Self> {
        let mut command =
            Command::new(std::env::current_exe().context("locate media worker executable")?);
        #[cfg(not(test))]
        command.arg("--kama-worker=media");
        #[cfg(test)]
        command
            .args([
                "--exact",
                "media_worker::tests::media_worker_child_entry",
                "--quiet",
                "--nocapture",
            ])
            .env("KAMA_MEDIA_WORKER_TEST_CHILD", "1");
        let mut process = Process(Some(
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .context("start media worker")?,
        ));
        let child = process
            .0
            .as_mut()
            .context("media worker process unavailable")?;
        let pid = child.id();
        let mut input = child
            .stdin
            .take()
            .context("media worker stdin unavailable")?;
        let output = child
            .stdout
            .take()
            .context("media worker stdout unavailable")?;
        let mut errors = child
            .stderr
            .take()
            .context("media worker stderr unavailable")?;
        let diagnostics = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&diagnostics);
        thread::spawn(move || {
            let mut buffer = [0; 4096];
            while let Ok(count) = errors.read(&mut buffer) {
                if count == 0 {
                    break;
                }
                if let Ok(mut captured) = captured.lock() {
                    let Some(chunk) = buffer.get(..count) else {
                        break;
                    };
                    captured.extend_from_slice(chunk);
                    if captured.len() > 32_768 {
                        let excess = captured.len().saturating_sub(32_768);
                        captured.drain(..excess);
                    }
                }
            }
        });
        let (tx, rx) = mpsc::sync_channel(1);
        let (request_tx, request_rx) = mpsc::sync_channel::<Request>(1);
        thread::spawn(move || {
            let mut reader = BufReader::new(output);
            for request in request_rx {
                let result = (|| -> Result<Reply> {
                    serde_json::to_writer(&mut input, &request)?;
                    input.write_all(b"\n")?;
                    input.flush()?;
                    #[cfg(test)]
                    let line = loop {
                        let line = read_message(&mut reader)?;
                        if line.trim().is_empty()
                            || line.starts_with("running ")
                            || line.starts_with("test ")
                        {
                            continue;
                        }
                        break line;
                    };
                    #[cfg(not(test))]
                    let line = read_message(&mut reader)?;
                    serde_json::from_str(&line).context("decode media worker response")
                })();
                let failed = result.is_err();
                if tx.send(result).is_err() || failed {
                    break;
                }
            }
        });
        Ok(Self {
            process,
            requests: request_tx,
            responses: rx,
            diagnostics,
            pid,
        })
    }

    fn execute(&mut self, request: Request, timeout: Duration) -> Result<Response> {
        self.requests
            .try_send(request)
            .map_err(|_| anyhow::anyhow!("media request channel is unavailable"))?;
        let result = self
            .responses
            .recv_timeout(timeout)
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => anyhow::anyhow!(
                    "media worker unresponsive for {} seconds",
                    timeout.as_secs_f64()
                ),
                mpsc::RecvTimeoutError::Disconnected => {
                    anyhow::anyhow!("media worker response stream disconnected")
                }
            })
            .and_then(|reply| reply)
            .and_then(|reply| {
                anyhow::ensure!(reply.pid == self.pid, "media worker PID mismatch");
                reply.result.map_err(anyhow::Error::msg)
            });
        result
            .with_context(|| {
                let state = match self.process.0.as_mut().map(Child::try_wait) {
                    Some(Ok(Some(status))) => format!("exited with {status}"),
                    Some(Err(error)) => format!("exit status unavailable: {error}"),
                    _ => "response failed or process is unresponsive".to_owned(),
                };
                let errors = self
                    .diagnostics
                    .lock()
                    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                    .unwrap_or_default();
                format!("media process {} ({state}): {errors}", self.pid)
            })
            .inspect_err(|error| crate::messages::error("Media worker", format!("{error:#}")))
    }
}

fn read_message(reader: &mut impl BufRead) -> Result<String> {
    let mut bytes = Vec::new();
    let count = reader
        .take(MAX_MESSAGE_BYTES + 1)
        .read_until(b'\n', &mut bytes)?;
    anyhow::ensure!(count > 0, "media worker closed its response stream");
    anyhow::ensure!(
        u64::try_from(count)? <= MAX_MESSAGE_BYTES,
        "media worker message exceeds size limit"
    );
    String::from_utf8(bytes).context("media worker message is not UTF-8")
}

#[expect(
    clippy::significant_drop_tightening,
    reason = "the stdin lock must stay held for the worker request loop"
)]
pub fn run() -> Result<()> {
    let input = std::io::stdin();
    let mut input = input.lock();
    let mut thumbnail_decoder = None;
    loop {
        if input.fill_buf()?.is_empty() {
            return Ok(());
        }
        let request = serde_json::from_str(&read_message(&mut input)?)
            .context("decode media worker request")?;
        let result = match request {
            Request::Capture {
                mut project,
                worker_state,
                timeline,
                playhead,
                plugin_paths,
                output,
                saved_path,
            } => (|| -> Result<Response> {
                worker_state.restore(&mut project)?;
                let size = project
                    .active_settings()
                    .canvas_size
                    .map(|value| value.max(1));
                let expected = rgba16_len(size)?;
                anyhow::ensure!(
                    playhead.is_finite() && expected <= 256 * 1024 * 1024,
                    "capture exceeds the frame size limit"
                );
                let pixels = crate::render::capture_processing_frame(
                    &project,
                    timeline,
                    playhead,
                    &plugin_paths,
                )?;
                anyhow::ensure!(
                    u64::try_from(pixels.len())? == expected,
                    "capture renderer returned an invalid frame size"
                );
                let mut raw = std::fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(output)?;
                raw.write_all(&pixels)?;
                if let Some(path) = &saved_path {
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    let (pixel_samples, _) = pixels.as_chunks::<2>();
                    let samples = pixel_samples
                        .iter()
                        .map(|bytes| u16::from_le_bytes(*bytes))
                        .collect::<Vec<_>>();
                    let image = image::ImageBuffer::<image::Rgba<u16>, Vec<u16>>::from_raw(
                        size[0], size[1], samples,
                    )
                    .context("invalid captured image dimensions")?;
                    let file = std::fs::OpenOptions::new()
                        .write(true)
                        .truncate(true)
                        .open(path)?;
                    let mut writer = std::io::BufWriter::new(file);
                    let result = image::DynamicImage::ImageRgba16(image)
                        .write_to(&mut writer, image::ImageFormat::Png)
                        .and_then(|()| writer.flush().map_err(image::ImageError::IoError));
                    if let Err(error) = result {
                        drop(writer);
                        let _ = std::fs::remove_file(path);
                        return Err(error.into());
                    }
                }
                Ok(Response::Captured { size, saved_path })
            })(),
            Request::Probe(path) => {
                crate::runtime::media::probe_av_media_local(&path).map(Response::Probe)
            }
            Request::Analyze { mut asset, path } => {
                asset.path = path;
                Ok(Response::Analysis(crate::waveform::analyze_media_local(
                    &asset,
                )))
            }
            Request::Thumbnail {
                path,
                time,
                fps,
                size,
            } => {
                anyhow::ensure!(
                    time.is_finite()
                        && fps.is_finite()
                        && size.into_iter().all(|value| (1..=1024).contains(&value)),
                    "invalid thumbnail request"
                );
                if thumbnail_decoder
                    .as_ref()
                    .is_none_or(|(current, _)| current != &path)
                {
                    thumbnail_decoder = Some((
                        path.clone(),
                        crate::runtime::media::ThumbnailDecoder::new(&path),
                    ));
                }
                thumbnail_decoder
                    .as_mut()
                    .context("thumbnail decoder unavailable")?
                    .1
                    .decode(time, fps, size)
                    .and_then(|frame| frame.as_deref().map(ThumbnailFrame::from_frame).transpose())
                    .map(Response::Thumbnail)
            }
            #[cfg(all(test, debug_assertions))]
            Request::Crash => std::process::abort(),
            #[cfg(all(test, debug_assertions))]
            Request::Hang => loop {
                thread::park();
            },
        };
        let reply = Reply {
            pid: std::process::id(),
            result: result.map_err(|error| format!("{error:#}")),
        };
        let mut output = std::io::stdout().lock();
        serde_json::to_writer(&mut output, &reply)?;
        output.write_all(b"\n")?;
        output.flush()?;
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    #[test]
    fn capture_never_overwrites_an_existing_destination() {
        let output =
            std::env::temp_dir().join(format!("kama-capture-collision-{}.png", std::process::id()));
        std::fs::write(&output, b"existing capture").unwrap();
        let project = crate::project::Project::new();
        let timeline = project.active_composition().timeline.clone();

        let result = super::capture(project, timeline, 0.0, String::new(), Some(output.clone()));

        assert!(result.is_err());
        assert_eq!(std::fs::read(&output).unwrap(), b"existing capture");
        std::fs::remove_file(output).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn capture_child_renders_requested_video_frame_and_saves_matching_png() {
        use crate::{effects::PipelineInstance, project::Project, timeline::TimelineState};
        let root = std::env::temp_dir().join(format!("kama-capture-job-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("red-then-blue.y4m");
        let output = root.join("captured-blue.png");
        let mut file = std::fs::File::create(&source).unwrap();
        file.write_all(b"YUV4MPEG2 W16 H16 F2:1 Ip A1:1 C420mpeg2 XCOLORRANGE=LIMITED\n")
            .unwrap();
        for (y, cb, cr) in [(81, 90, 240), (41, 240, 110)] {
            file.write_all(b"FRAME\n").unwrap();
            file.write_all(&[y; 256]).unwrap();
            file.write_all(&[cb; 64]).unwrap();
            file.write_all(&[cr; 64]).unwrap();
        }
        drop(file);
        let mut project = Project::new();
        project.active_composition_mut().settings.canvas_size = [16, 16];
        project.active_composition_mut().settings.frame_rate = 2.0;
        let media = project.import_media(&source).unwrap();
        let mut timeline =
            TimelineState::from_document(project.active_composition().timeline.clone());
        let track = timeline.tracks()[0].id;
        assert!(timeline.insert_media_clip_at(
            (track, 0.0),
            media,
            "video".into(),
            false,
            Some(1.0),
            PipelineInstance::effect_default()
        ));
        let captured = super::capture(
            project,
            timeline.document(),
            0.5,
            crate::preferences::load_plugin_paths(),
            Some(output.clone()),
        )
        .unwrap();
        assert_eq!(captured.size, [16, 16]);
        assert_eq!(captured.saved_path.as_ref(), Some(&output));
        let saved = image::open(&output).unwrap().to_rgba16();
        assert_eq!(saved.dimensions(), (16, 16));
        let (raw_pixels, _) = captured.pixels.as_chunks::<8>();
        for (raw, pixel) in raw_pixels.iter().zip(saved.pixels()) {
            let (channels, _) = raw.as_chunks::<2>();
            let values = channels
                .iter()
                .map(|bytes| u16::from_le_bytes(*bytes))
                .collect::<Vec<_>>();
            assert!(
                values
                    .get(2)
                    .zip(values.first())
                    .is_some_and(|(blue, red)| i32::from(*blue) - i32::from(*red) > 20_000),
                "capture at 0.5 seconds must be blue: {values:?}"
            );
            assert_eq!(values.as_slice(), &pixel.0);
        }
        std::fs::remove_file(source).unwrap();
        std::fs::remove_file(output).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[cfg(debug_assertions)]
    #[test]
    fn hung_media_job_reports_unresponsiveness_without_waiting_for_completion() {
        let mut session = super::Session::spawn().unwrap();
        let Err(error) = session.execute(super::Request::Hang, std::time::Duration::from_secs(1))
        else {
            panic!("hung child must reach the response deadline");
        };
        let diagnostic = format!("{error:#}");
        assert!(diagnostic.contains("media process"));
        assert!(diagnostic.contains("unresponsive for 1 seconds"));
    }

    #[test]
    fn child_thumbnail_returns_cpu_pixels_with_source_dimensions() {
        let path =
            std::env::temp_dir().join(format!("kama-thumbnail-job-{}.y4m", std::process::id()));
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(b"YUV4MPEG2 W16 H16 F24:1 Ip A1:1 C420mpeg2 XCOLORRANGE=LIMITED\nFRAME\n")
            .unwrap();
        file.write_all(&[235; 256]).unwrap();
        file.write_all(&[128; 128]).unwrap();
        file.write_all(b"FRAME\n").unwrap();
        file.write_all(&[16; 256]).unwrap();
        file.write_all(&[128; 128]).unwrap();
        drop(file);
        let frame = super::thumbnail(&path, 0.0, 24.0, [16, 16])
            .unwrap()
            .unwrap();
        assert_eq!([frame.source_width, frame.source_height], [16, 16]);
        assert_eq!([frame.fit_width, frame.fit_height], [16, 16]);
        let crate::runtime::video::VideoFramePixels::Rgba16(pixels) = &frame.pixels else {
            panic!("native FFmpeg frame ownership must not cross into the UI process");
        };
        assert_eq!(pixels.len(), 16 * 16 * 8);
        for channel in pixels.as_chunks::<2>().0 {
            assert!(u16::from_le_bytes(*channel) > 50_000);
        }
        let next = super::thumbnail(&path, 1.0 / 24.0, 24.0, [16, 16])
            .unwrap()
            .unwrap();
        let crate::runtime::video::VideoFramePixels::Rgba16(pixels) = &next.pixels else {
            panic!("second thumbnail must also contain independent CPU pixels");
        };
        for pixel in pixels.as_chunks::<8>().0 {
            for channel in pixel.as_chunks::<2>().0.iter().take(3) {
                assert!(u16::from_le_bytes(*channel) < 1_000);
            }
            assert_eq!(
                u16::from_le_bytes(*pixel.as_chunks::<2>().0.get(3).unwrap()),
                u16::MAX
            );
        }
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(debug_assertions)]
    #[test]
    fn crashing_a_media_job_returns_an_error_to_the_live_parent() {
        let Err(error) = super::execute(super::Request::Crash, std::time::Duration::from_secs(5))
        else {
            panic!("crashed child cannot produce a successful response");
        };
        let diagnostic = format!("{error:#}");
        assert!(diagnostic.contains("media process"));
        assert!(diagnostic.contains("closed its response stream"));
    }

    #[test]
    fn child_probes_audio_and_returns_nonempty_waveform_bands() {
        use crate::project::Project;
        let path = std::env::temp_dir().join(format!("kama-media-job-{}.wav", std::process::id()));
        let frames = 44_100_u32;
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(b"RIFF").unwrap();
        file.write_all(&(36 + frames * 2).to_le_bytes()).unwrap();
        file.write_all(b"WAVEfmt \x10\0\0\0\x01\0\x01\0").unwrap();
        file.write_all(&44_100_u32.to_le_bytes()).unwrap();
        file.write_all(&88_200_u32.to_le_bytes()).unwrap();
        file.write_all(b"\x02\0\x10\0data").unwrap();
        file.write_all(&(frames * 2).to_le_bytes()).unwrap();
        for index in 0..frames {
            let phase = f64::from(index) * std::f64::consts::TAU * 440.0 / 44_100.0;
            let sample = if index < 22_050 {
                0
            } else {
                (phase.sin() * 16_000.0) as i16
            };
            file.write_all(&sample.to_le_bytes()).unwrap();
        }
        drop(file);

        let probe = super::probe(&path).unwrap();
        assert!(probe.has_audio);
        assert!(!probe.has_video);
        assert_eq!(probe.tracks.len(), 1);
        assert_eq!(probe.tracks[0].sample_rate, Some(44_100));
        assert_eq!(probe.tracks[0].channels, Some(1));
        let mut project = Project::new();
        let id = project.import_media(&path).unwrap();
        let waveform = super::analyze(project.media(id).unwrap()).unwrap().unwrap();
        let audio = waveform.audio.expect("audio analysis produces bands");
        assert_eq!(audio.bands.len(), 6);
        for band in &audio.bands {
            assert_eq!(band.len(), 24);
            assert!(band[..10].iter().all(|value| *value == 0));
        }
        assert!(
            audio
                .bands
                .iter()
                .any(|band| band[14..].iter().any(|value| *value > 0))
        );
        assert!(waveform.video.is_none());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn child_probe_reports_missing_path_without_terminating_the_parent() {
        let path =
            std::env::temp_dir().join(format!("kama-missing-media-{}.wav", std::process::id()));
        let error = super::probe(&path).unwrap_err();
        let diagnostic = format!("{error:#}");
        assert!(diagnostic.contains("media process"));
        assert!(diagnostic.contains(path.to_str().unwrap()));
    }

    #[test]
    fn media_worker_child_entry() {
        if std::env::var_os("KAMA_MEDIA_WORKER_TEST_CHILD").is_some() {
            super::run().expect("media child completes one job");
        }
    }
}
