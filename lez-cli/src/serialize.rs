//! risc0-compatible serialization for IDL instruction data.

use lez_framework_core::idl::IdlType;
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
