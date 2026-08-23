use std::fmt::Display;

use crate::{BuildCtx, FormatKey, Rect};

use super::Style;

pub struct Button;

impl Button {
    pub fn build(ctx: &mut BuildCtx, id: impl Display, rect: Rect, text: &str, style: Style) {
        Self::build_filled(ctx, id, rect, text, style.control, style);
    }

    pub fn build_filled(
        ctx: &mut BuildCtx,
        id: impl Display,
        rect: Rect,
        text: &str,
        fill: crate::Color,
        style: Style,
    ) {
        crate::ui!(ctx, {
            Rect(FormatKey::new(format_args!("button {id}")), rect) {
                fill: fill; border: 1; border_color: style.border; border_radius: style.radius_md;
                font_size: 11.0 * style.text_scale; text_color: style.text; text_centered; text: text; interactive;
            }
        });
    }
}
