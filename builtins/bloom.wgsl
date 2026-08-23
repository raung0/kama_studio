fn bloom_bilinear(source: texture_2d<f32>, position: vec2<f32>, size: vec2<u32>) -> vec4<f32> {
    let max_p = vec2<f32>(vec2<u32>(max(size, vec2<u32>(1u))) - vec2<u32>(1u));
    let p = clamp(position, vec2<f32>(0.0), max_p);
    let base = vec2<i32>(floor(p));
    let next = min(base + vec2<i32>(1), vec2<i32>(size) - vec2<i32>(1));
    let f = fract(p);
    let a = mix(textureLoad(source, base, 0), textureLoad(source, vec2<i32>(next.x, base.y), 0), f.x);
    let b = mix(textureLoad(source, vec2<i32>(base.x, next.y), 0), textureLoad(source, next, 0), f.x);
    return mix(a, b, f.y);
}

fn bloom_extract(c: vec4<f32>, threshold: f32) -> vec4<f32> {
    let luma = dot(c.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    let knee = max(0.08, threshold * 0.2 + 0.04);
    return c * smoothstep(threshold - knee, threshold + knee, luma);
}

fn bloom_tap(source: texture_2d<f32>, p: vec2<f32>, offset: vec2<f32>, output_size: vec2<u32>, threshold: f32, weight: f32) -> vec4<f32> {
    return bloom_extract(bloom_bilinear(source, p + offset, output_size), threshold) * weight;
}

fn effect(source: texture_2d<f32>, pixel: vec2<u32>, uv: vec2<f32>, output_size: vec2<u32>, threshold: f32, intensity: f32, radius: f32) -> vec4<f32> {
    let p = vec2<f32>(pixel);
    let s = max(radius, 0.5);
    var glow = bloom_extract(bloom_bilinear(source, p, output_size), threshold) * 0.18;

    glow += bloom_tap(source, p, vec2<f32>(1.0, 0.0) * s * 1.1, output_size, threshold, 0.085);
    glow += bloom_tap(source, p, vec2<f32>(-1.0, 0.0) * s * 1.1, output_size, threshold, 0.085);
    glow += bloom_tap(source, p, vec2<f32>(0.0, 1.0) * s * 1.1, output_size, threshold, 0.085);
    glow += bloom_tap(source, p, vec2<f32>(0.0, -1.0) * s * 1.1, output_size, threshold, 0.085);

    glow += bloom_tap(source, p, vec2<f32>(0.7071, 0.7071) * s * 1.8, output_size, threshold, 0.055);
    glow += bloom_tap(source, p, vec2<f32>(-0.7071, 0.7071) * s * 1.8, output_size, threshold, 0.055);
    glow += bloom_tap(source, p, vec2<f32>(0.7071, -0.7071) * s * 1.8, output_size, threshold, 0.055);
    glow += bloom_tap(source, p, vec2<f32>(-0.7071, -0.7071) * s * 1.8, output_size, threshold, 0.055);

    glow += bloom_tap(source, p, vec2<f32>(1.0, 0.0) * s * 2.8, output_size, threshold, 0.030);
    glow += bloom_tap(source, p, vec2<f32>(-1.0, 0.0) * s * 2.8, output_size, threshold, 0.030);
    glow += bloom_tap(source, p, vec2<f32>(0.0, 1.0) * s * 2.8, output_size, threshold, 0.030);
    glow += bloom_tap(source, p, vec2<f32>(0.0, -1.0) * s * 2.8, output_size, threshold, 0.030);

    let source_color = textureLoad(source, vec2<i32>(pixel), 0);
    let weight = 0.18 + 4.0 * (0.085 + 0.055 + 0.030);
    return source_color + glow * (max(intensity, 0.0) / max(weight, 0.0001));
}
