use gw_core::entity::OAuthCredentials;

#[test]
fn oauth_quota_project_id_defaults_to_none() {
    use secrecy::SecretString;
    use time::OffsetDateTime;
    let o = OAuthCredentials {
        access_token: SecretString::new("a".into()),
        refresh_token: SecretString::new("r".into()),
        access_expires_at: OffsetDateTime::now_utc(),
        token_endpoint: "https://example.com/token".into(),
        scope: vec![],
        quota_project_id: None,
    };
    assert!(o.quota_project_id.is_none());
}
