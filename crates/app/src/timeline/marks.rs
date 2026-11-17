use super::{
    ClipOrigin, Color, CursorShape, Drag, ElementState, Icons, Key, KeyEvent, ModifiersState,
    NamedKey, RULER_H, Rect, StackId, TimelineLayout, TimelineState, i18n, intersects, theme, ui,
    widgets,
};
use serde::{Deserialize, Serialize};

const MARK_W: f32 = 14.0;
const MARK_H: f32 = 18.0;
const CELL: f32 = 30.0;
const COLOR_BUTTON_W: f32 = 16.0;
const COLOR_BUTTON_H: f32 = 22.0;
const PRESETS: [Color; 8] = [
    Color::rgb8(239, 83, 80),
    Color::rgb8(255, 152, 55),
    Color::rgb8(255, 217, 70),
    Color::rgb8(99, 199, 112),
    Color::rgb8(65, 202, 211),
    Color::rgb8(79, 145, 245),
    Color::rgb8(172, 112, 231),
    Color::rgb8(236, 127, 183),
];

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct TimelineMark {
    pub id: u32,
    pub time: f32,
    pub color: [f32; 4],
}

#[derive(Clone, Copy, Debug)]
pub(super) struct MarkPopup {
    pub stack: StackId,
    pub point: [f32; 2],
    pub time: f32,
}

impl TimelineState {
    pub(crate) fn mark_popup_contains(
        &self,
        snapshot: &ui::dock::LayoutSnapshot,
        point: [f32; 2],
    ) -> bool {
        let Some(popup) = self
            .mark_popup
            .filter(|_| !self.mark_animation.is_closing())
        else {
            return false;
        };
        let Some(layout) = self.focused_layout(snapshot) else {
            return false;
        };
        let rect = Self::mark_popup_rect(layout, popup);
        rect.contains(point)
            || self.mark_picker.borrow().popup_contains_in(
                Self::mark_picker_anchor(rect),
                layout.rect,
                point,
            )
    }

    pub(super) fn close_mark_popup(&mut self) {
        self.mark_animation.close();
        self.mark_picker.get_mut().close();
    }

    pub(crate) fn dismiss_mark_popup_outside(
        &mut self,
        snapshot: &ui::dock::LayoutSnapshot,
        point: [f32; 2],
    ) {
        if self.mark_popup.is_some()
            && self
                .focused_layout(snapshot)
                .is_none_or(|layout| !layout.rect.contains(point))
        {
            self.close_mark_popup();
        }
    }

    pub(crate) fn sync_mark_textures(&mut self, renderer: &mut ui::Renderer) -> anyhow::Result<()> {
        self.mark_picker.get_mut().sync_textures(renderer)
    }

    pub(super) fn open_mark_popup(&mut self, point: [f32; 2], time: f32) {
        self.mark_animation.restart();
        self.mark_picker.get_mut().close();
        self.mark_picker.get_mut().set_popup_on_right(true);
        self.mark_popup = Some(MarkPopup {
            stack: self.focused_stack.unwrap_or(StackId(0)),
            point,
            time: if self.frame_snap {
                (time.max(0.0) * self.frame_rate).round() / self.frame_rate
            } else {
                time.max(0.0)
            },
        });
    }

    fn insert_mark(&mut self, color: [f32; 4]) {
        let Some(popup) = self
            .mark_popup
            .filter(|_| !self.mark_animation.is_closing())
        else {
            return;
        };
        let Some(id) = self.allocate_mark_id() else {
            return;
        };
        self.marks.push(TimelineMark {
            id,
            time: popup.time,
            color,
        });
        self.clear_selection();
        self.selected_keyframes.clear();
        self.edit.selected_marks.insert(id);
        self.close_mark_popup();
    }

    pub(super) fn allocate_mark_id(&mut self) -> Option<u32> {
        let id = self
            .marks
            .iter()
            .map(|mark| mark.id)
            .max()
            .unwrap_or(0)
            .max(self.next_mark.saturating_sub(1))
            .checked_add(1)?;
        self.next_mark = id.saturating_add(1);
        Some(id)
    }

    pub(super) fn duplicate_selected_marks(&mut self) {
        let originals = self
            .marks
            .iter()
            .filter(|mark| self.edit.selected_marks.contains(&mark.id))
            .copied()
            .collect::<Vec<_>>();
        self.edit.selected_marks.clear();
        for mark in originals {
            if let Some(id) = self.allocate_mark_id() {
                self.edit.document.marks.push(TimelineMark { id, ..mark });
                self.edit.selected_marks.insert(id);
            }
        }
    }

