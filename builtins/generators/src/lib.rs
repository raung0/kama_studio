use kama_plugin::{generator, get_parameter, parameter_id};
use std::mem;

#[no_mangle]
pub extern "C" fn kama_alloc(len: i32) -> i32 {
    if len <= 0 {
        return 0;
    }
    let bytes = vec![0u8; len as usize].into_boxed_slice();
    Box::into_raw(bytes) as *mut u8 as i32
}






#[no_mangle]
pub unsafe extern "C" fn kama_dealloc(pointer: i32, len: i32) {
    if pointer > 0 && len > 0 {
        let slice = std::ptr::slice_from_raw_parts_mut(pointer as *mut u8, len as usize);
        drop(Box::from_raw(slice));
    }
}

fn frame(width: i32, height: i32) -> (Vec<f32>, usize) {
    let count = (width.max(1) as usize)
        .saturating_mul(height.max(1) as usize)
        .saturating_mul(4);
    let mut pixels = vec![0.0f32; count];
    let pointer = pixels.as_mut_ptr() as usize;
    (pixels, pointer)
}

fn export_frame(pixels: Vec<f32>, pointer: usize) -> i32 {
    mem::forget(pixels);
    pointer as i32
}

fn export_monitor_overlay(
    handles: &[(u32, i32, [f32; 2], [f32; 2])],
    lines: &[[u32; 2]],
) -> i64 {
    let mut bytes = Vec::with_capacity(12 + handles.len() * 24 + lines.len() * 8);
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&(handles.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&(lines.len() as u32).to_le_bytes());
    for (target, element, position, origin) in handles {
        bytes.extend_from_slice(&target.to_le_bytes());
        bytes.extend_from_slice(&element.to_le_bytes());
        for value in [position[0], position[1], origin[0], origin[1]] {
            bytes.extend_from_slice(&value.to_bits().to_le_bytes());
        }
    }
    for [start, end] in lines {
        bytes.extend_from_slice(&start.to_le_bytes());
        bytes.extend_from_slice(&end.to_le_bytes());
    }
    let len = bytes.len() as u32;
    let pointer = bytes.as_mut_ptr() as usize as u32;
    mem::forget(bytes);
    (i64::from(len) << 32) | i64::from(pointer)
}

#[no_mangle]
pub extern "C" fn monitor_mesh_warp(width: f32, height: f32, _time: f64) -> i64 {
    let definitions = [
        ("top_left", [0.0, 0.0]),
        ("top_center", [0.5, 0.0]),
        ("top_right", [1.0, 0.0]),
        ("center_right", [1.0, 0.5]),
        ("bottom_right", [1.0, 1.0]),
        ("bottom_center", [0.5, 1.0]),
        ("bottom_left", [0.0, 1.0]),
        ("center_left", [0.0, 0.5]),
    ];
    let handles = definitions
        .iter()
        .map(|(input, anchor)| {
            let origin = [anchor[0] * width.max(1.0), anchor[1] * height.max(1.0)];
            let offset = get_parameter(input, [0.0f32; 2]);
            (
                parameter_id(input),
                -1,
                [origin[0] + offset[0], origin[1] + offset[1]],
                origin,
            )
        })
        .collect::<Vec<_>>();
    export_monitor_overlay(
        &handles,
        &[[0, 1], [1, 2], [2, 3], [3, 4], [4, 5], [5, 6], [6, 7], [7, 0]],
    )
}

fn monitor_point_path(input: &str, fallback: Vec<[f32; 2]>, closed: bool) -> i64 {
    let points = get_parameter(input, fallback);
    let target = parameter_id(input);
    let handles = points
        .iter()
        .enumerate()
        .map(|(index, point)| (target, index as i32, *point, [0.0, 0.0]))
        .collect::<Vec<_>>();
    let segment_count = if closed {
        points.len()
    } else {
        points.len().saturating_sub(1)
    };
    let lines = (0..segment_count)
        .map(|index| [index as u32, ((index + 1) % points.len().max(1)) as u32])
        .collect::<Vec<_>>();
    export_monitor_overlay(&handles, &lines)
}

#[no_mangle]
pub extern "C" fn monitor_gradient(_width: f32, _height: f32, _time: f64) -> i64 {
    monitor_point_path(
        "points",
        vec![[320.0, 540.0], [1600.0, 540.0]],
        false,
    )
}

