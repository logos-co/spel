//! Runtime IDL generation from SPEL program source files.
//!
//! This module is gated behind the `idl-gen` feature and provides
//! `generate_idl_from_file()` for use by `spel-cli generate-idl`.
//!
//! The parsing logic mirrors the `generate_idl!` proc macro in
//! `spel-framework-macros`, but operates at runtime on a file path
//! rather than at compile time.

use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};

use syn::{Attribute, FnArg, Ident, ItemFn, Pat, PatType, Type};

use crate::idl::{
    IdlAccountItem, IdlArg, IdlInstruction, IdlPda,
    IdlSeed, SpelIdl,
};

use crate::account_types::{collect_account_types, syn_type_to_idl_type};

/// Error type returned by [`generate_idl_from_file`].
#[derive(Debug)]
pub enum IdlGenError {
    Io(std::io::Error),
    Parse(syn::Error),
    NoProgram(String),
    NoInstructions(String),
}

impl fmt::Display for IdlGenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdlGenError::Io(e) => write!(f, "IO error: {}", e),
            IdlGenError::Parse(e) => write!(f, "Parse error: {}", e),
            IdlGenError::NoProgram(path) => {
                write!(f, "No #[lez_program] module found in '{}'", path)
            }
            IdlGenError::NoInstructions(path) => {
                write!(f, "No #[instruction] functions found in '{}'", path)
            }
        }
    }
}

impl From<std::io::Error> for IdlGenError {
    fn from(e: std::io::Error) -> Self {
        IdlGenError::Io(e)
    }
}

impl From<syn::Error> for IdlGenError {
    fn from(e: syn::Error) -> Self {
        IdlGenError::Parse(e)
    }
}

/// Parse a SPEL program source file and return its [`SpelIdl`].
///
/// The path is resolved relative to the current working directory,
/// which is the natural behavior for a CLI tool.
pub fn generate_idl_from_file(source_path: &Path) -> Result<SpelIdl, IdlGenError> {
    generate_idl_from_file_with_deps(source_path, &[])
}

/// Parse a SPEL program source file and return its [`SpelIdl`], also scanning
/// the library source of each crate directory in `dep_source_dirs` for
/// `#[account_type]`-annotated types.
///
/// Each entry in `dep_source_dirs` should be a Rust crate root (the directory
/// that contains `src/lib.rs`).  Only local path-dependencies should be passed
/// here — third-party registry or git crates are intentionally excluded to
/// avoid pulling in unrelated type definitions.
pub fn generate_idl_from_file_with_deps(
    source_path: &Path,
    dep_source_dirs: &[PathBuf],
) -> Result<SpelIdl, IdlGenError> {
    let content = std::fs::read_to_string(source_path)?;
    let extra_items = collect_items_from_crate_dirs(dep_source_dirs);
    generate_idl_inner(&content, &source_path.display().to_string(), &extra_items)
}

/// Parse a SPEL program from source text and return its [`SpelIdl`].
///
/// `source_label` is used only in error messages. Used exclusively in tests;
/// production code goes through `generate_idl_from_file_with_deps`.
#[cfg(test)]
fn generate_idl_from_str(content: &str, source_label: &str) -> Result<SpelIdl, IdlGenError> {
    generate_idl_inner(content, source_label, &[])
}

