use std::{
    collections::VecDeque,
    sync::{Mutex, OnceLock},
};

use kama_ui::{BuildCtx, Color, Rect, ScrollState, Size};

use crate::{
    assets::{AppIcon, Icons},
    i18n, theme,
    widgets::{
        ContextMenuClick, ContextMenuItem, build_context_menu, context_menu_click,
        context_menu_rect,
    },
};

const MAX_MESSAGES: usize = 500;
const ROW_HEIGHT: f32 = 46.0;
const PAD: f32 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageSeverity {
    Warning,
    Error,
}

impl MessageSeverity {
    const fn label_key(self) -> &'static str {
        match self {
            Self::Warning => "severity-warning",
            Self::Error => "severity-error",
        }
    }

    const fn color(self) -> Color {
        match self {
            Self::Warning => Color::rgb8(0xf0, 0xa2, 0x15),
            Self::Error => Color::rgb8(0xe5, 0x5b, 0x5b),
        }
    }
}

#[derive(Clone, Debug)]
pub struct MessageEntry {
    id: u64,
    severity: MessageSeverity,
    source: String,
    message: String,
    repeats: u32,
}

#[derive(Default)]
struct MessageLog {
    next_id: u64,
    entries: VecDeque<MessageEntry>,
}

fn log() -> &'static Mutex<MessageLog> {
    static LOG: OnceLock<Mutex<MessageLog>> = OnceLock::new();
    LOG.get_or_init(|| {
        Mutex::new(MessageLog {
            next_id: 1,
            entries: VecDeque::new(),
        })
    })
}

pub fn warning(source: impl Into<String>, message: impl Into<String>) {
    push(MessageSeverity::Warning, source.into(), message.into());
}

pub fn error(source: impl Into<String>, message: impl Into<String>) {
    push(MessageSeverity::Error, source.into(), message.into());
}

fn push(severity: MessageSeverity, source: String, message: String) {
    let Ok(mut log) = log().lock() else {
        return;
    };
    if let Some(index) = log.entries.iter().position(|entry| {
        entry.severity == severity && entry.source == source && entry.message == message
    }) {
        if let Some(mut existing) = log.entries.remove(index) {
            existing.repeats = existing.repeats.saturating_add(1);
            log.entries.push_back(existing);
        }
        return;
    }
    let id = log.next_id;
    log.next_id = log.next_id.wrapping_add(1).max(1);
    log.entries.push_back(MessageEntry {
        id,
        severity,
        source,
        message,
        repeats: 1,
    });
    while log.entries.len() > MAX_MESSAGES {
        log.entries.pop_front();
    }
}

fn snapshot() -> Vec<MessageEntry> {
    log()
        .lock()
        .map(|log| log.entries.iter().cloned().collect())
        .unwrap_or_default()
}

pub fn drain_for_worker() -> Vec<(MessageSeverity, String, String)> {
    let Ok(mut log) = log().lock() else {
        return Vec::new();
    };
    std::mem::take(&mut log.entries)
        .into_iter()
        .map(|entry| (entry.severity, entry.source, entry.message))
        .collect()
}

#[derive(Default)]
pub struct MessagesState {
    scroll: ScrollState,
    context_menu: Option<MessageMenu>,
    clipboard: Option<arboard::Clipboard>,
}

struct MessageMenu {
    point: [f32; 2],
    entry: MessageEntry,
}

fn copy_items(labels: &[String; 2]) -> [ContextMenuItem<'_, bool>; 2] {
    [
        ContextMenuItem::new(&labels[0], Some(AppIcon::Copy), false),
        ContextMenuItem::new(&labels[1], Some(AppIcon::Copy), true),
    ]
}

impl MessagesState {
    pub fn close_context_menu(&mut self) -> bool {
        self.context_menu.take().is_some()
    }

    pub fn pointer_right_pressed(&mut self, rect: Rect, point: [f32; 2]) {
        self.context_menu = None;
        if !rect.contains(point) {
            return;
        }
        let local = [point[0] - rect.x, point[1] - rect.y];
        for (index, entry) in snapshot().into_iter().rev().enumerate() {
            let row = Rect::new(
                PAD,
                (index as f32).mul_add(ROW_HEIGHT, PAD - self.scroll.offset),
                rect.width - 2.0 * PAD,
                ROW_HEIGHT - 2.0,
            );
            if row.contains(local) {
                self.context_menu = Some(MessageMenu {
                    point: local,
                    entry,
                });
                break;
            }
        }
    }