#[no_mangle]
pub extern "C" fn monitor_polygon(_width: f32, _height: f32, _time: f64) -> i64 {
    monitor_point_path(
        "points",
        vec![[32.0, 24.0], [256.0, 24.0], [288.0, 176.0], [48.0, 208.0]],
        true,
    )
}

#[no_mangle]
pub extern "C" fn monitor_shape(width: f32, height: f32, _time: f64) -> i64 {
    let center = [width.max(1.0) * 0.5, height.max(1.0) * 0.5];
    if get_parameter("shape_type", 0u32) == 1 {
        let radius = get_parameter("radius", [480.0f32, 270.0]);
        let handles = [
            [center[0] - radius[0], center[1]],
            [center[0] + radius[0], center[1]],
            [center[0], center[1] - radius[1]],
            [center[0], center[1] + radius[1]],
        ]
        .into_iter()
        .map(|position| (parameter_id("radius"), -1, position, center))
        .collect::<Vec<_>>();
        export_monitor_overlay(&handles, &[[0, 1], [2, 3]])
    } else {
        let size = get_parameter("size", [960.0f32, 540.0]);
        let extent = [size[0] * 0.5, size[1] * 0.5];
        let handles = [
            [center[0] - extent[0], center[1] - extent[1]],
            [center[0] + extent[0], center[1] - extent[1]],
            [center[0] + extent[0], center[1] + extent[1]],
            [center[0] - extent[0], center[1] + extent[1]],
        ]
        .into_iter()
        .map(|position| (parameter_id("size"), -1, position, center))
        .collect::<Vec<_>>();
        export_monitor_overlay(&handles, &[[0, 1], [1, 2], [2, 3], [3, 0]])
    }
}

fn color_parameter(name: &str, fallback: [f32; 4]) -> [f32; 4] {
    let mut color = get_parameter(name, fallback);
    color[3] = color[3].clamp(0.0, 1.0);
    color
}

fn color() -> [f32; 4] {
    color_parameter("color", [1.0, 1.0, 1.0, 1.0])
}

fn border_extents(width: f32, alignment: u32) -> (f32, f32) {
    let width = width.max(0.0);
    match alignment.min(2) {
        1 => (width * 0.5, width * 0.5),
        2 => (0.0, width),
        _ => (width, 0.0),
    }
}

fn premultiply([r, g, b, a]: [f32; 4]) -> [f32; 4] {
    [r * a, g * a, b * a, a]
}

#[no_mangle]
pub extern "C" fn generate_solid(
    _params_ptr: i32,
    _params_len: i32,
    width: i32,
    height: i32,
    _time: f64,
) -> i32 {
    let (mut pixels, pointer) = frame(width, height);
    let [r, g, b, a] = color();
    let premul = [r * a, g * a, b * a, a];
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.copy_from_slice(&premul);
    }
    export_frame(pixels, pointer)
}

