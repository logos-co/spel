//! Account data inspection: fetch from sequencer, borsh-decode using IDL types,
//! and pretty-print as JSON.

use sequencer_service_rpc::{RpcClient as _, SequencerClient, SequencerClientBuilder};
use serde::Deserialize;
use spel_framework_core::decode;
use spel_framework_core::idl::SpelIdl;
use std::process;
use wallet::config::SequencerConnectionData;

use crate::hex::{decode_bytes_32, hex_decode, hex_encode};

/// Inspect an on-chain account: fetch its data, borsh-decode it using the IDL
/// type definition, and print the result as JSON.
///
/// `sequencer` is the `--sequencer <URL>` flag. Without it the sequencers come
/// from `wallet_config.json` in the wallet home; nothing else in the wallet
/// home is read.
pub async fn inspect_account(
    account_id_str: &str,
    idl: &SpelIdl,
    type_name: &str,
    data_hex: Option<&str>,
    sequencer: Option<&str>,
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
        fetch_account_data(account_id, sequencer).await
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
        },
        Err(e) if e.contains("not found in IDL") => {
            eprintln!("Type '{}' not found in IDL.", type_name);
            eprintln!("Available account types:");
            for acc in &idl.accounts {
                eprintln!("  {}", acc.name);
            }
            process::exit(1);
        },
        Err(e) => {
            eprintln!("Borsh decode failed: {}", e);
            process::exit(1);
        },
    }
}

/// The only part of `wallet_config.json` a public read needs. Deserialised on
/// its own so a config written by a newer wallet still parses here.
#[derive(Deserialize)]
struct SequencerList {
    sequencers: Vec<SequencerConnectionData>,
}

/// Where to fetch public accounts from: the `--sequencer` flag, else every
/// sequencer listed in the wallet home's `wallet_config.json`.
///
/// This deliberately never constructs a `WalletCore`: that loads and validates
/// `storage.json`, which a public read has no use for and which fails outright
/// when the storage was written by a wallet build with a different schema.
fn sequencers(flag: Option<&str>) -> Vec<SequencerConnectionData> {
    if let Some(url) = flag {
        let sequencer_addr = url.parse().unwrap_or_else(|e| {
            eprintln!("Invalid --sequencer URL '{}': {}", url, e);
            eprintln!("Include the scheme, e.g. https://testnet.lez.logos.co");
            process::exit(1);
        });
        return vec![SequencerConnectionData {
            sequencer_addr,
            basic_auth: None,
        }];
    }

    let config_path = wallet::helperfunctions::fetch_config_path().unwrap_or_else(|e| {
        eprintln!("Cannot locate the wallet config: {:?}", e);
        eprintln!("Pass --sequencer <URL>, set LEE_WALLET_HOME_DIR, or use --data <hex>");
        process::exit(1);
    });
    let config = std::fs::read_to_string(&config_path).unwrap_or_else(|e| {
        eprintln!("Cannot read {}: {}", config_path.display(), e);
        eprintln!("Pass --sequencer <URL>, point LEE_WALLET_HOME_DIR at a wallet home with a wallet_config.json, or use --data <hex>");
        process::exit(1);
    });
    let list: SequencerList = serde_json::from_str(&config).unwrap_or_else(|e| {
        eprintln!("Cannot parse {}: {}", config_path.display(), e);
        eprintln!("Pass --sequencer <URL> or use --data <hex>");
        process::exit(1);
    });
    if list.sequencers.is_empty() {
        eprintln!("{} lists no sequencers", config_path.display());
        eprintln!("Pass --sequencer <URL> or use --data <hex>");
        process::exit(1);
    }
    list.sequencers
}

/// A JSON-RPC client for one sequencer, with the same basic-auth header the
/// wallet sends.
fn sequencer_client(conn: &SequencerConnectionData) -> Result<SequencerClient, String> {
    let mut builder = SequencerClientBuilder::default();
    if let Some(basic_auth) = &conn.basic_auth {
        let name = "Authorization".parse().map_err(|e| format!("{e}"))?;
        let value = format!("Basic {basic_auth}")
            .parse()
            .map_err(|e| format!("invalid basic auth: {e}"))?;
        builder = builder.set_headers(std::iter::once((name, value)).collect());
    }
    builder
        .build(&conn.sequencer_addr)
        .map_err(|e| format!("cannot create client: {e}"))
}

/// Fetch the raw data of a public account from the first sequencer that
/// answers.
async fn fetch_account_data(account_id: nssa::AccountId, sequencer: Option<&str>) -> Vec<u8> {
    let mut failures = Vec::new();
    for conn in sequencers(sequencer) {
        let client = match sequencer_client(&conn) {
            Ok(client) => client,
            Err(e) => {
                failures.push(format!("{}: {}", conn.sequencer_addr, e));
                continue;
            },
        };
        match client.get_account(account_id).await {
            Ok(account) => return account.data.into_inner(),
            Err(e) => failures.push(format!("{}: {}", conn.sequencer_addr, e)),
        }
    }

    eprintln!("Failed to fetch account {}:", account_id);
    for failure in &failures {
        eprintln!("  {}", failure);
    }
    process::exit(1);
}
