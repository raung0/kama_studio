use kama_ui::components::ProgressBar;

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn build_active_modal(
    ctx: &mut ui::BuildCtx,
    width: f32,
    height: f32,
    modal: &mut Modal,
    registry: &CommandRegistry,
    render_phase: RenderPhase,
    about_logos: AboutLogos,
    icons: Icons,
) {
    match modal {
        Modal::About(dialog) => build_about_dialog(ctx, width, height, about_logos, dialog),
        Modal::LayoutSave(dialog) => build_layout_save_dialog(ctx, width, height, dialog),
        Modal::Discard(dialog) => build_discard_dialog(ctx, width, height, dialog),
        Modal::Busy(dialog) => build_busy_project_dialog(ctx, width, height, dialog, render_phase),
        Modal::Settings(dialog) => dialog.build(ctx, width, height, icons.get(AppIcon::Chevron)),
        Modal::Keybinds(dialog) => dialog.build(ctx, width, height, registry),
        Modal::Composition(dialog) => build_new_composition_dialog(ctx, width, height, dialog),
        Modal::SpeedDuration(dialog) => build_speed_duration_dialog(ctx, width, height, dialog),
        Modal::MissingMedia(dialog) => build_missing_media_dialog(ctx, width, height, dialog),
        Modal::Update(dialog) => build_update_dialog(ctx, width, height, dialog),
    }
}

pub(super) fn centered_dialog_rect(
    viewport_width: f32,
    viewport_height: f32,
    width: f32,
    height: f32,
) -> Rect {
    let viewport = Rect::new(0.0, 0.0, viewport_width, viewport_height);
    let centered = kama_ui::layout::centered(viewport, width, height);
    kama_ui::layout::fit_column_at(
        viewport,
        [centered.x.max(8.0), centered.y.max(8.0)],
        width,
        &[kama_ui::layout::Item::height(height)],
        0.0,
        0.0,
    )
    .1[0]
}

fn dialog_content_row(row: Rect) -> Rect {
    kama_ui::layout::row(
        row,
        &[
            kama_ui::layout::Item::width(14.0),
            kama_ui::layout::Item::fill(),
            kama_ui::layout::Item::width(14.0),
        ],
        0.0,
        0.0,
        ui::Align::Start,
    )[1]
}

#[derive(Clone, Copy)]
pub(super) enum ModalButtonRole {
    Secondary,
    Primary,
    Danger,
}

const ABOUT_DIALOG_W: f32 = 400.0;
const ABOUT_LOGO_H: f32 = 124.0;
const ABOUT_TEXT_H: f32 = 18.0;
const ABOUT_CONTENT_GAP: f32 = 2.0;

fn build_about_column(
    ctx: &mut ui::BuildCtx,
    logo: Option<ui::TextureId>,
    width: ui::Size,
    centered: bool,
) -> (ui::BlockId, ui::BlockId) {
    let mut button_id = ui::BlockId(0);
    let mut column = ctx
        .new()
        .id("about-kama-content")
        .width(width)
        .height(ui::Size::Fit)
        .column()
        .gap(ABOUT_CONTENT_GAP)
        .align_items(ui::Align::Center);
    if centered {
        column = column.overlay().centered();
    }
    let content_id = column
        .children(|ctx| {
            let _ = ctx
                .new()
                .width(ui::Size::Fill)
                .height(ui::Size::Pixels(30.0))
                .build();

            let mut logo_block = ctx
                .new()
                .width(ui::Size::Pixels(280.0))
                .height(ui::Size::Pixels(ABOUT_LOGO_H))
                .texture_mode(ui::TEXTURE_MODE_CONTAIN);
            if let Some(logo) = logo {
                logo_block = logo_block.fill_texture(logo);
            }
            let _ = logo_block.build();

            let _ = ctx
                .new()
                .width(ui::Size::Fit)
                .height(ui::Size::Fill)
                .build();

            let _ = ctx
                .new()
                .width(ui::Size::Fill)
                .height(ui::Size::Pixels(ABOUT_TEXT_H))
                .font_size(11.0)
                .text_color(theme::popup_text())
                .text_centered()
                .text(format!("Version {}", version::VERSION))
                .build();
            let _ = ctx
                .new()
                .width(ui::Size::Fill)
                .height(ui::Size::Pixels(ABOUT_TEXT_H))
                .font_size(11.0)
                .text_color(theme::popup_text())
                .text_centered()
                .text("Licensed under the GNU General Public License v3.0")
                .build();

            let _ = ctx
                .new()
                .width(ui::Size::Fill)
                .height(ui::Size::Pixels(24.0))
                .row()
                .children(|ctx| {
                    let _ = ctx
                        .new()
                        .width(ui::Size::Fill)
                        .height(ui::Size::Fill)
                        .build();
                    button_id = ctx
                        .new()
                        .id("about-kama-close")
                        .width(ui::Size::Pixels(70.0))
                        .height(ui::Size::Pixels(24.0))
                        .fill(theme::accent())
                        .border(1)
                        .border_color(theme::line_soft())
                        .border_radius(RADIUS_SM)
                        .font_size(10.0)
                        .text_color(theme::accent_text())
                        .text_centered()
                        .text("OK")
                        .interactive()
                        .build();
                    let _ = ctx
                        .new()
                        .width(ui::Size::Pixels(12.0))
                        .height(ui::Size::Fill)
                        .build();
                })
                .build();

            let _ = ctx
                .new()
                .width(ui::Size::Fill)
                .height(ui::Size::Pixels(12.0))
                .build();
        })
        .build();
    (content_id, button_id)
}

fn about_dialog_rects(viewport: Rect) -> (Rect, Rect) {
    let ((content, button), measured) = ui::measure_layout(viewport, |ctx| {
        build_about_column(ctx, None, ui::Size::Pixels(ABOUT_DIALOG_W), true)
    });
    (
        measured.rect(content).expect("about content layout"),
        measured.rect(button).expect("about button layout"),
    )
}

pub(super) fn about_dialog_rect(viewport_width: f32, viewport_height: f32) -> Rect {
    about_dialog_rects(Rect::new(0.0, 0.0, viewport_width, viewport_height)).0
}

pub(super) fn about_dialog_button_rect(dialog: Rect) -> Rect {
    about_dialog_rects(dialog).1
}

pub(super) fn build_about_dialog(
    ctx: &mut ui::BuildCtx,
    viewport_width: f32,
    viewport_height: f32,
    logos: AboutLogos,
    dialog: &SimpleDialog,
) {
    let viewport = Rect::new(0.0, 0.0, viewport_width, viewport_height);
    let rect = about_dialog_rect(viewport_width, viewport_height);
    let logo = match theme::effective_theme() {
        theme::ThemePreset::Light => logos.light,
        theme::ThemePreset::Dark => logos.dark,
        theme::ThemePreset::System => unreachable!("effective theme is always light or dark"),
    };
    build_modal(
        ctx,
        "about-kama-scrim",
        "about-kama-dialog",
        (viewport, rect),
        dialog.opacity(Instant::now()),
        |ctx| {
            build_about_column(ctx, Some(logo), Size::Fill, false);
        },
    );
}

pub(super) fn build_modal<K1: Hash, K2: Hash, F: FnOnce(&mut ui::BuildCtx)>(
    ctx: &mut ui::BuildCtx,
    scrim_key: K1,
    card_key: K2,
    bounds: (Rect, Rect),
    opacity: f32,
    children: F,
) {
    let (viewport, rect) = bounds;
    dialog::build_shell(ctx, scrim_key, card_key, viewport, rect, opacity, children);
}

pub(super) fn build_modal_title<K: Hash>(ctx: &mut ui::BuildCtx, key: K, rect: Rect, text: &str) {
    ui_text!(ctx, key, rect, 12.0, theme::popup_text(), text);
}

pub(super) fn build_modal_button<K: Hash>(
    ctx: &mut ui::BuildCtx,
    key: K,
    rect: Rect,
    label: &str,
    role: ModalButtonRole,
) {
    let (fill, border, text) = match role {
        ModalButtonRole::Secondary => (theme::control(), theme::line_soft(), theme::popup_text()),
        ModalButtonRole::Primary => (theme::accent(), theme::line_soft(), theme::accent_text()),
        ModalButtonRole::Danger => (
            Color::rgb8(0x9d, 0x3d, 0x34),
            Color::rgb8(0xd0, 0x66, 0x59),
            Color::WHITE,
        ),
    };
    ui::ui!(ctx, {
        Rect(key, rect) {
            fill: fill; border: 1; border_color: border; border_radius: RADIUS_SM;
            font_size: 10.0; text_color: text; text_centered; text: label; interactive;
        }
    });
}

#[derive(Clone, Copy)]
pub(super) struct ActionModalSpec {
    id: &'static str,
    title_key: &'static str,
    size: [f32; 2],
    description_height: f32,
    primary_label_key: &'static str,
    primary_width: f32,
    secondary_offset: f32,
}

const DISCARD_MODAL: ActionModalSpec = ActionModalSpec {
    id: "discard",
    title_key: "dialog-discard-title",
    size: [390.0, 132.0],
    description_height: 38.0,
    primary_label_key: "dialog-discard",
    primary_width: 80.0,
    secondary_offset: 174.0,
};
const BUSY_MODAL: ActionModalSpec = ActionModalSpec {
    id: "busy-project",
    title_key: "dialog-render-in-progress",
    size: [390.0, 128.0],
    description_height: 44.0,
    primary_label_key: "dialog-stop-and-continue",
    primary_width: 114.0,
    secondary_offset: 206.0,
};

