use crate::{
    parse::{parse_program_id, parse_value, ParsedValue},
    serialize::SerializeError,
};
use nssa::AccountId;
use nssa_core::{program::ProgramId, NullifierPublicKey};
use serde::ser::{SerializeSeq, SerializeTuple, SerializeTupleVariant};
use serde::Serialize;
use serde_json::Value;
use spel_framework_core::{idl::IdlType, pda::parse_bytes32};

#[derive(Debug)]
pub(crate) struct ValueParseError {
    pub(crate) path: Vec<usize>,
    pub(crate) reason: String,
}

impl ValueParseError {
    fn new(path: Vec<usize>, reason: impl Into<String>) -> Self {
        Self {
            path,
            reason: reason.into(),
        }
    }
}

pub(crate) enum ArgumentValue {
    Bool(bool),
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
    U128(u128),
    I8(i8),
    I16(i16),
    I32(i32),
    I64(i64),
    I128(i128),
    String(String),
    AccountId(AccountId),
    ProgramId(ProgramId),
    NullifierPublicKey(NullifierPublicKey),
    Array(Vec<Self>),
    Vec(Vec<Self>),
    Option(Option<Box<Self>>),
}

impl ArgumentValue {
    pub(crate) fn seed_bytes(&self, ty: &IdlType) -> Result<[u8; 32], &'static str> {
        match (self, ty) {
            (Self::Array(values), IdlType::Array { array })
                if matches!(array.0.as_ref(), IdlType::Primitive(primitive) if primitive == "u8")
                    && (1..=32).contains(&array.1) =>
            {
                let mut seed = [0; 32];
                for (index, value) in values.iter().enumerate() {
                    let Self::U8(value) = value else {
                        return Err("unsupported argument type for PDA seed");
                    };
                    seed[index] = *value;
                }
                Ok(seed)
            },
            (Self::U8(value), IdlType::Primitive(primitive)) if primitive == "u8" => {
                Ok(pad_seed(&value.to_le_bytes()))
            },
            (Self::U16(value), IdlType::Primitive(primitive)) if primitive == "u16" => {
                Ok(pad_seed(&value.to_le_bytes()))
            },
            (Self::U32(value), IdlType::Primitive(primitive)) if primitive == "u32" => {
                Ok(pad_seed(&value.to_le_bytes()))
            },
            (Self::U64(value), IdlType::Primitive(primitive)) if primitive == "u64" => {
                Ok(pad_seed(&value.to_le_bytes()))
            },
            (Self::U128(value), IdlType::Primitive(primitive)) if primitive == "u128" => {
                Ok(pad_seed(&value.to_le_bytes()))
            },
            (Self::String(value), IdlType::Primitive(primitive))
                if matches!(primitive.as_str(), "string" | "String") =>
            {
                if value.len() > 32 {
                    return Err("string argument exceeds 32 bytes for PDA seed");
                }
                Ok(pad_seed(value.as_bytes()))
            },
            (Self::AccountId(value), IdlType::Primitive(primitive))
                if primitive == "account_id" =>
            {
                Ok(*value.value())
            },
            (Self::ProgramId(value), IdlType::Primitive(primitive))
                if primitive == "program_id" =>
            {
                let mut seed = [0; 32];
                for (index, word) in value.iter().enumerate() {
                    seed[index * 4..(index + 1) * 4].copy_from_slice(&word.to_le_bytes());
                }
                Ok(seed)
            },
            _ => Err("unsupported argument type for PDA seed"),
        }
    }
}

