use std::fmt::Display;

use crate::{BuildCtx, FormatKey, Rect};

use super::Style;

const TRACK_H: f32 = 6.0;
const SEGMENT_WIDTH_RATIO: f32 = 0.24;

pub struct ProgressBar;

impl ProgressBar {
    pub fn build(ctx: &mut BuildCtx, id: impl Display, rect: Rect, value: f32, style: Style) {
        let track_height = TRACK_H * style.ui_scale;
        let track = Self::track(rect, track_height);
        let fill = Rect {
            width: track.width * value.clamp(0.0, 1.0),
            ..track
        };
        crate::ui!(ctx, {
            Rect(FormatKey::new(format_args!("progress-track {id}")), track) {
                fill: style.control; border: 1; border_color: style.border;
                border_radius: track_height * 0.5;
            }
            @if fill.width > 0.0 {
                Rect(FormatKey::new(format_args!("progress-fill {id}")), fill) {
                    fill: style.accent; border_radius: track_height * 0.5;
                }
            }
        });
    }

    pub fn build_indeterminate(
        ctx: &mut BuildCtx,
        id: impl Display,
        rect: Rect,
        phase: f32,
        style: Style,
    ) {
        let track_height = TRACK_H * style.ui_scale;
        let track = Self::track(rect, track_height);
        let segment_width = track.width * SEGMENT_WIDTH_RATIO;
        let travel = track.width + segment_width;
        let x = track.x - segment_width + travel * phase.rem_euclid(1.0);
        let left = x.max(track.x);
        let right = (x + segment_width).min(track.right());
        let segment = Rect::new(left, track.y, (right - left).max(0.0), track.height);
        crate::ui!(ctx, {
            Rect(FormatKey::new(format_args!("progress-track {id}")), track) {
                fill: style.control; border: 1; border_color: style.border;
                border_radius: track_height * 0.5;
            }
            @if segment.width > 0.0 {
                Rect(FormatKey::new(format_args!("progress-segment {id}")), segment) {
                    fill: style.accent; border_radius: track_height * 0.5;
                }
            }
        });
    }

    fn track(rect: Rect, height: f32) -> Rect {
        Rect::new(
            rect.x,
            height.mul_add(-0.5, rect.y + rect.height * 0.5),
            rect.width,
            height,
        )
    }
}
