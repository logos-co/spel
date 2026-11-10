//! CLI helpers: help text, message argument parsing and encoding.

use crate::parse::{parse_string_vec, parse_value, ParsedValue};
use crate::serialize::serialize_to_borsh;
use spel_framework_core::idl::{to_kebab, IdlMessage, IdlType, SpelIdl};
use std::collections::HashMap;

/// Print help for all commands derived from the IDL.
pub fn print_help(idl: &SpelIdl, binary_name: &str) {
    println!("🔧 {} — IDL v{}", idl.name, idl.version);
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
    println!("  -p, --program <NAME|ADDRESS>");
    println!("                             Program name from spel.toml or its address");
    println!();
    println!("COMMANDS:");
    println!("  program-id <FILE> [--format hex|json]  Extract the ImageID from program binaries");
    println!("  inspect <ACCOUNT-ID> --data <HEX> [--type <TYPE>]");
    println!("                             Decode actor state (the IDL's state type by default)");
    println!("  encode <MESSAGE> [ARGS]    Print a message's Borsh encoding as hex");
    println!("  generate-idl <CRATE_DIR> --message <ENUM> [--state <TYPE>] [--events <MODULE>]");
    println!("  idl                        Print the IDL");
    println!();
    println!("MESSAGES:");
    for message in &idl.messages {
        let fields: Vec<String> = message
            .fields
            .iter()
            .map(|f| format!("--{} <{}>", field_flag(&f.name), idl_type_hint(&f.type_)))
            .collect();
        println!("  {:<20} {}", to_kebab(&message.name), fields.join(" "));
    }
    println!();
    println!("TYPE FORMATS:");
    println!("  u128, u64, u32, u8    Decimal number");
    println!("  account_id            Base58 or 64-char hex");
    println!("  [u8; N]               Hex string (2*N hex chars) or UTF-8 string (≤N chars, right-padded)");
    println!("  Option<T>             The value, or `none`");
    println!("  struct                JSON object keyed by field name");
    println!("  enum                  Variant name, or {{\"Variant\": {{ ...fields }}}}");
    println!(
        "  Vec<String>           Repeat the flag, one element per occurrence: --foo a --foo b"
    );
}

/// Print detailed help for a single message.
pub fn print_message_help(message: &IdlMessage) {
    println!(
        "📋 {} (tag {}) — {} field(s)",
        message.name,
        message.tag,
        message.fields.len()
    );
    if let Some(docs) = &message.docs {
        println!();
        for line in docs.lines() {
            println!("  {line}");
        }
    }
    if message.fields.is_empty() {
        return;
    }
    println!();
    println!("FIELDS:");
    for field in &message.fields {
        println!(
            "  --{:<25} {} — format: {}",
            field_flag(&field.name),
            idl_type_display(&field.type_),
            idl_type_hint(&field.type_)
        );
        if let Some(docs) = &field.docs {
            println!("  {:<27} {}", "", docs.replace('\n', " "));
        }
    }
}

