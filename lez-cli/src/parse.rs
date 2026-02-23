//! IDL type-aware value parsing from CLI strings.

use lez_framework_core::idl::IdlType;
use crate::hex::{hex_decode, hex_encode};

/// A parsed CLI value with type information preserved.
#[derive(Debug, Clone)]
pub enum ParsedValue {
    Bool(bool),
    U8(u8),
    U32(u32),
    U64(u64),
    U128(u128),
    Str(String),
    ByteArray(Vec<u8>),         // [u8; N]
    U32Array(Vec<u32>),         // [u32; N] / ProgramId
    ByteArrayVec(Vec<Vec<u8>>), // Vec<[u8; 32]>
    None,                       // Option::None
    Some(Box<ParsedValue>),     // Option::Some
    Raw(String),                // fallback
}

impl std::fmt::Display for ParsedValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParsedValue::Bool(v) => write!(f, "{}", v),
            ParsedValue::U8(v) => write!(f, "{}", v),
            ParsedValue::U32(v) => write!(f, "{}", v),
            ParsedValue::U64(v) => write!(f, "{}", v),
            ParsedValue::U128(v) => write!(f, "{}", v),
            ParsedValue::Str(s) => write!(f, "\"{}\"", s),
            ParsedValue::ByteArray(bytes) => {
                if let Ok(s) = std::str::from_utf8(bytes) {
                    if s.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
                        let trimmed = s.trim_end_matches('\0');
                        return write!(f, "\"{}\" (hex: {})", trimmed, hex_encode(bytes));
                    }
                }
                write!(f, "0x{}", hex_encode(bytes))
            }
            ParsedValue::U32Array(vals) => {
                let strs: Vec<String> = vals.iter().map(|v| v.to_string()).collect();
                write!(f, "[{}]", strs.join(", "))
            }
            ParsedValue::ByteArrayVec(vecs) => {
                let strs: Vec<String> = vecs.iter().map(|v| format!("0x{}", hex_encode(v))).collect();
                write!(f, "[{}]", strs.join(", "))
            }
            ParsedValue::None => write!(f, "None"),
            ParsedValue::Some(inner) => write!(f, "Some({})", inner),
            ParsedValue::Raw(s) => write!(f, "{}", s),
        }
    }
}

/// Parse a CLI string value according to its IDL type.
pub fn parse_value(raw: &str, ty: &IdlType) -> Result<ParsedValue, String> {
    match ty {
        IdlType::Primitive(p) => parse_primitive(raw, p),
        IdlType::Array { array } => parse_array(raw, &array.0, array.1),
        IdlType::Vec { vec } => parse_vec(raw, vec),
        IdlType::Option { option } => {
            if raw == "none" || raw == "null" || raw.is_empty() {
                Ok(ParsedValue::None)
            } else {
                Ok(ParsedValue::Some(Box::new(parse_value(raw, option)?)))
            }
        }
        IdlType::Defined { defined } => Ok(ParsedValue::Raw(format!("{}({})", defined, raw))),
    }
}

fn parse_primitive(raw: &str, prim: &str) -> Result<ParsedValue, String> {
    match prim {
        "u8" => raw.parse::<u8>().map(ParsedValue::U8).map_err(|e| format!("Invalid u8 '{}': {}", raw, e)),
        "u32" => raw.parse::<u32>().map(ParsedValue::U32).map_err(|e| format!("Invalid u32 '{}': {}", raw, e)),
        "u64" => raw.parse::<u64>().map(ParsedValue::U64).map_err(|e| format!("Invalid u64 '{}': {}", raw, e)),
        "u128" => raw.parse::<u128>().map(ParsedValue::U128).map_err(|e| format!("Invalid u128 '{}': {}", raw, e)),
        "program_id" => parse_program_id(raw),
        "bool" => match raw {
            "true" | "1" | "yes" => Ok(ParsedValue::Bool(true)),
            "false" | "0" | "no" => Ok(ParsedValue::Bool(false)),
            _ => Err(format!("Invalid bool '{}': expected true/false", raw)),
        },
        "string" | "String" => Ok(ParsedValue::Str(raw.to_string())),
        other => Ok(ParsedValue::Raw(format!("{}({})", other, raw))),
    }
}

