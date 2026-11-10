use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs::File,
    io::{BufRead, BufReader, BufWriter, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use rodio::{
    ChannelCount, Decoder, MixerDeviceSink, Player, Sample, SampleRate, Source,
    mixer::{self, Mixer},
    source::Zero,
};

use crate::{
    effects::{
        GpuValue, PipelineInstance, PipelineKind, ValueEvalContext, ValueEvaluator,
        resolved_node_input_cached,
    },
    messages,
    plugin::PluginRegistry,
    project::{CompositionId, HostValue, MediaId, MediaKind, Project, VisualSource},
    runtime::wasm::{AudioWasmProcessor, AudioWasmRuntime, plugin_parameter_hash},
    timeline::{Clip, TimelineDocument, TimelineState, Track},
};
use serde::{Deserialize, Serialize};

const AUDIO_DRIFT_TOLERANCE: f32 = 0.090;
const AUDIO_RETRY_DELAY: Duration = Duration::from_secs(2);

fn deadline_after(duration: Duration) -> Instant {
    Instant::now()
        .checked_add(duration)
        .unwrap_or_else(Instant::now)
}

fn audio_startup_ready(
    ready_generation: Option<(u64, u64)>,
    expected_generation: (u64, u64),
    audio_required: bool,
    output_ready: bool,
    sources_ready: bool,
) -> bool {
    ready_generation == Some(expected_generation)
        && (!audio_required || (output_ready && sources_ready))
}

macro_rules! delegate_source_metadata {
    () => {
        fn sample_rate(&self) -> SampleRate {
            self.inner.sample_rate()
        }
        fn total_duration(&self) -> Option<Duration> {
            self.inner.total_duration()
        }
    };
    (channels) => {
        fn channels(&self) -> ChannelCount {
            self.inner.channels()
        }
        delegate_source_metadata!();
    };
}

struct OffsetAudioSource<S> {
    inner: S,
    offset: Duration,
}

impl<S: Source> OffsetAudioSource<S> {
    fn new(mut inner: S, offset: Duration) -> Result<Self, rodio::source::SeekError> {
        if !offset.is_zero() {
            inner.try_seek(offset)?;
        }
        Ok(Self { inner, offset })
    }
}

impl<S: Source> Iterator for OffsetAudioSource<S> {
    type Item = Sample;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next()
    }
}

impl<S: Source> Source for OffsetAudioSource<S> {
    fn current_span_len(&self) -> Option<usize> {
        self.inner.current_span_len()
    }
    fn channels(&self) -> ChannelCount {
        self.inner.channels()
    }
    fn sample_rate(&self) -> SampleRate {
        self.inner.sample_rate()
    }
    fn total_duration(&self) -> Option<Duration> {
        self.inner
            .total_duration()
            .map(|duration| duration.saturating_sub(self.offset))
    }
    fn try_seek(&mut self, pos: Duration) -> Result<(), rodio::source::SeekError> {
        let absolute = self.offset.checked_add(pos).unwrap_or(Duration::MAX);
        self.inner.try_seek(absolute)
    }
}

#[derive(Default)]
struct AudioMeter {
    left: AtomicU32,
    right: AtomicU32,
}

impl AudioMeter {
    fn publish(&self, levels: [f32; 2]) {
        self.left
            .store(levels[0].max(0.0).to_bits(), Ordering::Relaxed);
        self.right
            .store(levels[1].max(0.0).to_bits(), Ordering::Relaxed);
    }

    fn levels(&self) -> [f32; 2] {
        [
            f32::from_bits(self.left.load(Ordering::Relaxed)),
            f32::from_bits(self.right.load(Ordering::Relaxed)),
        ]
    }
}

struct MeteredSource<S> {
    inner: S,
    meter: Arc<AudioMeter>,
    channel: usize,
    peak: [f32; 2],
    samples_since_publish: usize,
}

impl<S> MeteredSource<S> {
    const fn new(inner: S, meter: Arc<AudioMeter>) -> Self {
        Self {
            inner,
            meter,
            channel: 0,
            peak: [0.0; 2],
            samples_since_publish: 0,
        }
    }
}

impl<S: Source> Iterator for MeteredSource<S> {
    type Item = Sample;

    fn next(&mut self) -> Option<Self::Item> {
        let sample = self.inner.next()?;
        let channels = self.inner.channels().get() as usize;
        let amplitude = sample.abs();
        if channels <= 1 {
            self.peak[0] = self.peak[0].max(amplitude);
            self.peak[1] = self.peak[1].max(amplitude);
        } else {
            match self.channel {
                0 => self.peak[0] = self.peak[0].max(amplitude),
                1 => self.peak[1] = self.peak[1].max(amplitude),
                _ => {
                    self.peak[0] = self.peak[0].max(amplitude);
                    self.peak[1] = self.peak[1].max(amplitude);
                }
            }
        }
        #[expect(
            clippy::arithmetic_side_effects,
            reason = "max(1) guarantees a nonzero channel divisor"
        )]
        {
            self.channel = self.channel.saturating_add(1) % channels.max(1);
        }
        self.samples_since_publish = self.samples_since_publish.saturating_add(1);
        if self.samples_since_publish >= channels.max(1).saturating_mul(256) {
            self.meter.publish(self.peak);
            self.peak = [0.0; 2];
            self.samples_since_publish = 0;
        }
        Some(sample)
    }
}

impl<S: Source> Source for MeteredSource<S> {
    fn current_span_len(&self) -> Option<usize> {
        self.inner.current_span_len()
    }
    delegate_source_metadata!(channels);
    fn try_seek(&mut self, pos: Duration) -> Result<(), rodio::source::SeekError> {
        self.peak = [0.0; 2];
        self.samples_since_publish = 0;
        self.meter.publish([0.0; 2]);
        self.inner.try_seek(pos)
    }
}

const AUDIO_BLOCK_FRAMES: usize = 256;

#[derive(Clone, Debug, PartialEq)]
struct AudioEffectSettings {
    key: String,
    module: PathBuf,
    entry: String,
    parameters: Arc<HashMap<u32, HostValue>>,
}

impl AudioEffectSettings {
    fn same_processor(&self, other: &Self) -> bool {
        self.key == other.key && self.module == other.module && self.entry == other.entry
    }
}

struct AudioEffectChain {
    processors: Vec<Option<AudioWasmProcessor>>,
    channels: usize,
    sample_rate: u32,
}

impl AudioEffectChain {
    fn new(
        runtime: Option<&mut AudioWasmRuntime>,
        settings: &[AudioEffectSettings],
        channels: usize,
        sample_rate: u32,
    ) -> Self {
        let sample_capacity = AUDIO_BLOCK_FRAMES.saturating_mul(channels.max(1));
        let mut runtime = runtime;
        let processors = settings
            .iter()
            .map(|effect| {
                let runtime = runtime.as_deref_mut()?;
                match runtime.processor(&effect.module, &effect.entry, sample_capacity) {
                    Ok(processor) => Some(processor),
                    Err(error) => {
                        messages::error(
                            "Audio plugin",
                            format!("{} unavailable: {error:#}", effect.key),
                        );
                        None
                    }
                }
            })
            .collect();
        Self {
            processors,
            channels: channels.max(1),
            sample_rate: sample_rate.max(1),
        }
    }

    fn apply(
        &mut self,
        settings: &[AudioEffectSettings],
        operation: &'static str,
        mut run: impl FnMut(&mut AudioWasmProcessor, &AudioEffectSettings) -> Result<()>,
    ) {
        for (slot, effect) in self.processors.iter_mut().zip(settings) {
            let failed = slot.as_mut().is_some_and(|processor| {
                run(processor, effect)
                    .inspect_err(|error| {
                        messages::error(
                            "Audio plugin",
                            format!(
                                "{} failed during {operation}; bypassing it: {error:#}",
                                effect.key
                            ),
                        );
                    })
                    .is_err()
            });
            if failed {
                *slot = None;
            }
        }
    }

    fn process(&mut self, samples: &mut [f32], settings: &[AudioEffectSettings]) {
        let channels = self.channels;
        let sample_rate = self.sample_rate;
        self.apply(settings, "processing", |processor, effect| {
            processor.process(samples, channels, sample_rate, &effect.parameters, false)
        });
    }

    fn reset(&mut self, settings: &[AudioEffectSettings]) {
        let channels = self.channels;
        let sample_rate = self.sample_rate;
        self.apply(settings, "reset", |processor, effect| {
            processor.reset(channels, sample_rate, &effect.parameters)
        });
    }
}

struct AudioFxSource<S> {
    inner: S,
    chain: AudioEffectChain,
    settings: Arc<Mutex<Arc<[AudioEffectSettings]>>>,
    output: Vec<f32>,
    cursor: usize,
}

impl<S: Source> AudioFxSource<S> {
    fn new(
        inner: S,
        settings: Arc<Mutex<Arc<[AudioEffectSettings]>>>,
        runtime: Option<&mut AudioWasmRuntime>,
    ) -> Self {
        let channels = inner.channels().get() as usize;
        let sample_rate = inner.sample_rate().get();
        let initial = settings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        Self {
            chain: AudioEffectChain::new(runtime, &initial, channels, sample_rate),
            inner,
            settings,
            output: Vec::with_capacity(AUDIO_BLOCK_FRAMES.saturating_mul(channels.max(1))),
            cursor: 0,
        }
    }

    fn refill(&mut self) -> bool {
        self.output.clear();
        let channels = self.inner.channels().get() as usize;
        let capacity = AUDIO_BLOCK_FRAMES.saturating_mul(channels.max(1));
        for _ in 0..capacity {
            let Some(sample) = self.inner.next() else {
                break;
            };
            self.output.push(sample);
        }
        #[expect(
            clippy::arithmetic_side_effects,
            reason = "max(1) guarantees a nonzero channel divisor"
        )]
        let complete = self
            .output
            .len()
            .saturating_sub(self.output.len() % channels.max(1));
        self.output.truncate(complete);
        if self.output.is_empty() {
            return false;
        }
        let settings = self
            .settings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        self.chain.process(&mut self.output, &settings);
        self.cursor = 0;
        true
    }
}

impl<S: Source> Iterator for AudioFxSource<S> {
    type Item = Sample;

    fn next(&mut self) -> Option<Self::Item> {
        if self.cursor >= self.output.len() && !self.refill() {
            return None;
        }
        let sample = *self.output.get(self.cursor)?;
        self.cursor = self.cursor.saturating_add(1);
        Some(sample)
    }
}

impl<S: Source> Source for AudioFxSource<S> {
    fn current_span_len(&self) -> Option<usize> {
        self.inner.current_span_len().map(|remaining| {
            remaining.saturating_add(self.output.len().saturating_sub(self.cursor))
        })
    }
    delegate_source_metadata!(channels);
    fn try_seek(&mut self, pos: Duration) -> Result<(), rodio::source::SeekError> {
        self.output.clear();
        self.cursor = 0;
        let settings = self
            .settings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        self.chain.reset(&settings);
        self.inner.try_seek(pos)
    }
}

#[derive(Default)]
struct TrackMixControl {
    volume: AtomicU32,
    pan: AtomicU32,
}

impl TrackMixControl {
    fn new(volume: f32, pan: f32) -> Self {
        let control = Self::default();
        control.publish(volume, pan);
        control
    }
    fn publish(&self, volume: f32, pan: f32) {
        self.volume
            .store(volume.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
        self.pan
            .store(pan.clamp(-1.0, 1.0).to_bits(), Ordering::Relaxed);
    }
    fn values(&self) -> (f32, f32) {
        (
            f32::from_bits(self.volume.load(Ordering::Relaxed)),
            f32::from_bits(self.pan.load(Ordering::Relaxed)),
        )
    }
}

struct StereoTrackSource<S> {
    inner: S,
    control: Arc<TrackMixControl>,
    source_channels: usize,
    output_channel: usize,
    frame: [f32; 2],
}

impl<S: Source> StereoTrackSource<S> {
    fn new(inner: S, control: Arc<TrackMixControl>) -> Self {
        let source_channels = inner.channels().get() as usize;
        Self {
            inner,
            control,
            source_channels,
            output_channel: 0,
            frame: [0.0; 2],
        }
    }
    fn read_frame(&mut self) -> Option<()> {
        let left = self.inner.next()?;
        let right = if self.source_channels == 1 {
            left
        } else {
            self.inner.next()?
        };
        for _ in 2..self.source_channels {
            let _ = self.inner.next();
        }
        let (volume, pan) = self.control.values();
        let gains = stereo_mix_gains(volume, pan);
        self.frame = [left * gains[0], right * gains[1]];
        Some(())
    }
}

impl<S: Source> Iterator for StereoTrackSource<S> {
    type Item = Sample;
    fn next(&mut self) -> Option<Self::Item> {
        if self.output_channel == 0 {
            self.read_frame()?;
        }
        let sample = *self.frame.get(self.output_channel)?;
        self.output_channel ^= 1;
        Some(sample)
    }
}

impl<S: Source> Source for StereoTrackSource<S> {
    #[expect(
        clippy::arithmetic_side_effects,
        reason = "max(1) guarantees a nonzero channel divisor"
    )]
    fn current_span_len(&self) -> Option<usize> {
        self.inner
            .current_span_len()
            .and_then(|samples| (samples / self.source_channels.max(1)).checked_mul(2))
    }
    #[expect(clippy::expect_used, reason = "two is a valid nonzero channel count")]
    fn channels(&self) -> ChannelCount {
        ChannelCount::new(2).expect("stereo channel count")
    }
    delegate_source_metadata!();
    fn try_seek(&mut self, pos: Duration) -> Result<(), rodio::source::SeekError> {
        self.output_channel = 0;
        self.frame = [0.0; 2];
        self.inner.try_seek(pos)
    }
}

