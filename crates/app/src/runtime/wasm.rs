use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::{bail, Context, Result};
use cosmic_text::{
    Align as CosmicAlign, Attrs, Buffer, Family, FontSystem, Metrics, Shaping, SwashCache,
    SwashContent, Wrap,
};
use wasmtime::{
    Caller, Config, Engine, Extern, Linker, Memory, Module, Store, StoreLimits, StoreLimitsBuilder,
    Strategy, TypedFunc,
};

use crate::{
    effects::GpuValue,
    embedded_vfs,
    project::{HostBinding, HostValue},
    runtime::video::CpuFrame,
};

pub const DEFAULT_RENDER_EXPORT: &str = "kama_render";

const BASE_FUEL_PER_RENDER: u64 = 16_000_000;
const FUEL_PER_OUTPUT_PIXEL: u64 = 768;
const MAX_FUEL_PER_RENDER: u64 = 2_000_000_000;
const MEMORY_LIMIT_BYTES: usize = 256 * 1024 * 1024;

fn render_fuel_budget(width: u32, height: u32) -> u64 {
    let pixels = u64::from(width.max(1)).saturating_mul(u64::from(height.max(1)));
    BASE_FUEL_PER_RENDER
        .saturating_add(pixels.saturating_mul(FUEL_PER_OUTPUT_PIXEL))
        .min(MAX_FUEL_PER_RENDER)
}

fn rgba32f_lengths(width: u32, height: u32) -> Option<(usize, usize)> {
    let values = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)?;
    let bytes = values.checked_mul(std::mem::size_of::<f32>())?;
    (bytes <= MEMORY_LIMIT_BYTES).then_some((values, bytes))
}



pub struct WasmRenderRequest<'a> {
    pub module_path: &'a Path,
    pub entry: &'a str,
    pub parameters: &'a BTreeMap<String, HostBinding>,
    pub size: [u32; 2],
    pub render_scale: f32,
    pub render_origin: [f32; 2],
    pub tight_bounds: bool,
    pub parameter_time: f64,
    pub local_time: f64,
}

pub struct WasmRuntime {
    engine: Engine,
    modules: HashMap<PathBuf, Arc<Module>>,
    text: Arc<Mutex<TextHost>>,
}

#[derive(Clone, Debug)]
pub struct WasmMonitorHandle {
    pub target: u32,
    pub element: i32,
    pub position: [f32; 2],
    pub origin: [f32; 2],
}

#[derive(Clone, Debug, Default)]
pub struct WasmMonitorOverlay {
    pub handles: Vec<WasmMonitorHandle>,
    pub lines: Vec<[usize; 2]>,
}

struct PluginStore {
    limits: StoreLimits,
    parameters: HashMap<u32, HostValue>,
    render_scale: f32,
    render_origin: [f32; 2],
    tight_bounds: bool,
    text: Arc<Mutex<TextHost>>,
}

struct TextHost {
    font_system: FontSystem,
    swash_cache: SwashCache,
}

impl Default for TextHost {
    fn default() -> Self {
        Self {
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
        }
    }
}

impl WasmRuntime {
    pub fn new() -> Result<Self> {
        let mut config = Config::new();
        config.strategy(Strategy::Cranelift);
        config.consume_fuel(true);
        let engine = Engine::new(&config).context("create Wasmtime engine")?;
        Ok(Self {
            engine,
            modules: HashMap::new(),
            text: Arc::new(Mutex::new(TextHost::default())),
        })
    }

    pub fn clear(&mut self) {
        self.modules.clear();
    }

    pub fn measure_text(
        &mut self,
        text: &str,
        font_family: Option<&str>,
        font_size: f32,
        render_scale: f32,
    ) -> [u32; 2] {
        self.text
            .lock()
            .map(|mut host| {
                host.measure(
                    text,
                    font_family,
                    font_size.max(1.0) * render_scale.max(0.000_001),
                )
            })
            .unwrap_or([1, 1])
    }

