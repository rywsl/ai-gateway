//! Verify HeadersMut / UrlMut behave as thin pass-throughs over their
//! accessor traits. This exercises the dyn-dispatch path the proxy and
//! task runtime use.

use gw_adapter::{HeadersAccessor, HeadersMut, UrlAccessor, UrlMut};
use std::collections::HashMap;

struct InMemoryHeaders(HashMap<String, String>);

impl HeadersAccessor for InMemoryHeaders {
    fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(|s| s.as_str())
    }
    fn set(&mut self, name: &str, value: &str) {
        self.0.insert(name.to_owned(), value.to_owned());
    }
    fn remove(&mut self, name: &str) {
        self.0.remove(name);
    }
}

struct InMemoryUrl {
    scheme: String,
    host: String,
    path: String,
}

impl UrlAccessor for InMemoryUrl {
    fn scheme(&self) -> &str {
        &self.scheme
    }
    fn host(&self) -> &str {
        &self.host
    }
    fn path(&self) -> &str {
        &self.path
    }
    fn set_scheme(&mut self, s: &str) {
        self.scheme = s.to_owned();
    }
    fn set_host(&mut self, h: &str) {
        self.host = h.to_owned();
    }
    fn set_path(&mut self, p: &str) {
        self.path = p.to_owned();
    }
}

#[test]
fn headers_set_get_remove_roundtrip() {
    let mut backing = InMemoryHeaders(HashMap::new());
    let mut h = HeadersMut {
        inner: &mut backing,
    };
    h.set("x-api-key", "secret");
    assert_eq!(h.get("x-api-key"), Some("secret"));
    h.remove("x-api-key");
    assert_eq!(h.get("x-api-key"), None);
}

#[test]
fn url_mutation_visible_through_view() {
    let mut backing = InMemoryUrl {
        scheme: "http".into(),
        host: "example.com".into(),
        path: "/v1/chat".into(),
    };
    let mut u = UrlMut {
        inner: &mut backing,
    };
    u.set_scheme("https");
    u.set_host("api.openai.com");
    u.set_path("/v1/responses");
    assert_eq!(u.scheme(), "https");
    assert_eq!(u.host(), "api.openai.com");
    assert_eq!(u.path(), "/v1/responses");
}
