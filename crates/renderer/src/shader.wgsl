struct FrameUniform {
    width: u32,
    height: u32,
    tile_x_count: u32,
    tile_y_count: u32,
    tile_size: u32,
    mouse_x: f32,
    mouse_y: f32,
    reveal_radius: f32,
}

struct DrawCommand {
    rect: vec4<f32>,
    fill_color: vec4<f32>,
    border_color: vec4<f32>,
    params: vec4<f32>,
    reveal_color: vec4<f32>,
    fill_uv: vec4<f32>,
    border_uv: vec4<f32>,
    clip_rect_0: vec4<f32>,
    clip_rect_1: vec4<f32>,
    clip_rect_2: vec4<f32>,
    clip_rect_3: vec4<f32>,
    clip_radii: vec4<f32>,
    texture_and_id: vec4<u32>,
    shape_data: vec4<u32>,
}

struct GpuVertex {
    position: vec2<f32>,
}

@group(0) @binding(0) var<uniform> frame: FrameUniform;
@group(0) @binding(1) var<storage, read> commands: array<DrawCommand>;
@group(0) @binding(2) var base_cache: texture_2d<f32>;
@group(0) @binding(3) var<storage, read> tile_offsets: array<u32>;
@group(0) @binding(4) var<storage, read> tile_indices: array<u32>;
@group(0) @binding(6) var<storage, read_write> previous_hashes: array<u32>;
@group(0) @binding(7) var<storage, read_write> dirty_tiles: array<u32>;
@group(0) @binding(8) var<storage, read_write> scan_args: array<atomic<u32>>;
@group(0) @binding(9) var ui_cache_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(10) var atlas_sampler: sampler;
@group(0) @binding(11) var glyph_atlas: texture_2d<f32>;
@group(0) @binding(12) var icon_atlas: texture_2d<f32>;
@group(0) @binding(13) var user_atlas: texture_2d<f32>;
@group(0) @binding(14) var external_0: texture_2d<f32>;
@group(0) @binding(15) var external_1: texture_2d<f32>;
@group(0) @binding(16) var external_2: texture_2d<f32>;
@group(0) @binding(17) var external_3: texture_2d<f32>;
@group(0) @binding(18) var external_4: texture_2d<f32>;
@group(0) @binding(19) var external_5: texture_2d<f32>;
@group(0) @binding(20) var external_6: texture_2d<f32>;
@group(0) @binding(21) var external_7: texture_2d<f32>;
@group(0) @binding(22) var<storage, read> vertices: array<GpuVertex>;
@group(0) @binding(23) var<storage, read> overlay_tile_offsets: array<u32>;
@group(0) @binding(24) var<storage, read> overlay_tile_indices: array<u32>;
@group(0) @binding(25) var<storage, read> overlay_active_tiles: array<u32>;
@group(0) @binding(26) var blurred_cache: texture_2d<f32>;

fn mix_hash(h0: u32, value: u32) -> u32 {
    var h = h0 ^ value;
    h = h * 0x85ebca6bu;
    h = h ^ (h >> 13u);
    h = h * 0xc2b2ae35u;
    return h ^ (h >> 16u);
}

fn hash_vec4f(h0: u32, value: vec4<f32>) -> u32 {
    var h = h0;
    h = mix_hash(h, bitcast<u32>(value.x));
    h = mix_hash(h, bitcast<u32>(value.y));
    h = mix_hash(h, bitcast<u32>(value.z));
    h = mix_hash(h, bitcast<u32>(value.w));
    return h;
}

fn hash_vec4u(h0: u32, value: vec4<u32>) -> u32 {
    var h = h0;
    h = mix_hash(h, value.x);
    h = mix_hash(h, value.y);
    h = mix_hash(h, value.z);
    h = mix_hash(h, value.w);
    return h;
}

