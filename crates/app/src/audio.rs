use std::{
    collections::{HashMap, HashSet},
    fs::File,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
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

const AUDIO_DRIFT_TOLERANCE: f32 = 0.090;
const AUDIO_RETRY_DELAY: Duration = Duration::from_secs(2);

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
    fn new(inner: S, meter: Arc<AudioMeter>) -> Self {
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
        self.channel = (self.channel + 1) % channels.max(1);
        self.samples_since_publish += 1;
        if self.samples_since_publish >= channels.max(1) * 256 {
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
            .unwrap_or_else(|poisoned| poisoned.into_inner())
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
        let complete = self.output.len() / channels.max(1) * channels.max(1);
        self.output.truncate(complete);
        if self.output.is_empty() {
            return false;
        }
        let settings = self
            .settings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
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
        let sample = self.output[self.cursor];
        self.cursor += 1;
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
            .unwrap_or_else(|poisoned| poisoned.into_inner())
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
        let sample = self.frame[self.output_channel];
        self.output_channel ^= 1;
        Some(sample)
    }
}

impl<S: Source> Source for StereoTrackSource<S> {
    fn current_span_len(&self) -> Option<usize> {
        self.inner
            .current_span_len()
            .map(|samples| samples / self.source_channels.max(1) * 2)
    }
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

pub struct AudioPlayback {
    _sink: Option<MixerDeviceSink>,
    mixer: Option<Mixer>,
    master_meter: Arc<AudioMeter>,
    voices: HashMap<u64, AudioVoice>,
    failed_media: HashMap<MediaId, (PathBuf, Instant)>,
    wasm: Option<AudioWasmRuntime>,
    playing: bool,
    master_muted: bool,
    clock_lead: f32,
    last_playhead: Option<f32>,
}

impl AudioPlayback {
    pub fn new() -> Self {
        let master_meter = Arc::new(AudioMeter::default());
        let wasm = AudioWasmRuntime::new()
            .inspect_err(|error| {
                messages::error("Audio", format!("WASM runtime unavailable: {error:#}"))
            })
            .ok();
        match rodio::DeviceSinkBuilder::open_default_sink() {
            Ok(sink) => {
                let channels = sink.config().channel_count();
                let sample_rate = sink.config().sample_rate();
                let (mixer, source) = mixer::mixer(channels, sample_rate);
                mixer.add(Zero::new(channels, sample_rate));
                sink.mixer()
                    .add(MeteredSource::new(source, Arc::clone(&master_meter)));
                Self {
                    _sink: Some(sink),
                    mixer: Some(mixer),
                    master_meter,
                    voices: HashMap::new(),
                    failed_media: HashMap::new(),
                    wasm,
                    playing: false,
                    master_muted: false,
                    clock_lead: 0.0,
                    last_playhead: None,
                }
            }
            Err(error) => {
                messages::warning("Audio", format!("output unavailable: {error}"));
                Self {
                    _sink: None,
                    mixer: None,
                    master_meter,
                    voices: HashMap::new(),
                    failed_media: HashMap::new(),
                    wasm,
                    playing: false,
                    master_muted: false,
                    clock_lead: 0.0,
                    last_playhead: None,
                }
            }
        }
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
    }

    pub fn set_master_muted(&mut self, muted: bool) {
        self.master_muted = muted;
    }

    pub fn sync(&mut self, project: &Project, timeline: &TimelineState, plugins: &PluginRegistry) {
        let Some(mixer) = self.mixer.as_ref() else {
            self.playing = false;
            self.clock_lead = 0.0;
            return;
        };

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
                    || desired.source_time + AUDIO_DRIFT_TOLERANCE * desired.speed.max(0.01)
                        < voice.source_anchor
                    || voice.player.empty()
            });
            if replace {
                if let Some(voice) = self.voices.remove(&clip) {
                    voice.player.stop();
                }
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
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) =
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
            let source_rewound = desired.source_time
                + AUDIO_DRIFT_TOLERANCE * desired.speed.max(0.01)
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
            self.clock_lead = positive_leads[positive_leads.len() / 2].clamp(0.0, 0.250);
        }
    }

    pub fn clock_lead(&self) -> f32 {
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
        self.source_offset + root_time * self.source_rate
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
            .map(|row| &row.pipeline)
            .unwrap_or(&clip.pipeline);
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
                let mut layers = Vec::with_capacity(outer_layers.len() + 1);
                layers.push(layer);
                layers.extend(outer_layers.iter().rev().copied());
                let source_offset = clip.source_offset + (time_offset - clip.start) * speed;
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
                let child_offset = clip.source_offset + (time_offset - clip.start) * speed;
                let child_rate = time_rate * speed;
                let child_duration = project.composition_duration(*composition_id);
                let mut next_outer = Vec::with_capacity(outer_layers.len() + 1);
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
                        depth + 1,
                        routes,
                    );
                }
            }
            _ => {}
        }
    }
}

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
        let source_at_start = source_offset64 + root_start64 * source_rate64;
        let cycle = (source_at_start / duration64).floor();
        if !cycle.is_finite() {
            break;
        }

        let cycle_end_root =
            (((cycle + 1.0) * duration64 - source_offset64) / source_rate64).min(root_limit64);
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
            (source_offset64 - cycle * duration64) as f32,
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
        let timeline_time = layer.time_offset + root_time * layer.time_rate;
        let local = layer.clip.timeline_local_time(timeline_time);
        settings.extend(audio_pipeline_settings(
            project,
            plugins,
            layer.pipeline,
            timeline_time as f64,
            local as f64,
        ));
        if let Some(track_pipeline) = &layer.track.pipeline {
            settings.extend(audio_pipeline_settings(
                project,
                plugins,
                track_pipeline,
                timeline_time as f64,
                timeline_time as f64,
            ));
        }
    }
    settings
}