fn audio_pipeline_settings(
    project: &Project,
    plugins: &PluginRegistry,
    instance: &PipelineInstance,
    keyframe_time: f64,
    local_time: f64,
) -> Vec<AudioEffectSettings> {
    #[expect(
        clippy::cast_sign_loss,
        reason = "keyframe time is clamped to zero and frame rate to at least one before conversion"
    )]
    fn append_pipeline_settings(
        project: &Project,
        plugins: &PluginRegistry,
        pipeline: &crate::effects::EffectPipeline,
        instance: Option<&PipelineInstance>,
        times: [f64; 2],
        visiting: &mut HashSet<u64>,
        output: &mut Vec<AudioEffectSettings>,
    ) {
        let [keyframe_time, local_time] = times;
        if pipeline.kind != PipelineKind::Audio || !visiting.insert(pipeline.id) {
            return;
        }
        let frame_rate = project.active_settings().frame_rate.max(1.0);
        let context = ValueEvalContext {
            timeline_time: keyframe_time,
            local_time,
            frame_index: (keyframe_time.max(0.0) * frame_rate).floor() as u64,
            frame_rate,
        };
        let mut values = ValueEvaluator::new(&pipeline.value_nodes, context);
        for node in pipeline.main_path() {
            let mut value =
                |input: &str| resolved_node_input_cached(node, instance, input, &mut values);
            if !value("enabled").and_then(GpuValue::bool).unwrap_or(true) {
                continue;
            }
            if node.node_type == crate::effects::PIPELINE_NODE_TYPE {
                if let Some(target) = value("pipeline")
                    .and_then(GpuValue::enum_index)
                    .and_then(|index| project.pipeline_node_target_index(pipeline.id, index))
                {
                    append_pipeline_settings(
                        project, plugins, target, None, times, visiting, output,
                    );
                }
                continue;
            }
            let Some(definition) = plugins.audio_effect(&node.node_type) else {
                continue;
            };
            let parameters = Arc::new(
                definition
                    .inputs
                    .iter()
                    .filter_map(|input| {
                        let parameter = if matches!(
                            input.ty,
                            crate::plugin::InputType::Text
                                | crate::plugin::InputType::Vec2Array
                                | crate::plugin::InputType::F32List
                        ) {
                            node.host_inputs
                                .get(&input.id)
                                .and_then(|binding| binding.evaluate(keyframe_time))
                        } else {
                            value(&input.id).map(HostValue::Gpu)
                        }?;
                        Some((plugin_parameter_hash(&input.id), parameter))
                    })
                    .collect(),
            );
            output.push(AudioEffectSettings {
                key: definition.key.clone(),
                module: definition.module.clone(),
                entry: definition.entry.clone(),
                parameters,
            });
        }
        visiting.remove(&pipeline.id);
    }

    let Some(pipeline) = instance.pipeline.and_then(|id| project.pipeline(id)) else {
        return Vec::new();
    };
    let mut output = Vec::new();
    let mut visiting = HashSet::new();
    append_pipeline_settings(
        project,
        plugins,
        pipeline,
        Some(instance),
        [keyframe_time, local_time],
        &mut visiting,
        &mut output,
    );
    output
}

struct AudioVoice {
    media: MediaId,
    path: PathBuf,
    track: u32,
    effects: Vec<AudioEffectSettings>,
    effect_settings: Arc<Mutex<Arc<[AudioEffectSettings]>>>,
    source_anchor: f32,
    mapping_offset: f32,
    last_source_time: f32,
    speed: f32,
    meter: Arc<AudioMeter>,
    control: Arc<TrackMixControl>,
    player: Player,
}

struct DesiredVoice {
    media: MediaId,
    path: PathBuf,
    track: u32,
    mapping_offset: f32,
    source_time: f32,
    volume: f32,
    pan: f32,
    speed: f32,
    effects: Vec<AudioEffectSettings>,
}

struct AudioReadiness {
    required: bool,
    sources_consuming: bool,
}

pub struct AudioEngine {
    sink: Option<MixerDeviceSink>,
    mixer: Option<Mixer>,
    master_meter: Arc<AudioMeter>,
    voices: HashMap<u64, AudioVoice>,
    failed_media: HashMap<MediaId, (PathBuf, Instant)>,
    wasm: Option<AudioWasmRuntime>,
    playing: bool,
    master_muted: bool,
    clock_lead: f32,
    last_playhead: Option<f32>,
    output_retry_at: Instant,
    output_retry_delay: Duration,
    readiness: AudioReadiness,
}

impl AudioEngine {
    pub fn new() -> Self {
        let master_meter = Arc::new(AudioMeter::default());
        let wasm = AudioWasmRuntime::new()
            .inspect_err(|error| {
                messages::error("Audio", format!("WASM runtime unavailable: {error:#}"));
            })
            .ok();
        let mut engine = Self {
            sink: None,
            mixer: None,
            master_meter,
            voices: HashMap::new(),
            failed_media: HashMap::new(),
            wasm,
            playing: false,
            master_muted: false,
            clock_lead: 0.0,
            last_playhead: None,
            output_retry_at: Instant::now(),
            output_retry_delay: Duration::from_secs(1),
            readiness: AudioReadiness {
                required: false,
                sources_consuming: false,
            },
        };
        engine.retry_output();
        engine
    }

    fn retry_output(&mut self) {
        match open_default_output() {
            Ok(sink) => {
                let channels = sink.config().channel_count();
                let sample_rate = sink.config().sample_rate();
                let (mixer, source) = mixer::mixer(channels, sample_rate);
                mixer.add(Zero::new(channels, sample_rate));
                sink.mixer()
                    .add(MeteredSource::new(source, Arc::clone(&self.master_meter)));
                self.sink = Some(sink);
                self.mixer = Some(mixer);
                self.output_retry_delay = Duration::from_secs(1);
            }
            Err(error) => {
                messages::warning(
                    "Audio",
                    format!("output unavailable; retry scheduled: {error:#?}"),
                );
                self.mixer = None;
                self.sink = None;
                self.output_retry_at = Instant::now()
                    .checked_add(self.output_retry_delay)
                    .unwrap_or_else(Instant::now);
                self.output_retry_delay = self
                    .output_retry_delay
                    .saturating_mul(2)
                    .min(Duration::from_secs(30));
            }
        }
    }

    pub const fn playback_ready(&self) -> (bool, bool, bool) {
        (
            self.sink.is_some(),
            self.readiness.required,
            self.readiness.sources_consuming,
        )
    }

    pub fn clear(&mut self) {
        for voice in self.voices.values() {
            voice.player.stop();
        }
        self.voices.clear();
        self.failed_media.clear();
        if let Some(wasm) = &mut self.wasm {
            wasm.clear();
        }
        self.playing = false;
        self.clock_lead = 0.0;
        self.last_playhead = None;
        self.readiness.sources_consuming = false;
    }

    pub const fn set_master_muted(&mut self, muted: bool) {
        self.master_muted = muted;
    }

    pub fn sync(&mut self, project: &Project, timeline: &TimelineState, plugins: &PluginRegistry) {
        let playhead = timeline.playhead();
        let playing = timeline.is_playing() && !timeline.is_scrubbing();
        let started_playing = playing && !self.playing;
        let transport_rewound = self
            .last_playhead
            .is_some_and(|previous| playhead + AUDIO_DRIFT_TOLERANCE < previous);
        self.last_playhead = Some(playhead);
        self.playing = playing;
        self.clock_lead = 0.0;
        let mut desired = HashMap::<u64, DesiredVoice>::new();
        collect_desired_voices(
            project,
            plugins,
            project.active_composition,
            timeline,
            playhead,
            &mut desired,
        );
        self.readiness.required = !desired.is_empty();
        let desired_voice_count = desired.len();
        if self.mixer.is_none() && Instant::now() >= self.output_retry_at {
            self.retry_output();
        }
        let Some(mixer) = self.mixer.as_ref() else {
            self.playing = false;
            self.clock_lead = 0.0;
            self.readiness.sources_consuming = false;
            return;
        };

        self.voices.retain(|clip, voice| {
            let keep = desired.contains_key(clip);
            if !keep {
                voice.player.stop();
            }
            keep
        });

        let mut positive_leads = Vec::new();
        for (clip, desired) in desired {
            if self
                .failed_media
                .get(&desired.media)
                .is_some_and(|(path, failed_at)| {
                    path == &desired.path && failed_at.elapsed() < AUDIO_RETRY_DELAY
                })
            {
                continue;
            }

            let replace = self.voices.get(&clip).is_some_and(|voice| {
                voice.media != desired.media
                    || voice.path != desired.path
                    || voice.effects.len() != desired.effects.len()
                    || !voice
                        .effects
                        .iter()
                        .zip(&desired.effects)
                        .all(|(current, next)| current.same_processor(next))
                    || (voice.mapping_offset - desired.mapping_offset).abs() > f32::EPSILON
                    || (voice.speed - desired.speed).abs() > f32::EPSILON
                    || AUDIO_DRIFT_TOLERANCE.mul_add(desired.speed.max(0.01), desired.source_time)
                        < voice.source_anchor
                    || voice.player.empty()
            });
            if replace && let Some(voice) = self.voices.remove(&clip) {
                voice.player.stop();
            }

            if let std::collections::hash_map::Entry::Vacant(entry) = self.voices.entry(clip) {
                match open_voice(
                    mixer,
                    self.wasm.as_mut(),
                    &desired,
                    playing,
                    self.master_muted,
                ) {
                    Ok(voice) => {
                        self.failed_media.remove(&desired.media);
                        entry.insert(voice);
                    }
                    Err(error) => {
                        messages::error(
                            "Audio decode",
                            format!("{}: {error:#}", desired.path.display()),
                        );
                        self.failed_media
                            .insert(desired.media, (desired.path.clone(), Instant::now()));
                        continue;
                    }
                }
            }

            let Some(voice) = self.voices.get_mut(&clip) else {
                continue;
            };
            voice.track = desired.track;
            if voice.effects != desired.effects {
                voice.effects.clone_from(&desired.effects);
                *voice
                    .effect_settings
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Arc::<[AudioEffectSettings]>::from(desired.effects.clone());
            }
            voice.control.publish(
                if self.master_muted {
                    0.0
                } else {
                    desired.volume
                },
                desired.pan,
            );
            let player_time =
                (desired.source_time - voice.source_anchor).max(0.0) / desired.speed.max(0.01);
            let actual = voice.player.get_pos().as_secs_f32();
            let source_rewound = AUDIO_DRIFT_TOLERANCE
                .mul_add(desired.speed.max(0.01), desired.source_time)
                < voice.last_source_time;
            let should_seek = if playing {
                if started_playing {
                    (actual - player_time).abs() > AUDIO_DRIFT_TOLERANCE
                } else {
                    ((transport_rewound || source_rewound)
                        && (actual - player_time).abs() > AUDIO_DRIFT_TOLERANCE)
                        || actual + AUDIO_DRIFT_TOLERANCE < player_time
                }
            } else {
                (actual - player_time).abs() > AUDIO_DRIFT_TOLERANCE
            };
            if should_seek {
                if let Err(error) = voice.player.try_seek(Duration::from_secs_f32(player_time)) {
                    messages::warning(
                        "Audio",
                        format!("seek failed for {}: {error}", voice.path.display()),
                    );
                }
            } else if playing && !started_playing {
                let lead = actual - player_time;
                if lead > 0.025 && lead.is_finite() {
                    positive_leads.push(lead);
                }
            }
            voice.last_source_time = desired.source_time;
            if playing {
                voice.player.play();
            } else {
                voice.player.pause();
            }
        }

        if playing && !started_playing && !positive_leads.is_empty() {
            positive_leads.sort_by(f32::total_cmp);
            if let Some(lead) = positive_leads.get(positive_leads.len() / 2) {
                self.clock_lead = lead.clamp(0.0, 0.250);
            }
        }
        self.readiness.sources_consuming = desired_voice_count == 0
            || (playing
                && self.voices.len() == desired_voice_count
                && self
                    .voices
                    .values()
                    .all(|voice| voice.player.get_pos() >= Duration::from_millis(5)));
    }

