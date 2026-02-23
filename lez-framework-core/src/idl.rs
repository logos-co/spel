//! IDL (Interface Definition Language) types for LEZ programs.
//!
//! The proc-macro generates an IDL JSON file at compile time that
//! describes the program's interface. This module defines the
//! serializable IDL format.
//!
//! ## LSSA-lang compatibility
//!
//! This IDL format is a superset of the lssa-lang IDL spec. Fields like
//! `discriminator`, `execution`, and `visibility` are included for
//! compatibility with lssa-lang tooling. All new fields are optional
//! and backward-compatible with existing LEZ programs.

use serde::{Deserialize, Serialize};

/// Top-level IDL for an LEZ program.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LezIdl {
    pub version: String,
    pub name: String,
    pub instructions: Vec<IdlInstruction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accounts: Vec<IdlAccountType>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub types: Vec<IdlTypeDef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<IdlError>,
    /// IDL spec identifier (lssa-lang compat).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<String>,
    /// Program metadata (lssa-lang compat).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<IdlMetadata>,
}

/// Program metadata (lssa-lang compat).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdlMetadata {
    pub name: String,
    pub version: String,
}

/// Execution mode for an instruction (lssa-lang compat).
///
/// Maps to lssa-lang's `Execution` type which has `public` and `private_owned` flags.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IdlExecution {
    #[serde(default)]
    pub public: bool,
    #[serde(default)]
    pub private_owned: bool,
}

/// An instruction in the IDL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdlInstruction {
    pub name: String,
    pub accounts: Vec<IdlAccountItem>,
    pub args: Vec<IdlArg>,
    /// SHA256("global:{name}")[..8] discriminator (lssa-lang compat).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discriminator: Option<Vec<u8>>,
    /// Execution mode (lssa-lang compat). Defaults to public.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<IdlExecution>,
    /// Variant name in PascalCase (lssa-lang compat).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
}

/// An account expected by an instruction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdlAccountItem {
    pub name: String,
    #[serde(default)]
    pub writable: bool,
    #[serde(default)]
    pub signer: bool,
    #[serde(default)]
    pub init: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pda: Option<IdlPda>,
    /// If true, this account represents a variable-length trailing list.
    #[serde(default, skip_serializing_if = "is_false")]
    pub rest: bool,
    /// Visibility tags (lssa-lang compat). e.g. ["public"].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub visibility: Vec<String>,
}

fn is_false(v: &bool) -> bool { !v }

/// PDA derivation specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdlPda {
    pub seeds: Vec<IdlSeed>,
}

/// A seed component for PDA derivation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum IdlSeed {
    #[serde(rename = "const")]
    Const { value: String },
    #[serde(rename = "account")]
    Account { path: String },
    #[serde(rename = "arg")]
    Arg { path: String },
}

/// An instruction argument.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdlArg {
    pub name: String,
    #[serde(rename = "type")]
    pub type_: IdlType,
}

/// Type representation in the IDL.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum IdlType {
    Primitive(String),
    Vec { vec: Box<IdlType> },
    Option { option: Box<IdlType> },
    Defined { defined: String },
    Array { array: (Box<IdlType>, usize) },
}

/// Account type definition in the IDL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdlAccountType {
    pub name: String,
    #[serde(rename = "type")]
    pub type_: IdlTypeDef,
}

/// Type definition (struct or enum).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdlTypeDef {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<IdlField>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<IdlEnumVariant>,
}

/// A field in a struct type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdlField {
    pub name: String,
    #[serde(rename = "type")]
    pub type_: IdlType,
}

/// An enum variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdlEnumVariant {
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<IdlField>,
}

/// Error definition in the IDL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdlError {
    pub code: u32,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg: Option<String>,
}

/// Compute the lssa-lang discriminator for an instruction name.
///
/// This is SHA256("global:{name}")[..8], matching lssa-lang's convention.
pub fn compute_discriminator(name: &str) -> Vec<u8> {
    use sha2::{Sha256, Digest};
    let mut hasher = Sha256::new();
    hasher.update(format!("global:{}", name).as_bytes());
    let result = hasher.finalize();
    result[..8].to_vec()
}

impl LezIdl {
    /// Create a new IDL with the given program name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            version: "0.1.0".to_string(),
            name: name.into(),
            instructions: vec![],
            accounts: vec![],
            types: vec![],
            errors: vec![],
            spec: None,
            metadata: None,
        }
    }

    /// Serialize the IDL to pretty-printed JSON.
    pub fn to_json_pretty(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}
