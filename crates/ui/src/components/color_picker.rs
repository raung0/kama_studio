use std::{
    collections::hash_map::DefaultHasher,
    fmt::Display,
    hash::{Hash, Hasher},
};

use anyhow::Result;
use winit::{
    event::{Ime, KeyEvent},
    keyboard::ModifiersState,
};

use crate::{Align, BlockId, BuildCtx, Color, CursorShape, Rect, Renderer, Size, TextureId};

use super::{ease, ColorButton, Style, TextEdit};

const SPEED: f32 = 18.0;
const POPUP_W: f32 = 300.0;
const POPUP_H: f32 = 352.0;
const TEXTURE_WIDTH: u32 = 256;
const COLOR_AREA_HEIGHT: u32 = 256;
const STRIP_HEIGHT: u32 = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Mode {
    Hsv,
    Rgb,
    Okhsl,
}

#[derive(Clone, Copy, Debug)]
enum Drag {
    Plane,
    Hue,
    Alpha,
}

#[derive(Clone, Copy)]
struct Layout {
    popup: Rect,
    tabs: [Rect; 3],
    preview: Rect,
    plane: Rect,
    hue: Rect,
    alpha: Rect,
    hex: Rect,
}

#[derive(Default)]
struct Textures {
    plane: Option<TextureId>,
    alpha: Option<TextureId>,
    checker: Option<TextureId>,
    signature: Option<u64>,
}



pub struct ColorPicker {
    linear: [f32; 4],
    hue: f32,
    mode: Mode,
    hex: TextEdit,
    open: bool,
    t: f32,
    drag: Option<Drag>,
    textures: Textures,
}

impl ColorPicker {
    pub fn new(color: Color) -> Self {
        let linear = color.to_array();
        let srgb = linear_to_srgb_rgba(linear);
        let hue = rgb_to_hsv([srgb[0], srgb[1], srgb[2]])[0];
        Self {
            linear,
            hue,
            mode: Mode::Hsv,
            hex: TextEdit::single_line(rgba_hex(linear)),
            open: false,
            t: 0.0,
            drag: None,
            textures: Textures::default(),
        }
    }

    pub fn color(&self) -> Color {
        ui_color(self.linear)
    }

    pub fn linear(&self) -> [f32; 4] {
        self.linear
    }

    pub fn set_linear(&mut self, value: [f32; 4]) {
        self.linear = [value[0], value[1], value[2], value[3].clamp(0.0, 1.0)];
        let srgb = linear_to_srgb_rgba(self.linear);
        let hsv = rgb_to_hsv([srgb[0], srgb[1], srgb[2]]);
        if hsv[1] > 1e-4 && hsv[2] > 1e-4 {
            self.hue = hsv[0];
        }
        if !self.hex.is_focused() {
            self.hex.reset(rgba_hex(self.linear));
        }
        self.textures.signature = None;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.drag = None;
        self.hex.set_focused(false);
    }

    
    
    pub fn open_and_focus_hex(&mut self) {
        self.open = true;
        self.t = self.t.max(0.05);
        self.drag = None;
        self.hex.set_focused(true);
    }

    pub fn tick(&mut self, dt: f32) {
        ease(&mut self.t, self.open as u8 as f32, SPEED, dt);
        self.hex.tick(dt);
    }

    pub fn is_animating(&self) -> bool {
        (self.t - self.open as u8 as f32).abs() > 0.001 || self.hex.is_animating()
    }

    pub fn is_editing(&self) -> bool {
        self.open && self.hex.is_focused()
    }

    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    pub fn caret_rect(&self, rect: Rect) -> Option<Rect> {
        self.caret_rect_bounded(rect, None)
    }

    pub fn caret_rect_in(&self, rect: Rect, bounds: Rect) -> Option<Rect> {
        self.caret_rect_bounded(rect, Some(bounds))
    }

    fn caret_rect_bounded(&self, rect: Rect, bounds: Option<Rect>) -> Option<Rect> {
        self.is_editing()
            .then(|| self.hex.caret_rect(self.layout(rect, bounds).hex))
    }

    pub fn popup_contains(&self, rect: Rect, point: [f32; 2]) -> bool {
        self.popup_contains_bounded(rect, None, point)
    }

    pub fn popup_contains_in(&self, rect: Rect, bounds: Rect, point: [f32; 2]) -> bool {
        self.popup_contains_bounded(rect, Some(bounds), point)
    }

    fn popup_contains_bounded(&self, rect: Rect, bounds: Option<Rect>, point: [f32; 2]) -> bool {
        self.open && self.layout(rect, bounds).popup.contains(point)
    }

    pub fn sync_textures(&mut self, renderer: &mut Renderer) -> Result<()> {
        if self.t <= 0.001 {
            return Ok(());
        }
        let signature = self.texture_signature();
        if self.textures.signature == Some(signature) {
            return Ok(());
        }
        let plane = self.plane_pixels();
        let alpha = alpha_pixels(self.linear);
        sync_texture(
            renderer,
            &mut self.textures.plane,
            TEXTURE_WIDTH,
            COLOR_AREA_HEIGHT,
            &plane,
        )?;
        sync_texture(
            renderer,
            &mut self.textures.alpha,
            TEXTURE_WIDTH,
            STRIP_HEIGHT,
            &alpha,
        )?;
        if self.textures.checker.is_none() {
            let checker = checker_pixels(48, 48, 8);
            sync_texture(renderer, &mut self.textures.checker, 48, 48, &checker)?;
        }
        self.textures.signature = Some(signature);
        Ok(())
    }

    pub fn pointer_pressed(
        &mut self,
        rect: Rect,
        point: [f32; 2],
        modifiers: ModifiersState,
    ) -> bool {
        self.pointer_pressed_bounded(rect, None, point, modifiers)
    }

    pub fn pointer_pressed_in(
        &mut self,
        rect: Rect,
        bounds: Rect,
        point: [f32; 2],
        modifiers: ModifiersState,
    ) -> bool {
        self.pointer_pressed_bounded(rect, Some(bounds), point, modifiers)
    }

