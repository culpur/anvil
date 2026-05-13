use std::path::Path;

/// Build script for the `runtime` crate.
///
/// Emits two build-time assets:
/// 1. `assets/config-schema.json` — JSON Schema for Anvil settings, published
///    to `https://anvilhub.culpur.net/config-schema.json` by the deploy pipeline.
/// 2. `OUT_DIR/release_notes.md` — copy of `RELEASE-NOTES-v{CARGO_PKG_VERSION}.md`
///    from the workspace root, embedded via `include_str!` so end users get the
///    current release notes baked into the binary (they don't ship with the .md).
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/config/schema.rs");
    println!("cargo:rerun-if-changed=src/config/mod.rs");
    println!("cargo:rerun-if-changed=src/sandbox.rs");
    println!("cargo:rerun-if-changed=assets/config-schema.json");

    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set");
    let asset_path = Path::new(&manifest).join("assets").join("config-schema.json");
    if !asset_path.exists() {
        println!(
            "cargo:warning=assets/config-schema.json not found; \
             run `cargo run -p runtime --example emit_schema` to regenerate it."
        );
    }

    embed_release_notes(&manifest);
}

/// Embed `RELEASE-NOTES-v{version}.md` into the binary at build time.
///
/// Strategy: look for `<workspace-root>/RELEASE-NOTES-v{CARGO_PKG_VERSION}.md`.
/// If found, copy to `OUT_DIR/release_notes.md`. If not (e.g., dev build during
/// version bump, or building a tagged commit before notes were written), write
/// a placeholder so `include_str!` still succeeds.
fn embed_release_notes(manifest: &str) {
    let version = std::env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION must be set");
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR must be set");
    let dest = Path::new(&out_dir).join("release_notes.md");

    // Workspace root is two levels up from crates/runtime/.
    let workspace_root = Path::new(manifest)
        .parent()
        .and_then(Path::parent)
        .expect("workspace root resolvable from crates/runtime");

    let notes_path = workspace_root.join(format!("RELEASE-NOTES-v{version}.md"));
    println!("cargo:rerun-if-changed={}", notes_path.display());

    let contents = std::fs::read_to_string(&notes_path).unwrap_or_else(|_| {
        println!(
            "cargo:warning=RELEASE-NOTES-v{version}.md not found at {}; \
             embedding placeholder. The /changelog command + env-block headline \
             will fall back to a generic message until notes are written.",
            notes_path.display()
        );
        format!(
            "# Anvil v{version}\n\nRelease notes not yet written for this build.\n\
             See https://github.com/culpur/anvil/releases for published notes.\n"
        )
    });

    std::fs::write(&dest, contents).expect("write release_notes.md to OUT_DIR");
}