    pub fn precompile(&mut self, path: &Path) -> Result<()> {
        let _ = self.module(path)?;
        Ok(())
    }

    
    
    
    
    
    
    
    
    
    pub fn render(&mut self, request: WasmRenderRequest<'_>) -> Result<CpuFrame> {
        let WasmRenderRequest {
            module_path,
            entry,
            parameters,
            size: [width, height],
            render_scale,
            render_origin,
            tight_bounds,
            parameter_time,
            local_time,
        } = request;
        let module = self.module(module_path)?;
        let resolved: BTreeMap<_, _> = parameters
            .iter()
            .filter_map(|(name, binding)| {
                binding
                    .evaluate(parameter_time)
                    .map(|value| (name.clone(), value))
            })
            .collect();
        let params = serde_json::to_vec(&resolved).context("serialize CPU plugin parameters")?;
        let hashed_parameters = resolved
            .into_iter()
            .map(|(name, value)| (plugin_parameter_hash(&name), value))
            .collect();
        let width_i32 = i32::try_from(width).context("CPU plugin frame width is too large")?;
        let height_i32 = i32::try_from(height).context("CPU plugin frame height is too large")?;
        let (value_count, byte_count) =
            rgba32f_lengths(width, height).context("CPU plugin frame is too large")?;
        let limits = StoreLimitsBuilder::new()
            .memory_size(MEMORY_LIMIT_BYTES)
            .instances(1)
            .memories(2)
            .tables(8)
            .build();
        let mut store = Store::new(
            &self.engine,
            PluginStore {
                limits,
                parameters: hashed_parameters,
                render_scale: render_scale.clamp(0.000_001, 1.0),
                render_origin,
                tight_bounds,
                text: Arc::clone(&self.text),
            },
        );
        store.limiter(|state| &mut state.limits);
        store
            .set_fuel(render_fuel_budget(width, height))
            .context("set WASM render fuel budget")?;

        let mut linker = Linker::new(&self.engine);
        install_host_abi(&mut linker)?;
        let instance = linker
            .instantiate(&mut store, &module)
            .with_context(|| format!("instantiate {}", module_path.display()))?;
        let memory = instance
            .get_memory(&mut store, "memory")
            .context("CPU plugin must export memory as `memory`")?;
        let alloc = instance
            .get_typed_func::<i32, i32>(&mut store, "kama_alloc")
            .context("CPU plugin must export kama_alloc(i32) -> i32")?;
        let render = instance
            .get_typed_func::<(i32, i32, i32, i32, f64), i32>(&mut store, entry)
            .with_context(|| format!("CPU plugin missing render export `{entry}`"))?;

        let params_len =
            i32::try_from(params.len()).context("CPU plugin parameter block too large")?;
        let params_ptr = alloc
            .call(&mut store, params_len)
            .context("CPU plugin kama_alloc failed")?;
        if params_ptr < 0 {
            bail!("CPU plugin kama_alloc returned a negative pointer");
        }
        memory
            .write(&mut store, params_ptr as usize, &params)
            .context("write CPU plugin parameters")?;

        let output_ptr = render
            .call(
                &mut store,
                (params_ptr, params_len, width_i32, height_i32, local_time),
            )
            .with_context(|| format!("CPU plugin `{entry}` trapped"))?;
        if output_ptr < 0 {
            bail!("CPU plugin returned a negative frame pointer");
        }

        let mut bytes = vec![0u8; byte_count];
        memory
            .read(&store, output_ptr as usize, &mut bytes)
            .context("CPU plugin returned an out-of-bounds frame")?;
        let mut pixels = Vec::with_capacity(value_count);
        for bytes in bytes.chunks_exact(4) {
            let value = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            pixels.push(if value.is_finite() { value } else { 0.0 });
        }

        if let Ok(dealloc) = instance.get_typed_func::<(i32, i32), ()>(&mut store, "kama_dealloc") {
            let _ = dealloc.call(&mut store, (params_ptr, params_len));
        }

        Ok(CpuFrame::from_pixels(width, height, pixels))
    }

