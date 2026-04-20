// ─── Anvil Web Config Panels — Phase 3 ────────────────────────────────────
// Vanilla JS, no build step. Injected into viewer.html.

// ── State ─────────────────────────────────────────────────────────────────

const CFG = {
  data: {},          // current config snapshot from host
  vaultLocked: true, // vault lock state
  activePanel: 'providers',
  saveTimers: {},    // debounce timers keyed by field
  configTabActive: false,
};

// ── Vault-sensitive field manifest ─────────────────────────────────────────
// These fields show •••• when vault is locked and cannot be edited.

const VAULT_SENSITIVE = new Set([
  'anthropic_api_key', 'openai_api_key', 'xai_api_key', 'ollama_api_key',
  'tavily_api_key', 'brave_search_api_key', 'exa_api_key',
  'perplexity_api_key', 'google_search_api_key', 'bing_search_api_key',
  'notify_discord_webhook', 'notify_slack_webhook', 'notify_telegram_token',
  'notify_matrix_token', 'notify_signal_sender',
  'github_token', 'wp_password',
  'db_url',
]);

// ── Panel registry ─────────────────────────────────────────────────────────

const PANELS = [
  { id: 'providers',   label: 'Providers' },
  { id: 'models',      label: 'Models' },
  { id: 'context',     label: 'Context' },
  { id: 'search',      label: 'Search' },
  { id: 'permissions', label: 'Permissions' },
  { id: 'display',     label: 'Display' },
  { id: 'integrations',label: 'Integrations' },
  { id: 'lang_theme',  label: 'Language & Theme' },
  { id: 'vault',       label: 'Vault' },
  { id: 'notifications',label: 'Notifications' },
  { id: 'failover',    label: 'Failover' },
  { id: 'ssh',         label: 'SSH' },
  { id: 'docker_k8s',  label: 'Docker & K8s' },
  { id: 'database',    label: 'Database' },
  { id: 'memory',      label: 'Memory & Archive' },
  { id: 'plugins',     label: 'Plugins & Cron' },
  { id: 'statusline',  label: 'Status Line' },
];

// ── WS message handlers (called from viewer.html handleMessage) ──────────

function handleConfigMessage(msg) {
  switch (msg.type) {
    case 'config_snapshot':
      CFG.data = msg.config || {};
      if (CFG.configTabActive) renderActivePanel();
      break;
    case 'config_saved':
      CFG.data = msg.config || {};
      showToast('Settings saved', 'success');
      if (CFG.configTabActive) renderActivePanel();
      break;
    case 'config_error':
      showToast(`Error (${msg.panel}.${msg.field}): ${msg.message}`, 'error');
      // Re-render to restore the field state
      if (CFG.configTabActive) renderActivePanel();
      break;
    case 'vault_state':
      const wasLocked = CFG.vaultLocked;
      CFG.vaultLocked = !!msg.locked;
      if (wasLocked !== CFG.vaultLocked && CFG.configTabActive) renderActivePanel();
      break;
  }
}

// ── Config tab activation ─────────────────────────────────────────────────

function activateConfigTab() {
  CFG.configTabActive = true;
  // Request snapshot on first activation if we don't have data yet
  if (Object.keys(CFG.data).length === 0 && typeof ws !== 'undefined' && ws) {
    ws.send(JSON.stringify({ type: 'config_get' }));
  }
  renderConfigRoot();
}

function deactivateConfigTab() {
  CFG.configTabActive = false;
}

// ── Root render ───────────────────────────────────────────────────────────

function renderConfigRoot() {
  const container = document.getElementById('config-root');
  if (!container) return;

  container.innerHTML = '';
  container.className = 'config-root';

  // Sidebar
  const sidebar = document.createElement('div');
  sidebar.className = 'config-sidebar';
  PANELS.forEach(p => {
    const item = document.createElement('div');
    item.className = 'config-sidebar-item' + (CFG.activePanel === p.id ? ' active' : '');
    item.textContent = p.label;
    item.onclick = () => {
      CFG.activePanel = p.id;
      renderConfigRoot();
    };
    sidebar.appendChild(item);
  });

  // Main pane
  const main = document.createElement('div');
  main.className = 'config-main';
  main.id = 'config-main-pane';

  container.appendChild(sidebar);
  container.appendChild(main);

  renderActivePanel();
}

