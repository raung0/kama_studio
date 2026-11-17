mod discovery;
mod lv2_editor;
mod runtime;

pub use discovery::{discover, rescan, run_scanner};
pub use runtime::{
    NativeProcessor, clear, drain_updates, open_editor, pump, retain_project, synchronize,
};

use std::{collections::BTreeMap, path::PathBuf};

use serde::{Deserialize, Serialize};

pub const DESCRIPTOR_INPUT: &str = "__native_plugin";
pub const STATE_INPUT: &str = "__native_state";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Format {
    Vst3,
    Vst2,
    Lv2,
    Clap,
}

impl Format {
    pub const ALL: [Self; 4] = [Self::Vst3, Self::Vst2, Self::Lv2, Self::Clap];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Vst3 => "VST3",
            Self::Vst2 => "VST2",
            Self::Lv2 => "LV2",
            Self::Clap => "CLAP",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchPaths {
    pub vst3: String,
    pub vst2: String,
    pub lv2: String,
    pub clap: String,
}

impl Default for SearchPaths {
    fn default() -> Self {
        #[cfg(target_os = "macos")]
        let roots = [
            "~/Library/Audio/Plug-Ins/VST3;/Library/Audio/Plug-Ins/VST3",
            "~/Library/Audio/Plug-Ins/VST;/Library/Audio/Plug-Ins/VST",
            "~/.lv2;~/Library/Audio/Plug-Ins/LV2;/Library/Audio/Plug-Ins/LV2;/usr/local/lib/lv2",
            "~/Library/Audio/Plug-Ins/CLAP;/Library/Audio/Plug-Ins/CLAP",
        ];
        #[cfg(target_os = "windows")]
        let roots = [
            "%COMMONPROGRAMFILES%\\VST3;%LOCALAPPDATA%\\Programs\\Common\\VST3",
            "%PROGRAMFILES%\\VstPlugins;%PROGRAMFILES%\\Steinberg\\VstPlugins;%COMMONPROGRAMFILES%\\VST2",
            "%APPDATA%\\LV2;%COMMONPROGRAMFILES%\\LV2",
            "%COMMONPROGRAMFILES%\\CLAP;%LOCALAPPDATA%\\Programs\\Common\\CLAP",
        ];
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let roots = [
            "~/.vst3;/usr/local/lib/vst3;/usr/lib/vst3",
            "~/.vst;~/.lxvst;/usr/local/lib/vst;/usr/lib/vst;/usr/lib/lxvst",
            "~/.lv2;/usr/local/lib/lv2;/usr/lib/lv2;/usr/lib64/lv2",
            "~/.clap;/usr/local/lib/clap;/usr/lib/clap",
        ];
        Self {
            vst3: roots[0].into(),
            vst2: roots[1].into(),
            lv2: roots[2].into(),
            clap: roots[3].into(),
        }
    }
}

impl SearchPaths {
    pub fn get(&self, format: Format) -> &str {
        match format {
            Format::Vst3 => &self.vst3,
            Format::Vst2 => &self.vst2,
            Format::Lv2 => &self.lv2,
            Format::Clap => &self.clap,
        }
    }

    pub fn directories(&self, format: Format) -> Vec<PathBuf> {
        self.get(format)
            .split(';')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| {
                let mut value = value.to_owned();
                if let Some(rest) = value.strip_prefix("~/") {
                    if let Some(home) = std::env::var_os("HOME") {
                        return PathBuf::from(home).join(rest);
                    }
                }
                for name in [
                    "COMMONPROGRAMFILES",
                    "PROGRAMFILES",
                    "LOCALAPPDATA",
                    "APPDATA",
                ] {
                    if let Ok(path) = std::env::var(name) {
                        value = value.replace(&format!("%{name}%"), &path);
                    }
                }
                PathBuf::from(value)
            })
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Parameter {
    pub id: u32,
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub step: Option<f64>,
    pub unit: String,
}

impl Parameter {
    pub fn input_id(&self) -> String {
        format!("param_{}", self.id)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Descriptor {
    pub format: Format,
    pub path: PathBuf,
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub parameters: Vec<Parameter>,
    pub has_editor: bool,
    #[serde(default)]
    pub lv2_editor: Option<lv2_editor::Ui>,
}

impl Descriptor {
    pub fn key(&self) -> String {
        format!("native.{}.{}", self.format.label(), self.id)
    }

    pub fn title(&self) -> String {
        format!("{} ({})", self.name, self.format.label())
    }

    pub fn definition(&self) -> crate::plugin::AudioEffectDefinition {
        use crate::plugin::{InputType, PluginInput};
        crate::plugin::AudioEffectDefinition {
            key: self.key(),
            name: self.title(),
            category: self.format.label().into(),
            description: self.vendor.clone(),
            module: self.path.clone(),
            entry: self.id.clone(),
            view: None,
            native: Some(self.clone()),
            inputs: self
                .parameters
                .iter()
                .map(|parameter| PluginInput {
                    id: parameter.input_id(),
                    name: parameter.name.clone(),
                    ty: InputType::F32,
                    default: toml::Value::Float(parameter.default),
                    min: Some(parameter.min as f32),
                    max: Some(parameter.max as f32),
                    options: Vec::new(),
                    suffix: parameter.unit.clone(),
                    step: parameter.step.map(|value| value as f32),
                    precision: Some(3),
                    visible_when: None,
                    monitor_handle: None,
                    pen_tool: false,
                    pen_closed: false,
                    monitor_colors: None,
                    monitor_midpoints: None,
                    monitor_resize_transform: false,
                })
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Update {
    pub pipeline: u64,
    pub node: u64,
    pub parameters: BTreeMap<String, f32>,
    pub state: Option<String>,
}
