mod host {
    #[cfg_attr(target_arch = "wasm32", link(wasm_import_module = "kama"))]
    unsafe extern "C" {
        pub fn param_f32(hash: i64, component: i32, fallback: f32) -> f32;
        pub fn param_u32(hash: i64, fallback: i32) -> i32;
        pub fn param_f32_list_len(hash: i64) -> i32;
        pub fn param_f32_list_get(hash: i64, index: i32, fallback: f32) -> f32;
        pub fn param_vec2_array_len(hash: i64) -> i32;
        pub fn param_vec2_array_get(hash: i64, index: i32, component: i32, fallback: f32) -> f32;
        pub fn render_scale() -> f32;
        pub fn render_origin(component: i32) -> f32;
        pub fn render_tight_bounds() -> i32;
        pub fn render_text_rgba32f(output_ptr: i32, width: i32, height: i32) -> i32;
    }
}

pub fn get_parameter<T: Parameter>(name: &str, fallback: T) -> T {
    T::get(parameter_hash(name), fallback)
}

pub fn parameter_id(name: &str) -> u32 {
    parameter_hash(name) as u32
}

mod private {
    pub trait Sealed {}
}

pub trait Parameter: private::Sealed + Sized {
    #[doc(hidden)]
    fn get(hash: i64, fallback: Self) -> Self;
}

impl private::Sealed for f32 {}
impl Parameter for f32 {
    fn get(hash: i64, fallback: Self) -> Self {
        unsafe { host::param_f32(hash, 0, fallback) }
    }
}

impl private::Sealed for u32 {}
impl Parameter for u32 {
    fn get(hash: i64, fallback: Self) -> Self {
        unsafe { host::param_u32(hash, fallback.min(i32::MAX as u32) as i32) as u32 }
    }
}

impl private::Sealed for bool {}
impl Parameter for bool {
    fn get(hash: i64, fallback: Self) -> Self {
        unsafe { host::param_u32(hash, i32::from(fallback)) != 0 }
    }
}

macro_rules! impl_vector_parameter {
    ($length:expr) => {
        impl private::Sealed for [f32; $length] {}
        impl Parameter for [f32; $length] {
            fn get(hash: i64, fallback: Self) -> Self {
                std::array::from_fn(|component| unsafe {
                    host::param_f32(hash, component as i32, fallback[component])
                })
            }
        }
    };
}

impl_vector_parameter!(2);
impl_vector_parameter!(3);
impl_vector_parameter!(4);

impl private::Sealed for Vec<f32> {}
impl Parameter for Vec<f32> {
    fn get(hash: i64, fallback: Self) -> Self {
        let len = unsafe { host::param_f32_list_len(hash) };
        if len <= 0 {
            return fallback;
        }
        (0..len)
            .map(|index| unsafe {
                host::param_f32_list_get(
                    hash,
                    index,
                    fallback.get(index as usize).copied().unwrap_or(0.0),
                )
            })
            .collect()
    }
}

impl private::Sealed for Vec<[f32; 2]> {}
impl Parameter for Vec<[f32; 2]> {
    fn get(hash: i64, fallback: Self) -> Self {
        let len = unsafe { host::param_vec2_array_len(hash) };
        if len <= 0 {
            return fallback;
        }
        (0..len)
            .map(|index| {
                let point_fallback = fallback.get(index as usize).copied().unwrap_or([0.0; 2]);
                std::array::from_fn(|component| unsafe {
                    host::param_vec2_array_get(
                        hash,
                        index,
                        component as i32,
                        point_fallback[component],
                    )
                })
            })
            .collect()
    }
}

pub mod generator {
    use super::host;

    pub fn render_scale() -> f32 {
        unsafe { host::render_scale() }
    }

    pub fn render_origin() -> [f32; 2] {
        unsafe { [host::render_origin(0), host::render_origin(1)] }
    }

    pub fn has_tight_bounds() -> bool {
        unsafe { host::render_tight_bounds() != 0 }
    }

    pub fn render_text_rgba32f(pixels: &mut [f32], width: i32, height: i32) -> Result<(), i32> {
        let required = (width.max(0) as usize)
            .saturating_mul(height.max(0) as usize)
            .saturating_mul(4);
        if pixels.len() < required {
            return Err(-1);
        }
        let status = unsafe {
            host::render_text_rgba32f(pixels.as_mut_ptr() as usize as i32, width, height)
        };
        if status == 0 {
            Ok(())
        } else {
            Err(status)
        }
    }
}

pub mod audio {
    pub const RESET: i32 = 1;

    pub fn reset_requested(flags: i32) -> bool {
        flags & RESET != 0
    }
}

fn parameter_hash(name: &str) -> i64 {
    let mut hash = 0x811c9dc5u32;
    for byte in name.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x01000193);
    }
    i64::from(hash)
}

#[cfg(test)]
mod tests {
    use super::parameter_hash;

    #[test]
    fn hashes_match_the_host_abi() {
        assert_eq!(parameter_hash("color"), 0x3d7e6258);
        assert_eq!(parameter_hash("border_color"), 0x94c31acf);
        assert_eq!(parameter_hash("gain_db"), 0x046c1bf5);
    }
}