fn command_hash(command_index: u32) -> u32 {
    let command = commands[command_index];
    var h = 0x811c9dc5u;
    h = hash_vec4f(h, command.rect);
    h = hash_vec4f(h, command.fill_color);
    h = hash_vec4f(h, command.border_color);
    h = hash_vec4f(h, command.params);
    h = hash_vec4f(h, command.reveal_color);
    h = hash_vec4f(h, command.fill_uv);
    h = hash_vec4f(h, command.border_uv);
    h = hash_vec4f(h, command.clip_rect_0);
    h = hash_vec4f(h, command.clip_rect_1);
    h = hash_vec4f(h, command.clip_rect_2);
    h = hash_vec4f(h, command.clip_rect_3);
    h = hash_vec4f(h, command.clip_radii);
    h = hash_vec4u(h, command.texture_and_id);
    if command.shape_data.x == 1u {
        // The UI folds a hash of local mesh geometry into texture_and_id. The vertex-buffer offset
        // is only an address and can change when unrelated meshes appear/disappear, so excluding it
        // prevents false invalidation cascades. Vertex count and clip count are still visual state.
        h = mix_hash(h, command.shape_data.x);
        h = mix_hash(h, command.shape_data.z);
        h = mix_hash(h, command.shape_data.w);
    } else {
        h = hash_vec4u(h, command.shape_data);
    }
    // Command order is already encoded by the tile hash folding this sequence in draw order, and
    // stable block IDs are part of texture_and_id. Hashing the transient command-buffer index made
    // every later command look dirty whenever an off-screen row was culled or became visible.

    if command.params.w != 0.0 {
        h = mix_hash(h, bitcast<u32>(frame.mouse_x));
        h = mix_hash(h, bitcast<u32>(frame.mouse_y));
    }

    return h;
}

@compute @workgroup_size(64)
fn cs_scan(@builtin(global_invocation_id) gid: vec3<u32>) {
    let tile = gid.x;
    let tile_count = frame.tile_x_count * frame.tile_y_count;
    if tile >= tile_count { return; }

    let start = tile_offsets[tile];
    let end = tile_offsets[tile + 1u];
    var current = mix_hash(0x811c9dc5u, end - start);
    var index = start;
    loop {
        if index >= end { break; }
        current = mix_hash(current, command_hash(tile_indices[index]));
        index += 1u;
    }

    if current != previous_hashes[tile] {
        previous_hashes[tile] = current;
        let work_index = atomicAdd(&scan_args[0], 1u);
        dirty_tiles[work_index] = tile;
    }
}

