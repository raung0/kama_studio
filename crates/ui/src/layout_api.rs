use taffy::{
    AlignItems, Dimension, Display, FlexDirection, FlexWrap, JustifyContent, LengthPercentage,
    LengthPercentageAuto, Overflow, Point, Rect as Edges, Size as Extent, Style,
};

use crate::{Align, BlockBuilder, Direction, Layer, ScrollState, Size};

impl BlockBuilder<'_> {
    /// Replaces layout properties with native Taffy semantics, including positioning and overflow.
    #[must_use]
    pub fn layout(self, style: Style) -> Self {
        self.block.layout = style;
        self.block.sizing = [None; 2];
        self.block.collapse = [None; 2];
        self.block.position = None;
        self.block.centered = false;
        self.block.legacy_absolute = false;
        self.block.clip_children = false;
        self
    }

    #[must_use]
    pub const fn width(self, size: Size) -> Self {
        self.legacy_size(0, size)
    }

    #[must_use]
    pub const fn height(self, size: Size) -> Self {
        self.legacy_size(1, size)
    }

    const fn legacy_size(self, axis: usize, size: Size) -> Self {
        let (size, collapse) = match size {
            Size::FitScale(scale) => (Size::Fit, Some(normalized_scale(scale))),
            size => (size, None),
        };
        let dimension = match size {
            Size::Pixels(value) => Dimension::length(value.max(0.0)),
            _ => Dimension::auto(),
        };
        if axis == 0 {
            self.block.layout.size.width = dimension;
            self.block.sizing[0] = Some(size);
            self.block.collapse[0] = collapse;
        } else {
            self.block.layout.size.height = dimension;
            self.block.sizing[1] = Some(size);
            self.block.collapse[1] = collapse;
        }
        self
    }

    #[must_use]
    pub const fn size(self, size: Extent<Dimension>) -> Self {
        self.block.layout.size = size;
        self.block.sizing = [None; 2];
        self.block.collapse = [None; 2];
        self
    }

    #[must_use]
    pub const fn min_size(self, size: Extent<LengthPercentageAuto>) -> Self {
        self.block.layout.min_size = size;
        self
    }

    #[must_use]
    pub const fn max_size(self, size: Extent<LengthPercentageAuto>) -> Self {
        self.block.layout.max_size = size;
        self
    }

    #[must_use]
    pub const fn aspect_ratio(self, ratio: f32) -> Self {
        self.block.layout.aspect_ratio = Some(ratio);
        self
    }

    #[must_use]
    pub const fn display(self, display: Display) -> Self {
        self.block.layout.display = display;
        self
    }

    #[must_use]
    pub const fn direction(self, direction: Direction) -> Self {
        self.flex_direction(match direction {
            Direction::Row => FlexDirection::Row,
            Direction::Column => FlexDirection::Column,
        })
    }

    #[must_use]
    pub const fn flex_direction(self, direction: FlexDirection) -> Self {
        self.block.layout.flex_direction = direction;
        self
    }

    #[must_use]
    pub const fn flex_wrap(self, wrap: FlexWrap) -> Self {
        self.block.layout.flex_wrap = wrap;
        self
    }

    #[must_use]
    pub const fn flex_grow(self, grow: f32) -> Self {
        self.block.layout.flex_grow = grow.max(0.0);
        self
    }

    #[must_use]
    pub const fn flex_shrink(self, shrink: f32) -> Self {
        self.block.layout.flex_shrink = shrink.max(0.0);
        self
    }

    #[must_use]
    pub const fn flex_basis(self, basis: Dimension) -> Self {
        self.block.layout.flex_basis = basis;
        self
    }

    #[must_use]
    pub const fn row(self) -> Self {
        self.display(Display::Flex)
            .flex_direction(FlexDirection::Row)
    }

    #[must_use]
    pub const fn column(self) -> Self {
        self.display(Display::Flex)
            .flex_direction(FlexDirection::Column)
    }

    #[must_use]
    pub const fn grid(self) -> Self {
        self.display(Display::Grid)
    }

    #[must_use]
    pub const fn row_if(self, enabled: bool) -> Self {
        if enabled { self.row() } else { self }
    }

    #[must_use]
    pub const fn column_if(self, enabled: bool) -> Self {
        if enabled { self.column() } else { self }
    }

    #[must_use]
    pub fn padding(self, padding: f32) -> Self {
        self.padding_edges(Edges::length(padding.max(0.0)))
    }

    #[must_use]
    pub const fn padding_edges(self, padding: Edges<LengthPercentage>) -> Self {
        self.block.layout.padding = padding;
        self
    }

    #[must_use]
    pub const fn margin(self, margin: Edges<LengthPercentageAuto>) -> Self {
        self.block.layout.margin = margin;
        self
    }

    #[must_use]
    pub fn gap(self, gap: f32) -> Self {
        self.gap_xy(Extent::length(gap.max(0.0)))
    }

    #[must_use]
    pub const fn gap_xy(self, gap: Extent<LengthPercentage>) -> Self {
        self.block.layout.gap = gap;
        self
    }

    #[must_use]
    pub const fn align_items(self, align: Align) -> Self {
        self.block.layout.align_items = match align {
            Align::Start => AlignItems::START,
            Align::Center => AlignItems::SAFE_CENTER,
            Align::End => AlignItems::SAFE_END,
        };
        self
    }

    #[must_use]
    pub const fn align_self(self, align: AlignItems) -> Self {
        self.block.layout.align_self = Some(align);
        self
    }

    #[must_use]
    pub const fn justify_content(self, align: Align) -> Self {
        self.block.layout.justify_content = match align {
            Align::Start => JustifyContent::START,
            Align::Center => JustifyContent::SAFE_CENTER,
            Align::End => JustifyContent::SAFE_END,
        };
        self
    }

    #[must_use]
    pub const fn content_centered(self) -> Self {
        self.align_items(Align::Center)
            .justify_content(Align::Center)
    }

    #[must_use]
    pub const fn content_centered_if(self, enabled: bool) -> Self {
        if enabled {
            self.content_centered()
        } else {
            self
        }
    }

    #[must_use]
    pub fn border(self, pixels: u32) -> Self {
        self.block.layout.border = Edges::length(pixels as f32);
        self.block.border_width = pixels as f32;
        self
    }

    /// Changes paint order without taking the block out of layout flow.
    #[must_use]
    pub const fn layer(self, layer: Layer) -> Self {
        self.block.layer = layer;
        self
    }

    #[must_use]
    pub const fn collapse_x(self, scale: f32) -> Self {
        self.block.collapse[0] = Some(normalized_scale(scale));
        self
    }

    #[must_use]
    pub const fn collapse_y(self, scale: f32) -> Self {
        self.block.collapse[1] = Some(normalized_scale(scale));
        self
    }

    #[must_use]
    pub const fn clip_children(self, enabled: bool) -> Self {
        self.block.clip_children = enabled;
        self.block.layout.overflow = Point {
            x: if enabled {
                Overflow::Hidden
            } else {
                Overflow::Visible
            },
            y: if enabled {
                Overflow::Hidden
            } else {
                Overflow::Visible
            },
        };
        self
    }

    #[must_use]
    pub const fn overflow_visible(self) -> Self {
        self.clip_children(false)
    }

    #[must_use]
    pub const fn overflow_visible_if(self, enabled: bool) -> Self {
        if enabled {
            self.overflow_visible()
        } else {
            self
        }
    }

    #[must_use]
    pub const fn horizontal_scroll(self, state: ScrollState) -> Self {
        self.block.horizontal_scroll = Some(state);
        self.block.layout.overflow.x = Overflow::Scroll;
        self
    }

    #[must_use]
    pub const fn vertical_scroll(self, state: ScrollState) -> Self {
        self.block.vertical_scroll = Some(state);
        self.block.layout.overflow.y = Overflow::Scroll;
        self
    }
}

const fn normalized_scale(scale: f32) -> f32 {
    if scale.is_finite() {
        scale.max(0.0)
    } else {
        0.0
    }
}