function renderActivePanel() {
  const main = document.getElementById('config-main-pane');
  if (!main) return;
  main.innerHTML = '';

  // Vault locked banner (except on vault panel itself which has its own)
  if (CFG.vaultLocked && CFG.activePanel !== 'vault') {
    const banner = el('div', 'vault-locked-banner');
    banner.innerHTML = '<span class="lock-icon">🔒</span><span class="banner-text">Vault locked — credential fields are hidden. Go to Vault panel to unlock.</span>';
    main.appendChild(banner);
  }

  const panelFns = {
    providers:    renderProvidersPanel,
    models:       renderModelsPanel,
    context:      renderContextPanel,
    search:       renderSearchPanel,
    permissions:  renderPermissionsPanel,
    display:      renderDisplayPanel,
    integrations: renderIntegrationsPanel,
    lang_theme:   renderLangThemePanel,
    vault:        renderVaultPanel,
    notifications: renderNotificationsPanel,
    failover:     renderFailoverPanel,
    ssh:          renderSshPanel,
    docker_k8s:   renderDockerK8sPanel,
    database:     renderDatabasePanel,
    memory:       renderMemoryPanel,
    plugins:      renderPluginsPanel,
    statusline:   renderStatusLinePanel,
  };

  const fn = panelFns[CFG.activePanel];
  if (fn) fn(main);
}

// ── Panel 1: Providers ────────────────────────────────────────────────────

function renderProvidersPanel(container) {
  panelHeader(container, 'Providers', 'Configure API credentials for AI providers.');
  const p = CFG.data.providers || {};

  appendRow(container,
    'Anthropic', 'API key or OAuth for Claude models.',
    maskedInput('anthropic_api_key', 'providers', p.anthropic_status || ''),
    statusBadge(p.anthropic_status)
  );
  appendRow(container,
    'OpenAI', 'API key for GPT / image models.',
    maskedInput('openai_api_key', 'providers', ''),
    statusBadge(p.openai_status)
  );
  appendRow(container,
    'Ollama host', 'Local Ollama server URL.',
    textInput('ollama_host', 'providers', p.ollama_host || 'http://localhost:11434'),
    statusBadge(p.ollama_status)
  );
  appendRow(container,
    'xAI', 'Grok API key.',
    maskedInput('xai_api_key', 'providers', ''),
    statusBadge(p.xai_status)
  );
}

// ── Panel 2: Models ───────────────────────────────────────────────────────

function renderModelsPanel(container) {
  panelHeader(container, 'Models', 'Set the default model, image model, and failover chain.');
  const m = CFG.data.models || {};

  appendRow(container,
    'Default model', 'Startup model for new sessions.',
    textInput('default_model', 'models', m.default_model || '')
  );
  appendRow(container,
    'Image model', 'Model used for image generation.',
    textInput('image_model', 'models', m.image_model || 'gpt-image-1.5')
  );

  // Failover chain (readonly list)
  const chain = Array.isArray(m.failover_chain) ? m.failover_chain : [];
  const chainEl = el('div', 'chain-list');
  if (chain.length === 0) {
    const empty = el('span', 'cron-empty');
    empty.textContent = 'No failover chain configured.';
    chainEl.appendChild(empty);
  } else {
    chain.forEach((model, i) => {
      const item = el('div', 'chain-item');
      item.innerHTML = `<span class="chain-num">${i + 1}</span><span>${escHtml(model)}</span>`;
      chainEl.appendChild(item);
    });
  }
  appendRow(container,
    'Failover chain', 'Configure via /failover add <model> in TUI.',
    chainEl
  );
}

// ── Panel 3: Context ──────────────────────────────────────────────────────

