//! Shared utility functions for code generation.

/// Convert a name to snake_case.
pub fn snake_case(s: &str) -> String {
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    collapse_underscores(&out)
}

/// Convert a name to PascalCase.
pub fn pascal_case(s: &str) -> String {
    let mut out = String::new();
    let mut upper = true;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            if upper {
                out.push(ch.to_ascii_uppercase());
                upper = false;
            } else {
                out.push(ch);
            }
        } else {
            upper = true;
        }
    }
    if out.is_empty() { "Program".to_string() } else { out }
}

/// Make a valid Rust identifier.
pub fn rust_ident(s: &str) -> String {
    let ident = snake_case(s);
    match ident.as_str() {
        "type" | "match" | "mod" | "enum" | "struct" | "fn" | "crate"
        | "self" | "super" | "pub" | "use" | "impl" | "trait" | "where"
        | "async" | "await" | "move" | "ref" | "mut" | "const" | "static"
        | "let" | "if" | "else" | "loop" | "while" | "for" | "in"
        | "return" | "break" | "continue" => format!("r#{}", ident),
        _ => ident,
    }
}

/// Map IDL type to Rust type string.
pub fn idl_type_to_rust(ty: &lez_framework_core::idl::IdlType) -> String {
    use lez_framework_core::idl::IdlType;
    match ty {
        IdlType::Primitive(p) => match p.as_str() {
            "account_id" | "AccountId" | "[u8; 32]" | "[u8;32]" => "AccountId".to_string(),
            "ProgramId" | "[u32; 8]" | "[u32;8]" => "ProgramId".to_string(),
            s => s.to_string(),
        },
        IdlType::Vec { vec } => format!("Vec<{}>", idl_type_to_rust(vec)),
        IdlType::Option { option } => format!("Option<{}>", idl_type_to_rust(option)),
        IdlType::Defined { defined } => defined.clone(),
        IdlType::Array { array: (elem, size) } => {
            format!("[{}; {}]", idl_type_to_rust(elem), size)
        }
    }
}

/// Map IDL type to a JSON parsing expression for FFI.
/// `var` is the expression to parse from (serde_json::Value).
pub fn idl_type_to_json_parse(ty: &lez_framework_core::idl::IdlType, var: &str) -> String {
    use lez_framework_core::idl::IdlType;
    match ty {
        IdlType::Primitive(p) => match p.as_str() {
            "account_id" | "AccountId" | "[u8; 32]" | "[u8;32]" => {
                format!("parse_account_id({var}.as_str().ok_or(\"expected string for AccountId\")?)?")
            }
            "ProgramId" | "[u32; 8]" | "[u32;8]" => {
                format!("parse_program_id({var}.as_str().ok_or(\"expected string for ProgramId\")?)?")
            }
            "String" => format!("{var}.as_str().ok_or(\"expected string\")?.to_string()"),
            "bool" => format!("{var}.as_bool().ok_or(\"expected bool\")?"),
            "u8" | "u16" | "u32" | "u64" | "u128" => {
                format!("{var}.as_u64().ok_or(\"expected number\")? as {p}")
            }
            "i8" | "i16" | "i32" | "i64" | "i128" => {
                format!("{var}.as_i64().ok_or(\"expected number\")? as {p}")
            }
            _ => format!("serde_json::from_value({var}.clone()).map_err(|e| format!(\"parse error: {{}}\", e))?"),
        },
        IdlType::Vec { vec } => {
            let inner = idl_type_to_json_parse(vec, "item");
            format!(
                "{var}.as_array().ok_or(\"expected array\")?.iter().map(|item| Ok({inner})).collect::<Result<Vec<_>, String>>()?"
            )
        }
        _ => format!("serde_json::from_value({var}.clone()).map_err(|e| format!(\"parse error: {{}}\", e))?"),
    }
}

fn collapse_underscores(s: &str) -> String {
    let mut out = String::new();
    let mut prev_underscore = false;
    for ch in s.chars() {
        if ch == '_' {
            if !prev_underscore {
                out.push('_');
                prev_underscore = true;
            }
        } else {
            out.push(ch);
            prev_underscore = false;
        }
    }
    out.trim_matches('_').to_string()
}
