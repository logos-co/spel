//! Transaction building and submission.

use std::collections::HashMap;
use std::fs;
use std::process;
use nssa::program::Program;
use nssa::public_transaction::{Message, WitnessSet};
use nssa::{AccountId, PublicTransaction};
use nssa_core::program::ProgramId;
use nssa_core::account::Nonce;
use spel_framework_core::idl::{IdlSeed, SpelIdl, IdlInstruction};
use crate::hex::{hex_encode, decode_bytes_32, parse_account_id};
use crate::parse::{parse_value, ParsedValue};
use crate::serialize::serialize_to_risc0;
use crate::pda::compute_pda_from_seeds;
use crate::cli::{snake_to_kebab, to_pascal_case};
use common::transaction::NSSATransaction;
use hex;
use sequencer_service_rpc::RpcClient as _;
use serde_json::{json, Value};
use wallet::WalletCore;

/// Format PDA seeds into a display string for human-readable output.
/// E.g. `[program_id, "owner", Account(vault)]`
fn format_pda_seeds(seeds: &[IdlSeed]) -> String {
    let parts: Vec<String> = std::iter::once("program_id".to_string())
        .chain(seeds.iter().map(|s| match s {
            IdlSeed::Const { value } => format!("\"{}\"", value),
            IdlSeed::Account { path } => format!("Account({})", path),
            IdlSeed::Arg { path } => format!("Arg({})", path),
        }))
        .collect();
    format!("[{}]", parts.join(", "))
}