    pub fn pointer_pressed(&mut self, rect: Rect, point: [f32; 2]) -> bool {
        let Some(menu) = self.context_menu.take() else {
            return false;
        };
        let labels = [
            i18n::text("command-edit-copy"),
            i18n::text("messages-copy-detailed"),
        ];
        let items = copy_items(&labels);
        let menu_rect = context_menu_rect(rect, menu.point, items.len());
        if let ContextMenuClick::Action(detailed) = context_menu_click(menu_rect, point, &items) {
            let text = if detailed {
                format!(
                    "{}: {}\n{}: {}\n{}: {}\n\n{}",
                    i18n::text("messages-severity"),
                    i18n::text(menu.entry.severity.label_key()),
                    i18n::text("messages-source"),
                    menu.entry.source,
                    i18n::text("messages-occurrences"),
                    menu.entry.repeats,
                    menu.entry.message
                )
            } else {
                menu.entry.message
            };
            if self.clipboard.is_none() {
                match arboard::Clipboard::new() {
                    Ok(clipboard) => self.clipboard = Some(clipboard),
                    Err(error) => {
                        self::error("Messages clipboard", error.to_string());
                        return true;
                    }
                }
            }
            if let Some(clipboard) = &mut self.clipboard {
                if let Err(error) = clipboard.set_text(text) {
                    self::error("Messages clipboard", error.to_string());
                }
            }
        }
        menu_rect.contains(point)
    }

    pub fn scroll(&mut self, rect: Rect, point: [f32; 2], delta: [f32; 2]) -> bool {
        if !rect.contains(point) {
            return false;
        }
        let content = PAD.mul_add(2.0, snapshot().len() as f32 * ROW_HEIGHT);
        let scrolled = self
            .scroll
            .scroll_by(-delta[1], (content - rect.height).max(0.0));
        if scrolled {
            self.close_context_menu();
        }
        scrolled
    }

    pub fn build(&self, ctx: &mut BuildCtx, rect: Rect, cursor: [f32; 2], icons: &Icons) {
        let entries = snapshot();
        kama_ui::ui!(ctx, {
            Column {
                id: "messages-panel-bg";
                width: Size::Fill;
                height: Size::Fill;
                padding: PAD;
                gap: 2.0;
                fill: theme::panel();
                vertical_scroll: self.scroll;
                clip_children: true;

                @if entries.is_empty() {
                    Block {
                        id: "messages-empty";
                        width: Size::Fill;
                        height: Size::Pixels(30.0);
                        font_size: 10.5;
                        text_color: theme::muted();
                        text: i18n::text("messages-empty");
                    }
                }

                @for entry in entries.iter().rev() {
                    @let severity_color = entry.severity.color();
                    @let repeat = if entry.repeats > 1 {
                        format!(" x{}", entry.repeats)
                    } else {
                        String::new()
                    };
                    Row {
                        id: @format("message-row-{}", entry.id);
                        width: Size::Fill;
                        height: Size::Pixels(ROW_HEIGHT - 2.0);
                        gap: 6.0;
                        fill: theme::control();
                        border: 1;
                        border_color: theme::line_soft();
                        border_radius: 5.0;
                        interactive;
                        tooltip: &entry.message;

                        Block {
                            id: @format("message-accent-{}", entry.id);
                            width: Size::Pixels(3.0);
                            height: Size::Fill;
                            fill: severity_color;
                            border_radius: 2.0;
                        }
                        Column {
                            width: Size::Fill;
                            height: Size::Fill;
                            Block { width: Size::Fill; height: Size::Pixels(3.0); }
                            Block {
                                id: @format("message-source-{}", entry.id);
                                width: Size::Fill;
                                height: Size::Pixels(15.0);
                                font_size: 8.5;
                                text_color: severity_color;
                                text: format!("{}: {}{}", i18n::text(entry.severity.label_key()), entry.source, repeat);
                            }
                            Block {
                                id: @format("message-text-{}", entry.id);
                                width: Size::Fill;
                                height: Size::Pixels(22.0);
                                font_size: 10.0;
                                text_color: theme::text();
                                text: &entry.message;
                            }
                        }
                    }
                }
            }
        });
        if let Some(menu) = &self.context_menu {
            let labels = [
                i18n::text("command-edit-copy"),
                i18n::text("messages-copy-detailed"),
            ];
            let items = copy_items(&labels);
            let bounds = Rect::new(0.0, 0.0, rect.width, rect.height);
            let menu_rect = context_menu_rect(bounds, menu.point, items.len());
            build_context_menu(
                ctx,
                "messages",
                menu_rect,
                [cursor[0] - rect.x, cursor[1] - rect.y],
                &items,
                icons,
            );
        }
    }
}
