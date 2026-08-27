use std::{
    collections::{HashMap, HashSet, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    ops::{Deref, DerefMut},
    path::Path,
    time::{Duration, Instant},
};

use crate::{
    RADIUS_LG, RADIUS_MD, RADIUS_SM,
    assets::{AppIcon, Icons},
    command::KeyBinding,
    effects::{
        Binding, EasingHandle, EffectNode, GpuValue, ImageBinding, ImageGraphIndex, Interpolation,
        PipelineId, PipelineInstance, PipelineKind, SocketRef,
    },
    gradient::{
        changed_point_index, colors_from_values, colors_to_values, insert_midpoint, inserted_color,
        normalized_midpoints, remove_midpoint,
    },
    i18n,
    plugin::{GeneratorDefinition, PluginRegistry},
    project::{
        CompositionId, GeneratorSource, HostBinding, HostValue, LayerComposite, MediaId, MediaKind,
        Model3dShading, PipelineSelectorRemap, Project, VisualSource,
        remap_pipeline_selector_binding,
    },
    runtime::wasm::DEFAULT_RENDER_EXPORT,
    theme,
    waveform::WaveformTextures,
    widgets::{self, ContextMenuItem},
};
use kama_ui::dock::{LayoutSnapshot, Rect, StackId};
use kama_ui::{self as ui, Align};
use kama_ui::{
    Color, CursorShape, Size,
    components::{Knob, ToggleButton},
};
use serde::{Deserialize, Serialize};
use winit::{
    event::{ElementState, KeyEvent, MouseButton},
    keyboard::{Key, ModifiersState, NamedKey},
};

mod edit_state;
mod interaction;
mod keyframes;
mod view;

pub use edit_state::TimelineEditState;

const HEADER_W: f32 = 230.0;
const TRANSPORT_BUTTON_W: f32 = 22.0;
const TRANSPORT_BUTTON_H: f32 = 20.0;
const TRANSPORT_BUTTON_GAP: f32 = 4.0;
const RULER_H: f32 = 28.0;
const BUILTIN_GRADIENT_GENERATOR: &str = "builtin.gradient";

fn ease_chevron(value: &mut f32, target: f32, dt: f32) {
    const SPEED: f32 = 30.0;
    *value += (target - *value) * (1.0 - (-SPEED * dt).exp());
    if (*value - target).abs() < 0.001 {
        *value = target;
    }
}

fn timeline_icon_toggle(
    ctx: &mut ui::BuildCtx,
    id: &str,
    rect: Rect,
    icon: ui::IconId,
    active: bool,
    tooltip: &str,
) {
    let style = widgets::component_style();
    ToggleButton::build(ctx, id, rect, "", active, style);
    ui::ui!(ctx, {
        Block {
            id: @format("{}-icon", id);
            bounds: (rect.x, rect.y, rect.width, rect.height);
            content_centered;

            Icon {
                id: @format("{}-glyph", id);
                icon!: icon;
                color!: theme::toggle_icon_color(active);
                width: Size::Pixels(16.0);
                height: Size::Pixels(16.0);
            }
        }
        Rect(("timeline-toggle-tooltip", id), rect) {
            interactive;
            tooltip: tooltip;
        }
    });
}

fn sync_gradient_stop_parameters(
    generator: &mut GeneratorSource,
    time: f64,
    previous_points: Option<Vec<[f32; 2]>>,
) {
    let GeneratorSource::Plugin {
        generator_type,
        parameters,
    } = generator
    else {
        return;
    };
    if generator_type != BUILTIN_GRADIENT_GENERATOR {
        return;
    }
    let points = match parameters
        .get("points")
        .and_then(|binding| binding.evaluate(time))
    {
        Some(HostValue::Vec2Array(points)) => points,
        _ => return,
    };
    let raw_colors = match parameters
        .get("colors")
        .and_then(|binding| binding.evaluate(time))
    {
        Some(HostValue::F32List(values)) => values,
        _ => Vec::new(),
    };

    let mut colors = colors_from_values(
        &raw_colors,
        previous_points.as_ref().map_or(points.len(), Vec::len),
    );
    if let Some(previous) = previous_points.as_ref() {
        if points.len() == previous.len() + 1 {
            let index = changed_point_index(previous, &points).min(colors.len());
            let color = inserted_color(&colors, index);
            colors.insert(index, color);
        } else if points.len() + 1 == previous.len() && !colors.is_empty() {
            let index = changed_point_index(previous, &points).min(colors.len().saturating_sub(1));
            colors.remove(index);
        }
    }
    colors = colors_from_values(&colors_to_values(&colors), points.len());

    let values = colors_to_values(&colors);
    parameters
        .entry("colors".into())
        .or_insert_with(|| HostBinding::Constant(HostValue::F32List(values.clone())))
        .set_value(time, HostValue::F32List(values));

    let raw_midpoints = match parameters
        .get("midpoints")
        .and_then(|binding| binding.evaluate(time))
    {
        Some(HostValue::F32List(values)) => values,
        _ => Vec::new(),
    };
    let old_point_count = previous_points.as_ref().map_or(points.len(), Vec::len);
    let mut midpoints = normalized_midpoints(&raw_midpoints, old_point_count);
    if let Some(previous) = previous_points.as_ref() {
        if points.len() == previous.len() + 1 {
            insert_midpoint(
                &mut midpoints,
                changed_point_index(previous, &points),
                previous.len(),
            );
        } else if points.len() + 1 == previous.len() && !midpoints.is_empty() {
            remove_midpoint(
                &mut midpoints,
                changed_point_index(previous, &points),
                previous.len(),
            );
        }
    }
    midpoints = normalized_midpoints(&midpoints, points.len());
    parameters
        .entry("midpoints".into())
        .or_insert_with(|| HostBinding::Constant(HostValue::F32List(midpoints.clone())))
        .set_value(time, HostValue::F32List(midpoints));
}

const OVERVIEW_H: f32 = 44.0;
const OVERVIEW_BATCH_W: f32 = 128.0;
const CLIP_PAD: f32 = 4.0;
const EDGE_W: f32 = 9.0;
const MIN_CLIP: f32 = 0.12;
const SNAP_PX: f32 = 8.0;
const CLIP_DRAG_THRESHOLD_PX: f32 = 3.0;
const KEYFRAME_AXIS_LOCK_RATIO: f32 = 2.0;
const TRACK_MIN: f32 = 48.0;
const TRACK_HANDLE_W: f32 = 18.0;
const TRACK_HEADER_PAD: f32 = 4.0;
const TRACK_HEADER_GAP: f32 = 4.0;
const TRACK_LABEL_W: f32 = 20.0;
const TRACK_NAME_W: f32 = 72.0;
const TRACK_BUTTON_W: f32 = 20.0;
const TRACK_TOP_H: f32 = 20.0;
const TRACK_VU_W: f32 = 68.0;
const KEYFRAME_LANE_H: f32 = 24.0;
const KEYFRAME_CURVE_H: f32 = 112.0;
const KEYFRAME_GRAPH_PAD: f32 = 20.0;
const DOUBLE_CLICK: Duration = Duration::from_millis(360);
const CLIP_SELECTION_FADE_SPEED: f32 = 18.0;
const FRAME_RATE: f32 = 30.0;
const MAX_PIXELS_PER_SECOND: f32 = FRAME_RATE * 40.0;

const AFTER_END: Color = Color::rgba8(0x00, 0x00, 0x00, 0x58);
const PLAYHEAD: Color = Color::rgb8(0xf0, 0x59, 0x47);
const SNAP: Color = Color::rgb8(0x57, 0xb7, 0xe8);
const VIDEO_A: Color = Color::rgb8(0x3c, 0x74, 0x92);
const VIDEO_B: Color = Color::rgb8(0x57, 0x65, 0x9b);
const AUDIO_A: Color = Color::rgb8(0x3e, 0x7c, 0x59);
const AUDIO_B: Color = Color::rgb8(0x68, 0x72, 0x3f);

#[derive(Clone, Copy)]
enum CompositeBindingKind {
    Opacity,
    BlendMode,
    AlphaBlendMode,
}

macro_rules! composite_keyframe_methods {
    ($has:ident, $many:ident, $toggle:ident, $kind:expr) => {
        pub fn $has(&self) -> bool {
            self.composite_has_keyframe($kind)
        }
        pub fn $many(&self) -> bool {
            self.composite_has_keyframes($kind)
        }
        pub fn $toggle(&mut self) {
            self.toggle_composite_keyframe($kind);
        }
    };
}

pub(crate) use kama_editor_core::document::{EndBehavior, TimelineViewState, TrackKind};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum ClipColor {
    VideoA,
    VideoB,
    AudioA,
    AudioB,
    Effect,
}

impl ClipColor {
    fn color(self) -> Color {
        match self {
            Self::VideoA => VIDEO_A,
            Self::VideoB => VIDEO_B,
            Self::AudioA => AUDIO_A,
            Self::AudioB => AUDIO_B,
            Self::Effect => Color::rgb8(0x8a, 0x58, 0x91),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Track {
    pub(crate) id: u32,
    pub(crate) name: String,
    pub(crate) kind: TrackKind,
    pub(crate) height: f32,
    pub(crate) muted: bool,
    pub(crate) solo: bool,
    pub(crate) pipeline: Option<PipelineInstance>,
    pub(crate) composite: LayerComposite,
    pub(crate) volume: Binding,
    pub(crate) pan: Binding,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) property_rows: Vec<LayerPropertyRow>,
}

fn default_track_volume() -> Binding {
    Binding::Constant(GpuValue::F32(1.0))
}
fn default_track_pan() -> Binding {
    Binding::Constant(GpuValue::F32(0.0))
}

impl Track {
    fn shift_owned_keyframes(&mut self, delta: f64) {
        self.composite.opacity.shift_keyframes(delta);
        self.composite.blend_mode.shift_keyframes(delta);
        self.composite.alpha_blend_mode.shift_keyframes(delta);
        self.volume.shift_keyframes(delta);
        self.pan.shift_keyframes(delta);
        if let Some(pipeline) = &mut self.pipeline {
            pipeline.shift_keyframes(delta);
        }
        for row in &mut self.property_rows {
            row.shift_keyframes(delta);
        }
    }

    pub(crate) fn property_row(
        &self,
        source: &VisualSource,
        source_instance: u64,
    ) -> Option<&LayerPropertyRow> {
        self.property_rows
            .iter()
            .find(|row| row.matches_source(source, source_instance))
    }

    pub(crate) fn property_row_mut(
        &mut self,
        source: &VisualSource,
        source_instance: u64,
    ) -> Option<&mut LayerPropertyRow> {
        self.property_rows
            .iter_mut()
            .find(|row| row.matches_source(source, source_instance))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Model3dClipTransform {
    pub(crate) size: Binding,
    pub(crate) position: Binding,
    pub(crate) rotation: Binding,
    pub(crate) scale: Binding,
    #[serde(default)]
    pub(crate) shading: Model3dShading,
}

impl Default for Model3dClipTransform {
    fn default() -> Self {
        Self {
            size: Binding::Constant(GpuValue::Vec3([2.0, 2.0, 2.0])),
            position: Binding::Constant(GpuValue::Vec3([0.0, 0.0, 0.0])),
            rotation: Binding::Constant(GpuValue::Vec3([0.0, 0.0, 0.0])),
            scale: Binding::Constant(GpuValue::Vec3([1.0, 1.0, 1.0])),
            shading: Model3dShading::default(),
        }
    }
}

impl Model3dClipTransform {
    fn binding(&self, input: &str) -> Option<&Binding> {
        match input {
            "size" => Some(&self.size),
            "position" => Some(&self.position),
            "rotation" => Some(&self.rotation),
            "scale" => Some(&self.scale),
            _ => None,
        }
    }

    fn binding_mut(&mut self, input: &str) -> Option<&mut Binding> {
        match input {
            "size" => Some(&mut self.size),
            "position" => Some(&mut self.position),
            "rotation" => Some(&mut self.rotation),
            "scale" => Some(&mut self.scale),
            _ => None,
        }
    }

    pub(crate) fn apply_legacy(
        &mut self,
        size: [f32; 3],
        scale: [f32; 3],
        rotation: [f32; 3],
        shading: Model3dShading,
    ) {
        fn transform_binding(binding: &mut Binding, add: [f32; 3], multiply: [f32; 3]) {
            let transform = |value: GpuValue| match value {
                GpuValue::Vec3(value) => GpuValue::Vec3(std::array::from_fn(|axis| {
                    value[axis] * multiply[axis] + add[axis]
                })),
                other => other,
            };
            match binding {
                Binding::Constant(value) => *value = transform(*value),
                Binding::Keyframes(track) => {
                    for key in &mut track.keys {
                        key.value = transform(key.value);
                    }
                }
                Binding::Components(channels) => {
                    channels.base = transform(channels.base);
                    for (axis, track) in channels.tracks.iter_mut().take(3).enumerate() {
                        for key in &mut track.keys {
                            key.value = key.value * multiply[axis] + add[axis];
                        }
                    }
                }
                Binding::Connection(_) => {}
            }
        }

        self.size = Binding::Constant(GpuValue::Vec3(size));

        transform_binding(&mut self.scale, [0.0; 3], scale);
        transform_binding(&mut self.rotation, rotation, [1.0; 3]);
        self.shading = shading;
    }
}

fn default_clip_opacity() -> f32 {
    1.0
}

fn default_clip_volume() -> f32 {
    1.0
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Clip {
    pub(crate) id: u32,
    pub(crate) track: u32,
    pub(crate) start: f32,
    pub(crate) duration: f32,
    pub(crate) speed: f32,
    pub(crate) source_offset: f32,
    #[serde(default = "default_clip_opacity")]
    pub(crate) opacity: f32,
    #[serde(default = "default_clip_volume")]
    pub(crate) volume: f32,
    pub(crate) fade_in: f32,
    pub(crate) fade_out: f32,
    pub(crate) group: Option<u32>,
    pub(crate) name: String,
    pub(crate) color: ClipColor,
    pub(crate) source: VisualSource,
    #[serde(default)]
    pub(crate) source_instance: u64,
    pub(crate) pipeline: PipelineInstance,
    pub(crate) composite: LayerComposite,
    #[serde(default)]
    pub(crate) model3d: Model3dClipTransform,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct LayerPropertyRow {
    pub(crate) source: VisualSource,
    #[serde(default)]
    pub(crate) source_instance: u64,
    pub(crate) pipeline: PipelineInstance,
    pub(crate) composite: LayerComposite,
    pub(crate) model3d: Model3dClipTransform,
}

#[derive(Clone, Debug, Deserialize)]
struct LegacyMediaPropertyRow {
    track: u32,
    media: MediaId,
    pipeline: PipelineInstance,
    composite: LayerComposite,
    #[serde(default)]
    model3d: Model3dClipTransform,
}

pub(crate) fn source_requires_instance(source: &VisualSource) -> bool {
    matches!(
        source,
        VisualSource::Generator(_) | VisualSource::EffectInput | VisualSource::AudioPlaceholder
    )
}

fn sources_share_property_row(
    left: &VisualSource,
    left_instance: u64,
    right: &VisualSource,
    right_instance: u64,
) -> bool {
    match (left, right) {
        (VisualSource::Media(left), VisualSource::Media(right))
        | (VisualSource::Media(left), VisualSource::Audio(right))
        | (VisualSource::Audio(left), VisualSource::Media(right))
        | (VisualSource::Audio(left), VisualSource::Audio(right)) => left == right,
        (VisualSource::Composition(left), VisualSource::Composition(right)) => left == right,
        (VisualSource::Generator(_), VisualSource::Generator(_))
        | (VisualSource::EffectInput, VisualSource::EffectInput)
        | (VisualSource::AudioPlaceholder, VisualSource::AudioPlaceholder) => {
            left_instance != 0 && left_instance == right_instance
        }
        _ => false,
    }
}

impl LayerPropertyRow {
    pub(crate) fn matches_source(&self, source: &VisualSource, source_instance: u64) -> bool {
        sources_share_property_row(&self.source, self.source_instance, source, source_instance)
    }

    pub(crate) fn matches(&self, clip: &Clip) -> bool {
        self.matches_source(&clip.source, clip.source_instance)
    }

    fn shift_keyframes(&mut self, delta: f64) {
        self.composite.opacity.shift_keyframes(delta);
        self.composite.blend_mode.shift_keyframes(delta);
        self.composite.alpha_blend_mode.shift_keyframes(delta);
        self.model3d.size.shift_keyframes(delta);
        self.model3d.position.shift_keyframes(delta);
        self.model3d.rotation.shift_keyframes(delta);
        self.model3d.scale.shift_keyframes(delta);
        self.pipeline.shift_keyframes(delta);
        if let VisualSource::Generator(source) = &mut self.source {
            for binding in source.parameters_mut().values_mut() {
                binding.shift_keyframes(delta);
            }
        }
    }
}

fn merge_keys<T: Clone>(
    target: &mut Vec<crate::effects::AnimatedKey<T>>,
    source: &[crate::effects::AnimatedKey<T>],
) {
    for key in source {
        if let Some(existing) = target.iter_mut().find(|item| item.time == key.time) {
            *existing = key.clone();
        } else {
            target.push(key.clone());
        }
    }
    target.sort_by(|left, right| left.time.total_cmp(&right.time));
}

fn merge_binding_animation(target: &mut Binding, source: &Binding) {
    if !source.has_keyframes() {
        return;
    }
    match (target, source) {
        (Binding::Keyframes(target), Binding::Keyframes(source)) => {
            merge_keys(&mut target.keys, &source.keys);
        }
        (Binding::Components(target), Binding::Components(source)) => {
            target.base = source.base;
            target
                .tracks
                .resize_with(source.tracks.len(), Default::default);
            for (target, source) in target.tracks.iter_mut().zip(&source.tracks) {
                merge_keys(&mut target.keys, &source.keys);
            }
        }
        (target, source) => *target = source.clone(),
    }
}

fn merge_host_animation(target: &mut HostBinding, source: &HostBinding) {
    if !source.has_keyframes() {
        return;
    }
    match (target, source) {
        (HostBinding::Gpu(target), HostBinding::Gpu(source)) => {
            merge_binding_animation(target, source)
        }
        (HostBinding::Keyframes(target), HostBinding::Keyframes(source)) => {
            for key in &source.keys {
                if let Some(existing) = target.keys.iter_mut().find(|item| item.time == key.time) {
                    *existing = key.clone();
                } else {
                    target.keys.push(key.clone());
                }
            }
            target
                .keys
                .sort_by(|left, right| left.time.total_cmp(&right.time));
        }
        (HostBinding::Components(target), HostBinding::Components(source)) => {
            target.base = source.base.clone();
            target
                .tracks
                .resize_with(source.tracks.len(), Default::default);
            for (target, source) in target.tracks.iter_mut().zip(&source.tracks) {
                merge_keys(&mut target.keys, &source.keys);
            }
        }
        (target, source) => *target = source.clone(),
    }
}

fn merge_source_animation(target: &mut VisualSource, source: &VisualSource) {
    let (VisualSource::Generator(target), VisualSource::Generator(source)) = (target, source)
    else {
        return;
    };
    for (input, source_binding) in source.parameters() {
        if let Some(target_binding) = target.parameters_mut().get_mut(input) {
            merge_host_animation(target_binding, source_binding);
        } else if source_binding.has_keyframes() {
            target
                .parameters_mut()
                .insert(input.clone(), source_binding.clone());
        }
    }
}

fn merge_pipeline_animation(target: &mut PipelineInstance, source: &PipelineInstance) {
    for source_node in &source.local_nodes {
        let Some(target_node) = target
            .local_nodes
            .iter_mut()
            .find(|node| node.id == source_node.id)
        else {
            continue;
        };
        for (input, source) in &source_node.inputs {
            if let Some(target) = target_node.inputs.get_mut(input) {
                merge_binding_animation(target, source);
            }
        }
        for (input, source) in &source_node.host_inputs {
            if let Some(target) = target_node.host_inputs.get_mut(input) {
                merge_host_animation(target, source);
            }
        }
    }
    for (node, input, source) in source.overrides.iter() {
        if let Some(target) = target.overrides.get_mut(node, input) {
            merge_binding_animation(target, source);
        } else if source.has_keyframes() {
            target.overrides.insert(node, input, source.clone());
        }
    }
}

fn merge_property_row_animation(target: &mut LayerPropertyRow, source: &LayerPropertyRow) {
    merge_binding_animation(&mut target.composite.opacity, &source.composite.opacity);
    merge_binding_animation(
        &mut target.composite.blend_mode,
        &source.composite.blend_mode,
    );
    merge_binding_animation(
        &mut target.composite.alpha_blend_mode,
        &source.composite.alpha_blend_mode,
    );
    merge_binding_animation(&mut target.model3d.size, &source.model3d.size);
    merge_binding_animation(&mut target.model3d.position, &source.model3d.position);
    merge_binding_animation(&mut target.model3d.rotation, &source.model3d.rotation);
    merge_binding_animation(&mut target.model3d.scale, &source.model3d.scale);
    merge_pipeline_animation(&mut target.pipeline, &source.pipeline);
    merge_source_animation(&mut target.source, &source.source);
}

#[derive(Clone, Debug)]
struct ClipboardClip {
    clip: Clip,
    properties: LayerPropertyRow,

    track_rank: usize,
    track_kind: TrackKind,
}

impl Clip {
    pub(crate) fn has_owned_keyframes(&self) -> bool {
        self.composite.opacity.has_keyframes()
            || self.composite.blend_mode.has_keyframes()
            || self.composite.alpha_blend_mode.has_keyframes()
            || self.model3d.size.has_keyframes()
            || self.model3d.position.has_keyframes()
            || self.model3d.rotation.has_keyframes()
            || self.model3d.scale.has_keyframes()
            || self.pipeline.local_nodes.iter().any(|node| {
                node.inputs.values().any(Binding::has_keyframes)
                    || node.host_inputs.values().any(HostBinding::has_keyframes)
            })
            || self
                .pipeline
                .overrides
                .iter()
                .any(|(_, _, binding)| binding.has_keyframes())
            || match &self.source {
                VisualSource::Generator(source) => {
                    source.parameters().values().any(HostBinding::has_keyframes)
                }
                _ => false,
            }
    }

    fn clear_owned_keyframes(&mut self) {
        fn clear(binding: &mut Binding, time: f64) {
            if binding.has_keyframes() {
                if let Some(value) = binding.evaluate(time) {
                    *binding = Binding::Constant(value);
                }
            }
        }
        let time = self.start as f64;
        clear(&mut self.composite.opacity, time);
        clear(&mut self.composite.blend_mode, time);
        clear(&mut self.composite.alpha_blend_mode, time);
        clear(&mut self.model3d.size, time);
        clear(&mut self.model3d.position, time);
        clear(&mut self.model3d.rotation, time);
        clear(&mut self.model3d.scale, time);
        for node in &mut self.pipeline.local_nodes {
            for binding in node.inputs.values_mut() {
                clear(binding, time);
            }
            for binding in node.host_inputs.values_mut() {
                match binding {
                    HostBinding::Gpu(binding) => clear(binding, time),
                    binding if binding.has_keyframes() => {
                        if let Some(value) = binding.evaluate(time) {
                            *binding = HostBinding::Constant(value);
                        }
                    }
                    _ => {}
                }
            }
        }
        self.pipeline.overrides.retain(|_, _, binding| {
            clear(binding, time);
            true
        });
        if let VisualSource::Generator(source) = &mut self.source {
            for binding in source.parameters_mut().values_mut() {
                match binding {
                    HostBinding::Gpu(binding) => clear(binding, time),
                    binding if binding.has_keyframes() => {
                        if let Some(value) = binding.evaluate(time) {
                            *binding = HostBinding::Constant(value);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    pub(crate) fn end(&self) -> f32 {
        self.start + self.duration
    }

    pub(crate) fn track_index(&self, tracks: &[Track]) -> Option<usize> {
        tracks.iter().position(|track| track.id == self.track)
    }

    pub(crate) fn timeline_local_time(&self, timeline_time: f32) -> f32 {
        (timeline_time - self.start).max(0.0)
    }

    pub(crate) fn local_time(&self, timeline_time: f32) -> f64 {
        self.timeline_local_time(timeline_time) as f64
    }

    pub(crate) fn source_time(&self, timeline_time: f32) -> f64 {
        self.source_offset as f64
            + self.timeline_local_time(timeline_time) as f64 * self.speed.max(0.01) as f64
    }

    pub(crate) fn looped_source_time(&self, timeline_time: f32, project: &Project) -> f64 {
        let source_time = self.source_time(timeline_time);
        let Some(duration) = clip_source_duration(project, &self.source) else {
            return source_time;
        };
        source_time.rem_euclid(duration as f64)
    }

    fn set_speed_preserving_source_span(&mut self, speed: f32) {
        let speed = speed.clamp(0.01, 100.0);
        let source_span = self.duration.max(MIN_CLIP) * self.speed.max(0.01);
        self.speed = speed;
        self.duration = (source_span / speed).max(MIN_CLIP);
        self.fade_in = self.fade_in.min(self.duration);
        self.fade_out = self.fade_out.min(self.duration);
    }
}

pub(crate) fn clip_source_duration(project: &Project, source: &VisualSource) -> Option<f32> {
    match source {
        VisualSource::Media(id) | VisualSource::Audio(id) => project
            .media(*id)
            .and_then(|asset| asset.duration)
            .map(|duration| duration.max(1.0e-6) as f32),
        VisualSource::Composition(id) => project.composition_duration(*id).map(|d| d.max(1.0e-6)),
        _ => None,
    }
}

#[derive(Clone, Copy)]
struct ClipWaveformSegment {
    texture: kama_ui::TextureId,
    rect: Rect,
    uv: [f32; 4],
    mode: u32,
}

fn clip_waveforms(
    clip: &Clip,
    clip_rect: Rect,
    project: &Project,
    textures: &WaveformTextures,
) -> Vec<ClipWaveformSegment> {
    let (media, audio) = match &clip.source {
        VisualSource::Media(media) => (*media, false),
        VisualSource::Audio(media) => (*media, true),
        _ => return Vec::new(),
    };
    let Some(duration) = clip_source_duration(project, &clip.source) else {
        return Vec::new();
    };
    let Some(texture) = textures.get(media) else {
        return Vec::new();
    };
    let Some(row) = (if audio {
        texture.audio_y
    } else {
        texture.video_y
    }) else {
        return Vec::new();
    };
    if texture.sample_count == 0 {
        return Vec::new();
    }

    let speed = clip.speed.max(0.01);
    let source_span = clip.duration.max(0.0) * speed;
    if source_span <= 1.0e-6 {
        return Vec::new();
    }
    let source_start = clip.source_offset.rem_euclid(duration);
    let cycle_count = ((source_start + source_span) / duration).ceil().max(1.0) as usize;
    let mut waveforms = Vec::new();
    for cycle in 0..cycle_count {
        let cycle_base = cycle as f32 * duration - source_start;
        for segment in &texture.segments {
            let segment_start =
                segment.sample_start as f32 / texture.sample_count as f32 * duration;
            let segment_end = segment.sample_end as f32 / texture.sample_count as f32 * duration;
            let overlap_start = 0.0f32.max(cycle_base + segment_start);
            let overlap_end = source_span.min(cycle_base + segment_end);
            if overlap_end <= overlap_start {
                continue;
            }
            let local_segment_start = overlap_start - cycle_base;
            let local_segment_end = overlap_end - cycle_base;
            let segment_span = (segment_end - segment_start).max(1.0e-6);
            let u0 = (local_segment_start - segment_start) / segment_span;
            let u1 = (local_segment_end - segment_start) / segment_span;
            let x0 = overlap_start / source_span * clip_rect.width;
            let x1 = overlap_end / source_span * clip_rect.width;
            waveforms.push(ClipWaveformSegment {
                texture: segment.texture,
                rect: Rect::new(x0, 0.0, (x1 - x0).max(0.5), clip_rect.height),
                uv: [u0, row[0], u1, row[1]],
                mode: if audio { 2 } else { 1 },
            });
        }
    }
    waveforms
}

#[derive(Clone, Copy, Debug)]
struct ClipOrigin {
    index: usize,
    id: u32,
    start: f32,
    duration: f32,
    track: u32,
    source_offset: f32,
    opacity: f32,
    volume: f32,
}

#[derive(Clone, Copy, Debug)]
struct ClipEdgeOrigin {
    start: f32,
    duration: f32,
    source_offset: f32,
    speed: f32,
}

#[derive(Clone, Copy, Debug)]
struct ClipMoveAnchor {
    track: u32,
    time: f32,
    pointer: [f32; 2],
}

#[derive(Clone, Copy, Debug)]
struct DuplicatePlacement {
    id: u32,
    start: f32,
    track: u32,
}

#[derive(Default)]
struct TrackIntervals(HashMap<u32, Vec<(f32, f32)>>);

impl TrackIntervals {
    fn from_clips(clips: &[Clip], excluded: &HashSet<u32>) -> Self {
        let mut tracks = HashMap::<u32, Vec<(f32, f32)>>::new();
        for clip in clips.iter().filter(|clip| !excluded.contains(&clip.id)) {
            tracks
                .entry(clip.track)
                .or_default()
                .push((clip.start, clip.end()));
        }
        tracks
            .values_mut()
            .for_each(|intervals| intervals.sort_by(|a, b| a.0.total_cmp(&b.0)));
        Self(tracks)
    }

    fn insert(&mut self, track: u32, start: f32, end: f32) {
        let intervals = self.0.entry(track).or_default();
        let index = intervals.partition_point(|interval| interval.0 < start);
        intervals.insert(index, (start, end));
    }

    fn has_space(&self, track: u32, start: f32, end: f32) -> bool {
        !self.0.get(&track).is_some_and(|intervals| {
            intervals[..intervals.partition_point(|interval| interval.0 < end)]
                .iter()
                .any(|&(other_start, other_end)| {
                    intervals_overlap(start, end, other_start, other_end)
                })
        })
    }
}

fn intervals_overlap(start: f32, end: f32, other_start: f32, other_end: f32) -> bool {
    let magnitude = start
        .abs()
        .max(end.abs())
        .max(other_start.abs())
        .max(other_end.abs())
        .max(1.0);
    let tolerance = (magnitude * f32::EPSILON * 4.0).clamp(1.0e-6, 1.0e-3);
    start < other_end - tolerance && end > other_start + tolerance
}

#[derive(Debug)]
struct PowerDuplicateState {
    source: Vec<DuplicatePlacement>,
    duplicates: Vec<u32>,
}

#[derive(Clone, Copy, Debug)]
enum OverviewPart {
    Left,
    Body,
    Right,
}

#[derive(Clone, Copy, Debug)]
enum JumpTarget {
    TimelineStart,
    ContentStart,
    ContentEnd,
    TimelineEnd,
}

#[derive(Clone, Copy, Debug)]
enum ContextKind {
    Selection,
    Empty {
        time: f32,
        track: Option<usize>,
        kind: Option<TrackKind>,
    },
    Track {
        id: u32,
        kind: TrackKind,
    },
    Mixer {
        track: u32,
        parameter: MixerParameter,
    },
    Keyframe,
}

#[derive(Clone, Copy, Debug)]
enum ContextCommand {
    CopySelection,
    CutSelection,
    Paste,
    Group,
    Ungroup,
    CloseGap,
    SpeedDuration,
    ReplaceSelectedClips,
    DeleteSelection,
    SetEnd,
    InsertVideoHere,
    InsertVideoFirst,
    InsertAudio,
    InsertEffectHere,
    RenameTrack,
    DeleteTrack,
    AddTrack(TrackKind),
    SetExactMixer,
    ToggleMixerKeyframe,
    EditKeyframeValue,
    SetKeyframeInterpolation(Interpolation),
    DeleteKeyframes,
    AddSelectionToComposition,
}

type ContextSpec = (
    &'static str,
    Option<KeyBinding>,
    Option<AppIcon>,
    ContextCommand,
);

type ContextItem = ContextMenuItem<'static, ContextCommand>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TimelineTool {
    Select,
    Razor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpeedDurationMode {
    SpeedPercent,
    PerClipDuration,
    TotalDuration,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum TimelineAction {
    InsertVideoClip {
        track: u32,
        time: f32,
    },
    InsertEffectClip {
        track: u32,
        time: f32,
    },
    CopySelection,
    CutSelection,
    Paste,
    PowerDuplicate,
    DeleteSelection,
    SelectBeforePlayhead,
    SelectAfterPlayhead,
    GroupSelection,
    UngroupSelection,
    CloseGap,
    SpeedDuration,
    ReplaceSelectedClips,
    ToggleRazorTool,
    CutAtPlayhead,
    CutClipAt {
        clip: u32,
        time: f32,
    },
    TogglePlayback,
    SeekBy(f32),
    StepFrames(i32),
    JumpTimelineStart,
    JumpContentStart,
    JumpContentEnd,
    JumpTimelineEnd,
    SetEnd,
    InsertAudio {
        time: f32,
        near: Option<usize>,
    },
    RenameTrack(u32),
    DeleteTrack(u32),
    AddTrack {
        kind: TrackKind,
        near: Option<usize>,
    },
    BeginMixerExact {
        point: [f32; 2],
        track: u32,
        parameter: MixerParameter,
    },
    ToggleMixerKeyframe {
        track: u32,
        parameter: MixerParameter,
    },
    ToggleEndBehavior,
    ToggleFrameSnap,
    ToggleGridSnap,
    ToggleClipSnap,
    TogglePlayheadSnap,
    ToggleFollowPlayhead,
    AddSelectionToComposition,
    ToggleTrackMute(u32),
    ToggleTrackSolo(u32),
}

impl TimelineAction {
    pub(crate) fn history_label(self) -> &'static str {
        match self {
            Self::InsertVideoClip { .. } => "Insert video clip",
            Self::InsertEffectClip { .. } => "Insert effect clip",
            Self::CopySelection => "Copy clips",
            Self::CutSelection => "Cut clips",
            Self::Paste => "Paste clips",
            Self::PowerDuplicate => "Power duplicate clips",
            Self::DeleteSelection => "Delete clips",
            Self::SelectBeforePlayhead | Self::SelectAfterPlayhead => "Select clips",
            Self::GroupSelection => "Group clips",
            Self::UngroupSelection => "Ungroup clips",
            Self::CloseGap => "Close timeline gap",
            Self::SpeedDuration => "Change clip speed/duration",
            Self::ReplaceSelectedClips => "Replace clip source",
            Self::ToggleRazorTool => "Toggle razor tool",
            Self::CutAtPlayhead | Self::CutClipAt { .. } => "Split clip",
            Self::TogglePlayback
            | Self::SeekBy(_)
            | Self::StepFrames(_)
            | Self::JumpTimelineStart
            | Self::JumpContentStart
            | Self::JumpContentEnd
            | Self::JumpTimelineEnd => "Move playhead",
            Self::SetEnd => "Set timeline end",
            Self::InsertAudio { .. } => "Insert audio clip",
            Self::RenameTrack(_) => "Rename track",
            Self::DeleteTrack(_) => "Delete track",
            Self::AddTrack { .. } => "Add track",
            Self::BeginMixerExact { parameter, .. }
            | Self::ToggleMixerKeyframe { parameter, .. } => match parameter {
                MixerParameter::Volume => "Edit track volume",
                MixerParameter::Pan => "Edit track pan",
            },
            Self::ToggleEndBehavior => "Change timeline end behavior",
            Self::ToggleFrameSnap => "Toggle frame snapping",
            Self::ToggleGridSnap => "Toggle grid snapping",
            Self::ToggleClipSnap => "Toggle clip snapping",
            Self::TogglePlayheadSnap => "Toggle playhead snapping",
            Self::ToggleFollowPlayhead => "Toggle follow playhead",
            Self::AddSelectionToComposition => "Create composition from clips",
            Self::ToggleTrackMute(_) => "Toggle track mute",
            Self::ToggleTrackSolo(_) => "Toggle track solo",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ContextMenu {
    stack: StackId,
    point: [f32; 2],
    kind: ContextKind,
}

#[derive(Debug)]
struct MixerExactEditor {
    stack: StackId,
    point: [f32; 2],
    track: u32,
    parameter: MixerParameter,
    value: String,
    replace_on_input: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum MixerParameter {
    Volume,
    Pan,
}

impl MixerParameter {
    fn limits(self) -> (f32, f32, f32) {
        match self {
            Self::Volume => (0.0, 1.0, 1.0),
            Self::Pan => (-1.0, 1.0, 0.0),
        }
    }
}

fn mixer_knobs(tracks: &[Track]) -> HashMap<(u32, MixerParameter), Knob> {
    let mut knobs = HashMap::new();
    for track in tracks.iter().filter(|track| track.kind == TrackKind::Audio) {
        let volume = track
            .volume
            .evaluate(0.0)
            .and_then(GpuValue::f32)
            .unwrap_or(1.0);
        let pan = track
            .pan
            .evaluate(0.0)
            .and_then(GpuValue::f32)
            .unwrap_or(0.0);
        knobs.insert(
            (track.id, MixerParameter::Volume),
            Knob::new(0.0, 1.0, 1.0)
                .step(0.01)
                .precision(0)
                .sensitivity(0.01)
                .formatter(|value, _| format!("{:.0}%", value * 100.0)),
        );
        knobs.insert(
            (track.id, MixerParameter::Pan),
            Knob::new(-1.0, 1.0, 0.0)
                .step(0.01)
                .precision(0)
                .sensitivity(0.01)
                .formatter(|value, _| format!("{:+.0}%", value * 100.0)),
        );
        if let Some(knob) = knobs.get_mut(&(track.id, MixerParameter::Volume)) {
            knob.set_value(volume as f64);
        }
        if let Some(knob) = knobs.get_mut(&(track.id, MixerParameter::Pan)) {
            knob.set_value(pan as f64);
        }
    }
    knobs
}

#[derive(Debug)]
struct RenameState {
    track: u32,
    value: String,
}

trait ScalarKeyframeBinding {
    fn scalar_count(&self) -> usize;
    fn scalar_keys(&self, component: usize) -> Vec<crate::effects::ScalarKeyframe>;
    fn edit_scalar_key(
        &mut self,
        component: usize,
        time: f64,
        next_time: Option<f64>,
        next_value: Option<f32>,
        interpolation: Option<Interpolation>,
    ) -> bool;
    fn edit_scalar_key_easing(
        &mut self,
        component: usize,
        time: f64,
        incoming: bool,
        handle: EasingHandle,
    ) -> bool;
    fn remove_scalar_key(&mut self, component: usize, time: f64) -> bool;
}

impl ScalarKeyframeBinding for Binding {
    fn scalar_count(&self) -> usize {
        self.evaluate(0.0)
            .map(GpuValue::component_count)
            .unwrap_or(0)
    }

    fn scalar_keys(&self, component: usize) -> Vec<crate::effects::ScalarKeyframe> {
        Binding::scalar_keys(self, component)
    }

    fn edit_scalar_key(
        &mut self,
        component: usize,
        time: f64,
        next_time: Option<f64>,
        next_value: Option<f32>,
        interpolation: Option<Interpolation>,
    ) -> bool {
        Binding::edit_scalar_key(self, component, time, next_time, next_value, interpolation)
    }

    fn edit_scalar_key_easing(
        &mut self,
        component: usize,
        time: f64,
        incoming: bool,
        handle: EasingHandle,
    ) -> bool {
        Binding::edit_scalar_key_easing(self, component, time, incoming, handle)
    }

    fn remove_scalar_key(&mut self, component: usize, time: f64) -> bool {
        Binding::remove_scalar_key(self, component, time)
    }
}

impl ScalarKeyframeBinding for HostBinding {
    fn scalar_count(&self) -> usize {
        HostBinding::scalar_count(self)
    }

    fn scalar_keys(&self, component: usize) -> Vec<crate::effects::ScalarKeyframe> {
        HostBinding::scalar_keys(self, component)
    }

    fn edit_scalar_key(
        &mut self,
        component: usize,
        time: f64,
        next_time: Option<f64>,
        next_value: Option<f32>,
        interpolation: Option<Interpolation>,
    ) -> bool {
        HostBinding::edit_scalar_key(self, component, time, next_time, next_value, interpolation)
    }

    fn edit_scalar_key_easing(
        &mut self,
        component: usize,
        time: f64,
        incoming: bool,
        handle: EasingHandle,
    ) -> bool {
        HostBinding::edit_scalar_key_easing(self, component, time, incoming, handle)
    }

    fn remove_scalar_key(&mut self, component: usize, time: f64) -> bool {
        HostBinding::remove_scalar_key(self, component, time)
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum KeyframeOwner {
    Track(u32),
    SourceRow { track: u32, row: usize },
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum KeyframeProperty {
    Opacity,
    BlendMode,
    AlphaBlendMode,
    Volume,
    Pan,
    Local { node: u64, input: String },
    LocalHost { node: u64, input: String },
    Override { node: u64, input: String },
    Generator(String),
    GeneratorHost(String),
    Model3d(String),
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct KeyframeBindingTarget {
    owner: KeyframeOwner,
    property: KeyframeProperty,
}

impl KeyframeBindingTarget {
    fn new(owner: KeyframeOwner, property: KeyframeProperty) -> Self {
        Self { owner, property }
    }

    fn is_host(&self) -> bool {
        matches!(
            self.property,
            KeyframeProperty::LocalHost { .. } | KeyframeProperty::GeneratorHost(_)
        )
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum KeyframeGroupOwner {
    Target(KeyframeOwner),
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct KeyframeLaneGroup {
    owner: KeyframeGroupOwner,
    property: KeyframeProperty,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct KeyframeLaneId {
    target: KeyframeBindingTarget,
    group: KeyframeLaneGroup,
    component: usize,
}

#[derive(Clone, Copy, Debug)]
struct KeyframeLanePoint {
    time: f64,
    value: f64,
    interpolation: crate::effects::Interpolation,
    ease_in: EasingHandle,
    ease_out: EasingHandle,
    custom_ease_in: bool,
    custom_ease_out: bool,
}

#[derive(Clone, Debug)]
struct KeyframeLane {
    id: KeyframeLaneId,
    label: String,
    value_range: (f64, f64),
    points: Vec<KeyframeLanePoint>,
}

#[derive(Clone, Debug)]
struct SelectedKeyframe {
    lane: KeyframeLaneId,
    time: f64,
}

#[derive(Clone, Debug)]
enum TimelineHoverTarget {
    Keyframe(KeyframeLaneId, f64),
    Clip(u32),
}

#[derive(Debug)]
struct KeyframeValueEditor {
    stack: StackId,
    point: [f32; 2],
    value: String,
    replace_on_input: bool,
}

#[derive(Clone, Debug)]
struct KeyframeDragPoint {
    lane: KeyframeLaneId,
    origin_time: f64,
    current_time: f64,
    origin_value: f32,
    value_per_pixel: f32,
    vertical: bool,
}

#[derive(Clone, Debug)]
enum KeyframeEaseDragKind {
    Control {
        lane: KeyframeLaneId,
        key_time: f64,
        incoming: bool,
    },
    Midpoint {
        lane: KeyframeLaneId,
        left_time: f64,
        right_time: f64,
    },
}

#[derive(Clone, Debug)]
struct KeyframeEaseDrag {
    kind: KeyframeEaseDragKind,
    rect: Rect,
    axis_range: (f64, f64),
    left_time: f64,
    right_time: f64,
    left_value: f64,
    right_value: f64,
}

#[derive(Clone, Copy, Debug)]
enum ShiftClipAdjustAxis {
    Pending,
    Horizontal,
    Vertical,
}

#[derive(Debug)]
enum Drag {
    Clips {
        anchor_track: u32,
        anchor_start: f32,
        start: [f32; 2],
        origins: Vec<ClipOrigin>,
        snap_points: Vec<f32>,
        keyframes: Vec<KeyframeDragPoint>,
        shift_adjust: Option<ShiftClipAdjustAxis>,
        shift_adjust_audio: bool,
        shift_adjust_anchor: u32,
        duplicated: bool,
        shift_toggle_on_click: Option<u32>,

        collapse_selection_on_click: Option<u32>,

        preview_tracks: Vec<u32>,
    },
    ClipEdge {
        id: u32,
        left: bool,
        rate_stretch: bool,
        origin: ClipEdgeOrigin,
    },
    Playhead,
    BoxSelect {
        start: [f32; 2],
        current: [f32; 2],
        additive: bool,
        stack: StackId,
        hit: Option<u32>,
    },
    Pan {
        start: [f32; 2],
        scroll_time: f64,
        scroll_y: f32,
    },
    Overview {
        part: OverviewPart,
        start_x: f32,
        scroll_time: f64,
        pixels_per_second: f32,
    },
    Keyframe {
        points: Vec<KeyframeDragPoint>,
        start: [f32; 2],
    },
    KeyframeEase(KeyframeEaseDrag),
    Track {
        id: u32,
        grab_y: f32,
        current_y: f32,
        origin_y: f32,
        heights: HashMap<u32, f32>,
    },
}

#[derive(Clone, Copy)]
struct TimelineLayout {
    rect: Rect,
    body: Rect,
    header_body: Rect,
    corner: Rect,
    ruler: Rect,
    overview: Rect,
    overview_header: Rect,
    overview_body: Rect,
    frame_snap_button: Rect,
    grid_snap_button: Rect,
    clip_snap_button: Rect,
    playhead_snap_button: Rect,
    tool_separator: Rect,
    razor_tool_button: Rect,
    follow_playhead_button: Rect,
}

impl TimelineLayout {
    fn new(rect: Rect) -> Self {
        let overview_h = OVERVIEW_H.min((rect.height - RULER_H).max(0.0));
        let vertical = crate::ui_layout::column(
            rect,
            &[
                crate::ui_layout::Item::height(RULER_H),
                crate::ui_layout::Item::fill(),
                crate::ui_layout::Item::height(overview_h),
            ],
            0.0,
            0.0,
            ui::Align::Start,
            None,
        );
        let corner = crate::ui_layout::row(
            vertical[0],
            &[
                crate::ui_layout::Item::width(HEADER_W.min(rect.width)),
                crate::ui_layout::Item::fill(),
            ],
            0.0,
            0.0,
            ui::Align::Start,
        )[0];
        let top = crate::ui_layout::row(
            vertical[0],
            &[
                crate::ui_layout::Item::width(HEADER_W),
                crate::ui_layout::Item::fill(),
            ],
            0.0,
            0.0,
            ui::Align::Start,
        );
        let middle = crate::ui_layout::row(
            vertical[1],
            &[
                crate::ui_layout::Item::width(HEADER_W),
                crate::ui_layout::Item::fill(),
            ],
            0.0,
            0.0,
            ui::Align::Start,
        );
        let overview_parts = crate::ui_layout::row(
            vertical[2],
            &[
                crate::ui_layout::Item::width(HEADER_W),
                crate::ui_layout::Item::fill(),
            ],
            0.0,
            0.0,
            ui::Align::Start,
        );
        let tools = crate::ui_layout::row(
            vertical[2],
            &[
                crate::ui_layout::Item::width(6.0),
                crate::ui_layout::Item::new(Size::Pixels(20.0), Size::Pixels(20.0)),
                crate::ui_layout::Item::width(4.0),
                crate::ui_layout::Item::new(Size::Pixels(20.0), Size::Pixels(20.0)),
                crate::ui_layout::Item::width(4.0),
                crate::ui_layout::Item::new(Size::Pixels(20.0), Size::Pixels(20.0)),
                crate::ui_layout::Item::width(4.0),
                crate::ui_layout::Item::new(Size::Pixels(20.0), Size::Pixels(20.0)),
                crate::ui_layout::Item::width(4.0),
                crate::ui_layout::Item::new(Size::Pixels(20.0), Size::Pixels(20.0)),
                crate::ui_layout::Item::width(8.0),
                crate::ui_layout::Item::new(Size::Pixels(1.0), Size::Pixels(16.0)),
                crate::ui_layout::Item::width(7.0),
                crate::ui_layout::Item::new(Size::Pixels(20.0), Size::Pixels(20.0)),
                crate::ui_layout::Item::fill(),
            ],
            0.0,
            0.0,
            ui::Align::Center,
        );
        Self {
            rect,
            body: middle[1],
            header_body: middle[0],
            corner,
            ruler: top[1],
            overview: vertical[2],
            overview_header: overview_parts[0],
            overview_body: overview_parts[1],
            frame_snap_button: tools[1],
            grid_snap_button: tools[3],
            clip_snap_button: tools[5],
            playhead_snap_button: tools[7],
            follow_playhead_button: tools[9],
            tool_separator: tools[11],
            razor_tool_button: tools[13],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RenderCacheState {
    Rendered,
    Dirty,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct RenderCacheRange {
    pub(crate) start: f32,
    pub(crate) end: f32,
    pub(crate) state: RenderCacheState,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct MediaDropPreviewSpec {
    pub(crate) video_tracks: usize,
    pub(crate) audio_tracks: usize,
    pub(crate) duration: f32,
}

#[derive(Debug)]
pub struct TimelineState {
    edit: TimelineEditState,
    selection_levels: HashMap<u32, f32>,
    audio_levels: HashMap<u32, [f32; 2]>,
    mixer_knobs: HashMap<(u32, MixerParameter), Knob>,
    selection_frame: Instant,
    playing: bool,
    playback_just_started: bool,
    tool: TimelineTool,
    frame_rate: f32,
    drag: Option<Drag>,
    snap_times: Vec<f32>,
    focused_stack: Option<StackId>,
    context_menu: Option<ContextMenu>,
    rename: Option<RenameState>,
    mixer_exact: Option<MixerExactEditor>,
    keyframe_value_editor: Option<KeyframeValueEditor>,
    selected_keyframes: Vec<SelectedKeyframe>,
    last_track_click: Option<(u32, Instant)>,
    track_offsets: HashMap<u32, f32>,
    cursor: [f32; 2],
    selected_track: Option<u32>,
    pending_action: Option<TimelineAction>,
    power_duplicate: Option<PowerDuplicateState>,
    render_cache_ranges: Vec<RenderCacheRange>,
    render_output_range: Option<(f32, f32)>,
    expanded_keyframe_tracks: HashSet<u32>,

    expanded_keyframe_lanes: HashSet<KeyframeLaneGroup>,
    keyframe_track_expansion: HashMap<u32, f32>,
    keyframe_lane_expansion: HashMap<KeyframeLaneGroup, f32>,

    keyframe_lane_snapshot: Vec<Vec<KeyframeLane>>,
    keyframe_row_heights: HashMap<u32, f32>,

    track_prefix_heights: Vec<f32>,
}

default_state! {
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct TimelineDocument {
        pub(crate) tracks: Vec<Track> = vec![
            Track {
                id: 1,
                name: "Video 1".into(),
                kind: TrackKind::Video,
                height: 62.0,
                muted: false,
                solo: false,
                pipeline: None,
                composite: LayerComposite::default(),
                volume: default_track_volume(),
                pan: default_track_pan(),
                property_rows: Vec::new(),
            },
            Track {
                id: 2,
                name: "Audio 1".into(),
                kind: TrackKind::Audio,
                height: 56.0,
                muted: false,
                solo: false,
                pipeline: None,
                composite: LayerComposite::default(),
                volume: default_track_volume(),
                pan: default_track_pan(),
                property_rows: Vec::new(),
            },
        ],
        pub(crate) clips: Vec<Clip>,
        #[serde(default, rename = "media_property_rows", skip_serializing)]
        legacy_media_property_rows: Vec<LegacyMediaPropertyRow> = Vec::new(),
        pub(crate) end_time: Option<f32>,
        pub(crate) end_behavior: EndBehavior = EndBehavior::Stop,
        pub(crate) next_group: u32 = 1,
        pub(crate) next_track: u32 = 3,
        pub(crate) next_clip: u32 = 1,
        #[serde(default)]
        pub(crate) next_source_instance: u64 = 1,
        #[serde(default)]
        pub(crate) view: TimelineViewState,
    }
}

impl Deref for TimelineDocument {
    type Target = TimelineViewState;

    fn deref(&self) -> &Self::Target {
        &self.view
    }
}

impl DerefMut for TimelineDocument {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.view
    }
}

impl TimelineDocument {
    pub(crate) fn composition_default() -> Self {
        Self::default()
    }

    pub(crate) fn make_paths_relative(&mut self, base: &Path) {
        fn relativize(source: &mut VisualSource, base: &Path) {
            let VisualSource::Generator(GeneratorSource::Wasm { module, .. }) = source else {
                return;
            };
            if let Ok(relative) = module.strip_prefix(base) {
                *module = relative.to_path_buf();
            }
        }
        for clip in &mut self.clips {
            relativize(&mut clip.source, base);
        }
        for track in &mut self.tracks {
            for row in &mut track.property_rows {
                relativize(&mut row.source, base);
            }
        }
    }

    pub(crate) fn resolve_relative_paths(&mut self, base: &Path) {
        fn resolve(source: &mut VisualSource, base: &Path) {
            let VisualSource::Generator(GeneratorSource::Wasm { module, .. }) = source else {
                return;
            };
            if module.is_relative() {
                *module = base.join(&*module);
            }
        }
        for clip in &mut self.clips {
            resolve(&mut clip.source, base);
        }
        for track in &mut self.tracks {
            for row in &mut track.property_rows {
                resolve(&mut row.source, base);
            }
        }
    }

    pub(crate) fn repair_id_counters(&mut self) {
        self.next_track = next_id(self.next_track, self.tracks.iter().map(|track| track.id));
        self.next_clip = next_id(self.next_clip, self.clips.iter().map(|clip| clip.id));
        self.next_group = next_id(
            self.next_group,
            self.clips.iter().filter_map(|clip| clip.group),
        );
        self.next_source_instance = next_u64_id(
            self.next_source_instance,
            self.clips.iter().map(|clip| clip.source_instance).chain(
                self.tracks
                    .iter()
                    .flat_map(|track| track.property_rows.iter().map(|row| row.source_instance)),
            ),
        );
        self.assign_missing_source_instances();
        self.migrate_clip_properties_to_rows();
        self.next_source_instance = next_u64_id(
            self.next_source_instance,
            self.clips.iter().map(|clip| clip.source_instance).chain(
                self.tracks
                    .iter()
                    .flat_map(|track| track.property_rows.iter().map(|row| row.source_instance)),
            ),
        );
    }

    pub(crate) fn property_row(
        &self,
        track: u32,
        source: &VisualSource,
        source_instance: u64,
    ) -> Option<&LayerPropertyRow> {
        self.tracks
            .iter()
            .find(|candidate| candidate.id == track)?
            .property_row(source, source_instance)
    }

    fn assign_missing_source_instances(&mut self) {
        let mut next = self.next_source_instance.max(1);
        for track in &mut self.tracks {
            for row in &mut track.property_rows {
                if source_requires_instance(&row.source) {
                    if row.source_instance == 0 {
                        row.source_instance = next;
                        next = next.saturating_add(1).max(1);
                    }
                } else {
                    row.source_instance = 0;
                }
            }
        }

        for clip in &mut self.clips {
            if source_requires_instance(&clip.source) {
                if clip.source_instance == 0 {
                    clip.source_instance = next;
                    next = next.saturating_add(1).max(1);
                }
            } else {
                clip.source_instance = 0;
            }
        }
        self.next_source_instance = next;
    }

    fn migrate_clip_properties_to_rows(&mut self) {
        for legacy in std::mem::take(&mut self.legacy_media_property_rows) {
            let Some(track_index) = self
                .tracks
                .iter()
                .position(|track| track.id == legacy.track)
            else {
                continue;
            };
            let source = self
                .clips
                .iter()
                .find(|clip| {
                    clip.track == legacy.track
                        && matches!(clip.source, VisualSource::Media(id) | VisualSource::Audio(id) if id == legacy.media)
                })
                .map(|clip| clip.source.clone())
                .unwrap_or_else(|| {
                    if self.tracks[track_index].kind == TrackKind::Audio {
                        VisualSource::Audio(legacy.media)
                    } else {
                        VisualSource::Media(legacy.media)
                    }
                });
            let track = &mut self.tracks[track_index];
            let incoming = LayerPropertyRow {
                source,
                source_instance: 0,
                pipeline: legacy.pipeline,
                composite: legacy.composite,
                model3d: legacy.model3d,
            };
            if let Some(row) = track.property_row_mut(&incoming.source, incoming.source_instance) {
                merge_property_row_animation(row, &incoming);
            } else {
                track.property_rows.push(incoming);
            }
        }

        for track in &mut self.tracks {
            let mut rows: Vec<LayerPropertyRow> = Vec::new();
            for row in std::mem::take(&mut track.property_rows) {
                if let Some(existing) = rows
                    .iter_mut()
                    .find(|candidate| candidate.matches_source(&row.source, row.source_instance))
                {
                    merge_property_row_animation(existing, &row);
                } else {
                    rows.push(row);
                }
            }
            track.property_rows = rows;
        }

        let mut clips = self.clips.clone();
        clips.sort_by(|left, right| {
            left.track
                .cmp(&right.track)
                .then_with(|| left.start.total_cmp(&right.start))
                .then_with(|| left.id.cmp(&right.id))
        });
        for clip in &clips {
            let Some(track) = self.tracks.iter_mut().find(|track| track.id == clip.track) else {
                continue;
            };
            if track
                .property_row(&clip.source, clip.source_instance)
                .is_none()
            {
                track.property_rows.push(LayerPropertyRow {
                    source: clip.source.clone(),
                    source_instance: clip.source_instance,
                    pipeline: clip.pipeline.clone(),
                    composite: clip.composite.clone(),
                    model3d: clip.model3d.clone(),
                });
            }
            let row = track
                .property_row_mut(&clip.source, clip.source_instance)
                .expect("property row was created above");
            merge_binding_animation(&mut row.composite.opacity, &clip.composite.opacity);
            merge_binding_animation(&mut row.composite.blend_mode, &clip.composite.blend_mode);
            merge_binding_animation(
                &mut row.composite.alpha_blend_mode,
                &clip.composite.alpha_blend_mode,
            );
            merge_binding_animation(&mut row.model3d.size, &clip.model3d.size);
            merge_binding_animation(&mut row.model3d.position, &clip.model3d.position);
            merge_binding_animation(&mut row.model3d.rotation, &clip.model3d.rotation);
            merge_binding_animation(&mut row.model3d.scale, &clip.model3d.scale);
            merge_pipeline_animation(&mut row.pipeline, &clip.pipeline);
            merge_source_animation(&mut row.source, &clip.source);
        }
        for clip in &mut self.clips {
            clip.clear_owned_keyframes();
        }
        self.prune_unused_property_rows();
    }

    pub(crate) fn prune_unused_property_rows(&mut self) {
        let clips = &self.clips;
        for track in &mut self.tracks {
            track.property_rows.retain(|row| {
                clips
                    .iter()
                    .any(|clip| clip.track == track.id && row.matches(clip))
            });
        }
    }

    pub(crate) fn clear_pipeline_references(&mut self, pipeline: u64) {
        clear_pipeline_references(&mut self.tracks, &mut self.clips, pipeline);
        for track in &mut self.tracks {
            for row in &mut track.property_rows {
                if row.pipeline.pipeline == Some(pipeline) {
                    row.pipeline.pipeline = None;
                    row.pipeline.overrides.clear();
                }
            }
        }
    }
}

fn next_id(current: u32, ids: impl Iterator<Item = u32>) -> u32 {
    let required = ids.max().unwrap_or(0).saturating_add(1).max(1);
    if current == u32::MAX {
        required
    } else {
        current.max(required)
    }
}

fn next_u64_id(current: u64, ids: impl Iterator<Item = u64>) -> u64 {
    let required = ids.max().unwrap_or(0).saturating_add(1).max(1);
    if current == u64::MAX {
        required
    } else {
        current.max(required)
    }
}

fn clear_pipeline_references(tracks: &mut [Track], clips: &mut [Clip], pipeline: u64) {
    for instance in tracks
        .iter_mut()
        .filter_map(|track| track.pipeline.as_mut())
    {
        if instance.pipeline == Some(pipeline) {
            instance.pipeline = None;
            instance.overrides.clear();
        }
    }
    for clip in clips {
        if clip.pipeline.pipeline == Some(pipeline) {
            clip.pipeline.pipeline = None;
            clip.pipeline.overrides.clear();
        }
    }
}

struct ClipPlacement {
    track: u32,
    time: f32,
    duration: f32,
    kind: TrackKind,
}

#[derive(Clone, Copy)]
enum ClipSource {
    Media(MediaId),
    Composition(CompositionId),
}

impl ClipSource {
    fn visual(self, audio: bool) -> VisualSource {
        match (self, audio) {
            (Self::Media(id), true) => VisualSource::Audio(id),
            (Self::Media(id), false) => VisualSource::Media(id),
            (Self::Composition(id), _) => VisualSource::Composition(id),
        }
    }

    fn audio_suffix(self) -> bool {
        matches!(self, Self::Composition(_))
    }
}

fn media_clip_duration(duration: Option<f64>) -> f32 {
    duration.unwrap_or(5.0).clamp(0.1, 24.0 * 60.0 * 60.0) as f32
}

struct NewClip {
    track: u32,
    time: f32,
    duration: f32,
    group: Option<u32>,
    name: String,
    color: ClipColor,
    source: VisualSource,
    pipeline: PipelineInstance,
}

pub(crate) struct CompositionExtraction {
    pub(crate) timeline: TimelineDocument,
    pub(crate) start: f32,
    pub(crate) duration: f32,
    pub(crate) has_video: bool,
    pub(crate) has_audio: bool,

    video_anchor_track: Option<u32>,
    audio_anchor_track: Option<u32>,
    video_was_solo: bool,
    audio_was_solo: bool,
}

impl Deref for TimelineState {
    type Target = TimelineEditState;

    fn deref(&self) -> &Self::Target {
        &self.edit
    }
}

impl DerefMut for TimelineState {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.edit
    }
}

impl Default for TimelineState {
    fn default() -> Self {
        Self::from_document(TimelineDocument::default())
    }
}

#[derive(Clone, Copy)]
struct PropertyRowLocation {
    track: usize,
    row: usize,
}

impl TimelineState {
    pub fn is_value_dragging(&self) -> bool {
        matches!(
            self.drag,
            Some(Drag::Clips {
                shift_adjust: Some(_),
                ..
            })
        ) || self.mixer_knobs.values().any(Knob::is_dragging)
    }

    fn prune_unused_property_rows(&mut self) {
        self.edit.document.prune_unused_property_rows();
    }

    fn ensure_property_row_for_clip(&mut self, id: u32) -> Option<PropertyRowLocation> {
        let clip = self.clips.iter().find(|clip| clip.id == id)?;
        let (track_id, source, source_instance, pipeline, composite, model3d) = (
            clip.track,
            clip.source.clone(),
            clip.source_instance,
            clip.pipeline.clone(),
            clip.composite.clone(),
            clip.model3d.clone(),
        );
        let track = self.track_index(track_id)?;
        if let Some(row) = self.tracks[track]
            .property_rows
            .iter()
            .position(|row| row.matches_source(&source, source_instance))
        {
            return Some(PropertyRowLocation { track, row });
        }
        self.tracks[track].property_rows.push(LayerPropertyRow {
            source,
            source_instance,
            pipeline,
            composite,
            model3d,
        });
        Some(PropertyRowLocation {
            track,
            row: self.tracks[track].property_rows.len() - 1,
        })
    }

    pub(crate) fn from_document(mut document: TimelineDocument) -> Self {
        document.repair_id_counters();
        let knobs = mixer_knobs(&document.tracks);
        let defaults = TimelineViewState::default();
        document.pixels_per_second = if document.pixels_per_second.is_finite() {
            document.pixels_per_second.clamp(1.0, MAX_PIXELS_PER_SECOND)
        } else {
            defaults.pixels_per_second
        };
        document.scroll_time = document.scroll_time.max(0.0);
        document.scroll_y = document.scroll_y.max(0.0);
        document.playhead = document.playhead.max(0.0);
        if !document.scroll_time.is_finite() {
            document.scroll_time = 0.0;
        }
        if !document.scroll_y.is_finite() {
            document.scroll_y = 0.0;
        }
        if !document.playhead.is_finite() {
            document.playhead = 0.0;
        }
        Self {
            edit: TimelineEditState::new(document),
            selection_levels: HashMap::new(),
            audio_levels: HashMap::new(),
            mixer_knobs: knobs,
            selection_frame: Instant::now(),
            playing: false,
            playback_just_started: false,
            tool: TimelineTool::Select,
            frame_rate: FRAME_RATE,
            drag: None,
            snap_times: Vec::new(),
            focused_stack: None,
            context_menu: None,
            rename: None,
            mixer_exact: None,
            keyframe_value_editor: None,
            selected_keyframes: Vec::new(),
            last_track_click: None,
            track_offsets: HashMap::new(),
            cursor: [0.0, 0.0],
            selected_track: None,
            pending_action: None,
            power_duplicate: None,
            render_cache_ranges: Vec::new(),
            render_output_range: None,
            expanded_keyframe_tracks: HashSet::new(),
            expanded_keyframe_lanes: HashSet::new(),
            keyframe_track_expansion: HashMap::new(),
            keyframe_lane_expansion: HashMap::new(),
            keyframe_lane_snapshot: Vec::new(),
            keyframe_row_heights: HashMap::new(),
            track_prefix_heights: Vec::new(),
        }
    }

    pub fn document(&self) -> TimelineDocument {
        self.edit.document().clone()
    }

    pub fn load_document(&mut self, document: TimelineDocument) {
        let focused_stack = self.focused_stack;
        *self = Self::from_document(document);
        self.focused_stack = focused_stack;
    }

    pub fn load_document_preserving_clipboard(&mut self, document: TimelineDocument) {
        let clipboard = std::mem::take(&mut self.clipboard);
        self.load_document(document);
        self.clipboard = clipboard;
    }

    pub(crate) fn ensure_composition_visual_pipelines(&mut self, plugins: &PluginRegistry) -> bool {
        let video_tracks = self
            .tracks
            .iter()
            .filter(|track| track.kind == TrackKind::Video)
            .map(|track| track.id)
            .collect::<HashSet<_>>();
        let Ok(template) = plugins.visual_pipeline_instance() else {
            return false;
        };
        let candidates = self
            .clips
            .iter()
            .filter(|clip| {
                video_tracks.contains(&clip.track)
                    && matches!(clip.source, VisualSource::Composition(_))
            })
            .map(|clip| clip.id)
            .collect::<Vec<_>>();
        let mut changed = false;
        for id in candidates {
            let Some(index) = self.ensure_property_row_for_clip(id) else {
                continue;
            };
            let pipeline = &mut self.tracks[index.track].property_rows[index.row].pipeline;
            if pipeline.transform().is_some() {
                continue;
            }
            pipeline.local_nodes = template.local_nodes.clone();
            pipeline.local_output = template.local_output.clone();
            changed = true;
        }
        changed
    }

    pub fn load_history_document(&mut self, document: TimelineDocument) {
        let focused_stack = self.focused_stack;
        let selected = self.selected.clone();
        let primary_selected = self.primary_selected;
        let clipboard = std::mem::take(&mut self.clipboard);
        let selected_track = self.selected_track;
        let playhead = self.playhead;
        let playing = self.playing;
        let frame_rate = self.frame_rate;
        let pixels_per_second = self.pixels_per_second;
        let scroll_time = self.scroll_time;
        let scroll_y = self.scroll_y;
        let frame_snap = self.frame_snap;
        let grid_snap = self.grid_snap;
        let clip_snap = self.clip_snap;
        let playhead_snap = self.playhead_snap;
        let follow_playhead = self.follow_playhead;
        let cursor = self.cursor;
        *self = Self::from_document(document);
        self.focused_stack = focused_stack;
        self.playhead = playhead;
        self.playing = playing;
        self.frame_rate = frame_rate;
        self.pixels_per_second = pixels_per_second;
        self.scroll_time = scroll_time;
        self.scroll_y = scroll_y;
        self.frame_snap = frame_snap;
        self.grid_snap = grid_snap;
        self.clip_snap = clip_snap;
        self.playhead_snap = playhead_snap;
        self.follow_playhead = follow_playhead;
        self.cursor = cursor;
        self.selected = selected
            .into_iter()
            .filter(|id| self.clips.iter().any(|clip| clip.id == *id))
            .collect();
        self.primary_selected = primary_selected.filter(|id| self.selected.contains(id));
        self.clipboard = clipboard;
        self.selected_track =
            selected_track.filter(|id| self.tracks.iter().any(|track| track.id == *id));
    }

    pub(crate) fn discard_media_from_clipboard(&mut self, media: &HashSet<MediaId>) {
        self.clipboard.retain(|entry| {
            !matches!(
                &entry.clip.source,
                VisualSource::Media(id) | VisualSource::Audio(id) if media.contains(id)
            )
        });
    }

    pub(crate) fn discard_composition_from_clipboard(&mut self, composition: CompositionId) {
        self.clipboard.retain(
            |entry| !matches!(&entry.clip.source, VisualSource::Composition(id) if *id == composition),
        );
    }

    pub fn playhead(&self) -> f32 {
        self.playhead
    }

    pub fn is_scrubbing(&self) -> bool {
        matches!(self.drag, Some(Drag::Playhead))
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn set_audio_levels(&mut self, levels: HashMap<u32, [f32; 2]>) {
        self.audio_levels = levels;
    }

    pub(crate) fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    pub(crate) fn clips(&self) -> &[Clip] {
        &self.clips
    }

    pub(crate) fn set_render_cache_ranges(&mut self, ranges: Vec<RenderCacheRange>) {
        self.render_cache_ranges = ranges;
    }

    pub(crate) fn set_render_output_range(&mut self, range: Option<(f32, f32)>) {
        self.render_output_range =
            range.filter(|(start, end)| start.is_finite() && end.is_finite() && end >= start);
    }

    pub(crate) fn render_end_seconds(&self) -> f32 {
        self.end_time
            .unwrap_or_else(|| self.clips.iter().map(Clip::end).fold(0.0_f32, f32::max))
    }

    pub(crate) fn extract_selection_for_composition(&mut self) -> Option<CompositionExtraction> {
        let mut selected = self
            .clips
            .iter()
            .filter(|clip| self.selected.contains(&clip.id))
            .cloned()
            .collect::<Vec<_>>();
        if selected.is_empty() {
            return None;
        }
        selected.sort_by(|left, right| {
            left.start
                .total_cmp(&right.start)
                .then(left.id.cmp(&right.id))
        });
        let start = selected
            .iter()
            .map(|clip| clip.start)
            .fold(f32::INFINITY, f32::min);
        let end = selected.iter().map(Clip::end).fold(start, f32::max);
        let track_kinds = self
            .tracks
            .iter()
            .map(|track| (track.id, track.kind))
            .collect::<HashMap<_, _>>();
        let has_audio = selected
            .iter()
            .any(|clip| track_kinds.get(&clip.track) == Some(&TrackKind::Audio));
        let has_video = selected
            .iter()
            .any(|clip| track_kinds.get(&clip.track) != Some(&TrackKind::Audio));

        let selected_track_ids = selected
            .iter()
            .map(|clip| clip.track)
            .collect::<HashSet<_>>();
        let selected_tracks = self
            .tracks
            .iter()
            .filter(|track| selected_track_ids.contains(&track.id))
            .collect::<Vec<_>>();
        let video_anchor_track = selected_tracks
            .iter()
            .filter(|track| track.kind != TrackKind::Audio)
            .min_by_key(|track| self.track_index(track.id).unwrap_or(usize::MAX))
            .map(|track| track.id);
        let audio_anchor_track = selected_tracks
            .iter()
            .filter(|track| track.kind == TrackKind::Audio)
            .min_by_key(|track| self.track_index(track.id).unwrap_or(usize::MAX))
            .map(|track| track.id);
        let video_was_solo = selected_tracks
            .iter()
            .any(|track| track.kind != TrackKind::Audio && track.solo);
        let audio_was_solo = selected_tracks
            .iter()
            .any(|track| track.kind == TrackKind::Audio && track.solo);
        let time_delta = -(start as f64);
        let mut timeline = TimelineDocument::composition_default();
        timeline.tracks = selected_tracks
            .into_iter()
            .map(|track| {
                let mut track = track.clone();

                track.property_rows.retain(|row| {
                    selected
                        .iter()
                        .any(|clip| clip.track == track.id && row.matches(clip))
                });
                track.shift_owned_keyframes(time_delta);
                track
            })
            .collect();
        for clip in &mut selected {
            clip.start = (clip.start - start).max(0.0);

            clip.clear_owned_keyframes();
        }
        timeline.clips = selected;
        timeline.next_track = timeline
            .tracks
            .iter()
            .map(|track| track.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1)
            .max(1);
        timeline.next_clip = timeline
            .clips
            .iter()
            .map(|clip| clip.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        timeline.next_group = timeline
            .clips
            .iter()
            .filter_map(|clip| clip.group)
            .max()
            .unwrap_or(0)
            .saturating_add(1)
            .max(1);
        timeline.next_source_instance =
            next_u64_id(
                1,
                timeline
                    .clips
                    .iter()
                    .map(|clip| clip.source_instance)
                    .chain(timeline.tracks.iter().flat_map(|track| {
                        track.property_rows.iter().map(|row| row.source_instance)
                    })),
            );
        timeline.end_time = Some((end - start).max(0.001));

        let selected = &self.edit.selected;
        self.edit
            .document
            .clips
            .retain(|clip| !selected.contains(&clip.id));
        self.clear_selection();
        self.prune_unused_property_rows();
        Some(CompositionExtraction {
            timeline,
            start,
            duration: (end - start).max(0.001),
            has_video,
            has_audio,
            video_anchor_track,
            audio_anchor_track,
            video_was_solo,
            audio_was_solo,
        })
    }

    fn add_composition_reference_track(
        &mut self,
        kind: TrackKind,
        anchor: Option<u32>,
        solo: bool,
        composition_name: &str,
    ) -> u32 {
        let index = anchor
            .and_then(|id| self.track_index(id))
            .unwrap_or_else(|| match kind {
                TrackKind::Audio => self.tracks.len(),
                TrackKind::Video | TrackKind::Effect => 0,
            });
        let inserted = self.add_track_at(kind, index);
        let track = &mut self.tracks[inserted];
        track.name = format!("{composition_name} Reference");
        track.solo = solo;
        track.id
    }

    pub(crate) fn insert_composition_reference(
        &mut self,
        composition: CompositionId,
        name: &str,
        extraction: &CompositionExtraction,
        visual_pipeline: PipelineInstance,
    ) {
        let video_track = extraction.has_video.then(|| {
            self.add_composition_reference_track(
                TrackKind::Video,
                extraction.video_anchor_track,
                extraction.video_was_solo,
                name,
            )
        });
        let audio_track = extraction.has_audio.then(|| {
            self.add_composition_reference_track(
                TrackKind::Audio,
                extraction.audio_anchor_track,
                extraction.audio_was_solo,
                name,
            )
        });
        let group = (extraction.has_video && extraction.has_audio).then(|| self.next_group_id());
        let mut inserted = Vec::new();
        if extraction.has_video {
            if let Some(track) = video_track {
                inserted.push(self.push_clip(NewClip {
                    track,
                    time: extraction.start,
                    duration: extraction.duration,
                    group,
                    name: name.to_string(),
                    color: ClipColor::VideoA,
                    source: VisualSource::Composition(composition),
                    pipeline: visual_pipeline,
                }));
            }
        }
        if extraction.has_audio {
            if let Some(track) = audio_track {
                inserted.push(self.push_clip(NewClip {
                    track,
                    time: extraction.start,
                    duration: extraction.duration,
                    group,
                    name: name.to_string(),
                    color: ClipColor::AudioA,
                    source: VisualSource::Composition(composition),
                    pipeline: PipelineInstance::effect_default(),
                }));
            }
        }
        self.select_clip_ids(&inserted);
    }

    fn follow_playhead(&mut self, snapshot: &LayoutSnapshot) {
        if !self.follow_playhead {
            return;
        }
        let Some(layout) = Self::visible_layout(snapshot) else {
            return;
        };
        let visible = self.visible_duration(layout);
        let midpoint = self.scroll_time + visible * 0.5;
        if self.playhead as f64 > midpoint {
            self.scroll_time = (self.playhead as f64 - visible * 0.5).max(0.0);
        }
    }

    pub(crate) fn sync_forward_playhead(&mut self, time: f32, snapshot: &LayoutSnapshot) {
        if !self.playing || self.is_scrubbing() || time <= self.playhead {
            return;
        }
        self.playhead = time;
        self.follow_playhead(snapshot);
    }

    pub(crate) fn set_selected_clip_start(&mut self, start: f32) {
        if !start.is_finite() {
            return;
        }
        let Some(id) = self.selected_clip_id() else {
            return;
        };
        let Some(anchor) = self.clips.iter().find(|clip| clip.id == id) else {
            return;
        };
        let group = anchor.group;
        let delta = start.max(0.0) - anchor.start;
        for clip in &mut self.clips {
            if clip.id == id || group.is_some() && clip.group == group {
                clip.start = (clip.start + delta).max(0.0);
            }
        }
    }

    pub(crate) fn set_selected_clip_end(&mut self, end: f32) {
        if !end.is_finite() {
            return;
        }
        let Some(id) = self.selected_clip_id() else {
            return;
        };
        let Some(anchor) = self.clips.iter().find(|clip| clip.id == id) else {
            return;
        };
        let group = anchor.group;
        let duration = (end - anchor.start).max(MIN_CLIP);
        for clip in &mut self.clips {
            if clip.id == id || group.is_some() && clip.group == group {
                clip.duration = duration;
                clip.fade_in = clip.fade_in.min(duration);
                clip.fade_out = clip.fade_out.min(duration);
            }
        }
    }

    pub(crate) fn selected_track(&self) -> Option<&Track> {
        let id = self.selected_track?;
        self.tracks.iter().find(|track| track.id == id)
    }

    pub fn clear_selection(&mut self) {
        self.selected.clear();
        self.primary_selected = None;
        self.selected_track = None;
    }

    pub fn select_clip_by_id(&mut self, id: u32, additive: bool) -> bool {
        if !self.clips.iter().any(|clip| clip.id == id) {
            return false;
        }
        self.selected_track = None;
        self.select_clip(id, additive);
        true
    }

    pub fn select_clip_ids(&mut self, ids: &[u32]) {
        self.selected = ids
            .iter()
            .copied()
            .filter(|id| self.clips.iter().any(|clip| clip.id == *id))
            .collect();
        self.primary_selected = ids.first().copied().filter(|id| self.selected.contains(id));
        self.selected_track = None;
    }

    pub(crate) fn selected_media_track_requirements(
        &self,
        _project: &Project,
    ) -> Option<(usize, usize)> {
        let track_kinds = self
            .tracks
            .iter()
            .map(|track| (track.id, track.kind))
            .collect::<HashMap<_, _>>();
        let mut requirements = None::<(usize, usize)>;
        for clip in &self.clips {
            if !self.selected.contains(&clip.id) {
                continue;
            }
            if !matches!(clip.source, VisualSource::Media(_) | VisualSource::Audio(_)) {
                continue;
            }
            let current = requirements.get_or_insert((0, 0));
            match track_kinds.get(&clip.track).copied() {
                Some(TrackKind::Video) => current.0 = 1,
                Some(TrackKind::Audio) => current.1 = 1,
                _ => {}
            }
        }
        requirements
    }

    pub(crate) fn has_compatible_replacement_media(&self, project: &Project) -> bool {
        use crate::project::MediaTrackKind;

        let Some((min_video, min_audio)) = self.selected_media_track_requirements(project) else {
            return false;
        };
        let excluded = self.selected_media_ids();
        project.media.iter().any(|asset| {
            if excluded.contains(&asset.id) {
                return false;
            }
            let video = if matches!(
                asset.kind,
                crate::project::MediaKind::Image { .. } | crate::project::MediaKind::Model3d
            ) {
                1
            } else {
                asset
                    .tracks
                    .iter()
                    .filter(|track| track.kind == MediaTrackKind::Video)
                    .count()
            };
            let audio = asset
                .tracks
                .iter()
                .filter(|track| track.kind == MediaTrackKind::Audio)
                .count();
            video >= min_video && audio >= min_audio
        })
    }

    pub(crate) fn replace_selected_media_source(
        &mut self,
        media: MediaId,
        audio: bool,
        name: &str,
    ) -> usize {
        let wanted_kind = if audio {
            TrackKind::Audio
        } else {
            TrackKind::Video
        };
        let track_kinds = self
            .tracks
            .iter()
            .map(|track| (track.id, track.kind))
            .collect::<HashMap<_, _>>();
        let source = if audio {
            VisualSource::Audio(media)
        } else {
            VisualSource::Media(media)
        };
        let selected = self.selected.clone();
        let replacements = self
            .clips
            .iter()
            .filter(|clip| {
                selected.contains(&clip.id)
                    && track_kinds.get(&clip.track).copied() == Some(wanted_kind)
            })
            .map(|clip| {
                let properties = self
                    .edit
                    .document
                    .property_row(clip.track, &clip.source, clip.source_instance)
                    .cloned()
                    .unwrap_or_else(|| LayerPropertyRow {
                        source: clip.source.clone(),
                        source_instance: clip.source_instance,
                        pipeline: clip.pipeline.clone(),
                        composite: clip.composite.clone(),
                        model3d: clip.model3d.clone(),
                    });
                (clip.id, clip.track, properties)
            })
            .collect::<Vec<_>>();

        for (id, _, _) in &replacements {
            let Some(clip) = self.clips.iter_mut().find(|clip| clip.id == *id) else {
                continue;
            };
            clip.source = source.clone();
            clip.source_instance = 0;
            clip.name = name.to_string();
        }

        for (_, track_id, mut properties) in replacements.iter().cloned() {
            let Some(track) = self.tracks.iter_mut().find(|track| track.id == track_id) else {
                continue;
            };
            if track.property_row(&source, 0).is_none() {
                properties.source = source.clone();
                properties.source_instance = 0;
                track.property_rows.push(properties);
            }
        }
        self.prune_unused_property_rows();
        replacements.len()
    }

    pub fn set_selected_endpoint_position(&mut self, input: bool, position: [f32; 2]) -> bool {
        let Some(instance) = self.selected_pipeline_mut() else {
            return false;
        };
        if input {
            instance.ui_input_position = Some(position);
        } else {
            instance.ui_output_position = Some(position);
        }
        true
    }

    pub fn set_selected_local_node_position(&mut self, node: u64, position: [f32; 2]) -> bool {
        let Some(node) = self.selected_pipeline_mut().and_then(|pipeline| {
            pipeline
                .local_nodes
                .iter_mut()
                .find(|candidate| candidate.id == node)
        }) else {
            return false;
        };
        node.ui_position = Some(position);
        true
    }

    pub fn insert_target(
        &self,
        snapshot: &LayoutSnapshot,
        point: [f32; 2],
    ) -> Option<(u32, f32, TrackKind)> {
        if let Some((_, layout)) = Self::active_layout(snapshot, point) {
            if layout.body.contains(point) {
                if let Some(track) = self
                    .track_at(layout, point[1])
                    .and_then(|index| self.tracks.get(index))
                {
                    return Some((track.id, self.time_at(layout, point[0]), track.kind));
                }
            }
        }
        if let Some(track) = self.selected_track() {
            return Some((track.id, self.playhead, track.kind));
        }
        if let Some(track) = self
            .selected_clip()
            .and_then(|clip| self.tracks.iter().find(|track| track.id == clip.track))
        {
            return Some((track.id, self.playhead, track.kind));
        }
        self.tracks
            .first()
            .map(|track| (track.id, self.playhead, track.kind))
    }

    pub fn take_action(&mut self) -> Option<TimelineAction> {
        self.pending_action.take()
    }

    pub(crate) fn keyboard_history_label(&self) -> Option<&'static str> {
        if let Some(editor) = &self.mixer_exact {
            return Some(match editor.parameter {
                MixerParameter::Volume => "Edit track volume",
                MixerParameter::Pan => "Edit track pan",
            });
        }
        self.rename.as_ref().map(|_| "Rename track")
    }

    pub(crate) fn history_gesture_label(&self) -> Option<&'static str> {
        if let Some((&(track, parameter), _)) =
            self.mixer_knobs.iter().find(|(_, knob)| knob.is_dragging())
        {
            let _ = track;
            return Some(match parameter {
                MixerParameter::Volume => "Adjust track volume",
                MixerParameter::Pan => "Adjust track pan",
            });
        }
        match self.drag.as_ref()? {
            Drag::ClipEdge {
                rate_stretch: true, ..
            } => Some("Rate stretch clip"),
            Drag::ClipEdge { .. } => Some("Trim clip"),
            Drag::Clips {
                duplicated: true, ..
            } => Some("Duplicate clips"),
            Drag::Clips {
                shift_adjust: Some(_),
                ..
            } => Some("Adjust clip offset and level"),
            Drag::Clips { .. } => Some("Move clips"),
            Drag::Track { .. } => Some("Reorder track"),
            Drag::Keyframe { .. } => Some("Move keyframe"),
            Drag::KeyframeEase(_) => Some("Edit keyframe easing"),

            Drag::Pan { .. } | Drag::BoxSelect { .. } | Drag::Playhead | Drag::Overview { .. } => {
                None
            }
        }
    }

    fn allocate_source_instance(&mut self) -> u64 {
        let id = self.next_source_instance.max(1);
        self.next_source_instance = id.saturating_add(1).max(1);
        id
    }

    fn source_instance_for_new_source(&mut self, source: &VisualSource) -> u64 {
        if source_requires_instance(source) {
            self.allocate_source_instance()
        } else {
            0
        }
    }

    fn push_clip(&mut self, clip: NewClip) -> u32 {
        let first_new_clip = self.clips.len();
        let NewClip {
            track,
            time,
            duration,
            group,
            name,
            color,
            source,
            pipeline,
        } = clip;
        let id = self.next_clip;
        self.next_clip += 1;
        let source_instance = self.source_instance_for_new_source(&source);
        self.clips.push(Clip {
            id,
            track,
            start: time.max(0.0),
            duration,
            speed: 1.0,
            source_offset: 0.0,
            opacity: 1.0,
            volume: 1.0,
            fade_in: 0.0,
            fade_out: 0.0,
            group,
            name,
            color,
            source,
            source_instance,
            pipeline,
            composite: LayerComposite::default(),
            model3d: Model3dClipTransform::default(),
        });
        self.initialize_end_from_clips_since(first_new_clip);
        let _ = self.ensure_property_row_for_clip(id);
        id
    }

    fn initialize_end_from_clips_since(&mut self, first_new_clip: usize) {
        if self.end_time.is_none() {
            self.set_initial_end_from_clips_since(first_new_clip);
        }
    }

    pub(crate) fn set_initial_end_from_clips_since(&mut self, first_new_clip: usize) {
        let Some(new_clips) = self.clips.get(first_new_clip..) else {
            return;
        };
        if let Some(end) = new_clips.iter().map(Clip::end).max_by(f32::total_cmp) {
            self.end_time = Some(end);
        }
    }

    fn select_only(&mut self, id: u32) {
        self.selected.clear();
        self.selected.insert(id);
        self.primary_selected = Some(id);
        self.selected_track = None;
    }

    fn next_group_id(&mut self) -> u32 {
        let group = self.next_group.max(1);
        self.next_group = group.saturating_add(1).max(1);
        group
    }

    fn select_group(&mut self, primary: u32, members: impl IntoIterator<Item = u32>) {
        self.selected.clear();
        self.selected.extend(members);
        self.primary_selected = Some(primary);
        self.selected_track = None;
    }

    fn nearest_or_create_audio_track(&mut self, video_index: usize) -> u32 {
        if let Some(track) = self
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, track)| track.kind == TrackKind::Audio)
            .min_by_key(|(index, _)| index.abs_diff(video_index))
            .map(|(_, track)| track.id)
        {
            return track;
        }
        let index = self.add_track_at(TrackKind::Audio, video_index);
        self.tracks[index].id
    }

    fn insert_clip_source_at(
        &mut self,
        placement: ClipPlacement,
        name: String,
        color: ClipColor,
        source: VisualSource,
        pipeline: PipelineInstance,
    ) -> Option<u32> {
        let ClipPlacement {
            track,
            time,
            duration,
            kind,
        } = placement;
        self.tracks
            .iter()
            .any(|candidate| candidate.id == track && candidate.kind == kind)
            .then_some(())?;
        let track = self.resolve_clip_placement(track, kind, time, duration);
        Some(self.push_clip(NewClip {
            track,
            time,
            duration,
            group: None,
            name,
            color,
            source,
            pipeline,
        }))
    }

    fn insert_av_pair_at(
        &mut self,
        at: (u32, f32),
        duration: f32,
        name: String,
        video_source: VisualSource,
        audio_source: VisualSource,
        visual_pipeline: PipelineInstance,
    ) -> bool {
        let (video_track, time) = at;
        let Some(video_index) = self.tracks.iter().position(|candidate| {
            candidate.id == video_track && candidate.kind == TrackKind::Video
        }) else {
            return false;
        };
        let video_track =
            self.resolve_clip_placement(video_track, TrackKind::Video, time, duration);
        let video_index = self.track_index(video_track).unwrap_or(video_index);
        let audio_track = self.nearest_or_create_audio_track(video_index);
        let audio_track =
            self.resolve_clip_placement(audio_track, TrackKind::Audio, time, duration);
        let group = self.next_group_id();
        let video_id = self.push_clip(NewClip {
            track: video_track,
            time,
            duration,
            group: Some(group),
            name: name.clone(),
            color: ClipColor::VideoA,
            source: video_source,
            pipeline: visual_pipeline,
        });
        let audio_id = self.push_clip(NewClip {
            track: audio_track,
            time,
            duration,
            group: Some(group),
            name: format!("{name} - Audio"),
            color: ClipColor::AudioA,
            source: audio_source,
            pipeline: PipelineInstance::effect_default(),
        });
        self.select_group(video_id, [video_id, audio_id]);
        true
    }

    fn insert_source_clip_at(
        &mut self,
        at: (u32, f32),
        source: ClipSource,
        name: String,
        audio: bool,
        duration: f32,
        visual_pipeline: PipelineInstance,
    ) -> bool {
        let Some(id) = self.insert_clip_source_at(
            ClipPlacement {
                track: at.0,
                time: at.1,
                duration,
                kind: if audio {
                    TrackKind::Audio
                } else {
                    TrackKind::Video
                },
            },
            if audio && source.audio_suffix() {
                format!("{name} - Audio")
            } else {
                name
            },
            if audio {
                ClipColor::AudioA
            } else {
                ClipColor::VideoA
            },
            source.visual(audio),
            if audio {
                PipelineInstance::effect_default()
            } else {
                visual_pipeline
            },
        ) else {
            return false;
        };
        self.select_only(id);
        true
    }

    fn insert_av_source_at(
        &mut self,
        at: (u32, f32),
        source: ClipSource,
        name: String,
        has_audio: bool,
        duration: f32,
        visual_pipeline: PipelineInstance,
    ) -> bool {
        if !has_audio {
            return self.insert_source_clip_at(at, source, name, false, duration, visual_pipeline);
        }
        self.insert_av_pair_at(
            at,
            duration,
            name,
            source.visual(false),
            source.visual(true),
            visual_pipeline,
        )
    }

    pub fn insert_media_clip_at(
        &mut self,
        at: (u32, f32),
        media: MediaId,
        name: String,
        audio: bool,
        duration: Option<f64>,
        visual_pipeline: PipelineInstance,
    ) -> bool {
        self.insert_source_clip_at(
            at,
            ClipSource::Media(media),
            name,
            audio,
            media_clip_duration(duration),
            visual_pipeline,
        )
    }

    pub fn insert_av_media_clip_at(
        &mut self,
        at: (u32, f32),
        media: MediaId,
        name: String,
        has_audio: bool,
        duration: Option<f64>,
        visual_pipeline: PipelineInstance,
    ) -> bool {
        self.insert_av_source_at(
            at,
            ClipSource::Media(media),
            name,
            has_audio,
            media_clip_duration(duration),
            visual_pipeline,
        )
    }

    pub fn insert_composition_clip_at(
        &mut self,
        at: (u32, f32),
        composition: CompositionId,
        name: String,
        audio: bool,
        duration: f32,
        visual_pipeline: PipelineInstance,
    ) -> bool {
        self.insert_source_clip_at(
            at,
            ClipSource::Composition(composition),
            name,
            audio,
            duration.max(MIN_CLIP),
            visual_pipeline,
        )
    }

    pub fn insert_av_composition_clip_at(
        &mut self,
        at: (u32, f32),
        composition: CompositionId,
        name: String,
        has_audio: bool,
        duration: f32,
        visual_pipeline: PipelineInstance,
    ) -> bool {
        self.insert_av_source_at(
            at,
            ClipSource::Composition(composition),
            name,
            has_audio,
            duration.max(MIN_CLIP),
            visual_pipeline,
        )
    }

    pub fn media_drop_anchor(
        &mut self,
        snapshot: &LayoutSnapshot,
        point: [f32; 2],
    ) -> Option<(u32, f32)> {
        let (_, layout) = Self::active_layout(snapshot, point)?;
        if !layout.body.contains(point) {
            return None;
        }
        let index = self.track_at(layout, point[1])?;
        let track = self.tracks.get(index)?.id;
        let raw = self.time_at(layout, point[0]);
        let snapped = self.insertion_snap_time(layout, raw);
        self.snap_times.clear();
        if (snapped - raw).abs() > f32::EPSILON {
            self.snap_times.push(snapped);
        }
        Some((track, snapped))
    }

    pub fn media_tracks_near(&mut self, anchor: u32, audio: bool, count: usize) -> Vec<u32> {
        if count == 0 {
            return Vec::new();
        }
        let kind = if audio {
            TrackKind::Audio
        } else {
            TrackKind::Video
        };
        let anchor_index = self.track_index(anchor).unwrap_or(0);
        let mut ids = self
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, track)| track.kind == kind)
            .map(|(index, track)| (index, track.id))
            .collect::<Vec<_>>();
        ids.sort_by_key(|(index, _)| index.abs_diff(anchor_index));
        let mut result = ids
            .into_iter()
            .take(count)
            .map(|(_, id)| id)
            .collect::<Vec<_>>();
        while result.len() < count {
            let insert_at = if audio { self.tracks.len() } else { 0 };
            let index = self.add_track_at(kind, insert_at);
            result.push(self.tracks[index].id);
        }
        result
    }

    pub fn media_drop_previews(
        &self,
        snapshot: &LayoutSnapshot,
        point: [f32; 2],
        specs: &[MediaDropPreviewSpec],
    ) -> Option<Vec<Rect>> {
        let (_, layout) = Self::active_layout(snapshot, point)?;
        if !layout.body.contains(point) {
            return None;
        }
        let anchor = self.track_at(layout, point[1])?;
        let raw = self.time_at(layout, point[0]);
        let time = self.insertion_snap_time(layout, raw);
        let mut cursor_time = time;
        let mut previews = Vec::new();
        let lane_h = self.tracks.get(anchor).map_or(58.0, |track| track.height);
        let existing_video = self
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.kind == TrackKind::Video)
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        let existing_audio = self
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.kind == TrackKind::Audio)
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        let nearest_index = |tracks: &[usize]| {
            tracks
                .iter()
                .copied()
                .min_by_key(|index| index.abs_diff(anchor))
        };
        let video_anchor = nearest_index(&existing_video).unwrap_or(anchor);
        let audio_anchor = nearest_index(&existing_audio).unwrap_or(anchor);

        for spec in specs {
            let duration = spec.duration.clamp(0.1, 24.0 * 60.0 * 60.0);
            let width = (duration * self.pixels_per_second).max(4.0);
            for stream in 0..spec.video_tracks {
                let track = existing_video
                    .iter()
                    .copied()
                    .filter(|i| *i >= video_anchor)
                    .nth(stream);
                let y = track.map(|i| self.track_y(layout, i)).unwrap_or_else(|| {
                    self.track_y(layout, video_anchor) - lane_h * (stream + 1) as f32
                });
                let rect = Rect::new(
                    self.time_x(layout, cursor_time),
                    y + CLIP_PAD,
                    width,
                    (lane_h - CLIP_PAD * 2.0).max(1.0),
                );
                let left = rect.x.max(layout.body.x);
                let top = rect.y.max(layout.body.y);
                let right = rect.right().min(layout.body.right());
                let bottom = rect.bottom().min(layout.body.bottom());
                if right > left && bottom > top {
                    previews.push(Rect::new(left, top, right - left, bottom - top));
                }
            }
            for stream in 0..spec.audio_tracks {
                let track = existing_audio
                    .iter()
                    .copied()
                    .filter(|i| *i >= audio_anchor)
                    .nth(stream);
                let y = track.map(|i| self.track_y(layout, i)).unwrap_or_else(|| {
                    self.track_y(layout, audio_anchor) + lane_h * (stream + 1) as f32
                });
                let rect = Rect::new(
                    self.time_x(layout, cursor_time),
                    y + CLIP_PAD,
                    width,
                    (lane_h - CLIP_PAD * 2.0).max(1.0),
                );
                let left = rect.x.max(layout.body.x);
                let top = rect.y.max(layout.body.y);
                let right = rect.right().min(layout.body.right());
                let bottom = rect.bottom().min(layout.body.bottom());
                if right > left && bottom > top {
                    previews.push(Rect::new(left, top, right - left, bottom - top));
                }
            }
            cursor_time += duration;
        }
        Some(previews)
    }

    pub fn insert_plugin_generator_at(
        &mut self,
        track: u32,
        time: f32,
        definition: &GeneratorDefinition,
        visual_pipeline: PipelineInstance,
    ) -> bool {
        let Ok(generator) = definition.instantiate() else {
            return false;
        };
        self.insert_generator_clip_at(track, time, &definition.name, generator, visual_pipeline)
    }

    pub fn insert_wasm_clip_at(
        &mut self,
        track: u32,
        time: f32,
        module: std::path::PathBuf,
        plugin_id: String,
        visual_pipeline: PipelineInstance,
    ) -> bool {
        let name = plugin_id.clone();
        self.insert_generator_clip_at(
            track,
            time,
            &name,
            GeneratorSource::Wasm {
                plugin_id,
                module,
                entry: DEFAULT_RENDER_EXPORT.into(),
                parameters: std::collections::BTreeMap::new(),
            },
            visual_pipeline,
        )
    }

    fn insert_generator_clip_at(
        &mut self,
        track: u32,
        time: f32,
        name: &str,
        generator: GeneratorSource,
        visual_pipeline: PipelineInstance,
    ) -> bool {
        let Some(id) = self.insert_clip_source_at(
            ClipPlacement {
                track,
                time,
                duration: 5.0,
                kind: TrackKind::Video,
            },
            format!("{name} {}", self.next_clip),
            ClipColor::VideoB,
            VisualSource::Generator(generator),
            visual_pipeline,
        ) else {
            return false;
        };
        self.select_only(id);
        true
    }

    pub fn insert_effect_clip_at(&mut self, track: u32, time: f32, pipeline: Option<u64>) -> bool {
        if !self
            .tracks
            .iter()
            .any(|candidate| candidate.id == track && candidate.kind == TrackKind::Effect)
        {
            return false;
        }
        let id = self.next_clip;
        let mut instance = PipelineInstance::effect_default();
        instance.pipeline = pipeline;
        let id = self.push_clip(NewClip {
            track,
            time,
            duration: 5.0,
            group: None,
            name: format!("Effect {id}"),
            color: ClipColor::Effect,
            source: VisualSource::EffectInput,
            pipeline: instance,
        });
        self.select_only(id);
        true
    }

    pub fn can_assign_pipeline(&self) -> bool {
        self.selected_clip().is_some()
            || self
                .selected_track()
                .is_some_and(|track| matches!(track.kind, TrackKind::Video | TrackKind::Audio))
    }

    fn ensure_property_row_for_moved_clip(
        &mut self,
        id: u32,
        previous_track: u32,
    ) -> Option<PropertyRowLocation> {
        let clip = self.clips.iter().find(|clip| clip.id == id)?;
        let (track_id, source, source_instance, pipeline, composite, model3d) = (
            clip.track,
            clip.source.clone(),
            clip.source_instance,
            clip.pipeline.clone(),
            clip.composite.clone(),
            clip.model3d.clone(),
        );
        let target_track = self.track_index(track_id)?;
        if let Some(row) = self.tracks[target_track]
            .property_rows
            .iter()
            .position(|row| row.matches_source(&source, source_instance))
        {
            return Some(PropertyRowLocation {
                track: target_track,
                row,
            });
        }
        let seed = self
            .track_index(previous_track)
            .and_then(|index| self.tracks[index].property_row(&source, source_instance))
            .cloned()
            .unwrap_or(LayerPropertyRow {
                source,
                source_instance,
                pipeline,
                composite,
                model3d,
            });
        self.tracks[target_track].property_rows.push(seed);
        Some(PropertyRowLocation {
            track: target_track,
            row: self.tracks[target_track].property_rows.len() - 1,
        })
    }

    pub fn selected_pipeline_kind(&self) -> PipelineKind {
        let selected_clip_is_audio = self.selected_clip().is_some_and(|clip| {
            clip.source.is_audio()
                || self
                    .tracks
                    .iter()
                    .find(|track| track.id == clip.track)
                    .is_some_and(|track| track.kind == TrackKind::Audio)
        });
        if selected_clip_is_audio
            || self
                .selected_track()
                .is_some_and(|track| track.kind == TrackKind::Audio)
        {
            PipelineKind::Audio
        } else {
            PipelineKind::Video
        }
    }

    pub fn selected_pipeline(&self) -> Option<&PipelineInstance> {
        self.selected_clip()
            .map(|clip| {
                self.edit
                    .document
                    .property_row(clip.track, &clip.source, clip.source_instance)
                    .map(|row| &row.pipeline)
                    .unwrap_or(&clip.pipeline)
            })
            .or_else(|| {
                self.selected_track()
                    .and_then(|track| track.pipeline.as_ref())
            })
    }

    pub(crate) fn clip_property_pipeline<'a>(&'a self, clip: &'a Clip) -> &'a PipelineInstance {
        self.edit
            .document
            .property_row(clip.track, &clip.source, clip.source_instance)
            .map(|row| &row.pipeline)
            .unwrap_or(&clip.pipeline)
    }

    pub fn set_selected_pipeline(&mut self, pipeline: Option<u64>) {
        self.set_selected_pipeline_impl(pipeline, false);
    }

    pub(crate) fn set_selected_pipeline_preserving_overrides(&mut self, pipeline: u64) {
        self.set_selected_pipeline_impl(Some(pipeline), true);
    }

    fn set_selected_pipeline_impl(&mut self, pipeline: Option<u64>, preserve_overrides: bool) {
        let assign = |instance: &mut PipelineInstance| {
            if instance.pipeline != pipeline {
                instance.pipeline = pipeline;
                if !preserve_overrides {
                    instance.overrides.clear();
                }
            }
        };
        if let Some(id) = self.selected_clip_id() {
            if let Some(index) = self.ensure_property_row_for_clip(id) {
                assign(&mut self.tracks[index.track].property_rows[index.row].pipeline);
            }
            return;
        }
        if let Some(id) = self.selected_track {
            if let Some(track) = self.tracks.iter_mut().find(|track| track.id == id) {
                if matches!(track.kind, TrackKind::Video | TrackKind::Audio) {
                    assign(
                        track
                            .pipeline
                            .get_or_insert_with(PipelineInstance::effect_default),
                    );
                }
            }
        }
    }

    pub fn clear_pipeline_references(&mut self, pipeline: u64) {
        self.document.clear_pipeline_references(pipeline);
    }

    pub(crate) fn remap_pipeline_selector_overrides(&mut self, remaps: &[PipelineSelectorRemap]) {
        for remap in remaps {
            let remap_instance = |instance: &mut PipelineInstance| {
                if instance.pipeline != Some(remap.owner) {
                    return;
                }
                for node in &remap.nodes {
                    if let Some(binding) = instance.overrides.get_mut(*node, "pipeline") {
                        remap_pipeline_selector_binding(
                            binding,
                            &remap.old_options,
                            &remap.new_options,
                        );
                    }
                }
            };
            for track in &mut self.tracks {
                if let Some(instance) = &mut track.pipeline {
                    remap_instance(instance);
                }
            }
            for clip in &mut self.clips {
                remap_instance(&mut clip.pipeline);
            }
            for track in &mut self.tracks {
                for row in &mut track.property_rows {
                    remap_instance(&mut row.pipeline);
                }
            }
        }
    }

    pub fn selected_keyframe_time(&self) -> f64 {
        self.playhead as f64
    }

    fn supports_speed(clip: &Clip, project: &Project) -> bool {
        match &clip.source {
            VisualSource::Audio(_)
            | VisualSource::AudioPlaceholder
            | VisualSource::Composition(_) => true,
            VisualSource::Media(id) => project
                .media(*id)
                .is_some_and(|asset| asset.kind == MediaKind::Video),
            _ => false,
        }
    }

    pub fn selected_speed(&self, project: &Project) -> Option<f32> {
        self.selected_clip()
            .filter(|clip| Self::supports_speed(clip, project))
            .map(|clip| clip.speed.max(0.01))
    }

    pub fn selected_clip_volume(&self) -> Option<f32> {
        let clip = self.selected_clip()?;
        let is_audio = clip.source.is_audio()
            || self
                .tracks
                .iter()
                .find(|track| track.id == clip.track)
                .is_some_and(|track| track.kind == TrackKind::Audio);
        is_audio.then_some(clip.volume.clamp(0.0, 1.0))
    }

    pub fn set_selected_clip_volume(&mut self, volume: f32) {
        let Some(primary) = self.selected_clip() else {
            return;
        };
        let primary_is_audio = primary.source.is_audio()
            || self
                .tracks
                .iter()
                .find(|track| track.id == primary.track)
                .is_some_and(|track| track.kind == TrackKind::Audio);
        if !primary_is_audio {
            return;
        }
        let selected = self.selected.clone();
        let audio_tracks = self
            .tracks
            .iter()
            .filter(|track| track.kind == TrackKind::Audio)
            .map(|track| track.id)
            .collect::<HashSet<_>>();
        let volume = volume.clamp(0.0, 1.0);
        for clip in &mut self.clips {
            if selected.contains(&clip.id)
                && (clip.source.is_audio() || audio_tracks.contains(&clip.track))
            {
                clip.volume = volume;
            }
        }
    }

    fn selected_clips(&self) -> impl Iterator<Item = &Clip> {
        self.clips
            .iter()
            .filter(|clip| self.selected.contains(&clip.id))
    }

    fn selected_groups(&self) -> HashSet<u32> {
        self.selected_clips()
            .filter_map(|clip| clip.group)
            .collect()
    }

    fn logical_selection(&self) -> HashSet<u32> {
        let groups = self.selected_groups();
        self.clips
            .iter()
            .filter(|clip| {
                self.selected.contains(&clip.id)
                    || clip.group.is_some_and(|group| groups.contains(&group))
            })
            .map(|clip| clip.id)
            .collect()
    }

    pub fn set_selected_speed(&mut self, project: &Project, speed: f32) {
        let selected = self.logical_selection();
        for clip in &mut self.clips {
            if selected.contains(&clip.id) && Self::supports_speed(clip, project) {
                clip.set_speed_preserving_source_span(speed);
            }
        }
    }

    pub(crate) fn set_clip_transform_value_at(
        &mut self,
        id: u32,
        time: f64,
        input: &str,
        value: GpuValue,
    ) -> bool {
        let Some(index) = self.ensure_property_row_for_clip(id) else {
            return false;
        };
        let pipeline = &mut self.tracks[index.track].property_rows[index.row].pipeline;
        let Some(binding) = pipeline
            .transform_mut()
            .and_then(|transform| transform.inputs.get_mut(input))
        else {
            return false;
        };
        binding.set_value(time, value);
        true
    }

    pub fn selected_logical_clip_count(&self) -> usize {
        let mut groups = HashSet::new();
        let mut ungrouped = 0usize;
        for clip in self.selected_clips() {
            if let Some(group) = clip.group {
                groups.insert(group);
            } else {
                ungrouped += 1;
            }
        }
        groups.len() + ungrouped
    }

    pub fn selected_duration(&self) -> Option<f32> {
        self.selected_clip().map(|clip| clip.duration.max(MIN_CLIP))
    }

    pub fn selected_total_logical_duration(&self) -> f32 {
        let mut grouped = HashMap::<u32, f32>::new();
        let mut total = 0.0;
        for clip in self.selected_clips() {
            let duration = clip.duration.max(MIN_CLIP);
            if let Some(group) = clip.group {
                grouped
                    .entry(group)
                    .and_modify(|current| *current = current.max(duration))
                    .or_insert(duration);
            } else {
                total += duration;
            }
        }
        total + grouped.values().sum::<f32>()
    }

    pub fn apply_speed_duration(&mut self, project: &Project, mode: SpeedDurationMode, value: f32) {
        if self.selected.is_empty() || !value.is_finite() || value <= 0.0 {
            return;
        }
        match mode {
            SpeedDurationMode::SpeedPercent => self.set_selected_speed(project, value / 100.0),
            SpeedDurationMode::PerClipDuration => self.set_selected_duration(project, value),
            SpeedDurationMode::TotalDuration => {
                let count = self.selected_logical_clip_count().max(1) as f32;
                self.set_selected_duration(project, value / count);
            }
        }
    }

    fn set_selected_duration(&mut self, project: &Project, duration: f32) {
        let duration = duration.max(MIN_CLIP);
        let selected = self.logical_selection();
        for clip in &mut self.clips {
            if !selected.contains(&clip.id) {
                continue;
            }
            if Self::supports_speed(clip, project) {
                let source_span = clip.duration.max(MIN_CLIP) * clip.speed.max(0.01);
                clip.duration = duration;
                clip.speed = (source_span / duration).clamp(0.01, 100.0);
            } else {
                clip.duration = duration;
            }
            clip.fade_in = clip.fade_in.min(clip.duration);
            clip.fade_out = clip.fade_out.min(clip.duration);
        }
    }

    fn selected_pipeline_id(&self) -> Option<PipelineId> {
        self.selected_pipeline()?.pipeline
    }

    fn selected_pipeline_binding<'a>(
        &'a self,
        project: &'a Project,
        node: u64,
        input: &str,
    ) -> Option<(&'a Binding, f64)> {
        let instance = self.selected_pipeline()?;
        let pipeline = project.pipeline(instance.pipeline?)?;
        let default = pipeline
            .nodes
            .iter()
            .find(|candidate| candidate.id == node)?
            .inputs
            .get(input)?;
        Some((
            instance.overrides.get(node, input).unwrap_or(default),
            self.selected_keyframe_time(),
        ))
    }

    fn selected_pipeline_override_binding_mut(
        &mut self,
        node: u64,
        input: &str,
    ) -> Option<(&mut Binding, f64)> {
        let time = self.selected_keyframe_time();
        let instance = self.selected_pipeline_mut()?;
        Some((instance.overrides.get_mut(node, input)?, time))
    }

    pub fn pipeline_input_value(
        &self,
        project: &Project,
        node: u64,
        input: &str,
    ) -> Option<GpuValue> {
        let (binding, time) = self.selected_pipeline_binding(project, node, input)?;
        binding.evaluate(time)
    }

    pub fn pipeline_input_has_keyframe(&self, project: &Project, node: u64, input: &str) -> bool {
        self.selected_pipeline_binding(project, node, input)
            .is_some_and(|(binding, time)| binding.has_keyframe(time))
    }

    pub fn pipeline_input_has_keyframes(&self, project: &Project, node: u64, input: &str) -> bool {
        self.selected_pipeline_binding(project, node, input)
            .is_some_and(|(binding, _)| binding.has_keyframes())
    }

    pub fn pipeline_host_input_value(
        &self,
        project: &Project,
        node: u64,
        input: &str,
    ) -> Option<HostValue> {
        let pipeline = self.selected_pipeline()?.pipeline?;
        project.pipeline_node_host_value(pipeline, node, input, self.selected_keyframe_time())
    }

    pub fn pipeline_host_input_has_keyframe(
        &self,
        project: &Project,
        node: u64,
        input: &str,
    ) -> bool {
        let Some(pipeline) = self.selected_pipeline_id() else {
            return false;
        };
        project.pipeline_node_host_has_keyframe(
            pipeline,
            node,
            input,
            self.selected_keyframe_time(),
        )
    }

    pub fn pipeline_host_input_has_keyframes(
        &self,
        project: &Project,
        node: u64,
        input: &str,
    ) -> bool {
        self.selected_pipeline_id()
            .is_some_and(|pipeline| project.pipeline_node_host_has_keyframes(pipeline, node, input))
    }

    pub fn pipeline_input_is_override(&self, node: u64, input: &str) -> bool {
        self.selected_pipeline()
            .is_some_and(|instance| instance.overrides.contains(node, input))
    }

    pub fn make_pipeline_input_unique(
        &mut self,
        project: &Project,
        node: u64,
        input: &str,
    ) -> bool {
        let Some(pipeline_id) = self.selected_pipeline_id() else {
            return false;
        };
        let Some(default) = project
            .pipeline(pipeline_id)
            .and_then(|pipeline| pipeline.node(node))
            .and_then(|node| node.inputs.get(input))
            .filter(|binding| !matches!(binding, Binding::Connection(_)))
            .cloned()
        else {
            return false;
        };
        let Some(instance) = self.selected_pipeline_mut() else {
            return false;
        };
        if instance.overrides.contains(node, input) {
            return false;
        }
        instance.overrides.insert(node, input, default);
        true
    }

    pub fn use_shared_pipeline_input(&mut self, node: u64, input: &str) -> bool {
        self.selected_pipeline_mut()
            .is_some_and(|instance| instance.overrides.remove(node, input).is_some())
    }

    pub fn set_pipeline_input_value(
        &mut self,
        project: &mut Project,
        node: u64,
        input: &str,
        value: GpuValue,
    ) -> bool {
        if let Some((binding, time)) = self.selected_pipeline_override_binding_mut(node, input) {
            binding.set_value(time, value);
            return true;
        }
        let Some(pipeline) = self.selected_pipeline_id() else {
            return false;
        };
        let time = self.selected_keyframe_time();
        project.set_pipeline_node_value_at(pipeline, node, input, time, value)
    }

    pub fn set_pipeline_input_component(
        &mut self,
        project: &mut Project,
        node: u64,
        input: &str,
        component: usize,
        value: f32,
        linked: bool,
    ) -> bool {
        if let Some((binding, time)) = self.selected_pipeline_override_binding_mut(node, input) {
            return binding.set_component_value(time, component, value, linked);
        }
        let Some(pipeline) = self.selected_pipeline_id() else {
            return false;
        };
        let time = self.selected_keyframe_time();
        project.set_pipeline_node_component_at(
            pipeline,
            node,
            input,
            time,
            (component, value, linked),
        )
    }

    pub fn reconcile_pipeline_overrides(&mut self, project: &Project) {
        fn reconcile(instance: &mut PipelineInstance, project: &Project) {
            let Some(pipeline) = instance.pipeline.and_then(|id| project.pipeline(id)) else {
                instance.overrides.clear();
                return;
            };
            instance.overrides.retain(|node, input, _| {
                pipeline
                    .node(node)
                    .and_then(|node| node.inputs.get(input))
                    .is_some_and(|binding| !matches!(binding, Binding::Connection(_)))
            });
        }

        for clip in &mut self.clips {
            reconcile(&mut clip.pipeline, project);
        }
        for track in &mut self.tracks {
            for row in &mut track.property_rows {
                reconcile(&mut row.pipeline, project);
            }
        }
        for track in &mut self.tracks {
            if let Some(instance) = &mut track.pipeline {
                reconcile(instance, project);
            }
        }
    }

    pub fn toggle_pipeline_keyframe(&mut self, project: &mut Project, node: u64, input: &str) {
        if let Some((binding, time)) = self.selected_pipeline_override_binding_mut(node, input) {
            binding.toggle_keyframe(time);
            return;
        }
        let Some(pipeline) = self.selected_pipeline_id() else {
            return;
        };
        let time = self.selected_keyframe_time();
        project.toggle_pipeline_node_keyframe(pipeline, node, input, time);
    }

    pub fn set_pipeline_host_input_value(
        &mut self,
        project: &mut Project,
        node: u64,
        input: &str,
        value: HostValue,
    ) -> bool {
        let Some(pipeline) = self.selected_pipeline_id() else {
            return false;
        };
        project.set_pipeline_node_host_value(
            pipeline,
            node,
            input,
            self.selected_keyframe_time(),
            value,
        )
    }

    pub fn toggle_pipeline_host_keyframe(
        &mut self,
        project: &mut Project,
        node: u64,
        input: &str,
    ) -> bool {
        let Some(pipeline) = self.selected_pipeline_id() else {
            return false;
        };
        project.toggle_pipeline_node_host_keyframe(
            pipeline,
            node,
            input,
            self.selected_keyframe_time(),
        )
    }

    fn selected_pipeline_mut(&mut self) -> Option<&mut PipelineInstance> {
        if let Some(id) = self.selected_clip_id() {
            let index = self.ensure_property_row_for_clip(id)?;
            return Some(&mut self.tracks[index.track].property_rows[index.row].pipeline);
        }
        let id = self.selected_track?;
        self.edit.track_mut(id)?.pipeline.as_mut()
    }

    pub fn connect_selected_local_image(&mut self, node: u64, source: Option<u64>) -> bool {
        let Some(instance) = self.selected_pipeline() else {
            return false;
        };
        let Some(input) = instance
            .local_nodes
            .iter()
            .find(|candidate| candidate.id == node)
            .and_then(EffectNode::stack_image_input_name)
            .map(str::to_owned)
        else {
            return false;
        };
        if let Some(source) = source {
            if source == node
                || !instance
                    .local_nodes
                    .iter()
                    .any(|candidate| candidate.id == source)
                || local_image_depends_on(instance, source, node)
            {
                return false;
            }
        }
        let Some(instance) = self.selected_pipeline_mut() else {
            return false;
        };
        let Some(target) = instance
            .local_nodes
            .iter_mut()
            .find(|candidate| candidate.id == node)
        else {
            return false;
        };
        target.image_inputs.insert(
            input,
            source.map_or(ImageBinding::PipelineInput, |source| {
                ImageBinding::Node(SocketRef {
                    node: source,
                    output: "image".into(),
                })
            }),
        );
        true
    }

    pub fn disconnect_selected_local_image(&mut self, node: u64) -> bool {
        let Some(instance) = self.selected_pipeline_mut() else {
            return false;
        };
        let Some(target) = instance
            .local_nodes
            .iter_mut()
            .find(|candidate| candidate.id == node)
        else {
            return false;
        };
        let Some((input, binding)) = target.stack_image_input() else {
            return false;
        };
        if matches!(binding, ImageBinding::Disconnected) {
            return false;
        }
        let input = input.to_owned();
        target
            .image_inputs
            .insert(input, ImageBinding::Disconnected);
        true
    }

    pub fn set_selected_local_output(&mut self, source: Option<u64>) -> bool {
        let Some(instance) = self.selected_pipeline() else {
            return false;
        };
        if source.is_some_and(|source| {
            !instance
                .local_nodes
                .iter()
                .any(|candidate| candidate.id == source)
        }) {
            return false;
        }
        let Some(instance) = self.selected_pipeline_mut() else {
            return false;
        };
        instance.local_output = source.map_or(ImageBinding::PipelineInput, |source| {
            ImageBinding::Node(SocketRef {
                node: source,
                output: "image".into(),
            })
        });
        true
    }

    pub fn disconnect_selected_local_output(&mut self) -> bool {
        let Some(instance) = self.selected_pipeline_mut() else {
            return false;
        };
        if matches!(&instance.local_output, ImageBinding::Disconnected) {
            return false;
        }
        instance.local_output = ImageBinding::Disconnected;
        true
    }

    pub fn remove_selected_local_node(&mut self, node: u64) -> bool {
        let Some(instance) = self.selected_pipeline_mut() else {
            return false;
        };
        let Some(index) = instance
            .local_nodes
            .iter()
            .position(|candidate| candidate.id == node)
        else {
            return false;
        };
        let upstream = instance.local_nodes[index]
            .stack_image_input()
            .map(|(_, binding)| binding.clone())
            .unwrap_or(ImageBinding::Disconnected);
        instance.local_nodes.remove(index);
        for target in &mut instance.local_nodes {
            target.replace_image_source(node, &upstream);
        }
        if matches!(&instance.local_output, ImageBinding::Node(socket) if socket.node == node) {
            instance.local_output = upstream;
        }
        true
    }

    fn transform_binding(&self, input: &str) -> Option<(&Binding, f64)> {
        Some((
            self.selected_pipeline()?.transform()?.inputs.get(input)?,
            self.playhead as f64,
        ))
    }

    fn transform_binding_mut(&mut self, input: &str) -> Option<(&mut Binding, f64)> {
        let time = self.playhead as f64;
        Some((
            self.selected_pipeline_mut()?
                .transform_mut()?
                .inputs
                .get_mut(input)?,
            time,
        ))
    }

    pub fn transform_value(&self, input: &str) -> Option<GpuValue> {
        let (binding, time) = self.transform_binding(input)?;
        binding.evaluate(time)
    }

    pub fn set_transform_value(&mut self, input: &str, value: GpuValue) {
        if let Some((binding, time)) = self.transform_binding_mut(input) {
            binding.set_value(time, value);
        }
    }

    fn selected_local_node(&self, node: u64) -> Option<&EffectNode> {
        self.selected_pipeline()?
            .local_nodes
            .iter()
            .find(|candidate| candidate.id == node)
    }

    fn selected_local_node_mut(&mut self, node: u64) -> Option<&mut EffectNode> {
        self.selected_pipeline_mut()?
            .local_nodes
            .iter_mut()
            .find(|candidate| candidate.id == node)
    }

    fn selected_local_binding_mut(
        &mut self,
        node: u64,
        input: &str,
    ) -> Option<(&mut Binding, f64)> {
        let time = self.selected_keyframe_time();
        let binding = self.selected_local_node_mut(node)?.inputs.get_mut(input)?;
        Some((binding, time))
    }

    fn selected_local_host_binding(&self, node: u64, input: &str) -> Option<(&HostBinding, f64)> {
        let binding = self.selected_local_node(node)?.host_inputs.get(input)?;
        Some((binding, self.selected_keyframe_time()))
    }

    fn selected_local_host_binding_mut(
        &mut self,
        node: u64,
        input: &str,
    ) -> Option<(&mut HostBinding, f64)> {
        let time = self.selected_keyframe_time();
        let binding = self
            .selected_local_node_mut(node)?
            .host_inputs
            .get_mut(input)?;
        Some((binding, time))
    }

    pub fn set_selected_local_node_value(
        &mut self,
        node: u64,
        input: &str,
        value: GpuValue,
    ) -> bool {
        let Some((binding, time)) = self.selected_local_binding_mut(node, input) else {
            return false;
        };
        binding.set_value(time, value);
        true
    }

    pub fn selected_local_node_host_value(&self, node: u64, input: &str) -> Option<HostValue> {
        let (binding, time) = self.selected_local_host_binding(node, input)?;
        binding.evaluate(time)
    }

    pub fn selected_local_node_host_has_keyframe(&self, node: u64, input: &str) -> bool {
        self.selected_local_host_binding(node, input)
            .is_some_and(|(binding, time)| binding.has_keyframe(time))
    }

    pub fn selected_local_node_host_has_keyframes(&self, node: u64, input: &str) -> bool {
        self.selected_local_host_binding(node, input)
            .is_some_and(|(binding, _)| binding.has_keyframes())
    }

    pub fn set_selected_local_node_host_value(
        &mut self,
        node: u64,
        input: &str,
        value: HostValue,
    ) -> bool {
        let Some((binding, time)) = self.selected_local_host_binding_mut(node, input) else {
            return false;
        };
        binding.set_value(time, value);
        true
    }

    pub fn toggle_selected_local_node_host_keyframe(&mut self, node: u64, input: &str) -> bool {
        let Some((binding, time)) = self.selected_local_host_binding_mut(node, input) else {
            return false;
        };
        binding.toggle_keyframe(time);
        true
    }

    pub fn set_selected_local_node_component(
        &mut self,
        node: u64,
        input: &str,
        component: usize,
        value: f32,
        linked: bool,
    ) -> bool {
        let time = self.selected_keyframe_time();
        let Some(node) = self.selected_local_node_mut(node) else {
            return false;
        };
        let Some(binding) = node.inputs.get_mut(input) else {
            return false;
        };
        if !binding.set_component_value(time, component, value, linked) {
            return false;
        }
        node.sync_dynamic_image_inputs();
        true
    }

    pub fn set_transform_component_linked(
        &mut self,
        input: &str,
        component: usize,
        value: f32,
        linked: bool,
    ) {
        if let Some((binding, time)) = self.transform_binding_mut(input) {
            binding.set_component_value(time, component, value, linked);
        }
    }

    pub fn toggle_transform_keyframe(&mut self, input: &str) {
        if let Some((binding, time)) = self.transform_binding_mut(input) {
            binding.toggle_keyframe(time);
        }
    }

    pub fn transform_has_keyframe(&self, input: &str) -> bool {
        self.transform_binding(input)
            .is_some_and(|(binding, time)| binding.has_keyframe(time))
    }

    pub fn transform_has_keyframes(&self, input: &str) -> bool {
        self.transform_binding(input)
            .is_some_and(|(binding, _)| binding.has_keyframes())
    }

    fn selected_model3d_clip_id(&self, project: &Project) -> Option<u32> {
        let clip = self.selected_clip()?;
        let VisualSource::Media(media) = &clip.source else {
            return None;
        };
        matches!(project.media(*media)?.kind, MediaKind::Model3d).then_some(clip.id)
    }

    fn model3d_binding<'a>(&'a self, project: &Project, input: &str) -> Option<(&'a Binding, f64)> {
        let clip_id = self.selected_model3d_clip_id(project)?;
        let clip = self.clips.iter().find(|clip| clip.id == clip_id)?;
        let VisualSource::Media(_media) = clip.source else {
            return None;
        };
        let model = self
            .edit
            .document
            .property_row(clip.track, &clip.source, clip.source_instance)
            .map(|row| &row.model3d)
            .unwrap_or(&clip.model3d);
        Some((model.binding(input)?, self.playhead as f64))
    }

    fn model3d_binding_mut<'a>(
        &'a mut self,
        project: &Project,
        input: &str,
    ) -> Option<(&'a mut Binding, f64)> {
        let time = self.playhead as f64;
        let clip_id = self.selected_model3d_clip_id(project)?;
        let index = self.ensure_property_row_for_clip(clip_id)?;
        Some((
            self.tracks[index.track].property_rows[index.row]
                .model3d
                .binding_mut(input)?,
            time,
        ))
    }

    pub fn selected_model3d_value(&self, project: &Project, input: &str) -> Option<GpuValue> {
        let (binding, time) = self.model3d_binding(project, input)?;
        binding.evaluate(time)
    }

    pub fn selected_model3d_shading(&self, project: &Project) -> Option<Model3dShading> {
        let clip_id = self.selected_model3d_clip_id(project)?;
        let clip = self.clips.iter().find(|clip| clip.id == clip_id)?;
        let VisualSource::Media(_media) = clip.source else {
            return None;
        };
        Some(
            self.edit
                .document
                .property_row(clip.track, &clip.source, clip.source_instance)
                .map(|row| row.model3d.shading)
                .unwrap_or(clip.model3d.shading),
        )
    }

    pub fn set_selected_model3d_shading(&mut self, project: &Project, shading: Model3dShading) {
        let Some(clip_id) = self.selected_model3d_clip_id(project) else {
            return;
        };
        if let Some(index) = self.ensure_property_row_for_clip(clip_id) {
            self.tracks[index.track].property_rows[index.row]
                .model3d
                .shading = shading;
        }
    }

    pub fn set_selected_model3d_value(&mut self, project: &Project, input: &str, value: GpuValue) {
        if let Some((binding, time)) = self.model3d_binding_mut(project, input) {
            binding.set_value(time, value);
        }
    }

    pub fn set_selected_model3d_component_linked(
        &mut self,
        project: &Project,
        input: &str,
        component: usize,
        value: f32,
        linked: bool,
    ) {
        if let Some((binding, time)) = self.model3d_binding_mut(project, input) {
            binding.set_component_value(time, component, value, linked);
        }
    }

    pub fn toggle_selected_model3d_keyframe(&mut self, project: &Project, input: &str) {
        if let Some((binding, time)) = self.model3d_binding_mut(project, input) {
            binding.toggle_keyframe(time);
        }
    }

    pub fn selected_model3d_has_keyframe(&self, project: &Project, input: &str) -> bool {
        self.model3d_binding(project, input)
            .is_some_and(|(binding, time)| binding.has_keyframe(time))
    }

    pub fn selected_model3d_has_keyframes(&self, project: &Project, input: &str) -> bool {
        self.model3d_binding(project, input)
            .is_some_and(|(binding, _)| binding.has_keyframes())
    }

    fn selected_composite(&self) -> Option<(&LayerComposite, f64)> {
        let time = self.playhead as f64;
        if let Some(clip) = self.selected_clip() {
            let composite = self
                .edit
                .document
                .property_row(clip.track, &clip.source, clip.source_instance)
                .map(|row| &row.composite)
                .unwrap_or(&clip.composite);
            return Some((composite, time));
        }
        self.selected_track().map(|track| (&track.composite, time))
    }

    fn selected_composite_mut(&mut self) -> Option<(&mut LayerComposite, f64)> {
        let time = self.playhead as f64;
        if let Some(id) = self.selected_clip_id() {
            let index = self.ensure_property_row_for_clip(id)?;
            return Some((
                &mut self.tracks[index.track].property_rows[index.row].composite,
                time,
            ));
        }
        Some((
            &mut self.edit.track_mut(self.selected_track?)?.composite,
            time,
        ))
    }

    fn composite_binding(composite: &LayerComposite, kind: CompositeBindingKind) -> &Binding {
        match kind {
            CompositeBindingKind::Opacity => &composite.opacity,
            CompositeBindingKind::BlendMode => &composite.blend_mode,
            CompositeBindingKind::AlphaBlendMode => &composite.alpha_blend_mode,
        }
    }

    fn composite_binding_mut(
        composite: &mut LayerComposite,
        kind: CompositeBindingKind,
    ) -> &mut Binding {
        match kind {
            CompositeBindingKind::Opacity => &mut composite.opacity,
            CompositeBindingKind::BlendMode => &mut composite.blend_mode,
            CompositeBindingKind::AlphaBlendMode => &mut composite.alpha_blend_mode,
        }
    }

    fn composite_has_keyframe(&self, kind: CompositeBindingKind) -> bool {
        self.selected_composite().is_some_and(|(composite, time)| {
            Self::composite_binding(composite, kind).has_keyframe(time)
        })
    }

    fn composite_has_keyframes(&self, kind: CompositeBindingKind) -> bool {
        self.selected_composite()
            .is_some_and(|(composite, _)| Self::composite_binding(composite, kind).has_keyframes())
    }

    fn toggle_composite_keyframe(&mut self, kind: CompositeBindingKind) {
        if let Some((composite, time)) = self.selected_composite_mut() {
            Self::composite_binding_mut(composite, kind).toggle_keyframe(time);
        }
    }

    pub fn set_selected_opacity(&mut self, value: f32) {
        if let Some((composite, time)) = self.selected_composite_mut() {
            Self::composite_binding_mut(composite, CompositeBindingKind::Opacity)
                .set_value(time, GpuValue::F32(value.clamp(0.0, 1.0)));
        }
    }

    composite_keyframe_methods!(
        selected_opacity_has_keyframe,
        selected_opacity_has_keyframes,
        toggle_selected_opacity_keyframe,
        CompositeBindingKind::Opacity
    );

    pub fn selected_opacity(&self) -> Option<f32> {
        self.selected_composite()
            .map(|(composite, time)| composite.opacity(time))
    }

    #[cfg(test)]
    fn cycle_selected_blend_mode(&mut self, direction: i32) {
        if let Some((composite, time)) = self.selected_composite_mut() {
            let current = composite
                .blend_mode
                .evaluate(time)
                .and_then(GpuValue::enum_index)
                .unwrap_or(0) as i32;
            let count = crate::project::BlendMode::ALL.len() as i32;
            Self::composite_binding_mut(composite, CompositeBindingKind::BlendMode).set_value(
                time,
                GpuValue::Enum((current + direction).rem_euclid(count) as u32),
            );
        }
    }

    pub fn selected_blend_mode(&self) -> Option<crate::project::BlendMode> {
        self.selected_composite()
            .map(|(composite, time)| composite.blend_mode(time))
    }

    pub fn set_selected_blend_mode(&mut self, index: usize) {
        if index >= crate::project::BlendMode::ALL.len() {
            return;
        }
        if let Some((composite, time)) = self.selected_composite_mut() {
            Self::composite_binding_mut(composite, CompositeBindingKind::BlendMode)
                .set_value(time, GpuValue::Enum(index as u32));
        }
    }

    composite_keyframe_methods!(
        selected_blend_mode_has_keyframe,
        selected_blend_mode_has_keyframes,
        toggle_selected_blend_mode_keyframe,
        CompositeBindingKind::BlendMode
    );

    pub fn selected_alpha_blend_mode(&self) -> Option<crate::project::AlphaBlendMode> {
        self.selected_composite()
            .map(|(composite, time)| composite.alpha_blend_mode(time))
    }

    pub fn set_selected_alpha_blend_mode(&mut self, index: usize) {
        if index >= crate::project::AlphaBlendMode::ALL.len() {
            return;
        }
        if let Some((composite, time)) = self.selected_composite_mut() {
            Self::composite_binding_mut(composite, CompositeBindingKind::AlphaBlendMode)
                .set_value(time, GpuValue::Enum(index as u32));
        }
    }

    composite_keyframe_methods!(
        selected_alpha_blend_mode_has_keyframe,
        selected_alpha_blend_mode_has_keyframes,
        toggle_selected_alpha_blend_mode_keyframe,
        CompositeBindingKind::AlphaBlendMode
    );

    fn selected_property_source(&self) -> Option<&VisualSource> {
        let clip = self.selected_clip()?;
        Some(
            self.edit
                .document
                .property_row(clip.track, &clip.source, clip.source_instance)
                .map(|row| &row.source)
                .unwrap_or(&clip.source),
        )
    }

    fn selected_property_source_mut(&mut self) -> Option<&mut VisualSource> {
        let id = self.selected_clip_id()?;
        let index = self.ensure_property_row_for_clip(id)?;
        Some(&mut self.tracks[index.track].property_rows[index.row].source)
    }

    pub fn selected_generator(&self) -> Option<&GeneratorSource> {
        match self.selected_property_source()? {
            VisualSource::Generator(generator) => Some(generator),
            _ => None,
        }
    }

    fn generator_gpu_binding_at(&self, input: &str) -> Option<(&Binding, f64)> {
        let (binding, time) = self.generator_host_binding_at(input)?;
        Some((binding.gpu()?, time))
    }

    fn generator_host_binding_at(&self, input: &str) -> Option<(&HostBinding, f64)> {
        let VisualSource::Generator(generator) = self.selected_property_source()? else {
            return None;
        };
        Some((generator.host_binding(input)?, self.playhead as f64))
    }

    fn generator_host_binding_at_mut(&mut self, input: &str) -> Option<(&mut HostBinding, f64)> {
        let time = self.playhead as f64;
        let VisualSource::Generator(generator) = self.selected_property_source_mut()? else {
            return None;
        };
        Some((generator.host_binding_mut(input)?, time))
    }

    pub fn generator_value(&self, input: &str) -> Option<GpuValue> {
        let (binding, time) = self.generator_gpu_binding_at(input)?;
        binding.evaluate(time)
    }

    pub fn generator_host_value(&self, input: &str) -> Option<HostValue> {
        let (binding, time) = self.generator_host_binding_at(input)?;
        binding.evaluate(time)
    }

    pub fn generator_has_keyframe(&self, input: &str) -> bool {
        self.generator_host_binding_at(input)
            .is_some_and(|(binding, time)| binding.has_keyframe(time))
    }

    pub fn generator_has_keyframes(&self, input: &str) -> bool {
        self.generator_host_binding_at(input)
            .is_some_and(|(binding, _)| binding.has_keyframes())
    }

    pub fn set_generator_value(&mut self, input: &str, value: GpuValue) {
        let time = self.playhead as f64;
        let Some(VisualSource::Generator(generator)) = self.selected_property_source_mut() else {
            return;
        };
        let binding = generator
            .parameters_mut()
            .entry(input.to_string())
            .or_insert_with(|| HostBinding::Gpu(Binding::Constant(value)));
        if let Some(binding) = binding.gpu_mut() {
            binding.set_value(time, value);
        }
    }

    pub fn set_generator_host_value(&mut self, input: &str, value: HostValue) {
        let time = self.playhead as f64;
        let Some(VisualSource::Generator(generator)) = self.selected_property_source_mut() else {
            return;
        };
        let previous_gradient_points = if input == "points" {
            generator
                .host_binding("points")
                .and_then(|binding| binding.evaluate(time))
                .and_then(|value| match value {
                    HostValue::Vec2Array(points) => Some(points),
                    _ => None,
                })
        } else {
            None
        };
        generator
            .parameters_mut()
            .entry(input.to_string())
            .or_insert_with(|| HostBinding::Constant(value.clone()))
            .set_value(time, value);
        sync_gradient_stop_parameters(generator, time, previous_gradient_points);
    }

    pub fn toggle_generator_keyframe(&mut self, input: &str) {
        if let Some((binding, time)) = self.generator_host_binding_at_mut(input) {
            binding.toggle_keyframe(time);
        }
    }

    pub fn selected_text(&self) -> Option<String> {
        self.selected_generator_host_string("text")
    }

    pub fn set_selected_text(&mut self, value: String) {
        self.set_selected_generator_host_string("text", value);
    }

    pub fn selected_font_family(&self) -> Option<String> {
        self.selected_generator_host_string("font_family")
    }

    pub fn set_selected_font_family(&mut self, value: String) {
        self.set_selected_generator_host_string("font_family", value);
    }

    fn selected_generator_host_string(&self, input: &str) -> Option<String> {
        let (binding, time) = self.generator_host_binding_at(input)?;
        match binding.evaluate(time)? {
            HostValue::String(value) => Some(value),
            _ => None,
        }
    }

    fn set_selected_generator_host_string(&mut self, input: &str, value: String) {
        if let Some((binding, time)) = self.generator_host_binding_at_mut(input) {
            binding.set_value(time, HostValue::String(value));
        }
    }

    pub(crate) fn track_mix(&self, track: u32) -> [f32; 2] {
        let Some(track) = self.tracks.iter().find(|candidate| candidate.id == track) else {
            return [1.0, 0.0];
        };
        [
            track
                .volume
                .evaluate(self.playhead as f64)
                .and_then(GpuValue::f32)
                .unwrap_or(1.0)
                .clamp(0.0, 1.0),
            track
                .pan
                .evaluate(self.playhead as f64)
                .and_then(GpuValue::f32)
                .unwrap_or(0.0)
                .clamp(-1.0, 1.0),
        ]
    }

    fn commit_mixer_exact(&mut self) {
        let Some(editor) = self.mixer_exact.take() else {
            return;
        };
        if let Ok(percent) = editor.value.trim().parse::<f32>() {
            self.set_track_mix(editor.track, editor.parameter, percent / 100.0);
        }
    }

    fn set_track_mix(&mut self, track: u32, parameter: MixerParameter, value: f32) {
        let time = self.playhead as f64;
        let Some(track) = self
            .tracks
            .iter_mut()
            .find(|candidate| candidate.id == track)
        else {
            return;
        };
        let (minimum, maximum, _) = parameter.limits();
        let binding = match parameter {
            MixerParameter::Volume => &mut track.volume,
            MixerParameter::Pan => &mut track.pan,
        };
        binding.set_value(time, GpuValue::F32(value.clamp(minimum, maximum)));
    }

    fn mixer_has_keyframe(&self, track: u32, parameter: MixerParameter) -> bool {
        self.tracks
            .iter()
            .find(|candidate| candidate.id == track)
            .is_some_and(|track| {
                let binding = match parameter {
                    MixerParameter::Volume => &track.volume,
                    MixerParameter::Pan => &track.pan,
                };
                binding.has_keyframe(self.playhead as f64)
            })
    }

    fn toggle_mixer_keyframe(&mut self, track: u32, parameter: MixerParameter) {
        let time = self.playhead as f64;
        if let Some(track) = self
            .tracks
            .iter_mut()
            .find(|candidate| candidate.id == track)
        {
            match parameter {
                MixerParameter::Volume => &mut track.volume,
                MixerParameter::Pan => &mut track.pan,
            }
            .toggle_keyframe(time);
        }
    }

    pub fn close_popups(&mut self) {
        self.context_menu = None;
        self.rename = None;
        self.mixer_exact = None;
        self.keyframe_value_editor = None;
    }

    pub fn set_focus(&mut self, stack: Option<StackId>) {
        if self.focused_stack != stack {
            self.context_menu = None;
            self.rename = None;
            self.mixer_exact = None;
            self.keyframe_value_editor = None;
        }
        self.focused_stack = stack;
    }

    pub fn tick(&mut self, snapshot: &LayoutSnapshot, frame_rate: f32) {
        self.frame_rate = frame_rate.max(1.0);
        for offset in self.track_offsets.values_mut() {
            *offset *= 0.72;
        }
        self.track_offsets.retain(|_, offset| offset.abs() > 0.25);

        let now = Instant::now();
        let elapsed = if std::mem::take(&mut self.playback_just_started) {
            0.0
        } else {
            now.saturating_duration_since(self.selection_frame)
                .as_secs_f32()
        };

        let dt = elapsed.min(0.05);
        self.selection_frame = now;

        for (track, amount) in &mut self.keyframe_track_expansion {
            let target = self.expanded_keyframe_tracks.contains(track) as u8 as f32;
            ease_chevron(amount, target, dt);
        }
        for (lane, amount) in &mut self.keyframe_lane_expansion {
            let target = self.expanded_keyframe_lanes.contains(lane) as u8 as f32;
            ease_chevron(amount, target, dt);
        }

        let accordion_animating =
            self.keyframe_track_expansion.iter().any(|(track, amount)| {
                let target = self.expanded_keyframe_tracks.contains(track) as u8 as f32;
                (*amount - target).abs() > 0.001
            }) || self.keyframe_lane_expansion.iter().any(|(lane, amount)| {
                let target = self.expanded_keyframe_lanes.contains(lane) as u8 as f32;
                (*amount - target).abs() > 0.001
            });

        if self.keyframe_lane_snapshot.len() != self.tracks.len() || !accordion_animating {
            let snapshot = (0..self.tracks.len())
                .map(|index| self.build_keyframe_lanes_for_track(index))
                .collect();
            self.keyframe_lane_snapshot = snapshot;
        }

        let mut keyframe_row_heights = std::mem::take(&mut self.keyframe_row_heights);
        keyframe_row_heights.clear();
        keyframe_row_heights.reserve(self.tracks.len());
        for index in 0..self.tracks.len() {
            let track_id = self.tracks[index].id;
            let track_open = self.keyframe_track_open_amount(track_id);
            let height = if track_open <= 0.001 {
                0.0
            } else {
                self.keyframe_property_groups(index)
                    .map(|(start, _)| {
                        self.keyframe_property_height(&self.keyframe_lanes_for_track(index)[start])
                    })
                    .sum::<f32>()
                    * track_open
            };
            keyframe_row_heights.insert(track_id, height);
        }
        self.keyframe_row_heights = keyframe_row_heights;

        let mut track_prefix_heights = std::mem::take(&mut self.track_prefix_heights);
        track_prefix_heights.clear();
        track_prefix_heights.reserve(self.tracks.len() + 1);
        track_prefix_heights.push(0.0);
        let mut total = 0.0;
        for index in 0..self.tracks.len() {
            let track_height = self.tracks[index].height;
            total += track_height + self.keyframe_rows_height(index);
            track_prefix_heights.push(total);
        }
        self.track_prefix_heights = track_prefix_heights;

        if matches!(self.drag, Some(Drag::Playhead)) {
            if let Some(layout) = self.focused_layout(snapshot) {
                let overflow = if self.cursor[0] < layout.body.x {
                    self.cursor[0] - layout.body.x
                } else if self.cursor[0] > layout.body.right() {
                    self.cursor[0] - layout.body.right()
                } else {
                    0.0
                };
                if overflow.abs() > 0.01 {
                    let scroll_pixels_per_second = overflow * 7.5;
                    self.scroll_time = (self.scroll_time
                        + scroll_pixels_per_second as f64 * dt as f64
                            / self.pixels_per_second.max(1.0) as f64)
                        .max(0.0);
                    let edge_x = self.cursor[0].clamp(layout.body.x, layout.body.right());
                    self.set_dragged_playhead(layout, self.time_at(layout, edge_x));
                }
            }
        }

        if self.playing && !self.is_scrubbing() {
            self.playhead += elapsed;
            if let Some(end) = self.end_time {
                if self.playhead >= end {
                    match self.end_behavior {
                        EndBehavior::Stop => {
                            self.playhead = end;
                            self.playing = false;
                        }
                        EndBehavior::Restart => self.playhead = 0.0,
                    }
                }
            }
            self.follow_playhead(snapshot);
        }
        let step = 1.0 - (-CLIP_SELECTION_FADE_SPEED * dt).exp();
        for id in &self.edit.selected {
            self.selection_levels.entry(*id).or_insert(0.0);
        }
        let selected = &self.edit.selected;
        self.selection_levels.retain(|id, level| {
            let target = if selected.contains(id) { 1.0 } else { 0.0 };
            *level += (target - *level) * step;
            if (*level - target).abs() < 0.002 {
                *level = target;
            }
            target > 0.0 || *level > 0.0
        });
        let values = self
            .tracks
            .iter()
            .filter(|track| track.kind == TrackKind::Audio)
            .flat_map(|track| {
                let [volume, pan] = self.track_mix(track.id);
                [
                    ((track.id, MixerParameter::Volume), volume),
                    ((track.id, MixerParameter::Pan), pan),
                ]
            })
            .collect::<Vec<_>>();
        for (key, value) in values {
            if let Some(knob) = self.mixer_knobs.get_mut(&key) {
                if !knob.is_dragging() {
                    knob.set_value(value as f64);
                }
                knob.tick(dt);
            }
        }
    }

    pub fn is_animating(&self) -> bool {
        !self.track_offsets.is_empty()
            || self.mixer_knobs.values().any(Knob::is_animating)
            || self.keyframe_track_expansion.iter().any(|(track, amount)| {
                let target = self.expanded_keyframe_tracks.contains(track) as u8 as f32;
                (*amount - target).abs() > 0.001
            })
            || self.keyframe_lane_expansion.iter().any(|(lane, amount)| {
                let target = self.expanded_keyframe_lanes.contains(lane) as u8 as f32;
                (*amount - target).abs() > 0.001
            })
            || self.selection_levels.iter().any(|(id, level)| {
                (*level - if self.selected.contains(id) { 1.0 } else { 0.0 }).abs() >= 0.002
            })
    }

    fn active_layout(
        snapshot: &LayoutSnapshot,
        point: [f32; 2],
    ) -> Option<(StackId, TimelineLayout)> {
        snapshot.stacks.iter().rev().find_map(|stack| {
            (stack.content.contains(point)
                && stack
                    .stack
                    .active_tab()
                    .is_some_and(|tab| tab.title == "Timeline"))
            .then_some((stack.stack.id, TimelineLayout::new(stack.content)))
        })
    }

    pub(crate) fn razor_cursor_at(&self, snapshot: &LayoutSnapshot, point: [f32; 2]) -> bool {
        self.tool == TimelineTool::Razor
            && Self::active_layout(snapshot, point)
                .is_some_and(|(_, layout)| layout.body.contains(point))
    }

    fn focused_layout(&self, snapshot: &LayoutSnapshot) -> Option<TimelineLayout> {
        let stack = snapshot.stack(self.focused_stack?)?;
        Some(TimelineLayout::new(stack.content))
    }

    fn visible_layout(snapshot: &LayoutSnapshot) -> Option<TimelineLayout> {
        snapshot.stacks.iter().find_map(|stack| {
            stack
                .stack
                .active_tab()
                .is_some_and(|tab| tab.title == "Timeline")
                .then_some(TimelineLayout::new(stack.content))
        })
    }

    fn visible_duration(&self, layout: TimelineLayout) -> f64 {
        layout.body.width as f64 / self.pixels_per_second as f64
    }

    fn overview_duration(&self, layout: TimelineLayout) -> f64 {
        let visible = self.visible_duration(layout);
        let extent = self
            .clips
            .iter()
            .map(Clip::end)
            .fold(self.playhead, f32::max)
            .max(self.end_time.unwrap_or(0.0))
            .max(60.0) as f64;
        extent.max(self.scroll_time + visible).max(visible)
    }

    fn track_prefix_height(&self, track: usize) -> f32 {
        self.track_prefix_heights
            .get(track)
            .copied()
            .unwrap_or_else(|| {
                (0..track.min(self.tracks.len()))
                    .map(|index| self.display_track_height(index))
                    .sum()
            })
    }

    fn track_rows(&self, layout: TimelineLayout) -> Vec<Rect> {
        (0..self.tracks.len())
            .map(|index| {
                Rect::new(
                    layout.body.x,
                    layout.body.y - self.scroll_y + self.track_prefix_height(index),
                    layout.body.width,
                    self.display_track_height(index),
                )
            })
            .collect()
    }

    fn track_base_y(&self, layout: TimelineLayout, track: usize) -> f32 {
        layout.body.y - self.scroll_y + self.track_prefix_height(track)
    }

    fn track_y(&self, layout: TimelineLayout, track: usize) -> f32 {
        let id = self.tracks[track].id;
        if let Some(Drag::Track {
            id: dragged,
            grab_y,
            current_y,
            origin_y,
            heights,
        }) = &self.drag
        {
            let base = layout.body.y - self.scroll_y
                + self.tracks[..track]
                    .iter()
                    .map(|track| heights.get(&track.id).copied().unwrap_or(track.height))
                    .sum::<f32>();
            if *dragged == id {
                let height = heights
                    .get(&id)
                    .copied()
                    .unwrap_or(self.tracks[track].height);
                let max_y = (layout.body.bottom() - height).max(layout.body.y);
                return (*current_y - *origin_y + layout.rect.y - *grab_y)
                    .clamp(layout.body.y, max_y);
            }
            return base + self.track_offsets.get(&id).copied().unwrap_or(0.0);
        }
        self.track_base_y(layout, track) + self.track_offsets.get(&id).copied().unwrap_or(0.0)
    }

    fn clip_rect(&self, layout: TimelineLayout, clip: &Clip) -> Rect {
        let track = clip.track_index(&self.tracks).unwrap_or(0);
        let y = self.track_y(layout, track) + CLIP_PAD;
        Rect {
            x: self.time_x(layout, clip.start),
            y,
            width: clip.duration * self.pixels_per_second,
            height: (self.tracks[track].height - CLIP_PAD * 2.0).max(1.0),
        }
    }

    fn track_at(&self, layout: TimelineLayout, y: f32) -> Option<usize> {
        self.track_rows(layout)
            .iter()
            .position(|row| y >= row.y && y < row.bottom())
    }

    fn header_track_at(&self, layout: TimelineLayout, point: [f32; 2]) -> Option<usize> {
        if point[0] >= layout.body.x || point[1] < layout.body.y || point[1] >= layout.body.bottom()
        {
            return None;
        }
        (0..self.tracks.len()).rev().find(|&index| {
            let y = self.track_y(layout, index);
            point[1] >= y && point[1] < y + self.display_track_height(index)
        })
    }

    fn track_row_rect(&self, layout: TimelineLayout, index: usize) -> Rect {
        crate::ui_layout::fit_column_at(
            layout.body,
            [layout.body.x, self.track_y(layout, index)],
            layout.body.width,
            &[crate::ui_layout::Item::height(
                self.display_track_height(index),
            )],
            0.0,
            0.0,
        )
        .1[0]
    }

    fn track_header_rect(&self, layout: TimelineLayout, index: usize) -> Rect {
        crate::ui_layout::fit_column_at(
            layout.header_body,
            [layout.header_body.x, self.track_y(layout, index)],
            layout.header_body.width,
            &[crate::ui_layout::Item::height(self.tracks[index].height)],
            0.0,
            0.0,
        )
        .1[0]
    }

    fn mixer_control_rects(
        &self,
        layout: TimelineLayout,
        index: usize,
        parameter: MixerParameter,
    ) -> (Rect, Rect) {
        let track = self.track_header_rect(layout, index);
        let vertical = crate::ui_layout::column(
            track,
            &[
                crate::ui_layout::Item::height(27.0),
                crate::ui_layout::Item::height(24.0),
                crate::ui_layout::Item::fill(),
            ],
            0.0,
            0.0,
            ui::Align::Start,
            None,
        );
        let controls = crate::ui_layout::row(
            vertical[1],
            &[
                crate::ui_layout::Item::width(23.0),
                crate::ui_layout::Item::width(8.0),
                crate::ui_layout::Item::width(53.0),
                crate::ui_layout::Item::width(4.0),
                crate::ui_layout::Item::width(8.0),
                crate::ui_layout::Item::width(53.0),
                crate::ui_layout::Item::fill(),
            ],
            0.0,
            0.0,
            ui::Align::Start,
        );
        match parameter {
            MixerParameter::Volume => (controls[1], controls[2]),
            MixerParameter::Pan => (controls[4], controls[5]),
        }
    }

    fn mixer_knob_rect(
        &self,
        layout: TimelineLayout,
        index: usize,
        parameter: MixerParameter,
    ) -> Rect {
        self.mixer_control_rects(layout, index, parameter).1
    }

    fn header_top_parts(&self, layout: TimelineLayout, index: usize) -> Vec<Rect> {
        let track = self.track_header_rect(layout, index);
        let columns = crate::ui_layout::row(
            track,
            &[
                crate::ui_layout::Item::width(TRACK_HANDLE_W),
                crate::ui_layout::Item::fill(),
            ],
            TRACK_HEADER_GAP,
            TRACK_HEADER_PAD + 1.0,
            ui::Align::Start,
        );
        let top = crate::ui_layout::column(
            columns[1],
            &[
                crate::ui_layout::Item::height(TRACK_TOP_H),
                crate::ui_layout::Item::fill(),
            ],
            TRACK_HEADER_GAP,
            0.0,
            ui::Align::Start,
            None,
        )[0];
        crate::ui_layout::row(
            top,
            &[
                crate::ui_layout::Item::width(TRACK_LABEL_W),
                crate::ui_layout::Item::width(TRACK_NAME_W),
                crate::ui_layout::Item::fill(),
                crate::ui_layout::Item::width(TRACK_BUTTON_W),
                crate::ui_layout::Item::width(TRACK_BUTTON_W),
            ],
            TRACK_HEADER_GAP,
            0.0,
            ui::Align::Start,
        )
    }

    fn header_button_rect(&self, layout: TimelineLayout, index: usize, solo: bool) -> Rect {
        self.header_top_parts(layout, index)[if solo { 4 } else { 3 }]
    }

    fn header_name_rect(&self, layout: TimelineLayout, index: usize) -> Rect {
        self.header_top_parts(layout, index)[1]
    }

    fn track_is_muted(&self, index: usize) -> bool {
        let track = &self.tracks[index];
        track.muted
            || (!track.solo
                && self
                    .tracks
                    .iter()
                    .any(|candidate| candidate.kind == track.kind && candidate.solo))
    }

    fn time_at(&self, layout: TimelineLayout, x: f32) -> f32 {
        (self.scroll_time + (x - layout.body.x) as f64 / self.pixels_per_second as f64).max(0.0)
            as f32
    }

    fn time_x(&self, layout: TimelineLayout, time: f32) -> f32 {
        layout.body.x + ((time as f64 - self.scroll_time) * self.pixels_per_second as f64) as f32
    }

    fn set_playhead(&mut self, time: f32) {
        self.playhead = if self.frame_snap {
            (time * self.frame_rate).round() / self.frame_rate
        } else {
            time
        };
    }

    fn set_dragged_playhead(&mut self, layout: TimelineLayout, time: f32) {
        if self.playhead_snap {
            let snapped = self.snap_time_with_options(layout, time, &[], false);
            if !self.snap_times.is_empty() {
                self.playhead = snapped.max(0.0);
                return;
            }
        }
        self.set_playhead(time);
    }

    fn seek_by(&mut self, seconds: f32) {
        self.set_playhead((self.playhead + seconds).max(0.0));
    }

    fn step_frames(&mut self, frames: i32) {
        let fps = self.frame_rate.max(1.0) as f64;
        let delta = frames as f64 / fps;
        self.playhead = ((self.playhead.max(0.0) as f64 + delta).max(0.0)) as f32;
    }

    fn jump_playhead(&mut self, snapshot: &LayoutSnapshot, target: JumpTarget) {
        let content_end = self.clips.iter().map(Clip::end).fold(0.0, f32::max);
        let time = match target {
            JumpTarget::TimelineStart => 0.0,
            JumpTarget::ContentStart => self
                .clips
                .iter()
                .map(|clip| clip.start)
                .reduce(f32::min)
                .unwrap_or(0.0),
            JumpTarget::ContentEnd => content_end,
            JumpTarget::TimelineEnd => self.end_time.unwrap_or(content_end),
        };
        self.set_playhead(time);
        if let Some(layout) = self.focused_layout(snapshot) {
            self.scroll_time =
                (self.playhead as f64 - self.visible_duration(layout) * 0.5).max(0.0);
        }
    }

    fn transport_parts(layout: TimelineLayout) -> (Rect, Vec<Rect>) {
        let controls = crate::ui_layout::column(
            layout.corner,
            &[
                crate::ui_layout::Item::height(4.0),
                crate::ui_layout::Item::height(TRANSPORT_BUTTON_H),
                crate::ui_layout::Item::fill(),
            ],
            0.0,
            0.0,
            ui::Align::Start,
            None,
        )[1];
        let parts = crate::ui_layout::row(
            controls,
            &[
                crate::ui_layout::Item::width(4.0),
                crate::ui_layout::Item::width(50.0),
                crate::ui_layout::Item::fill(),
                crate::ui_layout::Item::width(TRANSPORT_BUTTON_W),
                crate::ui_layout::Item::width(TRANSPORT_BUTTON_W),
                crate::ui_layout::Item::width(TRANSPORT_BUTTON_W),
                crate::ui_layout::Item::width(TRANSPORT_BUTTON_W),
            ],
            TRANSPORT_BUTTON_GAP,
            0.0,
            ui::Align::Start,
        );
        (controls, parts)
    }

    fn transport_button_rect(layout: TimelineLayout, index: usize) -> Rect {
        Self::transport_parts(layout).1[3 + index]
    }

    fn clips_at(&self, layout: TimelineLayout, point: [f32; 2]) -> Vec<(usize, Rect)> {
        self.clips
            .iter()
            .enumerate()
            .rev()
            .filter_map(|(index, clip)| {
                let rect = self.clip_rect(layout, clip);
                (rect.contains(point) && intersects(rect, layout.body)).then_some((index, rect))
            })
            .collect()
    }

    fn clip_at(&self, layout: TimelineLayout, point: [f32; 2]) -> Option<(usize, Rect)> {
        self.clips_at(layout, point).into_iter().next()
    }

    fn grouped_ids(&self, id: u32) -> Vec<u32> {
        let group = self
            .clips
            .iter()
            .find(|clip| clip.id == id)
            .and_then(|clip| clip.group);
        group.map_or_else(
            || vec![id],
            |group| {
                self.clips
                    .iter()
                    .filter(|clip| clip.group == Some(group))
                    .map(|clip| clip.id)
                    .collect()
            },
        )
    }

    fn select_clip(&mut self, id: u32, additive: bool) {
        let ids = self.grouped_ids(id);
        if additive {
            let remove = ids.iter().all(|id| self.selected.contains(id));
            for grouped in ids {
                if remove {
                    self.selected.remove(&grouped);
                } else {
                    self.selected.insert(grouped);
                }
            }
        } else if !ids.iter().all(|grouped| self.selected.contains(grouped)) {
            self.selected.clear();
            self.selected.extend(ids);
        }
        self.primary_selected = self
            .selected
            .contains(&id)
            .then_some(id)
            .or_else(|| self.selected.iter().copied().min());
    }

    fn replace_selection_with_clip(&mut self, id: u32) {
        let ids = self.grouped_ids(id);
        self.selected.clear();
        self.selected.extend(ids);
        self.primary_selected = self
            .selected
            .contains(&id)
            .then_some(id)
            .or_else(|| self.selected.iter().copied().min());
    }

    fn multi_selection_click_target(&self, id: u32) -> Option<u32> {
        if !self.selected.contains(&id) {
            return None;
        }
        let clicked_group = self.grouped_ids(id);
        self.selected
            .iter()
            .any(|selected| !clicked_group.contains(selected))
            .then_some(id)
    }

    fn context_rect(layout: TimelineLayout, menu: ContextMenu, item_count: usize) -> Rect {
        widgets::context_menu_rect(layout.rect, menu.point, item_count)
    }

    fn mixer_exact_rect(layout: TimelineLayout, editor: &MixerExactEditor) -> Rect {
        let anchor = Rect::new(
            layout.rect.x + editor.point[0],
            layout.rect.y + editor.point[1],
            1.0,
            1.0,
        );
        ui::place_popup(anchor, [188.0, 58.0], layout.rect, false, 4.0)
    }

    fn keyframe_value_editor_rect(layout: TimelineLayout, editor: &KeyframeValueEditor) -> Rect {
        let anchor = Rect::new(
            layout.rect.x + editor.point[0],
            layout.rect.y + editor.point[1],
            1.0,
            1.0,
        );
        ui::place_popup(anchor, [210.0, 88.0], layout.rect, false, 4.0)
    }

    fn keyframe_value_set_rect(layout: TimelineLayout, editor: &KeyframeValueEditor) -> Rect {
        let popup = Self::keyframe_value_editor_rect(layout, editor);
        Rect::new(popup.right() - 58.0, popup.bottom() - 30.0, 48.0, 22.0)
    }

    fn commit_keyframe_value_editor(&mut self) {
        let Some(editor) = self.keyframe_value_editor.take() else {
            return;
        };
        if let Ok(value) = editor.value.trim().parse::<f32>() {
            if value.is_finite() {
                self.set_selected_keyframe_value(value);
            }
        }
    }

    fn context_click(
        &mut self,
        layout: TimelineLayout,
        point: [f32; 2],
        project: &Project,
    ) -> bool {
        let Some(menu) = self.context_menu else {
            return false;
        };
        if menu.stack != self.focused_stack.unwrap_or(menu.stack) {
            self.context_menu = None;
            return false;
        }
        let items = context_items(menu.kind, self.has_compatible_replacement_media(project));
        let rect = Self::context_rect(layout, menu, items.len());
        match widgets::context_menu_click(rect, point, &items) {
            widgets::ContextMenuClick::Action(command) => {
                self.context_menu = None;
                self.execute_context(menu.kind, menu.point, command);
                true
            }
            widgets::ContextMenuClick::Disabled => {
                self.context_menu = None;
                true
            }
            widgets::ContextMenuClick::Outside => {
                self.context_menu = None;
                false
            }
        }
    }

    pub(crate) fn apply_action(
        &mut self,
        action: TimelineAction,
        snapshot: &LayoutSnapshot,
    ) -> bool {
        match action {
            TimelineAction::InsertVideoClip { .. }
            | TimelineAction::InsertEffectClip { .. }
            | TimelineAction::SpeedDuration
            | TimelineAction::ReplaceSelectedClips
            | TimelineAction::AddSelectionToComposition => {
                return false;
            }
            TimelineAction::CopySelection => self.copy_selection(),
            TimelineAction::CutSelection => self.cut_selection(),
            TimelineAction::Paste => self.paste(),
            TimelineAction::PowerDuplicate => self.power_duplicate(),
            TimelineAction::DeleteSelection => self.delete_selection(),
            TimelineAction::SelectBeforePlayhead => self.select_on_current_track(snapshot, false),
            TimelineAction::SelectAfterPlayhead => self.select_on_current_track(snapshot, true),
            TimelineAction::GroupSelection => self.group_selection(),
            TimelineAction::UngroupSelection => self.ungroup_selection(),
            TimelineAction::CloseGap => self.close_selected_gaps(),
            TimelineAction::ToggleRazorTool => self.toggle_razor_tool(),
            TimelineAction::CutAtPlayhead => self.cut_at_playhead(),
            TimelineAction::CutClipAt { clip, time } => self.cut_clip_at(clip, time),
            TimelineAction::TogglePlayback => self.toggle_playback(),
            TimelineAction::SeekBy(seconds) => self.seek_relative(seconds),
            TimelineAction::StepFrames(frames) => self.step_relative_frames(frames),
            TimelineAction::JumpTimelineStart => {
                self.jump_playhead(snapshot, JumpTarget::TimelineStart)
            }
            TimelineAction::JumpContentStart => {
                self.jump_playhead(snapshot, JumpTarget::ContentStart)
            }
            TimelineAction::JumpContentEnd => self.jump_playhead(snapshot, JumpTarget::ContentEnd),
            TimelineAction::JumpTimelineEnd => {
                self.jump_playhead(snapshot, JumpTarget::TimelineEnd)
            }
            TimelineAction::SetEnd => self.end_time = Some(self.playhead),
            TimelineAction::InsertAudio { time, near } => {
                self.insert_clip(TrackKind::Audio, time, near)
            }
            TimelineAction::RenameTrack(track) => self.begin_rename(track),
            TimelineAction::DeleteTrack(track) => self.delete_track(track),
            TimelineAction::AddTrack { kind, near } => {
                self.add_track(kind, near);
            }
            TimelineAction::BeginMixerExact {
                point,
                track,
                parameter,
            } => {
                let percent = match parameter {
                    MixerParameter::Volume => self.track_mix(track)[0],
                    MixerParameter::Pan => self.track_mix(track)[1],
                } * 100.0;
                let value = if (percent - percent.round()).abs() < 0.005 {
                    format!("{percent:.0}")
                } else {
                    format!("{percent:.2}")
                };
                self.mixer_exact = Some(MixerExactEditor {
                    stack: self.focused_stack.unwrap_or(StackId(0)),
                    point,
                    track,
                    parameter,
                    value,
                    replace_on_input: true,
                });
            }
            TimelineAction::ToggleMixerKeyframe { track, parameter } => {
                self.toggle_mixer_keyframe(track, parameter)
            }
            TimelineAction::ToggleEndBehavior => {
                self.end_behavior = match self.end_behavior {
                    EndBehavior::Stop => EndBehavior::Restart,
                    EndBehavior::Restart => EndBehavior::Stop,
                }
            }
            TimelineAction::ToggleFrameSnap => {
                self.frame_snap = !self.frame_snap;
                if self.frame_snap {
                    self.set_playhead(self.playhead);
                }
            }
            TimelineAction::ToggleGridSnap => self.grid_snap = !self.grid_snap,
            TimelineAction::ToggleClipSnap => self.clip_snap = !self.clip_snap,
            TimelineAction::TogglePlayheadSnap => self.playhead_snap = !self.playhead_snap,
            TimelineAction::ToggleFollowPlayhead => self.follow_playhead = !self.follow_playhead,
            TimelineAction::ToggleTrackMute(id) => {
                if let Some(track) = self.tracks.iter_mut().find(|track| track.id == id) {
                    track.muted = !track.muted;
                }
            }
            TimelineAction::ToggleTrackSolo(id) => {
                if let Some(track) = self.tracks.iter_mut().find(|track| track.id == id) {
                    track.solo = !track.solo;
                }
            }
        }
        true
    }

    pub(crate) fn copy_selection(&mut self) {
        let mut clips = self
            .clips
            .iter()
            .filter(|clip| self.selected.contains(&clip.id))
            .filter_map(|clip| {
                let track_index = self.track_index(clip.track)?;
                let track_kind = self.tracks[track_index].kind;
                let track_rank = self.track_rank_of_kind(clip.track, track_kind)?;
                let properties = self
                    .document
                    .property_row(clip.track, &clip.source, clip.source_instance)
                    .cloned()
                    .unwrap_or_else(|| LayerPropertyRow {
                        source: clip.source.clone(),
                        source_instance: clip.source_instance,
                        pipeline: clip.pipeline.clone(),
                        composite: clip.composite.clone(),
                        model3d: clip.model3d.clone(),
                    });
                Some(ClipboardClip {
                    clip: clip.clone(),
                    properties,
                    track_rank,
                    track_kind,
                })
            })
            .collect::<Vec<_>>();
        clips.sort_by(|left, right| {
            left.clip
                .start
                .total_cmp(&right.clip.start)
                .then_with(|| left.clip.id.cmp(&right.clip.id))
        });
        if !clips.is_empty() {
            self.clipboard = clips;
        }
    }

    pub(crate) fn cut_selection(&mut self) {
        if self.selected.is_empty() {
            return;
        }
        self.copy_selection();
        self.delete_selected();
    }

    pub(crate) fn paste(&mut self) {
        let Some(origin) = self
            .clipboard
            .iter()
            .map(|entry| entry.clip.start)
            .min_by(f32::total_cmp)
        else {
            return;
        };
        let clipboard = self.clipboard.clone();
        let initialize_end = self.end_time.is_none();
        let first_new_clip = self.clips.len();
        let mut groups = HashMap::new();
        let mut source_instances = HashMap::new();
        let mut pasted = Vec::new();
        let mut intervals = TrackIntervals::from_clips(&self.clips, &HashSet::new());
        for entry in clipboard {
            let ClipboardClip {
                mut clip,
                mut properties,
                track_rank,
                track_kind: kind,
            } = entry;
            clip.id = self.next_clip;
            self.next_clip += 1;
            if source_requires_instance(&clip.source) {
                let old = clip.source_instance;
                let source_instance = if let Some(existing) = source_instances.get(&old) {
                    *existing
                } else {
                    let next = self.allocate_source_instance();
                    source_instances.insert(old, next);
                    next
                };
                clip.source_instance = source_instance;
                properties.source_instance = source_instance;
            }
            let old_start = clip.start;
            clip.start = (self.playhead + clip.start - origin).max(0.0);

            let property_time_delta = (clip.start - old_start) as f64;
            let preferred_track = self.track_at_kind_rank(kind, track_rank);
            let wanted = self.track_index(preferred_track).unwrap_or(0);
            clip.track =
                self.find_available_track(kind, wanted, clip.start, clip.end(), &intervals);
            intervals.insert(clip.track, clip.start, clip.end());
            clip.group = clip
                .group
                .map(|group| *groups.entry(group).or_insert_with(|| self.next_group_id()));
            properties.shift_keyframes(property_time_delta);
            if let Some(track) = self.tracks.iter_mut().find(|track| track.id == clip.track) {
                if track
                    .property_row(&properties.source, properties.source_instance)
                    .is_none()
                {
                    track.property_rows.push(properties);
                }
            }
            pasted.push(clip.id);
            self.clips.push(clip);
        }
        if initialize_end {
            self.set_initial_end_from_clips_since(first_new_clip);
        }
        self.selected = pasted.iter().copied().collect();
        self.primary_selected = pasted.first().copied();
    }

    pub(crate) fn delete_selection(&mut self) {
        self.delete_selected();
    }

    pub(crate) fn group_selection(&mut self) {
        self.group_selected();
    }

    pub(crate) fn group_clip_ids(&mut self, ids: &[u32]) {
        if ids.len() < 2 {
            return;
        }
        let selected = ids.iter().copied().collect::<HashSet<_>>();
        let group = self.next_group_id();
        for clip in &mut self.document.clips {
            if selected.contains(&clip.id) {
                clip.group = Some(group);
            }
        }
    }

    pub(crate) fn ungroup_selection(&mut self) {
        self.ungroup_selected();
    }

    pub(crate) fn toggle_razor_tool(&mut self) {
        self.tool = match self.tool {
            TimelineTool::Select => TimelineTool::Razor,
            TimelineTool::Razor => TimelineTool::Select,
        };
    }

    fn cut_at_playhead(&mut self) {
        let crossing = |clip: &Clip, time: f32| {
            time > clip.start + MIN_CLIP * 0.5 && time < clip.end() - MIN_CLIP * 0.5
        };
        let ids = if self.selected.is_empty() {
            self.clips
                .iter()
                .filter(|clip| crossing(clip, self.playhead))
                .map(|clip| clip.id)
                .collect::<Vec<_>>()
        } else {
            self.clips
                .iter()
                .filter(|clip| self.selected.contains(&clip.id) && crossing(clip, self.playhead))
                .map(|clip| clip.id)
                .collect::<Vec<_>>()
        };
        self.split_clip_ids_at(&ids, self.playhead);
    }

    fn cut_clip_at(&mut self, clip: u32, time: f32) {
        let Some(group) = self
            .clips
            .iter()
            .find(|candidate| candidate.id == clip)
            .and_then(|clip| clip.group)
        else {
            self.split_clip_ids_at(&[clip], time);
            return;
        };
        let ids = self
            .clips
            .iter()
            .filter(|candidate| candidate.group == Some(group))
            .map(|candidate| candidate.id)
            .collect::<Vec<_>>();
        self.split_clip_ids_at(&ids, time);
    }

    fn split_clip_ids_at(&mut self, ids: &[u32], time: f32) {
        let targets = ids.iter().copied().collect::<HashSet<_>>();
        let previous_primary = self.primary_selected;
        let split_groups = self
            .clips
            .iter()
            .filter(|clip| {
                targets.contains(&clip.id)
                    && time > clip.start + MIN_CLIP * 0.5
                    && time < clip.end() - MIN_CLIP * 0.5
            })
            .filter_map(|clip| clip.group)
            .collect::<HashSet<_>>();
        let mut right_groups = HashMap::new();
        for group in split_groups {
            right_groups.insert(group, self.next_group_id());
        }

        let mut right_halves = Vec::new();
        let mut right_ids = Vec::new();
        let mut right_primary = None;
        let mut next_clip = self.document.next_clip;
        for clip in &mut self.document.clips {
            if !targets.contains(&clip.id)
                || time <= clip.start + MIN_CLIP * 0.5
                || time >= clip.end() - MIN_CLIP * 0.5
            {
                continue;
            }
            let old_end = clip.end();
            let left_duration = time - clip.start;
            let mut right = clip.clone();
            right.id = next_clip;
            next_clip = next_clip.saturating_add(1).max(1);
            right.group = clip
                .group
                .and_then(|group| right_groups.get(&group).copied());
            right.start = time;
            right.duration = old_end - time;
            right.source_offset = clip.source_offset + left_duration * clip.speed.max(0.01);
            right.fade_in = 0.0;
            right.fade_out = clip.fade_out.min(right.duration);
            clip.duration = left_duration;
            clip.fade_in = clip.fade_in.min(clip.duration);
            clip.fade_out = 0.0;
            if previous_primary == Some(clip.id) {
                right_primary = Some(right.id);
            }
            right_ids.push(right.id);
            right_halves.push(right);
        }
        self.document.next_clip = next_clip;
        if right_halves.is_empty() {
            return;
        }
        self.clips.extend(right_halves);

        self.selected = right_ids.iter().copied().collect();
        self.primary_selected = right_primary.or_else(|| right_ids.first().copied());
        self.selected_track = None;
    }

    fn close_selected_gaps(&mut self) {
        if self.selected.len() < 2 {
            return;
        }

        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        enum Unit {
            Group(u32),
            Clip(u32),
        }

        let unit_for = |clip: &Clip| clip.group.map(Unit::Group).unwrap_or(Unit::Clip(clip.id));
        let selected_units = self
            .clips
            .iter()
            .filter(|clip| self.selected.contains(&clip.id))
            .map(&unit_for)
            .collect::<HashSet<_>>();
        if selected_units.len() < 2 {
            return;
        }

        let mut units = selected_units
            .into_iter()
            .filter_map(|unit| {
                self.clips
                    .iter()
                    .filter(|clip| unit_for(clip) == unit)
                    .map(|clip| clip.start)
                    .min_by(f32::total_cmp)
                    .map(|start| (unit, start))
            })
            .collect::<Vec<_>>();
        units.sort_by(|left, right| {
            left.1
                .total_cmp(&right.1)
                .then_with(|| left.0.cmp(&right.0))
        });

        let mut previous_end = HashMap::<u32, f32>::new();
        for (unit, _) in units {
            let members = self
                .clips
                .iter()
                .filter(|clip| unit_for(clip) == unit)
                .map(|clip| (clip.track, clip.start))
                .collect::<Vec<_>>();

            let mut delta = members
                .iter()
                .filter_map(|(track, start)| previous_end.get(track).map(|end| *end - *start))
                .max_by(f32::total_cmp);

            if let Some(mut move_by) = delta.take() {
                move_by = move_by.min(0.0);
                for (member_track, member_start) in &members {
                    let blocker_end = self
                        .clips
                        .iter()
                        .filter(|clip| clip.track == *member_track && unit_for(clip) != unit)
                        .filter(|clip| clip.end() <= *member_start + MIN_CLIP * 0.5)
                        .map(Clip::end)
                        .max_by(f32::total_cmp)
                        .unwrap_or(0.0);
                    move_by = move_by.max(blocker_end - *member_start);
                    move_by = move_by.max(-*member_start);
                }

                if move_by < -MIN_CLIP * 0.5 {
                    for clip in &mut self.clips {
                        if unit_for(clip) == unit {
                            clip.start += move_by;
                        }
                    }
                }
            }

            for clip in self.clips.iter().filter(|clip| unit_for(clip) == unit) {
                previous_end
                    .entry(clip.track)
                    .and_modify(|end| *end = (*end).max(clip.end()))
                    .or_insert_with(|| clip.end());
            }
        }
    }

    fn duplicate_selection(&mut self) -> Vec<u32> {
        if self.selected.is_empty() {
            return Vec::new();
        }
        let selected = self.selected.clone();
        let mut groups = HashMap::<u32, u32>::new();
        let originals = self
            .clips
            .iter()
            .filter(|clip| selected.contains(&clip.id))
            .cloned()
            .collect::<Vec<_>>();
        let mut duplicated = Vec::with_capacity(originals.len());
        for mut clip in originals {
            clip.id = self.next_clip;
            self.next_clip = self.next_clip.saturating_add(1).max(1);
            clip.group = clip
                .group
                .map(|old| *groups.entry(old).or_insert_with(|| self.next_group_id()));
            duplicated.push(clip.id);
            self.clips.push(clip);
        }
        self.selected = duplicated.iter().copied().collect();
        self.primary_selected = duplicated.first().copied();
        duplicated
    }

    fn duplicate_selection_for_drag(&mut self) {
        let _ = self.duplicate_selection();
        self.power_duplicate = None;
    }

    fn selected_placements(&self) -> Vec<DuplicatePlacement> {
        self.clips
            .iter()
            .filter(|clip| self.selected.contains(&clip.id))
            .map(|clip| DuplicatePlacement {
                id: clip.id,
                start: clip.start,
                track: clip.track,
            })
            .collect()
    }

    fn track_rank_of_kind(&self, track: u32, kind: TrackKind) -> Option<usize> {
        self.tracks
            .iter()
            .filter(|candidate| candidate.kind == kind)
            .position(|candidate| candidate.id == track)
    }

    fn track_at_kind_rank(&mut self, kind: TrackKind, rank: usize) -> u32 {
        loop {
            if let Some(track) = self
                .tracks
                .iter()
                .filter(|track| track.kind == kind)
                .nth(rank)
                .map(|track| track.id)
            {
                return track;
            }
            let insert = self
                .tracks
                .iter()
                .rposition(|track| track.kind == kind)
                .map_or(self.tracks.len(), |index| index + 1);
            self.add_track_at(kind, insert);
        }
    }

    pub(crate) fn power_duplicate(&mut self) {
        let selected = self.selected_placements();
        if selected.is_empty() {
            self.power_duplicate = None;
            return;
        }

        let can_repeat = self.power_duplicate.as_ref().is_some_and(|state| {
            state.duplicates.len() == self.selected.len()
                && state.duplicates.iter().all(|id| self.selected.contains(id))
                && state.source.len() == state.duplicates.len()
        });
        if !can_repeat {
            let source = selected;
            let duplicates = self.duplicate_selection();
            self.power_duplicate =
                (!duplicates.is_empty()).then_some(PowerDuplicateState { source, duplicates });
            return;
        }

        let state = self.power_duplicate.take().unwrap();
        let current = state
            .duplicates
            .iter()
            .filter_map(|id| {
                self.clips
                    .iter()
                    .find(|clip| clip.id == *id)
                    .map(|clip| DuplicatePlacement {
                        id: clip.id,
                        start: clip.start,
                        track: clip.track,
                    })
            })
            .collect::<Vec<_>>();
        if current.len() != state.source.len() {
            self.power_duplicate = None;
            return;
        }

        let mut desired = Vec::with_capacity(current.len());
        for (source, current) in state.source.iter().zip(&current) {
            let Some(current_clip) = self
                .clips
                .iter()
                .find(|clip| clip.id == current.id)
                .cloned()
            else {
                self.power_duplicate = None;
                return;
            };
            let kind = self
                .tracks
                .iter()
                .find(|track| track.id == current.track)
                .map(|track| track.kind)
                .unwrap_or(TrackKind::Video);
            let source_rank = self.track_rank_of_kind(source.track, kind).unwrap_or(0) as isize;
            let current_rank = self.track_rank_of_kind(current.track, kind).unwrap_or(0) as isize;
            let rank_delta = current_rank - source_rank;
            let next_rank = (current_rank + rank_delta).max(0) as usize;
            desired.push((
                current_clip,
                (current.start + (current.start - source.start)).max(0.0),
                kind,
                next_rank,
            ));
        }

        let mut groups = HashMap::<u32, u32>::new();
        let mut duplicates = Vec::with_capacity(desired.len());
        for (mut clip, start, kind, track_rank) in desired {
            let previous_track = clip.track;
            clip.id = self.next_clip;
            self.next_clip = self.next_clip.saturating_add(1).max(1);
            clip.start = start;
            clip.track = self.track_at_kind_rank(kind, track_rank);
            clip.group = clip
                .group
                .map(|old| *groups.entry(old).or_insert_with(|| self.next_group_id()));
            let id = clip.id;
            duplicates.push(id);
            self.clips.push(clip);
            let _ = self.ensure_property_row_for_moved_clip(id, previous_track);
        }
        self.selected = duplicates.iter().copied().collect();
        self.primary_selected = duplicates.first().copied();
        self.selected_track = None;
        self.power_duplicate = Some(PowerDuplicateState {
            source: current,
            duplicates,
        });
    }

    fn select_on_current_track(&mut self, snapshot: &LayoutSnapshot, after: bool) {
        let track = self
            .selected_clip()
            .map(|clip| clip.track)
            .or(self.selected_track)
            .or_else(|| {
                Self::active_layout(snapshot, self.cursor)
                    .and_then(|(_, layout)| self.track_at(layout, self.cursor[1]))
                    .and_then(|index| self.tracks.get(index).map(|track| track.id))
            })
            .or_else(|| self.tracks.first().map(|track| track.id));
        let Some(track) = track else { return };

        let mut clips = self
            .clips
            .iter()
            .filter(|clip| clip.track == track)
            .filter(|clip| {
                if after {
                    clip.start >= self.playhead - MIN_CLIP * 0.5
                } else {
                    clip.end() <= self.playhead + MIN_CLIP * 0.5
                }
            })
            .collect::<Vec<_>>();
        clips.sort_by(|left, right| {
            left.start
                .total_cmp(&right.start)
                .then(left.id.cmp(&right.id))
        });
        let ids = clips.iter().map(|clip| clip.id).collect::<Vec<_>>();
        self.selected = ids.iter().copied().collect();
        self.primary_selected = if after {
            ids.first().copied()
        } else {
            ids.last().copied()
        };
        self.selected_track = Some(track);
        self.power_duplicate = None;
    }

    pub(crate) fn toggle_playback(&mut self) {
        self.playing = !self.playing;
        self.playback_just_started = self.playing;
        if self.playing {
            self.selection_frame = Instant::now();
        }
    }

    pub(crate) fn seek_relative(&mut self, seconds: f32) {
        self.seek_by(seconds);
    }

    pub(crate) fn step_relative_frames(&mut self, frames: i32) {
        self.step_frames(frames);
    }

    fn group_selected(&mut self) {
        if self.selected.len() < 2 {
            return;
        }
        let group = self.next_group_id();
        let selected = &self.edit.selected;
        for clip in &mut self.edit.document.clips {
            if selected.contains(&clip.id) {
                clip.group = Some(group);
            }
        }
    }

    fn ungroup_selected(&mut self) {
        let groups = self.selected_groups();
        for clip in &mut self.clips {
            if clip.group.is_some_and(|group| groups.contains(&group)) {
                clip.group = None;
            }
        }
    }

    fn delete_selected(&mut self) {
        let selected = &self.edit.selected;
        self.edit
            .document
            .clips
            .retain(|clip| !selected.contains(&clip.id));
        self.selected.clear();
        self.primary_selected = None;
        self.prune_unused_property_rows();
    }

    fn add_track(&mut self, kind: TrackKind, near: Option<usize>) -> usize {
        self.add_track_at(kind, near.unwrap_or(0))
    }

    fn add_track_at(&mut self, kind: TrackKind, index: usize) -> usize {
        let index = index.min(self.tracks.len());
        let id = self.next_track;
        self.next_track += 1;
        let ordinal = self
            .tracks
            .iter()
            .filter(|track| track.kind == kind)
            .count()
            + 1;
        let prefix = match kind {
            TrackKind::Video => "Video",
            TrackKind::Audio => "Audio",
            TrackKind::Effect => "Effect",
        };
        self.tracks.insert(
            index,
            Track {
                id,
                name: format!("{prefix} {ordinal}"),
                kind,
                height: 58.0,
                muted: false,
                solo: false,
                pipeline: None,
                composite: LayerComposite::default(),
                volume: default_track_volume(),
                pan: default_track_pan(),
                property_rows: Vec::new(),
            },
        );
        if kind == TrackKind::Audio {
            let track = &self.tracks[index];
            self.mixer_knobs
                .extend(mixer_knobs(std::slice::from_ref(track)));
        }
        index
    }

    fn remove_empty_preview_track(&mut self, id: u32) {
        if self.clips.iter().any(|clip| clip.track == id) {
            return;
        }
        if let Some(index) = self.track_index(id) {
            self.tracks.remove(index);
            self.track_offsets.remove(&id);
            self.mixer_knobs.retain(|(track, _), _| *track != id);
        }
    }

    fn delete_track(&mut self, id: u32) {
        if self.tracks.len() <= 1 {
            return;
        }
        let Some(index) = self.track_index(id) else {
            return;
        };
        let removed: HashSet<_> = self
            .clips
            .iter()
            .filter(|clip| clip.track == id)
            .map(|clip| clip.id)
            .collect();
        self.clips.retain(|clip| clip.track != id);
        self.selected.retain(|id| !removed.contains(id));
        if self
            .primary_selected
            .is_some_and(|id| removed.contains(&id))
        {
            self.primary_selected = self.selected.iter().copied().min();
        }
        self.tracks.remove(index);
        self.track_offsets.remove(&id);
        self.mixer_knobs.retain(|(track, _), _| *track != id);
    }

    fn insert_clip(&mut self, kind: TrackKind, time: f32, near: Option<usize>) {
        if kind != TrackKind::Audio {
            return;
        }
        let existing = near
            .filter(|&index| {
                self.tracks
                    .get(index)
                    .is_some_and(|track| track.kind == TrackKind::Audio)
            })
            .or_else(|| {
                self.tracks
                    .iter()
                    .enumerate()
                    .filter(|(_, track)| track.kind == TrackKind::Audio)
                    .min_by_key(|(index, _)| near.map_or(0, |near| (*index).abs_diff(near)))
                    .map(|(index, _)| index)
            });
        let track = existing.unwrap_or_else(|| self.add_track(TrackKind::Audio, near));
        let preferred = self.tracks[track].id;
        let track = self.resolve_clip_placement(preferred, TrackKind::Audio, time, 3.4);
        let first_new_clip = self.clips.len();
        let id = self.next_clip;
        self.next_clip += 1;
        let source = VisualSource::AudioPlaceholder;
        let source_instance = self.source_instance_for_new_source(&source);
        self.clips.push(Clip {
            id,
            track,
            start: time.max(0.0),
            duration: 3.4,
            speed: 1.0,
            source_offset: 0.0,
            opacity: 1.0,
            volume: 1.0,
            fade_in: 0.12,
            fade_out: 0.25,
            group: None,
            name: format!("New Audio Clip {id}"),
            color: if id.is_multiple_of(2) {
                ClipColor::AudioA
            } else {
                ClipColor::AudioB
            },
            source,
            source_instance,
            pipeline: PipelineInstance::effect_default(),
            composite: LayerComposite::default(),
            model3d: Model3dClipTransform::default(),
        });
        self.initialize_end_from_clips_since(first_new_clip);
        let _ = self.ensure_property_row_for_clip(id);
        self.selected.clear();
        self.selected_track = None;
        self.selected.insert(id);
        self.primary_selected = Some(id);
    }

    fn track_index(&self, id: u32) -> Option<usize> {
        self.tracks.iter().position(|track| track.id == id)
    }

    fn track_target_cached(
        &self,
        layout: TimelineLayout,
        y: f32,
        heights: &HashMap<u32, f32>,
    ) -> usize {
        if self.tracks.is_empty() {
            return 0;
        }
        let mut row_y = layout.body.y - self.scroll_y;
        for (index, track) in self.tracks.iter().enumerate() {
            let height = heights.get(&track.id).copied().unwrap_or(track.height);
            if y < row_y + height * 0.5 {
                return index;
            }
            row_y += height;
        }
        self.tracks.len() - 1
    }

    fn reorder_track_cached(&mut self, id: u32, target: usize, heights: &HashMap<u32, f32>) {
        let Some(from) = self.track_index(id) else {
            return;
        };
        let target = target.min(self.tracks.len().saturating_sub(1));
        if from == target {
            return;
        }

        let mut y = 0.0;
        let mut old_y = HashMap::with_capacity(self.tracks.len());
        for track in &self.tracks {
            old_y.insert(
                track.id,
                y + self.track_offsets.get(&track.id).copied().unwrap_or(0.0),
            );
            y += heights.get(&track.id).copied().unwrap_or(track.height);
        }
        let track = self.tracks.remove(from);
        self.tracks.insert(target, track);
        y = 0.0;
        let reordered_tracks: Vec<(u32, f32)> = self
            .tracks
            .iter()
            .map(|track| {
                (
                    track.id,
                    heights.get(&track.id).copied().unwrap_or(track.height),
                )
            })
            .collect();
        for (track_id, track_height) in reordered_tracks {
            if track_id != id {
                if let Some(old) = old_y.get(&track_id) {
                    self.track_offsets.insert(track_id, *old - y);
                }
            }
            y += track_height;
        }
        self.track_offsets.remove(&id);
    }

    fn move_keyframes_horizontal(&mut self, points: &mut [KeyframeDragPoint], delta_time: f64) {
        self.move_keyframes(points, delta_time, None, false);
    }

    fn move_keyframes(
        &mut self,
        points: &mut [KeyframeDragPoint],
        delta_time: f64,
        delta_y: Option<f32>,
        apply_frame_snap: bool,
    ) {
        if delta_time >= 0.0 {
            points.sort_by(|a, b| b.current_time.total_cmp(&a.current_time));
        } else {
            points.sort_by(|a, b| a.current_time.total_cmp(&b.current_time));
        }
        for key in points.iter_mut() {
            let mut next_time = (key.origin_time + delta_time).max(0.0);
            if apply_frame_snap && self.frame_snap {
                next_time = (next_time * self.frame_rate as f64).round() / self.frame_rate as f64;
            }
            let next_value = delta_y
                .filter(|_| key.vertical)
                .map(|delta_y| key.origin_value - delta_y * key.value_per_pixel);
            if self.edit_keyframe_lane_key(
                &key.lane,
                key.current_time,
                Some(next_time),
                next_value,
                None,
            ) {
                if let Some(selected) = self.selected_keyframes.iter_mut().find(|selected| {
                    selected.lane == key.lane
                        && (selected.time - key.current_time).abs() <= 1.0 / 24_000.0
                }) {
                    selected.time = next_time;
                }
                key.current_time = next_time;
            }
        }
    }

    fn resize_clip(
        &mut self,
        layout: TimelineLayout,
        id: u32,
        left: bool,
        x: f32,
        rate_stretch: bool,
        origin: ClipEdgeOrigin,
    ) {
        let Some(index) = self.clips.iter().position(|clip| clip.id == id) else {
            return;
        };
        let raw = self.time_at(layout, x);
        let edge = self.snap_time(layout, raw, &[id]);
        let end = origin.start + origin.duration;
        let source_span = origin.duration.max(MIN_CLIP) * origin.speed.max(0.01);
        if left {
            let earliest = if rate_stretch {
                0.0
            } else {
                (origin.start - origin.source_offset / origin.speed.max(0.01)).max(0.0)
            };
            let start = edge.clamp(earliest, end - MIN_CLIP);
            self.clips[index].start = start;
            self.clips[index].duration = end - start;
            self.clips[index].source_offset = if rate_stretch {
                origin.source_offset
            } else {
                (origin.source_offset + (start - origin.start) * origin.speed).max(0.0)
            };
        } else {
            self.clips[index].start = origin.start;
            self.clips[index].duration = (edge - origin.start).max(MIN_CLIP);
            self.clips[index].source_offset = origin.source_offset;
        }
        let start = self.clips[index].start;
        let duration = self.clips[index].duration;
        let group = self.clips[index].group;
        let resized: HashSet<_> = self
            .clips
            .iter()
            .filter(|clip| clip.id == id || group.is_some() && clip.group == group)
            .map(|clip| clip.id)
            .collect();
        let blocked = self
            .clips
            .iter()
            .filter(|clip| resized.contains(&clip.id))
            .any(|resized_clip| {
                self.clips.iter().any(|other| {
                    !resized.contains(&other.id)
                        && other.track == resized_clip.track
                        && intervals_overlap(start, start + duration, other.start, other.end())
                })
            });
        if blocked {
            self.clips[index].start = origin.start;
            self.clips[index].duration = origin.duration;
            self.clips[index].source_offset = origin.source_offset;
        }
        if rate_stretch {
            self.clips[index].speed = (source_span / self.clips[index].duration).clamp(0.01, 100.0);
        } else {
            self.clips[index].speed = origin.speed;
        }
        let duration = self.clips[index].duration;
        self.clips[index].fade_in = self.clips[index].fade_in.min(duration);
        self.clips[index].fade_out = self.clips[index].fade_out.min(duration);

        if let Some(group) = self.clips[index].group {
            let (start, duration, source_offset, speed) = {
                let anchor = &self.clips[index];
                (
                    anchor.start,
                    anchor.duration,
                    anchor.source_offset,
                    anchor.speed,
                )
            };
            for (peer_index, peer) in self.clips.iter_mut().enumerate() {
                if peer_index == index || peer.group != Some(group) {
                    continue;
                }
                peer.start = start;
                peer.duration = duration;
                peer.source_offset = source_offset;
                peer.speed = speed;
                peer.fade_in = peer.fade_in.min(duration);
                peer.fade_out = peer.fade_out.min(duration);
            }
        }
    }

    fn move_clips(
        &mut self,
        layout: TimelineLayout,
        anchor: ClipMoveAnchor,
        origins: &[ClipOrigin],
        snap_points: &[f32],
        preview_tracks: &mut Vec<u32>,
        point: [f32; 2],
    ) -> f64 {
        for origin in origins {
            self.clips[origin.index].track = origin.track;
        }
        for track in preview_tracks.drain(..) {
            self.remove_empty_preview_track(track);
        }

        let ClipMoveAnchor {
            track: anchor_track,
            time: anchor_start,
            pointer: start,
        } = anchor;

        let pointer_dx = point[0] - start[0];
        let pointer_dy = point[1] - start[1];
        if pointer_dx * pointer_dx + pointer_dy * pointer_dy
            < CLIP_DRAG_THRESHOLD_PX * CLIP_DRAG_THRESHOLD_PX
        {
            for origin in origins {
                self.clips[origin.index].start = origin.start;
                self.clips[origin.index].track = origin.track;
            }
            for track in preview_tracks.drain(..) {
                self.remove_empty_preview_track(track);
            }
            self.snap_times.clear();
            return 0.0;
        }

        let raw_dt = pointer_dx / self.pixels_per_second;
        let min_start = origins
            .iter()
            .map(|origin| origin.start)
            .fold(f32::INFINITY, f32::min);
        let mut dt = raw_dt.max(-min_start);
        let threshold = SNAP_PX / self.pixels_per_second;
        let mut best: Option<(f32, f32)> = None;
        for origin in origins {
            for edge in [origin.start + dt, origin.start + origin.duration + dt] {
                if let Some(snap) = nearest_sorted(edge, snap_points) {
                    let diff = snap - edge;
                    if diff.abs() <= threshold && best.is_none_or(|(old, _)| diff.abs() < old.abs())
                    {
                        best = Some((diff, snap));
                    }
                }
            }
        }
        if self.grid_snap {
            let edge = anchor_start + dt;
            if let Some((snap, distance)) = nearest_grid_snap(
                edge,
                self.scroll_time,
                self.pixels_per_second,
                layout.body.width,
            ) {
                let diff = snap - edge;
                if distance <= SNAP_PX
                    && best.is_none_or(|(old, _)| distance < old.abs() * self.pixels_per_second)
                {
                    best = Some((diff, snap));
                }
            }
        }
        self.snap_times.clear();
        if let Some((diff, snap)) = best {
            dt = (dt + diff).max(-min_start);
            self.snap_times.push(snap);
        }

        let wanted = self
            .track_at(layout, point[1])
            .unwrap_or_else(|| self.track_index(anchor_track).unwrap_or(0));
        let moving: HashSet<_> = origins.iter().map(|origin| origin.id).collect();
        let real_tracks = self
            .tracks
            .iter()
            .map(|track| track.id)
            .collect::<HashSet<_>>();
        let mut intervals = TrackIntervals::from_clips(&self.clips, &moving);
        let mut targets = HashMap::new();
        for origin in origins {
            let kind = self
                .track_index(origin.track)
                .map(|index| self.tracks[index].kind)
                .unwrap_or(TrackKind::Video);
            let start = origin.start + dt;
            let end = start + origin.duration;
            let target = self.find_available_track(kind, wanted, start, end, &intervals);
            intervals.insert(target, start, end);
            targets.insert(origin.id, target);
        }
        preview_tracks.extend(
            self.tracks
                .iter()
                .map(|track| track.id)
                .filter(|track| !real_tracks.contains(track)),
        );
        for origin in origins {
            let clip = &mut self.clips[origin.index];
            clip.start = origin.start + dt;
            clip.track = targets[&origin.id];
        }
        dt as f64
    }

    fn find_available_track(
        &mut self,
        kind: TrackKind,
        wanted: usize,
        start: f32,
        end: f32,
        intervals: &TrackIntervals,
    ) -> u32 {
        if let Some(track) = (0..self.tracks.len())
            .filter(|&index| self.tracks[index].kind == kind)
            .filter(|&index| intervals.has_space(self.tracks[index].id, start, end))
            .min_by_key(|&index| index.abs_diff(wanted))
            .map(|index| self.tracks[index].id)
        {
            return track;
        }
        let index = self.add_track_at(kind, wanted.min(self.tracks.len()));
        self.tracks[index].id
    }

    fn resolve_clip_placement(
        &mut self,
        preferred_track: u32,
        kind: TrackKind,
        start: f32,
        duration: f32,
    ) -> u32 {
        let wanted = self.track_index(preferred_track).unwrap_or(0);
        let intervals = TrackIntervals::from_clips(&self.clips, &HashSet::new());
        self.find_available_track(kind, wanted, start, start + duration, &intervals)
    }

    fn snap_candidate(
        &self,
        layout: TimelineLayout,
        time: f32,
        excluded: &[u32],
        include_playhead: bool,
    ) -> Option<f32> {
        let distance = |snap: f32| (snap - time).abs() * self.pixels_per_second;
        let mut best = include_playhead.then(|| (self.playhead, distance(self.playhead)));
        if self.clip_snap {
            if let Some(candidate) = self
                .clips
                .iter()
                .filter(|clip| !excluded.contains(&clip.id))
                .flat_map(|clip| [clip.start, clip.end()])
                .map(|snap| (snap, distance(snap)))
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .filter(|candidate| best.is_none_or(|best| candidate.1 < best.1))
            {
                best = Some(candidate);
            }
        }
        if self.grid_snap {
            if let Some(candidate) = nearest_grid_snap(
                time,
                self.scroll_time,
                self.pixels_per_second,
                layout.body.width,
            )
            .filter(|candidate| best.is_none_or(|best| candidate.1 < best.1))
            {
                best = Some(candidate);
            }
        }
        best.filter(|(_, distance)| *distance <= SNAP_PX)
            .map(|(snap, _)| snap)
    }

    fn insertion_snap_time(&self, layout: TimelineLayout, time: f32) -> f32 {
        self.snap_candidate(layout, time, &[], self.playhead_snap)
            .unwrap_or(time)
    }

    fn snap_time(&mut self, layout: TimelineLayout, time: f32, excluded: &[u32]) -> f32 {
        self.snap_time_with_options(layout, time, excluded, true)
    }

    fn snap_time_with_options(
        &mut self,
        layout: TimelineLayout,
        time: f32,
        excluded: &[u32],
        include_playhead: bool,
    ) -> f32 {
        self.snap_times.clear();
        if let Some(snap) = self.snap_candidate(layout, time, excluded, include_playhead) {
            self.snap_times.push(snap);
            snap
        } else {
            time
        }
    }

    fn clamp_scroll(&self, y: f32, layout: TimelineLayout) -> f32 {
        let total: f32 = (0..self.tracks.len())
            .map(|index| self.display_track_height(index))
            .sum();
        y.clamp(0.0, (total - layout.body.height).max(0.0))
    }

    fn overview_window(&self, layout: TimelineLayout) -> Rect {
        let body = layout.overview_body;
        let total = self.overview_duration(layout);
        let start = self.scroll_time / total;
        let visible = (self.visible_duration(layout) / total).min(1.0);
        let width = (visible * body.width as f64).min(body.width as f64) as f32;
        Rect {
            x: (body.x as f64 + start * body.width as f64)
                .max(body.x as f64)
                .min((body.right() - width).max(body.x) as f64) as f32,
            y: body.y + 4.0,
            width,
            height: (body.height - 8.0).max(1.0),
        }
    }

    fn move_overview(
        &mut self,
        layout: TimelineLayout,
        part: OverviewPart,
        dx: f32,
        scroll_time: f64,
        pixels_per_second: f32,
    ) {
        let total = self.overview_duration(layout);
        let body = layout.overview_body;
        let time_delta = dx as f64 / body.width.max(1.0) as f64 * total;
        let visible = layout.body.width as f64 / pixels_per_second as f64;
        match part {
            OverviewPart::Body => {
                self.scroll_time = (scroll_time + time_delta).max(0.0);
            }
            OverviewPart::Left => {
                let end = scroll_time + visible;
                let start = (scroll_time + time_delta).clamp(0.0, end);
                self.set_overview_window(layout, start, end - start);
            }
            OverviewPart::Right => {
                self.set_overview_window(layout, scroll_time, visible + time_delta);
            }
        }
    }

    fn set_overview_window(&mut self, layout: TimelineLayout, start: f64, duration: f64) {
        if duration <= 0.0 {
            return;
        }
        let pixels_per_second =
            (layout.body.width as f64 / duration).min(MAX_PIXELS_PER_SECOND as f64) as f32;
        if pixels_per_second.is_finite() && pixels_per_second > 0.0 {
            self.pixels_per_second = pixels_per_second;
            self.scroll_time = start.max(0.0);
        }
    }

    fn build_after_end(&self, ctx: &mut ui::BuildCtx, id: impl Hash, rect: Rect, x: f32) {
        let x = x.max(rect.x);
        if x < rect.right() {
            ui::ui!(ctx, {
                Rect(id, Rect { x, width: rect.right() - x, ..rect }) {
                    fill: AFTER_END;
                }
            });
        }
    }
}

fn context_specs(kind: ContextKind) -> Vec<ContextSpec> {
    use ContextCommand::*;
    const INSERT: Option<KeyBinding> = Some(KeyBinding::shifted('a'));
    const DELETE: Option<KeyBinding> = Some(KeyBinding::delete());
    const SET_END: ContextSpec = (
        "Set Timeline End at Playhead",
        None,
        Some(AppIcon::SkipEnd),
        ContextCommand::SetEnd,
    );

    fn with_track_actions(mut items: Vec<ContextSpec>, allow_delete: bool) -> Vec<ContextSpec> {
        items.extend([
            (
                "Insert Video Track",
                None,
                Some(AppIcon::Video),
                ContextCommand::AddTrack(TrackKind::Video),
            ),
            (
                "Insert Audio Track",
                None,
                Some(AppIcon::Audio),
                ContextCommand::AddTrack(TrackKind::Audio),
            ),
            (
                "Insert Effect Track",
                None,
                Some(AppIcon::Effect),
                ContextCommand::AddTrack(TrackKind::Effect),
            ),
        ]);
        if allow_delete {
            items.push((
                "Delete Track",
                None,
                Some(AppIcon::Delete),
                ContextCommand::DeleteTrack,
            ));
        }
        items
    }

    match kind {
        ContextKind::Mixer { .. } => vec![
            (
                "Set exact value…",
                None,
                Some(AppIcon::Inspector),
                SetExactMixer,
            ),
            (
                "Add/Remove Keyframe",
                None,
                Some(AppIcon::KeyframeSet),
                ToggleMixerKeyframe,
            ),
        ],
        ContextKind::Keyframe => vec![
            (
                "Edit value…",
                None,
                Some(AppIcon::Inspector),
                EditKeyframeValue,
            ),
            (
                "Hold",
                None,
                Some(AppIcon::KeyframeSet),
                SetKeyframeInterpolation(Interpolation::Step),
            ),
            (
                "Linear",
                None,
                Some(AppIcon::KeyframeSet),
                SetKeyframeInterpolation(Interpolation::Linear),
            ),
            (
                "Ease In",
                None,
                Some(AppIcon::KeyframeSet),
                SetKeyframeInterpolation(Interpolation::EaseIn),
            ),
            (
                "Ease Out",
                None,
                Some(AppIcon::KeyframeSet),
                SetKeyframeInterpolation(Interpolation::EaseOut),
            ),
            (
                "Ease In / Out",
                None,
                Some(AppIcon::KeyframeSet),
                SetKeyframeInterpolation(Interpolation::EaseInOut),
            ),
            (
                "Delete Keyframe(s)",
                None,
                Some(AppIcon::Delete),
                DeleteKeyframes,
            ),
        ],
        ContextKind::Selection => vec![
            (
                "Copy",
                Some(KeyBinding::primary('c')),
                Some(AppIcon::Copy),
                CopySelection,
            ),
            (
                "Cut",
                Some(KeyBinding::primary('x')),
                Some(AppIcon::Cut),
                CutSelection,
            ),
            (
                "Paste",
                Some(KeyBinding::primary('v')),
                Some(AppIcon::Paste),
                Paste,
            ),
            (
                "Group",
                Some(KeyBinding::plain('g')),
                Some(AppIcon::Group),
                Group,
            ),
            (
                "Ungroup",
                Some(KeyBinding::shifted('g')),
                Some(AppIcon::Ungroup),
                Ungroup,
            ),
            ("Close Gap", None, Some(AppIcon::CloseGap), CloseGap),
            (
                "Speed / Duration…",
                None,
                Some(AppIcon::SpeedDuration),
                SpeedDuration,
            ),
            (
                "Add to new Composition...",
                None,
                Some(AppIcon::Composition),
                AddSelectionToComposition,
            ),
            (
                "Replace selected clips",
                None,
                Some(AppIcon::Restore),
                ReplaceSelectedClips,
            ),
            ("Delete", DELETE, Some(AppIcon::Delete), DeleteSelection),
            SET_END,
        ],
        ContextKind::Empty {
            kind: Some(TrackKind::Video),
            ..
        } => with_track_actions(
            vec![
                (
                    "Insert Video Clip...",
                    INSERT,
                    Some(AppIcon::Video),
                    InsertVideoHere,
                ),
                ("Insert Audio Clip", None, Some(AppIcon::Audio), InsertAudio),
                SET_END,
            ],
            false,
        ),
        ContextKind::Empty {
            kind: Some(TrackKind::Audio),
            ..
        } => with_track_actions(
            vec![
                ("Insert Audio Clip", None, Some(AppIcon::Audio), InsertAudio),
                SET_END,
            ],
            false,
        ),
        ContextKind::Empty {
            kind: Some(TrackKind::Effect),
            ..
        } => with_track_actions(
            vec![
                (
                    "Insert Effect Clip...",
                    INSERT,
                    Some(AppIcon::Effect),
                    InsertEffectHere,
                ),
                SET_END,
            ],
            false,
        ),
        ContextKind::Empty { .. } => with_track_actions(
            vec![
                (
                    "Insert Video Clip...",
                    INSERT,
                    Some(AppIcon::Video),
                    InsertVideoFirst,
                ),
                ("Insert Audio Clip", None, Some(AppIcon::Audio), InsertAudio),
                SET_END,
            ],
            false,
        ),
        ContextKind::Track {
            kind: TrackKind::Video,
            ..
        } => with_track_actions(
            vec![
                (
                    "Insert Video Clip...",
                    INSERT,
                    Some(AppIcon::Video),
                    InsertVideoHere,
                ),
                ("Rename Track", None, Some(AppIcon::Rename), RenameTrack),
            ],
            true,
        ),
        ContextKind::Track {
            kind: TrackKind::Effect,
            ..
        } => with_track_actions(
            vec![
                (
                    "Insert Effect Clip...",
                    INSERT,
                    Some(AppIcon::Effect),
                    InsertEffectHere,
                ),
                ("Rename Track", None, Some(AppIcon::Rename), RenameTrack),
            ],
            true,
        ),
        ContextKind::Track {
            kind: TrackKind::Audio,
            ..
        } => with_track_actions(
            vec![("Rename Track", None, Some(AppIcon::Rename), RenameTrack)],
            true,
        ),
    }
}

fn context_items(kind: ContextKind, replacement_enabled: bool) -> Vec<ContextItem> {
    context_specs(kind)
        .into_iter()
        .map(|(label, shortcut, icon, action)| {
            let enabled =
                !matches!(action, ContextCommand::ReplaceSelectedClips) || replacement_enabled;
            ContextMenuItem::new(label, icon, action)
                .with_shortcut(shortcut.map(|binding| binding.to_string()))
                .enabled(enabled)
        })
        .collect()
}

fn normalized_rect(a: [f32; 2], b: [f32; 2]) -> Rect {
    Rect::new(
        a[0].min(b[0]),
        a[1].min(b[1]),
        (a[0] - b[0]).abs(),
        (a[1] - b[1]).abs(),
    )
}

fn push_rect_vertices(vertices: &mut Vec<[f32; 2]>, rect: Rect) {
    let right = rect.right();
    let bottom = rect.bottom();
    vertices.extend([
        [rect.x, rect.y],
        [right, rect.y],
        [rect.x, bottom],
        [rect.x, bottom],
        [right, rect.y],
        [right, bottom],
    ]);
}

fn keyframe_points_range(points: &[KeyframeLanePoint]) -> (f64, f64) {
    let minimum = points
        .iter()
        .map(|point| point.value)
        .fold(f64::INFINITY, f64::min);
    let maximum = points
        .iter()
        .map(|point| point.value)
        .fold(f64::NEG_INFINITY, f64::max);
    if !minimum.is_finite() || !maximum.is_finite() {
        (0.0, 1.0)
    } else {
        (minimum, maximum)
    }
}

fn keyframe_data_range(lane: &KeyframeLane) -> (f64, f64) {
    let (minimum, maximum) = lane.value_range;
    if (maximum - minimum).abs() <= 1.0e-12 {
        let buffer = maximum.abs().max(1.0) * 0.1;
        (minimum - buffer, maximum + buffer)
    } else {
        (minimum, maximum)
    }
}

fn keyframe_property_axis_range(lanes: &[KeyframeLane], height: f32) -> (f64, f64) {
    let mut minimum = f64::INFINITY;
    let mut maximum = f64::NEG_INFINITY;
    for lane in lanes {
        minimum = minimum.min(lane.value_range.0);
        maximum = maximum.max(lane.value_range.1);
    }
    if !minimum.is_finite() || !maximum.is_finite() {
        return (0.0, 1.0);
    }
    if (maximum - minimum).abs() <= 1.0e-12 {
        let buffer = maximum.abs().max(1.0) * 0.1;
        minimum -= buffer;
        maximum += buffer;
    }
    let usable = (height - KEYFRAME_GRAPH_PAD * 2.0).max(1.0) as f64;
    let units_per_pixel = (maximum - minimum) / usable;
    let buffer = units_per_pixel * KEYFRAME_GRAPH_PAD as f64;
    minimum = ((minimum - buffer) * 4.0).floor() / 4.0;
    maximum = ((maximum + buffer) * 4.0).ceil() / 4.0;
    if maximum <= minimum {
        maximum = minimum + 0.25;
    }
    (minimum, maximum)
}

fn keyframe_axis_range(lane: &KeyframeLane, height: f32) -> (f64, f64) {
    let (minimum, maximum) = keyframe_data_range(lane);
    let usable = (height - KEYFRAME_GRAPH_PAD * 2.0).max(1.0) as f64;
    let units_per_pixel = (maximum - minimum) / usable;
    let buffer = units_per_pixel * KEYFRAME_GRAPH_PAD as f64;
    let minimum = ((minimum - buffer) * 4.0).floor() / 4.0;
    let mut maximum = ((maximum + buffer) * 4.0).ceil() / 4.0;
    if maximum <= minimum {
        maximum = minimum + 0.25;
    }
    (minimum, maximum)
}

fn keyframe_segment_amount(a: KeyframeLanePoint, b: KeyframeLanePoint, t: f32) -> f32 {
    if a.interpolation == Interpolation::Step {
        return 0.0;
    }
    let (out, incoming) = keyframe_easing(a, b);
    crate::effects::bezier_easing_amount(out, incoming, t)
}

fn keyframe_easing(a: KeyframeLanePoint, b: KeyframeLanePoint) -> (EasingHandle, EasingHandle) {
    (
        if a.custom_ease_out {
            a.ease_out
        } else {
            crate::effects::preset_out_handle(a.interpolation)
        },
        if b.custom_ease_in {
            b.ease_in
        } else {
            crate::effects::preset_in_handle(b.interpolation)
        },
    )
}

const KEYFRAME_CURVE_MAX_SEGMENT_PX: f32 = 8.0;
const KEYFRAME_CURVE_FLATNESS_PX: f32 = 0.35;
const KEYFRAME_CURVE_MAX_DEPTH: u8 = 9;

fn keyframe_curve_point(
    a: KeyframeLanePoint,
    b: KeyframeLanePoint,
    start: [f32; 2],
    end: [f32; 2],
    t: f32,
) -> [f32; 2] {
    let mix = keyframe_segment_amount(a, b, t);
    [
        start[0] + (end[0] - start[0]) * t,
        start[1] + (end[1] - start[1]) * mix,
    ]
}

fn push_keyframe_curve_vertices(
    vertices: &mut Vec<[f32; 2]>,
    a: KeyframeLanePoint,
    b: KeyframeLanePoint,
    start: [f32; 2],
    end: [f32; 2],
    width: f32,
) {
    let (out, incoming) = keyframe_easing(a, b);
    if out == EasingHandle::LINEAR && incoming == EasingHandle::LINEAR {
        push_line_vertices(vertices, start, end, width);
        return;
    }

    #[allow(clippy::too_many_arguments)]
    fn subdivide(
        vertices: &mut Vec<[f32; 2]>,
        a: KeyframeLanePoint,
        b: KeyframeLanePoint,
        curve_start: [f32; 2],
        curve_end: [f32; 2],
        t0: f32,
        p0: [f32; 2],
        t1: f32,
        p1: [f32; 2],
        depth: u8,
        width: f32,
    ) {
        let mid_t = (t0 + t1) * 0.5;
        let mid = keyframe_curve_point(a, b, curve_start, curve_end, mid_t);
        let chord_mid_y = (p0[1] + p1[1]) * 0.5;
        let curve_error = (mid[1] - chord_mid_y).abs();
        let pixel_span = (p1[0] - p0[0]).abs();

        if depth < KEYFRAME_CURVE_MAX_DEPTH
            && (pixel_span > KEYFRAME_CURVE_MAX_SEGMENT_PX
                || curve_error > KEYFRAME_CURVE_FLATNESS_PX)
        {
            subdivide(
                vertices,
                a,
                b,
                curve_start,
                curve_end,
                t0,
                p0,
                mid_t,
                mid,
                depth + 1,
                width,
            );
            subdivide(
                vertices,
                a,
                b,
                curve_start,
                curve_end,
                mid_t,
                mid,
                t1,
                p1,
                depth + 1,
                width,
            );
        } else {
            push_line_vertices(vertices, p0, p1, width);
        }
    }

    subdivide(vertices, a, b, start, end, 0.0, start, 1.0, end, 0, width);
}

#[allow(clippy::too_many_arguments)]
fn keyframe_control_positions(
    timeline: &TimelineState,
    layout: TimelineLayout,
    rect: Rect,
    axis_range: Option<(f64, f64)>,
    a: KeyframeLanePoint,
    b: KeyframeLanePoint,
    out: EasingHandle,
    incoming: EasingHandle,
) -> ([f32; 2], [f32; 2]) {
    let span = (b.time - a.time).max(1.0e-9);
    let delta = b.value - a.value;
    let out_time = a.time + span * out.x as f64;
    let out_value = a.value + delta * out.y as f64;
    let in_time = b.time - span * incoming.x as f64;
    let in_value = b.value - delta * incoming.y as f64;
    (
        [
            timeline.time_x(layout, out_time as f32),
            keyframe_value_y(rect, axis_range, out_value),
        ],
        [
            timeline.time_x(layout, in_time as f32),
            keyframe_value_y(rect, axis_range, in_value),
        ],
    )
}

fn keyframe_value_at_y(rect: Rect, axis_range: (f64, f64), y: f32) -> f64 {
    let (minimum, maximum) = axis_range;
    let t = ((rect.bottom() - y) / rect.height.max(1.0)) as f64;
    minimum + (maximum - minimum) * t
}

fn keyframe_value_y(rect: Rect, axis_range: Option<(f64, f64)>, value: f64) -> f32 {
    let Some((minimum, maximum)) = axis_range else {
        return rect.y + rect.height * 0.5;
    };
    rect.bottom()
        - ((value - minimum) / (maximum - minimum).max(1.0e-12)) as f32 * rect.height.max(1.0)
}

fn format_keyframe_value(value: f32) -> String {
    let mut output = format!("{value:.4}");
    while output.contains('.') && output.ends_with('0') {
        output.pop();
    }
    if output.ends_with('.') {
        output.pop();
    }
    output
}

fn push_line_vertices(vertices: &mut Vec<[f32; 2]>, a: [f32; 2], b: [f32; 2], width: f32) {
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let length = (dx * dx + dy * dy).sqrt();
    if length <= 1.0e-5 {
        return;
    }
    let half = width * 0.5;
    let nx = -dy / length * half;
    let ny = dx / length * half;
    let p0 = [a[0] + nx, a[1] + ny];
    let p1 = [b[0] + nx, b[1] + ny];
    let p2 = [a[0] - nx, a[1] - ny];
    let p3 = [b[0] - nx, b[1] - ny];
    vertices.extend([p0, p1, p2, p2, p1, p3]);
}

fn push_diamond_vertices(vertices: &mut Vec<[f32; 2]>, center: [f32; 2], radius: f32) {
    let top = [center[0], center[1] - radius];
    let right = [center[0] + radius, center[1]];
    let bottom = [center[0], center[1] + radius];
    let left = [center[0] - radius, center[1]];
    vertices.extend([top, right, left, left, right, bottom]);
}

fn keyframe_component_label(component: usize, count: usize) -> String {
    if count == 1 {
        return "Value".to_owned();
    }
    match component {
        0 => "X".to_owned(),
        1 => "Y".to_owned(),
        2 => "Z".to_owned(),
        3 => "W".to_owned(),
        _ => format!("Component {}", component + 1),
    }
}

fn keyframe_property_color(id: &KeyframeLaneId) -> Color {
    let mut hasher = DefaultHasher::new();
    id.group.hash(&mut hasher);
    id.component.hash(&mut hasher);
    let hue = (hasher.finish() as f64 / u64::MAX as f64) as f32;
    let saturation = 0.68;
    let value = 0.94;
    let sector = hue * 6.0;
    let i = sector.floor() as i32;
    let f = sector - sector.floor();
    let p = value * (1.0 - saturation);
    let q = value * (1.0 - saturation * f);
    let t = value * (1.0 - saturation * (1.0 - f));
    let (r, g, b) = match i.rem_euclid(6) {
        0 => (value, t, p),
        1 => (q, value, p),
        2 => (p, value, t),
        3 => (p, q, value),
        4 => (t, p, value),
        _ => (value, p, q),
    };
    Color::rgb8((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}

fn intersects(a: Rect, b: Rect) -> bool {
    a.x < b.right() && a.right() > b.x && a.y < b.bottom() && a.bottom() > b.y
}

fn nearest_sorted(time: f32, points: &[f32]) -> Option<f32> {
    let index = points
        .binary_search_by(|point| point.total_cmp(&time))
        .unwrap_or_else(|index| index);
    match (
        index.checked_sub(1).and_then(|index| points.get(index)),
        points.get(index),
    ) {
        (Some(left), Some(right)) => Some(if time - left <= right - time {
            *left
        } else {
            *right
        }),
        (Some(left), None) => Some(*left),
        (None, right) => right.copied(),
    }
}

fn tick_step(pps: f32) -> f64 {
    let target = 72.0 / (pps as f64).max(f64::MIN_POSITIVE);
    if !target.is_finite() {
        return f64::MAX;
    }
    let magnitude = 10.0_f64.powf(target.log10().floor()).max(f64::MIN_POSITIVE);
    let unit = target / magnitude;
    magnitude
        * if unit <= 1.0 {
            1.0
        } else if unit <= 2.0 {
            2.0
        } else if unit <= 5.0 {
            5.0
        } else {
            10.0
        }
}

fn nearest_grid_snap(time: f32, scroll: f64, pps: f32, width: f32) -> Option<(f32, f32)> {
    let x = ((time as f64 - scroll) * pps as f64) as f32;
    timeline_ticks(scroll, pps, width)
        .map(|(_, grid_x, grid_time)| (grid_time as f32, (grid_x - x).abs()))
        .min_by(|a, b| a.1.total_cmp(&b.1))
}

fn timeline_ticks(scroll: f64, pps: f32, width: f32) -> impl Iterator<Item = (i64, f32, f64)> {
    let step = tick_step(pps);
    let pps = pps as f64;
    let spacing = step * pps;
    let first_index = (scroll / step).floor() as i64;
    let first_time = first_index as f64 * step;
    let first_x = (first_time - scroll) * pps;
    let count = (width as f64 / spacing).ceil().max(0.0) as usize + 2;
    (0..count)
        .map(move |index| {
            (
                first_index + index as i64,
                (first_x + index as f64 * spacing).round() as f32,
                first_time + index as f64 * step,
            )
        })
        .filter(|(_, _, time)| *time >= 0.0)
}

pub(crate) fn format_timecode(seconds: f32, frame_rate: f32) -> String {
    let frame_rate = frame_rate.max(1.0).round() as u64;
    let total_frames = (seconds.max(0.0) * frame_rate as f32).round() as u64;
    let frames = total_frames % frame_rate;
    let total_seconds = total_frames / frame_rate;
    let hours = total_seconds / 3600;
    let minutes = total_seconds / 60 % 60;
    let seconds = total_seconds % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}:{frames:02}")
}

pub(crate) fn parse_timecode(value: &str, frame_rate: f32) -> Option<f32> {
    let fps = frame_rate.max(1.0).round() as u64;
    let mut parts = value.trim().split(':');
    let hours = parts.next()?.parse::<u64>().ok()?;
    let minutes = parts.next()?.parse::<u64>().ok()?;
    let seconds = parts.next()?.parse::<u64>().ok()?;
    let frames = parts.next()?.parse::<u64>().ok()?;
    if parts.next().is_some() || minutes >= 60 || seconds >= 60 || frames >= fps {
        return None;
    }
    let total_seconds = hours
        .checked_mul(3600)?
        .checked_add(minutes.checked_mul(60)?)?
        .checked_add(seconds)?;
    let total_frames = total_seconds.checked_mul(fps)?.checked_add(frames)?;
    Some(total_frames as f32 / fps as f32)
}

fn format_time(seconds: f64, step: f64) -> String {
    let tenths = (seconds.max(0.0) * 10.0).round() as u64;
    let total = tenths / 10;
    let hours = total / 3600;
    let minutes = total / 60 % 60;
    let seconds = total % 60;
    match (hours, step < 1.0) {
        (0, false) => format!("{minutes:02}:{seconds:02}"),
        (0, true) => format!("{minutes:02}:{seconds:02}.{}", tenths % 10),
        (_, false) => format!("{hours}:{minutes:02}:{seconds:02}"),
        (_, true) => format!("{hours}:{minutes:02}:{seconds:02}.{}", tenths % 10),
    }
}

fn local_image_depends_on(instance: &PipelineInstance, node: u64, target: u64) -> bool {
    ImageGraphIndex::new(&instance.local_nodes).stack_depends_on(node, target)
}

#[cfg(test)]
mod effect_model_tests {
    use super::*;
    use std::collections::BTreeMap;

    fn track_id(timeline: &TimelineState, kind: TrackKind) -> u32 {
        timeline
            .tracks
            .iter()
            .find(|track| track.kind == kind)
            .unwrap()
            .id
    }

    fn insert_test_generator(timeline: &mut TimelineState) -> u32 {
        insert_test_generator_named(timeline, 0.0, "Plugin Generator")
    }

    fn insert_test_generator_named(timeline: &mut TimelineState, time: f32, name: &str) -> u32 {
        assert!(timeline.insert_generator_clip_at(
            timeline.tracks[0].id,
            time,
            name,
            GeneratorSource::Plugin {
                generator_type: "test.generator".into(),
                parameters: std::collections::BTreeMap::new(),
            },
            PipelineInstance::effect_default(),
        ));
        timeline.selected_clip_id().unwrap()
    }

    #[test]
    fn first_insert_initializes_timeline_end_once() {
        let mut timeline = TimelineState::default();
        assert_eq!(timeline.end_time, None);

        insert_test_generator_named(&mut timeline, 2.0, "First");
        assert_eq!(timeline.end_time, Some(7.0));

        insert_test_generator_named(&mut timeline, 12.0, "Later");
        assert_eq!(timeline.end_time, Some(7.0));
    }

    #[test]
    fn initial_insert_batch_end_uses_furthest_new_clip() {
        let mut timeline = TimelineState::default();
        let first_new_clip = timeline.clips.len();

        for (time, name) in [(0.0, "First"), (8.0, "Second")] {
            insert_test_generator_named(&mut timeline, time, name);
        }
        timeline.set_initial_end_from_clips_since(first_new_clip);

        assert_eq!(timeline.end_time, Some(13.0));
    }

    #[test]
    fn insert_target_keeps_selected_track_context() {
        let mut timeline = TimelineState::default();
        let audio_track = track_id(&timeline, TrackKind::Audio);
        timeline.selected_track = Some(audio_track);
        let (track, time, kind) = timeline
            .insert_target(&LayoutSnapshot::default(), [-1.0, -1.0])
            .expect("selected track is a valid insertion target");
        assert_eq!(track, audio_track);
        assert_eq!(time, timeline.playhead);
        assert_eq!(kind, TrackKind::Audio);
    }

    #[test]
    fn composition_video_clip_preserves_visual_pipeline() {
        let mut timeline = TimelineState::default();
        let video_track = track_id(&timeline, TrackKind::Video);
        let mut visual_pipeline = PipelineInstance::effect_default();
        visual_pipeline.pipeline = Some(77);
        assert!(timeline.insert_composition_clip_at(
            (video_track, 0.0),
            42,
            "Nested".into(),
            false,
            1.0,
            visual_pipeline,
        ));
        assert_eq!(
            timeline.selected_clip().unwrap().pipeline.pipeline,
            Some(77)
        );
    }

    #[test]
    fn composition_video_clip_repairs_to_visual_graph_with_transform() {
        let mut timeline = TimelineState::default();
        let video_track = track_id(&timeline, TrackKind::Video);
        assert!(timeline.insert_composition_clip_at(
            (video_track, 0.0),
            42,
            "Nested".into(),
            false,
            1.0,
            PipelineInstance::effect_default(),
        ));
        assert!(timeline.selected_pipeline().unwrap().transform().is_none());
        let plugins = PluginRegistry::load_default("").unwrap();
        assert!(timeline.ensure_composition_visual_pipelines(&plugins));
        assert!(timeline.selected_pipeline().unwrap().transform().is_some());
    }

    #[test]
    fn generators_are_plugin_sources() {
        let mut timeline = TimelineState::default();
        insert_test_generator(&mut timeline);
        let clip = timeline.selected_clip().unwrap();
        assert!(matches!(
            &clip.source,
            VisualSource::Generator(GeneratorSource::Plugin { generator_type, .. })
                if generator_type == "test.generator"
        ));
    }

    #[test]
    fn editing_new_generator_parameter_inserts_missing_binding() {
        let mut timeline = TimelineState::default();
        insert_test_generator(&mut timeline);
        assert!(timeline.generator_value("border_width").is_none());
        timeline.set_generator_value("border_width", GpuValue::F32(12.5));
        assert_eq!(
            timeline.generator_value("border_width"),
            Some(GpuValue::F32(12.5))
        );
    }

    fn assert_no_overlaps(timeline: &TimelineState) {
        for (index, clip) in timeline.clips.iter().enumerate() {
            assert!(timeline.clips.iter().skip(index + 1).all(|other| {
                clip.track != other.track
                    || !intervals_overlap(clip.start, clip.end(), other.start, other.end())
            }));
        }
    }

    #[test]
    fn seek_and_frame_step_preserve_play_state() {
        let mut timeline = TimelineState {
            playing: true,
            ..TimelineState::default()
        };
        timeline.seek_by(5.0);
        assert!(timeline.playing);
        timeline.step_frames(1);
        assert!(timeline.playing);
    }

    #[test]
    fn frame_step_moves_by_one_frame_every_time() {
        let mut timeline = TimelineState {
            frame_rate: 30.0,
            ..TimelineState::default()
        };
        let start = 1.0_f32;
        timeline.playhead = start;

        for press in 1..=120 {
            timeline.step_frames(1);
            let expected = start as f64 + press as f64 / 30.0;
            assert!((timeline.playhead as f64 - expected).abs() < 1.0e-4);
        }

        for press in 1..=120 {
            timeline.step_frames(-1);
            let expected = start as f64 + (120 - press) as f64 / 30.0;
            assert!((timeline.playhead as f64 - expected).abs() < 1.0e-4);
        }
    }

    #[test]
    fn frame_step_is_relative_even_when_playhead_is_between_frames() {
        let mut timeline = TimelineState {
            edit: TimelineEditState {
                document: TimelineDocument {
                    view: TimelineViewState {
                        playhead: 2.013,
                        ..TimelineViewState::default()
                    },
                    ..TimelineDocument::default()
                },
                selected: HashSet::new(),
                primary_selected: None,
                clipboard: Vec::new(),
            },
            frame_rate: 24.0,
            ..TimelineState::default()
        };
        let start = timeline.playhead;

        timeline.step_frames(1);
        assert!((timeline.playhead - (start + 1.0 / 24.0)).abs() < 1.0e-5);
        timeline.step_frames(1);
        assert!((timeline.playhead - (start + 2.0 / 24.0)).abs() < 1.0e-5);
    }

    #[test]
    fn touching_intervals_ignore_f32_roundoff() {
        let boundary = 3_600.0_f32;
        let next = f32::from_bits(boundary.to_bits() + 1);
        let ulp = next - boundary;

        assert!(!intervals_overlap(
            0.0,
            boundary + ulp,
            boundary,
            boundary + 1.0,
        ));
        assert!(intervals_overlap(
            0.0,
            boundary + 0.01,
            boundary,
            boundary + 1.0,
        ));
    }

    #[test]
    fn overlapping_insert_creates_nearest_compatible_track() {
        let mut timeline = TimelineState::default();
        let track = timeline.tracks[0].id;
        assert!(timeline.insert_generator_clip_at(
            track,
            0.0,
            "First",
            GeneratorSource::Plugin {
                generator_type: "test.first".into(),
                parameters: BTreeMap::new(),
            },
            PipelineInstance::effect_default(),
        ));
        assert!(timeline.insert_generator_clip_at(
            track,
            1.0,
            "Second",
            GeneratorSource::Plugin {
                generator_type: "test.second".into(),
                parameters: BTreeMap::new(),
            },
            PipelineInstance::effect_default(),
        ));
        assert_ne!(timeline.clips[0].track, timeline.clips[1].track);
        assert_no_overlaps(&timeline);
        assert_eq!(
            timeline
                .tracks
                .iter()
                .filter(|track| track.kind == TrackKind::Video)
                .count(),
            2
        );
    }

    #[test]
    fn overlap_preview_track_disappears_when_drag_returns_to_free_space() {
        let mut timeline = TimelineState::default();
        insert_test_generator(&mut timeline);
        let track = timeline.tracks[0].id;
        assert!(timeline.insert_generator_clip_at(
            track,
            6.0,
            "Second",
            GeneratorSource::Plugin {
                generator_type: "test.second".into(),
                parameters: BTreeMap::new(),
            },
            PipelineInstance::effect_default(),
        ));
        let moving = timeline.clips[1].clone();
        let origin = ClipOrigin {
            index: 1,
            id: moving.id,
            start: moving.start,
            duration: moving.duration,
            track: moving.track,
            source_offset: moving.source_offset,
            opacity: moving.opacity,
            volume: moving.volume,
        };
        let layout = TimelineLayout::new(Rect::new(0.0, 0.0, 1000.0, 500.0));
        let pointer = [timeline.time_x(layout, moving.start), layout.body.y + 10.0];
        let anchor = ClipMoveAnchor {
            track,
            time: moving.start,
            pointer,
        };
        let mut preview_tracks = Vec::new();
        timeline.move_clips(
            layout,
            anchor,
            &[origin],
            &[],
            &mut preview_tracks,
            [timeline.time_x(layout, 0.0), pointer[1]],
        );
        assert_eq!(preview_tracks.len(), 1);
        assert_eq!(
            timeline
                .tracks
                .iter()
                .filter(|track| track.kind == TrackKind::Video)
                .count(),
            2
        );

        timeline.move_clips(layout, anchor, &[origin], &[], &mut preview_tracks, pointer);
        assert!(preview_tracks.is_empty());
        assert_eq!(timeline.clips[1].track, track);
        assert_eq!(
            timeline
                .tracks
                .iter()
                .filter(|track| track.kind == TrackKind::Video)
                .count(),
            1
        );
    }

    #[test]
    fn selection_click_does_not_reassign_equal_span_clips() {
        let mut timeline = TimelineState::default();
        let first_track = timeline.tracks[0].id;
        assert!(timeline.insert_generator_clip_at(
            first_track,
            2.0,
            "First",
            GeneratorSource::Plugin {
                generator_type: "test.first".into(),
                parameters: BTreeMap::new(),
            },
            PipelineInstance::effect_default(),
        ));
        assert!(timeline.insert_generator_clip_at(
            first_track,
            2.0,
            "Second",
            GeneratorSource::Plugin {
                generator_type: "test.second".into(),
                parameters: BTreeMap::new(),
            },
            PipelineInstance::effect_default(),
        ));
        assert_eq!(timeline.clips.len(), 2);
        assert_ne!(timeline.clips[0].track, timeline.clips[1].track);

        let original = timeline
            .clips
            .iter()
            .map(|clip| (clip.id, clip.start, clip.track))
            .collect::<Vec<_>>();
        let origins = timeline
            .clips
            .iter()
            .enumerate()
            .map(|(index, clip)| ClipOrigin {
                index,
                id: clip.id,
                start: clip.start,
                duration: clip.duration,
                track: clip.track,
                source_offset: clip.source_offset,
                opacity: clip.opacity,
                volume: clip.volume,
            })
            .collect::<Vec<_>>();
        let clicked = timeline.clips[1].clone();
        let layout = TimelineLayout::new(Rect::new(0.0, 0.0, 1000.0, 500.0));
        let track_index = timeline.track_index(clicked.track).unwrap();
        let pointer = [
            timeline.time_x(layout, clicked.start) + 40.0,
            timeline.track_y(layout, track_index) + clicked.duration.min(20.0),
        ];
        let anchor = ClipMoveAnchor {
            track: clicked.track,
            time: clicked.start,
            pointer,
        };
        let mut preview_tracks = Vec::new();
        timeline.move_clips(layout, anchor, &origins, &[], &mut preview_tracks, pointer);

        assert!(preview_tracks.is_empty());
        for (id, start, track) in original {
            let clip = timeline.clips.iter().find(|clip| clip.id == id).unwrap();
            assert_eq!(clip.start, start);
            assert_eq!(clip.track, track);
        }
    }

    #[test]
    fn plain_click_inside_multi_selection_targets_only_clicked_clip() {
        let mut timeline = TimelineState::default();
        let track = timeline.tracks[0].id;
        for (start, name) in [(0.0, "First"), (6.0, "Second")] {
            assert!(timeline.insert_generator_clip_at(
                track,
                start,
                name,
                GeneratorSource::Plugin {
                    generator_type: format!("test.{name}"),
                    parameters: BTreeMap::new(),
                },
                PipelineInstance::effect_default(),
            ));
        }
        let first = timeline.clips[0].id;
        let second = timeline.clips[1].id;
        timeline.selected = HashSet::from([first, second]);
        timeline.primary_selected = Some(first);

        assert_eq!(timeline.multi_selection_click_target(second), Some(second));
        timeline.replace_selection_with_clip(second);
        assert_eq!(timeline.selected, HashSet::from([second]));
        assert_eq!(timeline.primary_selected, Some(second));
    }

    #[test]
    fn clipboard_pastes_selection_at_playhead_with_new_ids() {
        let mut timeline = TimelineState::default();
        insert_test_generator(&mut timeline);
        let original = timeline.selected_clip().unwrap().id;
        timeline.copy_selection();
        timeline.playhead = 4.0;
        timeline.paste();

        assert_eq!(timeline.clips.len(), 2);
        let pasted = timeline.selected_clip().unwrap();
        assert_ne!(pasted.id, original);
        assert_eq!(pasted.start, 4.0);
    }

    #[test]
    fn clipboard_survives_composition_switch_and_remaps_track_identity() {
        let mut timeline = TimelineState::default();
        insert_test_generator(&mut timeline);
        timeline.copy_selection();

        let mut target = TimelineDocument::default();
        target.tracks[0].id = 101;
        target.tracks[1].id = 202;
        target.next_track = 203;
        timeline.load_document_preserving_clipboard(target);
        timeline.playhead = 2.0;
        timeline.paste();

        assert_eq!(timeline.clips.len(), 1);
        assert_eq!(timeline.clips[0].track, 101);
        assert_eq!(timeline.clips[0].start, 2.0);
    }

    #[test]
    fn clipboard_preserves_same_kind_track_rank_across_compositions() {
        let mut timeline = TimelineState::default();
        let second_video_index = timeline.add_track_at(TrackKind::Video, timeline.tracks.len());
        let second_video = timeline.tracks[second_video_index].id;
        assert!(timeline.insert_generator_clip_at(
            second_video,
            0.0,
            "Second video track",
            GeneratorSource::Plugin {
                generator_type: "test.rank".into(),
                parameters: BTreeMap::new(),
            },
            PipelineInstance::effect_default(),
        ));
        timeline.copy_selection();

        timeline.load_document_preserving_clipboard(TimelineDocument::default());
        timeline.paste();

        assert_eq!(
            timeline
                .tracks
                .iter()
                .filter(|track| track.kind == TrackKind::Video)
                .count(),
            2
        );
        let pasted = timeline.selected_clip().unwrap();
        assert_eq!(
            timeline.track_rank_of_kind(pasted.track, TrackKind::Video),
            Some(1)
        );
    }

    #[test]
    fn composition_extraction_moves_every_selected_track_intact() {
        let mut timeline = TimelineState::default();
        let first_video = timeline.tracks[0].id;
        let second_video_index = timeline.add_track_at(TrackKind::Video, timeline.tracks.len());
        let second_video = timeline.tracks[second_video_index].id;
        for (track, name) in [(first_video, "Upper"), (second_video, "Lower")] {
            assert!(timeline.insert_generator_clip_at(
                track,
                1.0,
                name,
                GeneratorSource::Plugin {
                    generator_type: format!("test.{name}"),
                    parameters: BTreeMap::new(),
                },
                PipelineInstance::effect_default(),
            ));
        }
        let selected = timeline
            .clips
            .iter()
            .map(|clip| clip.id)
            .collect::<HashSet<_>>();
        timeline.selected = selected;
        timeline.primary_selected = timeline.clips.first().map(|clip| clip.id);

        let extraction = timeline.extract_selection_for_composition().unwrap();
        assert!(timeline.clips.is_empty());
        assert_eq!(extraction.timeline.clips.len(), 2);
        assert_eq!(extraction.timeline.tracks.len(), 2);
        assert_ne!(
            extraction.timeline.clips[0].track,
            extraction.timeline.clips[1].track
        );
        assert!(
            extraction
                .timeline
                .clips
                .iter()
                .all(|clip| (clip.start - 0.0).abs() < f32::EPSILON)
        );
    }

    #[test]
    fn composition_extraction_preserves_layer_rows_and_rebases_keyframes() {
        let mut timeline = TimelineState::default();
        let track = timeline.tracks[0].id;
        assert!(timeline.insert_generator_clip_at(
            track,
            5.0,
            "Animated",
            GeneratorSource::Plugin {
                generator_type: "test.animated".into(),
                parameters: BTreeMap::new(),
            },
            PipelineInstance::effect_default(),
        ));
        let source = timeline.selected_clip().unwrap().source.clone();
        let source_instance = timeline.selected_clip().unwrap().source_instance;
        let mut track_pipeline = PipelineInstance::effect_default();
        track_pipeline.pipeline = Some(88);
        let mut track_override = Binding::Constant(GpuValue::F32(1.0));
        track_override.toggle_keyframe(5.0);
        track_override.set_value(7.0, GpuValue::F32(2.0));
        track_pipeline.overrides.insert(456, "gain", track_override);
        let source_track = timeline
            .tracks
            .iter_mut()
            .find(|item| item.id == track)
            .unwrap();
        source_track.pipeline = Some(track_pipeline);
        source_track.composite.opacity.toggle_keyframe(5.0);
        source_track
            .composite
            .opacity
            .set_value(7.0, GpuValue::F32(0.8));

        let row = source_track
            .property_row_mut(&source, source_instance)
            .unwrap();
        row.pipeline.pipeline = Some(77);
        let mut override_binding = Binding::Constant(GpuValue::F32(0.25));
        override_binding.toggle_keyframe(5.0);
        override_binding.set_value(7.0, GpuValue::F32(0.75));
        row.pipeline
            .overrides
            .insert(123, "amount", override_binding);
        row.composite.opacity.toggle_keyframe(5.0);
        row.composite.opacity.set_value(7.0, GpuValue::F32(0.5));
        row.model3d.position.toggle_keyframe(5.0);
        row.model3d
            .position
            .set_value(7.0, GpuValue::Vec3([1.0, 2.0, 3.0]));
        if let VisualSource::Generator(source) = &mut row.source {
            let mut parameter = Binding::Constant(GpuValue::F32(10.0));
            parameter.toggle_keyframe(5.0);
            parameter.set_value(7.0, GpuValue::F32(20.0));
            source
                .parameters_mut()
                .insert("amount".into(), HostBinding::Gpu(parameter));
        }

        let extraction = timeline.extract_selection_for_composition().unwrap();
        let moved_track = extraction
            .timeline
            .tracks
            .iter()
            .find(|item| item.id == track)
            .unwrap();
        assert_eq!(moved_track.pipeline.as_ref().unwrap().pipeline, Some(88));
        assert_eq!(
            moved_track
                .pipeline
                .as_ref()
                .unwrap()
                .overrides
                .get(456, "gain")
                .unwrap()
                .scalar_keys(0)
                .iter()
                .map(|key| key.time)
                .collect::<Vec<_>>(),
            vec![0.0, 2.0]
        );
        assert_eq!(
            moved_track
                .composite
                .opacity
                .scalar_keys(0)
                .iter()
                .map(|key| key.time)
                .collect::<Vec<_>>(),
            vec![0.0, 2.0]
        );

        let moved = &extraction.timeline.clips[0];
        assert_eq!(moved.start, 0.0);
        assert!(
            !moved
                .pipeline
                .overrides
                .iter()
                .any(|(_, _, binding)| binding.has_keyframes())
        );
        assert!(!moved.composite.opacity.has_keyframes());
        assert!(!moved.model3d.position.has_keyframes());
        let moved_row = moved_track
            .property_row(&moved.source, moved.source_instance)
            .unwrap();
        assert_eq!(moved_row.pipeline.pipeline, Some(77));
        assert_eq!(
            moved_row
                .pipeline
                .overrides
                .get(123, "amount")
                .unwrap()
                .scalar_keys(0)
                .iter()
                .map(|key| key.time)
                .collect::<Vec<_>>(),
            vec![0.0, 2.0]
        );
        assert_eq!(
            moved_row
                .composite
                .opacity
                .scalar_keys(0)
                .iter()
                .map(|key| key.time)
                .collect::<Vec<_>>(),
            vec![0.0, 2.0]
        );
        assert_eq!(
            moved_row
                .model3d
                .position
                .scalar_keys(0)
                .iter()
                .map(|key| key.time)
                .collect::<Vec<_>>(),
            vec![0.0, 2.0]
        );
        let VisualSource::Generator(source) = &moved_row.source else {
            panic!("generator row moved as generator")
        };
        assert_eq!(
            source
                .parameters()
                .get("amount")
                .unwrap()
                .scalar_keys(0)
                .iter()
                .map(|key| key.time)
                .collect::<Vec<_>>(),
            vec![0.0, 2.0]
        );

        let track_count_before_reference = timeline.tracks.len();
        timeline.insert_composition_reference(
            42,
            "Nested",
            &extraction,
            PipelineInstance::effect_default(),
        );
        assert_eq!(timeline.tracks.len(), track_count_before_reference + 1);
        assert_eq!(
            timeline
                .tracks
                .iter()
                .find(|item| item.id == track)
                .unwrap()
                .pipeline
                .as_ref()
                .unwrap()
                .pipeline,
            Some(88)
        );
        let reference = timeline.selected_clip().unwrap();
        assert!(
            timeline
                .tracks
                .iter()
                .find(|item| item.id == reference.track)
                .unwrap()
                .pipeline
                .is_none()
        );
    }

    #[test]
    fn clipboard_paste_rebases_source_row_keyframes_across_compositions() {
        let mut timeline = TimelineState::default();
        insert_test_generator(&mut timeline);
        timeline.selected_clip_mut().unwrap().start = 3.0;
        timeline.playhead = 3.0;
        timeline.toggle_selected_opacity_keyframe();
        timeline.playhead = 4.0;
        timeline.set_selected_opacity(0.5);
        timeline.copy_selection();
        timeline.load_document_preserving_clipboard(TimelineDocument::default());
        timeline.playhead = 10.0;
        timeline.paste();

        let pasted = timeline.selected_clip().unwrap();
        assert_eq!(pasted.start, 10.0);
        let row = timeline
            .tracks
            .iter()
            .find(|track| track.id == pasted.track)
            .and_then(|track| track.property_row(&pasted.source, pasted.source_instance))
            .unwrap();
        assert_eq!(
            row.composite
                .opacity
                .scalar_keys(0)
                .iter()
                .map(|key| key.time)
                .collect::<Vec<_>>(),
            vec![10.0, 11.0]
        );
    }

    #[test]
    fn context_menu_exposes_all_track_types_and_effect_clip_insertion() {
        let items = context_items(
            ContextKind::Track {
                id: 1,
                kind: TrackKind::Effect,
            },
            true,
        );
        for kind in [TrackKind::Video, TrackKind::Audio, TrackKind::Effect] {
            assert!(items.iter().any(|item| matches!(
                item.action,
                ContextCommand::AddTrack(candidate) if candidate == kind
            )));
        }
        assert!(
            items
                .iter()
                .any(|item| matches!(item.action, ContextCommand::InsertEffectHere))
        );
        assert!(
            items
                .iter()
                .any(|item| matches!(item.action, ContextCommand::DeleteTrack))
        );

        let empty_items = context_items(
            ContextKind::Empty {
                time: 0.0,
                track: Some(0),
                kind: Some(TrackKind::Effect),
            },
            true,
        );
        assert!(
            empty_items
                .iter()
                .any(|item| matches!(item.action, ContextCommand::InsertEffectHere))
        );
        assert!(
            empty_items
                .iter()
                .any(|item| matches!(item.action, ContextCommand::AddTrack(TrackKind::Effect)))
        );
    }

    #[test]
    fn new_track_is_inserted_above_target_track() {
        let mut timeline = TimelineState::default();
        let original = timeline.tracks[0].id;
        let inserted = timeline.add_track(TrackKind::Audio, Some(0));
        assert_eq!(inserted, 0);
        assert_eq!(timeline.tracks[0].kind, TrackKind::Audio);
        assert_eq!(timeline.tracks[1].id, original);
    }

    #[test]
    fn effect_clip_requires_and_uses_an_effect_track() {
        let mut timeline = TimelineState::default();
        let video_track = track_id(&timeline, TrackKind::Video);
        assert!(!timeline.insert_effect_clip_at(video_track, 0.0, None));

        let effect_index = timeline.add_track(TrackKind::Effect, Some(0));
        let effect_track = timeline.tracks[effect_index].id;
        assert!(timeline.insert_effect_clip_at(effect_track, 0.0, None));
        let clip = timeline.selected_clip().unwrap();
        assert_eq!(clip.track, effect_track);
        assert!(matches!(&clip.source, VisualSource::EffectInput));
        assert!(clip.pipeline.transform().is_none());
    }

    #[test]
    fn cut_at_playhead_keeps_linked_halves_in_separate_groups() {
        let mut timeline = TimelineState::default();
        let video_track = track_id(&timeline, TrackKind::Video);
        assert!(timeline.insert_av_composition_clip_at(
            (video_track, 0.0),
            42,
            "Nested".into(),
            true,
            4.0,
            PipelineInstance::effect_default(),
        ));
        let left_group = timeline.selected_clip().unwrap().group.unwrap();
        timeline.playhead = 2.0;
        timeline.cut_at_playhead();

        let left = timeline
            .clips
            .iter()
            .filter(|clip| clip.start == 0.0)
            .collect::<Vec<_>>();
        let right = timeline
            .clips
            .iter()
            .filter(|clip| clip.start == 2.0)
            .collect::<Vec<_>>();
        assert_eq!(left.len(), 2);
        assert_eq!(right.len(), 2);
        assert!(left.iter().all(|clip| clip.group == Some(left_group)));
        let right_group = right[0].group.unwrap();
        assert_ne!(right_group, left_group);
        assert!(right.iter().all(|clip| clip.group == Some(right_group)));
        assert!(
            right
                .iter()
                .all(|clip| (clip.source_offset - 2.0).abs() < 1e-6)
        );
        assert_eq!(timeline.selected.len(), right.len());
        assert!(
            right
                .iter()
                .all(|clip| timeline.selected.contains(&clip.id))
        );
        assert!(
            left.iter()
                .all(|clip| !timeline.selected.contains(&clip.id))
        );
        assert_eq!(timeline.selected_clip().unwrap().start, 2.0);
    }

    #[test]
    fn close_gap_compacts_selected_clips_without_changing_duration() {
        let mut timeline = TimelineState::default();
        let first = insert_test_generator_named(&mut timeline, 0.0, "First");
        let second = insert_test_generator_named(&mut timeline, 8.0, "Second");
        timeline.selected = [first, second].into_iter().collect();
        timeline.primary_selected = Some(second);
        timeline.close_selected_gaps();
        let second = timeline
            .clips
            .iter()
            .find(|clip| clip.id == second)
            .unwrap();
        assert_eq!(second.start, 5.0);
        assert_eq!(second.duration, 5.0);
    }

    #[test]
    fn close_gap_compacts_every_selected_clip() {
        let mut timeline = TimelineState::default();
        let mut ids = Vec::new();
        for (time, name) in [(0.0, "First"), (8.0, "Second"), (17.0, "Third")] {
            ids.push(insert_test_generator_named(&mut timeline, time, name));
        }
        timeline.selected = ids.iter().copied().collect();
        timeline.primary_selected = ids.last().copied();

        timeline.close_selected_gaps();

        let starts = ids
            .iter()
            .map(|id| {
                timeline
                    .clips
                    .iter()
                    .find(|clip| clip.id == *id)
                    .unwrap()
                    .start
            })
            .collect::<Vec<_>>();
        assert_eq!(starts, vec![0.0, 5.0, 10.0]);
    }

    #[test]
    fn total_speed_duration_distributes_evenly_over_logical_clips() {
        let mut timeline = TimelineState::default();
        for (time, name) in [(0.0, "First"), (8.0, "Second")] {
            insert_test_generator_named(&mut timeline, time, name);
        }
        let ids = timeline
            .clips
            .iter()
            .map(|clip| clip.id)
            .collect::<Vec<_>>();
        timeline.selected = ids.iter().copied().collect();
        timeline.clips[0].duration = 2.0;
        timeline.clips[1].duration = 4.0;
        assert_eq!(timeline.selected_total_logical_duration(), 6.0);
        timeline.apply_speed_duration(&Project::new(), SpeedDurationMode::TotalDuration, 10.0);
        assert!(
            timeline
                .clips
                .iter()
                .all(|clip| (clip.duration - 5.0).abs() < 1e-6)
        );
    }

    #[test]
    fn alt_drag_duplication_assigns_fresh_clip_and_group_ids() {
        let mut timeline = TimelineState::default();
        let video_track = track_id(&timeline, TrackKind::Video);
        assert!(timeline.insert_av_composition_clip_at(
            (video_track, 0.0),
            42,
            "Nested".into(),
            true,
            4.0,
            PipelineInstance::effect_default(),
        ));
        let original_ids = timeline.selected.clone();
        let original_group = timeline.selected_clip().unwrap().group.unwrap();
        timeline.duplicate_selection_for_drag();
        assert_eq!(timeline.clips.len(), 4);
        assert_eq!(timeline.selected.len(), 2);
        assert!(timeline.selected.is_disjoint(&original_ids));
        let duplicate_group = timeline.selected_clip().unwrap().group.unwrap();
        assert_ne!(duplicate_group, original_group);
        assert!(
            timeline
                .clips
                .iter()
                .filter(|clip| timeline.selected.contains(&clip.id))
                .all(|clip| clip.group == Some(duplicate_group))
        );
    }

    #[test]
    fn power_duplicate_repeats_the_moved_offset() {
        let mut timeline = TimelineState::default();
        insert_test_generator(&mut timeline);

        timeline.power_duplicate();
        assert_eq!(timeline.clips.len(), 2);
        let first_duplicate = timeline.selected_clip_id().unwrap();
        timeline
            .clips
            .iter_mut()
            .find(|clip| clip.id == first_duplicate)
            .unwrap()
            .start = 7.0;

        timeline.power_duplicate();
        assert_eq!(timeline.clips.len(), 3);
        assert!((timeline.selected_clip().unwrap().start - 14.0).abs() < 1e-6);

        timeline.power_duplicate();
        assert_eq!(timeline.clips.len(), 4);
        assert!((timeline.selected_clip().unwrap().start - 21.0).abs() < 1e-6);
    }

    #[test]
    fn select_before_and_after_playhead_stays_on_current_track() {
        let mut timeline = TimelineState::default();
        for (time, name) in [(0.0, "Before"), (6.0, "Crossing"), (12.0, "After")] {
            insert_test_generator_named(&mut timeline, time, name);
        }
        let track = timeline.tracks[0].id;
        let before = timeline.clips[0].id;
        let after = timeline.clips[2].id;
        timeline.playhead = 8.0;
        timeline.selected.clear();
        timeline.primary_selected = None;
        timeline.selected_track = Some(track);

        timeline.select_on_current_track(&LayoutSnapshot::default(), false);
        assert_eq!(timeline.selected, HashSet::from([before]));

        timeline.select_on_current_track(&LayoutSnapshot::default(), true);
        assert_eq!(timeline.selected, HashSet::from([after]));
    }

    #[test]
    fn replacing_selected_media_keeps_clip_timing_and_speed() {
        let mut timeline = TimelineState::default();
        let video_track = track_id(&timeline, TrackKind::Video);
        assert!(timeline.insert_media_clip_at(
            (video_track, 3.0),
            11,
            "Old".into(),
            false,
            Some(20.0),
            PipelineInstance::effect_default(),
        ));
        let clip = timeline.selected_clip_mut().unwrap();
        clip.duration = 7.5;
        clip.speed = 1.75;
        clip.source_offset = 2.25;
        clip.fade_in = 0.4;
        clip.fade_out = 0.6;
        let before = (
            clip.start,
            clip.duration,
            clip.speed,
            clip.source_offset,
            clip.fade_in,
            clip.fade_out,
        );

        assert_eq!(
            timeline.replace_selected_media_source(22, false, "Replacement"),
            1
        );
        let clip = timeline.selected_clip().unwrap();
        assert!(matches!(clip.source, VisualSource::Media(22)));
        assert_eq!(clip.name, "Replacement");
        assert_eq!(
            (
                clip.start,
                clip.duration,
                clip.speed,
                clip.source_offset,
                clip.fade_in,
                clip.fade_out,
            ),
            before
        );
    }

    #[test]
    fn playback_starts_from_the_displayed_playhead_without_old_frame_time() {
        let mut timeline = TimelineState {
            edit: TimelineEditState {
                document: TimelineDocument {
                    view: TimelineViewState {
                        playhead: 5.0,
                        ..TimelineViewState::default()
                    },
                    ..TimelineDocument::default()
                },
                selected: HashSet::new(),
                primary_selected: None,
                clipboard: Vec::new(),
            },
            selection_frame: Instant::now() - Duration::from_secs(2),
            ..TimelineState::default()
        };

        timeline.toggle_playback();
        timeline.tick(&LayoutSnapshot::default(), 30.0);

        assert_eq!(timeline.playhead, 5.0);
    }

    #[test]
    fn source_row_keyframes_are_authored_in_composition_time() {
        let mut timeline = TimelineState::default();
        insert_test_generator(&mut timeline);
        let clip_id = timeline.selected_clip_id().unwrap();
        let clip = timeline
            .clips
            .iter_mut()
            .find(|clip| clip.id == clip_id)
            .unwrap();
        clip.start = 10.0;
        clip.duration = 10.0;
        timeline.playhead = 12.0;

        timeline.toggle_selected_opacity_keyframe();
        let clip = timeline
            .clips
            .iter()
            .find(|clip| clip.id == clip_id)
            .unwrap();
        let row = timeline
            .tracks
            .iter()
            .find(|track| track.id == clip.track)
            .and_then(|track| track.property_row(&clip.source, clip.source_instance))
            .unwrap();
        assert!(row.composite.opacity.has_keyframe(12.0));
        assert!(!row.composite.opacity.has_keyframe(2.0));

        timeline
            .clips
            .iter_mut()
            .find(|clip| clip.id == clip_id)
            .unwrap()
            .start = 20.0;
        let clip = timeline
            .clips
            .iter()
            .find(|clip| clip.id == clip_id)
            .unwrap();
        let row = timeline
            .tracks
            .iter()
            .find(|track| track.id == clip.track)
            .and_then(|track| track.property_row(&clip.source, clip.source_instance))
            .unwrap();
        assert!(row.composite.opacity.has_keyframe(12.0));
    }

    #[test]
    fn same_media_keyframes_share_layer_property_rows() {
        let mut timeline = TimelineState::default();
        let track = track_id(&timeline, TrackKind::Video);
        for (time, playhead) in [(0.0, 1.0), (6.0, 7.0)] {
            assert!(timeline.insert_media_clip_at(
                (track, time),
                42,
                "Repeated Media".into(),
                false,
                Some(5.0),
                PipelineInstance::effect_default(),
            ));
            timeline.playhead = playhead;
            timeline.toggle_selected_opacity_keyframe();
            timeline.toggle_selected_blend_mode_keyframe();
        }

        let track_index = timeline.track_index(track).unwrap();
        let lanes = timeline.build_keyframe_lanes_for_track(track_index);
        let media_groups = lanes
            .chunk_by(|left, right| left.id.group == right.id.group)
            .filter(|group| {
                matches!(
                    group[0].id.group.owner,
                    KeyframeGroupOwner::Target(KeyframeOwner::SourceRow { .. })
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(media_groups.len(), 2);
        assert!(
            media_groups
                .iter()
                .all(|group| group.len() == 1 && group[0].points.len() == 2)
        );
        assert!(media_groups.iter().all(|group| {
            group[0]
                .points
                .iter()
                .map(|point| point.time)
                .eq([1.0, 7.0])
        }));
        assert!(
            media_groups
                .iter()
                .any(|group| { matches!(group[0].id.group.property, KeyframeProperty::Opacity) })
        );
        assert!(
            media_groups
                .iter()
                .any(|group| { matches!(group[0].id.group.property, KeyframeProperty::BlendMode) })
        );

        let opacity = &media_groups
            .iter()
            .find(|group| matches!(group[0].id.group.property, KeyframeProperty::Opacity))
            .unwrap()[0];
        let second = opacity.points[1];
        assert!(matches!(
            opacity.id.target.owner,
            KeyframeOwner::SourceRow { .. }
        ));
        assert!(timeline.edit_keyframe_lane_key(&opacity.id, second.time, None, Some(0.25), None,));
        assert_eq!(
            timeline
                .tracks
                .iter()
                .find(|candidate| candidate.id == track)
                .and_then(|track| {
                    track.property_rows.iter().find(|row| {
                        matches!(
                            row.source,
                            VisualSource::Media(42) | VisualSource::Audio(42)
                        )
                    })
                })
                .unwrap()
                .composite
                .opacity(second.time),
            0.25,
        );
        assert!(timeline.clips.iter().all(|clip| {
            !clip.composite.opacity.has_keyframes() && !clip.composite.blend_mode.has_keyframes()
        }));
    }

    #[test]
    fn independent_generator_insertions_get_distinct_layer_rows() {
        let mut timeline = TimelineState::default();
        let first = insert_test_generator_named(&mut timeline, 0.0, "Generator A");
        timeline.playhead = 1.0;
        timeline.set_generator_value("amount", GpuValue::F32(0.25));
        timeline.toggle_generator_keyframe("amount");

        let second = insert_test_generator_named(&mut timeline, 6.0, "Generator B");
        timeline.playhead = 7.0;
        timeline.set_generator_value("amount", GpuValue::F32(0.75));
        timeline.toggle_generator_keyframe("amount");

        let first_clip = timeline.clips.iter().find(|clip| clip.id == first).unwrap();
        let second_clip = timeline
            .clips
            .iter()
            .find(|clip| clip.id == second)
            .unwrap();
        assert_eq!(first_clip.track, second_clip.track);
        assert_ne!(first_clip.source_instance, second_clip.source_instance);
        let track = timeline
            .tracks
            .iter()
            .find(|track| track.id == first_clip.track)
            .unwrap();
        let first_row = track
            .property_rows
            .iter()
            .find(|row| row.matches(first_clip))
            .unwrap();
        let second_row = track
            .property_rows
            .iter()
            .find(|row| row.matches(second_clip))
            .unwrap();
        assert!(!first_row.matches(second_clip));
        let VisualSource::Generator(first_generator) = &first_row.source else {
            panic!("first generator row lost its source");
        };
        let VisualSource::Generator(second_generator) = &second_row.source else {
            panic!("second generator row lost its source");
        };
        assert!(
            first_generator
                .host_binding("amount")
                .unwrap()
                .has_keyframe(1.0)
        );
        assert!(
            !first_generator
                .host_binding("amount")
                .unwrap()
                .has_keyframe(7.0)
        );
        assert!(
            second_generator
                .host_binding("amount")
                .unwrap()
                .has_keyframe(7.0)
        );
        assert!(
            !second_generator
                .host_binding("amount")
                .unwrap()
                .has_keyframe(1.0)
        );
    }

    #[test]
    fn duplicated_generator_occurrences_share_one_layer_row() {
        let mut timeline = TimelineState::default();
        let first = insert_test_generator_named(&mut timeline, 0.0, "Generator");
        timeline.playhead = 1.0;
        timeline.set_generator_value("amount", GpuValue::F32(0.25));
        timeline.toggle_generator_keyframe("amount");

        let second = timeline.duplicate_selection()[0];
        timeline
            .clips
            .iter_mut()
            .find(|clip| clip.id == second)
            .unwrap()
            .start = 6.0;
        timeline.playhead = 7.0;
        timeline.toggle_generator_keyframe("amount");
        timeline.set_generator_value("amount", GpuValue::F32(0.75));

        let first_clip = timeline.clips.iter().find(|clip| clip.id == first).unwrap();
        let second_clip = timeline
            .clips
            .iter()
            .find(|clip| clip.id == second)
            .unwrap();
        assert_eq!(first_clip.source_instance, second_clip.source_instance);
        let track = timeline
            .tracks
            .iter()
            .find(|track| track.id == first_clip.track)
            .unwrap();
        assert_eq!(
            track
                .property_rows
                .iter()
                .filter(|row| row.matches(first_clip))
                .count(),
            1
        );
        let row = track
            .property_rows
            .iter()
            .find(|row| row.matches(first_clip))
            .unwrap();
        assert!(row.matches(second_clip));
        let VisualSource::Generator(generator) = &row.source else {
            panic!("generator row lost its source");
        };
        let binding = generator.host_binding("amount").unwrap();
        assert!(binding.has_keyframe(1.0));
        assert!(binding.has_keyframe(7.0));
    }

    #[test]
    fn legacy_clip_animation_is_absorbed_by_the_media_row() {
        let mut timeline = TimelineState::default();
        let track = track_id(&timeline, TrackKind::Video);
        for start in [0.0, 6.0] {
            assert!(timeline.insert_media_clip_at(
                (track, start),
                42,
                "Repeated Media".into(),
                false,
                Some(5.0),
                PipelineInstance::effect_default(),
            ));
        }
        let mut legacy = timeline.document();
        for track in &mut legacy.tracks {
            track.property_rows.clear();
        }
        for (clip, time) in legacy.clips.iter_mut().zip([1.0, 7.0]) {
            clip.composite.opacity.toggle_keyframe(time);
            clip.composite
                .opacity
                .set_value(time, GpuValue::F32(time as f32 / 10.0));
        }

        let loaded = TimelineState::from_document(legacy);
        let row = loaded
            .tracks
            .iter()
            .find(|candidate| candidate.id == track)
            .and_then(|track| {
                track.property_rows.iter().find(|row| {
                    matches!(
                        row.source,
                        VisualSource::Media(42) | VisualSource::Audio(42)
                    )
                })
            })
            .unwrap();
        assert!(row.composite.opacity.has_keyframe(1.0));
        assert!(row.composite.opacity.has_keyframe(7.0));
        assert!(
            loaded
                .clips
                .iter()
                .all(|clip| !clip.composite.opacity.has_keyframes())
        );
    }

    #[test]
    fn v5_clip_owned_generators_migrate_without_merging_instances() {
        let mut timeline = TimelineState::default();
        let first = insert_test_generator_named(&mut timeline, 0.0, "Generator A");
        let second = insert_test_generator_named(&mut timeline, 6.0, "Generator B");
        let mut legacy = timeline.document();
        legacy.next_source_instance = 0;
        for track in &mut legacy.tracks {
            track.property_rows.clear();
        }
        for (clip_id, time, value) in [(first, 1.0, 0.25), (second, 7.0, 0.75)] {
            let clip = legacy
                .clips
                .iter_mut()
                .find(|clip| clip.id == clip_id)
                .unwrap();
            clip.source_instance = 0;
            let VisualSource::Generator(generator) = &mut clip.source else {
                panic!("legacy generator clip lost its source");
            };
            let mut binding = Binding::Constant(GpuValue::F32(value));
            binding.toggle_keyframe(time);
            binding.set_value(time, GpuValue::F32(value));
            generator
                .parameters_mut()
                .insert("amount".into(), HostBinding::Gpu(binding));
        }

        let loaded = TimelineState::from_document(legacy);
        let first_clip = loaded.clips.iter().find(|clip| clip.id == first).unwrap();
        let second_clip = loaded.clips.iter().find(|clip| clip.id == second).unwrap();
        assert_ne!(first_clip.source_instance, second_clip.source_instance);
        let track = loaded
            .tracks
            .iter()
            .find(|track| track.id == first_clip.track)
            .unwrap();
        let first_row = track
            .property_rows
            .iter()
            .find(|row| row.matches(first_clip))
            .unwrap();
        let second_row = track
            .property_rows
            .iter()
            .find(|row| row.matches(second_clip))
            .unwrap();
        assert!(!first_row.matches(second_clip));
        let VisualSource::Generator(first_generator) = &first_row.source else {
            panic!("first migrated row lost its generator source");
        };
        let VisualSource::Generator(second_generator) = &second_row.source else {
            panic!("second migrated row lost its generator source");
        };
        assert!(
            first_generator
                .host_binding("amount")
                .unwrap()
                .has_keyframe(1.0)
        );
        assert!(
            !first_generator
                .host_binding("amount")
                .unwrap()
                .has_keyframe(7.0)
        );
        assert!(
            second_generator
                .host_binding("amount")
                .unwrap()
                .has_keyframe(7.0)
        );
        assert!(
            !second_generator
                .host_binding("amount")
                .unwrap()
                .has_keyframe(1.0)
        );
        assert!(loaded.clips.iter().all(|clip| {
            match &clip.source {
                VisualSource::Generator(generator) => generator
                    .parameters()
                    .values()
                    .all(|binding| !binding.has_keyframes()),
                _ => true,
            }
        }));
    }

    #[test]
    fn v5_media_rows_migrate_into_the_owning_layer() {
        let mut timeline = TimelineState::default();
        let track = track_id(&timeline, TrackKind::Video);
        assert!(timeline.insert_media_clip_at(
            (track, 0.0),
            42,
            "Legacy Media".into(),
            false,
            Some(5.0),
            PipelineInstance::effect_default(),
        ));
        timeline.playhead = 2.0;
        timeline.toggle_selected_opacity_keyframe();

        let mut encoded = serde_json::to_value(timeline.document()).unwrap();
        encoded
            .as_object_mut()
            .unwrap()
            .remove("next_source_instance");
        for clip in encoded["clips"].as_array_mut().unwrap() {
            clip.as_object_mut().unwrap().remove("source_instance");
        }
        let tracks = encoded
            .get_mut("tracks")
            .and_then(serde_json::Value::as_array_mut)
            .unwrap();
        let track_json = tracks
            .iter_mut()
            .find(|value| value.get("id").and_then(serde_json::Value::as_u64) == Some(track as u64))
            .unwrap();
        let row = track_json
            .get_mut("property_rows")
            .and_then(serde_json::Value::as_array_mut)
            .unwrap()
            .remove(0);
        let mut row = row.as_object().unwrap().clone();
        row.remove("source");
        row.remove("source_instance");
        row.insert("track".into(), serde_json::Value::from(track));
        row.insert("media".into(), serde_json::Value::from(42_u64));
        encoded.as_object_mut().unwrap().insert(
            "media_property_rows".into(),
            serde_json::Value::Array(vec![serde_json::Value::Object(row)]),
        );

        let legacy: TimelineDocument = serde_json::from_value(encoded).unwrap();
        let loaded = TimelineState::from_document(legacy);
        let layer = loaded
            .tracks
            .iter()
            .find(|candidate| candidate.id == track)
            .unwrap();
        assert_eq!(layer.property_rows.len(), 1);
        let row = layer.property_row(&VisualSource::Media(42), 0).unwrap();
        assert!(row.composite.opacity.has_keyframe(2.0));
        assert!(
            loaded
                .clips
                .iter()
                .all(|clip| !clip.composite.opacity.has_keyframes())
        );

        let saved = serde_json::to_value(loaded.document()).unwrap();
        assert!(saved.get("media_property_rows").is_none());
        assert_eq!(
            saved["tracks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|value| value["id"].as_u64() == Some(track as u64))
                .unwrap()["property_rows"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn compositor_enums_can_be_stepped_keyframed() {
        let mut timeline = TimelineState::default();
        insert_test_generator(&mut timeline);
        timeline.toggle_selected_blend_mode_keyframe();
        assert!(timeline.selected_blend_mode_has_keyframe());
        timeline.cycle_selected_blend_mode(1);
        assert_eq!(
            timeline.selected_blend_mode(),
            Some(crate::project::BlendMode::Add)
        );

        timeline.toggle_selected_alpha_blend_mode_keyframe();
        assert!(timeline.selected_alpha_blend_mode_has_keyframe());
        timeline.set_selected_alpha_blend_mode(1);
        assert_eq!(
            timeline.selected_alpha_blend_mode(),
            Some(crate::project::AlphaBlendMode::PreserveDestination)
        );
    }
    #[test]
    fn audio_track_mix_is_keyframe_backed_and_serialized() {
        let mut timeline = TimelineState::default();
        let track = track_id(&timeline, TrackKind::Audio);
        timeline.set_track_mix(track, MixerParameter::Volume, 0.42);
        timeline.set_track_mix(track, MixerParameter::Pan, -0.25);
        timeline.toggle_mixer_keyframe(track, MixerParameter::Pan);
        assert_eq!(timeline.track_mix(track), [0.42, -0.25]);
        assert!(timeline.mixer_has_keyframe(track, MixerParameter::Pan));
        let encoded = serde_json::to_string(&timeline.document()).unwrap();
        let document: TimelineDocument = serde_json::from_str(&encoded).unwrap();
        let loaded = TimelineState::from_document(document);
        assert_eq!(loaded.track_mix(track), [0.42, -0.25]);
    }

    #[test]
    fn playback_only_stops_at_explicit_timeline_end() {
        let snapshot = LayoutSnapshot::default();
        let mut timeline = TimelineState {
            edit: TimelineEditState {
                document: TimelineDocument {
                    view: TimelineViewState {
                        playhead: 5.0,
                        ..TimelineViewState::default()
                    },
                    ..TimelineDocument::default()
                },
                selected: HashSet::new(),
                primary_selected: None,
                clipboard: Vec::new(),
            },
            playing: true,
            selection_frame: Instant::now() - Duration::from_millis(20),
            ..TimelineState::default()
        };
        timeline.tick(&snapshot, 30.0);
        assert!(timeline.playhead > 5.0);
        assert!(timeline.playing);

        timeline.end_time = Some(timeline.playhead + 0.005);
        timeline.selection_frame = Instant::now() - Duration::from_millis(20);
        let end = timeline.end_time.unwrap();
        timeline.tick(&snapshot, 30.0);
        assert_eq!(timeline.playhead, end);
        assert!(!timeline.playing);

        timeline.end_behavior = EndBehavior::Restart;
        timeline.playhead = end - 0.005;
        timeline.playing = true;
        timeline.selection_frame = Instant::now() - Duration::from_millis(20);
        timeline.tick(&snapshot, 30.0);
        assert_eq!(timeline.playhead, 0.0);
        assert!(timeline.playing);
    }

    #[test]
    fn pipeline_values_are_shared_until_explicitly_made_unique() {
        let mut project = Project::new();
        let pipeline = project.create_pipeline();
        project
            .pipeline_mut(pipeline)
            .unwrap()
            .nodes
            .push(crate::effects::EffectNode {
                id: 41,
                node_type: "test.effect".into(),
                execution: crate::effects::NodeExecution::PointwiseGpu,
                ui_position: None,
                image_inputs: std::collections::BTreeMap::new(),
                stack_input: None,
                inputs: std::collections::BTreeMap::from([(
                    "amount".into(),
                    Binding::Constant(GpuValue::F32(1.0)),
                )]),
                host_inputs: std::collections::BTreeMap::new(),
                dynamic_image_inputs: None,
            });
        let mut timeline = TimelineState::default();
        timeline.selected_track = Some(timeline.tracks[0].id);
        let mut instance = PipelineInstance::effect_default();
        instance.pipeline = Some(pipeline);
        timeline.tracks[0].pipeline = Some(instance);
        timeline.set_pipeline_input_value(&mut project, 41, "amount", GpuValue::F32(2.5));
        assert!(!timeline.pipeline_input_is_override(41, "amount"));
        assert_eq!(
            project.pipeline(pipeline).unwrap().node(41).unwrap().inputs["amount"].evaluate(0.0),
            Some(GpuValue::F32(2.5)),
        );

        assert!(timeline.make_pipeline_input_unique(&project, 41, "amount"));
        assert!(timeline.pipeline_input_is_override(41, "amount"));
        timeline.set_pipeline_input_value(&mut project, 41, "amount", GpuValue::F32(4.0));
        assert_eq!(
            timeline.pipeline_input_value(&project, 41, "amount"),
            Some(GpuValue::F32(4.0)),
        );
        assert_eq!(
            project.pipeline(pipeline).unwrap().node(41).unwrap().inputs["amount"].evaluate(0.0),
            Some(GpuValue::F32(2.5)),
        );
        assert!(timeline.use_shared_pipeline_input(41, "amount"));
        assert_eq!(
            timeline.pipeline_input_value(&project, 41, "amount"),
            Some(GpuValue::F32(2.5)),
        );
    }
}
