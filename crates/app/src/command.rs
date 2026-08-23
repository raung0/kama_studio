use std::{
    collections::{HashMap, VecDeque},
    fmt::{self, Display},
};

use kama_ui::dock::{StackId, TabId};
use serde::{Deserialize, Serialize};
use winit::{
    event::{ElementState, KeyEvent},
    keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey},
};

use crate::{
    assets::AppIcon,
    effects::{PipelineKind, ValueNodeKind},
    history::HistorySnapshot,
    panels::{InspectorAction, PipelineGraphAction},
    timeline::TimelineAction,
    GeneratorChoice, LayoutCommand, PanelKind,
};

#[derive(Clone, Debug)]
pub(crate) enum PaletteAction {
    NewProject,
    NewComposition,
    OpenProject,
    OpenRecentProject(std::path::PathBuf),
    SaveProject,
    SaveProjectAs,
    ImportMedia,
    ImportClipboard,
    ResetLayout,
    AddPanel(PanelKind, Option<StackId>),
    InsertGenerator {
        choice: GeneratorChoice,
        track: u32,
        time: f32,
    },
    InsertMedia {
        media: u64,
        track: u32,
        time: f32,
    },
    InsertAudioMedia {
        media: u64,
        track: u32,
        time: f32,
    },
    ReplaceSelectedClips {
        media: u64,
    },
    AssignPipeline(Option<u64>),
    CreateAndAssignPipeline(PipelineKind),
    CreatePipeline(PipelineKind),
    AddEffect(String),
    AddAudioEffect(String),
    AddGraphAudio {
        pipeline: u64,
        node_type: String,
        position: [f32; 2],
    },
    AddGraphNode {
        pipeline: u64,
        node_type: String,
        position: [f32; 2],
    },
    AddGraphGenerator {
        pipeline: u64,
        generator_type: String,
        position: [f32; 2],
    },
    AddGraphValue {
        pipeline: u64,
        kind: ValueNodeKind,
        position: [f32; 2],
    },
    AddGraphPipeline {
        pipeline: u64,
        position: [f32; 2],
    },
    InsertEffectClip {
        track: u32,
        time: f32,
        pipeline: Option<u64>,
    },
    InsertEffectClipWithNewPipeline {
        track: u32,
        time: f32,
    },
    SetFontFamily(String),
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum DockCommand {
    ToggleMaximize(StackId),
    CloseTab { stack: StackId, tab: TabId },
    ActivateTab { stack: StackId, tab: TabId },
}

#[derive(Clone, Debug)]
pub(crate) enum EditCommand {
    Timeline(TimelineAction),
    Inspector(InspectorAction),
    PipelineGraph(PipelineGraphAction),
    Undo,
    Redo,
    RestoreHistory(HistorySnapshot),
}

#[derive(Clone, Debug)]
pub(crate) enum EditorCommand {
    Edit(EditCommand),
    Action(PaletteAction),
    Layout(LayoutCommand),
    Dock(DockCommand),
    OpenCommandPalette,
    ToggleCurrentPanelMaximize,
    TogglePenTool,
    OpenSettings,
    OpenKeybinds,
    OpenUrl(&'static str),
    Exit,
}

impl EditorCommand {
    pub(crate) fn timeline(action: TimelineAction) -> Self {
        Self::Edit(EditCommand::Timeline(action))
    }

    pub(crate) fn inspector(action: InspectorAction) -> Self {
        Self::Edit(EditCommand::Inspector(action))
    }

    pub(crate) fn pipeline_graph(action: PipelineGraphAction) -> Self {
        Self::Edit(EditCommand::PipelineGraph(action))
    }

    pub(crate) fn undo() -> Self {
        Self::Edit(EditCommand::Undo)
    }

    pub(crate) fn redo() -> Self {
        Self::Edit(EditCommand::Redo)
    }

