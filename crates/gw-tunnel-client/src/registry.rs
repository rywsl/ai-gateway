use crate::resolver::ProxyNodeView;

pub trait ProxyNodeRegistry: Send + Sync + 'static {
    fn lookup_by_id(&self, node_id: &str) -> Option<ProxyNodeView>;

    fn verify_auth_token(&self, node_id: &str, candidate: &str) -> bool;
}