pub(super) fn action_dialog_rect(
    viewport_width: f32,
    viewport_height: f32,
    spec: ActionModalSpec,
) -> Rect {
    centered_dialog_rect(viewport_width, viewport_height, spec.size[0], spec.size[1])
}
pub(super) fn action_dialog_parts(dialog: Rect, spec: ActionModalSpec) -> (Rect, Rect, [Rect; 2]) {
    let rows = kama_ui::layout::column(
        dialog,
        &[
            kama_ui::layout::Item::height(10.0),
            kama_ui::layout::Item::height(20.0),
            kama_ui::layout::Item::height(8.0),
            kama_ui::layout::Item::height(spec.description_height),
            kama_ui::layout::Item::fill(),
            kama_ui::layout::Item::height(24.0),
            kama_ui::layout::Item::height(10.0),
        ],
        0.0,
        0.0,
        ui::Align::Start,
        None,
    );
    let title = dialog_content_row(rows[1]);
    let description = dialog_content_row(rows[3]);
    let gap = (spec.secondary_offset - spec.primary_width - 82.0).max(0.0);
    let buttons = kama_ui::layout::row(
        rows[5],
        &[
            kama_ui::layout::Item::fill(),
            kama_ui::layout::Item::width(70.0),
            kama_ui::layout::Item::width(gap),
            kama_ui::layout::Item::width(spec.primary_width),
            kama_ui::layout::Item::width(12.0),
        ],
        0.0,
        0.0,
        ui::Align::Start,
    );
    (title, description, [buttons[1], buttons[3]])
}

pub(super) fn action_button_rect(dialog: Rect, primary: bool, spec: ActionModalSpec) -> Rect {
    action_dialog_parts(dialog, spec).2[primary as usize]
}
pub(super) fn discard_dialog_rect(width: f32, height: f32) -> Rect {
    action_dialog_rect(width, height, DISCARD_MODAL)
}
pub(super) fn discard_button_rect(dialog: Rect, primary: bool) -> Rect {
    action_button_rect(dialog, primary, DISCARD_MODAL)
}
pub(super) fn busy_project_dialog_rect(width: f32, height: f32) -> Rect {
    action_dialog_rect(width, height, BUSY_MODAL)
}
pub(super) fn busy_project_button_rect(dialog: Rect, primary: bool) -> Rect {
    action_button_rect(dialog, primary, BUSY_MODAL)
}

pub(super) fn build_action_dialog(
    ctx: &mut ui::BuildCtx,
    viewport_width: f32,
    viewport_height: f32,
    dialog: &ActionDialog,
    spec: ActionModalSpec,
    description: impl Into<String>,
) {
    let opacity = dialog.opacity(Instant::now());
    let viewport = Rect::new(0.0, 0.0, viewport_width, viewport_height);
    let rect = action_dialog_rect(viewport_width, viewport_height, spec);
    let local = Rect::new(0.0, 0.0, rect.width, rect.height);
    let (title_rect, description_rect, _) = action_dialog_parts(local, spec);
    build_modal(
        ctx,
        (spec.id, "scrim"),
        (spec.id, "dialog"),
        (viewport, rect),
        opacity,
        |ctx| {
            build_modal_title(
                ctx,
                (spec.id, "title"),
                title_rect,
                &i18n::text(spec.title_key),
            );
            ui::ui!(ctx, {
                Rect((spec.id, "description"), description_rect) {
                    font_size: 10.5; text_color: theme::popup_muted(); text: description.into();
                }
            });
            for (primary, label) in [
                (false, i18n::text("dialog-cancel")),
                (true, i18n::text(spec.primary_label_key)),
            ] {
                build_modal_button(
                    ctx,
                    (spec.id, "button", primary),
                    action_button_rect(local, primary, spec),
                    &label,
                    if primary {
                        ModalButtonRole::Danger
                    } else {
                        ModalButtonRole::Secondary
                    },
                );
            }
        },
    );
}

pub(super) fn build_discard_dialog(
    ctx: &mut ui::BuildCtx,
    viewport_width: f32,
    viewport_height: f32,
    dialog: &ActionDialog,
) {
    build_action_dialog(
        ctx,
        viewport_width,
        viewport_height,
        dialog,
        DISCARD_MODAL,
        "Current project has unsaved changes. This cannot be undone.",
    );
}

pub(super) fn build_busy_project_dialog(
    ctx: &mut ui::BuildCtx,
    viewport_width: f32,
    viewport_height: f32,
    dialog: &ActionDialog,
    phase: RenderPhase,
) {
    let activity = if phase == RenderPhase::Transcoding {
        "The project is currently transcoding the final output."
    } else {
        "The project is currently rendering or maintaining its render cache."
    };
    build_action_dialog(
        ctx,
        viewport_width,
        viewport_height,
        dialog,
        BUSY_MODAL,
        format!(
            "{activity} Continuing will stop that job before closing or loading another project."
        ),
    );
}

const MISSING_MEDIA_DIALOG_W: f32 = 640.0;
const MISSING_MEDIA_ROW_H: f32 = 34.0;
const MISSING_MEDIA_ROW_GAP: f32 = 4.0;
const MISSING_MEDIA_LIST_MAX_H: f32 = 238.0;
const MISSING_MEDIA_NAME_W: f32 = 190.0;
const MISSING_MEDIA_COLUMN_GAP: f32 = 8.0;

fn missing_media_list_content_height(missing: usize) -> f32 {
    if missing == 0 {
        0.0
    } else {
        missing as f32 * MISSING_MEDIA_ROW_H
            + missing.saturating_sub(1) as f32 * MISSING_MEDIA_ROW_GAP
    }
}

pub(super) fn missing_media_dialog_rect(
    viewport_width: f32,
    viewport_height: f32,
    missing: usize,
) -> Rect {
    let list_height = missing_media_list_content_height(missing).min(MISSING_MEDIA_LIST_MAX_H);
    let desired_height = 122.0 + list_height.max(MISSING_MEDIA_ROW_H);
    let max_height = (viewport_height * 0.8).max(180.0);
    centered_dialog_rect(
        viewport_width,
        viewport_height,
        MISSING_MEDIA_DIALOG_W.min((viewport_width - 16.0).max(320.0)),
        desired_height.min(max_height),
    )
}

pub(super) fn missing_media_dialog_parts(dialog: Rect) -> (Rect, Rect, Rect, [Rect; 2]) {
    let rows = kama_ui::layout::column(
        dialog,
        &[
            kama_ui::layout::Item::height(12.0),
            kama_ui::layout::Item::height(20.0),
            kama_ui::layout::Item::height(6.0),
            kama_ui::layout::Item::height(30.0),
            kama_ui::layout::Item::height(8.0),
            kama_ui::layout::Item::fill(),
            kama_ui::layout::Item::height(10.0),
            kama_ui::layout::Item::height(24.0),
            kama_ui::layout::Item::height(12.0),
        ],
        0.0,
        0.0,
        ui::Align::Start,
        None,
    );
    let title = dialog_content_row(rows[1]);
    let message = dialog_content_row(rows[3]);
    let list = dialog_content_row(rows[5]);
    let buttons = kama_ui::layout::row(
        rows[7],
        &[
            kama_ui::layout::Item::fill(),
            kama_ui::layout::Item::width(70.0),
            kama_ui::layout::Item::width(8.0),
            kama_ui::layout::Item::width(96.0),
            kama_ui::layout::Item::width(12.0),
        ],
        0.0,
        0.0,
        ui::Align::Start,
    );
    (title, message, list, [buttons[1], buttons[3]])
}

pub(super) fn missing_media_button_rect(dialog: Rect, confirm: bool) -> Rect {
    missing_media_dialog_parts(dialog).3[confirm as usize]
}

pub(super) fn missing_media_max_scroll(dialog: Rect, missing: usize) -> f32 {
    let list = missing_media_dialog_parts(dialog).2;
    (missing_media_list_content_height(missing) - list.height).max(0.0)
}

pub(super) fn missing_media_picker_rect(dialog: Rect, index: usize, scroll: f32) -> Rect {
    let list = missing_media_dialog_parts(dialog).2;
    let row = Rect::new(
        list.x,
        list.y + index as f32 * (MISSING_MEDIA_ROW_H + MISSING_MEDIA_ROW_GAP) - scroll,
        list.width,
        MISSING_MEDIA_ROW_H,
    );
    kama_ui::layout::row(
        row,
        &[
            kama_ui::layout::Item::width(MISSING_MEDIA_NAME_W),
            kama_ui::layout::Item::width(MISSING_MEDIA_COLUMN_GAP),
            kama_ui::layout::Item::fill(),
        ],
        0.0,
        0.0,
        ui::Align::Start,
    )[2]
}

