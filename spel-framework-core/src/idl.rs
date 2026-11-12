//! IDL (Interface Definition Language) types for programs on LEZ's actor model.
//!
//! An actor program has three things a client needs to know: the messages it accepts (one
//! Borsh enum, the variant index is the tag), the state an actor of it holds, and the events
//! it emits. Who receives a message, who signs and which actors a conversation reaches are the
//! caller's business, not the program's, so the IDL says nothing about them.
//!
//! The format is version 2. Version 1 described instructions with account lists for the
//! retired `#[lez_program]` framework; [`SpelIdl::from_json`] refuses it with a clear error.

use serde::{Deserialize, Serialize};

/// The IDL format version this crate reads and writes.
pub const IDL_VERSION: &str = "2";

/// Top-level IDL for a program.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpelIdl {
    /// Always [`IDL_VERSION`].
    pub version: String,
    /// The program's name, also its event namespace (`token` for `token_core`).
    pub name: String,
    /// The external message enum's variants, in enum order.
    pub messages: Vec<IdlMessage>,
    /// The type of an actor's state, if the program declares one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<IdlType>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<IdlEvent>,
    /// Every struct and enum the messages, state and events reference, sorted by name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub types: Vec<IdlTypeDef>,
}

/// One variant of the message enum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdlMessage {
    /// The variant name, e.g. `Transfer`.
    pub name: String,
    /// The Borsh tag: the variant's index in the enum.
    pub tag: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docs: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<IdlField>,
}

/// An event a program emits, identified on chain by its selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdlEvent {
    pub name: String,
    /// `sha256("<namespace>::<name>")[..8]`, hex.
    pub selector: String,
    #[serde(rename = "type")]
    pub type_: IdlType,
}

/// Type representation in the IDL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum IdlType {
    Primitive(String),
    Vec { vec: Box<IdlType> },
    Option { option: Box<IdlType> },
    Defined { defined: String },
    Array { array: (Box<IdlType>, usize) },
}

/// A struct or enum definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdlTypeDef {
    pub name: String,
    /// `"struct"` or `"enum"`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<IdlField>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<IdlEnumVariant>,
}

/// A field of a struct, an enum variant or a message. Positional (tuple) fields are named by
/// their index: `"0"`, `"1"`, ….
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdlField {
    pub name: String,
    #[serde(rename = "type")]
    pub type_: IdlType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docs: Option<String>,
}

impl IdlField {
    /// Whether this is a tuple field, named by its position.
    pub fn is_positional(&self) -> bool {
        self.name.bytes().all(|b| b.is_ascii_digit())
    }
}

/// An enum variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdlEnumVariant {
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<IdlField>,
}

impl SpelIdl {
    /// Parse an IDL, refusing any version other than [`IDL_VERSION`].
    ///
    /// # Errors
    /// If the JSON is malformed or the IDL is of another version.
    pub fn from_json(json: &str) -> Result<Self, String> {
        let value: serde_json::Value =
            serde_json::from_str(json).map_err(|e| format!("invalid IDL JSON: {e}"))?;
        let version = value.get("version").and_then(|v| v.as_str()).unwrap_or("");
        if version != IDL_VERSION {
            return Err(format!(
                "IDL version '{version}' is not supported: this spel reads version \
                 {IDL_VERSION} (actor programs). Regenerate the IDL with \
                 `spel generate-idl <core-crate> --message <Enum>`."
            ));
        }
        serde_json::from_value(value).map_err(|e| format!("invalid IDL: {e}"))
    }

    /// Serialize the IDL to pretty-printed JSON.
    ///
    /// # Errors
    /// Never in practice: every IDL type serializes.
    pub fn to_json_pretty(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// The message named `name`, by variant name or its kebab/snake form (`InitializeHolding`,
    /// `initialize-holding`, `initialize_holding`).
    pub fn message(&self, name: &str) -> Option<&IdlMessage> {
        self.messages.iter().find(|m| {
            let kebab = to_kebab(&m.name);
            m.name == name || kebab == name || kebab.replace('-', "_") == name
        })
    }

    /// The definition of the struct or enum named `name`.
    pub fn type_def(&self, name: &str) -> Option<&IdlTypeDef> {
        self.types.iter().find(|t| t.name == name)
    }
}

/// `InitializeHolding` → `initialize-holding`.
pub fn to_kebab(pascal: &str) -> String {
    let mut out = String::with_capacity(pascal.len() + 4);
    for (i, c) in pascal.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
        } else if c == '_' {
            out.push('-');
        } else {
            out.push(c);
        }
    }
    out
}

/// `sha256("<namespace>::<name>")[..8]`, the LEZ event selector convention, as hex.
pub fn event_selector(namespace: &str, name: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(format!("{namespace}::{name}").as_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kebab_splits_on_capitals() {
        assert_eq!(to_kebab("InitializeHolding"), "initialize-holding");
        assert_eq!(to_kebab("ReadState"), "read-state");
        assert_eq!(to_kebab("Burn"), "burn");
    }

    #[test]
    fn version_one_is_refused() {
        let err = SpelIdl::from_json(r#"{"version":"0.1.0","name":"t","instructions":[]}"#)
            .unwrap_err();
        assert!(err.contains("version '0.1.0' is not supported"), "{err}");
    }

    #[test]
    fn version_two_round_trips() {
        let idl = SpelIdl {
            version: IDL_VERSION.into(),
            name: "t".into(),
            messages: vec![IdlMessage {
                name: "InitializeHolding".into(),
                tag: 0,
                docs: None,
                fields: vec![IdlField {
                    name: "definition".into(),
                    type_: IdlType::Primitive("account_id".into()),
                    docs: None,
                }],
            }],
            state: None,
            events: vec![],
            types: vec![],
        };
        let parsed = SpelIdl::from_json(&idl.to_json_pretty().unwrap()).unwrap();
        assert_eq!(parsed, idl);
        assert_eq!(parsed.message("initialize-holding").map(|m| m.tag), Some(0));
        assert_eq!(parsed.message("initialize_holding").map(|m| m.tag), Some(0));
    }

    #[test]
    fn selector_depends_on_namespace_and_name() {
        let selector = event_selector("token", "Transferred");
        assert_eq!(selector.len(), 16);
        assert_ne!(selector, event_selector("nft", "Transferred"));
        assert_ne!(selector, event_selector("token", "Burned"));
    }
}
