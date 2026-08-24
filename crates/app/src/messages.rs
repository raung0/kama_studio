use std::{
    collections::VecDeque,
    sync::{Mutex, OnceLock},
};

use kama_ui::{BuildCtx, Color, Rect, ScrollState, Size};

use crate::{i18n, theme};

const MAX_MESSAGES: usize = 500;
const ROW_HEIGHT: f32 = 46.0;
const PAD: f32 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageSeverity {
    Warning,
    Error,
}

impl MessageSeverity {
    fn label_key(self) -> &'static str {
        match self {
            Self::Warning => "severity-warning",
            Self::Error => "severity-error",
        }
    }

    fn color(self) -> Color {
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

#[derive(Default)]
pub struct MessagesState {
    scroll: ScrollState,
}

impl MessagesState {
    pub fn scroll(&mut self, rect: Rect, point: [f32; 2], delta: [f32; 2]) -> bool {
        if !rect.contains(point) {
            return false;
        }
        let content = snapshot().len() as f32 * ROW_HEIGHT + PAD * 2.0;
        self.scroll
            .scroll_by(-delta[1], (content - rect.height).max(0.0))
    }

    pub fn build(&self, ctx: &mut BuildCtx, _rect: Rect) {
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
    }
}
