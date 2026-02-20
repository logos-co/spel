//! Test variable-length account lists (rest accounts).
//! Verifies the IDL serialization round-trip with the `rest` field.

use nssa_framework_core::idl::{IdlAccountItem, IdlPda};

#[test]
fn test_rest_account_serializes() {
    let acc = IdlAccountItem {
        name: "members".to_string(),
        writable: false,
        signer: false,
        init: false,
        owner: None,
        pda: None,
        rest: true,
    };
    let json = serde_json::to_string(&acc).unwrap();
    assert!(json.contains("\"rest\":true"), "JSON: {}", json);
}

#[test]
fn test_non_rest_account_omits_rest() {
    let acc = IdlAccountItem {
        name: "state".to_string(),
        writable: true,
        signer: false,
        init: false,
        owner: None,
        pda: None,
        rest: false,
    };
    let json = serde_json::to_string(&acc).unwrap();
    assert!(!json.contains("rest"), "rest=false should be omitted, JSON: {}", json);
}

#[test]
fn test_rest_account_deserializes() {
    let json = r#"{"name":"members","writable":false,"signer":false,"init":false,"rest":true}"#;
    let acc: IdlAccountItem = serde_json::from_str(json).unwrap();
    assert!(acc.rest);
    assert_eq!(acc.name, "members");
}

#[test]
fn test_missing_rest_defaults_false() {
    let json = r#"{"name":"state","writable":true,"signer":false,"init":false}"#;
    let acc: IdlAccountItem = serde_json::from_str(json).unwrap();
    assert!(!acc.rest);
}
