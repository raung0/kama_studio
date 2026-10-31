extern crate self as kama_ui;

pub use kama_ui_macros::{ui, ui_component};
pub use taffy;
pub mod components;
pub mod control_registry;
pub mod dock;
pub mod layout;
mod layout_api;
mod layout_engine;
mod text_measure;
use std::{
    cell::Cell,
    collections::HashMap,
    hash::{Hash, Hasher},
    rc::Rc,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
    time::Instant,
};

use anyhow::{Result, anyhow};
use cosmic_text::{
    Align as CosmicAlign, Attrs, Buffer, CacheKey, Family, FontSystem, Metrics, Shaping,
    SwashCache, SwashContent, Wrap,
};

use kama_ui_renderer::{AtlasEntry, AtlasFull, DrawCommand, GpuVertex, TextureKind};
pub use kama_ui_renderer::{
    ClipShape, Color, ExternalTextureId, IconId, Rect, Renderer, TextureId, TextureSource,
};

const MAX_CLIPS: usize = 4;
const ROOT_SCOPE_SEED: u64 = 0xcbf2_9ce4_8422_2325;
const TOOLTIP_HOVER_DELAY: std::time::Duration = std::time::Duration::from_secs(1);
const TOOLTIP_FONT_SIZE: f32 = 10.5;
const TOOLTIP_PADDING: f32 = 5.0;
const TOOLTIP_BORDER: f32 = 1.0;
static POPUP_ACCENT_RGBA: AtomicU32 = AtomicU32::new(u32::from_be_bytes([0xc1, 0x2c, 0xff, 0xff]));
static DEFAULT_TEXT_RGBA: AtomicU32 = AtomicU32::new(u32::from_be_bytes([0xe5, 0xe5, 0xe5, 0xff]));
static TOOLTIP_BG_RGBA: AtomicU32 = AtomicU32::new(u32::from_be_bytes([0x1f, 0x1f, 0x1f, 0xf0]));
static TOOLTIP_TINT_RGBA: AtomicU32 = AtomicU32::new(u32::from_be_bytes([0x1f, 0x1f, 0x1f, 0xa8]));
static TOOLTIP_TEXT_RGBA: AtomicU32 = AtomicU32::new(u32::from_be_bytes([0xe5, 0xe5, 0xe5, 0xff]));
static ROUNDED_CORNERS_ENABLED: AtomicBool = AtomicBool::new(true);
static DARKEN_INTERACTIONS: AtomicBool = AtomicBool::new(false);
static REVEAL_STRENGTH_BITS: AtomicU32 = AtomicU32::new(0.5f32.to_bits());
static REVEAL_ACCENT_MIX_BITS: AtomicU32 = AtomicU32::new(0.25f32.to_bits());

pub fn set_popup_accent(color: Color) {
    store_color(&POPUP_ACCENT_RGBA, color);
}

fn atomic_color(value: &AtomicU32) -> Color {
    let [r, g, b, a] = value.load(Ordering::Relaxed).to_be_bytes();
    Color::rgba8(r, g, b, a)
}

fn store_color(value: &AtomicU32, color: Color) {
    let rgba = [color.r, color.g, color.b, color.a].map(color_byte);
    value.store(u32::from_be_bytes(rgba), Ordering::Relaxed);
}

