//! # NSSA Framework Proc Macros
//!
//! This crate provides the `#[nssa_program]` attribute macro that eliminates
//! boilerplate in NSSA/LEZ guest binaries, and the `generate_idl!` macro
//! for extracting IDL from program source files.
//!
//! ## Usage
//!
//! ```rust,ignore
//! use nssa_framework::prelude::*;
//!
//! #[nssa_program]
//! mod my_program {
//!     #[instruction]
//!     pub fn create(
//!         #[account(init, pda = const("my_state"))]
//!         state: AccountWithMetadata,
//!         name: String,
//!     ) -> NssaResult {
//!         // business logic only
//!     }
//! }
//! ```
//!
//! ## IDL Generation
//!
//! ```rust,ignore
//! // generate_idl.rs — one-liner!
//! nssa_framework::generate_idl!("src/bin/treasury.rs");
//! ```

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{
    parse::Parser,
    parse_macro_input, Attribute, FnArg, Ident, ItemFn, ItemMod, Pat, PatType, Type,
};

/// Main entry point: `#[nssa_program]` on a module.
///
/// This macro:
/// 1. Finds all `#[instruction]` functions in the module
/// 2. Generates a serde-serializable `Instruction` enum
/// 3. Generates the `fn main()` with read/dispatch/write boilerplate
/// 4. Generates account validation code per instruction
/// 5. Generates `PROGRAM_IDL_JSON` const with complete IDL (including PDA seeds)
/// Program-level configuration parsed from `#[nssa_program(...)]` attributes.
struct ProgramConfig {
    /// External instruction enum path, e.g. `my_crate::Instruction`.
    /// If set, the macro will NOT generate its own `Instruction` enum.
    external_instruction: Option<syn::Path>,
}

impl ProgramConfig {
    fn parse(attr: TokenStream) -> syn::Result<Self> {
        let mut config = ProgramConfig {
            external_instruction: None,
        };
        if attr.is_empty() {
            return Ok(config);
        }
        let parser = syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated;
        let metas = parser.parse(attr)?;
        for meta in metas {
            if let syn::Meta::NameValue(nv) = &meta {
                if nv.path.is_ident("instruction") {
                    if let syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) = &nv.value {
                        config.external_instruction = Some(s.parse()?);
                    } else {
                        return Err(syn::Error::new_spanned(&nv.value, "expected string literal"));
                    }
                } else {
                    return Err(syn::Error::new_spanned(&nv.path, "unknown attribute"));
                }
            } else {
                return Err(syn::Error::new_spanned(&meta, "expected name = value"));
            }
        }
        Ok(config)
    }
}

