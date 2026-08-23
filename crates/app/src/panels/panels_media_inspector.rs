use super::*;

pub(super) struct MediaInspectorLayout {
    pub(super) general: InspectorSectionLayout,
    pub(super) video: Option<InspectorSectionLayout>,
    pub(super) audio: Option<InspectorSectionLayout>,
    pub(super) model: Option<InspectorSectionLayout>,
    pub(super) end: f32,
}

pub(super) struct MediaInspectorSections<'a> {
    pub(super) general: &'a Accordion,
    pub(super) video: &'a Accordion,
    pub(super) audio: &'a Accordion,
    pub(super) model: &'a Accordion,
}

pub(super) fn media_inspector_layout(
    rect: Rect,
    asset: &crate::project::MediaAsset,
    selected_stream: MediaStream,
    sections: MediaInspectorSections<'_>,
    start: f32,
) -> MediaInspectorLayout {
    let MediaInspectorSections {
        general,
        video,
        audio,
        model,
    } = sections;
    let has_video_tracks = matches!(selected_stream, MediaStream::All | MediaStream::Video(_))
        && media_streams(asset)
            .iter()
            .any(|stream| matches!(stream, MediaStream::Video(_)));
    let has_audio_tracks = matches!(selected_stream, MediaStream::All | MediaStream::Audio(_))
        && media_streams(asset)
            .iter()
            .any(|stream| matches!(stream, MediaStream::Audio(_)));
    let specs = [
        (
            Some(
                media_general_detail_rows(asset).len() as f32 * ROW_H + INSPECTOR_SECTION_PAD * 2.0,
            ),
            general.open_amount(),
        ),
        (
            has_video_tracks.then(|| {
                media_video_detail_rows(asset, selected_stream).len() as f32 * ROW_H
                    + INSPECTOR_SECTION_PAD * 2.0
            }),
            video.open_amount(),
        ),
        (
            has_audio_tracks.then(|| {
                media_audio_detail_rows(asset, selected_stream).len() as f32 * ROW_H
                    + INSPECTOR_SECTION_PAD * 2.0
            }),
            audio.open_amount(),
        ),
        (
            (asset.kind == MediaKind::Model3d).then_some(ROW_H + INSPECTOR_SECTION_PAD * 2.0),
            model.open_amount(),
        ),
    ];
    let (sections, end) = inspector_layout_sections(rect, start, &specs);
    let mut sections = sections.into_iter();
    MediaInspectorLayout {
        general: sections
            .next()
            .expect("media general section slot")
            .expect("media general section"),
        video: sections.next().expect("media video section slot"),
        audio: sections.next().expect("media audio section slot"),
        model: sections.next().expect("media model section slot"),
        end,
    }
}

pub(super) fn build_media_detail_section(
    ctx: &mut kama_ui::BuildCtx,
    content: Rect,
    layout: InspectorSectionLayout,
    section: &Accordion,
    identity: (&str, &str, IconId),
    rows: Vec<(String, String)>,
) {
    let (title, id, chevron) = identity;
    accordion_header(ctx, section, content, layout.header, title, chevron);
    inspector_accordion_body(ctx, section, content, layout.header, layout.height, id);
    if section.is_visible() {
        let body = inspector_section_content(section, content, layout);
        ctx.with_clip(body, |ctx| {
            let row_rects = crate::ui_layout::column(
                body,
                &rows
                    .iter()
                    .map(|_| crate::ui_layout::Item::height(ROW_H))
                    .collect::<Vec<_>>(),
                0.0,
                0.0,
                kama_ui::Align::Start,
                None,
            );
            for (index, ((label, value), slot)) in rows.into_iter().zip(row_rects).enumerate() {
                let row = row_hit(body, slot.y);
                let label_rect = property_label_rect(row);
                ui_text!(
                    ctx,
                    ("media-detail-label", id, index),
                    label_rect,
                    9.5,
                    theme::text(),
                    &label
                );
                ui_text!(
                    ctx,
                    ("media-detail-value", id, index),
                    crate::ui_layout::row(
                        row,
                        &[
                            crate::ui_layout::Item::width(label_rect.width + 9.0),
                            crate::ui_layout::Item::fill(),
                            crate::ui_layout::Item::width(7.0),
                        ],
                        0.0,
                        0.0,
                        kama_ui::Align::Start,
                    )[1],
                    9.5,
                    theme::muted(),
                    &value
                );
            }
        });
    }
}

