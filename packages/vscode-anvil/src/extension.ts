import * as vscode from 'vscode';
import { AnvilChatProvider } from './chatProvider';
import { runAnvilOneShot } from './anvilProcess';

// ---------------------------------------------------------------------------
// Extension state
// ---------------------------------------------------------------------------
let statusBarItem: vscode.StatusBarItem;
let chatProvider: AnvilChatProvider;

// ---------------------------------------------------------------------------
// activate — called once when the extension loads
// ---------------------------------------------------------------------------
export function activate(context: vscode.ExtensionContext): void {
  chatProvider = new AnvilChatProvider(context.extensionUri);

  // Register the sidebar webview provider.
  context.subscriptions.push(
    vscode.window.registerWebviewViewProvider(
      AnvilChatProvider.viewId,
      chatProvider,
      { webviewOptions: { retainContextWhenHidden: true } }
    )
  );

  // Status bar
  statusBarItem = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 100);
  statusBarItem.command = 'anvil.chat';
  updateStatusBar('idle');
  statusBarItem.show();
  context.subscriptions.push(statusBarItem);

  // Probe Anvil version in the background so the status bar shows something useful.
  probeAnvilVersion();

  // -------------------------------------------------------------------------
  // Command: Open Chat
  // -------------------------------------------------------------------------
  context.subscriptions.push(
    vscode.commands.registerCommand('anvil.chat', async () => {
      await vscode.commands.executeCommand('workbench.view.extension.anvil-sidebar');
    })
  );

  // -------------------------------------------------------------------------
  // Command: Open Terminal (dedicated Anvil REPL terminal)
  // -------------------------------------------------------------------------
  context.subscriptions.push(
    vscode.commands.registerCommand('anvil.terminal', () => {
      const cfg = vscode.workspace.getConfiguration('anvil');
      const path = cfg.get<string>('path', 'anvil');
      const model = cfg.get<string>('model', 'claude-sonnet-4-6');
      const provider = cfg.get<string>('provider', 'anthropic');

      const terminal = vscode.window.createTerminal({
        name: 'Anvil',
        shellPath: path,
        shellArgs: ['--model', model, '--provider', provider],
      });
      terminal.show();
    })
  );

  // -------------------------------------------------------------------------
  // Command: Browse Hub
  // -------------------------------------------------------------------------
  context.subscriptions.push(
    vscode.commands.registerCommand('anvil.hub', async () => {
      const cfg = vscode.workspace.getConfiguration('anvil');
      const path = cfg.get<string>('path', 'anvil');

      const terminal = vscode.window.createTerminal({
        name: 'Anvil Hub',
        shellPath: path,
        shellArgs: ['hub'],
      });
      terminal.show();
    })
  );

  // -------------------------------------------------------------------------
  // Code-action commands — Explain, Refactor, Test, Fix, Docs
  // These all follow the same pattern:
  //   1. Get selected text from the active editor
  //   2. Build a prompt
  //   3. Show a progress notification while running Anvil one-shot
  //   4. Display result in output channel + optionally inject into chat
  // -------------------------------------------------------------------------
  const outputChannel = vscode.window.createOutputChannel('Anvil');
  context.subscriptions.push(outputChannel);

  registerCodeAction(context, outputChannel, 'anvil.explain', buildExplainPrompt);
  registerCodeAction(context, outputChannel, 'anvil.refactor', buildRefactorPrompt);
  registerCodeAction(context, outputChannel, 'anvil.test', buildTestPrompt);
  registerCodeAction(context, outputChannel, 'anvil.fix', buildFixPrompt);
  registerCodeAction(context, outputChannel, 'anvil.docs', buildDocsPrompt);

  // -------------------------------------------------------------------------
  // React to settings changes (model / provider / path)
  // -------------------------------------------------------------------------
  context.subscriptions.push(
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (e.affectsConfiguration('anvil')) {
        probeAnvilVersion();
      }
    })
  );
}

