use std::{cell::Cell, fmt::Display};

use crate::{Align, BuildCtx, Color, IconId, PopupDirection, PopupState, Rect, ScrollState, Size};

use super::{Style, ease};

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
    open: PopupState,
    t: f32,
    open_direction: ComboBoxOpenDirection,
    scroll: Cell<f32>,
    ui_scale: Cell<f32>,
    built_rect: Cell<Option<Rect>>,
    window_bounds: Cell<Option<Rect>>,
}

impl ComboBox {
    #[must_use]
    pub fn new(selected: usize) -> Self {
        Self {
            selected,
            open: PopupState::default(),
            t: 0.0,
            open_direction: ComboBoxOpenDirection::Down,
            scroll: Cell::new(0.0),
            ui_scale: Cell::new(1.0),
            built_rect: Cell::new(None),
            window_bounds: Cell::new(None),
        }
    }

    pub const fn open_direction(mut self, direction: ComboBoxOpenDirection) -> Self {
        self.open_direction = direction;
        self
    }

    pub const fn selected(&self) -> usize {
        self.selected
    }

    pub const fn set_selected(&mut self, selected: usize) {
        self.selected = selected;
    }

    pub fn is_open(&self) -> bool {
        self.open.is_open()
    }

    pub fn close(&mut self) {
        self.open.close();
    }

    pub fn toggle(&mut self) {
        self.open.toggle();
    }

    pub fn tick(&mut self, dt: f32) {
        ease(
            &mut self.t,
            f32::from(u8::from(self.open.is_open())),
            SPEED,
            dt,
        );
    }

    pub fn is_animating(&self) -> bool {
        (self.t - f32::from(u8::from(self.open.is_open()))).abs() > 0.001
    }

    pub fn option_at(&self, rect: Rect, point: [f32; 2], len: usize) -> Option<usize> {
        if !self.open.is_open() {
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
        self.open.is_open()
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
        let viewport_height = popup.height / self.t.max(0.001);
        let max = (len as f32 * OPTION_H)
            .mul_add(scale, -viewport_height)
            .max(0.0);
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
            self.open.close();
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
        self.built_rect.set(Some(rect));
        self.window_bounds.set(None);
        let id = id.to_string();
        self.build_control(ctx, &id, rect, options, chevron, style);
        self.build_popup_inner(ctx, &id, rect, options, None, style);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn build_in(
        &self,
        ctx: &mut BuildCtx,
        id: impl Display,
        rect: Rect,
        options: &[&str],
        chevron: IconId,
        window_bounds: Rect,
        style: Style,
    ) {
        self.built_rect.set(Some(rect));
        self.window_bounds.set(Some(window_bounds));
        let id = id.to_string();
        self.build_control(ctx, &id, rect, options, chevron, style);
        self.build_popup_inner(ctx, &id, rect, options, Some(window_bounds), style);
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
                fill: if self.open.is_open() { style.focused } else { style.control };
                border: 1;
                border_color: if self.open.is_open() { style.accent } else { style.border };
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
                    texture_rotation: std::f32::consts::PI.mul_add(-self.t, std::f32::consts::FRAC_PI_2);
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
        self.build_popup_inner(ctx, id, rect, options, None, style);
    }

    pub fn build_popup_in(
        &self,
        ctx: &mut BuildCtx,
        id: impl Display,
        rect: Rect,
        options: &[&str],
        window_bounds: Rect,
        style: Style,
    ) {
        self.build_popup_inner(ctx, id, rect, options, Some(window_bounds), style);
    }

    fn build_popup_inner(
        &self,
        ctx: &mut BuildCtx,
        id: impl Display,
        rect: Rect,
        options: &[&str],
        window_bounds: Option<Rect>,
        style: Style,
    ) {
        self.built_rect.set(Some(rect));
        self.window_bounds.set(window_bounds);
        let selected = self.selected.min(options.len().saturating_sub(1));
        let ui_scale = style.ui_scale.clamp(0.25, 4.0);
        self.ui_scale.set(ui_scale);
        let Some(popup) = self.popup_rect(rect, options.len()) else {
            return;
        };
        crate::ui!(ctx, {
            Block {
                id: @format("combobox-popup {}", id);
                top_overlay; dismissible_popup: self.open.clone();
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
        let desired_height = visible as f32 * OPTION_H * scale;
        let placement = self.effective_window_bounds(rect).map_or_else(
            || crate::PopupPlacement {
                rect: Rect::new(
                    rect.x,
                    match self.open_direction {
                        ComboBoxOpenDirection::Down => 4.0f32.mul_add(scale, rect.bottom()),
                        ComboBoxOpenDirection::Up => {
                            4.0f32.mul_add(-scale, rect.y) - desired_height
                        }
                    },
                    rect.width,
                    desired_height,
                ),
                direction: match self.open_direction {
                    ComboBoxOpenDirection::Down => PopupDirection::Down,
                    ComboBoxOpenDirection::Up => PopupDirection::Up,
                },
            },
            |bounds| {
                crate::place_popup_with_direction(
                    rect,
                    [rect.width, desired_height],
                    bounds,
                    self.open_direction == ComboBoxOpenDirection::Up,
                    4.0 * scale,
                )
            },
        );
        let max_scroll = (len as f32 * OPTION_H)
            .mul_add(scale, -placement.rect.height)
            .max(0.0);
        self.scroll.set(self.scroll.get().clamp(0.0, max_scroll));
        let height = placement.rect.height * self.t;
        let y = match placement.direction {
            PopupDirection::Down => ((1.0 - self.t) * 5.0).mul_add(-scale, placement.rect.y),
            PopupDirection::Up => {
                ((1.0 - self.t) * 5.0).mul_add(scale, placement.rect.bottom() - height)
            }
        };
        Some(Rect::new(placement.rect.x, y, placement.rect.width, height))
    }

    fn effective_window_bounds(&self, rect: Rect) -> Option<Rect> {
        let built = self.built_rect.get()?;
        let mut bounds = self.window_bounds.get()?;
        bounds.x += rect.x - built.x;
        bounds.y += rect.y - built.y;
        Some(bounds)
    }

    fn option_rects(&self, popup: Rect, len: usize) -> Vec<Rect> {
        let scale = self.ui_scale.get().clamp(0.25, 4.0);
        let (ids, measured) = crate::measure_layout(popup, |ctx| {
            let mut ids = Vec::with_capacity(len);
            let _ = ctx
                .new()
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
    fn popup_uses_window_bounds_after_anchor_is_translated() {
        let mut combo = ComboBox::new(0);
        combo.toggle();
        combo.t = 1.0;
        combo
            .built_rect
            .set(Some(crate::Rect::new(10.0, 180.0, 120.0, 28.0)));
        combo
            .window_bounds
            .set(Some(crate::Rect::new(-200.0, -100.0, 800.0, 340.0)));

        let absolute = crate::Rect::new(210.0, 280.0, 120.0, 28.0);
        let popup = combo.popup_rect(absolute, 3).expect("open popup");
        assert!(popup.y >= 0.0);
        assert!(popup.bottom() < absolute.y);
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
        assert!(combo.is_open());

        combo.select(1, true);
        assert_eq!(combo.selected(), 1);
        assert!(!combo.is_open());
    }
}
