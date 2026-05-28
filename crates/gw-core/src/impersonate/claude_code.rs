//! Claude Code profile — directly mirrors Aether's `fingerprint.py` pools and
//! `claude_code/constants.py` static header set. Re-derives deterministically
//! per VendorKey via `crate::impersonate::rng::seeded_rng`.

use crate::impersonate::rng::seeded_rng;
use crate::impersonate::IdentityVariable;
use rand::seq::SliceRandom;

const OS_POOL: &[&str] = &["Linux", "MacOS", "Windows"];
const ARCH_POOL: &[&str] = &["x64", "arm64"];
const NODE_VERSION_POOL: &[&str] = &["v20.18.1", "v22.12.0", "v22.14.0", "v24.13.0"];
const PACKAGE_VERSION_POOL: &[&str] = &["0.68.0", "0.69.0", "0.70.0", "0.71.0"];
const TIMEOUT_POOL: &[&str] = &["600", "900"];
const ELECTRON_POOL: &[&str] = &["35.5.1", "36.7.1", "37.3.0", "38.7.0", "39.2.3"];
const CHROME_VERSION_POOL: &[&str] = &["131.0.6778.265", "131.0.6778.139", "131.0.6778.108"];

const ANTHROPIC_VERSION: &str = "2023-06-01";
const BETA_TOKENS: &[&str] = &[
    "claude-code-20250219",
    "oauth-2025-04-20",
    "interleaved-thinking-2025-05-14",
];

pub fn derive(key_id: &str) -> IdentityVariable {
    let mut rng = seeded_rng(key_id);
    let os = pick(OS_POOL, &mut rng).to_string();
    let arch = pick(ARCH_POOL, &mut rng).to_string();
    let node_version = pick(NODE_VERSION_POOL, &mut rng).to_string();
    let package_version = pick(PACKAGE_VERSION_POOL, &mut rng).to_string();
    let timeout = pick(TIMEOUT_POOL, &mut rng).to_string();
    let chrome_version = pick(CHROME_VERSION_POOL, &mut rng).to_string();
    let electron_version = pick(ELECTRON_POOL, &mut rng).to_string();
    let platform_token = platform_token(&os, &arch);
    let user_agent = format!(
        "Mozilla/5.0 ({platform_token}) AppleWebKit/537.36 (KHTML, like Gecko) \
Chrome/{chrome_version} Electron/{electron_version} Safari/537.36"
    );
    IdentityVariable::ClaudeCode {
        os,
        arch,
        node_version,
        package_version,
        timeout,
        chrome_version,
        electron_version,
        user_agent,
    }
}

pub fn headers(v: &IdentityVariable, is_stream: bool) -> Vec<(String, String)> {
    let IdentityVariable::ClaudeCode {
        os,
        arch,
        node_version,
        package_version,
        timeout,
        user_agent,
        ..
    } = v
    else {
        panic!("claude_code::headers called with non-ClaudeCode variant");
    };
    let mut h = vec![
        ("X-Stainless-Lang".into(), "js".into()),
        ("X-Stainless-Runtime".into(), "node".into()),
        ("X-Stainless-Retry-Count".into(), "0".into()),
        ("X-App".into(), "cli".into()),
        (
            "Anthropic-Dangerous-Direct-Browser-Access".into(),
            "true".into(),
        ),
        ("anthropic-version".into(), ANTHROPIC_VERSION.into()),
        ("Accept".into(), "application/json".into()),
        ("X-Stainless-OS".into(), os.clone()),
        ("X-Stainless-Arch".into(), arch.clone()),
        ("X-Stainless-Runtime-Version".into(), node_version.clone()),
        ("X-Stainless-Package-Version".into(), package_version.clone()),
        ("X-Stainless-Timeout".into(), timeout.clone()),
        ("User-Agent".into(), user_agent.clone()),
        ("anthropic-beta".into(), BETA_TOKENS.join(",")),
    ];
    if is_stream {
        h.push(("x-stainless-helper-method".into(), "stream".into()));
    }
    h
}

fn pick<'a>(pool: &'a [&'a str], rng: &mut impl rand::Rng) -> &'a str {
    pool.choose(rng).copied().expect("non-empty pool")
}

fn platform_token(os: &str, arch: &str) -> String {
    match (os, arch) {
        ("Linux", "arm64") => "X11; Linux arm64".into(),
        ("Linux", _) => "X11; Linux x86_64".into(),
        ("Windows", _) => "Windows NT 10.0; Win64; x64".into(),
        ("MacOS", "arm64") => "Macintosh; ARM Mac OS X 14_0_0".into(),
        ("MacOS", _) => "Macintosh; Intel Mac OS X 10_15_7".into(),
        _ => "X11; Linux x86_64".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impersonate::IdentityVariable;

    #[test]
    fn derive_is_deterministic() {
        let a = derive("vk-stable-123");
        let b = derive("vk-stable-123");
        assert_eq!(a, b);
    }

    #[test]
    fn different_keys_diverge() {
        let a = derive("vk-A");
        let b = derive("vk-B");
        assert_ne!(a, b);
    }

    #[test]
    fn pool_values_only() {
        let v = derive("vk-test");
        let IdentityVariable::ClaudeCode {
            os,
            arch,
            package_version,
            ..
        } = v
        else {
            panic!("wrong variant");
        };
        assert!(["Linux", "MacOS", "Windows"].contains(&os.as_str()));
        assert!(["x64", "arm64"].contains(&arch.as_str()));
        assert!(["0.68.0", "0.69.0", "0.70.0", "0.71.0"].contains(&package_version.as_str()));
    }

    #[test]
    fn headers_static_keys_present() {
        let v = derive("vk-h");
        let h = headers(&v, /*is_stream*/ false);
        let keys: std::collections::HashSet<_> = h.iter().map(|(k, _)| k.as_str()).collect();
        for required in [
            "X-Stainless-Lang",
            "X-Stainless-Runtime",
            "X-Stainless-Retry-Count",
            "X-App",
            "Anthropic-Dangerous-Direct-Browser-Access",
            "anthropic-version",
            "Accept",
            "X-Stainless-OS",
            "X-Stainless-Arch",
            "X-Stainless-Runtime-Version",
            "X-Stainless-Package-Version",
            "X-Stainless-Timeout",
            "User-Agent",
            "anthropic-beta",
        ] {
            assert!(keys.contains(required), "missing header: {required}");
        }
        assert!(!keys.contains("x-stainless-helper-method"));
    }

    #[test]
    fn stream_adds_helper_method_header() {
        let v = derive("vk-stream");
        let h = headers(&v, /*is_stream*/ true);
        assert!(h
            .iter()
            .any(|(k, val)| k == "x-stainless-helper-method" && val == "stream"));
    }
}
