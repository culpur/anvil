//! W3C Trace Context propagation (RFC: <https://www.w3.org/TR/trace-context/>).
//!
//! ## Why this exists
//!
//! When Anvil is invoked from a parent process that is already part of a
//! distributed trace (CI runner, a wrapping agent, another tool), the parent
//! sets the `TRACEPARENT` environment variable.  W3C-compliant tools then:
//!
//!   1. Parse that header on startup and treat it as the parent span context.
//!   2. Propagate it (verbatim or with a fresh child span-id) to any child
//!      processes and outbound HTTP calls so the whole trace links up.
//!
//! Anvil's previous behaviour was to ignore `TRACEPARENT` entirely, breaking
//! distributed tracing.  This module fixes that gap with two design rules:
//!
//!   - **Never panic on malformed input.**  A bad `TRACEPARENT` from a parent
//!     process must not crash Anvil.  We log to stderr and fall back to a
//!     locally-generated root context.
//!   - **Never silently drop.**  If a parent supplied a valid `TRACEPARENT` it
//!     flows through to every child Anvil spawns and every outbound HTTP
//!     request Anvil makes — at minimum verbatim, ideally with a new
//!     child span-id so the parent edge is preserved in the graph.
//!
//! The CC parity item this addresses is CC-DRIFT-B5: CC v2.1.139 has a known
//! bug (CC issue #58307) where `TRACEPARENT` is silently dropped on the 2nd+
//! invocation when CC's own OTel state exists.  Anvil's contract is the
//! opposite: parent context is honored on every invocation, full stop.
//!
//! ## Format
//!
//! `version-traceid-parentid-flags`
//!
//! ```text
//! 00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01
//! ```
//!
//! - `version`: 2 hex chars (only `00` is currently spec-defined).
//! - `trace-id`: 32 hex chars, must not be all-zero.
//! - `parent-id` (span-id): 16 hex chars, must not be all-zero.
//! - `flags`: 2 hex chars (bit 0 = sampled).

use std::sync::OnceLock;

/// A parsed W3C trace context.  Holds raw byte arrays rather than strings to
/// keep validation tight and serialization deterministic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceContext {
    pub trace_id: [u8; 16],
    pub span_id: [u8; 8],
    pub flags: u8,
}

impl TraceContext {
    /// Generate a fresh root context with a random trace-id and span-id.
    ///
    /// `sampled` controls bit 0 of the flags byte.  Use `true` for normal
    /// operation; the value flows through unchanged in propagation.
    pub fn new_root(sampled: bool) -> Self {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        let mut trace_id = [0_u8; 16];
        let mut span_id = [0_u8; 8];
        // Reject the all-zero case which the spec forbids.  In practice
        // `thread_rng` will never return all zeros, but the loop is cheap
        // insurance against future changes.
        while trace_id.iter().all(|b| *b == 0) {
            rng.fill(&mut trace_id);
        }
        while span_id.iter().all(|b| *b == 0) {
            rng.fill(&mut span_id);
        }
        Self {
            trace_id,
            span_id,
            flags: if sampled { 0x01 } else { 0x00 },
        }
    }

    /// Build a child context that shares this context's trace-id but carries
    /// a freshly-generated span-id.  Flags are inherited.
    ///
    /// Use this when emitting a `TRACEPARENT` to a child process or outbound
    /// HTTP request so the parent edge is preserved in the trace graph.
    #[must_use]
    pub fn new_child(&self) -> Self {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        let mut span_id = [0_u8; 8];
        while span_id.iter().all(|b| *b == 0) {
            rng.fill(&mut span_id);
        }
        Self {
            trace_id: self.trace_id,
            span_id,
            flags: self.flags,
        }
    }

    /// Parse a W3C `TRACEPARENT` header string.
    ///
    /// Returns `None` on any deviation from the spec:
    /// wrong field count, wrong field length, non-hex characters,
    /// unsupported version, all-zero trace-id or span-id.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        // Trim whitespace defensively — some HTTP libs preserve it.
        let value = value.trim();
        let parts: Vec<&str> = value.split('-').collect();
        if parts.len() != 4 {
            return None;
        }
        let (version_s, trace_id_s, span_id_s, flags_s) =
            (parts[0], parts[1], parts[2], parts[3]);

