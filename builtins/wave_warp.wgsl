fn wave_sample(source: texture_2d<f32>, uv: vec2<f32>) -> vec4<f32> {
    let size = textureDimensions(source);
    if (any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0))) { return vec4<f32>(0.0); }
    let max_pixel = vec2<i32>(size) - vec2<i32>(1);
    let position = clamp(
        uv * vec2<f32>(size) - vec2<f32>(0.5),
        vec2<f32>(0.0),
        vec2<f32>(max_pixel),
    );
    let base = vec2<i32>(floor(position));
    let next = min(base + vec2<i32>(1), max_pixel);
    let f = fract(position);
    let a = mix(textureLoad(source, base, 0), textureLoad(source, vec2<i32>(next.x, base.y), 0), f.x);
    let b = mix(textureLoad(source, vec2<i32>(base.x, next.y), 0), textureLoad(source, next, 0), f.x);
    return mix(a, b, f.y);
}

fn wave_hash(value: f32) -> f32 {
    return fract(sin(value * 127.1 + 311.7) * 43758.5453123) * 2.0 - 1.0;
}

fn wave_shape(phase: f32, wave_type: u32) -> f32 {
    let tau = 6.28318530718;
    let cycle = fract(phase / tau);
    let centered = cycle * 2.0 - 1.0;
    switch wave_type {
        case 1u: { return select(-1.0, 1.0, sin(phase) >= 0.0); }
        case 2u: { return 1.0 - 4.0 * abs(cycle - 0.5); }
        case 3u: { return centered; }
        case 4u: { return -centered; }
        case 5u: {
            let x = centered;
            let arc = sqrt(max(0.0, 1.0 - x * x));
            return select(-arc, arc, cycle < 0.5);
        }
        case 6u: { return sqrt(max(0.0, 1.0 - centered * centered)) * 2.0 - 1.0; }
        case 7u: { return 1.0 - sqrt(max(0.0, 1.0 - centered * centered)) * 2.0; }
        case 8u: { return wave_hash(floor(phase / tau)); }
        case 9u: {
            let cell = floor(phase / tau);
            let t = smoothstep(0.0, 1.0, cycle);
            return mix(wave_hash(cell), wave_hash(cell + 1.0), t);
        }
        default: { return sin(phase); }
    }
}

fn wave_pin_edge(distance: f32, feather: f32) -> f32 {
    return smoothstep(0.0, max(feather, 1.0), max(distance, 0.0));
}

fn wave_pin_amount(
    pixel: vec2<f32>,
    output_size: vec2<u32>,
    amplitude: f32,
    pinning: u32,
    pin_top: bool,
    pin_bottom: bool,
    pin_left: bool,
    pin_right: bool
) -> f32 {
    if (pinning == 0u) { return 1.0; }
    let size = vec2<f32>(output_size);
    let feather = max(abs(amplitude) * 1.5, 8.0);
    let top = wave_pin_edge(pixel.y, feather);
    let bottom = wave_pin_edge(size.y - 1.0 - pixel.y, feather);
    let left = wave_pin_edge(pixel.x, feather);
    let right = wave_pin_edge(size.x - 1.0 - pixel.x, feather);
    switch pinning {
        case 1u: { return min(min(top, bottom), min(left, right)); }
        case 2u: { return min(top, bottom); }
        case 3u: { return min(left, right); }
        case 4u: {
            var amount = 1.0;
            var selected = false;
            if (pin_top) { amount = min(amount, top); selected = true; }
            if (pin_bottom) { amount = min(amount, bottom); selected = true; }
            if (pin_left) { amount = min(amount, left); selected = true; }
            if (pin_right) { amount = min(amount, right); selected = true; }
            if (selected) { return amount; }
            return 1.0;
        }
        default: { return 1.0; }
    }
}

fn effect(
    source: texture_2d<f32>,
    pixel: vec2<u32>,
    uv: vec2<f32>,
    output_size: vec2<u32>,
    local_time: f32,
    wave_type: u32,
    amplitude: f32,
    wavelength: f32,
    speed: f32,
    direction: f32,
    phase: f32,
    pinning: u32,
    pin_top: bool,
    pin_bottom: bool,
    pin_left: bool,
    pin_right: bool
) -> vec4<f32> {
    let angle = direction * 0.017453292519943295;
    let axis = vec2<f32>(cos(angle), sin(angle));
    let normal = vec2<f32>(-axis.y, axis.x);
    let p = vec2<f32>(pixel);
    let phase_radians = phase * 0.017453292519943295;
    let wave_phase = dot(p, axis) / max(wavelength, 1.0) * 6.28318530718
        - local_time * speed * 6.28318530718
        + phase_radians;
    let pin = wave_pin_amount(
        p,
        output_size,
        amplitude,
        pinning,
        pin_top,
        pin_bottom,
        pin_left,
        pin_right,
    );
    let offset = normal * wave_shape(wave_phase, wave_type) * amplitude * pin / vec2<f32>(output_size);
    return wave_sample(source, uv + offset);
}
