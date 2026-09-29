//! risc0-compatible serialization for IDL instruction data.

use crate::parse::ParsedValue;
use spel_framework_core::idl::IdlType;

#[derive(Debug)]
pub enum SerializeError {
    TypeMismatch { expected: String, got: String },
    Risc0(String),
    Borsh(String),
}

impl std::fmt::Display for SerializeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SerializeError::TypeMismatch { expected, got } => {
                write!(f, "type mismatch: expected {}, got {}", expected, got)
            },
            SerializeError::Risc0(msg) => write!(f, "risc0 serialization error: {}", msg),
            SerializeError::Borsh(msg) => write!(f, "borsh serialization error: {}", msg),
        }
    }
}

impl std::error::Error for SerializeError {}

enum DynamicValue {
    Bool(bool),
    U8(u8),
    U32(u32),
    U64(u64),
    U128(u128),
    Str(String),
    Tuple(Vec<DynamicValue>),
    Seq(Vec<DynamicValue>),
    UnitVariant(u32),
    StructVariant(u32, Vec<DynamicValue>),
    None,
    Some(Box<DynamicValue>),
}

impl serde::Serialize for DynamicValue {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            DynamicValue::Bool(v) => serializer.serialize_bool(*v),
            DynamicValue::U8(v) => serializer.serialize_u8(*v),
            DynamicValue::U32(v) => serializer.serialize_u32(*v),
            DynamicValue::U64(v) => serializer.serialize_u64(*v),
            DynamicValue::U128(v) => serializer.serialize_u128(*v),
            DynamicValue::Str(s) => serializer.serialize_str(s),
            DynamicValue::Tuple(elems) => {
                use serde::ser::SerializeTuple;
                let mut tup = serializer.serialize_tuple(elems.len())?;
                for elem in elems {
                    tup.serialize_element(elem)?;
                }
                tup.end()
            },
            DynamicValue::Seq(elems) => {
                use serde::ser::SerializeSeq;
                let mut seq = serializer.serialize_seq(Some(elems.len()))?;
                for elem in elems {
                    seq.serialize_element(elem)?;
                }
                seq.end()
            },
            DynamicValue::UnitVariant(idx) => serializer.serialize_unit_variant("", *idx, ""),
            DynamicValue::StructVariant(idx, fields) => {
                use serde::ser::SerializeStructVariant;
                let mut sv = serializer.serialize_struct_variant("", *idx, "", fields.len())?;
                for f in fields {
                    sv.serialize_field("", f)?;
                }
                sv.end()
            },
            DynamicValue::None => serializer.serialize_none(),
            DynamicValue::Some(inner) => serializer.serialize_some(inner.as_ref()),
        }
    }
}

