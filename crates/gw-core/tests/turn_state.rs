use gw_core::impersonate::turn_state::TurnStateCache;

#[tokio::test]
async fn same_conversation_returns_same_token() {
    let cache = TurnStateCache::new(std::time::Duration::from_secs(60), 1024);
    let a = cache.get_or_create("vk-1", Some("conv-A")).await;
    let b = cache.get_or_create("vk-1", Some("conv-A")).await;
    assert_eq!(a, b);
}

#[tokio::test]
async fn different_conversations_get_distinct_tokens() {
    let cache = TurnStateCache::new(std::time::Duration::from_secs(60), 1024);
    let a = cache.get_or_create("vk-1", Some("conv-A")).await;
    let b = cache.get_or_create("vk-1", Some("conv-B")).await;
    assert_ne!(a, b);
}

#[tokio::test]
async fn no_conversation_id_generates_fresh_each_call() {
    let cache = TurnStateCache::new(std::time::Duration::from_secs(60), 1024);
    let a = cache.get_or_create("vk-1", None).await;
    let b = cache.get_or_create("vk-1", None).await;
    assert_ne!(a, b);
}
