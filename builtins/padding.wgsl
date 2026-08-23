fn padding_sides(edges: u32, thickness: vec2<i32>) -> vec4<i32> {
    let horizontal = max(thickness.x, 0);
    let vertical = max(thickness.y, 0);
    if (edges == 1u) { return vec4<i32>(horizontal, 0, horizontal, 0); }
    if (edges == 2u) { return vec4<i32>(0, vertical, 0, vertical); }
    if (edges == 3u) { return vec4<i32>(horizontal, 0, 0, 0); }
    if (edges == 4u) { return vec4<i32>(0, 0, horizontal, 0); }
    if (edges == 5u) { return vec4<i32>(0, vertical, 0, 0); }
    if (edges == 6u) { return vec4<i32>(0, 0, 0, vertical); }
    return vec4<i32>(horizontal, vertical, horizontal, vertical);
}

fn effect(
    source: texture_2d<f32>,
    pixel: vec2<u32>,
    uv: vec2<f32>,
    source_size: vec2<u32>,
    edges: u32,
    thickness: vec2<i32>,
    color: vec4<f32>,
) -> vec4<f32> {
    let sides = padding_sides(edges, thickness);
    let source_pixel = vec2<i32>(pixel) - sides.xy;
    if (source_pixel.x >= 0 && source_pixel.y >= 0
        && source_pixel.x < i32(source_size.x) && source_pixel.y < i32(source_size.y)) {
        return textureLoad(source, source_pixel, 0);
    }
    return vec4<f32>(color.rgb * color.a, color.a);
}