    fn pointer_pressed_bounded(
        &mut self,
        rect: Rect,
        bounds: Option<Rect>,
        point: [f32; 2],
        modifiers: ModifiersState,
    ) -> bool {
        let layout = self.layout(rect, bounds);
        if self.open && layout.popup.contains(point) {
            for (index, tab) in layout.tabs.into_iter().enumerate() {
                if tab.contains(point) {
                    self.mode = [Mode::Hsv, Mode::Rgb, Mode::Okhsl][index];
                    self.textures.signature = None;
                    self.hex.set_focused(false);
                    return true;
                }
            }
            let drag = if self.mode == Mode::Rgb && layout.plane.contains(point) {
                Some(Drag::Plane)
            } else if self.mode != Mode::Rgb && wheel_ring_contains(layout.plane, point) {
                Some(Drag::Hue)
            } else if self.mode != Mode::Rgb
                && triangle_contains(layout.plane, self.current_hue(), point)
            {
                Some(Drag::Plane)
            } else if layout.alpha.contains(point) {
                Some(Drag::Alpha)
            } else {
                None
            };
            if let Some(drag) = drag {
                self.drag = Some(drag);
                self.hex.set_focused(false);
                self.update_from_pointer(layout, drag, point);
                return true;
            }
            if self.hex.pointer_pressed(layout.hex, point, modifiers) {
                return true;
            }
            return true;
        }
        if rect.contains(point) {
            self.open = !self.open;
            if self.open {
                self.t = self.t.max(0.05);
            } else {
                self.drag = None;
                self.hex.set_focused(false);
            }
            return true;
        }
        self.close();
        false
    }

    pub fn pointer_moved(&mut self, rect: Rect, point: [f32; 2]) -> bool {
        self.pointer_moved_bounded(rect, None, point)
    }

    pub fn pointer_moved_in(&mut self, rect: Rect, bounds: Rect, point: [f32; 2]) -> bool {
        self.pointer_moved_bounded(rect, Some(bounds), point)
    }

    fn pointer_moved_bounded(&mut self, rect: Rect, bounds: Option<Rect>, point: [f32; 2]) -> bool {
        if let Some(drag) = self.drag {
            self.update_from_pointer(self.layout(rect, bounds), drag, point);
            return true;
        }
        self.hex.pointer_moved(point)
    }

    pub fn pointer_released(&mut self) -> bool {
        let dragged = self.drag.take().is_some();
        dragged | self.hex.pointer_released()
    }

    pub fn handle_key(&mut self, event: &KeyEvent, modifiers: ModifiersState) -> bool {
        if !self.is_editing() {
            return false;
        }
        let response = self.hex.handle_key(event, modifiers);
        if response.changed {
            self.apply_hex();
        }
        response.handled
    }

    pub fn handle_ime(&mut self, event: &Ime) -> bool {
        if !self.is_editing() {
            return false;
        }
        let response = self.hex.handle_ime(event);
        if response.changed {
            self.apply_hex();
        }
        response.handled
    }

    pub fn build(&mut self, ctx: &mut BuildCtx, id: impl Display, rect: Rect, style: Style) {
        self.build_bounded(ctx, id, rect, None, style);
    }

    pub fn build_in(
        &mut self,
        ctx: &mut BuildCtx,
        id: impl Display,
        rect: Rect,
        bounds: Rect,
        style: Style,
    ) {
        self.build_bounded(ctx, id, rect, Some(bounds), style);
    }

    fn build_bounded(
        &mut self,
        ctx: &mut BuildCtx,
        id: impl Display,
        rect: Rect,
        bounds: Option<Rect>,
        style: Style,
    ) {
        ColorButton::build(ctx, &id, rect, self.color(), style);
        if self.t <= 0.001 {
            return;
        }
        let layout = self.layout(rect, bounds);
        let opacity = self.t;
        let popup_bg = crate::theme_popup_bg();
        let popup_tint = crate::theme_popup_tint();
        let shadow = Color::rgba(popup_bg.r, popup_bg.g, popup_bg.b, 0.22);
        crate::ui!(ctx, {
            Rect(@format("color-picker-shadow {id}"), Rect::new(
                layout.popup.x - 4.0, layout.popup.y - 4.0,
                layout.popup.width + 8.0, layout.popup.height + 8.0,
            )) {
                overlay; backdrop_blur: 30.0; backdrop_tint: popup_tint;
                opacity: opacity; fill: shadow; border_radius: style.radius_md;
            }
            Rect(@format("color-picker-popup {id}"), layout.popup) {
                overlay; backdrop_blur: 22.0; backdrop_tint: popup_tint;
                opacity: opacity; fill: popup_bg;
                border: 1; border_color: style.accent; border_radius: style.radius_md;
            }
            @for (index, (tab, label, mode)) in [
                (layout.tabs[0], "HSV", Mode::Hsv),
                (layout.tabs[1], "RGB", Mode::Rgb),
                (layout.tabs[2], "Okhsl", Mode::Okhsl),
            ].into_iter().enumerate() {
                @let selected = self.mode == mode;
                Rect(@format("color-picker-tab {id} {index}"), tab) {
                    overlay; opacity: opacity; fill: if selected { style.focused } else { style.control };
                    border: 1; border_color: if selected { style.accent } else { style.border };
                    border_radius: style.radius_sm; font_size: 9.0;
                    text_color: if selected { style.text } else { style.muted };
                    text_centered; text: label; interactive;
                }
            }
            Rect(@format("color-picker-preview-bg {id}"), layout.preview) {
                overlay; opacity: opacity; fill: Color::WHITE; border: 1;
                border_color: style.border; border_radius: style.radius_sm;
                fill_texture_opt: self.textures.checker;
            }
            Rect(@format("color-picker-preview {id}"), layout.preview) {
                overlay; opacity: opacity; fill: self.color(); border: 1;
                border_color: style.border; border_radius: style.radius_sm;
            }
        });

        if self.mode == Mode::Rgb {
            crate::ui!(ctx, {
                Rect(@format("color-picker-plane {id}"), layout.plane) {
                    overlay; opacity: opacity; fill: Color::WHITE; border: 1;
                    border_color: style.border; border_radius: style.radius_sm;
                    interactive; border_reveal;
                    fill_texture_opt: self.textures.plane;
                }
            });
        } else {
            
            
            
            crate::ui!(ctx, {
                Rect(@format("color-picker-wheel {id}"), layout.plane) {
                    overlay; opacity: opacity; fill: Color::WHITE;
                    border_radius: layout.plane.width * 0.5; cursor: CursorShape::Pointer;
                    fill_texture_opt: self.textures.plane;
                }
            });
        }

        match self.mode {
            Mode::Hsv => {
                let srgb = linear_to_srgb_rgba(self.linear);
                let hsv = rgb_to_hsv([srgb[0], srgb[1], srgb[2]]);
                wheel_handles(
                    ctx,
                    &id,
                    layout.plane,
                    self.current_hue(),
                    hsv_triangle_weights(hsv[1], hsv[2]),
                    "hsv",
                    opacity,
                );
            }
            Mode::Okhsl => {
                let hsl = linear_rgb_to_okhsl([self.linear[0], self.linear[1], self.linear[2]]);
                wheel_handles(
                    ctx,
                    &id,
                    layout.plane,
                    self.current_hue(),
                    okhsl_triangle_weights(hsl[1], hsl[2]),
                    "okhsl",
                    opacity,
                );
            }
            Mode::Rgb => {
                let srgb = linear_to_srgb_rgba(self.linear);
                let (rows, labels) = rgb_channel_layout(layout.plane);
                crate::ui!(ctx, {
                    @for channel in 0..3 {
                        @let row = rows[channel];
                        @let x = layout.plane.x + srgb[channel].clamp(0.0, 1.0) * layout.plane.width;
                        @let y = row.y + row.height * 0.5;
                        Rect(@format("color-picker-rgb-handle {id} {channel}"), Rect::new(
                            x - 2.0, y - row.height * 0.36, 4.0, row.height * 0.72,
                        )) {
                            overlay; opacity: opacity; fill: Color::WHITE; border: 1;
                            border_color: Color::BLACK; border_radius: 2.0;
                        }
                        Rect(@format("color-picker-rgb-label {id} {channel}"), labels[channel]) {
                            overlay; opacity: opacity; font_size: 8.5;
                            text_color: if srgb[channel] > 0.62 { Color::BLACK } else { Color::WHITE };
                            text_centered; text: ["R", "G", "B"][channel];
                        }
                    }
                });
                crate::ui!(ctx, {
                    Rect(@format("color-picker-rgb-values {id}"), layout.hue) {
                        overlay; opacity: opacity; font_size: 8.5; text_color: style.muted; text_centered;
                        text: format!("R {:03}   G {:03}   B {:03}",
                            (srgb[0] * 255.0).round() as u8,
                            (srgb[1] * 255.0).round() as u8,
                            (srgb[2] * 255.0).round() as u8,
                        );
                    }
                });
            }
        }

        crate::ui!(ctx, {
            Rect(@format("color-picker-alpha {id}"), layout.alpha) {
                overlay; opacity: opacity; fill: Color::WHITE; border: 1;
                border_color: style.border; border_radius: style.radius_sm; interactive; border_reveal;
                fill_texture_opt: self.textures.alpha;
            }
            Rect(@format("color-picker-hex {id}"), layout.hex) {
                overlay; opacity: opacity;
                fill: if self.hex.is_focused() { style.focused } else { style.control };
                border: 1; border_color: if self.hex.is_focused() { style.accent } else { style.border };
                border_radius: style.radius_sm; padding: 7.0; font_size: 10.5;
                text_color: style.text; text: self.hex.text().to_string();
            }
            @if self.hex.is_focused() {
                Rect(@format("color-picker-hex-caret {id}"), self.hex.caret_rect(layout.hex)) {
                    overlay; opacity: opacity; fill: style.text;
                }
            }
        });
        strip_handle(ctx, &id, layout.alpha, self.linear[3], "alpha", opacity);
    }

