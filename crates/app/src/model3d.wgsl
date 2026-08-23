struct SceneUniform {
    center: vec4<f32>,
    extent: vec4<f32>,
    size: vec4<f32>,
    scale: vec4<f32>,
    rotation: vec4<f32>,
    position: vec4<f32>,
    viewport: vec4<f32>,
    shading: vec4<u32>,
}

struct MaterialUniform {
    base_color: vec4<f32>,
    // metallic, roughness, transmission, normal scale
    factors: vec4<f32>,
    // emissive rgb, emissive intensity
    emissive: vec4<f32>,
    // normal, packed metallic/roughness, metallic, roughness
    texture_flags: vec4<u32>,
    // occlusion, emissive, transmission, reserved
    texture_flags2: vec4<u32>,
    // occlusion strength, reserved...
    extra: vec4<f32>,
}

@group(0) @binding(0) var<uniform> scene: SceneUniform;
@group(1) @binding(0) var<uniform> material: MaterialUniform;
@group(1) @binding(1) var base_texture: texture_2d<f32>;
@group(1) @binding(2) var base_sampler: sampler;
@group(1) @binding(3) var opacity_texture: texture_2d<f32>;
@group(1) @binding(4) var normal_texture: texture_2d<f32>;
@group(1) @binding(5) var metallic_roughness_texture: texture_2d<f32>;
@group(1) @binding(6) var metallic_texture: texture_2d<f32>;
@group(1) @binding(7) var roughness_texture: texture_2d<f32>;
@group(1) @binding(8) var occlusion_texture: texture_2d<f32>;
@group(1) @binding(9) var emissive_texture: texture_2d<f32>;
@group(1) @binding(10) var transmission_texture: texture_2d<f32>;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) color: vec4<f32>,
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) color: vec4<f32>,
}

fn rotate_xyz(value: vec3<f32>, angles: vec3<f32>) -> vec3<f32> {
    let sx = sin(angles.x); let cx = cos(angles.x);
    let sy = sin(angles.y); let cy = cos(angles.y);
    let sz = sin(angles.z); let cz = cos(angles.z);
    var p = value;
    p = vec3<f32>(p.x, p.y * cx - p.z * sx, p.y * sx + p.z * cx);
    p = vec3<f32>(p.x * cy + p.z * sy, p.y, -p.x * sy + p.z * cy);
    return vec3<f32>(p.x * cz - p.y * sz, p.x * sz + p.y * cz, p.z);
}

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    let extent = max(scene.extent.xyz, vec3<f32>(0.000001));
    let largest_extent = max(extent.x, max(extent.y, extent.z));
    let object_scale = scene.size.xyz * scene.scale.xyz;
    let angles = scene.rotation.xyz * 0.017453292519943295;
    var local = (input.position - scene.center.xyz) / largest_extent * object_scale;
    let world = rotate_xyz(local, angles) + scene.position.xyz;

    let normal_scale = max(abs(object_scale), vec3<f32>(0.000001));
    let normal = normalize(rotate_xyz(input.normal / normal_scale, angles));

    let camera_z = 5.0;
    let camera_depth = max(camera_z - world.z, 0.05);
    let aspect = max(scene.viewport.x / max(scene.viewport.y, 1.0), 0.000001);
    let focal = 1.0 / tan(22.5 * 0.017453292519943295);
    let near = 0.05;
    let far = 100.0;
    let depth = clamp((camera_depth - near) / (far - near), 0.0, 1.0);

    var output: VertexOutput;
    output.clip_position = vec4<f32>(
        world.x * focal / aspect,
        world.y * focal,
        depth * camera_depth,
        camera_depth
    );
    output.world_position = world;
    output.normal = normal;
    output.uv = vec2<f32>(input.uv.x, 1.0 - input.uv.y);
    output.color = input.color;
    return output;
}

fn fresnel_schlick(cos_theta: f32, f0: vec3<f32>) -> vec3<f32> {
    return f0 + (vec3<f32>(1.0) - f0) * pow(clamp(1.0 - cos_theta, 0.0, 1.0), 5.0);
}

fn distribution_ggx(n: vec3<f32>, h: vec3<f32>, roughness: f32) -> f32 {
    let a = roughness * roughness;
    let a2 = a * a;
    let ndoth = max(dot(n, h), 0.0);
    let ndoth2 = ndoth * ndoth;
    let denominator = ndoth2 * (a2 - 1.0) + 1.0;
    return a2 / max(3.14159265359 * denominator * denominator, 0.000001);
}

fn geometry_schlick_ggx(ndotv: f32, roughness: f32) -> f32 {
    let r = roughness + 1.0;
    let k = (r * r) / 8.0;
    return ndotv / max(ndotv * (1.0 - k) + k, 0.000001);
}

fn geometry_smith(n: vec3<f32>, v: vec3<f32>, l: vec3<f32>, roughness: f32) -> f32 {
    return geometry_schlick_ggx(max(dot(n, v), 0.0), roughness)
        * geometry_schlick_ggx(max(dot(n, l), 0.0), roughness);
}