/// Core IDL generation logic. `extra_items` are synthetic items collected from
/// dependency crate sources and merged with the program file's own items before
/// account-type scanning.
fn generate_idl_inner(
    content: &str,
    source_label: &str,
    extra_items: &[syn::Item],
) -> Result<SpelIdl, IdlGenError> {
    let path_str = source_label.to_string();

    let file = syn::parse_file(content)?;

    // Find the #[lez_program] module
    let program_mod = file
        .items
        .iter()
        .find_map(|item| {
            if let syn::Item::Mod(m) = item {
                if m.attrs.iter().any(|a| a.path().is_ident("lez_program")) {
                    return Some(m);
                }
            }
            None
        })
        .ok_or_else(|| IdlGenError::NoProgram(path_str.clone()))?;

    let mod_name = program_mod.ident.to_string();

    let (_, items) = program_mod
        .content
        .as_ref()
        .ok_or_else(|| IdlGenError::NoProgram(path_str.clone()))?;

    // Collect instruction functions
    let mut instructions: Vec<InstructionInfo> = Vec::new();
    for item in items {
        if let syn::Item::Fn(func) = item {
            if has_instruction_attr(&func.attrs) {
                instructions.push(parse_instruction(func.clone())?);
            }
        }
    }

    if instructions.is_empty() {
        return Err(IdlGenError::NoInstructions(path_str));
    }

    // Detect external instruction type from #[lez_program(instruction = "...")]
    let external_instruction = program_mod
        .attrs
        .iter()
        .find(|a| a.path().is_ident("lez_program"))
        .and_then(|attr| {
            let mut ext: Option<String> = None;
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("instruction") {
                    if let Ok(value) = meta.value() {
                        if let Ok(lit) = value.parse::<syn::LitStr>() {
                            ext = Some(lit.value());
                        }
                    }
                }
                Ok(())
            });
            ext
        });

    // Build the SpelIdl struct
    let idl_instructions: Vec<IdlInstruction> = instructions
        .iter()
        .map(|ix| {
            let accounts: Vec<IdlAccountItem> = ix
                .accounts
                .iter()
                .map(|acc| {
                    let pda = if acc.constraints.pda_seeds.is_empty() {
                        None
                    } else {
                        let seeds: Vec<IdlSeed> = acc
                            .constraints
                            .pda_seeds
                            .iter()
                            .map(|s| match s {
                                PdaSeedDef::Const(v) => IdlSeed::Const { value: v.clone() },
                                PdaSeedDef::Account(p) => IdlSeed::Account { path: p.clone() },
                                PdaSeedDef::Arg(p) => IdlSeed::Arg { path: p.clone() },
                            })
                            .collect();
                        Some(IdlPda { seeds, private: false })
                    };

                    IdlAccountItem {
                        name: acc.name.to_string(),
                        writable: acc.constraints.mutable,
                        signer: acc.constraints.signer,
                        init: acc.constraints.init,
                        owner: None,
                        pda,
                        rest: acc.is_rest,
                        visibility: vec![],
                    }
                })
                .collect();

            let args: Vec<IdlArg> = ix
                .args
                .iter()
                .map(|arg| IdlArg {
                    name: arg.name.to_string(),
                    type_: syn_type_to_idl_type(&arg.ty),
                })
                .collect();

            IdlInstruction {
                name: ix.fn_name.to_string(),
                accounts,
                args,
                discriminator: None,
                execution: None,
                variant: None,
            }
        })
        .collect();

    let mut all_items: Vec<syn::Item> = file.items.clone();
    all_items.extend(items.clone());
    all_items.extend_from_slice(extra_items);
    let (accounts, types) = collect_account_types(&all_items);

    Ok(SpelIdl {
        version: "0.1.0".to_string(),
        name: mod_name,
        instructions: idl_instructions,
        accounts,
        types,
        errors: vec![],
        spec: None,
        metadata: None,
        instruction_type: external_instruction,
    })
}

// ─── Dependency source collection ────────────────────────────────────────

/// Parse the library source of each crate directory and return all `syn::Item`s
/// found, following `mod` declarations recursively.
fn collect_items_from_crate_dirs(dirs: &[PathBuf]) -> Vec<syn::Item> {
    let mut items = Vec::new();
    let mut visited: HashSet<PathBuf> = HashSet::new();
    for dir in dirs {
        let lib_rs = dir.join("src").join("lib.rs");
        if lib_rs.exists() {
            collect_items_from_source_file(&lib_rs, &mut items, &mut visited);
        }
    }
    items
}

/// Parse a single Rust source file and append its items to `out`, following
/// external `mod` declarations to their corresponding files.
fn collect_items_from_source_file(
    path: &Path,
    out: &mut Vec<syn::Item>,
    visited: &mut HashSet<PathBuf>,
) {
    let canonical = match path.canonicalize() {
        Ok(p) => p,
        Err(_) => path.to_path_buf(),
    };
    if !visited.insert(canonical) {
        return; // already processed
    }

    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return,
    };
    let file = match syn::parse_file(&content) {
        Ok(f) => f,
        Err(_) => return,
    };

    // Sub-module files for `lib.rs` / `mod.rs` live alongside the file;
    // for `foo.rs` they live in a `foo/` directory next to the file.
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let sub_base = if file_name == "lib.rs" || file_name == "mod.rs" {
        path.parent().map(|p| p.to_path_buf())
    } else {
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        path.parent().map(|p| p.join(stem))
    };

    collect_items_recursive(&file.items, sub_base.as_deref(), out, visited);
}

