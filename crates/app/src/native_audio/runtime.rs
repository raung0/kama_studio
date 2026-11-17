use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap},
    path::Path,
    sync::{Arc, Mutex, Weak, mpsc},
    time::Duration,
};

use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use manguebyte_core::{AudioBuffer, ParamId};
use manguebyte_dsp::{
    ProcessContext, ProcessSpec, Processor, Transport,
    event::{Event, EventBuffer, EventKind},
};
use manguebyte_host::{
    any::{AnyProcessor, Kind, Library, Plugin},
    editor::{EditorRequest, ParentWindow},
};
use vst::{
    host::{Host, HostBuffer, PluginInstance, PluginLoader},
    plugin::Plugin as VstPlugin,
};
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    platform::pump_events::EventLoopExtPumpEvents,
    raw_window_handle::{HasWindowHandle, RawWindowHandle},
    window::{Window, WindowId},
};

use super::{Descriptor, Format, Parameter, Update};
use crate::{messages, project::HostValue, runtime::wasm::plugin_parameter_hash};

thread_local! {
    static CONTROLLERS: RefCell<Vec<Controller>> = const { RefCell::new(Vec::new()) };
    static UPDATES: RefCell<Vec<Update>> = const { RefCell::new(Vec::new()) };
    static WINDOWS: RefCell<Option<EventLoop<()>>> = const { RefCell::new(None) };
    static OPEN: RefCell<Vec<(u64, u64)>> = const { RefCell::new(Vec::new()) };
    static PREVIEWS: RefCell<Vec<(u64, u64, NativeProcessor)>> = const { RefCell::new(Vec::new()) };
}

#[derive(Default)]
struct VstHost;
impl Host for VstHost {}

fn vst2(path: &Path) -> Result<PluginInstance> {
    let binary = if path.is_dir() {
        std::fs::read_dir(path.join("Contents/MacOS"))?
            .flatten()
            .map(|entry| entry.path())
            .find(|path| path.is_file())
            .context("VST2 bundle has no executable")?
    } else {
        path.to_owned()
    };
    let mut loader =
        PluginLoader::load(&binary, Arc::new(Mutex::new(VstHost))).context("load VST2 binary")?;
    let mut plugin = loader.instance().context("instantiate VST2")?;
    plugin.init();
    Ok(plugin)
}

pub(super) fn inspect(path: &Path, format: Format) -> Result<Vec<Descriptor>> {
    if format == Format::Vst2 {
        let mut plugin = vst2(path)?;
        let info = plugin.get_info();
        if info.inputs <= 0
            || info.outputs <= 0
            || matches!(info.category, vst::plugin::Category::Synth)
        {
            return Ok(Vec::new());
        }
        let parameters = plugin.get_parameter_object();
        return Ok(vec![Descriptor {
            format,
            path: path.to_owned(),
            id: info.unique_id.to_string(),
            name: info.name,
            vendor: info.vendor,
            has_editor: plugin.get_editor().is_some(),
            lv2_editor: None,
            parameters: (0..info.parameters)
                .map(|index| Parameter {
                    id: index as u32,
                    name: parameters.get_parameter_name(index),
                    min: 0.0,
                    max: 1.0,
                    default: f64::from(parameters.get_parameter(index)),
                    step: None,
                    unit: String::new(),
                })
                .collect(),
        }]);
    }
    // Native code is loaded only in disposable scanners and processing workers.
    let library = unsafe { Library::load(path) }.context("load native plugin bundle")?;
    let mut output = Vec::new();
    for entry in library.plugins() {
        if matches!(entry.kind, Kind::Instrument | Kind::NoteEffect) {
            continue;
        }
        let plugin = library.create(&entry.id)?;
        if !plugin.audio_inputs().iter().any(|bus| bus.channels > 0)
            || !plugin.audio_outputs().iter().any(|bus| bus.channels > 0)
        {
            continue;
        }
        let lv2_editor = if format == Format::Lv2 {
            super::lv2_editor::discover(path, &entry.id)
        } else {
            None
        };
        let has_editor = lv2_editor.is_some()
            || plugin.as_clap().is_some_and(|plugin| plugin.has_editor())
            || plugin.as_vst3().is_some_and(|plugin| plugin.has_editor());
        output.push(Descriptor {
            format,
            path: path.to_owned(),
            id: entry.id.clone(),
            name: entry.name.clone(),
            vendor: entry.vendor.clone(),
            has_editor,
            lv2_editor,
            parameters: plugin
                .params()
                .into_iter()
                .filter(|parameter| !parameter.hidden && !parameter.read_only)
                .filter(|parameter| {
                    parameter.min.is_finite()
                        && parameter.max.is_finite()
                        && parameter.default.is_finite()
                        && parameter.min <= parameter.max
                })
                .map(|parameter| Parameter {
                    id: parameter.id.0,
                    name: parameter.name,
                    min: parameter.min,
                    max: parameter.max,
                    default: parameter.default,
                    unit: parameter.unit,
                    step: parameter
                        .steps
                        .filter(|steps| *steps > 0)
                        .map(|steps| (parameter.max - parameter.min) / f64::from(steps)),
                })
                .collect(),
        });
    }
    Ok(output)
}