// ---------------------------------------------------------------------------
// deactivate — called when the extension is unloaded
// ---------------------------------------------------------------------------
export function deactivate(): void {
  chatProvider?.dispose();
  statusBarItem?.dispose();
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function registerCodeAction(
  context: vscode.ExtensionContext,
  outputChannel: vscode.OutputChannel,
  commandId: string,
  buildPrompt: (code: string, languageId: string, fileName: string) => string
): void {
  context.subscriptions.push(
    vscode.commands.registerCommand(commandId, async () => {
      const editor = vscode.window.activeTextEditor;
      if (!editor) {
        vscode.window.showWarningMessage('Anvil: No active editor.');
        return;
      }

      const selection = editor.selection;
      const code = editor.document.getText(selection.isEmpty ? undefined : selection);
      if (!code.trim()) {
        vscode.window.showWarningMessage('Anvil: Nothing selected (or file is empty).');
        return;
      }

      const languageId = editor.document.languageId;
      const fileName = editor.document.fileName.split('/').pop() ?? 'file';
      const prompt = buildPrompt(code, languageId, fileName);

      const cfg = vscode.workspace.getConfiguration('anvil');
      const anvilPath = cfg.get<string>('path', 'anvil');
      const model = cfg.get<string>('model', 'claude-sonnet-4-6');
      const provider = cfg.get<string>('provider', 'anthropic');

      updateStatusBar('working');
      outputChannel.clear();
      outputChannel.show(true);
      outputChannel.appendLine(`[Anvil] ${commandId} — ${fileName}`);
      outputChannel.appendLine('─'.repeat(60));

      const abort = new AbortController();

      await vscode.window.withProgress(
        {
          location: vscode.ProgressLocation.Notification,
          title: `Anvil: ${commandId.replace('anvil.', '')}`,
          cancellable: true,
        },
        async (_progress, token) => {
          token.onCancellationRequested(() => abort.abort());

          try {
            await runAnvilOneShot(
              anvilPath,
              model,
              provider,
              prompt,
              (chunk) => {
                outputChannel.append(chunk);
              },
              abort.signal
            );
            outputChannel.appendLine('\n' + '─'.repeat(60));
            outputChannel.appendLine('[Anvil] Done.');
          } catch (err) {
            if (!abort.signal.aborted) {
              outputChannel.appendLine(`\n[Anvil] Error: ${err}`);
              vscode.window.showErrorMessage(`Anvil error: ${err}`);
            }
          }
        }
      );

      updateStatusBar('idle');
    })
  );
}

// ---------------------------------------------------------------------------
// Prompt builders
// ---------------------------------------------------------------------------

function buildExplainPrompt(code: string, languageId: string, fileName: string): string {
  return [
    `Explain the following ${languageId} code from \`${fileName}\`.`,
    `Be concise and focus on what it does, any important patterns used, and anything notable or potentially problematic.`,
    '',
    '```' + languageId,
    code,
    '```',
  ].join('\n');
}

function buildRefactorPrompt(code: string, languageId: string, fileName: string): string {
  return [
    `Refactor the following ${languageId} code from \`${fileName}\`.`,
    `Improve readability, performance, and maintainability.`,
    `Show the refactored code with a brief explanation of the changes made.`,
    '',
    '```' + languageId,
    code,
    '```',
  ].join('\n');
}

function buildTestPrompt(code: string, languageId: string, fileName: string): string {
  return [
    `Generate comprehensive unit tests for the following ${languageId} code from \`${fileName}\`.`,
    `Cover happy paths, edge cases, and error conditions.`,
    `Use the most common testing framework for this language.`,
    '',
    '```' + languageId,
    code,
    '```',
  ].join('\n');
}

function buildFixPrompt(code: string, languageId: string, fileName: string): string {
  return [
    `Identify and fix any bugs, errors, or issues in the following ${languageId} code from \`${fileName}\`.`,
    `Show the corrected code and explain what was wrong and why the fix works.`,
    '',
    '```' + languageId,
    code,
    '```',
  ].join('\n');
}

function buildDocsPrompt(code: string, languageId: string, fileName: string): string {
  return [
    `Generate documentation for the following ${languageId} code from \`${fileName}\`.`,
    `Use the idiomatic doc format for this language (JSDoc, docstrings, etc.).`,
    `Document all public functions, classes, parameters, and return values.`,
    '',
    '```' + languageId,
    code,
    '```',
  ].join('\n');
}

// ---------------------------------------------------------------------------
// Status bar
// ---------------------------------------------------------------------------

function updateStatusBar(state: 'idle' | 'working' | 'error'): void {
  switch (state) {
    case 'idle':
      statusBarItem.text = '$(anvil~spin) Anvil';
      statusBarItem.text = '$(circuit-board) Anvil';
      statusBarItem.tooltip = 'Anvil AI — click to open chat (Cmd+Shift+A)';
      statusBarItem.backgroundColor = undefined;
      break;
    case 'working':
      statusBarItem.text = '$(loading~spin) Anvil';
      statusBarItem.tooltip = 'Anvil is working...';
      statusBarItem.backgroundColor = undefined;
      break;
    case 'error':
      statusBarItem.text = '$(warning) Anvil';
      statusBarItem.tooltip = 'Anvil: error — check PATH or anvil.path setting';
      statusBarItem.backgroundColor = new vscode.ThemeColor('statusBarItem.errorBackground');
      break;
  }
}

async function probeAnvilVersion(): Promise<void> {
  const cfg = vscode.workspace.getConfiguration('anvil');
  const anvilPath = cfg.get<string>('path', 'anvil');

  try {
    let version = '';
    await runAnvilOneShot(anvilPath, '', '', '--version', (chunk) => { version += chunk; });
    const v = version.trim().split('\n')[0] ?? 'Anvil';
    statusBarItem.text = `$(circuit-board) ${v}`;
    statusBarItem.tooltip = `${v} — click to open chat (Cmd+Shift+A)`;
  } catch {
    // If --version fails, the binary may still be usable — don't mark as error yet.
    statusBarItem.text = '$(circuit-board) Anvil';
    statusBarItem.tooltip = 'Anvil AI — click to open chat (Cmd+Shift+A)';
  }
}
