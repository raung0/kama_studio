@group(0) @binding(0) var ui_cache: texture_2d<f32>;

@vertex
fn vs_present(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let position = vec2<f32>(f32(index & 1u), f32(index >> 1u)) * 4.0 - 1.0;
    return vec4<f32>(position, 0.0, 1.0);
}

@fragment
fn fs_present(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    var color = textureLoad(ui_cache, vec2<i32>(position.xy), 0);
    let noise = fract(52.9829189 * fract(dot(position.xy, vec2<f32>(0.06711056, 0.00583715)))) - 0.5;
    color = vec4<f32>(clamp(color.rgb + noise / 255.0, vec3<f32>(0.0), vec3<f32>(1.0)), color.a);
    return color;
}
