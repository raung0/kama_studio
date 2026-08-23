use std::{
    collections::{HashMap, HashSet},
    fs::File,
    path::{Path, PathBuf},
    sync::{
        mpsc::{self, Receiver, Sender},
        Arc,
    },
    thread,
};

use anyhow::{Context, Result};
use ffmpeg::{
    codec, format,
    media::Type,
    software::scaling::{context::Context as ScalingContext, flag::Flags as ScalingFlags},
    util::{format::pixel::Pixel, frame::video::Video},
};
use ffmpeg_next as ffmpeg;
pub use kama_editor_core::document::{AudioWaveform, VideoWaveform, WaveformData};
use kama_ui::{Renderer, TextureId};
use rodio::{Decoder, Source};

use crate::project::{MediaAsset, MediaId, MediaKind, Project};

const ANALYSIS_FPS: f64 = 24.0;
const VIDEO_ANALYSIS_WIDTH: u32 = 96;
const TEXTURE_SEGMENT_WIDTH: usize = 256;
const AUDIO_BANDS: usize = 6;

#[derive(Clone, Copy, Debug)]
pub struct WaveformSegment {
    pub texture: TextureId,
    pub sample_start: usize,
    pub sample_end: usize,
}

#[derive(Clone, Debug)]
pub struct WaveformTexture {
    pub sample_count: usize,
    pub segments: Vec<WaveformSegment>,
    pub video_y: Option<[f32; 2]>,
    pub audio_y: Option<[f32; 2]>,
}

struct AnalysisJob {
    epoch: u64,
    asset: MediaAsset,
}

struct AnalysisResult {
    epoch: u64,
    media: MediaId,
    path: PathBuf,
    waveform: Option<WaveformData>,
}

pub struct WaveformTextures {
    textures: HashMap<MediaId, WaveformTexture>,
    pending: HashSet<(MediaId, PathBuf)>,
    request_tx: Sender<AnalysisJob>,
    result_rx: Receiver<AnalysisResult>,
    epoch: u64,
}

