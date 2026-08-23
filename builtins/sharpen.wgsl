fn sample_clamped(source: texture_2d<f32>, p: vec2<i32>, size: vec2<u32>) -> vec4<f32> {
    return textureLoad(source, clamp(p, vec2<i32>(0), vec2<i32>(size) - vec2<i32>(1)), 0);
}
fn effect(source: texture_2d<f32>, pixel: vec2<u32>, uv: vec2<f32>, output_size: vec2<u32>, amount: f32) -> vec4<f32> {
    let p = vec2<i32>(pixel);
    let center = sample_clamped(source, p, output_size);
    let blur = (sample_clamped(source, p + vec2<i32>(1, 0), output_size) + sample_clamped(source, p - vec2<i32>(1, 0), output_size) + sample_clamped(source, p + vec2<i32>(0, 1), output_size) + sample_clamped(source, p - vec2<i32>(0, 1), output_size)) * 0.25;
    return center + (center - blur) * amount;
}
