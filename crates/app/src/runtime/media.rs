use std::{
    collections::{HashMap, VecDeque},
    ffi::{c_void, CString},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender, SyncSender, TrySendError},
        Arc, Condvar, Mutex, OnceLock, Weak,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use ffmpeg::{
    codec, format,
    media::Type,
    software::scaling::{context::Context as ScalingContext, flag::Flags as ScalingFlags},
    util::frame::video::Video,
    util::{color, format::pixel::Pixel},
    Rational,
};
use ffmpeg_next as ffmpeg;

use crate::{
    project::{MediaTrackInfo, MediaTrackKind},
    runtime::video::{NativeVideoLayout, VideoFrame},
};

const SEEK_RESTART_THRESHOLD_SECONDS: f64 = 1.0;
const SCRUB_SEEK_RESTART_THRESHOLD_SECONDS: f64 = 0.18;
pub const SCRUB_PREVIEW_FPS: f64 = 60.0;
const SCRUB_DECODE_INTERVAL: Duration = Duration::from_millis(12);
const SCRUB_CACHE_LEAD_SECONDS: f64 = 0.10;
const SCRUB_THUMBNAIL_INTERVAL_SECONDS: f64 = 0.50;
const SCRUB_THUMBNAIL_MAX_EDGE: u32 = 256;
const SCRUB_THUMBNAIL_CAPACITY: usize = 128;

const SCRUB_SEQUENTIAL_WINDOW_SECONDS: f64 = 4.0;

const FRAME_CACHE_CAPACITY: usize = 48;
const FRAME_CACHE_MAX_BYTES: usize = 384 * 1024 * 1024;
const PLAYBACK_PREROLL_FRAMES: usize = 18;
const PRELOAD_PREROLL_FRAMES: usize = 6;
const DECODE_POOL_MAX_THREADS: usize = 4;
const DECODE_POOL_MIN_THREADS: usize = 2;
const DECODE_RETRY_INTERVAL: Duration = Duration::from_millis(120);

struct DecodePolicy {
    active_monitor_decodes: AtomicU64,
    hardware_decoding_enabled: AtomicBool,
    hardware_epoch: AtomicU64,
    offline_export_priority: AtomicU64,
}

static DECODE_POLICY: DecodePolicy = DecodePolicy {
    active_monitor_decodes: AtomicU64::new(0),
    hardware_decoding_enabled: AtomicBool::new(true),
    hardware_epoch: AtomicU64::new(1),
    offline_export_priority: AtomicU64::new(0),
};

pub(crate) fn hardware_decoding_enabled() -> bool {
    DECODE_POLICY
        .hardware_decoding_enabled
        .load(Ordering::Acquire)
}

pub(crate) fn set_hardware_decoding_enabled(enabled: bool) {
    if DECODE_POLICY
        .hardware_decoding_enabled
        .swap(enabled, Ordering::AcqRel)
        != enabled
    {
        DECODE_POLICY.hardware_epoch.fetch_add(1, Ordering::AcqRel);
    }
}

fn hardware_decode_policy_epoch() -> u64 {
    DECODE_POLICY.hardware_epoch.load(Ordering::Acquire)
}

fn monitor_decode_active() -> bool {
    DECODE_POLICY.active_monitor_decodes.load(Ordering::Acquire) != 0
}

struct MonitorDecodeGuard;

impl MonitorDecodeGuard {
    fn enter() -> Self {
        DECODE_POLICY
            .active_monitor_decodes
            .fetch_add(1, Ordering::AcqRel);
        Self
    }
}

impl Drop for MonitorDecodeGuard {
    fn drop(&mut self) {
        DECODE_POLICY
            .active_monitor_decodes
            .fetch_sub(1, Ordering::AcqRel);
    }
}

pub(crate) struct OfflineExportPriorityGuard;

pub(crate) fn prioritize_offline_export() -> OfflineExportPriorityGuard {
    DECODE_POLICY
        .offline_export_priority
        .fetch_add(1, Ordering::AcqRel);
    OfflineExportPriorityGuard
}

impl Drop for OfflineExportPriorityGuard {
    fn drop(&mut self) {
        DECODE_POLICY
            .offline_export_priority
            .fetch_sub(1, Ordering::AcqRel);
    }
}

fn offline_export_has_priority() -> bool {
    DECODE_POLICY
        .offline_export_priority
        .load(Ordering::Acquire)
        != 0
}

#[derive(Default)]
struct HardwareScaleState {
    #[cfg(target_os = "macos")]
    videotoolbox: Option<VideoToolboxScaleSession>,
    #[cfg(target_os = "macos")]
    disabled: bool,
}

impl HardwareScaleState {
    fn scale_for_upload(
        &mut self,
        decoded: &Video,
        width: u32,
        height: u32,
    ) -> Result<Option<Video>> {
        #[cfg(target_os = "macos")]
        {
            if self.disabled
                || width == 0
                || height == 0
                || (width >= decoded.width() && height >= decoded.height())
                || !is_hardware_frame(decoded)
            {
                return Ok(None);
            }
            if self.videotoolbox.is_none() {
                match VideoToolboxScaleSession::new() {
                    Ok(session) => self.videotoolbox = Some(session),
                    Err(_) => {
                        self.disabled = true;
                        return Ok(None);
                    }
                }
            }
            let session = self
                .videotoolbox
                .as_mut()
                .expect("VideoToolbox scale session initialized");
            match session.scale(decoded, width, height) {
                Ok(frame) => Ok(Some(frame)),
                Err(_) => {
                    self.disabled = true;
                    Ok(None)
                }
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (decoded, width, height);
            Ok(None)
        }
    }
}

#[cfg(target_os = "macos")]
type VTPixelTransferSessionRef = *mut c_void;

#[cfg(target_os = "macos")]
#[link(name = "VideoToolbox", kind = "framework")]
extern "C" {
    fn VTPixelTransferSessionCreate(
        allocator: *const c_void,
        session_out: *mut VTPixelTransferSessionRef,
    ) -> i32;
    fn VTPixelTransferSessionTransferImage(
        session: VTPixelTransferSessionRef,
        source: *mut c_void,
        destination: *mut c_void,
    ) -> i32;
    fn VTPixelTransferSessionInvalidate(session: VTPixelTransferSessionRef);
}

#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(value: *const c_void);
}

#[cfg(target_os = "macos")]
struct VideoToolboxScaleSession {
    transfer: VTPixelTransferSessionRef,
    frames_ctx: *mut ffmpeg::ffi::AVBufferRef,
    width: u32,
    height: u32,
    source_frames_ctx: usize,
}

#[cfg(target_os = "macos")]
unsafe impl Send for VideoToolboxScaleSession {}

#[cfg(target_os = "macos")]
impl VideoToolboxScaleSession {
    fn new() -> Result<Self> {
        let mut transfer = std::ptr::null_mut();
        let status = unsafe { VTPixelTransferSessionCreate(std::ptr::null(), &mut transfer) };
        if status != 0 || transfer.is_null() {
            bail!("VTPixelTransferSessionCreate failed with OSStatus {status}");
        }
        Ok(Self {
            transfer,
            frames_ctx: std::ptr::null_mut(),
            width: 0,
            height: 0,
            source_frames_ctx: 0,
        })
    }

    fn clear_frames_context(&mut self) {
        if !self.frames_ctx.is_null() {
            unsafe { ffmpeg::ffi::av_buffer_unref(&mut self.frames_ctx) };
        }
        self.width = 0;
        self.height = 0;
        self.source_frames_ctx = 0;
    }

    fn ensure_frames_context(&mut self, source: &Video, width: u32, height: u32) -> Result<()> {
        let source_ref = unsafe { (*source.as_ptr()).hw_frames_ctx };
        if source_ref.is_null() {
            bail!("VideoToolbox frame has no AVHWFramesContext");
        }
        let source_ctx = unsafe { (*source_ref).data as *mut ffmpeg::ffi::AVHWFramesContext };
        if source_ctx.is_null() {
            bail!("VideoToolbox AVHWFramesContext has no data");
        }
        let source_identity = source_ctx as usize;
        if !self.frames_ctx.is_null()
            && self.width == width
            && self.height == height
            && self.source_frames_ctx == source_identity
        {
            return Ok(());
        }

        self.clear_frames_context();
        let device_ref = unsafe { (*source_ctx).device_ref };
        if device_ref.is_null() {
            bail!("VideoToolbox AVHWFramesContext has no device reference");
        }
        let frames_ref = unsafe { ffmpeg::ffi::av_hwframe_ctx_alloc(device_ref) };
        if frames_ref.is_null() {
            bail!("av_hwframe_ctx_alloc failed for VideoToolbox scaled surface");
        }
        let frames_ctx = unsafe { (*frames_ref).data as *mut ffmpeg::ffi::AVHWFramesContext };
        if frames_ctx.is_null() {
            let mut frames_ref = frames_ref;
            unsafe { ffmpeg::ffi::av_buffer_unref(&mut frames_ref) };
            bail!("scaled VideoToolbox AVHWFramesContext has no data");
        }
        unsafe {
            (*frames_ctx).format = (*source_ctx).format;
            (*frames_ctx).sw_format = (*source_ctx).sw_format;
            (*frames_ctx).width = width as i32;
            (*frames_ctx).height = height as i32;
        }
        let init = unsafe { ffmpeg::ffi::av_hwframe_ctx_init(frames_ref) };
        if init < 0 {
            let mut frames_ref = frames_ref;
            unsafe { ffmpeg::ffi::av_buffer_unref(&mut frames_ref) };
            bail!("av_hwframe_ctx_init failed for {width}x{height} VideoToolbox surface ({init})");
        }
        self.frames_ctx = frames_ref;
        self.width = width;
        self.height = height;
        self.source_frames_ctx = source_identity;
        Ok(())
    }

    fn scale(&mut self, source: &Video, width: u32, height: u32) -> Result<Video> {
        self.ensure_frames_context(source, width, height)?;
        let mut output = Video::empty();
        let allocate =
            unsafe { ffmpeg::ffi::av_hwframe_get_buffer(self.frames_ctx, output.as_mut_ptr(), 0) };
        if allocate < 0 {
            bail!("av_hwframe_get_buffer failed for scaled VideoToolbox frame ({allocate})");
        }
        let source_buffer = unsafe { (*source.as_ptr()).data[3] as *mut c_void };
        let destination_buffer = unsafe { (*output.as_ptr()).data[3] as *mut c_void };
        if source_buffer.is_null() || destination_buffer.is_null() {
            bail!("VideoToolbox frame is missing its CVPixelBufferRef");
        }
        let status = unsafe {
            VTPixelTransferSessionTransferImage(self.transfer, source_buffer, destination_buffer)
        };
        if status != 0 {
            bail!("VTPixelTransferSessionTransferImage failed with OSStatus {status}");
        }
        let props =
            unsafe { ffmpeg::ffi::av_frame_copy_props(output.as_mut_ptr(), source.as_ptr()) };
        if props < 0 {
            bail!("av_frame_copy_props failed for scaled VideoToolbox frame ({props})");
        }
        unsafe {
            (*output.as_mut_ptr()).width = width as i32;
            (*output.as_mut_ptr()).height = height as i32;
        }
        Ok(output)
    }
}

#[cfg(target_os = "macos")]
impl Drop for VideoToolboxScaleSession {
    fn drop(&mut self) {
        self.clear_frames_context();
        if !self.transfer.is_null() {
            unsafe {
                VTPixelTransferSessionInvalidate(self.transfer);
                CFRelease(self.transfer as *const c_void);
            }
            self.transfer = std::ptr::null_mut();
        }
    }
}

fn codec_packets_are_independent(id: codec::Id) -> bool {
    matches!(id, codec::Id::PRORES | codec::Id::UTVIDEO)
}

#[derive(Clone, Debug, Default)]
pub struct AvMediaProbe {
    pub has_video: bool,
    pub has_audio: bool,
    pub video_width: Option<u32>,
    pub video_height: Option<u32>,
    pub duration: Option<f64>,
    pub frame_rate: Option<f64>,
    pub tracks: Vec<MediaTrackInfo>,
}