#[expect(
    clippy::cast_sign_loss,
    reason = "clamping makes the rounded channel nonnegative; NaN casts to zero"
)]
fn color_byte(channel: f32) -> u8 {
    (channel.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn popup_accent() -> Color {
    atomic_color(&POPUP_ACCENT_RGBA)
}

fn default_text_color() -> Color {
    atomic_color(&DEFAULT_TEXT_RGBA)
}

pub(crate) fn theme_popup_bg() -> Color {
    atomic_color(&TOOLTIP_BG_RGBA)
}

pub(crate) fn theme_popup_tint() -> Color {
    atomic_color(&TOOLTIP_TINT_RGBA)
}

fn tooltip_text() -> Color {
    atomic_color(&TOOLTIP_TEXT_RGBA)
}

#[derive(Clone, Copy, Debug)]
pub enum ColorKind {
    Fixed(Color),
    Contrast,
}

impl From<Color> for ColorKind {
    fn from(color: Color) -> Self {
        Self::Fixed(color)
    }
}

impl ColorKind {
    fn resolve(self, fill: Color) -> Color {
        match self {
            Self::Fixed(color) => color,
            Self::Contrast if fill.a > 0.01 => {
                let luminance = fill
                    .b
                    .mul_add(0.0722, fill.g.mul_add(0.7152, fill.r * 0.2126));
                if luminance >= 0.56 {
                    Color::BLACK
                } else {
                    Color::WHITE
                }
            }
            Self::Contrast => default_text_color(),
        }
    }
}

pub fn set_theme_chrome(
    foreground: Color,
    tooltip_bg_color: Color,
    tooltip_tint_color: Color,
    tooltip_text_color: Color,
) {
    store_color(&DEFAULT_TEXT_RGBA, foreground);
    store_color(&TOOLTIP_BG_RGBA, tooltip_bg_color);
    store_color(&TOOLTIP_TINT_RGBA, tooltip_tint_color);
    store_color(&TOOLTIP_TEXT_RGBA, tooltip_text_color);
}

pub fn set_rounded_corners_enabled(enabled: bool) {
    ROUNDED_CORNERS_ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn rounded_corners_enabled() -> bool {
    ROUNDED_CORNERS_ENABLED.load(Ordering::Relaxed)
}

pub fn set_interaction_darken(darken: bool) {
    DARKEN_INTERACTIONS.store(darken, Ordering::Relaxed);
}

fn interaction_darken() -> bool {
    DARKEN_INTERACTIONS.load(Ordering::Relaxed)
}

pub fn set_reveal_strength(value: f32) {
    let value = if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.5
    };
    REVEAL_STRENGTH_BITS.store(value.to_bits(), Ordering::Relaxed);
}

pub fn reveal_strength() -> f32 {
    f32::from_bits(REVEAL_STRENGTH_BITS.load(Ordering::Relaxed)).clamp(0.0, 1.0)
}

pub fn set_reveal_accent_mix(value: f32) {
    let value = if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.25
    };
    REVEAL_ACCENT_MIX_BITS.store(value.to_bits(), Ordering::Relaxed);
}

pub fn reveal_accent_mix() -> f32 {
    f32::from_bits(REVEAL_ACCENT_MIX_BITS.load(Ordering::Relaxed)).clamp(0.0, 1.0)
}

fn reveal_target() -> Color {
    let neutral = if interaction_darken() {
        Color::BLACK
    } else {
        Color::WHITE
    };
    neutral.mix(popup_accent(), reveal_accent_mix())
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Size {
    #[default]
    Fit,
    /// Use the remaining space with the same weight as `FillPortion(1.0)`.
    Fill,
    /// Use a weighted share of the remaining space along the parent flow axis.
    FillPortion(f32),
    /// Use the intrinsic size multiplied by this scale. Useful for animated collapse/reveal.
    FitScale(f32),
    Pixels(f32),
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Direction {
    Row,
    #[default]
    Column,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layer {
    #[default]
    Base,
    Overlay,
    Popup,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FontKind {
    #[default]
    Sans,
    Monospace,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorShape {
    #[default]
    Auto,
    Arrow,
    Pointer,
    EwResize,
    NsResize,
    ZoomIn,
    ZoomOut,
    Grab,
    Grabbing,
    Passthrough,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct BlockId(pub u64);

pub struct FormatKey<'a>(std::fmt::Arguments<'a>);

impl<'a> FormatKey<'a> {
    #[must_use]
    pub const fn new(arguments: std::fmt::Arguments<'a>) -> Self {
        Self(arguments)
    }
}

impl std::fmt::Display for FormatKey<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::write(formatter, self.0)
    }
}

impl Hash for FormatKey<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        struct HashWriter<'a, H>(&'a mut H);

        impl<H: Hasher> std::fmt::Write for HashWriter<'_, H> {
            fn write_str(&mut self, value: &str) -> std::fmt::Result {
                self.0.write(value.as_bytes());
                Ok(())
            }
        }

        let _ = std::fmt::write(&mut HashWriter(state), self.0);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScrollState {
    pub offset: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScrollRange {
    pub horizontal: f32,
    pub vertical: f32,
}

impl ScrollState {
    pub fn scroll_by(&mut self, delta: f32, max_offset: f32) -> bool {
        let offset = (self.offset + delta).clamp(0.0, max_offset.max(0.0));
        if (offset - self.offset).abs() < 0.001 {
            return false;
        }
        self.offset = offset;
        true
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PopupDirection {
    Down,
    Up,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PopupPlacement {
    pub rect: Rect,
    pub direction: PopupDirection,
}

#[must_use]
pub fn place_popup_with_direction(
    anchor: Rect,
    size: [f32; 2],
    viewport: Rect,
    prefer_above: bool,
    gap: f32,
) -> PopupPlacement {
    let margin = 6.0;
    let width = size[0].min(f32::mul_add(margin, -2.0, viewport.width).max(1.0));
    let desired_height = size[1].max(1.0);
    let below_y = anchor.bottom() + gap;
    let available_below = (viewport.bottom() - margin - below_y).max(0.0);
    let available_above = (anchor.y - gap - (viewport.y + margin)).max(0.0);

    let direction = if prefer_above {
        if available_above >= desired_height || available_above >= available_below {
            PopupDirection::Up
        } else {
            PopupDirection::Down
        }
    } else if available_below >= desired_height || available_below >= available_above {
        PopupDirection::Down
    } else {
        PopupDirection::Up
    };
    let available_height = match direction {
        PopupDirection::Down => available_below,
        PopupDirection::Up => available_above,
    };
    let height = desired_height.min(available_height.max(1.0));
    let y = match direction {
        PopupDirection::Down => below_y,
        PopupDirection::Up => anchor.y - gap - height,
    };
    let min_x = viewport.x + margin;
    let max_x = (viewport.right() - width - margin).max(min_x);

    PopupPlacement {
        rect: Rect::new(anchor.x.clamp(min_x, max_x), y, width, height),
        direction,
    }
}

#[must_use]
pub fn place_popup(
    anchor: Rect,
    size: [f32; 2],
    viewport: Rect,
    prefer_above: bool,
    gap: f32,
) -> Rect {
    place_popup_with_direction(anchor, size, viewport, prefer_above, gap).rect
}

pub struct ClickEvent {
    pub id: BlockId,
    pub position: [f32; 2],
}

#[derive(Clone, Copy, Debug, Default)]
pub struct InputState {
    pub cursor: [f32; 2],
    pub mouse_pressed: bool,
    pub mouse_released: bool,
}

/// Shared open state for a dismissible top-level popup.
///
/// Popup components own this state; the UI runtime keeps a clone for the
/// currently rendered popup so an outside press can dismiss it before the
/// application dispatches that press to underlying content.
#[derive(Clone, Debug, Default)]
pub struct PopupState(Rc<Cell<bool>>);

impl PopupState {
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.0.get()
    }

    pub fn set_open(&self, open: bool) {
        self.0.set(open);
    }

    pub fn close(&self) {
        self.set_open(false);
    }

    pub fn toggle(&self) {
        self.set_open(!self.is_open());
    }
}

/// Fill texture sizing modes.
///
/// `0` preserves existing stretch behavior. `TEXTURE_MODE_CONTAIN` preserves
/// texture aspect ratio and centers it inside block bounds.
pub const TEXTURE_MODE_CONTAIN: u32 = 3;

#[expect(
    clippy::struct_excessive_bools,
    reason = "text wrapping, clipping, centering, interaction, animation and reveal are independent block properties"
)]
pub struct Block {
    pub id: BlockId,
    pub layout: taffy::Style,
    pub fill: Option<Color>,
    pub fill_texture: Option<TextureSource>,
    pub texture_uv: [f32; 4],
    pub texture_mode: u32,
    pub texture_rotation: f32,
    pub border_width: f32,
    pub border_color: Color,
    pub border_texture: Option<TextureSource>,
    pub border_radius: f32,
    pub text: Option<String>,
    pub text_color: Color,
    foreground: Option<ColorKind>,
    pub font_size: f32,
    pub font_kind: FontKind,
    pub text_align: Align,
    pub text_vertical_align: Align,
    pub text_wrap: bool,
    pub children: Vec<Self>,
    pub on_click: Option<Box<dyn FnMut(ClickEvent)>>,
    pub on_mouse_enter: Option<Box<dyn FnMut()>>,
    pub horizontal_scroll: Option<ScrollState>,
    pub vertical_scroll: Option<ScrollState>,
    pub cursor: CursorShape,

    popup_dismiss: Option<PopupState>,
    custom_vertices: Option<Vec<[f32; 2]>>,
    interactive: bool,
    clip_children: bool,
    layer: Layer,
    centered: bool,
    position: Option<[f32; 2]>,
    legacy_absolute: bool,
    sizing: [Option<Size>; 2],
    collapse: [Option<f32>; 2],
    backdrop_blur: f32,
    backdrop_tint: Color,
    opacity: f32,
    animate_entry: bool,
    animate_interaction: bool,
    animate_fill: bool,
    tooltip: Option<String>,
    reveal: bool,
    reveal_border_only: bool,

    rect: Rect,
    content_rect: Rect,
    padding_rect: Rect,
    explicit_clips: Vec<ClipShape>,
    clips: Vec<ClipShape>,
    content_clips: Vec<ClipShape>,
    scroll_range: ScrollRange,
    scope_seed: u64,
    hover_t: f32,
    press_t: f32,
    appear_t: f32,
}

impl Block {
    fn new(id: BlockId, scope_seed: u64) -> Self {
        Self {
            id,
            layout: taffy::Style {
                position: taffy::Position::Relative,
                flex_direction: taffy::FlexDirection::Column,
                align_items: taffy::AlignItems::START,
                justify_content: taffy::JustifyContent::START,
                min_size: taffy::Size::zero(),
                flex_shrink: 0.0,
                overflow: taffy::Point {
                    x: taffy::Overflow::Hidden,
                    y: taffy::Overflow::Hidden,
                },
                ..taffy::Style::default()
            },
            fill: None,
            fill_texture: None,
            texture_uv: [0.0, 0.0, 1.0, 1.0],
            texture_mode: 0,
            texture_rotation: 0.0,
            border_width: 0.0,
            border_color: Color::WHITE,
            border_texture: None,
            border_radius: 0.0,
            text: None,
            text_color: default_text_color(),
            foreground: None,
            font_size: 16.0,
            font_kind: FontKind::Sans,
            text_align: Align::Start,
            text_vertical_align: Align::Center,
            text_wrap: true,
            children: Vec::new(),
            on_click: None,
            on_mouse_enter: None,
            horizontal_scroll: None,
            vertical_scroll: None,
            cursor: CursorShape::Auto,
            popup_dismiss: None,
            custom_vertices: None,
            interactive: false,
            clip_children: true,
            layer: Layer::Base,
            centered: false,
            position: None,
            legacy_absolute: false,
            sizing: [Some(Size::Fit); 2],
            collapse: [None; 2],
            backdrop_blur: 0.0,
            backdrop_tint: Color::rgba(0.03, 0.04, 0.055, 0.52),
            opacity: 1.0,
            animate_entry: false,
            animate_interaction: true,
            animate_fill: false,
            tooltip: None,
            reveal: false,
            reveal_border_only: false,
            rect: Rect::default(),
            content_rect: Rect::default(),
            padding_rect: Rect::default(),
            explicit_clips: Vec::new(),
            clips: Vec::new(),
            content_clips: Vec::new(),
            scroll_range: ScrollRange::default(),
            scope_seed,
            hover_t: 0.0,
            press_t: 0.0,
            appear_t: 1.0,
        }
    }
}

pub trait Component<Props> {
    fn ui<'ui>(&mut self, ctx: &'ui mut BuildCtx, props: Props) -> BlockBuilder<'ui>;
}

pub struct BuildCtx {
    blocks: Vec<Block>,
    scope_seed: u64,
    next_index: u32,
    clip_stack: Vec<ClipShape>,
}

#[derive(Clone, Debug, Default)]
pub struct LayoutRects {
    rects: HashMap<BlockId, Rect>,
    content_rects: HashMap<BlockId, Rect>,
    scroll_ranges: HashMap<BlockId, ScrollRange>,
}

impl LayoutRects {
    #[must_use]
    pub fn rect(&self, id: BlockId) -> Option<Rect> {
        self.rects.get(&id).copied()
    }

    #[must_use]
    pub fn content_rect(&self, id: BlockId) -> Option<Rect> {
        self.content_rects.get(&id).copied()
    }

    #[must_use]
    pub fn scroll_range(&self, id: BlockId) -> Option<ScrollRange> {
        self.scroll_ranges.get(&id).copied()
    }
}

pub fn measure_layout<R>(
    viewport: Rect,
    build: impl FnOnce(&mut BuildCtx) -> R,
) -> (R, LayoutRects) {
    fn collect(
        blocks: &[Block],
        rects: &mut HashMap<BlockId, Rect>,
        content_rects: &mut HashMap<BlockId, Rect>,
        scroll_ranges: &mut HashMap<BlockId, ScrollRange>,
    ) {
        for block in blocks {
            rects.insert(block.id, block.rect);
            content_rects.insert(block.id, block.content_rect);
            if block.horizontal_scroll.is_some() || block.vertical_scroll.is_some() {
                scroll_ranges.insert(block.id, block.scroll_range);
            }
            collect(&block.children, rects, content_rects, scroll_ranges);
        }
    }

    let mut ctx = BuildCtx::with_seed(ROOT_SCOPE_SEED);
    let result = build(&mut ctx);
    let mut blocks = ctx.blocks;
    layout_roots(&mut blocks, viewport);

    let mut rects = HashMap::new();
    let mut content_rects = HashMap::new();
    let mut scroll_ranges = HashMap::new();
    collect(&blocks, &mut rects, &mut content_rects, &mut scroll_ranges);
    (
        result,
        LayoutRects {
            rects,
            content_rects,
            scroll_ranges,
        },
    )
}

impl BuildCtx {
    const fn with_seed(scope_seed: u64) -> Self {
        Self {
            blocks: Vec::new(),
            scope_seed,
            next_index: 0,
            clip_stack: Vec::new(),
        }
    }

    #[allow(clippy::new_ret_no_self)]
    #[expect(
        clippy::expect_used,
        reason = "block ID exhaustion must fail rather than reuse an ID; the last block exists because it was just pushed"
    )]
    /// Creates a block in this scope.
    ///
    /// # Panics
    /// Panics if more than `u32::MAX` blocks are created in this scope.
    pub fn new(&mut self) -> BlockBuilder<'_> {
        let auto_id = hash_pair(self.scope_seed, u64::from(self.next_index));
        self.next_index = self
            .next_index
            .checked_add(1)
            .expect("block ID space exhausted");
        let mut block = Block::new(BlockId(auto_id), auto_id);
        block.explicit_clips.clone_from(&self.clip_stack);
        self.blocks.push(block);
        BlockBuilder {
            block: self.blocks.last_mut().expect("just pushed a block"),
        }
    }

    pub fn with_clip<R>(&mut self, rect: Rect, build: impl FnOnce(&mut Self) -> R) -> R {
        self.clip_stack.push(ClipShape { rect, radius: 0.0 });
        let result = build(self);
        self.clip_stack.pop();
        result
    }

    pub fn rect<K: Hash>(&mut self, key: K, rect: Rect) -> BlockBuilder<'_> {
        self.new()
            .id(key)
            .bounds((rect.x, rect.y, rect.width, rect.height))
    }
}

