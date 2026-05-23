use gw_auth::mgmt::{is_management_path, parse_mgmt_authorization, MgmtAuth};

#[test]
fn admin_path_routed() {
    assert!(is_management_path("/admin/freeze"));
    assert!(is_management_path("/admin/"));
    assert!(!is_management_path("/admin")); // strict trailing slash
    assert!(!is_management_path("/v1/messages"));
    assert!(!is_management_path("/administer")); // no false positive
}

#[test]
fn parses_mgmt_header() {
    let p = parse_mgmt_authorization("Mgmt abc.def.ghi").unwrap();
    assert_eq!(
        p,
        MgmtAuth {
            token: "abc.def.ghi".into()
        }
    );
}

#[test]
fn rejects_business_authorization() {
    assert!(parse_mgmt_authorization("Bearer abc").is_none());
    assert!(parse_mgmt_authorization("").is_none());
    assert!(parse_mgmt_authorization("Mgmt ").is_none()); // empty token
}

#[test]
fn parse_trims_whitespace() {
    let p = parse_mgmt_authorization("  Mgmt  abc  ").unwrap();
    assert_eq!(p.token, "abc");
}