pub fn probe_av_media(path: &Path) -> Result<AvMediaProbe> {
    init_ffmpeg()?;
    let input = format::input(path).with_context(|| format!("open media {}", path.display()))?;
    let video = input.streams().best(Type::Video);
    let has_video = video.is_some();
    let has_audio = input.streams().best(Type::Audio).is_some();
    let duration = duration_seconds(&input);
    let frame_rate = video.as_ref().and_then(|stream| {
        rational_rate(stream.avg_frame_rate()).or_else(|| rational_rate(stream.rate()))
    });
    let (video_width, video_height) = video
        .and_then(|stream| codec::context::Context::from_parameters(stream.parameters()).ok())
        .and_then(|context| context.decoder().video().ok())
        .map_or((None, None), |decoder| {
            (Some(decoder.width()), Some(decoder.height()))
        });

    let tracks = input
        .streams()
        .filter_map(|stream| {
            let stream_index = stream.index();
            let stream_rate =
                rational_rate(stream.avg_frame_rate()).or_else(|| rational_rate(stream.rate()));
            let context = codec::context::Context::from_parameters(stream.parameters()).ok()?;
            let codec = context.id().name().to_string();
            match context.medium() {
                Type::Video => {
                    let decoder = context.decoder().video().ok()?;
                    Some(MediaTrackInfo {
                        kind: MediaTrackKind::Video,
                        stream_index,
                        codec,
                        bit_rate: (decoder.bit_rate() > 0).then_some(decoder.bit_rate() as u64),
                        width: Some(decoder.width()),
                        height: Some(decoder.height()),
                        frame_rate: stream_rate,
                        sample_rate: None,
                        channels: None,
                    })
                }
                Type::Audio => {
                    let decoder = context.decoder().audio().ok()?;
                    Some(MediaTrackInfo {
                        kind: MediaTrackKind::Audio,
                        stream_index,
                        codec,
                        bit_rate: (decoder.bit_rate() > 0).then_some(decoder.bit_rate() as u64),
                        width: None,
                        height: None,
                        frame_rate: None,
                        sample_rate: (decoder.rate() > 0).then_some(decoder.rate()),
                        channels: (decoder.channels() > 0).then_some(decoder.channels()),
                    })
                }
                _ => None,
            }
        })
        .collect();

    Ok(AvMediaProbe {
        has_video,
        has_audio,
        video_width,
        video_height,
        duration,
        frame_rate,
        tracks,
    })
}

type DecodedFrameBatch = (Arc<VideoFrame>, Vec<(f64, Arc<VideoFrame>)>, f64);

struct DecodeCancel<'a> {
    generation: &'a AtomicU64,
    expected: u64,
}

impl DecodeCancel<'_> {
    fn cancelled(&self) -> bool {
        self.generation.load(Ordering::Acquire) != self.expected
    }

    fn check(&self) -> Result<()> {
        if self.cancelled() {
            bail!("video decode request superseded");
        }
        Ok(())
    }
}

pub(crate) fn init_ffmpeg() -> Result<()> {
    static INIT: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| {
        ffmpeg::init().map_err(|error| error.to_string())?;
        ffmpeg::util::log::set_level(ffmpeg::util::log::Level::Error);
        Ok(())
    })
    .clone()
    .map_err(anyhow::Error::msg)
}

fn duration_seconds(input: &format::context::Input) -> Option<f64> {
    let duration = input.duration();
    (duration > 0)
        .then_some(duration as f64 / ffmpeg::ffi::AV_TIME_BASE as f64)
        .filter(|value| value.is_finite() && *value > 0.0)
}

fn clamp_decode_target(target_seconds: f64, duration: Option<f64>, frame_interval: f64) -> f64 {
    let target_seconds = target_seconds.max(0.0);
    duration
        .map(|duration| target_seconds.min((duration - frame_interval).max(0.0)))
        .unwrap_or(target_seconds)
}

fn rational_rate(rate: Rational) -> Option<f64> {
    let value = f64::from(rate);
    (value.is_finite() && value > 0.0).then_some(value)
}

struct DecoderState {
    input: format::context::Input,
    decoder: ffmpeg::decoder::Video,
    _hardware_decode: Option<Box<HardwareDecodeSelection>>,
    scaler: Option<ScalingContext>,
    scaler_format: Option<Pixel>,
    native_format: Option<Pixel>,
    observed_source_format: Option<Pixel>,
    hardware_scaler: HardwareScaleState,
    stream_index: usize,
    time_base: Rational,
    duration: Option<f64>,
    output_width: u32,
    output_height: u32,
    fit_width: u32,
    fit_height: u32,
    prefer_native: bool,
    intra_only: bool,
    decoded_until: f64,
    last_frame_time: Option<f64>,
    last_output_frame: Option<(f64, Arc<VideoFrame>)>,
    hardware_frame_seen: bool,
}

impl DecoderState {
    fn open(
        path: &Path,
        seek_seconds: f64,
        width: u32,
        height: u32,
        hardware_candidates: &[String],
        prefer_native: bool,
    ) -> Result<Self> {
        init_ffmpeg()?;
        let mut input =
            format::input(path).with_context(|| format!("open video {}", path.display()))?;
        let duration = duration_seconds(&input);
        let stream = input
            .streams()
            .best(Type::Video)
            .context("media has no video stream")?;
        let stream_index = stream.index();
        let time_base = stream.time_base();
        let stream_has_alpha = stream
            .metadata()
            .get("alpha_mode")
            .is_some_and(|value| value != "0");
        let mut context = codec::context::Context::from_parameters(stream.parameters())
            .context("create FFmpeg decoder context")?;
        let codec_id = context.id();
        let intra_only = codec_packets_are_independent(codec_id);
        let transparent_vpx_decoder = if stream_has_alpha {
            match codec_id {
                codec::Id::VP8 => Some("libvpx"),
                codec::Id::VP9 => Some("libvpx-vp9"),
                _ => None,
            }
        } else {
            None
        };

        let (decoder, hardware_decode) = if let Some(decoder_name) = transparent_vpx_decoder {
            let decoder_codec = codec::decoder::find_by_name(decoder_name).with_context(|| {
                format!("FFmpeg build is missing {decoder_name}, required for transparent WebM")
            })?;
            let decoder = context
                .decoder()
                .open_as(decoder_codec)
                .context("open FFmpeg transparent WebM decoder")?
                .video()
                .context("open FFmpeg transparent WebM video stream")?;
            (decoder, None)
        } else {
            let hardware_decode = try_enable_hardware_decode(&mut context, hardware_candidates);
            let decoder = context
                .decoder()
                .video()
                .context("open FFmpeg video decoder")?;
            if let Some(_selection) = &hardware_decode {}
            (decoder, hardware_decode)
        };
        let (fit_width, fit_height) = fit_size(decoder.width(), decoder.height(), width, height);
        let seek_seconds = duration
            .map(|duration| seek_seconds.min((duration - 0.001).max(0.0)))
            .unwrap_or(seek_seconds)
            .max(0.0);

        if seek_seconds > 0.0 {
            let target = (seek_seconds * ffmpeg::ffi::AV_TIME_BASE as f64).round() as i64;

            let _ = input.seek(target, ..target);
        }

        Ok(Self {
            input,
            decoder,
            _hardware_decode: hardware_decode,
            scaler: None,
            scaler_format: None,
            native_format: None,
            observed_source_format: None,
            hardware_scaler: HardwareScaleState::default(),
            stream_index,
            time_base,
            duration,
            output_width: width,
            output_height: height,
            fit_width,
            fit_height,
            prefer_native,
            intra_only,
            decoded_until: seek_seconds.max(0.0),
            last_frame_time: None,
            last_output_frame: None,
            hardware_frame_seen: false,
        })
    }

    fn hardware_device_name(&self) -> Option<&str> {
        self._hardware_decode
            .as_ref()
            .map(|selection| selection.device_name.as_str())
    }

    fn can_fallback_from_hardware_failure(&self) -> bool {
        self._hardware_decode.is_some() && !self.hardware_frame_seen
    }

    fn seek_to(&mut self, seconds: f64) -> Result<()> {
        let seconds = self
            .duration
            .map(|duration| seconds.min((duration - 0.001).max(0.0)))
            .unwrap_or(seconds)
            .max(0.0);
        let target = (seconds * ffmpeg::ffi::AV_TIME_BASE as f64).round() as i64;
        self.input
            .seek(target, ..target)
            .with_context(|| format!("seek decoder to {seconds:.3}s"))?;

        self.decoder.flush();
        self.decoded_until = seconds;
        self.last_frame_time = None;
        self.last_output_frame = None;
        Ok(())
    }

    fn set_output_size(&mut self, width: u32, height: u32) {
        if self.output_width == width && self.output_height == height {
            return;
        }
        let (fit_width, fit_height) =
            fit_size(self.decoder.width(), self.decoder.height(), width, height);
        self.output_width = width;
        self.output_height = height;
        self.fit_width = fit_width;
        self.fit_height = fit_height;

        self.scaler = None;
        self.scaler_format = None;
        self.last_output_frame = None;
    }

    fn last_frame_time(&self) -> Option<f64> {
        self.last_frame_time
    }

    fn decode_at(
        &mut self,
        target_seconds: f64,
        cache_from: f64,
        collect_nearby: bool,
        fast_keyframe_preview: bool,
        frame_interval: f64,
        cancel: Option<&DecodeCancel<'_>>,
    ) -> Result<Option<DecodedFrameBatch>> {
        if let Some(cancel) = cancel {
            cancel.check()?;
        }
        let requested_target_seconds = target_seconds.max(0.0);
        let frame_interval = frame_interval.max(1.0 / 240.0);
        let target_seconds =
            clamp_decode_target(requested_target_seconds, self.duration, frame_interval);
        let cache_from = cache_from.max(0.0).min(target_seconds);
        let timestamp_slop = (f64::from(self.time_base).abs() * 0.5).max(1.0e-6);
        let allow_eof_fallback = self
            .duration
            .is_some_and(|duration| requested_target_seconds + frame_interval * 2.0 >= duration);
        let eof_tolerance = (frame_interval * 3.0).clamp(0.05, 0.25);

        let mut eof_fallback = self
            .last_output_frame
            .as_ref()
            .and_then(|(timestamp, frame)| {
                let follows_last_frame = target_seconds + timestamp_slop >= *timestamp
                    && target_seconds - *timestamp <= eof_tolerance;
                (allow_eof_fallback || follows_last_frame).then(|| (*timestamp, Arc::clone(frame)))
            });
        let mut nearby = Vec::new();

        let stream_index = self.stream_index;
        let time_base = self.time_base;
        let output_width = self.output_width;
        let output_height = self.output_height;
        let fit_width = self.fit_width;
        let fit_height = self.fit_height;
        let prefer_native = self.prefer_native;
        let intra_only = self.intra_only;
        let decoder = &mut self.decoder;
        let scaler = &mut self.scaler;
        let scaler_format = &mut self.scaler_format;
        let native_format = &mut self.native_format;
        let observed_source_format = &mut self.observed_source_format;
        let hardware_scaler = &mut self.hardware_scaler;
        let decoded_until = &mut self.decoded_until;
        let last_frame_time = &mut self.last_frame_time;
        let last_output_frame = &mut self.last_output_frame;
        let hardware_frame_seen = &mut self.hardware_frame_seen;
        let hardware_rejected = self
            ._hardware_decode
            .as_ref()
            .map(|selection| &selection.rejected);
        let hardware_name = self
            ._hardware_decode
            .as_ref()
            .map(|selection| selection.device_name.clone());
        let mut convert = |decoded: &Video| {
            Self::convert_frame(FrameConversionArgs {
                scaler,
                scaler_format,
                native_format,
                observed_source_format,
                hardware_scaler,
                decoded,
                output_size: [output_width, output_height],
                fit_size: [fit_width, fit_height],
                prefer_native,
            })
            .map(Arc::new)
        };

        let mut packets = self.input.packets();
        loop {
            if let Some(cancel) = cancel {
                cancel.check()?;
            }
            let Some((stream, packet)) = packets.next() else {
                break;
            };
            if stream.index() != stream_index {
                continue;
            }

            if intra_only && !fast_keyframe_preview {
                if let Some(packet_pts) = packet.pts().or_else(|| packet.dts()) {
                    let packet_time = packet_pts as f64 * f64::from(time_base);
                    if packet_time + timestamp_slop < target_seconds {
                        continue;
                    }
                }
            }
            if let Err(error) = decoder.send_packet(&packet) {
                if hardware_rejected.is_some_and(|rejected| rejected.load(Ordering::Acquire))
                    && !*hardware_frame_seen
                {
                    bail!("hardware decoder rejected its surface format before producing a frame: {error}");
                }
                bail!("send FFmpeg video packet: {error}");
            }
            let mut decoded = Video::empty();
            loop {
                match decoder.receive_frame(&mut decoded) {
                    Ok(()) => {}
                    Err(ffmpeg::Error::Other { errno })
                        if errno == ffmpeg::util::error::EAGAIN
                            || errno == ffmpeg::util::error::EWOULDBLOCK =>
                    {
                        break;
                    }
                    Err(ffmpeg::Error::Eof) => break,
                    Err(error) => bail!("receive FFmpeg video frame: {error}"),
                }
                observe_hardware_frame(
                    &decoded,
                    hardware_frame_seen,
                    hardware_rejected,
                    hardware_name.as_deref(),
                )?;
                let timestamp = decoded
                    .timestamp()
                    .or_else(|| decoded.pts())
                    .map(|pts| pts as f64 * f64::from(time_base))
                    .unwrap_or(*decoded_until);
                *decoded_until = (*decoded_until).max(timestamp);
                *last_frame_time = Some(timestamp);
                if cancel.is_some_and(|cancel| cancel.cancelled()) {
                    continue;
                }
                if timestamp + timestamp_slop < target_seconds && !fast_keyframe_preview {
                    let retain_eof_candidate =
                        allow_eof_fallback || target_seconds - timestamp <= eof_tolerance;
                    if (collect_nearby && timestamp + timestamp_slop >= cache_from)
                        || retain_eof_candidate
                    {
                        let converted = convert(&decoded)?;
                        if retain_eof_candidate {
                            *last_output_frame = Some((timestamp, Arc::clone(&converted)));
                            eof_fallback = Some((timestamp, Arc::clone(&converted)));
                        }
                        if collect_nearby && timestamp + timestamp_slop >= cache_from {
                            nearby.push((timestamp, converted));
                            if nearby.len() > 4 {
                                nearby.remove(0);
                            }
                        }
                    }
                    continue;
                }
                let converted = convert(&decoded)?;
                *last_output_frame = Some((timestamp, Arc::clone(&converted)));
                nearby.push((timestamp, Arc::clone(&converted)));
                return Ok(Some((converted, nearby, timestamp)));
            }
        }

        decoder.send_eof().ok();
        loop {
            let mut decoded = Video::empty();
            match decoder.receive_frame(&mut decoded) {
                Ok(()) => {}
                Err(ffmpeg::Error::Eof) => break,
                Err(ffmpeg::Error::Other { errno })
                    if errno == ffmpeg::util::error::EAGAIN
                        || errno == ffmpeg::util::error::EWOULDBLOCK =>
                {
                    break;
                }
                Err(error) => bail!("receive flushed FFmpeg video frame: {error}"),
            }
            observe_hardware_frame(
                &decoded,
                hardware_frame_seen,
                hardware_rejected,
                hardware_name.as_deref(),
            )?;
            let timestamp = decoded
                .timestamp()
                .or_else(|| decoded.pts())
                .map(|pts| pts as f64 * f64::from(time_base))
                .unwrap_or(*decoded_until);
            *decoded_until = (*decoded_until).max(timestamp);
            *last_frame_time = Some(timestamp);
            if timestamp + timestamp_slop >= target_seconds {
                let converted = convert(&decoded)?;
                *last_output_frame = Some((timestamp, Arc::clone(&converted)));
                nearby.push((timestamp, Arc::clone(&converted)));
                return Ok(Some((converted, nearby, timestamp)));
            }
            if allow_eof_fallback || target_seconds - timestamp <= eof_tolerance {
                let converted = convert(&decoded)?;
                *last_output_frame = Some((timestamp, Arc::clone(&converted)));
                eof_fallback = Some((timestamp, converted));
            }
        }
        if let Some((timestamp, frame)) = eof_fallback {
            *last_output_frame = Some((timestamp, Arc::clone(&frame)));
            nearby.push((timestamp, Arc::clone(&frame)));
            return Ok(Some((frame, nearby, timestamp)));
        }
        Ok(None)
    }

