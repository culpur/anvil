# WebUI `/ssh` Feature-Parity Design Spec

Task #706, doc 3 of 3. Target release: v2.2.19. Author: webui-ssh
audit agent #1. Sister docs: `ssh-tui-audit.md`,
`ssh-relay-protocol-current.md`.

Goal: make `/ssh` in the passage viewer (`viewer.html`) work the same
way it does in the TUI — terminal tab, form modal, host picker, key
picker — with the constraint that **the vault and the SSH socket
both stay on the TUI host**. The viewer is a thin renderer + input
forwarder over WSS.

This spec is **implementation-ready**. Two follow-on agents will
implement: agent #2 (relay protocol + anvil-cli SSH forwarder), agent
#3 (viewer.html UI).

---

## 0. Non-goals (out-of-scope, deferred)

Explicitly **not** in v2.2.19:

- SFTP / SCP file transfer (deferred to v2.2.20+).
- SSH port forwarding (`-L` / `-R` / `-D`). Not in the TUI either.
- Multi-hop / jump host config (`ProxyJump`). Not in the TUI either.
- Multi-pane / tmux-style splits inside a single SSH tab.
- SSH agent forwarding (`-A`). Security review required first;
  separate task.
- Browser-side keyboard-interactive (2FA/OTP) UI. Driver supports it,
  bridge drops it. Add when we add it for the TUI.
- TOFU `known_hosts` host-key fingerprint UI. Inherits the TUI's
  no-op `check_server_key`; v2.3 follow-up.

---

## 1. Architecture summary

```
  Browser (viewer.html)              Passage relay                 anvil-cli (TUI host)
  ─────────────────────              ──────────────                ─────────────────────
   xterm.js instance(s)              content-neutral                vt100::Parser per SSH tab
   per SSH tab                       JSON forwarder                 russh client (existing)
        │                                  │                              │
        │  ssh.terminal_input (b64)        │                              │
        ├─────────────────────►            ├─────────────────────►        ├─► stdin pump → russh
        │                                  │                              │
        │  ssh.terminal_data (b64)         │                              │
        │ ◄─────────────────────           │ ◄─────────────────────       ◄── stdout pump from russh
        │                                  │                              │
        │  ssh.form_submit / ssh.connect   │                              │
        ├─────────────────────►            ├─────────────────────►        ├─► spawns SshTabState
        │                                  │  (rate-limited: 5/min/sess)  │
        │                                  │                              │
        │  ssh.connection_status           │                              │
        │ ◄─────────────────────           │ ◄─────────────────────       ◄── from SshConnState transitions
```

Key invariant: **the SSH TCP socket lives in the anvil-cli process.**
The browser sees only encrypted-over-WSS bytes that have already been
decrypted by russh on the host. The credential never traverses the
relay in any persistent form — passwords/passphrases ride a single
`ssh.connect` message and are zeroized on the host after russh
consumes them.

---

## 2. New relay message types (wire protocol additions)

All new tags are added to `RelayMessage`
(`crates/runtime/src/relay.rs`) and `KNOWN_RELAY_TAGS`. Drift gate
test enforces wire-tag/variant parity. JSON snake_case. Eleven new
variants; total wire tags goes 53 → 64.

### 2.1 Host → Web

```rust
// Host → Web: viewer should open the SSH form modal.
SshFormRequest { tab_id: usize },

// Host → Web: terminal bytes ready (base64 of raw bytes the russh
// driver delivered for the active SSH tab on the host).
SshTerminalData {
    tab_id: usize,
    /// base64(standard, no padding) of the byte chunk.
    data_b64: String,
    /// Monotonic per-tab sequence number for flow-control + dedup.
    /// Starts at 0; viewer can detect gaps but does not have to
    /// react (transport is in-order over a single WS).
    seq: u64,
},

// Host → Web: connection lifecycle transition.
SshConnectionStatus {
    tab_id: usize,
    /// One of "connecting" | "auth" | "connected" | "auth_failed"
    ///         | "disconnected" | "error"
    status: String,
    /// Free-form detail; auth method name during "auth", reason
    /// during "auth_failed" / "disconnected" / "error", empty otherwise.
    detail: String,
},

// Host → Web: list of vault-stored SSH aliases (response to ssh.list_aliases).
SshAliasList {
    aliases: Vec<SshAliasEntry>,
},

// Host → Web: list of usable private-key files (~/.ssh, response to ssh.list_keys).
SshKeyList {
    /// Bare filenames suitable for the picker (collapse_key_path applied).
    names: Vec<String>,
},
```

