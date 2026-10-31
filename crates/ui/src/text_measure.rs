use std::collections::HashMap;

use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Wrap};

use crate::FontKind;

const CACHE_LIMIT: usize = 1024;
pub const LINE_HEIGHT: f32 = 1.25;

#[derive(Hash, PartialEq, Eq)]
struct MeasureKey {
    text: String,
    font_size: u32,
    monospace: bool,
    wrap: bool,
    width: Option<u32>,
}

pub struct TextMeasurer {
    font_system: FontSystem,
    cache: HashMap<MeasureKey, [f32; 2]>,
}

impl TextMeasurer {
    pub(super) fn new() -> Self {
        Self {
            font_system: FontSystem::new(),
            cache: HashMap::new(),
        }
    }

    pub(super) fn measure(
        &mut self,
        text: &str,
        font_size: f32,
        font_kind: FontKind,
        wrap: bool,
        width: Option<f32>,
    ) -> [f32; 2] {
        let font_size = if font_size.is_finite() {
            font_size.max(1.0)
        } else {
            1.0
        };
        let width = width
            .filter(|value| value.is_finite())
            .map(|value| value.max(0.0));
        let key = MeasureKey {
            text: text.to_owned(),
            font_size: font_size.to_bits(),
            monospace: font_kind == FontKind::Monospace,
            wrap,
            width: width.map(f32::to_bits),
        };
        if let Some(size) = self.cache.get(&key) {
            return *size;
        }

        if self.cache.len() >= CACHE_LIMIT {
            self.cache.clear();
        }

        let line_height = font_size * LINE_HEIGHT;
        let mut buffer = Buffer::new(&mut self.font_system, Metrics::new(font_size, line_height));
        let size = {
            let mut borrowed = buffer.borrow_with(&mut self.font_system);
            borrowed.set_size(width, None);
            if !wrap {
                borrowed.set_wrap(Wrap::None);
            }
            let attrs = match font_kind {
                FontKind::Sans => Attrs::new(),
                FontKind::Monospace => Attrs::new().family(Family::Monospace),
            };
            borrowed.set_text(text, &attrs, Shaping::Advanced, None);
            borrowed
                .layout_runs()
                .fold([0.0_f32, 0.0_f32], |[width, height], run| {
                    [
                        width.max(run.line_w),
                        height.max(run.line_top + run.line_height),
                    ]
                })
        };
        self.cache.insert(key, size);
        size
    }
}

#[cfg(test)]
mod tests {
    use super::TextMeasurer;
    use crate::FontKind;

    #[test]
    fn proportional_font_measures_wider_glyphs_with_greater_advance() {
        let mut measurer = TextMeasurer::new();
        let wide = measurer.measure("WWWW", 20.0, FontKind::Sans, false, None)[0];
        let narrow = measurer.measure("iiii", 20.0, FontKind::Sans, false, None)[0];

        assert!(wide > narrow);
    }

    #[test]
    fn monospace_glyph_sequences_have_equal_advance() {
        let mut measurer = TextMeasurer::new();
        let wide = measurer.measure("WWWW", 20.0, FontKind::Monospace, false, None)[0];
        let narrow = measurer.measure("iiii", 20.0, FontKind::Monospace, false, None)[0];

        assert!((wide - narrow).abs() < 0.01);
    }

    #[test]
    fn wrapping_with_content_width_increases_text_height() {
        let mut measurer = TextMeasurer::new();
        let unwrapped = measurer.measure(
            "A sentence that should occupy several lines when wrapped.",
            18.0,
            FontKind::Sans,
            false,
            None,
        );
        let wrapped = measurer.measure(
            "A sentence that should occupy several lines when wrapped.",
            18.0,
            FontKind::Sans,
            true,
            Some(55.0),
        );

        assert!(wrapped[1] > unwrapped[1]);
    }
}
