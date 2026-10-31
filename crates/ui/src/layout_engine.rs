use taffy::{
    AvailableSpace, Dimension, FlexDirection, LengthPercentageAuto, NodeId, Position, Style,
    TaffyError, TaffyTree,
    geometry::{Rect as Edges, Size as Extent},
};

use crate::{Align, Block, Direction, Rect, Size, text_measure::TextMeasurer};

struct LayoutNode {
    id: NodeId,
    wrapper: Option<NodeId>,
    children: Vec<Self>,
}

struct Geometry {
    rect: Rect,
    children: Vec<Self>,
}

struct ScaledNode {
    id: NodeId,
    width: Option<f32>,
    height: Option<f32>,
    clamp_width: bool,
    clamp_height: bool,
}

pub fn layout(
    blocks: &mut [Block],
    viewport: Rect,
    text: &mut TextMeasurer,
) -> Result<(), TaffyError> {
    let geometry = compute_geometry(blocks, viewport, text)?;
    for (block, geometry) in blocks.iter_mut().zip(geometry) {
        apply_geometry(
            block,
            geometry,
            [viewport.x, viewport.y],
            &[],
            crate::Layer::Base,
            [viewport.x, viewport.y],
        );
    }
    Ok(())
}

fn compute_geometry(
    blocks: &[Block],
    viewport: Rect,
    text: &mut TextMeasurer,
) -> Result<Vec<Geometry>, TaffyError> {
    let mut tree = TaffyTree::<&Block>::new();
    tree.disable_rounding();
    let mut scaled = Vec::new();
    let nodes = blocks
        .iter()
        .map(|block| build_node(&mut tree, block, Direction::Column, 0.0, &mut scaled))
        .collect::<Result<Vec<_>, _>>()?;
    normalize_fill(&mut tree, blocks, &nodes, Direction::Column)?;
    let children = nodes.iter().map(LayoutNode::outer_id).collect::<Vec<_>>();
    let root = tree.new_with_children(
        Style {
            position: Position::Relative,
            size: Extent {
                width: Dimension::length(viewport.width.max(0.0)),
                height: Dimension::length(viewport.height.max(0.0)),
            },
            flex_direction: FlexDirection::Column,
            align_items: taffy::AlignItems::START,
            ..Style::default()
        },
        &children,
    )?;
    compute(&mut tree, root, viewport, text)?;
    // Resolve inner reveals first so an outer reveal measures their scaled size.
    for scaled in scaled {
        let size = tree.layout(scaled.id)?.size;
        let mut style = tree.style(scaled.id)?.clone();
        if let Some(scale) = scaled.width {
            style.size.width = Dimension::length(size.width * scale);
        }
        if let Some(scale) = scaled.height {
            style.size.height = Dimension::length(size.height * scale);
        }
        if scaled.clamp_width {
            style.max_size.width = LengthPercentageAuto::percent(1.0);
        }
        if scaled.clamp_height {
            style.max_size.height = LengthPercentageAuto::percent(1.0);
        }
        tree.set_style(scaled.id, style)?;
        compute(&mut tree, root, viewport, text)?;
    }
    nodes.iter().map(|node| node.geometry(&tree)).collect()
}

fn compute(
    tree: &mut TaffyTree<&Block>,
    root: NodeId,
    viewport: Rect,
    text: &mut TextMeasurer,
) -> Result<(), TaffyError> {
    tree.compute_layout_with_measure(
        root,
        Extent {
            width: AvailableSpace::Definite(viewport.width.max(0.0)),
            height: AvailableSpace::Definite(viewport.height.max(0.0)),
        },
        |inputs, _, context, style| {
            taffy::compute_leaf_layout(
                inputs,
                style,
                |_, _| 0.0,
                |known, available| {
                    let measured = context.and_then(|block| {
                        block.text.as_deref().map(|value| {
                            let width = known.width.or(match available.width {
                                AvailableSpace::Definite(width) => Some(width),
                                AvailableSpace::MinContent => Some(0.0),
                                AvailableSpace::MaxContent => None,
                            });
                            text.measure(
                                value,
                                block.font_size,
                                block.font_kind,
                                block.text_wrap,
                                width,
                            )
                        })
                    });
                    let [width, height] = measured.unwrap_or([0.0; 2]);
                    Extent {
                        width: known.width.unwrap_or(width),
                        height: known.height.unwrap_or(height),
                    }
                },
            )
        },
    )
}

