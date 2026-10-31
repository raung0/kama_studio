use kama_ui::{Rect, ScrollState, Size, measure_layout};

#[test]
fn fractional_fill_weights_use_all_remaining_space() {
    let [first, second] = kama_ui::layout::row_array(
        Rect::new(20.0, 10.0, 300.0, 40.0),
        &[
            kama_ui::layout::Item::fill_portion(0.1),
            kama_ui::layout::Item::fill_portion(0.2),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
    );
    assert_rect_close(first, Rect::new(20.0, 10.0, 100.0, 40.0));
    assert_rect_close(second, Rect::new(120.0, 10.0, 200.0, 40.0));
}

#[test]
fn cross_axis_fit_scale_scales_before_clamping_to_available_width() {
    let ((scaled, child), layout) = measure_layout(Rect::new(0.0, 0.0, 100.0, 40.0), |ctx| {
        let mut child = None;
        let scaled = ctx
            .new()
            .width(Size::FitScale(0.25))
            .height(Size::Pixels(20.0))
            .children(|ctx| {
                child = Some(
                    ctx.new()
                        .width(Size::Pixels(200.0))
                        .height(Size::Fill)
                        .build(),
                );
            })
            .build();
        (scaled, child.unwrap())
    });
    assert_eq!(layout.rect(scaled).unwrap().width, 50.0);
    assert_eq!(layout.rect(child).unwrap().width, 200.0);
}

#[test]
fn fixed_row_controls_keep_their_width_when_the_row_overflows() {
    let ((first, second), layout) = measure_layout(Rect::new(0.0, 0.0, 100.0, 30.0), |ctx| {
        let mut first = None;
        let mut second = None;
        let _ = ctx
            .new()
            .width(Size::Fill)
            .height(Size::Fill)
            .row()
            .children(|ctx| {
                first = Some(
                    ctx.new()
                        .width(Size::Pixels(72.0))
                        .height(Size::Fill)
                        .build(),
                );
                second = Some(
                    ctx.new()
                        .width(Size::Pixels(61.0))
                        .height(Size::Fill)
                        .build(),
                );
            })
            .build();
        (first.unwrap(), second.unwrap())
    });

    assert_eq!(layout.rect(first), Some(Rect::new(0.0, 0.0, 72.0, 30.0)));
    assert_eq!(layout.rect(second), Some(Rect::new(72.0, 0.0, 61.0, 30.0)));
}

#[test]
fn positioned_fill_overlay_uses_content_box_without_affecting_fit_size() {
    let ((container, flow_child, overlay), layout) =
        measure_layout(Rect::new(0.0, 0.0, 300.0, 200.0), |ctx| {
            let mut flow_child = None;
            let mut overlay = None;
            let container = ctx
                .new()
                .border(2)
                .padding(10.0)
                .children(|ctx| {
                    flow_child = Some(
                        ctx.new()
                            .width(Size::Pixels(50.0))
                            .height(Size::Pixels(30.0))
                            .build(),
                    );
                    overlay = Some(
                        ctx.new()
                            .overlay()
                            .position((7.0, 9.0))
                            .width(Size::Fill)
                            .height(Size::Fill)
                            .build(),
                    );
                })
                .build();
            (container, flow_child.unwrap(), overlay.unwrap())
        });

    assert_eq!(
        layout.rect(container),
        Some(Rect::new(0.0, 0.0, 74.0, 54.0))
    );
    assert_eq!(
        layout.rect(flow_child),
        Some(Rect::new(12.0, 12.0, 50.0, 30.0))
    );
    assert_eq!(
        layout.rect(overlay),
        Some(Rect::new(19.0, 21.0, 50.0, 30.0))
    );
}

#[test]
fn centered_overlay_is_centered_within_viewport_origin() {
    let (overlay, layout) = measure_layout(Rect::new(40.0, 25.0, 200.0, 120.0), |ctx| {
        ctx.new()
            .overlay()
            .centered()
            .width(Size::Pixels(60.0))
            .height(Size::Pixels(20.0))
            .build()
    });

    assert_eq!(
        layout.rect(overlay),
        Some(Rect::new(110.0, 75.0, 60.0, 20.0))
    );
}

#[test]
fn scroll_offset_moves_children_without_changing_scroll_extent() {
    let measure = |offset| {
        measure_layout(Rect::new(0.0, 0.0, 100.0, 60.0), |ctx| {
            let mut child = None;
            let scroll = ctx
                .new()
                .width(Size::Fill)
                .height(Size::Fill)
                .vertical_scroll(ScrollState { offset })
                .children(|ctx| {
                    child = Some(
                        ctx.new()
                            .width(Size::Fill)
                            .height(Size::Pixels(120.0))
                            .build(),
                    );
                })
                .build();
            (scroll, child.unwrap())
        })
    };
    let ((scroll_at_top, child_at_top), top) = measure(0.0);
    let ((scroll_scrolled, child_scrolled), scrolled) = measure(25.0);

    assert_eq!(
        top.rect(child_at_top),
        Some(Rect::new(0.0, 0.0, 100.0, 120.0))
    );
    assert_eq!(
        scrolled.rect(child_scrolled),
        Some(Rect::new(0.0, -25.0, 100.0, 120.0))
    );
    assert_eq!(top.scroll_range(scroll_at_top).unwrap().vertical, 60.0);
    assert_eq!(
        scrolled.scroll_range(scroll_scrolled).unwrap().vertical,
        60.0
    );
}

#[test]
fn weighted_fill_shares_space_left_after_fixed_width_padding_and_gap() {
    let ((first, fixed, second), layout) =
        measure_layout(Rect::new(0.0, 0.0, 300.0, 40.0), |ctx| {
            let mut first = None;
            let mut fixed = None;
            let mut second = None;
            let _ = ctx
                .new()
                .width(Size::Fill)
                .height(Size::Fill)
                .row()
                .padding(10.0)
                .gap(5.0)
                .children(|ctx| {
                    first = Some(
                        ctx.new()
                            .width(Size::FillPortion(1.0))
                            .height(Size::Fill)
                            .build(),
                    );
                    fixed = Some(
                        ctx.new()
                            .width(Size::Pixels(40.0))
                            .height(Size::Fill)
                            .build(),
                    );
                    second = Some(
                        ctx.new()
                            .width(Size::FillPortion(2.0))
                            .height(Size::Fill)
                            .build(),
                    );
                })
                .build();
            (first.unwrap(), fixed.unwrap(), second.unwrap())
        });

    let third = 230.0 / 3.0;
    assert_rect_close(
        layout.rect(first).unwrap(),
        Rect::new(10.0, 10.0, third, 20.0),
    );
    assert_rect_close(
        layout.rect(fixed).unwrap(),
        Rect::new(10.0 + third + 5.0, 10.0, 40.0, 20.0),
    );
    assert_rect_close(
        layout.rect(second).unwrap(),
        Rect::new(10.0 + third + 50.0, 10.0, third * 2.0, 20.0),
    );
}

fn assert_rect_close(actual: Rect, expected: Rect) {
    assert!(
        (actual.x - expected.x).abs() < 0.0001,
        "{actual:?} != {expected:?}"
    );
    assert!(
        (actual.y - expected.y).abs() < 0.0001,
        "{actual:?} != {expected:?}"
    );
    assert!(
        (actual.width - expected.width).abs() < 0.0001,
        "{actual:?} != {expected:?}"
    );
    assert!(
        (actual.height - expected.height).abs() < 0.0001,
        "{actual:?} != {expected:?}"
    );
}

#[test]
fn nested_fit_scale_preserves_intrinsic_height_through_each_scale() {
    let ((outer, inner, grandchild), layout) =
        measure_layout(Rect::new(0.0, 0.0, 120.0, 120.0), |ctx| {
            let mut inner = None;
            let mut grandchild = None;
            let outer = ctx
                .new()
                .width(Size::Pixels(80.0))
                .height(Size::FitScale(0.5))
                .children(|ctx| {
                    inner = Some(
                        ctx.new()
                            .width(Size::Fill)
                            .height(Size::FitScale(0.5))
                            .children(|ctx| {
                                grandchild = Some(
                                    ctx.new()
                                        .width(Size::Fill)
                                        .height(Size::Pixels(80.0))
                                        .build(),
                                );
                            })
                            .build(),
                    );
                })
                .build();
            (outer, inner.unwrap(), grandchild.unwrap())
        });

    assert_eq!(layout.rect(outer).unwrap().height, 20.0);
    assert_eq!(layout.rect(inner).unwrap().height, 40.0);
    assert_eq!(layout.rect(grandchild).unwrap().height, 80.0);
}

#[test]
fn wrapped_text_at_fixed_width_is_taller_than_unwrapped_text() {
    let measure = |wrap: bool| {
        measure_layout(Rect::new(0.0, 0.0, 70.0, 200.0), |ctx| {
            ctx.new()
                .width(Size::Pixels(70.0))
                .height(Size::Fit)
                .text("A deliberately long line of text that needs multiple lines")
                .no_wrap_if(!wrap)
                .build()
        })
    };
    let (wrapped, wrapped_layout) = measure(true);
    let (unwrapped, unwrapped_layout) = measure(false);

    assert!(
        wrapped_layout.rect(wrapped).unwrap().height
            > unwrapped_layout.rect(unwrapped).unwrap().height
    );
}
