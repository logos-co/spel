//! Generic IDL-driven CLI library for SPEL programs.
//!
//! Provides:
//! - IDL parsing and type-aware argument handling
//! - risc0-compatible serialization
//! - PDA computation from raw seeds
//! - Binary inspection (ProgramId extraction)
//! - Program deployment
//!
//! Sending messages is being ported to LEZ's actor model (`spel send`); until then a
//! message name after `--` reports that, and `encode` prints a message's bytes.
//!
//! Use `run()` for a complete CLI entry point, or import individual modules.

pub mod account_inspect;
pub mod cli;
pub mod config;
pub mod deploy;
pub mod generate_idl;
pub mod hex;
pub mod inspect;
pub mod parse;
pub mod serialize;

use cli::{encode_message, parse_message_args, print_help};
use config::SpelConfig;
use inspect::inspect_binaries;
use spel_framework_core::idl::SpelIdl;
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
    let mut type_name: Option<String> = None;
    let mut data_hex: Option<String> = None;
    let mut inspect_format: Option<String> = None;
    let mut fee_payer: Option<String> = None;
    let mut remaining_args: Vec<String> = vec![args[0].clone()];
    let mut i = 1;

    while i < args.len() {
        match args[i].as_str() {
            "--" => {
                // Everything after `--` is passed through as instruction args
                remaining_args.extend_from_slice(&args[i + 1..]);
                break;
            },
            "--idl" | "-i" => {
                i += 1;
                if i < args.len() {
                    idl_path = args[i].clone();
                }
            },
            "--program" | "-p" => {
                i += 1;
                if i < args.len() {
                    program_ref = Some(args[i].clone());
                }
            },
            "--program-id" => {
                eprintln!("⚠️  --program-id is deprecated. Use --program <HEX> instead.");
                i += 1;
                if i < args.len() {
                    program_ref = Some(args[i].clone());
                }
            },
            "--type" | "-t" => {
                i += 1;
                if i < args.len() {
                    type_name = Some(args[i].clone());
                }
            },
            "--data" | "-d" => {
                i += 1;
                if i < args.len() {
                    data_hex = Some(args[i].clone());
                }
            },
            "--format" => {
                i += 1;
                if i >= args.len() || args[i].starts_with('-') {
                    eprintln!("❌ --format requires a value: text, hex, or json");
                    process::exit(1);
                }
                inspect_format = Some(args[i].clone());
            },
            s if s.starts_with("--format=") => {
                let val = &s["--format=".len()..];
                if val.is_empty() {
                    eprintln!("❌ --format requires a value: text, hex, or json");
                    process::exit(1);
                }
                inspect_format = Some(val.to_string());
            },
            "--fee-payer" => {
                i += 1;
                if i >= args.len() || args[i].starts_with('-') {
                    eprintln!("❌ --fee-payer requires an account id");
                    process::exit(1);
                }
                fee_payer = Some(args[i].clone());
            },
            _ => remaining_args.push(args[i].clone()),
        }
        i += 1;
    }

    // Load spel.toml config
    let config = env::current_dir()
        .ok()
        .and_then(|cwd| SpelConfig::discover(&cwd));

    // Resolve --program value: config name → program account address.
    //
    // Since LEZ v0.2.5 a program is identified by its deployed header account, which
    // cannot be derived from the binary — so a path is no longer a way to name one.
    // The binary is still resolved alongside, as `spel deploy`'s default.
    let mut program_path: Option<String> = None;
    let mut program_address: Option<String> = None;

    if let Some(ref value) = program_ref {
        let resolved_from_config = config.as_ref().and_then(|(_, cfg)| {
            if cfg.has_program(value) {
                cfg.resolve_program(Some(value)).ok()
            } else {
                None
            }
        });

        if let Some(prog) = resolved_from_config {
            // Config name → address, IDL and binary from the config entry
            let config_dir = config.as_ref().unwrap().0.parent().unwrap();
            if idl_path.is_empty() {
                if let Some(ref idl) = prog.idl {
                    idl_path = config_dir.join(idl).to_string_lossy().to_string();
                }
            }
            program_address = prog.address.clone();
            program_path = prog
                .binary
                .as_ref()
                .map(|b| config_dir.join(b).to_string_lossy().to_string());
        } else if std::path::Path::new(value).exists() {
            eprintln!("❌ --program takes a program account address, not a binary path.");
            eprintln!("   Since LEZ v0.2.5 a program is identified by the header account it was");
            eprintln!("   deployed to, which is chosen at deploy time and is not derivable from");
            eprintln!("   '{}'.", value);
            eprintln!(
                "   Pass --program <address>, or name a [programs.<name>] entry in spel.toml"
            );
            eprintln!("   carrying `address` (and `binary` for privacy-preserving calls).");
            process::exit(1);
        } else {
            program_address = Some(value.clone());
        }
    }

    // Fill gaps from config default program (when --program not given or didn't resolve IDL)
    if let Some((ref config_path, ref cfg)) = config {
        if program_ref.is_none() {
            if let Ok(prog) = cfg.resolve_program(None) {
                let config_dir = config_path.parent().unwrap();
                if idl_path.is_empty() {
                    if let Some(ref idl) = prog.idl {
                        idl_path = config_dir.join(idl).to_string_lossy().to_string();
                    }
                }
                if program_address.is_none() {
                    program_address = prog.address.clone();
                }
                if program_path.is_none() {
                    program_path = prog
                        .binary
                        .as_ref()
                        .map(|b| config_dir.join(b).to_string_lossy().to_string());
                }
            }
        }
    }

    // Handle commands that don't need an IDL
    if let Some(cmd) = remaining_args.get(1).map(|s| s.as_str()) {
        match cmd {
            "program-id" => {
                inspect_binaries(&remaining_args[2..], inspect_format.as_deref());
                return;
            },
            "inspect" => {
                if idl_path.is_empty() {
                    eprintln!("Account inspection requires --idl <IDL_FILE>");
                    process::exit(1);
                }
                let account_id = remaining_args.get(2).unwrap_or_else(|| {
                    eprintln!("{}", account_inspect::USAGE);
                    process::exit(1);
                });
                let idl = load_idl(&idl_path);
                account_inspect::inspect_account(
                    account_id,
                    &idl,
                    type_name.as_deref(),
                    data_hex.as_deref(),
                )
                .await;
                return;
            },
            "generate-idl" => {
                let options = generate_idl::parse_args(&remaining_args[2..]).unwrap_or_else(|e| {
                    eprintln!("❌ {e}");
                    eprintln!("{}", generate_idl::USAGE);
                    process::exit(1);
                });
                match spel_framework_core::idl_gen::generate_idl(&options, |w| eprintln!("⚠️  {w}"))
                {
                    Ok(idl) => println!("{}", idl.to_json_pretty().unwrap()),
                    Err(e) => {
                        eprintln!("❌ {e}");
                        process::exit(1);
                    },
                }
                return;
            },
            "pda" if program_address.is_some() => {
                // Usage: <bin> --program <address> pda <seed1> [seed2] ...
                let mut raw_args =
                    vec!["--program-id".to_string(), program_address.clone().unwrap()];
                raw_args.extend_from_slice(&remaining_args[2..]);
                compute_pda_raw(&raw_args);
                return;
            },
            "deploy" => {
                let mut immutable = false;
                let mut binary: Option<String> = None;
                for arg in &remaining_args[2..] {
                    match arg.as_str() {
                        "--immutable" => immutable = true,
                        "-h" | "--help" => {
                            println!(
                                "Usage: spel deploy [BINARY] --fee-payer <ADDRESS> [--immutable]"
                            );
                            println!();
                            println!("Deploy a program through LEZ's program_loader: create a header account");
                            println!("and the segment accounts the binary needs in the wallet, upload it, and");
                            println!("print the program address (the header account) on stdout.");
                            println!();
                            println!("BINARY defaults to the spel.toml program's `binary`. The new accounts");
                            println!("hold nothing, so --fee-payer must name a funded account in the wallet.");
                            return;
                        },
                        s if s.starts_with('-') => {
                            eprintln!("❌ spel deploy: unknown option '{}'", s);
                            process::exit(1);
                        },
                        s if binary.is_none() => binary = Some(s.to_string()),
                        s => {
                            eprintln!("❌ spel deploy takes one binary, got another: '{}'", s);
                            process::exit(1);
                        },
                    }
                }
                let binary = binary.or(program_path.clone()).unwrap_or_else(|| {
                    eprintln!(
                        "Usage: {} deploy [BINARY] --fee-payer <ADDRESS> [--immutable]",
                        args[0]
                    );
                    eprintln!("   (no BINARY given and no spel.toml program with a `binary`)");
                    process::exit(1);
                });
                deploy::deploy_command(
                    std::path::Path::new(&binary),
                    fee_payer.as_deref(),
                    immutable,
                )
                .await;
                return;
            },
            _ => {},
        }
    }

    if idl_path.is_empty() {
        eprintln!("Usage: {} [OPTIONS] -- <COMMAND> [ARGS]", args[0]);
        eprintln!();
        eprintln!(
            "Tip: create a spel.toml with [program] or [programs.<name>] to avoid passing flags."
        );
        eprintln!();
        eprintln!("Commands that don't need --idl:");
        eprintln!("  deploy [BINARY] --fee-payer <ADDRESS>  Deploy a program, print its address");
        eprintln!(
            "  program-id <FILE> [FILE...]  Extract ProgramId from program .bin (R0BF) binary(ies)"
        );
        eprintln!("  inspect <ACCOUNT-ID> --idl <IDL> --data <HEX> [--type <TYPE>]");
        eprintln!("                           Decode actor state");
        eprintln!(
            "  generate-idl <CRATE_DIR> --message <ENUM> [--state <TYPE>] [--events <MODULE>]"
        );
        eprintln!();
        eprintln!("  pda --program <HEX> <SEED> [SEED...]  Compute a public PDA from raw seeds");
        eprintln!(
            "encode and idl need an IDL: pass --idl or name a spel.toml program with --program."
        );
        process::exit(1);
    }

    let idl = load_idl(&idl_path);

    let subcmd = remaining_args.get(1).map(|s| s.as_str());
    let binary_name = std::path::Path::new(&args[0])
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| args[0].clone());

    match subcmd {
        Some("--help") | Some("-h") | None => {
            print_help(&idl, &binary_name);
        },
        Some("idl") => {
            println!("{}", idl.to_json_pretty().unwrap());
        },
        Some("program-id") => {
            inspect_binaries(&remaining_args[2..], inspect_format.as_deref());
        },
        Some("encode") => {
            let name = remaining_args.get(2).unwrap_or_else(|| {
                eprintln!("Usage: {} encode <MESSAGE> [--field value ...]", args[0]);
                process::exit(1);
            });
            let message = idl.message(name).unwrap_or_else(|| {
                eprintln!("❌ No message '{}' in the IDL", name);
                print_help(&idl, &binary_name);
                process::exit(1);
            });
            let values = parse_message_args(&remaining_args[3..], message);
            match encode_message(&idl, message, &values) {
                Ok(bytes) => println!("{}", hex::hex_encode(&bytes)),
                Err(e) => {
                    eprintln!("❌ {e}");
                    process::exit(1);
                },
            }
        },
        Some("pda") => {
            eprintln!("❌ spel pda derives from raw seeds and needs the program's address:");
            eprintln!("   spel --program <address> pda <seed1> [seed2] ...");
            process::exit(1);
        },
        Some(cmd) => match idl.message(cmd) {
            Some(message) => {
                parse_message_args(&remaining_args[2..], message);
                eprintln!(
                    "❌ Cannot send '{}': sending messages is being ported to LEZ's actor model.",
                    message.name
                );
                eprintln!("   `spel encode` prints the message's bytes.");
                process::exit(1);
            },
            None => {
                eprintln!("Unknown command: {}", cmd);
                print_help(&idl, &binary_name);
                process::exit(1);
            },
        },
    }
}