fn rounded_rect_distance(point: vec2<f32>, rect: vec4<f32>, radius: f32) -> f32 {
    let center = rect.xy + rect.zw * 0.5;
    let half_size = max(rect.zw * 0.5, vec2<f32>(0.0));
    let r = min(max(radius, 0.0), min(half_size.x, half_size.y));
    let q = abs(point - center) - (half_size - vec2<f32>(r));
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

fn coverage_for(point: vec2<f32>, rect: vec4<f32>, radius: f32) -> f32 {
    return clamp(0.5 - rounded_rect_distance(point, rect, radius), 0.0, 1.0);
}

fn command_clip_rect(command: DrawCommand, index: u32) -> vec4<f32> {
    if index == 0u { return command.clip_rect_0; }
    if index == 1u { return command.clip_rect_1; }
    if index == 2u { return command.clip_rect_2; }
    return command.clip_rect_3;
}

fn command_clip_radius(command: DrawCommand, index: u32) -> f32 {
    if index == 0u { return command.clip_radii.x; }
    if index == 1u { return command.clip_radii.y; }
    if index == 2u { return command.clip_radii.z; }
    return command.clip_radii.w;
}

fn clip_coverage(command: DrawCommand, point: vec2<f32>) -> f32 {
    var coverage = 1.0;
    var index = 0u;
    let count = min(command.shape_data.w, 4u);
    loop {
        if index >= count { break; }
        coverage *= coverage_for(point, command_clip_rect(command, index), command_clip_radius(command, index));
        if coverage <= 0.0 { break; }
        index += 1u;
    }
    return coverage;
}

fn edge(a: vec2<f32>, b: vec2<f32>, point: vec2<f32>) -> f32 {
    return (point.x - a.x) * (b.y - a.y) - (point.y - a.y) * (b.x - a.x);
}

fn point_in_triangle(point: vec2<f32>, a: vec2<f32>, b: vec2<f32>, c: vec2<f32>) -> bool {
    let e0 = edge(a, b, point);
    let e1 = edge(b, c, point);
    let e2 = edge(c, a, point);
    let has_negative = e0 < 0.0 || e1 < 0.0 || e2 < 0.0;
    let has_positive = e0 > 0.0 || e1 > 0.0 || e2 > 0.0;
    return !(has_negative && has_positive);
}

fn mesh_contains(command: DrawCommand, point: vec2<f32>) -> bool {
    var triangle_index = 0u;
    loop {
        if triangle_index + 2u >= command.shape_data.z { break; }
        let offset = command.shape_data.y + triangle_index;
        let a = vertices[offset].position;
        let b = vertices[offset + 1u].position;
        let c = vertices[offset + 2u].position;
        if point_in_triangle(point, a, b, c) {
            return true;
        }
        triangle_index += 3u;
    }
    return false;
}

fn mesh_coverage(command: DrawCommand, point: vec2<f32>) -> f32 {
    var covered = 0.0;
    if mesh_contains(command, point + vec2<f32>(-0.25, -0.25)) { covered += 0.25; }
    if mesh_contains(command, point + vec2<f32>( 0.25, -0.25)) { covered += 0.25; }
    if mesh_contains(command, point + vec2<f32>(-0.25,  0.25)) { covered += 0.25; }
    if mesh_contains(command, point + vec2<f32>( 0.25,  0.25)) { covered += 0.25; }
    return covered;
}

fn shape_coverage(command: DrawCommand, point: vec2<f32>) -> f32 {
    if command.shape_data.x == 1u { return mesh_coverage(command, point); }
    return coverage_for(point, command.rect, command.params.x);
}

fn sample_texture(kind: u32, uv: vec2<f32>) -> vec4<f32> {
    if kind == 0u { return vec4<f32>(1.0); }
    if kind == 1u {
        let size = vec2<i32>(textureDimensions(glyph_atlas));
        let texel = clamp(vec2<i32>(uv * vec2<f32>(size)), vec2<i32>(0), size - vec2<i32>(1));
        return textureLoad(glyph_atlas, texel, 0);
    }
    if kind == 2u { return textureSampleLevel(icon_atlas, atlas_sampler, uv, 0.0); }
    if kind == 3u { return textureSampleLevel(user_atlas, atlas_sampler, uv, 0.0); }
    if kind == 4u { return textureSampleLevel(external_0, atlas_sampler, uv, 0.0); }
    if kind == 5u { return textureSampleLevel(external_1, atlas_sampler, uv, 0.0); }
    if kind == 6u { return textureSampleLevel(external_2, atlas_sampler, uv, 0.0); }
    if kind == 7u { return textureSampleLevel(external_3, atlas_sampler, uv, 0.0); }
    if kind == 8u { return textureSampleLevel(external_4, atlas_sampler, uv, 0.0); }
    if kind == 9u { return textureSampleLevel(external_5, atlas_sampler, uv, 0.0); }
    if kind == 10u { return textureSampleLevel(external_6, atlas_sampler, uv, 0.0); }
    return textureSampleLevel(external_7, atlas_sampler, uv, 0.0);
}

fn analytic_waveform(command: DrawCommand, local_uv: vec2<f32>) -> vec4<f32> {
    let source_x = mix(command.fill_uv.x, command.fill_uv.z, local_uv.x);
    let mode = command.shape_data.z;
    if mode == 1u {
        let sample = sample_texture(command.texture_and_id.x, vec2<f32>(source_x, command.fill_uv.y));
        let baseline = 1.0;
        let top = baseline - (0.04 + sample.a * 0.96);
        if local_uv.y < top || local_uv.y > baseline { return vec4<f32>(0.0); }
        let edge = 1.0 - smoothstep(0.0, 0.018, abs(local_uv.y - top));
        return vec4<f32>(min(sample.rgb + vec3<f32>(0.10 * edge), vec3<f32>(1.0)), 0.88);
    }
    if mode == 2u {
        let top_levels = sample_texture(
            command.texture_and_id.x,
            vec2<f32>(source_x, command.fill_uv.y),
        ).rgb;
        let bottom_levels = sample_texture(
            command.texture_and_id.x,
            vec2<f32>(source_x, command.fill_uv.w),
        ).rgb;
        let levels = select(top_levels, bottom_levels, local_uv.y >= 0.5);
        let distance_from_center = abs(local_uv.y - 0.5) * 6.0;
        var color = vec3<f32>(0.122);
        if distance_from_center <= levels.x {
            color = vec3<f32>(0.287);
        } else if distance_from_center <= levels.x + levels.y {
            color = vec3<f32>(0.539);
        } else if distance_from_center <= levels.x + levels.y + levels.z {
            color = vec3<f32>(0.880);
        }
        if abs(local_uv.y - 0.5) < 0.012 {
            color = vec3<f32>(0.162);
        }
        return vec4<f32>(color, 0.90);
    }
    return vec4<f32>(0.0);
}

fn over(source_straight: vec4<f32>, destination_premul: vec4<f32>) -> vec4<f32> {
    let source = vec4<f32>(source_straight.rgb * source_straight.a, source_straight.a);
    return source + destination_premul * (1.0 - source.a);
}

fn reveal_factor(point: vec2<f32>, strength: f32) -> f32 {
    let renderer_reveal_strength = abs(strength);
    if renderer_reveal_strength <= 0.0 { return 0.0; }
    let mouse = vec2<f32>(frame.mouse_x, frame.mouse_y);
    let radius = max(frame.reveal_radius, 1.0);
    let normalized = clamp(1.0 - distance(point, mouse) / radius, 0.0, 1.0);
    let smooth_profile = normalized * normalized * normalized *
        (normalized * (normalized * 6.0 - 15.0) + 10.0);
    return smooth_profile * renderer_reveal_strength;
}

fn reveal_light(point: vec2<f32>, command: DrawCommand, source: vec4<f32>, amount: f32) -> vec4<f32> {
    let glow = reveal_factor(point, command.params.w);
    let reveal_target = command.reveal_color.rgb;
    let lit_rgb = source.rgb + (reveal_target - source.rgb) * glow * amount;
    return vec4<f32>(clamp(lit_rgb, vec3<f32>(0.0), vec3<f32>(1.0)), source.a);
}

fn reveal_fill(point: vec2<f32>, command: DrawCommand, source: vec4<f32>) -> vec4<f32> {
    if command.params.w < 0.0 { return source; }
    return reveal_light(point, command, source, 0.018);
}

fn reveal_border(point: vec2<f32>, command: DrawCommand, source: vec4<f32>) -> vec4<f32> {
    return reveal_light(point, command, source, 0.045);
}

fn draw_command(command: DrawCommand, point: vec2<f32>, color: vec4<f32>) -> vec4<f32> {
    let clipping = clip_coverage(command, point);
    let outer = shape_coverage(command, point) * clipping;
    if outer <= 0.0 { return color; }

    var inner = outer;
    var border_coverage = 0.0;
    if command.shape_data.x == 0u && command.params.y > 0.0 {
        let border_width = command.params.y;
        let inner_rect = vec4<f32>(
            command.rect.xy + vec2<f32>(border_width),
            max(command.rect.zw - vec2<f32>(border_width * 2.0), vec2<f32>(0.0)),
        );
        inner = coverage_for(point, inner_rect, max(command.params.x - border_width, 0.0)) * clipping;
        border_coverage = max(outer - inner, 0.0);
    }

    let local_uv = clamp(
        (point - command.rect.xy) / max(command.rect.zw, vec2<f32>(1.0)),
        vec2<f32>(0.0),
        vec2<f32>(1.0),
    );
    var fill_local_uv = local_uv;
    if command.shape_data.x == 0u {
        let rotation = bitcast<f32>(command.shape_data.y);
        let centered_uv = local_uv - vec2<f32>(0.5);
        let c = cos(rotation);
        let s = sin(rotation);
        fill_local_uv = vec2<f32>(
            centered_uv.x * c + centered_uv.y * s,
            -centered_uv.x * s + centered_uv.y * c,
        ) + vec2<f32>(0.5);
    }
    var fill_sample = vec4<f32>(0.0);
    if all(fill_local_uv >= vec2<f32>(0.0)) && all(fill_local_uv <= vec2<f32>(1.0)) {
        if command.shape_data.x != 0u || command.shape_data.z == 0u {
            fill_sample = sample_texture(
                command.texture_and_id.x,
                mix(command.fill_uv.xy, command.fill_uv.zw, fill_local_uv),
            );
        } else {
            fill_sample = analytic_waveform(command, fill_local_uv);
        }
    }
    let border_sample = sample_texture(
        command.texture_and_id.y,
        mix(command.border_uv.xy, command.border_uv.zw, local_uv),
    );
    let fill_source = reveal_fill(point, command, command.fill_color * fill_sample);
    let border_source = reveal_border(point, command, command.border_color * border_sample);
    let opacity = command.params.z;
    var result = over(vec4<f32>(border_source.rgb, border_source.a * border_coverage * opacity), color);
    return over(vec4<f32>(fill_source.rgb, fill_source.a * inner * opacity), result);
}

@compute @workgroup_size(16, 16, 1)
fn cs_paint(
    @builtin(workgroup_id) workgroup_id: vec3<u32>,
    @builtin(local_invocation_id) local_id: vec3<u32>,
) {
    let tile = dirty_tiles[workgroup_id.x];
    let tile_x = tile % frame.tile_x_count;
    let tile_y = tile / frame.tile_x_count;
    let pixel = vec2<u32>(tile_x * frame.tile_size + local_id.x, tile_y * frame.tile_size + local_id.y);
    if pixel.x >= frame.width || pixel.y >= frame.height { return; }

    let point = vec2<f32>(pixel) + vec2<f32>(0.5);
    var color = vec4<f32>(0.0);
    var index = tile_offsets[tile];
    let end = tile_offsets[tile + 1u];
    loop {
        if index >= end { break; }
        color = draw_command(commands[tile_indices[index]], point, color);
        index += 1u;
    }

    textureStore(ui_cache_out, vec2<i32>(pixel), color);
}

fn blurred_base(point: vec2<f32>) -> vec4<f32> {
    let uv = clamp(point / vec2<f32>(f32(frame.width), f32(frame.height)), vec2<f32>(0.0), vec2<f32>(1.0));
    return textureSampleLevel(blurred_cache, atlas_sampler, uv, 0.0);
}

@compute @workgroup_size(16, 16, 1)
fn cs_overlay(
    @builtin(workgroup_id) workgroup_id: vec3<u32>,
    @builtin(local_invocation_id) local_id: vec3<u32>,
) {
    let tile = overlay_active_tiles[workgroup_id.x];
    let pixel = vec2<u32>(
        (tile % frame.tile_x_count) * frame.tile_size + local_id.x,
        (tile / frame.tile_x_count) * frame.tile_size + local_id.y,
    );
    if pixel.x >= frame.width || pixel.y >= frame.height { return; }

    let point = vec2<f32>(pixel) + vec2<f32>(0.5);
    var color = textureLoad(base_cache, vec2<i32>(pixel), 0);
    var list_index = overlay_tile_offsets[tile];
    let command_end = overlay_tile_offsets[tile + 1u];
    loop {
        if list_index >= command_end { break; }
        let command = commands[overlay_tile_indices[list_index]];
        if command.shape_data.x == 2u {
            let amount = clamp(shape_coverage(command, point) * clip_coverage(command, point) * command.params.z, 0.0, 1.0);
            color = mix(color, over(command.fill_color, blurred_base(point)), amount);
        } else {
            color = draw_command(command, point, color);
        }
        list_index += 1u;
    }
    textureStore(ui_cache_out, vec2<i32>(pixel), color);
}