enum MainPlugin {
    Modern(Plugin),
    Vst2 {
        plugin: Arc<Mutex<PluginInstance>>,
        editor: Option<Box<dyn vst::editor::Editor>>,
    },
}

struct Controller {
    plugin: MainPlugin,
    descriptor: Descriptor,
    pipeline: u64,
    node: u64,
    lease: Weak<()>,
    idle_audio: Arc<Mutex<Option<AudioPlugin>>>,
    spec: ProcessSpec,
    window: Option<Window>,
    lv2_editor: Option<super::lv2_editor::Editor>,
    last_values: BTreeMap<String, f32>,
    host_values: HashMap<u32, f64>,
    last_state: Option<String>,
    host_state: Option<String>,
    dirty: bool,
    retired: bool,
    ticks: u32,
    flush: Option<mpsc::SyncSender<()>>,
    flush_thread: Option<std::thread::JoinHandle<()>>,
}

impl Controller {
    fn stop_flush(&mut self) {
        self.flush = None;
        if let Some(thread) = self.flush_thread.take() {
            let _ = thread.join();
        }
    }

    fn reconfigure_audio(&mut self, old: AudioPlugin, spec: ProcessSpec) -> Result<AudioPlugin> {
        self.stop_flush();
        drop(old);
        let audio = match &mut self.plugin {
            MainPlugin::Modern(plugin) => {
                plugin.deactivate()?;
                let mut audio = plugin.activate_processor(spec.sample_rate, spec.max_block_size)?;
                audio.prepare(&spec);
                let audio = Arc::new(Mutex::new(audio));
                if self.descriptor.format == Format::Clap {
                    let (flush, thread) = start_flush_thread(&audio)?;
                    self.flush = Some(flush);
                    self.flush_thread = Some(thread);
                }
                AudioPlugin::Modern(audio)
            }
            MainPlugin::Vst2 { plugin, .. } => {
                let mut instance = plugin
                    .lock()
                    .map_err(|_| anyhow::anyhow!("VST2 instance poisoned"))?;
                let info = instance.get_info();
                instance.suspend();
                instance.set_sample_rate(spec.sample_rate as f32);
                instance.set_block_size(i64::try_from(spec.max_block_size)?);
                instance.resume();
                drop(instance);
                AudioPlugin::Vst2 {
                    plugin: Arc::clone(plugin),
                    buffer: VstBuffer(HostBuffer::from_info(&info)),
                    inputs: vec![vec![0.0; spec.max_block_size]; usize::try_from(info.inputs)?],
                    outputs: vec![vec![0.0; spec.max_block_size]; usize::try_from(info.outputs)?],
                }
            }
        };
        self.spec = spec;
        Ok(audio)
    }

    fn close(&mut self) {
        self.lv2_editor = None;
        match &mut self.plugin {
            MainPlugin::Modern(plugin) => {
                if let Some(plugin) = plugin.as_clap_mut() {
                    plugin.close_editor();
                }
                if let Some(plugin) = plugin.as_vst3_mut() {
                    plugin.close_editor();
                }
            }
            MainPlugin::Vst2 { editor, .. } => {
                if let Some(mut editor) = editor.take() {
                    editor.close();
                }
            }
        }
        self.window = None;
    }

