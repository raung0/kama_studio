fn effect(color: vec4<f32>, uv: vec2<f32>, exposure: f32, contrast: f32, saturation: f32, temperature: f32, tint: f32) -> vec4<f32> {
    if (color.a <= 0.000001) { return color; }
    var rgb = color.rgb / color.a;
    rgb = rgb * exp2(exposure);
    rgb = (rgb - vec3<f32>(0.18)) * contrast + vec3<f32>(0.18);
    let luma = dot(rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    rgb = mix(vec3<f32>(luma), rgb, saturation);
    rgb = rgb * vec3<f32>(1.0 + temperature * 0.12, 1.0 + tint * 0.06, 1.0 - temperature * 0.12);
    rgb.g = rgb.g * (1.0 - tint * 0.06);
    return vec4<f32>(max(rgb, vec3<f32>(0.0)) * color.a, color.a);
}