pub struct BlockBuilder<'a> {
    block: &'a mut Block,
}

macro_rules! builder_setters {
    ($($name:ident($arg:ident: $ty:ty) => $field:ident = $value:expr;)*) => {
        $(
            #[must_use]
            pub fn $name(self, $arg: $ty) -> Self {
                self.block.$field = $value;
                self
            }
        )*
    };
}

macro_rules! const_builder_setters {
    ($($name:ident($arg:ident: $ty:ty) => $field:ident = $value:expr;)*) => {
        $(
            #[must_use]
            pub const fn $name(self, $arg: $ty) -> Self {
                self.block.$field = $value;
                self
            }
        )*
    };
}

macro_rules! builder_flags {
    ($($name:ident, $conditional:ident => $($field:ident = $value:expr),+;)*) => {
        $(
            #[must_use]
            pub const fn $name(self) -> Self {
                let block = &mut *self.block;
                $(block.$field = $value;)+
                self
            }
            #[must_use]
            pub const fn $conditional(self, enabled: bool) -> Self {
                if enabled { self.$name() } else { self }
            }
        )*
    };
}

impl BlockBuilder<'_> {
    #[must_use]
    pub fn id<K: Hash>(self, key: K) -> Self {
        let block = &mut *self.block;
        let id = hash_value(block.scope_seed, &key);
        block.id = BlockId(id);
        block.scope_seed = id;
        self
    }

    const_builder_setters! {
        fill(color: Color) => fill = Some(color);
        texture_uv(uv: [f32; 4]) => texture_uv = uv;
        texture_mode(mode: u32) => texture_mode = mode;
        texture_rotation(radians: f32) => texture_rotation = radians;
        border_color(color: Color) => border_color = color;
        border_radius(value: f32) => border_radius = value.max(0.0);
        text_color(color: Color) => text_color = color;
        font_size(pixels: f32) => font_size = pixels.max(1.0);
        font_kind(value: FontKind) => font_kind = value;
        text_align(align: Align) => text_align = align;
        text_vertical_align(align: Align) => text_vertical_align = align;
        opacity(value: f32) => opacity = value.clamp(0.0, 1.0);
        cursor(shape: CursorShape) => cursor = shape;
    }

    builder_setters! {
        fill_texture(texture: impl Into<TextureSource>) => fill_texture = Some(texture.into());
        border_texture(texture: impl Into<TextureSource>) => border_texture = Some(texture.into());
        text(text: impl Into<String>) => text = Some(text.into());
        foreground(kind: impl Into<ColorKind>) => foreground = Some(kind.into());
    }

    #[must_use]
    pub fn fill_texture_opt<T: Into<TextureSource>>(self, texture: Option<T>) -> Self {
        self.block.fill_texture = texture.map(Into::into);
        self
    }

    #[must_use]
    pub fn tooltip(self, text: impl Into<String>) -> Self {
        self.block.tooltip = Some(text.into());
        self.block.interactive = true;
        self
    }

    builder_flags! {
        monospace, monospace_if => font_kind = FontKind::Monospace;
        reveal, reveal_if => reveal = true;
        border_reveal, border_reveal_if => reveal = true, reveal_border_only = true;
        interactive, interactive_if => interactive = true, reveal = true, foreground = Some(ColorKind::Contrast);
        interactive_no_reveal, interactive_no_reveal_if => interactive = true, foreground = Some(ColorKind::Contrast);
        text_centered, text_centered_if => text_align = Align::Center, text_vertical_align = Align::Center;
        animate_fill, animate_fill_if => animate_fill = true;
        no_wrap, no_wrap_if => text_wrap = false;
    }

    #[must_use]
    pub fn vertices(self, vertices: impl IntoIterator<Item = [f32; 2]>) -> Self {
        let block = &mut *self.block;
        block.custom_vertices = Some(vertices.into_iter().collect());
        self
    }

    const_builder_setters! {
        backdrop_tint(color: Color) => backdrop_tint = color;
        animate_entry(enabled: bool) => animate_entry = enabled;
        animate_interaction(enabled: bool) => animate_interaction = enabled;
    }

    builder_flags! {
        overlay, overlay_if => layer = Layer::Overlay, legacy_absolute = true;
        top_overlay, top_overlay_if => layer = Layer::Popup, legacy_absolute = true;
        centered, centered_if => centered = true;
    }

    #[must_use]
    pub fn position(self, (x, y): (f32, f32)) -> Self {
        self.block.position = Some((x, y).into());
        self.block.legacy_absolute = true;
        self
    }

    #[must_use]
    pub const fn bounds(self, (x, y, width, height): (f32, f32, f32, f32)) -> Self {
        let block = &mut *self.block;
        block.position = Some([x, y]);
        block.legacy_absolute = true;
        block.sizing = [Some(Size::Pixels(width)), Some(Size::Pixels(height))];
        block.collapse = [None; 2];
        block.layout.size = taffy::Size {
            width: taffy::Dimension::length(width.max(0.0)),
            height: taffy::Dimension::length(height.max(0.0)),
        };
        self
    }

    #[must_use]
    pub const fn backdrop_blur(self, radius: f32) -> Self {
        self.block.backdrop_blur = radius.max(0.0);
        self
    }

    #[must_use]
    pub const fn popup(self) -> Self {
        let block = &mut *self.block;
        block.layer = Layer::Popup;
        block.centered = true;
        block.legacy_absolute = true;
        block.animate_entry = true;
        self
    }

    /// Marks this top-level popup surface as owning outside-press dismissal.
    #[must_use]
    pub fn dismissible_popup(self, state: PopupState) -> Self {
        let block = &mut *self.block;
        block.layer = Layer::Popup;
        block.popup_dismiss = Some(state);
        block.legacy_absolute = true;
        self
    }

    #[must_use]
    pub fn on_mouse_enter(self, callback: impl FnMut() + 'static) -> Self {
        self.block.on_mouse_enter = Some(Box::new(callback));
        self.block.interactive = true;
        self
    }

    #[must_use]
    pub fn on_click(self, callback: impl FnMut(ClickEvent) + 'static) -> Self {
        let block = &mut *self.block;
        block.on_click = Some(Box::new(callback));
        block.interactive = true;
        block.foreground = Some(ColorKind::Contrast);
        block.reveal = true;
        block.animate_interaction = true;
        self
    }

    #[must_use]
    pub fn children<R>(self, f: impl FnOnce(&mut BuildCtx) -> R) -> Self {
        let seed = self.block.scope_seed;
        let mut ctx = BuildCtx::with_seed(seed);
        f(&mut ctx);
        self.block.children = ctx.blocks;
        self
    }

    #[must_use]
    pub const fn build(self) -> BlockId {
        self.block.id
    }
}

#[derive(Clone, Copy, Debug)]
struct AnimationState {
    hover: f32,
    press: f32,
    appear: f32,
    fill: Option<Color>,
    fill_target: Option<Color>,
    last_seen: u64,
}

impl Default for AnimationState {
    fn default() -> Self {
        Self {
            hover: 0.0,
            press: 0.0,
            appear: 1.0,
            fill: None,
            fill_target: None,
            last_seen: 0,
        }
    }
}

#[derive(Debug, Default)]
struct TooltipState {
    text: Option<String>,
    anchor: Rect,
    opacity: f32,
    hovered: Option<BlockId>,
    hover_started: Option<Instant>,
    ready: bool,
}

impl TooltipState {
    fn update(&mut self, target: Option<(BlockId, String, Rect)>, now: Instant, dt: f32) {
        if let Some((id, text, anchor)) = target {
            if self.hovered != Some(id) {
                self.hovered = Some(id);
                self.hover_started = Some(now);
                self.ready = false;
                self.opacity = 0.0;
            }
            self.text = Some(text);
            self.anchor = anchor;
            self.ready = self.hover_started.is_some_and(|started| {
                now.saturating_duration_since(started) >= TOOLTIP_HOVER_DELAY
            });
        } else {
            self.hovered = None;
            self.hover_started = None;
            self.ready = false;
        }

        self.opacity = approach(
            self.opacity,
            if self.ready { 1.0 } else { 0.0 },
            if self.ready { 18.0 } else { 14.0 },
            dt,
        );
        if self.hovered.is_none() && self.opacity < 0.001 {
            self.opacity = 0.0;
            self.text = None;
        }
    }

    fn is_animating(&self) -> bool {
        if self.ready {
            self.opacity < 0.999
        } else {
            self.hovered.is_some() || self.opacity > 0.001
        }
    }
}

pub struct Gui {
    hot: Option<BlockId>,
    active: Option<BlockId>,
    cursor_shape: CursorShape,
    font_system: FontSystem,
    swash_cache: SwashCache,
    glyphs: HashMap<CacheKey, AtlasEntry>,
    animations: HashMap<BlockId, AnimationState>,
    last_frame: Instant,
    frame_index: u64,
    tooltip: TooltipState,
    popup_capture: Option<(Rect, PopupState)>,
}

impl Default for Gui {
    fn default() -> Self {
        Self::new()
    }
}

impl Gui {
    #[must_use]
    pub fn new() -> Self {
        Self {
            hot: None,
            active: None,
            cursor_shape: CursorShape::Arrow,
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
            glyphs: HashMap::new(),
            animations: HashMap::new(),
            last_frame: Instant::now(),
            frame_index: 0,
            tooltip: TooltipState::default(),
            popup_capture: None,
        }
    }

