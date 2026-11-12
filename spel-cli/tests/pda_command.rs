//! `spel pda` regression tests against the real binary: public PDAs from raw seeds.

use lee_core::account::AccountId;
use lee_core::program::PdaSeed;
use sha2::{Digest, Sha256};
use std::process::Command;

const PROGRAM_ADDRESS_HEX: &str =
    "abababababababababababababababababababababababababababababababab";

fn spel_in_empty_dir(args: &[&str]) -> std::process::Output {
    let dir = tempfile::tempdir().unwrap();
    Command::new(env!("CARGO_BIN_EXE_spel"))
        .current_dir(dir.path())
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

fn program_id() -> AccountId {
    let bytes = hex::decode(PROGRAM_ADDRESS_HEX).expect("valid hex");
    AccountId::new(bytes.try_into().expect("32 bytes"))
}

/// A string seed, zero-padded to 32 bytes as the CLI does.
fn padded(seed: &str) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    bytes[..seed.len()].copy_from_slice(seed.as_bytes());
    bytes
}

#[test]
fn one_seed_is_used_as_is() {
    let out = spel_in_empty_dir(&["--program", PROGRAM_ADDRESS_HEX, "pda", "vault"]);
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let expected = AccountId::for_public_pda(&program_id(), &PdaSeed::new(padded("vault")));
    assert_eq!(stdout_of(&out), expected.to_string());
}

#[test]
fn several_seeds_are_hashed_together() {
    let user = "cd".repeat(32);
    let out = spel_in_empty_dir(&["--program", PROGRAM_ADDRESS_HEX, "pda", "vault", &user]);
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));

    let mut hasher = Sha256::new();
    hasher.update(padded("vault"));
    hasher.update([0xcdu8; 32]);
    let combined: [u8; 32] = hasher.finalize().into();
    let expected = AccountId::for_public_pda(&program_id(), &PdaSeed::new(combined));
    assert_eq!(stdout_of(&out), expected.to_string());
}

#[test]
fn a_seed_longer_than_32_bytes_is_refused() {
    let out = spel_in_empty_dir(&["--program", PROGRAM_ADDRESS_HEX, "pda", &"x".repeat(33)]);
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("max 32"),
        "stderr: {}",
        stderr_of(&out)
    );
}
