//! Runtime IDL generation from LEZ program source files.
//!
//! This module is gated behind the `idl-gen` feature and provides
//! `generate_idl_from_file()` for use by `lez-cli generate-idl`.
//!
//! The parsing logic mirrors the `generate_idl!` proc macro in
//! `lez-framework-macros`, but operates at runtime on a file path
//! rather than at compile time.

use std::fmt;
use std::path::Path;

use syn::{Attribute, FnArg, Ident, ItemFn, Pat, PatType, Type};

use crate::idl::{IdlAccountItem, IdlArg, IdlInstruction, IdlPda, IdlSeed, IdlType, LezIdl};

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

/// Parse a LEZ program source file and return its [`LezIdl`].
///
/// The path is resolved relative to the current working directory,
/// which is the natural behavior for a CLI tool.
pub fn generate_idl_from_file(source_path: &Path) -> Result<LezIdl, IdlGenError> {
    let content = std::fs::read_to_string(source_path)?;
    generate_idl_from_str(&content, &source_path.display().to_string())
}

/// Parse a LEZ program from source text and return its [`LezIdl`].
///
/// `source_label` is used only in error messages.
fn generate_idl_from_str(content: &str, source_label: &str) -> Result<LezIdl, IdlGenError> {
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

    // Build the LezIdl struct
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
                        Some(IdlPda { seeds })
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

    Ok(LezIdl {
        version: "0.1.0".to_string(),
        name: mod_name,
        instructions: idl_instructions,
        accounts: vec![],
        types: vec![],
        errors: vec![],
        spec: None,
        metadata: None,
        instruction_type: external_instruction,
    })
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

fn syn_type_to_idl_type(ty: &Type) -> IdlType {
    match ty {
        Type::Path(type_path) => {
            let segment = match type_path.path.segments.last() {
                Some(s) => s,
                None => return IdlType::Primitive("unknown".to_string()),
            };
            let ident = segment.ident.to_string();
            match ident.as_str() {
                "u8" | "u16" | "u32" | "u64" | "u128" | "i8" | "i16" | "i32" | "i64"
                | "i128" | "bool" | "String" => IdlType::Primitive(ident.to_lowercase()),
                "Vec" => {
                    if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                        if let Some(syn::GenericArgument::Type(inner)) = args.args.first() {
                            return IdlType::Vec {
                                vec: Box::new(syn_type_to_idl_type(inner)),
                            };
                        }
                    }
                    IdlType::Primitive("vec<unknown>".to_string())
                }
                "Option" => {
                    if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                        if let Some(syn::GenericArgument::Type(inner)) = args.args.first() {
                            return IdlType::Option {
                                option: Box::new(syn_type_to_idl_type(inner)),
                            };
                        }
                    }
                    IdlType::Primitive("option<unknown>".to_string())
                }
                "ProgramId" => IdlType::Primitive("program_id".to_string()),
                "AccountId" => IdlType::Primitive("account_id".to_string()),
                other => IdlType::Defined {
                    defined: other.to_string(),
                },
            }
        }
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
        }
        _ => IdlType::Primitive("unknown".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::idl::{IdlSeed, IdlType, LezIdl};

    fn ok(src: &str) -> LezIdl {
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
}