    fn layout(&self, rect: Rect, bounds: Option<Rect>) -> Layout {
        let (width, height, x, y) = if let Some(bounds) = bounds {
            let placed = crate::place_popup(rect, [POPUP_W, POPUP_H], bounds, false, 4.0);
            (placed.width, placed.height, placed.x, placed.y)
        } else {
            (
                POPUP_W.max(rect.width.min(POPUP_W)),
                POPUP_H,
                rect.x,
                rect.bottom() + 4.0,
            )
        };
        
        
        let popup = Rect::new(
            x,
            y - (1.0 - self.t) * 5.0,
            width,
            height * self.t.max(0.001),
        );
        let inner_w = (popup.width - 16.0).max(1.0);
        let plane_size = inner_w.min((height - 132.0).max(96.0)).min(210.0);
        let ((tabs, preview, plane, hue, alpha, hex), measured) =
            crate::measure_layout(popup, |ctx| {
                let mut tabs = [BlockId(0); 3];
                let mut preview = BlockId(0);
                let mut plane = BlockId(0);
                let mut hue = None;
                let mut alpha = BlockId(0);
                let mut hex = BlockId(0);
                ctx.new()
                    .column()
                    .width(Size::Fill)
                    .height(Size::Fill)
                    .padding(8.0)
                    .align_items(Align::Center)
                    .children(|ctx| {
                        ctx.new()
                            .row()
                            .width(Size::Fill)
                            .height(Size::Pixels(22.0))
                            .gap(2.0)
                            .children(|ctx| {
                                for tab in &mut tabs {
                                    *tab = ctx.new().width(Size::Fill).height(Size::Fill).build();
                                }
                            })
                            .build();
                        ctx.new()
                            .width(Size::Fill)
                            .height(Size::Pixels(5.0))
                            .build();
                        preview = ctx
                            .new()
                            .width(Size::Fill)
                            .height(Size::Pixels(24.0))
                            .build();
                        ctx.new()
                            .width(Size::Fill)
                            .height(Size::Pixels(5.0))
                            .build();
                        plane = ctx
                            .new()
                            .width(Size::Pixels(plane_size))
                            .height(Size::Pixels(plane_size))
                            .build();
                        ctx.new()
                            .width(Size::Fill)
                            .height(Size::Pixels(5.0))
                            .build();
                        if self.mode == Mode::Rgb {
                            hue = Some(
                                ctx.new()
                                    .width(Size::Fill)
                                    .height(Size::Pixels(14.0))
                                    .build(),
                            );
                            ctx.new()
                                .width(Size::Fill)
                                .height(Size::Pixels(5.0))
                                .build();
                        }
                        alpha = ctx
                            .new()
                            .width(Size::Fill)
                            .height(Size::Pixels(14.0))
                            .build();
                        ctx.new()
                            .width(Size::Fill)
                            .height(Size::Pixels(7.0))
                            .build();
                        hex = ctx
                            .new()
                            .width(Size::Fill)
                            .height(Size::Pixels(24.0))
                            .build();
                    })
                    .build();
                (tabs, preview, plane, hue, alpha, hex)
            });
        let alpha = measured.rect(alpha).expect("color picker alpha layout");
        Layout {
            popup,
            tabs: tabs.map(|id| measured.rect(id).expect("color picker tab layout")),
            preview: measured.rect(preview).expect("color picker preview layout"),
            plane: measured.rect(plane).expect("color picker plane layout"),
            hue: hue.and_then(|id| measured.rect(id)).unwrap_or(alpha),
            alpha,
            hex: measured.rect(hex).expect("color picker hex layout"),
        }
    }