    pub const fn clock_lead(&self) -> f32 {
        self.clock_lead
    }

    pub fn track_levels(&self) -> HashMap<u32, [f32; 2]> {
        if !self.playing {
            return HashMap::new();
        }
        let mut levels = HashMap::<u32, [f32; 2]>::new();
        for voice in self.voices.values() {
            let source = voice.meter.levels();
            let entry = levels.entry(voice.track).or_insert([0.0; 2]);
            entry[0] = entry[0].max(source[0]).min(1.0);
            entry[1] = entry[1].max(source[1]).min(1.0);
        }
        levels
    }

    pub fn master_levels(&self) -> [f32; 2] {
        if self.playing && !self.master_muted {
            self.master_meter.levels()
        } else {
            [0.0; 2]
        }
    }
}

fn open_default_output() -> Result<MixerDeviceSink> {
    use rodio::cpal::traits::{DeviceTrait, HostTrait};

    let host = rodio::cpal::default_host();
    let device = host
        .default_output_device()
        .context("the operating system has no default audio output device")?;
    let device_name = device.description().map_or_else(
        |error| format!("unnamed device ({error:?})"),
        |description| description.name().to_owned(),
    );
    let config = device.default_output_config().with_context(|| {
        format!("read default output config for selected device {device_name:?}")
    })?;
    let config_description = format!(
        "{} channels, {} Hz, {:?}",
        config.channels(),
        config.sample_rate(),
        config.sample_format()
    );
    let supported_configs = match device.supported_output_configs() {
        Ok(configs) => configs
            .map(|range| {
                format!(
                    "{}ch {}-{}Hz {:?}",
                    range.channels(),
                    range.min_sample_rate(),
                    range.max_sample_rate(),
                    range.sample_format()
                )
            })
            .collect::<Vec<_>>(),
        Err(error) => vec![format!("configuration query failed: {error:?}")],
    };
    let sink = rodio::DeviceSinkBuilder::from_device(device)
        .with_context(|| format!("configure selected output {device_name:?} at {config_description}"))?
        .with_buffer_size(rodio::cpal::BufferSize::Default)
        .open_stream()
        .with_context(|| {
            format!(
                "open selected output {device_name:?} at {config_description}; supported configs: {supported_configs:?}"
            )
        })?;
    Ok(sink)
}

