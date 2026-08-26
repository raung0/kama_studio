use std::fmt::Display;

use crate::{Align, BlockId, BuildCtx, FormatKey, Rect, Size};

use super::{ease, Style};

const SPEED: f32 = 22.0;
const TRACK: f32 = 4.0;
const THUMB: f32 = 12.0;
const VALUE_W: f32 = 48.0;

pub struct Slider {
    value: f32,
    shown: f32,
    drag_track: Option<Rect>,
}

impl Slider {
    #[must_use]
    pub const fn new(value: f32) -> Self {
        let value = value.clamp(0.0, 1.0);
        Self {
            value,
            shown: value,
            drag_track: None,
        }
    }

    #[must_use]
    pub const fn value(&self) -> f32 {
        self.value
    }

    pub const fn set_value(&mut self, value: f32) {
        let value = value.clamp(0.0, 1.0);
        self.value = value;
        self.shown = value;
    }

    #[must_use]
    pub const fn is_dragging(&self) -> bool {
        self.drag_track.is_some()
    }

    pub fn tick(&mut self, dt: f32) {
        ease(&mut self.shown, self.value, SPEED, dt);
    }

    #[must_use]
    pub fn is_animating(&self) -> bool {
        (self.shown - self.value).abs() > 0.001
    }

    pub fn pointer_pressed(&mut self, rect: Rect, point: [f32; 2]) -> bool {
        if !Self::hit_rect(rect).contains(point) {
            return false;
        }
        let track = Self::track(rect);
        self.drag_track = Some(track);
        self.set_target(track, point);
        true
    }

    pub fn pointer_moved(&mut self, point: [f32; 2]) -> bool {
        let Some(track) = self.drag_track else {
            return false;
        };
        self.set_target(track, point);
        true
    }

    pub const fn pointer_released(&mut self) -> bool {
        self.drag_track.take().is_some()
    }

    pub fn build(&self, ctx: &mut BuildCtx, id: impl Display, rect: Rect, style: Style) {
        let (_, track, value) = Self::layout(rect);
        let fill_width = track.width * self.shown;
        crate::ui!(ctx, {
            Rect(FormatKey::new(format_args!("slider-track {id}")), track) {
                fill: style.border; border_radius: TRACK * 0.5;
            }
            @if fill_width > 0.0 {
                Rect(FormatKey::new(format_args!("slider-fill {id}")), Rect { width: fill_width, ..track }) {
                    fill: style.accent; border_radius: TRACK * 0.5;
                }
            }
            Rect(FormatKey::new(format_args!("slider-thumb {id}")), Rect::new(
                THUMB.mul_add(-0.5, track.x + fill_width),
                THUMB.mul_add(-0.5, TRACK.mul_add(0.5, track.y)),
                THUMB,
                THUMB,
            )) {
                fill: style.accent; border: 1; border_color: style.focused;
                border_radius: THUMB * 0.5; reveal;
            }
            Rect(FormatKey::new(format_args!("slider-value {id}")), value) {
                font_size: 10.5; text_color: style.text; text: format!("{:3.0}%", self.value * 100.0);
            }
        });
    }

    fn layout(rect: Rect) -> (Rect, Rect, Rect) {
        let ((hit, track, value), measured) = crate::measure_layout(rect, |ctx| {
            let mut hit = BlockId(0);
            let mut track = BlockId(0);
            let mut value = BlockId(0);
            let _ = ctx
                .new()
                .row()
                .width(Size::Fill)
                .height(Size::Fill)
                .children(|ctx| {
                    hit = ctx
                        .new()
                        .row()
                        .width(Size::Fill)
                        .height(Size::Fill)
                        .align_items(Align::Center)
                        .children(|ctx| {
                            let _ = ctx
                                .new()
                                .width(Size::Pixels(THUMB * 0.5))
                                .height(Size::Fill)
                                .build();
                            track = ctx
                                .new()
                                .width(Size::Fill)
                                .height(Size::Pixels(TRACK))
                                .build();
                            let _ = ctx
                                .new()
                                .width(Size::Pixels(THUMB * 0.5))
                                .height(Size::Fill)
                                .build();
                        })
                        .build();
                    let _ = ctx
                        .new()
                        .width(Size::Pixels(8.0))
                        .height(Size::Fill)
                        .build();
                    value = ctx
                        .new()
                        .width(Size::Pixels(VALUE_W - 8.0))
                        .height(Size::Fill)
                        .build();
                })
                .build();
            (hit, track, value)
        });
        (
            measured.rect(hit).expect("slider hit layout"),
            measured.rect(track).expect("slider track layout"),
            measured.rect(value).expect("slider value layout"),
        )
    }

    fn hit_rect(rect: Rect) -> Rect {
        Self::layout(rect).0
    }

    fn track(rect: Rect) -> Rect {
        Self::layout(rect).1
    }

    fn set_target(&mut self, track: Rect, point: [f32; 2]) {
        self.value = if track.width > 0.0 {
            ((point[0] - track.x) / track.width).clamp(0.0, 1.0)
        } else {
            0.0
        };
    }
}
