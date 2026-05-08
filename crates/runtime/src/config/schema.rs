/// JSON Schema generation for `~/.anvil/config.json` (and the settings.json family).
///
/// The schema is built programmatically rather than via a single `schema_for!`
/// macro because `RuntimeConfig` is an opaque BTreeMap loader, not a serde
/// struct.  We compose the top-level object from the concrete typed sub-structs
/// that the config loader does deserialise (via `schemars::schema_for!`).
///
/// Draft: JSON Schema draft 2019-09 (schemars 0.8 default).
use std::io;
use std::path::Path;

use schemars::schema::{
    ArrayValidation, InstanceType, Metadata, ObjectValidation, RootSchema, Schema,
    SchemaObject, SingleOrVec, SubschemaValidation,
};
use schemars::schema_for;
use serde_json::Value;

/// Build the full Anvil config JSON Schema as a `serde_json::Value`.
///
/// The returned document is a JSON Schema draft 2019-09 object whose `title`
/// is `"Anvil Configuration"`.  It is intended to be published at
/// `https://anvilhub.culpur.net/config-schema.json` so editors can validate
/// `~/.anvil/config.json` (and the settings.json family) in real time.
#[must_use]
pub fn emit_schema() -> Value {
    let schema = build_root_schema();
    serde_json::to_value(&schema).expect("schema serialisation is infallible")
}

/// Write the schema JSON to `path`, creating parent directories as needed.
///
/// # Errors
/// Returns an `io::Error` if the file cannot be written.
pub fn write_schema_to(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let schema = emit_schema();
    let json = serde_json::to_string_pretty(&schema)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    std::fs::write(path, json)
}

// ---------------------------------------------------------------------------
// Schema construction helpers
// ---------------------------------------------------------------------------

/// schemars 0.8 uses `IndexMap` for `ObjectValidation::properties`.
type PropMap = indexmap::IndexMap<String, Schema>;

fn string_schema() -> Schema {
    Schema::Object(SchemaObject {
        instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::String))),
        ..Default::default()
    })
}

fn bool_schema() -> Schema {
    Schema::Object(SchemaObject {
        instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::Boolean))),
        ..Default::default()
    })
}

fn integer_schema() -> Schema {
    Schema::Object(SchemaObject {
        instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::Integer))),
        ..Default::default()
    })
}

fn string_array_schema() -> Schema {
    Schema::Object(SchemaObject {
        instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::Array))),
        array: Some(Box::new(ArrayValidation {
            items: Some(SingleOrVec::Single(Box::new(string_schema()))),
            ..Default::default()
        })),
        ..Default::default()
    })
}

fn string_map_schema() -> Schema {
    Schema::Object(SchemaObject {
        instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::Object))),
        object: Some(Box::new(ObjectValidation {
            additional_properties: Some(Box::new(string_schema())),
            ..Default::default()
        })),
        ..Default::default()
    })
}

fn bool_map_schema() -> Schema {
    Schema::Object(SchemaObject {
        instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::Object))),
        object: Some(Box::new(ObjectValidation {
            additional_properties: Some(Box::new(bool_schema())),
            ..Default::default()
        })),
        ..Default::default()
    })
}

fn desc(s: &str) -> Option<Box<Metadata>> {
    Some(Box::new(Metadata {
        description: Some(s.to_string()),
        ..Default::default()
    }))
}

fn object_schema(description: Option<Box<Metadata>>, props: PropMap, required: Vec<String>, additional: bool) -> Schema {
    let mut req_set = schemars::Set::new();
    for r in required {
        req_set.insert(r);
    }
    Schema::Object(SchemaObject {
        metadata: description,
        instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::Object))),
        object: Some(Box::new(ObjectValidation {
            properties: props,
            required: req_set,
            additional_properties: Some(Box::new(Schema::Bool(additional))),
            ..Default::default()
        })),
        ..Default::default()
    })
}