    pub fn monitor_overlay(
        &mut self,
        module_path: &Path,
        entry: &str,
        parameters: HashMap<u32, HostValue>,
        size: [f32; 2],
        time: f64,
    ) -> Result<WasmMonitorOverlay> {
        const HEADER_BYTES: usize = 12;
        const HANDLE_BYTES: usize = 24;
        const LINE_BYTES: usize = 8;
        const MAX_OVERLAY_BYTES: usize = 1024 * 1024;

        let module = self.module(module_path)?;
        let limits = StoreLimitsBuilder::new()
            .memory_size(MEMORY_LIMIT_BYTES)
            .instances(1)
            .memories(2)
            .tables(8)
            .build();
        let mut store = Store::new(
            &self.engine,
            PluginStore {
                limits,
                parameters,
                render_scale: 1.0,
                render_origin: [0.0; 2],
                tight_bounds: false,
                text: Arc::clone(&self.text),
            },
        );
        store.limiter(|state| &mut state.limits);
        store
            .set_fuel(BASE_FUEL_PER_RENDER)
            .context("set WASM monitor fuel budget")?;
        let mut linker = Linker::new(&self.engine);
        install_host_abi(&mut linker)?;
        let instance = linker
            .instantiate(&mut store, &module)
            .with_context(|| format!("instantiate monitor module {}", module_path.display()))?;
        let memory = instance
            .get_memory(&mut store, "memory")
            .context("monitor plugin must export memory")?;
        let overlay = instance
            .get_typed_func::<(f32, f32, f64), i64>(&mut store, entry)
            .with_context(|| format!("monitor plugin missing export `{entry}`"))?;
        let descriptor = overlay
            .call(&mut store, (size[0], size[1], time))
            .with_context(|| format!("monitor plugin `{entry}` trapped"))?
            as u64;
        let pointer = descriptor as u32 as usize;
        let byte_len = (descriptor >> 32) as usize;
        if !(HEADER_BYTES..=MAX_OVERLAY_BYTES).contains(&byte_len) {
            bail!("monitor plugin returned invalid buffer length {byte_len}");
        }
        let mut bytes = vec![0u8; byte_len];
        memory
            .read(&store, pointer, &mut bytes)
            .context("monitor plugin returned an out-of-bounds buffer")?;
        let u32_at = |offset: usize| {
            u32::from_le_bytes(
                bytes[offset..offset + 4]
                    .try_into()
                    .expect("checked record"),
            )
        };
        if u32_at(0) != 1 {
            bail!("monitor plugin returned an unsupported overlay version");
        }
        let handle_count = u32_at(4) as usize;
        let line_count = u32_at(8) as usize;
        let expected = HEADER_BYTES
            .checked_add(
                handle_count
                    .checked_mul(HANDLE_BYTES)
                    .context("monitor handle overflow")?,
            )
            .and_then(|value| value.checked_add(line_count.checked_mul(LINE_BYTES)?))
            .context("monitor overlay size overflow")?;
        if expected != byte_len {
            bail!("monitor plugin returned a malformed overlay buffer");
        }
        let mut handles = Vec::with_capacity(handle_count);
        for index in 0..handle_count {
            let offset = HEADER_BYTES + index * HANDLE_BYTES;
            let f32_at = |field: usize| f32::from_bits(u32_at(offset + field * 4));
            let position = [f32_at(2), f32_at(3)];
            let origin = [f32_at(4), f32_at(5)];
            if !position
                .iter()
                .chain(origin.iter())
                .all(|value| value.is_finite())
            {
                bail!("monitor plugin returned non-finite handle geometry");
            }
            handles.push(WasmMonitorHandle {
                target: u32_at(offset),
                element: u32_at(offset + 4) as i32,
                position,
                origin,
            });
        }
        let lines_offset = HEADER_BYTES + handle_count * HANDLE_BYTES;
        let mut lines = Vec::with_capacity(line_count);
        for index in 0..line_count {
            let offset = lines_offset + index * LINE_BYTES;
            let line = [u32_at(offset) as usize, u32_at(offset + 4) as usize];
            if line[0] >= handles.len() || line[1] >= handles.len() {
                bail!("monitor plugin line references an unknown handle");
            }
            lines.push(line);
        }
        if let Ok(dealloc) = instance.get_typed_func::<(i32, i32), ()>(&mut store, "kama_dealloc") {
            let _ = dealloc.call(&mut store, (pointer as i32, byte_len as i32));
        }
        Ok(WasmMonitorOverlay { handles, lines })
    }

    fn module(&mut self, path: &Path) -> Result<Arc<Module>> {
        if let Some(module) = self.modules.get(path) {
            return Ok(module.clone());
        }
        let module = Arc::new(
            if let Some(bytes) = embedded_vfs::read(path)? {
                Module::new(&self.engine, bytes)
            } else {
                Module::from_file(&self.engine, path)
            }
            .with_context(|| format!("compile CPU plugin {}", path.display()))?,
        );
        self.modules.insert(path.to_path_buf(), module.clone());
        Ok(module)
    }
}

