use std::{collections::HashSet, path::PathBuf};

use crate::{
    project::{CompositionId, MediaKind, Project, VisualSource},
    timeline::{Clip, Track, TrackKind},
};

use super::renderer::{nested_cache_scope, scaled_source_geometry, scoped_clip_id};

pub(super) const VIDEO_CLIP_PRELOAD_SECONDS: f32 = 3.0;
pub(super) const VIDEO_CLIP_PRELOAD_LIMIT: usize = 4;

#[derive(Debug)]
pub(super) struct VideoPreloadTarget {
    pub(super) clip_key: u64,
    pub(super) track_scope: Option<u64>,
    pub(super) track: u32,
    pub(super) path: PathBuf,
    pub(super) source_time: f64,
    pub(super) source_fps: f64,
    pub(super) source_step_seconds: f64,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) starts_in: f32,
}

#[allow(clippy::too_many_arguments)]
fn collect_upcoming_video_preloads(
    project: &Project,
    tracks: &[Track],
    clips: &[Clip],
    scope: Option<u64>,
    timeline_fps: f64,
    canvas_size: [u32; 2],
    preview_size: [u32; 2],
    window_start: f32,
    window_end: f32,
    root_eta_start: f32,
    root_seconds_per_timeline_second: f32,
    depth: usize,
    active_path: &mut HashSet<CompositionId>,
    targets: &mut Vec<VideoPreloadTarget>,
) {
    if depth >= 16 || window_end <= window_start {
        return;
    }

    let has_video_solo = tracks
        .iter()
        .any(|track| track.kind != TrackKind::Audio && track.solo);
    for clip in clips {
        let Some(track) = tracks.iter().find(|track| track.id == clip.track) else {
            continue;
        };
        if track.kind != TrackKind::Video || track.muted || (has_video_solo && !track.solo) {
            continue;
        }

        let overlap_start = window_start.max(clip.start);
        let overlap_end = window_end.min(clip.end());
        if overlap_end <= overlap_start {
            continue;
        }

        let source = track
            .property_row(&clip.source, clip.source_instance)
            .map(|row| &row.source)
            .unwrap_or(&clip.source);
        let clip_key = scope
            .map(|scope| scoped_clip_id(scope, clip.id))
            .unwrap_or_else(|| u64::from(clip.id));

        match source {
            VisualSource::Media(media) => {
                let preload_time = if clip.start >= window_start {
                    clip.start
                } else if root_eta_start > f32::EPSILON {
                    overlap_start
                } else {
                    continue;
                };
                let Some(asset) = project
                    .media(*media)
                    .filter(|asset| asset.kind == MediaKind::Video)
                else {
                    continue;
                };
                let mut source_time = clip.source_time(preload_time);
                if let Some(duration) = asset
                    .duration
                    .filter(|duration| duration.is_finite() && *duration > 1.0e-6)
                {
                    source_time = source_time.rem_euclid(duration);
                }
                let (target_width, target_height) = asset
                    .video_width
                    .zip(asset.video_height)
                    .map(|dimensions| {
                        scaled_source_geometry(
                            dimensions,
                            [0.0, 0.0],
                            canvas_size,
                            preview_size[0],
                            preview_size[1],
                        )
                        .size
                    })
                    .unwrap_or((preview_size[0].max(1), preview_size[1].max(1)));
                targets.push(VideoPreloadTarget {
                    clip_key,
                    track_scope: scope,
                    track: track.id,
                    path: asset.path.clone(),
                    source_time,
                    source_fps: asset.frame_rate.unwrap_or(timeline_fps).max(1.0),
                    source_step_seconds: clip.speed.max(0.01) as f64 / timeline_fps.max(1.0),
                    width: target_width,
                    height: target_height,
                    starts_in: root_eta_start
                        + (preload_time - window_start).max(0.0) * root_seconds_per_timeline_second,
                });
            }
            VisualSource::Composition(composition_id) => {
                let Some(composition) = project.composition(*composition_id) else {
                    continue;
                };
                if !active_path.insert(composition.id) {
                    continue;
                }

                let speed = clip.speed.max(0.01);
                let child_scope = nested_cache_scope(clip_key, composition.id);
                let duration = project
                    .composition_duration(composition.id)
                    .filter(|duration| duration.is_finite() && *duration > 1.0e-6);
                let mut parent_cursor = overlap_start;
                let mut segments = 0usize;
                while parent_cursor < overlap_end && segments < 32 {
                    segments += 1;
                    let source_time = clip.source_time(parent_cursor) as f32;
                    let (child_start, parent_segment_end) = if let Some(duration) = duration {
                        let child_start = source_time.rem_euclid(duration);
                        let until_wrap = ((duration - child_start).max(1.0e-6) / speed).max(1.0e-6);
                        (child_start, (parent_cursor + until_wrap).min(overlap_end))
                    } else {
                        (source_time, overlap_end)
                    };
                    let child_end = child_start + (parent_segment_end - parent_cursor) * speed;
                    let child_eta = root_eta_start
                        + (parent_cursor - window_start).max(0.0)
                            * root_seconds_per_timeline_second;
                    let child_preview = scaled_source_geometry(
                        (
                            composition.settings.canvas_size[0],
                            composition.settings.canvas_size[1],
                        ),
                        [0.0, 0.0],
                        canvas_size,
                        preview_size[0],
                        preview_size[1],
                    )
                    .size;
                    collect_upcoming_video_preloads(
                        project,
                        &composition.timeline.tracks,
                        &composition.timeline.clips,
                        Some(child_scope),
                        composition.settings.frame_rate,
                        composition.settings.canvas_size,
                        [child_preview.0, child_preview.1],
                        child_start,
                        child_end,
                        child_eta,
                        root_seconds_per_timeline_second / speed,
                        depth + 1,
                        active_path,
                        targets,
                    );
                    if parent_segment_end <= parent_cursor + 1.0e-6 {
                        break;
                    }
                    parent_cursor = parent_segment_end;
                }
                active_path.remove(&composition.id);
            }
            _ => {}
        }
    }
}

pub(super) fn upcoming_video_preloads(
    project: &Project,
    tracks: &[Track],
    clips: &[Clip],
    timeline_fps: f64,
    canvas_size: [u32; 2],
    preview_size: [u32; 2],
    playhead: f32,
) -> Vec<VideoPreloadTarget> {
    let mut targets = Vec::new();
    let mut active_path = HashSet::new();
    active_path.insert(project.active_composition);
    collect_upcoming_video_preloads(
        project,
        tracks,
        clips,
        None,
        timeline_fps,
        canvas_size,
        preview_size,
        playhead,
        playhead + VIDEO_CLIP_PRELOAD_SECONDS,
        0.0,
        1.0,
        0,
        &mut active_path,
        &mut targets,
    );
    targets.sort_unstable_by(|left, right| left.starts_in.total_cmp(&right.starts_in));
    targets
}