pub(super) fn build_media_inspector(
    ctx: &mut kama_ui::BuildCtx,
    rect: Rect,
    asset: &crate::project::MediaAsset,
    selected_stream: MediaStream,
    sections: (&Accordion, &Accordion, &Accordion, &Accordion),
    chevron: IconId,
    scroll_y: f32,
) -> f32 {
    let (general, video, audio, model) = sections;
    panel_title(
        ctx,
        ("inspector-media-title", asset.id),
        rect,
        &match selected_stream {
            MediaStream::All => format!("Media: {}", asset.name),
            MediaStream::Video(index) => {
                format!("Video track {}: {}", index + 1, asset.name)
            }
            MediaStream::Audio(index) => {
                format!("Audio track {}: {}", index + 1, asset.name)
            }
        },
        scroll_y,
    );
    let content = crate::ui_layout::scrolled_content(rect, scroll_y);
    let layout = media_inspector_layout(
        content,
        asset,
        selected_stream,
        MediaInspectorSections {
            general,
            video,
            audio,
            model,
        },
        rect.y + PANEL_HEADER_H - scroll_y,
    );
    build_media_detail_section(
        ctx,
        content,
        layout.general,
        general,
        ("General Information", "media-general", chevron),
        media_general_detail_rows(asset),
    );
    if let Some(section) = layout.video {
        let title = match selected_stream {
            MediaStream::Video(index) => format!("Video Track {}", index + 1),
            _ => "Video Tracks".to_string(),
        };
        build_media_detail_section(
            ctx,
            content,
            section,
            video,
            (&title, "media-video", chevron),
            media_video_detail_rows(asset, selected_stream),
        );
    }
    if let Some(section) = layout.audio {
        let title = match selected_stream {
            MediaStream::Audio(index) => format!("Audio Track {}", index + 1),
            _ => "Audio Tracks".to_string(),
        };
        build_media_detail_section(
            ctx,
            content,
            section,
            audio,
            (&title, "media-audio", chevron),
            media_audio_detail_rows(asset, selected_stream),
        );
    }
    if let Some(section) = layout.model {
        build_media_detail_section(
            ctx,
            content,
            section,
            model,
            ("3D Model", "media-model", chevron),
            vec![("Render controls".into(), "Clip properties".into())],
        );
    }
    (layout.end + scroll_y - rect.y + INSPECTOR_SECTION_PAD).max(0.0)
}

pub(super) fn media_general_detail_rows(
    asset: &crate::project::MediaAsset,
) -> Vec<(String, String)> {
    let mut rows = vec![
        ("Path".into(), asset.path.display().to_string()),
        ("Type".into(), media_kind_label(asset.kind).into()),
    ];
    if let MediaKind::Image { width, height } = asset.kind {
        rows.push(("Resolution".into(), format!("{width} × {height}")));
        rows.push((
            "Aspect ratio".into(),
            format_aspect_ratio(width.max(1), height.max(1)),
        ));
    }
    if let Some(duration) = asset.duration {
        rows.push(("Duration".into(), format_duration(duration)));
    }
    let streams = media_streams(asset);
    if !streams.is_empty() {
        let video_count = streams
            .iter()
            .filter(|stream| matches!(stream, MediaStream::Video(_)))
            .count();
        let audio_count = streams
            .iter()
            .filter(|stream| matches!(stream, MediaStream::Audio(_)))
            .count();
        rows.push(("Tracks".into(), streams.len().to_string()));
        if video_count > 0 {
            rows.push(("Video tracks".into(), video_count.to_string()));
        }
        if audio_count > 0 {
            rows.push(("Audio tracks".into(), audio_count.to_string()));
        }
        if !asset.tracks.is_empty() {
            let codecs = asset
                .tracks
                .iter()
                .map(|track| track.codec.as_str())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
                .join(", ");
            if !codecs.is_empty() {
                rows.push(("Codecs".into(), codecs));
            }
        }
    }
    rows
}

