use std::fmt::Display;

use crate::{BuildCtx, FormatKey, Rect};

use super::Style;

pub struct Label;

impl Label {
    pub fn build(ctx: &mut BuildCtx, id: impl Display, rect: Rect, text: &str, style: Style) {
        crate::ui!(ctx, {
            Rect(FormatKey::new(format_args!("label {id}")), rect) {
                font_size: 11.0 * style.text_scale; text_color: style.text; text: text;
            }
        });
    }
}
