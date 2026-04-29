fn main() {
    let idl = r#"{
        "version": "0.1.0",
        "name": "compile_test",
        "instructions": [
            {
                "name": "initialize",
                "accounts": [
                    {
                        "name": "state",
                        "writable": true, "signer": false, "init": true,
                        "pda": { "seeds": [{"kind": "const", "value": "state_v1"}] }
                    },
                    { "name": "admin", "writable": false, "signer": true, "init": false }
                ],
                "args": []
            },
            {
                "name": "create_entry",
                "accounts": [
                    {
                        "name": "entry",
                        "writable": true, "signer": false, "init": true,
                        "pda": { "seeds": [
                            {"kind": "const", "value": "entry"},
                            {"kind": "arg", "path": "owner_key"}
                        ]}
                    },
                    { "name": "owner", "writable": false, "signer": true, "init": false }
                ],
                "args": [
                    {"name": "owner_key", "type": "[u8; 32]"},
                    {"name": "label", "type": "string"}
                ]
            }
        ],
        "accounts": [
            {
                "name": "State",
                "type": {
                    "kind": "struct",
                    "fields": [
                        {"name": "admin",       "type": "account_id"},
                        {"name": "msg",         "type": "string"},
                        {"name": "last_tip",    "type": "u128"},
                        {"name": "post_count",  "type": "u64"}
                    ]
                }
            }
        ],
        "types": [],
        "errors": []
    }"#;

    let output = spel_client_gen::generate_from_idl_json(idl)
        .expect("FFI codegen failed for compile-test IDL");

    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    std::fs::write(out_dir.join("generated_ffi.rs"), &output.ffi_code)
        .expect("failed to write generated_ffi.rs");

    println!("cargo:rerun-if-changed=build.rs");
}