const AUDIO_WORKER_PROTOCOL: u16 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AudioProjectSnapshot {
    project: Project,
    worker_state: crate::project::WorkerProjectState,
    timeline: TimelineDocument,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AudioWorkerRequest {
    Hello {
        protocol: u16,
        plugin_paths: String,
    },
    Project {
        revision: u64,
        snapshot: Box<AudioProjectSnapshot>,
    },
    Transport {
        revision: u64,
        project_revision: u64,
        generation: u64,
        playhead: f32,
        playing: bool,
        scrubbing: bool,
        master_muted: bool,
    },
    Clear,
    Shutdown,
    #[cfg(debug_assertions)]
    Crash,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AudioWorkerResponse {
    Ready {
        protocol: u16,
        pid: u32,
    },
    State {
        revision: u64,
        project_revision: u64,
        generation: u64,
        playhead: f32,
        clock_lead: f32,
        #[serde(with = "audio_track_levels")]
        track_levels: HashMap<u32, [f32; 2]>,
        master_levels: [f32; 2],
        output_ready: bool,
        audio_required: bool,
        audio_sources_ready: bool,
        diagnostics: Vec<(bool, String, String)>,
    },
    ProjectAccepted {
        revision: u64,
        project_name: String,
        active_composition: CompositionId,
    },
    Error {
        message: String,
    },
    Stopped,
}

mod audio_track_levels {
    use std::collections::HashMap;

    use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};

    pub(super) fn serialize<S>(
        levels: &HashMap<u32, [f32; 2]>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        levels
            .iter()
            .map(|(track, level)| (track.to_string(), level))
            .collect::<HashMap<_, _>>()
            .serialize(serializer)
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<HashMap<u32, [f32; 2]>, D::Error>
    where
        D: Deserializer<'de>,
    {
        HashMap::<String, [f32; 2]>::deserialize(deserializer)?
            .into_iter()
            .map(|(track, level)| {
                track
                    .parse::<u32>()
                    .map(|track| (track, level))
                    .map_err(|error| {
                        D::Error::custom(format!("invalid audio track id {track:?}: {error}"))
                    })
            })
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioWorkerStatus {
    Starting,
    Ready { pid: u32 },
    Crashed,
    Unresponsive,
    Restarting,
    StartupFailed,
}

struct AudioWorkerProcess {
    child: Option<Child>,
    requests: Option<mpsc::SyncSender<AudioWorkerRequest>>,
    responses: mpsc::Receiver<AudioWorkerResponse>,
    diagnostics: Arc<Mutex<VecDeque<String>>>,
    metrics: Arc<Mutex<Option<(f32, u64)>>>,
    threads: Vec<thread::JoinHandle<()>>,
    pid: u32,
}

struct ChildGuard(Option<Child>);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl AudioWorkerProcess {
    fn spawn(plugin_paths: &str) -> Result<Self> {
        let executable = std::env::current_exe().context("locate Kama executable")?;
        let mut command = Command::new(executable);
        #[cfg(not(all(test, debug_assertions)))]
        command.arg("--kama-worker=audio");
        #[cfg(all(test, debug_assertions))]
        command
            .arg("--exact")
            .arg("audio::tests::audio_worker_child_entry")
            .arg("--nocapture")
            .env("KAMA_AUDIO_WORKER_TEST_CHILD", "1");
        let child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("spawn audio worker")?;
        let mut guard = ChildGuard(Some(child));
        let child = guard
            .0
            .as_mut()
            .context("audio worker process unavailable")?;
        let pid = child.id();
        let stdin = child
            .stdin
            .take()
            .context("audio worker stdin unavailable")?;
        let stdout = child
            .stdout
            .take()
            .context("audio worker stdout unavailable")?;
        let stderr = child
            .stderr
            .take()
            .context("audio worker stderr unavailable")?;
        let (request_tx, request_rx) = mpsc::sync_channel(4);
        let (response_tx, response_rx) = mpsc::sync_channel(64);
        let diagnostics = Arc::new(Mutex::new(VecDeque::new()));
        let metrics = Arc::new(Mutex::new(None));
        let diagnostic_log = Arc::clone(&diagnostics);
        let writer = thread::Builder::new()
            .name(format!("kama-audio-stdin-{pid}"))
            .spawn(move || audio_worker_writer(stdin, request_rx))
            .context("start audio worker writer")?;
        let reader = thread::Builder::new()
            .name(format!("kama-audio-stdout-{pid}"))
            .spawn(move || {
                audio_worker_reader(stdout, &response_tx, cfg!(all(test, debug_assertions)));
            })
            .context("start audio worker reader")?;
        let diagnostics_reader = thread::Builder::new()
            .name(format!("kama-audio-stderr-{pid}"))
            .spawn(move || audio_worker_diagnostics(stderr, &diagnostic_log))
            .context("start audio worker diagnostics reader")?;
        let metrics_output = Arc::downgrade(&metrics);
        thread::Builder::new()
            .name(format!("kama-audio-metrics-{pid}"))
            .spawn(move || audio_worker_metrics(pid, &metrics_output))
            .context("start audio worker metrics sampler")?;
        request_tx
            .try_send(AudioWorkerRequest::Hello {
                protocol: AUDIO_WORKER_PROTOCOL,
                plugin_paths: plugin_paths.to_owned(),
            })
            .map_err(|error| anyhow::anyhow!("send audio worker handshake: {error}"))?;
        Ok(Self {
            child: guard
                .0
                .take()
                .context("audio worker process unavailable")?
                .into(),
            requests: Some(request_tx),
            responses: response_rx,
            diagnostics,
            metrics,
            threads: vec![writer, reader, diagnostics_reader],
            pid,
        })
    }

    fn send(&self, request: AudioWorkerRequest) -> Result<()> {
        self.requests
            .as_ref()
            .context("audio worker is stopping")?
            .try_send(request)
            .map_err(|error| anyhow::anyhow!("audio worker request queue unavailable: {error}"))
    }

    fn stop(&mut self) {
        if let Some(requests) = self.requests.take() {
            let _ = requests.try_send(AudioWorkerRequest::Shutdown);
            drop(requests);
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            thread::spawn(move || {
                let _ = child.wait();
            });
        }
        self.threads.clear();
    }
}

fn audio_worker_metrics(pid: u32, output: &std::sync::Weak<Mutex<Option<(f32, u64)>>>) {
    #[cfg(unix)]
    while let Some(output) = output.upgrade() {
        let result = std::process::Command::new("ps")
            .env("LC_ALL", "C")
            .args(["-o", "%cpu=", "-o", "rss=", "-p", &pid.to_string()])
            .output();
        let Ok(result) = result else { break };
        if !result.status.success() {
            break;
        }
        let text = String::from_utf8_lossy(&result.stdout);
        let mut fields = text.split_whitespace();
        let (Some(cpu), Some(rss_kib)) = (fields.next(), fields.next()) else {
            break;
        };
        if let (Ok(cpu), Ok(rss_kib)) = (cpu.parse::<f32>(), rss_kib.parse::<u64>())
            && let Ok(mut metrics) = output.lock()
        {
            *metrics = Some((cpu, rss_kib.saturating_mul(1024)));
        }
        drop(output);
        thread::sleep(Duration::from_secs(1));
    }
    #[cfg(not(unix))]
    let _ = (pid, output);
}

impl Drop for AudioWorkerProcess {
    fn drop(&mut self) {
        self.stop();
    }
}

fn read_bounded_line(
    reader: &mut impl std::io::Read,
    limit: usize,
) -> std::io::Result<Option<String>> {
    let mut bytes = Vec::new();
    let mut byte = [0_u8; 1];
    let mut truncated = false;
    loop {
        match reader.read(&mut byte)? {
            0 if bytes.is_empty() => return Ok(None),
            0 => break,
            _ if byte[0] == b'\n' => break,
            _ if bytes.len() < limit => bytes.push(byte[0]),
            _ => truncated = true,
        }
    }
    while bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    let mut line = String::from_utf8_lossy(&bytes).into_owned();
    if truncated {
        line.push_str(" [truncated]");
    }
    Ok(Some(line))
}

fn audio_worker_diagnostics(stderr: impl std::io::Read, output: &Arc<Mutex<VecDeque<String>>>) {
    let mut reader = BufReader::new(stderr);
    while let Ok(Some(line)) = read_bounded_line(&mut reader, 1024) {
        if let Ok(mut output) = output.lock() {
            output.push_back(line);
            while output.len() > 32 {
                output.pop_front();
            }
        }
    }
}

fn audio_worker_writer(stdin: ChildStdin, requests: mpsc::Receiver<AudioWorkerRequest>) {
    let mut writer = BufWriter::new(stdin);
    for request in requests {
        if serde_json::to_writer(&mut writer, &request)
            .and_then(|()| writer.write_all(b"\n").map_err(serde_json::Error::io))
            .and_then(|()| writer.flush().map_err(serde_json::Error::io))
            .is_err()
        {
            break;
        }
        if matches!(request, AudioWorkerRequest::Shutdown) {
            break;
        }
    }
}

fn audio_worker_reader(
    stdout: impl std::io::Read,
    responses: &mpsc::SyncSender<AudioWorkerResponse>,
    allow_test_harness_output: bool,
) {
    let mut reader = BufReader::new(stdout);
    loop {
        let line = match read_bounded_line(&mut reader, 1024 * 1024) {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(error) => {
                let _ = responses.send(AudioWorkerResponse::Error {
                    message: format!("read audio worker protocol: {error}"),
                });
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str(&line) {
            Ok(response) => response,
            Err(_) if allow_test_harness_output && is_test_harness_output(&line) => continue,
            Err(error) => {
                if responses
                    .send(AudioWorkerResponse::Error {
                        message: format!(
                            "decode audio worker response: {error}; prefix={}",
                            line.chars().take(512).collect::<String>()
                        ),
                    })
                    .is_err()
                {
                    break;
                }
                continue;
            }
        };
        if responses.send(response).is_err() {
            break;
        }
    }
}

fn is_test_harness_output(line: &str) -> bool {
    let line = line.trim();
    line.starts_with("running ")
        || line.starts_with("test ")
        || line.starts_with("failures:")
        || line.starts_with("test result:")
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "worker lifecycle and transport flags are independent protocol states"
)]
pub struct AudioPlayback {
    process: Option<AudioWorkerProcess>,
    last_pid: Option<u32>,
    plugin_paths: String,
    project_revision: u64,
    project_key: Option<(u64, CompositionId, u64)>,
    live_snapshot_revision: u64,
    last_live_snapshot: Option<Instant>,
    sent_project_revision: Option<u64>,
    latest_snapshot: Option<AudioProjectSnapshot>,
    transport_revision: u64,
    generation: u64,
    timeline_transport_key: Option<(u64, bool, bool)>,
    applied_transport_revision: u64,
    master_muted: bool,
    playing: bool,
    pause_after_failure: bool,
    clear_pending: bool,
    transport_pending: bool,
    clock_playhead: f32,
    clock_lead: f32,
    track_levels: HashMap<u32, [f32; 2]>,
    master_levels: [f32; 2],
    crash_reported: bool,
    accepted_project_revision: Option<u64>,
    accepted_project_name: Option<String>,
    status: AudioWorkerStatus,
    startup_deadline: Instant,
    last_response_at: Instant,
    transport_waiting_since: Option<Instant>,
    ready_generation: Option<(u64, u64)>,
    readiness_target: f32,
    output_ready: bool,
    audio_required: bool,
    audio_sources_ready: bool,
    video_ready: bool,
    #[cfg(debug_assertions)]
    crash_requested: bool,
}

impl AudioPlayback {
    pub fn new(plugin_paths: &str) -> Self {
        let process = match AudioWorkerProcess::spawn(plugin_paths) {
            Ok(process) => Some(process),
            Err(error) => {
                messages::error("Audio worker", format!("startup failed: {error:#}"));
                None
            }
        };
        let status = if process.is_some() {
            AudioWorkerStatus::Starting
        } else {
            AudioWorkerStatus::StartupFailed
        };
        let last_pid = process.as_ref().map(|process| process.pid);
        Self {
            process,
            last_pid,
            plugin_paths: plugin_paths.to_owned(),
            project_revision: 0,
            project_key: None,
            live_snapshot_revision: 0,
            last_live_snapshot: None,
            sent_project_revision: None,
            latest_snapshot: None,
            transport_revision: 0,
            generation: 0,
            timeline_transport_key: None,
            applied_transport_revision: 0,
            master_muted: false,
            playing: false,
            pause_after_failure: false,
            clear_pending: false,
            transport_pending: false,
            clock_playhead: 0.0,
            clock_lead: 0.0,
            track_levels: HashMap::new(),
            master_levels: [0.0; 2],
            crash_reported: false,
            accepted_project_revision: None,
            accepted_project_name: None,
            status,
            startup_deadline: deadline_after(Duration::from_secs(10)),
            last_response_at: Instant::now(),
            transport_waiting_since: None,
            ready_generation: None,
            readiness_target: 0.0,
            output_ready: false,
            audio_required: false,
            audio_sources_ready: false,
            video_ready: false,
            #[cfg(debug_assertions)]
            crash_requested: false,
        }
    }

    pub fn clear(&mut self) {
        self.latest_snapshot = None;
        self.project_key = None;
        self.sent_project_revision = None;
        self.playing = false;
        self.clock_lead = 0.0;
        self.track_levels.clear();
        self.master_levels = [0.0; 2];
        self.accepted_project_revision = None;
        self.transport_waiting_since = None;
        self.generation = self.generation.saturating_add(1);
        self.clear_pending = true;
        self.timeline_transport_key = None;
    }

    pub const fn set_master_muted(&mut self, muted: bool) {
        self.master_muted = muted;
    }

    pub fn playback_ready(&self, timeline: &TimelineState) -> bool {
        if !timeline.is_playing() || timeline.is_scrubbing() {
            return true;
        }
        if self.timeline_transport_key
            != Some((
                timeline.seek_revision(),
                timeline.is_playing(),
                timeline.is_scrubbing(),
            ))
        {
            return false;
        }
        audio_startup_ready(
            self.ready_generation,
            (self.project_revision, self.generation),
            self.audio_required,
            self.output_ready,
            self.audio_sources_ready,
        )
    }

    pub const fn set_video_ready(&mut self, ready: bool) {
        self.video_ready = ready;
    }

    #[cfg(debug_assertions)]
    pub const fn crash_for_test(&mut self) {
        self.crash_requested = matches!(self.status, AudioWorkerStatus::Ready { .. });
    }

    pub fn sync(
        &mut self,
        project: &Project,
        timeline: &TimelineState,
        revision: u64,
        live_editing: bool,
    ) -> bool {
        let crashed = self.poll_worker();
        let transport_key = (
            timeline.seek_revision(),
            timeline.is_playing(),
            timeline.is_scrubbing(),
        );
        if self.timeline_transport_key != Some(transport_key) {
            self.timeline_transport_key = Some(transport_key);
            self.generation = self.generation.wrapping_add(1);
            self.clock_lead = 0.0;
            self.track_levels.clear();
            self.master_levels = [0.0; 2];
            self.ready_generation = None;
            self.readiness_target = timeline.playhead();
        }
        if live_editing
            && self
                .last_live_snapshot
                .is_none_or(|last| last.elapsed() >= Duration::from_millis(50))
        {
            self.live_snapshot_revision = self.live_snapshot_revision.saturating_add(1);
            self.last_live_snapshot = Some(Instant::now());
        }
        let key = (
            revision,
            project.active_composition,
            self.live_snapshot_revision,
        );
        let snapshot_changed = self.project_key != Some(key);
        if snapshot_changed {
            self.clock_lead = 0.0;
            self.transport_waiting_since = None;
            let mut snapshot = project.clone();
            snapshot.sync_active_timeline(timeline.document());
            self.latest_snapshot = Some(AudioProjectSnapshot {
                worker_state: crate::project::WorkerProjectState::capture(&snapshot),
                project: snapshot,
                timeline: timeline.document(),
            });
            self.project_revision = self.project_revision.saturating_add(1);
            self.project_key = Some(key);
            self.ready_generation = None;
            self.readiness_target = timeline.playhead();
        }
        let mut invalid_ready = false;
        if let Some(process) = &self.process {
            if self.clear_pending && process.send(AudioWorkerRequest::Clear).is_ok() {
                self.clear_pending = false;
            }
            #[cfg(debug_assertions)]
            if self.crash_requested && process.send(AudioWorkerRequest::Crash).is_ok() {
                self.crash_requested = false;
            }
            let worker_revision = self.project_revision;
            if !self.clear_pending
                && self.sent_project_revision != Some(worker_revision)
                && let Some(snapshot) = &self.latest_snapshot
                && process
                    .send(AudioWorkerRequest::Project {
                        revision: worker_revision,
                        snapshot: Box::new(snapshot.clone()),
                    })
                    .is_ok()
            {
                self.sent_project_revision = Some(worker_revision);
                self.last_response_at = Instant::now();
            }
            let project_ready =
                !self.clear_pending && self.accepted_project_revision == Some(worker_revision);
            self.transport_revision = self.transport_revision.saturating_add(1);
            if !timeline.is_playing() {
                self.pause_after_failure = false;
            }
            self.playing = project_ready
                && timeline.is_playing()
                && !timeline.is_scrubbing()
                && self.video_ready
                && !self.pause_after_failure;
            if project_ready {
                self.transport_pending = process
                    .send(AudioWorkerRequest::Transport {
                        revision: self.transport_revision,
                        project_revision: worker_revision,
                        generation: self.generation,
                        playhead: timeline.playhead(),
                        playing: self.playing,
                        scrubbing: timeline.is_scrubbing(),
                        master_muted: self.master_muted,
                    })
                    .is_err();
                if !self.transport_pending && self.transport_waiting_since.is_none() {
                    self.transport_waiting_since = Some(Instant::now());
                }
            }
            while let Ok(response) = process.responses.try_recv() {
                match response {
                    AudioWorkerResponse::State {
                        revision,
                        project_revision,
                        generation,
                        playhead,
                        clock_lead,
                        track_levels,
                        master_levels,
                        output_ready,
                        audio_required,
                        audio_sources_ready,
                        diagnostics,
                        ..
                    } => {
                        self.last_response_at = Instant::now();
                        self.transport_waiting_since = None;
                        if project_revision == self.project_revision
                            && generation == self.generation
                            && revision >= self.applied_transport_revision
                            && revision <= self.transport_revision
                        {
                            self.applied_transport_revision = revision;
                            self.clock_playhead = playhead;
                            self.clock_lead = clock_lead;
                            self.track_levels = track_levels;
                            self.master_levels = master_levels;
                            self.output_ready = output_ready;
                            self.audio_required = audio_required;
                            self.audio_sources_ready = audio_sources_ready;
                            if (playhead - self.readiness_target).abs() <= 0.0005 {
                                self.ready_generation = Some((project_revision, generation));
                            }
                            for (is_error, source, message) in diagnostics {
                                if is_error {
                                    messages::error(source, message);
                                } else {
                                    messages::warning(source, message);
                                }
                            }
                        }
                    }
                    AudioWorkerResponse::ProjectAccepted {
                        revision,
                        project_name,
                        ..
                    } => {
                        self.last_response_at = Instant::now();
                        self.accepted_project_revision = Some(revision);
                        self.accepted_project_name = Some(project_name);
                    }
                    AudioWorkerResponse::Error { message } => {
                        messages::error("Audio worker", message);
                    }
                    AudioWorkerResponse::Ready { protocol, pid } => {
                        if protocol == AUDIO_WORKER_PROTOCOL && pid == process.pid {
                            self.status = AudioWorkerStatus::Ready { pid };
                            self.startup_deadline = deadline_after(Duration::from_secs(10));
                            self.last_response_at = Instant::now();
                        } else {
                            invalid_ready = true;
                        }
                    }
                    AudioWorkerResponse::Stopped => {}
                }
            }
        }
        if invalid_ready {
            self.fail_worker("invalid ready handshake".to_owned());
        }
        let unhealthy = self.check_health();
        crashed || invalid_ready || unhealthy
    }

    fn poll_worker(&mut self) -> bool {
        let Some(process) = &mut self.process else {
            return false;
        };
        let Some(child) = process.child.as_mut() else {
            self.fail_worker("audio worker lost its process handle".to_owned());
            return true;
        };
        match child.try_wait() {
            Ok(Some(status)) => {
                let pid = process.pid;
                let was_starting = matches!(
                    self.status,
                    AudioWorkerStatus::Starting | AudioWorkerStatus::Restarting
                );
                let diagnostics = process
                    .diagnostics
                    .lock()
                    .map(|lines| lines.iter().cloned().collect::<Vec<_>>().join("\n"))
                    .unwrap_or_default();
                self.process = None;
                self.playing = false;
                self.pause_after_failure = true;
                self.clock_lead = 0.0;
                self.track_levels.clear();
                self.master_levels = [0.0; 2];
                self.ready_generation = None;
                self.output_ready = false;
                if !self.crash_reported {
                    messages::error(
                        "Audio worker",
                        format!(
                            "process {pid} crashed ({status}); playback paused. Restart manually.{}",
                            if diagnostics.is_empty() {
                                " No diagnostic was captured.".to_owned()
                            } else {
                                format!("\n{diagnostics}")
                            }
                        ),
                    );
                    self.crash_reported = true;
                }
                self.status = if was_starting {
                    AudioWorkerStatus::StartupFailed
                } else {
                    AudioWorkerStatus::Crashed
                };
                true
            }
            Ok(None) => false,
            Err(error) => {
                self.fail_worker(format!("cannot inspect child: {error}"));
                true
            }
        }
    }

    fn check_health(&mut self) -> bool {
        if self.process.is_none() {
            return false;
        }
        let reason = match self.status {
            AudioWorkerStatus::Starting | AudioWorkerStatus::Restarting
                if Instant::now() > self.startup_deadline =>
            {
                "audio worker startup timed out"
            }
            AudioWorkerStatus::Ready { .. }
                if self.accepted_project_revision.is_some()
                    && self
                        .transport_waiting_since
                        .is_some_and(|started| started.elapsed() > Duration::from_secs(3)) =>
            {
                "audio worker stopped responding for 3 seconds"
            }
            AudioWorkerStatus::Ready { .. }
                if self.sent_project_revision.is_some()
                    && self.accepted_project_revision != Some(self.project_revision)
                    && self.last_response_at.elapsed() > Duration::from_secs(10) =>
            {
                "audio worker did not initialize its project"
            }
            _ => return false,
        };
        self.fail_worker(reason.to_owned());
        true
    }

    fn fail_worker(&mut self, reason: String) {
        if let Some(mut process) = self.process.take() {
            process.stop();
        }
        self.playing = false;
        self.pause_after_failure = true;
        self.clock_lead = 0.0;
        self.track_levels.clear();
        self.master_levels = [0.0; 2];
        self.ready_generation = None;
        self.output_ready = false;
        self.status = AudioWorkerStatus::Unresponsive;
        if !self.crash_reported {
            messages::error("Audio worker", reason);
            self.crash_reported = true;
        }
    }

    pub fn restart(&mut self) {
        if !matches!(
            self.status,
            AudioWorkerStatus::Crashed
                | AudioWorkerStatus::StartupFailed
                | AudioWorkerStatus::Unresponsive
        ) {
            return;
        }
        self.status = AudioWorkerStatus::Restarting;
        self.crash_reported = false;
        if let Some(mut process) = self.process.take() {
            process.stop();
        }
        self.generation = self.generation.saturating_add(1);
        self.playing = false;
        self.pause_after_failure = true;
        self.accepted_project_revision = None;
        self.sent_project_revision = None;
        self.clear_pending = false;
        #[cfg(debug_assertions)]
        {
            self.crash_requested = false;
        }
        self.clock_lead = 0.0;
        self.track_levels.clear();
        self.master_levels = [0.0; 2];
        self.ready_generation = None;
        self.output_ready = false;
        self.start_worker();
    }

    fn start_worker(&mut self) {
        match AudioWorkerProcess::spawn(&self.plugin_paths) {
            Ok(process) => {
                self.last_pid = Some(process.pid);
                let mut sent_project_revision = None;
                if let Some(snapshot) = &self.latest_snapshot {
                    let revision = self.project_revision;
                    if process
                        .send(AudioWorkerRequest::Project {
                            revision,
                            snapshot: Box::new(snapshot.clone()),
                        })
                        .is_ok()
                    {
                        sent_project_revision = Some(revision);
                    }
                }
                self.process = Some(process);
                self.status = AudioWorkerStatus::Restarting;
                self.startup_deadline = deadline_after(Duration::from_secs(10));
                self.last_response_at = Instant::now();
                self.transport_waiting_since = None;
                self.crash_reported = false;
                self.accepted_project_revision = None;
                self.accepted_project_name = None;
                self.sent_project_revision = sent_project_revision;
            }
            Err(error) => {
                self.status = AudioWorkerStatus::StartupFailed;
                if !self.crash_reported {
                    messages::error("Audio worker", format!("restart failed: {error:#}"));
                    self.crash_reported = true;
                }
            }
        }
    }

    pub const fn status(&self) -> AudioWorkerStatus {
        self.status
    }

    pub const fn pid(&self) -> Option<u32> {
        self.last_pid
    }

    pub fn needs_polling(&self) -> bool {
        self.process.is_some()
            && (self.clear_pending
                || self.transport_pending
                || self.accepted_project_revision != Some(self.project_revision)
                || matches!(
                    self.status,
                    AudioWorkerStatus::Starting | AudioWorkerStatus::Restarting
                ))
    }

    pub fn resource_metrics(&self) -> Option<(f32, u64)> {
        self.process
            .as_ref()?
            .metrics
            .lock()
            .ok()
            .and_then(|value| *value)
    }

    pub fn take_clock_target(&mut self) -> Option<f32> {
        let lead = std::mem::take(&mut self.clock_lead);
        let target = self.clock_playhead + lead;
        (self.playing && lead > 0.0 && target.is_finite()).then_some(target)
    }

    pub fn track_levels(&self) -> HashMap<u32, [f32; 2]> {
        if self.playing {
            self.track_levels.clone()
        } else {
            HashMap::new()
        }
    }

    pub const fn master_levels(&self) -> [f32; 2] {
        if self.playing && !self.master_muted {
            self.master_levels
        } else {
            [0.0; 2]
        }
    }
}

#[expect(
    clippy::significant_drop_tightening,
    reason = "the stdin lock must remain held while reading subsequent worker requests"
)]
pub fn run_audio_worker() -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = BufWriter::new(std::io::stdout().lock());
    let mut lines = stdin.lock().lines();
    let hello = lines
        .next()
        .context("audio worker input closed before handshake")??;
    let request: AudioWorkerRequest = serde_json::from_str(&hello).context("invalid handshake")?;
    let AudioWorkerRequest::Hello {
        protocol,
        plugin_paths,
    } = request
    else {
        anyhow::bail!("audio worker expected handshake as first request");
    };
    if protocol != AUDIO_WORKER_PROTOCOL {
        write_audio_response(
            &mut stdout,
            &AudioWorkerResponse::Error {
                message: format!("unsupported protocol version {protocol}"),
            },
        )?;
        return Ok(());
    }
    let plugins = PluginRegistry::load_default(&plugin_paths).context("load audio plugins")?;
    write_audio_response(
        &mut stdout,
        &AudioWorkerResponse::Ready {
            protocol,
            pid: std::process::id(),
        },
    )?;
    let mut engine: Option<AudioEngine> = None;
    let mut project: Option<Project> = None;
    let mut timeline: Option<TimelineState> = None;
    let mut project_revision = None;
    let mut transport_revision = 0_u64;
    let mut transport_generation = 0_u64;
    for line in lines {
        let line = line.context("read audio worker request")?;
        let request: AudioWorkerRequest = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(error) => {
                write_audio_response(
                    &mut stdout,
                    &AudioWorkerResponse::Error {
                        message: format!("invalid request: {error}"),
                    },
                )?;
                continue;
            }
        };
        match request {
            AudioWorkerRequest::Project { revision, snapshot } => {
                if project_revision.is_some_and(|current| revision <= current) {
                    continue;
                }
                let mut restored = snapshot.project;
                if let Err(error) = snapshot.worker_state.restore(&mut restored) {
                    write_audio_response(
                        &mut stdout,
                        &AudioWorkerResponse::Error {
                            message: format!("restore audio worker project state: {error:#}"),
                        },
                    )?;
                    continue;
                }
                let active_composition = restored.active_composition;
                let project_name = restored.name.clone();
                let mut restored_timeline = TimelineState::default();
                restored_timeline.load_document(snapshot.timeline);
                restored_timeline.set_worker_transport(0.0, false);
                project = Some(restored);
                timeline = Some(restored_timeline);
                project_revision = Some(revision);
                engine.get_or_insert_with(AudioEngine::new);
                write_audio_response(
                    &mut stdout,
                    &AudioWorkerResponse::ProjectAccepted {
                        revision,
                        project_name,
                        active_composition,
                    },
                )?;
            }
            AudioWorkerRequest::Transport {
                revision,
                project_revision: required_project_revision,
                generation,
                playhead,
                playing,
                scrubbing,
                master_muted,
            } => {
                if revision <= transport_revision {
                    continue;
                }
                transport_revision = revision;
                if generation < transport_generation {
                    continue;
                }
                if generation > transport_generation {
                    transport_generation = generation;
                }
                if project_revision != Some(required_project_revision) {
                    continue;
                }
                let (Some(project), Some(timeline)) = (&project, &mut timeline) else {
                    continue;
                };
                let Some(engine) = &mut engine else {
                    engine = Some(AudioEngine::new());
                    continue;
                };
                timeline.set_worker_transport(playhead, playing && !scrubbing);
                engine.set_master_muted(master_muted);
                engine.sync(project, timeline, &plugins);
                let (output_ready, audio_required, audio_sources_ready) = engine.playback_ready();
                write_audio_response(
                    &mut stdout,
                    &AudioWorkerResponse::State {
                        revision,
                        project_revision: required_project_revision,
                        generation,
                        playhead,
                        clock_lead: engine.clock_lead(),
                        track_levels: engine.track_levels(),
                        master_levels: engine.master_levels(),
                        output_ready,
                        audio_required,
                        audio_sources_ready,
                        diagnostics: messages::drain_for_worker()
                            .into_iter()
                            .map(|(severity, source, message)| {
                                (
                                    severity == messages::MessageSeverity::Error,
                                    source,
                                    message,
                                )
                            })
                            .collect(),
                    },
                )?;
            }
            AudioWorkerRequest::Clear => {
                project = None;
                timeline = None;
                project_revision = None;
                if let Some(engine) = &mut engine {
                    engine.clear();
                }
            }
            AudioWorkerRequest::Shutdown => {
                write_audio_response(&mut stdout, &AudioWorkerResponse::Stopped)?;
                return Ok(());
            }
            AudioWorkerRequest::Hello { .. } => write_audio_response(
                &mut stdout,
                &AudioWorkerResponse::Error {
                    message: "handshake already completed".to_owned(),
                },
            )?,
            #[cfg(debug_assertions)]
            AudioWorkerRequest::Crash => std::process::abort(),
        }
    }
    Ok(())
}

fn write_audio_response(output: &mut impl Write, response: &AudioWorkerResponse) -> Result<()> {
    serde_json::to_writer(&mut *output, response)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

impl Drop for AudioPlayback {
    fn drop(&mut self) {
        if let Some(mut process) = self.process.take() {
            process.stop();
        }
    }
}

#[derive(Clone, Copy)]
struct AudioRouteLayer<'a> {
    clip: &'a Clip,
    track: &'a Track,
    pipeline: &'a PipelineInstance,

    time_offset: f32,
    time_rate: f32,
}

struct AudioRoute<'a> {
    id: u64,
    media: MediaId,
    path: &'a std::path::Path,
    output_track: u32,
    root_start: f32,
    root_end: f32,
    source_offset: f32,
    source_rate: f32,

    layers: Vec<AudioRouteLayer<'a>>,
}

