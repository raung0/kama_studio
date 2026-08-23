use kama_plugin::{audio, get_parameter};
use std::{cell::UnsafeCell, slice};

#[no_mangle]
pub extern "C" fn kama_alloc(len: i32) -> i32 {
    if len <= 0 {
        return 0;
    }
    
    let words = ((len as usize).saturating_add(3) / 4).max(1);
    let samples = vec![0.0f32; words].into_boxed_slice();
    Box::into_raw(samples) as *mut f32 as i32
}







#[no_mangle]
pub unsafe extern "C" fn kama_dealloc(pointer: i32, len: i32) {
    if pointer > 0 && len > 0 {
        let words = ((len as usize).saturating_add(3) / 4).max(1);
        let slice = std::ptr::slice_from_raw_parts_mut(pointer as *mut f32, words);
        drop(Box::from_raw(slice));
    }
}

struct Global<T>(UnsafeCell<T>);
unsafe impl<T: Send> Sync for Global<T> {}

impl<T> Global<T> {
    const fn new(value: T) -> Self {
        Self(UnsafeCell::new(value))
    }

    unsafe fn with_mut<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        f(&mut *self.0.get())
    }
}

unsafe fn samples<'a>(pointer: i32, frames: i32, channels: i32) -> Option<&'a mut [f32]> {
    if pointer <= 0 || frames < 0 || channels <= 0 {
        return None;
    }
    let len = (frames as usize).checked_mul(channels as usize)?;
    Some(slice::from_raw_parts_mut(pointer as *mut f32, len))
}

fn db_to_linear(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

#[no_mangle]
pub extern "C" fn process_gain(
    pointer: i32,
    frames: i32,
    channels: i32,
    _sample_rate: i32,
    _flags: i32,
) -> i32 {
    let Some(samples) = (unsafe { samples(pointer, frames, channels) }) else {
        return -1;
    };
    let linear = db_to_linear(get_parameter("gain_db", 0.0f32));
    for sample in samples {
        *sample *= linear;
    }
    0
}

#[derive(Clone, Copy)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl Default for Biquad {
    fn default() -> Self {
        Self { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0 }
    }
}

#[derive(Default)]
struct EqState {
    channels: usize,
    sample_rate: u32,
    bands: usize,
    filters: Vec<Biquad>,
    z1: Vec<f32>,
    z2: Vec<f32>,
}

impl EqState {
    fn configure(&mut self, channels: usize, sample_rate: u32, bands: usize) {
        let bands = bands.clamp(1, 31);
        if self.channels == channels && self.sample_rate == sample_rate && self.bands == bands {
            return;
        }
        self.channels = channels;
        self.sample_rate = sample_rate;
        self.bands = bands;
        self.filters.resize(bands, Biquad::default());
        self.z1.resize(channels.saturating_mul(bands), 0.0);
        self.z2.resize(channels.saturating_mul(bands), 0.0);
        self.reset();
    }

    fn reset(&mut self) {
        self.z1.fill(0.0);
        self.z2.fill(0.0);
    }

    fn update_filters(&mut self) {
        let rate = self.sample_rate.max(1) as f32;
        let nyquist = rate * 0.5;
        let low = 40.0_f32.min(nyquist * 0.4);
        let high = 16_000.0_f32.min(nyquist * 0.82).max(low * 1.01);
        let denom = self.bands.saturating_sub(1).max(1) as f32;
        let band_values = get_parameter("band_values", Vec::<f32>::new());
        let q = match self.bands {
            0..=3 => 0.8,
            4..=5 => 1.0,
            6..=10 => 1.4,
            11..=15 => 2.2,
            _ => 4.3,
        };
        for index in 0..self.bands {
            let t = if self.bands <= 1 { 0.5 } else { index as f32 / denom };
            let frequency = low * (high / low).powf(t);
            let gain = band_values.get(index).copied().unwrap_or(0.0).clamp(-24.0, 24.0);
            self.filters[index] = peaking_eq(rate, frequency, q, gain);
        }
    }
}

fn peaking_eq(sample_rate: f32, frequency: f32, q: f32, gain_db: f32) -> Biquad {
    let a = 10.0_f32.powf(gain_db / 40.0);
    let w0 = 2.0 * std::f32::consts::PI * frequency / sample_rate.max(1.0);
    let cosine = w0.cos();
    let alpha = w0.sin() / (2.0 * q.max(0.001));
    let b0 = 1.0 + alpha * a;
    let b1 = -2.0 * cosine;
    let b2 = 1.0 - alpha * a;
    let a0 = 1.0 + alpha / a;
    let a1 = -2.0 * cosine;
    let a2 = 1.0 - alpha / a;
    Biquad {
        b0: b0 / a0,
        b1: b1 / a0,
        b2: b2 / a0,
        a1: a1 / a0,
        a2: a2 / a0,
    }
}

static EQ_STATE: Global<Option<EqState>> = Global::new(None);