```rust
/// Mirrors what the TUI's host picker would show.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshAliasEntry {
    pub label: String,         // user-picked alias name
    pub host: String,
    pub port: u16,
    pub user: String,
    /// "agent" | "key" | "password" | "interactive"
    pub ssh_auth: String,
}
```

### 2.2 Web → Host

```rust
// Web → Host: viewer wants the saved-alias list (will reply with SshAliasList).
SshListAliases,

// Web → Host: viewer wants the ~/.ssh key enumeration (reply: SshKeyList).
SshListKeys,

// Web → Host: connect to a host. EITHER use_alias OR all of (host, port,
// user, auth) are provided; mixing is an error.
SshConnect {
    /// Vault alias label, if connecting by saved name.
    #[serde(skip_serializing_if = "Option::is_none")]
    use_alias: Option<String>,
    /// Ad-hoc form fields. Only honoured when use_alias is None.
    #[serde(skip_serializing_if = "Option::is_none")]
    host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<String>,
    /// "agent" | "key" | "password"
    #[serde(skip_serializing_if = "Option::is_none")]
    auth: Option<String>,
    /// For "key" auth: filename under ~/.ssh OR an absolute path.
    #[serde(skip_serializing_if = "Option::is_none")]
    key_path: Option<String>,
    /// For "password" or "key" auth (passphrase): the one-time secret.
    /// Host zeroizes this string after handing it to russh.
    #[serde(skip_serializing_if = "Option::is_none")]
    secret: Option<String>,
    /// Initial PTY dimensions from the browser.
    cols: u32,
    rows: u32,
    /// Optional vault alias to save the credential under after a
    /// successful connect (only honoured for non-agent auth, since
    /// agent has no material to persist).
    #[serde(skip_serializing_if = "Option::is_none")]
    save_alias: Option<String>,
},

// Web → Host: typed bytes from the viewer's xterm.js (base64).
SshTerminalInput {
    tab_id: usize,
    /// base64(standard, no padding).
    data_b64: String,
},

// Web → Host: viewer pane resized; propagate to PTY window-change.
SshTerminalResize {
    tab_id: usize,
    cols: u32,
    rows: u32,
},

// Web → Host: tear down the SSH tab (graceful close).
SshDisconnect { tab_id: usize },
```

### 2.3 TypeScript mirror (viewer-side)

```ts
type SshAliasEntry = {
  label: string;
  host: string;
  port: number;
  user: string;
  ssh_auth: "agent" | "key" | "password" | "interactive";
};

type SshRelayHostToWeb =
  | { type: "ssh_form_request"; tab_id: number }
  | { type: "ssh_terminal_data"; tab_id: number; data_b64: string; seq: number }
  | { type: "ssh_connection_status"; tab_id: number; status: "connecting"|"auth"|"connected"|"auth_failed"|"disconnected"|"error"; detail: string }
  | { type: "ssh_alias_list"; aliases: SshAliasEntry[] }
  | { type: "ssh_key_list"; names: string[] };

type SshRelayWebToHost =
  | { type: "ssh_list_aliases" }
  | { type: "ssh_list_keys" }
  | { type: "ssh_connect";
      use_alias?: string;
      host?: string; port?: number; user?: string;
      auth?: "agent"|"key"|"password";
      key_path?: string; secret?: string;
      cols: number; rows: number;
      save_alias?: string }
  | { type: "ssh_terminal_input"; tab_id: number; data_b64: string }
  | { type: "ssh_terminal_resize"; tab_id: number; cols: number; rows: number }
  | { type: "ssh_disconnect"; tab_id: number };
```

### 2.4 Wire-tag table additions to `KNOWN_RELAY_TAGS`

```rust
("ssh_form_request",        RelayDirection::HostToWeb),
("ssh_terminal_data",       RelayDirection::HostToWeb),
("ssh_connection_status",   RelayDirection::HostToWeb),
("ssh_alias_list",          RelayDirection::HostToWeb),
("ssh_key_list",            RelayDirection::HostToWeb),
("ssh_list_aliases",        RelayDirection::WebToHost),
("ssh_list_keys",           RelayDirection::WebToHost),
("ssh_connect",             RelayDirection::WebToHost),
("ssh_terminal_input",      RelayDirection::WebToHost),
("ssh_terminal_resize",     RelayDirection::WebToHost),
("ssh_disconnect",          RelayDirection::WebToHost),
```