function renderContextPanel(container) {
  panelHeader(container, 'Context', 'Control context window size, compaction, and integrations.');
  const c = CFG.data.context || {};

  appendRow(container,
    'Context size', 'Maximum tokens in the context window.',
    numberInput('context_size', 'context', c.context_size || 1000000, 1000, 2000000)
  );
  appendRow(container,
    'Auto-compact threshold', 'Trigger compaction at this % of context filled.',
    numberInput('compact_threshold', 'context', c.compact_threshold || 85, 1, 100)
  );
  appendRow(container,
    'QMD integration', 'Enable local QMD semantic search index.',
    toggleSwitch('qmd_enabled', 'context', !!c.qmd_status && !c.qmd_status.startsWith('disabled'))
  );
  appendRow(container,
    'History archival', 'Archive completed sessions to disk.',
    toggleSwitch('history_enabled', 'context', (c.history_count || 0) >= 0)
  );
}

// ── Panel 4: Search ───────────────────────────────────────────────────────

function renderSearchPanel(container) {
  panelHeader(container, 'Search', 'Configure search providers and API keys.');
  const s = CFG.data.search || {};
  const defaultSearch = s.default_search || 'duckduckgo';

  const providers = ['duckduckgo', 'tavily', 'brave', 'searxng', 'exa', 'perplexity', 'google', 'bing'];
  appendRow(container,
    'Default provider', 'Provider used when search is invoked.',
    selectInput('default_search', 'search', defaultSearch,
      providers.map(p => ({ value: p, label: p.charAt(0).toUpperCase() + p.slice(1) }))
    )
  );

  appendRow(container, 'Tavily API key', 'tavily.com', maskedInput('tavily_api_key', 'search', ''));
  appendRow(container, 'Brave API key', 'brave.com/search/api', maskedInput('brave_search_api_key', 'search', ''));
  appendRow(container, 'SearXNG URL', 'Your SearXNG instance URL.', textInput('searxng_url', 'search', ''));
  appendRow(container, 'Exa API key', 'exa.ai', maskedInput('exa_api_key', 'search', ''));
  appendRow(container, 'Perplexity API key', 'perplexity.ai', maskedInput('perplexity_api_key', 'search', ''));
  appendRow(container, 'Google API key', 'Google Custom Search API key.', maskedInput('google_search_api_key', 'search', ''));
  appendRow(container, 'Bing API key', 'Azure Bing Search key.', maskedInput('bing_search_api_key', 'search', ''));
}

// ── Panel 5: Permissions ──────────────────────────────────────────────────

function renderPermissionsPanel(container) {
  panelHeader(container, 'Permissions', 'Control what tools Anvil can use.');
  const perm = (CFG.data.permissions || {}).permission_mode || 'danger-full-access';

  const radioGroup = el('div', 'radio-group');
  const modes = [
    { value: 'read-only',          label: 'Read-only',          desc: 'Read files only. No writes or shell commands.' },
    { value: 'workspace-write',    label: 'Workspace write',    desc: 'Read + write workspace files. No shell commands.' },
    { value: 'danger-full-access', label: 'Full access',        desc: 'Full tool access including shell (default).' },
  ];
  modes.forEach(mode => {
    const label = document.createElement('label');
    label.className = 'radio-option';
    const input = document.createElement('input');
    input.type = 'radio';
    input.name = 'permission_mode';
    input.value = mode.value;
    input.checked = perm === mode.value;
    input.onchange = () => sendConfigUpdate('permissions', 'permission_mode', mode.value);
    const span = document.createElement('span');
    span.textContent = `${mode.label} — ${mode.desc}`;
    label.appendChild(input);
    label.appendChild(span);
    radioGroup.appendChild(label);
  });

  appendRow(container, 'Permission mode', 'Applies immediately to the current session.', radioGroup);
}

// ── Panel 6: Display ──────────────────────────────────────────────────────

