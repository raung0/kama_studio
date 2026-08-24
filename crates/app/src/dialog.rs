use std::{hash::Hash, sync::Arc};

use kama_ui::{BlockId, Color, CursorShape, Rect, ScrollState, Size};

use crate::theme;

pub(crate) const SEARCH_DIALOG_PADDING: f32 = 6.0;
pub(crate) const SEARCH_DIALOG_GAP: f32 = 3.0;
pub(crate) const SEARCH_DIALOG_TITLE_HEIGHT: f32 = 34.0;
pub(crate) const SEARCH_DIALOG_INPUT_HEIGHT: f32 = 30.0;
pub(crate) const SEARCH_DIALOG_ROW_HEIGHT: f32 = 31.0;
pub(crate) const SEARCH_DIALOG_FOOTER_HEIGHT: f32 = 18.0;
pub(crate) const SEARCH_DIALOG_CLOSE_WIDTH: f32 = SEARCH_DIALOG_FOOTER_HEIGHT;

#[derive(Clone, Copy)]
pub(crate) struct SearchSurfaceMetrics {
    pub(crate) padding: f32,
    pub(crate) gap: f32,
    pub(crate) title_height: f32,
    pub(crate) search_height: f32,
    pub(crate) auxiliary_height: f32,
    pub(crate) row_height: f32,
    pub(crate) row_gap: f32,
    pub(crate) footer_height: f32,
    pub(crate) close_width: f32,
}

impl SearchSurfaceMetrics {
    pub(crate) const fn dialog(has_title: bool) -> Self {
        Self {
            padding: SEARCH_DIALOG_PADDING,
            gap: SEARCH_DIALOG_GAP,
            title_height: if has_title {
                SEARCH_DIALOG_TITLE_HEIGHT
            } else {
                0.0
            },
            search_height: SEARCH_DIALOG_INPUT_HEIGHT,
            auxiliary_height: 0.0,
            row_height: SEARCH_DIALOG_ROW_HEIGHT,
            row_gap: SEARCH_DIALOG_GAP,
            footer_height: SEARCH_DIALOG_FOOTER_HEIGHT,
            close_width: SEARCH_DIALOG_CLOSE_WIDTH,
        }
    }
}

#[derive(Clone)]
pub(crate) struct SearchSurfaceRects {
    pub(crate) title: Option<Rect>,
    pub(crate) search: Rect,
    pub(crate) auxiliary: Option<Rect>,
    pub(crate) rows: Rect,
    pub(crate) max_scroll: f32,
    pub(crate) footer: Option<Rect>,
    pub(crate) help: Option<Rect>,
    pub(crate) close: Option<Rect>,
    pub(crate) empty: Rect,
    pub(crate) items: Arc<[Rect]>,
}

