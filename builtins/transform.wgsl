fn load_or_zero(source: texture_2d<f32>, coord: vec2<i32>, dimensions: vec2<u32>) -> vec4<f32> {
    if (coord.x < 0 || coord.y < 0 || coord.x >= i32(dimensions.x) || coord.y >= i32(dimensions.y)) {
        return vec4<f32>(0.0);
    }
    return textureLoad(source, coord, 0);
}

fn bilinear(source: texture_2d<f32>, position: vec2<f32>, dimensions: vec2<u32>) -> vec4<f32> {
    let base = vec2<i32>(floor(position));
    let fraction = fract(position);
    let a = load_or_zero(source, base, dimensions);
    let b = load_or_zero(source, base + vec2<i32>(1, 0), dimensions);
    let c = load_or_zero(source, base + vec2<i32>(0, 1), dimensions);
    let d = load_or_zero(source, base + vec2<i32>(1, 1), dimensions);
    return mix(mix(a, b, fraction.x), mix(c, d, fraction.x), fraction.y);
}

fn effect(
    source: texture_2d<f32>,
    pixel: vec2<u32>,
    uv: vec2<f32>,
    output_size: vec2<u32>,
    source_size: vec2<u32>,
    position: vec2<f32>,
    scale: vec2<f32>,
    anchor: vec2<f32>,
    rotation_degrees: f32,
) -> vec4<f32> {
    let size = vec2<f32>(output_size);
    let source_dimensions = vec2<f32>(source_size);
    let output_position = vec2<f32>(pixel) + vec2<f32>(0.5);
    let source_center = source_dimensions * 0.5;
    let placed_center = position * size;
    let safe_scale = vec2<f32>(
        select(scale.x, 0.000001, abs(scale.x) < 0.000001),
        select(scale.y, 0.000001, abs(scale.y) < 0.000001)
    );

    let anchor_source = anchor * source_dimensions;
    let scaled_anchor = placed_center + (anchor_source - source_center) * safe_scale;
    let angle = -rotation_degrees * 0.017453292519943295;
    let c = cos(angle);
    let s = sin(angle);
    let pivot_delta = output_position - scaled_anchor;
    let unrotated = vec2<f32>(
        pivot_delta.x * c - pivot_delta.y * s,
        pivot_delta.x * s + pivot_delta.y * c
    ) + scaled_anchor;
    let source_position = (unrotated - placed_center) / safe_scale
        + source_center
        - vec2<f32>(0.5);
    return bilinear(source, source_position, source_size);
}