    fn convert_frame(args: FrameConversionArgs<'_>) -> Result<VideoFrame> {
        let FrameConversionArgs {
            scaler,
            scaler_format,
            native_format,
            observed_source_format,
            hardware_scaler,
            decoded,
            output_size,
            fit_size,
            prefer_native,
        } = args;
        let [output_width, output_height] = output_size;
        let [fit_width, fit_height] = fit_size;
        let mut mapped = Video::empty();
        let mut transferred = Video::empty();
        let hardware = is_hardware_frame(decoded);
        let scaled_hardware = if hardware && prefer_native {
            hardware_scaler.scale_for_upload(decoded, fit_width, fit_height)?
        } else {
            None
        };
        let hardware_source = scaled_hardware.as_ref().unwrap_or(decoded);
        let source = if hardware {
            let map_result = unsafe {
                ffmpeg::ffi::av_hwframe_map(
                    mapped.as_mut_ptr(),
                    hardware_source.as_ptr(),
                    ffmpeg::ffi::AV_HWFRAME_MAP_READ as i32,
                )
            };
            if map_result >= 0 {
                &mapped
            } else {
                let transfer_result = unsafe {
                    ffmpeg::ffi::av_hwframe_transfer_data(
                        transferred.as_mut_ptr(),
                        hardware_source.as_ptr(),
                        0,
                    )
                };
                if transfer_result < 0 {
                    bail!(
                        "FFmpeg hardware-frame map ({map_result}) and transfer ({transfer_result}) both failed"
                    );
                }
                &transferred
            }
        } else {
            decoded
        };

        let source_format = source.format();
        if *observed_source_format != Some(source_format) {
            *observed_source_format = Some(source_format);
        }
        if prefer_native {
            if let Some(native) = native_yuv_frame(
                source,
                output_width,
                output_height,
                fit_width,
                fit_height,
                transfer_code(decoded.color_transfer_characteristic()),
                matches!(decoded.color_primaries(), color::Primaries::BT2020),
                yuv_matrix_code(decoded),
                decoded.color_range() == color::Range::JPEG,
            )? {
                if *native_format != Some(source_format) {
                    *native_format = Some(source_format);
                }
                return Ok(native);
            }
        }
        if scaler.is_none() || *scaler_format != Some(source_format) {
            let mut created = ScalingContext::get(
                source_format,
                source.width(),
                source.height(),
                Pixel::RGBA64LE,
                fit_width,
                fit_height,
                ScalingFlags::FAST_BILINEAR,
            )
            .context("create FFmpeg packed RGBA16 scaler")?;
            configure_scaler_color(&mut created, decoded);
            *scaler = Some(created);
            *scaler_format = Some(source_format);
        } else if let Some(existing) = scaler.as_mut() {
            configure_scaler_color(existing, decoded);
        }

        let mut rgba = Video::empty();
        scaler
            .as_mut()
            .context("FFmpeg scaler missing")?
            .run(source, &mut rgba)
            .context("scale FFmpeg frame to packed RGBA16")?;

        let pixels = copy_video_plane(&rgba, 0, fit_width as usize * 8, fit_height as usize)?;
        Ok(VideoFrame::from_rgba16(
            output_width,
            output_height,
            fit_width,
            fit_height,
            pixels,
            transfer_code(decoded.color_transfer_characteristic()),
            matches!(decoded.color_primaries(), color::Primaries::BT2020),
        ))
    }
}

#[allow(clippy::too_many_arguments)]
fn native_yuv_frame(
    source: &Video,
    output_width: u32,
    output_height: u32,
    fit_width: u32,
    fit_height: u32,
    transfer: u32,
    bt2020_primaries: bool,
    yuv_matrix: u32,
    full_range: bool,
) -> Result<Option<VideoFrame>> {
    let format = source.format();
    let source_width = source.width();
    let source_height = source.height();
    if source_width == 0 || source_height == 0 {
        return Ok(None);
    }

    #[cfg(target_os = "macos")]
    if matches!(format, Pixel::AYUV | Pixel::AYUV64LE) {
        let bit_depth = if format == Pixel::AYUV64LE { 16 } else { 8 };
        let bytes_per_sample = if bit_depth > 8 { 2usize } else { 1usize };
        validate_video_plane(
            source,
            0,
            source_width as usize * bytes_per_sample * 4,
            source_height as usize,
        )?;
        return Ok(Some(VideoFrame::from_native_yuv(
            output_width,
            output_height,
            source_width,
            source_height,
            fit_width,
            fit_height,
            NativeVideoLayout::Ayuv,
            bit_depth,
            retain_video_frame(source)?,
            true,
            transfer,
            bt2020_primaries,
            yuv_matrix,
            full_range,
        )));
    }

    let (layout, bit_depth, has_alpha, jpeg_range) = match format {
        Pixel::NV12 => (NativeVideoLayout::Nv12, 8, false, false),
        Pixel::P010LE => (NativeVideoLayout::P010, 10, false, false),
        Pixel::P210LE => (NativeVideoLayout::P210, 10, false, false),

        Pixel::YUV420P => (NativeVideoLayout::Yuv420p, 8, false, false),
        Pixel::YUVJ420P => (NativeVideoLayout::Yuv420p, 8, false, true),
        Pixel::YUVA420P => (NativeVideoLayout::Yuv420p, 8, true, false),
        Pixel::YUV420P9LE => (NativeVideoLayout::Yuv420p, 9, false, false),
        Pixel::YUVA420P9LE => (NativeVideoLayout::Yuv420p, 9, true, false),
        Pixel::YUV420P10LE => (NativeVideoLayout::Yuv420p, 10, false, false),
        Pixel::YUVA420P10LE => (NativeVideoLayout::Yuv420p, 10, true, false),
        Pixel::YUV420P12LE => (NativeVideoLayout::Yuv420p, 12, false, false),
        Pixel::YUV420P14LE => (NativeVideoLayout::Yuv420p, 14, false, false),
        Pixel::YUV420P16LE => (NativeVideoLayout::Yuv420p, 16, false, false),
        Pixel::YUVA420P16LE => (NativeVideoLayout::Yuv420p, 16, true, false),

        Pixel::YUV422P => (NativeVideoLayout::Yuv422p, 8, false, false),
        Pixel::YUVJ422P => (NativeVideoLayout::Yuv422p, 8, false, true),
        Pixel::YUVA422P => (NativeVideoLayout::Yuv422p, 8, true, false),
        Pixel::YUV422P9LE => (NativeVideoLayout::Yuv422p, 9, false, false),
        Pixel::YUVA422P9LE => (NativeVideoLayout::Yuv422p, 9, true, false),
        Pixel::YUV422P10LE => (NativeVideoLayout::Yuv422p, 10, false, false),
        Pixel::YUVA422P10LE => (NativeVideoLayout::Yuv422p, 10, true, false),
        Pixel::YUV422P12LE => (NativeVideoLayout::Yuv422p, 12, false, false),
        Pixel::YUVA422P12LE => (NativeVideoLayout::Yuv422p, 12, true, false),
        Pixel::YUV422P14LE => (NativeVideoLayout::Yuv422p, 14, false, false),
        Pixel::YUV422P16LE => (NativeVideoLayout::Yuv422p, 16, false, false),
        Pixel::YUVA422P16LE => (NativeVideoLayout::Yuv422p, 16, true, false),

        Pixel::YUV444P => (NativeVideoLayout::Yuv444p, 8, false, false),
        Pixel::YUVJ444P => (NativeVideoLayout::Yuv444p, 8, false, true),
        Pixel::YUVA444P => (NativeVideoLayout::Yuv444p, 8, true, false),
        Pixel::YUV444P9LE => (NativeVideoLayout::Yuv444p, 9, false, false),
        Pixel::YUVA444P9LE => (NativeVideoLayout::Yuv444p, 9, true, false),
        Pixel::YUV444P10LE => (NativeVideoLayout::Yuv444p, 10, false, false),
        Pixel::YUVA444P10LE => (NativeVideoLayout::Yuv444p, 10, true, false),
        Pixel::YUV444P12LE => (NativeVideoLayout::Yuv444p, 12, false, false),
        Pixel::YUVA444P12LE => (NativeVideoLayout::Yuv444p, 12, true, false),
        Pixel::YUV444P14LE => (NativeVideoLayout::Yuv444p, 14, false, false),
        Pixel::YUV444P16LE => (NativeVideoLayout::Yuv444p, 16, false, false),
        Pixel::YUVA444P16LE => (NativeVideoLayout::Yuv444p, 16, true, false),
        _ => return Ok(None),
    };

    let bytes_per_sample = if bit_depth > 8 { 2usize } else { 1usize };
    let chroma_width = layout.chroma_width(source_width);
    let chroma_height = layout.chroma_height(source_height);
    validate_video_plane(
        source,
        0,
        source_width as usize * bytes_per_sample,
        source_height as usize,
    )?;
    validate_video_plane(
        source,
        1,
        chroma_width as usize
            * bytes_per_sample
            * if matches!(
                layout,
                NativeVideoLayout::Nv12 | NativeVideoLayout::P010 | NativeVideoLayout::P210
            ) {
                2
            } else {
                1
            },
        chroma_height as usize,
    )?;
    if !matches!(
        layout,
        NativeVideoLayout::Nv12 | NativeVideoLayout::P010 | NativeVideoLayout::P210
    ) {
        validate_video_plane(
            source,
            2,
            chroma_width as usize * bytes_per_sample,
            chroma_height as usize,
        )?;
    }
    if has_alpha {
        validate_video_plane(
            source,
            3,
            source_width as usize * bytes_per_sample,
            source_height as usize,
        )?;
    }

    Ok(Some(VideoFrame::from_native_yuv(
        output_width,
        output_height,
        source_width,
        source_height,
        fit_width,
        fit_height,
        layout,
        bit_depth,
        retain_video_frame(source)?,
        has_alpha,
        transfer,
        bt2020_primaries,
        yuv_matrix,
        full_range || jpeg_range,
    )))
}

