use kama_ui::taffy::prelude::{Display, Style, fr, length};
use kama_ui::{BlockId, Rect, Size, measure_layout, ui};

#[test]
fn declarative_grid_uses_native_tracks_gap_and_padding() {
    let ((first, second), layout) = measure_layout(Rect::new(0.0, 0.0, 200.0, 40.0), |ctx| {
        let mut first = BlockId::default();
        let mut second = BlockId::default();
        ui!(ctx, {
            Grid {
                layout: Style {
                    display: Display::Grid,
                    size: kama_ui::taffy::Size { width: length(200.0), height: length(40.0) },
                    grid_template_columns: vec![length(40.0), fr(1.0)],
                    gap: kama_ui::taffy::geometry::Size::length(6.0),
                    padding: kama_ui::taffy::geometry::Rect::length(7.0),
                    ..Style::default()
                };

                @rust { first = ctx.new().width(Size::Fill).height(Size::Pixels(20.0)).build(); }
                @rust { second = ctx.new().width(Size::Fill).height(Size::Pixels(20.0)).build(); }
            }
        });
        (first, second)
    });

    assert_eq!(layout.rect(first), Some(Rect::new(7.0, 7.0, 40.0, 20.0)));
    assert_eq!(layout.rect(second), Some(Rect::new(53.0, 7.0, 140.0, 20.0)));
}
