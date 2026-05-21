# TUI `/ssh` Implementation Audit

Task #706 (v2.2.18 → v2.2.19 webui parity). Author: webui-ssh agent #1.
Scope: read-only audit of the embedded SSH client that shipped across
v2.2.12 (tasks #418–#424). Sister docs: `ssh-relay-protocol-current.md`,
`ssh-webui-design-spec.md`.

---

## 1. File layout

| Concern | File | LOC |
|---|---|---|
| Slash parser variant + dispatch | `crates/commands/src/lib.rs` (lines 405–415, 993, 3092) | — |
| Slash spec entry | `crates/commands/src/specs.rs` (line 1630) | — |
| Headless stub (says "TUI only") | `crates/commands/src/handlers.rs:533` | — |
| Driver (russh) — public entry | `crates/runtime/src/ssh/mod.rs` | 35 |
| Driver — config types | `crates/runtime/src/ssh/config.rs` | 44 |
| Driver — russh client handler, auth loop, I/O pumps | `crates/runtime/src/ssh/driver.rs` | 411 |
| Driver — session handle + events | `crates/runtime/src/ssh/session.rs` | 92 |
| Driver — tests | `crates/runtime/src/ssh/tests.rs` | 459 |
| Vault adapter — alias save/load/list | `crates/runtime/src/ssh/vault_alias.rs` | 484 |
| TUI form modal + key picker | `crates/anvil-cli/src/tui/ssh_form.rs` | 1132 |
| TUI vt100-backed tab state + key encoding | `crates/anvil-cli/src/tui/ssh_tab.rs` | 280 |
| Async-to-sync bridge thread | `crates/anvil-cli/src/tui/ssh_bridge.rs` | 150 |
| `/ssh` slash handler (form + save + list + alias lookup) | `crates/anvil-cli/src/main.rs:6466, 9580–9738` | — |
| Form-submit → tab spawn + key forwarding + Ctrl+B escape | `crates/anvil-cli/src/tui/input_handler.rs:412–530` | — |
| Integration test | `crates/anvil-cli/tests/ssh_integration.rs` | — |

No separate vault `host_credential.rs` exists. The schema reuses
`CredentialType::HostCredential` from `crates/runtime/src/vault/mod.rs`;
SSH-specific shape is encoded in `Credential.metadata` JSON, marshalled
through `runtime::ssh::SshMetadata` defined in `vault_alias.rs:46`.

---

## 2. Slash command surface

Parsed forms (commands crate, all snake-case stored as
`SlashCommand::Ssh { args: Option<String> }`):

| Form | Behaviour |
|---|---|
| `/ssh` (bare) | Open the SSH form modal on top of the active TUI tab. |
| `/ssh <alias>` | If vault is unlocked, `runtime::ssh::load_ssh_alias` looks up the label and `prefill`s the form. If not found, parse `<alias>` as `[user@]host[:port]` and prefill those fields. |
| `/ssh save <alias>` | Snapshot the active SSH tab's `destination` (`user@host:port`), reconstruct an `SshConfig` with auth=Agent, save via `runtime::ssh::save_ssh_alias`. Note: only Agent is reconstructible from a live tab — the original cred material was consumed at connect time. |
| `/ssh list` | Enumerate vault entries with `credential_type=HostCredential` AND `tag=ssh`. |

Dispatch site is `crates/anvil-cli/src/main.rs:6466`, which calls
`Self::handle_ssh_tui_command(args.as_deref(), tui)` at 9580. In headless
(`--print`) mode the static handler returns the "requires TUI" string
(`commands/src/handlers.rs:533`).

The bare-form path always opens the modal; alias-found path
`prefill`s and still requires the user to hit `[ Connect ]` — there is
**no auto-connect on `/ssh <alias>`**, even with a fully-populated
alias. (Open question if user wants auto-connect for webui — see design
spec section 3.3.)

---

## 3. End-to-end user flow

### 3.1 Bare `/ssh` (modal)

1. User types `/ssh` in the TUI input box.
2. Parser → `SlashCommand::Ssh { args: None }`.
3. `main.rs:6466` calls `handle_ssh_tui_command(None, tui)`.
4. `handle_ssh_tui_command` (9737) sets `tui.ssh_form = Some(SshFormState::new())`.
5. Next draw renders the modal (`ssh_form::SshFormState::render`,
   ssh_form.rs:587) — a 40%-width / 23-row centered floating dialog
   with rounded borders, `bg=Black`, title `" SSH Connect "`, fields:
   Host, Port (default "22"), User, Auth (`< Agent | Key file | Password >`),
   KeyPath + `[ Browse ~/.ssh ]` (only if auth=Key), Secret (only if
   auth!=Agent), Alias, `[ Connect ]`. Hint line at bottom.
6. While `tui.ssh_form.is_some()`, **all** keystrokes route to the form
   (input_handler.rs:415–473). Keys recognised by the form (Tab,
   Shift+Tab, Enter, Esc, Up/Down on Auth, arrows/Backspace/Delete in
   text fields, Ctrl+F to open key picker, char input) are consumed.
7. Submit (Enter on `[ Connect ]`, or Tab from `[ Connect ]`) calls
   `SshFormState::validate` → `Result<SshConfig, String>`. On `Err` the
   error is rendered as a red line at the bottom of the modal. On `Ok`
   the form returns `SshFormResult::Submit(config, alias)`.
8. input_handler.rs:430 clears `tui.ssh_form`, calls
   `crossterm::terminal::size()` for `(cols, rows)`, hands them to
   `ssh_bridge::spawn_session(config, (cols as u32, rows as u32))`.
9. `spawn_session` (ssh_bridge.rs:47) spawns a dedicated OS thread,
   builds a current-thread tokio runtime, calls
   `runtime::ssh::connect(config, initial_size)`, and pumps four
   channels between the async `SshSession` and four `std::sync::mpsc`
   channels (`SshChannels`).
10. Back in input_handler, an `SshTabState` is constructed (ssh_tab.rs:74)
    with the sync receivers + vt100 parser of `(rows, cols, 1000)`.
11. A new tab is created via `self.new_tab(format!("ssh:{dest}"), "ssh", "")`
    and switched to. The tab's `ssh: Option<SshTabState>` field carries
    the SSH state; chat-mode fields (`log`, `pending_text`, `input`,
    `branches`) are unused on SSH tabs.
12. Optional alias save: if `alias.is_some()` and vault is unlocked,
    `runtime::ssh::save_ssh_alias` persists the credential silently
    (failures are non-fatal).

### 3.2 `/ssh <alias>` (prefill)

Steps 1–2 same; parser produces `args: Some("alias")`.
`handle_ssh_tui_command` (main.rs:9697):

- If vault unlocked, try `load_ssh_alias(vm, alias)`. On success,
  `SshFormState::prefill(&cfg, Some(alias))` populates every field
  (`collapse_key_path` shortens any path under `~/.ssh/` back to the
  bare filename for display).
- If vault locked or alias missing, parse the arg as `[user@]host[:port]`
  and prefill host/user/port only.
- Form is opened — user still hits `[ Connect ]` to actually connect.

Vault-locked path **does not** open a modal asking to unlock — it
falls through to the host-spec parse silently. This is a UX gap the
webui spec inherits.

### 3.3 Connected SSH tab

While `tab.ssh.is_some()`:

- The main TUI tick polls `SshTabState::drain_stdout()` and
  `drain_events()`. `drain_stdout` calls `vt100::Parser::process(&chunk)`
  for every chunk on the sync receiver and returns `true` if any bytes
  were consumed (caller marks dirty).
- `drain_events` converts the trimmed `UiSshEvent` stream into
  `SshConnState` transitions: Connecting → AuthInProgress(method) →
  Connected, or Connecting → AuthFailed/Disconnected/Error.
- The tab's title (`ssh:user@host:port`) and the status footer (via
  `SshConnState::label()` returning "connecting" / "auth (password)" /
  "connected" / "disconnected" / "error") are kept in sync.
