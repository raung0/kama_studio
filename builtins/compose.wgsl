fn compose_linear_to_srgb(value: f32) -> f32 {
    let sign = select(-1.0, 1.0, value >= 0.0);
    let v = abs(value);
    return sign * select(12.92 * v, 1.055 * pow(v, 1.0 / 2.4) - 0.055, v > 0.0031308);
}

fn compose_srgb_to_linear(value: f32) -> f32 {
    let sign = select(-1.0, 1.0, value >= 0.0);
    let v = abs(value);
    return sign * select(v / 12.92, pow((v + 0.055) / 1.055, 2.4), v > 0.04045);
}

fn compose_blend_channel(dst: f32, src: f32, mode: u32) -> f32 {
    switch mode {
        case 1u: { return src + dst; }
        case 2u: { return dst - src; }
        case 3u: { return src * dst; }
        case 4u: { return 1.0 - (1.0 - src) * (1.0 - dst); }
        case 5u: { return select(1.0 - 2.0 * (1.0 - src) * (1.0 - dst), 2.0 * src * dst, dst <= 0.5); }
        case 6u: { return abs(dst - src); }
        case 7u: { return min(src, dst); }
        case 8u: { return max(src, dst); }
        case 9u: { return select(min(1.0, dst / max(1.0 - src, 0.000001)), 1.0, src >= 0.999999); }
        case 10u: { return select(1.0 - min(1.0, (1.0 - dst) / max(src, 0.000001)), 0.0, src <= 0.000001); }
        case 11u: { return select(1.0 - 2.0 * (1.0 - src) * (1.0 - dst), 2.0 * src * dst, src <= 0.5); }
        case 12u: {
            let d = select(sqrt(max(dst, 0.0)), ((16.0 * dst - 12.0) * dst + 4.0) * dst, dst <= 0.25);
            return select(dst + (2.0 * src - 1.0) * (d - dst), dst - (1.0 - 2.0 * src) * dst * (1.0 - dst), src <= 0.5);
        }
        case 13u: { return src + dst - 2.0 * src * dst; }
        case 14u: { return max(0.0, src + dst - 1.0); }
        case 15u: { return select(min(1.0, dst / max(src, 0.000001)), 1.0, src <= 0.000001 && dst > 0.0); }
        default: { return src; }
    }
}

fn blend_alpha(dst: f32, src: f32, mode: u32) -> f32 {
    switch mode {
        case 1u: { return dst; }
        case 2u: { return src; }
        case 3u: { return clamp(dst + src, 0.0, 1.0); }
        case 4u: { return clamp(dst - src, 0.0, 1.0); }
        case 5u: { return dst * src; }
        case 6u: { return min(dst, src); }
        case 7u: { return max(dst, src); }
        default: { return src + dst * (1.0 - src); }
    }
}

fn effect(
    image_1: texture_2d<f32>,
    image_2: texture_2d<f32>,
    pixel: vec2<u32>,
    uv: vec2<f32>,
    count: u32,
    blend_mode: u32,
    alpha_blend_mode: u32
) -> vec4<f32> {
    let p = vec2<i32>(pixel);
    let dst = textureLoad(image_1, p, 0);
    if (count <= 1u) { return dst; }
    let second_size = textureDimensions(image_2);
    let first_size = textureDimensions(image_1);
    let second_offset = (vec2<i32>(first_size) - vec2<i32>(second_size)) / 2;
    let second_pixel = p - second_offset;
    let source_in_bounds =
        all(second_pixel >= vec2<i32>(0)) && all(second_pixel < vec2<i32>(second_size));
    if (!source_in_bounds) { return dst; }
    let src = textureLoad(image_2, second_pixel, 0);
    let sa = clamp(src.a, 0.0, 1.0);
    let da = clamp(dst.a, 0.0, 1.0);
    let cs = src.rgb / max(sa, 0.000001);
    let cd = select(vec3<f32>(0.0), dst.rgb / max(da, 0.000001), da > 0.000001);
    let linear_src = clamp(cs, vec3<f32>(0.0), vec3<f32>(1.0));
    let linear_dst = clamp(cd, vec3<f32>(0.0), vec3<f32>(1.0));
    let blend_src = vec3<f32>(
        compose_linear_to_srgb(linear_src.r),
        compose_linear_to_srgb(linear_src.g),
        compose_linear_to_srgb(linear_src.b)
    );
    let blend_dst = vec3<f32>(
        compose_linear_to_srgb(linear_dst.r),
        compose_linear_to_srgb(linear_dst.g),
        compose_linear_to_srgb(linear_dst.b)
    );
    let blended_srgb = vec3<f32>(
        compose_blend_channel(blend_dst.r, blend_src.r, blend_mode),
        compose_blend_channel(blend_dst.g, blend_src.g, blend_mode),
        compose_blend_channel(blend_dst.b, blend_src.b, blend_mode)
    );
    let blended = vec3<f32>(
        compose_srgb_to_linear(blended_srgb.r),
        compose_srgb_to_linear(blended_srgb.g),
        compose_srgb_to_linear(blended_srgb.b)
    );
    let coverage_alpha = sa + da * (1.0 - sa);
    let coverage_rgb =
        (1.0 - sa) * dst.rgb + (1.0 - da) * src.rgb + sa * da * blended;
    let out_alpha = clamp(blend_alpha(da, sa, alpha_blend_mode), 0.0, 1.0);
    let straight_rgb = select(
        vec3<f32>(0.0),
        coverage_rgb / max(coverage_alpha, 0.000001),
        coverage_alpha > 0.000001
    );
    return vec4<f32>(straight_rgb * out_alpha, out_alpha);
}