### 2.5 Tab carry-over (no new TabKind)

The existing `tab_opened` already carries `{ tab_id, name, model,
session_id }`. We reuse it for SSH tabs with:

- `name`: `"ssh:user@host:port"` (matches TUI naming).
- `model`: `"ssh"` (sentinel string the viewer dispatches on — see
  section 3.1).
- `session_id`: `""`.

The viewer detects `model === "ssh"` on `tab_opened` and instantiates
an xterm.js inside that tab's content area instead of the chat layout.

---

## 3. Viewer.html UI elements

Single-file viewer (existing convention). All additions stay inside
`viewer.html`. Targets ~600 LOC across CSS / JS / HTML / xterm.js
inlined/CDN'd.

### 3.1 Tab routing on `model === "ssh"`

In `handleMessage`'s `tab_opened` case (`viewer.html:888`), branch on
`msg.model === "ssh"`:

```js
case 'tab_opened':
  tabs[msg.tab_id] = {
    name: msg.name, model: msg.model || '', session_id: msg.session_id || '',
    log: [], streaming: '', tokens: {input:0, output:0},
    // SSH-specific:
    ssh: msg.model === 'ssh' ? { term: null, seqLast: -1, status: 'connecting' } : null,
  };
  if (Object.keys(tabs).length === 1) activeTab = msg.tab_id;
  renderTabBar(); renderSecondaryTabBars(); updateStatusBar();
  if (tabs[msg.tab_id].ssh) attachXtermToTab(msg.tab_id);
  break;
```

`attachXtermToTab(tab_id)` mounts an xterm.js terminal into the tab's
content container, wires the `onData` callback to send
`ssh_terminal_input`, and wires the resize observer to send
`ssh_terminal_resize`.

`renderMessages()` (the chat-mode renderer) is a no-op for SSH tabs;
the xterm.js DOM owns the pane.

### 3.2 xterm.js choice

- **Version: `xterm` 5.5.x** (latest stable as of 2026-05).
- **Renderer: WebGL** with DOM fallback (`@xterm/addon-webgl`). WebGL
  is the only viable choice for sustained vt100 throughput (cat large
  file, `htop`, vim) on modest hardware. DOM fallback for the rare
  browser that lacks WebGL.
- **Addons:**
  - `@xterm/addon-fit` — auto-resize on container change.
  - `@xterm/addon-webgl` — WebGL renderer.
  - `@xterm/addon-web-links` — clickable URLs in terminal output.
  - NO `@xterm/addon-search` in v2.2.19 (defer).
- **Local echo: OFF** — SSH server already echoes; double echo is the
  classic xterm.js bug.
- **Scrollback: 10_000 lines** — matches industry default; vs TUI's
  1_000-line `vt100::Parser` setup (TUI is RAM-constrained per
  process, browser is not).
