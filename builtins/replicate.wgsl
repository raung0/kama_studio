fn effect(source: texture_2d<f32>, pixel: vec2<u32>, uv: vec2<f32>, tiles: vec2<f32>, mirror: bool) -> vec4<f32> {
    let count = max(round(tiles), vec2<f32>(1.0));
    var cell = uv * count;
    let whole = floor(cell);
    cell = fract(cell);
    if (mirror) {
        let parity = vec2<u32>(whole) & vec2<u32>(1u);
        if (parity.x == 1u) { cell.x = 1.0 - cell.x; }
        if (parity.y == 1u) { cell.y = 1.0 - cell.y; }
    }
    let size = textureDimensions(source);
    let p = clamp(vec2<i32>(cell * vec2<f32>(size)), vec2<i32>(0), vec2<i32>(size) - vec2<i32>(1));
    return textureLoad(source, p, 0);
}