function renderDisplayPanel(container) {
  panelHeader(container, 'Display', 'Toggle keybinding modes and interface options.');
  const d = CFG.data.display || {};

  appendRow(container,
    'Vim mode', 'Enable Vim keybindings in the TUI input.',
    toggleSwitch('vim_mode', 'display', !!d.vim_mode)
  );
  appendRow(container,
    'Chat mode', 'Disable tools; respond as a plain chat model.',
    toggleSwitch('chat_mode', 'display', !!d.chat_mode)
  );
  appendRow(container,
    'Tab forward key', 'Keybinding to switch to next tab.',
    textInput('tab_key_forward', 'display', d.tab_key_forward || 'Ctrl+]')
  );
  appendRow(container,
    'Tab back key', 'Keybinding to switch to previous tab.',
    textInput('tab_key_back', 'display', d.tab_key_back || 'Ctrl+[')
  );
}

// ── Panel 7: Integrations ─────────────────────────────────────────────────

function renderIntegrationsPanel(container) {
  panelHeader(container, 'Integrations', 'Connect external services.');
  const i = CFG.data.integrations || {};

  appendRow(container,
    'AnvilHub URL', 'Base URL for the AnvilHub marketplace.',
    textInput('anvilhub_url', 'integrations', i.anvilhub_url || 'https://anvilhub.culpur.net')
  );
  appendRow(container,
    'WordPress URL', 'Your WordPress site URL.',
    textInput('wp_url', 'integrations', i.wp_url || '')
  );
  appendRow(container,
    'WordPress user', 'WordPress username for API access.',
    textInput('wp_user', 'integrations', i.wp_user || '')
  );
  appendRow(container,
    'GitHub token', 'Personal access token for GitHub CLI tools.',
    maskedInput('github_token', 'integrations', '')
  );
}

// ── Panel 8: Language & Theme ─────────────────────────────────────────────

function renderLangThemePanel(container) {
  panelHeader(container, 'Language & Theme', 'Set display language and visual theme.');
  const d = CFG.data.display || {};

  const langs = [
    { value: 'en', label: 'English' },
    { value: 'de', label: 'Deutsch' },
    { value: 'fr', label: 'Français' },
    { value: 'es', label: 'Español' },
    { value: 'ja', label: '日本語' },
    { value: 'zh', label: '中文' },
    { value: 'pt', label: 'Português' },
    { value: 'ru', label: 'Русский' },
  ];
  appendRow(container,
    'Language', 'Interface language.',
    selectInput('language', 'lang_theme', d.language || 'en', langs)
  );

  const themes = [
    'culpur-defense', 'nord', 'dracula', 'gruvbox', 'catppuccin',
    'tokyo-night', 'one-dark', 'solarized-dark', 'monokai',
  ];
  appendRow(container,
    'Theme', 'Color theme for the TUI.',
    selectInput('theme', 'lang_theme', d.active_theme || 'culpur-defense',
      themes.map(t => ({ value: t, label: t }))
    )
  );
  appendRow(container,
    'Status line preset', 'Preset layout for the status bar.',
    selectInput('status_line_preset', 'lang_theme', d.status_line_preset || 'default',
      ['default', 'minimal', 'full', 'developer', 'compact', 'ops', 'security', 'writing']
        .map(t => ({ value: t, label: t }))
    )
  );
}

// ── Panel 9: Vault ────────────────────────────────────────────────────────