fn retain_video_frame(frame: &Video) -> Result<Arc<Video>> {
    let cloned = unsafe { ffmpeg::ffi::av_frame_clone(frame.as_ptr()) };
    if cloned.is_null() {
        bail!("FFmpeg could not retain decoded video frame");
    }
    Ok(Arc::new(unsafe { Video::wrap(cloned) }))
}

fn validate_video_plane(frame: &Video, plane: usize, row_bytes: usize, rows: usize) -> Result<()> {
    let stride = frame.stride(plane);
    let data = frame.data(plane);
    let source_len = rows
        .checked_mul(stride)
        .context("native video plane size overflow")?;
    if stride < row_bytes || source_len > data.len() {
        bail!("FFmpeg returned an invalid native video plane stride");
    }
    Ok(())
}

fn copy_video_plane(frame: &Video, plane: usize, row_bytes: usize, rows: usize) -> Result<Vec<u8>> {
    let stride = frame.stride(plane);
    let data = frame.data(plane);
    let source_len = rows
        .checked_mul(stride)
        .context("native video plane size overflow")?;
    let output_len = rows
        .checked_mul(row_bytes)
        .context("native video plane size overflow")?;
    if stride < row_bytes || source_len > data.len() {
        bail!("FFmpeg returned an invalid native video plane stride");
    }
    if stride == row_bytes {
        return Ok(data[..output_len].to_vec());
    }
    let mut output = vec![0u8; output_len];
    for (target, source) in output
        .chunks_exact_mut(row_bytes)
        .zip(data.chunks_exact(stride))
        .take(rows)
    {
        target.copy_from_slice(&source[..row_bytes]);
    }
    Ok(output)
}

fn yuv_matrix_code(frame: &Video) -> u32 {
    match frame.color_space() {
        color::Space::BT2020NCL | color::Space::BT2020CL => 2,
        color::Space::BT709 => 1,
        _ if frame.height() > 576 || frame.width() >= 1280 => 1,
        _ => 0,
    }
}

#[derive(Clone)]
struct ScrubFrame {
    frame: Arc<VideoFrame>,
    exact: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FrameKey {
    frame_index: i64,
    width: u32,
    height: u32,
}

#[derive(Default)]
struct FrameCache(crate::app_shared::BoundedCache<FrameKey, Arc<VideoFrame>>);

impl FrameCache {
    fn get(&mut self, key: FrameKey) -> Option<Arc<VideoFrame>> {
        self.0.get(&key).map(Arc::clone)
    }

    fn contains(&self, key: FrameKey) -> bool {
        self.0.contains(&key)
    }

    fn insert(&mut self, key: FrameKey, frame: Arc<VideoFrame>) {
        self.0.insert(key, frame);
        self.trim_to(FRAME_CACHE_CAPACITY, FRAME_CACHE_MAX_BYTES);
    }

    fn trim_to(&mut self, capacity: usize, max_bytes: usize) {
        self.0.trim(capacity, max_bytes, |frame| frame.byte_len());
    }

    fn iter(&self) -> impl Iterator<Item = &(FrameKey, Arc<VideoFrame>)> {
        self.0.iter()
    }

    fn nearest(&self, key: FrameKey) -> Option<Arc<VideoFrame>> {
        self.iter()
            .filter(|(cached, _)| cached.width == key.width && cached.height == key.height)
            .min_by_key(|(cached, _)| (cached.frame_index - key.frame_index).abs())
            .map(|(_, frame)| Arc::clone(frame))
    }
}

enum HardwareFailure {
    Runtime(String),
    Startup(String),
}

struct FrameConversionArgs<'a> {
    scaler: &'a mut Option<ScalingContext>,
    scaler_format: &'a mut Option<Pixel>,
    native_format: &'a mut Option<Pixel>,
    observed_source_format: &'a mut Option<Pixel>,
    hardware_scaler: &'a mut HardwareScaleState,
    decoded: &'a Video,
    output_size: [u32; 2],
    fit_size: [u32; 2],
    prefer_native: bool,
}

impl HardwareFailure {
    fn device(&self) -> &str {
        match self {
            Self::Runtime(device) | Self::Startup(device) => device,
        }
    }
}

struct BlockingVideoDecoder {
    path: PathBuf,
    state: Option<DecoderState>,
    cache: FrameCache,
    last_scrub_decode: Option<Instant>,
    hardware_candidates: Vec<String>,
    hardware_policy_epoch: u64,
    prefer_native: bool,
}

impl BlockingVideoDecoder {
    fn new(path: PathBuf) -> Self {
        Self::new_with_native_policy(path, true)
    }

    fn new_preview(path: PathBuf) -> Self {
        Self::new_with_native_policy(path, false)
    }

    fn new_with_native_policy(path: PathBuf, prefer_native: bool) -> Self {
        Self {
            path,
            state: None,
            cache: FrameCache::default(),
            last_scrub_decode: None,
            hardware_candidates: if hardware_decoding_enabled() {
                hardware_decode_names()
            } else {
                Vec::new()
            },
            hardware_policy_epoch: hardware_decode_policy_epoch(),
            prefer_native,
        }
    }

    fn sync_hardware_policy(&mut self) {
        let epoch = hardware_decode_policy_epoch();
        if self.hardware_policy_epoch == epoch {
            return;
        }
        self.hardware_policy_epoch = epoch;
        self.hardware_candidates = if hardware_decoding_enabled() {
            hardware_decode_names()
        } else {
            Vec::new()
        };
        self.state = None;
        self.cache = FrameCache::default();
        self.last_scrub_decode = None;
    }

    fn hardware_failure(&self) -> Option<HardwareFailure> {
        let state = self.state.as_ref()?;
        let device = state.hardware_device_name()?.to_owned();
        if state.hardware_frame_seen {
            Some(HardwareFailure::Runtime(device))
        } else if state.can_fallback_from_hardware_failure() {
            Some(HardwareFailure::Startup(device))
        } else {
            None
        }
    }

