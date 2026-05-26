//! Generic Borsh decoder for IDL-described account types.
//!
//! Used by `spel inspect` and by generated FFI crates to decode raw on-chain
//! account data into a `serde_json::Value` without knowing the concrete Rust
//! type at compile time.

use crate::idl::{IdlEnumVariant, IdlField, IdlType, IdlTypeDef, SpelIdl};
use base58::ToBase58;
use serde_json::{json, Value};

/// Try to decode `data` as each account type defined in `idl.accounts`, in order.
/// Returns `(type_name, decoded_fields)` for the first type that deserialises
/// without error, or `None` if no type matches.
pub fn decode_account_data_try_all(data: &[u8], idl: &SpelIdl) -> Option<(String, Value)> {
    for acc in &idl.accounts {
        if let Ok(fields) = decode_account_data_exact(data, &acc.name, idl) {
            return Some((acc.name.clone(), fields));
        }
    }
    None
}

/// Like `decode_account_data` but requires all bytes to be consumed — prevents
/// false-positive matches when a shorter type decodes successfully from a prefix.
fn decode_account_data_exact(data: &[u8], type_name: &str, idl: &SpelIdl) -> Result<Value, String> {
    let type_def = find_type_def(idl, type_name)
        .ok_or_else(|| format!("type '{type_name}' not found in IDL"))?;
    let mut cursor: &[u8] = data;
    let value = decode_type_def(&mut cursor, type_def, idl)?;
    if !cursor.is_empty() {
        return Err(format!("{} bytes remaining after decode", cursor.len()));
    }
    Ok(value)
}

/// Decode `data` as the IDL type named `type_name` (searched in `idl.accounts`
/// then `idl.types`).  Returns the decoded value as a JSON object, or an error
/// string if the type is not found or the data cannot be decoded.
pub fn decode_account_data(data: &[u8], type_name: &str, idl: &SpelIdl) -> Result<Value, String> {
    let type_def = find_type_def(idl, type_name)
        .ok_or_else(|| format!("type '{type_name}' not found in IDL"))?;
    let mut cursor: &[u8] = data;
    decode_type_def(&mut cursor, type_def, idl)
}

fn find_type_def<'a>(idl: &'a SpelIdl, name: &str) -> Option<&'a IdlTypeDef> {
    idl.accounts
        .iter()
        .find(|a| a.name == name)
        .map(|a| &a.type_)
        .or_else(|| idl.types.iter().find(|t| t.name == name))
}

fn decode_type_def(cursor: &mut &[u8], def: &IdlTypeDef, idl: &SpelIdl) -> Result<Value, String> {
    match def.kind.as_str() {
        "struct" => decode_struct(cursor, &def.fields, idl),
        "enum" => decode_enum(cursor, &def.variants, idl),
        other => Err(format!("unknown type kind: {other}")),
    }
}

fn decode_struct(cursor: &mut &[u8], fields: &[IdlField], idl: &SpelIdl) -> Result<Value, String> {
    let mut map = serde_json::Map::new();
    for field in fields {
        let value = decode_borsh_value(cursor, &field.type_, idl)
            .map_err(|e| format!("field '{}': {}", field.name, e))?;
        map.insert(field.name.clone(), value);
    }
    Ok(Value::Object(map))
}

fn decode_enum(
    cursor: &mut &[u8],
    variants: &[IdlEnumVariant],
    idl: &SpelIdl,
) -> Result<Value, String> {
    let variant_idx = read_u8(cursor)? as usize;
    if variant_idx >= variants.len() {
        return Err(format!(
            "enum variant index {} out of range (max {})",
            variant_idx,
            variants.len() - 1
        ));
    }
    let variant = &variants[variant_idx];
    if variant.fields.is_empty() {
        Ok(json!(variant.name))
    } else {
        let mut map = serde_json::Map::new();
        for field in &variant.fields {
            let value = decode_borsh_value(cursor, &field.type_, idl)?;
            map.insert(field.name.clone(), value);
        }
        Ok(json!({ &variant.name: map }))
    }
}

