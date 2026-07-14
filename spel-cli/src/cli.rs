//! CLI helpers: help text, argument parsing, string utilities.

use spel_framework_core::idl::{IdlInstruction, IdlType, SpelIdl};
use std::collections::HashMap;

/// Print help for all commands derived from the IDL.
pub fn print_help(idl: &SpelIdl, binary_name: &str) {
    println!("🔧 {} v{} — IDL-driven CLI", idl.name, idl.version);
    println!();
    println!("USAGE:");
    println!(
        "  {} <COMMAND> [ARGS]                  (with spel.toml)",
        binary_name
    );
    println!(
        "  {} [OPTIONS] -- <COMMAND> [ARGS]     (without spel.toml)",
        binary_name
    );
    println!();
    println!("OPTIONS:");
    println!("  -i, --idl <FILE>           IDL JSON file (or set in spel.toml)");
    println!("  -p, --program <NAME|HEX|FILE>");
    println!("                             Program name from spel.toml, 64-char hex program ID,");
    println!("                             or path to program binary (or set in spel.toml)");
    println!("  --dry-run[=text|json]      Resolve & print transaction without submitting (text default)");
    println!(
        "  --bin-<NAME> <FILE>        Additional program binary (auto-fills --<NAME>-program-id)"
    );
    println!();
    println!("COMMANDS:");
    println!("  program-id <FILE> [--format hex|json]  Extract ProgramId from ELF binary(ies)");
    println!("  inspect <ACCOUNT-ID> --type <TYPE>     Decode account data");
    println!("  generate-idl [PATH]        Generate IDL JSON (auto-detects methods/guest/src/bin/ if no path given)");
    println!("  idl                        Print IDL information");

    for ix in &idl.instructions {
        let cmd = snake_to_kebab(&ix.name);
        let args_desc: Vec<String> = ix
            .args
            .iter()
            .map(|a| {
                format!(
                    "--{} <{}>",
                    snake_to_kebab(&a.name),
                    idl_type_hint(&a.type_)
                )
            })
            .collect();
        let acct_desc: Vec<String> = ix
            .accounts
            .iter()
            .filter(|a| a.pda.is_none())
            .map(|a| format!("--{} <BASE58|HEX>", snake_to_kebab(&a.name)))
            .collect();
        let all_args: Vec<String> = args_desc.into_iter().chain(acct_desc).collect();
        println!("  {:<20} {}", cmd, all_args.join(" "));
    }
    println!();
    println!("TYPE FORMATS:");
    println!("  u128, u64, u32, u8    Decimal number");
    println!("  [u8; N]               Hex string (2*N hex chars) or UTF-8 string (≤N chars, right-padded)");
    println!("  [u32; 8] / program_id Comma-separated u32s: \"0,0,0,0,0,0,0,0\"");
    println!("  Vec<[u8; 32]>         Comma-separated hex strings: \"aabb...00,ccdd...00\"");
    println!("  Vec<String>           Repeat the flag, one element per occurrence: --foo a --foo b --foo c");
    println!();
    println!("CONFIG:");
    println!("  Create a spel.toml in your project root to avoid passing --idl and --program:");
    println!("    [program]");
    println!("    idl = \"my-project-idl.json\"");
    println!("    binary = \"path/to/program.bin\"");
    println!();
    println!("Auto-generated from IDL. Accounts marked as PDA are computed automatically.");
}

/// Print detailed help for a single instruction.
pub fn print_instruction_help(ix: &IdlInstruction) {
    println!(
        "📋 {} — {} account(s), {} arg(s)",
        ix.name,
        ix.accounts.len(),
        ix.args.len()
    );
    println!();
    println!("ACCOUNTS:");
    for acc in &ix.accounts {
        let mut flags = vec![];
        if acc.writable {
            flags.push("mut");
        }
        if acc.signer {
            flags.push("signer");
        }
        if acc.init {
            flags.push("init");
        }
        let flags_str = if flags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", flags.join(", "))
        };
        let pda_note = if acc.pda.is_some() {
            " (PDA — auto-computed)"
        } else {
            ""
        };
        println!("  {}{}{}", acc.name, flags_str, pda_note);
    }
    println!();
    println!("ARGS:");
    for arg in &ix.args {
        println!(
            "  --{:<25} {} ({}) — format: {}",
            snake_to_kebab(&arg.name),
            arg.name,
            idl_type_display(&arg.type_),
            idl_type_hint(&arg.type_)
        );
    }
    for acc in &ix.accounts {
        if acc.pda.is_none() {
            println!(
                "  --{:<25} Account ID for '{}'",
                snake_to_kebab(&acc.name),
                acc.name
            );
        }
    }
}