    pub const fn begin<'a>(&'a mut self, renderer: &'a mut Renderer, input: InputState) -> Ui<'a> {
        Ui {
            gui: self,
            renderer,
            input,
            root: BuildCtx::with_seed(ROOT_SCOPE_SEED),
        }
    }

    pub const fn cursor_shape(&self) -> CursorShape {
        self.cursor_shape
    }

    /// Dismisses the currently rendered top-level popup when `point` is
    /// outside its surface. Returns true when the press must be consumed.
    pub fn consume_popup_press(&mut self, point: [f32; 2]) -> bool {
        let Some((rect, state)) = self.popup_capture.as_ref() else {
            return false;
        };
        if !state.is_open() {
            self.popup_capture = None;
            return false;
        }
        if rect.contains(point) {
            return false;
        }
        state.close();
        self.popup_capture = None;
        true
    }

    pub fn measure_text_ink_width(&mut self, text: &str, font_size: f32, scale: f32) -> f32 {
        measure_text_ink_width(
            &mut self.font_system,
            &mut self.swash_cache,
            text,
            font_size,
            scale,
        )
    }

    pub fn font_families(&self) -> Vec<String> {
        let mut families = self
            .font_system
            .db()
            .faces()
            .filter_map(|face| face.families.first().map(|(family, _)| family.clone()))
            .filter(|family| !family.trim().is_empty())
            .collect::<Vec<_>>();
        families.sort_by_key(|family| family.to_lowercase());
        families.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        families
    }

    pub fn has_active_animations(&self) -> bool {
        const EPSILON: f32 = 0.001;

        self.tooltip.is_animating()
            || self.animations.values().any(|animation| {
                animation.last_seen == self.frame_index
                    && ((animation.hover > EPSILON && animation.hover < 1.0 - EPSILON)
                        || (animation.press > EPSILON && animation.press < 1.0 - EPSILON)
                        || (animation.appear - 1.0).abs() > EPSILON
                        || !colors_close(animation.fill, animation.fill_target, EPSILON))
            })
    }
}

fn popup_capture(blocks: &[Block]) -> Option<(Rect, PopupState)> {
    let mut capture = None;
    for block in blocks {
        if block.layout.display == taffy::Display::None {
            continue;
        }
        if let Some(state) = block.popup_dismiss.as_ref() {
            capture = Some((block.rect, state.clone()));
        }
        if let Some(child) = popup_capture(&block.children) {
            capture = Some(child);
        }
    }
    capture
}

pub struct Ui<'a> {
    gui: &'a mut Gui,
    renderer: &'a mut Renderer,
    input: InputState,
    root: BuildCtx,
}

impl std::ops::Deref for Ui<'_> {
    type Target = BuildCtx;

    fn deref(&self) -> &Self::Target {
        &self.root
    }
}

impl std::ops::DerefMut for Ui<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.root
    }
}

impl Ui<'_> {
    #[allow(clippy::new_ret_no_self)]
    pub fn new(&mut self) -> BlockBuilder<'_> {
        self.root.new()
    }

    /// Submits the built UI to the renderer.
    ///
    /// # Errors
    /// Returns an error if preparing or submitting renderer resources fails.
    pub fn finish(mut self) -> Result<()> {
        let viewport = Rect::new(
            0.0,
            0.0,
            self.renderer.logical_width(),
            self.renderer.logical_height(),
        );

        let mut roots = std::mem::take(&mut self.root.blocks);
        layout_roots(&mut roots, viewport);
        self.gui.popup_capture = popup_capture(&roots);
        self.handle_interaction(&mut roots);
        self.update_animations(&mut roots);
        let tooltip_size = if self.gui.tooltip.opacity > 0.0 {
            self.gui
                .tooltip
                .text
                .clone()
                .map(|text| measure_text(&mut self.gui.font_system, &text, TOOLTIP_FONT_SIZE))
        } else {
            None
        };
        if let Some(size) = tooltip_size {
            append_tooltip(&mut roots, viewport, &self.gui.tooltip, size);
        }

        let mut retried_glyph_atlas = false;
        let (base_commands, overlay_commands, vertices) = loop {
            let mut base_commands = Vec::new();
            let mut overlay_commands = Vec::new();
            let mut popup_commands = Vec::new();
            let mut vertices = Vec::new();
            let mut error = None;
            {
                let mut emit = EmitContext {
                    gui: &mut *self.gui,
                    renderer: &mut *self.renderer,
                    base_commands: &mut base_commands,
                    overlay_commands: &mut overlay_commands,
                    popup_commands: &mut popup_commands,
                    vertices: &mut vertices,
                };
                for block in &roots {
                    if let Err(err) =
                        emit_block(block, &mut emit, 1.0, Layer::Base, Color::TRANSPARENT, None)
                    {
                        error = Some(err);
                        break;
                    }
                }
            }

            match error {
                None => {
                    overlay_commands.extend(popup_commands);
                    break (base_commands, overlay_commands, vertices);
                }
                Some(err) if !retried_glyph_atlas && err.downcast_ref::<AtlasFull>().is_some() => {
                    self.gui.glyphs.clear();
                    self.renderer.reset_glyph_atlas();
                    retried_glyph_atlas = true;
                }
                Some(err) => return Err(err),
            }
        };
        self.renderer.render(
            &base_commands,
            &overlay_commands,
            &vertices,
            self.input.cursor,
        )
    }

    fn handle_interaction(&mut self, roots: &mut [Block]) {
        self.gui.cursor_shape = cursor_at(roots, self.input.cursor);
        let hot = hit_test(roots, self.input.cursor);
        if hot != self.gui.hot {
            if let Some(callback) = hot
                .and_then(|id| find_block_mut(roots, id))
                .and_then(|block| block.on_mouse_enter.as_mut())
            {
                callback();
            }
            self.gui.hot = hot;
        }
        if self.input.mouse_pressed {
            self.gui.active = hot;
            if let Some((id, callback)) = hot.and_then(|id| {
                find_block_mut(roots, id)
                    .and_then(|block| block.on_click.as_mut())
                    .map(|callback| (id, callback))
            }) {
                callback(ClickEvent {
                    id,
                    position: self.input.cursor,
                });
            }
        }
        if self.input.mouse_released {
            self.gui.active = None;
        }
    }

    fn update_animations(&mut self, roots: &mut [Block]) {
        self.gui.frame_index = self.gui.frame_index.wrapping_add(1);
        let now = Instant::now();
        let dt = now
            .duration_since(self.gui.last_frame)
            .as_secs_f32()
            .min(0.05);
        self.gui.last_frame = now;
        let frame = self.gui.frame_index;
        let hot = self.gui.hot;
        let active = self.gui.active;

        let tooltip = hot
            .and_then(|id| find_block(roots, id).map(|block| (id, block)))
            .and_then(|(id, block)| block.tooltip.clone().map(|text| (id, text, block.rect)));
        self.gui.tooltip.update(tooltip, now, dt);
        update_animation_tree(roots, &mut self.gui.animations, frame, dt, hot, active);
        self.gui
            .animations
            .retain(|_, animation| frame.saturating_sub(animation.last_seen) < 240);
    }
}

#[expect(
    clippy::arithmetic_side_effects,
    reason = "glyph x and placement left are i32 values whose widened i64 sum cannot overflow"
)]
fn measure_text_ink_width(
    font_system: &mut FontSystem,
    swash_cache: &mut SwashCache,
    text: &str,
    font_size: f32,
    scale: f32,
) -> f32 {
    let scale = scale.max(0.001);
    let line_height = font_size * text_measure::LINE_HEIGHT;
    let mut buffer = Buffer::new(font_system, Metrics::new(font_size, line_height));
    let glyphs = {
        let mut borrowed = buffer.borrow_with(font_system);
        borrowed.set_size(None, None);
        borrowed.set_wrap(Wrap::None);
        borrowed.set_text(
            text,
            &Attrs::new(),
            Shaping::Advanced,
            Some(CosmicAlign::Left),
        );
        let mut glyphs = Vec::new();
        for run in borrowed.layout_runs() {
            for glyph in run.glyphs {
                let physical = glyph.physical((0.0, run.line_y * scale), scale);
                glyphs.push((physical.cache_key, physical.x));
            }
        }
        glyphs
    };

    glyphs
        .into_iter()
        .filter_map(|(cache_key, x)| {
            swash_cache
                .get_image(font_system, cache_key)
                .as_ref()
                .map(|image| {
                    (i64::from(x) + i64::from(image.placement.left)) as f32
                        + image.placement.width as f32
                })
        })
        .fold(0.0_f32, f32::max)
        .max(0.0)
        / scale
}

fn measure_text(font_system: &mut FontSystem, text: &str, font_size: f32) -> [f32; 2] {
    let line_height = font_size * text_measure::LINE_HEIGHT;
    let mut buffer = Buffer::new(font_system, Metrics::new(font_size, line_height));
    let mut borrowed = buffer.borrow_with(font_system);
    borrowed.set_size(None, None);
    borrowed.set_wrap(Wrap::None);
    borrowed.set_text(
        text,
        &Attrs::new(),
        Shaping::Advanced,
        Some(CosmicAlign::Left),
    );
    borrowed
        .layout_runs()
        .fold([0.0_f32, line_height], |[width, height], run| {
            [
                width.max(run.line_w),
                height.max(run.line_top + run.line_height),
            ]
        })
}

