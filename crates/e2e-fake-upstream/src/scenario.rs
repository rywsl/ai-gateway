use std::collections::HashMap;

/// Top-level scenario loaded from JSON (or built-in default).
#[derive(serde::Deserialize, Clone, Debug)]
pub struct Scenario {
    pub routes: Vec<Route>,
}

/// A single route: match spec + optional auth check + response spec.
#[derive(serde::Deserialize, Clone, Debug)]
pub struct Route {
    pub r#match: MatchSpec,
    #[serde(default)]
    pub auth_check: Option<AuthCheck>,
    pub response: ResponseSpec,
}

/// Criteria for matching an incoming request.  All present fields must match.
#[derive(serde::Deserialize, Clone, Debug)]
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
#[derive(serde::Deserialize, Clone, Debug)]
pub struct AuthCheck {
    pub header: String,
    pub must_start_with: String,
}

/// How to respond once a route is matched.
#[derive(serde::Deserialize, Clone, Debug)]
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

    /// Static binary response with `Content-Encoding: gzip` set automatically.
    ///
    /// `body_base64` is the base64 of the **already-gzipped** payload bytes;
    /// the fake decodes the base64 to raw bytes and ships them verbatim with
    /// `content-encoding: gzip` + `content-length: <decoded.len()>`. Callers
    /// (typically Python) gzip-encode a payload and then base64-encode the
    /// gzip output, so the wire-side decompressor sees a real gzip frame.
    ///
    /// This variant exists because the plain `Static.body: String` cannot
    /// carry non-UTF-8 bytes (gzip frames contain arbitrary binary), and
    /// serde-deserializing binary through JSON requires a string carrier.
    ///
    /// Content-type defaults to `text/event-stream` (SSE-test-driven; callers
    /// shipping a gzipped JSON or plain-text response should override the
    /// `content-type` header explicitly in `headers`).
    GzipStatic {
        #[serde(default = "default_gzip_status")]
        status: u16,
        #[serde(default)]
        headers: HashMap<String, String>,
        body_base64: String,
        #[serde(default)]
        delay_ms: u64,
    },

    /// Weighted-random dispatch (E2E chaos soak, §4.1 of design spec).
    ///
    /// `variants[0]` is required to be the happy-path: when `CHAOS_ENABLED`
    /// is off the dispatch falls through to `variants[0]` unconditionally.
    /// Empty `variants` and nested `Weighted` are rejected by
    /// [`validate_scenario`] before reaching dispatch.
    Weighted {
        variants: Vec<WeightedVariant>,
    },
}

fn default_gzip_status() -> u16 {
    200
}

/// A single SSE event payload (the `data:` line value) with an optional delay
/// that is applied *before* this event is sent.
#[derive(serde::Deserialize, Clone, Debug)]
pub struct SseEvent {
    pub data: String,
    #[serde(default)]
    pub after_ms: u64,
}

#[derive(serde::Deserialize, Clone, Debug)]
pub struct WeightedVariant {
    pub weight: u32,
    pub spec: Box<ResponseSpec>,
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

/// Validate a deserialised [`Scenario`] before publishing it as the active
/// scenario. Rejects shapes that would produce undefined behaviour at
/// dispatch time:
///
/// - Empty `Weighted.variants` (no candidate to pick).
/// - Nested `Weighted` (dispatch unwraps exactly one layer; §4.3 of design).
///
/// Returns `Err(msg)` with a human-readable reason; caller maps to HTTP 400.
pub fn validate_scenario(s: &Scenario) -> Result<(), String> {
    for (i, route) in s.routes.iter().enumerate() {
        validate_response(&route.response, i, 0)?;
    }
    Ok(())
}

fn validate_response(r: &ResponseSpec, route_idx: usize, depth: u32) -> Result<(), String> {
    if let ResponseSpec::Weighted { variants } = r {
        if depth >= 1 {
            return Err(format!(
                "route[{route_idx}]: nested Weighted is not allowed \
                 (dispatch unwraps exactly one layer)"
            ));
        }
        if variants.is_empty() {
            return Err(format!(
                "route[{route_idx}]: Weighted with empty variants \
                 (dispatch has no candidate)"
            ));
        }
        for (vi, v) in variants.iter().enumerate() {
            validate_response(&v.spec, route_idx, depth + 1)
                .map_err(|e| format!("route[{route_idx}].variants[{vi}]: {e}"))?;
        }
    }
    // Non-Weighted variants carry no nested ResponseSpec today; if you add
    // a variant that does, extend this walker.
    Ok(())
}