impl AudioRoute<'_> {
    fn source_time(&self, root_time: f32) -> f32 {
        root_time.mul_add(self.source_rate, self.source_offset)
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_audio_routes<'a>(
    project: &'a Project,
    scope: u64,
    tracks: &'a [Track],
    clips: &'a [Clip],
    root_range: (f32, f32),
    time_offset: f32,
    time_rate: f32,
    output_track: Option<u32>,
    outer_layers: &[AudioRouteLayer<'a>],
    depth: usize,
    routes: &mut Vec<AudioRoute<'a>>,
) {
    if depth >= 16 || time_rate <= 0.0 || root_range.1 <= root_range.0 {
        return;
    }
    let any_solo = tracks.iter().any(|track| track.solo);
    for clip in clips {
        let Some(track) = tracks.iter().find(|track| track.id == clip.track) else {
            continue;
        };
        if track.kind != crate::timeline::TrackKind::Audio
            || track.muted
            || (any_solo && !track.solo)
        {
            continue;
        }

        let clip_root_start = (clip.start - time_offset) / time_rate;
        let clip_root_end = (clip.end() - time_offset) / time_rate;
        let overlap_start = root_range.0.max(clip_root_start);
        let overlap_end = root_range.1.min(clip_root_end);
        if overlap_end <= overlap_start {
            continue;
        }

        let pipeline = track
            .property_row(&clip.source, clip.source_instance)
            .map_or(&clip.pipeline, |row| &row.pipeline);
        let layer = AudioRouteLayer {
            clip,
            track,
            pipeline,
            time_offset,
            time_rate,
        };
        let output_track = output_track.unwrap_or(track.id);
        let speed = clip.speed.max(0.01);
        match &clip.source {
            VisualSource::Audio(media) => {
                let Some(asset) = project.media(*media) else {
                    continue;
                };
                if !matches!(asset.kind, MediaKind::Video | MediaKind::Audio) {
                    continue;
                }
                let mut layers = Vec::with_capacity(outer_layers.len().saturating_add(1));
                layers.push(layer);
                layers.extend(outer_layers.iter().rev().copied());
                let source_offset = (time_offset - clip.start).mul_add(speed, clip.source_offset);
                let source_rate = time_rate * speed;
                let source_duration = asset.duration.map(|duration| duration.max(1.0e-6) as f32);
                for (root_start, root_end, source_offset) in looped_source_segments(
                    (overlap_start, overlap_end),
                    source_offset,
                    source_rate,
                    source_duration,
                ) {
                    routes.push(AudioRoute {
                        id: scoped_audio_clip_id(scope, clip.id),
                        media: *media,
                        path: &asset.path,
                        output_track,
                        root_start,
                        root_end,
                        source_offset,
                        source_rate,
                        layers: layers.clone(),
                    });
                }
            }
            VisualSource::Composition(composition_id) => {
                let Some(composition) = project.composition(*composition_id) else {
                    continue;
                };
                let child_offset = (time_offset - clip.start).mul_add(speed, clip.source_offset);
                let child_rate = time_rate * speed;
                let child_duration = project.composition_duration(*composition_id);
                let mut next_outer = Vec::with_capacity(outer_layers.len().saturating_add(1));
                next_outer.extend_from_slice(outer_layers);
                next_outer.push(layer);
                for (root_start, root_end, child_offset) in looped_source_segments(
                    (overlap_start, overlap_end),
                    child_offset,
                    child_rate,
                    child_duration,
                ) {
                    collect_audio_routes(
                        project,
                        nested_audio_scope(scope, clip.id, composition.id),
                        &composition.timeline.tracks,
                        &composition.timeline.clips,
                        (root_start, root_end),
                        child_offset,
                        child_rate,
                        Some(output_track),
                        &next_outer,
                        depth.saturating_add(1),
                        routes,
                    );
                }
            }
            _ => {}
        }
    }
}

#[expect(
    clippy::while_float,
    reason = "the finite range loop explicitly advances to the next representable float or exits"
)]
fn looped_source_segments(
    root_range: (f32, f32),
    source_offset: f32,
    source_rate: f32,
    source_duration: Option<f32>,
) -> Vec<(f32, f32, f32)> {
    if !root_range.0.is_finite()
        || !root_range.1.is_finite()
        || !source_offset.is_finite()
        || !source_rate.is_finite()
        || source_rate <= 0.0
        || root_range.1 <= root_range.0
    {
        return Vec::new();
    }
    let Some(duration) =
        source_duration.filter(|duration| duration.is_finite() && *duration > 1.0e-6)
    else {
        return vec![(root_range.0, root_range.1, source_offset)];
    };

    let source_offset64 = f64::from(source_offset);
    let source_rate64 = f64::from(source_rate);
    let duration64 = f64::from(duration);
    let root_limit64 = f64::from(root_range.1);
    let mut segments = Vec::new();
    let mut root_start = root_range.0;

    while root_start < root_range.1 {
        let root_start64 = f64::from(root_start);
        let source_at_start = f64::mul_add(root_start64, source_rate64, source_offset64);
        let cycle = (source_at_start / duration64).floor();
        if !cycle.is_finite() {
            break;
        }

        let cycle_end_root = (f64::mul_add(cycle + 1.0, duration64, -source_offset64)
            / source_rate64)
            .min(root_limit64);
        let mut root_end = cycle_end_root as f32;
        if !root_end.is_finite() || root_end > root_range.1 {
            root_end = root_range.1;
        }
        if root_end <= root_start {
            root_end = next_f32_up(root_start).min(root_range.1);
        }
        if root_end <= root_start {
            break;
        }

        segments.push((
            root_start,
            root_end,
            f64::mul_add(cycle, -duration64, source_offset64) as f32,
        ));
        root_start = root_end;
    }
    segments
}

