use std::hash::Hash;

use kama_ui::{Color, Rect};

use crate::theme;

fn scrim() -> Color {
    Color::rgba8(0x00, 0x00, 0x00, 0x38)
}

pub(crate) fn build_shell<K1: Hash, K2: Hash, F: FnOnce(&mut kama_ui::BuildCtx)>(
    ctx: &mut kama_ui::BuildCtx,
    scrim_key: K1,
    panel_key: K2,
    viewport: Rect,
    panel: Rect,
    opacity: f32,
    children: F,
) {
    kama_ui::ui!(ctx, {
        Rect(scrim_key, viewport) {
            overlay;
            opacity: opacity;
            fill: scrim();
            interactive_no_reveal;
            animate_interaction: false;
        }
        Rect(panel_key, panel) {
            overlay;
            opacity: opacity;
            backdrop_blur: 28.0;
            backdrop_tint: theme::popup_tint();
            fill: theme::floating_bg();
            border: 1;
            border_color: theme::accent();
            border_radius: 10.0;
            children: children;
        }
    });
}