    pub(crate) fn restore_history(snapshot: HistorySnapshot) -> Self {
        Self::Edit(EditCommand::RestoreHistory(snapshot))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum CommandScope {
    Global,
    Media,
}

#[derive(Default)]
pub(crate) struct CommandQueue {
    pending: VecDeque<EditorCommand>,
}

impl CommandQueue {
    pub(crate) fn push(&mut self, command: EditorCommand) {
        self.pending.push_back(command);
    }

    pub(crate) fn pop(&mut self) -> Option<EditorCommand> {
        self.pending.pop_front()
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
enum BindingKey {
    Character(char),
    Comma,
    Period,
    Delete,
    Backspace,
    Space,
    Enter,
    Tab,
    Escape,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    PageUp,
    PageDown,
    BracketLeft,
    BracketRight,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub(crate) struct KeyBinding {
    key: BindingKey,
    primary: bool,
    shift: bool,
    alt: bool,
}

impl KeyBinding {
    const fn new(key: BindingKey, primary: bool, shift: bool) -> Self {
        Self {
            key,
            primary,
            shift,
            alt: false,
        }
    }

    pub(crate) const fn primary(key: char) -> Self {
        Self::new(BindingKey::Character(key), true, false)
    }
    pub(crate) const fn primary_shift(key: char) -> Self {
        Self::new(BindingKey::Character(key), true, true)
    }
    pub(crate) const fn plain(key: char) -> Self {
        Self::new(BindingKey::Character(key), false, false)
    }
    pub(crate) const fn shifted(key: char) -> Self {
        Self::new(BindingKey::Character(key), false, true)
    }
    pub(crate) const fn comma() -> Self {
        Self::new(BindingKey::Comma, false, false)
    }
    pub(crate) const fn period() -> Self {
        Self::new(BindingKey::Period, false, false)
    }
    pub(crate) const fn delete() -> Self {
        Self::new(BindingKey::Delete, false, false)
    }
    pub(crate) const fn space() -> Self {
        Self::new(BindingKey::Space, false, false)
    }
    pub(crate) const fn arrow_left() -> Self {
        Self::new(BindingKey::ArrowLeft, false, false)
    }
    pub(crate) const fn arrow_right() -> Self {
        Self::new(BindingKey::ArrowRight, false, false)
    }
    pub(crate) const fn primary_shift_arrow_left() -> Self {
        Self::new(BindingKey::ArrowLeft, true, true)
    }
    pub(crate) const fn primary_shift_arrow_right() -> Self {
        Self::new(BindingKey::ArrowRight, true, true)
    }
    pub(crate) const fn bracket_left() -> Self {
        Self::new(BindingKey::BracketLeft, false, false)
    }
    pub(crate) const fn bracket_right() -> Self {
        Self::new(BindingKey::BracketRight, false, false)
    }

    pub(crate) fn from_event(event: &KeyEvent, modifiers: ModifiersState) -> Option<Self> {
        if event.state != ElementState::Pressed {
            return None;
        }
        let primary = if cfg!(target_os = "macos") {
            modifiers.super_key()
        } else {
            modifiers.control_key()
        };
        if (cfg!(target_os = "macos") && modifiers.control_key())
            || (!cfg!(target_os = "macos") && modifiers.super_key())
        {
            return None;
        }
        let key = match event.physical_key {
            PhysicalKey::Code(KeyCode::Comma) => BindingKey::Comma,
            PhysicalKey::Code(KeyCode::Period) => BindingKey::Period,
            PhysicalKey::Code(KeyCode::BracketLeft) => BindingKey::BracketLeft,
            PhysicalKey::Code(KeyCode::BracketRight) => BindingKey::BracketRight,
            _ => match &event.logical_key {
                Key::Character(text) => {
                    let text = text.as_str();
                    match text {
                        "," => BindingKey::Comma,
                        "." => BindingKey::Period,
                        "[" => BindingKey::BracketLeft,
                        "]" => BindingKey::BracketRight,
                        _ => {
                            let ch = text.chars().next()?;
                            if text.chars().count() != 1 {
                                return None;
                            }
                            BindingKey::Character(ch.to_ascii_lowercase())
                        }
                    }
                }
                Key::Named(NamedKey::Delete) => BindingKey::Delete,
                Key::Named(NamedKey::Backspace) => {
                    if cfg!(target_os = "macos") {
                        BindingKey::Delete
                    } else {
                        BindingKey::Backspace
                    }
                }
                Key::Named(NamedKey::Space) => BindingKey::Space,
                Key::Named(NamedKey::Enter) => BindingKey::Enter,
                Key::Named(NamedKey::Tab) => BindingKey::Tab,
                Key::Named(NamedKey::Escape) => BindingKey::Escape,
                Key::Named(NamedKey::ArrowUp) => BindingKey::ArrowUp,
                Key::Named(NamedKey::ArrowDown) => BindingKey::ArrowDown,
                Key::Named(NamedKey::ArrowLeft) => BindingKey::ArrowLeft,
                Key::Named(NamedKey::ArrowRight) => BindingKey::ArrowRight,
                Key::Named(NamedKey::Home) => BindingKey::Home,
                Key::Named(NamedKey::End) => BindingKey::End,
                Key::Named(NamedKey::PageUp) => BindingKey::PageUp,
                Key::Named(NamedKey::PageDown) => BindingKey::PageDown,
                _ => return None,
            },
        };
        Some(Self {
            key,
            primary,
            shift: modifiers.shift_key(),
            alt: modifiers.alt_key(),
        })
    }

    fn display_for(self, macos: bool) -> String {
        let key = match self.key {
            BindingKey::Character(key) => key.to_ascii_uppercase().to_string(),
            BindingKey::Comma => ",".into(),
            BindingKey::Period => ".".into(),
            BindingKey::Delete if macos => "⌫".into(),
            BindingKey::Delete => "Delete".into(),
            BindingKey::Backspace if macos => "⌫".into(),
            BindingKey::Backspace => "Backspace".into(),
            BindingKey::Space => "Space".into(),
            BindingKey::Enter => "Enter".into(),
            BindingKey::Tab => "Tab".into(),
            BindingKey::Escape => "Esc".into(),
            BindingKey::ArrowUp => "↑".into(),
            BindingKey::ArrowDown => "↓".into(),
            BindingKey::ArrowLeft => "←".into(),
            BindingKey::ArrowRight => "→".into(),
            BindingKey::Home => "Home".into(),
            BindingKey::End => "End".into(),
            BindingKey::PageUp => "PageUp".into(),
            BindingKey::PageDown => "PageDown".into(),
            BindingKey::BracketLeft => "[".into(),
            BindingKey::BracketRight => "]".into(),
        };
        if macos {
            let mut shortcut = String::new();
            if self.alt {
                shortcut.push('⌥');
            }
            if self.shift {
                shortcut.push('⇧');
            }
            if self.primary {
                shortcut.push('⌘');
            }
            shortcut.push_str(&key);
            shortcut
        } else {
            let mut parts = Vec::new();
            if self.primary {
                parts.push("Ctrl".to_string());
            }
            if self.alt {
                parts.push("Alt".to_string());
            }
            if self.shift {
                parts.push("Shift".to_string());
            }
            parts.push(key);
            parts.join("+")
        }
    }
}

impl Display for KeyBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.display_for(cfg!(target_os = "macos")))
    }
}

pub(crate) struct CommandRegistration {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) description: String,
    pub(crate) shortcut: Option<KeyBinding>,
    pub(crate) icon: AppIcon,
    pub(crate) command: EditorCommand,
    pub(crate) scope: CommandScope,
    pub(crate) palette_visible: bool,
}

#[derive(Clone)]
pub(crate) struct CommandDefinition {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) description: String,
    pub(crate) shortcut: Option<KeyBinding>,
    pub(crate) icon: Option<AppIcon>,
    pub(crate) command: EditorCommand,
    scope: CommandScope,
    palette_visible: bool,
}

pub(crate) struct CommandRegistry {
    definitions: Vec<CommandDefinition>,
    indices: HashMap<String, usize>,
    bindings: HashMap<(CommandScope, KeyBinding), usize>,
}

impl CommandRegistry {
    pub(crate) fn editor_defaults() -> Self {
        let mut registry = Self {
            definitions: Vec::new(),
            indices: HashMap::new(),
            bindings: HashMap::new(),
        };

        for (id, label, description, shortcut, icon, command) in [
            ("project.new", "New Project", "Create an empty Kama project", Some(KeyBinding::primary('n')), AppIcon::New, EditorCommand::Action(PaletteAction::NewProject)),
            ("composition.new", "New Composition", "Create a new composition", Some(KeyBinding::primary_shift('n')), AppIcon::Composition, EditorCommand::Action(PaletteAction::NewComposition)),
            ("project.open", "Open Project", "Open a .kama project", Some(KeyBinding::primary('o')), AppIcon::Open, EditorCommand::Action(PaletteAction::OpenProject)),
            ("project.save", "Save Project", "Save current .kama project", Some(KeyBinding::primary('s')), AppIcon::Save, EditorCommand::Action(PaletteAction::SaveProject)),
            ("project.save-as", "Save Project As", "Save to another .kama file", Some(KeyBinding::primary_shift('s')), AppIcon::Save, EditorCommand::Action(PaletteAction::SaveProjectAs)),
            ("media.import", "Import Media", "Add media or WASM generators", Some(KeyBinding::primary('i')), AppIcon::Import, EditorCommand::Action(PaletteAction::ImportMedia)),
            ("workspace.reset-layout", "Reset Panel Layout", "Restore default editor workspace", None, AppIcon::Settings, EditorCommand::Action(PaletteAction::ResetLayout)),
            ("workspace.toggle-current-panel-maximize", "Maximize / Restore Current Panel", "Toggle the focused panel between docked and maximized", Some(KeyBinding::primary_shift('d')), AppIcon::Maximize, EditorCommand::ToggleCurrentPanelMaximize),
            ("edit.copy", "Copy", "Copy selected timeline clips", Some(KeyBinding::primary('c')), AppIcon::Copy, EditorCommand::timeline(TimelineAction::CopySelection)),
            ("edit.cut", "Cut", "Cut selected timeline clips", Some(KeyBinding::primary('x')), AppIcon::Cut, EditorCommand::timeline(TimelineAction::CutSelection)),
            ("edit.paste", "Paste", "Paste clips at playhead", Some(KeyBinding::primary('v')), AppIcon::Paste, EditorCommand::timeline(TimelineAction::Paste)),
            ("timeline.power-duplicate", "Power Duplicate", "Duplicate selected clips and repeat the previous duplicate offset", Some(KeyBinding::primary('j')), AppIcon::Copy, EditorCommand::timeline(TimelineAction::PowerDuplicate)),
            ("timeline.select-before-playhead", "Select Clips Before Playhead", "Select all clips before the playhead on the current track", Some(KeyBinding::primary_shift_arrow_left()), AppIcon::Timeline, EditorCommand::timeline(TimelineAction::SelectBeforePlayhead)),
            ("timeline.select-after-playhead", "Select Clips After Playhead", "Select all clips after the playhead on the current track", Some(KeyBinding::primary_shift_arrow_right()), AppIcon::Timeline, EditorCommand::timeline(TimelineAction::SelectAfterPlayhead)),
            ("timeline.delete-selection", "Delete Selection", "Delete selected timeline clips", Some(KeyBinding::delete()), AppIcon::Delete, EditorCommand::timeline(TimelineAction::DeleteSelection)),
            ("timeline.group-selection", "Group Selection", "Group selected timeline clips", Some(KeyBinding::plain('g')), AppIcon::Group, EditorCommand::timeline(TimelineAction::GroupSelection)),
            ("timeline.ungroup-selection", "Ungroup Selection", "Remove selected clips from groups", Some(KeyBinding::shifted('g')), AppIcon::Ungroup, EditorCommand::timeline(TimelineAction::UngroupSelection)),
            ("timeline.close-gap", "Close Gap", "Close gaps between selected clips while preserving linked sync", None, AppIcon::CloseGap, EditorCommand::timeline(TimelineAction::CloseGap)),
            ("timeline.speed-duration", "Speed / Duration", "Change selected clip speed or set per-clip/total duration", None, AppIcon::SpeedDuration, EditorCommand::timeline(TimelineAction::SpeedDuration)),
            ("timeline.toggle-razor-tool", "Toggle Razor Tool", "Toggle the timeline razor tool", Some(KeyBinding::plain('k')), AppIcon::ClipCut, EditorCommand::timeline(TimelineAction::ToggleRazorTool)),
            ("monitor.toggle-pen-tool", "Toggle Pen Tool", "Toggle Monitor polygon point editing", Some(KeyBinding::plain('p')), AppIcon::Pen, EditorCommand::TogglePenTool),
            ("timeline.cut-at-playhead", "Cut at Playhead", "Split selected clips at the playhead, or all crossing clips when nothing is selected", Some(KeyBinding::primary('k')), AppIcon::ClipCut, EditorCommand::timeline(TimelineAction::CutAtPlayhead)),
            ("timeline.toggle-playback", "Play / Pause", "Toggle timeline playback", Some(KeyBinding::space()), AppIcon::Play, EditorCommand::timeline(TimelineAction::TogglePlayback)),
            ("edit.undo", "Undo", "Move backward in branching edit history", Some(KeyBinding::primary('z')), AppIcon::Undo, EditorCommand::undo()),
            ("edit.redo", "Redo", "Move forward on preferred history branch", Some(KeyBinding::primary_shift('z')), AppIcon::Redo, EditorCommand::redo()),
            ("application.command-palette", "Command Palette", "Open the command palette", Some(KeyBinding::primary('p')), AppIcon::Search, EditorCommand::OpenCommandPalette),
            ("application.settings", "Settings", "Open appearance and editor settings", None, AppIcon::Settings, EditorCommand::OpenSettings),
            ("application.keybinds", "Keybinds", "Configure command keyboard shortcuts", None, AppIcon::Keybinds, EditorCommand::OpenKeybinds),
            ("application.exit", "Exit Kama", "Close editor", Some(KeyBinding::primary('q')), AppIcon::Exit, EditorCommand::Exit),
        ] {
            registry.register_editor_command(id, label, description, shortcut, icon, command);
        }
        registry.register_editor_command_without_icon(
            "application.report-issue",
            "Report an issue / give feedback",
            "Open GitHub issue form",
            EditorCommand::OpenUrl("https://github.com/raung0/kama_studio/issues/new"),
        );
        registry.register_editor_command_without_icon(
            "application.get-help",
            "Get help",
            "Open GitHub Q&A discussions",
            EditorCommand::OpenUrl(
                "https://github.com/raung0/kama_studio/discussions/categories/q-a",
            ),
        );

        registry.register_scoped_editor_command(CommandRegistration {
            id: "media.import-clipboard".into(),
            label: "Import from Clipboard".into(),
            description: "Import an image from the system clipboard into Media".into(),
            shortcut: Some(KeyBinding::primary('v')),
            icon: AppIcon::Paste,
            command: EditorCommand::Action(PaletteAction::ImportClipboard),
            scope: CommandScope::Media,
            palette_visible: true,
        });

        registry.register_hidden_editor_command(
            "workspace.save-layout",
            "Save Layout",
            "Open layout save dialog",
            AppIcon::Save,
            EditorCommand::Layout(LayoutCommand::Save),
        );
        for (id, label, icon, command) in [
            (
                "timeline.toggle-frame-snap",
                "Toggle Frame Snap",
                AppIcon::SnapFrame,
                TimelineAction::ToggleFrameSnap,
            ),
            (
                "timeline.toggle-grid-snap",
                "Toggle Grid Snap",
                AppIcon::SnapGrid,
                TimelineAction::ToggleGridSnap,
            ),
            (
                "timeline.toggle-clip-snap",
                "Toggle Clip Snap",
                AppIcon::SnapClips,
                TimelineAction::ToggleClipSnap,
            ),
            (
                "timeline.toggle-playhead-snap",
                "Toggle Playhead Snap",
                AppIcon::SnapPlayhead,
                TimelineAction::TogglePlayheadSnap,
            ),
            (
                "timeline.toggle-follow-playhead",
                "Toggle Follow Playhead",
                AppIcon::FollowPlayhead,
                TimelineAction::ToggleFollowPlayhead,
            ),
            (
                "timeline.seek-back-five",
                "Seek Back Five Seconds",
                AppIcon::Timeline,
                TimelineAction::SeekBy(-5.0),
            ),
            (
                "timeline.seek-forward-five",
                "Seek Forward Five Seconds",
                AppIcon::Timeline,
                TimelineAction::SeekBy(5.0),
            ),
            (
                "timeline.toggle-end-behavior",
                "Toggle End Behavior",
                AppIcon::SkipEnd,
                TimelineAction::ToggleEndBehavior,
            ),
            (
                "timeline.jump-start",
                "Jump to Timeline Start",
                AppIcon::SkipStart,
                TimelineAction::JumpTimelineStart,
            ),
            (
                "timeline.jump-end",
                "Jump to Timeline End",
                AppIcon::SkipEnd,
                TimelineAction::JumpTimelineEnd,
            ),
            (
                "timeline.jump-content-start",
                "Jump to Content Start",
                AppIcon::SkipStart,
                TimelineAction::JumpContentStart,
            ),
            (
                "timeline.jump-content-end",
                "Jump to Content End",
                AppIcon::SkipEnd,
                TimelineAction::JumpContentEnd,
            ),
            (
                "timeline.step-back",
                "Step Back One Frame",
                AppIcon::Timeline,
                TimelineAction::StepFrames(-1),
            ),
            (
                "timeline.step-forward",
                "Step Forward One Frame",
                AppIcon::Timeline,
                TimelineAction::StepFrames(1),
            ),
            (
                "timeline.set-end",
                "Set Timeline End",
                AppIcon::SkipEnd,
                TimelineAction::SetEnd,
            ),
        ] {
            registry.register_hidden_editor_command(
                id,
                label,
                "Context-bound timeline control",
                icon,
                EditorCommand::timeline(command),
            );
        }
        for panel in PanelKind::ALL {
            let info = panel.info();
            registry.register_hidden_editor_command(
                format!(
                    "panel.open.{}",
                    info.title.to_ascii_lowercase().replace(' ', "-")
                ),
                format!("Open {}", info.title),
                info.description,
                info.icon,
                EditorCommand::Action(PaletteAction::AddPanel(panel, None)),
            );
        }

        registry.set_shortcut("timeline.seek-back-five", Some(KeyBinding::arrow_left()));
        registry.set_shortcut(
            "timeline.seek-forward-five",
            Some(KeyBinding::arrow_right()),
        );
        registry.set_shortcut(
            "timeline.jump-content-start",
            Some(KeyBinding::bracket_left()),
        );
        registry.set_shortcut(
            "timeline.jump-content-end",
            Some(KeyBinding::bracket_right()),
        );
        registry.set_shortcut(
            "timeline.jump-start",
            Some(KeyBinding {
                shift: true,
                ..KeyBinding::bracket_left()
            }),
        );
        registry.set_shortcut(
            "timeline.jump-end",
            Some(KeyBinding {
                shift: true,
                ..KeyBinding::bracket_right()
            }),
        );
        registry.set_shortcut("timeline.step-back", Some(KeyBinding::comma()));
        registry.set_shortcut("timeline.step-forward", Some(KeyBinding::period()));
        registry
    }

