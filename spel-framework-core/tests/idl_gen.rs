//! IDL generation from core crates: golden IDLs for copies of lez-programs' `token_core` and
//! `nft_core`, small crates for each type shape and failure, and decoding through the result.
//!
//! Regenerate the goldens after an intended change with `UPDATE_GOLDEN=1 cargo test`.

use std::path::{Path, PathBuf};

use serde_json::json;
use spel_framework_core::decode::{decode_named, decode_state};
use spel_framework_core::idl::{event_selector, IdlType, SpelIdl};
use spel_framework_core::idl_gen::{generate_idl, GenOptions, IdlGenError};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn options(crate_dir: &Path, message: &str, state: Option<&str>, events: Option<&str>) -> GenOptions {
    GenOptions {
        crate_dir: crate_dir.to_owned(),
        message: message.to_owned(),
        state: state.map(str::to_owned),
        events: events.map(str::to_owned),
        namespace: None,
    }
}

fn generate(options: &GenOptions) -> Result<SpelIdl, IdlGenError> {
    generate_idl(options, |w| panic!("unexpected warning: {w}"))
}

fn check_golden(name: &str, state: &str) -> SpelIdl {
    let idl = generate(&options(
        &fixtures().join(format!("{name}_core")),
        "Message",
        Some(state),
        Some("event"),
    ))
    .expect("generates");
    let golden = fixtures().join("golden").join(format!("{name}.json"));
    let actual = idl.to_json_pretty().unwrap() + "\n";
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&golden, &actual).unwrap();
    }
    let expected = std::fs::read_to_string(&golden).expect("golden exists");
    assert_eq!(
        actual.trim_end(),
        expected.trim_end(),
        "{name} IDL changed; rerun with UPDATE_GOLDEN=1 if intended"
    );
    idl
}

#[test]
fn token_core_matches_its_golden_idl() {
    let idl = check_golden("token", "TokenState");
    assert_eq!(idl.name, "token");
    let tags: Vec<(&str, u8)> = idl.messages.iter().map(|m| (m.name.as_str(), m.tag)).collect();
    assert_eq!(
        tags,
        [
            ("NewDefinition", 0),
            ("InitializeHolding", 1),
            ("Transfer", 2),
            ("Burn", 3),
            ("Mint", 4),
            ("SetAuthority", 5),
            ("ReadState", 6)
        ]
    );
    assert!(idl.type_def("Internal").is_none(), "the internal enum stays out");
    let transferred = idl.events.iter().find(|e| e.name == "Transferred").unwrap();
    assert_eq!(transferred.selector, event_selector("token", "Transferred"));
}

#[test]
fn nft_core_matches_its_golden_idl() {
    let idl = check_golden("nft", "NftState");
    assert_eq!(idl.name, "nft");
    let new_definition = idl.message("new-definition").unwrap();
    assert_eq!(
        new_definition.fields[2].type_,
        IdlType::Defined { defined: "NftMetadata".into() }
    );
    assert_eq!(idl.type_def("MetadataStandard").unwrap().kind, "enum");
}

#[test]
fn token_states_decode_through_the_idl() {
    let idl = check_golden("token", "TokenState");
    let mut holding = vec![1u8];
    holding.extend([7u8; 32]);
    holding.extend(690u128.to_le_bytes());
    assert_eq!(
        decode_state(&holding, &idl).unwrap(),
        json!({ "Holding": { "definition_id": bs58_of([7u8; 32]), "balance": "690" } })
    );

    let mut definition = vec![0u8];
    definition.extend(5u32.to_le_bytes());
    definition.extend(b"Probe");
    definition.extend(1000u128.to_le_bytes());
    definition.push(0);
    assert_eq!(
        decode_state(&definition, &idl).unwrap(),
        json!({ "Definition": { "name": "Probe", "total_supply": "1000", "authority": null } })
    );

    let err = decode_state(&[holding.as_slice(), &[0]].concat(), &idl).unwrap_err();
    assert!(err.contains("1 bytes remaining"), "{err}");
}

#[test]
fn nft_states_decode_through_the_idl() {
    let idl = check_golden("nft", "NftState");
    let mut copy = vec![2u8];
    copy.extend([9u8; 32]);
    copy.push(1);
    assert_eq!(
        decode_state(&copy, &idl).unwrap(),
        json!({ "Copy": { "definition_id": bs58_of([9u8; 32]), "owned": true } })
    );
}

fn bs58_of(bytes: [u8; 32]) -> String {
    lee_core::account::AccountId::new(bytes).to_string()
}

// ── shapes ────────────────────────────────────────────────────────────────────