#[no_mangle]
pub extern "C" fn generate_shape(
    _params_ptr: i32,
    _params_len: i32,
    width: i32,
    height: i32,
    _time: f64,
) -> i32 {
    let width = width.max(1);
    let height = height.max(1);
    let (mut pixels, pointer) = frame(width, height);
    let fill = premultiply(color());
    let border = premultiply(color_parameter("border_color", [0.05, 0.05, 0.05, 1.0]));
    let scale = generator::render_scale().max(0.000_001);
    let border_width = get_parameter("border_width", 0.0f32).max(0.0) * scale;
    let border_alignment = get_parameter("border_alignment", 0u32);
    let (border_inner, border_outer) = border_extents(border_width, border_alignment);
    let center_x = width as f32 * 0.5;
    let center_y = height as f32 * 0.5;

    match get_parameter("shape_type", 0u32) {
        1 => {
            let [radius_x, radius_y] = get_parameter("radius", [480.0f32, 270.0]);
            let radius_x = radius_x.max(0.5) * scale;
            let radius_y = radius_y.max(0.5) * scale;

            if border_width > 0.0 {
                fill_ellipse(
                    &mut pixels,
                    width as usize,
                    height as usize,
                    center_x,
                    center_y,
                    radius_x + border_outer,
                    radius_y + border_outer,
                    border,
                );
                let inner_x = (radius_x - border_inner).max(0.0);
                let inner_y = (radius_y - border_inner).max(0.0);
                if inner_x > 0.5 && inner_y > 0.5 {
                    fill_ellipse(
                        &mut pixels,
                        width as usize,
                        height as usize,
                        center_x,
                        center_y,
                        inner_x,
                        inner_y,
                        fill,
                    );
                }
            } else {
                fill_ellipse(
                    &mut pixels,
                    width as usize,
                    height as usize,
                    center_x,
                    center_y,
                    radius_x,
                    radius_y,
                    fill,
                );
            }
        }
        _ => {
            let [shape_width, shape_height] = get_parameter("size", [960.0f32, 540.0]);
            let half_width = shape_width.max(0.5) * scale * 0.5;
            let half_height = shape_height.max(0.5) * scale * 0.5;

            if border_width > 0.0 {
                fill_centered_rect(
                    &mut pixels,
                    width as usize,
                    height as usize,
                    center_x,
                    center_y,
                    half_width + border_outer,
                    half_height + border_outer,
                    border,
                );
                let inner_half_width = (half_width - border_inner).max(0.0);
                let inner_half_height = (half_height - border_inner).max(0.0);
                if inner_half_width > 0.25 && inner_half_height > 0.25 {
                    fill_centered_rect(
                        &mut pixels,
                        width as usize,
                        height as usize,
                        center_x,
                        center_y,
                        inner_half_width,
                        inner_half_height,
                        fill,
                    );
                }
            } else {
                fill_centered_rect(
                    &mut pixels,
                    width as usize,
                    height as usize,
                    center_x,
                    center_y,
                    half_width,
                    half_height,
                    fill,
                );
            }
        }
    }
    export_frame(pixels, pointer)
}

fn default_gradient_color(index: usize) -> [f32; 4] {
    const PALETTE: [[f32; 4]; 8] = [
        [0.97, 0.32, 0.46, 1.0],
        [0.20, 0.58, 0.99, 1.0],
        [0.26, 0.83, 0.60, 1.0],
        [0.99, 0.73, 0.23, 1.0],
        [0.61, 0.41, 0.98, 1.0],
        [0.99, 0.47, 0.23, 1.0],
        [0.20, 0.86, 0.90, 1.0],
        [0.95, 0.32, 0.76, 1.0],
    ];
    PALETTE[index % PALETTE.len()]
}

fn read_gradient_color(values: &[f32], index: usize, fallback: [f32; 4]) -> [f32; 4] {
    let base = index.saturating_mul(4);
    let Some(values) = values.get(base..base + 4) else {
        return fallback;
    };
    [values[0], values[1], values[2], values[3].clamp(0.0, 1.0)]
}

fn premultiply_linear([r, g, b, a]: [f32; 4]) -> [f32; 4] {
    let a = a.clamp(0.0, 1.0);
    [r * a, g * a, b * a, a]
}

fn mix_premultiplied_gradient(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    let t = t.clamp(0.0, 1.0);
    let a = premultiply_linear(a);
    let b = premultiply_linear(b);
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
        a[3] + (b[3] - a[3]) * t,
    ]
}

const GRADIENT_LUT_SIZE: usize = 1024;

fn monotonic_stop_positions(mut positions: Vec<f32>) -> Vec<f32> {
    if positions.is_empty() {
        return positions;
    }
    positions[0] = 0.0;
    let last = positions.len() - 1;
    if last == 0 {
        return positions;
    }
    positions[last] = 1.0;
    let mut previous = 0.0;
    for position in &mut positions[1..last] {
        *position = position.clamp(previous, 1.0);
        previous = *position;
    }
    positions
}

fn gradient_midpoint_bias(t: f32, midpoint: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t <= 0.0 || t >= 1.0 {
        return t;
    }
    let midpoint = midpoint.clamp(0.01, 0.99);
    let exponent = 0.5f32.ln() / midpoint.ln();
    t.powf(exponent)
}