    pub(super) fn mark_rect(&self, layout: TimelineLayout, mark: &TimelineMark) -> Rect {
        Rect::new(
            MARK_W.mul_add(-0.5, self.time_x(layout, mark.time)),
            layout.ruler.y + RULER_H - MARK_H - 1.0,
            MARK_W,
            MARK_H,
        )
    }

    pub(super) fn mark_at(&self, layout: TimelineLayout, point: [f32; 2]) -> Option<u32> {
        if !layout.ruler.contains(point) {
            return None;
        }
        self.marks
            .iter()
            .rev()
            .find(|mark| self.mark_rect(layout, mark).inset(-2.0).contains(point))
            .map(|mark| mark.id)
    }

    pub(super) fn begin_mark_drag(
        &mut self,
        layout: TimelineLayout,
        point: [f32; 2],
        additive: bool,
        duplicate: bool,
    ) -> bool {
        let Some(id) = self.mark_at(layout, point) else {
            return false;
        };
        let was_selected = self.edit.selected_marks.contains(&id);
        let collapse = (!additive && was_selected && !duplicate).then_some(id);
        let toggle = (additive && was_selected && !duplicate).then_some(id);
        if !additive && !was_selected {
            self.clear_selection();
            self.selected_keyframes.clear();
        }
        self.edit.selected_marks.insert(id);
        let anchor = self
            .marks
            .iter()
            .find(|mark| mark.id == id)
            .map_or(0.0, |mark| mark.time);
        if duplicate {
            self.duplicate_selection();
        }
        self.drag = Some(Drag::Marks {
            start: point,
            anchor,
            collapse,
            toggle,
            preview_tracks: Vec::new(),
            keyframes: self.begin_keyframe_drag(layout, point),
            marks: self
                .marks
                .iter()
                .filter(|mark| self.edit.selected_marks.contains(&mark.id))
                .map(|mark| (mark.id, mark.time))
                .collect(),
            clips: self
                .clips
                .iter()
                .enumerate()
                .filter(|(_, clip)| self.selected.contains(&clip.id))
                .map(|(index, clip)| ClipOrigin {
                    index,
                    id: clip.id,
                    start: clip.start,
                    duration: clip.duration,
                    track: clip.track,
                    source_offset: clip.source_offset,
                    opacity: clip.opacity,
                    volume: clip.volume,
                })
                .collect(),
        });
        true
    }

    fn mark_popup_rect(layout: TimelineLayout, popup: MarkPopup) -> Rect {
        let width = CELL.mul_add(10.0, 12.0).min(layout.rect.width);
        let reserve = 308.0;
        Rect::new(
            (layout.rect.x + popup.point[0])
                .min(layout.rect.right() - width - reserve)
                .max(layout.rect.x),
            (layout.rect.y + popup.point[1] + 6.0)
                .min(layout.rect.bottom() - 40.0)
                .max(layout.rect.y),
            width,
            40.0,
        )
    }

    fn mark_color_rect(rect: Rect, index: usize) -> Rect {
        let cell = ((rect.width - 12.0) / 10.0).max(0.0);
        let width = COLOR_BUTTON_W.min(cell);
        Rect::new(
            (index as f32).mul_add(cell, (cell - width).mul_add(0.5, rect.x + 6.0)),
            (40.0 - COLOR_BUTTON_H).mul_add(0.5, rect.y),
            width,
            COLOR_BUTTON_H,
        )
    }

    fn mark_picker_anchor(rect: Rect) -> Rect {
        let button = Self::mark_color_rect(rect, PRESETS.len().saturating_add(1));
        let width = COLOR_BUTTON_H.min(((rect.width - 12.0) / 10.0).max(0.0));
        Rect::new(
            (button.width - width).mul_add(0.5, button.x),
            button.y,
            width,
            COLOR_BUTTON_H,
        )
    }

