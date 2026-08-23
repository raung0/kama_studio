use kama_ui::{components::TextEdit, IconId, Rect, Size};

use super::{property_chrome, row_hit, value_row, InspectorState, KeyframeControl, ROW_H};
use crate::{
    theme,
    timeline::{TimelineState, TrackKind},
};

pub(super) fn property_row_parts(row: Rect) -> (Rect, Rect, Rect) {
    let parts = crate::ui_layout::row(
        row,
        &[
            crate::ui_layout::Item::width(6.0),
            crate::ui_layout::Item::width(row.width * 0.38),
            crate::ui_layout::Item::width(3.0),
            crate::ui_layout::Item::new(Size::Fill, Size::Pixels((row.height - 4.0).max(1.0))),
            crate::ui_layout::Item::width(4.0),
            crate::ui_layout::Item::new(Size::Pixels(18.0), Size::Pixels(18.0)),
            crate::ui_layout::Item::width(4.0),
        ],
        0.0,
        0.0,
        kama_ui::Align::Center,
    );
    (parts[1], parts[3], parts[5])
}

pub(super) fn property_label_rect(row: Rect) -> Rect {
    let mut rect = property_row_parts(row).0;
    rect.y += 1.5;
    rect.height = (rect.height - 1.5).max(0.0);
    rect
}

pub(super) fn plain_property_parts(row: Rect) -> (Rect, Rect) {
    let parts = crate::ui_layout::row(
        row,
        &[
            crate::ui_layout::Item::width(6.0),
            crate::ui_layout::Item::width(row.width * 0.38),
            crate::ui_layout::Item::width(3.0),
            crate::ui_layout::Item::new(Size::Fill, Size::Pixels((row.height - 4.0).max(1.0))),
            crate::ui_layout::Item::width(4.0),
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

pub(super) fn selection_summary_height(timeline: &TimelineState) -> f32 {
    if timeline.selected_clip().is_some() {
        ROW_H * 2.0 + 4.0
    } else if timeline.selected_track().is_some() {
        ROW_H * 3.0 + 4.0
    } else {
        0.0
    }
}

pub(super) fn selection_summary_value_rect(rect: Rect, y: f32, index: usize) -> Rect {
    let row = row_hit(rect, y + ROW_H * index as f32);
    plain_property_parts(row).1
}

impl InspectorState {
    pub(super) fn build_selection_summary(
        &mut self,
        ctx: &mut kama_ui::BuildCtx,
        rect: Rect,
        timeline: &TimelineState,
        y: f32,
    ) -> f32 {
        if timeline.selected_clip().is_some() {
            for (index, (label, editor)) in
                [("Start", &mut self.clip_start), ("End", &mut self.clip_end)]
                    .into_iter()
                    .enumerate()
            {
                let row_y = y + ROW_H * index as f32;
                let row = row_hit(rect, row_y);
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
            y + ROW_H * 2.0 + 4.0
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
            for ((label, value), index) in [
                ("Type", kind.to_string()),
                ("Clips", clip_count),
                ("State", state.to_string()),
            ]
            .into_iter()
            .zip(0usize..)
            {
                value_row(ctx, rect, y + ROW_H * index as f32, label, &value);
            }
            y + ROW_H * 3.0 + 4.0
        } else {
            y
        }
    }
}
