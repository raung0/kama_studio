fn aberration_sample(source: texture_2d<f32>, position: vec2<f32>, size: vec2<u32>) -> vec4<f32> {
    let max_p = vec2<f32>(size) - vec2<f32>(1.0);
    let p = clamp(position, vec2<f32>(0.0), max_p);
    let p0 = vec2<i32>(floor(p));
    let p1 = min(p0 + vec2<i32>(1), vec2<i32>(size) - vec2<i32>(1));
    let f = fract(p);
    let a = mix(textureLoad(source, p0, 0), textureLoad(source, vec2<i32>(p1.x, p0.y), 0), f.x);
    let b = mix(textureLoad(source, vec2<i32>(p0.x, p1.y), 0), textureLoad(source, p1, 0), f.x);
    return mix(a, b, f.y);
}

fn channel_offset(aberration: vec3<f32>, radial: vec2<f32>, falloff: f32) -> vec2<f32> {
    // x = radial displacement in pixels; y/z = explicit horizontal/vertical displacement.
    return (radial * aberration.x + aberration.yz) * falloff;
}

fn effect(
    source: texture_2d<f32>,
    pixel: vec2<u32>,
    uv: vec2<f32>,
    output_size: vec2<u32>,
    point_of_interest: vec2<f32>,
    aberration_red: vec3<f32>,
    aberration_green: vec3<f32>,
    aberration_blue: vec3<f32>,
    falloff_distance: f32,
    falloff_invert: bool,
    horizontal_angular_field: f32,
    vertical_angular_field: f32,
) -> vec4<f32> {
    let size = vec2<f32>(output_size);
    let delta = uv - point_of_interest;
    let aspect = size.x / max(size.y, 1.0);
    let radial_space = vec2<f32>(delta.x * aspect, delta.y);
    let radius = length(radial_space);
    var radial = vec2<f32>(0.0);
    if (radius > 0.000001) {
        radial = normalize(radial_space) * vec2<f32>(1.0 / max(aspect, 0.0001), 1.0);
    }

    let hf = clamp(abs(horizontal_angular_field), 1.0, 360.0);
    let vf = clamp(abs(vertical_angular_field), 1.0, 360.0);
    let angular = length(vec2<f32>(delta.x * 180.0 / hf, delta.y * 180.0 / vf));
    let distance = max(falloff_distance, 0.0001);
    var falloff = smoothstep(0.0, distance, angular);
    if (falloff_invert) {
        falloff = 1.0 - falloff;
    }

    let center = vec2<f32>(pixel);
    let r = aberration_sample(source, center + channel_offset(aberration_red, radial, falloff), output_size);
    let g = aberration_sample(source, center + channel_offset(aberration_green, radial, falloff), output_size);
    let b = aberration_sample(source, center + channel_offset(aberration_blue, radial, falloff), output_size);
    let base = textureLoad(source, vec2<i32>(pixel), 0);
    return vec4<f32>(r.r, g.g, b.b, base.a);
}
