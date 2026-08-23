use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::{
    effects::{
        Binding, DynamicImageInputs, EffectNode, GpuValue, ImageBinding, NodeExecution,
        PipelineInstance, LOCAL_TRANSFORM_NODE_ID,
    },
    embedded_vfs, messages,
    project::{GeneratorSource, HostBinding, HostValue},
    runtime::wasm::{
        plugin_parameter_hash, AudioWasmRuntime, WasmRenderRequest, WasmRuntime,
        DEFAULT_RENDER_EXPORT,
    },
    shader_codegen::{
        build_fused_pointwise_shader, build_generator_shader, build_standalone_shader,
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectKind {
    Pointwise,
    Standalone,
}

impl EffectKind {
    pub fn execution(self) -> NodeExecution {
        match self {
            Self::Pointwise => NodeExecution::PointwiseGpu,
            Self::Standalone => NodeExecution::SpatialGpu,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectRole {
    VisualTransform,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeProperty {
    OutputSize,
    SourceSize,
    TimelineTime,
    LocalTime,
    Frame,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputType {
    F32,
    Angle,
    I32,
    U32,
    Bool,
    Vec2,
    Vec2i,
    Vec2Array,
    F32List,
    Vec3,
    Vec4,
    Color,
    Enum,
    Text,
}

impl InputType {
    pub fn wgsl_read(self, slot: usize) -> Option<String> {
        Some(match self {
            Self::F32 | Self::Angle => format!("effect_params.data[{slot}u].x"),
            Self::I32 => format!("i32(effect_params.data[{slot}u].x)"),
            Self::U32 | Self::Enum => format!("u32(max(effect_params.data[{slot}u].x, 0.0) + 0.5)"),
            Self::Bool => format!("effect_params.data[{slot}u].x >= 0.5"),
            Self::Vec2 => format!("effect_params.data[{slot}u].xy"),
            Self::Vec2i => format!("vec2<i32>(round(effect_params.data[{slot}u].xy))"),
            Self::Vec2Array | Self::F32List => return None,
            Self::Vec3 => format!("effect_params.data[{slot}u].xyz"),
            Self::Vec4 | Self::Color => format!("effect_params.data[{slot}u]"),
            Self::Text => return None,
        })
    }

    pub fn default_gpu(self, value: &toml::Value) -> Result<GpuValue> {
        fn number(value: &toml::Value) -> Result<f32> {
            let value = value
                .as_float()
                .map(|value| value as f32)
                .or_else(|| value.as_integer().map(|value| value as f32))
                .context("expected numeric plugin default")?;
            if !value.is_finite() {
                bail!("plugin default must be finite");
            }
            Ok(value)
        }
        fn vector<const N: usize>(value: &toml::Value) -> Result<[f32; N]> {
            let values = value.as_array().context("expected plugin default array")?;
            if values.len() != N {
                bail!(
                    "expected {N} values in plugin default, got {}",
                    values.len()
                );
            }
            let mut output = [0.0; N];
            for (index, value) in values.iter().enumerate() {
                output[index] = number(value)?;
            }
            Ok(output)
        }
        Ok(match self {
            Self::F32 | Self::Angle => GpuValue::F32(number(value)?),
            Self::I32 => GpuValue::I32(number(value)?.round() as i32),
            Self::U32 => GpuValue::U32(number(value)?.round().max(0.0) as u32),
            Self::Bool => GpuValue::Bool(value.as_bool().context("expected bool plugin default")?),
            Self::Vec2 | Self::Vec2i => GpuValue::Vec2(vector::<2>(value)?),
            Self::Vec2Array => bail!("vec2_array is host-only and cannot become a GPU value"),
            Self::F32List => bail!("f32_list is host-only and cannot become a GPU value"),
            Self::Vec3 => GpuValue::Vec3(vector::<3>(value)?),
            Self::Vec4 => GpuValue::Vec4(vector::<4>(value)?),
            Self::Color => GpuValue::Color(vector::<4>(value)?),
            Self::Enum => GpuValue::Enum(number(value)?.round().max(0.0) as u32),
            Self::Text => bail!("text is host-only and cannot become a GPU value"),
        })
    }

    pub fn default_host(self, value: &toml::Value) -> Result<HostBinding> {
        if self == Self::Text {
            return Ok(HostBinding::Constant(HostValue::String(
                value
                    .as_str()
                    .context("expected string plugin default")?
                    .to_owned(),
            )));
        }
        if self == Self::Vec2Array {
            let values = value
                .as_array()
                .context("expected plugin vec2 array default")?;
            let mut points = Vec::with_capacity(values.len());
            for value in values {
                let pair = value
                    .as_array()
                    .context("expected [x, y] point in vec2 array default")?;
                if pair.len() != 2 {
                    bail!(
                        "expected 2 values in vec2 array point default, got {}",
                        pair.len()
                    );
                }
                let number = |value: &toml::Value| {
                    value
                        .as_float()
                        .map(|value| value as f32)
                        .or_else(|| value.as_integer().map(|value| value as f32))
                        .context("expected numeric vec2 array component")
                };
                points.push([number(&pair[0])?, number(&pair[1])?]);
            }
            return Ok(HostBinding::Constant(HostValue::Vec2Array(points)));
        }
        if self == Self::F32List {
            let values = value
                .as_array()
                .context("expected plugin f32 list default")?;
            let mut output = Vec::with_capacity(values.len());
            for value in values {
                let number = value
                    .as_float()
                    .map(|value| value as f32)
                    .or_else(|| value.as_integer().map(|value| value as f32))
                    .context("expected numeric f32 list value")?;
                if !number.is_finite() {
                    bail!("plugin f32 list defaults must be finite");
                }
                output.push(number);
            }
            return Ok(HostBinding::Constant(HostValue::F32List(output)));
        }
        Ok(HostBinding::Gpu(Binding::Constant(
            self.default_gpu(value)?,
        )))
    }
}

#[derive(Clone, Debug)]
pub struct InputVisibility {
    pub input: String,
    pub equals: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MonitorHandleMode {
    Points,
    Size,
    Radius,
}

#[derive(Clone, Debug)]
pub struct MonitorWasmDefinition {
    pub module: PathBuf,
    pub entry: String,
}

#[derive(Clone, Debug)]
pub struct PluginInput {
    pub id: String,
    pub name: String,
    pub ty: InputType,
    pub default: toml::Value,
    pub min: Option<f32>,
    pub max: Option<f32>,
    pub options: Vec<String>,
    pub suffix: String,
    pub step: Option<f32>,
    pub precision: Option<usize>,
    pub visible_when: Option<InputVisibility>,
    pub monitor_handle: Option<MonitorHandleMode>,
    pub pen_tool: bool,
    pub pen_closed: bool,
    pub monitor_colors: Option<String>,
    pub monitor_midpoints: Option<String>,
    pub monitor_resize_transform: bool,
}

impl PluginInput {
    pub fn is_visible_with(&self, value: impl FnOnce(&str) -> Option<GpuValue>) -> bool {
        let Some(condition) = &self.visible_when else {
            return true;
        };
        value(&condition.input)
            .and_then(GpuValue::enum_index)
            .is_some_and(|current| current == condition.equals)
    }
}

fn validate_plugin_inputs(
    key: &str,
    inputs: &[PluginInput],
    allow_host_only: bool,
    hashed_parameters: bool,
    reserve_enabled: bool,
) -> Result<()> {
    let mut ids = HashSet::new();
    let mut hashes = HashMap::new();
    for input in inputs {
        if input.id.trim().is_empty() || input.name.trim().is_empty() {
            bail!("plugin {key} has an input with an empty id/name");
        }
        if !ids.insert(input.id.as_str()) {
            bail!("plugin {key} declares duplicate input {}", input.id);
        }
        if hashed_parameters {
            let hash = plugin_parameter_hash(&input.id);
            if let Some(previous) = hashes.insert(hash, input.id.as_str()) {
                bail!(
                    "plugin {key} inputs {previous} and {} have the same host parameter hash",
                    input.id
                );
            }
        }
        if reserve_enabled && input.id == "enabled" {
            bail!("plugin {key} may not declare reserved input `enabled`");
        }
        if !allow_host_only
            && matches!(
                input.ty,
                InputType::Text | InputType::Vec2Array | InputType::F32List
            )
        {
            bail!(
                "plugin {key} input {} uses unsupported host-only type {:?}",
                input.id,
                input.ty
            );
        }
        input
            .ty
            .default_host(&input.default)
            .with_context(|| format!("plugin {key} input {} has an invalid default", input.id))?;
        if input.min.is_some_and(|value| !value.is_finite())
            || input.max.is_some_and(|value| !value.is_finite())
            || input
                .step
                .is_some_and(|value| !value.is_finite() || value <= 0.0)
        {
            bail!(
                "plugin {key} input {} has invalid numeric bounds/step",
                input.id
            );
        }
        if input.min.zip(input.max).is_some_and(|(min, max)| min > max) {
            bail!("plugin {key} input {} has min greater than max", input.id);
        }
        if input.monitor_resize_transform && input.monitor_handle != Some(MonitorHandleMode::Size) {
            bail!(
                "plugin {key} input {} enables transform-aware resizing without size handles",
                input.id
            );
        }
        if input.ty == InputType::Enum {
            if input.options.is_empty() {
                bail!("plugin {key} enum input {} has no options", input.id);
            }
            let mut options = HashSet::new();
            for option in &input.options {
                if option.trim().is_empty() {
                    bail!("plugin {key} enum input {} has an empty option", input.id);
                }
                if !options.insert(option.as_str()) {
                    bail!(
                        "plugin {key} enum input {} declares duplicate option {option}",
                        input.id
                    );
                }
            }
            let selected = input
                .ty
                .default_gpu(&input.default)?
                .enum_index()
                .unwrap_or(u32::MAX) as usize;
            if selected >= input.options.len() {
                bail!(
                    "plugin {key} enum input {} has an out-of-range default",
                    input.id
                );
            }
        }
    }
    for input in inputs {
        for (role, linked) in [
            ("monitor colors", input.monitor_colors.as_deref()),
            ("monitor midpoints", input.monitor_midpoints.as_deref()),
        ] {
            let Some(linked) = linked else {
                continue;
            };
            if input.monitor_handle != Some(MonitorHandleMode::Points) {
                bail!(
                    "plugin {key} input {} declares {role} without point monitor handles",
                    input.id
                );
            }
            let Some(companion) = inputs.iter().find(|candidate| candidate.id == linked) else {
                bail!(
                    "plugin {key} input {} {role} reference missing input {linked}",
                    input.id
                );
            };
            if companion.ty != InputType::F32List {
                bail!(
                    "plugin {key} input {} {role} input {linked} must be f32_list",
                    input.id
                );
            }
        }
        let Some(condition) = &input.visible_when else {
            continue;
        };
        let Some(controller) = inputs
            .iter()
            .find(|candidate| candidate.id == condition.input)
        else {
            bail!(
                "plugin {key} input {} visibility references missing input {}",
                input.id,
                condition.input
            );
        };
        if controller.ty != InputType::Enum || condition.equals as usize >= controller.options.len()
        {
            bail!(
                "plugin {key} input {} has invalid visibility condition on {}",
                input.id,
                condition.input
            );
        }
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct PluginImageInput {
    pub id: String,
    pub name: String,
    pub required: bool,
}

fn instantiate_plugin_inputs(
    key: &str,
    inputs: &[PluginInput],
) -> Result<BTreeMap<String, Binding>> {
    let mut bindings =
        BTreeMap::from([("enabled".into(), Binding::Constant(GpuValue::Bool(true)))]);
    for input in inputs {
        if matches!(
            input.ty,
            InputType::Text | InputType::Vec2Array | InputType::F32List
        ) {
            bail!(
                "plugin node {} declares unsupported host-only input {}",
                key,
                input.id
            );
        }
        if input.id != "enabled" {
            bindings.insert(
                input.id.clone(),
                Binding::Constant(input.ty.default_gpu(&input.default)?),
            );
        }
    }
    Ok(bindings)
}

#[derive(Clone, Debug)]
pub struct EffectDefinition {
    pub key: String,
    pub plugin_id: String,
    pub id: String,
    pub name: String,
    pub category: String,
    pub kind: EffectKind,
    pub role: Option<EffectRole>,
    pub source: String,
    pub entry: String,
    pub uses: Vec<RuntimeProperty>,
    pub image_inputs: Vec<PluginImageInput>,
    pub dynamic_image_inputs: Option<DynamicImageInputs>,
    pub inputs: Vec<PluginInput>,
    pub monitor: Option<MonitorWasmDefinition>,
}

impl EffectDefinition {
    pub fn is_stack_insertable(&self) -> bool {
        !self.image_inputs.is_empty()
    }

    pub fn primary_image_input(&self) -> Option<&str> {
        self.image_inputs
            .iter()
            .find(|input| input.required)
            .or_else(|| self.image_inputs.first())
            .map(|input| input.id.as_str())
    }

    pub fn namespace_for_node(&self, node_id: u64) -> String {
        format!(
            "plugin_{}_{}_n{}",
            shader_ident(&self.plugin_id),
            shader_ident(&self.id),
            node_id
        )
    }

    pub fn instantiate(&self, node_id: u64) -> Result<EffectNode> {
        let mut node = EffectNode {
            id: node_id,
            node_type: self.key.clone(),
            execution: self.kind.execution(),
            ui_position: None,
            image_inputs: self
                .image_inputs
                .iter()
                .map(|input| (input.id.clone(), ImageBinding::Disconnected))
                .collect(),
            stack_input: self.primary_image_input().map(str::to_owned),
            inputs: instantiate_plugin_inputs(&self.key, &self.inputs)?,
            host_inputs: BTreeMap::new(),
            dynamic_image_inputs: self.dynamic_image_inputs.clone(),
        };
        node.sync_dynamic_image_inputs();
        Ok(node)
    }
}

#[derive(Clone, Debug)]
pub struct AudioEffectDefinition {
    pub key: String,
    pub name: String,
    pub category: String,
    pub description: String,
    pub module: PathBuf,
    pub entry: String,
    pub inputs: Vec<PluginInput>,
    pub view: Option<String>,
}

impl AudioEffectDefinition {
    pub fn instantiate(&self, node_id: u64) -> Result<EffectNode> {
        Ok(EffectNode {
            id: node_id,
            node_type: self.key.clone(),

            execution: NodeExecution::SpatialGpu,
            ui_position: None,
            image_inputs: BTreeMap::from([("audio".into(), ImageBinding::Disconnected)]),
            stack_input: Some("audio".into()),
            inputs: {
                let mut inputs =
                    BTreeMap::from([("enabled".into(), Binding::Constant(GpuValue::Bool(true)))]);
                for input in &self.inputs {
                    if !matches!(
                        input.ty,
                        InputType::Text | InputType::Vec2Array | InputType::F32List
                    ) {
                        inputs.insert(
                            input.id.clone(),
                            Binding::Constant(input.ty.default_gpu(&input.default)?),
                        );
                    }
                }
                inputs
            },
            host_inputs: {
                let mut inputs = BTreeMap::new();
                for input in &self.inputs {
                    if matches!(
                        input.ty,
                        InputType::Text | InputType::Vec2Array | InputType::F32List
                    ) {
                        inputs.insert(input.id.clone(), input.ty.default_host(&input.default)?);
                    }
                }
                inputs
            },
            dynamic_image_inputs: None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GeneratorBackend {
    Gpu,
    Wasm,
}

#[derive(Clone, Debug)]
pub struct GeneratorBounds {
    pub points_input: String,
    pub padding_input: Option<String>,
    pub padding_inputs: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct GeneratorDefinition {
    pub key: String,
    pub plugin_id: String,
    pub id: String,
    pub name: String,
    pub description: String,
    pub backend: GeneratorBackend,

    pub uses_time: bool,
    pub bounds: Option<GeneratorBounds>,
    pub source: Option<String>,
    pub module: Option<PathBuf>,
    pub entry: Option<String>,
    pub monitor_entry: Option<String>,
    pub monitor_module: Option<PathBuf>,
    pub inputs: Vec<PluginInput>,
}

impl GeneratorDefinition {
    pub fn wasm_export(&self) -> Option<(&Path, &str)> {
        if self.backend != GeneratorBackend::Wasm {
            return None;
        }
        self.module.as_deref().map(|module| {
            (
                module,
                self.entry.as_deref().unwrap_or(DEFAULT_RENDER_EXPORT),
            )
        })
    }

    pub fn instantiate_parameters(&self) -> Result<BTreeMap<String, HostBinding>> {
        self.inputs
            .iter()
            .map(|input| Ok((input.id.clone(), input.ty.default_host(&input.default)?)))
            .collect()
    }

    pub fn instantiate(&self) -> Result<GeneratorSource> {
        Ok(GeneratorSource::Plugin {
            generator_type: self.key.clone(),
            parameters: self.instantiate_parameters()?,
        })
    }

    pub fn instantiate_graph_node(&self, node_id: u64) -> Result<EffectNode> {
        let mut inputs = BTreeMap::new();
        let mut host_inputs = BTreeMap::new();
        for input in &self.inputs {
            match input.ty {
                InputType::Text | InputType::Vec2Array | InputType::F32List => {
                    host_inputs.insert(input.id.clone(), input.ty.default_host(&input.default)?);
                }
                _ => {
                    inputs.insert(
                        input.id.clone(),
                        Binding::Constant(input.ty.default_gpu(&input.default)?),
                    );
                }
            }
        }
        let execution = match self.backend {
            GeneratorBackend::Gpu => NodeExecution::GeneratorGpu,
            GeneratorBackend::Wasm => NodeExecution::GeneratorCpu,
        };
        Ok(EffectNode {
            id: node_id,
            node_type: self.key.clone(),
            execution,
            ui_position: None,
            image_inputs: BTreeMap::new(),
            stack_input: None,
            inputs,
            host_inputs,
            dynamic_image_inputs: None,
        })
    }
}

#[derive(Clone, Default)]
pub struct PluginRegistry {
    effects: HashMap<String, EffectDefinition>,
    audio_effects: HashMap<String, AudioEffectDefinition>,
    generators: HashMap<String, GeneratorDefinition>,
}

impl PluginRegistry {
    pub fn load_default(configured_paths: &str) -> Result<Self> {
        let mut registry = Self::default();
        let builtin_root = PathBuf::from(embedded_vfs::BUILTIN_PLUGIN_ROOT);
        registry.load_root(&builtin_root)?;
        let mut seen = HashSet::from([builtin_root]);
        for root in configured_paths
            .split(';')
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
        {
            if !seen.insert(root.clone()) {
                continue;
            }
            if let Err(error) = registry.load_root(&root) {
                messages::error(
                    "Plugin loader",
                    format!("plugin path {} skipped: {error:#}", root.display()),
                );
            }
        }
        registry.validate_wasm();
        Ok(registry)
    }

    fn load_root(&mut self, root: &Path) -> Result<()> {
        let mut manifests = if let Some(manifests) = embedded_vfs::plugin_manifests(root)? {
            manifests
        } else {
            let mut manifests = Vec::new();
            let direct_manifest = root.join("plugin.toml");
            if direct_manifest.is_file() {
                manifests.push(direct_manifest);
            }
            for entry in fs::read_dir(root)
                .with_context(|| format!("read plugin directory {}", root.display()))?
            {
                let path = entry?.path();
                let manifest = path.join("plugin.toml");
                if path.is_dir() && manifest.is_file() {
                    manifests.push(manifest);
                }
            }
            manifests
        };
        manifests.sort();
        for manifest in manifests {
            let checkpoint = self.clone();
            if let Err(error) = self.load_manifest(&manifest) {
                *self = checkpoint;
                messages::error(
                    "Plugin loader",
                    format!("{} disabled: {error:#}", manifest.display()),
                );
            }
        }
        Ok(())
    }

    pub fn validate_gpu(&mut self, device: &wgpu::Device) {
        let effect_keys = self.effects.keys().cloned().collect::<Vec<_>>();
        for key in effect_keys {
            let result = (|| -> Result<()> {
                let effect = self
                    .effects
                    .get(&key)
                    .context("effect disappeared during validation")?;
                let source = match effect.kind {
                    EffectKind::Pointwise => {
                        build_fused_pointwise_shader(std::slice::from_ref(&effect.key), self)?
                    }
                    EffectKind::Standalone => build_standalone_shader(&effect.key, self)?,
                };
                validate_wgpu_shader(device, &effect.key, &source)
            })();
            if let Err(error) = result {
                self.effects.remove(&key);
                messages::error(
                    "Plugin validation",
                    format!("effect {key} disabled: {error:#}"),
                );
            }
        }

        let generator_keys = self
            .generators
            .iter()
            .filter(|(_, generator)| generator.backend == GeneratorBackend::Gpu)
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in generator_keys {
            let result = self
                .generators
                .get(&key)
                .context("generator disappeared during validation")
                .and_then(build_generator_shader)
                .and_then(|source| validate_wgpu_shader(device, &key, &source));
            if let Err(error) = result {
                self.generators.remove(&key);
                messages::error(
                    "Plugin validation",
                    format!("generator {key} disabled: {error:#}"),
                );
            }
        }
    }

    fn validate_wasm(&mut self) {
        let audio_keys = self.audio_effects.keys().cloned().collect::<Vec<_>>();
        match AudioWasmRuntime::new() {
            Ok(mut runtime) => {
                for key in &audio_keys {
                    let definition = self.audio_effects.get(key).cloned();
                    let result = definition
                        .context("audio effect disappeared during validation")
                        .and_then(|effect| validate_audio_effect(&mut runtime, &effect));
                    if let Err(error) = result {
                        self.audio_effects.remove(key);
                        messages::error(
                            "Plugin validation",
                            format!("audio effect {key} disabled: {error:#}"),
                        );
                    }
                }
            }
            Err(error) => {
                if !audio_keys.is_empty() {
                    messages::error(
                        "Plugin validation",
                        format!(
                            "audio plugin runtime unavailable; audio effects disabled: {error:#}"
                        ),
                    );
                    self.audio_effects.clear();
                }
            }
        }

        let generator_keys = self
            .generators
            .iter()
            .filter(|(_, generator)| generator.backend == GeneratorBackend::Wasm)
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        match WasmRuntime::new() {
            Ok(mut runtime) => {
                for key in &generator_keys {
                    let definition = self.generators.get(key).cloned();
                    let result = definition
                        .context("generator disappeared during validation")
                        .and_then(|generator| validate_wasm_generator(&mut runtime, &generator));
                    if let Err(error) = result {
                        self.generators.remove(key);
                        messages::error(
                            "Plugin validation",
                            format!("generator {key} disabled: {error:#}"),
                        );
                    }
                }
            }
            Err(error) => {
                if !generator_keys.is_empty() {
                    messages::error(
                        "Plugin validation",
                        format!(
                            "WASM generator runtime unavailable; generators disabled: {error:#}"
                        ),
                    );
                    self.generators
                        .retain(|_, generator| generator.backend != GeneratorBackend::Wasm);
                }
            }
        }
    }

    pub fn effect(&self, key: &str) -> Option<&EffectDefinition> {
        self.effects.get(key)
    }

    pub fn effects(&self) -> impl Iterator<Item = &EffectDefinition> {
        sorted_definitions(self.effects.values())
    }

    pub fn audio_effect(&self, key: &str) -> Option<&AudioEffectDefinition> {
        self.audio_effects.get(key)
    }

    pub fn audio_effects(&self) -> impl Iterator<Item = &AudioEffectDefinition> {
        sorted_definitions(self.audio_effects.values())
    }

    pub fn generator(&self, key: &str) -> Option<&GeneratorDefinition> {
        self.generators.get(key)
    }

    pub fn visual_pipeline_instance(&self) -> Result<PipelineInstance> {
        let definition = self
            .effects
            .values()
            .find(|effect| effect.role == Some(EffectRole::VisualTransform))
            .context("no plugin provides the visual_transform effect role")?;
        let input = definition
            .primary_image_input()
            .map(str::to_owned)
            .context("visual_transform effect has no image input")?;
        let mut transform = definition.instantiate(LOCAL_TRANSFORM_NODE_ID)?;
        transform
            .image_inputs
            .insert(input, ImageBinding::PipelineInput);
        Ok(PipelineInstance {
            ui_input_position: None,
            ui_output_position: None,
            local_nodes: vec![transform],
            local_output: crate::effects::ImageBinding::Node(crate::effects::SocketRef {
                node: LOCAL_TRANSFORM_NODE_ID,
                output: "image".into(),
            }),
            pipeline: None,
            overrides: Default::default(),
        })
    }

    pub fn generators(&self) -> impl Iterator<Item = &GeneratorDefinition> {
        sorted_definitions(self.generators.values())
    }

    fn contains_definition(&self, key: &str) -> bool {
        self.effects.contains_key(key)
            || self.audio_effects.contains_key(key)
            || self.generators.contains_key(key)
    }

    fn load_manifest(&mut self, manifest_path: &Path) -> Result<()> {
        let text = read_plugin_text(manifest_path)?;
        let raw: RawManifest =
            toml::from_str(&text).with_context(|| format!("parse {}", manifest_path.display()))?;
        let base = manifest_path
            .parent()
            .context("plugin manifest has no parent")?;
        if raw.id.trim().is_empty() || raw.name.trim().is_empty() || raw.version.trim().is_empty() {
            bail!(
                "plugin manifest {} has an empty id/name/version",
                manifest_path.display()
            );
        }

        for effect in raw.effects {
            let key = format!("{}.{}", raw.id, effect.id);
            if self.contains_definition(&key) {
                bail!("duplicate plugin definition {key}");
            }
            if effect.id.trim().is_empty() || effect.name.trim().is_empty() {
                bail!("effect {key} has an empty id/name");
            }
            if effect.shader.trim().is_empty() || effect.entry.trim().is_empty() {
                bail!("effect {key} has an empty shader/entry");
            }
            let shader_path = base.join(&effect.shader);
            let source = read_plugin_text(&shader_path)
                .with_context(|| format!("read shader {}", shader_path.display()))?;
            let inputs: Vec<PluginInput> =
                effect.inputs.into_iter().map(PluginInput::from).collect();
            validate_plugin_inputs(&key, &inputs, false, false, true)?;
            if effect.role == Some(EffectRole::VisualTransform)
                && self
                    .effects
                    .values()
                    .any(|candidate| candidate.role == Some(EffectRole::VisualTransform))
            {
                bail!("multiple plugins provide the visual_transform effect role");
            }
            let image_inputs = effect.image_inputs.map_or_else(
                || {
                    vec![PluginImageInput {
                        id: "image".into(),
                        name: "Image".into(),
                        required: true,
                    }]
                },
                |inputs| {
                    inputs
                        .into_iter()
                        .map(|input| PluginImageInput {
                            id: input.id,
                            name: input.name,
                            required: input.required,
                        })
                        .collect()
                },
            );
            if effect.kind == EffectKind::Pointwise && image_inputs.len() != 1 {
                bail!("pointwise effect {key} must have exactly one image input");
            }
            if image_inputs.len() > 2 {
                bail!("effect {key} may declare at most two shader image inputs");
            }
            let mut image_ids = HashSet::new();
            for image in &image_inputs {
                if image.id.trim().is_empty() || image.name.trim().is_empty() {
                    bail!("effect {key} has an image input with an empty id/name");
                }
                if !image_ids.insert(image.id.as_str()) {
                    bail!("effect {key} declares duplicate image input {}", image.id);
                }
            }
            if let Some(dynamic) = &effect.dynamic_image_inputs {
                if dynamic.prefix.is_empty() || dynamic.min == 0 || dynamic.max < dynamic.min {
                    bail!("effect {key} has invalid dynamic image input limits");
                }
                let Some(count) = inputs.iter().find(|input| input.id == dynamic.count_input)
                else {
                    bail!(
                        "effect {key} dynamic image inputs reference missing count input {}",
                        dynamic.count_input
                    );
                };
                if !matches!(
                    count.ty,
                    InputType::U32 | InputType::I32 | InputType::F32 | InputType::Enum
                ) {
                    bail!("effect {key} dynamic image input count must be numeric");
                }
                if image_inputs.len() != 2 {
                    bail!("effect {key} dynamic compose shader must declare exactly two physical image inputs");
                }
            }
            let monitor = effect
                .monitor
                .map(|monitor| {
                    if monitor.module.trim().is_empty() || monitor.entry.trim().is_empty() {
                        bail!("effect {key} monitor module/export may not be empty");
                    }
                    Ok(MonitorWasmDefinition {
                        module: base.join(monitor.module),
                        entry: monitor.entry,
                    })
                })
                .transpose()?;
            self.effects.insert(
                key.clone(),
                EffectDefinition {
                    key,
                    plugin_id: raw.id.clone(),
                    id: effect.id,
                    name: effect.name,
                    category: normalize_category(effect.category.as_deref()),
                    kind: effect.kind,
                    role: effect.role,
                    source,
                    entry: effect.entry,
                    uses: effect.uses,
                    image_inputs,
                    dynamic_image_inputs: effect.dynamic_image_inputs.map(|dynamic| {
                        DynamicImageInputs {
                            count_input: dynamic.count_input,
                            prefix: dynamic.prefix,
                            min: dynamic.min.max(1),
                            max: dynamic.max.max(dynamic.min.max(1)),
                        }
                    }),
                    inputs,
                    monitor,
                },
            );
        }

        for effect in raw.audio_effects {
            let key = format!("{}.{}", raw.id, effect.id);
            if self.contains_definition(&key) {
                bail!("duplicate plugin definition {key}");
            }
            if effect.id.trim().is_empty() || effect.name.trim().is_empty() {
                bail!("audio effect {key} has an empty id/name");
            }
            if effect.module.trim().is_empty() || effect.entry.trim().is_empty() {
                bail!("audio effect {key} has an empty module/entry");
            }
            let module = base.join(&effect.module);
            let inputs = effect
                .inputs
                .into_iter()
                .map(PluginInput::from)
                .collect::<Vec<_>>();
            validate_plugin_inputs(&key, &inputs, true, true, true)?;
            self.audio_effects.insert(
                key.clone(),
                AudioEffectDefinition {
                    key,
                    name: effect.name,
                    category: normalize_category(effect.category.as_deref()),
                    description: effect.description.unwrap_or_default(),
                    module,
                    entry: effect.entry,
                    inputs,
                    view: effect.view,
                },
            );
        }

        for generator in raw.generators {
            let key = format!("{}.{}", raw.id, generator.id);
            if self.contains_definition(&key) {
                bail!("duplicate plugin definition {key}");
            }
            if generator.id.trim().is_empty() || generator.name.trim().is_empty() {
                bail!("generator {key} has an empty id/name");
            }
            if generator
                .entry
                .as_deref()
                .is_some_and(|entry| entry.trim().is_empty())
            {
                bail!("generator {key} has an empty entry");
            }
            let module = generator.module.map(|path| base.join(path));
            let source = generator
                .shader
                .as_ref()
                .map(|shader| {
                    let shader_path = base.join(shader);
                    read_plugin_text(&shader_path)
                        .with_context(|| format!("read generator shader {}", shader_path.display()))
                })
                .transpose()?;
            if generator.backend == GeneratorBackend::Gpu && source.is_none() {
                bail!("GPU generator {key} must declare a shader");
            }
            if generator.backend == GeneratorBackend::Wasm && module.is_none() {
                bail!("WASM generator {key} must declare a module");
            }
            let inputs: Vec<PluginInput> = generator
                .inputs
                .into_iter()
                .map(PluginInput::from)
                .collect();
            validate_plugin_inputs(
                &key,
                &inputs,
                generator.backend == GeneratorBackend::Wasm,
                generator.backend == GeneratorBackend::Wasm,
                false,
            )?;
            for input in &inputs {
                match input.monitor_handle {
                    Some(MonitorHandleMode::Points) if input.ty != InputType::Vec2Array => bail!(
                        "generator {key} input {} uses point handles but is not vec2_array",
                        input.id
                    ),
                    Some(MonitorHandleMode::Size | MonitorHandleMode::Radius)
                        if input.ty != InputType::Vec2 =>
                    {
                        bail!(
                            "generator {key} input {} uses vec2 monitor handles but is not vec2",
                            input.id
                        )
                    }
                    _ => {}
                }
                if input.pen_tool && input.monitor_handle != Some(MonitorHandleMode::Points) {
                    bail!(
                        "generator {key} input {} enables pen editing without point monitor handles",
                        input.id
                    );
                }
            }
            if inputs.iter().any(|input| input.monitor_handle.is_some()) {
                if generator
                    .monitor_entry
                    .as_deref()
                    .is_none_or(|entry| entry.trim().is_empty())
                {
                    bail!("generator {key} declares monitor handles without a WASM monitor export");
                }
                if module.is_none() && generator.monitor_module.is_none() {
                    bail!("generator {key} declares monitor handles without a WASM monitor module");
                }
            }
            if let Some(bounds) = &generator.bounds {
                let Some(points) = inputs.iter().find(|input| input.id == bounds.points_input)
                else {
                    bail!(
                        "generator {key} bounds reference missing points input {}",
                        bounds.points_input
                    );
                };
                if points.ty != InputType::Vec2Array {
                    bail!(
                        "generator {key} bounds points input {} must be vec2_array",
                        bounds.points_input
                    );
                }
                for padding_input in bounds
                    .padding_input
                    .iter()
                    .chain(bounds.padding_inputs.iter())
                {
                    let Some(padding) = inputs.iter().find(|input| input.id == *padding_input)
                    else {
                        bail!("generator {key} bounds reference missing padding input {padding_input}");
                    };
                    if !matches!(padding.ty, InputType::F32 | InputType::I32 | InputType::U32) {
                        bail!(
                            "generator {key} bounds padding input {padding_input} must be scalar"
                        );
                    }
                }
            }
            self.generators.insert(
                key.clone(),
                GeneratorDefinition {
                    key,
                    plugin_id: raw.id.clone(),
                    id: generator.id,
                    name: generator.name,
                    description: generator.description.unwrap_or_default(),
                    backend: generator.backend,
                    uses_time: generator.uses_time,
                    bounds: generator.bounds.map(|bounds| GeneratorBounds {
                        points_input: bounds.points_input,
                        padding_input: bounds.padding_input,
                        padding_inputs: bounds.padding_inputs,
                    }),
                    source,
                    module,
                    entry: generator.entry,
                    monitor_entry: generator.monitor_entry,
                    monitor_module: generator.monitor_module.map(|path| base.join(path)),
                    inputs,
                },
            );
        }

        Ok(())
    }
}

fn read_plugin_text(path: &Path) -> Result<String> {
    if let Some(text) = embedded_vfs::read_to_string(path)? {
        return Ok(text);
    }
    fs::read_to_string(path).with_context(|| format!("read {}", path.display()))
}

fn validate_audio_effect(
    runtime: &mut AudioWasmRuntime,
    effect: &AudioEffectDefinition,
) -> Result<()> {
    let mut processor = runtime
        .processor(&effect.module, &effect.entry, 1024)
        .with_context(|| format!("instantiate audio effect {}", effect.key))?;
    let parameters = Arc::new(
        effect
            .inputs
            .iter()
            .map(|input| {
                let binding = input.ty.default_host(&input.default)?;
                let value = binding.evaluate(0.0).with_context(|| {
                    format!("audio effect input {} has no default value", input.id)
                })?;
                Ok((plugin_parameter_hash(&input.id), value))
            })
            .collect::<Result<HashMap<_, _>>>()?,
    );
    let mut empty: [f32; 0] = [];
    processor
        .process(&mut empty, 2, 48_000, &parameters, true)
        .with_context(|| format!("smoke-test audio effect {}", effect.key))
}

fn validate_wasm_generator(
    runtime: &mut WasmRuntime,
    generator: &GeneratorDefinition,
) -> Result<()> {
    let (module, entry) = generator
        .wasm_export()
        .with_context(|| format!("WASM generator {} has no module/export", generator.key))?;
    runtime
        .precompile(module)
        .with_context(|| format!("compile WASM generator {}", generator.key))?;
    let parameters = generator
        .instantiate_parameters()
        .with_context(|| format!("instantiate defaults for WASM generator {}", generator.key))?;
    runtime
        .render(WasmRenderRequest {
            module_path: module,
            entry,
            parameters: &parameters,
            size: [1, 1],
            render_scale: 1.0,
            render_origin: [0.0, 0.0],
            tight_bounds: false,
            parameter_time: 0.0,
            local_time: 0.0,
        })
        .with_context(|| format!("smoke-test WASM generator {}", generator.key))?;
    Ok(())
}

fn validate_wgpu_shader(device: &wgpu::Device, key: &str, source: &str) -> Result<()> {
    let error_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(key),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let _pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(key),
        layout: None,
        module: &module,
        entry_point: Some("main"),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: None,
    });
    if let Some(error) = pollster::block_on(error_scope.pop()) {
        bail!("GPU effect {key} failed wgpu validation: {error}");
    }
    Ok(())
}

#[derive(Deserialize)]
struct RawManifest {
    id: String,
    name: String,
    version: String,
    #[serde(default)]
    effects: Vec<RawEffect>,
    #[serde(default)]
    audio_effects: Vec<RawAudioEffect>,
    #[serde(default)]
    generators: Vec<RawGenerator>,
}

#[derive(Deserialize)]
struct RawEffect {
    id: String,
    name: String,
    #[serde(default)]
    category: Option<String>,
    kind: EffectKind,
    #[serde(default)]
    role: Option<EffectRole>,
    shader: String,
    #[serde(default = "default_effect_entry")]
    entry: String,
    #[serde(default)]
    uses: Vec<RuntimeProperty>,
    #[serde(default)]
    image_inputs: Option<Vec<RawImageInput>>,
    #[serde(default)]
    dynamic_image_inputs: Option<RawDynamicImageInputs>,
    #[serde(default)]
    inputs: Vec<RawInput>,
    #[serde(default)]
    monitor: Option<RawMonitorWasm>,
}

#[derive(Deserialize)]
struct RawMonitorWasm {
    module: String,
    entry: String,
}

#[derive(Deserialize)]
struct RawDynamicImageInputs {
    count_input: String,
    prefix: String,
    #[serde(default = "default_dynamic_min")]
    min: usize,
    #[serde(default = "default_dynamic_max")]
    max: usize,
}

fn default_dynamic_min() -> usize {
    1
}
fn default_dynamic_max() -> usize {
    64
}

fn normalize_category(category: Option<&str>) -> String {
    category
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Other")
        .to_owned()
}

#[derive(Deserialize)]
struct RawAudioEffect {
    id: String,
    name: String,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    description: Option<String>,
    module: String,
    entry: String,
    #[serde(default)]
    view: Option<String>,
    #[serde(default)]
    inputs: Vec<RawInput>,
}

#[derive(Deserialize)]
struct RawGeneratorBounds {
    points_input: String,
    #[serde(default)]
    padding_input: Option<String>,
    #[serde(default)]
    padding_inputs: Vec<String>,
}

#[derive(Deserialize)]
struct RawGenerator {
    id: String,
    name: String,
    #[serde(default)]
    description: Option<String>,
    backend: GeneratorBackend,
    #[serde(default)]
    uses_time: bool,
    #[serde(default)]
    bounds: Option<RawGeneratorBounds>,
    #[serde(default)]
    shader: Option<String>,
    #[serde(default)]
    module: Option<String>,
    #[serde(default)]
    entry: Option<String>,
    #[serde(default)]
    monitor_entry: Option<String>,
    #[serde(default)]
    monitor_module: Option<String>,
    #[serde(default)]
    inputs: Vec<RawInput>,
}

#[derive(Deserialize)]
struct RawImageInput {
    id: String,
    name: String,
    #[serde(default = "default_true")]
    required: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
struct RawInputVisibility {
    input: String,
    equals: u32,
}

#[derive(Deserialize)]
struct RawInput {
    id: String,
    name: String,
    #[serde(rename = "type")]
    ty: InputType,
    default: toml::Value,
    #[serde(default)]
    min: Option<f32>,
    #[serde(default)]
    max: Option<f32>,
    #[serde(default)]
    options: Vec<String>,
    #[serde(default)]
    suffix: String,
    #[serde(default)]
    step: Option<f32>,
    #[serde(default)]
    precision: Option<usize>,
    #[serde(default)]
    visible_when: Option<RawInputVisibility>,
    #[serde(default)]
    monitor_handle: Option<MonitorHandleMode>,
    #[serde(default)]
    pen_tool: bool,
    #[serde(default = "default_true")]
    pen_closed: bool,
    #[serde(default)]
    monitor_colors: Option<String>,
    #[serde(default)]
    monitor_midpoints: Option<String>,
    #[serde(default)]
    monitor_resize_transform: bool,
}

impl From<RawInput> for PluginInput {
    fn from(value: RawInput) -> Self {
        Self {
            id: value.id,
            name: value.name,
            ty: value.ty,
            default: value.default,
            min: value.min,
            max: value.max,
            options: value.options,
            suffix: value.suffix,
            step: value.step,
            precision: value.precision,
            visible_when: value.visible_when.map(|condition| InputVisibility {
                input: condition.input,
                equals: condition.equals,
            }),
            monitor_handle: value.monitor_handle,
            pen_tool: value.pen_tool,
            pen_closed: value.pen_closed,
            monitor_colors: value.monitor_colors,
            monitor_midpoints: value.monitor_midpoints,
            monitor_resize_transform: value.monitor_resize_transform,
        }
    }
}

fn default_effect_entry() -> String {
    "effect".into()
}

fn sorted_definitions<'a, T: NamedDefinition + 'a>(
    values: impl Iterator<Item = &'a T>,
) -> impl Iterator<Item = &'a T> {
    let mut values = values.collect::<Vec<_>>();
    values.sort_by(|a, b| a.name().cmp(b.name()).then_with(|| a.key().cmp(b.key())));
    values.into_iter()
}

trait NamedDefinition {
    fn key(&self) -> &str;
    fn name(&self) -> &str;
}

macro_rules! named_definition {
    ($($ty:ty),+ $(,)?) => {
        $(impl NamedDefinition for $ty {
            fn key(&self) -> &str { &self.key }
            fn name(&self) -> &str { &self.name }
        })+
    };
}

named_definition!(EffectDefinition, AudioEffectDefinition, GeneratorDefinition);

pub(crate) fn shader_ident(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_ascii_alphanumeric() || character == '_' {
            output.push(character);
        } else {
            output.push('_');
        }
    }
    if output.is_empty() || output.as_bytes()[0].is_ascii_digit() {
        output.insert(0, '_');
    }
    output
}
