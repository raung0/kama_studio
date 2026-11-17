use std::{
    collections::HashMap,
    ffi::{CStr, CString, c_char, c_void},
    fs,
    path::{Path, PathBuf},
    ptr,
};

use anyhow::{Context, Result};
use libloading::Library;
use manguebyte_core::ParamId;
use manguebyte_host::any::Plugin;
use oxrdf::{NamedOrBlankNode, Term, Triple};
use serde::{Deserialize, Serialize};

const UI: &str = "http://lv2plug.in/ns/extensions/ui#";
const LV2: &str = "http://lv2plug.in/ns/lv2core#";
const TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ui {
    uri: String,
    binary: PathBuf,
    bundle: PathBuf,
    required_features: Vec<String>,
}

pub fn discover(bundle: &Path, plugin: &str) -> Option<Ui> {
    let mut triples = Vec::<Triple>::new();
    let mut pending = vec![bundle.to_owned()];
    let mut visited = std::collections::HashSet::new();
    while let Some(path) = pending.pop() {
        let path = path.canonicalize().ok()?;
        if !path.starts_with(bundle.canonicalize().ok()?)
            || !visited.insert(path.clone())
            || visited.len() > 512
        {
            continue;
        }
        if path.is_dir() {
            pending.extend(fs::read_dir(path).ok()?.flatten().map(|entry| entry.path()));
        } else if path.extension().is_some_and(|extension| extension == "ttl") {
            if fs::metadata(&path).ok()?.len() > 4 * 1024 * 1024 {
                continue;
            }
            let base = url::Url::from_file_path(&path).ok()?;
            let parser = oxttl::TurtleParser::new()
                .with_base_iri(base.as_str())
                .ok()?;
            triples.extend(
                parser
                    .for_reader(fs::File::open(path).ok()?)
                    .filter_map(Result::ok),
            );
        }
    }
    #[cfg(target_os = "macos")]
    let class = format!("{UI}CocoaUI");
    #[cfg(target_os = "windows")]
    let class = format!("{UI}WindowsUI");
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let class = format!("{UI}X11UI");
    for triple in &triples {
        if !subject_is(triple, plugin) || triple.predicate.as_str() != format!("{UI}ui") {
            continue;
        }
        let Term::NamedNode(uri) = &triple.object else {
            continue;
        };
        if !triples.iter().any(|triple| {
            subject_is(triple, uri.as_str())
                && triple.predicate.as_str() == TYPE
                && matches!(&triple.object, Term::NamedNode(node) if node.as_str() == class)
        }) {
            continue;
        }
        let binary = triples.iter().find_map(|triple| {
            if subject_is(triple, uri.as_str()) && matches!(triple.predicate.as_str(), predicate if predicate == format!("{UI}binary") || predicate == format!("{LV2}binary")) {
                if let Term::NamedNode(node) = &triple.object { return url::Url::parse(node.as_str()).ok()?.to_file_path().ok(); }
            }
            None
        })?;
        let required_features = triples
            .iter()
            .filter_map(|triple| {
                if subject_is(triple, uri.as_str())
                    && triple.predicate.as_str() == format!("{LV2}requiredFeature")
                {
                    if let Term::NamedNode(node) = &triple.object {
                        return Some(node.as_str().to_owned());
                    }
                }
                None
            })
            .collect();
        return Some(Ui {
            uri: uri.as_str().into(),
            binary,
            bundle: bundle.to_owned(),
            required_features,
        });
    }
    None
}

fn subject_is(triple: &Triple, uri: &str) -> bool {
    matches!(&triple.subject, NamedOrBlankNode::NamedNode(node) if node.as_str() == uri)
}

#[repr(C)]
struct Feature {
    uri: *const c_char,
    data: *mut c_void,
}

type Write = unsafe extern "C" fn(*mut c_void, u32, u32, u32, *const c_void);

#[repr(C)]
struct Descriptor {
    uri: *const c_char,
    instantiate: Option<
        unsafe extern "C" fn(
            *const Descriptor,
            *const c_char,
            *const c_char,
            Option<Write>,
            *mut c_void,
            *mut *mut c_void,
            *const *const Feature,
        ) -> *mut c_void,
    >,
    cleanup: Option<unsafe extern "C" fn(*mut c_void)>,
    port_event: Option<unsafe extern "C" fn(*mut c_void, u32, u32, u32, *const c_void)>,
    extension_data: Option<unsafe extern "C" fn(*const c_char) -> *const c_void>,
}

#[repr(C)]
struct Idle {
    idle: Option<unsafe extern "C" fn(*mut c_void) -> i32>,
}

struct Callbacks {
    writes: Vec<(u32, f32)>,
    ports: HashMap<u32, ParamId>,
}

