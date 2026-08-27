use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[cfg(test)]
use crate::effects::BuiltinNodePreset;

use crate::{
    effects::{
        Binding, EffectNode, EffectPipeline, GpuValue, ImageBinding, ImageGraphIndex, NodeId,
        PipelineId, PipelineInstance, PipelineKind, SocketRef, ValueEvalContext, ValueGraphIndex,
        ValueNode, ValueNodeKind, evaluate_value_node,
    },
    file_io::atomic_write_json,
    plugin::{AudioEffectDefinition, EffectDefinition, GeneratorDefinition, PluginRegistry},
    runtime::media::probe_av_media,
    timeline::TimelineDocument,
};

pub const KAMA_FORMAT_VERSION: u32 = 7;
const MIN_MIGRATABLE_FORMAT_VERSION: u32 = 5;
pub use kama_editor_core::document::{
    AlphaBlendMode, BlendMode, CompositionId, CompositionSettings, GeneratorSource, LayerComposite,
    MAX_CANVAS_DIMENSION, MAX_FRAME_RATE, MediaAsset, MediaId, MediaKind, MediaTrackInfo,
    MediaTrackKind, Model3dShading, ProjectBackground, VisualSource,
};
pub use kama_editor_core::parameters::{HostBinding, HostValue};
#[cfg(test)]
use kama_editor_core::parameters::{HostKeyframe, HostKeyframeTrack};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Composition {
    pub id: CompositionId,
    pub name: String,
    pub settings: CompositionSettings,
    pub timeline: TimelineDocument,
}

