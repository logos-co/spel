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
pub fn idl_type_to_rust(ty: &spel_framework_core::idl::IdlType) -> String {
    use spel_framework_core::idl::IdlType;
    match ty {
        IdlType::Primitive(p) => match p.as_str() {
            "account_id" | "AccountId" | "[u8; 32]" | "[u8;32]" => "AccountId".to_string(),
            "ProgramId" | "[u32; 8]" | "[u32;8]" => "ProgramId".to_string(),
            "string" => "String".to_string(),
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
pub fn idl_type_to_json_parse(ty: &spel_framework_core::idl::IdlType, var: &str) -> String {
    use spel_framework_core::idl::IdlType;
    match ty {
        IdlType::Primitive(p) => match p.as_str() {
            "account_id" | "AccountId" | "[u8; 32]" | "[u8;32]" => {
                format!("parse_account_id({var}.as_str().ok_or(\"expected string for AccountId\")?)?")
            }
            "ProgramId" | "[u32; 8]" | "[u32;8]" => {
                format!("parse_program_id({var}.as_str().ok_or(\"expected string for ProgramId\")?)?")
            }
            "string" | "String" => format!("{var}.as_str().ok_or(\"expected string\")?.to_string()"),
            "bool" => format!("{var}.as_bool().ok_or(\"expected bool\")?"),
            "u8" | "u16" | "u32" => {
                format!("{var}.as_u64().ok_or(\"expected number\")? as {p}")
            }
            // u64 may arrive as a JSON string (from the generated Qt UI, which sends raw
            // text to avoid IEEE-754 precision loss) or as a JSON number (from CLI callers).
            "u64" => {
                format!("{{ let _v = &{var}; if let Some(_s) = _v.as_str() {{ _s.parse::<u64>().map_err(|_| format!(\"invalid u64: {{}}\", _s))? }} else {{ _v.as_u64().ok_or(\"expected u64\")? }} }}")
            }
            "u128" => {
                // Accept either a JSON string (for values > u64::MAX) or a JSON number.
                format!("{{ let _v = &{var}; if let Some(_s) = _v.as_str() {{ _s.parse::<u128>().map_err(|_| format!(\"invalid u128: {{}}\", _s))? }} else {{ _v.as_u64().ok_or(\"expected u128\")? as u128 }} }}")
            }
            "i8" | "i16" | "i32" => {
                format!("{var}.as_i64().ok_or(\"expected number\")? as {p}")
            }
            // i64 may arrive as a JSON string (same reason as u64).
            "i64" => {
                format!("{{ let _v = &{var}; if let Some(_s) = _v.as_str() {{ _s.parse::<i64>().map_err(|_| format!(\"invalid i64: {{}}\", _s))? }} else if let Some(_n) = _v.as_i64() {{ _n }} else {{ let _u = _v.as_u64().ok_or(\"expected i64\")?; if _u > i64::MAX as u64 {{ return Err(format!(\"i64 overflow: {{}}\", _u)); }} _u as i64 }} }}")
            }
            "i128" => {
                // Accept either a JSON string (for values outside i64 range) or a JSON number.
                format!("{{ let _v = &{var}; if let Some(_s) = _v.as_str() {{ _s.parse::<i128>().map_err(|_| format!(\"invalid i128: {{}}\", _s))? }} else if let Some(_n) = _v.as_i64() {{ _n as i128 }} else {{ _v.as_u64().ok_or(\"expected i128\")? as i128 }} }}")
            }
            _ => format!("serde_json::from_value({var}.clone()).map_err(|e| format!(\"parse error: {{}}\", e))?"),
        },
        IdlType::Vec { vec } => {
            let inner = idl_type_to_json_parse(vec, "item");
            format!(
                "{var}.as_array().ok_or(\"expected array\")?.iter().map(|item| Ok({inner})).collect::<Result<Vec<_>, String>>()?"
            )
        }
        IdlType::Array { array: (elem, 32) } if matches!(elem.as_ref(), IdlType::Primitive(p) if p == "u8") => {
            // [u8; 32]: accept base58, hex, or raw hex — use parse_bytes32 (not parse_account_id)
            // so the result type is [u8; 32], matching instruction enum field types.
            format!("parse_bytes32({var}.as_str().ok_or(\"expected string for [u8; 32]\")?)?")
        }
        _ => format!("serde_json::from_value({var}.clone()).map_err(|e| format!(\"parse error: {{}}\", e))?"),
    }
}

/// Returns true if `ty` is a `Vec` whose element type is a 32-byte value
/// (`[u8; 32]`, `AccountId`, or any spelling thereof).
pub fn is_vec_bytes32(ty: &spel_framework_core::idl::IdlType) -> bool {
    use spel_framework_core::idl::IdlType;
    if let IdlType::Vec { vec } = ty {
        matches!(vec.as_ref(),
            IdlType::Primitive(p) if matches!(p.as_str(),
                "account_id" | "AccountId" | "[u8; 32]" | "[u8;32]")
        ) || matches!(vec.as_ref(),
            IdlType::Array { array: (elem, 32) } if matches!(elem.as_ref(),
                IdlType::Primitive(p) if p == "u8")
        )
    } else {
        false
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
