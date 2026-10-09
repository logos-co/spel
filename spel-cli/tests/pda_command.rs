//! `spel pda` regression tests against the real binary: IDL-mode routing and
//! the private-PDA `--identifier` flag.

use nssa_core::encryption::ViewingPublicKey;
use nssa_core::program::PdaSeed;
use nssa_core::NullifierPublicKey;
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
const NPK_HEX: &str = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";

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

fn fixture_vpk() -> ViewingPublicKey {
    ViewingPublicKey::from_seed(&[1u8; 32], &[2u8; 32])
}

fn expected_private_vault(identifier: u128) -> String {
    let program_id = fixture_program_id();
    let seed = vault_seed();
    let npk_bytes: [u8; 32] = hex::decode(NPK_HEX)
        .expect("fixture npk is valid hex")
        .try_into()
        .expect("fixture npk is 32 bytes");
    let npk = NullifierPublicKey(npk_bytes);
    nssa::AccountId::for_private_pda(
        &program_id,
        &PdaSeed::new(seed),
        &npk,
        &fixture_vpk(),
        identifier,
    )
    .to_string()
}

fn private_pda_args<'a>(idl: &'a str, vpk_hex: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec![
        "--idl",
        idl,
        "--program",
        PROGRAM_ID_HEX,
        "pda",
        "private_vault",
        "--npk",
        NPK_HEX,
        "--vpk",
        vpk_hex,
    ];
    args.extend_from_slice(extra);
    args
}

#[test]
fn private_pda_without_identifier_derives_with_zero() {
    let dir = tempfile::tempdir().unwrap();
    let idl = write_fixture_idl(dir.path());
    let vpk_hex = hex::encode(fixture_vpk().to_bytes());

    let out = spel(&private_pda_args(idl.to_str().unwrap(), &vpk_hex, &[]));
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    assert_eq!(stdout_of(&out), expected_private_vault(0));
}

#[test]
fn private_pda_identifier_is_threaded_into_derivation() {
    let dir = tempfile::tempdir().unwrap();
    let idl = write_fixture_idl(dir.path());
    let vpk_hex = hex::encode(fixture_vpk().to_bytes());

    let out = spel(&private_pda_args(
        idl.to_str().unwrap(),
        &vpk_hex,
        &["--identifier", "7"],
    ));
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    assert_eq!(stdout_of(&out), expected_private_vault(7));
    assert_ne!(stdout_of(&out), expected_private_vault(0));
}

#[test]
fn private_pda_identifier_accepts_hex_and_u128_max() {
    let dir = tempfile::tempdir().unwrap();
    let idl = write_fixture_idl(dir.path());
    let vpk_hex = hex::encode(fixture_vpk().to_bytes());

    let out = spel(&private_pda_args(
        idl.to_str().unwrap(),
        &vpk_hex,
        &["--identifier", "0xff"],
    ));
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    assert_eq!(stdout_of(&out), expected_private_vault(0xff));

    let max = u128::MAX.to_string();
    let out = spel(&private_pda_args(
        idl.to_str().unwrap(),
        &vpk_hex,
        &["--identifier", &max],
    ));
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    assert_eq!(stdout_of(&out), expected_private_vault(u128::MAX));
}

#[test]
fn private_pda_identifier_rejects_malformed_values() {
    let dir = tempfile::tempdir().unwrap();
    let idl = write_fixture_idl(dir.path());
    let vpk_hex = hex::encode(fixture_vpk().to_bytes());

    for bad in [
        "-1",
        "abc",
        "1.5",
        "340282366920938463463374607431768211456",
    ] {
        let out = spel(&private_pda_args(
            idl.to_str().unwrap(),
            &vpk_hex,
            &["--identifier", bad],
        ));
        assert!(!out.status.success(), "'{bad}' must be rejected");
        let err = stderr_of(&out);
        assert!(
            err.contains("Invalid --identifier") && err.contains(bad),
            "stderr names the flag and the value for '{bad}': {err}"
        );
    }

    // Flag given as the last token, no value.
    let out = spel(&private_pda_args(
        idl.to_str().unwrap(),
        &vpk_hex,
        &["--identifier"],
    ));
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("Missing value for --identifier"),
        "stderr: {}",
        stderr_of(&out)
    );
}