/// Parse CLI args for an instruction into a key → list-of-values map.
///
/// Each `--key value` occurrence is appended to the entry for `key`, so a
/// flag may be repeated.  Scalar args take the last value; `Vec<String>`
/// args consume every value supplied.  A bare `--flag` with no value is
/// only legal for boolean args (per the IDL) or the universal `--help`/
/// `-h`; for anything else we exit with a `missing value` error so a
/// missing CLI value can't be silently stored as the literal "true".
pub fn parse_instruction_args(
    args: &[String],
    ix: &IdlInstruction,
) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    let mut i = 0;
    while i < args.len() {
        if args[i].starts_with("--") {
            let key = args[i][2..].to_string();
            if i + 1 < args.len() && !args[i + 1].starts_with("--") {
                map.entry(key).or_default().push(args[i + 1].clone());
                i += 2;
            } else {
                // Bare flag (no value follows).  Only valid for bool args
                // or --help/-h; everything else is a user error.
                let is_bool = ix.args.iter().any(|a| {
                    snake_to_kebab(&a.name) == key
                        && matches!(&a.type_, IdlType::Primitive(p) if p == "bool")
                });
                let is_help = key == "help" || key == "h";
                if is_bool || is_help {
                    map.entry(key).or_default().push("true".to_string());
                } else {
                    eprintln!("❌ --{}: missing value", key);
                    std::process::exit(1);
                }
                i += 1;
            }
        } else {
            i += 1;
        }
    }

    if map.contains_key("help") || map.contains_key("h") {
        print_instruction_help(ix);
        std::process::exit(0);
    }

    map
}

// ─── String utilities ────────────────────────────────────────────

pub fn snake_to_kebab(s: &str) -> String {
    s.replace('_', "-")
}

pub fn to_pascal_case(s: &str) -> String {
    s.split('_')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                None => String::new(),
                Some(ch) => ch.to_uppercase().collect::<String>() + c.as_str(),
            }
        })
        .collect()
}

pub fn idl_type_display(ty: &IdlType) -> String {
    match ty {
        IdlType::Primitive(s) => s.clone(),
        IdlType::Vec { vec } => format!("Vec<{}>", idl_type_display(vec)),
        IdlType::Option { option } => format!("Option<{}>", idl_type_display(option)),
        IdlType::Defined { defined } => defined.clone(),
        IdlType::Array { array } => format!("[{}; {}]", idl_type_display(&array.0), array.1),
    }
}

pub fn idl_type_hint(ty: &IdlType) -> String {
    match ty {
        IdlType::Primitive(s) => match s.as_str() {
            "u8" | "u32" | "u64" | "u128" => "NUMBER".to_string(),
            "program_id" => "u32,u32,...(×8)".to_string(),
            "bool" => "true|false".to_string(),
            _ => s.to_uppercase(),
        },
        IdlType::Vec { vec } => match &**vec {
            IdlType::Array { array } => match &*array.0 {
                IdlType::Primitive(p) if p == "u8" => format!("HEX{},...", array.1 * 2),
                _ => "LIST".to_string(),
            },
            _ => "LIST".to_string(),
        },
        IdlType::Option { option } => format!("OPT<{}>", idl_type_hint(option)),
        IdlType::Defined { defined } => defined.clone(),
        IdlType::Array { array } => match &*array.0 {
            IdlType::Primitive(p) if p == "u8" => format!("HEX{}|STR≤{}", array.1 * 2, array.1),
            _ => format!("[_; {}]", array.1),
        },
    }
}
