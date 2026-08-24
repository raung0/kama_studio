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

use crate::{
    Align, BlockId, BuildCtx, Color, CursorShape, LayoutRects, PopupDirection, PopupState, Rect,
    Renderer, Size, TextureId,
};

use super::{ease, ColorButton, Style, TextEdit};

const SPEED: f32 = 18.0;
const POPUP_W: f32 = 300.0;
const TEXTURE_WIDTH: u32 = 256;
const COLOR_AREA_HEIGHT: u32 = 256;
const STRIP_HEIGHT: u32 = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Mode {
    Hsv,
    Rgb,
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
    tabs: [Rect; 2],
    plane: Rect,
    hue: Rect,
    alpha: Rect,
    hex: Rect,
}

#[derive(Clone, Copy)]
struct ContentIds {
    root: BlockId,
    tabs: [BlockId; 2],
    plane: BlockId,
    hue: Option<BlockId>,
    alpha: BlockId,
    hex: BlockId,
}

#[derive(Default)]
struct Textures {
    plane: Option<TextureId>,
    alpha: Option<TextureId>,
    signature: Option<u64>,
}

pub struct ColorPicker {
    linear: [f32; 4],
    hue: f32,
    mode: Mode,
    hex: TextEdit,
    open: PopupState,
    t: f32,
    drag: Option<Drag>,
    textures: Textures,
    built_rect: Option<Rect>,
    window_bounds: Option<Rect>,
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
            open: PopupState::default(),
            t: 0.0,
            drag: None,
            textures: Textures::default(),
            built_rect: None,
            window_bounds: None,
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

    pub fn is_open(&self) -> bool {
        self.open.is_open()
    }

    pub fn close(&mut self) {
        self.open.close();
        self.drag = None;
        self.hex.set_focused(false);
    }

    pub fn open_and_focus_hex(&mut self) {
        self.open.set_open(true);
        self.t = self.t.max(0.05);
        self.drag = None;
        self.hex.set_focused(true);
    }

    pub fn tick(&mut self, dt: f32) {
        ease(&mut self.t, self.open.is_open() as u8 as f32, SPEED, dt);
        self.hex.tick(dt);
    }

    pub fn is_animating(&self) -> bool {
        (self.t - self.open.is_open() as u8 as f32).abs() > 0.001 || self.hex.is_animating()
    }

    pub fn is_editing(&self) -> bool {
        self.open.is_open() && self.hex.is_focused()
    }

    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    pub fn caret_rect(&self, rect: Rect) -> Option<Rect> {
        self.caret_rect_bounded(rect, self.effective_window_bounds(rect))
    }

    pub fn caret_rect_in(&self, rect: Rect, bounds: Rect) -> Option<Rect> {
        self.caret_rect_bounded(rect, Some(bounds))
    }

    fn caret_rect_bounded(&self, rect: Rect, bounds: Option<Rect>) -> Option<Rect> {
        self.is_editing()
            .then(|| self.hex.caret_rect(self.layout(rect, bounds).hex))
    }

    pub fn popup_contains(&self, rect: Rect, point: [f32; 2]) -> bool {
        self.popup_contains_bounded(rect, self.effective_window_bounds(rect), point)
    }

    pub fn popup_contains_in(&self, rect: Rect, bounds: Rect, point: [f32; 2]) -> bool {
        self.popup_contains_bounded(rect, Some(bounds), point)
    }

    fn popup_contains_bounded(&self, rect: Rect, bounds: Option<Rect>, point: [f32; 2]) -> bool {
        self.open.is_open() && self.layout(rect, bounds).popup.contains(point)
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
        self.textures.signature = Some(signature);
        Ok(())
    }

    pub fn pointer_pressed(
        &mut self,
        rect: Rect,
        point: [f32; 2],
        modifiers: ModifiersState,
    ) -> bool {
        self.pointer_pressed_bounded(rect, self.effective_window_bounds(rect), point, modifiers)
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
        if self.open.is_open() && layout.popup.contains(point) {
            for (index, tab) in layout.tabs.into_iter().enumerate() {
                if tab.contains(point) {
                    self.mode = [Mode::Hsv, Mode::Rgb][index];
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
            self.open.toggle();
            if self.open.is_open() {
                self.t = self.t.max(0.05);
            } else {
                self.drag = None;
                self.hex.set_focused(false);
            }
            return true;
        }
        let was_open = self.open.is_open();
        self.close();
        was_open
    }

    pub fn pointer_moved(&mut self, rect: Rect, point: [f32; 2]) -> bool {
        self.pointer_moved_bounded(rect, self.effective_window_bounds(rect), point)
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
        self.built_rect = Some(rect);
        self.window_bounds = bounds;
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
                top_overlay; backdrop_blur: 30.0; backdrop_tint: popup_tint;
                opacity: opacity; fill: shadow; border_radius: style.radius_md;
            }
            Rect(@format("color-picker-popup {id}"), layout.popup) {
                top_overlay; dismissible_popup: self.open.clone(); backdrop_blur: 22.0; backdrop_tint: popup_tint;
                opacity: opacity; fill: popup_bg;
                border: 1; border_color: style.accent; border_radius: style.radius_md;
            }
            @for (index, (tab, label, mode)) in [
                (layout.tabs[0], "HSV", Mode::Hsv),
                (layout.tabs[1], "RGB", Mode::Rgb),
            ].into_iter().enumerate() {
                @let selected = self.mode == mode;
                Rect(@format("color-picker-tab {id} {index}"), tab) {
                    top_overlay; opacity: opacity; fill: if selected { style.focused } else { style.control };
                    border: 1; border_color: if selected { style.accent } else { style.border };
                    border_radius: style.radius_sm; font_size: 9.0;
                    text_color: if selected { style.text } else { style.muted };
                    text_centered; text: label; interactive;
                }
            }
        });

