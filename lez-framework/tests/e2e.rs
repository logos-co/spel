//! End-to-end tests for the lez-framework pipeline:
//! scaffold → build → IDL generation → FFI build → test
//!
//! These tests exercise a real #[lez_program] fixture program located at
//! tests/e2e/fixture_program/ by shelling out to cargo commands and
//! validating the generated IDL and client/FFI code.

use std::path::PathBuf;
use std::process::Command;

fn fixture_manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/e2e/fixture_program/Cargo.toml")
}

// ---------------------------------------------------------------------------
// Step 1 + 3: Build — cargo build the fixture program targeting host
// ---------------------------------------------------------------------------

#[test]
fn e2e_build() {
    let output = Command::new("cargo")
        .args(["build", "--manifest-path"])
        .arg(fixture_manifest())
        .output()
        .expect("Failed to run cargo build");

    assert!(
        output.status.success(),
        "cargo build failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ---------------------------------------------------------------------------
// Step 2: IDL generation — extract IDL from the fixture and validate
// ---------------------------------------------------------------------------

#[test]
fn e2e_idl_generation() {
    let output = Command::new("cargo")
        .args(["run", "--manifest-path"])
        .arg(fixture_manifest())
        .output()
        .expect("Failed to run fixture binary");

    assert!(
        output.status.success(),
        "cargo run failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let idl_json = String::from_utf8(output.stdout).unwrap();
    let idl_json = idl_json.trim();
    let idl: lez_framework::idl::LezIdl =
        serde_json::from_str(idl_json).expect("IDL JSON should be valid");

    // Top-level fields
    assert_eq!(idl.version, "0.1.0");
    assert_eq!(idl.name, "treasury");
    assert_eq!(idl.instructions.len(), 4);

    // initialize instruction
    let init = &idl.instructions[0];
    assert_eq!(init.name, "initialize");
    assert_eq!(init.accounts.len(), 2);
    assert!(init.accounts[0].init, "state should be init");
    assert!(init.accounts[0].writable, "init implies writable");
    assert!(init.accounts[0].pda.is_some(), "state should have PDA");
    let pda = init.accounts[0].pda.as_ref().unwrap();
    assert_eq!(pda.seeds.len(), 1);
    assert!(init.accounts[1].signer, "authority should be signer");
    assert_eq!(init.args.len(), 1);
    assert_eq!(init.args[0].name, "threshold");

    // create_vault instruction
    let vault = &idl.instructions[1];
    assert_eq!(vault.name, "create_vault");
    assert_eq!(vault.accounts.len(), 2);
    assert!(vault.accounts[0].init, "vault should be init");
    assert!(vault.accounts[0].pda.is_some(), "vault should have PDA");
    assert!(vault.accounts[1].signer, "owner should be signer");
    assert_eq!(vault.args.len(), 1);
    assert_eq!(vault.args[0].name, "owner_key");

    // create_config instruction
    let config = &idl.instructions[2];
    assert_eq!(config.name, "create_config");
    assert_eq!(config.accounts.len(), 2);
    assert!(config.accounts[0].init, "config should be init");
    assert!(config.accounts[0].pda.is_some(), "config should have PDA");
    let config_pda = config.accounts[0].pda.as_ref().unwrap();
    assert_eq!(config_pda.seeds.len(), 2);
    assert!(config.accounts[1].signer, "admin should be signer");
    assert_eq!(config.args.len(), 1);
    assert_eq!(config.args[0].name, "user_id");

    // transfer instruction
    let transfer = &idl.instructions[3];
    assert_eq!(transfer.name, "transfer");
    assert_eq!(transfer.accounts.len(), 3);
    assert!(transfer.accounts[0].writable, "from should be writable");
    assert!(transfer.accounts[1].writable, "to should be writable");
    assert!(transfer.accounts[2].signer, "signer should be signer");
    assert_eq!(transfer.args.len(), 2);
    assert_eq!(transfer.args[0].name, "amount");
    assert_eq!(transfer.args[1].name, "memo");
}

// ---------------------------------------------------------------------------
// Step 4: FFI build — generate client/FFI code from IDL and validate
// ---------------------------------------------------------------------------

#[test]
fn e2e_ffi_build() {
    // Extract IDL from fixture
    let output = Command::new("cargo")
        .args(["run", "--manifest-path"])
        .arg(fixture_manifest())
        .output()
        .expect("Failed to run fixture binary");

    assert!(output.status.success());
    let idl_json = String::from_utf8(output.stdout).unwrap();

    // Generate client + FFI code
    let codegen = lez_client_gen::generate_from_idl_json(idl_json.trim())
        .expect("Client codegen should succeed");

    // Client code assertions
    assert!(!codegen.client_code.is_empty());
    assert!(
        codegen.client_code.contains("TreasuryInstruction"),
        "client should contain TreasuryInstruction enum"
    );
    assert!(
        codegen.client_code.contains("TreasuryClient"),
        "client should contain TreasuryClient struct"
    );
    assert!(
        codegen.client_code.contains("fn initialize"),
        "client should have initialize method"
    );
    assert!(
        codegen.client_code.contains("fn transfer"),
        "client should have transfer method"
    );
    assert!(
        codegen.client_code.contains("InitializeAccounts"),
        "client should have InitializeAccounts struct"
    );
    assert!(
        codegen.client_code.contains("TransferAccounts"),
        "client should have TransferAccounts struct"
    );

    // FFI code assertions
    assert!(!codegen.ffi_code.is_empty());
    assert!(
        codegen.ffi_code.contains("treasury_initialize"),
        "FFI should have treasury_initialize function"
    );
    assert!(
        codegen.ffi_code.contains("treasury_transfer"),
        "FFI should have treasury_transfer function"
    );
    assert!(
        codegen.ffi_code.contains("extern \"C\""),
        "FFI should have extern C functions"
    );
    assert!(
        codegen.ffi_code.contains("treasury_free_string"),
        "FFI should have free_string function"
    );

    // Header assertions
    assert!(!codegen.header.is_empty());
    assert!(
        codegen.header.contains("treasury_initialize"),
        "header should declare treasury_initialize"
    );
    assert!(
        codegen.header.contains("treasury_transfer"),
        "header should declare treasury_transfer"
    );
    assert!(
        codegen.header.contains("TREASURY_FFI_H"),
        "header should have include guard"
    );
}

// ---------------------------------------------------------------------------
// Step 5: Test — cargo test the fixture (validates cfg-gate fix)
// ---------------------------------------------------------------------------

#[test]
fn e2e_test() {
    let output = Command::new("cargo")
        .args(["test", "--manifest-path"])
        .arg(fixture_manifest())
        .output()
        .expect("Failed to run cargo test");

    assert!(
        output.status.success(),
        "cargo test on fixture failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{}{}", stdout, stderr);
    assert!(
        combined.contains("test result: ok"),
        "Expected all fixture tests to pass:\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );
}
