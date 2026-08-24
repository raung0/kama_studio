use anyhow::{bail, Context, Result};
use naga::{
    back::wgsl::{self, WriterFlags},
    front::wgsl::parse_str,
    valid::{Capabilities, ValidationFlags, Validator},
};

use crate::plugin::{
    EffectDefinition, EffectKind, GeneratorDefinition, PluginRegistry, RuntimeProperty,
};

pub fn build_fused_pointwise_shader(
    node_types: &[String],
    plugins: &PluginRegistry,
) -> Result<String> {
    let mut modules = String::new();
    let mut calls = String::new();
    let mut parameter_slot = 0usize;

    for (index, node_type) in node_types.iter().enumerate() {
        let Some(effect) = plugins.effect(node_type) else {
            continue;
        };
        if effect.kind != EffectKind::Pointwise {
            bail!("cannot fuse non-pointwise effect {node_type}");
        }
        let namespace = effect.namespace_for_node(index as u64);
        let namespaced = namespace_effect(effect, &namespace)
            .with_context(|| format!("namespace pointwise node {node_type}"))?;
        modules.push('\n');
        modules.push_str("// ");
        modules.push_str(node_type);
        modules.push('\n');
        modules.push_str(&namespaced);
        modules.push('\n');

        let enabled_slot = parameter_slot;
        parameter_slot += 1;
        let mut args = vec!["color".to_owned(), "uv".to_owned()];
        append_runtime_arguments(effect, &mut args);
        for input in &effect.inputs {
            let read = input.ty.wgsl_read(parameter_slot).with_context(|| {
                format!("GPU effect {node_type} has host-only input {}", input.id)
            })?;
            parameter_slot += 1;
            args.push(read);
        }
        calls.push_str(&format!(
            "    if effect_params.data[{enabled_slot}u].x >= 0.5 {{\n        color = {namespace}_{}({});\n    }}\n",
            effect.entry,
            args.join(", ")
        ));
    }

    let body = format!(
        "    var color = textureLoad(source_tex, vec2<i32>(gid.xy), 0);\n{calls}    textureStore(output_tex, vec2<i32>(gid.xy), color);"
    );
    let wrapper = build_effect_wrapper(parameter_slot.max(1), 1, &body);
    validate_assembled(
        &format!("{modules}\n{wrapper}"),
        "assembled fused Effect Pipeline WGSL",
    )
}

fn build_effect_wrapper(parameter_count: usize, image_input_count: usize, body: &str) -> String {
    let (images, output_binding, params_binding, runtime_binding, source_dimensions) =
        match image_input_count {
            0 => (String::new(), 0, 1, 2, "effect_runtime.output_source_size.xy"),
            1 => (
                "@group(0) @binding(0)\nvar source_tex: texture_2d<f32>;\n".to_string(),
                1,
                2,
                3,
                "textureDimensions(source_tex)",
            ),
            2 => (
                "@group(0) @binding(0)\nvar effect_input_0: texture_2d<f32>;\n\n@group(0) @binding(1)\nvar effect_input_1: texture_2d<f32>;\n".to_string(),
                2,
                3,
                4,
                "textureDimensions(effect_input_0)",
            ),
            _ => unreachable!("validated plugin image input count"),
        };
    format!(
        r#"
struct EffectParams {{
    data: array<vec4<f32>, {parameter_count}>,
}}

struct EffectRuntime {{
    output_source_size: vec4<u32>,
    times: vec4<f32>,
    frame: vec4<u32>,
}}

{images}
@group(0) @binding({output_binding})
var output_tex: texture_storage_2d<rgba16float, write>;

@group(0) @binding({params_binding})
var<uniform> effect_params: EffectParams;

@group(0) @binding({runtime_binding})
var<uniform> effect_runtime: EffectRuntime;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    let source_dimensions = {source_dimensions};
    let output_dimensions = textureDimensions(output_tex);
    if (gid.x >= output_dimensions.x || gid.y >= output_dimensions.y) {{
        return;
    }}
    let uv = (vec2<f32>(gid.xy) + vec2<f32>(0.5)) / vec2<f32>(output_dimensions);
{body}
}}
"#
    )
}