    fn with_hardware_fallback<T>(
        &mut self,
        operation: &str,
        cancel: Option<&DecodeCancel<'_>>,
        mut attempt: impl FnMut(&mut Self) -> Result<T>,
    ) -> Result<T> {
        self.sync_hardware_policy();
        let mut last_error = None;
        let mut reset_runtime_hardware: Option<String> = None;
        let max_attempts = self
            .hardware_candidates
            .len()
            .saturating_mul(2)
            .saturating_add(1)
            .max(1);

        for _ in 0..max_attempts {
            if let Some(cancel) = cancel {
                cancel.check()?;
            }

            let error = match attempt(self) {
                Ok(value) => return Ok(value),
                Err(error) if cancel.is_some_and(DecodeCancel::cancelled) => return Err(error),
                Err(error) => error,
            };
            let Some(failure) = self.hardware_failure() else {
                return Err(error);
            };
            let device = failure.device();
            if matches!(failure, HardwareFailure::Runtime(_))
                && reset_runtime_hardware.as_deref() != Some(device)
            {
                self.state = None;
                reset_runtime_hardware = Some(device.to_owned());
            } else {
                self.hardware_candidates
                    .retain(|candidate| candidate != device);
                self.state = None;
                reset_runtime_hardware = None;
            }
            last_error = Some(error);
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("video {operation} failed")))
    }

    fn frame(
        &mut self,
        time: f64,
        timeline_fps: f64,
        width: u32,
        height: u32,
        interactive_scrub: bool,
        cancel: Option<&DecodeCancel<'_>>,
    ) -> Result<Option<Arc<VideoFrame>>> {
        self.with_hardware_fallback("decode", cancel, |decoder| {
            decoder.frame_once(time, timeline_fps, width, height, interactive_scrub, cancel)
        })
    }

    fn scrub_preview(
        &mut self,
        time: f64,
        timeline_fps: f64,
        width: u32,
        height: u32,
        cancel: Option<&DecodeCancel<'_>>,
    ) -> Result<Option<ScrubFrame>> {
        self.with_hardware_fallback("scrub decode", cancel, |decoder| {
            decoder.scrub_preview_once(time, timeline_fps, width, height, cancel)
        })
    }

    fn scrub_preview_once(
        &mut self,
        time: f64,
        timeline_fps: f64,
        width: u32,
        height: u32,
        cancel: Option<&DecodeCancel<'_>>,
    ) -> Result<Option<ScrubFrame>> {
        if let Some(cancel) = cancel {
            cancel.check()?;
        }
        let fps = timeline_fps.max(1.0);
        let request_fps = fps.clamp(1.0, SCRUB_PREVIEW_FPS);
        let request_index = (time.max(0.0) * request_fps).floor() as i64;
        let target_time = request_index as f64 / request_fps;
        let frame_index = (target_time * fps).floor() as i64;

        let key = FrameKey {
            frame_index,
            width,
            height,
        };
        if let Some(frame) = self.cache.get(key) {
            return Ok(Some(ScrubFrame { frame, exact: true }));
        }

        if self
            .last_scrub_decode
            .is_some_and(|last| last.elapsed() < SCRUB_DECODE_INTERVAL)
        {
            if let Some(frame) = self.cache.nearest(key) {
                return Ok(Some(ScrubFrame {
                    frame,
                    exact: false,
                }));
            }
        }

        let dimensions_changed = self
            .state
            .as_ref()
            .is_some_and(|state| state.output_width != width || state.output_height != height);
        let half_frame = 0.5 / fps;
        let can_decode_forward = self.state.as_ref().is_some_and(|state| {
            !dimensions_changed
                && state.last_frame_time().is_some_and(|decoded_time| {
                    target_time + half_frame >= decoded_time
                        && target_time - decoded_time <= SCRUB_SEQUENTIAL_WINDOW_SECONDS
                })
        });

        if self.state.is_none() {
            self.state = Some(DecoderState::open(
                &self.path,
                target_time,
                width,
                height,
                &self.hardware_candidates,
                self.prefer_native,
            )?);
        } else if dimensions_changed || !can_decode_forward {
            if let Some(state) = self.state.as_mut() {
                if let Some(cancel) = cancel {
                    cancel.check()?;
                }
                if dimensions_changed {
                    state.set_output_size(width, height);
                }
                if state.seek_to(target_time).is_err() {
                    self.state = Some(DecoderState::open(
                        &self.path,
                        target_time,
                        width,
                        height,
                        &self.hardware_candidates,
                        self.prefer_native,
                    )?);
                }
            }
        }

        if let Some(cancel) = cancel {
            cancel.check()?;
        }
        let Some((frame, _nearby, decoded_time)) = self
            .state
            .as_mut()
            .context("FFmpeg decoder state missing")?
            .decode_at(
                target_time,
                target_time,
                false,
                !can_decode_forward,
                1.0 / fps,
                cancel,
            )
            .with_context(|| {
                format!(
                    "fast scrub preview {} at {target_time:.3}s",
                    self.path.display()
                )
            })?
        else {
            return Ok(None);
        };

        let decoded_index = (decoded_time.max(0.0) * fps).floor() as i64;
        self.cache.insert(
            FrameKey {
                frame_index: decoded_index,
                width,
                height,
            },
            Arc::clone(&frame),
        );
        self.last_scrub_decode = Some(Instant::now());
        Ok(Some(ScrubFrame {
            frame,
            exact: decoded_index == frame_index,
        }))
    }

    fn frame_once(
        &mut self,
        time: f64,
        timeline_fps: f64,
        width: u32,
        height: u32,
        interactive_scrub: bool,
        cancel: Option<&DecodeCancel<'_>>,
    ) -> Result<Option<Arc<VideoFrame>>> {
        if let Some(cancel) = cancel {
            cancel.check()?;
        }
        let source_fps = timeline_fps.max(1.0);
        let request_fps = if interactive_scrub {
            source_fps.clamp(1.0, SCRUB_PREVIEW_FPS)
        } else {
            source_fps
        };
        let request_index = (time.max(0.0) * request_fps).floor() as i64;
        let target_time = request_index as f64 / request_fps;
        let frame_index = (target_time * source_fps).floor() as i64;
        let fps = source_fps;
        let key = FrameKey {
            frame_index,
            width,
            height,
        };
        if let Some(frame) = self.cache.get(key) {
            return Ok(Some(frame));
        }

        if interactive_scrub
            && self
                .last_scrub_decode
                .is_some_and(|last| last.elapsed() < SCRUB_DECODE_INTERVAL)
        {
            if let Some(frame) = self.cache.nearest(key) {
                return Ok(Some(frame));
            }
        }

        let half_frame = 0.5 / fps;
        let restart_threshold = if interactive_scrub {
            SCRUB_SEEK_RESTART_THRESHOLD_SECONDS
        } else {
            SEEK_RESTART_THRESHOLD_SECONDS
        };
        let restart = self.state.as_ref().is_none_or(|state| {
            state.output_width != width
                || state.output_height != height
                || target_time + half_frame < state.decoded_until
                || target_time - state.decoded_until > restart_threshold
        });
        if restart {
            let seek_time = (target_time
                - if interactive_scrub {
                    SCRUB_CACHE_LEAD_SECONDS
                } else {
                    0.35
                })
            .max(0.0);
            let dimensions_changed = self
                .state
                .as_ref()
                .is_some_and(|state| state.output_width != width || state.output_height != height);
            if self.state.is_none() {
                self.state = Some(DecoderState::open(
                    &self.path,
                    seek_time,
                    width,
                    height,
                    &self.hardware_candidates,
                    self.prefer_native,
                )?);
            } else if let Some(state) = self.state.as_mut() {
                if let Some(cancel) = cancel {
                    cancel.check()?;
                }
                if dimensions_changed {
                    state.set_output_size(width, height);
                }
                if state.seek_to(seek_time).is_err() {
                    self.state = Some(DecoderState::open(
                        &self.path,
                        seek_time,
                        width,
                        height,
                        &self.hardware_candidates,
                        self.prefer_native,
                    )?);
                }
            }
        }

        let cache_from = if restart {
            (target_time
                - if interactive_scrub {
                    SCRUB_CACHE_LEAD_SECONDS
                } else {
                    0.45
                })
            .max(0.0)
        } else {
            target_time
        };
        let Some((frame, nearby, _decoded_time)) = self
            .state
            .as_mut()
            .context("FFmpeg decoder state missing")?
            .decode_at(
                target_time,
                cache_from,
                interactive_scrub && restart,
                false,
                1.0 / fps,
                cancel,
            )
            .with_context(|| format!("decode {} at {target_time:.3}s", self.path.display()))?
        else {
            return Ok(None);
        };

        for (timestamp, nearby_frame) in nearby {
            self.cache.insert(
                FrameKey {
                    frame_index: (timestamp.max(0.0) * fps).floor() as i64,
                    width,
                    height,
                },
                nearby_frame,
            );
        }
        self.cache.insert(key, Arc::clone(&frame));
        if interactive_scrub {
            self.last_scrub_decode = Some(Instant::now());
        } else {
            self.last_scrub_decode = None;
        }
        Ok(Some(frame))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ExportDecodeKey {
    frame_index: i64,
    fps_bits: u64,
    width: u32,
    height: u32,
}

impl ExportDecodeKey {
    fn new(time: f64, fps: f64, width: u32, height: u32) -> Self {
        let fps = fps.max(1.0);
        Self {
            frame_index: (time.max(0.0) * fps).floor() as i64,
            fps_bits: fps.to_bits(),
            width,
            height,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ExportDecodeRequest {
    key: ExportDecodeKey,
    time: f64,
    timeline_fps: f64,
    width: u32,
    height: u32,
}

struct ExportDecodeResponse {
    key: ExportDecodeKey,
    result: Result<Arc<VideoFrame>>,
}

struct ExportDecodeWorker {
    requests: SyncSender<ExportDecodeRequest>,
    responses: Receiver<ExportDecodeResponse>,
}

fn export_decode_lanes(path: &Path) -> usize {
    if init_ffmpeg().is_ok() {
        if let Ok(input) = format::input(path) {
            if let Some(stream) = input.streams().best(Type::Video) {
                if let Ok(context) = codec::context::Context::from_parameters(stream.parameters()) {
                    if codec_packets_are_independent(context.id()) {
                        return 2;
                    }
                }
            }
        }
    }
    1
}

fn spawn_export_decode_worker(path: PathBuf, lane: usize) -> Option<ExportDecodeWorker> {
    let (request_tx, request_rx) = mpsc::sync_channel::<ExportDecodeRequest>(2);
    let (response_tx, response_rx) = mpsc::channel::<ExportDecodeResponse>();
    let worker_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("media")
        .to_owned();
    match thread::Builder::new()
        .name(format!("kama-export-decode-{lane}-{worker_name}"))
        .spawn(move || {
            let mut decoder = BlockingVideoDecoder::new(path);
            while let Ok(request) = request_rx.recv() {
                let result = decoder
                    .frame(
                        request.time,
                        request.timeline_fps,
                        request.width,
                        request.height,
                        false,
                        None,
                    )
                    .and_then(|frame| {
                        frame.with_context(|| {
                            format!("video reached end of stream before {:.3}s", request.time)
                        })
                    });
                if let Ok(frame) = &result {
                    decoder.cache.trim_to(1, frame.byte_len().max(1));
                }
                if response_tx
                    .send(ExportDecodeResponse {
                        key: request.key,
                        result,
                    })
                    .is_err()
                {
                    break;
                }
            }
        }) {
        Ok(_) => Some(ExportDecodeWorker {
            requests: request_tx,
            responses: response_rx,
        }),
        Err(_) => None,
    }
}

pub(crate) struct ExportVideoDecoder {
    workers: Vec<ExportDecodeWorker>,
    pending: VecDeque<(ExportDecodeKey, usize)>,
    last_request: Option<(f64, f64, u32, u32)>,
    last_delta: Option<f64>,
    stable_delta_count: u8,
    prefetch_announced: bool,
    next_lane: usize,
    last_frame: Option<(ExportDecodeKey, Arc<VideoFrame>)>,
}

impl ExportVideoDecoder {
    pub(crate) fn new(path: PathBuf) -> Self {
        let requested_lanes = export_decode_lanes(&path);
        let mut workers = Vec::with_capacity(requested_lanes);
        for lane in 0..requested_lanes {
            if let Some(worker) = spawn_export_decode_worker(path.clone(), lane) {
                workers.push(worker);
            }
        }
        if workers.is_empty() {
            let (requests, request_rx) = mpsc::sync_channel(1);
            drop(request_rx);
            let (_response_tx, responses) = mpsc::channel();
            workers.push(ExportDecodeWorker {
                requests,
                responses,
            });
        }
        Self {
            workers,
            pending: VecDeque::new(),
            last_request: None,
            last_delta: None,
            stable_delta_count: 0,
            prefetch_announced: false,
            next_lane: 0,
            last_frame: None,
        }
    }

    fn choose_lane(&mut self) -> usize {
        let lane = self.next_lane % self.workers.len();
        self.next_lane = (lane + 1) % self.workers.len();
        lane
    }

    fn pending_lane(&self, key: ExportDecodeKey) -> Option<usize> {
        self.pending
            .iter()
            .find_map(|(pending, lane)| (*pending == key).then_some(*lane))
    }

    fn submit_blocking(&mut self, request: ExportDecodeRequest) -> Result<()> {
        let lane = self.choose_lane();
        self.workers[lane]
            .requests
            .send(request)
            .map_err(|_| anyhow::anyhow!("exact export decoder worker stopped"))?;
        self.pending.push_back((request.key, lane));
        Ok(())
    }

    fn try_submit_prefetch(&mut self, request: ExportDecodeRequest) {
        if self.pending_lane(request.key).is_some() {
            return;
        }
        let lane = self.choose_lane();
        match self.workers[lane].requests.try_send(request) {
            Ok(()) => {
                self.pending.push_back((request.key, lane));
                if !self.prefetch_announced {
                    self.prefetch_announced = true;
                }
            }
            Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) => {}
        }
    }

    fn receive_until(&mut self, wanted: ExportDecodeKey) -> Result<Arc<VideoFrame>> {
        let lane = self
            .pending_lane(wanted)
            .context("exact export decoder request disappeared")?;
        loop {
            let response = self.workers[lane]
                .responses
                .recv()
                .map_err(|_| anyhow::anyhow!("exact export decoder worker stopped"))?;
            if let Some(position) = self
                .pending
                .iter()
                .position(|(key, pending_lane)| *key == response.key && *pending_lane == lane)
            {
                self.pending.remove(position);
            }
            if response.key == wanted {
                return response.result;
            }
        }
    }

    pub(crate) fn frame(
        &mut self,
        time: f64,
        timeline_fps: f64,
        width: u32,
        height: u32,
    ) -> Result<Arc<VideoFrame>> {
        let fps = timeline_fps.max(1.0);
        let key = ExportDecodeKey::new(time, fps, width, height);
        if let Some((cached_key, frame)) = &self.last_frame {
            if *cached_key == key {
                return Ok(Arc::clone(frame));
            }
        }
        if self.pending_lane(key).is_none() {
            self.submit_blocking(ExportDecodeRequest {
                key,
                time,
                timeline_fps: fps,
                width,
                height,
            })?;
        }
        let frame = self.receive_until(key)?;

        let compatible_previous =
            self.last_request
                .is_some_and(|(_, previous_fps, previous_width, previous_height)| {
                    previous_fps.to_bits() == fps.to_bits()
                        && previous_width == width
                        && previous_height == height
                });
        if compatible_previous {
            let previous_time = self.last_request.expect("checked above").0;
            let delta = time - previous_time;
            let stable = delta > 0.0
                && delta <= 1.0
                && self
                    .last_delta
                    .is_some_and(|last| (last - delta).abs() <= (1.0 / fps) * 0.05);
            self.stable_delta_count = if stable {
                self.stable_delta_count.saturating_add(1)
            } else {
                0
            };
            self.last_delta = Some(delta);
            if self.stable_delta_count >= 1 {
                for ahead in 1..=self.workers.len() {
                    let next_time = time + delta * ahead as f64;
                    let next_key = ExportDecodeKey::new(next_time, fps, width, height);
                    if next_key.frame_index > key.frame_index {
                        self.try_submit_prefetch(ExportDecodeRequest {
                            key: next_key,
                            time: next_time,
                            timeline_fps: fps,
                            width,
                            height,
                        });
                    }
                }
            }
        } else {
            self.last_delta = None;
            self.stable_delta_count = 0;
        }
        self.last_request = Some((time, fps, width, height));
        self.last_frame = Some((key, Arc::clone(&frame)));
        Ok(frame)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DecodeRequestKind {
    Playback,
    Scrub,
    Preload,
}

#[derive(Clone, Copy, Debug)]
struct DecodeRequest {
    key: FrameKey,
    time: f64,
    source_fps: f64,

    source_step_seconds: f64,
    kind: DecodeRequestKind,
}

struct DecodeResponse {
    generation: u64,
    key: FrameKey,
    exact: bool,
    result: Result<Arc<VideoFrame>>,
}

fn preroll_frame_key(request: DecodeRequest, offset: usize) -> FrameKey {
    let source_fps = request.source_fps.max(1.0);
    let source_step_seconds =
        if request.source_step_seconds.is_finite() && request.source_step_seconds > 0.0 {
            request.source_step_seconds
        } else {
            1.0 / source_fps
        };
    let target_time = request.time + offset as f64 * source_step_seconds;
    FrameKey {
        frame_index: (target_time.max(0.0) * source_fps).floor() as i64,
        width: request.key.width,
        height: request.key.height,
    }
}

fn playback_request_is_covered_by_preroll(
    anchor: DecodeRequest,
    pending: DecodeRequest,
    horizon: FrameKey,
) -> bool {
    if anchor.kind != DecodeRequestKind::Playback
        || pending.kind != DecodeRequestKind::Playback
        || anchor.key.width != pending.key.width
        || anchor.key.height != pending.key.height
        || pending.key.frame_index < anchor.key.frame_index
        || pending.key.frame_index > horizon.frame_index
    {
        return false;
    }
    let fps_tolerance = anchor.source_fps.max(1.0) * 1.0e-6;
    let step_tolerance = (1.0 / anchor.source_fps.max(1.0)) * 0.02;
    (anchor.source_fps - pending.source_fps).abs() <= fps_tolerance
        && (anchor.source_step_seconds - pending.source_step_seconds).abs() <= step_tolerance
}

fn decode_retry_due(last_failed: Option<(FrameKey, Instant)>, key: FrameKey, now: Instant) -> bool {
    last_failed.is_some_and(|(failed, at)| {
        failed == key && now.saturating_duration_since(at) >= DECODE_RETRY_INTERVAL
    })
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PreviewBuildRequest {
    source_fps: f64,
    width: u32,
    height: u32,
}

#[derive(Default)]
struct PreviewCacheState {
    request: Option<PreviewBuildRequest>,
    frames: VecDeque<(f64, Arc<VideoFrame>)>,
    revision: u64,
}

struct SharedPreviewCache {
    requests: Sender<PreviewBuildRequest>,
    state: Mutex<PreviewCacheState>,
}

impl SharedPreviewCache {
    fn ensure(&self, request: PreviewBuildRequest) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.request == Some(request) {
            return;
        }
        state.request = Some(request);
        state.frames.clear();
        state.revision = state.revision.wrapping_add(1);
        drop(state);
        let _ = self.requests.send(request);
    }

    fn nearest(&self, time: f64, width: u32, height: u32) -> Option<Arc<VideoFrame>> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (_, frame) = state
            .frames
            .iter()
            .min_by(|(left, _), (right, _)| (left - time).abs().total_cmp(&(right - time).abs()))?;
        let mut display = (**frame).clone();
        let (fit_width, fit_height) = fit_size(
            display.source_width,
            display.source_height,
            width.max(1),
            height.max(1),
        );
        display.width = width.max(1);
        display.height = height.max(1);
        display.fit_width = fit_width;
        display.fit_height = fit_height;
        Some(Arc::new(display))
    }

    fn revision(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .revision
    }
}

static VIDEO_PREVIEW_CACHES: OnceLock<Mutex<HashMap<PathBuf, Arc<SharedPreviewCache>>>> =
    OnceLock::new();

fn shared_video_preview_cache(path: &Path) -> Arc<SharedPreviewCache> {
    let caches = VIDEO_PREVIEW_CACHES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut caches = caches
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(cache) = caches.get(path) {
        return Arc::clone(cache);
    }

    let (requests, request_rx) = mpsc::channel();
    let cache = Arc::new(SharedPreviewCache {
        requests,
        state: Mutex::new(PreviewCacheState::default()),
    });
    caches.insert(path.to_path_buf(), Arc::clone(&cache));
    drop(caches);

    let worker_cache = Arc::downgrade(&cache);
    let worker_path = path.to_path_buf();
    let preview_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("media")
        .to_owned();
    let _ = thread::Builder::new()
        .name(format!("kama-video-preview-{preview_name}"))
        .spawn(move || video_preview_worker(worker_path, request_rx, worker_cache));
    cache
}

pub(crate) fn warm_video_preview_cache(
    path: &Path,
    source_fps: f64,
    source_width: u32,
    source_height: u32,
) {
    shared_video_preview_cache(path).ensure(PreviewBuildRequest {
        source_fps: source_fps.max(1.0),
        width: source_width.max(1),
        height: source_height.max(1),
    });
}

pub(crate) fn retain_video_preview_caches<'a>(paths: impl IntoIterator<Item = &'a Path>) {
    let keep = paths
        .into_iter()
        .map(Path::to_path_buf)
        .collect::<std::collections::HashSet<_>>();
    if let Some(caches) = VIDEO_PREVIEW_CACHES.get() {
        caches
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|path, _| keep.contains(path));
    }
}

struct DecodeWorkerQueue {
    queue: Mutex<VecDeque<Arc<DecodeSession>>>,
    wake: Condvar,
}

impl DecodeWorkerQueue {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(VecDeque::new()),
            wake: Condvar::new(),
        })
    }

    fn enqueue(&self, session: Arc<DecodeSession>, urgent: bool) {
        let mut queue = self
            .queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if urgent {
            queue.push_front(session);
        } else {
            queue.push_back(session);
        }
        drop(queue);
        self.wake.notify_one();
    }

    fn worker_loop(&self) {
        let mut decoders: HashMap<u64, BlockingVideoDecoder> = HashMap::new();
        loop {
            let session = {
                let mut queue = self
                    .queue
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                while queue.is_empty() {
                    queue = self
                        .wake
                        .wait(queue)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                }
                queue.pop_front().expect("decode queue checked non-empty")
            };

            if session.closed.load(Ordering::Acquire) {
                decoders.remove(&session.id);
                session.finish_run();
                continue;
            }

            let decoder = decoders
                .entry(session.id)
                .or_insert_with(|| BlockingVideoDecoder::new(session.path.clone()));
            session.run(decoder);
            if session.closed.load(Ordering::Acquire) {
                decoders.remove(&session.id);
            }
        }
    }
}

