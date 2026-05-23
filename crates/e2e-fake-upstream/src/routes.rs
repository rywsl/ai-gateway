use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use hyper::header::HeaderMap;
use regex::Regex;

use crate::scenario::{Route, Scenario};

/// Per-route request hit counters.  Indexed by route position in the scenario.
/// Reset whenever the scenario is replaced via the admin endpoint.
/// Wrapped in `Arc<RwLock<_>>` at the server layer (`SharedCounters`).
pub type HitCounters = Vec<AtomicU32>;

/// Build a fresh counter vec sized to match the supplied scenario.
pub fn make_counters(scenario: &Scenario) -> HitCounters {
    (0..scenario.routes.len())
        .map(|_| AtomicU32::new(0))
        .collect()
}

/// Per-(route, weighted-variant) hit counters.
///
/// Parallel to [`HitCounters`] but sized only for routes whose response is
/// `ResponseSpec::Weighted`. Indexed by `(route_idx, variant_idx)`.
/// Reset whenever the scenario is replaced via /admin/scenario.
pub type WeightedHits = Vec<Vec<AtomicU64>>;

pub fn make_weighted_counters(scenario: &crate::scenario::Scenario) -> WeightedHits {
    scenario
        .routes
        .iter()
        .map(|r| match &r.response {
            crate::scenario::ResponseSpec::Weighted { variants } => {
                variants.iter().map(|_| AtomicU64::new(0)).collect()
            }
            _ => Vec::new(),
        })
        .collect()
}

/// First-match route lookup.
///
/// Returns `(route_index, &Route)` for the first route whose `MatchSpec`
/// matches *all* provided fields, or `None` when nothing matches.
///
/// Matching rules:
/// - `method` — case-insensitive equality
/// - `path_regex` — full match via `regex::Regex` (anchored by the pattern
///   itself; e.g. `^/v1/chat/completions$`)
/// - `header_eq` — `(name, value)`: case-insensitive header name lookup,
///   exact value comparison
pub fn match_route<'r>(
    scenario: &'r Scenario,
    method: &str,
    path: &str,
    headers: &HeaderMap,
    counters: &HitCounters,
) -> Option<(usize, &'r Route)> {
    for (idx, route) in scenario.routes.iter().enumerate() {
        let spec = &route.r#match;

        // Method check
        if let Some(ref m) = spec.method {
            if !m.eq_ignore_ascii_case(method) {
                continue;
            }
        }

        // Path regex check — full match (anchored).
        if let Some(ref pat) = spec.path_regex {
            // Wrap user pattern so that it must match the ENTIRE path.
            // Patterns already anchored (e.g. `^/v1/...$`) still work correctly.
            let anchored = format!("^(?:{pat})$");
            // Compiling per-call is acceptable for a test helper (not a hot path).
            let re = match Regex::new(&anchored) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("[fake-upstream] invalid path_regex {pat:?}: {e}");
                    continue;
                }
            };
            if !re.is_match(path) {
                continue;
            }
        }

        // Header equality check
        if let Some((ref name, ref expected)) = spec.header_eq {
            let name_lower = name.to_lowercase();
            let found = headers.iter().any(|(k, v)| {
                k.as_str().eq_ignore_ascii_case(&name_lower)
                    && v.to_str().unwrap_or("") == expected.as_str()
            });
            if !found {
                continue;
            }
        }

        // All conditions passed — record the hit and return.
        if let Some(counter) = counters.get(idx) {
            counter.fetch_add(1, Ordering::Relaxed);
        }
        return Some((idx, route));
    }
    None
}
