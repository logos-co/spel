//! Shared account type scanning logic for IDL generation.
//!
//! This module provides functions to scan Rust source items for `#[account_type]`-annotated
//! types and collect helper types referenced by them. Both the CLI path (`spel generate-idl`)
//! in `spel-framework-core` and the proc-macro path (`lez_program`, `generate_idl!`) use this
//! logic to ensure consistent IDL output.

use std::collections::HashSet;

use syn::{Attribute, Item, ItemEnum, ItemStruct, Type};

use crate::idl::{IdlAccountType, IdlEnumVariant, IdlField, IdlType, IdlTypeDef};

// ─── Account type scanning ────────────────────────────────────────────────

/// Check if an item has the `#[account_type]` attribute.
///
/// Matches both the bare form `#[account_type]` and the fully-qualified
/// form `#[spel_framework_macros::account_type]` (idiomatic when importing
/// via a path rather than a `use` declaration).
pub fn has_account_type_attr(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| {
        let path = a.path();
        path.is_ident("account_type")
            || path
                .segments
                .last()
                .is_some_and(|s| s.ident == "account_type")
    })
}

/// Convert a Rust `syn::Type` to an IDL type representation.
pub(crate) fn syn_type_to_idl_type(ty: &Type) -> IdlType {
    match ty {
        Type::Path(type_path) => {
            let segment = match type_path.path.segments.last() {
                Some(s) => s,
                None => return IdlType::Primitive("unknown".to_string()),
            };
            let ident = segment.ident.to_string();
            match ident.as_str() {
                "u8" | "u16" | "u32" | "u64" | "u128" | "i8" | "i16" | "i32" | "i64" | "i128"
                | "bool" | "String" => IdlType::Primitive(ident.to_lowercase()),
                "Vec" => {
                    if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                        if let Some(syn::GenericArgument::Type(inner)) = args.args.first() {
                            return IdlType::Vec {
                                vec: Box::new(syn_type_to_idl_type(inner)),
                            };
                        }
                    }
                    IdlType::Primitive("vec<unknown>".to_string())
                },
                "Option" => {
                    if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                        if let Some(syn::GenericArgument::Type(inner)) = args.args.first() {
                            return IdlType::Option {
                                option: Box::new(syn_type_to_idl_type(inner)),
                            };
                        }
                    }
                    IdlType::Primitive("option<unknown>".to_string())
                },
                "ProgramId" => IdlType::Primitive("program_id".to_string()),
                "AccountId" => IdlType::Primitive("account_id".to_string()),
                other => IdlType::Defined {
                    defined: other.to_string(),
                },
            }
        },
        Type::Array(arr) => {
            let elem = syn_type_to_idl_type(&arr.elem);
            if let syn::Expr::Lit(lit) = &arr.len {
                if let syn::Lit::Int(n) = &lit.lit {
                    if let Ok(size) = n.base10_parse::<usize>() {
                        return IdlType::Array {
                            array: (Box::new(elem), size),
                        };
                    }
                }
            }
            IdlType::Array {
                array: (Box::new(elem), 0),
            }
        },
        _ => IdlType::Primitive("unknown".to_string()),
    }
}

/// Parse a named struct annotated with `#[account_type]` into an [`IdlAccountType`].
/// Returns `None` for tuple / unit structs (no named fields to describe).
pub fn parse_struct_account_type(item: &ItemStruct) -> Option<IdlAccountType> {
    let fields = if let syn::Fields::Named(named) = &item.fields {
        named
            .named
            .iter()
            .filter_map(|f| {
                f.ident.as_ref().map(|ident| IdlField {
                    name: ident.to_string(),
                    type_: syn_type_to_idl_type(&f.ty),
                })
            })
            .collect()
    } else {
        return None;
    };
    Some(IdlAccountType {
        name: item.ident.to_string(),
        type_: IdlTypeDef {
            name: String::new(),
            kind: "struct".to_string(),
            fields,
            variants: vec![],
        },
    })
}

/// Parse an enum annotated with `#[account_type]` into an [`IdlAccountType`].
/// Only named-field variants are supported; tuple variants are emitted with no fields.
pub fn parse_enum_account_type(item: &ItemEnum) -> IdlAccountType {
    let variants = item
        .variants
        .iter()
        .map(|v| {
            let fields = if let syn::Fields::Named(named) = &v.fields {
                named
                    .named
                    .iter()
                    .filter_map(|f| {
                        f.ident.as_ref().map(|ident| IdlField {
                            name: ident.to_string(),
                            type_: syn_type_to_idl_type(&f.ty),
                        })
                    })
                    .collect()
            } else {
                vec![]
            };
            IdlEnumVariant {
                name: v.ident.to_string(),
                fields,
            }
        })
        .collect();
    IdlAccountType {
        name: item.ident.to_string(),
        type_: IdlTypeDef {
            name: String::new(),
            kind: "enum".to_string(),
            fields: vec![],
            variants,
        },
    }
}