    fn open(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        if let Some(window) = &self.window {
            window.focus_window();
            return Ok(());
        }
        let window = event_loop.create_window(
            Window::default_attributes()
                .with_title(self.descriptor.title())
                .with_inner_size(LogicalSize::new(800, 600)),
        )?;
        let (parent, raw) = parent(&window)?;
        let size = match &mut self.plugin {
            MainPlugin::Modern(plugin) => {
                if let Some(plugin) = plugin.as_clap_mut() {
                    let size = plugin.open_editor(parent, Some(window.scale_factor()))?;
                    Some((size.width, size.height))
                } else if let Some(plugin) = plugin.as_vst3_mut() {
                    let size = plugin.open_editor(parent, Some(window.scale_factor()))?;
                    Some((size.width, size.height))
                } else {
                    let ui = self
                        .descriptor
                        .lv2_editor
                        .as_ref()
                        .context("plugin has no supported native editor")?;
                    self.lv2_editor = Some(super::lv2_editor::Editor::open(
                        ui,
                        &self.descriptor.id,
                        plugin,
                        raw,
                    )?);
                    None
                }
            }
            MainPlugin::Vst2 { plugin, editor } => {
                let mut plugin = plugin
                    .lock()
                    .map_err(|_| anyhow::anyhow!("VST2 instance poisoned"))?;
                let mut view = plugin.get_editor().context("plugin has no editor")?;
                anyhow::ensure!(view.open(raw), "VST2 editor refused to open");
                let (width, height) = view.size();
                *editor = Some(view);
                Some((width.max(1) as u32, height.max(1) as u32))
            }
        };
        if let Some((width, height)) = size {
            let _ = window.request_inner_size(LogicalSize::new(width, height));
        }
        self.window = Some(window);
        Ok(())
    }

    fn tick(&mut self) {
        if self.retired {
            return;
        }
        if let (Some(editor), MainPlugin::Modern(plugin)) = (&mut self.lv2_editor, &mut self.plugin)
        {
            match editor.pump(plugin) {
                Ok(changed) => self.dirty |= changed,
                Err(error) => messages::error("LV2 editor", format!("{error:#}")),
            }
        }
        let mut editor_requests = Vec::new();
        match &mut self.plugin {
            MainPlugin::Modern(plugin) => {
                if let Some(plugin) = plugin.as_clap_mut() {
                    use manguebyte_host::clap::HostEvent;
                    for notice in plugin.pump() {
                        match notice {
                            HostEvent::Editor(request) => editor_requests.push(request),
                            HostEvent::ProcessFailed(failure) => messages::error(
                                "Audio plugin",
                                format!(
                                    "{} failed during processing: {failure:?}",
                                    self.descriptor.title()
                                ),
                            ),
                            HostEvent::ParamValue { .. }
                            | HostEvent::StateDirty
                            | HostEvent::ParamsRescanned(_) => self.dirty = true,
                            HostEvent::FlushRequested => {
                                if let Some(flush) = &self.flush {
                                    let _ = flush.try_send(());
                                }
                            }
                            HostEvent::RestartRequested => messages::warning(
                                "Native plugin",
                                format!(
                                    "{} requested a restart; toggle the effect off and on to reconfigure it",
                                    self.descriptor.title()
                                ),
                            ),
                            HostEvent::Log { message, .. } => {
                                messages::warning(
                                    "Audio plugin",
                                    format!("{}: {message}", self.descriptor.title()),
                                );
                            }
                            _ => {}
                        }
                    }
                } else if let Some(plugin) = plugin.as_vst3_mut() {
                    use manguebyte_host::vst3::HostEvent;
                    for notice in plugin.pump() {
                        match notice {
                            HostEvent::Editor(request) => editor_requests.push(request),
                            HostEvent::ProcessFailed(failure) => messages::error(
                                "Audio plugin",
                                format!(
                                    "{} failed during processing: {failure:?}",
                                    self.descriptor.title()
                                ),
                            ),
                            _ => self.dirty = true,
                        }
                    }
                } else {
                    for notice in plugin.pump() {
                        match notice {
                            manguebyte_host::any::Notice::ProcessFailed(failure) => {
                                messages::error(
                                    "Audio plugin",
                                    format!(
                                        "{} failed during processing: {failure:?}",
                                        self.descriptor.title()
                                    ),
                                )
                            }
                            manguebyte_host::any::Notice::Message(message) => messages::warning(
                                "Audio plugin",
                                format!("{}: {message}", self.descriptor.title()),
                            ),
                            _ => self.dirty = true,
                        }
                    }
                }
            }
            MainPlugin::Vst2 { editor, .. } => {
                if let Some(editor) = editor {
                    editor.idle();
                    self.dirty = true;
                }
            }
        }
        for request in editor_requests {
            match request {
                EditorRequest::Resize(size) => {
                    if let Some(window) = &self.window {
                        let _ =
                            window.request_inner_size(LogicalSize::new(size.width, size.height));
                    }
                }
                EditorRequest::Show => {
                    if let Some(window) = &self.window {
                        window.set_visible(true);
                    }
                }
                EditorRequest::Hide => {
                    if let Some(window) = &self.window {
                        window.set_visible(false);
                    }
                }
                EditorRequest::Closed => self.window = None,
                _ => {}
            }
        }
        self.ticks = self.ticks.wrapping_add(1);
        if self.ticks % 10 != 0 || !self.dirty {
            return;
        }
        let values: BTreeMap<_, _> = self
            .descriptor
            .parameters
            .iter()
            .filter_map(|parameter| {
                let value = match &self.plugin {
                    MainPlugin::Modern(plugin) => plugin
                        .param_value(ParamId(parameter.id))
                        .map(|value| value as f32),
                    MainPlugin::Vst2 { plugin, .. } => plugin.try_lock().ok().map(|mut plugin| {
                        plugin
                            .get_parameter_object()
                            .get_parameter(parameter.id as i32)
                    }),
                }?;
                value.is_finite().then(|| (parameter.input_id(), value))
            })
            .collect();
        let changed = values
            .iter()
            .filter(|(key, value)| self.last_values.get(*key) != Some(*value))
            .map(|(key, value)| (key.clone(), *value))
            .collect::<BTreeMap<_, _>>();
        self.last_values = values;
        let state = if self.dirty {
            let result = match &mut self.plugin {
                MainPlugin::Modern(plugin) => plugin.save_state().ok(),
                MainPlugin::Vst2 { plugin, .. } => plugin
                    .try_lock()
                    .ok()
                    .map(|mut plugin| plugin.get_parameter_object().get_preset_data()),
            };
            self.dirty = false;
            result
                .map(|bytes| STANDARD.encode(bytes))
                .filter(|state| self.last_state.as_ref() != Some(state))
        } else {
            None
        };
        if state.is_some() {
            self.last_state.clone_from(&state);
        }
        if !changed.is_empty() || state.is_some() {
            UPDATES.with_borrow_mut(|updates| {
                updates.push(Update {
                    pipeline: self.pipeline,
                    node: self.node,
                    parameters: changed,
                    state,
                })
            });
        }
    }
}

