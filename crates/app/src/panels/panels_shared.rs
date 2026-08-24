use kama_ui::{components::TextEdit, IconId, Rect, Size};

use super::{property_chrome, row_hit, value_row, InspectorState, KeyframeControl, ROW_H};
use crate::{
    theme,
    timeline::{TimelineState, TrackKind},
};

pub(super) fn property_row_parts(row: Rect) -> (Rect, Rect, Rect) {
    let parts = kama_ui::layout::row(
        row,
        &[
            kama_ui::layout::Item::width(6.0),
            kama_ui::layout::Item::fill_portion(0.38),
            kama_ui::layout::Item::width(3.0),
            kama_ui::layout::Item::new(
                Size::FillPortion(0.62),
                Size::Pixels((row.height - 4.0).max(1.0)),
            ),
            kama_ui::layout::Item::width(4.0),
            kama_ui::layout::Item::new(Size::Pixels(18.0), Size::Pixels(18.0)),
            kama_ui::layout::Item::width(4.0),
        ],
        0.0,
        0.0,
        kama_ui::Align::Center,
    );
    (parts[1], parts[3], parts[5])
}

pub(super) fn property_label_rect(row: Rect) -> Rect {
    let label = property_row_parts(row).0;
    kama_ui::layout::column(
        label,
        &[
            kama_ui::layout::Item::height(1.5),
            kama_ui::layout::Item::fill(),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
        None,
    )[1]
}

pub(super) fn plain_property_parts(row: Rect) -> (Rect, Rect) {
    let parts = kama_ui::layout::row(
        row,
        &[
            kama_ui::layout::Item::width(6.0),
            kama_ui::layout::Item::fill_portion(0.38),
            kama_ui::layout::Item::width(3.0),
            kama_ui::layout::Item::new(
                Size::FillPortion(0.62),
                Size::Pixels((row.height - 4.0).max(1.0)),
            ),
            kama_ui::layout::Item::width(4.0),
        ],
        0.0,
        0.0,
        kama_ui::Align::Center,
    );
    (parts[1], parts[3])
}

pub(super) fn property_control_rect(rect: Rect, y: f32) -> Rect {
    property_row_parts(row_hit(rect, y)).1
}

pub(super) fn editor_value_rect(rect: Rect, y: f32) -> Rect {
    property_control_rect(rect, y)
}

pub(super) fn editor_property_row(
    ctx: &mut kama_ui::BuildCtx,
    rect: Rect,
    y: f32,
    label: &str,
    editor: &mut TextEdit,
    placeholder: &str,
    keyframe: KeyframeControl,
) {
    let control = property_chrome(ctx, rect, y, label, "editor", Some(keyframe));
    editor.build(
        ctx,
        format!("inspector-{label}-{}", y.to_bits()),
        control,
        placeholder,
        crate::widgets::component_style(),
    );
}

pub(super) fn font_family_property_row(
    ctx: &mut kama_ui::BuildCtx,
    rect: Rect,
    y: f32,
    label: &str,
    family: &str,
    keyframe: KeyframeControl,
    chevron: IconId,
) {
    let control = property_chrome(ctx, rect, y, label, "font-family", Some(keyframe));
    let style = crate::widgets::component_style();
    kama_ui::ui!(ctx, {
        Row {
            id: @format("inspector-font-family-{label}-{}", y.to_bits());
            bounds: (control.x, control.y, control.width, control.height);
            padding: 0.0;
            align_items: kama_ui::Align::Center;
            fill: style.control;
            border: 1;
            border_color: style.border;
            border_radius: style.radius_sm;
            interactive;

            HSpacer { width: Size::Pixels(5.0); }
            Block {
                width: Size::Fill;
                height: Size::Fill;
                font_size: 11.0 * style.text_scale;
                text_color: style.text;
                text: family;
            }
            HSpacer { width: Size::Pixels(3.0); }
            Icon {
                icon!: chevron;
                color!: style.muted;
                texture_rotation: std::f32::consts::FRAC_PI_2;
                width: Size::Pixels(16.0);
                height: Size::Pixels(16.0);
            }
            HSpacer { width: Size::Pixels(5.0); }
        }
    });
}

fn selection_summary_row_count(timeline: &TimelineState) -> usize {
    if timeline.selected_clip().is_some() {
        2
    } else if timeline.selected_track().is_some() {
        3
    } else {
        0
    }
}

pub(super) fn selection_summary_rects(
    rect: Rect,
    y: f32,
    timeline: &TimelineState,
) -> (Rect, Vec<Rect>) {
    let count = selection_summary_row_count(timeline);
    if count == 0 {
        return (Rect::new(rect.x, y, rect.width, 0.0), Vec::new());
    }
    let mut items = vec![kama_ui::layout::Item::height(ROW_H); count];
    items.push(kama_ui::layout::Item::height(4.0));
    let (root, mut rows) =
        kama_ui::layout::fit_column_at(rect, [rect.x, y], rect.width, &items, 0.0, 0.0);
    rows.truncate(count);
    (root, rows)
}

pub(super) fn selection_summary_value_rect(
    rect: Rect,
    y: f32,
    timeline: &TimelineState,
    index: usize,
) -> Rect {
    let (_, rows) = selection_summary_rects(rect, y, timeline);
    rows.get(index)
        .copied()
        .map(|row| row_hit(rect, row.y))
        .map(plain_property_parts)
        .map(|(_, control)| control)
        .unwrap_or_default()
}

impl InspectorState {
    pub(super) fn build_selection_summary(
        &mut self,
        ctx: &mut kama_ui::BuildCtx,
        rect: Rect,
        timeline: &TimelineState,
        y: f32,
    ) -> f32 {
        let (summary, rows) = selection_summary_rects(rect, y, timeline);
        if timeline.selected_clip().is_some() {
            for ((index, (label, editor)), row) in
                [("Start", &mut self.clip_start), ("End", &mut self.clip_end)]
                    .into_iter()
                    .enumerate()
                    .zip(rows)
            {
                let row = row_hit(rect, row.y);
                let (label_rect, control) = plain_property_parts(row);
                ui_text!(
                    ctx,
                    ("selection-summary-label", label, index),
                    label_rect,
                    9.5,
                    theme::text(),
                    label
                );
                editor.build(
                    ctx,
                    format!("selection-summary-{label}-{index}"),
                    control,
                    "00:00:00:00",
                    crate::widgets::component_style(),
                );
            }
            summary.bottom()
        } else if let Some(track) = timeline.selected_track() {
            let kind = match track.kind {
                TrackKind::Video => "Video",
                TrackKind::Audio => "Audio",
                TrackKind::Effect => "Effect",
            };
            let clip_count = timeline
                .clips()
                .iter()
                .filter(|clip| clip.track == track.id)
                .count()
                .to_string();
            let state = match (track.muted, track.solo) {
                (true, true) => "Muted + Solo",
                (true, false) => "Muted",
                (false, true) => "Solo",
                (false, false) => "Active",
            };
            for ((label, value), row) in [
                ("Type", kind.to_string()),
                ("Clips", clip_count),
                ("State", state.to_string()),
            ]
            .into_iter()
            .zip(rows)
            {
                value_row(ctx, rect, row.y, label, &value);
            }
            summary.bottom()
        } else {
            y
        }
    }
}
