use std::time::Instant;

use kama_ui::components::TextEdit;

use crate::{PendingDiscardAction, POPUP_FADE_DURATION};

pub(super) struct PopupAnimation {
    opened_at: Instant,
    closing: Option<(Instant, f32)>,
}

impl PopupAnimation {
    pub(super) fn new() -> Self {
        Self {
            opened_at: Instant::now(),
            closing: None,
        }
    }

    pub(super) fn close(&mut self) {
        if self.closing.is_none() {
            let now = Instant::now();
            self.closing = Some((now, self.opacity(now)));
        }
    }

    pub(super) fn restart(&mut self) {
        *self = Self::new();
    }

    pub(super) fn opacity(&self, now: Instant) -> f32 {
        let eased = |started: Instant| {
            let t = (now.saturating_duration_since(started).as_secs_f32()
                / POPUP_FADE_DURATION.as_secs_f32())
            .clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        };
        self.closing.map_or_else(
            || eased(self.opened_at),
            |(started, opacity)| opacity * (1.0 - eased(started)),
        )
    }

    pub(super) fn finished(&self, now: Instant) -> bool {
        self.closing.is_some_and(|(started, _)| {
            now.saturating_duration_since(started) >= POPUP_FADE_DURATION
        })
    }

    pub(super) fn is_closing(&self) -> bool {
        self.closing.is_some()
    }
    pub(super) fn is_animating(&self) -> bool {
        self.opened_at.elapsed() < POPUP_FADE_DURATION || self.closing.is_some()
    }
}

macro_rules! popup_dialog_methods {
    ($type:ty) => {
        impl $type {
            pub fn close(&mut self) {
                self.animation.close();
            }
            pub fn opacity(&self, now: Instant) -> f32 {
                self.animation.opacity(now)
            }
            pub fn finished(&self, now: Instant) -> bool {
                self.animation.finished(now)
            }
            pub fn is_closing(&self) -> bool {
                self.animation.is_closing()
            }
            pub fn is_animating(&self) -> bool {
                self.animation.is_animating()
            }
        }
    };
}

macro_rules! popup_editor_dialog_methods {
    ($type:ty, $field:ident) => {
        impl $type {
            pub fn close(&mut self) {
                self.animation.close();
                self.$field.set_focused(false);
            }
            pub fn opacity(&self, now: Instant) -> f32 {
                self.animation.opacity(now)
            }
            pub fn finished(&self, now: Instant) -> bool {
                self.animation.finished(now)
            }
            pub fn is_closing(&self) -> bool {
                self.animation.is_closing()
            }
            pub fn is_animating(&self) -> bool {
                self.animation.is_animating() || self.$field.is_animating()
            }
        }
    };
}

pub(super) struct SimpleDialog {
    pub(super) animation: PopupAnimation,
}
impl SimpleDialog {
    pub(super) fn new() -> Self {
        Self {
            animation: PopupAnimation::new(),
        }
    }
}
popup_dialog_methods!(SimpleDialog);

pub(super) struct ActionDialog {
    pub(super) action: PendingDiscardAction,
    pub(super) animation: PopupAnimation,
}
impl ActionDialog {
    pub(super) fn new(action: PendingDiscardAction) -> Self {
        Self {
            action,
            animation: PopupAnimation::new(),
        }
    }
}
popup_dialog_methods!(ActionDialog);

pub(super) struct LayoutSaveDialog {
    pub(super) editor: TextEdit,
    pub(super) animation: PopupAnimation,
}
impl LayoutSaveDialog {
    pub(super) fn new() -> Self {
        let mut editor = TextEdit::single_line("");
        editor.set_focused(true);
        Self {
            editor,
            animation: PopupAnimation::new(),
        }
    }
    pub(super) fn tick(&mut self, dt: f32) {
        self.editor.tick(dt);
    }
}
popup_editor_dialog_methods!(LayoutSaveDialog, editor);
