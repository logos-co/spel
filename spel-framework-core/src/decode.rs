//! Generic Borsh decoder for IDL-described types.
//!
//! Used by `spel inspect` to decode an actor's state (and later events and messages) into a
//! `serde_json::Value` without knowing the concrete Rust type at compile time.

use crate::idl::{IdlEnumVariant, IdlField, IdlType, IdlTypeDef, SpelIdl};
use base58::ToBase58;
use serde_json::{json, Value};

/// Decode an actor state as the IDL's state type. Every byte must be consumed.
///
/// # Errors
/// If the IDL declares no state type, or the bytes are not a value of it.
pub fn decode_state(data: &[u8], idl: &SpelIdl) -> Result<Value, String> {
    let state = idl
        .state
        .as_ref()
        .ok_or_else(|| format!("the '{}' IDL declares no state type", idl.name))?;
    decode_value(data, state, idl)
}

/// Decode `data` as the struct or enum named `type_name`. Every byte must be consumed.
///
/// # Errors
/// If the type is not in the IDL, or the bytes are not a value of it.
pub fn decode_named(data: &[u8], type_name: &str, idl: &SpelIdl) -> Result<Value, String> {
    if idl.type_def(type_name).is_none() {
        return Err(format!("type '{type_name}' not found in IDL"));
    }
    decode_value(
        data,
        &IdlType::Defined {
            defined: type_name.to_owned(),
        },
        idl,
    )
}

/// Decode `data` as a value of `ty`. Every byte must be consumed.
///
/// # Errors
/// If the bytes are not a value of `ty`, or bytes remain after it.
pub fn decode_value(data: &[u8], ty: &IdlType, idl: &SpelIdl) -> Result<Value, String> {
    let mut cursor: &[u8] = data;
    let value = decode_borsh_value(&mut cursor, ty, idl)?;
    if !cursor.is_empty() {
        return Err(format!("{} bytes remaining after decode", cursor.len()));
    }
    Ok(value)
}

fn decode_type_def(cursor: &mut &[u8], def: &IdlTypeDef, idl: &SpelIdl) -> Result<Value, String> {
    match def.kind.as_str() {
        "struct" => decode_fields(cursor, &def.fields, idl),
        "enum" => decode_enum(cursor, &def.variants, idl),
        other => Err(format!("unknown type kind: {other}")),
    }
}

/// Named fields become an object; positional (tuple) fields become the one value, or an array
/// of several.
fn decode_fields(cursor: &mut &[u8], fields: &[IdlField], idl: &SpelIdl) -> Result<Value, String> {
    let positional = fields.iter().all(IdlField::is_positional);
    let mut values = Vec::with_capacity(fields.len());
    for field in fields {
        let value = decode_borsh_value(cursor, &field.type_, idl)
            .map_err(|e| format!("field '{}': {}", field.name, e))?;
        values.push((field.name.clone(), value));
    }
    if !positional {
        return Ok(Value::Object(values.into_iter().collect()));
    }
    let mut values: Vec<Value> = values.into_iter().map(|(_, value)| value).collect();
    if values.len() == 1 {
        Ok(values.remove(0))
    } else {
        Ok(Value::Array(values))
    }
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
        return Ok(json!(variant.name));
    }
    let value = decode_fields(cursor, &variant.fields, idl)?;
    Ok(json!({ &variant.name: value }))
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
                Ok(json!(hex_encode(&buf)))
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
        IdlType::Defined { defined: name } => match idl.type_def(name) {
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
        "program_id" => {
            let mut buf = [0u8; 32];
            read_exact(cursor, &mut buf)?;
            Ok(json!(hex_encode(&buf)))
        },
        "account_id" => {
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

/// Base58, as `AccountId`'s `Display` and serde forms render it.
fn account_id_encode(bytes: &[u8]) -> String {
    bytes.to_base58()
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
