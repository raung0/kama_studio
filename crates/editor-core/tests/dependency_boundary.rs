use std::{fs, path::Path};

#[test]
fn editor_core_stays_headless() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(root.join("Cargo.toml")).expect("read editor-core manifest");
    let forbidden_dependencies = [
        "kama-ui",
        "kama-ui-renderer",
        "winit",
        "wgpu",
        "ffmpeg",
        "rodio",
        "wasmtime",
        "rfd",
    ];
    for dependency in forbidden_dependencies {
        assert!(
            !manifest.lines().any(|line| {
                line.trim_start()
                    .strip_prefix(dependency)
                    .is_some_and(|suffix| suffix.trim_start().starts_with('='))
            }),
            "editor-core must not depend on {dependency}"
        );
    }
}
