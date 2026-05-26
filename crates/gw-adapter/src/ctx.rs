//! `AdapterCtx` — the runtime-agnostic context handed to every adapter hook.
//!
//! Per A-T0 §ADR-AT0-2 the adapter sees only `dyn HeadersAccessor` and
//! `dyn UrlAccessor` views of the outbound request. The proxy / task
//! runtime implements these traits over their native types
//! (pingora::http::RequestHeader, etc.) at the call site.

use gw_core::entity::VendorKey;
use gw_core::ids::{KeyId, ModelName, UserId, VendorId};
use gw_core::protocol::{ApiFormat, ProtocolKind};
use gw_health::store::KeyHealthStore;
use std::sync::Arc;

/// Phase 1d carries health_store only; oauth_cache (K-C) is added in Phase 3.
pub struct AdapterCtx<'a> {
    pub vendor_id: VendorId,
    pub vendor_key_id: KeyId,
    pub vendor_key: Arc<VendorKey>,

    pub client_protocol: ProtocolKind,
    pub upstream_protocol: ProtocolKind,
    pub model: ModelName,

    pub user_id: UserId,
    pub api_format: ApiFormat,

    /// K-A handle — adapter reads health snapshots (e.g. circuit state)
    /// but never writes back (only K-B vendor analyzers and the logging
    /// filter do).
    pub health_store: &'a Arc<KeyHealthStore>,

    pub headers_mut: HeadersMut<'a>,
    pub url_mut: UrlMut<'a>,

    pub trace_id: &'a str,
    pub request_id: &'a str,
}

pub struct HeadersMut<'a> {
    pub inner: &'a mut dyn HeadersAccessor,
}

pub trait HeadersAccessor: Send {
    fn get(&self, name: &str) -> Option<&str>;
    fn set(&mut self, name: &str, value: &str);
    fn remove(&mut self, name: &str);
}

impl HeadersMut<'_> {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.inner.get(name)
    }
    pub fn set(&mut self, name: &str, value: &str) {
        self.inner.set(name, value);
    }
    pub fn remove(&mut self, name: &str) {
        self.inner.remove(name);
    }
}

pub struct UrlMut<'a> {
    pub inner: &'a mut dyn UrlAccessor,
}

pub trait UrlAccessor: Send {
    fn scheme(&self) -> &str;
    fn host(&self) -> &str;
    fn path(&self) -> &str;
    fn set_scheme(&mut self, scheme: &str);
    fn set_host(&mut self, host: &str);
    fn set_path(&mut self, path: &str);
}

impl UrlMut<'_> {
    pub fn scheme(&self) -> &str {
        self.inner.scheme()
    }
    pub fn host(&self) -> &str {
        self.inner.host()
    }
    pub fn path(&self) -> &str {
        self.inner.path()
    }
    pub fn set_scheme(&mut self, s: &str) {
        self.inner.set_scheme(s);
    }
    pub fn set_host(&mut self, h: &str) {
        self.inner.set_host(h);
    }
    pub fn set_path(&mut self, p: &str) {
        self.inner.set_path(p);
    }
}