impl Default for WaveformTextures {
    fn default() -> Self {
        let (request_tx, request_rx) = mpsc::channel::<AnalysisJob>();
        let (result_tx, result_rx) = mpsc::channel::<AnalysisResult>();
        let _ = thread::Builder::new()
            .name("kama-waveform-analysis".into())
            .spawn(move || {
                while let Ok(job) = request_rx.recv() {
                    let waveform = analyze_media(&job.asset);
                    if result_tx
                        .send(AnalysisResult {
                            epoch: job.epoch,
                            media: job.asset.id,
                            path: job.asset.path,
                            waveform,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            });
        Self {
            textures: HashMap::new(),
            pending: HashSet::new(),
            request_tx,
            result_rx,
            epoch: 0,
        }
    }
}

impl WaveformTextures {
    pub fn clear(&mut self) {
        self.textures.clear();
        self.pending.clear();
        self.epoch = self.epoch.wrapping_add(1);
    }

    pub fn get(&self, media: MediaId) -> Option<&WaveformTexture> {
        self.textures.get(&media)
    }

    pub fn is_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn queue_asset(&mut self, asset: &MediaAsset) {
        if !matches!(asset.kind, MediaKind::Video | MediaKind::Audio) || asset.waveform.is_some() {
            return;
        }
        let key = (asset.id, asset.path.clone());
        if !self.pending.insert(key.clone()) {
            return;
        }
        if self
            .request_tx
            .send(AnalysisJob {
                epoch: self.epoch,
                asset: asset.clone(),
            })
            .is_err()
        {
            self.pending.remove(&key);
        }
    }

    pub fn queue_missing(&mut self, project: &Project) {
        for asset in &project.media {
            self.queue_asset(asset);
        }
    }

    pub fn poll(&mut self, project: &mut Project) -> bool {
        let mut changed = false;
        while let Ok(result) = self.result_rx.try_recv() {
            self.pending.remove(&(result.media, result.path.clone()));
            if result.epoch != self.epoch {
                continue;
            }
            let Some(asset) = project
                .media
                .iter_mut()
                .find(|asset| asset.id == result.media && asset.path == result.path)
            else {
                continue;
            };
            asset.waveform = result.waveform.map(Arc::new);
            changed = true;
        }
        changed
    }

    pub fn sync(&mut self, renderer: &mut Renderer, project: &Project) -> Result<()> {
        for asset in &project.media {
            if self.textures.contains_key(&asset.id) {
                continue;
            }
            let Some(data) = asset.waveform.as_ref() else {
                continue;
            };
            let sample_count = data.sample_count();
            if sample_count == 0 {
                continue;
            }
            let texture_rows =
                usize::from(data.video.is_some()) + usize::from(data.audio.is_some()) * 2;
            let row_uv = |row: usize| {
                if texture_rows <= 1 {
                    0.0
                } else {
                    row as f32 / (texture_rows - 1) as f32
                }
            };
            let video_y = data.video.as_ref().map(|_| [row_uv(0); 2]);
            let audio_y = data.audio.as_ref().map(|_| {
                let row = usize::from(data.video.is_some());
                [row_uv(row), row_uv(row + 1)]
            });
            let mut segments = Vec::new();
            for sample_start in (0..sample_count).step_by(TEXTURE_SEGMENT_WIDTH) {
                let sample_end = (sample_start + TEXTURE_SEGMENT_WIDTH).min(sample_count);
                let (width, height, pixels) =
                    waveform_segment_pixels(data, sample_start, sample_end);
                let texture = renderer
                    .register_texture_rgba8(width, height, &pixels)
                    .with_context(|| format!("upload waveform texture for {}", asset.name))?;
                segments.push(WaveformSegment {
                    texture,
                    sample_start,
                    sample_end,
                });
            }
            self.textures.insert(
                asset.id,
                WaveformTexture {
                    sample_count,
                    segments,
                    video_y,
                    audio_y,
                },
            );
        }
        Ok(())
    }
}

pub fn analyze_media(asset: &MediaAsset) -> Option<WaveformData> {
    let video = matches!(asset.kind, MediaKind::Video)
        .then(|| analyze_video(&asset.path))
        .transpose()
        .ok()
        .flatten();
    let has_audio = asset.has_audio || matches!(asset.kind, MediaKind::Audio);
    let target_samples = asset
        .duration
        .map(|duration| (duration * ANALYSIS_FPS).ceil().max(1.0) as usize)
        .or_else(|| video.as_ref().map(|video| video.activity.len()));
    let audio = has_audio
        .then(|| analyze_audio(&asset.path, target_samples))
        .transpose()
        .ok()
        .flatten();
    (video.is_some() || audio.is_some()).then_some(WaveformData { video, audio })
}

fn analyze_video(path: &Path) -> Result<VideoWaveform> {
    crate::runtime::media::init_ffmpeg()?;
    let mut input =
        format::input(path).with_context(|| format!("open video {}", path.display()))?;
    let stream = input
        .streams()
        .best(Type::Video)
        .context("media has no video stream")?;
    let stream_index = stream.index();
    let time_base = stream.time_base();
    let source_rate = {
        let rate = f64::from(stream.avg_frame_rate());
        if rate.is_finite() && rate > 0.0 {
            rate
        } else {
            30.0
        }
    };
    let mut context = codec::context::Context::from_parameters(stream.parameters())
        .context("create waveform decoder context")?;
    context.set_threading(codec::threading::Config {
        kind: codec::threading::Type::Frame,
        count: thread::available_parallelism().map_or(4, |count| count.get().min(8)),
    });
    let mut decoder = context
        .decoder()
        .video()
        .context("open waveform video decoder")?;
    let source_width = decoder.width().max(1);
    let source_height = decoder.height().max(1);
    let analysis_height =
        ((source_height as f64 * VIDEO_ANALYSIS_WIDTH as f64 / source_width as f64).round() as u32)
            .max(1);
    let mut scaler = ScalingContext::get(
        decoder.format(),
        source_width,
        source_height,
        Pixel::RGB24,
        VIDEO_ANALYSIS_WIDTH,
        analysis_height,
        ScalingFlags::BILINEAR,
    )
    .context("create waveform scaler")?;

    let target_fps = ANALYSIS_FPS.min(source_rate);
    let sample_interval = 1.0 / target_fps.max(1.0);
    let mut colors = Vec::new();
    let mut raw_activity = Vec::new();
    let mut previous_gray = Vec::<u8>::new();
    let mut next_sample_time = 0.0;

    for (packet_stream, packet) in input.packets() {
        if packet_stream.index() != stream_index {
            continue;
        }
        decoder
            .send_packet(&packet)
            .context("send waveform packet")?;
        receive_video_samples(
            &mut decoder,
            &mut scaler,
            time_base,
            analysis_height,
            &mut next_sample_time,
            sample_interval,
            &mut previous_gray,
            &mut colors,
            &mut raw_activity,
        )?;
    }
    decoder.send_eof().ok();
    receive_video_samples(
        &mut decoder,
        &mut scaler,
        time_base,
        analysis_height,
        &mut next_sample_time,
        sample_interval,
        &mut previous_gray,
        &mut colors,
        &mut raw_activity,
    )?;
    anyhow::ensure!(!colors.is_empty(), "no video frames decoded for waveform");

    Ok(VideoWaveform {
        colors,
        activity: normalize_activity(&raw_activity, 0.40, 95.0, true),
    })
}

#[allow(clippy::too_many_arguments)]
fn receive_video_samples(
    decoder: &mut ffmpeg::decoder::Video,
    scaler: &mut ScalingContext,
    time_base: ffmpeg::Rational,
    analysis_height: u32,
    next_sample_time: &mut f64,
    sample_interval: f64,
    previous_gray: &mut Vec<u8>,
    colors: &mut Vec<[u8; 3]>,
    raw_activity: &mut Vec<f32>,
) -> Result<()> {
    let mut frame = Video::empty();
    while decoder.receive_frame(&mut frame).is_ok() {
        let timestamp = frame
            .timestamp()
            .or_else(|| frame.pts())
            .map(|pts| pts as f64 * f64::from(time_base))
            .unwrap_or(*next_sample_time);
        if timestamp + 1.0e-9 < *next_sample_time {
            continue;
        }
        *next_sample_time += sample_interval;
        if timestamp > *next_sample_time + sample_interval {
            *next_sample_time = timestamp + sample_interval;
        }

        let mut rgb = Video::empty();
        scaler
            .run(&frame, &mut rgb)
            .context("scale waveform frame")?;
        let row_bytes = VIDEO_ANALYSIS_WIDTH as usize * 3;
        let stride = rgb.stride(0);
        let source = rgb.data(0);
        let mut packed = Vec::with_capacity(row_bytes * analysis_height as usize);
        for y in 0..analysis_height as usize {
            let start = y * stride;
            if start + row_bytes <= source.len() {
                packed.extend_from_slice(&source[start..start + row_bytes]);
            }
        }
        if packed.is_empty() {
            continue;
        }
        colors.push(representative_lab_rgb(&packed));
        let gray = packed
            .chunks_exact(3)
            .map(|pixel| {
                (pixel[0] as f32 * 0.299 + pixel[1] as f32 * 0.587 + pixel[2] as f32 * 0.114)
                    .round() as u8
            })
            .collect::<Vec<_>>();
        raw_activity.push(lucas_kanade_flow_score(
            previous_gray,
            &gray,
            VIDEO_ANALYSIS_WIDTH as usize,
            analysis_height as usize,
        ));
        *previous_gray = gray;
    }
    Ok(())
}

fn lucas_kanade_flow_score(previous: &[u8], current: &[u8], width: usize, height: usize) -> f32 {
    if previous.len() != current.len() || width < 7 || height < 7 {
        return 0.0;
    }
    let mut magnitudes = Vec::with_capacity(width * height / 9);
    for cy in (3..height - 3).step_by(3) {
        for cx in (3..width - 3).step_by(3) {
            let mut xx = 0.0;
            let mut yy = 0.0;
            let mut xy = 0.0;
            let mut xt = 0.0;
            let mut yt = 0.0;
            for y in cy - 2..=cy + 2 {
                for x in cx - 2..=cx + 2 {
                    let i = y * width + x;
                    let ix = (current[i + 1] as f32 - current[i - 1] as f32
                        + previous[i + 1] as f32
                        - previous[i - 1] as f32)
                        * 0.25;
                    let iy = (current[i + width] as f32 - current[i - width] as f32
                        + previous[i + width] as f32
                        - previous[i - width] as f32)
                        * 0.25;
                    let it = current[i] as f32 - previous[i] as f32;
                    xx += ix * ix;
                    yy += iy * iy;
                    xy += ix * iy;
                    xt += ix * it;
                    yt += iy * it;
                }
            }
            let determinant = xx * yy - xy * xy;
            if determinant > 1.0e-3 {
                let u = (xy * yt - yy * xt) / determinant;
                let v = (xy * xt - xx * yt) / determinant;
                magnitudes.push((u * u + v * v).sqrt());
            }
        }
    }
    if magnitudes.is_empty() {
        return 0.0;
    }
    let index = ((magnitudes.len() - 1) as f32 * 0.75).round() as usize;
    let (_, value, _) = magnitudes.select_nth_unstable_by(index, f32::total_cmp);
    *value / ((width * width + height * height) as f32).sqrt().max(1.0)
}

fn representative_lab_rgb(pixels: &[u8]) -> [u8; 3] {
    let mut sum = [0.0f64; 3];
    let mut count = 0usize;
    for pixel in pixels.chunks_exact(3) {
        let [l, a, b] = rgb_to_lab(pixel[0], pixel[1], pixel[2]);
        sum[0] += l as f64;
        sum[1] += a as f64;
        sum[2] += b as f64;
        count += 1;
    }
    lab_to_rgb([
        (sum[0] / count.max(1) as f64) as f32,
        (sum[1] / count.max(1) as f64) as f32,
        (sum[2] / count.max(1) as f64) as f32,
    ])
}

fn rgb_to_lab(r: u8, g: u8, b: u8) -> [f32; 3] {
    let linear = |value: u8| {
        let value = value as f32 / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    let r = linear(r);
    let g = linear(g);
    let b = linear(b);
    let x = (r * 0.4124564 + g * 0.3575761 + b * 0.1804375) / 0.95047;
    let y = r * 0.2126729 + g * 0.7151522 + b * 0.072175;
    let z = (r * 0.0193339 + g * 0.119192 + b * 0.9503041) / 1.08883;
    let f = |value: f32| {
        if value > 0.008856 {
            value.cbrt()
        } else {
            7.787 * value + 16.0 / 116.0
        }
    };
    let fx = f(x);
    let fy = f(y);
    let fz = f(z);
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

fn lab_to_rgb(lab: [f32; 3]) -> [u8; 3] {
    let fy = (lab[0] + 16.0) / 116.0;
    let fx = lab[1] / 500.0 + fy;
    let fz = fy - lab[2] / 200.0;
    let inverse = |value: f32| {
        let cube = value * value * value;
        if cube > 0.008856 {
            cube
        } else {
            (value - 16.0 / 116.0) / 7.787
        }
    };
    let x = inverse(fx) * 0.95047;
    let y = inverse(fy);
    let z = inverse(fz) * 1.08883;
    let channels = [
        x * 3.2404542 - y * 1.5371385 - z * 0.4985314,
        -x * 0.969266 + y * 1.8760108 + z * 0.041556,
        x * 0.0556434 - y * 0.2040259 + z * 1.0572252,
    ];
    let encode = |value: f32| {
        let value = if value <= 0.0031308 {
            12.92 * value
        } else {
            1.055 * value.max(0.0).powf(1.0 / 2.4) - 0.055
        };
        (value.clamp(0.0, 1.0) * 255.0).round() as u8
    };
    [
        encode(channels[0]),
        encode(channels[1]),
        encode(channels[2]),
    ]
}

#[derive(Clone, Copy)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    fn lowpass(rate: f32, frequency: f32, q: f32) -> Self {
        Self::design(rate, frequency, q, false)
    }

    fn highpass(rate: f32, frequency: f32, q: f32) -> Self {
        Self::design(rate, frequency, q, true)
    }

    fn design(rate: f32, frequency: f32, q: f32, highpass: bool) -> Self {
        let omega = std::f32::consts::TAU * frequency.min(rate * 0.46) / rate.max(1.0);
        let cosine = omega.cos();
        let alpha = omega.sin() / (2.0 * q);
        let a0 = 1.0 + alpha;
        let (b0, b1, b2) = if highpass {
            ((1.0 + cosine) * 0.5, -(1.0 + cosine), (1.0 + cosine) * 0.5)
        } else {
            ((1.0 - cosine) * 0.5, 1.0 - cosine, (1.0 - cosine) * 0.5)
        };
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: -2.0 * cosine / a0,
            a2: (1.0 - alpha) / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        let output = self.b0 * input + self.z1;
        self.z1 = self.b1 * input - self.a1 * output + self.z2;
        self.z2 = self.b2 * input - self.a2 * output;
        output
    }
}

struct AudioBands {
    low: [Biquad; 2],
    mid_high: [Biquad; 2],
    mid_low: [Biquad; 2],
    high: [Biquad; 2],
}

impl AudioBands {
    fn new(rate: f32) -> Self {
        let q = [0.5411961, 1.306563];
        Self {
            low: q.map(|q| Biquad::lowpass(rate, 250.0, q)),
            mid_high: q.map(|q| Biquad::highpass(rate, 250.0, q)),
            mid_low: q.map(|q| Biquad::lowpass(rate, 4000.0, q)),
            high: q.map(|q| Biquad::highpass(rate, 4000.0, q)),
        }
    }

    fn process(&mut self, input: f32) -> [f32; 3] {
        let low = self
            .low
            .iter_mut()
            .fold(input, |value, filter| filter.process(value));
        let mid = self
            .mid_high
            .iter_mut()
            .fold(input, |value, filter| filter.process(value));
        let mid = self
            .mid_low
            .iter_mut()
            .fold(mid, |value, filter| filter.process(value));
        let high = self
            .high
            .iter_mut()
            .fold(input, |value, filter| filter.process(value));
        [low.abs(), mid.abs(), high.abs()]
    }
}

fn analyze_audio(path: &Path, target_samples: Option<usize>) -> Result<AudioWaveform> {
    let file = File::open(path).with_context(|| format!("open audio {}", path.display()))?;
    let mut source =
        Decoder::try_from(file).with_context(|| format!("decode audio {}", path.display()))?;
    let channels = source.channels().get() as usize;
    let sample_rate = source.sample_rate().get() as usize;
    let bucket_frames = (sample_rate as f64 / ANALYSIS_FPS).round().max(1.0) as usize;
    let mut filters = [
        AudioBands::new(sample_rate as f32),
        AudioBands::new(sample_rate as f32),
    ];
    let mut peaks: [Vec<f32>; AUDIO_BANDS] = std::array::from_fn(|_| Vec::new());
    let mut bucket = [0.0f32; AUDIO_BANDS];
    let mut histograms = [[0u64; 2048]; AUDIO_BANDS];
    let mut channel = 0usize;
    let mut frame = [0.0f32; 2];
    let mut frames_in_bucket = 0usize;

    for sample in &mut source {
        if channel < 2 {
            frame[channel] = sample;
        }
        channel += 1;
        if channel < channels.max(1) {
            continue;
        }
        channel = 0;
        if channels == 1 {
            frame[1] = frame[0];
        }
        for side in 0..2 {
            let values = filters[side].process(frame[side]);
            for (band, value) in values.into_iter().enumerate() {
                let index = side * 3 + band;
                bucket[index] = bucket[index].max(value);
                let bin = (value.min(4.0) * (2047.0 / 4.0)).round() as usize;
                histograms[index][bin] += 1;
            }
        }
        frames_in_bucket += 1;
        if frames_in_bucket >= bucket_frames {
            for band in 0..AUDIO_BANDS {
                peaks[band].push(bucket[band]);
                bucket[band] = 0.0;
            }
            frames_in_bucket = 0;
        }
    }
    if frames_in_bucket > 0 {
        for band in 0..AUDIO_BANDS {
            peaks[band].push(bucket[band]);
        }
    }
    anyhow::ensure!(
        !peaks[0].is_empty(),
        "no audio samples decoded for waveform"
    );

    let target = target_samples.unwrap_or(peaks[0].len()).max(1);
    let bands = std::array::from_fn(|band| {
        let reference = histogram_percentile(&histograms[band], 99.5).max(1.0e-9);
        let values = resample_peak_f32(&peaks[band], target);
        values
            .into_iter()
            .map(|value| ((value / reference).clamp(0.0, 1.0).powf(0.62) * 255.0).round() as u8)
            .collect()
    });
    Ok(AudioWaveform { bands })
}

fn histogram_percentile(histogram: &[u64; 2048], percentile: f32) -> f32 {
    let total = histogram.iter().sum::<u64>();
    if total == 0 {
        return 1.0;
    }
    let target = (total as f64 * percentile as f64 / 100.0).ceil() as u64;
    let mut cumulative = 0u64;
    for (index, count) in histogram.iter().enumerate() {
        cumulative += count;
        if cumulative >= target {
            return index as f32 * 4.0 / 2047.0;
        }
    }
    4.0
}

fn normalize_activity(values: &[f32], gamma: f32, percentile: f32, close_gaps: bool) -> Vec<u8> {
    if values.is_empty() {
        return Vec::new();
    }
    let mut nonzero = values
        .iter()
        .copied()
        .filter(|value| value.is_finite() && *value > 1.0e-8)
        .collect::<Vec<_>>();
    nonzero.sort_by(f32::total_cmp);
    let scale = nonzero
        .get(((nonzero.len().saturating_sub(1)) as f32 * percentile / 100.0).round() as usize)
        .copied()
        .unwrap_or(1.0)
        .max(1.0e-8);
    let mut normalized = values
        .iter()
        .map(|value| (value.max(0.0) / scale).clamp(0.0, 1.0).powf(gamma))
        .collect::<Vec<_>>();
    if close_gaps && normalized.len() >= 7 {
        let dilated = rolling_extreme(&normalized, 3, f32::max, 0.0);
        let closed = rolling_extreme(&dilated, 3, f32::min, 1.0);
        for (value, closed) in normalized.iter_mut().zip(closed) {
            *value = value.max(closed);
        }
    }
    normalized
        .into_iter()
        .map(|value| (value * 255.0).round() as u8)
        .collect()
}

fn rolling_extreme(
    values: &[f32],
    radius: usize,
    combine: fn(f32, f32) -> f32,
    initial: f32,
) -> Vec<f32> {
    (0..values.len())
        .map(|index| {
            let start = index.saturating_sub(radius);
            let end = (index + radius + 1).min(values.len());
            values[start..end].iter().copied().fold(initial, combine)
        })
        .collect()
}

fn resample_peak_f32(values: &[f32], width: usize) -> Vec<f32> {
    (0..width)
        .map(|x| {
            let start = x * values.len() / width;
            let end = ((x + 1) * values.len()).div_ceil(width).max(start + 1);
            values[start.min(values.len() - 1)..end.min(values.len())]
                .iter()
                .copied()
                .fold(0.0, f32::max)
        })
        .collect()
}

fn waveform_segment_pixels(
    data: &WaveformData,
    sample_start: usize,
    sample_end: usize,
) -> (u32, u32, Vec<u8>) {
    let width = sample_end - sample_start;
    let height = usize::from(data.video.is_some()) + usize::from(data.audio.is_some()) * 2;
    let mut pixels = vec![0u8; width * height * 4];
    let mut row = 0usize;
    if let Some(video) = &data.video {
        for (x, sample) in (sample_start..sample_end).enumerate() {
            let color = video.colors.get(sample).copied().unwrap_or([110, 130, 145]);
            let activity = video.activity.get(sample).copied().unwrap_or(0);
            set_pixel(
                &mut pixels,
                width,
                x,
                row,
                [color[0], color[1], color[2], activity],
            );
        }
        row += 1;
    }
    if let Some(audio) = &data.audio {
        for (x, sample) in (sample_start..sample_end).enumerate() {
            let levels = |offset: usize| {
                [
                    encode_linear_byte(audio.bands[offset].get(sample).copied().unwrap_or(0)),
                    encode_linear_byte(audio.bands[offset + 1].get(sample).copied().unwrap_or(0)),
                    encode_linear_byte(audio.bands[offset + 2].get(sample).copied().unwrap_or(0)),
                    255,
                ]
            };
            set_pixel(&mut pixels, width, x, row, levels(0));
            set_pixel(&mut pixels, width, x, row + 1, levels(3));
        }
    }
    (width as u32, height as u32, pixels)
}

fn encode_linear_byte(value: u8) -> u8 {
    let value = value as f32 / 255.0;
    let encoded = if value <= 0.003_130_8 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    (encoded.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn set_pixel(pixels: &mut [u8], texture_width: usize, x: usize, y: usize, color: [u8; 4]) {
    let index = (y * texture_width + x) * 4;
    pixels[index..index + 4].copy_from_slice(&color);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peak_resampling_preserves_short_activity() {
        let mut input = vec![0.0f32; 1024];
        input[511] = 1.0;
        let output = resample_peak_f32(&input, 16);
        assert_eq!(output.iter().copied().fold(0.0, f32::max), 1.0);
    }

    #[test]
    fn texture_segments_keep_native_sample_width() {
        let data = WaveformData {
            video: Some(VideoWaveform {
                colors: vec![[255, 0, 0]; 300],
                activity: vec![255; 300],
            }),
            audio: None,
        };
        let (width, height, pixels) = waveform_segment_pixels(&data, 256, 300);
        assert_eq!((width, height), (44, 1));
        assert_eq!(pixels.len(), 44 * 4);
    }

    #[test]
    fn static_frames_have_zero_motion() {
        let frame = vec![80u8; 96 * 54];
        assert_eq!(lucas_kanade_flow_score(&frame, &frame, 96, 54), 0.0);
    }
}
