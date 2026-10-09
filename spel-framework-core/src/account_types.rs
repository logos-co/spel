//! Shared account type scanning logic for IDL generation.
//!
//! This module provides functions to scan Rust source items for `#[account_type]`-annotated
//! types and collect helper types referenced by them. Both the CLI path (`spel generate-idl`)
//! in `spel-framework-core` and the proc-macro path (`lez_program`, `generate_idl!`) use this
//! logic to ensure consistent IDL output.

use std::collections::{HashMap, HashSet};

use syn::{Attribute, Item, ItemEnum, ItemStruct, Type};

use crate::{
    idl::{IdlAccountType, IdlEnumVariant, IdlField, IdlType, IdlTypeDef},
    idl_gen::{is_account_type, is_context_type, is_vec_account_type},
};

// ─── Account type scanning ────────────────────────────────────────────────

/// Where a referenced type's definition is found when the scanned items
/// do not declare it.
///
/// Annotations are discovered only in connected sources, so a type
/// reached by reference may live in a crate that was never scanned.
/// `AdminConfig.slot: AuthoritySlot` is the standing example: the field
/// names a type from the extension's own dependency. The walk asks for
/// it by name, and only then does anything go looking.
pub trait TypeDefSource {
    /// The definition of `name`, or `None` when nothing declares it.
    fn find(&mut self, name: &str) -> Option<IdlTypeDef>;
}

/// Resolves nothing: every reference is every reference is answered from
/// the scanned items alone.
pub struct NoTypeDefs;

impl TypeDefSource for NoTypeDefs {
    fn find(&mut self, _name: &str) -> Option<IdlTypeDef> {
        None
    }
}

/// Scan `items` for `#[account_type]`-annotated types and return:
/// - `accounts`: directly annotated types (primary account data layouts)
/// - `types`: helper types referenced by account types but not themselves annotated
///
/// Helper types are resolved transitively: if `Vault` references `VaultStatus`
/// and `VaultStatus` references `StatusFlags`, all three end up in the IDL.
pub fn collect_account_types(items: &[Item]) -> (Vec<IdlAccountType>, Vec<IdlTypeDef>) {
    collect_account_types_from(items, &mut NoTypeDefs)
}

/// [`collect_account_types`] with somewhere to look for definitions the
/// scanned items do not contain.
pub fn collect_account_types_from(
    items: &[Item],
    defs: &mut impl TypeDefSource,
) -> (Vec<IdlAccountType>, Vec<IdlTypeDef>) {
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

    // Seed the queue with references from annotated types, deduplicated while
    // preserving discovery order. A `HashSet` round-trip here would make the
    // discovery order — and therefore the emitted IDL — non-deterministic
    // across processes (hash seeds are randomized per run).
    let mut queued: HashSet<String> = HashSet::new();
    let mut queue: Vec<String> = Vec::new();
    for account in &accounts {
        for name in collect_defined_refs(&account.type_) {
            if !visited.contains(&name) && queued.insert(name.clone()) {
                queue.push(name);
            }
        }
    }

    // Pass 1.5: seed the queue from #[instruction] fn argument types, so
    // defined types used only as instruction args (e.g. an enum argument)
    // also end up in the IDL's `types` section.
    for item in items {
        let Item::Fn(f) = item else { continue };
        if !f.attrs.iter().any(|a| a.path().is_ident("instruction")) {
            continue;
        }
        for input in &f.sig.inputs {
            let syn::FnArg::Typed(pt) = input else {
                continue;
            };
            if is_account_shaped(&pt.ty) {
                continue;
            }
            let idl_ty = syn_type_to_idl_type(&pt.ty);
            let mut refs = Vec::new();
            collect_defined_refs_from_type(&idl_ty, &mut refs);
            for name in refs {
                if !visited.contains(&name) {
                    queue.push(name);
                }
            }
        }
    }

    let index = index_type_items(items);
    while !queue.is_empty() {
        let batch: Vec<String> = std::mem::take(&mut queue);
        for name in batch {
            if visited.contains(&name) {
                continue;
            }
            visited.insert(name.clone());
            if let Some(def) = find_and_parse_type(&index, &name, defs) {
                // Enqueue any new references from this helper type.
                for ref_name in collect_defined_refs(&def) {
                    if !visited.contains(&ref_name) {
                        queue.push(ref_name);
                    }
                }
                helper_types.push(def);
            }
            // Nothing declares it anywhere the program can see, so it stays an
            // unresolved Defined reference in the IDL. The decoder reports a
            // clear error if it meets that reference at runtime.
        }
    }

    // Emit helper types in a canonical (name-sorted) order so the generated IDL
    // is byte-stable across processes and independent of source declaration
    // order. Directly annotated `accounts` already follow source item order.
    helper_types.sort_by(|a, b| a.name.cmp(&b.name));

    (accounts, helper_types)
}

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