    fn register_editor_command(
        &mut self,
        id: impl Into<String>,
        label: impl Into<String>,
        description: impl Into<String>,
        shortcut: Option<KeyBinding>,
        icon: AppIcon,
        command: EditorCommand,
    ) {
        self.insert(CommandDefinition {
            id: id.into(),
            label: label.into(),
            description: description.into(),
            shortcut,
            icon: Some(icon),
            command,
            scope: CommandScope::Global,
            palette_visible: true,
        });
    }

    fn register_editor_command_without_icon(
        &mut self,
        id: impl Into<String>,
        label: impl Into<String>,
        description: impl Into<String>,
        command: EditorCommand,
    ) {
        self.insert(CommandDefinition {
            id: id.into(),
            label: label.into(),
            description: description.into(),
            shortcut: None,
            icon: None,
            command,
            scope: CommandScope::Global,
            palette_visible: true,
        });
    }

    fn register_scoped_editor_command(&mut self, registration: CommandRegistration) {
        self.insert(CommandDefinition {
            id: registration.id,
            label: registration.label,
            description: registration.description,
            shortcut: registration.shortcut,
            icon: Some(registration.icon),
            command: registration.command,
            scope: registration.scope,
            palette_visible: registration.palette_visible,
        });
    }

    fn register_hidden_editor_command(
        &mut self,
        id: impl Into<String>,
        label: impl Into<String>,
        description: impl Into<String>,
        icon: AppIcon,
        command: EditorCommand,
    ) {
        self.insert(CommandDefinition {
            id: id.into(),
            label: label.into(),
            description: description.into(),
            shortcut: None,
            icon: Some(icon),
            command,
            scope: CommandScope::Global,
            palette_visible: false,
        });
    }

