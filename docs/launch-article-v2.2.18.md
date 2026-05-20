<!--
DALL-E PROMPT (use for hero image generation via OpenAI API):
---
"A sleek dark terminal interface split into two vertical panels on a matte black workstation.
The left panel shows a session rail with glowing teal tab indicators. The right panel displays
streaming AI output with tool call cards that fold and collapse like origami. A subtle waveform
at the bottom represents real-time context usage — the bar is three-quarters full, not overflowing.
The aesthetic is calm, precise, technical — dark mode, teal accent (#00d4d4), no people."
---

TITLE: Anvil v2.2.18: Fix the Compaction Bug, Restore Copy-Paste, Finish the Web Viewer

SLUG: anvil-v2-2-18-autocompact-mouse-capture-web-viewer

TAGS: anvil, release, coding-assistant, terminal, rust, devtools

-->

<h1>Anvil v2.2.18: Fix the Compaction Bug, Restore Copy-Paste, Finish the Web Viewer</h1>

<p>Three bugs. One release. Anvil v2.2.18 lands today with fixes that have been quietly annoying users for several versions, plus the web viewer finally earning its tab-parity badge with the desktop TUI.</p>

<h2>The autocompact bug you didn't know you had</h2>

<p>If you've been using Anvil on claude-sonnet-4-5, Gemini 1.5 Pro, or any other long-context model and noticing that sessions feel like they compact after just a few turns — you weren't imagining it. There was a real bug.</p>

<p>The culprit: <code>maybe_auto_compact</code> was computing its 80% threshold against <code>max_output_tokens</code> rather than the actual context window size. <code>max_output_tokens</code> is typically 8,000 to 16,000 tokens. Your context window is 64,000 to 200,000+ tokens. The result: on a model with a 200K context window, Anvil was firing autocompact when the session reached roughly 6,400 tokens of input — about 3% of the available window.</p>

<p>In practice this meant most long-context model sessions were being compacted after 3–5 turns rather than 30–50 turns. You'd see the "session compacted" notice, lose some context, and carry on — never knowing the session had given up 97% of its available runway.</p>

<p>The fix is one-line: threshold computation now reads <code>session.context_window</code> from the provider's <code>/models</code> response. The 80% trigger percentage is unchanged. If you configure a <code>max_context_tokens</code> override in your settings, that value is used instead. You don't need to change anything — the fix is automatic on upgrade.</p>

<p>To verify it's working after upgrading, run <code>/compact why</code>. Anvil will print the threshold calculation including which window size it's using. A correctly configured claude-sonnet-4-5 session should show a threshold around 160,000 tokens, not 6,400.</p>

<h2>Mouse capture: back to the safe default</h2>

<p>Mouse capture in the terminal is one of those features where defaulting ON has a real cost: it silently breaks copy-paste in most terminals. On macOS Terminal.app, copy is Cmd+C. On Gnome Terminal and kitty, it's Ctrl+Shift+C. On Windows Terminal, it's Ctrl+C. When mouse capture is active, the terminal hands those events to the application instead of handling them as keyboard shortcuts — and if the application isn't expecting them, nothing happens.</p>

<p>Anvil's mouse capture mode (which enables scroll and click support inside the TUI) was defaulting ON. That meant every new user on Gnome Terminal, kitty, or Windows Terminal who installed Anvil and tried to copy a code snippet from the output found that Ctrl+Shift+C did nothing. Not ideal for a first impression.</p>

<p>v2.2.18 flips the default to OFF. Mouse capture is now explicitly opt-in: <code>/config mouse_capture true</code> or the <code>--mouse</code> flag at launch. A one-time toast explains the tradeoff when you first run. Users who had mouse capture working before — and who knew what they were doing — can re-enable it in one command.</p>

<p>A type-level regression test (<code>mouse_capture_default_off_regression</code>) asserts <code>TuiConfig::default().mouse_capture == false</code> so this default can't silently change in a future diff without breaking a test.</p>

<h2>Web viewer: tabs actually work now</h2>

<p>The AnvilHub web viewer has had a tab UI since v2.2.16 introduced the tab architecture in the desktop TUI. What it didn't have was tab <em>routing</em>. Every session's messages broadcast to every connected viewer regardless of which tab created them. If you had two active sessions, messages from session A would appear in session B's scrollback.</p>

<p>The root cause was a <code>paired_count</code> gate in the relay that was supposed to prevent duplicate events on multi-viewer setups. Instead, it blocked correct routing after the first reconnect. Removing the gate and replacing it with stable tab IDs (generated once at creation, never reused) fixed the routing entirely.</p>

<p>v2.2.18 also lands the full <code>/tab</code> command set in the viewer: <code>/tab new</code>, <code>/tab rename &lt;name&gt;</code>, <code>/tab switch &lt;n&gt;</code>, and <code>Ctrl+T</code> for new-tab from any state. The viewer's default layout is now Vertical Split + Tabs, matching the TUI default.</p>

<p>The relay carries more information in this release too. The status footer now shows a proper cost-type chip (OAuth, local, or cloud) instead of a fabricated dollar amount for providers where per-token cost isn't publicly available. Memory snapshots are cached and broadcast so the memory rail in the viewer populates correctly after a reconnect. Session metadata includes <code>context_max</code> so the context-window progress bar reflects the actual window size — not a hardcoded 100K.</p>

<h2>TUI stability: the keyboard-dies bug</h2>

<p>One v2.2.17 regression got widespread enough to warrant a callout: after canceling an <code>/mcp builder</code> flow or any other inline operation that temporarily leaves the alt-screen, the TUI keyboard would stop responding. Characters typed after that point never reached the input box.</p>

<p>The bug was in <code>restore_alt_screen</code>: it was restoring the alt-screen buffer but not re-enabling raw mode. Crossterm's raw mode is what routes keyboard input to the application instead of to the shell. Once it was off, input processing effectively stopped. The fix is one line: <code>terminal::enable_raw_mode()?</code> re-added to the restore path.</p>

<h2>Wizard paste fix (#685)</h2>

<p>Bracketed paste — the terminal protocol that wraps clipboard content in <code>\x1b[200~</code> / <code>\x1b[201~</code> markers so applications can distinguish pasted text from typed input — now works inside textarea modals. This affects the multi-line description field in <code>/mcp builder</code>, the long-prompt field in several wizard steps, and any other <code>TextareaModal</code> in the UI.</p>

<p>The fix wires the existing <code>tui::paste::handle_paste</code> logic (the same code that handles paste in the main input box) into the textarea modal event loop. Line-ending normalization (<code>\r\n</code> → <code>\n</code>) and bracketed-paste sequence stripping apply identically.</p>

<h2>Release pipeline hardening (#654)</h2>

<p><code>release.sh</code> Phase 6 runs SSH-based deploy steps against production. With <code>set -e</code> active at the script level, a failed SSH hop should immediately abort the pipeline. It wasn't: certain SSH invocations in Phase 6 were constructed in ways that allowed their exit codes to be masked, letting the script proceed to subsequent phases against a stale remote state.</p>

<p>Every Phase 6 SSH call now has an explicit <code>|| { echo "Phase 6 SSH failed: ..."; exit 1; }</code> guard. Failed deploys surface immediately rather than silently corrupting the release state.</p>

<h2>Upgrade path</h2>

<p>v2.2.18 is binary-compatible with v2.2.17 sessions. <code>anvil --continue</code> and <code>anvil --resume &lt;id&gt;</code> work across the upgrade without data migration.</p>

<p>The two new config keys (<code>mouse_capture</code>, <code>mouse_capture_toast_seen</code>) are optional — existing <code>~/.anvil/config.json</code> files parse without changes. The autocompact fix is fully automatic; no configuration change is needed or recommended.</p>

<pre><code># macOS / Linux:
curl -fsSL https://anvilhub.culpur.net/install.sh | sh

# Homebrew:
brew upgrade culpur/anvil/anvil

# Windows / FreeBSD / NetBSD:
# https://github.com/culpur/anvil/releases/tag/v2.2.18</code></pre>

<p>Full release notes: <a href="https://github.com/culpur/anvil/releases/tag/v2.2.18">github.com/culpur/anvil/releases/tag/v2.2.18</a></p>