/// Collect all `Defined { name }` type references that appear anywhere within a
/// type definition (fields of structs, fields of enum variants).
fn collect_defined_refs(type_def: &IdlTypeDef) -> Vec<String> {
    let mut refs = Vec::new();
    for field in &type_def.fields {
        collect_defined_refs_from_type(&field.type_, &mut refs);
    }
    for variant in &type_def.variants {
        for field in &variant.fields {
            collect_defined_refs_from_type(&field.type_, &mut refs);
        }
    }
    refs
}

fn collect_defined_refs_from_type(ty: &IdlType, out: &mut Vec<String>) {
    match ty {
        IdlType::Defined { defined } => out.push(defined.clone()),
        IdlType::Vec { vec } => collect_defined_refs_from_type(vec, out),
        IdlType::Option { option } => collect_defined_refs_from_type(option, out),
        IdlType::Array { array: (inner, _) } => collect_defined_refs_from_type(inner, out),
        IdlType::Primitive(_) => {},
    }
}

/// Look up a type by name in the top-level items of a file and parse it.
/// Returns `None` if not found or the item cannot be represented (e.g. tuple struct).
fn find_and_parse_type(items: &[Item], name: &str) -> Option<IdlTypeDef> {
    for item in items {
        match item {
            Item::Struct(s) if s.ident == name => {
                return parse_struct_account_type(s).map(|at| IdlTypeDef {
                    name: name.to_string(),
                    ..at.type_
                });
            },
            Item::Enum(e) if e.ident == name => {
                let mut def = parse_enum_account_type(e).type_;
                def.name = name.to_string();
                return Some(def);
            },
            _ => {},
        }
    }
    None
}

/// Scan `items` for `#[account_type]`-annotated types and return:
/// - `accounts`: directly annotated types (primary account data layouts)
/// - `types`: helper types referenced by account types but not themselves annotated
///
/// Helper types are resolved transitively: if `Vault` references `VaultStatus`
/// and `VaultStatus` references `StatusFlags`, all three end up in the IDL.
pub fn collect_account_types(items: &[Item]) -> (Vec<IdlAccountType>, Vec<IdlTypeDef>) {
    // Pass 1: collect directly annotated types.
    let mut accounts: Vec<IdlAccountType> = Vec::new();
    let mut annotated_names: HashSet<String> = HashSet::new();

    for item in items {
        match item {
            Item::Struct(s) if has_account_type_attr(&s.attrs) => {
                if let Some(at) = parse_struct_account_type(s) {
                    annotated_names.insert(at.name.clone());
                    accounts.push(at);
                }
            },
            Item::Enum(e) if has_account_type_attr(&e.attrs) => {
                let at = parse_enum_account_type(e);
                annotated_names.insert(at.name.clone());
                accounts.push(at);
            },
            _ => {},
        }
    }

    // Pass 2: BFS over Defined-type references to collect helper types.
    let mut helper_types: Vec<IdlTypeDef> = Vec::new();
    let mut visited: HashSet<String> = annotated_names.clone();

    let mut queue: Vec<String> = accounts
        .iter()
        .flat_map(|a| collect_defined_refs(&a.type_))
        .filter(|n| !visited.contains(n))
        .collect::<HashSet<_>>() // deduplicate the initial queue
        .into_iter()
        .collect();

    while !queue.is_empty() {
        let batch: Vec<String> = std::mem::take(&mut queue);
        for name in batch {
            if visited.contains(&name) {
                continue;
            }
            visited.insert(name.clone());
            if let Some(def) = find_and_parse_type(items, &name) {
                // Enqueue any new references from this helper type.
                for ref_name in collect_defined_refs(&def) {
                    if !visited.contains(&ref_name) {
                        queue.push(ref_name);
                    }
                }
                helper_types.push(def);
            }
            // If the type isn't found in the file (e.g. it's from an external crate),
            // leave it as an unresolved Defined reference in the IDL. The decoder will
            // report a clear error if it encounters that reference at runtime.
        }
    }

    (accounts, helper_types)
}
