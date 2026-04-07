"use strict";
var __createBinding = (this && this.__createBinding) || (Object.create ? (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    var desc = Object.getOwnPropertyDescriptor(m, k);
    if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
      desc = { enumerable: true, get: function() { return m[k]; } };
    }
    Object.defineProperty(o, k2, desc);
}) : (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    o[k2] = m[k];
}));
var __setModuleDefault = (this && this.__setModuleDefault) || (Object.create ? (function(o, v) {
    Object.defineProperty(o, "default", { enumerable: true, value: v });
}) : function(o, v) {
    o["default"] = v;
});
var __importStar = (this && this.__importStar) || (function () {
    var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function (o) {
            var ar = [];
            for (var k in o) if (Object.prototype.hasOwnProperty.call(o, k)) ar[ar.length] = k;
            return ar;
        };
        return ownKeys(o);
    };
    return function (mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        __setModuleDefault(result, mod);
        return result;
    };
})();
Object.defineProperty(exports, "__esModule", { value: true });
exports.AnvilChatProvider = void 0;
const vscode = __importStar(require("vscode"));
const anvilProcess_1 = require("./anvilProcess");
// ---------------------------------------------------------------------------
// AnvilChatProvider — WebviewViewProvider for the sidebar chat panel.
// Manages a persistent AnvilProcess per workspace session and forwards
// messages between the webview and the CLI subprocess.
// ---------------------------------------------------------------------------
class AnvilChatProvider {
    constructor(extensionUri) {
        this.anvilProc = null;
        this.pendingRestart = false;
        this.extensionUri = extensionUri;
    }
    // Called by VS Code when the sidebar panel first becomes visible.
    resolveWebviewView(webviewView, _context, _token) {
        this.view = webviewView;
        webviewView.webview.options = {
            enableScripts: true,
            localResourceRoots: [this.extensionUri],
        };
        webviewView.webview.html = this.getHtml(webviewView.webview);
        // Messages from the webview (user typed a prompt, clicked clear, etc.)
        webviewView.webview.onDidReceiveMessage(async (msg) => {
            switch (msg.type) {
                case 'send':
                    await this.handleUserMessage(msg.text);
                    break;
                case 'clear':
                    this.clearChat();
                    break;
                case 'ready':
                    // Webview DOM is ready — boot the subprocess if we haven't yet.
                    await this.ensureProcess();
                    break;
                case 'restart':
                    await this.restartProcess();
                    break;
            }
        });
        webviewView.onDidDispose(() => {
            this.view = undefined;
        });
    }
    // Called by extension.ts when a code-action command (explain/refactor/etc.)
    // wants to inject a pre-built prompt into the chat.
    async sendCodeAction(prompt) {
        // Reveal the sidebar panel.
        await vscode.commands.executeCommand('anvil.chatView.focus');
        await this.handleUserMessage(prompt, true);
    }
    // -------------------------------------------------------------------------
    async ensureProcess() {
        if (this.anvilProc?.isReady()) {
            this.postStatus('connected');
            return;
        }
        const cfg = vscode.workspace.getConfiguration('anvil');
        const path = cfg.get('path', 'anvil');
        const model = cfg.get('model', 'claude-sonnet-4-6');
        const provider = cfg.get('provider', 'anthropic');
        this.postStatus('connecting');
        this.anvilProc = new anvilProcess_1.AnvilProcess(path, model, provider);
        this.anvilProc.on('partial', (line) => {
            this.post({ type: 'partial', text: line });
        });
        this.anvilProc.on('response', (resp) => {
            if (resp.type === 'text') {
                this.post({ type: 'response', text: resp.content });
            }
            else if (resp.type === 'error') {
                this.post({ type: 'error', text: resp.content });
            }
            else if (resp.type === 'done') {
                this.post({ type: 'done' });
            }
        });
        this.anvilProc.on('exit', (_code) => {
            this.postStatus('disconnected');
            if (!this.pendingRestart) {
                this.post({ type: 'error', text: 'Anvil process exited unexpectedly. Click Reconnect to restart.' });
            }
        });
        this.anvilProc.on('error', (err) => {
            this.postStatus('error');
            this.post({ type: 'error', text: `Could not start Anvil: ${err.message}\n\nCheck that "anvil" is on your PATH or set anvil.path in settings.` });
        });
        try {
            await this.anvilProc.start();
            this.postStatus('connected');
        }
        catch (err) {
            this.postStatus('error');
            this.post({ type: 'error', text: `Failed to start Anvil process: ${err}` });
        }
    }
    async handleUserMessage(text, showInChat = false) {
        if (!text.trim()) {
            return;
        }
        if (showInChat) {
            this.post({ type: 'user', text });
        }
        await this.ensureProcess();
        if (!this.anvilProc?.isReady()) {
            this.post({ type: 'error', text: 'Anvil is not ready. Please wait or click Reconnect.' });
            return;
        }
        const sent = this.anvilProc.send(text);
        if (!sent) {
            this.post({ type: 'error', text: 'Failed to send message to Anvil.' });
        }
    }
    async restartProcess() {
        this.pendingRestart = true;
        this.anvilProc?.stop();
        this.anvilProc = null;
        this.pendingRestart = false;
        this.post({ type: 'system', text: 'Reconnecting to Anvil...' });
        await this.ensureProcess();
    }
    clearChat() {
        this.post({ type: 'clear' });
    }
    post(msg) {
        this.view?.webview.postMessage(msg);
    }
    postStatus(status) {
        this.post({ type: 'status', status });
    }
    // -------------------------------------------------------------------------
    // HTML / CSS / JS for the webview
    // -------------------------------------------------------------------------
    getHtml(webview) {
        // Use a nonce to allow only specific inline scripts (CSP requirement).
        const nonce = generateNonce();
        return /* html */ `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1.0" />
  <meta http-equiv="Content-Security-Policy"
        content="default-src 'none';
                 style-src 'unsafe-inline';
                 script-src 'nonce-${nonce}';
                 img-src ${webview.cspSource} data:;" />
  <title>Anvil Chat</title>
  <style>
    *, *::before, *::after { box-sizing: border-box; margin: 0; padding: 0; }

    body {
      font-family: var(--vscode-font-family);
      font-size: var(--vscode-font-size);
      color: var(--vscode-foreground);
      background: var(--vscode-sideBar-background, #1e1e1e);
      display: flex;
      flex-direction: column;
      height: 100vh;
      overflow: hidden;
    }

    /* Header */
    #header {
      display: flex;
      align-items: center;
      justify-content: space-between;
      padding: 6px 10px;
      background: var(--vscode-titleBar-activeBackground, #333);
      border-bottom: 1px solid var(--vscode-panel-border, #444);
      flex-shrink: 0;
    }
    #header-title {
      font-weight: 600;
      font-size: 11px;
      text-transform: uppercase;
      letter-spacing: 0.08em;
      color: var(--vscode-titleBar-activeForeground, #ccc);
    }
    #header-actions { display: flex; gap: 4px; }
    .hdr-btn {
      background: none;
      border: none;
      cursor: pointer;
      color: var(--vscode-icon-foreground, #aaa);
      padding: 2px 4px;
      border-radius: 3px;
      font-size: 11px;
      line-height: 1.4;
    }
    .hdr-btn:hover { background: var(--vscode-toolbar-hoverBackground, #444); }

    /* Status bar */
    #status-bar {
      padding: 2px 10px;
      font-size: 10px;
      display: flex;
      align-items: center;
      gap: 5px;
      background: var(--vscode-statusBar-background, #007acc);
      color: var(--vscode-statusBar-foreground, #fff);
      flex-shrink: 0;
    }
    #status-dot {
      width: 7px; height: 7px;
      border-radius: 50%;
      background: #888;
      flex-shrink: 0;
    }
    #status-dot.connected  { background: #4ec94e; }
    #status-dot.connecting { background: #f0a500; }
    #status-dot.error      { background: #e05252; }
    #status-dot.disconnected { background: #888; }

    /* Message list */
    #messages {
      flex: 1;
      overflow-y: auto;
      padding: 10px;
      display: flex;
      flex-direction: column;
      gap: 8px;
      scroll-behavior: smooth;
    }
    #messages::-webkit-scrollbar { width: 5px; }
    #messages::-webkit-scrollbar-thumb { background: var(--vscode-scrollbarSlider-background, #555); border-radius: 3px; }

    .msg {
      max-width: 100%;
      border-radius: 6px;
      padding: 8px 10px;
      line-height: 1.5;
      word-break: break-word;
      white-space: pre-wrap;
      font-size: 12.5px;
    }
    .msg-user {
      background: var(--vscode-inputValidation-infoBorder, #007acc);
      color: #fff;
      align-self: flex-end;
      border-bottom-right-radius: 2px;
    }
    .msg-assistant {
      background: var(--vscode-editor-inactiveSelectionBackground, #2d2d2d);
      border: 1px solid var(--vscode-panel-border, #3a3a3a);
      align-self: flex-start;
      border-bottom-left-radius: 2px;
    }
    .msg-error {
      background: var(--vscode-inputValidation-errorBackground, #5a1d1d);
      border: 1px solid var(--vscode-inputValidation-errorBorder, #be1100);
      color: var(--vscode-errorForeground, #f48771);
      align-self: stretch;
      font-size: 11.5px;
    }
    .msg-system {
      color: var(--vscode-descriptionForeground, #888);
      font-style: italic;
      font-size: 11px;
      align-self: center;
    }

    /* Code blocks inside assistant messages */
    .msg-assistant pre {
      background: var(--vscode-textCodeBlock-background, #1a1a1a);
      border: 1px solid var(--vscode-panel-border, #3a3a3a);
      border-radius: 4px;
      padding: 8px;
      overflow-x: auto;
      font-family: var(--vscode-editor-font-family, monospace);
      font-size: 11.5px;
      margin: 6px 0;
      white-space: pre;
    }
    .msg-assistant code {
      font-family: var(--vscode-editor-font-family, monospace);
      font-size: 11.5px;
      background: var(--vscode-textCodeBlock-background, #1a1a1a);
      padding: 1px 4px;
      border-radius: 3px;
    }

    /* Thinking / streaming indicator */
    #thinking {
      display: none;
      align-items: center;
      gap: 6px;
      padding: 6px 10px;
      color: var(--vscode-descriptionForeground, #888);
      font-size: 11px;
    }
    #thinking.visible { display: flex; }
    .dot-pulse span {
      display: inline-block;
      width: 5px; height: 5px;
      background: var(--vscode-descriptionForeground, #888);
      border-radius: 50%;
      margin: 0 1px;
      animation: pulse 1.2s infinite ease-in-out;
    }
    .dot-pulse span:nth-child(2) { animation-delay: 0.2s; }
    .dot-pulse span:nth-child(3) { animation-delay: 0.4s; }
    @keyframes pulse {
      0%, 80%, 100% { transform: scale(0.6); opacity: 0.4; }
      40% { transform: scale(1); opacity: 1; }
    }

    /* Input area */
    #input-area {
      display: flex;
      flex-direction: column;
      gap: 5px;
      padding: 8px 10px;
      border-top: 1px solid var(--vscode-panel-border, #444);
      flex-shrink: 0;
      background: var(--vscode-sideBar-background, #1e1e1e);
    }
    #prompt {
      width: 100%;
      resize: none;
      background: var(--vscode-input-background, #3c3c3c);
      color: var(--vscode-input-foreground, #d4d4d4);
      border: 1px solid var(--vscode-input-border, #3c3c3c);
      border-radius: 4px;
      padding: 6px 8px;
      font-family: var(--vscode-font-family);
      font-size: 12.5px;
      line-height: 1.45;
      outline: none;
      min-height: 56px;
      max-height: 160px;
      overflow-y: auto;
    }
    #prompt:focus { border-color: var(--vscode-focusBorder, #007fd4); }
    #prompt::placeholder { color: var(--vscode-input-placeholderForeground, #888); }

    #input-footer {
      display: flex;
      justify-content: space-between;
      align-items: center;
    }
    #hint { font-size: 10px; color: var(--vscode-descriptionForeground, #777); }
    #send-btn {
      background: var(--vscode-button-background, #0e639c);
      color: var(--vscode-button-foreground, #fff);
      border: none;
      border-radius: 3px;
      padding: 4px 12px;
      font-size: 12px;
      cursor: pointer;
    }
    #send-btn:hover { background: var(--vscode-button-hoverBackground, #1177bb); }
    #send-btn:disabled { opacity: 0.45; cursor: default; }
  </style>
</head>
<body>
  <div id="header">
    <span id="header-title">Anvil Chat</span>
    <div id="header-actions">
      <button class="hdr-btn" id="btn-reconnect" title="Reconnect">&#8635;</button>
      <button class="hdr-btn" id="btn-clear" title="Clear chat">&#128465;</button>
    </div>
  </div>

  <div id="status-bar">
    <div id="status-dot"></div>
    <span id="status-text">Initializing...</span>
  </div>

  <div id="messages"></div>

  <div id="thinking">
    <span>Anvil is thinking</span>
    <div class="dot-pulse"><span></span><span></span><span></span></div>
  </div>

  <div id="input-area">
    <textarea id="prompt" placeholder="Ask Anvil anything… (Enter to send, Shift+Enter for newline)" rows="3"></textarea>
    <div id="input-footer">
      <span id="hint">Shift+Enter for newline</span>
      <button id="send-btn">Send</button>
    </div>
  </div>

<script nonce="${nonce}">
(function() {
  const vscode = acquireVsCodeApi();

  const messagesEl = document.getElementById('messages');
  const promptEl   = document.getElementById('prompt');
  const sendBtn    = document.getElementById('send-btn');
  const thinkingEl = document.getElementById('thinking');
  const statusDot  = document.getElementById('status-dot');
  const statusText = document.getElementById('status-text');
  const btnClear   = document.getElementById('btn-clear');
  const btnReconnect = document.getElementById('btn-reconnect');

  let waitingForResponse = false;
  let streamingMsg = null;       // The current assistant bubble being streamed into
  let streamBuffer = '';

  // --- Helpers ---

  function escapeHtml(str) {
    return str
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;');
  }

  // Very lightweight markdown renderer (code fences + inline code + bold/italic)
  function renderMarkdown(text) {
    // Fenced code blocks
    text = text.replace(/\`\`\`(\\w*)\\n?([\\s\\S]*?)\`\`\`/g, (_, lang, code) => {
      return '<pre><code>' + escapeHtml(code.trimEnd()) + '</code></pre>';
    });
    // Inline code
    text = text.replace(/\`([^\`]+)\`/g, (_, c) => '<code>' + escapeHtml(c) + '</code>');
    // Bold
    text = text.replace(/\\*\\*(.+?)\\*\\*/g, '<strong>$1</strong>');
    // Italic
    text = text.replace(/\\*(.+?)\\*/g, '<em>$1</em>');
    // Newlines to <br> (outside of pre blocks — this is a simplification)
    text = text.replace(/\\n/g, '<br>');
    return text;
  }

  function addMessage(role, text) {
    const div = document.createElement('div');
    div.className = 'msg msg-' + role;
    if (role === 'assistant') {
      div.innerHTML = renderMarkdown(text);
    } else if (role === 'error' || role === 'system') {
      div.textContent = text;
    } else {
      div.textContent = text;
    }
    messagesEl.appendChild(div);
    messagesEl.scrollTop = messagesEl.scrollHeight;
    return div;
  }

  function setThinking(on) {
    thinkingEl.className = on ? 'visible' : '';
  }

  function setSending(on) {
    waitingForResponse = on;
    sendBtn.disabled = on;
    setThinking(on);
  }

  function setStatus(s, label) {
    statusDot.className = s;
    statusText.textContent = label || s.charAt(0).toUpperCase() + s.slice(1);
  }

  // --- Sending ---

  function sendMessage() {
    const text = promptEl.value.trim();
    if (!text || waitingForResponse) { return; }

    addMessage('user', text);
    promptEl.value = '';
    promptEl.style.height = 'auto';
    setSending(true);
    streamingMsg = null;
    streamBuffer = '';

    vscode.postMessage({ type: 'send', text });
  }

  // --- Event listeners ---

  sendBtn.addEventListener('click', sendMessage);

  promptEl.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      sendMessage();
    }
  });

  // Auto-grow textarea
  promptEl.addEventListener('input', () => {
    promptEl.style.height = 'auto';
    promptEl.style.height = Math.min(promptEl.scrollHeight, 160) + 'px';
  });

  btnClear.addEventListener('click', () => {
    vscode.postMessage({ type: 'clear' });
  });

  btnReconnect.addEventListener('click', () => {
    vscode.postMessage({ type: 'restart' });
  });

  // --- Messages from extension ---

  window.addEventListener('message', (event) => {
    const msg = event.data;

    switch (msg.type) {
      case 'user':
        // Code-action commands inject user bubbles this way
        addMessage('user', msg.text);
        setSending(true);
        streamBuffer = '';
        streamingMsg = null;
        break;

      case 'partial':
        // Progressive streaming: append to a growing assistant bubble
        streamBuffer += msg.text + '\\n';
        if (!streamingMsg) {
          streamingMsg = addMessage('assistant', '');
        }
        streamingMsg.innerHTML = renderMarkdown(streamBuffer);
        messagesEl.scrollTop = messagesEl.scrollHeight;
        break;

      case 'response':
        // Full response (non-streaming path or final flush)
        setThinking(false);
        if (streamingMsg) {
          streamingMsg.innerHTML = renderMarkdown(msg.text);
          streamingMsg = null;
          streamBuffer = '';
        } else {
          addMessage('assistant', msg.text);
        }
        break;

      case 'done':
        setSending(false);
        streamingMsg = null;
        streamBuffer = '';
        break;

      case 'error':
        setSending(false);
        addMessage('error', msg.text);
        streamingMsg = null;
        streamBuffer = '';
        break;

      case 'system':
        addMessage('system', msg.text);
        break;

      case 'status':
        const labels = {
          connected:    'Connected to Anvil',
          connecting:   'Connecting...',
          disconnected: 'Disconnected',
          error:        'Connection error',
        };
        setStatus(msg.status, labels[msg.status] || msg.status);
        break;

      case 'clear':
        while (messagesEl.firstChild) { messagesEl.removeChild(messagesEl.firstChild); }
        streamingMsg = null;
        streamBuffer = '';
        break;
    }
  });

  // Signal webview is ready
  vscode.postMessage({ type: 'ready' });
})();
</script>
</body>
</html>`;
    }
    dispose() {
        this.anvilProc?.stop();
    }
}
exports.AnvilChatProvider = AnvilChatProvider;
AnvilChatProvider.viewId = 'anvil.chatView';
function generateNonce() {
    let text = '';
    const possible = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789';
    for (let i = 0; i < 32; i++) {
        text += possible.charAt(Math.floor(Math.random() * possible.length));
    }
    return text;
}
//# sourceMappingURL=chatProvider.js.map