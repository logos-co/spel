//! CLI helpers: help text, argument parsing, string utilities.

use std::collections::HashMap;
use spel_framework_core::idl::{IdlType, IdlInstruction, SpelIdl};

/// Print help for all commands derived from the IDL.
pub fn print_help(idl: &SpelIdl, binary_name: &str) {
    println!("🔧 {} v{} — IDL-driven CLI", idl.name, idl.version);
    println!();
    println!("USAGE:");
    println!("  {} [OPTIONS] <COMMAND> [ARGS]", binary_name);
    println!();
    println!("OPTIONS:");
    println!("  -i, --idl <FILE>           IDL JSON file");
    println!("  -p, --program <FILE>       Program binary");
    println!("  --dry-run                  Print parsed/serialized data without submitting");
    println!("  --bin-<NAME> <FILE>        Additional program binary (auto-fills --<NAME>-program-id)");
    println!();
    println!("COMMANDS:");
    println!("  inspect <FILE> [FILE...]   Print ProgramId for ELF binary(ies)");
    println!("  generate-idl [PATH]        Generate IDL JSON (auto-detects methods/guest/src/bin/ if no path given)");
    println!("  idl                        Print IDL information");

    for ix in &idl.instructions {
        let cmd = snake_to_kebab(&ix.name);
        let args_desc: Vec<String> = ix.args.iter()
            .map(|a| format!("--{} <{}>", snake_to_kebab(&a.name), idl_type_hint(&a.type_)))
            .collect();
        let acct_desc: Vec<String> = ix.accounts.iter()
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
    println!();
    println!("Auto-generated from IDL. Accounts marked as PDA are computed automatically.");
}

/// Print detailed help for a single instruction.
pub fn print_instruction_help(ix: &IdlInstruction) {
    println!("📋 {} — {} account(s), {} arg(s)", ix.name, ix.accounts.len(), ix.args.len());
    println!();
    println!("ACCOUNTS:");
    for acc in &ix.accounts {
        let mut flags = vec![];
        if acc.writable { flags.push("mut"); }
        if acc.signer { flags.push("signer"); }
        if acc.init { flags.push("init"); }
        let flags_str = if flags.is_empty() { String::new() } else { format!(" [{}]", flags.join(", ")) };
        let pda_note = if acc.pda.is_some() { " (PDA — auto-computed)" } else { "" };
        println!("  {}{}{}", acc.name, flags_str, pda_note);
    }
    println!();
    println!("ARGS:");
    for arg in &ix.args {
        println!("  --{:<25} {} ({}) — format: {}",
            snake_to_kebab(&arg.name), arg.name, idl_type_display(&arg.type_), idl_type_hint(&arg.type_));
    }
    for acc in &ix.accounts {
        if acc.pda.is_none() {
            println!("  --{:<25} Account ID for '{}'", snake_to_kebab(&acc.name), acc.name);
        }
    }
}

/// Parse CLI args for an instruction into a key-value map.
pub fn parse_instruction_args(args: &[String], ix: &IdlInstruction) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut i = 0;
    while i < args.len() {
        if args[i].starts_with("--") {
            let key = args[i][2..].to_string();
            if i + 1 < args.len() && !args[i + 1].starts_with("--") {
                map.insert(key, args[i + 1].clone());
                i += 2;
            } else {
                map.insert(key, "true".to_string());
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