const AUDIO_MEMORY_LIMIT_BYTES: usize = 32 * 1024 * 1024;
const AUDIO_INIT_FUEL: u64 = 16_000_000;
const AUDIO_BASE_FUEL: u64 = 1_000_000;
const AUDIO_FUEL_PER_SAMPLE: u64 = 2_048;

struct AudioPluginStore {
    limits: StoreLimits,
    parameters: Arc<HashMap<u32, HostValue>>,
}

pub struct AudioWasmRuntime {
    engine: Engine,
    modules: HashMap<PathBuf, Arc<Module>>,
}

pub struct AudioWasmProcessor {
    store: Store<AudioPluginStore>,
    memory: Memory,
    process: TypedFunc<(i32, i32, i32, i32, i32), i32>,
    buffer_ptr: i32,
    sample_capacity: usize,
}

impl AudioWasmRuntime {
    pub fn new() -> Result<Self> {
        let mut config = Config::new();
        config.strategy(Strategy::Cranelift);
        config.consume_fuel(true);
        Ok(Self {
            engine: Engine::new(&config).context("create audio Wasmtime engine")?,
            modules: HashMap::new(),
        })
    }

    pub fn clear(&mut self) {
        self.modules.clear();
    }

    pub fn processor(
        &mut self,
        module_path: &Path,
        entry: &str,
        sample_capacity: usize,
    ) -> Result<AudioWasmProcessor> {
        let module = if let Some(module) = self.modules.get(module_path) {
            Arc::clone(module)
        } else {
            let module = Arc::new(
                if let Some(bytes) = embedded_vfs::read(module_path)? {
                    Module::new(&self.engine, bytes)
                } else {
                    Module::from_file(&self.engine, module_path)
                }
                .with_context(|| format!("compile audio plugin {}", module_path.display()))?,
            );
            self.modules
                .insert(module_path.to_path_buf(), Arc::clone(&module));
            module
        };
        let limits = StoreLimitsBuilder::new()
            .memory_size(AUDIO_MEMORY_LIMIT_BYTES)
            .instances(1)
            .memories(1)
            .tables(2)
            .build();
        let mut store = Store::new(
            &self.engine,
            AudioPluginStore {
                limits,
                parameters: Arc::new(HashMap::new()),
            },
        );
        store.limiter(|state| &mut state.limits);
        store
            .set_fuel(AUDIO_INIT_FUEL)
            .context("set audio WASM initialization fuel budget")?;
        let mut linker = Linker::new(&self.engine);
        install_audio_host_abi(&mut linker)?;
        let instance = linker
            .instantiate(&mut store, &module)
            .with_context(|| format!("instantiate audio plugin {}", module_path.display()))?;
        let memory = instance
            .get_memory(&mut store, "memory")
            .context("audio plugin must export memory as `memory`")?;
        let alloc = instance
            .get_typed_func::<i32, i32>(&mut store, "kama_alloc")
            .context("audio plugin must export kama_alloc(i32) -> i32")?;
        let process = instance
            .get_typed_func::<(i32, i32, i32, i32, i32), i32>(&mut store, entry)
            .with_context(|| format!("audio plugin missing process export `{entry}`"))?;
        let byte_capacity = sample_capacity
            .checked_mul(std::mem::size_of::<f32>())
            .context("audio plugin block buffer is too large")?;
        let byte_capacity =
            i32::try_from(byte_capacity).context("audio plugin block exceeds i32")?;
        let buffer_ptr = alloc
            .call(&mut store, byte_capacity)
            .context("audio plugin kama_alloc failed")?;
        if buffer_ptr <= 0 {
            bail!("audio plugin kama_alloc returned an invalid pointer");
        }
        Ok(AudioWasmProcessor {
            store,
            memory,
            process,
            buffer_ptr,
            sample_capacity,
        })
    }
}

