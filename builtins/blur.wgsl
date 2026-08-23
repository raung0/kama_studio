fn blur_bilinear(source: texture_2d<f32>, position: vec2<f32>, size: vec2<u32>) -> vec4<f32> {
    let max_p = vec2<f32>(vec2<u32>(max(size, vec2<u32>(1u))) - vec2<u32>(1u));
    let p = clamp(position, vec2<f32>(0.0), max_p);
    let base = vec2<i32>(floor(p));
    let next = min(base + vec2<i32>(1), vec2<i32>(size) - vec2<i32>(1));
    let f = fract(p);
    let a = mix(textureLoad(source, base, 0), textureLoad(source, vec2<i32>(next.x, base.y), 0), f.x);
    let b = mix(textureLoad(source, vec2<i32>(base.x, next.y), 0), textureLoad(source, next, 0), f.x);
    return mix(a, b, f.y);
}

fn effect(source: texture_2d<f32>, pixel: vec2<u32>, uv: vec2<f32>, output_size: vec2<u32>, radius: f32) -> vec4<f32> {
    let r = clamp(radius, 0.0, 64.0);
    if (r <= 0.001) { return textureLoad(source, vec2<i32>(pixel), 0); }
    let p = vec2<f32>(pixel);
    let s = max(r * 0.52, 0.35);
    var sum = blur_bilinear(source, p, output_size) * 0.20;
    var weight = 0.20;
    let axis_w = 0.085;
    let diag_w = 0.060;
    let outer_w = 0.035;

    sum += blur_bilinear(source, p + vec2<f32>(1.0, 0.0) * s, output_size) * axis_w;
    sum += blur_bilinear(source, p + vec2<f32>(-1.0, 0.0) * s, output_size) * axis_w;
    sum += blur_bilinear(source, p + vec2<f32>(0.0, 1.0) * s, output_size) * axis_w;
    sum += blur_bilinear(source, p + vec2<f32>(0.0, -1.0) * s, output_size) * axis_w;

    sum += blur_bilinear(source, p + vec2<f32>(0.7071, 0.7071) * s * 1.45, output_size) * diag_w;
    sum += blur_bilinear(source, p + vec2<f32>(-0.7071, 0.7071) * s * 1.45, output_size) * diag_w;
    sum += blur_bilinear(source, p + vec2<f32>(0.7071, -0.7071) * s * 1.45, output_size) * diag_w;
    sum += blur_bilinear(source, p + vec2<f32>(-0.7071, -0.7071) * s * 1.45, output_size) * diag_w;

    sum += blur_bilinear(source, p + vec2<f32>(1.0, 0.0) * s * 2.15, output_size) * outer_w;
    sum += blur_bilinear(source, p + vec2<f32>(-1.0, 0.0) * s * 2.15, output_size) * outer_w;
    sum += blur_bilinear(source, p + vec2<f32>(0.0, 1.0) * s * 2.15, output_size) * outer_w;
    sum += blur_bilinear(source, p + vec2<f32>(0.0, -1.0) * s * 2.15, output_size) * outer_w;

    weight += 4.0 * (axis_w + diag_w + outer_w);
    return sum / max(weight, 0.0001);
}
