use std::collections::HashMap;

/// Top-level scenario loaded from JSON (or built-in default).
#[derive(serde::Deserialize, Clone)]
pub struct Scenario {
    pub routes: Vec<Route>,
}

/// A single route: match spec + optional auth check + response spec.
#[derive(serde::Deserialize, Clone)]
pub struct Route {
    pub r#match: MatchSpec,
    #[serde(default)]
    pub auth_check: Option<AuthCheck>,
    pub response: ResponseSpec,
}

/// Criteria for matching an incoming request.  All present fields must match.
#[derive(serde::Deserialize, Clone)]
pub struct MatchSpec {
    #[serde(default)]
    pub method: Option<String>,
    /// Evaluated as a full `regex::Regex` match against the request path.
    #[serde(default)]
    pub path_regex: Option<String>,
    /// `(header-name, expected-value)` — exact value, case-insensitive name.
    #[serde(default)]
    pub header_eq: Option<(String, String)>,
}

/// Optional bearer / API-key validation the fake upstream performs itself.
#[derive(serde::Deserialize, Clone)]
pub struct AuthCheck {
    pub header: String,
    pub must_start_with: String,
}

/// How to respond once a route is matched.
#[derive(serde::Deserialize, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResponseSpec {
    /// Plain HTTP response with an optional pre-response delay.
    Static {
        status: u16,
        #[serde(default)]
        headers: HashMap<String, String>,
        body: String,
        #[serde(default)]
        delay_ms: u64,
    },
    /// Server-Sent Events stream.  Each event is `data: <data>\n\n`.
    Sse {
        #[serde(default)]
        delay_ms: u64,
        events: Vec<SseEvent>,
    },

    // ── E1-B failure-injection variants ─────────────────────────────────────
    // Dispatched by `server::dispatch_spec` to the handlers in `crate::inject`.
    TcpResetAfterHeaders,
    TcpResetMidBody {
        after_bytes: usize,
    },
    ConnectionRefusedFirstN {
        n: u32,
    },
    SlowBodyBytePerSecond {
        body: String,
        bps: u32,
    },
    PartialSseEvent {
        half_body: String,
    },
    StatusWithRetryAfter {
        status: u16,
        retry_after_secs: u32,
        body: String,
    },
}

/// A single SSE event payload (the `data:` line value) with an optional delay
/// that is applied *before* this event is sent.
#[derive(serde::Deserialize, Clone)]
pub struct SseEvent {
    pub data: String,
    #[serde(default)]
    pub after_ms: u64,
}

// ── Built-in baseline scenario ────────────────────────────────────────────────

/// Returns the hard-coded fallback scenario used when no scenario file is found.
pub fn default_scenario() -> Scenario {
    Scenario {
        routes: vec![Route {
            r#match: MatchSpec {
                method: Some("POST".to_owned()),
                path_regex: Some("^/v1/chat/completions$".to_owned()),
                header_eq: None,
            },
            auth_check: None,
            response: ResponseSpec::Sse {
                delay_ms: 0,
                events: vec![
                    SseEvent {
                        data: r#"{"choices":[{"delta":{"content":"hi"}}]}"#.to_owned(),
                        after_ms: 0,
                    },
                    SseEvent {
                        data: "[DONE]".to_owned(),
                        after_ms: 0,
                    },
                ],
            },
        }],
    }
}
