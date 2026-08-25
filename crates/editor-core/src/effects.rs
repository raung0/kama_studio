use std::{
    collections::{BTreeMap, HashMap, HashSet},
    hash::Hash,
};

use serde::{Deserialize, Serialize};

pub type PipelineId = u64;
pub type NodeId = u64;

pub const LOCAL_TRANSFORM_NODE_ID: NodeId = 0;
pub const PIPELINE_NODE_TYPE: &str = "builtin.pipeline";

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub enum GpuValue {
    F32(f32),
    I32(i32),
    U32(u32),
    Bool(bool),
    Vec2([f32; 2]),
    Vec3([f32; 3]),
    Vec4([f32; 4]),
    Color([f32; 4]),
    Enum(u32),
}

impl GpuValue {
    #[must_use]
    pub const fn f32(self) -> Option<f32> {
        match self {
            Self::F32(value) => Some(value),
            _ => None,
        }
    }

    #[must_use]
    pub const fn bool(self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(value),
            _ => None,
        }
    }

    #[must_use]
    pub const fn vec2(self) -> Option<[f32; 2]> {
        match self {
            Self::Vec2(value) => Some(value),
            _ => None,
        }
    }

    #[must_use]
    pub const fn enum_index(self) -> Option<u32> {
        match self {
            Self::Enum(value) => Some(value),
            _ => None,
        }
    }

    #[must_use]
    pub fn numeric_count(self) -> Option<usize> {
        Some(match self {
            Self::U32(value) | Self::Enum(value) => value as usize,
            Self::I32(value) => value.max(0) as usize,
            Self::F32(value) => value.round().max(0.0) as usize,
            _ => return None,
        })
    }

    #[must_use]
    pub const fn color(self) -> Option<[f32; 4]> {
        match self {
            Self::Color(value) => Some(value),
            _ => None,
        }
    }

    #[must_use]
    pub const fn zeroed(self) -> Self {
        match self {
            Self::F32(_) => Self::F32(0.0),
            Self::I32(_) => Self::I32(0),
            Self::U32(_) => Self::U32(0),
            Self::Bool(_) => Self::Bool(false),
            Self::Vec2(_) => Self::Vec2([0.0; 2]),
            Self::Vec3(_) => Self::Vec3([0.0; 3]),
            Self::Vec4(_) => Self::Vec4([0.0; 4]),
            Self::Color(_) => Self::Color([0.0, 0.0, 0.0, 1.0]),
            Self::Enum(_) => Self::Enum(0),
        }
    }

    #[must_use]
    pub fn compatible(self, other: Self) -> bool {
        std::mem::discriminant(&self) == std::mem::discriminant(&other)
    }

    #[must_use]
    pub const fn component_count(self) -> usize {
        match self {
            Self::F32(_) | Self::I32(_) | Self::U32(_) | Self::Bool(_) | Self::Enum(_) => 1,
            Self::Vec2(_) => 2,
            Self::Vec3(_) => 3,
            Self::Vec4(_) | Self::Color(_) => 4,
        }
    }

    #[must_use]
    pub const fn components_linkable(self) -> bool {
        matches!(self, Self::Vec2(_) | Self::Vec3(_) | Self::Vec4(_))
    }

    #[must_use]
    pub fn numeric(self, component: Option<usize>) -> Option<f64> {
        Some(match (self, component) {
            (Self::F32(value), None | Some(0)) => f64::from(value),
            (Self::I32(value), None | Some(0)) => f64::from(value),
            (Self::U32(value), None | Some(0)) => f64::from(value),
            (Self::Bool(value), None | Some(0)) => f64::from(u8::from(value)),
            (Self::Enum(value), None | Some(0)) => f64::from(value),
            (Self::Vec2(value), Some(component)) => f64::from(*value.get(component)?),
            (Self::Vec3(value), Some(component)) => f64::from(*value.get(component)?),
            (Self::Vec4(value) | Self::Color(value), Some(component)) => {
                f64::from(*value.get(component)?)
            }
            _ => return None,
        })
    }

    #[must_use]
    pub fn with_numeric(self, component: Option<usize>, next: f32) -> Option<Self> {
        Some(match (self, component) {
            (Self::F32(_), None | Some(0)) => Self::F32(next),
            (Self::I32(_), None | Some(0)) => Self::I32(next.round() as i32),
            (Self::U32(_), None | Some(0)) => Self::U32(next.round().max(0.0) as u32),
            (Self::Bool(_), None | Some(0)) => Self::Bool(next >= 0.5),
            (Self::Enum(_), None | Some(0)) => Self::Enum(next.round().max(0.0) as u32),
            (Self::Vec2(mut values), Some(component)) => {
                *values.get_mut(component)? = next;
                Self::Vec2(values)
            }
            (Self::Vec3(mut values), Some(component)) => {
                *values.get_mut(component)? = next;
                Self::Vec3(values)
            }
            (Self::Vec4(mut values), Some(component)) => {
                *values.get_mut(component)? = next;
                Self::Vec4(values)
            }
            (Self::Color(mut values), Some(component)) => {
                *values.get_mut(component)? = next.clamp(0.0, 1.0);
                Self::Color(values)
            }
            _ => return None,
        })
    }

    #[must_use]
    pub fn with_component(self, component: usize, next: f32, linked: bool) -> Option<Self> {
        fn update(values: &mut [f32], component: usize, next: f32, linked: bool) -> bool {
            let Some(current) = values.get(component).copied() else {
                return false;
            };
            if linked && values.len() > 1 {
                if current.abs() > 0.000_001 {
                    let ratio = next / current;
                    for (index, value) in values.iter_mut().enumerate() {
                        if index != component {
                            *value *= ratio;
                        }
                    }
                } else {
                    let delta = next - current;
                    for (index, value) in values.iter_mut().enumerate() {
                        if index != component {
                            *value += delta;
                        }
                    }
                }
            }
            values[component] = next;
            true
        }

        Some(match self {
            Self::F32(_) if component == 0 => Self::F32(next),
            Self::I32(_) if component == 0 => Self::I32(next.round() as i32),
            Self::U32(_) if component == 0 => Self::U32(next.round().max(0.0) as u32),
            Self::Enum(_) if component == 0 => Self::Enum(next.round().max(0.0) as u32),
            Self::Bool(_) if component == 0 => Self::Bool(next >= 0.5),
            Self::Vec2(mut values) => {
                update(&mut values, component, next, linked).then_some(())?;
                Self::Vec2(values)
            }
            Self::Vec3(mut values) => {
                update(&mut values, component, next, linked).then_some(())?;
                Self::Vec3(values)
            }
            Self::Vec4(mut values) => {
                update(&mut values, component, next, linked).then_some(())?;
                Self::Vec4(values)
            }
            Self::Color(mut values) if component < 4 => {
                values[component] = next.clamp(0.0, 1.0);
                Self::Color(values)
            }
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Interpolation {
    Step,
    Linear,
    Cubic,
    EaseIn,
    EaseOut,
    EaseInOut,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct EasingHandle {
    pub x: f32,
    pub y: f32,
}

impl EasingHandle {
    pub const LINEAR: Self = Self {
        x: 1.0 / 3.0,
        y: 1.0 / 3.0,
    };

    pub(crate) const fn clamped(self) -> Self {
        Self {
            x: self.x.clamp(0.001, 0.999),
            y: self.y.clamp(-4.0, 4.0),
        }
    }
}

const fn default_easing_handle() -> EasingHandle {
    EasingHandle::LINEAR
}

#[must_use]
pub fn preset_out_handle(interpolation: Interpolation) -> EasingHandle {
    match interpolation {
        Interpolation::Cubic | Interpolation::EaseOut | Interpolation::EaseInOut => EasingHandle {
            x: 1.0 / 3.0,
            y: 0.0,
        },
        _ => EasingHandle::LINEAR,
    }
}

#[must_use]
pub fn preset_in_handle(interpolation: Interpolation) -> EasingHandle {
    match interpolation {
        Interpolation::Cubic | Interpolation::EaseIn | Interpolation::EaseInOut => EasingHandle {
            x: 1.0 / 3.0,
            y: 0.0,
        },
        _ => EasingHandle::LINEAR,
    }
}

fn cubic_bezier(a: f32, b: f32, t: f32) -> f32 {
    let inv = 1.0 - t;
    (t * t).mul_add(t, (3.0 * inv * t * t).mul_add(b, 3.0 * inv * inv * t * a))
}

#[must_use]
pub fn bezier_easing_amount(out: EasingHandle, incoming: EasingHandle, x: f32) -> f32 {
    let out = out.clamped();
    let incoming = incoming.clamped();
    let p1x = out.x;
    let p1y = out.y;
    let p2x = 1.0 - incoming.x;
    let p2y = 1.0 - incoming.y;
    let x = x.clamp(0.0, 1.0);

    let mut lo = 0.0;
    let mut hi = 1.0;
    for _ in 0..12 {
        let u = (lo + hi) * 0.5;
        if cubic_bezier(p1x, p2x, u) < x {
            lo = u;
        } else {
            hi = u;
        }
    }
    cubic_bezier(p1y, p2y, (lo + hi) * 0.5)
}

#[must_use]
pub fn interpolation_amount(left: Interpolation, right: Interpolation, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if left == Interpolation::Step {
        return 0.0;
    }

    let ease_out = matches!(
        left,
        Interpolation::Cubic | Interpolation::EaseOut | Interpolation::EaseInOut
    );
    let ease_in = matches!(
        right,
        Interpolation::Cubic | Interpolation::EaseIn | Interpolation::EaseInOut
    );
    if !ease_out && !ease_in {
        return t;
    }

    let t2 = t * t;
    let t3 = t2 * t;
    let start_slope = if ease_out { 0.0 } else { 1.0 };
    let end_slope = if ease_in { 0.0 } else { 1.0 };
    (t3 - t2).mul_add(
        end_slope,
        (2.0f32.mul_add(-t2, t3) + t).mul_add(start_slope, 3.0f32.mul_add(t2, -2.0 * t3)),
    )
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnimatedKey<T> {
    pub time: f64,
    pub value: T,
    pub interpolation: Interpolation,
    #[serde(default = "default_easing_handle")]
    pub ease_in: EasingHandle,
    #[serde(default = "default_easing_handle")]
    pub ease_out: EasingHandle,
    #[serde(default)]
    pub custom_ease_in: bool,
    #[serde(default)]
    pub custom_ease_out: bool,
}

#[doc(hidden)]
pub trait TimedKey {
    fn time(&self) -> f64;
    fn set_time(&mut self, time: f64);
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeyTrack<K> {
    pub keys: Vec<K>,
}

impl<K> Default for KeyTrack<K> {
    fn default() -> Self {
        Self { keys: Vec::new() }
    }
}

impl<K: TimedKey> KeyTrack<K> {
    pub(crate) fn key_index_within(&self, time: f64, epsilon: f64) -> Option<usize> {
        let index = self.keys.partition_point(|key| key.time() < time - epsilon);
        self.keys
            .get(index)
            .is_some_and(|key| (key.time() - time).abs() <= epsilon)
            .then_some(index)
    }

    pub(crate) fn insertion_index(&self, time: f64) -> usize {
        self.keys.partition_point(|key| key.time() < time)
    }

    pub(crate) fn remove_within(&mut self, time: f64, epsilon: f64) -> bool {
        self.key_index_within(time, epsilon)
            .map(|index| self.keys.remove(index))
            .is_some()
    }

    pub(crate) fn retime(&mut self, index: usize, time: f64) {
        self.keys[index].set_time(time.max(0.0));
        self.keys.sort_by(|a, b| a.time().total_cmp(&b.time()));
    }

    pub(crate) fn shift(&mut self, delta: f64) {
        self.keys
            .iter_mut()
            .for_each(|key| key.set_time(key.time() + delta));
    }
}

impl<T> TimedKey for AnimatedKey<T> {
    fn time(&self) -> f64 {
        self.time
    }

    fn set_time(&mut self, time: f64) {
        self.time = time;
    }
}

#[doc(hidden)]
pub trait AnimatedValue: Copy {
    fn interpolate(self, other: Self, amount: f32) -> Self;
}

impl AnimatedValue for f32 {
    fn interpolate(self, other: Self, amount: f32) -> Self {
        (other - self).mul_add(amount, self)
    }
}

impl AnimatedValue for GpuValue {
    fn interpolate(self, other: Self, amount: f32) -> Self {
        interpolate(self, other, amount)
    }
}

impl<T: AnimatedValue> KeyTrack<AnimatedKey<T>> {
    const EPSILON: f64 = 1.0 / 24_000.0;

    #[must_use]
    pub fn evaluate(&self, time: f64) -> Option<T> {
        let first = self.keys.first()?;
        if time <= first.time {
            return Some(first.value);
        }
        let last = self.keys.last()?;
        if time >= last.time {
            return Some(last.value);
        }
        let right = self.keys.partition_point(|key| key.time <= time);
        let a = &self.keys[right.saturating_sub(1)];
        let b = &self.keys[right];
        if a.interpolation == Interpolation::Step || b.time <= a.time {
            return Some(a.value);
        }
        let t = ((time - a.time) / (b.time - a.time)).clamp(0.0, 1.0) as f32;
        let out = if a.custom_ease_out {
            a.ease_out
        } else {
            preset_out_handle(a.interpolation)
        };
        let incoming = if b.custom_ease_in {
            b.ease_in
        } else {
            preset_in_handle(b.interpolation)
        };
        let amount = bezier_easing_amount(out, incoming, t);
        Some(a.value.interpolate(b.value, amount))
    }

    pub fn set_key(&mut self, time: f64, value: T, interpolation: Interpolation) {
        if let Some(index) = self.key_index(time) {
            let key = &mut self.keys[index];
            key.value = value;
            key.interpolation = interpolation;
            return;
        }
        let index = self.insertion_index(time);
        self.keys.insert(
            index,
            AnimatedKey {
                time,
                value,
                interpolation,
                ease_in: default_easing_handle(),
                ease_out: default_easing_handle(),
                custom_ease_in: false,
                custom_ease_out: false,
            },
        );
    }

    pub fn edit(
        &mut self,
        time: f64,
        next_time: Option<f64>,
        next_value: Option<T>,
        interpolation: Option<Interpolation>,
    ) -> bool {
        let Some(index) = self.key_index(time) else {
            return false;
        };
        let key = &mut self.keys[index];
        if let Some(value) = next_value {
            key.value = value;
        }
        if let Some(interpolation) = interpolation {
            key.interpolation = interpolation;
            key.custom_ease_in = false;
            key.custom_ease_out = false;
        }
        if let Some(next_time) = next_time {
            self.retime(index, next_time);
        }
        true
    }

    pub fn edit_easing(&mut self, time: f64, incoming: bool, handle: EasingHandle) -> bool {
        let Some(index) = self.key_index(time) else {
            return false;
        };
        let key = &mut self.keys[index];
        if incoming {
            key.ease_in = handle.clamped();
            key.custom_ease_in = true;
        } else {
            key.ease_out = handle.clamped();
            key.custom_ease_out = true;
        }
        key.interpolation = Interpolation::Cubic;
        true
    }

    #[must_use]
    pub fn key_index(&self, time: f64) -> Option<usize> {
        self.key_index_within(time, Self::EPSILON)
    }

    #[must_use]
    pub fn has_key(&self, time: f64) -> bool {
        self.key_index(time).is_some()
    }

    pub fn remove_key(&mut self, time: f64) -> bool {
        self.remove_within(time, Self::EPSILON)
    }
}

pub type ScalarKeyframe = AnimatedKey<f32>;
pub type ScalarKeyframeTrack = KeyTrack<ScalarKeyframe>;
pub type Keyframe = AnimatedKey<GpuValue>;
pub type KeyframeTrack = KeyTrack<Keyframe>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ComponentKeyframes {
    pub base: GpuValue,
    pub tracks: Vec<ScalarKeyframeTrack>,
}

impl ComponentKeyframes {
    fn component_option(&self, component: usize) -> Option<usize> {
        (self.base.component_count() > 1).then_some(component)
    }

    #[must_use]
    pub fn evaluate(&self, time: f64) -> Option<GpuValue> {
        let mut value = self.base;
        for (component, track) in self.tracks.iter().enumerate() {
            let Some(next) = track.evaluate(time) else {
                continue;
            };
            value = value.with_numeric(self.component_option(component), next)?;
        }
        Some(value)
    }
}

fn interpolate(a: GpuValue, b: GpuValue, amount: f32) -> GpuValue {
    let lerp = |a: f32, b: f32| (b - a).mul_add(amount, a);
    match (a, b) {
        (GpuValue::F32(a), GpuValue::F32(b)) => GpuValue::F32(lerp(a, b)),
        (GpuValue::I32(a), GpuValue::I32(b)) => {
            GpuValue::I32(lerp(a as f32, b as f32).round() as i32)
        }
        (GpuValue::U32(a), GpuValue::U32(b)) => {
            GpuValue::U32(lerp(a as f32, b as f32).round().max(0.0) as u32)
        }
        (GpuValue::Vec2(a), GpuValue::Vec2(b)) => {
            GpuValue::Vec2([lerp(a[0], b[0]), lerp(a[1], b[1])])
        }
        (GpuValue::Vec3(a), GpuValue::Vec3(b)) => {
            GpuValue::Vec3([lerp(a[0], b[0]), lerp(a[1], b[1]), lerp(a[2], b[2])])
        }
        (GpuValue::Vec4(a), GpuValue::Vec4(b)) => GpuValue::Vec4([
            lerp(a[0], b[0]),
            lerp(a[1], b[1]),
            lerp(a[2], b[2]),
            lerp(a[3], b[3]),
        ]),
        (GpuValue::Color(a), GpuValue::Color(b)) => GpuValue::Color([
            lerp(a[0], b[0]),
            lerp(a[1], b[1]),
            lerp(a[2], b[2]),
            lerp(a[3], b[3]),
        ]),

        (GpuValue::Bool(_), GpuValue::Bool(_)) | (GpuValue::Enum(_), GpuValue::Enum(_)) => a,
        _ => a,
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SocketRef {
    pub node: NodeId,
    pub output: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ImageBinding {
    Disconnected,
    #[default]
    PipelineInput,
    Node(SocketRef),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Binding {
    Constant(GpuValue),
    Keyframes(KeyframeTrack),
    #[serde(alias = "ComponentKeyframes")]
    Components(ComponentKeyframes),
    Connection(SocketRef),
}

impl Binding {
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
            Self::Constant(_) | Self::Connection(_) => {}
        }
    }

    #[must_use]
    pub fn evaluate(&self, time: f64) -> Option<GpuValue> {
        match self {
            Self::Constant(value) => Some(*value),
            Self::Keyframes(track) => track.evaluate(time),
            Self::Components(channels) => channels.evaluate(time),
            Self::Connection(_) => None,
        }
    }

    fn components_from_track(track: &KeyframeTrack) -> Option<ComponentKeyframes> {
        let first = track.keys.first()?.value;
        let count = first.component_count();
        let mut tracks = vec![ScalarKeyframeTrack::default(); count];
        for key in &track.keys {
            for (component, channel) in tracks.iter_mut().enumerate().take(count) {
                let component_arg = (count > 1).then_some(component);
                if let Some(value) = key.value.numeric(component_arg) {
                    channel.set_key(key.time, value as f32, key.interpolation);
                }
            }
        }
        Some(ComponentKeyframes {
            base: first,
            tracks,
        })
    }

    fn ensure_components(&mut self) -> Option<&mut ComponentKeyframes> {
        if let Self::Keyframes(track) = self {
            let channels = Self::components_from_track(track)?;
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
            .is_some_and(|key| key.value.component_count() > 1))
            || self.ensure_components().is_some()
    }

    pub fn set_value(&mut self, time: f64, value: GpuValue) {
        match self {
            Self::Constant(current) => *current = value,
            Self::Keyframes(track) => track.set_key(time, value, default_interpolation(value)),
            Self::Components(channels) => {
                channels.base = value;
                let count = value.component_count();
                if channels.tracks.len() < count {
                    channels
                        .tracks
                        .resize_with(count, ScalarKeyframeTrack::default);
                }
                for component in 0..count {
                    let arg = (count > 1).then_some(component);
                    if let Some(next) = value.numeric(arg) {
                        let interpolation = channels.tracks[component]
                            .keys
                            .last()
                            .map_or_else(|| default_interpolation(value), |key| key.interpolation);
                        channels.tracks[component].set_key(time, next as f32, interpolation);
                    }
                }
            }
            Self::Connection(_) => {}
        }
    }

    pub fn set_component_value(
        &mut self,
        time: f64,
        component: usize,
        next_component: f32,
        linked: bool,
    ) -> bool {
        let Some(current) = self.evaluate(time) else {
            return false;
        };
        let Some(next) = current.with_component(component, next_component, linked) else {
            return false;
        };
        match self {
            Self::Constant(value) => {
                *value = next;
                true
            }
            Self::Keyframes(track) if current.component_count() > 1 => {
                let Some(channels) = Self::components_from_track(track) else {
                    return false;
                };
                *self = Self::Components(channels);
                self.set_component_value(time, component, next_component, linked)
            }
            Self::Keyframes(track) => {
                track.set_key(time, next, default_interpolation(next));
                true
            }
            Self::Components(channels) => {
                channels.base = next;
                let count = next.component_count();
                if channels.tracks.len() < count {
                    channels
                        .tracks
                        .resize_with(count, ScalarKeyframeTrack::default);
                }
                for index in 0..count {
                    if index != component && !linked {
                        continue;
                    }
                    let arg = (count > 1).then_some(index);
                    if let Some(value) = next.numeric(arg) {
                        let interpolation = channels.tracks[index]
                            .keys
                            .last()
                            .map_or_else(|| default_interpolation(next), |key| key.interpolation);
                        channels.tracks[index].set_key(time, value as f32, interpolation);
                    }
                }
                true
            }
            Self::Connection(_) => false,
        }
    }

    pub fn toggle_keyframe(&mut self, time: f64) {
        match self {
            Self::Constant(value) if value.component_count() > 1 => {
                let value = *value;
                let count = value.component_count();
                let mut channels = ComponentKeyframes {
                    base: value,
                    tracks: vec![ScalarKeyframeTrack::default(); count],
                };
                for component in 0..count {
                    let arg = Some(component);
                    if let Some(next) = value.numeric(arg) {
                        channels.tracks[component].set_key(
                            time,
                            next as f32,
                            default_interpolation(value),
                        );
                    }
                }
                *self = Self::Components(channels);
            }
            Self::Constant(value) => {
                let value = *value;
                let mut track = KeyframeTrack::default();
                track.set_key(time, value, default_interpolation(value));
                *self = Self::Keyframes(track);
            }
            Self::Keyframes(track) => {
                if track.has_key(time) {
                    let fallback = track.evaluate(time);
                    track.remove_key(time);
                    if track.keys.is_empty() {
                        if let Some(value) = fallback {
                            *self = Self::Constant(value);
                        }
                    }
                } else if let Some(value) = track.evaluate(time) {
                    track.set_key(time, value, default_interpolation(value));
                }
            }
            Self::Components(channels) => {
                let any = channels.tracks.iter().any(|track| track.has_key(time));
                if any {
                    for track in &mut channels.tracks {
                        track.remove_key(time);
                    }
                    if channels.tracks.iter().all(|track| track.keys.is_empty()) {
                        let value = channels.evaluate(time).unwrap_or(channels.base);
                        *self = Self::Constant(value);
                    }
                } else if let Some(value) = channels.evaluate(time) {
                    let count = value.component_count();
                    for component in 0..count {
                        let arg = (count > 1).then_some(component);
                        if let Some(next) = value.numeric(arg) {
                            channels.tracks[component].set_key(
                                time,
                                next as f32,
                                default_interpolation(value),
                            );
                        }
                    }
                }
            }
            Self::Connection(_) => {}
        }
    }

    #[must_use]
    pub fn scalar_keys(&self, component: usize) -> Vec<ScalarKeyframe> {
        match self {
            Self::Keyframes(track) => {
                let count = track
                    .keys
                    .first()
                    .map_or(0, |key| key.value.component_count());
                let arg = (count > 1).then_some(component);
                track
                    .keys
                    .iter()
                    .filter_map(|key| {
                        key.value.numeric(arg).map(|value| ScalarKeyframe {
                            time: key.time,
                            value: value as f32,
                            interpolation: key.interpolation,
                            ease_in: key.ease_in,
                            ease_out: key.ease_out,
                            custom_ease_in: key.custom_ease_in,
                            custom_ease_out: key.custom_ease_out,
                        })
                    })
                    .collect()
            }
            Self::Components(channels) => channels
                .tracks
                .get(component)
                .map(|track| track.keys.clone())
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    pub fn edit_scalar_key(
        &mut self,
        component: usize,
        time: f64,
        next_time: Option<f64>,
        next_value: Option<f32>,
        interpolation: Option<Interpolation>,
    ) -> bool {
        if !self.prepare_scalar_edit() {
            return false;
        }
        match self {
            Self::Keyframes(track) => {
                let Some(index) = track
                    .keys
                    .iter()
                    .position(|key| (key.time - time).abs() <= 1.0 / 24_000.0)
                else {
                    return false;
                };
                if let Some(value) = next_value {
                    let current = track.keys[index].value;
                    let arg = (current.component_count() > 1).then_some(component);
                    let Some(next) = current.with_numeric(arg, value) else {
                        return false;
                    };
                    track.keys[index].value = next;
                }
                if let Some(interpolation) = interpolation {
                    track.keys[index].interpolation = interpolation;
                    track.keys[index].custom_ease_in = false;
                    track.keys[index].custom_ease_out = false;
                }
                if let Some(next_time) = next_time {
                    track.keys[index].time = next_time.max(0.0);
                    track.keys.sort_by(|a, b| a.time.total_cmp(&b.time));
                }
                true
            }
            Self::Components(channels) => channels
                .tracks
                .get_mut(component)
                .is_some_and(|track| track.edit(time, next_time, next_value, interpolation)),
            _ => false,
        }
    }

    pub fn edit_scalar_key_easing(
        &mut self,
        component: usize,
        time: f64,
        incoming: bool,
        handle: EasingHandle,
    ) -> bool {
        if !self.prepare_scalar_edit() {
            return false;
        }
        let handle = handle.clamped();
        match self {
            Self::Keyframes(track) => {
                let Some(index) = track
                    .keys
                    .iter()
                    .position(|key| (key.time - time).abs() <= 1.0 / 24_000.0)
                else {
                    return false;
                };
                let key = &mut track.keys[index];
                if incoming {
                    key.ease_in = handle;
                    key.custom_ease_in = true;
                } else {
                    key.ease_out = handle;
                    key.custom_ease_out = true;
                }
                key.interpolation = Interpolation::Cubic;
                true
            }
            Self::Components(channels) => channels
                .tracks
                .get_mut(component)
                .is_some_and(|track| track.edit_easing(time, incoming, handle)),
            _ => false,
        }
    }

    pub fn remove_scalar_key(&mut self, component: usize, time: f64) -> bool {
        if !self.prepare_scalar_edit() {
            return false;
        }
        match self {
            Self::Keyframes(track) => track.remove_key(time),
            Self::Components(channels) => channels
                .tracks
                .get_mut(component)
                .is_some_and(|track| track.remove_key(time)),
            _ => false,
        }
    }

    #[must_use]
    pub fn has_keyframe(&self, time: f64) -> bool {
        match self {
            Self::Keyframes(track) => track.has_key(time),
            Self::Components(channels) => channels.tracks.iter().any(|track| track.has_key(time)),
            _ => false,
        }
    }

    #[must_use]
    pub fn has_keyframes(&self) -> bool {
        match self {
            Self::Keyframes(track) => !track.keys.is_empty(),
            Self::Components(channels) => {
                channels.tracks.iter().any(|track| !track.keys.is_empty())
            }
            _ => false,
        }
    }
}

#[must_use]
pub const fn default_interpolation(value: GpuValue) -> Interpolation {
    match value {
        GpuValue::Bool(_) | GpuValue::Enum(_) => Interpolation::Step,
        _ => Interpolation::Linear,
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum NodeExecution {
    PointwiseGpu,
    SpatialGpu,
    KernelGpu,
    GeneratorGpu,
    GeneratorCpu,
    CpuWasm,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DynamicImageInputs {
    pub count_input: String,
    pub prefix: String,
    pub min: usize,
    pub max: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EffectNode {
    pub id: NodeId,
    pub node_type: String,
    pub execution: NodeExecution,

    pub ui_position: Option<[f32; 2]>,

    pub image_inputs: BTreeMap<String, ImageBinding>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack_input: Option<String>,
    pub inputs: BTreeMap<String, Binding>,

    pub host_inputs: BTreeMap<String, crate::parameters::HostBinding>,

    pub dynamic_image_inputs: Option<DynamicImageInputs>,
}

#[derive(Clone, Copy, Debug)]
pub enum BuiltinNodePreset {
    Exposure,
    Contrast,
    Invert,
}

impl EffectNode {
    #[must_use]
    pub fn stack_image_input(&self) -> Option<(&str, &ImageBinding)> {
        self.stack_input
            .as_deref()
            .and_then(|name| self.image_inputs.get_key_value(name))
            .or_else(|| self.image_inputs.get_key_value("image"))
            .or_else(|| self.image_inputs.first_key_value())
            .map(|(name, binding)| (name.as_str(), binding))
    }

    #[must_use]
    pub fn stack_image_input_name(&self) -> Option<&str> {
        self.stack_image_input().map(|(name, _)| name)
    }

    pub fn replace_image_source(&mut self, source: NodeId, stack_replacement: &ImageBinding) {
        let stack_input = self.stack_image_input_name().map(str::to_owned);
        for (input, binding) in &mut self.image_inputs {
            if matches!(binding, ImageBinding::Node(socket) if socket.node == source) {
                *binding = if stack_input.as_deref() == Some(input.as_str()) {
                    stack_replacement.clone()
                } else {
                    ImageBinding::Disconnected
                };
            }
        }
    }

    pub fn image_input_names(&self) -> Vec<String> {
        if let Some(dynamic) = &self.dynamic_image_inputs {
            let count = self
                .inputs
                .get(&dynamic.count_input)
                .and_then(|binding| binding.evaluate(0.0))
                .and_then(GpuValue::numeric_count)
                .unwrap_or(dynamic.min)
                .clamp(dynamic.min, dynamic.max);
            return (1..=count)
                .map(|index| format!("{}{}", dynamic.prefix, index))
                .collect();
        }
        self.image_inputs.keys().cloned().collect()
    }

    pub fn sync_dynamic_image_inputs(&mut self) -> bool {
        let Some(dynamic) = self.dynamic_image_inputs.clone() else {
            return false;
        };
        let count = self
            .inputs
            .get(&dynamic.count_input)
            .and_then(|binding| binding.evaluate(0.0))
            .and_then(GpuValue::numeric_count)
            .unwrap_or(dynamic.min)
            .clamp(dynamic.min, dynamic.max);
        let mut next = BTreeMap::new();
        for index in 1..=count {
            let name = format!("{}{}", dynamic.prefix, index);
            next.insert(
                name.clone(),
                self.image_inputs
                    .get(&name)
                    .cloned()
                    .unwrap_or(ImageBinding::Disconnected),
            );
        }
        if next == self.image_inputs {
            return false;
        }
        self.image_inputs = next;
        true
    }

    #[must_use]
    pub fn builtin(id: NodeId, preset: BuiltinNodePreset) -> Self {
        let mut inputs =
            BTreeMap::from([("enabled".into(), Binding::Constant(GpuValue::Bool(true)))]);
        let (node_type, execution) = match preset {
            BuiltinNodePreset::Exposure => {
                inputs.insert("exposure".into(), Binding::Constant(GpuValue::F32(0.0)));
                ("builtin.exposure", NodeExecution::PointwiseGpu)
            }
            BuiltinNodePreset::Contrast => {
                inputs.insert("contrast".into(), Binding::Constant(GpuValue::F32(1.0)));
                ("builtin.contrast", NodeExecution::PointwiseGpu)
            }
            BuiltinNodePreset::Invert => ("builtin.invert", NodeExecution::PointwiseGpu),
        };
        Self {
            id,
            node_type: node_type.into(),
            execution,
            ui_position: None,
            image_inputs: BTreeMap::from([("image".into(), ImageBinding::PipelineInput)]),
            stack_input: Some("image".into()),
            inputs,
            host_inputs: BTreeMap::new(),
            dynamic_image_inputs: None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ValueEvalContext {
    pub timeline_time: f64,
    pub local_time: f64,
    pub frame_index: u64,
    pub frame_rate: f64,
}

#[derive(Clone, Copy)]
enum RuntimeValue {
    TimelineTime,
    LocalTime,
    FrameCount,
    LocalFrame,
    FrameRate,
    Pi,
    Tau,
}

#[derive(Clone, Copy)]
enum ValueNodeOp {
    Constant,
    Runtime(RuntimeValue),
    Unary(fn(f32) -> f32),
    Binary(fn(f32, f32) -> f32),
    Ternary(fn(f32, f32, f32) -> f32),
}

const fn finite_or_zero(value: f32) -> f32 {
    if value.is_finite() {
        value
    } else {
        0.0
    }
}

macro_rules! value_node_kinds {
    ($($variant:ident => ($label:expr, $detail:expr, $inputs:expr, $constant:expr, $runtime:expr, $op:expr)),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
        pub enum ValueNodeKind { $($variant),+ }

        impl ValueNodeKind {
            pub const INSERTABLE: [Self; value_node_kinds!(@count $($variant),+)] = [$(Self::$variant),+];
            pub const fn label(self) -> &'static str { match self { $(Self::$variant => $label),+ } }
            pub const fn detail(self) -> &'static str { match self { $(Self::$variant => $detail),+ } }
            pub const fn is_constant(self) -> bool { match self { $(Self::$variant => $constant),+ } }
            pub const fn is_runtime_source(self) -> bool { match self { $(Self::$variant => $runtime),+ } }
            pub const fn input_names(self) -> &'static [&'static str] { match self { $(Self::$variant => $inputs),+ } }
            fn operation(self) -> ValueNodeOp { match self { $(Self::$variant => $op),+ } }
        }
    };
    (@count $($variant:ident),+) => { <[()]>::len(&[$(value_node_kinds!(@unit $variant)),+]) };
    (@unit $variant:ident) => { () };
}

value_node_kinds! {
    Float => ("Float", "Constant value", &[], true, false, ValueNodeOp::Constant),
    Vec2 => ("Vector 2", "Constant value", &[], true, false, ValueNodeOp::Constant),
    Color => ("Color", "Constant value", &[], true, false, ValueNodeOp::Constant),
    Timestamp => ("Timestamp", "Composition timeline time in seconds", &[], false, true, ValueNodeOp::Runtime(RuntimeValue::TimelineTime)),
    LocalTimestamp => ("Local Timestamp", "Current clip/effect local time in seconds", &[], false, true, ValueNodeOp::Runtime(RuntimeValue::LocalTime)),
    FrameCount => ("Frame Count", "Current composition frame number", &[], false, true, ValueNodeOp::Runtime(RuntimeValue::FrameCount)),
    LocalFrame => ("Local Frame", "Current local frame number", &[], false, true, ValueNodeOp::Runtime(RuntimeValue::LocalFrame)),
    FrameRate => ("Frame Rate", "Current composition frame rate", &[], false, true, ValueNodeOp::Runtime(RuntimeValue::FrameRate)),
    Pi => ("Pi", "Mathematical constant", &[], false, false, ValueNodeOp::Runtime(RuntimeValue::Pi)),
    Tau => ("Tau", "Mathematical constant", &[], false, false, ValueNodeOp::Runtime(RuntimeValue::Tau)),
    Add => ("Add", "Component-wise math value", &["A", "B"], false, false, ValueNodeOp::Binary(|a, b| a + b)),
    Subtract => ("Subtract", "Component-wise math value", &["A", "B"], false, false, ValueNodeOp::Binary(|a, b| a - b)),
    Multiply => ("Multiply", "Component-wise math value", &["A", "B"], false, false, ValueNodeOp::Binary(|a, b| a * b)),
    Divide => ("Divide", "Component-wise math value", &["A", "B"], false, false, ValueNodeOp::Binary(|a, b| if b.abs() <= 1e-8 { 0.0 } else { a / b })),
    Modulo => ("Modulo", "Component-wise math value", &["A", "B"], false, false, ValueNodeOp::Binary(|a, b| if b.abs() <= 1e-8 { 0.0 } else { a.rem_euclid(b) })),
    Power => ("Power", "Component-wise math value", &["A", "B"], false, false, ValueNodeOp::Binary(|a, b| finite_or_zero(a.powf(b)))),
    Min => ("Minimum", "Component-wise math value", &["A", "B"], false, false, ValueNodeOp::Binary(f32::min)),
    Max => ("Maximum", "Component-wise math value", &["A", "B"], false, false, ValueNodeOp::Binary(f32::max)),
    Clamp => ("Clamp", "Clamp Value between Min and Max", &["Value", "Min", "Max"], false, false, ValueNodeOp::Ternary(|value, min, max| value.clamp(min.min(max), min.max(max)))),
    Lerp => ("Lerp", "Interpolate A to B by T", &["A", "B", "T"], false, false, ValueNodeOp::Ternary(|a, b, t| (b - a).mul_add(t, a))),
    Negate => ("Negate", "Unary math value", &["Value"], false, false, ValueNodeOp::Unary(|value| -value)),
    Abs => ("Absolute", "Unary math value", &["Value"], false, false, ValueNodeOp::Unary(f32::abs)),
    Sqrt => ("Square Root", "Unary math value", &["Value"], false, false, ValueNodeOp::Unary(|value| value.max(0.0).sqrt())),
    Sin => ("Sine", "Unary math value", &["Value"], false, false, ValueNodeOp::Unary(f32::sin)),
    Cos => ("Cosine", "Unary math value", &["Value"], false, false, ValueNodeOp::Unary(f32::cos)),
    Tan => ("Tangent", "Unary math value", &["Value"], false, false, ValueNodeOp::Unary(|value| finite_or_zero(value.tan()))),
    Floor => ("Floor", "Unary math value", &["Value"], false, false, ValueNodeOp::Unary(f32::floor)),
    Ceil => ("Ceiling", "Unary math value", &["Value"], false, false, ValueNodeOp::Unary(f32::ceil)),
    Round => ("Round", "Unary math value", &["Value"], false, false, ValueNodeOp::Unary(f32::round)),
    Fract => ("Fraction", "Unary math value", &["Value"], false, false, ValueNodeOp::Unary(f32::fract)),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValueNode {
    pub id: NodeId,
    pub kind: ValueNodeKind,
    pub value: GpuValue,
    pub inputs: BTreeMap<String, Binding>,
    pub ui_position: Option<[f32; 2]>,
}

fn evaluate_value_node_in(
    nodes: &HashMap<NodeId, &ValueNode>,
    node: NodeId,
    context: ValueEvalContext,
    cache: &mut HashMap<NodeId, GpuValue>,
    visiting: &mut std::collections::HashSet<NodeId>,
) -> Option<GpuValue> {
    fn resolve_binding(
        binding: &Binding,
        nodes: &HashMap<NodeId, &ValueNode>,
        context: ValueEvalContext,
        cache: &mut HashMap<NodeId, GpuValue>,
        visiting: &mut std::collections::HashSet<NodeId>,
    ) -> Option<GpuValue> {
        match binding {
            Binding::Connection(socket) if socket.output == "value" => {
                resolve_node(socket.node, nodes, context, cache, visiting)
            }
            Binding::Connection(_) => None,
            _ => binding.evaluate(context.timeline_time),
        }
    }

    fn resolve_node(
        node_id: NodeId,
        nodes: &HashMap<NodeId, &ValueNode>,
        context: ValueEvalContext,
        cache: &mut HashMap<NodeId, GpuValue>,
        visiting: &mut std::collections::HashSet<NodeId>,
    ) -> Option<GpuValue> {
        if let Some(value) = cache.get(&node_id).copied() {
            return Some(value);
        }
        if !visiting.insert(node_id) {
            return None;
        }
        let Some(&node) = nodes.get(&node_id) else {
            visiting.remove(&node_id);
            return None;
        };
        let input = |name: &str,
                     cache: &mut HashMap<NodeId, GpuValue>,
                     visiting: &mut std::collections::HashSet<NodeId>| {
            node.inputs
                .get(name)
                .and_then(|binding| resolve_binding(binding, nodes, context, cache, visiting))
                .unwrap_or(GpuValue::F32(0.0))
        };
        let names = node.kind.input_names();
        let value = match node.kind.operation() {
            ValueNodeOp::Constant => node.value,
            ValueNodeOp::Runtime(source) => GpuValue::F32(match source {
                RuntimeValue::TimelineTime => context.timeline_time as f32,
                RuntimeValue::LocalTime => context.local_time as f32,
                RuntimeValue::FrameCount => context.frame_index as f32,
                RuntimeValue::LocalFrame => {
                    (context.local_time.max(0.0) * context.frame_rate.max(1.0)).floor() as f32
                }
                RuntimeValue::FrameRate => context.frame_rate.max(1.0) as f32,
                RuntimeValue::Pi => std::f32::consts::PI,
                RuntimeValue::Tau => std::f32::consts::TAU,
            }),
            ValueNodeOp::Unary(op) => unary_value(input(names[0], cache, visiting), op),
            ValueNodeOp::Binary(op) => binary_value(
                input(names[0], cache, visiting),
                input(names[1], cache, visiting),
                op,
            ),
            ValueNodeOp::Ternary(op) => ternary_value(
                input(names[0], cache, visiting),
                input(names[1], cache, visiting),
                input(names[2], cache, visiting),
                op,
            ),
        };
        visiting.remove(&node_id);
        cache.insert(node_id, value);
        Some(value)
    }

    #[derive(Clone, Copy)]
    enum Shape {
        Scalar,
        Vec2,
        Vec3,
        Vec4,
        Color,
    }

    const fn lanes(value: GpuValue) -> ([f32; 4], usize, Shape) {
        match value {
            GpuValue::F32(value) => ([value, value, value, value], 1, Shape::Scalar),
            GpuValue::I32(value) => ([value as f32; 4], 1, Shape::Scalar),
            GpuValue::U32(value) | GpuValue::Enum(value) => ([value as f32; 4], 1, Shape::Scalar),
            GpuValue::Bool(value) => ([if value { 1.0 } else { 0.0 }; 4], 1, Shape::Scalar),
            GpuValue::Vec2(value) => ([value[0], value[1], 0.0, 0.0], 2, Shape::Vec2),
            GpuValue::Vec3(value) => ([value[0], value[1], value[2], 0.0], 3, Shape::Vec3),
            GpuValue::Vec4(value) => (value, 4, Shape::Vec4),
            GpuValue::Color(value) => (value, 4, Shape::Color),
        }
    }

    const fn result_shape(a: GpuValue, b: GpuValue) -> (usize, Shape) {
        let (_, ac, ashape) = lanes(a);
        let (_, bc, bshape) = lanes(b);
        if ac >= bc {
            (ac, ashape)
        } else {
            (bc, bshape)
        }
    }

    const fn from_lanes(values: [f32; 4], count: usize, shape: Shape) -> GpuValue {
        match (count, shape) {
            (1, _) => GpuValue::F32(values[0]),
            (2, _) => GpuValue::Vec2([values[0], values[1]]),
            (3, _) => GpuValue::Vec3([values[0], values[1], values[2]]),
            (4, Shape::Color) => GpuValue::Color(values),
            _ => GpuValue::Vec4(values),
        }
    }

    fn unary_value(value: GpuValue, op: impl Fn(f32) -> f32) -> GpuValue {
        let (mut values, count, shape) = lanes(value);
        for value in values.iter_mut().take(count) {
            *value = finite_or_zero(op(*value));
        }
        from_lanes(values, count, shape)
    }

    fn binary_value(a: GpuValue, b: GpuValue, op: impl Fn(f32, f32) -> f32) -> GpuValue {
        let (av, ac, _) = lanes(a);
        let (bv, bc, _) = lanes(b);
        let (count, shape) = result_shape(a, b);
        let mut result = [0.0; 4];
        for index in 0..count {
            let a = av[if ac == 1 { 0 } else { index.min(ac - 1) }];
            let b = bv[if bc == 1 { 0 } else { index.min(bc - 1) }];
            result[index] = finite_or_zero(op(a, b));
        }
        from_lanes(result, count, shape)
    }

    fn ternary_value(
        a: GpuValue,
        b: GpuValue,
        c: GpuValue,
        op: impl Fn(f32, f32, f32) -> f32,
    ) -> GpuValue {
        let (ab_count, ab_shape) = result_shape(a, b);
        let (_, cc, _) = lanes(c);
        let (count, shape) = if cc > ab_count {
            let (_, _, cshape) = lanes(c);
            (cc, cshape)
        } else {
            (ab_count, ab_shape)
        };
        let (av, ac, _) = lanes(a);
        let (bv, bc, _) = lanes(b);
        let (cv, cc, _) = lanes(c);
        let mut result = [0.0; 4];
        for index in 0..count {
            let lane = |values: [f32; 4], lanes: usize| {
                values[if lanes == 1 { 0 } else { index.min(lanes - 1) }]
            };
            result[index] = finite_or_zero(op(lane(av, ac), lane(bv, bc), lane(cv, cc)));
        }
        from_lanes(result, count, shape)
    }

    resolve_node(node, nodes, context, cache, visiting)
}

pub struct ValueEvaluator<'a> {
    nodes: HashMap<NodeId, &'a ValueNode>,
    context: ValueEvalContext,
    cache: HashMap<NodeId, GpuValue>,
    visiting: std::collections::HashSet<NodeId>,
}

impl<'a> ValueEvaluator<'a> {
    #[must_use]
    pub fn new(nodes: &'a [ValueNode], context: ValueEvalContext) -> Self {
        Self {
            nodes: nodes.iter().map(|node| (node.id, node)).collect(),
            context,
            cache: HashMap::with_capacity(nodes.len()),
            visiting: std::collections::HashSet::with_capacity(nodes.len()),
        }
    }

    pub fn evaluate(&mut self, node: NodeId) -> Option<GpuValue> {
        evaluate_value_node_in(
            &self.nodes,
            node,
            self.context,
            &mut self.cache,
            &mut self.visiting,
        )
    }
}

#[must_use]
pub fn evaluate_value_node(
    nodes: &[ValueNode],
    node: NodeId,
    context: ValueEvalContext,
) -> Option<GpuValue> {
    ValueEvaluator::new(nodes, context).evaluate(node)
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum PipelineKind {
    #[default]
    Video,
    Audio,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EffectPipeline {
    pub id: PipelineId,
    pub name: String,
    pub revision: u64,
    pub kind: PipelineKind,
    pub nodes: Vec<EffectNode>,
    pub value_nodes: Vec<ValueNode>,
    pub output: ImageBinding,
    pub ui_input_position: Option<[f32; 2]>,
    pub ui_output_position: Option<[f32; 2]>,
}

pub trait DependencyNode {
    fn id(&self) -> NodeId;
    fn push_dependencies(&self, stack: &mut Vec<NodeId>);
}

impl DependencyNode for EffectNode {
    fn id(&self) -> NodeId {
        self.id
    }
    fn push_dependencies(&self, stack: &mut Vec<NodeId>) {
        stack.extend(self.image_inputs.values().filter_map(image_source));
    }
}

impl DependencyNode for ValueNode {
    fn id(&self) -> NodeId {
        self.id
    }
    fn push_dependencies(&self, stack: &mut Vec<NodeId>) {
        stack.extend(self.inputs.values().filter_map(|binding| match binding {
            Binding::Connection(socket) => Some(socket.node),
            _ => None,
        }));
    }
}

pub struct DependencyGraph<'a, N> {
    nodes: &'a [N],
    indices: HashMap<NodeId, usize>,
}

impl<'a, N: DependencyNode> DependencyGraph<'a, N> {
    pub fn new(nodes: &'a [N]) -> Self {
        Self {
            nodes,
            indices: nodes
                .iter()
                .enumerate()
                .map(|(index, node)| (node.id(), index))
                .collect(),
        }
    }

    #[must_use]
    pub fn contains(&self, id: NodeId) -> bool {
        self.indices.contains_key(&id)
    }

    #[must_use]
    pub fn node(&self, id: NodeId) -> Option<&'a N> {
        self.indices.get(&id).map(|&index| &self.nodes[index])
    }

    #[must_use]
    pub fn depends_on(&self, start: NodeId, target: NodeId) -> bool {
        let mut stack = vec![start];
        let mut seen = HashSet::new();
        while let Some(id) = stack.pop() {
            if let Some(node) = seen.insert(id).then(|| self.node(id)).flatten() {
                let previous = stack.len();
                node.push_dependencies(&mut stack);
                if stack[previous..].contains(&target) {
                    return true;
                }
            }
        }
        false
    }
}

pub type ImageGraphIndex<'a> = DependencyGraph<'a, EffectNode>;
pub type ValueGraphIndex<'a> = DependencyGraph<'a, ValueNode>;

impl<'a> DependencyGraph<'a, EffectNode> {
    pub fn stack_depends_on(&self, mut start: NodeId, target: NodeId) -> bool {
        let mut seen = HashSet::new();
        while seen.insert(start) {
            if start == target {
                return true;
            }
            let Some(next) = self
                .node(start)
                .and_then(EffectNode::stack_image_input)
                .and_then(|(_, binding)| image_source(binding))
            else {
                break;
            };
            start = next;
        }
        false
    }

    #[must_use]
    pub fn main_path(&self, output: &ImageBinding) -> Vec<&'a EffectNode> {
        let mut path = Vec::new();
        let mut cursor = image_source(output);
        let mut seen = HashSet::new();
        while let Some(id) = cursor.filter(|id| seen.insert(*id)) {
            let Some(node) = self.node(id) else { break };
            path.push(node);
            cursor = node
                .stack_image_input()
                .and_then(|(_, binding)| image_source(binding));
        }
        path.reverse();
        path
    }

    #[must_use]
    pub fn stack_evaluation_order(&self, output: &ImageBinding) -> Vec<usize> {
        fn visit(
            index: &DependencyGraph<'_, EffectNode>,
            id: NodeId,
            seen: &mut HashSet<NodeId>,
            out: &mut Vec<usize>,
        ) {
            if !seen.insert(id) {
                return;
            }
            let Some(&position) = index.indices.get(&id) else {
                return;
            };
            if let Some(source) = index.nodes[position]
                .stack_image_input()
                .and_then(|(_, binding)| image_source(binding))
            {
                visit(index, source, seen, out);
            }
            out.push(position);
        }

        let mut order = Vec::with_capacity(self.nodes.len());
        if let Some(output) = image_source(output) {
            visit(self, output, &mut HashSet::new(), &mut order);
        }
        order
    }
}

const fn image_source(binding: &ImageBinding) -> Option<NodeId> {
    match binding {
        ImageBinding::Node(socket) => Some(socket.node),
        ImageBinding::Disconnected | ImageBinding::PipelineInput => None,
    }
}

impl EffectPipeline {
    #[must_use]
    pub fn node(&self, id: NodeId) -> Option<&EffectNode> {
        self.nodes.iter().find(|node| node.id == id)
    }

    pub fn node_mut(&mut self, id: NodeId) -> Option<&mut EffectNode> {
        self.nodes.iter_mut().find(|node| node.id == id)
    }

    #[must_use]
    pub fn value_node(&self, id: NodeId) -> Option<&ValueNode> {
        self.value_nodes.iter().find(|node| node.id == id)
    }

    pub fn value_node_mut(&mut self, id: NodeId) -> Option<&mut ValueNode> {
        self.value_nodes.iter_mut().find(|node| node.id == id)
    }

    #[must_use]
    pub fn main_path(&self) -> Vec<&EffectNode> {
        ImageGraphIndex::new(&self.nodes).main_path(&self.output)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InputAddress {
    pub node: NodeId,
    pub input: String,
}

#[derive(Clone, Debug, Default)]
pub struct BindingOverrides {
    values: BTreeMap<NodeId, BTreeMap<String, Binding>>,
}

impl BindingOverrides {
    #[must_use]
    pub fn get(&self, node: NodeId, input: &str) -> Option<&Binding> {
        self.values.get(&node)?.get(input)
    }

    pub fn get_mut(&mut self, node: NodeId, input: &str) -> Option<&mut Binding> {
        self.values.get_mut(&node)?.get_mut(input)
    }

    #[must_use]
    pub fn contains(&self, node: NodeId, input: &str) -> bool {
        self.get(node, input).is_some()
    }

    pub fn iter(&self) -> impl Iterator<Item = (NodeId, &str, &Binding)> {
        self.values.iter().flat_map(|(&node, inputs)| {
            inputs
                .iter()
                .map(move |(input, binding)| (node, input.as_str(), binding))
        })
    }

    pub fn insert(
        &mut self,
        node: NodeId,
        input: impl Into<String>,
        binding: Binding,
    ) -> Option<Binding> {
        self.values
            .entry(node)
            .or_default()
            .insert(input.into(), binding)
    }

    pub fn shift_keyframes(&mut self, delta: f64) {
        for inputs in self.values.values_mut() {
            for binding in inputs.values_mut() {
                binding.shift_keyframes(delta);
            }
        }
    }

    pub fn remove(&mut self, node: NodeId, input: &str) -> Option<Binding> {
        let inputs = self.values.get_mut(&node)?;
        let removed = inputs.remove(input);
        if inputs.is_empty() {
            self.values.remove(&node);
        }
        removed
    }

    pub fn clear(&mut self) {
        self.values.clear();
    }

    pub fn retain(&mut self, mut keep: impl FnMut(NodeId, &str, &mut Binding) -> bool) {
        self.values.retain(|node, inputs| {
            inputs.retain(|input, binding| keep(*node, input, binding));
            !inputs.is_empty()
        });
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PipelineInstance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_input_position: Option<[f32; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_output_position: Option<[f32; 2]>,
    pub local_nodes: Vec<EffectNode>,
    pub local_output: ImageBinding,
    pub pipeline: Option<PipelineId>,
    #[serde(with = "binding_override_map")]
    pub overrides: BindingOverrides,
}

impl PipelineInstance {
    pub fn shift_keyframes(&mut self, delta: f64) {
        if delta.abs() <= f64::EPSILON {
            return;
        }
        for node in &mut self.local_nodes {
            for binding in node.inputs.values_mut() {
                binding.shift_keyframes(delta);
            }
            for binding in node.host_inputs.values_mut() {
                binding.shift_keyframes(delta);
            }
        }
        self.overrides.shift_keyframes(delta);
    }
}

fn resolved_node_binding<'a>(
    node: &'a EffectNode,
    instance: Option<&'a PipelineInstance>,
    input: &str,
) -> Option<&'a Binding> {
    instance
        .and_then(|instance| instance.overrides.get(node.id, input))
        .or_else(|| node.inputs.get(input))
}

pub fn resolved_node_input_cached(
    node: &EffectNode,
    instance: Option<&PipelineInstance>,
    input: &str,
    evaluator: &mut ValueEvaluator<'_>,
) -> Option<GpuValue> {
    match resolved_node_binding(node, instance, input)? {
        Binding::Connection(socket) if socket.output == "value" => evaluator.evaluate(socket.node),
        Binding::Connection(_) => None,
        binding => binding.evaluate(evaluator.context.timeline_time),
    }
}

mod binding_override_map {
    use super::{Binding, BindingOverrides, InputAddress, NodeId};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize)]
    struct AddressRef<'a> {
        node: NodeId,
        input: &'a str,
    }

    #[derive(Serialize)]
    struct EntryRef<'a> {
        input: AddressRef<'a>,
        binding: &'a Binding,
    }

    #[derive(Deserialize)]
    struct Entry {
        input: InputAddress,
        binding: Binding,
    }

    pub fn serialize<S>(values: &BindingOverrides, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        values
            .values
            .iter()
            .flat_map(|(node, inputs)| {
                inputs.iter().map(move |(input, binding)| EntryRef {
                    input: AddressRef { node: *node, input },
                    binding,
                })
            })
            .collect::<Vec<_>>()
            .serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<BindingOverrides, D::Error>
    where
        D: Deserializer<'de>,
    {
        let entries = Vec::<Entry>::deserialize(deserializer)?;
        let mut values = BindingOverrides::default();
        for Entry { input, binding } in entries {
            if values.insert(input.node, input.input, binding).is_some() {
                return Err(<D::Error as serde::de::Error>::custom(
                    "duplicate pipeline override input",
                ));
            }
        }
        Ok(values)
    }
}

impl PipelineInstance {
    #[must_use]
    pub fn effect_default() -> Self {
        Self {
            ui_input_position: None,
            ui_output_position: None,
            local_nodes: Vec::new(),
            local_output: ImageBinding::PipelineInput,
            pipeline: None,
            overrides: BindingOverrides::default(),
        }
    }

    #[must_use]
    pub fn transform(&self) -> Option<&EffectNode> {
        self.local_nodes
            .iter()
            .find(|node| node.id == LOCAL_TRANSFORM_NODE_ID)
    }

    pub fn transform_mut(&mut self) -> Option<&mut EffectNode> {
        self.local_nodes
            .iter_mut()
            .find(|node| node.id == LOCAL_TRANSFORM_NODE_ID)
    }
}
