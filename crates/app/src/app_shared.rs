use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
};

use kama_ui::dock::{Axis, DockLayoutSpec, DockState, DropZone, Rect};

use crate::{
    model3d,
    project::{MediaId, Project},
    runtime,
    timeline::{MediaDropPreviewSpec, TimelineState},
    PanelKind, DOCK_EDGE,
};

#[derive(Debug)]
pub(crate) struct BoundedCache<K, V> {
    entries: VecDeque<(K, V)>,
}

impl<K, V> Default for BoundedCache<K, V> {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
        }
    }
}

impl<K: Eq, V> BoundedCache<K, V> {
    pub(crate) fn get(&mut self, key: &K) -> Option<&V> {
        let index = self
            .entries
            .iter()
            .position(|(candidate, _)| candidate == key)?;
        let hit = self.entries.remove(index)?;
        self.entries.push_front(hit);
        self.entries.front().map(|(_, value)| value)
    }

    pub(crate) fn contains(&self, key: &K) -> bool {
        self.entries.iter().any(|(candidate, _)| candidate == key)
    }

    pub(crate) fn insert(&mut self, key: K, value: V) {
        if let Some(index) = self
            .entries
            .iter()
            .position(|(candidate, _)| candidate == &key)
        {
            self.entries.remove(index);
        }
        self.entries.push_front((key, value));
    }

    pub(crate) fn trim(
        &mut self,
        capacity: usize,
        max_weight: usize,
        weight: impl Fn(&V) -> usize,
    ) {
        while self.entries.len() > capacity.max(1)
            || (self.entries.len() > 1
                && self
                    .entries
                    .iter()
                    .map(|(_, value)| weight(value))
                    .sum::<usize>()
                    > max_weight.max(1))
        {
            self.entries.pop_back();
        }
    }

    pub(crate) fn latest(&self) -> Option<&V> {
        self.entries.front().map(|(_, value)| value)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &(K, V)> {
        self.entries.iter()
    }
}

pub(super) fn default_dock() -> DockState {
    let top = DockLayoutSpec::split(
        Axis::Horizontal,
        0.20,
        DockLayoutSpec::stack(PanelKind::Media.layout_title()),
        DockLayoutSpec::split(
            Axis::Horizontal,
            0.75,
            DockLayoutSpec::stack(PanelKind::Monitor.layout_title()),
            DockLayoutSpec::Stack(vec![
                PanelKind::Inspector.layout_title().into(),
                PanelKind::Render.layout_title().into(),
                PanelKind::History.layout_title().into(),
            ]),
        ),
    );
    DockState::from_spec(DockLayoutSpec::split(
        Axis::Vertical,
        0.60,
        top,
        DockLayoutSpec::split(
            Axis::Horizontal,
            0.86,
            DockLayoutSpec::Stack(vec![
                PanelKind::Timeline.layout_title().into(),
                PanelKind::Pipeline.layout_title().into(),
                PanelKind::Messages.layout_title().into(),
            ]),
            DockLayoutSpec::stack(PanelKind::Meters.layout_title()),
        ),
    ))
}

pub(super) fn missing_project_media(project: &Project) -> Vec<(MediaId, PathBuf)> {
    project
        .media
        .iter()
        .filter(|asset| !asset.path.exists())
        .map(|asset| (asset.id, asset.path.clone()))
        .collect()
}

pub(super) fn startup_project_path() -> Option<PathBuf> {
    std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("kama"))
        })
}

pub(super) fn document_content_signature(project: &Project, timeline: &TimelineState) -> Vec<u8> {
    let mut document = project.clone();
    document.sync_active_timeline(timeline.document());
    for composition in &mut document.compositions {
        composition.timeline.view = Default::default();
    }
    document.authored_signature()
}

pub(super) fn document_view_signature(project: &Project, timeline: &TimelineState) -> Vec<u8> {
    let active = project.active_composition;
    let active_view = timeline.document().view;
    let views = project
        .compositions
        .iter()
        .map(|composition| {
            let mut view = if composition.id == active {
                active_view
            } else {
                composition.timeline.view
            };

            view.playhead = 0.0;
            (composition.id, view)
        })
        .collect::<Vec<_>>();
    serde_json::to_vec(&views).unwrap_or_default()
}

pub(super) fn external_media_preview_spec(path: &Path) -> Option<MediaDropPreviewSpec> {
    if !path.is_file() {
        return None;
    }
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if extension == "kama" {
        return None;
    }
    if extension == "wasm"
        || model3d::is_supported_path(path)
        || image::image_dimensions(path).is_ok()
    {
        return Some(MediaDropPreviewSpec {
            video_tracks: 1,
            audio_tracks: 0,
            duration: 5.0,
        });
    }
    if let Ok(probe) = runtime::media::probe_av_media(path) {
        if probe.has_video || probe.has_audio {
            let video_tracks = probe
                .tracks
                .iter()
                .filter(|track| track.kind == crate::project::MediaTrackKind::Video)
                .count();
            let audio_tracks = probe
                .tracks
                .iter()
                .filter(|track| track.kind == crate::project::MediaTrackKind::Audio)
                .count();
            return Some(MediaDropPreviewSpec {
                video_tracks: video_tracks.max(usize::from(probe.has_video)),
                audio_tracks: audio_tracks.max(usize::from(probe.has_audio)),
                duration: probe.duration.unwrap_or(5.0).clamp(0.1, 24.0 * 60.0 * 60.0) as f32,
            });
        }
    }
    let audio = matches!(
        extension.as_str(),
        "wav" | "mp3" | "flac" | "aac" | "ogg" | "m4a"
    );
    Some(MediaDropPreviewSpec {
        video_tracks: usize::from(!audio),
        audio_tracks: usize::from(audio),
        duration: 5.0,
    })
}

pub(super) fn sanitize_file_name(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|character| {
            if matches!(
                character,
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
            ) {
                '_'
            } else {
                character
            }
        })
        .collect();
    let sanitized = sanitized.trim();
    if sanitized.is_empty() {
        "Untitled".into()
    } else {
        sanitized.into()
    }
}

pub(super) fn edge_drop(rect: Rect, point: [f32; 2]) -> Option<(DropZone, Rect)> {
    let edges = [
        ((point[0] - rect.x).abs(), DropZone::Left),
        ((rect.right() - point[0]).abs(), DropZone::Right),
        ((point[1] - rect.y).abs(), DropZone::Top),
        ((rect.bottom() - point[1]).abs(), DropZone::Bottom),
    ];
    let (distance, zone) = edges.into_iter().min_by(|a, b| a.0.total_cmp(&b.0))?;
    if distance > DOCK_EDGE {
        return None;
    }
    let mut preview = rect;
    match zone {
        DropZone::Left => preview.width *= 0.28,
        DropZone::Right => {
            preview.x += preview.width * 0.72;
            preview.width *= 0.28;
        }
        DropZone::Top => preview.height *= 0.28,
        DropZone::Bottom => {
            preview.y += preview.height * 0.72;
            preview.height *= 0.28;
        }
        DropZone::Center => unreachable!(),
    }
    Some((zone, preview.inset(4.0)))
}
