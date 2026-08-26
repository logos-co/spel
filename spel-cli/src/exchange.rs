//! `spel sign` and `spel submit`: the CLI half of witness exchange

use std::io::Write;
use std::{io, process};

use common::transaction::LeeTransaction;
use nssa::public_transaction::WitnessSet;
use nssa::{AccountId, PublicKey, PublicTransaction, Signature};
use sequencer_service_rpc::RpcClient as _;
use wallet::WalletCore;

use crate::blob::{TxBlob, WitnessEntry};
use crate::hex::{decode_bytes_32, hex_encode};

/// Of the missing signer ids, those the key lookup answers for.
/// Fails on a id that does not decode.
fn detect_signable(
    missing: &[String],
    hold_keys: impl Fn(AccountId) -> bool,
) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for id_str in missing {
        let bytes = decode_bytes_32(id_str)
            .map_err(|e| format!("invalid signer id '{}': {}", id_str, e))?;
        if hold_keys(AccountId::new(bytes)) {
            out.push(id_str.clone());
        }
    }
    Ok(out)
}

/// Load a blob, verify it, show what it is, and append witnesses for
/// every required signer whose key the local wallet holds.
pub async fn sign_command(path: &str) {
    let mut blob = TxBlob::load(path).unwrap_or_else(|e| {
        eprintln!("❌ {}", e);
        process::exit(1);
    });

    // Refuse to touch a blob whose existing witnesses do not verify:
    // stale nonces, tampering, or corruption;
    blob.verify_witnesses().unwrap_or_else(|e| {
        eprintln!("❌ {}", e);
        process::exit(1);
    });

    let message = blob.message().unwrap_or_else(|e| {
        eprintln!("❌ {}", e);
        process::exit(1);
    });

    // What the exporter claims this transaction does.
    println!("=== Summary (embedded by exporter) ===");
    print!("{}", blob.summary);
    println!();

    // What the bytes actually say. Rendered locally from message_hex,
    // independent of the summary text.
    println!("=== Decoded from message bytes ===");
    let program_id_hex: String = message
        .program_id
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .map(|b| format!("{:02x}", b))
        .collect();
    println!("Program ID: {}", program_id_hex);
    let instruction_data_hex: String = message
        .instruction_data
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .map(|b| format!("{:02x}", b))
        .collect();
    println!("Instruction data: 0x{}", instruction_data_hex);
    println!("Accounts:");
    for id in &message.account_ids {
        println!("  0x{}", hex_encode(id.value()));
    }
    // Nonces pair with the signers list, not with the accounts above.
    println!("Signer nonces:");
    for (id, nonce) in blob.signers.iter().zip(message.nonces.iter()) {
        println!("  {} nonce={}", id, nonce.0);
    }
    println!();

    println!("Signers:");
    for id in &blob.signers {
        let status = if blob.witnesses.contains_key(id) {
            "✅ signed"
        } else {
            "⏳ missing"
        };
        println!("  {} {}", status, id);
    }

    println!();

    if blob.missing_signers().is_empty() {
        println!("All witnesses collected. Submit with: spel submit {}", path);
        return;
    }

    // Keys this wallet holds for the still-missing signers.
    let wallet_core = WalletCore::from_env().await.unwrap_or_else(|e| {
        eprintln!("❌ Failed to initialize wallet: {:?}", e);
        eprintln!("   Set NSSA_WALLET_HOME_DIR environment variable");
        process::exit(1);
    });

    let missing: Vec<String> = blob.missing_signers().into_iter().cloned().collect();
    let signable = detect_signable(&missing, |id| {
        wallet_core.get_account_public_signing_key(id).is_some()
    })
    .unwrap_or_else(|e| {
        eprintln!("❌ {}", e);
        process::exit(1);
    });

    if signable.is_empty() {
        eprintln!("❌ This wallet holds no key for any missing signer.");
        process::exit(1);
    }

    println!("This wallet can sign for:");
    for id in &signable {
        println!("  {}", id);
    }
    print!("Sign and update the file? [y/N] ");
    io::stdout().flush().unwrap();
    let mut answer = String::new();
    io::stdin().read_line(&mut answer).unwrap();
    if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
        println!("Aborted, nothing written.");
        process::exit(1);
    }

    // v0.2.0: sign the 32-byte message hash, matching WitnessSet::is_valid_for.
    let message_hash = blob
        .message()
        .unwrap_or_else(|e| {
            eprintln!("❌ {}", e);
            process::exit(1);
        })
        .hash();
    for id in signable {
        let bytes = decode_bytes_32(&id).unwrap();
        let key = wallet_core
            .get_account_public_signing_key(AccountId::new(bytes))
            .expect("key vanished between detection and signing");
        let signature = Signature::new(key, &message_hash);
        let pubkey = PublicKey::new_from_private_key(key);
        blob.witnesses.insert(
            id,
            WitnessEntry {
                pubkey,
                signature: signature.to_string(),
            },
        );
    }

    blob.save(path).unwrap_or_else(|e| {
        eprintln!("❌ {}", e);
        process::exit(1);
    });

    println!("✅ Witnesses added, file updated: {}", path);
    if blob.missing_signers().is_empty() {
        println!("All witnesses collected. Submit with: spel submit {}", path);
    } else {
        println!("Still missing:");
        for id in blob.missing_signers() {
            println!("  {}", id);
        }
        println!("Send the file to the remaining signers");
    }
}