struct DecodePool {
    workers: Vec<Arc<DecodeWorkerQueue>>,
    next_worker: AtomicU64,
}

impl DecodePool {
    fn new() -> Arc<Self> {
        let worker_count = thread::available_parallelism()
            .map(|count| count.get().saturating_sub(1))
            .unwrap_or(DECODE_POOL_MIN_THREADS)
            .clamp(DECODE_POOL_MIN_THREADS, DECODE_POOL_MAX_THREADS);
        let mut workers = Vec::with_capacity(worker_count);
        for index in 0..worker_count {
            let worker = DecodeWorkerQueue::new();
            let worker_thread = Arc::clone(&worker);
            if thread::Builder::new()
                .name(format!("kama-video-decode-{index}"))
                .spawn(move || worker_thread.worker_loop())
                .is_ok()
            {
                workers.push(worker);
            }
        }
        Arc::new(Self {
            workers,
            next_worker: AtomicU64::new(0),
        })
    }

    fn assign_worker(&self) -> Option<usize> {
        (!self.workers.is_empty())
            .then(|| self.next_worker.fetch_add(1, Ordering::AcqRel) as usize % self.workers.len())
    }

    fn enqueue(&self, worker: usize, session: Arc<DecodeSession>, urgent: bool) {
        if let Some(queue) = self.workers.get(worker) {
            queue.enqueue(session, urgent);
        }
    }

    fn has_workers(&self) -> bool {
        !self.workers.is_empty()
    }
}

static VIDEO_DECODE_POOL: OnceLock<Arc<DecodePool>> = OnceLock::new();
static NEXT_DECODE_SESSION_ID: AtomicU64 = AtomicU64::new(1);

fn video_decode_pool() -> &'static Arc<DecodePool> {
    VIDEO_DECODE_POOL.get_or_init(DecodePool::new)
}

struct DecodeSession {
    id: u64,
    path: PathBuf,
    worker: Option<usize>,
    pending: Mutex<Option<(u64, DecodeRequest)>>,
    responses: Sender<DecodeResponse>,
    generation: AtomicU64,
    scheduled: AtomicBool,
    closed: AtomicBool,
}

impl DecodeSession {
    fn new(path: PathBuf, responses: Sender<DecodeResponse>) -> Arc<Self> {
        let pool = video_decode_pool();
        Arc::new(Self {
            id: NEXT_DECODE_SESSION_ID.fetch_add(1, Ordering::AcqRel),
            path,
            worker: pool.assign_worker(),
            pending: Mutex::new(None),
            responses,
            generation: AtomicU64::new(1),
            scheduled: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        })
    }

    fn current_generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    fn submit(self: &Arc<Self>, request: DecodeRequest, interrupt: bool) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        let generation = if interrupt {
            self.generation.fetch_add(1, Ordering::AcqRel) + 1
        } else {
            self.current_generation()
        };
        *self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((generation, request));
        self.schedule();
    }

    fn interrupt(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        *self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }

    fn close(self: &Arc<Self>) {
        self.closed.store(true, Ordering::Release);
        self.interrupt();
        self.schedule();
    }

    fn has_work(&self) -> bool {
        self.scheduled.load(Ordering::Acquire)
            || self
                .pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_some()
    }

    fn pending_is_urgent(&self) -> bool {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .is_some_and(|(_, request)| request.kind != DecodeRequestKind::Preload)
    }

    fn schedule(self: &Arc<Self>) {
        let Some(worker) = self.worker else {
            return;
        };
        if !video_decode_pool().has_workers() {
            return;
        }
        if !self.scheduled.swap(true, Ordering::AcqRel) {
            video_decode_pool().enqueue(worker, Arc::clone(self), self.pending_is_urgent());
        }
    }

    fn take_pending(&self) -> Option<(u64, DecodeRequest)> {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }

    fn finish_run(self: &Arc<Self>) {
        self.scheduled.store(false, Ordering::Release);
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some();
        if pending {
            self.schedule();
        }
    }

    fn send_response(
        &self,
        generation: u64,
        key: FrameKey,
        exact: bool,
        result: Result<Arc<VideoFrame>>,
    ) -> bool {
        if self.closed.load(Ordering::Acquire) || self.current_generation() != generation {
            return true;
        }
        self.responses
            .send(DecodeResponse {
                generation,
                key,
                exact,
                result,
            })
            .is_ok()
    }

    fn run(self: &Arc<Self>, decoder: &mut BlockingVideoDecoder) {
        if self.closed.load(Ordering::Acquire) {
            self.finish_run();
            return;
        }

        'requests: while let Some((generation, request)) = self.take_pending() {
            if self.closed.load(Ordering::Acquire) || self.current_generation() != generation {
                continue;
            }
            let cancel = DecodeCancel {
                generation: &self.generation,
                expected: generation,
            };
            let _monitor_guard =
                (request.kind != DecodeRequestKind::Preload).then(MonitorDecodeGuard::enter);

            if request.kind == DecodeRequestKind::Scrub {
                let result = decoder.scrub_preview(
                    request.time,
                    request.source_fps,
                    request.key.width,
                    request.key.height,
                    Some(&cancel),
                );
                let (result, exact) = match result {
                    Ok(Some(preview)) => (Ok(preview.frame), preview.exact),
                    Ok(None) => (
                        Err(anyhow::anyhow!(
                            "video reached end of stream before {:.3}s",
                            request.time
                        )),
                        false,
                    ),
                    Err(error) => (Err(error), false),
                };
                let refine = !exact && result.is_ok();
                if !self.send_response(generation, request.key, exact, result) {
                    break 'requests;
                }
                if refine
                    && self.current_generation() == generation
                    && self
                        .pending
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .is_none()
                {
                    let exact_result = decoder
                        .frame(
                            request.time,
                            request.source_fps,
                            request.key.width,
                            request.key.height,
                            false,
                            Some(&cancel),
                        )
                        .and_then(|frame| {
                            frame.with_context(|| {
                                format!("video reached end of stream before {:.3}s", request.time)
                            })
                        });
                    if !self.send_response(generation, request.key, true, exact_result) {
                        break 'requests;
                    }
                }
                continue;
            }

            let result = decoder
                .frame(
                    request.time,
                    request.source_fps,
                    request.key.width,
                    request.key.height,
                    false,
                    Some(&cancel),
                )
                .and_then(|frame| {
                    frame.with_context(|| {
                        format!("video reached end of stream before {:.3}s", request.time)
                    })
                });
            let frame_bytes = result.as_ref().ok().map(|frame| frame.byte_len());
            if !self.send_response(generation, request.key, true, result) {
                break 'requests;
            }
            if self.current_generation() != generation {
                continue;
            }

            let max_preroll = match request.kind {
                DecodeRequestKind::Playback => PLAYBACK_PREROLL_FRAMES,
                DecodeRequestKind::Preload => PRELOAD_PREROLL_FRAMES,
                DecodeRequestKind::Scrub => 0,
            };
            let frame_bytes = frame_bytes.unwrap_or(FRAME_CACHE_MAX_BYTES).max(1);
            let byte_capacity = (FRAME_CACHE_MAX_BYTES / frame_bytes).max(1);
            let preroll_count = max_preroll.min(FRAME_CACHE_CAPACITY).min(byte_capacity);
            let source_fps = request.source_fps.max(1.0);
            let horizon = preroll_frame_key(request, preroll_count);
            let mut previous_key = request.key;
            for offset in 1..=preroll_count {
                if self.closed.load(Ordering::Acquire) || self.current_generation() != generation {
                    continue 'requests;
                }

                let pending = self
                    .pending
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .as_ref()
                    .copied();
                if let Some((pending_generation, pending_request)) = pending {
                    if pending_generation != generation
                        || !playback_request_is_covered_by_preroll(
                            request,
                            pending_request,
                            horizon,
                        )
                    {
                        continue 'requests;
                    }
                }
                let key = preroll_frame_key(request, offset);
                if key == previous_key {
                    continue;
                }
                previous_key = key;
                let decode_time = key.frame_index as f64 / source_fps;
                let result = decoder.frame(
                    decode_time,
                    source_fps,
                    key.width,
                    key.height,
                    false,
                    Some(&cancel),
                );
                let Ok(Some(frame)) = result else {
                    break;
                };
                if !self.send_response(generation, key, true, Ok(frame)) {
                    break 'requests;
                }
            }
        }
        self.finish_run();
    }
}