// ---------------------------------------------------------------------------
// Top-level schema builder
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines)]
fn build_root_schema() -> RootSchema {
    // Sub-schemas derived from concrete typed structs.
    let hook_spec_schema = schema_for!(plugins::HookSpec);
    let output_style_schema = schema_for!(crate::config::OutputStyle);
    let sandbox_schema = schema_for!(crate::sandbox::SandboxConfig);

    // --- hooks -----------------------------------------------------------
    let hook_spec_value = serde_json::to_value(&hook_spec_schema)
        .expect("hook_spec schema to value");
    // Extract the inline schema object — schemars 0.8 may inline or ref it.
    let hook_spec_inline = serde_json::from_value::<Schema>(
        hook_spec_value
            .get("definitions")
            .and_then(|d| d.get("HookSpec"))
            .cloned()
            .unwrap_or_else(|| hook_spec_value.clone()),
    )
    .unwrap_or(Schema::Bool(true));

    let hook_array_schema = Schema::Object(SchemaObject {
        instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::Array))),
        array: Some(Box::new(ArrayValidation {
            items: Some(SingleOrVec::Single(Box::new(hook_spec_inline))),
            ..Default::default()
        })),
        ..Default::default()
    });

    let mut hooks_props = PropMap::new();
    hooks_props.insert("PreToolUse".to_string(), hook_array_schema.clone());
    hooks_props.insert("PostToolUse".to_string(), hook_array_schema);
    let hooks_schema = object_schema(desc("Hook commands run before/after tool use."), hooks_props, vec![], false);

    // --- mcpServers ------------------------------------------------------
    let mut stdio_props = PropMap::new();
    stdio_props.insert("command".to_string(), Schema::Object(SchemaObject {
        metadata: desc("Executable path or name."),
        instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::String))),
        ..Default::default()
    }));
    stdio_props.insert("args".to_string(), string_array_schema());
    stdio_props.insert("env".to_string(), string_map_schema());
    stdio_props.insert("alwaysLoad".to_string(), Schema::Object(SchemaObject {
        metadata: desc("When true, tools bypass tool-search deferral."),
        instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::Boolean))),
        ..Default::default()
    }));
    let stdio_server = object_schema(
        desc("Stdio-transport MCP server (default when `type` is absent)."),
        stdio_props,
        vec!["command".to_string()],
        true,
    );

    let mut mcp_oauth_props = PropMap::new();
    mcp_oauth_props.insert("clientId".to_string(), string_schema());
    mcp_oauth_props.insert("callbackPort".to_string(), integer_schema());
    mcp_oauth_props.insert("authServerMetadataUrl".to_string(), string_schema());
    mcp_oauth_props.insert("xaa".to_string(), bool_schema());
    let mcp_oauth_schema = object_schema(
        desc("Per-server OAuth configuration."),
        mcp_oauth_props,
        vec![],
        false,
    );

    let mut remote_props = PropMap::new();
    remote_props.insert("type".to_string(), string_schema());
    remote_props.insert("url".to_string(), string_schema());
    remote_props.insert("headers".to_string(), string_map_schema());
    remote_props.insert("headersHelper".to_string(), string_schema());
    remote_props.insert("oauth".to_string(), mcp_oauth_schema);
    remote_props.insert("alwaysLoad".to_string(), bool_schema());
    let remote_server = object_schema(
        desc("HTTP/SSE/WebSocket MCP server."),
        remote_props,
        vec!["type".to_string(), "url".to_string()],
        true,
    );

    let mcp_server_entry = Schema::Object(SchemaObject {
        subschemas: Some(Box::new(SubschemaValidation {
            one_of: Some(vec![stdio_server, remote_server]),
            ..Default::default()
        })),
        ..Default::default()
    });

    let mcp_servers_schema = Schema::Object(SchemaObject {
        metadata: desc("Map of MCP server name → server configuration."),
        instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::Object))),
        object: Some(Box::new(ObjectValidation {
            additional_properties: Some(Box::new(mcp_server_entry)),
            ..Default::default()
        })),
        ..Default::default()
    });

    // --- oauth -----------------------------------------------------------
    let mut oauth_props = PropMap::new();
    oauth_props.insert("clientId".to_string(), string_schema());
    oauth_props.insert("authorizeUrl".to_string(), string_schema());
    oauth_props.insert("tokenUrl".to_string(), string_schema());
    oauth_props.insert("callbackPort".to_string(), integer_schema());
    oauth_props.insert("manualRedirectUrl".to_string(), string_schema());
    oauth_props.insert("scopes".to_string(), string_array_schema());
    let oauth_schema = object_schema(
        desc("Global OAuth configuration for the Anthropic API."),
        oauth_props,
        vec!["clientId".to_string(), "authorizeUrl".to_string(), "tokenUrl".to_string()],
        false,
    );

    // --- lspServers ------------------------------------------------------
    let mut lsp_entry_props = PropMap::new();
    lsp_entry_props.insert("name".to_string(), string_schema());
    lsp_entry_props.insert("command".to_string(), string_schema());
    lsp_entry_props.insert("args".to_string(), string_array_schema());
    lsp_entry_props.insert("env".to_string(), string_map_schema());
    lsp_entry_props.insert("workspaceRoot".to_string(), string_schema());
    lsp_entry_props.insert("extensionToLanguage".to_string(), string_map_schema());
    let lsp_entry = object_schema(
        desc("A single LSP server configuration."),
        lsp_entry_props,
        vec!["name".to_string(), "command".to_string()],
        false,
    );
    let lsp_servers_schema = Schema::Object(SchemaObject {
        metadata: desc("Array of LSP server definitions."),
        instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::Array))),
        array: Some(Box::new(ArrayValidation {
            items: Some(SingleOrVec::Single(Box::new(lsp_entry))),
            ..Default::default()
        })),
        ..Default::default()
    });

    // --- plugins ---------------------------------------------------------
    let mut plugins_props = PropMap::new();
    plugins_props.insert("externalDirectories".to_string(), string_array_schema());
    plugins_props.insert("installRoot".to_string(), string_schema());
    plugins_props.insert("registryPath".to_string(), string_schema());
    plugins_props.insert("bundledRoot".to_string(), string_schema());
    let plugins_schema = object_schema(
        desc("Plugin directory and registry paths."),
        plugins_props,
        vec![],
        false,
    );

    // --- permissionMode --------------------------------------------------
    let permission_mode_enum = Schema::Object(SchemaObject {
        metadata: desc("Explicit permission mode override."),
        instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::String))),
        enum_values: Some(vec![
            Value::String("default".to_string()),
            Value::String("plan".to_string()),
            Value::String("read-only".to_string()),
            Value::String("acceptEdits".to_string()),
            Value::String("auto".to_string()),
            Value::String("workspace-write".to_string()),
            Value::String("dontAsk".to_string()),
            Value::String("danger-full-access".to_string()),
        ]),
        ..Default::default()
    });

    let mut perm_props = PropMap::new();
    perm_props.insert("defaultMode".to_string(), permission_mode_enum.clone());
    let permissions_schema = object_schema(
        desc("Legacy permissions block."),
        perm_props,
        vec![],
        true,
    );

    // --- sandbox ---------------------------------------------------------
    let sandbox_value = serde_json::to_value(&sandbox_schema).expect("sandbox schema to value");
    let sandbox_resolved = serde_json::from_value::<Schema>(
        sandbox_value
            .get("definitions")
            .and_then(|d| d.get("SandboxConfig"))
            .cloned()
            .unwrap_or_else(|| sandbox_value.clone()),
    )
    .unwrap_or(Schema::Bool(true));

    // --- output_style ----------------------------------------------------
    let os_value = serde_json::to_value(&output_style_schema).expect("output_style schema to value");
    let output_style_resolved = serde_json::from_value::<Schema>(
        os_value
            .get("definitions")
            .and_then(|d| d.get("OutputStyle"))
            .cloned()
            .unwrap_or_else(|| os_value.clone()),
    )
    .unwrap_or(Schema::Bool(true));

    // --- profiles (permissive until W4 lands) ----------------------------
    let profiles_schema = Schema::Object(SchemaObject {
        metadata: desc(
            "Named profile overrides. Schema will tighten when the profiles workstream lands.",
        ),
        instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::Object))),
        object: Some(Box::new(ObjectValidation {
            additional_properties: Some(Box::new(Schema::Bool(true))),
            ..Default::default()
        })),
        ..Default::default()
    });

    // --- top-level object ------------------------------------------------
    let mut top_props = PropMap::new();

    top_props.insert(
        "$schema".to_string(),
        Schema::Object(SchemaObject {
            metadata: desc("JSON Schema URI for this config file."),
            instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::String))),
            ..Default::default()
        }),
    );
    top_props.insert(
        "model".to_string(),
        Schema::Object(SchemaObject {
            metadata: desc("Default model identifier (e.g. claude-opus-4-6)."),
            instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::String))),
            ..Default::default()
        }),
    );
    top_props.insert("hooks".to_string(), hooks_schema);
    top_props.insert("mcpServers".to_string(), mcp_servers_schema);
    top_props.insert("oauth".to_string(), oauth_schema);
    top_props.insert("lspServers".to_string(), lsp_servers_schema);
    top_props.insert("plugins".to_string(), plugins_schema);
    top_props.insert("enabledPlugins".to_string(), bool_map_schema());
    top_props.insert("permissionMode".to_string(), permission_mode_enum);
    top_props.insert("permissions".to_string(), permissions_schema);
    top_props.insert("sandbox".to_string(), sandbox_resolved);
    top_props.insert("output_style".to_string(), output_style_resolved);
    top_props.insert("profiles".to_string(), profiles_schema);
    top_props.insert(
        "env".to_string(),
        Schema::Object(SchemaObject {
            metadata: desc("Extra environment variables injected into tool subprocesses."),
            instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::Object))),
            object: Some(Box::new(ObjectValidation {
                additional_properties: Some(Box::new(string_schema())),
                ..Default::default()
            })),
            ..Default::default()
        }),
    );

    let root_object = SchemaObject {
        metadata: Some(Box::new(Metadata {
            title: Some("Anvil Configuration".to_string()),
            description: Some(
                "Schema for ~/.anvil/config.json and the settings.json family. \
                 Published at https://anvilhub.culpur.net/config-schema.json."
                    .to_string(),
            ),
            ..Default::default()
        })),
        instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::Object))),
        object: Some(Box::new(ObjectValidation {
            properties: top_props,
            additional_properties: Some(Box::new(Schema::Bool(true))),
            ..Default::default()
        })),
        ..Default::default()
    };

    RootSchema {
        meta_schema: Some("http://json-schema.org/draft-07/schema#".to_string()),
        schema: root_object,
        definitions: Default::default(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The top-level schema must carry the expected `title`.
    #[test]
    fn schema_title_is_anvil_configuration() {
        let schema = emit_schema();
        let title = schema
            .pointer("/title")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(title, "Anvil Configuration");
    }

    /// Every property the config loader reads must appear in the `properties`
    /// map so editors get IntelliSense on every key.
    #[test]
    fn schema_covers_all_top_level_fields() {
        let schema = emit_schema();
        let props = schema
            .pointer("/properties")
            .and_then(|v| v.as_object())
            .expect("schema must have a properties object");

        let required_keys = [
            "model",
            "hooks",
            "mcpServers",
            "oauth",
            "lspServers",
            "plugins",
            "enabledPlugins",
            "permissionMode",
            "permissions",
            "sandbox",
            "output_style",
            "profiles",
            "env",
        ];
        for key in &required_keys {
            assert!(
                props.contains_key(*key),
                "schema missing top-level property: {key}"
            );
        }
    }

    /// Round-trip a representative config through `emit_schema` and verify the
    /// schema value is valid JSON (not just a Default-zero struct).
    #[test]
    fn schema_round_trips_to_valid_json() {
        let schema = emit_schema();
        let json_str = serde_json::to_string(&schema).expect("schema must serialise");
        assert!(!json_str.is_empty());
        let reparsed: Value = serde_json::from_str(&json_str).expect("must be valid JSON");
        assert!(reparsed.is_object());
    }

    /// `write_schema_to` writes a file containing valid JSON.
    #[test]
    fn write_schema_to_creates_valid_file() {
        let dir = std::env::temp_dir().join(format!(
            "schema-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let path = dir.join("config-schema.json");
        write_schema_to(&path).expect("write should succeed");
        let contents = std::fs::read_to_string(&path).expect("file should exist");
        let parsed: Value = serde_json::from_str(&contents).expect("file must be valid JSON");
        assert_eq!(
            parsed.pointer("/title").and_then(|v| v.as_str()),
            Some("Anvil Configuration")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
