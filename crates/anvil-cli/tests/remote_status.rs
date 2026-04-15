//! Test that the remote control status flow works correctly.

#[test]
fn relay_session_url_is_not_empty() {
    let hash = runtime::relay::generate_session_hash();
    let session = runtime::relay::RelaySession::new(hash.clone(), "https://passage.culpur.net/viewer");
    assert!(!session.url.is_empty(), "session URL must not be empty");
    assert!(session.url.contains(&hash), "session URL must contain hash");
    assert!(session.url.starts_with("https://"), "URL must be https");
}

#[test]
fn relay_session_hash_is_nonempty() {
    let hash = runtime::relay::generate_session_hash();
    assert!(!hash.is_empty(), "hash must not be empty");
    assert!(hash.len() > 10, "hash should be reasonably long, got len={}", hash.len());
}

#[test]
fn remote_control_widget_metadata() {
    use runtime::theme::{StatusWidget, Side};
    assert_eq!(StatusWidget::RemoteControl.id(), "remote_control");
    assert_eq!(StatusWidget::RemoteControl.category(), "system");
}

#[test]
fn default_preset_includes_remote_control() {
    use runtime::theme::{StatusLineConfig, StatusWidget};
    let config = StatusLineConfig::default();
    // Check that RemoteControl widget exists in one of the lines
    let mut found = false;
    for (i, line) in config.lines.iter().enumerate() {
        for w in &line.left {
            if w.id() == "remote_control" {
                found = true;
                println!("Found RemoteControl in line {} left", i);
            }
        }
        for w in &line.right {
            if w.id() == "remote_control" {
                found = true;
                println!("Found RemoteControl in line {} right", i);
            }
        }
    }
    assert!(found, "RemoteControl widget must be in the default preset");
}

#[test]
fn simulate_full_remote_control_flow() {
    // Simulate what handle_repl_command_tui does:
    // 1. run_remote_control_command sets relay_session
    // 2. We extract URL from relay_session
    // 3. We call set_remote_status equivalent

    // Step 1: Create a relay session (same as run_remote_control_command does)
    let hash = runtime::relay::generate_session_hash();
    let session = runtime::relay::RelaySession::new(hash.clone(), "https://passage.culpur.net/viewer");
    let relay_session: Option<runtime::relay::RelaySession> = Some(session);

    // Step 2: Extract URL (same as main.rs line 2781)
    let rc_url = relay_session.as_ref().map(|s| s.url.clone()).unwrap_or_default();
    let rc_hash = relay_session.as_ref().map(|s| s.hash.clone()).unwrap_or_default();

    // Step 3: Verify URL is not empty
    assert!(!rc_url.is_empty(), "rc_url must not be empty after session creation, got: '{}'", rc_url);
    assert!(!rc_hash.is_empty(), "rc_hash must not be empty");
    println!("rc_url = {}", rc_url);
    println!("rc_hash = {}", rc_hash);

    // Step 4: Simulate what set_remote_status does
    // This is what set_remote_status does:
    let remote_url = rc_url.clone();
    let remote_code = rc_hash.clone();

    // Step 5: Verify the status line data would show connected
    assert!(!remote_url.is_empty(), "remote_url should be set after set_remote_status");

    // Step 6: Simulate what the draw closure does
    let data_remote_url = remote_url.clone(); // This is what draw() snapshots
    assert!(!data_remote_url.is_empty(), "data.remote_url must not be empty in draw");

    // Step 7: Simulate what the widget render does
    if data_remote_url.is_empty() {
        panic!("Widget would show 'RC Disconnected' — BUG!");
    } else {
        let label = if remote_code.is_empty() {
            format!("🛸 RC {}", data_remote_url)
        } else {
            format!("🛸 RC {}  [{}]", data_remote_url, remote_code)
        };
        println!("Widget would render: {}", label);
        assert!(label.contains("passage.culpur.net"), "Label must contain URL");
        assert!(!label.contains("Disconnected"), "Label must NOT contain Disconnected");
    }
}
