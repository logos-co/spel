//! Transaction building and submission.

use crate::cli::{snake_to_kebab, to_pascal_case};
use crate::hex::{decode_bytes_32, hex_encode, parse_account_id};
use crate::parse::{parse_value, ParsedValue};
use crate::pda::compute_pda_from_seeds;
use crate::serialize::serialize_to_risc0;
use common::transaction::NSSATransaction;
use hex;
use nssa::program::Program;
use nssa::public_transaction::{Message, WitnessSet};
use nssa::{AccountId, PublicTransaction};
use nssa_core::account::Nonce;
use nssa_core::program::ProgramId;
use sequencer_service_rpc::RpcClient as _;
use spel_framework_core::idl::{IdlInstruction, IdlSeed, SpelIdl};
use std::collections::HashMap;
use std::fs;
use std::process;
use wallet::WalletCore;

/// Execute an instruction: parse args, build TX, optionally submit.
pub async fn execute_instruction(
    idl: &SpelIdl,
    ix: &IdlInstruction,
    args: &HashMap<String, String>,
    program_path: Option<&str>,
    program_id_hex: Option<&str>,
    dry_run_format: Option<&str>,
    extra_bins: &HashMap<String, String>,
) {
    println!("📋 Instruction: {}", ix.name);
    println!();

    let mut args = args.clone();

    // Auto-fill program-id args from binary paths
    for (key, bin_path) in extra_bins {
        if !args.contains_key(key) {
            if let Ok(bytes) = fs::read(bin_path) {
                if let Ok(program) = Program::new(bytes) {
                    let id = program.id();
                    let id_str: Vec<String> = id.iter().map(|w| w.to_string()).collect();
                    let val = id_str.join(",");
                    println!("  ℹ️  Auto-filled --{} from {}", key, bin_path);
                    args.insert(key.clone(), val);
                }
            }
        }
    }

    // Validate required args
    let mut missing = vec![];
    for arg in &ix.args {
        let key = snake_to_kebab(&arg.name);
        if !args.contains_key(&key) {
            missing.push(format!("--{}", key));
        }
    }
    for acc in &ix.accounts {
        // rest accounts are variadic (0 or more) — never required
        if acc.pda.is_none() && !acc.rest {
            let key = snake_to_kebab(&acc.name);
            if !args.contains_key(&key) {
                missing.push(format!("--{}", key));
            }
        }
    }
    if !missing.is_empty() {
        eprintln!("❌ Missing required arguments: {}", missing.join(", "));
        process::exit(1);
    }

    // Parse instruction args
    let mut parsed_args: Vec<(&str, &spel_framework_core::idl::IdlType, ParsedValue)> = Vec::new();
    let mut has_errors = false;
    for arg in &ix.args {
        let key = snake_to_kebab(&arg.name);
        let raw = args.get(&key).unwrap();
        match parse_value(raw, &arg.type_) {
            Ok(val) => parsed_args.push((&arg.name, &arg.type_, val)),
            Err(e) => {
                eprintln!("❌ --{}: {}", key, e);
                has_errors = true;
            }
        }
    }

    // Parse non-PDA account IDs
    let mut parsed_accounts: Vec<(&str, Vec<u8>, bool)> = Vec::new();
    // rest accounts are variadic: each expands to 0 or more AccountIds
    let mut rest_accounts: Vec<(&str, Vec<(Vec<u8>, bool)>)> = Vec::new();
    for acc in &ix.accounts {
        if acc.pda.is_some() {
            continue;
        }
        if acc.rest {
            let key = snake_to_kebab(&acc.name);
            if !args.contains_key(&key) {
                continue;
            }
        }
        let key = snake_to_kebab(&acc.name);
        if acc.rest {
            // variadic: optional, comma-separated list of account IDs (0 entries is valid)
            let entries: Vec<(Vec<u8>, bool)> = if let Some(raw) = args.get(&key) {
                raw.split(',')
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .map(|s| match parse_account_id(s) {
                        Ok((bytes, is_priv)) => (bytes.to_vec(), is_priv),
                        Err(e) => {
                            eprintln!("❌ --{}: {}", key, e);
                            has_errors = true;
                            (vec![], false)
                        }
                    })
                    .collect()
            } else {
                vec![] // rest accounts are optional — 0 is valid
            };
            rest_accounts.push((&acc.name, entries));
        } else {
            let raw = args.get(&key).unwrap();
            match parse_account_id(raw) {
                Ok((bytes, is_priv)) => parsed_accounts.push((&acc.name, bytes.to_vec(), is_priv)),
                Err(e) => {
                    eprintln!("❌ --{}: {}", key, e);
                    has_errors = true;
                }
            }
        }
    }
    if has_errors {
        process::exit(1);
    }

    // Build risc0 serialized data
    let ix_index = idl
        .instructions
        .iter()
        .position(|i| i.name == ix.name)
        .unwrap_or(0);
    let risc0_args: Vec<_> = parsed_args.iter().map(|(_, ty, val)| (*ty, val)).collect();
    let instruction_data = serialize_to_risc0(ix_index as u32, &risc0_args);

    // ─── Resolve program_id (once) ────────────────────────────────
    let (program_id, program_obj): (ProgramId, Option<Program>) = if let Some(hex) = program_id_hex
    {
        let bytes = decode_bytes_32(hex).unwrap_or_else(|e| {
            eprintln!("❌ Invalid program ID '{}': {}", hex, e);
            process::exit(1);
        });
        let mut pid = [0u32; 8];
        for (i, chunk) in bytes.chunks(4).enumerate() {
            pid[i] = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        (pid, None)
    } else if let Some(path) = program_path {
        let program_bytecode = fs::read(path).unwrap_or_else(|e| {
            eprintln!("❌ Failed to read program binary '{}': {}", path, e);
            eprintln!("   Hint: pass --program <64-char-hex> to skip loading the binary");
            process::exit(1);
        });
        let program = Program::new(program_bytecode).unwrap_or_else(|e| {
            eprintln!("❌ Failed to load program: {:?}", e);
            process::exit(1);
        });
        let pid = program.id();
        (pid, Some(program))
    } else {
        eprintln!(
            "❌ No program specified. Use --program <name|hex|path> or configure in spel.toml."
        );
        process::exit(1);
    };

    let program_id_display = hex_encode(
        &program_id
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .collect::<Vec<u8>>(),
    );

    // Build account map for PDA resolution
    let mut account_map: HashMap<String, AccountId> = HashMap::new();
    for (name, bytes, _) in &parsed_accounts {
        let mut arr = [0u8; 32];
        arr.copy_from_slice(bytes);
        account_map.insert(name.to_string(), AccountId::new(arr));
    }
    // Note: rest accounts are variadic; store first entry (if any) for PDA seed resolution
    for (name, entries) in &rest_accounts {
        if let Some((first, _)) = entries.first() {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(first);
            account_map.insert(name.to_string(), AccountId::new(arr));
        }
    }

    // Resolve external account references needed by PDA seeds
    for acc in &ix.accounts {
        if let Some(pda) = &acc.pda {
            for seed in &pda.seeds {
                if let IdlSeed::Account { path } = seed {
                    if !account_map.contains_key(path) {
                        let key = snake_to_kebab(path);
                        if let Some(raw) = args.get(&key) {
                            match decode_bytes_32(raw) {
                                Ok(bytes) => {
                                    account_map.insert(path.clone(), AccountId::new(bytes));
                                }
                                Err(e) => {
                                    eprintln!("❌ --{}: {}", key, e);
                                    process::exit(1);
                                }
                            }
                        } else {
                            eprintln!(
                                "❌ PDA '{}' requires account '{}' — provide --{}",
                                acc.name, path, key
                            );
                            process::exit(1);
                        }
                    }
                }
            }
        }
    }

    let mut parsed_arg_map: HashMap<String, ParsedValue> = HashMap::new();
    for (name, _, val) in &parsed_args {
        parsed_arg_map.insert(name.to_string(), val.clone());
    }

    // Resolve PDA accounts
    for acc in &ix.accounts {
        if let Some(pda) = &acc.pda {
            match compute_pda_from_seeds(&pda.seeds, &program_id, &account_map, &parsed_arg_map) {
                Ok(id) => {
                    account_map.insert(acc.name.clone(), id);
                }
                Err(e) => {
                    eprintln!("❌ Failed to compute PDA for '{}': {}", acc.name, e);
                    process::exit(1);
                }
            }
        }
    }

    // ─── Dry-run summary ────────────────────────────────────────
    if let Some(fmt) = dry_run_format {
        // Build instruction data hex string
        let ix_data_hex: String = instruction_data
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .map(|b| format!("{:02x}", b))
            .collect();

        // Try to fetch nonces (non-fatal if wallet unavailable)
        let signer_names: Vec<&str> = ix
            .accounts
            .iter()
            .filter(|a| a.signer)
            .map(|a| a.name.as_str())
            .collect();
        let signer_nonces: HashMap<String, Option<u64>> = {
            let mut nonces_map = HashMap::new();
            if !signer_names.is_empty() {
                if let Ok(wallet_core) = WalletCore::from_env() {
                    let signer_ids: Vec<AccountId> = signer_names
                        .iter()
                        .filter_map(|n| account_map.get(*n).copied())
                        .collect();
                    match wallet_core.get_accounts_nonces(signer_ids.clone()).await {
                        Ok(nonces) => {
                            for (name, &Nonce(n)) in signer_names.iter().zip(nonces.iter()) {
                                nonces_map.insert(name.to_string(), Some(n as u64));
                            }
                        }
                        Err(_) => {
                            for name in &signer_names {
                                nonces_map.insert(name.to_string(), None);
                            }
                        }
                    }
                } else {
                    for name in &signer_names {
                        nonces_map.insert(name.to_string(), None);
                    }
                }
            }
            nonces_map
        };

        if fmt == "json" {
            // JSON output
            let mut accounts_json: Vec<String> = Vec::new();
            for acc in &ix.accounts {
                let id = account_map
                    .get(&acc.name)
                    .map(|a| format!("{}", a))
                    .unwrap_or_else(|| "(unresolved)".to_string());
                let mut flags: Vec<&str> = Vec::new();
                if acc.signer {
                    flags.push("signer");
                }
                if acc.writable {
                    flags.push("writable");
                }
                let flags_str = flags
                    .iter()
                    .map(|f| format!("\"{}\"", f))
                    .collect::<Vec<_>>()
                    .join(", ");

                if let Some(pda) = &acc.pda {
                    let seeds: Vec<String> = std::iter::once("\"program_id\"".to_string())
                        .chain(pda.seeds.iter().map(|s| match s {
                            IdlSeed::Const { value } => format!("\"{}\"", value),
                            IdlSeed::Account { path } => format!("\"Account({})\"", path),
                            IdlSeed::Arg { path } => format!("\"Arg({})\"", path),
                        }))
                        .collect();
                    accounts_json.push(format!(
                        "    {{\"name\": \"{}\", \"id\": \"{}\", \"flags\": [{}], \"is_pda\": true, \"seeds\": [{}]}}",
                        acc.name, id, flags_str, seeds.join(", ")
                    ));
                } else {
                    accounts_json.push(format!(
                        "    {{\"name\": \"{}\", \"id\": \"{}\", \"flags\": [{}]}}",
                        acc.name, id, flags_str
                    ));
                }
            }

            let mut args_json_entries: Vec<String> = Vec::new();
            for (name, _, val) in &parsed_args {
                let val_str = match val {
                    ParsedValue::U8(n) => format!("{}", n),
                    ParsedValue::U32(n) => format!("{}", n),
                    ParsedValue::U64(n) => format!("{}", n),
                    ParsedValue::U128(n) => format!("{}", n),
                    _ => format!("\"{}\"", val),
                };
                args_json_entries.push(format!("    \"{}\": {}", name, val_str));
            }

            let mut signers_json_entries: Vec<String> = Vec::new();
            for name in &signer_names {
                let nonce_str = match signer_nonces.get(*name) {
                    Some(Some(n)) => format!("{}", n),
                    _ => "null".to_string(),
                };
                signers_json_entries
                    .push(format!("    \"{}\": {{\"nonce\": {}}}", name, nonce_str));
            }

            println!("{{");
            println!("  \"program_id\": \"{}\",", program_id_display);
            println!("  \"accounts\": [");
            println!("{}", accounts_json.join(",\n"));
            println!("  ],");
            println!("  \"arguments\": {{");
            println!("{}", args_json_entries.join(",\n"));
            println!("  }},");
            println!("  \"instruction_data\": \"{}\",", ix_data_hex);
            println!("  \"signers\": {{");
            println!("{}", signers_json_entries.join(",\n"));
            println!("  }}");
            println!("}}");
        } else {
            // Text output
            println!("=== Dry Run ===");
            println!("Program ID: {}", program_id_display);
            println!("Accounts:");
            for acc in &ix.accounts {
                let id = account_map
                    .get(&acc.name)
                    .map(|a| format!("{}", a))
                    .unwrap_or_else(|| "(unresolved)".to_string());
                let mut flags: Vec<&str> = Vec::new();
                if acc.signer {
                    flags.push("signer");
                }
                if acc.writable {
                    flags.push("writable");
                }

                if let Some(pda) = &acc.pda {
                    let flags_str = if flags.is_empty() {
                        String::new()
                    } else {
                        format!("  [{}]", flags.join(", "))
                    };
                    println!("  PDA {} \u{2192} {}{}", acc.name, id, flags_str);
                    let seeds: Vec<String> = std::iter::once("program_id".to_string())
                        .chain(pda.seeds.iter().map(|s| match s {
                            IdlSeed::Const { value } => format!("\"{}\"", value),
                            IdlSeed::Account { path } => format!("Account({})", path),
                            IdlSeed::Arg { path } => format!("Arg({})", path),
                        }))
                        .collect();
                    println!("    seeds: [{}]", seeds.join(", "));
                } else {
                    let flags_str = if flags.is_empty() {
                        String::new()
                    } else {
                        format!("  [{}]", flags.join(", "))
                    };
                    println!("  {} \u{2192} {}{}", acc.name, id, flags_str);
                }
            }
            println!("Arguments:");
            for (name, _, val) in &parsed_args {
                println!("  --{} {}", snake_to_kebab(name), val);
            }
            println!("Instruction data: {}", ix_data_hex);
            if !signer_names.is_empty() {
                println!("Signers:");
                for name in &signer_names {
                    let nonce_str = match signer_nonces.get(*name) {
                        Some(Some(n)) => format!("nonce={}", n),
                        _ => "nonce=(unknown)".to_string(),
                    };
                    println!("  {}: {}", name, nonce_str);
                }
            }
            println!("================");
            println!("Dry run complete \u{2014} not submitted.");
        }
        return;
    }

    // ─── Normal display ─────────────────────────────────────────
    println!("Accounts:");
    for acc in &ix.accounts {
        if acc.pda.is_some() {
            let id = account_map
                .get(&acc.name)
                .map(|a| format!("{}", a))
                .unwrap_or_default();
            println!("  \u{1f4e6} {} \u{2192} {} (PDA)", acc.name, id);
        } else if acc.rest {
            if let Some((_, entries)) = rest_accounts.iter().find(|(n, _)| *n == acc.name) {
                if entries.is_empty() {
                    println!(
                        "  \u{1f4e6} {} \u{2192} (none \u{2014} variadic rest)",
                        acc.name
                    );
                } else {
                    for (e, _) in entries {
                        println!("  \u{1f4e6} {} \u{2192} 0x{}", acc.name, hex_encode(e));
                    }
                }
            }
        } else {
            let account_bytes = parsed_accounts
                .iter()
                .find(|(n, _, _)| *n == acc.name)
                .unwrap();
            println!(
                "  \u{1f4e6} {} \u{2192} 0x{}",
                acc.name,
                hex_encode(&account_bytes.1)
            );
        }
    }
    println!();
    println!("Arguments (parsed):");
    for (name, _, val) in &parsed_args {
        println!("  {} = {}", name, val);
    }
    println!();
    println!("\u{1f527} Transaction:");
    println!("  Program ID: {}", program_id_display);
    println!("  instruction index: {}", ix_index);
    println!("  instruction: {} {{", to_pascal_case(&ix.name));
    for (name, _, val) in &parsed_args {
        println!("    {}: {},", name, val);
    }
    println!("  }}");
    println!();
    println!(
        "  Serialized instruction data ({} u32 words):",
        instruction_data.len()
    );
    let hex_words: Vec<String> = instruction_data
        .iter()
        .map(|w| format!("{:08x}", w))
        .collect();
    println!("    [{}]", hex_words.join(", "));
    println!();

    // ─── Transaction submission ──────────────────────────────────
    println!("\u{1f4e4} Submitting transaction...");
    println!("  Program ID: {:?}", program_id);

    let wallet_core = WalletCore::from_env().unwrap_or_else(|e| {
        eprintln!("❌ Failed to initialize wallet: {:?}", e);
        eprintln!("   Set NSSA_WALLET_HOME_DIR environment variable");
        process::exit(1);
    });

    // Check if any account has a Private/ prefix
    let has_private = parsed_accounts.iter().any(|(_, _, is_priv)| *is_priv)
        || rest_accounts
            .iter()
            .any(|(_, entries)| entries.iter().any(|(_, is_priv)| *is_priv));

    if has_private {
        // ─── Privacy-preserving transaction ──────────────────
        use nssa::privacy_preserving_transaction::circuit::ProgramWithDependencies;
        use wallet::PrivacyPreservingAccount;

        let program = program_obj.unwrap_or_else(|| {
            eprintln!(
                "❌ Privacy-preserving transactions require the program binary (not --program-id)"
            );
            process::exit(1);
        });

        // Build dependencies from extra_bins
        let mut dependencies = HashMap::new();
        for (_, bin_path) in extra_bins {
            if let Ok(bytes) = fs::read(bin_path) {
                if let Ok(dep_program) = Program::new(bytes) {
                    dependencies.insert(dep_program.id(), dep_program);
                }
            }
        }
        let program_with_deps = ProgramWithDependencies::new(program, dependencies);

        // Build privacy-preserving account list
        let mut pp_accounts: Vec<PrivacyPreservingAccount> = Vec::new();
        for acc in &ix.accounts {
            if acc.rest {
                if let Some((_, entries)) = rest_accounts.iter().find(|(n, _)| *n == acc.name) {
                    for (bytes, is_priv) in entries {
                        let mut arr = [0u8; 32];
                        arr.copy_from_slice(bytes);
                        let account_id = AccountId::new(arr);
                        if *is_priv {
                            pp_accounts.push(PrivacyPreservingAccount::PrivateOwned(account_id));
                        } else {
                            pp_accounts.push(PrivacyPreservingAccount::Public(account_id));
                        }
                    }
                }
            } else if let Some((_, _, is_priv)) =
                parsed_accounts.iter().find(|(n, _, _)| *n == acc.name)
            {
                let id = *account_map.get(&acc.name).unwrap_or_else(|| {
                    eprintln!("❌ Account '{}' not resolved", acc.name);
                    process::exit(1);
                });
                if *is_priv {
                    pp_accounts.push(PrivacyPreservingAccount::PrivateOwned(id));
                } else {
                    pp_accounts.push(PrivacyPreservingAccount::Public(id));
                }
            } else {
                // PDA account — always public
                let id = *account_map.get(&acc.name).unwrap_or_else(|| {
                    eprintln!("❌ Account '{}' not resolved", acc.name);
                    process::exit(1);
                });
                pp_accounts.push(PrivacyPreservingAccount::Public(id));
            }
        }

        let (response, _shared_secrets) = wallet_core
            .send_privacy_preserving_tx(pp_accounts, instruction_data, &program_with_deps)
            .await
            .unwrap_or_else(|e| {
                eprintln!(
                    "❌ Failed to submit privacy-preserving transaction: {:?}",
                    e
                );
                process::exit(1);
            });

        println!("📤 Privacy-preserving transaction submitted!");
        println!("   tx_hash: {}", hex::encode(response.0));
        println!("   Waiting for confirmation...");

        let poller = wallet::poller::TxPoller::new(
            wallet_core.config(),
            wallet_core.sequencer_client.clone(),
        );

        match poller.poll_tx(response).await {
            Ok(_) => println!("✅ Transaction confirmed — included in a block."),
            Err(e) => {
                eprintln!("❌ Transaction NOT confirmed: {e:#}");
                process::exit(1);
            }
        }
    } else {
        // ─── Public transaction (existing path) ──────────────
        let mut account_ids: Vec<AccountId> = Vec::new();
        for acc in &ix.accounts {
            if acc.rest {
                if let Some((_, entries)) = rest_accounts.iter().find(|(n, _)| *n == acc.name) {
                    for (bytes, _) in entries {
                        let mut arr = [0u8; 32];
                        arr.copy_from_slice(bytes);
                        account_ids.push(AccountId::new(arr));
                    }
                }
            } else {
                let id = account_map.get(&acc.name).unwrap_or_else(|| {
                    eprintln!("❌ Account '{}' not resolved", acc.name);
                    process::exit(1);
                });
                account_ids.push(*id);
            }
        }

        let signer_accounts: Vec<AccountId> = ix
            .accounts
            .iter()
            .filter(|a| a.signer)
            .map(|a| *account_map.get(&a.name).unwrap())
            .collect();

        let nonces = if signer_accounts.is_empty() {
            vec![]
        } else {
            wallet_core
                .get_accounts_nonces(signer_accounts.clone())
                .await
                .unwrap_or_else(|e| {
                    eprintln!("❌ Failed to fetch nonces: {:?}", e);
                    process::exit(1);
                })
        };

        let signing_keys: Vec<_> = signer_accounts
            .iter()
            .map(|id| {
                wallet_core
                    .storage()
                    .user_data
                    .get_pub_account_signing_key(*id)
                    .unwrap_or_else(|| {
                        eprintln!("❌ Signing key not found for account {}", id);
                        process::exit(1);
                    })
            })
            .collect();

        let message = Message::new_preserialized(program_id, account_ids, nonces, instruction_data);
        let witness_set = WitnessSet::for_message(&message, &signing_keys);
        let tx = PublicTransaction::new(message, witness_set);

        let tx_hash = wallet_core
            .sequencer_client
            .send_transaction(NSSATransaction::Public(tx))
            .await
            .unwrap_or_else(|e| {
                eprintln!("❌ Failed to submit transaction: {:?}", e);
                process::exit(1);
            });

        println!("📤 Transaction submitted!");
        println!("   tx_hash: {}", tx_hash);
        println!("   Waiting for confirmation...");

        let poller = wallet::poller::TxPoller::new(
            wallet_core.config(),
            wallet_core.sequencer_client.clone(),
        );

        match poller.poll_tx(tx_hash).await {
            Ok(_) => println!("✅ Transaction confirmed — included in a block."),
            Err(e) => {
                eprintln!("❌ Transaction NOT confirmed: {e:#}");
                process::exit(1);
            }
        }
    }
}
