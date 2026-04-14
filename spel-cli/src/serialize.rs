//! risc0-compatible serialization for IDL instruction data.

use spel_framework_core::idl::IdlType;
use crate::parse::ParsedValue;

#[derive(Debug)]
pub enum SerializeError {
    TypeMismatch { expected: String, got: String },
    UnsupportedType { type_name: String },
    Risc0(String),
}

impl std::fmt::Display for SerializeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SerializeError::TypeMismatch { expected, got } => {
                write!(f, "type mismatch: expected {}, got {}", expected, got)
            }
            SerializeError::UnsupportedType { type_name } => {
                write!(f, "unsupported type: {}", type_name)
            }
            SerializeError::Risc0(msg) => write!(f, "risc0 serialization error: {}", msg),
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
            }
            DynamicValue::Seq(elems) => {
                use serde::ser::SerializeSeq;
                let mut seq = serializer.serialize_seq(Some(elems.len()))?;
                for elem in elems {
                    seq.serialize_element(elem)?;
                }
                seq.end()
            }
            DynamicValue::None => serializer.serialize_none(),
            DynamicValue::Some(inner) => serializer.serialize_some(inner.as_ref()),
        }
    }
}

fn to_dynamic_value(ty: &IdlType, val: &ParsedValue) -> Result<DynamicValue, SerializeError> {
    match (ty, val) {
        (IdlType::Primitive(p), _) => primitive_to_dynamic(p.as_str(), val),
        (IdlType::Array { .. }, ParsedValue::ByteArray(bytes)) => {
            Ok(DynamicValue::Tuple(bytes.iter().map(|b| DynamicValue::U8(*b)).collect()))
        }
        (IdlType::Array { .. }, ParsedValue::U32Array(vals)) => {
            Ok(DynamicValue::Tuple(vals.iter().map(|v| DynamicValue::U32(*v)).collect()))
        }
        (IdlType::Vec { vec: _ }, ParsedValue::ByteArray(bytes)) => {
            Ok(DynamicValue::Seq(bytes.iter().map(|b| DynamicValue::U8(*b)).collect()))
        }
        (IdlType::Vec { vec: _ }, ParsedValue::U32Array(vals)) => {
            Ok(DynamicValue::Seq(vals.iter().map(|v| DynamicValue::U32(*v)).collect()))
        }
        (IdlType::Vec { vec: elem_ty }, ParsedValue::ByteArrayVec(vecs)) => {
            let elements: Result<Vec<_>, _> = vecs
                .iter()
                .map(|v| to_dynamic_value(elem_ty, &ParsedValue::ByteArray(v.clone())))
                .collect();
            Ok(DynamicValue::Seq(elements?))
        }
        (IdlType::Vec { vec: _ }, ParsedValue::Raw(s)) => {
            // Fallback: parse CSV of u32 values (e.g. "0,200,0,0,0")
            let vals: Vec<u32> = s
                .split(',')
                .filter_map(|x| x.trim().parse::<u32>().ok())
                .collect();
            Ok(DynamicValue::Seq(vals.iter().map(|v| DynamicValue::U32(*v)).collect()))
        }
        (IdlType::Option { option: _ }, ParsedValue::None) => Ok(DynamicValue::None),
        (IdlType::Option { option }, ParsedValue::Some(inner)) => {
            Ok(DynamicValue::Some(Box::new(to_dynamic_value(option, inner)?)))
        }
        (IdlType::Option { option }, _) => {
            // Non-None, non-Some value with Option type -> wrap as Some
            Ok(DynamicValue::Some(Box::new(to_dynamic_value(option, val)?)))
        }
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
        ("program_id", ParsedValue::U32Array(vals)) => {
            Ok(DynamicValue::Tuple(vals.iter().map(|v| DynamicValue::U32(*v)).collect()))
        }
        _ => Err(SerializeError::TypeMismatch {
            expected: prim.to_string(),
            got: format!("{:?}", val),
        }),
    }
}

struct InstructionData<'a> {
    variant_index: u32,
    fields: &'a [DynamicValue],
}

