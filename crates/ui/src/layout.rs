use crate::{Align, Rect, ScrollState, Size};

#[derive(Clone, Copy, Debug)]
pub struct Item {
    pub width: Size,
    pub height: Size,
}

impl Item {
    pub const fn new(width: Size, height: Size) -> Self {
        Self { width, height }
    }

    pub const fn fill() -> Self {
        Self::new(Size::Fill, Size::Fill)
    }

    pub const fn height(height: f32) -> Self {
        Self::new(Size::Fill, Size::Pixels(height))
    }

    pub const fn width(width: f32) -> Self {
        Self::new(Size::Pixels(width), Size::Fill)
    }

    pub const fn fill_portion(portion: f32) -> Self {
        Self::new(Size::FillPortion(portion), Size::Fill)
    }
}

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

pub fn inset(rect: Rect, padding: f32) -> Rect {
    column(rect, &[Item::fill()], 0.0, padding, Align::Start, None)[0]
}

pub fn row(rect: Rect, items: &[Item], gap: f32, padding: f32, align: Align) -> Vec<Rect> {
    flow(rect, items, true, gap, padding, align, None).1
}

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

pub fn scrolled_content(rect: Rect, offset: f32) -> Rect {
    column(
        rect,
        &[Item::height(rect.height + offset.max(0.0))],
        0.0,
        0.0,
        Align::Start,
        Some(ScrollState {
            offset: offset.max(0.0),
        }),
    )[0]
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
