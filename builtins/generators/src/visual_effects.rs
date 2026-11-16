use kama_plugin::{generator, get_parameter, output_blur, output_color_matrix};

fn identity() -> [[f32; 4]; 5] {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
        [0.0; 4],
    ]
}

#[no_mangle]
pub extern "C" fn scene_rgba(_: i32, _: i32, _: i32, _: i32, _: f64) -> i32 {
    let mut matrix = identity();
    for (row, name) in matrix
        .iter_mut()
        .take(4)
        .zip(["red", "green", "blue", "alpha"])
    {
        let scale = (get_parameter(name, 100.0_f32) / 100.0).clamp(0.0, 1.0);
        for component in row {
            *component *= scale;
        }
    }
    if output_color_matrix(0, matrix) {
        0
    } else {
        -1
    }
}

#[no_mangle]
pub extern "C" fn scene_invert(_: i32, _: i32, _: i32, _: i32, _: f64) -> i32 {
    let mut matrix = identity();
    matrix[0][0] = -1.0;
    matrix[1][1] = -1.0;
    matrix[2][2] = -1.0;
    matrix[4] = [1.0, 1.0, 1.0, 0.0];
    if output_color_matrix(0, matrix) {
        0
    } else {
        -1
    }
}

#[no_mangle]
pub extern "C" fn scene_black_white(_: i32, _: i32, _: i32, _: i32, _: f64) -> i32 {
    let mut matrix = identity();
    let amount = get_parameter("amount", 1.0_f32);
    for (index, row) in matrix.iter_mut().take(3).enumerate() {
        for (component, luma) in row.iter_mut().zip([0.2126, 0.7152, 0.0722]) {
            *component = luma * amount;
        }
        if let Some(diagonal) = row.get_mut(index) {
            *diagonal += 1.0 - amount;
        }
    }
    if output_color_matrix(0, matrix) {
        0
    } else {
        -1
    }
}

#[no_mangle]
pub extern "C" fn scene_blur(_: i32, _: i32, _: i32, _: i32, _: f64) -> i32 {
    let scale = generator::render_scale().max(0.000_001);
    let radius = get_parameter("radius", 4.0_f32 * scale).clamp(0.0, 64.0 * scale);
    if output_blur(0, radius) {
        0
    } else {
        -1
    }
}
