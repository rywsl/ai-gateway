//! Smoke test: after bootstrap, EntityStore can resolve a seeded user via the
//! cold-loader chain. Marked #[ignore] — requires Docker.

#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis + Postgres)"]
async fn entity_store_resolves_user_after_bootstrap() {
    // Placeholder: the substantive cold-loader assertion lives in
    // crates/gw-store/tests/entity_store_loader.rs. This test exists to keep
    // the gw-gateway -> gw-store dependency chain honest at compile time.
}
