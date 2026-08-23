use std::{cell::Cell, fmt::Display};

use crate::{Align, BuildCtx, Color, IconId, Rect, ScrollState, Size};

use super::{ease, Style};

const OPTION_H: f32 = 26.0;
const MAX_VISIBLE_OPTIONS: usize = 10;
const SPEED: f32 = 18.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ComboBoxOpenDirection {
    #[default]
    Down,
    Up,
}

pub struct ComboBox {
    selected: usize,
    open: bool,
    t: f32,
    open_direction: ComboBoxOpenDirection,
    scroll: Cell<f32>,
    ui_scale: Cell<f32>,
}

impl ComboBox {
    pub fn new(selected: usize) -> Self {
        Self {
            selected,
            open: false,
            t: 0.0,
            open_direction: ComboBoxOpenDirection::Down,
            scroll: Cell::new(0.0),
            ui_scale: Cell::new(1.0),
        }
    }

    pub fn open_direction(mut self, direction: ComboBoxOpenDirection) -> Self {
        self.open_direction = direction;
        self
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn set_selected(&mut self, selected: usize) {
        self.selected = selected;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
    }

    pub fn tick(&mut self, dt: f32) {
        ease(&mut self.t, self.open as u8 as f32, SPEED, dt);
    }

    pub fn is_animating(&self) -> bool {
        (self.t - self.open as u8 as f32).abs() > 0.001
    }

    pub fn option_at(&self, rect: Rect, point: [f32; 2], len: usize) -> Option<usize> {
        if !self.open {
            return None;
        }
        let popup = self.popup_rect(rect, len)?;
        if !popup.contains(point) {
            return None;
        }
        self.option_rects(popup, len)
            .into_iter()
            .position(|rect| rect.contains(point))
    }

    pub fn popup_contains(&self, rect: Rect, point: [f32; 2], len: usize) -> bool {
        self.open
            && self
                .popup_rect(rect, len)
                .is_some_and(|popup| popup.contains(point))
    }

    pub fn scroll(&self, rect: Rect, point: [f32; 2], delta: [f32; 2], len: usize) -> bool {
        let Some(popup) = self
            .popup_rect(rect, len)
            .filter(|popup| popup.contains(point))
        else {
            return false;
        };
        let scale = self.ui_scale.get().clamp(0.25, 4.0);
        let max = (len.saturating_sub(MAX_VISIBLE_OPTIONS) as f32 * OPTION_H * scale).max(0.0);
        let axis = if delta[1].abs() >= delta[0].abs() {
            delta[1]
        } else {
            delta[0]
        };
        self.scroll.set((self.scroll.get() - axis).clamp(0.0, max));
        popup.height > 0.0
    }

    pub fn select(&mut self, index: usize, close: bool) {
        self.selected = index;
        if close {
            self.open = false;
        }
    }

    pub fn build(
        &self,
        ctx: &mut BuildCtx,
        id: impl Display,
        rect: Rect,
        options: &[&str],
        chevron: IconId,
        style: Style,
    ) {
        let id = id.to_string();
        self.build_control(ctx, &id, rect, options, chevron, style);
        self.build_popup(ctx, &id, rect, options, style);
    }

    pub fn build_control(
        &self,
        ctx: &mut BuildCtx,
        id: impl Display,
        rect: Rect,
        options: &[&str],
        chevron: IconId,
        style: Style,
    ) {
        let selected = self.selected.min(options.len().saturating_sub(1));
        let label = options.get(selected).copied().unwrap_or_default();
        let ui_scale = style.ui_scale.clamp(0.25, 4.0);
        self.ui_scale.set(ui_scale);
        crate::ui!(ctx, {
            Row {
                id: @format("combobox {}", id);
                bounds: (rect.x, rect.y, rect.width, rect.height);
                padding: 0.0;
                align_items: Align::Center;
                fill: if self.open { style.focused } else { style.control };
                border: 1;
                border_color: if self.open { style.accent } else { style.border };
                border_radius: style.radius_sm;
                interactive;

                HSpacer {
                    width: Size::Pixels(5.0 * ui_scale);
                }
                Block {
                    id: @format("combobox-label {}", id);
                    width: Size::Fill;
                    height: Size::Fill;
                    font_size: 11.0 * style.text_scale;
                    text_color: style.text;
                    text: label;
                }
                HSpacer {
                    width: Size::Pixels(3.0 * ui_scale);
                }
                Icon {
                    id: @format("combobox-chevron {}", &id);
                    icon!: chevron;
                    color!: style.muted;
                    texture_rotation: std::f32::consts::FRAC_PI_2
                        - std::f32::consts::PI * self.t;
                    width: Size::Pixels(16.0 * ui_scale);
                    height: Size::Pixels(16.0 * ui_scale);
                }
                HSpacer {
                    width: Size::Pixels(5.0 * ui_scale);
                }
            }
        });
    }

    pub fn build_popup(
        &self,
        ctx: &mut BuildCtx,
        id: impl Display,
        rect: Rect,
        options: &[&str],
        style: Style,
    ) {
        let selected = self.selected.min(options.len().saturating_sub(1));
        let ui_scale = style.ui_scale.clamp(0.25, 4.0);
        self.ui_scale.set(ui_scale);
        let Some(popup) = self.popup_rect(rect, options.len()) else {
            return;
        };
        crate::ui!(ctx, {
            Block {
                id: @format("combobox-popup {}", id);
                overlay;
                bounds: (popup.x, popup.y, popup.width, popup.height);
                fill: Color::TRANSPARENT;
                backdrop_blur: 22.0;
                backdrop_tint: crate::theme_popup_tint();
                border: 1;
                border_color: style.accent;
                border_radius: style.radius_md;
                padding: 0.0;
                opacity: self.t;
                vertical_scroll: ScrollState { offset: self.scroll.get() };

                @for (index, option) in options.iter().copied().enumerate() {
                    Row {
                        id: @format("combobox-option {} {}", id, index);
                        width: Size::Fill;
                        height: Size::Pixels(OPTION_H * ui_scale);
                        fill: if index == selected { style.accent } else { Color::TRANSPARENT };
                        border_radius: style.radius_sm;
                        padding: 0.0;
                        align_items: Align::Center;
                        interactive;

                        HSpacer {
                            width: Size::Pixels(6.0 * ui_scale);
                        }
                        Block {
                            width: Size::Fill;
                            height: Size::Fill;
                            font_size: 10.5 * style.text_scale;
                            text_color: if index == selected { style.accent_text } else { style.text };
                            text: option;
                        }
                        HSpacer {
                            width: Size::Pixels(6.0 * ui_scale);
                        }
                    }
                }
            }
        });
    }

    fn popup_rect(&self, rect: Rect, len: usize) -> Option<Rect> {
        if self.t <= 0.001 {
            return None;
        }
        let scale = self.ui_scale.get().clamp(0.25, 4.0);
        let visible = len.min(MAX_VISIBLE_OPTIONS);
        let max_scroll = len.saturating_sub(visible) as f32 * OPTION_H * scale;
        self.scroll.set(self.scroll.get().clamp(0.0, max_scroll));
        let height = visible as f32 * OPTION_H * scale * self.t;
        let y = match self.open_direction {
            ComboBoxOpenDirection::Down => {
                rect.bottom() + 4.0 * scale - (1.0 - self.t) * 5.0 * scale
            }
            ComboBoxOpenDirection::Up => {
                rect.y - 4.0 * scale - height + (1.0 - self.t) * 5.0 * scale
            }
        };
        Some(Rect::new(rect.x, y, rect.width, height))
    }

    fn option_rects(&self, popup: Rect, len: usize) -> Vec<Rect> {
        let scale = self.ui_scale.get().clamp(0.25, 4.0);
        let (ids, measured) = crate::measure_layout(popup, |ctx| {
            let mut ids = Vec::with_capacity(len);
            ctx.new()
                .column()
                .width(Size::Fill)
                .height(Size::Fill)
                .vertical_scroll(ScrollState {
                    offset: self.scroll.get(),
                })
                .children(|ctx| {
                    for _ in 0..len {
                        ids.push(
                            ctx.new()
                                .width(Size::Fill)
                                .height(Size::Pixels(OPTION_H * scale))
                                .build(),
                        );
                    }
                })
                .build();
            ids
        });
        ids.into_iter()
            .map(|id| measured.rect(id).expect("combobox option layout"))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::ComboBox;

    #[test]
    fn popup_can_open_upward() {
        use super::ComboBoxOpenDirection;
        use crate::Rect;

        let mut combo = ComboBox::new(0).open_direction(ComboBoxOpenDirection::Up);
        combo.toggle();
        combo.t = 1.0;
        let control = Rect::new(10.0, 100.0, 120.0, 28.0);
        let popup = combo.popup_rect(control, 3).expect("open popup");
        assert!(popup.bottom() < control.y);
    }

    #[test]
    fn popup_height_is_capped() {
        use crate::Rect;

        let mut combo = ComboBox::new(0);
        combo.toggle();
        combo.t = 1.0;
        let popup = combo
            .popup_rect(Rect::new(10.0, 20.0, 120.0, 28.0), 100)
            .expect("open popup");
        assert_eq!(
            popup.height,
            super::MAX_VISIBLE_OPTIONS as f32 * super::OPTION_H
        );
    }

    #[test]
    fn scrolled_popup_hit_tests_visible_option() {
        use crate::Rect;

        let mut combo = ComboBox::new(0);
        combo.toggle();
        combo.t = 1.0;
        let control = Rect::new(10.0, 20.0, 120.0, 28.0);
        let popup = combo.popup_rect(control, 12).expect("open popup");
        assert!(combo.scroll(control, [popup.x + 4.0, popup.y + 4.0], [0.0, -52.0], 12));
        assert_eq!(
            combo.option_at(control, [popup.x + 4.0, popup.y + 13.0], 12),
            Some(2)
        );
    }

    #[test]
    fn selection_can_keep_popup_open() {
        let mut combo = ComboBox::new(0);
        combo.toggle();
        combo.select(2, false);

        assert_eq!(combo.selected(), 2);
        assert!(combo.open);

        combo.select(1, true);
        assert_eq!(combo.selected(), 1);
        assert!(!combo.open);
    }
}