    pub(super) fn mark_popup_pressed(
        &mut self,
        layout: TimelineLayout,
        point: [f32; 2],
        modifiers: ModifiersState,
    ) -> bool {
        let Some(popup) = self
            .mark_popup
            .filter(|_| !self.mark_animation.is_closing())
        else {
            return false;
        };
        let rect = Self::mark_popup_rect(layout, popup);
        let anchor = Self::mark_picker_anchor(rect);
        if anchor.contains(point)
            || self
                .mark_picker
                .borrow()
                .popup_contains_in(anchor, layout.rect, point)
        {
            self.mark_picker
                .get_mut()
                .pointer_pressed_in(anchor, layout.rect, point, modifiers);
            return true;
        }
        for index in 0..=PRESETS.len() {
            if Self::mark_color_rect(rect, index).contains(point) {
                if let Some(color) = PRESETS.get(index) {
                    self.insert_mark(color.to_array());
                } else {
                    let color = self.mark_picker.get_mut().linear();
                    self.insert_mark(color);
                }
                return true;
            }
        }
        if !rect.contains(point) {
            self.close_mark_popup();
        }
        true
    }

    pub(super) fn mark_popup_moved(&mut self, layout: TimelineLayout, point: [f32; 2]) -> bool {
        let Some(popup) = self
            .mark_popup
            .filter(|_| !self.mark_animation.is_closing())
        else {
            return false;
        };
        let rect = Self::mark_popup_rect(layout, popup);
        self.mark_picker.get_mut().pointer_moved_in(
            Self::mark_picker_anchor(rect),
            layout.rect,
            point,
        )
    }

    pub(super) fn mark_popup_key(&mut self, event: &KeyEvent, modifiers: ModifiersState) -> bool {
        if self.mark_popup.is_none() || self.mark_animation.is_closing() {
            return false;
        }
        if event.state != ElementState::Pressed {
            return true;
        }
        match event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.close_mark_popup();
            }
            _ if self.mark_picker.borrow().is_open() => {
                self.mark_picker.get_mut().handle_key(event, modifiers);
            }
            _ => {}
        }
        true
    }

    pub(super) fn mark_picker_caret(&self, layout: TimelineLayout) -> Option<Rect> {
        let popup = self.mark_popup?;
        let rect = Self::mark_popup_rect(layout, popup);
        self.mark_picker
            .borrow()
            .caret_rect_in(Self::mark_picker_anchor(rect), layout.rect)
    }

    pub(super) fn build_marks(
        &self,
        ctx: &mut ui::BuildCtx,
        layout: TimelineLayout,
        stack: StackId,
        icons: &Icons,
    ) {
        ctx.with_clip(layout.ruler, |ctx| {
            for mark in &self.marks {
                let rect = self.mark_rect(layout, mark);
                if !intersects(rect, layout.ruler) { continue; }
                if self.edit.selected_marks.contains(&mark.id) {
                    ui::ui!(ctx, {
                        Rect(("mark-selection", stack.0, mark.id), rect.inset(-2.0)) {
                            fill: theme::popup_text(); fill_texture: icons.mark;
                        }
                    });
                }
                ui::ui!(ctx, {
                    Rect(("timeline-mark", stack.0, mark.id), rect) {
                        fill: Color::rgba(mark.color[0], mark.color[1], mark.color[2], mark.color[3]);
                        fill_texture: icons.mark; interactive; cursor: CursorShape::Pointer;
                    }
                });
            }
        });
    }

    pub(super) fn build_mark_popup(
        &self,
        ctx: &mut ui::BuildCtx,
        layout: TimelineLayout,
        stack: StackId,
        icons: &Icons,
    ) {
        let Some(popup) = self.mark_popup.filter(|popup| popup.stack == stack) else {
            return;
        };
        let rect = Self::mark_popup_rect(layout, popup);
        let opacity = self.mark_animation.opacity(std::time::Instant::now());
        crate::dialog::build_panel_shell(ctx, ("mark-popup", stack.0), rect, opacity, |_| {});
        ui::ui!(ctx, {
            Rect(("mark-popup-content", stack.0), layout.rect) {
                overlay; overflow_visible; opacity: opacity;
                @rust {
                    for index in 0..=PRESETS.len() {
                        let button = Self::mark_color_rect(rect, index);
                        let color = PRESETS.get(index).copied().unwrap_or_else(theme::popup_text);
                        let texture = if index == PRESETS.len() { icons.custom_mark } else { icons.mark };
                        ui::ui!(ctx, {
                            Rect(("mark-color-button", stack.0, index), button) {
                                fill: color; fill_texture: texture; interactive;
                                cursor: CursorShape::Pointer;
                                tooltip: i18n::text(if index == PRESETS.len() { "timeline-custom-mark-color" } else { "timeline-add-mark" });
                            }
                        });
                    }
                    self.mark_picker.borrow_mut().build_in(
                        ctx, "mark-custom-picker", Self::mark_picker_anchor(rect),
                        layout.rect, widgets::component_style(),
                    );
                }
            }
        });
    }
}
