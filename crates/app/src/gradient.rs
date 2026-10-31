#[expect(
    clippy::arithmetic_side_effects,
    reason = "the fixed palette has eight elements, so its length is a nonzero divisor"
)]
#[expect(
    clippy::indexing_slicing,
    reason = "reducing the index modulo the nonempty palette length keeps it in bounds"
)]
pub const fn default_color(index: usize) -> [f32; 4] {
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

pub fn colors_from_values(values: &[f32], count: usize) -> Vec<[f32; 4]> {
    let mut colors = Vec::with_capacity(count);
    let mut fallback = default_color(0);
    let mut chunks = values.as_chunks::<4>().0.iter();
    for _ in 0..count {
        if let Some(&[red, green, blue, alpha]) = chunks.next() {
            fallback = [red, green, blue, alpha.clamp(0.0, 1.0)];
        }
        colors.push(fallback);
    }
    colors
}

pub fn colors_to_values(colors: &[[f32; 4]]) -> Vec<f32> {
    colors.iter().flatten().copied().collect()
}

#[expect(
    clippy::float_cmp,
    reason = "gradient editing detects any exact coordinate change, including sub-epsilon adjustments"
)]
pub fn changed_point_index(old: &[[f32; 2]], new: &[[f32; 2]]) -> usize {
    old.iter()
        .zip(new)
        .position(|(old, new)| old != new)
        .unwrap_or_else(|| old.len().min(new.len()))
}

pub fn inserted_color(colors: &[[f32; 4]], index: usize) -> [f32; 4] {
    let index = index.min(colors.len());
    match (
        index.checked_sub(1).and_then(|left| colors.get(left)),
        colors.get(index),
    ) {
        (Some(left), Some(right)) => [
            f32::midpoint(left[0], right[0]),
            f32::midpoint(left[1], right[1]),
            f32::midpoint(left[2], right[2]),
            f32::midpoint(left[3], right[3]),
        ],
        (Some(left), None) => *left,
        (None, Some(right)) => *right,
        (None, None) => default_color(0),
    }
}

pub fn normalized_midpoints(values: &[f32], point_count: usize) -> Vec<f32> {
    (0..point_count.saturating_sub(1))
        .map(|index| values.get(index).copied().unwrap_or(0.5).clamp(0.01, 0.99))
        .collect()
}

pub fn insert_midpoint(midpoints: &mut Vec<f32>, index: usize, old_point_count: usize) {
    if old_point_count == 0 {
        return;
    }
    if index == 0 {
        midpoints.insert(0, 0.5);
    } else if index >= old_point_count {
        midpoints.push(0.5);
    } else {
        if let Some(left) = midpoints.get_mut(index.saturating_sub(1)) {
            *left = 0.5;
        }
        midpoints.insert(index.min(midpoints.len()), 0.5);
    }
    midpoints.resize(old_point_count, 0.5);
}

pub fn remove_midpoint(midpoints: &mut Vec<f32>, index: usize, old_point_count: usize) {
    if old_point_count <= 1 || midpoints.is_empty() {
        midpoints.clear();
        return;
    }
    if index == 0 {
        midpoints.remove(0);
    } else if index >= old_point_count.saturating_sub(1) {
        midpoints.pop();
    } else {
        if index < midpoints.len() {
            midpoints.remove(index);
        }
        if let Some(merged) = midpoints.get_mut(index.saturating_sub(1)) {
            *merged = 0.5;
        }
    }
    midpoints.resize(old_point_count.saturating_sub(2), 0.5);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_colors_extend_the_last_complete_stop() {
        let colors = colors_from_values(&[0.1, 0.2, 0.3, 1.5], 3);
        assert_eq!(colors, vec![[0.1, 0.2, 0.3, 1.0]; 3]);
    }

    #[test]
    fn inserted_color_interpolates_only_between_neighbors() {
        let colors = [[0.0, 0.2, 0.4, 0.6], [1.0, 0.8, 0.6, 0.4]];
        assert_eq!(inserted_color(&colors, 1), [0.5, 0.5, 0.5, 0.5]);
        assert_eq!(inserted_color(&colors, 2), colors[1]);
    }

    #[test]
    fn insertion_beyond_the_last_stop_uses_its_color() {
        let colors = [[0.2, 0.3, 0.7, 0.9], [0.8, 0.6, 0.4, 0.5]];
        assert_eq!(inserted_color(&colors, usize::MAX), [0.8, 0.6, 0.4, 0.5]);
    }

    #[test]
    fn midpoint_insert_and_remove_keep_segment_count_in_sync() {
        let mut midpoints = vec![0.2, 0.8];
        insert_midpoint(&mut midpoints, 1, 3);
        assert_eq!(midpoints, vec![0.5, 0.5, 0.8]);
        remove_midpoint(&mut midpoints, 1, 4);
        assert_eq!(midpoints, vec![0.5, 0.8]);
    }
}
