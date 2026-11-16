@group(0) @binding(0) var source_tex: texture_2d<f32>;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba16float, write>;
struct ColorParams {
    rows: array<vec4<f32>, 4>,
    offsets: vec4<f32>,
}
@group(0) @binding(2) var<uniform> params: ColorParams;
@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let size = textureDimensions(output_tex);
    if gid.x >= size.x || gid.y >= size.y { return; }
    let pixel = vec2<i32>(gid.xy);
    let color = textureLoad(source_tex, pixel, 0);
    var straight = vec4<f32>(0.0, 0.0, 0.0, color.a);
    if color.a > 0.0 { straight = vec4<f32>(color.rgb / color.a, color.a); }
    let adjusted = vec4<f32>(
        dot(params.rows[0], straight),
        dot(params.rows[1], straight),
        dot(params.rows[2], straight),
        dot(params.rows[3], straight)
    ) + params.offsets;
    textureStore(output_tex, pixel, vec4<f32>(adjusted.rgb * adjusted.a, adjusted.a));
}
