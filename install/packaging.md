# Anvil v2.2.7 — Package Build Guide

## Building .deb (Debian/Ubuntu)

Install cargo-deb:

```sh
cargo install cargo-deb
```

Add the following to `crates/anvil-cli/Cargo.toml` under `[package.metadata.deb]`:

```toml
[package.metadata.deb]
maintainer = "Culpur Defense Engineering <jyoung@culpur.net>"
copyright = "2025, Culpur Defense"
license-file = ["../../LICENSE", "0"]
extended-description = """\
Anvil is a full-featured AI coding assistant with multi-provider support \
(Anthropic, OpenAI, Google, xAI, Ollama), an encrypted vault for API keys, \
and the AnvilHub package ecosystem."""
depends = "$auto, libc6 (>= 2.35), ca-certificates, git"
recommends = "nodejs (>= 18), npm"
section = "devel"
priority = "optional"
assets = [
    ["target/release/anvil", "usr/bin/", "755"],
    ["install/completions/anvil.bash", "usr/share/bash-completion/completions/anvil", "644"],
    ["install/completions/anvil.zsh", "usr/share/zsh/vendor-completions/_anvil", "644"],
    ["install/completions/anvil.fish", "usr/share/fish/completions/anvil.fish", "644"],
    ["install/completions/anvil.ps1", "usr/share/anvil/completions/anvil.ps1", "644"],
]
maintainer-scripts = "install/debian/"
```

Build:

```sh
cargo build --release -p anvil-cli
cargo deb -p anvil-cli
# Output: target/debian/anvil_2.2.7_amd64.deb
```

Install:

```sh
sudo dpkg -i target/debian/anvil_2.2.7_amd64.deb
anvil --setup
```

---

## Building .rpm (RHEL/Fedora)

Install cargo-generate-rpm:

```sh
cargo install cargo-generate-rpm
```

Add to `crates/anvil-cli/Cargo.toml`:

```toml
[package.metadata.generate-rpm]
assets = [
    { source = "target/release/anvil", dest = "/usr/bin/anvil", mode = "755" },
    { source = "install/completions/anvil.bash", dest = "/usr/share/bash-completion/completions/anvil", mode = "644" },
    { source = "install/completions/anvil.zsh", dest = "/usr/share/zsh/site-functions/_anvil", mode = "644" },
    { source = "install/completions/anvil.fish", dest = "/usr/share/fish/vendor_completions.d/anvil.fish", mode = "644" },
]

[package.metadata.generate-rpm.requires]
glibc = ">= 2.35"
ca-certificates = "*"
git = "*"
```

Build:

```sh
cargo build --release -p anvil-cli
cargo generate-rpm -p crates/anvil-cli
# Output: target/generate-rpm/anvil-2.2.7-1.aarch64.rpm  (or x86_64)
```

Install:

```sh
sudo rpm -i target/generate-rpm/anvil-2.2.7-1.*.rpm
# or
sudo dnf install target/generate-rpm/anvil-2.2.7-1.*.rpm
anvil --setup
```

---

## Hosting on anvilhub.culpur.net

The installer scripts must be served at:

- `https://anvilhub.culpur.net/install.sh`
- `https://anvilhub.culpur.net/install.ps1`

### Option A — Static files in Next.js `public/` directory

Place files in the AnvilHub web project:

```
anvilhub-web/
  public/
    install.sh
    install.ps1
    completions/
      anvil.bash
      anvil.zsh
      anvil.fish
      anvil.ps1
```

Next.js serves `public/` at the root automatically. After copying:

```sh
cp install/install.sh anvilhub-web/public/
cp install/install.ps1 anvilhub-web/public/
cp -r install/completions anvilhub-web/public/
npx next build
pm2 restart anvilhub-web
```

### Option B — Route handler (if dynamic headers needed)

Add `anvilhub-web/app/install.sh/route.ts`:

```typescript
import { readFileSync } from 'fs'
import { NextResponse } from 'next/server'

export async function GET() {
  const content = readFileSync('./public/install.sh', 'utf-8')
  return new NextResponse(content, {
    headers: {
      'Content-Type': 'text/plain; charset=utf-8',
      'Cache-Control': 'public, max-age=300',
    },
  })
}
```

### SHA256 files

GitHub Actions should publish `.sha256` companion files alongside each
release binary. Add to `.github/workflows/release.yml`:

```yaml
- name: Generate SHA256 checksums
  run: |
    for f in target/release/anvil-*; do
      sha256sum "$f" | awk '{print $1}' > "${f}.sha256"
    done
```

---

## Windows Manual Test Steps

Automated testing of `install.ps1` requires a Windows machine.
The following steps should be run on a Windows 10 or 11 system:

1. Open PowerShell as a regular user (not Administrator).
2. Run:
   ```powershell
   Set-ExecutionPolicy Bypass -Scope Process -Force
   iwr -useb https://anvilhub.culpur.net/install.ps1 | iex
   ```
3. Verify:
   - `anvil --version` prints `2.2.7`
   - `$env:LOCALAPPDATA\Programs\anvil\anvil.exe` exists
   - `$env:PATH` includes `$env:LOCALAPPDATA\Programs\anvil`
   - `anvil --check` runs and prints a checklist
   - Tab completion works in a new PowerShell session
4. Uninstall test:
   ```powershell
   anvil --uninstall
   ```
   Verify binary removed, data directory prompt appears.
