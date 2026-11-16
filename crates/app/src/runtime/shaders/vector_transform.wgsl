@group(0) @binding(0) var source_tex: texture_2d<f32>;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba16float, write>;
struct TransformParams { rows: array<vec4<f32>, 2>, }
@group(0) @binding(2) var<uniform> params: TransformParams;
fn load_or_zero(pixel: vec2<i32>) -> vec4<f32> {
    let size = vec2<i32>(textureDimensions(source_tex));
    if any(pixel < vec2<i32>(0)) || any(pixel >= size) { return vec4<f32>(0.0); }
    return textureLoad(source_tex, pixel, 0);
}
@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let size = textureDimensions(output_tex);
    if gid.x >= size.x || gid.y >= size.y { return; }
    let p = vec3<f32>(vec2<f32>(gid.xy) + vec2<f32>(0.5), 1.0);
    let position = vec2<f32>(dot(params.rows[0].xyz, p), dot(params.rows[1].xyz, p)) - vec2<f32>(0.5);
    let base = vec2<i32>(floor(position));
    let f = fract(position);
    let a = mix(load_or_zero(base), load_or_zero(base + vec2<i32>(1, 0)), f.x);
    let b = mix(load_or_zero(base + vec2<i32>(0, 1)), load_or_zero(base + vec2<i32>(1, 1)), f.x);
    textureStore(output_tex, vec2<i32>(gid.xy), mix(a, b, f.y));
}
