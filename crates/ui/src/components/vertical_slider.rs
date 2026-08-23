use std::fmt::Display;

use crate::{BuildCtx, FormatKey, Rect, Size};

use super::{ease, Style};

const SPEED: f32 = 22.0;
const TRACK: f32 = 4.0;
const THUMB_H: f32 = 8.0;
const THUMB_W: f32 = 14.0;


pub struct VerticalSlider {
    value: f32,
    shown: f32,
    drag_track: Option<Rect>,
}

impl VerticalSlider {
    pub fn new(value: f32) -> Self {
        let value = value.clamp(0.0, 1.0);
        Self {
            value,
            shown: value,
            drag_track: None,
        }
    }

    pub fn value(&self) -> f32 {
        self.value
    }

    pub fn set_value(&mut self, value: f32) {
        let value = value.clamp(0.0, 1.0);
        self.value = value;
        
        
        if self.drag_track.is_none() {
            self.shown = value;
        }
    }

    pub fn is_dragging(&self) -> bool {
        self.drag_track.is_some()
    }

    pub fn tick(&mut self, dt: f32) {
        ease(&mut self.shown, self.value, SPEED, dt);
    }

    pub fn is_animating(&self) -> bool {
        (self.shown - self.value).abs() > 0.001
    }

    pub fn pointer_pressed(&mut self, rect: Rect, point: [f32; 2]) -> bool {
        if !rect.contains(point) {
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

    pub fn pointer_released(&mut self) -> bool {
        self.drag_track.take().is_some()
    }

    pub fn build(&self, ctx: &mut BuildCtx, id: impl Display, rect: Rect, style: Style) {
        let track = Self::track(rect);
        let fill_height = track.height * self.shown;
        crate::ui!(ctx, {
            Rect(FormatKey::new(format_args!("vertical-slider-track {id}")), track) {
                fill: style.border; border_radius: TRACK * 0.5;
            }
            @if fill_height > 0.0 {
                Rect(FormatKey::new(format_args!("vertical-slider-fill {id}")), Rect::new(
                    track.x,
                    track.bottom() - fill_height,
                    track.width,
                    fill_height,
                )) {
                    fill: style.accent; border_radius: TRACK * 0.5;
                }
            }
            Rect(FormatKey::new(format_args!("vertical-slider-thumb {id}")), Rect::new(
                track.x + TRACK * 0.5 - THUMB_W * 0.5,
                track.bottom() - fill_height - THUMB_H * 0.5,
                THUMB_W,
                THUMB_H,
            )) {
                fill: style.accent; border: 1; border_color: style.focused;
                border_radius: THUMB_H * 0.5; reveal;
            }
        });
    }

    fn track(rect: Rect) -> Rect {
        let height = (rect.height - THUMB_H).max(0.0);
        let (track, measured) = crate::measure_layout(rect, |ctx| {
            ctx.new()
                .overlay()
                .centered()
                .width(Size::Pixels(TRACK))
                .height(Size::Pixels(height))
                .build()
        });
        measured.rect(track).expect("vertical slider track layout")
    }

    fn set_target(&mut self, track: Rect, point: [f32; 2]) {
        self.value = if track.height > 0.0 {
            (1.0 - (point[1] - track.y) / track.height).clamp(0.0, 1.0)
        } else {
            0.0
        };
    }
}