        if version_s.len() != 2 || trace_id_s.len() != 32
            || span_id_s.len() != 16 || flags_s.len() != 2
        {
            return None;
        }

        let version = decode_hex_byte(version_s)?;
        // Spec: version 0xff is reserved as invalid; future versions must be
        // forward-compatible but we conservatively accept only 0x00 today.
        if version != 0x00 {
            return None;
        }

        let mut trace_id = [0_u8; 16];
        decode_hex_into(trace_id_s, &mut trace_id)?;
        if trace_id.iter().all(|b| *b == 0) {
            return None;
        }

        let mut span_id = [0_u8; 8];
        decode_hex_into(span_id_s, &mut span_id)?;
        if span_id.iter().all(|b| *b == 0) {
            return None;
        }

        let flags = decode_hex_byte(flags_s)?;

        Some(Self { trace_id, span_id, flags })
    }

    /// Serialize back to the canonical `version-traceid-spanid-flags` form.
    #[must_use]
    pub fn to_header(&self) -> String {
        let mut out = String::with_capacity(55);
        out.push_str("00-");
        for byte in &self.trace_id {
            out.push_str(&format!("{byte:02x}"));
        }
        out.push('-');
        for byte in &self.span_id {
            out.push_str(&format!("{byte:02x}"));
        }
        out.push('-');
        out.push_str(&format!("{:02x}", self.flags));
        out
    }
}

fn decode_hex_byte(s: &str) -> Option<u8> {
    u8::from_str_radix(s, 16).ok()
}

fn decode_hex_into(s: &str, out: &mut [u8]) -> Option<()> {
    if s.len() != out.len() * 2 {
        return None;
    }
    for (i, slot) in out.iter_mut().enumerate() {
        let pair = s.get(i * 2..i * 2 + 2)?;
        *slot = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(())
}

// ---------------------------------------------------------------------------
// Process-wide state
// ---------------------------------------------------------------------------

/// The current trace context Anvil propagates to children and HTTP calls.
///
/// Set exactly once via `init_from_env` at startup.  `None` means propagation
/// is inactive (env was unset or malformed and we chose not to generate a
/// local root — current policy is to always have *some* context once OTel is
/// enabled, so in practice `Some` after init).
static CURRENT_CONTEXT: OnceLock<Option<TraceContext>> = OnceLock::new();

/// Opaque `TRACESTATE` value to propagate verbatim.  Per spec we never parse
/// or interpret this; vendor-specific data flows through unchanged.
static CURRENT_TRACESTATE: OnceLock<Option<String>> = OnceLock::new();

/// Initialise the propagation state from the environment.
///
/// Looks up `TRACEPARENT` and `TRACESTATE`.  Invalid `TRACEPARENT` logs one
/// warning line to stderr and falls back to a generated root.  `TRACESTATE`
/// is captured verbatim if present (no parse, no validation — spec-required
/// pass-through).
///
/// `generate_root_when_absent` controls behaviour when no `TRACEPARENT` is
/// in the environment: `true` generates a fresh trace-id so Anvil is the
/// root of its own trace; `false` leaves propagation inactive (used by
/// tests).
pub fn init_from_env(generate_root_when_absent: bool) {
    let context = match std::env::var("TRACEPARENT") {
        Ok(value) if !value.is_empty() => match TraceContext::parse(&value) {
            Some(ctx) => Some(ctx),
            None => {
                eprintln!(
                    "[otel] ignoring malformed TRACEPARENT (expected W3C format \
                     `00-<32hex>-<16hex>-<2hex>`); falling back to local root"
                );
                if generate_root_when_absent {
                    Some(TraceContext::new_root(true))
                } else {
                    None
                }
            }
        },
        _ => {
            if generate_root_when_absent {
                Some(TraceContext::new_root(true))
            } else {
                None
            }
        }
    };
    let _ = CURRENT_CONTEXT.set(context);

    let tracestate = std::env::var("TRACESTATE")
        .ok()
        .filter(|v| !v.is_empty());
    let _ = CURRENT_TRACESTATE.set(tracestate);
}

/// Return the propagated trace context, if any.
#[must_use]
pub fn current() -> Option<&'static TraceContext> {
    CURRENT_CONTEXT.get().and_then(|opt| opt.as_ref())
}

/// Return the opaque `TRACESTATE` value to propagate, if any.
#[must_use]
pub fn current_tracestate() -> Option<&'static str> {
    CURRENT_TRACESTATE.get().and_then(|opt| opt.as_deref())
}

