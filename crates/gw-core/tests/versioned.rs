use gw_core::versioned::{upsert_if_newer, Versioned};

#[test]
fn newer_version_overwrites() {
    let mut cur = Some(Versioned::new(1, "a".to_string()));
    let upd = Versioned::new(2, "b".to_string());
    assert!(upsert_if_newer(&mut cur, upd));
    assert_eq!(cur.as_ref().unwrap().version, 2);
    assert_eq!(&**cur.as_ref().unwrap().value, "b");
}

#[test]
fn equal_version_rejected() {
    let mut cur = Some(Versioned::new(2, "a".to_string()));
    let upd = Versioned::new(2, "b".to_string());
    assert!(!upsert_if_newer(&mut cur, upd));
    assert_eq!(&**cur.as_ref().unwrap().value, "a");
}

#[test]
fn older_version_rejected() {
    let mut cur = Some(Versioned::new(3, "a".to_string()));
    let upd = Versioned::new(2, "b".to_string());
    assert!(!upsert_if_newer(&mut cur, upd));
    assert_eq!(&**cur.as_ref().unwrap().value, "a");
}

#[test]
fn empty_cur_accepts() {
    let mut cur: Option<Versioned<String>> = None;
    assert!(upsert_if_newer(&mut cur, Versioned::new(1, "x".into())));
}