pub fn build_generator_shader(generator: &GeneratorDefinition) -> Result<String> {
    let source = generator
        .source
        .as_deref()
        .context("GPU generator has no shader source")?;
    let namespace = format!(
        "plugin_{}_{}_generator",
        crate::plugin::shader_ident(&generator.plugin_id),
        crate::plugin::shader_ident(&generator.id),
    );
    let namespaced = namespace_module(source, &generator.key, &namespace)
        .with_context(|| format!("namespace GPU generator {}", generator.key))?;

    let mut args = vec![
        "gid.xy".to_owned(),
        "uv".to_owned(),
        "dimensions".to_owned(),
    ];
    for (slot, input) in generator.inputs.iter().enumerate() {
        let read = input.ty.wgsl_read(slot).with_context(|| {
            format!(
                "GPU generator {} has host-only input {}",
                generator.key, input.id
            )
        })?;
        args.push(read);
    }
    let parameter_count = generator.inputs.len().max(1);
    let entry = generator.entry.as_deref().unwrap_or("generate");
    let wrapper = format!(
        r#"
struct EffectParams {{
    data: array<vec4<f32>, {parameter_count}>,
}}

@group(0) @binding(0)
var output_tex: texture_storage_2d<rgba16float, write>;

@group(0) @binding(1)
var<uniform> effect_params: EffectParams;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    let dimensions = textureDimensions(output_tex);
    if (gid.x >= dimensions.x || gid.y >= dimensions.y) {{
        return;
    }}
    let uv = (vec2<f32>(gid.xy) + vec2<f32>(0.5)) / vec2<f32>(dimensions);
    let color = {namespace}_{entry}({args});
    textureStore(output_tex, vec2<i32>(gid.xy), color);
}}
"#,
        args = args.join(", "),
    );
    validate_assembled(
        &format!("{namespaced}\n{wrapper}"),
        "assembled GPU generator WGSL",
    )
}

pub fn build_standalone_shader(node_type: &str, plugins: &PluginRegistry) -> Result<String> {
    let effect = plugins
        .effect(node_type)
        .with_context(|| format!("unknown standalone effect {node_type}"))?;
    if effect.kind != EffectKind::Standalone {
        bail!("cannot build standalone wrapper for pointwise effect {node_type}");
    }
    let namespace = effect.namespace_for_node(0);
    let namespaced = namespace_effect(effect, &namespace)
        .with_context(|| format!("namespace standalone node {node_type}"))?;

    let image_count = effect.image_inputs.len();
    let mut args = match image_count {
        0 => vec!["gid.xy".to_owned(), "uv".to_owned()],
        1 => vec![
            "source_tex".to_owned(),
            "gid.xy".to_owned(),
            "uv".to_owned(),
        ],
        2 => vec![
            "effect_input_0".to_owned(),
            "effect_input_1".to_owned(),
            "gid.xy".to_owned(),
            "uv".to_owned(),
        ],
        _ => unreachable!("validated plugin image input count"),
    };
    append_runtime_arguments(effect, &mut args);
    for (slot, input) in effect.inputs.iter().enumerate() {
        let read = input
            .ty
            .wgsl_read(slot + 1)
            .with_context(|| format!("GPU effect {node_type} has host-only input {}", input.id))?;
        args.push(read);
    }
    let initial_color = match image_count {
        0 => "vec4<f32>(0.0)",
        1 => "textureLoad(source_tex, vec2<i32>(gid.xy), 0)",
        2 => "textureLoad(effect_input_0, vec2<i32>(gid.xy), 0)",
        _ => unreachable!("validated plugin image input count"),
    };
    let body = format!(
        "    var color = {initial_color};\n    if effect_params.data[0u].x >= 0.5 {{\n        color = {namespace}_{}({});\n    }}\n    textureStore(output_tex, vec2<i32>(gid.xy), color);",
        effect.entry,
        args.join(", ")
    );
    let wrapper = build_effect_wrapper(effect.inputs.len() + 1, image_count, &body);
    validate_assembled(
        &format!("{namespaced}\n{wrapper}"),
        "assembled standalone Effect Pipeline WGSL",
    )
}

