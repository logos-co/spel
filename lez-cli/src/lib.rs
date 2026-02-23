//! Generic IDL-driven CLI library for LEZ programs.
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
pub mod cli;
pub mod init;

use cli::{print_help, parse_instruction_args, snake_to_kebab};
use init::init_project;
use inspect::inspect_binaries;
use tx::execute_instruction;
use lez_framework_core::idl::LezIdl;
use std::collections::HashMap;
use std::{env, fs, process};

/// Run the generic IDL-driven CLI. Call this from your program's main():
///
/// ```no_run
/// #[tokio::main]
/// async fn main() {
///     lez_cli::run().await;
/// }
/// ```
pub async fn run() {
    let args: Vec<String> = env::args().collect();

    let mut idl_path = String::new();
    let mut program_path = "program.bin".to_string();
    let mut dry_run = false;
    let mut extra_bins: HashMap<String, String> = HashMap::new();
    let mut remaining_args: Vec<String> = vec![args[0].clone()];
    let mut i = 1;

    while i < args.len() {
        match args[i].as_str() {
            "--idl" | "-i" => {
                i += 1;
                if i < args.len() { idl_path = args[i].clone(); }
            }
            "--program" | "-p" => {
                i += 1;
                if i < args.len() { program_path = args[i].clone(); }
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

    // Handle commands that don't need an IDL
    if let Some(cmd) = remaining_args.get(1).map(|s| s.as_str()) {
        match cmd {
            "init" => {
                let name = remaining_args.get(2).unwrap_or_else(|| {
                    eprintln!("Usage: {} init <project-name>", args[0]);
                    process::exit(1);
                });
                init_project(name);
                return;
            }
            "inspect" => {
                inspect_binaries(&remaining_args[2..]);
                return;
            }
            _ => {}
        }
    }

    if idl_path.is_empty() {
        eprintln!("Usage: {} --idl <IDL_FILE> <COMMAND> [ARGS]", args[0]);
        eprintln!();
        eprintln!("Commands that don't need --idl:");
        eprintln!("  init <name>              Scaffold a new LEZ project");
        eprintln!("  inspect <FILE> [FILE...]  Print ProgramId for ELF binary(ies)");
        eprintln!();
        eprintln!("For all other commands, provide an IDL JSON file.");
        process::exit(1);
    }

    let idl_content = match fs::read_to_string(&idl_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error reading IDL '{}': {}", idl_path, e);
            process::exit(1);
        }
    };
    let idl: LezIdl = serde_json::from_str(&idl_content).unwrap_or_else(|e| {
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
        Some("inspect") => {
            inspect_binaries(&remaining_args[2..]);
        }
        Some(cmd) => {
            let instruction = idl.instructions.iter().find(|ix| {
                snake_to_kebab(&ix.name) == cmd || ix.name == cmd
            });

            match instruction {
                Some(ix) => {
                    let cli_args = parse_instruction_args(&remaining_args[2..], ix);
                    execute_instruction(
                        &idl, ix, &cli_args, &program_path, dry_run, &extra_bins,
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