fn build_node<'a>(
    tree: &mut TaffyTree<&'a Block>,
    block: &'a Block,
    parent_direction: Direction,
    parent_padding: f32,
    scaled: &mut Vec<ScaledNode>,
) -> Result<LayoutNode, TaffyError> {
    let children = block
        .children
        .iter()
        .map(|child| build_node(tree, child, block.direction, block.padding, scaled))
        .collect::<Result<Vec<_>, _>>()?;
    normalize_fill(tree, &block.children, &children, block.direction)?;
    let absolute = !crate::is_flow(block);
    let direction = if absolute {
        Direction::Column
    } else {
        parent_direction
    };
    let style = block_style(block, direction, absolute);
    let id = if children.is_empty() {
        tree.new_leaf_with_context(style, block)?
    } else {
        let ids = children
            .iter()
            .map(LayoutNode::outer_id)
            .collect::<Vec<_>>();
        tree.new_with_children(style, &ids)?
    };
    if matches!(block.width, Size::FitScale(_)) || matches!(block.height, Size::FitScale(_)) {
        scaled.push(ScaledNode {
            id,
            width: scale(block.width),
            height: scale(block.height),
            clamp_width: !absolute
                && parent_direction == Direction::Column
                && scale(block.width).is_some(),
            clamp_height: !absolute
                && parent_direction == Direction::Row
                && scale(block.height).is_some(),
        });
    }
    let wrapper = if absolute {
        let [x, y] = block.position.unwrap_or([0.0; 2]);
        let centered = block.centered && block.position.is_none();
        Some(tree.new_with_children(
            Style {
                position: Position::Absolute,
                // Absolute insets use the padding box; our API positions inside the content box.
                inset: Edges {
                    left: LengthPercentageAuto::length(parent_padding + x),
                    right: LengthPercentageAuto::length(parent_padding - x),
                    top: LengthPercentageAuto::length(parent_padding + y),
                    bottom: LengthPercentageAuto::length(parent_padding - y),
                },
                flex_direction: FlexDirection::Column,
                justify_content: if centered {
                    taffy::JustifyContent::SAFE_CENTER
                } else {
                    taffy::JustifyContent::START
                },
                align_items: if centered {
                    taffy::AlignItems::SAFE_CENTER
                } else {
                    taffy::AlignItems::START
                },
                min_size: Extent::zero(),
                ..Style::default()
            },
            &[id],
        )?)
    } else {
        None
    };
    Ok(LayoutNode {
        id,
        wrapper,
        children,
    })
}

impl LayoutNode {
    fn outer_id(&self) -> NodeId {
        self.wrapper.unwrap_or(self.id)
    }

    fn geometry(&self, tree: &TaffyTree<&Block>) -> Result<Geometry, TaffyError> {
        let layout = tree.layout(self.id)?;
        let offset = self.wrapper.map(|id| tree.layout(id)).transpose()?;
        Ok(Geometry {
            rect: Rect::new(
                layout.location.x + offset.map_or(0.0, |layout| layout.location.x),
                layout.location.y + offset.map_or(0.0, |layout| layout.location.y),
                layout.size.width,
                layout.size.height,
            ),
            children: self
                .children
                .iter()
                .map(|node| node.geometry(tree))
                .collect::<Result<_, _>>()?,
        })
    }
}

fn block_style(block: &Block, parent_direction: Direction, absolute: bool) -> Style {
    let horizontal = parent_direction == Direction::Row;
    let main = if horizontal {
        block.width
    } else {
        block.height
    };
    let cross = if horizontal {
        block.height
    } else {
        block.width
    };
    let mut style = Style {
        position: Position::Relative,
        size: Extent {
            width: dimension(block.width, absolute),
            height: dimension(block.height, absolute),
        },
        min_size: Extent::zero(),
        padding: Edges::length(block.padding),
        border: Edges::length(block.border_width),
        gap: Extent::length(block.gap),
        flex_direction: match block.direction {
            Direction::Row => FlexDirection::Row,
            Direction::Column => FlexDirection::Column,
        },
        align_items: match block.align_items {
            Align::Start => taffy::AlignItems::START,
            Align::Center => taffy::AlignItems::SAFE_CENTER,
            Align::End => taffy::AlignItems::SAFE_END,
        },
        justify_content: match block.justify_content {
            Align::Start => taffy::JustifyContent::START,
            Align::Center => taffy::JustifyContent::SAFE_CENTER,
            Align::End => taffy::JustifyContent::SAFE_END,
        },
        flex_shrink: 0.0,
        ..Style::default()
    };
    if let Some(weight) = fill_weight(main) {
        style.flex_grow = if absolute { 1.0 } else { weight };
        // Unlike a definite zero, 0% retains intrinsic size in a fit-sized parent.
        style.flex_basis = Dimension::percent(0.0);
    }
    if fill_weight(cross).is_some() {
        style.align_self = Some(taffy::AlignItems::STRETCH);
    } else if !absolute && matches!(cross, Size::Fit) {
        if horizontal {
            style.max_size.height = LengthPercentageAuto::percent(1.0);
        } else {
            style.max_size.width = LengthPercentageAuto::percent(1.0);
        }
    }
    style
}