impl Serialize for ArgumentValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Bool(value) => serializer.serialize_bool(*value),
            Self::U8(value) => serializer.serialize_u8(*value),
            Self::U16(value) => serializer.serialize_u16(*value),
            Self::U32(value) => serializer.serialize_u32(*value),
            Self::U64(value) => serializer.serialize_u64(*value),
            Self::U128(value) => serializer.serialize_u128(*value),
            Self::I8(value) => serializer.serialize_i8(*value),
            Self::I16(value) => serializer.serialize_i16(*value),
            Self::I32(value) => serializer.serialize_i32(*value),
            Self::I64(value) => serializer.serialize_i64(*value),
            Self::I128(value) => serializer.serialize_i128(*value),
            Self::String(value) => serializer.serialize_str(value),
            Self::AccountId(value) => value.serialize(serializer),
            Self::ProgramId(value) => value.serialize(serializer),
            Self::NullifierPublicKey(value) => value.serialize(serializer),
            Self::Array(values) => {
                let mut tuple = serializer.serialize_tuple(values.len())?;
                for value in values {
                    tuple.serialize_element(value)?;
                }
                tuple.end()
            },
            Self::Vec(values) => {
                let mut sequence = serializer.serialize_seq(Some(values.len()))?;
                for value in values {
                    sequence.serialize_element(value)?;
                }
                sequence.end()
            },
            Self::Option(None) => serializer.serialize_none(),
            Self::Option(Some(value)) => serializer.serialize_some(value),
        }
    }
}

pub(crate) fn validate_type(ty: &IdlType) -> Result<(), String> {
    match ty {
        IdlType::Primitive(primitive) if is_supported_primitive(primitive) => Ok(()),
        IdlType::Primitive(_) => Err("unsupported instruction argument primitive".to_string()),
        IdlType::Array { array } => validate_type(&array.0),
        IdlType::Vec { vec } => validate_type(vec),
        IdlType::Option { option } if matches!(option.as_ref(), IdlType::Option { .. }) => {
            Err("immediate nested option is unsupported".to_string())
        },
        IdlType::Option { option } => validate_type(option),
        IdlType::Defined { .. } => {
            Err("defined instruction argument types are unsupported".to_string())
        },
    }
}

pub(crate) fn validate_seed_type(ty: &IdlType) -> Result<(), &'static str> {
    match ty {
        IdlType::Array { array }
            if matches!(array.0.as_ref(), IdlType::Primitive(primitive) if primitive == "u8")
                && (1..=32).contains(&array.1) =>
        {
            Ok(())
        },
        IdlType::Primitive(primitive)
            if matches!(
                primitive.as_str(),
                "u8" | "u16"
                    | "u32"
                    | "u64"
                    | "u128"
                    | "string"
                    | "String"
                    | "account_id"
                    | "program_id"
            ) =>
        {
            Ok(())
        },
        _ => Err("unsupported argument type for PDA seed"),
    }
}

pub(crate) fn parse_argument(raw: &str, ty: &IdlType) -> Result<ArgumentValue, ValueParseError> {
    match ty {
        IdlType::Array { .. } | IdlType::Vec { .. } | IdlType::Option { .. } => {
            match serde_json::from_str(raw) {
                Ok(value) => match parse_json_value(&value, ty, vec![]) {
                    Ok(value) => Ok(value),
                    Err(json_error) => parse_legacy_argument(raw, ty).or(Err(json_error)),
                },
                Err(_) => parse_legacy_argument(raw, ty),
            }
        },
        _ => parse_scalar(raw, ty, vec![]),
    }
}

pub(crate) fn serialize_instruction(
    variant_index: u32,
    fields: &[&ArgumentValue],
) -> Result<Vec<u32>, SerializeError> {
    let instruction = SerializedInstruction {
        variant_index,
        fields,
    };

    risc0_zkvm::serde::to_vec(&instruction)
        .map_err(|error| SerializeError::Risc0(error.to_string()))
}

