use spel_client_gen::generate_from_idl_json;

#[test]
fn verify_generated_code_is_valid_rust() {
    let idl = r#"{
        "version": "0.1.0",
        "name": "token_vault",
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
                                {"kind": "arg", "path": "owner"}
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
                    {"name": "owner", "type": "[u8; 32]"}
                ]
            }
        ],
        "accounts": [
            {
                "name": "VaultState",
                "type": {
                    "kind": "struct",
                    "fields": [
                        {"name": "owner_id", "type": "[u8; 32]"},
                        {"name": "balance", "type": "u64"}
                    ]
                }
            }
        ],
        "types": [],
        "errors": []
    }"#;

    let output = generate_from_idl_json(idl).expect("codegen should succeed");
    let gen_code = &output.ffi_code;

    // Verify the generated code has balanced braces and parentheses
    let mut brace_depth = 0usize;
    let mut paren_depth = 0usize;
    let mut bracket_depth = 0usize;
    for c in gen_code.chars() {
        match c {
            '{' => brace_depth += 1,
            '}' => brace_depth = brace_depth.checked_sub(1).expect("unbalanced braces"),
            '(' => paren_depth += 1,
            ')' => paren_depth = paren_depth.checked_sub(1).expect("unbalanced parentheses"),
            '[' => bracket_depth += 1,
            ']' => bracket_depth = bracket_depth.checked_sub(1).expect("unbalanced brackets"),
            _ => {}
        }
    }
    assert_eq!(brace_depth, 0, "unbalanced braces in generated code");
    assert_eq!(paren_depth, 0, "unbalanced parentheses in generated code");
    assert_eq!(bracket_depth, 0, "unbalanced brackets in generated code");

    // Verify key structures are present
    assert!(gen_code.contains("extern \"C\""), "should generate extern \"C\" functions");
    assert!(gen_code.contains("fn token_vault_fetch_vault_state"), "should generate fetch function");
    assert!(gen_code.contains("pub struct VaultState"), "should generate account struct");
    assert!(gen_code.contains("borsh::BorshDeserialize"), "should derive BorshDeserialize");
    assert!(gen_code.contains("borsh::BorshSerialize"), "should derive BorshSerialize");
    assert!(gen_code.contains("serde::Serialize"), "should derive Serialize");
    assert!(gen_code.contains("serde::Deserialize"), "should derive Deserialize");

    // Write generated code to temp file for debugging if test fails
    let tmp = std::env::temp_dir().join("gen_ffi_test.rs");
    std::fs::write(&tmp, gen_code).ok();
}
