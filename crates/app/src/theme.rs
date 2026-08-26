use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};

use kama_ui::Color;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum ThemePreset {
    System,
    Light,
    #[default]
    Dark,
}

static THEME: AtomicU8 = AtomicU8::new(2);
static SYSTEM_LIGHT: AtomicU8 = AtomicU8::new(0);
static THEME_MIX_BITS: AtomicU32 = AtomicU32::new(0.0f32.to_bits());
static DARK_ACCENT_RGBA: AtomicU32 = AtomicU32::new(u32::from_be_bytes([0xc1, 0x2c, 0xff, 0xff]));
static LIGHT_ACCENT_RGBA: AtomicU32 = AtomicU32::new(u32::from_be_bytes([0xa0, 0x70, 0xff, 0xff]));
static BRIGHTNESS_BITS: AtomicU32 = AtomicU32::new(0.08f32.to_bits());
static ACCENT_MIXING_BITS: AtomicU32 = AtomicU32::new(0.03f32.to_bits());

fn resolved_theme(theme: ThemePreset) -> ThemePreset {
    match theme {
        ThemePreset::System => {
            if SYSTEM_LIGHT.load(Ordering::Relaxed) != 0 {
                ThemePreset::Light
            } else {
                ThemePreset::Dark
            }
        }
        theme => theme,
    }
}

fn theme_target(theme: ThemePreset) -> f32 {
    match resolved_theme(theme) {
        ThemePreset::Dark => 0.0,
        ThemePreset::Light => 1.0,
        ThemePreset::System => unreachable!("system theme resolves to light or dark"),
    }
}

fn store_theme(theme: ThemePreset) {
    THEME.store(
        match theme {
            ThemePreset::System => 0,
            ThemePreset::Light => 1,
            ThemePreset::Dark => 2,
        },
        Ordering::Relaxed,
    );
}

fn theme_mix() -> f32 {
    f32::from_bits(THEME_MIX_BITS.load(Ordering::Relaxed)).clamp(0.0, 1.0)
}

fn set_theme_mix(value: f32) {
    THEME_MIX_BITS.store(value.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
}

pub(crate) fn set_theme(theme: ThemePreset) {
    store_theme(theme);
}

pub(crate) fn set_theme_immediate(theme: ThemePreset) {
    store_theme(theme);
    set_theme_mix(theme_target(theme));
    sync_kama_ui_theme();
}

pub(crate) fn theme() -> ThemePreset {
    match THEME.load(Ordering::Relaxed) {
        0 => ThemePreset::System,
        1 => ThemePreset::Light,
        _ => ThemePreset::Dark,
    }
}

pub(crate) fn effective_theme() -> ThemePreset {
    resolved_theme(theme())
}

pub(crate) fn set_system_appearance(light: bool) {
    SYSTEM_LIGHT.store(u8::from(light), Ordering::Relaxed);
    if theme() == ThemePreset::System {
        sync_kama_ui_theme();
    }
}

pub(crate) fn tick(dt: f32) -> bool {
    let target = theme_target(theme());
    let current = theme_mix();
    let step = 1.0 - (-10.0 * dt.max(0.0)).exp();
    let mut next = current + (target - current) * step;
    let animating = (target - next).abs() > 0.001;
    if !animating {
        next = target;
    }
    if (next - current).abs() > f32::EPSILON {
        set_theme_mix(next);
    }
    sync_kama_ui_theme();
    animating
}

pub(crate) fn set_dark_accent_rgba8(rgba: [u8; 4]) {
    DARK_ACCENT_RGBA.store(u32::from_be_bytes(rgba), Ordering::Relaxed);
    sync_kama_ui_theme();
}

pub(crate) fn set_light_accent_rgba8(rgba: [u8; 4]) {
    LIGHT_ACCENT_RGBA.store(u32::from_be_bytes(rgba), Ordering::Relaxed);
    sync_kama_ui_theme();
}

pub(crate) fn dark_accent_rgba8() -> [u8; 4] {
    DARK_ACCENT_RGBA.load(Ordering::Relaxed).to_be_bytes()
}

pub(crate) fn light_accent_rgba8() -> [u8; 4] {
    LIGHT_ACCENT_RGBA.load(Ordering::Relaxed).to_be_bytes()
}

fn rgba8_color(rgba: [u8; 4]) -> Color {
    Color::rgba8(rgba[0], rgba[1], rgba[2], rgba[3])
}

pub(crate) fn accent() -> Color {
    rgba8_color(dark_accent_rgba8()).mix(rgba8_color(light_accent_rgba8()), theme_mix())
}

pub(crate) fn accent_text() -> Color {
    let accent = accent();

    let luminance = accent.r * 0.2126 + accent.g * 0.7152 + accent.b * 0.0722;
    if luminance >= 0.56 {
        Color::BLACK
    } else {
        Color::WHITE
    }
}

pub(crate) fn set_brightness(value: f32) {
    let value = if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.08
    };
    BRIGHTNESS_BITS.store(value.to_bits(), Ordering::Relaxed);
    sync_kama_ui_theme();
}