fn build_gradient_lut(
    positions: &[f32],
    midpoints: &[f32],
    colors: &[[f32; 4]],
) -> Vec<[f32; 4]> {
    if colors.is_empty() {
        return vec![[0.0; 4]; GRADIENT_LUT_SIZE];
    }
    if colors.len() == 1 || positions.len() <= 1 {
        return vec![premultiply_linear(colors[0]); GRADIENT_LUT_SIZE];
    }

    let mut lut = Vec::with_capacity(GRADIENT_LUT_SIZE);
    let mut segment = 1usize;
    for sample in 0..GRADIENT_LUT_SIZE {
        let t = sample as f32 / (GRADIENT_LUT_SIZE - 1) as f32;
        while segment + 1 < positions.len() && t > positions[segment] {
            segment += 1;
        }
        let left = segment.saturating_sub(1).min(colors.len() - 1);
        let right = segment.min(colors.len() - 1);
        let start = positions.get(left).copied().unwrap_or(0.0);
        let end = positions.get(right).copied().unwrap_or(1.0);
        let local = if end <= start + 1e-6 {
            1.0
        } else {
            (t - start) / (end - start)
        };
        let midpoint = midpoints.get(left).copied().unwrap_or(0.5);
        lut.push(mix_premultiplied_gradient(
            colors[left],
            colors[right],
            gradient_midpoint_bias(local, midpoint),
        ));
    }
    lut
}

fn lut_index(t: f32) -> usize {
    (t.clamp(0.0, 1.0) * (GRADIENT_LUT_SIZE - 1) as f32).round() as usize
}

fn linear_gradient_geometry(points: &[[f32; 2]]) -> ([f32; 2], [f32; 2], f32, Vec<f32>) {
    let start = points[0];
    let end = *points.last().unwrap_or(&start);
    let axis = [end[0] - start[0], end[1] - start[1]];
    let length_sq = axis[0] * axis[0] + axis[1] * axis[1];
    if length_sq <= 1e-6 {
        return (start, axis, length_sq, vec![0.0; points.len()]);
    }
    let positions = points
        .iter()
        .map(|point| {
            (((point[0] - start[0]) * axis[0] + (point[1] - start[1]) * axis[1]) / length_sq)
                .clamp(0.0, 1.0)
        })
        .collect();
    (start, axis, length_sq, monotonic_stop_positions(positions))
}

fn oval_gradient_geometry(points: &[[f32; 2]], size: [f32; 2]) -> ([f32; 2], f32, Vec<f32>) {
    let center = points[0];
    let width = size[0].max(1.0);
    let height = size[1].max(1.0);
    let radii = points
        .iter()
        .map(|point| {
            let dx = (point[0] - center[0]) / width;
            let dy = (point[1] - center[1]) / height;
            (dx * dx + dy * dy).sqrt()
        })
        .collect::<Vec<_>>();
    let outer = radii.last().copied().unwrap_or(0.0).max(1e-6);
    let positions = monotonic_stop_positions(
        radii
            .into_iter()
            .map(|radius| (radius / outer).clamp(0.0, 1.0))
            .collect(),
    );
    (center, outer, positions)
}

