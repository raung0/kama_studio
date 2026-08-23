fn generate(pixel: vec2<u32>, uv: vec2<f32>, output_size: vec2<u32>, color_a: vec4<f32>, color_b: vec4<f32>, cell_size: f32) -> vec4<f32> {
    let cell = max(cell_size, 1.0);
    let tile = vec2<u32>(floor(vec2<f32>(pixel) / cell));
    let c = select(color_a, color_b, ((tile.x + tile.y) & 1u) == 1u);
    return vec4<f32>(c.rgb * c.a, c.a);
}
