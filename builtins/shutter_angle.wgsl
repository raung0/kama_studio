fn shutter_sample(source: texture_2d<f32>, position: vec2<f32>, size: vec2<u32>) -> vec4<f32> {
    let max_p = vec2<f32>(size) - vec2<f32>(1.0);
    let p = clamp(position, vec2<f32>(0.0), max_p);
    let p0 = vec2<i32>(floor(p));
    let p1 = min(p0 + vec2<i32>(1), vec2<i32>(size) - vec2<i32>(1));
    let f = fract(p);
    let a = mix(textureLoad(source, p0, 0), textureLoad(source, vec2<i32>(p1.x, p0.y), 0), f.x);
    let b = mix(textureLoad(source, vec2<i32>(p0.x, p1.y), 0), textureLoad(source, p1, 0), f.x);
    return mix(a, b, f.y);
}

fn effect(source: texture_2d<f32>, pixel: vec2<u32>, uv: vec2<f32>, output_size: vec2<u32>, shutter_angle: f32, direction: f32, distance: f32) -> vec4<f32> {
    let amount = max(distance, 0.0) * clamp(shutter_angle, 0.0, 720.0) / 360.0;
    if (amount <= 0.001) { return textureLoad(source, vec2<i32>(pixel), 0); }
    let radians = direction * 0.017453292519943295;
    let axis = vec2<f32>(cos(radians), sin(radians)) * amount;
    let sample_count = i32(clamp(ceil(amount * 2.0), 16.0, 512.0));
    var sum = vec4<f32>(0.0);
    var weight = 0.0;
    for (var i = 0; i <= sample_count; i = i + 1) {
        let t = f32(i) / f32(sample_count) * 2.0 - 1.0;
        let w = 1.0 - abs(t) * 0.32;
        sum += shutter_sample(source, vec2<f32>(pixel) + axis * t, output_size) * w;
        weight += w;
    }
    return sum / max(weight, 0.0001);
}