    fn apply_hex(&mut self) {
        if let Some(linear) = parse_rgba_hex(self.hex.text()) {
            self.set_linear(linear);
        }
    }

    fn current_hue(&self) -> f32 {
        match self.mode {
            Mode::Hsv | Mode::Rgb => {
                let srgb = linear_to_srgb_rgba(self.linear);
                let hsv = rgb_to_hsv([srgb[0], srgb[1], srgb[2]]);
                if hsv[1] > 1e-4 && hsv[2] > 1e-4 {
                    hsv[0]
                } else {
                    self.hue
                }
            }
            Mode::Okhsl => {
                let hsl = linear_rgb_to_okhsl([self.linear[0], self.linear[1], self.linear[2]]);
                if hsl[1] > 1e-4 && hsl[2] > 1e-4 && hsl[2] < 1.0 - 1e-4 {
                    hsl[0]
                } else {
                    self.hue
                }
            }
        }
    }

    fn update_from_pointer(&mut self, layout: Layout, drag: Drag, point: [f32; 2]) {
        match drag {
            Drag::Plane => {
                let alpha = self.linear[3];
                match self.mode {
                    Mode::Hsv => {
                        let hue = self.current_hue();
                        let weights = triangle_weights_clamped(layout.plane, hue, point);
                        let [s, v] = hsv_from_triangle_weights(weights);
                        let rgb = hsv_to_rgb([hue, s, v]);
                        self.set_linear(srgb_to_linear_rgba([rgb[0], rgb[1], rgb[2], alpha]));
                        self.hue = hue;
                    }
                    Mode::Okhsl => {
                        let hue = self.current_hue();
                        let weights = triangle_weights_clamped(layout.plane, hue, point);
                        let [s, l] = okhsl_from_triangle_weights(weights);
                        let rgb = okhsl_to_linear_rgb([hue, s, l]);
                        self.set_linear([rgb[0], rgb[1], rgb[2], alpha]);
                        self.hue = hue;
                    }
                    Mode::Rgb => {
                        let x = ((point[0] - layout.plane.x) / layout.plane.width.max(1.0))
                            .clamp(0.0, 1.0);
                        let row = ((point[1] - layout.plane.y)
                            / (layout.plane.height / 3.0).max(1.0))
                        .floor()
                        .clamp(0.0, 2.0) as usize;
                        let mut srgb = linear_to_srgb_rgba(self.linear);
                        srgb[row] = x;
                        self.set_linear(srgb_to_linear_rgba(srgb));
                    }
                }
            }
            Drag::Hue => {
                let geometry = wheel_geometry(layout.plane, self.current_hue());
                let dx = point[0] - geometry.center[0];
                let dy = point[1] - geometry.center[1];
                let hue = ((dy.atan2(dx) + std::f32::consts::FRAC_PI_2) / std::f32::consts::TAU)
                    .rem_euclid(1.0);
                self.hue = hue;
                let alpha = self.linear[3];
                match self.mode {
                    Mode::Hsv => {
                        let srgb = linear_to_srgb_rgba(self.linear);
                        let hsv = rgb_to_hsv([srgb[0], srgb[1], srgb[2]]);
                        let rgb = hsv_to_rgb([hue, hsv[1], hsv[2]]);
                        self.set_linear(srgb_to_linear_rgba([rgb[0], rgb[1], rgb[2], alpha]));
                    }
                    Mode::Okhsl => {
                        let hsl =
                            linear_rgb_to_okhsl([self.linear[0], self.linear[1], self.linear[2]]);
                        let rgb = okhsl_to_linear_rgb([hue, hsl[1], hsl[2]]);
                        self.set_linear([rgb[0], rgb[1], rgb[2], alpha]);
                    }
                    Mode::Rgb => {}
                }
                
                self.hue = hue;
                self.textures.signature = None;
            }
            Drag::Alpha => {
                let mut color = self.linear;
                color[3] =
                    ((point[0] - layout.alpha.x) / layout.alpha.width.max(1.0)).clamp(0.0, 1.0);
                self.set_linear(color);
            }
        }
    }

    fn texture_signature(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.mode.hash(&mut hasher);
        self.hue.to_bits().hash(&mut hasher);
        for channel in self.linear {
            channel.to_bits().hash(&mut hasher);
        }
        hasher.finish()
    }

    fn plane_pixels(&self) -> Vec<u8> {
        let width = TEXTURE_WIDTH as usize;
        let height = COLOR_AREA_HEIGHT as usize;
        let mut pixels = vec![0u8; width * height * 4];
        let srgb_current = linear_to_srgb_rgba(self.linear);
        let texture_rect = Rect::new(0.0, 0.0, width as f32, height as f32);
        let hue = self.current_hue();
        let geometry = wheel_geometry(texture_rect, hue);

        for y in 0..height {
            let fy = y as f32 / (height - 1) as f32;
            for x in 0..width {
                let fx = x as f32 / (width - 1) as f32;
                let srgb = if self.mode == Mode::Rgb {
                    let channel = ((fy * 3.0).floor() as usize).min(2);
                    let mut rgb = srgb_current;
                    rgb[channel] = fx;
                    [rgb[0], rgb[1], rgb[2], 1.0]
                } else {
                    wheel_triangle_pixel(self.mode, hue, geometry, [x as f32 + 0.5, y as f32 + 0.5])
                };
                write_pixel(&mut pixels, (y * width + x) * 4, srgb);
            }
        }
        pixels
    }
}

#[derive(Clone, Copy)]
struct WheelGeometry {
    center: [f32; 2],
    outer_radius: f32,
    inner_radius: f32,
    hue: [f32; 2],
    white: [f32; 2],
    black: [f32; 2],
}

fn wheel_geometry(rect: Rect, hue: f32) -> WheelGeometry {
    let center = [rect.x + rect.width * 0.5, rect.y + rect.height * 0.5];
    let outer_radius = (rect.width.min(rect.height) * 0.495).max(1.0);
    let inner_radius = outer_radius * 0.78;
    let triangle_radius = (inner_radius * 0.96).max(1.0);
    let hue_angle = hue.rem_euclid(1.0) * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
    WheelGeometry {
        center,
        outer_radius,
        inner_radius,
        hue: point_on_circle(center, triangle_radius, hue_angle),
        white: point_on_circle(
            center,
            triangle_radius,
            hue_angle - std::f32::consts::TAU / 3.0,
        ),
        black: point_on_circle(
            center,
            triangle_radius,
            hue_angle + std::f32::consts::TAU / 3.0,
        ),
    }
}