fn audio_route_settings(
    project: &Project,
    plugins: &PluginRegistry,
    route: &AudioRoute<'_>,
    root_time: f32,
) -> Vec<AudioEffectSettings> {
    let mut settings = Vec::new();
    for layer in &route.layers {
        let timeline_time = root_time.mul_add(layer.time_rate, layer.time_offset);
        let local = layer.clip.timeline_local_time(timeline_time);
        settings.extend(audio_pipeline_settings(
            project,
            plugins,
            layer.pipeline,
            f64::from(timeline_time),
            f64::from(local),
        ));
        if let Some(track_pipeline) = &layer.track.pipeline {
            settings.extend(audio_pipeline_settings(
                project,
                plugins,
                track_pipeline,
                f64::from(timeline_time),
                f64::from(timeline_time),
            ));
        }
    }
    settings
}

fn audio_route_mix(route: &AudioRoute<'_>, root_time: f32) -> (f32, f32) {
    let mut volume = 1.0;
    let mut pan = 0.0f32;
    for layer in &route.layers {
        let timeline_time = root_time.mul_add(layer.time_rate, layer.time_offset);
        let local = layer.clip.timeline_local_time(timeline_time);
        let [track_volume, track_pan] = track_mix_at(layer.track, timeline_time);
        volume *= clip_volume(
            local,
            layer.clip.duration,
            layer.clip.fade_in,
            layer.clip.fade_out,
        ) * layer.clip.volume.clamp(0.0, 1.0)
            * track_volume;
        pan = (pan + track_pan).clamp(-1.0, 1.0);
    }
    (volume, pan)
}

fn next_f32_up(value: f32) -> f32 {
    if value.is_nan() || value == f32::INFINITY {
        return value;
    }
    if value == 0.0 {
        return f32::from_bits(1);
    }
    let bits = value.to_bits();
    f32::from_bits(if value > 0.0 {
        bits.saturating_add(1)
    } else {
        bits.saturating_sub(1)
    })
}

fn collect_desired_voices(
    project: &Project,
    plugins: &PluginRegistry,
    scope: u64,
    timeline: &TimelineState,
    timeline_time: f32,
    desired: &mut HashMap<u64, DesiredVoice>,
) {
    let point_end = next_f32_up(timeline_time);
    let mut routes = Vec::new();
    collect_audio_routes(
        project,
        scope,
        timeline.tracks(),
        timeline.clips(),
        (timeline_time, point_end),
        0.0,
        1.0,
        None,
        &[],
        0,
        &mut routes,
    );
    for route in routes {
        let (volume, pan) = audio_route_mix(&route, timeline_time);
        desired.insert(
            route.id,
            DesiredVoice {
                media: route.media,
                path: route.path.to_path_buf(),
                track: route.output_track,
                mapping_offset: route.source_offset,
                source_time: route.source_time(timeline_time),
                volume,
                pan,
                speed: route.source_rate,
                effects: audio_route_settings(project, plugins, &route, timeline_time),
            },
        );
    }
}

fn scoped_audio_clip_id(scope: u64, clip: u32) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    scope.hash(&mut hasher);
    clip.hash(&mut hasher);
    hasher.finish()
}

fn nested_audio_scope(scope: u64, clip: u32, composition: CompositionId) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    scope.hash(&mut hasher);
    clip.hash(&mut hasher);
    composition.hash(&mut hasher);
    hasher.finish()
}

fn stereo_mix_gains(volume: f32, pan: f32) -> [f32; 2] {
    let pan = pan.clamp(-1.0, 1.0);
    [
        volume
            * if pan <= 0.0 {
                1.0
            } else {
                (pan * std::f32::consts::FRAC_PI_2).cos()
            },
        volume
            * if pan >= 0.0 {
                1.0
            } else {
                (-pan * std::f32::consts::FRAC_PI_2).cos()
            },
    ]
}

fn track_mix_at(track: &Track, time: f32) -> [f32; 2] {
    [
        track
            .volume
            .evaluate(f64::from(time))
            .and_then(GpuValue::f32)
            .unwrap_or(1.0)
            .clamp(0.0, 1.0),
        track
            .pan
            .evaluate(f64::from(time))
            .and_then(GpuValue::f32)
            .unwrap_or(0.0)
            .clamp(-1.0, 1.0),
    ]
}

fn open_voice(
    mixer: &Mixer,
    wasm: Option<&mut AudioWasmRuntime>,
    desired: &DesiredVoice,
    playing: bool,
    master_muted: bool,
) -> Result<AudioVoice> {
    let file = File::open(&desired.path)
        .with_context(|| format!("open audio source {}", desired.path.display()))?;
    let source = Decoder::try_from(file)
        .with_context(|| format!("decode audio source {}", desired.path.display()))?;
    let source = OffsetAudioSource::new(
        source,
        Duration::from_secs_f32(desired.source_time.max(0.0)),
    )
    .with_context(|| format!("seek audio source {}", desired.path.display()))?;
    let effect_settings = Arc::new(Mutex::new(Arc::<[AudioEffectSettings]>::from(
        desired.effects.clone(),
    )));
    let source = AudioFxSource::new(source, Arc::clone(&effect_settings), wasm);
    let control = Arc::new(TrackMixControl::new(
        if master_muted { 0.0 } else { desired.volume },
        desired.pan,
    ));
    let source = StereoTrackSource::new(source, Arc::clone(&control));
    let meter = Arc::new(AudioMeter::default());
    let source = MeteredSource::new(source, Arc::clone(&meter));
    let player = Player::connect_new(mixer);
    player.pause();
    player.append(source);
    player.set_speed(desired.speed.max(0.01));
    if playing {
        player.play();
    }
    Ok(AudioVoice {
        media: desired.media,
        path: desired.path.clone(),
        track: desired.track,
        effects: desired.effects.clone(),
        effect_settings,
        source_anchor: desired.source_time,
        mapping_offset: desired.mapping_offset,
        last_source_time: desired.source_time,
        speed: desired.speed.max(0.01),
        meter,
        control,
        player,
    })
}

fn clip_volume(local: f32, duration: f32, fade_in: f32, fade_out: f32) -> f32 {
    let mut volume = 1.0_f32;
    if fade_in > f32::EPSILON {
        volume = volume.min((local / fade_in).clamp(0.0, 1.0));
    }
    if fade_out > f32::EPSILON {
        volume = volume.min(((duration - local) / fade_out).clamp(0.0, 1.0));
    }
    volume
}

