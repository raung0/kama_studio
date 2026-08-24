use std::{collections::HashMap, path::PathBuf, sync::Arc};

use anyhow::Result;
use kama_ui::{
    components::{ColorPicker, ComboBox, Slider, TextEdit},
    Color, IconId, Rect, Renderer, ScrollState,
};
use serde::{Deserialize, Serialize};
use winit::{
    event::{ElementState, Ime, KeyEvent},
    keyboard::{Key, ModifiersState, NamedKey},
};

use crate::{
    app_modal::PopupAnimation,
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
    #[serde(default)]
    language: Option<String>,
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
    0.15
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
            language: None,
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
    let _ = crate::i18n::initialize(preferences.language.as_deref());
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
        language: crate::i18n::preference(),
    };
    let _ = atomic_write_json(&path(), &preferences);
}

const SETTINGS_W: f32 = 560.0;
const SETTINGS_H: f32 = 464.0;
const KEYBINDS_W: f32 = 680.0;
const KEYBINDS_H: f32 = 580.0;

#[derive(Clone, Copy)]
struct SearchRowRects {
    row: Rect,
    label: Rect,
    value: Rect,
    accent_text: Rect,
    accent_swatch: Rect,
}

#[derive(Clone)]
struct PreferenceDialogRects {
    shell: dialog::SearchDialogRects,
    items: Arc<[SearchRowRects]>,
}

#[derive(Clone, Copy, PartialEq)]
struct SearchRectsKey {
    rect: Rect,
    row_count: usize,
    scroll_offset: f32,
    label_ratio: f32,
}

fn centered(width: f32, height: f32, w: f32, h: f32) -> Rect {
    kama_ui::layout::centered(Rect::new(0.0, 0.0, width, height), w, h)
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

fn preference_dialog_rects(
    rect: Rect,
    row_count: usize,
    scroll: ScrollState,
    label_ratio: f32,
) -> PreferenceDialogRects {
    let shell = dialog::measure_search_dialog(rect, true, row_count, scroll);
    let label_weight = label_ratio.clamp(0.0, 1.0);
    let value_weight = (1.0 - label_weight).max(0.0);
    let items = shell
        .items
        .iter()
        .copied()
        .map(|row| {
            let inner = kama_ui::layout::inset(row, 3.0);
            let split = kama_ui::layout::row(
                inner,
                &[
                    kama_ui::layout::Item::fill_portion(label_weight),
                    kama_ui::layout::Item::fill_portion(value_weight),
                ],
                0.0,
                0.0,
                kama_ui::Align::Center,
            );
            let value_parts = kama_ui::layout::row(
                split[1],
                &[
                    kama_ui::layout::Item::fill(),
                    kama_ui::layout::Item::width(30.0),
                ],
                4.0,
                0.0,
                kama_ui::Align::Center,
            );
            SearchRowRects {
                row,
                label: split[0],
                value: split[1],
                accent_text: value_parts[0],
                accent_swatch: value_parts[1],
            }
        })
        .collect::<Vec<_>>()
        .into();
    PreferenceDialogRects { shell, items }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingEntry {
    Language,
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
            SettingEntry::Language,
            "Language System English locale translation i18n",
        ),
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

struct SearchDialogState {
    query: TextEdit,
    selected: usize,
    scroll: ScrollState,
    rect_cache: Option<(SearchRectsKey, PreferenceDialogRects)>,
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
            rect_cache: None,
            closed: false,
        }
    }

    fn rects(&mut self, rect: Rect, row_count: usize, label_ratio: f32) -> PreferenceDialogRects {
        let key = SearchRectsKey {
            rect,
            row_count,
            scroll_offset: self.scroll.offset,
            label_ratio,
        };
        if self
            .rect_cache
            .as_ref()
            .is_none_or(|(cached, _)| *cached != key)
        {
            self.rect_cache = Some((
                key,
                preference_dialog_rects(rect, row_count, self.scroll, label_ratio),
            ));
        }
        self.rect_cache
            .as_ref()
            .expect("search dialog rect cache")
            .1
            .clone()
    }

    fn cached_rects(&self) -> Option<&PreferenceDialogRects> {
        self.rect_cache.as_ref().map(|(_, rects)| rects)
    }

    fn invalidate_rects(&mut self) {
        self.rect_cache = None;
    }

    fn max_scroll(&self) -> f32 {
        self.cached_rects()
            .map_or(0.0, |rects| rects.shell.max_scroll)
    }

    fn scroll_by(&mut self, delta: f32) -> bool {
        let max_scroll = self.max_scroll();
        let changed = self.scroll.scroll_by(delta, max_scroll);
        if changed {
            self.invalidate_rects();
        }
        changed
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
            self.invalidate_rects();
        }
        (response.changed, response.handled)
    }

    fn edit_ime(&mut self, event: &Ime) -> (bool, bool) {
        let response = self.query.handle_ime(event);
        if response.changed {
            self.selected = 0;
            self.scroll.offset = 0.0;
            self.invalidate_rects();
        }
        (response.changed, response.handled)
    }

    fn close_if_outside(&mut self, rect: Rect, close: Rect, point: [f32; 2]) -> bool {
        if close.contains(point) || !rect.contains(point) {
            self.closed = true;
            return true;
        }
        false
    }

    fn ensure_selected_visible(&mut self) {
        let Some(rects) = self.cached_rects().cloned() else {
            return;
        };
        if rects.items.is_empty() {
            self.scroll.offset = 0.0;
            self.selected = 0;
            return;
        }
        self.selected = self.selected.min(rects.items.len() - 1);
        let selected = rects.items[self.selected].row;
        let viewport = rects.shell.rows;
        let max_scroll = rects.shell.max_scroll;
        let next = if selected.y < viewport.y {
            self.scroll.offset - (viewport.y - selected.y)
        } else if selected.bottom() > viewport.bottom() {
            self.scroll.offset + (selected.bottom() - viewport.bottom())
        } else {
            self.scroll.offset
        }
        .clamp(0.0, max_scroll);
        if (next - self.scroll.offset).abs() > f32::EPSILON {
            self.scroll.offset = next;
            self.invalidate_rects();
        }
    }
}