pub(super) fn build_missing_media_dialog(
    ctx: &mut ui::BuildCtx,
    viewport_width: f32,
    viewport_height: f32,
    dialog: &MissingMediaDialog,
) {
    let viewport = Rect::new(0.0, 0.0, viewport_width, viewport_height);
    let rect = missing_media_dialog_rect(viewport_width, viewport_height, dialog.missing().len());
    let local = Rect::new(0.0, 0.0, rect.width, rect.height);
    let (title_rect, message_rect, list_rect, _) = missing_media_dialog_parts(local);
    let message = if dialog.is_project_load() {
        "Choose replacement paths for missing media. Apply replacements until every item is resolved, or cancel opening the project."
    } else {
        "Some media files are no longer available. This dialog will stay open and update until every missing item is resolved."
    };
    build_modal(
        ctx,
        "missing-media-scrim",
        "missing-media-dialog",
        (viewport, rect),
        dialog.opacity(Instant::now()),
        |ctx| {
            build_modal_title(
                ctx,
                "missing-media-title",
                title_rect,
                &i18n::text("dialog-missing-media"),
            );
            ui::ui!(ctx, {
                Rect("missing-media-message", message_rect) {
                    font_size: 10.5;
                    text_color: theme::popup_muted();
                    text: message;
                }

                Column {
                    id: "missing-media-list";
                    bounds: (list_rect.x, list_rect.y, list_rect.width, list_rect.height);
                    gap: MISSING_MEDIA_ROW_GAP;
                    vertical_scroll: dialog.scroll;
                    clip_children: true;

                    @for (index, entry) in dialog.missing().iter().enumerate() {
                        @let picker_text = entry.replacement.as_ref().map_or_else(
                            || entry.missing_path.display().to_string(),
                            |path| path.display().to_string(),
                        );
                        Row {
                            id: @format("missing-media-row-{index}");
                            width: Size::Fill;
                            height: Size::Pixels(MISSING_MEDIA_ROW_H);
                            gap: MISSING_MEDIA_COLUMN_GAP;

                            Block {
                                id: @format("missing-media-name-{index}");
                                width: Size::Pixels(MISSING_MEDIA_NAME_W);
                                height: Size::Fill;
                                padding: 7.0;
                                fill: theme::control();
                                border: 1;
                                border_color: theme::line_soft();
                                border_radius: RADIUS_SM;
                                font_size: 10.0;
                                text_color: theme::popup_text();
                                text: &entry.name;
                                tooltip: entry.missing_path.display().to_string();
                            }

                            Row {
                                id: @format("missing-media-picker-{index}");
                                width: Size::Fill;
                                height: Size::Fill;
                                padding: 7.0;
                                gap: 6.0;
                                fill: theme::control();
                                border: 1;
                                border_color: if entry.replacement.is_some() {
                                    theme::accent()
                                } else {
                                    theme::line_soft()
                                };
                                border_radius: RADIUS_SM;
                                interactive;
                                tooltip: picker_text.clone();

                                Block {
                                    width: Size::Fill;
                                    height: Size::Fill;
                                    font_size: 9.5;
                                    text_color: if entry.replacement.is_some() {
                                        theme::popup_text()
                                    } else {
                                        theme::popup_muted()
                                    };
                                    text: picker_text;
                                }
                                Block {
                                    width: Size::Pixels(18.0);
                                    height: Size::Fill;
                                    font_size: 13.0;
                                    text_color: theme::popup_muted();
                                    text: "…";
                                }
                            }
                        }
                    }
                }
            });
            if dialog.is_project_load() {
                build_modal_button(
                    ctx,
                    "missing-media-cancel",
                    missing_media_button_rect(local, false),
                    &i18n::text("dialog-cancel"),
                    ModalButtonRole::Secondary,
                );
            }
            build_modal_button(
                ctx,
                "missing-media-confirm",
                missing_media_button_rect(local, true),
                &i18n::text(if dialog.is_project_load() {
                    "dialog-load-project"
                } else {
                    "dialog-apply"
                }),
                ModalButtonRole::Primary,
            );
        },
    );
}

const UPDATE_DIALOG_W: f32 = 440.0;
const UPDATE_DIALOG_H: f32 = 190.0;

pub(super) fn update_dialog_rect(viewport_width: f32, viewport_height: f32) -> Rect {
    centered_dialog_rect(
        viewport_width,
        viewport_height,
        UPDATE_DIALOG_W.min((viewport_width - 16.0).max(280.0)),
        UPDATE_DIALOG_H,
    )
}

fn update_dialog_parts(dialog: Rect) -> (Rect, Rect, Rect, [Rect; 2]) {
    let rows = kama_ui::layout::column(
        dialog,
        &[
            kama_ui::layout::Item::height(12.0),
            kama_ui::layout::Item::height(20.0),
            kama_ui::layout::Item::height(8.0),
            kama_ui::layout::Item::height(64.0),
            kama_ui::layout::Item::height(8.0),
            kama_ui::layout::Item::height(20.0),
            kama_ui::layout::Item::fill(),
            kama_ui::layout::Item::height(24.0),
            kama_ui::layout::Item::height(12.0),
        ],
        0.0,
        0.0,
        ui::Align::Start,
        None,
    );
    let title = dialog_content_row(rows[1]);
    let body = dialog_content_row(rows[3]);
    let progress = dialog_content_row(rows[5]);
    let buttons = kama_ui::layout::row(
        rows[7],
        &[
            kama_ui::layout::Item::fill(),
            kama_ui::layout::Item::width(70.0),
            kama_ui::layout::Item::width(8.0),
            kama_ui::layout::Item::width(92.0),
            kama_ui::layout::Item::width(12.0),
        ],
        0.0,
        0.0,
        ui::Align::Start,
    );
    (title, body, progress, [buttons[1], buttons[3]])
}

pub(super) fn update_dialog_button_rect(dialog: Rect, primary: bool) -> Rect {
    update_dialog_parts(dialog).3[primary as usize]
}

fn update_dialog_title(dialog: &UpdateDialog) -> &'static str {
    match &dialog.state {
        UpdateDialogState::Available => "Update available",
        UpdateDialogState::Installing => "Updating Kama",
        UpdateDialogState::Installed => "Update installed",
        UpdateDialogState::Failed(_) => "Update failed",
    }
}

fn update_dialog_message(dialog: &UpdateDialog) -> String {
    match &dialog.state {
        UpdateDialogState::Available => format!(
            "Kama {} is available. You are currently using {}. Update now to install the new version.",
            dialog.version,
            version::VERSION,
        ),
        UpdateDialogState::Installing => format!(
            "Downloading and installing Kama {}. Keep Kama open until the update finishes.",
            dialog.version,
        ),
        UpdateDialogState::Installed => format!(
            "Kama {} was installed successfully. Restart Kama when you're ready to use the new version.",
            dialog.version,
        ),
        UpdateDialogState::Failed(error) => {
            let mut error = error.replace('\n', " ");
            if error.chars().count() > 150 {
                error = error.chars().take(147).collect::<String>() + "...";
            }
            format!(
                "The update could not be installed. You can retry or close this dialog.\n{error}"
            )
        }
    }
}

pub(super) fn build_update_dialog(
    ctx: &mut ui::BuildCtx,
    viewport_width: f32,
    viewport_height: f32,
    dialog: &UpdateDialog,
) {
    let viewport = Rect::new(0.0, 0.0, viewport_width, viewport_height);
    let rect = update_dialog_rect(viewport_width, viewport_height);
    let local = Rect::new(0.0, 0.0, rect.width, rect.height);
    let (title, body, progress, _) = update_dialog_parts(local);
    build_modal(
        ctx,
        "update-scrim",
        "update-dialog",
        (viewport, rect),
        dialog.opacity(Instant::now()),
        |ctx| {
            build_modal_title(ctx, "update-title", title, update_dialog_title(dialog));
            ui::ui!(ctx, {
                Rect("update-message", body) {
                    font_size: 10.5;
                    text_color: theme::popup_muted();
                    text: update_dialog_message(dialog);
                }
            });

            match &dialog.state {
                UpdateDialogState::Installing => ProgressBar::build_indeterminate(
                    ctx,
                    "update-progress",
                    progress,
                    dialog.progress_phase,
                    component_style(),
                ),
                UpdateDialogState::Installed => {
                    ProgressBar::build(ctx, "update-progress", progress, 1.0, component_style())
                }
                UpdateDialogState::Available | UpdateDialogState::Failed(_) => {}
            }

            match &dialog.state {
                UpdateDialogState::Available => {
                    build_modal_button(
                        ctx,
                        "update-cancel",
                        update_dialog_button_rect(local, false),
                        "Cancel",
                        ModalButtonRole::Secondary,
                    );
                    build_modal_button(
                        ctx,
                        "update-install",
                        update_dialog_button_rect(local, true),
                        "Update",
                        ModalButtonRole::Primary,
                    );
                }
                UpdateDialogState::Failed(_) => {
                    build_modal_button(
                        ctx,
                        "update-close",
                        update_dialog_button_rect(local, false),
                        "Close",
                        ModalButtonRole::Secondary,
                    );
                    build_modal_button(
                        ctx,
                        "update-retry",
                        update_dialog_button_rect(local, true),
                        "Retry",
                        ModalButtonRole::Primary,
                    );
                }
                UpdateDialogState::Installed => build_modal_button(
                    ctx,
                    "update-done",
                    update_dialog_button_rect(local, true),
                    "Close",
                    ModalButtonRole::Primary,
                ),
                UpdateDialogState::Installing => {}
            }
        },
    );
}

#[derive(Clone, Copy)]
pub(super) struct TextModalSpec {
    id: &'static str,
    size: [f32; 2],
    input_y: f32,
    placeholder_key: &'static str,
    confirm: &'static str,
}

impl TextModalSpec {
    pub(super) fn rect(self, width: f32, height: f32) -> Rect {
        centered_dialog_rect(width, height, self.size[0], self.size[1])
    }

    fn title(self, dialog: Rect) -> Rect {
        let rows = kama_ui::layout::column(
            dialog,
            &[
                kama_ui::layout::Item::height(8.0),
                kama_ui::layout::Item::height(20.0),
                kama_ui::layout::Item::fill(),
            ],
            0.0,
            0.0,
            ui::Align::Start,
            None,
        );
        kama_ui::layout::row(
            rows[1],
            &[
                kama_ui::layout::Item::width(12.0),
                kama_ui::layout::Item::fill(),
                kama_ui::layout::Item::width(12.0),
            ],
            0.0,
            0.0,
            ui::Align::Start,
        )[1]
    }