fn to_dynamic_value(ty: &IdlType, val: &ParsedValue) -> Result<DynamicValue, SerializeError> {
    match (ty, val) {
        (IdlType::Primitive(p), _) => primitive_to_dynamic(p.as_str(), val),
        (IdlType::Array { .. }, ParsedValue::ByteArray(bytes)) => Ok(DynamicValue::Tuple(
            bytes.iter().map(|b| DynamicValue::U8(*b)).collect(),
        )),
        (IdlType::Array { .. }, ParsedValue::U32Array(vals)) => Ok(DynamicValue::Tuple(
            vals.iter().map(|v| DynamicValue::U32(*v)).collect(),
        )),
        (IdlType::Vec { vec: _ }, ParsedValue::ByteArray(bytes)) => Ok(DynamicValue::Seq(
            bytes.iter().map(|b| DynamicValue::U8(*b)).collect(),
        )),
        (IdlType::Vec { vec: _ }, ParsedValue::U32Array(vals)) => Ok(DynamicValue::Seq(
            vals.iter().map(|v| DynamicValue::U32(*v)).collect(),
        )),
        (IdlType::Vec { vec: elem_ty }, ParsedValue::ByteArrayVec(vecs)) => {
            let elements: Result<Vec<_>, _> = vecs
                .iter()
                .map(|v| to_dynamic_value(elem_ty, &ParsedValue::ByteArray(v.clone())))
                .collect();
            Ok(DynamicValue::Seq(elements?))
        },
        (IdlType::Vec { vec: elem_ty }, ParsedValue::StringVec(strs)) if matches!(elem_ty.as_ref(), IdlType::Primitive(p) if p == "string" || p == "String") => {
            Ok(DynamicValue::Seq(
                strs.iter().cloned().map(DynamicValue::Str).collect(),
            ))
        },
        (IdlType::Vec { vec }, ParsedValue::Raw(s)) if matches!(vec.as_ref(), IdlType::Primitive(p) if p == "u32") =>
        {
            // Fallback: parse CSV of u32 values (e.g. "0,200,0,0,0")
            let vals: Vec<u32> = s
                .split(',')
                .filter_map(|x| x.trim().parse::<u32>().ok())
                .collect();
            Ok(DynamicValue::Seq(
                vals.iter().map(|v| DynamicValue::U32(*v)).collect(),
            ))
        },
        (IdlType::Vec { vec: elem_ty }, ParsedValue::Seq(items)) => {
            let elements: Result<Vec<_>, _> = items
                .iter()
                .map(|item| to_dynamic_value(elem_ty, item))
                .collect();
            Ok(DynamicValue::Seq(elements?))
        },
        (IdlType::Option { option: _ }, ParsedValue::None) => Ok(DynamicValue::None),
        (IdlType::Option { option }, ParsedValue::Some(inner)) => Ok(DynamicValue::Some(Box::new(
            to_dynamic_value(option, inner)?,
        ))),
        (IdlType::Option { option }, _) => {
            // Non-None, non-Some value with Option type -> wrap as Some
            Ok(DynamicValue::Some(Box::new(to_dynamic_value(option, val)?)))
        },
        (IdlType::Defined { .. }, ParsedValue::EnumVariant { index, fields, .. }) => {
            if fields.is_empty() {
                Ok(DynamicValue::UnitVariant(*index))
            } else {
                let converted: Result<Vec<_>, _> = fields
                    .iter()
                    .map(|(_, fty, fval)| to_dynamic_value(fty, fval))
                    .collect();
                Ok(DynamicValue::StructVariant(*index, converted?))
            }
        },
        _ => Err(SerializeError::TypeMismatch {
            expected: format!("{:?}", ty),
            got: format!("{:?}", val),
        }),
    }
}

fn primitive_to_dynamic(prim: &str, val: &ParsedValue) -> Result<DynamicValue, SerializeError> {
    match (prim, val) {
        ("bool", ParsedValue::Bool(v)) => Ok(DynamicValue::Bool(*v)),
        ("u8", ParsedValue::U8(v)) => Ok(DynamicValue::U8(*v)),
        ("u32", ParsedValue::U32(v)) => Ok(DynamicValue::U32(*v)),
        ("u64", ParsedValue::U64(v)) => Ok(DynamicValue::U64(*v)),
        ("u128", ParsedValue::U128(v)) => Ok(DynamicValue::U128(*v)),
        ("string" | "String", ParsedValue::Str(s)) => Ok(DynamicValue::Str(s.clone())),
        ("program_id", ParsedValue::U32Array(vals)) => Ok(DynamicValue::Tuple(
            vals.iter().map(|v| DynamicValue::U32(*v)).collect(),
        )),
        // `AccountId` is `{ value: [u8; 32] }` and its BorshSerialize writes those 32 raw
        // bytes -- no length prefix. Its base58 form comes from `SerializeDisplay`, which is
        // the SERDE impl and no longer governs the wire: instruction data has been Borsh
        // since LEZ v0.2.5. Emitting the base58 text here produced a borsh String (a u32
        // length of 44 followed by ASCII) where the guest expected 32 bytes, so every
        // instruction taking an `account_id` argument failed to deserialize and the program
        // reverted -- visible on chain only as an advanced nonce and no state change.
        ("account_id", ParsedValue::Str(s)) => {
            let bytes = crate::hex::decode_bytes_32(s)
                .map_err(|e| SerializeError::Borsh(format!("invalid account_id '{s}': {e}")))?;
            Ok(DynamicValue::Tuple(
                bytes.iter().map(|b| DynamicValue::U8(*b)).collect(),
            ))
        },
        _ => Err(SerializeError::TypeMismatch {
            expected: prim.to_string(),
            got: format!("{:?}", val),
        }),
    }
}

