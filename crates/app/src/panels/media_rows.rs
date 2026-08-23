use super::*;

pub(super) const MEDIA_ROW_H: f32 = 30.0;
pub(super) const MEDIA_TRACK_H: f32 = 25.0;
pub(super) const MEDIA_ITEM_GAP: f32 = 3.0;
pub(super) fn media_list_top(rect: Rect) -> f32 {
    crate::ui_layout::inset(rect, 4.0).y
}

pub(super) fn media_disclosure_rect(row: Rect) -> Rect {
    crate::ui_layout::row(
        row,
        &[
            crate::ui_layout::Item::fill(),
            crate::ui_layout::Item::new(Size::Pixels(16.0), Size::Pixels(16.0)),
            crate::ui_layout::Item::width(5.0),
        ],
        0.0,
        0.0,
        kama_ui::Align::Center,
    )[1]
}

pub(super) fn composition_row_parts(row: Rect) -> (Rect, Rect, Rect, Rect) {
    let parts = crate::ui_layout::row(
        row,
        &[
            crate::ui_layout::Item::width(6.0),
            crate::ui_layout::Item::width(18.0),
            crate::ui_layout::Item::width(5.0),
            crate::ui_layout::Item::fill(),
            crate::ui_layout::Item::width(5.0),
            crate::ui_layout::Item::width(78.0),
            crate::ui_layout::Item::width(4.0),
            crate::ui_layout::Item::new(Size::Pixels(16.0), Size::Pixels(16.0)),
            crate::ui_layout::Item::width(5.0),
        ],
        0.0,
        0.0,
        kama_ui::Align::Center,
    );
    (parts[1], parts[3], parts[5], parts[7])
}

pub(super) fn media_row_parts(row: Rect, duration_width: f32) -> (Rect, Rect, Rect, Rect) {
    let parts = crate::ui_layout::row(
        row,
        &[
            crate::ui_layout::Item::width(6.0),
            crate::ui_layout::Item::width(18.0),
            crate::ui_layout::Item::width(4.0),
            crate::ui_layout::Item::fill(),
            crate::ui_layout::Item::width(4.0),
            crate::ui_layout::Item::width(duration_width),
            crate::ui_layout::Item::width(4.0),
            crate::ui_layout::Item::new(Size::Pixels(16.0), Size::Pixels(16.0)),
            crate::ui_layout::Item::width(5.0),
        ],
        0.0,
        0.0,
        kama_ui::Align::Center,
    );
    (parts[1], parts[3], parts[5], parts[7])
}

pub(super) fn media_stream_row_parts(row: Rect) -> (Rect, Rect) {
    let parts = crate::ui_layout::row(
        row,
        &[
            crate::ui_layout::Item::width(12.0),
            crate::ui_layout::Item::width(18.0),
            crate::ui_layout::Item::width(4.0),
            crate::ui_layout::Item::fill(),
            crate::ui_layout::Item::width(6.0),
        ],
        0.0,
        0.0,
        kama_ui::Align::Center,
    );
    (parts[1], parts[3])
}

pub(super) fn draw_composition_row(
    ctx: &mut kama_ui::BuildCtx,
    row: Rect,
    composition: &crate::project::Composition,
    selected: bool,
    active: bool,
    open_amount: f32,
    icons: Icons,
) {
    let [width, height] = composition.settings.canvas_size;
    let (symbol, name, details, disclosure) = composition_row_parts(row);
    kama_ui::ui!(ctx, {
        Rect(("composition-row", composition.id), row) {
            fill: if selected || active { theme::focused() } else { theme::control() };
            border: 1;
            border_color: if active { theme::accent() } else if selected { theme::line() } else { theme::line_soft() };
            border_radius: RADIUS_SM;
            interactive;
        }
        Block {
            id: @format("composition-disclosure-{}", composition.id);
            bounds: (disclosure.x, disclosure.y, disclosure.width, disclosure.height);
            content_centered;

            Icon {
                id: @format("composition-disclosure-icon-{}", composition.id);
                icon!: icons.get(AppIcon::Chevron);
                color!: if selected || active { theme::accent() } else { theme::muted() };
                texture_rotation: std::f32::consts::FRAC_PI_2 * open_amount;
                width: Size::Pixels(12.0);
                height: Size::Pixels(12.0);
            }
        }
        Block {
            id: @format("composition-symbol-{}", composition.id);
            bounds: (symbol.x, symbol.y, symbol.width, symbol.height);
            content_centered;

            Icon {
                id: @format("composition-symbol-icon-{}", composition.id);
                icon!: icons.get(AppIcon::Composition);
                color!: if active { theme::accent() } else { theme::muted() };
                width: Size::Pixels(16.0);
                height: Size::Pixels(16.0);
            }
        }
        Rect(("composition-name", composition.id), name) {
            font_size: 9.5;
            text_color: theme::text();
            text: composition.name.clone();
        }
        Rect(("composition-details", composition.id), details) {
            font_size: 8.0;
            text_color: theme::muted();
            text_centered;
            text: format!("{}×{} {:.2}", width, height, composition.settings.frame_rate);
        }
    });
}

