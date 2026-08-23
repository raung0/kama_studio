use std::fs;

fn main() -> Result<(), String> {
    for path in ["src/shader.wgsl", "src/blur.wgsl", "src/present.wgsl"] {
        println!("cargo:rerun-if-changed={path}");
        let source = fs::read_to_string(path).map_err(|error| format!("{path}: {error:#?}"))?;
        let module =
            naga::front::wgsl::parse_str(&source).map_err(|error| format!("{path}: {error:#?}"))?;
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .map_err(|error| format!("{path}: {error:#?}"))?;
    }
    Ok(())
}
