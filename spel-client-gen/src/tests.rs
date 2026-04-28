//! Tests for spel-client-gen.

use crate::generate_from_idl_json;

/// Sample IDL similar to what the spel-framework macro generates.
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

    // PDA computation — standalone function
    assert!(output.client_code.contains("pub fn compute_multisig_state_pda("));


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
    assert!(output.ffi_code.contains("send_transaction"));

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
    assert!(ffi.contains("send_transaction"), "FFI should submit transaction");
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
    use spel_framework_core::idl::*;
    use crate::ffi_codegen::generate_pda_helpers;

    let idl = SpelIdl {
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
    use spel_framework_core::idl::*;
    use crate::ffi_codegen::generate_pda_helpers;

    let idl = SpelIdl {
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
    use spel_framework_core::idl::*;
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

    let idl = SpelIdl {
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

#[test]
fn test_pda_helpers_u64_single_seed() {
    use spel_framework_core::idl::*;
    use crate::ffi_codegen::generate_pda_helpers;

    // A PDA with a single u64 arg seed (e.g. proposal_index)
    let idl = SpelIdl {
        version: "0.1.0".to_string(),
        name: "test_program".to_string(),
        instructions: vec![IdlInstruction {
            name: "create_proposal".to_string(),
            accounts: vec![IdlAccountItem {
                name: "proposal".to_string(),
                writable: true,
                signer: false,
                init: true,
                owner: None,
                pda: Some(IdlPda {
                    seeds: vec![IdlSeed::Arg { path: "proposal_index".to_string() }],
                }),
                rest: false,
                visibility: vec![],
            }],
            args: vec![IdlArg {
                name: "proposal_index".to_string(),
                type_: IdlType::Primitive("u64".to_string()),
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

    // Function signature: u64 passed by value (no &)
    assert!(output.contains("pub fn compute_proposal_pda("), "missing fn signature: {}", output);
    assert!(output.contains("proposal_index: u64"), "u64 param should be by value: {}", output);
    assert!(!output.contains("proposal_index: &u64"), "u64 param must not be by reference: {}", output);
    assert!(output.contains("-> AccountId"), "missing return type: {}", output);

    // Single u64 seed: uses to_le_bytes() padded into [u8; 32]
    assert!(output.contains("to_le_bytes()"), "u64 seed must use to_le_bytes: {}", output);
    assert!(output.contains("seed_bytes[..8].copy_from_slice"), "must copy 8 bytes of u64: {}", output);
    assert!(output.contains("PdaSeed::new(seed_bytes)"), "must create PdaSeed: {}", output);
}

#[test]
fn test_pda_helpers_u64_multi_seed() {
    use spel_framework_core::idl::*;
    use crate::ffi_codegen::generate_pda_helpers;

    // A PDA with const + u64 arg seeds (e.g. proposal with index)
    let idl = SpelIdl {
        version: "0.1.0".to_string(),
        name: "test_program".to_string(),
        instructions: vec![IdlInstruction {
            name: "create_proposal".to_string(),
            accounts: vec![IdlAccountItem {
                name: "proposal".to_string(),
                writable: true,
                signer: false,
                init: true,
                owner: None,
                pda: Some(IdlPda {
                    seeds: vec![
                        IdlSeed::Arg { path: "create_key".to_string() },
                        IdlSeed::Arg { path: "proposal_index".to_string() },
                    ],
                }),
                rest: false,
                visibility: vec![],
            }],
            args: vec![
                IdlArg {
                    name: "create_key".to_string(),
                    type_: IdlType::Primitive("[u8; 32]".to_string()),
                },
                IdlArg {
                    name: "proposal_index".to_string(),
                    type_: IdlType::Primitive("u64".to_string()),
                },
            ],
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

    // Function signature: [u8;32] by ref, u64 by value
    assert!(output.contains("pub fn compute_proposal_pda("), "missing fn signature: {}", output);
    assert!(output.contains("create_key: &[u8; 32]"), "create_key should be by reference: {}", output);
    assert!(output.contains("proposal_index: u64"), "u64 param should be by value: {}", output);
    assert!(!output.contains("proposal_index: &u64"), "u64 param must not be by reference: {}", output);

    // Multi-seed: uses SHA256
    assert!(output.contains("Sha256"), "multi-seed must use SHA256: {}", output);
    assert!(output.contains("hasher.update"), "must call hasher.update: {}", output);

    // u64 seed uses to_le_bytes, not as &[u8]
    assert!(output.contains("proposal_index.to_le_bytes()"), "u64 seed must use to_le_bytes: {}", output);
    assert!(!output.contains("proposal_index as &[u8]"), "u64 must not use as &[u8]: {}", output);

    // [u8;32] seed uses as &[u8]
    assert!(output.contains("create_key as &[u8]"), "byte array seed must use as &[u8]: {}", output);
}

#[test]
fn test_standalone_pda_helpers() {
    let output = generate_from_idl_json(SAMPLE_IDL).expect("codegen should succeed");
    let code = &output.client_code;

    // PDA helper is a standalone pub function (not a method)
    assert!(
        code.contains("pub fn compute_multisig_state_pda(program_id: &ProgramId"),
        "should generate standalone PDA helper with program_id parameter"
    );

    // Should use spel_framework_core::pda::compute_pda
    assert!(
        code.contains("spel_framework_core::pda::compute_pda(program_id"),
        "PDA helper should use framework core compute_pda"
    );

    // Should use create_key seed (from first occurrence); [u8; 32] maps to AccountId
    assert!(
        code.contains("create_key: &AccountId"),
        "PDA helper should take create_key arg seed"
    );

    // Deduplication: only one compute_multisig_state_pda (not two, despite appearing in both instructions)
    let count = code.matches("pub fn compute_multisig_state_pda(").count();
    assert_eq!(count, 1, "should deduplicate PDA helpers by account name");
}

#[test]
fn test_fetch_helpers() {
    let output = generate_from_idl_json(SAMPLE_IDL).expect("codegen should succeed");
    let code = &output.client_code;

    // Fetch helper is a method on the client
    assert!(
        code.contains("pub async fn fetch_multisig_state<T: BorshDeserialize>("),
        "should generate fetch helper"
    );

    // Fetch helper calls PDA computation
    assert!(
        code.contains("compute_multisig_state_pda(&self.program_id"),
        "fetch helper should call PDA helper with self.program_id"
    );

    // Fetch helper deserializes with Borsh
    assert!(
        code.contains("T::try_from_slice("),
        "fetch helper should use BorshDeserialize"
    );

    // Fetch helper gets account from sequencer
    assert!(
        code.contains("get_account(account_id)"),
        "fetch helper should fetch account data"
    );

    // Deduplication: only one fetch_multisig_state
    let count = code.matches("async fn fetch_multisig_state<").count();
    assert_eq!(count, 1, "should deduplicate fetch helpers by account name");
}

#[test]
fn test_borsh_import() {
    let output = generate_from_idl_json(SAMPLE_IDL).expect("codegen should succeed");
    assert!(
        output.client_code.contains("use borsh::BorshDeserialize;"),
        "should import BorshDeserialize"
    );
}

#[test]
fn test_pda_helper_with_numeric_seed() {
    let idl = r#"{
        "version": "0.1.0",
        "name": "counter",
        "instructions": [{
            "name": "increment",
            "accounts": [
                {
                    "name": "counter_state",
                    "writable": true,
                    "signer": false,
                    "init": false,
                    "pda": {
                        "seeds": [
                            {"kind": "const", "value": "counter"},
                            {"kind": "arg", "path": "counter_id"}
                        ]
                    }
                }
            ],
            "args": [
                {"name": "counter_id", "type": "u64"}
            ]
        }]
    }"#;
    let output = generate_from_idl_json(idl).expect("codegen should succeed");
    let code = &output.client_code;

    // u64 arg should be passed by value
    assert!(
        code.contains("counter_id: u64"),
        "numeric seed arg should be passed by value"
    );

    // Should use to_be_bytes for u64 and pad to [u8; 32]
    assert!(
        code.contains("counter_id_be = counter_id.to_be_bytes()"),
        "should convert u64 to big-endian bytes"
    );

    // Fetch helper for this account
    assert!(
        code.contains("async fn fetch_counter_state<T: BorshDeserialize>("),
        "should generate fetch helper for counter_state"
    );
}

#[test]
fn test_pda_helper_with_account_seed() {
    let idl = r#"{
        "version": "0.1.0",
        "name": "vault",
        "instructions": [{
            "name": "create_vault",
            "accounts": [
                {
                    "name": "vault_state",
                    "writable": true,
                    "signer": false,
                    "init": true,
                    "pda": {
                        "seeds": [
                            {"kind": "const", "value": "vault"},
                            {"kind": "account", "path": "owner"}
                        ]
                    }
                },
                {
                    "name": "owner",
                    "writable": false,
                    "signer": true,
                    "init": false
                }
            ],
            "args": []
        }]
    }"#;
    let output = generate_from_idl_json(idl).expect("codegen should succeed");
    let code = &output.client_code;

    // Account seed should be &AccountId
    assert!(
        code.contains("owner: &AccountId"),
        "account seed param should be &AccountId"
    );

    // Should use value() for AccountId to get &[u8; 32]
    assert!(
        code.contains("owner.value()"),
        "should use value() for account seed"
    );
}

#[test]
fn test_no_pda_no_helpers() {
    let idl = r#"{
        "version": "0.1.0",
        "name": "simple",
        "instructions": [{
            "name": "do_thing",
            "accounts": [
                {"name": "state", "writable": true, "signer": false, "init": false}
            ],
            "args": [{"name": "value", "type": "u64"}]
        }]
    }"#;
    let output = generate_from_idl_json(idl).expect("codegen should succeed");
    let code = &output.client_code;

    // No PDA helpers should be generated
    assert!(
        !code.contains("pub fn compute_"),
        "should not generate PDA helpers when no PDAs"
    );
    assert!(
        !code.contains("async fn fetch_"),
        "should not generate fetch helpers when no PDAs"
    );
}

#[test]
fn test_string_type_lowercased() {
    let idl = r#"{
        "version": "0.1.0",
        "name": "whisper_wall",
        "instructions": [{
            "name": "whisper",
            "accounts": [
                {"name": "user", "writable": true, "signer": true, "init": false}
            ],
            "args": [{"name": "msg", "type": "string"}]
        }]
    }"#;
    let output = generate_from_idl_json(idl).expect("codegen should succeed");
    
    // Client code should use `String` (uppercase), not `string` (lowercase)
    assert!(
        output.client_code.contains("msg: String"),
        "client code should have msg: String, got:\n{}",
        output.client_code
    );
    
    // FFI code should also use `String`
    assert!(
        output.ffi_code.contains("msg: String"),
        "ffi code should have msg: String, got:\n{}",
        output.ffi_code
    );
}

#[test]
fn test_ffi_parse_account_id_strips_prefix() {
    let output = generate_from_idl_json(SAMPLE_IDL).expect("codegen should succeed");

    // The generated parse_account_id should strip Public/ and Private/ prefixes
    assert!(
        output.ffi_code.contains(r#"s.strip_prefix("Public/")"#),
        "parse_account_id should strip Public/ prefix: {}",
        output.ffi_code
    );
    assert!(
        output.ffi_code.contains(r#"s.strip_prefix("Private/")"#),
        "parse_account_id should strip Private/ prefix: {}",
        output.ffi_code
    );
}

// ── FFI fetch function generation ─────────────────────────────────────────────

/// IDL with a const-only PDA and a u128 field (WhisperWall-like).
const WHISPER_WALL_IDL: &str = r#"{
    "version": "0.1.0",
    "name": "whisper_wall",
    "instructions": [
        {
            "name": "post",
            "accounts": [
                {
                    "name": "wall_state",
                    "writable": true,
                    "signer": false,
                    "init": false,
                    "pda": {
                        "seeds": [
                            {"kind": "const", "value": "wall_v2"}
                        ]
                    }
                },
                {
                    "name": "author",
                    "writable": true,
                    "signer": true,
                    "init": false
                }
            ],
            "args": [
                {"name": "message", "type": "string"},
                {"name": "nonce", "type": "u128"}
            ]
        }
    ],
    "accounts": [
        {
            "name": "WallState",
            "type": {
                "kind": "struct",
                "fields": [
                    {"name": "post_count", "type": "u64"},
                    {"name": "total_nonce", "type": "u128"},
                    {"name": "owner", "type": "account_id"}
                ]
            }
        }
    ],
    "types": [],
    "errors": []
}"#;

/// IDL with an arg-seeded PDA.
const ARG_SEED_IDL: &str = r#"{
    "version": "0.1.0",
    "name": "vault_prog",
    "instructions": [
        {
            "name": "create_vault",
            "accounts": [
                {
                    "name": "vault_state",
                    "writable": true,
                    "signer": false,
                    "init": true,
                    "pda": {
                        "seeds": [
                            {"kind": "const", "value": "vault"},
                            {"kind": "arg", "path": "vault_id"}
                        ]
                    }
                },
                {
                    "name": "owner",
                    "writable": false,
                    "signer": true,
                    "init": false
                }
            ],
            "args": [
                {"name": "vault_id", "type": "[u8; 32]"},
                {"name": "balance", "type": "u64"}
            ]
        }
    ],
    "accounts": [
        {
            "name": "VaultState",
            "type": {
                "kind": "struct",
                "fields": [
                    {"name": "vault_id", "type": "[u8; 32]"},
                    {"name": "balance", "type": "u64"},
                    {"name": "owner", "type": "account_id"}
                ]
            }
        }
    ],
    "types": [],
    "errors": []
}"#;

/// IDL with an account-seeded PDA.
const ACCOUNT_SEED_IDL: &str = r#"{
    "version": "0.1.0",
    "name": "pool_prog",
    "instructions": [
        {
            "name": "create_pool",
            "accounts": [
                {
                    "name": "pool_state",
                    "writable": true,
                    "signer": false,
                    "init": true,
                    "pda": {
                        "seeds": [
                            {"kind": "const", "value": "pool"},
                            {"kind": "account", "path": "creator"}
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
            "args": []
        }
    ],
    "accounts": [
        {
            "name": "PoolState",
            "type": {
                "kind": "struct",
                "fields": [
                    {"name": "creator", "type": "account_id"},
                    {"name": "token_count", "type": "u64"}
                ]
            }
        }
    ],
    "types": [],
    "errors": []
}"#;

#[test]
fn test_ffi_fetch_const_pda_no_trailing_comma() {
    let output = generate_from_idl_json(WHISPER_WALL_IDL).expect("codegen should succeed");
    let ffi = &output.ffi_code;

    // Fetch function must be emitted
    assert!(
        ffi.contains("pub extern \"C\" fn whisper_wall_fetch_wall_state("),
        "missing fetch function: {ffi}"
    );

    // Const-only PDA: compute_pda_with_program must be called with program_id + seed slice
    assert!(
        ffi.contains("compute_pda_with_program(&program_id, &["),
        "must use compute_pda_with_program with program_id: {ffi}"
    );

    // The PDA computation should use inline const seed only (no trailing comma after slice)
    assert!(
        ffi.contains(r#"b"wall_v2","#),
        "const seed must be inlined: {ffi}"
    );
    assert!(
        !ffi.contains(r#"b"wall_v2", )"#),
        "must not generate trailing comma after seed slice: {ffi}"
    );
}

#[test]
fn test_ffi_fetch_u128_field_uses_to_string() {
    let output = generate_from_idl_json(WHISPER_WALL_IDL).expect("codegen should succeed");
    let ffi = &output.ffi_code;

    // u128 field must use .to_string() in json! macro, not bare value
    assert!(
        ffi.contains("state.total_nonce.to_string()"),
        "u128 field must use .to_string() in json!: {ffi}"
    );

    // u64 field should be direct (no .to_string())
    assert!(
        ffi.contains("state.post_count"),
        "u64 field should be present: {ffi}"
    );
    // The u64 field should NOT be followed by .to_string()
    assert!(
        !ffi.contains("state.post_count.to_string()"),
        "u64 field must not use .to_string(): {ffi}"
    );
}

#[test]
fn test_ffi_fetch_account_id_field_uses_hex_encode() {
    let output = generate_from_idl_json(WHISPER_WALL_IDL).expect("codegen should succeed");
    let ffi = &output.ffi_code;

    // account_id field must use hex::encode
    assert!(
        ffi.contains("hex::encode(&state.owner)"),
        "account_id field must use hex::encode: {ffi}"
    );
}

#[test]
fn test_ffi_fetch_arg_seeded_pda_uses_args_accessor() {
    let output = generate_from_idl_json(ARG_SEED_IDL).expect("codegen should succeed");
    let ffi = &output.ffi_code;

    // Fetch function must be emitted
    assert!(
        ffi.contains("pub extern \"C\" fn vault_prog_fetch_vault_state("),
        "missing fetch function: {ffi}"
    );

    // Arg seed must be read from args["vault_id"], not a bare variable
    assert!(
        ffi.contains(r#"args["vault_id"]"#),
        "arg seed must be accessed via args[\"vault_id\"]: {ffi}"
    );

    // Must NOT use a bare `vault_id` variable that was never declared
    // (The parse line must set `let vault_id = ...` before using it in compute_pda)
    let fetch_impl_start = ffi.find("fn vault_prog_fetch_vault_state_impl").unwrap_or(0);
    let fetch_section = &ffi[fetch_impl_start..];
    let args_access_pos = fetch_section.find(r#"args["vault_id"]"#).unwrap_or(usize::MAX);
    let vault_id_in_pda_pos = fetch_section.find("vault_id.as_ref()").unwrap_or(usize::MAX);
    assert!(
        args_access_pos < vault_id_in_pda_pos,
        "args[\"vault_id\"] parse must come before vault_id use in PDA: {ffi}"
    );
}

#[test]
fn test_ffi_fetch_account_seeded_pda_uses_parse_account_id() {
    let output = generate_from_idl_json(ACCOUNT_SEED_IDL).expect("codegen should succeed");
    let ffi = &output.ffi_code;

    // Fetch function must be emitted
    assert!(
        ffi.contains("pub extern \"C\" fn pool_prog_fetch_pool_state("),
        "missing fetch function: {ffi}"
    );

    // Account seed must be parsed as AccountId from args JSON
    assert!(
        ffi.contains(r#"args["creator"]"#),
        "account seed must be accessed via args[\"creator\"]: {ffi}"
    );
    assert!(
        ffi.contains("parse_account_id"),
        "account seed must use parse_account_id: {ffi}"
    );
}

#[test]
fn test_ffi_fetch_sets_sequencer_url() {
    let output = generate_from_idl_json(WHISPER_WALL_IDL).expect("codegen should succeed");
    let ffi = &output.ffi_code;

    // sequencer_url must be set (not silently ignored)
    assert!(
        ffi.contains("NSSA_SEQUENCER_URL"),
        "fetch function must set NSSA_SEQUENCER_URL env var: {ffi}"
    );
}

#[test]
fn test_ffi_fetch_struct_generated() {
    let output = generate_from_idl_json(WHISPER_WALL_IDL).expect("codegen should succeed");
    let ffi = &output.ffi_code;

    // BorshDeserialize struct must be generated
    assert!(
        ffi.contains("#[derive(borsh::BorshDeserialize)]"),
        "must emit BorshDeserialize struct: {ffi}"
    );
    assert!(
        ffi.contains("struct WallStateState {"),
        "must emit WallStateState struct: {ffi}"
    );
    assert!(
        ffi.contains("pub post_count: u64"),
        "struct must have post_count: u64: {ffi}"
    );
    assert!(
        ffi.contains("pub total_nonce: u128"),
        "struct must have total_nonce: u128: {ffi}"
    );
}

#[test]
fn test_ffi_fetch_deduplication() {
    // Same PDA account in two instructions — fetch function generated exactly once
    let idl = r#"{
        "version": "0.1.0",
        "name": "multi_ix",
        "instructions": [
            {
                "name": "read",
                "accounts": [
                    {
                        "name": "shared_state",
                        "writable": false,
                        "signer": false,
                        "init": false,
                        "pda": {"seeds": [{"kind": "const", "value": "shared"}]}
                    }
                ],
                "args": []
            },
            {
                "name": "write",
                "accounts": [
                    {
                        "name": "shared_state",
                        "writable": true,
                        "signer": false,
                        "init": false,
                        "pda": {"seeds": [{"kind": "const", "value": "shared"}]}
                    }
                ],
                "args": []
            }
        ],
        "accounts": [
            {
                "name": "SharedState",
                "type": {
                    "kind": "struct",
                    "fields": [{"name": "value", "type": "u64"}]
                }
            }
        ],
        "types": [],
        "errors": []
    }"#;
    let output = generate_from_idl_json(idl).expect("codegen should succeed");
    let count = output.ffi_code.matches("pub extern \"C\" fn multi_ix_fetch_shared_state(").count();
    assert_eq!(count, 1, "fetch function should be generated exactly once, got {count}");
}

#[test]
fn test_ffi_fetch_no_type_info_no_function() {
    // PDA account without a matching entry in idl.accounts → no fetch function generated
    let output = generate_from_idl_json(SAMPLE_IDL).expect("codegen should succeed");
    // SAMPLE_IDL has multisig_state PDA but no accounts[] entry
    assert!(
        !output.ffi_code.contains("fn my_multisig_fetch_multisig_state"),
        "should not generate fetch function when no account type info is available"
    );
}

#[test]
fn test_header_includes_fetch_declaration() {
    let output = generate_from_idl_json(WHISPER_WALL_IDL).expect("codegen should succeed");

    assert!(
        output.header.contains("char* whisper_wall_fetch_wall_state(const char* args_json)"),
        "C header must include fetch function declaration: {}",
        output.header
    );
}

#[test]
fn test_ffi_fetch_borsh_decode_in_function() {
    let output = generate_from_idl_json(WHISPER_WALL_IDL).expect("codegen should succeed");
    let ffi = &output.ffi_code;

    // Must call try_from_slice on the state struct via the fully-qualified trait path
    assert!(
        ffi.contains("<WallStateState as borsh::BorshDeserialize>::try_from_slice("),
        "fetch function must decode via <WallStateState as borsh::BorshDeserialize>::try_from_slice: {ffi}"
    );

    // Must call get_account
    assert!(
        ffi.contains("get_account(pda)"),
        "fetch function must call get_account(pda): {ffi}"
    );

    // Must return success JSON with state
    assert!(
        ffi.contains("\"success\": true"),
        "fetch function must return success:true: {ffi}"
    );
    assert!(
        ffi.contains("\"state\""),
        "fetch function must return state field: {ffi}"
    );
}

// ── Syntax validity tests ─────────────────────────────────────────────────────
//
// Parse the generated FFI code as a syn::File to catch syntax errors
// (trailing commas, malformed expressions, bad types) without needing
// the full nssa/wallet/tokio dependency tree available at test time.
//
// This catches bugs like:
//   - `compute_pda(&program_id, )` — trailing comma (Bug 3 from PR #147)
//   - bare undefined variables in PDA seed expressions (Bug 1)
//   - `body_lines` ordering inversions that reference variables before they're bound (Bug 2)
//   - `[u32;8].as_bytes()` call expressions that are syntactically invalid (Bug 6)

fn assert_parses_as_rust(label: &str, src: &str) {
    // Parse the source as-is as a full Rust file.
    // This validates syntax structure without requiring the full dependency tree
    // or successful type checking during tests.
    match syn::parse_str::<syn::File>(src) {
        Ok(_) => {}
        Err(e) => panic!("{label}: generated code is not valid Rust syntax:\n{e}\n\nSource:\n{src}"),
    }
}

#[test]
fn test_ffi_code_is_valid_rust_syntax_const_pda() {
    let output = generate_from_idl_json(WHISPER_WALL_IDL).expect("codegen should succeed");
    assert_parses_as_rust("WHISPER_WALL_IDL ffi_code", &output.ffi_code);
}

#[test]
fn test_ffi_code_is_valid_rust_syntax_arg_seed() {
    let output = generate_from_idl_json(ARG_SEED_IDL).expect("codegen should succeed");
    assert_parses_as_rust("ARG_SEED_IDL ffi_code", &output.ffi_code);
}

#[test]
fn test_ffi_code_is_valid_rust_syntax_account_seed() {
    let output = generate_from_idl_json(ACCOUNT_SEED_IDL).expect("codegen should succeed");
    assert_parses_as_rust("ACCOUNT_SEED_IDL ffi_code", &output.ffi_code);
}

#[test]
fn test_account_seed_pda_binding_order() {
    // ACCOUNT_SEED_IDL lists pool_state (PDA) before creator (plain account) in ix.accounts.
    // The two-pass resolver must emit `let creator = ...` before `let pool_state = ...` so
    // that `creator.as_ref()` in the PDA seed slice references an already-bound variable.
    let output = generate_from_idl_json(ACCOUNT_SEED_IDL).expect("codegen should succeed");
    let ffi = &output.ffi_code;

    let pos_creator = ffi.find("let creator = parse_account_id")
        .expect("must bind creator from JSON args");
    let pos_pool_state = ffi.find("let pool_state = compute_pda_with_program")
        .expect("must compute pool_state PDA");

    assert!(
        pos_creator < pos_pool_state,
        "plain account binding (creator) must appear before PDA binding (pool_state) in generated code"
    );
}

#[test]
fn test_ffi_code_is_valid_rust_syntax_sample_idl() {
    let output = generate_from_idl_json(SAMPLE_IDL).expect("codegen should succeed");
    assert_parses_as_rust("SAMPLE_IDL ffi_code", &output.ffi_code);
}
