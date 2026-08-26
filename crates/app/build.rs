#![allow(clippy::arithmetic_side_effects, clippy::exit)]

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
    let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
        eprintln!("invalid icon file name: {}", path.display());
        std::process::exit(1);
    };
    stem.split('_')
        .map(|word| {
            let mut chars = word.chars();
            let Some(first) = chars.next() else {
                return String::new();
            };
            first.to_uppercase().chain(chars).collect::<String>()
        })
        .collect()
}

fn main() {
    if matches!(std::env::var("CARGO_CFG_TARGET_OS"), Ok(value) if value == "windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("icon.ico");
        if let Err(error) = res.compile() {
            eprintln!("windows resource compile failed: {error}");
            std::process::exit(1);
        }
    }

    let Some(out_dir_os) = env::var_os("OUT_DIR") else {
        eprintln!("OUT_DIR missing");
        std::process::exit(1);
    };
    let out_dir = PathBuf::from(out_dir_os);
    println!("cargo:rerun-if-changed=assets");
    embed_builtin_plugin(&out_dir);
    let mut icons = match fs::read_dir("assets") {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|extension| extension == "svg"))
            .collect::<Vec<_>>(),
        Err(error) => {
            eprintln!("read assets: {error}");
            std::process::exit(1);
        }
    };
    icons.sort();
    let icon_bytes = usize::try_from(ICON_SIZE)
        .ok()
        .and_then(|size| size.checked_mul(size))
        .and_then(|size| size.checked_mul(4))
        .unwrap_or_else(|| {
            eprintln!("icon size overflow");
            std::process::exit(1);
        });
    let atlas_bytes = icon_bytes.checked_mul(icons.len()).unwrap_or_else(|| {
        eprintln!("icon atlas size overflow");
        std::process::exit(1);
    });
    let mut atlas = Vec::with_capacity(atlas_bytes);
    let variants = icons
        .iter()
        .map(|path| variant(path))
        .collect::<Vec<_>>()
        .join(",");
    if let Err(error) = fs::write(
        out_dir.join("app_icons.rs"),
        format!(
            "#[allow(dead_code)] #[repr(usize)] #[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum AppIcon {{ {variants} }} impl AppIcon {{ const COUNT: usize = {}; }}",
            icons.len()
        ),
    ) {
        eprintln!("write app_icons.rs: {error}");
        std::process::exit(1);
    }

    for path in icons {
        let data = match fs::read(&path) {
            Ok(data) => data,
            Err(error) => {
                eprintln!("{}: {error}", path.display());
                std::process::exit(1);
            }
        };
        let options = Options::default();
        let tree = match Tree::from_data(&data, &options) {
            Ok(tree) => tree,
            Err(error) => {
                eprintln!("{}: {error}", path.display());
                std::process::exit(1);
            }
        };
        let size = tree.size();
        let transform = Transform::from_scale(
            ICON_SIZE as f32 / size.width(),
            ICON_SIZE as f32 / size.height(),
        );
        let Some(mut pixmap) = Pixmap::new(ICON_SIZE, ICON_SIZE) else {
            eprintln!("create pixmap for {}", path.display());
            std::process::exit(1);
        };
        resvg::render(&tree, transform, &mut pixmap.as_mut());
        for pixel in pixmap.data_mut().chunks_exact_mut(4) {
            let [red, green, blue, alpha] = pixel else {
                continue;
            };
            let alpha = u32::from(*alpha);
            if alpha == 0 {
                continue;
            }
            for channel in [red, green, blue] {
                *channel = ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
        atlas.extend_from_slice(pixmap.data());
    }

    if let Err(error) = fs::write(out_dir.join("icons.rgba"), atlas) {
        eprintln!("write icons.rgba: {error}");
        std::process::exit(1);
    }
    export_app_version();
    export_update_repository();
}

fn export_update_repository() {
    println!("cargo:rerun-if-env-changed=GITHUB_REPOSITORY");
    let repository =
        env::var("GITHUB_REPOSITORY").unwrap_or_else(|_| "raung0/kama_studio".to_owned());
    println!("cargo:rustc-env=APP_UPDATE_REPOSITORY={repository}");
}

fn export_app_version() {
    let Some(manifest_dir) = env::var_os("CARGO_MANIFEST_DIR") else {
        eprintln!("CARGO_MANIFEST_DIR missing");
        std::process::exit(1);
    };
    let workspace = PathBuf::from(manifest_dir).join("../..");
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
    let Some(manifest_dir) = env::var_os("CARGO_MANIFEST_DIR") else {
        eprintln!("CARGO_MANIFEST_DIR missing");
        std::process::exit(1);
    };
    let source = PathBuf::from(manifest_dir).join("../..").join("builtins");
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
        if let Err(error) = fs::remove_dir_all(&embedded_root) {
            eprintln!(
                "remove stale embedded plugin directory {}: {error}",
                embedded_root.display()
            );
            std::process::exit(1);
        }
    }
    if let Err(error) = fs::create_dir_all(&embedded) {
        eprintln!(
            "create embedded builtin directory {}: {error}",
            embedded.display()
        );
        std::process::exit(1);
    }

    let entries = match fs::read_dir(&source) {
        Ok(entries) => entries,
        Err(error) => {
            eprintln!("read {}: {error}", source.display());
            std::process::exit(1);
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                eprintln!("read {}: {error}", source.display());
                std::process::exit(1);
            }
        };
        let path = entry.path();
        if path.is_file() {
            if let Err(error) = fs::copy(&path, embedded.join(entry.file_name())) {
                eprintln!("copy builtin asset {}: {error}", path.display());
                std::process::exit(1);
            }
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
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .status()
            .unwrap_or_else(|error| {
                eprintln!("launch builtin {crate_name} WASM build: {error}");
                std::process::exit(1);
            });
        assert!(
            status.success(),
            "builtin {crate_name} WASM build failed; ensure wasm32-unknown-unknown is installed (the Nix flake includes it)"
        );
        let wasm = target
            .join("wasm32-unknown-unknown")
            .join("release")
            .join(artifact);
        if let Err(error) = fs::copy(&wasm, embedded.join(embedded_name)) {
            eprintln!("copy {}: {error}", wasm.display());
            std::process::exit(1);
        }
    }
}

fn emit_rerun_if_changed(path: &Path) {
    if path.is_file() {
        println!("cargo:rerun-if-changed={}", path.display());
        return;
    }
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) => {
            eprintln!("read {}: {error}", path.display());
            std::process::exit(1);
        }
    };
    for entry in entries {
        let path = match entry {
            Ok(entry) => entry.path(),
            Err(error) => {
                eprintln!("read {}: {error}", path.display());
                std::process::exit(1);
            }
        };

        if path.is_dir() && path.file_name().is_some_and(|name| name == "target") {
            continue;
        }
        emit_rerun_if_changed(&path);
    }
}