fn append_tooltip(
    roots: &mut Vec<Block>,
    viewport: Rect,
    state: &TooltipState,
    text_size: [f32; 2],
) {
    let Some(text) = state.text.as_deref() else {
        return;
    };
    let inset = (TOOLTIP_PADDING + TOOLTIP_BORDER) * 2.0;
    let width = text_size[0] + inset;
    let height = text_size[1] + inset;
    let x = (state.anchor.width - width)
        .mul_add(0.5, state.anchor.x)
        .clamp(6.0, (viewport.width - width - 6.0).max(6.0));
    let below = state.anchor.bottom() + 8.0;
    let y = if below + height <= viewport.bottom() - 6.0 {
        below
    } else {
        (state.anchor.y - height - 8.0).max(6.0)
    };

    let mut ctx = BuildCtx::with_seed(ROOT_SCOPE_SEED);
    let _ = ctx
        .new()
        .id("ui-tooltip")
        .overlay()
        .bounds((x, y, width, height))
        .backdrop_blur(18.0)
        .backdrop_tint(theme_popup_tint())
        .fill(theme_popup_bg())
        .border(1)
        .border_color(popup_accent())
        .border_radius(7.0)
        .padding(TOOLTIP_PADDING)
        .font_size(TOOLTIP_FONT_SIZE)
        .text_color(tooltip_text())
        .text_centered()
        .no_wrap()
        .text(text)
        .opacity(state.opacity)
        .build();
    layout_roots(&mut ctx.blocks, viewport);
    roots.extend(ctx.blocks);
}

fn find_block(blocks: &[Block], id: BlockId) -> Option<&Block> {
    for block in blocks {
        if block.layout.display == taffy::Display::None {
            continue;
        }
        if block.id == id {
            return Some(block);
        }
        if let Some(block) = find_block(&block.children, id) {
            return Some(block);
        }
    }
    None
}

#[expect(
    clippy::expect_used,
    reason = "layout nodes are created and used within the same tree without removal"
)]
fn layout_roots(blocks: &mut [Block], viewport: Rect) {
    thread_local! {
        static TEXT: std::cell::RefCell<text_measure::TextMeasurer> =
            std::cell::RefCell::new(text_measure::TextMeasurer::new());
    }
    TEXT.with_borrow_mut(|text| {
        layout_engine::layout(blocks, viewport, text).expect("valid layout tree");
    });
}

fn prepare_block(
    block: &mut Block,
    rect: Rect,
    inherited_clips: &[ClipShape],
    inherited_layer: Layer,
    explicit_clip_origin: [f32; 2],
    viewport: Rect,
) {
    block.rect = rect;
    if !rounded_corners_enabled() {
        let min_side = rect.width.min(rect.height).max(0.0);
        let true_circle = (rect.width - rect.height).abs() <= 1.0
            && min_side > 0.0
            && block.border_radius >= min_side * 0.45;
        if !true_circle {
            block.border_radius = 0.0;
        }
    }
    block.clips = inherited_clips.to_vec();
    if !block.legacy_absolute || block.layer <= inherited_layer {
        for mut clip in block.explicit_clips.iter().copied() {
            clip.rect.x += explicit_clip_origin[0];
            clip.rect.y += explicit_clip_origin[1];
            push_clip(&mut block.clips, clip);
        }
    }
    block.layer = block.layer.max(inherited_layer);
    block.content_clips = block.clips.clone();
    let clip_x = block.clip_children || block.layout.overflow.x != taffy::Overflow::Visible;
    let clip_y = block.clip_children || block.layout.overflow.y != taffy::Overflow::Visible;
    if clip_x || clip_y {
        push_clip(
            &mut block.content_clips,
            ClipShape {
                rect: Rect::new(
                    if clip_x {
                        block.padding_rect.x
                    } else {
                        viewport.x
                    },
                    if clip_y {
                        block.padding_rect.y
                    } else {
                        viewport.y
                    },
                    if clip_x {
                        block.padding_rect.width
                    } else {
                        viewport.width
                    },
                    if clip_y {
                        block.padding_rect.height
                    } else {
                        viewport.height
                    },
                ),
                radius: if clip_x && clip_y {
                    (block.border_radius - block.border_width).max(0.0)
                } else {
                    0.0
                },
            },
        );
    }
}

const fn is_flow(block: &Block) -> bool {
    !block.legacy_absolute
        && !matches!(
            block.layout.position,
            taffy::Position::Absolute | taffy::Position::Fixed
        )
}

fn push_clip(clips: &mut Vec<ClipShape>, clip: ClipShape) {
    if clip.radius <= 0.001
        && let Some(flat) = clips.iter_mut().find(|existing| existing.radius <= 0.001)
    {
        flat.rect = flat.rect.intersect(clip.rect);
        return;
    }

    if clips.iter().any(|existing| {
        (existing.rect.x - clip.rect.x).abs() < 0.001
            && (existing.rect.y - clip.rect.y).abs() < 0.001
            && (existing.rect.width - clip.rect.width).abs() < 0.001
            && (existing.rect.height - clip.rect.height).abs() < 0.001
            && (existing.radius - clip.radius).abs() < 0.001
    }) {
        return;
    }

    if clips.len() == MAX_CLIPS {
        let remove_index = clips
            .iter()
            .position(|existing| existing.radius > 0.001)
            .unwrap_or(0);
        clips.remove(remove_index);
    }
    clips.push(clip);
}

fn cursor_at(blocks: &[Block], position: [f32; 2]) -> CursorShape {
    cursor_at_layer(blocks, position, Layer::Popup, Layer::Base)
        .or_else(|| cursor_at_layer(blocks, position, Layer::Overlay, Layer::Base))
        .or_else(|| cursor_at_layer(blocks, position, Layer::Base, Layer::Base))
        .unwrap_or(CursorShape::Arrow)
}

fn cursor_at_layer(
    blocks: &[Block],
    position: [f32; 2],
    desired: Layer,
    inherited: Layer,
) -> Option<CursorShape> {
    for block in blocks.iter().rev() {
        if block.layout.display == taffy::Display::None {
            continue;
        }
        let effective = inherited.max(block.layer);
        if let Some(shape) = cursor_at_layer(&block.children, position, desired, effective) {
            return Some(shape);
        }
        if effective != desired
            || !block
                .clips
                .iter()
                .all(|clip| rounded_rect_contains(clip.rect, clip.radius, position))
            || !block_contains(block, position)
        {
            continue;
        }
        let shape = match block.cursor {
            CursorShape::Auto if block.interactive => CursorShape::Pointer,
            CursorShape::Auto | CursorShape::Passthrough => continue,
            shape => shape,
        };
        return Some(shape);
    }
    None
}

fn hit_test(blocks: &[Block], position: [f32; 2]) -> Option<BlockId> {
    hit_test_layer(blocks, position, Layer::Popup, Layer::Base)
        .or_else(|| hit_test_layer(blocks, position, Layer::Overlay, Layer::Base))
        .or_else(|| hit_test_layer(blocks, position, Layer::Base, Layer::Base))
}

fn hit_test_layer(
    blocks: &[Block],
    position: [f32; 2],
    desired: Layer,
    inherited: Layer,
) -> Option<BlockId> {
    for block in blocks.iter().rev() {
        if block.layout.display == taffy::Display::None {
            continue;
        }
        let effective = inherited.max(block.layer);
        if let Some(id) = hit_test_layer(&block.children, position, desired, effective) {
            return Some(id);
        }
        if effective != desired
            || !block
                .clips
                .iter()
                .all(|clip| rounded_rect_contains(clip.rect, clip.radius, position))
        {
            continue;
        }
        if block.interactive && block_contains(block, position) {
            return Some(block.id);
        }
    }
    None
}

fn block_contains(block: &Block, point: [f32; 2]) -> bool {
    let Some(vertices) = block.custom_vertices.as_deref() else {
        return rounded_rect_contains(block.rect, block.border_radius, point);
    };
    let local = [point[0] - block.rect.x, point[1] - block.rect.y];
    vertices
        .as_chunks::<3>()
        .0
        .iter()
        .any(|triangle| point_in_triangle_2d(local, triangle[0], triangle[1], triangle[2]))
}

fn point_in_triangle_2d(point: [f32; 2], a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> bool {
    fn edge(a: [f32; 2], b: [f32; 2], point: [f32; 2]) -> f32 {
        (point[1] - a[1]).mul_add(-(b[0] - a[0]), (point[0] - a[0]) * (b[1] - a[1]))
    }
    let e0 = edge(a, b, point);
    let e1 = edge(b, c, point);
    let e2 = edge(c, a, point);
    let has_negative = e0 < 0.0 || e1 < 0.0 || e2 < 0.0;
    let has_positive = e0 > 0.0 || e1 > 0.0 || e2 > 0.0;
    !(has_negative && has_positive)
}

fn rounded_rect_contains(rect: Rect, radius: f32, point: [f32; 2]) -> bool {
    if !rect.contains(point) {
        return false;
    }
    let radius = radius.max(0.0).min(rect.width.min(rect.height) * 0.5);
    if radius <= 0.0 {
        return true;
    }
    let center_x = rect.width.mul_add(0.5, rect.x);
    let center_y = rect.height.mul_add(0.5, rect.y);
    let inner_x = rect.width.mul_add(0.5, -radius).max(0.0);
    let inner_y = rect.height.mul_add(0.5, -radius).max(0.0);
    let nearest_x = point[0].clamp(center_x - inner_x, center_x + inner_x);
    let nearest_y = point[1].clamp(center_y - inner_y, center_y + inner_y);
    let dx = point[0] - nearest_x;
    let dy = point[1] - nearest_y;
    dy.mul_add(dy, dx * dx) <= radius * radius
}

fn find_block_mut(blocks: &mut [Block], id: BlockId) -> Option<&mut Block> {
    for block in blocks {
        if block.layout.display == taffy::Display::None {
            continue;
        }
        if block.id == id {
            return Some(block);
        }
        if let Some(block) = find_block_mut(&mut block.children, id) {
            return Some(block);
        }
    }
    None
}

fn approach(current: f32, target: f32, speed: f32, dt: f32) -> f32 {
    let amount = 1.0 - (-speed * dt).exp();
    (target - current).mul_add(amount, current)
}

fn colors_close(a: Option<Color>, b: Option<Color>, epsilon: f32) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => {
            (a.r - b.r).abs() < epsilon
                && (a.g - b.g).abs() < epsilon
                && (a.b - b.b).abs() < epsilon
                && (a.a - b.a).abs() < epsilon
        }
        (None, None) => true,
        _ => false,
    }
}

