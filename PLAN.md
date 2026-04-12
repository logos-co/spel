# Plan: Replace hand-rolled serializer with risc0 serde

## Context

The spel CLI serializes instruction args into `Vec<u32>` for submission to LEZ programs. The guest-side deserializes using `risc0_zkvm::serde::Deserializer` (see `nssa_core::program::read_nssa_inputs` at LEZ v0.2.0-rc1, commit 35d8df0).

The CLI currently hand-rolls this serialization in `spel-cli/src/serialize.rs`. This has caused multiple bugs:
- **IDL type mismatch** (PR #129): macro emitted `Primitive("[u8; 32]")` instead of `Array{Primitive("u8"), 32}`
- **Silent failures**: six `eprintln!` + continue paths where type mismatches silently produce truncated/corrupt `Vec<u32>` with no error signal
- **Format fragility**: the hand-rolled code must exactly match `risc0_zkvm::serde`'s wire format by coincidence, with no compile-time guarantee

## Goal

Replace `serialize_to_risc0` with a `serde::Serialize` implementation that delegates to `risc0_zkvm::serde::to_vec`, eliminating format divergence by construction.

## Key Insight

`risc0_zkvm::serde::to_vec(&impl Serialize)` is the canonical serialization path (used by `nssa::program::Program::serialize_instruction`). If we implement `serde::Serialize` for a dynamic value type that mirrors our `ParsedValue`, we can call `to_vec` directly and get format correctness for free.

This pattern is well-established (cf. `serde_json::Value`).

## risc0 serde format reference

Verified against risc0-zkvm 3.0.5 (the version resolved in Cargo.lock):

| Rust type | serde trait method | Wire format (u32 words) |
|-----------|-------------------|------------------------|
| `u8` | `serialize_u8` | 1 word (zero-extended) |
| `u32` | `serialize_u32` | 1 word |
| `u64` | `serialize_u64` | 2 words (lo, hi) |
| `u128` | `serialize_u128` | 4 words (LE bytes packed) |
| `bool` | `serialize_bool` | 1 word (0 or 1) |
| `String` | `serialize_str` | length + padded bytes |
| `[T; N]` | `serialize_tuple(N)` | N elements, no length prefix |
| `Vec<T>` | `serialize_seq(len)` | length prefix + elements |
| `Option::None` | `serialize_none` | 1 word (0) |
| `Option::Some(v)` | `serialize_some` | 1 word (1) + value |
| enum struct variant | `serialize_struct_variant` | variant_index + fields |

Critical: `[u8; 32]` serializes as 32 individual u32 words (one per byte), NOT packed 4-per-word. The existing roundtrip test in `serialize.rs` (`serialize_deserialize_roundtrip_with_bytes32`) validates this.

## Implementation

### Files to modify

- `spel-cli/src/serialize.rs` — rewrite core serialization logic
- `spel-cli/src/tx.rs` — update call site (line ~123)

### Files for reference (read-only)

- `spel-cli/src/parse.rs` — `ParsedValue` enum, `parse_value()` function
- `spel-framework-core/src/idl.rs` — `IdlType` enum, `SpelIdl` struct
- `~/.cargo/registry/src/index.crates.io-*/risc0-zkvm-3.0.5/src/serde/serializer.rs` — canonical format reference

### Task 1: Add `SerializeError` type

In `serialize.rs`, define:

```rust
#[derive(Debug)]
pub enum SerializeError {
    TypeMismatch { expected: String, got: String },
    UnsupportedType { type_name: String },
    Risc0(String),
}

impl std::fmt::Display for SerializeError { ... }
```

Change `serialize_to_risc0` return type to `Result<Vec<u32>, SerializeError>`.

### Task 2: Create `DynamicValue` enum

```rust
enum DynamicValue {
    Bool(bool),
    U8(u8),
    U32(u32),
    U64(u64),
    U128(u128),
    Str(String),
    Tuple(Vec<DynamicValue>),           // [T; N] — no length prefix
    Seq(Vec<DynamicValue>),             // Vec<T> — length-prefixed
    None,
    Some(Box<DynamicValue>),
}
```

Key difference from `ParsedValue`: no `ByteArray(Vec<u8>)`. A `[u8; 32]` becomes `Tuple(vec![U8(0x01), U8(0x02), ...])` — 32 individual elements. This matches what serde's derive generates for fixed-size arrays.

### Task 3: Implement `serde::Serialize` for `DynamicValue`

```rust
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
```

### Task 4: Create `DynamicInstruction` wrapper

```rust
struct DynamicInstruction {
    variant_index: u32,
    variant_name: String,
    fields: Vec<(String, DynamicValue)>,  // (field_name, value)
}

impl serde::Serialize for DynamicInstruction {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStructVariant;
        let mut sv = serializer.serialize_struct_variant(
            "Instruction",
            self.variant_index,
            // Note: risc0's serializer ignores the variant name string,
            // only uses the index. But we pass it for correctness.
            &self.variant_name,
            self.fields.len(),
        )?;
        for (name, value) in &self.fields {
            sv.serialize_field(name, value)?;
        }
        sv.end()
    }
}
```

### Task 5: Write `to_dynamic_value` conversion

```rust
fn to_dynamic_value(
    ty: &IdlType,
    val: &ParsedValue,
) -> Result<DynamicValue, SerializeError> {
    match (ty, val) {
        (IdlType::Primitive(p), _) => primitive_to_dynamic(p, val),
        (IdlType::Array { array }, ParsedValue::ByteArray(bytes)) => {
            // [u8; N] → Tuple of N individual U8 values
            Ok(DynamicValue::Tuple(bytes.iter().map(|b| DynamicValue::U8(*b)).collect()))
        }
        (IdlType::Array { array }, ParsedValue::U32Array(vals)) => {
            Ok(DynamicValue::Tuple(vals.iter().map(|v| DynamicValue::U32(*v)).collect()))
        }
        (IdlType::Vec { vec }, ParsedValue::ByteArray(bytes)) => {
            Ok(DynamicValue::Seq(bytes.iter().map(|b| DynamicValue::U8(*b)).collect()))
        }
        (IdlType::Vec { vec }, ParsedValue::U32Array(vals)) => {
            Ok(DynamicValue::Seq(vals.iter().map(|v| DynamicValue::U32(*v)).collect()))
        }
        (IdlType::Vec { vec: elem_ty }, ParsedValue::ByteArrayVec(vecs)) => {
            let elements: Result<Vec<_>, _> = vecs.iter()
                .map(|v| {
                    let inner = ParsedValue::ByteArray(v.clone());
                    to_dynamic_value(elem_ty, &inner)
                })
                .collect();
            Ok(DynamicValue::Seq(elements?))
        }
        (IdlType::Option { .. }, ParsedValue::None) => Ok(DynamicValue::None),
        (IdlType::Option { option }, ParsedValue::Some(inner)) => {
            Ok(DynamicValue::Some(Box::new(to_dynamic_value(option, inner)?)))
        }
        (IdlType::Option { option }, val) => {
            // Non-None, non-Some value with Option type → wrap as Some
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
            // ProgramId = [u32; 8]
            Ok(DynamicValue::Tuple(vals.iter().map(|v| DynamicValue::U32(*v)).collect()))
        }
        _ => Err(SerializeError::TypeMismatch {
            expected: prim.to_string(),
            got: format!("{:?}", val),
        }),
    }
}
```

### Task 6: Rewrite `serialize_to_risc0`

```rust
pub fn serialize_to_risc0(
    variant_index: u32,
    variant_name: &str,
    parsed_args: &[(&str, &IdlType, &ParsedValue)],  // (field_name, type, value)
) -> Result<Vec<u32>, SerializeError> {
    let fields: Vec<(String, DynamicValue)> = parsed_args
        .iter()
        .map(|(name, ty, val)| {
            to_dynamic_value(ty, val).map(|dv| (name.to_string(), dv))
        })
        .collect::<Result<_, _>>()?;

    let instruction = DynamicInstruction {
        variant_index,
        variant_name: variant_name.to_string(),
        fields,
    };

    risc0_zkvm::serde::to_vec(&instruction)
        .map_err(|e| SerializeError::Risc0(e.to_string()))
}
```

**Note on signature change**: The function now takes `variant_name` and field names. Check the call site in `tx.rs` (~line 120-123) — the instruction name and arg names are available from the IDL. Update accordingly.

### Task 7: Update `tx.rs` call site

The current call site at `tx.rs:121-123`:
```rust
let ix_index = idl.instructions.iter().position(|i| i.name == ix.name).unwrap_or(0);
let risc0_args: Vec<_> = parsed_args.iter().map(|(_, ty, val)| (*ty, val)).collect();
let instruction_data = serialize_to_risc0(ix_index as u32, &risc0_args);
```

Change to:
```rust
let ix_index = idl.instructions.iter().position(|i| i.name == ix.name).unwrap_or(0);
let variant_name = to_pascal_case_str(&ix.name);
let risc0_args: Vec<_> = parsed_args.iter()
    .map(|(name, ty, val)| (name.as_str(), *ty, val))
    .collect();
let instruction_data = serialize_to_risc0(ix_index as u32, &variant_name, &risc0_args)
    .unwrap_or_else(|e| {
        eprintln!("  Serialization error: {}", e);
        std::process::exit(1);
    });
```

### Task 8: Tests

The existing roundtrip tests (`serialize_deserialize_roundtrip_with_bytes32`, `risc0_reference_bytes32_format`, etc.) should continue to pass with the new implementation. They are the primary regression safety net.

Add additional roundtrip tests for:
- All primitive types: `bool`, `u8`, `u32`, `u64`, `u128`, `String`
- `[u32; 8]` (ProgramId)
- `Vec<u8>`, `Vec<u32>`
- `Vec<[u8; 32]>` (ByteArrayVec)
- `Option<u32>` — both Some and None
- Multi-field instruction with mixed types
- Error cases: type mismatches return `Err`, not silent truncation

### Task 9: Delete dead code

After the rewrite, `serialize_bytes_padded`, `serialize_primitive_risc0`, `serialize_array_risc0`, and `serialize_vec_risc0` are all dead code. Remove them.

## Verification

```bash
# Build (requires nightly)
cargo +nightly build -p spel

# Run serialize tests
cargo +nightly test -p spel serialize::tests

# Run all spel-cli tests
cargo +nightly test -p spel

# Run framework tests (no regressions)
cargo test -p spel-framework-core -p spel-framework-macros -p spel-client-gen
```

The critical test is `serialize_deserialize_roundtrip_with_bytes32` — if it passes, format compatibility with LEZ v0.2.0-rc1 is proven.

## Out of scope

- **`Defined` type support**: Custom struct/enum args from the IDL. The conversion function should return `SerializeError::UnsupportedType` for now. Can be added later by looking up type definitions in `SpelIdl::types`.
- **Per-program CLI code generation** (Approach C): Maximum type safety but requires architecture change. Worth considering long-term.
- **`IdlType::Primitive` as an enum instead of `String`**: Would catch invalid type names at parse time. Good follow-up but not required for this change.
