use kama_ui::taffy::{
    Dimension, FlexWrap, LengthPercentage, LengthPercentageAuto, Position, Style,
};
use kama_ui::{Layer, Rect, ScrollState, measure_layout};

fn px(value: f32) -> Dimension {
    Dimension::length(value)
}
fn pct(value: f32) -> Dimension {
    Dimension::percent(value)
}

#[test]
fn native_padding_and_percentage_size_resolve_from_nonzero_viewport() {
    let ((parent, child), layout) = measure_layout(Rect::new(30.0, 20.0, 200.0, 100.0), |ctx| {
        let mut child = None;
        let parent = ctx
            .new()
            .layout(Style {
                size: kama_ui::taffy::Size {
                    width: px(100.0),
                    height: px(60.0),
                },
                padding: kama_ui::taffy::Rect {
                    left: LengthPercentage::length(10.0),
                    right: LengthPercentage::length(20.0),
                    top: LengthPercentage::length(5.0),
                    bottom: LengthPercentage::length(15.0),
                },
                ..Style::default()
            })
            .children(|ctx| {
                child = Some(
                    ctx.new()
                        .size(kama_ui::taffy::Size {
                            width: pct(0.5),
                            height: pct(0.5),
                        })
                        .build(),
                );
            })
            .build();
        (parent, child.unwrap())
    });
    assert_eq!(
        layout.rect(parent),
        Some(Rect::new(30.0, 20.0, 100.0, 60.0))
    );
    assert_eq!(
        layout.content_rect(parent),
        Some(Rect::new(40.0, 25.0, 70.0, 40.0))
    );
    assert_eq!(layout.rect(child), Some(Rect::new(40.0, 25.0, 35.0, 20.0)));
}

#[test]
fn native_min_max_constraints_are_applied() {
    let (block, layout) = measure_layout(Rect::new(0.0, 0.0, 300.0, 200.0), |ctx| {
        ctx.new()
            .size(kama_ui::taffy::Size {
                width: px(50.0),
                height: px(150.0),
            })
            .min_size(kama_ui::taffy::Size {
                width: LengthPercentageAuto::length(80.0),
                height: LengthPercentageAuto::length(20.0),
            })
            .max_size(kama_ui::taffy::Size {
                width: LengthPercentageAuto::length(100.0),
                height: LengthPercentageAuto::length(100.0),
            })
            .build()
    });
    assert_eq!(layout.rect(block), Some(Rect::new(0.0, 0.0, 80.0, 100.0)));
}

#[test]
fn native_aspect_ratio_resolves_auto_height() {
    let (block, layout) = measure_layout(Rect::new(0.0, 0.0, 300.0, 200.0), |ctx| {
        ctx.new()
            .size(kama_ui::taffy::Size {
                width: px(60.0),
                height: Dimension::auto(),
            })
            .aspect_ratio(2.0)
            .build()
    });
    assert_eq!(layout.rect(block), Some(Rect::new(0.0, 0.0, 60.0, 30.0)));
}

#[test]
fn native_flex_wrap_places_third_fixed_item_on_next_line() {
    let (items, layout) = measure_layout(Rect::new(0.0, 0.0, 140.0, 100.0), |ctx| {
        let mut items = Vec::new();
        let _ = ctx
            .new()
            .layout(Style {
                size: kama_ui::taffy::Size {
                    width: px(140.0),
                    height: px(100.0),
                },
                flex_wrap: FlexWrap::Wrap,
                align_content: kama_ui::taffy::AlignContent::START,
                gap: kama_ui::taffy::Size {
                    width: LengthPercentage::length(10.0),
                    height: LengthPercentage::length(7.0),
                },
                ..Style::default()
            })
            .children(|ctx| {
                for _ in 0..3 {
                    items.push(
                        ctx.new()
                            .size(kama_ui::taffy::Size {
                                width: px(60.0),
                                height: px(20.0),
                            })
                            .build(),
                    );
                }
            })
            .build();
        items
    });
    assert_eq!(layout.rect(items[0]), Some(Rect::new(0.0, 0.0, 60.0, 20.0)));
    assert_eq!(
        layout.rect(items[1]),
        Some(Rect::new(70.0, 0.0, 60.0, 20.0))
    );
    assert_eq!(
        layout.rect(items[2]),
        Some(Rect::new(0.0, 27.0, 60.0, 20.0))
    );
}