fn update_animation_tree(
    blocks: &mut [Block],
    animations: &mut HashMap<BlockId, AnimationState>,
    frame: u64,
    dt: f32,
    hot: Option<BlockId>,
    active: Option<BlockId>,
) {
    for block in blocks {
        let hover_target = f32::from(block.animate_interaction && hot == Some(block.id));
        let press_target = f32::from(block.animate_interaction && active == Some(block.id));
        let tracked = block.animate_entry
            || block.animate_fill
            || hover_target > 0.0
            || press_target > 0.0
            || animations.contains_key(&block.id);
        if tracked {
            let settled = {
                let animation = animations.entry(block.id).or_default();
                let was_absent =
                    animation.last_seen == 0 || animation.last_seen.saturating_add(1) < frame;
                if was_absent && block.animate_entry {
                    animation.appear = 0.0;
                }
                animation.last_seen = frame;
                animation.hover = approach(animation.hover, hover_target, 18.0, dt);
                animation.press = approach(animation.press, press_target, 28.0, dt);
                animation.appear = if block.animate_entry {
                    approach(animation.appear, 1.0, 14.0, dt)
                } else {
                    1.0
                };
                if block.animate_fill {
                    let target = block.fill;
                    if was_absent {
                        animation.fill = target;
                    }
                    animation.fill_target = target;
                    animation.fill = match (animation.fill, target) {
                        (Some(current), Some(target)) => {
                            Some(current.mix(target, 1.0 - (-18.0 * dt).exp()))
                        }
                        (_, target) => target,
                    };
                    block.fill = animation.fill;
                }
                block.hover_t = animation.hover;
                block.press_t = animation.press;
                block.appear_t = animation.appear;
                !block.animate_entry
                    && !block.animate_fill
                    && hover_target == 0.0
                    && press_target == 0.0
                    && animation.hover.abs() < 0.001
                    && animation.press.abs() < 0.001
            };
            if settled {
                animations.remove(&block.id);
            }
        } else {
            block.hover_t = 0.0;
            block.press_t = 0.0;
            block.appear_t = 1.0;
        }
        update_animation_tree(&mut block.children, animations, frame, dt, hot, active);
    }
}

struct EmitContext<'a> {
    gui: &'a mut Gui,
    renderer: &'a mut Renderer,
    base_commands: &'a mut Vec<DrawCommand>,
    overlay_commands: &'a mut Vec<DrawCommand>,
    popup_commands: &'a mut Vec<DrawCommand>,
    vertices: &'a mut Vec<GpuVertex>,
}

fn emit_block(
    block: &Block,
    emit: &mut EmitContext<'_>,
    inherited_opacity: f32,
    inherited_layer: Layer,
    inherited_background: Color,
    inherited_foreground: Option<ColorKind>,
) -> Result<()> {
    if block.layout.display == taffy::Display::None {
        return Ok(());
    }
    let scale = emit.renderer.scale_factor();
    let effective_layer = inherited_layer.max(block.layer);
    let commands = match effective_layer {
        Layer::Base => &mut *emit.base_commands,
        Layer::Overlay => &mut *emit.overlay_commands,
        Layer::Popup => &mut *emit.popup_commands,
    };

    let opacity = inherited_opacity * block.opacity * block.appear_t;
    let is_icon = matches!(block.fill_texture, Some(TextureSource::Icon(_)));
    let raw_fill = block.fill.unwrap_or_else(|| {
        if block.fill_texture.is_some() {
            Color::WHITE
        } else {
            Color::TRANSPARENT
        }
    });
    let background = if is_icon {
        inherited_background
    } else {
        block
            .fill
            .filter(|fill| fill.a > 0.0)
            .map_or(inherited_background, |fill| {
                composite_over(fill, inherited_background)
            })
    };
    let foreground = block.foreground.or(inherited_foreground);
    let fill_color = if is_icon {
        foreground.map_or(raw_fill, |kind| kind.resolve(background))
    } else {
        raw_fill
    };
    let interaction_target = if interaction_darken() {
        Color::rgba(0.0, 0.0, 0.0, fill_color.a)
    } else {
        Color::rgba(1.0, 1.0, 1.0, fill_color.a)
    };
    let interactive_fill = fill_color.mix(interaction_target, block.press_t * 0.08);
    let border_color = block.border_color;
    let (physical_clips, clip_count) = scaled_clips(&block.clips, scale);
    let physical_clips = clip_prefix(&physical_clips, clip_count);

    if effective_layer != Layer::Base && block.backdrop_blur > 0.0 {
        commands.push(DrawCommand::backdrop_blur(
            block.id.0 ^ 0xb10b_b10b_b10b_b10b,
            block.rect.scaled(scale),
            physical_clips,
            block.border_radius * scale,
            block.backdrop_tint,
            opacity,
        ));
    }

    if let Some(custom_vertices) = block.custom_vertices.as_deref() {
        if custom_vertices.len() >= 3 {
            let vertex_offset = emit.vertices.len() as u32;
            let mut geometry_hash = 0xcbf2_9ce4_8422_2325u64;
            for [x, y] in custom_vertices.iter().copied() {
                for bits in [x.to_bits(), y.to_bits()] {
                    geometry_hash ^= u64::from(bits);
                    geometry_hash = geometry_hash.wrapping_mul(0x0000_0100_0000_01b3);
                }
                emit.vertices.push(GpuVertex {
                    position: [(block.rect.x + x) * scale, (block.rect.y + y) * scale],
                });
            }
            geometry_hash ^= custom_vertices.len() as u64;
            geometry_hash = geometry_hash.wrapping_mul(0x0000_0100_0000_01b3);
            let fill = emit.renderer.resolve_texture(block.fill_texture);
            commands.push(DrawCommand::mesh(
                block.id.0,
                block.rect.scaled(scale),
                physical_clips,
                interactive_fill,
                fill,
                vertex_offset,
                custom_vertices.len() as u32,
                geometry_hash,
                opacity,
            ));
        }
    } else if block.fill.is_some()
        || block.fill_texture.is_some()
        || block.border_width > 0.0
        || block.border_texture.is_some()
        || block.reveal
    {
        let fill = emit.renderer.resolve_texture(block.fill_texture);
        let border = emit.renderer.resolve_texture(block.border_texture);
        commands.push(DrawCommand::rounded_block(
            block.id.0,
            block.rect.scaled(scale),
            physical_clips,
            interactive_fill,
            border_color,
            block.border_radius * scale,
            block.border_width * scale,
            fill,
            border,
            opacity,
            if block.reveal {
                let strength = reveal_strength();
                if block.reveal_border_only {
                    -strength
                } else {
                    strength
                }
            } else {
                0.0
            },
            if block.reveal {
                reveal_target()
            } else {
                Color::TRANSPARENT
            },
            block.texture_uv,
            block.texture_mode,
            block.texture_rotation,
        ));
    }

    if let Some(text) = block.text.as_deref() {
        let (content_clips, clip_count) = scaled_clips(&block.content_clips, scale);
        emit_text(
            &TextRenderArgs {
                block,
                text,
                clips: clip_prefix(&content_clips, clip_count),
                opacity,
                text_color: foreground.map_or(block.text_color, |kind| kind.resolve(background)),
            },
            emit.gui,
            emit.renderer,
            commands,
        )?;
    }
    for child in &block.children {
        emit_block(
            child,
            emit,
            opacity,
            effective_layer,
            background,
            foreground,
        )?;
    }
    Ok(())
}

fn composite_over(foreground: Color, background: Color) -> Color {
    let alpha = background.a.mul_add(1.0 - foreground.a, foreground.a);
    if alpha <= 1e-6 {
        return Color::TRANSPARENT;
    }
    let background_weight = background.a * (1.0 - foreground.a);
    Color::rgba(
        background
            .r
            .mul_add(background_weight, foreground.r * foreground.a)
            / alpha,
        background
            .g
            .mul_add(background_weight, foreground.g * foreground.a)
            / alpha,
        background
            .b
            .mul_add(background_weight, foreground.b * foreground.a)
            / alpha,
        alpha,
    )
}

fn scaled_clips(clips: &[ClipShape], scale: f32) -> ([ClipShape; MAX_CLIPS], usize) {
    let mut scaled = [ClipShape::default(); MAX_CLIPS];
    let count = clips.len().min(MAX_CLIPS);
    for (output, clip) in scaled.iter_mut().zip(clips.iter()) {
        *output = ClipShape {
            rect: clip.rect.scaled(scale),
            radius: clip.radius * scale,
        };
    }
    (scaled, count)
}

#[expect(
    clippy::expect_used,
    reason = "scaled_clips limits the returned count to the fixed array length"
)]
fn clip_prefix(clips: &[ClipShape; MAX_CLIPS], count: usize) -> &[ClipShape] {
    clips
        .get(..count)
        .expect("scaled clip count exceeds capacity")
}

