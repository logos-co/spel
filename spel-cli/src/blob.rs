//! Witness-exchange blob format v1 (multi-signature transaction exchange).
//!
//! Some instructions need signatures from accounts whose keys live on
//! different machines. Authorization is derived from the witness set over
//! the transaction message, so every signer must sign the same message
//! bytes, and those bytes already contain every signer's nonce. This module
//! defines a partial-transaction file that carries the message between
//! machines while signatures are collected.
//!
//! Flow:
//! 1. The exporting CLI builds the complete message (fetching nonces for
//!    all signers, including co-signers), signs with the keys its wallet
//!    holds, and writes the blob instead of submitting.
//! 2. Each co-signer runs `spel sign <blob>`. The CLI verifies the
//!    witnesses already present, shows the embedded summary for
//!    confirmation, appends a witness for every required signer whose key
//!    the local wallet holds, and writes the file back.
//! 3. Once no signer is missing, anyone runs `spel submit <blob>`.
//!
//! The file is JSON with these fields:
//! - `version`: format version, this module reads and writes 1
//! - `summary`: human-readable rendering of the transaction, produced by
//!   the exporting CLI from the built bytes
//! - `message_hex`: borsh-serialized nssa `Message`, hex. Signatures are
//!   made over exactly these bytes
//! - `signers`: account ids ("0x" plus hex) that must sign. The order
//!   matches the nonce order inside the message and dictates the witness
//!   order when the final transaction is assembled
//! - `witnesses`: map from account id to `{pubkey, signature}`, both hex
//!
//! A blob goes stale when any listed signer's on-chain nonce changes,
//! because the nonces inside `message_hex` no longer match. Signing or
//! submitting a stale blob fails, and the flow restarts with a fresh
//! export.
//!
//! Trust model v1: co-signers decide based on the embedded `summary`,
//! trusting the exporting CLI to have rendered it from the real bytes.
//! Independent decoding of `message_hex` against an IDL is a planned
//! upgrade.
//!
//! The format only depends on nssa types and borsh plus JSON encoding, so
//! any tool can produce or consume it.

use std::collections::BTreeMap;
use std::fs;

use nssa::public_transaction::{Message, WitnessSet};
use nssa::{AccountId, PublicKey, Signature};
use serde::{Deserialize, Serialize};

use crate::hex::{decode_bytes_32, hex_decode};

/// One collected signature: who signed (pubkey) and the signature itself.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WitnessEntry {
    /// Schnorr public key, hex. Validated as a curve point on parse.
    pub pubkey: PublicKey,
    /// Schnorr signature over the message bytes, hex (64 bytes).
    pub signature: String,
}

/// Partial transaction traveling between signers.
#[derive(Debug, Serialize, Deserialize)]
pub struct TxBlob {
    /// Format version. This code reads and writes version 1.
    pub version: u32,
    /// Human-readable summary rendered by the exporting CLI. Trust model v1:
    /// co-signers read this before signing.
    pub summary: String,
    /// Borsh-serialized nssa Message, hex. These exact bytes get signed.
    pub message_hex: String,
    /// Account ids ("0x" + hex) that must sign. ORDER MATTERS: it matches
    /// the nonce order inside the message and dictates witness order at submit.
    pub signers: Vec<String>,
    /// Collected witnesses, keyed by account id from `signers`.
    pub witnesses: BTreeMap<String, WitnessEntry>,
}

impl TxBlob {
    pub fn load(path: &str) -> Result<TxBlob, String> {
        let content = fs::read_to_string(path)
            .map_err(|e| format!("cannot read blob file '{}': {}", path, e))?;
        let blob = serde_json::from_str::<TxBlob>(&content)
            .map_err(|e| format!("invalid blob JSON in '{}': {}", path, e))?;
        if blob.version != 1 {
            return Err(format!(
                "unsupported blob version {}, this CLI supports version 1",
                blob.version
            ));
        }

        for key in blob.witnesses.keys() {
            if !blob.signers.contains(key) {
                return Err(format!("witness for '{}' is not in the signers list", key));
            }
        }

        Ok(blob)
    }

    pub fn save(&self, path: &str) -> Result<(), String> {
        let mut content = serde_json::to_string_pretty(self)
            .map_err(|e| format!("cannot convert to json: {}", e))?;
        content.push('\n');
        fs::write(path, &content).map_err(|e| format!("cannot write blob file '{}': {}", path, e))
    }