impl AudioWasmProcessor {
    pub fn process(
        &mut self,
        samples: &mut [f32],
        channels: usize,
        sample_rate: u32,
        parameters: &Arc<HashMap<u32, HostValue>>,
        reset: bool,
    ) -> Result<()> {
        if channels == 0
            || samples.len() > self.sample_capacity
            || !samples.len().is_multiple_of(channels)
        {
            bail!("invalid audio plugin block shape");
        }
        self.store.data_mut().parameters = Arc::clone(parameters);
        self.store
            .set_fuel(
                AUDIO_BASE_FUEL
                    .saturating_add((samples.len() as u64).saturating_mul(AUDIO_FUEL_PER_SAMPLE)),
            )
            .context("set audio WASM fuel budget")?;
        if !samples.is_empty() {
            self.memory
                .write(
                    &mut self.store,
                    self.buffer_ptr as usize,
                    bytemuck::cast_slice(samples),
                )
                .context("write audio plugin block")?;
        }
        let status = self
            .process
            .call(
                &mut self.store,
                (
                    self.buffer_ptr,
                    i32::try_from(samples.len() / channels)
                        .context("audio frame count overflow")?,
                    i32::try_from(channels).context("audio channel count overflow")?,
                    i32::try_from(sample_rate).context("audio sample rate overflow")?,
                    if reset { 1 } else { 0 },
                ),
            )
            .context("audio plugin trapped")?;
        if status != 0 {
            bail!("audio plugin returned status {status}");
        }
        if !samples.is_empty() {
            self.memory
                .read(
                    &self.store,
                    self.buffer_ptr as usize,
                    bytemuck::cast_slice_mut(samples),
                )
                .context("read audio plugin block")?;
        }
        Ok(())
    }

    pub fn reset(
        &mut self,
        channels: usize,
        sample_rate: u32,
        parameters: &Arc<HashMap<u32, HostValue>>,
    ) -> Result<()> {
        self.process(&mut [], channels, sample_rate, parameters, true)
    }
}

macro_rules! install_parameter_host_abi {
    ($linker:expr, $store:ty) => {{
        $linker.func_wrap(
            "kama",
            "param_f32",
            |caller: Caller<'_, $store>, hash: i64, component: i32, fallback: f32| -> f32 {
                caller
                    .data()
                    .parameters
                    .get(&(hash as u32))
                    .and_then(|value| host_component(value, component.max(0) as usize))
                    .unwrap_or(fallback)
            },
        )?;
        $linker.func_wrap(
            "kama",
            "param_u32",
            |caller: Caller<'_, $store>, hash: i64, fallback: i32| -> i32 {
                caller
                    .data()
                    .parameters
                    .get(&(hash as u32))
                    .and_then(|value| match value {
                        HostValue::Gpu(GpuValue::U32(value) | GpuValue::Enum(value)) => {
                            Some(*value as i32)
                        }
                        HostValue::Gpu(GpuValue::I32(value)) => Some(*value),
                        HostValue::Gpu(GpuValue::Bool(value)) => Some(i32::from(*value)),
                        HostValue::Gpu(GpuValue::F32(value)) => Some(value.round() as i32),
                        _ => None,
                    })
                    .unwrap_or(fallback)
            },
        )?;
        $linker.func_wrap(
            "kama",
            "param_f32_list_len",
            |caller: Caller<'_, $store>, hash: i64| -> i32 {
                caller
                    .data()
                    .parameters
                    .get(&(hash as u32))
                    .and_then(|value| match value {
                        HostValue::F32List(values) => i32::try_from(values.len()).ok(),
                        _ => None,
                    })
                    .unwrap_or(0)
            },
        )?;
        $linker.func_wrap(
            "kama",
            "param_f32_list_get",
            |caller: Caller<'_, $store>, hash: i64, index: i32, fallback: f32| -> f32 {
                usize::try_from(index)
                    .ok()
                    .and_then(|index| {
                        caller
                            .data()
                            .parameters
                            .get(&(hash as u32))
                            .and_then(|value| match value {
                                HostValue::F32List(values) => values.get(index).copied(),
                                _ => None,
                            })
                    })
                    .unwrap_or(fallback)
            },
        )?;
    }};
}

fn install_audio_host_abi(linker: &mut Linker<AudioPluginStore>) -> Result<()> {
    install_parameter_host_abi!(linker, AudioPluginStore);
    Ok(())
}

fn gpu_component(value: &GpuValue, component: usize) -> Option<f32> {
    match value {
        GpuValue::F32(value) => (component == 0).then_some(*value),
        GpuValue::I32(value) => (component == 0).then_some(*value as f32),
        GpuValue::U32(value) | GpuValue::Enum(value) => (component == 0).then_some(*value as f32),
        GpuValue::Bool(value) => (component == 0).then_some(if *value { 1.0 } else { 0.0 }),
        GpuValue::Vec2(value) => value.get(component).copied(),
        GpuValue::Vec3(value) => value.get(component).copied(),
        GpuValue::Vec4(value) | GpuValue::Color(value) => value.get(component).copied(),
    }
}

