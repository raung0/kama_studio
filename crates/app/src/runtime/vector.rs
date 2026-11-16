use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, OnceLock},
};

use anyhow::{Context, Result, bail};
use resvg::{tiny_skia, usvg};

use crate::project::{HostBinding, HostValue};

#[derive(Clone, Copy)]
pub enum VectorFilter {
    ColorMatrix([[f32; 4]; 5]),
    Blur(f32),
}

impl VectorFilter {
    pub fn commutes_with_transform(self) -> bool {
        let Self::ColorMatrix(matrix) = self else {
            return false;
        };
        matrix.iter().take(3).all(|row| row[3] == 0.0)
            && matrix[3].iter().take(3).all(|value| *value == 0.0)
            && matrix[4][3] == 0.0
    }

    pub fn svg(self, id: &str, size: [u32; 2], content: &str) -> String {
        let primitive = match self {
            Self::ColorMatrix(matrix) => {
                let offsets = matrix.get(4).copied().unwrap_or([0.0; 4]);
                let values = matrix
                    .iter()
                    .take(4)
                    .zip(offsets)
                    .flat_map(|(row, offset)| row.iter().copied().chain(std::iter::once(offset)))
                    .map(|value| value.to_string())
                    .collect::<Vec<_>>()
                    .join(" ");
                format!("<feColorMatrix type=\"matrix\" values=\"{values}\"/>")
            }
            Self::Blur(radius) => format!("<feGaussianBlur stdDeviation=\"{}\"/>", radius / 3.0),
        };
        format!(
            "<defs><filter id=\"{id}\" filterUnits=\"userSpaceOnUse\" x=\"0\" y=\"0\" width=\"{}\" height=\"{}\" color-interpolation-filters=\"linearRGB\">{primitive}</filter></defs><g filter=\"url(#{id})\">{content}</g>",
            size[0], size[1]
        )
    }
}

#[derive(Clone)]
pub struct VectorImage {
    pub tree: Arc<usvg::Tree>,
    pub transform: tiny_skia::Transform,
    feather: f32,
}

impl VectorImage {
    pub fn from_svg(data: &[u8]) -> Result<Self> {
        static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
        let fontdb = Arc::clone(FONTS.get_or_init(|| {
            let mut fonts = usvg::fontdb::Database::new();
            fonts.load_system_fonts();
            Arc::new(fonts)
        }));
        let mut options = usvg::Options {
            fontdb,
            ..usvg::Options::default()
        };
        options.image_href_resolver.resolve_string = Box::new(|_, _| None);
        let tree = usvg::Tree::from_data(data, &options).context("parse SVG vector data")?;
        Ok(Self {
            tree: Arc::new(tree),
            transform: tiny_skia::Transform::identity(),
            feather: 0.0,
        })
    }

    pub fn from_file(path: &Path) -> Result<Self> {
        Self::from_svg(
            &std::fs::read(path).with_context(|| format!("read SVG {}", path.display()))?,
        )
    }

    pub fn size(&self) -> [f32; 2] {
        [self.tree.size().width(), self.tree.size().height()]
    }

    pub(crate) fn rasterize_srgb(&self, size: [u32; 2]) -> Result<tiny_skia::Pixmap> {
        if size[0] == 0 || size[1] == 0 {
            bail!("vector raster size must be nonzero");
        }
        let mut pixmap =
            tiny_skia::Pixmap::new(size[0], size[1]).context("allocate vector raster")?;
        resvg::render(&self.tree, self.transform, &mut pixmap.as_mut());
        Ok(pixmap)
    }

    pub fn transformed(&self, transform: tiny_skia::Transform) -> Self {
        Self {
            tree: Arc::clone(&self.tree),
            transform: self.transform.post_concat(transform),
            feather: self.feather,
        }
    }

    pub(crate) fn feather_radius(&self) -> f32 {
        let t = self.transform;
        self.feather * t.sx.hypot(t.ky).max(t.kx.hypot(t.sy))
    }