impl DynamicValue {
    /// Append this value's Borsh encoding to `out`.
    ///
    /// Borsh is the instruction wire format since LEZ v0.2.5. It is a
    /// length-prefixed little-endian format with no field names and no padding:
    ///
    /// - integers little-endian, `bool` as one byte
    /// - `String` and `Vec<T>` as a u32 length prefix then the elements
    /// - fixed arrays and tuples as their elements, with no prefix
    /// - `Option<T>` as `0u8`, or `1u8` followed by the value
    /// - an enum as a u8 variant tag, then that variant's fields
    fn write_borsh(&self, out: &mut Vec<u8>) -> Result<(), SerializeError> {
        match self {
            DynamicValue::Bool(v) => out.push(u8::from(*v)),
            DynamicValue::U8(v) => out.push(*v),
            DynamicValue::U32(v) => out.extend_from_slice(&v.to_le_bytes()),
            DynamicValue::U64(v) => out.extend_from_slice(&v.to_le_bytes()),
            DynamicValue::U128(v) => out.extend_from_slice(&v.to_le_bytes()),
            DynamicValue::Str(s) => {
                let len = u32::try_from(s.len())
                    .map_err(|_| SerializeError::Borsh("string exceeds u32 length".into()))?;
                out.extend_from_slice(&len.to_le_bytes());
                out.extend_from_slice(s.as_bytes());
            },
            // A fixed-size array or tuple: elements only, the length is in the type.
            DynamicValue::Tuple(elems) => {
                for elem in elems {
                    elem.write_borsh(out)?;
                }
            },
            DynamicValue::Seq(elems) => {
                let len = u32::try_from(elems.len())
                    .map_err(|_| SerializeError::Borsh("sequence exceeds u32 length".into()))?;
                out.extend_from_slice(&len.to_le_bytes());
                for elem in elems {
                    elem.write_borsh(out)?;
                }
            },
            DynamicValue::UnitVariant(idx) => out.push(variant_tag(*idx)?),
            DynamicValue::StructVariant(idx, fields) => {
                out.push(variant_tag(*idx)?);
                for f in fields {
                    f.write_borsh(out)?;
                }
            },
            DynamicValue::None => out.push(0),
            DynamicValue::Some(inner) => {
                out.push(1);
                inner.write_borsh(out)?;
            },
        }
        Ok(())
    }
}

/// Borsh encodes an enum variant as a single tag byte, so an index past 255 has
/// no representation.
fn variant_tag(index: u32) -> Result<u8, SerializeError> {
    u8::try_from(index).map_err(|_| {
        SerializeError::Borsh(format!(
            "variant index {index} exceeds the 255 Borsh encodes in one tag byte"
        ))
    })
}

