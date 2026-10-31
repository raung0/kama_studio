use std::{
    collections::{HashMap, HashSet},
    fs::File,
    path::{Path, PathBuf},
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
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

const fn nonnegative_f32_to_usize(value: f32) -> usize {
    #[expect(
        clippy::cast_sign_loss,
        reason = "max clamps negative and NaN values to zero"
    )]
    {
        value.max(0.0) as usize
    }
}

const fn nonnegative_f64_to_usize(value: f64) -> usize {
    #[expect(
        clippy::cast_sign_loss,
        reason = "max clamps negative and NaN values to zero"
    )]
    {
        value.max(0.0) as usize
    }
}

const fn nonnegative_f64_to_u32(value: f64) -> u32 {
    #[expect(
        clippy::cast_sign_loss,
        reason = "max clamps negative and NaN values to zero"
    )]
    {
        value.max(0.0) as u32
    }
}

const fn nonnegative_f64_to_u64(value: f64) -> u64 {
    #[expect(
        clippy::cast_sign_loss,
        reason = "max clamps negative and NaN values to zero"
    )]
    {
        value.max(0.0) as u64
    }
}

const fn nonnegative_f32_to_u8(value: f32) -> u8 {
    #[expect(
        clippy::cast_sign_loss,
        reason = "max clamps negative and NaN values to zero"
    )]
    {
        value.max(0.0) as u8
    }
}

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
            let Some(asset) = project.media.iter_mut().find(|asset| {
                (asset.id, asset.path.as_path()) == (result.media, result.path.as_path())
            }) else {
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
            let texture_rows: usize = match (data.video.is_some(), data.audio.is_some()) {
                (false, false) => 0,
                (true, false) => 1,
                (false, true) => 2,
                (true, true) => 3,
            };
            let row_uv = |row: usize| {
                if texture_rows <= 1 {
                    0.0
                } else {
                    row as f32 / texture_rows.saturating_sub(1) as f32
                }
            };
            let video_y = data.video.as_ref().map(|_| [row_uv(0); 2]);
            let audio_y = data.audio.as_ref().map(|_| {
                let row = usize::from(data.video.is_some());
                [row_uv(row), row_uv(row.saturating_add(1))]
            });
            let mut segments = Vec::new();
            for sample_start in (0..sample_count).step_by(TEXTURE_SEGMENT_WIDTH) {
                let sample_end = sample_start
                    .saturating_add(TEXTURE_SEGMENT_WIDTH)
                    .min(sample_count);
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
        .map(|duration| nonnegative_f64_to_usize((duration * ANALYSIS_FPS).ceil().max(1.0)))
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
    let analysis_height = nonnegative_f64_to_u32(
        (f64::from(source_height) * f64::from(VIDEO_ANALYSIS_WIDTH) / f64::from(source_width))
            .round(),
    )
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
            .map_or(*next_sample_time, |pts| pts as f64 * f64::from(time_base));
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
        anyhow::ensure!(stride > 0, "waveform frame has zero stride");
        let capacity = row_bytes
            .checked_mul(analysis_height as usize)
            .context("waveform frame size overflow")?;
        let mut packed = Vec::with_capacity(capacity);
        for row in source.chunks(stride).take(analysis_height as usize) {
            if let Some(pixels) = row.get(..row_bytes) {
                packed.extend_from_slice(pixels);
            }
        }
        if packed.is_empty() {
            continue;
        }
        colors.push(representative_lab_rgb(&packed));
        let gray = packed
            .as_chunks::<3>()
            .0
            .iter()
            .map(|pixel| {
                nonnegative_f32_to_u8(
                    f32::mul_add(
                        f32::from(pixel[2]),
                        0.114,
                        f32::mul_add(f32::from(pixel[1]), 0.587, f32::from(pixel[0]) * 0.299),
                    )
                    .round(),
                )
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
    #[expect(
        clippy::expect_used,
        reason = "an image whose dimensions overflow cannot fit in a slice"
    )]
    let pixel_count = width
        .checked_mul(height)
        .expect("waveform image dimensions overflow");
    assert_eq!(
        current.len(),
        pixel_count,
        "waveform image dimensions do not match pixels"
    );
    let pixel = |data: &[u8], index: usize| {
        #[expect(
            clippy::expect_used,
            reason = "the image length is validated and the neighborhood stays inside the three-pixel border"
        )]
        *data.get(index).expect("waveform flow pixel in bounds")
    };
    let mut magnitudes = Vec::with_capacity(pixel_count / 9);
    for cy in (3..height.saturating_sub(3)).step_by(3) {
        for cx in (3..width.saturating_sub(3)).step_by(3) {
            let mut xx = 0.0;
            let mut yy = 0.0;
            let mut xy = 0.0;
            let mut xt = 0.0;
            let mut yt = 0.0;
            for y in cy.saturating_sub(2)..=cy.saturating_add(2) {
                for x in cx.saturating_sub(2)..=cx.saturating_add(2) {
                    let i = y.saturating_mul(width).saturating_add(x);
                    let ix = (f32::from(pixel(current, i.saturating_add(1)))
                        - f32::from(pixel(current, i.saturating_sub(1)))
                        + f32::from(pixel(previous, i.saturating_add(1)))
                        - f32::from(pixel(previous, i.saturating_sub(1))))
                        * 0.25;
                    let iy = (f32::from(pixel(current, i.saturating_add(width)))
                        - f32::from(pixel(current, i.saturating_sub(width)))
                        + f32::from(pixel(previous, i.saturating_add(width)))
                        - f32::from(pixel(previous, i.saturating_sub(width))))
                        * 0.25;
                    let it = f32::from(pixel(current, i)) - f32::from(pixel(previous, i));
                    xx = ix.mul_add(ix, xx);
                    yy = iy.mul_add(iy, yy);
                    xy = ix.mul_add(iy, xy);
                    xt = ix.mul_add(it, xt);
                    yt = iy.mul_add(it, yt);
                }
            }
            let determinant = f32::mul_add(xy, -xy, xx * yy);
            if determinant > 1.0e-3 {
                let u = f32::mul_add(yy, -xt, xy * yt) / determinant;
                let v = f32::mul_add(xx, -yt, xy * xt) / determinant;
                magnitudes.push(u.hypot(v));
            }
        }
    }
    if magnitudes.is_empty() {
        return 0.0;
    }
    let index =
        nonnegative_f32_to_usize((magnitudes.len().saturating_sub(1) as f32 * 0.75).round());
    let (_, value, _) = magnitudes.select_nth_unstable_by(index, f32::total_cmp);
    *value / (width as f32).hypot(height as f32).max(1.0)
}

