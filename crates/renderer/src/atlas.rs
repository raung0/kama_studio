use anyhow::{bail, Context, Result};
use std::fmt;

#[derive(Debug)]
pub struct AtlasFull {
    width: u32,
    height: u32,
}

impl AtlasFull {
    const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

impl fmt::Display for AtlasFull {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "texture atlas is full ({}x{})", self.width, self.height)
    }
}

impl std::error::Error for AtlasFull {}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AtlasEntry {
    pub uv: [f32; 4],
    pub width: u32,
    pub height: u32,
    pub(crate) x: u32,
    pub(crate) y: u32,
}

pub struct TextureAtlas {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
    bytes_per_pixel: u32,
    next_x: u32,
    next_y: u32,
    row_height: u32,
}

impl TextureAtlas {
    pub fn new(
        device: &wgpu::Device,
        label: &str,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
        bytes_per_pixel: u32,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            texture,
            view,
            width,
            height,
            bytes_per_pixel,
            next_x: 1,
            next_y: 1,
            row_height: 0,
        }
    }

    pub const fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    pub const fn reset(&mut self) {
        self.next_x = 1;
        self.next_y = 1;
        self.row_height = 0;
    }

    pub fn upload(
        &mut self,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        pixels: &[u8],
    ) -> Result<AtlasEntry> {
        if width == 0 || height == 0 {
            return Ok(AtlasEntry {
                uv: [0.0; 4],
                width,
                height,
                x: 0,
                y: 0,
            });
        }
        self.validate(width, height, pixels, "upload")?;

        let padded_width = width
            .checked_add(1)
            .context("atlas upload width overflow")?;
        let padded_height = height
            .checked_add(1)
            .context("atlas upload height overflow")?;
        if padded_width >= self.width || padded_height >= self.height {
            bail!(
                "texture does not fit in atlas ({}x{})",
                self.width,
                self.height
            );
        }
        if self.next_x.saturating_add(padded_width) > self.width {
            self.next_x = 1;
            self.next_y = self
                .next_y
                .checked_add(self.row_height + 1)
                .context("atlas row position overflow")?;
            self.row_height = 0;
        }
        if self.next_y.saturating_add(padded_height) > self.height {
            return Err(AtlasFull::new(self.width, self.height).into());
        }

        let x = self.next_x;
        let y = self.next_y;
        self.next_x += padded_width;
        self.row_height = self.row_height.max(height);

        self.write(queue, x, y, width, height, pixels);

        Ok(AtlasEntry {
            uv: [
                (x as f32 + 0.5) / self.width as f32,
                (y as f32 + 0.5) / self.height as f32,
                (x as f32 + width as f32 - 0.5) / self.width as f32,
                (y as f32 + height as f32 - 0.5) / self.height as f32,
            ],
            width,
            height,
            x,
            y,
        })
    }

    pub fn update(&self, queue: &wgpu::Queue, entry: AtlasEntry, pixels: &[u8]) -> Result<()> {
        self.validate(entry.width, entry.height, pixels, "update")?;
        if entry.width == 0 || entry.height == 0 {
            return Ok(());
        }
        self.write(queue, entry.x, entry.y, entry.width, entry.height, pixels);
        Ok(())
    }

    fn validate(&self, width: u32, height: u32, pixels: &[u8], action: &str) -> Result<()> {
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|value| value.checked_mul(self.bytes_per_pixel as usize))
            .with_context(|| format!("atlas {action} byte count overflow"))?;
        if pixels.len() != expected {
            bail!(
                "atlas {action} expected {expected} bytes for {width}x{height}, got {}",
                pixels.len()
            );
        }
        Ok(())
    }

    fn write(&self, queue: &wgpu::Queue, x: u32, y: u32, width: u32, height: u32, pixels: &[u8]) {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * self.bytes_per_pixel),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
    }
}