impl Composition {
    pub fn new(id: CompositionId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            settings: CompositionSettings::default(),
            timeline: TimelineDocument::composition_default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project {
    pub format_version: u32,
    pub name: String,
    pub media: Vec<MediaAsset>,
    pub pipelines: Vec<EffectPipeline>,
    pub compositions: Vec<Composition>,
    pub next_media_id: MediaId,
    pub next_pipeline_id: PipelineId,
    pub next_node_id: NodeId,
    pub next_composition_id: CompositionId,

    #[serde(skip)]
    pub active_composition: CompositionId,
}

#[derive(Clone, Debug)]
pub(crate) struct PipelineSelectorRemap {
    pub(crate) owner: PipelineId,
    pub(crate) nodes: Vec<NodeId>,
    pub(crate) old_options: Vec<PipelineId>,
    pub(crate) new_options: Vec<PipelineId>,
}

pub(crate) fn remap_pipeline_selector_binding(
    binding: &mut Binding,
    old_options: &[PipelineId],
    new_options: &[PipelineId],
) {
    let remap = |value: &mut GpuValue| {
        let GpuValue::Enum(index) = *value else {
            return;
        };
        if index == 0 {
            return;
        }
        let mapped = old_options
            .get(index.saturating_sub(1) as usize)
            .and_then(|target| new_options.iter().position(|candidate| candidate == target))
            .and_then(|index| u32::try_from(index).ok())
            .and_then(|index| index.checked_add(1))
            .unwrap_or(0);
        *value = GpuValue::Enum(mapped);
    };
    match binding {
        Binding::Constant(value) => remap(value),
        Binding::Keyframes(track) => {
            for key in &mut track.keys {
                remap(&mut key.value);
            }
        }
        Binding::Components(channels) => remap(&mut channels.base),
        Binding::Connection(_) => {}
    }
}

impl Project {
    pub fn new() -> Self {
        Self {
            format_version: KAMA_FORMAT_VERSION,
            name: "Untitled".into(),
            media: Vec::new(),
            pipelines: Vec::new(),
            compositions: vec![Composition::new(1, "Main")],
            next_media_id: 1,
            next_pipeline_id: 1,
            next_node_id: 1,
            next_composition_id: 2,
            active_composition: 1,
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let data = fs::read(path).with_context(|| format!("read {}", path.display()))?;
        let mut project: Self = serde_json::from_slice(&data)
            .with_context(|| format!("parse .kama v{KAMA_FORMAT_VERSION} {}", path.display()))?;
        let loaded_format_version = project.format_version;
        if !(MIN_MIGRATABLE_FORMAT_VERSION..=KAMA_FORMAT_VERSION).contains(&project.format_version)
        {
            anyhow::bail!(
                "unsupported .kama format version {}; expected {}..={}",
                project.format_version,
                MIN_MIGRATABLE_FORMAT_VERSION,
                KAMA_FORMAT_VERSION
            );
        }

        anyhow::ensure!(
            !project.compositions.is_empty(),
            ".kama project has no compositions"
        );
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        for asset in &mut project.media {
            migrate_and_resolve_media_path(asset, base, loaded_format_version);
            if asset.kind == MediaKind::Unknown && crate::model3d::is_supported_path(&asset.path) {
                asset.kind = MediaKind::Model3d;
            }
            if asset.tracks.is_empty() && matches!(asset.kind, MediaKind::Video | MediaKind::Audio)
            {
                if let Ok(probe) = probe_av_media(&asset.path) {
                    asset.has_audio = probe.has_audio;
                    asset.tracks = probe.tracks;
                }
            }
        }
        let legacy_model_settings = project
            .media
            .iter_mut()
            .filter_map(|asset| {
                let settings = asset.legacy_model.take()?;
                let intrinsic_size =
                    crate::model3d::probe_size(&asset.path).unwrap_or(settings.size);
                Some((asset.id, (settings, intrinsic_size)))
            })
            .collect::<std::collections::HashMap<_, _>>();
        for composition in &mut project.compositions {
            for clip in &mut composition.timeline.clips {
                let VisualSource::Media(media) = &clip.source else {
                    continue;
                };
                let Some((settings, intrinsic_size)) = legacy_model_settings.get(media) else {
                    continue;
                };

                let size: [f32; 3] = std::array::from_fn(|axis| {
                    let intrinsic = intrinsic_size[axis];
                    if intrinsic.abs() > 1.0e-6 {
                        settings.size[axis] / intrinsic * 2.0
                    } else {
                        2.0
                    }
                });
                clip.model3d.apply_legacy(
                    size,
                    settings.scale,
                    settings.rotation,
                    settings.shading,
                );
            }
        }
        for composition in &mut project.compositions {
            composition
                .settings
                .validate()
                .with_context(|| format!("invalid composition {}", composition.name))?;
            composition.timeline.resolve_relative_paths(base);
        }

        for composition in &mut project.compositions {
            composition.timeline.repair_id_counters();
        }
        project.repair_pipeline_references();
        project.validate_structure()?;
        project.format_version = KAMA_FORMAT_VERSION;
        project.repair_id_counters();
        project.active_composition = project
            .compositions
            .iter()
            .find(|composition| composition.name == "Main")
            .unwrap_or(&project.compositions[0])
            .id;
        Ok(project)
    }

    fn repair_pipeline_references(&mut self) {
        let video = self
            .pipelines
            .iter()
            .filter(|pipeline| pipeline.kind == PipelineKind::Video)
            .map(|pipeline| pipeline.id)
            .collect::<HashSet<_>>();
        let audio = self
            .pipelines
            .iter()
            .filter(|pipeline| pipeline.kind == PipelineKind::Audio)
            .map(|pipeline| pipeline.id)
            .collect::<HashSet<_>>();
        for composition in &mut self.compositions {
            for track in &mut composition.timeline.tracks {
                if let Some(instance) = &mut track.pipeline {
                    repair_pipeline_reference(
                        instance,
                        if track.kind == crate::timeline::TrackKind::Audio {
                            &audio
                        } else {
                            &video
                        },
                    );
                }
            }
            let track_kinds = composition
                .timeline
                .tracks
                .iter()
                .map(|track| (track.id, track.kind))
                .collect::<BTreeMap<_, _>>();
            for clip in &mut composition.timeline.clips {
                let Some(kind) = track_kinds.get(&clip.track) else {
                    continue;
                };
                repair_pipeline_reference(
                    &mut clip.pipeline,
                    if *kind == crate::timeline::TrackKind::Audio {
                        &audio
                    } else {
                        &video
                    },
                );
            }
            for track in &mut composition.timeline.tracks {
                for row in &mut track.property_rows {
                    repair_pipeline_reference(
                        &mut row.pipeline,
                        if track.kind == crate::timeline::TrackKind::Audio {
                            &audio
                        } else {
                            &video
                        },
                    );
                }
            }
        }
    }

    fn validate_structure(&self) -> Result<()> {
        validate_ids("media", self.media.iter().map(|asset| asset.id), u64::MAX)?;
        validate_ids(
            "pipeline",
            self.pipelines.iter().map(|pipeline| pipeline.id),
            u64::MAX,
        )?;
        validate_ids(
            "composition",
            self.compositions.iter().map(|composition| composition.id),
            u64::MAX,
        )?;
        let media = self
            .media
            .iter()
            .map(|asset| asset.id)
            .collect::<HashSet<_>>();
        for asset in &self.media {
            anyhow::ensure!(
                asset
                    .duration
                    .is_none_or(|value| value.is_finite() && value >= 0.0)
                    && asset.frame_rate.is_none_or(|value| {
                        value.is_finite() && value > 0.0 && value <= MAX_FRAME_RATE
                    })
                    && asset.video_width.is_none_or(|value| value > 0)
                    && asset.video_height.is_none_or(|value| value > 0),
                "media {} has invalid stream metadata",
                asset.name
            );
        }
        let compositions = self
            .compositions
            .iter()
            .map(|composition| composition.id)
            .collect::<HashSet<_>>();
        for pipeline in &self.pipelines {
            let label = format!("pipeline {}", pipeline.name);
            validate_ids(
                &format!("node in {}", label),
                pipeline
                    .nodes
                    .iter()
                    .map(|node| node.id)
                    .chain(pipeline.value_nodes.iter().map(|node| node.id)),
                u64::MAX,
            )?;
            validate_image_graph(&pipeline.nodes, &pipeline.output, &label)?;
            let value_ids = pipeline
                .value_nodes
                .iter()
                .map(|node| node.id)
                .collect::<HashSet<_>>();
            for (node_id, inputs) in pipeline
                .nodes
                .iter()
                .map(|node| (node.id, &node.inputs))
                .chain(
                    pipeline
                        .value_nodes
                        .iter()
                        .map(|node| (node.id, &node.inputs)),
                )
            {
                for binding in inputs.values() {
                    if let Binding::Connection(socket) = binding {
                        anyhow::ensure!(
                            value_ids.contains(&socket.node),
                            "node {node_id} in {label} references missing value node {}",
                            socket.node
                        );
                    }
                }
            }
            for node in &pipeline.value_nodes {
                anyhow::ensure!(
                    !ValueGraphIndex::new(&pipeline.value_nodes).depends_on(node.id, node.id),
                    "{label} contains a value graph cycle through node {}",
                    node.id
                );
            }
        }
        for composition in &self.compositions {
            let timeline = &composition.timeline;
            validate_ids(
                &format!("track in composition {}", composition.name),
                timeline.tracks.iter().map(|track| u64::from(track.id)),
                u64::from(u32::MAX),
            )?;
            validate_ids(
                &format!("clip in composition {}", composition.name),
                timeline.clips.iter().map(|clip| u64::from(clip.id)),
                u64::from(u32::MAX),
            )?;
            anyhow::ensure!(
                timeline
                    .clips
                    .iter()
                    .filter_map(|clip| clip.group)
                    .all(|group| group != u32::MAX),
                "composition {} has exhausted its group id space",
                composition.name
            );
            anyhow::ensure!(
                timeline
                    .end_time
                    .is_none_or(|end| end.is_finite() && end >= 0.0),
                "composition {} has an invalid timeline end",
                composition.name
            );
            for track in &timeline.tracks {
                anyhow::ensure!(
                    track.height.is_finite() && track.height > 0.0,
                    "track {} has an invalid height",
                    track.name
                );
                if let Some(instance) = &track.pipeline {
                    self.validate_pipeline_instance(
                        instance,
                        track.kind == crate::timeline::TrackKind::Audio,
                        &format!("track {}", track.name),
                    )?;
                }
            }
            for clip in &timeline.clips {
                let track = timeline
                    .tracks
                    .iter()
                    .find(|track| track.id == clip.track)
                    .with_context(|| {
                        format!(
                            "clip {} in composition {} references missing track {}",
                            clip.name, composition.name, clip.track
                        )
                    })?;
                anyhow::ensure!(
                    [
                        clip.start,
                        clip.duration,
                        clip.speed,
                        clip.source_offset,
                        clip.fade_in,
                        clip.fade_out
                    ]
                    .into_iter()
                    .all(f32::is_finite)
                        && clip.start >= 0.0
                        && clip.duration > 0.0
                        && clip.speed > 0.0
                        && clip.fade_in >= 0.0
                        && clip.fade_out >= 0.0,
                    "clip {} has invalid timing",
                    clip.name
                );
                match &clip.source {
                    VisualSource::Media(id) | VisualSource::Audio(id) => anyhow::ensure!(
                        media.contains(id),
                        "clip {} references missing media {id}",
                        clip.name
                    ),
                    VisualSource::Composition(id) => {
                        anyhow::ensure!(
                            compositions.contains(id),
                            "clip {} references missing composition {id}",
                            clip.name
                        );
                        let mut visited = HashSet::new();
                        anyhow::ensure!(
                            *id != composition.id
                                && !self.composition_reaches(*id, composition.id, &mut visited),
                            "composition {} contains a recursive composition reference",
                            composition.name
                        );
                    }
                    _ => {}
                }
                anyhow::ensure!(
                    crate::timeline::source_requires_instance(&clip.source)
                        == (clip.source_instance != 0),
                    "clip {} has invalid source identity",
                    clip.name
                );
                let source_matches_track = match track.kind {
                    crate::timeline::TrackKind::Audio => {
                        clip.source.is_audio()
                            || matches!(&clip.source, VisualSource::Composition(_))
                    }
                    crate::timeline::TrackKind::Video => clip.source.is_renderable_visual(),
                    crate::timeline::TrackKind::Effect => clip.source.is_effect_input(),
                };
                anyhow::ensure!(
                    source_matches_track,
                    "clip {} has a source incompatible with track {}",
                    clip.name,
                    track.name
                );
                anyhow::ensure!(
                    track.property_rows.iter().any(|row| row.matches(clip)),
                    "clip {} has no layer-owned source row",
                    clip.name
                );
                anyhow::ensure!(
                    !clip.has_owned_keyframes(),
                    "clip {} still contains clip-owned keyframes after migration",
                    clip.name
                );
                self.validate_pipeline_instance(
                    &clip.pipeline,
                    track.kind == crate::timeline::TrackKind::Audio,
                    &format!("clip {}", clip.name),
                )?;
            }
            for track in &timeline.tracks {
                for (row_index, row) in track.property_rows.iter().enumerate() {
                    match &row.source {
                        VisualSource::Media(id) | VisualSource::Audio(id) => anyhow::ensure!(
                            media.contains(id),
                            "layer property row references missing media {id}"
                        ),
                        VisualSource::Composition(id) => anyhow::ensure!(
                            compositions.contains(id),
                            "layer property row references missing composition {id}"
                        ),
                        _ => {}
                    }
                    anyhow::ensure!(
                        crate::timeline::source_requires_instance(&row.source)
                            == (row.source_instance != 0),
                        "layer {} has invalid source-row identity",
                        track.name
                    );
                    anyhow::ensure!(
                        track.property_rows[..row_index].iter().all(|other| {
                            !other.matches_source(&row.source, row.source_instance)
                        }),
                        "layer {} contains duplicate rows for one source",
                        track.name
                    );
                    anyhow::ensure!(
                        timeline
                            .clips
                            .iter()
                            .any(|clip| clip.track == track.id && row.matches(clip)),
                        "layer {} contains an orphan source row",
                        track.name
                    );
                    let source_matches_track = match track.kind {
                        crate::timeline::TrackKind::Audio => {
                            row.source.is_audio()
                                || matches!(&row.source, VisualSource::Composition(_))
                        }
                        crate::timeline::TrackKind::Video => row.source.is_renderable_visual(),
                        crate::timeline::TrackKind::Effect => row.source.is_effect_input(),
                    };
                    anyhow::ensure!(
                        source_matches_track,
                        "layer property row has a source incompatible with track {}",
                        track.name
                    );
                    self.validate_pipeline_instance(
                        &row.pipeline,
                        track.kind == crate::timeline::TrackKind::Audio,
                        &format!("layer {} source row", track.name),
                    )?;
                }
            }
        }
        Ok(())
    }

    fn validate_pipeline_instance(
        &self,
        instance: &PipelineInstance,
        audio: bool,
        label: &str,
    ) -> Result<()> {
        if let Some(id) = instance.pipeline {
            let expected = if audio {
                PipelineKind::Audio
            } else {
                PipelineKind::Video
            };
            anyhow::ensure!(
                self.pipeline(id)
                    .is_some_and(|pipeline| pipeline.kind == expected),
                "{label} references an invalid {:?} pipeline {id}",
                expected
            );
        }
        validate_ids(
            &format!("local node on {label}"),
            instance.local_nodes.iter().map(|node| node.id),
            u64::MAX,
        )?;
        validate_image_graph(&instance.local_nodes, &instance.local_output, label)
    }

    pub(crate) fn reconcile_plugin_metadata(&mut self, plugins: &PluginRegistry) {
        fn reconcile(node: &mut EffectNode, plugins: &PluginRegistry) {
            let (primary, inputs) = if let Some(effect) = plugins.effect(&node.node_type) {
                (effect.primary_image_input(), effect.inputs.as_slice())
            } else if let Some(effect) = plugins.audio_effect(&node.node_type) {
                (Some("audio"), effect.inputs.as_slice())
            } else if let Some(generator) = plugins.generator(&node.node_type) {
                if node.node_type == "builtin.shape" && !node.inputs.contains_key("shape_type") {
                    node.inputs
                        .insert("shape_type".into(), Binding::Constant(GpuValue::Enum(1)));
                }
                (None, generator.inputs.as_slice())
            } else {
                return;
            };

            node.inputs
                .entry("enabled".into())
                .or_insert(Binding::Constant(GpuValue::Bool(true)));
            for input in inputs {
                if input.id == "enabled" {
                    continue;
                }
                if matches!(
                    input.ty,
                    crate::plugin::InputType::Text
                        | crate::plugin::InputType::Vec2Array
                        | crate::plugin::InputType::F32List
                ) {
                    if !node.host_inputs.contains_key(&input.id) {
                        if let Ok(default) = input.ty.default_host(&input.default) {
                            node.host_inputs.insert(input.id.clone(), default);
                        }
                    }
                } else if !node.inputs.contains_key(&input.id) {
                    if let Ok(default) = input.ty.default_gpu(&input.default) {
                        node.inputs
                            .insert(input.id.clone(), Binding::Constant(default));
                    }
                }
            }

            node.stack_input = primary
                .filter(|input| node.image_inputs.contains_key(*input))
                .map(str::to_owned);
        }

        for pipeline in &mut self.pipelines {
            for node in &mut pipeline.nodes {
                reconcile(node, plugins);
            }
        }
        for composition in &mut self.compositions {
            for clip in &mut composition.timeline.clips {
                if let VisualSource::Generator(GeneratorSource::Plugin {
                    generator_type,
                    parameters,
                }) = &mut clip.source
                {
                    if let Some(definition) = plugins.generator(generator_type) {
                        if generator_type == "builtin.shape"
                            && !parameters.contains_key("shape_type")
                        {
                            parameters.insert(
                                "shape_type".into(),
                                HostBinding::Gpu(Binding::Constant(GpuValue::Enum(1))),
                            );
                        }
                        for input in &definition.inputs {
                            if !parameters.contains_key(&input.id) {
                                if let Ok(default) = input.ty.default_host(&input.default) {
                                    parameters.insert(input.id.clone(), default);
                                }
                            }
                        }
                    }
                }
                for node in &mut clip.pipeline.local_nodes {
                    reconcile(node, plugins);
                }
            }
            for pipeline in composition
                .timeline
                .tracks
                .iter_mut()
                .filter_map(|track| track.pipeline.as_mut())
            {
                for node in &mut pipeline.local_nodes {
                    reconcile(node, plugins);
                }
            }
            for track in &mut composition.timeline.tracks {
                for row in &mut track.property_rows {
                    if let VisualSource::Generator(GeneratorSource::Plugin {
                        generator_type,
                        parameters,
                    }) = &mut row.source
                    {
                        if let Some(definition) = plugins.generator(generator_type) {
                            if generator_type == "builtin.shape"
                                && !parameters.contains_key("shape_type")
                            {
                                parameters.insert(
                                    "shape_type".into(),
                                    HostBinding::Gpu(Binding::Constant(GpuValue::Enum(1))),
                                );
                            }
                            for input in &definition.inputs {
                                if !parameters.contains_key(&input.id) {
                                    if let Ok(default) = input.ty.default_host(&input.default) {
                                        parameters.insert(input.id.clone(), default);
                                    }
                                }
                            }
                        }
                    }
                    for node in &mut row.pipeline.local_nodes {
                        reconcile(node, plugins);
                    }
                }
            }
        }
    }

    fn repair_id_counters(&mut self) {
        self.next_media_id =
            next_u64_id(self.next_media_id, self.media.iter().map(|asset| asset.id));
        self.next_pipeline_id = next_u64_id(
            self.next_pipeline_id,
            self.pipelines.iter().map(|pipeline| pipeline.id),
        );
        let shared_node_ids = self.pipelines.iter().flat_map(|pipeline| {
            pipeline
                .nodes
                .iter()
                .map(|node| node.id)
                .chain(pipeline.value_nodes.iter().map(|node| node.id))
        });
        let local_node_ids = self.compositions.iter().flat_map(|composition| {
            let clip_nodes = composition
                .timeline
                .clips
                .iter()
                .flat_map(|clip| clip.pipeline.local_nodes.iter().map(|node| node.id));
            let track_nodes = composition
                .timeline
                .tracks
                .iter()
                .filter_map(|track| track.pipeline.as_ref())
                .flat_map(|pipeline| pipeline.local_nodes.iter().map(|node| node.id));
            let row_nodes = composition
                .timeline
                .tracks
                .iter()
                .flat_map(|track| track.property_rows.iter())
                .flat_map(|row| row.pipeline.local_nodes.iter().map(|node| node.id));
            clip_nodes.chain(track_nodes).chain(row_nodes)
        });
        self.next_node_id = next_u64_id(self.next_node_id, shared_node_ids.chain(local_node_ids));
        self.next_composition_id = next_u64_id(
            self.next_composition_id,
            self.compositions.iter().map(|composition| composition.id),
        );
    }

    pub fn authored_signature(&mut self) -> Vec<u8> {
        let waveforms = self
            .media
            .iter_mut()
            .map(|asset| asset.waveform.take())
            .collect::<Vec<_>>();
        let signature =
            serde_json::to_vec(&*self).unwrap_or_else(|_| format!("{self:?}").into_bytes());
        for (asset, waveform) in self.media.iter_mut().zip(waveforms) {
            asset.waveform = waveform;
        }
        signature
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        let mut persisted = self.clone();
        persisted.format_version = KAMA_FORMAT_VERSION;
        for asset in &mut persisted.media {
            let absolute = absolute_media_path(base, &asset.path);
            asset.absolute_path = absolute.clone();
            asset.relative_path = relative_media_path(base, &absolute).unwrap_or_default();
        }
        for composition in &mut persisted.compositions {
            composition.timeline.make_paths_relative(base);
        }
        atomic_write_json(path, &persisted).with_context(|| format!("write {}", path.display()))
    }

    pub fn composition(&self, id: CompositionId) -> Option<&Composition> {
        self.compositions
            .iter()
            .find(|composition| composition.id == id)
    }

    pub fn composition_mut(&mut self, id: CompositionId) -> Option<&mut Composition> {
        self.compositions
            .iter_mut()
            .find(|composition| composition.id == id)
    }

    pub fn active_composition(&self) -> &Composition {
        self.composition(self.active_composition)
            .or_else(|| self.compositions.first())
            .expect("project always has at least one composition")
    }

    pub fn active_composition_mut(&mut self) -> &mut Composition {
        let id = self.active_composition;
        let index = self
            .compositions
            .iter()
            .position(|composition| composition.id == id)
            .unwrap_or(0);
        &mut self.compositions[index]
    }

    pub fn active_settings(&self) -> &CompositionSettings {
        &self.active_composition().settings
    }

    pub fn set_active_composition(&mut self, id: CompositionId) -> bool {
        if self.composition(id).is_none() {
            return false;
        }
        self.active_composition = id;
        true
    }

    pub fn sync_active_timeline(&mut self, timeline: TimelineDocument) {
        self.active_composition_mut().timeline = timeline;
    }

    pub fn create_composition(&mut self, name: impl Into<String>) -> CompositionId {
        let id = self.next_composition_id.max(1);
        self.next_composition_id = id.saturating_add(1);
        self.compositions.push(Composition::new(id, name));
        id
    }

    pub fn rename_composition(&mut self, id: CompositionId, name: impl Into<String>) -> bool {
        let name = name.into();
        let name = name.trim();
        if name.is_empty() {
            return false;
        }
        let Some(composition) = self.composition_mut(id) else {
            return false;
        };
        if composition.name == name {
            return false;
        }
        composition.name = name.to_owned();
        let name = name.to_owned();
        for composition in &mut self.compositions {
            for clip in &mut composition.timeline.clips {
                if matches!(&clip.source, VisualSource::Composition(child) if *child == id) {
                    clip.name = name.clone();
                }
            }
        }
        true
    }

    pub fn duplicate_composition(&mut self, id: CompositionId) -> Option<CompositionId> {
        let source = self.composition(id)?.clone();
        let base = format!("{} Copy", source.name);
        let name = if self
            .compositions
            .iter()
            .all(|composition| composition.name != base.as_str())
        {
            base
        } else {
            (2u32..)
                .map(|number| format!("{base} {number}"))
                .find(|candidate| {
                    self.compositions
                        .iter()
                        .all(|composition| composition.name != candidate.as_str())
                })
                .expect("composition copy suffix space is unbounded")
        };
        let new_id = self.next_composition_id.max(1);
        self.next_composition_id = new_id.saturating_add(1);
        let mut duplicate = source;
        duplicate.id = new_id;
        duplicate.name = name;
        self.compositions.push(duplicate);
        Some(new_id)
    }

    pub fn remove_composition(&mut self, id: CompositionId) -> bool {
        if self.compositions.len() <= 1 {
            return false;
        }
        let Some(index) = self
            .compositions
            .iter()
            .position(|composition| composition.id == id)
        else {
            return false;
        };
        self.compositions.remove(index);
        for composition in &mut self.compositions {
            composition.timeline.clips.retain(
                |clip| !matches!(&clip.source, VisualSource::Composition(child) if *child == id),
            );
            composition.timeline.prune_unused_property_rows();
        }
        if self.active_composition == id {
            self.active_composition = self.compositions[index.min(self.compositions.len() - 1)].id;
        }
        true
    }

    pub fn composition_duration(&self, id: CompositionId) -> Option<f32> {
        let composition = self.composition(id)?;
        let content_end = composition
            .timeline
            .clips
            .iter()
            .map(|clip| clip.end())
            .fold(0.0f32, f32::max);
        let explicit_or_content = composition.timeline.end_time.unwrap_or(content_end);
        Some(if explicit_or_content <= 0.0 {
            5.0
        } else {
            explicit_or_content
                .max(content_end)
                .max(1.0f32 / composition.settings.frame_rate.max(1.0) as f32)
        })
    }

    pub fn composition_has_audio(&self, id: CompositionId) -> bool {
        let Some(composition) = self.composition(id) else {
            return false;
        };
        let audio_tracks = composition
            .timeline
            .tracks
            .iter()
            .filter(|track| track.kind == crate::timeline::TrackKind::Audio)
            .map(|track| track.id)
            .collect::<std::collections::HashSet<_>>();
        composition
            .timeline
            .clips
            .iter()
            .any(|clip| audio_tracks.contains(&clip.track))
    }

    pub fn can_reference_composition(&self, parent: CompositionId, child: CompositionId) -> bool {
        if parent == child
            || self.composition(parent).is_none()
            || self.composition(child).is_none()
        {
            return false;
        }
        let mut visited = std::collections::HashSet::new();
        !self.composition_reaches(child, parent, &mut visited)
    }

    fn composition_reaches(
        &self,
        from: CompositionId,
        target: CompositionId,
        visited: &mut std::collections::HashSet<CompositionId>,
    ) -> bool {
        if from == target {
            return true;
        }
        if !visited.insert(from) {
            return false;
        }
        let Some(composition) = self.composition(from) else {
            return false;
        };
        composition.timeline.clips.iter().any(|clip| {
            let VisualSource::Composition(child) = &clip.source else {
                return false;
            };
            *child == target || self.composition_reaches(*child, target, visited)
        })
    }

    pub fn import_media(&mut self, path: PathBuf) -> Result<MediaId> {
        let id = self.next_media_id;
        self.next_media_id += 1;
        let asset = media_asset_from_path(id, path)?;
        self.media.push(asset);
        Ok(id)
    }

    pub fn replace_media(&mut self, id: MediaId, path: PathBuf) -> Result<()> {
        let Some(index) = self.media.iter().position(|asset| asset.id == id) else {
            anyhow::bail!("media asset {id} does not exist");
        };
        let replacement = media_asset_from_path(id, path)?;
        ensure_media_replacement_compatible(self.media[index].kind, replacement.kind)?;
        self.media[index] = replacement;
        Ok(())
    }

    pub fn validate_media_replacement(&self, id: MediaId, path: &Path) -> Result<()> {
        let Some(current) = self.media(id) else {
            anyhow::bail!("media asset {id} does not exist");
        };
        let replacement = media_asset_from_path(id, path.to_path_buf())?;
        ensure_media_replacement_compatible(current.kind, replacement.kind)
    }

    pub(crate) fn resolve_missing_media_paths(&mut self, base: &Path) -> usize {
        let mut resolved = 0;
        for asset in &mut self.media {
            if asset.path.is_file() {
                continue;
            }
            let candidate = resolve_stored_media_path(asset, base);
            if candidate.is_file() && candidate != asset.path {
                asset.path = candidate.clone();
                asset.absolute_path = absolute_media_path(base, &candidate);
                if asset.relative_path.as_os_str().is_empty() {
                    asset.relative_path =
                        relative_media_path(base, &asset.absolute_path).unwrap_or_default();
                }
                resolved += 1;
            }
        }
        resolved
    }

    pub(crate) fn update_media_path_references(&mut self, base: &Path) {
        for asset in &mut self.media {
            let absolute = absolute_media_path(base, &asset.path);
            asset.absolute_path = absolute.clone();
            asset.relative_path = relative_media_path(base, &absolute).unwrap_or_default();
        }
    }

    pub fn media(&self, id: MediaId) -> Option<&MediaAsset> {
        self.media.iter().find(|asset| asset.id == id)
    }

    pub fn remove_media(&mut self, media: &HashSet<MediaId>) -> usize {
        if media.is_empty() {
            return 0;
        }
        let before = self.media.len();
        self.media.retain(|asset| !media.contains(&asset.id));
        for composition in &mut self.compositions {
            composition.timeline.clips.retain(|clip| {
                !matches!(
                    &clip.source,
                    VisualSource::Media(id) | VisualSource::Audio(id) if media.contains(id)
                )
            });
            composition.timeline.prune_unused_property_rows();
        }
        before - self.media.len()
    }

    pub fn pipeline(&self, id: PipelineId) -> Option<&EffectPipeline> {
        self.pipelines.iter().find(|pipeline| pipeline.id == id)
    }

    pub(crate) fn pipeline_mut(&mut self, id: PipelineId) -> Option<&mut EffectPipeline> {
        self.pipelines.iter_mut().find(|pipeline| pipeline.id == id)
    }

    pub fn rename_pipeline(&mut self, id: PipelineId, name: impl Into<String>) -> bool {
        let Some(pipeline) = self.pipeline_mut(id) else {
            return false;
        };
        let name = name.into();
        let trimmed = name.trim();
        if trimmed.is_empty() || pipeline.name == trimmed {
            return false;
        }
        pipeline.name = trimmed.to_string();
        true
    }

    pub(crate) fn remove_pipeline(&mut self, id: PipelineId) -> Option<Vec<PipelineSelectorRemap>> {
        let index = self
            .pipelines
            .iter()
            .position(|pipeline| pipeline.id == id)?;

        let selector_remaps = self
            .pipelines
            .iter()
            .filter_map(|owner| {
                let nodes = owner
                    .nodes
                    .iter()
                    .filter(|node| node.node_type == crate::effects::PIPELINE_NODE_TYPE)
                    .map(|node| node.id)
                    .collect::<Vec<_>>();
                (!nodes.is_empty()).then(|| {
                    let options = self
                        .pipelines
                        .iter()
                        .filter(|candidate| {
                            candidate.kind == owner.kind && candidate.id != owner.id
                        })
                        .map(|candidate| candidate.id)
                        .collect::<Vec<_>>();
                    (owner.id, nodes, options)
                })
            })
            .collect::<Vec<_>>();
        self.pipelines.remove(index);
        for composition in &mut self.compositions {
            composition.timeline.clear_pipeline_references(id);
        }
        let mut applied_remaps = Vec::new();
        for (owner, nodes, old_options) in selector_remaps {
            if owner == id {
                continue;
            }
            let new_options = self
                .pipeline_node_options(owner)
                .into_iter()
                .map(|(pipeline, _)| pipeline)
                .collect::<Vec<_>>();
            if let Some(owner_pipeline) = self.pipeline_mut(owner) {
                let mut changed = false;
                for node_id in &nodes {
                    let Some(binding) = owner_pipeline
                        .node_mut(*node_id)
                        .and_then(|node| node.inputs.get_mut("pipeline"))
                    else {
                        continue;
                    };
                    remap_pipeline_selector_binding(binding, &old_options, &new_options);
                    changed = true;
                }
                if changed {
                    owner_pipeline.revision = owner_pipeline.revision.saturating_add(1);
                }
            }
            for composition in &mut self.compositions {
                let remap_instance = |instance: &mut PipelineInstance| {
                    if instance.pipeline != Some(owner) {
                        return;
                    }
                    for node_id in &nodes {
                        if let Some(binding) = instance.overrides.get_mut(*node_id, "pipeline") {
                            remap_pipeline_selector_binding(binding, &old_options, &new_options);
                        }
                    }
                };
                for track in &mut composition.timeline.tracks {
                    if let Some(instance) = &mut track.pipeline {
                        remap_instance(instance);
                    }
                }
                for clip in &mut composition.timeline.clips {
                    remap_instance(&mut clip.pipeline);
                }
            }
            applied_remaps.push(PipelineSelectorRemap {
                owner,
                nodes,
                old_options,
                new_options,
            });
        }
        Some(applied_remaps)
    }

    pub fn create_pipeline(&mut self) -> PipelineId {
        self.create_pipeline_kind(PipelineKind::Video)
    }

    pub fn create_pipeline_kind(&mut self, kind: PipelineKind) -> PipelineId {
        let id = self.next_pipeline_id;
        self.next_pipeline_id += 1;
        let label = if kind == PipelineKind::Audio {
            "Audio Pipeline"
        } else {
            "Effect Pipeline"
        };
        self.pipelines.push(EffectPipeline {
            id,
            name: format!("{label} {id}"),
            revision: 1,
            kind,
            nodes: Vec::new(),
            value_nodes: Vec::new(),
            output: ImageBinding::PipelineInput,
            ui_input_position: None,
            ui_output_position: None,
        });
        id
    }

    pub fn duplicate_pipeline(&mut self, pipeline: PipelineId) -> Option<PipelineId> {
        let source = self.pipeline(pipeline)?.clone();
        let id = self.next_pipeline_id;
        self.next_pipeline_id += 1;
        let mut duplicate = source;
        duplicate.id = id;
        duplicate.name = format!("{} Copy", duplicate.name);

        duplicate.revision = duplicate.revision.saturating_add(1);
        self.pipelines.push(duplicate);
        Some(id)
    }

    pub fn add_plugin_node(
        &mut self,
        pipeline: PipelineId,
        definition: &EffectDefinition,
    ) -> Option<NodeId> {
        let id = self.add_plugin_node_at(pipeline, definition, None)?;
        let pipeline = self.pipeline_mut(pipeline)?;
        let previous = pipeline.output.clone();
        if let Some(input) = definition.primary_image_input() {
            pipeline
                .node_mut(id)?
                .image_inputs
                .insert(input.into(), previous);
        }
        pipeline.output = ImageBinding::Node(SocketRef {
            node: id,
            output: "image".into(),
        });
        Some(id)
    }

    fn add_effect_node_at(
        &mut self,
        pipeline: PipelineId,
        kind: PipelineKind,
        ui_position: Option<[f32; 2]>,
        instantiate: impl FnOnce(NodeId) -> Result<EffectNode>,
    ) -> Option<NodeId> {
        let index = self
            .pipelines
            .iter()
            .position(|candidate| candidate.id == pipeline && candidate.kind == kind)?;
        let id = self.next_node_id;
        let next_id = id.checked_add(1)?;
        let mut node = instantiate(id).ok()?;
        node.ui_position = ui_position;
        self.next_node_id = next_id;
        let pipeline = &mut self.pipelines[index];
        pipeline.nodes.push(node);
        pipeline.revision = pipeline.revision.saturating_add(1);
        Some(id)
    }

    pub fn add_plugin_node_at(
        &mut self,
        pipeline: PipelineId,
        definition: &EffectDefinition,
        ui_position: Option<[f32; 2]>,
    ) -> Option<NodeId> {
        self.add_effect_node_at(pipeline, PipelineKind::Video, ui_position, |id| {
            definition.instantiate(id)
        })
    }

    pub fn add_generator_node_at(
        &mut self,
        pipeline: PipelineId,
        definition: &GeneratorDefinition,
        ui_position: Option<[f32; 2]>,
    ) -> Option<NodeId> {
        self.add_effect_node_at(pipeline, PipelineKind::Video, ui_position, |id| {
            definition.instantiate_graph_node(id)
        })
    }

    pub fn add_audio_node_at(
        &mut self,
        pipeline: PipelineId,
        definition: &AudioEffectDefinition,
        ui_position: Option<[f32; 2]>,
    ) -> Option<NodeId> {
        self.add_effect_node_at(pipeline, PipelineKind::Audio, ui_position, |id| {
            definition.instantiate(id)
        })
    }

    pub fn append_audio_node(
        &mut self,
        pipeline: PipelineId,
        definition: &AudioEffectDefinition,
    ) -> Option<NodeId> {
        self.append_audio_node_at(pipeline, definition, None)
    }

    pub fn append_audio_node_at(
        &mut self,
        pipeline: PipelineId,
        definition: &AudioEffectDefinition,
        ui_position: Option<[f32; 2]>,
    ) -> Option<NodeId> {
        let id = self.add_audio_node_at(pipeline, definition, ui_position)?;
        let pipeline = self.pipeline_mut(pipeline)?;
        let previous = pipeline.output.clone();
        pipeline
            .node_mut(id)?
            .image_inputs
            .insert("audio".into(), previous);
        pipeline.output = ImageBinding::Node(SocketRef {
            node: id,
            output: "audio".into(),
        });
        Some(id)
    }

    pub fn add_pipeline_node_at(
        &mut self,
        pipeline: PipelineId,
        ui_position: Option<[f32; 2]>,
    ) -> Option<NodeId> {
        let kind = self.pipeline(pipeline)?.kind;
        let input_name = if kind == PipelineKind::Audio {
            "audio"
        } else {
            "image"
        };
        let has_candidate = self
            .pipelines
            .iter()
            .any(|candidate| candidate.kind == kind && candidate.id != pipeline);
        let selected = if has_candidate { 1 } else { 0 };
        self.add_effect_node_at(pipeline, kind, ui_position, |id| {
            Ok(EffectNode {
                id,
                node_type: crate::effects::PIPELINE_NODE_TYPE.into(),
                execution: crate::effects::NodeExecution::SpatialGpu,
                ui_position: None,
                image_inputs: BTreeMap::from([(input_name.into(), ImageBinding::PipelineInput)]),
                stack_input: Some(input_name.into()),
                inputs: BTreeMap::from([
                    ("enabled".into(), Binding::Constant(GpuValue::Bool(true))),
                    (
                        "pipeline".into(),
                        Binding::Constant(GpuValue::Enum(selected)),
                    ),
                ]),
                host_inputs: BTreeMap::new(),
                dynamic_image_inputs: None,
            })
        })
    }

    pub fn pipeline_node_target_index(
        &self,
        owner: PipelineId,
        index: u32,
    ) -> Option<&EffectPipeline> {
        let owner_kind = self.pipeline(owner)?.kind;
        let index = index.checked_sub(1)?;
        self.pipelines
            .iter()
            .filter(|candidate| candidate.kind == owner_kind && candidate.id != owner)
            .nth(index as usize)
    }

    pub fn pipeline_node_options(&self, owner: PipelineId) -> Vec<(PipelineId, String)> {
        let Some(kind) = self.pipeline(owner).map(|pipeline| pipeline.kind) else {
            return Vec::new();
        };
        self.pipelines
            .iter()
            .filter(|candidate| candidate.kind == kind && candidate.id != owner)
            .map(|candidate| (candidate.id, candidate.name.clone()))
            .collect()
    }

    pub fn add_value_node_at(
        &mut self,
        pipeline: PipelineId,
        kind: ValueNodeKind,
        ui_position: Option<[f32; 2]>,
    ) -> Option<NodeId> {
        let index = self
            .pipelines
            .iter()
            .position(|candidate| candidate.id == pipeline)?;
        let id = self.next_node_id;
        let next_id = id.checked_add(1)?;
        let value = match kind {
            ValueNodeKind::Float => GpuValue::F32(0.0),
            ValueNodeKind::Vec2 => GpuValue::Vec2([0.0, 0.0]),
            ValueNodeKind::Color => GpuValue::Color([1.0, 1.0, 1.0, 1.0]),
            _ => GpuValue::F32(0.0),
        };
        let inputs: BTreeMap<String, Binding> = kind
            .input_names()
            .iter()
            .map(|name| {
                let default = match (kind, *name) {
                    (ValueNodeKind::Divide | ValueNodeKind::Modulo | ValueNodeKind::Power, "B") => {
                        1.0
                    }
                    (ValueNodeKind::Clamp, "Max") => 1.0,
                    (ValueNodeKind::Lerp, "B") => 1.0,
                    (ValueNodeKind::Lerp, "T") => 0.5,
                    _ => 0.0,
                };
                (
                    (*name).to_string(),
                    Binding::Constant(GpuValue::F32(default)),
                )
            })
            .collect();
        self.next_node_id = next_id;
        let pipeline = &mut self.pipelines[index];
        pipeline.value_nodes.push(ValueNode {
            id,
            kind,
            value,
            inputs,
            ui_position,
        });
        pipeline.revision = pipeline.revision.saturating_add(1);
        Some(id)
    }

    pub fn set_value_node_position(
        &mut self,
        pipeline: PipelineId,
        node: NodeId,
        position: [f32; 2],
    ) -> bool {
        let Some(node) = self
            .pipeline_mut(pipeline)
            .and_then(|pipeline| pipeline.value_node_mut(node))
        else {
            return false;
        };
        node.ui_position = Some(position);
        true
    }

    pub fn set_value_node_value(
        &mut self,
        pipeline: PipelineId,
        node: NodeId,
        value: GpuValue,
    ) -> bool {
        let Some(value_node) = self
            .pipeline_mut(pipeline)
            .and_then(|pipeline| pipeline.value_node_mut(node))
        else {
            return false;
        };
        if !value_node.kind.is_constant() {
            return false;
        }
        value_node.value = value;
        true
    }

    pub fn set_value_node_component(
        &mut self,
        pipeline: PipelineId,
        node: NodeId,
        component: usize,
        value: f32,
        linked: bool,
    ) -> bool {
        let Some(pipeline) = self.pipeline_mut(pipeline) else {
            return false;
        };
        let Some(node) = pipeline.value_node_mut(node) else {
            return false;
        };
        if !node.kind.is_constant() {
            return false;
        }
        let Some(next) = node.value.with_component(component, value, linked) else {
            return false;
        };
        node.value = next;

        true
    }

    pub fn set_value_node_input_value(
        &mut self,
        pipeline: PipelineId,
        node: NodeId,
        input: &str,
        value: GpuValue,
    ) -> bool {
        let Some(binding) = self
            .pipeline_mut(pipeline)
            .and_then(|pipeline| pipeline.value_node_mut(node))
            .and_then(|node| node.inputs.get_mut(input))
        else {
            return false;
        };
        binding.set_value(0.0, value);
        true
    }

    pub fn set_value_node_input_component(
        &mut self,
        pipeline: PipelineId,
        node: NodeId,
        input: &str,
        component: usize,
        value: f32,
        linked: bool,
    ) -> bool {
        let Some(binding) = self
            .pipeline_mut(pipeline)
            .and_then(|pipeline| pipeline.value_node_mut(node))
            .and_then(|node| node.inputs.get_mut(input))
        else {
            return false;
        };
        let Some(current) = binding.evaluate(0.0) else {
            return false;
        };
        let Some(next) = current.with_component(component, value, linked) else {
            return false;
        };
        binding.set_value(0.0, next);
        true
    }

    fn edit_pipeline_node_input(
        &mut self,
        pipeline: PipelineId,
        node: NodeId,
        input: &str,
        edit: impl FnOnce(&mut Binding) -> bool,
    ) -> bool {
        let Some(pipeline) = self.pipeline_mut(pipeline) else {
            return false;
        };
        let topology_changed = {
            let Some(node) = pipeline.node_mut(node) else {
                return false;
            };
            let Some(binding) = node.inputs.get_mut(input) else {
                return false;
            };
            if matches!(binding, Binding::Connection(_)) || !edit(binding) {
                return false;
            }
            node.sync_dynamic_image_inputs()
        };
        if topology_changed {
            pipeline.revision = pipeline.revision.saturating_add(1);
        }
        true
    }

    pub fn set_pipeline_node_value(
        &mut self,
        pipeline: PipelineId,
        node: NodeId,
        input: &str,
        value: GpuValue,
    ) -> bool {
        self.set_pipeline_node_value_at(pipeline, node, input, 0.0, value)
    }

    pub fn set_pipeline_node_value_at(
        &mut self,
        pipeline: PipelineId,
        node: NodeId,
        input: &str,
        time: f64,
        value: GpuValue,
    ) -> bool {
        self.edit_pipeline_node_input(pipeline, node, input, |binding| {
            binding.set_value(time, value);
            true
        })
    }

    pub fn set_pipeline_node_component(
        &mut self,
        pipeline: PipelineId,
        node: NodeId,
        input: &str,
        component: usize,
        value: f32,
        linked: bool,
    ) -> bool {
        self.set_pipeline_node_component_at(pipeline, node, input, 0.0, (component, value, linked))
    }

    pub fn set_pipeline_node_component_at(
        &mut self,
        pipeline: PipelineId,
        node: NodeId,
        input: &str,
        time: f64,
        edit: (usize, f32, bool),
    ) -> bool {
        let (component, value, linked) = edit;
        self.edit_pipeline_node_input(pipeline, node, input, |binding| {
            binding.set_component_value(time, component, value, linked)
        })
    }

    pub fn toggle_pipeline_node_keyframe(
        &mut self,
        pipeline: PipelineId,
        node: NodeId,
        input: &str,
        time: f64,
    ) -> bool {
        self.edit_pipeline_node_input(pipeline, node, input, |binding| {
            binding.toggle_keyframe(time);
            true
        })
    }

    pub fn pipeline_node_host_value(
        &self,
        pipeline: PipelineId,
        node: NodeId,
        input: &str,
        time: f64,
    ) -> Option<HostValue> {
        self.pipeline(pipeline)?
            .node(node)?
            .host_inputs
            .get(input)?
            .evaluate(time)
    }

    pub fn set_pipeline_node_host_value(
        &mut self,
        pipeline: PipelineId,
        node: NodeId,
        input: &str,
        time: f64,
        value: HostValue,
    ) -> bool {
        let Some(binding) = self
            .pipeline_mut(pipeline)
            .and_then(|pipeline| pipeline.node_mut(node))
            .and_then(|node| node.host_inputs.get_mut(input))
        else {
            return false;
        };
        binding.set_value(time, value);
        true
    }

    pub fn pipeline_node_host_has_keyframe(
        &self,
        pipeline: PipelineId,
        node: NodeId,
        input: &str,
        time: f64,
    ) -> bool {
        self.pipeline(pipeline)
            .and_then(|pipeline| pipeline.node(node))
            .and_then(|node| node.host_inputs.get(input))
            .is_some_and(|binding| binding.has_keyframe(time))
    }

    pub fn pipeline_node_host_has_keyframes(
        &self,
        pipeline: PipelineId,
        node: NodeId,
        input: &str,
    ) -> bool {
        self.pipeline(pipeline)
            .and_then(|pipeline| pipeline.node(node))
            .and_then(|node| node.host_inputs.get(input))
            .is_some_and(HostBinding::has_keyframes)
    }

    pub fn toggle_pipeline_node_host_keyframe(
        &mut self,
        pipeline: PipelineId,
        node: NodeId,
        input: &str,
        time: f64,
    ) -> bool {
        let Some(binding) = self
            .pipeline_mut(pipeline)
            .and_then(|pipeline| pipeline.node_mut(node))
            .and_then(|node| node.host_inputs.get_mut(input))
        else {
            return false;
        };
        binding.toggle_keyframe(time);
        true
    }

    pub fn remove_value_node(&mut self, pipeline: PipelineId, node: NodeId) -> bool {
        let Some(pipeline) = self.pipeline_mut(pipeline) else {
            return false;
        };
        let Some(index) = pipeline
            .value_nodes
            .iter()
            .position(|candidate| candidate.id == node)
        else {
            return false;
        };
        let fallback = evaluate_value_node(
            &pipeline.value_nodes,
            node,
            ValueEvalContext {
                timeline_time: 0.0,
                local_time: 0.0,
                frame_index: 0,
                frame_rate: 60.0,
            },
        )
        .unwrap_or(pipeline.value_nodes[index].value)
        .zeroed();
        let _removed = pipeline.value_nodes.remove(index);
        for effect in &mut pipeline.nodes {
            for binding in effect.inputs.values_mut() {
                if matches!(binding, Binding::Connection(socket) if socket.node == node) {
                    *binding = Binding::Constant(fallback);
                }
            }
        }
        for value in &mut pipeline.value_nodes {
            for binding in value.inputs.values_mut() {
                if matches!(binding, Binding::Connection(socket) if socket.node == node) {
                    *binding = Binding::Constant(fallback);
                }
            }
        }
        pipeline.revision = pipeline.revision.saturating_add(1);
        true
    }

    pub fn connect_pipeline_value(
        &mut self,
        pipeline: PipelineId,
        node: NodeId,
        input: &str,
        source: NodeId,
    ) -> bool {
        let Some(pipeline) = self.pipeline_mut(pipeline) else {
            return false;
        };
        if !pipeline
            .value_nodes
            .iter()
            .any(|candidate| candidate.id == source)
        {
            return false;
        }
        let targets_value_node = pipeline
            .value_nodes
            .iter()
            .any(|candidate| candidate.id == node);
        if targets_value_node {
            if source == node
                || ValueGraphIndex::new(&pipeline.value_nodes).depends_on(source, node)
            {
                return false;
            }
        } else if pipeline
            .node(node)
            .and_then(|target| target.dynamic_image_inputs.as_ref())
            .is_some_and(|dynamic| dynamic.count_input == input)
        {
            return false;
        }
        let binding = if targets_value_node {
            pipeline
                .value_node_mut(node)
                .and_then(|target| target.inputs.get_mut(input))
        } else {
            pipeline
                .node_mut(node)
                .and_then(|target| target.inputs.get_mut(input))
        };
        let Some(binding) = binding else {
            return false;
        };
        *binding = Binding::Connection(SocketRef {
            node: source,
            output: "value".into(),
        });
        pipeline.revision = pipeline.revision.saturating_add(1);
        true
    }

    pub fn disconnect_pipeline_value(
        &mut self,
        pipeline: PipelineId,
        node: NodeId,
        input: &str,
    ) -> bool {
        let Some(pipeline) = self.pipeline_mut(pipeline) else {
            return false;
        };
        let target_is_value = pipeline
            .value_nodes
            .iter()
            .any(|candidate| candidate.id == node);
        let source = if target_is_value {
            pipeline
                .value_nodes
                .iter()
                .find(|candidate| candidate.id == node)
                .and_then(|target| target.inputs.get(input))
        } else {
            pipeline
                .nodes
                .iter()
                .find(|candidate| candidate.id == node)
                .and_then(|target| target.inputs.get(input))
        }
        .and_then(|binding| match binding {
            Binding::Connection(socket) => Some(socket.node),
            _ => None,
        });
        let Some(source) = source else {
            return false;
        };
        let fallback = evaluate_value_node(
            &pipeline.value_nodes,
            source,
            ValueEvalContext {
                timeline_time: 0.0,
                local_time: 0.0,
                frame_index: 0,
                frame_rate: 60.0,
            },
        )
        .or_else(|| {
            pipeline
                .value_nodes
                .iter()
                .find(|candidate| candidate.id == source)
                .map(|source| source.value)
        })
        .unwrap_or(GpuValue::F32(0.0))
        .zeroed();
        let binding = if target_is_value {
            pipeline
                .value_node_mut(node)
                .and_then(|target| target.inputs.get_mut(input))
        } else {
            pipeline
                .node_mut(node)
                .and_then(|target| target.inputs.get_mut(input))
        };
        let Some(binding) = binding else {
            return false;
        };
        *binding = Binding::Constant(fallback);
        pipeline.revision = pipeline.revision.saturating_add(1);
        true
    }

    pub fn set_pipeline_endpoint_position(
        &mut self,
        pipeline: PipelineId,
        input: bool,
        position: [f32; 2],
    ) -> bool {
        let Some(pipeline) = self.pipeline_mut(pipeline) else {
            return false;
        };
        if input {
            pipeline.ui_input_position = Some(position);
        } else {
            pipeline.ui_output_position = Some(position);
        }
        true
    }

    #[cfg(test)]
    pub fn add_builtin_node(
        &mut self,
        pipeline: PipelineId,
        preset: BuiltinNodePreset,
    ) -> Option<NodeId> {
        self.add_builtin_node_at(pipeline, preset, None)
    }

    #[cfg(test)]
    pub fn add_builtin_node_at(
        &mut self,
        pipeline: PipelineId,
        preset: BuiltinNodePreset,
        ui_position: Option<[f32; 2]>,
    ) -> Option<NodeId> {
        let id = self.next_node_id;
        self.next_node_id += 1;
        let pipeline = self.pipeline_mut(pipeline)?;
        let mut node = EffectNode::builtin(id, preset);
        node.ui_position = ui_position;
        node.image_inputs
            .insert("image".into(), pipeline.output.clone());
        pipeline.nodes.push(node);
        pipeline.output = ImageBinding::Node(SocketRef {
            node: id,
            output: "image".into(),
        });
        pipeline.revision = pipeline.revision.saturating_add(1);
        Some(id)
    }

    pub fn set_pipeline_node_position(
        &mut self,
        pipeline: PipelineId,
        node: NodeId,
        position: [f32; 2],
    ) -> bool {
        let Some(node) = self
            .pipeline_mut(pipeline)
            .and_then(|pipeline| pipeline.node_mut(node))
        else {
            return false;
        };
        node.ui_position = Some(position);

        true
    }

    pub fn remove_pipeline_node(&mut self, pipeline: PipelineId, node: NodeId) -> bool {
        let Some(pipeline) = self.pipeline_mut(pipeline) else {
            return false;
        };
        let Some(index) = pipeline
            .nodes
            .iter()
            .position(|candidate| candidate.id == node)
        else {
            return false;
        };
        let upstream = pipeline.nodes[index]
            .stack_image_input()
            .map(|(_, binding)| binding.clone())
            .unwrap_or(ImageBinding::Disconnected);
        pipeline.nodes.remove(index);
        for candidate in &mut pipeline.nodes {
            candidate.replace_image_source(node, &upstream);
        }
        if matches!(&pipeline.output, ImageBinding::Node(socket) if socket.node == node) {
            pipeline.output = upstream;
        }
        pipeline.revision = pipeline.revision.saturating_add(1);
        true
    }

    pub fn connect_pipeline_image_input(
        &mut self,
        pipeline: PipelineId,
        node: NodeId,
        input: &str,
        source: Option<NodeId>,
    ) -> bool {
        let Some(pipeline) = self.pipeline_mut(pipeline) else {
            return false;
        };
        let output = pipeline_output_socket(pipeline.kind);
        if !pipeline
            .nodes
            .iter()
            .find(|candidate| candidate.id == node)
            .is_some_and(|candidate| candidate.image_inputs.contains_key(input))
        {
            return false;
        }
        if let Some(source) = source {
            if source == node
                || !pipeline
                    .nodes
                    .iter()
                    .any(|candidate| candidate.id == source)
                || ImageGraphIndex::new(&pipeline.nodes).depends_on(source, node)
            {
                return false;
            }
        }
        let Some(target) = pipeline.node_mut(node) else {
            return false;
        };
        let binding = source.map_or(ImageBinding::PipelineInput, |node| {
            ImageBinding::Node(SocketRef {
                node,
                output: output.into(),
            })
        });
        target.image_inputs.insert(input.into(), binding);
        pipeline.revision = pipeline.revision.saturating_add(1);
        true
    }

    pub fn disconnect_pipeline_image_input(
        &mut self,
        pipeline: PipelineId,
        node: NodeId,
        input: &str,
    ) -> bool {
        let Some(pipeline) = self.pipeline_mut(pipeline) else {
            return false;
        };
        let Some(target) = pipeline.node_mut(node) else {
            return false;
        };
        let Some(binding) = target.image_inputs.get_mut(input) else {
            return false;
        };
        *binding = ImageBinding::Disconnected;
        pipeline.revision = pipeline.revision.saturating_add(1);
        true
    }

    pub fn insert_pipeline_node_on_wire(
        &mut self,
        pipeline: PipelineId,
        node: NodeId,
        source: Option<NodeId>,
        destination: Option<NodeId>,
        destination_input: Option<&str>,
    ) -> bool {
        if source == Some(node) || destination == Some(node) {
            return false;
        }
        self.edit_pipeline_graph(pipeline, "edited pipeline", |candidate| {
            let output = pipeline_output_socket(candidate.kind);
            if source.is_some_and(|source| candidate.node(source).is_none()) {
                return false;
            }
            let Some(inserted_input) = candidate
                .node(node)
                .and_then(EffectNode::stack_image_input_name)
                .map(str::to_owned)
            else {
                return false;
            };

            let destination_input = match destination {
                Some(destination) => {
                    let Some(target) = candidate.node(destination) else {
                        return false;
                    };
                    let Some(input) = destination_input
                        .filter(|input| target.image_inputs.contains_key(*input))
                        .or_else(|| target.stack_image_input_name())
                        .map(str::to_owned)
                    else {
                        return false;
                    };
                    let Some(existing) = target.image_inputs.get(&input) else {
                        return false;
                    };
                    if !image_binding_has_source(existing, source) {
                        return false;
                    }
                    Some(input)
                }
                None => {
                    if destination_input.is_some()
                        || !image_binding_has_source(&candidate.output, source)
                    {
                        return false;
                    }
                    None
                }
            };

            let source_binding = source.map_or(ImageBinding::PipelineInput, |source| {
                ImageBinding::Node(SocketRef {
                    node: source,
                    output: output.into(),
                })
            });
            let node_binding = ImageBinding::Node(SocketRef {
                node,
                output: output.into(),
            });
            let Some(inserted) = candidate.node_mut(node) else {
                return false;
            };
            inserted.image_inputs.insert(inserted_input, source_binding);
            if let Some((destination, input)) = destination.zip(destination_input) {
                let Some(target) = candidate.node_mut(destination) else {
                    return false;
                };
                target.image_inputs.insert(input, node_binding);
            } else {
                candidate.output = node_binding;
            }

            true
        })
    }

    pub fn disconnect_pipeline_output(&mut self, pipeline: PipelineId) -> bool {
        let Some(pipeline) = self.pipeline_mut(pipeline) else {
            return false;
        };
        if matches!(&pipeline.output, ImageBinding::Disconnected) {
            return false;
        }
        pipeline.output = ImageBinding::Disconnected;
        pipeline.revision = pipeline.revision.saturating_add(1);
        true
    }

    pub fn set_pipeline_output(&mut self, pipeline: PipelineId, source: Option<NodeId>) -> bool {
        let Some(pipeline) = self.pipeline_mut(pipeline) else {
            return false;
        };
        let output = pipeline_output_socket(pipeline.kind);
        if let Some(source) = source {
            if !pipeline
                .nodes
                .iter()
                .any(|candidate| candidate.id == source)
            {
                return false;
            }
        }
        pipeline.output = source.map_or(ImageBinding::PipelineInput, |node| {
            ImageBinding::Node(SocketRef {
                node,
                output: output.into(),
            })
        });
        pipeline.revision = pipeline.revision.saturating_add(1);
        true
    }

    pub fn move_pipeline_node(
        &mut self,
        pipeline: PipelineId,
        node: NodeId,
        direction: i32,
    ) -> bool {
        if direction == 0 {
            return false;
        }
        self.edit_pipeline_graph(pipeline, "reordered pipeline", |candidate| {
            let output = pipeline_output_socket(candidate.kind);
            let Some(mut path) = candidate
                .main_path()
                .into_iter()
                .map(|node| {
                    node.stack_image_input_name()
                        .map(|input| (node.id, input.to_owned()))
                })
                .collect::<Option<Vec<_>>>()
            else {
                return false;
            };
            let Some(path_index) = path.iter().position(|(candidate, _)| *candidate == node) else {
                return false;
            };
            let target = path_index as i32 + direction.signum();
            if target < 0 || target >= path.len() as i32 {
                return false;
            }
            path.swap(path_index, target as usize);

            let mut upstream = ImageBinding::PipelineInput;
            for (node_id, input) in path {
                let Some(node) = candidate.node_mut(node_id) else {
                    return false;
                };
                node.image_inputs.insert(input, upstream.clone());
                upstream = ImageBinding::Node(SocketRef {
                    node: node_id,
                    output: output.into(),
                });
            }
            candidate.output = upstream;
            true
        })
    }

    fn edit_pipeline_graph(
        &mut self,
        pipeline: PipelineId,
        label: &str,
        edit: impl FnOnce(&mut EffectPipeline) -> bool,
    ) -> bool {
        let Some(index) = self
            .pipelines
            .iter()
            .position(|candidate| candidate.id == pipeline)
        else {
            return false;
        };
        let mut candidate = self.pipelines[index].clone();
        if !edit(&mut candidate)
            || validate_image_graph(&candidate.nodes, &candidate.output, label).is_err()
        {
            return false;
        }
        candidate.revision = candidate.revision.saturating_add(1);
        self.pipelines[index] = candidate;
        true
    }
}

fn pipeline_output_socket(kind: PipelineKind) -> &'static str {
    match kind {
        PipelineKind::Video => "image",
        PipelineKind::Audio => "audio",
    }
}

fn repair_pipeline_reference(instance: &mut PipelineInstance, valid: &HashSet<PipelineId>) {
    if instance
        .pipeline
        .is_some_and(|pipeline| !valid.contains(&pipeline))
    {
        instance.pipeline = None;
        instance.overrides.clear();
    }
}

fn validate_ids(kind: &str, ids: impl Iterator<Item = u64>, exhausted_at: u64) -> Result<()> {
    let mut seen = HashSet::new();
    for id in ids {
        anyhow::ensure!(id != exhausted_at, "{kind} id space is exhausted");
        anyhow::ensure!(seen.insert(id), "duplicate {kind} id {id}");
    }
    Ok(())
}

fn next_u64_id(current: u64, ids: impl Iterator<Item = u64>) -> u64 {
    let required = ids.max().unwrap_or(0).saturating_add(1).max(1);
    if current == u64::MAX {
        required
    } else {
        current.max(required)
    }
}

fn media_asset_from_path(id: MediaId, path: PathBuf) -> Result<MediaAsset> {
    let path = absolute_media_path(Path::new("."), &path);
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Media")
        .to_string();
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let (kind, duration, frame_rate, video_width, video_height, has_audio, tracks) =
        if extension == "wasm" {
            (
                MediaKind::WasmPlugin,
                None,
                None,
                None,
                None,
                false,
                Vec::new(),
            )
        } else if let Ok((width, height)) = image::image_dimensions(&path) {
            (
                MediaKind::Image { width, height },
                None,
                None,
                Some(width),
                Some(height),
                false,
                Vec::new(),
            )
        } else {
            let probe = probe_av_media(&path).unwrap_or_default();
            if probe.has_video || probe.has_audio {
                let kind = if probe.has_video {
                    MediaKind::Video
                } else {
                    MediaKind::Audio
                };
                (
                    kind,
                    probe.duration,
                    probe.frame_rate,
                    probe.video_width,
                    probe.video_height,
                    probe.has_audio,
                    probe.tracks,
                )
            } else if crate::model3d::is_supported_path(&path) {
                let _ = crate::model3d::probe_size(&path)?;
                (
                    MediaKind::Model3d,
                    None,
                    None,
                    None,
                    None,
                    false,
                    Vec::new(),
                )
            } else {
                (
                    guess_media_kind(&path),
                    None,
                    None,
                    None,
                    None,
                    false,
                    Vec::new(),
                )
            }
        };
    Ok(MediaAsset {
        id,
        name,
        absolute_path: path.clone(),
        relative_path: PathBuf::new(),
        path,
        kind,
        duration,
        frame_rate,
        video_width,
        video_height,
        has_audio,
        tracks,
        waveform: None,
        legacy_model: None,
    })
}

fn ensure_media_replacement_compatible(current: MediaKind, replacement: MediaKind) -> Result<()> {
    let compatible = match current {
        MediaKind::Image { .. } | MediaKind::Video => {
            matches!(replacement, MediaKind::Image { .. } | MediaKind::Video)
        }
        MediaKind::Audio => matches!(replacement, MediaKind::Audio),
        MediaKind::Model3d => matches!(replacement, MediaKind::Model3d),
        MediaKind::WasmPlugin => matches!(replacement, MediaKind::WasmPlugin),
        MediaKind::Unknown => matches!(replacement, MediaKind::Unknown),
    };
    anyhow::ensure!(
        compatible,
        "replacement for {} media must be {}; selected file is {}",
        media_kind_label(current),
        media_replacement_requirement(current),
        media_kind_label(replacement),
    );
    Ok(())
}

fn media_kind_label(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Image { .. } => "image",
        MediaKind::Video => "video",
        MediaKind::Audio => "audio",
        MediaKind::Model3d => "3D model",
        MediaKind::WasmPlugin => "WASM plugin",
        MediaKind::Unknown => "unknown",
    }
}

fn media_replacement_requirement(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Image { .. } | MediaKind::Video => "an image or video",
        MediaKind::Audio => "audio",
        MediaKind::Model3d => "a 3D model",
        MediaKind::WasmPlugin => "a WASM plugin",
        MediaKind::Unknown => "the same media type",
    }
}