pub struct VideoDecoder {
    session: Arc<DecodeSession>,
    responses: Receiver<DecodeResponse>,
    preview_cache: Arc<SharedPreviewCache>,
    preview_revision: u64,
    cache: FrameCache,
    last_submitted: Option<(FrameKey, DecodeRequestKind)>,
    last_error: Option<String>,
    last_failed: Option<(FrameKey, Instant)>,
    last_display_request: Option<FrameKey>,
    last_display_scrubbing: bool,
    last_presented: Option<(FrameKey, Arc<VideoFrame>)>,
    scrub_preview: Option<(FrameKey, Arc<VideoFrame>)>,
    hardware_policy_epoch: u64,
}

impl VideoDecoder {
    pub fn new(path: PathBuf) -> Self {
        let (response_tx, responses) = mpsc::channel();
        let session = DecodeSession::new(path.clone(), response_tx);
        let preview_cache = shared_video_preview_cache(&path);
        let preview_revision = preview_cache.revision();
        Self {
            session,
            responses,
            preview_cache,
            preview_revision,
            cache: FrameCache::default(),
            last_submitted: None,
            last_error: None,
            last_failed: None,
            last_display_request: None,
            last_display_scrubbing: false,
            last_presented: None,
            scrub_preview: None,
            hardware_policy_epoch: hardware_decode_policy_epoch(),
        }
    }

    fn sync_decode_policy(&mut self) {
        let epoch = hardware_decode_policy_epoch();
        if self.hardware_policy_epoch == epoch {
            return;
        }
        self.hardware_policy_epoch = epoch;
        self.session.interrupt();
        self.cache = FrameCache::default();
        self.last_submitted = None;
        self.last_failed = None;
        self.last_presented = None;
        self.scrub_preview = None;
    }

    pub fn poll_completed(&mut self) -> bool {
        let mut changed = false;
        let generation = self.session.current_generation();
        loop {
            let response = match self.responses.try_recv() {
                Ok(response) => response,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break,
            };
            if response.generation != generation {
                continue;
            }
            match response.result {
                Ok(frame) => {
                    let advances_visible_playback = response.exact
                        && self.last_display_request.is_some_and(|display| {
                            display.width == response.key.width
                                && display.height == response.key.height
                                && response.key.frame_index <= display.frame_index
                        })
                        && self.last_presented.as_ref().is_none_or(|(presented, _)| {
                            presented.width != response.key.width
                                || presented.height != response.key.height
                                || response.key.frame_index > presented.frame_index
                        });
                    if response.exact {
                        self.cache.insert(response.key, frame);
                    } else if self.last_display_request == Some(response.key) {
                        self.scrub_preview = Some((response.key, frame));
                    }
                    self.last_error = None;
                    if self
                        .last_failed
                        .is_some_and(|(failed, _)| failed == response.key)
                    {
                        self.last_failed = None;
                    }
                    changed |= self.last_display_request == Some(response.key)
                        || advances_visible_playback;
                }
                Err(error) => {
                    let message = format!("{error:#}");
                    self.last_failed = Some((response.key, Instant::now()));
                    if self
                        .last_submitted
                        .is_some_and(|(submitted, _)| submitted == response.key)
                    {
                        self.last_submitted = None;
                    }
                    if self.last_error.as_deref() != Some(&message) {
                        self.last_error = Some(message);
                    }
                }
            }
        }
        let preview_revision = self.preview_cache.revision();
        if preview_revision != self.preview_revision {
            self.preview_revision = preview_revision;
            changed |= self.last_display_scrubbing;
        }
        changed
    }

    fn nearest_preview(&self, time: f64, width: u32, height: u32) -> Option<Arc<VideoFrame>> {
        self.preview_cache.nearest(time, width, height)
    }

    pub fn frame(
        &mut self,
        time: f64,
        source_fps: f64,
        source_step_seconds: f64,
        width: u32,
        height: u32,
        interactive_scrub: bool,
    ) -> (Option<Arc<VideoFrame>>, bool) {
        self.sync_decode_policy();
        self.poll_completed();
        let source_fps = source_fps.max(1.0);
        let request_fps = if interactive_scrub {
            source_fps.clamp(1.0, SCRUB_PREVIEW_FPS)
        } else {
            source_fps
        };
        let request_index = (time.max(0.0) * request_fps).floor() as i64;
        let target_time = request_index as f64 / request_fps;
        let key = FrameKey {
            frame_index: (target_time * source_fps).floor() as i64,
            width,
            height,
        };
        if self.last_failed.is_some_and(|(failed, _)| failed != key) {
            self.last_failed = None;
        }

        let previous = self.last_display_request;
        let entering_scrub = !self.last_display_scrubbing && interactive_scrub;
        let leaving_scrub = self.last_display_scrubbing && !interactive_scrub;
        let expected_advance = (source_step_seconds.max(0.0) * source_fps).max(1.0);
        let continuous_forward = !interactive_scrub
            && previous.is_some_and(|previous| {
                previous.width == key.width
                    && previous.height == key.height
                    && key.frame_index >= previous.frame_index
                    && (key.frame_index - previous.frame_index) as f64
                        <= expected_advance * 4.0 + 3.0
            });
        let discontinuity =
            previous.is_some() && !interactive_scrub && !leaving_scrub && !continuous_forward;
        let hard_interrupt = entering_scrub
            || leaving_scrub
            || discontinuity
            || (interactive_scrub && previous != Some(key));
        if hard_interrupt {
            self.session.interrupt();
            self.last_submitted = None;
            if entering_scrub || discontinuity {
                self.last_presented = None;
            }
        }
        self.last_display_request = Some(key);
        self.last_display_scrubbing = interactive_scrub;

        let kind = if interactive_scrub {
            DecodeRequestKind::Scrub
        } else {
            DecodeRequestKind::Playback
        };
        let cached_frame = self.cache.get(key);

        let retry_due = decode_retry_due(self.last_failed, key, Instant::now());
        let retry_waiting = self
            .last_failed
            .is_some_and(|(failed, _)| failed == key && !retry_due);
        if cached_frame.is_none()
            && !self.session.has_work()
            && self.last_submitted == Some((key, kind))
        {
            self.last_submitted = None;
        }
        let should_submit = (self.last_failed.is_none() || retry_due)
            && self.last_submitted != Some((key, kind))
            && (kind != DecodeRequestKind::Playback
                || cached_frame.is_none()
                || !self.session.has_work());
        if should_submit {
            self.last_submitted = Some((key, kind));
            self.session.submit(
                DecodeRequest {
                    key,
                    time: target_time,
                    source_fps,
                    source_step_seconds,
                    kind,
                },
                false,
            );
        }

        if let Some(frame) = cached_frame {
            if !interactive_scrub {
                self.scrub_preview = None;
                self.last_presented = Some((key, Arc::clone(&frame)));
            }
            return (Some(frame), false);
        }

        if interactive_scrub {
            if let Some((preview_key, preview)) = self.scrub_preview.as_ref() {
                if *preview_key == key {
                    return (
                        Some(Arc::clone(preview)),
                        self.session.has_work() || retry_waiting,
                    );
                }
            }
            if let Some(preview) = self.nearest_preview(target_time, width, height) {
                return (Some(preview), self.session.has_work() || retry_waiting);
            }
        } else if leaving_scrub {
            if let Some((preview_key, preview)) = self.scrub_preview.as_ref() {
                if preview_key.width == width && preview_key.height == height {
                    return (
                        Some(Arc::clone(preview)),
                        self.session.has_work() || retry_waiting,
                    );
                }
            }
        }

        let held = self
            .cache
            .iter()
            .filter(|(cached, _)| {
                cached.width == width
                    && cached.height == height
                    && cached.frame_index <= key.frame_index
            })
            .max_by_key(|(cached, _)| cached.frame_index)
            .map(|(cached, frame)| (*cached, Arc::clone(frame)))
            .or_else(|| {
                self.cache
                    .iter()
                    .filter(|(cached, _)| cached.width == width && cached.height == height)
                    .min_by_key(|(cached, _)| (cached.frame_index - key.frame_index).abs())
                    .map(|(cached, frame)| (*cached, Arc::clone(frame)))
            });
        let presented = if let Some((candidate_key, candidate)) = held {
            if continuous_forward {
                let previous = self
                    .last_presented
                    .as_ref()
                    .map(|(previous_key, previous)| (*previous_key, Arc::clone(previous)));
                if let Some((previous_key, previous)) = previous {
                    if candidate_key.frame_index < previous_key.frame_index {
                        Some(previous)
                    } else {
                        self.last_presented = Some((candidate_key, Arc::clone(&candidate)));
                        Some(candidate)
                    }
                } else {
                    self.last_presented = Some((candidate_key, Arc::clone(&candidate)));
                    Some(candidate)
                }
            } else {
                if !interactive_scrub {
                    self.last_presented = Some((candidate_key, Arc::clone(&candidate)));
                }
                Some(candidate)
            }
        } else if continuous_forward {
            self.last_presented
                .as_ref()
                .map(|(_, frame)| Arc::clone(frame))
        } else {
            None
        };
        (presented, self.session.has_work() || retry_waiting)
    }

    pub fn preload(
        &mut self,
        time: f64,
        source_fps: f64,
        source_step_seconds: f64,
        width: u32,
        height: u32,
    ) {
        self.sync_decode_policy();
        self.poll_completed();
        let source_fps = source_fps.max(1.0);
        let frame_index = (time.max(0.0) * source_fps).floor() as i64;
        let key = FrameKey {
            frame_index,
            width,
            height,
        };
        if self.cache.contains(key)
            || self.last_submitted == Some((key, DecodeRequestKind::Preload))
        {
            return;
        }
        self.last_submitted = Some((key, DecodeRequestKind::Preload));
        self.session.submit(
            DecodeRequest {
                key,
                time: frame_index as f64 / source_fps,
                source_fps,
                source_step_seconds,
                kind: DecodeRequestKind::Preload,
            },
            false,
        );
    }
}

impl Drop for VideoDecoder {
    fn drop(&mut self) {
        self.session.close();
    }
}

fn scrub_thumbnail_dimensions(width: u32, height: u32) -> (u32, u32) {
    let width = width.max(1);
    let height = height.max(1);
    let largest = width.max(height);
    if largest <= SCRUB_THUMBNAIL_MAX_EDGE {
        return (width, height);
    }
    let scale = SCRUB_THUMBNAIL_MAX_EDGE as f64 / largest as f64;
    (
        (width as f64 * scale).round().max(1.0) as u32,
        (height as f64 * scale).round().max(1.0) as u32,
    )
}

