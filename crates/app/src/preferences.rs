use std::{collections::HashMap, path::PathBuf};

use anyhow::Result;
use kama_ui::{
    components::{ColorPicker, ComboBox, Slider, TextEdit},
    BlockId, Color, IconId, Rect, Renderer, ScrollState, Size,
};
use serde::{Deserialize, Serialize};
use winit::{
    event::{ElementState, Ime, KeyEvent},
    keyboard::{Key, ModifiersState, NamedKey},
};

use crate::{
    command::{fuzzy_score, CommandRegistry, KeyBinding},
    dialog,
    file_io::{app_data_dir, atomic_write_json, read_json},
    runtime::media::{hardware_decoding_enabled, set_hardware_decoding_enabled},
    theme::{self, ThemePreset},
    widgets::component_style,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PreferencesFile {
    #[serde(default)]
    theme: ThemePreset,
    #[serde(default = "default_dark_accent", alias = "accent")]
    dark_accent: [u8; 4],
    #[serde(default = "default_light_accent")]
    light_accent: [u8; 4],
    #[serde(default)]
    keybinds: HashMap<String, Option<KeyBinding>>,
    #[serde(default)]
    plugin_paths: String,
    #[serde(default = "default_rounded_corners")]
    rounded_corners: bool,
    #[serde(default = "default_brightness")]
    brightness: f32,
    #[serde(default = "default_accent_mixing")]
    accent_mixing: f32,
    #[serde(default = "default_reveal_strength")]
    reveal_strength: f32,
    #[serde(default = "default_reveal_accent_mix")]
    reveal_accent_mix: f32,
    #[serde(default = "default_hardware_decoding")]
    hardware_decoding: bool,
}

fn default_dark_accent() -> [u8; 4] {
    [0xc1, 0x2c, 0xff, 0xff]
}

fn default_light_accent() -> [u8; 4] {
    [0xa0, 0x70, 0xff, 0xff]
}

fn default_rounded_corners() -> bool {
    true
}

fn default_brightness() -> f32 {
    0.08
}

fn default_accent_mixing() -> f32 {
    0.03
}

fn default_reveal_strength() -> f32 {
    0.5
}

fn default_reveal_accent_mix() -> f32 {
    0.25
}

fn default_hardware_decoding() -> bool {
    true
}

const THEME_OPTIONS: [&str; 3] = ["System", "Light", "Dark"];

fn theme_index(theme: ThemePreset) -> usize {
    match theme {
        ThemePreset::System => 0,
        ThemePreset::Light => 1,
        ThemePreset::Dark => 2,
    }
}

fn theme_at_index(index: usize) -> ThemePreset {
    match index {
        1 => ThemePreset::Light,
        2 => ThemePreset::Dark,
        _ => ThemePreset::System,
    }
}

impl Default for PreferencesFile {
    fn default() -> Self {
        Self {
            theme: ThemePreset::Dark,
            dark_accent: default_dark_accent(),
            light_accent: default_light_accent(),
            keybinds: HashMap::new(),
            plugin_paths: String::new(),
            rounded_corners: default_rounded_corners(),
            brightness: default_brightness(),
            accent_mixing: default_accent_mixing(),
            reveal_strength: default_reveal_strength(),
            reveal_accent_mix: default_reveal_accent_mix(),
            hardware_decoding: default_hardware_decoding(),
        }
    }
}

fn path() -> PathBuf {
    app_data_dir().join("settings.json")
}

pub(crate) fn load_plugin_paths() -> String {
    read_json::<PreferencesFile>(&path())
        .unwrap_or_default()
        .plugin_paths
}

pub(crate) fn load(registry: &mut CommandRegistry) {
    let preferences = read_json::<PreferencesFile>(&path()).unwrap_or_default();
    theme::set_theme_immediate(preferences.theme);
    let dark_accent = if preferences.dark_accent == [0xf0, 0xa2, 0x15, 0xff] {
        default_dark_accent()
    } else {
        preferences.dark_accent
    };
    theme::set_dark_accent_rgba8(dark_accent);
    let light_accent = if preferences.light_accent == [0xd9, 0x7a, 0xff, 0xff] {
        default_light_accent()
    } else {
        preferences.light_accent
    };
    theme::set_light_accent_rgba8(light_accent);
    theme::set_brightness(preferences.brightness);
    theme::set_accent_mixing(preferences.accent_mixing);
    kama_ui::set_reveal_strength(preferences.reveal_strength);
    kama_ui::set_reveal_accent_mix(preferences.reveal_accent_mix);
    kama_ui::set_rounded_corners_enabled(preferences.rounded_corners);
    set_hardware_decoding_enabled(preferences.hardware_decoding);
    for (id, binding) in preferences.keybinds {
        let id = if id == "timeline.toggle-cut-tool" {
            "timeline.toggle-razor-tool"
        } else {
            id.as_str()
        };
        registry.set_shortcut(id, binding);
    }
}

pub(crate) fn save(registry: &CommandRegistry, plugin_paths: &str) {
    let preferences = PreferencesFile {
        theme: theme::theme(),
        dark_accent: theme::dark_accent_rgba8(),
        light_accent: theme::light_accent_rgba8(),
        keybinds: registry
            .definitions()
            .iter()
            .map(|definition| (definition.id.clone(), definition.shortcut))
            .collect(),
        plugin_paths: plugin_paths.to_owned(),
        rounded_corners: kama_ui::rounded_corners_enabled(),
        brightness: theme::brightness(),
        accent_mixing: theme::accent_mixing(),
        reveal_strength: kama_ui::reveal_strength(),
        reveal_accent_mix: kama_ui::reveal_accent_mix(),
        hardware_decoding: hardware_decoding_enabled(),
    };
    let _ = atomic_write_json(&path(), &preferences);
}

const SETTINGS_W: f32 = 560.0;
const SETTINGS_H: f32 = 464.0;
const KEYBINDS_W: f32 = 680.0;
const KEYBINDS_H: f32 = 580.0;
const HEADER_H: f32 = 42.0;
const SEARCH_H: f32 = 38.0;
const ROW_H: f32 = 34.0;
const BODY_PAD: f32 = 12.0;

#[derive(Clone, Copy)]
struct SearchDialogLayout {
    title: Rect,
    close: Rect,
    search: Rect,
    rows: Rect,
    help: Rect,
}

fn centered(width: f32, height: f32, w: f32, h: f32) -> Rect {
    let viewport = Rect::new(0.0, 0.0, width, height);
    let (id, measured) = kama_ui::measure_layout(viewport, |ctx| {
        ctx.new()
            .overlay()
            .centered()
            .width(Size::Pixels(w))
            .height(Size::Pixels(h))
            .build()
    });
    measured.rect(id).expect("centered dialog layout")
}

fn settings_rect(width: f32, height: f32) -> Rect {
    centered(
        width,
        height,
        SETTINGS_W.min((width - 24.0).max(1.0)),
        SETTINGS_H.min((height - 24.0).max(1.0)),
    )
}

fn keybinds_rect(width: f32, height: f32) -> Rect {
    centered(
        width,
        height,
        KEYBINDS_W.min((width - 24.0).max(1.0)),
        KEYBINDS_H.min((height - 24.0).max(1.0)),
    )
}

fn search_dialog_layout(rect: Rect) -> SearchDialogLayout {
    let vertical = crate::ui_layout::column(
        rect,
        &[
            crate::ui_layout::Item::height(HEADER_H),
            crate::ui_layout::Item::height(2.0),
            crate::ui_layout::Item::height(SEARCH_H),
            crate::ui_layout::Item::height(7.0),
            crate::ui_layout::Item::fill(),
            crate::ui_layout::Item::height(BODY_PAD),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
        None,
    );
    let header = crate::ui_layout::row(
        vertical[0],
        &[
            crate::ui_layout::Item::width(14.0),
            crate::ui_layout::Item::fill(),
            crate::ui_layout::Item::width(5.0),
            crate::ui_layout::Item::width(27.0),
            crate::ui_layout::Item::width(8.0),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
    );
    let title = crate::ui_layout::column(
        header[1],
        &[
            crate::ui_layout::Item::height(11.0),
            crate::ui_layout::Item::height(22.0),
            crate::ui_layout::Item::fill(),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
        None,
    )[1];
    let close = crate::ui_layout::column(
        header[3],
        &[
            crate::ui_layout::Item::height(8.0),
            crate::ui_layout::Item::height(27.0),
            crate::ui_layout::Item::fill(),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
        None,
    )[1];
    let search = crate::ui_layout::row(
        vertical[2],
        &[
            crate::ui_layout::Item::width(BODY_PAD),
            crate::ui_layout::Item::fill(),
            crate::ui_layout::Item::width(BODY_PAD),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
    )[1];
    let rows = crate::ui_layout::row(
        vertical[4],
        &[
            crate::ui_layout::Item::width(BODY_PAD),
            crate::ui_layout::Item::fill(),
            crate::ui_layout::Item::width(BODY_PAD),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
    )[1];
    let help_row = crate::ui_layout::column(
        rect,
        &[
            crate::ui_layout::Item::fill(),
            crate::ui_layout::Item::height(14.0),
            crate::ui_layout::Item::height(6.0),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
        None,
    )[1];
    let help = crate::ui_layout::row(
        help_row,
        &[
            crate::ui_layout::Item::width(18.0),
            crate::ui_layout::Item::fill(),
            crate::ui_layout::Item::width(18.0),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
    )[1];
    SearchDialogLayout {
        title,
        close,
        search,
        rows,
        help,
    }
}

fn search_rect(rect: Rect) -> Rect {
    search_dialog_layout(rect).search
}

fn rows_rect(rect: Rect) -> Rect {
    search_dialog_layout(rect).rows
}

fn dialog_frame(ctx: &mut kama_ui::BuildCtx, id: &str, rect: Rect, title: &str) {
    let layout = search_dialog_layout(rect);
    ui_text!(
        ctx,
        (id, "title"),
        layout.title,
        14.0,
        theme::popup_text(),
        title,
    );
    kama_ui::ui!(ctx, {
        Rect((id, "close"), layout.close) {
            fill: theme::control(); border: 1; border_color: theme::line(); border_radius: 5.0;
            font_size: 15.0; text_color: theme::popup_text(); text_centered; text: "×"; interactive;
        }
    });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingEntry {
    Theme,
    DarkAccent,
    LightAccent,
    Brightness,
    AccentMixing,
    RevealStrength,
    RevealAccentMix,
    RoundedCorners,
    HardwareDecoding,
    PluginPaths,
}

fn filtered_settings(query: &str) -> Vec<SettingEntry> {
    let entries = [
        (
            SettingEntry::Theme,
            "Theme preset System Light Dark appearance",
        ),
        (
            SettingEntry::DarkAccent,
            "Dark accent color picker colour hex appearance",
        ),
        (
            SettingEntry::LightAccent,
            "Light accent color picker colour hex appearance",
        ),
        (
            SettingEntry::Brightness,
            "Brightness background surface light dark appearance theme",
        ),
        (
            SettingEntry::AccentMixing,
            "Accent mixing background surface tint color appearance",
        ),
        (
            SettingEntry::RevealStrength,
            "Reveal strength flashlight glow hover interaction appearance",
        ),
        (
            SettingEntry::RevealAccentMix,
            "Reveal accent mix flashlight glow hover accent color tint appearance",
        ),
        (
            SettingEntry::RoundedCorners,
            "Rounded corners square corners appearance border radius",
        ),
        (
            SettingEntry::HardwareDecoding,
            "Hardware decoding video playback GPU decoder VideoToolbox VAAPI DXVA CUDA software",
        ),
        (
            SettingEntry::PluginPaths,
            "Plugin paths folders directories extensions search path",
        ),
    ];
    if query.trim().is_empty() {
        return entries.into_iter().map(|(entry, _)| entry).collect();
    }
    let mut scored = entries
        .into_iter()
        .filter_map(|(entry, text)| fuzzy_score(query, text).map(|score| (score, entry)))
        .collect::<Vec<_>>();
    scored.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    scored.into_iter().map(|(_, entry)| entry).collect()
}

fn flow_row_rect(body: Rect, index: usize, scroll: ScrollState) -> Rect {
    let (ids, measured) = kama_ui::measure_layout(body, |ctx| {
        let mut ids = Vec::with_capacity(index + 1);
        ctx.new()
            .width(Size::Fill)
            .height(Size::Fill)
            .gap(3.0)
            .vertical_scroll(scroll)
            .children(|ctx| {
                for _ in 0..=index {
                    ids.push(
                        ctx.new()
                            .width(Size::Fill)
                            .height(Size::Pixels(ROW_H - 3.0))
                            .build(),
                    );
                }
            })
            .build();
        ids
    });
    measured.rect(ids[index]).expect("search dialog row layout")
}

fn settings_row_rect(rect: Rect, index: usize) -> Rect {
    flow_row_rect(rows_rect(rect), index, ScrollState::default())
}

fn setting_row_parts(row: Rect) -> (Rect, Rect) {
    let ((label, value), measured) = kama_ui::measure_layout(row, |ctx| {
        let mut label = BlockId(0);
        let mut value = BlockId(0);
        ctx.new()
            .row()
            .width(Size::Fill)
            .height(Size::Fill)
            .padding(3.0)
            .children(|ctx| {
                label = ctx
                    .new()
                    .width(Size::Pixels((row.width * 0.57 - 3.0).max(0.0)))
                    .height(Size::Fill)
                    .build();
                value = ctx.new().width(Size::Fill).height(Size::Fill).build();
            })
            .build();
        (label, value)
    });
    (
        measured.rect(label).expect("setting label layout"),
        measured.rect(value).expect("setting value layout"),
    )
}

fn setting_value_rect(row: Rect) -> Rect {
    setting_row_parts(row).1
}

fn setting_label_row(row: Rect, width_ratio: f32) -> Rect {
    let vertical = crate::ui_layout::column(
        row,
        &[
            crate::ui_layout::Item::height(5.0),
            crate::ui_layout::Item::height(20.0),
            crate::ui_layout::Item::fill(),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
        None,
    );
    crate::ui_layout::row(
        vertical[1],
        &[
            crate::ui_layout::Item::width(8.0),
            crate::ui_layout::Item::width((row.width * width_ratio - 12.0).max(0.0)),
            crate::ui_layout::Item::fill(),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
    )[1]
}

fn setting_label_rect(row: Rect) -> Rect {
    setting_label_row(row, 0.55)
}

fn keybind_label_rect(row: Rect) -> Rect {
    setting_label_row(row, 0.62)
}

fn empty_results_rect(body: Rect) -> Rect {
    let vertical = crate::ui_layout::column(
        body,
        &[
            crate::ui_layout::Item::height(6.0),
            crate::ui_layout::Item::height(24.0),
            crate::ui_layout::Item::fill(),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
        None,
    );
    crate::ui_layout::row(
        vertical[1],
        &[
            crate::ui_layout::Item::width(8.0),
            crate::ui_layout::Item::fill(),
            crate::ui_layout::Item::width(8.0),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
    )[1]
}

fn setting_accent_rects(value: Rect) -> (Rect, Rect) {
    let ((text, swatch), measured) = kama_ui::measure_layout(value, |ctx| {
        let mut text = BlockId(0);
        let mut swatch = BlockId(0);
        ctx.new()
            .row()
            .width(Size::Fill)
            .height(Size::Fill)
            .gap(4.0)
            .children(|ctx| {
                text = ctx.new().width(Size::Fill).height(Size::Fill).build();
                swatch = ctx
                    .new()
                    .width(Size::Pixels(30.0))
                    .height(Size::Fill)
                    .build();
            })
            .build();
        (text, swatch)
    });
    (
        measured.rect(text).expect("accent text layout"),
        measured.rect(swatch).expect("accent swatch layout"),
    )
}

fn keybind_row_parts(row: Rect) -> (Rect, Rect) {
    let ((label, value), measured) = kama_ui::measure_layout(row, |ctx| {
        let mut label = BlockId(0);
        let mut value = BlockId(0);
        ctx.new()
            .row()
            .width(Size::Fill)
            .height(Size::Fill)
            .padding(3.0)
            .children(|ctx| {
                label = ctx
                    .new()
                    .width(Size::Pixels((row.width * 0.62 - 3.0).max(0.0)))
                    .height(Size::Fill)
                    .build();
                value = ctx.new().width(Size::Fill).height(Size::Fill).build();
            })
            .build();
        (label, value)
    });
    (
        measured.rect(label).expect("keybind label layout"),
        measured.rect(value).expect("keybind value layout"),
    )
}

struct SearchDialogState {
    query: TextEdit,
    selected: usize,
    scroll: ScrollState,
    closed: bool,
}

impl SearchDialogState {
    fn new() -> Self {
        let mut query = TextEdit::single_line("");
        query.set_focused(true);
        Self {
            query,
            selected: 0,
            scroll: ScrollState::default(),
            closed: false,
        }
    }

    fn move_selection(&mut self, count: usize, delta: isize) {
        if count == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(count as isize) as usize;
    }

    fn edit_key(&mut self, event: &KeyEvent, modifiers: ModifiersState) -> (bool, bool) {
        self.query.set_focused(true);
        let response = self.query.handle_key(event, modifiers);
        if response.changed {
            self.selected = 0;
            self.scroll.offset = 0.0;
        }
        (response.changed, response.handled)
    }

    fn edit_ime(&mut self, event: &Ime) -> (bool, bool) {
        let response = self.query.handle_ime(event);
        if response.changed {
            self.selected = 0;
            self.scroll.offset = 0.0;
        }
        (response.changed, response.handled)
    }

    fn close_if_outside(&mut self, rect: Rect, point: [f32; 2]) -> bool {
        if search_dialog_layout(rect).close.contains(point) || !rect.contains(point) {
            self.closed = true;
            return true;
        }
        false
    }

    fn ensure_selected_visible(&mut self, body_height: f32, count: usize) {
        if count == 0 {
            self.scroll.offset = 0.0;
            self.selected = 0;
            return;
        }
        self.selected = self.selected.min(count - 1);
        let top = self.selected as f32 * ROW_H;
        let bottom = top + ROW_H;
        if top < self.scroll.offset {
            self.scroll.offset = top;
        }
        if bottom > self.scroll.offset + body_height {
            self.scroll.offset = (bottom - body_height).max(0.0);
        }
        self.scroll.offset = self
            .scroll
            .offset
            .min((count as f32 * ROW_H - body_height).max(0.0));
    }
}

pub(crate) struct SettingsDialog {
    search: SearchDialogState,
    opacity: f32,
    dark_accent: ColorPicker,
    light_accent: ColorPicker,
    theme_combo: ComboBox,
    brightness: Slider,
    accent_mixing: Slider,
    reveal_strength: Slider,
    reveal_accent_mix: Slider,
    plugin_paths: TextEdit,
}

impl SettingsDialog {
    pub(crate) fn new(plugin_paths: &str) -> Self {
        Self {
            search: SearchDialogState::new(),
            opacity: 0.0,
            dark_accent: ColorPicker::new(Color::rgba8(
                theme::dark_accent_rgba8()[0],
                theme::dark_accent_rgba8()[1],
                theme::dark_accent_rgba8()[2],
                theme::dark_accent_rgba8()[3],
            )),
            light_accent: ColorPicker::new(Color::rgba8(
                theme::light_accent_rgba8()[0],
                theme::light_accent_rgba8()[1],
                theme::light_accent_rgba8()[2],
                theme::light_accent_rgba8()[3],
            )),
            theme_combo: ComboBox::new(theme_index(theme::theme())),
            brightness: Slider::new(theme::brightness()),
            accent_mixing: Slider::new(theme::accent_mixing()),
            reveal_strength: Slider::new(kama_ui::reveal_strength()),
            reveal_accent_mix: Slider::new(kama_ui::reveal_accent_mix()),
            plugin_paths: TextEdit::single_line(plugin_paths),
        }
    }
    pub(crate) fn plugin_paths(&self) -> &str {
        self.plugin_paths.text()
    }
    pub(crate) fn is_closed(&self) -> bool {
        self.search.closed && self.opacity <= 0.001
    }
    pub(crate) fn tick(&mut self, dt: f32) {
        let target = if self.search.closed { 0.0 } else { 1.0 };
        let step = 1.0 - (-30.0 * dt).exp();
        self.opacity += (target - self.opacity) * step;
        if (self.opacity - target).abs() < 0.001 {
            self.opacity = target;
        }
        self.search.query.tick(dt);
        self.dark_accent.tick(dt);
        self.light_accent.tick(dt);
        self.theme_combo.tick(dt);
        self.brightness.tick(dt);
        self.accent_mixing.tick(dt);
        self.reveal_strength.tick(dt);
        self.reveal_accent_mix.tick(dt);
        self.plugin_paths.tick(dt);
    }
    pub(crate) fn is_animating(&self) -> bool {
        (self.opacity - if self.search.closed { 0.0 } else { 1.0 }).abs() > 0.001
            || self.search.query.is_animating()
            || self.dark_accent.is_animating()
            || self.light_accent.is_animating()
            || self.theme_combo.is_animating()
            || self.brightness.is_animating()
            || self.accent_mixing.is_animating()
            || self.reveal_strength.is_animating()
            || self.reveal_accent_mix.is_animating()
            || self.plugin_paths.is_animating()
    }
    pub(crate) fn sync_textures(&mut self, renderer: &mut Renderer) -> Result<()> {
        self.dark_accent.sync_textures(renderer)?;
        self.light_accent.sync_textures(renderer)
    }

    pub(crate) fn build(
        &mut self,
        ctx: &mut kama_ui::BuildCtx,
        width: f32,
        height: f32,
        chevron: IconId,
    ) {
        dialog::build_shell(
            ctx,
            "settings-scrim",
            "settings-dialog-shell",
            Rect::new(0.0, 0.0, width, height),
            settings_rect(width, height),
            self.opacity,
            |_| {},
        );
        kama_ui::ui!(ctx, {
            Rect("settings-overlay-root", Rect::new(0.0, 0.0, width, height)) {
                overlay; overflow_visible; opacity: self.opacity;
                @rust {
                let rect = settings_rect(width, height);
                dialog_frame(ctx, "settings-dialog", rect, "Settings");
                let search = search_rect(rect);
                self.search.query.build(
                    ctx,
                    "settings-search",
                    search,
                    "Search settings…",
                    component_style(),
                );
                let entries = filtered_settings(self.search.query.text());
                if entries.is_empty() {
                    ui_text!(
                        ctx,
                        "settings-no-results",
                        empty_results_rect(rows_rect(rect)),
                        10.5,
                        theme::popup_muted(),
                        "No fuzzy matches",
                    );
                    return;
                }
                let selected = self.search.selected.min(entries.len() - 1);
                let mut dark_accent_swatch = None;
                let mut light_accent_swatch = None;
                for (index, entry) in entries.iter().copied().enumerate() {
                    let row = settings_row_rect(rect, index);
                    let active = index == selected;
                    kama_ui::ui!(ctx, {
                        Rect(("settings-row", index), row) {
                            fill: if active { theme::accent_hover() } else { Color::TRANSPARENT };
                            border: 1; border_color: if active { theme::accent() } else { Color::TRANSPARENT };
                            border_radius: 5.0; interactive;
                        }
                    });
                    let (name, value) = match entry {
                        SettingEntry::Theme => (
                            "Theme preset",
                            match theme::theme() {
                                ThemePreset::System => "System".to_string(),
                                ThemePreset::Light => "Light".to_string(),
                                ThemePreset::Dark => "Dark".to_string(),
                            },
                        ),
                        SettingEntry::DarkAccent => {
                            let rgba = theme::dark_accent_rgba8();
                            (
                                "Dark accent color",
                                format!("#{:02X}{:02X}{:02X}{:02X}", rgba[0], rgba[1], rgba[2], rgba[3]),
                            )
                        }
                        SettingEntry::LightAccent => {
                            let rgba = theme::light_accent_rgba8();
                            (
                                "Light accent color",
                                format!("#{:02X}{:02X}{:02X}{:02X}", rgba[0], rgba[1], rgba[2], rgba[3]),
                            )
                        }
                        SettingEntry::Brightness => ("Brightness", String::new()),
                        SettingEntry::AccentMixing => ("Accent mixing", String::new()),
                        SettingEntry::RevealStrength => ("Reveal strength", String::new()),
                        SettingEntry::RevealAccentMix => ("Reveal accent mix", String::new()),
                        SettingEntry::RoundedCorners => (
                            "Rounded corners",
                            if kama_ui::rounded_corners_enabled() { "On" } else { "Off" }.to_string(),
                        ),
                        SettingEntry::HardwareDecoding => (
                            "Hardware decoding",
                            if hardware_decoding_enabled() { "On" } else { "Off" }.to_string(),
                        ),
                        SettingEntry::PluginPaths => ("Plugin paths", String::new()),
                    };
                    ui_text!(
                        ctx,
                        ("settings-name", index),
                        setting_label_rect(row),
                        10.5,
                        if active { theme::accent_text() } else { theme::popup_text() },
                        name,
                    );
                    let value_rect = setting_value_rect(row);
                    match entry {
                        SettingEntry::Theme => {
                            self.theme_combo.set_selected(theme_index(theme::theme()));
                            self.theme_combo.build_control(
                                ctx,
                                "settings-theme-value",
                                value_rect,
                                &THEME_OPTIONS,
                                chevron,
                                component_style(),
                            );
                        }
                        SettingEntry::DarkAccent | SettingEntry::LightAccent => {
                            let (accent_value, swatch) = setting_accent_rects(value_rect);
                            kama_ui::ui!(ctx, {
                                Rect(("settings-accent-value", index), accent_value) {
                                    fill: theme::control(); border: 1; border_color: theme::line(); border_radius: 4.0;
                                    padding: 6.0; font_size: 10.0; text_color: theme::popup_text(); text: value;
                                }
                            });
                            match entry {
                                SettingEntry::DarkAccent => dark_accent_swatch = Some(swatch),
                                SettingEntry::LightAccent => light_accent_swatch = Some(swatch),
                                _ => unreachable!(),
                            }
                        }
                        SettingEntry::Brightness => {
                            self.brightness.build(ctx, "settings-brightness", value_rect, component_style());
                        }
                        SettingEntry::AccentMixing => {
                            self.accent_mixing.build(ctx, "settings-accent-mixing", value_rect, component_style());
                        }
                        SettingEntry::RevealStrength => {
                            self.reveal_strength.build(ctx, "settings-reveal-strength", value_rect, component_style());
                        }
                        SettingEntry::RevealAccentMix => {
                            self.reveal_accent_mix.build(ctx, "settings-reveal-accent-mix", value_rect, component_style());
                        }
                        SettingEntry::RoundedCorners | SettingEntry::HardwareDecoding => {
                            kama_ui::ui!(ctx, {
                                Rect(("settings-toggle-value", index), value_rect) {
                                    fill: theme::control(); border: 1; border_color: theme::line(); border_radius: 4.0;
                                    padding: 6.0; font_size: 10.0; text_color: theme::popup_text(); text: value;
                                }
                            });
                        }
                        SettingEntry::PluginPaths => {
                            self.plugin_paths.build(
                                ctx,
                                "settings-plugin-paths",
                                value_rect,
                                "folder;folder;…",
                                component_style(),
                            );
                        }
                    }
                }


                if let Some(swatch) = dark_accent_swatch {
                    self.dark_accent.build_in(
                        ctx,
                        "settings-dark-accent",
                        swatch,
                        rect,
                        component_style(),
                    );
                }
                if let Some(swatch) = light_accent_swatch {
                    self.light_accent.build_in(
                        ctx,
                        "settings-light-accent",
                        swatch,
                        rect,
                        component_style(),
                    );
                }


                if let Some(index) = entries.iter().position(|entry| *entry == SettingEntry::Theme) {
                    self.theme_combo.build_popup(
                        ctx,
                        "settings-theme-value",
                        setting_value_rect(settings_row_rect(rect, index)),
                        &THEME_OPTIONS,
                        component_style(),
                    );
                }
                }
            }
        });
    }

    fn activate(&mut self, entry: SettingEntry) {
        match entry {
            SettingEntry::Theme => {
                self.plugin_paths.set_focused(false);
                self.close_accent_pickers();
                self.theme_combo.toggle();
            }
            SettingEntry::DarkAccent => {
                self.theme_combo.close();
                self.search.query.set_focused(false);
                self.plugin_paths.set_focused(false);
                self.light_accent.close();
                self.dark_accent.open_and_focus_hex();
            }
            SettingEntry::LightAccent => {
                self.theme_combo.close();
                self.search.query.set_focused(false);
                self.plugin_paths.set_focused(false);
                self.dark_accent.close();
                self.light_accent.open_and_focus_hex();
            }
            SettingEntry::Brightness
            | SettingEntry::AccentMixing
            | SettingEntry::RevealStrength
            | SettingEntry::RevealAccentMix => {
                self.theme_combo.close();
                self.search.query.set_focused(false);
                self.plugin_paths.set_focused(false);
                self.dark_accent.close();
                self.light_accent.close();
            }
            SettingEntry::RoundedCorners => {
                self.theme_combo.close();
                self.search.query.set_focused(false);
                self.plugin_paths.set_focused(false);
                self.dark_accent.close();
                self.light_accent.close();
                kama_ui::set_rounded_corners_enabled(!kama_ui::rounded_corners_enabled());
            }
            SettingEntry::HardwareDecoding => {
                self.theme_combo.close();
                self.search.query.set_focused(false);
                self.plugin_paths.set_focused(false);
                self.dark_accent.close();
                self.light_accent.close();
                set_hardware_decoding_enabled(!hardware_decoding_enabled());
            }
            SettingEntry::PluginPaths => {
                self.theme_combo.close();
                self.search.query.set_focused(false);
                self.dark_accent.close();
                self.light_accent.close();
                self.plugin_paths.set_focused(true);
            }
        }
    }

    pub(crate) fn pointer_pressed(
        &mut self,
        width: f32,
        height: f32,
        point: [f32; 2],
        modifiers: ModifiersState,
    ) -> bool {
        let rect = settings_rect(width, height);
        let entries = filtered_settings(self.search.query.text());
        if let Some(index) = entries
            .iter()
            .position(|entry| *entry == SettingEntry::Theme)
        {
            let value = setting_value_rect(settings_row_rect(rect, index));
            if let Some(option) = self
                .theme_combo
                .option_at(value, point, THEME_OPTIONS.len())
            {
                self.theme_combo.select(option, true);
                theme::set_theme(theme_at_index(option));
                self.search.selected = index;
                self.search.query.set_focused(false);
                self.plugin_paths.set_focused(false);
                self.close_accent_pickers();
                return true;
            }
            if value.contains(point) {
                self.search.selected = index;
                self.search.query.set_focused(false);
                self.plugin_paths.set_focused(false);
                self.close_accent_pickers();
                self.theme_combo.toggle();
                return true;
            }
            if self.theme_combo.is_open()
                && !self
                    .theme_combo
                    .popup_contains(value, point, THEME_OPTIONS.len())
            {
                self.theme_combo.close();
            }
        }

        for entry in [SettingEntry::DarkAccent, SettingEntry::LightAccent] {
            let Some(index) = entries.iter().position(|candidate| *candidate == entry) else {
                continue;
            };
            let value = setting_value_rect(settings_row_rect(rect, index));
            let (_, swatch) = setting_accent_rects(value);
            let handled = match entry {
                SettingEntry::DarkAccent => self
                    .dark_accent
                    .pointer_pressed_in(swatch, rect, point, modifiers),
                SettingEntry::LightAccent => self
                    .light_accent
                    .pointer_pressed_in(swatch, rect, point, modifiers),
                _ => unreachable!(),
            };
            if handled {
                self.search.selected = index;
                self.search.query.set_focused(false);
                self.plugin_paths.set_focused(false);
                self.apply_accent(entry);
                return true;
            }
        }
        if let Some(index) = entries
            .iter()
            .position(|entry| *entry == SettingEntry::Brightness)
        {
            let value = setting_value_rect(settings_row_rect(rect, index));
            if self.brightness.pointer_pressed(value, point) {
                self.search.selected = index;
                self.search.query.set_focused(false);
                self.plugin_paths.set_focused(false);
                self.close_accent_pickers();
                theme::set_brightness(self.brightness.value());
                return true;
            }
        }
        if let Some(index) = entries
            .iter()
            .position(|entry| *entry == SettingEntry::AccentMixing)
        {
            let value = setting_value_rect(settings_row_rect(rect, index));
            if self.accent_mixing.pointer_pressed(value, point) {
                self.search.selected = index;
                self.search.query.set_focused(false);
                self.plugin_paths.set_focused(false);
                self.close_accent_pickers();
                theme::set_accent_mixing(self.accent_mixing.value());
                return true;
            }
        }
        if let Some(index) = entries
            .iter()
            .position(|entry| *entry == SettingEntry::RevealStrength)
        {
            let value = setting_value_rect(settings_row_rect(rect, index));
            if self.reveal_strength.pointer_pressed(value, point) {
                self.search.selected = index;
                self.search.query.set_focused(false);
                self.plugin_paths.set_focused(false);
                self.close_accent_pickers();
                kama_ui::set_reveal_strength(self.reveal_strength.value());
                return true;
            }
        }
        if let Some(index) = entries
            .iter()
            .position(|entry| *entry == SettingEntry::RevealAccentMix)
        {
            let value = setting_value_rect(settings_row_rect(rect, index));
            if self.reveal_accent_mix.pointer_pressed(value, point) {
                self.search.selected = index;
                self.search.query.set_focused(false);
                self.plugin_paths.set_focused(false);
                self.close_accent_pickers();
                kama_ui::set_reveal_accent_mix(self.reveal_accent_mix.value());
                return true;
            }
        }
        if let Some(index) = entries
            .iter()
            .position(|entry| *entry == SettingEntry::PluginPaths)
        {
            let value = setting_value_rect(settings_row_rect(rect, index));
            if self.plugin_paths.pointer_pressed(value, point, modifiers) {
                self.search.selected = index;
                self.search.query.set_focused(false);
                return true;
            }
        }
        if self.search.close_if_outside(rect, point) {
            return true;
        }
        if self
            .search
            .query
            .pointer_pressed(search_rect(rect), point, modifiers)
        {
            return true;
        }
        for (index, entry) in entries.into_iter().enumerate() {
            if settings_row_rect(rect, index).contains(point) {
                self.search.selected = index;
                self.activate(entry);
                return true;
            }
        }
        true
    }

    pub(crate) fn pointer_moved(&mut self, width: f32, height: f32, point: [f32; 2]) -> bool {
        let rect = settings_rect(width, height);
        let entries = filtered_settings(self.search.query.text());
        let mut changed = false;
        if self.brightness.pointer_moved(point) {
            theme::set_brightness(self.brightness.value());
            changed = true;
        }
        if self.accent_mixing.pointer_moved(point) {
            theme::set_accent_mixing(self.accent_mixing.value());
            changed = true;
        }
        if self.reveal_strength.pointer_moved(point) {
            kama_ui::set_reveal_strength(self.reveal_strength.value());
            changed = true;
        }
        if self.reveal_accent_mix.pointer_moved(point) {
            kama_ui::set_reveal_accent_mix(self.reveal_accent_mix.value());
            changed = true;
        }
        if self.plugin_paths.is_focused() {
            changed |= self.plugin_paths.pointer_moved(point);
        }
        for entry in [SettingEntry::DarkAccent, SettingEntry::LightAccent] {
            let Some(index) = entries.iter().position(|candidate| *candidate == entry) else {
                continue;
            };
            let value = setting_value_rect(settings_row_rect(rect, index));
            let (_, swatch) = setting_accent_rects(value);
            let accent_changed = match entry {
                SettingEntry::DarkAccent => self.dark_accent.pointer_moved_in(swatch, rect, point),
                SettingEntry::LightAccent => {
                    self.light_accent.pointer_moved_in(swatch, rect, point)
                }
                _ => unreachable!(),
            };
            if accent_changed {
                self.apply_accent(entry);
            }
            changed |= accent_changed;
        }
        changed
    }

    pub(crate) fn pointer_released(&mut self) -> bool {
        self.dark_accent.pointer_released()
            | self.light_accent.pointer_released()
            | self.brightness.pointer_released()
            | self.accent_mixing.pointer_released()
            | self.reveal_strength.pointer_released()
            | self.reveal_accent_mix.pointer_released()
            | self.plugin_paths.pointer_released()
    }

    pub(crate) fn handle_key(&mut self, event: &KeyEvent, modifiers: ModifiersState) -> bool {
        if event.state != ElementState::Pressed {
            return false;
        }
        if self.plugin_paths.is_focused() {
            if matches!(
                &event.logical_key,
                Key::Named(NamedKey::Escape | NamedKey::Enter)
            ) {
                self.plugin_paths.set_focused(false);
                self.search.query.set_focused(true);
                return true;
            }
            return self.plugin_paths.handle_key(event, modifiers).handled;
        }
        if self.dark_accent.is_editing() || self.light_accent.is_editing() {
            if matches!(&event.logical_key, Key::Named(NamedKey::Escape)) {
                self.close_accent_pickers();
                self.search.query.set_focused(true);
                return true;
            }
            let (entry, changed) = if self.dark_accent.is_editing() {
                (
                    SettingEntry::DarkAccent,
                    self.dark_accent.handle_key(event, modifiers),
                )
            } else {
                (
                    SettingEntry::LightAccent,
                    self.light_accent.handle_key(event, modifiers),
                )
            };
            if changed {
                self.apply_accent(entry);
            }
            return true;
        }
        let entries = filtered_settings(self.search.query.text());
        if self.theme_combo.is_open() {
            match &event.logical_key {
                Key::Named(NamedKey::Escape | NamedKey::Enter) => {
                    self.theme_combo.close();
                    return true;
                }
                Key::Named(NamedKey::ArrowDown) => {
                    let selected = (self.theme_combo.selected() + 1) % THEME_OPTIONS.len();
                    self.theme_combo.set_selected(selected);
                    theme::set_theme(theme_at_index(selected));
                    return true;
                }
                Key::Named(NamedKey::ArrowUp) => {
                    let selected = (self.theme_combo.selected() + THEME_OPTIONS.len() - 1)
                        % THEME_OPTIONS.len();
                    self.theme_combo.set_selected(selected);
                    theme::set_theme(theme_at_index(selected));
                    return true;
                }
                _ => {}
            }
        }
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.search.closed = true;
                true
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.search.move_selection(entries.len(), 1);
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.search.move_selection(entries.len(), -1);
                true
            }
            Key::Named(NamedKey::Enter) => {
                if let Some(entry) = entries
                    .get(self.search.selected.min(entries.len().saturating_sub(1)))
                    .copied()
                {
                    self.activate(entry);
                }
                true
            }
            _ => {
                let (changed, handled) = self.search.edit_key(event, modifiers);
                if changed {
                    self.close_accent_pickers();
                    self.plugin_paths.set_focused(false);
                }
                handled
            }
        }
    }

    pub(crate) fn handle_ime(&mut self, event: &Ime) -> bool {
        if self.plugin_paths.is_focused() {
            return self.plugin_paths.handle_ime(event).handled;
        }
        if self.dark_accent.is_editing() || self.light_accent.is_editing() {
            let (entry, changed) = if self.dark_accent.is_editing() {
                (SettingEntry::DarkAccent, self.dark_accent.handle_ime(event))
            } else {
                (
                    SettingEntry::LightAccent,
                    self.light_accent.handle_ime(event),
                )
            };
            if changed {
                self.apply_accent(entry);
            }
            return changed;
        }
        let (changed, handled) = self.search.edit_ime(event);
        if changed {
            self.close_accent_pickers();
            self.plugin_paths.set_focused(false);
        }
        handled
    }

    pub(crate) fn caret_rect(&self, width: f32, height: f32) -> Option<Rect> {
        let rect = settings_rect(width, height);
        let entries = filtered_settings(self.search.query.text());
        if let Some(index) = entries
            .iter()
            .position(|entry| *entry == SettingEntry::PluginPaths)
        {
            if self.plugin_paths.is_focused() {
                return Some(
                    self.plugin_paths
                        .caret_rect(setting_value_rect(settings_row_rect(rect, index))),
                );
            }
        }
        for entry in [SettingEntry::DarkAccent, SettingEntry::LightAccent] {
            let Some(index) = entries.iter().position(|candidate| *candidate == entry) else {
                continue;
            };
            let value = setting_value_rect(settings_row_rect(rect, index));
            let (_, swatch) = setting_accent_rects(value);
            let caret = match entry {
                SettingEntry::DarkAccent => self.dark_accent.caret_rect_in(swatch, rect),
                SettingEntry::LightAccent => self.light_accent.caret_rect_in(swatch, rect),
                _ => unreachable!(),
            };
            if caret.is_some() {
                return caret;
            }
        }
        self.search
            .query
            .is_focused()
            .then(|| self.search.query.caret_rect(search_rect(rect)))
    }

    fn close_accent_pickers(&mut self) {
        self.dark_accent.close();
        self.light_accent.close();
    }

    fn apply_accent(&self, entry: SettingEntry) {
        let color = match entry {
            SettingEntry::DarkAccent => self.dark_accent.color(),
            SettingEntry::LightAccent => self.light_accent.color(),
            _ => return,
        };
        let rgba = [
            (color.r.clamp(0.0, 1.0) * 255.0).round() as u8,
            (color.g.clamp(0.0, 1.0) * 255.0).round() as u8,
            (color.b.clamp(0.0, 1.0) * 255.0).round() as u8,
            0xff,
        ];
        match entry {
            SettingEntry::DarkAccent => theme::set_dark_accent_rgba8(rgba),
            SettingEntry::LightAccent => theme::set_light_accent_rgba8(rgba),
            _ => {}
        }
    }
}

fn filtered_keybind_indices(registry: &CommandRegistry, query: &str) -> Vec<usize> {
    if query.trim().is_empty() {
        return (0..registry.definitions().len()).collect();
    }
    let mut scored = registry
        .definitions()
        .iter()
        .enumerate()
        .filter_map(|(index, definition)| {
            let shortcut = definition
                .shortcut
                .map_or_else(|| "unbound".to_string(), |binding| binding.to_string());
            fuzzy_score(
                query,
                &format!(
                    "{} {} {} {}",
                    definition.label, definition.description, definition.id, shortcut
                ),
            )
            .map(|score| (score, index))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0).then_with(|| {
            registry.definitions()[a.1]
                .label
                .cmp(&registry.definitions()[b.1].label)
        })
    });
    scored.into_iter().map(|(_, index)| index).collect()
}

pub(crate) struct KeybindsDialog {
    search: SearchDialogState,
    opacity: f32,
    capturing: Option<String>,
}

impl KeybindsDialog {
    pub(crate) fn new() -> Self {
        Self {
            search: SearchDialogState::new(),
            opacity: 0.0,
            capturing: None,
        }
    }
    pub(crate) fn is_closed(&self) -> bool {
        self.search.closed && self.opacity <= 0.001
    }
    pub(crate) fn tick(&mut self, dt: f32) {
        let target = if self.search.closed { 0.0 } else { 1.0 };
        let step = 1.0 - (-30.0 * dt).exp();
        self.opacity += (target - self.opacity) * step;
        if (self.opacity - target).abs() < 0.001 {
            self.opacity = target;
        }
        self.search.query.tick(dt);
    }
    pub(crate) fn is_animating(&self) -> bool {
        (self.opacity - if self.search.closed { 0.0 } else { 1.0 }).abs() > 0.001
            || self.search.query.is_animating()
    }

    fn body(&self, rect: Rect) -> Rect {
        rows_rect(rect)
    }

    pub(crate) fn build(
        &mut self,
        ctx: &mut kama_ui::BuildCtx,
        width: f32,
        height: f32,
        registry: &CommandRegistry,
    ) {
        dialog::build_shell(
            ctx,
            "keybinds-scrim",
            "keybinds-dialog-shell",
            Rect::new(0.0, 0.0, width, height),
            keybinds_rect(width, height),
            self.opacity,
            |_| {},
        );
        kama_ui::ui!(ctx, {
            Rect("keybinds-overlay-root", Rect::new(0.0, 0.0, width, height)) {
                overlay; overflow_visible; opacity: self.opacity;
                @rust {
                let rect = keybinds_rect(width, height);
                dialog_frame(ctx, "keybinds-dialog", rect, "Keybinds");
                self.search.query.build(
                    ctx,
                    "keybinds-search",
                    search_rect(rect),
                    "Search commands…",
                    component_style(),
                );
                let body = self.body(rect);
                let indices = filtered_keybind_indices(registry, self.search.query.text());
                if indices.is_empty() {
                    ui_text!(
                        ctx,
                        "keybinds-no-results",
                        empty_results_rect(body),
                        10.5,
                        theme::popup_muted(),
                        "No fuzzy matches",
                    );
                }
                for (visible_index, definition_index) in indices.into_iter().enumerate() {
                    let definition = &registry.definitions()[definition_index];
                    let row = flow_row_rect(body, visible_index, self.search.scroll);
                    if row.bottom() < body.y || row.y > body.bottom() {
                        continue;
                    }
                    let selected = visible_index == self.search.selected;
                    kama_ui::ui!(ctx, {
                        Rect(("keybind-row", &definition.id), row) {
                            fill: if selected { theme::accent_hover() } else { Color::TRANSPARENT };
                            border: 1; border_color: if selected { theme::accent() } else { Color::TRANSPARENT };
                            border_radius: 5.0; interactive;
                        }
                    });
                    kama_ui::ui!(ctx, {
                        Rect(("keybind-label", &definition.id), keybind_label_rect(row)) {
                            font_size: 10.5; text_color: if selected { theme::accent_text() } else { theme::popup_text() }; text: &definition.label;
                            tooltip: &definition.description;
                        }
                    });
                    let value = if self.capturing.as_deref() == Some(definition.id.as_str()) {
                        "Press shortcut…  (⌫ unbind)".to_string()
                    } else {
                        definition
                            .shortcut
                            .map_or_else(|| "Unbound".into(), |binding| binding.to_string())
                    };
                    kama_ui::ui!(ctx, {
                        Rect(("keybind-value", &definition.id), keybind_row_parts(row).1) {
                            fill: if self.capturing.as_deref() == Some(definition.id.as_str()) {
                                theme::focused()
                            } else { theme::control() };
                            border: 1; border_color: theme::line(); border_radius: 4.0; padding: 6.0;
                            font_size: 9.5; text_color: theme::popup_text(); text: value;
                        }
                    });
                }
                ui_text!(
                    ctx,
                    "keybind-help",
                    search_dialog_layout(rect).help,
                    9.0,
                    theme::popup_dim(),
                    "↑↓ select    ↵ rebind    esc cancel/close",
                );
                }
            }
        });
    }

    pub(crate) fn pointer_pressed(
        &mut self,
        width: f32,
        height: f32,
        point: [f32; 2],
        registry: &mut CommandRegistry,
    ) -> bool {
        let rect = keybinds_rect(width, height);
        if self.search.close_if_outside(rect, point) {
            return true;
        }
        if self
            .search
            .query
            .pointer_pressed(search_rect(rect), point, ModifiersState::empty())
        {
            return true;
        }
        let body = self.body(rect);
        if !body.contains(point) {
            return true;
        }
        let indices = filtered_keybind_indices(registry, self.search.query.text());
        let Some(visible_index) = (0..indices.len())
            .find(|&index| flow_row_rect(body, index, self.search.scroll).contains(point))
        else {
            return true;
        };
        let definition_index = indices[visible_index];
        self.search.selected = visible_index;
        self.capturing = Some(registry.definitions()[definition_index].id.clone());
        self.search.query.set_focused(false);
        true
    }

    pub(crate) fn scroll(
        &mut self,
        width: f32,
        height: f32,
        point: [f32; 2],
        delta: [f32; 2],
        registry: &CommandRegistry,
    ) -> bool {
        let rect = keybinds_rect(width, height);
        if !rect.contains(point) {
            return false;
        }
        let body_h = self.body(rect).height;
        let count = filtered_keybind_indices(registry, self.search.query.text()).len();
        self.search
            .scroll
            .scroll_by(-delta[1], (count as f32 * ROW_H - body_h).max(0.0))
    }

    pub(crate) fn handle_key(
        &mut self,
        event: &KeyEvent,
        modifiers: ModifiersState,
        registry: &mut CommandRegistry,
    ) -> bool {
        if event.state != ElementState::Pressed {
            return false;
        }
        if let Some(id) = self.capturing.clone() {
            match &event.logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.capturing = None;
                    self.search.query.set_focused(true);
                }
                Key::Named(NamedKey::Backspace) | Key::Named(NamedKey::Delete) => {
                    registry.set_shortcut(&id, None);
                    self.capturing = None;
                    self.search.query.set_focused(true);
                }
                _ => {
                    if let Some(binding) = KeyBinding::from_event(event, modifiers) {
                        registry.set_shortcut(&id, Some(binding));
                        self.capturing = None;
                        self.search.query.set_focused(true);
                    }
                }
            }
            return true;
        }
        let body_height = KEYBINDS_H - HEADER_H - SEARCH_H - 33.0;
        let indices = filtered_keybind_indices(registry, self.search.query.text());
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.search.closed = true;
                true
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.search.move_selection(indices.len(), 1);
                self.search
                    .ensure_selected_visible(body_height, indices.len());
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.search.move_selection(indices.len(), -1);
                self.search
                    .ensure_selected_visible(body_height, indices.len());
                true
            }
            Key::Named(NamedKey::Enter) => {
                if let Some(&definition_index) =
                    indices.get(self.search.selected.min(indices.len().saturating_sub(1)))
                {
                    self.capturing = Some(registry.definitions()[definition_index].id.clone());
                    self.search.query.set_focused(false);
                }
                true
            }
            _ => self.search.edit_key(event, modifiers).1,
        }
    }

    pub(crate) fn handle_ime(&mut self, event: &Ime) -> bool {
        if self.capturing.is_some() {
            return false;
        }
        self.search.edit_ime(event).1
    }

    pub(crate) fn caret_rect(&self, width: f32, height: f32) -> Option<Rect> {
        self.search.query.is_focused().then(|| {
            self.search
                .query
                .caret_rect(search_rect(keybinds_rect(width, height)))
        })
    }
}
