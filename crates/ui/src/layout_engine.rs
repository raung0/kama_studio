use taffy::{
    AvailableSpace, Dimension, Display, FlexDirection, LengthPercentageAuto, NodeId, Position,
    Style, TaffyError, TaffyTree,
    geometry::{Rect as Edges, Size as Extent},
};

use crate::{Block, Rect, Size, text_measure::TextMeasurer};

struct LayoutNode {
    id: NodeId,
    wrapper: Option<NodeId>,
    children: Vec<Self>,
}

struct Geometry {
    rect: Rect,
    position: Position,
    establishes_containing_block: bool,
    content: Rect,
    padding: Rect,
    scroll_range: crate::ScrollRange,
    border_width: f32,
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
            ApplyContext {
                viewport,
                scroll: [0.0; 2],
                containing_scroll: [0.0; 2],
                clips: &[],
                layer: crate::Layer::Base,
                explicit_clip_origin: [viewport.x, viewport.y],
            },
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
        .map(|block| build_node(&mut tree, block, &root_style(viewport), &mut scaled))
        .collect::<Result<Vec<_>, _>>()?;
    normalize_fill(&mut tree, blocks, &nodes, &root_style(viewport))?;
    let children = nodes.iter().map(LayoutNode::outer_id).collect::<Vec<_>>();
    let root = tree.new_with_children(root_style(viewport), &children)?;
    compute(&mut tree, root, viewport, text)?;
    for node in &nodes {
        resolve_wrapper_padding(&mut tree, node, root)?;
    }
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
    nodes
        .iter()
        .map(|node| node.geometry(&tree, [0.0; 2], [0.0; 2]))
        .collect()
}

fn root_style(viewport: Rect) -> Style {
    Style {
        position: Position::Relative,
        size: Extent {
            width: Dimension::length(viewport.width.max(0.0)),
            height: Dimension::length(viewport.height.max(0.0)),
        },
        flex_direction: FlexDirection::Column,
        align_items: taffy::AlignItems::START,
        ..Style::default()
    }
}