unsafe extern "C" fn write(
    controller: *mut c_void,
    port: u32,
    size: u32,
    protocol: u32,
    buffer: *const c_void,
) {
    if controller.is_null() || buffer.is_null() || protocol != 0 || size != 4 {
        return;
    }
    // LV2 UI calls retain the boxed controller until cleanup and supply a float for protocol 0.
    let callbacks = unsafe { &mut *controller.cast::<Callbacks>() };
    let value = unsafe { ptr::read_unaligned(buffer.cast::<f32>()) };
    if value.is_finite() && callbacks.ports.contains_key(&port) && callbacks.writes.len() < 4096 {
        callbacks.writes.push((port, value));
    }
}

pub struct Editor {
    descriptor: *const Descriptor,
    handle: *mut c_void,
    callbacks: Box<Callbacks>,
    _parent: Box<Feature>,
    _features: Box<[*const Feature]>,
    _library: Library,
}

impl Editor {
    pub fn open(ui: &Ui, plugin_uri: &str, plugin: &Plugin, parent: *mut c_void) -> Result<Self> {
        anyhow::ensure!(
            ui.required_features
                .iter()
                .all(|feature| feature == &format!("{UI}parent")),
            "LV2 UI requires unsupported features: {:?}",
            ui.required_features
        );
        let plugin = plugin.as_lv2().context("not an LV2 instance")?;
        let ports = plugin
            .params()
            .iter()
            .map(|parameter| (parameter.port, parameter.id))
            .collect();
        let mut callbacks = Box::new(Callbacks {
            writes: Vec::with_capacity(4096),
            ports,
        });
        let parent_feature = Box::new(Feature {
            uri: c"http://lv2plug.in/ns/extensions/ui#parent".as_ptr(),
            data: parent,
        });
        let features = vec![&*parent_feature as *const Feature, ptr::null()].into_boxed_slice();
        // The UI binary is untrusted native code, just like the DSP binary. It is confined to the audio worker.
        let library = unsafe { Library::new(&ui.binary) }?;
        let entry = unsafe {
            library.get::<unsafe extern "C" fn(u32) -> *const Descriptor>(b"lv2ui_descriptor\0")
        }?;
        let mut descriptor = ptr::null();
        for index in 0..1024 {
            let candidate = unsafe { entry(index) };
            if candidate.is_null() {
                break;
            }
            let uri = unsafe { (*candidate).uri };
            if !uri.is_null() && unsafe { CStr::from_ptr(uri) }.to_bytes() == ui.uri.as_bytes() {
                descriptor = candidate;
                break;
            }
        }
        anyhow::ensure!(!descriptor.is_null(), "LV2 UI descriptor not found");
        let instantiate =
            unsafe { (*descriptor).instantiate }.context("LV2 UI has no instantiate callback")?;
        let uri = CString::new(plugin_uri)?;
        let bundle = CString::new(format!(
            "{}{sep}",
            ui.bundle.display(),
            sep = std::path::MAIN_SEPARATOR
        ))?;
        let mut widget = ptr::null_mut();
        let handle = unsafe {
            instantiate(
                descriptor,
                uri.as_ptr(),
                bundle.as_ptr(),
                Some(write),
                (&mut *callbacks as *mut Callbacks).cast(),
                &mut widget,
                features.as_ptr(),
            )
        };
        anyhow::ensure!(!handle.is_null(), "LV2 UI refused to instantiate");
        Ok(Self {
            descriptor,
            handle,
            callbacks,
            _parent: parent_feature,
            _features: features,
            _library: library,
        })
    }

    pub fn pump(&mut self, plugin: &mut Plugin) -> Result<bool> {
        let changed = !self.callbacks.writes.is_empty();
        for (port, value) in self.callbacks.writes.drain(..) {
            if let Some(id) = self.callbacks.ports.get(&port) {
                plugin.set_param(*id, f64::from(value))?;
            }
        }
        if let Some(port_event) = unsafe { (*self.descriptor).port_event } {
            for (port, id) in &self.callbacks.ports {
                if let Some(value) = plugin.param_value(*id) {
                    let value = value as f32;
                    unsafe { port_event(self.handle, *port, 4, 0, (&raw const value).cast()) };
                }
            }
        }
        if let Some(extension) = unsafe { (*self.descriptor).extension_data } {
            let idle =
                unsafe { extension(c"http://lv2plug.in/ns/extensions/ui#idleInterface".as_ptr()) }
                    .cast::<Idle>();
            if !idle.is_null() {
                if let Some(idle) = unsafe { (*idle).idle } {
                    unsafe { idle(self.handle) };
                }
            }
        }
        Ok(changed)
    }
}

impl Drop for Editor {
    fn drop(&mut self) {
        if let Some(cleanup) = unsafe { (*self.descriptor).cleanup } {
            unsafe { cleanup(self.handle) };
        }
    }
}