    /// Raw message bytes. Signatures are made over and verified against
    /// exactly these bytes.
    pub fn message_bytes(&self) -> Result<Vec<u8>, String> {
        hex_decode(&self.message_hex).map_err(|e| format!("blog message_hex: {}", e))
    }

    /// Decoded message, for display and for assembling the final transaction.
    pub fn message(&self) -> Result<Message, String> {
        let bytes = self.message_bytes()?;
        borsh::from_slice::<Message>(&bytes).map_err(|e| format!("cannot decode message: {}", e))
    }

    /// Signers that have no witness yet.
    pub fn missing_signers(&self) -> Vec<&String> {
        self.signers
            .iter()
            .filter(|s| !self.witnesses.contains_key(*s))
            .collect()
    }

    /// Check every collected witness: signature must verify over the stored
    /// message bytes, and pubkey must derive the claimed account id.
    pub fn verify_witnesses(&self) -> Result<(), String> {
        // v0.2.0: witnesses sign the 32-byte message hash, not the raw bytes.
        let message_hash = self.message()?.hash();
        for (account_id, entry) in &self.witnesses {
            let sig = entry.signature.parse::<Signature>().map_err(|e| {
                format!(
                    "witness for '{}' has malformed signature: {}",
                    account_id, e
                )
            })?;
            if !sig.is_valid_for(&message_hash, &entry.pubkey) {
                return Err(format!(
                    "witness for '{}' does not verify against the message \
                    (stale nonce or corrupted blob?)",
                    account_id
                ));
            }
            let claimed = decode_bytes_32(account_id)
                .map_err(|e| format!("invalid account id '{}': {}", account_id, e))?;
            let derived = AccountId::from(&entry.pubkey);
            if derived.value() != &claimed {
                return Err(format!(
                    "witness for '{}' is signed by a key that does not belong \
                    to that account",
                    account_id
                ));
            }
        }
        Ok(())
    }

