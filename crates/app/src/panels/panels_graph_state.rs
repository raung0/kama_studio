use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GraphNodeTarget {
    Input,
    Local(u64),
    Shared(u64),
    Value(u64),
    Output,
}

impl GraphNodeTarget {
    fn header_color(self) -> Color {
        match self {
            Self::Input => Color::rgb8(0x2f, 0x45, 0x36),
            Self::Output => Color::rgb8(0x3a, 0x3a, 0x3a),
            Self::Local(_) => Color::rgb8(0x2d, 0x3a, 0x48),
            Self::Shared(_) => Color::rgb8(0x4a, 0x36, 0x20),
            Self::Value(_) => Color::rgb8(0x35, 0x2d, 0x4a),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphMonitorSelection {
    Local {
        node: u64,
    },
    Shared {
        pipeline: u64,
        node: u64,
        follows_clip: bool,
    },
}

#[derive(Clone, Copy, Debug)]
struct GraphDrag {
    target: GraphNodeTarget,

    world_offset: [f32; 2],
}

#[derive(Clone, Debug)]
struct GraphGroupDrag {
    start_world: [f32; 2],
    positions: Vec<(GraphNodeTarget, [f32; 2])>,
}

#[derive(Clone, Debug)]
struct GraphBlockSelection {
    start: [f32; 2],
    current: [f32; 2],
    base: HashSet<GraphNodeTarget>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct GraphPropertyKey {
    target: GraphNodeTarget,
    input: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum GraphControlTarget {
    Property(GraphPropertyKey),
    ValueNode(u64),
    HostGradientStop {
        target: GraphNodeTarget,
        input: String,
        index: usize,
    },
}

impl GraphControlTarget {
    fn node(&self) -> GraphNodeTarget {
        match self {
            Self::Property(key) => key.target,
            Self::ValueNode(node) => GraphNodeTarget::Value(*node),
            Self::HostGradientStop { target, .. } => *target,
        }
    }

    fn component_action(&self, component: usize, value: f32, linked: bool) -> PipelineGraphAction {
        match self {
            Self::Property(key) => PipelineGraphAction::SetEffectComponent {
                target: key.target,
                input: key.input.clone(),
                component,
                value,
                linked,
            },
            Self::ValueNode(node) => PipelineGraphAction::SetValueComponent {
                node: *node,
                component,
                value,
                linked,
            },
            Self::HostGradientStop { .. } => PipelineGraphAction::None,
        }
    }
}

#[derive(Clone, Debug)]
enum GraphContextTarget {
    PipelineSelector,
    Property(GraphPropertyKey),
    Node(GraphNodeTarget),
    Wire(GraphWire),
}

#[derive(Clone, Debug)]
struct GraphContextMenu {
    point: [f32; 2],
    target: GraphContextTarget,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphWire {
    LocalImage {
        source: Option<u64>,
        destination: Option<u64>,
    },
    Image {
        source: Option<u64>,
        destination: Option<u64>,
        input: Option<String>,
    },
    Value {
        source: u64,
        destination: u64,
        input: String,
    },
}

#[derive(Clone, Copy, Debug)]
enum PendingGraphWire {
    LocalImage(Option<u64>),
    Image(Option<u64>),
    Value(u64),
}

default_state! {
    pub struct PipelineGraphState {
        selected_node: Option<GraphNodeTarget>,
        selected_nodes: HashSet<GraphNodeTarget>,
        selected_wire: Option<GraphWire>,
        pinned_pipeline: Option<u64>,
        pending_wire: Option<PendingGraphWire>,
        pending_cursor: Option<[f32; 2]>,
        pending_action: Option<PipelineGraphAction>,
        pipeline_id: Option<u64>,
        pipeline_combo: ComboBox = ComboBox::new(0),
        last_pipeline_count: usize,
        pipeline_name: TextEdit = TextEdit::single_line(""),
        renaming: bool,
        controls: PropertyControls<
            (GraphControlTarget, usize),
            GraphPropertyKey,
            GraphPropertyKey,
            (GraphNodeTarget, usize),
            GraphControlTarget,
        >,
        property_links: HashSet<GraphControlTarget>,
        property_link_rects: HashMap<GraphControlTarget, Rect>,
        color_swatch_rects: HashMap<GraphControlTarget, Rect>,
        host_gradient_colors: HashMap<GraphControlTarget, Vec<f32>>,
        host_eq_values: HashMap<GraphNodeTarget, Vec<f32>>,
        host_eq_scroll: HashMap<GraphNodeTarget, f32>,
        host_eq_scroll_rects: HashMap<GraphNodeTarget, (Rect, f32)>,
        host_eq_keyframe_rects: HashMap<GraphNodeTarget, Rect>,
        context_menu: Option<GraphContextMenu>,
        cursor: [f32; 2],
        drag: Option<GraphDrag>,
        group_drag: Option<GraphGroupDrag>,
        block_selection: Option<GraphBlockSelection>,
        z_order: Vec<GraphNodeTarget>,
        last_rect: Option<Rect>,
        pan_drag: Option<([f32; 2], [f32; 2])>,
        pan: [f32; 2],
        zoom: f32 = 1.0,
        local_input_position: [f32; 2] = [24.0, 100.0],
        local_output_position: [f32; 2] = [820.0, 100.0],
    }
}

#[derive(Clone, Debug)]
pub enum PipelineGraphAction {
    None,
    SelectPipeline(Option<u64>),
    Create,
    RemovePipeline(u64),
    InsertNode,
    Remove(GraphNodeTarget),
    RemoveMany(Vec<GraphNodeTarget>),
    DeleteWire(GraphWire),
    MoveNode {
        target: GraphNodeTarget,
        position: [f32; 2],
    },
    MoveNodes(Vec<(GraphNodeTarget, [f32; 2])>),
    ConnectLocalImage {
        node: u64,
        source: Option<u64>,
    },
    SetLocalOutput {
        source: Option<u64>,
    },
    ConnectSharedBoundary {
        source: Option<u64>,
        destination: Option<u64>,
    },
    ConnectImage {
        node: u64,
        input: String,
        source: Option<u64>,
    },
    SetOutput {
        source: Option<u64>,
    },
    ConnectValue {
        node: u64,
        input: String,
        source: u64,
    },
    SetValueComponent {
        node: u64,
        component: usize,
        value: f32,
        linked: bool,
    },
    SetEffectComponent {
        target: GraphNodeTarget,
        input: String,
        component: usize,
        value: f32,
        linked: bool,
    },
    SetEffectValue {
        target: GraphNodeTarget,
        input: String,
        value: GpuValue,
    },
    SetHostValue {
        target: GraphNodeTarget,
        input: String,
        value: crate::project::HostValue,
    },
    ToggleHostKeyframe {
        target: GraphNodeTarget,
        input: String,
    },
    SetValueNodeValue {
        node: u64,
        value: GpuValue,
    },
    MakeInputUnique {
        node: u64,
        input: String,
    },
    UseSharedInput {
        node: u64,
        input: String,
    },
    InsertNodeOnWire {
        node: u64,
        source: Option<u64>,
        destination: Option<u64>,
        destination_input: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub(super) struct GraphInput {
    pub(super) name: String,
    pub(super) definition: Option<PluginInput>,
    pub(super) row_height: Option<f32>,
}

impl AsRef<str> for GraphInput {
    fn as_ref(&self) -> &str {
        &self.name
    }
}

#[derive(Clone, Debug)]
pub(super) struct GraphCard {
    pub(super) kind: GraphNodeTarget,
    pub(super) label: String,
    pub(super) image_inputs: Vec<String>,
    pub(super) inputs: Vec<GraphInput>,
    pub(super) host_inputs: Vec<GraphInput>,
}

#[derive(Clone, Copy)]
struct GraphModel<'a> {
    project: &'a Project,
    timeline: &'a TimelineState,
    pinned: Option<u64>,
}

impl<'a> GraphModel<'a> {
    fn new(project: &'a Project, timeline: &'a TimelineState, pinned: Option<u64>) -> Self {
        Self {
            project,
            timeline,
            pinned,
        }
    }

    fn pipeline_id(self) -> Option<u64> {
        self.pinned
            .or_else(|| self.timeline.selected_pipeline()?.pipeline)
    }

    fn pipeline(self) -> Option<&'a crate::effects::EffectPipeline> {
        self.project.pipeline(self.pipeline_id()?)
    }

    fn value_node(self, id: u64) -> Option<&'a crate::effects::ValueNode> {
        self.pipeline()?.value_node(id)
    }

    fn effect(self, kind: GraphNodeTarget) -> Option<&'a crate::effects::EffectNode> {
        match kind {
            GraphNodeTarget::Local(id) => self
                .timeline
                .selected_pipeline()?
                .local_nodes
                .iter()
                .find(|node| node.id == id),
            GraphNodeTarget::Shared(id) => self.pipeline()?.node(id),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
struct GraphWireGeometry {
    wire: GraphWire,
    from: [f32; 2],
    to: [f32; 2],
    editable: bool,
}

impl GraphWireGeometry {
    fn editable(wire: GraphWire, from: [f32; 2], to: [f32; 2]) -> Self {
        Self {
            wire,
            from,
            to,
            editable: true,
        }
    }
}

struct GraphScene {
    cards: Vec<GraphCard>,
    rects: Vec<Rect>,
    wires: Vec<GraphWireGeometry>,
}

fn graph_input_at<T: AsRef<str>>(
    inputs: &[T],
    point: [f32; 2],
    mut port: impl FnMut(usize) -> Rect,
) -> Option<&str> {
    inputs
        .iter()
        .enumerate()
        .find(|(index, _)| port(*index).contains(point))
        .map(|(_, input)| input.as_ref())
}

fn pending_from_wire(wire: &GraphWire) -> PendingGraphWire {
    match wire {
        GraphWire::LocalImage { source, .. } => PendingGraphWire::LocalImage(*source),
        GraphWire::Image { source, .. } => PendingGraphWire::Image(*source),
        GraphWire::Value { source, .. } => PendingGraphWire::Value(*source),
    }
}

fn occupied_graph_input(
    scene: &GraphScene,
    order: &[GraphNodeTarget],
    point: [f32; 2],
) -> Option<(PendingGraphWire, GraphWire)> {
    let index = graph_card_draw_order(&scene.cards, order)
        .into_iter()
        .next_back()?;
    let card = &scene.cards[index];
    let rect = scene.rects[index];
    let wire = match card.kind {
            GraphNodeTarget::Local(node) if graph_image_input_port(rect).contains(point) => scene
                .wires
                .iter()
                .find(|geometry| matches!(&geometry.wire, GraphWire::LocalImage { destination: Some(destination), .. } if *destination == node)),
            GraphNodeTarget::Shared(node) => graph_input_at(&card.image_inputs, point, |index| {
                graph_named_image_input_port(rect, index)
            })
            .and_then(|input| {
                scene.wires.iter().find(|geometry| {
                    matches!(&geometry.wire, GraphWire::Image { destination: Some(destination), input: Some(candidate), .. }
                        if *destination == node && candidate == input)
                })
            })
            .or_else(|| {
                graph_input_at(&card.inputs, point, |index| graph_scalar_input_port(rect, card, index))
                    .and_then(|input| {
                        scene.wires.iter().find(|geometry| {
                            matches!(&geometry.wire, GraphWire::Value { destination, input: candidate, .. }
                                if *destination == node && candidate == input)
                        })
                    })
            }),
            GraphNodeTarget::Value(node) => graph_input_at(&card.inputs, point, |index| {
                graph_value_input_port(rect, index)
            })
            .and_then(|input| {
                scene.wires.iter().find(|geometry| {
                    matches!(&geometry.wire, GraphWire::Value { destination, input: candidate, .. }
                        if *destination == node && candidate == input)
                })
            }),
            GraphNodeTarget::Output if graph_image_input_port(rect).contains(point) => scene
                .wires
                .iter()
                .find(|geometry| matches!(&geometry.wire,
                    GraphWire::LocalImage { destination: None, .. }
                    | GraphWire::Image { destination: None, .. }
                )),
        _ => None,
    }?;
    Some((pending_from_wire(&wire.wire), wire.wire.clone()))
}

fn graph_output_at(
    cards: &[GraphCard],
    rects: &[Rect],
    order: &[GraphNodeTarget],
    point: [f32; 2],
    shared_input: bool,
) -> Option<PendingGraphWire> {
    graph_card_draw_order(cards, order)
        .into_iter()
        .rev()
        .find_map(|index| {
            let card = &cards[index];
            let rect = rects[index];
            match card.kind {
                GraphNodeTarget::Input if graph_image_output_port(rect).contains(point) => {
                    Some(if shared_input {
                        PendingGraphWire::Image(None)
                    } else {
                        PendingGraphWire::LocalImage(None)
                    })
                }
                GraphNodeTarget::Local(node) if graph_image_output_port(rect).contains(point) => {
                    Some(PendingGraphWire::LocalImage(Some(node)))
                }
                GraphNodeTarget::Shared(node) if graph_image_output_port(rect).contains(point) => {
                    Some(PendingGraphWire::Image(Some(node)))
                }
                GraphNodeTarget::Value(node) if graph_value_output_port(rect).contains(point) => {
                    Some(PendingGraphWire::Value(node))
                }
                _ => None,
            }
        })
}

fn graph_connect_action(
    pending: PendingGraphWire,
    card: &GraphCard,
    rect: Rect,
    point: [f32; 2],
    followed_shared: bool,
) -> Option<PipelineGraphAction> {
    match (pending, card.kind) {
        (PendingGraphWire::LocalImage(source), GraphNodeTarget::Local(node))
            if source != Some(node) && graph_image_input_port(rect).contains(point) =>
        {
            Some(PipelineGraphAction::ConnectLocalImage { node, source })
        }
        (PendingGraphWire::LocalImage(source), GraphNodeTarget::Output)
            if !followed_shared && graph_image_input_port(rect).contains(point) =>
        {
            Some(PipelineGraphAction::SetLocalOutput { source })
        }
        (PendingGraphWire::Image(source), GraphNodeTarget::Shared(node)) => {
            graph_input_at(&card.image_inputs, point, |index| {
                graph_named_image_input_port(rect, index)
            })
            .map(|input| PipelineGraphAction::ConnectImage {
                node,
                input: input.into(),
                source,
            })
        }
        (PendingGraphWire::Image(source), GraphNodeTarget::Local(node))
            if followed_shared && graph_image_input_port(rect).contains(point) =>
        {
            Some(PipelineGraphAction::ConnectSharedBoundary {
                source,
                destination: Some(node),
            })
        }
        (PendingGraphWire::Image(source), GraphNodeTarget::Output)
            if graph_image_input_port(rect).contains(point) =>
        {
            Some(if followed_shared {
                PipelineGraphAction::ConnectSharedBoundary {
                    source,
                    destination: None,
                }
            } else {
                PipelineGraphAction::SetOutput { source }
            })
        }
        (PendingGraphWire::Value(source), GraphNodeTarget::Shared(node)) => {
            graph_input_at(&card.inputs, point, |index| {
                graph_scalar_input_port(rect, card, index)
            })
            .map(|input| PipelineGraphAction::ConnectValue {
                node,
                input: input.into(),
                source,
            })
        }
        (PendingGraphWire::Value(source), GraphNodeTarget::Value(node)) if source != node => {
            graph_input_at(&card.inputs, point, |index| {
                graph_value_input_port(rect, index)
            })
            .map(|input| PipelineGraphAction::ConnectValue {
                node,
                input: input.into(),
                source,
            })
        }
        _ => None,
    }
}

pub(super) fn set_eq_band(values: &mut Vec<f32>, index: usize, normalized: f32) {
    if values.len() <= index {
        values.resize(index + 1, 0.0);
    }
    values[index] = (normalized.clamp(0.0, 1.0) * 48.0 - 24.0).clamp(-24.0, 24.0);
}

pub(super) const GRAPH_CARD_W: f32 = 184.0;
pub(super) const GRAPH_CARD_BASE_H: f32 = 58.0;
const GRAPH_MIN_ZOOM: f32 = 0.28;
const GRAPH_MAX_ZOOM: f32 = 2.25;
const GRAPH_CONTROLS_MIN_ZOOM: f32 = 0.58;
pub(super) const GRAPH_INPUT_H: f32 = 18.0;
pub(super) const GRAPH_IMAGE_INPUT_H: f32 = 16.0;
pub(super) const GRAPH_TOOLBAR_H: f32 = 42.0;

fn graph_host_inputs(
    node: &crate::effects::EffectNode,
    kind: GraphNodeTarget,
    graph: GraphModel<'_>,
    plugins: &PluginRegistry,
) -> Vec<GraphInput> {
    let mut inputs = Vec::new();
    let mut seen = HashSet::new();
    if let Some(definitions) = plugin_node_inputs(plugins, &node.node_type) {
        for definition in definitions {
            if plugin_input_uses_host_binding(definition)
                && node.host_inputs.contains_key(&definition.id)
            {
                seen.insert(definition.id.clone());
                let row_height = if node.node_type == BUILTIN_GRADIENT_GENERATOR
                    && definition.id == "colors"
                    && definition.ty == InputType::F32List
                {
                    let count = graph_host_value(kind, "points", graph)
                        .and_then(|value| match value {
                            crate::project::HostValue::Vec2Array(points) => Some(points.len()),
                            _ => None,
                        })
                        .unwrap_or(0);
                    Some((count + 1).max(1) as f32 * GRAPH_INPUT_H)
                } else {
                    None
                };
                inputs.push(GraphInput {
                    name: definition.id.clone(),
                    definition: Some(definition.clone()),
                    row_height,
                });
            }
        }
    }
    inputs.extend(
        node.host_inputs
            .keys()
            .filter(|name| seen.insert((*name).clone()))
            .map(|name| GraphInput {
                name: name.clone(),
                definition: None,
                row_height: None,
            }),
    );
    inputs
}

fn graph_generator_inputs(
    timeline: &TimelineState,
    plugins: &PluginRegistry,
) -> (String, Vec<GraphInput>, Vec<GraphInput>) {
    let Some(GeneratorSource::Plugin { generator_type, .. }) = timeline.selected_generator() else {
        return ("Input".into(), Vec::new(), Vec::new());
    };
    let Some(generator) = plugins.generator(generator_type) else {
        return ("Input".into(), Vec::new(), Vec::new());
    };

    let mut inputs = Vec::new();
    let mut host_inputs = Vec::new();
    for definition in generator
        .inputs
        .iter()
        .filter(|input| input.is_visible_with(|id| timeline.generator_value(id)))
    {
        let graph_input = GraphInput {
            name: definition.id.clone(),
            definition: Some(definition.clone()),
            row_height: if generator.key == BUILTIN_GRADIENT_GENERATOR
                && definition.id == "colors"
                && definition.ty == InputType::F32List
            {
                let count = match timeline.generator_host_value("points") {
                    Some(crate::project::HostValue::Vec2Array(points)) => points.len(),
                    _ => 0,
                };
                Some((count + 1).max(1) as f32 * GRAPH_INPUT_H)
            } else {
                None
            },
        };
        if plugin_input_uses_host_binding(definition) {
            host_inputs.push(graph_input);
        } else {
            inputs.push(graph_input);
        }
    }
    (generator.name.clone(), inputs, host_inputs)
}

fn graph_effect_card(
    node: &crate::effects::EffectNode,
    kind: GraphNodeTarget,
    graph: GraphModel<'_>,
    plugins: &PluginRegistry,
) -> GraphCard {
    GraphCard {
        kind,
        label: plugin_node_name(plugins, &node.node_type),
        image_inputs: node.image_input_names(),
        inputs: graph_node_inputs(node, kind, graph, plugins),
        host_inputs: graph_host_inputs(node, kind, graph, plugins),
    }
}

fn graph_cards(graph: GraphModel<'_>, plugins: &PluginRegistry) -> Vec<GraphCard> {
    let GraphModel {
        project,
        timeline,
        pinned,
    } = graph;
    let selected_instance = timeline.selected_pipeline();
    let direct_pipeline = pinned.and_then(|id| project.pipeline(id));
    if direct_pipeline.is_none() && selected_instance.is_none() {
        return Vec::new();
    }
    let (source_label, source_inputs, source_host_inputs) = if pinned.is_none() {
        graph_generator_inputs(timeline, plugins)
    } else {
        ("Input".into(), Vec::new(), Vec::new())
    };
    let mut cards = vec![GraphCard {
        kind: GraphNodeTarget::Input,
        label: source_label,
        image_inputs: Vec::new(),
        inputs: source_inputs,
        host_inputs: source_host_inputs,
    }];
    if let Some(shared) = direct_pipeline.or_else(|| {
        selected_instance
            .and_then(|instance| instance.pipeline)
            .and_then(|id| project.pipeline(id))
    }) {
        cards.extend(
            shared.nodes.iter().map(|node| {
                graph_effect_card(node, GraphNodeTarget::Shared(node.id), graph, plugins)
            }),
        );
        cards.extend(shared.value_nodes.iter().map(|node| {
            GraphCard {
                kind: GraphNodeTarget::Value(node.id),
                label: node.kind.label().to_string(),
                image_inputs: Vec::new(),
                inputs: node
                    .kind
                    .input_names()
                    .iter()
                    .map(|name| GraphInput {
                        name: (*name).to_string(),
                        definition: None,
                        row_height: None,
                    })
                    .collect(),
                host_inputs: Vec::new(),
            }
        }));
    }
    if pinned.is_none() {
        if let Some(instance) = selected_instance {
            cards.extend(instance.local_nodes.iter().map(|node| {
                graph_effect_card(node, GraphNodeTarget::Local(node.id), graph, plugins)
            }));
        }
    }
    cards.push(GraphCard {
        kind: GraphNodeTarget::Output,
        label: i18n::text("graph-output"),
        image_inputs: Vec::new(),
        inputs: Vec::new(),
        host_inputs: Vec::new(),
    });
    cards
}

fn graph_node_inputs(
    node: &crate::effects::EffectNode,
    kind: GraphNodeTarget,
    graph: GraphModel<'_>,
    plugins: &PluginRegistry,
) -> Vec<GraphInput> {
    let GraphModel {
        project,
        timeline,
        pinned,
    } = graph;
    if node.node_type == crate::effects::PIPELINE_NODE_TYPE {
        let owner = match kind {
            GraphNodeTarget::Shared(_) => pinned.or_else(|| {
                timeline
                    .selected_pipeline()
                    .and_then(|instance| instance.pipeline)
            }),
            _ => None,
        };
        let options = owner
            .map(|owner| {
                std::iter::once("None".to_string())
                    .chain(
                        project
                            .pipeline_node_options(owner)
                            .into_iter()
                            .map(|(_, name)| name),
                    )
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| vec!["None".to_string()]);
        let definition = PluginInput {
            id: "pipeline".into(),
            name: "Pipeline".into(),
            ty: InputType::Enum,
            default: toml::Value::Integer(0),
            min: None,
            max: None,
            options,
            suffix: String::new(),
            step: None,
            precision: None,
            visible_when: None,
            monitor_handle: None,
            pen_tool: false,
            pen_closed: true,
            monitor_colors: None,
            monitor_midpoints: None,
            monitor_resize_transform: false,
        };
        return vec![GraphInput {
            name: "pipeline".into(),
            definition: Some(definition),
            row_height: None,
        }];
    }
    let mut inputs = Vec::new();
    let mut seen = HashSet::new();
    if let Some(definitions) = plugin_node_inputs(plugins, &node.node_type) {
        for definition in definitions {
            if definition.id != "enabled"
                && node.inputs.contains_key(&definition.id)
                && definition
                    .is_visible_with(|condition| graph_effect_value(kind, condition, graph))
            {
                seen.insert(definition.id.clone());
                inputs.push(GraphInput {
                    name: definition.id.clone(),
                    definition: Some(definition.clone()),
                    row_height: None,
                });
            }
        }
    }

    inputs.extend(
        node.inputs
            .keys()
            .filter(|name| name.as_str() != "enabled" && seen.insert((*name).clone()))
            .map(|name| GraphInput {
                name: name.clone(),
                definition: None,
                row_height: None,
            }),
    );
    inputs
}

pub(super) fn graph_property_row_height(definition: Option<&PluginInput>) -> f32 {
    match definition.map(|definition| definition.ty) {
        Some(InputType::Angle) => ANGLE_ROW_H,
        Some(InputType::Color) => 24.0,
        Some(InputType::Enum | InputType::Bool) => 24.0,
        _ => 22.0,
    }
}

pub(super) fn graph_host_row_height(input: &GraphInput) -> f32 {
    if let Some(height) = input.row_height {
        return height;
    }
    match input.definition.as_ref().map(|definition| definition.ty) {
        Some(InputType::F32List)
            if input
                .definition
                .as_ref()
                .is_some_and(|definition| definition.id == "band_values") =>
        {
            112.0
        }
        _ => GRAPH_INPUT_H,
    }
}

fn graph_card_height(card: &GraphCard, graph: GraphModel<'_>) -> f32 {
    match card.kind {
        GraphNodeTarget::Output => GRAPH_CARD_BASE_H,
        GraphNodeTarget::Value(id) => graph.value_node(id).map_or(GRAPH_CARD_BASE_H, |node| {
            if node.kind.is_constant() {
                GRAPH_CARD_BASE_H + node.value.component_count() as f32 * GRAPH_INPUT_H + 10.0
            } else {
                GRAPH_CARD_BASE_H + node.inputs.len() as f32 * GRAPH_INPUT_H + 10.0
            }
        }),
        GraphNodeTarget::Input | GraphNodeTarget::Local(_) | GraphNodeTarget::Shared(_) => {
            let property_height = card
                .inputs
                .iter()
                .map(|input| graph_property_row_height(input.definition.as_ref()))
                .sum::<f32>();
            GRAPH_CARD_BASE_H
                + card.image_inputs.len().saturating_sub(1) as f32 * GRAPH_IMAGE_INPUT_H
                + property_height
                + card
                    .host_inputs
                    .iter()
                    .map(graph_host_row_height)
                    .sum::<f32>()
                + 8.0
        }
    }
}

pub(super) fn graph_stable_fallback(target: GraphNodeTarget) -> [f32; 2] {
    match target {
        GraphNodeTarget::Input => [24.0, 100.0],
        GraphNodeTarget::Output => [820.0, 100.0],
        GraphNodeTarget::Local(id) => [540.0, 100.0 + id as f32 * 132.0],
        GraphNodeTarget::Shared(id) => {
            let ordinal = id.saturating_sub(1) as f32;
            [292.0, 100.0 + ordinal * 240.0]
        }
        GraphNodeTarget::Value(id) => {
            let ordinal = id.saturating_sub(1) as f32;
            [44.0, 360.0 + ordinal * 180.0]
        }
    }
}

fn graph_node_world_position(
    card: &GraphCard,
    graph: GraphModel<'_>,
    local_input: [f32; 2],
    local_output: [f32; 2],
) -> [f32; 2] {
    let GraphModel {
        project,
        timeline,
        pinned,
    } = graph;
    let selected_instance = timeline.selected_pipeline();
    let shared = pinned.and_then(|id| project.pipeline(id)).or_else(|| {
        selected_instance
            .and_then(|instance| instance.pipeline)
            .and_then(|id| project.pipeline(id))
    });
    match card.kind {
        GraphNodeTarget::Input => {
            if pinned.is_some() {
                shared
                    .and_then(|pipeline| pipeline.ui_input_position)
                    .unwrap_or(local_input)
            } else {
                selected_instance
                    .and_then(|instance| instance.ui_input_position)
                    .unwrap_or(local_input)
            }
        }
        GraphNodeTarget::Output => {
            if pinned.is_some() {
                shared
                    .and_then(|pipeline| pipeline.ui_output_position)
                    .unwrap_or(local_output)
            } else {
                selected_instance
                    .and_then(|instance| instance.ui_output_position)
                    .unwrap_or(local_output)
            }
        }
        GraphNodeTarget::Local(id) => selected_instance
            .and_then(|instance| instance.local_nodes.iter().find(|node| node.id == id))
            .and_then(|node| node.ui_position)
            .unwrap_or_else(|| graph_stable_fallback(card.kind)),
        GraphNodeTarget::Shared(id) => shared
            .and_then(|pipeline| pipeline.node(id))
            .and_then(|node| node.ui_position)
            .unwrap_or_else(|| graph_stable_fallback(card.kind)),
        GraphNodeTarget::Value(id) => shared
            .and_then(|pipeline| pipeline.value_nodes.iter().find(|node| node.id == id))
            .and_then(|node| node.ui_position)
            .unwrap_or_else(|| graph_stable_fallback(card.kind)),
    }
}

fn graph_card_rects(
    canvas: Rect,
    cards: &[GraphCard],
    graph: GraphModel<'_>,
    pan: [f32; 2],
    zoom: f32,
    local_input: [f32; 2],
    local_output: [f32; 2],
) -> Vec<Rect> {
    let zoom = zoom.clamp(GRAPH_MIN_ZOOM, GRAPH_MAX_ZOOM);
    cards
        .iter()
        .map(|card| {
            let position = graph_node_world_position(card, graph, local_input, local_output);
            Rect::new(
                canvas.x + pan[0] + position[0] * zoom,
                canvas.y + pan[1] + position[1] * zoom,
                GRAPH_CARD_W * zoom,
                graph_card_height(card, graph) * zoom,
            )
        })
        .collect()
}

fn graph_effect_value(
    kind: GraphNodeTarget,
    input: &str,
    graph: GraphModel<'_>,
) -> Option<GpuValue> {
    let GraphModel {
        project,
        timeline,
        pinned,
    } = graph;
    match kind {
        GraphNodeTarget::Local(_) => graph
            .effect(kind)?
            .inputs
            .get(input)?
            .evaluate(timeline.selected_keyframe_time()),
        GraphNodeTarget::Shared(id) if pinned.is_none() => {
            let pipeline_id = timeline.selected_pipeline()?.pipeline?;
            let node = project.pipeline(pipeline_id)?.node(id)?;
            if node
                .dynamic_image_inputs
                .as_ref()
                .is_some_and(|dynamic| dynamic.count_input == input)
            {
                node.inputs
                    .get(input)?
                    .evaluate(timeline.selected_keyframe_time())
            } else {
                timeline.pipeline_input_value(project, id, input)
            }
        }
        GraphNodeTarget::Shared(_) => graph
            .effect(kind)?
            .inputs
            .get(input)?
            .evaluate(timeline.selected_keyframe_time()),
        GraphNodeTarget::Input if graph.pinned.is_none() => graph.timeline.generator_value(input),
        GraphNodeTarget::Input | GraphNodeTarget::Value(_) | GraphNodeTarget::Output => None,
    }
}

fn graph_host_value(
    target: GraphNodeTarget,
    input: &str,
    graph: GraphModel<'_>,
) -> Option<crate::project::HostValue> {
    match target {
        GraphNodeTarget::Input if graph.pinned.is_none() => {
            graph.timeline.generator_host_value(input)
        }
        GraphNodeTarget::Local(node) if graph.pinned.is_none() => {
            graph.timeline.selected_local_node_host_value(node, input)
        }
        GraphNodeTarget::Shared(node) => graph.project.pipeline_node_host_value(
            graph.pipeline_id()?,
            node,
            input,
            graph.timeline.selected_keyframe_time(),
        ),
        _ => None,
    }
}

fn graph_host_has_keyframe(target: GraphNodeTarget, input: &str, graph: GraphModel<'_>) -> bool {
    match target {
        GraphNodeTarget::Input if graph.pinned.is_none() => {
            graph.timeline.generator_has_keyframe(input)
        }
        GraphNodeTarget::Local(node) if graph.pinned.is_none() => graph
            .timeline
            .selected_local_node_host_has_keyframe(node, input),
        GraphNodeTarget::Shared(node) => graph.pipeline_id().is_some_and(|pipeline| {
            graph.project.pipeline_node_host_has_keyframe(
                pipeline,
                node,
                input,
                graph.timeline.selected_keyframe_time(),
            )
        }),
        _ => false,
    }
}

fn graph_host_has_keyframes(target: GraphNodeTarget, input: &str, graph: GraphModel<'_>) -> bool {
    match target {
        GraphNodeTarget::Input if graph.pinned.is_none() => {
            graph.timeline.generator_has_keyframes(input)
        }
        GraphNodeTarget::Local(node) if graph.pinned.is_none() => graph
            .timeline
            .selected_local_node_host_has_keyframes(node, input),
        GraphNodeTarget::Shared(node) => graph.pipeline_id().is_some_and(|pipeline| {
            graph
                .project
                .pipeline_node_host_has_keyframes(pipeline, node, input)
        }),
        _ => false,
    }
}

fn graph_target_value(
    target: GraphNodeTarget,
    input: &str,
    graph: GraphModel<'_>,
) -> Option<GpuValue> {
    match target {
        GraphNodeTarget::Local(_) | GraphNodeTarget::Shared(_) => {
            graph_effect_value(target, input, graph)
        }
        GraphNodeTarget::Value(id) => graph
            .value_node(id)?
            .inputs
            .get(input)?
            .evaluate(graph.timeline.selected_keyframe_time()),
        GraphNodeTarget::Input if graph.pinned.is_none() => graph.timeline.generator_value(input),
        GraphNodeTarget::Input | GraphNodeTarget::Output => None,
    }
}

fn graph_gpu_component(value: GpuValue, component: usize) -> Option<f32> {
    match value {
        GpuValue::F32(value) if component == 0 => Some(value),
        GpuValue::Vec2(value) => value.get(component).copied(),
        GpuValue::Vec3(value) => value.get(component).copied(),
        GpuValue::Vec4(value) | GpuValue::Color(value) => value.get(component).copied(),
        _ => None,
    }
}

fn graph_property_definition<'a>(
    target: GraphNodeTarget,
    input: &str,
    graph: GraphModel<'_>,
    plugins: &'a PluginRegistry,
) -> Option<&'a PluginInput> {
    if target == GraphNodeTarget::Input && graph.pinned.is_none() {
        let generator = selected_generator_plugin_type(graph.timeline)?;
        return plugins
            .generator(generator)?
            .inputs
            .iter()
            .find(|definition| definition.id == input);
    }
    plugin_node_input(plugins, &graph.effect(target)?.node_type, input)
}

fn set_gradient_color_values(values: &mut Vec<f32>, index: usize, color: [f32; 4]) {
    let base = index * 4;
    if values.len() < base + 4 {
        values.resize(base + 4, 0.0);
    }
    values[base..base + 4].copy_from_slice(&color);
}

fn card_index(cards: &[GraphCard], target: GraphNodeTarget) -> Option<usize> {
    cards.iter().position(|card| card.kind == target)
}

fn graph_card_draw_order(cards: &[GraphCard], z_order: &[GraphNodeTarget]) -> Vec<usize> {
    let mut order = (0..cards.len()).collect::<Vec<_>>();
    order.sort_by_key(|index| {
        let target = cards[*index].kind;
        z_order
            .iter()
            .position(|candidate| *candidate == target)
            .map_or(0usize, |position| position + 1)
    });
    order
}

fn graph_unique_property_at(
    card: &GraphCard,
    card_rect: Rect,
    point: [f32; 2],
    graph: GraphModel<'_>,
) -> Option<GraphPropertyKey> {
    let GraphModel {
        project,
        timeline,
        pinned,
    } = graph;
    if pinned.is_some() {
        return None;
    }
    let GraphNodeTarget::Shared(node_id) = card.kind else {
        return None;
    };
    let input = card
        .inputs
        .iter()
        .enumerate()
        .find(|(index, _)| graph_property_row_rect(card_rect, card, *index).contains(point))?;
    let input = input.1;

    let can_toggle_unique = timeline.pipeline_input_is_override(node_id, &input.name)
        || timeline
            .selected_pipeline()
            .and_then(|instance| instance.pipeline)
            .and_then(|pipeline_id| project.pipeline(pipeline_id))
            .and_then(|pipeline| pipeline.node(node_id))
            .is_some_and(|node| {
                node.dynamic_image_inputs
                    .as_ref()
                    .is_none_or(|dynamic| dynamic.count_input != input.name)
                    && node.inputs.get(&input.name).is_some_and(|binding| {
                        !matches!(binding, crate::effects::Binding::Connection(_))
                    })
            });
    can_toggle_unique.then(|| GraphPropertyKey {
        target: GraphNodeTarget::Shared(node_id),
        input: input.name.clone(),
    })
}

fn graph_value_summary(value: GpuValue, definition: Option<&PluginInput>) -> String {
    match value {
        GpuValue::Color(color) => format!(
            "#{:02X}{:02X}{:02X}",
            (color[0].clamp(0.0, 1.0) * 255.0).round() as u8,
            (color[1].clamp(0.0, 1.0) * 255.0).round() as u8,
            (color[2].clamp(0.0, 1.0) * 255.0).round() as u8,
        ),
        GpuValue::Enum(index) => definition
            .and_then(|definition| definition.options.get(index as usize))
            .cloned()
            .unwrap_or_else(|| index.to_string()),
        GpuValue::F32(value) => format!("{value:.3}"),
        value => format_gpu_value(value),
    }
}

fn graph_number_settings(
    value: GpuValue,
    definition: Option<&PluginInput>,
) -> ((f64, f64), f64, usize, &str) {
    let integer = matches!(
        value,
        GpuValue::I32(_) | GpuValue::U32(_) | GpuValue::Enum(_) | GpuValue::Bool(_)
    );
    let mut minimum = if matches!(
        value,
        GpuValue::Color(_) | GpuValue::Bool(_) | GpuValue::U32(_) | GpuValue::Enum(_)
    ) {
        0.0
    } else {
        f64::NEG_INFINITY
    };
    let mut maximum = if matches!(value, GpuValue::Color(_) | GpuValue::Bool(_)) {
        1.0
    } else {
        f64::INFINITY
    };
    let mut sensitivity = if matches!(value, GpuValue::Color(_)) {
        0.005
    } else if integer {
        1.0
    } else {
        0.01
    };
    let mut precision = if integer { 0 } else { 3 };
    let mut suffix = "";
    if let Some(definition) = definition {
        minimum = definition.min.map_or(minimum, f64::from);
        maximum = definition.max.map_or(maximum, f64::from);
        sensitivity = definition.step.map_or(sensitivity, f64::from);
        precision = definition.precision.unwrap_or(precision);
        suffix = &definition.suffix;
    }
    ((minimum, maximum), sensitivity, precision, suffix)
}

fn graph_property_label_rect(card_rect: Rect, card: &GraphCard, input_index: usize) -> Rect {
    let row = graph_property_row_rect(card_rect, card, input_index);
    let scale = graph_card_scale(card_rect);
    let inner_h = (row.height - 6.0 * scale).max(10.0 * scale);
    crate::ui_layout::row(
        row,
        &[
            crate::ui_layout::Item::width(10.0 * scale),
            crate::ui_layout::Item::new(Size::Pixels(54.0 * scale), Size::Pixels(inner_h)),
            crate::ui_layout::Item::fill(),
        ],
        0.0,
        0.0,
        kama_ui::Align::Center,
    )[1]
}

fn graph_property_value_area(
    card_rect: Rect,
    card: &GraphCard,
    input_index: usize,
    linkable: bool,
) -> Rect {
    let row = graph_property_row_rect(card_rect, card, input_index);
    let scale = graph_card_scale(card_rect);
    let reserve = if linkable { 22.0 * scale } else { 0.0 };
    let inner_h = (row.height - 6.0 * scale).max(12.0 * scale);
    crate::ui_layout::row(
        row,
        &[
            crate::ui_layout::Item::width(66.0 * scale),
            crate::ui_layout::Item::new(Size::Fill, Size::Pixels(inner_h)),
            crate::ui_layout::Item::width(10.0 * scale + reserve),
        ],
        0.0,
        0.0,
        kama_ui::Align::Center,
    )[1]
}

fn graph_property_link_rect(card_rect: Rect, card: &GraphCard, input_index: usize) -> Rect {
    let row = graph_property_row_rect(card_rect, card, input_index);
    let scale = graph_card_scale(card_rect);
    let height = (row.height - 6.0 * scale)
        .min(18.0 * scale)
        .max(10.0 * scale);
    crate::ui_layout::row(
        row,
        &[
            crate::ui_layout::Item::fill(),
            crate::ui_layout::Item::new(Size::Pixels(19.0 * scale), Size::Pixels(height)),
            crate::ui_layout::Item::width(10.0 * scale),
        ],
        0.0,
        0.0,
        kama_ui::Align::Center,
    )[1]
}

fn graph_property_component_rect(
    card_rect: Rect,
    card: &GraphCard,
    input_index: usize,
    component: usize,
    component_count: usize,
    linkable: bool,
) -> Rect {
    graph_component_rect(
        graph_property_value_area(card_rect, card, input_index, linkable),
        graph_card_scale(card_rect),
        component,
        component_count,
    )
}

fn graph_angle_parts(card_rect: Rect, card: &GraphCard, input_index: usize) -> (Rect, Rect, Rect) {
    let scale = graph_card_scale(card_rect);
    let area = graph_property_value_area(card_rect, card, input_index, false);
    let vertical = crate::ui_layout::column(
        area,
        &[
            crate::ui_layout::Item::height(18.0 * scale),
            crate::ui_layout::Item::height(10.0 * scale),
            crate::ui_layout::Item::height((ANGLE_ROW_H - 34.0) * scale),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
        None,
    );
    let top = crate::ui_layout::row(
        vertical[0],
        &[
            crate::ui_layout::Item::fill(),
            crate::ui_layout::Item::fill(),
        ],
        3.0 * scale,
        0.0,
        kama_ui::Align::Start,
    );
    (top[0], top[1], vertical[2])
}

fn graph_color_swatch_rect(card_rect: Rect, card: &GraphCard, input_index: usize) -> Rect {
    graph_property_value_area(card_rect, card, input_index, false)
}

fn image_binding_source(
    binding: &crate::effects::ImageBinding,
    pipeline_input: Option<[f32; 2]>,
    node_point: impl FnOnce(u64) -> Option<[f32; 2]>,
) -> Option<(Option<u64>, [f32; 2])> {
    use crate::effects::ImageBinding;
    match binding {
        ImageBinding::Disconnected => None,
        ImageBinding::PipelineInput => pipeline_input.map(|point| (None, point)),
        ImageBinding::Node(socket) => {
            node_point(socket.node).map(|point| (Some(socket.node), point))
        }
    }
}

fn graph_node_output_point(
    cards: &[GraphCard],
    rects: &[Rect],
    target: impl FnOnce(u64) -> GraphNodeTarget,
    node: u64,
) -> Option<[f32; 2]> {
    card_index(cards, target(node)).map(|index| graph_image_output_point(rects[index]))
}

fn shared_pipeline_output_point(
    graph: GraphModel<'_>,
    cards: &[GraphCard],
    rects: &[Rect],
) -> Option<[f32; 2]> {
    let GraphModel {
        project,
        timeline,
        pinned,
    } = graph;
    let input = cards
        .iter()
        .position(|card| matches!(card.kind, GraphNodeTarget::Input))
        .map(|index| graph_image_output_point(rects[index]))?;
    let Some(pipeline_id) = pinned.or_else(|| timeline.selected_pipeline()?.pipeline) else {
        return Some(input);
    };
    image_binding_source(
        &project.pipeline(pipeline_id)?.output,
        Some(input),
        |node| graph_node_output_point(cards, rects, GraphNodeTarget::Shared, node),
    )
    .map(|(_, point)| point)
}

fn pending_graph_source_point(
    pending: PendingGraphWire,
    graph: GraphModel<'_>,
    cards: &[GraphCard],
    rects: &[Rect],
) -> Option<[f32; 2]> {
    match pending {
        PendingGraphWire::LocalImage(None) => shared_pipeline_output_point(graph, cards, rects),
        PendingGraphWire::LocalImage(Some(node)) => {
            graph_node_output_point(cards, rects, GraphNodeTarget::Local, node)
        }
        PendingGraphWire::Image(None) => cards
            .iter()
            .position(|card| matches!(card.kind, GraphNodeTarget::Input))
            .map(|index| graph_image_output_point(rects[index])),
        PendingGraphWire::Image(Some(node)) => {
            graph_node_output_point(cards, rects, GraphNodeTarget::Shared, node)
        }
        PendingGraphWire::Value(node) => card_index(cards, GraphNodeTarget::Value(node))
            .map(|index| graph_value_output_point(rects[index])),
    }
}

fn graph_value_source(
    cards: &[GraphCard],
    rects: &[Rect],
    binding: &crate::effects::Binding,
) -> Option<(u64, [f32; 2])> {
    let crate::effects::Binding::Connection(socket) = binding else {
        return None;
    };
    card_index(cards, GraphNodeTarget::Value(socket.node))
        .map(|index| (socket.node, graph_value_output_point(rects[index])))
}

fn graph_wire_geometry(
    graph: GraphModel<'_>,
    cards: &[GraphCard],
    rects: &[Rect],
) -> Vec<GraphWireGeometry> {
    use crate::effects::ImageBinding;
    let GraphModel {
        project,
        timeline,
        pinned,
    } = graph;
    let mut wires = Vec::new();
    let Some(input_index) = cards
        .iter()
        .position(|card| matches!(card.kind, GraphNodeTarget::Input))
    else {
        return wires;
    };
    let Some(output_index) = cards
        .iter()
        .position(|card| matches!(card.kind, GraphNodeTarget::Output))
    else {
        return wires;
    };
    let graph_input = graph_image_output_point(rects[input_index]);
    let followed_instance = pinned
        .is_none()
        .then(|| timeline.selected_pipeline())
        .flatten();
    let pipeline = pinned
        .or_else(|| followed_instance.and_then(|instance| instance.pipeline))
        .and_then(|id| project.pipeline(id));
    let selected_instance = followed_instance
        .filter(|_| pipeline.is_none_or(|pipeline| pipeline.kind == PipelineKind::Video));

    if let Some(pipeline) = pipeline {
        for node in &pipeline.nodes {
            let Some(destination_index) = card_index(cards, GraphNodeTarget::Shared(node.id))
            else {
                continue;
            };
            for (image_index, name) in cards[destination_index].image_inputs.iter().enumerate() {
                let binding = node
                    .image_inputs
                    .get(name)
                    .unwrap_or(&ImageBinding::Disconnected);
                if let Some((source, from)) =
                    image_binding_source(binding, Some(graph_input), |node| {
                        graph_node_output_point(cards, rects, GraphNodeTarget::Shared, node)
                    })
                {
                    wires.push(GraphWireGeometry::editable(
                        GraphWire::Image {
                            source,
                            destination: Some(node.id),
                            input: Some(name.clone()),
                        },
                        from,
                        graph_named_image_input_point(rects[destination_index], image_index),
                    ));
                }
            }
            for (input_index, (name, binding)) in node
                .inputs
                .iter()
                .filter(|(name, _)| name.as_str() != "enabled")
                .enumerate()
            {
                let Some((source, from)) = graph_value_source(cards, rects, binding) else {
                    continue;
                };
                wires.push(GraphWireGeometry::editable(
                    GraphWire::Value {
                        source,
                        destination: node.id,
                        input: name.clone(),
                    },
                    from,
                    graph_scalar_input_point(
                        rects[destination_index],
                        &cards[destination_index],
                        input_index,
                    ),
                ));
            }
        }
        for node in &pipeline.value_nodes {
            let Some(destination_index) = card_index(cards, GraphNodeTarget::Value(node.id)) else {
                continue;
            };
            for (input_index, input) in cards[destination_index].inputs.iter().enumerate() {
                let Some((source, from)) = node
                    .inputs
                    .get(&input.name)
                    .and_then(|binding| graph_value_source(cards, rects, binding))
                else {
                    continue;
                };
                wires.push(GraphWireGeometry::editable(
                    GraphWire::Value {
                        source,
                        destination: node.id,
                        input: input.name.clone(),
                    },
                    from,
                    graph_value_input_point(rects[destination_index], input_index),
                ));
            }
        }
    }

    let shared_output = shared_pipeline_output_point(graph, cards, rects);
    if let Some(instance) = selected_instance {
        for node in &instance.local_nodes {
            let Some(destination_index) = card_index(cards, GraphNodeTarget::Local(node.id)) else {
                continue;
            };
            let binding = node
                .image_inputs
                .get("image")
                .unwrap_or(&ImageBinding::Disconnected);
            if let Some((source, from)) = image_binding_source(binding, shared_output, |node| {
                graph_node_output_point(cards, rects, GraphNodeTarget::Local, node)
            }) {
                wires.push(GraphWireGeometry::editable(
                    GraphWire::LocalImage {
                        source,
                        destination: Some(node.id),
                    },
                    from,
                    graph_image_input_point(rects[destination_index]),
                ));
            }
        }
        if let Some((source, from)) =
            image_binding_source(&instance.local_output, shared_output, |node| {
                graph_node_output_point(cards, rects, GraphNodeTarget::Local, node)
            })
        {
            wires.push(GraphWireGeometry::editable(
                GraphWire::LocalImage {
                    source,
                    destination: None,
                },
                from,
                graph_image_input_point(rects[output_index]),
            ));
        }
        return wires;
    }

    if let Some(pipeline) = pipeline {
        if let Some((source, from)) =
            image_binding_source(&pipeline.output, Some(graph_input), |node| {
                graph_node_output_point(cards, rects, GraphNodeTarget::Shared, node)
            })
        {
            wires.push(GraphWireGeometry::editable(
                GraphWire::Image {
                    source,
                    destination: None,
                    input: None,
                },
                from,
                graph_image_input_point(rects[output_index]),
            ));
        }
    }
    wires
}

fn draw_graph_grid(ctx: &mut kama_ui::BuildCtx, canvas: Rect, pan: [f32; 2], zoom: f32) {
    for (level, world, min_pixels, color) in [
        (
            0usize,
            32.0,
            12.0,
            theme::line().mix(theme::timeline_bg(), 0.90),
        ),
        (
            1usize,
            128.0,
            20.0,
            theme::line().mix(theme::timeline_bg(), 0.74),
        ),
    ] {
        let spacing = world * zoom;
        if spacing < min_pixels {
            continue;
        }
        for (axis, origin, end) in [
            (
                0usize,
                canvas.x + pan[0].rem_euclid(spacing),
                canvas.right(),
            ),
            (
                1usize,
                canvas.y + pan[1].rem_euclid(spacing),
                canvas.bottom(),
            ),
        ] {
            let mut position = origin;
            let mut index = 0usize;
            while position < end {
                let line = if axis == 0 {
                    Rect::new(position, canvas.y, 1.0, canvas.height)
                } else {
                    Rect::new(canvas.x, position, canvas.width, 1.0)
                };
                kama_ui::ui!(ctx, {
                    Rect(("graph-grid", level, axis, index), line) {
                        fill: color;
                    }
                });
                position += spacing;
                index += 1;
            }
        }
    }
}

fn graph_curve_points(from: [f32; 2], to: [f32; 2], zoom: f32) -> Vec<[f32; 2]> {
    let zoom = zoom.max(0.000_1);
    let handle = ((to[0] - from[0]).abs() * 0.45).clamp(52.0 * zoom, 180.0 * zoom);
    let c1 = [from[0] + handle, from[1]];
    let c2 = [to[0] - handle, to[1]];
    const STEPS: usize = 24;
    (0..=STEPS)
        .map(|step| {
            let t = step as f32 / STEPS as f32;
            let u = 1.0 - t;
            [
                u * u * u * from[0]
                    + 3.0 * u * u * t * c1[0]
                    + 3.0 * u * t * t * c2[0]
                    + t * t * t * to[0],
                u * u * u * from[1]
                    + 3.0 * u * u * t * c1[1]
                    + 3.0 * u * t * t * c2[1]
                    + t * t * t * to[1],
            ]
        })
        .collect()
}

fn draw_graph_curve(
    ctx: &mut kama_ui::BuildCtx,
    id: usize,
    from: [f32; 2],
    to: [f32; 2],
    color: Color,
    zoom: f32,
) {
    let points = graph_curve_points(from, to, zoom);
    let margin = (3.0 * zoom).max(1.0);
    let min_x = points.iter().map(|p| p[0]).fold(f32::INFINITY, f32::min) - margin;
    let min_y = points.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min) - margin;
    let max_x = points
        .iter()
        .map(|p| p[0])
        .fold(f32::NEG_INFINITY, f32::max)
        + margin;
    let max_y = points
        .iter()
        .map(|p| p[1])
        .fold(f32::NEG_INFINITY, f32::max)
        + margin;
    let bounds = Rect::new(
        min_x,
        min_y,
        (max_x - min_x).max(1.0),
        (max_y - min_y).max(1.0),
    );
    let half_width = (1.5 * zoom).max(0.6);
    let mut vertices = Vec::with_capacity(24 * 6);
    for pair in points.windows(2) {
        let a = pair[0];
        let b = pair[1];
        let dx = b[0] - a[0];
        let dy = b[1] - a[1];
        let length = (dx * dx + dy * dy).sqrt().max(0.0001);
        let nx = -dy / length * half_width;
        let ny = dx / length * half_width;
        let a0 = [a[0] + nx - bounds.x, a[1] + ny - bounds.y];
        let a1 = [a[0] - nx - bounds.x, a[1] - ny - bounds.y];
        let b0 = [b[0] + nx - bounds.x, b[1] + ny - bounds.y];
        let b1 = [b[0] - nx - bounds.x, b[1] - ny - bounds.y];
        vertices.extend([a0, b0, a1, a1, b0, b1]);
    }
    kama_ui::ui!(ctx, {
        Rect(("pipeline-edge", id), bounds) {
            fill: color;
            vertices: vertices;
        }
    });
}

fn point_segment_distance(point: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let ab = [b[0] - a[0], b[1] - a[1]];
    let ap = [point[0] - a[0], point[1] - a[1]];
    let length2 = ab[0] * ab[0] + ab[1] * ab[1];
    let t = if length2 <= f32::EPSILON {
        0.0
    } else {
        ((ap[0] * ab[0] + ap[1] * ab[1]) / length2).clamp(0.0, 1.0)
    };
    let closest = [a[0] + ab[0] * t, a[1] + ab[1] * t];
    let dx = point[0] - closest[0];
    let dy = point[1] - closest[1];
    (dx * dx + dy * dy).sqrt()
}

fn graph_wire_hit(from: [f32; 2], to: [f32; 2], point: [f32; 2], zoom: f32) -> bool {
    let tolerance = (10.0 * zoom).max(4.0);
    graph_curve_points(from, to, zoom)
        .windows(2)
        .any(|pair| point_segment_distance(point, pair[0], pair[1]) <= tolerance)
}

fn graph_wire_crosses_rect(from: [f32; 2], to: [f32; 2], rect: Rect, zoom: f32) -> bool {
    graph_curve_points(from, to, zoom)
        .into_iter()
        .any(|point| rect.contains(point))
}

impl PipelineGraphState {
    fn scene(
        &self,
        rect: Rect,
        project: &Project,
        timeline: &TimelineState,
        plugins: &PluginRegistry,
    ) -> GraphScene {
        let graph = GraphModel::new(project, timeline, self.pinned_pipeline);
        let cards = graph_cards(graph, plugins);
        let rects = graph_card_rects(
            graph_canvas_rect(rect),
            &cards,
            graph,
            self.pan,
            self.zoom,
            self.local_input_position,
            self.local_output_position,
        );
        let wires = graph_wire_geometry(graph, &cards, &rects);
        GraphScene {
            cards,
            rects,
            wires,
        }
    }

    pub fn monitor_selection(&self, timeline: &TimelineState) -> Option<GraphMonitorSelection> {
        match self.selected_node? {
            GraphNodeTarget::Local(node) if self.pinned_pipeline.is_none() => {
                Some(GraphMonitorSelection::Local { node })
            }
            GraphNodeTarget::Local(_) => None,
            GraphNodeTarget::Shared(node) => {
                let pipeline = self.pinned_pipeline.or_else(|| {
                    timeline
                        .selected_pipeline()
                        .and_then(|instance| instance.pipeline)
                })?;
                Some(GraphMonitorSelection::Shared {
                    pipeline,
                    node,
                    follows_clip: self.pinned_pipeline.is_none(),
                })
            }
            GraphNodeTarget::Input | GraphNodeTarget::Value(_) | GraphNodeTarget::Output => None,
        }
    }

    pub fn tick(&mut self, dt: f32) {
        self.pipeline_name.tick(dt);
        self.pipeline_combo.tick(dt);
        self.controls.tick(dt);
    }

    pub fn is_animating(&self) -> bool {
        self.pipeline_name.is_animating()
            || self.pipeline_combo.is_animating()
            || self.controls.is_animating()
    }

    pub fn is_cursor_lock_dragging(&self) -> bool {
        self.controls.is_cursor_lock_dragging()
    }

    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
            || self.group_drag.is_some()
            || self.block_selection.is_some()
            || self.pan_drag.is_some()
            || self.pending_wire.is_some()
            || self.controls.is_dragging()
    }

    pub fn set_focused(&mut self, focused: bool) {
        if focused {
            return;
        }
        self.pipeline_name.set_focused(false);
        self.pipeline_combo.close();
        self.pending_wire = None;
        self.pending_cursor = None;
        self.controls.blur();
        self.context_menu = None;
        self.drag = None;
        self.group_drag = None;
        self.block_selection = None;
    }

    pub fn popup_contains(&self, rect: Rect, point: [f32; 2]) -> bool {
        let toolbar = graph_toolbar_layout(rect);
        let pipeline_count = self.last_pipeline_count.saturating_add(1);
        self.pipeline_combo
            .popup_contains(toolbar.combo, point, pipeline_count)
            || self.controls.popup_contains(rect, point)
    }

    pub fn sync_color_picker_textures(&mut self, renderer: &mut Renderer) -> Result<()> {
        self.controls.color_picker.sync_textures(renderer)
    }

    fn bring_to_front(&mut self, target: GraphNodeTarget) {
        self.z_order.retain(|candidate| *candidate != target);
        self.z_order.push(target);
    }

    fn select_single_node(&mut self, target: GraphNodeTarget) {
        self.selected_node = Some(target);
        self.selected_nodes.clear();
        self.selected_nodes.insert(target);
        self.selected_wire = None;
    }

    fn clear_node_selection(&mut self) {
        self.selected_node = None;
        self.selected_nodes.clear();
    }

    fn selected_targets(&self) -> Vec<GraphNodeTarget> {
        let mut targets = self.selected_nodes.iter().copied().collect::<Vec<_>>();
        targets.sort_by_key(|target| match *target {
            GraphNodeTarget::Input => (0, 0),
            GraphNodeTarget::Local(id) => (1, id),
            GraphNodeTarget::Shared(id) => (2, id),
            GraphNodeTarget::Value(id) => (3, id),
            GraphNodeTarget::Output => (4, 0),
        });
        targets
    }

    fn delete_selection_action(&mut self, take_wire: bool) -> PipelineGraphAction {
        let wire = if take_wire {
            self.selected_wire.take()
        } else {
            self.selected_wire.clone()
        };
        if let Some(wire) = wire {
            return PipelineGraphAction::DeleteWire(wire);
        }
        let targets = self
            .selected_targets()
            .into_iter()
            .filter(|target| !matches!(target, GraphNodeTarget::Input | GraphNodeTarget::Output))
            .collect::<Vec<_>>();
        let action = match targets.as_slice() {
            [] => PipelineGraphAction::None,
            [target] => PipelineGraphAction::Remove(*target),
            _ => PipelineGraphAction::RemoveMany(targets),
        };
        if !matches!(action, PipelineGraphAction::None) {
            self.clear_node_selection();
        }
        action
    }

    fn begin_pipeline_rename(&mut self) {
        if self.pipeline_id.is_none() {
            return;
        }
        self.pipeline_combo.close();
        self.controls.numbers.blur();
        self.controls.angles.blur();
        self.controls.enums.close();
        self.controls.color_picker.close();
        self.controls.color_target = None;
        self.controls.color_rect = None;
        self.renaming = true;
        self.pipeline_name.set_focused(true);
    }

    fn graph_context_items(
        &self,
        menu: &GraphContextMenu,
        timeline: &TimelineState,
    ) -> Vec<ContextMenuItem<'static>> {
        let item = |label: &'static str, icon: Option<AppIcon>, enabled: bool| ContextMenuItem {
            label,
            shortcut: None,
            icon,
            enabled,
        };
        match &menu.target {
            GraphContextTarget::PipelineSelector => {
                let has_pipeline = self.pipeline_id.is_some();
                vec![
                    item("Rename Pipeline", Some(AppIcon::Rename), has_pipeline),
                    item("New Pipeline", Some(AppIcon::New), true),
                    item("Remove Pipeline", Some(AppIcon::Remove), has_pipeline),
                ]
            }
            GraphContextTarget::Property(key) => {
                let GraphNodeTarget::Shared(node) = key.target else {
                    return Vec::new();
                };
                if timeline.pipeline_input_is_override(node, &key.input) {
                    vec![item("Use Shared Value", None, true)]
                } else {
                    vec![item("Make Unique", None, true)]
                }
            }
            GraphContextTarget::Node(GraphNodeTarget::Input | GraphNodeTarget::Output) => {
                Vec::new()
            }
            GraphContextTarget::Node(_) => vec![item("Delete Node", Some(AppIcon::Delete), true)],
            GraphContextTarget::Wire(_) => {
                vec![item("Delete Connection", Some(AppIcon::Delete), true)]
            }
        }
    }

    fn graph_context_action(
        &mut self,
        target: GraphContextTarget,
        index: usize,
        timeline: &TimelineState,
    ) -> PipelineGraphAction {
        match target {
            GraphContextTarget::PipelineSelector => match index {
                0 => {
                    self.begin_pipeline_rename();
                    PipelineGraphAction::None
                }
                1 => PipelineGraphAction::Create,
                2 => self.pipeline_id.map_or(
                    PipelineGraphAction::None,
                    PipelineGraphAction::RemovePipeline,
                ),
                _ => PipelineGraphAction::None,
            },
            GraphContextTarget::Property(key) => {
                if index != 0 {
                    return PipelineGraphAction::None;
                }
                let GraphNodeTarget::Shared(node) = key.target else {
                    return PipelineGraphAction::None;
                };
                if timeline.pipeline_input_is_override(node, &key.input) {
                    PipelineGraphAction::UseSharedInput {
                        node,
                        input: key.input,
                    }
                } else {
                    PipelineGraphAction::MakeInputUnique {
                        node,
                        input: key.input,
                    }
                }
            }
            GraphContextTarget::Node(GraphNodeTarget::Input | GraphNodeTarget::Output) => {
                PipelineGraphAction::None
            }
            GraphContextTarget::Node(target) if index == 0 => PipelineGraphAction::Remove(target),
            GraphContextTarget::Wire(wire) if index == 0 => PipelineGraphAction::DeleteWire(wire),
            GraphContextTarget::Node(_) | GraphContextTarget::Wire(_) => PipelineGraphAction::None,
        }
    }

    fn build_graph_context_menu(
        &self,
        ctx: &mut kama_ui::BuildCtx,
        rect: Rect,
        timeline: &TimelineState,
        icons: Icons,
    ) {
        let Some(menu) = self.context_menu.as_ref() else {
            return;
        };
        let items = self.graph_context_items(menu, timeline);
        if items.is_empty() {
            return;
        }
        let menu_rect = context_menu_rect(rect, menu.point, items.len());
        build_context_menu(ctx, "pipeline-graph", menu_rect, self.cursor, &items, icons);
    }

    fn reset_interaction(&mut self, reset_view: bool) {
        self.clear_node_selection();
        self.selected_wire = None;
        self.pending_wire = None;
        self.pending_cursor = None;
        self.context_menu = None;
        self.group_drag = None;
        self.block_selection = None;
        if reset_view {
            self.z_order.clear();
            self.renaming = false;
        }
    }

    pub fn clear_selection(&mut self) {
        self.reset_interaction(false);
    }

    pub fn follow_selection(&mut self) {
        self.pinned_pipeline = None;
        self.reset_interaction(true);
    }

    pub fn open_pipeline(&mut self, pipeline: u64) {
        self.pinned_pipeline = Some(pipeline);
        self.reset_interaction(true);
    }

    pub fn graph_kind(&self, project: &Project, timeline: &TimelineState) -> PipelineKind {
        self.pinned_pipeline
            .and_then(|id| project.pipeline(id))
            .map_or_else(
                || timeline.selected_pipeline_kind(),
                |pipeline| pipeline.kind,
            )
    }

    pub fn target_pipeline(&self, project: &Project, timeline: &TimelineState) -> Option<u64> {
        let kind = self.graph_kind(project, timeline);
        self.pinned_pipeline
            .filter(|id| {
                project
                    .pipeline(*id)
                    .is_some_and(|pipeline| pipeline.kind == kind)
            })
            .or_else(|| {
                timeline
                    .selected_pipeline()
                    .and_then(|instance| instance.pipeline)
                    .filter(|id| {
                        project
                            .pipeline(*id)
                            .is_some_and(|pipeline| pipeline.kind == kind)
                    })
            })
    }

    pub fn is_pinned(&self) -> bool {
        self.pinned_pipeline.is_some()
    }

    pub fn insertion_position(&self, rect: Rect, point: [f32; 2]) -> [f32; 2] {
        let canvas = graph_canvas_rect(rect);
        let world = graph_screen_to_world(canvas, self.pan, self.zoom, point);
        [
            world[0] - GRAPH_CARD_W * 0.5,
            world[1] - GRAPH_CARD_BASE_H * 0.5,
        ]
    }

    fn begin_wire(&mut self, wire: PendingGraphWire, rect: Rect, point: [f32; 2]) {
        self.pending_wire = Some(wire);
        self.pending_cursor = Some([point[0] - rect.x, point[1] - rect.y]);
        self.selected_wire = None;
    }

    fn sync_pipeline_name(&mut self, project: &Project, timeline: &TimelineState) {
        let pipeline_id = self.target_pipeline(project, timeline);
        let changed = self.pipeline_id != pipeline_id;
        if changed {
            self.pipeline_id = pipeline_id;
            self.renaming = false;
        }
        let name = pipeline_id
            .and_then(|id| project.pipeline(id))
            .map_or("", |pipeline| pipeline.name.as_str());
        sync_text_edit(&mut self.pipeline_name, changed, name);
        let selected = self
            .pinned_pipeline
            .and_then(|id| {
                project
                    .pipelines
                    .iter()
                    .position(|pipeline| pipeline.id == id)
            })
            .map_or(0, |index| index + 1);
        if !self.pipeline_combo.is_open() {
            self.pipeline_combo.set_selected(selected);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn build_graph_numeric(
        &mut self,
        ctx: &mut kama_ui::BuildCtx,
        target: GraphControlTarget,
        value: GpuValue,
        link_rect: impl FnOnce() -> Rect,
        component_rect: impl Fn(usize, usize, bool) -> Rect,
        link_id: impl std::fmt::Display,
        component_id: impl Fn(usize) -> String,
        settings: NumberSettings<'_>,
        link_icons: (IconId, IconId),
        style: Style,
    ) {
        let count = value.component_count();
        let linkable = value.components_linkable();
        let linked = self.property_links.contains(&target);
        if linkable {
            let rect = link_rect();
            self.property_link_rects.insert(target.clone(), rect);
            let link_id = link_id.to_string();
            toggle_icon_button(
                ctx,
                &link_id,
                rect,
                if linked { link_icons.0 } else { link_icons.1 },
                linked,
                if linked {
                    "Linked components"
                } else {
                    "Unlinked components"
                },
                style,
            );
        }
        for component in 0..count {
            self.controls.numbers.build(
                ctx,
                component_id(component),
                (target.clone(), component),
                (
                    component_rect(component, count, linkable),
                    value.numeric(Some(component)).unwrap_or_default(),
                    settings,
                ),
                style,
            );
        }
    }

    fn build_graph_color(
        &mut self,
        ctx: &mut kama_ui::BuildCtx,
        target: GraphControlTarget,
        layout: (Rect, Rect),
        color: [f32; 4],
        id: impl std::fmt::Display,
        style: Style,
    ) {
        let (swatch, _bounds) = layout;
        ColorButton::build(ctx, id, swatch, ui_color(color), style);
        self.color_swatch_rects.insert(target.clone(), swatch);
        if self.controls.color_target.as_ref() == Some(&target) {
            self.controls.color_rect = Some(swatch);
            if !self.controls.color_picker.is_dragging() {
                self.controls.color_picker.set_linear(color);
            }
            self.controls.color_picker.build_in(
                ctx,
                "graph-color-picker",
                swatch,
                self.controls.popup_bounds,
                style,
            );
        }
    }

    fn build_node_property(
        &mut self,
        ctx: &mut kama_ui::BuildCtx,
        card: &GraphCard,
        layout: (Rect, Rect, usize),
        property: (GraphNodeTarget, &str, Option<GpuValue>, bool),
        icons: Icons,
    ) {
        let chevron = icons.get(AppIcon::Chevron);
        let (bounds, card_rect, input_index) = layout;
        let (target, name, value, unique) = property;
        let definition = card
            .inputs
            .get(input_index)
            .and_then(|input| input.definition.as_ref());
        let style = graph_component_style(self.zoom);
        let display_name = definition
            .map(|definition| definition.name.as_str())
            .unwrap_or(name);
        let label = graph_property_label_rect(card_rect, card, input_index);
        let (name_label, unique_marker) = graph_property_label_parts(label, unique, self.zoom);
        ui_text!(
            ctx,
            ("pipeline-input-label", target, input_index),
            name_label,
            8.0 * self.zoom,
            theme::muted(),
            display_name,
        );
        if let Some(marker) = unique_marker {
            kama_ui::ui!(ctx, {
                Rect(("pipeline-input-unique", target, input_index), marker) {
                    font_size: 8.0 * self.zoom; text_color: theme::muted(); text_centered; text: "*";
                }
            });
        }

        let Some(value) = value else {
            let value_rect = graph_property_value_area(card_rect, card, input_index, false);
            ui_text!(
                ctx,
                ("pipeline-linked-label", target, input_index),
                value_rect,
                7.6 * self.zoom,
                Color::rgb8(0xb7, 0x8d, 0xff),
                "connected",
            );
            return;
        };

        if self.zoom < GRAPH_CONTROLS_MIN_ZOOM {
            let value_rect = graph_property_value_area(card_rect, card, input_index, false);
            kama_ui::ui!(ctx, {
                Rect(("pipeline-property-summary", target, input_index), value_rect) {
                    font_size: 7.0 * self.zoom; text_color: theme::text(); text: graph_value_summary(value, definition);
                }
            });
            return;
        }

        let key = GraphPropertyKey {
            target,
            input: name.to_string(),
        };
        match definition.map(|definition| definition.ty) {
            Some(InputType::Enum) | Some(InputType::Bool) => {
                let is_bool = definition.is_some_and(|definition| definition.ty == InputType::Bool);
                let options = if is_bool {
                    vec!["Off", "On"]
                } else {
                    definition.map_or_else(Vec::new, |definition| {
                        definition.options.iter().map(String::as_str).collect()
                    })
                };
                let selected = if is_bool {
                    value.bool().unwrap_or(false) as usize
                } else {
                    value.enum_index().unwrap_or(0) as usize
                }
                .min(options.len().saturating_sub(1));
                self.controls.enums.build(
                    ctx,
                    FormatKey::new(format_args!("graph-enum-{target:?}-{name}")),
                    key.clone(),
                    (
                        graph_property_value_area(card_rect, card, input_index, false),
                        selected,
                    ),
                    &options,
                    (chevron, style),
                    self.controls.popup_bounds,
                );
            }
            Some(InputType::Color) => self.build_graph_color(
                ctx,
                GraphControlTarget::Property(key.clone()),
                (
                    graph_color_swatch_rect(card_rect, card, input_index),
                    bounds,
                ),
                value.color().unwrap_or([0.0, 0.0, 0.0, 1.0]),
                FormatKey::new(format_args!("graph-color-{target:?}-{name}")),
                style,
            ),
            Some(InputType::Angle) => {
                let angle = value.f32().unwrap_or(0.0);
                self.controls.angles.build(
                    ctx,
                    key,
                    &format!("graph-angle-{target:?}-{name}"),
                    graph_angle_parts(card_rect, card, input_index),
                    (angle, 1000.0, false),
                    style,
                );
            }
            _ => self.build_graph_numeric(
                ctx,
                GraphControlTarget::Property(key),
                value,
                || graph_property_link_rect(card_rect, card, input_index),
                |component, count, linkable| {
                    graph_property_component_rect(
                        card_rect,
                        card,
                        input_index,
                        component,
                        count,
                        linkable,
                    )
                },
                FormatKey::new(format_args!("graph-property-link-{target:?}-{name}")),
                |component| format!("graph-property-{target:?}-{name}-{component}"),
                graph_number_settings(value, definition),
                (icons.get(AppIcon::Link), icons.get(AppIcon::Unlink)),
                style,
            ),
        }
    }

    pub fn build(
        &mut self,
        ctx: &mut kama_ui::BuildCtx,
        rect: Rect,
        project: &Project,
        timeline: &TimelineState,
        plugins: &PluginRegistry,
        icons: Icons,
        popup_bounds: Rect,
    ) {
        let chevron = icons.get(AppIcon::Chevron);
        let graph = GraphModel::new(project, timeline, self.pinned_pipeline);
        self.sync_pipeline_name(project, timeline);
        self.last_rect = Some(rect);
        self.last_pipeline_count = project.pipelines.len();
        self.controls.clear_layout();
        self.controls.popup_bounds = popup_bounds;
        self.property_link_rects.clear();
        self.controls.color_rect = None;
        self.color_swatch_rects.clear();
        self.host_gradient_colors.clear();
        self.host_eq_values.clear();
        self.host_eq_scroll_rects.clear();
        self.host_eq_keyframe_rects.clear();
        let rect = Rect::new(0.0, 0.0, rect.width, rect.height);
        let toolbar = graph_toolbar_layout(rect);
        let kind = self.graph_kind(project, timeline);
        let graph_style = graph_component_style(self.zoom);
        let toolbar_style = crate::widgets::component_style();
        kama_ui::ui!(ctx, {
            Rect("pipeline-toolbar-layer", toolbar.bar) { overlay; overflow_visible; children: |ctx| {
                let followed = self
                    .target_pipeline(project, timeline)
                    .and_then(|id| project.pipeline(id))
                    .map_or("selection".to_string(), |pipeline| pipeline.name.clone());
                let mut option_names = Vec::with_capacity(project.pipelines.len() + 1);
                option_names.push(format!("Follow instance: {followed}"));
                option_names.extend(project.pipelines.iter().map(|pipeline| {
                    let kind = if pipeline.kind == PipelineKind::Audio {
                        "Audio"
                    } else {
                        "Video"
                    };
                    format!("{kind}: {}", pipeline.name)
                }));
                let option_refs = option_names.iter().map(String::as_str).collect::<Vec<_>>();
                if self.renaming && self.pipeline_id.is_some() {
                    self.pipeline_name.build(
                        ctx,
                        "graph-pipeline-name",
                        toolbar.combo,
                        if kind == PipelineKind::Audio {
                            "Audio Pipeline"
                        } else {
                            "Effect Pipeline"
                        },
                        toolbar_style,
                    );
                } else {
                    self.pipeline_combo.build_in(
                        ctx,
                        "graph-pipeline-combo",
                        toolbar.combo,
                        &option_refs,
                        chevron,
                        popup_bounds,
                        toolbar_style,
                    );
                }
            }; }
        });

        let canvas = graph_canvas_rect(rect);
        kama_ui::ui!(ctx, {
            Rect("pipeline-canvas", canvas) {
                fill: theme::timeline_bg();
            }
        });
        draw_graph_grid(ctx, canvas, self.pan, self.zoom);

        if let Some(selection) = &self.block_selection {
            let area = graph_selection_rect(selection.start, selection.current);
            kama_ui::ui!(ctx, {
                Rect("pipeline-block-selection", area) {
                    fill: Color::rgba(theme::accent().r, theme::accent().g, theme::accent().b, 0.10);
                    border: 1; border_color: theme::accent();
                }
            });
        }

        let scene = self.scene(rect, project, timeline, plugins);
        if scene.cards.is_empty() {
            kama_ui::ui!(ctx, {
                @if self.pan_drag.is_some() {
                    Rect("pipeline-pan-cursor", canvas) {
                        cursor: CursorShape::Grabbing;
                    }
                }
            });
            self.build_graph_context_menu(ctx, rect, timeline, icons);
            return;
        }

        for (index, geometry) in scene.wires.iter().enumerate() {
            let selected = self.selected_wire.as_ref() == Some(&geometry.wire);
            draw_graph_curve(
                ctx,
                index,
                geometry.from,
                geometry.to,
                if selected {
                    Color::rgb8(0xff, 0xc4, 0x54)
                } else if geometry.editable {
                    theme::accent()
                } else {
                    Color::rgb8(0x68, 0x78, 0x86)
                },
                self.zoom,
            );
        }

        if let (Some(pending), Some(cursor)) = (self.pending_wire, self.pending_cursor) {
            if let Some(from) =
                pending_graph_source_point(pending, graph, &scene.cards, &scene.rects)
            {
                draw_graph_curve(
                    ctx,
                    50_000,
                    from,
                    cursor,
                    Color::rgb8(0xff, 0xd0, 0x78),
                    self.zoom,
                );
            }
        }

        for index in graph_card_draw_order(&scene.cards, &self.z_order) {
            let card = &scene.cards[index];
            let card_rect = scene.rects[index];
            let target = card.kind;
            let selected =
                self.selected_node == Some(target) || self.selected_nodes.contains(&target);
            let card_scale = graph_card_scale(card_rect);
            let (header, title) = graph_card_header_parts(card_rect);
            kama_ui::ui!(ctx, {
                Rect(("pipeline-node", index), card_rect) {
                    fill: if selected { theme::control().mix(theme::accent(), 0.12) } else { theme::control() };
                    border: if selected { 2 } else { 1 };
                    border_color: if selected { theme::accent() } else { theme::line() };
                    border_radius: (RADIUS_SM * card_scale).max(2.0);
                }
                Rect(("pipeline-node-header", index), header) {
                    fill: if selected {
                        theme::accent()
                    } else {
                        card.kind.header_color().mix(theme::control(), 0.18)
                    };
                    border_radius: (RADIUS_SM * card_scale).max(2.0);
                }
                Rect(("pipeline-node-title", index), title) {
                    font_size: 9.5 * card_scale;
                    text_color: if selected { Color::BLACK } else { theme::text() };
                    text: card.label.clone();
                }
            });

            match card.kind {
                GraphNodeTarget::Output => {
                    graph_port(
                        ctx,
                        ("pipeline-input-port", index),
                        graph_image_input_port(card_rect),
                        theme::accent(),
                        (5.0 * card_scale).max(2.0),
                    );
                    graph_label(
                        ctx,
                        ("pipeline-output-input-label", index),
                        graph_image_input_label_rect(card_rect, 0),
                        card_scale,
                        graph_image_socket_label(kind),
                    );
                }
                GraphNodeTarget::Value(id) => {
                    graph_port(
                        ctx,
                        ("pipeline-value-output", index),
                        graph_value_output_port(card_rect),
                        Color::rgb8(0xb7, 0x8d, 0xff),
                        (5.0 * card_scale).max(2.0),
                    );
                    graph_output_label(
                        ctx,
                        ("pipeline-value-output-label", index),
                        graph_output_label_rect(card_rect),
                        card_scale,
                        "Value",
                    );
                    if let Some(value_node) = graph.value_node(id) {
                        for (input_index, input) in card.inputs.iter().enumerate() {
                            graph_port(
                                ctx,
                                ("pipeline-value-input", index, input_index),
                                graph_value_input_port(card_rect, input_index),
                                Color::rgb8(0xb7, 0x8d, 0xff),
                                (5.0 * card_scale).max(2.0),
                            );
                            graph_label(
                                ctx,
                                ("pipeline-value-input-label", index, input_index),
                                graph_value_input_label_rect(card_rect, input_index),
                                card_scale,
                                &input.name,
                            );
                            let target = GraphNodeTarget::Value(id);
                            let binding = value_node.inputs.get(&input.name);
                            if let Some(value) = binding.and_then(|binding| {
                                binding.evaluate(timeline.selected_keyframe_time())
                            }) {
                                self.build_graph_numeric(
                                    ctx,
                                    GraphControlTarget::Property(GraphPropertyKey {
                                        target,
                                        input: input.name.clone(),
                                    }),
                                    value,
                                    || graph_value_input_link_rect(card_rect, input_index),
                                    |component, count, linkable| {
                                        graph_value_input_component_rect(
                                            card_rect,
                                            input_index,
                                            component,
                                            count,
                                            linkable,
                                        )
                                    },
                                    FormatKey::new(format_args!(
                                        "graph-value-input-link-{id}-{input_index}"
                                    )),
                                    |component| {
                                        format!("graph-value-input-{id}-{input_index}-{component}")
                                    },
                                    ((f64::NEG_INFINITY, f64::INFINITY), 1.0, 3, ""),
                                    (icons.get(AppIcon::Link), icons.get(AppIcon::Unlink)),
                                    graph_style,
                                );
                            } else {
                                ui_text!(
                                    ctx,
                                    ("pipeline-value-input-connected", index, input_index),
                                    graph_value_input_area(card_rect, input_index, false),
                                    7.6 * card_scale,
                                    Color::rgb8(0xb7, 0x8d, 0xff),
                                    "connected"
                                );
                            }
                        }
                        if value_node.kind.is_constant() {
                            let color =
                                matches!(value_node.kind, crate::effects::ValueNodeKind::Color);
                            if let (true, GpuValue::Color(value)) = (color, value_node.value) {
                                self.build_graph_color(
                                    ctx,
                                    GraphControlTarget::ValueNode(id),
                                    (graph_value_swatch_rect(card_rect), rect),
                                    value,
                                    FormatKey::new(format_args!("graph-value-swatch-{id}")),
                                    graph_style,
                                );
                            }
                            self.build_graph_numeric(
                                ctx,
                                GraphControlTarget::ValueNode(id),
                                value_node.value,
                                || graph_value_link_rect(card_rect),
                                |component, _, linkable| {
                                    graph_value_component_rect(card_rect, component, linkable)
                                },
                                FormatKey::new(format_args!("graph-value-link-{id}")),
                                |component| format!("graph-value-{id}-{component}"),
                                (
                                    if color {
                                        (0.0, 1.0)
                                    } else {
                                        (f64::NEG_INFINITY, f64::INFINITY)
                                    },
                                    if color { 0.005 } else { 0.01 },
                                    3,
                                    "",
                                ),
                                (icons.get(AppIcon::Link), icons.get(AppIcon::Unlink)),
                                graph_style,
                            );
                        } else {
                            graph_label(
                                ctx,
                                ("pipeline-value-detail", index),
                                graph_value_detail_rect(card_rect),
                                card_scale,
                                value_node.kind.detail(),
                            );
                        }
                    }
                }
                GraphNodeTarget::Input | GraphNodeTarget::Local(_) | GraphNodeTarget::Shared(_) => {
                    for (image_index, image_input) in card.image_inputs.iter().enumerate() {
                        graph_port(
                            ctx,
                            ("pipeline-image-input", index, image_index),
                            graph_named_image_input_port(card_rect, image_index),
                            theme::muted(),
                            (5.0 * card_scale).max(2.0),
                        );
                        graph_label(
                            ctx,
                            ("pipeline-image-input-label", index, image_index),
                            graph_image_input_label_rect(card_rect, image_index),
                            card_scale,
                            friendly_name(image_input),
                        );
                    }
                    graph_port(
                        ctx,
                        ("pipeline-image-output", index),
                        graph_image_output_port(card_rect),
                        theme::muted(),
                        (5.0 * card_scale).max(2.0),
                    );
                    graph_output_label(
                        ctx,
                        ("pipeline-image-output-label", index),
                        graph_output_label_rect(card_rect),
                        card_scale,
                        graph_image_socket_label(kind),
                    );
                    for (input_index, input) in card.inputs.iter().enumerate() {
                        let name = input.name.as_str();
                        graph_port(
                            ctx,
                            ("pipeline-value-input", index, input_index),
                            graph_scalar_input_port(card_rect, card, input_index),
                            Color::rgb8(0xb7, 0x8d, 0xff),
                            (4.0 * self.zoom).max(2.0),
                        );
                        let value = graph_effect_value(card.kind, name, graph);
                        let unique = matches!(
                            card.kind,
                            GraphNodeTarget::Shared(id)
                                if self.pinned_pipeline.is_none()
                                    && timeline.pipeline_input_is_override(id, name)
                        );
                        self.build_node_property(
                            ctx,
                            card,
                            (rect, card_rect, input_index),
                            (target, name, value, unique),
                            icons,
                        );
                    }
                    for (host_index, input) in card.host_inputs.iter().enumerate() {
                        let name = input.name.as_str();
                        let row = graph_host_row_rect(card_rect, card, host_index);
                        let scale = graph_card_scale(card_rect);
                        let definition = input.definition.as_ref();
                        if definition.is_some_and(|definition| {
                            definition.ty == InputType::F32List && definition.id == "band_values"
                        }) {
                            let (label, keyframe, viewport) = graph_host_eq_parts(row, scale);
                            ui_text!(
                                ctx,
                                ("pipeline-host-input-label", index, host_index),
                                label,
                                7.6 * card_scale,
                                theme::muted(),
                                definition.map_or_else(
                                    || friendly_name(name),
                                    |input| input.name.clone()
                                ),
                            );
                            let keyframe_state = keyframe_control(
                                icons,
                                graph_host_has_keyframe(target, name, graph),
                                graph_host_has_keyframes(target, name, graph),
                            );
                            self.host_eq_keyframe_rects.insert(target, keyframe);
                            toggle_icon_button(
                                ctx,
                                &format!("pipeline-host-keyframe-{index}-{host_index}"),
                                keyframe,
                                keyframe_state.icon,
                                keyframe_state.keyed,
                                if keyframe_state.keyed {
                                    "Remove keyframe"
                                } else {
                                    "Add keyframe"
                                },
                                graph_style,
                            );

                            const BAND_COUNTS: [usize; 5] = [3, 5, 10, 15, 31];
                            let mode = graph_effect_value(card.kind, "band_count", graph)
                                .and_then(GpuValue::enum_index)
                                .unwrap_or(2) as usize;
                            let count = BAND_COUNTS[mode.min(BAND_COUNTS.len() - 1)];
                            let values = graph_host_value(target, name, graph)
                                .and_then(|value| match value {
                                    crate::project::HostValue::F32List(values) => Some(values),
                                    _ => None,
                                })
                                .unwrap_or_default();
                            self.host_eq_values.insert(target, values.clone());
                            let layout = build_graphic_eq_controls(
                                ctx,
                                &mut self.controls.sliders,
                                (
                                    ("pipeline-host-eq-bg", index, host_index),
                                    ("pipeline-host-eq-zero", index, host_index),
                                ),
                                GraphicEqBuild {
                                    viewport,
                                    count,
                                    scroll: *self.host_eq_scroll.get(&target).unwrap_or(&0.0),
                                    values: &values,
                                    min_slot_width: (18.0 * scale).max(12.0),
                                    radius: (4.0 * scale).max(2.0),
                                    zero_inset: 2.0,
                                    enabled: self.zoom >= GRAPH_CONTROLS_MIN_ZOOM,
                                    style: graph_style,
                                },
                                |band| (target, band),
                                |band| format!("graph-eq-{target:?}-{band}"),
                            );
                            self.host_eq_scroll.insert(target, layout.scroll);
                            self.host_eq_scroll_rects
                                .insert(target, (viewport, layout.max_scroll));
                            continue;
                        }

                        if definition.is_some_and(|definition| {
                            definition.ty == InputType::F32List
                                && definition.id == "colors"
                                && match card.kind {
                                    GraphNodeTarget::Input => {
                                        selected_generator_plugin_type(timeline)
                                            == Some(BUILTIN_GRADIENT_GENERATOR)
                                    }
                                    _ => graph.effect(card.kind).is_some_and(|node| {
                                        node.node_type == BUILTIN_GRADIENT_GENERATOR
                                    }),
                                }
                        }) {
                            let point_count = graph_host_value(target, "points", graph)
                                .and_then(|value| match value {
                                    crate::project::HostValue::Vec2Array(points) => {
                                        Some(points.len())
                                    }
                                    _ => None,
                                })
                                .unwrap_or(0);
                            let values = graph_host_value(target, name, graph)
                                .and_then(|value| match value {
                                    crate::project::HostValue::F32List(values) => Some(values),
                                    _ => None,
                                })
                                .unwrap_or_default();
                            let colors = colors_from_values(&values, point_count);
                            let color_values = colors_to_values(&colors);
                            let rows = crate::ui_layout::column(
                                row,
                                &vec![
                                    crate::ui_layout::Item::height(GRAPH_INPUT_H * scale);
                                    colors.len() + 1
                                ],
                                0.0,
                                0.0,
                                kama_ui::Align::Start,
                                None,
                            );
                            let (label, _) = graph_host_value_parts(rows[0], scale);
                            ui_text!(
                                ctx,
                                ("pipeline-host-input-label", index, host_index),
                                label,
                                7.6 * card_scale,
                                theme::muted(),
                                definition.map_or_else(
                                    || friendly_name(name),
                                    |input| input.name.clone()
                                ),
                            );
                            for (stop_index, (color, stop_row)) in
                                colors.into_iter().zip(rows.into_iter().skip(1)).enumerate()
                            {
                                let (stop_label, value_rect) =
                                    graph_host_value_parts(stop_row, scale);
                                ui_text!(
                                    ctx,
                                    (
                                        "pipeline-gradient-stop-label",
                                        index,
                                        host_index,
                                        stop_index
                                    ),
                                    stop_label,
                                    7.2 * card_scale,
                                    theme::muted(),
                                    &format!("Stop {}", stop_index + 1),
                                );
                                let swatch = Rect::new(
                                    value_rect.x,
                                    value_rect.y + 2.0 * scale,
                                    value_rect.width,
                                    (value_rect.height - 4.0 * scale).max(1.0),
                                );
                                let color_target = GraphControlTarget::HostGradientStop {
                                    target,
                                    input: name.to_string(),
                                    index: stop_index,
                                };
                                self.host_gradient_colors
                                    .insert(color_target.clone(), color_values.clone());
                                self.build_graph_color(
                                    ctx,
                                    color_target,
                                    (swatch, rect),
                                    color,
                                    FormatKey::new(format_args!(
                                        "graph-gradient-stop-{target:?}-{stop_index}"
                                    )),
                                    graph_style,
                                );
                            }
                            continue;
                        }

                        let (label, value_rect) = graph_host_value_parts(row, scale);
                        ui_text!(
                            ctx,
                            ("pipeline-host-input-label", index, host_index),
                            label,
                            7.6 * card_scale,
                            theme::muted(),
                            definition
                                .map_or_else(|| friendly_name(name), |input| input.name.clone()),
                        );
                        let point_mode = match card.kind {
                            GraphNodeTarget::Input => selected_generator_plugin_type(timeline)
                                .and_then(|generator| plugins.generator(generator))
                                .and_then(|generator| {
                                    generator.inputs.iter().find(|input| input.id == *name)
                                }),
                            _ => graph
                                .effect(card.kind)
                                .and_then(|node| plugins.generator(&node.node_type))
                                .and_then(|generator| {
                                    generator.inputs.iter().find(|input| input.id == *name)
                                }),
                        }
                        .map(|input| (input.monitor_handle.is_some(), input.pen_tool))
                        .unwrap_or((false, false));
                        let summary = host_value_summary(
                            graph_host_value(target, name, graph),
                            point_mode.0,
                            point_mode.1,
                        );
                        ui_text!(
                            ctx,
                            ("pipeline-host-input-value", index, host_index),
                            value_rect,
                            7.6 * card_scale,
                            theme::text(),
                            summary,
                        );
                    }
                }
            }
        }

        if self.pan_drag.is_some() {
            kama_ui::ui!(ctx, {
                Rect("pipeline-pan-cursor", canvas) {
                    cursor: CursorShape::Grabbing;
                }
            });
        }

        self.build_graph_context_menu(ctx, rect, timeline, icons);
    }

    fn set_graphic_eq_host_band(
        &mut self,
        target: GraphNodeTarget,
        index: usize,
        normalized: f32,
    ) -> PipelineGraphAction {
        let values = self
            .host_eq_values
            .entry(target)
            .or_insert_with(|| vec![0.0; 31]);
        set_eq_band(values, index, normalized);
        PipelineGraphAction::SetHostValue {
            target,
            input: "band_values".into(),
            value: crate::project::HostValue::F32List(values.clone()),
        }
    }

    pub fn pointer_pressed(
        &mut self,
        rect: Rect,
        point: [f32; 2],
        modifiers: ModifiersState,
        project: &Project,
        timeline: &TimelineState,
        plugins: &PluginRegistry,
    ) -> PipelineGraphAction {
        let graph = GraphModel::new(project, timeline, self.pinned_pipeline);
        if let Some(menu) = self.context_menu.clone() {
            let items = self.graph_context_items(&menu, timeline);
            let menu_rect = context_menu_rect(rect, menu.point, items.len());
            if let Some(index) = context_menu_hit(menu_rect, point, items.len()) {
                let enabled = items.get(index).is_some_and(|item| item.enabled);
                self.context_menu = None;
                return if enabled {
                    self.graph_context_action(menu.target, index, timeline)
                } else {
                    PipelineGraphAction::None
                };
            }
            self.context_menu = None;
        }
        let toolbar = graph_toolbar_layout(rect);
        let option_count = project.pipelines.len() + 1;
        if !self.renaming {
            if let Some(index) = self
                .pipeline_combo
                .option_at(toolbar.combo, point, option_count)
            {
                self.pipeline_combo.select(index, true);
                self.renaming = false;
                return if index == 0 {
                    PipelineGraphAction::SelectPipeline(None)
                } else {
                    PipelineGraphAction::SelectPipeline(
                        project.pipelines.get(index - 1).map(|pipeline| pipeline.id),
                    )
                };
            }
        }
        if !rect.contains(point) {
            self.pipeline_combo.close();
            return PipelineGraphAction::None;
        }
        if self.renaming && self.pipeline_id.is_some() && toolbar.combo.contains(point) {
            self.pipeline_name
                .pointer_pressed(toolbar.combo, point, modifiers);
            return PipelineGraphAction::None;
        }
        if toolbar.combo.contains(point) && !self.renaming {
            self.pipeline_combo.toggle();
            return PipelineGraphAction::None;
        }
        self.pipeline_combo.close();

        let local_point = [point[0] - rect.x, point[1] - rect.y];
        let local_bounds = self.controls.popup_bounds;

        if let (Some(target), Some(swatch)) =
            (self.controls.color_target.clone(), self.controls.color_rect)
        {
            let before = self.controls.color_picker.linear();
            if self.controls.color_picker.pointer_pressed_in(
                swatch,
                local_bounds,
                local_point,
                modifiers,
            ) {
                let after = self.controls.color_picker.linear();
                return if after != before {
                    self.graph_color_action(&target, after)
                } else {
                    PipelineGraphAction::None
                };
            }
        }

        if let Some((key, index)) = self.controls.enums.select_option(rect, point) {
            self.pipeline_name.set_focused(false);
            self.renaming = false;
            self.controls.numbers.blur();
            self.controls.angles.blur();
            let is_bool = graph_property_definition(key.target, &key.input, graph, plugins)
                .is_some_and(|definition| definition.ty == InputType::Bool);
            return PipelineGraphAction::SetEffectValue {
                target: key.target,
                input: key.input,
                value: if is_bool {
                    GpuValue::Bool(index != 0)
                } else {
                    GpuValue::Enum(index as u32)
                },
            };
        }
        if let Some(key) = self.controls.enums.toggle_at(rect, point) {
            self.pipeline_name.set_focused(false);
            self.renaming = false;
            self.controls.numbers.blur();
            self.controls.angles.blur();
            self.select_single_node(key.target);
            self.bring_to_front(key.target);
            return PipelineGraphAction::None;
        }
        self.controls.enums.close();

        if let Some((key, value)) = self.controls.angles.pointer_pressed(rect, point, modifiers) {
            self.pipeline_name.set_focused(false);
            self.renaming = false;
            self.controls.numbers.blur();
            self.select_single_node(key.target);
            self.bring_to_front(key.target);
            return value.map_or(PipelineGraphAction::None, |value| {
                PipelineGraphAction::SetEffectValue {
                    target: key.target,
                    input: key.input,
                    value: GpuValue::F32(value),
                }
            });
        }

        if let Some((target, absolute)) = hit_local(&self.color_swatch_rects, rect, point) {
            let local = offset_rect(absolute, -rect.x, -rect.y);
            let color = match &target {
                GraphControlTarget::Property(key) => {
                    graph_target_value(key.target, &key.input, graph).and_then(GpuValue::color)
                }
                GraphControlTarget::ValueNode(node) => self
                    .target_pipeline(project, timeline)
                    .and_then(|pipeline| project.pipeline(pipeline))
                    .and_then(|pipeline| pipeline.value_node(*node))
                    .and_then(|node| node.value.color()),
                GraphControlTarget::HostGradientStop { index, .. } => self
                    .host_gradient_colors
                    .get(&target)
                    .and_then(|values| values.get(index * 4..index * 4 + 4))
                    .map(|value| [value[0], value[1], value[2], value[3]]),
            }
            .unwrap_or([0.0, 0.0, 0.0, 1.0]);
            self.pipeline_name.set_focused(false);
            self.renaming = false;
            self.controls.numbers.blur();
            self.controls.angles.blur();
            self.controls.color_target = Some(target.clone());
            self.controls.color_rect = Some(local);
            self.controls.color_picker.close();
            self.controls.color_picker.set_linear(color);
            let _ = self.controls.color_picker.pointer_pressed_in(
                local,
                local_bounds,
                local_point,
                modifiers,
            );
            let node_target = target.node();
            self.select_single_node(node_target);
            self.bring_to_front(node_target);
            return PipelineGraphAction::None;
        }
        if self.controls.color_target.take().is_some() {
            self.controls.color_picker.close();
            self.controls.color_rect = None;
        }

        if let Some((target, _)) = hit_local(&self.host_eq_keyframe_rects, rect, point) {
            self.select_single_node(target);
            self.bring_to_front(target);
            return PipelineGraphAction::ToggleHostKeyframe {
                target,
                input: "band_values".into(),
            };
        }
        if let Some((key, value)) = self.controls.sliders.pointer_pressed(rect, point) {
            self.select_single_node(key.0);
            self.bring_to_front(key.0);
            return self.set_graphic_eq_host_band(key.0, key.1, value);
        }

        if let Some((target, _)) = hit_local(&self.property_link_rects, rect, point) {
            if !self.property_links.insert(target.clone()) {
                self.property_links.remove(&target);
            }
            let node = target.node();
            self.select_single_node(node);
            self.bring_to_front(node);
            return PipelineGraphAction::None;
        }

        if let Some(((target, component), value)) = self
            .controls
            .numbers
            .pointer_pressed(rect, point, modifiers)
        {
            self.pipeline_name.set_focused(false);
            self.renaming = false;
            self.controls.angles.blur();
            let node = target.node();
            self.select_single_node(node);
            self.bring_to_front(node);
            return value.map_or(PipelineGraphAction::None, |value| {
                target.component_action(
                    component,
                    value as f32,
                    self.property_links.contains(&target),
                )
            });
        }

        let scene = self.scene(rect, project, timeline, plugins);
        if scene.cards.is_empty() {
            return PipelineGraphAction::None;
        }

        if let Some((pending, wire)) = occupied_graph_input(&scene, &self.z_order, point) {
            self.begin_wire(pending, rect, point);
            return PipelineGraphAction::DeleteWire(wire);
        }

        let shared_input = self.pinned_pipeline.is_some()
            || timeline
                .selected_pipeline()
                .and_then(|instance| instance.pipeline)
                .is_some();
        if let Some(pending) = graph_output_at(
            &scene.cards,
            &scene.rects,
            &self.z_order,
            point,
            shared_input,
        ) {
            self.begin_wire(pending, rect, point);
            return PipelineGraphAction::None;
        }

        let draw_order = graph_card_draw_order(&scene.cards, &self.z_order);
        for index in draw_order.into_iter().rev() {
            let card = &scene.cards[index];
            let card_rect = scene.rects[index];
            if card_rect.contains(point) {
                let target = card.kind;
                let command_additive = modifiers.control_key() || modifiers.super_key();
                if command_additive && self.selected_nodes.contains(&target) {
                    self.selected_nodes.remove(&target);
                    self.selected_node = self.selected_targets().first().copied();
                    self.selected_wire = None;
                    self.drag = None;
                    self.group_drag = None;
                    return PipelineGraphAction::None;
                }
                if command_additive || modifiers.shift_key() {
                    self.selected_nodes.insert(target);
                    self.selected_node = Some(target);
                    self.selected_wire = None;
                } else if !self.selected_nodes.contains(&target) || self.selected_nodes.len() <= 1 {
                    self.select_single_node(target);
                } else {
                    self.selected_node = Some(target);
                    self.selected_wire = None;
                }
                self.bring_to_front(target);
                if self.selected_nodes.len() > 1 {
                    let canvas = graph_canvas_rect(rect);
                    let positions = scene
                        .cards
                        .iter()
                        .filter(|card| self.selected_nodes.contains(&card.kind))
                        .map(|card| {
                            (
                                card.kind,
                                graph_node_world_position(
                                    card,
                                    graph,
                                    self.local_input_position,
                                    self.local_output_position,
                                ),
                            )
                        })
                        .collect();
                    self.drag = None;
                    self.group_drag = Some(GraphGroupDrag {
                        start_world: graph_screen_to_world(canvas, self.pan, self.zoom, point),
                        positions,
                    });
                } else {
                    self.group_drag = None;
                    self.drag = Some(GraphDrag {
                        target,
                        world_offset: [
                            (point[0] - card_rect.x) / self.zoom.max(0.000_1),
                            (point[1] - card_rect.y) / self.zoom.max(0.000_1),
                        ],
                    });
                }
                return PipelineGraphAction::None;
            }
        }
        if let Some(geometry) = scene.wires.iter().find(|geometry| {
            geometry.editable && graph_wire_hit(geometry.from, geometry.to, point, self.zoom)
        }) {
            self.selected_wire = Some(geometry.wire.clone());
            self.clear_node_selection();
            self.drag = None;
            self.group_drag = None;
            return PipelineGraphAction::None;
        }
        let canvas = graph_canvas_rect(rect);
        if canvas.contains(point) {
            let additive =
                modifiers.shift_key() || modifiers.control_key() || modifiers.super_key();
            let base = if additive {
                self.selected_nodes.clone()
            } else {
                HashSet::new()
            };
            if !additive {
                self.clear_node_selection();
            }
            self.block_selection = Some(GraphBlockSelection {
                start: local_point,
                current: local_point,
                base,
            });
        } else {
            self.clear_node_selection();
        }
        self.selected_wire = None;
        self.pending_wire = None;
        self.pending_cursor = None;
        self.drag = None;
        self.group_drag = None;
        PipelineGraphAction::None
    }

    pub fn pointer_right_pressed(
        &mut self,
        rect: Rect,
        point: [f32; 2],
        project: &Project,
        timeline: &TimelineState,
        plugins: &PluginRegistry,
    ) -> PipelineGraphAction {
        self.cursor = [point[0] - rect.x, point[1] - rect.y];
        let toolbar = graph_toolbar_layout(rect);
        if toolbar.combo.contains(point) {
            self.pipeline_combo.close();
            self.context_menu = Some(GraphContextMenu {
                point: self.cursor,
                target: GraphContextTarget::PipelineSelector,
            });
            return PipelineGraphAction::None;
        }
        let graph = GraphModel::new(project, timeline, self.pinned_pipeline);
        if !graph_canvas_rect(rect).contains(point) {
            self.context_menu = None;
            return PipelineGraphAction::None;
        }
        let scene = self.scene(rect, project, timeline, plugins);
        let draw_order = graph_card_draw_order(&scene.cards, &self.z_order);
        for index in draw_order.into_iter().rev() {
            let card = &scene.cards[index];
            let card_rect = scene.rects[index];
            if !card_rect.contains(point) {
                continue;
            }
            let target = card.kind;
            self.select_single_node(target);
            self.bring_to_front(target);
            let context_target = graph_unique_property_at(card, card_rect, point, graph)
                .map(GraphContextTarget::Property)
                .unwrap_or(GraphContextTarget::Node(target));
            if matches!(
                context_target,
                GraphContextTarget::Node(GraphNodeTarget::Input | GraphNodeTarget::Output)
            ) {
                self.context_menu = None;
                return PipelineGraphAction::None;
            }
            self.context_menu = Some(GraphContextMenu {
                point: self.cursor,
                target: context_target,
            });
            return PipelineGraphAction::None;
        }
        if let Some(wire) = scene
            .wires
            .iter()
            .rev()
            .find(|wire| wire.editable && graph_wire_hit(wire.from, wire.to, point, self.zoom))
        {
            self.clear_node_selection();
            self.selected_wire = Some(wire.wire.clone());
            self.context_menu = Some(GraphContextMenu {
                point: self.cursor,
                target: GraphContextTarget::Wire(wire.wire.clone()),
            });
            return PipelineGraphAction::None;
        }
        self.context_menu = None;
        self.clear_node_selection();
        self.selected_wire = None;
        PipelineGraphAction::InsertNode
    }

    pub fn pointer_moved(&mut self, rect: Rect, point: [f32; 2]) -> PipelineGraphAction {
        self.cursor = [point[0] - rect.x, point[1] - rect.y];
        if let Some(selection) = &mut self.block_selection {
            let canvas = graph_canvas_rect(Rect::new(0.0, 0.0, rect.width, rect.height));
            selection.current = [
                (point[0] - rect.x).clamp(canvas.x, canvas.right()),
                (point[1] - rect.y).clamp(canvas.y, canvas.bottom()),
            ];
            return PipelineGraphAction::None;
        }
        if let Some((key, value)) = self.controls.sliders.pointer_moved(point) {
            return self.set_graphic_eq_host_band(key.0, key.1, value);
        }
        if let Some((key, value)) = self.controls.angles.pointer_moved(point) {
            return PipelineGraphAction::SetEffectValue {
                target: key.target,
                input: key.input,
                value: GpuValue::F32(value),
            };
        }
        if let (Some(target), Some(swatch), Some(panel)) = (
            self.controls.color_target.clone(),
            self.controls.color_rect,
            self.last_rect,
        ) {
            let local = [point[0] - panel.x, point[1] - panel.y];
            let bounds = Rect::new(0.0, 0.0, panel.width, panel.height);
            let before = self.controls.color_picker.linear();
            if self
                .controls
                .color_picker
                .pointer_moved_in(swatch, bounds, local)
            {
                let after = self.controls.color_picker.linear();
                if after != before {
                    return self.graph_color_action(&target, after);
                }
                return PipelineGraphAction::None;
            }
        }
        if let Some(((target, component), value)) = self.controls.numbers.pointer_moved(point) {
            return target.component_action(
                component,
                value as f32,
                self.property_links.contains(&target),
            );
        }
        if let Some((start, pan)) = self.pan_drag {
            self.pan = [pan[0] + point[0] - start[0], pan[1] + point[1] - start[1]];
            return PipelineGraphAction::None;
        }
        if self.pending_wire.is_some() {
            self.pending_cursor = Some([point[0] - rect.x, point[1] - rect.y]);
            return PipelineGraphAction::None;
        }
        if let Some(drag) = &self.group_drag {
            let canvas = graph_canvas_rect(rect);
            let cursor_world = graph_screen_to_world(canvas, self.pan, self.zoom, point);
            let delta = [
                cursor_world[0] - drag.start_world[0],
                cursor_world[1] - drag.start_world[1],
            ];
            return PipelineGraphAction::MoveNodes(
                drag.positions
                    .iter()
                    .map(|(target, origin)| (*target, [origin[0] + delta[0], origin[1] + delta[1]]))
                    .collect(),
            );
        }
        let Some(drag) = self.drag else {
            return PipelineGraphAction::None;
        };
        let canvas = graph_canvas_rect(rect);
        let cursor_world = graph_screen_to_world(canvas, self.pan, self.zoom, point);
        let position = [
            cursor_world[0] - drag.world_offset[0],
            cursor_world[1] - drag.world_offset[1],
        ];
        PipelineGraphAction::MoveNode {
            target: drag.target,
            position,
        }
    }

    pub fn pointer_released(
        &mut self,
        rect: Rect,
        point: [f32; 2],
        project: &Project,
        timeline: &TimelineState,
        plugins: &PluginRegistry,
    ) -> PipelineGraphAction {
        self.controls.pointer_released();
        self.pipeline_name.pointer_released();
        self.pan_drag = None;

        let scene = self.scene(rect, project, timeline, plugins);

        if let Some(selection) = self.block_selection.take() {
            let local_area = graph_selection_rect(selection.start, selection.current);
            let area = offset_rect(local_area, rect.x, rect.y);
            let mut selected = selection.base;
            for (card, card_rect) in scene.cards.iter().zip(&scene.rects) {
                if graph_rects_intersect(*card_rect, area) {
                    selected.insert(card.kind);
                }
            }
            self.selected_nodes = selected;
            let draw_order = graph_card_draw_order(&scene.cards, &self.z_order)
                .into_iter()
                .rev()
                .map(|index| scene.cards[index].kind)
                .filter(|target| self.selected_nodes.contains(target))
                .collect::<Vec<_>>();
            self.selected_node = draw_order
                .iter()
                .copied()
                .find(|target| !matches!(target, GraphNodeTarget::Input | GraphNodeTarget::Output))
                .or_else(|| draw_order.first().copied());
            self.selected_wire = None;
            self.drag = None;
            self.group_drag = None;
            return PipelineGraphAction::None;
        }

        if let Some(pending) = self.pending_wire.take() {
            self.pending_cursor = None;
            let followed_shared = self.pinned_pipeline.is_none()
                && timeline
                    .selected_pipeline()
                    .is_some_and(|instance| instance.pipeline.is_some());
            let action = graph_card_draw_order(&scene.cards, &self.z_order)
                .into_iter()
                .rev()
                .find_map(|index| {
                    graph_connect_action(
                        pending,
                        &scene.cards[index],
                        scene.rects[index],
                        point,
                        followed_shared,
                    )
                })
                .unwrap_or(PipelineGraphAction::None);
            self.drag = None;
            return action;
        }

        if self.group_drag.take().is_some() {
            self.drag = None;
            return PipelineGraphAction::None;
        }

        let dragged = self.drag.take();
        if let Some(GraphDrag {
            target: GraphNodeTarget::Shared(node),
            ..
        }) = dragged
        {
            if let Some(card_index) = scene
                .cards
                .iter()
                .position(|card| matches!(card.kind, GraphNodeTarget::Shared(id) if id == node))
            {
                let dropped = scene.rects[card_index];
                if let Some((source, destination, destination_input)) = scene
                    .wires
                    .iter()
                    .filter(|geometry| geometry.editable)
                    .find_map(|geometry| match &geometry.wire {
                        GraphWire::Image {
                            source,
                            destination,
                            input,
                        } if *source != Some(node)
                            && *destination != Some(node)
                            && graph_wire_crosses_rect(
                                geometry.from,
                                geometry.to,
                                dropped,
                                self.zoom,
                            ) =>
                        {
                            Some((*source, *destination, input.clone()))
                        }
                        _ => None,
                    })
                {
                    return PipelineGraphAction::InsertNodeOnWire {
                        node,
                        source,
                        destination,
                        destination_input,
                    };
                }
            }
        }
        PipelineGraphAction::None
    }

    pub fn frame_all(
        &mut self,
        rect: Rect,
        project: &Project,
        timeline: &TimelineState,
        plugins: &PluginRegistry,
    ) -> bool {
        let graph = GraphModel::new(project, timeline, self.pinned_pipeline);
        let cards = graph_cards(graph, plugins);
        if cards.is_empty() {
            return false;
        }
        let canvas = graph_canvas_rect(rect);
        let model_rects = graph_card_rects(
            Rect::new(0.0, 0.0, canvas.width, canvas.height),
            &cards,
            graph,
            [0.0, 0.0],
            1.0,
            self.local_input_position,
            self.local_output_position,
        );
        let min_x = model_rects
            .iter()
            .map(|rect| rect.x)
            .fold(f32::INFINITY, f32::min);
        let min_y = model_rects
            .iter()
            .map(|rect| rect.y)
            .fold(f32::INFINITY, f32::min);
        let max_x = model_rects
            .iter()
            .map(|rect| rect.x)
            .fold(f32::NEG_INFINITY, f32::max);
        let max_y = model_rects
            .iter()
            .map(|rect| rect.y)
            .fold(f32::NEG_INFINITY, f32::max);
        let max_h = model_rects
            .iter()
            .map(|rect| rect.height)
            .fold(GRAPH_CARD_BASE_H, f32::max);
        let x_span = (max_x - min_x).max(1.0);
        let y_span = (max_y - min_y).max(1.0);
        let padding = 28.0;
        let available_x = (canvas.width - GRAPH_CARD_W - padding * 2.0).max(1.0);
        let available_y = (canvas.height - max_h - padding * 2.0).max(1.0);
        self.zoom = (available_x / x_span)
            .min(available_y / y_span)
            .min(1.25)
            .clamp(GRAPH_MIN_ZOOM, 1.25);

        let framed = graph_card_rects(
            Rect::new(0.0, 0.0, canvas.width, canvas.height),
            &cards,
            graph,
            [0.0, 0.0],
            self.zoom,
            self.local_input_position,
            self.local_output_position,
        );
        let left = framed
            .iter()
            .map(|rect| rect.x)
            .fold(f32::INFINITY, f32::min);
        let top = framed
            .iter()
            .map(|rect| rect.y)
            .fold(f32::INFINITY, f32::min);
        let right = framed
            .iter()
            .map(|rect| rect.right())
            .fold(f32::NEG_INFINITY, f32::max);
        let bottom = framed
            .iter()
            .map(|rect| rect.bottom())
            .fold(f32::NEG_INFINITY, f32::max);
        self.pan = [
            (canvas.width - (right - left)) * 0.5 - left,
            (canvas.height - (bottom - top)) * 0.5 - top,
        ];
        true
    }

    pub fn pointer_middle_pressed(
        &mut self,
        rect: Rect,
        point: [f32; 2],
        project: &Project,
        timeline: &TimelineState,
        plugins: &PluginRegistry,
    ) -> PipelineGraphAction {
        let graph = GraphModel::new(project, timeline, self.pinned_pipeline);
        if let Some((GraphControlTarget::Property(key), component)) =
            self.controls.numbers.target_at(rect, point)
        {
            if let Some(default) = graph_property_definition(key.target, &key.input, graph, plugins)
                .and_then(|definition| definition.ty.default_gpu(&definition.default).ok())
                .and_then(|value| graph_gpu_component(value, component))
            {
                let linked = self
                    .property_links
                    .contains(&GraphControlTarget::Property(key.clone()));
                return PipelineGraphAction::SetEffectComponent {
                    target: key.target,
                    input: key.input,
                    component,
                    value: default,
                    linked,
                };
            }
        }
        if let Some(key) = self.controls.angles.target_at(rect, point) {
            if let Some(GpuValue::F32(default)) =
                graph_property_definition(key.target, &key.input, graph, plugins)
                    .and_then(|definition| definition.ty.default_gpu(&definition.default).ok())
            {
                return PipelineGraphAction::SetEffectValue {
                    target: key.target,
                    input: key.input,
                    value: GpuValue::F32(default),
                };
            }
        }
        if !graph_canvas_rect(rect).contains(point) {
            return PipelineGraphAction::None;
        }
        self.pan_drag = Some((point, self.pan));
        PipelineGraphAction::None
    }

    pub fn scroll_popup(&self, rect: Rect, point: [f32; 2], delta: [f32; 2]) -> bool {
        let toolbar = graph_toolbar_layout(rect);
        let pipeline_count = self.last_pipeline_count.saturating_add(1);
        self.pipeline_combo
            .scroll(toolbar.combo, point, delta, pipeline_count)
            || self.controls.enums.scroll_popup(rect, point, delta)
    }

    pub fn scroll(
        &mut self,
        rect: Rect,
        point: [f32; 2],
        delta: [f32; 2],
        modifiers: ModifiersState,
    ) -> bool {
        if self.scroll_popup(rect, point, delta) {
            return true;
        }
        let canvas = graph_canvas_rect(rect);
        if !canvas.contains(point) {
            return false;
        }
        if let Some((target, max_scroll)) =
            self.host_eq_scroll_rects
                .iter()
                .find_map(|(target, (local, max))| {
                    let absolute = offset_rect(*local, rect.x, rect.y);
                    absolute.contains(point).then_some((*target, *max))
                })
        {
            if max_scroll > 0.0 && !(modifiers.control_key() || modifiers.super_key()) {
                let axis = if delta[0].abs() > delta[1].abs() {
                    delta[0]
                } else {
                    delta[1]
                };
                let scroll = self.host_eq_scroll.entry(target).or_insert(0.0);
                *scroll = (*scroll - axis).clamp(0.0, max_scroll);
                return true;
            }
        }
        if modifiers.control_key() || modifiers.super_key() {
            return self.zoom_at(rect, point, (delta[1] * 0.0025).exp());
        }
        self.pan[0] += delta[0];
        self.pan[1] += delta[1];
        true
    }

    pub fn pinch_zoom(&mut self, rect: Rect, point: [f32; 2], delta: f64) -> bool {
        if !delta.is_finite() || delta.abs() <= f64::EPSILON {
            return graph_canvas_rect(rect).contains(point);
        }
        self.zoom_at(rect, point, (delta as f32).exp())
    }

    fn zoom_at(&mut self, rect: Rect, point: [f32; 2], factor: f32) -> bool {
        self.set_zoom_at(rect, point, self.zoom * factor.clamp(0.5, 2.0))
    }

    fn set_zoom_at(&mut self, rect: Rect, point: [f32; 2], zoom: f32) -> bool {
        let canvas = graph_canvas_rect(rect);
        if !canvas.contains(point) {
            return false;
        }
        let before = graph_screen_to_world(canvas, self.pan, self.zoom, point);
        self.zoom = zoom.clamp(GRAPH_MIN_ZOOM, GRAPH_MAX_ZOOM);
        self.pan = [
            point[0] - canvas.x - before[0] * self.zoom,
            point[1] - canvas.y - before[1] * self.zoom,
        ];
        true
    }

    fn graph_color_action(
        &mut self,
        target: &GraphControlTarget,
        color: [f32; 4],
    ) -> PipelineGraphAction {
        match target {
            GraphControlTarget::Property(key) => PipelineGraphAction::SetEffectValue {
                target: key.target,
                input: key.input.clone(),
                value: GpuValue::Color(color),
            },
            GraphControlTarget::ValueNode(node) => PipelineGraphAction::SetValueNodeValue {
                node: *node,
                value: GpuValue::Color(color),
            },
            GraphControlTarget::HostGradientStop {
                target,
                input,
                index,
            } => {
                let color_target = GraphControlTarget::HostGradientStop {
                    target: *target,
                    input: input.clone(),
                    index: *index,
                };
                let values = self.host_gradient_colors.entry(color_target).or_default();
                set_gradient_color_values(values, *index, color);
                PipelineGraphAction::SetHostValue {
                    target: *target,
                    input: input.clone(),
                    value: crate::project::HostValue::F32List(values.clone()),
                }
            }
        }
    }

    fn apply_color_direct(
        &mut self,
        target: &GraphControlTarget,
        color: [f32; 4],
        project: &mut Project,
        timeline: &mut TimelineState,
    ) {
        match target {
            GraphControlTarget::Property(key) => {
                self.apply_property_value_direct(key, GpuValue::Color(color), project, timeline);
            }
            GraphControlTarget::ValueNode(node) => {
                if let Some(pipeline) = self.target_pipeline(project, timeline) {
                    project.set_value_node_value(pipeline, *node, GpuValue::Color(color));
                }
            }
            GraphControlTarget::HostGradientStop {
                target: node_target,
                input,
                index,
            } => {
                let values = self.host_gradient_colors.entry(target.clone()).or_default();
                set_gradient_color_values(values, *index, color);
                let value = crate::project::HostValue::F32List(values.clone());
                match *node_target {
                    GraphNodeTarget::Local(node) => {
                        timeline.set_selected_local_node_host_value(node, input, value);
                    }
                    GraphNodeTarget::Shared(node) => {
                        if let Some(pipeline) = self.pinned_pipeline {
                            project.set_pipeline_node_host_value(
                                pipeline,
                                node,
                                input,
                                timeline.selected_keyframe_time(),
                                value,
                            );
                        } else {
                            timeline.set_pipeline_host_input_value(project, node, input, value);
                        }
                    }
                    GraphNodeTarget::Input => {
                        timeline.set_generator_host_value(input, value);
                    }
                    GraphNodeTarget::Value(_) | GraphNodeTarget::Output => {}
                }
            }
        }
    }

    fn apply_property_value_direct(
        &self,
        key: &GraphPropertyKey,
        value: GpuValue,
        project: &mut Project,
        timeline: &mut TimelineState,
    ) {
        match key.target {
            GraphNodeTarget::Local(node) => {
                timeline.set_selected_local_node_value(node, &key.input, value);
            }
            GraphNodeTarget::Shared(node) => {
                if let Some(pipeline) = self.pinned_pipeline {
                    project.set_pipeline_node_value(pipeline, node, &key.input, value);
                } else {
                    timeline.set_pipeline_input_value(project, node, &key.input, value);
                }
                timeline.reconcile_pipeline_overrides(project);
            }
            GraphNodeTarget::Value(node) => {
                if let Some(pipeline) = self.target_pipeline(project, timeline) {
                    project.set_value_node_input_value(pipeline, node, &key.input, value);
                }
            }
            GraphNodeTarget::Input => timeline.set_generator_value(&key.input, value),
            GraphNodeTarget::Output => {}
        }
    }

    fn apply_component_direct(
        &self,
        target: &GraphControlTarget,
        component: usize,
        value: f32,
        project: &mut Project,
        timeline: &mut TimelineState,
    ) {
        let linked = self.property_links.contains(target);
        match target {
            GraphControlTarget::ValueNode(node) => {
                if let Some(pipeline) = self.pipeline_id {
                    project.set_value_node_component(pipeline, *node, component, value, linked);
                }
            }
            GraphControlTarget::Property(key) => match key.target {
                GraphNodeTarget::Local(node) => {
                    timeline.set_selected_local_node_component(
                        node, &key.input, component, value, linked,
                    );
                }
                GraphNodeTarget::Shared(node) => {
                    if let Some(pipeline) = self.pinned_pipeline {
                        project.set_pipeline_node_component(
                            pipeline, node, &key.input, component, value, linked,
                        );
                    } else {
                        timeline.set_pipeline_input_component(
                            project, node, &key.input, component, value, linked,
                        );
                    }
                    timeline.reconcile_pipeline_overrides(project);
                }
                GraphNodeTarget::Input => {
                    if let Some(current) = timeline.generator_value(&key.input) {
                        if let Some(next) = current.with_component(component, value, linked) {
                            timeline.set_generator_value(&key.input, next);
                        }
                    }
                }
                GraphNodeTarget::Value(_) | GraphNodeTarget::Output => {}
            },
            GraphControlTarget::HostGradientStop { .. } => {}
        }
    }

    fn edit_number_inputs(
        &mut self,
        mut edit: impl FnMut(&mut NumberInput) -> Option<f64>,
        project: &mut Project,
        timeline: &mut TimelineState,
    ) -> bool {
        if let Some((key, value)) = self.controls.angles.edit(&mut edit) {
            if let Some(value) = value {
                self.apply_property_value_direct(&key, GpuValue::F32(value), project, timeline);
            }
            return true;
        }
        if let Some(key) = self.controls.numbers.editing_target() {
            if let Some(value) = self.controls.numbers.edit(&key, edit) {
                self.apply_component_direct(&key.0, key.1, value as f32, project, timeline);
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
        if self.controls.color_target.is_some() {
            let before = self.controls.color_picker.linear();
            if self.controls.color_picker.handle_key(event, modifiers) {
                let after = self.controls.color_picker.linear();
                if after != before {
                    if let Some(target) = self.controls.color_target.clone() {
                        self.apply_color_direct(&target, after, project, timeline);
                    }
                }
                return true;
            }
        }
        if self.edit_number_inputs(
            |input| input.handle_key(event, modifiers),
            project,
            timeline,
        ) {
            return true;
        }
        if self.renaming {
            let response = self.pipeline_name.handle_key(event, modifiers);
            if response.handled {
                if response.changed {
                    if let Some(id) = self.pipeline_id {
                        project.rename_pipeline(id, self.pipeline_name.text());
                    }
                }
                return true;
            }
            return false;
        }
        if matches!(
            event.logical_key,
            Key::Named(NamedKey::Delete | NamedKey::Backspace)
        ) {
            let action = self.delete_selection_action(true);
            if !matches!(action, PipelineGraphAction::None) {
                self.pending_action = Some(action);
                return true;
            }
        }
        false
    }

    pub fn take_action(&mut self) -> Option<PipelineGraphAction> {
        self.pending_action.take()
    }

    pub fn handle_ime(
        &mut self,
        event: &Ime,
        project: &mut Project,
        timeline: &mut TimelineState,
    ) -> bool {
        if self.controls.color_target.is_some() {
            let before = self.controls.color_picker.linear();
            if self.controls.color_picker.handle_ime(event) {
                let after = self.controls.color_picker.linear();
                if after != before {
                    if let Some(target) = self.controls.color_target.clone() {
                        self.apply_color_direct(&target, after, project, timeline);
                    }
                }
                return true;
            }
        }
        if self.edit_number_inputs(|input| input.handle_ime(event), project, timeline) {
            return true;
        }
        if !self.renaming {
            return false;
        }
        let response = self.pipeline_name.handle_ime(event);
        if response.handled {
            if response.changed {
                if let Some(id) = self.pipeline_id {
                    project.rename_pipeline(id, self.pipeline_name.text());
                }
            }
            return true;
        }
        false
    }

    pub fn ime_area(&self, rect: Rect) -> Option<Rect> {
        if let Some(swatch) = self.controls.color_rect {
            if let Some(caret) = self
                .controls
                .color_picker
                .caret_rect_in(swatch, self.controls.popup_bounds)
            {
                return Some(offset_rect(caret, rect.x, rect.y));
            }
        }
        if self.renaming && self.pipeline_id.is_some() && self.pipeline_name.is_focused() {
            return Some(
                self.pipeline_name
                    .caret_rect(graph_toolbar_layout(rect).combo),
            );
        }
        self.controls.caret_rect(rect)
    }
}