/// A throwaway crate `shape_core` with the given `lib.rs` (and extra source files).
fn shape_crate(lib: &str, extra: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"shape_core\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), lib).unwrap();
    for (path, content) in extra {
        let path = dir.path().join("src").join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    dir
}

const SHAPES: &str = r#"
use lee_core::account::AccountId;

pub type Amount = u128;

/// The messages.
pub enum Message {
    /// No fields.
    Ping,
    Pay { to: Option<AccountId>, amount: Amount, memo: Vec<u8> },
    Pair(u64, Inner),
    Nest { outer: Box<Outer>, tags: [u8; 4] },
}

pub struct Inner(pub u32);

pub enum Outer {
    A(Inner),
    B { flag: bool },
}

#[cfg(test)]
pub struct OnlyInTests;
"#;

#[test]
fn every_shape_is_described() {
    let dir = shape_crate(SHAPES, &[]);
    let idl = generate(&options(dir.path(), "Message", Some("Outer"), None)).unwrap();
    assert_eq!(idl.name, "shape");
    assert_eq!(idl.message("ping").unwrap().docs.as_deref(), Some("No fields."));
    assert!(idl.message("ping").unwrap().fields.is_empty());

    let pay = serde_json::to_value(&idl.message("pay").unwrap().fields).unwrap();
    assert_eq!(
        pay,
        json!([
            { "name": "to", "type": { "option": "account_id" } },
            { "name": "amount", "type": "u128" },
            { "name": "memo", "type": { "vec": "u8" } }
        ]),
        "aliases resolve, AccountId is a primitive"
    );

    let pair = &idl.message("pair").unwrap().fields;
    assert_eq!((pair[0].name.as_str(), pair[1].name.as_str()), ("0", "1"));

    let nest = serde_json::to_value(&idl.message("nest").unwrap().fields).unwrap();
    assert_eq!(
        nest,
        json!([
            { "name": "outer", "type": { "defined": "Outer" } },
            { "name": "tags", "type": { "array": ["u8", 4] } }
        ]),
        "Box is transparent"
    );

    let names: Vec<&str> = idl.types.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["Inner", "Outer"], "referenced types only, sorted; no aliases");
}

#[test]
fn shapes_decode_through_the_idl() {
    let dir = shape_crate(SHAPES, &[]);
    let idl = generate(&options(dir.path(), "Message", Some("Outer"), None)).unwrap();
    assert_eq!(decode_state(&[0, 42, 0, 0, 0], &idl).unwrap(), json!({ "A": 42 }));
    assert_eq!(decode_state(&[1, 1], &idl).unwrap(), json!({ "B": { "flag": true } }));
    assert_eq!(decode_named(&[1, 0, 0, 0], "Inner", &idl).unwrap(), json!(1));
    assert!(decode_state(&[2], &idl).unwrap_err().contains("out of range"));
}

#[test]
fn events_come_from_trait_impls_in_the_named_module() {
    let dir = shape_crate(
        "pub mod outer;\npub enum Message { Ping }\n",
        &[(
            "outer.rs",
            "pub mod ev {\n\
                 pub trait E { const NAME: &'static str; }\n\
                 pub struct Paid { pub amount: u128 }\n\
                 impl E for Paid { const NAME: &'static str = \"Paid\"; }\n\
             }\n",
        )],
    );
    let mut opts = options(dir.path(), "Message", None, Some("outer::ev"));
    opts.namespace = Some("custom".into());
    let idl = generate(&opts).unwrap();
    assert_eq!(idl.name, "custom");
    assert_eq!(idl.events.len(), 1);
    assert_eq!(idl.events[0].selector, event_selector("custom", "Paid"));
    assert!(idl.type_def("Paid").is_some());
}

// ── failures ──────────────────────────────────────────────────────────────────

#[test]
fn a_missing_message_enum_fails() {
    let dir = shape_crate(SHAPES, &[]);
    let err = generate(&options(dir.path(), "Msg", None, None)).unwrap_err();
    assert!(matches!(err, IdlGenError::NotFound { ref name, .. } if name == "Msg"), "{err}");
}

#[test]
fn a_message_that_is_not_an_enum_fails() {
    let dir = shape_crate(SHAPES, &[]);
    let err = generate(&options(dir.path(), "Inner", None, None)).unwrap_err();
    assert!(matches!(err, IdlGenError::MessageNotEnum { .. }), "{err}");
}

#[test]
fn a_missing_state_type_fails() {
    let dir = shape_crate(SHAPES, &[]);
    let err = generate(&options(dir.path(), "Message", Some("State"), None)).unwrap_err();
    assert!(matches!(err, IdlGenError::NotFound { ref name, .. } if name == "State"), "{err}");
}

#[test]
fn a_test_only_type_is_not_found() {
    let dir = shape_crate(SHAPES, &[]);
    let err = generate(&options(dir.path(), "Message", Some("OnlyInTests"), None)).unwrap_err();
    assert!(matches!(err, IdlGenError::NotFound { .. }), "{err}");
}

#[test]
fn an_undefined_type_fails_naming_its_user() {
    let dir = shape_crate("pub enum Message { Go { x: Missing } }\n", &[]);
    let err = generate(&options(dir.path(), "Message", None, None)).unwrap_err();
    assert!(
        matches!(err, IdlGenError::UnknownType { ref name, ref used_by }
            if name == "Missing" && used_by == "Message::Go"),
        "{err}"
    );
}

#[test]
fn an_events_module_without_events_fails() {
    let dir = shape_crate(SHAPES, &[]);
    let err = generate(&options(dir.path(), "Message", None, Some("event"))).unwrap_err();
    assert!(matches!(err, IdlGenError::NoEvents { .. }), "{err}");
}

#[test]
fn an_unsupported_type_fails() {
    let dir = shape_crate("pub enum Message { Go { x: (u8, u8) } }\n", &[]);
    let err = generate(&options(dir.path(), "Message", None, None)).unwrap_err();
    assert!(matches!(err, IdlGenError::Unsupported { .. }), "{err}");
}
