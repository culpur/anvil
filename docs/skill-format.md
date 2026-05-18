# SKILL.md Format

Every Anvil skill is a directory with a `SKILL.md` file at its root.
The file starts with a YAML frontmatter block (fenced by `---` lines)
followed by free-form Markdown that contains the skill's prompt body.

```text
my-skill/
  SKILL.md        ← frontmatter + body (required)
  resources/      ← arbitrary supporting files (optional)
```

Skills are discovered from these roots, in order of precedence:

1. `.codex/skills/` (project)
2. `.anvil/skills/` (project)
3. `$CODEX_HOME/skills/` (user)
4. `~/.codex/skills/` (user)
5. `~/.anvil/skills/` (user)
6. Bundled skills (shipped inside the Anvil binary)

The first occurrence of a skill name wins; later ones are shown as
shadowed in `/skills list`.

## Frontmatter schema

| Key           | Required | Default | Notes                                              |
| ------------- | -------- | ------- | -------------------------------------------------- |
| `name`        | yes      | —       | Used as `/skill <name>` invocation key.            |
| `description` | no       | `null`  | One-line summary shown in `/skills list`.          |
| `triggers`    | no       | `[]`    | Keywords / phrases that auto-invoke the skill.     |
| `chains_to`   | no       | `[]`    | Skills to invoke when this one completes.          |
| `inputs`      | no       | `[]`    | Slot list — input handles for the chain builder.   |
| `outputs`     | no       | `[]`    | Slot list — output handles for the chain builder.  |

`triggers` and `chains_to` are documented in the skill-triggers and
skill-chaining design notes respectively. The remainder of this page
focuses on `inputs` / `outputs`, added in v2.2.17 (task #529, sub-track C).

## Inputs and outputs

`inputs:` and `outputs:` declare the data slots a skill exposes. The
v2.2.17 skill-chain builder (React Flow canvas, sub-track D) uses them to
render valid handles on each node so the user can wire one skill's output
into another skill's input.

Each slot is a YAML mapping:

| Slot key      | Required | Default | Notes                                                  |
| ------------- | -------- | ------- | ------------------------------------------------------ |
| `name`        | yes      | —       | Must match `^[a-z][a-z0-9_]*$`, max 32 chars.          |
| `kind`        | no       | `text`  | One of: `text`, `file`, `json`, `image`, `boolean`.    |
| `description` | no       | `null`  | One-line description shown in the builder UI tooltip.  |
| `required`    | no       | `true`  | Whether the slot must be wired before the chain runs.  |

### Validation rules

* Slot names that fail the regex or exceed the length budget are silently
  dropped — the rest of the slot list still parses.
* Duplicate names **within the same list** drop later occurrences;
  cross-list duplicates (an input and output sharing a name) are allowed.
* An unknown / unparseable `kind:` value falls back to `text`.
* An unparseable `required:` value falls back to `true`.

### Backwards compatibility

Skills that omit `inputs:` and `outputs:` continue to work exactly as
before — the chain builder simply renders them as nodes with no
typed handles, and they remain invocable via `/skill <name>`.

## Examples

### Example 1 — text inputs, JSON output

```yaml
---
name: vulnapi-scanner
description: Scans an OpenAPI endpoint for common OWASP issues.
inputs:
  - name: target_url
    kind: text
    description: "API endpoint to scan"
    required: true
  - name: auth_token
    kind: text
    required: false
outputs:
  - name: report
    kind: json
    description: "OWASP findings"
---

# vulnapi-scanner

You receive a `target_url` and optional `auth_token`. Probe the endpoint
for common OWASP issues and return a structured `report`.
```

### Example 2 — file input, multiple typed outputs

```yaml
---
name: code-review
description: Reviews a source file and emits diagnostics + a summary.
inputs:
  - name: source_file
    kind: file
    description: "Path to the file under review"
    required: true
  - name: style_guide
    kind: file
    required: false
outputs:
  - name: diagnostics
    kind: json
    description: "Per-line findings"
  - name: summary
    kind: text
    description: "Reviewer summary"
  - name: passes_review
    kind: boolean
---

# code-review

Read `source_file` (honouring `style_guide` when supplied), produce a
list of diagnostics, a short summary, and a pass/fail boolean.
```

## Where the parser lives

* **Anvil (Rust)**: `crates/commands/src/agents.rs::parse_skill_frontmatter`
  — canonical schema. Returns a `SkillFrontmatter` with `inputs:
  Vec<SkillSlot>` and `outputs: Vec<SkillSlot>`.
* **passage (TypeScript)**: `src/utils/skillManifest.ts::parseSkillManifest`
  — mirror used by `GET /v1/hub/packages/:slug` to surface slots into
  `manifest.inputs` / `manifest.outputs` for the /builder UI.

Both parsers share the same validation semantics. When in doubt, the
Rust implementation is the source of truth.
