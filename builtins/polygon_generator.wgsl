fn generate(pixel: vec2<u32>, uv: vec2<f32>, output_size: vec2<u32>, sides: u32, radius: f32, rotation: f32, color: vec4<f32>, border_color: vec4<f32>, border_width: f32, border_alignment: u32) -> vec4<f32> {
    let n = f32(clamp(sides, 3u, 64u));
    let size = vec2<f32>(output_size);
    let aspect = size.x / max(size.y, 1.0);
    var p = (uv - vec2<f32>(0.5)) * vec2<f32>(aspect, 1.0) * 2.0;
    let r = length(p);
    var a = atan2(p.y, p.x) - rotation * 0.017453292519943295;
    let sector = 6.28318530718 / n;
    a = abs(fract(a / sector + 0.5) * sector - sector * 0.5);
    let edge = max(radius, 0.001) * cos(3.14159265359 / n) / max(cos(a), 0.0001);
    let aa = max(2.0 / max(min(size.x, size.y), 1.0), 0.0015);
    let px_to_radius = 2.0 / max(min(size.x, size.y), 1.0);
    let stroke = max(border_width, 0.0) * px_to_radius;
    let alignment = min(border_alignment, 2u);
    let inner_extent = select(select(stroke, stroke * 0.5, alignment == 1u), 0.0, alignment == 2u);
    let outer_extent = select(select(0.0, stroke * 0.5, alignment == 1u), stroke, alignment == 2u);
    let outer_edge = edge + outer_extent;
    let inner_edge = max(edge - inner_extent, 0.0);
    let outer = 1.0 - smoothstep(outer_edge - aa, outer_edge + aa, r);
    let inner = select(outer, 1.0 - smoothstep(inner_edge - aa, inner_edge + aa, r), border_width > 0.0);
    let border = clamp(outer - inner, 0.0, 1.0);
    let fill_alpha = color.a * inner;
    let stroke_alpha = border_color.a * border;
    return vec4<f32>(
        color.rgb * fill_alpha + border_color.rgb * stroke_alpha,
        clamp(fill_alpha + stroke_alpha, 0.0, 1.0),
    );
}
