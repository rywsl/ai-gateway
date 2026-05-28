use gw_core::ids::*;

#[test]
fn user_id_roundtrip_json() {
    let id = UserId::from("u123");
    let s = serde_json::to_string(&id).unwrap();
    assert_eq!(s, "\"u123\"");
    let back: UserId = serde_json::from_str(&s).unwrap();
    assert_eq!(back, id);
}

#[test]
fn debug_format_includes_type_name() {
    let id = UserId::from("u1");
    let s = format!("{id:?}");
    assert!(s.contains("UserId"));
    assert!(s.contains("u1"));
}
