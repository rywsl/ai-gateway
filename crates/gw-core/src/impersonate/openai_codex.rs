//! OpenAI Codex profile — headers per codex-rs source observation.
//! Note: `OpenAI-Beta` value pinned to `responses=v1`; verify against current
//! codex-rs source before Phase 5 rollout.

use crate::impersonate::rng::seeded_rng;
use crate::impersonate::IdentityVariable;
use rand::seq::SliceRandom;
use uuid::Uuid;

const CODEX_VERSION_POOL: &[&str] = &["0.4.0", "0.4.1", "0.4.2", "0.5.0"];
const OS_POOL: &[&str] = &["linux", "macos", "windows"];
const ARCH_POOL: &[&str] = &["x86_64", "aarch64"];
const OPENAI_BETA: &str = "responses=v1";
const CODEX_INSTALL_NAMESPACE: &str = "codex:install:";

pub struct PerRequest {
    pub session_id: Uuid,
    pub thread_id: Uuid,
    pub window_id: Uuid,
    /// Hex-encoded sticky token resolved by the gateway via
    /// `turn_state::get_or_create` keyed by `(key_id, conversation_id)`.
    pub turn_state: String,
}

pub fn derive(key_id: &str) -> IdentityVariable {
    let mut rng = seeded_rng(key_id);
    let codex_version = pick(CODEX_VERSION_POOL, &mut rng).to_string();
    let os = pick(OS_POOL, &mut rng).to_string();
    let arch = pick(ARCH_POOL, &mut rng).to_string();
    let installation_id = Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("{CODEX_INSTALL_NAMESPACE}{key_id}").as_bytes(),
    );
    let user_agent = format!("codex_cli_rs/{codex_version} ({os}; {arch})");
    IdentityVariable::OpenAiCodex {
        codex_version,
        os,
        arch,
        installation_id,
        user_agent,
    }
}

pub fn headers(
    v: &IdentityVariable,
    is_stream: bool,
    per_req: &PerRequest,
) -> Vec<(String, String)> {
    let IdentityVariable::OpenAiCodex {
        installation_id,
        user_agent,
        ..
    } = v
    else {
        panic!("openai_codex::headers called with non-OpenAiCodex variant");
    };
    let accept = if is_stream {
        "text/event-stream"
    } else {
        "application/json"
    };
    vec![
        ("Accept".into(), accept.into()),
        ("OpenAI-Beta".into(), OPENAI_BETA.into()),
        ("User-Agent".into(), user_agent.clone()),
        (
            "x-codex-installation-id".into(),
            installation_id.to_string(),
        ),
        ("session-id".into(), per_req.session_id.to_string()),
        ("thread-id".into(), per_req.thread_id.to_string()),
        ("x-client-request-id".into(), per_req.thread_id.to_string()),
        ("x-codex-window-id".into(), per_req.window_id.to_string()),
        ("x-codex-turn-state".into(), per_req.turn_state.clone()),
    ]
}

fn pick<'a>(pool: &'a [&'a str], rng: &mut impl rand::Rng) -> &'a str {
    pool.choose(rng).copied().expect("non-empty pool")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impersonate::IdentityVariable;

    #[test]
    fn derive_is_deterministic() {
        assert_eq!(derive("vk-x"), derive("vk-x"));
    }

    #[test]
    fn installation_id_is_uuid_v5_stable_per_key() {
        let IdentityVariable::OpenAiCodex {
            installation_id: a, ..
        } = derive("vk-z")
        else {
            panic!()
        };
        let IdentityVariable::OpenAiCodex {
            installation_id: b, ..
        } = derive("vk-z")
        else {
            panic!()
        };
        assert_eq!(a, b);
    }

    #[test]
    fn headers_set_includes_static_and_per_key() {
        let v = derive("vk-y");
        let h = headers(&v, /*is_stream*/ true, &PerRequest::dummy());
        let keys: std::collections::HashSet<_> = h.iter().map(|(k, _)| k.as_str()).collect();
        for required in [
            "Accept",
            "OpenAI-Beta",
            "User-Agent",
            "x-codex-installation-id",
            "session-id",
            "thread-id",
            "x-client-request-id",
            "x-codex-window-id",
            "x-codex-turn-state",
        ] {
            assert!(keys.contains(required), "missing header: {required}");
        }
    }

    #[test]
    fn stream_accept_is_event_stream() {
        let v = derive("vk-stream");
        let h = headers(&v, true, &PerRequest::dummy());
        let accept = h.iter().find(|(k, _)| k == "Accept").unwrap().1.clone();
        assert_eq!(accept, "text/event-stream");
    }

    #[test]
    fn nonstream_accept_is_json() {
        let v = derive("vk-nonstream");
        let h = headers(&v, false, &PerRequest::dummy());
        let accept = h.iter().find(|(k, _)| k == "Accept").unwrap().1.clone();
        assert_eq!(accept, "application/json");
    }

    impl PerRequest {
        fn dummy() -> Self {
            Self {
                session_id: uuid::Uuid::nil(),
                thread_id: uuid::Uuid::nil(),
                window_id: uuid::Uuid::nil(),
                turn_state: "0".into(),
            }
        }
    }
}
