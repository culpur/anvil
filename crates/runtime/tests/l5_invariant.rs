//! Phase 3.6 L5 invariant integration test [SECURITY].
//!
//! Zero-injection contract: no L5 byte (vault content, encrypted private
//! project memory) may appear in:
//!   - The rendered system prompt
//!   - The L7 file cache (file-cache entries or their bodies)
//!   - The L7 command cache (entries or stdout/stderr)
//!
//! This is the safety-net test for Phase 3. If a future commit introduces
//! a leak — by piping vault content through any of the cache or prompt
//! surfaces — this test fails. Do NOT relax the assertions: fix the leak.
//!
//! The test uses well-known sentinel values that are recognisable in any
//! byte buffer they end up in:
//!   - `l5_sentinel_value_THIS_MUST_NOT_LEAK`
//!   - `l5_private_host_DO_NOT_LEAK`
//!
//! Both vault-unlocked AND vault-locked states are exercised.

// SAFETY: env::set_var is unsafe in Rust 2024. This test is single-process
// and serial; we restore the env on the way out.
#![allow(unsafe_code)]

use std::path::PathBuf;

use runtime::{
    CommandCacheManager, Credential, CredentialType, FileCacheManager,
    PrivateProjectMemory, VaultManager,
};

const SENTINEL_VAULT_VALUE: &str = "l5_sentinel_value_THIS_MUST_NOT_LEAK";
const SENTINEL_PRIVATE_HOST: &str = "l5_private_host_DO_NOT_LEAK";
const VAULT_PASSWORD: &str = "phase-3.6-l5-invariant-test-pw";

struct EnvGuard {
    prev_home: Option<std::ffi::OsString>,
    _tempdir: tempfile::TempDir,
}

