//! `spel pda` regression tests against the real binary: IDL-mode routing.

use nssa_core::program::PdaSeed;
use std::process::Command;

fn spel(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_spel"))
        .args(args)
        .output()
        .expect("failed to run spel binary")
}

fn stdout_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// One public PDA and one private PDA, both seeded by the constant "vault".
fn write_fixture_idl(dir: &std::path::Path) -> std::path::PathBuf {
    let idl = serde_json::json!({
        "version": "0.1.0",
        "name": "fixture",
        "instructions": [{
            "name": "init",
            "accounts": [
                { "name": "caller", "signer": true },
                { "name": "public_vault", "init": true,
                  "pda": { "seeds": [{ "kind": "const", "value": "vault" }] } },
                { "name": "private_vault", "init": true,
                  "pda": { "seeds": [{ "kind": "const", "value": "vault" }], "private": true } }
            ],
            "args": []
        }]
    });
    let path = dir.join("fixture-idl.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&idl).unwrap()).unwrap();
    path
}

const PROGRAM_ID_HEX: &str = "abababababababababababababababababababababababababababababababab";

/// The fixture program id as the CLI decodes it: 32 hex bytes read as 8 little-endian u32s.
fn fixture_program_id() -> [u32; 8] {
    let bytes = hex::decode(PROGRAM_ID_HEX).expect("fixture program id is valid hex");
    let words: Vec<u32> = bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().expect("chunks_exact(4) yields 4 bytes")))
        .collect();
    words.try_into().expect("32 bytes make 8 words")
}

/// The IDL's const seed "vault", zero-padded to 32 bytes as the CLI does.
fn vault_seed() -> [u8; 32] {
    let mut seed = [0u8; 32];
    seed[..b"vault".len()].copy_from_slice(b"vault");
    seed
}

fn expected_public_vault() -> String {
    let program_id = fixture_program_id();
    let seed = vault_seed();
    nssa::AccountId::for_public_pda(&program_id, &PdaSeed::new(seed)).to_string()
}

// Half of #187 that stayed open: with `--idl` given, `--program <hex> pda <account>`
// still routed into raw mode and hashed the account *name* as a seed, so the
// documented `--idl ... --program <hex> pda vault` form derived the wrong address.
#[test]
fn idl_given_routes_pda_to_idl_mode_not_raw() {
    let dir = tempfile::tempdir().unwrap();
    let idl = write_fixture_idl(dir.path());

    let out = spel(&[
        "--idl",
        idl.to_str().unwrap(),
        "--program",
        PROGRAM_ID_HEX,
        "pda",
        "public_vault",
    ]);
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    assert_eq!(
        stdout_of(&out),
        expected_public_vault(),
        "must derive from the IDL seed \"vault\", not from the raw token \"public_vault\""
    );
}

// Raw mode is still reachable when no IDL is given.
#[test]
fn no_idl_keeps_raw_pda_mode() {
    let out = spel(&["--program", PROGRAM_ID_HEX, "pda", "vault"]);
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    assert_eq!(stdout_of(&out), expected_public_vault());
}