    /// Assemble the final witness set IN SIGNERS ORDER, which matches the
    /// nonce order inside the message. Fails if any signer has no witness.
    pub fn witness_set(&self) -> Result<WitnessSet, String> {
        let mut witnesses_pairs: Vec<(Signature, PublicKey)> = Vec::new();
        for id in &self.signers {
            let entry = self
                .witnesses
                .get(id)
                .ok_or_else(|| format!("no witness for '{}'", id))?;
            let sig = entry
                .signature
                .parse::<Signature>()
                .map_err(|e| format!("witness for '{}' has malformed signature: {}", id, e))?;
            witnesses_pairs.push((sig, entry.pubkey.clone()));
        }

        Ok(WitnessSet::from_raw_parts(witnesses_pairs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nssa::PrivateKey;

    use crate::hex::hex_encode;

    fn witness_entry(message: &Message, key: &PrivateKey) -> WitnessEntry {
        let signature = Signature::new(key, &message.hash());

        let pubkey = PublicKey::new_from_private_key(key);
        WitnessEntry {
            pubkey,
            signature: signature.to_string(),
        }
    }

    /// A valid one-signer blob with a correct witness, plus the key that signed it.
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
        let mut witnesses = BTreeMap::new();
        witnesses.insert(id.clone(), witness_entry(&message, &key));
        let blob = TxBlob {
            version: 1,
            summary: "test".to_string(),
            message_hex: hex_encode(&bytes),
            signers: vec![id],
            witnesses,
        };
        (blob, key)
    }

    #[test]
    fn verify_accepts_valid_witness() {
        let (blob, _key) = signed_blob();
        assert!(blob.verify_witnesses().is_ok());
    }

    #[test]
    fn roundtrip_save_load() {
        let dir = tempfile::tempdir().unwrap();
        let path_buf = dir.path().join("b.json");
        let path = path_buf.to_str().unwrap();
        let (blob, _key) = signed_blob();
        blob.save(path).unwrap();
        let loaded = TxBlob::load(path).unwrap();

        assert_eq!(blob.version, loaded.version);
        assert_eq!(blob.summary, loaded.summary);
        assert_eq!(blob.message_hex, loaded.message_hex);
        assert_eq!(blob.signers, loaded.signers);
    }

    #[test]
    fn load_rejects_bad_version() {
        let dir = tempfile::tempdir().unwrap();
        let path_buf = dir.path().join("b.json");
        let path = path_buf.to_str().unwrap();
        let (mut blob, _key) = signed_blob();
        blob.version = 2;
        blob.save(path).unwrap();

        let err = TxBlob::load(path).unwrap_err();
        assert!(
            err.contains("unsupported blob version"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn load_rejects_unknown_witness() {
        let dir = tempfile::tempdir().unwrap();
        let path_buf = dir.path().join("b.json");
        let path = path_buf.to_str().unwrap();

        let (mut blob, _key) = signed_blob();

        let entry = blob.witnesses.values().next().unwrap();
        blob.witnesses
            .insert(format!("0x{}", "ee".repeat(32)), entry.clone());
        blob.save(path).unwrap();

        let err = TxBlob::load(path).unwrap_err();
        assert!(
            err.contains("not in the signers list"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn verify_rejects_tampered_message() {
        let (mut blob, key) = signed_blob();

        // Tamper: swap in a different (still-decodable) message. Its hash no
        // longer matches what the witness signed, so verification must reject it.
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

        let err = blob.verify_witnesses().unwrap_err();
        assert!(
            err.contains("does not verify against the message"),
            "unexpected error: {}",
            err
        )
    }

    #[test]
    fn verify_rejects_wrong_key() {
        let (mut blob, _key) = signed_blob();

        // Attacker: different key, but a perfectly VALID signature over the message.
        let attacker_key = PrivateKey::try_new([2; 32]).unwrap();
        let attacker_pubkey = PublicKey::new_from_private_key(&attacker_key);
        let signature = Signature::new(&attacker_key, &blob.message().unwrap().hash());

        // Claim it as the legitimate signer's witness.
        let victim_id = blob.signers[0].clone();
        blob.witnesses.insert(
            victim_id,
            WitnessEntry {
                pubkey: attacker_pubkey,
                signature: signature.to_string(),
            },
        );

        let err = blob.verify_witnesses().unwrap_err();
        assert!(err.contains("does not belong"), "unexpected error: {}", err);
    }

    #[test]
    fn missing_signers_lists_unsigned() {
        let (mut blob, _key) = signed_blob();

        // Fully signed blob: nothing missing.
        assert!(blob.missing_signers().is_empty());

        // Remove the witness: its signer must show up.
        blob.witnesses.clear();
        assert_eq!(blob.missing_signers(), vec![&blob.signers[0]]);
    }

    #[test]
    fn witness_set_assembles_in_signers_order() {
        let key = PrivateKey::try_new([1; 32]).unwrap();
        let pubkey = PublicKey::new_from_private_key(&key);

        // Two made-up ids with KNOWN sort order: "bb..." > "aa...".
        // signers lists bb first, the map iterates aa first — orders differ.
        let id_hi = format!("0x{}", "bb".repeat(32));
        let id_lo = format!("0x{}", "aa".repeat(32));

        let mut witnesses = BTreeMap::new();
        // Distinct fake signatures so order is observable in the output.
        witnesses.insert(
            id_hi.clone(),
            WitnessEntry {
                pubkey: pubkey.clone(),
                signature: "11".repeat(64),
            },
        );
        witnesses.insert(
            id_lo.clone(),
            WitnessEntry {
                pubkey,
                signature: "22".repeat(64),
            },
        );
        let blob = TxBlob {
            version: 1,
            summary: String::new(),
            message_hex: String::new(), // witness_set never reads it
            signers: vec![id_hi, id_lo],
            witnesses,
        };

        let ws = blob.witness_set().unwrap();
        let pairs = ws.signatures_and_public_keys();
        assert_eq!(pairs[0].0.to_string(), "11".repeat(64));
        assert_eq!(pairs[1].0.to_string(), "22".repeat(64));
    }

    #[test]
    fn witness_set_fails_on_missing_witness() {
        let key = PrivateKey::try_new([1; 32]).unwrap();
        let pubkey = PublicKey::new_from_private_key(&key);

        // Two made-up ids with KNOWN sort order: "bb..." > "aa...".
        // signers lists bb first, the map iterates aa first — orders differ.
        let id_hi = format!("0x{}", "bb".repeat(32));
        let id_lo = format!("0x{}", "aa".repeat(32));

        let mut witnesses = BTreeMap::new();
        // Distinct fake signatures so order is observable in the output.
        witnesses.insert(
            id_hi.clone(),
            WitnessEntry {
                pubkey: pubkey.clone(),
                signature: "11".repeat(64),
            },
        );

        let blob = TxBlob {
            version: 1,
            summary: String::new(),
            message_hex: String::new(), // witness_set never reads it
            signers: vec![id_hi, id_lo],
            witnesses,
        };

        let err = blob.witness_set().unwrap_err();
        assert!(err.contains("no witness for"), "unexpected error: {}", err)
    }
}