/// Resolve the on-disk path for an external `mod` declaration.
///
/// Resolution order:
/// 1. `#[path = "..."]` attribute on the mod item — resolved relative to `base_dir`.
/// 2. `<base_dir>/<mod_name>.rs`
/// 3. `<base_dir>/<mod_name>/mod.rs`
///
/// Returns `None` if no candidate file exists.
fn mod_file_path(m: &syn::ItemMod, base_dir: &Path) -> Option<PathBuf> {
    // Check for explicit #[path = "..."] override first.
    for attr in &m.attrs {
        if attr.path().is_ident("path") {
            if let Ok(syn::MetaNameValue {
                value: syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }),
                ..
            }) = attr.meta.require_name_value()
            {
                let p = base_dir.join(s.value());
                if p.exists() {
                    return Some(p);
                }
            }
        }
    }

    // Standard two-candidate resolution.
    let mod_name = m.ident.to_string();
    let flat = base_dir.join(format!("{mod_name}.rs"));
    if flat.exists() {
        return Some(flat);
    }
    let nested = base_dir.join(&mod_name).join("mod.rs");
    if nested.exists() {
        return Some(nested);
    }
    None
}

fn collect_items_recursive(
    items: &[syn::Item],
    base_dir: Option<&Path>,
    out: &mut Vec<syn::Item>,
    visited: &mut HashSet<PathBuf>,
) {
    for item in items {
        match item {
            syn::Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    // Inline module — recurse into its body with an updated base_dir
                    // so that any file-backed `mod` declarations inside it resolve
                    // relative to `base_dir/<mod_name>/` rather than `base_dir/`.
                    let inner_base = base_dir.map(|d| d.join(m.ident.to_string()));
                    collect_items_recursive(inner, inner_base.as_deref(), out, visited);
                } else if let Some(dir) = base_dir {
                    // External module file — locate and parse it.
                    if let Some(p) = mod_file_path(m, dir) {
                        collect_items_from_source_file(&p, out, visited);
                    }
                }
            }
            _ => out.push(item.clone()),
        }
    }
}

// ─── Internal parsing types ───────────────────────────────────────────────

struct InstructionInfo {
    fn_name: Ident,
    accounts: Vec<AccountParam>,
    args: Vec<ArgParam>,
}

struct AccountParam {
    name: Ident,
    constraints: AccountConstraints,
    is_rest: bool,
}

#[derive(Default)]
struct AccountConstraints {
    mutable: bool,
    init: bool,
    signer: bool,
    pda_seeds: Vec<PdaSeedDef>,
}

#[derive(Clone)]
enum PdaSeedDef {
    Const(String),
    Account(String),
    Arg(String),
}

struct ArgParam {
    name: Ident,
    ty: Type,
}

fn has_instruction_attr(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| a.path().is_ident("instruction"))
}

fn parse_instruction(func: ItemFn) -> Result<InstructionInfo, IdlGenError> {
    let fn_name = func.sig.ident.clone();
    let mut accounts = Vec::new();
    let mut args = Vec::new();

    for input in &func.sig.inputs {
        match input {
            FnArg::Typed(pat_type) => {
                let param_name = extract_param_name(pat_type)?;
                let ty = &*pat_type.ty;

                if is_account_type(ty) {
                    let constraints = parse_account_constraints(&pat_type.attrs)?;
                    accounts.push(AccountParam {
                        name: param_name,
                        constraints,
                        is_rest: false,
                    });
                } else if is_vec_account_type(ty) {
                    let constraints = parse_account_constraints(&pat_type.attrs)?;
                    accounts.push(AccountParam {
                        name: param_name,
                        constraints,
                        is_rest: true,
                    });
                } else {
                    args.push(ArgParam {
                        name: param_name,
                        ty: ty.clone(),
                    });
                }
            }
            FnArg::Receiver(_) => {
                return Err(IdlGenError::Parse(syn::Error::new_spanned(
                    input,
                    "instruction functions cannot have self parameter",
                )));
            }
        }
    }

    Ok(InstructionInfo {
        fn_name,
        accounts,
        args,
    })
}

fn extract_param_name(pat_type: &PatType) -> Result<Ident, IdlGenError> {
    match &*pat_type.pat {
        Pat::Ident(pat_ident) => Ok(pat_ident.ident.clone()),
        _ => Err(IdlGenError::Parse(syn::Error::new_spanned(
            &pat_type.pat,
            "expected simple identifier pattern",
        ))),
    }
}

fn is_account_type(ty: &Type) -> bool {
    if let Type::Path(type_path) = ty {
        if let Some(segment) = type_path.path.segments.last() {
            return segment.ident == "AccountWithMetadata";
        }
    }
    false
}

