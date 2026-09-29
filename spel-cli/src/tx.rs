//! Transaction building and submission.

use crate::blob::{TxBlob, WitnessEntry};
use crate::cli::{snake_to_kebab, to_pascal_case};
use crate::hex::{decode_bytes_32, hex_encode, parse_account_id};
use crate::parse::{parse_string_vec, parse_value, ParsedValue};
use crate::pda::compute_pda_from_seeds;
use crate::serialize::serialize_to_borsh;
use common::transaction::LeeTransaction;
use hex;
use nssa::program::Program;
use nssa::public_transaction::{Message, WitnessSet};
use nssa::{AccountId, FeeDeclaration, PublicTransaction};
use nssa::{PublicKey, Signature};
use nssa_core::account::Nonce;
use sequencer_service_rpc::RpcClient as _;
use serde_json::{json, Value};
use spel_framework_core::idl::{IdlInstruction, IdlSeed, IdlType, SpelIdl};
use spel_framework_core::pda::DEFAULT_PRIVATE_PDA_IDENTIFIER;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fs;
use std::process;
use wallet::{WalletCore, DEFAULT_GAS_LIMIT, DEFAULT_MAX_FEE};

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

/// Output format for a `--dry-run` summary.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DryRunFormat {
    Text,
    Json,
}

const UNRESOLVED: &str = "(unresolved)";

/// Execute an instruction: parse args, build TX, optionally submit.
///
/// `dry_run`:
///   - `None`                       — submit to the sequencer
///   - `Some(DryRunFormat::Text)`   — resolve & print a human-readable summary
///   - `Some(DryRunFormat::Json)`   — resolve & emit JSON to stdout
/// Pull the most-recently-supplied value for `key`.  Scalar flags consume
/// the last `--key value` they see; `Vec<String>` args bypass this helper
/// and consume the whole vec.
fn last_value<'a>(map: &'a HashMap<String, Vec<String>>, key: &str) -> Option<&'a str> {
    map.get(key).and_then(|v| v.last().map(|s| s.as_str()))
}

/// True if this IDL type is `Vec<String>` — the one shape that opts in to
/// flag repetition on the CLI.
fn is_vec_string(ty: &IdlType) -> bool {
    matches!(
        ty,
        IdlType::Vec { vec } if matches!(vec.as_ref(), IdlType::Primitive(p) if p == "string" || p == "String"),
    )
}

