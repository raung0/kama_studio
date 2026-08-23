fn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> {
    return vec4<f32>(vec3<f32>(color.a) - color.rgb, color.a);
}