/// Read and parse an IDL file, exiting with a message on failure.
fn load_idl(path: &str) -> SpelIdl {
    let content = fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("Error reading IDL '{}': {}", path, e);
        process::exit(1);
    });
    SpelIdl::from_json(&content).unwrap_or_else(|e| {
        eprintln!("❌ {e}");
        process::exit(1);
    })
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
    use lee::AccountId;
    use lee_core::program::PdaSeed;

    // Parse --program-id (the program's deployed account, since LEZ v0.2.5)
    let pid_hex = match args.windows(2).find(|w| w[0] == "--program-id") {
        Some(w) => &w[1],
        None => {
            eprintln!("Usage: pda --program-id <address> <seed1> [seed2] ...");
            process::exit(1);
        },
    };

    let pid_bytes = spel_framework_core::pda::parse_bytes32(pid_hex).unwrap_or_else(|e| {
        eprintln!("❌ Invalid program address '{}': {}", pid_hex, e);
        process::exit(1);
    });
    let program_account_id = AccountId::new(pid_bytes);

    // Collect seed args (everything that's not --program-id or its value)
    let mut seeds: Vec<[u8; 32]> = Vec::new();
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if arg == "--program-id" {
            skip_next = true;
            continue;
        }
        if arg.starts_with("--") {
            continue;
        }

        // Try as 64-char hex first, then as zero-padded string
        let seed_bytes: [u8; 32] = if arg.len() == 64 && arg.chars().all(|c| c.is_ascii_hexdigit())
        {
            decode_bytes_32(arg).unwrap_or_else(|e| {
                eprintln!("❌ Invalid hex seed '{}': {}", arg, e);
                process::exit(1);
            })
        } else {
            let mut bytes = [0u8; 32];
            let src = arg.as_bytes();
            if src.len() > 32 {
                eprintln!("❌ Seed '{}' is {} bytes, max 32", arg, src.len());
                process::exit(1);
            }
            bytes[..src.len()].copy_from_slice(src);
            bytes
        };
        seeds.push(seed_bytes);
    }

    if seeds.is_empty() {
        eprintln!("❌ At least one seed required");
        eprintln!("Usage: pda --program-id <address> <seed1> [seed2] ...");
        process::exit(1);
    }

    // Combine seeds via SHA-256(seed1 || seed2 || ...)
    use risc0_zkvm::sha::{Impl, Sha256};
    let combined: [u8; 32] = if seeds.len() == 1 {
        seeds[0]
    } else {
        let mut input = Vec::with_capacity(seeds.len() * 32);
        for s in &seeds {
            input.extend_from_slice(s);
        }
        Impl::hash_bytes(&input)
            .as_bytes()
            .try_into()
            .expect("SHA-256 is 32 bytes")
    };

    let pda_seed = PdaSeed::new(combined);
    let account_id = AccountId::for_public_pda(&program_account_id, &pda_seed);
    println!("{}", account_id);
}