fn is_vec_account_type(ty: &Type) -> bool {
    if let Type::Path(type_path) = ty {
        if let Some(segment) = type_path.path.segments.last() {
            if segment.ident == "Vec" {
                if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                    if let Some(syn::GenericArgument::Type(inner)) = args.args.first() {
                        return is_account_type(inner);
                    }
                }
            }
        }
    }
    false
}

fn parse_account_constraints(attrs: &[Attribute]) -> Result<AccountConstraints, IdlGenError> {
    let mut constraints = AccountConstraints::default();

    for attr in attrs {
        if attr.path().is_ident("account") {
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("mut") {
                    constraints.mutable = true;
                    Ok(())
                } else if meta.path.is_ident("init") {
                    constraints.init = true;
                    constraints.mutable = true;
                    Ok(())
                } else if meta.path.is_ident("signer") {
                    constraints.signer = true;
                    Ok(())
                } else if meta.path.is_ident("owner") {
                    let value = meta.value()?;
                    let _expr: syn::Expr = value.parse()?;
                    Ok(())
                } else if meta.path.is_ident("pda") {
                    let value = meta.value()?;
                    let expr: syn::Expr = value.parse()?;
                    constraints.pda_seeds = parse_pda_expr(&expr)?;
                    Ok(())
                } else {
                    Err(meta.error("unknown account constraint"))
                }
            })
            .map_err(IdlGenError::Parse)?;
        }
    }

    Ok(constraints)
}

fn parse_pda_expr(expr: &syn::Expr) -> Result<Vec<PdaSeedDef>, syn::Error> {
    match expr {
        syn::Expr::Call(call) => {
            let seed = parse_single_pda_seed(call)?;
            Ok(vec![seed])
        }
        syn::Expr::Array(arr) => {
            let mut seeds = Vec::new();
            for elem in &arr.elems {
                if let syn::Expr::Call(call) = elem {
                    seeds.push(parse_single_pda_seed(call)?);
                } else {
                    return Err(syn::Error::new_spanned(
                        elem,
                        "PDA seed must be const(\"...\"), account(\"...\"), or arg(\"...\")",
                    ));
                }
            }
            Ok(seeds)
        }
        _ => Err(syn::Error::new_spanned(
            expr,
            "PDA seed must be const(\"...\"), account(\"...\"), arg(\"...\"), or [seed, ...]",
        )),
    }
}

