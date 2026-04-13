//! Transaction building and submission.

use std::collections::HashMap;
use std::fs;
use std::process;
use nssa::program::Program;
use nssa::public_transaction::{Message, WitnessSet};
use nssa::{AccountId, PublicTransaction};
use nssa_core::program::ProgramId;
use spel_framework_core::idl::{IdlSeed, SpelIdl, IdlInstruction};
use crate::hex::{hex_encode, decode_bytes_32, parse_account_id};
use crate::parse::{parse_value, ParsedValue};
use crate::serialize::serialize_to_risc0;
use crate::pda::compute_pda_from_seeds;
use crate::cli::{snake_to_kebab, to_pascal_case};
use common::transaction::NSSATransaction;
use hex;
use sequencer_service_rpc::RpcClient as _;
use wallet::WalletCore;


/// Format PDA seeds into a display string for human-readable output.
/// E.g. `[program_id, "owner", Account(vault)]`
fn format_pda_seeds(seeds: &[IdlSeed]) -> String {
    let parts: std::vec::Vec<String> = std::iter::once("program_id".to_string())
        .chain(seeds.iter().map(|s| match s {
            IdlSeed::Const { value } => format!(""{}"", value),
            IdlSeed::Account { path } => format!("Account({})", path),
            IdlSeed::Arg { path } => format!("Arg({})", path),
        }))
        .collect();
    format!("[{}]", parts.join(", "))
}

/// Format PDA seeds into a JSON array for machine-readable output.
fn pda_seeds_json(seeds: &[IdlSeed]) -> serde_json::Value {
    let mut arr: Vec<serde_json::Value> = vec![serde_json::json!({"type": "program_id"})];
    for s in seeds {
        arr.push(match s {
            IdlSeed::Const { value } => serde_json::json!({"type": "const", "value": value}),
            IdlSeed::Account { path } => serde_json::json!({"type": "account", "name": path}),
            IdlSeed::Arg { path } => serde_json::json!({"type": "arg", "name": path}),
        });
    }
    serde_json::Value::Array(arr)
}

