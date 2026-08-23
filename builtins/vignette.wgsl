fn effect(color: vec4<f32>, uv: vec2<f32>, output_size: vec2<u32>, amount: f32, midpoint: f32, softness: f32, roundness: f32) -> vec4<f32> {
    let size = max(vec2<f32>(output_size), vec2<f32>(1.0));
    let aspect = size.x / size.y;
    let centered = uv - vec2<f32>(0.5);
    let x_scale = mix(1.0, aspect, clamp(roundness, 0.0, 1.0));
    let distance = length(centered * vec2<f32>(x_scale, 1.0)) * 2.0;
    let pixel_width = 2.0 / min(size.x, size.y);
    let transition = max(softness, pixel_width);
    let edge = smoothstep(midpoint, midpoint + transition, distance);
    return vec4<f32>(color.rgb * (1.0 - edge * amount), color.a);
}
