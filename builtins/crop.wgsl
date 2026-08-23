fn effect(color: vec4<f32>, uv: vec2<f32>, crop: vec4<f32>, feather: f32) -> vec4<f32> {
    let left = crop.x;
    let top = crop.y;
    let right = 1.0 - crop.z;
    let bottom = 1.0 - crop.w;
    let f = max(feather, 0.00001);
    let mask_x = smoothstep(left, left + f, uv.x) * (1.0 - smoothstep(right - f, right, uv.x));
    let mask_y = smoothstep(top, top + f, uv.y) * (1.0 - smoothstep(bottom - f, bottom, uv.y));
    return color * mask_x * mask_y;
}