function renderVaultPanel(container) {
  panelHeader(container, 'Vault', 'Manage encrypted credentials and vault session.');
  const v = CFG.data.vault || {};

  // Prominent lock state
  const lockState = el('div', 'vault-lock-state ' + (CFG.vaultLocked ? 'locked' : 'unlocked'));
  lockState.innerHTML = `
    <span class="lock-big">${CFG.vaultLocked ? '🔒' : '🔓'}</span>
    <div class="lock-info">
      <div class="lock-title">${CFG.vaultLocked ? 'Vault Locked' : 'Vault Unlocked'}</div>
      <div class="lock-sub">${CFG.vaultLocked
        ? 'Sensitive fields are hidden. Unlock to edit credentials.'
        : 'Vault is active. Sensitive fields are editable.'
      }</div>
    </div>
  `;
  container.appendChild(lockState);

  // Unlock form when locked
  if (CFG.vaultLocked) {
    const form = el('div', 'vault-unlock-form');
    const pwInput = document.createElement('input');
    pwInput.type = 'password';
    pwInput.placeholder = 'Vault password';
    pwInput.id = 'vault-unlock-pw';
    const btn = document.createElement('button');
    btn.className = 'btn-vault-unlock';
    btn.textContent = 'Unlock';
    btn.onclick = () => {
      const pw = document.getElementById('vault-unlock-pw').value;
      if (!pw) return;
      if (ws) ws.send(JSON.stringify({ type: 'user_message', tab_id: 0, message: `/vault unlock ${pw}` }));
      showToast('Unlock request sent', 'info');
      pwInput.value = '';
    };
    pwInput.addEventListener('keydown', e => { if (e.key === 'Enter') btn.click(); });
    form.appendChild(pwInput);
    form.appendChild(btn);
    container.appendChild(form);
  }

  // Settings
  appendRow(container,
    'Session TTL', 'Seconds before vault auto-locks after unlock.',
    numberInput('vault_session_ttl', 'vault', v.vault_session_ttl || 1800, 60, 86400)
  );
  appendRow(container,
    'Auto-lock', 'Automatically lock vault after TTL expires.',
    toggleSwitch('vault_auto_lock', 'vault', !!v.vault_auto_lock)
  );
}

// ── Panel 10: Notifications ───────────────────────────────────────────────

function renderNotificationsPanel(container) {
  panelHeader(container, 'Notifications', 'Configure alert delivery channels.');
  const n = CFG.data.notifications || {};

  const platforms = ['desktop', 'discord', 'slack', 'telegram', 'matrix', 'signal', 'none'];
  appendRow(container,
    'Platform', 'Active notification delivery channel.',
    selectInput('notify_platform', 'notifications', n.notify_platform || 'desktop',
      platforms.map(p => ({ value: p, label: p.charAt(0).toUpperCase() + p.slice(1) }))
    )
  );
  appendRow(container, 'Discord webhook', 'Webhook URL for Discord alerts.', maskedInput('notify_discord_webhook', 'notifications', ''));
  appendRow(container, 'Slack webhook', 'Webhook URL for Slack alerts.', maskedInput('notify_slack_webhook', 'notifications', ''));
  appendRow(container, 'Telegram token', 'Bot token for Telegram alerts.', maskedInput('notify_telegram_token', 'notifications', ''));
  appendRow(container, 'Matrix homeserver', 'e.g. https://matrix.org', textInput('notify_matrix_homeserver', 'notifications', n.notify_matrix_homeserver || ''));
  appendRow(container, 'Signal sender', 'Signal CLI sender number.', maskedInput('notify_signal_sender', 'notifications', ''));
}

// ── Panel 11: Failover ────────────────────────────────────────────────────

function renderFailoverPanel(container) {
  panelHeader(container, 'Failover', 'Control model fallback behavior.');
  const f = CFG.data.failover || {};

  appendRow(container,
    'Cooldown (seconds)', 'Wait this many seconds before retrying a failed model.',
    numberInput('failover_cooldown', 'failover', f.failover_cooldown || 60, 0, 3600)
  );
  appendRow(container,
    'Budget', 'Cost budget in USD per session (0 = unlimited).',
    numberInput('failover_budget', 'failover', f.failover_budget || 0, 0, 1000)
  );
  appendRow(container,
    'Auto-recovery', 'Automatically retry primary model after cooldown.',
    toggleSwitch('failover_auto_recovery', 'failover', f.failover_auto_recovery !== false)
  );
}

// ── Panel 12: SSH ─────────────────────────────────────────────────────────