pub fn namespace_effect(effect: &EffectDefinition, namespace: &str) -> Result<String> {
    namespace_module(&effect.source, &effect.key, namespace)
}

fn namespace_module(source: &str, display_name: &str, namespace: &str) -> Result<String> {
    let mut module = parse_str(source)
        .with_context(|| format!("failed to parse plugin module {display_name}"))?;
    if !module.entry_points.is_empty() {
        bail!(
            "plugin module {display_name} contains an entry point; plugins expose ordinary functions and Kama owns the compute wrapper"
        );
    }
    for (_, ty) in module.types.iter() {
        if ty.name.is_some() {
            bail!("plugin module {display_name} declares a named WGSL type; plugin types must remain anonymous");
        }
    }

    let prefix = format!("{namespace}_");
    for (_, constant) in module.constants.iter_mut() {
        if let Some(name) = &mut constant.name {
            *name = format!("{prefix}{name}");
        }
    }
    for (_, override_) in module.overrides.iter_mut() {
        if let Some(name) = &mut override_.name {
            *name = format!("{prefix}{name}");
        }
    }
    for (_, global) in module.global_variables.iter_mut() {
        if global.binding.is_some() {
            bail!("plugin module {display_name} declares @group/@binding; GPU resources are host-owned");
        }
        if let Some(name) = &mut global.name {
            *name = format!("{prefix}{name}");
        }
    }
    for (_, function) in module.functions.iter_mut() {
        if let Some(name) = &mut function.name {
            *name = format!("{prefix}{name}");
        }
    }
    emit_valid_wgsl(&module)
}

fn append_runtime_arguments(effect: &EffectDefinition, args: &mut Vec<String>) {
    for property in &effect.uses {
        args.push(match property {
            RuntimeProperty::OutputSize => "effect_runtime.output_source_size.xy".into(),
            RuntimeProperty::SourceSize => "effect_runtime.output_source_size.zw".into(),
            RuntimeProperty::TimelineTime => "effect_runtime.times.x".into(),
            RuntimeProperty::LocalTime => "effect_runtime.times.y".into(),
            RuntimeProperty::Frame => "effect_runtime.frame.x".into(),
        });
    }
}

fn validate_assembled(source: &str, context: &str) -> Result<String> {
    let module = parse_str(source).with_context(|| format!("{context} failed to parse"))?;
    emit_valid_wgsl(&module)
}