fn install_host_abi(linker: &mut Linker<PluginStore>) -> Result<()> {
    install_parameter_host_abi!(linker, PluginStore);
    linker.func_wrap(
        "kama",
        "param_vec2_array_len",
        |caller: Caller<'_, PluginStore>, hash: i64| -> i32 {
            let hash = hash as u32;
            caller
                .data()
                .parameters
                .get(&hash)
                .and_then(|value| match value {
                    HostValue::Vec2Array(points) => i32::try_from(points.len()).ok(),
                    _ => None,
                })
                .unwrap_or(0)
        },
    )?;
    linker.func_wrap(
        "kama",
        "param_vec2_array_get",
        |caller: Caller<'_, PluginStore>,
         hash: i64,
         index: i32,
         component: i32,
         fallback: f32|
         -> f32 {
            if index < 0 || !(0..=1).contains(&component) {
                return fallback;
            }
            let hash = hash as u32;
            caller
                .data()
                .parameters
                .get(&hash)
                .and_then(|value| match value {
                    HostValue::Vec2Array(points) => points
                        .get(index as usize)
                        .map(|point| point[component as usize]),
                    _ => None,
                })
                .unwrap_or(fallback)
        },
    )?;
    linker.func_wrap(
        "kama",
        "render_scale",
        |caller: Caller<'_, PluginStore>| -> f32 { caller.data().render_scale },
    )?;
    linker.func_wrap(
        "kama",
        "render_origin",
        |caller: Caller<'_, PluginStore>, component: i32| -> f32 {
            caller
                .data()
                .render_origin
                .get(component.max(0) as usize)
                .copied()
                .unwrap_or(0.0)
        },
    )?;
    linker.func_wrap(
        "kama",
        "render_tight_bounds",
        |caller: Caller<'_, PluginStore>| -> i32 { i32::from(caller.data().tight_bounds) },
    )?;
    linker.func_wrap(
        "kama",
        "render_text_rgba32f",
        |mut caller: Caller<'_, PluginStore>, output_ptr: i32, width: i32, height: i32| -> i32 {
            if output_ptr < 0 || width <= 0 || height <= 0 {
                return -1;
            }
            let Some((value_count, byte_len)) = rgba32f_lengths(width as u32, height as u32) else {
                return -4;
            };
            let text =
                host_string(host_parameter(&caller.data().parameters, "text")).unwrap_or_default();
            let family = host_string(host_parameter(&caller.data().parameters, "font_family"))
                .filter(|family| !family.is_empty());
            let font_size = caller
                .data()
                .parameters
                .get(&plugin_parameter_hash("font_size"))
                .and_then(|value| host_component(value, 0))
                .unwrap_or(72.0)
                .max(1.0)
                * caller.data().render_scale.max(0.000_001);
            let color = host_color(host_parameter(&caller.data().parameters, "color"));
            let text_host = Arc::clone(&caller.data().text);
            let frame = match text_host.lock() {
                Ok(mut host) => host.render(
                    &text,
                    family.as_deref(),
                    font_size,
                    color,
                    width as u32,
                    height as u32,
                ),
                Err(_) => return -2,
            };
            if frame.pixels.len() != value_count {
                return -4;
            }
            let Some(Extern::Memory(memory)) = caller.get_export("memory") else {
                return -3;
            };
            let start = output_ptr as usize;
            let end = match start.checked_add(byte_len) {
                Some(end) => end,
                None => return -4,
            };
            let data = memory.data_mut(&mut caller);
            if end > data.len() {
                return -5;
            }
            for (index, value) in frame.pixels.iter().enumerate() {
                let offset = start + index * 4;
                data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            }
            0
        },
    )?;
    Ok(())
}

pub(crate) fn plugin_parameter_hash(value: &str) -> u32 {
    value.bytes().fold(0x811c9dc5u32, |hash, byte| {
        (hash ^ byte as u32).wrapping_mul(0x01000193)
    })
}

fn host_parameter<'a>(
    parameters: &'a HashMap<u32, HostValue>,
    name: &str,
) -> Option<&'a HostValue> {
    parameters.get(&plugin_parameter_hash(name))
}

fn host_component(value: &HostValue, component: usize) -> Option<f32> {
    match value {
        HostValue::Gpu(value) => gpu_component(value, component),
        _ => None,
    }
}

