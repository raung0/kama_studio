fn effect(source: texture_2d<f32>, pixel: vec2<u32>, uv: vec2<f32>, output_size: vec2<u32>, pixel_size: f32) -> vec4<f32> {
    let block = max(u32(round(pixel_size)), 1u);
    let origin = (pixel / vec2<u32>(block)) * vec2<u32>(block);
    let center = min(origin + vec2<u32>(block / 2u), output_size - vec2<u32>(1));
    return textureLoad(source, vec2<i32>(center), 0);
}