pub async fn submit_command(path: &str) {
    let blob = TxBlob::load(path).unwrap_or_else(|e| {
        eprintln!("❌ {}", e);
        process::exit(1);
    });

    // Refuse to touch a blob whose existing witnesses do not verify:
    // stale nonces, tampering, or corruption;
    blob.verify_witnesses().unwrap_or_else(|e| {
        eprintln!("❌ {}", e);
        process::exit(1);
    });

    let message = blob.message().unwrap_or_else(|e| {
        eprintln!("❌ {}", e);
        process::exit(1);
    });

    if !blob.missing_signers().is_empty() {
        eprintln!("❌ missing signers. run: spel sign <file>");
        process::exit(1);
    }

    let witness_set = blob.witness_set().unwrap_or_else(|e| {
        eprintln!("❌ {}", e);
        process::exit(1);
    });
    let tx = PublicTransaction::new(message, witness_set);

    let wallet_core = WalletCore::from_env().await.unwrap_or_else(|e| {
        eprintln!("❌ Failed to initialize wallet: {:?}", e);
        eprintln!("   Set NSSA_WALLET_HOME_DIR environment variable");
        process::exit(1);
    });
    let tx_hash = wallet_core
        .helm_owned()
        .send_transaction(LeeTransaction::Public(tx))
        .await
        .unwrap_or_else(|e| {
            eprintln!("❌ Failed to submit transaction: {:?}", e);
            process::exit(1);
        });

    println!("📤 Transaction submitted!");
    println!("   tx_hash: {}", tx_hash);
    println!("   Waiting for confirmation...");

    let poller = wallet_core.poller_helm();

    match poller.poll_tx(tx_hash).await {
        Ok(_) => println!("✅ Transaction confirmed — included in a block."),
        Err(e) => {
            eprintln!("❌ Transaction NOT confirmed: {e:#}");
            process::exit(1);
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_signable_filters_by_held_keys() {
        let held = AccountId::new([0xaa; 32]);
        let missing = vec![
            format!("0x{}", "aa".repeat(32)),
            format!("0x{}", "bb".repeat(32)),
        ];

        let signable = detect_signable(&missing, |id| id == held).unwrap();

        assert_eq!(signable, vec![format!("0x{}", "aa".repeat(32))]);
    }

    #[test]
    fn detect_signable_rejects_undecodable_id() {
        let missing = vec!["0xzz".to_string()];

        let err = detect_signable(&missing, |_| true).unwrap_err();

        assert!(
            err.contains("invalid signer id"),
            "unexpected error: {}",
            err
        );
    }
}
