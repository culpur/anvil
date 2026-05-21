pub mod jsonrpc;
pub mod server_manager;
pub mod transport;
pub mod types;

pub use jsonrpc::{JsonRpcError, JsonRpcId, JsonRpcRequest, JsonRpcResponse};
pub use server_manager::{
    ManagedMcpTool, McpServerManager, McpServerManagerError, UnsupportedMcpServer,
};
pub use transport::{spawn_mcp_stdio_process, McpStdioProcess};
pub use types::{
    advance_pagination_cursor, McpInitializeClientInfo, McpInitializeParams, McpInitializeResult,
    McpInitializeServerInfo, McpListPromptsParams, McpListPromptsResult,
    McpListResourceTemplatesParams, McpListResourceTemplatesResult, McpListResourcesParams,
    McpListResourcesResult, McpListToolsParams, McpListToolsResult, McpPrompt, McpPromptArgument,
    McpReadResourceParams, McpReadResourceResult, McpResource, McpResourceContents,
    McpResourceTemplate, McpTool, McpToolCallContent, McpToolCallParams, McpToolCallResult,
};

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::ErrorKind;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;
    use tokio::runtime::Builder;

    use crate::config::{
        ConfigSource, McpRemoteServerConfig, McpSdkServerConfig, McpServerConfig,
        McpStdioServerConfig, McpWebSocketServerConfig, ScopedMcpServerConfig,
    };
    use crate::mcp::mcp_tool_name;
    use crate::mcp_client::McpClientBootstrap;

    use super::{
        spawn_mcp_stdio_process, JsonRpcId, JsonRpcRequest, JsonRpcResponse,
        McpInitializeClientInfo, McpInitializeParams, McpInitializeResult, McpInitializeServerInfo,
        McpListToolsResult, McpReadResourceParams, McpReadResourceResult, McpServerManager,
        McpServerManagerError, McpStdioProcess, McpTool, McpToolCallParams,
    };

    fn temp_dir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be after epoch")
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        std::env::temp_dir().join(format!("runtime-mcp-stdio-{pid}-{nanos}-{n}"))
    }

    fn write_echo_script() -> PathBuf {
        let root = temp_dir();
        fs::create_dir_all(&root).expect("temp dir");
        let script_path = root.join("echo-mcp.sh");
        fs::write(
            &script_path,
            "#!/bin/sh\nprintf 'READY:%s\\n' \"$MCP_TEST_TOKEN\"\nIFS= read -r line\nprintf 'ECHO:%s\\n' \"$line\"\n",
        )
        .expect("write script");
        let mut permissions = fs::metadata(&script_path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("chmod");
        script_path
    }

    fn write_jsonrpc_script() -> PathBuf {
        let root = temp_dir();
        fs::create_dir_all(&root).expect("temp dir");
        let script_path = root.join("jsonrpc-mcp.py");
        let script = [
            "#!/usr/bin/env python3",
            "import json, sys",
            "header = b''",
            r"while not header.endswith(b'\r\n\r\n'):",
            "    chunk = sys.stdin.buffer.read(1)",
            "    if not chunk:",
            "        raise SystemExit(1)",
            "    header += chunk",
            "length = 0",
            r"for line in header.decode().split('\r\n'):",
            r"    if line.lower().startswith('content-length:'):",
            r"        length = int(line.split(':', 1)[1].strip())",
            "payload = sys.stdin.buffer.read(length)",
            "request = json.loads(payload.decode())",
            r"assert request['jsonrpc'] == '2.0'",
            r"assert request['method'] == 'initialize'",
            r"response = json.dumps({",
            r"    'jsonrpc': '2.0',",
            r"    'id': request['id'],",
            r"    'result': {",
            r"        'protocolVersion': request['params']['protocolVersion'],",
            r"        'capabilities': {'tools': {}},",
            r"        'serverInfo': {'name': 'fake-mcp', 'version': '0.1.0'}",
            r"    }",
            r"}).encode()",
            r"sys.stdout.buffer.write(f'Content-Length: {len(response)}\r\n\r\n'.encode() + response)",
            "sys.stdout.buffer.flush()",
            "",
        ]
        .join("\n");
        fs::write(&script_path, script).expect("write script");
        let mut permissions = fs::metadata(&script_path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("chmod");
        script_path
    }

    #[allow(clippy::too_many_lines)]
    fn write_mcp_server_script() -> PathBuf {
        let root = temp_dir();
        fs::create_dir_all(&root).expect("temp dir");
        let script_path = root.join("fake-mcp-server.py");
        let script = [
            "#!/usr/bin/env python3",
            "import json, sys",
            "",
            "def read_message():",
            "    header = b''",
            r"    while not header.endswith(b'\r\n\r\n'):",
            "        chunk = sys.stdin.buffer.read(1)",
            "        if not chunk:",
            "            return None",
            "        header += chunk",
            "    length = 0",
            r"    for line in header.decode().split('\r\n'):",
            r"        if line.lower().startswith('content-length:'):",
            r"            length = int(line.split(':', 1)[1].strip())",
            "    payload = sys.stdin.buffer.read(length)",
            "    return json.loads(payload.decode())",
            "",
            "def send_message(message):",
            "    payload = json.dumps(message).encode()",
            r"    sys.stdout.buffer.write(f'Content-Length: {len(payload)}\r\n\r\n'.encode() + payload)",
            "    sys.stdout.buffer.flush()",
            "",
            "while True:",
            "    request = read_message()",
            "    if request is None:",
            "        break",
            "    method = request['method']",
            "    if method == 'initialize':",
            "        send_message({",
            "            'jsonrpc': '2.0',",
            "            'id': request['id'],",
            "            'result': {",
            "                'protocolVersion': request['params']['protocolVersion'],",
            "                'capabilities': {'tools': {}, 'resources': {}},",
            "                'serverInfo': {'name': 'fake-mcp', 'version': '0.2.0'}",
            "            }",
            "        })",
            "    elif method == 'tools/list':",
            "        send_message({",
            "            'jsonrpc': '2.0',",
            "            'id': request['id'],",
            "            'result': {",
            "                'tools': [",
            "                    {",
            "                        'name': 'echo',",
            "                        'description': 'Echoes text',",
            "                        'inputSchema': {",
            "                            'type': 'object',",
            "                            'properties': {'text': {'type': 'string'}},",
            "                            'required': ['text']",
            "                        }",
            "                    }",
            "                ]",
            "            }",
            "        })",
            "    elif method == 'tools/call':",
            "        args = request['params'].get('arguments') or {}",
            "        if request['params']['name'] == 'fail':",
            "            send_message({",
            "                'jsonrpc': '2.0',",
            "                'id': request['id'],",
            "                'error': {'code': -32001, 'message': 'tool failed'},",
            "            })",
            "        else:",
            "            text = args.get('text', '')",
            "            send_message({",
            "                'jsonrpc': '2.0',",
            "                'id': request['id'],",
            "                'result': {",
            "                    'content': [{'type': 'text', 'text': f'echo:{text}'}],",
            "                    'structuredContent': {'echoed': text},",
            "                    'isError': False",
            "                }",
            "            })",
            "    elif method == 'resources/list':",
            "        send_message({",
            "            'jsonrpc': '2.0',",
            "            'id': request['id'],",
            "            'result': {",
            "                'resources': [",
            "                    {",
            "                        'uri': 'file://guide.txt',",
            "                        'name': 'guide',",
            "                        'description': 'Guide text',",
            "                        'mimeType': 'text/plain'",
            "                    }",
            "                ]",
            "            }",
            "        })",
            "    elif method == 'resources/read':",
            "        uri = request['params']['uri']",
            "        send_message({",
            "            'jsonrpc': '2.0',",
            "            'id': request['id'],",
            "            'result': {",
            "                'contents': [",
            "                    {",
            "                        'uri': uri,",
            "                        'mimeType': 'text/plain',",
            "                        'text': f'contents for {uri}'",
            "                    }",
            "                ]",
            "            }",
            "        })",
            "    else:",
            "        send_message({",
            "            'jsonrpc': '2.0',",
            "            'id': request['id'],",
            "            'error': {'code': -32601, 'message': f'unknown method: {method}'},",
            "        })",
            "",
        ]
        .join("\n");
        fs::write(&script_path, script).expect("write script");
        let mut permissions = fs::metadata(&script_path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("chmod");
        script_path
    }

    #[allow(clippy::too_many_lines)]
    fn write_manager_mcp_server_script() -> PathBuf {
        let root = temp_dir();
        fs::create_dir_all(&root).expect("temp dir");
        let script_path = root.join("manager-mcp-server.py");
        let script = [
            "#!/usr/bin/env python3",
            "import json, os, sys",
            "",
            "LABEL = os.environ.get('MCP_SERVER_LABEL', 'server')",
            "LOG_PATH = os.environ.get('MCP_LOG_PATH')",
            "initialize_count = 0",
            "",
            "def log(method):",
            "    if LOG_PATH:",
            "        with open(LOG_PATH, 'a', encoding='utf-8') as handle:",
            "            handle.write(f'{method}\\n')",
            "",
            "def read_message():",
            "    header = b''",
            r"    while not header.endswith(b'\r\n\r\n'):",
            "        chunk = sys.stdin.buffer.read(1)",
            "        if not chunk:",
            "            return None",
            "        header += chunk",
            "    length = 0",
            r"    for line in header.decode().split('\r\n'):",
            r"        if line.lower().startswith('content-length:'):",
            r"            length = int(line.split(':', 1)[1].strip())",
            "    payload = sys.stdin.buffer.read(length)",
            "    return json.loads(payload.decode())",
            "",
            "def send_message(message):",
            "    payload = json.dumps(message).encode()",
            r"    sys.stdout.buffer.write(f'Content-Length: {len(payload)}\r\n\r\n'.encode() + payload)",
            "    sys.stdout.buffer.flush()",
            "",
            "while True:",
            "    request = read_message()",
            "    if request is None:",
            "        break",
            "    method = request['method']",
            "    log(method)",
            "    if method == 'initialize':",
            "        initialize_count += 1",
            "        send_message({",
            "            'jsonrpc': '2.0',",
            "            'id': request['id'],",
            "            'result': {",
            "                'protocolVersion': request['params']['protocolVersion'],",
            "                'capabilities': {'tools': {}},",
            "                'serverInfo': {'name': LABEL, 'version': '1.0.0'}",
            "            }",
            "        })",
            "    elif method == 'tools/list':",
            "        send_message({",
            "            'jsonrpc': '2.0',",
            "            'id': request['id'],",
            "            'result': {",
            "                'tools': [",
            "                    {",
            "                        'name': 'echo',",
            "                        'description': f'Echo tool for {LABEL}',",
            "                        'inputSchema': {",
            "                            'type': 'object',",
            "                            'properties': {'text': {'type': 'string'}},",
            "                            'required': ['text']",
            "                        }",
            "                    }",
            "                ]",
            "            }",
            "        })",
            "    elif method == 'tools/call':",
            "        args = request['params'].get('arguments') or {}",
            "        text = args.get('text', '')",
            "        send_message({",
            "            'jsonrpc': '2.0',",
            "            'id': request['id'],",
            "            'result': {",
            "                'content': [{'type': 'text', 'text': f'{LABEL}:{text}'}],",
            "                'structuredContent': {",
            "                    'server': LABEL,",
            "                    'echoed': text,",
            "                    'initializeCount': initialize_count",
            "                },",
            "                'isError': False",
            "            }",
            "        })",
            "    else:",
            "        send_message({",
            "            'jsonrpc': '2.0',",
            "            'id': request['id'],",
            "            'error': {'code': -32601, 'message': f'unknown method: {method}'},",
            "        })",
            "",
        ]
        .join("\n");
        fs::write(&script_path, script).expect("write script");
        let mut permissions = fs::metadata(&script_path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("chmod");
        script_path
    }

    fn sample_bootstrap(script_path: &Path) -> McpClientBootstrap {
        let config = ScopedMcpServerConfig {
            scope: ConfigSource::Local,
            config: McpServerConfig::Stdio(McpStdioServerConfig {
                command: "/bin/sh".to_string(),
                args: vec![script_path.to_string_lossy().into_owned()],
                env: BTreeMap::from([("MCP_TEST_TOKEN".to_string(), "secret-value".to_string())]),
                always_load: false,
            }),
        };
        McpClientBootstrap::from_scoped_config("stdio server", &config)
    }

    fn script_transport(script_path: &Path) -> crate::mcp_client::McpStdioTransport {
        crate::mcp_client::McpStdioTransport {
            command: python_command(),
            args: vec![script_path.to_string_lossy().into_owned()],
            env: BTreeMap::new(),
        }
    }

    fn python_command() -> String {
        for key in ["MCP_TEST_PYTHON", "PYTHON3", "PYTHON"] {
            if let Ok(value) = std::env::var(key)
                && !value.trim().is_empty() {
                    return value;
                }
        }

        for candidate in ["python3", "python"] {
            if Command::new(candidate).arg("--version").output().is_ok() {
                return candidate.to_string();
            }
        }

        panic!("expected a Python interpreter for MCP stdio tests")
    }

    fn cleanup_script(script_path: &Path) {
        if let Err(error) = fs::remove_file(script_path) {
            assert_eq!(error.kind(), std::io::ErrorKind::NotFound, "cleanup script");
        }
        if let Err(error) = fs::remove_dir_all(script_path.parent().expect("script parent")) {
            assert_eq!(error.kind(), std::io::ErrorKind::NotFound, "cleanup dir");
        }
    }

    fn manager_server_config(
        script_path: &Path,
        label: &str,
        log_path: &Path,
    ) -> ScopedMcpServerConfig {
        manager_server_config_with_always_load(script_path, label, log_path, false)
    }

    fn manager_server_config_with_always_load(
        script_path: &Path,
        label: &str,
        log_path: &Path,
        always_load: bool,
    ) -> ScopedMcpServerConfig {
        ScopedMcpServerConfig {
            scope: ConfigSource::Local,
            config: McpServerConfig::Stdio(McpStdioServerConfig {
                command: python_command(),
                args: vec![script_path.to_string_lossy().into_owned()],
                env: BTreeMap::from([
                    ("MCP_SERVER_LABEL".to_string(), label.to_string()),
                    (
                        "MCP_LOG_PATH".to_string(),
                        log_path.to_string_lossy().into_owned(),
                    ),
                ]),
                always_load,
            }),
        }
    }

    #[test]
    fn spawns_stdio_process_and_round_trips_io() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let script_path = write_echo_script();
            let bootstrap = sample_bootstrap(&script_path);
            let mut process = spawn_mcp_stdio_process(&bootstrap).expect("spawn stdio process");

            let ready = process.read_line().await.expect("read ready");
            assert_eq!(ready, "READY:secret-value\n");

            process
                .write_line("ping from client")
                .await
                .expect("write line");

            let echoed = process.read_line().await.expect("read echo");
            assert_eq!(echoed, "ECHO:ping from client\n");

            let status = process.wait().await.expect("wait for exit");
            assert!(status.success());

            cleanup_script(&script_path);
        });
    }

    #[test]
    fn rejects_non_stdio_bootstrap() {
        let config = ScopedMcpServerConfig {
            scope: ConfigSource::Local,
            config: McpServerConfig::Sdk(crate::config::McpSdkServerConfig {
                name: "sdk-server".to_string(),
                always_load: false,
            }),
        };
        let bootstrap = McpClientBootstrap::from_scoped_config("sdk server", &config);
        let error = spawn_mcp_stdio_process(&bootstrap).expect_err("non-stdio should fail");
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
    }

    #[test]
    fn round_trips_initialize_request_and_response_over_stdio_frames() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let script_path = write_jsonrpc_script();
            let transport = script_transport(&script_path);
            let mut process = McpStdioProcess::spawn(&transport).expect("spawn transport directly");

            let response = process
                .initialize(
                    JsonRpcId::Number(1),
                    McpInitializeParams {
                        protocol_version: "2025-03-26".to_string(),
                        capabilities: json!({"roots": {}}),
                        client_info: McpInitializeClientInfo {
                            name: "runtime-tests".to_string(),
                            version: "0.1.0".to_string(),
                        },
                    },
                )
                .await
                .expect("initialize roundtrip");

            assert_eq!(response.id, JsonRpcId::Number(1));
            assert_eq!(response.error, None);
            assert_eq!(
                response.result,
                Some(McpInitializeResult {
                    protocol_version: "2025-03-26".to_string(),
                    capabilities: json!({"tools": {}}),
                    server_info: McpInitializeServerInfo {
                        name: "fake-mcp".to_string(),
                        version: "0.1.0".to_string(),
                    },
                })
            );

            let status = process.wait().await.expect("wait for exit");
            assert!(status.success());

            cleanup_script(&script_path);
        });
    }

    #[test]
    fn write_jsonrpc_request_emits_content_length_frame() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let script_path = write_jsonrpc_script();
            let transport = script_transport(&script_path);
            let mut process = McpStdioProcess::spawn(&transport).expect("spawn transport directly");
            let request = JsonRpcRequest::new(
                JsonRpcId::Number(7),
                "initialize",
                Some(json!({
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": {"name": "runtime-tests", "version": "0.1.0"}
                })),
            );

            process.send_request(&request).await.expect("send request");
            let response: JsonRpcResponse<serde_json::Value> =
                process.read_response().await.expect("read response");

            assert_eq!(response.id, JsonRpcId::Number(7));
            assert_eq!(response.jsonrpc, "2.0");

            let status = process.wait().await.expect("wait for exit");
            assert!(status.success());

            cleanup_script(&script_path);
        });
    }

    #[test]
    fn direct_spawn_uses_transport_env() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let script_path = write_echo_script();
            let transport = crate::mcp_client::McpStdioTransport {
                command: "/bin/sh".to_string(),
                args: vec![script_path.to_string_lossy().into_owned()],
                env: BTreeMap::from([("MCP_TEST_TOKEN".to_string(), "direct-secret".to_string())]),
            };
            let mut process = McpStdioProcess::spawn(&transport).expect("spawn transport directly");
            let ready = process.read_available().await.expect("read ready");
            assert_eq!(String::from_utf8_lossy(&ready), "READY:direct-secret\n");
            process.terminate().await.expect("terminate child");
            let _ = process.wait().await.expect("wait after kill");

            cleanup_script(&script_path);
        });
    }

    #[test]
    fn lists_tools_calls_tool_and_reads_resources_over_jsonrpc() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let script_path = write_mcp_server_script();
            let transport = script_transport(&script_path);
            let mut process = McpStdioProcess::spawn(&transport).expect("spawn fake mcp server");

            let tools = process
                .list_tools(JsonRpcId::Number(2), None)
                .await
                .expect("list tools");
            assert_eq!(tools.error, None);
            assert_eq!(tools.id, JsonRpcId::Number(2));
            assert_eq!(
                tools.result,
                Some(McpListToolsResult {
                    tools: vec![McpTool {
                        name: "echo".to_string(),
                        description: Some("Echoes text".to_string()),
                        input_schema: Some(json!({
                            "type": "object",
                            "properties": {"text": {"type": "string"}},
                            "required": ["text"]
                        })),
                        annotations: None,
                        meta: None,
                    }],
                    next_cursor: None,
                })
            );

            let call = process
                .call_tool(
                    JsonRpcId::String("call-1".to_string()),
                    McpToolCallParams {
                        name: "echo".to_string(),
                        arguments: Some(json!({"text": "hello"})),
                        meta: None,
                    },
                )
                .await
                .expect("call tool");
            assert_eq!(call.error, None);
            let call_result = call.result.expect("tool result");
            assert_eq!(call_result.is_error, Some(false));
            assert_eq!(
                call_result.structured_content,
                Some(json!({"echoed": "hello"}))
            );
            assert_eq!(call_result.content.len(), 1);
            assert_eq!(call_result.content[0].kind, "text");
            assert_eq!(
                call_result.content[0].data.get("text"),
                Some(&json!("echo:hello"))
            );

            let resources = process
                .list_resources(JsonRpcId::Number(3), None)
                .await
                .expect("list resources");
            let resources_result = resources.result.expect("resources result");
            assert_eq!(resources_result.resources.len(), 1);
            assert_eq!(resources_result.resources[0].uri, "file://guide.txt");
            assert_eq!(
                resources_result.resources[0].mime_type.as_deref(),
                Some("text/plain")
            );

            let read = process
                .read_resource(
                    JsonRpcId::Number(4),
                    McpReadResourceParams {
                        uri: "file://guide.txt".to_string(),
                    },
                )
                .await
                .expect("read resource");
            assert_eq!(
                read.result,
                Some(McpReadResourceResult {
                    contents: vec![super::McpResourceContents {
                        uri: "file://guide.txt".to_string(),
                        mime_type: Some("text/plain".to_string()),
                        text: Some("contents for file://guide.txt".to_string()),
                        blob: None,
                        meta: None,
                    }],
                })
            );

            process.terminate().await.expect("terminate child");
            let _ = process.wait().await.expect("wait after kill");
            cleanup_script(&script_path);
        });
    }

    #[test]
    fn surfaces_jsonrpc_errors_from_tool_calls() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let script_path = write_mcp_server_script();
            let transport = script_transport(&script_path);
            let mut process = McpStdioProcess::spawn(&transport).expect("spawn fake mcp server");

            let response = process
                .call_tool(
                    JsonRpcId::Number(9),
                    McpToolCallParams {
                        name: "fail".to_string(),
                        arguments: None,
                        meta: None,
                    },
                )
                .await
                .expect("call tool with error response");

            assert_eq!(response.id, JsonRpcId::Number(9));
            assert!(response.result.is_none());
            assert_eq!(response.error.as_ref().map(|e| e.code), Some(-32001));
            assert_eq!(
                response.error.as_ref().map(|e| e.message.as_str()),
                Some("tool failed")
            );

            process.terminate().await.expect("terminate child");
            let _ = process.wait().await.expect("wait after kill");
            cleanup_script(&script_path);
        });
    }

    #[test]
    fn manager_discovers_tools_from_stdio_config() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let script_path = write_manager_mcp_server_script();
            let root = script_path.parent().expect("script parent");
            let log_path = root.join("alpha.log");
            let servers = BTreeMap::from([(
                "alpha".to_string(),
                manager_server_config(&script_path, "alpha", &log_path),
            )]);
            let mut manager = McpServerManager::from_servers(&servers);

            let tools = manager.discover_tools().await.expect("discover tools");

            assert_eq!(tools.len(), 1);
            assert_eq!(tools[0].server_name, "alpha");
            assert_eq!(tools[0].raw_name, "echo");
            assert_eq!(tools[0].qualified_name, mcp_tool_name("alpha", "echo"));
            assert_eq!(tools[0].tool.name, "echo");
            assert!(
                !tools[0].always_load,
                "default config (alwaysLoad omitted) should yield deferred tools"
            );
            assert!(manager.unsupported_servers().is_empty());

            manager.shutdown().await.expect("shutdown");
            cleanup_script(&script_path);
        });
    }

    // FEAT-41 (Claude Code v2.1.121 parity): a server configured
    // with `alwaysLoad: true` propagates that flag through to every
    // discovered `ManagedMcpTool`.  Consumers (providers.rs) read
    // `ManagedMcpTool::always_load` to decide whether the tool is
    // exposed to the model immediately or kept behind tool-search.
    #[test]
    fn manager_marks_discovered_tools_with_server_always_load_flag() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let script_path = write_manager_mcp_server_script();
            let root = script_path.parent().expect("script parent");
            let hot_log = root.join("hot.log");
            let cold_log = root.join("cold.log");
            let servers = BTreeMap::from([
                (
                    "hot".to_string(),
                    manager_server_config_with_always_load(
                        &script_path,
                        "hot",
                        &hot_log,
                        true,
                    ),
                ),
                (
                    "cold".to_string(),
                    manager_server_config_with_always_load(
                        &script_path,
                        "cold",
                        &cold_log,
                        false,
                    ),
                ),
            ]);
            let mut manager = McpServerManager::from_servers(&servers);

            let tools = manager.discover_tools().await.expect("discover tools");

            let hot = tools
                .iter()
                .find(|tool| tool.server_name == "hot")
                .expect("hot server should yield a tool");
            let cold = tools
                .iter()
                .find(|tool| tool.server_name == "cold")
                .expect("cold server should yield a tool");

            assert!(
                hot.always_load,
                "alwaysLoad: true server's tool must be flagged for immediate availability"
            );
            assert!(
                !cold.always_load,
                "default server's tool must remain deferred (alwaysLoad=false)"
            );

            manager.shutdown().await.expect("shutdown");
            cleanup_script(&script_path);
        });
    }

    #[test]
    fn manager_routes_tool_calls_to_correct_server() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let script_path = write_manager_mcp_server_script();
            let root = script_path.parent().expect("script parent");
            let alpha_log = root.join("alpha.log");
            let beta_log = root.join("beta.log");
            let servers = BTreeMap::from([
                (
                    "alpha".to_string(),
                    manager_server_config(&script_path, "alpha", &alpha_log),
                ),
                (
                    "beta".to_string(),
                    manager_server_config(&script_path, "beta", &beta_log),
                ),
            ]);
            let mut manager = McpServerManager::from_servers(&servers);

            let tools = manager.discover_tools().await.expect("discover tools");
            assert_eq!(tools.len(), 2);

            let alpha = manager
                .call_tool(
                    &mcp_tool_name("alpha", "echo"),
                    Some(json!({"text": "hello"})),
                )
                .await
                .expect("call alpha tool");
            let beta = manager
                .call_tool(
                    &mcp_tool_name("beta", "echo"),
                    Some(json!({"text": "world"})),
                )
                .await
                .expect("call beta tool");

            assert_eq!(
                alpha
                    .result
                    .as_ref()
                    .and_then(|result| result.structured_content.as_ref())
                    .and_then(|value| value.get("server")),
                Some(&json!("alpha"))
            );
            assert_eq!(
                beta.result
                    .as_ref()
                    .and_then(|result| result.structured_content.as_ref())
                    .and_then(|value| value.get("server")),
                Some(&json!("beta"))
            );

            manager.shutdown().await.expect("shutdown");
            cleanup_script(&script_path);
        });
    }

    #[test]
    fn manager_records_unsupported_non_stdio_servers_without_panicking() {
        let servers = BTreeMap::from([
            (
                "http".to_string(),
                ScopedMcpServerConfig {
                    scope: ConfigSource::Local,
                    config: McpServerConfig::Http(McpRemoteServerConfig {
                        url: "https://example.test/mcp".to_string(),
                        headers: BTreeMap::new(),
                        headers_helper: None,
                        oauth: None,
                        always_load: false,
                    }),
                },
            ),
            (
                "sdk".to_string(),
                ScopedMcpServerConfig {
                    scope: ConfigSource::Local,
                    config: McpServerConfig::Sdk(McpSdkServerConfig {
                        name: "sdk-server".to_string(),
                        always_load: false,
                    }),
                },
            ),
            (
                "ws".to_string(),
                ScopedMcpServerConfig {
                    scope: ConfigSource::Local,
                    config: McpServerConfig::Ws(McpWebSocketServerConfig {
                        url: "wss://example.test/mcp".to_string(),
                        headers: BTreeMap::new(),
                        headers_helper: None,
                        always_load: false,
                    }),
                },
            ),
        ]);

        let manager = McpServerManager::from_servers(&servers);
        let unsupported = manager.unsupported_servers();

        assert_eq!(unsupported.len(), 3);
        assert_eq!(unsupported[0].server_name, "http");
        assert_eq!(unsupported[1].server_name, "sdk");
        assert_eq!(unsupported[2].server_name, "ws");
    }

    #[test]
    fn manager_shutdown_terminates_spawned_children_and_is_idempotent() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let script_path = write_manager_mcp_server_script();
            let root = script_path.parent().expect("script parent");
            let log_path = root.join("alpha.log");
            let servers = BTreeMap::from([(
                "alpha".to_string(),
                manager_server_config(&script_path, "alpha", &log_path),
            )]);
            let mut manager = McpServerManager::from_servers(&servers);

            manager.discover_tools().await.expect("discover tools");
            manager.shutdown().await.expect("first shutdown");
            manager.shutdown().await.expect("second shutdown");

            cleanup_script(&script_path);
        });
    }

    #[test]
    fn manager_reuses_spawned_server_between_discovery_and_call() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let script_path = write_manager_mcp_server_script();
            let root = script_path.parent().expect("script parent");
            let log_path = root.join("alpha.log");
            let servers = BTreeMap::from([(
                "alpha".to_string(),
                manager_server_config(&script_path, "alpha", &log_path),
            )]);
            let mut manager = McpServerManager::from_servers(&servers);

            manager.discover_tools().await.expect("discover tools");
            let response = manager
                .call_tool(
                    &mcp_tool_name("alpha", "echo"),
                    Some(json!({"text": "reuse"})),
                )
                .await
                .expect("call tool");

            assert_eq!(
                response
                    .result
                    .as_ref()
                    .and_then(|result| result.structured_content.as_ref())
                    .and_then(|value| value.get("initializeCount")),
                Some(&json!(1))
            );

            let log = fs::read_to_string(&log_path).expect("read log");
            assert_eq!(log.lines().filter(|line| *line == "initialize").count(), 1);
            assert_eq!(
                log.lines().collect::<Vec<_>>(),
                vec!["initialize", "tools/list", "tools/call"]
            );

            manager.shutdown().await.expect("shutdown");
            cleanup_script(&script_path);
        });
    }

    #[test]
    fn manager_reports_unknown_qualified_tool_name() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let script_path = write_manager_mcp_server_script();
            let root = script_path.parent().expect("script parent");
            let log_path = root.join("alpha.log");
            let servers = BTreeMap::from([(
                "alpha".to_string(),
                manager_server_config(&script_path, "alpha", &log_path),
            )]);
            let mut manager = McpServerManager::from_servers(&servers);

            let error = manager
                .call_tool(
                    &mcp_tool_name("alpha", "missing"),
                    Some(json!({"text": "nope"})),
                )
                .await
                .expect_err("unknown qualified tool should fail");

            match error {
                McpServerManagerError::UnknownTool { qualified_name } => {
                    assert_eq!(qualified_name, mcp_tool_name("alpha", "missing"));
                }
                other => panic!("expected unknown tool error, got {other:?}"),
            }

            cleanup_script(&script_path);
        });
    }

    // ── Task #715: MCP pagination regression tests ──────────────────────────
    //
    // CC v2.1.144 (tools/list) and v2.1.146 (resources/list,
    // resources/templates/list, prompts/list) shipped a fix where servers
    // returning a `nextCursor` were having pages beyond the first dropped on
    // the floor. Anvil already had cursor-aware loops for tools/list and
    // resources/list but two gaps remained:
    //   1. An empty-string cursor (`Some("")`) was not recognised as
    //      "no more pages" — the loop would re-request page 1 forever.
    //   2. resources/templates/list and prompts/list had no list handler at
    //      all (no transport method, no server-manager method).
    // The tests below pin both fixes.

    fn write_paginated_mcp_server_script() -> PathBuf {
        let root = temp_dir();
        fs::create_dir_all(&root).expect("temp dir");
        let script_path = root.join("paginated-mcp-server.py");
        let script = [
            "#!/usr/bin/env python3",
            "import json, sys",
            "",
            "def read_message():",
            "    header = b''",
            r"    while not header.endswith(b'\r\n\r\n'):",
            "        chunk = sys.stdin.buffer.read(1)",
            "        if not chunk:",
            "            return None",
            "        header += chunk",
            "    length = 0",
            r"    for line in header.decode().split('\r\n'):",
            r"        if line.lower().startswith('content-length:'):",
            r"            length = int(line.split(':', 1)[1].strip())",
            "    payload = sys.stdin.buffer.read(length)",
            "    return json.loads(payload.decode())",
            "",
            "def send_message(message):",
            "    payload = json.dumps(message).encode()",
            r"    sys.stdout.buffer.write(f'Content-Length: {len(payload)}\r\n\r\n'.encode() + payload)",
            "    sys.stdout.buffer.flush()",
            "",
            "# Pagination scheme:",
            "#   tools/list  : page 1 -> 2 tools + nextCursor='page2'",
            "#                 page 2 -> 2 tools + no cursor (final).",
            "#   resources/templates/list : 1 template/page + nextCursor='page2'",
            "#                              page 2 -> 1 template + no cursor.",
            "#   prompts/list : page 1 -> 1 prompt + nextCursor='page2'",
            "#                  page 2 -> 1 prompt + nextCursor='' (empty,",
            "#                  spec-equivalent to no-more; v2.2.18 task #715).",
            "while True:",
            "    request = read_message()",
            "    if request is None:",
            "        break",
            "    method = request['method']",
            "    params = request.get('params') or {}",
            "    if method == 'initialize':",
            "        send_message({",
            "            'jsonrpc': '2.0',",
            "            'id': request['id'],",
            "            'result': {",
            "                'protocolVersion': params.get('protocolVersion', '2025-03-26'),",
            "                'capabilities': {'tools': {}, 'resources': {}, 'prompts': {}},",
            "                'serverInfo': {'name': 'paginated-mcp', 'version': '1.0.0'}",
            "            }",
            "        })",
            "    elif method == 'tools/list':",
            "        cursor = params.get('cursor')",
            "        if cursor is None:",
            "            tools = [",
            "                {'name': 'alpha', 'inputSchema': {'type': 'object'}},",
            "                {'name': 'bravo', 'inputSchema': {'type': 'object'}},",
            "            ]",
            "            next_cursor = 'page2'",
            "        elif cursor == 'page2':",
            "            tools = [",
            "                {'name': 'charlie', 'inputSchema': {'type': 'object'}},",
            "                {'name': 'delta', 'inputSchema': {'type': 'object'}},",
            "            ]",
            "            next_cursor = None",
            "        else:",
            "            tools = []",
            "            next_cursor = None",
            "        result = {'tools': tools}",
            "        if next_cursor is not None:",
            "            result['nextCursor'] = next_cursor",
            "        send_message({",
            "            'jsonrpc': '2.0',",
            "            'id': request['id'],",
            "            'result': result,",
            "        })",
            "    elif method == 'resources/templates/list':",
            "        cursor = params.get('cursor')",
            "        if cursor is None:",
            "            templates = [{'uriTemplate': 'file://logs/{date}.log', 'name': 'log'}]",
            "            next_cursor = 'page2'",
            "        else:",
            "            templates = [{'uriTemplate': 'file://reports/{kind}.txt', 'name': 'report'}]",
            "            next_cursor = None",
            "        result = {'resourceTemplates': templates}",
            "        if next_cursor is not None:",
            "            result['nextCursor'] = next_cursor",
            "        send_message({",
            "            'jsonrpc': '2.0',",
            "            'id': request['id'],",
            "            'result': result,",
            "        })",
            "    elif method == 'prompts/list':",
            "        cursor = params.get('cursor')",
            "        if cursor is None:",
            "            prompts = [{'name': 'summarise'}]",
            "            next_cursor = 'page2'",
            "        else:",
            "            # Task #715: an empty-string cursor MUST terminate the loop.",
            "            prompts = [{'name': 'translate'}]",
            "            next_cursor = ''",
            "        result = {'prompts': prompts}",
            "        if next_cursor is not None:",
            "            result['nextCursor'] = next_cursor",
            "        send_message({",
            "            'jsonrpc': '2.0',",
            "            'id': request['id'],",
            "            'result': result,",
            "        })",
            "    else:",
            "        send_message({",
            "            'jsonrpc': '2.0',",
            "            'id': request['id'],",
            "            'error': {'code': -32601, 'message': f'unknown method: {method}'},",
            "        })",
            "",
        ]
        .join("\n");
        fs::write(&script_path, script).expect("write script");
        let mut permissions = fs::metadata(&script_path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("chmod");
        script_path
    }

    fn paginated_server_config(script_path: &Path) -> ScopedMcpServerConfig {
        ScopedMcpServerConfig {
            scope: ConfigSource::Local,
            config: McpServerConfig::Stdio(McpStdioServerConfig {
                command: python_command(),
                args: vec![script_path.to_string_lossy().into_owned()],
                env: BTreeMap::new(),
                always_load: false,
            }),
        }
    }

    #[test]
    fn manager_discover_tools_concatenates_all_paginated_pages() {
        // Spec: an MCP server that splits its tools across two
        // `tools/list` responses (page 1 returns `nextCursor: "page2"`,
        // page 2 returns no cursor) MUST surface ALL four tools to the
        // registry. Before task #715 only page 1 was consumed.
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let script_path = write_paginated_mcp_server_script();
            let servers = BTreeMap::from([(
                "paginated".to_string(),
                paginated_server_config(&script_path),
            )]);
            let mut manager = McpServerManager::from_servers(&servers);

            let tools = manager.discover_tools().await.expect("discover tools");
            let names: Vec<String> = tools.iter().map(|t| t.raw_name.clone()).collect();

            assert_eq!(
                names,
                vec![
                    "alpha".to_string(),
                    "bravo".to_string(),
                    "charlie".to_string(),
                    "delta".to_string(),
                ],
                "all four tools across both pages must be discovered"
            );

            manager.shutdown().await.expect("shutdown");
            cleanup_script(&script_path);
        });
    }

    #[test]
    fn manager_list_resource_templates_paginates_until_cursor_exhausted() {
        // Spec: `resources/templates/list` (added in task #715) follows
        // the same cursor protocol as tools/list. Two pages, one
        // template each, must concatenate.
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let script_path = write_paginated_mcp_server_script();
            let servers = BTreeMap::from([(
                "paginated".to_string(),
                paginated_server_config(&script_path),
            )]);
            let mut manager = McpServerManager::from_servers(&servers);

            let templates = manager
                .list_resource_templates("paginated")
                .await
                .expect("list templates");
            let uris: Vec<String> = templates
                .iter()
                .map(|t| t.uri_template.clone())
                .collect();

            assert_eq!(
                uris,
                vec![
                    "file://logs/{date}.log".to_string(),
                    "file://reports/{kind}.txt".to_string(),
                ],
                "templates from both pages must be returned"
            );

            manager.shutdown().await.expect("shutdown");
            cleanup_script(&script_path);
        });
    }

    #[test]
    fn manager_list_prompts_terminates_on_empty_string_cursor() {
        // Spec: per MCP spec a server may signal "no more pages" by
        // returning `nextCursor: ""` instead of omitting the field.
        // Before task #715 the loop only broke on `None` and would
        // re-request page 2 forever. This test ensures BOTH the empty
        // cursor terminates AND that both prompts (page 1 + page 2)
        // are returned.
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let script_path = write_paginated_mcp_server_script();
            let servers = BTreeMap::from([(
                "paginated".to_string(),
                paginated_server_config(&script_path),
            )]);
            let mut manager = McpServerManager::from_servers(&servers);

            let prompts = manager.list_prompts("paginated").await.expect("list prompts");
            let names: Vec<String> = prompts.iter().map(|p| p.name.clone()).collect();

            assert_eq!(
                names,
                vec!["summarise".to_string(), "translate".to_string()],
                "both prompts must be returned and the empty-string cursor MUST terminate"
            );

            manager.shutdown().await.expect("shutdown");
            cleanup_script(&script_path);
        });
    }

    #[test]
    fn advance_pagination_cursor_treats_empty_and_whitespace_as_exhausted() {
        use super::advance_pagination_cursor;
        assert_eq!(advance_pagination_cursor(None), None);
        assert_eq!(advance_pagination_cursor(Some(String::new())), None);
        assert_eq!(advance_pagination_cursor(Some("   ".to_string())), None);
        assert_eq!(
            advance_pagination_cursor(Some("page2".to_string())),
            Some("page2".to_string())
        );
        // Non-empty cursors with surrounding whitespace pass through
        // unchanged (we don't trim mid-cursor — that would corrupt opaque
        // tokens).
        assert_eq!(
            advance_pagination_cursor(Some("  page2".to_string())),
            Some("  page2".to_string())
        );
    }

    // CC-DRIFT-B8 contract test: empty-string values inside MCP tool
    // arguments must survive serialization with the key intact. Pinned here
    // to lock the contract against future serde-attr drift on
    // McpToolCallParams::arguments.
    #[test]
    fn empty_string_arg_value_survives_serialization() {
        let params = McpToolCallParams {
            name: "echo".to_string(),
            arguments: Some(json!({ "text": "" })),
            meta: None,
        };

        let wire = serde_json::to_value(&params).expect("serialize");
        let args = wire
            .get("arguments")
            .expect("arguments key must be present on wire");
        let text = args
            .get("text")
            .expect("text key must be preserved with empty value");
        assert_eq!(text, &json!(""), "empty-string value must round-trip");
        assert!(text.is_string(), "value must remain a string, not null");
    }
}
