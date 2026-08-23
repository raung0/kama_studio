use std::fmt::Display;

use crate::{BuildCtx, FormatKey, IconId, Rect, Size};

use super::{ease, Style};

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
    pub fn new(open: bool) -> Self {
        Self {
            open,
            t: open as u8 as f32,
        }
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
    }

    pub fn tick(&mut self, dt: f32) {
        ease(&mut self.t, self.open as u8 as f32, SPEED, dt);
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn open_amount(&self) -> f32 {
        self.t
    }

    pub fn is_visible(&self) -> bool {
        self.t > 0.001
    }

    pub fn is_animating(&self) -> bool {
        (self.t - self.open as u8 as f32).abs() > 0.001
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

    pub fn body_rect(&self, header: Rect, height: f32) -> Option<Rect> {
        if self.t <= 0.001 {
            return None;
        }
        Some(Rect {
            x: header.x,
            y: header.bottom() + 4.0,
            width: header.width,
            height: (height * self.t).max(0.0),
        })
    }

    pub fn build_body(
        &self,
        ctx: &mut BuildCtx,
        id: impl Display,
        header: Rect,
        height: f32,
        style: Style,
    ) -> Option<Rect> {
        let rect = self.body_rect(header, height)?;
        crate::ui!(ctx, {
            Rect(FormatKey::new(format_args!("accordion-body {id}")), rect) {
                fill: style.control; border: 1; border_color: style.border; border_radius: style.radius_sm;
                opacity: self.t;
            }
        });
        Some(rect)
    }

    pub fn content_rect(&self, header: Rect, height: f32, padding: f32) -> Option<Rect> {
        let body = self.body_rect(header, height)?;
        let padding = padding.max(0.0);
        Some(Rect {
            x: body.x + padding,
            y: body.y + padding,
            width: (body.width - padding * 2.0).max(0.0),
            height: (body.height - padding * 2.0).max(0.0),
        })
    }

    pub fn build(
        &self,
        ctx: &mut BuildCtx,
        id: impl Display,
        header: Rect,
        body_height: f32,
        content: AccordionContent<'_>,
        style: Style,
    ) {
        let AccordionContent {
            title,
            body,
            chevron,
        } = content;
        self.build_header(ctx, &id, header, title, chevron, style);

        if let Some(rect) = self.build_body(ctx, &id, header, body_height, style) {
            crate::ui!(ctx, {
                Rect(FormatKey::new(format_args!("accordion-content {id}")), rect) {
                    padding: 9.0; font_size: 10.5; text_color: style.muted; opacity: self.t; text: body;
                }
            });
        }
    }
}
