//! The exported-transaction file of `send --export`, `spel sign` and `spel submit`: what it
//! accepts and what it refuses, without a sequencer.

use std::collections::BTreeMap;

use lee::public_transaction::Message;
use lee::{AccountId, FeeDeclaration, PrivateKey, PublicKey};
use lee_core::account::{Actor, Nonce};
use spel::exchange::{missing, TxFile, Witness, VERSION};

#[allow(clippy::unwrap_used)] // a fixed, valid key
fn key(byte: u8) -> PrivateKey {
    PrivateKey::try_new([byte; 32]).unwrap()
}

fn account(key: &PrivateKey) -> AccountId {
    AccountId::from(&PublicKey::new_from_private_key(key))
}

/// An increment of alice's counter, signed by alice and paid by `payer`.
fn message(alice: &PrivateKey, payer: &PrivateKey, amount: u8) -> Message {
    let program = AccountId::new([9; 32]);
    let root = Actor::new(account(alice), program);
    let nonces = BTreeMap::from([(account(alice), Nonce(3)), (account(payer), Nonce(7))]);
    Message::new(
        root,
        vec![0, amount, 0, 0, 0, 0, 0, 0, 0],
        [root],
        nonces,
        Some(FeeDeclaration::new(account(payer), 1000, 0, 5000)),
        Vec::new(),
    )
}

#[test]
fn a_file_collects_one_signature_per_signer_until_none_is_missing() {
    let (alice, payer) = (key(1), key(2));
    let message = message(&alice, &payer, 4);
    let mut file = TxFile::new(&message, "increment".into());
    assert_eq!(file.version, VERSION);
    assert_eq!(file.message().unwrap(), message);

    file.sign(&message, &payer);
    let verified = file.verified(&message).unwrap();
    assert_eq!(missing(&message, &verified), [account(&alice)]);
    let error = file.transaction().unwrap_err();
    assert!(error.contains(&format!("not signed by {}", account(&alice))), "{error}");

    file.sign(&message, &alice);
    let verified = file.verified(&message).unwrap();
    assert!(missing(&message, &verified).is_empty());
    let tx = file.transaction().unwrap();
    assert_eq!(tx.message(), &message);
    assert!(tx.witness_set().is_valid_for(&message));
}

#[test]
fn a_file_round_trips_through_json() {
    let (alice, payer) = (key(1), key(2));
    let message = message(&alice, &payer, 4);
    let mut file = TxFile::new(&message, "increment".into());
    file.sign(&message, &payer);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tx.json");
    let path = path.to_str().unwrap();
    file.save(path).unwrap();
    assert_eq!(TxFile::load(path).unwrap(), file);
}

#[test]
fn another_version_is_refused() {
    let (alice, payer) = (key(1), key(2));
    let mut file = TxFile::new(&message(&alice, &payer, 4), String::new());
    file.version = 1;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tx.json");
    let path = path.to_str().unwrap();
    file.save(path).unwrap();
    let error = TxFile::load(path).unwrap_err();
    assert!(error.contains("format version 1"), "{error}");
}

#[test]
fn a_signature_over_another_message_is_refused() {
    let (alice, payer) = (key(1), key(2));
    let signed = message(&alice, &payer, 4);
    let mut file = TxFile::new(&signed, String::new());
    file.sign(&signed, &payer);
    // The message swapped under the signature: another amount.
    let swapped = message(&alice, &payer, 5);
    file.message = TxFile::new(&swapped, String::new()).message;
    let error = file.verified(&swapped).unwrap_err();
    assert!(error.contains("doesn't verify"), "{error}");
    assert!(file.transaction().is_err());
}

#[test]
fn a_signature_by_a_non_signer_is_refused() {
    let (alice, payer, mallory) = (key(1), key(2), key(3));
    let message = message(&alice, &payer, 4);
    let mut file = TxFile::new(&message, String::new());
    file.sign(&message, &mallory);
    let error = file.verified(&message).unwrap_err();
    assert!(error.contains("isn't a signer"), "{error}");
}

#[test]
fn a_second_signature_by_one_signer_is_refused() {
    let (alice, payer) = (key(1), key(2));
    let message = message(&alice, &payer, 4);
    let mut file = TxFile::new(&message, String::new());
    file.sign(&message, &payer);
    file.sign(&message, &payer);
    let error = file.verified(&message).unwrap_err();
    assert!(error.contains("signed twice"), "{error}");
}

#[test]
fn a_witness_claiming_another_key_is_refused() {
    let (alice, payer) = (key(1), key(2));
    let message = message(&alice, &payer, 4);
    let mut file = TxFile::new(&message, String::new());
    file.sign(&message, &payer);
    // The payer's signature, presented as alice's.
    let Witness { signature, .. } = file.witnesses[0].clone();
    file.witnesses[0] = Witness {
        public_key: PublicKey::new_from_private_key(&alice).to_string(),
        signature,
    };
    let error = file.verified(&message).unwrap_err();
    assert!(error.contains("doesn't verify"), "{error}");
}

#[test]
fn a_message_that_is_not_canonically_encoded_is_refused() {
    let (alice, payer) = (key(1), key(2));
    let mut file = TxFile::new(&message(&alice, &payer, 4), String::new());
    file.message.push_str("00");
    assert!(file.message().is_err());
}
