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

    // PDA computation lives in the client
    assert!(output.client_code.contains("compute_multisig_state_pda"));

    // Correct endianness — in client's parse_program_id_hex
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

    // AccountId parsing helper emitted in FFI
    assert!(output.ffi_code.contains("parse_account_id"));

    // FFI is self-contained (inline transaction building, no super::client import)
    assert!(!output.ffi_code.contains("use super::client::*"));

    // FFI emits full WalletCore transaction building
    assert!(output.ffi_code.contains("use wallet::WalletCore"));
    assert!(output.ffi_code.contains("tokio::runtime::Runtime::new"));
    assert!(output.ffi_code.contains("rt.block_on"));
    assert!(output.ffi_code.contains("send_tx_public"));

    // FFI returns tx_hash JSON
    assert!(output.ffi_code.contains("tx_hash"));
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
fn test_account_order_in_client() {
    let output = generate_from_idl_json(SAMPLE_IDL).expect("codegen should succeed");

    // Account ordering is now enforced in the client (accounts struct + account_ids vec).
    // For approve: the IDL order is multisig_state, proposal, member.
    let client = &output.client_code;
    let approve_struct_start = client.find("pub struct ApproveAccounts").unwrap();
    let approve_section = &client[approve_struct_start..];

    let ms_pos = approve_section.find("multisig_state").unwrap();
    let prop_pos = approve_section.find("proposal").unwrap();
    let member_pos = approve_section.find("member").unwrap();

    assert!(ms_pos < prop_pos, "multisig_state should come before proposal in ApproveAccounts");
    assert!(prop_pos < member_pos, "proposal should come before member in ApproveAccounts");
}

#[test]
fn test_ffi_calls_client_methods() {
    let output = generate_from_idl_json(SAMPLE_IDL).expect("codegen should succeed");

    // The FFI impl builds instruction enum and submits transaction inline
    let ffi = &output.ffi_code;
    assert!(ffi.contains("Message::try_new"), "FFI should build Message");
    assert!(ffi.contains("send_tx_public"), "FFI should submit transaction");
    assert!(ffi.contains("MyMultisigInstruction"), "FFI should reference instruction enum");
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
    // FFI should handle rest accounts as optional array, defaulting to empty
    assert!(output.ffi_code.contains("signers"));
}

#[test]
fn test_pda_helpers_single_arg_seed() {
    use lez_framework_core::idl::*;
    use crate::ffi_codegen::generate_pda_helpers;

    let idl = LezIdl {
        version: "0.1.0".to_string(),
        name: "test_program".to_string(),
        instructions: vec![IdlInstruction {
            name: "create".to_string(),
            accounts: vec![IdlAccountItem {
                name: "multisig_state".to_string(),
                writable: true,
                signer: false,
                init: true,
                owner: None,
                pda: Some(IdlPda {
                    seeds: vec![IdlSeed::Arg { path: "create_key".to_string() }],
                }),
                rest: false,
                visibility: vec![],
            }],
            args: vec![IdlArg {
                name: "create_key".to_string(),
                type_: IdlType::Primitive("[u8; 32]".to_string()),

            }],
            discriminator: None,
            execution: None,
            variant: None,
        }],
        accounts: vec![],
        types: vec![],
        errors: vec![],
        spec: None,
        metadata: None,
        instruction_type: None,
    };

    let output = generate_pda_helpers(&idl);

    // Function signature
    assert!(output.contains("pub fn compute_multisig_state_pda("), "missing fn signature: {}", output);
    assert!(output.contains("program_id: &ProgramId"), "missing program_id param: {}", output);
    assert!(output.contains("create_key: &[u8; 32]"), "missing create_key param: {}", output);
    assert!(output.contains("-> AccountId"), "missing return type: {}", output);

    // Single-seed: use directly (no SHA256)
    assert!(output.contains("PdaSeed::new(seed_bytes)"), "missing PdaSeed::new: {}", output);
    assert!(output.contains("AccountId::from((program_id, &pda_seed))"), "missing AccountId::from: {}", output);

    // Single seed means no SHA256 hasher
    assert!(!output.contains("Sha256"), "single-seed should not use SHA256: {}", output);
}