    pub(super) fn input(self, dialog: Rect) -> Rect {
        let rows = kama_ui::layout::column(
            dialog,
            &[
                kama_ui::layout::Item::height(self.input_y),
                kama_ui::layout::Item::height(28.0),
                kama_ui::layout::Item::fill(),
            ],
            0.0,
            0.0,
            ui::Align::Start,
            None,
        );
        kama_ui::layout::row(
            rows[1],
            &[
                kama_ui::layout::Item::width(12.0),
                kama_ui::layout::Item::fill(),
                kama_ui::layout::Item::width(12.0),
            ],
            0.0,
            0.0,
            ui::Align::Start,
        )[1]
    }

    pub(super) fn button(self, dialog: Rect, confirm: bool) -> Rect {
        let rows = kama_ui::layout::column(
            dialog,
            &[
                kama_ui::layout::Item::fill(),
                kama_ui::layout::Item::height(24.0),
                kama_ui::layout::Item::height(10.0),
            ],
            0.0,
            0.0,
            ui::Align::Start,
            None,
        );
        let buttons = kama_ui::layout::row(
            rows[1],
            &[
                kama_ui::layout::Item::fill(),
                kama_ui::layout::Item::width(72.0),
                kama_ui::layout::Item::width(8.0),
                kama_ui::layout::Item::width(72.0),
                kama_ui::layout::Item::width(12.0),
            ],
            0.0,
            0.0,
            ui::Align::Start,
        );
        buttons[if confirm { 3 } else { 1 }]
    }
}

pub(super) const NEW_COMPOSITION_MODAL: TextModalSpec = TextModalSpec {
    id: "new-composition",
    size: [360.0, 112.0],
    input_y: 34.0,
    placeholder_key: "dialog-composition-name",
    confirm: "dialog-create",
};
pub(super) const SPEED_DURATION_MODAL: TextModalSpec = TextModalSpec {
    id: "speed-duration",
    size: [420.0, 176.0],
    input_y: 82.0,
    placeholder_key: "dialog-value",
    confirm: "dialog-apply",
};
pub(super) const LAYOUT_SAVE_MODAL: TextModalSpec = TextModalSpec {
    id: "layout-save",
    size: [340.0, 112.0],
    input_y: 34.0,
    placeholder_key: "dialog-layout-name",
    confirm: "dialog-save",
};

pub(super) fn build_text_modal(
    ctx: &mut ui::BuildCtx,
    viewport: [f32; 2],
    dialog: &mut TextEdit,
    title: &str,
    spec: TextModalSpec,
    chrome: (f32, Color),
    extra: impl FnOnce(&mut ui::BuildCtx, Rect),
) {
    let [width, height] = viewport;
    let rect = spec.rect(width, height);
    let local = Rect::new(0.0, 0.0, rect.width, rect.height);
    let (opacity, text_color) = chrome;
    let mut input_style = component_style();
    input_style.text = text_color;
    build_modal(
        ctx,
        (spec.id, "scrim"),
        (spec.id, "dialog"),
        (Rect::new(0.0, 0.0, width, height), rect),
        opacity,
        |ctx| {
            build_modal_title(ctx, (spec.id, "title"), spec.title(local), title);
            extra(ctx, local);
            dialog.build(
                ctx,
                format_args!("{}-input", spec.id),
                spec.input(local),
                &i18n::text(spec.placeholder_key),
                input_style,
            );
            for (confirm, label) in [
                (false, i18n::text("dialog-cancel")),
                (true, i18n::text(spec.confirm)),
            ] {
                build_modal_button(
                    ctx,
                    (spec.id, "button", confirm),
                    spec.button(local, confirm),
                    &label,
                    if confirm {
                        ModalButtonRole::Primary
                    } else {
                        ModalButtonRole::Secondary
                    },
                );
            }
        },
    );
}

pub(super) fn build_new_composition_dialog(
    ctx: &mut ui::BuildCtx,
    viewport_width: f32,
    viewport_height: f32,
    dialog: &mut NewCompositionDialog,
) {
    let title = dialog.title();
    let opacity = dialog.opacity(Instant::now());
    build_text_modal(
        ctx,
        [viewport_width, viewport_height],
        &mut dialog.editor,
        title,
        NEW_COMPOSITION_MODAL,
        (opacity, theme::popup_text()),
        |_, _| {},
    );
}

pub(super) fn speed_duration_mode_rect(dialog: Rect, index: usize) -> Rect {
    let rows = kama_ui::layout::column(
        dialog,
        &[
            kama_ui::layout::Item::height(36.0),
            kama_ui::layout::Item::height(26.0),
            kama_ui::layout::Item::fill(),
        ],
        0.0,
        0.0,
        ui::Align::Start,
        None,
    );
    kama_ui::layout::row(
        rows[1],
        &[
            kama_ui::layout::Item::width(12.0),
            kama_ui::layout::Item::fill(),
            kama_ui::layout::Item::width(6.0),
            kama_ui::layout::Item::fill(),
            kama_ui::layout::Item::width(6.0),
            kama_ui::layout::Item::fill(),
            kama_ui::layout::Item::width(12.0),
        ],
        0.0,
        0.0,
        ui::Align::Start,
    )[1 + index * 2]
}

pub(super) fn build_speed_duration_dialog(
    ctx: &mut ui::BuildCtx,
    viewport_width: f32,
    viewport_height: f32,
    dialog: &mut SpeedDurationDialog,
) {
    let mode = dialog.mode;
    let opacity = dialog.opacity(Instant::now());
    build_text_modal(
        ctx,
        [viewport_width, viewport_height],
        &mut dialog.editor,
        "Speed / Duration",
        SPEED_DURATION_MODAL,
        (opacity, theme::popup_text()),
        |ctx, local| {
            for (index, (candidate, label)) in [
                (SpeedDurationMode::SpeedPercent, "Speed %"),
                (SpeedDurationMode::PerClipDuration, "Per Clip"),
                (SpeedDurationMode::TotalDuration, "Total"),
            ]
            .into_iter()
            .enumerate()
            {
                build_modal_button(
                    ctx,
                    ("speed-duration-mode", index),
                    speed_duration_mode_rect(local, index),
                    label,
                    if mode == candidate {
                        ModalButtonRole::Primary
                    } else {
                        ModalButtonRole::Secondary
                    },
                );
            }
        },
    );
}

pub(super) fn build_layout_save_dialog(
    ctx: &mut ui::BuildCtx,
    viewport_width: f32,
    viewport_height: f32,
    dialog: &mut LayoutSaveDialog,
) {
    let opacity = dialog.opacity(Instant::now());
    build_text_modal(
        ctx,
        [viewport_width, viewport_height],
        &mut dialog.editor,
        "Save Layout",
        LAYOUT_SAVE_MODAL,
        (opacity, theme::popup_text()),
        |_, _| {},
    );
}

#[derive(Clone, Copy)]
pub(super) struct PaletteMetrics {
    width: f32,
    title_h: f32,
    input_h: f32,
    row_h: f32,
    row_gap: f32,
    breadcrumb_h: f32,
    footer_h: f32,
}

const PALETTE_VIEWPORT_MAX_FRACTION: f32 = 0.8;
const ADD_MENU_MAX_HEIGHT: f32 = 600.0;

