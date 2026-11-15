fn effect(color: vec4<f32>, uv: vec2<f32>, red: f32, green: f32, blue: f32, alpha: f32) -> vec4<f32> {
    let channels = clamp(vec4<f32>(red, green, blue, alpha) / 100.0, vec4<f32>(0.0), vec4<f32>(1.0));
    return vec4<f32>(color.rgb * channels.rgb, color.a) * channels.a;
}