    fn insert(&mut self, mut definition: CommandDefinition) {
        let id = definition.id.clone();
        let shortcut = definition.shortcut.take();
        let index = if let Some(index) = self.indices.get(&id).copied() {
            if let Some(old) = self.definitions[index].shortcut {
                self.bindings.remove(&(self.definitions[index].scope, old));
            }
            self.definitions[index] = definition;
            index
        } else {
            let index = self.definitions.len();
            self.indices.insert(id, index);
            self.definitions.push(definition);
            index
        };
        self.set_shortcut_at(index, shortcut);
    }

    fn set_shortcut_at(&mut self, index: usize, shortcut: Option<KeyBinding>) {
        if let Some(old) = self.definitions[index].shortcut.take() {
            self.bindings.remove(&(self.definitions[index].scope, old));
        }
        if let Some(binding) = shortcut {
            let key = (self.definitions[index].scope, binding);
            if let Some(previous) = self.bindings.insert(key, index) {
                self.definitions[previous].shortcut = None;
            }
        }
        self.definitions[index].shortcut = shortcut;
    }

    pub(crate) fn definitions(&self) -> &[CommandDefinition] {
        &self.definitions
    }

    pub(crate) fn definition(&self, id: &str) -> Option<&CommandDefinition> {
        self.indices
            .get(id)
            .and_then(|index| self.definitions.get(*index))
    }

