use std::fmt::Display;

use crate::{BuildCtx, FormatKey, Rect};

use super::Style;

pub struct ToggleButton;

impl ToggleButton {
    pub fn build(
        ctx: &mut BuildCtx,
        id: impl Display,
        rect: Rect,
        text: &str,
        active: bool,
        style: Style,
    ) {
        crate::ui!(ctx, {
            Rect(FormatKey::new(format_args!("toggle-button {id}")), rect) {
                fill: if active { style.accent } else { style.control }; animate_fill; border: 1;
                border_color: if active { style.accent } else { style.border }; border_radius: style.radius_md;
                font_size: 11.0 * style.text_scale; text_color: if active { style.accent_text } else { style.text };
                text_centered; text: text; interactive;
            }
        });
    }
}