fn video_preview_worker(
    path: PathBuf,
    requests: Receiver<PreviewBuildRequest>,
    cache: Weak<SharedPreviewCache>,
) {
    let duration = probe_av_media(&path).ok().and_then(|probe| probe.duration);

    let mut decoder = BlockingVideoDecoder::new_preview(path.clone());
    let mut request = match requests.recv() {
        Ok(request) => request,
        Err(_) => return,
    };
    loop {
        let (preview_width, preview_height) =
            scrub_thumbnail_dimensions(request.width, request.height);
        let interval = duration
            .map(|duration| {
                SCRUB_THUMBNAIL_INTERVAL_SECONDS
                    .max(duration / (SCRUB_THUMBNAIL_CAPACITY.saturating_sub(1).max(1) as f64))
            })
            .unwrap_or(SCRUB_THUMBNAIL_INTERVAL_SECONDS);
        let mut time = 0.0;
        loop {
            if offline_export_has_priority() || monitor_decode_active() {
                match requests.recv_timeout(Duration::from_millis(12)) {
                    Ok(new_request) => request = new_request,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
                continue;
            }
            match requests.try_recv() {
                Ok(new_request) if new_request != request => {
                    request = new_request;
                    break;
                }
                Ok(_) | Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => return,
            }
            if duration.is_some_and(|duration| time > duration + 1.0e-6) {
                match requests.recv() {
                    Ok(new_request) => request = new_request,
                    Err(_) => return,
                }
                break;
            }
            let frame = match decoder.frame(
                time,
                request.source_fps,
                preview_width,
                preview_height,
                false,
                None,
            ) {
                Ok(Some(frame)) => frame,
                Ok(None) => {
                    match requests.recv() {
                        Ok(new_request) => request = new_request,
                        Err(_) => return,
                    }
                    break;
                }
                Err(_) => {
                    return;
                }
            };
            let Some(cache) = cache.upgrade() else {
                return;
            };
            let mut state = cache
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.request == Some(request) {
                if let Some(index) = state
                    .frames
                    .iter()
                    .position(|(cached_time, _)| (*cached_time - time).abs() < 1.0e-6)
                {
                    state.frames.remove(index);
                }
                state.frames.push_back((time, frame));
                while state.frames.len() > SCRUB_THUMBNAIL_CAPACITY {
                    state.frames.pop_front();
                }
                state.revision = state.revision.wrapping_add(1);
            }
            drop(state);
            drop(cache);
            time += interval;
        }
    }
}

fn fit_size(
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
) -> (u32, u32) {
    if source_width == 0 || source_height == 0 || target_width == 0 || target_height == 0 {
        return (target_width.max(1), target_height.max(1));
    }
    let scale = (target_width as f64 / source_width as f64)
        .min(target_height as f64 / source_height as f64);
    let width = (source_width as f64 * scale)
        .round()
        .clamp(1.0, target_width as f64) as u32;
    let height = (source_height as f64 * scale)
        .round()
        .clamp(1.0, target_height as f64) as u32;
    (width, height)
}

struct HardwareDecodeSelection {
    device_name: String,
    pixel_format: ffmpeg::ffi::AVPixelFormat,

    rejected: AtomicBool,
}

unsafe extern "C" fn prefer_hardware_format(
    context: *mut ffmpeg::ffi::AVCodecContext,
    formats: *const ffmpeg::ffi::AVPixelFormat,
) -> ffmpeg::ffi::AVPixelFormat {
    if context.is_null() || formats.is_null() || (*context).opaque.is_null() {
        return ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_NONE;
    }

    let selection = &*((*context).opaque as *const HardwareDecodeSelection);
    let mut cursor = formats;
    let mut software_fallback = ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_NONE;
    while (*cursor as i32) != -1 {
        if *cursor == selection.pixel_format {
            return *cursor;
        }

        if (software_fallback as i32) < 0 {
            let descriptor = ffmpeg::ffi::av_pix_fmt_desc_get(*cursor);
            if !descriptor.is_null() && ((*descriptor).flags & (1 << 3)) == 0 {
                software_fallback = *cursor;
            }
        }
        cursor = cursor.add(1);
    }

    selection.rejected.store(true, Ordering::Release);
    software_fallback
}

fn linux_vaapi_candidates() -> Vec<String> {
    let mut render_nodes = std::fs::read_dir("/dev/dri")
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            name.starts_with("renderD")
                .then(|| format!("vaapi@{}", entry.path().display()))
        })
        .collect::<Vec<_>>();
    render_nodes.sort();
    if render_nodes.is_empty() {
        render_nodes.push("vaapi".into());
    }
    render_nodes
}

fn hardware_decode_names() -> Vec<String> {
    if cfg!(target_os = "macos") {
        vec!["videotoolbox".into()]
    } else if cfg!(target_os = "windows") {
        vec![
            "d3d11va".into(),
            "dxva2".into(),
            "qsv".into(),
            "cuda".into(),
        ]
    } else if cfg!(target_os = "linux") {
        let mut candidates = linux_vaapi_candidates();
        candidates.extend(["vulkan".into(), "qsv".into(), "cuda".into()]);
        candidates
    } else {
        vec!["vulkan".into(), "cuda".into(), "vaapi".into()]
    }
}

fn try_enable_hardware_decode(
    context: &mut codec::context::Context,
    names: &[String],
) -> Option<Box<HardwareDecodeSelection>> {
    const METHOD_HW_DEVICE_CTX: i32 = 0x01;
    if names.is_empty() {
        return None;
    }

    unsafe {
        let context_ptr = context.as_mut_ptr();
        let codec_ptr = ffmpeg::ffi::avcodec_find_decoder((*context_ptr).codec_id);
        if codec_ptr.is_null() {
            return None;
        }

        for candidate in names {
            let (name, candidate_device) = candidate
                .split_once('@')
                .map_or((candidate.as_str(), None), |(name, device)| {
                    (name, Some(device))
                });
            let c_name = match CString::new(name) {
                Ok(value) => value,
                Err(_) => continue,
            };
            let device_type = ffmpeg::ffi::av_hwdevice_find_type_by_name(c_name.as_ptr());
            if (device_type as i32) == 0 {
                continue;
            }

            let mut hw_pixel_format = None;
            let mut index = 0;
            loop {
                let config = ffmpeg::ffi::avcodec_get_hw_config(codec_ptr, index);
                if config.is_null() {
                    break;
                }
                if ((*config).methods & METHOD_HW_DEVICE_CTX) != 0
                    && (*config).device_type == device_type
                {
                    hw_pixel_format = Some((*config).pix_fmt);
                    break;
                }
                index += 1;
            }
            let Some(hw_pixel_format) = hw_pixel_format else {
                continue;
            };

            let explicit_device = if name == "vaapi" {
                candidate_device
                    .map(str::to_owned)
                    .and_then(|value| CString::new(value).ok())
            } else {
                None
            };
            let device_ptr = explicit_device
                .as_ref()
                .map_or(std::ptr::null(), |value| value.as_ptr());

            let mut device = std::ptr::null_mut();
            let create_result = ffmpeg::ffi::av_hwdevice_ctx_create(
                &mut device,
                device_type,
                device_ptr,
                std::ptr::null_mut(),
                0,
            );
            if create_result < 0 {
                continue;
            }

            let device_ref = ffmpeg::ffi::av_buffer_ref(device);
            ffmpeg::ffi::av_buffer_unref(&mut device);
            if device_ref.is_null() {
                continue;
            }

            let mut selection = Box::new(HardwareDecodeSelection {
                device_name: candidate.to_string(),
                pixel_format: hw_pixel_format,
                rejected: AtomicBool::new(false),
            });
            (*context_ptr).opaque =
                (&mut *selection as *mut HardwareDecodeSelection).cast::<c_void>();
            (*context_ptr).hw_device_ctx = device_ref;
            (*context_ptr).get_format = Some(prefer_hardware_format);
            return Some(selection);
        }
    }
    None
}

fn is_hardware_frame(frame: &Video) -> bool {
    unsafe {
        let raw: ffmpeg::ffi::AVPixelFormat = frame.format().into();
        let descriptor = ffmpeg::ffi::av_pix_fmt_desc_get(raw);
        !descriptor.is_null() && ((*descriptor).flags & (1 << 3)) != 0
    }
}

fn observe_hardware_frame(
    frame: &Video,
    seen: &mut bool,
    rejected: Option<&AtomicBool>,
    backend: Option<&str>,
) -> Result<()> {
    if is_hardware_frame(frame) {
        if !*seen {
            if let Some(_backend) = backend {}
        }
        *seen = true;
    } else if rejected.is_some_and(|flag| flag.load(Ordering::Acquire)) && !*seen {
        bail!("hardware decoder rejected its surface format; reopening with the next backend");
    }
    Ok(())
}

fn transfer_code(transfer: color::TransferCharacteristic) -> u32 {
    match transfer {
        color::TransferCharacteristic::Linear => 0,
        color::TransferCharacteristic::IEC61966_2_1 => 1,
        color::TransferCharacteristic::GAMMA22 => 2,
        color::TransferCharacteristic::GAMMA28 => 3,
        color::TransferCharacteristic::SMPTE2084 => 4,
        color::TransferCharacteristic::ARIB_STD_B67 => 5,
        _ => 6,
    }
}

fn configure_scaler_color(scaler: &mut ScalingContext, frame: &Video) {
    let coefficients = match frame.color_space() {
        color::Space::BT709 => 1,
        color::Space::FCC => 4,
        color::Space::SMPTE240M => 7,
        color::Space::BT2020NCL | color::Space::BT2020CL => 9,

        _ if frame.height() > 576 || frame.width() >= 1280 => 1,
        _ => 5,
    };
    let source_full_range = i32::from(frame.color_range() == color::Range::JPEG);
    unsafe {
        let table = ffmpeg::ffi::sws_getCoefficients(coefficients);
        if !table.is_null() {
            let _ = ffmpeg::ffi::sws_setColorspaceDetails(
                scaler.as_mut_ptr(),
                table,
                source_full_range,
                table,
                1,
                0,
                1 << 16,
                1 << 16,
            );
        }
    }
}

#[cfg(test)]
mod decode_scheduler_tests {
    use super::*;

    fn playback_request(
        frame_index: i64,
        source_fps: f64,
        source_step_seconds: f64,
    ) -> DecodeRequest {
        DecodeRequest {
            key: FrameKey {
                frame_index,
                width: 1920,
                height: 1080,
            },
            time: frame_index as f64 / source_fps,
            source_fps,
            source_step_seconds,
            kind: DecodeRequestKind::Playback,
        }
    }

    #[test]
    fn exact_duration_targets_the_final_displayable_frame() {
        let duration = 13.792;
        let target = clamp_decode_target(duration, Some(duration), 1.0 / 24.0);
        assert!(target < duration);
        assert!((target - (duration - 1.0 / 24.0)).abs() < 1.0e-9);
    }

    #[test]
    fn two_x_preroll_targets_only_displayed_source_frames() {
        let request = playback_request(100, 30.0, 2.0 / 30.0);
        let frames = (1..=4)
            .map(|offset| preroll_frame_key(request, offset).frame_index)
            .collect::<Vec<_>>();
        assert_eq!(frames, vec![102, 104, 106, 108]);
    }

    #[test]
    fn fractional_speed_preroll_preserves_alternating_stride() {
        let request = playback_request(100, 30.0, 1.5 / 30.0);
        let frames = (1..=6)
            .map(|offset| preroll_frame_key(request, offset).frame_index)
            .collect::<Vec<_>>();
        assert_eq!(frames, vec![101, 103, 104, 106, 107, 109]);
    }

    #[test]
    fn mixed_source_and_timeline_rates_use_source_time_step() {
        let request = playback_request(120, 60.0, 1.0 / 24.0);
        let frames = (1..=4)
            .map(|offset| preroll_frame_key(request, offset).frame_index)
            .collect::<Vec<_>>();
        assert_eq!(frames, vec![122, 125, 127, 130]);
    }

    #[test]
    fn nearby_playback_ticks_do_not_cancel_existing_preroll() {
        let anchor = playback_request(100, 30.0, 2.0 / 30.0);
        let horizon = preroll_frame_key(anchor, PLAYBACK_PREROLL_FRAMES);
        assert!(playback_request_is_covered_by_preroll(
            anchor,
            playback_request(124, 30.0, 2.0 / 30.0),
            horizon,
        ));
    }

    #[test]
    fn preroll_yields_for_rate_changes_and_far_catch_up() {
        let anchor = playback_request(100, 30.0, 2.0 / 30.0);
        let horizon = preroll_frame_key(anchor, PLAYBACK_PREROLL_FRAMES);
        assert!(!playback_request_is_covered_by_preroll(
            anchor,
            playback_request(124, 30.0, 1.5 / 30.0),
            horizon,
        ));
        assert!(!playback_request_is_covered_by_preroll(
            anchor,
            playback_request(horizon.frame_index + 2, 30.0, 2.0 / 30.0),
            horizon,
        ));
    }

    #[test]
    fn failed_decode_becomes_retryable_after_backoff() {
        let key = playback_request(100, 30.0, 1.0 / 30.0).key;
        let now = Instant::now();
        assert!(!decode_retry_due(Some((key, now)), key, now));
        assert!(decode_retry_due(
            Some((key, now)),
            key,
            now + DECODE_RETRY_INTERVAL,
        ));
    }
}