fn parse_json_value(
    value: &Value,
    ty: &IdlType,
    path: Vec<usize>,
) -> Result<ArgumentValue, ValueParseError> {
    match ty {
        IdlType::Primitive(primitive) => parse_json_primitive(value, primitive, path),
        IdlType::Array { array } => {
            let values = value
                .as_array()
                .ok_or_else(|| ValueParseError::new(path.clone(), "expected JSON array"))?;
            if values.len() != array.1 {
                return Err(ValueParseError::new(
                    path,
                    "JSON array has an unexpected length",
                ));
            }

            let mut parsed = Vec::with_capacity(values.len());
            for (index, value) in values.iter().enumerate() {
                let mut nested_path = path.clone();
                nested_path.push(index);
                parsed.push(parse_json_value(value, &array.0, nested_path)?);
            }
            Ok(ArgumentValue::Array(parsed))
        },
        IdlType::Vec { vec } => {
            let values = value
                .as_array()
                .ok_or_else(|| ValueParseError::new(path.clone(), "expected JSON array"))?;
            let mut parsed = Vec::with_capacity(values.len());
            for (index, value) in values.iter().enumerate() {
                let mut nested_path = path.clone();
                nested_path.push(index);
                parsed.push(parse_json_value(value, vec, nested_path)?);
            }
            Ok(ArgumentValue::Vec(parsed))
        },
        IdlType::Option { option } if value.is_null() => Ok(ArgumentValue::Option(None)),
        IdlType::Option { option } => Ok(ArgumentValue::Option(Some(Box::new(parse_json_value(
            value, option, path,
        )?)))),
        IdlType::Defined { .. } => Err(ValueParseError::new(
            path,
            "defined instruction argument types are unsupported",
        )),
    }
}

fn parse_json_primitive(
    value: &Value,
    primitive: &str,
    path: Vec<usize>,
) -> Result<ArgumentValue, ValueParseError> {
    match primitive {
        "bool" => value
            .as_bool()
            .map(ArgumentValue::Bool)
            .ok_or_else(|| ValueParseError::new(path, "expected JSON boolean")),
        "string" | "String" | "account_id" | "program_id" | "nullifier_public_key" => value
            .as_str()
            .ok_or_else(|| ValueParseError::new(path.clone(), "expected JSON string"))
            .and_then(|raw| parse_primitive(raw, primitive, path)),
        primitive if is_integer_primitive(primitive) => value
            .as_number()
            .ok_or_else(|| ValueParseError::new(path.clone(), "expected JSON integer"))
            .and_then(|number| parse_primitive(&number.to_string(), primitive, path)),
        _ => Err(ValueParseError::new(
            path,
            "unsupported instruction argument primitive",
        )),
    }
}

fn parse_scalar(
    raw: &str,
    ty: &IdlType,
    path: Vec<usize>,
) -> Result<ArgumentValue, ValueParseError> {
    let IdlType::Primitive(primitive) = ty else {
        return Err(ValueParseError::new(
            path,
            "expected instruction argument primitive",
        ));
    };
    parse_primitive(raw, primitive, path)
}

fn parse_legacy_argument(raw: &str, ty: &IdlType) -> Result<ArgumentValue, ValueParseError> {
    match ty {
        IdlType::Array { .. } | IdlType::Vec { .. } => {
            let value = parse_value(raw, ty)
                .map_err(|_| invalid_legacy_value(legacy_error_path(raw, ty)))?;
            argument_value_from_cli(value, ty)
        },
        IdlType::Option { option } => {
            if matches!(raw, "none" | "null") || raw.is_empty() {
                return Ok(ArgumentValue::Option(None));
            }
            parse_argument(raw, option).map(|value| ArgumentValue::Option(Some(Box::new(value))))
        },
        IdlType::Primitive(_) | IdlType::Defined { .. } => Err(invalid_legacy_value(vec![])),
    }
}

fn invalid_legacy_value(path: Vec<usize>) -> ValueParseError {
    ValueParseError::new(path, "invalid legacy CLI value")
}