#[expect(
    clippy::cast_sign_loss,
    reason = "frame count is derived from a nonnegative duration and output offsets are clamped to zero before conversion"
)]
#[expect(
    clippy::while_float,
    reason = "finite source positions are reduced by one until below one for interpolation"
)]
pub fn render_audio_wav(
    project: &Project,
    plugins: &PluginRegistry,
    timeline: &TimelineDocument,
    start: f32,
    end: f32,
    sample_rate: u32,
    path: &std::path::Path,
) -> Result<()> {
    use std::io::Write;

    anyhow::ensure!(
        start.is_finite() && end.is_finite(),
        "audio render range must be finite"
    );
    let sample_rate = sample_rate.max(8_000);
    anyhow::ensure!(
        sample_rate <= u32::MAX / 8,
        "audio sample rate exceeds RIFF/WAV byte-rate limit"
    );
    let duration = (f64::from(end) - f64::from(start)).max(0.0);
    let frames = (duration * f64::from(sample_rate)).ceil();
    let max_frames = (u32::MAX.saturating_sub(36) as usize) / 8;
    anyhow::ensure!(
        frames <= max_frames as f64,
        "audio render exceeds RIFF/WAV 4 GiB limit"
    );
    let frames = frames as usize;
    let mut mix = vec![
        0.0f32;
        frames
            .checked_mul(2)
            .context("audio sample count overflow")?
    ];
    let mut routes = Vec::new();
    collect_audio_routes(
        project,
        project.active_composition,
        &timeline.tracks,
        &timeline.clips,
        (start, end),
        0.0,
        1.0,
        None,
        &[],
        0,
        &mut routes,
    );
    let mut wasm = AudioWasmRuntime::new()
        .inspect_err(|error| {
            messages::error(
                "Audio render",
                format!("WASM runtime unavailable: {error:#}"),
            );
        })
        .ok();

    for route in routes {
        let file = File::open(route.path)
            .with_context(|| format!("open audio source {}", route.path.display()))?;
        let mut decoder = Decoder::try_from(file)
            .with_context(|| format!("decode audio source {}", route.path.display()))?;
        let channels = decoder.channels().get() as usize;
        let source_sample_rate = decoder.sample_rate().get().max(1);
        let source_start = route.source_time(route.root_start);
        decoder
            .try_seek(Duration::from_secs_f32(source_start))
            .with_context(|| format!("seek audio source {}", route.path.display()))?;

        let mut read_frame = || -> Option<[f32; 2]> {
            let left = decoder.next()?;
            let right = if channels <= 1 { left } else { decoder.next()? };
            for _ in 2..channels {
                let _ = decoder.next();
            }
            Some([left, right])
        };
        let Some(mut a) = read_frame() else {
            continue;
        };
        let mut b = read_frame().unwrap_or([0.0; 2]);
        let step = source_sample_rate as f32 / sample_rate as f32 * route.source_rate.max(0.01);
        anyhow::ensure!(step.is_finite(), "audio source rate is not finite");
        let mut source_pos = 0.0f32;
        let out_first = (f64::from(route.root_start - start) * f64::from(sample_rate))
            .round()
            .max(0.0) as usize;
        let out_last = (f64::from(route.root_end - start) * f64::from(sample_rate))
            .round()
            .max(0.0) as usize;
        let mut settings = audio_route_settings(project, plugins, &route, route.root_start);
        let mut chain = AudioEffectChain::new(wasm.as_mut(), &settings, 2, sample_rate);
        let mut output_frame = out_first;
        let output_end = out_last.min(frames);
        let mut block = Vec::with_capacity(AUDIO_BLOCK_FRAMES * 2);
        let mut gains = Vec::with_capacity(AUDIO_BLOCK_FRAMES);

        while output_frame < output_end {
            let root_time = start + output_frame as f32 / sample_rate as f32;
            let next = audio_route_settings(project, plugins, &route, root_time);
            let same_chain = settings.len() == next.len()
                && settings
                    .iter()
                    .zip(&next)
                    .all(|(current, next)| current.same_processor(next));
            if !same_chain {
                chain = AudioEffectChain::new(wasm.as_mut(), &next, 2, sample_rate);
            }
            settings = next;

            block.clear();
            gains.clear();
            let block_frames = AUDIO_BLOCK_FRAMES.min(output_end.saturating_sub(output_frame));
            for offset in 0..block_frames {
                while source_pos >= 1.0 {
                    a = b;
                    b = read_frame().unwrap_or([0.0; 2]);
                    source_pos -= 1.0;
                }
                block.extend_from_slice(&[
                    f32::mul_add(b[0] - a[0], source_pos, a[0]),
                    f32::mul_add(b[1] - a[1], source_pos, a[1]),
                ]);
                source_pos += step;
                let frame_time =
                    start + output_frame.saturating_add(offset) as f32 / sample_rate as f32;
                let (volume, pan) = audio_route_mix(&route, frame_time);
                gains.push(stereo_mix_gains(volume, pan));
            }
            chain.process(&mut block, &settings);
            for (offset, (stereo, gain)) in block.as_chunks::<2>().0.iter().zip(&gains).enumerate()
            {
                let target = output_frame
                    .checked_add(offset)
                    .and_then(|frame| frame.checked_mul(2))
                    .context("audio mix offset overflow")?;
                let samples = mix
                    .get_mut(target..target.checked_add(2).context("audio mix offset overflow")?)
                    .context("audio mix frame out of bounds")?;
                let [left, right] = samples else {
                    anyhow::bail!("invalid stereo mix frame")
                };
                *left = f32::mul_add(stereo[0], gain[0], *left);
                *right = f32::mul_add(stereo[1], gain[1], *right);
            }
            output_frame = output_frame
                .checked_add(block_frames)
                .context("audio frame count overflow")?;
        }
    }

    let data_len = mix
        .len()
        .checked_mul(std::mem::size_of::<f32>())
        .context("audio render size overflow")?;
    let data_len = u32::try_from(data_len).context("audio render exceeds RIFF/WAV 4 GiB limit")?;
    let riff_len = 36u32
        .checked_add(data_len)
        .context("audio render exceeds RIFF/WAV 4 GiB limit")?;
    let mut file = File::create(path).with_context(|| format!("create {}", path.display()))?;
    file.write_all(b"RIFF")?;
    file.write_all(&riff_len.to_le_bytes())?;
    file.write_all(b"WAVEfmt ")?;
    file.write_all(&16u32.to_le_bytes())?;
    file.write_all(&3u16.to_le_bytes())?;
    file.write_all(&2u16.to_le_bytes())?;
    file.write_all(&sample_rate.to_le_bytes())?;
    file.write_all(&(sample_rate.saturating_mul(8)).to_le_bytes())?;
    file.write_all(&8u16.to_le_bytes())?;
    file.write_all(&32u16.to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&data_len.to_le_bytes())?;
    file.write_all(bytemuck::cast_slice(&mix))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{StereoTrackSource, TrackMixControl, looped_source_segments, next_f32_up};
    use rodio::buffer::SamplesBuffer;
    use std::sync::Arc;

    #[test]
    fn startup_readiness_rejects_an_ack_from_an_older_transport_generation() {
        assert!(!super::audio_startup_ready(
            Some((8, 20)),
            (8, 21),
            false,
            true,
            true,
        ));
    }

    #[test]
    fn active_audio_waits_for_output_and_source_consumption() {
        assert!(!super::audio_startup_ready(
            Some((8, 21)),
            (8, 21),
            true,
            false,
            false,
        ));
        assert!(!super::audio_startup_ready(
            Some((8, 21)),
            (8, 21),
            true,
            true,
            false,
        ));
    }

    #[test]
    fn silent_active_audio_is_ready_after_its_source_is_consumed() {
        assert!(super::audio_startup_ready(
            Some((8, 21)),
            (8, 21),
            true,
            true,
            true,
        ));
    }

    #[test]
    fn scenes_without_active_audio_do_not_wait_for_an_output_device() {
        assert!(super::audio_startup_ready(
            Some((8, 21)),
            (8, 21),
            false,
            false,
            false,
        ));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn audio_worker_child_entry() {
        if std::env::var_os("KAMA_AUDIO_WORKER_TEST_CHILD").is_some() {
            super::run_audio_worker().expect("audio worker should complete its protocol");
        }
    }

    #[test]
    fn diagnostic_lines_are_bounded_without_truncating_short_lines() {
        let mut short = &b"ok\n"[..];
        assert_eq!(
            super::read_bounded_line(&mut short, 8).unwrap().as_deref(),
            Some("ok")
        );
        let mut long = &b"0123456789\n"[..];
        assert_eq!(
            super::read_bounded_line(&mut long, 4).unwrap().as_deref(),
            Some("0123 [truncated]")
        );
    }

    #[test]
    fn audio_project_wire_restores_the_resolved_media_path() {
        use super::{AudioProjectSnapshot, AudioWorkerRequest};
        use crate::{
            project::{MediaAsset, MediaKind, Project, WorkerProjectState},
            timeline::TimelineState,
        };
        use std::path::PathBuf;

        let mut project = Project::new();
        project.media.push(MediaAsset {
            id: 19,
            name: "narration.wav".into(),
            path: PathBuf::from("/resolved/current/narration.wav"),
            absolute_path: PathBuf::from("/stale/saved/narration.wav"),
            relative_path: PathBuf::from("audio/narration.wav"),
            kind: MediaKind::Audio,
            duration: Some(1.0),
            frame_rate: None,
            video_width: None,
            video_height: None,
            has_audio: true,
            tracks: Vec::new(),
            waveform: None,
            legacy_model: None,
        });
        let active_composition = project.active_composition;
        let snapshot = AudioProjectSnapshot {
            worker_state: WorkerProjectState::capture(&project),
            project,
            timeline: TimelineState::default().document(),
        };
        let request = AudioWorkerRequest::Project {
            revision: 6,
            snapshot: Box::new(snapshot),
        };

        let encoded = serde_json::to_vec(&request).unwrap();
        let AudioWorkerRequest::Project { snapshot, .. } =
            serde_json::from_slice(&encoded).unwrap()
        else {
            panic!("audio project request expected");
        };
        let AudioProjectSnapshot {
            mut project,
            worker_state,
            ..
        } = *snapshot;
        worker_state.restore(&mut project).unwrap();
        assert_eq!(
            project.media[0].path,
            PathBuf::from("/resolved/current/narration.wav")
        );
        assert_eq!(project.active_composition, active_composition);
    }

    #[cfg(debug_assertions)]
    #[test]
    fn audio_worker_accepts_a_project_with_a_runtime_media_path() {
        use super::{AudioPlayback, AudioWorkerStatus};
        use crate::project::{MediaAsset, MediaKind, Project};
        use std::{
            path::PathBuf,
            time::{Duration, Instant, SystemTime, UNIX_EPOCH},
        };

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "kama-audio-worker-source-{}-{unique}.wav",
            std::process::id()
        ));
        std::fs::write(&path, b"runtime-resolved audio fixture").unwrap();
        let mut project = Project::new();
        project.media.push(MediaAsset {
            id: 29,
            name: "relinked.wav".into(),
            path: path.clone(),
            absolute_path: PathBuf::from("/stale/saved/relinked.wav"),
            relative_path: PathBuf::from("audio/relinked.wav"),
            kind: MediaKind::Audio,
            duration: Some(1.0),
            frame_rate: None,
            video_width: None,
            video_height: None,
            has_audio: true,
            tracks: Vec::new(),
            waveform: None,
            legacy_model: None,
        });
        let timeline = crate::timeline::TimelineState::default();
        let mut worker = AudioPlayback::new("");
        let deadline = Instant::now() + Duration::from_secs(8);
        while worker.accepted_project_revision != Some(1) {
            assert!(
                Instant::now() < deadline,
                "audio child did not accept runtime media path; status={:?}",
                worker.status()
            );
            assert!(!matches!(
                worker.status(),
                AudioWorkerStatus::Crashed | AudioWorkerStatus::StartupFailed
            ));
            worker.sync(&project, &timeline, 51, false);
            std::thread::yield_now();
        }
        assert_eq!(
            worker.accepted_project_name.as_deref(),
            Some(project.name.as_str())
        );
        assert_eq!(
            worker.latest_snapshot.as_ref().unwrap().project.media[0].path,
            path
        );
        let _ = std::fs::remove_file(path);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn vaxry_audio_worker_streams_nonzero_track_levels_for_over_three_seconds() {
        use super::{AudioPlayback, AudioWorkerStatus};
        use crate::{project::Project, timeline::TimelineState};
        use std::{
            path::Path,
            thread,
            time::{Duration, Instant},
        };

        let Ok(project_path) = std::env::var("KAMA_AUDIO_REPRO_PROJECT") else {
            return;
        };
        let project =
            Project::load(Path::new(&project_path)).expect("load audio reproduction project");
        for asset in &project.media {
            assert!(
                !asset.path.as_os_str().is_empty() && asset.path.is_file(),
                "audio worker source '{}' is not resolved: {}",
                asset.name,
                asset.path.display()
            );
        }
        let mut timeline = TimelineState::default();
        timeline.load_document(project.active_composition().timeline.clone());
        timeline.seek_relative(1.25);
        timeline.toggle_playback();
        let mut worker = AudioPlayback::new(&crate::preferences::load_plugin_paths());
        worker.set_video_ready(true);
        let pid = worker.pid().expect("audio worker process started");
        let startup_deadline = Instant::now() + Duration::from_secs(12);
        let mut active_since: Option<Instant> = None;
        while active_since.is_none_or(|started| started.elapsed() < Duration::from_millis(3_200)) {
            assert!(
                Instant::now() < startup_deadline,
                "audio worker did not stream active levels; status={:?}, playing={}, levels={:?}, diagnostics={:?}",
                worker.status(),
                worker.playing,
                worker.track_levels,
                crate::messages::drain_for_worker()
            );
            assert!(!worker.sync(&project, &timeline, 0, false));
            assert!(!matches!(
                worker.status(),
                AudioWorkerStatus::Crashed
                    | AudioWorkerStatus::Unresponsive
                    | AudioWorkerStatus::StartupFailed
            ));
            assert_eq!(worker.pid(), Some(pid));
            if worker
                .track_levels
                .values()
                .any(|levels| levels[0].max(levels[1]) > 0.02)
            {
                active_since.get_or_insert_with(Instant::now);
            }
            thread::sleep(Duration::from_millis(16));
        }
        assert!(worker.accepted_project_revision.is_some());
        assert!(worker.playing);
    }

    #[test]
    fn response_backpressure_preserves_lifecycle_messages() {
        let input = b"{\"type\":\"ready\",\"protocol\":1,\"pid\":4210}\n{\"type\":\"ready\",\"protocol\":1,\"pid\":4211}\n{\"type\":\"stopped\"}\n";
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let reader = std::thread::spawn(move || super::audio_worker_reader(&input[..], &tx, false));
        let timeout = std::time::Duration::from_secs(2);
        assert!(matches!(
            rx.recv_timeout(timeout).unwrap(),
            super::AudioWorkerResponse::Ready { pid: 4210, .. }
        ));
        assert!(matches!(
            rx.recv_timeout(timeout).unwrap(),
            super::AudioWorkerResponse::Ready { pid: 4211, .. }
        ));
        assert!(matches!(
            rx.recv_timeout(timeout).unwrap(),
            super::AudioWorkerResponse::Stopped
        ));
        reader.join().unwrap();
    }

    #[test]
    fn state_response_with_numeric_track_levels_survives_worker_reader() {
        let response = super::AudioWorkerResponse::State {
            revision: 11,
            project_revision: 4,
            generation: 9,
            playhead: 7.25,
            clock_lead: 0.125,
            track_levels: [(37, [0.625, 0.375])].into(),
            master_levels: [0.625, 0.375],
            output_ready: true,
            audio_required: false,
            audio_sources_ready: false,
            diagnostics: Vec::new(),
        };
        let mut input = b"{malformed protocol line}\n".to_vec();
        serde_json::to_writer(&mut input, &response).unwrap();
        input.push(b'\n');
        let (tx, rx) = std::sync::mpsc::sync_channel(2);
        let reader = std::thread::spawn(move || {
            super::audio_worker_reader(&input[..], &tx, false);
        });

        assert!(matches!(
            rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap(),
            super::AudioWorkerResponse::Error { message }
                if message.contains("decode audio worker response")
        ));
        let parsed = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        let super::AudioWorkerResponse::State { track_levels, .. } = parsed else {
            panic!("state response with track levels expected");
        };
        assert_eq!(track_levels.get(&37), Some(&[0.625, 0.375]));
        reader.join().unwrap();
    }

    #[cfg(debug_assertions)]
    #[test]
    fn clear_retries_when_the_command_queue_is_full() {
        use super::{AudioPlayback, AudioWorkerRequest};
        use crate::{project::Project, timeline::TimelineState};

        let mut worker = AudioPlayback::new("");
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        tx.send(AudioWorkerRequest::Shutdown).unwrap();
        let original_requests = worker.process.as_mut().unwrap().requests.replace(tx);
        worker.clear();
        worker.sync(&Project::new(), &TimelineState::default(), 37, false);
        assert!(worker.clear_pending, "clear must survive a full queue");
        assert!(matches!(
            rx.try_recv().unwrap(),
            AudioWorkerRequest::Shutdown
        ));
        worker.sync(&Project::new(), &TimelineState::default(), 37, false);
        assert!(matches!(rx.try_recv().unwrap(), AudioWorkerRequest::Clear));
        assert!(!worker.clear_pending);
        worker.process.as_mut().unwrap().requests = original_requests;
    }

    #[cfg(debug_assertions)]
    #[test]
    fn delayed_audio_clock_is_an_absolute_one_shot_target() {
        use super::{AudioPlayback, AudioWorkerResponse};
        use crate::{project::Project, timeline::TimelineState};
        use std::collections::HashMap;
        use std::time::{Duration, Instant};

        let mut worker = AudioPlayback::new("");
        worker.set_video_ready(true);
        let project = Project::new();
        let mut timeline = TimelineState::default();
        timeline.set_worker_transport(7.5, true);
        let deadline = Instant::now() + Duration::from_secs(8);
        while worker.accepted_project_revision != Some(1) {
            assert!(Instant::now() < deadline, "initialize worker project");
            worker.sync(&project, &timeline, 37, false);
            std::thread::yield_now();
        }
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let process = worker.process.as_mut().unwrap();
        let real_responses = std::mem::replace(&mut process.responses, rx);
        tx.send(AudioWorkerResponse::State {
            revision: worker.transport_revision,
            project_revision: 1,
            generation: worker.generation,
            playhead: 7.5,
            clock_lead: 0.125,
            track_levels: HashMap::new(),
            master_levels: [0.0; 2],
            output_ready: true,
            audio_required: false,
            audio_sources_ready: false,
            diagnostics: Vec::new(),
        })
        .unwrap();
        timeline.set_worker_transport(10.0, true);
        worker.sync(&project, &timeline, 37, false);
        let target = worker.take_clock_target().expect("fresh audio observation");
        assert_eq!(target, 7.625);
        timeline.sync_forward_playhead(target, &kama_ui::dock::LayoutSnapshot::default());
        assert_eq!(
            timeline.playhead(),
            10.0,
            "a delayed observation cannot add drift to the current clock"
        );
        assert_eq!(
            worker.take_clock_target(),
            None,
            "a redraw cannot reuse the same correction"
        );
        worker.process.as_mut().unwrap().responses = real_responses;
    }

    #[cfg(debug_assertions)]
    #[test]
    fn seeking_rejects_delayed_clock_and_meter_updates() {
        use super::{AudioPlayback, AudioWorkerResponse};
        use crate::{project::Project, timeline::TimelineState};
        use std::time::{Duration, Instant};

        let mut worker = AudioPlayback::new("");
        let project = Project::new();
        let mut timeline = TimelineState::default();
        let deadline = Instant::now() + Duration::from_secs(8);
        while worker.accepted_project_revision != Some(1) {
            assert!(
                Instant::now() < deadline,
                "worker must initialize the project"
            );
            worker.sync(&project, &timeline, 37, false);
            std::thread::yield_now();
        }
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let process = worker.process.as_mut().unwrap();
        let real_responses = std::mem::replace(&mut process.responses, rx);
        tx.send(AudioWorkerResponse::State {
            revision: worker.transport_revision,
            project_revision: 1,
            generation: worker.generation,
            playhead: timeline.playhead(),
            clock_lead: 0.18,
            track_levels: [(2, [0.75, 0.5])].into(),
            master_levels: [0.75, 0.5],
            output_ready: true,
            audio_required: true,
            audio_sources_ready: true,
            diagnostics: Vec::new(),
        })
        .unwrap();
        worker.transport_waiting_since =
            Some(Instant::now().checked_sub(Duration::from_secs(4)).unwrap());
        timeline.seek_relative(11.375);
        assert!(!worker.sync(&project, &timeline, 37, false));
        assert!(matches!(
            worker.status(),
            super::AudioWorkerStatus::Ready { .. }
        ));
        assert_eq!(worker.take_clock_target(), None);
        assert!(worker.track_levels.is_empty());
        assert_eq!(worker.master_levels, [0.0, 0.0]);
        worker.process.as_mut().unwrap().responses = real_responses;
    }

    #[cfg(all(debug_assertions, unix))]
    #[test]
    fn unresponsive_worker_stops_and_requires_manual_restart() {
        use super::{AudioPlayback, AudioWorkerStatus};
        use crate::{project::Project, timeline::TimelineState};
        use std::time::{Duration, Instant};

        let mut worker = AudioPlayback::new("");
        let project = Project::new();
        let timeline = TimelineState::default();
        let deadline = Instant::now() + Duration::from_secs(8);
        while worker.accepted_project_revision != Some(1) {
            assert!(
                Instant::now() < deadline,
                "worker must initialize the project"
            );
            worker.sync(&project, &timeline, 37, false);
            std::thread::yield_now();
        }
        let pid = worker.pid().expect("audio worker pid is retained");
        assert_eq!(worker.process.as_ref().unwrap().pid, pid);
        assert!(
            std::process::Command::new("kill")
                .args(["-STOP", &pid.to_string()])
                .status()
                .unwrap()
                .success()
        );
        worker.last_response_at = Instant::now().checked_sub(Duration::from_secs(4)).unwrap();
        assert!(
            !worker.check_health(),
            "time spent idle without an outstanding request is not a stall"
        );
        assert!(
            !worker.sync(&project, &timeline, 37, false),
            "a request after an idle gap gets a fresh response deadline"
        );
        worker.transport_waiting_since =
            Some(Instant::now().checked_sub(Duration::from_secs(4)).unwrap());
        assert!(worker.sync(&project, &timeline, 37, false));
        assert_eq!(worker.status(), AudioWorkerStatus::Unresponsive);
        assert!(worker.process.is_none());
        assert_eq!(worker.pid(), Some(pid));
        assert_eq!(worker.take_clock_target(), None);
        assert!(!worker.sync(&project, &timeline, 37, false));
        assert!(worker.process.is_none());
        worker.restart();
        assert!(matches!(worker.status(), AudioWorkerStatus::Restarting));
        assert_ne!(worker.process.as_ref().unwrap().pid, pid);
        assert_eq!(
            worker.pid(),
            worker.process.as_ref().map(|process| process.pid)
        );
    }

    #[cfg(all(debug_assertions, unix))]
    #[test]
    fn worker_crash_restarts_with_latest_snapshot_and_drop_reaps_child() {
        use super::AudioPlayback;
        use crate::{project::Project, timeline::TimelineState};
        use std::{
            process::Command,
            thread,
            time::{Duration, Instant},
        };

        fn await_revision(
            worker: &mut AudioPlayback,
            project: &Project,
            timeline: &TimelineState,
            history_revision: u64,
            worker_revision: u64,
        ) {
            let deadline = Instant::now() + Duration::from_secs(8);
            while Instant::now() < deadline {
                let _ = worker.sync(project, timeline, history_revision, false);
                if worker.accepted_project_revision == Some(worker_revision) {
                    return;
                }
                thread::yield_now();
            }
            panic!("audio worker did not acknowledge snapshot {worker_revision}");
        }

        fn process_exists(pid: u32) -> bool {
            Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        }

        let mut worker = AudioPlayback::new("");
        let first_pid = worker.process.as_ref().expect("worker starts").pid;
        let mut project = Project::new();
        project.name = "Before unsaved edit".to_owned();
        let mut timeline = TimelineState::default();
        await_revision(&mut worker, &project, &timeline, 37, 1);
        project.name = "Latest unsaved edit".to_owned();
        let _ = worker.sync(&project, &timeline, 38, false);
        await_revision(&mut worker, &project, &timeline, 38, 2);
        worker.crash_for_test();

        let deadline = Instant::now() + Duration::from_secs(8);
        let mut detected = 0;
        while Instant::now() < deadline && detected == 0 {
            detected += usize::from(worker.sync(&project, &timeline, 38, false));
            thread::yield_now();
        }
        assert_eq!(detected, 1, "abnormal worker exit is reported once");

        assert!(
            worker.process.is_none(),
            "crash must not restart implicitly"
        );
        assert_eq!(worker.status(), super::AudioWorkerStatus::Crashed);
        assert_eq!(worker.pid(), Some(first_pid));
        for _ in 0..3 {
            assert!(!worker.sync(&project, &timeline, 38, false));
            assert!(
                worker.process.is_none(),
                "updates must not restart a failed worker"
            );
        }
        timeline.toggle_playback();
        worker.restart();
        let restarted_pid = worker.process.as_ref().expect("manual restart starts").pid;
        assert_ne!(restarted_pid, first_pid);
        assert_eq!(worker.pid(), Some(restarted_pid));
        await_revision(&mut worker, &project, &timeline, 38, 2);
        assert_eq!(
            worker.accepted_project_name.as_deref(),
            Some("Latest unsaved edit")
        );
        assert!(!worker.playing, "restarted worker transport remains paused");
        timeline.toggle_playback();
        worker.sync(&project, &timeline, 38, false);
        timeline.toggle_playback();
        worker.set_video_ready(true);
        worker.sync(&project, &timeline, 38, false);
        assert!(
            worker.playing,
            "playback can resume after an explicit pause and play"
        );
        drop(worker);

        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && process_exists(restarted_pid) {
            thread::yield_now();
        }
        assert!(
            !process_exists(restarted_pid),
            "drop must reap the child process"
        );
    }

    #[test]
    fn live_audio_point_range_remains_nonempty_at_late_playheads() {
        for time in [0.0_f32, 27.666_666, 32.408_005, 59.0, 3_600.0] {
            let end = next_f32_up(time);
            assert!(end > time, "next point after {time} must advance");
        }
        assert_eq!(32.408_005_f32 + 0.000_001_f32, 32.408_005_f32);
    }

    #[test]
    fn looped_source_segments_handles_one_ulp_scrub_range_before_four_seconds() {
        let time = f32::from_bits(4.0_f32.to_bits() - 1);
        let end = next_f32_up(time);
        assert_eq!(end, 4.0);

        let segments = looped_source_segments((time, end), 0.0, 1.0, Some(10.0));
        assert_eq!(segments, vec![(time, end, 0.0)]);
    }

    #[test]
    fn looped_source_segments_handles_large_cycle_without_stalling() {
        let time = 4.0_f32;
        let end = next_f32_up(time);
        let segments = looped_source_segments((time, end), 1_000.0, 0.01, Some(0.000_01));

        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].0, time);
        assert_eq!(segments[0].1, end);
    }

    #[test]
    fn looped_source_segments_wrap_at_media_end() {
        let segments = looped_source_segments((0.0, 5.0), 8.0, 1.0, Some(10.0));
        assert_eq!(segments, vec![(0.0, 2.0, 8.0), (2.0, 5.0, -2.0)]);
        assert_eq!(segments[1].2 + segments[1].0, 0.0);
    }

    #[test]
    fn stereo_track_source_applies_live_volume_and_pan() {
        let input = SamplesBuffer::new(
            rodio::ChannelCount::new(2).unwrap(),
            rodio::SampleRate::new(48_000).unwrap(),
            vec![1.0, 1.0, 1.0, 1.0],
        );
        let control = Arc::new(TrackMixControl::new(0.5, 1.0));
        let mut source = StereoTrackSource::new(input, Arc::clone(&control));
        assert!(source.next().unwrap().abs() < 0.000_01);
        assert_eq!(source.next(), Some(0.5));

        control.publish(1.0, -1.0);
        assert_eq!(source.next(), Some(1.0));
        assert!(source.next().unwrap().abs() < 0.000_01);
    }
}
