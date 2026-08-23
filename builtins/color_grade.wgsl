fn grade_preset(rgb: vec3<f32>, preset: u32) -> vec3<f32> {
    switch preset {
        case 1u: { return vec3<f32>(rgb.r * 1.08 + rgb.b * 0.03, rgb.g * 1.01, rgb.b * 0.92); }
        case 2u: { return vec3<f32>(rgb.r * 0.94, rgb.g * 1.01, rgb.b * 1.09 + rgb.r * 0.02); }
        case 3u: { return pow(max(rgb, vec3<f32>(0.0)), vec3<f32>(0.88)) * vec3<f32>(1.04, 1.00, 0.96); }
        case 4u: {
            let l = dot(rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
            return mix(vec3<f32>(l), rgb * vec3<f32>(1.04, 0.98, 0.90), 0.55);
        }
        default: { return rgb; }
    }
}
fn effect(
    color: vec4<f32>,
    uv: vec2<f32>,
    preset: u32,
    intensity: f32,
    lift: vec3<f32>,
    gamma: vec3<f32>,
    gain: vec3<f32>,
) -> vec4<f32> {
    if (color.a <= 0.000001) { return color; }
    let rgb = color.rgb / color.a;
    let corrected = pow(
        max(rgb + lift, vec3<f32>(0.0)),
        vec3<f32>(1.0) / max(gamma, vec3<f32>(0.01)),
    ) * gain;
    let graded = grade_preset(corrected, preset);
    return vec4<f32>(mix(rgb, graded, clamp(intensity, 0.0, 1.0)) * color.a, color.a);
}
