//! Integration coverage for W3C Trace Context propagation through subprocess
//! environment.
//!
//! These tests live in `tests/` (one binary per file) so each gets a fresh
//! process — `TRACEPARENT` propagation state is gated by a `OnceLock` that
//! cannot be reset, so the only way to exercise the "active context" branch
//! cleanly is in a process whose env we control before any propagation
//! state is initialised.

use std::process::Command;

use runtime::otel::traceparent::{self, TraceContext};

#[test]
fn inject_into_command_writes_traceparent_when_context_active() {
    // Initialise from this process's env.  `TRACEPARENT` is almost certainly
    // unset here, so the initialiser will generate a fresh root for us with
    // `generate_root_when_absent = true`.
    traceparent::init_from_env(true);

    let ctx = traceparent::current().expect(
        "init_from_env(true) must produce a current context when env is empty",
    );
    let trace_id = ctx.trace_id;

    // Build a child command and inject; the child's TRACEPARENT must carry
    // the *same* trace-id as ours (with a fresh child span-id).
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg("printf '%s' \"$TRACEPARENT\"");
    traceparent::inject_into_command(&mut cmd);

    let output = cmd.output().expect("spawn sh subprocess");
    let header = String::from_utf8(output.stdout).expect("utf8 stdout");

    let parsed = TraceContext::parse(&header).unwrap_or_else(|| {
        panic!("child TRACEPARENT must be valid W3C header, got: {header:?}")
    });
    assert_eq!(
        parsed.trace_id, trace_id,
        "child trace-id must match the parent (Anvil) trace-id"
    );
    // Child span-id is freshly generated; just confirm it isn't the same as
    // ours, so the parent edge would be visible in a real collector.
    assert_ne!(
        parsed.span_id, ctx.span_id,
        "child span-id must differ from parent so the parent edge is preserved"
    );
}

#[test]
fn parse_round_trip_through_to_header_is_byte_stable() {
    let header = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01";
    let ctx = TraceContext::parse(header).expect("parse spec example");
    assert_eq!(ctx.to_header(), header);
}

#[test]
fn malformed_traceparent_does_not_panic_and_yields_none() {
    assert!(TraceContext::parse("garbage").is_none());
    assert!(TraceContext::parse("00-not-hex-here").is_none());
    assert!(TraceContext::parse("").is_none());
}