fn legacy_error_path(raw: &str, ty: &IdlType) -> Vec<usize> {
    match ty {
        IdlType::Array { array } if matches!(array.0.as_ref(), IdlType::Primitive(primitive) if primitive == "u32") => {
            csv_error_path(raw, Some(array.1), |value| value.parse::<u32>().is_ok())
        },
        IdlType::Vec { vec } => match vec.as_ref() {
            IdlType::Primitive(primitive) if primitive == "u8" => {
                csv_error_path(raw, None, |value| value.parse::<u8>().is_ok())
            },
            IdlType::Primitive(primitive) if primitive == "u32" => {
                csv_error_path(raw, None, |value| value.parse::<u32>().is_ok())
            },
            IdlType::Array { array } if matches!(array.0.as_ref(), IdlType::Primitive(primitive) if primitive == "u8") => {
                csv_error_path(raw, None, |value| {
                    legacy_byte_array_is_valid(value, array.1)
                })
            },
            _ => Vec::new(),
        },
        IdlType::Primitive(_) | IdlType::Option { .. } | IdlType::Defined { .. } => Vec::new(),
        IdlType::Array { .. } => Vec::new(),
    }
}

fn csv_error_path<F>(raw: &str, expected_len: Option<usize>, is_valid: F) -> Vec<usize>
where
    F: Fn(&str) -> bool,
{
    let values: Vec<_> = raw.split(',').map(str::trim).collect();
    if expected_len.is_some_and(|expected_len| values.len() != expected_len) {
        return Vec::new();
    }
    values
        .into_iter()
        .position(|value| !is_valid(value))
        .map_or_else(Vec::new, |index| vec![index])
}

fn legacy_byte_array_is_valid(raw: &str, element_len: usize) -> bool {
    if element_len == 32 {
        return crate::hex::decode_bytes_32(raw).is_ok();
    }

    let hex = raw
        .strip_prefix("0x")
        .or_else(|| raw.strip_prefix("0X"))
        .unwrap_or(raw);
    crate::hex::hex_decode(hex).is_ok_and(|bytes| bytes.len() == element_len)
}

fn argument_value_from_cli(
    value: ParsedValue,
    ty: &IdlType,
) -> Result<ArgumentValue, ValueParseError> {
    match (value, ty) {
        (ParsedValue::ByteArray(value), IdlType::Array { array })
            if matches!(array.0.as_ref(), IdlType::Primitive(primitive) if primitive == "u8")
                && value.len() == array.1 =>
        {
            Ok(ArgumentValue::Array(
                value.into_iter().map(ArgumentValue::U8).collect(),
            ))
        },
        (ParsedValue::U32Array(value), IdlType::Array { array })
            if matches!(array.0.as_ref(), IdlType::Primitive(primitive) if primitive == "u32")
                && value.len() == array.1 =>
        {
            Ok(ArgumentValue::Array(
                value.into_iter().map(ArgumentValue::U32).collect(),
            ))
        },
        (ParsedValue::ByteArray(value), IdlType::Vec { vec }) if matches!(vec.as_ref(), IdlType::Primitive(primitive) if primitive == "u8") => {
            Ok(ArgumentValue::Vec(
                value.into_iter().map(ArgumentValue::U8).collect(),
            ))
        },
        (ParsedValue::Raw(value), IdlType::Vec { vec })
            if value.is_empty()
                && matches!(vec.as_ref(), IdlType::Primitive(primitive) if primitive == "u32") =>
        {
            Ok(ArgumentValue::Vec(Vec::new()))
        },
        (ParsedValue::Raw(value), ty) => Err(invalid_legacy_value(legacy_error_path(&value, ty))),
        (ParsedValue::U32Array(value), IdlType::Vec { vec }) if matches!(vec.as_ref(), IdlType::Primitive(primitive) if primitive == "u32") => {
            Ok(ArgumentValue::Vec(
                value.into_iter().map(ArgumentValue::U32).collect(),
            ))
        },
        (ParsedValue::ByteArrayVec(values), IdlType::Vec { vec }) => {
            let IdlType::Array { array } = vec.as_ref() else {
                return Err(invalid_legacy_value(vec![]));
            };
            if !matches!(array.0.as_ref(), IdlType::Primitive(primitive) if primitive == "u8")
                || values.iter().any(|value| value.len() != array.1)
            {
                return Err(invalid_legacy_value(vec![]));
            }
            Ok(ArgumentValue::Vec(
                values
                    .into_iter()
                    .map(|value| {
                        ArgumentValue::Array(value.into_iter().map(ArgumentValue::U8).collect())
                    })
                    .collect(),
            ))
        },
        _ => Err(invalid_legacy_value(vec![])),
    }
}