#[test]
fn public_pda_refuses_identifier() {
    let dir = tempfile::tempdir().unwrap();
    let idl = write_fixture_idl(dir.path());

    let out = spel(&[
        "--idl",
        idl.to_str().unwrap(),
        "--program",
        PROGRAM_ID_HEX,
        "pda",
        "public_vault",
        "--identifier",
        "7",
    ]);
    assert!(!out.status.success(), "public PDA must refuse --identifier");
    let err = stderr_of(&out);
    assert!(
        err.contains("--identifier") && err.contains("public_vault"),
        "stderr names the flag and the account: {err}"
    );
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
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_spel"))
        .current_dir(dir.path())
        .args(["--program", PROGRAM_ID_HEX, "pda", "vault"])
        .output()
        .expect("failed to run spel binary");
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    assert_eq!(stdout_of(&out), expected_public_vault());
}

#[test]
fn identifier_equals_form_is_named_in_the_error() {
    let dir = tempfile::tempdir().unwrap();
    let idl = write_fixture_idl(dir.path());
    let vpk_hex = hex::encode(fixture_vpk().to_bytes());

    let out = spel(&private_pda_args(
        idl.to_str().unwrap(),
        &vpk_hex,
        &["--identifier=7"],
    ));
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(
        err.contains("--key=value form is not supported") && err.contains("--identifier <value>"),
        "stderr explains the form and the fix: {err}"
    );
}

/// An instruction whose PDA is seeded by an arg literally named `identifier`.
/// Before this flag existed, `--identifier` was that seed arg; it must stay so.
fn write_identifier_seed_idl(dir: &std::path::Path) -> std::path::PathBuf {
    let idl = serde_json::json!({
        "version": "0.1.0",
        "name": "fixture",
        "instructions": [{
            "name": "open",
            "accounts": [
                { "name": "slot", "init": true,
                  "pda": { "seeds": [
                      { "kind": "const", "value": "slot" },
                      { "kind": "arg", "path": "identifier" }
                  ] } },
                { "name": "private_slot", "init": true,
                  "pda": { "seeds": [
                      { "kind": "const", "value": "slot" },
                      { "kind": "arg", "path": "identifier" }
                  ], "private": true } }
            ],
            "args": [{ "name": "identifier", "type": "u64" }]
        }]
    });
    let path = dir.join("identifier-seed-idl.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&idl).unwrap()).unwrap();
    path
}

fn expected_slot_seed(identifier_arg: u64) -> [u8; 32] {
    // Mirrors the CLI: SHA-256(const_seed || arg_seed), u64 arg big-endian in the last 8 bytes.
    use risc0_zkvm::sha::{Impl, Sha256};
    let mut const_seed = [0u8; 32];
    const_seed[..b"slot".len()].copy_from_slice(b"slot");
    let mut arg_seed = [0u8; 32];
    arg_seed[24..].copy_from_slice(&identifier_arg.to_be_bytes());
    let mut input = Vec::with_capacity(64);
    input.extend_from_slice(&const_seed);
    input.extend_from_slice(&arg_seed);
    Impl::hash_bytes(&input)
        .as_bytes()
        .try_into()
        .expect("SHA-256 is 32 bytes")
}

#[test]
fn seed_arg_named_identifier_keeps_its_meaning_on_public_pda() {
    let dir = tempfile::tempdir().unwrap();
    let idl = write_identifier_seed_idl(dir.path());

    let out = spel(&[
        "--idl",
        idl.to_str().unwrap(),
        "--program",
        PROGRAM_ID_HEX,
        "pda",
        "slot",
        "--identifier",
        "5",
    ]);
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let expected = nssa::AccountId::for_public_pda(
        &fixture_program_id(),
        &PdaSeed::new(expected_slot_seed(5)),
    )
    .to_string();
    assert_eq!(
        stdout_of(&out),
        expected,
        "--identifier must be the seed arg here"
    );
}

#[test]
fn seed_arg_named_identifier_on_private_pda_uses_default_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let idl = write_identifier_seed_idl(dir.path());
    let vpk_hex = hex::encode(fixture_vpk().to_bytes());

    let out = spel(&[
        "--idl",
        idl.to_str().unwrap(),
        "--program",
        PROGRAM_ID_HEX,
        "pda",
        "private_slot",
        "--npk",
        NPK_HEX,
        "--vpk",
        &vpk_hex,
        "--identifier",
        "5",
    ]);
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let npk_bytes: [u8; 32] = hex::decode(NPK_HEX).unwrap().try_into().unwrap();
    let expected = nssa::AccountId::for_private_pda(
        &fixture_program_id(),
        &PdaSeed::new(expected_slot_seed(5)),
        &NullifierPublicKey(npk_bytes),
        &fixture_vpk(),
        0,
    )
    .to_string();
    assert_eq!(
        stdout_of(&out),
        expected,
        "seed arg consumed, private identifier 0"
    );
    let err = stderr_of(&out);
    assert!(
        err.contains("seed arg named 'identifier'") && err.contains("identifier 0"),
        "stderr explains the shadowing: {err}"
    );
}
