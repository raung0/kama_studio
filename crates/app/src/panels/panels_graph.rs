use kama_ui::{Rect, Size};

use super::{GRAPH_CARD_W, GRAPH_TOOLBAR_H};
use crate::plugin::{InputType, PluginInput};

#[derive(Clone, Copy)]
pub(super) struct GraphToolbarLayout {
    pub(super) bar: Rect,
    pub(super) combo: Rect,
}

pub(super) fn graph_toolbar_layout(rect: Rect) -> GraphToolbarLayout {
    let bar = crate::ui_layout::column(
        rect,
        &[
            crate::ui_layout::Item::height(GRAPH_TOOLBAR_H),
            crate::ui_layout::Item::fill(),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
        None,
    )[0];
    let combo_width = 360.0f32.min((bar.width - 16.0).max(1.0));
    let parts = crate::ui_layout::row(
        bar,
        &[
            crate::ui_layout::Item::fill(),
            crate::ui_layout::Item::new(Size::Pixels(combo_width), Size::Pixels(25.0)),
            crate::ui_layout::Item::fill(),
        ],
        0.0,
        0.0,
        kama_ui::Align::Center,
    );
    GraphToolbarLayout {
        bar,
        combo: parts[1],
    }
}

pub(super) fn graph_canvas_rect(rect: Rect) -> Rect {
    rect
}

pub(super) fn graph_selection_rect(a: [f32; 2], b: [f32; 2]) -> Rect {
    let left = a[0].min(b[0]);
    let top = a[1].min(b[1]);
    Rect::new(left, top, (a[0] - b[0]).abs(), (a[1] - b[1]).abs())
}

pub(super) fn graph_rects_intersect(a: Rect, b: Rect) -> bool {
    a.x <= b.right() && a.right() >= b.x && a.y <= b.bottom() && a.bottom() >= b.y
}

pub(super) fn graph_screen_to_world(
    canvas: Rect,
    pan: [f32; 2],
    zoom: f32,
    point: [f32; 2],
) -> [f32; 2] {
    let zoom = zoom.max(0.000_1);
    [
        (point[0] - canvas.x - pan[0]) / zoom,
        (point[1] - canvas.y - pan[1]) / zoom,
    ]
}

pub(super) fn graph_card_scale(card: Rect) -> f32 {
    (card.width / GRAPH_CARD_W).max(0.000_1)
}

pub(super) fn plugin_input_uses_host_binding(input: &PluginInput) -> bool {
    matches!(
        input.ty,
        InputType::Text | InputType::Vec2Array | InputType::F32List
    )
}
