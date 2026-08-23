fn effect(color: vec4<f32>, uv: vec2<f32>, amount: f32) -> vec4<f32> {
    if (color.a <= 0.000001) { return color; }
    let rgb = color.rgb / color.a;
    let luma = dot(rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    return vec4<f32>(mix(rgb, vec3<f32>(luma), amount) * color.a, color.a);
}