- **Theme:** match the existing viewer.html dark palette
  (background `#0d1117`, foreground `#c9d1d9`, ANSI 16 colors taken
  from the TUI's `tui::theme` defaults). Exact hex table in section
  4.4 below.
- **Load mechanism:** vendored from `node_modules/xterm` at build time
  into a single `<script>` block inside `viewer.html` (the
  single-file viewer convention prevents external CDN deps from
  blocking on the corporate proxy / CSP issues). Total cost: ~250 KB
  including WebGL addon — acceptable for a once-loaded viewer.

### 3.3 SSH form modal

Triggered by:

- viewer-side: typing `/ssh` in any input box (the existing
  `sendMessage` / `dispatchSlash` path detects `/ssh` and opens the
  modal locally — does NOT round-trip through the host); OR
- host-side: `ssh_form_request` event (e.g. the user typed `/ssh`
  in the TUI input but the viewer happens to be the active "front
  end" — speculative, low-priority).

Modal layout (mirrors `ssh_form::SshFormState`):

```
┌─ SSH Connect ─────────────────────────────────────┐
│                                                    │
│  Host:    [____________________]                   │
│  Port:    [22__]                                   │
│  User:    [____________________]                   │
│                                                    │
│  Auth:    ( ) Agent  ( ) Key file  (●) Password    │
│                                                    │
│  Key:     [____________________] [Browse…]         │   ← only when Key
│  Secret:  [••••••••••••••••••]                     │   ← only when Key/Password
│                                                    │
│  Save as alias (optional): [_______________]       │
│                                                    │
│  Or pick a saved host:                             │
│    ▾ guard (soulofall@guard.example:30022 / agent) │   ← dropdown
│                                                    │
│         [ Cancel ]            [ Connect ]          │
└────────────────────────────────────────────────────┘
```

Form behaviour:

- All input fields: `autocomplete="off"` AND `data-lpignore="true"`
  AND `data-form-type="other"` AND `data-1p-ignore="true"` AND
  `data-bwignore="true"` — defeats LastPass / 1Password / Bitwarden
  autofill (security boundary 4.2).
- The "Secret" field is `<input type="password">` (browser dot-mask).
- The "Or pick a saved host" dropdown is populated by sending
  `ssh_list_aliases` when the modal opens; renders entries as
  "label (user@host:port / auth)". Selecting an entry sets
  `use_alias` and disables all manual fields except "Save as alias".
- A "🔒 Vault locked — unlock to use saved hosts" banner replaces the
  dropdown when `vault_state.locked === true`. Clicking it opens the
  existing viewer vault-unlock modal.
- "Browse" opens a sub-modal listing the keys returned by
  `ssh_list_keys` — filter-as-you-type, Enter to select, Esc to back.
- Submit validates client-side (host non-empty, port 1–65535, user
  non-empty, auth-method specifics) then sends `ssh_connect` with all
  populated fields.

### 3.4 Host picker dropdown

When the modal opens it dispatches `ssh_list_aliases`. The host
responds with `ssh_alias_list`. Each entry renders one line:

```
  guard            soulofall@guard.example.net:30022   [agent]
  dev0001          maverick@10.0.70.80:22              [key]
  jumpbox          root@bastion.culpur.net:30022       [password]
```

Selecting an entry:

1. Disables all manual form fields.
2. Sets a `use_alias` state.
3. The "Save as alias" field stays editable so the user can
   "re-save" under a new label.
4. The "Connect" button stays enabled.

### 3.5 Key-file picker (the `[Browse]` sub-modal)

- Triggered by clicking `[Browse]` OR `Ctrl+F` while focus is in the
  modal AND auth=Key.
- Sends `ssh_list_keys`. Host responds with `ssh_key_list { names }`.
- Renders a vertical list inside a smaller sub-modal:
  ```
  ┌─ Browse ~/.ssh ────────────────┐
  │ Filter: __                      │
  │                                 │
  │ > id_ed25519                    │
  │   id_rsa                        │
  │   culpur_key                    │
  │   work_key                      │
  │                                 │
  │ Enter=select  Esc=back          │
  └─────────────────────────────────┘
  ```
- Filter narrows case-insensitively; matches the TUI's
  `SshKeyPicker::filtered`.
- Enter populates the form's "Key" field with the bare filename. The
  host's `resolve_key_path` will turn it into a real path.

### 3.6 Terminal tab rendering

When the viewer receives `tab_opened` with `model === "ssh"` it:

1. Adds the tab to the tab strip with prefix glyph "▣ ssh:" (or
   `[ssh]` ASCII fallback for environments that strip Unicode), title
   = the hostname portion of `name`.
2. Mounts a `<div class="ssh-pane" data-tab-id="..."></div>` inside
   the tab content area, replacing the chat rendering for that tab.
3. Constructs an `xterm.Terminal({ cols, rows, theme, scrollback: 10000,
   cursorBlink: true, fontFamily: 'JetBrains Mono, Menlo, monospace',
   fontSize: 13 })`, loads the WebGL + fit + web-links addons,
   `.open(div)`, `.fit()`.
4. Wires `term.onData(d => ws.send(JSON.stringify({ type:
   'ssh_terminal_input', tab_id, data_b64: base64(d) })))`.
5. Wires a `ResizeObserver` on the pane to `.fit()` + send
   `ssh_terminal_resize { cols, rows }`.

On `ssh_terminal_data`: `atob(data_b64)` → `term.write(decoded)`. The
viewer tracks `seqLast` per tab; gaps (rare — single ordered WS)
emit a console warning but otherwise the buffer is best-effort.

On `tab_closed` for an SSH tab: `term.dispose()`, remove the pane.

### 3.7 Status footer

When an SSH tab is active, the footer (existing status bar at the
bottom of viewer.html) shows:

```
●  user@host:port    connected   2m 15s    seq 1284    [Disconnect]
```

Fields:

- `●` / `○` glyph: green `●` when `status === "connected"`, yellow
  `●` during "connecting"/"auth", red `○` for `auth_failed` /
  `disconnected` / `error`.
- `user@host:port`: from the `name` field of `tab_opened`.
- Status label: from the last `ssh_connection_status.status`.
- Elapsed: client-side timer since the first "connecting" event.
- `seq`: monotonic counter of `ssh_terminal_data` events received
  for this tab — useful for "is the stream alive?" at a glance.
- `[Disconnect]` button: sends `ssh_disconnect { tab_id }`. Has a
  confirm dialog if there's been terminal input/output in the last
  30 seconds.

Latency / ping is **not** in v2.2.19 — it requires a host-side
ping/pong over the relay that doesn't exist yet. Add in v2.3.

### 3.8 Disconnect UX

- Slash: typing `/ssh close` in the chat input of an SSH tab sends
  `ssh_disconnect`. (The viewer-side slash detector intercepts this
  pattern before the chat-message round-trip.)
- Button: the footer `[Disconnect]` button.
- Confirm modal if `Date.now() - lastTerminalActivityAt < 30000`:
  "This SSH session was active recently. Disconnect?" / [Cancel] /
  [Disconnect].
- On disconnect: viewer locally optimistically marks the tab status
  as `disconnected`, awaits the host's authoritative
  `ssh_connection_status: disconnected` which then triggers
  `tab_closed` cleanup on the host side.

---

## 4. Auth + vault flow in browser

### 4.1 Connect-by-alias

```
Viewer → Host: ssh_connect { use_alias: "guard", cols: 220, rows: 50 }
Host:
  if vault locked:
    Host → Viewer: ssh_connection_status { status: "error", detail: "vault is locked" }
    (Viewer's existing vault-unlock modal handles re-unlock UX.)
  else:
    cfg = load_ssh_alias("guard")
    spawn_session(cfg, (cols, rows))
    open new SSH tab
    emit tab_opened { name: "ssh:soulofall@guard.example:30022", model: "ssh", ... }
    emit ssh_connection_status { status: "connecting", ... }
    …auth driven by russh…
    emit ssh_connection_status { status: "connected", ... }
```

### 4.2 Ad-hoc connect (form submit)

```
Viewer → Host: ssh_connect {
  host: "guard.example.net", port: 30022, user: "soulofall",
  auth: "password", secret: "hunter2",
  cols: 220, rows: 50,
  save_alias: "guard"  // optional
}
Host:
  if vault locked AND save_alias present:
    proceed with connect, but skip the save (and post a system message
    "Vault locked — not saved").
  cfg = SshConfig { host, port, user, auth: Password(secret) }
  zeroize the cloned secret string in the dispatcher before handing off
  spawn_session(cfg, (cols, rows))
  …same as above…
  if save_alias is Some AND vault unlocked:
    save_ssh_alias(label=save_alias, &cfg)
```

The wire field `secret` in `ssh_connect` is:

- Marked `#[serde(skip_serializing_if = "Option::is_none")]` so
  agent/none cases never carry it.
- Cleared from the host-side `RelayMessage` struct via a manual
  zeroize step after `SshConfig` is built. (Standard `zeroize` crate
  on the `String`.)
- Never written to the relay debug log on the host
  (`tracing` filter skips message payload for `ssh_connect`).
- Never persisted to passage's replay cache — passage's switch
  forwards `ssh_*` events but the cache is whitelisted to
  session_meta / memory_snapshot / tab_opened; `ssh_*` will NOT be
  added to that cache.

### 4.3 Browser-side credential hygiene

- The form's secret input is cleared on submit (`input.value = ""`)
  before the WS send.
- The form's state object is not stored in `localStorage`. (The
  viewer already follows this convention for other modals; this
  spec just makes it explicit.)
- The `Save as alias` checkbox is OFF by default — opt-in only.
- DOM secret-field has `autocomplete="off"` + `data-lpignore="true"`
  + `data-1p-ignore="true"` + `data-bwignore="true"` +
  `data-form-type="other"`. Tested combinations for LastPass /
  1Password / Bitwarden.

---

## 5. Security boundaries

### 5.1 Transport

- **WSS only.** Existing passage setup terminates TLS at Cloudflare;
  hop to passage is over the internal network. Inherits v2.2.18
  posture; no new exposure.
- `ssh_connect.secret` and `ssh_terminal_input.data_b64` are the only
  sensitive payloads. Both ride the same WSS as everything else; no
  separate channel needed.

### 5.2 Browser hygiene (anti-autofill)

- All form inputs in the SSH modal: `autocomplete="off"` +
  `data-lpignore="true"` + `data-1p-ignore="true"` +
  `data-bwignore="true"` + `data-form-type="other"`.
- The wrapping `<form>` element: `autocomplete="off"` +
  `data-lpignore="true"`.
- Tests in the viewer test plan (section 6 below) include a
  manual-check item for each major password manager.

### 5.3 Relay rate limiting

- New rate limiter in `passage-culpur.net/src/routes/relay/sessions.ts`:
  the relay (which has no application-level state per the v2.2.18
  default-allow model) gets a **per-session sliding-window counter**
  keyed by `(hash, "ssh_connect")` — max 5 `ssh_connect` messages
  per 60 seconds.
- Exceeding the limit drops the message + sends a synthetic
  `ssh_connection_status { status: "error", detail: "rate limit:
  too many connection attempts" }` back to the viewer.
- The counter is in-memory on the relay (acceptable: passage relay
  is single-process per session via the existing `RelaySession` map).
- Applies only to `ssh_connect` — `ssh_terminal_input` /
  `ssh_terminal_resize` are not rate-limited (a connected session
  needs free-flow).

### 5.4 Log scrubbing (host)

- `tracing` calls on the host must NOT log raw host strings or
  `data_b64` payloads. Use `%redacted` markers.
- Terminal data is `base64(raw_bytes)` — raw bytes can contain
  arbitrary escape sequences. Before logging connection metadata, the
  host scrubs `\x00-\x1f` (excluding `\t\n\r`) and `\x7f` from
  `host` / `user` / `detail` strings (defense-in-depth against log
  injection / terminal-control-sequence shenanigans).

### 5.5 Slash whitelist update

The existing `remote_control::slash_dispatch_route` whitelist
(`schedule`, `daemon`, `remote-control`, `share`) stays narrow. `/ssh`
intentionally does NOT join this list — webui SSH rides the
dedicated `ssh_*` message family, which gets its own rate-limit and
its own audit log line on the host. This keeps the slash-dispatch
attack surface small.

### 5.6 Permission gate

`ssh_connect` is gated behind:

1. Paired (passage enforces).
2. Vault unlocked **iff** `use_alias` is set OR `save_alias` is set.
   Ad-hoc connect without save needs no vault.
3. The host's session-wide "remote control allows destructive
   actions" toggle (already exists for `slash_dispatch`). New `/ssh`
   actions inherit the same toggle.

### 5.7 Secret zeroize

- `ssh_connect.secret: Option<String>` — host clones into
  `SshConfig`, then `zeroize::Zeroize::zeroize(&mut secret)` on the
  relay-message field BEFORE `SshConfig` is moved into the bridge.
- The russh driver already moves the secret into the auth call; once
  authenticated the secret is no longer referenced.
- For password auth that fails, the secret is zeroized at
  `auth_password` return (driver should `Drop`-zeroize — separate
  small TUI-side change, in scope for agent #2).

---

## 6. Implementation phases

### 6.1 Phase 1 — Backend (anvil-cli + passage relay)

**Owner:** agent #2. **Estimated effort: 6 hours.**

**Files touched (anvil-cli):**

- `crates/runtime/src/relay.rs` — add 11 `RelayMessage` variants, 11
  `KNOWN_RELAY_TAGS` entries, 11 `type_tag` arms. Update drift gate
  test. ~150 LOC.
- `crates/anvil-cli/src/remote_control.rs` — new `SshRequest`
  routing path (separate from `slash_dispatch_route` — does NOT
  extend that whitelist). ~100 LOC.
- `crates/anvil-cli/src/tui/ssh_bridge.rs` — add a new
  `spawn_session_for_remote(...)` that emits `ssh_terminal_data`
  events into the relay broadcast channel in addition to the local
  sync channel. ~50 LOC.
- `crates/anvil-cli/src/main.rs` — relay-input dispatcher (the big
  `__foo:bar` sentinel match) gains arms for `__ssh_connect:...` /
  `__ssh_list_aliases` / `__ssh_list_keys` / `__ssh_terminal_input` /
  `__ssh_terminal_resize` / `__ssh_disconnect`. Each arm does the
  vault-gate / russh handoff. ~150 LOC.
- `Cargo.toml` (anvil-cli or runtime): add `zeroize = "1"` if not
  already pulled in (it is — vault uses it).

**Files touched (passage):**

- `src/routes/relay/sessions.ts` — add `ssh_connect` rate limiter
  (sliding window, 5/min/session). Add `ssh_*` to the client →
  host allowlist switch. ~100 LOC.
- Tests in `tests/relay-ssh-rate-limit.test.ts` — new file. ~50
  LOC.

**Tests needed:**

- Drift gate (auto): every new variant has a `KNOWN_RELAY_TAGS` row.
- Unit: `SshConnect` round-trips JSON with all-None / all-Some
  payloads.
- Unit: `SshConnect.secret` is zeroized after the host moves it into
  an `SshConfig`.
- Unit: passage rate-limit drops the 6th `ssh_connect` in a 60s
  window, lets the 5th through.
- Integration: a mock russh server + a fake viewer (Rust test) that
  sends `ssh_connect` and observes `ssh_terminal_data` flow.

**Breakage risk: LOW.** All additions; no enum-variant removals; no
field renames. Existing v2.2.18 wire stays untouched.

### 6.2 Phase 2 — viewer.html UI

**Owner:** agent #3. **Estimated effort: 8 hours.**

**Files touched:**

- `passage-culpur.net/public/viewer.html` — single-file additions.
  Sub-sections:
  - Inline `<script>` block: xterm.js 5.5 + WebGL + fit + web-links
    addons, ~250 KB (vendored from node_modules at viewer-build time
    by an existing copy step — or inlined as a one-time vendor
    commit). ~250 KB.
  - JS: ~400 LOC for the message handlers, xterm attach/detach, form
    modal, picker sub-modal, status footer, slash interception.
  - CSS: ~80 LOC for the modal + pane + footer styling. Matches
    existing dark-theme variables.
  - HTML: ~80 LOC of modal + picker markup, hidden by default.

**Tests needed:**

- Manual smoke: load viewer, type `/ssh`, fill form, submit, verify
  terminal opens, `ls` echoes, `vim` renders, `htop` updates,
  `Ctrl+C` interrupts, `exit` closes.
- Manual smoke: open with vault locked → dropdown shows lock banner →
  unlock → dropdown populates with aliases.
- Manual smoke: `[Browse]` opens the key picker, filter narrows,
  Enter selects.
- Manual smoke: disconnect button shows confirm modal when
  recently-active.
- Password-manager autofill: verify LastPass / 1Password / Bitwarden
  do NOT autofill the SSH form on three browsers (Chrome / Firefox /
  Safari).

**Breakage risk: MEDIUM.** xterm.js adds ~250 KB to the viewer
download. Tab-switching code in `viewer.html:967` (`renderTabBar`)
must learn to NOT replace the SSH pane content on tab-switch — only
hide/show it. Subtle; touches the tab-render hot path.

### 6.3 Phase 3 — Wire-up + integration test

**Owner:** agent #3 (or shared). **Estimated effort: 4 hours.**

**Files touched:**

- `crates/anvil-cli/tests/ssh_webui_integration.rs` — new file. ~200
  LOC. Spins up a mock SSH server in-process, mocks a relay socket,
  injects `ssh_connect` → asserts `tab_opened` + the russh handshake
  fires → injects `ssh_terminal_input("ls\n")` → asserts the mock
  server sees `b"ls\n"` → fakes server reply → asserts
  `ssh_terminal_data` events come back with matching base64 → sends
  `ssh_disconnect` → asserts `tab_closed` + bridge cleanup.

**Tests needed:**

- Full round-trip happy path (above).
- Auth failure path: bad password → `ssh_connection_status:
  auth_failed` event → no `tab_opened`.
- Rate-limit path: 6 `ssh_connect`s in a row → 6th rejected with
  synthetic error.
- Disconnect cleanup: closing the WS mid-stream → the russh thread
  exits within 1s, no leaked tokio task.

**Breakage risk: LOW.** Tests are additive; they are the regression
gate, not the change surface.

### 6.4 Cumulative phase summary

| Phase | LOC est. | Effort | Owner | Risk |
|---|---|---|---|---|
| 1 — Backend | ~450 LOC (300 Rust + 150 TS) | 6h | agent #2 | low |
| 2 — viewer.html UI | ~600 LOC (HTML/CSS/JS) | 8h | agent #3 | medium |
| 3 — Integration test | ~200 LOC | 4h | agent #3 | low |
| **Total** | **~1250 LOC** | **18h** | | |

---

## 7. Open questions for the implementer

1. **xterm.js delivery:** vendor (inline `<script>`) vs. CDN reference
   vs. served from passage as a sibling static asset? Recommendation:
   vendored inline into the single-file viewer to keep the
   single-file invariant intact. ~250 KB on a viewer load is fine.

2. **`/ssh <alias>` auto-connect in browser?** TUI doesn't
   auto-connect; design above matches (form opens prefilled, user
   hits Connect). If we want browser-side auto-connect we add a
   query param `?ssh=<alias>` to the viewer URL. Out of scope for
   v2.2.19; flag as a v2.3 follow-up.

3. **Multiple SSH tabs concurrent?** Yes — each gets its own
   xterm.js instance and its own per-tab seq counter. The host
   already supports N concurrent `SshTabState`s; the relay broadcast
   carries `tab_id` so per-tab routing works for free.

4. **What if the browser closes mid-session?** The viewer's WS
   `onclose` triggers passage's `client_disconnected` event which the
   host already handles. The SSH session stays alive on the host (the
   TUI tab is unaffected). If the viewer reconnects, it learns about
   the SSH tab via the host's session_snapshot replay. The terminal
   buffer is NOT replayed (no replay cache for `ssh_terminal_data`,
   per section 3 of the relay-protocol doc); the viewer just gets the
   "tab exists" signal and the user resumes typing into a blank
   xterm. (Optional: emit a one-shot `clear screen` ANSI seq from the
   host on browser reconnect — out of scope for v2.2.19.)

5. **Vault unlock UX inside the SSH form modal?** The existing
   viewer already has a vault-unlock modal (triggered by `vault_state
   { locked: true }`). The SSH modal's lock banner re-uses it.
   Sequence: user clicks lock banner → vault-unlock modal opens on
   top → unlock submits → on success, `vault_state { locked: false }`
   propagates → SSH modal banner hides + `ssh_list_aliases` is
   re-dispatched.

---

## 8. Acceptance criteria

A v2.2.19 release with this feature is GREEN when:

- [ ] All 11 new relay tags land in `KNOWN_RELAY_TAGS` and the drift
      gate test passes.
- [ ] `ssh_connect` rate-limit test in passage passes.
- [ ] Round-trip integration test (Phase 3) passes.
- [ ] Manual: `/ssh` in viewer on a fresh paired session opens the
      form modal.
- [ ] Manual: filling host/port/user/agent + Connect spawns a real
      SSH session, xterm.js renders the shell, typing reaches the
      server, output renders, `exit` closes the tab cleanly.
- [ ] Manual: vault-saved alias appears in dropdown when vault is
      unlocked; clicking it auto-fills the form.
- [ ] Manual: LastPass / 1Password / Bitwarden do NOT autofill the
      Secret field.
- [ ] No `ssh_terminal_data` payloads appear in the host's
      `~/.anvil/anvil.log`.
- [ ] No `ssh_connect.secret` value appears in passage's stdout/stderr
      or pm2 logs.
- [ ] Drift gate test confirms all 11 new tags carry the correct
      `RelayDirection`.
- [ ] Existing v2.2.18 behaviour (non-SSH tabs, chat layouts, slash
      dispatch, hub install) is unchanged — regression tests pass.

---

## 9. Cross-references

- TUI implementation audit: `docs/ssh-tui-audit.md`
- Current relay protocol: `docs/ssh-relay-protocol-current.md`
- Original SSH client release notes: `RELEASE-NOTES-v2.2.12.md`
- v2.2.18 webui rewrite reference: `RELEASE-NOTES-v2.2.18.md`
- Slash dispatch enum: `crates/commands/src/lib.rs:405`
- Relay enum: `crates/runtime/src/relay.rs:113`
- Existing passage relay logic: `passage-culpur.net/src/routes/relay/sessions.ts`