fn migrate_and_resolve_media_path(asset: &mut MediaAsset, base: &Path, format_version: u32) {
    if format_version < 7 {
        let legacy = asset.absolute_path.clone();
        if legacy.is_relative() {
            asset.relative_path = legacy.clone();
            asset.absolute_path = absolute_media_path(base, &legacy);
        } else {
            asset.absolute_path = legacy;
            asset.relative_path =
                relative_media_path(base, &asset.absolute_path).unwrap_or_default();
        }
    }

    let resolved = resolve_stored_media_path(asset, base);
    asset.path = resolved.clone();
    asset.absolute_path = absolute_media_path(base, &resolved);
    if asset.relative_path.as_os_str().is_empty() {
        asset.relative_path = relative_media_path(base, &asset.absolute_path).unwrap_or_default();
    }
}

fn resolve_stored_media_path(asset: &MediaAsset, base: &Path) -> PathBuf {
    let absolute = (!asset.absolute_path.as_os_str().is_empty())
        .then(|| absolute_media_path(base, &asset.absolute_path));
    if let Some(path) = absolute.as_ref().filter(|path| path.is_file()) {
        return path.clone();
    }

    let relative = (!asset.relative_path.as_os_str().is_empty())
        .then(|| absolute_media_path(base, &asset.relative_path));
    if let Some(path) = relative.as_ref().filter(|path| path.is_file()) {
        return path.clone();
    }

    absolute.or(relative).unwrap_or_default()
}