#[no_mangle]
pub extern "C" fn process_eq(
    pointer: i32,
    frames: i32,
    channels: i32,
    sample_rate: i32,
    flags: i32,
) -> i32 {
    const BAND_COUNTS: [usize; 5] = [3, 5, 10, 15, 31];
    let channels = channels.max(1) as usize;
    let band_mode = get_parameter("band_count", 2u32).min(4) as usize;
    let bands = BAND_COUNTS[band_mode];
    unsafe {
        EQ_STATE.with_mut(|slot| {
            let state = slot.get_or_insert_with(EqState::default);
            state.configure(channels, sample_rate.max(1) as u32, bands);
            if audio::reset_requested(flags) {
                state.reset();
            }
            state.update_filters();
            let Some(samples) = samples(pointer, frames, channels as i32) else {
                return if frames == 0 { 0 } else { -1 };
            };
            for (sample_index, sample) in samples.iter_mut().enumerate() {
                let channel = sample_index % channels;
                let mut value = *sample;
                for band in 0..state.bands {
                    let filter = state.filters[band];
                    let state_index = channel * state.bands + band;
                    let output = filter.b0 * value + state.z1[state_index];
                    state.z1[state_index] =
                        filter.b1 * value - filter.a1 * output + state.z2[state_index];
                    state.z2[state_index] = filter.b2 * value - filter.a2 * output;
                    value = output;
                }
                *sample = value;
            }
            0
        })
    }
}

#[derive(Default)]
struct LimiterState {
    envelope: f32,
}

static LIMITER_STATE: Global<LimiterState> = Global::new(LimiterState { envelope: 1.0 });

#[no_mangle]
pub extern "C" fn process_limiter(
    pointer: i32,
    frames: i32,
    channels: i32,
    sample_rate: i32,
    flags: i32,
) -> i32 {
    unsafe {
        LIMITER_STATE.with_mut(|state| {
            if audio::reset_requested(flags) {
                state.envelope = 1.0;
            }
            let Some(samples) = samples(pointer, frames, channels) else {
                return if frames == 0 { 0 } else { -1 };
            };
            let threshold = db_to_linear(get_parameter("threshold_db", -1.0f32)).max(0.000_001);
            let release_ms = get_parameter("release_ms", 80.0f32).max(1.0);
            let release = (-1.0 / (sample_rate.max(1) as f32 * (release_ms / 1000.0))).exp();
            for sample in samples {
                let magnitude = sample.abs();
                let target = if magnitude > threshold {
                    threshold / magnitude.max(0.000_001)
                } else {
                    1.0
                };
                if target < state.envelope {
                    state.envelope = target;
                } else {
                    state.envelope = 1.0 - (1.0 - state.envelope) * release;
                }
                *sample *= state.envelope;
            }
            0
        })
    }
}

#[derive(Default)]
struct ReverbState {
    channels: usize,
    sample_rate: u32,
    delay_ms: f32,
    buffer: Vec<f32>,
    cursor: usize,
}

impl ReverbState {
    fn configure(&mut self, channels: usize, sample_rate: u32, delay_ms: f32) {
        if self.channels == channels
            && self.sample_rate == sample_rate
            && (self.delay_ms - delay_ms).abs() <= f32::EPSILON
        {
            return;
        }
        self.channels = channels;
        self.sample_rate = sample_rate;
        self.delay_ms = delay_ms;
        let frames = ((sample_rate.max(1) as f32 * delay_ms.max(1.0) / 1000.0).round() as usize)
            .max(1);
        self.buffer.clear();
        self.buffer.resize(frames.saturating_mul(channels.max(1)), 0.0);
        self.cursor = 0;
    }

    fn reset(&mut self) {
        self.buffer.fill(0.0);
        self.cursor = 0;
    }
}

static REVERB_STATE: Global<Option<ReverbState>> = Global::new(None);

#[no_mangle]
pub extern "C" fn process_reverb(
    pointer: i32,
    frames: i32,
    channels: i32,
    sample_rate: i32,
    flags: i32,
) -> i32 {
    let mix = get_parameter("mix", 0.2f32).clamp(0.0, 1.0);
    let decay = get_parameter("decay", 0.45f32).clamp(0.0, 0.98);
    let delay_ms = get_parameter("delay_ms", 95.0f32).max(1.0);
    unsafe {
        REVERB_STATE.with_mut(|slot| {
            let state = slot.get_or_insert_with(ReverbState::default);
            state.configure(channels.max(1) as usize, sample_rate.max(1) as u32, delay_ms);
            if audio::reset_requested(flags) {
                state.reset();
            }
            let Some(samples) = samples(pointer, frames, channels) else {
                return if frames == 0 { 0 } else { -1 };
            };
            if state.buffer.is_empty() {
                return 0;
            }
            for sample in samples {
                let delayed = state.buffer[state.cursor];
                state.buffer[state.cursor] = (*sample + delayed * decay).clamp(-4.0, 4.0);
                state.cursor = (state.cursor + 1) % state.buffer.len();
                *sample = *sample * (1.0 - mix) + delayed * mix;
            }
            0
        })
    }
}
