@group(0) @binding(0) var source_texture: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;
@group(0) @binding(2) var destination_texture: texture_storage_2d<rgba16float, write>;

fn sample_source(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(source_texture, source_sampler, clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0)), 0.0);
}

@compute @workgroup_size(16, 16, 1)
fn cs_blur(@builtin(global_invocation_id) id: vec3<u32>) {
    let source_size = textureDimensions(source_texture);
    let size = textureDimensions(destination_texture);
    if id.x >= size.x || id.y >= size.y { return; }

    let texel = vec2<f32>(1.0) / vec2<f32>(size);
    let uv = (vec2<f32>(id.xy) + vec2<f32>(0.5)) * texel;
    var color = vec4<f32>(0.0);
    if source_size.x > size.x || source_size.y > size.y {
        color = sample_source(uv) * 0.5;
        color += sample_source(uv + texel * vec2<f32>(-1.0, -1.0)) * 0.125;
        color += sample_source(uv + texel * vec2<f32>( 1.0, -1.0)) * 0.125;
        color += sample_source(uv + texel * vec2<f32>(-1.0,  1.0)) * 0.125;
        color += sample_source(uv + texel * vec2<f32>( 1.0,  1.0)) * 0.125;
    } else {
        color += sample_source(uv + texel * vec2<f32>(-1.0, -1.0)) / 6.0;
        color += sample_source(uv + texel * vec2<f32>( 1.0, -1.0)) / 6.0;
        color += sample_source(uv + texel * vec2<f32>(-1.0,  1.0)) / 6.0;
        color += sample_source(uv + texel * vec2<f32>( 1.0,  1.0)) / 6.0;
        color += sample_source(uv + texel * vec2<f32>(-2.0,  0.0)) / 12.0;
        color += sample_source(uv + texel * vec2<f32>( 2.0,  0.0)) / 12.0;
        color += sample_source(uv + texel * vec2<f32>( 0.0, -2.0)) / 12.0;
        color += sample_source(uv + texel * vec2<f32>( 0.0,  2.0)) / 12.0;
    }
    textureStore(destination_texture, vec2<i32>(id.xy), color);
}
