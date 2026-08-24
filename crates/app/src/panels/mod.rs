use std::{
    collections::{BTreeMap, HashMap, HashSet},
    hash::Hash,
};

use anyhow::Result;
use kama_ui::{
    components::{
        Accordion, Button, ColorButton, ColorPicker, ComboBox, EditResponse, Knob, NumberInput,
        SpinInput, Style, TextEdit, ToggleButton, VerticalSlider,
    },
    control_registry::{
        ComboControl, ControlRegistry, EnumControlsExt, KnobControlsExt, NumberControlsExt,
        SliderControlsExt,
    },
    BlockId, Color, CursorShape, FormatKey, IconId, Rect, Renderer, Size,
};
use winit::{
    event::{Ime, KeyEvent},
    keyboard::{Key, ModifiersState, NamedKey},
};

use crate::{
    assets::{AppIcon, Icons},
    effects::{GpuValue, PipelineKind},
    gradient::{colors_from_values, colors_to_values, default_color},
    plugin::{InputType, PluginInput, PluginRegistry},
    project::{
        AlphaBlendMode, BlendMode, CompositionId, GeneratorSource, HostValue, MediaId, MediaKind,
        Project, ProjectBackground, MAX_CANVAS_DIMENSION, MAX_FRAME_RATE,
    },
    theme,
    timeline::{format_timecode, parse_timecode, TimelineState, TrackKind},
    widgets::{build_context_menu, context_menu_hit, context_menu_rect, ContextMenuItem},
    RADIUS_SM,
};

const ROW_H: f32 = 29.0;
mod panels_graph;
mod panels_graph_cards;
mod panels_graph_state;
mod panels_graphic_eq;
mod panels_media;
mod panels_media_inspector;
mod panels_project;
mod panels_shared;
use panels_graph::*;
use panels_graph_cards::*;
#[cfg(test)]
use panels_graph_state::graph_stable_fallback;
use panels_graph_state::{
    graph_host_row_height, graph_property_row_height, set_eq_band, GraphCard, GRAPH_CARD_BASE_H,
    GRAPH_CARD_W, GRAPH_IMAGE_INPUT_H, GRAPH_INPUT_H, GRAPH_TOOLBAR_H,
};
pub(crate) use panels_graph_state::{
    GraphMonitorSelection, GraphNodeTarget, GraphWire, PipelineGraphAction, PipelineGraphState,
};
use panels_graphic_eq::*;
pub(crate) use panels_media::{MediaAction, MediaDragItem, MediaPanelState, MediaStream};
use panels_media_inspector::*;
pub(crate) use panels_project::ProjectOptionsState;
use panels_shared::*;
const ACCORDION_H: f32 = 32.0;
const PANEL_HEADER_H: f32 = 32.0;
const PANEL_GAP: f32 = 7.0;

type NumberSettings<'a> = ((f64, f64), f64, usize, &'a str);

#[derive(Clone, Copy)]
struct KeyframeControl {
    icon: IconId,
    keyed: bool,
}

fn keyframe_control(icons: Icons, keyed: bool, animated: bool) -> KeyframeControl {
    let icon = if keyed {
        AppIcon::KeyframeSet
    } else if animated {
        AppIcon::KeyframeUnsetInAnimation
    } else {
        AppIcon::KeyframeUnset
    };
    KeyframeControl {
        icon: icons.get(icon),
        keyed,
    }
}

fn icon_button(
    ctx: &mut kama_ui::BuildCtx,
    id: &str,
    rect: Rect,
    icon: IconId,
    tooltip: &str,
    style: Style,
) {
    icon_button_with_cursor(ctx, id, rect, icon, tooltip, style, CursorShape::Pointer);
}

fn icon_button_with_cursor(
    ctx: &mut kama_ui::BuildCtx,
    id: &str,
    rect: Rect,
    icon: IconId,
    tooltip: &str,
    style: Style,
    cursor: CursorShape,
) {
    Button::build(ctx, id, rect, "", style);
    kama_ui::ui!(ctx, {
        Block {
            id: @format("{}-icon", id);
            bounds: (rect.x, rect.y, rect.width, rect.height);
            content_centered;

            Icon {
                id: @format("{}-glyph", id);
                icon!: icon;
                color!: theme::text();
                width: Size::Pixels(15.0);
                height: Size::Pixels(15.0);
            }
        }
        Rect(("icon-button-tooltip", id), rect) {
            interactive;
            cursor: cursor;
            tooltip: tooltip;
        }
    });
}

fn toggle_icon_button(
    ctx: &mut kama_ui::BuildCtx,
    id: &str,
    rect: Rect,
    icon: IconId,
    active: bool,
    tooltip: &str,
    style: Style,
) {
    ToggleButton::build(ctx, id, rect, "", active, style);
    kama_ui::ui!(ctx, {
        Block {
            id: @format("{}-icon", id);
            bounds: (rect.x, rect.y, rect.width, rect.height);
            content_centered;

            Icon {
                id: @format("{}-glyph", id);
                icon!: icon;
                color!: theme::toggle_icon_color(active);
                width: Size::Pixels(15.0);
                height: Size::Pixels(15.0);
            }
        }
        Rect(("toggle-icon-button-tooltip", id), rect) {
            interactive;
            tooltip: tooltip;
        }
    });
}

fn graph_component_style(zoom: f32) -> Style {
    crate::widgets::component_style().with_scale(zoom)
}

fn graph_port<K: Hash>(ctx: &mut kama_ui::BuildCtx, key: K, rect: Rect, color: Color, radius: f32) {
    kama_ui::ui!(ctx, {
        Rect(key, rect) {
            fill: color;
            border_radius: radius;
        }
    });
}

fn graph_label<K: Hash>(
    ctx: &mut kama_ui::BuildCtx,
    key: K,
    rect: Rect,
    scale: f32,
    text: impl Into<String>,
) {
    kama_ui::ui!(ctx, {
        Rect(key, rect) {
            font_size: 7.6 * scale;
            text_color: theme::muted();
            text: text.into();
        }
    });
}

fn graph_output_label<K: Hash>(
    ctx: &mut kama_ui::BuildCtx,
    key: K,
    rect: Rect,
    scale: f32,
    text: impl Into<String>,
) {
    kama_ui::ui!(ctx, {
        Rect(key, rect) {
            font_size: 7.6 * scale;
            text_color: theme::muted();
            text_align: kama_ui::Align::End;
            text: text.into();
        }
    });
}

type NumberControls<K> = ControlRegistry<K, NumberInput>;
type KnobControls<K> = ControlRegistry<K, Knob>;
type SliderControls<K> = ControlRegistry<K, VerticalSlider>;
type EnumControls<K> = ControlRegistry<K, ComboControl>;

struct AngleControls<K> {
    knob: KnobControls<K>,
    numbers: [NumberControls<K>; 2],
    round_turns: bool,
}

impl<K> Default for AngleControls<K> {
    fn default() -> Self {
        Self {
            knob: Default::default(),
            numbers: [Default::default(), Default::default()],
            round_turns: false,
        }
    }
}

impl<K: Clone + Eq + Hash> AngleControls<K> {
    fn clear_layout(&mut self) {
        self.knob.clear_layout();
        self.numbers
            .iter_mut()
            .for_each(|controls| controls.clear_layout());
    }

