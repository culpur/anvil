# MCP Builder — Reference-MCP audit + template spec

Task: #638 (v2.2.18-install-arc) — ship a wizard / slash command that
scaffolds a new MCP server from scratch in Node.js or Python, with the
generated project building cleanly out of the box and registering with
Anvil automatically.

This audit is the prerequisite: survey the 4 reference MCPs under
`~/projects/mcp-servers/`, extract conventions, note drift, and pin
down the template spec the scaffolder will emit.

---

## 1. Reference MCPs surveyed

| MCP             | Lines (index.js) | SDK API style          | Schema lib   | Test runner            | Tests |
| --------------- | ---------------- | ---------------------- | ------------ | ---------------------- | ----- |
| anvil-release   | 258              | low-level `Server`     | JSON Schema  | `node --test`          | yes   |
| culpur-infra    | 761              | high-level `McpServer` | zod          | `node --test`          | yes   |
| wordpress       | 982              | low-level `Server`     | JSON Schema  | `node --test`          | yes   |
| safe-edit       | 395 (library)    | none — library         | n/a          | `node --test`          | yes   |

`safe-edit` is a sibling library (not an MCP), consumed by
`anvil-release` and `wordpress` via `file:` workspace link. The MCP
builder does NOT need to reproduce safe-edit — it's a hardening tool
specific to config-file edits in the Culpur infra, not a general MCP
primitive.

Every project is:
- ESM (`"type": "module"`)
- private (`"private": true`)
- `node index.js` startable
- stdio-transport
- single entrypoint file (plus optional `lib/` split)
- `@modelcontextprotocol/sdk` pinned at `^1.0.0`

---

## 2. Convention drift between the four references

Two patterns coexist:

### 2a. Low-level `Server` + JSON Schema (anvil-release, wordpress)

```js
import { Server } from '@modelcontextprotocol/sdk/server/index.js';
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';
import {
  CallToolRequestSchema,
  ListToolsRequestSchema,
} from '@modelcontextprotocol/sdk/types.js';

const TOOLS = [ { name, description, inputSchema: { type: 'object', properties: {…} } }, … ];
const server = new Server({ name, version }, { capabilities: { tools: {} } });
server.setRequestHandler(ListToolsRequestSchema, async () => ({ tools: TOOLS }));
server.setRequestHandler(CallToolRequestSchema, async (request) => {
  const { name, arguments: args = {} } = request.params;
  switch (name) { case '…': … }
});
await server.connect(new StdioServerTransport());
```

### 2b. High-level `McpServer` + zod (culpur-infra)

```js
import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js';
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';
import { z } from 'zod';

const server = new McpServer({ name, version });
server.tool(
  'culpur_ssh',
  'Description…',
  { host: z.enum(['dev0001','web','guard']), command: z.string().min(1) },
  async ({ host, command }) => ({ content: [{ type: 'text', text: '…' }] }),
);
await server.connect(new StdioServerTransport());
```

### 2c. Resolution

Both are first-class in the SDK. The high-level path is significantly
shorter for the common case (4–8 tools, each a small async fn) and avoids
the `switch (name)` dispatch the user has to hand-write. **The
scaffolder emits the high-level `McpServer` + zod pattern** because:

1. Less boilerplate per tool = less surface for the user to mis-edit.
2. zod schemas double as input validation at runtime; JSON Schema with the
   low-level API is only a description, not enforcement.
3. culpur-infra is the most recent of the three (1.1.0 vs anvil-release at
   2.0.0 and wordpress at 1.0.0 with older patterns frozen in for
   safe-edit compatibility).

The audit doc itself does NOT prescribe rewriting the reference MCPs —
they're out of scope (instructed by user).

---

## 3. Honesty contract — observed in anvil-release

All anvil-release tools return:

```js
{
  ok: boolean,            // overall success
  blockers: string[],     // gating failures
  warnings: string[],     // non-fatal observations
  subresults: { … },      // per-sub-step result map
  message: string,        // human one-liner
  // …extra tool-specific fields
}
```

