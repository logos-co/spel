//! Transaction building and submission.

use std::collections::HashMap;
use std::fs;
use std::process;
use nssa::program::Program;
use nssa::public_transaction::{Message, WitnessSet};
use nssa::{AccountId, PublicTransaction};
use nssa_core::program::ProgramId;
use spel_framework_core::idl::{IdlSeed, SpelIdl, IdlInstruction};
use crate::hex::hex_encode;
use crate::parse::{parse_value, ParsedValue};
use crate::serialize::serialize_to_risc0;
use crate::pda::compute_pda_from_seeds;
use crate::cli::{snake_to_kebab, to_pascal_case};
use common::transaction::NSSATransaction;
use sequencer_service_rpc::RpcClient as _;
use wallet::WalletCore;

/// Execute an instruction: parse args, build TX, optionally submit.
pub async fn execute_instruction(
    idl: &SpelIdl,
    ix: &IdlInstruction,
    args: &HashMap<String, String>,
    program_path: &str,
    program_id_hex: Option<&str>,
    dry_run: bool,
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
    let mut parsed_accounts: Vec<(&str, Vec<u8>)> = Vec::new();
    // rest accounts are variadic: each expands to 0 or more AccountIds
    let mut rest_accounts: Vec<(&str, Vec<Vec<u8>>)> = Vec::new();
    for acc in &ix.accounts {
        if acc.pda.is_some() { continue; }
        if acc.rest { let key = snake_to_kebab(&acc.name); if !args.contains_key(&key) { continue; } }
        let key = snake_to_kebab(&acc.name);
        if acc.rest {
            // variadic: optional, comma-separated list of account IDs (0 entries is valid)
            let entries: Vec<Vec<u8>> = if let Some(raw) = args.get(&key) {
                raw.split(',')
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .map(|s| {
                        match decode_bytes_32(s) {
                            Ok(bytes) => bytes.to_vec(),
                            Err(e) => { eprintln!("❌ --{}: {}", key, e); has_errors = true; vec![] }
                        }
                    })
                    .collect()
            } else {
                vec![] // rest accounts are optional — 0 is valid
            };
            rest_accounts.push((&acc.name, entries));
        } else {
            let raw = args.get(&key).unwrap();
            match decode_bytes_32(raw) {
                Ok(bytes) => parsed_accounts.push((&acc.name, bytes.to_vec())),
                Err(e) => { eprintln!("❌ --{}: {}", key, e); has_errors = true; }
            }
        }
    }
    if has_errors { process::exit(1); }

    // Build risc0 serialized data
    let ix_index = idl.instructions.iter().position(|i| i.name == ix.name).unwrap_or(0);
    let risc0_args: Vec<_> = parsed_args.iter().map(|(_, ty, val)| (*ty, val)).collect();
    let instruction_data = serialize_to_risc0(ix_index as u32, &risc0_args);

    // Display
    println!("Accounts:");
    for acc in &ix.accounts {
        if acc.pda.is_some() {
            println!("  📦 {} → auto-computed (PDA)", acc.name);
        } else if acc.rest {
            if let Some((_, entries)) = rest_accounts.iter().find(|(n, _)| *n == acc.name) {
                if entries.is_empty() {
                    println!("  📦 {} → (none — variadic rest)", acc.name);
                } else {
                    for e in entries {
                        println!("  📦 {} → 0x{}", acc.name, hex_encode(e));
                    }
                }
            }
        } else {
            let account_bytes = parsed_accounts.iter().find(|(n, _)| *n == acc.name).unwrap();
            println!("  📦 {} → 0x{}", acc.name, hex_encode(&account_bytes.1));
        }
    }
    println!();
    println!("Arguments (parsed):");
    for (name, _, val) in &parsed_args {
        println!("  {} = {}", name, val);
    }
    println!();
    println!("🔧 Transaction:");
    if let Some(pid) = program_id_hex {
        println!("  program-id: {}", pid);
    } else {
        println!("  program: {}", program_path);
    }
    println!("  instruction index: {}", ix_index);
    println!("  instruction: {} {{", to_pascal_case(&ix.name));
    for (name, _, val) in &parsed_args {
        println!("    {}: {},", name, val);
    }
    println!("  }}");
    println!();
    println!("  Serialized instruction data ({} u32 words):", instruction_data.len());
    let hex_words: Vec<String> = instruction_data.iter().map(|w| format!("{:08x}", w)).collect();
    println!("    [{}]", hex_words.join(", "));
    println!();

    if dry_run {
        println!("⚠️  Dry run — omit --dry-run to submit the transaction.");
        return;
    }

    // ─── Transaction submission ──────────────────────────────────
    println!("📤 Submitting transaction...");

    // Resolve program_id: from --program-id hex flag, or by loading the binary
    use crate::hex::decode_bytes_32;
    let program_id: ProgramId = if let Some(hex) = program_id_hex {
        let bytes = decode_bytes_32(hex).unwrap_or_else(|e| {
            eprintln!("❌ Invalid --program-id '{}': {}", hex, e);
            process::exit(1);
        });
        let mut pid = [0u32; 8];
        for (i, chunk) in bytes.chunks(4).enumerate() {
            pid[i] = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        pid
    } else {
        let program_bytecode = fs::read(program_path).unwrap_or_else(|e| {
            eprintln!("❌ Failed to read program binary '{}': {}", program_path, e);
            eprintln!("   Hint: pass --program-id <hex> to skip loading the binary");
            process::exit(1);
        });
        Program::new(program_bytecode).unwrap_or_else(|e| {
            eprintln!("❌ Failed to load program: {:?}", e);
            process::exit(1);
        }).id()
    };
    println!("  Program ID: {:?}", program_id);

    // Build account map for PDA resolution
    let mut account_map: HashMap<String, AccountId> = HashMap::new();
    for (name, bytes) in &parsed_accounts {
        let mut arr = [0u8; 32];
        arr.copy_from_slice(bytes);
        account_map.insert(name.to_string(), AccountId::new(arr));
    }
    // Note: rest accounts are variadic; store first entry (if any) for PDA seed resolution
    for (name, entries) in &rest_accounts {
        if let Some(first) = entries.first() {
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

    // Resolve PDA accounts
    for acc in &ix.accounts {
        if let Some(pda) = &acc.pda {
            match compute_pda_from_seeds(&pda.seeds, &program_id, &account_map, &parsed_arg_map) {
                Ok(id) => {
                    println!("  PDA {} → {}", acc.name, id);
                    account_map.insert(acc.name.clone(), id);
                }
                Err(e) => {
                    eprintln!("❌ Failed to compute PDA for '{}': {}", acc.name, e);
                    process::exit(1);
                }
            }
        }
    }

    let mut account_ids: Vec<AccountId> = Vec::new();
    for acc in &ix.accounts {
        if acc.rest {
            // expand variadic rest accounts in order (may be 0 or more)
            if let Some((_, entries)) = rest_accounts.iter().find(|(n, _)| *n == acc.name) {
                for bytes in entries {
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

    let wallet_core = WalletCore::from_env().unwrap_or_else(|e| {
        eprintln!("❌ Failed to initialize wallet: {:?}", e);
        eprintln!("   Set NSSA_WALLET_HOME_DIR environment variable");
        process::exit(1);
    });

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
