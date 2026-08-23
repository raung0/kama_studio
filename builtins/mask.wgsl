fn mask_sample(mask: texture_2d<f32>, uv: vec2<f32>) -> vec4<f32> {
    let dimensions = textureDimensions(mask);
    let pixel = clamp(
        vec2<i32>(uv * vec2<f32>(dimensions)),
        vec2<i32>(0),
        vec2<i32>(dimensions) - vec2<i32>(1),
    );
    return textureLoad(mask, pixel, 0);
}

fn effect(
    frame: texture_2d<f32>,
    mask: texture_2d<f32>,
    pixel: vec2<u32>,
    uv: vec2<f32>,
    channel: u32,
    invert: bool,
) -> vec4<f32> {
    let base = textureLoad(frame, vec2<i32>(pixel), 0);
    let mask_color = mask_sample(mask, uv);
    // Working textures are premultiplied, so luminance naturally falls to zero with alpha while
    // still preserving normal black/white matte semantics. The old max(alpha, luminance) made an
    // opaque black mask fully reveal the source.
    let luminance = dot(mask_color.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    var amount = select(
        select(luminance, mask_color.a, channel == 1u),
        luminance * mask_color.a,
        channel == 2u,
    );
    if (invert) {
        amount = 1.0 - amount;
    }
    return base * clamp(amount, 0.0, 1.0);
}
