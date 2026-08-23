fn generate(pixel: vec2<u32>, uv: vec2<f32>, output_size: vec2<u32>, background: vec4<f32>, line_color: vec4<f32>, spacing: vec2<f32>, line_width: f32) -> vec4<f32> {
    let p = vec2<f32>(pixel) + vec2<f32>(0.5);
    let safe_spacing = max(spacing, vec2<f32>(1.0));
    let cell = min(fract(p / safe_spacing), 1.0 - fract(p / safe_spacing)) * safe_spacing;
    let line = min(cell.x, cell.y) <= max(line_width, 0.0) * 0.5;
    let c = select(background, line_color, line);
    return vec4<f32>(c.rgb * c.a, c.a);
}