fn parse_program_id(raw: &str) -> Result<ParsedValue, String> {
    if raw.contains(',') {
        let parts: Vec<&str> = raw.split(',').map(|s| s.trim()).collect();
        if parts.len() != 8 {
            return Err(format!("ProgramId needs 8 u32 values, got {}", parts.len()));
        }
        let mut vals = Vec::with_capacity(8);
        for (i, p) in parts.iter().enumerate() {
            let v = if p.starts_with("0x") || p.starts_with("0X") {
                u32::from_str_radix(&p[2..], 16)
            } else {
                p.parse::<u32>()
            };
            vals.push(v.map_err(|e| format!("ProgramId[{}] invalid u32 '{}': {}", i, p, e))?);
        }
        Ok(ParsedValue::U32Array(vals))
    } else if raw.len() == 64 && raw.chars().all(|c| c.is_ascii_hexdigit()) {
        let bytes = hex_decode(raw)?;
        let mut vals = Vec::with_capacity(8);
        for chunk in bytes.chunks(4) {
            vals.push(u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        }
        Ok(ParsedValue::U32Array(vals))
    } else {
        Err(format!("Invalid ProgramId '{}': expected 8 comma-separated u32s or 64 hex chars", raw))
    }
}

fn parse_array(raw: &str, elem_type: &IdlType, size: usize) -> Result<ParsedValue, String> {
    match elem_type {
        IdlType::Primitive(p) if p == "u8" => {
            if raw.len() == size * 2 && raw.chars().all(|c| c.is_ascii_hexdigit()) {
                let bytes = hex_decode(raw)?;
                if bytes.len() != size {
                    return Err(format!("Expected {} bytes, got {}", size, bytes.len()));
                }
                Ok(ParsedValue::ByteArray(bytes))
            } else if raw.starts_with("0x") || raw.starts_with("0X") {
                let hex = &raw[2..];
                let bytes = hex_decode(hex)?;
                if bytes.len() != size {
                    return Err(format!("Expected {} bytes from hex, got {}", size, bytes.len()));
                }
                Ok(ParsedValue::ByteArray(bytes))
            } else {
                let str_bytes = raw.as_bytes();
                if str_bytes.len() > size {
                    return Err(format!("String '{}' is {} bytes, max {} for [u8; {}]", raw, str_bytes.len(), size, size));
                }
                let mut bytes = vec![0u8; size];
                bytes[..str_bytes.len()].copy_from_slice(str_bytes);
                Ok(ParsedValue::ByteArray(bytes))
            }
        }
        IdlType::Primitive(p) if p == "u32" => {
            let parts: Vec<&str> = raw.split(',').map(|s| s.trim()).collect();
            if parts.len() != size {
                return Err(format!("Expected {} u32 values, got {}", size, parts.len()));
            }
            let mut vals = Vec::with_capacity(size);
            for p in &parts {
                vals.push(p.parse::<u32>().map_err(|e| format!("Invalid u32 '{}': {}", p, e))?);
            }
            Ok(ParsedValue::U32Array(vals))
        }
        _ => Ok(ParsedValue::Raw(raw.to_string())),
    }
}

fn parse_vec(raw: &str, elem_type: &IdlType) -> Result<ParsedValue, String> {
    match elem_type {
        IdlType::Array { array } => match &*array.0 {
            IdlType::Primitive(p) if p == "u8" => {
                let size = array.1;
                if raw.is_empty() {
                    return Ok(ParsedValue::ByteArrayVec(vec![]));
                }
                let parts: Vec<&str> = raw.split(',').map(|s| s.trim()).collect();
                let mut result = Vec::with_capacity(parts.len());
                for (i, part) in parts.iter().enumerate() {
                    if size == 32 {
                        let bytes = crate::hex::decode_bytes_32(part)
                            .map_err(|e| format!("Element [{}]: {}", i, e))?;
                        result.push(bytes.to_vec());
                    } else {
                        let hex = part.strip_prefix("0x").or_else(|| part.strip_prefix("0X")).unwrap_or(part);
                        let bytes = hex_decode(hex).map_err(|e| format!("Element [{}]: {}", i, e))?;
                        if bytes.len() != size {
                            return Err(format!("Element [{}]: expected {} bytes, got {} from '{}'", i, size, bytes.len(), part));
                        }
                        result.push(bytes);
                    }
                }
                Ok(ParsedValue::ByteArrayVec(result))
            }
            _ => Ok(ParsedValue::Raw(raw.to_string())),
        },
        _ => Ok(ParsedValue::Raw(raw.to_string())),
    }
}