function renderSshPanel(container) {
  panelHeader(container, 'SSH', 'Configure SSH key and bastion settings.');
  const s = CFG.data.ssh || {};

  appendRow(container, 'Key path', 'Path to your SSH private key.', textInput('ssh_key_path', 'ssh', s.ssh_key_path || '~/.ssh/id_ed25519'));
  appendRow(container, 'Bastion host', 'Jump host for SSH tunnels.', textInput('ssh_bastion_host', 'ssh', s.ssh_bastion_host || ''));
  appendRow(container, 'Config path', 'Path to SSH config file.', textInput('ssh_config_path', 'ssh', s.ssh_config_path || '~/.ssh/config'));
}

// ── Panel 13: Docker & K8s ────────────────────────────────────────────────

function renderDockerK8sPanel(container) {
  panelHeader(container, 'Docker & Kubernetes', 'Configure container and orchestration settings.');
  const d = CFG.data.docker_k8s || {};

  appendRow(container, 'Compose file', 'Path to docker-compose.yml.', textInput('docker_compose_file', 'docker_k8s', d.docker_compose_file || ''));
  appendRow(container, 'Registry URL', 'Container registry base URL.', textInput('docker_registry', 'docker_k8s', d.docker_registry || ''));
  appendRow(container, 'K8s context', 'kubectl context name.', textInput('k8s_context', 'docker_k8s', d.k8s_context || ''));
  appendRow(container, 'K8s namespace', 'Default Kubernetes namespace.', textInput('k8s_namespace', 'docker_k8s', d.k8s_namespace || 'default'));
}

// ── Panel 14: Database ────────────────────────────────────────────────────

function renderDatabasePanel(container) {
  panelHeader(container, 'Database', 'Configure database connection and schema tooling.');
  const d = CFG.data.database || {};

  appendRow(container, 'Database URL', 'Connection string (stored in vault when unlocked).', maskedInput('db_url', 'database', ''));
  appendRow(container,
    'Schema tool', 'Tool used for schema introspection.',
    selectInput('db_schema_tool', 'database', d.db_schema_tool || 'prisma',
      ['prisma', 'sqlx', 'diesel', 'alembic', 'flyway', 'liquibase', 'none']
        .map(t => ({ value: t, label: t }))
    )
  );
}

// ── Panel 15: Memory & Archive ────────────────────────────────────────────

function renderMemoryPanel(container) {
  panelHeader(container, 'Memory & Archive', 'Control how conversations are saved and archived.');
  const m = CFG.data.memory || {};

  appendRow(container, 'Auto-save memory', 'Automatically save context to memory files.', toggleSwitch('auto_save_memory', 'memory', m.auto_save_memory !== false));
  appendRow(container, 'Archive every N sessions', 'Sessions between archive snapshots.', numberInput('archive_frequency', 'memory', m.archive_frequency || 5, 1, 100));
  appendRow(container, 'Retention (days)', 'Days to keep archived sessions.', numberInput('archive_retention_days', 'memory', m.archive_retention_days || 30, 1, 3650));
  appendRow(container, 'Memory directory', 'Override default ~/.anvil/memory path.', textInput('memory_dir', 'memory', m.memory_dir || ''));
}

// ── Panel 16: Plugins & Cron ──────────────────────────────────────────────

function renderPluginsPanel(container) {
  panelHeader(container, 'Plugins & Cron', 'Manage plugin search paths and scheduled jobs.');
  const p = CFG.data.plugins || {};

  appendRow(container, 'Search paths', 'Colon-separated paths to search for plugins.', textInput('plugin_search_paths', 'plugins', p.plugin_search_paths || ''));
  appendRow(container, 'Auto-enable plugins', 'Enable newly discovered plugins automatically.', toggleSwitch('auto_enable_plugins', 'plugins', !!p.auto_enable_plugins));
  appendRow(container, 'Cron enabled', 'Enable the Anvil cron scheduler.', toggleSwitch('cron_enabled', 'plugins', !!p.cron_enabled));

  // Active cron jobs — readonly
  const jobs = Array.isArray(p.active_cron_jobs) ? p.active_cron_jobs : [];
  const jobList = el('div', 'cron-list');
  if (jobs.length === 0) {
    const empty = el('span', 'cron-empty');
    empty.textContent = 'No active cron jobs.';
    jobList.appendChild(empty);
  } else {
    jobs.forEach(j => {
      const item = el('div', 'cron-item');
      item.textContent = j;
      jobList.appendChild(item);
    });
  }
  appendRow(container, 'Active cron jobs', 'Read-only. Manage via /cron in TUI.', jobList);
}