pub(crate) fn brightness() -> f32 {
    f32::from_bits(BRIGHTNESS_BITS.load(Ordering::Relaxed)).clamp(0.0, 1.0)
}

pub(crate) fn set_accent_mixing(value: f32) {
    let value = if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.03
    };
    ACCENT_MIXING_BITS.store(value.to_bits(), Ordering::Relaxed);
    sync_kama_ui_theme();
}

pub(crate) fn accent_mixing() -> f32 {
    f32::from_bits(ACCENT_MIXING_BITS.load(Ordering::Relaxed)).clamp(0.0, 1.0)
}

fn rgb_to_hsl(color: Color) -> [f32; 3] {
    let max = color.r.max(color.g).max(color.b);
    let min = color.r.min(color.g).min(color.b);
    let lightness = (max + min) * 0.5;
    let delta = max - min;
    if delta <= 1e-6 {
        return [0.0, 0.0, lightness];
    }
    let saturation = delta / (1.0 - (2.0 * lightness - 1.0).abs()).max(1e-6);
    let hue_sector = if max == color.r {
        ((color.g - color.b) / delta).rem_euclid(6.0)
    } else if max == color.g {
        (color.b - color.r) / delta + 2.0
    } else {
        (color.r - color.g) / delta + 4.0
    };
    [
        (hue_sector / 6.0).rem_euclid(1.0),
        saturation.clamp(0.0, 1.0),
        lightness.clamp(0.0, 1.0),
    ]
}