#[no_mangle]
pub extern "C" fn generate_gradient(
    _params_ptr: i32,
    _params_len: i32,
    width: i32,
    height: i32,
    _time: f64,
) -> i32 {
    let width = width.max(1);
    let height = height.max(1);
    let (mut pixels, pointer) = frame(width, height);
    let mut points = get_parameter("points", Vec::<[f32; 2]>::new());
    if points.is_empty() {
        return export_frame(pixels, pointer);
    }

    let scale = generator::render_scale().max(0.000_001);
    for point in &mut points {
        point[0] *= scale;
        point[1] *= scale;
    }
    let color_values = get_parameter("colors", Vec::<f32>::new());
    let midpoints = get_parameter("midpoints", Vec::<f32>::new());
    let mut colors = Vec::with_capacity(points.len());
    let mut fallback = default_gradient_color(0);
    for index in 0..points.len() {
        fallback = read_gradient_color(&color_values, index, fallback);
        colors.push(fallback);
    }

    if points.len() == 1 {
        let color = premultiply_linear(colors[0]);
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.copy_from_slice(&color);
        }
        return export_frame(pixels, pointer);
    }

    let kind = get_parameter("kind", 0u32);
    if kind == 1 {
        let (center, outer, positions) =
            oval_gradient_geometry(&points, [width as f32, height as f32]);
        let lut = build_gradient_lut(&positions, &midpoints, &colors);
        let width_norm = width as f32;
        let height_norm = height as f32;
        for y in 0..height as usize {
            let dy = (y as f32 + 0.5 - center[1]) / height_norm;
            let dy_sq = dy * dy;
            for x in 0..width as usize {
                let dx = (x as f32 + 0.5 - center[0]) / width_norm;
                let radius = (dx * dx + dy_sq).sqrt();
                let color = lut[lut_index(radius / outer)];
                pixels[(y * width as usize + x) * 4..(y * width as usize + x) * 4 + 4]
                    .copy_from_slice(&color);
            }
        }
    } else {
        let (start, axis, length_sq, positions) = linear_gradient_geometry(&points);
        let lut = build_gradient_lut(&positions, &midpoints, &colors);
        if length_sq <= 1e-6 {
            let color = lut[0];
            for pixel in pixels.chunks_exact_mut(4) {
                pixel.copy_from_slice(&color);
            }
        } else {
            for y in 0..height as usize {
                let py = y as f32 + 0.5 - start[1];
                let row_base = py * axis[1];
                for x in 0..width as usize {
                    let px = x as f32 + 0.5 - start[0];
                    let t = (px * axis[0] + row_base) / length_sq;
                    let color = lut[lut_index(t)];
                    let offset = (y * width as usize + x) * 4;
                    pixels[offset..offset + 4].copy_from_slice(&color);
                }
            }
        }
    }
    export_frame(pixels, pointer)
}

