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

    // ── E1-B variants ───────────────────────────────────────────────────────
    // These are declared here so the DSL is complete, but are NOT implemented
    // until E1-B.  Selecting one in a scenario returns HTTP 501.
    // Fields are unused until E1-B; suppress dead_code lint for now.
    #[allow(dead_code)]
    TcpResetAfterHeaders,
    TcpResetMidBody {
        #[allow(dead_code)]
        after_bytes: usize,
    },
    ConnectionRefusedFirstN {
        #[allow(dead_code)]
        n: u32,
    },
    SlowBodyBytePerSecond {
        #[allow(dead_code)]
        body: String,
        #[allow(dead_code)]
        bps: u32,
    },
    PartialSseEvent {
        #[allow(dead_code)]
        half_body: String,
    },
    StatusWithRetryAfter {
        #[allow(dead_code)]
        status: u16,
        #[allow(dead_code)]
        retry_after_secs: u32,
        #[allow(dead_code)]
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