pub async fn execute_instruction(
    idl: &SpelIdl,
    ix: &IdlInstruction,
    args: &HashMap<String, Vec<String>>,
    program_path: Option<&str>,
    program_address: Option<&str>,
    dry_run: Option<DryRunFormat>,
    extra_bins: &HashMap<String, String>,
    co_signers: &[String],
    fee_payer: Option<&str>,
    gas_limit: Option<u64>,
    export: Option<&str>,
) {
    // In JSON dry-run mode, suppress all human-readable preamble — only emit JSON to stdout.
    let json_mode = dry_run == Some(DryRunFormat::Json);
    macro_rules! say { ($($arg:tt)*) => { if !json_mode { println!($($arg)*); } } }

    say!("📋 Instruction: {}", ix.name);
    say!("");

    let args = args.clone();

    // Auto-fill program-id args from binary paths
    // A program's address used to be its image id, so `--bin name=path` could fill a
    // program-id argument straight from the binary. Since LEZ v0.2.5 the address is
    // chosen at deploy time and the binary does not carry it, so the value has to be
    // passed explicitly; `--bin` now only supplies the ELF for the privacy path.
    for (key, bin_path) in extra_bins {
        if !args.contains_key(key) {
            say!(
                "  ℹ️  --{key} not auto-filled from {bin_path}: a program's address is not \
                 in its binary. Pass --{key} <address>."
            );
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

    // Parse instruction args.
    // Vec<String> is the one shape that consumes every repeated --flag
    // value; every other type takes the last occurrence (scalar semantics).
    let mut parsed_args: Vec<(&str, &IdlType, ParsedValue)> = Vec::new();
    let mut has_errors = false;
    for arg in &ix.args {
        let key = snake_to_kebab(&arg.name);
        let values = args.get(&key).unwrap();
        let result = if is_vec_string(&arg.type_) {
            Ok(parse_string_vec(values))
        } else {
            parse_value(
                values.last().map(|s| s.as_str()).unwrap_or(""),
                &arg.type_,
                &idl.types,
            )
        };
        match result {
            Ok(val) => parsed_args.push((&arg.name, &arg.type_, val)),
            Err(e) => {
                eprintln!("❌ --{}: {}", key, e);
                has_errors = true;
            },
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
        let key = snake_to_kebab(&acc.name);
        if acc.rest {
            // variadic: optional, comma-separated list of account IDs (0 entries is valid).
            // Failed entries are skipped (not pushed as placeholders) so downstream
            // consumers never see a zero-length Vec where a 32-byte AccountId is expected.
            let entries: Vec<(Vec<u8>, bool)> = if let Some(raw) = last_value(&args, &key) {
                raw.split(',')
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .filter_map(|s| match parse_account_id(s) {
                        Ok((bytes, is_priv)) => Some((bytes.to_vec(), is_priv)),
                        Err(e) => {
                            eprintln!("❌ --{}: {}", key, e);
                            has_errors = true;
                            None
                        },
                    })
                    .collect()
            } else {
                vec![]
            };
            rest_accounts.push((&acc.name, entries));
        } else {
            let raw = last_value(&args, &key).unwrap();
            match parse_account_id(raw) {
                Ok((bytes, is_priv)) => parsed_accounts.push((&acc.name, bytes.to_vec(), is_priv)),
                Err(e) => {
                    eprintln!("❌ --{}: {}", key, e);
                    has_errors = true;
                },
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
    let borsh_args: Vec<_> = parsed_args.iter().map(|(_, ty, val)| (*ty, val)).collect();
    let instruction_data = serialize_to_borsh(ix_index as u32, &borsh_args).unwrap_or_else(|e| {
        eprintln!("❌ Serialization error: {}", e);
        process::exit(1);
    });

    // ─── Resolve the program's account, and its binary if we have one ─────────
    //
    // Since LEZ v0.2.5 a transaction names the program by its deployed header
    // account. The binary is a separate, optional input: only the
    // privacy-preserving path needs it, to prove against the ELF.
    let program_account_id: AccountId = match program_address {
        Some(value) => {
            let bytes = spel_framework_core::pda::parse_bytes32(value).unwrap_or_else(|e| {
                eprintln!("❌ Invalid program address '{}': {}", value, e);
                process::exit(1);
            });
            AccountId::new(bytes)
        },
        None => {
            eprintln!(
                "❌ No program specified. Use --program <name|address> or configure in spel.toml."
            );
            process::exit(1);
        },
    };

    let program_obj: Option<Program> = program_path.and_then(|path| {
        let program_bytecode = fs::read(path).ok()?;
        match Program::new(program_bytecode.into()) {
            Ok(program) => Some(program),
            Err(e) => {
                eprintln!("⚠️  Ignoring unreadable program binary '{}': {:?}", path, e);
                None
            },
        }
    });

    let program_account_str = program_account_id.to_string();

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
                        if let Some(raw) = last_value(&args, &key) {
                            match decode_bytes_32(raw) {
                                Ok(bytes) => {
                                    say!("  ℹ️  Using --{} for PDA seed '{}'", key, path);
                                    account_map.insert(path.clone(), AccountId::new(bytes));
                                },
                                Err(e) => {
                                    eprintln!("❌ --{}: {}", key, e);
                                    process::exit(1);
                                },
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

    // Compute PDAs
    for acc in &ix.accounts {
        if let Some(pda) = &acc.pda {
            match compute_pda_from_seeds(
                &pda.seeds,
                &program_account_id,
                &account_map,
                &parsed_arg_map,
                None,
                None,
                DEFAULT_PRIVATE_PDA_IDENTIFIER,
            ) {
                Ok(id) => {
                    account_map.insert(acc.name.clone(), id);
                },
                Err(e) => {
                    eprintln!("❌ Failed to compute PDA for '{}': {}", acc.name, e);
                    process::exit(1);
                },
            }
        }
    }

    // ─── Dry-run summary ────────────────────────────────────────
    if let Some(fmt) = dry_run {
        let signer_names: Vec<&str> = ix
            .accounts
            .iter()
            .filter(|a| a.signer)
            .map(|a| a.name.as_str())
            .collect();
        let signer_ids: Vec<AccountId> = signer_names
            .iter()
            .filter_map(|n| account_map.get(*n).copied())
            .collect();
        let signer_nonces = fetch_nonces_best_effort(signer_ids).await;

        let summary = DryRunSummary {
            program_id_hex: &program_account_str,
            ix,
            account_map: &account_map,
            parsed_account_ids: &parsed_accounts
                .iter()
                .map(|(n, b, _)| (*n, b.as_slice()))
                .collect::<Vec<_>>(),
            rest_account_ids: &rest_accounts
                .iter()
                .map(|(n, es)| (*n, es.iter().map(|(b, _)| b.as_slice()).collect::<Vec<_>>()))
                .collect::<Vec<_>>(),
            parsed_args: &parsed_args,
            instruction_data: &instruction_data,
            signer_names: &signer_names,
            signer_nonces: &signer_nonces,
        };
        match fmt {
            DryRunFormat::Json => print_dry_run_json(&summary),
            DryRunFormat::Text => {
                println!("=== Dry Run ===");
                print!("{}", render_dry_run_text(&summary));
                println!("================");
                println!("Dry run complete — not submitted.");
            },
        }
        return;
    }

    // ─── Pre-submission display ─────────────────────────────────
    say!("Accounts:");
    for acc in &ix.accounts {
        if let Some(pda) = &acc.pda {
            let id = account_map
                .get(&acc.name)
                .map(|a| format!("{}", a))
                .unwrap_or_else(|| UNRESOLVED.to_string());
            say!("  📦 {} → {} (PDA)", acc.name, id);
            say!("    seeds: {}", format_pda_seeds(&pda.seeds));
        } else if acc.rest {
            if let Some((_, entries)) = rest_accounts.iter().find(|(n, _)| *n == acc.name) {
                if entries.is_empty() {
                    say!("  📦 {} → (none — variadic rest)", acc.name);
                } else {
                    for (e, _) in entries {
                        say!("  📦 {} → 0x{}", acc.name, hex_encode(e));
                    }
                }
            }
        } else {
            let account_bytes = parsed_accounts
                .iter()
                .find(|(n, _, _)| *n == acc.name)
                .unwrap();
            say!("  📦 {} → 0x{}", acc.name, hex_encode(&account_bytes.1));
        }
    }
    say!("");
    say!("Arguments (parsed):");
    for (name, _, val) in &parsed_args {
        say!("  {} = {}", name, val);
    }
    say!("");
    say!("🔧 Transaction:");
    say!("  program:    {}", program_account_str);
    if let Some(path) = program_path {
        say!("  binary:     {}", path);
    }
    say!("  instruction index: {}", ix_index);
    say!("  instruction: {} {{", to_pascal_case(&ix.name));
    for (name, _, val) in &parsed_args {
        say!("    {}: {},", name, val);
    }
    say!("  }}");
    say!("");
    say!(
        "  Serialized instruction data ({} bytes, borsh):",
        instruction_data.len()
    );
    let ix_data_hex: String = instruction_data
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    say!("    0x{ix_data_hex}");
    say!("");

    // ─── Transaction submission ─────────────────────────────────
    if export.is_some() {
        say!("📦 Building partial transaction for export...");
    } else {
        say!("📤 Submitting transaction...");
    }

    let wallet_core = WalletCore::from_env().await.unwrap_or_else(|e| {
        eprintln!("❌ Failed to initialize wallet: {:?}", e);
        eprintln!("   Set LEE_WALLET_HOME_DIR environment variable");
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
        use wallet::AccountIdentity;

        let program = program_obj.unwrap_or_else(|| {
            eprintln!(
                "❌ Privacy-preserving transactions require the program binary (not --program-id)"
            );
            process::exit(1);
        });

        // Build dependencies from extra_bins
        let mut dependencies = HashMap::new();
        // Dependencies are keyed by the dependency's deployed account id since v0.2.5.
        // A binary does not carry that, so take it from the same-named argument the
        // caller passed (e.g. `--bin token=... --token-program-id <address>`).
        for (name, bin_path) in extra_bins {
            let Ok(bytes) = fs::read(bin_path) else {
                continue;
            };
            let Ok(dep_program) = Program::new(bytes.into()) else {
                continue;
            };
            let dep_address = args
                .get(name)
                .and_then(|v| v.first())
                .and_then(|raw| spel_framework_core::pda::parse_bytes32(raw).ok());
            match dep_address {
                Some(bytes) => {
                    dependencies.insert(AccountId::new(bytes), dep_program);
                },
                None => {
                    eprintln!(
                        "⚠️  Skipping dependency '{name}': no address. Pass --{name} \
                         <address> so the proof can name the program it calls."
                    );
                },
            }
        }
        let program_with_deps =
            ProgramWithDependencies::new(program, program_account_id, dependencies);

        // Build privacy-preserving account list
        let mut pp_accounts: Vec<AccountIdentity> = Vec::new();
        for acc in &ix.accounts {
            if acc.rest {
                if let Some((_, entries)) = rest_accounts.iter().find(|(n, _)| *n == acc.name) {
                    for (bytes, is_priv) in entries {
                        let mut arr = [0u8; 32];
                        arr.copy_from_slice(bytes);
                        let account_id = AccountId::new(arr);
                        if *is_priv {
                            pp_accounts.push(AccountIdentity::PrivateOwned(account_id));
                        } else {
                            pp_accounts.push(AccountIdentity::Public(account_id));
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
                    pp_accounts.push(AccountIdentity::PrivateOwned(id));
                } else {
                    pp_accounts.push(AccountIdentity::Public(id));
                }
            } else {
                // PDA account — always public
                let id = *account_map.get(&acc.name).unwrap_or_else(|| {
                    eprintln!("❌ Account '{}' not resolved", acc.name);
                    process::exit(1);
                });
                pp_accounts.push(AccountIdentity::Public(id));
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

        say!("📤 Privacy-preserving transaction submitted!");
        say!("   tx_hash: {}", hex::encode(response.0));
        say!("   Waiting for confirmation...");

        let poller = wallet_core.poller_helm();

        match poller.poll_tx(response).await {
            Ok(_) => say!("✅ Transaction confirmed — included in a block."),
            Err(e) => {
                eprintln!("❌ Transaction NOT confirmed: {e:#}");
                process::exit(1);
            },
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

        let mut signer_accounts: Vec<AccountId> = ix
            .accounts
            .iter()
            .filter(|a| a.signer)
            .map(|a| *account_map.get(&a.name).unwrap())
            .collect();

        for co_signer in co_signers {
            let bytes = decode_bytes_32(co_signer).unwrap_or_else(|e| {
                eprintln!("❌ --co-signer '{}': {}", co_signer, e);
                process::exit(1);
            });
            let account_id = AccountId::new(bytes);
            if !signer_accounts.contains(&account_id) {
                signer_accounts.push(account_id);
            }
        }

        // Since LEZ v0.2.5 an ordinary program call is a CHARGED transaction: omitting
        // the fee declaration is rejected outright as `MissingFeeDeclaration`, which the
        // sequencer reports as the opaque "Incorrect fee". So every public transaction
        // now names a payer.
        //
        // The payer must also SIGN -- `is_fee_authorized` looks for a witness whose
        // account id equals the payer -- so it joins `signer_accounts` (and therefore the
        // nonce list, which pairs with signatures, not with `account_ids`). Default is the
        // first signer, i.e. self-pay; `--fee-payer` covers the common case where the
        // signers are freshly created accounts holding nothing.
        let payer = match fee_payer {
            Some(raw) => {
                let bytes = decode_bytes_32(raw).unwrap_or_else(|e| {
                    eprintln!("❌ --fee-payer '{}': {}", raw, e);
                    process::exit(1);
                });
                let id = AccountId::new(bytes);
                if !signer_accounts.contains(&id) {
                    signer_accounts.push(id);
                }
                id
            },
            None => *signer_accounts.first().unwrap_or_else(|| {
                eprintln!("❌ This transaction has no signing account to pay its fee.");
                eprintln!("   Name a funded account with --fee-payer <ADDRESS>.");
                process::exit(1);
            }),
        };

        // `gas_limit` is not an accounting figure: it is the zkVM SESSION LIMIT the guest
        // runs under (`chain_state::apply` passes it straight to
        // `from_public_transaction_metered` as the cycle budget). Overrun is not an error the
        // caller sees -- the guest bails, the full budget is charged, and the transaction
        // lands having advanced nonces and written nothing. The wallet's 2M default is sized
        // for its own small native calls; a RISC Zero guest costs millions of cycles, so
        // anything non-trivial needs `--gas-limit`. The ceiling is `MAX_GAS_EXEC` (10M).
        let gas_limit = gas_limit.unwrap_or(DEFAULT_GAS_LIMIT);
        // Keep max_fee in step with the limit, mirroring how the wallet sizes its default
        // (gas + assumed data bytes, at 8x the genesis minimum base fee).
        let max_fee = std::cmp::max(DEFAULT_MAX_FEE, u128::from(gas_limit + 100_000) * 64);

        let nonces = if signer_accounts.is_empty() {
            vec![]
        } else {
            wallet_core
                .get_accounts_nonces(&signer_accounts)
                .await
                .unwrap_or_else(|e| {
                    eprintln!("❌ Failed to fetch nonces: {:?}", e);
                    process::exit(1);
                })
        };

        let message = Message::new_preserialized(
            program_account_id,
            account_ids,
            nonces,
            instruction_data,
            Some(FeeDeclaration::new(payer, gas_limit, 0, max_fee)),
        );

        if let Some(export_path) = export {
            // (1) the exact bytes every signer signs
            let message_bytes = borsh::to_vec(&message).unwrap_or_else(|e| {
                eprintln!("❌ Failed to serialize message: {:?}", e);
                process::exit(1);
            });

            // (2) render the summary from the built data, same renderer as --dry-run
            let signer_names: Vec<&str> = ix
                .accounts
                .iter()
                .filter(|a| a.signer)
                .map(|a| a.name.as_str())
                .collect();
            let co_signer_labels: Vec<String> = signer_accounts[signer_names.len()..]
                .iter()
                .map(|id| format!("co-signer 0x{}", hex_encode(id.value())))
                .collect();
            let all_signer_names: Vec<&str> = signer_names
                .iter()
                .copied()
                .chain(co_signer_labels.iter().map(|s| s.as_str()))
                .collect();
            let signer_nonces: Vec<Option<Nonce>> =
                message.nonces.iter().copied().map(Some).collect();
            let summary_data = DryRunSummary {
                program_id_hex: &program_account_str,
                ix,
                account_map: &account_map,
                parsed_account_ids: &parsed_accounts
                    .iter()
                    .map(|(n, b, _)| (*n, b.as_slice()))
                    .collect::<Vec<_>>(),
                rest_account_ids: &rest_accounts
                    .iter()
                    .map(|(n, es)| (*n, es.iter().map(|(b, _)| b.as_slice()).collect::<Vec<_>>()))
                    .collect::<Vec<_>>(),
                parsed_args: &parsed_args,
                instruction_data: &message.instruction_data,
                signer_names: &all_signer_names,
                signer_nonces: &signer_nonces,
            };
            let summary = render_dry_run_text(&summary_data);

            // (3) sign with every required key this wallet holds, skip the rest.
            // v0.2.0: witnesses sign the 32-byte message hash, matching
            // WitnessSet::is_valid_for at submit.
            let message_hash = message.hash();
            let mut witnesses = BTreeMap::new();
            for account_id in &signer_accounts {
                if let Some(key) = wallet_core.get_account_public_signing_key(*account_id) {
                    let signature = Signature::new(key, &message_hash);
                    let pubkey = PublicKey::new_from_private_key(key);
                    witnesses.insert(
                        format!("0x{}", hex_encode(account_id.value())),
                        WitnessEntry {
                            pubkey,
                            signature: signature.to_string(),
                        },
                    );
                }
            }

            // (4) assemble and write
            let tx_blob = TxBlob {
                version: 1,
                summary,
                message_hex: hex_encode(&message_bytes),
                signers: signer_accounts
                    .iter()
                    .map(|id| format!("0x{}", hex_encode(id.value())))
                    .collect(),
                witnesses,
            };
            tx_blob.save(export_path).unwrap_or_else(|e| {
                eprintln!("❌ {}", e);
                process::exit(1);
            });

            say!("📦 Partial transaction written to {}", export_path);
            say!(
                "   Signed {} of {} required signers.",
                tx_blob.witnesses.len(),
                tx_blob.signers.len()
            );
            for missing in tx_blob.missing_signers() {
                say!("   Missing: {}", missing);
            }
            say!("   Send the file to the remaining signers to run: spel sign <file>");
            return;
        }

        let signing_keys: Vec<_> = signer_accounts
            .iter()
            .map(|id| {
                wallet_core
                    .get_account_public_signing_key(*id)
                    .unwrap_or_else(|| {
                        eprintln!("❌ Signing key not found for account {}", id);
                        process::exit(1);
                    })
            })
            .collect();

        let witness_set = WitnessSet::for_message(&message, &signing_keys);
        let tx = PublicTransaction::new(message, witness_set);

        let tx_hash = wallet_core
            .helm_owned()
            .send_transaction(LeeTransaction::Public(tx))
            .await
            .unwrap_or_else(|e| {
                eprintln!("❌ Failed to submit transaction: {:?}", e);
                process::exit(1);
            });

        say!("📤 Transaction submitted!");
        say!("   tx_hash: {}", tx_hash);
        say!("   Waiting for confirmation...");

        let poller = wallet_core.poller_helm();

        match poller.poll_tx(tx_hash).await {
            Ok(_) => say!("✅ Transaction confirmed — included in a block."),
            Err(e) => {
                eprintln!("❌ Transaction NOT confirmed: {e:#}");
                process::exit(1);
            },
        }
    }
}

/// Best-effort nonce fetch for a dry-run summary. Returns one entry per signer:
/// `Some(Nonce)` on success, `None` if the wallet env is unavailable or the
/// sequencer call fails.
async fn fetch_nonces_best_effort(signer_ids: Vec<AccountId>) -> Vec<Option<Nonce>> {
    if signer_ids.is_empty() {
        return vec![];
    }
    let len = signer_ids.len();
    let result = async {
        let wc = WalletCore::from_env().await.ok()?;
        wc.get_accounts_nonces(&signer_ids).await.ok()
    }
    .await;
    match result {
        Some(ns) => ns.into_iter().map(Some).collect(),
        None => vec![None; len],
    }
}

/// Bundle of resolved transaction data passed to the dry-run renderers.
struct DryRunSummary<'a> {
    program_id_hex: &'a str,
    ix: &'a IdlInstruction,
    account_map: &'a HashMap<String, AccountId>,
    /// Named (non-PDA, non-rest) accounts: `(name, raw_32_bytes)`.
    parsed_account_ids: &'a [(&'a str, &'a [u8])],
    /// Rest (variadic) accounts: `(name, [raw_32_bytes…])`.
    rest_account_ids: &'a [(&'a str, Vec<&'a [u8]>)],
    parsed_args: &'a [(&'a str, &'a IdlType, ParsedValue)],
    instruction_data: &'a [u8],
    signer_names: &'a [&'a str],
    signer_nonces: &'a [Option<Nonce>],
}

/// Render a non-rest account as JSON, including PDA seed metadata when applicable.
fn account_to_json(acc: &spel_framework_core::idl::IdlAccountItem, id_str: String) -> Value {
    let mut flags: Vec<&str> = Vec::new();
    if acc.signer {
        flags.push("signer");
    }
    if acc.writable {
        flags.push("writable");
    }

    let mut obj = json!({
        "name": acc.name,
        "id": id_str,
        "flags": flags,
    });
    if let Some(pda) = &acc.pda {
        let seeds: Vec<Value> = pda
            .seeds
            .iter()
            .map(|s| match s {
                IdlSeed::Const { value } => json!({"kind": "const", "value": value}),
                IdlSeed::Account { path } => json!({"kind": "account", "path": path}),
                IdlSeed::Arg { path } => json!({"kind": "arg", "path": path}),
            })
            .collect();
        obj["is_pda"] = json!(true);
        obj["seeds"] = json!(seeds);
    }
    obj
}

fn print_dry_run_json(s: &DryRunSummary<'_>) {
    let mut accounts_json: Vec<Value> = Vec::new();
    for acc in &s.ix.accounts {
        if acc.rest {
            if let Some((_, entries)) = s.rest_account_ids.iter().find(|(n, _)| *n == acc.name) {
                if entries.is_empty() {
                    accounts_json.push(json!({
                        "name": acc.name,
                        "id": null,
                        "flags": ["rest"],
                        "is_rest": true,
                    }));
                } else {
                    for bytes in entries {
                        accounts_json.push(json!({
                            "name": acc.name,
                            "id": format!("0x{}", hex_encode(bytes)),
                            "flags": ["rest"],
                            "is_rest": true,
                        }));
                    }
                }
            }
        } else if acc.pda.is_some() {
            let id_str = s
                .account_map
                .get(&acc.name)
                .map(|a| format!("{}", a))
                .unwrap_or_else(|| UNRESOLVED.to_string());
            accounts_json.push(account_to_json(acc, id_str));
        } else {
            // Non-rest, non-PDA accounts must be in parsed_account_ids by construction
            // (validated via `missing` check earlier in execute_instruction).
            let id_str = match s.parsed_account_ids.iter().find(|(n, _)| *n == acc.name) {
                Some((_, b)) => format!("0x{}", hex_encode(b)),
                None => {
                    debug_assert!(
                        false,
                        "non-rest non-PDA account '{}' missing from parsed_account_ids",
                        acc.name
                    );
                    UNRESOLVED.to_string()
                },
            };
            accounts_json.push(account_to_json(acc, id_str));
        }
    }

    let args_json: serde_json::Map<String, Value> = s
        .parsed_args
        .iter()
        .map(|(name, _, val)| {
            let v = match val {
                ParsedValue::U8(n) => json!(n),
                ParsedValue::U32(n) => json!(n),
                ParsedValue::U64(n) => json!(n),
                // u128 exceeds JSON's 53-bit integer precision — encode as decimal string.
                ParsedValue::U128(n) => json!(n.to_string()),
                other => json!(other.to_string()),
            };
            (name.to_string(), v)
        })
        .collect();

    let signers_json: serde_json::Map<String, Value> = s
        .signer_names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let nonce_val = s.signer_nonces.get(i).and_then(|n| n.as_ref())
            // u128 nonce — encode as decimal string to avoid silent truncation.
            .map(|n| json!(n.0.to_string()))
            .unwrap_or(Value::Null);
            (name.to_string(), json!({"nonce": nonce_val}))
        })
        .collect();

    let ix_data_hex: String = s
        .instruction_data
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect();

    let summary = json!({
        "program_id": s.program_id_hex,
        "instruction": s.ix.name,
        "accounts": accounts_json,
        "arguments": args_json,
        "instruction_data": ix_data_hex,
        "signers": signers_json,
    });
    println!("{}", serde_json::to_string_pretty(&summary).unwrap());
}

fn render_dry_run_text(s: &DryRunSummary<'_>) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    writeln!(out, "Program ID: {}", s.program_id_hex).unwrap();
    writeln!(out, "Instruction: {}", s.ix.name).unwrap();
    writeln!(out).unwrap();
    writeln!(out, "Accounts:").unwrap();
    for acc in &s.ix.accounts {
        let mut flags: Vec<&str> = Vec::new();
        if acc.signer {
            flags.push("signer");
        }
        if acc.writable {
            flags.push("writable");
        }
        if acc.rest {
            flags.push("rest");
        }
        let flags_str = if flags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", flags.join(", "))
        };

        if let Some(pda) = &acc.pda {
            let id = s
                .account_map
                .get(&acc.name)
                .map(|a| format!("{}", a))
                .unwrap_or_else(|| UNRESOLVED.to_string());
            writeln!(out, "  PDA {} → {}{}", acc.name, id, flags_str).unwrap();
            writeln!(out, "    seeds: {}", format_pda_seeds(&pda.seeds)).unwrap();
        } else if acc.rest {
            if let Some((_, entries)) = s.rest_account_ids.iter().find(|(n, _)| *n == acc.name) {
                if entries.is_empty() {
                    writeln!(out, "  {} → (none — variadic rest){}", acc.name, flags_str).unwrap();
                } else {
                    for bytes in entries {
                        writeln!(out, "  {} → 0x{}{}", acc.name, hex_encode(bytes), flags_str)
                            .unwrap();
                    }
                }
            }
        } else if let Some((_, b)) = s.parsed_account_ids.iter().find(|(n, _)| *n == acc.name) {
            writeln!(out, "  {} → 0x{}{}", acc.name, hex_encode(b), flags_str).unwrap();
        } else {
            debug_assert!(
                false,
                "non-rest non-PDA account '{}' missing from parsed_account_ids",
                acc.name
            );
            writeln!(out, "  {} → {}{}", acc.name, UNRESOLVED, flags_str).unwrap();
        }
    }
    writeln!(out).unwrap();
    writeln!(out, "Arguments:").unwrap();
    for (name, _, val) in s.parsed_args {
        writeln!(out, "  --{} {}", snake_to_kebab(name), val).unwrap();
    }
    writeln!(out).unwrap();
    let ix_data_hex: String = s
        .instruction_data
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect();
    writeln!(out, "Instruction data: 0x{}", ix_data_hex).unwrap();
    if !s.signer_names.is_empty() {
        writeln!(out).unwrap();
        writeln!(out, "Signers:").unwrap();
        for (i, name) in s.signer_names.iter().enumerate() {
            let nonce_str = match s.signer_nonces.get(i).and_then(|n| n.as_ref()) {
                Some(n) => format!("nonce={}", n.0),
                None => "nonce=(unknown)".to_string(),
            };
            writeln!(out, "  {}: {}", name, nonce_str).unwrap();
        }
    }
    out
}
