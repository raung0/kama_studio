fn effect(color: vec4<f32>, uv: vec2<f32>, output_size: vec2<u32>, amount: f32, midpoint: f32, softness: f32, roundness: f32) -> vec4<f32> {
    let size = max(vec2<f32>(output_size), vec2<f32>(1.0));
    let aspect = size.x / size.y;
    let centered = uv - vec2<f32>(0.5);

    // roundness = 0 keeps the vignette in normalized frame space. At 1, X is corrected by
    // the frame aspect ratio so equal distances in the mask represent equal pixel distances.
    let x_scale = mix(1.0, aspect, clamp(roundness, 0.0, 1.0));
    let distance = length(centered * vec2<f32>(x_scale, 1.0)) * 2.0;

    // Never let the transition become narrower than roughly one output pixel. This keeps the
    // vignette stable in half/quarter-resolution monitor previews without changing its geometry.
    let pixel_width = 2.0 / min(size.x, size.y);
    let transition = max(softness, pixel_width);
    let edge = smoothstep(midpoint, midpoint + transition, distance);
    return vec4<f32>(color.rgb * (1.0 - edge * amount), color.a);
}