The scaffolder bakes this shape into the **default tool implementation
stub** for both Node.js and Python paths. New-MCP authors get a working
example of the honesty contract for free.

---

## 4. Anvil registration mechanism

Anvil discovers MCP servers by reading `~/.anvil/settings.json`:

```json
{
  "mcpServers": {
    "<name>": {
      "command": "<absolute path to node|python|binary>",
      "args": ["<entry script>"]
    }
  }
}
```

`/mcp list` enumerates `mcpServers.{keys}` (see
`crates/anvil-cli/src/commands_extra.rs::run_mcp_command`). The
scaffolder must:
- write the new MCP into `~/.anvil/settings.json::mcpServers.<name>`
- match the existing precedent set by `wizard_qmd.rs::wire_qmd_into_settings`
- be idempotent (overwrite the entry for the same name, leave others)

---

## 5. Python — greenfield (no existing reference MCP)

All four reference MCPs are Node.js. There is no Python MCP in the
repo. The Python scaffold ships with no precedent inside this repo
but the upstream MCP Python SDK is `mcp` (PyPI), idiomatic pattern is:

```python
from mcp.server.fastmcp import FastMCP
mcp = FastMCP("<name>")

@mcp.tool()
def my_tool(arg: str) -> str:
    """Tool description."""
    return f"result: {arg}"

if __name__ == "__main__":
    mcp.run()
```

This is the **Python equivalent of `McpServer` + zod**: decorator-based
tool registration, Python type hints driving the schema. The user's
brief explicitly locks BOTH Node and Python as equal-feature for
v2.2.18 — we ship the FastMCP path.

---

## 6. Template spec (locked)

The scaffolder is a pure function:

```rust
pub fn scaffold_files(spec: &McpBuilderSpec) -> Vec<(PathBuf, String)>
```

It returns the in-memory file list for the new project. A separate
`write_scaffold_to_disk` step actually hits disk. This split is
required for unit-testing — see task brief §4 ("pure scaffolder").

### 6a. Node.js scaffold output

```
<output_dir>/
  index.js          // McpServer + zod + tool stubs
  package.json      // pinned SDK + zod + node --test script
  test.js           // 1 test per tool, calling the tool fn directly
  .gitignore        // node_modules, .env
  README.md         // short one-pager
```

### 6b. Python scaffold output

```
<output_dir>/
  __main__.py       // FastMCP + decorator stubs
  pyproject.toml    // mcp[cli] pinned, pytest, package metadata
  test_server.py    // 1 test per tool with pytest
  .gitignore        // .venv, __pycache__, dist
  README.md         // short one-pager
```

### 6c. Per-tool: spec → emitted code

`McpToolSpec { name, description, inputs: Vec<McpToolInput>, output_kind }`
maps onto:

| Field | Node | Python |
| ----- | ---- | ------ |
| name        | `server.tool('name', …)` | `@mcp.tool(name="name")` |
| description | 2nd arg | docstring on the function |
| inputs      | zod object | typed function params |
| output_kind | text vs json honesty-contract template | same |

Input types supported in v2.2.18:
- `string`, `number`, `boolean` (required + optional)
- `enum` of strings

`array`, `object`, and nested schemas are out of scope for the first
slice. The audit doc calls this out explicitly so a follow-up task can
extend the input-types list without re-architecting.

---

## 7. Name validation

Per task brief: kebab-case, no clash with installed MCPs.

Rules:
- `^[a-z][a-z0-9]*(-[a-z0-9]+)*$`
- 3-32 chars
- Not in `~/.anvil/settings.json::mcpServers.keys`
- Not in a hard-coded reserved list: `["anvil", "mcp", "test", "server"]`

If validation fails the wizard re-opens the name prompt with an inline
hint. No silent overwrites of existing MCP entries.

---

## 8. Where the wizard lives (slash-command path)

`/mcp builder` is added as a sub-action of the existing
`SlashCommand::Mcp { action: Option<String> }`. The variant already
exists — no enum change. The drift gate (`slash_command_specs().len()
== 122`) is unaffected; only `MCP_SUBCOMMANDS` grows by one row.

