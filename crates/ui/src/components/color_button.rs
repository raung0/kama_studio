use std::fmt::Display;

use crate::{BuildCtx, Color, FormatKey, Rect};

use super::Style;

pub struct ColorButton;

impl ColorButton {
    pub fn build(ctx: &mut BuildCtx, id: impl Display, rect: Rect, color: Color, style: Style) {
        crate::ui!(ctx, {
            Rect(FormatKey::new(format_args!("color-button {id}")), rect) {
                fill: color; border: 1; border_color: style.border; border_radius: style.radius_md;
                interactive; border_reveal;
            }
        });
    }
}
