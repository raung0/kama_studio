use std::{
    fmt::Display,
    ops::Range,
    sync::{Mutex, OnceLock},
};

use arboard::Clipboard;
use cosmic_text::{
    Action, Attrs, Buffer, Change, Cursor, Edit, Editor, FontSystem, Metrics, Motion, Selection,
    Shaping, Wrap,
};
use winit::{
    event::{ElementState, Ime, KeyEvent},
    keyboard::{Key, ModifiersState, NamedKey},
};

use super::{ease, Style};
use crate::{Align, BuildCtx, Rect};

const PAD: f32 = 7.0;
const FONT: f32 = 11.0;
const LINE_H: f32 = 13.75;
const CARET_SPEED: f32 = 30.0;

#[derive(Clone, Copy, Default)]
pub struct EditResponse {
    pub handled: bool,
    pub changed: bool,
}

impl EditResponse {
    const HANDLED: Self = Self {
        handled: true,
        changed: false,
    };

    const fn changed(changed: bool) -> Self {
        Self {
            handled: true,
            changed,
        }
    }
}

pub struct TextEdit {
    editor: Editor<'static>,
    text: String,
    undo: Vec<Change>,
    redo: Vec<Change>,
    caret: [f32; 2],
    caret_target: [f32; 2],
    scroll: [f32; 2],
    viewport: [f32; 2],
    preedit_layout: Option<Buffer>,
    focused: bool,
    multiline: bool,
    drag: Option<(Rect, Cursor)>,
    clipboard: Option<Clipboard>,
    preedit: String,
    preedit_cursor: usize,
    text_scale: f32,
    ui_scale: f32,
}

impl TextEdit {
    pub fn single_line(text: impl Into<String>) -> Self {
        Self::new(text.into(), false)
    }

    pub fn multiline(text: impl Into<String>) -> Self {
        Self::new(text.into(), true)
    }