/// Serialize an instruction to the Borsh wire format LEZ reads (`Vec<u8>`).
///
/// Produces the variant tag byte, then each field in order — matching the
/// `#[derive(BorshSerialize)]` layout of the `Instruction` enum the guest
/// deserializes with `read_lee_call`.
///
/// Because the tag leads, instruction variants are append-only: inserting one
/// shifts the encoding of every variant after it.
pub fn serialize_to_borsh(
    variant_index: u32,
    parsed_args: &[(&IdlType, &ParsedValue)],
) -> Result<Vec<u8>, SerializeError> {
    let fields: Vec<DynamicValue> = parsed_args
        .iter()
        .map(|(ty, val)| to_dynamic_value(ty, val))
        .collect::<Result<_, _>>()?;

    let mut out = vec![variant_tag(variant_index)?];
    for field in &fields {
        field.write_borsh(&mut out)?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{parse_string_vec, parse_value};
    use spel_framework_core::idl::IdlType;

    /// Verify risc0's own serializer as the reference for [u8; 32] format.
    #[test]
    fn risc0_reference_bytes32_format() {
        let seed: [u8; 32] = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
            0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c,
            0x1d, 0x1e, 0x1f, 0x20,
        ];

        #[derive(serde::Serialize)]
        enum TestInstruction {
            CommitRun {
                seed: [u8; 32],
                class: u8,
                strength: u32,
            },
        }

        let reference = risc0_zkvm::serde::to_vec(&TestInstruction::CommitRun {
            seed,
            class: 2,
            strength: 42,
        })
        .unwrap();

        // Each u8 is its own u32 word (not packed)
        // word[0] = variant index (0)
        // word[1..33] = 32 u8 values, each as u32
        // word[33] = class (2)
        // word[34] = strength (42)
        assert_eq!(
            reference.len(),
            35,
            "expected 35 words: 1 variant + 32 seed + 1 class + 1 strength"
        );
        assert_eq!(reference[0], 0, "variant index");
        assert_eq!(reference[1], 0x01, "seed[0]");
        assert_eq!(reference[2], 0x02, "seed[1]");
        assert_eq!(reference[33], 2, "class");
        assert_eq!(reference[34], 42, "strength");
    }

    #[test]
    fn dynamic_value_u32_smoke() {
        let val = DynamicValue::U32(42);
        let words = risc0_zkvm::serde::to_vec(&val).unwrap();
        assert_eq!(words, vec![42]);
    }

    #[test]
    fn dynamic_value_tuple_no_length_prefix() {
        // Tuple (fixed-size array) should NOT have a length prefix.
        let val = DynamicValue::Tuple(vec![DynamicValue::U8(1), DynamicValue::U8(2)]);
        let words = risc0_zkvm::serde::to_vec(&val).unwrap();
        assert_eq!(words, vec![1, 2]);
    }

    #[test]
    fn dynamic_value_seq_has_length_prefix() {
        // Seq (Vec) should have a length prefix.
        let val = DynamicValue::Seq(vec![DynamicValue::U8(1), DynamicValue::U8(2)]);
        let words = risc0_zkvm::serde::to_vec(&val).unwrap();
        assert_eq!(words, vec![2, 1, 2]); // length=2, then elements
    }

    #[test]
    fn to_dynamic_value_vec_u32_from_raw_csv() {
        let ty = IdlType::Vec {
            vec: Box::new(IdlType::Primitive("u32".to_string())),
        };
        let val = ParsedValue::Raw("1,2,3".to_string());

        let dv = to_dynamic_value(&ty, &val).unwrap();
        let words = risc0_zkvm::serde::to_vec(&dv).unwrap();
        assert_eq!(words, vec![3, 1, 2, 3]); // length=3, then values
    }

    #[test]
    fn to_dynamic_value_vec_string_emits_seq_of_str() {
        let ty = IdlType::Vec {
            vec: Box::new(IdlType::Primitive("string".to_string())),
        };
        let parsed = parse_string_vec(&["foo".to_string(), "bar".to_string(), "baz".to_string()]);
        let dv = to_dynamic_value(&ty, &parsed).unwrap();
        let words = risc0_zkvm::serde::to_vec(&dv).unwrap();
        // Seq of 3 strings: length=3, then each string is its own length + bytes
        // (one u32 word per byte, then padded to next word boundary).
        // We assert the length prefix here and rely on serde_roundtrip_vec_string
        // for the full bit-level contract check.
        assert_eq!(words[0], 3, "Seq length prefix");
    }

    #[test]
    fn to_dynamic_value_type_mismatch_returns_err() {
        let ty = IdlType::Primitive("u8".to_string());
        let val = ParsedValue::Str("not a u8".to_string());

        let result = to_dynamic_value(&ty, &val);
        assert!(result.is_err());
    }

    #[test]
    fn to_dynamic_value_vec_u128_seq_has_length_prefix_and_four_words_per_element() {
        let ty = IdlType::Vec {
            vec: Box::new(IdlType::Primitive("u128".to_string())),
        };
        let val = parse_value("300,700", &ty, &[]).unwrap();

        let dv = to_dynamic_value(&ty, &val).unwrap();
        let words = risc0_zkvm::serde::to_vec(&dv).unwrap();
        // length prefix, then each u128 as four little-endian u32 words
        assert_eq!(words, vec![2, 300, 0, 0, 0, 700, 0, 0, 0]);
    }

    #[test]
    fn to_dynamic_value_empty_vec_u128_is_zero_length_seq() {
        let ty = IdlType::Vec {
            vec: Box::new(IdlType::Primitive("u128".to_string())),
        };
        let val = parse_value("", &ty, &[]).unwrap();

        let dv = to_dynamic_value(&ty, &val).unwrap();
        let words = risc0_zkvm::serde::to_vec(&dv).unwrap();
        assert_eq!(words, vec![0]);
    }

    // ── Borsh wire format ────────────────────────────────────────────────
    //
    // These round-trip through the real `borsh` crate rather than asserting byte
    // layouts by hand: what matters is that what this encoder emits is exactly what
    // a guest's derived `BorshDeserialize` reads back.

    use borsh::BorshDeserialize;

    fn prim(name: &str) -> IdlType {
        IdlType::Primitive(name.to_string())
    }

    /// Encode one argument and hand back everything after the variant tag.
    fn encode_one(ty: &IdlType, raw: &str) -> Vec<u8> {
        let parsed = parse_value(raw, ty, &[]).expect("value parses");
        let bytes = serialize_to_borsh(0, &[(ty, &parsed)]).expect("encodes");
        assert_eq!(bytes[0], 0, "variant tag leads the encoding");
        bytes[1..].to_vec()
    }

    #[test]
    fn variant_tag_is_one_leading_byte() {
        let bytes = serialize_to_borsh(3, &[]).unwrap();
        assert_eq!(bytes, vec![3], "a fieldless variant is just its tag");
    }

    #[test]
    fn variant_index_past_a_byte_is_refused() {
        let err = serialize_to_borsh(256, &[]).expect_err("256 has no tag byte");
        assert!(
            matches!(err, SerializeError::Borsh(_)),
            "expected a borsh error, got: {err}"
        );
    }

    #[test]
    fn primitives_round_trip() {
        assert_eq!(
            u8::try_from_slice(&encode_one(&prim("u8"), "7")).unwrap(),
            7
        );
        assert_eq!(
            u32::try_from_slice(&encode_one(&prim("u32"), "4000000000")).unwrap(),
            4_000_000_000
        );
        assert_eq!(
            u64::try_from_slice(&encode_one(&prim("u64"), "18446744073709551615")).unwrap(),
            u64::MAX
        );
        assert_eq!(
            u128::try_from_slice(&encode_one(
                &prim("u128"),
                "340282366920938463463374607431768211455"
            ))
            .unwrap(),
            u128::MAX
        );
        assert!(bool::try_from_slice(&encode_one(&prim("bool"), "true")).unwrap());
        assert_eq!(
            String::try_from_slice(&encode_one(&prim("string"), "hello")).unwrap(),
            "hello"
        );
    }

    #[test]
    fn fixed_array_has_no_length_prefix() {
        let ty = IdlType::Array {
            array: (Box::new(prim("u8")), 32),
        };
        let hex = "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";
        let payload = encode_one(&ty, hex);

        assert_eq!(
            payload.len(),
            32,
            "a fixed array is its elements, nothing else"
        );
        let decoded = <[u8; 32]>::try_from_slice(&payload).unwrap();
        assert_eq!(decoded[0], 0x01);
        assert_eq!(decoded[31], 0x20);
    }

    #[test]
    fn vec_carries_a_u32_length_prefix() {
        let ty = IdlType::Vec {
            vec: Box::new(prim("u8")),
        };
        let val = ParsedValue::ByteArray(vec![0x3b, 0x50, 0x9c, 0x40]);
        let bytes = serialize_to_borsh(0, &[(&ty, &val)]).expect("encodes");
        let payload = &bytes[1..];

        assert_eq!(&payload[..4], &4u32.to_le_bytes(), "u32 length leads");
        assert_eq!(payload.len(), 8, "4-byte prefix + 4 bytes");
        assert_eq!(
            Vec::<u8>::try_from_slice(payload).unwrap(),
            vec![0x3b, 0x50, 0x9c, 0x40]
        );
    }

    #[test]
    fn empty_vec_is_a_zero_length_prefix() {
        let ty = IdlType::Vec {
            vec: Box::new(prim("u128")),
        };
        let payload = encode_one(&ty, "");
        assert_eq!(payload, 0u32.to_le_bytes().to_vec());
        assert!(Vec::<u128>::try_from_slice(&payload).unwrap().is_empty());
    }

    #[test]
    fn several_fields_concatenate_in_order() {
        let u64_ty = prim("u64");
        let u32_ty = prim("u32");
        let a = parse_value("1", &u64_ty, &[]).unwrap();
        let b = parse_value("2", &u32_ty, &[]).unwrap();

        let bytes = serialize_to_borsh(1, &[(&u64_ty, &a), (&u32_ty, &b)]).unwrap();

        let mut expected = vec![1u8];
        expected.extend_from_slice(&1u64.to_le_bytes());
        expected.extend_from_slice(&2u32.to_le_bytes());
        assert_eq!(
            bytes, expected,
            "tag, then each field in declaration order, no padding"
        );
    }

    /// The shape a guest actually sees: a derived enum decoding the bytes we sent.
    #[test]
    fn decodes_as_the_guest_instruction_enum_would() {
        #[derive(borsh::BorshDeserialize, Debug, PartialEq)]
        enum Instruction {
            Noop,
            Transfer { amount: u128, memo: String },
        }

        let amount_ty = prim("u128");
        let memo_ty = prim("string");
        let amount = parse_value("250", &amount_ty, &[]).unwrap();
        let memo = parse_value("rent", &memo_ty, &[]).unwrap();

        let bytes = serialize_to_borsh(1, &[(&amount_ty, &amount), (&memo_ty, &memo)]).unwrap();

        assert_eq!(
            Instruction::try_from_slice(&bytes).unwrap(),
            Instruction::Transfer {
                amount: 250,
                memo: "rent".to_string()
            }
        );
    }
}