impl serde::Serialize for InstructionData<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeTupleVariant;
        let mut tv = serializer.serialize_tuple_variant(
            "",
            self.variant_index,
            "",
            self.fields.len(),
        )?;
        for field in self.fields {
            tv.serialize_field(field)?;
        }
        tv.end()
    }
}

/// Serialize an instruction to risc0 serde format (Vec<u32>).
///
/// Produces: variant_index (u32), then each field serialized in order.
/// Delegates to `risc0_zkvm::serde::to_vec` for format correctness.
pub fn serialize_to_risc0(
    variant_index: u32,
    parsed_args: &[(&IdlType, &ParsedValue)],
) -> Result<Vec<u32>, SerializeError> {
    let fields: Vec<DynamicValue> = parsed_args
        .iter()
        .map(|(ty, val)| to_dynamic_value(ty, val))
        .collect::<Result<_, _>>()?;

    let instruction = InstructionData {
        variant_index,
        fields: &fields,
    };

    risc0_zkvm::serde::to_vec(&instruction).map_err(|e| SerializeError::Risc0(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use spel_framework_core::idl::IdlType;
    use crate::parse::parse_value;
    use risc0_zkvm::serde::Deserializer;
    use serde::Deserialize;

    #[test]
    fn serialize_bytes32_one_word_per_byte() {
        // risc0 serde format: each u8 is its own u32 word (zero-extended).
        // A [u8; 32] produces 32 u32 words, NOT 8 packed words.
        let idl_type = IdlType::Array {
            array: (Box::new(IdlType::Primitive("u8".to_string())), 32),
        };

        let parsed = parse_value(
            "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20",
            &idl_type,
        ).unwrap();

        let words = serialize_to_risc0(0, &[(&idl_type, &parsed)]).unwrap();

        // words[0] = variant index, words[1..33] = 32 individual u8-as-u32 words
        let payload = &words[1..];
        assert_eq!(payload.len(), 32, "expected 32 u32 words for [u8; 32]");
        assert_eq!(payload[0], 0x01);
        assert_eq!(payload[1], 0x02);
        assert_eq!(payload[31], 0x20);
    }

    #[test]
    fn serialize_vec_u8_one_word_per_byte() {
        // Vec<u8> in risc0 serde: length prefix + one u32 word per byte.
        let elem_type = IdlType::Primitive("u8".to_string());
        let idl_type = IdlType::Vec { vec: Box::new(elem_type) };

        let bytes = ParsedValue::ByteArray(vec![0x3b, 0x50, 0x9c, 0x40]);

        let words = serialize_to_risc0(0, &[(&idl_type, &bytes)]).unwrap();

        // words[0] = variant index, words[1] = length (4), words[2..6] = bytes as u32
        let payload = &words[1..];
        assert_eq!(payload[0], 4, "length prefix");
        assert_eq!(payload.len(), 5, "1 length + 4 bytes");
        assert_eq!(payload[1], 0x3b);
        assert_eq!(payload[2], 0x50);
    }

    #[test]
    fn serialize_vec_byte_array_one_word_per_byte() {
        // Vec<[u8; 4]>: vec length prefix, then each element's bytes as individual words.
        let inner = IdlType::Array {
            array: (Box::new(IdlType::Primitive("u8".to_string())), 4),
        };
        let idl_type = IdlType::Vec { vec: Box::new(inner) };

        let bytes = ParsedValue::ByteArrayVec(vec![
            vec![0x3b, 0x50, 0x9c, 0x40],
            vec![0x61, 0x13, 0x01, 0xf7],
        ]);

        let words = serialize_to_risc0(0, &[(&idl_type, &bytes)]).unwrap();

        // words[0] = variant, words[1] = vec len (2), words[2..6] = elem0, words[6..10] = elem1
        let payload = &words[1..];
        assert_eq!(payload[0], 2, "vec length");
        assert_eq!(payload.len(), 9, "1 length + 2*4 bytes");
        assert_eq!(payload[1], 0x3b);
        assert_eq!(payload[5], 0x61);
    }

    /// Verify risc0's own serializer as the reference for [u8; 32] format.
    #[test]
    fn risc0_reference_bytes32_format() {
        let seed: [u8; 32] = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08,
            0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10,
            0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18,
            0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f, 0x20,
        ];

        #[derive(serde::Serialize)]
        enum TestInstruction {
            CommitRun { seed: [u8; 32], class: u8, strength: u32 },
        }

        let reference = risc0_zkvm::serde::to_vec(
            &TestInstruction::CommitRun { seed, class: 2, strength: 42 }
        ).unwrap();

        // Each u8 is its own u32 word (not packed)
        // word[0] = variant index (0)
        // word[1..33] = 32 u8 values, each as u32
        // word[33] = class (2)
        // word[34] = strength (42)
        assert_eq!(reference.len(), 35, "expected 35 words: 1 variant + 32 seed + 1 class + 1 strength");
        assert_eq!(reference[0], 0, "variant index");
        assert_eq!(reference[1], 0x01, "seed[0]");
        assert_eq!(reference[2], 0x02, "seed[1]");
        assert_eq!(reference[33], 2, "class");
        assert_eq!(reference[34], 42, "strength");
    }

    /// Verifies spel CLI's serialization is compatible with the guest-side
    /// Deserializer from risc0_zkvm (used by nssa_core::program::read_nssa_inputs).
    /// This is the contract between the CLI (transaction sender) and the
    /// on-chain program (transaction executor) at LEZ v0.2.0-rc1.
    #[test]
    fn serialize_deserialize_roundtrip_with_bytes32() {
        #[derive(Deserialize, Debug, PartialEq)]
        enum TestInstruction {
            CommitRun {
                seed: [u8; 32],
                class: u8,
                strength: u32,
            },
        }

        // 1. Define IDL arg types matching the enum variant fields
        let seed_type = IdlType::Array {
            array: (Box::new(IdlType::Primitive("u8".into())), 32),
        };
        let class_type = IdlType::Primitive("u8".into());
        let strength_type = IdlType::Primitive("u32".into());

        // 2. Parse CLI values exactly as spel would
        let seed_hex = "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";
        let parsed_seed = parse_value(seed_hex, &seed_type).unwrap();
        let parsed_class = parse_value("2", &class_type).unwrap();
        let parsed_strength = parse_value("42", &strength_type).unwrap();

        // 3. Serialize to u32 words (variant_index=0 for CommitRun)
        let words = serialize_to_risc0(0, &[
            (&seed_type, &parsed_seed),
            (&class_type, &parsed_class),
            (&strength_type, &parsed_strength),
        ]).unwrap();

        // 4. Deserialize using risc0's Deserializer — the SAME code the guest runs
        let instruction: TestInstruction =
            TestInstruction::deserialize(&mut Deserializer::new(words.as_ref()))
                .expect("guest-side deserialization must succeed");

        // 5. Assert values survived the roundtrip
        let expected_seed: [u8; 32] = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08,
            0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10,
            0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18,
            0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f, 0x20,
        ];
        assert_eq!(
            instruction,
            TestInstruction::CommitRun {
                seed: expected_seed,
                class: 2,
                strength: 42,
            }
        );
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
    fn to_dynamic_value_bytes32_matches_old_serializer() {
        let ty = IdlType::Array {
            array: (Box::new(IdlType::Primitive("u8".to_string())), 32),
        };
        let hex = "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";
        let parsed = parse_value(hex, &ty).unwrap();

        let dv = to_dynamic_value(&ty, &parsed).unwrap();
        let serde_words = risc0_zkvm::serde::to_vec(&dv).unwrap();

        // Full serializer output (minus variant index)
        let mut full_words = serialize_to_risc0(0, &[(&ty, &parsed)]).unwrap();
        full_words.remove(0); // remove variant index

        assert_eq!(serde_words, full_words);
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
    fn to_dynamic_value_type_mismatch_returns_err() {
        let ty = IdlType::Primitive("u8".to_string());
        let val = ParsedValue::Str("not a u8".to_string());

        let result = to_dynamic_value(&ty, &val);
        assert!(result.is_err());
    }

    /// The critical contract test: serde path must roundtrip through risc0 Deserializer.
    #[test]
    fn serde_roundtrip_with_bytes32() {
        #[derive(Deserialize, Debug, PartialEq)]
        enum TestInstruction {
            CommitRun {
                seed: [u8; 32],
                class: u8,
                strength: u32,
            },
        }

        let seed_type = IdlType::Array {
            array: (Box::new(IdlType::Primitive("u8".into())), 32),
        };
        let class_type = IdlType::Primitive("u8".into());
        let strength_type = IdlType::Primitive("u32".into());

        let parsed_seed = parse_value(
            "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20",
            &seed_type,
        ).unwrap();
        let parsed_class = parse_value("2", &class_type).unwrap();
        let parsed_strength = parse_value("42", &strength_type).unwrap();

        let words = serialize_to_risc0(0, &[
            (&seed_type, &parsed_seed),
            (&class_type, &parsed_class),
            (&strength_type, &parsed_strength),
        ]).unwrap();

        let instruction: TestInstruction =
            TestInstruction::deserialize(&mut Deserializer::new(words.as_ref()))
                .expect("guest-side deserialization must succeed");

        let expected_seed: [u8; 32] = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08,
            0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10,
            0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18,
            0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f, 0x20,
        ];
        assert_eq!(
            instruction,
            TestInstruction::CommitRun { seed: expected_seed, class: 2, strength: 42 }
        );
    }

    #[test]
    fn serde_roundtrip_all_primitives() {
        #[derive(Deserialize, Debug, PartialEq)]
        enum TestInstruction {
            AllPrims {
                b: bool,
                v8: u8,
                v32: u32,
                v64: u64,
                v128: u128,
            },
        }

        let types: Vec<IdlType> = vec![
            IdlType::Primitive("bool".into()),
            IdlType::Primitive("u8".into()),
            IdlType::Primitive("u32".into()),
            IdlType::Primitive("u64".into()),
            IdlType::Primitive("u128".into()),
        ];
        let vals: Vec<ParsedValue> = vec![
            ParsedValue::Bool(true),
            ParsedValue::U8(255),
            ParsedValue::U32(0xDEADBEEF),
            ParsedValue::U64(0x0102030405060708),
            ParsedValue::U128(0x0102030405060708090a0b0c0d0e0f10),
        ];

        let args: Vec<(&IdlType, &ParsedValue)> =
            types.iter().zip(vals.iter()).collect();
        let words = serialize_to_risc0(0, &args).unwrap();

        let instruction: TestInstruction =
            TestInstruction::deserialize(&mut Deserializer::new(words.as_ref()))
                .expect("deserialization must succeed");

        assert_eq!(
            instruction,
            TestInstruction::AllPrims {
                b: true,
                v8: 255,
                v32: 0xDEADBEEF,
                v64: 0x0102030405060708,
                v128: 0x0102030405060708090a0b0c0d0e0f10,
            }
        );
    }

    #[test]
    fn serde_roundtrip_option() {
        #[derive(Deserialize, Debug, PartialEq)]
        enum TestInstruction {
            Opts { a: Option<u32>, b: Option<u32> },
        }

        let opt_type = IdlType::Option {
            option: Box::new(IdlType::Primitive("u32".into())),
        };

        let some_val = ParsedValue::Some(Box::new(ParsedValue::U32(42)));
        let none_val = ParsedValue::None;

        let words = serialize_to_risc0(0, &[
            (&opt_type, &some_val),
            (&opt_type, &none_val),
        ]).unwrap();

        let instruction: TestInstruction =
            TestInstruction::deserialize(&mut Deserializer::new(words.as_ref()))
                .expect("deserialization must succeed");

        assert_eq!(
            instruction,
            TestInstruction::Opts { a: Some(42), b: None }
        );
    }

    #[test]
    fn serde_roundtrip_vec_types() {
        #[derive(Deserialize, Debug, PartialEq)]
        enum TestInstruction {
            Vecs { a: Vec<u8>, b: Vec<u32> },
        }

        let vec_u8_type = IdlType::Vec {
            vec: Box::new(IdlType::Primitive("u8".into())),
        };
        let vec_u32_type = IdlType::Vec {
            vec: Box::new(IdlType::Primitive("u32".into())),
        };

        let val_u8 = ParsedValue::ByteArray(vec![0x3b, 0x50]);
        let val_u32 = ParsedValue::U32Array(vec![100, 200]);

        let words = serialize_to_risc0(0, &[
            (&vec_u8_type, &val_u8),
            (&vec_u32_type, &val_u32),
        ]).unwrap();

        let instruction: TestInstruction =
            TestInstruction::deserialize(&mut Deserializer::new(words.as_ref()))
                .expect("deserialization must succeed");

        assert_eq!(
            instruction,
            TestInstruction::Vecs { a: vec![0x3b, 0x50], b: vec![100, 200] }
        );
    }

    #[test]
    fn serde_roundtrip_vec_byte_arrays() {
        #[derive(Deserialize, Debug, PartialEq)]
        enum TestInstruction {
            ByteVec { data: Vec<[u8; 4]> },
        }

        let inner_type = IdlType::Array {
            array: (Box::new(IdlType::Primitive("u8".into())), 4),
        };
        let vec_type = IdlType::Vec { vec: Box::new(inner_type) };

        let val = ParsedValue::ByteArrayVec(vec![
            vec![0x01, 0x02, 0x03, 0x04],
            vec![0x05, 0x06, 0x07, 0x08],
        ]);

        let words = serialize_to_risc0(0, &[(&vec_type, &val)]).unwrap();

        let instruction: TestInstruction =
            TestInstruction::deserialize(&mut Deserializer::new(words.as_ref()))
                .expect("deserialization must succeed");

        assert_eq!(
            instruction,
            TestInstruction::ByteVec {
                data: vec![[0x01, 0x02, 0x03, 0x04], [0x05, 0x06, 0x07, 0x08]]
            }
        );
    }

    #[test]
    fn serde_error_on_type_mismatch() {
        let ty = IdlType::Primitive("u8".to_string());
        let val = ParsedValue::Str("wrong".to_string());

        let result = serialize_to_risc0(0, &[(&ty, &val)]);
        assert!(result.is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spel_framework_core::idl::IdlType;
    use crate::parse::parse_value;
    use risc0_zkvm::serde::Deserializer;
    use serde::Deserialize;

    #[test]
    fn serialize_bytes32_one_word_per_byte() {
        // risc0 serde format: each u8 is its own u32 word (zero-extended).
        // A [u8; 32] produces 32 u32 words, NOT 8 packed words.
        let idl_type = IdlType::Array {
            array: (Box::new(IdlType::Primitive("u8".to_string())), 32),
        };

        let parsed = parse_value(
            "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20",
            &idl_type,
        ).unwrap();

        let words = serialize_to_risc0(0, &[(&idl_type, &parsed)]);

        // words[0] = variant index, words[1..33] = 32 individual u8-as-u32 words
        let payload = &words[1..];
        assert_eq!(payload.len(), 32, "expected 32 u32 words for [u8; 32]");
        assert_eq!(payload[0], 0x01);
        assert_eq!(payload[1], 0x02);
        assert_eq!(payload[31], 0x20);
    }

    #[test]
    fn serialize_vec_u8_one_word_per_byte() {
        // Vec<u8> in risc0 serde: length prefix + one u32 word per byte.
        let elem_type = IdlType::Primitive("u8".to_string());
        let idl_type = IdlType::Vec { vec: Box::new(elem_type) };

        let bytes = ParsedValue::ByteArray(vec![0x3b, 0x50, 0x9c, 0x40]);

        let words = serialize_to_risc0(0, &[(&idl_type, &bytes)]);

        // words[0] = variant index, words[1] = length (4), words[2..6] = bytes as u32
        let payload = &words[1..];
        assert_eq!(payload[0], 4, "length prefix");
        assert_eq!(payload.len(), 5, "1 length + 4 bytes");
        assert_eq!(payload[1], 0x3b);
        assert_eq!(payload[2], 0x50);
    }

    #[test]
    fn serialize_vec_byte_array_one_word_per_byte() {
        // Vec<[u8; 4]>: vec length prefix, then each element's bytes as individual words.
        let inner = IdlType::Array {
            array: (Box::new(IdlType::Primitive("u8".to_string())), 4),
        };
        let idl_type = IdlType::Vec { vec: Box::new(inner) };

        let bytes = ParsedValue::ByteArrayVec(vec![
            vec![0x3b, 0x50, 0x9c, 0x40],
            vec![0x61, 0x13, 0x01, 0xf7],
        ]);

        let words = serialize_to_risc0(0, &[(&idl_type, &bytes)]);

        // words[0] = variant, words[1] = vec len (2), words[2..6] = elem0, words[6..10] = elem1
        let payload = &words[1..];
        assert_eq!(payload[0], 2, "vec length");
        assert_eq!(payload.len(), 9, "1 length + 2*4 bytes");
        assert_eq!(payload[1], 0x3b);
        assert_eq!(payload[5], 0x61);
    }

    /// Verify risc0's own serializer as the reference for [u8; 32] format.
    #[test]
    fn risc0_reference_bytes32_format() {
        let seed: [u8; 32] = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08,
            0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10,
            0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18,
            0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f, 0x20,
        ];

        #[derive(serde::Serialize)]
        enum TestInstruction {
            CommitRun { seed: [u8; 32], class: u8, strength: u32 },
        }

        let reference = risc0_zkvm::serde::to_vec(
            &TestInstruction::CommitRun { seed, class: 2, strength: 42 }
        ).unwrap();

        // Each u8 is its own u32 word (not packed)
        // word[0] = variant index (0)
        // word[1..33] = 32 u8 values, each as u32
        // word[33] = class (2)
        // word[34] = strength (42)
        assert_eq!(reference.len(), 35, "expected 35 words: 1 variant + 32 seed + 1 class + 1 strength");
        assert_eq!(reference[0], 0, "variant index");
        assert_eq!(reference[1], 0x01, "seed[0]");
        assert_eq!(reference[2], 0x02, "seed[1]");
        assert_eq!(reference[33], 2, "class");
        assert_eq!(reference[34], 42, "strength");
    }

    /// Verifies spel CLI's serialization is compatible with the guest-side
    /// Deserializer from risc0_zkvm (used by nssa_core::program::read_nssa_inputs).
    /// This is the contract between the CLI (transaction sender) and the
    /// on-chain program (transaction executor) at LEZ v0.2.0-rc1.
    #[test]
    fn serialize_deserialize_roundtrip_with_bytes32() {
        #[derive(Deserialize, Debug, PartialEq)]
        enum TestInstruction {
            CommitRun {
                seed: [u8; 32],
                class: u8,
                strength: u32,
            },
        }

        // 1. Define IDL arg types matching the enum variant fields
        let seed_type = IdlType::Array {
            array: (Box::new(IdlType::Primitive("u8".into())), 32),
        };
        let class_type = IdlType::Primitive("u8".into());
        let strength_type = IdlType::Primitive("u32".into());

        // 2. Parse CLI values exactly as spel would
        let seed_hex = "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";
        let parsed_seed = parse_value(seed_hex, &seed_type).unwrap();
        let parsed_class = parse_value("2", &class_type).unwrap();
        let parsed_strength = parse_value("42", &strength_type).unwrap();

        // 3. Serialize to u32 words (variant_index=0 for CommitRun)
        let words = serialize_to_risc0(0, &[
            (&seed_type, &parsed_seed),
            (&class_type, &parsed_class),
            (&strength_type, &parsed_strength),
        ]);

        // 4. Deserialize using risc0's Deserializer — the SAME code the guest runs
        let instruction: TestInstruction =
            TestInstruction::deserialize(&mut Deserializer::new(words.as_ref()))
                .expect("guest-side deserialization must succeed");

        // 5. Assert values survived the roundtrip
        let expected_seed: [u8; 32] = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08,
            0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10,
            0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18,
            0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f, 0x20,
        ];
        assert_eq!(
            instruction,
            TestInstruction::CommitRun {
                seed: expected_seed,
                class: 2,
                strength: 42,
            }
        );
    }
}