struct TextRenderArgs<'a> {
    block: &'a Block,
    text: &'a str,
    clips: &'a [ClipShape],
    opacity: f32,
    text_color: Color,
}

fn emit_text(
    args: &TextRenderArgs<'_>,
    gui: &mut Gui,
    renderer: &mut Renderer,
    commands: &mut Vec<DrawCommand>,
) -> Result<()> {
    let text_rect = args.block.content_rect;
    if text_rect.width <= 0.0 || text_rect.height <= 0.0 {
        return Ok(());
    }

    let scale = renderer.scale_factor();
    let block = args.block;
    let clips = args.clips;
    let opacity = args.opacity;
    let text_color = args.text_color;
    let mut buffer = Buffer::new(
        &mut gui.font_system,
        Metrics::new(
            args.block.font_size,
            args.block.font_size * text_measure::LINE_HEIGHT,
        ),
    );
    let physical_glyphs = {
        let mut borrowed = buffer.borrow_with(&mut gui.font_system);
        borrowed.set_size(Some(text_rect.width), Some(text_rect.height));
        if !args.block.text_wrap {
            borrowed.set_wrap(Wrap::None);
        }
        let attrs = match args.block.font_kind {
            FontKind::Sans => Attrs::new(),
            FontKind::Monospace => Attrs::new().family(Family::Monospace),
        };
        let horizontal_align = match args.block.text_align {
            Align::Start => CosmicAlign::Left,
            Align::Center => CosmicAlign::Center,
            Align::End => CosmicAlign::Right,
        };
        borrowed.set_text(args.text, &attrs, Shaping::Advanced, Some(horizontal_align));

        let runs: Vec<_> = borrowed.layout_runs().collect();
        let content_height = runs
            .last()
            .map_or(0.0, |run| run.line_top + run.line_height);
        let y_shift = (text_rect.height - content_height).max(0.0)
            * match args.block.text_vertical_align {
                Align::Start => 0.0,
                Align::Center => 0.5,
                Align::End => 1.0,
            };

        let mut physical = Vec::new();
        for run in runs {
            let origin = (
                text_rect.x * scale,
                ((text_rect.y + run.line_y + y_shift) * scale).round(),
            );
            for glyph in run.glyphs {
                let font_size = glyph.font_size * scale;
                let x = glyph
                    .font_size
                    .mul_add(glyph.x_offset, glyph.x)
                    .mul_add(scale, origin.0);
                let y = glyph
                    .font_size
                    .mul_add(-glyph.y_offset, glyph.y)
                    .mul_add(scale, origin.1);
                if !font_size.is_finite()
                    || font_size <= 0.0
                    || font_size > 16_384.0
                    || !x.is_finite()
                    || !y.is_finite()
                    || x.abs() >= 2_000_000_000.0
                    || y.abs() >= 2_000_000_000.0
                {
                    continue;
                }
                physical.push(glyph.physical(origin, scale));
            }
        }
        physical
    };

    for physical in physical_glyphs {
        let entry = if let Some(entry) = gui.glyphs.get(&physical.cache_key).copied() {
            entry
        } else {
            let Some(image) = gui
                .swash_cache
                .get_image(&mut gui.font_system, physical.cache_key)
                .as_ref()
            else {
                continue;
            };
            let width = image.placement.width;
            let height = image.placement.height;
            if width == 0 || height == 0 {
                continue;
            }
            let rgba = swash_to_rgba(image.content, &image.data, width, height);
            let entry = renderer.upload_glyph(width, height, &rgba)?;
            gui.glyphs.insert(physical.cache_key, entry);
            entry
        };

        let image = gui
            .swash_cache
            .get_image(&mut gui.font_system, physical.cache_key)
            .as_ref()
            .ok_or_else(|| anyhow!(""))?;
        let rect = Rect::new(
            (physical.x.saturating_add(image.placement.left)) as f32,
            (physical.y.saturating_sub(image.placement.top)) as f32,
            image.placement.width as f32,
            image.placement.height as f32,
        );
        let glyph_color = if matches!(&image.content, SwashContent::Color) {
            Color::rgba(1.0, 1.0, 1.0, text_color.a)
        } else {
            text_color
        };
        commands.push(DrawCommand::glyph(
            block.id.0,
            rect,
            clips,
            glyph_color,
            entry.uv,
            TextureKind::Glyph,
            opacity,
        ));
    }
    Ok(())
}

fn swash_to_rgba(content: SwashContent, data: &[u8], width: u32, height: u32) -> Vec<u8> {
    let count = (width as usize).saturating_mul(height as usize);
    match content {
        SwashContent::Mask => data
            .iter()
            .take(count)
            .flat_map(|alpha| [255, 255, 255, *alpha])
            .collect(),
        SwashContent::SubpixelMask => data
            .as_chunks::<4>()
            .0
            .iter()
            .take(count)
            .flat_map(|rgba| [255, 255, 255, rgba[0].max(rgba[1]).max(rgba[2])])
            .collect(),
        SwashContent::Color => data.to_vec(),
    }
}

const fn hash_pair(a: u64, b: u64) -> u64 {
    let mut hash = a ^ 0x9e37_79b9_7f4a_7c15;
    hash ^= b
        .wrapping_add(0x9e37_79b9_7f4a_7c15)
        .wrapping_add(hash << 6)
        .wrapping_add(hash >> 2);
    avalanche(hash)
}

fn hash_value<K: Hash>(seed: u64, value: &K) -> u64 {
    struct SeededHasher(u64);
    impl Hasher for SeededHasher {
        fn finish(&self) -> u64 {
            avalanche(self.0)
        }
        fn write(&mut self, bytes: &[u8]) {
            for &byte in bytes {
                self.0 ^= u64::from(byte);
                self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
            }
        }
    }
    let mut hasher = SeededHasher(seed ^ 0xcbf2_9ce4_8422_2325);
    value.hash(&mut hasher);
    hasher.finish()
}

