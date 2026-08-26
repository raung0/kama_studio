use kama_ui::{measure_layout, ui, ui_component, Align, BuildCtx, Color, IconId, Rect, Size};

#[derive(Default)]
struct BadgeComponent;

#[ui_component("Badge", stateless)]
impl Component for BadgeComponent {
    fn ui(&mut self, ctx: &mut BuildCtx, text: String) -> BlockBuilder<'_> {
        let _ = ctx.new().text(text);
    }
}

#[derive(Default)]
struct Meter {
    builds: u32,
}

#[ui_component("Meter")]
impl Component for Meter {
    fn ui(&mut self, ctx: &mut BuildCtx, value: f32) -> BlockBuilder<'_> {
        self.builds += 1;
        let _ = ctx.new().text(format!("{value:.0}%"));
    }
}

#[allow(dead_code)]
fn build_tree(ctx: &mut BuildCtx, meter: &mut Meter, show: bool, items: &[u32], icon: IconId) {
    ui!(ctx, {
        @let padding = 4.0;
        Column {
            id: @format("root {}", items.len());
            width: Size::Fill;
            padding: padding;
            interactive_if: show;

            Row {
                width: Size::Fill;
                gap: 2.0;

                Rect(
                    @format("direct-rect {}", items.len()),
                    kama_ui::Rect::new(0.0, 0.0, 1.0, 1.0),
                ) {
                    fill: Color::TRANSPARENT;
                }

                @for item in items {
                    Block {
                        id: @format("item {}", item);
                        width: Size::Fill;
                        height: Size::Pixels(20.0);
                        text: item.to_string();
                    }
                }

                HSpacer {}

                Icon {
                    id: "status";
                    icon!: icon;
                    color!: Color::WHITE;
                    texture_rotation: 0.5;
                    width: Size::Pixels(16.0);
                    height: Size::Pixels(16.0);
                }
            }

            @if show {
                Meter(meter) {
                    value!: 42.0;
                    width: Size::Fill;
                }
            } @else {
                Badge {
                    text!: "hidden".to_owned();
                    text_color: Color::WHITE;
                }
            }

            @match show {
                true => { VSpacer {} },
                false => {
                    @rust {
                        let _ = ctx.new().height(Size::Pixels(1.0)).build();
                    }
                }
            }
        }
    });
}

#[test]
fn measured_layout_uses_declarative_flow_geometry() {
    let ((left, first, second), layout) = measure_layout(Rect::new(0.0, 0.0, 120.0, 40.0), |ctx| {
        let mut left = Default::default();
        let mut first = Default::default();
        let mut second = Default::default();
        let _ = ctx
            .new()
            .width(Size::Fill)
            .height(Size::Pixels(40.0))
            .row()
            .align_items(Align::Center)
            .gap(4.0)
            .children(|ctx| {
                left = ctx
                    .new()
                    .width(Size::Pixels(20.0))
                    .height(Size::Fill)
                    .build();
                first = ctx
                    .new()
                    .width(Size::Fill)
                    .height(Size::Pixels(24.0))
                    .build();
                second = ctx
                    .new()
                    .width(Size::Fill)
                    .height(Size::Pixels(16.0))
                    .build();
            })
            .build();
        (left, first, second)
    });

    assert_eq!(layout.rect(left), Some(Rect::new(0.0, 0.0, 20.0, 40.0)));
    assert_eq!(layout.rect(first), Some(Rect::new(24.0, 8.0, 46.0, 24.0)));
    assert_eq!(layout.rect(second), Some(Rect::new(74.0, 12.0, 46.0, 16.0)));
}
