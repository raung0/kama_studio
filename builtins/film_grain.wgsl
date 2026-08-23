fn hash21(p: vec2<f32>) -> f32 { return fract(sin(dot(p, vec2<f32>(127.1, 311.7))) * 43758.5453); }
fn effect(color: vec4<f32>, uv: vec2<f32>, output_size: vec2<u32>, frame: u32, amount: f32, size: f32) -> vec4<f32> {
    if (color.a <= 0.000001) { return color; }
    let cells = floor(uv * vec2<f32>(output_size) / max(size, 1.0));
    let noise = hash21(cells + vec2<f32>(f32(frame) * 0.7549, f32(frame) * 0.5698)) * 2.0 - 1.0;
    let rgb = max(color.rgb / color.a + vec3<f32>(noise * amount), vec3<f32>(0.0));
    return vec4<f32>(rgb * color.a, color.a);
}
