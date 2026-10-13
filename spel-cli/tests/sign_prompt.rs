//! The co-signer prompt, exercised through the real binary. A fully
//! signed blob makes `spel sign` print the decoded section and return
//! before touching any wallet, so the output is deterministic.

use std::collections::BTreeMap;
use std::process::Command;

use nssa::public_transaction::Message;
use nssa::{AccountId, PrivateKey, PublicKey, Signature};
use spel::blob::{TxBlob, WitnessEntry};
use spel::hex::hex_encode;

#[test]
fn sign_prompt_decodes_operative_content_from_the_bytes() {
    let key = PrivateKey::try_new([1; 32]).unwrap();
    let pubkey = PublicKey::new_from_private_key(&key);
    let account_id = AccountId::from(&pubkey);
    let message = Message::new_preserialized(
        AccountId::new([0; 32]),
        vec![account_id],
        vec![1_u128.into()],
        vec![1, 2, 3, 4],
        None,
    );
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
        summary: "benign-looking summary".to_string(),
        message_hex: hex_encode(&bytes),
        signers: vec![id],
        witnesses,
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("blob.json");
    blob.save(path.to_str().unwrap()).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_spel"))
        .arg("sign")
        .arg(&path)
        .output()
        .expect("run spel sign");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "sign must exit clean on a fully signed blob:\n{stdout}"
    );
    assert!(
        stdout.contains("=== Decoded from message bytes ==="),
        "prompt lost its decode section:\n{stdout}"
    );
    // Instruction data is a borsh byte string since LEZ v0.2.5, so the payload
    // bytes 1,2,3,4 render as themselves rather than as risc0 u32 words. The
    // prompt renders the field exactly as the capture writer does.
    assert!(
        stdout.contains("Instruction data: 0x01020304"),
        "prompt must show the instruction data bytes:\n{stdout}"
    );
}
