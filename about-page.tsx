import Link from "next/link";
import type { Metadata } from "next";

export const metadata: Metadata = {
  title: "About Anvil",
  description:
    "Anvil is the AI coding assistant built for professionals. Multi-provider, full-screen TUI, 1M token context, and 44+ built-in tools.",
};

const FEATURES = [
  {
    title: "Multi-Provider",
    desc: "Switch between Claude, OpenAI, Ollama, and xAI instantly. Smart failover keeps you coding when rate limits hit. Configure multiple providers and let Anvil route intelligently.",
    icon: "⚡",
  },
  {
    title: "1M Token Context",
    desc: "Never lose context mid-session. Automatic archival to a searchable history when the window fills up. QMD-powered retrieval brings it back the moment it's relevant.",
    icon: "🧠",
  },
  {
    title: "44+ Built-in Tools",
    desc: "Bash execution, file operations, web search, MCP protocol, LSP integration, image generation, and more. Everything you need without configuration.",
    icon: "🔧",
  },
  {
    title: "Full-Screen TUI",
    desc: "Tabs, streaming output, tool call visualization, and a Claude Code-style footer. Vim mode, mouse support, and customizable keybindings.",
    icon: "🖥",
  },
  {
    title: "Smart Memory",
    desc: "QMD knowledge base auto-injects relevant context from your previous sessions. Pin files, drag-and-drop images, and search your codebase inline.",
    icon: "📌",
  },
  {
    title: "AnvilHub Marketplace",
    desc: "Install community-built skills, plugins, agents, and themes with a single command. Publish your own and share with the community.",
    icon: "🛒",
  },
];

const HOW_IT_WORKS = [
  {
    step: "1",
    title: "Install",
    desc: 'Run the installer or download the binary. Anvil is a single 13MB binary — no runtime dependencies.',
  },
  {
    step: "2",
    title: "Login",
    desc: 'Run `anvil login` to authenticate with your AI provider. Supports OAuth for Anthropic and API keys for all providers.',
  },
  {
    step: "3",
    title: "Code",
    desc: 'Run `anvil` in any project directory. Anvil reads your codebase context and you\'re ready to go.',
  },
  {
    step: "4",
    title: "Extend",
    desc: 'Browse AnvilHub for skills and plugins that match your workflow. Install with `anvil install <package-name>`.',
  },
];