#[test]
fn test_pda_helpers_multi_seed() {
    use lez_framework_core::idl::*;
    use crate::ffi_codegen::generate_pda_helpers;

    let idl = LezIdl {
        version: "0.1.0".to_string(),
        name: "test_program".to_string(),
        instructions: vec![IdlInstruction {
            name: "create".to_string(),
            accounts: vec![IdlAccountItem {
                name: "multisig_state".to_string(),
                writable: true,
                signer: false,
                init: true,
                owner: None,
                pda: Some(IdlPda {
                    seeds: vec![
                        IdlSeed::Const { value: "multisig_state__".to_string() },
                        IdlSeed::Arg { path: "create_key".to_string() },
                    ],
                }),
                rest: false,
                visibility: vec![],
            }],
            args: vec![IdlArg {
                name: "create_key".to_string(),
                type_: IdlType::Primitive("[u8; 32]".to_string()),

            }],
            discriminator: None,
            execution: None,
            variant: None,
        }],
        accounts: vec![],
        types: vec![],
        errors: vec![],
        spec: None,
        metadata: None,
        instruction_type: None,
    };

    let output = generate_pda_helpers(&idl);

    // Function signature
    assert!(output.contains("pub fn compute_multisig_state_pda("), "missing fn signature: {}", output);
    assert!(output.contains("create_key: &[u8; 32]"), "missing create_key param: {}", output);

    // Multi-seed: must use SHA256
    assert!(output.contains("Sha256"), "multi-seed must use SHA256: {}", output);
    assert!(output.contains("hasher.update"), "must call hasher.update: {}", output);
    assert!(output.contains("multisig_state__"), "must inline const seed: {}", output);

    // Doc comment seeds annotation
    assert!(output.contains("Seeds: ["), "missing Seeds doc comment: {}", output);
    assert!(output.contains("arg(create_key)"), "missing arg seed in doc: {}", output);
}

#[test]
fn test_pda_helpers_deduplication() {
    use lez_framework_core::idl::*;
    use crate::ffi_codegen::generate_pda_helpers;

    // Same account name appears in two instructions — should only generate one helper
    let make_ix = |name: &str| IdlInstruction {
        name: name.to_string(),
        accounts: vec![IdlAccountItem {
            name: "shared_state".to_string(),
            writable: true,
            signer: false,
            init: false,
            owner: None,
            pda: Some(IdlPda {
                seeds: vec![IdlSeed::Arg { path: "my_key".to_string() }],
            }),
            rest: false,
            visibility: vec![],
        }],
        args: vec![IdlArg {
            name: "my_key".to_string(),
            type_: IdlType::Primitive("[u8; 32]".to_string()),
        }],
        discriminator: None,
        execution: None,
        variant: None,
    };

    let idl = LezIdl {
        version: "0.1.0".to_string(),
        name: "test_program".to_string(),
        instructions: vec![make_ix("create"), make_ix("update")],
        accounts: vec![],
        types: vec![],
        errors: vec![],
        spec: None,
        metadata: None,
        instruction_type: None,
    };

    let output = generate_pda_helpers(&idl);

    // Should appear exactly once
    let count = output.matches("pub fn compute_shared_state_pda(").count();
    assert_eq!(count, 1, "account PDA helper should be generated exactly once, got {}", count);
}

#[test]
fn test_pda_helpers_in_ffi_output() {
    // Verify generate_ffi includes PDA helpers in its output
    let output = generate_from_idl_json(SAMPLE_IDL).expect("codegen should succeed");

    // The SAMPLE_IDL has multisig_state with a 2-seed PDA (const + arg)
    assert!(
        output.ffi_code.contains("pub fn compute_multisig_state_pda("),
        "FFI output must include PDA helper function"
    );
    assert!(
        output.ffi_code.contains("create_key: &[u8; 32]"),
        "FFI PDA helper must have create_key param"
    );
    assert!(
        output.ffi_code.contains("Sha256"),
        "FFI PDA helper for multi-seed must use SHA256"
    );
}