fn point_on_circle(center: [f32; 2], radius: f32, angle: f32) -> [f32; 2] {
    [
        center[0] + angle.cos() * radius,
        center[1] + angle.sin() * radius,
    ]
}

fn wheel_ring_contains(rect: Rect, point: [f32; 2]) -> bool {
    let geometry = wheel_geometry(rect, 0.0);
    let dx = point[0] - geometry.center[0];
    let dy = point[1] - geometry.center[1];
    let distance = (dx * dx + dy * dy).sqrt();
    distance >= geometry.inner_radius && distance <= geometry.outer_radius
}

fn triangle_contains(rect: Rect, hue: f32, point: [f32; 2]) -> bool {
    triangle_weights(rect, hue, point)
        .into_iter()
        .all(|weight| weight >= -1e-4)
}

fn triangle_weights(rect: Rect, hue: f32, point: [f32; 2]) -> [f32; 3] {
    let geometry = wheel_geometry(rect, hue);
    barycentric(point, geometry.hue, geometry.white, geometry.black)
}

fn triangle_weights_clamped(rect: Rect, hue: f32, point: [f32; 2]) -> [f32; 3] {
    let geometry = wheel_geometry(rect, hue);
    let weights = barycentric(point, geometry.hue, geometry.white, geometry.black);
    clamp_triangle_weights(geometry, point, weights)
}

fn clamp_triangle_weights(geometry: WheelGeometry, point: [f32; 2], weights: [f32; 3]) -> [f32; 3] {
    if weights.into_iter().all(|weight| weight >= 0.0) {
        return weights;
    }

    let candidates = [
        closest_point_on_segment(point, geometry.hue, geometry.white),
        closest_point_on_segment(point, geometry.white, geometry.black),
        closest_point_on_segment(point, geometry.black, geometry.hue),
    ];
    let closest = candidates
        .into_iter()
        .min_by(|a, b| distance_squared(point, *a).total_cmp(&distance_squared(point, *b)))
        .unwrap_or(geometry.hue);
    let mut weights = barycentric(closest, geometry.hue, geometry.white, geometry.black);
    for weight in &mut weights {
        *weight = weight.clamp(0.0, 1.0);
    }
    let sum = weights.iter().sum::<f32>().max(1e-7);
    [weights[0] / sum, weights[1] / sum, weights[2] / sum]
}

fn barycentric(point: [f32; 2], a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> [f32; 3] {
    let v0 = [b[0] - a[0], b[1] - a[1]];
    let v1 = [c[0] - a[0], c[1] - a[1]];
    let v2 = [point[0] - a[0], point[1] - a[1]];
    let d00 = v0[0] * v0[0] + v0[1] * v0[1];
    let d01 = v0[0] * v1[0] + v0[1] * v1[1];
    let d11 = v1[0] * v1[0] + v1[1] * v1[1];
    let d20 = v2[0] * v0[0] + v2[1] * v0[1];
    let d21 = v2[0] * v1[0] + v2[1] * v1[1];
    let denominator = d00 * d11 - d01 * d01;
    if denominator.abs() <= 1e-7 {
        return [1.0, 0.0, 0.0];
    }
    let white = (d11 * d20 - d01 * d21) / denominator;
    let black = (d00 * d21 - d01 * d20) / denominator;
    [1.0 - white - black, white, black]
}

fn closest_point_on_segment(point: [f32; 2], a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    let ab = [b[0] - a[0], b[1] - a[1]];
    let length_squared = ab[0] * ab[0] + ab[1] * ab[1];
    if length_squared <= 1e-7 {
        return a;
    }
    let t =
        (((point[0] - a[0]) * ab[0] + (point[1] - a[1]) * ab[1]) / length_squared).clamp(0.0, 1.0);
    [a[0] + ab[0] * t, a[1] + ab[1] * t]
}

fn distance_squared(a: [f32; 2], b: [f32; 2]) -> f32 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    dx * dx + dy * dy
}

fn hsv_triangle_weights(saturation: f32, value: f32) -> [f32; 3] {
    let value = value.clamp(0.0, 1.0);
    let hue = saturation.clamp(0.0, 1.0) * value;
    let white = value - hue;
    [hue, white, 1.0 - value]
}

fn hsv_from_triangle_weights(weights: [f32; 3]) -> [f32; 2] {
    let value = (weights[0] + weights[1]).clamp(0.0, 1.0);
    let saturation = if value <= 1e-7 {
        0.0
    } else {
        (weights[0] / value).clamp(0.0, 1.0)
    };
    [saturation, value]
}

fn okhsl_triangle_weights(saturation: f32, lightness: f32) -> [f32; 3] {
    let lightness = lightness.clamp(0.0, 1.0);
    let hue = saturation.clamp(0.0, 1.0) * 2.0 * lightness.min(1.0 - lightness);
    let white = (lightness - hue * 0.5).clamp(0.0, 1.0);
    [hue, white, (1.0 - hue - white).clamp(0.0, 1.0)]
}

fn okhsl_from_triangle_weights(weights: [f32; 3]) -> [f32; 2] {
    let lightness = (weights[1] + weights[0] * 0.5).clamp(0.0, 1.0);
    let max_hue = 2.0 * lightness.min(1.0 - lightness);
    let saturation = if max_hue <= 1e-7 {
        0.0
    } else {
        (weights[0] / max_hue).clamp(0.0, 1.0)
    };
    [saturation, lightness]
}

fn wheel_triangle_pixel(
    mode: Mode,
    selected_hue: f32,
    geometry: WheelGeometry,
    point: [f32; 2],
) -> [f32; 4] {
    
    
    const AA_RADIUS: f32 = 0.85;

    let dx = point[0] - geometry.center[0];
    let dy = point[1] - geometry.center[1];
    let distance = (dx * dx + dy * dy).sqrt();
    let ring_distance = (geometry.outer_radius - distance).min(distance - geometry.inner_radius);
    let ring_coverage = antialias_coverage(ring_distance, AA_RADIUS);
    if ring_distance > -AA_RADIUS * 2.0 {
        let hue =
            ((dy.atan2(dx) + std::f32::consts::FRAC_PI_2) / std::f32::consts::TAU).rem_euclid(1.0);
        let mut color = hue_color(mode, hue);
        color[3] = ring_coverage;
        if ring_coverage > 0.0 {
            return color;
        }
        
        
        if ring_distance > -AA_RADIUS * 1.25 {
            return color;
        }
    }

    let weights = barycentric(point, geometry.hue, geometry.white, geometry.black);
    let triangle_distance = triangle_signed_distance(geometry, weights);
    if triangle_distance <= -AA_RADIUS * 2.0 {
        return [0.0, 0.0, 0.0, 0.0];
    }
    let weights = clamp_triangle_weights(geometry, point, weights);
    let coverage = antialias_coverage(triangle_distance, AA_RADIUS);
    let mut color = match mode {
        Mode::Hsv => {
            let [saturation, value] = hsv_from_triangle_weights(weights);
            let rgb = hsv_to_rgb([selected_hue, saturation, value]);
            [rgb[0], rgb[1], rgb[2], 1.0]
        }
        Mode::Okhsl => {
            let [saturation, lightness] = okhsl_from_triangle_weights(weights);
            let linear = okhsl_to_linear_rgb([selected_hue, saturation, lightness]);
            linear_to_srgb_rgba([linear[0], linear[1], linear[2], 1.0])
        }
        Mode::Rgb => [0.0, 0.0, 0.0, 0.0],
    };
    color[3] = coverage;
    color
}