fn representative_lab_rgb(pixels: &[u8]) -> [u8; 3] {
    let mut sum = [0.0f64; 3];
    let mut count = 0usize;
    for pixel in pixels.as_chunks::<3>().0 {
        let [l, a, b] = rgb_to_lab(pixel[0], pixel[1], pixel[2]);
        sum[0] += f64::from(l);
        sum[1] += f64::from(a);
        sum[2] += f64::from(b);
        count = count.saturating_add(1);
    }
    lab_to_rgb([
        (sum[0] / count.max(1) as f64) as f32,
        (sum[1] / count.max(1) as f64) as f32,
        (sum[2] / count.max(1) as f64) as f32,
    ])
}

fn rgb_to_lab(red: u8, green: u8, blue: u8) -> [f32; 3] {
    let linear = |value: u8| {
        let value = f32::from(value) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    let red = linear(red);
    let green = linear(green);
    let blue = linear(blue);
    let x = f32::mul_add(
        blue,
        0.180_437_5,
        f32::mul_add(green, 0.357_576_1, red * 0.412_456_4),
    ) / 0.95047;
    let y = f32::mul_add(
        blue,
        0.072_175,
        f32::mul_add(green, 0.715_152_2, red * 0.212_672_9),
    );
    let z = f32::mul_add(
        blue,
        0.950_304_1,
        f32::mul_add(green, 0.119_192, red * 0.019_333_9),
    ) / 1.08883;
    let f = |value: f32| {
        if value > 0.008_856 {
            value.cbrt()
        } else {
            7.787f32.mul_add(value, 16.0 / 116.0)
        }
    };
    let fx = f(x);
    let fy = f(y);
    let fz = f(z);
    [
        116.0f32.mul_add(fy, -16.0),
        500.0 * (fx - fy),
        200.0 * (fy - fz),
    ]
}

fn lab_to_rgb(lab: [f32; 3]) -> [u8; 3] {
    let fy = (lab[0] + 16.0) / 116.0;
    let fx = lab[1] / 500.0 + fy;
    let fz = fy - lab[2] / 200.0;
    let inverse = |value: f32| {
        let cube = value * value * value;
        if cube > 0.008_856 {
            cube
        } else {
            (value - 16.0 / 116.0) / 7.787
        }
    };
    let x = inverse(fx) * 0.95047;
    let y = inverse(fy);
    let z = inverse(fz) * 1.08883;
    let channels = [
        f32::mul_add(
            z,
            -0.498_531_4,
            f32::mul_add(y, -1.537_138_5, x * 3.240_454_2),
        ),
        f32::mul_add(z, 0.041_556, f32::mul_add(y, 1.876_010_8, -x * 0.969_266)),
        f32::mul_add(
            z,
            1.057_225_2,
            f32::mul_add(y, -0.204_025_9, x * 0.055_643_4),
        ),
    ];
    let encode = |value: f32| {
        let value = if value <= 0.003_130_8 {
            12.92 * value
        } else {
            1.055f32.mul_add(value.max(0.0).powf(1.0 / 2.4), -0.055)
        };
        nonnegative_f32_to_u8((value.clamp(0.0, 1.0) * 255.0).round())
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
            (
                f32::midpoint(1.0, cosine),
                -(1.0 + cosine),
                f32::midpoint(1.0, cosine),
            )
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
        let output = self.b0.mul_add(input, self.z1);
        self.z1 = self.a1.mul_add(-output, self.b1 * input) + self.z2;
        self.z2 = self.a2.mul_add(-output, self.b2 * input);
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
        let q = [0.541_196_1, 1.306_563];
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
    let bucket_frames =
        nonnegative_f64_to_usize((sample_rate as f64 / ANALYSIS_FPS).round().max(1.0));
    let mut filters = [
        AudioBands::new(sample_rate as f32),
        AudioBands::new(sample_rate as f32),
    ];
    let mut peaks: [Vec<f32>; AUDIO_BANDS] = std::array::from_fn(|_| Vec::new());
    let mut bucket = [0.0f32; AUDIO_BANDS];
    let mut histograms = vec![[0u64; 2048]; AUDIO_BANDS];
    let mut channel = 0usize;
    let mut frame = [0.0f32; 2];
    let mut frames_in_bucket = 0usize;

    for sample in &mut source {
        if let Some(value) = frame.get_mut(channel) {
            *value = sample;
        }
        channel = channel.saturating_add(1);
        if channel < channels.max(1) {
            continue;
        }
        channel = 0;
        if channels == 1 {
            frame[1] = frame[0];
        }
        let (bucket_chunks, _) = bucket.as_chunks_mut::<3>();
        let (histogram_chunks, _) = histograms.as_chunks_mut::<3>();
        for (((filter, input), buckets), histograms) in filters
            .iter_mut()
            .zip(frame)
            .zip(bucket_chunks)
            .zip(histogram_chunks)
        {
            let values = filter.process(input);
            for ((value, bucket), histogram) in values.into_iter().zip(buckets).zip(histograms) {
                *bucket = bucket.max(value);
                let bin = nonnegative_f32_to_usize((value.min(4.0) * (2047.0 / 4.0)).round());
                #[expect(
                    clippy::expect_used,
                    reason = "the magnitude is clamped to 0..=4 before mapping to 2048 bins"
                )]
                let count = histogram
                    .get_mut(bin)
                    .expect("audio histogram bin in bounds");
                *count = count.saturating_add(1);
            }
        }
        frames_in_bucket = frames_in_bucket.saturating_add(1);
        if frames_in_bucket >= bucket_frames {
            for (peaks, value) in peaks.iter_mut().zip(&mut bucket) {
                peaks.push(*value);
                *value = 0.0;
            }
            frames_in_bucket = 0;
        }
    }
    if frames_in_bucket > 0 {
        for (peaks, value) in peaks.iter_mut().zip(bucket) {
            peaks.push(value);
        }
    }
    anyhow::ensure!(
        !peaks[0].is_empty(),
        "no audio samples decoded for waveform"
    );

    let target = target_samples.unwrap_or(peaks[0].len()).max(1);
    let mut histograms = histograms.into_iter();
    let bands = peaks.map(|peaks| {
        #[expect(
            clippy::expect_used,
            reason = "histograms and peaks both have AUDIO_BANDS elements"
        )]
        let histogram = histograms.next().expect("audio band histogram exists");
        let reference = histogram_percentile(&histogram, 99.5).max(1.0e-9);
        let values = resample_peak_f32(&peaks, target);
        values
            .into_iter()
            .map(|value| {
                nonnegative_f32_to_u8(
                    ((value / reference).clamp(0.0, 1.0).powf(0.62) * 255.0).round(),
                )
            })
            .collect()
    });
    Ok(AudioWaveform { bands })
}