pub(super) fn palette_metrics(state: &PaletteState) -> PaletteMetrics {
    let add_menu = state.kind.is_some_and(PaletteKind::is_add_menu);
    let font_menu = matches!(state.kind, Some(PaletteKind::FontFamily));
    let searching = !state.query.text().trim().is_empty();
    if font_menu {
        PaletteMetrics {
            width: 340.0,
            title_h: 0.0,
            input_h: 29.0,
            row_h: 26.0,
            row_gap: 2.0,
            breadcrumb_h: 0.0,
            footer_h: 0.0,
        }
    } else if add_menu {
        PaletteMetrics {
            width: 320.0,
            title_h: 0.0,
            input_h: 29.0,
            row_h: if searching { 36.0 } else { 24.0 },
            row_gap: 3.0,
            breadcrumb_h: if state.path.is_empty() || searching {
                0.0
            } else {
                18.0
            },
            footer_h: 0.0,
        }
    } else {
        let replacement = matches!(state.kind, Some(PaletteKind::ReplaceSelectedClips { .. }));
        PaletteMetrics {
            width: if matches!(state.kind, Some(PaletteKind::Commands)) {
                560.0
            } else {
                360.0
            },
            title_h: if replacement {
                dialog::SEARCH_DIALOG_TITLE_HEIGHT
            } else {
                0.0
            },
            input_h: dialog::SEARCH_DIALOG_INPUT_HEIGHT,
            row_h: dialog::SEARCH_DIALOG_ROW_HEIGHT,
            row_gap: dialog::SEARCH_DIALOG_GAP,
            breadcrumb_h: 0.0,
            footer_h: if replacement {
                0.0
            } else {
                dialog::SEARCH_DIALOG_FOOTER_HEIGHT
            },
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct PaletteRects {
    title: Option<Rect>,
    input: Rect,
    back: Option<Rect>,
    body: Rect,
    footer: Option<Rect>,
}

fn measure_palette_rects(
    viewport: Rect,
    state: &PaletteState,
    visible_rows: usize,
    constrained: bool,
) -> (PaletteRects, Rect) {
    let metrics = palette_metrics(state);
    let (layout, root) = dialog::measure_search_surface(
        viewport,
        dialog::SearchSurfaceMetrics {
            padding: dialog::SEARCH_DIALOG_PADDING,
            gap: dialog::SEARCH_DIALOG_GAP,
            title_height: metrics.title_h,
            search_height: metrics.input_h,
            auxiliary_height: metrics.breadcrumb_h,
            row_height: metrics.row_h,
            row_gap: metrics.row_gap,
            footer_height: metrics.footer_h,
            close_width: dialog::SEARCH_DIALOG_CLOSE_WIDTH,
        },
        visible_rows,
        ScrollState::default(),
        constrained,
    );
    (
        PaletteRects {
            title: layout.title,
            input: layout.search,
            back: layout.auxiliary,
            body: layout.rows,
            footer: layout.footer,
        },
        root,
    )
}

pub(super) fn palette_natural_height(state: &PaletteState, visible_rows: usize) -> f32 {
    let metrics = palette_metrics(state);
    measure_palette_rects(
        Rect::new(0.0, 0.0, metrics.width, 1.0),
        state,
        visible_rows,
        false,
    )
    .1
    .height
}

pub(super) fn palette_max_height(state: &PaletteState, viewport_height: f32) -> f32 {
    let viewport_limit = (viewport_height - dialog::SEARCH_DIALOG_PADDING * 2.0).max(1.0);
    if state.kind.is_some_and(PaletteKind::is_add_menu) {
        ADD_MENU_MAX_HEIGHT.min(viewport_limit)
    } else {
        (viewport_height * PALETTE_VIEWPORT_MAX_FRACTION)
            .min(viewport_limit)
            .max(1.0)
    }
}

pub(super) fn palette_visible_rows(
    state: &PaletteState,
    entries: usize,
    viewport_height: f32,
) -> usize {
    if entries == 0 {
        return 0;
    }
    let max_height = palette_max_height(state, viewport_height);
    (1..=entries)
        .take_while(|&rows| palette_natural_height(state, rows) <= max_height)
        .last()
        .unwrap_or(1)
}

pub(super) fn palette_unscrolled_rows(state: &PaletteState, entries: usize) -> Vec<Rect> {
    if entries == 0 {
        return Vec::new();
    }
    let metrics = palette_metrics(state);
    kama_ui::layout::fit_column_at(
        Rect::new(0.0, 0.0, metrics.width, 1.0),
        [0.0, 0.0],
        metrics.width,
        &vec![kama_ui::layout::Item::height(metrics.row_h); entries],
        metrics.row_gap,
        0.0,
    )
    .1
}

pub(super) fn palette_max_scroll(state: &PaletteState, entries: usize, visible_rows: usize) -> f32 {
    if entries <= visible_rows || visible_rows == 0 {
        return 0.0;
    }
    let metrics = palette_metrics(state);
    let visible_rects = palette_unscrolled_rows(state, visible_rows);
    let visible_height = visible_rects
        .last()
        .zip(visible_rects.first())
        .map_or(0.0, |(last, first)| last.bottom() - first.y);
    let (scroll, measured) = kama_ui::measure_layout(
        Rect::new(0.0, 0.0, metrics.width, visible_height.max(1.0)),
        |ctx| {
            let root = ctx
                .new()
                .column()
                .width(Size::Fill)
                .height(Size::Fill)
                .vertical_scroll(ScrollState::default())
                .gap(metrics.row_gap)
                .children(|ctx| {
                    for _ in 0..entries {
                        let _ = ctx
                            .new()
                            .width(Size::Fill)
                            .height(Size::Pixels(metrics.row_h))
                            .build();
                    }
                })
                .build();
            root
        },
    );
    measured
        .scroll_range(scroll)
        .expect("palette scroll range")
        .vertical
}

pub(super) fn palette_rects(
    popup: Rect,
    state: &PaletteState,
    visible_rows: usize,
) -> PaletteRects {
    if matches!(state.kind, Some(PaletteKind::Commands)) && state.anchor.is_none() {
        let layout =
            dialog::measure_search_dialog(popup, false, visible_rows, ScrollState::default());
        return PaletteRects {
            title: None,
            input: layout.search,
            back: None,
            body: layout.rows,
            footer: Some(layout.footer),
        };
    }
    measure_palette_rects(popup, state, visible_rows, true).0
}

pub(super) fn palette_rect(
    viewport_width: f32,
    viewport_height: f32,
    state: &PaletteState,
    visible_rows: usize,
) -> Rect {
    let metrics = palette_metrics(state);
    let height =
        palette_natural_height(state, visible_rows).min(palette_max_height(state, viewport_height));
    let viewport = Rect::new(0.0, 0.0, viewport_width, viewport_height);
    if state.kind.is_some_and(PaletteKind::is_add_menu) {
        let anchor = state.anchor.unwrap_or_else(|| {
            let centered = kama_ui::layout::centered(viewport, 1.0, 1.0);
            kama_ui::layout::fit_column_at(
                viewport,
                [centered.x + 0.5, centered.y + 0.5],
                1.0,
                &[kama_ui::layout::Item::height(1.0)],
                0.0,
                0.0,
            )
            .1[0]
        });
        return ui::place_popup(anchor, [metrics.width, height], viewport, false, 2.0);
    }
    if let Some(anchor) = state.anchor {
        return ui::place_popup(anchor, [metrics.width, height], viewport, false, 2.0);
    }
    let width = metrics.width.min((viewport_width - 12.0).max(1.0));
    let height = height.min((viewport_height - 12.0).max(1.0));
    let centered = kama_ui::layout::centered(viewport, width, height);
    let y = if matches!(state.kind, Some(PaletteKind::Commands)) {
        74.0f32.min((viewport_height - height - 8.0).max(8.0))
    } else {
        centered.y.max(8.0)
    };
    kama_ui::layout::fit_column_at(
        viewport,
        [centered.x.max(8.0), y],
        width,
        &[kama_ui::layout::Item::height(height)],
        0.0,
        0.0,
    )
    .1[0]
}

pub(super) fn palette_input_rect(popup: Rect, state: &PaletteState) -> Rect {
    palette_rects(popup, state, 0).input
}

pub(super) fn palette_back_rect(popup: Rect, state: &PaletteState) -> Option<Rect> {
    palette_rects(popup, state, 0).back
}

pub(super) fn palette_body_rect(popup: Rect, state: &PaletteState, visible_rows: usize) -> Rect {
    palette_rects(popup, state, visible_rows).body
}

pub(super) fn palette_header_close_rect(popup: Rect, state: &PaletteState) -> Option<Rect> {
    let title = palette_rects(popup, state, 0).title?;
    Some(
        kama_ui::layout::row(
            title,
            &[
                kama_ui::layout::Item::fill(),
                kama_ui::layout::Item::width(27.0),
            ],
            0.0,
            0.0,
            ui::Align::Start,
        )[1],
    )
}

pub(super) fn palette_footer_close_rect(
    popup: Rect,
    state: &PaletteState,
    visible_rows: usize,
) -> Option<Rect> {
    if matches!(state.kind, Some(PaletteKind::Commands)) && state.anchor.is_none() {
        return Some(
            dialog::measure_search_dialog(popup, false, visible_rows, ScrollState::default()).close,
        );
    }
    let footer = palette_rects(popup, state, visible_rows).footer?;
    Some(
        kama_ui::layout::row(
            footer,
            &[
                kama_ui::layout::Item::fill(),
                kama_ui::layout::Item::width(dialog::SEARCH_DIALOG_CLOSE_WIDTH),
            ],
            0.0,
            0.0,
            ui::Align::Start,
        )[1],
    )
}

pub(super) fn palette_virtual_rows(body: Rect, state: &PaletteState, entries: usize) -> Vec<Rect> {
    let metrics = palette_metrics(state);
    kama_ui::layout::column(
        body,
        &vec![kama_ui::layout::Item::height(metrics.row_h); entries],
        metrics.row_gap,
        0.0,
        ui::Align::Start,
        Some(state.scroll),
    )
}

pub(super) fn palette_row_at(
    popup: Rect,
    state: &PaletteState,
    point: [f32; 2],
    entries: usize,
    visible_rows: usize,
) -> Option<usize> {
    let body = palette_body_rect(popup, state, visible_rows);
    if !body.contains(point) || entries == 0 {
        return None;
    }
    palette_virtual_rows(body, state, entries)
        .iter()
        .position(|row| row.contains(point))
}

#[allow(clippy::too_many_arguments)]
fn build_command_palette_dialog(
    ctx: &mut ui::Ui<'_>,
    viewport_width: f32,
    viewport_height: f32,
    state: &mut PaletteState,
    entries: &[PaletteEntry],
    icons: Icons,
    rect: Rect,
    opacity: f32,
) {
    let layout = dialog::measure_search_dialog(rect, false, entries.len(), state.scroll);
    dialog::build_search_dialog(
        ctx,
        "palette-scrim",
        "palette-dialog-shell",
        "command-palette",
        Rect::new(0.0, 0.0, viewport_width, viewport_height),
        rect,
        opacity,
        &layout,
        None,
        &i18n::text("palette-hint"),
        |ctx, layout| {
            state.query.build(
                ctx,
                "palette-input",
                layout.search,
                &i18n::text("palette-command-search"),
                component_style(),
            );
            ctx.with_clip(layout.rows, |ctx| {
                if entries.is_empty() {
                    ui_text!(
                        ctx,
                        "palette-no-results",
                        layout.empty,
                        10.5,
                        theme::popup_muted(),
                        i18n::text("settings-no-results"),
                    );
                }
                let selected = state.selected.min(entries.len().saturating_sub(1));
                for (index, entry) in entries.iter().enumerate() {
                    let row = layout.items[index];
                    if row.bottom() < layout.rows.y || row.y > layout.rows.bottom() {
                        continue;
                    }
                    let active = index == selected;
                    let inner = kama_ui::layout::inset(row, 5.0);
                    let parts = kama_ui::layout::row(
                        inner,
                        &[
                            kama_ui::layout::Item::width(18.0),
                            kama_ui::layout::Item::width(131.0),
                            kama_ui::layout::Item::fill(),
                        ],
                        7.0,
                        0.0,
                        ui::Align::Center,
                    );
                    ui::ui!(ctx, {
                        Rect(("palette-row", index), row) {
                            fill: if active { theme::accent_hover() } else { Color::TRANSPARENT };
                            border: 1;
                            border_color: if active { theme::accent() } else { Color::TRANSPARENT };
                            border_radius: RADIUS_SM;
                            interactive;
                        }
                        Icon {
                            id: @format("palette-entry-icon {}", index);
                            icon!: icons.get(entry.icon);
                            color!: if active { theme::popup_text() } else { theme::popup_muted() };
                            texture_mode: ui::TEXTURE_MODE_CONTAIN;
                            bounds: (parts[0].x, parts[0].y, parts[0].width, parts[0].height);
                        }
                        Rect(("palette-entry-label", index), parts[1]) {
                            font_size: 10.5;
                            text_color: theme::popup_text();
                            text: entry.label.clone();
                        }
                        Rect(("palette-entry-detail", index), parts[2]) {
                            font_size: 9.5;
                            text_color: theme::popup_muted();
                            text: entry.detail.clone();
                        }
                    });
                }
            });
        },
    );
}

pub(super) fn build_palette(
    ctx: &mut ui::Ui<'_>,
    viewport_width: f32,
    viewport_height: f32,
    state: &mut PaletteState,
    entries: &[PaletteEntry],
    icons: Icons,
) {
    let is_commands = matches!(state.kind, Some(PaletteKind::Commands));
    let is_add_menu = state.kind.is_some_and(PaletteKind::is_add_menu);
    let is_font_menu = matches!(state.kind, Some(PaletteKind::FontFamily));
    let compact_rows = is_add_menu || is_font_menu;
    let searching = !state.query.text().trim().is_empty();
    let metrics = palette_metrics(state);
    let visible_rows = palette_visible_rows(state, entries.len(), viewport_height);
    let max_scroll = palette_max_scroll(state, entries.len(), visible_rows);
    state.scroll.offset = state.scroll.offset.clamp(0.0, max_scroll);
    let rect = palette_rect(viewport_width, viewport_height, state, visible_rows);
    let palette_rects = palette_rects(rect, state, visible_rows);
    let width = rect.width;
    let height = rect.height;
    let (x, y) = (rect.x, rect.y);
    let breadcrumb = state.path.join(" ▶ ");
    let opacity = state.opacity(Instant::now());
    if is_commands && state.anchor.is_none() {
        build_command_palette_dialog(
            ctx,
            viewport_width,
            viewport_height,
            state,
            entries,
            icons,
            rect,
            opacity,
        );
        return;
    }
    if state.anchor.is_some() {
        dialog::build_panel_shell(ctx, "palette-dialog-shell", rect, opacity, |_| {});
    } else {
        dialog::build_shell(
            ctx,
            "palette-scrim",
            "palette-dialog-shell",
            Rect::new(0.0, 0.0, viewport_width, viewport_height),
            rect,
            opacity,
            |_| {},
        );
    }
    ui::ui!(ctx, {
        Block {
            id: if is_commands {
                "command-palette"
            } else if is_add_menu {
                "add-menu"
            } else if is_font_menu {
                "font-family-picker"
            } else {
                "panel-picker"
            };
            overlay;
            bounds: (x, y, width, height);
            fill: Color::TRANSPARENT;
            padding: dialog::SEARCH_DIALOG_PADDING;
            gap: dialog::SEARCH_DIALOG_GAP;
            opacity: opacity;

            @if matches!(state.kind, Some(PaletteKind::ReplaceSelectedClips { .. })) {
                Row {
                    id: "replacement-picker-header";
                    width: Size::Fill;
                    height: Size::Pixels(metrics.title_h);
                    padding: 0.0;

                    Block {
                        width: Size::Fill;
                        height: Size::Fill;
                        font_size: 14.0;
                        text_color: theme::popup_text();
                        text: i18n::text("palette-replace-selected-clips");
                    }
                    Block {
                        id: "replacement-picker-close";
                        width: Size::Pixels(27.0);
                        height: Size::Pixels(27.0);
                        fill: theme::control();
                        border: 1;
                        border_color: theme::line();
                        border_radius: 5.0;
                        font_size: 15.0;
                        text_color: theme::popup_text();
                        text_centered;
                        text: "×";
                        interactive;
                        tooltip: i18n::text("dialog-close");
                    }
                }
            }

            Block {
                id: "palette-input-row";
                width: Size::Fill;
                height: Size::Pixels(metrics.input_h);

                @rust {
                    state.query.build(
                        ctx,
                        "palette-input",
                        Rect::new(
                            0.0,
                            0.0,
                            (width - dialog::SEARCH_DIALOG_PADDING * 2.0).max(0.0),
                            metrics.input_h,
                        ),
                        &match state.kind {
                            Some(PaletteKind::Commands) => i18n::text("palette-command-search"),
                            Some(PaletteKind::AddPanel(_)) => i18n::text("palette-add-panel-search"),
                            Some(PaletteKind::TimelineAdd { kind: TrackKind::Video, .. }) => i18n::text("palette-video-search"),
                            Some(PaletteKind::TimelineAdd { kind: TrackKind::Audio, .. }) => i18n::text("palette-audio-search"),
                            Some(PaletteKind::TimelineAdd { kind: TrackKind::Effect, .. }) => i18n::text("palette-effect-search"),
                            Some(PaletteKind::VideoClip { .. }) => i18n::text("palette-generators-search"),
                            Some(PaletteKind::PipelineAssignment(PipelineKind::Audio)) => i18n::text("palette-audio-pipeline-search"),
                            Some(PaletteKind::NewPipeline) => i18n::text("palette-pipeline-type-search"),
                            Some(PaletteKind::PipelineAssignment(PipelineKind::Video)) => i18n::text("palette-effect-pipeline-search"),
                            Some(PaletteKind::AddEffect { audio: true }) => i18n::text("palette-audio-effects-search"),
                            Some(PaletteKind::AddEffect { audio: false }) => i18n::text("palette-effects-search"),
                            Some(PaletteKind::FontFamily) => i18n::text("palette-fonts-search"),
                            Some(PaletteKind::NodeInsert { .. }) => i18n::text("palette-nodes-search"),
                            Some(PaletteKind::EffectClip { .. }) => i18n::text("palette-effect-search"),
                            Some(PaletteKind::ReplaceSelectedClips { .. }) => i18n::text("palette-replacement-search"),
                            None => i18n::text("palette-search"),
                        },
                        component_style(),
                    );
                }
            }

            @if metrics.breadcrumb_h > 0.0 {
                Row {
                    id: "add-menu-back";
                    width: Size::Fill;
                    height: Size::Pixels(metrics.breadcrumb_h);
                    padding: 2.0;
                    gap: 4.0;
                    fill: Color::TRANSPARENT;
                    border_radius: RADIUS_SM;
                    interactive;

                    Block {
                        width: Size::Pixels(12.0);
                        height: Size::Fill;
                        font_size: 10.0;
                        text_color: theme::popup_muted();
                        text: "‹";
                    }
                    Block {
                        width: Size::Fill;
                        height: Size::Fill;
                        font_size: 8.75;
                        text_color: theme::popup_muted();
                        text: breadcrumb.clone();
                    }
                }
            }

            Block {
                id: "palette-scroll-container";
                width: Size::Fill;
                height: Size::Pixels(palette_rects.body.height);
                padding: 0.0;
                gap: metrics.row_gap;
                vertical_scroll: state.scroll;

                @if entries.is_empty() {
                    Block {
                        width: Size::Fill;
                        height: Size::Pixels(28.0);
                        padding: 6.0;
                        font_size: 10.5;
                        text_color: theme::popup_muted();
                        text: i18n::text("settings-no-results");
                    }
                }

                @if compact_rows {
                    @for (index, entry) in entries.iter().enumerate() {
                        @let selected = index == state.selected.min(entries.len().saturating_sub(1));
                        @let submenu = entry.is_submenu();
                        @let path = entry.breadcrumb();

                        Row {
                            id: @format("add-menu-row {} {}", index, entry.label);
                            fill: if selected { theme::accent_hover() } else { theme::control() };
                            border: 1;
                            border_color: if selected { theme::accent() } else { theme::line() };
                            border_radius: RADIUS_SM;
                            width: Size::Fill;
                            height: Size::Pixels(metrics.row_h);
                            padding: if is_add_menu && searching { 3.0 } else { 4.0 };
                            gap: 4.0;
                            interactive;

                            Column {
                                width: Size::Fill;
                                height: Size::Fill;
                                padding: 0.0;
                                gap: 0.0;

                                Block {
                                    width: Size::Fill;
                                    height: if is_add_menu && searching { Size::Pixels(16.0) } else { Size::Fill };
                                    font_size: 10.25;
                                    text_color: theme::popup_text();
                                    text: entry.label.clone();
                                }
                                @if is_add_menu && searching && !submenu {
                                    Block {
                                        width: Size::Fill;
                                        height: Size::Fill;
                                        font_size: 8.25;
                                        text_color: theme::popup_dim();
                                        text: path;
                                    }
                                }
                            }
                            @if submenu {
                                Block {
                                    width: Size::Pixels(14.0);
                                    height: Size::Fill;
                                    font_size: 10.0;
                                    text_color: theme::popup_muted();
                                    text_centered;
                                    text: "›";
                                }
                            }
                        }
                    }
                }

                @if !compact_rows {
                    @for (index, entry) in entries.iter().enumerate() {
                        @let selected = index == state.selected.min(entries.len().saturating_sub(1));

                        Row {
                            id: @format("palette-row {}", entry.label);
                            fill: if selected { theme::accent_hover() } else { Color::TRANSPARENT };
                            border: 1;
                            border_color: if selected { theme::accent() } else { Color::TRANSPARENT };
                            border_radius: RADIUS_SM;
                            width: Size::Fill;
                            height: Size::Pixels(metrics.row_h);
                            padding: 5.0;
                            gap: 7.0;
                            interactive;

                            Icon {
                                id: @format("palette-entry-icon {}", entry.label);
                                icon!: icons.get(entry.icon);
                                color!: if selected { theme::popup_text() } else { theme::popup_muted() };
                                width: Size::Pixels(18.0);
                                height: Size::Pixels(18.0);
                            }
                            Block {
                                width: Size::Pixels(131.0);
                                height: Size::Fill;
                                font_size: 10.5;
                                text_color: theme::popup_text();
                                text: entry.label.clone();
                            }
                            Block {
                                width: Size::Fill;
                                height: Size::Fill;
                                font_size: 9.5;
                                text_color: theme::popup_muted();
                                text: entry.detail.clone();
                            }
                        }
                    }
                }
            }

            @if !compact_rows {
                Row {
                    id: "palette-footer";
                    width: Size::Fill;
                    height: Size::Pixels(metrics.footer_h);

                    Block {
                        width: Size::Fill;
                        height: Size::Fill;
                        font_size: 9.0;
                        text_color: theme::popup_dim();
                        text: i18n::text("palette-hint");
                    }
                    Block {
                        id: "palette-close";
                        width: Size::Pixels(44.0);
                        height: Size::Fill;
                        interactive;
                        tooltip: i18n::text("dialog-close");
                        content_centered;

                        Icon {
                            id: "palette-close-icon";
                            icon!: icons.get(AppIcon::Close);
                            color!: theme::popup_muted();
                            width: Size::Pixels(16.0);
                            height: Size::Pixels(16.0);
                        }
                    }
                }
            }
        }
    });
}

pub(super) fn plugin_menu_name(id: &str) -> String {
    if id == "builtin" {
        "builtins".into()
    } else {
        id.to_string()
    }
}

pub(super) fn category_menu_path(category: &str) -> Vec<String> {
    let path = category
        .split('▶')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if path.is_empty() {
        vec!["Other".into()]
    } else {
        path
    }
}

pub(super) fn plugin_menu_path(plugin_id: &str) -> Vec<String> {
    vec!["Plugins".into(), plugin_menu_name(plugin_id)]
}

pub(super) fn effect_menu_path(effect: &crate::plugin::EffectDefinition) -> Vec<String> {
    category_menu_path(&effect.category)
}

pub(super) fn audio_effect_menu_path(effect: &crate::plugin::AudioEffectDefinition) -> Vec<String> {
    let mut path = vec!["Audio Effects".into()];
    path.extend(category_menu_path(&effect.category));
    path
}

pub(super) fn audio_effect_plugin_path(key: &str) -> Vec<String> {
    let plugin = key.split('.').next().unwrap_or("plugin");
    plugin_menu_path(plugin)
}

pub(super) fn value_node_menu_path(kind: ValueNodeKind) -> Vec<String> {
    use ValueNodeKind::*;
    match kind {
        Float | Vec2 | Color => vec!["General".into(), "Input".into()],
        Timestamp | LocalTimestamp | FrameCount | LocalFrame | FrameRate => {
            vec!["General".into(), "Time".into()]
        }
        Pi | Tau => vec!["General".into(), "Math".into(), "Constants".into()],
        Sin | Cos | Tan => vec!["General".into(), "Math".into(), "Trigonometry".into()],
        Add | Subtract | Multiply | Divide | Modulo | Power | Min | Max | Clamp | Lerp | Negate
        | Abs | Sqrt | Floor | Ceil | Round | Fract => {
            vec!["General".into(), "Math".into()]
        }
    }
}

pub(super) fn organize_add_menu_entries(
    state: &PaletteState,
    entries: Vec<PaletteEntry>,
    query: &str,
    media_only: bool,
) -> Vec<PaletteEntry> {
    let media = |entry: &PaletteEntry| entry.path.first().is_some_and(|part| part == "Media");
    let mut leaves = entries
        .into_iter()
        .filter(|entry| !media_only || media(entry))
        .collect::<Vec<_>>();

    if !query.is_empty() || media_only {
        if !query.is_empty() {
            let mut scored = leaves
                .into_iter()
                .filter_map(|entry| {
                    let aliases = entry
                        .aliases
                        .iter()
                        .map(|path| path.join(" ▶ "))
                        .collect::<Vec<_>>()
                        .join(" ");
                    let candidate = format!("{} {} {}", entry.breadcrumb(), aliases, entry.detail);
                    fuzzy_score(query, &candidate).map(|score| (score, entry))
                })
                .collect::<Vec<_>>();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.label.cmp(&b.1.label)));
            return scored.into_iter().map(|(_, entry)| entry).collect();
        }
        leaves.sort_by(|a, b| a.label.cmp(&b.label));
        return leaves;
    }

    let mut visible = Vec::new();
    let mut submenus = Vec::<String>::new();
    for entry in leaves {
        let mut leaf_visible = false;
        for path in std::iter::once(&entry.path).chain(entry.aliases.iter()) {
            if !path.as_slice().starts_with(state.path.as_slice()) {
                continue;
            }
            if let Some(next) = path.get(state.path.len()) {
                if submenus.iter().any(|submenu| submenu == next) {
                    continue;
                }
                submenus.push(next.clone());
                visible.push(PaletteEntry {
                    label: next.clone(),
                    detail: String::new(),
                    path: state.path.clone(),
                    aliases: Vec::new(),
                    icon: AppIcon::Plus,
                    target: PaletteTarget::Submenu(next.clone()),
                });
            } else {
                leaf_visible = true;
            }
        }
        if leaf_visible {
            visible.push(entry);
        }
    }
    visible
}

pub(super) fn palette_add(
    label: &str,
    detail: impl Into<String>,
    path: Vec<String>,
    icon: AppIcon,
    action: PaletteAction,
) -> PaletteEntry {
    PaletteEntry {
        label: label.to_string(),
        detail: detail.into(),
        path,
        aliases: Vec::new(),
        icon,
        target: PaletteTarget::Command(EditorCommand::Action(action)),
    }
}

pub(super) fn add_video_palette_entries(
    entries: &mut Vec<PaletteEntry>,
    project: &Project,
    plugins: &PluginRegistry,
    track: u32,
    time: f32,
) {
    for generator in plugins.generators() {
        entries.push(
            palette_add(
                &generator.name,
                format!("{} {}", generator.description, generator.key),
                vec!["Generators".into()],
                AppIcon::Video,
                PaletteAction::InsertGenerator {
                    choice: GeneratorChoice::Plugin(generator.key.clone()),
                    track,
                    time,
                },
            )
            .alias(plugin_menu_path(&generator.plugin_id)),
        );
    }
    for asset in &project.media {
        let entry = if matches!(asset.kind, MediaKind::WasmPlugin) {
            Some(palette_add(
                &asset.name,
                "CPU/WASM generator plugin",
                vec!["Generators".into()],
                AppIcon::Node,
                PaletteAction::InsertGenerator {
                    choice: GeneratorChoice::Wasm(asset.id),
                    track,
                    time,
                },
            ))
        } else if matches!(
            asset.kind,
            MediaKind::Video | MediaKind::Image { .. } | MediaKind::Model3d | MediaKind::Unknown
        ) {
            Some(palette_add(
                &asset.name,
                asset.path.display().to_string(),
                vec!["Media".into()],
                match asset.kind {
                    MediaKind::Image { .. } => AppIcon::Image,
                    MediaKind::Video => AppIcon::Video,
                    MediaKind::Model3d => AppIcon::Node,
                    _ => AppIcon::Media,
                },
                PaletteAction::InsertMedia {
                    media: asset.id,
                    track,
                    time,
                },
            ))
        } else {
            None
        };
        entries.extend(entry);
    }
}

pub(super) fn add_effect_clip_palette_entries(
    entries: &mut Vec<PaletteEntry>,
    project: &Project,
    track: u32,
    time: f32,
    video_only: bool,
) {
    entries.extend([
        palette_add(
            "New Effect Pipeline",
            "Create a pipeline and an Effect Clip",
            vec!["Effect Clip".into()],
            AppIcon::Effect,
            PaletteAction::InsertEffectClipWithNewPipeline { track, time },
        ),
        palette_add(
            "Empty Effect Clip",
            "Create an Effect Clip with no named pipeline yet",
            vec!["Effect Clip".into()],
            AppIcon::Effect,
            PaletteAction::InsertEffectClip {
                track,
                time,
                pipeline: None,
            },
        ),
    ]);
    entries.extend(
        project
            .pipelines
            .iter()
            .filter(|pipeline| !video_only || pipeline.kind == PipelineKind::Video)
            .map(|pipeline| {
                palette_add(
                    &pipeline.name,
                    "Effect Clip processes everything below through this pipeline",
                    vec!["Pipelines".into()],
                    AppIcon::Effect,
                    PaletteAction::InsertEffectClip {
                        track,
                        time,
                        pipeline: Some(pipeline.id),
                    },
                )
            }),
    );
}

pub(super) fn asset_track_counts(asset: &crate::project::MediaAsset) -> (usize, usize) {
    use crate::project::MediaTrackKind;
    let video = if matches!(asset.kind, MediaKind::Image { .. } | MediaKind::Model3d) {
        1
    } else {
        asset
            .tracks
            .iter()
            .filter(|track| track.kind == MediaTrackKind::Video)
            .count()
    };
    let audio = asset
        .tracks
        .iter()
        .filter(|track| track.kind == MediaTrackKind::Audio)
        .count();
    (video, audio)
}

pub(super) fn palette_entries(
    state: &PaletteState,
    project: &Project,
    plugins: &PluginRegistry,
    command_registry: &CommandRegistry,
) -> Vec<PaletteEntry> {
    let mut entries = Vec::new();
    let e = |label: &str, detail: &str, icon: AppIcon, action: PaletteAction| PaletteEntry {
        label: label.to_string(),
        detail: detail.to_string(),
        path: Vec::new(),
        aliases: Vec::new(),
        icon,
        target: PaletteTarget::Command(EditorCommand::Action(action)),
    };
    let raw_query = state.query.text().trim();
    let mut filter_query = raw_query;
    let mut media_only = false;

    match state.kind {
        Some(PaletteKind::Commands) => {
            entries.extend(
                command_registry
                    .palette_definitions()
                    .map(|definition| PaletteEntry {
                        label: definition.label.clone(),
                        detail: definition.shortcut.map_or_else(
                            || definition.description.clone(),
                            |shortcut| format!("{}    {shortcut}", definition.description),
                        ),
                        path: Vec::new(),
                        aliases: Vec::new(),
                        icon: definition.icon.unwrap_or(AppIcon::Plus),
                        target: PaletteTarget::Command(definition.command.clone()),
                    }),
            );
        }
        Some(PaletteKind::AddPanel(stack)) => {
            for panel in PanelKind::ALL {
                let info = panel.info();
                entries.push(e(
                    &panel.display_title(),
                    &panel.display_description(),
                    info.icon,
                    PaletteAction::AddPanel(panel, Some(stack)),
                ));
            }
        }
        Some(PaletteKind::TimelineAdd { track, time, kind }) => {
            if kind == TrackKind::Video {
                if let Some(query) = raw_query.strip_prefix('@') {
                    filter_query = query.trim();
                    media_only = true;
                }
            }
            match kind {
                TrackKind::Video => {
                    add_video_palette_entries(&mut entries, project, plugins, track, time);
                }
                TrackKind::Audio => {
                    for asset in &project.media {
                        if matches!(asset.kind, MediaKind::Audio)
                            || (matches!(asset.kind, MediaKind::Video) && asset.has_audio)
                        {
                            entries.push(palette_add(
                                &asset.name,
                                asset.path.display().to_string(),
                                vec!["Media".into(), "Audio".into()],
                                AppIcon::Audio,
                                PaletteAction::InsertAudioMedia {
                                    media: asset.id,
                                    track,
                                    time,
                                },
                            ));
                        }
                    }
                }
                TrackKind::Effect => {
                    add_effect_clip_palette_entries(&mut entries, project, track, time, true);
                }
            }
        }
        Some(PaletteKind::VideoClip { track, time }) => {
            if let Some(query) = raw_query.strip_prefix('@') {
                filter_query = query.trim();
                media_only = true;
            }
            add_video_palette_entries(&mut entries, project, plugins, track, time);
        }
        Some(PaletteKind::ReplaceSelectedClips {
            min_video_tracks,
            min_audio_tracks,
        }) => {
            for asset in &project.media {
                if state.replacement_excluded_media.contains(&asset.id) {
                    continue;
                }
                let (video_tracks, audio_tracks) = asset_track_counts(asset);
                if video_tracks < min_video_tracks || audio_tracks < min_audio_tracks {
                    continue;
                }
                entries.push(e(
                    &asset.name,
                    &format!(
                        "{}    {video_tracks} video / {audio_tracks} audio",
                        asset.path.display()
                    ),
                    match asset.kind {
                        MediaKind::Audio => AppIcon::Audio,
                        MediaKind::Image { .. } => AppIcon::Image,
                        MediaKind::Model3d => AppIcon::Node,
                        _ => AppIcon::Video,
                    },
                    PaletteAction::ReplaceSelectedClips { media: asset.id },
                ));
            }
        }
        Some(PaletteKind::NewPipeline) => {
            entries.push(e(
                "Video Pipeline",
                "Create a visual/effect pipeline",
                AppIcon::Video,
                PaletteAction::CreatePipeline(PipelineKind::Video),
            ));
            entries.push(e(
                "Audio Pipeline",
                "Create an audio processing pipeline",
                AppIcon::Audio,
                PaletteAction::CreatePipeline(PipelineKind::Audio),
            ));
        }
        Some(PaletteKind::PipelineAssignment(kind)) => {
            entries.push(e(
                "None",
                "Remove the named pipeline assignment",
                AppIcon::Remove,
                PaletteAction::AssignPipeline(None),
            ));
            entries.push(e(
                if kind == PipelineKind::Audio {
                    "New Audio Pipeline"
                } else {
                    "New Effect Pipeline"
                },
                "Create, assign, and keep it resident",
                if kind == PipelineKind::Audio {
                    AppIcon::Audio
                } else {
                    AppIcon::Effect
                },
                PaletteAction::CreateAndAssignPipeline(kind),
            ));
            for pipeline in project
                .pipelines
                .iter()
                .filter(|pipeline| pipeline.kind == kind)
            {
                entries.push(e(
                    &pipeline.name,
                    if kind == PipelineKind::Audio {
                        "Assign this shared Audio Pipeline"
                    } else {
                        "Assign this shared Effect Pipeline"
                    },
                    if kind == PipelineKind::Audio {
                        AppIcon::Audio
                    } else {
                        AppIcon::Effect
                    },
                    PaletteAction::AssignPipeline(Some(pipeline.id)),
                ));
            }
        }
        Some(PaletteKind::FontFamily) => {
            entries.push(e(
                "System fallback",
                "",
                AppIcon::Search,
                PaletteAction::SetFontFamily(String::new()),
            ));
            entries.extend(state.font_options.iter().map(|family| {
                e(
                    family,
                    "",
                    AppIcon::Search,
                    PaletteAction::SetFontFamily(family.clone()),
                )
            }));
        }
        Some(PaletteKind::AddEffect { audio: true }) => {
            for effect in plugins.audio_effects() {
                entries.push(
                    palette_add(
                        &effect.name,
                        format!("{} {}", effect.description, effect.key),
                        audio_effect_menu_path(effect),
                        AppIcon::Audio,
                        PaletteAction::AddAudioEffect(effect.key.clone()),
                    )
                    .alias(audio_effect_plugin_path(&effect.key)),
                );
            }
        }
        Some(PaletteKind::AddEffect { audio: false }) => {
            for effect in plugins
                .effects()
                .filter(|effect| effect.is_stack_insertable())
            {
                entries.push(
                    palette_add(
                        &effect.name,
                        effect.key.clone(),
                        effect_menu_path(effect),
                        AppIcon::Effect,
                        PaletteAction::AddEffect(effect.key.clone()),
                    )
                    .alias(plugin_menu_path(&effect.plugin_id)),
                );
            }
        }
        Some(PaletteKind::NodeInsert { pipeline, position }) => {
            entries.push(palette_add(
                "Pipeline",
                "Run through another pipeline",
                vec!["General".into()],
                AppIcon::Graph,
                PaletteAction::AddGraphPipeline { pipeline, position },
            ));
            let value_nodes = || {
                ValueNodeKind::INSERTABLE.into_iter().map(|kind| {
                    palette_add(
                        kind.label(),
                        kind.detail(),
                        value_node_menu_path(kind),
                        AppIcon::Node,
                        PaletteAction::AddGraphValue {
                            pipeline,
                            kind,
                            position,
                        },
                    )
                })
            };
            if project
                .pipeline(pipeline)
                .is_some_and(|pipeline| pipeline.kind == PipelineKind::Audio)
            {
                for effect in plugins.audio_effects() {
                    entries.push(
                        palette_add(
                            &effect.name,
                            format!("{} {}", effect.description, effect.key),
                            audio_effect_menu_path(effect),
                            AppIcon::Audio,
                            PaletteAction::AddGraphAudio {
                                pipeline,
                                node_type: effect.key.clone(),
                                position,
                            },
                        )
                        .alias(audio_effect_plugin_path(&effect.key)),
                    );
                }
                entries.extend(value_nodes());
            } else {
                entries.extend(value_nodes());
                for generator in plugins.generators() {
                    entries.push(
                        palette_add(
                            &generator.name,
                            format!("{} {}", generator.description, generator.key),
                            vec!["Generators".into()],
                            AppIcon::Node,
                            PaletteAction::AddGraphGenerator {
                                pipeline,
                                generator_type: generator.key.clone(),
                                position,
                            },
                        )
                        .alias(plugin_menu_path(&generator.plugin_id)),
                    );
                }
                for effect in plugins.effects() {
                    entries.push(
                        palette_add(
                            &effect.name,
                            effect.key.clone(),
                            effect_menu_path(effect),
                            AppIcon::Node,
                            PaletteAction::AddGraphNode {
                                pipeline,
                                node_type: effect.key.clone(),
                                position,
                            },
                        )
                        .alias(plugin_menu_path(&effect.plugin_id)),
                    );
                }
            }
        }
        Some(PaletteKind::EffectClip { track, time }) => {
            add_effect_clip_palette_entries(&mut entries, project, track, time, false);
        }
        None => return Vec::new(),
    }

    if state.kind.is_some_and(PaletteKind::is_add_menu) {
        return organize_add_menu_entries(state, entries, filter_query, media_only);
    }
    if filter_query.is_empty() {
        return entries;
    }
    let mut entries = entries
        .into_iter()
        .filter_map(|entry| {
            let candidate = format!("{} {}", entry.label, entry.detail);
            fuzzy_score(filter_query, &candidate).map(|score| (score, entry))
        })
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.label.cmp(&b.1.label)));
    entries.into_iter().map(|(_, entry)| entry).collect()
}