#[test]
fn popup_layer_changes_paint_order_without_removing_block_from_flow() {
    let ((popup, following), layout) = measure_layout(Rect::new(0.0, 0.0, 200.0, 80.0), |ctx| {
        let mut popup = None;
        let mut following = None;
        let _ = ctx
            .new()
            .column()
            .children(|ctx| {
                popup = Some(
                    ctx.new()
                        .size(kama_ui::taffy::Size {
                            width: px(70.0),
                            height: px(20.0),
                        })
                        .layer(Layer::Popup)
                        .build(),
                );
                following = Some(
                    ctx.new()
                        .size(kama_ui::taffy::Size {
                            width: px(30.0),
                            height: px(10.0),
                        })
                        .build(),
                );
            })
            .build();
        (popup.unwrap(), following.unwrap())
    });
    assert_eq!(layout.rect(popup), Some(Rect::new(0.0, 0.0, 70.0, 20.0)));
    assert_eq!(
        layout.rect(following),
        Some(Rect::new(0.0, 20.0, 30.0, 10.0))
    );
}

#[test]
fn native_absolute_child_uses_padding_box_and_skips_static_intermediate() {
    let ((ancestor, child), layout) = measure_layout(Rect::new(0.0, 0.0, 200.0, 100.0), |ctx| {
        let mut child = None;
        let ancestor = ctx
            .new()
            .layout(Style {
                position: Position::Relative,
                size: kama_ui::taffy::Size {
                    width: px(120.0),
                    height: px(80.0),
                },
                padding: kama_ui::taffy::Rect::length(10.0),
                ..Style::default()
            })
            .children(|ctx| {
                let _ = ctx
                    .new()
                    .layout(Style {
                        position: Position::Static,
                        size: kama_ui::taffy::Size {
                            width: px(50.0),
                            height: px(30.0),
                        },
                        ..Style::default()
                    })
                    .children(|ctx| {
                        child = Some(
                            ctx.new()
                                .layout(Style {
                                    position: Position::Absolute,
                                    inset: kama_ui::taffy::Rect {
                                        left: LengthPercentageAuto::length(5.0),
                                        top: LengthPercentageAuto::length(7.0),
                                        ..kama_ui::taffy::Rect::auto()
                                    },
                                    size: kama_ui::taffy::Size {
                                        width: px(20.0),
                                        height: px(10.0),
                                    },
                                    ..Style::default()
                                })
                                .build(),
                        );
                    })
                    .build();
            })
            .build();
        (ancestor, child.unwrap())
    });
    assert_eq!(
        layout.rect(ancestor),
        Some(Rect::new(0.0, 0.0, 120.0, 80.0))
    );
    assert_eq!(layout.rect(child), Some(Rect::new(5.0, 7.0, 20.0, 10.0)));
}

#[test]
fn raw_layout_replacement_clears_legacy_hints_and_later_setters_win() {
    let (block, layout) = measure_layout(Rect::new(0.0, 0.0, 200.0, 100.0), |ctx| {
        ctx.new()
            .width(kama_ui::Size::Pixels(150.0))
            .height(kama_ui::Size::Pixels(60.0))
            .layout(Style::default())
            .size(kama_ui::taffy::Size {
                width: px(80.0),
                height: px(40.0),
            })
            .build()
    });
    assert_eq!(layout.rect(block), Some(Rect::new(0.0, 0.0, 80.0, 40.0)));
}

#[test]
fn native_scroll_extent_includes_visible_grandchildren_but_respects_clipping() {
    let measure = |clip| {
        measure_layout(Rect::new(0.0, 0.0, 100.0, 50.0), |ctx| {
            ctx.new()
                .size(kama_ui::taffy::Size {
                    width: px(100.0),
                    height: px(50.0),
                })
                .vertical_scroll(ScrollState { offset: 0.0 })
                .children(|ctx| {
                    let _ = ctx
                        .new()
                        .layout(Style {
                            overflow: kama_ui::taffy::Point {
                                x: if clip {
                                    kama_ui::taffy::Overflow::Hidden
                                } else {
                                    kama_ui::taffy::Overflow::Visible
                                },
                                y: if clip {
                                    kama_ui::taffy::Overflow::Hidden
                                } else {
                                    kama_ui::taffy::Overflow::Visible
                                },
                            },
                            ..Style::default()
                        })
                        .size(kama_ui::taffy::Size {
                            width: px(100.0),
                            height: px(20.0),
                        })
                        .children(|ctx| {
                            let _ = ctx
                                .new()
                                .size(kama_ui::taffy::Size {
                                    width: px(100.0),
                                    height: px(100.0),
                                })
                                .build();
                        })
                        .build();
                })
                .build()
        })
    };
    let (visible, visible_layout) = measure(false);
    let (clipped, clipped_layout) = measure(true);
    assert_eq!(visible_layout.scroll_range(visible).unwrap().vertical, 50.0);
    assert_eq!(clipped_layout.scroll_range(clipped).unwrap().vertical, 0.0);
}