impl Drop for Controller {
    fn drop(&mut self) {
        self.close();
        self.stop_flush();
    }
}

fn start_flush_thread(
    audio: &Arc<Mutex<AnyProcessor>>,
) -> Result<(mpsc::SyncSender<()>, std::thread::JoinHandle<()>)> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let processor = Arc::clone(audio);
    let thread = std::thread::Builder::new()
        .name("clap-parameter-flush".into())
        .spawn(move || {
            while receiver.recv().is_ok() {
                if let Ok(mut processor) = processor.lock() {
                    if let Some(processor) = processor.as_clap_mut() {
                        processor.flush();
                    }
                }
            }
        })?;
    Ok((sender, thread))
}

enum AudioPlugin {
    Modern(Arc<Mutex<AnyProcessor>>),
    Vst2 {
        plugin: Arc<Mutex<PluginInstance>>,
        buffer: VstBuffer,
        inputs: Vec<Vec<f32>>,
        outputs: Vec<Vec<f32>>,
    },
}

struct VstBuffer(HostBuffer<f32>);

// HostBuffer stores pointer scratch space, not borrowed audio. Every use below rebinds
// every pointer to buffers exclusively owned by the receiving audio thread.
unsafe impl Send for VstBuffer {}

pub struct NativeProcessor {
    plugin: Option<AudioPlugin>,
    idle_audio: Arc<Mutex<Option<AudioPlugin>>>,
    buffer: AudioBuffer<f32>,
    events: EventBuffer,
    parameters: Vec<(u32, u32, f64, f64, f64)>,
    transport: Transport,
    sample_rate: f64,
    last_host_time: Option<f64>,
    _lease: Arc<()>,
}

