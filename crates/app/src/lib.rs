macro_rules! labeled_enum {
    ($(#[$meta:meta])* $vis:vis enum $name:ident {
        $($(#[$variant_meta:meta])* $variant:ident => $label:expr),+ $(,)?
    }) => {
        $(#[$meta])*
        $vis enum $name { $( $(#[$variant_meta])* $variant, )+ }

        impl $name {
            pub const ALL: [Self; labeled_enum!(@count $($variant),+)] = [$(Self::$variant),+];
            pub fn label(self) -> &'static str {
                match self { $(Self::$variant => $label),+ }
            }
        }
    };
    (@count $($variant:ident),+) => { <[()]>::len(&[$(labeled_enum!(@unit $variant)),+]) };
    (@unit $variant:ident) => { () };
}

macro_rules! ui_text {
    ($ctx:expr, $key:expr, $rect:expr, $size:expr, $color:expr, $text:expr $(,)?) => {
        kama_ui::ui!($ctx, {
            Rect($key, $rect) {
                font_size: $size;
                text_color: $color;
                text: $text;
            }
        });
    };
}

macro_rules! default_state {
    ($(#[$meta:meta])* $vis:vis struct $name:ident {
        $($(#[$fmeta:meta])* $fvis:vis $field:ident: $ty:ty $(= $default:expr)?),* $(,)?
    }) => {
        $(#[$meta])*
        $vis struct $name { $( $(#[$fmeta])* $fvis $field: $ty, )* }

        impl Default for $name {
            fn default() -> Self {
                Self { $( $field: default_state!(@value $($default)?), )* }
            }
        }
    };
    (@value $default:expr) => { $default };
    (@value) => { Default::default() };
}

mod app_actions;
mod app_events;
mod app_menu;
#[macro_use]
mod app_modal;
mod app_shared;
mod app_ui_helpers;
mod assets;
mod audio;
mod clip_graph_cache;
mod command;
mod dialog;
mod editor;
mod effects;
mod embedded_vfs;
mod file_io;
mod gradient;
mod history;
mod i18n;
mod messages;
mod meters;
mod model3d;
mod monitor;
mod panels;
mod playback;
mod plugin;
mod preferences;
mod project;
#[path = "app_project_commands.rs"]
mod project_commands;
mod project_io;
mod render;
mod runtime;
mod shader_codegen;
mod theme;
mod timeline;
mod ui_layout;
mod version;
mod waveform;
mod widgets;
use app_menu::*;
use app_modal::*;

use std::{
    collections::{HashMap, HashSet, VecDeque},
    hash::Hash,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use app_events::*;
use app_shared::*;
use app_ui_helpers::*;
use assets::{AboutLogos, AppIcon, Icons};
use audio::AudioPlayback;
use command::{
    fuzzy_score, CommandQueue, CommandRegistry, CommandScope, DockCommand, EditCommand,
    EditorCommand, PaletteAction,
};
use editor::EditorSession;
use effects::{EffectRuntime, PipelineKind, ValueNodeKind};
use file_io::atomic_write_json;
use history::{HistoryPanelState, HistorySnapshot, HistoryState};
use kama_ui as ui;
use kama_ui::components::TextEdit;
use kama_ui::dock::{
    drop_preview, drop_zone, insertion_index, Axis, DockLayoutSpec, DockState, DockTransfer,
    DropZone, LayoutSnapshot, Rect, SplitId, StackId, StackLayout, TabId,
};
use kama_ui::{Color, CursorShape, Gui, InputState, Renderer, ScrollState, Size};
use messages::MessagesState;
use meters::MetersState;
use monitor::{MonitorAction, MonitorBuildContext, MonitorPointerContext, MonitorState};
use panels::{
    GraphNodeTarget, InspectorAction, InspectorBuildContext, InspectorPointerContext,
    InspectorState, MediaAction, MediaDragItem, MediaPanelState, MediaStream, PipelineGraphAction,
    PipelineGraphState, ProjectOptionsState,
};
use playback::FrameRenderer;
use plugin::PluginRegistry;
use preferences::{KeybindsDialog, SettingsDialog};
use project::{CompositionId, MediaAsset, MediaId, MediaKind, Project};
use render::{RenderPanelState, RenderPhase};
use timeline::{MediaDropPreviewSpec, SpeedDurationMode, TimelineAction, TimelineState, TrackKind};
use widgets::{component_style, WidgetGallery};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalPosition, LogicalSize, PhysicalPosition},
    event::{
        DeviceEvent, ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, TouchPhase,
        WindowEvent,
    },
    event_loop::{ActiveEventLoop, ControlFlow, DeviceEvents, EventLoop},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{CursorGrabMode, CursorIcon, CustomCursor, Theme as WindowTheme, Window, WindowId},
};

#[cfg(target_os = "macos")]
use muda::{
    accelerator::{Accelerator, Code, Modifiers},
    Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu,
};
#[cfg(target_os = "macos")]
use winit::platform::macos::EventLoopBuilderExtMacOS;

const MEDIA_PRESENCE_CHECK_INTERVAL: Duration = Duration::from_secs(1);

const POPUP_FADE_DURATION: Duration = Duration::from_millis(110);
const DOCK_EDGE: f32 = 32.0;
const FOCUS_FADE_SPEED: f32 = 18.0;
const RADIUS_SM: f32 = 5.0;
const RADIUS_MD: f32 = 8.0;
const RADIUS_LG: f32 = 11.0;
const TAB_ICON_SIZE: f32 = 16.0;

const TAB_IDLE: Color = Color::TRANSPARENT;
const DROP_BG: Color = Color::rgba8(0xff, 0xff, 0xff, 0x05);
const DROP_BORDER: Color = Color::rgba8(0xff, 0xff, 0xff, 0x0f);

#[repr(usize)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PanelKind {
    Media,
    Monitor,
    Inspector,
    ProjectOptions,
    History,
    Pipeline,
    Render,
    Timeline,
    Messages,
    Widgets,
    Meters,
}

#[derive(Clone, Copy)]
struct PanelInfo {
    layout_title: &'static str,
    title_key: &'static str,
    description_key: &'static str,
    icon: AppIcon,
}

macro_rules! panel_info {
    ($($variant:ident => ($layout_title:expr, $title_key:expr, $description_key:expr, $icon:expr)),+ $(,)?) => {
        impl PanelKind {
            const INFO: &'static [PanelInfo] = &[$(PanelInfo { layout_title: $layout_title, title_key: $title_key, description_key: $description_key, icon: $icon }),+];
            fn info(self) -> PanelInfo { Self::INFO[self as usize] }
            fn layout_title(self) -> &'static str { self.info().layout_title }
            fn display_title(self) -> String { i18n::text(self.info().title_key) }
            fn display_description(self) -> String { i18n::text(self.info().description_key) }
            fn from_title(title: &str) -> Option<Self> {
                [$(Self::$variant),+].into_iter().find(|panel| {
                    if !cfg!(debug_assertions) && *panel == Self::Widgets { return false; }
                    panel.layout_title() == title
                })
            }
        }
    };
}

panel_info! {
    Media => ("Media", "panel-media", "panel-media-description", AppIcon::Media),
    Monitor => ("Monitor", "panel-monitor", "panel-monitor-description", AppIcon::Monitor),
    Inspector => ("Inspector", "panel-inspector", "panel-inspector-description", AppIcon::Inspector),
    ProjectOptions => ("Composition Settings", "panel-project-options", "panel-project-options-description", AppIcon::Composition),
    History => ("History", "panel-history", "panel-history-description", AppIcon::History),
    Pipeline => ("Graph", "panel-pipeline", "panel-pipeline-description", AppIcon::Graph),
    Render => ("Render", "panel-render", "panel-render-description", AppIcon::Render),
    Timeline => ("Timeline", "panel-timeline", "panel-timeline-description", AppIcon::Timeline),
    Messages => ("Messages", "panel-messages", "panel-messages-description", AppIcon::Messages),
    Widgets => ("Widgets", "panel-widgets", "panel-widgets-description", AppIcon::Widgets),
    Meters => ("Meters", "panel-meters", "panel-meters-description", AppIcon::Meters),
}

impl PanelKind {
    #[cfg(debug_assertions)]
    const ALL: [Self; 10] = [
        Self::Media,
        Self::Monitor,
        Self::Inspector,
        Self::History,
        Self::Pipeline,
        Self::Render,
        Self::Timeline,
        Self::Messages,
        Self::Widgets,
        Self::Meters,
    ];
    #[cfg(not(debug_assertions))]
    const ALL: [Self; 9] = [
        Self::Media,
        Self::Monitor,
        Self::Inspector,
        Self::History,
        Self::Pipeline,
        Self::Render,
        Self::Timeline,
        Self::Messages,
        Self::Meters,
    ];
}

#[derive(Clone, Debug)]
enum GeneratorChoice {
    Plugin(String),
    Wasm(u64),
}

#[derive(Clone, Copy, Debug)]
enum PaletteKind {
    Commands,
    AddPanel(StackId),
    TimelineAdd {
        track: u32,
        time: f32,
        kind: TrackKind,
    },
    VideoClip {
        track: u32,
        time: f32,
    },
    PipelineAssignment(PipelineKind),
    FontFamily,
    NewPipeline,
    AddEffect {
        audio: bool,
    },
    NodeInsert {
        pipeline: u64,
        position: [f32; 2],
    },
    EffectClip {
        track: u32,
        time: f32,
    },
    ReplaceSelectedClips {
        min_video_tracks: usize,
        min_audio_tracks: usize,
    },
}

impl PaletteKind {
    fn is_add_menu(self) -> bool {
        matches!(
            self,
            Self::TimelineAdd { .. }
                | Self::VideoClip { .. }
                | Self::AddEffect { .. }
                | Self::NodeInsert { .. }
                | Self::EffectClip { .. }
        )
    }
}

default_state! {
    struct PaletteState {
        kind: Option<PaletteKind>,
        pending_open: Option<(PaletteKind, Option<Rect>)>,
        query: TextEdit = TextEdit::single_line(""),
        selected: usize,
        scroll: ScrollState,
        hovered: Option<usize>,
        path: Vec<String>,
        font_options: Vec<String>,
        replacement_excluded_media: HashSet<MediaId>,
        anchor: Option<Rect>,
        opened_at: Option<Instant>,
        closing: Option<(Instant, f32)>,
    }
}

impl PaletteState {
    fn close(&mut self) {
        self.pending_open = None;
        self.query.set_focused(false);
        if self.kind.is_some() && self.closing.is_none() {
            let now = Instant::now();
            self.closing = Some((now, self.opacity(now)));
        }
    }

    fn is_command_dialog(&self) -> bool {
        (matches!(self.kind, Some(PaletteKind::Commands)) && self.anchor.is_none())
            || self.pending_open.is_some_and(|(kind, anchor)| {
                matches!(kind, PaletteKind::Commands) && anchor.is_none()
            })
    }

    fn close_immediately(&mut self) {
        self.pending_open = None;
        self.kind = None;
        self.query.reset("");
        self.query.set_focused(false);
        self.selected = 0;
        self.scroll.offset = 0.0;
        self.hovered = None;
        self.path.clear();
        self.replacement_excluded_media.clear();
        self.anchor = None;
        self.opened_at = None;
        self.closing = None;
    }

    fn opacity(&self, now: Instant) -> f32 {
        fn eased(elapsed: Duration) -> f32 {
            let t = (elapsed.as_secs_f32() / POPUP_FADE_DURATION.as_secs_f32()).clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        }

        if let Some((started, start_opacity)) = self.closing {
            return start_opacity * (1.0 - eased(now.saturating_duration_since(started)));
        }
        self.opened_at
            .map_or(1.0, |started| eased(now.saturating_duration_since(started)))
    }

    fn finish_transitions(&mut self, now: Instant) {
        if self.closing.is_some_and(|(started, _)| {
            now.saturating_duration_since(started) >= POPUP_FADE_DURATION
        }) {
            self.kind = None;
            self.query.reset("");
            self.selected = 0;
            self.scroll.offset = 0.0;
            self.hovered = None;
            self.path.clear();
            self.replacement_excluded_media.clear();
            self.anchor = None;
            self.opened_at = None;
            self.closing = None;
        }
    }

    fn advance_after_frame(&mut self) {
        let Some((kind, anchor)) = self.pending_open.take() else {
            return;
        };
        self.kind = Some(kind);
        self.query.reset("");
        self.query.set_focused(true);
        self.selected = 0;
        self.scroll.offset = 0.0;
        self.hovered = None;
        self.path.clear();
        self.anchor = anchor;
        self.opened_at = Some(Instant::now());
        self.closing = None;
    }

    fn tick(&mut self, dt: f32) {
        self.query.tick(dt);
    }

    fn is_animating(&self) -> bool {
        self.pending_open.is_some()
            || self
                .opened_at
                .is_some_and(|started| started.elapsed() < POPUP_FADE_DURATION)
            || self.closing.is_some()
            || self.query.is_animating()
    }
}

#[derive(Clone, Debug)]
enum PendingDiscardAction {
    Exit,
    NewProject,
    OpenProjectDialog,
    LoadProject(PathBuf),
}

struct MediaInsertionCursor {
    anchor_track: u32,
    video_tracks: Vec<u32>,
    audio_tracks: Vec<u32>,
    time: f32,
}

impl MediaInsertionCursor {
    fn new(_timeline: &mut TimelineState, anchor_track: u32, time: f32) -> Self {
        Self {
            anchor_track,
            video_tracks: Vec::new(),
            audio_tracks: Vec::new(),
            time,
        }
    }

    fn tracks(
        &mut self,
        timeline: &mut TimelineState,
        video: usize,
        audio: usize,
    ) -> (Vec<u32>, Vec<u32>) {
        if self.video_tracks.len() < video {
            self.video_tracks = timeline.media_tracks_near(self.anchor_track, false, video);
        }
        if self.audio_tracks.len() < audio {
            self.audio_tracks = timeline.media_tracks_near(self.anchor_track, true, audio);
        }
        (
            self.video_tracks[..video].to_vec(),
            self.audio_tracks[..audio].to_vec(),
        )
    }

    fn advance(&mut self, duration: f32) {
        self.time += duration;
    }
}

#[derive(Clone, Copy)]
enum NewCompositionMode {
    Blank,
    FromSelection,
    Rename(CompositionId),
}

struct NewCompositionDialog {
    editor: TextEdit,
    mode: NewCompositionMode,
    animation: PopupAnimation,
}

impl NewCompositionDialog {
    fn new(mode: NewCompositionMode) -> Self {
        let mut editor = TextEdit::single_line("Composition");
        editor.set_focused(true);
        Self {
            editor,
            mode,
            animation: PopupAnimation::new(),
        }
    }

    fn rename(composition: CompositionId, name: &str) -> Self {
        let mut editor = TextEdit::single_line(name);
        editor.set_focused(true);
        Self {
            editor,
            mode: NewCompositionMode::Rename(composition),
            animation: PopupAnimation::new(),
        }
    }

    fn title(&self) -> &'static str {
        match self.mode {
            NewCompositionMode::Blank => "New Composition",
            NewCompositionMode::FromSelection => "Add to new Composition",
            NewCompositionMode::Rename(_) => "Rename Composition",
        }
    }
}

popup_editor_dialog_methods!(NewCompositionDialog, editor);

struct SpeedDurationDialog {
    editor: TextEdit,
    mode: SpeedDurationMode,
    animation: PopupAnimation,
}

impl SpeedDurationDialog {
    fn new(timeline: &TimelineState, project: &Project) -> Self {
        let value = timeline.selected_speed(project).unwrap_or(1.0) * 100.0;
        let mut editor = TextEdit::single_line(format!("{value:.2}"));
        editor.set_focused(true);
        Self {
            editor,
            mode: SpeedDurationMode::SpeedPercent,
            animation: PopupAnimation::new(),
        }
    }

    fn value(&self) -> Option<f32> {
        self.editor
            .text()
            .trim()
            .trim_end_matches('%')
            .trim_end_matches('s')
            .trim()
            .parse::<f32>()
            .ok()
            .filter(|value| value.is_finite() && *value > 0.0)
    }

    fn set_mode(&mut self, mode: SpeedDurationMode, timeline: &TimelineState, project: &Project) {
        self.mode = mode;
        let value = match mode {
            SpeedDurationMode::SpeedPercent => {
                timeline.selected_speed(project).unwrap_or(1.0) * 100.0
            }
            SpeedDurationMode::PerClipDuration => timeline.selected_duration().unwrap_or(1.0),
            SpeedDurationMode::TotalDuration => {
                timeline.selected_total_logical_duration().max(0.001)
            }
        };
        self.editor.reset(format!("{value:.3}"));
        self.editor.set_focused(true);
    }
}

popup_editor_dialog_methods!(SpeedDurationDialog, editor);

#[derive(Clone, Debug)]
enum PaletteTarget {
    Command(EditorCommand),
    Submenu(String),
}

#[derive(Clone, Debug)]
struct PaletteEntry {
    label: String,
    detail: String,
    path: Vec<String>,
    aliases: Vec<Vec<String>>,
    icon: AppIcon,
    target: PaletteTarget,
}

impl PaletteEntry {
    fn breadcrumb(&self) -> String {
        self.path
            .iter()
            .map(String::as_str)
            .chain(std::iter::once(self.label.as_str()))
            .collect::<Vec<_>>()
            .join(" ▶ ")
    }

    fn is_submenu(&self) -> bool {
        matches!(&self.target, PaletteTarget::Submenu(_))
    }

    fn alias(mut self, path: Vec<String>) -> Self {
        self.aliases.push(path);
        self
    }
}

#[derive(Debug)]
struct DragMotion {
    start: [f32; 2],
    current: [f32; 2],
    dragging: bool,
    detached: bool,
}

impl DragMotion {
    fn new(point: [f32; 2]) -> Self {
        Self {
            start: point,
            current: point,
            dragging: false,
            detached: false,
        }
    }
}

#[derive(Debug)]
enum DockDrag {
    Tab {
        tab: TabId,
        source_stack: StackId,
        original_index: usize,
    },
    Panel {
        source_stack: StackId,
    },
}

#[derive(Debug)]
enum DragState {
    Splitter {
        id: SplitId,
        parent_rect: Rect,
    },
    Dock {
        item: DockDrag,
        motion: DragMotion,
    },
    Media {
        items: Vec<MediaDragItem>,
        motion: DragMotion,
        open_on_click: Option<CompositionId>,
    },
}

#[derive(Clone, Debug)]
struct ExternalDragItem {
    path: PathBuf,
    preview: Option<MediaDropPreviewSpec>,
}

struct PendingProjectLoad {
    path: PathBuf,
    project: Project,
    missing: Vec<(MediaId, PathBuf)>,
}

struct MissingMediaDialog {
    pending: Option<PendingProjectLoad>,
    missing_paths: Vec<PathBuf>,
    animation: PopupAnimation,
}

impl MissingMediaDialog {
    fn new(path: PathBuf, project: Project, missing: Vec<(MediaId, PathBuf)>) -> Self {
        let missing_paths = missing.iter().map(|(_, path)| path.clone()).collect();
        Self {
            pending: Some(PendingProjectLoad {
                path,
                project,
                missing,
            }),
            missing_paths,
            animation: PopupAnimation::new(),
        }
    }

    fn missing(&self) -> &[PathBuf] {
        &self.missing_paths
    }

    fn take(&mut self) -> Option<PendingProjectLoad> {
        self.pending.take()
    }
}

popup_dialog_methods!(MissingMediaDialog);

enum Modal {
    About(SimpleDialog),
    LayoutSave(LayoutSaveDialog),
    Discard(ActionDialog),
    Busy(ActionDialog),
    Settings(Box<SettingsDialog>),
    Keybinds(Box<KeybindsDialog>),
    Composition(NewCompositionDialog),
    SpeedDuration(SpeedDurationDialog),
    MissingMedia(MissingMediaDialog),
}

impl Modal {
    fn tick(&mut self, dt: f32) {
        match self {
            Self::LayoutSave(dialog) => dialog.tick(dt),
            Self::Composition(dialog) => dialog.editor.tick(dt),
            Self::SpeedDuration(dialog) => dialog.editor.tick(dt),
            Self::Settings(dialog) => dialog.tick(dt),
            Self::Keybinds(dialog) => dialog.tick(dt),
            Self::About(_) | Self::Discard(_) | Self::Busy(_) | Self::MissingMedia(_) => {}
        }
    }

    fn is_animating(&self) -> bool {
        match self {
            Self::LayoutSave(dialog) => dialog.is_animating(),
            Self::Composition(dialog) => dialog.is_animating(),
            Self::SpeedDuration(dialog) => dialog.is_animating(),
            Self::Settings(dialog) => dialog.is_animating(),
            Self::Keybinds(dialog) => dialog.is_animating(),
            Self::About(dialog) => dialog.is_animating(),
            Self::MissingMedia(dialog) => dialog.is_animating(),
            Self::Discard(dialog) | Self::Busy(dialog) => dialog.is_animating(),
        }
    }

    fn restart_entry_animation(&mut self) {
        match self {
            Self::About(dialog) => dialog.animation.restart(),
            Self::LayoutSave(dialog) => dialog.animation.restart(),
            Self::Discard(dialog) | Self::Busy(dialog) => dialog.animation.restart(),
            Self::Composition(dialog) => dialog.animation.restart(),
            Self::SpeedDuration(dialog) => dialog.animation.restart(),
            Self::MissingMedia(dialog) => dialog.animation.restart(),
            Self::Settings(dialog) => dialog.restart_entry_animation(),
            Self::Keybinds(dialog) => dialog.restart_entry_animation(),
        }
    }

    fn finished(&self, now: Instant) -> bool {
        match self {
            Self::About(dialog) => dialog.finished(now),
            Self::LayoutSave(dialog) => dialog.finished(now),
            Self::Discard(dialog) | Self::Busy(dialog) => dialog.finished(now),
            Self::Settings(dialog) => dialog.is_closed(),
            Self::Keybinds(dialog) => dialog.is_closed(),
            Self::Composition(dialog) => dialog.finished(now),
            Self::SpeedDuration(dialog) => dialog.finished(now),
            Self::MissingMedia(dialog) => dialog.finished(now),
        }
    }
}

fn make_razor_cursor(event_loop: &ActiveEventLoop) -> Result<CustomCursor> {
    const CURSOR_SIZE: u32 = 32;
    let source = image::RgbaImage::from_raw(
        assets::ICON_SIZE,
        assets::ICON_SIZE,
        assets::icon_rgba(AppIcon::ClipCut).to_vec(),
    )
    .context("build razor cursor image")?;
    let rgba = image::imageops::resize(
        &source,
        CURSOR_SIZE,
        CURSOR_SIZE,
        image::imageops::FilterType::Lanczos3,
    )
    .into_raw();
    let source = CustomCursor::from_rgba(rgba, CURSOR_SIZE as u16, CURSOR_SIZE as u16, 3, 8)
        .context("create razor cursor image")?;
    Ok(event_loop.create_custom_cursor(source))
}

struct EditorWindowState {
    window: Arc<Window>,
    renderer: Renderer,
    icons: Icons,
    about_logos: AboutLogos,
    gui: Gui,
    input: InputState,
    cursor_physical: [f64; 2],
    cursor: [f32; 2],
    modifiers: ModifiersState,
    dock: DockState,
    palette: PaletteState,
    snapshot: LayoutSnapshot,
    drag: Option<DragState>,
    external_drag_items: Vec<ExternalDragItem>,
    external_drag_uses_window_cursor: bool,
    animated_drop_preview: Option<Rect>,
    focus_levels: HashMap<StackId, f32>,
    focus_frame: Instant,
    #[cfg(not(target_os = "macos"))]
    app_menu: AppMenuState,
    #[cfg(not(target_os = "macos"))]
    app_menu_keyboard: AppMenuKeyboardState,
    modal: Option<Modal>,
    modal_queue: VecDeque<Modal>,
    waveform_textures: waveform::WaveformTextures,
    playback: FrameRenderer,
    monitor: MonitorState,
    razor_cursor_active: bool,
    touch_gesture_cursor: Option<CursorIcon>,
    value_drag_cursor_locked: bool,
    value_drag_cursor_anchor: Option<[f64; 2]>,
    ime_sync: ImeSyncState,
}

impl EditorWindowState {
    fn new(
        event_loop: &ActiveEventLoop,
        dock: DockState,
        project: &Project,
        effects: &EffectRuntime,
        plugins: &PluginRegistry,
        position: Option<PhysicalPosition<i32>>,
    ) -> Result<Self> {
        let mut attributes = Window::default_attributes()
            .with_title("Kama Studio")
            .with_inner_size(LogicalSize::new(900.0, 640.0));
        if let Some(position) = position {
            attributes = attributes.with_position(position);
        }
        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .context("create detached editor window")?,
        );
        theme::set_system_appearance(matches!(window.theme(), Some(WindowTheme::Light)));
        let mut renderer = pollster::block_on(Renderer::new(window.clone()))?;
        let icons = Icons::load(&mut renderer)?;
        let about_logos = AboutLogos::load(&mut renderer)?;
        let mut waveform_textures = waveform::WaveformTextures::default();
        waveform_textures.queue_missing(project);
        let monitor = MonitorState::default();
        let mut playback = FrameRenderer::new(&renderer, effects, plugins);
        for asset in &project.media {
            if matches!(asset.kind, MediaKind::WasmPlugin) {
                if let Err(error) = playback.precompile_wasm(&asset.path) {
                    messages::error(
                        "WASM plugin",
                        format!("precompile failed for {}: {error:#}", asset.path.display()),
                    );
                }
            }
        }
        Ok(Self {
            window,
            renderer,
            icons,
            about_logos,
            gui: Gui::new(),
            input: InputState::default(),
            cursor_physical: [0.0, 0.0],
            cursor: [0.0, 0.0],
            modifiers: ModifiersState::empty(),
            dock,
            palette: PaletteState::default(),
            snapshot: LayoutSnapshot::default(),
            drag: None,
            external_drag_items: Vec::new(),
            external_drag_uses_window_cursor: false,
            animated_drop_preview: None,
            focus_levels: HashMap::new(),
            focus_frame: Instant::now(),
            #[cfg(not(target_os = "macos"))]
            app_menu: AppMenuState::Closed,
            #[cfg(not(target_os = "macos"))]
            app_menu_keyboard: AppMenuKeyboardState::default(),
            modal: None,
            modal_queue: VecDeque::new(),
            waveform_textures,
            playback,
            monitor,
            razor_cursor_active: false,
            touch_gesture_cursor: None,
            value_drag_cursor_locked: false,
            value_drag_cursor_anchor: None,
            ime_sync: ImeSyncState::default(),
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct ImeSyncState {
    allowed: bool,
    area: Option<Rect>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct ImeSyncUpdate {
    allowed: Option<bool>,
    area: Option<Rect>,
}

impl ImeSyncState {
    fn update(&mut self, area: Option<Rect>) -> ImeSyncUpdate {
        let allowed = area.is_some();
        let update = ImeSyncUpdate {
            allowed: (allowed != self.allowed).then_some(allowed),
            area: (area != self.area).then_some(area).flatten(),
        };
        self.allowed = allowed;
        self.area = area;
        update
    }
}

struct EditorApp {
    window: Arc<Window>,
    renderer: Renderer,
    icons: Icons,
    about_logos: AboutLogos,
    gui: Gui,
    input: InputState,
    cursor_physical: [f64; 2],
    cursor: [f32; 2],
    modifiers: ModifiersState,
    dock: DockState,
    palette: PaletteState,
    command_registry: CommandRegistry,
    plugin_paths: String,
    command_queue: CommandQueue,
    snapshot: LayoutSnapshot,
    drag: Option<DragState>,
    external_drag_items: Vec<ExternalDragItem>,
    external_drag_uses_window_cursor: bool,
    ignored_external_drops: HashSet<PathBuf>,
    animated_drop_preview: Option<Rect>,
    focus_levels: HashMap<StackId, f32>,
    focus_frame: Instant,
    #[cfg(not(target_os = "macos"))]
    app_menu: AppMenuState,
    #[cfg(not(target_os = "macos"))]
    app_menu_keyboard: AppMenuKeyboardState,
    modal: Option<Modal>,
    modal_queue: VecDeque<Modal>,
    exit_requested: bool,
    #[cfg(target_os = "macos")]
    native_menu: NativeMenu,
    editor: EditorSession,
    next_media_presence_check: Instant,
    plugins: PluginRegistry,
    effects: EffectRuntime,
    waveform_textures: waveform::WaveformTextures,
    history_panel: HistoryPanelState,
    audio: AudioPlayback,
    media: MediaPanelState,
    playback: FrameRenderer,
    monitor: MonitorState,
    inspector: InspectorState,
    project_options: ProjectOptionsState,
    pipeline_graph: PipelineGraphState,
    render_panel: RenderPanelState,
    widgets: WidgetGallery,
    meters: MetersState,
    messages: MessagesState,
    razor_cursor: CustomCursor,
    razor_cursor_active: bool,
    touch_gesture_cursor: Option<CursorIcon>,
    value_drag_cursor_locked: bool,
    value_drag_cursor_anchor: Option<[f64; 2]>,
    ime_sync: ImeSyncState,
    secondary_windows: HashMap<WindowId, EditorWindowState>,
}

impl EditorApp {
    fn new(event_loop: &ActiveEventLoop) -> Result<Self> {
        let attributes = Window::default_attributes()
            .with_title("Kama Studio")
            .with_inner_size(LogicalSize::new(1280.0, 820.0));
        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .context("create window")?,
        );
        theme::set_system_appearance(matches!(window.theme(), Some(WindowTheme::Light)));
        let mut renderer = pollster::block_on(Renderer::new(window.clone()))?;
        let icons = Icons::load(&mut renderer)?;
        let about_logos = AboutLogos::load(&mut renderer)?;
        let razor_cursor = make_razor_cursor(event_loop)?;
        let mut command_registry = CommandRegistry::editor_defaults();
        preferences::load(&mut command_registry);
        #[cfg(target_os = "macos")]
        let native_menu = NativeMenu::install(&command_registry);

        let plugin_paths = preferences::load_plugin_paths();
        let mut plugins =
            PluginRegistry::load_default(&plugin_paths).context("load Kama plugins")?;
        plugins.validate_gpu(renderer.device());
        let startup_project = startup_project_path();
        let mut project = Project::new();
        let project_path = None;
        project.reconcile_plugin_metadata(&plugins);
        let mut timeline = TimelineState::default();
        timeline.load_document(project.active_composition().timeline.clone());
        timeline.ensure_composition_visual_pipelines(&plugins);
        timeline.reconcile_pipeline_overrides(&project);
        let mut waveform_textures = waveform::WaveformTextures::default();
        waveform_textures.queue_missing(&project);
        let mut effects = EffectRuntime::default();
        effects.rebuild(&project.pipelines);
        let monitor = MonitorState::default();
        let mut playback = FrameRenderer::new(&renderer, &effects, &plugins);
        for asset in &project.media {
            if matches!(asset.kind, MediaKind::WasmPlugin) {
                if let Err(error) = playback.precompile_wasm(&asset.path) {
                    messages::error(
                        "WASM plugin",
                        format!("precompile failed for {}: {error:#}", asset.path.display()),
                    );
                }
            }
        }
        let render_panel = RenderPanelState::new(&project, &timeline);
        let editor = EditorSession::new(project, timeline, project_path);
        let mut editor_app = Self {
            window,
            renderer,
            icons,
            about_logos,
            gui: Gui::new(),
            input: InputState::default(),
            cursor_physical: [0.0, 0.0],
            cursor: [0.0, 0.0],
            modifiers: ModifiersState::empty(),
            dock: default_dock(),
            palette: PaletteState::default(),
            command_registry,
            plugin_paths,
            command_queue: CommandQueue::default(),
            snapshot: LayoutSnapshot::default(),
            drag: None,
            external_drag_items: Vec::new(),
            external_drag_uses_window_cursor: false,
            ignored_external_drops: HashSet::new(),
            animated_drop_preview: None,
            focus_levels: HashMap::new(),
            focus_frame: Instant::now(),
            #[cfg(not(target_os = "macos"))]
            app_menu: AppMenuState::Closed,
            #[cfg(not(target_os = "macos"))]
            app_menu_keyboard: AppMenuKeyboardState::default(),
            modal: None,
            modal_queue: VecDeque::new(),
            exit_requested: false,
            #[cfg(target_os = "macos")]
            native_menu,
            editor,
            next_media_presence_check: Instant::now() + MEDIA_PRESENCE_CHECK_INTERVAL,
            plugins,
            effects,
            waveform_textures,
            history_panel: HistoryPanelState::default(),
            audio: AudioPlayback::new(),
            media: MediaPanelState::default(),
            playback,
            monitor,
            inspector: InspectorState::default(),
            project_options: ProjectOptionsState::default(),
            pipeline_graph: PipelineGraphState::default(),
            render_panel,
            widgets: WidgetGallery::default(),
            meters: MetersState::default(),
            messages: MessagesState::default(),
            razor_cursor,
            razor_cursor_active: false,
            touch_gesture_cursor: None,
            value_drag_cursor_locked: false,
            value_drag_cursor_anchor: None,
            ime_sync: ImeSyncState::default(),
            secondary_windows: HashMap::new(),
        };
        editor_app.update_window_title();
        if let Some(path) = startup_project {
            if let Err(error) = editor_app.load_project_unchecked(&path) {
                messages::error(
                    "Project",
                    format!("could not open {}: {error:#}", path.display()),
                );
            }
        }
        Ok(editor_app)
    }

    fn swap_window_state(&mut self, state: &mut EditorWindowState) {
        std::mem::swap(&mut self.window, &mut state.window);
        std::mem::swap(&mut self.renderer, &mut state.renderer);
        std::mem::swap(&mut self.icons, &mut state.icons);
        std::mem::swap(&mut self.about_logos, &mut state.about_logos);
        std::mem::swap(&mut self.gui, &mut state.gui);
        std::mem::swap(&mut self.input, &mut state.input);
        std::mem::swap(&mut self.cursor_physical, &mut state.cursor_physical);
        std::mem::swap(&mut self.cursor, &mut state.cursor);
        std::mem::swap(&mut self.modifiers, &mut state.modifiers);
        std::mem::swap(&mut self.dock, &mut state.dock);
        std::mem::swap(&mut self.palette, &mut state.palette);
        std::mem::swap(&mut self.snapshot, &mut state.snapshot);
        std::mem::swap(&mut self.drag, &mut state.drag);
        std::mem::swap(
            &mut self.external_drag_items,
            &mut state.external_drag_items,
        );
        std::mem::swap(
            &mut self.external_drag_uses_window_cursor,
            &mut state.external_drag_uses_window_cursor,
        );
        std::mem::swap(
            &mut self.animated_drop_preview,
            &mut state.animated_drop_preview,
        );
        std::mem::swap(&mut self.focus_levels, &mut state.focus_levels);
        std::mem::swap(&mut self.focus_frame, &mut state.focus_frame);
        #[cfg(not(target_os = "macos"))]
        std::mem::swap(&mut self.app_menu, &mut state.app_menu);
        #[cfg(not(target_os = "macos"))]
        std::mem::swap(&mut self.app_menu_keyboard, &mut state.app_menu_keyboard);
        std::mem::swap(&mut self.modal, &mut state.modal);
        std::mem::swap(&mut self.modal_queue, &mut state.modal_queue);
        std::mem::swap(&mut self.waveform_textures, &mut state.waveform_textures);
        std::mem::swap(&mut self.playback, &mut state.playback);
        std::mem::swap(&mut self.monitor, &mut state.monitor);
        std::mem::swap(
            &mut self.razor_cursor_active,
            &mut state.razor_cursor_active,
        );
        std::mem::swap(
            &mut self.touch_gesture_cursor,
            &mut state.touch_gesture_cursor,
        );
        std::mem::swap(
            &mut self.value_drag_cursor_locked,
            &mut state.value_drag_cursor_locked,
        );
        std::mem::swap(
            &mut self.value_drag_cursor_anchor,
            &mut state.value_drag_cursor_anchor,
        );
        std::mem::swap(&mut self.ime_sync, &mut state.ime_sync);
    }

    fn activate_window(&mut self, window_id: WindowId) -> bool {
        if self.window.id() == window_id {
            return true;
        }
        let Some(mut target) = self.secondary_windows.remove(&window_id) else {
            return false;
        };
        let previous = self.window.id();
        self.swap_window_state(&mut target);
        self.secondary_windows.insert(previous, target);
        true
    }

    fn window_count(&self) -> usize {
        1 + self.secondary_windows.len()
    }

    fn request_redraw_all(&self) {
        self.window.request_redraw();
        for state in self.secondary_windows.values() {
            state.window.request_redraw();
        }
    }

    fn close_window(&mut self, window_id: WindowId) -> bool {
        if self.window_count() <= 1 {
            return false;
        }
        if self.window.id() != window_id {
            return self.secondary_windows.remove(&window_id).is_some();
        }
        let Some(next_id) = self.secondary_windows.keys().next().copied() else {
            return false;
        };
        let mut next = self
            .secondary_windows
            .remove(&next_id)
            .expect("selected detached window must exist");
        self.swap_window_state(&mut next);

        drop(next);
        true
    }

    fn create_detached_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        transfer: DockTransfer,
        screen_position: Option<[f64; 2]>,
    ) -> Result<WindowId> {
        let position = screen_position
            .map(|point| PhysicalPosition::new((point[0] - 80.0) as i32, (point[1] - 20.0) as i32));
        let dock = DockState::from_spec(transfer.into_layout_spec());
        let state = EditorWindowState::new(
            event_loop,
            dock,
            &self.editor.project,
            &self.effects,
            &self.plugins,
            position,
        )?;
        let id = state.window.id();
        state.window.request_redraw();
        self.secondary_windows.insert(id, state);
        self.update_window_title();
        Ok(id)
    }

    fn cursor_screen_physical(&self) -> Option<[f64; 2]> {
        let origin = self.window.inner_position().ok()?;
        Some([
            origin.x as f64 + self.cursor_physical[0],
            origin.y as f64 + self.cursor_physical[1],
        ])
    }

    fn cursor_inside_active_window(&self) -> bool {
        let size = self.window.inner_size();
        self.cursor_physical[0] >= 0.0
            && self.cursor_physical[1] >= 0.0
            && self.cursor_physical[0] < size.width as f64
            && self.cursor_physical[1] < size.height as f64
    }

    fn logical_point_for_window(
        &self,
        window_id: WindowId,
        position: PhysicalPosition<f64>,
    ) -> Option<[f32; 2]> {
        let scale = if self.window.id() == window_id {
            self.renderer.scale_factor() as f64
        } else {
            self.secondary_windows
                .get(&window_id)?
                .renderer
                .scale_factor() as f64
        };
        Some([(position.x / scale) as f32, (position.y / scale) as f32])
    }

    fn screen_point_for_window(
        &self,
        window_id: WindowId,
        position: PhysicalPosition<f64>,
    ) -> Option<[f64; 2]> {
        let window = if self.window.id() == window_id {
            &self.window
        } else {
            &self.secondary_windows.get(&window_id)?.window
        };
        let origin = window.inner_position().ok()?;
        Some([origin.x as f64 + position.x, origin.y as f64 + position.y])
    }

    fn set_active_cursor_from_screen(&mut self, screen: [f64; 2]) {
        let Ok(origin) = self.window.inner_position() else {
            return;
        };
        self.cursor_physical = [screen[0] - origin.x as f64, screen[1] - origin.y as f64];
        let scale = self.renderer.scale_factor() as f64;
        self.cursor = [
            (self.cursor_physical[0] / scale) as f32,
            (self.cursor_physical[1] / scale) as f32,
        ];
        self.input.cursor = self.cursor;
        self.pointer_moved();
    }

    fn window_dock_empty(&self, window_id: WindowId) -> bool {
        if self.window.id() == window_id {
            self.dock.is_empty()
        } else {
            self.secondary_windows
                .get(&window_id)
                .is_some_and(|state| state.dock.is_empty())
        }
    }

    fn restore_dock_transfer(&mut self, window_id: WindowId, transfer: DockTransfer) -> bool {
        if !self.activate_window(window_id) {
            return false;
        }
        if self.dock.is_empty() {
            self.dock = DockState::from_spec(transfer.into_layout_spec());
            return true;
        }
        let Some(stack) = self.dock.focused else {
            return false;
        };
        self.dock
            .drop_external(transfer, stack, DropZone::Center, None)
            .is_some()
    }

    fn window_at_screen_point(&self, screen: [f64; 2]) -> Option<(WindowId, [f32; 2])> {
        let active_id = self.window.id();
        self.secondary_windows.iter().find_map(|(&id, state)| {
            if id == active_id {
                return None;
            }
            let origin = state.window.inner_position().ok()?;
            let size = state.window.inner_size();
            let x = screen[0] - origin.x as f64;
            let y = screen[1] - origin.y as f64;
            if x < 0.0 || y < 0.0 || x >= size.width as f64 || y >= size.height as f64 {
                return None;
            }
            let scale = state.renderer.scale_factor() as f64;
            Some((id, [(x / scale) as f32, (y / scale) as f32]))
        })
    }

    fn take_dock_transfer(&mut self) -> Option<DockTransfer> {
        let drag = self.drag.take()?;
        let DragState::Dock { item, motion } = drag else {
            self.drag = Some(drag);
            return None;
        };
        if !motion.dragging {
            self.drag = Some(DragState::Dock { item, motion });
            return None;
        }
        self.animated_drop_preview = None;
        match item {
            DockDrag::Tab {
                tab, source_stack, ..
            } => self.dock.detach_tab(source_stack, tab),
            DockDrag::Panel { source_stack } => self.dock.detach_stack(source_stack),
        }
    }

    fn drop_dock_transfer(&mut self, transfer: DockTransfer, point: [f32; 2]) -> bool {
        let Some(target) = self.snapshot.stack_at(point) else {
            return false;
        };
        let zone = if target.tab_bar.contains(point) {
            DropZone::Center
        } else {
            drop_zone(target.rect, point)
        };
        let insert = (zone == DropZone::Center)
            .then(|| insertion_index(&self.snapshot, target.stack.id, TabId(u64::MAX), point[0]));
        self.dock
            .drop_external(transfer, target.stack.id, zone, insert)
            .is_some()
    }

    fn sync_inactive_windows_after_project_reset(&mut self) {
        for state in self.secondary_windows.values_mut() {
            state.waveform_textures.clear();
            state.waveform_textures.queue_missing(&self.editor.project);
            state.playback.clear_caches();
            state.monitor.clear_captured_frame();
            state
                .playback
                .sync_compiled_effects(&state.renderer, &self.effects, &self.plugins);
        }
    }

    fn sync_cursor(&mut self) {
        if let Some(cursor) = self.touch_gesture_cursor {
            self.window.set_cursor(cursor);
            self.razor_cursor_active = false;
            return;
        }
        let razor_cursor_active = self.modal.is_none()
            && self.palette.kind.is_none()
            && self.drag.is_none()
            && self
                .editor
                .timeline
                .razor_cursor_at(&self.snapshot, self.cursor);
        if razor_cursor_active {
            if !self.razor_cursor_active {
                self.window.set_cursor(self.razor_cursor.clone());
            }
        } else {
            let cursor = match self.gui.cursor_shape() {
                CursorShape::Pointer => CursorIcon::Pointer,
                CursorShape::EwResize => CursorIcon::EwResize,
                CursorShape::NsResize => CursorIcon::NsResize,
                CursorShape::ZoomIn => CursorIcon::ZoomIn,
                CursorShape::ZoomOut => CursorIcon::ZoomOut,
                CursorShape::Grab => CursorIcon::Grab,
                CursorShape::Grabbing => CursorIcon::Grabbing,
                CursorShape::Auto | CursorShape::Arrow | CursorShape::Passthrough => {
                    CursorIcon::Default
                }
            };
            self.window.set_cursor(cursor);
        }
        self.razor_cursor_active = razor_cursor_active;
    }

    fn workspace_rect(&self) -> Rect {
        let width = self.renderer.logical_width();
        let height = self.renderer.logical_height();
        let top = app_menu_height();
        kama_ui::layout::column(
            Rect::new(0.0, 0.0, width, height.max(top + 1.0)),
            &[
                kama_ui::layout::Item::height(top),
                kama_ui::layout::Item::fill(),
            ],
            0.0,
            0.0,
            ui::Align::Start,
            None,
        )[1]
    }

    fn draw(&mut self) -> Result<()> {
        self.remove_missing_media_files();
        let width = self.renderer.logical_width();
        let height = self.renderer.logical_height();
        let workspace = self.workspace_rect();
        let text_scale = self.renderer.scale_factor();
        self.snapshot = {
            let (dock, gui) = (&mut self.dock, &mut self.gui);
            dock.layout_with_tab_measure(workspace, |title| {
                gui.measure_text_ink_width(title, 10.5, text_scale)
            })
        };
        let focused_panel = self.focused_panel();
        self.sync_panel_focus(focused_panel);
        self.editor.timeline.tick(
            &self.snapshot,
            self.editor.project.active_settings().frame_rate as f32,
        );
        self.refresh_window_title_if_needed();
        self.audio.set_master_muted(self.monitor.master_muted());
        self.audio
            .sync(&self.editor.project, &self.editor.timeline, &self.plugins);
        let audio_lead = self.audio.clock_lead();
        if audio_lead > 0.0 {
            self.editor.timeline.sync_forward_playhead(
                self.editor.timeline.playhead() + audio_lead,
                &self.snapshot,
            );
        }
        self.editor
            .timeline
            .set_audio_levels(self.audio.track_levels());
        let render_edit_revision = self.editor.history.revision();
        let render_editing = self.editor.history_gesture.is_some();
        let render_interactive = render_editing
            || self.editor.timeline.is_playing()
            || self.editor.timeline.is_scrubbing();
        self.render_panel.tick_render(
            &self.renderer,
            &self.editor.project,
            &self.editor.timeline,
            &self.plugins,
            (render_edit_revision, render_editing, render_interactive),
        );
        self.render_panel.sync_timeline_ranges(
            &mut self.editor.timeline,
            self.editor.project.active_composition,
            self.editor.project.active_settings().frame_rate,
        );
        let render_cache_preview = self.render_panel.cached_preview(
            self.editor.timeline.playhead(),
            self.editor.project.active_settings().frame_rate,
            self.editor.project.active_composition,
        );
        let preview_size = self.monitor.preview_render_size(&self.editor.project);
        let render_scale = self.monitor.preview_render_scale(&self.editor.project);
        let captured_preview = self.monitor.captured_preview();
        self.playback.refresh_preview(
            &mut self.renderer,
            &self.editor.project,
            &self.editor.timeline,
            &self.effects,
            &self.plugins,
            render_cache_preview.as_ref(),
            preview_size,
            render_scale,
            captured_preview,
        )?;
        self.inspector
            .sync_color_picker_textures(&mut self.renderer)?;
        self.pipeline_graph
            .sync_color_picker_textures(&mut self.renderer)?;
        self.project_options
            .sync_color_picker_textures(&mut self.renderer)?;
        self.widgets
            .sync_color_picker_textures(&mut self.renderer)?;
        if let Some(Modal::Settings(dialog)) = &mut self.modal {
            dialog.sync_textures(&mut self.renderer)?;
        }
        self.waveform_textures.poll(&mut self.editor.project);
        self.waveform_textures
            .sync(&mut self.renderer, &self.editor.project)?;

        let transition_now = Instant::now();
        self.palette.finish_transitions(transition_now);
        if self
            .modal
            .as_ref()
            .is_some_and(|modal| modal.finished(transition_now))
        {
            self.modal = None;
            self.advance_modal_queue();
        }
        let palette_entries = self.palette.kind.map(|_| {
            palette_entries(
                &self.palette,
                &self.editor.project,
                &self.plugins,
                &self.command_registry,
            )
        });
        let snapshot = &self.snapshot;
        let drag = &self.drag;
        let external_drag_items = &self.external_drag_items;
        let cursor = self.cursor;
        let project = &self.editor.project;
        let timeline = &self.editor.timeline;
        let history = &self.editor.history;
        let history_panel = &mut self.history_panel;
        let icons = self.icons;
        let about_logos = self.about_logos;
        let focused_stack = self.dock.focused;
        let maximized_stack = self.dock.maximized_stack();
        let now = Instant::now();
        let dt = now
            .saturating_duration_since(self.focus_frame)
            .as_secs_f32()
            .min(0.05);
        self.focus_frame = now;
        let theme_animating = theme::tick(dt);
        self.meters.tick(self.audio.master_levels(), dt);
        if let Some(modal) = &mut self.modal {
            modal.tick(dt);
        }
        self.palette.tick(dt);
        self.widgets.tick(dt);
        self.media.tick(dt);
        self.inspector.tick(dt);
        self.project_options.tick(dt);
        self.pipeline_graph.tick(dt);
        self.render_panel.tick_ui(dt);
        self.monitor.tick(dt);
        let widgets_animating = self.widgets.is_animating();
        let media_animating = self.media.is_animating();
        let media = &self.media;
        let inspector_animating = self.inspector.is_animating();
        let project_options_animating = self.project_options.is_animating();
        let graph_animating = self.pipeline_graph.is_animating();
        let monitor_animating = self.monitor.is_animating();
        let render_animating = self.render_panel.is_animating();
        let meters_animating = self.meters.is_animating();
        let modal_animating = self.modal.as_ref().is_some_and(Modal::is_animating);
        let monitor_waiting = self.playback.is_waiting_for_video();
        let monitor = &self.monitor;
        let playback = &self.playback;
        let widgets = &mut self.widgets;
        let meters = &self.meters;
        let messages = &self.messages;
        let modal = &mut self.modal;
        let command_registry = &self.command_registry;
        let inspector = &mut self.inspector;
        let project_options = &mut self.project_options;
        let pipeline_graph = &mut self.pipeline_graph;
        let render_panel = &mut self.render_panel;
        self.focus_levels
            .retain(|id, _| snapshot.stack(*id).is_some());
        let show_focus = maximized_stack.is_none() && snapshot.stacks.len() > 1;
        let focus_step = 1.0 - (-FOCUS_FADE_SPEED * dt).exp();
        let mut focus_animating = false;
        for stack in &snapshot.stacks {
            let target = if show_focus && focused_stack == Some(stack.stack.id) {
                1.0
            } else {
                0.0
            };
            let level = self.focus_levels.entry(stack.stack.id).or_insert(target);
            *level += (target - *level) * focus_step;
            if (*level - target).abs() < 0.002 {
                *level = target;
            } else {
                focus_animating = true;
            }
        }
        let dragged_tab = match drag.as_ref() {
            Some(DragState::Dock {
                item: DockDrag::Tab { tab, .. },
                motion,
            }) if motion.dragging && motion.detached => Some(*tab),
            _ => None,
        };
        let preview_target = match drag.as_ref() {
            Some(DragState::Dock { item, motion })
                if motion.dragging
                    && (!matches!(item, DockDrag::Tab { .. }) || motion.detached) =>
            {
                edge_drop(workspace, motion.current)
                    .map(|(_, preview)| preview)
                    .or_else(|| {
                        snapshot.stack_at(motion.current).and_then(|target| {
                            (!matches!(item, DockDrag::Tab { .. })
                                || !target.tab_bar.contains(motion.current))
                            .then(|| {
                                drop_preview(target.rect, drop_zone(target.rect, motion.current))
                            })
                        })
                    })
            }
            _ => None,
        };
        let animated_drop_preview = if let Some(target) = preview_target {
            let current = self.animated_drop_preview.get_or_insert(target);
            current.x += (target.x - current.x) * 0.20;
            current.y += (target.y - current.y) * 0.20;
            current.width += (target.width - current.width) * 0.20;
            current.height += (target.height - current.height) * 0.20;
            Some(*current)
        } else {
            self.animated_drop_preview = None;
            None
        };

        let media_drag_overlay = match drag.as_ref() {
            Some(DragState::Media { items, motion, .. }) if motion.dragging => items
                .first()
                .and_then(|item| match item {
                    MediaDragItem::Media { media, stream } => project.media(*media).map(|asset| {
                        let title = match stream {
                            MediaStream::All => asset.name.clone(),
                            MediaStream::Video(index) => {
                                format!("{} - Video track {}", asset.name, index + 1)
                            }
                            MediaStream::Audio(index) => {
                                format!("{} - Audio track {}", asset.name, index + 1)
                            }
                        };
                        (title, asset.duration)
                    }),
                    MediaDragItem::Composition {
                        composition,
                        stream,
                    } => project.composition(*composition).map(|source| {
                        let title = match stream {
                            MediaStream::All => source.name.clone(),
                            MediaStream::Video(_) => format!("{} - Video", source.name),
                            MediaStream::Audio(_) => format!("{} - Audio", source.name),
                        };
                        (
                            title,
                            project.composition_duration(source.id).map(f64::from),
                        )
                    }),
                })
                .map(|(first_title, _duration)| {
                    let composition_cycle = items.iter().any(|item| match item {
                        MediaDragItem::Composition { composition, .. } => !project
                            .can_reference_composition(project.active_composition, *composition),
                        MediaDragItem::Media { .. } => false,
                    });
                    let all_media = items
                        .iter()
                        .filter_map(|item| match item {
                            MediaDragItem::Media {
                                media,
                                stream: MediaStream::All,
                            } => Some(*media),
                            _ => None,
                        })
                        .collect::<HashSet<_>>();
                    let specs = items
                        .iter()
                        .filter_map(|item| match item {
                            MediaDragItem::Media { media, stream }
                                if *stream != MediaStream::All && all_media.contains(media) =>
                            {
                                None
                            }
                            MediaDragItem::Media { media, stream } => {
                                project.media(*media).map(|asset| {
                                    let duration = asset
                                        .duration
                                        .unwrap_or(5.0)
                                        .clamp(0.1, 24.0 * 60.0 * 60.0)
                                        as f32;
                                    match stream {
                                        MediaStream::Audio(_) => MediaDropPreviewSpec {
                                            video_tracks: 0,
                                            audio_tracks: 1,
                                            duration,
                                        },
                                        MediaStream::Video(_) => MediaDropPreviewSpec {
                                            video_tracks: 1,
                                            audio_tracks: 0,
                                            duration,
                                        },
                                        MediaStream::All => {
                                            let video_tracks = if asset.tracks.is_empty() {
                                                usize::from(!matches!(asset.kind, MediaKind::Audio))
                                            } else {
                                                asset
                                                    .tracks
                                                    .iter()
                                                    .filter(|track| {
                                                        track.kind
                                                            == crate::project::MediaTrackKind::Video
                                                    })
                                                    .count()
                                            };
                                            let audio_tracks = if asset.tracks.is_empty() {
                                                usize::from(
                                                    matches!(asset.kind, MediaKind::Audio)
                                                        || matches!(asset.kind, MediaKind::Video)
                                                            && asset.has_audio,
                                                )
                                            } else {
                                                asset
                                                    .tracks
                                                    .iter()
                                                    .filter(|track| {
                                                        track.kind
                                                            == crate::project::MediaTrackKind::Audio
                                                    })
                                                    .count()
                                            };
                                            MediaDropPreviewSpec {
                                                video_tracks,
                                                audio_tracks,
                                                duration,
                                            }
                                        }
                                    }
                                })
                            }
                            MediaDragItem::Composition {
                                composition,
                                stream,
                            } => project.composition(*composition).map(|_| {
                                let duration =
                                    project.composition_duration(*composition).unwrap_or(5.0);
                                let has_audio = project.composition_has_audio(*composition);
                                match stream {
                                    MediaStream::Audio(_) => MediaDropPreviewSpec {
                                        video_tracks: 0,
                                        audio_tracks: 1,
                                        duration,
                                    },
                                    MediaStream::Video(_) => MediaDropPreviewSpec {
                                        video_tracks: 1,
                                        audio_tracks: 0,
                                        duration,
                                    },
                                    MediaStream::All => MediaDropPreviewSpec {
                                        video_tracks: 1,
                                        audio_tracks: usize::from(has_audio),
                                        duration,
                                    },
                                }
                            }),
                        })
                        .collect::<Vec<_>>();
                    let preview = (!composition_cycle)
                        .then(|| timeline.media_drop_previews(snapshot, motion.current, &specs))
                        .flatten();
                    let title = if composition_cycle {
                        format!("{first_title} - cannot nest recursively")
                    } else if items.len() == 1 {
                        first_title
                    } else {
                        format!("{} items", items.len())
                    };
                    (title, preview)
                }),
            _ => None,
        }
        .or_else(|| {
            if external_drag_items.is_empty() {
                return None;
            }
            let specs = external_drag_items
                .iter()
                .filter_map(|item| item.preview)
                .collect::<Vec<_>>();
            let preview = (!specs.is_empty())
                .then(|| timeline.media_drop_previews(snapshot, cursor, &specs))
                .flatten();
            let title = if external_drag_items.len() == 1 {
                external_drag_items[0]
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("Media")
                    .to_string()
            } else {
                format!("{} files", external_drag_items.len())
            };
            Some((title, preview))
        });

        let focus_levels = &self.focus_levels;
        let mut ctx = self.gui.begin(&mut self.renderer, self.input);
        ui::ui!(ctx, {
            Block {
                id: "rad-shell";
                fill: theme::bg();
                width: Size::Fill;
                height: Size::Fill;

                @for stack in &snapshot.stacks {
                    @rust {
                        build_stack(
                            ctx,
                            stack,
                            StackBuildContext {
                                snapshot,
                                cursor,
                                dragged_tab,
                                maximized: maximized_stack == Some(stack.stack.id),
                                focused: focused_stack == Some(stack.stack.id),
                                focus: focus_levels
                                    .get(&stack.stack.id)
                                    .copied()
                                    .unwrap_or(0.0),
                                icons,
                            },
                            StackBuildState {
                                project,
                                plugins: &self.plugins,
                                timeline,
                                media,
                                playback,
                                monitor,
                                history,
                                history_panel,
                                inspector,
                                project_options,
                                pipeline_graph,
                                render_panel,
                                widgets,
                                meters,
                                messages,
                                waveform_textures: &self.waveform_textures,
                            },
                        );
                    }
                }

                @for splitter in &snapshot.splitters {
                    @let active = matches!(
                        drag.as_ref(),
                        Some(DragState::Splitter { id, .. }) if *id == splitter.split_id
                    );

                    Block {
                        id: @format("splitter {}", splitter.split_id.0);
                        bounds: (
                            splitter.rect.x,
                            splitter.rect.y,
                            splitter.rect.width,
                            splitter.rect.height,
                        );
                        fill: if active { theme::accent() } else { theme::line_soft() };
                        cursor: match splitter.axis {
                            Axis::Horizontal => CursorShape::EwResize,
                            Axis::Vertical => CursorShape::NsResize,
                        };
                        interactive;
                    }
                }
            }
        });

        #[cfg(not(target_os = "macos"))]
        build_app_menu(
            &mut ctx,
            width,
            self.app_menu,
            self.app_menu_keyboard,
            cursor,
            &self.command_registry,
            icons,
        );

        let drag_overlay = match drag.as_ref() {
            Some(DragState::Dock { item, motion }) if motion.dragging => match item {
                DockDrag::Tab { tab, .. } if motion.detached => {
                    let title = snapshot
                        .stacks
                        .iter()
                        .flat_map(|stack| stack.stack.tabs.iter())
                        .find(|candidate| candidate.id == *tab)
                        .map_or("Tab".to_string(), |candidate| candidate.title.clone());
                    Some((title, "dragged-tab-ghost"))
                }
                DockDrag::Tab { .. } => None,
                DockDrag::Panel { source_stack } => {
                    let count = snapshot
                        .stack(*source_stack)
                        .map_or(0, |stack| stack.stack.tabs.len());
                    Some((
                        format!("{count} tab{}", if count == 1 { "" } else { "s" }),
                        "dragged-panel-ghost",
                    ))
                }
            },
            _ => None,
        };
        if let Some((title, ghost_id)) = drag_overlay {
            if let Some(preview) = animated_drop_preview {
                ui::ui!(ctx, {
                    Block {
                        id: "dock-drop-preview";
                        overlay;
                        bounds: (
                            preview.x,
                            preview.y,
                            preview.width,
                            preview.height,
                        );
                        fill: DROP_BG;
                        border: 2;
                        border_color: DROP_BORDER;
                        border_radius: RADIUS_MD;
                        animate_entry: true;
                    }
                });
            }

            ui::ui!(ctx, {
                Block {
                    id: ghost_id;
                    overlay;
                    bounds: (
                        cursor[0] + 10.0,
                        cursor[1] + 12.0,
                        (title.chars().count() as f32 * 7.0 + 28.0).clamp(96.0, 210.0),
                        25.0,
                    );
                    fill: theme::floating_bg();
                    border: 1;
                    border_color: theme::accent();
                    border_radius: RADIUS_MD;
                    padding: 5.0;
                    font_size: 11.0;
                    text_color: theme::popup_text();
                    text: title;
                    animate_entry: true;
                }
            });
        }

        if let Some((title, preview)) = media_drag_overlay {
            if let Some(previews) = preview {
                for (index, preview) in previews.into_iter().enumerate() {
                    ui::ui!(ctx, {
                        Block {
                            id: @format("media-drop-preview-{}", index);
                            overlay;
                            bounds: (preview.x, preview.y, preview.width, preview.height);
                            fill: Color::rgba(theme::accent().r, theme::accent().g, theme::accent().b, 0x22 as f32 / 255.0);
                            border: 2;
                            border_color: theme::accent();
                            border_radius: RADIUS_MD;
                        }
                    });
                }
            }
            ui::ui!(ctx, {
                Block {
                    id: "media-drag-ghost";
                    overlay;
                    bounds: (cursor[0] + 12.0, cursor[1] + 12.0, (title.chars().count() as f32 * 7.0 + 30.0).clamp(120.0, 260.0), 27.0);
                    fill: theme::floating_bg();
                    border: 1;
                    border_color: theme::accent();
                    border_radius: RADIUS_MD;
                    padding: 6.0;
                    font_size: 10.5;
                    text_color: theme::popup_text();
                    text: title;
                }
            });
        }

        if let Some(entries) = palette_entries.as_deref() {
            build_palette(&mut ctx, width, height, &mut self.palette, entries, icons);
        }
        if let Some(modal) = modal.as_mut() {
            build_active_modal(
                &mut ctx,
                width,
                height,
                modal,
                command_registry,
                render_panel.phase(),
                about_logos,
                icons,
            );
        }

        ctx.finish()?;
        self.sync_cursor();
        self.palette.advance_after_frame();
        if self.gui.has_active_animations()
            || self.palette.is_animating()
            || self.editor.timeline.is_playing()
            || self.editor.timeline.is_animating()
            || widgets_animating
            || media_animating
            || inspector_animating
            || project_options_animating
            || graph_animating
            || monitor_animating
            || render_animating
            || modal_animating
            || monitor_waiting
            || meters_animating
            || self.dock.is_animating()
            || self.waveform_textures.is_pending()
            || focus_animating
            || theme_animating
        {
            self.window.request_redraw();
        }
        self.input.mouse_pressed = false;
        self.input.mouse_released = false;
        Ok(())
    }

    fn panel_for_stack(&self, stack: StackId) -> Option<PanelKind> {
        self.snapshot
            .stack(stack)?
            .stack
            .active_tab()
            .and_then(|tab| PanelKind::from_title(&tab.title))
    }

    fn focused_panel(&self) -> Option<(StackId, PanelKind)> {
        let stack = self.dock.focused?;
        Some((stack, self.panel_for_stack(stack)?))
    }

    fn focused_kind(&self) -> Option<PanelKind> {
        self.focused_panel().map(|(_, panel)| panel)
    }

    fn panel_content(&self, stack: StackId) -> Option<Rect> {
        self.snapshot.stack(stack).map(|layout| layout.content)
    }

    fn focused_content(&self, panel: PanelKind) -> Option<Rect> {
        let (stack, kind) = self.focused_panel()?;
        (kind == panel).then(|| self.panel_content(stack)).flatten()
    }

    fn set_panel_focus(&mut self, stack: StackId, panel: PanelKind) {
        self.dock.focus_stack(stack);
        self.sync_panel_focus(Some((stack, panel)));
    }

    fn sync_panel_focus(&mut self, focused: Option<(StackId, PanelKind)>) {
        self.editor.timeline.set_focus(
            focused
                .filter(|(_, panel)| *panel == PanelKind::Timeline)
                .map(|(stack, _)| stack),
        );
        self.widgets.set_focused(
            focused
                .filter(|(_, panel)| *panel == PanelKind::Widgets)
                .map(|(stack, _)| stack),
        );
        let panel = focused.map(|(_, panel)| panel);
        self.inspector
            .set_focused(panel == Some(PanelKind::Inspector));
        self.project_options.set_focused(
            panel == Some(PanelKind::ProjectOptions)
                || (panel == Some(PanelKind::Inspector)
                    && self.media.selected_composition().is_some()),
        );
        self.pipeline_graph
            .set_focused(panel == Some(PanelKind::Pipeline));
    }

    fn focus_stack(&mut self, stack: StackId) -> Option<PanelKind> {
        let panel = self.panel_for_stack(stack)?;
        self.set_panel_focus(stack, panel);
        Some(panel)
    }

    fn focus_panel_at_cursor(&mut self) -> Option<(StackId, PanelKind)> {
        let Some(stack) = self
            .snapshot
            .content_at(self.cursor)
            .map(|layout| layout.stack.id)
        else {
            self.sync_panel_focus(None);
            return None;
        };
        self.focus_stack(stack).map(|panel| (stack, panel))
    }

    fn history_label(panel: Option<PanelKind>) -> &'static str {
        match panel {
            Some(PanelKind::Media) => "Edit media",
            Some(PanelKind::Monitor) => "Transform in preview",
            Some(PanelKind::Inspector) => "Edit properties",
            Some(PanelKind::ProjectOptions) => "Change composition settings",
            Some(PanelKind::Pipeline) => "Edit effect graph",
            Some(PanelKind::Render) => "Change render settings",
            Some(PanelKind::Timeline) => "Edit timeline",
            Some(PanelKind::Messages) => "View messages",
            Some(PanelKind::History) => "Navigate history",
            Some(PanelKind::Widgets | PanelKind::Meters) | None => "Edit",
        }
    }

    fn begin_history_gesture(&mut self) {
        let panel = self
            .snapshot
            .stack_at(self.cursor)
            .and_then(|stack| stack.stack.active_tab())
            .and_then(|tab| PanelKind::from_title(&tab.title));
        self.editor.begin_gesture(Self::history_label(panel));
    }

    fn set_history_gesture_label(&mut self, label: impl Into<String>) {
        self.editor.set_gesture_label(label);
    }

    fn finish_history_gesture(&mut self) {
        self.editor.finish_gesture();
    }

    fn restore_history_snapshot(&mut self, snapshot: HistorySnapshot) {
        self.editor.restore_snapshot(snapshot, &self.plugins);
        self.audio.clear();
        self.media.clear_selection();
        self.waveform_textures.clear();
        self.waveform_textures.queue_missing(&self.editor.project);
        self.effects.rebuild(&self.editor.project.pipelines);
        self.playback.clear_caches();
        self.monitor.clear_captured_frame();
        self.playback
            .sync_compiled_effects(&self.renderer, &self.effects, &self.plugins);
        self.playback.invalidate();
        self.sync_inactive_windows_after_project_reset();
        self.request_redraw_all();
        self.update_window_title();
    }

    fn key_history_label(&self) -> &'static str {
        if self.focused_kind() == Some(PanelKind::Timeline) {
            return self
                .editor
                .timeline
                .keyboard_history_label()
                .unwrap_or("Timeline edit");
        }
        Self::history_label(self.focused_kind())
    }

    fn record_key_history(&mut self, before: HistorySnapshot, label: &'static str, coalesce: bool) {
        self.editor.record_after(label, before, coalesce);
    }

    fn handle_file_command(&mut self, command: FileCommand) -> bool {
        let command = match command {
            FileCommand::NewProject => self.command_registry.command("project.new"),
            FileCommand::Save => self.command_registry.command("project.save"),
            FileCommand::SaveAs => self.command_registry.command("project.save-as"),
            FileCommand::Load => self.command_registry.command("project.open"),
            FileCommand::LoadRecent(path) => Some(EditorCommand::Action(
                PaletteAction::OpenRecentProject(path),
            )),
            FileCommand::ImportMedia => self.command_registry.command("media.import"),
            FileCommand::Exit => self.command_registry.command("application.exit"),
        };
        if let Some(command) = command {
            self.command_queue.push(command);
        }
        false
    }

    fn dismiss_transient_ui(&mut self) {
        self.palette.close();
        #[cfg(not(target_os = "macos"))]
        {
            self.app_menu = AppMenuState::Closed;
            self.app_menu_keyboard.active = false;
        }
        self.media.close_context_menu();
        self.editor.timeline.close_popups();
        self.sync_panel_focus(None);
        self.monitor.close_popups();
        self.render_panel.close_popups();
    }

    fn scroll_focused_popup(&mut self, delta: [f32; 2]) -> bool {
        let Some((stack, panel)) = self.focused_panel() else {
            return false;
        };
        let Some(content) = self.panel_content(stack) else {
            return false;
        };
        let point = self.cursor;
        match panel {
            PanelKind::Widgets => {
                self.widgets.popup_contains(content, point)
                    && self.widgets.scroll(content, point, delta)
            }
            PanelKind::Monitor => {
                self.monitor.popup_contains(content, point)
                    && self.monitor.scroll_popup(content, point, delta)
            }
            PanelKind::Render => {
                self.render_panel.popup_contains(content, point)
                    && self.render_panel.scroll(content, point, delta)
            }
            PanelKind::Inspector if self.media.selected_composition().is_some() => {
                self.project_options.popup_contains(content, point)
                    && self.project_options.scroll_popup(content, point, delta)
            }
            PanelKind::Inspector => {
                self.inspector.popup_contains(content, point)
                    && self.inspector.scroll_popup(content, point, delta)
            }
            PanelKind::ProjectOptions => {
                self.project_options.popup_contains(content, point)
                    && self.project_options.scroll_popup(content, point, delta)
            }
            PanelKind::Pipeline => {
                self.pipeline_graph.popup_contains(content, point)
                    && self.pipeline_graph.scroll_popup(content, point, delta)
            }
            _ => false,
        }
    }

    fn press_focused_popup(&mut self) -> bool {
        let Some((stack, panel)) = self.focused_panel() else {
            return false;
        };
        let Some(content) = self.panel_content(stack) else {
            return false;
        };
        let point = self.cursor;
        let modifiers = self.modifiers;
        match panel {
            PanelKind::Widgets if self.widgets.popup_contains(content, point) => {
                self.widgets
                    .pointer_pressed(stack, content, point, modifiers);
            }
            PanelKind::Monitor if self.monitor.popup_contains(content, point) => {
                let graph_selection = self.pipeline_graph.monitor_selection(&self.editor.timeline);
                if self.monitor.pointer_pressed(
                    content,
                    point,
                    MonitorPointerContext {
                        modifiers,
                        project: &mut self.editor.project,
                        plugins: &self.plugins,
                        graph_selection,
                        timeline: &mut self.editor.timeline,
                        source_geometry: self.playback.preview_output().source_geometry,
                    },
                ) {
                    self.playback.invalidate();
                }
            }
            PanelKind::Render if self.render_panel.popup_contains(content, point) => {
                self.render_panel.pointer_pressed(
                    content,
                    point,
                    modifiers,
                    &self.editor.project,
                    &self.editor.timeline,
                );
            }
            PanelKind::Pipeline if self.pipeline_graph.popup_contains(content, point) => {
                let action = self.pipeline_graph.pointer_pressed(
                    content,
                    point,
                    modifiers,
                    &self.editor.project,
                    &self.editor.timeline,
                    &self.plugins,
                );
                if !matches!(action, PipelineGraphAction::None) {
                    self.handle_pipeline_graph_action(action);
                }
            }
            PanelKind::Inspector
                if if self.media.selected_composition().is_some() {
                    self.project_options.popup_contains(content, point)
                } else {
                    self.inspector.popup_contains(content, point)
                } =>
            {
                self.press_inspector(content);
            }
            PanelKind::ProjectOptions if self.project_options.popup_contains(content, point) => {
                self.press_project_options(content);
            }
            _ => return false,
        }
        true
    }

    fn open_modal(&mut self, modal: Modal) {
        if self.palette.is_command_dialog() {
            self.palette.close_immediately();
        }
        self.dismiss_transient_ui();
        if self.modal.is_some() {
            self.modal_queue.push_back(modal);
        } else {
            self.modal = Some(modal);
        }
    }

    fn advance_modal_queue(&mut self) {
        if self.modal.is_some() {
            return;
        }
        if let Some(mut modal) = self.modal_queue.pop_front() {
            modal.restart_entry_animation();
            self.modal = Some(modal);
        }
    }

    fn restore_handled_modal(&mut self, modal: Modal, keep: bool) {
        let opened_while_handling = self.modal.take();
        if keep {
            self.modal = Some(modal);
        }
        if let Some(next) = opened_while_handling {
            self.modal_queue.push_back(next);
        }
        if !keep {
            self.advance_modal_queue();
        }
    }

    fn open_layout_save_dialog(&mut self) {
        self.open_modal(Modal::LayoutSave(LayoutSaveDialog::new()));
    }

    fn refresh_layout_menu(&mut self) {
        #[cfg(target_os = "macos")]
        self.native_menu.refresh_layouts();
    }

    fn save_layout_named(&mut self, name: &str) {
        let name = sanitize_file_name(name.trim().trim_end_matches(".kama-layout"));
        if name.is_empty() {
            return;
        }
        let path = layout_data_dir().join(format!("{name}.kama-layout"));
        if atomic_write_json(&path, &self.dock.layout_spec()).is_err() {
            return;
        }
        self.refresh_layout_menu();
    }

    fn load_layout(&mut self, path: &Path) {
        if let Ok(layout) = std::fs::read(path)
            .map_err(anyhow::Error::from)
            .and_then(|data| serde_json::from_slice::<DockLayoutSpec>(&data).map_err(Into::into))
        {
            self.drag = None;
            self.dock = DockState::from_spec(layout);
        }
    }

    fn delete_layout(&mut self, path: &Path) {
        if std::fs::remove_file(path).is_err() {
            return;
        }
        self.refresh_layout_menu();
    }

    fn handle_layout_command(&mut self, command: LayoutCommand) {
        self.command_queue.push(EditorCommand::Layout(command));
    }

    fn execute_layout_command(&mut self, command: LayoutCommand) {
        match command {
            LayoutCommand::Save => self.open_layout_save_dialog(),
            LayoutCommand::SaveNamed(name) => self.save_layout_named(&name),
            LayoutCommand::Load(path) => self.load_layout(&path),
            LayoutCommand::Delete(path) => self.delete_layout(&path),
            LayoutCommand::RestoreDefault => {
                self.drag = None;
                self.dock = default_dock();
            }
        }
    }

    fn handle_modal_pointer(&mut self) -> bool {
        let Some(mut modal) = self.modal.take() else {
            return false;
        };
        let width = self.renderer.logical_width();
        let height = self.renderer.logical_height();
        let point = self.cursor;
        let mut keep = true;

        match &mut modal {
            Modal::About(dialog) => {
                if !dialog.is_closing() {
                    let rect = about_dialog_rect(width, height);
                    if !rect.contains(point) || about_dialog_button_rect(rect).contains(point) {
                        dialog.close();
                    }
                }
            }
            Modal::LayoutSave(dialog) => {
                if !dialog.is_closing() {
                    let rect = LAYOUT_SAVE_MODAL.rect(width, height);
                    if !rect.contains(point)
                        || LAYOUT_SAVE_MODAL.button(rect, false).contains(point)
                    {
                        dialog.close();
                    } else if LAYOUT_SAVE_MODAL.button(rect, true).contains(point) {
                        let name = dialog.editor.text().to_string();
                        dialog.close();
                        self.handle_layout_command(LayoutCommand::SaveNamed(name));
                    } else {
                        dialog.editor.pointer_pressed(
                            LAYOUT_SAVE_MODAL.input(rect),
                            point,
                            self.modifiers,
                        );
                    }
                }
            }
            Modal::Discard(dialog) => {
                if !dialog.is_closing() {
                    let rect = discard_dialog_rect(width, height);
                    let discard = discard_button_rect(rect, true).contains(point);
                    if discard
                        || discard_button_rect(rect, false).contains(point)
                        || !rect.contains(point)
                    {
                        let action = discard.then(|| dialog.action.clone());
                        dialog.close();
                        if let Some(action) = action {
                            self.perform_discard_action(action);
                        }
                    }
                }
            }
            Modal::Busy(dialog) => {
                if !dialog.is_closing() {
                    let rect = busy_project_dialog_rect(width, height);
                    if busy_project_button_rect(rect, true).contains(point) {
                        let action = dialog.action.clone();
                        self.render_panel.cancel_active();
                        self.request_discard_action_after_render(action);
                        keep = false;
                    } else if busy_project_button_rect(rect, false).contains(point)
                        || !rect.contains(point)
                    {
                        dialog.close();
                    }
                }
            }
            Modal::MissingMedia(dialog) => {
                if !dialog.is_closing() {
                    let rect = missing_media_dialog_rect(width, height, dialog.missing().len());
                    if missing_media_button_rect(rect, false).contains(point)
                        || !rect.contains(point)
                    {
                        dialog.close();
                    } else if missing_media_button_rect(rect, true).contains(point) {
                        self.confirm_missing_media_load(dialog);
                        dialog.close();
                    }
                }
            }
            Modal::Settings(dialog) => {
                dialog.pointer_pressed(width, height, point, self.modifiers);
                self.plugin_paths = dialog.plugin_paths().to_owned();
                preferences::save(&self.command_registry, &self.plugin_paths);
                keep = !dialog.is_closed();
            }
            Modal::Keybinds(dialog) => {
                dialog.pointer_pressed(width, height, point, &mut self.command_registry);
                preferences::save(&self.command_registry, &self.plugin_paths);
                keep = !dialog.is_closed();
            }
            Modal::Composition(dialog) => {
                if !dialog.is_closing() {
                    let rect = NEW_COMPOSITION_MODAL.rect(width, height);
                    if !rect.contains(point)
                        || NEW_COMPOSITION_MODAL.button(rect, false).contains(point)
                    {
                        dialog.close();
                    } else if NEW_COMPOSITION_MODAL.button(rect, true).contains(point) {
                        self.confirm_new_composition_dialog(dialog);
                        dialog.close();
                    } else {
                        dialog.editor.pointer_pressed(
                            NEW_COMPOSITION_MODAL.input(rect),
                            point,
                            self.modifiers,
                        );
                    }
                }
            }
            Modal::SpeedDuration(dialog) => {
                if !dialog.is_closing() {
                    let rect = SPEED_DURATION_MODAL.rect(width, height);
                    if !rect.contains(point)
                        || SPEED_DURATION_MODAL.button(rect, false).contains(point)
                    {
                        dialog.close();
                    } else if SPEED_DURATION_MODAL.button(rect, true).contains(point) {
                        self.confirm_speed_duration_dialog(dialog);
                        dialog.close();
                    } else if let Some(mode) = [
                        SpeedDurationMode::SpeedPercent,
                        SpeedDurationMode::PerClipDuration,
                        SpeedDurationMode::TotalDuration,
                    ]
                    .into_iter()
                    .enumerate()
                    .find_map(|(index, mode)| {
                        speed_duration_mode_rect(rect, index)
                            .contains(point)
                            .then_some(mode)
                    }) {
                        dialog.set_mode(mode, &self.editor.timeline, &self.editor.project);
                    } else {
                        dialog.editor.pointer_pressed(
                            SPEED_DURATION_MODAL.input(rect),
                            point,
                            self.modifiers,
                        );
                    }
                }
            }
        }

        self.restore_handled_modal(modal, keep);
        true
    }

    fn take_exit_request(&mut self) -> bool {
        std::mem::take(&mut self.exit_requested)
    }

    fn value_drag_active(&self) -> bool {
        self.inspector.is_cursor_lock_dragging()
            || self.project_options.is_cursor_lock_dragging()
            || self.pipeline_graph.is_cursor_lock_dragging()
            || self.render_panel.is_value_dragging()
            || self.widgets.is_cursor_lock_dragging()
            || self.editor.timeline.is_value_dragging()
    }

    fn sync_value_drag_cursor(&mut self, event_loop: &ActiveEventLoop) {
        if self.value_drag_active() {
            if self.value_drag_cursor_locked {
                return;
            }
            let anchor = self.cursor_physical;

            if self
                .window
                .set_cursor_position(PhysicalPosition::new(anchor[0], anchor[1]))
                .is_ok()
            {
                self.value_drag_cursor_anchor = Some(anchor);
                self.value_drag_cursor_locked = true;
                self.window.set_cursor_visible(false);
            }
            return;
        }
        self.release_value_drag_cursor(event_loop);
    }

    fn release_value_drag_cursor(&mut self, event_loop: &ActiveEventLoop) {
        if !self.value_drag_cursor_locked {
            return;
        }
        if let Some(anchor) = self.value_drag_cursor_anchor.take() {
            let _ = self
                .window
                .set_cursor_position(PhysicalPosition::new(anchor[0], anchor[1]));
            self.cursor_physical = anchor;
            let scale = self.renderer.scale_factor() as f64;
            self.cursor = [(anchor[0] / scale) as f32, (anchor[1] / scale) as f32];
            self.input.cursor = self.cursor;
        }
        let _ = self.window.set_cursor_grab(CursorGrabMode::None);
        self.window.set_cursor_visible(true);
        self.value_drag_cursor_locked = false;
        if self.external_drag_items.is_empty() {
            event_loop.listen_device_events(DeviceEvents::WhenFocused);
        }
    }

    fn move_value_drag_cursor_warped(&mut self, position: PhysicalPosition<f64>) {
        if !self.value_drag_cursor_locked {
            return;
        }
        let Some(anchor) = self.value_drag_cursor_anchor else {
            return;
        };
        let dx = position.x - anchor[0];
        let dy = position.y - anchor[1];
        if dx.abs() < 0.5 && dy.abs() < 0.5 {
            return;
        }
        let scale = self.renderer.scale_factor() as f64;
        self.cursor[0] += (dx / scale) as f32;
        self.cursor[1] += (dy / scale) as f32;
        self.input.cursor = self.cursor;
        self.pointer_moved();
        let _ = self
            .window
            .set_cursor_position(PhysicalPosition::new(anchor[0], anchor[1]));
    }

    fn pointer_pressed(&mut self) {
        if self.gui.consume_popup_press(self.cursor) {
            self.input.mouse_pressed = false;
            return;
        }
        if self.handle_modal_pointer() {
            return;
        }
        if self.drag.take().is_some() {
            self.finish_history_gesture();
        }
        self.begin_history_gesture();
        self.pointer_pressed_inner();
        if self.focused_kind() == Some(PanelKind::Timeline) {
            if let Some(label) = self.editor.timeline.history_gesture_label() {
                self.set_history_gesture_label(label);
            }
        }
    }

    fn pointer_pressed_inner(&mut self) {
        #[cfg(not(target_os = "macos"))]
        if self.handle_app_menu_pointer() {
            return;
        }

        if self.palette.kind.is_some() {
            let entries = palette_entries(
                &self.palette,
                &self.editor.project,
                &self.plugins,
                &self.command_registry,
            );
            let visible_rows =
                palette_visible_rows(&self.palette, entries.len(), self.renderer.logical_height());
            let popup_rect = palette_rect(
                self.renderer.logical_width(),
                self.renderer.logical_height(),
                &self.palette,
                visible_rows,
            );
            if !popup_rect.contains(self.cursor) {
                self.palette.close();
                self.input.mouse_pressed = false;
                return;
            }
            if palette_header_close_rect(popup_rect, &self.palette)
                .is_some_and(|close| close.contains(self.cursor))
            {
                self.palette.close();
                return;
            }
            if self.palette.query.pointer_pressed(
                palette_input_rect(popup_rect, &self.palette),
                self.cursor,
                self.modifiers,
            ) {
                return;
            }
            if palette_back_rect(popup_rect, &self.palette)
                .is_some_and(|rect| rect.contains(self.cursor))
            {
                self.palette_back();
                return;
            }
            if let Some(row) = palette_row_at(
                popup_rect,
                &self.palette,
                self.cursor,
                entries.len(),
                visible_rows,
            ) {
                self.palette.selected = row;
                self.accept_palette();
                return;
            }
            if palette_footer_close_rect(popup_rect, &self.palette, visible_rows)
                .is_some_and(|footer| footer.contains(self.cursor))
            {
                self.palette.close();
            }
            return;
        }

        if self.press_focused_popup() {
            return;
        }

        let hovered_panel = self.focus_panel_at_cursor();
        if !matches!(hovered_panel, Some((_, PanelKind::Media))) {
            self.media.close_context_menu();
        }
        match hovered_panel {
            Some((_, PanelKind::Timeline)) => {
                if self.editor.timeline.pointer_pressed(
                    &self.snapshot,
                    self.cursor,
                    MouseButton::Left,
                    self.modifiers,
                ) {
                    self.media.clear_selection();
                    if let Some(action) = self.editor.timeline.take_action() {
                        self.handle_timeline_action(action);
                    }
                    return;
                }
            }
            Some((stack, PanelKind::Media)) => {
                if let Some(content) = self.panel_content(stack) {
                    let action = self.media.pointer_pressed(
                        content,
                        self.cursor,
                        self.modifiers,
                        &self.editor.project,
                    );
                    if let MediaAction::BeginDrag {
                        items,
                        open_on_click,
                    } = &action
                    {
                        self.editor.timeline.clear_selection();
                        self.drag = Some(DragState::Media {
                            items: items.clone(),
                            motion: DragMotion::new(self.cursor),
                            open_on_click: *open_on_click,
                        });
                        return;
                    }
                    if !matches!(action, MediaAction::None) {
                        self.handle_media_action(action);
                        return;
                    }
                }
            }
            Some((stack, PanelKind::Inspector)) => {
                if let Some(content) = self.panel_content(stack) {
                    if self.press_inspector(content) {
                        return;
                    }
                }
            }
            Some((stack, PanelKind::ProjectOptions)) => {
                if let Some(content) = self.panel_content(stack) {
                    if self.press_project_options(content) {
                        return;
                    }
                }
            }
            Some((stack, PanelKind::Render)) => {
                if let Some(content) = self.panel_content(stack) {
                    if self.render_panel.pointer_pressed(
                        content,
                        self.cursor,
                        self.modifiers,
                        &self.editor.project,
                        &self.editor.timeline,
                    ) {
                        return;
                    }
                }
            }
            Some((stack, PanelKind::History)) => {
                if let Some(content) = self.panel_content(stack) {
                    if let Some(snapshot) = self.history_panel.pointer_pressed(
                        &mut self.editor.history,
                        content,
                        self.cursor,
                    ) {
                        self.command_queue
                            .push(EditorCommand::restore_history(snapshot));
                        return;
                    }
                }
            }
            Some((stack, PanelKind::Pipeline)) => {
                if let Some(content) = self.panel_content(stack) {
                    self.set_panel_focus(stack, PanelKind::Pipeline);
                    let action = self.pipeline_graph.pointer_pressed(
                        content,
                        self.cursor,
                        self.modifiers,
                        &self.editor.project,
                        &self.editor.timeline,
                        &self.plugins,
                    );
                    if !matches!(&action, PipelineGraphAction::None) {
                        self.handle_pipeline_graph_action(action);
                        return;
                    }
                }
            }
            Some((stack, PanelKind::Widgets)) => {
                if let Some(layout) = self.snapshot.stack(stack) {
                    if self.widgets.pointer_pressed(
                        stack,
                        layout.content,
                        self.cursor,
                        self.modifiers,
                    ) {
                        return;
                    }
                }
            }
            Some((stack, PanelKind::Monitor)) => {
                if let Some(content) = self.panel_content(stack) {
                    let graph_selection =
                        self.pipeline_graph.monitor_selection(&self.editor.timeline);
                    if self.monitor.pointer_pressed(
                        content,
                        self.cursor,
                        MonitorPointerContext {
                            modifiers: self.modifiers,
                            project: &mut self.editor.project,
                            plugins: &self.plugins,
                            graph_selection,
                            timeline: &mut self.editor.timeline,
                            source_geometry: self.playback.preview_output().source_geometry,
                        },
                    ) {
                        if let Some(action) = self.monitor.take_action() {
                            self.handle_monitor_action(action);
                        }
                        self.media.clear_selection();
                        self.playback.invalidate();
                        return;
                    }
                }
            }
            Some((_, PanelKind::Meters | PanelKind::Messages)) => return,
            None => {}
        }

        if let Some(stack) = self.snapshot.maximize_at(self.cursor) {
            self.command_queue
                .push(EditorCommand::Dock(DockCommand::ToggleMaximize(
                    stack.stack.id,
                )));
            return;
        }

        if let Some(stack) = self.snapshot.plus_at(self.cursor) {
            self.palette.pending_open =
                Some((PaletteKind::AddPanel(stack.stack.id), Some(stack.plus_rect)));
            return;
        }

        if let Some(splitter) = self.snapshot.splitter_at(self.cursor) {
            self.drag = Some(DragState::Splitter {
                id: splitter.split_id,
                parent_rect: splitter.parent_rect,
            });
            return;
        }

        if let Some(tab) = self.snapshot.tab_at(self.cursor) {
            if dock_tab_close_rect(tab.rect).contains(self.cursor) {
                self.command_queue
                    .push(EditorCommand::Dock(DockCommand::CloseTab {
                        stack: tab.stack_id,
                        tab: tab.tab_id,
                    }));
                return;
            }
            let panel = self
                .snapshot
                .stack(tab.stack_id)
                .and_then(|layout| layout.stack.tabs.iter().find(|item| item.id == tab.tab_id))
                .and_then(|item| PanelKind::from_title(&item.title));
            self.command_queue
                .push(EditorCommand::Dock(DockCommand::ActivateTab {
                    stack: tab.stack_id,
                    tab: tab.tab_id,
                }));
            if let Some(panel) = panel {
                self.set_panel_focus(tab.stack_id, panel);
            }
            self.drag = Some(DragState::Dock {
                item: DockDrag::Tab {
                    tab: tab.tab_id,
                    source_stack: tab.stack_id,
                    original_index: tab.index,
                },
                motion: DragMotion::new(self.cursor),
            });
            return;
        }

        if let Some(stack) = self
            .snapshot
            .stacks
            .iter()
            .rev()
            .find(|stack| stack.tab_bar.contains(self.cursor))
        {
            self.drag = Some(DragState::Dock {
                item: DockDrag::Panel {
                    source_stack: stack.stack.id,
                },
                motion: DragMotion::new(self.cursor),
            });
        }
    }

    fn pointer_middle_pressed(&mut self) {
        if self.palette.kind.is_some() {
            return;
        }
        match self.focus_panel_at_cursor() {
            Some((_, PanelKind::Timeline)) => {
                if self.editor.timeline.pointer_pressed(
                    &self.snapshot,
                    self.cursor,
                    MouseButton::Middle,
                    self.modifiers,
                ) {
                    return;
                }
            }
            Some((stack, PanelKind::Pipeline)) => {
                if let Some(content) = self.panel_content(stack) {
                    let action = self.pipeline_graph.pointer_middle_pressed(
                        content,
                        self.cursor,
                        &self.editor.project,
                        &self.editor.timeline,
                        &self.plugins,
                    );
                    if !matches!(action, PipelineGraphAction::None) {
                        self.begin_history_gesture();
                        self.set_history_gesture_label("Reset graph property");
                        self.handle_pipeline_graph_action(action);
                        self.finish_history_gesture();
                        return;
                    }
                    if self.pipeline_graph.is_dragging() {
                        return;
                    }
                }
            }
            Some((stack, PanelKind::Inspector)) if self.media.selected_composition().is_none() => {
                if let Some(content) = self.panel_content(stack) {
                    self.begin_history_gesture();
                    let handled = self.inspector.pointer_middle_pressed(
                        content,
                        self.cursor,
                        InspectorPointerContext {
                            modifiers: self.modifiers,
                            project: &mut self.editor.project,
                            timeline: &mut self.editor.timeline,
                            media_selection: self.media.selected_with_stream(),
                            plugins: &self.plugins,
                        },
                    );
                    if handled {
                        self.set_history_gesture_label("Reset property");
                        self.playback.invalidate();
                    }
                    self.finish_history_gesture();
                    if handled {
                        return;
                    }
                }
            }
            Some((stack, PanelKind::Monitor)) => {
                if let Some(content) = self.panel_content(stack) {
                    if self.monitor.pointer_middle_pressed(content, self.cursor) {
                        return;
                    }
                }
            }
            _ => {}
        }
        if let Some(tab) = self.snapshot.tab_at(self.cursor) {
            self.command_queue
                .push(EditorCommand::Dock(DockCommand::CloseTab {
                    stack: tab.stack_id,
                    tab: tab.tab_id,
                }));
            self.drag = None;
        }
    }

    fn pointer_moved(&mut self) {
        if let Some(modal) = &mut self.modal {
            match modal {
                Modal::Settings(dialog) => {
                    if dialog.pointer_moved(
                        self.renderer.logical_width(),
                        self.renderer.logical_height(),
                        self.cursor,
                    ) {
                        self.plugin_paths = dialog.plugin_paths().to_owned();
                        preferences::save(&self.command_registry, &self.plugin_paths);
                    }
                }
                Modal::Composition(dialog) if !dialog.is_closing() => {
                    dialog.editor.pointer_moved(self.cursor);
                }
                Modal::SpeedDuration(dialog) if !dialog.is_closing() => {
                    dialog.editor.pointer_moved(self.cursor);
                }
                Modal::LayoutSave(dialog) if !dialog.is_closing() => {
                    dialog.editor.pointer_moved(self.cursor);
                }
                Modal::About(_)
                | Modal::Composition(_)
                | Modal::SpeedDuration(_)
                | Modal::LayoutSave(_)
                | Modal::Keybinds(_)
                | Modal::Discard(_)
                | Modal::Busy(_)
                | Modal::MissingMedia(_) => {}
            }
            return;
        }
        #[cfg(not(target_os = "macos"))]
        if self.hover_app_menu() {
            return;
        }
        if self.palette.kind.is_some() {
            self.palette.query.pointer_moved(self.cursor);
            let entries = palette_entries(
                &self.palette,
                &self.editor.project,
                &self.plugins,
                &self.command_registry,
            );
            let visible =
                palette_visible_rows(&self.palette, entries.len(), self.renderer.logical_height());
            let popup = palette_rect(
                self.renderer.logical_width(),
                self.renderer.logical_height(),
                &self.palette,
                visible,
            );
            let hovered = palette_row_at(popup, &self.palette, self.cursor, entries.len(), visible);
            if hovered != self.palette.hovered {
                self.palette.hovered = hovered;
                if let Some(index) = hovered {
                    self.palette.selected = index;
                }
            }
            return;
        }
        if let Some(drag) = self.drag.as_mut() {
            match drag {
                DragState::Splitter { id, parent_rect } => {
                    self.dock.set_split_ratio(*id, self.cursor, *parent_rect)
                }
                DragState::Media { motion, .. } => {
                    motion.current = self.cursor;
                    let dx = self.cursor[0] - motion.start[0];
                    let dy = self.cursor[1] - motion.start[1];
                    motion.dragging |= dx * dx + dy * dy > 16.0;
                }
                DragState::Dock { item, motion } => {
                    motion.current = self.cursor;
                    let dx = self.cursor[0] - motion.start[0];
                    let dy = self.cursor[1] - motion.start[1];
                    motion.dragging |= dx * dx + dy * dy > 36.0;
                    if !motion.dragging {
                        return;
                    }
                    let DockDrag::Tab {
                        tab, source_stack, ..
                    } = item
                    else {
                        return;
                    };
                    if !motion.detached {
                        let Some(source) = self.snapshot.stack(*source_stack) else {
                            return;
                        };
                        let over_other_stack = self
                            .snapshot
                            .stack_at(self.cursor)
                            .is_some_and(|target| target.stack.id != *source_stack);
                        motion.detached = over_other_stack || !source.tab_bar.contains(self.cursor);
                        if !motion.detached {
                            let index = insertion_index(
                                &self.snapshot,
                                *source_stack,
                                *tab,
                                self.cursor[0],
                            );
                            self.dock.drop_tab(
                                *tab,
                                *source_stack,
                                *source_stack,
                                DropZone::Center,
                                Some(index),
                            );
                        }
                    }
                }
            }
            return;
        }
        if self.monitor.pointer_moved(
            self.cursor,
            self.modifiers,
            &mut self.editor.project,
            &self.plugins,
            &mut self.editor.timeline,
        ) {
            self.playback.invalidate();
            return;
        }
        let timeline_focused = self.focused_kind() == Some(PanelKind::Timeline);
        let media_focused = self.focused_kind() == Some(PanelKind::Media);
        let inspector_focused = self.focused_kind() == Some(PanelKind::Inspector);
        let project_options_focused = self.focused_kind() == Some(PanelKind::ProjectOptions);
        let graph_focused = self.focused_kind() == Some(PanelKind::Pipeline);
        let render_focused = self.focused_kind() == Some(PanelKind::Render);
        if self.widgets.pointer_moved(self.cursor) {
            return;
        }
        if media_focused {
            if let Some((stack, _)) = self.focused_panel() {
                if let Some(content) = self.panel_content(stack) {
                    if self.media.pointer_moved(content, self.cursor) {
                        return;
                    }
                }
            }
        }
        if inspector_focused {
            if let Some(composition) = self.media.selected_composition() {
                if self.project_options.pointer_moved(
                    self.cursor,
                    &mut self.editor.project,
                    composition,
                ) {
                    self.playback.invalidate();
                    return;
                }
            } else if self.inspector.pointer_moved(
                self.cursor,
                &mut self.editor.project,
                &mut self.editor.timeline,
            ) {
                if self.inspector.is_cursor_lock_dragging() {
                    self.playback.invalidate();
                }
                return;
            }
        }
        if project_options_focused {
            let composition = self.editor.project.active_composition;
            if self.project_options.pointer_moved(
                self.cursor,
                &mut self.editor.project,
                composition,
            ) {
                self.playback.invalidate();
                return;
            }
        }
        if timeline_focused
            && self.editor.timeline.pointer_moved(
                &self.snapshot,
                self.cursor,
                self.modifiers,
                &self.editor.project,
            )
        {
            if let Some(label) = self.editor.timeline.history_gesture_label() {
                self.set_history_gesture_label(label);
            }
            self.playback.invalidate();
            return;
        }
        if render_focused {
            if let Some((stack, _)) = self.focused_panel() {
                if let Some(content) = self.panel_content(stack) {
                    if self.render_panel.pointer_moved(content, self.cursor) {
                        return;
                    }
                }
            }
        }
        if graph_focused {
            if let Some((stack, _)) = self.focused_panel() {
                if let Some(content) = self.panel_content(stack) {
                    let action = self.pipeline_graph.pointer_moved(content, self.cursor);
                    if !matches!(&action, PipelineGraphAction::None) {
                        self.handle_pipeline_graph_action(action);
                    }
                }
            }
        }
    }

    fn release_dock_drag(&mut self) -> bool {
        let Some(drag) = self.drag.take() else {
            return false;
        };
        let (item, motion) = match drag {
            DragState::Dock { item, motion } => (item, motion),
            DragState::Splitter { .. } => return true,
            other => {
                self.drag = Some(other);
                return false;
            }
        };
        if !motion.dragging {
            return true;
        }
        let point = motion.current;
        if !motion.detached {
            if let DockDrag::Tab {
                tab, source_stack, ..
            } = &item
            {
                let index = insertion_index(&self.snapshot, *source_stack, *tab, point[0]);
                self.dock.drop_tab(
                    *tab,
                    *source_stack,
                    *source_stack,
                    DropZone::Center,
                    Some(index),
                );
                return true;
            }
        }
        if let Some((zone, _)) = edge_drop(self.workspace_rect(), point) {
            match item {
                DockDrag::Tab {
                    tab, source_stack, ..
                } => {
                    self.dock.dock_tab_to_edge(tab, source_stack, zone);
                }
                DockDrag::Panel { source_stack } => {
                    self.dock.dock_stack_to_edge(source_stack, zone);
                }
            }
            return true;
        }
        if let Some(target) = self.snapshot.stack_at(point) {
            match item {
                DockDrag::Tab {
                    tab, source_stack, ..
                } => {
                    let zone = if target.tab_bar.contains(point) {
                        DropZone::Center
                    } else {
                        drop_zone(target.rect, point)
                    };
                    let insert = (zone == DropZone::Center)
                        .then(|| insertion_index(&self.snapshot, target.stack.id, tab, point[0]));
                    self.dock
                        .drop_tab(tab, source_stack, target.stack.id, zone, insert);
                }
                DockDrag::Panel { source_stack } => {
                    self.dock.drop_stack(
                        source_stack,
                        target.stack.id,
                        drop_zone(target.rect, point),
                    );
                }
            }
        }
        true
    }

    fn pointer_released(&mut self, button: MouseButton) {
        self.pointer_released_inner(button);
        if button == MouseButton::Left {
            self.finish_history_gesture();
        }
    }

    fn pointer_released_inner(&mut self, button: MouseButton) {
        if let Some(modal) = &mut self.modal {
            if button == MouseButton::Left {
                match modal {
                    Modal::Settings(dialog) => {
                        dialog.pointer_released();
                    }
                    Modal::Composition(dialog) if !dialog.is_closing() => {
                        dialog.editor.pointer_released();
                    }
                    Modal::SpeedDuration(dialog) if !dialog.is_closing() => {
                        dialog.editor.pointer_released();
                    }
                    Modal::LayoutSave(dialog) if !dialog.is_closing() => {
                        dialog.editor.pointer_released();
                    }
                    Modal::About(_)
                    | Modal::Composition(_)
                    | Modal::SpeedDuration(_)
                    | Modal::LayoutSave(_)
                    | Modal::Keybinds(_)
                    | Modal::Discard(_)
                    | Modal::Busy(_)
                    | Modal::MissingMedia(_) => {}
                }
            }
            return;
        }
        if self.palette.kind.is_some() {
            if button == MouseButton::Left {
                self.palette.query.pointer_released();
            }
            return;
        }

        if button == MouseButton::Left && self.release_dock_drag() {
            return;
        }

        if button == MouseButton::Middle && self.monitor.pointer_middle_released() {
            return;
        }

        if button == MouseButton::Left && self.monitor.pointer_released() {
            return;
        }

        if button == MouseButton::Left {
            if let Some(DragState::Media {
                items,
                motion,
                open_on_click,
            }) = self.drag.take()
            {
                if motion.dragging {
                    if let Some((track, time)) = self
                        .editor
                        .timeline
                        .media_drop_anchor(&self.snapshot, self.cursor)
                    {
                        if self.insert_media_drag_items(&items, track, time) {
                            self.media.clear_selection();
                            self.playback.invalidate();
                        }
                    }
                } else if let Some(composition) = open_on_click {
                    self.switch_composition(composition);
                }
                return;
            }
        }

        let timeline_focused = self.focused_kind() == Some(PanelKind::Timeline);
        let inspector_focused = self.focused_kind() == Some(PanelKind::Inspector);
        let project_options_focused = self.focused_kind() == Some(PanelKind::ProjectOptions);
        let graph_focused = self.focused_kind() == Some(PanelKind::Pipeline);
        let render_focused = self.focused_kind() == Some(PanelKind::Render);
        if button == MouseButton::Left && self.widgets.pointer_released() {
            return;
        }
        if button == MouseButton::Left && inspector_focused {
            let handled = if self.media.selected_composition().is_some() {
                self.project_options.pointer_released()
            } else {
                self.inspector.pointer_released()
            };
            if handled {
                self.playback.invalidate();
                return;
            }
        }
        if button == MouseButton::Left
            && project_options_focused
            && self.project_options.pointer_released()
        {
            self.playback.invalidate();
            return;
        }
        if button == MouseButton::Left && render_focused && self.render_panel.pointer_released() {
            return;
        }
        if graph_focused && matches!(button, MouseButton::Left | MouseButton::Middle) {
            let was_dragging = self.pipeline_graph.is_dragging();
            if let Some((stack, _)) = self.focused_panel() {
                if let Some(content) = self.panel_content(stack) {
                    let action = self.pipeline_graph.pointer_released(
                        content,
                        self.cursor,
                        &self.editor.project,
                        &self.editor.timeline,
                        &self.plugins,
                    );
                    if !matches!(&action, PipelineGraphAction::None) {
                        self.handle_pipeline_graph_action(action);
                    }
                }
            }
            if was_dragging {
                return;
            }
        }
        if timeline_focused {
            let handled = self.editor.timeline.pointer_released(
                &self.snapshot,
                self.cursor,
                button,
                self.modifiers,
                &self.editor.project,
            );
            if let Some(action) = self.editor.timeline.take_action() {
                self.handle_timeline_action(action);
            }
            if handled {
                self.playback.invalidate();
            }
        }
    }

    fn handle_modal_key(&mut self, event: &KeyEvent) -> bool {
        let Some(mut modal) = self.modal.take() else {
            return false;
        };
        let mut keep = true;
        match &mut modal {
            Modal::About(dialog) => {
                if !dialog.is_closing()
                    && matches!(
                        event.logical_key,
                        Key::Named(NamedKey::Escape | NamedKey::Enter)
                    )
                {
                    dialog.close();
                }
            }
            Modal::Settings(dialog) => {
                dialog.handle_key(event, self.modifiers);
                self.plugin_paths = dialog.plugin_paths().to_owned();
                preferences::save(&self.command_registry, &self.plugin_paths);
                keep = !dialog.is_closed();
            }
            Modal::Keybinds(dialog) => {
                dialog.handle_key(event, self.modifiers, &mut self.command_registry);
                preferences::save(&self.command_registry, &self.plugin_paths);
                keep = !dialog.is_closed();
            }
            Modal::Composition(dialog) if !dialog.is_closing() => match event.logical_key {
                Key::Named(NamedKey::Escape) => dialog.close(),
                Key::Named(NamedKey::Enter) => {
                    self.confirm_new_composition_dialog(dialog);
                    dialog.close();
                }
                _ => {
                    dialog.editor.handle_key(event, self.modifiers);
                }
            },
            Modal::Composition(_) => {}
            Modal::SpeedDuration(dialog) if !dialog.is_closing() => match event.logical_key {
                Key::Named(NamedKey::Escape) => dialog.close(),
                Key::Named(NamedKey::Enter) => {
                    self.confirm_speed_duration_dialog(dialog);
                    dialog.close();
                }
                _ => {
                    dialog.editor.handle_key(event, self.modifiers);
                }
            },
            Modal::SpeedDuration(_) => {}
            Modal::Busy(dialog) => {
                if !dialog.is_closing() {
                    match event.logical_key {
                        Key::Named(NamedKey::Enter) => {
                            let action = dialog.action.clone();
                            self.render_panel.cancel_active();
                            self.request_discard_action_after_render(action);
                            keep = false;
                        }
                        Key::Named(NamedKey::Escape) => dialog.close(),
                        _ => {}
                    }
                }
            }
            Modal::Discard(dialog) => {
                if !dialog.is_closing() {
                    match event.logical_key {
                        Key::Named(NamedKey::Enter) => {
                            let action = dialog.action.clone();
                            dialog.close();
                            self.perform_discard_action(action);
                        }
                        Key::Named(NamedKey::Escape) => dialog.close(),
                        _ => {}
                    }
                }
            }
            Modal::MissingMedia(dialog) if !dialog.is_closing() => match event.logical_key {
                Key::Named(NamedKey::Escape) => dialog.close(),
                Key::Named(NamedKey::Enter) => {
                    self.confirm_missing_media_load(dialog);
                    dialog.close();
                }
                _ => {}
            },
            Modal::MissingMedia(_) => {}
            Modal::LayoutSave(dialog) => {
                if !dialog.is_closing() {
                    match event.logical_key {
                        Key::Named(NamedKey::Escape) => dialog.close(),
                        Key::Named(NamedKey::Enter) => {
                            let name = dialog.editor.text().to_string();
                            dialog.close();
                            self.handle_layout_command(LayoutCommand::SaveNamed(name));
                        }
                        _ => {
                            dialog.editor.handle_key(event, self.modifiers);
                        }
                    }
                }
            }
        }
        self.restore_handled_modal(modal, keep);
        true
    }

    fn handle_key(&mut self, event: &KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }
        if self.handle_modal_key(event) {
            return;
        }

        #[cfg(not(target_os = "macos"))]
        if !matches!(self.app_menu, AppMenuState::Closed) && self.handle_app_menu_key(event) {
            return;
        }

        if self.focused_kind() == Some(PanelKind::Monitor) {
            if let Some(content) = self.focused_content(PanelKind::Monitor) {
                let direction = match event.logical_key {
                    Key::Named(NamedKey::ArrowUp) => Some(-1),
                    Key::Named(NamedKey::ArrowDown) => Some(1),
                    _ => None,
                };
                if direction.is_some_and(|direction| {
                    self.monitor.cycle_hover_selection(
                        content,
                        self.cursor,
                        &self.editor.project,
                        &mut self.editor.timeline,
                        self.playback.preview_output().source_geometry,
                        direction,
                    )
                }) {
                    self.playback.invalidate();
                    return;
                }
            }
            let command_modifier = self.modifiers.super_key() || self.modifiers.control_key();
            if !command_modifier
                && matches!(&event.logical_key, Key::Character(text) if text.as_str().eq_ignore_ascii_case("f"))
            {
                self.monitor.zoom_to_fit();
                return;
            }
            if matches!(
                event.logical_key,
                Key::Named(NamedKey::Delete | NamedKey::Backspace)
            ) {
                if let Some(content) = self.focused_content(PanelKind::Monitor) {
                    let graph_selection =
                        self.pipeline_graph.monitor_selection(&self.editor.timeline);
                    if self.monitor.delete_selected_pen_point(
                        content,
                        &mut self.editor.project,
                        &mut self.editor.timeline,
                        &self.plugins,
                        graph_selection,
                        self.playback.preview_output().source_geometry,
                    ) {
                        self.playback.invalidate();
                        return;
                    }
                }
            }
        }

        let text_editing = self.ime_area().is_some();
        let command_scope = if self.focused_kind() == Some(PanelKind::Media) {
            CommandScope::Media
        } else {
            CommandScope::Global
        };
        let graph_delete_key = self.focused_kind() == Some(PanelKind::Pipeline)
            && matches!(
                event.logical_key,
                Key::Named(NamedKey::Delete | NamedKey::Backspace)
            );
        if !text_editing && !graph_delete_key {
            if let Some(command) =
                self.command_registry
                    .command_for_key(event, self.modifiers, command_scope)
            {
                self.command_queue.push(command);
                return;
            }
        }

        let command_modifier = self.modifiers.super_key() || self.modifiers.control_key();
        let skip_history = command_modifier
            && matches!(
                &event.logical_key,
                Key::Character(text)
                    if text.as_str().eq_ignore_ascii_case("s")
                        || text.as_str().eq_ignore_ascii_case("o")
                        || text.as_str().eq_ignore_ascii_case("n")
            );
        let before = (!skip_history).then(|| {
            self.editor
                .history
                .capture(&self.editor.project, &self.editor.timeline)
        });
        let history_label = self.key_history_label();
        let coalesce = event
            .text
            .as_deref()
            .is_some_and(|text| !text.chars().all(char::is_control));
        self.handle_key_inner(event);
        if let Some(before) = before {
            self.record_key_history(before, history_label, coalesce);
        }
    }

    fn handle_key_inner(&mut self, event: &KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }

        #[cfg(not(target_os = "macos"))]
        if self.handle_app_menu_key(event) {
            return;
        }

        if matches!(event.logical_key, Key::Named(NamedKey::Escape)) {
            if let Some(DragState::Dock {
                item:
                    DockDrag::Tab {
                        tab,
                        source_stack,
                        original_index,
                    },
                ..
            }) = self.drag.as_ref()
            {
                let (tab, source_stack, original_index) = (*tab, *source_stack, *original_index);
                self.drag = None;
                self.dock.drop_tab(
                    tab,
                    source_stack,
                    source_stack,
                    DropZone::Center,
                    Some(original_index),
                );
                self.animated_drop_preview = None;
                return;
            }
        }

        let command_modifier = self.modifiers.super_key() || self.modifiers.control_key();
        if self.palette.kind.is_none() {
            let focused_panel = self.focused_kind();
            if focused_panel == Some(PanelKind::Widgets)
                && self.widgets.handle_key(event, self.modifiers)
            {
                return;
            }
            if focused_panel == Some(PanelKind::Media) {
                if let Some(content) = self.focused_content(PanelKind::Media) {
                    if self
                        .media
                        .handle_key(content, event, self.modifiers, &self.editor.project)
                    {
                        return;
                    }
                }
            }
            if focused_panel == Some(PanelKind::Inspector) {
                let handled = if let Some(composition) = self.media.selected_composition() {
                    self.project_options.handle_key(
                        event,
                        self.modifiers,
                        &mut self.editor.project,
                        composition,
                    )
                } else if self.media.selected().is_none() {
                    self.inspector.handle_key(
                        event,
                        self.modifiers,
                        &mut self.editor.project,
                        &mut self.editor.timeline,
                    )
                } else {
                    false
                };
                if handled {
                    self.playback.invalidate();
                    return;
                }
            }
            if focused_panel == Some(PanelKind::ProjectOptions) {
                let composition = self.editor.project.active_composition;
                if self.project_options.handle_key(
                    event,
                    self.modifiers,
                    &mut self.editor.project,
                    composition,
                ) {
                    self.playback.invalidate();
                    return;
                }
            }
            if focused_panel == Some(PanelKind::Render)
                && self.render_panel.handle_key(event, self.modifiers)
            {
                return;
            }
            if focused_panel == Some(PanelKind::Pipeline)
                && self.pipeline_graph.handle_key(
                    event,
                    self.modifiers,
                    &mut self.editor.project,
                    &mut self.editor.timeline,
                )
            {
                if let Some(action) = self.pipeline_graph.take_action() {
                    self.handle_pipeline_graph_action(action);
                }
                self.playback.invalidate();
                return;
            }
            if focused_panel == Some(PanelKind::Pipeline)
                && !command_modifier
                && matches!(&event.logical_key, Key::Character(text) if text.as_str().eq_ignore_ascii_case("f"))
            {
                if let Some((stack, _)) = self.focused_panel() {
                    if let Some(content) = self.panel_content(stack) {
                        if self.pipeline_graph.frame_all(
                            content,
                            &self.editor.project,
                            &self.editor.timeline,
                            &self.plugins,
                        ) {
                            return;
                        }
                    }
                }
            }
            if focused_panel == Some(PanelKind::Timeline)
                && self
                    .editor
                    .timeline
                    .handle_key(&self.snapshot, event, self.modifiers)
            {
                if let Some(action) = self.editor.timeline.take_action() {
                    self.handle_timeline_action(action);
                }
                return;
            }

            if matches!(event.logical_key, Key::Named(NamedKey::Escape)) {
                self.clear_editor_selection();
                return;
            }

            let shift_a = self.modifiers.shift_key()
                && !command_modifier
                && matches!(&event.logical_key, Key::Character(text) if text.as_str().eq_ignore_ascii_case("a"));
            if shift_a {
                match focused_panel {
                    Some(PanelKind::Timeline) => {
                        if let Some((track, time, kind)) = self
                            .editor
                            .timeline
                            .insert_target(&self.snapshot, self.cursor)
                        {
                            self.palette.pending_open = Some((
                                PaletteKind::TimelineAdd { track, time, kind },
                                Some(Rect::new(self.cursor[0], self.cursor[1], 1.0, 1.0)),
                            ));
                            return;
                        }
                    }
                    Some(PanelKind::Pipeline) => {
                        let mut graph_changed = false;
                        let pipeline = if let Some(id) = self.graph_pipeline() {
                            id
                        } else if self.editor.timeline.can_assign_pipeline() {
                            let kind = self.editor.timeline.selected_pipeline_kind();
                            let id = self.editor.project.create_pipeline_kind(kind);
                            self.editor.timeline.set_selected_pipeline(Some(id));
                            self.pipeline_graph.follow_selection();
                            graph_changed = true;
                            id
                        } else {
                            let id = self.editor.project.create_pipeline();
                            self.pipeline_graph.open_pipeline(id);
                            graph_changed = true;
                            id
                        };
                        if graph_changed {
                            self.sync_effect_runtime();
                        }
                        if let Some((stack, _)) = self.focused_panel() {
                            if let Some(content) = self.panel_content(stack) {
                                let point = if content.contains(self.cursor) {
                                    self.cursor
                                } else {
                                    [
                                        content.x + content.width * 0.5,
                                        content.y + content.height * 0.5,
                                    ]
                                };
                                let position =
                                    self.pipeline_graph.insertion_position(content, point);
                                self.palette.pending_open = Some((
                                    PaletteKind::NodeInsert { pipeline, position },
                                    Some(Rect::new(point[0], point[1], 1.0, 1.0)),
                                ));
                                return;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        if self.palette.kind.is_none() {
            return;
        }

        let add_menu = self.palette.kind.is_some_and(PaletteKind::is_add_menu);
        let empty_query = self.palette.query.text().trim().is_empty();
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                if add_menu && !self.palette.path.is_empty() {
                    self.palette.query.reset("");
                    self.palette_back();
                } else {
                    self.palette.close();
                }
            }
            Key::Named(NamedKey::ArrowDown) => self.move_palette_selection(1),
            Key::Named(NamedKey::ArrowUp) => self.move_palette_selection(-1),
            Key::Named(NamedKey::ArrowRight) if add_menu && empty_query => {
                self.open_selected_palette_submenu();
            }
            Key::Named(NamedKey::ArrowLeft) | Key::Named(NamedKey::Backspace)
                if add_menu && empty_query =>
            {
                self.palette_back();
            }
            Key::Named(NamedKey::Enter) => self.accept_palette(),
            _ => {
                let response = self.palette.query.handle_key(event, self.modifiers);
                if response.changed {
                    self.reset_palette_page();
                }
            }
        }
    }

    fn handle_ime(&mut self, event: &Ime) {
        let before = self
            .editor
            .history
            .capture(&self.editor.project, &self.editor.timeline);
        let history_label = self.key_history_label();
        self.handle_ime_inner(event);
        self.record_key_history(before, history_label, true);
    }

    fn handle_ime_inner(&mut self, event: &Ime) {
        if let Some(modal) = &mut self.modal {
            match modal {
                Modal::Settings(dialog) => {
                    if dialog.handle_ime(event) {
                        self.plugin_paths = dialog.plugin_paths().to_owned();
                        preferences::save(&self.command_registry, &self.plugin_paths);
                    }
                }
                Modal::Keybinds(dialog) => {
                    if dialog.handle_ime(event) {
                        preferences::save(&self.command_registry, &self.plugin_paths);
                    }
                }
                Modal::Composition(dialog) if !dialog.is_closing() => {
                    dialog.editor.handle_ime(event);
                }
                Modal::SpeedDuration(dialog) if !dialog.is_closing() => {
                    dialog.editor.handle_ime(event);
                }
                Modal::LayoutSave(dialog) if !dialog.is_closing() => {
                    dialog.editor.handle_ime(event);
                }
                Modal::About(_)
                | Modal::Composition(_)
                | Modal::SpeedDuration(_)
                | Modal::LayoutSave(_)
                | Modal::Discard(_)
                | Modal::Busy(_)
                | Modal::MissingMedia(_) => {}
            }
            return;
        }
        if self.palette.kind.is_some() {
            let response = self.palette.query.handle_ime(event);
            if response.changed {
                self.reset_palette_page();
            }
            return;
        }
        match self.focused_kind() {
            Some(PanelKind::Widgets) => {
                self.widgets.handle_ime(event);
            }
            Some(PanelKind::Inspector) => {
                let handled = if let Some(composition) = self.media.selected_composition() {
                    self.project_options
                        .handle_ime(event, &mut self.editor.project, composition)
                } else if self.media.selected().is_none() {
                    self.inspector.handle_ime(
                        event,
                        &mut self.editor.project,
                        &mut self.editor.timeline,
                    )
                } else {
                    false
                };
                if handled {
                    self.playback.invalidate();
                }
            }
            Some(PanelKind::ProjectOptions) => {
                let composition = self.editor.project.active_composition;
                if self
                    .project_options
                    .handle_ime(event, &mut self.editor.project, composition)
                {
                    self.playback.invalidate();
                }
            }
            Some(PanelKind::Render) => {
                self.render_panel.handle_ime(event);
            }
            Some(PanelKind::Pipeline) => {
                self.pipeline_graph.handle_ime(
                    event,
                    &mut self.editor.project,
                    &mut self.editor.timeline,
                );
            }
            _ => {}
        }
    }

    fn ime_area(&self) -> Option<Rect> {
        let width = self.renderer.logical_width();
        let height = self.renderer.logical_height();
        if let Some(modal) = &self.modal {
            return match modal {
                Modal::Settings(dialog) => dialog.caret_rect(width, height),
                Modal::Keybinds(dialog) => dialog.caret_rect(width, height),
                Modal::Composition(dialog) if !dialog.is_closing() => {
                    Some(dialog.editor.caret_rect(
                        NEW_COMPOSITION_MODAL.input(NEW_COMPOSITION_MODAL.rect(width, height)),
                    ))
                }
                Modal::SpeedDuration(dialog) if !dialog.is_closing() => {
                    Some(dialog.editor.caret_rect(
                        SPEED_DURATION_MODAL.input(SPEED_DURATION_MODAL.rect(width, height)),
                    ))
                }
                Modal::LayoutSave(dialog) if !dialog.is_closing() => Some(
                    dialog
                        .editor
                        .caret_rect(LAYOUT_SAVE_MODAL.input(LAYOUT_SAVE_MODAL.rect(width, height))),
                ),
                Modal::About(_)
                | Modal::Composition(_)
                | Modal::SpeedDuration(_)
                | Modal::LayoutSave(_)
                | Modal::Discard(_)
                | Modal::Busy(_)
                | Modal::MissingMedia(_) => None,
            };
        }
        if self.palette.kind.is_some() {
            if !self.palette.query.is_focused() {
                return None;
            }
            let entries = palette_entries(
                &self.palette,
                &self.editor.project,
                &self.plugins,
                &self.command_registry,
            );
            let rows =
                palette_visible_rows(&self.palette, entries.len(), self.renderer.logical_height());
            let popup = palette_rect(
                self.renderer.logical_width(),
                self.renderer.logical_height(),
                &self.palette,
                rows,
            );
            return Some(
                self.palette
                    .query
                    .caret_rect(palette_input_rect(popup, &self.palette)),
            );
        }
        let (stack, panel) = self.focused_panel()?;
        let rect = self.snapshot.stack(stack)?.content;
        match panel {
            PanelKind::Widgets => self.widgets.ime_area(rect),
            PanelKind::Inspector => self.media.selected_composition().map_or_else(
                || {
                    self.media
                        .selected()
                        .is_none()
                        .then(|| {
                            self.inspector.ime_area(
                                rect,
                                &self.editor.project,
                                &self.editor.timeline,
                                &self.plugins,
                            )
                        })
                        .flatten()
                },
                |_| self.project_options.ime_area(rect),
            ),
            PanelKind::ProjectOptions => self.project_options.ime_area(rect),
            PanelKind::Render => self.render_panel.ime_area(rect),
            PanelKind::Pipeline => self.pipeline_graph.ime_area(rect),
            _ => None,
        }
    }

    fn sync_ime(&mut self) {
        let area = self.ime_area();
        let update = self.ime_sync.update(area);
        if let Some(allowed) = update.allowed {
            self.window.set_ime_allowed(allowed);
        }
        if let Some(area) = update.area {
            self.window.set_ime_cursor_area(
                LogicalPosition::new(area.x as f64, area.y as f64),
                LogicalSize::new(area.width.max(1.0) as f64, area.height.max(1.0) as f64),
            );
        }
    }

    fn open_panel(&mut self, panel: PanelKind) {
        if let Some((stack_id, tab_id)) = self.snapshot.stacks.iter().find_map(|stack| {
            stack
                .stack
                .tabs
                .iter()
                .find(|tab| tab.title == panel.layout_title())
                .map(|tab| (stack.stack.id, tab.id))
        }) {
            self.dock.activate_tab(stack_id, tab_id);
            self.set_panel_focus(stack_id, panel);
            return;
        }
        let neighbor = if panel == PanelKind::Pipeline {
            PanelKind::Timeline
        } else {
            PanelKind::Inspector
        };
        let target = self
            .snapshot
            .stacks
            .iter()
            .find(|stack| {
                stack
                    .stack
                    .tabs
                    .iter()
                    .any(|tab| tab.title == neighbor.layout_title())
            })
            .map(|stack| stack.stack.id)
            .or(self.dock.focused);
        if let Some(stack) = target {
            self.dock.add_tab(stack, panel.layout_title());
            self.set_panel_focus(stack, panel);
        }
    }

    fn press_inspector(&mut self, content: Rect) -> bool {
        if let Some(composition) = self.media.selected_composition() {
            if !self.project_options.pointer_pressed(
                content,
                self.cursor,
                self.modifiers,
                &mut self.editor.project,
                composition,
            ) {
                return false;
            }
            self.playback.invalidate();
            return true;
        }
        if !self.inspector.pointer_pressed(
            content,
            self.cursor,
            InspectorPointerContext {
                modifiers: self.modifiers,
                project: &mut self.editor.project,
                timeline: &mut self.editor.timeline,
                media_selection: self.media.selected_with_stream(),
                plugins: &self.plugins,
            },
        ) {
            return false;
        }
        self.playback.invalidate();
        if let Some(action) = self.inspector.take_action() {
            self.handle_inspector_action(action);
        }
        true
    }

    fn press_project_options(&mut self, content: Rect) -> bool {
        let composition = self.editor.project.active_composition;
        if !self.project_options.pointer_pressed(
            content,
            self.cursor,
            self.modifiers,
            &mut self.editor.project,
            composition,
        ) {
            return false;
        }
        self.playback.invalidate();
        true
    }

    fn has_unsaved_changes(&self) -> bool {
        self.editor.has_unsaved_changes()
    }

    fn request_discard_action(&mut self, action: PendingDiscardAction) {
        if self.modal.as_ref().is_some_and(|modal| {
            !matches!(
                modal,
                Modal::About(_) | Modal::Settings(_) | Modal::Keybinds(_)
            )
        }) {
            return;
        }
        if self.render_panel.is_active() {
            self.open_modal(Modal::Busy(ActionDialog::new(action)));
        } else {
            self.request_discard_action_after_render(action);
        }
    }

    fn request_discard_action_after_render(&mut self, action: PendingDiscardAction) {
        if self.has_unsaved_changes() {
            self.open_modal(Modal::Discard(ActionDialog::new(action)));
        } else {
            self.perform_discard_action(action);
        }
    }

    fn perform_discard_action(&mut self, action: PendingDiscardAction) {
        match action {
            PendingDiscardAction::Exit => self.exit_requested = true,
            PendingDiscardAction::NewProject => self.new_project_unchecked(),
            PendingDiscardAction::OpenProjectDialog => self.open_project_dialog_unchecked(),
            PendingDiscardAction::LoadProject(path) => {
                let _ = self.load_project_unchecked(&path);
            }
        }
    }

    fn request_exit(&mut self) {
        self.request_discard_action(PendingDiscardAction::Exit);
    }

    fn new_project(&mut self) {
        self.request_discard_action(PendingDiscardAction::NewProject);
    }

    fn import_clipboard_image(&mut self) -> Result<()> {
        let mut clipboard = arboard::Clipboard::new().context("open system clipboard")?;
        let image = clipboard
            .get_image()
            .context("clipboard does not contain an image")?;
        let width = u32::try_from(image.width).context("clipboard image width is too large")?;
        let height = u32::try_from(image.height).context("clipboard image height is too large")?;
        let pixels = image::RgbaImage::from_raw(width, height, image.bytes.into_owned())
            .context("clipboard image buffer size is invalid")?;

        let root = self
            .editor
            .project_path
            .as_ref()
            .and_then(|path| path.parent())
            .map(Path::to_path_buf)
            .unwrap_or(std::env::current_dir()?);
        let imported = root.join("Imported");
        std::fs::create_dir_all(&imported)
            .with_context(|| format!("create {}", imported.display()))?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let mut path = imported.join(format!("Clipboard-{stamp}.png"));
        let mut suffix = 2u32;
        while path.exists() {
            path = imported.join(format!("Clipboard-{stamp}-{suffix}.png"));
            suffix = suffix.saturating_add(1);
        }
        image::DynamicImage::ImageRgba8(pixels)
            .save(&path)
            .with_context(|| format!("save {}", path.display()))?;
        self.import_path(path)
    }

    fn handle_monitor_action(&mut self, action: MonitorAction) {
        let size = self.editor.project.active_settings().canvas_size;
        let playhead = self.editor.timeline.playhead();
        let captured = self
            .playback
            .render_export_rgba16_on(crate::playback::ExportRgba16Args {
                device: self.renderer.device(),
                queue: self.renderer.queue(),
                project: &self.editor.project,
                timeline: &self.editor.timeline,
                runtime: (&self.effects, &self.plugins),
                timeline_time: playhead,
            });
        let pixels = match captured {
            Ok(pixels) => pixels,
            Err(error) => {
                messages::error("Frame capture", format!("capture failed: {error:#}"));
                return;
            }
        };
        match action {
            MonitorAction::CaptureTemporaryFrame => {
                self.monitor.set_captured_frame(size, pixels);
            }
            MonitorAction::CaptureFrame => {
                let before = self
                    .editor
                    .history
                    .capture(&self.editor.project, &self.editor.timeline);
                let result = (|| -> Result<PathBuf> {
                    let root = self
                        .editor
                        .project_path
                        .as_ref()
                        .and_then(|path| path.parent())
                        .map(Path::to_path_buf)
                        .unwrap_or(std::env::current_dir()?);
                    let captures = root.join("Captures");
                    std::fs::create_dir_all(&captures)?;
                    let fps = self.editor.project.active_settings().frame_rate.max(1.0);
                    let frame = (playhead.max(0.0) as f64 * fps).round() as u64;
                    let stem = sanitize_file_name(&self.editor.project.active_composition().name);
                    let base = format!("{stem}-frame-{frame:06}");
                    let mut path = captures.join(format!("{base}.png"));
                    let mut suffix = 2u32;
                    while path.exists() {
                        path = captures.join(format!("{base}-{suffix}.png"));
                        suffix = suffix.saturating_add(1);
                    }
                    let samples = pixels
                        .chunks_exact(2)
                        .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
                        .collect::<Vec<_>>();
                    let image = image::ImageBuffer::<image::Rgba<u16>, Vec<u16>>::from_raw(
                        size[0].max(1),
                        size[1].max(1),
                        samples,
                    )
                    .context("captured frame size did not match the composition")?;
                    image::DynamicImage::ImageRgba16(image)
                        .save(&path)
                        .with_context(|| format!("save {}", path.display()))?;
                    self.import_path(path.clone())?;
                    Ok(path)
                })();
                match result {
                    Ok(_path) => {
                        self.monitor.set_captured_frame(size, pixels);
                        self.editor.history.record_after(
                            "Capture frame",
                            before,
                            &self.editor.project,
                            &self.editor.timeline,
                            false,
                        );
                    }
                    Err(error) => {
                        messages::error("Frame capture", format!("save failed: {error:#}"))
                    }
                }
            }
        }
    }

    fn warm_media_scrub_thumbnails(asset: &MediaAsset) {
        if !matches!(asset.kind, MediaKind::Video) {
            return;
        }
        let width = asset.video_width.unwrap_or(1).max(1);
        let height = asset.video_height.unwrap_or(1).max(1);
        let fps = asset
            .frame_rate
            .unwrap_or(runtime::media::SCRUB_PREVIEW_FPS)
            .max(1.0);
        runtime::media::warm_video_preview_cache(&asset.path, fps, width, height);
    }

    fn warm_project_scrub_thumbnails(&self) {
        runtime::media::retain_video_preview_caches(
            self.editor
                .project
                .media
                .iter()
                .filter(|asset| matches!(asset.kind, MediaKind::Video))
                .map(|asset| asset.path.as_path()),
        );
        for asset in &self.editor.project.media {
            Self::warm_media_scrub_thumbnails(asset);
        }
    }

    fn import_media_path(&mut self, path: PathBuf) -> Result<MediaId> {
        let is_wasm = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("wasm"));
        let media = project_io::import_media(&mut self.editor.project, path.clone())?;
        if let Some(asset) = self.editor.project.media(media) {
            self.waveform_textures.queue_asset(asset);
            Self::warm_media_scrub_thumbnails(asset);
        }
        self.playback.clear_media_caches();
        if is_wasm {
            self.playback.precompile_wasm(&path)?;
        }
        Ok(media)
    }

    fn import_path(&mut self, path: PathBuf) -> Result<()> {
        if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("kama"))
        {
            self.load_project(&path);
            return Ok(());
        }
        self.import_media_path(path).map(|_| ())
    }

    fn hover_external_file(&mut self, path: PathBuf) {
        if self
            .external_drag_items
            .iter()
            .any(|item| item.path == path)
        {
            return;
        }
        if self.external_drag_items.is_empty() {
            self.external_drag_uses_window_cursor = false;
        }
        self.ignored_external_drops.remove(&path);
        self.external_drag_items.push(ExternalDragItem {
            preview: external_media_preview_spec(&path),
            path,
        });
    }

    fn move_external_drag_cursor_raw(&mut self, delta: (f64, f64)) {
        if self.external_drag_items.is_empty() || self.external_drag_uses_window_cursor {
            return;
        }
        let size = self.window.inner_size();
        self.cursor_physical[0] = (self.cursor_physical[0] + delta.0).clamp(0.0, size.width as f64);
        self.cursor_physical[1] =
            (self.cursor_physical[1] + delta.1).clamp(0.0, size.height as f64);
        let scale = self.renderer.scale_factor() as f64;
        self.cursor = [
            (self.cursor_physical[0] / scale) as f32,
            (self.cursor_physical[1] / scale) as f32,
        ];
        self.input.cursor = self.cursor;
        self.pointer_moved();
        self.window.request_redraw();
    }

    fn drop_external_file(&mut self, path: PathBuf) {
        self.external_drag_uses_window_cursor = false;
        if self.ignored_external_drops.remove(&path) {
            return;
        }
        let mut paths = if self
            .external_drag_items
            .iter()
            .any(|item| item.path == path)
        {
            self.external_drag_items
                .drain(..)
                .map(|item| item.path)
                .collect::<Vec<_>>()
        } else {
            vec![path.clone()]
        };
        if !paths.iter().any(|candidate| candidate == &path) {
            paths.insert(0, path.clone());
        }
        for candidate in &paths {
            if candidate != &path {
                self.ignored_external_drops.insert(candidate.clone());
            }
        }
        self.handle_external_drop(paths);
    }

    fn handle_external_drop(&mut self, paths: Vec<PathBuf>) {
        if paths.len() == 1
            && paths[0].is_file()
            && paths[0]
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("kama"))
        {
            self.command_queue
                .push(EditorCommand::Action(PaletteAction::OpenRecentProject(
                    paths[0].clone(),
                )));
            return;
        }

        let target = self
            .editor
            .timeline
            .media_drop_anchor(&self.snapshot, self.cursor);
        let before = self
            .editor
            .history
            .capture(&self.editor.project, &self.editor.timeline);
        let mut items = Vec::new();
        for path in paths {
            if !path.is_file() {
                messages::warning(
                    "Import media",
                    format!("Skipped non-file {}", path.display()),
                );
                continue;
            }
            if path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("kama"))
            {
                messages::warning(
                    "Import media",
                    format!(
                        "Skipped project file {} in a multi-file media drop",
                        path.display()
                    ),
                );
                continue;
            }
            match self.import_media_path(path.clone()) {
                Ok(media) => items.push(MediaDragItem::Media {
                    media,
                    stream: MediaStream::All,
                }),
                Err(error) => {
                    messages::warning("Import media", format!("{}: {error:#}", path.display()))
                }
            }
        }
        if items.is_empty() {
            return;
        }

        let inserted =
            target.is_some_and(|(track, time)| self.insert_media_drag_items(&items, track, time));
        if inserted {
            self.media.clear_selection();
            self.playback.invalidate();
        }
        self.editor.history.record_after(
            if inserted {
                "Drop media into timeline"
            } else {
                "Import media"
            },
            before,
            &self.editor.project,
            &self.editor.timeline,
            false,
        );
    }

    fn update_window_title(&self) {
        let location = self
            .editor
            .project_path
            .as_ref()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .unwrap_or("Untitled.kama");
        let dirty = if self.editor.is_unsaved() { "*" } else { "" };
        let title = format!("Kama Studio — {location}{dirty}");
        self.window.set_title(&title);
        for state in self.secondary_windows.values() {
            state.window.set_title(&title);
        }
    }

    fn refresh_window_title_if_needed(&mut self) {
        if self.editor.refresh_dirty_state() {
            self.update_window_title();
        }
    }

    fn mark_document_saved(&mut self) {
        self.editor.mark_saved();
    }

    fn reset_palette_page(&mut self) {
        self.palette.selected = 0;
        self.palette.scroll.offset = 0.0;
        self.palette.hovered = None;
    }

    fn keep_palette_selection_visible(&mut self, entries: usize) {
        let visible = palette_visible_rows(&self.palette, entries, self.renderer.logical_height());
        if entries == 0 || visible == 0 {
            self.reset_palette_page();
            return;
        }
        self.palette.selected = self.palette.selected.min(entries - 1);
        let popup = palette_rect(
            self.renderer.logical_width(),
            self.renderer.logical_height(),
            &self.palette,
            visible,
        );
        let viewport_extent = palette_body_rect(popup, &self.palette, visible).height;
        let row = palette_unscrolled_rows(&self.palette, entries)[self.palette.selected];
        let top = row.y;
        let bottom = row.bottom();
        let max_scroll = palette_max_scroll(&self.palette, entries, visible);
        let offset = if top < self.palette.scroll.offset {
            top
        } else if bottom > self.palette.scroll.offset + viewport_extent {
            bottom - viewport_extent
        } else {
            self.palette.scroll.offset
        };
        self.palette.scroll.offset = offset.clamp(0.0, max_scroll);
    }

    fn move_palette_selection(&mut self, delta: i32) {
        let entries = palette_entries(
            &self.palette,
            &self.editor.project,
            &self.plugins,
            &self.command_registry,
        );
        let len = entries.len() as i32;
        if len > 0 {
            self.palette.selected = (self.palette.selected as i32 + delta).rem_euclid(len) as usize;
            self.keep_palette_selection_visible(entries.len());
        }
    }

    fn palette_back(&mut self) -> bool {
        if self.palette.path.is_empty() {
            return false;
        }
        self.palette.path.pop();
        self.reset_palette_page();
        true
    }

    fn open_selected_palette_submenu(&mut self) -> bool {
        let entries = palette_entries(
            &self.palette,
            &self.editor.project,
            &self.plugins,
            &self.command_registry,
        );
        let submenu = entries
            .into_iter()
            .nth(self.palette.selected)
            .and_then(|entry| match entry.target {
                PaletteTarget::Submenu(submenu) => Some(submenu),
                PaletteTarget::Command(_) => None,
            });
        let Some(submenu) = submenu else {
            return false;
        };
        self.palette.path.push(submenu);
        self.reset_palette_page();
        true
    }

    fn scroll_palette(&mut self, delta: [f32; 2]) -> bool {
        let entries = palette_entries(
            &self.palette,
            &self.editor.project,
            &self.plugins,
            &self.command_registry,
        );
        let visible =
            palette_visible_rows(&self.palette, entries.len(), self.renderer.logical_height());
        let popup = palette_rect(
            self.renderer.logical_width(),
            self.renderer.logical_height(),
            &self.palette,
            visible,
        );
        if !palette_body_rect(popup, &self.palette, visible).contains(self.cursor) {
            return false;
        }
        let axis = if delta[1].abs() >= delta[0].abs() {
            delta[1]
        } else {
            delta[0]
        };
        if axis.abs() < f32::EPSILON {
            return true;
        }
        let max_scroll = palette_max_scroll(&self.palette, entries.len(), visible);
        self.palette.scroll.scroll_by(-axis, max_scroll);
        self.palette.hovered = None;
        true
    }

    fn accept_palette(&mut self) {
        let entries = palette_entries(
            &self.palette,
            &self.editor.project,
            &self.plugins,
            &self.command_registry,
        );
        let target = entries
            .into_iter()
            .nth(self.palette.selected)
            .map(|entry| entry.target);
        match target {
            Some(PaletteTarget::Command(command)) => {
                self.command_queue.push(command);
                self.palette.close();
            }
            Some(PaletteTarget::Submenu(submenu)) => {
                self.palette.path.push(submenu);
                self.reset_palette_page();
            }
            None => {}
        }
    }

    fn drain_commands(&mut self) {
        while let Some(command) = self.command_queue.pop() {
            self.execute_command(command);
        }
    }

    fn execute_command(&mut self, command: EditorCommand) {
        match command {
            EditorCommand::Edit(command) => self.execute_edit_command(command),
            EditorCommand::Action(action) => self.execute_palette_action(action),
            EditorCommand::Layout(command) => self.execute_layout_command(command),
            EditorCommand::Dock(command) => match command {
                DockCommand::ToggleMaximize(stack) => {
                    self.dock.toggle_maximize(stack);
                }
                DockCommand::CloseTab { stack, tab } => {
                    self.dock.close_tab(stack, tab);
                }
                DockCommand::ActivateTab { stack, tab } => self.dock.activate_tab(stack, tab),
            },
            EditorCommand::OpenCommandPalette => {
                if self.modal.is_none() && self.modal_queue.is_empty() {
                    self.palette.pending_open = Some((PaletteKind::Commands, None));
                }
            }
            EditorCommand::ToggleCurrentPanelMaximize => {
                if let Some(stack) = self.dock.focused {
                    self.dock.toggle_maximize(stack);
                }
            }
            EditorCommand::TogglePenTool => self.monitor.toggle_pen_tool(),
            EditorCommand::OpenSettings => self.open_modal(Modal::Settings(Box::new(
                SettingsDialog::new(&self.plugin_paths),
            ))),
            EditorCommand::OpenKeybinds => {
                self.open_modal(Modal::Keybinds(Box::new(KeybindsDialog::new())));
            }
            EditorCommand::OpenUrl(url) => {
                if let Err(error) = open::that(url) {
                    eprintln!("failed to open URL {url}: {error}");
                }
            }
            EditorCommand::Exit => self.request_exit(),
        }
    }

    fn execute_edit_command(&mut self, command: EditCommand) {
        match command {
            EditCommand::Timeline(action) => {
                let before = self.editor.history_gesture.is_none().then(|| {
                    self.editor
                        .history
                        .capture(&self.editor.project, &self.editor.timeline)
                });
                self.execute_timeline_action(action);
                if let Some(before) = before {
                    self.editor.history.record_after(
                        action.history_label(),
                        before,
                        &self.editor.project,
                        &self.editor.timeline,
                        false,
                    );
                }
            }
            EditCommand::Inspector(action) => {
                let before = self.editor.history_gesture.is_none().then(|| {
                    self.editor
                        .history
                        .capture(&self.editor.project, &self.editor.timeline)
                });
                self.execute_inspector_action(action);
                if let Some(before) = before {
                    self.editor.history.record_after(
                        "Inspector command",
                        before,
                        &self.editor.project,
                        &self.editor.timeline,
                        false,
                    );
                }
            }
            EditCommand::PipelineGraph(action) => {
                let before = self.editor.history_gesture.is_none().then(|| {
                    self.editor
                        .history
                        .capture(&self.editor.project, &self.editor.timeline)
                });
                self.execute_pipeline_graph_action(action);
                if let Some(before) = before {
                    self.editor.history.record_after(
                        "Graph command",
                        before,
                        &self.editor.project,
                        &self.editor.timeline,
                        false,
                    );
                }
            }
            EditCommand::Undo => {
                if let Some(snapshot) = self.editor.history.undo() {
                    self.editor.history_gesture = None;
                    self.restore_history_snapshot(snapshot);
                }
            }
            EditCommand::Redo => {
                if let Some(snapshot) = self.editor.history.redo() {
                    self.editor.history_gesture = None;
                    self.restore_history_snapshot(snapshot);
                }
            }
            EditCommand::RestoreHistory(snapshot) => {
                self.editor.history_gesture = None;
                self.restore_history_snapshot(snapshot);
            }
        }
    }

    fn execute_palette_action(&mut self, action: PaletteAction) {
        let history_label = match &action {
            PaletteAction::ImportMedia | PaletteAction::ImportClipboard => "Import media",
            PaletteAction::InsertGenerator { .. } => "Insert generator clip",
            PaletteAction::InsertMedia { .. } | PaletteAction::InsertAudioMedia { .. } => {
                "Insert media clip"
            }
            PaletteAction::ReplaceSelectedClips { .. } => "Replace clip source",
            PaletteAction::AssignPipeline(_) => "Assign pipeline",
            PaletteAction::CreateAndAssignPipeline(_) => "Create and assign pipeline",
            PaletteAction::CreatePipeline(_) => "Create pipeline",
            PaletteAction::AddEffect(_) => "Add video effect",
            PaletteAction::AddAudioEffect(_) | PaletteAction::AddGraphAudio { .. } => {
                "Add audio effect"
            }
            PaletteAction::AddGraphNode { .. } => "Add effect node",
            PaletteAction::AddGraphGenerator { .. } => "Add generator node",
            PaletteAction::AddGraphValue { .. } => "Add value node",
            PaletteAction::AddGraphPipeline { .. } => "Add pipeline node",
            PaletteAction::InsertEffectClip { .. }
            | PaletteAction::InsertEffectClipWithNewPipeline { .. } => "Insert effect clip",
            PaletteAction::SetFontFamily(_) => "Set font",
            _ => "Run command",
        };
        let records_history = !matches!(
            action,
            PaletteAction::NewProject
                | PaletteAction::NewComposition
                | PaletteAction::OpenProject
                | PaletteAction::OpenRecentProject(_)
                | PaletteAction::SaveProject
                | PaletteAction::SaveProjectAs
                | PaletteAction::ResetLayout
                | PaletteAction::AddPanel(_, _)
        );
        let before = records_history.then(|| {
            self.editor
                .history
                .capture(&self.editor.project, &self.editor.timeline)
        });
        self.editor.history_gesture = None;
        let mut graph_changed = false;
        match action {
            PaletteAction::NewProject => self.new_project(),
            PaletteAction::NewComposition => {
                self.open_modal(Modal::Composition(NewCompositionDialog::new(
                    NewCompositionMode::Blank,
                )));
            }
            PaletteAction::OpenProject => self.open_project_dialog(),
            PaletteAction::OpenRecentProject(path) => self.load_project(&path),
            PaletteAction::SaveProject => self.save_project(),
            PaletteAction::SaveProjectAs => self.save_project_as(),
            PaletteAction::ImportMedia => self.import_media_dialog(),
            PaletteAction::ImportClipboard => {
                if let Err(error) = self.import_clipboard_image() {
                    messages::warning("Clipboard import", format!("{error:#}"));
                }
            }
            PaletteAction::ResetLayout => self.dock = default_dock(),
            PaletteAction::AddPanel(panel, stack) => {
                if let Some(stack) = stack.or(self.dock.focused) {
                    self.dock.add_tab(stack, panel.layout_title());
                }
            }
            PaletteAction::InsertGenerator {
                choice,
                track,
                time,
            } => {
                let inserted = match choice {
                    GeneratorChoice::Plugin(key) => {
                        self.plugins.generator(&key).is_some_and(|definition| {
                            self.plugins
                                .visual_pipeline_instance()
                                .is_ok_and(|visual_pipeline| {
                                    self.editor.timeline.insert_plugin_generator_at(
                                        track,
                                        time,
                                        definition,
                                        visual_pipeline,
                                    )
                                })
                        })
                    }
                    GeneratorChoice::Wasm(media) => self
                        .editor
                        .project
                        .media(media)
                        .cloned()
                        .is_some_and(|asset| {
                            self.insert_media_asset_at(asset, MediaStream::All, track, time)
                        }),
                };
                if inserted {
                    self.media.clear_selection();
                }
            }
            PaletteAction::InsertMedia { media, track, time } => {
                let inserted = self
                    .editor
                    .project
                    .media(media)
                    .cloned()
                    .is_some_and(|asset| {
                        !matches!(asset.kind, MediaKind::Audio | MediaKind::WasmPlugin)
                            && self.insert_media_asset_at(asset, MediaStream::All, track, time)
                    });
                if inserted {
                    self.media.clear_selection();
                }
            }
            PaletteAction::InsertAudioMedia { media, track, time } => {
                let inserted = self
                    .editor
                    .project
                    .media(media)
                    .cloned()
                    .is_some_and(|asset| {
                        (matches!(asset.kind, MediaKind::Audio)
                            || matches!(asset.kind, MediaKind::Video) && asset.has_audio)
                            && self.insert_media_asset_at(asset, MediaStream::Audio(0), track, time)
                    });
                if inserted {
                    self.media.clear_selection();
                }
            }
            PaletteAction::ReplaceSelectedClips { media } => {
                let Some(asset) = self.editor.project.media(media) else {
                    return;
                };
                let video_name = asset.name.clone();
                let audio_name = if matches!(asset.kind, MediaKind::Video) {
                    format!("{} - Audio", asset.name)
                } else {
                    asset.name.clone()
                };
                let replaced_video =
                    self.editor
                        .timeline
                        .replace_selected_media_source(media, false, &video_name);
                let replaced_audio =
                    self.editor
                        .timeline
                        .replace_selected_media_source(media, true, &audio_name);
                if replaced_video + replaced_audio == 0 {
                    return;
                }
                self.audio.clear();
            }
            PaletteAction::AssignPipeline(pipeline) => {
                self.editor.timeline.set_selected_pipeline(pipeline)
            }
            PaletteAction::CreateAndAssignPipeline(kind) => {
                let pipeline = self.editor.project.create_pipeline_kind(kind);
                self.editor.timeline.set_selected_pipeline(Some(pipeline));
                graph_changed = true;
            }
            PaletteAction::CreatePipeline(kind) => {
                let pipeline = self.editor.project.create_pipeline_kind(kind);
                self.pipeline_graph.open_pipeline(pipeline);
                graph_changed = true;
            }
            PaletteAction::AddEffect(node_type) => {
                if let Some(pipeline) = self.selected_or_create_pipeline(PipelineKind::Video) {
                    graph_changed = self.plugins.effect(&node_type).is_some_and(|definition| {
                        self.editor
                            .project
                            .add_plugin_node(pipeline, definition)
                            .is_some()
                    });
                }
            }
            PaletteAction::AddAudioEffect(node_type) => {
                if let Some(pipeline) = self.selected_or_create_pipeline(PipelineKind::Audio) {
                    graph_changed =
                        self.plugins
                            .audio_effect(&node_type)
                            .is_some_and(|definition| {
                                self.editor
                                    .project
                                    .append_audio_node(pipeline, definition)
                                    .is_some()
                            });
                }
            }
            PaletteAction::AddGraphAudio {
                pipeline,
                node_type,
                position,
            } => {
                if self
                    .plugins
                    .audio_effect(&node_type)
                    .is_some_and(|definition| {
                        self.editor
                            .project
                            .append_audio_node_at(pipeline, definition, Some(position))
                            .is_some()
                    })
                {
                    graph_changed = true;
                }
            }
            PaletteAction::AddGraphNode {
                pipeline,
                node_type,
                position,
            } => {
                if self.plugins.effect(&node_type).is_some_and(|definition| {
                    self.editor
                        .project
                        .add_plugin_node_at(pipeline, definition, Some(position))
                        .is_some()
                }) {
                    graph_changed = true;
                }
            }
            PaletteAction::AddGraphGenerator {
                pipeline,
                generator_type,
                position,
            } => {
                if let Some(definition) = self.plugins.generator(&generator_type) {
                    if self
                        .editor
                        .project
                        .add_generator_node_at(pipeline, definition, Some(position))
                        .is_some()
                    {
                        graph_changed = true;
                    }
                }
            }
            PaletteAction::AddGraphValue {
                pipeline,
                kind,
                position,
            } => {
                if self
                    .editor
                    .project
                    .add_value_node_at(pipeline, kind, Some(position))
                    .is_some()
                {
                    graph_changed = true;
                }
            }
            PaletteAction::AddGraphPipeline { pipeline, position } => {
                if self
                    .editor
                    .project
                    .add_pipeline_node_at(pipeline, Some(position))
                    .is_some()
                {
                    graph_changed = true;
                }
            }
            PaletteAction::InsertEffectClip {
                track,
                time,
                pipeline,
            } => {
                if self
                    .editor
                    .timeline
                    .insert_effect_clip_at(track, time, pipeline)
                {
                    self.media.clear_selection();
                }
            }
            PaletteAction::InsertEffectClipWithNewPipeline { track, time } => {
                let pipeline = self.editor.project.create_pipeline();
                if self
                    .editor
                    .timeline
                    .insert_effect_clip_at(track, time, Some(pipeline))
                {
                    self.media.clear_selection();
                }
                graph_changed = true;
            }
            PaletteAction::SetFontFamily(family) => {
                self.editor.timeline.set_selected_font_family(family);
            }
        }
        if graph_changed {
            self.sync_effect_runtime();
        }
        self.playback.invalidate();
        if let Some(before) = before {
            self.editor.history.record_after(
                history_label,
                before,
                &self.editor.project,
                &self.editor.timeline,
                false,
            );
        }
    }
}

#[derive(Clone, Copy)]
struct StackBuildContext<'a> {
    snapshot: &'a LayoutSnapshot,
    cursor: [f32; 2],
    dragged_tab: Option<TabId>,
    maximized: bool,
    focused: bool,
    focus: f32,
    icons: Icons,
}

struct StackBuildState<'a> {
    project: &'a Project,
    plugins: &'a PluginRegistry,
    timeline: &'a TimelineState,
    media: &'a MediaPanelState,
    playback: &'a FrameRenderer,
    monitor: &'a MonitorState,
    history: &'a HistoryState,
    history_panel: &'a mut HistoryPanelState,
    inspector: &'a mut InspectorState,
    project_options: &'a mut ProjectOptionsState,
    pipeline_graph: &'a mut PipelineGraphState,
    render_panel: &'a mut RenderPanelState,
    widgets: &'a mut WidgetGallery,
    meters: &'a MetersState,
    messages: &'a MessagesState,
    waveform_textures: &'a waveform::WaveformTextures,
}

fn build_stack(
    ctx: &mut ui::BuildCtx,
    layout: &StackLayout,
    view: StackBuildContext<'_>,
    state: StackBuildState<'_>,
) {
    let StackBuildState {
        project,
        plugins,
        timeline,
        media,
        playback,
        monitor,
        history,
        history_panel,
        inspector,
        project_options,
        pipeline_graph,
        render_panel,
        widgets,
        meters,
        messages,
        waveform_textures,
    } = state;
    let StackBuildContext {
        snapshot,
        cursor,
        dragged_tab,
        maximized,
        focused,
        focus,
        icons,
    } = view;
    let stack = &layout.stack;
    let active_tab = stack.active_tab().map(|tab| tab.id);

    ui::ui!(ctx, {
        Block {
            id: @format("stack-border {}", stack.id.0);
            bounds: (
                layout.rect.x - 1.0,
                layout.rect.y - 1.0,
                layout.rect.width + 2.0,
                layout.rect.height + 2.0,
            );
            border: 1;
            border_color: theme::line();
        }

        Column {
            id: @format("stack {}", stack.id.0);
            bounds: (
                layout.rect.x,
                layout.rect.y,
                layout.rect.width,
                layout.rect.height,
            );
            fill: theme::panel();

            Block {
                id: @format("tab-bar {}", stack.id.0);
                fill: theme::tab_bar();
                width: Size::Fill;
                height: Size::Pixels(ui::dock::TAB_BAR_HEIGHT);

                @for hit in snapshot.tabs.iter().filter(|tab| tab.stack_id == stack.id) {
                    @rust {
                        let Some(tab) = stack.tabs.iter().find(|tab| tab.id == hit.tab_id) else {
                            continue;
                        };
                        let selected = active_tab == Some(tab.id);
                        let is_dragged = dragged_tab == Some(tab.id);
                        let hovered = !is_dragged && hit.rect.contains(cursor);
                        let leading_icon = if hovered {
                            AppIcon::Close
                        } else {
                            PanelKind::from_title(&tab.title)
                                .map_or(AppIcon::CommandPalette, |panel| panel.info().icon)
                        };
                        let leading_color = if hovered {
                            theme::accent_hover()
                        } else if selected {
                            theme::text()
                        } else {
                            theme::muted()
                        };
                        let x = hit.rect.x - layout.tab_bar.x;
                        let y = hit.rect.y - layout.tab_bar.y;
                    }

                    Block {
                        id: @format("tab {}", tab.id.0);
                        bounds: (x, y, hit.rect.width, hit.rect.height + RADIUS_SM);
                        fill: if selected { theme::tab_active() } else { TAB_IDLE };
                        border: 1;
                        border_color: if selected && focused {
                            theme::accent()
                        } else {
                            theme::line_soft()
                        };
                        border_radius: RADIUS_SM;
                        opacity: if is_dragged { 0.0 } else { hit.opacity };
                        interactive;
                    }

                    Row {
                        id: @format("tab-content {}", tab.id.0);
                        bounds: (x, y, hit.rect.width, hit.rect.height);
                        opacity: if is_dragged { 0.0 } else { hit.opacity };
                        padding: 2.0;
                        gap: 2.0;

                        Block {
                            width: Size::Pixels(18.0);
                            height: Size::Fill;
                            content_centered;

                            Icon {
                                id: @format("tab-leading {}", tab.id.0);
                                icon!: icons.get(leading_icon);
                                color!: leading_color;
                                width: Size::Pixels(TAB_ICON_SIZE);
                                height: Size::Pixels(TAB_ICON_SIZE);
                            }
                        }

                        Block {
                            id: @format("tab-title {}", tab.id.0);
                            width: Size::Fill;
                            height: Size::Fill;
                            font_size: 10.5;
                            text_color: if selected { theme::text() } else { theme::muted() };
                            text: PanelKind::from_title(&tab.title)
                                .map(|panel| panel.display_title())
                                .unwrap_or_else(|| tab.title.clone());
                        }
                    }
                }

                Block {
                    id: @format("plus {}", stack.id.0);
                    bounds: (
                        layout.plus_rect.x - layout.tab_bar.x,
                        layout.plus_rect.y - layout.tab_bar.y,
                        layout.plus_rect.width,
                        layout.plus_rect.height,
                    );
                    fill: theme::tab_active();
                    border: 1;
                    border_color: theme::line_soft();
                    border_radius: RADIUS_SM;
                    interactive;
                    tooltip: i18n::text("dock-add-panel");
                    content_centered;

                    Icon {
                        id: @format("plus-icon {}", stack.id.0);
                        icon!: icons.get(AppIcon::Plus);
                        color!: theme::muted();
                        width: Size::Pixels(18.0);
                        height: Size::Pixels(18.0);
                    }
                }

                @if layout.maximize_rect.width > 0.0 {
                    Block {
                        id: @format("maximize {}", stack.id.0);
                        bounds: (
                            layout.maximize_rect.x - layout.tab_bar.x,
                            layout.maximize_rect.y - layout.tab_bar.y,
                            layout.maximize_rect.width,
                            layout.maximize_rect.height,
                        );
                        fill: theme::tab_active();
                        border: 1;
                        border_color: theme::line_soft();
                        border_radius: RADIUS_SM;
                        interactive;
                        tooltip: if maximized {
                            i18n::text("dock-restore-panel")
                        } else {
                            i18n::text("dock-maximize-panel")
                        };
                        content_centered;

                        Icon {
                            id: @format("maximize-icon {}", stack.id.0);
                            icon!: icons.get(if maximized {
                                AppIcon::Restore
                            } else {
                                AppIcon::Maximize
                            });
                            color!: theme::muted();
                            width: Size::Pixels(16.0);
                            height: Size::Pixels(16.0);
                        }
                    }
                }
            }

            Block {
                id: @format("tab-separator {}", stack.id.0);
                width: Size::Fill;
                height: Size::Pixels(ui::dock::TAB_SEPARATOR_HEIGHT);
                fill: theme::line().mix(theme::accent(), focus);
            }

            Block {
                id: @format("panel-content {}", stack.id.0);
                fill: theme::panel();
                width: Size::Fill;
                height: Size::Fill;

                @rust {
                    if let Some(panel) = stack
                        .active_tab()
                        .and_then(|tab| PanelKind::from_title(&tab.title))
                    {
                        match panel {
                            PanelKind::Media => media.build(ctx, layout.content, project, icons),
                            PanelKind::Monitor => monitor.build(
                                ctx,
                                layout.content,
                                MonitorBuildContext {
                                    project,
                                    timeline,
                                    plugins,
                                    graph_selection: pipeline_graph.monitor_selection(timeline),
                                    output: playback.preview_output(),
                                    icons,
                                },
                            ),
                            PanelKind::Inspector => {
                                if let Some(composition) = media.selected_composition() {
                                    project_options.build(
                                        ctx,
                                        layout.content,
                                        project,
                                        composition,
                                        icons.get(AppIcon::Chevron),
                                    );
                                } else {
                                    inspector.build(
                                        ctx,
                                        layout.content,
                                        InspectorBuildContext {
                                            project,
                                            timeline,
                                            media_selection: media.selected_with_stream(),
                                            plugins,
                                            icons,
                                        },
                                    );
                                }
                            }
                            PanelKind::ProjectOptions => project_options.build(
                                ctx,
                                layout.content,
                                project,
                                project.active_composition,
                                icons.get(AppIcon::Chevron),
                            ),
                            PanelKind::History => history_panel.build(history, ctx, layout.content),
                            PanelKind::Pipeline => pipeline_graph.build(ctx, layout.content, project, timeline, plugins, icons),
                            PanelKind::Render => render_panel.build(ctx, layout.content, project, timeline, icons),
                            PanelKind::Timeline => {
                                timeline.build(
                                    ctx,
                                    stack.id,
                                    layout.content,
                                    icons,
                                    project,
                                    waveform_textures,
                                )
                            }
                            PanelKind::Widgets => {
                                widgets.build(ctx, stack.id, layout.content, icons)
                            }
                            PanelKind::Meters => meters.build(ctx, layout.content),
                            PanelKind::Messages => messages.build(ctx, layout.content),
                        }
                    }
                }
            }
        }
    });
}

#[allow(clippy::too_many_arguments)]
#[derive(Default)]
struct App {
    editor_app: Option<EditorApp>,
    focused_window: Option<WindowId>,
    dock_drag_owner: Option<WindowId>,
    dock_drag_target: Option<(WindowId, [f32; 2])>,
    dock_drag_outside_owner: bool,
    last_pointer_screen: Option<[f64; 2]>,
}

impl App {
    fn finish_dock_drag_release(&mut self, event_loop: &ActiveEventLoop) -> bool {
        let Some(editor_app) = self.editor_app.as_mut() else {
            self.dock_drag_owner = None;
            self.dock_drag_target = None;
            self.dock_drag_outside_owner = false;
            return false;
        };
        let source_id = editor_app.window.id();
        let screen = self
            .last_pointer_screen
            .or_else(|| editor_app.cursor_screen_physical());
        let target = self
            .dock_drag_target
            .take()
            .or_else(|| screen.and_then(|screen| editor_app.window_at_screen_point(screen)));
        if target.is_none()
            && !self.dock_drag_outside_owner
            && editor_app.cursor_inside_active_window()
        {
            editor_app.pointer_released(MouseButton::Left);
            self.dock_drag_owner = None;
            self.dock_drag_target = None;
            self.dock_drag_outside_owner = false;
            return true;
        }

        let Some(transfer) = editor_app.take_dock_transfer() else {
            editor_app.pointer_released(MouseButton::Left);
            self.dock_drag_owner = None;
            self.dock_drag_target = None;
            self.dock_drag_outside_owner = false;
            return false;
        };
        let source_empty = editor_app.window_dock_empty(source_id);

        let moved = if let Some((target_id, target_point)) = target {
            let fallback = transfer.clone();
            if editor_app.activate_window(target_id)
                && editor_app.drop_dock_transfer(transfer, target_point)
            {
                self.focused_window = Some(target_id);
                true
            } else {
                match editor_app.create_detached_window(event_loop, fallback.clone(), screen) {
                    Ok(new_id) => {
                        let _ = editor_app.activate_window(new_id);
                        self.focused_window = Some(new_id);
                        true
                    }
                    Err(error) => {
                        messages::error("Window", format!("could not detach pane: {error:#}"));
                        let restored = editor_app.restore_dock_transfer(source_id, fallback);
                        if restored {
                            self.focused_window = Some(source_id);
                        }
                        false
                    }
                }
            }
        } else {
            let restore = transfer.clone();
            match editor_app.create_detached_window(event_loop, transfer, screen) {
                Ok(new_id) => {
                    let _ = editor_app.activate_window(new_id);
                    self.focused_window = Some(new_id);
                    true
                }
                Err(error) => {
                    messages::error("Window", format!("could not detach pane: {error:#}"));
                    let restored = editor_app.restore_dock_transfer(source_id, restore);
                    if restored {
                        self.focused_window = Some(source_id);
                    }
                    false
                }
            }
        };
        if moved && source_empty {
            let _ = editor_app.close_window(source_id);
        }
        self.dock_drag_owner = None;
        self.dock_drag_target = None;
        self.dock_drag_outside_owner = false;
        editor_app.request_redraw_all();
        true
    }
}

impl ApplicationHandler<AppEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.editor_app.is_none() {
            match EditorApp::new(event_loop) {
                Ok(editor_app) => {
                    let window_id = editor_app.window.id();
                    editor_app.window.request_redraw();
                    self.focused_window = Some(window_id);
                    self.editor_app = Some(editor_app);
                }
                Err(_) => {
                    event_loop.exit();
                }
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(editor_app) = self.editor_app.as_ref() else {
            event_loop.set_control_flow(ControlFlow::Wait);
            return;
        };
        let deadline = editor_app.next_media_presence_check;
        if Instant::now() >= deadline {
            editor_app.request_redraw_all();
            event_loop.set_control_flow(ControlFlow::Wait);
        } else {
            event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let propagate_redraw = !matches!(&event, WindowEvent::RedrawRequested);
        if let WindowEvent::CursorMoved { position, .. } = &event {
            self.last_pointer_screen = self
                .editor_app
                .as_ref()
                .and_then(|app| app.screen_point_for_window(window_id, *position));
        }

        if self.dock_drag_owner == Some(window_id)
            && matches!(&event, WindowEvent::CursorMoved { .. })
        {
            self.dock_drag_target = None;
        }
        if self.dock_drag_owner == Some(window_id) {
            match &event {
                WindowEvent::CursorLeft { .. } => self.dock_drag_outside_owner = true,
                WindowEvent::CursorEntered { .. } => self.dock_drag_outside_owner = false,
                _ => {}
            }
        }

        if let Some(owner) = self.dock_drag_owner {
            if owner != window_id {
                if let WindowEvent::CursorMoved { position, .. } = &event {
                    self.dock_drag_target = self
                        .editor_app
                        .as_ref()
                        .and_then(|app| app.logical_point_for_window(window_id, *position))
                        .map(|point| (window_id, point));
                    if let (Some(editor_app), Some(screen)) =
                        (self.editor_app.as_mut(), self.last_pointer_screen)
                    {
                        if editor_app.activate_window(owner) {
                            editor_app.set_active_cursor_from_screen(screen);
                            editor_app.window.request_redraw();
                        }
                    }
                    if let Some(editor_app) = self.editor_app.as_ref() {
                        if let Some(state) = editor_app.secondary_windows.get(&window_id) {
                            state.window.request_redraw();
                        }
                    }
                    return;
                }
                if matches!(
                    &event,
                    WindowEvent::MouseInput {
                        state: ElementState::Released,
                        button: MouseButton::Left,
                        ..
                    }
                ) {
                    if let Some(editor_app) = self.editor_app.as_mut() {
                        if editor_app.activate_window(owner) {
                            if let Some(screen) = self.last_pointer_screen {
                                editor_app.set_active_cursor_from_screen(screen);
                            }
                        }
                    }
                    self.finish_dock_drag_release(event_loop);
                    if let Some(editor_app) = self.editor_app.as_mut() {
                        editor_app.drain_commands();
                        editor_app.sync_ime();
                    }
                    return;
                }
            }
        }

        if matches!(
            &event,
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            }
        ) && self.dock_drag_owner == Some(window_id)
        {
            if let Some(editor_app) = self.editor_app.as_mut() {
                let _ = editor_app.activate_window(window_id);
            }
            self.finish_dock_drag_release(event_loop);
            if let Some(editor_app) = self.editor_app.as_mut() {
                editor_app.drain_commands();
                if editor_app.take_exit_request() {
                    event_loop.exit();
                    return;
                }
                editor_app.sync_ime();
            }
            return;
        }

        let Some(editor_app) = self.editor_app.as_mut() else {
            return;
        };
        if !editor_app.activate_window(window_id) {
            return;
        }

        match event {
            WindowEvent::CloseRequested => {
                if editor_app.window_count() > 1 {
                    let _ = editor_app.close_window(window_id);
                    if self.focused_window == Some(window_id) {
                        self.focused_window = Some(editor_app.window.id());
                    }
                    if self.dock_drag_owner == Some(window_id) {
                        self.dock_drag_owner = None;
                        self.dock_drag_target = None;
                        self.dock_drag_outside_owner = false;
                    }
                    editor_app.request_redraw_all();
                    return;
                }
                editor_app.request_exit();
                editor_app.window.request_redraw();
            }
            WindowEvent::Resized(size) => {
                let scale_factor = editor_app.window.scale_factor();
                editor_app.renderer.resize(size, scale_factor);
                editor_app.window.request_redraw();
            }
            WindowEvent::ThemeChanged(system_theme) => {
                theme::set_system_appearance(matches!(system_theme, WindowTheme::Light));
                editor_app.window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                editor_app
                    .renderer
                    .resize(editor_app.window.inner_size(), scale_factor);
                let scale = editor_app.renderer.scale_factor() as f64;
                editor_app.cursor = [
                    (editor_app.cursor_physical[0] / scale) as f32,
                    (editor_app.cursor_physical[1] / scale) as f32,
                ];
                editor_app.input.cursor = editor_app.cursor;
                editor_app.window.request_redraw();
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                editor_app.modifiers = modifiers.state();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                editor_app.handle_key(&event);
                editor_app.window.request_redraw();
            }
            WindowEvent::Ime(event) => {
                editor_app.handle_ime(&event);
                editor_app.window.request_redraw();
            }
            WindowEvent::CursorMoved { position, .. } => {
                if editor_app.value_drag_cursor_locked {
                    editor_app.move_value_drag_cursor_warped(position);
                } else {
                    if !editor_app.external_drag_items.is_empty() {
                        editor_app.external_drag_uses_window_cursor = true;
                    }
                    editor_app.cursor_physical = [position.x, position.y];
                    let scale = editor_app.renderer.scale_factor() as f64;
                    editor_app.cursor = [(position.x / scale) as f32, (position.y / scale) as f32];
                    editor_app.input.cursor = editor_app.cursor;
                    editor_app.pointer_moved();
                    editor_app.sync_value_drag_cursor(event_loop);
                }
                editor_app.window.request_redraw();
            }
            WindowEvent::HoveredFile(path) => {
                event_loop.listen_device_events(DeviceEvents::Always);
                editor_app.hover_external_file(path);
                editor_app.window.request_redraw();
            }
            WindowEvent::HoveredFileCancelled => {
                editor_app.external_drag_items.clear();
                editor_app.external_drag_uses_window_cursor = false;
                event_loop.listen_device_events(DeviceEvents::WhenFocused);
                editor_app.window.request_redraw();
            }
            WindowEvent::DroppedFile(path) => {
                editor_app.drop_external_file(path);
                event_loop.listen_device_events(DeviceEvents::WhenFocused);
                editor_app.window.request_redraw();
            }
            WindowEvent::MouseWheel { delta, phase, .. } => {
                let trackpad = matches!(&delta, MouseScrollDelta::PixelDelta(_));
                let delta = match delta {
                    MouseScrollDelta::LineDelta(x, y) => [x * 40.0, y * 40.0],
                    MouseScrollDelta::PixelDelta(delta) => [delta.x as f32, delta.y as f32],
                };
                if trackpad && matches!(phase, TouchPhase::Ended | TouchPhase::Cancelled) {
                    editor_app.touch_gesture_cursor = None;
                }
                let active_trackpad_gesture =
                    trackpad && matches!(phase, TouchPhase::Started | TouchPhase::Moved);
                if editor_app.palette.kind.is_some() {
                    let _ = editor_app.scroll_palette(delta);
                    editor_app.window.request_redraw();
                    return;
                }
                if let Some(modal) = editor_app.modal.as_mut() {
                    if let Modal::Keybinds(dialog) = modal {
                        let width = editor_app.renderer.logical_width();
                        let height = editor_app.renderer.logical_height();
                        dialog.scroll(
                            width,
                            height,
                            editor_app.cursor,
                            delta,
                            &editor_app.command_registry,
                        );
                    }
                    editor_app.window.request_redraw();
                    return;
                }
                if editor_app.palette.kind.is_none() {
                    if editor_app.scroll_focused_popup(delta) {
                        editor_app.window.request_redraw();
                        return;
                    }
                    if let Some(stack) = editor_app
                        .snapshot
                        .stacks
                        .iter()
                        .rev()
                        .find(|stack| stack.tab_viewport.contains(editor_app.cursor))
                    {
                        let amount = if delta[0].abs() > delta[1].abs() {
                            -delta[0]
                        } else {
                            -delta[1]
                        };
                        editor_app.dock.scroll_tabs(stack.stack.id, amount);
                    } else {
                        match editor_app.focus_panel_at_cursor() {
                            Some((_, PanelKind::Timeline)) => {
                                editor_app.editor.timeline.scroll(
                                    &editor_app.snapshot,
                                    editor_app.cursor,
                                    delta,
                                    editor_app.modifiers,
                                );
                            }
                            Some((stack, PanelKind::Media)) => {
                                if let Some(content) = editor_app
                                    .snapshot
                                    .stack(stack)
                                    .map(|layout| layout.content)
                                {
                                    editor_app.media.scroll(
                                        content,
                                        editor_app.cursor,
                                        delta,
                                        &editor_app.editor.project,
                                    );
                                }
                            }
                            Some((stack, PanelKind::Widgets)) => {
                                if let Some(content) = editor_app
                                    .snapshot
                                    .stack(stack)
                                    .map(|layout| layout.content)
                                {
                                    editor_app.widgets.scroll(content, editor_app.cursor, delta);
                                }
                            }
                            Some((stack, PanelKind::Inspector)) => {
                                if let Some(content) = editor_app
                                    .snapshot
                                    .stack(stack)
                                    .map(|layout| layout.content)
                                {
                                    if editor_app.media.selected_composition().is_none() {
                                        editor_app.inspector.scroll(
                                            content,
                                            editor_app.cursor,
                                            delta,
                                        );
                                    }
                                }
                            }
                            Some((stack, PanelKind::Render)) => {
                                if let Some(content) = editor_app
                                    .snapshot
                                    .stack(stack)
                                    .map(|layout| layout.content)
                                {
                                    editor_app.render_panel.scroll(
                                        content,
                                        editor_app.cursor,
                                        delta,
                                    );
                                }
                            }
                            Some((stack, PanelKind::History)) => {
                                if let Some(content) = editor_app
                                    .snapshot
                                    .stack(stack)
                                    .map(|layout| layout.content)
                                {
                                    editor_app.history_panel.scroll(
                                        &editor_app.editor.history,
                                        content,
                                        editor_app.cursor,
                                        delta,
                                    );
                                }
                            }
                            Some((stack, PanelKind::Messages)) => {
                                if let Some(content) = editor_app
                                    .snapshot
                                    .stack(stack)
                                    .map(|layout| layout.content)
                                {
                                    editor_app
                                        .messages
                                        .scroll(content, editor_app.cursor, delta);
                                }
                            }
                            Some((stack, PanelKind::Monitor)) => {
                                if let Some(content) = editor_app
                                    .snapshot
                                    .stack(stack)
                                    .map(|layout| layout.content)
                                {
                                    let handled = editor_app.monitor.scroll(
                                        content,
                                        editor_app.cursor,
                                        delta,
                                        editor_app.modifiers,
                                        &editor_app.editor.project,
                                    );
                                    if handled && active_trackpad_gesture {
                                        editor_app.touch_gesture_cursor = if editor_app
                                            .modifiers
                                            .control_key()
                                            || editor_app.modifiers.super_key()
                                        {
                                            (delta[1] > 0.0).then_some(CursorIcon::ZoomIn).or_else(
                                                || (delta[1] < 0.0).then_some(CursorIcon::ZoomOut),
                                            )
                                        } else {
                                            Some(CursorIcon::Grabbing)
                                        };
                                    }
                                }
                            }
                            Some((stack, PanelKind::Pipeline)) => {
                                if let Some(content) = editor_app
                                    .snapshot
                                    .stack(stack)
                                    .map(|layout| layout.content)
                                {
                                    let handled = editor_app.pipeline_graph.scroll(
                                        content,
                                        editor_app.cursor,
                                        delta,
                                        editor_app.modifiers,
                                    );
                                    if handled && active_trackpad_gesture {
                                        editor_app.touch_gesture_cursor = if editor_app
                                            .modifiers
                                            .control_key()
                                            || editor_app.modifiers.super_key()
                                        {
                                            (delta[1] > 0.0).then_some(CursorIcon::ZoomIn).or_else(
                                                || (delta[1] < 0.0).then_some(CursorIcon::ZoomOut),
                                            )
                                        } else {
                                            Some(CursorIcon::Grabbing)
                                        };
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                editor_app.window.request_redraw();
            }
            WindowEvent::PinchGesture { delta, phase, .. } => {
                if matches!(phase, TouchPhase::Ended | TouchPhase::Cancelled) {
                    editor_app.touch_gesture_cursor = None;
                }
                let active_gesture = matches!(phase, TouchPhase::Started | TouchPhase::Moved);
                if editor_app.modal.is_none() && editor_app.palette.kind.is_none() {
                    match editor_app.focus_panel_at_cursor() {
                        Some((stack, PanelKind::Pipeline)) => {
                            if let Some(content) = editor_app
                                .snapshot
                                .stack(stack)
                                .map(|layout| layout.content)
                            {
                                let handled = editor_app.pipeline_graph.pinch_zoom(
                                    content,
                                    editor_app.cursor,
                                    delta,
                                );
                                if handled && active_gesture && delta.abs() > f64::EPSILON {
                                    editor_app.touch_gesture_cursor = Some(if delta > 0.0 {
                                        CursorIcon::ZoomIn
                                    } else {
                                        CursorIcon::ZoomOut
                                    });
                                }
                            }
                        }
                        Some((stack, PanelKind::Monitor)) => {
                            if let Some(content) = editor_app
                                .snapshot
                                .stack(stack)
                                .map(|layout| layout.content)
                            {
                                let handled = editor_app.monitor.pinch_zoom(
                                    content,
                                    editor_app.cursor,
                                    delta,
                                    &editor_app.editor.project,
                                );
                                if handled && active_gesture && delta.abs() > f64::EPSILON {
                                    editor_app.touch_gesture_cursor = Some(if delta > 0.0 {
                                        CursorIcon::ZoomIn
                                    } else {
                                        CursorIcon::ZoomOut
                                    });
                                }
                            }
                        }
                        _ => {}
                    }
                }
                editor_app.window.request_redraw();
            }
            WindowEvent::Focused(true) => {
                self.focused_window = Some(window_id);
                editor_app.window.request_redraw();
            }
            WindowEvent::Focused(false) => {
                if self.dock_drag_owner != Some(window_id) {
                    editor_app.release_value_drag_cursor(event_loop);
                    editor_app.drag = None;
                    editor_app.touch_gesture_cursor = None;
                    editor_app.monitor.pointer_middle_released();
                    editor_app.input.mouse_pressed = false;
                    editor_app.input.mouse_released = true;
                    editor_app.finish_history_gesture();
                }
                editor_app.window.request_redraw();
            }
            WindowEvent::MouseInput { state, button, .. } => {
                match (button, state) {
                    (MouseButton::Left, ElementState::Pressed) => {
                        editor_app.input.mouse_pressed = true;
                        editor_app.pointer_pressed();
                        if matches!(editor_app.drag, Some(DragState::Dock { .. })) {
                            self.dock_drag_owner = Some(window_id);
                            self.dock_drag_target = None;
                            self.dock_drag_outside_owner = false;
                        }
                        editor_app.sync_value_drag_cursor(event_loop);
                        if editor_app.take_exit_request() {
                            event_loop.exit();
                            return;
                        }
                    }
                    (MouseButton::Left, ElementState::Released) => {
                        editor_app.input.mouse_released = true;
                        editor_app.pointer_released(button);
                        editor_app.sync_value_drag_cursor(event_loop);
                    }
                    (MouseButton::Middle, ElementState::Pressed) if editor_app.modal.is_none() => {
                        editor_app.pointer_middle_pressed()
                    }
                    (MouseButton::Middle, ElementState::Released) => {
                        editor_app.pointer_released(button)
                    }
                    (MouseButton::Right, ElementState::Pressed)
                        if editor_app.modal.is_none() && editor_app.palette.kind.is_none() =>
                    {
                        let hovered_panel = editor_app.focus_panel_at_cursor();
                        if !matches!(hovered_panel, Some((_, PanelKind::Media))) {
                            editor_app.media.close_context_menu();
                        }
                        if !matches!(hovered_panel, Some((_, PanelKind::Inspector))) {
                            editor_app.inspector.close_context_menu();
                        }
                        match hovered_panel {
                            Some((_, PanelKind::Timeline)) => {
                                editor_app.editor.timeline.pointer_pressed(
                                    &editor_app.snapshot,
                                    editor_app.cursor,
                                    button,
                                    editor_app.modifiers,
                                );
                            }
                            Some((stack, PanelKind::Media)) => {
                                if let Some(content) = editor_app
                                    .snapshot
                                    .stack(stack)
                                    .map(|layout| layout.content)
                                {
                                    editor_app.media.pointer_right_pressed(
                                        content,
                                        editor_app.cursor,
                                        &editor_app.editor.project,
                                    );
                                }
                            }
                            Some((stack, PanelKind::Pipeline)) => {
                                if let Some(content) = editor_app
                                    .snapshot
                                    .stack(stack)
                                    .map(|layout| layout.content)
                                {
                                    let action = editor_app.pipeline_graph.pointer_right_pressed(
                                        content,
                                        editor_app.cursor,
                                        &editor_app.editor.project,
                                        &editor_app.editor.timeline,
                                        &editor_app.plugins,
                                    );
                                    if !matches!(action, PipelineGraphAction::None) {
                                        editor_app.handle_pipeline_graph_action(action);
                                    }
                                }
                            }
                            Some((stack, PanelKind::Inspector))
                                if editor_app.media.selected_composition().is_none() =>
                            {
                                if let Some(content) = editor_app
                                    .snapshot
                                    .stack(stack)
                                    .map(|layout| layout.content)
                                {
                                    editor_app.inspector.pointer_right_pressed(
                                        content,
                                        editor_app.cursor,
                                        &editor_app.editor.project,
                                        &editor_app.editor.timeline,
                                        &editor_app.plugins,
                                    );
                                }
                            }
                            _ => {}
                        }
                    }
                    (MouseButton::Right, ElementState::Released) => {
                        editor_app.pointer_released(button);
                    }
                    _ => {}
                }
                editor_app.window.request_redraw();
            }
            WindowEvent::RedrawRequested if editor_app.draw().is_err() => event_loop.exit(),
            WindowEvent::RedrawRequested => {}
            _ => {}
        }
        if propagate_redraw {
            editor_app.request_redraw_all();
        }
        editor_app.drain_commands();
        if editor_app.take_exit_request() {
            event_loop.exit();
            return;
        }
        editor_app.sync_ime();
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: winit::event::DeviceId,
        event: DeviceEvent,
    ) {
        let target = self.dock_drag_owner.or(self.focused_window);
        let Some(editor_app) = self.editor_app.as_mut() else {
            return;
        };
        if let Some(target) = target {
            let _ = editor_app.activate_window(target);
        }
        if let DeviceEvent::MouseMotion { delta } = event {
            if !editor_app.value_drag_cursor_locked {
                editor_app.move_external_drag_cursor_raw(delta);
            }
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: AppEvent) {
        if let (Some(editor_app), Some(window_id)) = (self.editor_app.as_mut(), self.focused_window)
        {
            let _ = editor_app.activate_window(window_id);
        }
        match event {
            AppEvent::Interrupt => {
                let Some(editor_app) = self.editor_app.as_mut() else {
                    event_loop.exit();
                    return;
                };
                editor_app.request_exit();
                editor_app.window.request_redraw();
            }
            #[cfg(target_os = "macos")]
            AppEvent::Menu(event) => {
                let Some(editor_app) = self.editor_app.as_mut() else {
                    return;
                };
                if editor_app.native_menu.about_requested(&event) {
                    editor_app.palette.close();
                    editor_app.modal = Some(Modal::About(SimpleDialog::new()));
                    editor_app.window.request_redraw();
                } else if editor_app.native_menu.settings_requested(&event) {
                    if let Some(command) =
                        editor_app.command_registry.command("application.settings")
                    {
                        editor_app.command_queue.push(command);
                    }
                    editor_app.window.request_redraw();
                } else if let Some(command_id) = editor_app.native_menu.view_command(&event) {
                    if let Some(command) = editor_app.command_registry.command(command_id) {
                        editor_app.command_queue.push(command);
                    }
                    editor_app.window.request_redraw();
                } else if let Some(command_id) = editor_app.native_menu.help_command(&event) {
                    if let Some(command) = editor_app.command_registry.command(command_id) {
                        editor_app.command_queue.push(command);
                    }
                    editor_app.window.request_redraw();
                } else if let Some(command) = editor_app.native_menu.file_command(&event) {
                    if editor_app.handle_file_command(command) {
                        event_loop.exit();
                        return;
                    }
                    editor_app.window.request_redraw();
                } else if let Some(command) = editor_app.native_menu.edit_command(&event) {
                    if let Some(command) = editor_app.command_registry.command(command) {
                        editor_app.command_queue.push(command);
                    }
                    editor_app.window.request_redraw();
                } else if let Some(command) = editor_app.native_menu.layout_command(&event) {
                    editor_app.handle_layout_command(command);
                    editor_app.window.request_redraw();
                }
            }
        }
        let Some(editor_app) = self.editor_app.as_mut() else {
            return;
        };
        editor_app.drain_commands();
        editor_app.request_redraw_all();
        if editor_app.take_exit_request() {
            event_loop.exit();
        }
    }
}

pub fn run() -> Result<()> {
    let mut builder = EventLoop::<AppEvent>::with_user_event();
    #[cfg(target_os = "macos")]
    builder.with_default_menu(false);
    let event_loop = builder.build()?;

    let interrupt_proxy = event_loop.create_proxy();
    ctrlc::set_handler(move || {
        let _ = interrupt_proxy.send_event(AppEvent::Interrupt);
    })
    .context("install Ctrl+C handler")?;

    #[cfg(target_os = "macos")]
    {
        let menu_proxy = event_loop.create_proxy();
        MenuEvent::set_event_handler(Some(move |event| {
            let _ = menu_proxy.send_event(AppEvent::Menu(event));
        }));
    }

    event_loop.set_control_flow(ControlFlow::Wait);
    event_loop.run_app(&mut App::default())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ime_sync_only_updates_changed_platform_state() {
        let mut state = ImeSyncState::default();
        let first = Rect::new(10.0, 20.0, 2.0, 14.0);
        let moved = Rect::new(24.0, 20.0, 2.0, 14.0);

        assert_eq!(state.update(None), ImeSyncUpdate::default());
        assert_eq!(
            state.update(Some(first)),
            ImeSyncUpdate {
                allowed: Some(true),
                area: Some(first),
            }
        );
        assert_eq!(state.update(Some(first)), ImeSyncUpdate::default());
        assert_eq!(
            state.update(Some(moved)),
            ImeSyncUpdate {
                allowed: None,
                area: Some(moved),
            }
        );
        assert_eq!(
            state.update(None),
            ImeSyncUpdate {
                allowed: Some(false),
                area: None,
            }
        );
        assert_eq!(state.update(None), ImeSyncUpdate::default());
    }
}