/// Serialize the current context as a `TRACEPARENT` header string with a
/// fresh child span-id, so the receiver sees Anvil as their parent.
#[must_use]
pub fn header_for_child() -> Option<String> {
    current().map(|ctx| ctx.new_child().to_header())
}

/// Inject `TRACEPARENT` and (if present) `TRACESTATE` into a
/// `std::process::Command` environment.
///
/// No-op if propagation is inactive.
pub fn inject_into_command(cmd: &mut std::process::Command) {
    if let Some(header) = header_for_child() {
        cmd.env("TRACEPARENT", header);
    }
    if let Some(state) = current_tracestate() {
        cmd.env("TRACESTATE", state);
    }
}

/// Tokio variant of `inject_into_command`.  Same semantics, different type.
pub fn inject_into_tokio_command(cmd: &mut tokio::process::Command) {
    if let Some(header) = header_for_child() {
        cmd.env("TRACEPARENT", header);
    }
    if let Some(state) = current_tracestate() {
        cmd.env("TRACESTATE", state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_example() {
        // Spec example.
        let header = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01";
        let ctx = TraceContext::parse(header).expect("valid header");
        assert_eq!(ctx.flags, 0x01);
        assert_eq!(ctx.trace_id[0], 0x0a);
        assert_eq!(ctx.trace_id[15], 0x9c);
        assert_eq!(ctx.span_id[0], 0xb7);
        assert_eq!(ctx.span_id[7], 0x31);
    }

    #[test]
    fn round_trips_through_to_header() {
        let header = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01";
        let ctx = TraceContext::parse(header).expect("valid header");
        assert_eq!(ctx.to_header(), header);
    }

    #[test]
    fn rejects_wrong_field_count() {
        assert!(TraceContext::parse("00-aaaa-bbbb").is_none());
        assert!(TraceContext::parse("00-aaaa-bbbb-cc-extra").is_none());
    }

    #[test]
    fn rejects_wrong_length() {
        // Trace-id too short.
        assert!(TraceContext::parse("00-0af7-b7ad6b7169203331-01").is_none());
        // Span-id too long.
        assert!(TraceContext::parse(
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b716920333100-01"
        )
        .is_none());
    }

    #[test]
    fn rejects_non_hex() {
        let bad = "00-zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz-b7ad6b7169203331-01";
        assert!(TraceContext::parse(bad).is_none());
    }

    #[test]
    fn rejects_unsupported_version() {
        let bad = "ff-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01";
        assert!(TraceContext::parse(bad).is_none());
    }

    #[test]
    fn rejects_all_zero_trace_id() {
        let bad = "00-00000000000000000000000000000000-b7ad6b7169203331-01";
        assert!(TraceContext::parse(bad).is_none());
    }

    #[test]
    fn rejects_all_zero_span_id() {
        let bad = "00-0af7651916cd43dd8448eb211c80319c-0000000000000000-01";
        assert!(TraceContext::parse(bad).is_none());
    }

    #[test]
    fn trims_surrounding_whitespace() {
        let header = "  00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01  ";
        assert!(TraceContext::parse(header).is_some());
    }

    #[test]
    fn new_root_is_well_formed_and_self_parsing() {
        let ctx = TraceContext::new_root(true);
        assert_eq!(ctx.flags, 0x01);
        assert!(ctx.trace_id.iter().any(|b| *b != 0));
        assert!(ctx.span_id.iter().any(|b| *b != 0));
        let header = ctx.to_header();
        let parsed = TraceContext::parse(&header).expect("self-round-trip");
        assert_eq!(parsed, ctx);
    }

    #[test]
    fn new_child_shares_trace_id_and_picks_new_span_id() {
        let parent = TraceContext::new_root(true);
        let child = parent.new_child();
        assert_eq!(child.trace_id, parent.trace_id);
        assert_eq!(child.flags, parent.flags);
        assert_ne!(child.span_id, parent.span_id);
    }

    #[test]
    fn flags_unsampled_zero() {
        let ctx = TraceContext::new_root(false);
        assert_eq!(ctx.flags, 0x00);
        assert!(ctx.to_header().ends_with("-00"));
    }

    #[test]
    fn empty_string_is_rejected() {
        assert!(TraceContext::parse("").is_none());
    }
}