fn host_color(value: Option<&HostValue>) -> [f32; 4] {
    match value {
        Some(HostValue::Gpu(GpuValue::Color(value) | GpuValue::Vec4(value))) => *value,
        _ => [1.0, 1.0, 1.0, 1.0],
    }
}

fn host_string(value: Option<&HostValue>) -> Option<String> {
    match value {
        Some(HostValue::String(value)) => Some(value.clone()),
        _ => None,
    }
}

impl TextHost {
    fn measure(&mut self, text: &str, font_family: Option<&str>, font_size: f32) -> [u32; 2] {
        let mut buffer = Buffer::new(
            &mut self.font_system,
            Metrics::new(font_size, font_size * 1.2),
        );
        let mut borrowed = buffer.borrow_with(&mut self.font_system);
        borrowed.set_size(None, None);
        borrowed.set_wrap(Wrap::None);
        let attrs = font_family
            .map(|family| Attrs::new().family(Family::Name(family)))
            .unwrap_or_else(Attrs::new);
        borrowed.set_text(text, &attrs, Shaping::Advanced, None);
        let mut width = 0.0f32;
        let mut height = 0.0f32;
        for run in borrowed.layout_runs() {
            width = width.max(run.line_w);
            height = height.max(run.line_top + run.line_height);
        }
        
        [
            width.ceil().max(1.0) as u32 + 4,
            height.ceil().max(1.0) as u32 + 4,
        ]
    }

    fn render(
        &mut self,
        text: &str,
        font_family: Option<&str>,
        font_size: f32,
        color: [f32; 4],
        width: u32,
        height: u32,
    ) -> CpuFrame {
        let mut frame = CpuFrame::transparent(width, height);
        let mut buffer = Buffer::new(
            &mut self.font_system,
            Metrics::new(font_size, font_size * 1.2),
        );
        let glyphs = {
            let mut borrowed = buffer.borrow_with(&mut self.font_system);
            borrowed.set_size(Some(width as f32), Some(height as f32));
            borrowed.set_wrap(Wrap::None);
            let attrs = font_family
                .map(|family| Attrs::new().family(Family::Name(family)))
                .unwrap_or_else(Attrs::new);
            borrowed.set_text(text, &attrs, Shaping::Advanced, Some(CosmicAlign::Center));
            let runs: Vec<_> = borrowed.layout_runs().collect();
            let content_height = runs
                .last()
                .map_or(0.0, |run| run.line_top + run.line_height);
            let y_shift = (height as f32 - content_height).max(0.0) * 0.5;
            let mut glyphs = Vec::new();
            for run in runs {
                let origin = (0.0, run.line_y + y_shift);
                for glyph in run.glyphs.iter() {
                    glyphs.push(glyph.physical(origin, 1.0));
                }
            }
            glyphs
        };

        for glyph in glyphs {
            let Some(image) = self
                .swash_cache
                .get_image(&mut self.font_system, glyph.cache_key)
                .as_ref()
            else {
                continue;
            };
            let mask = swash_to_rgba_mask(
                &image.content,
                &image.data,
                image.placement.width,
                image.placement.height,
            );
            let left = glyph.x + image.placement.left;
            let top = glyph.y - image.placement.top;
            for y in 0..image.placement.height as i32 {
                let py = top + y;
                if py < 0 || py >= height as i32 {
                    continue;
                }
                for x in 0..image.placement.width as i32 {
                    let px = left + x;
                    if px < 0 || px >= width as i32 {
                        continue;
                    }
                    let index = ((y as u32 * image.placement.width + x as u32) * 4) as usize;
                    let glyph_rgba = [
                        mask[index] as f32 / 255.0,
                        mask[index + 1] as f32 / 255.0,
                        mask[index + 2] as f32 / 255.0,
                        mask[index + 3] as f32 / 255.0,
                    ];
                    let alpha = (glyph_rgba[3] * color[3]).clamp(0.0, 1.0);
                    let rgb = if matches!(&image.content, SwashContent::Color) {
                        [
                            srgb_to_linear(glyph_rgba[0]) * color[0],
                            srgb_to_linear(glyph_rgba[1]) * color[1],
                            srgb_to_linear(glyph_rgba[2]) * color[2],
                        ]
                    } else {
                        [color[0], color[1], color[2]]
                    };
                    let src = [rgb[0] * alpha, rgb[1] * alpha, rgb[2] * alpha, alpha];
                    let dst = frame.rgba(px as u32, py as u32);
                    frame.set_rgba(px as u32, py as u32, alpha_over(dst, src));
                }
            }
        }
        frame
    }
}