    fn build(
        &mut self,
        ctx: &mut kama_ui::BuildCtx,
        target: K,
        id: &str,
        parts: (Rect, Rect, Rect),
        angle: (f32, f64, bool),
        style: Style,
    ) {
        let (angle, turn_limit, round_turns) = angle;
        let (turns, degrees) = split_angle(angle);
        self.round_turns = round_turns;
        for (index, (rect, value, settings, suffix)) in [
            (
                parts.0,
                turns as f64,
                ((-turn_limit, turn_limit), 1.0, 0usize, "x"),
                "turns",
            ),
            (
                parts.1,
                degrees as f64,
                ((-359.9, 359.9), 0.5, 1usize, "°"),
                "degrees",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            self.numbers[index].build(
                ctx,
                format!("{id}-{suffix}"),
                target.clone(),
                (rect, value, settings),
                style,
            );
        }
        self.knob.build(
            ctx,
            format!("{id}-knob"),
            target,
            (parts.2, angle as f64),
            || angle_knob(angle),
            style,
        );
    }

    fn tick(&mut self, dt: f32) {
        self.knob.tick(dt);
        self.numbers
            .iter_mut()
            .for_each(|controls| controls.tick(dt));
    }

    fn is_animating(&self) -> bool {
        self.knob.is_animating() || self.numbers.iter().any(|controls| controls.is_animating())
    }

    fn is_dragging(&self) -> bool {
        self.knob.is_dragging() || self.numbers.iter().any(|controls| controls.is_dragging())
    }

    fn is_number_dragging(&self) -> bool {
        self.numbers.iter().any(|controls| controls.is_dragging())
    }

    fn target_at(&self, panel: Rect, point: [f32; 2]) -> Option<K> {
        self.knob.target_at(panel, point).or_else(|| {
            self.numbers
                .iter()
                .find_map(|controls| controls.target_at(panel, point))
        })
    }

    fn blur(&mut self) {
        self.numbers.iter_mut().for_each(|controls| controls.blur());
    }

    fn pointer_released(&mut self) -> bool {
        self.numbers
            .iter_mut()
            .fold(self.knob.pointer_released(), |handled, controls| {
                controls.pointer_released() | handled
            })
    }

    fn update_part(&self, target: &K, part: usize, value: f64) -> f32 {
        let turns = self.numbers[0].get(target).map_or(0.0, NumberInput::value);
        let degrees = self.numbers[1].get(target).map_or(0.0, NumberInput::value);
        if part == 0 {
            let turns = if self.round_turns {
                value.round()
            } else {
                value
            };
            turns as f32 * 360.0 + degrees as f32
        } else {
            turns as f32 * 360.0 + value as f32
        }
    }

    fn pointer_pressed(
        &mut self,
        panel: Rect,
        point: [f32; 2],
        modifiers: ModifiersState,
    ) -> Option<(K, Option<f32>)> {
        if let Some((target, value)) = self.knob.pointer_pressed(panel, point) {
            self.numbers.iter_mut().for_each(|controls| controls.blur());
            return Some((target, value.map(|value| value as f32)));
        }
        let (part, target, value) =
            self.numbers
                .iter_mut()
                .enumerate()
                .find_map(|(part, controls)| {
                    controls
                        .pointer_pressed(panel, point, modifiers)
                        .map(|(target, value)| (part, target, value))
                })?;
        for (index, controls) in self.numbers.iter_mut().enumerate() {
            if index != part {
                controls.blur();
            }
        }
        let value = value.map(|value| self.update_part(&target, part, value));
        Some((target, value))
    }

    fn pointer_moved(&mut self, point: [f32; 2]) -> Option<(K, f32)> {
        if let Some((target, value)) = self.knob.pointer_moved(point) {
            return Some((target, value as f32));
        }
        let (part, target, value) =
            self.numbers
                .iter_mut()
                .enumerate()
                .find_map(|(part, controls)| {
                    controls
                        .pointer_moved(point)
                        .map(|(target, value)| (part, target, value))
                })?;
        Some((target.clone(), self.update_part(&target, part, value)))
    }

    fn edit(
        &mut self,
        mut edit: impl FnMut(&mut NumberInput) -> Option<f64>,
    ) -> Option<(K, Option<f32>)> {
        let (part, target) = self
            .numbers
            .iter()
            .enumerate()
            .find_map(|(part, controls)| controls.editing_target().map(|target| (part, target)))?;
        let value = self.numbers[part].edit(&target, &mut edit);
        Some((
            target.clone(),
            value.map(|value| self.update_part(&target, part, value)),
        ))
    }

    fn caret_rect(&self, panel: Rect) -> Option<Rect> {
        self.numbers
            .iter()
            .find_map(|controls| controls.caret_rect(panel))
    }
}

struct PropertyControls<N, E, A, S, C> {
    numbers: NumberControls<N>,
    enums: EnumControls<E>,
    angles: AngleControls<A>,
    sliders: SliderControls<S>,
    color_picker: ColorPicker,
    color_target: Option<C>,
    color_rect: Option<Rect>,
}

impl<N, E, A, S, C> Default for PropertyControls<N, E, A, S, C> {
    fn default() -> Self {
        Self {
            numbers: Default::default(),
            enums: Default::default(),
            angles: Default::default(),
            sliders: Default::default(),
            color_picker: ColorPicker::new(Color::BLACK),
            color_target: None,
            color_rect: None,
        }
    }
}

impl<N: Clone + Eq + Hash, E: Clone + Eq + Hash, A: Clone + Eq + Hash, S: Eq + Hash, C>
    PropertyControls<N, E, A, S, C>
{
    fn tick(&mut self, dt: f32) {
        self.numbers.tick(dt);
        self.enums.tick(dt);
        self.angles.tick(dt);
        self.sliders.tick(dt);
        self.color_picker.tick(dt);
    }

    fn is_animating(&self) -> bool {
        self.numbers.is_animating()
            || self.enums.is_animating()
            || self.angles.is_animating()
            || self.sliders.is_animating()
            || self.color_picker.is_animating()
    }

    fn clear_layout(&mut self) {
        self.numbers.clear_layout();
        self.enums.clear_layout();
        self.angles.clear_layout();
        self.sliders.clear_layout();
    }

    fn blur(&mut self) {
        self.numbers.blur();
        self.angles.blur();
        self.enums.close();
        self.sliders.pointer_released();
        self.color_picker.close();
        self.color_target = None;
        self.color_rect = None;
    }

    fn is_dragging(&self) -> bool {
        self.numbers.is_dragging()
            || self.angles.is_dragging()
            || self.sliders.is_dragging()
            || self.color_picker.is_dragging()
    }

    fn is_cursor_lock_dragging(&self) -> bool {
        self.numbers.is_dragging()
            || self.angles.is_number_dragging()
            || self.sliders.is_dragging()
            || self.color_picker.is_dragging()
    }

    fn pointer_released(&mut self) -> bool {
        self.numbers.pointer_released()
            | self.angles.pointer_released()
            | self.sliders.pointer_released()
            | self.color_picker.pointer_released()
    }

    fn color_picker_contains(&self, rect: Rect, point: [f32; 2]) -> bool {
        let Some(swatch) = self.color_rect else {
            return false;
        };
        self.color_picker.popup_contains_in(
            swatch,
            Rect::new(0.0, 0.0, rect.width, rect.height),
            [point[0] - rect.x, point[1] - rect.y],
        )
    }

    fn popup_contains(&self, rect: Rect, point: [f32; 2]) -> bool
    where
        E: Clone,
    {
        self.enums.popup_contains(rect, point) || self.color_picker_contains(rect, point)
    }

    fn caret_rect(&self, panel: Rect) -> Option<Rect> {
        self.numbers
            .caret_rect(panel)
            .or_else(|| self.angles.caret_rect(panel))
    }
}

mod media_rows;

use media_rows::*;

#[derive(Clone, Debug)]
pub enum InspectorAction {
    ChoosePipeline(Rect),
    ChooseFont(Rect),
    CreatePipeline,
    AddEffect,
    MoveEffect(u64, i32),
    RemoveEffect(u64),
    MakeIndependent,
    OpenGraph,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum PluginPropertyTarget {
    Generator(String),
    Pipeline { node: u64, input: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum InspectorColorTarget {
    Plugin(PluginPropertyTarget),
    GradientStop(usize),
}

impl PluginPropertyTarget {
    fn value(&self, project: &Project, timeline: &TimelineState) -> Option<GpuValue> {
        match self {
            Self::Generator(input) => timeline.generator_value(input),
            Self::Pipeline { node, input } => timeline.pipeline_input_value(project, *node, input),
        }
    }

    fn set_value(
        &self,
        project: &mut Project,
        timeline: &mut TimelineState,
        value: GpuValue,
    ) -> bool {
        match self {
            Self::Generator(input) => {
                timeline.set_generator_value(input, value);
                true
            }
            Self::Pipeline { node, input } => {
                timeline.set_pipeline_input_value(project, *node, input, value)
            }
        }
    }

    fn host_value(&self, project: &Project, timeline: &TimelineState) -> Option<HostValue> {
        match self {
            Self::Generator(input) => timeline.generator_host_value(input),
            Self::Pipeline { node, input } => timeline
                .pipeline_input_value(project, *node, input)
                .map(HostValue::Gpu)
                .or_else(|| timeline.pipeline_host_input_value(project, *node, input)),
        }
    }

    fn set_host_value(
        &self,
        project: &mut Project,
        timeline: &mut TimelineState,
        value: HostValue,
    ) -> bool {
        match self {
            Self::Generator(input) => {
                timeline.set_generator_host_value(input, value);
                true
            }
            Self::Pipeline { node, input } => match value {
                HostValue::Gpu(value) => {
                    timeline.set_pipeline_input_value(project, *node, input, value)
                }
                value => timeline.set_pipeline_host_input_value(project, *node, input, value),
            },
        }
    }

    fn toggle_keyframe(&self, project: &mut Project, timeline: &mut TimelineState) {
        match self {
            Self::Generator(input) => timeline.toggle_generator_keyframe(input),
            Self::Pipeline { node, input } => {
                timeline.toggle_pipeline_keyframe(project, *node, input)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InspectorSectionTarget {
    Source,
    Effects,
    Transform,
    Compositing,
}

#[derive(Clone, Debug)]
enum InspectorContextTarget {
    Number(InspectorNumberTarget),
    Angle(InspectorAngleTarget),
    Enum(InspectorEnumTarget),
    Plugin(PluginPropertyTarget),
    Section(InspectorSectionTarget),
}

#[derive(Clone, Debug)]
struct InspectorContextMenu {
    point: [f32; 2],
    target: InspectorContextTarget,
}

#[derive(Clone, Debug)]
struct InspectorSourceValues {
    speed: Option<f32>,
    generator: Option<String>,
    parameters: BTreeMap<String, HostValue>,
}

#[derive(Clone, Debug)]
struct InspectorEffectValues {
    node_type: String,
    occurrence: usize,
    parameters: BTreeMap<String, HostValue>,
}

#[derive(Clone, Debug)]
enum InspectorClipboard {
    Number(f64),
    Angle(f32),
    Enum(usize),
    Plugin(HostValue),
    Source(InspectorSourceValues),
    Effects(Vec<InspectorEffectValues>),
    Transform(BTreeMap<String, GpuValue>),
    Compositing {
        opacity: Option<f32>,
        blend_mode: Option<usize>,
        alpha_blend_mode: Option<usize>,
    },
}

fn generator_clipboard_key(generator: &GeneratorSource) -> String {
    match generator {
        GeneratorSource::Plugin { generator_type, .. } => format!("plugin:{generator_type}"),
        GeneratorSource::Wasm {
            plugin_id, entry, ..
        } => format!("wasm:{plugin_id}:{entry}"),
    }
}

fn inspector_angle_value(
    target: &InspectorAngleTarget,
    project: &Project,
    timeline: &TimelineState,
) -> Option<f32> {
    match target {
        InspectorAngleTarget::Rotation => {
            timeline.transform_value("rotation").and_then(GpuValue::f32)
        }
        InspectorAngleTarget::Plugin(target) => {
            target.value(project, timeline).and_then(GpuValue::f32)
        }
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum InspectorNumberTarget {
    Plugin {
        target: PluginPropertyTarget,
        component: Option<usize>,
        percent: bool,
    },
    Speed,
    Volume,
    Transform {
        input: String,
        component: usize,
        display: TransformNumberDisplay,
    },
    Model3dClip {
        input: String,
        component: usize,
    },
    Opacity,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum InspectorEnumTarget {
    Plugin {
        target: PluginPropertyTarget,
        boolean: bool,
    },
    Model3dClipShading,
    BlendMode,
    AlphaBlendMode,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum InspectorVectorTarget {
    Plugin(PluginPropertyTarget),
    Transform(String),
    Model3dClip(String),
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum InspectorAngleTarget {
    Rotation,
    Plugin(PluginPropertyTarget),
}

#[derive(Clone, Debug)]
enum InspectorResetTarget {
    Plugin {
        target: PluginPropertyTarget,
        default: crate::project::HostValue,
    },
    Speed,
    Volume,
    Transform {
        input: String,
        default: GpuValue,
    },
    Model3dClip {
        input: String,
        default: GpuValue,
    },
    Opacity,
    BlendMode,
    AlphaBlendMode,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum TransformNumberDisplay {
    PositionPixels,
    Percent,
}

const TRANSFORM_VECTOR_ROWS: [(&str, &str, TransformNumberDisplay); 3] = [
    (
        "Position",
        "position",
        TransformNumberDisplay::PositionPixels,
    ),
    ("Scale", "scale", TransformNumberDisplay::Percent),
    ("Anchor", "anchor", TransformNumberDisplay::Percent),
];

#[derive(Clone, Copy)]
enum InspectorTextField {
    Text,
    ClipStart,
    ClipEnd,
    PipelineName,
}

pub(crate) struct InspectorBuildContext<'a> {
    pub project: &'a Project,
    pub timeline: &'a TimelineState,
    pub media_selection: Option<(MediaId, MediaStream)>,
    pub plugins: &'a PluginRegistry,
    pub icons: Icons,
}

pub(crate) struct InspectorPointerContext<'a> {
    pub modifiers: ModifiersState,
    pub project: &'a mut Project,
    pub timeline: &'a mut TimelineState,
    pub media_selection: Option<(MediaId, MediaStream)>,
    pub plugins: &'a PluginRegistry,
}

default_state! {
    pub struct InspectorState {
        media_asset: Option<MediaId>,
        media_general_section: Accordion = Accordion::new(true),
        media_video_section: Accordion = Accordion::new(true),
        media_audio_section: Accordion = Accordion::new(true),
        media_model_section: Accordion = Accordion::new(true),
        text_clip: Option<u32>,
        text: TextEdit = TextEdit::single_line(""),
        summary_clip: Option<u32>,
        clip_start: TextEdit = TextEdit::single_line(""),
        clip_end: TextEdit = TextEdit::single_line(""),
        pipeline_id: Option<u64>,
        pipeline_name: TextEdit = TextEdit::single_line(""),
        pending_action: Option<InspectorAction>,
        source_section: Accordion = Accordion::new(true),
        pipeline_section: Accordion = Accordion::new(true),
        controls_section: Accordion = Accordion::new(true),
        model3d_clip_section: Accordion = Accordion::new(true),
        transform_section: Accordion = Accordion::new(true),
        compositing_section: Accordion = Accordion::new(true),
        effect_sections: HashMap<u64, Accordion>,
        scroll_y: f32,
        content_height: f32,
        controls: PropertyControls<
            InspectorNumberTarget,
            InspectorEnumTarget,
            InspectorAngleTarget,
            (u64, usize),
            InspectorColorTarget,
        >,
        eq_scroll: HashMap<u64, f32>,
        eq_scroll_rects: HashMap<u64, (Rect, f32)>,
        vector_links: HashSet<InspectorVectorTarget> = HashSet::from([
            InspectorVectorTarget::Transform("scale".into()),
            InspectorVectorTarget::Model3dClip("scale".into()),
        ]),
        vector_link_rects: HashMap<InspectorVectorTarget, Rect>,
        reset_targets: Vec<(Rect, InspectorResetTarget)>,
        context_menu: Option<InspectorContextMenu>,
        context_cursor: [f32; 2] = [0.0, 0.0],
        value_clipboard: Option<InspectorClipboard>,
        last_rect: Option<Rect>,
    }
}

impl InspectorState {
    pub fn sync_color_picker_textures(&mut self, renderer: &mut Renderer) -> Result<()> {
        self.controls.color_picker.sync_textures(renderer)
    }

    pub fn tick(&mut self, dt: f32) {
        self.text.tick(dt);
        self.clip_start.tick(dt);
        self.clip_end.tick(dt);
        self.pipeline_name.tick(dt);
        self.controls.tick(dt);
        for section in [
            &mut self.media_general_section,
            &mut self.media_video_section,
            &mut self.media_audio_section,
            &mut self.media_model_section,
            &mut self.source_section,
            &mut self.pipeline_section,
            &mut self.controls_section,
            &mut self.model3d_clip_section,
            &mut self.transform_section,
            &mut self.compositing_section,
        ] {
            section.tick(dt);
        }
        self.effect_sections
            .values_mut()
            .for_each(|section| section.tick(dt));
    }

    pub fn is_animating(&self) -> bool {
        self.text.is_animating()
            || self.clip_start.is_animating()
            || self.clip_end.is_animating()
            || self.pipeline_name.is_animating()
            || self.controls.is_animating()
            || [
                &self.media_general_section,
                &self.media_video_section,
                &self.media_audio_section,
                &self.media_model_section,
                &self.source_section,
                &self.pipeline_section,
                &self.controls_section,
                &self.model3d_clip_section,
                &self.transform_section,
                &self.compositing_section,
            ]
            .into_iter()
            .any(Accordion::is_animating)
            || self.effect_sections.values().any(Accordion::is_animating)
    }

    fn clear_editor_focus(&mut self) {
        self.text.set_focused(false);
        self.clip_start.set_focused(false);
        self.clip_end.set_focused(false);
        self.pipeline_name.set_focused(false);
        self.controls.blur();
    }

    pub fn set_focused(&mut self, focused: bool) {
        if focused {
            return;
        }
        self.context_menu = None;
        self.clear_editor_focus();
    }

    pub fn popup_contains(&self, rect: Rect, point: [f32; 2]) -> bool {
        let context_menu = self.context_menu.as_ref().is_some_and(|menu| {
            let local = [point[0] - rect.x, point[1] - rect.y];
            context_menu_rect(Rect::new(0.0, 0.0, rect.width, rect.height), menu.point, 2)
                .contains(local)
        });
        context_menu || self.controls.popup_contains(rect, point)
    }

    pub fn close_context_menu(&mut self) {
        self.context_menu = None;
    }

    pub fn take_action(&mut self) -> Option<InspectorAction> {
        self.pending_action.take()
    }

    fn context_paste_enabled(
        &self,
        target: &InspectorContextTarget,
        project: &Project,
        timeline: &TimelineState,
    ) -> bool {
        match (target, self.value_clipboard.as_ref()) {
            (InspectorContextTarget::Number(_), Some(InspectorClipboard::Number(_)))
            | (InspectorContextTarget::Angle(_), Some(InspectorClipboard::Angle(_))) => true,
            (InspectorContextTarget::Enum(target), Some(InspectorClipboard::Enum(index))) => self
                .controls
                .enums
                .get(target)
                .is_some_and(|control| *index < control.option_count),
            (InspectorContextTarget::Plugin(target), Some(InspectorClipboard::Plugin(value))) => {
                target
                    .host_value(project, timeline)
                    .is_some_and(|current| current.compatible(value))
            }
            (
                InspectorContextTarget::Section(InspectorSectionTarget::Source),
                Some(InspectorClipboard::Source(_)),
            )
            | (
                InspectorContextTarget::Section(InspectorSectionTarget::Effects),
                Some(InspectorClipboard::Effects(_)),
            )
            | (
                InspectorContextTarget::Section(InspectorSectionTarget::Transform),
                Some(InspectorClipboard::Transform(_)),
            )
            | (
                InspectorContextTarget::Section(InspectorSectionTarget::Compositing),
                Some(InspectorClipboard::Compositing { .. }),
            ) => true,
            _ => false,
        }
    }

    fn build_value_context_menu(
        &self,
        ctx: &mut kama_ui::BuildCtx,
        rect: Rect,
        project: &Project,
        timeline: &TimelineState,
        icons: Icons,
    ) {
        let Some(menu) = self.context_menu.as_ref() else {
            return;
        };
        let menu_rect = context_menu_rect(rect, menu.point, 2);
        let plural = matches!(&menu.target, InspectorContextTarget::Section(_));
        let items = [
            ContextMenuItem {
                label: if plural { "Copy Values" } else { "Copy Value" },
                shortcut: None,
                icon: Some(AppIcon::Copy),
                enabled: true,
            },
            ContextMenuItem {
                label: if plural {
                    "Paste Values"
                } else {
                    "Paste Value"
                },
                shortcut: None,
                icon: Some(AppIcon::Paste),
                enabled: self.context_paste_enabled(&menu.target, project, timeline),
            },
        ];
        build_context_menu(
            ctx,
            "inspector-values",
            menu_rect,
            self.context_cursor,
            &items,
            icons,
        );
    }

    fn copy_context_value(
        &self,
        target: &InspectorContextTarget,
        project: &Project,
        timeline: &TimelineState,
    ) -> Option<InspectorClipboard> {
        match target {
            InspectorContextTarget::Number(target) => self
                .controls
                .numbers
                .get(target)
                .map(|input| InspectorClipboard::Number(input.value())),
            InspectorContextTarget::Angle(target) => {
                inspector_angle_value(target, project, timeline).map(InspectorClipboard::Angle)
            }
            InspectorContextTarget::Enum(target) => self
                .controls
                .enums
                .get(target)
                .map(|control| InspectorClipboard::Enum(control.combo.selected())),
            InspectorContextTarget::Plugin(target) => target
                .host_value(project, timeline)
                .map(InspectorClipboard::Plugin),
            InspectorContextTarget::Section(section) => {
                self.copy_section_values(*section, project, timeline)
            }
        }
    }

    fn copy_section_values(
        &self,
        section: InspectorSectionTarget,
        project: &Project,
        timeline: &TimelineState,
    ) -> Option<InspectorClipboard> {
        match section {
            InspectorSectionTarget::Source => {
                let mut parameters = BTreeMap::new();
                let generator = timeline.selected_generator();
                if let Some(generator) = generator {
                    for input in generator.parameters().keys() {
                        if let Some(value) = timeline.generator_host_value(input) {
                            parameters.insert(input.clone(), value);
                        }
                    }
                }
                Some(InspectorClipboard::Source(InspectorSourceValues {
                    speed: timeline.selected_speed(project),
                    generator: generator.map(generator_clipboard_key),
                    parameters,
                }))
            }
            InspectorSectionTarget::Transform => {
                let values = ["position", "scale", "anchor", "rotation"]
                    .into_iter()
                    .filter_map(|input| {
                        timeline
                            .transform_value(input)
                            .map(|value| (input.to_string(), value))
                    })
                    .collect::<BTreeMap<_, _>>();
                (!values.is_empty()).then_some(InspectorClipboard::Transform(values))
            }
            InspectorSectionTarget::Compositing => Some(InspectorClipboard::Compositing {
                opacity: timeline.selected_opacity(),
                blend_mode: timeline.selected_blend_mode().and_then(|value| {
                    BlendMode::ALL
                        .iter()
                        .position(|candidate| *candidate == value)
                }),
                alpha_blend_mode: timeline.selected_alpha_blend_mode().and_then(|value| {
                    AlphaBlendMode::ALL
                        .iter()
                        .position(|candidate| *candidate == value)
                }),
            }),
            InspectorSectionTarget::Effects => {
                let pipeline = timeline
                    .selected_pipeline()
                    .and_then(|instance| instance.pipeline)
                    .and_then(|id| project.pipeline(id))?;
                let mut occurrences = HashMap::<String, usize>::new();
                let mut effects = Vec::new();
                for node in pipeline.main_path() {
                    let occurrence = occurrences.entry(node.node_type.clone()).or_default();
                    let node_occurrence = *occurrence;
                    *occurrence += 1;
                    let mut parameters = BTreeMap::new();
                    for input in node.inputs.keys() {
                        if let Some(value) = timeline.pipeline_input_value(project, node.id, input)
                        {
                            parameters.insert(input.clone(), HostValue::Gpu(value));
                        }
                    }
                    for input in node.host_inputs.keys() {
                        if let Some(value) =
                            timeline.pipeline_host_input_value(project, node.id, input)
                        {
                            parameters.insert(input.clone(), value);
                        }
                    }
                    effects.push(InspectorEffectValues {
                        node_type: node.node_type.clone(),
                        occurrence: node_occurrence,
                        parameters,
                    });
                }
                Some(InspectorClipboard::Effects(effects))
            }
        }
    }

    fn paste_context_value(
        &mut self,
        target: &InspectorContextTarget,
        project: &mut Project,
        timeline: &mut TimelineState,
    ) -> bool {
        let Some(clipboard) = self.value_clipboard.clone() else {
            return false;
        };
        match (target, clipboard) {
            (InspectorContextTarget::Number(target), InspectorClipboard::Number(value)) => {
                self.apply_number_value(target, value, project, timeline);
                true
            }
            (InspectorContextTarget::Angle(target), InspectorClipboard::Angle(value)) => {
                Self::apply_angle_value(target, value, project, timeline);
                true
            }
            (InspectorContextTarget::Enum(target), InspectorClipboard::Enum(index)) => {
                if !self
                    .controls
                    .enums
                    .get(target)
                    .is_some_and(|control| index < control.option_count)
                {
                    return false;
                }
                self.apply_enum_value(target, index, project, timeline)
            }
            (InspectorContextTarget::Plugin(target), InspectorClipboard::Plugin(value)) => target
                .host_value(project, timeline)
                .filter(|current| current.compatible(&value))
                .is_some_and(|_| target.set_host_value(project, timeline, value)),
            (InspectorContextTarget::Section(section), clipboard) => {
                self.paste_section_values(*section, clipboard, project, timeline)
            }
            _ => false,
        }
    }

    fn apply_enum_value(
        &mut self,
        target: &InspectorEnumTarget,
        index: usize,
        project: &mut Project,
        timeline: &mut TimelineState,
    ) -> bool {
        match target {
            InspectorEnumTarget::Plugin { target, boolean } => target.set_value(
                project,
                timeline,
                if *boolean {
                    GpuValue::Bool(index != 0)
                } else {
                    GpuValue::Enum(index as u32)
                },
            ),
            InspectorEnumTarget::Model3dClipShading => {
                timeline.set_selected_model3d_shading(
                    project,
                    crate::project::Model3dShading::from_index(index),
                );
                true
            }
            InspectorEnumTarget::BlendMode => {
                if index >= BlendMode::ALL.len() {
                    return false;
                }
                timeline.set_selected_blend_mode(index);
                true
            }
            InspectorEnumTarget::AlphaBlendMode => {
                if index >= AlphaBlendMode::ALL.len() {
                    return false;
                }
                timeline.set_selected_alpha_blend_mode(index);
                true
            }
        }
    }

    fn paste_section_values(
        &mut self,
        section: InspectorSectionTarget,
        clipboard: InspectorClipboard,
        project: &mut Project,
        timeline: &mut TimelineState,
    ) -> bool {
        match (section, clipboard) {
            (InspectorSectionTarget::Source, InspectorClipboard::Source(values)) => {
                let mut changed = false;
                if let Some(speed) = values.speed {
                    if timeline.selected_speed(project).is_some() {
                        timeline.set_selected_speed(project, speed);
                        changed = true;
                    }
                }
                let destination = timeline.selected_generator().map(generator_clipboard_key);
                if destination == values.generator && destination.is_some() {
                    for (input, value) in values.parameters {
                        let compatible = timeline
                            .generator_host_value(&input)
                            .is_some_and(|current| current.compatible(&value));
                        if compatible {
                            timeline.set_generator_host_value(&input, value);
                            changed = true;
                        }
                    }
                }
                changed
            }
            (InspectorSectionTarget::Transform, InspectorClipboard::Transform(values)) => {
                let mut changed = false;
                for (input, value) in values {
                    if timeline
                        .transform_value(&input)
                        .is_some_and(|current| current.compatible(value))
                    {
                        timeline.set_transform_value(&input, value);
                        changed = true;
                    }
                }
                changed
            }
            (
                InspectorSectionTarget::Compositing,
                InspectorClipboard::Compositing {
                    opacity,
                    blend_mode,
                    alpha_blend_mode,
                },
            ) => {
                let mut changed = false;
                if let Some(opacity) = opacity {
                    if timeline.selected_opacity().is_some() {
                        timeline.set_selected_opacity(opacity);
                        changed = true;
                    }
                }
                if let Some(index) = blend_mode {
                    if timeline.selected_blend_mode().is_some() && index < BlendMode::ALL.len() {
                        timeline.set_selected_blend_mode(index);
                        changed = true;
                    }
                }
                if let Some(index) = alpha_blend_mode {
                    if timeline.selected_alpha_blend_mode().is_some()
                        && index < AlphaBlendMode::ALL.len()
                    {
                        timeline.set_selected_alpha_blend_mode(index);
                        changed = true;
                    }
                }
                changed
            }
            (InspectorSectionTarget::Effects, InspectorClipboard::Effects(values)) => {
                let Some(pipeline_id) = timeline
                    .selected_pipeline()
                    .and_then(|instance| instance.pipeline)
                else {
                    return false;
                };
                let Some(pipeline) = project.pipeline(pipeline_id) else {
                    return false;
                };
                let mut occurrences = HashMap::<String, usize>::new();
                let destinations = pipeline
                    .main_path()
                    .into_iter()
                    .map(|node| {
                        let occurrence = occurrences.entry(node.node_type.clone()).or_default();
                        let result = (node.id, node.node_type.clone(), *occurrence);
                        *occurrence += 1;
                        result
                    })
                    .collect::<Vec<_>>();
                let mut changed = false;
                for effect in values {
                    let Some((node, _, _)) =
                        destinations.iter().find(|(_, node_type, occurrence)| {
                            node_type == &effect.node_type && *occurrence == effect.occurrence
                        })
                    else {
                        continue;
                    };
                    for (input, value) in effect.parameters {
                        match value {
                            HostValue::Gpu(value) => {
                                let compatible = timeline
                                    .pipeline_input_value(project, *node, &input)
                                    .is_some_and(|current| current.compatible(value));
                                if compatible
                                    && timeline
                                        .set_pipeline_input_value(project, *node, &input, value)
                                {
                                    changed = true;
                                }
                            }
                            value => {
                                let compatible = timeline
                                    .pipeline_host_input_value(project, *node, &input)
                                    .is_some_and(|current| current.compatible(&value));
                                if compatible
                                    && timeline.set_pipeline_host_input_value(
                                        project, *node, &input, value,
                                    )
                                {
                                    changed = true;
                                }
                            }
                        }
                    }
                }
                changed
            }
            _ => false,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn build_model3d_clip_row(
        &mut self,
        ctx: &mut kama_ui::BuildCtx,
        row: (Rect, f32, &str),
        project: &Project,
        timeline: &TimelineState,
        input: &str,
        settings: NumberSettings<'_>,
        icons: Icons,
    ) {
        let Some(GpuValue::Vec3(value)) = timeline.selected_model3d_value(project, input) else {
            return;
        };
        self.build_vector_row(
            ctx,
            row.0,
            row.1,
            row.2,
            InspectorVectorTarget::Model3dClip(input.into()),
            3,
            settings,
            keyframe_control(
                icons,
                timeline.selected_model3d_has_keyframe(project, input),
                timeline.selected_model3d_has_keyframes(project, input),
            ),
            |component| {
                (
                    InspectorNumberTarget::Model3dClip {
                        input: input.into(),
                        component,
                    },
                    value[component] as f64,
                )
            },
        );
    }

    fn build_number_property(
        &mut self,
        ctx: &mut kama_ui::BuildCtx,
        row: (Rect, f32, &str),
        target: InspectorNumberTarget,
        value: f64,
        settings: NumberSettings<'_>,
        keyframe: Option<KeyframeControl>,
    ) {
        let (rect, y, label) = row;
        let control = property_chrome(ctx, rect, y, label, "number", keyframe);
        let kind = if keyframe.is_some() {
            "number"
        } else {
            "plain-number"
        };
        self.controls.numbers.build(
            ctx,
            FormatKey::new(format_args!("inspector-{kind}-{label}-{}", y.to_bits())),
            target,
            (control, value, settings),
            crate::widgets::component_style(),
        );
    }

    fn build_enum_property<T: AsRef<str>>(
        &mut self,
        ctx: &mut kama_ui::BuildCtx,
        row: (Rect, f32, &str),
        target: InspectorEnumTarget,
        selected: usize,
        options: &[T],
        chrome: (KeyframeControl, IconId),
    ) {
        let (rect, y, label) = row;
        let (keyframe, chevron) = chrome;
        let combo = property_chrome(ctx, rect, y, label, "enum", Some(keyframe));
        self.controls.enums.build(
            ctx,
            FormatKey::new(format_args!("inspector-enum-{label}-{}", y.to_bits())),
            target,
            (combo, selected),
            options,
            (chevron, crate::widgets::component_style()),
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn build_vector_row(
        &mut self,
        ctx: &mut kama_ui::BuildCtx,
        rect: Rect,
        y: f32,
        label: &str,
        link_target: InspectorVectorTarget,
        count: usize,
        settings: NumberSettings<'_>,
        keyframe: KeyframeControl,
        mut component: impl FnMut(usize) -> (InspectorNumberTarget, f64),
    ) {
        let row = row_hit(rect, y);
        let parts = kama_ui::layout::row(
            row,
            &[
                kama_ui::layout::Item::width(6.0),
                kama_ui::layout::Item::fill_portion(0.26),
                kama_ui::layout::Item::width(3.0),
                kama_ui::layout::Item::fill_portion(0.74),
                kama_ui::layout::Item::width(4.0),
                kama_ui::layout::Item::new(
                    Size::Pixels(19.0),
                    Size::Pixels((row.height - 6.0).max(1.0)),
                ),
                kama_ui::layout::Item::width(4.0),
                kama_ui::layout::Item::new(Size::Pixels(18.0), Size::Pixels(18.0)),
                kama_ui::layout::Item::width(4.0),
            ],
            0.0,
            0.0,
            kama_ui::Align::Center,
        );
        let label_rect = parts[1];
        let controls = parts[3];
        let link = parts[5];
        let key = parts[7];
        ui_text!(
            ctx,
            FormatKey::new(format_args!("vector-label-{link_target:?}")),
            label_rect,
            9.5,
            theme::text(),
            label
        );
        self.vector_link_rects.insert(link_target.clone(), link);
        ToggleButton::build(
            ctx,
            FormatKey::new(format_args!("vector-link-{link_target:?}")),
            link,
            "↔",
            self.vector_links.contains(&link_target),
            crate::widgets::component_style(),
        );

        let mut component_items = Vec::with_capacity(count.saturating_mul(2).saturating_sub(1));
        for index in 0..count {
            if index > 0 {
                component_items.push(kama_ui::layout::Item::width(3.0));
            }
            component_items.push(kama_ui::layout::Item::new(
                Size::Fill,
                Size::Pixels((row.height - 4.0).max(1.0)),
            ));
        }
        let component_rects =
            kama_ui::layout::row(controls, &component_items, 0.0, 0.0, kama_ui::Align::Center);
        for index in 0..count {
            let (target, value) = component(index);
            self.controls.numbers.build(
                ctx,
                FormatKey::new(format_args!("vector-value-{link_target:?}-{index}")),
                target,
                (component_rects[index * 2], value, settings),
                crate::widgets::component_style(),
            );
        }
        toggle_icon_button(
            ctx,
            &format!("vector-key-{link_target:?}"),
            key,
            keyframe.icon,
            keyframe.keyed,
            if keyframe.keyed {
                "Remove keyframe"
            } else {
                "Add keyframe"
            },
            crate::widgets::component_style(),
        );
    }

    fn build_plugin_vector_property(
        &mut self,
        ctx: &mut kama_ui::BuildCtx,
        row: (Rect, f32, &str),
        target: PluginPropertyTarget,
        value: GpuValue,
        settings: (NumberSettings<'_>, KeyframeControl),
    ) {
        let (rect, y, label) = row;
        let (settings, keyframe) = settings;
        let count = value.component_count().clamp(2, 4);
        self.build_vector_row(
            ctx,
            rect,
            y,
            label,
            InspectorVectorTarget::Plugin(target.clone()),
            count,
            settings,
            keyframe,
            |component| {
                (
                    InspectorNumberTarget::Plugin {
                        target: target.clone(),
                        component: Some(component),
                        percent: false,
                    },
                    value.numeric(Some(component)).unwrap_or_default(),
                )
            },
        );
    }

    fn build_transform_number_row(
        &mut self,
        ctx: &mut kama_ui::BuildCtx,
        row: (Rect, f32, &str),
        timeline: &TimelineState,
        source: (&str, TransformNumberDisplay, [f32; 2]),
        icons: Icons,
    ) {
        let (rect, y, label) = row;
        let (input, display, extent) = source;
        let value = timeline
            .transform_value(input)
            .and_then(GpuValue::vec2)
            .unwrap_or([0.0; 2]);
        let suffix = match display {
            TransformNumberDisplay::PositionPixels => " px",
            TransformNumberDisplay::Percent => "%",
        };
        self.build_vector_row(
            ctx,
            rect,
            y,
            label,
            InspectorVectorTarget::Transform(input.into()),
            2,
            ((f64::NEG_INFINITY, f64::INFINITY), 1.0, 1, suffix),
            keyframe_control(
                icons,
                timeline.transform_has_keyframe(input),
                timeline.transform_has_keyframes(input),
            ),
            |component| {
                let display_value = match display {
                    TransformNumberDisplay::PositionPixels => {
                        value[component] as f64 * extent[component].max(1.0) as f64
                    }
                    TransformNumberDisplay::Percent => value[component] as f64 * 100.0,
                };
                (
                    InspectorNumberTarget::Transform {
                        input: input.into(),
                        component,
                        display,
                    },
                    display_value,
                )
            },
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn build_angle_row(
        &mut self,
        ctx: &mut kama_ui::BuildCtx,
        rect: Rect,
        y: f32,
        label: &str,
        id: &str,
        target: InspectorAngleTarget,
        angle: f32,
        keyframe: KeyframeControl,
    ) {
        let (label_rect, turns, degrees, knob, key) = transform_rotation_parts(rect, y);
        ui_text!(
            ctx,
            format!("{id}-label"),
            label_rect,
            9.5,
            theme::text(),
            label
        );
        self.controls.angles.build(
            ctx,
            target,
            id,
            (turns, degrees, knob),
            (angle, 100.0, true),
            crate::widgets::component_style(),
        );
        toggle_icon_button(
            ctx,
            &format!("{id}-key"),
            key,
            keyframe.icon,
            keyframe.keyed,
            if keyframe.keyed {
                "Remove keyframe"
            } else {
                "Add keyframe"
            },
            crate::widgets::component_style(),
        );
    }

    fn build_rotation_number_row(
        &mut self,
        ctx: &mut kama_ui::BuildCtx,
        rect: Rect,
        y: f32,
        rotation: f32,
        keyframe: KeyframeControl,
    ) {
        self.build_angle_row(
            ctx,
            rect,
            y,
            "Rotation",
            "transform-rotation",
            InspectorAngleTarget::Rotation,
            rotation,
            keyframe,
        );
    }

    fn build_angle_property(
        &mut self,
        ctx: &mut kama_ui::BuildCtx,
        row: (Rect, f32, &str),
        target: PluginPropertyTarget,
        angle: f32,
        keyframe: KeyframeControl,
    ) {
        let (rect, y, label) = row;
        let id = format!("angle-{target:?}");
        self.build_angle_row(
            ctx,
            rect,
            y,
            label,
            &id,
            InspectorAngleTarget::Plugin(target),
            angle,
            keyframe,
        );
    }

    fn apply_angle_value(
        target: &InspectorAngleTarget,
        angle: f32,
        project: &mut Project,
        timeline: &mut TimelineState,
    ) {
        match target {
            InspectorAngleTarget::Rotation => {
                timeline.set_transform_value("rotation", GpuValue::F32(angle))
            }
            InspectorAngleTarget::Plugin(target) => {
                target.set_value(project, timeline, GpuValue::F32(angle));
            }
        }
    }

    fn apply_number_value(
        &self,
        target: &InspectorNumberTarget,
        display_value: f64,
        project: &mut Project,
        timeline: &mut TimelineState,
    ) {
        match target {
            InspectorNumberTarget::Plugin {
                target,
                component,
                percent,
            } => {
                let Some(current) = target.value(project, timeline) else {
                    return;
                };
                let raw = if *percent {
                    display_value as f32 / 100.0
                } else {
                    display_value as f32
                };
                let linked = self
                    .vector_links
                    .contains(&InspectorVectorTarget::Plugin(target.clone()));
                let next = match *component {
                    Some(component) => current.with_component(component, raw, linked),
                    None => current.with_numeric(None, raw),
                };
                if let Some(next) = next {
                    target.set_value(project, timeline, next);
                }
            }
            InspectorNumberTarget::Speed => {
                timeline.set_selected_speed(project, display_value as f32 / 100.0)
            }
            InspectorNumberTarget::Volume => {
                timeline.set_selected_clip_volume(display_value as f32 / 100.0)
            }
            InspectorNumberTarget::Transform {
                input,
                component,
                display,
            } => {
                let raw = match display {
                    TransformNumberDisplay::PositionPixels => {
                        let extent = if *component == 0 {
                            project.active_settings().canvas_size[0]
                        } else {
                            project.active_settings().canvas_size[1]
                        };
                        display_value as f32 / extent.max(1) as f32
                    }
                    TransformNumberDisplay::Percent => display_value as f32 / 100.0,
                };
                let linked = self
                    .vector_links
                    .contains(&InspectorVectorTarget::Transform(input.clone()));
                timeline.set_transform_component_linked(input, *component, raw, linked);
            }
            InspectorNumberTarget::Model3dClip { input, component } => {
                let linked = self
                    .vector_links
                    .contains(&InspectorVectorTarget::Model3dClip(input.clone()));
                timeline.set_selected_model3d_component_linked(
                    project,
                    input,
                    *component,
                    display_value as f32,
                    linked,
                );
            }
            InspectorNumberTarget::Opacity => {
                timeline.set_selected_opacity((display_value as f32 / 100.0).clamp(0.0, 1.0));
            }
        }
    }

    fn sync_selection(&mut self, project: &Project, timeline: &TimelineState) {
        let text_clip = timeline
            .selected_clip()
            .filter(|_| timeline.selected_text().is_some())
            .map(|clip| clip.id);
        let text_changed = self.text_clip != text_clip;
        self.text_clip = text_clip;
        sync_text_edit(
            &mut self.text,
            text_changed,
            &timeline.selected_text().unwrap_or_default(),
        );

        let summary_clip = timeline.selected_clip().map(|clip| clip.id);
        let summary_changed = self.summary_clip != summary_clip;
        self.summary_clip = summary_clip;
        if let Some(clip) = timeline.selected_clip() {
            sync_text_edit(
                &mut self.clip_start,
                summary_changed,
                &format_timecode(clip.start, project.active_settings().frame_rate as f32),
            );
            sync_text_edit(
                &mut self.clip_end,
                summary_changed,
                &format_timecode(clip.end(), project.active_settings().frame_rate as f32),
            );
        } else {
            sync_text_edit(&mut self.clip_start, summary_changed, "");
            sync_text_edit(&mut self.clip_end, summary_changed, "");
        }

        let pipeline_id = timeline
            .selected_pipeline()
            .and_then(|instance| instance.pipeline);
        let pipeline_changed = self.pipeline_id != pipeline_id;
        self.pipeline_id = pipeline_id;
        let name = pipeline_id
            .and_then(|id| project.pipeline(id))
            .map_or("", |pipeline| pipeline.name.as_str());
        sync_text_edit(&mut self.pipeline_name, pipeline_changed, name);
    }

    fn sync_effect_sections(&mut self, project: &Project, timeline: &TimelineState) {
        let live = timeline
            .selected_pipeline()
            .and_then(|instance| instance.pipeline)
            .and_then(|id| project.pipeline(id))
            .map(|pipeline| {
                pipeline
                    .main_path()
                    .into_iter()
                    .map(|node| node.id)
                    .collect::<HashSet<_>>()
            })
            .unwrap_or_default();
        self.effect_sections.retain(|id, _| live.contains(id));
        for id in live {
            self.effect_sections
                .entry(id)
                .or_insert_with(|| Accordion::new(true));
        }
    }

    fn register_plugin_reset(
        &mut self,
        rect: Rect,
        definition: &PluginInput,
        target: PluginPropertyTarget,
    ) {
        let Ok(default) = definition.ty.default_host(&definition.default) else {
            return;
        };
        let Some(default) = default.evaluate(0.0) else {
            return;
        };
        self.reset_targets
            .push((rect, InspectorResetTarget::Plugin { target, default }));
    }

    fn register_transform_reset(&mut self, rect: Rect, input: &str, plugins: &PluginRegistry) {
        let Some(definition) = plugins
            .effects()
            .find(|definition| definition.role == Some(crate::plugin::EffectRole::VisualTransform))
            .and_then(|definition| {
                definition
                    .inputs
                    .iter()
                    .find(|candidate| candidate.id == input)
            })
        else {
            return;
        };
        let Ok(default) = definition.ty.default_gpu(&definition.default) else {
            return;
        };
        self.reset_targets.push((
            rect,
            InspectorResetTarget::Transform {
                input: input.to_string(),
                default,
            },
        ));
    }

    fn register_model3d_clip_reset(&mut self, rect: Rect, input: &str, default: GpuValue) {
        self.reset_targets.push((
            rect,
            InspectorResetTarget::Model3dClip {
                input: input.to_string(),
                default,
            },
        ));
    }

    fn apply_reset_target(
        &mut self,
        target: InspectorResetTarget,
        project: &mut Project,
        timeline: &mut TimelineState,
    ) -> bool {
        self.clear_editor_focus();

        match target {
            InspectorResetTarget::Plugin { target, default } => match (target, default) {
                (PluginPropertyTarget::Generator(input), crate::project::HostValue::Gpu(value)) => {
                    timeline.set_generator_value(&input, value);
                    true
                }
                (PluginPropertyTarget::Generator(input), value) => {
                    timeline.set_generator_host_value(&input, value);
                    true
                }
                (
                    PluginPropertyTarget::Pipeline { node, input },
                    crate::project::HostValue::Gpu(value),
                ) => timeline.set_pipeline_input_value(project, node, &input, value),
                (PluginPropertyTarget::Pipeline { node, input }, value) => {
                    timeline.set_pipeline_host_input_value(project, node, &input, value)
                }
            },
            InspectorResetTarget::Speed => {
                if timeline.selected_speed(project).is_none() {
                    return false;
                }
                timeline.set_selected_speed(project, 1.0);
                true
            }
            InspectorResetTarget::Volume => {
                if timeline.selected_clip_volume().is_none() {
                    return false;
                }
                timeline.set_selected_clip_volume(1.0);
                true
            }
            InspectorResetTarget::Transform { input, default } => {
                if timeline.transform_value(&input).is_none() {
                    return false;
                }
                timeline.set_transform_value(&input, default);
                true
            }
            InspectorResetTarget::Model3dClip { input, default } => {
                if timeline.selected_model3d_value(project, &input).is_none() {
                    return false;
                }
                timeline.set_selected_model3d_value(project, &input, default);
                true
            }
            InspectorResetTarget::Opacity => {
                if timeline.selected_opacity().is_none() {
                    return false;
                }
                timeline.set_selected_opacity(1.0);
                true
            }
            InspectorResetTarget::BlendMode => {
                if timeline.selected_blend_mode().is_none() {
                    return false;
                }
                timeline.set_selected_blend_mode(0);
                true
            }
            InspectorResetTarget::AlphaBlendMode => {
                if timeline.selected_alpha_blend_mode().is_none() {
                    return false;
                }
                timeline.set_selected_alpha_blend_mode(0);
                true
            }
        }
    }

    fn reset_number_component(
        &mut self,
        target: &InspectorNumberTarget,
        row_default: Option<&InspectorResetTarget>,
        project: &mut Project,
        timeline: &mut TimelineState,
    ) -> bool {
        match target {
            InspectorNumberTarget::Transform {
                input, component, ..
            } => {
                let Some(InspectorResetTarget::Transform {
                    input: reset_input,
                    default,
                }) = row_default
                else {
                    return false;
                };
                if reset_input != input {
                    return false;
                }
                let Some(current) = timeline.transform_value(input) else {
                    return false;
                };
                let Some(value) = reset_gpu_component(current, *default, *component) else {
                    return false;
                };
                timeline.set_transform_value(input, value);
                true
            }
            InspectorNumberTarget::Model3dClip { input, component } => {
                let Some(InspectorResetTarget::Model3dClip {
                    input: reset_input,
                    default,
                }) = row_default
                else {
                    return false;
                };
                if reset_input != input {
                    return false;
                }
                let Some(current) = timeline.selected_model3d_value(project, input) else {
                    return false;
                };
                let Some(value) = reset_gpu_component(current, *default, *component) else {
                    return false;
                };
                timeline.set_selected_model3d_value(project, input, value);
                true
            }
            InspectorNumberTarget::Plugin {
                target,
                component: Some(component),
                ..
            } => {
                let Some(InspectorResetTarget::Plugin {
                    target: reset_target,
                    default: crate::project::HostValue::Gpu(default),
                }) = row_default
                else {
                    return false;
                };
                if reset_target != target {
                    return false;
                }
                let Some(current) = target.value(project, timeline) else {
                    return false;
                };
                let Some(value) = reset_gpu_component(current, *default, *component) else {
                    return false;
                };
                target.set_value(project, timeline, value)
            }
            _ => false,
        }
    }

    pub fn pointer_middle_pressed(
        &mut self,
        rect: Rect,
        point: [f32; 2],
        input: InspectorPointerContext<'_>,
    ) -> bool {
        if !rect.contains(point) {
            return false;
        }
        let local_point = [point[0] - rect.x, point[1] - rect.y];
        let row_default = self
            .reset_targets
            .iter()
            .rev()
            .find_map(|(rect, target)| rect.contains(local_point).then_some(target.clone()));
        if let Some(number_target) = self.controls.numbers.target_at(rect, point) {
            if self.reset_number_component(
                &number_target,
                row_default.as_ref(),
                input.project,
                input.timeline,
            ) {
                return true;
            }
        }
        let Some(target) = row_default else {
            return false;
        };
        self.apply_reset_target(target, input.project, input.timeline)
    }

    fn rects(
        &self,
        rect: Rect,
        project: &Project,
        timeline: &TimelineState,
        plugins: &PluginRegistry,
    ) -> InspectorRects {
        let content = kama_ui::layout::scrolled_content(rect, self.scroll_y);
        let start = rect.y + PANEL_HEADER_H - self.scroll_y;
        let effect_rows = timeline
            .selected_pipeline()
            .and_then(|instance| instance.pipeline)
            .and_then(|id| project.pipeline(id))
            .map(|pipeline| {
                vec![
                    effect_control_rows(
                        rect.width,
                        pipeline,
                        plugins,
                        project,
                        timeline,
                        &self.effect_sections,
                    )
                    .1,
                ]
            });
        let specs = [
            (
                (timeline.selected_generator().is_some()
                    || timeline.selected_speed(project).is_some()
                    || timeline.selected_clip_volume().is_some())
                .then(|| source_section_rows(project, timeline, plugins)),
                self.source_section.open_amount(),
            ),
            (
                timeline
                    .can_assign_pipeline()
                    .then(|| pipeline_section_rows(timeline)),
                self.pipeline_section.open_amount(),
            ),
            (effect_rows, self.controls_section.open_amount()),
            (
                timeline
                    .selected_model3d_value(project, "position")
                    .is_some()
                    .then(model3d_clip_section_rows),
                self.model3d_clip_section.open_amount(),
            ),
            (
                timeline
                    .selected_pipeline()
                    .is_some_and(|pipeline| pipeline.transform().is_some())
                    .then(transform_section_rows),
                self.transform_section.open_amount(),
            ),
            (
                (timeline.selected_pipeline_kind() != PipelineKind::Audio)
                    .then(compositing_section_rows),
                self.compositing_section.open_amount(),
            ),
        ];
        let summary = selection_summary_rects(content, start, timeline).0;
        let (sections, end) = measure_inspector_sections(rect, summary.bottom(), &specs);
        let mut sections = sections.into_iter();
        InspectorRects {
            content,
            content_height: (end + self.scroll_y - rect.y + INSPECTOR_SECTION_PAD).max(0.0),
            source: sections.next().expect("source section"),
            pipeline: sections.next().expect("pipeline section"),
            effects: sections.next().expect("effects section"),
            model3d: sections.next().expect("3D model section"),
            transform: sections.next().expect("transform section"),
            compositing: sections.next().expect("compositing section"),
        }
    }

    pub fn build(
        &mut self,
        ctx: &mut kama_ui::BuildCtx,
        rect: Rect,
        view: InspectorBuildContext<'_>,
    ) {
        let InspectorBuildContext {
            project,
            timeline,
            media_selection,
            plugins,
            icons,
        } = view;
        let chevron = icons.get(AppIcon::Chevron);
        self.sync_selection(project, timeline);
        self.sync_effect_sections(project, timeline);
        self.last_rect = Some(rect);
        self.controls.clear_layout();
        self.eq_scroll_rects.clear();
        self.vector_link_rects.clear();
        self.reset_targets.clear();
        self.controls.color_rect = None;

        let rect = Rect::new(0.0, 0.0, rect.width, rect.height);
        kama_ui::ui!(ctx, {
            Rect("inspector-bg", rect) {
                fill: theme::panel();
            }
        });
        if let Some((media_id, media_stream)) = media_selection {
            let Some(asset) = project.media(media_id) else {
                return;
            };
            if self.media_asset != Some(asset.id) {
                self.media_asset = Some(asset.id);
                self.scroll_y = 0.0;
            }
            self.content_height = build_media_inspector(
                ctx,
                rect,
                asset,
                media_stream,
                (
                    &self.media_general_section,
                    &self.media_video_section,
                    &self.media_audio_section,
                    &self.media_model_section,
                ),
                chevron,
                self.scroll_y,
            );
            let content = kama_ui::layout::scrolled_content(rect, self.scroll_y);
            let _media_layout = media_inspector_rects(
                content,
                asset,
                media_stream,
                MediaInspectorSections {
                    general: &self.media_general_section,
                    video: &self.media_video_section,
                    audio: &self.media_audio_section,
                    model: &self.media_model_section,
                },
                rect.y + PANEL_HEADER_H - self.scroll_y,
            );
            let viewport = rect.height.max(1.0);
            self.scroll_y = self.scroll_y.min((self.content_height - viewport).max(0.0));
            return;
        }
        self.media_asset = None;
        let Some(title) = inspector_title(timeline) else {
            self.scroll_y = 0.0;
            self.content_height = 0.0;
            let empty_slot = kama_ui::layout::column(
                rect,
                &[
                    kama_ui::layout::Item::height(40.0),
                    kama_ui::layout::Item::fill(),
                ],
                0.0,
                0.0,
                kama_ui::Align::Start,
                None,
            )[0];
            let empty = kama_ui::layout::row(
                empty_slot,
                &[
                    kama_ui::layout::Item::width(12.0),
                    kama_ui::layout::Item::fill(),
                    kama_ui::layout::Item::width(12.0),
                ],
                0.0,
                0.0,
                kama_ui::Align::Start,
            )[1];
            kama_ui::ui!(ctx, {
                Rect("inspector-empty", empty) {
                    font_size: 10.5; text_color: theme::muted(); text: "Select a clip or track to edit it.";
                }
            });
            return;
        };
        panel_title(ctx, "inspector-title", rect, &title, self.scroll_y);

        let layout = self.rects(rect, project, timeline, plugins);
        let content_rect = layout.content;
        self.build_selection_summary(
            ctx,
            content_rect,
            timeline,
            rect.y + PANEL_HEADER_H - self.scroll_y,
        );

        if let Some(body) = layout.source.and_then(|section| {
            build_inspector_section(
                ctx,
                &self.source_section,
                section,
                "Source",
                "source",
                chevron,
            )
        }) {
            ctx.with_clip(body, |ctx| {
                self.build_source_content(
                    ctx,
                    body,
                    project,
                    timeline,
                    (plugins, chevron, icons, body.y),
                );
            });
        }

        if let Some(body) = layout.pipeline.and_then(|section| {
            build_inspector_section(
                ctx,
                &self.pipeline_section,
                section,
                "Effect Pipeline",
                "pipeline",
                chevron,
            )
        }) {
            ctx.with_clip(body, |ctx| {
                let mut body_cursor = body.y;
                if !timeline.can_assign_pipeline() {
                    value_row(
                        ctx,
                        body,
                        body_cursor,
                        "Pipeline",
                        "Not available for this layer",
                    );
                } else {
                    draw_pipeline_selector(
                        ctx,
                        body,
                        body_cursor,
                        self.pipeline_id,
                        &mut self.pipeline_name,
                    );
                }
                body_cursor += ROW_H;
                if timeline.can_assign_pipeline() {
                    let buttons = inspector_pipeline_buttons(body, body_cursor);
                    for (index, label) in ["+ Effect", "Unique", "Graph"].into_iter().enumerate() {
                        let fill = if index == 2 {
                            theme::focused()
                        } else {
                            theme::control()
                        };
                        Button::build_filled(
                            ctx,
                            format!("inspector-pipeline-action-{index}"),
                            buttons[index],
                            label,
                            fill,
                            crate::widgets::component_style(),
                        );
                    }
                }
            });
        }

        if let Some(body) = layout.effects.and_then(|section| {
            build_inspector_section(
                ctx,
                &self.controls_section,
                section,
                "Effects",
                "effects",
                chevron,
            )
        }) {
            ctx.with_clip(body, |ctx| {
                if let Some(shared) = timeline
                    .selected_pipeline()
                    .and_then(|instance| instance.pipeline)
                    .and_then(|id| project.pipeline(id))
                {
                    let properties = effect_property_rect(body);
                    for (offset, row) in effect_control_rows(
                        body.width,
                        shared,
                        plugins,
                        project,
                        timeline,
                        &self.effect_sections,
                    )
                    .0
                    {
                        let y = body.y + offset;
                        match row {
                            EffectControlRow::Header {
                                node,
                                body: node_body,
                            } => {
                                if let Some(node_section) = self.effect_sections.get(&node.id) {
                                    draw_effect_header(
                                        ctx,
                                        (body, y),
                                        node,
                                        node_section,
                                        node_body,
                                        (icons, plugins),
                                        (
                                            timeline
                                                .pipeline_input_value(project, node.id, "enabled")
                                                .and_then(GpuValue::bool)
                                                .unwrap_or(true),
                                            keyframe_control(
                                                icons,
                                                timeline.pipeline_input_has_keyframe(
                                                    project, node.id, "enabled",
                                                ),
                                                timeline.pipeline_input_has_keyframes(
                                                    project, node.id, "enabled",
                                                ),
                                            ),
                                        ),
                                    );
                                }
                            }
                            EffectControlRow::EmptyParameters => {
                                value_row(ctx, properties, y, "Parameters", "None");
                            }
                            EffectControlRow::EmptyPipeline => {
                                value_row(ctx, body, y, "Effects", "No effects - use + Effect");
                            }
                            EffectControlRow::GraphicEq { node } => {
                                if let Some(definition) =
                                    plugin_node_input(plugins, &node.node_type, "band_values")
                                {
                                    self.register_plugin_reset(
                                        Rect::new(
                                            properties.x,
                                            y,
                                            properties.width,
                                            ROW_H + GRAPHIC_EQ_H,
                                        ),
                                        definition,
                                        PluginPropertyTarget::Pipeline {
                                            node: node.id,
                                            input: "band_values".into(),
                                        },
                                    );
                                }
                                self.build_graphic_eq(
                                    ctx, properties, y, node, project, timeline, icons,
                                );
                            }
                            EffectControlRow::Input { node, input } => {
                                let keyframe = keyframe_control(
                                    icons,
                                    timeline.pipeline_input_has_keyframe(project, node.id, input),
                                    timeline.pipeline_input_has_keyframes(project, node.id, input),
                                );
                                let value = timeline.pipeline_input_value(project, node.id, input);
                                let target = PluginPropertyTarget::Pipeline {
                                    node: node.id,
                                    input: input.into(),
                                };
                                if let Some(definition) =
                                    plugin_node_input(plugins, &node.node_type, input)
                                {
                                    let reset_rect = if definition.ty == InputType::Angle {
                                        rotation_row_hit(properties, y)
                                    } else {
                                        row_hit(properties, y)
                                    };
                                    self.register_plugin_reset(
                                        reset_rect,
                                        definition,
                                        target.clone(),
                                    );
                                    if !self.build_plugin_property(
                                        ctx,
                                        (properties, y),
                                        definition,
                                        target,
                                        value,
                                        (keyframe, chevron),
                                    ) {
                                        property_row(
                                            ctx,
                                            properties,
                                            y,
                                            &definition.name,
                                            value
                                                .map(format_gpu_value)
                                                .as_deref()
                                                .unwrap_or("Linked"),
                                            keyframe,
                                        );
                                    }
                                } else {
                                    let value = value
                                        .map(format_gpu_value)
                                        .unwrap_or_else(|| "Linked".into());
                                    property_row(
                                        ctx,
                                        properties,
                                        y,
                                        &friendly_name(input),
                                        &value,
                                        keyframe,
                                    );
                                }
                            }
                        }
                    }
                }
            });
        }
        if let Some(body) = layout.model3d.and_then(|section| {
            build_inspector_section(
                ctx,
                &self.model3d_clip_section,
                section,
                "3D Model",
                "model3d-clip",
                chevron,
            )
        }) {
            ctx.with_clip(body, |ctx| {
                let rows =
                    kama_ui::layout::stack(body, body.y, &[ROW_H, ROW_H, ROW_H, ROW_H, ROW_H]);
                for ((label, input, settings, default), row) in [
                    (
                        "Size",
                        "size",
                        ((0.0, 10_000.0), 0.01, 2, ""),
                        GpuValue::Vec3([2.0, 2.0, 2.0]),
                    ),
                    (
                        "Position",
                        "position",
                        ((-10_000.0, 10_000.0), 0.01, 2, ""),
                        GpuValue::Vec3([0.0, 0.0, 0.0]),
                    ),
                    (
                        "Rotation",
                        "rotation",
                        ((-36_000.0, 36_000.0), 0.1, 1, "°"),
                        GpuValue::Vec3([0.0, 0.0, 0.0]),
                    ),
                    (
                        "Scale",
                        "scale",
                        ((-1_000.0, 1_000.0), 0.01, 2, ""),
                        GpuValue::Vec3([1.0, 1.0, 1.0]),
                    ),
                ]
                .into_iter()
                .zip(rows.iter().copied())
                {
                    self.build_model3d_clip_row(
                        ctx,
                        (body, row.y, label),
                        project,
                        timeline,
                        input,
                        settings,
                        icons,
                    );
                    self.register_model3d_clip_reset(row_hit(body, row.y), input, default);
                }
                if let Some(shading) = timeline.selected_model3d_shading(project) {
                    let row = rows[4];
                    let (_, combo, _) = property_row_parts(row);
                    ui_text!(
                        ctx,
                        "model3d-clip-shading-label",
                        property_label_rect(row),
                        9.5,
                        theme::text(),
                        "Shading"
                    );
                    self.controls.enums.build(
                        ctx,
                        "model3d-clip-shading",
                        InspectorEnumTarget::Model3dClipShading,
                        (combo, shading.index()),
                        &crate::project::Model3dShading::OPTIONS,
                        (chevron, crate::widgets::component_style()),
                    );
                }
            });
        }

        if let Some(body) = layout.transform.and_then(|section| {
            build_inspector_section(
                ctx,
                &self.transform_section,
                section,
                "Transform",
                "transform",
                chevron,
            )
        }) {
            ctx.with_clip(body, |ctx| {
                let rows =
                    kama_ui::layout::stack(body, body.y, &[ROW_H, ROW_H, ROW_H, ANGLE_ROW_H]);
                let size = project
                    .active_settings()
                    .canvas_size
                    .map(|value| value as f32);
                for ((label, input, display), row) in
                    TRANSFORM_VECTOR_ROWS.into_iter().zip(rows.iter().copied())
                {
                    let extent = if display == TransformNumberDisplay::PositionPixels {
                        size
                    } else {
                        [1.0; 2]
                    };
                    self.build_transform_number_row(
                        ctx,
                        (body, row.y, label),
                        timeline,
                        (input, display, extent),
                        icons,
                    );
                    self.register_transform_reset(row_hit(body, row.y), input, plugins);
                }
                let rotation = timeline
                    .transform_value("rotation")
                    .and_then(GpuValue::f32)
                    .unwrap_or(0.0);
                self.build_rotation_number_row(
                    ctx,
                    body,
                    rows[3].y,
                    rotation,
                    keyframe_control(
                        icons,
                        timeline.transform_has_keyframe("rotation"),
                        timeline.transform_has_keyframes("rotation"),
                    ),
                );
                self.register_transform_reset(
                    rotation_row_hit(body, rows[3].y),
                    "rotation",
                    plugins,
                );
            });
        }

        if let Some(body) = layout.compositing.and_then(|section| {
            build_inspector_section(
                ctx,
                &self.compositing_section,
                section,
                "Compositing",
                "compositing",
                chevron,
            )
        }) {
            ctx.with_clip(body, |ctx| {
                let rows = kama_ui::layout::stack(body, body.y, &[ROW_H, ROW_H, ROW_H]);
                self.build_number_property(
                    ctx,
                    (body, rows[0].y, "Opacity"),
                    InspectorNumberTarget::Opacity,
                    timeline.selected_opacity().unwrap_or(1.0) as f64 * 100.0,
                    ((0.0, 100.0), 0.5, 1, "%"),
                    Some(keyframe_control(
                        icons,
                        timeline.selected_opacity_has_keyframe(),
                        timeline.selected_opacity_has_keyframes(),
                    )),
                );
                self.reset_targets
                    .push((row_hit(body, rows[0].y), InspectorResetTarget::Opacity));
                let blend_mode = timeline.selected_blend_mode().unwrap_or(BlendMode::Normal);
                let blend_options = BlendMode::ALL.map(|mode| mode.label().to_string());
                let selected = BlendMode::ALL
                    .iter()
                    .position(|mode| *mode == blend_mode)
                    .unwrap_or(0);
                self.build_enum_property(
                    ctx,
                    (body, rows[1].y, "Blend Mode"),
                    InspectorEnumTarget::BlendMode,
                    selected,
                    &blend_options,
                    (
                        keyframe_control(
                            icons,
                            timeline.selected_blend_mode_has_keyframe(),
                            timeline.selected_blend_mode_has_keyframes(),
                        ),
                        chevron,
                    ),
                );
                self.reset_targets
                    .push((row_hit(body, rows[1].y), InspectorResetTarget::BlendMode));

                let alpha_blend_mode = timeline
                    .selected_alpha_blend_mode()
                    .unwrap_or(AlphaBlendMode::SourceOver);
                let alpha_blend_options = AlphaBlendMode::ALL.map(|mode| mode.label().to_string());
                let alpha_selected = AlphaBlendMode::ALL
                    .iter()
                    .position(|mode| *mode == alpha_blend_mode)
                    .unwrap_or(0);
                self.build_enum_property(
                    ctx,
                    (body, rows[2].y, "Alpha Blend"),
                    InspectorEnumTarget::AlphaBlendMode,
                    alpha_selected,
                    &alpha_blend_options,
                    (
                        keyframe_control(
                            icons,
                            timeline.selected_alpha_blend_mode_has_keyframe(),
                            timeline.selected_alpha_blend_mode_has_keyframes(),
                        ),
                        chevron,
                    ),
                );
                self.reset_targets.push((
                    row_hit(body, rows[2].y),
                    InspectorResetTarget::AlphaBlendMode,
                ));
            });
        }

        self.content_height = layout.content_height;
        let viewport = rect.height.max(1.0);
        self.scroll_y = self
            .scroll_y
            .clamp(0.0, (self.content_height - viewport).max(0.0));
        self.build_value_context_menu(ctx, rect, project, timeline, icons);
    }

    fn build_color_property(
        &mut self,
        ctx: &mut kama_ui::BuildCtx,
        row: (Rect, f32, &str),
        target: InspectorColorTarget,
        color: [f32; 4],
        keyframe: KeyframeControl,
    ) {
        let (rect, y, label) = row;
        color_property_row(ctx, rect, y, label, color, keyframe);
        self.build_color_picker(ctx, rect, y, target, color, "inspector-color-picker");
    }

    fn build_gradient_stop_color(
        &mut self,
        ctx: &mut kama_ui::BuildCtx,
        rect: Rect,
        y: f32,
        index: usize,
        color: [f32; 4],
    ) {
        gradient_stop_row(ctx, rect, y, index, color);
        self.build_color_picker(
            ctx,
            rect,
            y,
            InspectorColorTarget::GradientStop(index),
            color,
            "inspector-gradient-color-picker",
        );
    }

    fn build_color_picker(
        &mut self,
        ctx: &mut kama_ui::BuildCtx,
        rect: Rect,
        y: f32,
        target: InspectorColorTarget,
        color: [f32; 4],
        id: &str,
    ) {
        if self.controls.color_target.as_ref() != Some(&target) {
            return;
        }
        let Some(panel) = self.last_rect else {
            return;
        };
        let bounds = Rect::new(0.0, 0.0, panel.width, panel.height);
        let swatch = color_swatch_rect(rect, y);
        self.controls.color_rect = Some(swatch);
        self.controls.color_picker.set_linear(color);
        self.controls.color_picker.build_in(
            ctx,
            id,
            swatch,
            bounds,
            crate::widgets::component_style(),
        );
    }

    fn build_plugin_property(
        &mut self,
        ctx: &mut kama_ui::BuildCtx,
        row: (Rect, f32),
        definition: &PluginInput,
        target: PluginPropertyTarget,
        value: Option<GpuValue>,
        chrome: (KeyframeControl, IconId),
    ) -> bool {
        let (rect, y) = row;
        let (keyframe, chevron) = chrome;
        match definition.ty {
            InputType::Text | InputType::Vec2Array | InputType::F32List => return false,
            InputType::Color => self.build_color_property(
                ctx,
                (rect, y, &definition.name),
                InspectorColorTarget::Plugin(target.clone()),
                value
                    .and_then(GpuValue::color)
                    .unwrap_or([0.0, 0.0, 0.0, 1.0]),
                keyframe,
            ),
            InputType::Bool => {
                let options = ["Off", "On"];
                let selected = (value.and_then(|value| value.numeric(None)).unwrap_or(0.0)
                    as usize)
                    .min(options.len() - 1);
                self.build_enum_property(
                    ctx,
                    (rect, y, &definition.name),
                    InspectorEnumTarget::Plugin {
                        target,
                        boolean: true,
                    },
                    selected,
                    &options,
                    (keyframe, chevron),
                );
            }
            InputType::Enum if !definition.options.is_empty() => {
                let selected = value.and_then(GpuValue::enum_index).unwrap_or(0) as usize;
                let selected = selected.min(definition.options.len().saturating_sub(1));
                self.build_enum_property(
                    ctx,
                    (rect, y, &definition.name),
                    InspectorEnumTarget::Plugin {
                        target,
                        boolean: false,
                    },
                    selected,
                    &definition.options,
                    (keyframe, chevron),
                );
            }
            InputType::Vec2 | InputType::Vec2i | InputType::Vec3 | InputType::Vec4 => {
                if let Some(value) = value {
                    self.build_plugin_vector_property(
                        ctx,
                        (rect, y, &definition.name),
                        target,
                        value,
                        (
                            (
                                (
                                    definition.min.map_or(f64::NEG_INFINITY, f64::from),
                                    definition.max.map_or(f64::INFINITY, f64::from),
                                ),
                                definition.step.map_or(0.02, f64::from),
                                definition.precision.unwrap_or(3),
                                &definition.suffix,
                            ),
                            keyframe,
                        ),
                    );
                } else {
                    property_row(ctx, rect, y, &definition.name, "- ", keyframe);
                }
            }
            InputType::Angle => self.build_angle_property(
                ctx,
                (rect, y, &definition.name),
                target,
                value.and_then(GpuValue::f32).unwrap_or(0.0),
                keyframe,
            ),
            InputType::F32 | InputType::I32 | InputType::U32 | InputType::Enum => {
                let integer = matches!(
                    definition.ty,
                    InputType::I32 | InputType::U32 | InputType::Enum
                );
                self.build_number_property(
                    ctx,
                    (rect, y, &definition.name),
                    InspectorNumberTarget::Plugin {
                        target,
                        component: None,
                        percent: false,
                    },
                    value.and_then(|value| value.numeric(None)).unwrap_or(0.0),
                    (
                        (
                            definition.min.map_or(f64::NEG_INFINITY, f64::from),
                            definition.max.map_or(f64::INFINITY, f64::from),
                        ),
                        definition
                            .step
                            .map_or(if integer { 1.0 } else { 0.02 }, f64::from),
                        definition.precision.unwrap_or(if integer { 0 } else { 3 }),
                        &definition.suffix,
                    ),
                    Some(keyframe),
                );
            }
        }
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn build_graphic_eq(
        &mut self,
        ctx: &mut kama_ui::BuildCtx,
        rect: Rect,
        y: f32,
        node: &crate::effects::EffectNode,
        project: &Project,
        timeline: &TimelineState,
        icons: Icons,
    ) {
        const BAND_COUNTS: [usize; 5] = [3, 5, 10, 15, 31];
        const MIN_SLOT_W: f32 = 28.0;
        let mode = timeline
            .pipeline_input_value(project, node.id, "band_count")
            .and_then(GpuValue::enum_index)
            .unwrap_or(2) as usize;
        let count = BAND_COUNTS[mode.min(BAND_COUNTS.len() - 1)];
        let values = timeline
            .pipeline_host_input_value(project, node.id, "band_values")
            .and_then(|value| match value {
                crate::project::HostValue::F32List(values) => Some(values),
                _ => None,
            })
            .unwrap_or_default();
        let keyframe = keyframe_control(
            icons,
            timeline.pipeline_host_input_has_keyframe(project, node.id, "band_values"),
            timeline.pipeline_host_input_has_keyframes(project, node.id, "band_values"),
        );
        property_row(
            ctx,
            rect,
            y,
            "Band Values",
            &format!("{count} bands"),
            keyframe,
        );

        let eq_rows = kama_ui::layout::stack(rect, y, &[ROW_H, 5.0, GRAPHIC_EQ_H - 10.0]);
        let graph = kama_ui::layout::row(
            eq_rows[2],
            &[
                kama_ui::layout::Item::width(5.0),
                kama_ui::layout::Item::fill(),
                kama_ui::layout::Item::width(5.0),
            ],
            0.0,
            0.0,
            kama_ui::Align::Start,
        )[1];
        let layout = build_graphic_eq_controls(
            ctx,
            &mut self.controls.sliders,
            (("graphic-eq-bg", node.id), ("graphic-eq-zero", node.id)),
            GraphicEqBuild {
                viewport: graph,
                count,
                scroll: *self.eq_scroll.get(&node.id).unwrap_or(&0.0),
                values: &values,
                min_slot_width: MIN_SLOT_W,
                radius: 5.0,
                zero_inset: 4.0,
                enabled: true,
                style: crate::widgets::component_style(),
            },
            |index| (node.id, index),
            |index| format!("graphic-eq-{}-{index}", node.id),
        );
        self.eq_scroll.insert(node.id, layout.scroll);
        self.eq_scroll_rects
            .insert(node.id, (graph, layout.max_scroll));
        for index in 0..count {
            let slider = graphic_eq_slider_rect(layout, index);
            if graphic_eq_visible_slider(layout, slider) {
                ui_text!(
                    ctx,
                    ("graphic-eq-frequency", node.id, index),
                    kama_ui::layout::fit_column_at(
                        slider,
                        [slider.x, layout.slider_bottom + 4.0],
                        slider.width,
                        &[kama_ui::layout::Item::height(16.0)],
                        0.0,
                        0.0,
                    )
                    .1[0],
                    8.0,
                    theme::muted(),
                    eq_frequency_label(index, count)
                );
            }
        }
    }

    fn build_source_content(
        &mut self,
        ctx: &mut kama_ui::BuildCtx,
        rect: Rect,
        project: &Project,
        timeline: &TimelineState,
        view: (&PluginRegistry, IconId, Icons, f32),
    ) -> f32 {
        let (plugins, chevron, icons, start_y) = view;
        let speed = timeline.selected_speed(project);
        let generator = timeline.selected_generator();

        let input_rows = match generator {
            Some(GeneratorSource::Plugin { .. }) => visible_generator_inputs(timeline, plugins),
            _ => None,
        };
        let volume = timeline.selected_clip_volume();
        let mut heights = Vec::new();
        if speed.is_some() {
            heights.push(ROW_H);
        }
        if volume.is_some() {
            heights.push(ROW_H);
        }
        match generator {
            Some(GeneratorSource::Plugin { .. }) => {
                if let Some(inputs) = input_rows.as_ref() {
                    heights.extend(
                        inputs
                            .iter()
                            .map(|input| generator_input_height(timeline, input)),
                    );
                } else {
                    heights.push(ROW_H);
                }
            }
            Some(GeneratorSource::Wasm { .. }) => heights.extend([ROW_H; 3]),
            None => {}
        }
        let rows = kama_ui::layout::stack(rect, start_y, &heights);
        let mut row_index = 0;

        if let Some(speed) = speed {
            let row = rows[row_index];
            row_index += 1;
            self.build_number_property(
                ctx,
                (rect, row.y, "Speed"),
                InspectorNumberTarget::Speed,
                speed as f64 * 100.0,
                ((1.0, 10_000.0), 0.5, 1, "%"),
                None,
            );
            self.reset_targets
                .push((row_hit(rect, row.y), InspectorResetTarget::Speed));
        }

        if let Some(volume) = volume {
            let row = rows[row_index];
            row_index += 1;
            self.build_number_property(
                ctx,
                (rect, row.y, "Volume"),
                InspectorNumberTarget::Volume,
                volume as f64 * 100.0,
                ((0.0, 100.0), 0.5, 1, "%"),
                None,
            );
            self.reset_targets
                .push((row_hit(rect, row.y), InspectorResetTarget::Volume));
        }

        match generator {
            Some(GeneratorSource::Plugin { generator_type, .. }) => {
                if let Some(inputs) = input_rows {
                    for input in inputs {
                        let slot = rows[row_index];
                        row_index += 1;
                        let y = slot.y;
                        self.register_plugin_reset(
                            slot,
                            input,
                            PluginPropertyTarget::Generator(input.id.clone()),
                        );
                        let keyframe = keyframe_control(
                            icons,
                            timeline.generator_has_keyframe(&input.id),
                            timeline.generator_has_keyframes(&input.id),
                        );
                        match (input.ty, input.id.as_str()) {
                            (InputType::Text, "text") => editor_property_row(
                                ctx,
                                rect,
                                y,
                                &input.name,
                                &mut self.text,
                                "Text",
                                keyframe,
                            ),
                            (InputType::Text, "font_family") => font_family_property_row(
                                ctx,
                                rect,
                                y,
                                &input.name,
                                timeline
                                    .selected_font_family()
                                    .filter(|family| !family.trim().is_empty())
                                    .as_deref()
                                    .unwrap_or("System fallback"),
                                keyframe,
                                chevron,
                            ),
                            (InputType::Text, _) => {
                                value_row(ctx, rect, y, &input.name, "Host text property");
                            }
                            (InputType::F32List, "colors")
                                if generator_type == BUILTIN_GRADIENT_GENERATOR =>
                            {
                                let colors = selected_gradient_stop_colors(timeline);
                                let stop_rows = kama_ui::layout::stack(
                                    slot,
                                    slot.y,
                                    &vec![ROW_H; colors.len() + 1],
                                );
                                gradient_color_header_row(
                                    ctx,
                                    rect,
                                    stop_rows[0].y,
                                    colors.len(),
                                    keyframe,
                                    icons,
                                );
                                for (index, (color, stop_row)) in colors
                                    .into_iter()
                                    .zip(stop_rows.into_iter().skip(1))
                                    .enumerate()
                                {
                                    self.build_gradient_stop_color(
                                        ctx, rect, stop_row.y, index, color,
                                    );
                                }
                            }
                            (InputType::Vec2Array | InputType::F32List, _) => {
                                let summary = host_value_summary(
                                    timeline.generator_host_value(&input.id),
                                    input.monitor_handle.is_some(),
                                    input.pen_tool,
                                );
                                property_row(ctx, rect, y, &input.name, &summary, keyframe);
                            }
                            _ => {
                                self.build_plugin_property(
                                    ctx,
                                    (rect, y),
                                    input,
                                    PluginPropertyTarget::Generator(input.id.clone()),
                                    timeline.generator_value(&input.id),
                                    (keyframe, chevron),
                                );
                            }
                        }
                    }
                } else {
                    value_row(ctx, rect, rows[row_index].y, "Generator", generator_type);
                }
            }
            Some(GeneratorSource::Wasm {
                plugin_id,
                entry,
                parameters,
                ..
            }) => {
                let wasm_rows = &rows[row_index..row_index + 3];
                value_row(ctx, rect, wasm_rows[0].y, "Plugin", plugin_id);
                value_row(ctx, rect, wasm_rows[1].y, "Entry", entry);
                value_row(
                    ctx,
                    rect,
                    wasm_rows[2].y,
                    "Parameters",
                    &parameters.len().to_string(),
                );
            }
            None => {}
        }

        rows.last().map_or(start_y, |row| row.bottom()) + 4.0
    }

    fn plugin_property_pointer(
        &mut self,
        layout: (Rect, Rect, f32),
        property: (InputType, PluginPropertyTarget),
        pointer: ([f32; 2], ModifiersState),
        project: &mut Project,
        timeline: &mut TimelineState,
    ) -> bool {
        let (panel, rect, y) = layout;
        let (ty, target) = property;
        let (point, modifiers) = pointer;
        let row = if ty == InputType::Angle {
            rotation_row_hit(rect, y)
        } else {
            row_hit(rect, y)
        };
        if !row.contains(point) {
            return false;
        }
        if keyframe_rect(rect, y).contains(point) {
            target.toggle_keyframe(project, timeline);
        } else if ty == InputType::Color {
            let swatch = color_swatch_rect(rect, y);
            if swatch.contains(point) {
                let color = target
                    .value(project, timeline)
                    .and_then(GpuValue::color)
                    .unwrap_or([0.0, 0.0, 0.0, 1.0]);
                self.open_color_picker(
                    panel,
                    swatch,
                    InspectorColorTarget::Plugin(target),
                    color,
                    point,
                    modifiers,
                );
            }
        }
        true
    }

    pub fn pointer_right_pressed(
        &mut self,
        rect: Rect,
        point: [f32; 2],
        project: &Project,
        timeline: &TimelineState,
        plugins: &PluginRegistry,
    ) -> bool {
        if !rect.contains(point) || inspector_title(timeline).is_none() {
            self.context_menu = None;
            return false;
        }
        let local = [point[0] - rect.x, point[1] - rect.y];
        self.context_cursor = local;

        let mut target = self
            .controls
            .numbers
            .target_at(rect, point)
            .map(InspectorContextTarget::Number)
            .or_else(|| {
                self.controls
                    .angles
                    .target_at(rect, point)
                    .map(InspectorContextTarget::Angle)
            })
            .or_else(|| {
                self.controls
                    .enums
                    .target_at(rect, point)
                    .map(InspectorContextTarget::Enum)
            });

        if target.is_none() {
            target = self.reset_targets.iter().rev().find_map(|(hit, reset)| {
                if !hit.contains(local) {
                    return None;
                }
                match reset {
                    InspectorResetTarget::Plugin { target, .. } => {
                        Some(InspectorContextTarget::Plugin(target.clone()))
                    }
                    _ => None,
                }
            });
        }

        if target.is_none() {
            let layout = self.rects(rect, project, timeline, plugins);
            target = [
                (layout.source, InspectorSectionTarget::Source),
                (layout.effects, InspectorSectionTarget::Effects),
                (layout.transform, InspectorSectionTarget::Transform),
                (layout.compositing, InspectorSectionTarget::Compositing),
            ]
            .into_iter()
            .find_map(|(section, kind)| {
                section
                    .filter(|section| section.header.contains(point))
                    .map(|_| InspectorContextTarget::Section(kind))
            });
        }

        let Some(target) = target else {
            self.context_menu = None;
            return false;
        };
        self.clear_editor_focus();
        self.context_menu = Some(InspectorContextMenu {
            point: local,
            target,
        });
        true
    }

    pub fn pointer_pressed(
        &mut self,
        rect: Rect,
        point: [f32; 2],
        input: InspectorPointerContext<'_>,
    ) -> bool {
        let InspectorPointerContext {
            modifiers,
            project,
            timeline,
            media_selection,
            plugins,
        } = input;
        let local_point = [point[0] - rect.x, point[1] - rect.y];
        self.context_cursor = local_point;
        if media_selection.is_none() {
            if let Some(menu) = self.context_menu.clone() {
                let menu_rect =
                    context_menu_rect(Rect::new(0.0, 0.0, rect.width, rect.height), menu.point, 2);
                if let Some(index) = context_menu_hit(menu_rect, local_point, 2) {
                    let paste_enabled = self.context_paste_enabled(&menu.target, project, timeline);
                    self.context_menu = None;
                    match index {
                        0 => {
                            if let Some(value) =
                                self.copy_context_value(&menu.target, project, timeline)
                            {
                                self.value_clipboard = Some(value);
                            }
                        }
                        1 if paste_enabled => {
                            self.paste_context_value(&menu.target, project, timeline);
                        }
                        _ => {}
                    }
                    return true;
                }
                if menu_rect.contains(local_point) {
                    return true;
                }
                self.context_menu = None;
            }
        } else {
            self.context_menu = None;
        }
        if let Some((media_id, media_stream)) = media_selection {
            let over_popup = self.controls.enums.popup_contains(rect, point);
            if !rect.contains(point) && !over_popup {
                return false;
            }
            if self.controls.enums.select_option(rect, point).is_some() {
                return true;
            }
            if self.controls.enums.toggle_at(rect, point).is_some() {
                return true;
            }
            self.controls.enums.close();
            if let Some((target, value)) = self
                .controls
                .numbers
                .pointer_pressed(rect, point, modifiers)
            {
                if let Some(value) = value {
                    self.apply_number_value(&target, value, project, timeline);
                }
                return true;
            }
            let Some(asset) = project.media(media_id).cloned() else {
                return false;
            };
            let local = Rect::new(0.0, 0.0, rect.width, rect.height);
            let point = [point[0] - rect.x, point[1] - rect.y];
            let content = kama_ui::layout::scrolled_content(local, self.scroll_y);
            let layout = media_inspector_rects(
                content,
                &asset,
                media_stream,
                MediaInspectorSections {
                    general: &self.media_general_section,
                    video: &self.media_video_section,
                    audio: &self.media_audio_section,
                    model: &self.media_model_section,
                },
                PANEL_HEADER_H - self.scroll_y,
            );
            for (section, accordion) in [
                (Some(layout.general), &mut self.media_general_section),
                (layout.video, &mut self.media_video_section),
                (layout.audio, &mut self.media_audio_section),
                (layout.model, &mut self.media_model_section),
            ] {
                if section.is_some_and(|section| section.header.contains(point)) {
                    accordion.toggle();
                    break;
                }
            }
            return true;
        }
        if self.handle_color_picker_pointer(rect, point, modifiers, project, timeline) {
            return true;
        }
        let over_popup = self.popup_contains(rect, point);
        if (!rect.contains(point) && !over_popup) || inspector_title(timeline).is_none() {
            self.text.set_focused(false);
            self.clip_start.set_focused(false);
            self.clip_end.set_focused(false);
            self.pipeline_name.set_focused(false);
            return false;
        }
        self.sync_selection(project, timeline);

        let summary_y = rect.y + PANEL_HEADER_H - self.scroll_y;
        if timeline.selected_clip().is_some() {
            let content = self.rects(rect, project, timeline, plugins).content;
            let hit = (0usize..2).find_map(|index| {
                let field = selection_summary_value_rect(content, summary_y, timeline, index);
                field.contains(point).then_some((index, field))
            });
            if let Some((index, field)) = hit {
                self.clear_editor_focus();
                if index == 0 {
                    self.clip_end.set_focused(false);
                    self.clip_start.pointer_pressed(field, point, modifiers);
                } else {
                    self.clip_start.set_focused(false);
                    self.clip_end.pointer_pressed(field, point, modifiers);
                }
                return true;
            }
        }
        self.sync_effect_sections(project, timeline);
        if let Some((target, _)) = hit_local(&self.vector_link_rects, rect, point) {
            if !self.vector_links.insert(target.clone()) {
                self.vector_links.remove(&target);
            }
            return true;
        }
        if let Some((target, index)) = self.controls.enums.select_option(rect, point) {
            self.clear_editor_focus();
            match &target {
                InspectorEnumTarget::Plugin { target, boolean } => {
                    let value = if *boolean {
                        GpuValue::Bool(index != 0)
                    } else {
                        GpuValue::Enum(index as u32)
                    };
                    target.set_value(project, timeline, value);
                }
                InspectorEnumTarget::Model3dClipShading => {
                    timeline.set_selected_model3d_shading(
                        project,
                        crate::project::Model3dShading::from_index(index),
                    );
                }
                InspectorEnumTarget::BlendMode => timeline.set_selected_blend_mode(index),
                InspectorEnumTarget::AlphaBlendMode => {
                    timeline.set_selected_alpha_blend_mode(index)
                }
            }
            return true;
        }
        if self.controls.enums.toggle_at(rect, point).is_some() {
            self.clear_editor_focus();
            return true;
        }
        self.controls.enums.close();
        if let Some((key, value)) = self.controls.sliders.pointer_pressed(rect, point) {
            set_graphic_eq_band(project, timeline, key.0, key.1, value);
            return true;
        }
        if self.controls.numbers.target_at(rect, point).is_some() {
            self.clear_editor_focus();
            if let Some((target, value)) = self
                .controls
                .numbers
                .pointer_pressed(rect, point, modifiers)
            {
                if let Some(value) = value {
                    self.apply_number_value(&target, value, project, timeline);
                }
                return true;
            }
        }
        if self.controls.angles.target_at(rect, point).is_some() {
            self.clear_editor_focus();
            if let Some((target, value)) =
                self.controls.angles.pointer_pressed(rect, point, modifiers)
            {
                if let Some(value) = value {
                    Self::apply_angle_value(&target, value, project, timeline);
                }
                return true;
            }
        }
        let layout = self.rects(rect, project, timeline, plugins);
        for (section, accordion) in [
            (layout.source, &mut self.source_section),
            (layout.pipeline, &mut self.pipeline_section),
            (layout.effects, &mut self.controls_section),
            (layout.model3d, &mut self.model3d_clip_section),
            (layout.transform, &mut self.transform_section),
            (layout.compositing, &mut self.compositing_section),
        ] {
            if section.is_some_and(|section| section.header.contains(point)) {
                accordion.toggle();
                return true;
            }
        }

        if let Some(section) = layout.source {
            if self.source_section.open_amount() > 0.98 {
                let body = inspector_section_content(section);
                if let Some(inputs) = visible_generator_inputs(timeline, plugins) {
                    let has_speed = timeline.selected_speed(project).is_some();
                    let input_offset = if has_speed { 1 } else { 0 };
                    let mut heights = Vec::with_capacity(inputs.len() + input_offset);
                    if has_speed {
                        heights.push(ROW_H);
                    }
                    heights.extend(
                        inputs
                            .iter()
                            .map(|input| generator_input_height(timeline, input)),
                    );
                    let rows = kama_ui::layout::stack(body, body.y, &heights);
                    for (input, slot) in inputs.into_iter().zip(rows.into_iter().skip(input_offset))
                    {
                        let y = slot.y;
                        if input.ty == InputType::Text {
                            let hit = row_hit(body, y);
                            if hit.contains(point) {
                                if keyframe_rect(body, y).contains(point) {
                                    timeline.toggle_generator_keyframe(&input.id);
                                } else if input.id == "text" {
                                    self.pipeline_name.set_focused(false);
                                    self.controls.blur();
                                    self.text.pointer_pressed(
                                        editor_value_rect(body, y),
                                        point,
                                        modifiers,
                                    );
                                } else if input.id == "font_family" {
                                    self.clear_editor_focus();
                                    let field = editor_value_rect(body, y);
                                    if field.contains(point) {
                                        self.pending_action =
                                            Some(InspectorAction::ChooseFont(field));
                                    }
                                }
                                return true;
                            }
                        } else if is_selected_gradient_generator(timeline)
                            && input.id == "colors"
                            && input.ty == InputType::F32List
                        {
                            let colors = selected_gradient_stop_colors(timeline);
                            let stop_rows = kama_ui::layout::stack(
                                slot,
                                slot.y,
                                &vec![ROW_H; colors.len() + 1],
                            );
                            let header_y = stop_rows[0].y;
                            let header = row_hit(body, header_y);
                            if header.contains(point) {
                                if keyframe_rect(body, header_y).contains(point) {
                                    timeline.toggle_generator_keyframe("colors");
                                } else if gradient_stop_add_rect(body, header_y).contains(point) {
                                    add_selected_gradient_stop(timeline);
                                } else if gradient_stop_remove_rect(body, header_y).contains(point)
                                    && remove_selected_gradient_stop(timeline)
                                {
                                    self.controls.color_picker.close();
                                    self.controls.color_target = None;
                                    self.controls.color_rect = None;
                                }
                                return true;
                            }
                            for (index, (color, stop_row)) in colors
                                .into_iter()
                                .zip(stop_rows.into_iter().skip(1))
                                .enumerate()
                            {
                                let hit = row_hit(body, stop_row.y);
                                if hit.contains(point) {
                                    let swatch = color_swatch_rect(body, stop_row.y);
                                    if swatch.contains(point) {
                                        self.open_color_picker(
                                            rect,
                                            swatch,
                                            InspectorColorTarget::GradientStop(index),
                                            color,
                                            point,
                                            modifiers,
                                        );
                                    }
                                    return true;
                                }
                            }
                        } else if self.plugin_property_pointer(
                            (rect, body, y),
                            (input.ty, PluginPropertyTarget::Generator(input.id.clone())),
                            (point, modifiers),
                            project,
                            timeline,
                        ) {
                            return true;
                        }
                    }
                }
            }
        }

        if let Some(section) = layout.pipeline {
            if self.pipeline_section.open_amount() > 0.98 {
                let body = inspector_section_content(section);
                let y = body.y;
                if row_hit(body, y).contains(point) {
                    if !timeline.can_assign_pipeline() {
                        return true;
                    }
                    let (_, name, choose, plus) = pipeline_selector_parts(body, y);
                    if self.pipeline_id.is_some() && name.contains(point) {
                        self.text.set_focused(false);
                        self.controls.blur();
                        self.pipeline_name.pointer_pressed(name, point, modifiers);
                    } else if plus.contains(point) {
                        self.pending_action = Some(InspectorAction::CreatePipeline);
                    } else if choose.contains(point) || name.contains(point) {
                        self.pending_action = Some(InspectorAction::ChoosePipeline(Rect::new(
                            name.x,
                            name.y,
                            choose.right() - name.x,
                            name.height,
                        )));
                    }
                    return true;
                }
                if timeline.can_assign_pipeline() {
                    let buttons = inspector_pipeline_buttons(body, y + ROW_H);
                    if let Some(index) = buttons.iter().position(|button| button.contains(point)) {
                        self.pending_action = Some(match index {
                            0 => InspectorAction::AddEffect,
                            1 => InspectorAction::MakeIndependent,
                            _ => InspectorAction::OpenGraph,
                        });
                        return true;
                    }
                }
            }
        }

        if let Some(section) = layout.effects {
            if self.controls_section.open_amount() > 0.98 {
                enum EffectHit {
                    Move(u64, i32),
                    ToggleEnabled(u64),
                    ToggleKeyframe(u64),
                    Remove(u64),
                    ToggleSection(u64),
                    GraphicEq(u64, bool),
                    Property(f32, InputType, PluginPropertyTarget),
                    Consumed,
                }

                let body = inspector_section_content(section);
                let properties = effect_property_rect(body);
                let hit = timeline
                    .selected_pipeline()
                    .and_then(|instance| instance.pipeline)
                    .and_then(|id| project.pipeline(id))
                    .and_then(|shared| {
                        effect_control_rows(
                            body.width,
                            shared,
                            plugins,
                            project,
                            timeline,
                            &self.effect_sections,
                        )
                        .0
                        .into_iter()
                        .find_map(|(offset, row)| {
                            let row_y = body.y + offset;
                            match row {
                                EffectControlRow::Header { node, .. } => {
                                    if let Some(index) = node_action_rects(body, row_y)
                                        .iter()
                                        .position(|action| action.contains(point))
                                    {
                                        return Some(match index {
                                            0 => EffectHit::Move(node.id, -1),
                                            1 => EffectHit::Move(node.id, 1),
                                            2 => EffectHit::ToggleEnabled(node.id),
                                            3 => EffectHit::ToggleKeyframe(node.id),
                                            _ => EffectHit::Remove(node.id),
                                        });
                                    }
                                    node_header_rect(body, row_y)
                                        .contains(point)
                                        .then_some(EffectHit::ToggleSection(node.id))
                                }
                                EffectControlRow::GraphicEq { node } => Some(EffectHit::GraphicEq(
                                    node.id,
                                    keyframe_rect(properties, row_y).contains(point),
                                )),
                                EffectControlRow::Input { node, input } => {
                                    let target = PluginPropertyTarget::Pipeline {
                                        node: node.id,
                                        input: input.into(),
                                    };
                                    if let Some(definition) =
                                        plugin_node_input(plugins, &node.node_type, input)
                                    {
                                        let row = if definition.ty == InputType::Angle {
                                            rotation_row_hit(properties, row_y)
                                        } else {
                                            row_hit(properties, row_y)
                                        };
                                        row.contains(point).then_some(EffectHit::Property(
                                            row_y,
                                            definition.ty,
                                            target,
                                        ))
                                    } else {
                                        row_hit(properties, row_y)
                                            .contains(point)
                                            .then_some(EffectHit::Consumed)
                                    }
                                }
                                _ => None,
                            }
                        })
                    });

                if let Some(hit) = hit {
                    match hit {
                        EffectHit::Move(node, direction) => {
                            self.pending_action =
                                Some(InspectorAction::MoveEffect(node, direction));
                        }
                        EffectHit::ToggleEnabled(node) => {
                            let enabled = timeline
                                .pipeline_input_value(project, node, "enabled")
                                .and_then(GpuValue::bool)
                                .unwrap_or(true);
                            timeline.set_pipeline_input_value(
                                project,
                                node,
                                "enabled",
                                GpuValue::Bool(!enabled),
                            );
                        }
                        EffectHit::ToggleKeyframe(node) => {
                            timeline.toggle_pipeline_keyframe(project, node, "enabled");
                        }
                        EffectHit::Remove(node) => {
                            self.pending_action = Some(InspectorAction::RemoveEffect(node));
                        }
                        EffectHit::ToggleSection(node) => {
                            if let Some(section) = self.effect_sections.get_mut(&node) {
                                section.toggle();
                            }
                        }
                        EffectHit::GraphicEq(node, true) => {
                            timeline.toggle_pipeline_host_keyframe(project, node, "band_values");
                        }
                        EffectHit::Property(row_y, ty, target) => {
                            self.plugin_property_pointer(
                                (rect, properties, row_y),
                                (ty, target),
                                (point, modifiers),
                                project,
                                timeline,
                            );
                        }
                        EffectHit::GraphicEq(_, false) | EffectHit::Consumed => {}
                    }
                    return true;
                }
            }
        }

        if let Some(section) = layout.model3d {
            if self.model3d_clip_section.open_amount() > 0.98 {
                let body = inspector_section_content(section);
                let rows =
                    kama_ui::layout::stack(body, body.y, &[ROW_H, ROW_H, ROW_H, ROW_H, ROW_H]);
                for (input, row) in ["size", "position", "rotation", "scale"]
                    .into_iter()
                    .zip(rows.into_iter().take(4))
                {
                    if row_hit(body, row.y).contains(point) {
                        if keyframe_rect(body, row.y).contains(point) {
                            timeline.toggle_selected_model3d_keyframe(project, input);
                        }
                        return true;
                    }
                }
            }
        }

        if let Some(section) = layout.transform {
            if self.transform_section.open_amount() > 0.98 {
                let body = inspector_section_content(section);
                let rows =
                    kama_ui::layout::stack(body, body.y, &[ROW_H, ROW_H, ROW_H, ANGLE_ROW_H]);
                for ((_, input, _), row) in
                    TRANSFORM_VECTOR_ROWS.into_iter().zip(rows.iter().copied())
                {
                    if transform_vec2_row_hit(body, row.y, point, timeline, input) {
                        return true;
                    }
                }
                let rotation_y = rows[3].y;
                if keyframe_rect(body, rotation_y).contains(point) {
                    timeline.toggle_transform_keyframe("rotation");
                    return true;
                }
                if rotation_row_hit(body, rotation_y).contains(point) {
                    return true;
                }
            }
        }

        if let Some(section) = layout.compositing {
            if self.compositing_section.open_amount() > 0.98 {
                let body = inspector_section_content(section);
                let rows = kama_ui::layout::stack(body, body.y, &[ROW_H, ROW_H, ROW_H]);
                if row_hit(body, rows[0].y).contains(point) {
                    if keyframe_rect(body, rows[0].y).contains(point) {
                        timeline.toggle_selected_opacity_keyframe();
                    }
                    return true;
                }
                if row_hit(body, rows[1].y).contains(point) {
                    if keyframe_rect(body, rows[1].y).contains(point) {
                        timeline.toggle_selected_blend_mode_keyframe();
                    }
                    return true;
                }
                if row_hit(body, rows[2].y).contains(point) {
                    if keyframe_rect(body, rows[2].y).contains(point) {
                        timeline.toggle_selected_alpha_blend_mode_keyframe();
                    }
                    return true;
                }
            }
        }

        false
    }

    fn handle_color_picker_pointer(
        &mut self,
        rect: Rect,
        point: [f32; 2],
        modifiers: ModifiersState,
        project: &mut Project,
        timeline: &mut TimelineState,
    ) -> bool {
        let Some(swatch) = self.controls.color_rect else {
            return false;
        };
        let bounds = Rect::new(0.0, 0.0, rect.width, rect.height);
        let point = [point[0] - rect.x, point[1] - rect.y];
        let before = self.controls.color_picker.linear();
        let handled = self
            .controls
            .color_picker
            .pointer_pressed_in(swatch, bounds, point, modifiers);
        if handled {
            if self.controls.color_picker.linear() != before {
                if let Some(target) = self.controls.color_target.as_ref() {
                    return apply_color_target(
                        target,
                        project,
                        timeline,
                        self.controls.color_picker.linear(),
                    );
                }
            }
            return true;
        }
        if self.controls.color_target.take().is_some() {
            self.controls.color_rect = None;
        }
        false
    }

    fn open_color_picker(
        &mut self,
        panel: Rect,
        swatch: Rect,
        target: InspectorColorTarget,
        color: [f32; 4],
        point: [f32; 2],
        modifiers: ModifiersState,
    ) {
        self.text.set_focused(false);
        self.clip_start.set_focused(false);
        self.clip_end.set_focused(false);
        self.pipeline_name.set_focused(false);
        self.controls.blur();
        let local_swatch = offset_rect(swatch, -panel.x, -panel.y);
        let bounds = Rect::new(0.0, 0.0, panel.width, panel.height);
        let point = [point[0] - panel.x, point[1] - panel.y];
        self.controls.color_target = Some(target);
        self.controls.color_rect = Some(local_swatch);
        self.controls.color_picker.set_linear(color);
        let _ =
            self.controls
                .color_picker
                .pointer_pressed_in(local_swatch, bounds, point, modifiers);
    }

    pub fn scroll_popup(&self, rect: Rect, point: [f32; 2], delta: [f32; 2]) -> bool {
        self.controls.enums.scroll_popup(rect, point, delta)
    }

    pub fn scroll(&mut self, rect: Rect, point: [f32; 2], delta: [f32; 2]) -> bool {
        if self.scroll_popup(rect, point, delta) {
            return true;
        }
        if !rect.contains(point) {
            return false;
        }
        if let Some((node, max_scroll)) =
            self.eq_scroll_rects
                .iter()
                .find_map(|(node, (local, max))| {
                    let absolute = offset_rect(*local, rect.x, rect.y);
                    absolute.contains(point).then_some((*node, *max))
                })
        {
            if max_scroll > 0.0 {
                let axis = if delta[0].abs() > delta[1].abs() {
                    delta[0]
                } else {
                    delta[1]
                };
                let scroll = self.eq_scroll.entry(node).or_insert(0.0);
                let next = (*scroll - axis).clamp(0.0, max_scroll);
                if (next - *scroll).abs() > 0.001 {
                    *scroll = next;
                    return true;
                }
            }
        }
        let viewport = rect.height.max(1.0);
        if self.content_height <= viewport {
            self.scroll_y = 0.0;
            return false;
        }
        let max_scroll = (self.content_height - viewport).max(0.0);
        self.scroll_y = (self.scroll_y - delta[1]).clamp(0.0, max_scroll);
        true
    }

    pub fn pointer_moved(
        &mut self,
        point: [f32; 2],
        project: &mut Project,
        timeline: &mut TimelineState,
    ) -> bool {
        if self.context_menu.is_some() {
            if let Some(rect) = self.last_rect {
                self.context_cursor = [point[0] - rect.x, point[1] - rect.y];
            }
            return true;
        }
        if let Some((target, value)) = self.controls.angles.pointer_moved(point) {
            Self::apply_angle_value(&target, value, project, timeline);
            return true;
        }
        if let Some((key, value)) = self.controls.sliders.pointer_moved(point) {
            set_graphic_eq_band(project, timeline, key.0, key.1, value);
            return true;
        }
        if let Some((target, value)) = self.controls.numbers.pointer_moved(point) {
            self.apply_number_value(&target, value, project, timeline);
            return true;
        }
        if let (Some(target), Some(swatch), Some(rect)) = (
            self.controls.color_target.as_ref(),
            self.controls.color_rect,
            self.last_rect,
        ) {
            let bounds = Rect::new(0.0, 0.0, rect.width, rect.height);
            let local_point = [point[0] - rect.x, point[1] - rect.y];
            let before = self.controls.color_picker.linear();
            if self
                .controls
                .color_picker
                .pointer_moved_in(swatch, bounds, local_point)
            {
                if self.controls.color_picker.linear() != before {
                    return apply_color_target(
                        target,
                        project,
                        timeline,
                        self.controls.color_picker.linear(),
                    );
                }
                return true;
            }
        }
        self.text.pointer_moved(point)
            || self.clip_start.pointer_moved(point)
            || self.clip_end.pointer_moved(point)
            || self.pipeline_name.pointer_moved(point)
    }

    pub fn is_cursor_lock_dragging(&self) -> bool {
        self.controls.is_cursor_lock_dragging()
    }

    pub fn pointer_released(&mut self) -> bool {
        self.controls.pointer_released()
            | self.text.pointer_released()
            | self.clip_start.pointer_released()
            | self.clip_end.pointer_released()
            | self.pipeline_name.pointer_released()
    }

    fn edit_fields(
        &mut self,
        mut edit_number: impl FnMut(&mut NumberInput) -> Option<f64>,
        mut edit_color: impl FnMut(&mut ColorPicker) -> bool,
        mut edit_text: impl FnMut(&mut TextEdit) -> EditResponse,
        project: &mut Project,
        timeline: &mut TimelineState,
    ) -> bool {
        if let Some((target, value)) = self.controls.angles.edit(&mut edit_number) {
            if let Some(value) = value {
                Self::apply_angle_value(&target, value, project, timeline);
            }
            return true;
        }
        if let Some(target) = self.controls.numbers.editing_target() {
            if let Some(value) = self.controls.numbers.edit(&target, &mut edit_number) {
                self.apply_number_value(&target, value, project, timeline);
            }
            return true;
        }
        if edit_color(&mut self.controls.color_picker) {
            if let Some(target) = self.controls.color_target.as_ref() {
                return apply_color_target(
                    target,
                    project,
                    timeline,
                    self.controls.color_picker.linear(),
                );
            }
            return true;
        }
        let pipeline_id = self.pipeline_id;
        for (field, kind) in [
            (&mut self.text, InspectorTextField::Text),
            (&mut self.clip_start, InspectorTextField::ClipStart),
            (&mut self.clip_end, InspectorTextField::ClipEnd),
            (&mut self.pipeline_name, InspectorTextField::PipelineName),
        ] {
            let response = edit_text(field);
            if !response.handled {
                continue;
            }
            if response.changed {
                match kind {
                    InspectorTextField::Text => {
                        timeline.set_selected_text(field.text().to_string())
                    }
                    InspectorTextField::ClipStart => {
                        if let Some(value) = parse_timecode(
                            field.text(),
                            project.active_settings().frame_rate as f32,
                        ) {
                            timeline.set_selected_clip_start(value);
                        }
                    }
                    InspectorTextField::ClipEnd => {
                        if let Some(value) = parse_timecode(
                            field.text(),
                            project.active_settings().frame_rate as f32,
                        ) {
                            timeline.set_selected_clip_end(value);
                        }
                    }
                    InspectorTextField::PipelineName => {
                        if let Some(id) = pipeline_id {
                            project.rename_pipeline(id, field.text());
                        }
                    }
                }
            }
            return true;
        }
        false
    }

    pub fn handle_key(
        &mut self,
        event: &KeyEvent,
        modifiers: ModifiersState,
        project: &mut Project,
        timeline: &mut TimelineState,
    ) -> bool {
        self.edit_fields(
            |input| input.handle_key(event, modifiers),
            |picker| picker.handle_key(event, modifiers),
            |editor| editor.handle_key(event, modifiers),
            project,
            timeline,
        )
    }

    pub fn handle_ime(
        &mut self,
        event: &Ime,
        project: &mut Project,
        timeline: &mut TimelineState,
    ) -> bool {
        self.edit_fields(
            |input| input.handle_ime(event),
            |picker| picker.handle_ime(event),
            |editor| editor.handle_ime(event),
            project,
            timeline,
        )
    }

    pub fn ime_area(
        &self,
        rect: Rect,
        project: &Project,
        timeline: &TimelineState,
        plugins: &PluginRegistry,
    ) -> Option<Rect> {
        if let Some(caret) = self.controls.caret_rect(rect) {
            return Some(caret);
        }
        if let Some(swatch) = self.controls.color_rect {
            let bounds = Rect::new(0.0, 0.0, rect.width, rect.height);
            if let Some(caret) = self.controls.color_picker.caret_rect_in(swatch, bounds) {
                return Some(offset_rect(caret, rect.x, rect.y));
            }
        }
        let layout = self.rects(rect, project, timeline, plugins);
        if timeline.selected_clip().is_some() {
            let summary_y = rect.y + PANEL_HEADER_H - self.scroll_y;
            for (editor, index) in [(&self.clip_start, 0usize), (&self.clip_end, 1usize)] {
                if editor.is_focused() {
                    return Some(editor.caret_rect(selection_summary_value_rect(
                        layout.content,
                        summary_y,
                        timeline,
                        index,
                    )));
                }
            }
        }
        if self.source_section.is_open() {
            if let Some(section) = layout.source {
                let body = inspector_section_content(section);
                if let Some(inputs) = visible_generator_inputs(timeline, plugins) {
                    let has_speed = timeline.selected_speed(project).is_some();
                    let input_offset = if has_speed { 1 } else { 0 };
                    let mut heights = Vec::with_capacity(inputs.len() + input_offset);
                    if has_speed {
                        heights.push(ROW_H);
                    }
                    heights.extend(
                        inputs
                            .iter()
                            .map(|input| generator_input_height(timeline, input)),
                    );
                    let rows = kama_ui::layout::stack(body, body.y, &heights);
                    for (input, row) in inputs.into_iter().zip(rows.into_iter().skip(input_offset))
                    {
                        if input.ty == InputType::Text
                            && input.id == "text"
                            && self.text.is_focused()
                        {
                            return Some(self.text.caret_rect(editor_value_rect(body, row.y)));
                        }
                    }
                }
            }
        }

        if self.pipeline_section.is_open() && self.pipeline_name.is_focused() {
            if let Some(section) = layout.pipeline {
                let body = inspector_section_content(section);
                let (_, name, _, _) = pipeline_selector_parts(body, body.y);
                return Some(self.pipeline_name.caret_rect(name));
            }
        }
        None
    }
}

#[derive(Clone, Copy)]
struct InspectorSectionRects {
    header: Rect,
    body: Rect,
    content: Rect,
}

#[derive(Clone, Copy)]
struct InspectorRects {
    content: Rect,
    content_height: f32,
    source: Option<InspectorSectionRects>,
    pipeline: Option<InspectorSectionRects>,
    effects: Option<InspectorSectionRects>,
    model3d: Option<InspectorSectionRects>,
    transform: Option<InspectorSectionRects>,
    compositing: Option<InspectorSectionRects>,
}

fn measure_inspector_sections(
    rect: Rect,
    start: f32,
    specs: &[(Option<Vec<f32>>, f32)],
) -> (Vec<Option<InspectorSectionRects>>, f32) {
    #[derive(Clone, Copy)]
    struct SectionIds {
        header: BlockId,
        body: BlockId,
        content: BlockId,
    }

    if specs.iter().all(|(rows, _)| rows.is_none()) {
        return (vec![None; specs.len()], start);
    }

    let ((root, ids), measured) = kama_ui::measure_layout(rect, |ctx| {
        let mut ids = Vec::with_capacity(specs.len());
        let root = ctx
            .new()
            .overlay()
            .position((0.0, start - rect.y))
            .width(Size::Fill)
            .height(Size::Fit)
            .column()
            .children(|ctx| {
                for (rows, open) in specs {
                    let Some(rows) = rows else {
                        ids.push(None);
                        continue;
                    };
                    let mut header = BlockId::default();
                    let mut body = BlockId::default();
                    let mut content = BlockId::default();
                    ctx.new()
                        .width(Size::Fill)
                        .height(Size::Fit)
                        .column()
                        .children(|ctx| {
                            ctx.new()
                                .width(Size::Fill)
                                .height(Size::Pixels(ACCORDION_H))
                                .row()
                                .children(|ctx| {
                                    ctx.new()
                                        .width(Size::Pixels(7.0))
                                        .height(Size::Fill)
                                        .build();
                                    header = ctx
                                        .new()
                                        .width(Size::Fill)
                                        .height(Size::Pixels(ACCORDION_H - 4.0))
                                        .build();
                                    ctx.new()
                                        .width(Size::Pixels(7.0))
                                        .height(Size::Fill)
                                        .build();
                                })
                                .build();
                            ctx.new()
                                .width(Size::Fill)
                                .height(Size::FitScale(*open))
                                .row()
                                .children(|ctx| {
                                    ctx.new()
                                        .width(Size::Pixels(7.0))
                                        .height(Size::Fill)
                                        .build();
                                    body = ctx
                                        .new()
                                        .width(Size::Fill)
                                        .height(Size::Fill)
                                        .padding(INSPECTOR_SECTION_PAD)
                                        .column()
                                        .children(|ctx| {
                                            content = ctx
                                                .new()
                                                .width(Size::Fill)
                                                .height(Size::Fill)
                                                .children(|ctx| {
                                                    ctx.new()
                                                        .width(Size::Fill)
                                                        .height(Size::Fit)
                                                        .column()
                                                        .children(|ctx| {
                                                            for height in rows {
                                                                ctx.new()
                                                                    .width(Size::Fill)
                                                                    .height(Size::Pixels(*height))
                                                                    .build();
                                                            }
                                                        })
                                                        .build();
                                                })
                                                .build();
                                        })
                                        .build();
                                    ctx.new()
                                        .width(Size::Pixels(7.0))
                                        .height(Size::Fill)
                                        .build();
                                })
                                .build();
                            ctx.new()
                                .width(Size::Fill)
                                .height(Size::Pixels(INSPECTOR_SECTION_GAP))
                                .build();
                        })
                        .build();
                    ids.push(Some(SectionIds {
                        header,
                        body,
                        content,
                    }));
                }
            })
            .build();
        (root, ids)
    });

    let rect_for = |id: BlockId| measured.rect(id).expect("inspector rect");
    let sections = ids
        .into_iter()
        .map(|ids| {
            ids.map(|ids| InspectorSectionRects {
                header: rect_for(ids.header),
                body: rect_for(ids.body),
                content: rect_for(ids.content),
            })
        })
        .collect();
    (sections, rect_for(root).bottom())
}

fn inspector_accordion_body(
    ctx: &mut kama_ui::BuildCtx,
    section: &Accordion,
    body: Rect,
    label: &str,
) {
    section.build_body_rect(
        ctx,
        FormatKey::new(format_args!("inspector-section-{label}")),
        body,
        crate::widgets::component_style(),
    );
}

fn inspector_section_content(layout: InspectorSectionRects) -> Rect {
    layout.content
}

fn build_inspector_section(
    ctx: &mut kama_ui::BuildCtx,
    section: &Accordion,
    layout: InspectorSectionRects,
    title: &str,
    id: &str,
    chevron: IconId,
) -> Option<Rect> {
    accordion_header(ctx, section, layout.header, title, chevron);
    inspector_accordion_body(ctx, section, layout.body, id);
    section.is_visible().then_some(layout.content)
}

fn visible_generator_inputs<'a>(
    timeline: &TimelineState,
    plugins: &'a PluginRegistry,
) -> Option<Vec<&'a PluginInput>> {
    let GeneratorSource::Plugin { generator_type, .. } = timeline.selected_generator()? else {
        return None;
    };
    plugins.generator(generator_type).map(|definition| {
        definition
            .inputs
            .iter()
            .filter(|input| input.is_visible_with(|id| timeline.generator_value(id)))
            .collect()
    })
}

fn source_section_rows(
    project: &Project,
    timeline: &TimelineState,
    plugins: &PluginRegistry,
) -> Vec<f32> {
    let mut rows = match timeline.selected_generator() {
        Some(GeneratorSource::Plugin { .. }) => visible_generator_inputs(timeline, plugins)
            .map(|inputs| {
                inputs
                    .iter()
                    .map(|input| generator_input_height(timeline, input))
                    .collect()
            })
            .unwrap_or_else(|| vec![ROW_H]),
        Some(GeneratorSource::Wasm { .. }) => vec![ROW_H; 3],
        None => Vec::new(),
    };
    if timeline.selected_speed(project).is_some() {
        rows.push(ROW_H);
    }
    if timeline.selected_clip_volume().is_some() {
        rows.push(ROW_H);
    }
    rows
}

fn pipeline_section_rows(timeline: &TimelineState) -> Vec<f32> {
    let mut rows = vec![ROW_H];
    if timeline.can_assign_pipeline() {
        rows.push(30.0);
    }
    rows
}

const INSPECTOR_SECTION_PAD: f32 = 8.0;
const INSPECTOR_SECTION_GAP: f32 = 5.0;
const EFFECT_NODE_HEADER_H: f32 = 26.0;
const EFFECT_NODE_GAP: f32 = 7.0;
const EFFECT_NODE_CONTENT_PAD: f32 = 10.0;

enum EffectControlRow<'a> {
    Header {
        node: &'a crate::effects::EffectNode,
        body: Rect,
    },
    Input {
        node: &'a crate::effects::EffectNode,
        input: &'a str,
    },
    GraphicEq {
        node: &'a crate::effects::EffectNode,
    },
    EmptyParameters,
    EmptyPipeline,
}

fn input_control_height(ty: InputType) -> f32 {
    if ty == InputType::Angle {
        ANGLE_ROW_H
    } else {
        ROW_H
    }
}

fn generator_input_height(timeline: &TimelineState, input: &PluginInput) -> f32 {
    if is_selected_gradient_generator(timeline)
        && input.id == "colors"
        && input.ty == InputType::F32List
    {
        ROW_H + selected_gradient_stop_points(timeline).len() as f32 * ROW_H
    } else {
        input_control_height(input.ty)
    }
}

fn effect_input_visible(
    node: &crate::effects::EffectNode,
    input: &PluginInput,
    project: &Project,
    timeline: &TimelineState,
) -> bool {
    input.is_visible_with(|controller| timeline.pipeline_input_value(project, node.id, controller))
}

const GRAPHIC_EQ_H: f32 = 176.0;

fn is_graphic_eq(plugins: &PluginRegistry, node_type: &str) -> bool {
    plugins
        .audio_effect(node_type)
        .and_then(|definition| definition.view.as_deref())
        == Some("graphic_eq")
}

fn effect_body_rows<'a>(
    node: &'a crate::effects::EffectNode,
    plugins: &'a PluginRegistry,
    project: &Project,
    timeline: &TimelineState,
) -> (Vec<(f32, EffectControlRow<'a>)>, f32) {
    let mut controls = Vec::new();
    let mut heights = Vec::new();

    if is_graphic_eq(plugins, &node.node_type) {
        if node.inputs.contains_key("band_count") {
            controls.push(EffectControlRow::Input {
                node,
                input: "band_count",
            });
            heights.push(ROW_H);
        }
        controls.push(EffectControlRow::GraphicEq { node });
        heights.push(ROW_H + GRAPHIC_EQ_H);
        if !node.inputs.contains_key("band_count") {
            heights.push(ROW_H);
        }
    } else {
        if let Some(inputs) = plugin_node_inputs(plugins, &node.node_type) {
            for input in inputs {
                if input.id != "enabled"
                    && node.inputs.contains_key(&input.id)
                    && effect_input_visible(node, input, project, timeline)
                {
                    controls.push(EffectControlRow::Input {
                        node,
                        input: input.id.as_str(),
                    });
                    heights.push(input_control_height(input.ty));
                }
            }
        }
        for input in node
            .inputs
            .keys()
            .filter(|input| input.as_str() != "enabled")
        {
            if plugin_node_input(plugins, &node.node_type, input).is_none() {
                controls.push(EffectControlRow::Input { node, input });
                heights.push(ROW_H);
            }
        }
        if controls.is_empty() {
            controls.push(EffectControlRow::EmptyParameters);
            heights.push(ROW_H);
        }
    }

    let (body, row_rects) = kama_ui::layout::fit_column_at(
        Rect::new(0.0, 0.0, 1.0, 1.0),
        [0.0, 0.0],
        1.0,
        &heights
            .iter()
            .copied()
            .map(kama_ui::layout::Item::height)
            .collect::<Vec<_>>(),
        0.0,
        EFFECT_NODE_CONTENT_PAD,
    );
    let rows = controls
        .into_iter()
        .zip(row_rects)
        .map(|(row, rect)| (rect.y, row))
        .collect();
    (rows, body.height)
}

fn effect_control_rows<'a>(
    width: f32,
    shared: &'a crate::effects::EffectPipeline,
    plugins: &'a PluginRegistry,
    project: &Project,
    timeline: &TimelineState,
    sections: &HashMap<u64, Accordion>,
) -> (Vec<(f32, EffectControlRow<'a>)>, f32) {
    let nodes = shared.main_path();
    if nodes.is_empty() {
        let (root, rows) = kama_ui::layout::fit_column_at(
            Rect::new(0.0, 0.0, width.max(1.0), ROW_H),
            [0.0, 0.0],
            width.max(1.0),
            &[kama_ui::layout::Item::height(ROW_H)],
            0.0,
            0.0,
        );
        return (
            vec![(rows[0].y, EffectControlRow::EmptyPipeline)],
            root.height,
        );
    }

    struct NodeRects {
        header: BlockId,
        body: BlockId,
    }

    let nodes = nodes
        .iter()
        .map(|&node| {
            let (rows, extent) = effect_body_rows(node, plugins, project, timeline);
            let open = sections.get(&node.id).map_or(1.0, Accordion::open_amount);
            (node, rows, extent, open)
        })
        .collect::<Vec<_>>();
    let viewport = Rect::new(0.0, 0.0, width.max(1.0), 1.0);
    let ((root, ids), measured) = kama_ui::measure_layout(viewport, |ctx| {
        let mut ids = Vec::with_capacity(nodes.len());
        let root = ctx
            .new()
            .width(Size::Fill)
            .height(Size::Fit)
            .column()
            .children(|ctx| {
                for (_, _, extent, open) in &nodes {
                    let mut header = BlockId::default();
                    let mut body = BlockId::default();
                    ctx.new()
                        .width(Size::Fill)
                        .height(Size::Fit)
                        .column()
                        .children(|ctx| {
                            header = ctx
                                .new()
                                .width(Size::Fill)
                                .height(Size::Pixels(EFFECT_NODE_HEADER_H))
                                .build();
                            ctx.new()
                                .width(Size::Fill)
                                .height(Size::Pixels(4.0))
                                .build();
                            body = ctx
                                .new()
                                .width(Size::Fill)
                                .height(Size::FitScale(*open))
                                .children(|ctx| {
                                    ctx.new()
                                        .width(Size::Fill)
                                        .height(Size::Pixels(*extent))
                                        .build();
                                })
                                .build();
                            ctx.new()
                                .width(Size::Fill)
                                .height(Size::Pixels(EFFECT_NODE_GAP))
                                .build();
                        })
                        .build();
                    ids.push(NodeRects { header, body });
                }
            })
            .build();
        (root, ids)
    });

    let mut rows = Vec::new();
    for ((node, body_rows, _, open), ids) in nodes.into_iter().zip(ids) {
        let header = measured.rect(ids.header).expect("effect header rect");
        let body = measured.rect(ids.body).expect("effect body rect");
        rows.push((header.y, EffectControlRow::Header { node, body }));
        if open > 0.98 {
            rows.extend(
                body_rows
                    .into_iter()
                    .map(|(offset, row)| (body.y + offset, row)),
            );
        }
    }
    (rows, measured.rect(root).expect("effect stack rect").height)
}

const ANGLE_ROW_H: f32 = 78.0;

fn model3d_clip_section_rows() -> Vec<f32> {
    vec![ROW_H; 5]
}

fn transform_section_rows() -> Vec<f32> {
    vec![ROW_H, ROW_H, ROW_H, ANGLE_ROW_H]
}

fn compositing_section_rows() -> Vec<f32> {
    vec![ROW_H; 3]
}

fn accordion_header(
    ctx: &mut kama_ui::BuildCtx,
    section: &Accordion,
    header: Rect,
    label: &str,
    chevron: IconId,
) {
    section.build_header(
        ctx,
        FormatKey::new(format_args!("inspector-section-{label}")),
        header,
        label,
        chevron,
        crate::widgets::component_style(),
    );
}

fn inspector_pipeline_buttons(rect: Rect, y: f32) -> [Rect; 3] {
    let row = row_hit(rect, y);
    let vertical = kama_ui::layout::column(
        row,
        &[
            kama_ui::layout::Item::height(2.0),
            kama_ui::layout::Item::height(24.0),
            kama_ui::layout::Item::fill(),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
        None,
    );
    let buttons = kama_ui::layout::row(
        vertical[1],
        &[
            kama_ui::layout::Item::fill(),
            kama_ui::layout::Item::fill(),
            kama_ui::layout::Item::fill(),
        ],
        4.0,
        0.0,
        kama_ui::Align::Start,
    );
    [buttons[0], buttons[1], buttons[2]]
}

fn pipeline_selector_parts(rect: Rect, y: f32) -> (Rect, Rect, Rect, Rect) {
    let row = row_hit(rect, y);
    let parts = kama_ui::layout::row(
        row,
        &[
            kama_ui::layout::Item::width(6.0),
            kama_ui::layout::Item::fill_portion(0.30),
            kama_ui::layout::Item::width(2.0),
            kama_ui::layout::Item::new(
                Size::FillPortion(0.70),
                Size::Pixels((row.height - 4.0).max(1.0)),
            ),
            kama_ui::layout::Item::width(3.0),
            kama_ui::layout::Item::new(
                Size::Pixels(22.0),
                Size::Pixels((row.height - 4.0).max(1.0)),
            ),
            kama_ui::layout::Item::width(3.0),
            kama_ui::layout::Item::new(
                Size::Pixels(22.0),
                Size::Pixels((row.height - 4.0).max(1.0)),
            ),
            kama_ui::layout::Item::width(2.0),
        ],
        0.0,
        0.0,
        kama_ui::Align::Center,
    );
    (parts[1], parts[3], parts[5], parts[7])
}

fn draw_pipeline_selector(
    ctx: &mut kama_ui::BuildCtx,
    rect: Rect,
    y: f32,
    pipeline: Option<u64>,
    editor: &mut TextEdit,
) {
    let (label, name, choose, plus) = pipeline_selector_parts(rect, y);
    ui_text!(
        ctx,
        ("inspector-pipeline-label", y.to_bits()),
        label,
        9.5,
        theme::text(),
        "Pipeline"
    );
    if pipeline.is_some() {
        editor.build(
            ctx,
            "inspector-pipeline-name",
            name,
            "Effect Pipeline",
            crate::widgets::component_style(),
        );
    } else {
        kama_ui::ui!(ctx, {
            Rect(("inspector-pipeline-none", y.to_bits()), name) {
                fill: theme::focused(); border_radius: RADIUS_SM;
                font_size: 9.0; text_color: theme::muted(); text: "None";
            }
        });
    }
    for (id, button, text) in [("choose", choose, "▾"), ("new", plus, "+")] {
        Button::build_filled(
            ctx,
            format!("inspector-pipeline-selector-button-{id}"),
            button,
            text,
            theme::focused(),
            crate::widgets::component_style(),
        );
    }
}

fn reset_gpu_component(current: GpuValue, default: GpuValue, component: usize) -> Option<GpuValue> {
    Some(match (current, default) {
        (GpuValue::F32(_), GpuValue::F32(value)) if component == 0 => GpuValue::F32(value),
        (GpuValue::Vec2(mut values), GpuValue::Vec2(defaults)) if component < 2 => {
            values[component] = defaults[component];
            GpuValue::Vec2(values)
        }
        (GpuValue::Vec3(mut values), GpuValue::Vec3(defaults)) if component < 3 => {
            values[component] = defaults[component];
            GpuValue::Vec3(values)
        }
        (GpuValue::Vec4(mut values), GpuValue::Vec4(defaults)) if component < 4 => {
            values[component] = defaults[component];
            GpuValue::Vec4(values)
        }
        (GpuValue::Color(mut values), GpuValue::Color(defaults)) if component < 4 => {
            values[component] = defaults[component];
            GpuValue::Color(values)
        }
        _ => return None,
    })
}

fn selected_generator_plugin_type(timeline: &TimelineState) -> Option<&str> {
    match timeline.selected_generator()? {
        GeneratorSource::Plugin { generator_type, .. } => Some(generator_type.as_str()),
        GeneratorSource::Wasm { .. } => None,
    }
}

fn is_selected_gradient_generator(timeline: &TimelineState) -> bool {
    selected_generator_plugin_type(timeline) == Some(BUILTIN_GRADIENT_GENERATOR)
}

fn selected_gradient_stop_points(timeline: &TimelineState) -> Vec<[f32; 2]> {
    match timeline.generator_host_value("points") {
        Some(crate::project::HostValue::Vec2Array(points)) => points,
        _ => Vec::new(),
    }
}

fn selected_gradient_stop_colors(timeline: &TimelineState) -> Vec<[f32; 4]> {
    let points = selected_gradient_stop_points(timeline);
    let count = points.len();
    match timeline.generator_host_value("colors") {
        Some(crate::project::HostValue::F32List(values)) => colors_from_values(&values, count),
        _ => (0..count).map(default_color).collect(),
    }
}

fn set_selected_gradient_stop_color(
    timeline: &mut TimelineState,
    index: usize,
    color: [f32; 4],
) -> bool {
    if !is_selected_gradient_generator(timeline) {
        return false;
    }
    let points = selected_gradient_stop_points(timeline);
    if index >= points.len() {
        return false;
    }
    let mut colors = selected_gradient_stop_colors(timeline);
    if let Some(slot) = colors.get_mut(index) {
        *slot = color;
        timeline.set_generator_host_value(
            "colors",
            crate::project::HostValue::F32List(colors_to_values(&colors)),
        );
        return true;
    }
    false
}

fn add_selected_gradient_stop(timeline: &mut TimelineState) -> bool {
    if !is_selected_gradient_generator(timeline) {
        return false;
    }
    let mut points = selected_gradient_stop_points(timeline);
    let mut colors = selected_gradient_stop_colors(timeline);
    let point = points.last().copied().unwrap_or([960.0, 540.0]);
    let color = colors.last().copied().unwrap_or_else(|| default_color(0));
    points.push(point);
    colors.push(color);
    timeline.set_generator_host_value("points", crate::project::HostValue::Vec2Array(points));
    timeline.set_generator_host_value(
        "colors",
        crate::project::HostValue::F32List(colors_to_values(&colors)),
    );
    true
}

fn remove_selected_gradient_stop(timeline: &mut TimelineState) -> bool {
    if !is_selected_gradient_generator(timeline) {
        return false;
    }
    let mut points = selected_gradient_stop_points(timeline);
    if points.len() <= 1 {
        return false;
    }
    let mut colors = selected_gradient_stop_colors(timeline);
    points.pop();
    colors.pop();
    timeline.set_generator_host_value("points", crate::project::HostValue::Vec2Array(points));
    timeline.set_generator_host_value(
        "colors",
        crate::project::HostValue::F32List(colors_to_values(&colors)),
    );
    true
}

fn apply_color_target(
    target: &InspectorColorTarget,
    project: &mut Project,
    timeline: &mut TimelineState,
    color: [f32; 4],
) -> bool {
    match target {
        InspectorColorTarget::Plugin(target) => {
            target.set_value(project, timeline, GpuValue::Color(color))
        }
        InspectorColorTarget::GradientStop(index) => {
            set_selected_gradient_stop_color(timeline, *index, color)
        }
    }
}

fn value_row(ctx: &mut kama_ui::BuildCtx, rect: Rect, y: f32, label: &str, value: &str) {
    let row = row_hit(rect, y);
    let (label_rect, value_rect) = plain_property_parts(row);
    ui_text!(
        ctx,
        ("inspector-static-label", label, y.to_bits()),
        label_rect,
        9.5,
        theme::text(),
        label
    );
    ui_text!(
        ctx,
        ("inspector-static-value", label, y.to_bits()),
        value_rect,
        9.5,
        theme::muted(),
        value,
    );
}

fn property_chrome(
    ctx: &mut kama_ui::BuildCtx,
    rect: Rect,
    y: f32,
    label: &str,
    kind: &'static str,
    keyframe: Option<KeyframeControl>,
) -> Rect {
    let row = row_hit(rect, y);
    let label_rect = property_label_rect(row);
    ui_text!(
        ctx,
        ("inspector-property-label", kind, label, y.to_bits()),
        label_rect,
        9.5,
        theme::text(),
        label
    );
    if let Some(keyframe) = keyframe {
        toggle_icon_button(
            ctx,
            &format!("inspector-{kind}-key-{label}-{}", y.to_bits()),
            keyframe_rect(rect, y),
            keyframe.icon,
            keyframe.keyed,
            if keyframe.keyed {
                "Remove keyframe"
            } else {
                "Add keyframe"
            },
            crate::widgets::component_style(),
        );
        property_control_rect(rect, y)
    } else {
        plain_property_parts(row).1
    }
}

fn sync_text_edit(editor: &mut TextEdit, identity_changed: bool, value: &str) {
    if identity_changed {
        editor.set_focused(false);
    }
    if identity_changed || (!editor.is_focused() && editor.text() != value) {
        editor.reset(value);
    }
}

fn panel_title<K: Hash>(
    ctx: &mut kama_ui::BuildCtx,
    key: K,
    rect: Rect,
    title: &str,
    scroll_y: f32,
) {
    let (_, rows) = kama_ui::layout::fit_column_at(
        rect,
        [rect.x, rect.y - scroll_y],
        rect.width,
        &[
            kama_ui::layout::Item::height(7.0),
            kama_ui::layout::Item::height(20.0),
        ],
        0.0,
        0.0,
    );
    let title_rect = kama_ui::layout::row(
        rows[1],
        &[
            kama_ui::layout::Item::width(10.0),
            kama_ui::layout::Item::fill(),
            kama_ui::layout::Item::width(10.0),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
    )[1];
    ui_text!(ctx, key, title_rect, 11.0, theme::text(), title);
}

fn property_row_frame<K: Hash>(ctx: &mut kama_ui::BuildCtx, key: K, row: Rect) {
    kama_ui::ui!(ctx, {
        Rect(key, row) {
            fill: theme::control();
            border: 1;
            border_color: theme::line();
            border_radius: RADIUS_SM;
        }
    });
}

fn property_label<K: Hash>(ctx: &mut kama_ui::BuildCtx, key: K, row: Rect, label: &str) {
    ui_text!(
        ctx,
        key,
        property_label_rect(row),
        9.5,
        theme::text(),
        label
    );
}

fn offset_rect(rect: Rect, x: f32, y: f32) -> Rect {
    Rect::new(rect.x + x, rect.y + y, rect.width, rect.height)
}

fn hit_local<K: Clone>(
    rects: &HashMap<K, Rect>,
    panel: Rect,
    point: [f32; 2],
) -> Option<(K, Rect)> {
    rects.iter().find_map(|(key, &local)| {
        let absolute = offset_rect(local, panel.x, panel.y);
        absolute.contains(point).then(|| (key.clone(), absolute))
    })
}

fn row_hit(rect: Rect, y: f32) -> Rect {
    kama_ui::layout::fit_column_at(
        rect,
        [rect.x, y],
        rect.width,
        &[kama_ui::layout::Item::height(ROW_H - 3.0)],
        0.0,
        0.0,
    )
    .1[0]
}

fn project_row_hit(rect: Rect, y: f32) -> Rect {
    let (_, rows) = kama_ui::layout::fit_column_at(
        rect,
        [rect.x, y],
        rect.width,
        &[kama_ui::layout::Item::height(ROW_H)],
        0.0,
        0.0,
    );
    kama_ui::layout::row(
        rows[0],
        &[
            kama_ui::layout::Item::width(8.0),
            kama_ui::layout::Item::new(Size::Fill, Size::Pixels(ROW_H - 3.0)),
            kama_ui::layout::Item::width(8.0),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
    )[1]
}

fn keyframe_rect(rect: Rect, y: f32) -> Rect {
    property_row_parts(row_hit(rect, y)).2
}

#[cfg(test)]
mod graph_canvas_tests {
    use super::*;

    #[test]
    fn screen_to_world_preserves_negative_freeform_coordinates() {
        let canvas = Rect::new(10.0, 20.0, 800.0, 600.0);
        let pan = [120.0, -40.0];
        let zoom = 1.75;
        let world = [-96.0, 144.0];
        let screen = [
            canvas.x + pan[0] + world[0] * zoom,
            canvas.y + pan[1] + world[1] * zoom,
        ];
        let resolved = graph_screen_to_world(canvas, pan, zoom, screen);
        assert!((resolved[0] - world[0]).abs() < 0.0001);
        assert!((resolved[1] - world[1]).abs() < 0.0001);
    }

    #[test]
    fn fallback_position_depends_on_identity_not_collection_order() {
        let a = graph_stable_fallback(GraphNodeTarget::Shared(17));
        let b = graph_stable_fallback(GraphNodeTarget::Shared(29));
        assert_eq!(a, graph_stable_fallback(GraphNodeTarget::Shared(17)));
        assert_eq!(b, graph_stable_fallback(GraphNodeTarget::Shared(29)));
        assert_ne!(a, b);
    }
}