pub(super) fn draw_media_row(
    ctx: &mut kama_ui::BuildCtx,
    row: Rect,
    asset: &crate::project::MediaAsset,
    selected: bool,
    open_amount: f32,
    icons: Icons,
) {
    let duration_width = 64.0f32.min(row.width * 0.3);
    let (symbol, name, duration, disclosure) = media_row_parts(row, duration_width);
    let icon = media_kind_icon(asset.kind);
    kama_ui::ui!(ctx, {
        Rect(("media-row", asset.id), row) {
            fill: if selected { theme::focused() } else { theme::control() };
            border: 1;
            border_color: if selected { theme::accent() } else { theme::line() };
            border_radius: RADIUS_SM;
            reveal;
        }
        @if !media_streams(asset).is_empty() {
            Block {
                id: @format("media-disclosure-{}", asset.id);
                bounds: (disclosure.x, disclosure.y, disclosure.width, disclosure.height);
                content_centered;

                Icon {
                    id: @format("media-disclosure-icon-{}", asset.id);
                    icon!: icons.get(AppIcon::Chevron);
                    color!: if selected { theme::accent() } else { theme::muted() };
                    texture_rotation: std::f32::consts::FRAC_PI_2 * open_amount;
                    width: Size::Pixels(12.0);
                    height: Size::Pixels(12.0);
                }
            }
        }
        Block {
            id: @format("media-kind-icon-{}", asset.id);
            bounds: (symbol.x, symbol.y, symbol.width, symbol.height);
            content_centered;

            Icon {
                id: @format("media-kind-glyph-{}", asset.id);
                icon!: icons.get(icon);
                color!: if selected { theme::accent() } else { theme::muted() };
                width: Size::Pixels(16.0);
                height: Size::Pixels(16.0);
            }
        }
        Rect(("media-name", asset.id), name) {
            font_size: 9.5;
            text_color: theme::text();
            text: asset.name.clone();
        }
        Rect(("media-duration", asset.id), duration) {
            font_size: 8.5;
            text_color: theme::muted();
            text_centered;
            text: asset.duration.map(format_duration).unwrap_or_default();
        }
    });
}

pub(super) fn media_streams(asset: &crate::project::MediaAsset) -> Vec<MediaStream> {
    if !asset.tracks.is_empty() {
        let mut video = 0usize;
        let mut audio = 0usize;
        return asset
            .tracks
            .iter()
            .map(|track| match track.kind {
                crate::project::MediaTrackKind::Video => {
                    let stream = MediaStream::Video(video);
                    video += 1;
                    stream
                }
                crate::project::MediaTrackKind::Audio => {
                    let stream = MediaStream::Audio(audio);
                    audio += 1;
                    stream
                }
            })
            .collect();
    }

    
    match asset.kind {
        MediaKind::Video => {
            let mut streams = vec![MediaStream::Video(0)];
            if asset.has_audio {
                streams.push(MediaStream::Audio(0));
            }
            streams
        }
        MediaKind::Audio => vec![MediaStream::Audio(0)],
        _ => Vec::new(),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw_media_stream_row(
    ctx: &mut kama_ui::BuildCtx,
    row: Rect,
    namespace: &'static str,
    id: u64,
    stream: MediaStream,
    selected: bool,
    track_label: bool,
    open_amount: f32,
    icons: Icons,
) {
    let (icon, label, color) = match (stream, track_label) {
        (MediaStream::Video(index), false) => (
            AppIcon::Video,
            format!("Video {}", index + 1),
            Color::rgb8(0x79, 0xb8, 0xdc),
        ),
        (MediaStream::Video(index), true) => (
            AppIcon::Video,
            format!("Video track {}", index + 1),
            Color::rgb8(0x79, 0xb8, 0xdc),
        ),
        (MediaStream::Audio(index), false) => (
            AppIcon::Audio,
            format!("Audio {}", index + 1),
            Color::rgb8(0x8e, 0xd0, 0xa5),
        ),
        (MediaStream::Audio(index), true) => (
            AppIcon::Audio,
            format!("Audio track {}", index + 1),
            Color::rgb8(0x8e, 0xd0, 0xa5),
        ),
        (MediaStream::All, _) => return,
    };
    let (symbol, name) = media_stream_row_parts(row);
    kama_ui::ui!(ctx, {
        Rect((namespace, "track", id, stream), row) {
            fill: if selected { theme::focused() } else { theme::control() };
            opacity: open_amount;
            border: 1;
            border_color: if selected { theme::accent() } else { theme::line() };
            border_radius: RADIUS_SM;
            reveal;
        }
        Block {
            id: @format("{}-track-symbol-{}-{:?}", namespace, id, stream);
            bounds: (symbol.x, symbol.y, symbol.width, symbol.height);
            opacity: open_amount;
            content_centered;

            Icon {
                id: @format("{}-track-icon-{}-{:?}", namespace, id, stream);
                icon!: icons.get(icon);
                color!: color;
                width: Size::Pixels(15.0);
                height: Size::Pixels(15.0);
            }
        }
        Rect((namespace, "track-name", id, stream), name) {
            font_size: 9.0;
            opacity: open_amount;
            text_color: if selected { theme::text() } else { theme::muted() };
            text: label;
        }
    });
}

pub(super) fn media_kind_icon(kind: MediaKind) -> AppIcon {
    match kind {
        MediaKind::Image { .. } => AppIcon::Image,
        MediaKind::Video => AppIcon::Video,
        MediaKind::Audio => AppIcon::Audio,
        MediaKind::Model3d => AppIcon::Node,
        MediaKind::WasmPlugin => AppIcon::Node,
        MediaKind::Unknown => AppIcon::Media,
    }
}

pub(super) fn media_kind_label(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Image { .. } => "Image",
        MediaKind::Video => "Video",
        MediaKind::Audio => "Audio",
        MediaKind::Model3d => "3D model",
        MediaKind::WasmPlugin => "CPU/WASM generator",
        MediaKind::Unknown => "Unknown media",
    }
}

pub(super) fn format_duration(seconds: f64) -> String {
    let total_millis = (seconds.max(0.0) * 1000.0).round() as u64;
    let minutes = total_millis / 60_000;
    let secs = (total_millis % 60_000) / 1000;
    let millis = total_millis % 1000;
    format!("{minutes:02}:{secs:02}.{millis:03}")
}
