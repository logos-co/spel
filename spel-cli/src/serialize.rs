//! risc0-compatible serialization for IDL instruction data.

use spel_framework_core::idl::IdlType;
use crate::parse::ParsedValue;

/// Serialize an instruction to risc0 serde format (Vec<u32>).
///
/// Produces: variant_index (u32), then each field serialized in order.
/// Matches `risc0_zkvm::serde::to_vec` for an enum struct variant.
pub fn serialize_to_risc0(
    variant_index: u32,
    parsed_args: &[(&IdlType, &ParsedValue)],
) -> Vec<u32> {
    let mut out = vec![variant_index];
    for (ty, val) in parsed_args {
        serialize_value_risc0(&mut out, ty, val);
    }
    out
}

fn serialize_value_risc0(out: &mut Vec<u32>, ty: &IdlType, val: &ParsedValue) {
    match (ty, val) {
        (IdlType::Primitive(p), _) => serialize_primitive_risc0(out, p.as_str(), val),
        (IdlType::Array { array }, _) => serialize_array_risc0(out, &array.0, array.1, val),
        (IdlType::Vec { vec }, _) => serialize_vec_risc0(out, vec, val),
        (IdlType::Option { option: _ }, ParsedValue::None) => {
            out.push(0);
        }
        (IdlType::Option { option }, ParsedValue::Some(inner)) => {
            out.push(1);
            serialize_value_risc0(out, option, inner);
        }
        (IdlType::Option { option }, _) => {
            out.push(1);
            serialize_value_risc0(out, option, val);
        }
        _ => {
            eprintln!("⚠️  Cannot serialize Defined/Raw type in risc0 format: {:?}", val);
        }
    }
}

fn serialize_primitive_risc0(out: &mut Vec<u32>, prim: &str, val: &ParsedValue) {
    match (prim, val) {
        ("bool", ParsedValue::Bool(b)) => out.push(if *b { 1 } else { 0 }),
        ("u8", ParsedValue::U8(v)) => out.push(*v as u32),
        ("u32", ParsedValue::U32(v)) => out.push(*v),
        ("u64", ParsedValue::U64(v)) => {
            out.push(*v as u32);
            out.push((*v >> 32) as u32);
        }
        ("u128", ParsedValue::U128(v)) => {
            let bytes = v.to_le_bytes();
            for chunk in bytes.chunks(4) {
                out.push(u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
            }
        }
        ("program_id", ParsedValue::U32Array(vals)) => {
            for v in vals {
                out.push(*v);
            }
        }
        ("string" | "String", ParsedValue::Str(s)) => {
            let bytes = s.as_bytes();
            out.push(bytes.len() as u32);
            serialize_bytes_padded(out, bytes);
        }
        _ => {
            eprintln!("⚠️  Type mismatch in risc0 serialization: prim={}, val={:?}", prim, val);
        }
    }
}

fn serialize_array_risc0(out: &mut Vec<u32>, elem_type: &IdlType, _size: usize, val: &ParsedValue) {
    match (elem_type, val) {
        (IdlType::Primitive(p), ParsedValue::ByteArray(bytes)) if p == "u8" => {
            for b in bytes {
                out.push(*b as u32);
            }
        }
        (IdlType::Primitive(p), ParsedValue::U32Array(vals)) if p == "u32" => {
            for v in vals {
                out.push(*v);
            }
        }
        _ => {
            eprintln!("⚠️  Cannot serialize array type in risc0 format: {:?}", val);
        }
    }
}

fn serialize_vec_risc0(out: &mut Vec<u32>, elem_type: &IdlType, val: &ParsedValue) {
    match (elem_type, val) {
        // Vec<u32> — comma-separated decimal values
        (IdlType::Primitive(p), ParsedValue::U32Array(vals)) if p == "u32" => {
            out.push(vals.len() as u32);
            for v in vals {
                out.push(*v);
            }
        }
        // Vec<u8> — byte array (already parsed)
        (IdlType::Primitive(p), ParsedValue::ByteArray(bytes)) if p == "u8" => {
            out.push(bytes.len() as u32);
            for b in bytes {
                out.push(*b as u32);
            }
        }
        // Vec<u32> — passed as Raw CSV string (e.g. "0,200,0,0,0")
        (IdlType::Primitive(p), ParsedValue::Raw(s)) if p == "u32" => {
            let vals: Vec<u32> = s.split(',')
                .filter_map(|x| x.trim().parse::<u32>().ok())
                .collect();
            out.push(vals.len() as u32);
            for v in vals {
                out.push(v);
            }
        }
        (IdlType::Array { array }, ParsedValue::ByteArrayVec(vecs)) => {
            out.push(vecs.len() as u32);
            match &*array.0 {
                IdlType::Primitive(p) if p == "u8" => {
                    for v in vecs {
                        for b in v {
                            out.push(*b as u32);
                        }
                    }
                }
                _ => {
                    eprintln!("⚠️  Cannot serialize Vec element type in risc0 format");
                }
            }
        }
        _ => {
            eprintln!("⚠️  Cannot serialize Vec type in risc0 format: {:?}", val);
        }
    }
}

fn serialize_bytes_padded(out: &mut Vec<u32>, bytes: &[u8]) {
    let mut i = 0;
    while i < bytes.len() {
        let remaining = bytes.len() - i;
        let mut word_bytes = [0u8; 4];
        let take = remaining.min(4);
        word_bytes[..take].copy_from_slice(&bytes[i..i + take]);
        out.push(u32::from_le_bytes(word_bytes));
        i += 4;
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