impl NativeProcessor {
    pub fn new(
        descriptor: &Descriptor,
        pipeline: u64,
        node: u64,
        state: Option<&str>,
        parameters: &HashMap<u32, HostValue>,
        channels: usize,
        sample_rate: u32,
        frames: usize,
        preview: bool,
    ) -> Result<Self> {
        let spec = ProcessSpec {
            sample_rate: f64::from(sample_rate),
            max_block_size: frames,
            num_channels: channels,
        };
        if !preview {
            let removed = PREVIEWS.with_borrow_mut(|previews| {
                let index = previews
                    .iter()
                    .position(|(owner, id, _)| *owner == pipeline && *id == node)?;
                Some(previews.remove(index))
            });
            drop(removed);
        }
        let reused = CONTROLLERS
            .with_borrow_mut(|controllers| {
                let controller = controllers.iter_mut().find(|controller| {
                    !controller.retired
                        && controller.pipeline == pipeline
                        && controller.node == node
                        && controller.lease.strong_count() == 0
                        && controller.descriptor == *descriptor
                })?;
                let audio = controller
                    .idle_audio
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()?;
                let audio = if controller.spec == spec {
                    audio
                } else {
                    match controller.reconfigure_audio(audio, spec) {
                        Ok(audio) => audio,
                        Err(error) => return Some(Err(error)),
                    }
                };
                let lease = Arc::new(());
                controller.lease = Arc::downgrade(&lease);
                Some(Ok((audio, Arc::clone(&controller.idle_audio), lease)))
            })
            .transpose()?;
        if let Some((audio, idle_audio, lease)) = reused {
            return Ok(Self::from_audio(descriptor, spec, audio, idle_audio, lease));
        }
        let lease = Arc::new(());
        let idle_audio = Arc::new(Mutex::new(None));
        let mut flush = None;
        let mut flush_thread = None;
        let (main, audio) = if descriptor.format == Format::Vst2 {
            let mut plugin = vst2(&descriptor.path)?;
            let info = plugin.get_info();
            anyhow::ensure!(
                info.inputs > 0 && info.outputs > 0 && info.inputs <= 64 && info.outputs <= 64,
                "unsupported VST2 channel layout"
            );
            if let Some(state) = state {
                plugin
                    .get_parameter_object()
                    .load_preset_data(&STANDARD.decode(state)?);
            }
            for parameter in &descriptor.parameters {
                if let Some(value) = parameter_value(parameters, parameter) {
                    plugin
                        .get_parameter_object()
                        .set_parameter(parameter.id as i32, value as f32);
                }
            }
            plugin.set_sample_rate(sample_rate as f32);
            plugin.set_block_size(frames as i64);
            plugin.resume();
            let buffer = VstBuffer(HostBuffer::from_info(&info));
            let plugin = Arc::new(Mutex::new(plugin));
            (
                MainPlugin::Vst2 {
                    plugin: Arc::clone(&plugin),
                    editor: None,
                },
                AudioPlugin::Vst2 {
                    plugin,
                    buffer,
                    inputs: vec![vec![0.0; frames]; info.inputs as usize],
                    outputs: vec![vec![0.0; frames]; info.outputs as usize],
                },
            )
        } else {
            // The library and controller remain on their creating thread; only the audio half moves.
            let library = unsafe { Library::load(&descriptor.path) }?;
            let mut plugin = library.create(&descriptor.id)?;
            if let Some(state) = state {
                plugin.load_state(&STANDARD.decode(state)?)?;
            }
            for parameter in &descriptor.parameters {
                if let Some(value) = parameter_value(parameters, parameter) {
                    plugin.set_param(ParamId(parameter.id), value)?;
                }
            }
            let mut audio = plugin.activate_processor(f64::from(sample_rate), frames)?;
            audio.prepare(&ProcessSpec {
                sample_rate: f64::from(sample_rate),
                max_block_size: frames,
                num_channels: channels,
            });
            let audio = Arc::new(Mutex::new(audio));
            if descriptor.format == Format::Clap {
                let (sender, thread) = start_flush_thread(&audio)?;
                flush_thread = Some(thread);
                flush = Some(sender);
            }
            (MainPlugin::Modern(plugin), AudioPlugin::Modern(audio))
        };
        CONTROLLERS.with_borrow_mut(|controllers| {
            controllers.push(Controller {
                plugin: main,
                descriptor: descriptor.clone(),
                pipeline,
                node,
                lease: Arc::downgrade(&lease),
                idle_audio: Arc::clone(&idle_audio),
                spec,
                window: None,
                lv2_editor: None,
                last_values: descriptor
                    .parameters
                    .iter()
                    .map(|parameter| (parameter.input_id(), parameter.default as f32))
                    .collect(),
                host_values: descriptor
                    .parameters
                    .iter()
                    .filter_map(|parameter| {
                        Some((parameter.id, parameter_value(parameters, parameter)?))
                    })
                    .collect(),
                last_state: state.map(str::to_owned),
                host_state: state.map(str::to_owned),
                dirty: false,
                retired: false,
                ticks: 0,
                flush,
                flush_thread,
            })
        });
        Ok(Self::from_audio(descriptor, spec, audio, idle_audio, lease))
    }

    fn from_audio(
        descriptor: &Descriptor,
        spec: ProcessSpec,
        audio: AudioPlugin,
        idle_audio: Arc<Mutex<Option<AudioPlugin>>>,
        lease: Arc<()>,
    ) -> Self {
        Self {
            plugin: Some(audio),
            idle_audio,
            buffer: AudioBuffer::new(spec.num_channels, spec.max_block_size),
            events: EventBuffer::with_capacity(descriptor.parameters.len()),
            parameters: descriptor
                .parameters
                .iter()
                .map(|parameter| {
                    (
                        parameter.id,
                        plugin_parameter_hash(&parameter.input_id()),
                        parameter.min,
                        parameter.max,
                        f64::NAN,
                    )
                })
                .collect(),
            transport: Transport {
                playing: true,
                ..Transport::STOPPED
            },
            sample_rate: spec.sample_rate,
            last_host_time: None,
            _lease: lease,
        }
    }

