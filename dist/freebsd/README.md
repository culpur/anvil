# FreeBSD Port Skeleton

This directory contains a skeleton FreeBSD ports `Makefile` for Anvil.

## Purpose

The `Makefile` here documents how a FreeBSD packager would create a port for
submission to the FreeBSD ports tree. It is not yet submitted. Until an official
port lands, FreeBSD users have two installation options:

**Option 1 — curl installer (recommended):**
```sh
curl -fsSL https://anvilhub.culpur.net/install.sh | sh
```
The installer detects FreeBSD and downloads the correct pre-built binary from
the GitHub release.

**Option 2 — direct binary download:**
```sh
# x86_64
fetch https://github.com/culpur/anvil/releases/latest/download/anvil-x86_64-unknown-freebsd
chmod +x anvil-x86_64-unknown-freebsd
sudo mv anvil-x86_64-unknown-freebsd /usr/local/bin/anvil

# ARM64
fetch https://github.com/culpur/anvil/releases/latest/download/anvil-aarch64-unknown-freebsd
chmod +x anvil-aarch64-unknown-freebsd
sudo mv anvil-aarch64-unknown-freebsd /usr/local/bin/anvil
```

## Submitting to the ports tree

To submit this port officially:

1. Install `ports-mgmt/poudriere` and build + test the port in a clean jail.
2. Run `portlint` and `poudriere testport` to verify compliance.
3. Submit a diff to the FreeBSD bug tracker (bugs.freebsd.org) with category
   `ports` and component `Individual Port(s)`.

See the [Porter's Handbook](https://docs.freebsd.org/en/books/porters-handbook/)
for full submission guidelines.
