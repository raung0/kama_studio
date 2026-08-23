fn effect(color: vec4<f32>, uv: vec2<f32>, output_size: vec2<u32>, radius: vec4<f32>) -> vec4<f32> {
    let size = vec2<f32>(output_size);
    let p = uv * size;
    var r = radius.x;
    if (p.x >= size.x * 0.5 && p.y < size.y * 0.5) { r = radius.y; }
    if (p.x >= size.x * 0.5 && p.y >= size.y * 0.5) { r = radius.z; }
    if (p.x < size.x * 0.5 && p.y >= size.y * 0.5) { r = radius.w; }
    r = clamp(r, 0.0, min(size.x, size.y) * 0.5);
    let center = vec2<f32>(select(r, size.x - r, p.x >= size.x * 0.5), select(r, size.y - r, p.y >= size.y * 0.5));
    let in_corner = (p.x < r || p.x > size.x - r) && (p.y < r || p.y > size.y - r);
    let distance = length(p - center);
    let mask = select(1.0, 1.0 - smoothstep(r - 1.0, r + 1.0, distance), in_corner && r > 0.0);
    return color * mask;
}
