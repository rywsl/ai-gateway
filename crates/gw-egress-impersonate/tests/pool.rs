use gw_core::impersonate::ImpersonateProfile;
use gw_egress_impersonate::pool::{ClientPool, PoolKey};

#[tokio::test]
async fn pool_returns_cached_client_for_same_key() {
    let pool = ClientPool::new(4);
    let a = pool
        .get_or_create(PoolKey {
            profile: ImpersonateProfile::ClaudeCodeChrome131,
            proxy: None,
        })
        .await
        .unwrap();
    let b = pool
        .get_or_create(PoolKey {
            profile: ImpersonateProfile::ClaudeCodeChrome131,
            proxy: None,
        })
        .await
        .unwrap();
    assert!(std::sync::Arc::ptr_eq(&a, &b));
}

#[tokio::test]
async fn pool_evicts_lru_at_capacity() {
    let pool = ClientPool::new(2);
    let _a = pool
        .get_or_create(PoolKey {
            profile: ImpersonateProfile::ClaudeCodeChrome131,
            proxy: None,
        })
        .await
        .unwrap();
    let _b = pool
        .get_or_create(PoolKey {
            profile: ImpersonateProfile::OpenAiCodex,
            proxy: None,
        })
        .await
        .unwrap();
    let _c = pool
        .get_or_create(PoolKey {
            profile: ImpersonateProfile::GeminiCli,
            proxy: None,
        })
        .await
        .unwrap();
    assert_eq!(pool.size(), 2);
}

#[tokio::test]
async fn proxy_url_normalization_doesnt_split_pool() {
    let pool = ClientPool::new(4);
    let a = pool
        .get_or_create(PoolKey {
            profile: ImpersonateProfile::ClaudeCodeChrome131,
            proxy: Some("http://Example.com:8080/".parse().unwrap()),
        })
        .await
        .unwrap();
    let b = pool
        .get_or_create(PoolKey {
            profile: ImpersonateProfile::ClaudeCodeChrome131,
            proxy: Some("http://example.com:8080".parse().unwrap()),
        })
        .await
        .unwrap();
    assert!(std::sync::Arc::ptr_eq(&a, &b));
}