/// Execute an instruction: parse args, build TX, optionally submit.
///
/// `dry_run`:
///   - `None`             — submit to the sequencer
///   - `Some("text")`     — resolve & print human-readable summary, do not submit
///   - `Some("json")`     — resolve & emit JSON to stdout, do not submit
pub async fn execute_instruction(
    idl: &SpelIdl,
    ix: &IdlInstruction,
    args: &HashMap<String, String>,
    program_path: Option<&str>,
    program_id_hex: Option<&str>,
    dry_run: Option<&str>,
    extra_bins: &HashMap<String, String>,
) {
    // In JSON dry-run mode, suppress all human-readable preamble — only emit JSON to stdout.
    let json_mode = dry_run == Some("json");
    macro_rules! info { ($($arg:tt)*) => { if !json_mode { println!($($arg)*); } } }

    info!("📋 Instruction: {}", ix.name);
    info!("");

    let mut args = args.clone();

    // Auto-fill program-id args from binary paths
    for (key, bin_path) in extra_bins {
        if !args.contains_key(key) {
            if let Ok(bytes) = fs::read(bin_path) {
                if let Ok(program) = Program::new(bytes) {
                    let id = program.id();
                    let id_str: Vec<String> = id.iter().map(|w| w.to_string()).collect();
                    let val = id_str.join(",");
                    info!("  ℹ️  Auto-filled --{} from {}", key, bin_path);
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
    let instruction_data = serialize_to_risc0(ix_index as u32, &risc0_args)
        .unwrap_or_else(|e| {
            eprintln!("❌ Serialization error: {}", e);
            process::exit(1);
        });

    // ─── Resolve program_id (load binary if needed) ─────────────
    let (program_id, program_obj): (ProgramId, Option<Program>) = if let Some(hex) = program_id_hex {
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
            eprintln!("   Hint: pass --program <64-char-hex> to skip loading the binary.");
            eprintln!("   Or configure in spel.toml.");
            process::exit(1);
        });
        let program = Program::new(program_bytecode).unwrap_or_else(|e| {
            eprintln!("❌ Failed to load program: {:?}", e);
            process::exit(1);
        });
        let pid = program.id();
        (pid, Some(program))
    } else {
        eprintln!("❌ No program specified. Use --program <name|hex|path> or configure in spel.toml.");
        process::exit(1);
    };
    let program_id_hex_str: String = program_id
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .map(|b| format!("{:02x}", b))
        .collect();

    // ─── Build account map and resolve PDAs ─────────────────────
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
                                    info!("  ℹ️  Using --{} for PDA seed '{}'", key, path);
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

    // Compute PDAs
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
    if let Some(fmt) = dry_run {
        let signer_names: Vec<&str> = ix.accounts.iter().filter(|a| a.signer).map(|a| a.name.as_str()).collect();
        let signer_ids: Vec<AccountId> = signer_names.iter().filter_map(|n| account_map.get(*n).copied()).collect();

        // Best-effort nonce fetch — graceful degradation if wallet unavailable.
        let signer_nonces: Vec<Option<Nonce>> = if signer_ids.is_empty() {
            vec![]
        } else {
            match WalletCore::from_env() {
                Ok(wc) => match wc.get_accounts_nonces(signer_ids.clone()).await {
                    Ok(ns) => ns.into_iter().map(Some).collect(),
                    Err(_) => vec![None; signer_ids.len()],
                },
                Err(_) => vec![None; signer_ids.len()],
            }
        };

        if fmt == "json" {
            print_dry_run_json(
                &program_id_hex_str,
                ix,
                &account_map,
                &parsed_accounts,
                &rest_accounts,
                &parsed_args,
                &instruction_data,
                &signer_names,
                &signer_nonces,
            );
        } else {
            print_dry_run_text(
                &program_id_hex_str,
                ix,
                &account_map,
                &parsed_accounts,
                &rest_accounts,
                &parsed_args,
                &instruction_data,
                &signer_names,
                &signer_nonces,
            );
        }
        return;
    }

    // ─── Pre-submission display ─────────────────────────────────
    info!("Accounts:");
    for acc in &ix.accounts {
        if let Some(pda) = &acc.pda {
            let id = account_map.get(&acc.name).map(|a| format!("{}", a)).unwrap_or_default();
            info!("  📦 {} → {} (PDA)", acc.name, id);
            info!("    seeds: {}", format_pda_seeds(&pda.seeds));
        } else if acc.rest {
            if let Some((_, entries)) = rest_accounts.iter().find(|(n, _)| *n == acc.name) {
                if entries.is_empty() {
                    info!("  📦 {} → (none — variadic rest)", acc.name);
                } else {
                    for (e, _) in entries {
                        info!("  📦 {} → 0x{}", acc.name, hex_encode(e));
                    }
                }
            }
        } else {
            let account_bytes = parsed_accounts.iter().find(|(n, _, _)| *n == acc.name).unwrap();
            info!("  📦 {} → 0x{}", acc.name, hex_encode(&account_bytes.1));
        }
    }
    info!("");
    info!("Arguments (parsed):");
    for (name, _, val) in &parsed_args {
        info!("  {} = {}", name, val);
    }
    info!("");
    info!("🔧 Transaction:");
    info!("  program-id: {}", program_id_hex_str);
    if program_id_hex.is_none() {
        if let Some(path) = program_path {
            info!("  program:    {}", path);
        }
    }
    info!("  instruction index: {}", ix_index);
    info!("  instruction: {} {{", to_pascal_case(&ix.name));
    for (name, _, val) in &parsed_args {
        info!("    {}: {},", name, val);
    }
    info!("  }}");
    info!("");
    info!("  Serialized instruction data ({} u32 words):", instruction_data.len());
    let hex_words: Vec<String> = instruction_data.iter().map(|w| format!("{:08x}", w)).collect();
    info!("    [{}]", hex_words.join(", "));
    info!("");

    // ─── Transaction submission ─────────────────────────────────
    info!("📤 Submitting transaction...");

    let wallet_core = WalletCore::from_env().unwrap_or_else(|e| {
        eprintln!("❌ Failed to initialize wallet: {:?}", e);
        eprintln!("   Set NSSA_WALLET_HOME_DIR environment variable");
        process::exit(1);
    });

    // Check if any account has a Private/ prefix
    let has_private = parsed_accounts.iter().any(|(_, _, is_priv)| *is_priv)
        || rest_accounts.iter().any(|(_, entries)| entries.iter().any(|(_, is_priv)| *is_priv));

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

        info!("📤 Privacy-preserving transaction submitted!");
        info!("   tx_hash: {}", hex::encode(response.0));
        info!("   Waiting for confirmation...");

        let poller = wallet::poller::TxPoller::new(
            wallet_core.config(),
            wallet_core.sequencer_client.clone(),
        );

        match poller.poll_tx(response).await {
            Ok(_) => info!("✅ Transaction confirmed — included in a block."),
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

        let signer_accounts: Vec<AccountId> = ix.accounts.iter()
            .filter(|a| a.signer)
            .map(|a| *account_map.get(&a.name).unwrap())
            .collect();

        let nonces = if signer_accounts.is_empty() {
            vec![]
        } else {
            wallet_core.get_accounts_nonces(signer_accounts.clone()).await.unwrap_or_else(|e| {
                eprintln!("❌ Failed to fetch nonces: {:?}", e);
                process::exit(1);
            })
        };

        let signing_keys: Vec<_> = signer_accounts.iter().map(|id| {
            wallet_core.storage().user_data.get_pub_account_signing_key(*id).unwrap_or_else(|| {
                eprintln!("❌ Signing key not found for account {}", id);
                process::exit(1);
            })
        }).collect();

        let message = Message::new_preserialized(program_id, account_ids, nonces, instruction_data);
        let witness_set = WitnessSet::for_message(&message, &signing_keys);
        let tx = PublicTransaction::new(message, witness_set);

        let tx_hash = wallet_core.sequencer_client.send_transaction(NSSATransaction::Public(tx)).await.unwrap_or_else(|e| {
            eprintln!("❌ Failed to submit transaction: {:?}", e);
            process::exit(1);
        });

        info!("📤 Transaction submitted!");
        info!("   tx_hash: {}", tx_hash);
        info!("   Waiting for confirmation...");

        let poller = wallet::poller::TxPoller::new(
            wallet_core.config(),
            wallet_core.sequencer_client.clone(),
        );

        match poller.poll_tx(tx_hash).await {
            Ok(_) => info!("✅ Transaction confirmed — included in a block."),
            Err(e) => {
                eprintln!("❌ Transaction NOT confirmed: {e:#}");
                process::exit(1);
            }
        }
    }
}

/// Render an account list entry as JSON, including PDA seed metadata when applicable.
fn account_to_json(
    acc: &spel_framework_core::idl::IdlAccountItem,
    id_str: String,
) -> Value {
    let mut flags: Vec<&str> = Vec::new();
    if acc.signer { flags.push("signer"); }
    if acc.writable { flags.push("writable"); }

    let mut obj = json!({
        "name": acc.name,
        "id": id_str,
        "flags": flags,
    });
    if let Some(pda) = &acc.pda {
        let seeds: Vec<Value> = pda.seeds.iter().map(|s| match s {
            IdlSeed::Const { value } => json!({"kind": "const", "value": value}),
            IdlSeed::Account { path } => json!({"kind": "account", "path": path}),
            IdlSeed::Arg { path } => json!({"kind": "arg", "path": path}),
        }).collect();
        obj["is_pda"] = json!(true);
        obj["seeds"] = json!(seeds);
    }
    obj
}

#[allow(clippy::too_many_arguments)]
fn print_dry_run_json(
    program_id_hex: &str,
    ix: &IdlInstruction,
    account_map: &HashMap<String, AccountId>,
    parsed_accounts: &[(&str, Vec<u8>, bool)],
    rest_accounts: &[(&str, Vec<(Vec<u8>, bool)>)],
    parsed_args: &[(&str, &spel_framework_core::idl::IdlType, ParsedValue)],
    instruction_data: &[u32],
    signer_names: &[&str],
    signer_nonces: &[Option<Nonce>],
) {
    // accounts: include all (PDA, named, and per-entry rest accounts).
    let mut accounts_json: Vec<Value> = Vec::new();
    for acc in &ix.accounts {
        if acc.rest {
            if let Some((_, entries)) = rest_accounts.iter().find(|(n, _)| *n == acc.name) {
                if entries.is_empty() {
                    accounts_json.push(json!({
                        "name": acc.name,
                        "id": null,
                        "flags": ["rest"],
                        "is_rest": true,
                    }));
                } else {
                    for (bytes, _) in entries {
                        accounts_json.push(json!({
                            "name": acc.name,
                            "id": format!("0x{}", hex_encode(bytes)),
                            "flags": ["rest"],
                            "is_rest": true,
                        }));
                    }
                }
            }
        } else {
            let id_str = account_map
                .get(&acc.name)
                .map(|a| {
                    if acc.pda.is_some() {
                        format!("{}", a)
                    } else if let Some((_, b, _)) = parsed_accounts.iter().find(|(n, _, _)| *n == acc.name) {
                        format!("0x{}", hex_encode(b))
                    } else {
                        format!("{}", a)
                    }
                })
                .unwrap_or_else(|| "(unresolved)".to_string());
            accounts_json.push(account_to_json(acc, id_str));
        }
    }

    let args_json: serde_json::Map<String, Value> = parsed_args.iter().map(|(name, _, val)| {
        let v = match val {
            ParsedValue::U8(n) => json!(n),
            ParsedValue::U32(n) => json!(n),
            ParsedValue::U64(n) => json!(n),
            // u128 is not natively representable in JSON numbers — encode as decimal string.
            ParsedValue::U128(n) => json!(n.to_string()),
            other => json!(other.to_string()),
        };
        (name.to_string(), v)
    }).collect();

    let signers_json: serde_json::Map<String, Value> = signer_names.iter().enumerate().map(|(i, name)| {
        let nonce_val = signer_nonces.get(i).and_then(|n| n.as_ref())
            // u128 nonce — encode as decimal string to avoid silent truncation.
            .map(|n| json!(n.0.to_string()))
            .unwrap_or(Value::Null);
        (name.to_string(), json!({"nonce": nonce_val}))
    }).collect();

    let ix_data_hex: String = instruction_data
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .map(|b| format!("{:02x}", b))
        .collect();

    let summary = json!({
        "program_id": program_id_hex,
        "instruction": ix.name,
        "accounts": accounts_json,
        "arguments": args_json,
        "instruction_data": ix_data_hex,
        "signers": signers_json,
    });
    println!("{}", serde_json::to_string_pretty(&summary).unwrap());
}

#[allow(clippy::too_many_arguments)]
fn print_dry_run_text(
    program_id_hex: &str,
    ix: &IdlInstruction,
    account_map: &HashMap<String, AccountId>,
    parsed_accounts: &[(&str, Vec<u8>, bool)],
    rest_accounts: &[(&str, Vec<(Vec<u8>, bool)>)],
    parsed_args: &[(&str, &spel_framework_core::idl::IdlType, ParsedValue)],
    instruction_data: &[u32],
    signer_names: &[&str],
    signer_nonces: &[Option<Nonce>],
) {
    println!("=== Dry Run ===");
    println!("Program ID: {}", program_id_hex);
    println!("Instruction: {}", ix.name);
    println!();
    println!("Accounts:");
    for acc in &ix.accounts {
        let mut flags: Vec<&str> = Vec::new();
        if acc.signer { flags.push("signer"); }
        if acc.writable { flags.push("writable"); }
        let flags_str = if flags.is_empty() { String::new() } else { format!(" [{}]", flags.join(", ")) };

        if let Some(pda) = &acc.pda {
            let id = account_map.get(&acc.name).map(|a| format!("{}", a)).unwrap_or_else(|| "(unresolved)".into());
            println!("  PDA {} → {}{}", acc.name, id, flags_str);
            println!("    seeds: {}", format_pda_seeds(&pda.seeds));
        } else if acc.rest {
            if let Some((_, entries)) = rest_accounts.iter().find(|(n, _)| *n == acc.name) {
                if entries.is_empty() {
                    println!("  {} → (none — variadic rest){}", acc.name, flags_str);
                } else {
                    for (bytes, _) in entries {
                        println!("  {} → 0x{}{}", acc.name, hex_encode(bytes), flags_str);
                    }
                }
            }
        } else if let Some((_, b, _)) = parsed_accounts.iter().find(|(n, _, _)| *n == acc.name) {
            println!("  {} → 0x{}{}", acc.name, hex_encode(b), flags_str);
        }
    }
    println!();
    println!("Arguments:");
    for (name, _, val) in parsed_args {
        println!("  --{} {}", snake_to_kebab(name), val);
    }
    println!();
    let ix_data_hex: String = instruction_data
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .map(|b| format!("{:02x}", b))
        .collect();
    println!("Instruction data: 0x{}", ix_data_hex);
    if !signer_names.is_empty() {
        println!();
        println!("Signers:");
        for (i, name) in signer_names.iter().enumerate() {
            let nonce_str = match signer_nonces.get(i).and_then(|n| n.as_ref()) {
                Some(n) => format!("nonce={}", n.0),
                None => "nonce=(unknown)".to_string(),
            };
            println!("  {}: {}", name, nonce_str);
        }
    }
    println!("================");
    println!("Dry run complete — not submitted.");
}
