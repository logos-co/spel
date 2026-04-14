//! Generic IDL-driven CLI library for SPEL programs.
//!
//! Provides:
//! - IDL parsing and type-aware argument handling
//! - risc0-compatible serialization
//! - Transaction building and submission
//! - PDA computation from IDL seeds
//! - Binary inspection (ProgramId extraction)
//!
//! Use `run()` for a complete CLI entry point, or import individual modules.

pub mod hex;
pub mod parse;
pub mod serialize;
pub mod pda;
pub mod tx;
pub mod inspect;
pub mod account_inspect;
pub mod cli;
pub mod config;
pub mod init;
pub mod generate_idl;

use cli::{print_help, parse_instruction_args, snake_to_kebab};
use config::SpelConfig;
use init::init_project;
use inspect::inspect_binaries;
use tx::execute_instruction;
use pda::compute_pda_from_seeds;
use spel_framework_core::idl::{SpelIdl, IdlSeed};
use parse::ParsedValue;
use std::collections::HashMap;
use std::{env, fs, process};

/// Run the generic IDL-driven CLI. Call this from your program's main():
///
/// ```no_run
/// #[tokio::main]
/// async fn main() {
///     spel::run().await;
/// }
/// ```
pub async fn run() {
    let args: Vec<String> = env::args().collect();

    let mut idl_path = String::new();
    let mut program_ref: Option<String> = None; // raw --program value
    let mut dry_run = false;
    let mut type_name: Option<String> = None;
    let mut data_hex: Option<String> = None;
    let mut extra_bins: HashMap<String, String> = HashMap::new();
    let mut remaining_args: Vec<String> = vec![args[0].clone()];
    let mut used_separator = false;
    let mut i = 1;

    while i < args.len() {
        match args[i].as_str() {
            "--" => {
                // Everything after `--` is passed through as instruction args
                used_separator = true;
                remaining_args.extend_from_slice(&args[i + 1..]);
                break;
            }
            "--idl" | "-i" => {
                i += 1;
                if i < args.len() { idl_path = args[i].clone(); }
            }
            "--program" | "-p" => {
                i += 1;
                if i < args.len() { program_ref = Some(args[i].clone()); }
            }
            "--program-id" => {
                eprintln!("⚠️  --program-id is deprecated. Use --program <HEX> instead.");
                i += 1;
                if i < args.len() { program_ref = Some(args[i].clone()); }
            }
            "--type" | "-t" => {
                i += 1;
                if i < args.len() { type_name = Some(args[i].clone()); }
            }
            "--data" | "-d" => {
                i += 1;
                if i < args.len() { data_hex = Some(args[i].clone()); }
            }
            "--dry-run" => { dry_run = true; }
            s if s.starts_with("--bin-") => {
                let name = s.strip_prefix("--bin-").unwrap().to_string();
                i += 1;
                if i < args.len() {
                    extra_bins.insert(format!("{}-program-id", name), args[i].clone());
                }
            }
            _ => remaining_args.push(args[i].clone()),
        }
        i += 1;
    }

    // Load spel.toml config
    let config = env::current_dir()
        .ok()
        .and_then(|cwd| SpelConfig::discover(&cwd));
    let has_config = config.is_some();

    // Resolve --program value: config name → 64-char hex → file path
    let mut program_path: Option<String> = None;
    let mut program_id_hex: Option<String> = None;

    if let Some(ref value) = program_ref {
        let resolved_from_config = config.as_ref().and_then(|(_, cfg)| {
            if cfg.has_program(value) {
                cfg.resolve_program(Some(value)).ok()
            } else {
                None
            }
        });

        if let Some(prog) = resolved_from_config {
            // Config name → set both IDL and binary from config entry
            if idl_path.is_empty() {
                if let Some(ref idl) = prog.idl { idl_path = idl.clone(); }
            }
            program_path = prog.binary.clone();
        } else if is_hex_program_id(value) {
            program_id_hex = Some(value.clone());
        } else {
            program_path = Some(value.clone());
        }
    }

    // Fill gaps from config default program (when --program not given or didn't resolve IDL)
    if let Some((_, ref cfg)) = config {
        if program_ref.is_none() {
            if let Ok(prog) = cfg.resolve_program(None) {
                if idl_path.is_empty() {
                    if let Some(ref idl) = prog.idl { idl_path = idl.clone(); }
                }
                if program_path.is_none() {
                    program_path = prog.binary.clone();
                }
            }
        }
    }

    // Handle commands that don't need an IDL
    if let Some(cmd) = remaining_args.get(1).map(|s| s.as_str()) {
        match cmd {
            "init" => {
                // Check for help flag
                if remaining_args.get(2) == Some(&"-h".to_string()) 
                    || remaining_args.get(2) == Some(&"--help".to_string()) {
                    println!("Usage: spel init <project-name> [OPTIONS]");
                    println!();
                    println!("Create a new SPEL project");
                    println!();
                    println!("Options:");
                    println!("  --lez-tag <TAG>     LEZ version tag (default: v0.2.0-rc1)");
                    println!("  --spel-rev <REV>    SPEL revision (default: refs/pull/122/head)");
                    println!("  --lez-rev <REV>     LEZ revision (alternative to --lez-tag)");
                    println!("  --spel-tag <TAG>    SPEL tag (alternative to --spel-rev)");
                    println!();
                    println!("Examples:");
                    println!("  spel init my-project");
                    println!("  spel init my-project --lez-tag v0.2.0-rc1 --spel-rev refs/pull/122/head");
                    return;
                }
                let mut lez_tag: Option<String> = None;
                let mut spel_tag: Option<String> = None;
                let mut lez_rev: Option<String> = None;
                let mut spel_rev: Option<String> = None;
                let mut name_arg_idx = 2;

                while name_arg_idx < remaining_args.len() {
                    let arg = &remaining_args[name_arg_idx];
                    if arg == "--lez-tag" {
                        name_arg_idx += 1;
                        if name_arg_idx < remaining_args.len() { lez_tag = Some(remaining_args[name_arg_idx].clone()); }
                    } else if arg == "--spel-tag" {
                        name_arg_idx += 1;
                        if name_arg_idx < remaining_args.len() { spel_tag = Some(remaining_args[name_arg_idx].clone()); }
                    } else if arg == "--lez-rev" {
                        name_arg_idx += 1;
                        if name_arg_idx < remaining_args.len() { lez_rev = Some(remaining_args[name_arg_idx].clone()); }
                    } else if arg == "--spel-rev" {
                        name_arg_idx += 1;
                        if name_arg_idx < remaining_args.len() { spel_rev = Some(remaining_args[name_arg_idx].clone()); }
                    } else {
                        break;
                    }
                    name_arg_idx += 1;
                }

                let name = remaining_args.get(name_arg_idx).unwrap_or_else(|| {
                    eprintln!("Usage: {} init <project-name> [--lez-tag <tag>] [--spel-tag <tag>] [--lez-rev <rev>] [--spel-rev <rev>]", args[0]);
                    process::exit(1);
                });
                init_project(name, lez_tag.as_deref(), spel_tag.as_deref(), lez_rev.as_deref(), spel_rev.as_deref());
                return;
            }
            "inspect" if type_name.is_none() && data_hex.is_none() && idl_path.is_empty() => {
                inspect_binaries(&remaining_args[2..]);
                return;
            }
            "inspect" => {
                // Account inspection mode: --type and --idl required
                if idl_path.is_empty() {
                    eprintln!("Account inspection requires --idl <IDL_FILE>");
                    process::exit(1);
                }
                if type_name.is_none() {
                    eprintln!("Account inspection requires --type <TypeName>");
                    process::exit(1);
                }
                let account_id = remaining_args.get(2).unwrap_or_else(|| {
                    eprintln!("Usage: {} inspect <account-id> --idl <IDL> --type <TypeName> [--data <hex>]", args[0]);
                    process::exit(1);
                });
                let idl_content = match fs::read_to_string(&idl_path) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("Error reading IDL '{}': {}", idl_path, e);
                        process::exit(1);
                    }
                };
                let idl: SpelIdl = serde_json::from_str(&idl_content).unwrap_or_else(|e| {
                    eprintln!("Error parsing IDL: {}", e);
                    process::exit(1);
                });
                account_inspect::inspect_account(
                    account_id,
                    &idl,
                    type_name.as_ref().unwrap(),
                    data_hex.as_deref(),
                ).await;
                return;
            }
            "generate-idl" => {
                use spel_framework_core::idl_gen::generate_idl_from_file;
                use generate_idl::discover_sources;

                let arg = remaining_args.get(2).map(|s| s.as_str());
                let sources = discover_sources(arg).unwrap_or_else(|e| {
                    eprintln!("Error: {}", e);
                    process::exit(1);
                });

                if sources.len() == 1 {
                    match generate_idl_from_file(&sources[0]) {
                        Ok(idl) => println!("{}", serde_json::to_string_pretty(&idl).unwrap()),
                        Err(e) => {
                            eprintln!("Error: {}", e);
                            process::exit(1);
                        }
                    }
                } else {
                    // Multiple programs: write <name>-idl.json for each
                    let mut had_error = false;
                    for source in &sources {
                        match generate_idl_from_file(source) {
                            Ok(idl) => {
                                let out_name = format!("{}-idl.json", idl.name);
                                match fs::write(&out_name, serde_json::to_string_pretty(&idl).unwrap()) {
                                    Ok(_) => eprintln!("✅ {}", out_name),
                                    Err(e) => {
                                        eprintln!("Error writing {}: {}", out_name, e);
                                        had_error = true;
                                    }
                                }
                            }
                            Err(e) => {
                                eprintln!("Error processing {}: {}", source.display(), e);
                                had_error = true;
                            }
                        }
                    }
                    if had_error {
                        process::exit(1);
                    }
                }
                return;
            }
            "pda" if program_id_hex.is_some() && remaining_args.get(2).is_some_and(|s| !s.starts_with("--")) => {
                // Raw PDA mode: no IDL needed
                // Triggered when --program <hex> resolves to a program ID + pda command
                // Usage: <bin> --program <hex> pda <seed1> [seed2] ...
                let mut raw_args = vec!["--program-id".to_string(), program_id_hex.clone().unwrap()];
                raw_args.extend_from_slice(&remaining_args[2..]);
                compute_pda_raw(&raw_args);
                return;
            }
            _ => {}
        }
    }

    if idl_path.is_empty() {
        eprintln!("Usage: {} [OPTIONS] -- <COMMAND> [ARGS]", args[0]);
        eprintln!();
        eprintln!("Tip: create a spel.toml with [program] or [programs.<name>] to avoid passing flags.");
        eprintln!();
        eprintln!("Commands that don't need --idl:");
        eprintln!("  init <name>              Scaffold a new SPEL project");
        eprintln!("  inspect <FILE> [FILE...]  Print ProgramId for ELF binary(ies)");
        eprintln!("  inspect <ACCOUNT-ID> --idl <IDL> --type <TYPE>  Decode account data");
        eprintln!("  generate-idl [PATH]      Generate IDL JSON from a program source file or project directory");
        eprintln!();
        eprintln!("  pda <ACCOUNT> [--seed-arg VALUE...]  Compute a PDA defined in the IDL");
        eprintln!("  pda --program <HEX> <SEED> [SEED...]  Compute arbitrary PDA (no IDL needed)");
        eprintln!("For all other commands, provide an IDL file via --idl or spel.toml.");
        process::exit(1);
    }

    let idl_content = match fs::read_to_string(&idl_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error reading IDL '{}': {}", idl_path, e);
            process::exit(1);
        }
    };
    let idl: SpelIdl = serde_json::from_str(&idl_content).unwrap_or_else(|e| {
        eprintln!("Error parsing IDL: {}", e);
        process::exit(1);
    });

    let subcmd = remaining_args.get(1).map(|s| s.as_str());
    let binary_name = std::path::Path::new(&args[0])
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| args[0].clone());

    match subcmd {
        Some("--help") | Some("-h") | None => {
            print_help(&idl, &binary_name);
        }
        Some("idl") => {
            println!("{}", serde_json::to_string_pretty(&idl).unwrap());
        }
        Some("inspect") if type_name.is_some() => {
            let account_id = remaining_args.get(2).unwrap_or_else(|| {
                eprintln!("Usage: {} inspect <account-id> --idl <IDL> --type <TypeName> [--data <hex>]", args[0]);
                process::exit(1);
            });
            account_inspect::inspect_account(
                account_id,
                &idl,
                type_name.as_ref().unwrap(),
                data_hex.as_deref(),
            ).await;
        }
        Some("inspect") => {
            inspect_binaries(&remaining_args[2..]);
        }
        Some("pda") => {
            compute_pda_command(&idl, program_path.as_deref(), program_id_hex.as_deref(), &remaining_args[2..]);
        }
        Some(cmd) => {
            let instruction = idl.instructions.iter().find(|ix| {
                snake_to_kebab(&ix.name) == cmd || ix.name == cmd
            });

            match instruction {
                Some(ix) => {
                    if !used_separator && !has_config {
                        eprintln!("⚠️  Deprecation: mixing CLI and instruction args without '--' separator.");
                        eprintln!("    Consider adding a spel.toml to your project, or use:");
                        eprintln!("      spel --idl <FILE> -- <command> --arg1 value1");
                        eprintln!();
                    }
                    let cli_args = parse_instruction_args(&remaining_args[2..], ix);
                    execute_instruction(
                        &idl, ix, &cli_args, program_path.as_deref(), program_id_hex.as_deref(), dry_run, &extra_bins,
                    ).await;
                }
                None => {
                    eprintln!("Unknown command: {}", cmd);
                    print_help(&idl, &binary_name);
                    process::exit(1);
                }
            }
        }
    }
}