fn mapped_normal(input: VertexOutput) -> vec3<f32> {
    let n = normalize(input.normal);
    if (material.texture_flags.x == 0u) {
        return n;
    }

    let dp1 = dpdx(input.world_position);
    let dp2 = dpdy(input.world_position);
    let duv1 = dpdx(input.uv);
    let duv2 = dpdy(input.uv);
    let determinant = duv1.x * duv2.y - duv1.y * duv2.x;
    if (abs(determinant) <= 0.0000001) {
        return n;
    }
    let inverse = 1.0 / determinant;
    let tangent = normalize((dp1 * duv2.y - dp2 * duv1.y) * inverse);
    let bitangent = normalize((-dp1 * duv2.x + dp2 * duv1.x) * inverse);
    var sampled: vec3<f32>;
    if (material.extra.z > 0.5) {
        let height = textureSample(normal_texture, base_sampler, input.uv).r;
        let dhdx = dpdx(height);
        let dhdy = dpdy(height);
        let dhdu = (dhdx * duv2.y - dhdy * duv1.y) * inverse;
        let dhdv = (-dhdx * duv2.x + dhdy * duv1.x) * inverse;
        sampled = normalize(vec3<f32>(
            -dhdu * material.factors.w,
            -dhdv * material.factors.w,
            1.0,
        ));
    } else {
        sampled = textureSample(normal_texture, base_sampler, input.uv).xyz * 2.0 - vec3<f32>(1.0);
        sampled.x *= material.factors.w;
        sampled.y *= material.factors.w;
    }
    return normalize(tangent * sampled.x + bitangent * sampled.y + n * sampled.z);
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let texel = textureSample(base_texture, base_sampler, input.uv);
    let opacity_texel = textureSample(opacity_texture, base_sampler, input.uv);
    let base = material.base_color * input.color * texel;

    var mapped_opacity = opacity_texel.r;
    if (opacity_texel.a < 0.9999) {
        mapped_opacity = opacity_texel.a;
    }

    var coverage = clamp(base.a * mapped_opacity, 0.0, 1.0);
    let alpha_mode = material.texture_flags2.w;
    if (alpha_mode == 1u) {
        coverage = 1.0;
    } else if (alpha_mode == 2u) {
        if (coverage < material.extra.y) { discard; }
        coverage = 1.0;
    }

    var transmission = clamp(material.factors.z, 0.0, 1.0);
    if (material.texture_flags2.z != 0u) {
        transmission *= textureSample(transmission_texture, base_sampler, input.uv).r;
    }
    let alpha = clamp(coverage * (1.0 - transmission), 0.0, 1.0);
    if (alpha <= 0.000001) { discard; }

    var metallic = clamp(material.factors.x, 0.0, 1.0);
    var roughness = clamp(material.factors.y, 0.045, 1.0);
    if (material.texture_flags.y != 0u) {
        let packed = textureSample(metallic_roughness_texture, base_sampler, input.uv);
        roughness *= packed.g;
        metallic *= packed.b;
    }
    if (material.texture_flags.z != 0u) {
        metallic *= textureSample(metallic_texture, base_sampler, input.uv).r;
    }
    if (material.texture_flags.w != 0u) {
        roughness *= textureSample(roughness_texture, base_sampler, input.uv).r;
    }
    metallic = clamp(metallic, 0.0, 1.0);
    roughness = clamp(roughness, 0.045, 1.0);

    var ao = 1.0;
    if (material.texture_flags2.x != 0u) {
        let sampled_ao = textureSample(occlusion_texture, base_sampler, input.uv).r;
        ao = mix(1.0, sampled_ao, clamp(material.extra.x, 0.0, 1.0));
    }

    var emission = material.emissive.rgb;
    if (material.texture_flags2.y != 0u) {
        emission *= textureSample(emissive_texture, base_sampler, input.uv).rgb;
    }
    emission *= material.emissive.w;

    var linear_rgb = max(base.rgb, vec3<f32>(0.0));
    if (scene.shading.x == 1u) {
        let n = mapped_normal(input);
        let v = normalize(vec3<f32>(0.0, 0.0, 5.0) - input.world_position);
        let l = normalize(vec3<f32>(-0.45, 0.75, 0.55));
        let h = normalize(v + l);
        let radiance = vec3<f32>(3.0);
        let f0 = mix(vec3<f32>(0.04), linear_rgb, metallic);
        let f = fresnel_schlick(max(dot(h, v), 0.0), f0);
        let d = distribution_ggx(n, h, roughness);
        let g = geometry_smith(n, v, l, roughness);
        let denominator = max(4.0 * max(dot(n, v), 0.0) * max(dot(n, l), 0.0), 0.000001);
        let specular = d * g * f / denominator;
        let kd = (vec3<f32>(1.0) - f) * (1.0 - metallic);
        let ndotl = max(dot(n, l), 0.0);
        let direct = (kd * linear_rgb / 3.14159265359 + specular) * radiance * ndotl;
        let ambient = linear_rgb * (0.035 + 0.065 * max(n.y, 0.0)) * ao;
        linear_rgb = direct + ambient + emission;
    } else {
        linear_rgb += emission;
    }

    return vec4<f32>(linear_rgb * alpha, alpha);
}