fn histogram_percentile(histogram: &[u64; 2048], percentile: f32) -> f32 {
    let total = histogram.iter().sum::<u64>();
    if total == 0 {
        return 1.0;
    }
    let target = nonnegative_f64_to_u64((total as f64 * f64::from(percentile) / 100.0).ceil());
    let mut cumulative = 0u64;
    for (index, count) in histogram.iter().enumerate() {
        cumulative = cumulative.saturating_add(*count);
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
        .get(nonnegative_f32_to_usize(
            ((nonzero.len().saturating_sub(1)) as f32 * percentile / 100.0).round(),
        ))
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
        .map(|value| nonnegative_f32_to_u8((value * 255.0).round()))
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
            let end = index
                .saturating_add(radius)
                .saturating_add(1)
                .min(values.len());
            #[expect(
                clippy::expect_used,
                reason = "the rolling window endpoints are ordered and clamped to the slice length"
            )]
            values
                .get(start..end)
                .expect("rolling window in bounds")
                .iter()
                .copied()
                .fold(initial, combine)
        })
        .collect()
}

fn resample_peak_f32(values: &[f32], width: usize) -> Vec<f32> {
    assert!(
        !values.is_empty() || width == 0,
        "cannot resample empty peaks"
    );
    let Some(nonzero_width) = std::num::NonZeroU128::new(width as u128) else {
        return Vec::new();
    };
    (0..width)
        .map(|x| {
            #[expect(
                clippy::expect_used,
                reason = "the product of two usize values fits u128 on supported platforms"
            )]
            let start_numerator = (x as u128)
                .checked_mul(values.len() as u128)
                .expect("resample offset overflow");
            #[expect(
                clippy::expect_used,
                reason = "nonzero_width is constructed from a nonzero width"
            )]
            let start = start_numerator
                .checked_div(nonzero_width.get())
                .expect("resample divisor is nonzero");
            #[expect(
                clippy::expect_used,
                reason = "x is less than width, so x + 1 fits usize and its product fits u128"
            )]
            let end = (x.saturating_add(1) as u128)
                .checked_mul(values.len() as u128)
                .expect("resample end overflow")
                .div_ceil(nonzero_width.get());
            let start = (start as usize).min(values.len().saturating_sub(1));
            let end = (end as usize)
                .max(start.saturating_add(1))
                .min(values.len());
            #[expect(
                clippy::expect_used,
                reason = "start and end are ordered and clamped to the nonempty slice"
            )]
            values
                .get(start..end)
                .expect("resample window in bounds")
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
    #[expect(
        clippy::expect_used,
        reason = "segment endpoints are generated in ascending order"
    )]
    let width = sample_end
        .checked_sub(sample_start)
        .expect("invalid waveform segment");
    let height = match (data.video.is_some(), data.audio.is_some()) {
        (false, false) => 0usize,
        (true, false) => 1,
        (false, true) => 2,
        (true, true) => 3,
    };
    #[expect(
        clippy::expect_used,
        reason = "segment pixels must fit in an addressable allocation"
    )]
    let size = width
        .checked_mul(height)
        .and_then(|size| size.checked_mul(4))
        .expect("waveform texture size overflow");
    let mut pixels = vec![0u8; size];
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
        row = row.saturating_add(1);
    }
    if let Some(audio) = &data.audio {
        for (x, sample) in (sample_start..sample_end).enumerate() {
            let levels = |bands: &[Vec<u8>]| {
                let mut levels = [0, 0, 0, 255];
                for (channel, band) in levels.iter_mut().zip(bands) {
                    *channel = encode_linear_byte(band.get(sample).copied().unwrap_or(0));
                }
                levels
            };
            let (left, right) = audio.bands.split_at(3);
            set_pixel(&mut pixels, width, x, row, levels(left));
            set_pixel(&mut pixels, width, x, row.saturating_add(1), levels(right));
        }
    }
    (width as u32, height as u32, pixels)
}

fn encode_linear_byte(value: u8) -> u8 {
    let value = f32::from(value) / 255.0;
    let encoded = if value <= 0.003_130_8 {
        value * 12.92
    } else {
        1.055f32.mul_add(value.powf(1.0 / 2.4), -0.055)
    };
    nonnegative_f32_to_u8((encoded.clamp(0.0, 1.0) * 255.0).round())
}

fn set_pixel(pixels: &mut [u8], texture_width: usize, x: usize, y: usize, color: [u8; 4]) {
    #[expect(
        clippy::expect_used,
        reason = "pixel coordinates come from the allocated waveform segment"
    )]
    let index = y
        .checked_mul(texture_width)
        .and_then(|index| index.checked_add(x))
        .and_then(|index| index.checked_mul(4))
        .expect("waveform pixel offset overflow");
    #[expect(
        clippy::expect_used,
        reason = "pixel coordinates come from the allocated waveform segment"
    )]
    let end = index.checked_add(4).expect("waveform pixel end overflow");
    #[expect(
        clippy::expect_used,
        reason = "pixel coordinates come from the allocated waveform segment"
    )]
    pixels
        .get_mut(index..end)
        .expect("waveform pixel in bounds")
        .copy_from_slice(&color);
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