// ── Panel 17: Status Line (stub) ──────────────────────────────────────────

function renderStatusLinePanel(container) {
  panelHeader(container, 'Status Line', 'Visual status bar configuration.');

  const stub = el('div', 'statusline-stub');
  stub.innerHTML = `
    <div class="stub-icon">⊞</div>
    <div class="stub-title">Status Line Editor</div>
    <div class="stub-desc">
      The full drag-and-drop status line editor is available in the TUI.
      Web-based editing is planned for Phase 3b.
    </div>
  `;
  const btn = document.createElement('button');
  btn.className = 'btn-open-tui';
  btn.textContent = 'Configure in TUI (/configure statusline)';
  btn.onclick = () => {
    if (ws && paired) {
      ws.send(JSON.stringify({ type: 'user_message', tab_id: activeTab, message: '/configure statusline' }));
      showToast('Sent /configure statusline to TUI', 'info');
    }
  };
  stub.appendChild(btn);
  container.appendChild(stub);

  // Show current preset as informational
  const d = CFG.data.display || {};
  if (d.status_line_preset) {
    const info = el('p', '');
    info.style.cssText = 'font-size:12px;color:var(--text-dim);margin-top:16px';
    info.textContent = `Current preset: ${escHtml(d.status_line_preset)}`;
    container.appendChild(info);
  }
}

// ── Control builders ──────────────────────────────────────────────────────

function toggleSwitch(field, panel, checked) {
  const wrap = el('label', 'toggle-switch' + (CFG.vaultLocked && VAULT_SENSITIVE.has(field) ? ' disabled' : ''));
  const input = document.createElement('input');
  input.type = 'checkbox';
  input.checked = checked;
  input.disabled = CFG.vaultLocked && VAULT_SENSITIVE.has(field);
  input.onchange = () => sendConfigUpdate(panel, field, input.checked);
  const track = el('div', 'toggle-track');
  const thumb = el('div', 'toggle-thumb');
  wrap.appendChild(input);
  wrap.appendChild(track);
  wrap.appendChild(thumb);
  return wrap;
}

function textInput(field, panel, value) {
  const isLocked = CFG.vaultLocked && VAULT_SENSITIVE.has(field);
  const input = document.createElement('input');
  input.type = 'text';
  input.className = 'cfg-input';
  input.value = isLocked ? '' : value;
  input.placeholder = isLocked ? '••••' : '';
  input.disabled = isLocked;
  if (!isLocked) {
    input.addEventListener('input', () => debouncedSave(panel, field, input.value, input));
  }
  return input;
}

function maskedInput(field, panel, value) {
  const isLocked = CFG.vaultLocked && VAULT_SENSITIVE.has(field);
  const wrap = el('div', 'masked-wrap');
  const input = document.createElement('input');
  input.type = 'password';
  input.className = 'cfg-input';
  input.value = isLocked ? '' : value;
  input.placeholder = isLocked ? '••••' : 'Enter value...';
  input.disabled = isLocked;

  const toggleBtn = document.createElement('button');
  toggleBtn.className = 'masked-toggle-btn';
  toggleBtn.textContent = '👁';
  toggleBtn.title = 'Show / hide';
  toggleBtn.type = 'button';
  toggleBtn.onclick = () => {
    input.type = input.type === 'password' ? 'text' : 'password';
  };

  if (!isLocked) {
    input.addEventListener('input', () => debouncedSave(panel, field, input.value, input));
  }

  wrap.appendChild(input);
  wrap.appendChild(toggleBtn);
  return wrap;
}