fn emit_valid_wgsl(module: &naga::Module) -> Result<String> {
    let info = Validator::new(ValidationFlags::all(), Capabilities::all())
        .validate(module)
        .context("Naga validation failed")?;
    wgsl::write_string(module, &info, WriterFlags::empty()).context("Naga WGSL emission failed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_builtin_gpu_effects_and_generators_validate_through_naga() {
        let registry = PluginRegistry::load_default("").unwrap();
        for effect in [
            "builtin.vignette",
            "builtin.color_grade",
            "builtin.film_grain",
            "builtin.color_correction",
            "builtin.chroma_key",
            "builtin.invert",
            "builtin.rounded_corners",
            "builtin.crop",
            "builtin.black_white",
        ] {
            build_fused_pointwise_shader(&[effect.to_owned()], &registry).unwrap();
        }
        for effect in [
            "builtin.blur",
            "builtin.padding",
            "builtin.bloom",
            "builtin.lens_distortion",
            "builtin.shutter_angle",
            "builtin.wave_warp",
            "builtin.replicate",
            "builtin.mask",
            "builtin.compose",
        ] {
            build_standalone_shader(effect, &registry).unwrap();
        }
        let mask = registry.effect("builtin.mask").unwrap();
        assert_eq!(
            mask.image_inputs
                .iter()
                .map(|input| (input.id.as_str(), input.required))
                .collect::<Vec<_>>(),
            vec![("frame", true), ("mask", true)],
        );
        let channel = mask
            .inputs
            .iter()
            .find(|input| input.id == "channel")
            .unwrap();
        assert_eq!(channel.ty, crate::plugin::InputType::Enum);
        assert_eq!(
            channel
                .options
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["Luminance", "Alpha", "Luminance × Alpha"],
        );
        for generator in [
            "builtin.grid",
            "builtin.checkerboard",
            "builtin.regular_polygon",
        ] {
            build_generator_shader(registry.generator(generator).unwrap()).unwrap();
        }
        let compose = registry.effect("builtin.compose").unwrap();
        let dynamic = compose.dynamic_image_inputs.as_ref().unwrap();
        assert_eq!(dynamic.count_input, "count");
        assert_eq!(dynamic.prefix, "image_");
        assert_eq!((dynamic.min, dynamic.max), (1, 64));

        let polygon = registry.generator("builtin.polygon").unwrap();
        assert_eq!(polygon.backend, crate::plugin::GeneratorBackend::Wasm);
        let bounds = polygon.bounds.as_ref().unwrap();
        assert_eq!(bounds.points_input, "points");
        assert_eq!(bounds.padding_input.as_deref(), Some("feather"));
        let points = polygon
            .inputs
            .iter()
            .find(|input| input.id == "points")
            .unwrap();
        assert_eq!(points.ty, crate::plugin::InputType::Vec2Array);
        assert_eq!(
            points.monitor_handle,
            Some(crate::plugin::MonitorHandleMode::Points)
        );
        assert!(points.pen_tool);

        let gradient = registry.generator("builtin.gradient").unwrap();
        let gradient_points = gradient
            .inputs
            .iter()
            .find(|input| input.id == "points")
            .unwrap();
        assert_eq!(gradient_points.monitor_colors.as_deref(), Some("colors"));
        assert_eq!(
            gradient_points.monitor_midpoints.as_deref(),
            Some("midpoints")
        );
        assert_eq!(gradient.monitor_entry.as_deref(), Some("monitor_gradient"));

        let mesh = registry.effect("builtin.mesh_warp").unwrap();
        let monitor = mesh.monitor.as_ref().unwrap();
        assert_eq!(monitor.entry, "monitor_mesh_warp");
        assert!(monitor.module.ends_with("generators.wasm"));

        let shape = registry.generator("builtin.shape").unwrap();
        let shape_type = shape
            .inputs
            .iter()
            .find(|input| input.id == "shape_type")
            .unwrap();
        assert_eq!(shape_type.options, ["Rect", "Ellipse"]);
        let size = shape
            .inputs
            .iter()
            .find(|input| input.id == "size")
            .unwrap();
        assert_eq!(
            size.monitor_handle,
            Some(crate::plugin::MonitorHandleMode::Size)
        );
        assert!(size.monitor_resize_transform);
        assert_eq!(size.visible_when.as_ref().unwrap().input, "shape_type");
        assert_eq!(size.visible_when.as_ref().unwrap().equals, 0);
        let radius = shape
            .inputs
            .iter()
            .find(|input| input.id == "radius")
            .unwrap();
        assert_eq!(
            radius.monitor_handle,
            Some(crate::plugin::MonitorHandleMode::Radius),
        );
        assert_eq!(radius.visible_when.as_ref().unwrap().input, "shape_type");
        assert_eq!(radius.visible_when.as_ref().unwrap().equals, 1);
    }

    #[test]
    fn namespace_has_no_textual_rewrite_dependency() {
        let effect = EffectDefinition {
            key: "test.tint".into(),
            plugin_id: "test".into(),
            id: "tint".into(),
            name: "Tint".into(),
            category: "Color".into(),
            kind: EffectKind::Pointwise,
            role: None,
            source: "fn helper(v: f32) -> f32 { return v; } fn effect(c: vec4<f32>, uv: vec2<f32>) -> vec4<f32> { return c * helper(1.0); }".into(),
            entry: "effect".into(),
            uses: Vec::new(),
            image_inputs: vec![crate::plugin::PluginImageInput {
                id: "image".into(),
                name: "Image".into(),
                required: true,
            }],
            dynamic_image_inputs: None,
            inputs: Vec::new(),
            monitor: None,
        };
        let source = namespace_effect(&effect, "plugin_test_tint_n7").unwrap();
        assert!(source.contains("plugin_test_tint_n7_effect"));
        assert!(source.contains("plugin_test_tint_n7_helper"));
    }
}