fn parse_primitive(
    raw: &str,
    primitive: &str,
    path: Vec<usize>,
) -> Result<ArgumentValue, ValueParseError> {
    macro_rules! parse_unsigned {
        ($ty:ty, $variant:ident) => {{
            if !is_decimal(raw, false) {
                return Err(ValueParseError::new(
                    path,
                    "invalid unsigned decimal integer",
                ));
            }
            raw.parse::<$ty>()
                .map(ArgumentValue::$variant)
                .map_err(|_| ValueParseError::new(path, "integer is outside the supported range"))
        }};
    }

    macro_rules! parse_signed {
        ($ty:ty, $variant:ident) => {{
            if !is_decimal(raw, true) {
                return Err(ValueParseError::new(path, "invalid signed decimal integer"));
            }
            raw.parse::<$ty>()
                .map(ArgumentValue::$variant)
                .map_err(|_| ValueParseError::new(path, "integer is outside the supported range"))
        }};
    }

    match primitive {
        "bool" => match raw {
            "true" | "1" | "yes" => Ok(ArgumentValue::Bool(true)),
            "false" | "0" | "no" => Ok(ArgumentValue::Bool(false)),
            _ => Err(ValueParseError::new(path, "invalid boolean")),
        },
        "u8" => parse_unsigned!(u8, U8),
        "u16" => parse_unsigned!(u16, U16),
        "u32" => parse_unsigned!(u32, U32),
        "u64" => parse_unsigned!(u64, U64),
        "u128" => parse_unsigned!(u128, U128),
        "i8" => parse_signed!(i8, I8),
        "i16" => parse_signed!(i16, I16),
        "i32" => parse_signed!(i32, I32),
        "i64" => parse_signed!(i64, I64),
        "i128" => parse_signed!(i128, I128),
        "string" | "String" => Ok(ArgumentValue::String(raw.to_owned())),
        "account_id" => parse_bytes32(raw)
            .map(AccountId::new)
            .map(ArgumentValue::AccountId)
            .map_err(|_| ValueParseError::new(path, "invalid account ID")),
        "program_id" => parse_program_id(raw)
            .map(ArgumentValue::ProgramId)
            .map_err(|_| ValueParseError::new(path, "invalid program ID")),
        "nullifier_public_key" => parse_bytes32(raw)
            .map(NullifierPublicKey)
            .map(ArgumentValue::NullifierPublicKey)
            .map_err(|_| ValueParseError::new(path, "invalid nullifier public key")),
        _ => Err(ValueParseError::new(
            path,
            "unsupported instruction argument primitive",
        )),
    }
}

fn is_supported_primitive(primitive: &str) -> bool {
    matches!(
        primitive,
        "bool"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "string"
            | "String"
            | "account_id"
            | "program_id"
            | "nullifier_public_key"
    )
}

fn is_integer_primitive(primitive: &str) -> bool {
    matches!(
        primitive,
        "u8" | "u16" | "u32" | "u64" | "u128" | "i8" | "i16" | "i32" | "i64" | "i128"
    )
}

