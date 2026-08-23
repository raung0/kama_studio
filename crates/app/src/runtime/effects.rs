use std::{
    collections::{hash_map::DefaultHasher, HashMap},
    hash::{Hash, Hasher},
    sync::Arc,
};

#[cfg(test)]
use std::collections::BTreeMap;

pub use kama_editor_core::effects::*;

#[derive(Clone, Debug)]
pub struct CompiledFragment {
    pub key: u64,
    pub execution: NodeExecution,
    pub node_types: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct CompiledStage {
    pub fragment: Arc<CompiledFragment>,

    pub node_ids: Vec<NodeId>,
}

#[derive(Clone, Debug)]
pub struct CompiledPipeline {
    pub revision: u64,
    pub stages: Vec<CompiledStage>,
}

#[derive(Default)]
pub struct EffectRuntime {
    compiled: HashMap<PipelineId, Arc<CompiledPipeline>>,
    fragments: HashMap<u64, Arc<CompiledFragment>>,
}

impl EffectRuntime {
    pub fn rebuild(&mut self, pipelines: &[EffectPipeline]) {
        let mut live = HashMap::new();
        for pipeline in pipelines {
            if pipeline.kind != PipelineKind::Video {
                continue;
            }
            if let Some(existing) = self.compiled.get(&pipeline.id) {
                if existing.revision == pipeline.revision {
                    live.insert(pipeline.id, existing.clone());
                    continue;
                }
            }
            let compiled = Arc::new(self.compile_pipeline(pipeline));
            live.insert(pipeline.id, compiled);
        }
        self.compiled = live;
    }

    pub fn compiled(&self, pipeline: PipelineId) -> Option<&Arc<CompiledPipeline>> {
        self.compiled.get(&pipeline)
    }

    pub fn compiled_pipelines(&self) -> impl Iterator<Item = &CompiledPipeline> {
        self.compiled.values().map(Arc::as_ref)
    }

    #[cfg(test)]
    pub fn fragment_count(&self) -> usize {
        self.fragments.len()
    }

    fn compile_pipeline(&mut self, pipeline: &EffectPipeline) -> CompiledPipeline {
        let mut stages = Vec::new();
        let mut run = Vec::<&EffectNode>::new();
        for node in pipeline.main_path() {
            if let Some(first) = run.first() {
                if first.execution != node.execution
                    || node.execution != NodeExecution::PointwiseGpu
                {
                    stages.push(self.compile_stage(&run));
                    run.clear();
                }
            }
            run.push(node);
            if node.execution != NodeExecution::PointwiseGpu {
                stages.push(self.compile_stage(&run));
                run.clear();
            }
        }
        if !run.is_empty() {
            stages.push(self.compile_stage(&run));
        }
        CompiledPipeline {
            revision: pipeline.revision,
            stages,
        }
    }

    fn compile_stage(&mut self, nodes: &[&EffectNode]) -> CompiledStage {
        CompiledStage {
            fragment: self.intern_fragment(nodes),
            node_ids: nodes.iter().map(|node| node.id).collect(),
        }
    }