fn antialias_coverage(signed_distance: f32, radius: f32) -> f32 {
    let t = ((signed_distance + radius) / (radius * 2.0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn triangle_signed_distance(geometry: WheelGeometry, weights: [f32; 3]) -> f32 {
    let hue_altitude = point_line_distance(geometry.hue, geometry.white, geometry.black);
    let white_altitude = point_line_distance(geometry.white, geometry.black, geometry.hue);
    let black_altitude = point_line_distance(geometry.black, geometry.hue, geometry.white);
    (weights[0] * hue_altitude)
        .min(weights[1] * white_altitude)
        .min(weights[2] * black_altitude)
}

fn point_line_distance(point: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let ab = [b[0] - a[0], b[1] - a[1]];
    let length = (ab[0] * ab[0] + ab[1] * ab[1]).sqrt();
    if length <= 1e-7 {
        return 0.0;
    }
    ((point[0] - a[0]) * ab[1] - (point[1] - a[1]) * ab[0]).abs() / length
}

fn hue_color(mode: Mode, hue: f32) -> [f32; 4] {
    match mode {
        Mode::Okhsl => {
            let linear = okhsl_to_linear_rgb([hue, 1.0, 0.5]);
            linear_to_srgb_rgba([linear[0], linear[1], linear[2], 1.0])
        }
        Mode::Hsv | Mode::Rgb => {
            let rgb = hsv_to_rgb([hue, 1.0, 1.0]);
            [rgb[0], rgb[1], rgb[2], 1.0]
        }
    }
}

fn wheel_handles(
    ctx: &mut BuildCtx,
    id: &impl Display,
    rect: Rect,
    hue: f32,
    weights: [f32; 3],
    suffix: &str,
    opacity: f32,
) {
    let geometry = wheel_geometry(rect, hue);
    let hue_radius = (geometry.inner_radius + geometry.outer_radius) * 0.5;
    let hue_point = point_on_circle(
        geometry.center,
        hue_radius,
        hue.rem_euclid(1.0) * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2,
    );
    let selection = [
        geometry.hue[0] * weights[0]
            + geometry.white[0] * weights[1]
            + geometry.black[0] * weights[2],
        geometry.hue[1] * weights[0]
            + geometry.white[1] * weights[1]
            + geometry.black[1] * weights[2],
    ];

    crate::ui!(ctx, {
        Rect(@format("color-picker-hue-marker-outer {id} {suffix}"), Rect::new(
            hue_point[0] - 5.5, hue_point[1] - 5.5, 11.0, 11.0,
        )) {
            overlay; opacity: opacity; fill: Color::TRANSPARENT; border: 2;
            border_color: Color::BLACK; border_radius: 5.5;
        }
        Rect(@format("color-picker-hue-marker-inner {id} {suffix}"), Rect::new(
            hue_point[0] - 4.0, hue_point[1] - 4.0, 8.0, 8.0,
        )) {
            overlay; opacity: opacity; fill: Color::TRANSPARENT; border: 1;
            border_color: Color::WHITE; border_radius: 4.0;
        }
        Rect(@format("color-picker-triangle-marker-outer {id} {suffix}"), Rect::new(
            selection[0] - 5.0, selection[1] - 5.0, 10.0, 10.0,
        )) {
            overlay; opacity: opacity; fill: Color::TRANSPARENT; border: 2;
            border_color: Color::BLACK; border_radius: 5.0;
        }
        Rect(@format("color-picker-triangle-marker-inner {id} {suffix}"), Rect::new(
            selection[0] - 3.5, selection[1] - 3.5, 7.0, 7.0,
        )) {
            overlay; opacity: opacity; fill: Color::TRANSPARENT; border: 1;
            border_color: Color::WHITE; border_radius: 3.5;
        }
    });
}

fn strip_handle(
    ctx: &mut BuildCtx,
    id: &impl Display,
    rect: Rect,
    value: f32,
    suffix: &str,
    opacity: f32,
) {
    let x = rect.x + value.clamp(0.0, 1.0) * rect.width;
    crate::ui!(ctx, {
        Rect(@format("color-picker-strip-handle {id} {suffix}"), Rect::new(x - 2.0, rect.y - 2.0, 4.0, rect.height + 4.0)) {
            overlay; opacity: opacity; fill: Color::WHITE; border: 1;
            border_color: Color::BLACK; border_radius: 2.0;
        }
    });
}

fn rgb_channel_layout(plane: Rect) -> ([Rect; 3], [Rect; 3]) {
    let ((rows, labels), measured) = crate::measure_layout(plane, |ctx| {
        let mut rows = [BlockId(0); 3];
        let mut labels = [BlockId(0); 3];
        ctx.new()
            .column()
            .width(Size::Fill)
            .height(Size::Fill)
            .children(|ctx| {
                for channel in 0..3 {
                    rows[channel] = ctx
                        .new()
                        .row()
                        .width(Size::Fill)
                        .height(Size::Fill)
                        .children(|ctx| {
                            ctx.new()
                                .width(Size::Pixels(6.0))
                                .height(Size::Fill)
                                .build();
                            labels[channel] = ctx
                                .new()
                                .width(Size::Pixels(18.0))
                                .height(Size::Fill)
                                .build();
                            ctx.new().width(Size::Fill).height(Size::Fill).build();
                        })
                        .build();
                }
            })
            .build();
        (rows, labels)
    });
    (
        rows.map(|id| measured.rect(id).expect("color picker RGB row layout")),
        labels.map(|id| measured.rect(id).expect("color picker RGB label layout")),
    )
}

fn sync_texture(
    renderer: &mut Renderer,
    texture: &mut Option<TextureId>,
    width: u32,
    height: u32,
    pixels: &[u8],
) -> Result<()> {
    if let Some(id) = *texture {
        renderer.update_texture_rgba8(id, pixels)?;
    } else {
        *texture = Some(renderer.register_texture_rgba8(width, height, pixels)?);
    }
    Ok(())
}

fn checker_pixels(width: u32, height: u32, tile: usize) -> Vec<u8> {
    let width = width as usize;
    let height = height as usize;
    let mut pixels = vec![0u8; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            let checker = if ((x / tile) + (y / tile)).is_multiple_of(2) {
                0.30
            } else {
                0.52
            };
            write_pixel(
                &mut pixels,
                (y * width + x) * 4,
                [checker, checker, checker, 1.0],
            );
        }
    }
    pixels
}

fn alpha_pixels(linear: [f32; 4]) -> Vec<u8> {
    let width = TEXTURE_WIDTH as usize;
    let height = STRIP_HEIGHT as usize;
    let straight = linear_to_srgb_rgba([linear[0], linear[1], linear[2], 1.0]);
    let mut pixels = vec![0u8; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            let alpha = x as f32 / (width - 1) as f32;
            let checker = if ((x / 8) + (y / 8)) % 2 == 0 {
                0.30
            } else {
                0.52
            };
            write_pixel(
                &mut pixels,
                (y * width + x) * 4,
                [
                    straight[0] * alpha + checker * (1.0 - alpha),
                    straight[1] * alpha + checker * (1.0 - alpha),
                    straight[2] * alpha + checker * (1.0 - alpha),
                    1.0,
                ],
            );
        }
    }
    pixels
}

fn write_pixel(pixels: &mut [u8], offset: usize, srgb: [f32; 4]) {
    for channel in 0..4 {
        pixels[offset + channel] = (srgb[channel].clamp(0.0, 1.0) * 255.0).round() as u8;
    }
}

fn ui_color(linear: [f32; 4]) -> Color {
    Color::from_linear(linear)
}

fn linear_to_srgb_rgba(value: [f32; 4]) -> [f32; 4] {
    [
        linear_to_srgb(value[0]),
        linear_to_srgb(value[1]),
        linear_to_srgb(value[2]),
        value[3].clamp(0.0, 1.0),
    ]
}
fn srgb_to_linear_rgba(value: [f32; 4]) -> [f32; 4] {
    [
        srgb_to_linear(value[0]),
        srgb_to_linear(value[1]),
        srgb_to_linear(value[2]),
        value[3].clamp(0.0, 1.0),
    ]
}
fn linear_to_srgb(value: f32) -> f32 {
    if value <= 0.0031308 {
        value * 12.92
    } else {
        1.055 * value.max(0.0).powf(1.0 / 2.4) - 0.055
    }
}
fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn rgb_to_hsv(rgb: [f32; 3]) -> [f32; 3] {
    let [r, g, b] = rgb;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let mut h = if delta <= 1e-7 {
        0.0
    } else if max == r {
        ((g - b) / delta).rem_euclid(6.0)
    } else if max == g {
        (b - r) / delta + 2.0
    } else {
        (r - g) / delta + 4.0
    } / 6.0;
    if !h.is_finite() {
        h = 0.0;
    }
    let s = if max <= 1e-7 { 0.0 } else { delta / max };
    [h.rem_euclid(1.0), s.clamp(0.0, 1.0), max.clamp(0.0, 1.0)]
}

fn hsv_to_rgb(hsv: [f32; 3]) -> [f32; 3] {
    let h = hsv[0].rem_euclid(1.0) * 6.0;
    let s = hsv[1].clamp(0.0, 1.0);
    let v = hsv[2].clamp(0.0, 1.0);
    let c = v * s;
    let x = c * (1.0 - ((h.rem_euclid(2.0)) - 1.0).abs());
    let (r, g, b) = match h.floor() as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [r + m, g + m, b + m]
}

fn rgba_hex(linear: [f32; 4]) -> String {
    let srgb = linear_to_srgb_rgba(linear);
    format!(
        "{:02X}{:02X}{:02X}{:02X}",
        byte(srgb[0]),
        byte(srgb[1]),
        byte(srgb[2]),
        byte(srgb[3])
    )
}
fn parse_rgba_hex(text: &str) -> Option<[f32; 4]> {
    let text = text.trim().trim_start_matches('#');
    let (rgb, alpha) = match text.len() {
        6 => (&text[..6], 255),
        8 => (&text[..6], u8::from_str_radix(&text[6..8], 16).ok()?),
        _ => return None,
    };
    let r = u8::from_str_radix(&rgb[0..2], 16).ok()? as f32 / 255.0;
    let g = u8::from_str_radix(&rgb[2..4], 16).ok()? as f32 / 255.0;
    let b = u8::from_str_radix(&rgb[4..6], 16).ok()? as f32 / 255.0;
    Some(srgb_to_linear_rgba([r, g, b, alpha as f32 / 255.0]))
}
fn byte(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn linear_rgb_to_okhsl(rgb: [f32; 3]) -> [f32; 3] {
    let lab = linear_srgb_to_oklab(rgb);
    let chroma = (lab[1] * lab[1] + lab[2] * lab[2]).sqrt();
    if chroma <= 1e-7 {
        return [0.0, 0.0, toe(lab[0]).clamp(0.0, 1.0)];
    }
    let a = lab[1] / chroma;
    let b = lab[2] / chroma;
    let h = (0.5 + 0.5 * (-lab[2]).atan2(-lab[1]) / std::f32::consts::PI).rem_euclid(1.0);
    let [c0, cmid, cmax] = okhsl_chromas(lab[0], a, b);
    let mid = 0.8;
    let s = if chroma < cmid {
        let k1 = mid * c0;
        let k2 = 1.0 - safe_div(k1, cmid);
        let t = safe_div(chroma, k1 + k2 * chroma);
        t * mid
    } else {
        let k0 = cmid;
        let k1 = (1.0 - mid) * cmid * cmid * 1.25 * 1.25 / c0.max(1e-7);
        let k2 = 1.0 - safe_div(k1, cmax - cmid);
        let delta = chroma - k0;
        let t = safe_div(delta, k1 + k2 * delta);
        mid + (1.0 - mid) * t
    };
    [h, s.clamp(0.0, 1.0), toe(lab[0]).clamp(0.0, 1.0)]
}
fn okhsl_to_linear_rgb(okhsl: [f32; 3]) -> [f32; 3] {
    let h = okhsl[0].rem_euclid(1.0);
    let s = okhsl[1].clamp(0.0, 1.0);
    let l = okhsl[2].clamp(0.0, 1.0);
    if l <= 0.0 {
        return [0.0; 3];
    }
    if l >= 1.0 {
        return [1.0; 3];
    }
    let angle = 2.0 * std::f32::consts::PI * h;
    let a = angle.cos();
    let b = angle.sin();
    let lightness = toe_inv(l);
    let [c0, cmid, cmax] = okhsl_chromas(lightness, a, b);
    let mid = 0.8;
    let chroma = if s < mid {
        let t = 1.25 * s;
        let k1 = mid * c0;
        let k2 = 1.0 - safe_div(k1, cmid);
        safe_div(t * k1, 1.0 - k2 * t)
    } else {
        let t = (s - mid) / (1.0 - mid);
        let k0 = cmid;
        let k1 = (1.0 - mid) * cmid * cmid * 1.25 * 1.25 / c0.max(1e-7);
        let k2 = 1.0 - safe_div(k1, cmax - cmid);
        k0 + safe_div(t * k1, 1.0 - k2 * t)
    };
    let rgb = oklab_to_linear_srgb([lightness, chroma * a, chroma * b]);
    [
        rgb[0].clamp(0.0, 1.0),
        rgb[1].clamp(0.0, 1.0),
        rgb[2].clamp(0.0, 1.0),
    ]
}
fn okhsl_chromas(l: f32, a: f32, b: f32) -> [f32; 3] {
    let cmax = max_chroma(l, a, b);
    let [lc, cc] = numeric_cusp(a, b);
    let smax = cc / lc.max(1e-6);
    let tmax = cc / (1.0 - lc).max(1e-6);
    let triangle = (l * smax).min((1.0 - l) * tmax).max(1e-7);
    let k = cmax / triangle;
    let [smid, tmid] = st_mid(a, b);
    let ca = (l * smid).max(1e-7);
    let cb = ((1.0 - l) * tmid).max(1e-7);
    let cmid = 0.9 * k * (1.0 / (1.0 / ca.powi(4) + 1.0 / cb.powi(4))).sqrt().sqrt();
    let ca0 = (l * 0.4).max(1e-7);
    let cb0 = ((1.0 - l) * 0.8).max(1e-7);
    let c0 = (1.0 / (1.0 / (ca0 * ca0) + 1.0 / (cb0 * cb0))).sqrt();
    [
        c0.max(1e-7),
        cmid.clamp(1e-7, cmax.max(1e-7)),
        cmax.max(1e-7),
    ]
}
fn numeric_cusp(a: f32, b: f32) -> [f32; 2] {
    let (mut lo, mut hi) = (0.001, 0.999);
    for _ in 0..18 {
        let l1 = lo + (hi - lo) / 3.0;
        let l2 = hi - (hi - lo) / 3.0;
        if max_chroma(l1, a, b) < max_chroma(l2, a, b) {
            lo = l1
        } else {
            hi = l2
        }
    }
    let l = (lo + hi) * 0.5;
    [l, max_chroma(l, a, b)]
}
fn max_chroma(l: f32, a: f32, b: f32) -> f32 {
    if l <= 0.0 || l >= 1.0 {
        return 0.0;
    }
    let (mut lo, mut hi) = (0.0, 0.6);
    while in_gamut(oklab_to_linear_srgb([l, hi * a, hi * b])) && hi < 2.0 {
        hi *= 1.5
    }
    for _ in 0..18 {
        let mid = (lo + hi) * 0.5;
        if in_gamut(oklab_to_linear_srgb([l, mid * a, mid * b])) {
            lo = mid
        } else {
            hi = mid
        }
    }
    lo
}
fn st_mid(a: f32, b: f32) -> [f32; 2] {
    let s = 0.11516993
        + 1.0
            / (7.4477897
                + 4.1590123 * b
                + a * (-2.1955736
                    + 1.751984 * b
                    + a * (-2.1370494 - 10.02301 * b
                        + a * (-4.2489457 + 5.387708 * b + 4.69891 * a))));
    let t = 0.11239642
        + 1.0
            / (1.6132032 - 0.6812438 * b
                + a * (0.40370613
                    + 0.9014812 * b
                    + a * (-0.27087942
                        + 0.6122399 * b
                        + a * (0.00299215 - 0.45399567 * b - 0.14661872 * a))));
    [s, t]
}
fn toe(x: f32) -> f32 {
    let k1 = 0.206;
    let k2 = 0.03;
    let k3 = (1.0 + k1) / (1.0 + k2);
    0.5 * (k3 * x - k1 + ((k3 * x - k1).powi(2) + 4.0 * k2 * k3 * x).sqrt())
}
fn toe_inv(x: f32) -> f32 {
    let k1 = 0.206;
    let k2 = 0.03;
    let k3 = (1.0 + k1) / (1.0 + k2);
    (x * x + k1 * x) / (k3 * (x + k2).max(1e-7))
}
fn linear_srgb_to_oklab(rgb: [f32; 3]) -> [f32; 3] {
    let l = (0.41222146 * rgb[0] + 0.53633255 * rgb[1] + 0.051445995 * rgb[2]).cbrt();
    let m = (0.2119035 * rgb[0] + 0.6806995 * rgb[1] + 0.10739696 * rgb[2]).cbrt();
    let s = (0.08830246 * rgb[0] + 0.28171885 * rgb[1] + 0.6299787 * rgb[2]).cbrt();
    [
        0.21045426 * l + 0.7936178 * m - 0.004072047 * s,
        1.9779985 * l - 2.4285922 * m + 0.4505937 * s,
        0.025904037 * l + 0.78277177 * m - 0.80867577 * s,
    ]
}
fn oklab_to_linear_srgb(lab: [f32; 3]) -> [f32; 3] {
    let l = (lab[0] + 0.39633778 * lab[1] + 0.21580376 * lab[2]).powi(3);
    let m = (lab[0] - 0.105561346 * lab[1] - 0.06385417 * lab[2]).powi(3);
    let s = (lab[0] - 0.08948418 * lab[1] - 1.2914855 * lab[2]).powi(3);
    [
        4.0767417 * l - 3.3077116 * m + 0.23096994 * s,
        -1.268438 * l + 2.6097574 * m - 0.34131938 * s,
        -0.0041960864 * l - 0.7034186 * m + 1.7076147 * s,
    ]
}
fn in_gamut(rgb: [f32; 3]) -> bool {
    rgb.into_iter().all(|v| (-1e-5..=1.00001).contains(&v))
}
fn safe_div(a: f32, b: f32) -> f32 {
    if b.abs() <= 1e-7 {
        0.0
    } else {
        a / b
    }
}
