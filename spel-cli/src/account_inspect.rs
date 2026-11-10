//! Actor state inspection: Borsh-decode an actor's state with the IDL and pretty-print it as
//! JSON.

use spel_framework_core::decode;
use spel_framework_core::idl::SpelIdl;
use std::process;

use crate::hex::{decode_bytes_32, hex_decode, hex_encode};

pub const USAGE: &str = "Usage: spel inspect <ACCOUNT-ID> --idl <IDL> --data <HEX> [--type <TYPE>]";

/// Inspect an actor's state: Borsh-decode it as `type_name`, or as the IDL's state type when
/// none is given, and print the result as JSON.
pub async fn inspect_account(
    account_id_str: &str,
    idl: &SpelIdl,
    type_name: Option<&str>,
    data_hex: Option<&str>,
) {
    // --data decodes offline, whatever the account.
    let data = if let Some(hex) = data_hex {
        hex_decode(hex).unwrap_or_else(|e| {
            eprintln!("Invalid --data hex: {}", e);
            process::exit(1);
        })
    } else {
        let account_bytes = decode_bytes_32(account_id_str).unwrap_or_else(|e| {
            eprintln!("Invalid account ID '{}': {}", account_id_str, e);
            process::exit(1);
        });
        fetch_account_data(lee::AccountId::new(account_bytes)).await
    };

    eprintln!("Data:    {} bytes", data.len());
    eprintln!("Hex:     {}", hex_encode(&data));
    eprintln!();

    if data.is_empty() {
        eprintln!("Account data is empty (account may not exist or has no data).");
        process::exit(1);
    }

    let decoded = match type_name {
        Some(name) => decode::decode_named(&data, name, idl),
        None => decode::decode_state(&data, idl),
    };
    match decoded {
        Ok(value) => {
            println!("{}", serde_json::to_string_pretty(&value).unwrap());
        },
        Err(e) => {
            eprintln!("Borsh decode failed: {}", e);
            if let Some(name) = type_name.filter(|name| idl.type_def(name).is_none()) {
                let names: Vec<&str> = idl.types.iter().map(|t| t.name.as_str()).collect();
                eprintln!("'{name}' is not in the IDL. Types: {}", names.join(", "));
            }
            process::exit(1);
        },
    }
}

// An account holds one state per program under LEZ's actor model, so fetching needs the
// program as well; `spel inspect ACCOUNT@PROGRAM` lands with the actor-model port.
async fn fetch_account_data(account_id: lee::AccountId) -> Vec<u8> {
    eprintln!(
        "Fetching {account_id} from the sequencer is being ported to LEZ's actor model, where an"
    );
    eprintln!("account holds one state per program. Decode offline with --data <hex> meanwhile.");
    process::exit(1);
}
