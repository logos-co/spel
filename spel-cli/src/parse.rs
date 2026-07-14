//! IDL type-aware value parsing from CLI strings.

use crate::hex::{hex_decode, hex_encode};
use spel_framework_core::idl::IdlType;

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
    StringVec(Vec<String>),     // Vec<String>
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
            },
            ParsedValue::U32Array(vals) => {
                let strs: Vec<String> = vals.iter().map(|v| v.to_string()).collect();
                write!(f, "[{}]", strs.join(", "))
            },
            ParsedValue::ByteArrayVec(vecs) => {
                let strs: Vec<String> = vecs
                    .iter()
                    .map(|v| format!("0x{}", hex_encode(v)))
                    .collect();
                write!(f, "[{}]", strs.join(", "))
            },
            ParsedValue::StringVec(strs) => {
                let quoted: Vec<String> = strs.iter().map(|s| format!("\"{}\"", s)).collect();
                write!(f, "[{}]", quoted.join(", "))
            },
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
        },
        IdlType::Defined { defined } => Ok(ParsedValue::Raw(format!("{}({})", defined, raw))),
    }
}

fn parse_primitive(raw: &str, prim: &str) -> Result<ParsedValue, String> {
    match prim {
        "u8" => raw
            .parse::<u8>()
            .map(ParsedValue::U8)
            .map_err(|e| format!("Invalid u8 '{}': {}", raw, e)),
        "u32" => raw
            .parse::<u32>()
            .map(ParsedValue::U32)
            .map_err(|e| format!("Invalid u32 '{}': {}", raw, e)),
        "u64" => raw
            .parse::<u64>()
            .map(ParsedValue::U64)
            .map_err(|e| format!("Invalid u64 '{}': {}", raw, e)),
        "u128" => raw
            .parse::<u128>()
            .map(ParsedValue::U128)
            .map_err(|e| format!("Invalid u128 '{}': {}", raw, e)),
        "program_id" => parse_program_id(raw),
        // `AccountId` serializes via `SerializeDisplay` (base58 string), so normalize the
        // input (base58 or 0x-hex) to canonical base58 and carry it as a string.
        "account_id" => {
            let bytes = crate::hex::decode_bytes_32(raw)?;
            Ok(ParsedValue::Str(nssa::AccountId::new(bytes).to_string()))
        },
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
        // base58 (or 0x-prefixed hex) ImageID → little-endian u32 limbs, matching the bare-hex
        // branch above so all representations of the same ProgramId agree.
        //
        // #243: `decode_bytes_32` strips `Public/`/`Private/` prefixes and accepts any
        // base58 string that decodes to 32 bytes. Left unchecked that silently reinterprets
        // an account id (or a mistyped value) as a ProgramId and builds the transaction
        // against the wrong program with no diagnostic. A ProgramId is an ImageID — never
        // account-prefixed — so reject account prefixes, and require base58 input to be
        // canonical (re-encode to exactly the input) instead of accepting junk that merely
        // happens to decode to 32 bytes.
        if raw.starts_with("Public/") || raw.starts_with("Private/") {
            return Err(format!(
                "Invalid ProgramId '{}': that is an account id (Public/Private prefix). A ProgramId \
                 is an ImageID: 8 comma-separated u32s, a 64-char hex string, or base58.",
                raw
            ));
        }
        let bytes = crate::hex::decode_bytes_32(raw).map_err(|_| {
            format!(
                "Invalid ProgramId '{}': expected 8 comma-separated u32s, a 64-char hex ImageID, or base58",
                raw
            )
        })?;
        // Hex is unambiguous (fixed 64-char length, validated on decode); base58 is dense,
        // so require the decoded bytes to re-encode to exactly the input — rejecting
        // non-canonical / accidentally-32-byte-decodable strings rather than accepting them.
        let hex_body = raw
            .strip_prefix("0x")
            .or_else(|| raw.strip_prefix("0X"))
            .unwrap_or(raw);
        let is_hex = hex_body.len() == 64 && hex_body.bytes().all(|b| b.is_ascii_hexdigit());
        if !is_hex {
            use base58::ToBase58;
            let canonical = bytes.to_base58();
            if canonical != raw {
                return Err(format!(
                    "Invalid ProgramId '{}': not a canonical base58 ImageID (its 32 bytes re-encode to '{}')",
                    raw, canonical
                ));
            }
        }
        let mut vals = Vec::with_capacity(8);
        for chunk in bytes.chunks(4) {
            vals.push(u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        }
        Ok(ParsedValue::U32Array(vals))
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
                    return Err(format!(
                        "Expected {} bytes from hex, got {}",
                        size,
                        bytes.len()
                    ));
                }
                Ok(ParsedValue::ByteArray(bytes))
            } else {
                let str_bytes = raw.as_bytes();
                if str_bytes.len() > size {
                    return Err(format!(
                        "String '{}' is {} bytes, max {} for [u8; {}]",
                        raw,
                        str_bytes.len(),
                        size,
                        size
                    ));
                }
                let mut bytes = vec![0u8; size];
                bytes[..str_bytes.len()].copy_from_slice(str_bytes);
                Ok(ParsedValue::ByteArray(bytes))
            }
        },
        IdlType::Primitive(p) if p == "u32" => {
            let parts: Vec<&str> = raw.split(',').map(|s| s.trim()).collect();
            if parts.len() != size {
                return Err(format!("Expected {} u32 values, got {}", size, parts.len()));
            }
            let mut vals = Vec::with_capacity(size);
            for p in &parts {
                vals.push(
                    p.parse::<u32>()
                        .map_err(|e| format!("Invalid u32 '{}': {}", p, e))?,
                );
            }
            Ok(ParsedValue::U32Array(vals))
        },
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
                        let hex = part
                            .strip_prefix("0x")
                            .or_else(|| part.strip_prefix("0X"))
                            .unwrap_or(part);
                        let bytes =
                            hex_decode(hex).map_err(|e| format!("Element [{}]: {}", i, e))?;
                        if bytes.len() != size {
                            return Err(format!(
                                "Element [{}]: expected {} bytes, got {} from '{}'",
                                i,
                                size,
                                bytes.len(),
                                part
                            ));
                        }
                        result.push(bytes);
                    }
                }
                Ok(ParsedValue::ByteArrayVec(result))
            },
            _ => Ok(ParsedValue::Raw(raw.to_string())),
        },
        // Vec<u8> — comma-separated decimal values
        IdlType::Primitive(p) if p == "u8" => {
            let bytes: Result<Vec<u8>, _> =
                raw.split(',').map(|s| s.trim().parse::<u8>()).collect();
            match bytes {
                Ok(b) => Ok(ParsedValue::ByteArray(b)),
                Err(_) => Ok(ParsedValue::Raw(raw.to_string())),
            }
        },
        // Vec<u32> — comma-separated decimal values
        IdlType::Primitive(p) if p == "u32" => {
            let vals: Result<Vec<u32>, _> =
                raw.split(',').map(|s| s.trim().parse::<u32>()).collect();
            match vals {
                Ok(v) => Ok(ParsedValue::U32Array(v)),
                Err(_) => Ok(ParsedValue::Raw(raw.to_string())),
            }
        },
        _ => Ok(ParsedValue::Raw(raw.to_string())),
    }
}