    pub(crate) fn set_shortcut(&mut self, id: &str, shortcut: Option<KeyBinding>) -> bool {
        let Some(index) = self.indices.get(id).copied() else {
            return false;
        };
        self.set_shortcut_at(index, shortcut);
        true
    }

    pub(crate) fn palette_definitions(&self) -> impl Iterator<Item = &CommandDefinition> {
        self.definitions
            .iter()
            .filter(|definition| definition.palette_visible)
    }

    pub(crate) fn command(&self, id: &str) -> Option<EditorCommand> {
        self.indices
            .get(id)
            .and_then(|index| self.definitions.get(*index))
            .map(|definition| definition.command.clone())
    }

    pub(crate) fn command_for_key(
        &self,
        event: &KeyEvent,
        modifiers: ModifiersState,
        scope: CommandScope,
    ) -> Option<EditorCommand> {
        let binding = KeyBinding::from_event(event, modifiers)?;
        self.bindings
            .get(&(scope, binding))
            .or_else(|| self.bindings.get(&(CommandScope::Global, binding)))
            .and_then(|index| self.definitions.get(*index))
            .map(|definition| definition.command.clone())
    }
}

pub(crate) fn fuzzy_score(query: &str, candidate: &str) -> Option<i32> {
    let query = query.to_lowercase();
    let candidate = candidate.to_lowercase();
    let chars = candidate.chars().collect::<Vec<_>>();
    let mut cursor = 0;
    let mut score = 0;
    let mut previous_match = None;
    for needle in query.chars().filter(|character| !character.is_whitespace()) {
        let index = chars
            .iter()
            .enumerate()
            .skip(cursor)
            .find_map(|(index, &character)| (character == needle).then_some(index))?;
        score += 10;
        if index == 0 || !chars[index - 1].is_alphanumeric() {
            score += 12;
        }
        if previous_match.is_some_and(|previous| previous + 1 == index) {
            score += 18;
        }
        score -= (index.saturating_sub(cursor) as i32).min(8);
        previous_match = Some(index);
        cursor = index + 1;
    }
    Some(score - candidate.len() as i32 / 8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_binding_display_is_platform_specific() {
        let save_as = KeyBinding::primary_shift('s');
        assert_eq!(save_as.display_for(true), "⇧⌘S");
        assert_eq!(save_as.display_for(false), "Ctrl+Shift+S");
        assert_eq!(KeyBinding::delete().display_for(true), "⌫");
        assert_eq!(KeyBinding::delete().display_for(false), "Delete");
    }

    #[test]
    fn queue_is_fifo() {
        let mut queue = CommandQueue::default();
        queue.push(EditorCommand::undo());
        queue.push(EditorCommand::redo());
        assert!(matches!(
            queue.pop(),
            Some(EditorCommand::Edit(EditCommand::Undo))
        ));
        assert!(matches!(
            queue.pop(),
            Some(EditorCommand::Edit(EditCommand::Redo))
        ));
        assert!(queue.pop().is_none());
    }

    #[test]
    fn registry_ids_are_unique_and_resolvable() {
        let registry = CommandRegistry::editor_defaults();
        let ids = registry
            .definitions
            .iter()
            .map(|definition| definition.id.as_str())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(ids.len(), registry.definitions.len());
        assert!(matches!(
            registry.command("edit.undo"),
            Some(EditorCommand::Edit(EditCommand::Undo))
        ));
        for id in [
            "edit.copy",
            "edit.cut",
            "edit.paste",
            "timeline.power-duplicate",
            "timeline.select-before-playhead",
            "timeline.select-after-playhead",
            "timeline.close-gap",
            "timeline.speed-duration",
            "timeline.toggle-razor-tool",
            "timeline.cut-at-playhead",
            "media.import",
            "media.import-clipboard",
            "project.save-as",
            "workspace.toggle-current-panel-maximize",
        ] {
            assert!(registry.command(id).is_some(), "missing command {id}");
        }
        let paste = KeyBinding::primary('v');
        let global = registry
            .bindings
            .get(&(CommandScope::Global, paste))
            .copied()
            .unwrap();
        let media = registry
            .bindings
            .get(&(CommandScope::Media, paste))
            .copied()
            .unwrap();
        assert_eq!(registry.definitions[global].id, "edit.paste");
        assert_eq!(registry.definitions[media].id, "media.import-clipboard");

        assert!(registry.command("panel.open.timeline").is_some());
        assert!(registry
            .palette_definitions()
            .all(|definition| !definition.id.starts_with("panel.open.")));
        assert!(registry.command("missing").is_none());
    }
}