- The vt100 screen is rendered into the active-tab pane (rendering site
  is the layout module's tab-content path — vt100 cells are read from
  `parser.screen()` and painted as styled spans).

Keys typed while an SSH tab is active are intercepted in
`input_handler.rs:488–530`:

- `Ctrl+B` is the SSH-mode escape prefix (screen/tmux convention). It
  is **never** forwarded. Sets `ssh_escape_pending = true`.
- `Ctrl+B` then digit `0–9`: switch to that tab index.
- `Ctrl+B` then `q` / `Q`: close the SSH tab. `self.active_tab_mut().ssh
  = None` drops `SshTabState`, which drops its `Sender`s, which causes
  the bridge thread's tokio tasks to exit and the OS thread to finish.
- `Ctrl+B` then any other key: clear escape state, fall through to
  normal forward.
- Any other key is encoded to bytes by `ssh_tab::key_event_to_bytes`
  (xterm-style CSI sequences for arrows / Home / End / Page / F1–F12,
  control-byte folding for `Ctrl+letter`, ESC-prefix for `Alt+letter`,
  CR for Enter, DEL (`0x7f`) for Backspace) and sent via
  `SshTabState::send_bytes` → `stdin_tx.send`.

Resize: triggered by `SshTabState::resize(cols, rows)`. Updates the
local vt100 parser via `parser.screen_mut().set_size(rows, cols)` AND
sends `(cols as u32, rows as u32)` on `resize_tx`. The bridge forwards
it to the russh session; the russh driver sends an SSH `window-change`
request via `channel.window_change(cols, rows, 0, 0)`.