Invocation flow from inside the TUI:
1. User types `/mcp builder`.
2. `run_mcp_command(Some("builder"))` matches the new arm.
3. Arm calls `leave_alt_screen_for_inline_op()` (existing pattern
   shared with `/provider ollama` — see
   `cmd_provider.rs::560-583`).
4. The scaffolder runs in plain stdio (alt-screen left). println is
   safe here because the alt-screen is no longer active —
   matches `feedback-tui-stdout-anti-pattern`'s exception clause.
5. On return: `restore_alt_screen()` and the slash command returns a
   one-line status string for the chat scrollback.

Headless invocation (`anvil --print /mcp builder`) follows the same
code path with no alt-screen involvement at all.

---

## 9. 8-axis capability contract for `/mcp builder`

| Axis            | Site                                                            | Status |
| --------------- | --------------------------------------------------------------- | ------ |
| 1. Definition   | `SlashCommand::Mcp { action }` (reuses existing variant)        | exists |
| 2. Registration | `crates/commands/src/lib.rs::from_str_internal` (already wires) | exists |
| 3. Completion   | `MCP_SUBCOMMANDS` row for `builder`                             | NEW    |
| 4. Handler      | `run_mcp_command` arm + new module `mcp_builder.rs`             | NEW    |
| 5. Dispatch     | `main.rs::SlashCommand::Mcp { action }` (reused)                | exists |
| 6. Rendering    | inline subprocess prompts after alt-screen leave                | NEW    |
| 7. Gate         | name validation + alt-screen leave/restore guard                | NEW    |
| 8. OTel + tests | scaffolder unit tests + integration test for generated Node MCP | NEW    |

---

## 10. Deferrals (explicit, per feedback-no-silent-deferral)

NOTHING from the task brief's locked scope is deferred. Both Node and
Python ship in the first slice with:
- working scaffold
- working tests on the generated project (Node `node --test`, Python
  `pytest`)
- auto-registration into `~/.anvil/settings.json`

Explicit out-of-scope items for v2.2.18 (call them out so the next
task picks them up):
- `array` / `object` / nested input schemas in the tool builder
- Multi-file `lib/` split in the scaffolded project (the
  anvil-release pattern); the scaffolder emits a single
  `index.js` / `__main__.py` and the user can split themselves
- Auto-install (`npm install` / `pip install`) is best-effort — if
  it fails the scaffolder still writes the files and prints the
  manual install command. Registering with Anvil happens regardless
- TypeScript template (the four reference MCPs are plain JS; TS would
  be a new convention to land in a follow-up)

---

## 11. Test plan

Unit tests live in `crates/anvil-cli/src/mcp_builder.rs::tests`:
- name validation (kebab-case, length, reserved, clash)
- `scaffold_files` for Node and Python: every emitted file is
  non-empty, file count matches the spec
- Idempotent settings.json wiring

Integration test (gated under `#[ignore]` so CI doesn't need `node` on
PATH, runnable locally with `cargo test -p anvil-cli -- --ignored
generated_node_mcp_starts`):
- Run `scaffold_files` for a 1-tool Node MCP into a temp dir
- Write to disk
- `node --test test.js` on the result — must pass
- Spawn `node index.js`, send `{"method":"tools/list"}` over stdin,
  assert the tool we asked for appears

Python integration test is symmetric (`pytest test_server.py` +
`python -m <name>` MCP smoke). It's similarly `#[ignore]`d because
`python3` and `pip install mcp` is not a build-bot guarantee.

---

## 12. Closing note on infra leakage

The generated MCP templates must contain ZERO Culpur-specific
hostnames, IPs, ports, usernames, or SDK tokens. The reference MCPs
have legitimate hard-coded infra (the Cloudflare token in
wordpress/index.js:29, BASTION + dev0001 in culpur-infra) — the
scaffolder does NOT carry those over. Every template emits a clean
"hello world" tool implementation with no internal data baked in.