fn absolute_media_path(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        let base = if base.is_absolute() {
            base.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(base)
        };
        base.join(path)
    }
}

fn relative_media_path(base: &Path, target: &Path) -> Option<PathBuf> {
    let base = absolute_media_path(Path::new("."), base);
    let target = absolute_media_path(Path::new("."), target);
    let base_components = base.components().collect::<Vec<_>>();
    let target_components = target.components().collect::<Vec<_>>();

    let common = base_components
        .iter()
        .zip(&target_components)
        .take_while(|(left, right)| left == right)
        .count();
    if common == 0 && (base.is_absolute() || target.is_absolute()) {
        return None;
    }

    let mut relative = PathBuf::new();
    for component in &base_components[common..] {
        if matches!(component, std::path::Component::Normal(_)) {
            relative.push("..");
        }
    }
    for component in &target_components[common..] {
        relative.push(component.as_os_str());
    }
    Some(relative)
}

fn guess_media_kind(path: &Path) -> MediaKind {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(
        extension.as_str(),
        "mp4" | "mov" | "mkv" | "webm" | "avi" | "m4v"
    ) {
        MediaKind::Video
    } else if matches!(
        extension.as_str(),
        "wav" | "mp3" | "flac" | "aac" | "ogg" | "m4a"
    ) {
        MediaKind::Audio
    } else if crate::model3d::is_supported_path(path) {
        MediaKind::Model3d
    } else if extension == "wasm" {
        MediaKind::WasmPlugin
    } else {
        MediaKind::Unknown
    }
}