fn swash_to_rgba_mask(content: &SwashContent, data: &[u8], width: u32, height: u32) -> Vec<u8> {
    let count = width as usize * height as usize;
    match content {
        SwashContent::Mask => data
            .iter()
            .take(count)
            .flat_map(|alpha| [255, 255, 255, *alpha])
            .collect(),
        SwashContent::SubpixelMask => data
            .chunks_exact(4)
            .take(count)
            .flat_map(|rgba| [255, 255, 255, rgba[0].max(rgba[1]).max(rgba[2])])
            .collect(),
        SwashContent::Color => data.to_vec(),
        #[allow(unreachable_patterns)]
        _ => vec![0; count * 4],
    }
}

fn alpha_over(dst: [f32; 4], src: [f32; 4]) -> [f32; 4] {
    let inverse = 1.0 - src[3].clamp(0.0, 1.0);
    [
        src[0] + dst[0] * inverse,
        src[1] + dst[1] * inverse,
        src[2] + dst[2] * inverse,
        src[3] + dst[3] * inverse,
    ]
}

fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod monitor_tests {
    use super::*;
    use crate::plugin::PluginRegistry;

    #[test]
    fn builtin_wasm_owns_monitor_geometry_and_topology() {
        let registry = PluginRegistry::load_default("").unwrap();
        let definition = registry.effect("builtin.mesh_warp").unwrap();
        let monitor = definition.monitor.as_ref().unwrap();
        let node = definition.instantiate(7).unwrap();
        let parameters = node
            .inputs
            .iter()
            .filter_map(|(name, binding)| {
                binding
                    .evaluate(0.0)
                    .map(|value| (plugin_parameter_hash(name), HostValue::Gpu(value)))
            })
            .collect();
        let mut runtime = WasmRuntime::new().unwrap();
        let overlay = runtime
            .monitor_overlay(
                &monitor.module,
                &monitor.entry,
                parameters,
                [1920.0, 1080.0],
                0.0,
            )
            .unwrap();
        assert_eq!(overlay.handles.len(), 8);
        assert_eq!(overlay.lines.len(), 8);
        assert_eq!(overlay.handles[0].target, plugin_parameter_hash("top_left"));
        assert_eq!(overlay.handles[0].position, [0.0, 0.0]);
        assert_eq!(overlay.handles[4].position, [1920.0, 1080.0]);

        let gradient = registry.generator("builtin.gradient").unwrap();
        let parameters = gradient
            .instantiate_parameters()
            .unwrap()
            .into_iter()
            .filter_map(|(name, binding)| {
                binding
                    .evaluate(0.0)
                    .map(|value| (plugin_parameter_hash(&name), value))
            })
            .collect();
        let overlay = runtime
            .monitor_overlay(
                gradient.module.as_ref().unwrap(),
                gradient.monitor_entry.as_deref().unwrap(),
                parameters,
                [1920.0, 1080.0],
                0.0,
            )
            .unwrap();
        assert_eq!(overlay.handles.len(), 2);
        assert_eq!(overlay.lines, [[0, 1]]);
        assert_eq!(overlay.handles[0].element, 0);
        assert_eq!(overlay.handles[1].element, 1);
        assert_eq!(overlay.handles[0].target, plugin_parameter_hash("points"));

        let shape = registry.generator("builtin.shape").unwrap();
        let parameters = shape
            .instantiate_parameters()
            .unwrap()
            .into_iter()
            .filter_map(|(name, binding)| {
                binding
                    .evaluate(0.0)
                    .map(|value| (plugin_parameter_hash(&name), value))
            })
            .collect();
        let overlay = runtime
            .monitor_overlay(
                shape.monitor_module.as_ref().unwrap(),
                shape.monitor_entry.as_deref().unwrap(),
                parameters,
                [960.0, 540.0],
                0.0,
            )
            .unwrap();
        assert_eq!(overlay.handles.len(), 4);
        assert_eq!(overlay.lines.len(), 4);
        assert!(overlay
            .handles
            .iter()
            .all(|handle| handle.target == plugin_parameter_hash("size")));
    }
}