/// Build a `ParsedValue::StringVec` from one or more repeated `--flag <value>`
/// occurrences on the CLI.  The caller is expected to have collected every
/// occurrence of the flag — empty input yields an empty vec, matching the
/// IDL contract `Vec<String>`.
pub fn parse_string_vec(values: &[String]) -> ParsedValue {
    ParsedValue::StringVec(values.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use spel_framework_core::idl::IdlType;

    #[test]
    fn pda_seed_from_bytes32_arg() {
        // Simulate what happens when IDL has a [u8; 32] arg used as a PDA seed:
        // 1. IDL declares arg type as Array { array: (Primitive("u8"), 32) }
        // 2. User passes hex string on CLI
        // 3. parse_value should produce ParsedValue::ByteArray(...)
        // 4. PDA resolver should extract the 32 bytes as seed material

        let idl_type = IdlType::Array {
            array: (Box::new(IdlType::Primitive("u8".to_string())), 32),
        };

        let hex_input = "4343434343434343434343434343434343434343434343434343434343434343";
        let parsed = parse_value(hex_input, &idl_type).expect("should parse [u8; 32] from hex");

        // Must be ByteArray, not Raw — Raw causes PDA computation to fail
        match &parsed {
            ParsedValue::ByteArray(bytes) => {
                assert_eq!(bytes.len(), 32);
                assert_eq!(bytes[0], 0x43);
            },
            other => panic!("expected ByteArray, got {:?}", other),
        }
    }

    #[test]
    fn parse_string_vec_multiple_values() {
        let parsed = parse_string_vec(&["foo".to_string(), "bar".to_string(), "baz".to_string()]);
        match parsed {
            ParsedValue::StringVec(v) => assert_eq!(v, vec!["foo", "bar", "baz"]),
            other => panic!("expected StringVec, got {:?}", other),
        }
    }

    #[test]
    fn parse_string_vec_empty_input_yields_empty_vec() {
        match parse_string_vec(&[]) {
            ParsedValue::StringVec(v) => assert!(v.is_empty()),
            other => panic!("expected empty StringVec, got {:?}", other),
        }
    }

    #[test]
    fn parse_string_vec_single_element_yields_singleton() {
        match parse_string_vec(&["bafybeionly".to_string()]) {
            ParsedValue::StringVec(v) => assert_eq!(v, vec!["bafybeionly"]),
            other => panic!("expected StringVec, got {:?}", other),
        }
    }

    #[test]
    fn parse_string_vec_preserves_commas_in_elements() {
        // Repetition contract: an element containing a comma is one element,
        // never split.  This is the user-facing difference from the previous
        // CSV approach.
        let parsed = parse_string_vec(&["foo,bar".to_string(), "baz".to_string()]);
        match parsed {
            ParsedValue::StringVec(v) => assert_eq!(v, vec!["foo,bar", "baz"]),
            other => panic!("expected StringVec, got {:?}", other),
        }
    }

    #[test]
    fn primitive_bytes32_string_does_not_parse_as_byte_array() {
        // This is the bug: the macro emits Primitive("[u8; 32]") which
        // falls through to Raw instead of being parsed as a byte array.
        // This test documents the broken behavior that the macro fix addresses.
        let buggy_type = IdlType::Primitive("[u8; 32]".to_string());

        let hex_input = "4343434343434343434343434343434343434343434343434343434343434343";
        let parsed = parse_value(hex_input, &buggy_type).expect("should not error");

        // With Primitive("[u8; 32]"), parse_primitive doesn't recognize it → Raw
        assert!(
            matches!(&parsed, ParsedValue::Raw(_)),
            "Primitive('[u8; 32]') should fall through to Raw, got {:?}",
            parsed
        );
    }

    #[test]
    fn program_id_accepts_hex_and_canonical_base58() {
        // All legitimate ProgramId encodings still resolve to the same u32 limbs.
        let hex = "0000000100000002000000030000000400000005000000060000000700000008";
        let from_bare = parse_program_id(hex).expect("bare 64-char hex");
        let from_0x = parse_program_id(&format!("0x{hex}")).expect("0x-prefixed hex");
        assert_eq!(format!("{from_bare:?}"), format!("{from_0x:?}"));

        use base58::ToBase58;
        let bytes: [u8; 32] = crate::hex::decode_bytes_32(hex).unwrap();
        let from_b58 = parse_program_id(&bytes.to_base58()).expect("canonical base58 accepted");
        assert_eq!(format!("{from_b58:?}"), format!("{from_bare:?}"));
    }

    #[test]
    fn program_id_rejects_account_prefixed_input() {
        // #243: an account id (Public/Private prefix) must not be silently accepted
        // as a ProgramId — that would build the tx against the wrong program.
        use base58::ToBase58;
        let id = [7u8; 32].to_base58();

        let err = parse_program_id(&format!("Public/{id}"))
            .expect_err("Public/-prefixed account id must be rejected");
        assert!(err.contains("account id"), "unexpected error: {err}");

        assert!(
            parse_program_id(&format!("Private/{id}")).is_err(),
            "Private/-prefixed account id must be rejected"
        );

        // The bare 32-byte value is still a structurally valid ImageID (indistinguishable
        // from a program id without the prefix), so it is accepted.
        assert!(parse_program_id(&id).is_ok());
    }

    #[test]
    fn program_id_rejects_unrecognized_input() {
        // Fail-closed for junk that doesn't decode to a 32-byte ImageID.
        assert!(parse_program_id("not-a-program-id").is_err());
        assert!(parse_program_id("deadbeef").is_err());
    }
}
