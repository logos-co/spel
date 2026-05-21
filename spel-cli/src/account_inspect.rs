//! Account data inspection: fetch from sequencer, borsh-decode using IDL types,
//! and pretty-print as JSON.

use spel_framework_core::idl::SpelIdl;
use spel_framework_core::decode;
use std::process;

use crate::hex::{decode_bytes_32, hex_decode, hex_encode};

/// Inspect an on-chain account: fetch its data, borsh-decode it using the IDL
/// type definition, and print the result as JSON.
pub async fn inspect_account(
    account_id_str: &str,
    idl: &SpelIdl,
    type_name: &str,
    data_hex: Option<&str>,
) {
    // Parse account ID (base58 or hex)
    let account_bytes = decode_bytes_32(account_id_str).unwrap_or_else(|e| {
        eprintln!("Invalid account ID '{}': {}", account_id_str, e);
        process::exit(1);
    });
    let account_id = nssa::AccountId::new(account_bytes);

    // Get raw account data: from --data flag or from sequencer
    let data = if let Some(hex) = data_hex {
        hex_decode(hex).unwrap_or_else(|e| {
            eprintln!("Invalid --data hex: {}", e);
            process::exit(1);
        })
    } else {
        fetch_account_data(account_id).await
    };

    eprintln!("Account: {}", account_id);
    eprintln!("Data:    {} bytes", data.len());
    eprintln!("Hex:     {}", hex_encode(&data));
    eprintln!();

    if data.is_empty() {
        eprintln!("Account data is empty (account may not exist or has no data).");
        process::exit(1);
    }

    // Borsh decode via shared library
    match decode::decode_account_data(&data, type_name, idl) {
        Ok(value) => {
            println!("{}", serde_json::to_string_pretty(&value).unwrap());
        }
        Err(e) if e.contains("not found in IDL") => {
            eprintln!("Type '{}' not found in IDL.", type_name);
            eprintln!("Available account types:");
            for acc in &idl.accounts {
                eprintln!("  {}", acc.name);
            }
            process::exit(1);
        }
        Err(e) => {
            eprintln!("Borsh decode failed: {}", e);
            process::exit(1);
        }
    }
}

async fn fetch_account_data(account_id: nssa::AccountId) -> Vec<u8> {
    let wallet_core = wallet::WalletCore::from_env().unwrap_or_else(|e| {
        eprintln!("Failed to initialize wallet: {:?}", e);
        eprintln!("Set NSSA_WALLET_HOME_DIR or use --data <hex>");
        process::exit(1);
    });

    let account = wallet_core
        .get_account_public(account_id)
        .await
        .unwrap_or_else(|e| {
            eprintln!("Failed to fetch account {}: {:?}", account_id, e);
            process::exit(1);
        });

    account.data.to_vec()
}