fn audio_route_mix(route: &AudioRoute<'_>, root_time: f32) -> (f32, f32) {
    let mut volume = 1.0;
    let mut pan = 0.0f32;
    for layer in &route.layers {
        let timeline_time = layer.time_offset + root_time * layer.time_rate;
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
    f32::from_bits(if value > 0.0 { bits + 1 } else { bits - 1 })
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
            .evaluate(time as f64)
            .and_then(GpuValue::f32)
            .unwrap_or(1.0)
            .clamp(0.0, 1.0),
        track
            .pan
            .evaluate(time as f64)
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

pub(crate) fn render_audio_wav(
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
    let max_frames = ((u32::MAX - 36) as usize) / (2 * std::mem::size_of::<f32>());
    anyhow::ensure!(
        frames <= max_frames as f64,
        "audio render exceeds RIFF/WAV 4 GiB limit"
    );
    let frames = frames as usize;
    let mut mix = vec![0.0f32; frames * 2];
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
            )
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
        let mut source_pos = 0.0f32;
        let out_first = ((route.root_start - start) as f64 * sample_rate as f64)
            .round()
            .max(0.0) as usize;
        let out_last = ((route.root_end - start) as f64 * sample_rate as f64)
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
            let block_frames = AUDIO_BLOCK_FRAMES.min(output_end - output_frame);
            for offset in 0..block_frames {
                while source_pos >= 1.0 {
                    a = b;
                    b = read_frame().unwrap_or([0.0; 2]);
                    source_pos -= 1.0;
                }
                block.extend_from_slice(&[
                    a[0] + (b[0] - a[0]) * source_pos,
                    a[1] + (b[1] - a[1]) * source_pos,
                ]);
                source_pos += step;
                let frame_time = start + (output_frame + offset) as f32 / sample_rate as f32;
                let (volume, pan) = audio_route_mix(&route, frame_time);
                gains.push(stereo_mix_gains(volume, pan));
            }
            chain.process(&mut block, &settings);
            for (offset, (stereo, gain)) in block.chunks_exact(2).zip(&gains).enumerate() {
                let target = (output_frame + offset) * 2;
                mix[target] += stereo[0] * gain[0];
                mix[target + 1] += stereo[1] * gain[1];
            }
            output_frame += block_frames;
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
