#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const TRANSPARENT: Self = Self::rgba(0.0, 0.0, 0.0, 0.0);
    pub const WHITE: Self = Self::rgb(1.0, 1.0, 1.0);
    pub const BLACK: Self = Self::rgb(0.0, 0.0, 0.0);
    #[must_use]
    pub const fn rgb(r: f32, g: f32, b: f32) -> Self {
        Self::rgba(r, g, b, 1.0)
    }
    #[must_use]
    pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }
    #[must_use]
    pub const fn rgb8(r: u8, g: u8, b: u8) -> Self {
        Self::rgba8(r, g, b, 0xff)
    }
    #[must_use]
    pub const fn rgba8(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self::rgba(
            r as f32 / 255.0,
            g as f32 / 255.0,
            b as f32 / 255.0,
            a as f32 / 255.0,
        )
    }
    #[must_use]
    pub fn mix(self, other: Self, amount: f32) -> Self {
        let t = amount.clamp(0.0, 1.0);
        Self::rgba(
            (other.r - self.r).mul_add(t, self.r),
            (other.g - self.g).mul_add(t, self.g),
            (other.b - self.b).mul_add(t, self.b),
            (other.a - self.a).mul_add(t, self.a),
        )
    }

    #[must_use]
    pub fn from_linear(value: [f32; 4]) -> Self {
        fn linear_to_srgb(value: f32) -> f32 {
            if value <= 0.003_130_8 {
                value * 12.92
            } else {
                1.055f32.mul_add(value.max(0.0).powf(1.0 / 2.4), -0.055)
            }
        }
        Self::rgba(
            linear_to_srgb(value[0]).clamp(0.0, 1.0),
            linear_to_srgb(value[1]).clamp(0.0, 1.0),
            linear_to_srgb(value[2]).clamp(0.0, 1.0),
            value[3].clamp(0.0, 1.0),
        )
    }

    #[must_use]
    pub fn to_array(self) -> [f32; 4] {
        fn srgb_to_linear(value: f32) -> f32 {
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        }
        [
            srgb_to_linear(self.r.clamp(0.0, 1.0)),
            srgb_to_linear(self.g.clamp(0.0, 1.0)),
            srgb_to_linear(self.b.clamp(0.0, 1.0)),
            self.a.clamp(0.0, 1.0),
        ]
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}
impl Rect {
    #[must_use]
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    #[must_use]
    pub fn contains(self, p: [f32; 2]) -> bool {
        p[0] >= self.x
            && p[1] >= self.y
            && p[0] < self.x + self.width
            && p[1] < self.y + self.height
    }
    #[must_use]
    pub fn right(self) -> f32 {
        self.x + self.width
    }
    #[must_use]
    pub fn bottom(self) -> f32 {
        self.y + self.height
    }
    #[must_use]
    pub fn inset(self, amount: f32) -> Self {
        Self {
            x: self.x + amount,
            y: self.y + amount,
            width: amount.mul_add(-2.0, self.width).max(0.0),
            height: amount.mul_add(-2.0, self.height).max(0.0),
        }
    }
    #[must_use]
    pub fn lerp(self, to: Self, t: f32) -> Self {
        Self {
            x: (to.x - self.x).mul_add(t, self.x),
            y: (to.y - self.y).mul_add(t, self.y),
            width: (to.width - self.width).mul_add(t, self.width),
            height: (to.height - self.height).mul_add(t, self.height),
        }
    }
    #[must_use]
    pub fn intersect(self, other: Self) -> Self {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = (self.x + self.width).min(other.x + other.width);
        let y1 = (self.y + self.height).min(other.y + other.height);
        Self {
            x: x0,
            y: y0,
            width: (x1 - x0).max(0.0),
            height: (y1 - y0).max(0.0),
        }
    }
    #[must_use]
    pub fn scaled(self, scale: f32) -> Self {
        Self {
            x: self.x * scale,
            y: self.y * scale,
            width: self.width * scale,
            height: self.height * scale,
        }
    }
    #[must_use]
    pub fn centered_in(self, parent: Self) -> Self {
        Self {
            x: (parent.width - self.width).mul_add(0.5, parent.x),
            y: (parent.height - self.height).mul_add(0.5, parent.y),
            ..self
        }
    }
    #[must_use]
    pub const fn as_array(self) -> [f32; 4] {
        [self.x, self.y, self.width, self.height]
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ClipShape {
    pub rect: Rect,
    pub radius: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TextureId(pub u32);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ExternalTextureId(pub u32);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct IconId(pub u32);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextureSource {
    Atlas(TextureId),
    Icon(IconId),
    External(ExternalTextureId),
}
impl From<TextureId> for TextureSource {
    fn from(v: TextureId) -> Self {
        Self::Atlas(v)
    }
}
impl From<IconId> for TextureSource {
    fn from(v: IconId) -> Self {
        Self::Icon(v)
    }
}
impl From<ExternalTextureId> for TextureSource {
    fn from(v: ExternalTextureId) -> Self {
        Self::External(v)
    }
}
