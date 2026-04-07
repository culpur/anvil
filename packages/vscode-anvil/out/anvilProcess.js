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
exports.AnvilProcess = void 0;
exports.runAnvilOneShot = runAnvilOneShot;
const cp = __importStar(require("child_process"));
const readline = __importStar(require("readline"));
const events_1 = require("events");
class AnvilProcess extends events_1.EventEmitter {
    constructor(anvilPath, model, provider) {
        super();
        this.proc = null;
        this.rl = null;
        this.buffer = '';
        this.ready = false;
        this.anvilPath = anvilPath;
        this.model = model;
        this.provider = provider;
    }
    start() {
        return new Promise((resolve, reject) => {
            const args = ['--model', this.model, '--provider', this.provider, '--repl'];
            try {
                this.proc = cp.spawn(this.anvilPath, args, {
                    stdio: ['pipe', 'pipe', 'pipe'],
                    env: { ...process.env },
                });
            }
            catch (err) {
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
            this.proc.stderr.on('data', (data) => {
                const text = data.toString();
                // Surface stderr lines that look like real errors, not startup noise
                if (text.toLowerCase().includes('error') || text.toLowerCase().includes('fatal')) {
                    this.emit('response', { type: 'error', content: text.trim() });
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
    handleOutputLine(line) {
        // Detect end-of-response sentinel.  We look for a blank line that
        // follows content, or the REPL prompt re-appearing.
        if (line.includes('anvil>')) {
            if (this.buffer.trim()) {
                this.emit('response', { type: 'text', content: this.buffer.trimEnd() });
                this.buffer = '';
            }
            this.emit('response', { type: 'done', content: '' });
            return;
        }
        this.buffer += line + '\n';
        // Stream partial lines so the UI can show progressive output.
        this.emit('partial', line);
    }
    send(prompt) {
        if (!this.ready || !this.proc?.stdin) {
            return false;
        }
        this.buffer = '';
        this.proc.stdin.write(prompt + '\n');
        return true;
    }
    isReady() {
        return this.ready;
    }
    stop() {
        this.ready = false;
        if (this.proc) {
            try {
                this.proc.stdin?.end();
                this.proc.kill('SIGTERM');
            }
            catch {
                // best effort
            }
            this.proc = null;
        }
        if (this.rl) {
            this.rl.close();
            this.rl = null;
        }
    }
    restart(anvilPath, model, provider) {
        this.anvilPath = anvilPath;
        this.model = model;
        this.provider = provider;
        this.stop();
        return this.start();
    }
}
exports.AnvilProcess = AnvilProcess;
// ---------------------------------------------------------------------------
// One-shot execution helper used by code-action commands (explain / fix / etc.)
// Does NOT use the persistent REPL — just spawns anvil with -p <prompt> and
// collects stdout.  Much simpler and avoids REPL state contamination.
// ---------------------------------------------------------------------------
function runAnvilOneShot(anvilPath, model, provider, prompt, onData, signal) {
    return new Promise((resolve, reject) => {
        const args = ['--model', model, '--provider', provider, '-p', prompt];
        let proc;
        try {
            proc = cp.spawn(anvilPath, args, {
                stdio: ['ignore', 'pipe', 'pipe'],
                env: { ...process.env },
            });
        }
        catch (err) {
            reject(new Error(`Failed to spawn anvil: ${err}`));
            return;
        }
        signal?.addEventListener('abort', () => {
            try {
                proc.kill('SIGTERM');
            }
            catch { /* ignore */ }
        });
        proc.stdout?.on('data', (data) => {
            onData(data.toString());
        });
        let stderr = '';
        proc.stderr?.on('data', (d) => { stderr += d.toString(); });
        proc.on('error', reject);
        proc.on('exit', (code) => {
            if (code === 0 || code === null) {
                resolve();
            }
            else {
                reject(new Error(`anvil exited ${code}: ${stderr.trim()}`));
            }
        });
    });
}
//# sourceMappingURL=anvilProcess.js.map