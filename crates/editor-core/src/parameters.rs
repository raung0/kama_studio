use serde::{Deserialize, Serialize};

use crate::effects::{
    Binding, EasingHandle, GpuValue, Interpolation, KeyTrack, ScalarKeyframe, ScalarKeyframeTrack,
    TimedKey,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum HostValue {
    Gpu(GpuValue),
    Vec2Array(Vec<[f32; 2]>),
    F32List(Vec<f32>),
    String(String),
    Bytes(Vec<u8>),
}

impl HostValue {
    #[must_use]
    pub fn compatible(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Gpu(a), Self::Gpu(b)) => a.compatible(*b),
            _ => std::mem::discriminant(self) == std::mem::discriminant(other),
        }
    }

    #[must_use]
    pub const fn scalar_count(&self) -> usize {
        match self {
            Self::Vec2Array(values) => values.len().saturating_mul(2),
            Self::F32List(values) => values.len(),
            Self::Gpu(value) => value.component_count(),
            _ => 0,
        }
    }

    #[must_use]
    pub fn scalar(&self, channel: usize) -> Option<f32> {
        match self {
            Self::Vec2Array(values) => values
                .get(channel / 2)
                .and_then(|pair| pair.get(channel % 2))
                .copied(),
            Self::F32List(values) => values.get(channel).copied(),
            Self::Gpu(value) => value
                .numeric((value.component_count() > 1).then_some(channel))
                .map(|value| value as f32),
            _ => None,
        }
    }

    #[must_use]
    pub fn with_scalar(mut self, channel: usize, next: f32) -> Option<Self> {
        match &mut self {
            Self::Vec2Array(values) => *values.get_mut(channel / 2)?.get_mut(channel % 2)? = next,
            Self::F32List(values) => *values.get_mut(channel)? = next,
            Self::Gpu(value) => {
                *value =
                    value.with_numeric((value.component_count() > 1).then_some(channel), next)?;
            }
            _ => return None,
        }
        Some(self)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HostKeyframe {
    pub time: f64,
    pub value: HostValue,
}

impl TimedKey for HostKeyframe {
    fn time(&self) -> f64 {
        self.time
    }

    fn set_time(&mut self, time: f64) {
        self.time = time;
    }
}

pub type HostKeyframeTrack = KeyTrack<HostKeyframe>;

impl KeyTrack<HostKeyframe> {
    const KEY_EPSILON: f64 = 1e-6;

    fn key_index(&self, time: f64) -> Option<usize> {
        self.key_index_within(time, Self::KEY_EPSILON)
    }

    #[must_use]
    pub fn evaluate(&self, time: f64) -> Option<HostValue> {
        let first = self.keys.first()?;
        let split = self.keys.partition_point(|key| key.time <= time + 1e-9);
        let previous = if split == 0 {
            first
        } else {
            self.keys.get(split.saturating_sub(1))?
        };
        let Some(next) = self.keys.get(split) else {
            return Some(previous.value.clone());
        };
        if next.time <= previous.time + f64::EPSILON {
            return Some(previous.value.clone());
        }
        let t = ((time - previous.time) / (next.time - previous.time)).clamp(0.0, 1.0) as f32;
        Some(interpolate_host_value(&previous.value, &next.value, t))
    }

    #[must_use]
    pub fn has_keyframe(&self, time: f64) -> bool {
        self.key_index(time).is_some()
    }

    pub fn set_value(&mut self, time: f64, value: HostValue) {
        if let Some(index) = self.key_index(time) {
            if let Some(v) = self.keys.get_mut(index) {
                v.value = value;
            }
        } else {
            let index = self.insertion_index(time);
            self.keys.insert(index, HostKeyframe { time, value });
        }
    }

    pub fn remove_keyframe(&mut self, time: f64) {
        self.remove_within(time, Self::KEY_EPSILON);
    }
}

fn interpolate_host_value(a: &HostValue, b: &HostValue, t: f32) -> HostValue {
    match (a, b) {
        (HostValue::Vec2Array(a), HostValue::Vec2Array(b)) if a.len() == b.len() => {
            HostValue::Vec2Array(
                a.iter()
                    .zip(b)
                    .map(|(a, b)| {
                        [
                            (b[0] - a[0]).mul_add(t, a[0]),
                            (b[1] - a[1]).mul_add(t, a[1]),
                        ]
                    })
                    .collect(),
            )
        }
        (HostValue::F32List(a), HostValue::F32List(b)) if a.len() == b.len() => {
            HostValue::F32List(a.iter().zip(b).map(|(a, b)| a + (b - a) * t).collect())
        }

        _ => a.clone(),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HostComponentKeyframes {
    pub base: HostValue,
    pub tracks: Vec<ScalarKeyframeTrack>,
}

impl HostComponentKeyframes {
    const fn scalar_count(value: &HostValue) -> usize {
        value.scalar_count()
    }

    fn scalar(value: &HostValue, channel: usize) -> Option<f32> {
        value.scalar(channel)
    }

    fn with_scalar(value: HostValue, channel: usize, next: f32) -> Option<HostValue> {
        value.with_scalar(channel, next)
    }

    fn evaluate(&self, time: f64) -> Option<HostValue> {
        let mut value = self.base.clone();
        for (channel, track) in self.tracks.iter().enumerate() {
            let Some(next) = track.evaluate(time) else {
                continue;
            };
            value = Self::with_scalar(value, channel, next)?;
        }
        Some(value)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum HostBinding {
    Constant(HostValue),

    Keyframes(HostKeyframeTrack),
    Components(HostComponentKeyframes),

    Gpu(Binding),
}

impl HostBinding {
    pub fn shift_keyframes(&mut self, delta: f64) {
        if delta.abs() <= f64::EPSILON {
            return;
        }
        match self {
            Self::Keyframes(track) => {
                track.shift(delta);
            }
            Self::Components(channels) => {
                for track in &mut channels.tracks {
                    track.shift(delta);
                }
            }
            Self::Gpu(binding) => binding.shift_keyframes(delta),
            Self::Constant(_) => {}
        }
    }

    #[must_use]
    pub const fn gpu(&self) -> Option<&Binding> {
        match self {
            Self::Gpu(binding) => Some(binding),
            _ => None,
        }
    }

    pub const fn gpu_mut(&mut self) -> Option<&mut Binding> {
        match self {
            Self::Gpu(binding) => Some(binding),
            _ => None,
        }
    }

    pub fn evaluate(&self, time: f64) -> Option<HostValue> {
        match self {
            Self::Constant(value) => Some(value.clone()),
            Self::Keyframes(track) => track.evaluate(time),
            Self::Components(channels) => channels.evaluate(time),
            Self::Gpu(binding) => binding.evaluate(time).map(HostValue::Gpu),
        }
    }

    #[must_use]
    pub fn has_keyframe(&self, time: f64) -> bool {
        match self {
            Self::Constant(_) => false,
            Self::Keyframes(track) => track.has_keyframe(time),
            Self::Components(channels) => channels.tracks.iter().any(|track| track.has_key(time)),
            Self::Gpu(binding) => binding.has_keyframe(time),
        }
    }

    #[must_use]
    pub fn has_keyframes(&self) -> bool {
        match self {
            Self::Constant(_) => false,
            Self::Keyframes(track) => !track.keys.is_empty(),
            Self::Components(channels) => {
                channels.tracks.iter().any(|track| !track.keys.is_empty())
            }
            Self::Gpu(binding) => binding.has_keyframes(),
        }
    }

    pub fn set_value(&mut self, time: f64, value: HostValue) {
        match self {
            Self::Constant(current) => *current = value,
            Self::Keyframes(track) => track.set_value(time, value),
            Self::Components(channels) => {
                channels.base = value.clone();
                let count = HostComponentKeyframes::scalar_count(&value);
                if channels.tracks.len() < count {
                    channels
                        .tracks
                        .resize_with(count, ScalarKeyframeTrack::default);
                }
                for channel in 0..count {
                    if let Some(next) = HostComponentKeyframes::scalar(&value, channel) {
                        if let Some(track) = channels.tracks.get_mut(channel) {
                            track.set_key(time, next, Interpolation::Linear);
                        }
                    }
                }
            }
            Self::Gpu(binding) => {
                if let HostValue::Gpu(value) = value {
                    binding.set_value(time, value);
                }
            }
        }
    }

    pub fn toggle_keyframe(&mut self, time: f64) {
        match self {
            Self::Constant(value) if HostComponentKeyframes::scalar_count(value) > 1 => {
                let base = value.clone();
                let count = HostComponentKeyframes::scalar_count(&base);
                let mut channels = HostComponentKeyframes {
                    base: base.clone(),
                    tracks: vec![ScalarKeyframeTrack::default(); count],
                };
                for channel in 0..count {
                    if let Some(next) = HostComponentKeyframes::scalar(&base, channel) {
                        if let Some(track) = channels.tracks.get_mut(channel) {
                            track.set_key(time, next, Interpolation::Linear);
                        }
                    }
                }
                *self = Self::Components(channels);
            }
            Self::Constant(value) => {
                let value = value.clone();
                *self = Self::Keyframes(HostKeyframeTrack {
                    keys: vec![HostKeyframe { time, value }],
                });
            }
            Self::Keyframes(track) if track.has_keyframe(time) => {
                let fallback = track.evaluate(time);
                track.remove_keyframe(time);
                if track.keys.is_empty() {
                    if let Some(value) = fallback {
                        *self = Self::Constant(value);
                    }
                }
            }
            Self::Keyframes(track) => {
                if let Some(value) = track.evaluate(time) {
                    track.set_value(time, value);
                }
            }
            Self::Components(channels) => {
                let any = channels.tracks.iter().any(|track| track.has_key(time));
                if any {
                    for track in &mut channels.tracks {
                        track.remove_key(time);
                    }
                    if channels.tracks.iter().all(|track| track.keys.is_empty()) {
                        let value = channels
                            .evaluate(time)
                            .unwrap_or_else(|| channels.base.clone());
                        *self = Self::Constant(value);
                    }
                } else if let Some(value) = channels.evaluate(time) {
                    for channel in 0..channels.tracks.len() {
                        if let Some(next) = HostComponentKeyframes::scalar(&value, channel) {
                            if let Some(track) = channels.tracks.get_mut(channel) {
                                track.set_key(time, next, Interpolation::Linear);
                            }
                        }
                    }
                }
            }
            Self::Gpu(binding) => binding.toggle_keyframe(time),
        }
    }

    fn ensure_components(&mut self) -> Option<&mut HostComponentKeyframes> {
        if let Self::Keyframes(track) = self {
            let first = track.keys.first()?.value.clone();
            let count = HostComponentKeyframes::scalar_count(&first);
            if count == 0 {
                return None;
            }
            let mut channels = HostComponentKeyframes {
                base: first,
                tracks: vec![ScalarKeyframeTrack::default(); count],
            };
            for key in &track.keys {
                for channel in 0..count {
                    if let Some(value) = HostComponentKeyframes::scalar(&key.value, channel) {
                        if let Some(track) = channels.tracks.get_mut(channel) {
                            track.set_key(key.time, value, Interpolation::Linear);
                        }
                    }
                }
            }
            *self = Self::Components(channels);
        }
        match self {
            Self::Components(channels) => Some(channels),
            _ => None,
        }
    }

    fn prepare_scalar_edit(&mut self) -> bool {
        !matches!(self, Self::Keyframes(track) if track
            .keys
            .first()
            .is_some_and(|key| HostComponentKeyframes::scalar_count(&key.value) > 0))
            || self.ensure_components().is_some()
    }

    #[must_use]
    pub fn scalar_keys(&self, channel: usize) -> Vec<ScalarKeyframe> {
        match self {
            Self::Gpu(binding) => binding.scalar_keys(channel),
            Self::Components(channels) => channels
                .tracks
                .get(channel)
                .map(|track| track.keys.clone())
                .unwrap_or_default(),
            Self::Keyframes(track) => track
                .keys
                .iter()
                .filter_map(|key| {
                    HostComponentKeyframes::scalar(&key.value, channel).map(|value| {
                        ScalarKeyframe {
                            time: key.time,
                            value,
                            interpolation: Interpolation::Linear,
                            ease_in: EasingHandle::LINEAR,
                            ease_out: EasingHandle::LINEAR,
                            custom_ease_in: false,
                            custom_ease_out: false,
                        }
                    })
                })
                .collect(),
            Self::Constant(_) => Vec::new(),
        }
    }

    pub fn edit_scalar_key_easing(
        &mut self,
        channel: usize,
        time: f64,
        incoming: bool,
        handle: EasingHandle,
    ) -> bool {
        if let Self::Gpu(binding) = self {
            return binding.edit_scalar_key_easing(channel, time, incoming, handle);
        }
        if !self.prepare_scalar_edit() {
            return false;
        }
        let Self::Components(channels) = self else {
            return false;
        };
        channels
            .tracks
            .get_mut(channel)
            .is_some_and(|track| track.edit_easing(time, incoming, handle))
    }

    pub fn scalar_count(&self) -> usize {
        match self {
            Self::Gpu(binding) => binding.evaluate(0.0).map_or(0, GpuValue::component_count),
            Self::Constant(value) => HostComponentKeyframes::scalar_count(value),
            Self::Keyframes(track) => track
                .keys
                .first()
                .map_or(0, |key| HostComponentKeyframes::scalar_count(&key.value)),
            Self::Components(channels) => channels.tracks.len(),
        }
    }

    pub fn edit_scalar_key(
        &mut self,
        channel: usize,
        time: f64,
        next_time: Option<f64>,
        next_value: Option<f32>,
        interpolation: Option<Interpolation>,
    ) -> bool {
        if let Self::Gpu(binding) = self {
            return binding.edit_scalar_key(channel, time, next_time, next_value, interpolation);
        }
        if !self.prepare_scalar_edit() {
            return false;
        }
        let Self::Components(channels) = self else {
            return false;
        };
        channels
            .tracks
            .get_mut(channel)
            .is_some_and(|track| track.edit(time, next_time, next_value, interpolation))
    }

    pub fn remove_scalar_key(&mut self, channel: usize, time: f64) -> bool {
        if let Self::Gpu(binding) = self {
            return binding.remove_scalar_key(channel, time);
        }
        if !self.prepare_scalar_edit() {
            return false;
        }
        let Self::Components(channels) = self else {
            return false;
        };
        channels
            .tracks
            .get_mut(channel)
            .is_some_and(|track| track.remove_key(time))
    }
}