        if self.mode == Mode::Rgb {
            crate::ui!(ctx, {
                Rect(@format("color-picker-plane {id}"), layout.plane) {
                    top_overlay; opacity: opacity; fill: Color::WHITE; border: 1;
                    border_color: style.border; border_radius: style.radius_sm;
                    interactive; border_reveal;
                    fill_texture_opt: self.textures.plane;
                }
            });
        } else {
            crate::ui!(ctx, {
                Rect(@format("color-picker-wheel {id}"), layout.plane) {
                    top_overlay; opacity: opacity; fill: Color::WHITE;
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
                            top_overlay; opacity: opacity; fill: Color::WHITE; border: 1;
                            border_color: Color::BLACK; border_radius: 2.0;
                        }
                        Rect(@format("color-picker-rgb-label {id} {channel}"), labels[channel]) {
                            top_overlay; opacity: opacity; font_size: 8.5;
                            text_color: if srgb[channel] > 0.62 { Color::BLACK } else { Color::WHITE };
                            text_centered; text: ["R", "G", "B"][channel];
                        }
                    }
                });
                crate::ui!(ctx, {
                    Rect(@format("color-picker-rgb-values {id}"), layout.hue) {
                        top_overlay; opacity: opacity; font_size: 8.5; text_color: style.muted; text_centered;
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
                top_overlay; opacity: opacity; fill: Color::WHITE; border: 1;
                border_color: style.border; border_radius: style.radius_sm; interactive; border_reveal;
                fill_texture_opt: self.textures.alpha;
            }
            Rect(@format("color-picker-hex {id}"), layout.hex) {
                top_overlay; opacity: opacity;
                fill: if self.hex.is_focused() { style.focused } else { style.control };
                border: 1; border_color: if self.hex.is_focused() { style.accent } else { style.border };
                border_radius: style.radius_sm; padding: 7.0; font_size: 10.5;
                text_color: style.text; text: self.hex.text().to_string();
            }
            @if self.hex.is_focused() {
                Rect(@format("color-picker-hex-caret {id}"), self.hex.caret_rect(layout.hex)) {
                    top_overlay; opacity: opacity; fill: style.text;
                }
            }
        });
        strip_handle(ctx, &id, layout.alpha, self.linear[3], "alpha", opacity);
    }

    fn layout(&self, rect: Rect, bounds: Option<Rect>) -> Layout {
        let width = POPUP_W;
        let max_plane_size = (width - 16.0).clamp(1.0, 210.0);
        let probe_viewport = Rect::new(0.0, 0.0, width, 1.0);
        let (probe_ids, probe) = self.measure_content(probe_viewport, max_plane_size, Size::Fit);
        let desired_height = probe
            .rect(probe_ids.root)
            .expect("color picker content layout")
            .height;

        let mut placement = if let Some(bounds) = bounds {
            crate::place_popup_with_direction(rect, [width, desired_height], bounds, false, 4.0)
        } else {
            crate::PopupPlacement {
                rect: Rect::new(rect.x, rect.bottom() + 4.0, width, desired_height),
                direction: PopupDirection::Down,
            }
        };

        let overflow = (desired_height - placement.rect.height).max(0.0);
        let plane_size = (max_plane_size - overflow).max(48.0);
        if overflow > 0.001 {
            let (fit_ids, fit) = self.measure_content(probe_viewport, plane_size, Size::Fit);
            let fit_height = fit
                .rect(fit_ids.root)
                .expect("color picker fitted content layout")
                .height;
            if let Some(bounds) = bounds {
                placement = crate::place_popup_with_direction(
                    rect,
                    [width, fit_height],
                    bounds,
                    placement.direction == PopupDirection::Up,
                    4.0,
                );
            } else {
                placement.rect.height = fit_height;
            }
        }

        let animated_height = placement.rect.height * self.t.max(0.001);
        let popup = match placement.direction {
            PopupDirection::Down => Rect::new(
                placement.rect.x,
                placement.rect.y,
                placement.rect.width,
                animated_height,
            ),
            PopupDirection::Up => Rect::new(
                placement.rect.x,
                placement.rect.bottom() - animated_height,
                placement.rect.width,
                animated_height,
            ),
        };

        let (ids, measured) = self.measure_content(popup, plane_size, Size::Fill);
        let alpha = measured.rect(ids.alpha).expect("color picker alpha layout");
        Layout {
            popup,
            tabs: ids
                .tabs
                .map(|id| measured.rect(id).expect("color picker tab layout")),
            plane: measured.rect(ids.plane).expect("color picker plane layout"),
            hue: ids.hue.and_then(|id| measured.rect(id)).unwrap_or(alpha),
            alpha,
            hex: measured.rect(ids.hex).expect("color picker hex layout"),
        }
    }

    fn effective_window_bounds(&self, rect: Rect) -> Option<Rect> {
        let built = self.built_rect?;
        let mut bounds = self.window_bounds?;
        bounds.x += rect.x - built.x;
        bounds.y += rect.y - built.y;
        Some(bounds)
    }

    fn measure_content(
        &self,
        viewport: Rect,
        plane_size: f32,
        height: Size,
    ) -> (ContentIds, LayoutRects) {
        crate::measure_layout(viewport, |ctx| {
            let mut tabs = [BlockId(0); 2];
            let mut plane = BlockId(0);
            let mut hue = None;
            let mut alpha = BlockId(0);
            let mut hex = BlockId(0);
            let root = ctx
                .new()
                .column()
                .width(Size::Fill)
                .height(height)
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
            ContentIds {
                root,
                tabs,
                plane,
                hue,
                alpha,
                hex,
            }
        })
    }

    fn apply_hex(&mut self) {
        if let Some(linear) = parse_rgba_hex(self.hex.text()) {
            self.set_linear(linear);
        }
    }

    fn current_hue(&self) -> f32 {
        let srgb = linear_to_srgb_rgba(self.linear);
        let hsv = rgb_to_hsv([srgb[0], srgb[1], srgb[2]]);
        if hsv[1] > 1e-4 && hsv[2] > 1e-4 {
            hsv[0]
        } else {
            self.hue
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

fn hue_color(_mode: Mode, hue: f32) -> [f32; 4] {
    let rgb = hsv_to_rgb([hue, 1.0, 1.0]);
    [rgb[0], rgb[1], rgb[2], 1.0]
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
            top_overlay; opacity: opacity; fill: Color::TRANSPARENT; border: 2;
            border_color: Color::BLACK; border_radius: 5.5;
        }
        Rect(@format("color-picker-hue-marker-inner {id} {suffix}"), Rect::new(
            hue_point[0] - 4.0, hue_point[1] - 4.0, 8.0, 8.0,
        )) {
            top_overlay; opacity: opacity; fill: Color::TRANSPARENT; border: 1;
            border_color: Color::WHITE; border_radius: 4.0;
        }
        Rect(@format("color-picker-triangle-marker-outer {id} {suffix}"), Rect::new(
            selection[0] - 5.0, selection[1] - 5.0, 10.0, 10.0,
        )) {
            top_overlay; opacity: opacity; fill: Color::TRANSPARENT; border: 2;
            border_color: Color::BLACK; border_radius: 5.0;
        }
        Rect(@format("color-picker-triangle-marker-inner {id} {suffix}"), Rect::new(
            selection[0] - 3.5, selection[1] - 3.5, 7.0, 7.0,
        )) {
            top_overlay; opacity: opacity; fill: Color::TRANSPARENT; border: 1;
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
            top_overlay; opacity: opacity; fill: Color::WHITE; border: 1;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popup_height_is_intrinsic_to_mode_content() {
        let mut picker = ColorPicker::new(Color::WHITE);
        picker.t = 1.0;
        let control = Rect::new(40.0, 40.0, 120.0, 24.0);
        let bounds = Rect::new(0.0, 0.0, 500.0, 700.0);

        picker.mode = Mode::Hsv;
        let hsv = picker.layout(control, Some(bounds));
        picker.mode = Mode::Rgb;
        let rgb = picker.layout(control, Some(bounds));

        assert!(rgb.popup.height > hsv.popup.height);
        assert!(hsv.popup.y >= control.bottom() + 4.0 - f32::EPSILON);
        assert!(rgb.popup.y >= control.bottom() + 4.0 - f32::EPSILON);
    }

    #[test]
    fn popup_can_extend_past_panel_inside_window() {
        let mut picker = ColorPicker::new(Color::WHITE);
        picker.t = 1.0;
        let control = Rect::new(40.0, 170.0, 120.0, 24.0);
        let window_bounds = Rect::new(-300.0, -100.0, 900.0, 700.0);

        let popup = picker.layout(control, Some(window_bounds)).popup;

        assert!(popup.y >= control.bottom());
        assert!(popup.bottom() > 200.0);
        assert!(popup.bottom() <= window_bounds.bottom());
    }
}
