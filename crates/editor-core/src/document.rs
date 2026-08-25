use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{
    effects::{Binding, GpuValue},
    parameters::HostBinding,
};

macro_rules! labeled_enum {
    ($(#[$meta:meta])* $vis:vis enum $name:ident {
        $($(#[$variant_meta:meta])* $variant:ident => $label:expr),+ $(,)?
    }) => {
        $(#[$meta])*
        $vis enum $name { $( $(#[$variant_meta])* $variant, )+ }

        impl $name {
            pub const ALL: [Self; labeled_enum!(@count $($variant),+)] = [$(Self::$variant),+];
            pub const fn label(self) -> &'static str {
                match self { $(Self::$variant => $label),+ }
            }
        }
    };
    (@count $($variant:ident),+) => { <[()]>::len(&[$(labeled_enum!(@unit $variant)),+]) };
    (@unit $variant:ident) => { () };
}

pub const MAX_CANVAS_DIMENSION: u32 = 16_384;
pub const MAX_FRAME_RATE: f64 = 1_000.0;

pub type MediaId = u64;
pub type CompositionId = u64;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum TrackKind {
    Video,
    Audio,
    Effect,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum EndBehavior {
    #[default]
    Stop,
    Restart,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct TimelineViewState {
    pub pixels_per_second: f32,
    pub scroll_time: f64,
    pub scroll_y: f32,
    pub playhead: f32,
    pub frame_snap: bool,
    pub grid_snap: bool,
    pub clip_snap: bool,
    pub playhead_snap: bool,
    pub follow_playhead: bool,
}

impl Default for TimelineViewState {
    fn default() -> Self {
        Self {
            pixels_per_second: 82.0,
            scroll_time: 0.0,
            scroll_y: 0.0,
            playhead: 0.0,
            frame_snap: true,
            grid_snap: true,
            clip_snap: true,
            playhead_snap: true,
            follow_playhead: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompositionSettings {
    pub canvas_size: [u32; 2],
    pub frame_rate: f64,
    pub background: ProjectBackground,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub enum ProjectBackground {
    Transparent,
    Solid { color: [f32; 4] },
}

impl Default for ProjectBackground {
    fn default() -> Self {
        Self::Solid {
            color: [0.0, 0.0, 0.0, 1.0],
        }
    }
}

impl CompositionSettings {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.canvas_size
                .iter()
                .all(|&dimension| (1..=MAX_CANVAS_DIMENSION).contains(&dimension)),
            "composition canvas dimensions must be between 1 and {MAX_CANVAS_DIMENSION}"
        );
        anyhow::ensure!(
            self.frame_rate.is_finite() && (1.0..=MAX_FRAME_RATE).contains(&self.frame_rate),
            "composition frame rate must be between 1 and {MAX_FRAME_RATE}"
        );
        let color = match self.background {
            ProjectBackground::Transparent => None,
            ProjectBackground::Solid { color } => Some(color),
        };
        anyhow::ensure!(
            color.is_none_or(|color| color.into_iter().all(f32::is_finite)),
            "composition background contains a non-finite color component"
        );
        Ok(())
    }
}

impl Default for CompositionSettings {
    fn default() -> Self {
        Self {
            canvas_size: [1920, 1080],
            frame_rate: 60.0,
            background: ProjectBackground::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum MediaKind {
    Image { width: u32, height: u32 },
    Video,
    Audio,
    Model3d,
    WasmPlugin,
    Unknown,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum Model3dShading {
    #[default]
    Unlit,
    Pbr,
}

impl Model3dShading {
    pub const OPTIONS: [&'static str; 2] = ["Unlit", "PBR"];

    #[must_use]
    pub const fn from_index(index: usize) -> Self {
        if index == 1 {
            Self::Pbr
        } else {
            Self::Unlit
        }
    }

    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Unlit => 0,
            Self::Pbr => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum MediaTrackKind {
    Video,
    Audio,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MediaTrackInfo {
    pub kind: MediaTrackKind,

    pub stream_index: usize,
    pub codec: String,
    pub bit_rate: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub frame_rate: Option<f64>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WaveformData {
    pub video: Option<VideoWaveform>,
    pub audio: Option<AudioWaveform>,
}

impl WaveformData {
    #[must_use]
    pub fn sample_count(&self) -> usize {
        self.video
            .as_ref()
            .map(|video| video.activity.len())
            .or_else(|| {
                self.audio
                    .as_ref()
                    .and_then(|audio| audio.bands.first().map(Vec::len))
            })
            .unwrap_or(0)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VideoWaveform {
    pub colors: Vec<[u8; 3]>,
    pub activity: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AudioWaveform {
    pub bands: [Vec<u8>; 6],
}

#[doc(hidden)]
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
pub struct LegacyModel3dSettings {
    #[serde(default = "legacy_model3d_size")]
    pub size: [f32; 3],
    #[serde(default = "legacy_model3d_scale")]
    pub scale: [f32; 3],
    #[serde(default)]
    pub rotation: [f32; 3],
    #[serde(default)]
    pub shading: Model3dShading,
}

const fn legacy_model3d_size() -> [f32; 3] {
    [2.0, 2.0, 2.0]
}

const fn legacy_model3d_scale() -> [f32; 3] {
    [1.0, 1.0, 1.0]
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MediaAsset {
    pub id: MediaId,
    pub name: String,
    pub path: PathBuf,
    pub kind: MediaKind,
    pub duration: Option<f64>,
    pub frame_rate: Option<f64>,
    pub video_width: Option<u32>,
    pub video_height: Option<u32>,
    pub has_audio: bool,
    #[serde(default)]
    pub tracks: Vec<MediaTrackInfo>,
    pub waveform: Option<Arc<WaveformData>>,
    #[doc(hidden)]
    #[serde(default, rename = "model", skip_serializing)]
    pub legacy_model: Option<LegacyModel3dSettings>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum GeneratorSource {
    Plugin {
        generator_type: String,
        parameters: BTreeMap<String, HostBinding>,
    },
    Wasm {
        plugin_id: String,
        module: PathBuf,
        entry: String,
        parameters: BTreeMap<String, HostBinding>,
    },
}

impl GeneratorSource {
    #[must_use]
    pub const fn parameters(&self) -> &BTreeMap<String, HostBinding> {
        match self {
            Self::Plugin { parameters, .. } | Self::Wasm { parameters, .. } => parameters,
        }
    }

    pub const fn parameters_mut(&mut self) -> &mut BTreeMap<String, HostBinding> {
        match self {
            Self::Plugin { parameters, .. } | Self::Wasm { parameters, .. } => parameters,
        }
    }

    #[must_use]
    pub fn host_binding(&self, input: &str) -> Option<&HostBinding> {
        self.parameters().get(input)
    }

    pub fn host_binding_mut(&mut self, input: &str) -> Option<&mut HostBinding> {
        self.parameters_mut().get_mut(input)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum VisualSource {
    Media(MediaId),
    Composition(CompositionId),

    Audio(MediaId),
    Generator(GeneratorSource),

    EffectInput,
    AudioPlaceholder,
}

impl VisualSource {
    #[must_use]
    pub const fn is_audio(&self) -> bool {
        matches!(self, Self::Audio(_) | Self::AudioPlaceholder)
    }

    #[must_use]
    pub const fn is_effect_input(&self) -> bool {
        matches!(self, Self::EffectInput)
    }

    #[must_use]
    pub const fn is_renderable_visual(&self) -> bool {
        !self.is_audio() && !self.is_effect_input()
    }
}

labeled_enum! {
    #[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
    pub enum BlendMode {
        Normal => "Normal",
        Add => "Add",
        Subtract => "Subtract",
        Multiply => "Multiply",
        Screen => "Screen",
        Overlay => "Overlay",
        Difference => "Difference",
        Darken => "Darken",
        Lighten => "Lighten",
        ColorDodge => "Color Dodge",
        ColorBurn => "Color Burn",
        HardLight => "Hard Light",
        SoftLight => "Soft Light",
        Exclusion => "Exclusion",
        LinearBurn => "Linear Burn",
        Divide => "Divide",
    }
}

labeled_enum! {
    #[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
    pub enum AlphaBlendMode {
        SourceOver => "Source Over",
        PreserveDestination => "Preserve Destination",
        Replace => "Replace",
        Add => "Add",
        Subtract => "Subtract",
        Multiply => "Multiply",
        Min => "Min",
        Max => "Max",
    }
}

const fn default_alpha_blend_binding() -> Binding {
    Binding::Constant(GpuValue::Enum(0))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LayerComposite {
    pub opacity: Binding,
    pub blend_mode: Binding,
    #[serde(default = "default_alpha_blend_binding")]
    pub alpha_blend_mode: Binding,
}

impl Default for LayerComposite {
    fn default() -> Self {
        Self {
            opacity: Binding::Constant(GpuValue::F32(1.0)),
            blend_mode: Binding::Constant(GpuValue::Enum(0)),
            alpha_blend_mode: default_alpha_blend_binding(),
        }
    }
}

impl LayerComposite {
    pub fn opacity(&self, time: f64) -> f32 {
        self.opacity
            .evaluate(time)
            .and_then(GpuValue::f32)
            .unwrap_or(1.0)
            .clamp(0.0, 1.0)
    }

    pub fn blend_mode(&self, time: f64) -> BlendMode {
        let index = self
            .blend_mode
            .evaluate(time)
            .and_then(GpuValue::enum_index)
            .unwrap_or(0) as usize;
        BlendMode::ALL[index.min(BlendMode::ALL.len() - 1)]
    }

    pub fn alpha_blend_mode(&self, time: f64) -> AlphaBlendMode {
        let index = self
            .alpha_blend_mode
            .evaluate(time)
            .and_then(GpuValue::enum_index)
            .unwrap_or(0) as usize;
        AlphaBlendMode::ALL[index.min(AlphaBlendMode::ALL.len() - 1)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composition_defaults_are_valid() {
        let settings = CompositionSettings::default();
        assert_eq!(settings.canvas_size, [1920, 1080]);
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn composition_validation_rejects_invalid_dimensions_and_rate() {
        let mut settings = CompositionSettings::default();
        settings.canvas_size[0] = 0;
        assert!(settings.validate().is_err());
        settings.canvas_size[0] = 1920;
        settings.frame_rate = f64::NAN;
        assert!(settings.validate().is_err());
    }

    #[test]
    fn waveform_sample_count_uses_available_stream_data() {
        let waveform = WaveformData {
            video: Some(VideoWaveform {
                colors: vec![[0; 3]; 3],
                activity: vec![0; 3],
            }),
            audio: None,
        };
        assert_eq!(waveform.sample_count(), 3);
    }
}