fn decode_borsh_value(cursor: &mut &[u8], ty: &IdlType, idl: &SpelIdl) -> Result<Value, String> {
    match ty {
        IdlType::Primitive(name) => decode_primitive(cursor, name),
        IdlType::Array {
            array: (inner, len),
        } => {
            if matches!(inner.as_ref(), IdlType::Primitive(s) if s == "u8") {
                let mut buf = vec![0u8; *len];
                read_exact(cursor, &mut buf)?;
                if *len == 32 {
                    Ok(json!(account_id_encode(&buf)))
                } else {
                    Ok(json!(hex_encode(&buf)))
                }
            } else {
                let mut arr = Vec::with_capacity(*len);
                for _ in 0..*len {
                    arr.push(decode_borsh_value(cursor, inner, idl)?);
                }
                Ok(json!(arr))
            }
        },
        IdlType::Vec { vec: inner } => {
            let len = read_u32(cursor)? as usize;
            if matches!(inner.as_ref(), IdlType::Primitive(s) if s == "u8") {
                let mut buf = vec![0u8; len];
                read_exact(cursor, &mut buf)?;
                Ok(json!(hex_encode(&buf)))
            } else {
                let mut arr = Vec::with_capacity(len);
                for _ in 0..len {
                    arr.push(decode_borsh_value(cursor, inner, idl)?);
                }
                Ok(json!(arr))
            }
        },
        IdlType::Option { option: inner } => {
            let tag = read_u8(cursor)?;
            match tag {
                0 => Ok(Value::Null),
                1 => decode_borsh_value(cursor, inner, idl),
                _ => Err(format!("invalid Option tag: {tag}")),
            }
        },
        IdlType::Defined { defined: name } => match find_type_def(idl, name) {
            Some(def) => decode_type_def(cursor, def, idl),
            None => Err(format!("undefined type: {name}")),
        },
    }
}

fn decode_primitive(cursor: &mut &[u8], name: &str) -> Result<Value, String> {
    match name {
        "u8" => Ok(json!(read_u8(cursor)?)),
        "u16" => Ok(json!(read_u16(cursor)?)),
        "u32" => Ok(json!(read_u32(cursor)?)),
        "u64" => Ok(json!(read_u64(cursor)?.to_string())), // string to avoid JSON precision loss
        "u128" => Ok(json!(read_u128(cursor)?.to_string())),
        "i8" => Ok(json!(read_u8(cursor)? as i8)),
        "i16" => Ok(json!(read_u16(cursor)? as i16)),
        "i32" => Ok(json!(read_u32(cursor)? as i32)),
        "i64" => Ok(json!((read_u64(cursor)? as i64).to_string())),
        "i128" => Ok(json!((read_u128(cursor)? as i128).to_string())),
        "bool" => Ok(json!(read_u8(cursor)? != 0)),
        "string" => {
            let len = read_u32(cursor)? as usize;
            let mut buf = vec![0u8; len];
            read_exact(cursor, &mut buf)?;
            String::from_utf8(buf)
                .map(|s| json!(s))
                .map_err(|e| format!("invalid UTF-8: {e}"))
        },
        "program_id" | "ProgramId" | "[u32; 8]" | "[u32;8]" => {
            let mut buf = [0u8; 32];
            read_exact(cursor, &mut buf)?;
            Ok(json!(hex_encode(&buf)))
        },
        "account_id" | "AccountId" | "[u8; 32]" | "[u8;32]" => {
            let mut buf = [0u8; 32];
            read_exact(cursor, &mut buf)?;
            Ok(json!(account_id_encode(&buf)))
        },
        other => Err(format!("unknown primitive type: {other}")),
    }
}

// ── cursor helpers ────────────────────────────────────────────────────────────

fn read_exact(cursor: &mut &[u8], buf: &mut [u8]) -> Result<(), String> {
    if cursor.len() < buf.len() {
        return Err(format!(
            "unexpected end of data: need {} bytes, have {}",
            buf.len(),
            cursor.len()
        ));
    }
    buf.copy_from_slice(&cursor[..buf.len()]);
    *cursor = &cursor[buf.len()..];
    Ok(())
}

fn read_u8(cursor: &mut &[u8]) -> Result<u8, String> {
    let mut b = [0u8; 1];
    read_exact(cursor, &mut b)?;
    Ok(b[0])
}

fn read_u16(cursor: &mut &[u8]) -> Result<u16, String> {
    let mut b = [0u8; 2];
    read_exact(cursor, &mut b)?;
    Ok(u16::from_le_bytes(b))
}

fn read_u32(cursor: &mut &[u8]) -> Result<u32, String> {
    let mut b = [0u8; 4];
    read_exact(cursor, &mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn read_u64(cursor: &mut &[u8]) -> Result<u64, String> {
    let mut b = [0u8; 8];
    read_exact(cursor, &mut b)?;
    Ok(u64::from_le_bytes(b))
}

fn read_u128(cursor: &mut &[u8]) -> Result<u128, String> {
    let mut b = [0u8; 16];
    read_exact(cursor, &mut b)?;
    Ok(u128::from_le_bytes(b))
}

fn account_id_encode(bytes: &[u8]) -> String {
    format!("Public/{}", bytes.to_base58())
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