/// Last path segment of a type, e.g. `nssa_core::account::AccountWithMetadata`
/// → `AccountWithMetadata`. `None` for non-path types (references, tuples).
fn last_ident(ty: &Type) -> Option<String> {
    match ty {
        Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
        _ => None,
    }
}

/// True for instruction params that are accounts rather than data args:
/// `AccountWithMetadata`, `Vec<AccountWithMetadata>`, and `ProgramContext`.
fn is_account_shaped(ty: &Type) -> bool {
    is_account_type(ty) || is_vec_account_type(ty) || is_context_type(ty)
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

/// The nameable type items, by name, first declaration winning.
///
/// `items` merges the consumer's file with every crate in its
/// dependency graph, so one name can appear more than once. Building
/// from the back leaves the earliest declaration in the map.
fn index_type_items(items: &[Item]) -> HashMap<String, &Item> {
    items
        .iter()
        .rev()
        .filter_map(|item| match item {
            Item::Struct(s) => Some((s.ident.to_string(), item)),
            Item::Enum(e) => Some((e.ident.to_string(), item)),
            Item::Type(t) => Some((t.ident.to_string(), item)),
            _ => None,
        })
        .collect()
}

/// Look up a type by name in an [`index_type_items`] index and parse it.
/// Returns `None` if not found or the item cannot be represented (e.g. tuple struct).
fn find_and_parse_type(
    index: &HashMap<String, &Item>,
    name: &str,
    defs: &mut impl TypeDefSource,
) -> Option<IdlTypeDef> {
    let Some(item) = index.get(name) else {
        // Not declared in the items in hand; ask the source. This is
        // also the alias-target path: an alias in a connected crate may
        // point at a type an unowned crate declares.
        return defs.find(name);
    };
    match item {
        Item::Struct(s) => parse_struct_account_type(s).map(|at| IdlTypeDef {
            name: name.to_string(),
            ..at.type_
        }),
        Item::Enum(e) => {
            let mut def = parse_enum_account_type(e).type_;
            def.name = name.to_string();
            Some(def)
        },
        // Type alias: resolve the target and emit its def under the
        // alias name, so instruction args referencing the alias find
        // a matching entry in the IDL's `types` array.
        Item::Type(t) => {
            let mut def = find_and_parse_type(index, &last_ident(&t.ty)?, defs)?;
            def.name = name.to_string();
            Some(def)
        },
        _ => None,
    }
}

/// The definition of `name` among `items`, aliases followed within the
/// same items. For sources that parse one file at a time.
pub(crate) fn type_def_from_items(items: &[Item], name: &str) -> Option<IdlTypeDef> {
    find_and_parse_type(&index_type_items(items), name, &mut NoTypeDefs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(src: &str) -> Vec<Item> {
        syn::parse_file(src).expect("failed to parse source").items
    }

    /// Answers exactly one name, counting the asks.
    struct OneDef {
        name: &'static str,
        asked: Vec<String>,
    }

    impl TypeDefSource for OneDef {
        fn find(&mut self, name: &str) -> Option<IdlTypeDef> {
            self.asked.push(name.to_string());
            (name == self.name).then(|| IdlTypeDef {
                name: name.to_string(),
                kind: "struct".to_string(),
                fields: vec![IdlField {
                    name: "holder".to_string(),
                    type_: IdlType::Primitive("account_id".to_string()),
                }],
                variants: vec![],
            })
        }
    }

    // A referenced type the items do not declare is asked of the source;
    // the def it returns joins `types` like any locally resolved helper.
    #[test]
    fn missing_reference_is_answered_by_the_source() {
        let src = r#"
            #[account_type]
            pub struct AdminConfig { pub slot: AuthoritySlot }
        "#;
        let mut defs = OneDef {
            name: "AuthoritySlot",
            asked: vec![],
        };
        let (_, helpers) = collect_account_types_from(&items(src), &mut defs);
        assert_eq!(defs.asked, vec!["AuthoritySlot"]);
        assert!(helpers.iter().any(|t| t.name == "AuthoritySlot"));
    }

    // The alias-target path: the alias is in hand, its target is not.
    // The def comes back under the alias name, which is what keeps
    // `AdminCandidate` in the IDL when its target lives in an unowned
    // crate.
    #[test]
    fn alias_target_is_resolved_through_the_source() {
        let src = r#"
            #[account_type]
            pub struct Wrapper { pub c: AdminCandidate }
            pub type AdminCandidate = AuthorityCandidate;
        "#;
        let mut defs = OneDef {
            name: "AuthorityCandidate",
            asked: vec![],
        };
        let (_, helpers) = collect_account_types_from(&items(src), &mut defs);
        assert!(
            helpers.iter().any(|t| t.name == "AdminCandidate"),
            "the target's def must come back under the alias name: {helpers:?}"
        );
    }

    #[test]
    fn first_declaration_of_a_repeated_name_wins() {
        // The scanned items merge the consumer's file with every crate in
        // its dependency graph, so one name can appear more than once. It
        // resolves to the first declaration in item order, not to
        // whichever one the lookup index happened to keep.
        let src = r#"
            pub struct Helper { pub first: u8 }
            pub struct Helper { pub second: u8 }

            #[account_type]
            pub struct VaultState {
                pub helper: Helper,
            }
        "#;
        let (_, helpers) = collect_account_types(&items(src));
        let helper = helpers
            .iter()
            .find(|t| t.name == "Helper")
            .expect("the referenced helper resolves");
        let fields: Vec<&str> = helper.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(fields, vec!["first"]);
    }

    #[test]
    fn helper_types_returned_in_sorted_order() {
        // Helper types are discovered via a BFS whose order must not depend on
        // `HashSet` iteration (randomized per process). They are returned in a
        // canonical name-sorted order so the generated IDL is byte-stable across
        // processes. The account references helpers in non-alphabetical order.
        let src = r#"
            pub struct Zeta { pub x: u8 }
            pub struct Alpha { pub x: u8 }
            pub struct Mu { pub x: u8 }

            #[account_type]
            pub struct VaultState {
                pub zeta: Zeta,
                pub alpha: Alpha,
                pub mu: Mu,
            }
        "#;
        let (accounts, helpers) = collect_account_types(&items(src));
        assert_eq!(accounts.len(), 1);
        let names: Vec<&str> = helpers.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["Alpha", "Mu", "Zeta"]);
    }

    #[test]
    fn instruction_arg_defined_types_are_collected() {
        let file: syn::File = syn::parse_quote! {
            pub enum MyChoice {
                A,
                B { x: u64 },
            }

            #[instruction]
            pub fn do_it(caller: AccountWithMetadata, choice: MyChoice) -> SpelResult {
                todo!()
            }
        };
        let (accounts, types) = collect_account_types(&file.items);
        assert!(accounts.is_empty());
        assert_eq!(types.len(), 1);
        assert_eq!(types[0].name, "MyChoice");
        assert_eq!(types[0].kind, "enum");
    }

    #[test]
    fn alias_to_enum_resolves_under_alias_name() {
        let src = r#"
            pub enum Real { A, B { x: u64 } }
            pub type Alias = Real;

            #[instruction]
            pub fn do_it(choice: Alias) -> SpelResult { todo!() }
        "#;
        let items = syn::parse_file(src).unwrap().items;
        let (_, types) = collect_account_types(&items);
        assert_eq!(types.len(), 1);
        assert_eq!(types[0].name, "Alias");
    }
}
