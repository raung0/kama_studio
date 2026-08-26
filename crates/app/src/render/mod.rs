use kama_ui::{
    components::{Accordion, Button, ComboBox, NumberInput, SpinInput, TextEdit, ToggleButton},
    measure_layout, Align, BlockId, Color, IconId, Rect, ScrollState, Size,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use winit::{
    event::{Ime, KeyEvent},
    keyboard::ModifiersState,
};

use crate::{
    assets::{AppIcon, Icons},
    file_io::{app_data_dir, atomic_write_json, read_json},
    i18n,
    playback::RenderCachePreview,
    plugin::PluginRegistry,
    project::{CompositionId, Project},
    theme,
    timeline::TimelineState,
    Renderer,
};

mod engine;
mod spec;

pub(crate) use engine::RenderPhase;
use engine::RenderSession;
use spec::RenderSpec;

const ROW_H: f32 = 28.0;
const SECTION_H: f32 = 27.0;
const PAD: f32 = 8.0;
const SECTION_CONTENT_PAD: f32 = 8.0;
const SECTION_GAP: f32 = 5.0;
const DEFAULT_CACHE_CHUNK_SECONDS: f64 = 8.0;

labeled_enum! {
    #[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
    pub(crate) enum VideoCodec {
        H264 => "H.264",
        H265 => "H.265 / HEVC",
        ProRes4444 => "ProRes 4444",
        Vp9 => "VP9",
        Gif => "GIF",
        Ffv1 => "FFV1",
    }
}

impl VideoCodec {
    fn supports_alpha(self) -> bool {
        matches!(self, Self::ProRes4444 | Self::Vp9 | Self::Ffv1)
    }
}

labeled_enum! {
    #[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
    pub(crate) enum RenderResolution {
        #[default]
        Canvas => "Canvas / Project",
        Uhd4K => "3840×2160 (4K UHD)",
        Qhd => "2560×1440 (QHD)",
        FullHd => "1920×1080 (Full HD)",
        Hd => "1280×720 (HD)",
        VerticalFullHd => "1080×1920 (Vertical)",
        Square1080 => "1080×1080 (Square)",
    }
}

impl RenderResolution {
    fn dimensions(self, canvas: [u32; 2]) -> [u32; 2] {
        match self {
            Self::Canvas => [canvas[0].max(1), canvas[1].max(1)],
            Self::Uhd4K => [3840, 2160],
            Self::Qhd => [2560, 1440],
            Self::FullHd => [1920, 1080],
            Self::Hd => [1280, 720],
            Self::VerticalFullHd => [1080, 1920],
            Self::Square1080 => [1080, 1080],
        }
    }
}

labeled_enum! {
    #[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
    pub(crate) enum AudioCodec { Aac => "AAC", Opus => "Opus", Flac => "FLAC", Pcm => "PCM 24-bit", }
}

fn compatible_containers(codec: VideoCodec) -> &'static [&'static str] {
    match codec {
        VideoCodec::H264 | VideoCodec::H265 => &["mp4", "mov", "mkv"],
        VideoCodec::ProRes4444 => &["mov", "mkv"],
        VideoCodec::Vp9 => &["webm", "mkv"],
        VideoCodec::Gif => &["gif"],
        VideoCodec::Ffv1 => &["mkv"],
    }
}