/// Execute an instruction: parse args, build TX, optionally submit.
pub async fn execute_instruction(
    idl: &SpelIdl,
    ix: &IdlInstruction,
    args: &HashMap<String, String>,
    program_path: &str,
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
            Err(e) => { eprintln!("❌ --{}: {}", key, e); has_errors = true; }
        }
    }

    // Parse non-PDA account IDs
    let mut parsed_accounts: Vec<(&str, Vec<u8>, bool)> = Vec::new();
    // rest accounts are variadic: each expands to 0 or more AccountIds
    let mut rest_accounts: Vec<(&str, Vec<(Vec<u8>, bool)>)> = Vec::new();
    for acc in &ix.accounts {
        if acc.pda.is_some() { continue; }
        if acc.rest { let key = snake_to_kebab(&acc.name); if !args.contains_key(&key) { continue; } }
        let key = snake_to_kebab(&acc.name);
        if acc.rest {
            // variadic: optional, comma-separated list of account IDs (0 entries is valid)
            let entries: Vec<(Vec<u8>, bool)> = if let Some(raw) = args.get(&key) {
                raw.split(',')
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .map(|s| {
                        match parse_account_id(s) {
                            Ok((bytes, is_priv)) => (bytes.to_vec(), is_priv),
                            Err(e) => { eprintln!("❌ --{}: {}", key, e); has_errors = true; (vec![], false) }
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
                Err(e) => { eprintln!("❌ --{}: {}", key, e); has_errors = true; }
            }
        }
    }
    if has_errors { process::exit(1); }

    // Build risc0 serialized data
    let ix_index = idl.instructions.iter().position(|i| i.name == ix.name).unwrap_or(0);
    let risc0_args: Vec<_> = parsed_args.iter().map(|(_, ty, val)| (*ty, val)).collect();
    let instruction_data = serialize_to_risc0(ix_index as u32, &risc0_args);

    // ─── Step 4: Load program binary or resolve program ID ───
    let (program_id, _program_obj): (ProgramId, Option<Program>) = if let Some(hex) = program_id_hex {
        let bytes = decode_bytes_32(hex).unwrap_or_else(|e| {
            eprintln!("❌ Invalid --program-id '{}': {}", hex, e);
            process::exit(1);
        });
        let mut pid = [0u32; 8];
        for (i, chunk) in bytes.chunks(4).enumerate() {
            pid[i] = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        (pid, None)
    } else if !program_path.is_empty() && std::path::Path::new(program_path).exists() {
        let program_bytecode = fs::read(program_path).unwrap_or_else(|e| {
            eprintln!("❌ Failed to read program binary '{}': {}", program_path, e);
            eprintln!("   Hint: pass --program-id <hex> to skip loading the binary");
            process::exit(1);
        });
        let program = Program::new(program_bytecode).unwrap_or_else(|e| {
            eprintln!("❌ Failed to load program: {:?}", e);
            process::exit(1);
        });
        let pid = program.id();
        (pid, Some(program))
    } else {
        eprintln!("❌ Program binary or --program-id required. Pass --program <path> or --program-id <hex>.");
        process::exit(1);
    };

    // ─── Step 5: Build account map ───
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
                                    println!("  ℹ️  Using --{} for PDA seed '{}'", key, path);
                                    account_map.insert(path.clone(), AccountId::new(bytes));
                                }
                                Err(e) => { eprintln!("❌ --{}: {}", key, e); process::exit(1); }
                            }
                        } else {
                            eprintln!("❌ PDA '{}' requires account '{}' — provide --{}", acc.name, path, key);
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

    // ─── Step 6: Resolve PDA accounts ───
    for acc in &ix.accounts {
        if let Some(pda) = &acc.pda {
            match compute_pda_from_seeds(&pda.seeds, &program_id, &account_map, &parsed_arg_map) {
                Ok(id) => {
                    let seed_str = format_pda_seeds(&pda.seeds);
                    println!("  PDA {} → {}", acc.name, id);
                    println!("    seeds: {}", seed_str);
                    account_map.insert(acc.name.clone(), id);
                }
                Err(e) => {
                    eprintln!("❌ Failed to compute PDA for '{}': {}", acc.name, e);
                    process::exit(1);
                }
            }
        }
    }

    // Check if any account has a Private/ prefix
    let has_private = parsed_accounts.iter().any(|(_, _, is_priv)| *is_priv)
        || rest_accounts.iter().any(|(_, entries)| entries.iter().any(|(_, is_priv)| *is_priv));

    // ─── Step 7: Fetch nonces for signer accounts ───
    let signer_accounts: Vec<(String, AccountId)> = ix.accounts.iter()
        .filter(|a| a.signer)
        .map(|a| {
            let id = *account_map.get(&a.name).unwrap_or_else(|| {
                eprintln!("❌ Signer account '{}' not resolved", a.name);
                process::exit(1);
            });
            (a.name.clone(), id)
        })
        .collect();

    let nonces: Vec<Option<u64>> = if signer_accounts.is_empty() {
        vec![]
    } else {
        let wallet_core = WalletCore::from_env().unwrap_or_else(|e| {
            eprintln!("❌ Failed to initialize wallet: {:?}", e);
            eprintln!("   Set NSSA_WALLET_HOME_DIR environment variable");
            process::exit(1);
        });
        let signer_ids: Vec<AccountId> = signer_accounts.iter().map(|(_, id)| *id).collect();
        match wallet_core.get_accounts_nonces(signer_ids).await {
            Ok(ns) => ns.into_iter().map(Some).collect(),
            Err(e) => {
                eprintln!("⚠️  Could not fetch nonces: {:?}", e);
                signer_accounts.iter().map(|_| None).collect()
            }
        }
    };

    // ─── Step 8 & 9: Print transaction summary (and JSON if requested) ───
    let program_id_hex_str = {
        let parts: Vec<String> = program_id.iter().map(|w| format!("{:08x}", w)).collect();
        parts.join("")
    };


    // Build account list for display and JSON
    let mut accounts_summary: Vec<String> = Vec::new();
    let mut accounts_json: Vec<serde_json::Value> = Vec::new();
    let mut raw_account_ids: Vec<String> = Vec::new();

    for acc in &ix.accounts {
        if acc.pda.is_some() {
            let id = account_map.get(&acc.name).unwrap();
            let id_str = format!("{}", id);
            let seed_str = format_pda_seeds(&acc.pda.as_ref().unwrap().seeds);
            accounts_summary.push(format!("  {} → {}", acc.name, id_str));
            accounts_summary.push(format!("    seeds: {}", seed_str));
            raw_account_ids.push(id_str.clone());
            accounts_json.push(serde_json::json!({
                "name": acc.name,
                "address": id_str,
                "pda": id_str,
                "pda_seeds": pda_seeds_json(&acc.pda.as_ref().unwrap().seeds),
                "signer": acc.signer,
                "writable": acc.writable,
                "rest": acc.rest,
            }));
        } else if acc.rest {
            if let Some((_, entries)) = rest_accounts.iter().find(|(n, _)| *n == acc.name) {
                if entries.is_empty() {
                    accounts_summary.push(format!("  {} → (none — variadic rest)", acc.name));
                } else {
                    for (e, _) in entries {
                        let id_str = format!("0x{}", hex_encode(e));
                        accounts_summary.push(format!("  {} → {}", acc.name, id_str));
                        raw_account_ids.push(id_str.clone());
                    }
                }
            }
        } else {
            let (_, bytes, _) = parsed_accounts.iter().find(|(n, _, _)| *n == acc.name).unwrap();
            let id_str = format!("0x{}", hex_encode(bytes));
            let mut flags = vec![];
            if acc.signer { flags.push("signer"); }
            if acc.writable { flags.push("writable"); }
            accounts_summary.push(format!("  {} → {}  [{}]", acc.name, id_str, flags.join(", ")));
            raw_account_ids.push(id_str.clone());
            accounts_json.push(serde_json::json!({
                "name": acc.name,
                "address": id_str,
                "pda": null,
                "signer": acc.signer,
                "writable": acc.writable,
                "rest": acc.rest,
            }));
        }
    }

    // Arguments for display and JSON
    let mut args_summary: Vec<String> = Vec::new();
    let mut args_json: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    for (name, _, val) in &parsed_args {
        args_summary.push(format!("  {}: {}", name, val));
        args_json.insert(name.to_string(), serde_json::Value::String(val.to_string()));
    }

    // Signers for display and JSON
    let mut signers_summary: Vec<String> = Vec::new();
    let mut signers_json: Vec<serde_json::Value> = Vec::new();
    for (i, (name, id)) in signer_accounts.iter().enumerate() {
        let nonce_str = match nonces.get(i) {
            Some(Some(n)) => format!("nonce={}", n),
            _ => "nonce=(unknown)".to_string(),
        };
        signers_summary.push(format!("  {}: {}", name, nonce_str));
        signers_json.push(serde_json::json!({
            "name": name,
            "address": format!("{}", id),
            "nonce": nonces.get(i).and_then(|n| *n),
        }));
    }

    // Instruction data words
    let hex_words: Vec<String> = instruction_data.iter().map(|w| format!("{:08x}", w)).collect();
    let instruction_data_hex = hex_words.join("");

    // JSON output
    if let Some(fmt) = dry_run_format {
        if fmt == "json" {
        let json_obj = serde_json::json!({
            "dry_run": true,
            "program_id": program_id_hex_str,
            "instruction_index": ix_index,
            "instruction_name": ix.name,
            "accounts": accounts_json,
            "arguments": args_json,
            "instruction_data_hex": instruction_data_hex,
            "instruction_data_words": instruction_data,
            "signers": signers_json,
            "raw_account_ids": raw_account_ids,
        });
        println!("{}", serde_json::to_string_pretty(&json_obj).unwrap());
        println!();
        }
    }

    // Human-readable summary
    println!("━━━ Transaction Summary ━━━━━━━━━━━━━━━━━━━━━━━");
    println!("🔷 Program ID: {}  (derived from binary)", program_id_hex_str);
    println!("📋 Instruction index: {}  ({})", ix_index, ix.name);
    println!();
    println!("📦 Accounts:");
    for line in &accounts_summary {
        println!("{}", line);
    }
    println!();
    println!("Parsed Arguments:");
    for line in &args_summary {
        println!("{}", line);
    }
    println!();
    println!("🔧 Serialized instruction data ({} u32 words):", instruction_data.len());
    println!("    [{}]", hex_words.join(", "));
    println!();
    if signer_accounts.is_empty() {
        println!("🔑 Signers: (none)");
    } else {
        println!("🔑 Signers and Nonces:");
        for line in &signers_summary {
            println!("{}", line);
        }
    }
    println!();
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!();
    println!("⚠️  Dry run — this is what WOULD be submitted.");
    println!("   Remove --dry-run to send this transaction.");

    // ─── Step 10: Early return for dry-run ───
    if dry_run_format.is_some() {
        return;
    }

    // ─── Transaction submission ──────────────────────────────────
    println!();
    println!("📤 Submitting transaction...");
    println!("  Program ID: {:?}", program_id);

    // Reload program for submission (program_obj was consumed earlier; re-read if needed)
    let program_obj = if program_id_hex.is_some() {
        None
    } else {
        let program_bytecode = fs::read(program_path).unwrap_or_else(|e| {
            eprintln!("❌ Failed to read program binary '{}': {}", program_path, e);
            process::exit(1);
        });
        Some(Program::new(program_bytecode).unwrap_or_else(|e| {
            eprintln!("❌ Failed to load program: {:?}", e);
            process::exit(1);
        }))
    };

    let wallet_core = WalletCore::from_env().unwrap_or_else(|e| {
        eprintln!("❌ Failed to initialize wallet: {:?}", e);
        eprintln!("   Set NSSA_WALLET_HOME_DIR environment variable");
        process::exit(1);
    });

    // has_private already checked above (before Step 8)

    if has_private {
        // ─── Privacy-preserving transaction ──────────────────
        use wallet::PrivacyPreservingAccount;
        use nssa::privacy_preserving_transaction::circuit::ProgramWithDependencies;

        let program = program_obj.unwrap_or_else(|| {
            eprintln!("❌ Privacy-preserving transactions require the program binary (not --program-id)");
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
            } else if let Some((_, _, is_priv)) = parsed_accounts.iter().find(|(n, _, _)| *n == acc.name) {
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

        let (response, _shared_secrets) = wallet_core.send_privacy_preserving_tx(
            pp_accounts,
            instruction_data,
            &program_with_deps,
        ).await.unwrap_or_else(|e| {
            eprintln!("❌ Failed to submit privacy-preserving transaction: {:?}", e);
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

        let signer_ids: Vec<AccountId> = signer_accounts.iter().map(|(_, id)| *id).collect();

        // Re-fetch nonces for submission (already fetched above, but need to get them again for the actual submission path)
        // We already have nonces from the dry-run fetch, re-use them
        let nonces_for_submit: Vec<u64> = nonces.iter().map(|n| n.unwrap_or_else(|| {
            eprintln!("❌ Nonce unknown for signer — cannot submit. Run --dry-run to see nonces.");
            process::exit(1);
        })).collect();

        let signing_keys: Vec<_> = signer_ids.iter().map(|id| {
            wallet_core.storage().user_data.get_pub_account_signing_key(*id).unwrap_or_else(|| {
                eprintln!("❌ Signing key not found for account {}", id);
                process::exit(1);
            })
        }).collect();

        let message = Message::new_preserialized(program_id, account_ids, nonces_for_submit, instruction_data);
        let witness_set = WitnessSet::for_message(&message, &signing_keys);
        let tx = PublicTransaction::new(message, witness_set);

        let tx_hash = wallet_core.sequencer_client.send_transaction(NSSATransaction::Public(tx)).await.unwrap_or_else(|e| {
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