    pub fn process(
        &mut self,
        samples: &mut [f32],
        channels: usize,
        parameters: &HashMap<u32, HostValue>,
    ) -> Result<()> {
        anyhow::ensure!(
            channels > 0 && samples.len() % channels == 0,
            "invalid native audio buffer"
        );
        let frames = samples.len() / channels;
        anyhow::ensure!(
            frames <= self.buffer.capacity(),
            "native audio block exceeds capacity"
        );
        self.buffer.set_num_frames(frames);
        self.buffer.copy_from_interleaved(samples);
        self.events.clear();
        for (id, hash, min, max, previous) in &mut self.parameters {
            let Some(HostValue::Gpu(value)) = parameters.get(hash) else {
                continue;
            };
            let Some(value) = value.numeric(None).filter(|value| value.is_finite()) else {
                continue;
            };
            let value = value.clamp(*min, *max);
            if value != *previous {
                let _ = self.events.push(Event {
                    offset: 0,
                    kind: EventKind::ParamValue {
                        param: ParamId(*id),
                        value,
                    },
                });
            }
        }
        match self
            .plugin
            .as_mut()
            .context("native audio processor unavailable")?
        {
            AudioPlugin::Modern(plugin) => {
                let Ok(mut plugin) = plugin.try_lock() else {
                    return Ok(());
                };
                plugin.process_with(
                    &mut self.buffer,
                    &ProcessContext::new(&self.events, &self.transport),
                );
            }
            AudioPlugin::Vst2 {
                plugin,
                buffer,
                inputs,
                outputs,
            } => {
                let Ok(mut plugin) = plugin.try_lock() else {
                    return Ok(());
                };
                let parameters = plugin.get_parameter_object();
                for event in &self.events {
                    if let EventKind::ParamValue { param, value } = event.kind {
                        parameters.set_parameter(param.0 as i32, value as f32);
                    }
                }
                for (index, input) in inputs.iter_mut().enumerate() {
                    let source = index.min(channels.saturating_sub(1));
                    input
                        .get_mut(..frames)
                        .context("VST2 input capacity")?
                        .copy_from_slice(self.buffer.channel(source));
                }
                let mut input_slices: [&[f32]; 64] = [&[]; 64];
                for (slice, input) in input_slices.iter_mut().zip(inputs.iter()) {
                    *slice = input.get(..frames).context("VST2 input capacity")?;
                }
                let output_count = outputs.len();
                let mut output_slices: [&mut [f32]; 64] = std::array::from_fn(|_| &mut [][..]);
                for (slice, output) in output_slices.iter_mut().zip(outputs.iter_mut()) {
                    *slice = output.get_mut(..frames).context("VST2 output capacity")?;
                }
                let mut block = buffer.0.bind(
                    input_slices
                        .get(..inputs.len())
                        .context("VST2 channel limit")?,
                    output_slices
                        .get_mut(..output_count)
                        .context("VST2 channel limit")?,
                );
                plugin.process(&mut block);
                for channel in 0..channels {
                    let output = outputs
                        .get(channel.min(outputs.len().saturating_sub(1)))
                        .context("VST2 missing output")?;
                    self.buffer
                        .channel_mut(channel)
                        .copy_from_slice(output.get(..frames).context("VST2 output capacity")?);
                }
            }
        }
        for (_, hash, min, max, previous) in &mut self.parameters {
            if let Some(HostValue::Gpu(value)) = parameters.get(hash) {
                if let Some(value) = value.numeric(None).filter(|value| value.is_finite()) {
                    *previous = value.clamp(*min, *max);
                }
            }
        }
        self.buffer.copy_to_interleaved(samples);
        self.transport.advance(frames, self.sample_rate);
        Ok(())
    }

    pub fn set_timeline_time(&mut self, time: f64) {
        if time.is_finite() && self.last_host_time != Some(time) {
            self.transport.position_seconds = Some(time);
            self.transport.position_samples = Some((time * self.sample_rate).round() as i64);
            self.last_host_time = Some(time);
        }
    }

    pub fn reset(&mut self) {
        self.last_host_time = None;
        if let Some(AudioPlugin::Modern(plugin)) = &mut self.plugin {
            if let Ok(mut plugin) = plugin.try_lock() {
                plugin.reset();
            }
        }
        for (_, _, _, _, previous) in &mut self.parameters {
            *previous = f64::NAN;
        }
    }
}

impl Drop for NativeProcessor {
    fn drop(&mut self) {
        *self
            .idle_audio
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = self.plugin.take();
    }
}

