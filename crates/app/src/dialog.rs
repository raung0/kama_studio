use std::{hash::Hash, sync::Arc};

use kama_ui::{BlockId, Color, Rect, ScrollState, Size};

use crate::theme;

pub(crate) const SEARCH_DIALOG_PADDING: f32 = 6.0;
pub(crate) const SEARCH_DIALOG_GAP: f32 = 3.0;
pub(crate) const SEARCH_DIALOG_TITLE_HEIGHT: f32 = 34.0;
pub(crate) const SEARCH_DIALOG_INPUT_HEIGHT: f32 = 30.0;
pub(crate) const SEARCH_DIALOG_ROW_HEIGHT: f32 = 31.0;
pub(crate) const SEARCH_DIALOG_FOOTER_HEIGHT: f32 = 18.0;
pub(crate) const SEARCH_DIALOG_CLOSE_WIDTH: f32 = SEARCH_DIALOG_FOOTER_HEIGHT;

#[derive(Clone)]
pub(crate) struct SearchDialogRects {
    pub(crate) title: Option<Rect>,
    pub(crate) search: Rect,
    pub(crate) rows: Rect,
    pub(crate) max_scroll: f32,
    pub(crate) footer: Rect,
    pub(crate) help: Rect,
    pub(crate) close: Rect,
    pub(crate) empty: Rect,
    pub(crate) items: Arc<[Rect]>,
}

pub(crate) fn measure_search_dialog(
    rect: Rect,
    has_title: bool,
    row_count: usize,
    scroll: ScrollState,
) -> SearchDialogRects {
    let (((title, search, rows, footer, help, close, empty), row_ids), measured) =
        kama_ui::measure_layout(rect, |ctx| {
            let mut title = None;
            let mut search = BlockId(0);
            let mut rows = BlockId(0);
            let mut footer = BlockId(0);
            let mut help = BlockId(0);
            let mut close = BlockId(0);
            let mut empty = BlockId(0);
            let mut row_ids = Vec::with_capacity(row_count);

            ctx.new()
                .column()
                .width(Size::Fill)
                .height(Size::Fill)
                .padding(SEARCH_DIALOG_PADDING)
                .gap(SEARCH_DIALOG_GAP)
                .children(|ctx| {
                    if has_title {
                        title = Some(
                            ctx.new()
                                .width(Size::Fill)
                                .height(Size::Pixels(SEARCH_DIALOG_TITLE_HEIGHT))
                                .build(),
                        );
                    }
                    search = ctx
                        .new()
                        .width(Size::Fill)
                        .height(Size::Pixels(SEARCH_DIALOG_INPUT_HEIGHT))
                        .build();
                    rows = ctx
                        .new()
                        .column()
                        .width(Size::Fill)
                        .height(Size::Fill)
                        .vertical_scroll(scroll)
                        .children(|ctx| {
                            ctx.new()
                                .column()
                                .width(Size::Fill)
                                .height(Size::Fit)
                                .gap(SEARCH_DIALOG_GAP)
                                .children(|ctx| {
                                    if row_count == 0 {
                                        empty = ctx
                                            .new()
                                            .width(Size::Fill)
                                            .height(Size::Pixels(28.0))
                                            .build();
                                    } else {
                                        for _ in 0..row_count {
                                            row_ids.push(
                                                ctx.new()
                                                    .width(Size::Fill)
                                                    .height(Size::Pixels(SEARCH_DIALOG_ROW_HEIGHT))
                                                    .build(),
                                            );
                                        }
                                    }
                                })
                                .build();
                        })
                        .build();
                    footer = ctx
                        .new()
                        .row()
                        .width(Size::Fill)
                        .height(Size::Pixels(SEARCH_DIALOG_FOOTER_HEIGHT))
                        .children(|ctx| {
                            help = ctx.new().width(Size::Fill).height(Size::Fill).build();
                            close = ctx
                                .new()
                                .width(Size::Pixels(SEARCH_DIALOG_CLOSE_WIDTH))
                                .height(Size::Fill)
                                .build();
                        })
                        .build();
                })
                .build();

            ((title, search, rows, footer, help, close, empty), row_ids)
        });

    let rect_for = |id: BlockId, what: &str| {
        measured
            .rect(id)
            .unwrap_or_else(|| panic!("{what} search dialog rect"))
    };
    SearchDialogRects {
        title: title.map(|id| rect_for(id, "title")),
        search: rect_for(search, "search"),
        rows: rect_for(rows, "rows"),
        max_scroll: measured
            .scroll_range(rows)
            .expect("search dialog scroll range")
            .vertical,
        footer: rect_for(footer, "footer"),
        help: rect_for(help, "help"),
        close: rect_for(close, "close"),
        empty: if row_count == 0 {
            rect_for(empty, "empty")
        } else {
            Rect::default()
        },
        items: row_ids
            .into_iter()
            .map(|id| rect_for(id, "row"))
            .collect::<Vec<_>>()
            .into(),
    }
}

fn scrim() -> Color {
    Color::rgba8(0x00, 0x00, 0x00, 0x38)
}

pub(crate) fn build_shell<K1: Hash, K2: Hash, F: FnOnce(&mut kama_ui::BuildCtx)>(
    ctx: &mut kama_ui::BuildCtx,
    scrim_key: K1,
    panel_key: K2,
    viewport: Rect,
    panel: Rect,
    opacity: f32,
    children: F,
) {
    kama_ui::ui!(ctx, {
        Rect(scrim_key, viewport) {
            overlay;
            opacity: opacity;
            fill: scrim();
            interactive_no_reveal;
            animate_interaction: false;
        }
        Rect(panel_key, panel) {
            overlay;
            opacity: opacity;
            backdrop_blur: 28.0;
            backdrop_tint: theme::popup_tint();
            fill: theme::floating_bg();
            border: 1;
            border_color: theme::accent();
            border_radius: 10.0;
            children: children;
        }
    });
}

pub(crate) fn build_search_dialog<K1, K2, K3, F>(
    ctx: &mut kama_ui::BuildCtx,
    scrim_key: K1,
    panel_key: K2,
    overlay_key: K3,
    viewport: Rect,
    panel: Rect,
    opacity: f32,
    layout: &SearchDialogRects,
    title: Option<&str>,
    help: &str,
    children: F,
) where
    K1: Hash,
    K2: Hash,
    K3: Hash,
    F: FnOnce(&mut kama_ui::BuildCtx, &SearchDialogRects),
{
    build_shell(ctx, scrim_key, panel_key, viewport, panel, opacity, |_| {});
    kama_ui::ui!(ctx, {
        Rect(overlay_key, viewport) {
            overlay; overflow_visible; opacity: opacity;
            @rust {
                if let (Some(title), Some(title_rect)) = (title, layout.title) {
                    kama_ui::ui!(ctx, {
                        Rect(("search-dialog-title", title), title_rect) {
                            padding: SEARCH_DIALOG_PADDING;
                            font_size: 14.0;
                            text_color: theme::popup_text();
                            text: title;
                        }
                    });
                }
                children(ctx, layout);
                kama_ui::ui!(ctx, {
                    Rect(("search-dialog-help", help), layout.help) {
                        font_size: 9.0;
                        text_color: theme::popup_dim();
                        text: help;
                    }
                    Rect(("search-dialog-close", help), layout.close) {
                        font_size: 15.0;
                        text_color: theme::popup_muted();
                        text_centered;
                        text: "×";
                        interactive;
                        tooltip: "Close";
                    }
                });
            }
        }
    });
}