#[proc_macro_attribute]
pub fn nssa_program(attr: TokenStream, item: TokenStream) -> TokenStream {
    let config = match ProgramConfig::parse(attr) {
        Ok(c) => c,
        Err(err) => return err.to_compile_error().into(),
    };
    let input = parse_macro_input!(item as ItemMod);
    match expand_nssa_program(input, config) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// Marker attribute for instruction functions within an `#[nssa_program]` module.
/// Processed by `#[nssa_program]`, not standalone.
#[proc_macro_attribute]
pub fn instruction(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Generate IDL from a program source file.
///
/// Parses the given Rust source file, finds the `#[nssa_program]` module,
/// and generates a `fn main()` that prints the complete IDL as JSON.
///
/// ```rust,ignore
/// nssa_framework_macros::generate_idl!("../../methods/guest/src/bin/treasury.rs");
/// ```
#[proc_macro]
pub fn generate_idl(input: TokenStream) -> TokenStream {
    let lit = parse_macro_input!(input as syn::LitStr);
    let file_path = lit.value();

    match expand_generate_idl(&file_path, &lit) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

// ─── Internal expansion logic ────────────────────────────────────────────

/// Parsed info about one instruction function.
struct InstructionInfo {
    fn_name: Ident,
    /// Account parameters (AccountWithMetadata type), in order
    accounts: Vec<AccountParam>,
    /// Non-account parameters (the instruction args)
    args: Vec<ArgParam>,
    /// The original function item (with #[instruction] stripped)
    func: ItemFn,
}

struct AccountParam {
    name: Ident,
    constraints: AccountConstraints,
    /// True if this is a Vec<AccountWithMetadata> (variable-length trailing accounts)
    is_rest: bool,
}

#[derive(Default)]
struct AccountConstraints {
    mutable: bool,
    init: bool,
    owner: Option<syn::Expr>,
    signer: bool,
    pda_seeds: Vec<PdaSeedDef>,
}

/// A PDA seed definition from the `#[account(pda = ...)]` attribute.
#[derive(Clone)]
enum PdaSeedDef {
    /// `const("some_string")` — a constant string seed
    Const(String),
    /// `account("other_account_name")` — seed derived from another account's ID
    Account(String),
    /// `arg("some_arg")` — seed derived from an instruction argument
    Arg(String),
}

struct ArgParam {
    name: Ident,
    ty: Type,
}

fn expand_nssa_program(input: ItemMod, config: ProgramConfig) -> syn::Result<TokenStream2> {
    let mod_name = &input.ident;

    let (_, items) = input
        .content
        .as_ref()
        .ok_or_else(|| syn::Error::new_spanned(&input, "nssa_program module must have a body"))?;

    // Collect instruction functions and other items
    let mut instructions: Vec<InstructionInfo> = Vec::new();
    let mut other_items: Vec<TokenStream2> = Vec::new();

    for item in items {
        match item {
            syn::Item::Fn(func) => {
                if has_instruction_attr(&func.attrs) {
                    instructions.push(parse_instruction(func.clone())?);
                } else {
                    other_items.push(quote! { #func });
                }
            }
            other => {
                other_items.push(quote! { #other });
            }
        }
    }

    if instructions.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "nssa_program must contain at least one #[instruction] function",
        ));
    }

    // Generate the Instruction enum (or use external one)
    let enum_def = if config.external_instruction.is_none() {
        let enum_variants = generate_enum_variants(&instructions);
        quote! {
            #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
            pub enum Instruction {
                #(#enum_variants),*
            }
        }
    } else {
        // External instruction: import it as `Instruction` if it's not already named that
        let path = config.external_instruction.as_ref().unwrap();
        quote! {
            use #path as Instruction;
        }
    };

    // Generate match arms for dispatch
    let match_arms = generate_match_arms(mod_name, &instructions);

    // Generate the handler functions (with #[instruction] stripped, account attrs stripped)
    let handler_fns = generate_handler_fns(&instructions);

    // Generate validation functions
    let validation_fns = generate_validation(&instructions);

    // Generate main function
    let main_fn = quote! {
        fn main() {
            // Read inputs from zkVM host
            let (nssa_core::program::ProgramInput { pre_states, instruction }, instruction_words)
                = nssa_core::program::read_nssa_inputs::<Instruction>();
            let pre_states_clone = pre_states.clone();

            // Dispatch to instruction handler
            let result: Result<
                (Vec<nssa_core::program::AccountPostState>, Vec<nssa_core::program::ChainedCall>),
                nssa_framework::error::NssaError
            > = match instruction {
                #(#match_arms)*
            };

            // Handle result
            let (post_states, chained_calls) = match result {
                Ok(output) => output,
                Err(e) => {
                    panic!("Program error [{}]: {}", e.error_code(), e);
                }
            };

            // Write outputs to zkVM host
            nssa_core::program::write_nssa_outputs_with_chained_call(
                instruction_words,
                pre_states_clone,
                post_states,
                chained_calls,
            );
        }
    };

    // Generate IDL function and const JSON
    let idl_fn = generate_idl_fn(mod_name, &instructions);
    let idl_json = generate_idl_json(mod_name, &instructions);

    // Assemble everything
    let expanded = quote! {
        // The instruction enum (used by both on-chain and client)
        #enum_def

        // Complete IDL as a const JSON string (accessible from any target)
        pub const PROGRAM_IDL_JSON: &str = #idl_json;

        // The program module with handler functions
        mod #mod_name {
            use super::*;

            #(#other_items)*

            #(#handler_fns)*

            #(#validation_fns)*
        }

        // IDL generation (available at host-side for tooling)
        #idl_fn

        // The guest binary entry point
        #main_fn
    };

    Ok(expanded)
}

fn has_instruction_attr(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| a.path().is_ident("instruction"))
}

fn parse_instruction(func: ItemFn) -> syn::Result<InstructionInfo> {
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
                return Err(syn::Error::new_spanned(
                    input,
                    "instruction functions cannot have self parameter",
                ));
            }
        }
    }

    Ok(InstructionInfo {
        fn_name,
        accounts,
        args,
        func,
    })
}

