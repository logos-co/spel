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

    // Write the full generated code to a temp file
    let tmp = std::env::temp_dir().join("gen_ffi_test.rs");
    std::fs::write(&tmp, gen_code).expect("write tmp");

    // Find workspace root via CARGO_MANIFEST_DIR (package dir) → go up one level to workspace root
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let workspace_root = std::path::Path::new(&manifest_dir).parent().unwrap();
    let deps = workspace_root.join("target/debug/deps");

    fn find_rlib(deps: &std::path::Path, name: &str) -> String {
        let prefix = format!("lib{}", name);
        let mut matches: Vec<_> = std::fs::read_dir(deps)
            .expect("read deps dir")
            .filter_map(|e| e.ok())
            .filter(|e| {
                let fname = e.file_name().to_string_lossy().into_owned();
                fname.starts_with(&prefix)
                    && fname.ends_with(".rlib")
                    && fname.len() > prefix.len() + ".rlib".len()
                    && fname[prefix.len()+1..fname.len()-".rlib".len()].chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_')
            })
            .map(|e| e.path())
            .collect();
        matches.sort();
        matches.pop().unwrap().to_str().expect("valid path").to_string()
    }

    let mut cmd = std::process::Command::new("rustc");
    cmd.arg("--crate-type").arg("lib")
       .arg("--edition").arg("2021")
       .arg("-L").arg(&deps);  // Add -L so rustc can find deps

    for lib in &[
        "nssa", "serde_json", "sha2", "borsh", "serde", "wallet",
        "sequencer_service_rpc", "tokio", "hex", "spel_framework_core",
        "common", "nssa_core",
    ] {
        cmd.arg("--extern")
           .arg(format!("{}={}", lib, find_rlib(&deps, lib)));
    }
    cmd.arg(&tmp);

    let output = cmd.output().expect("rustc failed");
    let stderr = String::from_utf8_lossy(&output.stderr);

    let _ = std::fs::remove_file(&tmp);

    if !output.status.success() {
        eprintln!("=== GENERATED CODE ({} chars) ===", gen_code.len());
        eprintln!("{}", gen_code);
        eprintln!("=== END GENERATED CODE ===");
        panic!("Generated code does NOT compile!\nstderr:\n{}", stderr);
    }
}