export default function AboutPage() {
  return (
    <div className="max-w-5xl mx-auto px-4 sm:px-6 py-16">
      {/* Hero */}
      <div className="text-center mb-20">
        <h1 className="text-5xl font-bold mb-6">
          About <span className="text-accent-cyan">Anvil</span>
        </h1>
        <p className="text-xl text-text-secondary max-w-2xl mx-auto mb-8">
          Anvil is the AI coding assistant built for developers who demand control.
          Multi-provider, full-screen TUI, 1M token context, and intelligent context
          management — all in a 13MB binary.
        </p>
        <div className="flex flex-col sm:flex-row gap-4 justify-center">
          <Link
            href="/install"
            className="px-8 py-3 bg-accent-cyan text-bg-primary rounded-xl font-semibold hover:bg-accent-bright transition-colors"
          >
            Install Anvil
          </Link>
          <Link
            href="/"
            className="px-8 py-3 bg-bg-card border border-border text-white rounded-xl font-semibold hover:bg-bg-card-hover hover:border-border-hover transition-colors"
          >
            Browse AnvilHub
          </Link>
        </div>
      </div>

      {/* What is Anvil */}
      <section className="mb-20">
        <h2 className="text-3xl font-bold mb-6">What is Anvil?</h2>
        <div className="bg-bg-card border border-border rounded-xl p-8">
          <p className="text-text-secondary text-lg leading-relaxed mb-4">
            Anvil is a terminal-native AI coding assistant that runs entirely in your terminal.
            Unlike web-based tools, Anvil lives where you work — in your shell, with direct
            access to your filesystem, build tools, and development environment.
          </p>
          <p className="text-text-secondary text-lg leading-relaxed mb-4">
            Built by Culpur Defense for performance and distributed as a single binary, Anvil connects
            to multiple AI providers and intelligently routes your requests. When Claude hits
            a rate limit, Anvil can automatically fall back to OpenAI or a local Ollama model
            without interrupting your flow.
          </p>
          <p className="text-text-secondary text-lg leading-relaxed">
            The QMD (Query-Memory-Dispatch) system gives Anvil persistent memory across
            sessions. Your codebase knowledge, conversation history, and pinned context
            are always available, even when the context window fills up.
          </p>
        </div>
      </section>

      {/* Features */}
      <section className="mb-20">
        <h2 className="text-3xl font-bold mb-10">Features</h2>
        <div className="grid grid-cols-1 md:grid-cols-2 gap-6">
          {FEATURES.map((f) => (
            <div
              key={f.title}
              className="bg-bg-card border border-border rounded-xl p-6 hover:border-border-hover transition-colors"
            >
              <div className="text-3xl mb-3">{f.icon}</div>
              <h3 className="text-lg font-semibold text-white mb-2">{f.title}</h3>
              <p className="text-text-secondary text-sm leading-relaxed">{f.desc}</p>
            </div>
          ))}
        </div>
      </section>

      {/* How it works */}
      <section className="mb-20">
        <h2 className="text-3xl font-bold mb-10">How It Works</h2>
        <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-6">
          {HOW_IT_WORKS.map((step) => (
            <div key={step.step} className="text-center">
              <div className="w-12 h-12 rounded-full bg-accent-cyan/10 border border-accent-cyan/30 flex items-center justify-center text-accent-cyan font-bold text-lg mx-auto mb-4">
                {step.step}
              </div>
              <h3 className="font-semibold text-white mb-2">{step.title}</h3>
              <p className="text-text-secondary text-sm">{step.desc}</p>
            </div>
          ))}
        </div>
      </section>

      {/* Built by Culpur */}
      <section className="mb-20">
        <div className="bg-bg-card border border-border rounded-xl p-8 text-center">
          <h2 className="text-2xl font-bold mb-4">Built by Culpur Defense</h2>
          <p className="text-text-secondary max-w-xl mx-auto mb-6">
            Anvil is developed and maintained by Culpur Defense Inc. — a cybersecurity and
            defense technology company building tools for professionals who operate in
            high-stakes environments.
          </p>
          <a
            href="https://culpur.net"
            className="text-accent-cyan hover:text-accent-bright transition-colors"
            target="_blank"
            rel="noopener noreferrer"
          >
            culpur.net →
          </a>
        </div>
      </section>

      {/* Changelog */}
      <section className="mb-20">
        <h2 className="text-3xl font-bold mb-10">Changelog</h2>

        {/* v2.1.0.1 */}
        <div className="mb-10">
          <div className="flex items-center gap-3 mb-4">
            <span className="text-lg font-bold text-white">v2.1.0.1</span>
            <span className="text-xs text-text-muted bg-bg-card border border-border rounded-full px-3 py-1">April 7, 2026</span>
          </div>
          <ul className="space-y-1.5 text-sm text-text-secondary list-none">
            <li><span className="text-accent-cyan mr-2">&#10003;</span>Expanded <code className="text-accent-cyan">/notify</code> with Discord, Slack, Telegram, WhatsApp, and Signal support</li>
          </ul>
        </div>

        {/* v2.1.0 */}
        <div className="mb-10">
          <div className="flex items-center gap-3 mb-4">
            <span className="text-lg font-bold text-white">v2.1.0</span>
            <span className="text-xs text-text-muted bg-bg-card border border-border rounded-full px-3 py-1">April 7, 2026</span>
          </div>
          <ul className="space-y-1.5 text-sm text-text-secondary list-none">
            <li><span className="text-accent-cyan mr-2">&#10003;</span>AES-256-GCM encrypted credential vault with Argon2id KDF</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span>File write sandbox (project boundary enforcement)</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span>Native Ollama /api/chat with thinking mode support</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span>Multi-line input (1-5 lines dynamic growth)</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span>Ctrl+C clear/double-tap exit</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span>Codebase modularized: 134 modules, 394 tests</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span>Content filter: modern OpenAI key format detection</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span>Total: 90 commands, 45 tools, 7 agent types, 4 providers</li>
          </ul>
        </div>

        {/* v2.0.0 */}
        <div className="mb-10">
          <div className="flex items-center gap-3 mb-4">
            <span className="text-lg font-bold text-white">v2.0.0</span>
            <span className="text-xs text-text-muted bg-bg-card border border-border rounded-full px-3 py-1">April 7, 2026</span>
          </div>
          <ul className="space-y-1.5 text-sm text-text-secondary list-none">
            <li><span className="text-accent-cyan mr-2">&#10003;</span>Permission memory (persistent tool approvals)</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span>Content filtering / injection defense</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span>Configurable keybindings</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span>Agent SendMessage / Continue</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span>Worktree isolation for agents</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span>/fast mode toggle</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span>Rich slash command help</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span>Clipboard image paste</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span>Background agent notifications</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span>/review-pr command</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span>First-run setup wizard with provider configuration</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span>Total: 90 commands, 45 tools, 7 agents, 4 providers</li>
          </ul>
        </div>

        {/* v1.0.3 */}
        <div className="mb-10">
          <div className="flex items-center gap-3 mb-4">
            <span className="text-lg font-bold text-white">v1.0.3</span>
            <span className="text-xs text-text-muted bg-bg-card border border-border rounded-full px-3 py-1">April 7, 2026</span>
          </div>
          <p className="text-text-secondary text-sm mb-3 font-medium">21 new features:</p>
          <ul className="space-y-1.5 text-sm text-text-secondary list-none">
            <li><span className="text-accent-cyan mr-2">&#10003;</span>Credential vault with AES-256-GCM encryption + Argon2id key derivation + TOTP support</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/lsp</code> — Language server integration (start, symbols, references)</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/notebook</code> — Jupyter notebook run, cell execution, and export</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/k8s</code> — Kubernetes cluster management (pods, logs, apply, describe)</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/iac</code> — Terraform/OpenTofu IaC (plan, apply, validate, drift)</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/pipeline</code> — CI/CD pipeline builder (generate, lint, run)</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/review</code> — AI code review (file, staged diff, PR)</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/deps</code> — Dependency management (tree, outdated, audit, why)</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/mono</code> — Monorepo workspace tools (list, graph, changed, run)</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/browser</code> — Browser automation (open, screenshot, test)</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/notify</code> — Desktop notifications (macOS, Linux)</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/migrate</code> — Codebase migration assistant (framework, language, deps)</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/regex</code> — Regex builder, tester, and explainer</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/ssh</code> — SSH session manager (list, connect, tunnel, keys)</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/logs</code> — Log analysis (tail, search, analyze, stats)</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/markdown</code> — Markdown tools (preview, toc, lint)</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/snippets</code> — Code snippet library (save, list, get, search)</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/finetune</code> — AI model fine-tuning pipeline (prepare, validate, start, status)</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/webhook</code> — Webhook endpoint management (list, add, test, remove)</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/plugin-sdk</code> — Plugin development (init, build, test, publish)</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span>Total: 90 commands, 45 tools, 7 agent types, 8 themes, 7 languages</li>
          </ul>
        </div>

        {/* v1.0.2 */}
        <div className="mb-10">
          <div className="flex items-center gap-3 mb-4">
            <span className="text-lg font-bold text-white">v1.0.2</span>
            <span className="text-xs text-text-muted bg-bg-card border border-border rounded-full px-3 py-1">April 7, 2026</span>
          </div>
          <p className="text-text-secondary text-sm mb-3 font-medium">20 new features:</p>
          <ul className="space-y-1.5 text-sm text-text-secondary list-none">
            <li><span className="text-accent-cyan mr-2">&#10003;</span>Multi-language support: English, German, Spanish, French, Japanese, Chinese, Russian</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/hub</code> — AnvilHub marketplace integration (browse, search, install packages)</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/semantic-search</code> — AST-aware symbol search across codebases</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/docker</code> — Container management (ps, logs, compose, build)</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/test</code> — Test generation, runner, and coverage reporting</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/git</code> — Advanced operations (rebase, conflicts, cherry-pick, stash)</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/refactor</code> — Codebase refactoring (rename, extract, move)</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/screenshot</code> — Screen capture for AI vision analysis</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/db</code> — Database tools (connect, schema, query, migrate)</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/security</code> — Vulnerability scanning, secret detection, dependency audit</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/api</code> — OpenAPI spec generation, mock server, endpoint testing</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/docs</code> — Documentation generation (README, architecture, changelog)</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/scaffold</code> — Project templates (Rust, Node, Python, React, Next.js, Go, Docker)</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/perf</code> — Performance profiling and benchmarking</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/debug</code> — Debugging assistant with error explanation</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/theme</code> — 3 new built-in themes (Monokai, Gruvbox, Catppuccin)</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/changelog</code> — Auto-generate changelogs from git history</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">/env</code> — Environment variable management</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span><code className="text-accent-cyan">anvil --update</code> — Self-update to latest release</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span>Update notification in TUI footer</li>
          </ul>
        </div>

        {/* v1.0.1a */}
        <div className="mb-10">
          <div className="flex items-center gap-3 mb-4">
            <span className="text-lg font-bold text-white">v1.0.1a</span>
            <span className="text-xs text-text-muted bg-bg-card border border-border rounded-full px-3 py-1">April 4, 2026</span>
          </div>
          <ul className="space-y-1.5 text-sm text-text-secondary list-none">
            <li><span className="text-accent-cyan mr-2">&#10003;</span>Multi-provider support (Anthropic, OpenAI, Ollama, xAI)</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span>Full-screen TUI with ratatui</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span>Interactive <code className="text-accent-cyan">/configure</code> menu</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span>N-level command completion cascade</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span>QMD knowledge base integration</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span>Context archival system</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span>Failover chains</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span>File drag-and-drop with vision</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span>Image generation</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span>90 slash commands, 45 tools, 7 agent types</li>
          </ul>
        </div>

        {/* v1.0.0 */}
        <div className="mb-10">
          <div className="flex items-center gap-3 mb-4">
            <span className="text-lg font-bold text-white">v1.0.0</span>
            <span className="text-xs text-text-muted bg-bg-card border border-border rounded-full px-3 py-1">April 2, 2026</span>
          </div>
          <ul className="space-y-1.5 text-sm text-text-secondary list-none">
            <li><span className="text-accent-cyan mr-2">&#10003;</span>Initial release</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span>Rust CLI with Anthropic OAuth</li>
            <li><span className="text-accent-cyan mr-2">&#10003;</span>Basic REPL and tool execution</li>
          </ul>
        </div>
      </section>

      {/* CTA */}
      <div className="text-center border-t border-border pt-16">
        <h2 className="text-3xl font-bold mb-4">Ready to get started?</h2>
        <p className="text-text-secondary mb-8">
          Install Anvil in seconds on macOS, Linux, or Windows.
        </p>
        <Link
          href="/install"
          className="inline-block px-10 py-4 bg-accent-cyan text-bg-primary rounded-xl font-semibold text-lg hover:bg-accent-bright transition-colors"
        >
          Install Now
        </Link>
      </div>
    </div>
  );
}
