fn effect(color: vec4<f32>, uv: vec2<f32>, key_color: vec4<f32>, tolerance: f32, softness: f32, spill: f32) -> vec4<f32> {
    if (color.a <= 0.000001) { return color; }
    var rgb = color.rgb / color.a;
    let key = key_color.rgb;
    let distance = length(rgb - key);
    let keep = smoothstep(tolerance, tolerance + max(softness, 0.0001), distance);
    let green_excess = max(rgb.g - max(rgb.r, rgb.b), 0.0);
    rgb.g = max(0.0, rgb.g - green_excess * spill * (1.0 - keep));
    let alpha = color.a * keep;
    return vec4<f32>(rgb * alpha, alpha);
}