const fn avalanche(mut value: u64) -> u64 {
    value ^= value >> 30;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    #[test]
    fn contrast_foreground_tracks_fill_lightness() {
        assert_eq!(
            ColorKind::Contrast.resolve(Color::rgb8(0xf4, 0xd8, 0x5d)),
            Color::BLACK
        );
        assert_eq!(
            ColorKind::Contrast.resolve(Color::rgb8(0x32, 0x28, 0x48)),
            Color::WHITE
        );
    }

    #[test]
    fn contrast_foreground_falls_back_for_transparent_fill() {
        assert_eq!(
            ColorKind::Contrast.resolve(Color::TRANSPARENT),
            default_text_color()
        );
    }

    #[test]
    fn fit_text_uses_shaped_glyph_advances() {
        let ((wide, narrow), measured) = measure_layout(Rect::new(0.0, 0.0, 400.0, 100.0), |ctx| {
            let wide = ctx
                .new()
                .width(Size::Fit)
                .height(Size::Pixels(20.0))
                .font_size(10.0)
                .text("WWWWWWWWWW")
                .build();
            let narrow = ctx
                .new()
                .width(Size::Fit)
                .height(Size::Pixels(20.0))
                .font_size(10.0)
                .text("iiiiiiiiii")
                .build();
            (wide, narrow)
        });

        assert!(measured.rect(wide).unwrap().width > measured.rect(narrow).unwrap().width);
    }

    #[test]
    fn explicit_clip_tracks_nested_parent_origin() {
        let mut ctx = BuildCtx::with_seed(ROOT_SCOPE_SEED);
        let _ = ctx
            .new()
            .bounds((40.0, 30.0, 100.0, 100.0))
            .children(|ctx| {
                ctx.with_clip(Rect::new(5.0, 6.0, 30.0, 20.0), |ctx| {
                    let _ = ctx.new().bounds((7.0, 8.0, 10.0, 10.0)).build();
                });
            })
            .build();

        let mut blocks = ctx.blocks;
        layout_roots(&mut blocks, Rect::new(0.0, 0.0, 300.0, 200.0));
        let child = &blocks[0].children[0];

        assert_eq!(child.rect, Rect::new(47.0, 38.0, 10.0, 10.0));
        assert_eq!(child.clips.len(), 1);
        assert_eq!(child.clips[0].rect, Rect::new(45.0, 36.0, 30.0, 20.0));
    }

    #[test]
    fn fit_container_honors_fixed_child_sizes() {
        let ((root, first, second), measured) =
            measure_layout(Rect::new(0.0, 0.0, 400.0, 400.0), |ctx| {
                let mut first = BlockId(0);
                let mut second = BlockId(0);
                let root = ctx
                    .new()
                    .overlay()
                    .width(Size::Pixels(180.0))
                    .height(Size::Fit)
                    .gap(3.0)
                    .padding(6.0)
                    .children(|ctx| {
                        first = ctx
                            .new()
                            .width(Size::Fill)
                            .height(Size::Pixels(38.0))
                            .build();
                        second = ctx
                            .new()
                            .width(Size::Fill)
                            .height(Size::Pixels(62.0))
                            .build();
                    })
                    .build();
                (root, first, second)
            });

        let root = measured.rect(root).unwrap();
        let first = measured.rect(first).unwrap();
        let second = measured.rect(second).unwrap();
        assert_eq!(root.height, 115.0);
        assert_eq!(first.height, 38.0);
        assert_eq!(second.height, 62.0);
        assert_eq!(first.y, 6.0);
        assert_eq!(second.y, 47.0);
    }

    #[test]
    fn positioned_fit_container_can_exceed_viewport() {
        let ((root, child), measured) = measure_layout(Rect::new(0.0, 0.0, 160.0, 80.0), |ctx| {
            let mut child = BlockId(0);
            let root = ctx
                .new()
                .overlay()
                .position((0.0, 0.0))
                .width(Size::Pixels(120.0))
                .height(Size::Fit)
                .children(|ctx| {
                    child = ctx
                        .new()
                        .width(Size::Fill)
                        .height(Size::Pixels(240.0))
                        .build();
                })
                .build();
            (root, child)
        });

        assert_eq!(measured.rect(root).unwrap().height, 240.0);
        assert_eq!(measured.rect(child).unwrap().height, 240.0);
    }

    #[test]
    fn fit_container_uses_fill_child_intrinsic_minimum() {
        let ((root, fill, row), measured) =
            measure_layout(Rect::new(0.0, 0.0, 400.0, 400.0), |ctx| {
                let mut fill = BlockId(0);
                let mut row = BlockId(0);
                let root = ctx
                    .new()
                    .overlay()
                    .width(Size::Pixels(200.0))
                    .height(Size::Fit)
                    .children(|ctx| {
                        fill = ctx
                            .new()
                            .width(Size::Fill)
                            .height(Size::Fill)
                            .children(|ctx| {
                                row = ctx
                                    .new()
                                    .width(Size::Pixels(120.0))
                                    .height(Size::Pixels(44.0))
                                    .build();
                            })
                            .build();
                    })
                    .build();
                (root, fill, row)
            });

        assert_eq!(measured.rect(root).unwrap().height, 44.0);
        assert_eq!(measured.rect(fill).unwrap().height, 44.0);
        assert_eq!(measured.rect(row).unwrap().height, 44.0);
    }

    #[test]
    fn fill_portions_share_remaining_space_by_weight() {
        let ((one, two), measured) = measure_layout(Rect::new(0.0, 0.0, 300.0, 40.0), |ctx| {
            let mut one = BlockId::default();
            let mut two = BlockId::default();
            let _ = ctx
                .new()
                .width(Size::Fill)
                .height(Size::Fill)
                .row()
                .children(|ctx| {
                    one = ctx
                        .new()
                        .width(Size::FillPortion(1.0))
                        .height(Size::Fill)
                        .build();
                    two = ctx
                        .new()
                        .width(Size::FillPortion(2.0))
                        .height(Size::Fill)
                        .build();
                })
                .build();
            (one, two)
        });

        assert_eq!(measured.rect(one).unwrap().width, 100.0);
        assert_eq!(measured.rect(two).unwrap().width, 200.0);
    }

    #[test]
    fn fit_scale_scales_intrinsic_main_axis_size() {
        let ((scaled, child), measured) =
            measure_layout(Rect::new(0.0, 0.0, 200.0, 200.0), |ctx| {
                let mut child = BlockId::default();
                let scaled = ctx
                    .new()
                    .width(Size::Fill)
                    .height(Size::FitScale(0.25))
                    .children(|ctx| {
                        child = ctx
                            .new()
                            .width(Size::Fill)
                            .height(Size::Pixels(80.0))
                            .build();
                    })
                    .build();
                (scaled, child)
            });

        assert_eq!(measured.rect(scaled).unwrap().height, 20.0);
        assert_eq!(measured.rect(child).unwrap().height, 80.0);
    }

    #[test]
    fn popup_placement_reports_downward_direction() {
        let placement = place_popup_with_direction(
            Rect::new(20.0, 20.0, 80.0, 24.0),
            [120.0, 100.0],
            Rect::new(0.0, 0.0, 400.0, 400.0),
            false,
            4.0,
        );

        assert_eq!(placement.direction, PopupDirection::Down);
        assert!(placement.rect.y >= 48.0);
    }

    #[test]
    fn popup_placement_reports_upward_direction_when_below_does_not_fit() {
        let anchor = Rect::new(20.0, 350.0, 80.0, 24.0);
        let placement = place_popup_with_direction(
            anchor,
            [120.0, 100.0],
            Rect::new(0.0, 0.0, 400.0, 400.0),
            false,
            4.0,
        );

        assert_eq!(placement.direction, PopupDirection::Up);
        assert!(placement.rect.bottom() <= anchor.y - 4.0 + f32::EPSILON);
    }

    #[test]
    fn popup_placement_shrinks_on_one_side_without_overlapping_anchor() {
        let anchor = Rect::new(20.0, 50.0, 80.0, 24.0);
        let placement = place_popup_with_direction(
            anchor,
            [120.0, 100.0],
            Rect::new(0.0, 0.0, 200.0, 140.0),
            false,
            4.0,
        );

        assert_eq!(placement.direction, PopupDirection::Down);
        assert!(placement.rect.y >= anchor.bottom() + 4.0 - f32::EPSILON);
        assert!(placement.rect.bottom() <= 134.0 + f32::EPSILON);
        assert!(placement.rect.height < 100.0);
    }

    #[test]
    fn scroll_range_comes_from_laid_out_content() {
        let (scroll, measured) = measure_layout(Rect::new(0.0, 0.0, 120.0, 70.0), |ctx| {
            ctx.new()
                .column()
                .width(Size::Fill)
                .height(Size::Fill)
                .vertical_scroll(ScrollState::default())
                .gap(5.0)
                .children(|ctx| {
                    for _ in 0..3 {
                        let _ = ctx
                            .new()
                            .width(Size::Fill)
                            .height(Size::Pixels(30.0))
                            .build();
                    }
                })
                .build()
        });

        assert_eq!(
            measured.scroll_range(scroll),
            Some(ScrollRange {
                horizontal: 0.0,
                vertical: 30.0,
            })
        );
    }

    #[test]
    fn fit_cross_axis_honors_fixed_child_size() {
        let ((root, child), measured) = measure_layout(Rect::new(0.0, 0.0, 400.0, 400.0), |ctx| {
            let mut child = BlockId(0);
            let root = ctx
                .new()
                .overlay()
                .width(Size::Fit)
                .height(Size::Pixels(40.0))
                .children(|ctx| {
                    child = ctx
                        .new()
                        .width(Size::Pixels(175.0))
                        .height(Size::Fill)
                        .build();
                })
                .build();
            (root, child)
        });

        assert_eq!(measured.rect(root).unwrap().width, 175.0);
        assert_eq!(measured.rect(child).unwrap().width, 175.0);
    }
}

#[cfg(test)]
mod cursor_tests {
    use super::*;

    fn block(id: u64, cursor: CursorShape, interactive: bool) -> Block {
        let mut block = Block::new(BlockId(id), id);
        block.rect = Rect::new(0.0, 0.0, 20.0, 20.0);
        block.cursor = cursor;
        block.interactive = interactive;
        block
    }

    #[test]
    fn interactive_blocks_default_to_pointer_cursor() {
        let blocks = vec![block(1, CursorShape::Auto, true)];
        assert_eq!(cursor_at(&blocks, [10.0, 10.0]), CursorShape::Pointer);
    }

    #[test]
    fn display_none_subtree_cannot_capture_pointer_input() {
        let behind = block(1, CursorShape::Pointer, true);
        let mut hidden = block(2, CursorShape::EwResize, true);
        hidden.layout.display = taffy::Display::None;
        hidden.children.push(block(3, CursorShape::Grab, true));
        let blocks = [behind, hidden];

        assert_eq!(hit_test(&blocks, [10.0, 10.0]), Some(BlockId(1)));
        assert_eq!(cursor_at(&blocks, [10.0, 10.0]), CursorShape::Pointer);
    }

    #[test]
    fn native_overflow_clips_input_only_on_the_requested_axis() {
        let mut ctx = BuildCtx::with_seed(ROOT_SCOPE_SEED);
        let mut child = BlockId::default();
        let _ = ctx
            .new()
            .layout(taffy::Style {
                position: taffy::Position::Relative,
                size: taffy::Size::from_lengths(40.0, 20.0),
                overflow: taffy::Point {
                    x: taffy::Overflow::Visible,
                    y: taffy::Overflow::Hidden,
                },
                ..taffy::Style::default()
            })
            .children(|ctx| {
                child = ctx
                    .new()
                    .layout(taffy::Style {
                        position: taffy::Position::Absolute,
                        inset: taffy::Rect {
                            left: taffy::LengthPercentageAuto::length(60.0),
                            top: taffy::LengthPercentageAuto::length(10.0),
                            ..taffy::Rect::auto()
                        },
                        size: taffy::Size::from_lengths(20.0, 20.0),
                        ..taffy::Style::default()
                    })
                    .interactive()
                    .build();
            })
            .build();
        layout_roots(&mut ctx.blocks, Rect::new(0.0, 0.0, 100.0, 100.0));

        assert_eq!(hit_test(&ctx.blocks, [70.0, 15.0]), Some(child));
        assert_eq!(hit_test(&ctx.blocks, [70.0, 25.0]), None);
    }

    #[test]
    fn passthrough_cursor_falls_through_to_block_behind() {
        let behind = block(1, CursorShape::Pointer, false);
        let front = block(2, CursorShape::Passthrough, true);
        assert_eq!(
            cursor_at(&[behind, front], [10.0, 10.0]),
            CursorShape::Pointer
        );
    }

    #[test]
    fn explicit_cursor_shape_wins_over_block_behind() {
        let behind = block(1, CursorShape::Pointer, false);
        let front = block(2, CursorShape::EwResize, false);
        assert_eq!(
            cursor_at(&[behind, front], [10.0, 10.0]),
            CursorShape::EwResize
        );
    }

    #[test]
    fn popup_cursor_wins_over_later_overlay() {
        let mut popup = block(1, CursorShape::Pointer, true);
        popup.layer = Layer::Popup;
        let mut overlay = block(2, CursorShape::EwResize, true);
        overlay.layer = Layer::Overlay;
        assert_eq!(
            cursor_at(&[popup, overlay], [10.0, 10.0]),
            CursorShape::Pointer
        );
    }
}