    fn new(text: String, multiline: bool) -> Self {
        let text = normalize(&text, multiline);
        let mut buffer = Buffer::new_empty(Metrics::new(FONT, LINE_H));
        buffer.set_wrap(Wrap::None);
        buffer.set_size(None, None);
        buffer.set_text(&text, &Attrs::new(), Shaping::Advanced, None);
        let cursor = end_cursor(&buffer);
        let mut editor = Editor::new(buffer);
        editor.set_cursor(cursor);
        let mut edit = Self {
            editor,
            text,
            undo: Vec::new(),
            redo: Vec::new(),
            caret: [0.0; 2],
            caret_target: [0.0; 2],
            scroll: [0.0; 2],
            viewport: [0.0; 2],
            preedit_layout: None,
            focused: false,
            multiline,
            drag: None,
            clipboard: Clipboard::new().ok(),
            preedit: String::new(),
            preedit_cursor: 0,
            text_scale: 1.0,
            ui_scale: 1.0,
        };
        edit.refresh_caret_target();
        edit.caret = edit.caret_target;
        edit
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn reset(&mut self, text: impl AsRef<str>) {
        let focused = self.focused;
        let clipboard = self.clipboard.take();
        let scales = (self.text_scale, self.ui_scale);
        *self = Self::new(normalize(text.as_ref(), self.multiline), self.multiline);
        self.focused = focused;
        self.clipboard = clipboard;
        self.set_scales(scales.0, scales.1);
    }

    pub fn set_focused(&mut self, focused: bool) {
        if self.focused == focused {
            return;
        }
        self.focused = focused;
        self.drag = None;
        self.clear_preedit();
        if !focused {
            self.editor.set_selection(Selection::None);
        }
        self.refresh_caret_target();
        if focused {
            self.ensure_caret_visible();
        }
        self.caret = self.caret_target;
    }

    #[must_use]
    pub const fn is_focused(&self) -> bool {
        self.focused
    }

    pub fn tick(&mut self, dt: f32) {
        self.refresh_caret_target();
        if self.focused {
            for axis in 0..2 {
                ease(
                    &mut self.caret[axis],
                    self.caret_target[axis],
                    CARET_SPEED,
                    dt,
                );
            }
        } else {
            self.caret = self.caret_target;
        }
    }

    #[must_use]
    pub fn is_animating(&self) -> bool {
        self.focused
            && self
                .caret
                .iter()
                .zip(self.caret_target)
                .any(|(a, b)| (a - b).abs() > 0.01)
    }

    pub fn pointer_pressed(
        &mut self,
        rect: Rect,
        point: [f32; 2],
        modifiers: ModifiersState,
    ) -> bool {
        if !rect.contains(point) {
            self.set_focused(false);
            return false;
        }
        self.set_focused(true);
        let cursor = self.cursor_from_point(rect, point);
        let anchor = if modifiers.shift_key() {
            selection_anchor(self.editor.selection()).unwrap_or(self.editor.cursor())
        } else {
            cursor
        };
        self.editor.set_cursor(cursor);
        self.editor.set_selection(if modifiers.shift_key() {
            Selection::Normal(anchor)
        } else {
            Selection::None
        });
        self.drag = Some((rect, anchor));
        self.refresh_caret_target();
        true
    }

    pub fn pointer_moved(&mut self, point: [f32; 2]) -> bool {
        let Some((rect, anchor)) = self.drag else {
            return false;
        };
        let cursor = self.cursor_from_point(rect, point);
        self.editor.set_cursor(cursor);
        self.editor.set_selection(if cursor == anchor {
            Selection::None
        } else {
            Selection::Normal(anchor)
        });
        self.refresh_caret_target();
        true
    }

    pub const fn pointer_released(&mut self) -> bool {
        self.drag.take().is_some()
    }

    pub fn scroll(&mut self, rect: Rect, delta: [f32; 2]) -> bool {
        self.sync_layout();
        self.set_viewport(rect);
        let content = self.content_size();
        let max = [
            (content[0] - self.viewport[0]).max(0.0),
            (content[1] - self.viewport[1]).max(0.0),
        ];
        let delta = if self.multiline {
            delta
        } else {
            [
                if delta[0].abs() > 0.001 {
                    delta[0]
                } else {
                    delta[1]
                },
                0.0,
            ]
        };
        let old = self.scroll;
        for axis in 0..2 {
            self.scroll[axis] = (self.scroll[axis] - delta[axis]).clamp(0.0, max[axis]);
        }
        old.iter()
            .zip(self.scroll)
            .any(|(a, b)| (a - b).abs() > 0.001)
    }

    pub fn handle_ime(&mut self, event: &Ime) -> EditResponse {
        if !self.focused {
            return EditResponse::default();
        }
        let response = match event {
            Ime::Enabled => EditResponse::HANDLED,
            Ime::Disabled => {
                self.clear_preedit();
                EditResponse::HANDLED
            }
            Ime::Preedit(text, cursor) => {
                self.preedit = normalize(text, self.multiline);
                self.preedit_cursor = cursor.map_or_else(
                    || self.preedit.chars().count(),
                    |(start, _)| char_index_at_byte(&self.preedit, start),
                );
                self.preedit_layout = None;
                EditResponse::HANDLED
            }
            Ime::Commit(text) => {
                self.clear_preedit();
                if !self.multiline
                    && text
                        .chars()
                        .any(|character| matches!(character, '\r' | '\n'))
                {
                    let committed = text.split(['\r', '\n']).next().unwrap_or_default();
                    let changed = !committed.is_empty()
                        && self.edit(|editor, _| editor.insert_string(committed, None));
                    self.set_focused(false);
                    EditResponse::changed(changed)
                } else {
                    let text = normalize(text, self.multiline);
                    EditResponse::changed(self.edit(|editor, _| editor.insert_string(&text, None)))
                }
            }
        };
        self.refresh_caret_target();
        response
    }

    pub fn handle_key(&mut self, event: &KeyEvent, modifiers: ModifiersState) -> EditResponse {
        if !self.focused || event.state != ElementState::Pressed {
            return EditResponse::default();
        }
        if matches!(event.logical_key, Key::Named(NamedKey::Process)) || !self.preedit.is_empty() {
            return EditResponse::HANDLED;
        }
        let shift = modifiers.shift_key();
        let command = modifiers.super_key() || modifiers.control_key();
        let response = match &event.logical_key {
            Key::Named(NamedKey::Escape) if self.multiline => {
                self.set_focused(false);
                EditResponse::HANDLED
            }
            Key::Named(NamedKey::Enter) if !self.multiline => {
                self.set_focused(false);
                EditResponse::HANDLED
            }
            Key::Named(key) if self.navigate(key, modifiers) => EditResponse::HANDLED,
            Key::Named(NamedKey::Backspace) => EditResponse::changed(self.delete(false, modifiers)),
            Key::Named(NamedKey::Delete) => EditResponse::changed(self.delete(true, modifiers)),
            Key::Named(NamedKey::Copy) => {
                self.copy();
                EditResponse::HANDLED
            }
            Key::Named(NamedKey::Cut) => EditResponse::changed(self.cut()),
            Key::Named(NamedKey::Paste) => EditResponse::changed(self.paste()),
            Key::Named(NamedKey::Undo) => EditResponse::changed(self.undo()),
            Key::Named(NamedKey::Redo) => EditResponse::changed(self.redo()),
            Key::Named(NamedKey::Enter) if self.multiline => {
                EditResponse::changed(self.action(Action::Enter))
            }
            Key::Named(NamedKey::Tab) if self.multiline && !command => {
                EditResponse::changed(self.edit(|editor, _| editor.insert_string("\t", None)))
            }
            Key::Character(text) if command => match text.as_str().to_ascii_lowercase().as_str() {
                "a" => {
                    self.select_all();
                    EditResponse::HANDLED
                }
                "c" => {
                    self.copy();
                    EditResponse::HANDLED
                }
                "x" => EditResponse::changed(self.cut()),
                "v" => EditResponse::changed(self.paste()),
                "w" => EditResponse::changed(self.delete_word(false)),
                "z" => EditResponse::changed(if shift { self.redo() } else { self.undo() }),
                "y" => EditResponse::changed(self.redo()),
                _ => EditResponse::default(),
            },
            _ if !command => {
                let Some(text) = event.text.as_deref() else {
                    return EditResponse::default();
                };
                let text = normalize(text, self.multiline);
                EditResponse::changed(
                    text.chars().all(|c| !c.is_control())
                        && self.edit(|editor, _| editor.insert_string(&text, None)),
                )
            }
            _ => EditResponse::default(),
        };
        if response.handled {
            self.refresh_caret_target();
        }
        response
    }

    fn navigate(&mut self, key: &NamedKey, modifiers: ModifiersState) -> bool {
        let command = modifiers.super_key() || modifiers.control_key();
        let motion = match key {
            NamedKey::ArrowLeft => {
                if modifiers.super_key() {
                    Motion::Home
                } else if modifiers.alt_key() || modifiers.control_key() {
                    Motion::LeftWord
                } else {
                    Motion::Left
                }
            }
            NamedKey::ArrowRight => {
                if modifiers.super_key() {
                    Motion::End
                } else if modifiers.alt_key() || modifiers.control_key() {
                    Motion::RightWord
                } else {
                    Motion::Right
                }
            }
            NamedKey::ArrowUp if self.multiline => {
                if modifiers.super_key() {
                    Motion::BufferStart
                } else {
                    Motion::Up
                }
            }
            NamedKey::ArrowDown if self.multiline => {
                if modifiers.super_key() {
                    Motion::BufferEnd
                } else {
                    Motion::Down
                }
            }
            NamedKey::Home => {
                if command {
                    Motion::BufferStart
                } else {
                    Motion::Home
                }
            }
            NamedKey::End => {
                if command {
                    Motion::BufferEnd
                } else {
                    Motion::End
                }
            }
            _ => return false,
        };
        let shift = modifiers.shift_key();
        if shift && self.editor.selection() == Selection::None {
            self.editor
                .set_selection(Selection::Normal(self.editor.cursor()));
        } else if !shift {
            if let Some((start, end)) = self.editor.selection_bounds() {
                if matches!(key, NamedKey::ArrowLeft | NamedKey::ArrowRight) {
                    self.editor.set_cursor(if *key == NamedKey::ArrowLeft {
                        start
                    } else {
                        end
                    });
                    self.editor.set_selection(Selection::None);
                    return true;
                }
            }
            self.editor.set_selection(Selection::None);
        }
        with_font(|font| self.editor.action(font, Action::Motion(motion)));
        true
    }

    fn delete(&mut self, forward: bool, modifiers: ModifiersState) -> bool {
        if modifiers.super_key() {
            self.delete_motion(if forward { Motion::End } else { Motion::Home }, forward)
        } else if modifiers.alt_key() || modifiers.control_key() {
            self.delete_word(forward)
        } else {
            self.action(if forward {
                Action::Delete
            } else {
                Action::Backspace
            })
        }
    }

    fn delete_word(&mut self, forward: bool) -> bool {
        self.delete_motion(
            if forward {
                Motion::NextWord
            } else {
                Motion::PreviousWord
            },
            forward,
        )
    }

    fn delete_motion(&mut self, motion: Motion, forward: bool) -> bool {
        if self.editor.selection_bounds().is_some() {
            return self.action(Action::Backspace);
        }
        self.edit(|editor, font| {
            let original = editor.cursor();
            editor.action(font, Action::Motion(motion));
            let target = editor.cursor();
            let (start, end) = if forward {
                (original, target)
            } else {
                (target, original)
            };
            editor.set_cursor(start);
            editor.delete_range(start, end);
        })
    }

    fn action(&mut self, action: Action) -> bool {
        self.edit(|editor, font| editor.action(font, action))
    }

    fn edit(&mut self, action: impl FnOnce(&mut Editor<'static>, &mut FontSystem)) -> bool {
        self.editor.start_change();
        with_font(|font| action(&mut self.editor, font));
        let changed = self.editor.finish_change().is_some_and(|change| {
            if change.items.is_empty() {
                return false;
            }
            if self.undo.len() == 128 {
                self.undo.remove(0);
            }
            self.undo.push(change);
            self.redo.clear();
            true
        });
        if changed {
            self.sync_text();
        }
        changed
    }

    fn undo(&mut self) -> bool {
        let Some(change) = self.undo.pop() else {
            return false;
        };
        let mut reverse = change.clone();
        reverse.reverse();
        let changed = self.editor.apply_change(&reverse);
        if changed {
            self.redo.push(change);
            self.sync_text();
        }
        changed
    }

    fn redo(&mut self) -> bool {
        let Some(change) = self.redo.pop() else {
            return false;
        };
        let changed = self.editor.apply_change(&change);
        if changed {
            self.undo.push(change);
            self.sync_text();
        }
        changed
    }

    fn copy(&mut self) {
        let Some(text) = self.editor.copy_selection() else {
            return;
        };
        if self.clipboard.is_none() {
            self.clipboard = Clipboard::new().ok();
        }
        if let Some(clipboard) = self.clipboard.as_mut() {
            let _ = clipboard.set_text(text);
        }
    }

    fn cut(&mut self) -> bool {
        if self.editor.selection_bounds().is_none() {
            return false;
        }
        self.copy();
        self.action(Action::Backspace)
    }

    fn paste(&mut self) -> bool {
        if self.clipboard.is_none() {
            self.clipboard = Clipboard::new().ok();
        }
        let Some(text) = self
            .clipboard
            .as_mut()
            .and_then(|clipboard| clipboard.get_text().ok())
        else {
            return false;
        };
        let text = normalize(&text, self.multiline);
        self.edit(|editor, _| editor.insert_string(&text, None))
    }

    fn select_all(&mut self) {
        let (start, end) = self
            .editor
            .with_buffer(|buffer| (Cursor::default(), end_cursor(buffer)));
        self.editor.set_selection(Selection::Normal(start));
        self.editor.set_cursor(end);
    }

    #[must_use]
    pub fn caret_rect(&self, rect: Rect) -> Rect {
        Rect::new(
            rect.x + self.pad() + self.caret[0] - self.scroll[0],
            rect.y + self.text_y(rect.height) + self.caret[1] - self.scroll[1],
            2.0 * self.ui_scale,
            self.line_height(),
        )
    }

    pub fn build(
        &mut self,
        ctx: &mut BuildCtx,
        id: impl Display,
        rect: Rect,
        placeholder: &str,
        style: Style,
    ) {
        self.set_scales(style.text_scale, style.ui_scale);
        self.sync_layout();
        let viewport_changed = self.set_viewport(rect);
        let target = self.layout_caret_target();
        let caret_changed = target
            .iter()
            .zip(self.caret_target)
            .any(|(a, b)| (a - b).abs() > 0.001);
        self.caret_target = target;
        if viewport_changed || caret_changed {
            self.ensure_caret_visible();
        }
        let content = self.content_size();
        let text_x = self.pad() - self.scroll[0];
        let text_y = self.text_y(rect.height) - self.scroll[1];
        let text_width = content[0].max(self.viewport[0]);
        let text_height = content[1].max(self.viewport[1]).max(self.line_height());
        let font_size = self.font_size();
        let caret_x = self.pad() + self.caret[0] - self.scroll[0];
        let caret_y = self.text_y(rect.height) + self.caret[1] - self.scroll[1];
        let caret_width = 1.5 * self.ui_scale;
        let line_height = self.line_height();
        let underline_height = self.ui_scale.max(0.5);
        crate::ui!(ctx, {
            Block {
                id: @format("text-edit {}", id);
                bounds: (rect.x, rect.y, rect.width, rect.height);
                fill: if self.focused { style.focused } else { style.control };
                border: 1;
                border_color: if self.focused { style.accent } else { style.border };
                border_radius: style.radius_sm;
                reveal;

                @let local = Rect::new(0.0, 0.0, rect.width, rect.height);
                @if self.focused {
                    @for selection in self.selection_rects(local) {
                        Block {
                            bounds: (selection.x, selection.y, selection.width, selection.height);
                            fill: style.accent;
                            opacity: 0.32;
                        }
                    }
                }
                @let display = self.display_text();
                @let empty = display.is_empty();
                Block {
                    position: (text_x, text_y);
                    width: crate::Size::Pixels(text_width);
                    height: crate::Size::Pixels(text_height);
                    font_size: font_size;
                    no_wrap;
                    text_vertical_align: Align::Start;
                    text_color: if empty { style.muted } else { style.text };
                    text: if empty { placeholder.to_string() } else { display };
                }
                @if self.focused && !self.preedit.is_empty() {
                    @for preedit in self.preedit_rects(local) {
                        Block {
                            bounds: (preedit.x, preedit.bottom() - underline_height, preedit.width.max(underline_height), underline_height);
                            fill: style.accent;
                        }
                    }
                }
                @if self.focused {
                    Block {
                        id: @format("text-caret {}", id);
                        bounds: (caret_x, caret_y, caret_width, line_height);
                        fill: style.text;
                    }
                }
            }
        });
    }

    fn set_scales(&mut self, text_scale: f32, ui_scale: f32) {
        let text_scale = text_scale.clamp(0.25, 4.0);
        let ui_scale = ui_scale.clamp(0.25, 4.0);
        let text_changed = (self.text_scale - text_scale).abs() > 0.001;
        let ui_changed = (self.ui_scale - ui_scale).abs() > 0.001;
        if !text_changed && !ui_changed {
            return;
        }
        self.text_scale = text_scale;
        self.ui_scale = ui_scale;
        if text_changed {
            let metrics = Metrics::new(self.font_size(), self.line_height());
            self.editor
                .with_buffer_mut(|buffer| buffer.set_metrics(metrics));
            self.preedit_layout = None;
        }
        self.refresh_caret_target();
        self.caret = self.caret_target;
    }

    fn pad(&self) -> f32 {
        PAD * self.ui_scale
    }

    fn font_size(&self) -> f32 {
        FONT * self.text_scale
    }

    fn line_height(&self) -> f32 {
        LINE_H * self.text_scale
    }

    fn sync_text(&mut self) {
        self.text = self.editor.with_buffer(|buffer| {
            buffer
                .lines
                .iter()
                .map(cosmic_text::BufferLine::text)
                .collect::<Vec<_>>()
                .join("\n")
        });
        self.preedit_layout = None;
    }

    fn clear_preedit(&mut self) {
        self.preedit.clear();
        self.preedit_cursor = 0;
        self.preedit_layout = None;
    }

    fn display_text(&self) -> String {
        if self.preedit.is_empty() {
            return self.text.clone();
        }
        let mut text = self.text.clone();
        let range = self.composition_range();
        let bytes = char_byte(&text, range.start)..char_byte(&text, range.end);
        text.replace_range(bytes, &self.preedit);
        text
    }

    fn composition_range(&self) -> Range<usize> {
        self.editor.selection_bounds().map_or_else(
            || {
                let cursor = global_index(&self.text, self.editor.cursor());
                cursor..cursor
            },
            |(start, end)| global_index(&self.text, start)..global_index(&self.text, end),
        )
    }

    fn sync_layout(&mut self) {
        if self.preedit.is_empty() {
            with_font(|font| self.editor.shape_as_needed(font, false));
            self.preedit_layout = None;
        } else {
            let display = self.display_text();
            let metrics = Metrics::new(self.font_size(), self.line_height());
            let layout = self.preedit_layout.get_or_insert_with(|| {
                let mut buffer = Buffer::new_empty(metrics);
                buffer.set_wrap(Wrap::None);
                buffer.set_size(None, None);
                buffer
            });
            layout.set_text(&display, &Attrs::new(), Shaping::Advanced, None);
            with_font(|font| layout.shape_until_scroll(font, false));
        }
    }

    fn with_layout<T>(&self, f: impl FnOnce(&Buffer) -> T) -> T {
        if let Some(layout) = self.preedit_layout.as_ref() {
            f(layout)
        } else {
            self.editor.with_buffer(f)
        }
    }

    fn refresh_caret_target(&mut self) {
        self.sync_layout();
        let target = self.layout_caret_target();
        let changed = target
            .iter()
            .zip(self.caret_target)
            .any(|(a, b)| (a - b).abs() > 0.001);
        self.caret_target = target;
        if changed {
            self.ensure_caret_visible();
        }
    }

    fn set_viewport(&mut self, rect: Rect) -> bool {
        let pad = self.pad();
        let line_height = self.line_height();
        let viewport = [
            pad.mul_add(-2.0, rect.width).max(0.0),
            if self.multiline {
                pad.mul_add(-2.0, rect.height).max(0.0)
            } else {
                rect.height.max(line_height)
            },
        ];
        let changed = viewport
            .iter()
            .zip(self.viewport)
            .any(|(a, b)| (a - b).abs() > 0.001);
        self.viewport = viewport;
        changed
    }

    fn content_size(&self) -> [f32; 2] {
        self.with_layout(|layout| {
            layout
                .layout_runs()
                .fold([0.0_f32, self.line_height()], |size, run| {
                    [
                        size[0].max(run.line_w + 2.0),
                        size[1].max(run.line_top + run.line_height),
                    ]
                })
        })
    }

    fn ensure_caret_visible(&mut self) {
        if self.viewport[0] <= 0.0 || self.viewport[1] <= 0.0 {
            return;
        }
        let old = self.scroll;
        let content = self.content_size();
        let max = [
            (content[0] - self.viewport[0]).max(0.0),
            (content[1] - self.viewport[1]).max(0.0),
        ];
        for (axis, max) in max.into_iter().enumerate() {
            let extent = if axis == 0 {
                2.0 * self.ui_scale
            } else {
                self.line_height()
            };
            if self.caret_target[axis] < self.scroll[axis] {
                self.scroll[axis] = self.caret_target[axis];
            } else if self.caret_target[axis] + extent > self.scroll[axis] + self.viewport[axis] {
                self.scroll[axis] = self.caret_target[axis] + extent - self.viewport[axis];
            }
            self.scroll[axis] = self.scroll[axis].clamp(0.0, max);
        }
        if old
            .iter()
            .zip(self.scroll)
            .any(|(a, b)| (a - b).abs() > 0.001)
        {
            self.caret = self.caret_target;
        }
    }

    fn layout_caret_target(&self) -> [f32; 2] {
        if let Some(layout) = self.preedit_layout.as_ref() {
            let cursor = self.composition_range().start
                + self.preedit_cursor.min(self.preedit.chars().count());
            layout
                .cursor_position(&cosmic_cursor(&self.display_text(), cursor))
                .map_or([0.0; 2], |(x, y)| [x, y])
        } else {
            self.editor
                .cursor_position()
                .map_or([0.0; 2], |(x, y)| [x as f32, y as f32])
        }
    }

    fn text_y(&self, height: f32) -> f32 {
        if self.multiline {
            self.pad()
        } else {
            ((height - self.line_height()) * 0.5).max(0.0)
        }
    }

    fn cursor_from_point(&mut self, rect: Rect, point: [f32; 2]) -> Cursor {
        self.sync_layout();
        self.set_viewport(rect);
        let x = point[0] - rect.x - self.pad() + self.scroll[0];
        let y = point[1] - rect.y - self.text_y(rect.height) + self.scroll[1];
        self.editor
            .with_buffer(|buffer| buffer.hit(x, y))
            .unwrap_or(self.editor.cursor())
    }

    fn range_rects(&self, rect: Rect, layout: &Buffer, start: Cursor, end: Cursor) -> Vec<Rect> {
        layout
            .layout_runs()
            .flat_map(|run| {
                let y = rect.y + self.text_y(rect.height) + run.line_top - self.scroll[1];
                run.highlight(start, end)
                    .map(move |(x, width)| {
                        Rect::new(
                            rect.x + self.pad() + x - self.scroll[0],
                            y,
                            width,
                            run.line_height,
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn selection_rects(&self, rect: Rect) -> Vec<Rect> {
        if !self.preedit.is_empty() {
            return Vec::new();
        }
        self.editor
            .selection_bounds()
            .map_or_else(Vec::new, |(start, end)| {
                self.editor
                    .with_buffer(|layout| self.range_rects(rect, layout, start, end))
            })
    }

    fn preedit_rects(&self, rect: Rect) -> Vec<Rect> {
        let Some(layout) = self.preedit_layout.as_ref() else {
            return Vec::new();
        };
        let display = self.display_text();
        let start = self.composition_range().start;
        self.range_rects(
            rect,
            layout,
            cosmic_cursor(&display, start),
            cosmic_cursor(&display, start + self.preedit.chars().count()),
        )
    }
}

fn normalize(text: &str, multiline: bool) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    if multiline {
        text
    } else {
        text.replace(['\n', '\t'], " ")
    }
}

fn end_cursor(buffer: &Buffer) -> Cursor {
    let line = buffer.lines.len().saturating_sub(1);
    Cursor::new(line, buffer.lines[line].text().len())
}

const fn selection_anchor(selection: Selection) -> Option<Cursor> {
    match selection {
        Selection::Normal(cursor) | Selection::Line(cursor) | Selection::Word(cursor) => {
            Some(cursor)
        }
        Selection::None => None,
    }
}

fn line_ranges(text: &str) -> impl Iterator<Item = Range<usize>> + '_ {
    let mut start = 0;
    text.split('\n').map(move |line| {
        let end = start + line.chars().count();
        let range = start..end;
        start = end + 1;
        range
    })
}

fn cosmic_cursor(text: &str, index: usize) -> Cursor {
    let len = text.chars().count();
    let (line, range) = line_ranges(text)
        .enumerate()
        .find(|(_, range)| range.contains(&index) || index == range.end)
        .unwrap_or((text.matches('\n').count(), len..len));
    let line_start = char_byte(text, range.start);
    Cursor::new(line, char_byte(text, index.min(range.end)) - line_start)
}

fn global_index(text: &str, cursor: Cursor) -> usize {
    line_ranges(text)
        .take(cursor.line)
        .map(|range| range.len() + 1)
        .sum::<usize>()
        + text
            .lines()
            .nth(cursor.line)
            .map_or(0, |line| char_index_at_byte(line, cursor.index))
}

fn char_byte(text: &str, index: usize) -> usize {
    text.char_indices()
        .nth(index)
        .map_or(text.len(), |(byte, _)| byte)
}

fn char_index_at_byte(text: &str, byte: usize) -> usize {
    text.char_indices()
        .take_while(|(index, _)| *index < byte)
        .count()
}

fn with_font<T>(f: impl FnOnce(&mut FontSystem) -> T) -> T {
    static FONT_SYSTEM: OnceLock<Mutex<FontSystem>> = OnceLock::new();
    let mut font = FONT_SYSTEM
        .get_or_init(|| Mutex::new(FontSystem::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    f(&mut font)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_line_ime_newline_confirms_without_inserting_space() {
        let mut edit = TextEdit::single_line("hello");
        edit.set_focused(true);
        let response = edit.handle_ime(&Ime::Commit("\n".into()));
        assert!(response.handled);
        assert!(!response.changed);
        assert_eq!(edit.text(), "hello");
        assert!(!edit.is_focused());
    }

    #[test]
    fn horizontal_scroll_keeps_caret_visible() {
        let mut edit = TextEdit::single_line("abcdefghijklmnopqrstuvwxyz");
        let rect = Rect::new(0.0, 0.0, 40.0, 24.0);
        edit.set_viewport(rect);
        edit.ensure_caret_visible();
        assert!(edit.scroll[0] > 0.0);
        assert!(edit.caret_target[0] >= edit.scroll[0]);
        assert!(edit.caret_target[0] + 2.0 <= edit.scroll[0] + edit.viewport[0] + 0.001);
    }

    #[test]
    fn vertical_scroll_keeps_caret_visible() {
        let mut edit = TextEdit::multiline("one\ntwo\nthree\nfour\nfive");
        let rect = Rect::new(0.0, 0.0, 80.0, 30.0);
        edit.set_viewport(rect);
        edit.ensure_caret_visible();
        assert!(edit.scroll[1] > 0.0);
        assert!(edit.caret_target[1] >= edit.scroll[1]);
        assert!(edit.caret_target[1] + LINE_H <= edit.scroll[1] + edit.viewport[1] + 0.001);
    }

    #[test]
    fn manual_scroll_limits_do_not_depend_on_caret() {
        let mut edit = TextEdit::single_line("abcdefghijklmnopqrstuvwxyz");
        let rect = Rect::new(0.0, 0.0, 40.0, 24.0);
        edit.set_viewport(rect);
        edit.editor.set_cursor(Cursor::default());
        edit.refresh_caret_target();
        edit.scroll(rect, [0.0, -10_000.0]);
        let max_scroll = (edit.content_size()[0] - edit.viewport[0]).max(0.0);
        assert!((edit.scroll[0] - max_scroll).abs() < 0.001);
    }
}