fn image_binding_has_source(binding: &ImageBinding, source: Option<NodeId>) -> bool {
    match (binding, source) {
        (ImageBinding::PipelineInput, None) => true,
        (ImageBinding::Node(socket), Some(source)) => socket.node == source,
        _ => false,
    }
}

fn validate_image_graph(nodes: &[EffectNode], output: &ImageBinding, label: &str) -> Result<()> {
    let graph = ImageGraphIndex::new(nodes);
    let validate_binding = |binding: &ImageBinding, context: &str| -> Result<()> {
        if let ImageBinding::Node(socket) = binding {
            anyhow::ensure!(
                graph.contains(socket.node),
                "{context} in {label} references missing image node {}",
                socket.node
            );
        }
        Ok(())
    };

    validate_binding(output, "output")?;
    for node in nodes {
        for (input, binding) in &node.image_inputs {
            validate_binding(binding, &format!("input {input} on node {}", node.id))?;
            if let ImageBinding::Node(socket) = binding {
                anyhow::ensure!(
                    socket.node != node.id && !graph.depends_on(socket.node, node.id),
                    "{label} contains an image graph cycle through node {}",
                    node.id
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn media_replacement_compatibility_keeps_visual_and_audio_categories_separate() {
        let image = MediaKind::Image {
            width: 1,
            height: 1,
        };
        assert!(ensure_media_replacement_compatible(image, MediaKind::Video).is_ok());
        assert!(ensure_media_replacement_compatible(MediaKind::Video, image).is_ok());
        assert!(ensure_media_replacement_compatible(MediaKind::Audio, MediaKind::Audio).is_ok());
        assert!(ensure_media_replacement_compatible(MediaKind::Audio, MediaKind::Video).is_err());
        assert!(ensure_media_replacement_compatible(MediaKind::Video, MediaKind::Audio).is_err());
    }

    #[test]
    fn composition_duplicate_names_are_unique_and_delete_keeps_project_valid() {
        let mut project = Project::new();
        let original = project.active_composition;
        let original_name = project.active_composition().name.clone();
        let first = project.duplicate_composition(original).unwrap();
        let second = project.duplicate_composition(original).unwrap();
        assert_ne!(first, original);
        assert_ne!(second, first);
        assert_eq!(
            project.composition(first).unwrap().name,
            format!("{original_name} Copy")
        );
        assert_eq!(
            project.composition(second).unwrap().name,
            format!("{original_name} Copy 2")
        );

        assert!(project.set_active_composition(first));
        assert!(project.remove_composition(first));
        assert_ne!(project.active_composition, first);
        assert!(project.composition(project.active_composition).is_some());
    }

    #[test]
    fn host_point_and_float_lists_interpolate_between_keyframes() {
        let points = HostKeyframeTrack {
            keys: vec![
                HostKeyframe {
                    time: 0.0,
                    value: HostValue::Vec2Array(vec![[0.0, 10.0], [20.0, 30.0]]),
                },
                HostKeyframe {
                    time: 2.0,
                    value: HostValue::Vec2Array(vec![[10.0, 30.0], [40.0, 50.0]]),
                },
            ],
        };
        assert_eq!(
            points.evaluate(1.0),
            Some(HostValue::Vec2Array(vec![[5.0, 20.0], [30.0, 40.0]]))
        );

        let bands = HostKeyframeTrack {
            keys: vec![
                HostKeyframe {
                    time: 0.0,
                    value: HostValue::F32List(vec![-6.0, 0.0, 6.0]),
                },
                HostKeyframe {
                    time: 2.0,
                    value: HostValue::F32List(vec![6.0, 12.0, -6.0]),
                },
            ],
        };
        assert_eq!(
            bands.evaluate(1.0),
            Some(HostValue::F32List(vec![0.0, 6.0, 0.0]))
        );
    }

    #[test]
    fn kama_round_trip_keeps_relative_asset_references() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("kama-project-test-{}-{unique}", std::process::id()));
        fs::create_dir_all(root.join("media")).unwrap();
        let project_path = root.join("test.kama");
        let media_path = root.join("media").join("still.png");

        let mut project = Project::new();
        project.name = "Round Trip".into();
        project.media.push(MediaAsset {
            id: 1,
            name: "still.png".into(),
            path: media_path.clone(),
            absolute_path: media_path.clone(),
            relative_path: PathBuf::new(),
            kind: MediaKind::Image {
                width: 64,
                height: 64,
            },
            duration: None,
            frame_rate: None,
            video_width: None,
            video_height: None,
            has_audio: false,
            tracks: Vec::new(),
            waveform: None,
            legacy_model: None,
        });
        project.next_media_id = 2;
        let pipeline = project.create_pipeline();
        assert!(project.rename_pipeline(pipeline, "Shared Grade"));
        let exposure = project
            .add_builtin_node(pipeline, BuiltinNodePreset::Exposure)
            .unwrap();
        let mut instance = PipelineInstance::effect_default();
        instance.pipeline = Some(pipeline);
        instance
            .overrides
            .insert(exposure, "exposure", Binding::Constant(GpuValue::F32(1.25)));
        project.active_composition_mut().timeline.tracks[0].pipeline = Some(instance);
        project.save(&project_path).unwrap();

        let persisted = fs::read_to_string(&project_path).unwrap();
        assert!(persisted.contains("\"absolute_path\""));
        assert!(persisted.contains("\"relative_path\""));
        assert!(persisted.contains("media/still.png") || persisted.contains("media\\\\still.png"));
        let loaded = Project::load(&project_path).unwrap();
        assert_eq!(loaded.name, "Round Trip");
        assert_eq!(loaded.media[0].path, media_path);
        assert_eq!(loaded.pipeline(pipeline).unwrap().name, "Shared Grade");
        let loaded_override = loaded.active_composition().timeline.tracks[0]
            .pipeline
            .as_ref()
            .and_then(|instance| instance.overrides.get(exposure, "exposure"))
            .and_then(|binding| binding.evaluate(0.0));
        assert_eq!(loaded_override, Some(GpuValue::F32(1.25)));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn kama_load_prefers_absolute_media_path_then_relative_fallback() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kama-project-path-test-{}-{unique}",
            std::process::id()
        ));
        let original = root.join("original");
        let moved = root.join("moved");
        fs::create_dir_all(original.join("media")).unwrap();
        fs::create_dir_all(moved.join("media")).unwrap();
        let original_media = original.join("media").join("still.png");
        let moved_media = moved.join("media").join("still.png");
        fs::write(&original_media, b"original").unwrap();
        fs::write(&moved_media, b"moved").unwrap();

        let mut project = Project::new();
        project.media.push(MediaAsset {
            id: 1,
            name: "still.png".into(),
            path: original_media.clone(),
            absolute_path: original_media.clone(),
            relative_path: PathBuf::from("media/still.png"),
            kind: MediaKind::Unknown,
            duration: None,
            frame_rate: None,
            video_width: None,
            video_height: None,
            has_audio: false,
            tracks: Vec::new(),
            waveform: None,
            legacy_model: None,
        });
        project.next_media_id = 2;

        let project_path = moved.join("test.kama");
        atomic_write_json(&project_path, &project).unwrap();
        let loaded = Project::load(&project_path).unwrap();
        assert_eq!(loaded.media[0].path, original_media);

        fs::remove_file(&original_media).unwrap();
        let loaded = Project::load(&project_path).unwrap();
        assert_eq!(loaded.media[0].path, moved_media);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn pipeline_image_connections_reject_cycles() {
        let mut project = Project::new();
        let pipeline = project.create_pipeline();
        let a = project
            .add_builtin_node(pipeline, BuiltinNodePreset::Exposure)
            .unwrap();
        let b = project
            .add_builtin_node(pipeline, BuiltinNodePreset::Contrast)
            .unwrap();
        assert!(!project.connect_pipeline_image_input(pipeline, a, "image", Some(b)));
        assert!(project.connect_pipeline_image_input(pipeline, b, "image", Some(a)));
    }

    #[test]
    fn inserting_node_on_named_image_wire_preserves_destination_socket() {
        let mut project = Project::new();
        let pipeline_id = project.create_pipeline();
        let inserted_id = 100;
        let mask_id = 101;
        let inserted = EffectNode {
            id: inserted_id,
            node_type: "test.inserted".into(),
            execution: crate::effects::NodeExecution::SpatialGpu,
            ui_position: None,
            image_inputs: BTreeMap::from([("image".into(), ImageBinding::Disconnected)]),
            stack_input: None,
            inputs: BTreeMap::new(),
            host_inputs: BTreeMap::new(),
            dynamic_image_inputs: None,
        };
        let mask = EffectNode {
            id: mask_id,
            node_type: "test.mask".into(),
            execution: crate::effects::NodeExecution::SpatialGpu,
            ui_position: None,
            image_inputs: BTreeMap::from([
                ("frame".into(), ImageBinding::Disconnected),
                ("mask".into(), ImageBinding::PipelineInput),
            ]),
            stack_input: None,
            inputs: BTreeMap::new(),
            host_inputs: BTreeMap::new(),
            dynamic_image_inputs: None,
        };
        let pipeline = project.pipeline_mut(pipeline_id).unwrap();
        pipeline.nodes.extend([inserted, mask]);
        pipeline.output = ImageBinding::Node(SocketRef {
            node: mask_id,
            output: "image".into(),
        });

        assert!(project.insert_pipeline_node_on_wire(
            pipeline_id,
            inserted_id,
            None,
            Some(mask_id),
            Some("mask"),
        ));
        let mask = project
            .pipeline(pipeline_id)
            .unwrap()
            .node(mask_id)
            .unwrap();
        assert!(matches!(
            mask.image_inputs.get("mask"),
            Some(ImageBinding::Node(socket)) if socket.node == inserted_id
        ));
        assert!(matches!(
            mask.image_inputs.get("frame"),
            Some(ImageBinding::Disconnected)
        ));
    }

    #[test]
    fn inserting_connected_node_on_wire_rejects_cycles_atomically() {
        let mut project = Project::new();
        let pipeline_id = project.create_pipeline();
        let inserted_id = 200;
        let destination_id = 201;
        let inserted = EffectNode {
            id: inserted_id,
            node_type: "test.inserted".into(),
            execution: crate::effects::NodeExecution::SpatialGpu,
            ui_position: None,
            image_inputs: BTreeMap::from([
                ("image".into(), ImageBinding::Disconnected),
                (
                    "mask".into(),
                    ImageBinding::Node(SocketRef {
                        node: destination_id,
                        output: "image".into(),
                    }),
                ),
            ]),
            stack_input: Some("image".into()),
            inputs: BTreeMap::new(),
            host_inputs: BTreeMap::new(),
            dynamic_image_inputs: None,
        };
        let destination = EffectNode {
            id: destination_id,
            node_type: "test.destination".into(),
            execution: crate::effects::NodeExecution::SpatialGpu,
            ui_position: None,
            image_inputs: BTreeMap::from([("image".into(), ImageBinding::PipelineInput)]),
            stack_input: Some("image".into()),
            inputs: BTreeMap::new(),
            host_inputs: BTreeMap::new(),
            dynamic_image_inputs: None,
        };
        project
            .pipeline_mut(pipeline_id)
            .unwrap()
            .nodes
            .extend([inserted, destination]);

        assert!(!project.insert_pipeline_node_on_wire(
            pipeline_id,
            inserted_id,
            None,
            Some(destination_id),
            Some("image"),
        ));
        let pipeline = project.pipeline(pipeline_id).unwrap();
        assert!(matches!(
            pipeline.node(destination_id).unwrap().image_inputs["image"],
            ImageBinding::PipelineInput
        ));
        assert!(matches!(
            pipeline.node(inserted_id).unwrap().image_inputs["image"],
            ImageBinding::Disconnected
        ));
    }

    #[test]
    fn reordering_stack_rejects_secondary_input_cycles_atomically() {
        let mut project = Project::new();
        let pipeline_id = project.create_pipeline();
        let first_id = 210;
        let second_id = 211;
        let first = EffectNode {
            id: first_id,
            node_type: "test.first".into(),
            execution: crate::effects::NodeExecution::SpatialGpu,
            ui_position: None,
            image_inputs: BTreeMap::from([("image".into(), ImageBinding::PipelineInput)]),
            stack_input: Some("image".into()),
            inputs: BTreeMap::new(),
            host_inputs: BTreeMap::new(),
            dynamic_image_inputs: None,
        };
        let second = EffectNode {
            id: second_id,
            node_type: "test.second".into(),
            execution: crate::effects::NodeExecution::SpatialGpu,
            ui_position: None,
            image_inputs: BTreeMap::from([
                (
                    "image".into(),
                    ImageBinding::Node(SocketRef {
                        node: first_id,
                        output: "image".into(),
                    }),
                ),
                (
                    "mask".into(),
                    ImageBinding::Node(SocketRef {
                        node: first_id,
                        output: "image".into(),
                    }),
                ),
            ]),
            stack_input: Some("image".into()),
            inputs: BTreeMap::new(),
            host_inputs: BTreeMap::new(),
            dynamic_image_inputs: None,
        };
        let pipeline = project.pipeline_mut(pipeline_id).unwrap();
        pipeline.nodes.extend([first, second]);
        pipeline.output = ImageBinding::Node(SocketRef {
            node: second_id,
            output: "image".into(),
        });

        assert!(!project.move_pipeline_node(pipeline_id, first_id, 1));
        let pipeline = project.pipeline(pipeline_id).unwrap();
        assert!(matches!(
            pipeline.node(first_id).unwrap().image_inputs["image"],
            ImageBinding::PipelineInput
        ));
        assert!(matches!(
            pipeline.node(second_id).unwrap().image_inputs["image"],
            ImageBinding::Node(SocketRef { node, .. }) if node == first_id
        ));
    }

    #[test]
    fn dynamic_image_count_is_structural_not_value_connectable() {
        let mut project = Project::new();
        let pipeline_id = project.create_pipeline();
        let value = project
            .add_value_node_at(pipeline_id, ValueNodeKind::Float, None)
            .unwrap();
        let compose_id = 999;
        project
            .pipeline_mut(pipeline_id)
            .unwrap()
            .nodes
            .push(EffectNode {
                id: compose_id,
                node_type: "test.compose".into(),
                execution: crate::effects::NodeExecution::SpatialGpu,
                ui_position: None,
                image_inputs: BTreeMap::from([
                    ("image_1".into(), ImageBinding::Disconnected),
                    ("image_2".into(), ImageBinding::Disconnected),
                ]),
                stack_input: None,
                inputs: BTreeMap::from([("count".into(), Binding::Constant(GpuValue::U32(2)))]),
                host_inputs: BTreeMap::new(),
                dynamic_image_inputs: Some(crate::effects::DynamicImageInputs {
                    count_input: "count".into(),
                    prefix: "image_".into(),
                    min: 1,
                    max: 64,
                }),
            });

        assert!(!project.connect_pipeline_value(pipeline_id, compose_id, "count", value));
        assert_eq!(
            project
                .pipeline(pipeline_id)
                .unwrap()
                .node(compose_id)
                .unwrap()
                .image_input_names(),
            vec!["image_1".to_string(), "image_2".to_string()]
        );
    }

    #[test]
    fn value_graph_rejects_cycles() {
        let mut project = Project::new();
        let pipeline = project.create_pipeline();
        let a = project
            .add_value_node_at(pipeline, ValueNodeKind::Add, None)
            .unwrap();
        let b = project
            .add_value_node_at(pipeline, ValueNodeKind::Multiply, None)
            .unwrap();
        assert!(project.connect_pipeline_value(pipeline, b, "A", a));
        assert!(!project.connect_pipeline_value(pipeline, a, "A", b));
    }

    #[test]
    fn layer_composite_missing_alpha_blend_mode_defaults_to_source_over() {
        let mut value = serde_json::to_value(LayerComposite::default()).unwrap();
        value.as_object_mut().unwrap().remove("alpha_blend_mode");
        let composite: LayerComposite = serde_json::from_value(value).unwrap();
        assert_eq!(composite.alpha_blend_mode(0.0), AlphaBlendMode::SourceOver);
    }

    #[test]
    fn host_strings_use_stepped_keyframes() {
        let mut binding = HostBinding::Constant(HostValue::String("A".into()));
        binding.toggle_keyframe(0.0);
        binding.set_value(1.0, HostValue::String("B".into()));
        assert_eq!(binding.evaluate(0.5), Some(HostValue::String("A".into())));
        assert_eq!(binding.evaluate(1.0), Some(HostValue::String("B".into())));
    }
}
