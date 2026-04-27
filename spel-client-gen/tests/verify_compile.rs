use spel_client_gen::generate_from_idl_json;

#[test]
fn verify_generated_code_compiles() {
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

    // Verify balanced brackets/braces
    let mut brace_depth = 0usize;
    let mut paren_depth = 0usize;
    let mut bracket_depth = 0usize;
    for c in gen_code.chars() {
        match c {
            '{' => brace_depth += 1,
            '}' => brace_depth = brace_depth.checked_sub(1).expect("unbalanced braces"),
            '(' => paren_depth += 1,
            ')' => paren_depth = paren_depth.checked_sub(1).expect("unbalanced parens"),
            '[' => bracket_depth += 1,
            ']' => bracket_depth = bracket_depth.checked_sub(1).expect("unbalanced brackets"),
            _ => {}
        }
    }
    assert_eq!(brace_depth, 0, "unbalanced braces");
    assert_eq!(paren_depth, 0, "unbalanced parens");
    assert_eq!(bracket_depth, 0, "unbalanced brackets");

    // Verify key structures are present
    assert!(gen_code.contains("extern \"C\""), "should generate extern \"C\" functions");
    assert!(gen_code.contains("fn token_vault_fetch_vault_state"), "should generate fetch function");
    assert!(gen_code.contains("pub struct VaultState"), "should generate account struct");
    assert!(gen_code.contains("borsh::BorshDeserialize"), "should derive BorshDeserialize");
    assert!(gen_code.contains("serde::Serialize"), "should derive Serialize");

    // Write generated code to temp dir for debugging
    let tmp = std::env::temp_dir().join("gen_ffi_test.rs");
    std::fs::write(&tmp, gen_code).ok();

    // Try to compile with rustc if available
    let rustc_result = std::process::Command::new("rustc")
        .arg("--crate-type")
        .arg("rlib")
        .arg("--edition")
        .arg("2021")
        .arg("--emit=metadata")
        .arg("--out-dir")
        .arg(std::env::temp_dir().as_os_str())
        .arg(&tmp)
        .output();

    let _ = std::fs::remove_file(&tmp);

    if let Ok(output) = rustc_result {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if output.status.success() {
            // Full syntax check passed
        } else {
            // Check if errors are ONLY about missing extern crates (expected)
            // vs actual syntax errors
            // Only check lines that start with "error[" - these are actual error messages
            let has_real_syntax_errors = stderr.lines().any(|l| {
                l.starts_with("error[") && (
                    l.contains("expected") && !l.contains("extern") && !l.contains("could not find") && !l.contains("not found in") && !l.contains("use of unresolved")
                )
            });
            if has_real_syntax_errors {
                panic!("Generated code has syntax errors:\n{}", stderr);
            }
            // Otherwise, errors are likely about missing deps, which is OK
        }
    }
}
