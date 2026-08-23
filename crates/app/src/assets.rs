use anyhow::{ensure, Context, Result};
use kama_ui::{IconId, Renderer, TextureId};

pub(crate) const ICON_SIZE: u32 = 80;
const ICON_BYTES: usize = (ICON_SIZE * ICON_SIZE * 4) as usize;
const ICON_DATA: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/icons.rgba"));

include!(concat!(env!("OUT_DIR"), "/app_icons.rs"));

#[derive(Clone, Copy)]
pub struct Icons([IconId; AppIcon::COUNT]);

impl Icons {
    pub fn load(renderer: &mut Renderer) -> Result<Self> {
        ensure!(
            ICON_DATA.len() == ICON_BYTES * AppIcon::COUNT,
            "embedded icon data has the wrong size"
        );
        let fallback = renderer.builtin_icon();
        let mut ids = [fallback; AppIcon::COUNT];
        for (id, pixels) in ids.iter_mut().zip(ICON_DATA.chunks_exact(ICON_BYTES)) {
            *id = renderer.register_icon_rgba8(ICON_SIZE, ICON_SIZE, pixels)?;
        }
        Ok(Self(ids))
    }

    pub fn get(self, icon: AppIcon) -> IconId {
        self.0[icon as usize]
    }
}

#[derive(Clone, Copy)]
pub struct AboutLogos {
    pub light: TextureId,
    pub dark: TextureId,
}

impl AboutLogos {
    pub fn load(renderer: &mut Renderer) -> Result<Self> {
        Ok(Self {
            light: load_texture(renderer, include_bytes!("../assets/logo_light.png"))
                .context("load light Kama Studio logo")?,
            dark: load_texture(renderer, include_bytes!("../assets/logo_dark.png"))
                .context("load dark Kama Studio logo")?,
        })
    }
}

fn load_texture(renderer: &mut Renderer, bytes: &[u8]) -> Result<TextureId> {
    let image = image::load_from_memory(bytes)
        .context("decode embedded image")?
        .to_rgba8();
    renderer.register_texture_rgba8(image.width(), image.height(), image.as_raw())
}

pub(crate) fn icon_rgba(icon: AppIcon) -> &'static [u8] {
    let start = icon as usize * ICON_BYTES;
    &ICON_DATA[start..start + ICON_BYTES]
}