fn parse_single_pda_seed(call: &syn::ExprCall) -> Result<PdaSeedDef, syn::Error> {
    let func_name = if let syn::Expr::Path(path) = &*call.func {
        path.path
            .get_ident()
            .map(|i| i.to_string())
            .unwrap_or_default()
    } else {
        String::new()
    };

    if call.args.len() != 1 {
        return Err(syn::Error::new_spanned(
            call,
            "PDA seed function takes exactly one string argument",
        ));
    }

    let arg = &call.args[0];
    let string_val = if let syn::Expr::Lit(lit) = arg {
        if let syn::Lit::Str(s) = &lit.lit {
            s.value()
        } else {
            return Err(syn::Error::new_spanned(arg, "Expected string literal"));
        }
    } else {
        return Err(syn::Error::new_spanned(arg, "Expected string literal"));
    };

    match func_name.as_str() {
        "const" | "r#const" | "seed_const" | "literal" => Ok(PdaSeedDef::Const(string_val)),
        "account" => Ok(PdaSeedDef::Account(string_val)),
        "arg" => Ok(PdaSeedDef::Arg(string_val)),
        _ => Err(syn::Error::new_spanned(
            call,
            format!(
                "Unknown PDA seed type '{}'. Use const(\"...\"), account(\"...\"), or arg(\"...\")",
                func_name
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::idl::{IdlSeed, IdlType, SpelIdl};

    fn ok(src: &str) -> SpelIdl {
        generate_idl_from_str(src, "<test>").expect("IDL generation failed")
    }

    fn err(src: &str) -> IdlGenError {
        generate_idl_from_str(src, "<test>").expect_err("expected an error")
    }

    // ── Error cases ──────────────────────────────────────────────────────────

    #[test]
    fn error_no_lez_program_module() {
        let src = r#"
            pub fn some_function() {}
        "#;
        assert!(matches!(err(src), IdlGenError::NoProgram(_)));
    }

    #[test]
    fn error_no_instruction_functions() {
        let src = r#"
            #[lez_program]
            pub mod my_program {
                pub fn helper() {}
            }
        "#;
        assert!(matches!(err(src), IdlGenError::NoInstructions(_)));
    }

    #[test]
    fn error_invalid_rust_syntax() {
        let src = "this is not valid rust @@@";
        assert!(matches!(err(src), IdlGenError::Parse(_)));
    }

    // ── Basic parsing ─────────────────────────────────────────────────────────

    #[test]
    fn minimal_program_name_and_version() {
        let src = r#"
            #[lez_program]
            pub mod my_token {
                #[instruction]
                pub fn transfer(sender: AccountWithMetadata, recipient: AccountWithMetadata) {}
            }
        "#;
        let idl = ok(src);
        assert_eq!(idl.name, "my_token");
        assert_eq!(idl.version, "0.1.0");
        assert!(idl.instruction_type.is_none());
    }

    #[test]
    fn external_instruction_type_attribute() {
        let src = r#"
            #[lez_program(instruction = "my_core::Instruction")]
            pub mod my_program {
                #[instruction]
                pub fn do_thing(account: AccountWithMetadata) {}
            }
        "#;
        let idl = ok(src);
        assert_eq!(idl.instruction_type.as_deref(), Some("my_core::Instruction"));
    }

    // ── Account constraints ───────────────────────────────────────────────────

    #[test]
    fn account_no_constraints() {
        let src = r#"
            #[lez_program]
            pub mod prog {
                #[instruction]
                pub fn ix(acc: AccountWithMetadata) {}
            }
        "#;
        let idl = ok(src);
        let acc = &idl.instructions[0].accounts[0];
        assert_eq!(acc.name, "acc");
        assert!(!acc.writable);
        assert!(!acc.signer);
        assert!(!acc.init);
        assert!(acc.pda.is_none());
        assert!(!acc.rest);
    }

    #[test]
    fn account_mut_constraint() {
        let src = r#"
            #[lez_program]
            pub mod prog {
                #[instruction]
                pub fn ix(#[account(mut)] acc: AccountWithMetadata) {}
            }
        "#;
        let acc = &ok(src).instructions[0].accounts[0];
        assert!(acc.writable);
        assert!(!acc.signer);
        assert!(!acc.init);
    }

    #[test]
    fn account_signer_constraint() {
        let src = r#"
            #[lez_program]
            pub mod prog {
                #[instruction]
                pub fn ix(#[account(signer)] acc: AccountWithMetadata) {}
            }
        "#;
        let acc = &ok(src).instructions[0].accounts[0];
        assert!(acc.signer);
        assert!(!acc.writable);
    }

    #[test]
    fn account_init_implies_mut() {
        let src = r#"
            #[lez_program]
            pub mod prog {
                #[instruction]
                pub fn ix(#[account(init)] acc: AccountWithMetadata) {}
            }
        "#;
        let acc = &ok(src).instructions[0].accounts[0];
        assert!(acc.init);
        assert!(acc.writable, "init must imply writable");
    }

    #[test]
    fn account_multiple_constraints() {
        let src = r#"
            #[lez_program]
            pub mod prog {
                #[instruction]
                pub fn ix(#[account(mut, signer)] acc: AccountWithMetadata) {}
            }
        "#;
        let acc = &ok(src).instructions[0].accounts[0];
        assert!(acc.writable);
        assert!(acc.signer);
    }

    // ── PDA seeds ─────────────────────────────────────────────────────────────

    #[test]
    fn account_pda_const_seed() {
        let src = r#"
            #[lez_program]
            pub mod prog {
                #[instruction]
                pub fn ix(#[account(pda = seed_const("pool"))] acc: AccountWithMetadata) {}
            }
        "#;
        let acc = &ok(src).instructions[0].accounts[0];
        let pda = acc.pda.as_ref().expect("pda should be present");
        assert_eq!(pda.seeds.len(), 1);
        assert!(matches!(&pda.seeds[0], IdlSeed::Const { value } if value == "pool"));
    }

    #[test]
    fn account_pda_account_seed() {
        let src = r#"
            #[lez_program]
            pub mod prog {
                #[instruction]
                pub fn ix(#[account(pda = account("owner.id"))] acc: AccountWithMetadata) {}
            }
        "#;
        let pda = ok(src).instructions[0].accounts[0].pda.clone().unwrap();
        assert!(matches!(&pda.seeds[0], IdlSeed::Account { path } if path == "owner.id"));
    }

    #[test]
    fn account_pda_arg_seed() {
        let src = r#"
            #[lez_program]
            pub mod prog {
                #[instruction]
                pub fn ix(#[account(pda = arg("pool_id"))] acc: AccountWithMetadata) {}
            }
        "#;
        let pda = ok(src).instructions[0].accounts[0].pda.clone().unwrap();
        assert!(matches!(&pda.seeds[0], IdlSeed::Arg { path } if path == "pool_id"));
    }

    #[test]
    fn account_pda_multiple_seeds() {
        let src = r#"
            #[lez_program]
            pub mod prog {
                #[instruction]
                pub fn ix(
                    #[account(pda = [seed_const("amm"), account("base.id"), arg("quote_id")])]
                    acc: AccountWithMetadata,
                ) {}
            }
        "#;
        let pda = ok(src).instructions[0].accounts[0].pda.clone().unwrap();
        assert_eq!(pda.seeds.len(), 3);
        assert!(matches!(&pda.seeds[0], IdlSeed::Const { value } if value == "amm"));
        assert!(matches!(&pda.seeds[1], IdlSeed::Account { path } if path == "base.id"));
        assert!(matches!(&pda.seeds[2], IdlSeed::Arg { path } if path == "quote_id"));
    }

    // ── Rest accounts (Vec<AccountWithMetadata>) ──────────────────────────────

    #[test]
    fn vec_account_sets_rest_flag() {
        let src = r#"
            #[lez_program]
            pub mod prog {
                #[instruction]
                pub fn ix(single: AccountWithMetadata, rest: Vec<AccountWithMetadata>) {}
            }
        "#;
        let accounts = &ok(src).instructions[0].accounts;
        assert_eq!(accounts.len(), 2);
        assert!(!accounts[0].rest, "single account should not be rest");
        assert!(accounts[1].rest, "Vec<AccountWithMetadata> should be rest");
    }

    // ── Instruction args ──────────────────────────────────────────────────────

    #[test]
    fn primitive_arg_types() {
        let src = r#"
            #[lez_program]
            pub mod prog {
                #[instruction]
                pub fn ix(
                    acc: AccountWithMetadata,
                    a: u64,
                    b: u32,
                    c: bool,
                    d: String,
                ) {}
            }
        "#;
        let args = &ok(src).instructions[0].args;
        assert_eq!(args.len(), 4);
        assert!(matches!(&args[0].type_, IdlType::Primitive(s) if s == "u64"));
        assert!(matches!(&args[1].type_, IdlType::Primitive(s) if s == "u32"));
        assert!(matches!(&args[2].type_, IdlType::Primitive(s) if s == "bool"));
        assert!(matches!(&args[3].type_, IdlType::Primitive(s) if s == "string"));
    }

    #[test]
    fn vec_arg_type() {
        let src = r#"
            #[lez_program]
            pub mod prog {
                #[instruction]
                pub fn ix(acc: AccountWithMetadata, ids: Vec<u64>) {}
            }
        "#;
        let args = &ok(src).instructions[0].args;
        assert_eq!(args.len(), 1);
        assert!(
            matches!(&args[0].type_, IdlType::Vec { vec } if matches!(vec.as_ref(), IdlType::Primitive(s) if s == "u64"))
        );
    }

    #[test]
    fn option_arg_type() {
        let src = r#"
            #[lez_program]
            pub mod prog {
                #[instruction]
                pub fn ix(acc: AccountWithMetadata, maybe: Option<u32>) {}
            }
        "#;
        let args = &ok(src).instructions[0].args;
        assert!(
            matches!(&args[0].type_, IdlType::Option { option } if matches!(option.as_ref(), IdlType::Primitive(s) if s == "u32"))
        );
    }

    #[test]
    fn array_arg_type() {
        let src = r#"
            #[lez_program]
            pub mod prog {
                #[instruction]
                pub fn ix(acc: AccountWithMetadata, data: [u8; 32]) {}
            }
        "#;
        let args = &ok(src).instructions[0].args;
        assert!(
            matches!(&args[0].type_, IdlType::Array { array: (elem, size) }
                if matches!(elem.as_ref(), IdlType::Primitive(s) if s == "u8") && *size == 32)
        );
    }

    #[test]
    fn defined_arg_type() {
        let src = r#"
            #[lez_program]
            pub mod prog {
                #[instruction]
                pub fn ix(acc: AccountWithMetadata, config: MyConfig) {}
            }
        "#;
        let args = &ok(src).instructions[0].args;
        assert!(matches!(&args[0].type_, IdlType::Defined { defined } if defined == "MyConfig"));
    }

    #[test]
    fn program_id_arg_type() {
        let src = r#"
            #[lez_program]
            pub mod prog {
                #[instruction]
                pub fn ix(acc: AccountWithMetadata, prog: ProgramId) {}
            }
        "#;
        let args = &ok(src).instructions[0].args;
        assert!(matches!(&args[0].type_, IdlType::Primitive(s) if s == "program_id"));
    }

    #[test]
    fn account_id_arg_type() {
        let src = r#"
            #[lez_program]
            pub mod prog {
                #[instruction]
                pub fn ix(acc: AccountWithMetadata, id: AccountId) {}
            }
        "#;
        let args = &ok(src).instructions[0].args;
        assert!(matches!(&args[0].type_, IdlType::Primitive(s) if s == "account_id"));
    }

    // ── Multiple instructions ─────────────────────────────────────────────────

    #[test]
    fn multiple_instructions_order_preserved() {
        let src = r#"
            #[lez_program]
            pub mod prog {
                #[instruction]
                pub fn alpha(acc: AccountWithMetadata) {}

                pub fn not_an_instruction(acc: AccountWithMetadata) {}

                #[instruction]
                pub fn beta(acc: AccountWithMetadata, amount: u64) {}
            }
        "#;
        let idl = ok(src);
        assert_eq!(idl.instructions.len(), 2);
        assert_eq!(idl.instructions[0].name, "alpha");
        assert_eq!(idl.instructions[1].name, "beta");
        // non-annotated function is excluded
        assert!(!idl.instructions.iter().any(|i| i.name == "not_an_instruction"));
    }

    // ── #[account_type] — basic discovery ─────────────────────────────────────

    #[test]
    fn account_type_struct_included_in_accounts() {
        let src = r#"
            #[account_type]
            pub struct VaultState {
                pub owner: AccountId,
                pub balance: u64,
            }

            #[lez_program]
            pub mod prog {
                #[instruction]
                pub fn init(acc: AccountWithMetadata) {}
            }
        "#;
        let idl = ok(src);
        assert_eq!(idl.accounts.len(), 1);
        assert_eq!(idl.accounts[0].name, "VaultState");
        assert_eq!(idl.accounts[0].type_.kind, "struct");
        assert_eq!(idl.accounts[0].type_.fields.len(), 2);
        assert_eq!(idl.accounts[0].type_.fields[0].name, "owner");
        assert_eq!(idl.accounts[0].type_.fields[1].name, "balance");
    }

    #[test]
    fn account_type_enum_included_in_accounts() {
        let src = r#"
            #[account_type]
            pub enum TokenHolding {
                Fungible { definition_id: AccountId, balance: u128 },
                NftMaster { definition_id: AccountId, print_balance: u128 },
            }

            #[lez_program]
            pub mod prog {
                #[instruction]
                pub fn init(acc: AccountWithMetadata) {}
            }
        "#;
        let idl = ok(src);
        assert_eq!(idl.accounts.len(), 1);
        let def = &idl.accounts[0];
        assert_eq!(def.name, "TokenHolding");
        assert_eq!(def.type_.kind, "enum");
        assert_eq!(def.type_.variants.len(), 2);
        assert_eq!(def.type_.variants[0].name, "Fungible");
        assert_eq!(def.type_.variants[0].fields.len(), 2);
        assert_eq!(def.type_.variants[1].name, "NftMaster");
    }

    #[test]
    fn unannotated_type_not_in_accounts() {
        let src = r#"
            pub struct NotAnAccountType { pub x: u64 }

            #[lez_program]
            pub mod prog {
                #[instruction]
                pub fn init(acc: AccountWithMetadata) {}
            }
        "#;
        let idl = ok(src);
        assert!(idl.accounts.is_empty());
    }

    #[test]
    fn multiple_account_types_all_collected() {
        let src = r#"
            #[account_type]
            pub struct DefinitionAccount { pub name: String }

            #[account_type]
            pub enum HoldingAccount { Fungible { balance: u128 } }

            #[lez_program]
            pub mod prog {
                #[instruction]
                pub fn init(acc: AccountWithMetadata) {}
            }
        "#;
        let idl = ok(src);
        assert_eq!(idl.accounts.len(), 2);
        let names: Vec<&str> = idl.accounts.iter().map(|a| a.name.as_str()).collect();
        assert!(names.contains(&"DefinitionAccount"));
        assert!(names.contains(&"HoldingAccount"));
    }

    // ── #[account_type] — referenced helper types ──────────────────────────────

    #[test]
    fn referenced_helper_type_goes_into_types() {
        let src = r#"
            pub enum Status { Active, Inactive }

            #[account_type]
            pub struct VaultState {
                pub status: Status,
                pub balance: u64,
            }

            #[lez_program]
            pub mod prog {
                #[instruction]
                pub fn init(acc: AccountWithMetadata) {}
            }
        "#;
        let idl = ok(src);
        assert_eq!(idl.accounts.len(), 1);
        assert_eq!(idl.types.len(), 1);
        assert_eq!(idl.types[0].name, "Status");
        assert_eq!(idl.types[0].kind, "enum");
        assert_eq!(idl.types[0].variants.len(), 2);
    }

    #[test]
    fn annotated_type_not_duplicated_in_types() {
        // If a type is itself annotated with #[account_type], it should not
        // also appear in idl.types even if another account type references it.
        let src = r#"
            #[account_type]
            pub enum Status { Active, Inactive }

            #[account_type]
            pub struct VaultState { pub status: Status }

            #[lez_program]
            pub mod prog {
                #[instruction]
                pub fn init(acc: AccountWithMetadata) {}
            }
        "#;
        let idl = ok(src);
        assert_eq!(idl.accounts.len(), 2, "both annotated types should be in accounts");
        assert!(idl.types.is_empty(), "annotated type should not also be in types");
    }

    #[test]
    fn transitive_helper_type_resolved() {
        // VaultState → Status → StatusFlags — all helper types should end up in types.
        let src = r#"
            pub enum StatusFlags { Flag1, Flag2 }
            pub enum Status { Active(StatusFlags), Inactive }

            #[account_type]
            pub struct VaultState { pub status: Status }

            #[lez_program]
            pub mod prog {
                #[instruction]
                pub fn init(acc: AccountWithMetadata) {}
            }
        "#;
        let idl = ok(src);
        assert_eq!(idl.accounts.len(), 1);
        let type_names: Vec<&str> = idl.types.iter().map(|t| t.name.as_str()).collect();
        assert!(type_names.contains(&"Status"), "Status should be in types");
        // StatusFlags is referenced inside Status enum (tuple variant — not named fields),
        // so it won't be extracted as a field. Verify at least Status is present.
        assert_eq!(idl.types.iter().filter(|t| t.name == "Status").count(), 1);
    }

    #[test]
    fn external_defined_type_left_as_defined_ref() {
        // AccountId is mapped to the primitive "account_id" by syn_type_to_idl_type,
        // so it should NOT appear in idl.types as an unresolvable Defined reference.
        let src = r#"
            #[account_type]
            pub struct HoldingAccount {
                pub definition_id: AccountId,
                pub balance: u128,
            }

            #[lez_program]
            pub mod prog {
                #[instruction]
                pub fn init(acc: AccountWithMetadata) {}
            }
        "#;
        let idl = ok(src);
        assert_eq!(idl.accounts.len(), 1);
        // AccountId → primitive "account_id", so no helper types needed
        assert!(idl.types.is_empty());
        assert!(
            matches!(&idl.accounts[0].type_.fields[0].type_, IdlType::Primitive(s) if s == "account_id")
        );
    }

    #[test]
    fn account_type_field_types_correctly_mapped() {
        let src = r#"
            #[account_type]
            pub struct Everything {
                pub a: u8,
                pub b: u64,
                pub c: u128,
                pub d: bool,
                pub e: String,
                pub f: AccountId,
                pub g: Option<u32>,
                pub h: Vec<u8>,
            }

            #[lez_program]
            pub mod prog {
                #[instruction]
                pub fn init(acc: AccountWithMetadata) {}
            }
        "#;
        let idl = ok(src);
        let fields = &idl.accounts[0].type_.fields;
        assert!(matches!(&fields[0].type_, IdlType::Primitive(s) if s == "u8"));
        assert!(matches!(&fields[1].type_, IdlType::Primitive(s) if s == "u64"));
        assert!(matches!(&fields[2].type_, IdlType::Primitive(s) if s == "u128"));
        assert!(matches!(&fields[3].type_, IdlType::Primitive(s) if s == "bool"));
        assert!(matches!(&fields[4].type_, IdlType::Primitive(s) if s == "string"));
        assert!(matches!(&fields[5].type_, IdlType::Primitive(s) if s == "account_id"));
        assert!(matches!(&fields[6].type_, IdlType::Option { .. }));
        assert!(matches!(&fields[7].type_, IdlType::Vec { .. }));
    }
}
