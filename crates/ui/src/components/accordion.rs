use std::fmt::Display;

use crate::{BuildCtx, FormatKey, IconId, Rect, Size};

use super::{Style, ease};

const SPEED: f32 = 30.0;

pub struct AccordionContent<'a> {
    pub title: &'a str,
    pub body: &'a str,
    pub chevron: IconId,
}

pub struct Accordion {
    open: bool,
    t: f32,
}

impl Accordion {
    #[must_use]
    pub const fn new(open: bool) -> Self {
        Self {
            open,
            t: if open { 1.0 } else { 0.0 },
        }
    }

    pub const fn toggle(&mut self) {
        self.open = !self.open;
    }

    pub fn tick(&mut self, dt: f32) {
        ease(&mut self.t, f32::from(u8::from(self.open)), SPEED, dt);
    }

    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open
    }

    #[must_use]
    pub const fn open_amount(&self) -> f32 {
        self.t
    }

    #[must_use]
    pub fn is_visible(&self) -> bool {
        self.t > 0.001
    }

    #[must_use]
    pub fn is_animating(&self) -> bool {
        (self.t - f32::from(u8::from(self.open))).abs() > 0.001
    }

    pub fn build_header(
        &self,
        ctx: &mut BuildCtx,
        id: impl Display,
        header: Rect,
        title: &str,
        chevron: IconId,
        style: Style,
    ) {
        crate::ui!(ctx, {
            Row {
                id: @format("accordion {}", id);
                bounds: (header.x, header.y, header.width, header.height);
                padding: 5.0;
                gap: 5.0;
                fill: style.control;
                border: 1;
                border_color: style.border;
                border_radius: style.radius_sm;
                interactive;

                Icon {
                    id: @format("accordion-chevron {}", &id);
                    icon!: chevron;
                    color!: style.muted;
                    texture_rotation: std::f32::consts::FRAC_PI_2 * self.t;
                    width: Size::Pixels(16.0);
                    height: Size::Fill;
                }
                Block {
                    id: @format("accordion-title {}", id);
                    width: Size::Fill;
                    height: Size::Fill;
                    font_size: 11.0;
                    text_color: style.text;
                    text: title;
                }
            }
        });
    }

    pub fn build_body_rect(&self, ctx: &mut BuildCtx, id: impl Display, rect: Rect, style: Style) {
        if self.t <= 0.001 || rect.height <= 0.001 {
            return;
        }
        crate::ui!(ctx, {
            Rect(FormatKey::new(format_args!("accordion-body {id}")), rect) {
                fill: style.control; border: 1; border_color: style.border; border_radius: style.radius_sm;
                opacity: self.t;
            }
        });
    }

    pub fn build(
        &self,
        ctx: &mut BuildCtx,
        id: impl Display,
        header: Rect,
        body_rect: Rect,
        content: AccordionContent<'_>,
        style: Style,
    ) {
        let AccordionContent {
            title,
            body,
            chevron,
        } = content;
        self.build_header(ctx, &id, header, title, chevron, style);
        self.build_body_rect(ctx, &id, body_rect, style);
        if self.t > 0.001 && body_rect.height > 0.001 {
            crate::ui!(ctx, {
                Rect(FormatKey::new(format_args!("accordion-content {id}")), body_rect) {
                    padding: 9.0; font_size: 10.5; text_color: style.muted; opacity: self.t; text: body;
                }
            });
        }
    }
}