fn compatible_audio_codecs(container: &str) -> &'static [AudioCodec] {
    match container {
        "mp4" => &[AudioCodec::Aac],
        "mov" => &[AudioCodec::Aac, AudioCodec::Pcm],
        "webm" => &[AudioCodec::Opus],
        "gif" => &[AudioCodec::Aac],
        _ => &AudioCodec::ALL,
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct RenderPreset {
    pub name: String,
    pub category: String,
    pub container: String,
    pub video_codec: VideoCodec,
    #[serde(default)]
    pub resolution: RenderResolution,
    pub quality: u8,
    pub include_audio: bool,
    pub audio_codec: AudioCodec,
    pub audio_bitrate_kbps: u32,
    pub sample_rate: u32,
}

impl RenderPreset {
    fn extension(&self) -> &str {
        &self.container
    }
}

fn built_in_presets() -> Vec<RenderPreset> {
    let preset = |name: &str,
                  category: &str,
                  container: &str,
                  video_codec,
                  quality,
                  audio_codec,
                  audio_bitrate_kbps| RenderPreset {
        name: name.into(),
        category: category.into(),
        container: container.into(),
        video_codec,
        resolution: RenderResolution::Canvas,
        quality,
        include_audio: true,
        audio_codec,
        audio_bitrate_kbps,
        sample_rate: 48_000,
    };
    vec![
        preset(
            "Web H.264",
            "Web",
            "mp4",
            VideoCodec::H264,
            18,
            AudioCodec::Aac,
            256,
        ),
        preset(
            "Web HEVC",
            "Web",
            "mp4",
            VideoCodec::H265,
            20,
            AudioCodec::Aac,
            256,
        ),
        preset(
            "WebM",
            "Web",
            "webm",
            VideoCodec::Vp9,
            28,
            AudioCodec::Opus,
            192,
        ),
        preset(
            "Alpha WebM",
            "Alpha",
            "webm",
            VideoCodec::Vp9,
            18,
            AudioCodec::Opus,
            192,
        ),
        RenderPreset {
            name: "Animated GIF".into(),
            category: "Web".into(),
            container: "gif".into(),
            video_codec: VideoCodec::Gif,
            resolution: RenderResolution::Canvas,
            quality: 0,
            include_audio: false,
            audio_codec: AudioCodec::Aac,
            audio_bitrate_kbps: 0,
            sample_rate: 48_000,
        },
        preset(
            "ProRes 4444",
            "Editing / Mezzanine",
            "mov",
            VideoCodec::ProRes4444,
            0,
            AudioCodec::Pcm,
            0,
        ),
        preset(
            "Lossless FFV1",
            "Archive",
            "mkv",
            VideoCodec::Ffv1,
            0,
            AudioCodec::Flac,
            0,
        ),
    ]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
enum RenderCombo {
    Preset,
    Resolution,
    VideoCodec,
    Container,
    AudioCodec,
    SampleRate,
    Bitrate,
}

impl RenderCombo {
    const ALL: [Self; 7] = [
        Self::Preset,
        Self::Resolution,
        Self::VideoCodec,
        Self::Container,
        Self::AudioCodec,
        Self::SampleRate,
        Self::Bitrate,
    ];
}

#[derive(Clone, Copy)]
#[repr(usize)]
enum RenderNumber {
    Quality,
    Begin,
    End,
}

impl RenderNumber {
    const ALL: [Self; 3] = [Self::Quality, Self::Begin, Self::End];
}

pub(crate) struct RenderPanelState {
    settings: RenderSpec,
    builtins: Vec<RenderPreset>,
    user_presets: Vec<RenderPreset>,
    preset_name: TextEdit,
    numbers: [NumberInput; 3],
    begin_set: bool,
    end_set: bool,
    combos: [ComboBox; 7],
    sections: [Accordion; 5],
    scroll: ScrollState,
    content_height: f32,
    session: RenderSession,
    start_requested: bool,
}

impl RenderPanelState {
    pub(crate) fn new(project: &Project, timeline: &TimelineState) -> Self {
        let presets = built_in_presets();
        let preset = presets[0].clone();
        let end = timeline.render_end_seconds();
        let fps = project.active_settings().frame_rate.max(1.0);
        let end_frame = (end as f64 * fps).ceil().max(1.0) as u64 - 1;
        let output = PathBuf::from(format!("render.{}", preset.extension()));
        let quality = preset.quality;
        Self {
            settings: RenderSpec {
                preset,
                output,
                overwrite: true,
                begin_frame: 0,
                end_frame,
                background: true,
                transcode: true,
            },
            builtins: presets,
            user_presets: load_user_presets(),
            preset_name: TextEdit::single_line("My preset"),
            numbers: [
                NumberInput::new(quality as f64)
                    .bounds(0.0, 51.0)
                    .precision(0),
                NumberInput::new(0.0)
                    .bounds(0.0, 100_000_000.0)
                    .precision(0),
                NumberInput::new(end_frame as f64)
                    .bounds(0.0, 100_000_000.0)
                    .precision(0),
            ],
            begin_set: false,
            end_set: false,
            combos: std::array::from_fn(|index| ComboBox::new([0, 0, 0, 0, 0, 1, 2][index])),
            sections: std::array::from_fn(|_| Accordion::new(true)),
            scroll: ScrollState::default(),
            content_height: 0.0,
            session: RenderSession::default(),
            start_requested: false,
        }
    }

    pub(crate) fn phase(&self) -> RenderPhase {
        self.session.phase()
    }

    pub(crate) fn is_active(&self) -> bool {
        self.session.is_active()
    }

    pub(crate) fn cancel_active(&mut self) {
        self.session.cancel();
        self.start_requested = false;
    }

    pub(crate) fn is_animating(&self) -> bool {
        self.session.is_active()
            || self.session.has_pending_update()
            || self.numbers.iter().any(NumberInput::is_animating)
            || self.preset_name.is_animating()
            || self.combos.iter().any(ComboBox::is_animating)
            || self.sections.iter().any(Accordion::is_animating)
    }

    pub(crate) fn is_value_dragging(&self) -> bool {
        self.numbers.iter().any(NumberInput::is_dragging)
    }

    pub(crate) fn tick_ui(&mut self, dt: f32) {
        self.numbers.iter_mut().for_each(|input| input.tick(dt));
        self.preset_name.tick(dt);
        self.combos.iter_mut().for_each(|combo| combo.tick(dt));
        self.sections
            .iter_mut()
            .for_each(|section| section.tick(dt));
    }

    pub(crate) fn sync_timeline_ranges(
        &mut self,
        timeline: &mut TimelineState,
        active_composition: CompositionId,
        fps: f64,
    ) {
        let fps = fps.max(1.0);
        timeline.set_render_output_range((self.begin_set && self.end_set).then_some((
            self.settings.begin_frame as f32 / fps as f32,
            self.settings.end_frame.saturating_add(1) as f32 / fps as f32,
        )));
        self.session
            .sync_timeline_ranges(timeline, active_composition);
    }

    pub(crate) fn cached_preview(
        &self,
        timeline_time: f32,
        fps: f64,
        active_composition: CompositionId,
    ) -> Option<RenderCachePreview> {
        self.session
            .cached_preview(timeline_time, fps, active_composition)
    }

    pub(crate) fn tick_render(
        &mut self,
        renderer: &Renderer,
        project: &Project,
        timeline: &TimelineState,
        plugins: &PluginRegistry,
        interaction: (u64, bool, bool),
    ) {
        if self.start_requested {
            self.start_requested = false;
            if !self.render_range_ready() {
                return;
            }
            let begin = self.settings.begin_frame.min(self.settings.end_frame);
            self.settings.begin_frame = begin;
            self.settings.end_frame = self.settings.end_frame.max(begin);
            let _ = self.session.start(
                self.settings.clone(),
                renderer,
                project,
                timeline,
                plugins,
                interaction,
            );
        }
        self.session.update(
            self.settings.end_frame,
            renderer,
            project,
            timeline,
            plugins,
            interaction,
        );
    }

    fn apply_preset(&mut self, preset: RenderPreset) {
        self.settings.preset = preset;
        self.normalize_target();
    }

    fn normalize_target(&mut self) {
        let containers = compatible_containers(self.settings.preset.video_codec);
        if !containers.contains(&self.settings.preset.container.as_str()) {
            self.settings.preset.container = containers[0].to_string();
        }
        if self.settings.preset.container == "gif" {
            self.settings.preset.include_audio = false;
        }
        let audio = compatible_audio_codecs(&self.settings.preset.container);
        if !audio.contains(&self.settings.preset.audio_codec) {
            self.settings.preset.audio_codec = audio[0];
        }
        self.settings
            .output
            .set_extension(self.settings.preset.extension());
    }

    fn preset_count(&self) -> usize {
        self.builtins.len() + self.user_presets.len()
    }

    fn preset_at(&self, index: usize) -> Option<RenderPreset> {
        if index < self.builtins.len() {
            self.builtins.get(index).cloned()
        } else {
            self.user_presets.get(index - self.builtins.len()).cloned()
        }
    }

    fn selected_preset_index(&self) -> usize {
        self.builtins
            .iter()
            .chain(self.user_presets.iter())
            .position(|preset| {
                preset.name == self.settings.preset.name
                    && preset.category == self.settings.preset.category
            })
            .unwrap_or(0)
    }

    fn combo(&self, kind: RenderCombo) -> &ComboBox {
        &self.combos[kind as usize]
    }

    fn combo_mut(&mut self, kind: RenderCombo) -> &mut ComboBox {
        &mut self.combos[kind as usize]
    }

    fn number(&self, kind: RenderNumber) -> &NumberInput {
        &self.numbers[kind as usize]
    }

    fn number_mut(&mut self, kind: RenderNumber) -> &mut NumberInput {
        &mut self.numbers[kind as usize]
    }

    pub(crate) fn close_popups(&mut self) {
        self.combos.iter_mut().for_each(ComboBox::close);
    }

    fn combo_section(kind: RenderCombo) -> usize {
        match kind {
            RenderCombo::Preset => 0,
            RenderCombo::Resolution | RenderCombo::VideoCodec | RenderCombo::Container => 1,
            RenderCombo::AudioCodec | RenderCombo::SampleRate | RenderCombo::Bitrate => 2,
        }
    }

    fn combo_len(&self, kind: RenderCombo) -> usize {
        match kind {
            RenderCombo::Preset => self.preset_count(),
            RenderCombo::Resolution => RenderResolution::ALL.len(),
            RenderCombo::VideoCodec => VideoCodec::ALL.len(),
            RenderCombo::Container => compatible_containers(self.settings.preset.video_codec).len(),
            RenderCombo::AudioCodec => {
                compatible_audio_codecs(&self.settings.preset.container).len()
            }
            RenderCombo::SampleRate => 3,
            RenderCombo::Bitrate => 4,
        }
    }

    fn apply_combo(&mut self, kind: RenderCombo, index: usize) {
        self.combo_mut(kind).select(index, true);
        match kind {
            RenderCombo::Preset => {
                if let Some(preset) = self.preset_at(index) {
                    self.apply_preset(preset);
                }
            }
            RenderCombo::Resolution => {
                self.settings.preset.resolution = RenderResolution::ALL[index]
            }
            RenderCombo::VideoCodec => {
                self.settings.preset.video_codec = VideoCodec::ALL[index];
                self.normalize_target();
            }
            RenderCombo::Container => {
                self.settings.preset.container =
                    compatible_containers(self.settings.preset.video_codec)[index].to_string();
                self.normalize_target();
            }
            RenderCombo::AudioCodec => {
                self.settings.preset.audio_codec =
                    compatible_audio_codecs(&self.settings.preset.container)[index];
            }
            RenderCombo::SampleRate => {
                self.settings.preset.sample_rate = [44_100, 48_000, 96_000][index]
            }
            RenderCombo::Bitrate => {
                self.settings.preset.audio_bitrate_kbps = [128, 192, 256, 320][index]
            }
        }
    }

    fn apply_number(&mut self, kind: RenderNumber, value: f64) {
        match kind {
            RenderNumber::Quality => {
                self.settings.preset.quality = value.round().clamp(0.0, 51.0) as u8
            }
            RenderNumber::Begin => {
                self.begin_set = true;
                self.settings.begin_frame = value.round().max(0.0) as u64;
            }
            RenderNumber::End => {
                self.end_set = true;
                self.settings.end_frame = value.round().max(0.0) as u64;
            }
        }
    }

    fn sync_controls(&mut self) {
        let number_values = [
            self.settings.preset.quality as f64,
            self.settings.begin_frame as f64,
            self.settings.end_frame as f64,
        ];
        for (input, value) in self.numbers.iter_mut().zip(number_values) {
            input.set_value(value);
        }

        let containers = compatible_containers(self.settings.preset.video_codec);
        let audio = compatible_audio_codecs(&self.settings.preset.container);
        let selections = [
            self.selected_preset_index(),
            RenderResolution::ALL
                .iter()
                .position(|value| *value == self.settings.preset.resolution)
                .unwrap_or(0),
            VideoCodec::ALL
                .iter()
                .position(|value| *value == self.settings.preset.video_codec)
                .unwrap_or(0),
            containers
                .iter()
                .position(|value| *value == self.settings.preset.container)
                .unwrap_or(0),
            audio
                .iter()
                .position(|value| *value == self.settings.preset.audio_codec)
                .unwrap_or(0),
            match self.settings.preset.sample_rate {
                44_100 => 0,
                96_000 => 2,
                _ => 1,
            },
            match self.settings.preset.audio_bitrate_kbps {
                0..=128 => 0,
                129..=192 => 1,
                193..=256 => 2,
                _ => 3,
            },
        ];
        for (combo, selected) in self.combos.iter_mut().zip(selections) {
            combo.set_selected(selected);
        }
    }

    fn section_amounts(&self) -> [f32; 5] {
        std::array::from_fn(|index| self.sections[index].open_amount())
    }

    fn section_open(&self, index: usize) -> bool {
        self.sections[index].open_amount() > 0.98
    }

    fn render_range_ready(&self) -> bool {
        self.begin_set && self.end_set
    }

    pub(crate) fn build(
        &mut self,
        ctx: &mut kama_ui::BuildCtx,
        rect: Rect,
        project: &Project,
        timeline: &TimelineState,
        icons: Icons,
        popup_bounds: Rect,
    ) {
        let chevron = icons.get(AppIcon::Chevron);
        if !self.begin_set {
            self.settings.begin_frame = 0;
        }
        if !self.end_set {
            let fps = project.active_settings().frame_rate.max(1.0);
            self.settings.end_frame =
                (timeline.render_end_seconds() as f64 * fps).ceil().max(1.0) as u64 - 1;
        }
        self.sync_controls();
        let unscrolled = measure_render_rects(
            rect.width,
            0.0,
            &self.settings.preset,
            self.section_amounts(),
        );
        self.content_height = unscrolled.content_height;
        self.scroll.offset = self
            .scroll
            .offset
            .min((self.content_height - rect.height).max(0.0));
        let layout = measure_render_rects(
            rect.width,
            self.scroll.offset,
            &self.settings.preset,
            self.section_amounts(),
        );
        let containers = compatible_containers(self.settings.preset.video_codec);
        let audio_codecs = compatible_audio_codecs(&self.settings.preset.container);

        kama_ui::ui!(ctx, {
            Rect("render-panel-bg", Rect::new(0.0, 0.0, rect.width, rect.height)) {
                fill: theme::panel();
            }
        });
        let style = crate::widgets::component_style();
        for (index, body) in layout.section_bodies.into_iter().enumerate() {
            if body.height <= 0.001 {
                continue;
            }
            kama_ui::ui!(ctx, {
                Row {
                    id: @format("render-section-body-row-{index}");
                    bounds: (body.x, body.y, body.width, body.height);
                    padding: 0.0;

                    HSpacer { width: Size::Pixels(PAD); }
                    Block {
                        id: @format("render-section-body-{index}");
                        width: Size::Fill;
                        height: Size::Fill;
                        fill: style.control;
                        border: 1;
                        border_color: style.border;
                        border_radius: style.radius_sm;
                        opacity: self.sections[index].open_amount();
                    }
                    HSpacer { width: Size::Pixels(PAD); }
                }
            });
        }
        for (index, title) in [
            i18n::text("render-presets"),
            i18n::text("render-video"),
            i18n::text("render-audio"),
            i18n::text("render-output"),
            i18n::text("render-controls"),
        ]
        .into_iter()
        .enumerate()
        {
            self.sections[index].build_header(
                ctx,
                format!("render-section-{index}"),
                layout.sections[index],
                &title,
                chevron,
                style,
            );
        }

        if self.sections[0].is_visible() {
            ctx.with_clip(layout.section_bodies[0], |ctx| {
                let field = layout.combo_field(RenderCombo::Preset).unwrap();
                label_at(
                    ctx,
                    field.label,
                    &i18n::text("render-preset"),
                    theme::text(),
                );
                let preset_names = self
                    .builtins
                    .iter()
                    .chain(self.user_presets.iter())
                    .map(|preset| format!("{}: {}", preset.category, preset.name))
                    .collect::<Vec<_>>();
                let preset_options = preset_names.iter().map(String::as_str).collect::<Vec<_>>();
                self.combo(RenderCombo::Preset).build_in(
                    ctx,
                    "render-preset-combo",
                    layout.combo(RenderCombo::Preset).unwrap(),
                    &preset_options,
                    chevron,
                    popup_bounds,
                    style,
                );
                self.preset_name.build(
                    ctx,
                    "render-preset-name",
                    layout.preset_name,
                    &i18n::text("render-preset-name"),
                    style,
                );
                Button::build(
                    ctx,
                    "render-save-preset",
                    layout.preset_save,
                    &i18n::text("render-save-preset"),
                    style,
                );
            });
        }

        if self.sections[1].is_visible() {
            ctx.with_clip(layout.section_bodies[1], |ctx| {
                let field = layout.combo_field(RenderCombo::Resolution).unwrap();
                label_at(
                    ctx,
                    field.label,
                    &i18n::text("render-resolution"),
                    theme::text(),
                );
                let resolution_options = RenderResolution::ALL
                    .iter()
                    .map(|value| value.label())
                    .collect::<Vec<_>>();
                self.combo(RenderCombo::Resolution).build_in(
                    ctx,
                    "render-resolution",
                    layout.combo(RenderCombo::Resolution).unwrap(),
                    &resolution_options,
                    chevron,
                    popup_bounds,
                    style,
                );
                let field = layout.combo_field(RenderCombo::VideoCodec).unwrap();
                label_at(ctx, field.label, &i18n::text("render-codec"), theme::text());
                let video_options = VideoCodec::ALL
                    .iter()
                    .map(|value| value.label())
                    .collect::<Vec<_>>();
                self.combo(RenderCombo::VideoCodec).build_in(
                    ctx,
                    "render-video-codec",
                    layout.combo(RenderCombo::VideoCodec).unwrap(),
                    &video_options,
                    chevron,
                    popup_bounds,
                    style,
                );
                let field = layout.combo_field(RenderCombo::Container).unwrap();
                label_at(
                    ctx,
                    field.label,
                    &i18n::text("render-container"),
                    theme::text(),
                );
                self.combo(RenderCombo::Container).build_in(
                    ctx,
                    "render-container",
                    layout.combo(RenderCombo::Container).unwrap(),
                    containers,
                    chevron,
                    popup_bounds,
                    style,
                );
                label_at(
                    ctx,
                    layout.number_field(RenderNumber::Quality).label,
                    &i18n::text("render-quality"),
                    theme::text(),
                );
                self.number_mut(RenderNumber::Quality).build(
                    ctx,
                    "render-quality",
                    layout.number(RenderNumber::Quality),
                    "",
                    style,
                );
            });
        }

        if self.sections[2].is_visible() {
            ctx.with_clip(layout.section_bodies[2], |ctx| {
                toggle_row_at(
                    ctx,
                    layout.include_audio,
                    &i18n::text("render-include-audio"),
                    self.settings.preset.include_audio,
                );
                if let Some(field) = layout.combo_field(RenderCombo::AudioCodec) {
                    label_at(ctx, field.label, &i18n::text("render-codec"), theme::text());
                    let labels = audio_codecs
                        .iter()
                        .map(|codec| codec.label())
                        .collect::<Vec<_>>();
                    self.combo(RenderCombo::AudioCodec).build_in(
                        ctx,
                        "render-audio-codec",
                        field.control,
                        &labels,
                        chevron,
                        popup_bounds,
                        style,
                    );
                }
                if let Some(field) = layout.combo_field(RenderCombo::SampleRate) {
                    label_at(
                        ctx,
                        field.label,
                        &i18n::text("render-sample-rate"),
                        theme::text(),
                    );
                    self.combo(RenderCombo::SampleRate).build_in(
                        ctx,
                        "render-sample-rate",
                        field.control,
                        &["44100 Hz", "48000 Hz", "96000 Hz"],
                        chevron,
                        popup_bounds,
                        style,
                    );
                }
                if let Some(field) = layout.combo_field(RenderCombo::Bitrate) {
                    label_at(
                        ctx,
                        field.label,
                        &i18n::text("render-bitrate"),
                        theme::text(),
                    );
                    self.combo(RenderCombo::Bitrate).build_in(
                        ctx,
                        "render-audio-bitrate",
                        field.control,
                        &["128 kb/s", "192 kb/s", "256 kb/s", "320 kb/s"],
                        chevron,
                        popup_bounds,
                        style,
                    );
                }
            });
        }

        if self.sections[3].is_visible() {
            ctx.with_clip(layout.section_bodies[3], |ctx| {
            label_at(ctx, layout.path_label, &i18n::text("render-path"), theme::text());
            kama_ui::ui!(ctx, {
                Rect("render-output-path", layout.path) {
                    fill: theme::control(); border: 1; border_color: theme::line(); border_radius: 5.0;
                    padding: 6.0; font_size: 10.0; text_color: theme::accent();
                    text: self.settings.output.display().to_string(); interactive; tooltip: i18n::text("render-choose-output");
                }
            });
            toggle_row_at(
                ctx,
                layout.overwrite,
                &i18n::text("render-overwrite"),
                self.settings.overwrite,
            );
            });
        }

        if self.sections[4].is_visible() {
            ctx.with_clip(layout.section_bodies[4], |ctx| {
            label_at(
                ctx,
                layout.number_field(RenderNumber::Begin).label,
                &i18n::text("render-begin-frame"),
                theme::text(),
            );
            if self.begin_set {
                self.number_mut(RenderNumber::Begin).build(
                    ctx,
                    "render-begin-frame",
                    layout.number(RenderNumber::Begin),
                    "",
                    style,
                );
            } else {
                SpinInput::build(
                    ctx,
                    "render-begin-frame",
                    layout.number(RenderNumber::Begin),
                    &i18n::text("render-not-set"),
                    style,
                );
            }
            two_buttons_at(
                ctx,
                layout.begin_buttons,
                [&i18n::text("render-playhead"), &i18n::text("render-start")],
                "render-begin",
            );
            label_at(
                ctx,
                layout.number_field(RenderNumber::End).label,
                &i18n::text("render-end-frame"),
                theme::text(),
            );
            if self.end_set {
                self.number_mut(RenderNumber::End).build(
                    ctx,
                    "render-end-frame",
                    layout.number(RenderNumber::End),
                    "",
                    style,
                );
            } else {
                SpinInput::build(
                    ctx,
                    "render-end-frame",
                    layout.number(RenderNumber::End),
                    &i18n::text("render-not-set"),
                    style,
                );
            }
            two_buttons_at(
                ctx,
                layout.end_buttons,
                [&i18n::text("render-playhead"), &i18n::text("render-end")],
                "render-end",
            );
            toggle_row_at(
                ctx,
                layout.background,
                &i18n::text("render-background-task"),
                self.settings.background,
            );
            toggle_row_at(
                ctx,
                layout.transcode,
                &i18n::text("render-transcode-output"),
                self.settings.transcode,
            );

            let phase = self.phase();
            let range_ready = self.render_range_ready();
            match phase {
                RenderPhase::Rendering => {
                    Button::build(ctx, "render-pause", layout.action, &i18n::text("render-pause"), style)
                }
                RenderPhase::Paused => {
                    Button::build(ctx, "render-resume", layout.action, &i18n::text("render-resume"), style)
                }
                RenderPhase::Transcoding => Button::build_filled(
                    ctx,
                    "render-transcoding",
                    layout.action,
                    &i18n::text("render-transcoding"),
                    theme::focused(),
                    style,
                ),
                _ if range_ready => {
                    Button::build_filled(
                        ctx,
                        "render-start",
                        layout.action,
                        &i18n::text("render-start-render"),
                        theme::accent(),
                        style,
                    );
                    render_button_icon(
                        ctx,
                        "render-start-icon",
                        layout.action,
                        icons.get(AppIcon::StartRender),
                        theme::panel(),
                    );
                }
                _ => {
                    kama_ui::ui!(ctx, {
                        Rect("render-start-disabled", layout.action) {
                            fill: theme::control(); border: 1; border_color: theme::line(); border_radius: style.radius_md;
                            font_size: 11.0 * style.text_scale; text_color: theme::muted().mix(theme::control(), 0.45);
                            text_centered; text: i18n::text("render-start-render");
                        }
                    });
                    render_button_icon(
                        ctx,
                        "render-start-disabled-icon",
                        layout.action,
                        icons.get(AppIcon::StartRender),
                        theme::muted(),
                    );
                }
            }
            });
        }
    }

    pub(crate) fn popup_contains(&self, rect: Rect, point: [f32; 2]) -> bool {
        let p = [point[0] - rect.x, point[1] - rect.y];
        let layout = measure_render_rects(
            rect.width,
            self.scroll.offset,
            &self.settings.preset,
            self.section_amounts(),
        );
        RenderCombo::ALL.into_iter().any(|kind| {
            self.section_open(Self::combo_section(kind))
                && layout.combo(kind).is_some_and(|control| {
                    self.combo(kind)
                        .popup_contains(control, p, self.combo_len(kind))
                })
        })
    }

    pub(crate) fn scroll(&mut self, rect: Rect, point: [f32; 2], delta: [f32; 2]) -> bool {
        let p = [point[0] - rect.x, point[1] - rect.y];
        let layout = measure_render_rects(
            rect.width,
            self.scroll.offset,
            &self.settings.preset,
            self.section_amounts(),
        );
        for kind in RenderCombo::ALL {
            if !self.section_open(Self::combo_section(kind)) {
                continue;
            }
            let Some(control) = layout.combo(kind) else {
                continue;
            };
            let len = self.combo_len(kind);
            if self.combo_mut(kind).scroll(control, p, delta, len) {
                return true;
            }
        }
        rect.contains(point)
            && self
                .scroll
                .scroll_by(-delta[1], (self.content_height - rect.height).max(0.0))
    }

    pub(crate) fn pointer_pressed(
        &mut self,
        rect: Rect,
        point: [f32; 2],
        modifiers: ModifiersState,
        project: &Project,
        timeline: &TimelineState,
    ) -> bool {
        if !rect.contains(point) && !self.popup_contains(rect, point) {
            return false;
        }
        let p = [point[0] - rect.x, point[1] - rect.y];
        let layout = measure_render_rects(
            rect.width,
            self.scroll.offset,
            &self.settings.preset,
            self.section_amounts(),
        );
        for kind in RenderCombo::ALL {
            if !self.section_open(Self::combo_section(kind)) {
                continue;
            }
            let Some(control) = layout.combo(kind) else {
                continue;
            };
            let len = self.combo_len(kind);
            if let Some(index) = self.combo(kind).option_at(control, p, len) {
                self.apply_combo(kind, index);
                return true;
            }
        }

        for index in 0..5 {
            if layout.sections[index].contains(p) {
                self.close_popups();
                self.sections[index].toggle();
                return true;
            }
        }

        for kind in RenderCombo::ALL {
            if self.section_open(Self::combo_section(kind))
                && layout
                    .combo(kind)
                    .is_some_and(|control| control.contains(p))
            {
                self.close_popups();
                self.combo_mut(kind).toggle();
                return true;
            }
        }

        if self.section_open(0) {
            if self
                .preset_name
                .pointer_pressed(layout.preset_name, p, modifiers)
            {
                return true;
            }
            if layout.preset_save.contains(p) {
                let mut preset = self.settings.preset.clone();
                preset.name = self.preset_name.text().trim().to_string();
                preset.category = "User".into();
                if !preset.name.is_empty() {
                    self.user_presets
                        .retain(|candidate| candidate.name != preset.name);
                    self.user_presets.push(preset.clone());
                    self.apply_preset(preset);
                    save_user_presets(&self.user_presets);
                }
                return true;
            }
        }
        if self.section_open(1) {
            if let Some(value) = self.number_mut(RenderNumber::Quality).pointer_pressed(
                layout.number(RenderNumber::Quality),
                p,
                modifiers,
            ) {
                self.apply_number(RenderNumber::Quality, value);
                return true;
            }
        }
        if self.section_open(2)
            && self.settings.preset.container != "gif"
            && layout.include_audio.control.contains(p)
        {
            self.settings.preset.include_audio = !self.settings.preset.include_audio;
            return true;
        }
        self.close_popups();
        if self.section_open(3) {
            if layout.path.contains(p) {
                let extension = self.settings.preset.extension().to_string();
                let dialog = rfd::FileDialog::new()
                    .add_filter("Video", &[extension.as_str()])
                    .set_file_name(
                        self.settings
                            .output
                            .file_name()
                            .and_then(|value| value.to_str())
                            .unwrap_or("render.mp4"),
                    );
                if let Some(mut path) = dialog.save_file() {
                    path.set_extension(&extension);
                    self.settings.output = path;
                }
                return true;
            }
            if layout.overwrite.control.contains(p) {
                self.settings.overwrite = !self.settings.overwrite;
                return true;
            }
        }
        if self.section_open(4) {
            if layout.number(RenderNumber::Begin).contains(p) {
                self.begin_set = true;
                if let Some(value) = self.number_mut(RenderNumber::Begin).pointer_pressed(
                    layout.number(RenderNumber::Begin),
                    p,
                    modifiers,
                ) {
                    self.apply_number(RenderNumber::Begin, value);
                }
                return true;
            }
            if layout.begin_buttons[0].contains(p) {
                self.begin_set = true;
                self.settings.begin_frame = (timeline.playhead().max(0.0) as f64
                    * project.active_settings().frame_rate)
                    .round() as u64;
                return true;
            }
            if layout.begin_buttons[1].contains(p) {
                self.begin_set = true;
                self.settings.begin_frame = 0;
                return true;
            }
            if layout.number(RenderNumber::End).contains(p) {
                self.end_set = true;
                if let Some(value) = self.number_mut(RenderNumber::End).pointer_pressed(
                    layout.number(RenderNumber::End),
                    p,
                    modifiers,
                ) {
                    self.apply_number(RenderNumber::End, value);
                }
                return true;
            }
            if layout.end_buttons[0].contains(p) {
                self.end_set = true;
                self.settings.end_frame = (timeline.playhead().max(0.0) as f64
                    * project.active_settings().frame_rate)
                    .round() as u64;
                return true;
            }
            if layout.end_buttons[1].contains(p) {
                self.end_set = true;
                self.settings.end_frame = (timeline.render_end_seconds() as f64
                    * project.active_settings().frame_rate)
                    .ceil()
                    .max(1.0) as u64
                    - 1;
                return true;
            }
            if layout.background.control.contains(p) {
                self.settings.background = !self.settings.background;
                self.session.set_background(self.settings.background);
                return true;
            }
            if layout.transcode.control.contains(p) {
                self.settings.transcode = !self.settings.transcode;
                return true;
            }
            if layout.action.contains(p) {
                match self.phase() {
                    RenderPhase::Rendering => self.session.pause(),
                    RenderPhase::Paused => self.session.resume(),
                    RenderPhase::Transcoding => {}
                    _ if self.render_range_ready() => self.start_requested = true,
                    _ => {}
                }
                return true;
            }
        }
        true
    }

    pub(crate) fn pointer_moved(&mut self, rect: Rect, point: [f32; 2]) -> bool {
        let point = [point[0] - rect.x, point[1] - rect.y];
        let mut changed = false;
        for kind in RenderNumber::ALL {
            if let Some(value) = self.number_mut(kind).pointer_moved(point) {
                self.apply_number(kind, value);
                changed = true;
            }
        }
        changed
    }

    pub(crate) fn pointer_released(&mut self) -> bool {
        self.numbers
            .iter_mut()
            .fold(false, |changed, input| input.pointer_released() | changed)
    }

    pub(crate) fn handle_key(&mut self, event: &KeyEvent, modifiers: ModifiersState) -> bool {
        if self.preset_name.handle_key(event, modifiers).changed {
            return true;
        }
        for kind in RenderNumber::ALL {
            if let Some(value) = self.number_mut(kind).handle_key(event, modifiers) {
                self.apply_number(kind, value);
                return true;
            }
        }
        false
    }

    pub(crate) fn handle_ime(&mut self, event: &Ime) -> bool {
        if self.preset_name.handle_ime(event).changed {
            return true;
        }
        for kind in RenderNumber::ALL {
            if let Some(value) = self.number_mut(kind).handle_ime(event) {
                self.apply_number(kind, value);
                return true;
            }
        }
        false
    }

    pub(crate) fn ime_area(&self, rect: Rect) -> Option<Rect> {
        let layout = measure_render_rects(
            rect.width,
            self.scroll.offset,
            &self.settings.preset,
            self.section_amounts(),
        );
        let global = |local: Rect| {
            Rect::new(
                rect.x + local.x,
                rect.y + local.y,
                local.width,
                local.height,
            )
        };
        if self.preset_name.is_focused() {
            return Some(self.preset_name.caret_rect(global(layout.preset_name)));
        }
        RenderNumber::ALL
            .into_iter()
            .find_map(|kind| self.number(kind).caret_rect(global(layout.number(kind))))
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct RenderFieldRects {
    label: Rect,
    control: Rect,
}

#[derive(Clone, Copy)]
struct RenderRects {
    sections: [Rect; 5],
    section_bodies: [Rect; 5],
    combos: [Option<RenderFieldRects>; 7],
    numbers: [RenderFieldRects; 3],
    preset_name: Rect,
    preset_save: Rect,
    include_audio: RenderFieldRects,
    path_label: Rect,
    path: Rect,
    overwrite: RenderFieldRects,
    begin_buttons: [Rect; 2],
    end_buttons: [Rect; 2],
    background: RenderFieldRects,
    transcode: RenderFieldRects,
    action: Rect,
    content_height: f32,
}

impl RenderRects {
    fn combo(self, kind: RenderCombo) -> Option<Rect> {
        self.combos[kind as usize].map(|field| field.control)
    }

    fn combo_field(self, kind: RenderCombo) -> Option<RenderFieldRects> {
        self.combos[kind as usize]
    }

    fn number(self, kind: RenderNumber) -> Rect {
        self.numbers[kind as usize].control
    }

    fn number_field(self, kind: RenderNumber) -> RenderFieldRects {
        self.numbers[kind as usize]
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct RenderFieldIds {
    label: BlockId,
    control: BlockId,
}

#[derive(Clone, Copy, Debug, Default)]
struct RenderRectIds {
    root: BlockId,
    sections: [BlockId; 5],
    section_bodies: [BlockId; 5],
    combos: [Option<RenderFieldIds>; 7],
    numbers: [RenderFieldIds; 3],
    preset_name: BlockId,
    preset_save: BlockId,
    alpha: BlockId,
    include_audio: RenderFieldIds,
    path_label: BlockId,
    path: BlockId,
    overwrite: RenderFieldIds,
    begin_buttons: [BlockId; 2],
    end_buttons: [BlockId; 2],
    background: RenderFieldIds,
    transcode: RenderFieldIds,
    status: BlockId,
    action: BlockId,
}

fn build_spacer(ctx: &mut kama_ui::BuildCtx, width: Size, height: Size) {
    let _ = ctx.new().width(width).height(height).build();
}

fn build_full_width_item(
    ctx: &mut kama_ui::BuildCtx,
    height: f32,
    item_height: f32,
    align: Align,
) -> BlockId {
    let mut item = BlockId::default();
    let _ = ctx
        .new()
        .width(Size::Fill)
        .height(Size::Pixels(height))
        .row()
        .align_items(align)
        .children(|ctx| {
            build_spacer(ctx, Size::Pixels(PAD), Size::Fill);
            item = ctx
                .new()
                .width(Size::Fill)
                .height(Size::Pixels(item_height))
                .build();
            build_spacer(ctx, Size::Pixels(PAD), Size::Fill);
        })
        .build();
    item
}

fn build_field_row(ctx: &mut kama_ui::BuildCtx) -> RenderFieldIds {
    let mut field = RenderFieldIds::default();
    let _ = ctx
        .new()
        .width(Size::Fill)
        .height(Size::Pixels(ROW_H))
        .row()
        .align_items(Align::Center)
        .children(|ctx| {
            build_spacer(ctx, Size::Pixels(PAD), Size::Fill);
            field.label = ctx
                .new()
                .width(Size::FillPortion(0.42))
                .height(Size::Pixels(20.0))
                .build();
            field.control = ctx
                .new()
                .width(Size::FillPortion(0.58))
                .height(Size::Pixels(24.0))
                .build();
            build_spacer(ctx, Size::Pixels(PAD), Size::Fill);
        })
        .build();
    field
}

fn build_two_button_row(ctx: &mut kama_ui::BuildCtx) -> [BlockId; 2] {
    let mut buttons = [BlockId::default(); 2];
    let _ = ctx
        .new()
        .width(Size::Fill)
        .height(Size::Pixels(ROW_H))
        .row()
        .align_items(Align::Center)
        .children(|ctx| {
            build_spacer(ctx, Size::Pixels(PAD), Size::Fill);
            build_spacer(ctx, Size::FillPortion(0.42), Size::Fill);
            let _ = ctx
                .new()
                .width(Size::FillPortion(0.58))
                .height(Size::Fill)
                .row()
                .gap(4.0)
                .align_items(Align::Center)
                .children(|ctx| {
                    for button in &mut buttons {
                        *button = ctx
                            .new()
                            .width(Size::Fill)
                            .height(Size::Pixels(24.0))
                            .build();
                    }
                })
                .build();
            build_spacer(ctx, Size::Pixels(PAD), Size::Fill);
        })
        .build();
    buttons
}

fn build_section(
    ctx: &mut kama_ui::BuildCtx,
    open: f32,
    body: impl FnOnce(&mut kama_ui::BuildCtx),
) -> (BlockId, BlockId) {
    let mut header = BlockId::default();
    let mut section_body = BlockId::default();
    let _ = ctx
        .new()
        .width(Size::Fill)
        .height(Size::Fit)
        .column()
        .children(|ctx| {
            header = build_full_width_item(ctx, SECTION_H - 2.0, SECTION_H - 2.0, Align::Start);
            build_spacer(ctx, Size::Fill, Size::Pixels(2.0));
            section_body = ctx
                .new()
                .width(Size::Fill)
                .height(Size::FitScale(open))
                .column()
                .children(|ctx| {
                    let _ = ctx
                        .new()
                        .width(Size::Fill)
                        .height(Size::Fit)
                        .padding(SECTION_CONTENT_PAD)
                        .column()
                        .children(body)
                        .build();
                })
                .build();
            build_spacer(ctx, Size::Fill, Size::Pixels(SECTION_GAP));
        })
        .build();
    (header, section_body)
}

fn measure_render_rects(
    width: f32,
    scroll: f32,
    preset: &RenderPreset,
    open: [f32; 5],
) -> RenderRects {
    let (ids, measured) = measure_layout(Rect::new(0.0, 0.0, width, 1.0), |ctx| {
        let mut ids = RenderRectIds::default();
        let root = ctx
            .new()
            .position((0.0, -scroll))
            .width(Size::Fill)
            .height(Size::Fit)
            .column()
            .children(|ctx| {
                build_spacer(ctx, Size::Fill, Size::Pixels(7.0));

                let (section, section_body) = build_section(ctx, open[0], |ctx| {
                    ids.combos[RenderCombo::Preset as usize] = Some(build_field_row(ctx));
                    let _ = ctx
                        .new()
                        .width(Size::Fill)
                        .height(Size::Pixels(ROW_H))
                        .row()
                        .align_items(Align::Center)
                        .children(|ctx| {
                            build_spacer(ctx, Size::Pixels(PAD), Size::Fill);
                            let _ = ctx
                                .new()
                                .width(Size::Fill)
                                .height(Size::Fill)
                                .row()
                                .gap(5.0)
                                .align_items(Align::Center)
                                .children(|ctx| {
                                    ids.preset_name = ctx
                                        .new()
                                        .width(Size::Fill)
                                        .height(Size::Pixels(24.0))
                                        .build();
                                    ids.preset_save = ctx
                                        .new()
                                        .width(Size::Pixels(90.0))
                                        .height(Size::Pixels(24.0))
                                        .build();
                                })
                                .build();
                            build_spacer(ctx, Size::Pixels(PAD + 1.0), Size::Fill);
                        })
                        .build();
                    build_spacer(ctx, Size::Fill, Size::Pixels(6.0));
                });
                ids.sections[0] = section;
                ids.section_bodies[0] = section_body;

                let (section, section_body) = build_section(ctx, open[1], |ctx| {
                    ids.combos[RenderCombo::Resolution as usize] = Some(build_field_row(ctx));
                    ids.combos[RenderCombo::VideoCodec as usize] = Some(build_field_row(ctx));
                    ids.combos[RenderCombo::Container as usize] = Some(build_field_row(ctx));
                    ids.numbers[RenderNumber::Quality as usize] = build_field_row(ctx);
                    ids.alpha = build_full_width_item(ctx, 22.0, 20.0, Align::Start);
                });
                ids.sections[1] = section;
                ids.section_bodies[1] = section_body;

                let (section, section_body) = build_section(ctx, open[2], |ctx| {
                    ids.include_audio = build_field_row(ctx);
                    if preset.include_audio {
                        ids.combos[RenderCombo::AudioCodec as usize] = Some(build_field_row(ctx));
                        ids.combos[RenderCombo::SampleRate as usize] = Some(build_field_row(ctx));
                        if matches!(preset.audio_codec, AudioCodec::Aac | AudioCodec::Opus) {
                            ids.combos[RenderCombo::Bitrate as usize] = Some(build_field_row(ctx));
                        }
                    }
                });
                ids.sections[2] = section;
                ids.section_bodies[2] = section_body;

                let (section, section_body) = build_section(ctx, open[3], |ctx| {
                    ids.path_label = build_full_width_item(ctx, 18.0, 20.0, Align::Start);
                    ids.path = build_full_width_item(ctx, 26.0, 26.0, Align::Start);
                    build_spacer(ctx, Size::Fill, Size::Pixels(5.0));
                    ids.overwrite = build_field_row(ctx);
                });
                ids.sections[3] = section;
                ids.section_bodies[3] = section_body;

                let (section, section_body) = build_section(ctx, open[4], |ctx| {
                    ids.numbers[RenderNumber::Begin as usize] = build_field_row(ctx);
                    ids.begin_buttons = build_two_button_row(ctx);
                    ids.numbers[RenderNumber::End as usize] = build_field_row(ctx);
                    ids.end_buttons = build_two_button_row(ctx);
                    ids.background = build_field_row(ctx);
                    ids.transcode = build_field_row(ctx);
                    ids.status = build_full_width_item(ctx, 20.0, 20.0, Align::Start);
                    build_spacer(ctx, Size::Fill, Size::Pixels(3.0));
                    ids.action = build_full_width_item(ctx, ROW_H, 24.0, Align::Start);
                });
                ids.sections[4] = section;
                ids.section_bodies[4] = section_body;

                build_spacer(ctx, Size::Fill, Size::Pixels(PAD));
            })
            .build();
        ids.root = root;
        ids
    });

    let rect = |id: BlockId| measured.rect(id).unwrap_or_default();
    let field = |ids: RenderFieldIds| RenderFieldRects {
        label: rect(ids.label),
        control: rect(ids.control),
    };
    let combos = std::array::from_fn(|index| ids.combos[index].map(field));

    RenderRects {
        sections: ids.sections.map(rect),
        section_bodies: ids.section_bodies.map(rect),
        combos,
        numbers: ids.numbers.map(field),
        preset_name: rect(ids.preset_name),
        preset_save: rect(ids.preset_save),
        include_audio: field(ids.include_audio),
        path_label: rect(ids.path_label),
        path: rect(ids.path),
        overwrite: field(ids.overwrite),
        begin_buttons: ids.begin_buttons.map(rect),
        end_buttons: ids.end_buttons.map(rect),
        background: field(ids.background),
        transcode: field(ids.transcode),
        action: rect(ids.action),
        content_height: rect(ids.root).height,
    }
}

fn render_button_icon(
    ctx: &mut kama_ui::BuildCtx,
    id: &str,
    rect: Rect,
    icon: IconId,
    color: Color,
) {
    kama_ui::ui!(ctx, {
        Row {
            id: @format("{}-layout", id);
            bounds: (rect.x, rect.y, rect.width, rect.height);
            padding: 0.0;

            HSpacer { width: Size::Pixels(8.0); }
            Block {
                id: id;
                width: Size::Pixels(18.0);
                height: Size::Fill;
                content_centered;

                Icon {
                    id: @format("{}-glyph", id);
                    icon!: icon;
                    color!: color;
                    width: Size::Pixels(15.0);
                    height: Size::Pixels(15.0);
                }
            }
            HSpacer { width: Size::Fill; }
        }
    });
}

fn label_at(ctx: &mut kama_ui::BuildCtx, rect: Rect, text: &str, color: Color) {
    ui_text!(
        ctx,
        ("render-label", text, (rect.y * 10.0) as i32),
        rect,
        9.5,
        color,
        text,
    );
}

fn toggle_row_at(ctx: &mut kama_ui::BuildCtx, field: RenderFieldRects, name: &str, value: bool) {
    label_at(ctx, field.label, name, theme::text());
    ToggleButton::build(
        ctx,
        format!("render-toggle-{name}"),
        field.control,
        if value { "On" } else { "Off" },
        value,
        crate::widgets::component_style(),
    );
}
fn two_buttons_at(ctx: &mut kama_ui::BuildCtx, rects: [Rect; 2], labels: [&str; 2], id: &str) {
    for (index, (rect, label)) in rects.into_iter().zip(labels).enumerate() {
        Button::build(
            ctx,
            format!("{id}-{index}"),
            rect,
            label,
            crate::widgets::component_style(),
        );
    }
}

fn presets_path() -> PathBuf {
    app_data_dir().join("render-presets.json")
}

fn load_user_presets() -> Vec<RenderPreset> {
    read_json(&presets_path()).unwrap_or_default()
}

fn save_user_presets(presets: &[RenderPreset]) {
    let _ = atomic_write_json(&presets_path(), &presets);
}

impl Drop for RenderPanelState {
    fn drop(&mut self) {
        self.cancel_active();
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    fn assert_range_button_layout(width: f32) {
        let preset = built_in_presets().remove(0);
        let layout = measure_render_rects(width, 0.0, &preset, [0.0, 0.0, 0.0, 0.0, 1.0]);

        for (buttons, number) in [
            (layout.begin_buttons, layout.number(RenderNumber::Begin)),
            (layout.end_buttons, layout.number(RenderNumber::End)),
        ] {
            assert!((buttons[0].width - buttons[1].width).abs() < 0.001);
            assert!((buttons[0].x - number.x).abs() < 0.001);
            assert!((buttons[1].right() - number.right()).abs() < 0.001);
            assert!((buttons[1].x - buttons[0].right() - 4.0).abs() < 0.001);
        }
    }

    #[test]
    fn range_buttons_split_the_control_column_evenly() {
        assert_range_button_layout(180.0);
        assert_range_button_layout(480.0);
    }
}
