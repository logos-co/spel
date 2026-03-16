//! Account data inspection: fetch from sequencer, borsh-decode using IDL types,
//! and pretty-print as JSON.

use lez_framework_core::idl::{IdlEnumVariant, IdlField, IdlType, IdlTypeDef, LezIdl};
use serde_json::{json, Value};
use std::process;

use crate::hex::{decode_bytes_32, hex_decode, hex_encode};

/// Inspect an on-chain account: fetch its data, borsh-decode it using the IDL
/// type definition, and print the result as JSON.
pub async fn inspect_account(
    account_id_str: &str,
    idl: &LezIdl,
    type_name: &str,
    data_hex: Option<&str>,
) {
    // Parse account ID (base58 or hex)
    let account_bytes = decode_bytes_32(account_id_str).unwrap_or_else(|e| {
        eprintln!("Invalid account ID '{}': {}", account_id_str, e);
        process::exit(1);
    });
    let account_id = nssa::AccountId::new(account_bytes);

    // Get raw account data: from --data flag or from sequencer
    let data = if let Some(hex) = data_hex {
        hex_decode(hex).unwrap_or_else(|e| {
            eprintln!("Invalid --data hex: {}", e);
            process::exit(1);
        })
    } else {
        fetch_account_data(account_id).await
    };

    eprintln!("Account: {}", account_id);
    eprintln!("Data:    {} bytes", data.len());
    eprintln!("Hex:     {}", hex_encode(&data));
    eprintln!();

    if data.is_empty() {
        eprintln!("Account data is empty (account may not exist or has no data).");
        process::exit(1);
    }

    // Find type definition in IDL
    let type_def = find_type_def(idl, type_name).unwrap_or_else(|| {
        eprintln!("Type '{}' not found in IDL.", type_name);
        eprintln!("Available account types:");
        for acc in &idl.accounts {
            eprintln!("  {}", acc.name);
        }
        process::exit(1);
    });

    // Borsh decode
    let mut cursor: &[u8] = &data;
    match decode_type_def(&mut cursor, type_def, idl) {
        Ok(value) => {
            let remaining = cursor.len();
            println!("{}", serde_json::to_string_pretty(&value).unwrap());
            if remaining > 0 {
                eprintln!("{} trailing bytes after decoding", remaining);
            }
        }
        Err(e) => {
            eprintln!("Borsh decode failed: {}", e);
            process::exit(1);
        }
    }
}

async fn fetch_account_data(account_id: nssa::AccountId) -> Vec<u8> {
    let wallet_core = wallet::WalletCore::from_env().unwrap_or_else(|e| {
        eprintln!("Failed to initialize wallet: {:?}", e);
        eprintln!("Set NSSA_WALLET_HOME_DIR or use --data <hex>");
        process::exit(1);
    });

    let account = wallet_core
        .get_account_public(account_id)
        .await
        .unwrap_or_else(|e| {
            eprintln!("Failed to fetch account {}: {:?}", account_id, e);
            process::exit(1);
        });

    account.data.to_vec()
}

fn find_type_def<'a>(idl: &'a LezIdl, name: &str) -> Option<&'a IdlTypeDef> {
    idl.accounts
        .iter()
        .find(|a| a.name == name)
        .map(|a| &a.type_)
}

// ── Borsh decoding from IDL types ────────────────────────────────────

fn decode_type_def(
    cursor: &mut &[u8],
    def: &IdlTypeDef,
    idl: &LezIdl,
) -> Result<Value, String> {
    match def.kind.as_str() {
        "struct" => decode_struct(cursor, &def.fields, idl),
        "enum" => decode_enum(cursor, &def.variants, idl),
        other => Err(format!("Unknown type kind: {}", other)),
    }
}