fn resolve_wrapper_padding(
    tree: &mut TaffyTree<&Block>,
    node: &LayoutNode,
    parent: NodeId,
) -> Result<(), TaffyError> {
    if let Some(wrapper) = node.wrapper {
        let padding = tree.layout(parent)?.padding;
        let mut style = tree.style(wrapper)?.clone();
        style.padding = padding.map(taffy::LengthPercentage::length);
        tree.set_style(wrapper, style)?;
    }
    for child in &node.children {
        resolve_wrapper_padding(tree, child, node.id)?;
    }
    Ok(())
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
    parent_style: &Style,
    scaled: &mut Vec<ScaledNode>,
) -> Result<LayoutNode, TaffyError> {
    let children = block
        .children
        .iter()
        .map(|child| build_node(tree, child, &block.layout, scaled))
        .collect::<Result<Vec<_>, _>>()?;
    normalize_fill(tree, &block.children, &children, &block.layout)?;
    let absolute = block.legacy_absolute;
    let style = block_style(block, parent_style, absolute);
    let id = if children.is_empty() {
        tree.new_leaf_with_context(style, block)?
    } else {
        let ids = children
            .iter()
            .map(LayoutNode::outer_id)
            .collect::<Vec<_>>();
        tree.new_with_children(style, &ids)?
    };
    if block.collapse.iter().any(Option::is_some) {
        let horizontal = is_row(parent_style.flex_direction);
        scaled.push(ScaledNode {
            id,
            width: block.collapse[0],
            height: block.collapse[1],
            clamp_width: !absolute
                && parent_style.display == Display::Flex
                && !horizontal
                && block.collapse[0].is_some()
                && block.sizing[0].is_some(),
            clamp_height: !absolute
                && parent_style.display == Display::Flex
                && horizontal
                && block.collapse[1].is_some()
                && block.sizing[1].is_some(),
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
                    left: LengthPercentageAuto::length(x),
                    right: LengthPercentageAuto::length(-x),
                    top: LengthPercentageAuto::length(y),
                    bottom: LengthPercentageAuto::length(-y),
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
                padding: parent_style.padding,
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

    fn geometry(
        &self,
        tree: &TaffyTree<&Block>,
        parent_origin: [f32; 2],
        containing_origin: [f32; 2],
    ) -> Result<Geometry, TaffyError> {
        let layout = tree.layout(self.id)?;
        let offset = self.wrapper.map(|id| tree.layout(id)).transpose()?;
        let position = tree.style(self.id)?.position;
        // Taffy reports out-of-flow locations relative to their containing block, not their tree parent.
        let origin = if offset.is_some() || position == Position::Absolute {
            containing_origin
        } else if position == Position::Fixed {
            [0.0; 2]
        } else {
            parent_origin
        };
        let world = [
            origin[0] + layout.location.x + offset.map_or(0.0, |layout| layout.location.x),
            origin[1] + layout.location.y + offset.map_or(0.0, |layout| layout.location.y),
        ];
        let child_containing = if position == Position::Static {
            containing_origin
        } else {
            world
        };
        Ok(Geometry {
            rect: Rect::new(world[0], world[1], layout.size.width, layout.size.height),
            position: if self.wrapper.is_some() {
                Position::Absolute
            } else {
                position
            },
            establishes_containing_block: position != Position::Static,
            content: Rect::new(
                layout.border.left + layout.padding.left,
                layout.border.top + layout.padding.top,
                layout.content_box_width().max(0.0),
                layout.content_box_height().max(0.0),
            ),
            padding: Rect::new(
                layout.border.left,
                layout.border.top,
                (layout.size.width - layout.border.left - layout.border.right).max(0.0),
                (layout.size.height - layout.border.top - layout.border.bottom).max(0.0),
            ),
            scroll_range: crate::ScrollRange {
                horizontal: layout.scroll_width(),
                vertical: layout.scroll_height(),
            },
            border_width: layout
                .border
                .left
                .max(layout.border.right)
                .max(layout.border.top)
                .max(layout.border.bottom),
            children: self
                .children
                .iter()
                .map(|node| node.geometry(tree, world, child_containing))
                .collect::<Result<_, _>>()?,
        })
    }
}

fn block_style(block: &Block, parent_style: &Style, absolute: bool) -> Style {
    let mut style = block.layout.clone();
    if absolute {
        style.position = Position::Relative;
    }
    let horizontal = !absolute && is_row(parent_style.flex_direction);
    let grid_item = !absolute && parent_style.display == Display::Grid;
    if let Some(width) = block.sizing[0] {
        style.size.width = dimension(width, absolute);
    }
    if let Some(height) = block.sizing[1] {
        style.size.height = dimension(height, absolute);
    }
    if grid_item {
        if block.sizing[0].and_then(fill_weight).is_some() {
            style.justify_self = Some(taffy::AlignItems::STRETCH);
        }
        if block.sizing[1].and_then(fill_weight).is_some() {
            style.align_self = Some(taffy::AlignItems::STRETCH);
        }
    } else {
        let (main, cross) = if horizontal {
            block.sizing.into()
        } else {
            (block.sizing[1], block.sizing[0])
        };
        if let Some(weight) = main.and_then(fill_weight) {
            style.flex_grow = if absolute { 1.0 } else { weight };
            // Unlike a definite zero, 0% retains intrinsic size in a fit-sized parent.
            style.flex_basis = Dimension::percent(0.0);
        }
        if cross.and_then(fill_weight).is_some() {
            style.align_self = Some(taffy::AlignItems::STRETCH);
        } else if !absolute && matches!(cross, Some(Size::Fit)) {
            if horizontal && block.collapse[1].is_none() && style.max_size.height.is_auto() {
                style.max_size.height = LengthPercentageAuto::percent(1.0);
            } else if !horizontal && block.collapse[0].is_none() && style.max_size.width.is_auto() {
                style.max_size.width = LengthPercentageAuto::percent(1.0);
            }
        }
    }
    if block.collapse[0].is_some() {
        style.size.width = Dimension::max_content();
    }
    if block.collapse[1].is_some() {
        style.size.height = Dimension::max_content();
    }
    style
}

fn normalize_fill(
    tree: &mut TaffyTree<&Block>,
    blocks: &[Block],
    nodes: &[LayoutNode],
    parent_style: &Style,
) -> Result<(), TaffyError> {
    if parent_style.display != Display::Flex {
        return Ok(());
    }
    let horizontal = is_row(parent_style.flex_direction);
    let weights = blocks
        .iter()
        .filter(|block| crate::is_flow(block))
        .filter_map(|block| {
            if horizontal {
                block.sizing[0]
            } else {
                block.sizing[1]
            }
            .and_then(fill_weight)
        });
    let total: f64 = weights.clone().map(f64::from).sum();
    let count = weights.filter(|weight| *weight > 0.0).count() as f64;
    if total > 0.0 {
        for (_, node) in blocks.iter().zip(nodes).filter(|(block, _)| {
            crate::is_flow(block)
                && (if horizontal {
                    block.sizing[0]
                } else {
                    block.sizing[1]
                })
                .and_then(fill_weight)
                .is_some()
        }) {
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

const fn finite_positive(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

const fn is_row(direction: FlexDirection) -> bool {
    matches!(direction, FlexDirection::Row | FlexDirection::RowReverse)
}

#[derive(Clone, Copy)]
struct ApplyContext<'a> {
    viewport: Rect,
    scroll: [f32; 2],
    containing_scroll: [f32; 2],
    clips: &'a [crate::ClipShape],
    layer: crate::Layer,
    explicit_clip_origin: [f32; 2],
}

fn apply_geometry(block: &mut Block, geometry: Geometry, context: ApplyContext<'_>) {
    let scroll = match geometry.position {
        Position::Fixed => [0.0; 2],
        Position::Absolute => context.containing_scroll,
        _ => context.scroll,
    };
    let rect = Rect::new(
        context.viewport.x + geometry.rect.x - scroll[0],
        context.viewport.y + geometry.rect.y - scroll[1],
        geometry.rect.width,
        geometry.rect.height,
    );
    block.content_rect = Rect::new(
        rect.x + geometry.content.x,
        rect.y + geometry.content.y,
        geometry.content.width,
        geometry.content.height,
    );
    block.padding_rect = Rect::new(
        rect.x + geometry.padding.x,
        rect.y + geometry.padding.y,
        geometry.padding.width,
        geometry.padding.height,
    );
    block.border_width = geometry.border_width;
    crate::prepare_block(
        block,
        rect,
        context.clips,
        context.layer,
        context.explicit_clip_origin,
        context.viewport,
    );
    let mut clip_origin = [block.content_rect.x, block.content_rect.y];
    let own_scroll = [
        block
            .horizontal_scroll
            .map_or(0.0, |scroll| scroll.offset.max(0.0)),
        block
            .vertical_scroll
            .map_or(0.0, |scroll| scroll.offset.max(0.0)),
    ];
    clip_origin[0] -= own_scroll[0];
    clip_origin[1] -= own_scroll[1];
    let child_scroll = [scroll[0] + own_scroll[0], scroll[1] + own_scroll[1]];
    let containing_scroll = if geometry.establishes_containing_block {
        child_scroll
    } else {
        context.containing_scroll
    };
    for (child, geometry) in block.children.iter_mut().zip(geometry.children) {
        let clips = if child.legacy_absolute && child.layer > block.layer {
            &[][..]
        } else {
            block.content_clips.as_slice()
        };
        apply_geometry(
            child,
            geometry,
            ApplyContext {
                viewport: context.viewport,
                scroll: child_scroll,
                containing_scroll,
                clips,
                layer: block.layer,
                explicit_clip_origin: clip_origin,
            },
        );
    }
    block.scroll_range = crate::ScrollRange {
        horizontal: if block.horizontal_scroll.is_some() {
            geometry.scroll_range.horizontal
        } else {
            0.0
        },
        vertical: if block.vertical_scroll.is_some() {
            geometry.scroll_range.vertical
        } else {
            0.0
        },
    };
}
