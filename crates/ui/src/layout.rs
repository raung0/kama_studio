use crate::{Align, Rect, ScrollState, Size};

#[derive(Clone, Copy, Debug)]
pub struct Item {
    pub width: Size,
    pub height: Size,
}

impl Item {
    #[must_use]
    pub const fn new(width: Size, height: Size) -> Self {
        Self { width, height }
    }

    #[must_use]
    pub const fn fill() -> Self {
        Self::new(Size::Fill, Size::Fill)
    }

    #[must_use]
    pub const fn height(height: f32) -> Self {
        Self::new(Size::Fill, Size::Pixels(height))
    }

    #[must_use]
    pub const fn width(width: f32) -> Self {
        Self::new(Size::Pixels(width), Size::Fill)
    }

    #[must_use]
    pub const fn fill_portion(portion: f32) -> Self {
        Self::new(Size::FillPortion(portion), Size::Fill)
    }
}

#[must_use]
#[expect(
    clippy::expect_used,
    reason = "the centered block is created in this measured layout"
)]
/// # Panics
/// Panics if the layout engine fails to return the block it measured.
pub fn centered(viewport: Rect, width: f32, height: f32) -> Rect {
    let (id, measured) = crate::measure_layout(viewport, |ctx| {
        ctx.new()
            .overlay()
            .centered()
            .width(Size::Pixels(width))
            .height(Size::Pixels(height))
            .build()
    });
    measured.rect(id).expect("centered layout")
}

#[must_use]
pub fn inset(rect: Rect, padding: f32) -> Rect {
    let [result] = column_array(rect, &[Item::fill()], 0.0, padding, Align::Start, None);
    result
}

#[must_use]
pub fn row(rect: Rect, items: &[Item], gap: f32, padding: f32, align: Align) -> Vec<Rect> {
    flow(rect, items, true, gap, padding, align, None).1
}

#[must_use]
pub fn row_array<const N: usize>(
    rect: Rect,
    items: &[Item; N],
    gap: f32,
    padding: f32,
    align: Align,
) -> [Rect; N] {
    layout_array(row(rect, items, gap, padding, align))
}

#[must_use]
pub fn row_scrolled(
    rect: Rect,
    items: &[Item],
    gap: f32,
    padding: f32,
    align: Align,
    scroll: ScrollState,
) -> Vec<Rect> {
    flow(rect, items, true, gap, padding, align, Some(scroll)).1
}

#[must_use]
pub fn column(
    rect: Rect,
    items: &[Item],
    gap: f32,
    padding: f32,
    align: Align,
    scroll: Option<ScrollState>,
) -> Vec<Rect> {
    flow(rect, items, false, gap, padding, align, scroll).1
}

#[must_use]
pub fn column_array<const N: usize>(
    rect: Rect,
    items: &[Item; N],
    gap: f32,
    padding: f32,
    align: Align,
    scroll: Option<ScrollState>,
) -> [Rect; N] {
    layout_array(column(rect, items, gap, padding, align, scroll))
}

#[must_use]
pub fn scrolled_content(rect: Rect, offset: f32) -> Rect {
    let [result] = column_array(
        rect,
        &[Item::height(rect.height + offset.max(0.0))],
        0.0,
        0.0,
        Align::Start,
        Some(ScrollState {
            offset: offset.max(0.0),
        }),
    );
    result
}

pub fn stack(rect: Rect, start_y: f32, heights: &[f32]) -> Vec<Rect> {
    fit_column_at(
        rect,
        [rect.x, start_y],
        rect.width,
        &heights
            .iter()
            .copied()
            .map(Item::height)
            .collect::<Vec<_>>(),
        0.0,
        0.0,
    )
    .1
}

#[must_use]
#[expect(
    clippy::expect_used,
    reason = "the root and each child ID are created in this measured layout"
)]
/// # Panics
/// Panics if the layout engine fails to return a block it measured.
pub fn fit_column_at(
    viewport: Rect,
    position: [f32; 2],
    width: f32,
    items: &[Item],
    gap: f32,
    padding: f32,
) -> (Rect, Vec<Rect>) {
    let ((root, ids), measured) = crate::measure_layout(viewport, |ctx| {
        let mut ids = Vec::with_capacity(items.len());
        let root = ctx
            .new()
            .overlay()
            .position((position[0] - viewport.x, position[1] - viewport.y))
            .width(Size::Pixels(width))
            .height(Size::Fit)
            .gap(gap)
            .padding(padding)
            .children(|ctx| {
                for item in items {
                    ids.push(ctx.new().width(item.width).height(item.height).build());
                }
            })
            .build();
        (root, ids)
    });
    (
        measured.rect(root).expect("fit column layout"),
        ids.into_iter()
            .map(|id| measured.rect(id).expect("fit column child layout"))
            .collect(),
    )
}

#[must_use]
pub fn fit_column_array_at<const N: usize>(
    viewport: Rect,
    position: [f32; 2],
    width: f32,
    items: &[Item; N],
    gap: f32,
    padding: f32,
) -> (Rect, [Rect; N]) {
    let (root, children) = fit_column_at(viewport, position, width, items, gap, padding);
    (root, layout_array(children))
}

#[expect(
    clippy::expect_used,
    reason = "layout creates exactly one measured child for each input item"
)]
fn layout_array<const N: usize>(rects: Vec<Rect>) -> [Rect; N] {
    rects.try_into().expect("layout child count changed")
}

#[expect(
    clippy::expect_used,
    reason = "the root and each child ID are created in this measured layout"
)]
fn flow(
    rect: Rect,
    items: &[Item],
    horizontal: bool,
    gap: f32,
    padding: f32,
    align: Align,
    scroll: Option<ScrollState>,
) -> (Rect, Vec<Rect>) {
    let ((root, ids), measured) = crate::measure_layout(rect, |ctx| {
        let mut ids = Vec::with_capacity(items.len());
        let mut root = ctx
            .new()
            .width(Size::Fill)
            .height(Size::Fill)
            .gap(gap)
            .padding(padding)
            .align_items(align);
        root = if horizontal {
            root.row()
        } else {
            root.column()
        };
        if let Some(scroll) = scroll {
            root = if horizontal {
                root.horizontal_scroll(scroll)
            } else {
                root.vertical_scroll(scroll)
            };
        }
        let root = root
            .children(|ctx| {
                for item in items {
                    ids.push(ctx.new().width(item.width).height(item.height).build());
                }
            })
            .build();
        (root, ids)
    });
    (
        measured.rect(root).expect("flow root layout"),
        ids.into_iter()
            .map(|id| measured.rect(id).expect("flow child layout"))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::{Align, Item, Rect, row_array};

    #[test]
    fn row_array_preserves_item_order_and_layout_spacing() {
        let [first, second] = row_array(
            Rect::new(0.0, 0.0, 100.0, 20.0),
            &[Item::width(30.0), Item::fill()],
            10.0,
            5.0,
            Align::Start,
        );

        assert!((first.x - 5.0).abs() < f32::EPSILON);
        assert!((first.width - 30.0).abs() < f32::EPSILON);
        assert!((second.x - 45.0).abs() < f32::EPSILON);
        assert!((second.width - 50.0).abs() < f32::EPSILON);
    }
}
