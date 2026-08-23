use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use resvg::{
    tiny_skia::{Pixmap, Transform},
    usvg::{Options, Tree},
};

const ICON_SIZE: u32 = 80;
fn variant(path: &Path) -> String {
    path.file_stem()
        .unwrap()
        .to_str()
        .unwrap()
        .split('_')
        .map(|word| {
            let mut chars = word.chars();
            chars
                .next()
                .unwrap()
                .to_uppercase()
                .chain(chars)
                .collect::<String>()
        })
        .collect()
}

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap() == "windows" {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("icon.ico");
        res.compile().unwrap();
    }

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    println!("cargo:rerun-if-changed=assets");
    embed_builtin_plugin(&out_dir);
    let mut icons = fs::read_dir("assets")
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "svg"))
        .collect::<Vec<_>>();
    icons.sort();
    let icon_bytes = ICON_SIZE as usize * ICON_SIZE as usize * 4;
    let mut atlas = Vec::with_capacity(icon_bytes * icons.len());
    let variants = icons
        .iter()
        .map(|path| variant(path))
        .collect::<Vec<_>>()
        .join(",");
    fs::write(
        out_dir.join("app_icons.rs"),
        format!(
            "#[allow(dead_code)] #[repr(usize)] #[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum AppIcon {{ {variants} }} impl AppIcon {{ const COUNT: usize = {}; }}",
            icons.len()
        ),
    )
    .unwrap();

    for path in icons {
        let data = fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let options = Options::default();
        let tree = Tree::from_data(&data, &options)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let size = tree.size();
        let transform = Transform::from_scale(
            ICON_SIZE as f32 / size.width(),
            ICON_SIZE as f32 / size.height(),
        );
        let mut pixmap = Pixmap::new(ICON_SIZE, ICON_SIZE).unwrap();
        resvg::render(&tree, transform, &mut pixmap.as_mut());
        for pixel in pixmap.data_mut().chunks_exact_mut(4) {
            let alpha = pixel[3] as u32;
            if alpha == 0 {
                continue;
            }
            for channel in &mut pixel[..3] {
                *channel = ((*channel as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
        atlas.extend_from_slice(pixmap.data());
    }

    fs::write(out_dir.join("icons.rgba"), atlas).unwrap();
    export_app_version();
}

fn export_app_version() {
    let workspace = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../..");
    let version_file = workspace.join("VERSION");
    println!("cargo:rerun-if-changed={}", version_file.display());

    let version = fs::read_to_string(&version_file)
        .ok()
        .map(|version| version.trim().to_owned())
        .filter(|version| !version.is_empty())
        .unwrap_or_else(|| "dev".to_owned());

    println!("cargo:rustc-env=APP_VERSION={version}");
}

fn embed_builtin_plugin(out_dir: &Path) {
    let source = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("../..")
        .join("builtins");
    emit_rerun_if_changed(&source);

    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let modules = [
        (
            "generators",
            "kama_builtin_generators.wasm",
            "generators.wasm",
        ),
        ("audio", "kama_builtin_audio.wasm", "audio.wasm"),
    ];
    let embedded_root = out_dir.join("embedded-plugins");
    let embedded = embedded_root.join("builtins");
    if embedded_root.exists() {
        fs::remove_dir_all(&embedded_root).unwrap_or_else(|error| {
            panic!(
                "remove stale embedded plugin directory {}: {error}",
                embedded_root.display()
            )
        });
    }
    fs::create_dir_all(&embedded).unwrap_or_else(|error| {
        panic!(
            "create embedded builtin directory {}: {error}",
            embedded.display()
        )
    });

    for entry in
        fs::read_dir(&source).unwrap_or_else(|error| panic!("read {}: {error}", source.display()))
    {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_file() {
            fs::copy(&path, embedded.join(entry.file_name()))
                .unwrap_or_else(|error| panic!("copy builtin asset {}: {error}", path.display()));
        }
    }

    for (crate_name, artifact, embedded_name) in modules {
        let manifest = source.join(crate_name).join("Cargo.toml");
        let target = out_dir.join(format!("builtin-{crate_name}-target"));
        let status = Command::new(&cargo)
            .arg("build")
            .arg("--quiet")
            .arg("--release")
            .arg("--offline")
            .arg("--target")
            .arg("wasm32-unknown-unknown")
            .arg("--manifest-path")
            .arg(&manifest)
            .arg("--target-dir")
            .arg(&target)
            .status()
            .unwrap_or_else(|error| panic!("launch builtin {crate_name} WASM build: {error}"));
        if !status.success() {
            panic!(
                "builtin {crate_name} WASM build failed; ensure wasm32-unknown-unknown is installed (the Nix flake includes it)"
            );
        }
        let wasm = target
            .join("wasm32-unknown-unknown")
            .join("release")
            .join(artifact);
        fs::copy(&wasm, embedded.join(embedded_name))
            .unwrap_or_else(|error| panic!("copy {}: {error}", wasm.display()));
    }
}

fn emit_rerun_if_changed(path: &Path) {
    if path.is_file() {
        println!("cargo:rerun-if-changed={}", path.display());
        return;
    }
    for entry in
        fs::read_dir(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
    {
        let path = entry.unwrap().path();

        if path.is_dir() && path.file_name().is_some_and(|name| name == "target") {
            continue;
        }
        emit_rerun_if_changed(&path);
    }
}