    pub fn to_svg(&self, size: [u32; 2], prefix: &str) -> String {
        let inner = self.tree.to_string(&usvg::WriteOptions {
            id_prefix: Some(prefix.to_owned()),
            ..usvg::WriteOptions::default()
        });
        let transform = self.transform;
        let filter = if self.feather > 0.0 {
            format!(
                "<defs><filter id=\"{prefix}feather\" filterUnits=\"userSpaceOnUse\" x=\"0\" y=\"0\" width=\"{}\" height=\"{}\"><feGaussianBlur stdDeviation=\"{}\"/></filter></defs>",
                size[0],
                size[1],
                self.feather_radius() / 3.0
            )
        } else {
            String::new()
        };
        let filter_attr = if self.feather > 0.0 {
            format!(" filter=\"url(#{prefix}feather)\"")
        } else {
            String::new()
        };
        format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{}\" height=\"{}\">{filter}<g{filter_attr}><g transform=\"matrix({} {} {} {} {} {})\">{}</g></g></svg>",
            size[0],
            size[1],
            transform.sx,
            transform.ky,
            transform.kx,
            transform.sy,
            transform.tx,
            transform.ty,
            inner
        )
    }

    pub fn generator(
        key: &str,
        parameters: &BTreeMap<String, HostBinding>,
        time: f64,
        canvas_size: [f32; 2],
        origin: [f32; 2],
    ) -> Result<Option<Self>> {
        if !matches!(
            key,
            "builtin.polygon" | "builtin.shape" | "builtin.regular_polygon"
        ) {
            return Ok(None);
        }
        let value = |name: &str| {
            parameters
                .get(name)
                .and_then(|binding| binding.evaluate(time))
        };
        let scalar = |name: &str, fallback: f32| match value(name) {
            Some(HostValue::Gpu(v)) => {
                v.numeric(None).unwrap_or_else(|| f64::from(fallback)) as f32
            }
            Some(HostValue::F32List(v)) => v.first().copied().unwrap_or(fallback),
            _ => fallback,
        };
        let vector = |name: &str, fallback: [f32; 2]| match value(name) {
            Some(HostValue::Gpu(v)) => [
                v.numeric(Some(0)).unwrap_or_else(|| f64::from(fallback[0])) as f32,
                v.numeric(Some(1)).unwrap_or_else(|| f64::from(fallback[1])) as f32,
            ],
            _ => fallback,
        };
        let color = |name: &str, fallback: [f32; 4]| match value(name) {
            Some(HostValue::Gpu(v)) => (0..4)
                .zip(fallback)
                .map(|(index, default)| {
                    v.numeric(Some(index)).unwrap_or_else(|| f64::from(default)) as f32
                })
                .collect::<Vec<_>>()
                .try_into()
                .unwrap_or(fallback),
            _ => fallback,
        };
        let points = match value("points") {
            Some(HostValue::Vec2Array(v)) => v,
            _ => Vec::new(),
        };
        let controls_in = match value("controls_in") {
            Some(HostValue::Vec2Array(v)) => v,
            _ => Vec::new(),
        };
        let controls_out = match value("controls_out") {
            Some(HostValue::Vec2Array(v)) => v,
            _ => Vec::new(),
        };
        let shape_origin = vector("shape_origin", [0.0, 0.0]);
        let size = vector("size", [960.0, 540.0]);
        let radius = vector("radius", [480.0, 270.0]);
        let path;
        if key == "builtin.regular_polygon" {
            let count = usize::try_from(scalar("sides", 6.0).clamp(3.0, 64.0) as i64).unwrap_or(3);
            let r = scalar("radius", 0.35).max(0.001) * canvas_size[1] * 0.5;
            let angle = scalar("rotation", 0.0).to_radians() + std::f32::consts::PI / count as f32;
            let vertices: Vec<[f32; 2]> = (0..count)
                .map(|i| {
                    let a = angle + std::f32::consts::TAU * i as f32 / count as f32;
                    [
                        r.mul_add(a.cos(), canvas_size[0] * 0.5),
                        r.mul_add(a.sin(), canvas_size[1] * 0.5),
                    ]
                })
                .collect();
            path = path_for_points(&vertices, &[], &[], origin);
        } else if key == "builtin.shape" || key == "builtin.polygon" {
            let shape_type = enum_value(scalar("shape_type", 0.0));
            let dimensions = if shape_type == 1 {
                radius.map(|v| v * 2.0)
            } else {
                size
            };
            let offset = if key == "builtin.shape" {
                [
                    (canvas_size[0] - dimensions[0]) * 0.5,
                    (canvas_size[1] - dimensions[1]) * 0.5,
                ]
            } else {
                shape_origin
            };
            let x = offset[0] - origin[0];
            let y = offset[1] - origin[1];
            if key == "builtin.shape" {
                match shape_type {
                    0 => path = format!("M{x} {y} h{} v{} h-{} Z", size[0], size[1], size[0]),
                    1 => {
                        path = format!(
                            "M {} {} a {} {} 0 1 0 {} 0 a {} {} 0 1 0 -{} 0 Z",
                            x,
                            y + radius[1],
                            radius[0],
                            radius[1],
                            radius[0] * 2.0,
                            radius[0],
                            radius[1],
                            radius[0] * 2.0
                        );
                    }
                    _ => return Ok(None),
                }
            } else {
                match shape_type {
                    0 => path = path_for_points(&points, &controls_in, &controls_out, origin),
                    1 => path = format!("M{x} {y} h{} v{} h-{} Z", size[0], size[1], size[0]),
                    2 => {
                        path = format!(
                            "M {} {} a {} {} 0 1 0 {} 0 a {} {} 0 1 0 -{} 0 Z",
                            x,
                            y + radius[1],
                            radius[0],
                            radius[1],
                            radius[0] * 2.0,
                            radius[0],
                            radius[1],
                            radius[0] * 2.0
                        );
                    }
                    _ => return Ok(None),
                }
            }
        } else {
            path = path_for_points(&points, &controls_in, &controls_out, origin);
        }
        let fill = svg_color(color("color", [0.95, 0.58, 0.12, 1.0]));
        let border = svg_color(color("border_color", [0.05, 0.05, 0.05, 1.0]));
        let width = scalar("border_width", 0.0).max(0.0);
        let alignment = enum_value(scalar("border_alignment", 1.0));
        let feather = scalar("feather", 0.0).max(0.0);
        let w = canvas_size[0].max(1.0);
        let h = canvas_size[1].max(1.0);
        let (stroke_width, clip, mask) = match alignment {
            0 => (
                width * 2.0,
                format!("<clipPath id=\"c\"><path d=\"{path}\"/></clipPath>"),
                String::new(),
            ),
            2 => (
                width * 2.0,
                String::new(),
                format!(
                    "<mask id=\"m\" maskUnits=\"userSpaceOnUse\" x=\"0\" y=\"0\" width=\"{w}\" height=\"{h}\"><rect width=\"{w}\" height=\"{h}\" fill=\"white\"/><path d=\"{path}\" fill=\"black\"/></mask>"
                ),
            ),
            _ => (width, String::new(), String::new()),
        };
        let stroke = if alignment == 2 {
            String::new()
        } else {
            format!(
                "<path d=\"{path}\" fill=\"none\" stroke=\"#{:02x}{:02x}{:02x}\" stroke-opacity=\"{}\" stroke-width=\"{stroke_width}\"{} />",
                border[0],
                border[1],
                border[2],
                f32::from(border[3]) / 255.0,
                if alignment == 0 {
                    " clip-path=\"url(#c)\""
                } else {
                    ""
                }
            )
        };
        let outside = if alignment == 2 {
            format!(
                "<path d=\"{path}\" fill=\"none\" stroke=\"#{:02x}{:02x}{:02x}\" stroke-opacity=\"{}\" stroke-width=\"{}\" mask=\"url(#m)\"/>",
                border[0],
                border[1],
                border[2],
                f32::from(border[3]) / 255.0,
                width * 2.0
            )
        } else {
            String::new()
        };
        let svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" viewBox=\"0 0 {w} {h}\"><defs>{clip}{mask}</defs><g><path d=\"{path}\" fill=\"#{:02x}{:02x}{:02x}\" fill-opacity=\"{}\"/>{stroke}{outside}</g></svg>",
            fill[0],
            fill[1],
            fill[2],
            f32::from(fill[3]) / 255.0
        );
        let tree = usvg::Tree::from_data(svg.as_bytes(), &usvg::Options::default())
            .context("parse generated vector geometry")?;
        Ok(Some(Self {
            tree: Arc::new(tree),
            transform: tiny_skia::Transform::identity(),
            feather,
        }))
    }
}