function numberInput(field, panel, value, min, max) {
  const isLocked = CFG.vaultLocked && VAULT_SENSITIVE.has(field);
  const input = document.createElement('input');
  input.type = 'number';
  input.className = 'cfg-number';
  input.value = isLocked ? '' : value;
  input.min = min;
  input.max = max;
  input.disabled = isLocked;
  if (!isLocked) {
    input.addEventListener('input', () => {
      const n = Number(input.value);
      if (!Number.isNaN(n) && n >= min && n <= max) {
        debouncedSave(panel, field, n, input);
      }
    });
  }
  return input;
}

function selectInput(field, panel, value, options) {
  const isLocked = CFG.vaultLocked && VAULT_SENSITIVE.has(field);
  const select = document.createElement('select');
  select.className = 'cfg-select';
  select.disabled = isLocked;
  options.forEach(opt => {
    const o = document.createElement('option');
    o.value = typeof opt === 'string' ? opt : opt.value;
    o.textContent = typeof opt === 'string' ? opt : opt.label;
    o.selected = o.value === value;
    select.appendChild(o);
  });
  if (!isLocked) {
    select.onchange = () => sendConfigUpdate(panel, field, select.value);
  }
  return select;
}

function statusBadge(status) {
  if (!status) return null;
  const badge = el('span', 'status-badge ' + (status.startsWith('✓') ? 'ok' : 'missing'));
  badge.textContent = status.startsWith('✓') ? 'configured' : 'not set';
  return badge;
}

// ── Layout helpers ────────────────────────────────────────────────────────

function panelHeader(container, title, desc) {
  const h = el('div', 'panel-title');
  h.textContent = title;
  container.appendChild(h);
  const d = el('div', 'panel-desc');
  d.textContent = desc;
  container.appendChild(d);
}

function appendRow(container, label, hint, ...controls) {
  const row = el('div', 'setting-row');

  const labelCol = el('div', 'setting-label-col');
  const labelEl = el('div', 'setting-label');
  labelEl.textContent = label;
  labelCol.appendChild(labelEl);
  if (hint) {
    const hintEl = el('div', 'setting-hint');
    hintEl.textContent = hint;
    labelCol.appendChild(hintEl);
  }

  const ctrl = el('div', 'setting-control');
  controls.forEach(c => { if (c) ctrl.appendChild(c); });

  row.appendChild(labelCol);
  row.appendChild(ctrl);
  container.appendChild(row);
}

// ── Config update protocol ────────────────────────────────────────────────

function sendConfigUpdate(panel, field, value) {
  if (!ws || !paired) return;
  ws.send(JSON.stringify({ type: 'config_update', panel, field, value }));
}

function debouncedSave(panel, field, value, inputEl) {
  clearTimeout(CFG.saveTimers[field]);
  if (inputEl) inputEl.className = inputEl.className.replace(' saving', '').replace(' saved', '').replace(' error', '') + ' saving';
  CFG.saveTimers[field] = setTimeout(() => {
    sendConfigUpdate(panel, field, value);
    if (inputEl) {
      inputEl.className = inputEl.className.replace(' saving', '') + ' saved';
      setTimeout(() => {
        if (inputEl) inputEl.className = inputEl.className.replace(' saved', '');
      }, 1500);
    }
  }, 500);
}

// ── Toast notification ────────────────────────────────────────────────────

function showToast(message, type) {
  let container = document.getElementById('toast-container');
  if (!container) {
    container = el('div', 'toast-container');
    container.id = 'toast-container';
    document.body.appendChild(container);
  }
  const toast = el('div', 'toast ' + (type || 'info'));
  toast.textContent = message;
  container.appendChild(toast);
  setTimeout(() => { toast.remove(); }, 3500);
}

// ── Utility ───────────────────────────────────────────────────────────────

function el(tag, className) {
  const e = document.createElement(tag);
  if (className) e.className = className;
  return e;
}

function escHtml(s) {
  const d = document.createElement('div');
  d.textContent = String(s);
  return d.innerHTML;
}