fn parent(window: &Window) -> Result<(ParentWindow, *mut std::ffi::c_void)> {
    let raw = window.window_handle()?.as_raw();
    let (parent, pointer) = match raw {
        RawWindowHandle::AppKit(handle) => (
            ParentWindow::cocoa(handle.ns_view.as_ptr() as usize),
            handle.ns_view.as_ptr(),
        ),
        RawWindowHandle::Win32(handle) => (
            ParentWindow::win32(handle.hwnd.get() as usize),
            handle.hwnd.get() as *mut _,
        ),
        RawWindowHandle::Xlib(handle) => (
            ParentWindow::x11(u64::from(handle.window)),
            handle.window as *mut _,
        ),
        RawWindowHandle::Xcb(handle) => (
            ParentWindow::x11(u64::from(handle.window.get())),
            handle.window.get() as usize as *mut _,
        ),
        _ => anyhow::bail!("plugin editor requires Cocoa, Win32, or X11 (use XWayland on Linux)"),
    };
    Ok((parent.context("invalid native window handle")?, pointer))
}

#[derive(Default)]
struct WindowHandler;
impl ApplicationHandler for WindowHandler {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.about_to_wait(event_loop);
    }
    fn window_event(&mut self, _: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if matches!(event, WindowEvent::CloseRequested) {
            let mut closed = Vec::new();
            CONTROLLERS.with_borrow_mut(|controllers| {
                for controller in controllers {
                    if controller
                        .window
                        .as_ref()
                        .is_some_and(|window| window.id() == id)
                    {
                        controller.ticks = 9;
                        controller.dirty = true;
                        controller.tick();
                        controller.close();
                        closed.push((controller.pipeline, controller.node));
                    }
                }
            });
            PREVIEWS.with_borrow_mut(|previews| {
                previews.retain(|(pipeline, node, _)| !closed.contains(&(*pipeline, *node)))
            });
        }
    }
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let requests = OPEN.with_borrow_mut(std::mem::take);
        CONTROLLERS.with_borrow_mut(|controllers| {
            for (pipeline, node) in requests {
                if let Some(controller) = controllers.iter_mut().find(|controller| !controller.retired && controller.pipeline == pipeline && controller.node == node && controller.lease.strong_count() > 0) {
                    if let Err(error) = controller.open(event_loop) { messages::error("Plugin editor", format!("{}: {error:#}", controller.descriptor.title())); }
                } else { messages::warning("Plugin editor", "No active plugin instance. Position the playhead over a clip using this effect."); }
            }
        });
    }
}

pub fn open_editor(
    pipeline: u64,
    node: u64,
    descriptor: &Descriptor,
    state: Option<&str>,
    parameters: &HashMap<u32, HostValue>,
) -> Result<()> {
    let exists = CONTROLLERS.with_borrow(|controllers| {
        controllers.iter().any(|controller| {
            !controller.retired
                && controller.pipeline == pipeline
                && controller.node == node
                && controller.lease.strong_count() > 0
        })
    });
    if !exists {
        let processor = NativeProcessor::new(
            descriptor, pipeline, node, state, parameters, 2, 48_000, 256, true,
        )?;
        PREVIEWS.with_borrow_mut(|previews| previews.push((pipeline, node, processor)));
    }
    OPEN.with_borrow_mut(|requests| requests.push((pipeline, node)));
    Ok(())
}

pub fn run_event_loop() {
    let needed = OPEN.with_borrow(|requests| !requests.is_empty());
    WINDOWS.with_borrow_mut(|slot| {
        if slot.is_none() && needed {
            let mut builder = EventLoop::builder();
            #[cfg(target_os = "linux")]
            {
                use winit::platform::x11::EventLoopBuilderExtX11;
                builder.with_x11();
            }
            match builder.build() {
                Ok(event_loop) => *slot = Some(event_loop),
                Err(error) => {
                    messages::error("Plugin editor", error.to_string());
                    OPEN.with_borrow_mut(Vec::clear);
                }
            }
        }
        if let Some(event_loop) = slot {
            event_loop.pump_app_events(Some(Duration::ZERO), &mut WindowHandler);
        }
    });
}

pub fn pump() {
    run_event_loop();
    CONTROLLERS.with_borrow_mut(|controllers| {
        for controller in controllers.iter_mut() {
            if controller.lease.strong_count() == 0 {
                controller.ticks = 9;
            }
            controller.tick();
        }
        controllers.retain(|controller| {
            controller.lease.strong_count() > 0 || controller.window.is_some()
        });
    });
}

pub fn drain_updates() -> Vec<Update> {
    UPDATES.with_borrow_mut(std::mem::take)
}