/// Compute and print a PDA from the IDL definition.
///
/// Usage: <binary> --idl <IDL> pda <account-name> [--<seed-arg> <value> ...]
///
/// Looks up the named account across all instructions, finds its PDA seeds,
/// resolves them using provided args, and prints the base58 AccountId.
fn compute_pda_command(idl: &SpelIdl, program_path: Option<&str>, program_id_hex: Option<&str>, args: &[String]) {
    let account_name = match args.first() {
        Some(n) => n.as_str(),
        None => {
            eprintln!("Usage: pda <account-name> [--<seed-arg> <value> ...]");
            eprintln!();
            eprintln!("Available PDA accounts:");
            for ix in &idl.instructions {
                for acc in &ix.accounts {
                    if acc.pda.is_some() {
                        eprintln!("  {} (in instruction: {})", acc.name, ix.name);
                    }
                }
            }
            std::process::exit(1);
        }
    };

    // Find account definition with PDA seeds and its owning instruction
    let found = idl.instructions.iter()
        .find_map(|ix| {
            ix.accounts.iter()
                .find(|acc| acc.name == account_name || snake_to_kebab(&acc.name) == account_name)
                .and_then(|acc| acc.pda.as_ref().map(|pda| (ix, pda)))
        });

    let (owning_ix, pda_def) = match found {
        Some(pair) => pair,
        None => {
            eprintln!("❌ No PDA account named '{}' found in IDL", account_name);
            eprintln!("   Available PDAs:");
            for ix in &idl.instructions {
                for acc in &ix.accounts {
                    if acc.pda.is_some() {
                        eprintln!("     {} ({})", acc.name, ix.name);
                    }
                }
            }
            std::process::exit(1);
        }
    };

    // Build a map from arg name to IDL type using the owning instruction's args
    let arg_types: HashMap<&str, &spel_framework_core::idl::IdlType> = owning_ix
        .args
        .iter()
        .map(|a| (a.name.as_str(), &a.type_))
        .collect();

    // Parse --key value pairs from remaining args, using IDL types when available
    let mut seed_args: HashMap<String, ParsedValue> = HashMap::new();
    let mut i = 1;
    while i < args.len() {
        if let Some(key) = args[i].strip_prefix("--") {
            if i + 1 < args.len() {
                let raw = &args[i + 1];
                let arg_name = key.replace('-', "_");
                let parsed = if let Some(ty) = arg_types.get(arg_name.as_str()) {
                    parse::parse_value(raw, ty).unwrap_or_else(|e| {
                        eprintln!("⚠️  Failed to parse --{} as {}: {}", key, format!("{:?}", ty), e);
                        ParsedValue::Str(raw.clone())
                    })
                } else {
                    ParsedValue::Str(raw.clone())
                };
                seed_args.insert(arg_name, parsed);
                i += 2;
            } else {
                eprintln!("❌ Missing value for --{}", key);
                std::process::exit(1);
            }
        } else {
            i += 1;
        }
    }

    // Get program_id: from global --program-id flag, or by loading the binary
    use nssa::program::Program;
    use crate::hex::decode_bytes_32;

    let program_id: nssa_core::program::ProgramId = if let Some(hex) = program_id_hex {
        let bytes = decode_bytes_32(hex).unwrap_or_else(|e| {
            eprintln!("❌ Invalid program ID '{}': {}", hex, e);
            std::process::exit(1);
        });
        let mut pid = [0u32; 8];
        for (i, chunk) in bytes.chunks(4).enumerate() {
            pid[i] = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        pid
    } else if let Some(path) = program_path {
        if std::path::Path::new(path).exists() {
            let program_bytes = std::fs::read(path).unwrap_or_else(|e| {
                eprintln!("❌ Cannot read program binary '{}': {}", path, e);
                std::process::exit(1);
            });
            Program::new(program_bytes).unwrap_or_else(|e| {
                eprintln!("❌ Invalid program binary: {:?}", e);
                std::process::exit(1);
            }).id()
        } else {
            eprintln!("❌ Program binary not found: {}", path);
            std::process::exit(1);
        }
    } else {
        eprintln!("❌ Program ID required to compute PDA.");
        eprintln!("   Pass --program <name>           (from spel.toml)");
        eprintln!("   Or   --program <64-char-hex>    (program ID)");
        eprintln!("   Or   --program <path-to-binary>");
        std::process::exit(1);
    };

    // Compute PDA
    match compute_pda_from_seeds(&pda_def.seeds, &program_id, &HashMap::new(), &seed_args) {
        Ok(account_id) => {
            println!("{}", account_id);
        }
        Err(e) => {
            eprintln!("❌ Failed to compute PDA: {}", e);
            eprintln!();
            eprintln!("Seeds for '{}':", account_name);
            for seed in &pda_def.seeds {
                match seed {
                    IdlSeed::Const { value } => eprintln!("  const: {:?}", value),
                    IdlSeed::Arg { path } => eprintln!("  arg: --{}", path.replace('_', "-")),
                    IdlSeed::Account { path } => eprintln!("  account: {}", path),
                }
            }
            std::process::exit(1);
        }
    }
}

/// Compute an arbitrary PDA from a program ID and raw seeds — no IDL required.
///
/// Usage: pda --program-id <64-char-hex> <seed1> [seed2] ...
///
/// Seeds can be:
///   - hex string (64 chars = 32 bytes)
///   - plain string (zero-padded to 32 bytes)
///
/// Output: base58 AccountId = SHA-256(PREFIX || program_id || SHA-256(seed1_32 || seed2_32 || ...))
///
/// Example:
///   multisig --program-id abc123... pda multisig_vault__ <create_key_hex>
fn compute_pda_raw(args: &[String]) {
    use crate::hex::decode_bytes_32;
    use nssa_core::program::{PdaSeed, ProgramId};
    use nssa::AccountId;

    // Parse --program-id
    let pid_hex = match args.windows(2).find(|w| w[0] == "--program-id") {
        Some(w) => &w[1],
        None => {
            eprintln!("Usage: pda --program-id <64-char-hex> <seed1> [seed2] ...");
            std::process::exit(1);
        }
    };

    let pid_bytes = decode_bytes_32(pid_hex).unwrap_or_else(|e| {
        eprintln!("❌ Invalid --program-id '{}': {}", pid_hex, e);
        std::process::exit(1);
    });
    let mut program_id: ProgramId = [0u32; 8];
    for (i, chunk) in pid_bytes.chunks(4).enumerate() {
        program_id[i] = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }

    // Collect seed args (everything that's not --program-id or its value)
    let mut seeds: Vec<[u8; 32]> = Vec::new();
    let mut skip_next = false;
    for arg in args {
        if skip_next { skip_next = false; continue; }
        if arg == "--program-id" { skip_next = true; continue; }
        if arg.starts_with("--") { continue; }

        // Try as 64-char hex first, then as zero-padded string
        let seed_bytes: [u8; 32] = if arg.len() == 64 && arg.chars().all(|c| c.is_ascii_hexdigit()) {
            decode_bytes_32(arg).unwrap_or_else(|e| {
                eprintln!("❌ Invalid hex seed '{}': {}", arg, e);
                std::process::exit(1);
            })
        } else {
            let mut bytes = [0u8; 32];
            let src = arg.as_bytes();
            if src.len() > 32 {
                eprintln!("❌ Seed '{}' is {} bytes, max 32", arg, src.len());
                std::process::exit(1);
            }
            bytes[..src.len()].copy_from_slice(src);
            bytes
        };
        seeds.push(seed_bytes);
    }

    if seeds.is_empty() {
        eprintln!("❌ At least one seed required");
        eprintln!("Usage: pda --program-id <hex> <seed1> [seed2] ...");
        std::process::exit(1);
    }

    // Combine seeds via SHA-256(seed1 || seed2 || ...)
    use risc0_zkvm::sha::{Impl, Sha256};
    let combined: [u8; 32] = if seeds.len() == 1 {
        seeds[0]
    } else {
        let mut input = Vec::with_capacity(seeds.len() * 32);
        for s in &seeds { input.extend_from_slice(s); }
        Impl::hash_bytes(&input).as_bytes().try_into().expect("SHA-256 is 32 bytes")
    };

    let pda_seed = PdaSeed::new(combined);
    let account_id = AccountId::from((&program_id, &pda_seed));
    println!("{}", account_id);
}

/// Check if a string is a 64-character hex program ID.
fn is_hex_program_id(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}
