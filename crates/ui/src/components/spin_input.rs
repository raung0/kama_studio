use std::fmt::Display;

use crate::{BuildCtx, FormatKey, Rect};

use super::Style;



pub struct SpinInput;

impl SpinInput {
    pub fn build(ctx: &mut BuildCtx, id: impl Display, rect: Rect, value: &str, style: Style) {
        crate::ui!(ctx, {
            Rect(FormatKey::new(format_args!("spin {id}")), rect) {
                fill: style.control; border: 1; border_color: style.border; border_radius: style.radius_md;
                font_size: 10.0 * style.text_scale; text_color: style.text; text_centered; text: value; interactive;
            }
        });
    }
}