fn extract_param_name(pat_type: &PatType) -> syn::Result<Ident> {
    match &*pat_type.pat {
        Pat::Ident(pat_ident) => Ok(pat_ident.ident.clone()),
        _ => Err(syn::Error::new_spanned(
            &pat_type.pat,
            "expected simple identifier pattern",
        )),
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

/// Check if a type is Vec<AccountWithMetadata> (variable-length account list).
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

fn parse_account_constraints(attrs: &[Attribute]) -> syn::Result<AccountConstraints> {
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
                    let expr: syn::Expr = value.parse()?;
                    constraints.owner = Some(expr);
                    Ok(())
                } else if meta.path.is_ident("pda") {
                    // Parse PDA seeds: pda = const("value"), pda = account("name"), pda = arg("name")
                    let value = meta.value()?;
                    let expr: syn::Expr = value.parse()?;
                    constraints.pda_seeds = parse_pda_expr(&expr)?;
                    Ok(())
                } else {
                    Err(meta.error("unknown account constraint"))
                }
            })?;
        }
    }

    Ok(constraints)
}

/// Parse PDA seed expressions.
///
/// Supports:
/// - `const("string")` — constant seed
/// - `account("name")` — account-derived seed
/// - `arg("name")` — argument-derived seed
/// - `[const("a"), account("b")]` — multiple seeds (array syntax)
fn parse_pda_expr(expr: &syn::Expr) -> syn::Result<Vec<PdaSeedDef>> {
    match expr {
        // Single seed: const("value") or account("name")
        syn::Expr::Call(call) => {
            let seed = parse_single_pda_seed(call)?;
            Ok(vec![seed])
        }
        // Multiple seeds: [const("a"), account("b")]
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

fn parse_single_pda_seed(call: &syn::ExprCall) -> syn::Result<PdaSeedDef> {
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

// ─── Code generation helpers ─────────────────────────────────────────────

fn generate_enum_variants(instructions: &[InstructionInfo]) -> Vec<TokenStream2> {
    instructions
        .iter()
        .map(|ix| {
            let variant_name = to_pascal_case(&ix.fn_name);
            let fields: Vec<TokenStream2> = ix
                .args
                .iter()
                .map(|arg| {
                    let name = &arg.name;
                    let ty = &arg.ty;
                    quote! { #name: #ty }
                })
                .collect();

            if fields.is_empty() {
                quote! { #variant_name }
            } else {
                quote! { #variant_name { #(#fields),* } }
            }
        })
        .collect()
}

fn generate_match_arms(mod_name: &Ident, instructions: &[InstructionInfo]) -> Vec<TokenStream2> {
    instructions
        .iter()
        .map(|ix| {
            let variant_name = to_pascal_case(&ix.fn_name);
            let fn_name = &ix.fn_name;
            let num_accounts = ix.accounts.len();

            let field_names: Vec<&Ident> = ix.args.iter().map(|a| &a.name).collect();
            let pattern = if field_names.is_empty() {
                quote! { Instruction::#variant_name }
            } else {
                quote! { Instruction::#variant_name { #(#field_names),* } }
            };

            let has_rest = ix.accounts.iter().any(|a| a.is_rest);
            let account_destructure = if has_rest {
                // Split into fixed accounts + rest
                let fixed_accounts: Vec<&AccountParam> = ix.accounts.iter().filter(|a| !a.is_rest).collect();
                let rest_account = ix.accounts.iter().find(|a| a.is_rest).unwrap();
                let num_fixed = fixed_accounts.len();
                let fixed_names: Vec<&Ident> = fixed_accounts.iter().map(|a| &a.name).collect();
                let rest_name = &rest_account.name;
                
                quote! {
                    if pre_states.len() < #num_fixed {
                        panic!(
                            "Account count mismatch: expected at least {}, got {}",
                            #num_fixed, pre_states.len()
                        );
                    }
                    let (fixed_accounts, rest_accounts) = pre_states.split_at(#num_fixed);
                    let [#(#fixed_names),*] = <[_; #num_fixed]>::try_from(fixed_accounts.to_vec())
                        .unwrap_or_else(|v: Vec<_>| panic!(
                            "Account count mismatch: expected {}, got {}",
                            #num_fixed, v.len()
                        ));
                    let #rest_name: Vec<nssa_core::account::AccountWithMetadata> = rest_accounts.to_vec();
                }
            } else {
                let account_names: Vec<&Ident> = ix.accounts.iter().map(|a| &a.name).collect();
                quote! {
                    let [#(#account_names),*] = <[_; #num_accounts]>::try_from(pre_states)
                        .unwrap_or_else(|v: Vec<_>| panic!(
                            "Account count mismatch: expected {}, got {}",
                            #num_accounts, v.len()
                        ));
                }
            };

            // Check if this instruction has any validation (signer/init checks)
            let has_validation = ix.accounts.iter().any(|a| a.constraints.signer || a.constraints.init);
            let validate_fn_name = format_ident!("__validate_{}", ix.fn_name);

            let call_args: Vec<TokenStream2> = ix
                .accounts
                .iter()
                .map(|a| {
                    let name = &a.name;
                    quote! { #name }
                })
                .chain(ix.args.iter().map(|a| {
                    let name = &a.name;
                    quote! { #name }
                }))
                .collect();

            let validation_call = if has_validation {
                if has_rest {
                    // For instructions with Vec accounts, build the slice dynamically
                    let fixed_refs: Vec<TokenStream2> = ix.accounts.iter()
                        .filter(|a| !a.is_rest)
                        .map(|a| { let name = &a.name; quote! { #name.clone() } })
                        .collect();
                    let rest_ref = &ix.accounts.iter().find(|a| a.is_rest).unwrap().name;
                    quote! {
                        let mut __all_accounts = vec![#(#fixed_refs),*];
                        __all_accounts.extend(#rest_ref.clone());
                        #mod_name::#validate_fn_name(&__all_accounts).expect("account validation failed");
                    }
                } else {
                    let account_refs: Vec<TokenStream2> = ix
                        .accounts
                        .iter()
                        .map(|a| {
                            let name = &a.name;
                            quote! { #name }
                        })
                        .collect();
                    quote! {
                        #mod_name::#validate_fn_name(&[#(#account_refs.clone()),*]).expect("account validation failed");
                    }
                }
            } else {
                quote! {}
            };

            quote! {
                #pattern => {
                    #account_destructure
                    #validation_call
                    #mod_name::#fn_name(#(#call_args),*)
                        .map(|output| (output.post_states, output.chained_calls))
                }
            }
        })
        .collect()
}

fn generate_handler_fns(instructions: &[InstructionInfo]) -> Vec<TokenStream2> {
    instructions
        .iter()
        .map(|ix| {
            let mut func = ix.func.clone();
            func.attrs.retain(|a| !a.path().is_ident("instruction"));
            for input in &mut func.sig.inputs {
                if let FnArg::Typed(pat_type) = input {
                    pat_type.attrs.retain(|a| !a.path().is_ident("account"));
                }
            }
            quote! { #func }
        })
        .collect()
}

fn generate_validation(instructions: &[InstructionInfo]) -> Vec<TokenStream2> {
    instructions
        .iter()
        .map(|ix| {
            let fn_name = format_ident!("__validate_{}", ix.fn_name);
            
            // Generate signer checks for accounts with #[account(signer)]
            let signer_checks: Vec<TokenStream2> = ix
                .accounts
                .iter()
                .enumerate()
                .filter(|(_, acc)| acc.constraints.signer)
                .map(|(i, acc)| {
                    let acc_name = acc.name.to_string();
                    let idx = i;
                    quote! {
                        if !accounts[#idx].is_authorized {
                            return Err(nssa_framework::error::NssaError::Unauthorized {
                                message: format!("Account '{}' (index {}) must be a signer", #acc_name, #idx),
                            });
                        }
                    }
                })
                .collect();
            
            // Generate init checks for accounts with #[account(init)]
            let init_checks: Vec<TokenStream2> = ix
                .accounts
                .iter()
                .enumerate()
                .filter(|(_, acc)| acc.constraints.init)
                .map(|(i, acc)| {
                    let acc_name = acc.name.to_string();
                    let idx = i;
                    quote! {
                        if accounts[#idx].account != nssa_core::account::Account::default() {
                            return Err(nssa_framework::error::NssaError::AccountAlreadyInitialized {
                                account_index: #idx,
                            });
                        }
                    }
                })
                .collect();

            if signer_checks.is_empty() && init_checks.is_empty() {
                return quote! {};
            }

            quote! {
                #[allow(dead_code)]
                pub fn #fn_name(accounts: &[nssa_core::account::AccountWithMetadata]) -> Result<(), nssa_framework::error::NssaError> {
                    #(#signer_checks)*
                    #(#init_checks)*
                    Ok(())
                }
            }
        })
        .collect()
}

fn to_pascal_case(ident: &Ident) -> Ident {
    let s = ident.to_string();
    let pascal: String = s
        .split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect();
    format_ident!("{}", pascal)
}

// ─── IDL type conversion ─────────────────────────────────────────────────

fn rust_type_to_idl_string(ty: &Type) -> String {
    match ty {
        Type::Path(type_path) => {
            let segment = type_path.path.segments.last().unwrap();
            let ident = segment.ident.to_string();
            match ident.as_str() {
                "u8" | "u16" | "u32" | "u64" | "u128" | "i8" | "i16" | "i32" | "i64"
                | "i128" | "bool" | "String" => ident.to_lowercase(),
                "Vec" => {
                    if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                        if let Some(syn::GenericArgument::Type(inner)) = args.args.first() {
                            format!("vec<{}>", rust_type_to_idl_string(inner))
                        } else {
                            "vec<unknown>".to_string()
                        }
                    } else {
                        "vec<unknown>".to_string()
                    }
                }
                "ProgramId" => "program_id".to_string(),
                other => other.to_string(),
            }
        }
        Type::Array(arr) => {
            let elem = rust_type_to_idl_string(&arr.elem);
            if let syn::Expr::Lit(lit) = &arr.len {
                if let syn::Lit::Int(n) = &lit.lit {
                    return format!("[{}; {}]", elem, n);
                }
            }
            format!("[{}; ?]", elem)
        }
        _ => "unknown".to_string(),
    }
}

/// Convert a Rust IDL type string to the JSON representation.
/// This produces a JSON value string for embedding in const IDL JSON.
fn rust_type_to_idl_json(ty: &Type) -> String {
    match ty {
        Type::Path(type_path) => {
            let segment = type_path.path.segments.last().unwrap();
            let ident = segment.ident.to_string();
            match ident.as_str() {
                "u8" | "u16" | "u32" | "u64" | "u128" | "i8" | "i16" | "i32" | "i64"
                | "i128" | "bool" | "String" => {
                    format!("\"{}\"", ident.to_lowercase())
                }
                "Vec" => {
                    if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                        if let Some(syn::GenericArgument::Type(inner)) = args.args.first() {
                            format!("{{\"vec\":{}}}", rust_type_to_idl_json(inner))
                        } else {
                            "\"vec<unknown>\"".to_string()
                        }
                    } else {
                        "\"vec<unknown>\"".to_string()
                    }
                }
                "ProgramId" => "\"program_id\"".to_string(),
                other => format!("{{\"defined\":\"{}\"}}", other),
            }
        }
        Type::Array(arr) => {
            let elem = rust_type_to_idl_json(&arr.elem);
            if let syn::Expr::Lit(lit) = &arr.len {
                if let syn::Lit::Int(n) = &lit.lit {
                    return format!("{{\"array\":[{},{}]}}", elem, n);
                }
            }
            format!("{{\"array\":[{},0]}}", elem)
        }
        _ => "\"unknown\"".to_string(),
    }
}

// ─── IDL generation (code-based, for __program_idl()) ────────────────────

fn generate_idl_fn(mod_name: &Ident, instructions: &[InstructionInfo]) -> TokenStream2 {
    let program_name = mod_name.to_string();

    let instruction_literals: Vec<TokenStream2> = instructions
        .iter()
        .map(|ix| {
            let ix_name = ix.fn_name.to_string();

            let account_literals: Vec<TokenStream2> = ix
                .accounts
                .iter()
                .map(|acc| {
                    let acc_name = acc.name.to_string().trim_start_matches('_').to_string();
                    let writable = acc.constraints.mutable;
                    let signer = acc.constraints.signer;
                    let init = acc.constraints.init;

                    let pda_expr = if acc.constraints.pda_seeds.is_empty() {
                        quote! { None }
                    } else {
                        let seed_literals: Vec<TokenStream2> = acc
                            .constraints
                            .pda_seeds
                            .iter()
                            .map(|seed| match seed {
                                PdaSeedDef::Const(val) => quote! {
                                    nssa_framework::idl::IdlSeed::Const { value: #val.to_string() }
                                },
                                PdaSeedDef::Account(name) => quote! {
                                    nssa_framework::idl::IdlSeed::Account { path: #name.to_string() }
                                },
                                PdaSeedDef::Arg(name) => quote! {
                                    nssa_framework::idl::IdlSeed::Arg { path: #name.to_string() }
                                },
                            })
                            .collect();

                        quote! {
                            Some(nssa_framework::idl::IdlPda {
                                seeds: vec![#(#seed_literals),*],
                            })
                        }
                    };

                    let is_rest = acc.is_rest;
                    quote! {
                        nssa_framework::idl::IdlAccountItem {
                            name: #acc_name.to_string(),
                            writable: #writable,
                            signer: #signer,
                            init: #init,
                            owner: None,
                            pda: #pda_expr,
                            rest: #is_rest,
                        }
                    }
                })
                .collect();

            let arg_literals: Vec<TokenStream2> = ix
                .args
                .iter()
                .map(|arg| {
                    let arg_name = arg.name.to_string().trim_start_matches('_').to_string();
                    let type_str = rust_type_to_idl_string(&arg.ty);
                    quote! {
                        nssa_framework::idl::IdlArg {
                            name: #arg_name.to_string(),
                            type_: nssa_framework::idl::IdlType::Primitive(#type_str.to_string()),
                        }
                    }
                })
                .collect();

            quote! {
                nssa_framework::idl::IdlInstruction {
                    name: #ix_name.to_string(),
                    accounts: vec![#(#account_literals),*],
                    args: vec![#(#arg_literals),*],
                }
            }
        })
        .collect();

    quote! {
        #[allow(dead_code)]
        pub fn __program_idl() -> nssa_framework::idl::NssaIdl {
            nssa_framework::idl::NssaIdl {
                version: "0.1.0".to_string(),
                name: #program_name.to_string(),
                instructions: vec![#(#instruction_literals),*],
                accounts: vec![],
                types: vec![],
                errors: vec![],
            }
        }
    }
}

// ─── IDL generation (JSON string, for PROGRAM_IDL_JSON const) ────────────

fn generate_idl_json(mod_name: &Ident, instructions: &[InstructionInfo]) -> String {
    let program_name = mod_name.to_string();

    let instructions_json: Vec<String> = instructions
        .iter()
        .map(|ix| {
            let ix_name = &ix.fn_name.to_string();

            let accounts_json: Vec<String> = ix
                .accounts
                .iter()
                .map(|acc| {
                    let name = acc.name.to_string();
                    let writable = acc.constraints.mutable;
                    let signer = acc.constraints.signer;
                    let init = acc.constraints.init;

                    let pda_json = if acc.constraints.pda_seeds.is_empty() {
                        String::new()
                    } else {
                        let seeds: Vec<String> = acc
                            .constraints
                            .pda_seeds
                            .iter()
                            .map(|seed| match seed {
                                PdaSeedDef::Const(val) => {
                                    format!("{{\"kind\":\"const\",\"value\":\"{}\"}}", val)
                                }
                                PdaSeedDef::Account(name) => {
                                    format!("{{\"kind\":\"account\",\"path\":\"{}\"}}", name)
                                }
                                PdaSeedDef::Arg(name) => {
                                    format!("{{\"kind\":\"arg\",\"path\":\"{}\"}}", name)
                                }
                            })
                            .collect();
                        format!(",\"pda\":{{\"seeds\":[{}]}}", seeds.join(","))
                    };

                    format!(
                        "{{\"name\":\"{}\",\"writable\":{},\"signer\":{},\"init\":{}{}}}",
                        name, writable, signer, init, pda_json
                    )
                })
                .collect();

            let args_json: Vec<String> = ix
                .args
                .iter()
                .map(|arg| {
                    let name = arg.name.to_string();
                    let type_json = rust_type_to_idl_json(&arg.ty);
                    format!("{{\"name\":\"{}\",\"type\":{}}}", name, type_json)
                })
                .collect();

            format!(
                "{{\"name\":\"{}\",\"accounts\":[{}],\"args\":[{}]}}",
                ix_name,
                accounts_json.join(","),
                args_json.join(",")
            )
        })
        .collect();

    format!(
        "{{\"version\":\"0.1.0\",\"name\":\"{}\",\"instructions\":[{}],\"accounts\":[],\"types\":[],\"errors\":[]}}",
        program_name,
        instructions_json.join(",")
    )
}

// ─── generate_idl! macro implementation ──────────────────────────────────

fn expand_generate_idl(file_path: &str, span_token: &syn::LitStr) -> syn::Result<TokenStream2> {
    // Try the path as-is first, then relative to CARGO_MANIFEST_DIR
    let resolved_path = if std::path::Path::new(file_path).exists() {
        file_path.to_string()
    } else if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") {
        let p = std::path::Path::new(&manifest_dir).join(file_path);
        p.to_string_lossy().to_string()
    } else {
        file_path.to_string()
    };

    // Read the source file
    let content = std::fs::read_to_string(&resolved_path).map_err(|e| {
        syn::Error::new_spanned(
            span_token,
            format!("Failed to read '{}' (resolved: '{}'): {}", file_path, resolved_path, e),
        )
    })?;

    // Parse as a Rust file
    let file = syn::parse_file(&content).map_err(|e| {
        syn::Error::new_spanned(
            span_token,
            format!("Failed to parse '{}': {}", file_path, e),
        )
    })?;

    // Find the #[nssa_program] module
    let mut program_mod: Option<&ItemMod> = None;
    for item in &file.items {
        if let syn::Item::Mod(m) = item {
            if m.attrs.iter().any(|a| a.path().is_ident("nssa_program")) {
                program_mod = Some(m);
                break;
            }
        }
    }

    let program_mod = program_mod.ok_or_else(|| {
        syn::Error::new_spanned(
            span_token,
            format!(
                "No #[nssa_program] module found in '{}'",
                file_path
            ),
        )
    })?;

    let mod_name = &program_mod.ident;

    let (_, items) = program_mod.content.as_ref().ok_or_else(|| {
        syn::Error::new_spanned(span_token, "nssa_program module has no body")
    })?;

    // Parse instructions
    let mut instructions: Vec<InstructionInfo> = Vec::new();
    for item in items {
        if let syn::Item::Fn(func) = item {
            if has_instruction_attr(&func.attrs) {
                instructions.push(parse_instruction(func.clone())?);
            }
        }
    }

    if instructions.is_empty() {
        return Err(syn::Error::new_spanned(
            span_token,
            "No #[instruction] functions found in the program module",
        ));
    }

    // Generate the IDL JSON
    let idl_json = generate_idl_json(mod_name, &instructions);

    // Embed the resolved path for cargo tracking
    let resolved = resolved_path.clone();

    // Generate a main() that pretty-prints the IDL
    Ok(quote! {
        fn main() {
            // Help cargo track source changes
            const _SOURCE: &str = include_str!(#resolved);
            let json: serde_json::Value = serde_json::from_str(#idl_json)
                .expect("Generated IDL JSON is invalid");
            println!("{}", serde_json::to_string_pretty(&json).unwrap());
        }
    })
}
