/// Build script for the `runtime` crate.
///
/// Emits `assets/config-schema.json` as a deterministic build-time snapshot of
/// the Anvil config JSON Schema so it can be queried from the installed package
/// and published to `https://anvilhub.culpur.net/config-schema.json` by the
/// deploy pipeline (Workstream 5 — schema publishing).
fn main() {
    // Rebuild only when config sources change or the asset itself is updated.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/config/schema.rs");
    println!("cargo:rerun-if-changed=src/config/mod.rs");
    println!("cargo:rerun-if-changed=src/sandbox.rs");
    println!("cargo:rerun-if-changed=assets/config-schema.json");

    // If the committed asset is missing (e.g. fresh checkout before running
    // the emit_schema example), warn but do NOT fail the build.
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set");
    let asset_path = std::path::Path::new(&manifest).join("assets").join("config-schema.json");
    if !asset_path.exists() {
        println!(
            "cargo:warning=assets/config-schema.json not found; \
             run `cargo run -p runtime --example emit_schema` to regenerate it."
        );
    }
}