fn is_decimal(raw: &str, signed: bool) -> bool {
    let digits = if signed {
        raw.strip_prefix('+')
            .or_else(|| raw.strip_prefix('-'))
            .unwrap_or(raw)
    } else {
        raw.strip_prefix('+').unwrap_or(raw)
    };
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

fn pad_seed(value: &[u8]) -> [u8; 32] {
    let mut seed = [0; 32];
    seed[..value.len()].copy_from_slice(value);
    seed
}

struct SerializedInstruction<'a> {
    variant_index: u32,
    fields: &'a [&'a ArgumentValue],
}

impl Serialize for SerializedInstruction<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut instruction =
            serializer.serialize_tuple_variant("", self.variant_index, "", self.fields.len())?;
        for field in self.fields {
            instruction.serialize_field(field)?;
        }
        instruction.end()
    }
}

#[cfg(test)]
mod tests {
    use nssa::AccountId;
    use risc0_zkvm::serde::Deserializer;
    use serde::Deserialize;

    use super::*;

    fn primitive(name: &str) -> IdlType {
        IdlType::Primitive(name.to_string())
    }

    fn array(element: IdlType, len: usize) -> IdlType {
        IdlType::Array {
            array: (Box::new(element), len),
        }
    }

    fn vector(element: IdlType) -> IdlType {
        IdlType::Vec {
            vec: Box::new(element),
        }
    }

    fn option(element: IdlType) -> IdlType {
        IdlType::Option {
            option: Box::new(element),
        }
    }

    #[test]
    fn parses_every_supported_scalar_primitive() {
        for (raw, name) in [
            ("true", "bool"),
            ("+1", "u8"),
            ("+1", "u16"),
            ("+1", "u32"),
            ("+1", "u64"),
            ("+1", "u128"),
            ("-1", "i8"),
            ("-1", "i16"),
            ("-1", "i32"),
            ("-1", "i64"),
            ("-1", "i128"),
        ] {
            assert!(parse_argument(raw, &primitive(name)).is_ok(), "{name}");
        }
        assert!(parse_argument("raw string", &primitive("string")).is_ok());
        assert!(parse_argument("raw string", &primitive("String")).is_ok());
        assert!(parse_argument(&"11".repeat(32), &primitive("account_id")).is_ok());
        assert!(parse_argument(&"01000000".repeat(8), &primitive("program_id")).is_ok());
        assert!(parse_argument(&"22".repeat(32), &primitive("nullifier_public_key")).is_ok());

        assert!(parse_argument("maybe", &primitive("bool")).is_err());
        assert!(parse_argument(" 1", &primitive("u32")).is_err());
        assert!(parse_argument("0x10", &primitive("u32")).is_err());
    }

    #[test]
    fn accepts_cli_boolean_aliases() {
        for (raw, expected) in [("1", true), ("yes", true), ("0", false), ("no", false)] {
            assert!(matches!(
                parse_argument(raw, &primitive("bool")),
                Ok(ArgumentValue::Bool(value)) if value == expected
            ));
        }
    }

    #[test]
    fn accepts_cli_program_id_aliases() {
        let bytes = [0x11; 32];
        let expected = parse_program_id(&format!("0x{}", "11".repeat(32))).unwrap();
        let inputs = [
            format!("0x{}", "11".repeat(32)),
            AccountId::new(bytes).to_string(),
        ];

        for input in inputs {
            assert!(matches!(
                parse_argument(&input, &primitive("program_id")),
                Ok(ArgumentValue::ProgramId(value)) if value == expected
            ));
        }
    }

