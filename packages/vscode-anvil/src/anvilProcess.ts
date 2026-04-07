import * as cp from 'child_process';
import * as readline from 'readline';
import { EventEmitter } from 'events';

export interface AnvilResponse {
  type: 'text' | 'error' | 'done';
  content: string;
}

export class AnvilProcess extends EventEmitter {
  private proc: cp.ChildProcess | null = null;
  private rl: readline.Interface | null = null;
  private anvilPath: string;
  private model: string;
  private provider: string;
  private buffer = '';
  private ready = false;

  constructor(anvilPath: string, model: string, provider: string) {
    super();
    this.anvilPath = anvilPath;
    this.model = model;
    this.provider = provider;
  }

  start(): Promise<void> {
    return new Promise((resolve, reject) => {
      const args = ['--model', this.model, '--provider', this.provider, '--repl'];

      try {
        this.proc = cp.spawn(this.anvilPath, args, {
          stdio: ['pipe', 'pipe', 'pipe'],
          env: { ...process.env },
        });
      } catch (err) {
        reject(new Error(`Failed to spawn anvil: ${err}`));
        return;
      }

      if (!this.proc.stdout || !this.proc.stdin || !this.proc.stderr) {
        reject(new Error('Failed to open stdio pipes'));
        return;
      }

      this.proc.on('error', (err) => {
        this.emit('error', err);
        reject(err);
      });

      this.proc.on('exit', (code, signal) => {
        this.ready = false;
        this.emit('exit', code, signal);
      });

      this.proc.stderr.on('data', (data: Buffer) => {
        const text = data.toString();
        // Surface stderr lines that look like real errors, not startup noise
        if (text.toLowerCase().includes('error') || text.toLowerCase().includes('fatal')) {
          this.emit('response', { type: 'error', content: text.trim() } as AnvilResponse);
        }
      });

      this.rl = readline.createInterface({ input: this.proc.stdout });

      // Wait for the first prompt line that signals Anvil is ready.
      // Anvil REPL typically prints a banner then "anvil> " or similar.
      // We resolve after 800 ms of silence so we never hang forever.
      let resolved = false;
      const timer = setTimeout(() => {
        if (!resolved) {
          resolved = true;
          this.ready = true;
          resolve();
        }
      }, 800);

      this.rl.on('line', (line) => {
        // Detect REPL ready prompt (handles several likely formats)
        if (!resolved && (line.includes('anvil>') || line.includes('ready') || line.trim() === '')) {
          clearTimeout(timer);
          resolved = true;
          this.ready = true;
          resolve();
          return;
        }

        if (resolved) {
          this.handleOutputLine(line);
        }
      });
    });
  }

  private handleOutputLine(line: string): void {
    // Detect end-of-response sentinel.  We look for a blank line that
    // follows content, or the REPL prompt re-appearing.
    if (line.includes('anvil>')) {
      if (this.buffer.trim()) {
        this.emit('response', { type: 'text', content: this.buffer.trimEnd() } as AnvilResponse);
        this.buffer = '';
      }
      this.emit('response', { type: 'done', content: '' } as AnvilResponse);
      return;
    }

    this.buffer += line + '\n';

    // Stream partial lines so the UI can show progressive output.
    this.emit('partial', line);
  }

  send(prompt: string): boolean {
    if (!this.ready || !this.proc?.stdin) {
      return false;
    }
    this.buffer = '';
    this.proc.stdin.write(prompt + '\n');
    return true;
  }

  isReady(): boolean {
    return this.ready;
  }

  stop(): void {
    this.ready = false;
    if (this.proc) {
      try {
        this.proc.stdin?.end();
        this.proc.kill('SIGTERM');
      } catch {
        // best effort
      }
      this.proc = null;
    }
    if (this.rl) {
      this.rl.close();
      this.rl = null;
    }
  }

  restart(anvilPath: string, model: string, provider: string): Promise<void> {
    this.anvilPath = anvilPath;
    this.model = model;
    this.provider = provider;
    this.stop();
    return this.start();
  }
}

// ---------------------------------------------------------------------------
// One-shot execution helper used by code-action commands (explain / fix / etc.)
// Does NOT use the persistent REPL — just spawns anvil with -p <prompt> and
// collects stdout.  Much simpler and avoids REPL state contamination.
// ---------------------------------------------------------------------------
export function runAnvilOneShot(
  anvilPath: string,
  model: string,
  provider: string,
  prompt: string,
  onData: (chunk: string) => void,
  signal?: AbortSignal
): Promise<void> {
  return new Promise((resolve, reject) => {
    const args = ['--model', model, '--provider', provider, '-p', prompt];

    let proc: cp.ChildProcess;
    try {
      proc = cp.spawn(anvilPath, args, {
        stdio: ['ignore', 'pipe', 'pipe'],
        env: { ...process.env },
      });
    } catch (err) {
      reject(new Error(`Failed to spawn anvil: ${err}`));
      return;
    }

    signal?.addEventListener('abort', () => {
      try { proc.kill('SIGTERM'); } catch { /* ignore */ }
    });

    proc.stdout?.on('data', (data: Buffer) => {
      onData(data.toString());
    });

    let stderr = '';
    proc.stderr?.on('data', (d: Buffer) => { stderr += d.toString(); });

    proc.on('error', reject);
    proc.on('exit', (code) => {
      if (code === 0 || code === null) {
        resolve();
      } else {
        reject(new Error(`anvil exited ${code}: ${stderr.trim()}`));
      }
    });
  });
}