impl EnvGuard {
    fn install(home: tempfile::TempDir) -> Self {
        let prev_home = std::env::var_os("ANVIL_CONFIG_HOME");
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", home.path()); }
        Self {
            prev_home,
            _tempdir: home,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.prev_home {
            Some(v) => unsafe { std::env::set_var("ANVIL_CONFIG_HOME", v); },
            None => unsafe { std::env::remove_var("ANVIL_CONFIG_HOME"); },
        }
    }
}

/// Stand up a fresh vault, unlock it, store sentinel data, then exercise
/// the prompt + cache surfaces and assert NO sentinel byte appears anywhere.
#[test]
fn l5_invariant_no_vault_byte_leaks_to_cache_or_prompt() {
    let home = tempfile::tempdir().expect("home tempdir");
    let home_path = home.path().to_path_buf();
    let _env = EnvGuard::install(home);

    // ── Step 1-2: setup + unlock vault, store the sentinel credential.
    let vault_dir = home_path.join("vault");
    let mut vm = VaultManager::new(vault_dir);
    vm.setup(VAULT_PASSWORD).expect("vault setup");
    vm.unlock(VAULT_PASSWORD).expect("vault unlock");

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let cred = Credential {
        label: "l5_sentinel_label".to_string(),
        username: None,
        secret: SENTINEL_VAULT_VALUE.to_string(),
        notes: None,
        created_at: now,
        credential_type: CredentialType::default(),
        url: None,
        tags: Vec::new(),
        updated_at: 0,
        expires_at: None,
        last_rotated: None,
        metadata: serde_json::Value::Object(serde_json::Map::new()),
    };
    vm.upsert_credential(&cred).expect("store sentinel credential");

    // ── Step 3: store a private memory entry. Use a tempdir as the project
    // root and feed its hash-derived path into PrivateProjectMemory.
    let project = tempfile::tempdir().expect("project tempdir");
    let private = PrivateProjectMemory::for_project(project.path());
    let kek = vm.master_key_for_session().expect("kek").to_owned();
    private
        .add_entry(&kek, "hostname", &format!("hostname={SENTINEL_PRIVATE_HOST}"))
        .expect("store private infra");

    // ── Step 4-7: render system prompt, walk caches, assert no sentinel.
    assert_no_l5_byte_in_surfaces(project.path(), &home_path, "unlocked");

    // ── Step 9: lock the vault, repeat assertions.
    vm.lock();
    assert!(!vm.is_unlocked(), "vault must be locked after lock()");
    assert_no_l5_byte_in_surfaces(project.path(), &home_path, "locked");
}

/// Run the prompt build + cache walk, then check every byte for sentinels.
///
/// `state` is just a label embedded in assertion failures so the test
/// output distinguishes the unlocked vs locked code path.
fn assert_no_l5_byte_in_surfaces(project_root: &std::path::Path, home: &std::path::Path, state: &str) {
    use runtime::load_system_prompt_sections_with_identity;

    // Render the full system prompt with default identity. The 1M-context
    // bug fix that ensured ANVIL.md + ProjectContext flow through here
    // means a leak would manifest as the sentinel appearing in `.body`.
    let sections = load_system_prompt_sections_with_identity(
        project_root,
        "2026-05-13",
        "TestOS",
        "1.0",
        None,
        None,
        None,
    )
    .expect("prompt build");

    let rendered = sections
        .iter()
        .map(|s| s.body.clone())
        .collect::<Vec<_>>()
        .join("\n\n");

    assert!(
        !rendered.contains(SENTINEL_VAULT_VALUE),
        "[{state}] L5 vault sentinel leaked into rendered system prompt!\n\
         Prompt size: {} bytes\n\
         Found at offset: {:?}",
        rendered.len(),
        rendered.find(SENTINEL_VAULT_VALUE),
    );
    assert!(
        !rendered.contains(SENTINEL_PRIVATE_HOST),
        "[{state}] L5 private-memory sentinel leaked into rendered system prompt!\n\
         Prompt size: {} bytes\n\
         Found at offset: {:?}",
        rendered.len(),
        rendered.find(SENTINEL_PRIVATE_HOST),
    );

    // Walk the L7 file cache. No path may contain "vault", no body may
    // contain either sentinel.
    let fc = FileCacheManager::new(project_root.to_path_buf())
        .expect("file cache manager");
    let entries = fc.list().expect("file cache list");
    for entry in entries {
        let path_str = entry.path.to_string_lossy().to_lowercase();
        assert!(
            !path_str.contains("vault"),
            "[{state}] file-cache entry path contains 'vault': {:?}",
            entry.path
        );
        if let Some(s) = &entry.summary {
            assert!(
                !s.contains(SENTINEL_VAULT_VALUE),
                "[{state}] file-cache summary contains vault sentinel"
            );
            assert!(
                !s.contains(SENTINEL_PRIVATE_HOST),
                "[{state}] file-cache summary contains private sentinel"
            );
        }
        for sym in &entry.key_symbols {
            assert!(
                !sym.contains(SENTINEL_VAULT_VALUE),
                "[{state}] file-cache key_symbols contain vault sentinel"
            );
            assert!(
                !sym.contains(SENTINEL_PRIVATE_HOST),
                "[{state}] file-cache key_symbols contain private sentinel"
            );
        }
    }

    // Walk the L7 command cache. No body and no touched_files may carry
    // the sentinel.
    let cc = CommandCacheManager::new(project_root.to_path_buf())
        .expect("command cache manager");
    let entries = cc.list().expect("command cache list");
    for entry in entries {
        assert!(
            !entry.stdout.contains(SENTINEL_VAULT_VALUE),
            "[{state}] cmd-cache stdout contains vault sentinel"
        );
        assert!(
            !entry.stderr.contains(SENTINEL_VAULT_VALUE),
            "[{state}] cmd-cache stderr contains vault sentinel"
        );
        assert!(
            !entry.stdout.contains(SENTINEL_PRIVATE_HOST),
            "[{state}] cmd-cache stdout contains private sentinel"
        );
        assert!(
            !entry.stderr.contains(SENTINEL_PRIVATE_HOST),
            "[{state}] cmd-cache stderr contains private sentinel"
        );
        for p in &entry.touched_files {
            let s = p.to_string_lossy().to_lowercase();
            assert!(
                !s.contains("vault"),
                "[{state}] cmd-cache touched_files contains 'vault': {:?}",
                p
            );
        }
    }

    // Belt-and-braces: also walk the home dir filesystem for the
    // unencrypted byte sequences themselves. The vault.bin and the
    // encrypted private-memory blob will pass — they're encrypted —
    // but any plaintext file containing the sentinel would be flagged.
    let _ = home; // home is canonicalised inside walk_for_plaintext.
    walk_for_plaintext_leaks(home, state);
}

/// Recurse `root` and assert no plaintext file contains either sentinel.
/// Excludes the vault directory itself (vault.bin is ciphertext-only) and
/// the private/ directory (the .enc blob is also ciphertext).
fn walk_for_plaintext_leaks(root: &std::path::Path, state: &str) {
    fn visit(dir: &std::path::Path, state: &str) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            // Skip the encrypted blobs themselves — their contents are
            // ciphertext + nonce + tag, not plaintext.
            if name == "vault" || name == "private" {
                continue;
            }
            // Also skip the auto-promote nominations dir if we don't
            // know its full content (we never write the sentinel there,
            // but Phase 3.3 also REJECTS credential-looking content
            // from nominations, so the dir should be empty/Knowledge-only
            // by construction).
            if path.is_dir() {
                visit(&path, state);
            } else if let Ok(buf) = std::fs::read(&path) {
                let s = String::from_utf8_lossy(&buf);
                assert!(
                    !s.contains(SENTINEL_VAULT_VALUE),
                    "[{state}] plaintext file under home contains vault sentinel: {:?}",
                    path
                );
                assert!(
                    !s.contains(SENTINEL_PRIVATE_HOST),
                    "[{state}] plaintext file under home contains private sentinel: {:?}",
                    path
                );
            }
        }
    }
    visit(root, state);
}

/// Compile-time guard: ensure the sentinel constants stay distinctive.
/// If anyone "cleans up" these strings into shorter forms, the test loses
/// its forensic value.
#[test]
fn sentinel_constants_are_distinctive() {
    assert!(SENTINEL_VAULT_VALUE.len() > 20);
    assert!(SENTINEL_PRIVATE_HOST.len() > 20);
    assert!(SENTINEL_VAULT_VALUE.contains("THIS_MUST_NOT_LEAK"));
    assert!(SENTINEL_PRIVATE_HOST.contains("DO_NOT_LEAK"));
    // Keep the function-using path lint happy without renaming.
    let _ = PathBuf::new();
}