    #[test]
    fn accepts_cli_array_vector_and_option_forms() {
        let byte_array = array(primitive("u8"), 3);
        let parsed = parse_argument("abc", &byte_array).unwrap();
        assert!(matches!(
            parsed,
            ArgumentValue::Array(values)
                if matches!(values.as_slice(), [
                    ArgumentValue::U8(b'a'),
                    ArgumentValue::U8(b'b'),
                    ArgumentValue::U8(b'c'),
                ])
        ));

        let word_array = array(primitive("u32"), 3);
        let parsed = parse_argument("1, 2, 3", &word_array).unwrap();
        assert!(matches!(
            parsed,
            ArgumentValue::Array(values)
                if matches!(values.as_slice(), [
                    ArgumentValue::U32(1),
                    ArgumentValue::U32(2),
                    ArgumentValue::U32(3),
                ])
        ));

        let byte_vector = vector(primitive("u8"));
        let parsed = parse_argument("1, 2, 3", &byte_vector).unwrap();
        assert!(matches!(
            parsed,
            ArgumentValue::Vec(values)
                if matches!(values.as_slice(), [
                    ArgumentValue::U8(1),
                    ArgumentValue::U8(2),
                    ArgumentValue::U8(3),
                ])
        ));

        let word_vector = vector(primitive("u32"));
        let parsed = parse_argument("1, 2, 3", &word_vector).unwrap();
        assert!(matches!(
            parsed,
            ArgumentValue::Vec(values)
                if matches!(values.as_slice(), [
                    ArgumentValue::U32(1),
                    ArgumentValue::U32(2),
                    ArgumentValue::U32(3),
                ])
        ));

        let byte_array_vector = vector(array(primitive("u8"), 2));
        let parsed = parse_argument("0102,0304", &byte_array_vector).unwrap();
        let ArgumentValue::Vec(values) = parsed else {
            panic!("expected byte-array vector");
        };
        let [ArgumentValue::Array(first), ArgumentValue::Array(second)] = values.as_slice() else {
            panic!("expected two byte arrays");
        };
        assert!(matches!(
            first.as_slice(),
            [ArgumentValue::U8(1), ArgumentValue::U8(2)]
        ));
        assert!(matches!(
            second.as_slice(),
            [ArgumentValue::U8(3), ArgumentValue::U8(4)]
        ));

        let optional_byte = option(primitive("u8"));
        assert!(matches!(
            parse_argument("1", &optional_byte),
            Ok(ArgumentValue::Option(Some(value))) if matches!(*value, ArgumentValue::U8(1))
        ));
        for raw in ["none", ""] {
            assert!(matches!(
                parse_argument(raw, &optional_byte),
                Ok(ArgumentValue::Option(None))
            ));
        }
    }

    #[test]
    fn rejects_partially_invalid_cli_csv() {
        for ty in [vector(primitive("u8")), vector(primitive("u32"))] {
            assert!(parse_argument("1,invalid,3", &ty).is_err());
        }
    }

    #[test]
    fn preserves_legacy_csv_error_paths() {
        let cases = [
            ("1,invalid,3", vector(primitive("u8"))),
            ("1,invalid,3", vector(primitive("u32"))),
            ("1,invalid,3", array(primitive("u32"), 3)),
            ("0102,invalid", vector(array(primitive("u8"), 2))),
        ];

        for (raw, ty) in cases {
            let Err(error) = parse_argument(raw, &ty) else {
                panic!("expected invalid legacy CSV: {raw}");
            };
            assert_eq!(error.path, vec![1], "{raw}");
            assert_eq!(error.reason, "invalid legacy CLI value");
        }
    }

    #[test]
    fn serializes_cli_and_json_container_forms_identically() {
        let bytes = vector(primitive("u8"));
        let cli_value = parse_argument("1, 2, 3", &bytes).unwrap();
        let json_value = parse_argument("[1, 2, 3]", &bytes).unwrap();

        assert_eq!(
            serialize_instruction(0, &[&cli_value]).unwrap(),
            serialize_instruction(0, &[&json_value]).unwrap(),
        );
    }