fn normalize_fill(
    tree: &mut TaffyTree<&Block>,
    blocks: &[Block],
    nodes: &[LayoutNode],
    direction: Direction,
) -> Result<(), TaffyError> {
    let weights = blocks
        .iter()
        .filter(|block| crate::is_flow(block))
        .filter_map(|block| {
            fill_weight(if direction == Direction::Row {
                block.width
            } else {
                block.height
            })
        });
    let total: f64 = weights.clone().map(f64::from).sum();
    let count = weights.filter(|weight| *weight > 0.0).count() as f64;
    if total > 0.0 {
        for (_, node) in blocks
            .iter()
            .zip(nodes)
            .filter(|(block, _)| crate::is_flow(block))
        {
            let mut style = tree.style(node.id)?.clone();
            // CSS grow factors below one leave free space; FillPortion always consumes it.
            style.flex_grow = (f64::from(style.flex_grow) / total * count) as f32;
            tree.set_style(node.id, style)?;
        }
    }
    Ok(())
}

const fn dimension(size: Size, absolute: bool) -> Dimension {
    match size {
        Size::Pixels(value) => Dimension::length(value.max(0.0)),
        Size::FitScale(_) => Dimension::max_content(),
        Size::Fit if absolute => Dimension::max_content(),
        _ => Dimension::auto(),
    }
}

const fn fill_weight(size: Size) -> Option<f32> {
    match size {
        Size::Fill => Some(1.0),
        Size::FillPortion(weight) => Some(finite_positive(weight)),
        _ => None,
    }
}

const fn scale(size: Size) -> Option<f32> {
    match size {
        Size::FitScale(value) => Some(finite_positive(value)),
        _ => None,
    }
}

const fn finite_positive(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

fn apply_geometry(
    block: &mut Block,
    geometry: Geometry,
    origin: [f32; 2],
    inherited_clips: &[crate::ClipShape],
    inherited_layer: crate::Layer,
    explicit_clip_origin: [f32; 2],
) {
    let rect = Rect::new(
        origin[0] + geometry.rect.x,
        origin[1] + geometry.rect.y,
        geometry.rect.width,
        geometry.rect.height,
    );
    crate::prepare_block(
        block,
        rect,
        inherited_clips,
        inherited_layer,
        explicit_clip_origin,
    );
    let viewport_content = rect.inset(block.padding + block.border_width);
    let mut content = viewport_content;
    let scroll = [
        block
            .horizontal_scroll
            .map_or(0.0, |scroll| scroll.offset.max(0.0)),
        block
            .vertical_scroll
            .map_or(0.0, |scroll| scroll.offset.max(0.0)),
    ];
    content.x -= scroll[0];
    content.y -= scroll[1];
    for (child, geometry) in block.children.iter_mut().zip(geometry.children) {
        let clips = if !crate::is_flow(child) && child.layer > block.layer {
            &[][..]
        } else {
            block.content_clips.as_slice()
        };
        apply_geometry(
            child,
            geometry,
            [rect.x - scroll[0], rect.y - scroll[1]],
            clips,
            block.layer,
            [content.x, content.y],
        );
    }
    let content_width = block
        .children
        .iter()
        .map(|child| child.rect.right() - content.x)
        .fold(0.0, f32::max);
    let content_height = block
        .children
        .iter()
        .map(|child| child.rect.bottom() - content.y)
        .fold(0.0, f32::max);
    block.scroll_range = crate::ScrollRange {
        horizontal: if block.horizontal_scroll.is_some() {
            (content_width - viewport_content.width).max(0.0)
        } else {
            0.0
        },
        vertical: if block.vertical_scroll.is_some() {
            (content_height - viewport_content.height).max(0.0)
        } else {
            0.0
        },
    };
}