fn fill_rect(
    pixels: &mut [f32],
    stride: usize,
    x0: usize,
    x1: usize,
    y0: usize,
    y1: usize,
    color: [f32; 4],
) {
    for y in y0..y1 {
        let row = &mut pixels[(y * stride + x0) * 4..(y * stride + x1) * 4];
        for pixel in row.chunks_exact_mut(4) {
            pixel.copy_from_slice(&color);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn fill_centered_rect(
    pixels: &mut [f32],
    stride: usize,
    height: usize,
    center_x: f32,
    center_y: f32,
    half_width: f32,
    half_height: f32,
    color: [f32; 4],
) {
    let x0 = (center_x - half_width).floor().max(0.0) as usize;
    let x1 = (center_x + half_width).ceil().min(stride as f32) as usize;
    let y0 = (center_y - half_height).floor().max(0.0) as usize;
    let y1 = (center_y + half_height).ceil().min(height as f32) as usize;
    if x0 < x1 && y0 < y1 {
        fill_rect(pixels, stride, x0, x1, y0, y1, color);
    }
}

#[allow(clippy::too_many_arguments)]
fn fill_ellipse(
    pixels: &mut [f32],
    stride: usize,
    height: usize,
    center_x: f32,
    center_y: f32,
    radius_x: f32,
    radius_y: f32,
    color: [f32; 4],
) {
    let y0 = (center_y - radius_y).floor().max(0.0) as usize;
    let y1 = (center_y + radius_y).ceil().min(height as f32) as usize;
    for y in y0..y1 {
        let dy = (y as f32 + 0.5 - center_y) / radius_y;
        let span = (1.0 - dy * dy).max(0.0).sqrt() * radius_x;
        let x0 = (center_x - span).floor().max(0.0) as usize;
        let x1 = (center_x + span).ceil().min(stride as f32) as usize;
        let row = &mut pixels[(y * stride + x0) * 4..(y * stride + x1) * 4];
        for pixel in row.chunks_exact_mut(4) {
            pixel.copy_from_slice(&color);
        }
    }
}

#[no_mangle]
pub extern "C" fn generate_text(
    _params_ptr: i32,
    _params_len: i32,
    width: i32,
    height: i32,
    _time: f64,
) -> i32 {
    let (mut pixels, pointer) = frame(width, height);
    
    
    let _ = generator::render_text_rgba32f(&mut pixels, width, height);
    export_frame(pixels, pointer)
}

#[no_mangle]
pub extern "C" fn generate_polygon(
    _params_ptr: i32,
    _params_len: i32,
    width: i32,
    height: i32,
    _time: f64,
) -> i32 {
    let width = width.max(1);
    let height = height.max(1);
    let (mut pixels, pointer) = frame(width, height);
    let mut points = get_parameter("points", Vec::<[f32; 2]>::new());
    if points.len() < 3 {
        return export_frame(pixels, pointer);
    }

    let scale = generator::render_scale().max(0.000_001);
    let border_width_project = get_parameter("border_width", 0.0f32).max(0.0);
    let border_alignment = get_parameter("border_alignment", 0u32);
    let tight = generator::has_tight_bounds();
    let origin = if tight {
        generator::render_origin()
    } else {
        [0.0, 0.0]
    };
    let coordinate_scale = if tight {
        let feather = get_parameter("feather", 1.0f32).max(0.0);
        
        
        let padding = (feather + border_width_project).ceil().max(1.0) + 1.0;
        let min_x = points.iter().map(|p| p[0]).fold(f32::INFINITY, f32::min) - padding;
        let min_y = points.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min) - padding;
        let max_x = points.iter().map(|p| p[0]).fold(f32::NEG_INFINITY, f32::max) + padding;
        let max_y = points.iter().map(|p| p[1]).fold(f32::NEG_INFINITY, f32::max) + padding;
        [
            width as f32 / (max_x - min_x).max(1.0),
            height as f32 / (max_y - min_y).max(1.0),
        ]
    } else {
        [scale, scale]
    };

    for point in &mut points {
        point[0] = (point[0] - origin[0]) * coordinate_scale[0];
        point[1] = (point[1] - origin[1]) * coordinate_scale[1];
    }

    let fill = color();
    let border = color_parameter("border_color", [0.05, 0.05, 0.05, 1.0]);
    let border_width = border_width_project * coordinate_scale[0].min(coordinate_scale[1]);
    let (border_inner, border_outer) = border_extents(border_width, border_alignment);
    let feather = get_parameter("feather", 1.0f32)
        .max(0.5)
        * coordinate_scale[0].min(coordinate_scale[1]);
    let min_x = points
        .iter()
        .map(|point| point[0])
        .fold(f32::INFINITY, f32::min);
    let min_y = points
        .iter()
        .map(|point| point[1])
        .fold(f32::INFINITY, f32::min);
    let max_x = points
        .iter()
        .map(|point| point[0])
        .fold(f32::NEG_INFINITY, f32::max);
    let max_y = points
        .iter()
        .map(|point| point[1])
        .fold(f32::NEG_INFINITY, f32::max);
    let raster_padding = feather + border_outer + 1.0;
    let x0 = (min_x - raster_padding).floor().max(0.0) as usize;
    let y0 = (min_y - raster_padding).floor().max(0.0) as usize;
    let x1 = (max_x + raster_padding).ceil().min(width as f32) as usize;
    let y1 = (max_y + raster_padding).ceil().min(height as f32) as usize;
    let edges = polygon_edges(&points);
    rasterize_polygon(
        &mut pixels,
        width as usize,
        &edges,
        [x0, y0, x1, y1],
        PolygonRasterStyle { feather, border_inner, border_outer, fill, border },
    );
    export_frame(pixels, pointer)
}

#[derive(Clone, Copy)]
struct PolygonEdge {
    start: [f32; 2],
    end: [f32; 2],
    delta: [f32; 2],
    length_sq: f32,
}

fn polygon_edges(points: &[[f32; 2]]) -> Vec<PolygonEdge> {
    let mut edges = Vec::with_capacity(points.len());
    let mut start = *points.last().unwrap_or(&[0.0, 0.0]);
    for &end in points {
        let delta = [end[0] - start[0], end[1] - start[1]];
        edges.push(PolygonEdge {
            start,
            end,
            delta,
            length_sq: delta[0] * delta[0] + delta[1] * delta[1],
        });
        start = end;
    }
    edges
}

#[derive(Clone, Copy)]
struct PolygonRasterStyle {
    feather: f32,
    border_inner: f32,
    border_outer: f32,
    fill: [f32; 4],
    border: [f32; 4],
}

fn rasterize_polygon(
    pixels: &mut [f32],
    stride: usize,
    edges: &[PolygonEdge],
    bounds: [usize; 4],
    style: PolygonRasterStyle,
) {
    let [x0, y0, x1, y1] = bounds;
    let PolygonRasterStyle { feather, border_inner, border_outer, fill: fill_color, border: border_color } = style;
    let mut intersections = Vec::with_capacity(edges.len());

    
    
    for y in y0..y1 {
        let sample_y = y as f32 + 0.5;
        intersections.clear();
        for edge in edges {
            if (edge.start[1] > sample_y) == (edge.end[1] > sample_y) {
                continue;
            }
            let t = (sample_y - edge.start[1]) / edge.delta[1];
            intersections.push(edge.start[0] + edge.delta[0] * t);
        }
        intersections.sort_unstable_by(f32::total_cmp);
        for pair in intersections.chunks_exact(2) {
            let start = (pair[0] - 0.5).ceil().max(x0 as f32) as usize;
            let end = (pair[1] - 0.5).ceil().min(x1 as f32).max(start as f32) as usize;
            for x in start..end {
                pixels[(y * stride + x) * 4 + 3] = 1.0;
            }
        }
    }

    
    
    let band = border_inner.max(border_outer) + feather + 1.0;
    for &edge in edges {
        let edge_x0 = (edge.start[0].min(edge.end[0]) - band)
            .floor()
            .max(x0 as f32) as usize;
        let edge_y0 = (edge.start[1].min(edge.end[1]) - band)
            .floor()
            .max(y0 as f32) as usize;
        let edge_x1 = (edge.start[0].max(edge.end[0]) + band)
            .ceil()
            .min(x1 as f32) as usize;
        let edge_y1 = (edge.start[1].max(edge.end[1]) + band)
            .ceil()
            .min(y1 as f32) as usize;
        for y in edge_y0..edge_y1 {
            for x in edge_x0..edge_x1 {
                let index = (y * stride + x) * 4;
                let encoded = distance_sq_to_segment(
                    [x as f32 + 0.5, y as f32 + 0.5],
                    edge,
                ) + 1.0;
                if pixels[index] == 0.0 || encoded < pixels[index] {
                    pixels[index] = encoded;
                }
            }
        }
    }

    for y in y0..y1 {
        for x in x0..x1 {
            let index = (y * stride + x) * 4;
            let inside = pixels[index + 3] > 0.5;
            let signed = if pixels[index] == 0.0 {
                if inside { f32::NEG_INFINITY } else { f32::INFINITY }
            } else {
                let distance = (pixels[index] - 1.0).max(0.0).sqrt();
                if inside { -distance } else { distance }
            };
            let outer = if signed.is_infinite() {
                inside as u8 as f32
            } else {
                1.0 - smoothstep(-feather, feather, signed - border_outer)
            };
            let fill = if border_inner <= 0.0 && border_outer <= 0.0 {
                outer
            } else if signed.is_infinite() {
                inside as u8 as f32
            } else {
                1.0 - smoothstep(-feather, feather, signed + border_inner)
            };
            let border = (outer - fill).clamp(0.0, 1.0);
            let fill = fill.clamp(0.0, 1.0);
            let fill_alpha = fill_color[3] * fill;
            let border_alpha = border_color[3] * border;
            pixels[index] = fill_color[0] * fill_alpha + border_color[0] * border_alpha;
            pixels[index + 1] = fill_color[1] * fill_alpha + border_color[1] * border_alpha;
            pixels[index + 2] = fill_color[2] * fill_alpha + border_color[2] * border_alpha;
            pixels[index + 3] = (fill_alpha + border_alpha).clamp(0.0, 1.0);
        }
    }
}

fn distance_sq_to_segment(point: [f32; 2], edge: PolygonEdge) -> f32 {
    let ap = [point[0] - edge.start[0], point[1] - edge.start[1]];
    let t = if edge.length_sq <= 1e-6 {
        0.0
    } else {
        ((ap[0] * edge.delta[0] + ap[1] * edge.delta[1]) / edge.length_sq).clamp(0.0, 1.0)
    };
    let dx = point[0] - (edge.start[0] + edge.delta[0] * t);
    let dy = point[1] - (edge.start[1] + edge.delta[1] * t);
    dx * dx + dy * dy
}

fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    let t = ((value - edge0) / (edge1 - edge0).max(1e-6)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
