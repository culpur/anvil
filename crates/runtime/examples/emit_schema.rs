/// Regenerate `assets/config-schema.json`.
///
/// Run from the repo root:
///   cargo run -p runtime --example emit_schema
fn main() {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let output = std::path::Path::new(manifest)
        .join("assets")
        .join("config-schema.json");

    runtime::write_config_schema_to(&output)
        .expect("failed to write config-schema.json");

    println!("Wrote {}", output.display());
}