fn decode_struct(
    cursor: &mut &[u8],
    fields: &[IdlField],
    idl: &LezIdl,
) -> Result<Value, String> {
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
    idl: &LezIdl,
) -> Result<Value, String> {
    let variant_idx = read_u8(cursor)? as usize;
    if variant_idx >= variants.len() {
        return Err(format!(
            "Enum variant index {} out of range (max {})",
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

fn decode_borsh_value(
    cursor: &mut &[u8],
    ty: &IdlType,
    idl: &LezIdl,
) -> Result<Value, String> {
    match ty {
        IdlType::Primitive(name) => decode_primitive(cursor, name),
        IdlType::Array {
            array: (inner, len),
        } => {
            // [u8; N] → hex string
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
        }
        IdlType::Vec { vec: inner } => {
            let len = read_u32(cursor)? as usize;
            // Vec<u8> → hex string
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
        }
        IdlType::Option { option: inner } => {
            let tag = read_u8(cursor)?;
            match tag {
                0 => Ok(Value::Null),
                1 => decode_borsh_value(cursor, inner, idl),
                _ => Err(format!("Invalid Option tag: {}", tag)),
            }
        }
        IdlType::Defined { defined: name } => match find_type_def(idl, name) {
            Some(def) => decode_type_def(cursor, def, idl),
            None => Err(format!("Undefined type: {}", name)),
        },
    }
}

fn decode_primitive(cursor: &mut &[u8], name: &str) -> Result<Value, String> {
    match name {
        "u8" => Ok(json!(read_u8(cursor)?)),
        "u16" => Ok(json!(read_u16(cursor)?)),
        "u32" => Ok(json!(read_u32(cursor)?)),
        "u64" => {
            let v = read_u64(cursor)?;
            // Use string for u64 to avoid JSON precision loss
            Ok(json!(v.to_string()))
        }
        "u128" => {
            let v = read_u128(cursor)?;
            Ok(json!(v.to_string()))
        }
        "i8" => Ok(json!(read_u8(cursor)? as i8)),
        "i16" => Ok(json!(read_u16(cursor)? as i16)),
        "i32" => Ok(json!(read_u32(cursor)? as i32)),
        "i64" => {
            let v = read_u64(cursor)? as i64;
            Ok(json!(v.to_string()))
        }
        "i128" => {
            let v = read_u128(cursor)? as i128;
            Ok(json!(v.to_string()))
        }
        "bool" => Ok(json!(read_u8(cursor)? != 0)),
        "string" => {
            let len = read_u32(cursor)? as usize;
            let mut buf = vec![0u8; len];
            read_exact(cursor, &mut buf)?;
            let s = String::from_utf8(buf).map_err(|e| format!("Invalid UTF-8: {}", e))?;
            Ok(json!(s))
        }
        "program_id" => {
            // ProgramId is [u32; 8] = 32 bytes
            let mut buf = [0u8; 32];
            read_exact(cursor, &mut buf)?;
            Ok(json!(hex_encode(&buf)))
        }
        other => Err(format!("Unknown primitive type: {}", other)),
    }
}

// ── Cursor helpers ───────────────────────────────────────────────────

fn read_exact(cursor: &mut &[u8], buf: &mut [u8]) -> Result<(), String> {
    if cursor.len() < buf.len() {
        return Err(format!(
            "Unexpected end of data: need {} bytes, have {}",
            buf.len(),
            cursor.len()
        ));
    }
    buf.copy_from_slice(&cursor[..buf.len()]);
    *cursor = &cursor[buf.len()..];
    Ok(())
}

fn read_u8(cursor: &mut &[u8]) -> Result<u8, String> {
    let mut buf = [0u8; 1];
    read_exact(cursor, &mut buf)?;
    Ok(buf[0])
}

fn read_u16(cursor: &mut &[u8]) -> Result<u16, String> {
    let mut buf = [0u8; 2];
    read_exact(cursor, &mut buf)?;
    Ok(u16::from_le_bytes(buf))
}

fn read_u32(cursor: &mut &[u8]) -> Result<u32, String> {
    let mut buf = [0u8; 4];
    read_exact(cursor, &mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

fn read_u64(cursor: &mut &[u8]) -> Result<u64, String> {
    let mut buf = [0u8; 8];
    read_exact(cursor, &mut buf)?;
    Ok(u64::from_le_bytes(buf))
}

fn read_u128(cursor: &mut &[u8]) -> Result<u128, String> {
    let mut buf = [0u8; 16];
    read_exact(cursor, &mut buf)?;
    Ok(u128::from_le_bytes(buf))
}
