fn lens_sample(source: texture_2d<f32>, uv: vec2<f32>) -> vec4<f32> {
    let dimensions = textureDimensions(source);
    if (any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0))) { return vec4<f32>(0.0); }
    let position = uv * vec2<f32>(dimensions) - vec2<f32>(0.5);
    let base = vec2<i32>(floor(position));
    let f = fract(position);
    let hi = vec2<i32>(dimensions) - vec2<i32>(1);
    let a = textureLoad(source, clamp(base, vec2<i32>(0), hi), 0);
    let b = textureLoad(source, clamp(base + vec2<i32>(1, 0), vec2<i32>(0), hi), 0);
    let c = textureLoad(source, clamp(base + vec2<i32>(0, 1), vec2<i32>(0), hi), 0);
    let d = textureLoad(source, clamp(base + vec2<i32>(1, 1), vec2<i32>(0), hi), 0);
    return mix(mix(a, b, f.x), mix(c, d, f.x), f.y);
}

fn effect(source: texture_2d<f32>, pixel: vec2<u32>, uv: vec2<f32>, distortion: f32, center: vec2<f32>) -> vec4<f32> {
    let p = uv - center;
    let r2 = dot(p, p) * 4.0;
    let factor = 1.0 + distortion * r2 + 0.35 * distortion * distortion * r2 * r2;
    return lens_sample(source, center + p * factor);
}