fn path_for_points(
    points: &[[f32; 2]],
    controls_in: &[[f32; 2]],
    controls_out: &[[f32; 2]],
    origin: [f32; 2],
) -> String {
    use std::fmt::Write as _;

    let Some(first) = points.first() else {
        return String::new();
    };
    let zero = [0.0, 0.0];
    let outgoing: Vec<_> = controls_out
        .iter()
        .copied()
        .chain(std::iter::repeat(zero))
        .take(points.len())
        .collect();
    let mut incoming: Vec<_> = controls_in
        .iter()
        .copied()
        .chain(std::iter::repeat(zero))
        .take(points.len())
        .collect();
    incoming.rotate_left(1);
    let mut d = format!("M {} {}", first[0] - origin[0], first[1] - origin[1]);
    for (((a, b), out), incoming) in points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .zip(outgoing)
        .zip(incoming)
    {
        if out
            .iter()
            .chain(incoming.iter())
            .any(|offset| offset.abs() > f32::EPSILON)
        {
            let _ = write!(
                d,
                " C {} {} {} {} {} {}",
                a[0] + out[0] - origin[0],
                a[1] + out[1] - origin[1],
                b[0] + incoming[0] - origin[0],
                b[1] + incoming[1] - origin[1],
                b[0] - origin[0],
                b[1] - origin[1]
            );
        } else {
            let _ = write!(d, " L {} {}", b[0] - origin[0], b[1] - origin[1]);
        }
    }
    d.push('Z');
    d
}

fn enum_value(value: f32) -> u32 {
    u32::try_from(value as i64).unwrap_or(0)
}

fn svg_color(color: [f32; 4]) -> [u8; 4] {
    let [red, green, blue, alpha] = color;
    let encode = |value: f32| {
        let value = value.clamp(0.0, 1.0);
        u8::try_from(
            (if value <= 0.003_130_8 {
                value * 12.92
            } else {
                1.055f32.mul_add(value.powf(1.0 / 2.4), -0.055)
            } * 255.0)
                .round() as i64,
        )
        .unwrap_or(0)
    };
    [
        encode(red),
        encode(green),
        encode(blue),
        u8::try_from((alpha.clamp(0.0, 1.0) * 255.0).round() as i64).unwrap_or(0),
    ]
}

pub fn dimensions_from_file(path: &Path) -> Result<[u32; 2]> {
    let size = VectorImage::from_file(path)?.size();
    Ok(size.map(|dimension| u32::try_from(dimension.ceil() as i64).unwrap_or(0)))
}