fn parameter_value(parameters: &HashMap<u32, HostValue>, parameter: &Parameter) -> Option<f64> {
    let HostValue::Gpu(value) = parameters.get(&plugin_parameter_hash(&parameter.input_id()))?
    else {
        return None;
    };
    value
        .numeric(None)
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(parameter.min, parameter.max))
}

pub fn synchronize(
    pipeline: u64,
    node: u64,
    state: Option<&str>,
    parameters: &HashMap<u32, HostValue>,
) {
    CONTROLLERS.with_borrow_mut(|controllers| {
        for controller in controllers.iter_mut().filter(|controller| {
            !controller.retired && controller.pipeline == pipeline && controller.node == node
        }) {
            if let Some(state) = state.filter(|state| {
                controller.host_state.as_deref() != Some(*state)
                    && controller.last_state.as_deref() != Some(*state)
            }) {
                let result = STANDARD
                    .decode(state)
                    .map_err(anyhow::Error::from)
                    .and_then(|bytes| match &mut controller.plugin {
                        MainPlugin::Modern(plugin) => {
                            plugin.load_state(&bytes).map_err(anyhow::Error::from)
                        }
                        MainPlugin::Vst2 { plugin, .. } => {
                            let mut plugin = plugin
                                .lock()
                                .map_err(|_| anyhow::anyhow!("VST2 instance poisoned"))?;
                            plugin.get_parameter_object().load_preset_data(&bytes);
                            Ok(())
                        }
                    });
                match result {
                    Ok(()) => {
                        controller.last_state = Some(state.into());
                        controller.host_values.clear();
                    }
                    Err(error) => messages::error("Plugin state", format!("{error:#}")),
                }
            }
            controller.host_state = state.map(str::to_owned);
            for parameter in &controller.descriptor.parameters {
                let Some(value) = parameter_value(parameters, parameter) else {
                    continue;
                };
                if controller.host_values.get(&parameter.id) == Some(&value) {
                    continue;
                }
                let result = match &mut controller.plugin {
                    MainPlugin::Modern(plugin) => plugin
                        .set_param(ParamId(parameter.id), value)
                        .map_err(anyhow::Error::from),
                    MainPlugin::Vst2 { plugin, .. } => plugin
                        .lock()
                        .map_err(|_| anyhow::anyhow!("VST2 instance poisoned"))
                        .map(|mut plugin| {
                            plugin
                                .get_parameter_object()
                                .set_parameter(parameter.id as i32, value as f32)
                        }),
                };
                if result.is_ok() {
                    controller.host_values.insert(parameter.id, value);
                    controller
                        .last_values
                        .insert(parameter.input_id(), value as f32);
                    if let Some(flush) = &controller.flush {
                        let _ = flush.try_send(());
                    }
                } else if let Err(error) = result {
                    messages::error(
                        "Audio plugin",
                        format!(
                            "{} failed to set {}: {error:#}",
                            controller.descriptor.title(),
                            parameter.name
                        ),
                    );
                }
            }
        }
    });
}

pub fn clear() {
    PREVIEWS.with_borrow_mut(Vec::clear);
    OPEN.with_borrow_mut(Vec::clear);
    UPDATES.with_borrow_mut(Vec::clear);
    CONTROLLERS.with_borrow_mut(|controllers| {
        for controller in controllers.iter_mut() {
            controller.close();
            controller.retired = true;
        }
        controllers.retain(|controller| controller.lease.strong_count() > 0);
    });
}

pub fn retain_project(project: &crate::project::Project) {
    let removed = CONTROLLERS.with_borrow_mut(|controllers| {
        let mut removed = Vec::new();
        for controller in controllers.iter_mut() {
            let present = project
                .pipeline(controller.pipeline)
                .and_then(|pipeline| pipeline.node(controller.node))
                .is_some_and(|node| node.node_type == controller.descriptor.key());
            if !present {
                controller.close();
                controller.retired = true;
                removed.push((controller.pipeline, controller.node));
            }
        }
        removed
    });
    PREVIEWS.with_borrow_mut(|previews| {
        previews.retain(|(pipeline, node, _)| !removed.contains(&(*pipeline, *node)))
    });
    OPEN.with_borrow_mut(|requests| requests.retain(|request| !removed.contains(request)));
    UPDATES.with_borrow_mut(|updates| {
        updates.retain(|update| !removed.contains(&(update.pipeline, update.node)))
    });
    CONTROLLERS.with_borrow_mut(|controllers| {
        controllers.retain(|controller| {
            controller.lease.strong_count() > 0
                || (!controller.retired && controller.window.is_some())
        })
    });
}
