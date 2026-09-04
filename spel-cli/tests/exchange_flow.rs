//! `spel sign` and `spel submit` failure paths, exercised through the real
//! binary. Every case here is rejected before the wallet is touched, so no
//! wallet home or sequencer is needed and the output is deterministic. The
//! happy path (export → sign → submit against a live sequencer) lives in
//! scripts/multisig-e2e-test.sh.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Output};

use nssa::public_transaction::Message;
use nssa::{AccountId, PrivateKey, PublicKey, Signature};
use spel::blob::{TxBlob, WitnessEntry};
use spel::hex::hex_encode;

/// A valid blob signed by `key`, listing its account as the only signer.
fn signed_blob() -> (TxBlob, PrivateKey) {
    let key = PrivateKey::try_new([1; 32]).unwrap();
    let pubkey = PublicKey::new_from_private_key(&key);
    let account_id = AccountId::from(&pubkey);
    let message = Message::try_new(
        [0; 8],
        vec![account_id],
        vec![1_u128.into()],
        vec![1, 2, 3, 4],
    )
    .unwrap();
    let bytes = borsh::to_vec(&message).unwrap();

    let id = format!("0x{}", hex_encode(account_id.value()));
    let signature = Signature::new(&key, &message.hash());
    let mut witnesses = BTreeMap::new();
    witnesses.insert(
        id.clone(),
        WitnessEntry {
            pubkey,
            signature: signature.to_string(),
        },
    );
    let blob = TxBlob {
        version: 1,
        summary: "test".to_string(),
        message_hex: hex_encode(&bytes),
        signers: vec![id],
        witnesses,
    };
    (blob, key)
}

fn run_spel(subcommand: &str, path: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_spel"))
        .arg(subcommand)
        .arg(path)
        .output()
        .expect("run spel")
}

#[test]
fn submit_rejects_blob_with_missing_witness() {
    // A second required signer with no witness. The present witness still
    // verifies, so the rejection is specifically about completeness.
    let (mut blob, _key) = signed_blob();
    blob.signers.push(format!("0x{}", "bb".repeat(32)));

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("blob.json");
    blob.save(path.to_str().unwrap()).unwrap();

    let output = run_spel("submit", &path);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success(), "submit must fail: {stderr}");
    assert!(
        stderr.contains("missing signers"),
        "rejection must name the cause: {stderr}"
    );
}

#[test]
fn submit_rejects_tampered_message() {
    // Swap in a different (still-decodable) message after signing. Its hash
    // no longer matches what the witness signed.
    let (mut blob, key) = signed_blob();
    let pubkey = PublicKey::new_from_private_key(&key);
    let account_id = AccountId::from(&pubkey);
    let tampered = Message::try_new(
        [0; 8],
        vec![account_id],
        vec![1_u128.into()],
        vec![9, 9, 9, 9],
    )
    .unwrap();
    blob.message_hex = hex_encode(&borsh::to_vec(&tampered).unwrap());

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("blob.json");
    blob.save(path.to_str().unwrap()).unwrap();

    let output = run_spel("submit", &path);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success(), "submit must fail: {stderr}");
    assert!(
        stderr.contains("does not verify against the message"),
        "rejection must name the cause: {stderr}"
    );
}

#[test]
fn sign_rejects_tampered_message() {
    // The same tampering must stop `spel sign` before it shows the prompt,
    // so a co-signer can never be asked to sign bytes nobody vouched for.
    let (mut blob, key) = signed_blob();
    let pubkey = PublicKey::new_from_private_key(&key);
    let account_id = AccountId::from(&pubkey);
    let tampered = Message::try_new(
        [0; 8],
        vec![account_id],
        vec![1_u128.into()],
        vec![9, 9, 9, 9],
    )
    .unwrap();
    blob.message_hex = hex_encode(&borsh::to_vec(&tampered).unwrap());

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("blob.json");
    blob.save(path.to_str().unwrap()).unwrap();

    let output = run_spel("sign", &path);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success(), "sign must fail: {stderr}");
    assert!(
        stderr.contains("does not verify against the message"),
        "rejection must name the cause: {stderr}"
    );
    assert!(
        !stdout.contains("Sign and update the file?"),
        "prompt must not be reached on a tampered blob:\n{stdout}"
    );
}

#[test]
fn submit_rejects_unreadable_blob() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("does-not-exist.json");

    let output = run_spel("submit", &path);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success(), "submit must fail: {stderr}");
    assert!(
        stderr.contains("cannot read blob file"),
        "rejection must name the cause: {stderr}"
    );
}