/// Parse CLI args for a message into a key → list-of-values map.
///
/// Each `--key value` occurrence is appended to the entry for `key`, so a
/// flag may be repeated.  Scalar fields take the last value; `Vec<String>`
/// fields consume every value supplied.  A bare `--flag` with no value is
/// only legal for boolean fields or the universal `--help`/`-h`; for
/// anything else we exit with a `missing value` error so a missing CLI
/// value can't be silently stored as the literal "true".
///
/// Keys outside the message's fields exit with an error: a global flag
/// misplaced after the `--` separator must not fall through.
pub fn parse_message_args(args: &[String], message: &IdlMessage) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    let mut i = 0;
    while i < args.len() {
        if let Some(key) = args[i].strip_prefix("--") {
            let key = key.to_string();
            if i + 1 < args.len() && !args[i + 1].starts_with("--") {
                map.entry(key).or_default().push(args[i + 1].clone());
                i += 2;
            } else {
                let is_bool = message.fields.iter().any(|f| {
                    field_flag(&f.name) == key
                        && matches!(&f.type_, IdlType::Primitive(p) if p == "bool")
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
        print_message_help(message);
        std::process::exit(0);
    }
    if let Err(msg) = reject_unknown_keys(&map, message) {
        eprintln!("{msg}");
        std::process::exit(1);
    }

    map
}

/// Encode `message` with the field values in `args` (from [`parse_message_args`]): the tag,
/// then each field in declaration order, as the program's Borsh `Message` enum.
///
/// # Errors
/// A missing field or a value that doesn't parse as its type.
pub fn encode_message(
    idl: &SpelIdl,
    message: &IdlMessage,
    args: &HashMap<String, Vec<String>>,
) -> Result<Vec<u8>, String> {
    let mut parsed: Vec<(&IdlType, ParsedValue)> = Vec::with_capacity(message.fields.len());
    for field in &message.fields {
        let flag = field_flag(&field.name);
        let values = args.get(&flag).map(Vec::as_slice).unwrap_or_default();
        let Some(last) = values.last() else {
            return Err(format!(
                "missing --{flag} ({})",
                idl_type_display(&field.type_)
            ));
        };
        let is_string_vec = matches!(&field.type_, IdlType::Vec { vec }
            if matches!(vec.as_ref(), IdlType::Primitive(p) if p == "string"));
        let value = if is_string_vec {
            parse_string_vec(values)
        } else {
            parse_value(last, &field.type_, &idl.types).map_err(|e| format!("--{flag}: {e}"))?
        };
        parsed.push((&field.type_, value));
    }
    let refs: Vec<(&IdlType, &ParsedValue)> = parsed.iter().map(|(t, v)| (*t, v)).collect();
    serialize_to_borsh(u32::from(message.tag), &refs).map_err(|e| e.to_string())
}

/// The CLI flag for a field: `total_supply` → `total-supply`.
pub fn field_flag(name: &str) -> String {
    name.replace('_', "-")
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

/// Rejects any `--key` that isn't one of the message's fields, kebab-case.
/// A miss is a typo or a misplaced global flag, and either must stop the run.
fn reject_unknown_keys(
    map: &HashMap<String, Vec<String>>,
    message: &IdlMessage,
) -> Result<(), String> {
    let known: std::collections::HashSet<String> =
        message.fields.iter().map(|f| field_flag(&f.name)).collect();
    match map.keys().find(|k| !known.contains(*k)) {
        Some(key) => Err(format!(
            "❌ --{key}: unknown argument for '{}'\n   \
            Global flags go before the '--' separator.",
            to_kebab(&message.name)
        )),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_idl() -> SpelIdl {
        SpelIdl::from_json(
            &serde_json::json!({
                "version": "2",
                "name": "t",
                "messages": [
                    { "name": "Transfer", "tag": 0, "fields": [
                        { "name": "to", "type": "account_id" },
                        { "name": "amount", "type": "u128" }
                    ] },
                    { "name": "SetMeta", "tag": 1, "fields": [
                        { "name": "meta", "type": { "defined": "Meta" } },
                        { "name": "owner", "type": { "option": "account_id" } }
                    ] }
                ],
                "types": [
                    { "name": "Kind", "kind": "enum", "variants": [ { "name": "A" }, { "name": "B" } ] },
                    { "name": "Meta", "kind": "struct", "fields": [
                        { "name": "kind", "type": { "defined": "Kind" } },
                        { "name": "uri", "type": "string" }
                    ] }
                ]
            })
            .to_string(),
        )
        .expect("fixture parses")
    }

    fn map_of(pairs: &[(&str, &str)]) -> HashMap<String, Vec<String>> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), vec![(*v).to_string()]))
            .collect()
    }

    #[test]
    fn fields_are_the_vocabulary() {
        let idl = fixture_idl();
        let transfer = idl.message("transfer").unwrap();
        assert!(reject_unknown_keys(&map_of(&[("to", "x"), ("amount", "1")]), transfer).is_ok());
        let err =
            reject_unknown_keys(&map_of(&[("to", "x"), ("export", "y")]), transfer).unwrap_err();
        assert!(err.contains("--export"), "{err}");
    }

    #[test]
    fn encodes_tag_then_fields() {
        let idl = fixture_idl();
        let to = "ab".repeat(32);
        let bytes = encode_message(
            &idl,
            idl.message("transfer").unwrap(),
            &map_of(&[("to", &to), ("amount", "300")]),
        )
        .unwrap();
        let mut expected = vec![0u8];
        expected.extend([0xab; 32]);
        expected.extend(300u128.to_le_bytes());
        assert_eq!(bytes, expected);
    }

    #[test]
    fn encodes_struct_and_option_fields() {
        let idl = fixture_idl();
        let bytes = encode_message(
            &idl,
            idl.message("set-meta").unwrap(),
            &map_of(&[
                ("meta", r#"{"kind":"B","uri":"ipfs://x"}"#),
                ("owner", "none"),
            ]),
        )
        .unwrap();
        let mut expected = vec![1u8, 1u8];
        expected.extend(8u32.to_le_bytes());
        expected.extend(b"ipfs://x");
        expected.push(0);
        assert_eq!(bytes, expected);
    }

    #[test]
    fn missing_field_is_named() {
        let idl = fixture_idl();
        let err = encode_message(
            &idl,
            idl.message("transfer").unwrap(),
            &map_of(&[("amount", "1")]),
        )
        .unwrap_err();
        assert!(err.contains("--to"), "{err}");
    }
}