pub(super) fn media_video_detail_rows(
    asset: &crate::project::MediaAsset,
    selected_stream: MediaStream,
) -> Vec<(String, String)> {
    let video_tracks = asset
        .tracks
        .iter()
        .filter(|track| track.kind == crate::project::MediaTrackKind::Video)
        .enumerate()
        .filter(|(index, _)| match selected_stream {
            MediaStream::Video(selected) => *index == selected,
            _ => true,
        })
        .collect::<Vec<_>>();
    if video_tracks.is_empty() {
        let mut rows = vec![("Track 1 type".into(), "Video".into())];
        if let (Some(width), Some(height)) = (asset.video_width, asset.video_height) {
            rows.push(("Track 1 resolution".into(), format!("{width} × {height}")));
            rows.push((
                "Track 1 aspect ratio".into(),
                format_aspect_ratio(width.max(1), height.max(1)),
            ));
        }
        if let Some(rate) = asset.frame_rate {
            rows.push(("Track 1 frame rate".into(), format!("{rate:.3} fps")));
        }
        return rows;
    }

    let mut rows = Vec::new();
    for (index, track) in video_tracks {
        let prefix = format!("Track {}", index + 1);
        rows.push((format!("{prefix} codec"), track.codec.clone()));
        rows.push((
            format!("{prefix} stream"),
            format!("#{}", track.stream_index),
        ));
        if let (Some(width), Some(height)) = (track.width, track.height) {
            rows.push((
                format!("{prefix} resolution"),
                format!("{width} × {height}"),
            ));
            rows.push((
                format!("{prefix} aspect ratio"),
                format_aspect_ratio(width.max(1), height.max(1)),
            ));
        }
        if let Some(rate) = track.frame_rate {
            rows.push((format!("{prefix} frame rate"), format!("{rate:.3} fps")));
        }
        if let Some(bit_rate) = track.bit_rate {
            rows.push((format!("{prefix} bitrate"), format_bit_rate(bit_rate)));
        }
    }
    rows
}

pub(super) fn media_audio_detail_rows(
    asset: &crate::project::MediaAsset,
    selected_stream: MediaStream,
) -> Vec<(String, String)> {
    let audio_tracks = asset
        .tracks
        .iter()
        .filter(|track| track.kind == crate::project::MediaTrackKind::Audio)
        .enumerate()
        .filter(|(index, _)| match selected_stream {
            MediaStream::Audio(selected) => *index == selected,
            _ => true,
        })
        .collect::<Vec<_>>();
    let mut rows = Vec::new();
    if audio_tracks.is_empty() {
        rows.push(("Track 1 type".into(), "Audio".into()));
    } else {
        for (index, track) in audio_tracks {
            let prefix = format!("Track {}", index + 1);
            rows.push((format!("{prefix} codec"), track.codec.clone()));
            rows.push((
                format!("{prefix} stream"),
                format!("#{}", track.stream_index),
            ));
            if let Some(sample_rate) = track.sample_rate {
                rows.push((
                    format!("{prefix} sample rate"),
                    format!("{} Hz", sample_rate),
                ));
            }
            if let Some(channels) = track.channels {
                rows.push((format!("{prefix} channels"), channels.to_string()));
            }
            if let Some(bit_rate) = track.bit_rate {
                rows.push((format!("{prefix} bitrate"), format_bit_rate(bit_rate)));
            }
        }
    }
    if let Some(duration) = asset.duration {
        rows.push(("Duration".into(), format_duration(duration)));
    }
    rows
}

pub(super) fn format_bit_rate(bit_rate: u64) -> String {
    if bit_rate >= 1_000_000 {
        format!("{:.2} Mbps", bit_rate as f64 / 1_000_000.0)
    } else if bit_rate >= 1_000 {
        format!("{:.0} kbps", bit_rate as f64 / 1_000.0)
    } else {
        format!("{bit_rate} bps")
    }
}

pub(super) fn format_aspect_ratio(width: u32, height: u32) -> String {
    fn gcd(mut a: u32, mut b: u32) -> u32 {
        while b != 0 {
            let next = a % b;
            a = b;
            b = next;
        }
        a.max(1)
    }
    let divisor = gcd(width, height);
    format!("{}:{}", width / divisor, height / divisor)
}

pub(super) fn inspector_title(timeline: &TimelineState) -> Option<String> {
    if let Some(clip) = timeline.selected_clip() {
        return Some(format!("Clip: {}", clip.name));
    }
    timeline.selected_track().map(|track| {
        let kind = match track.kind {
            TrackKind::Video => "Video Track",
            TrackKind::Audio => "Audio Track",
            TrackKind::Effect => "Effect Track",
        };
        format!("{kind}: {}", track.name)
    })
}