### 3.4 Disconnect / cleanup

- Server-side EOF / exit-status / exit-signal → russh emits
  `ChannelMsg::Eof | ExitStatus | ExitSignal`, driver sends
  `SshEvent::Disconnected(reason)` and exits its select loop.
- The events relay sees `Disconnected`, sends `UiSshEvent::Disconnected`
  on the sync channel. The stdout pump (which drives the tokio
  `block_on`) drops out of `recv()` when its channel closes, and the
  bridge sends one more best-effort `Disconnected(None)` (de-dupe is
  the TUI's job — `stream_finished` flag in `SshTabState`).
- `SshConnState::Disconnected` sets `stream_finished = true` and the
  TUI stops redrawing the tab on idle ticks.
- Closing the tab via `Ctrl+B q`: drops `SshTabState`, which drops the
  `Sender`s. The bridge thread's `stdin_to_remote` and `resize_to_remote`
  spawn_blocking tasks exit when their `Receiver::recv()` returns Err;
  the stdout pump exits when its tx fails; the russh session is
  dropped, which closes the TCP connection.
- No "force-close" / "kill -9" path. No reconnect logic. No
  known_hosts handling — `ClientHandler::check_server_key` always
  returns `Ok(true)` (TOFU disabled; TODO comment in driver.rs:26
  says known_hosts is deferred to v2.3).

---

## 4. Data flow (one frame)

```
  TUI input box
      │ Crossterm KeyEvent (sync, on the TUI main thread)
      ▼
  input_handler::handle_key  ─── checks tab.ssh.is_some() ──┐
                                                            │
                  ┌─ ssh_tab::key_event_to_bytes(key) ──────┘
                  │   returns Vec<u8> in xterm/POSIX shape
                  ▼
  SshTabState::send_bytes  ──► std::sync::mpsc::Sender<Vec<u8>>
                                       │
                                       ▼  (sync→async cross-thread)
  ssh_bridge spawn_blocking thread:    │
        stdin_rx.recv() ──► tokio::sync::mpsc::Sender::blocking_send
                                       │
                                       ▼
  driver::run_stdin_pump (tokio task):
        AsyncWrite::write_all into the russh channel writer
                                       │
                                       ▼
  russh client encrypts + frames the bytes onto the TCP socket
                                       │
                                       ▼
  …network…
                                       │
  russh receives ChannelMsg::Data { data }
                                       │
                                       ▼
  driver::run_output_resize_pump:
        stdout_tx.send(data.to_vec())  (tokio mpsc)
                                       │
                                       ▼
  ssh_bridge events_relay (tokio task on the bridge thread):
        while let Some(chunk) = session.stdout.recv().await {
            stdout_tx.send(chunk)      (std::sync::mpsc, cross-thread)
        }
                                       │
                                       ▼  (async→sync cross-thread)
  TUI main thread, next tick:
        SshTabState::drain_stdout()
            while let Ok(chunk) = stdout_rx.try_recv():
                parser.process(&chunk)  // vt100 state machine
        → mark redraw dirty
                                       │
                                       ▼
  layout renderer reads parser.screen() and paints styled cells
```

Lifecycle events (Connecting / Auth / Connected / Disconnected) ride a
parallel `events_rx` channel that drains every tick into
`SshConnState`.

---

## 5. Connection lifecycle

| Phase | Trigger | State | TUI surface |
|---|---|---|---|
| Form submit | User Enter on `[ Connect ]` | `Connecting` | tab opened with `ssh:` prefix, "SSH connecting to …" system message |
| TCP+SSH handshake | `client::connect` resolves | `Connecting` | (no change) |
| Auth attempt | `SshEvent::AuthAttempt { method }` | `AuthInProgress(method)` | footer reads "auth (agent|key|password|interactive)" |
| Auth success | `SshEvent::AuthSuccess` (skipped — immediately followed by `Connected`) | — | — |
| Shell ready | `SshEvent::Connected` | `Connected` | footer "connected", remote bytes begin rendering |
| Auth fail | `SshEvent::AuthFailure(reason)` | `AuthFailed(reason)` + `stream_finished=true` | footer "auth failed", tab is dead |
| Server disconnect | EOF / ExitStatus / ExitSignal | `Disconnected(reason)` + `stream_finished=true` | footer "disconnected" |
| Local error | `SshEvent::Error(msg)` | `Error(msg)` + `stream_finished=true` | footer "error" |

There is no per-connection timeout — TCP connect inherits the default
russh/socket timeout. No reconnect. Closing the tab is the only
recovery from a `*Failed` / `Disconnected` / `Error` state.

---

## 6. Large outputs, scrollback, backpressure

- **Scrollback:** `vt100::Parser::new(rows, cols, 1000)` — 1000 lines of
  vt100 scrollback (cells, not bytes). Hard-coded in ssh_tab.rs:85.
- **ANSI handling:** delegated entirely to the `vt100` crate — full
  xterm-256color escape sequence support (request_pty in
  driver.rs:67 advertises `"xterm-256color"`).
- **Backpressure:** every mpsc channel in this stack is bounded:
  - russh→stdout: `CHAN_BUF = 256` chunks (driver.rs:17).
  - tokio events: 256.
  - bridge sync mpsc: unbounded `std::sync::mpsc::channel` (no
    backpressure on the sync side — output that the TUI cannot drain
    fast enough accumulates in memory). This is a known soft cap; in
    practice the TUI drains in `try_recv` loops every tick.
- **Stdin:** typed bytes are tiny; no backpressure pressure point.
- **No throttle / rate-limit** on the SSH side — a fast remote (e.g.
  `cat /dev/urandom | base64`) will fill the scrollback and saturate
  the vt100 parser. The bridge's sync mpsc grows unbounded; OOM is
  theoretically possible but never observed in v2.2.12 dogfood.

---

## 7. Events the SSH tab emits to the rest of the TUI

The SSH tab is **passive** for the rest of the TUI. It does not push
events into the main session log, the rail, the agent system, or OTel.
The only side-effects are:

- `tab.ssh.is_some()` flips key routing in `input_handler::handle_key`.
- `SshConnState::label()` populates the status footer (when the active
  tab is an SSH tab).
- Tab title (`ssh:user@host:port`) appears in the tab strip rendering.
- `push_system("SSH connecting to …")` / `"SSH tab closed."` /
  `"SSH form cancelled."` are pushed to the **chat** scrollback of the
  currently-active tab (not the SSH tab itself — SSH tabs have no chat
  log). This is a small UX quirk worth flagging for the webui parity:
  these system lines should land in the host's "non-ssh" scrollback,
  not be relayed as events on the SSH tab.

No OTel spans, no QMD entries, no MemorySnapshot updates from SSH
activity.

---

## 8. Vault `HostCredential` shape

From `crates/runtime/src/ssh/vault_alias.rs` (SshMetadata + alias
adapter):

```
Credential {
    label:            <alias>,                          // user-picked
    credential_type:  CredentialType::HostCredential,
    username:         Some(<ssh user>),
    secret:           <password | passphrase | "">,     // AES-256-GCM
    url:              Some(<"host:port">),
    notes:            None,
    tags:             vec!["ssh"],
    metadata:         { "kind": "ssh",
                        "ssh_auth": "agent|key|password|interactive",
                        "key_path": Some(<path>)? },
    …timestamps…
}
```

Round-trip helpers: `save_ssh_alias`, `load_ssh_alias`,
`list_ssh_aliases` (filters by `credential_type=HostCredential` AND
`tags contains "ssh"`).

`parse_host_port` accepts `host:port` or bare `host` (defaults port 22)
— webui equivalent must match.

---

## 9. Known gaps inherited by the webui design

These are TUI-side gaps that the webui design should not paper over:

1. **No auto-connect on `/ssh <alias>`** — even with a fully-populated
   alias the user must hit `[ Connect ]`. Worth deciding for the webui.
2. **`/ssh save` only saves Agent auth** because the original credential
   material is consumed by russh at connect time. The webui can do
   better — the form already knows the auth method at submit time, so
   the save can happen before the credential is moved into russh.
3. **`check_server_key` is a no-op TOFU**. No `~/.anvil/known_hosts`.
   The webui will inherit this; the spec should flag it as a v2.3
   follow-up.
4. **No keyboard-interactive UI** (per ssh_bridge.rs:11 comment).
   Driver supports `InteractivePrompt` but the bridge drops it. Webui
   will need a modal challenge dialog — out of scope for v2.2.19.
5. **`/ssh save` from an active tab cannot recover the credential** —
   webui can fix this by capturing the credential at form-submit time.
6. **Bridge's sync mpsc is unbounded** — webui must add an explicit
   bound (or chunk-aware throttle) when shovelling vt100 bytes over
   WSS, otherwise a fast remote will saturate the relay and lag the
   browser.
7. **`/ssh` is not currently in the slash-dispatch whitelist for remote
   sessions** (`remote_control.rs:slash_dispatch_route` returns
   `NotWhitelisted` for anything other than `schedule`, `daemon`,
   `remote-control`, `share`). Webui parity requires extending the
   whitelist OR routing `/ssh` through a dedicated message type. The
   design spec picks the dedicated-message approach for safety
   (rate-limited at the relay).