fn hsl_to_rgb(hue: f32, saturation: f32, lightness: f32, alpha: f32) -> Color {
    let hue = hue.rem_euclid(1.0);
    let saturation = saturation.clamp(0.0, 1.0);
    let lightness = lightness.clamp(0.0, 1.0);
    if saturation <= 1e-6 {
        return Color::rgba(lightness, lightness, lightness, alpha);
    }
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let sector = hue * 6.0;
    let x = chroma * (1.0 - (sector.rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match sector.floor() as i32 {
        0 => (chroma, x, 0.0),
        1 => (x, chroma, 0.0),
        2 => (0.0, chroma, x),
        3 => (0.0, x, chroma),
        4 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };
    let offset = lightness - chroma * 0.5;
    Color::rgba(r + offset, g + offset, b + offset, alpha)
}

fn appearance_lightness(base: f32) -> f32 {
    let mix = theme_mix();

    let target = brightness() + (1.0 - 2.0 * brightness()) * mix;

    let reference = 0.122 + (0.973 - 0.122) * mix;
    if target < reference {
        base * (target / reference.max(1e-4))
    } else {
        base + (1.0 - base) * ((target - reference) / (1.0 - reference).max(1e-4))
    }
}

fn surface_color(base: Color) -> Color {
    let [accent_hue, _, _] = rgb_to_hsl(accent());
    let [base_hue, base_saturation, base_lightness] = rgb_to_hsl(base);
    let mixing = accent_mixing();

    let hue = if mixing <= 1e-6 { base_hue } else { accent_hue };
    let saturation = base_saturation + (0.62 - base_saturation) * mixing;
    hsl_to_rgb(
        hue,
        saturation,
        appearance_lightness(base_lightness),
        base.a,
    )
}

fn interaction_overlay(alpha: f32) -> Color {
    let level = 1.0 - theme_mix();
    Color::rgba(level, level, level, alpha)
}

pub(crate) fn accent_hover() -> Color {
    let mix = theme_mix();
    let target = Color::WHITE.mix(Color::BLACK, mix);
    let amount = 0.24 + (0.12 - 0.24) * mix;
    accent().mix(target, amount)
}

#[derive(Clone, Copy)]
struct Palette {
    bg: Color,
    panel: Color,
    tab_bar: Color,
    tab_active: Color,
    control: Color,
    focused: Color,
    line: Color,
    line_soft: Color,
    text: Color,
    muted: Color,
    timeline_bg: Color,
    timeline_bg_alt: Color,
    timeline_header: Color,
    timeline_header_active: Color,
    timeline_line: Color,
    timeline_grid: Color,
    timeline_text: Color,
    timeline_muted: Color,
}

const DARK: Palette = Palette {
    bg: Color::rgb8(0x1f, 0x1f, 0x1f),
    panel: Color::rgb8(0x1f, 0x1f, 0x1f),
    tab_bar: Color::rgb8(0x22, 0x22, 0x22),
    tab_active: Color::rgb8(0x33, 0x33, 0x33),
    control: Color::rgb8(0x28, 0x28, 0x28),
    focused: Color::rgb8(0x3a, 0x3a, 0x3a),
    line: Color::rgb8(0x40, 0x40, 0x40),
    line_soft: Color::rgb8(0x42, 0x49, 0x4e),
    text: Color::rgb8(0xe5, 0xe5, 0xe5),
    muted: Color::rgb8(0xa4, 0xa4, 0xa4),
    timeline_bg: Color::rgb8(0x18, 0x19, 0x1a),
    timeline_bg_alt: Color::rgb8(0x1d, 0x1f, 0x20),
    timeline_header: Color::rgb8(0x23, 0x25, 0x26),
    timeline_header_active: Color::rgb8(0x2b, 0x2d, 0x2f),
    timeline_line: Color::rgb8(0x37, 0x3a, 0x3c),
    timeline_grid: Color::rgba8(0xff, 0xff, 0xff, 0x12),
    timeline_text: Color::rgb8(0xdc, 0xde, 0xdf),
    timeline_muted: Color::rgb8(0x8c, 0x92, 0x95),
};

const LIGHT: Palette = Palette {
    bg: Color::rgb8(0xf1, 0xf2, 0xf4),
    panel: Color::rgb8(0xf8, 0xf8, 0xf9),
    tab_bar: Color::rgb8(0xea, 0xeb, 0xed),
    tab_active: Color::WHITE,
    control: Color::rgb8(0xe7, 0xe8, 0xea),
    focused: Color::rgb8(0xd9, 0xdb, 0xde),
    line: Color::rgb8(0xc5, 0xc8, 0xcd),
    line_soft: Color::rgb8(0xd2, 0xd5, 0xda),

    text: Color::rgb8(0x12, 0x14, 0x17),
    muted: Color::rgb8(0x49, 0x4d, 0x54),
    timeline_bg: Color::rgb8(0xee, 0xef, 0xf1),
    timeline_bg_alt: Color::rgb8(0xe5, 0xe7, 0xea),
    timeline_header: Color::rgb8(0xdc, 0xdf, 0xe3),
    timeline_header_active: Color::rgb8(0xd0, 0xd3, 0xd8),
    timeline_line: Color::rgb8(0xc3, 0xc7, 0xcc),
    timeline_grid: Color::rgba8(0x00, 0x00, 0x00, 0x14),
    timeline_text: Color::rgb8(0x12, 0x14, 0x17),
    timeline_muted: Color::rgb8(0x4d, 0x52, 0x59),
};

fn palette_color(dark: Color, light: Color) -> Color {
    dark.mix(light, theme_mix())
}

macro_rules! surface_accessors {
    ($($name:ident),+ $(,)?) => {
        $(
            #[inline]
            pub(crate) fn $name() -> Color {
                surface_color(palette_color(DARK.$name, LIGHT.$name))
            }
        )+
    };
}

macro_rules! palette_accessors {
    ($($name:ident),+ $(,)?) => {
        $(
            #[inline]
            pub(crate) fn $name() -> Color {
                palette_color(DARK.$name, LIGHT.$name)
            }
        )+
    };
}

surface_accessors!(
    bg,
    panel,
    tab_bar,
    tab_active,
    control,
    focused,
    line,
    line_soft,
    timeline_bg,
    timeline_bg_alt,
    timeline_header,
    timeline_header_active,
    timeline_line,
);
palette_accessors!(text, muted, timeline_grid, timeline_text, timeline_muted);

pub(crate) fn toggle_icon_color(active: bool) -> Color {
    if active {
        palette_color(DARK.timeline_bg, LIGHT.text)
    } else {
        text()
    }
}

pub(crate) fn floating_bg() -> Color {
    let base = panel();
    Color::rgba(base.r, base.g, base.b, 0.94)
}

pub(crate) fn popup_tint() -> Color {
    let base = panel();
    Color::rgba(base.r, base.g, base.b, 0.66)
}

pub(crate) fn popup_text() -> Color {
    text()
}

pub(crate) fn popup_muted() -> Color {
    muted()
}

pub(crate) fn popup_dim() -> Color {
    muted().mix(panel(), 0.38)
}

pub(crate) fn popup_hover() -> Color {
    interaction_overlay(0.08)
}

fn sync_kama_ui_theme() {
    let foreground = text();
    let popup = floating_bg();
    let tint = popup_tint();
    kama_ui::set_popup_accent(accent());
    kama_ui::set_theme_chrome(foreground, popup, tint, foreground);
    kama_ui::set_interaction_darken(theme_mix() >= 0.5);
}