    fn intern_fragment(&mut self, nodes: &[&EffectNode]) -> Arc<CompiledFragment> {
        let key = fragment_key(nodes);
        if let Some(fragment) = self.fragments.get(&key) {
            return fragment.clone();
        }
        let fragment = Arc::new(CompiledFragment {
            key,
            execution: nodes
                .first()
                .map_or(NodeExecution::PointwiseGpu, |node| node.execution),
            node_types: nodes.iter().map(|node| node.node_type.clone()).collect(),
        });
        self.fragments.insert(key, fragment.clone());
        fragment
    }
}

fn fragment_key(nodes: &[&EffectNode]) -> u64 {
    let mut hasher = DefaultHasher::new();
    let local_ids: HashMap<NodeId, usize> = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id, index))
        .collect();
    for node in nodes {
        node.node_type.hash(&mut hasher);
        node.execution.hash(&mut hasher);

        for (name, image) in &node.image_inputs {
            name.hash(&mut hasher);
            match image {
                ImageBinding::Disconnected => 0u8.hash(&mut hasher),
                ImageBinding::PipelineInput => 1u8.hash(&mut hasher),
                ImageBinding::Node(socket) => {
                    2u8.hash(&mut hasher);
                    socket.output.hash(&mut hasher);
                    match local_ids.get(&socket.node) {
                        Some(index) => index.hash(&mut hasher),
                        None => usize::MAX.hash(&mut hasher),
                    }
                }
            }
        }
        for (name, binding) in &node.inputs {
            name.hash(&mut hasher);
            if let Binding::Connection(socket) = binding {
                socket.output.hash(&mut hasher);
                match local_ids.get(&socket.node) {
                    Some(index) => {
                        0u8.hash(&mut hasher);
                        index.hash(&mut hasher);
                    }
                    None => {
                        1u8.hash(&mut hasher);
                    }
                }
            }
        }
    }
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporal_easing_uses_the_selected_keyframe_side() {
        let leaving = interpolation_amount(Interpolation::EaseOut, Interpolation::Linear, 0.25);
        let arriving = interpolation_amount(Interpolation::Linear, Interpolation::EaseIn, 0.25);
        assert!(
            leaving < 0.25,
            "Ease Out should soften departure from the left key"
        );
        assert!(
            arriving > 0.25,
            "Ease In should soften arrival into the right key"
        );
        assert_eq!(
            interpolation_amount(Interpolation::EaseIn, Interpolation::Linear, 0.25),
            0.25,
            "Ease In should not alter the selected key's outgoing side",
        );
    }

    #[test]
    fn scalar_track_ease_in_affects_the_segment_arriving_at_the_key() {
        let track = ScalarKeyframeTrack {
            keys: vec![
                ScalarKeyframe {
                    time: 0.0,
                    value: 0.0,
                    interpolation: Interpolation::Linear,
                    ease_in: EasingHandle::LINEAR,
                    ease_out: EasingHandle::LINEAR,
                    custom_ease_in: false,
                    custom_ease_out: false,
                },
                ScalarKeyframe {
                    time: 1.0,
                    value: 1.0,
                    interpolation: Interpolation::EaseIn,
                    ease_in: EasingHandle::LINEAR,
                    ease_out: EasingHandle::LINEAR,
                    custom_ease_in: false,
                    custom_ease_out: false,
                },
            ],
        };
        assert!(track.evaluate(0.75).unwrap() < 1.0);
        assert!(track.evaluate(0.75).unwrap() > 0.75);
    }

    fn chain_pipeline(id: PipelineId, first: NodeId, second: NodeId) -> EffectPipeline {
        let mut exposure = EffectNode::builtin(first, BuiltinNodePreset::Exposure);
        exposure
            .image_inputs
            .insert("image".into(), ImageBinding::PipelineInput);
        let mut contrast = EffectNode::builtin(second, BuiltinNodePreset::Contrast);
        contrast.image_inputs.insert(
            "image".into(),
            ImageBinding::Node(SocketRef {
                node: first,
                output: "image".into(),
            }),
        );
        EffectPipeline {
            id,
            name: format!("P{id}"),
            revision: 1,
            kind: PipelineKind::Video,
            nodes: vec![exposure, contrast],
            value_nodes: Vec::new(),
            output: ImageBinding::Node(SocketRef {
                node: second,
                output: "image".into(),
            }),
            ui_input_position: None,
            ui_output_position: None,
        }
    }

    #[test]
    fn bools_and_enums_are_always_stepped() {
        let mut bools = KeyframeTrack::default();
        bools.set_key(0.0, GpuValue::Bool(false), Interpolation::Linear);
        bools.set_key(1.0, GpuValue::Bool(true), Interpolation::Linear);
        assert_eq!(bools.evaluate(0.5), Some(GpuValue::Bool(false)));

        let mut enums = KeyframeTrack::default();
        enums.set_key(0.0, GpuValue::Enum(2), Interpolation::Cubic);
        enums.set_key(1.0, GpuValue::Enum(7), Interpolation::Cubic);
        assert_eq!(enums.evaluate(0.5), Some(GpuValue::Enum(2)));
    }

    #[test]
    fn inspector_path_is_driven_by_output_reachability() {
        let mut pipeline = chain_pipeline(1, 10, 11);
        pipeline
            .nodes
            .push(EffectNode::builtin(99, BuiltinNodePreset::Invert));
        let path: Vec<_> = pipeline
            .main_path()
            .into_iter()
            .map(|node| node.id)
            .collect();
        assert_eq!(path, vec![10, 11]);
    }

    #[test]
    fn structurally_equal_pipelines_share_compiled_fragments() {
        let a = chain_pipeline(1, 10, 11);
        let b = chain_pipeline(2, 200, 201);
        let mut runtime = EffectRuntime::default();
        runtime.rebuild(&[a, b]);
        assert_eq!(runtime.fragment_count(), 1);
        let a_fragment = &runtime.compiled(1).unwrap().stages[0].fragment;
        let b_fragment = &runtime.compiled(2).unwrap().stages[0].fragment;
        assert!(Arc::ptr_eq(a_fragment, b_fragment));
    }

    #[test]
    fn dynamic_image_inputs_resize_without_losing_surviving_connections() {
        let mut node = EffectNode {
            id: 7,
            node_type: "test.compose".into(),
            execution: NodeExecution::SpatialGpu,
            ui_position: None,
            image_inputs: BTreeMap::from([
                ("image_1".into(), ImageBinding::PipelineInput),
                (
                    "image_2".into(),
                    ImageBinding::Node(SocketRef {
                        node: 3,
                        output: "image".into(),
                    }),
                ),
            ]),
            stack_input: Some("image_1".into()),
            inputs: BTreeMap::from([("count".into(), Binding::Constant(GpuValue::U32(2)))]),
            host_inputs: BTreeMap::new(),
            dynamic_image_inputs: Some(DynamicImageInputs {
                count_input: "count".into(),
                prefix: "image_".into(),
                min: 1,
                max: 64,
            }),
        };
        node.inputs
            .get_mut("count")
            .unwrap()
            .set_value(0.0, GpuValue::U32(4));
        assert!(node.sync_dynamic_image_inputs());
        assert_eq!(
            node.image_input_names(),
            vec!["image_1", "image_2", "image_3", "image_4"]
        );
        assert!(matches!(
            node.image_inputs["image_1"],
            ImageBinding::PipelineInput
        ));
        assert!(matches!(
            node.image_inputs["image_2"],
            ImageBinding::Node(_)
        ));
        assert!(matches!(
            node.image_inputs["image_3"],
            ImageBinding::Disconnected
        ));

        node.inputs
            .get_mut("count")
            .unwrap()
            .set_value(0.0, GpuValue::U32(1));
        assert!(node.sync_dynamic_image_inputs());
        assert_eq!(node.image_input_names(), vec!["image_1"]);
        assert_eq!(node.image_inputs.len(), 1);
    }

    #[test]
    fn parameter_changes_do_not_recompile_pipeline_topology() {
        let mut pipeline = chain_pipeline(1, 10, 11);
        let mut runtime = EffectRuntime::default();
        runtime.rebuild(std::slice::from_ref(&pipeline));
        let compiled = runtime.compiled(1).unwrap().clone();
        pipeline.nodes[0]
            .inputs
            .insert("exposure".into(), Binding::Constant(GpuValue::F32(2.0)));
        runtime.rebuild(std::slice::from_ref(&pipeline));
        assert!(Arc::ptr_eq(&compiled, runtime.compiled(1).unwrap()));
    }

    #[test]
    fn value_graph_chains_runtime_sources_and_math() {
        let nodes = vec![
            ValueNode {
                id: 1,
                kind: ValueNodeKind::Timestamp,
                value: GpuValue::F32(0.0),
                inputs: BTreeMap::new(),
                ui_position: None,
            },
            ValueNode {
                id: 2,
                kind: ValueNodeKind::Multiply,
                value: GpuValue::F32(0.0),
                inputs: BTreeMap::from([
                    (
                        "A".into(),
                        Binding::Connection(SocketRef {
                            node: 1,
                            output: "value".into(),
                        }),
                    ),
                    ("B".into(), Binding::Constant(GpuValue::F32(2.0))),
                ]),
                ui_position: None,
            },
            ValueNode {
                id: 3,
                kind: ValueNodeKind::Add,
                value: GpuValue::F32(0.0),
                inputs: BTreeMap::from([
                    (
                        "A".into(),
                        Binding::Connection(SocketRef {
                            node: 2,
                            output: "value".into(),
                        }),
                    ),
                    ("B".into(), Binding::Constant(GpuValue::Vec2([1.0, 3.0]))),
                ]),
                ui_position: None,
            },
        ];
        let value = evaluate_value_node(
            &nodes,
            3,
            ValueEvalContext {
                timeline_time: 2.0,
                local_time: 0.25,
                frame_index: 48,
                frame_rate: 24.0,
            },
        );
        assert_eq!(value, Some(GpuValue::Vec2([5.0, 7.0])));
    }

    #[test]
    fn value_node_keyframes_use_composition_time() {
        let mut track = KeyframeTrack::default();
        track.set_key(10.0, GpuValue::F32(2.0), Interpolation::Linear);
        track.set_key(11.0, GpuValue::F32(4.0), Interpolation::Linear);
        let nodes = [ValueNode {
            id: 1,
            kind: ValueNodeKind::Negate,
            value: GpuValue::F32(0.0),
            inputs: BTreeMap::from([("Value".into(), Binding::Keyframes(track))]),
            ui_position: None,
        }];
        let Some(GpuValue::F32(value)) = evaluate_value_node(
            &nodes,
            1,
            ValueEvalContext {
                timeline_time: 10.5,
                local_time: 0.5,
                frame_index: 252,
                frame_rate: 24.0,
            },
        ) else {
            panic!("expected scalar value");
        };
        assert!((value + 3.0).abs() < 0.001);
    }

    #[test]
    fn local_frame_uses_effect_frame_rate() {
        let nodes = [ValueNode {
            id: 1,
            kind: ValueNodeKind::LocalFrame,
            value: GpuValue::F32(0.0),
            inputs: BTreeMap::new(),
            ui_position: None,
        }];
        assert_eq!(
            evaluate_value_node(
                &nodes,
                1,
                ValueEvalContext {
                    timeline_time: 10.0,
                    local_time: 1.5,
                    frame_index: 300,
                    frame_rate: 24.0,
                },
            ),
            Some(GpuValue::F32(36.0))
        );
    }
}