    #[test]
    fn serializes_cli_parser_forms_identically() {
        fn assert_matches_cli(raw: &str, ty: IdlType) {
            let cli_value = parse_value(raw, &ty).unwrap();
            let cli_words = crate::serialize::serialize_to_risc0(0, &[(&ty, &cli_value)]).unwrap();
            let resolver_value = parse_argument(raw, &ty).unwrap();
            assert_eq!(
                serialize_instruction(0, &[&resolver_value]).unwrap(),
                cli_words,
                "{raw}"
            );
        }

        assert_matches_cli("yes", primitive("bool"));
        assert_matches_cli("abc", array(primitive("u8"), 3));
        assert_matches_cli("1, 2, 3", array(primitive("u32"), 3));
        assert_matches_cli("1, 2, 3", vector(primitive("u8")));
        assert_matches_cli("1, 2, 3", vector(primitive("u32")));
        assert_matches_cli("", vector(primitive("u32")));
        assert_matches_cli("0102,0304", vector(array(primitive("u8"), 2)));
        assert_matches_cli("1", option(primitive("u8")));
        assert_matches_cli("1,2,3,4,5,6,7,8", primitive("program_id"));
    }

    #[test]
    fn serializes_id_like_primitives_with_their_upstream_shapes() {
        #[derive(Debug, Deserialize, PartialEq)]
        enum TestInstruction {
            Execute {
                account_id: AccountId,
                program_id: ProgramId,
                nullifier_public_key: NullifierPublicKey,
                signed: i128,
            },
        }

        let account_id = parse_argument(&"11".repeat(32), &primitive("account_id")).unwrap();
        let program_id = parse_argument("1,2,3,4,5,6,7,8", &primitive("program_id")).unwrap();
        let nullifier_public_key =
            parse_argument(&"22".repeat(32), &primitive("nullifier_public_key")).unwrap();
        let signed = parse_argument(
            "-170141183460469231731687303715884105728",
            &primitive("i128"),
        )
        .unwrap();
        let words = serialize_instruction(
            0,
            &[&account_id, &program_id, &nullifier_public_key, &signed],
        )
        .unwrap();
        let decoded = TestInstruction::deserialize(&mut Deserializer::new(words.as_ref())).unwrap();

        assert_eq!(
            decoded,
            TestInstruction::Execute {
                account_id: AccountId::new([0x11; 32]),
                program_id: [1, 2, 3, 4, 5, 6, 7, 8],
                nullifier_public_key: NullifierPublicKey([0x22; 32]),
                signed: i128::MIN,
            }
        );
    }

    #[test]
    fn validates_and_encodes_pda_seed_argument_types() {
        let unsigned = primitive("u16");
        let value = parse_argument("513", &unsigned).unwrap();
        let seed = value.seed_bytes(&unsigned).unwrap();
        assert_eq!(&seed[..2], &513_u16.to_le_bytes());
        assert!(seed[2..].iter().all(|byte| *byte == 0));

        let byte_array = IdlType::Array {
            array: (Box::new(primitive("u8")), 3),
        };
        let value = parse_argument("[1,2,3]", &byte_array).unwrap();
        assert_eq!(value.seed_bytes(&byte_array).unwrap()[..3], [1, 2, 3]);

        let program = primitive("program_id");
        let value = parse_argument("1,2,3,4,5,6,7,8", &program).unwrap();
        let seed = value.seed_bytes(&program).unwrap();
        assert_eq!(&seed[..4], &1_u32.to_le_bytes());
        assert_eq!(&seed[28..], &8_u32.to_le_bytes());

        let signed = primitive("i8");
        let value = parse_argument("-1", &signed).unwrap();
        assert!(validate_seed_type(&signed).is_err());
        assert!(value.seed_bytes(&signed).is_err());

        let long_string = primitive("string");
        let value = parse_argument(&"x".repeat(33), &long_string).unwrap();
        assert!(value.seed_bytes(&long_string).is_err());

        let nested_option = IdlType::Option {
            option: Box::new(IdlType::Option {
                option: Box::new(primitive("u8")),
            }),
        };
        assert!(validate_type(&nested_option).is_err());
    }
}