pub(crate) struct SettingsDialog {
    search: SearchDialogState,
    animation: PopupAnimation,
    dark_accent: ColorPicker,
    light_accent: ColorPicker,
    theme_combo: ComboBox,
    language_combo: ComboBox,
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
            animation: PopupAnimation::new(),
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
            language_combo: ComboBox::new(
                crate::i18n::language_options()
                    .iter()
                    .position(|option| option.language == crate::i18n::preference())
                    .unwrap_or_default(),
            ),
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
        self.search.closed && self.animation.finished(std::time::Instant::now())
    }
    pub(crate) fn tick(&mut self, dt: f32) {
        self.search.query.tick(dt);
        self.dark_accent.tick(dt);
        self.light_accent.tick(dt);
        self.theme_combo.tick(dt);
        self.language_combo.tick(dt);
        self.brightness.tick(dt);
        self.accent_mixing.tick(dt);
        self.reveal_strength.tick(dt);
        self.reveal_accent_mix.tick(dt);
        self.plugin_paths.tick(dt);
    }
    pub(crate) fn is_animating(&self) -> bool {
        self.animation.is_animating()
            || self.search.query.is_animating()
            || self.dark_accent.is_animating()
            || self.light_accent.is_animating()
            || self.theme_combo.is_animating()
            || self.language_combo.is_animating()
            || self.brightness.is_animating()
            || self.accent_mixing.is_animating()
            || self.reveal_strength.is_animating()
            || self.reveal_accent_mix.is_animating()
            || self.plugin_paths.is_animating()
    }
    pub(crate) fn restart_entry_animation(&mut self) {
        self.animation.restart();
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
        let rect = settings_rect(width, height);
        let entries = filtered_settings(self.search.query.text());
        let language_options = crate::i18n::language_options();
        let language_labels = language_options
            .iter()
            .map(|option| option.label.as_str())
            .collect::<Vec<_>>();
        let layout = self.search.rects(rect, entries.len(), 0.57);
        dialog::build_search_dialog(
            ctx,
            "settings-scrim",
            "settings-dialog-shell",
            "settings-overlay-root",
            Rect::new(0.0, 0.0, width, height),
            rect,
            self.animation.opacity(std::time::Instant::now()),
            &layout.shell,
            Some(&crate::i18n::text("settings-title")),
            &crate::i18n::text("settings-hint"),
            |ctx, _shell| {
                self.search.query.build(
                    ctx,
                    "settings-search",
                    layout.shell.search,
                    &crate::i18n::text("settings-search-placeholder"),
                    component_style(),
                );
                let mut dark_accent_swatch = None;
                let mut light_accent_swatch = None;
                ctx.with_clip(layout.shell.rows, |ctx| {
                    if entries.is_empty() {
                        ui_text!(
                            ctx,
                            "settings-no-results",
                            layout.shell.empty,
                            10.5,
                            theme::popup_muted(),
                            crate::i18n::text("settings-no-results"),
                        );
                    } else {
                        let selected = self.search.selected.min(entries.len() - 1);
                        for (index, entry) in entries.iter().copied().enumerate() {
                        let row = layout.items[index];
                        let active = index == selected;
                        kama_ui::ui!(ctx, {
                            Rect(("settings-row", index), row.row) {
                                fill: if active { theme::accent_hover() } else { Color::TRANSPARENT };
                                border: 1; border_color: if active { theme::accent() } else { Color::TRANSPARENT };
                                border_radius: 5.0; interactive;
                            }
                        });
                        let (name, value) = match entry {
                            SettingEntry::Language => (
                                crate::i18n::text("settings-language"),
                                language_options
                                    .get(self.language_combo.selected())
                                    .map(|option| option.label.clone())
                                    .unwrap_or_default(),
                            ),
                            SettingEntry::Theme => (
                                crate::i18n::text("settings-theme"),
                                match theme::theme() {
                                    ThemePreset::System => crate::i18n::text("settings-system"),
                                    ThemePreset::Light => crate::i18n::text("settings-light"),
                                    ThemePreset::Dark => crate::i18n::text("settings-dark"),
                                },
                            ),
                            SettingEntry::DarkAccent => {
                                let rgba = theme::dark_accent_rgba8();
                                (
                                    crate::i18n::text("settings-dark-accent"),
                                    format!("#{:02X}{:02X}{:02X}{:02X}", rgba[0], rgba[1], rgba[2], rgba[3]),
                                )
                            }
                            SettingEntry::LightAccent => {
                                let rgba = theme::light_accent_rgba8();
                                (
                                    crate::i18n::text("settings-light-accent"),
                                    format!("#{:02X}{:02X}{:02X}{:02X}", rgba[0], rgba[1], rgba[2], rgba[3]),
                                )
                            }
                            SettingEntry::Brightness => (crate::i18n::text("settings-brightness"), String::new()),
                            SettingEntry::AccentMixing => (crate::i18n::text("settings-accent-mixing"), String::new()),
                            SettingEntry::RevealStrength => (crate::i18n::text("settings-reveal-strength"), String::new()),
                            SettingEntry::RevealAccentMix => (crate::i18n::text("settings-reveal-accent-mix"), String::new()),
                            SettingEntry::RoundedCorners => (
                                crate::i18n::text("settings-rounded-corners"),
                                crate::i18n::text(if kama_ui::rounded_corners_enabled() { "settings-on" } else { "settings-off" }),
                            ),
                            SettingEntry::HardwareDecoding => (
                                crate::i18n::text("settings-hardware-decoding"),
                                crate::i18n::text(if hardware_decoding_enabled() { "settings-on" } else { "settings-off" }),
                            ),
                            SettingEntry::PluginPaths => (crate::i18n::text("settings-plugin-paths"), String::new()),
                        };
                        kama_ui::ui!(ctx, {
                            Rect(("settings-name", index), row.label) {
                                padding: 5.0; font_size: 10.5;
                                text_color: if active { theme::accent_text() } else { theme::popup_text() };
                                text: name;
                            }
                        });
                        match entry {
                            SettingEntry::Language => {
                                self.language_combo.build_control(
                                    ctx,
                                    "settings-language-value",
                                    row.value,
                                    &language_labels,
                                    chevron,
                                    component_style(),
                                );
                            }
                            SettingEntry::Theme => {
                                self.theme_combo.set_selected(theme_index(theme::theme()));
                                self.theme_combo.build_control(
                                    ctx,
                                    "settings-theme-value",
                                    row.value,
                                    &THEME_OPTIONS,
                                    chevron,
                                    component_style(),
                                );
                            }
                            SettingEntry::DarkAccent | SettingEntry::LightAccent => {
                                kama_ui::ui!(ctx, {
                                    Rect(("settings-accent-value", index), row.accent_text) {
                                        fill: theme::control();
                                        border: 1;
                                        border_color: theme::line();
                                        border_radius: 4.0;
                                        padding: 6.0;
                                        font_size: 10.0;
                                        text_color: theme::popup_text();
                                        text: value;
                                    }
                                });
                                match entry {
                                    SettingEntry::DarkAccent => dark_accent_swatch = Some(row.accent_swatch),
                                    SettingEntry::LightAccent => light_accent_swatch = Some(row.accent_swatch),
                                    _ => unreachable!(),
                                }
                            }
                            SettingEntry::Brightness => {
                                self.brightness.build(
                                    ctx,
                                    "settings-brightness",
                                    row.value,
                                    component_style(),
                                );
                            }
                            SettingEntry::AccentMixing => {
                                self.accent_mixing.build(
                                    ctx,
                                    "settings-accent-mixing",
                                    row.value,
                                    component_style(),
                                );
                            }
                            SettingEntry::RevealStrength => {
                                self.reveal_strength.build(
                                    ctx,
                                    "settings-reveal-strength",
                                    row.value,
                                    component_style(),
                                );
                            }
                            SettingEntry::RevealAccentMix => {
                                self.reveal_accent_mix.build(
                                    ctx,
                                    "settings-reveal-accent-mix",
                                    row.value,
                                    component_style(),
                                );
                            }
                            SettingEntry::RoundedCorners | SettingEntry::HardwareDecoding => {
                                kama_ui::ui!(ctx, {
                                    Rect(("settings-toggle-value", index), row.value) {
                                        fill: theme::control();
                                        border: 1;
                                        border_color: theme::line();
                                        border_radius: 4.0;
                                        padding: 6.0;
                                        font_size: 10.0;
                                        text_color: theme::popup_text();
                                        text: value;
                                    }
                                });
                            }
                            SettingEntry::PluginPaths => {
                                self.plugin_paths.build(
                                    ctx,
                                    "settings-plugin-paths",
                                    row.value,
                                    "folder;folder;…",
                                    component_style(),
                                );
                            }
                        }
                    }

                    }
                });

                if let Some(swatch) = dark_accent_swatch {
                    self.dark_accent.build_in(
                        ctx,
                        "settings-dark-accent",
                        swatch,
                        Rect::new(0.0, 0.0, width, height),
                        component_style(),
                    );
                }
                if let Some(swatch) = light_accent_swatch {
                    self.light_accent.build_in(
                        ctx,
                        "settings-light-accent",
                        swatch,
                        Rect::new(0.0, 0.0, width, height),
                        component_style(),
                    );
                }
                if let Some(index) = entries
                    .iter()
                    .position(|entry| *entry == SettingEntry::Theme)
                {
                    self.theme_combo.build_popup_in(
                        ctx,
                        "settings-theme-value",
                        layout.items[index].value,
                        &THEME_OPTIONS,
                        Rect::new(0.0, 0.0, width, height),
                        component_style(),
                    );
                }
                if let Some(index) = entries
                    .iter()
                    .position(|entry| *entry == SettingEntry::Language)
                {
                    self.language_combo.build_popup_in(
                        ctx,
                        "settings-language-value",
                        layout.items[index].value,
                        &language_labels,
                        Rect::new(0.0, 0.0, width, height),
                        component_style(),
                    );
                }
            },
        );
    }

    fn activate(&mut self, entry: SettingEntry) {
        match entry {
            SettingEntry::Language => {
                self.plugin_paths.set_focused(false);
                self.close_accent_pickers();
                self.theme_combo.close();
                self.language_combo.toggle();
            }
            SettingEntry::Theme => {
                self.language_combo.close();
                self.plugin_paths.set_focused(false);
                self.close_accent_pickers();
                self.theme_combo.toggle();
            }
            SettingEntry::DarkAccent => {
                self.language_combo.close();
                self.theme_combo.close();
                self.search.query.set_focused(false);
                self.plugin_paths.set_focused(false);
                self.light_accent.close();
                self.dark_accent.open_and_focus_hex();
            }
            SettingEntry::LightAccent => {
                self.language_combo.close();
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
                self.language_combo.close();
                self.theme_combo.close();
                self.search.query.set_focused(false);
                self.plugin_paths.set_focused(false);
                self.dark_accent.close();
                self.light_accent.close();
            }
            SettingEntry::RoundedCorners => {
                self.language_combo.close();
                self.theme_combo.close();
                self.search.query.set_focused(false);
                self.plugin_paths.set_focused(false);
                self.dark_accent.close();
                self.light_accent.close();
                kama_ui::set_rounded_corners_enabled(!kama_ui::rounded_corners_enabled());
            }
            SettingEntry::HardwareDecoding => {
                self.language_combo.close();
                self.theme_combo.close();
                self.search.query.set_focused(false);
                self.plugin_paths.set_focused(false);
                self.dark_accent.close();
                self.light_accent.close();
                set_hardware_decoding_enabled(!hardware_decoding_enabled());
            }
            SettingEntry::PluginPaths => {
                self.language_combo.close();
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
        let language_options = crate::i18n::language_options();
        let layout = self.search.rects(rect, entries.len(), 0.57);
        if let Some(index) = entries
            .iter()
            .position(|entry| *entry == SettingEntry::Language)
        {
            let value = layout.items[index].value;
            if let Some(option) =
                self.language_combo
                    .option_at(value, point, language_options.len())
            {
                self.language_combo.select(option, true);
                let _ = crate::i18n::set_language(language_options[option].language.as_deref());
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
                self.language_combo.toggle();
                return true;
            }
            if self.language_combo.is_open()
                && !self
                    .language_combo
                    .popup_contains(value, point, language_options.len())
            {
                self.language_combo.close();
            }
        }
        if let Some(index) = entries
            .iter()
            .position(|entry| *entry == SettingEntry::Theme)
        {
            let value = layout.items[index].value;
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
            let swatch = layout.items[index].accent_swatch;
            let handled = match entry {
                SettingEntry::DarkAccent => {
                    self.dark_accent.pointer_pressed(swatch, point, modifiers)
                }
                SettingEntry::LightAccent => {
                    self.light_accent.pointer_pressed(swatch, point, modifiers)
                }
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
            let value = layout.items[index].value;
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
            let value = layout.items[index].value;
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
            let value = layout.items[index].value;
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
            let value = layout.items[index].value;
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
            let value = layout.items[index].value;
            if self.plugin_paths.pointer_pressed(value, point, modifiers) {
                self.search.selected = index;
                self.search.query.set_focused(false);
                return true;
            }
        }
        if self
            .search
            .close_if_outside(rect, layout.shell.close, point)
        {
            self.animation.close();
            return true;
        }
        if self
            .search
            .query
            .pointer_pressed(layout.shell.search, point, modifiers)
        {
            return true;
        }
        for (index, entry) in entries.into_iter().enumerate() {
            if layout.items[index].row.contains(point) {
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
        let layout = self.search.rects(rect, entries.len(), 0.57);
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
            let swatch = layout.items[index].accent_swatch;
            let accent_changed = match entry {
                SettingEntry::DarkAccent => self.dark_accent.pointer_moved(swatch, point),
                SettingEntry::LightAccent => self.light_accent.pointer_moved(swatch, point),
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
        let language_options = crate::i18n::language_options();
        if self.language_combo.is_open() {
            match &event.logical_key {
                Key::Named(NamedKey::Escape | NamedKey::Enter) => {
                    self.language_combo.close();
                    return true;
                }
                Key::Named(NamedKey::ArrowDown | NamedKey::ArrowUp) => {
                    let selected = match event.logical_key {
                        Key::Named(NamedKey::ArrowDown) => {
                            (self.language_combo.selected() + 1) % language_options.len()
                        }
                        Key::Named(NamedKey::ArrowUp) => {
                            (self.language_combo.selected() + language_options.len() - 1)
                                % language_options.len()
                        }
                        _ => unreachable!(),
                    };
                    self.language_combo.set_selected(selected);
                    let _ =
                        crate::i18n::set_language(language_options[selected].language.as_deref());
                    return true;
                }
                _ => {}
            }
        }
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
                self.animation.close();
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

    pub(crate) fn caret_rect(&self, _width: f32, _height: f32) -> Option<Rect> {
        let entries = filtered_settings(self.search.query.text());
        let layout = self.search.cached_rects()?;
        if let Some(index) = entries
            .iter()
            .position(|entry| *entry == SettingEntry::PluginPaths)
        {
            if self.plugin_paths.is_focused() {
                return Some(self.plugin_paths.caret_rect(layout.items[index].value));
            }
        }
        for entry in [SettingEntry::DarkAccent, SettingEntry::LightAccent] {
            let Some(index) = entries.iter().position(|candidate| *candidate == entry) else {
                continue;
            };
            let swatch = layout.items[index].accent_swatch;
            let caret = match entry {
                SettingEntry::DarkAccent => self.dark_accent.caret_rect(swatch),
                SettingEntry::LightAccent => self.light_accent.caret_rect(swatch),
                _ => unreachable!(),
            };
            if caret.is_some() {
                return caret;
            }
        }
        self.search
            .query
            .is_focused()
            .then(|| self.search.query.caret_rect(layout.shell.search))
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

#[cfg(test)]
mod tests {
    use super::PreferencesFile;

    #[test]
    fn existing_preferences_without_language_use_system_default() {
        let preferences: PreferencesFile = serde_json::from_str("{\"plugin_paths\": \"plugins\"}")
            .expect("existing settings JSON should deserialize");
        assert_eq!(preferences.language, None);
        assert_eq!(preferences.plugin_paths, "plugins");
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
    animation: PopupAnimation,
    capturing: Option<String>,
}

impl KeybindsDialog {
    pub(crate) fn new() -> Self {
        Self {
            search: SearchDialogState::new(),
            animation: PopupAnimation::new(),
            capturing: None,
        }
    }
    pub(crate) fn is_closed(&self) -> bool {
        self.search.closed && self.animation.finished(std::time::Instant::now())
    }
    pub(crate) fn tick(&mut self, dt: f32) {
        self.search.query.tick(dt);
    }
    pub(crate) fn is_animating(&self) -> bool {
        self.animation.is_animating() || self.search.query.is_animating()
    }

    pub(crate) fn restart_entry_animation(&mut self) {
        self.animation.restart();
    }

    pub(crate) fn build(
        &mut self,
        ctx: &mut kama_ui::BuildCtx,
        width: f32,
        height: f32,
        registry: &CommandRegistry,
    ) {
        let rect = keybinds_rect(width, height);
        let indices = filtered_keybind_indices(registry, self.search.query.text());
        let layout = self.search.rects(rect, indices.len(), 0.62);
        dialog::build_search_dialog(
            ctx,
            "keybinds-scrim",
            "keybinds-dialog-shell",
            "keybinds-overlay-root",
            Rect::new(0.0, 0.0, width, height),
            rect,
            self.animation.opacity(std::time::Instant::now()),
            &layout.shell,
            Some(&crate::i18n::text("keybinds-title")),
            &crate::i18n::text("keybinds-hint"),
            |ctx, _shell| {
                self.search.query.build(
                    ctx,
                    "keybinds-search",
                    layout.shell.search,
                    &crate::i18n::text("keybinds-search-placeholder"),
                    component_style(),
                );
                ctx.with_clip(layout.shell.rows, |ctx| {
                    if indices.is_empty() {
                        ui_text!(
                            ctx,
                            "keybinds-no-results",
                            layout.shell.empty,
                            10.5,
                            theme::popup_muted(),
                            crate::i18n::text("settings-no-results"),
                        );
                    }
                    for (visible_index, definition_index) in indices.into_iter().enumerate() {
                    let definition = &registry.definitions()[definition_index];
                    let row = layout.items[visible_index];
                    if row.row.bottom() < layout.shell.rows.y || row.row.y > layout.shell.rows.bottom() {
                        continue;
                    }
                    let selected = visible_index == self.search.selected;
                    kama_ui::ui!(ctx, {
                        Rect(("keybind-row", &definition.id), row.row) {
                            fill: if selected { theme::accent_hover() } else { Color::TRANSPARENT };
                            border: 1; border_color: if selected { theme::accent() } else { Color::TRANSPARENT };
                            border_radius: 5.0; interactive;
                        }
                        Rect(("keybind-label", &definition.id), row.label) {
                            padding: 5.0; font_size: 10.5;
                            text_color: if selected { theme::accent_text() } else { theme::popup_text() };
                            text: &definition.label; tooltip: &definition.description;
                        }
                    });
                    let value = if self.capturing.as_deref() == Some(definition.id.as_str()) {
                        crate::i18n::text("keybinds-capture")
                    } else {
                        definition
                            .shortcut
                            .map_or_else(|| crate::i18n::text("keybinds-unbound"), |binding| binding.to_string())
                    };
                    kama_ui::ui!(ctx, {
                        Rect(("keybind-value", &definition.id), row.value) {
                            fill: if self.capturing.as_deref() == Some(definition.id.as_str()) {
                                theme::focused()
                            } else { theme::control() };
                            border: 1; border_color: theme::line(); border_radius: 4.0; padding: 6.0;
                            font_size: 9.5; text_color: theme::popup_text(); text: value;
                        }
                    });
                    }
                });
            },
        );
    }

    pub(crate) fn pointer_pressed(
        &mut self,
        width: f32,
        height: f32,
        point: [f32; 2],
        registry: &mut CommandRegistry,
    ) -> bool {
        let rect = keybinds_rect(width, height);
        let indices = filtered_keybind_indices(registry, self.search.query.text());
        let layout = self.search.rects(rect, indices.len(), 0.62);
        if self
            .search
            .close_if_outside(rect, layout.shell.close, point)
        {
            self.animation.close();
            return true;
        }
        if self
            .search
            .query
            .pointer_pressed(layout.shell.search, point, ModifiersState::empty())
        {
            return true;
        }
        if !layout.shell.rows.contains(point) {
            return true;
        }
        let Some(visible_index) = layout.items.iter().position(|row| row.row.contains(point))
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
        let count = filtered_keybind_indices(registry, self.search.query.text()).len();
        let _ = self.search.rects(rect, count, 0.62);
        self.search.scroll_by(-delta[1])
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
        let indices = filtered_keybind_indices(registry, self.search.query.text());
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.search.closed = true;
                self.animation.close();
                true
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.search.move_selection(indices.len(), 1);
                self.search.ensure_selected_visible();
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.search.move_selection(indices.len(), -1);
                self.search.ensure_selected_visible();
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

    pub(crate) fn caret_rect(&self, _width: f32, _height: f32) -> Option<Rect> {
        let layout = self.search.cached_rects()?;
        self.search
            .query
            .is_focused()
            .then(|| self.search.query.caret_rect(layout.shell.search))
    }
}