pub(crate) fn measure_search_surface(
    rect: Rect,
    metrics: SearchSurfaceMetrics,
    row_count: usize,
    scroll: ScrollState,
    constrained: bool,
) -> (SearchSurfaceRects, Rect) {
    #[derive(Default)]
    struct Ids {
        title: Option<BlockId>,
        search: BlockId,
        auxiliary: Option<BlockId>,
        rows: BlockId,
        footer: Option<BlockId>,
        help: Option<BlockId>,
        close: Option<BlockId>,
        empty: BlockId,
        items: Vec<BlockId>,
    }

    let ((root, ids), measured) = kama_ui::measure_layout(rect, |ctx| {
        let mut ids = Ids {
            items: Vec::with_capacity(row_count),
            ..Ids::default()
        };
        let root = ctx
            .new()
            .column()
            .width(Size::Fill)
            .height(if constrained { Size::Fill } else { Size::Fit })
            .padding(metrics.padding)
            .gap(metrics.gap)
            .children(|ctx| {
                if metrics.title_height > 0.0 {
                    ids.title = Some(
                        ctx.new()
                            .width(Size::Fill)
                            .height(Size::Pixels(metrics.title_height))
                            .build(),
                    );
                }
                ids.search = ctx
                    .new()
                    .width(Size::Fill)
                    .height(Size::Pixels(metrics.search_height))
                    .build();
                if metrics.auxiliary_height > 0.0 {
                    ids.auxiliary = Some(
                        ctx.new()
                            .width(Size::Fill)
                            .height(Size::Pixels(metrics.auxiliary_height))
                            .build(),
                    );
                }
                ids.rows = ctx
                    .new()
                    .column()
                    .width(Size::Fill)
                    .height(if constrained { Size::Fill } else { Size::Fit })
                    .vertical_scroll(scroll)
                    .children(|ctx| {
                        ctx.new()
                            .column()
                            .width(Size::Fill)
                            .height(Size::Fit)
                            .gap(metrics.row_gap)
                            .children(|ctx| {
                                if row_count == 0 {
                                    ids.empty = ctx
                                        .new()
                                        .width(Size::Fill)
                                        .height(Size::Pixels(28.0))
                                        .build();
                                } else {
                                    for _ in 0..row_count {
                                        ids.items.push(
                                            ctx.new()
                                                .width(Size::Fill)
                                                .height(Size::Pixels(metrics.row_height))
                                                .build(),
                                        );
                                    }
                                }
                            })
                            .build();
                    })
                    .build();
                if metrics.footer_height > 0.0 {
                    ids.footer = Some(
                        ctx.new()
                            .row()
                            .width(Size::Fill)
                            .height(Size::Pixels(metrics.footer_height))
                            .children(|ctx| {
                                ids.help =
                                    Some(ctx.new().width(Size::Fill).height(Size::Fill).build());
                                if metrics.close_width > 0.0 {
                                    ids.close = Some(
                                        ctx.new()
                                            .width(Size::Pixels(metrics.close_width))
                                            .height(Size::Fill)
                                            .build(),
                                    );
                                }
                            })
                            .build(),
                    );
                }
            })
            .build();
        (root, ids)
    });

    let rect_for = |id: BlockId, what: &str| {
        measured
            .rect(id)
            .unwrap_or_else(|| panic!("{what} search surface rect"))
    };
    let surface = SearchSurfaceRects {
        title: ids.title.map(|id| rect_for(id, "title")),
        search: rect_for(ids.search, "search"),
        auxiliary: ids.auxiliary.map(|id| rect_for(id, "auxiliary")),
        rows: rect_for(ids.rows, "rows"),
        max_scroll: measured
            .scroll_range(ids.rows)
            .expect("search surface scroll range")
            .vertical,
        footer: ids.footer.map(|id| rect_for(id, "footer")),
        help: ids.help.map(|id| rect_for(id, "help")),
        close: ids.close.map(|id| rect_for(id, "close")),
        empty: if row_count == 0 {
            rect_for(ids.empty, "empty")
        } else {
            Rect::default()
        },
        items: ids
            .items
            .into_iter()
            .map(|id| rect_for(id, "row"))
            .collect::<Vec<_>>()
            .into(),
    };
    (surface, rect_for(root, "root"))
}

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
    let surface = measure_search_surface(
        rect,
        SearchSurfaceMetrics::dialog(has_title),
        row_count,
        scroll,
        true,
    )
    .0;
    SearchDialogRects {
        title: surface.title,
        search: surface.search,
        rows: surface.rows,
        max_scroll: surface.max_scroll,
        footer: surface.footer.expect("search dialog footer"),
        help: surface.help.expect("search dialog help"),
        close: surface.close.expect("search dialog close"),
        empty: surface.empty,
        items: surface.items,
    }
}

fn scrim() -> Color {
    Color::rgba8(0x00, 0x00, 0x00, 0x38)
}

pub(crate) fn build_panel_shell<K: Hash, F: FnOnce(&mut kama_ui::BuildCtx)>(
    ctx: &mut kama_ui::BuildCtx,
    panel_key: K,
    panel: Rect,
    opacity: f32,
    children: F,
) {
    kama_ui::ui!(ctx, {
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
            cursor: CursorShape::Passthrough;
            animate_interaction: false;
        }
    });
    build_panel_shell(ctx, panel_key, panel, opacity, children);
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
