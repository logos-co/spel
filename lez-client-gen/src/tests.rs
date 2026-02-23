//! Tests for lez-client-gen.

use crate::generate_from_idl_json;

/// Sample IDL similar to what the lez-framework macro generates.
const SAMPLE_IDL: &str = r#"{
    "version": "0.1.0",
    "name": "my_multisig",
    "instructions": [
        {
            "name": "create",
            "accounts": [
                {
                    "name": "multisig_state",
                    "writable": true,
                    "signer": false,
                    "init": true,
                    "pda": {
                        "seeds": [
                            {"kind": "const", "value": "multisig_state__"},
                            {"kind": "arg", "path": "create_key"}
                        ]
                    }
                },
                {
                    "name": "creator",
                    "writable": false,
                    "signer": true,
                    "init": false
                }
            ],
            "args": [
                {"name": "create_key", "type": "[u8; 32]"},
                {"name": "threshold", "type": "u64"},
                {"name": "members", "type": {"vec": "[u8; 32]"}}
            ]
        },
        {
            "name": "approve",
            "accounts": [
                {
                    "name": "multisig_state",
                    "writable": false,
                    "signer": false,
                    "init": false,
                    "pda": {
                        "seeds": [
                            {"kind": "const", "value": "multisig_state__"}
                        ]
                    }
                },
                {
                    "name": "proposal",
                    "writable": true,
                    "signer": false,
                    "init": false
                },
                {
                    "name": "member",
                    "writable": false,
                    "signer": true,
                    "init": false
                }
            ],
            "args": [
                {"name": "proposal_id", "type": "u64"}
            ]
        }
    ],
    "accounts": [],
    "types": [],
    "errors": []
}"#;

#[test]
fn test_parse_and_generate() {
    let output = generate_from_idl_json(SAMPLE_IDL).expect("codegen should succeed");

    // Client code checks
    assert!(output.client_code.contains("pub enum MyMultisigInstruction"));
    assert!(output.client_code.contains("Create {"));
    assert!(output.client_code.contains("Approve {"));
    assert!(output.client_code.contains("pub struct CreateAccounts"));
    assert!(output.client_code.contains("pub struct ApproveAccounts"));
    assert!(output.client_code.contains("pub struct MyMultisigClient"));
    assert!(output.client_code.contains("async fn create("));
    assert!(output.client_code.contains("async fn approve("));

    // PDA computation
    assert!(output.client_code.contains("compute_multisig_state_pda"));

    // Correct endianness
    assert!(output.client_code.contains("from_le_bytes"));
}

#[test]
fn test_ffi_generation() {
    let output = generate_from_idl_json(SAMPLE_IDL).expect("codegen should succeed");

    // FFI function names
    assert!(output.ffi_code.contains("pub extern \"C\" fn my_multisig_create("));
    assert!(output.ffi_code.contains("pub extern \"C\" fn my_multisig_approve("));
    assert!(output.ffi_code.contains("pub extern \"C\" fn my_multisig_free_string("));
    assert!(output.ffi_code.contains("pub extern \"C\" fn my_multisig_version("));

    // Account parsing - base58 support
    assert!(output.ffi_code.contains("parse_account_id"));

    // ProgramId parsing - LE byte order
    assert!(output.ffi_code.contains("from_le_bytes"));

    // PDA computation
    assert!(output.ffi_code.contains("compute_pda"));
}

#[test]
fn test_header_generation() {
    let output = generate_from_idl_json(SAMPLE_IDL).expect("codegen should succeed");

    assert!(output.header.contains("MY_MULTISIG_FFI_H"));
    assert!(output.header.contains("char* my_multisig_create(const char* args_json)"));
    assert!(output.header.contains("char* my_multisig_approve(const char* args_json)"));
    assert!(output.header.contains("void my_multisig_free_string(char* s)"));
}

#[test]
fn test_account_order_preserved() {
    let output = generate_from_idl_json(SAMPLE_IDL).expect("codegen should succeed");

    // For approve: account_ids vec should list accounts in IDL order
    let ffi = &output.ffi_code;
    let approve_impl_start = ffi.find("fn my_multisig_approve_impl").unwrap();
    let approve_section = &ffi[approve_impl_start..];

    // Find the account_ids vec construction
    let vec_start = approve_section.find("let mut account_ids = vec![").unwrap();
    let vec_section = &approve_section[vec_start..];

    let ms_pos = vec_section.find("multisig_state").unwrap();
    let prop_pos = vec_section.find("proposal").unwrap();
    let member_pos = vec_section.find("member").unwrap();

    assert!(ms_pos < prop_pos, "multisig_state should come before proposal in account_ids");
    assert!(prop_pos < member_pos, "proposal should come before member in account_ids");
}

#[test]
fn test_invalid_json_error() {
    let result = generate_from_idl_json("not json");
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("failed to parse IDL JSON"));
}

#[test]
fn test_empty_instructions() {
    let idl = r#"{
        "version": "0.1.0",
        "name": "empty_program",
        "instructions": []
    }"#;
    let output = generate_from_idl_json(idl).expect("should handle empty instructions");
    assert!(output.client_code.contains("EmptyProgramInstruction"));
    assert!(output.ffi_code.contains("empty_program_free_string"));
}

#[test]
fn test_rest_accounts() {
    let idl = r#"{
        "version": "0.1.0",
        "name": "test_prog",
        "instructions": [{
            "name": "multi_sign",
            "accounts": [
                {"name": "state", "writable": true, "signer": false, "init": false},
                {"name": "signers", "writable": false, "signer": true, "init": false, "rest": true}
            ],
            "args": []
        }],
        "accounts": [],
        "types": [],
        "errors": []
    }"#;
    let output = generate_from_idl_json(idl).expect("should handle rest accounts");
    assert!(output.client_code.contains("pub signers: Vec<AccountId>"));
}
