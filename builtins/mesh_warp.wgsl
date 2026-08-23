fn mesh_warp_sample(source: texture_2d<f32>, uv: vec2<f32>) -> vec4<f32> {
    if (any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0))) { return vec4<f32>(0.0); }
    let size = textureDimensions(source);
    let max_pixel = vec2<i32>(size) - vec2<i32>(1);
    let position = clamp(uv * vec2<f32>(size) - vec2<f32>(0.5), vec2<f32>(0.0), vec2<f32>(max_pixel));
    let base = vec2<i32>(floor(position));
    let next = min(base + vec2<i32>(1), max_pixel);
    let f = fract(position);
    let a = mix(textureLoad(source, base, 0), textureLoad(source, vec2<i32>(next.x, base.y), 0), f.x);
    let b = mix(textureLoad(source, vec2<i32>(base.x, next.y), 0), textureLoad(source, next, 0), f.x);
    return mix(a, b, f.y);
}

fn mesh_warp_row(
    u: f32,
    left: vec2<f32>,
    center: vec2<f32>,
    right: vec2<f32>,
) -> vec2<f32> {
    if (u <= 0.5) {
        return mix(left, center, u * 2.0);
    }
    return mix(center, right, (u - 0.5) * 2.0);
}

fn effect(
    source: texture_2d<f32>,
    pixel: vec2<u32>,
    uv: vec2<f32>,
    output_size: vec2<u32>,
    top_left: vec2<f32>,
    top_center: vec2<f32>,
    top_right: vec2<f32>,
    center_left: vec2<f32>,
    center_right: vec2<f32>,
    bottom_left: vec2<f32>,
    bottom_center: vec2<f32>,
    bottom_right: vec2<f32>,
    amount: f32
) -> vec4<f32> {
    let top = mesh_warp_row(uv.x, top_left, top_center, top_right);
    let middle = mesh_warp_row(uv.x, center_left, vec2<f32>(0.0), center_right);
    let bottom = mesh_warp_row(uv.x, bottom_left, bottom_center, bottom_right);
    var offset_pixels: vec2<f32>;
    if (uv.y <= 0.5) {
        offset_pixels = mix(top, middle, uv.y * 2.0);
    } else {
        offset_pixels = mix(middle, bottom, (uv.y - 0.5) * 2.0);
    }
    offset_pixels *= amount;
    let source_uv = uv - offset_pixels / max(vec2<f32>(output_size), vec2<f32>(1.0));
    return mesh_warp_sample(source, source_uv);
}
