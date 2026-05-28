//! Gemini CLI profile — UA template & x-goog-* headers per gemini-cli TS source.
//! Note: real client uses Node + undici; we impersonate Chrome131 at TLS layer
//! for higher pass rate against Google backends.

use crate::impersonate::rng::seeded_rng;
use crate::impersonate::IdentityVariable;
use rand::seq::SliceRandom;
use uuid::Uuid;

const CLI_VERSION_POOL: &[&str] = &["0.1.18", "0.1.19", "0.1.20", "0.1.21"];
const OS_TYPE_POOL: &[&str] = &["linux", "darwin", "win32"];
const ARCH_POOL: &[&str] = &["x64", "arm64"];
const SURFACE: &str = "unknown";
const GEMINI_INSTALL_NAMESPACE: &str = "gemini:install:";

pub fn derive(key_id: &str) -> IdentityVariable {
    let mut rng = seeded_rng(key_id);
    let cli_version = pick(CLI_VERSION_POOL, &mut rng).to_string();
    let os_type = pick(OS_TYPE_POOL, &mut rng).to_string();
    let arch = pick(ARCH_POOL, &mut rng).to_string();
    let installation_id = Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("{GEMINI_INSTALL_NAMESPACE}{key_id}").as_bytes(),
    );
    IdentityVariable::GeminiCli {
        cli_version,
        os_type,
        arch,
        installation_id,
    }
}

pub fn headers(
    v: &IdentityVariable,
    model: &str,
    quota_project_id: Option<&str>,
) -> Vec<(String, String)> {
    let IdentityVariable::GeminiCli {
        cli_version,
        os_type,
        arch,
        installation_id,
    } = v
    else {
        panic!("gemini_cli::headers called with non-GeminiCli variant");
    };
    let ua = format!(
        "GeminiCLI-/{}/{} ({}; {}; {})",
        cli_version, model, os_type, arch, SURFACE
    );
    let mut h = vec![
        ("Content-Type".into(), "application/json".into()),
        ("User-Agent".into(), ua),
        (
            "x-gemini-api-privileged-user-id".into(),
            installation_id.to_string(),
        ),
    ];
    if let Some(p) = quota_project_id {
        h.push(("X-Goog-User-Project".into(), p.into()));
    }
    h
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
        assert_eq!(derive("vk-g"), derive("vk-g"));
    }

    #[test]
    fn user_agent_substitutes_model_per_request() {
        let v = derive("vk-1");
        let h = headers(&v, /*model*/ "gemini-1.5-pro", /*quota*/ None);
        let ua = h.iter().find(|(k, _)| k == "User-Agent").unwrap().1.clone();
        assert!(ua.contains("gemini-1.5-pro"));
    }

    #[test]
    fn quota_project_id_when_some() {
        let v = derive("vk-2");
        let h = headers(&v, "gemini-1.5-pro", Some("my-project"));
        assert!(h
            .iter()
            .any(|(k, val)| k == "X-Goog-User-Project" && val == "my-project"));
    }

    #[test]
    fn quota_project_id_absent_when_none() {
        let v = derive("vk-3");
        let h = headers(&v, "gemini-1.5-pro", None);
        assert!(!h.iter().any(|(k, _)| k == "X-Goog-User-Project"));
    }

    #[test]
    fn installation_id_is_uuid_v5_stable() {
        let IdentityVariable::GeminiCli {
            installation_id: a, ..
        } = derive("vk-x")
        else {
            panic!()
        };
        let IdentityVariable::GeminiCli {
            installation_id: b, ..
        } = derive("vk-x")
        else {
            panic!()
        };
        assert_eq!(a, b);
    }
}
