//! # SPEL Framework Proc Macros
//!
//! This crate provides the `#[lez_program]` attribute macro that eliminates
//! boilerplate in SPEL guest binaries, and the `generate_idl!` macro
//! for extracting IDL from program source files.
//!
//! ## Usage
//!
//! ```rust,ignore
//! use spel_framework::prelude::*;
//!
//! #[lez_program]
//! mod my_program {
//!     #[instruction]
//!     pub fn create(
//!         #[account(init, pda = const("my_state"))]
//!         state: AccountWithMetadata,
//!         name: String,
//!     ) -> SpelResult {
//!         // business logic only
//!     }
//! }
//! ```
//!
//! ## IDL Generation
//!
//! ```rust,ignore
//! // generate_idl.rs — one-liner!
//! spel_framework::generate_idl!("src/bin/treasury.rs");
//! ```

use proc_macro::TokenStream;
use sha2::{Sha256, Digest};
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{
    parse::Parser,
    parse_macro_input, Attribute, FnArg, Ident, ItemFn, ItemMod, Pat, PatType, Type,
    visit_mut::{self, VisitMut},
};

mod account_types;

/// Main entry point: `#[lez_program]` on a module.
///
/// This macro:
/// 1. Finds all `#[instruction]` functions in the module
/// 2. Generates a serde-serializable `Instruction` enum
/// 3. Generates the `fn main()` with read/dispatch/write boilerplate
/// 4. Generates account validation code per instruction
/// 5. Generates `PROGRAM_IDL_JSON` const with complete IDL (including PDA seeds)
/// Program-level configuration parsed from `#[lez_program(...)]` attributes.
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
pub fn lez_program(attr: TokenStream, item: TokenStream) -> TokenStream {
    let config = match ProgramConfig::parse(attr) {
        Ok(c) => c,
        Err(err) => return err.to_compile_error().into(),
    };
    let input = parse_macro_input!(item as ItemMod);
    match expand_lez_program(input, config) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// Marker attribute for instruction functions within an `#[lez_program]` module.
/// Processed by `#[lez_program]`, not standalone.
#[proc_macro_attribute]
pub fn instruction(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Marker attribute for account data types.
///
/// Place this on any struct or enum whose Borsh-encoded bytes are stored
/// in on-chain accounts. `spel generate-idl` will include the type in the
/// IDL so that `spel inspect` can decode account data of this shape.
///
/// ```rust,ignore
/// #[account_type]
/// #[derive(BorshSerialize, BorshDeserialize)]
/// pub struct VaultState {
///     pub owner: AccountId,
///     pub balance: u64,
/// }
/// ```
///
/// This attribute is a no-op at compile time; it is consumed solely by the
/// IDL generator.
#[proc_macro_attribute]
pub fn account_type(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Marks an instruction as requiring admin authority.
///
/// Place this on any `#[instruction]` function that should only be callable
/// by the admin authority. The macro injects a runtime check that verifies
/// the signer matches the admin key stored in the config PDA.
///
/// ## Usage
///
/// ```rust,ignore
/// #[lez_program]
/// mod my_program {
///     #[instruction]
///     #[require_admin(config)]
///     pub fn set_value(
///         #[account(mut, pda = literal("config"))]
///         config: AccountWithMetadata,
///         #[account(signer)]
///         admin: AccountWithMetadata,
///         value: u64,
///     ) -> SpelResult {
///         // admin check is injected automatically before this body runs
///         // ...
///     }
/// }
/// ```
///
/// The `config` argument names the account parameter that holds the
/// `AdminConfig` state. The macro reads `AdminConfig` from that account
/// and calls `assert_admin()` with the signer before the handler body runs.
///
/// This attribute is processed by `#[lez_program]`, not standalone.
#[proc_macro_attribute]
pub fn require_admin(_attr: TokenStream, item: TokenStream) -> TokenStream {
    // Marker only — actual injection is handled by #[lez_program] expansion
    // which detects this attribute and wraps the handler body with the admin check.
    item
}

/// Generate IDL from a program source file.
///
/// Parses the given Rust source file, finds the `#[lez_program]` module,
/// and generates a `fn main()` that prints the complete IDL as JSON.
///
/// ```rust,ignore
/// spel_framework_macros::generate_idl!("../../methods/guest/src/bin/treasury.rs");
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
    /// True if this instruction has a ProgramContext parameter.
    /// The context is injected by the dispatcher and never appears in IDL/ABI.
    has_context: bool,
    /// If Some(config_account_name), this instruction is gated by admin authority.
    /// The named account must contain an AdminConfig with an AdminState.
    require_admin: Option<String>,
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
    /// True when `private_pda` keyword is present — address includes the caller's npk.
    private_pda: bool,
    /// Name of the instruction arg supplying the `NullifierPublicKey` for derivation.
    npk_arg: Option<String>,
}

/// A PDA seed definition from the `#[account(pda = ...)]` attribute.
#[derive(Clone)]
enum PdaSeedDef {
    /// `const("some_string")` — a constant string seed.
    /// `literal("some_string")` is accepted as an alias for backwards compatibility.
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

fn expand_lez_program(input: ItemMod, config: ProgramConfig) -> syn::Result<TokenStream2> {
    let mod_name = &input.ident;

    let (_, items) = input
        .content
        .as_ref()
        .ok_or_else(|| syn::Error::new_spanned(&input, "lez_program module must have a body"))?;

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
            "lez_program must contain at least one #[instruction] function",
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

    // Generate per-instruction __claims_*() functions for auto-claim
    let claim_fns = generate_claim_fns(&instructions);

    // Generate main function.
    // `pub fn main` (not just `fn main`) is required so the zkVM linker can find the entry point
    // when this crate is compiled as a guest binary dependency.
    let main_fn = quote! {
        pub fn main() {
            // Read inputs from zkVM host
            let (nssa_core::program::ProgramInput { self_program_id, caller_program_id, pre_states, instruction }, instruction_words)
                = nssa_core::program::read_nssa_inputs::<Instruction>();
            let pre_states_clone = pre_states.clone();

            // Dispatch to instruction handler
            let result: Result<
                spel_framework::SpelOutputParts,
                spel_framework::error::SpelError
            > = match instruction {
                #(#match_arms)*
            };

            // Handle result
            let parts = match result {
                Ok(output) => output,
                Err(e) => {
                    panic!("Program error [{}]: {}", e.error_code(), e);
                }
            };
            let post_states = parts.post_states;
            let chained_calls = parts.chained_calls;
            let block_validity_window = parts.block_validity_window;
            let timestamp_validity_window = parts.timestamp_validity_window;

            // Filter out non-program-owned, non-default-state accounts from the output.
            //
            // LEZ validate_execution rule 7: if post.program_owner == DEFAULT_PROGRAM_ID
            // and pre.account != Account::default(), validation fails. This would happen
            // for signer accounts (e.g., proposer/executor) whose nonce has been incremented
            // by a prior transaction — they are not owned by the program and must not be
            // returned in the program's post-states.
            //
            // We drop any (pre, post) pair where:
            //   - pre.program_owner == DEFAULT_PROGRAM_ID (not owned by this program), AND
            //   - pre.account != Account::default() (has non-trivial state), AND
            //   - post has no claim (init accounts are fine since their pre == default)
            let (filtered_pre, filtered_post): (
                Vec<nssa_core::account::AccountWithMetadata>,
                Vec<nssa_core::program::AccountPostState>,
            ) = pre_states_clone
                .into_iter()
                .zip(post_states.into_iter())
                .filter(|(pre, post)| {
                    let is_default_owner =
                        pre.account.program_owner == nssa_core::program::DEFAULT_PROGRAM_ID;
                    let pre_is_default =
                        pre.account == nssa_core::account::Account::default();
                    let has_claim = post.required_claim().is_some();
                    !is_default_owner || pre_is_default || has_claim
                })
                .unzip();

            // Write outputs to zkVM host
            nssa_core::program::ProgramOutput::new(
                self_program_id,
                caller_program_id,
                instruction_words,
                filtered_pre,
                filtered_post,
            )
            .with_chained_calls(chained_calls)
            .with_block_validity_window(block_validity_window)
            .with_timestamp_validity_window(timestamp_validity_window)
            .write();
        }
    };

    // Generate IDL function and const JSON
    let ext_instr_str: Option<String> = config.external_instruction.as_ref().map(|p| {
        let segments: Vec<String> = p.segments.iter().map(|s| s.ident.to_string()).collect();
        segments.join("::")
    });

    // Collect #[account_type] annotated types from the source file's top-level items.
    // Expands the candidate set to cover common Rust module/bin layouts and verifies
    // that the candidate file actually defines the target module, avoiding false matches.
    let (accounts, types) = {
        let module_path = mod_name.to_string();
        let mut result = (Vec::new(), Vec::new());

        if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") {
            let manifest = std::path::Path::new(&manifest_dir);

            // Check if a parsed file defines the target module
            let file_matches_module = |parsed_file: &syn::File| {
                parsed_file.items.iter().any(|item| {
                    matches!(item, syn::Item::Mod(item_mod) if item_mod.ident == *mod_name)
                })
            };

            let mut candidate_paths: Vec<std::path::PathBuf> = vec![
                manifest.join("src/bin").join(format!("{}.rs", module_path)),          // src/bin/{name}.rs
                manifest.join("src/bin").join(&module_path).join("main.rs"),           // src/bin/{name}/main.rs
                manifest.join("src").join(format!("{}.rs", module_path)),              // src/{name}.rs
                manifest.join("src").join(&module_path).join("mod.rs"),                // src/{name}/mod.rs
                manifest.join("src").join("lib.rs"),                                   // src/lib.rs
                manifest.join("src").join("main.rs"),                                  // src/main.rs
            ];

            // Scan src/bin/ for additional files and subdirectories
            let src_bin = manifest.join("src/bin");
            if let Ok(entries) = std::fs::read_dir(&src_bin) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                        if !candidate_paths.iter().any(|p| p == &path) {
                            candidate_paths.push(path);
                        }
                    } else if path.is_dir() {
                        let main_rs = path.join("main.rs");
                        if main_rs.is_file() && !candidate_paths.iter().any(|p| p == &main_rs) {
                            candidate_paths.push(main_rs);
                        }
                    }
                }
            }

            // Scan src/ for additional files and subdirectories
            let src = manifest.join("src");
            if let Ok(entries) = std::fs::read_dir(&src) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                        if !candidate_paths.iter().any(|p| p == &path) {
                            candidate_paths.push(path);
                        }
                    } else if path.is_dir() {
                        let mod_rs = path.join("mod.rs");
                        if mod_rs.is_file() && !candidate_paths.iter().any(|p| p == &mod_rs) {
                            candidate_paths.push(mod_rs);
                        }
                    }
                }
            }

            for guest_path in &candidate_paths {
                if let Ok(content_str) = std::fs::read_to_string(guest_path) {
                    if let Ok(parsed_file) = syn::parse_file(&content_str) {
                        if file_matches_module(&parsed_file) {
                            // Collect from top-level items AND from inside the
                            // #[lez_program] module body (account types are often
                            // defined inside the module).
                            let mut all_items: Vec<syn::Item> = parsed_file.items.clone();
                            for item in &parsed_file.items {
                                if let syn::Item::Mod(m) = item {
                                    if m.ident == *mod_name {
                                        if let Some((_, mod_items)) = &m.content {
                                            all_items.extend(mod_items.clone());
                                        }
                                    }
                                }
                            }
                            result = account_types::collect_account_types(&all_items);
                            break;
                        }
                    }
                }
            }
        }
        result
    };

    let idl_fn = generate_idl_fn(mod_name, &instructions, ext_instr_str.as_deref(), accounts.clone(), types.clone());
    let idl_json = generate_idl_json(mod_name, &instructions, ext_instr_str.as_deref(), accounts, types);

    // Assemble everything
    let expanded = quote! {
        // The instruction enum (used by both on-chain and client)
        #enum_def

        // Complete IDL as a const JSON string (accessible from any target)
        pub const PROGRAM_IDL_JSON: &str = #idl_json;

        // The program module is pub so host-side tests and tooling can call handler functions,
        // validation helpers (__validate_*), and claims helpers (__claims_*) directly.
        pub mod #mod_name {
            use super::*;

            #(#other_items)*

            #(#handler_fns)*

            #(#validation_fns)*

            #(#claim_fns)*
        }

        // IDL generation (available at host-side for tooling)
        #idl_fn

        // The guest binary entry point (cfg-gated so cargo test works on host)
        #[cfg(not(test))]
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
    let mut has_context = false;

    for (idx, input) in func.sig.inputs.iter().enumerate() {
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
                } else if is_context_type(ty) {
                    // ProgramContext — injected by dispatcher, not part of ABI/IDL.
                    if has_context {
                        return Err(syn::Error::new_spanned(
                            ty,
                            "instruction functions can have at most one ProgramContext parameter",
                        ));
                    }
                    if idx != 0 {
                        return Err(syn::Error::new_spanned(
                            ty,
                            "ProgramContext must be the first parameter of an instruction function",
                        ));
                    }
                    has_context = true;
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

    // Check for #[require_admin(config_account)] attribute
    let require_admin = func.attrs.iter().find_map(|attr| {
        if attr.path().is_ident("require_admin") {
            // Parse the account name from #[require_admin(config)]
            let mut account_name = String::from("config"); // default
            let _ = attr.parse_nested_meta(|meta| {
                account_name = meta.path.get_ident()
                    .map(|i| i.to_string())
                    .unwrap_or_else(|| "config".to_string());
                Ok(())
            });
            Some(account_name)
        } else {
            None
        }
    });

    Ok(InstructionInfo {
        fn_name,
        accounts,
        args,
        has_context,
        require_admin,
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

/// Check if a type is ProgramContext (execution context injected by dispatcher).
fn is_context_type(ty: &Type) -> bool {
    if let Type::Path(type_path) = ty {
        if let Some(segment) = type_path.path.segments.last() {
            return segment.ident == "ProgramContext";
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
                } else if meta.path.is_ident("private_pda") {
                    constraints.private_pda = true;
                    Ok(())
                } else if meta.path.is_ident("npk") {
                    // npk = arg("arg_name") — instruction arg supplying the NullifierPublicKey
                    let value = meta.value()?;
                    let expr: syn::Expr = value.parse()?;
                    if let syn::Expr::Call(call) = &expr {
                        if let syn::Expr::Path(path) = &*call.func {
                            if path.path.is_ident("arg") {
                                if let Some(syn::Expr::Lit(lit)) = call.args.first() {
                                    if let syn::Lit::Str(s) = &lit.lit {
                                        constraints.npk_arg = Some(s.value());
                                        return Ok(());
                                    }
                                }
                            }
                        }
                    }
                    Err(meta.error("npk must be npk = arg(\"arg_name\")"))
                } else {
                    Err(meta.error("unknown account constraint"))
                }
            })?;
        }
    }

    // Validate constraint consistency
    if constraints.private_pda {
        if constraints.pda_seeds.is_empty() {
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                "`private_pda` requires `pda = ...` seeds",
            ));
        }
        if constraints.npk_arg.is_none() {
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                "`private_pda` requires `npk = arg(\"arg_name\")`",
            ));
        }
        // Validate the npk arg name is a valid Rust identifier
        let npk_name = constraints.npk_arg.as_deref().unwrap();
        if syn::parse_str::<syn::Ident>(npk_name).is_err() {
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                format!("`npk` arg name `{npk_name}` is not a valid Rust identifier"),
            ));
        }
    } else if constraints.npk_arg.is_some() {
        return Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            "`npk` can only be used together with `private_pda`",
        ));
    }

    Ok(constraints)
}

/// Parse PDA seed expressions.
///
/// Supports:
/// - `const("string")` — constant seed (`literal("string")` is accepted as an alias)
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

            // Check if this instruction has any validation (signer/init/owner/pda checks)
            let has_validation = ix.accounts.iter().any(|a| {
                a.constraints.signer || a.constraints.init || a.constraints.owner.is_some() || !a.constraints.pda_seeds.is_empty()
            });
            let validate_fn_name = format_ident!("__validate_{}", ix.fn_name);

            let call_args: Vec<TokenStream2> = {
                let mut args: Vec<TokenStream2> = Vec::new();
                // Context is always first if present (enforced by parse_instruction).
                // caller_program_id is Option<ProgramId> from ProgramInput; default to zeroed ID.
                if ix.has_context {
                    args.push(quote! {
                        spel_framework::context::ProgramContext::new(
                            self_program_id,
                            caller_program_id.unwrap_or(nssa_core::program::DEFAULT_PROGRAM_ID)
                        )
                    });
                }
                args.extend(ix.accounts.iter().map(|a| {
                    let name = &a.name;
                    quote! { #name }
                }));
                args.extend(ix.args.iter().map(|a| {
                    let name = &a.name;
                    quote! { #name }
                }));
                args
            };

            // Collect arg seed values to pass to validation
            let arg_seed_values: Vec<TokenStream2> = {
                let mut names = Vec::new();
                for acc in &ix.accounts {
                    for seed in &acc.constraints.pda_seeds {
                        if let PdaSeedDef::Arg(name) = seed {
                            if !names.contains(name) {
                                names.push(name.clone());
                            }
                        }
                    }
                }
                names.iter().map(|name| {
                    let arg_ident = format_ident!("{}", name);
                    quote! { &#arg_ident }
                }).collect()
            };

            // Collect npk arg values for private PDA accounts
            let npk_arg_values: Vec<TokenStream2> = {
                let mut names = Vec::new();
                for acc in &ix.accounts {
                    if let Some(ref npk) = acc.constraints.npk_arg {
                        if !names.contains(npk) {
                            names.push(npk.clone());
                        }
                    }
                }
                names.iter().map(|name| {
                    let arg_ident = format_ident!("{}", name);
                    quote! { &#arg_ident }
                }).collect()
            };

            let all_extra_args: Vec<TokenStream2> = arg_seed_values.iter()
                .chain(npk_arg_values.iter())
                .cloned()
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
                        #mod_name::#validate_fn_name(
                            &__all_accounts,
                            &self_program_id,
                            &instruction_words,
                            #(#all_extra_args),*
                        ).expect("account validation failed");
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
                        #mod_name::#validate_fn_name(
                            &[#(#account_refs.clone()),*],
                            &self_program_id,
                            &instruction_words,
                            #(#all_extra_args),*
                        ).expect("account validation failed");
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
                        .map(|output| output.into_parts())
                }
            }
        })
        .collect()
}

// ─── SpelOutput::execute() auto-claim transformer ──────────────────────

/// Walks a handler function body and rewrites `SpelOutput::execute(...)` calls:
///
/// - **Fixed accounts** (`vec![a, b]`):
///   → `SpelOutput::execute_with_claims(&[a.account.clone(), ...], &__claims_fn(...), calls)`
///
/// - **Dynamic accounts** (any expression, for instructions with `Vec<AccountWithMetadata>`):
///   → `{ let __accs = accounts_expr; let __extracted = ...; SpelOutput::execute_with_claims(&__extracted, &__claims_fn(__accs.len() - NUM_FIXED, ...), calls) }`
///   The block binds accounts_expr once to avoid double evaluation.
struct ExecuteTransformer<'a> {
    accounts: &'a [AccountParam],
    fn_name: &'a Ident,
}

impl<'a> ExecuteTransformer<'a> {
    fn has_rest(&self) -> bool {
        self.accounts.iter().any(|a| a.is_rest)
    }

    fn num_fixed(&self) -> usize {
        self.accounts.iter().filter(|a| !a.is_rest).count()
    }

    /// Collect arg seed values as function call arguments for __claims_* functions.
    /// For each unique PdaSeedDef::Arg across all accounts, generates: &arg_name
    fn arg_seed_args(&self) -> Vec<TokenStream2> {
        let mut names: Vec<String> = Vec::new();
        for acc in self.accounts {
            for seed in &acc.constraints.pda_seeds {
                if let PdaSeedDef::Arg(name) = seed {
                    if !names.contains(name) {
                        names.push(name.clone());
                    }
                }
            }
        }
        names.iter().map(|name| {
            let ident = format_ident!("{}", name);
            quote! { &#ident }
        }).collect()
    }

    /// Collect account-seed arguments for the vec![ident, ...] pattern.
    /// For each unique PdaSeedDef::Account, generates: &*ident.account_id.value()
    fn account_seed_args_from_idents(&self, account_idents: &[Ident]) -> Vec<TokenStream2> {
        let mut seen: Vec<String> = Vec::new();
        let mut result = Vec::new();
        for acc in self.accounts {
            for seed in &acc.constraints.pda_seeds {
                if let PdaSeedDef::Account(path) = seed {
                    let name = path.split('.').next().unwrap_or(path.as_str()).to_string();
                    if !seen.contains(&name) {
                        seen.push(name.clone());
                        if let Some(ident) = account_idents.iter().find(|i| i.to_string() == name) {
                            result.push(quote! { &*#ident.account_id.value() });
                        }
                    }
                }
            }
        }
        result
    }

    /// Collect account-seed arguments for the rest-accounts branch.
    /// `binding` is the local variable name holding Vec<AccountWithMetadata>.
    /// For each unique PdaSeedDef::Account, generates: &*binding[idx].account_id.value()
    fn account_seed_args_for_rest(&self, binding: &TokenStream2) -> Vec<TokenStream2> {
        let mut seen: Vec<String> = Vec::new();
        let mut result = Vec::new();
        for acc in self.accounts {
            for seed in &acc.constraints.pda_seeds {
                if let PdaSeedDef::Account(path) = seed {
                    let name = path.split('.').next().unwrap_or(path.as_str()).to_string();
                    if !seen.contains(&name) {
                        seen.push(name.clone());
                        let idx = self.accounts.iter()
                            .position(|a| a.name.to_string() == name)
                            .unwrap_or(0);
                        result.push(quote! { &*#binding[#idx].account_id.value() });
                    }
                }
            }
        }
        result
    }
}

impl<'a> VisitMut for ExecuteTransformer<'a> {
    fn visit_expr_mut(&mut self, expr: &mut syn::Expr) {
        // Recurse into sub-expressions first
        visit_mut::visit_expr_mut(self, expr);

        // Clone what we need before mutably borrowing expr below
        let (accounts_arg, chained_arg) = {
            let call = if let syn::Expr::Call(c) = &*expr { c } else { return; };
            if !is_spel_output_execute(&call.func) || call.args.len() != 2 {
                return;
            }
            (call.args[0].clone(), call.args[1].clone())
        };

        let claims_fn = format_ident!("__claims_{}", self.fn_name);
        let arg_seed_args: Vec<TokenStream2> = self.arg_seed_args();

        // Try vec![ident, ...] pattern first (fixed-size accounts, most common case)
        if let Some(account_idents) = extract_vec_macro_idents(&accounts_arg) {
            // Verify all account names are known before transforming
            let mut account_clones: Vec<TokenStream2> = Vec::new();
            for ident in &account_idents {
                if self.accounts.iter().find(|a| a.name == *ident).is_none() {
                    return; // unknown account — don't transform
                }
                account_clones.push(quote! { #ident.account.clone() });
            }
            let account_seed_args = self.account_seed_args_from_idents(&account_idents);
            let all_seed_args: Vec<TokenStream2> = arg_seed_args.into_iter()
                .chain(account_seed_args)
                .collect();
            if let syn::Expr::Call(call) = expr {
                call.func = syn::parse_quote! { SpelOutput::execute_with_claims };
                call.args.clear();
                call.args.push(syn::parse_quote! { &[#(#account_clones),*] });
                call.args.push(syn::parse_quote! { &#claims_fn(#(#all_seed_args),*) });
                call.args.push(syn::parse_quote! { #chained_arg });
            }
            return;
        }

        // For instructions with Vec<AccountWithMetadata> (rest accounts): use a block to bind
        // accounts_expr exactly once, fixing double evaluation and allowing account-seed lookup.
        if self.has_rest() {
            let num_fixed = self.num_fixed();
            let accs = quote! { __accs };
            let account_seed_args = self.account_seed_args_for_rest(&accs);
            let all_seed_args: Vec<TokenStream2> = arg_seed_args.into_iter()
                .chain(account_seed_args)
                .collect();
            *expr = syn::parse_quote! {
                {
                    let __accs: ::std::vec::Vec<_> = #accounts_arg;
                    let __extracted: ::std::vec::Vec<_> =
                        __accs.iter().map(|__a| __a.account.clone()).collect();
                    SpelOutput::execute_with_claims(
                        &__extracted,
                        &#claims_fn(__accs.len() - #num_fixed #(, #all_seed_args)*),
                        #chained_arg
                    )
                }
            };
            return;
        }

        // Fixed-account instruction with an arbitrary accounts expression (e.g. a Vec<Account>
        // variable built by the handler). The vec![name, ...] pattern above handles the common
        // case; this catches anything else. Note: account(...) PDA seeds cannot be resolved here
        // because AccountWithMetadata is not available — use vec![...] for those instructions.
        let all_seed_args: Vec<TokenStream2> = arg_seed_args;
        if let syn::Expr::Call(call) = expr {
            call.func = syn::parse_quote! { SpelOutput::execute_with_claims };
            call.args.clear();
            call.args.push(syn::parse_quote! { &#accounts_arg });
            call.args.push(syn::parse_quote! { &#claims_fn(#(#all_seed_args),*) });
            call.args.push(syn::parse_quote! { #chained_arg });
        }
    }
}

fn is_spel_output_execute(func: &syn::Expr) -> bool {
    if let syn::Expr::Path(ep) = func {
        let segments: Vec<_> = ep.path.segments.iter().collect();
        if segments.len() == 2 {
            return segments[0].ident == "SpelOutput" && segments[1].ident == "execute";
        }
    }
    false
}

fn extract_vec_macro_idents(expr: &syn::Expr) -> Option<Vec<Ident>> {
    if let syn::Expr::Macro(em) = expr {
        if em.mac.path.is_ident("vec") {
            let parser =
                syn::punctuated::Punctuated::<syn::Ident, syn::Token![,]>::parse_terminated;
            if let Ok(idents) = parser.parse2(em.mac.tokens.clone()) {
                return Some(idents.into_iter().collect());
            }
        }
    }
    None
}


fn generate_handler_fns(instructions: &[InstructionInfo]) -> Vec<TokenStream2> {
    instructions
        .iter()
        .map(|ix| {
            let mut func = ix.func.clone();
            func.attrs.retain(|a| !a.path().is_ident("instruction"));
            func.attrs.retain(|a| !a.path().is_ident("require_admin"));
            for input in &mut func.sig.inputs {
                if let FnArg::Typed(pat_type) = input {
                    pat_type.attrs.retain(|a| !a.path().is_ident("account"));
                }
            }

            // Inject admin authority check if #[require_admin] is present
            if let Some(config_name) = &ix.require_admin {
                let config_ident = format_ident!("{}", config_name);
                // Find the signer account (first account with signer constraint)
                let signer_name = ix.accounts.iter()
                    .find(|a| a.constraints.signer)
                    .map(|a| a.name.clone());

                if let Some(signer_ident) = signer_name {
                    // Prepend admin check to function body
                    let original_block = func.block.clone();
                    func.block = syn::parse_quote! {
                        {
                            // Injected by #[require_admin]: verify admin authority
                            {
                                let __admin_data = &#config_ident.account.data;
                                let __admin_state = spel_admin_authority::AdminConfig::try_from_slice(__admin_data)
                                    .map_err(|_| spel_framework::error::SpelError::Unauthorized {
                                        message: "Failed to deserialize AdminConfig from config account".to_string(),
                                    })?;
                                let __signer_key = *#signer_ident.account_id.value();
                                __admin_state.admin_state.assert_admin(&__signer_key)
                                    .map_err(|e| spel_framework::error::SpelError::Unauthorized {
                                        message: e.to_string(),
                                    })?;
                            }
                            #original_block
                        }
                    };
                }
            }

            // Transform SpelOutput::execute(vec![...], calls) → execute_with_claims
            let mut transformer = ExecuteTransformer {
                accounts: &ix.accounts,
                fn_name: &ix.fn_name,
            };
            transformer.visit_item_fn_mut(&mut func);
            quote! { #func }
        })
        .collect()
}

/// Generate the `AutoClaim` token stream for a single account based on its constraints.
///
/// For `PdaSeedDef::Account`, the generated expression references a `__account_seed_{name}: &[u8;32]`
/// parameter that the caller (claims function) receives at runtime, matching the actual account ID
/// used by the validation function. This is the correct counterpart to `generate_validation`.
fn generate_single_claim_expr(acc: &AccountParam) -> TokenStream2 {
    if acc.constraints.init && !acc.constraints.pda_seeds.is_empty() {
        let seed_bytes: Vec<TokenStream2> = acc
            .constraints
            .pda_seeds
            .iter()
            .map(|seed| {
                match seed {
                    PdaSeedDef::Const(v) => {
                        let val = v.clone();
                        quote! { &spel_framework::pda::seed_from_str(#val) }
                    }
                    PdaSeedDef::Account(path) => {
                        // Use a runtime parameter holding the actual account ID bytes,
                        // matching how generate_validation resolves account seeds.
                        let account_name = path.split('.').next().unwrap_or(path.as_str());
                        let ident = format_ident!("__account_seed_{}", account_name);
                        quote! { #ident }  // already &[u8; 32]
                    }
                    PdaSeedDef::Arg(name) => {
                        let ident = format_ident!("__pda_arg_{}", name);
                        quote! { &spel_framework::pda::ToSeed::to_seed(#ident) }
                    }
                }
            })
            .collect();
        if seed_bytes.len() == 1 {
            let seed = &seed_bytes[0];
            quote! {
                spel_framework::spel_output::AutoClaim::Claimed(
                    nssa_core::program::Claim::Pda(
                        nssa_core::program::PdaSeed::new(*#seed)
                    )
                )
            }
        } else {
            quote! {
                spel_framework::spel_output::AutoClaim::pda_from_seeds(
                    &[#(#seed_bytes),*]
                )
            }
        }
    } else if acc.constraints.init {
        quote! {
            spel_framework::spel_output::AutoClaim::Claimed(
                nssa_core::program::Claim::Authorized
            )
        }
    } else {
        quote! { spel_framework::spel_output::AutoClaim::None }
    }
}

/// Generate per-instruction `__claims_{fn_name}()` functions that return
/// `Vec<AutoClaim>` based on account constraints. These are used by
/// `SpelOutput::execute_with_claims()` so users don't have to manually
/// choose `new()` vs `new_claimed()`.
///
/// Auto-claim rules:
/// - `#[account(init, pda = ...)]` → `Claim::Pda(seeds)`
/// - `#[account(init, signer)]`    → `Claim::Authorized`
/// - `#[account(init)]`            → `Claim::Authorized`
/// - `#[account(mut)]`             → `Claim::None`
/// - `#[account]`                  → `Claim::None`
///
/// For instructions with `Vec<AccountWithMetadata>` (rest accounts), the
/// generated function takes a `rest_count: usize` parameter and repeats
/// the rest account's claim that many times.
///
/// For `account(...)` PDA seeds, the generated function takes an additional
/// `__account_seed_{name}: &[u8; 32]` parameter per referenced account, so the
/// caller can pass the actual runtime account ID (matching what validation does).

/// Collect the unique PDA arg seed parameters for a given instruction as typed
/// `__pda_arg_<name>: &<type>` token streams, used in generated function signatures.
fn pda_arg_params(ix: &InstructionInfo) -> Vec<TokenStream2> {
    let mut names: Vec<String> = Vec::new();
    for acc in &ix.accounts {
        for seed in &acc.constraints.pda_seeds {
            if let PdaSeedDef::Arg(name) = seed {
                if !names.contains(name) {
                    names.push(name.clone());
                }
            }
        }
    }
    names.iter().map(|name| {
        let ident = format_ident!("__pda_arg_{}", name);
        let actual_type = ix.args.iter()
            .find(|a| a.name.to_string() == *name)
            .map(|a| &a.ty);
        if let Some(ty) = actual_type {
            quote! { #ident: &#ty }
        } else {
            quote! { #ident: &[u8; 32] }
        }
    }).collect()
}

/// Collect the unique PDA account seed parameters for a given instruction as typed
/// `__account_seed_<name>: &[u8; 32]` token streams.
fn pda_account_seed_params(ix: &InstructionInfo) -> Vec<TokenStream2> {
    let mut names: Vec<String> = Vec::new();
    for acc in &ix.accounts {
        for seed in &acc.constraints.pda_seeds {
            if let PdaSeedDef::Account(path) = seed {
                let name = path.split('.').next().unwrap_or(path.as_str()).to_string();
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
    }
    names.iter().map(|name| {
        let ident = format_ident!("__account_seed_{}", name);
        quote! { #ident: &[u8; 32] }
    }).collect()
}

fn generate_claim_fns(instructions: &[InstructionInfo]) -> Vec<TokenStream2> {
    instructions
        .iter()
        .map(|ix| {
            let fn_name = format_ident!("__claims_{}", ix.fn_name);
            let has_rest = ix.accounts.iter().any(|a| a.is_rest);
            let arg_params = pda_arg_params(ix);
            let account_seed_params = pda_account_seed_params(ix);
            let all_params: Vec<TokenStream2> = arg_params.into_iter()
                .chain(account_seed_params)
                .collect();

            if has_rest {
                let fixed_claims: Vec<TokenStream2> = ix
                    .accounts
                    .iter()
                    .filter(|a| !a.is_rest)
                    .map(|acc| generate_single_claim_expr(acc))
                    .collect();

                let rest_acc = ix.accounts.iter().find(|a| a.is_rest).unwrap();
                let rest_claim = generate_single_claim_expr(rest_acc);

                quote! {
                    #[allow(dead_code)]
                    pub fn #fn_name(rest_count: usize, #(#all_params),*) -> Vec<spel_framework::spel_output::AutoClaim> {
                        let mut claims = vec![#(#fixed_claims),*];
                        claims.extend(
                            std::iter::repeat(#rest_claim).take(rest_count)
                        );
                        claims
                    }
                }
            } else {
                let claim_exprs: Vec<TokenStream2> = ix
                    .accounts
                    .iter()
                    .map(|acc| generate_single_claim_expr(acc))
                    .collect();

                quote! {
                    #[allow(dead_code)]
                    pub fn #fn_name(#(#all_params),*) -> Vec<spel_framework::spel_output::AutoClaim> {
                        vec![#(#claim_exprs),*]
                    }
                }
            }
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
                            return Err(spel_framework::error::SpelError::Unauthorized {
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
                .map(|(i, _acc)| {
                    let idx = i;
                    quote! {
                        if accounts[#idx].account != nssa_core::account::Account::default() {
                            return Err(spel_framework::error::SpelError::AccountAlreadyInitialized {
                                account_index: #idx,
                            });
                        }
                    }
                })
                .collect();

            // Generate owner checks for accounts with #[account(owner = expr)]
            let owner_checks: Vec<TokenStream2> = ix
                .accounts
                .iter()
                .enumerate()
                .filter(|(_, acc)| acc.constraints.owner.is_some())
                .map(|(i, acc)| {
                    let idx = i;
                    let acc_name = acc.name.to_string();
                    let owner_expr = acc.constraints.owner.as_ref().unwrap();
                    // self_program_id is passed as &ProgramId; deref for comparison.
                    quote! {
                        if accounts[#idx].account.program_owner != *#owner_expr {
                            return Err(spel_framework::error::SpelError::AccountOwnerMismatch {
                                account_name: #acc_name.to_string(),
                            });
                        }
                    }
                })
                .collect();

            // Extra parameters for arg PDA seeds
            let arg_seed_params = pda_arg_params(ix);

            // Extra parameters for private PDA npk args: __npk_arg_<name>: &nssa_core::NullifierPublicKey
            let npk_params: Vec<TokenStream2> = {
                let mut seen: Vec<String> = Vec::new();
                let mut params = Vec::new();
                for acc in &ix.accounts {
                    if let Some(ref npk) = acc.constraints.npk_arg {
                        if !seen.contains(npk) {
                            seen.push(npk.clone());
                            let ident = format_ident!("__npk_arg_{}", npk);
                            params.push(quote! { #ident: &nssa_core::NullifierPublicKey });
                        }
                    }
                }
                params
            };

            // Generate PDA checks for accounts with pda_seeds
            let pda_checks: Vec<TokenStream2> = ix
                .accounts
                .iter()
                .enumerate()
                .filter(|(_, acc)| !acc.constraints.pda_seeds.is_empty())
                .map(|(i, acc)| {
                    let acc_name = acc.name.to_string();
                    let idx = i;

                    let seed_exprs: Vec<TokenStream2> = acc
                        .constraints
                        .pda_seeds
                        .iter()
                        .enumerate()
                        .map(|(j, seed)| {
                            let var = format_ident!("__seed_{}", j);
                            match seed {
                                PdaSeedDef::Const(val) => {
                                    quote! { let #var = spel_framework::pda::seed_from_str(#val); }
                                }
                                PdaSeedDef::Account(path) => {
                                    // Strip ".id" or other suffixes — we always use account_id
                                    let account_name = path.split('.').next().unwrap_or(path);
                                    let account_idx = ix.accounts.iter()
                                        .position(|a| a.name == account_name)
                                        .unwrap_or_else(|| panic!(
                                            "PDA seed references unknown account '{}'", account_name
                                        ));
                                    quote! { let #var = *accounts[#account_idx].account_id.value(); }
                                }
                                PdaSeedDef::Arg(field_name) => {
                                    let param_name = format_ident!("__pda_arg_{}", field_name);
                                    quote! { let #var = spel_framework::pda::ToSeed::to_seed(#param_name); }
                                }
                            }
                        })
                        .collect();

                    let seed_refs: Vec<TokenStream2> = (0..acc.constraints.pda_seeds.len())
                        .map(|j| {
                            let var = format_ident!("__seed_{}", j);
                            quote! { &#var }
                        })
                        .collect();

                    if acc.constraints.private_pda {
                        // Private PDA: address = for_private_pda(program_id, seed, npk)
                        let npk_name = acc.constraints.npk_arg.as_deref()
                            .expect("private_pda without npk_arg — should have been caught in parse_account_constraints");
                        let npk_param = format_ident!("__npk_arg_{}", npk_name);
                        quote! {
                            {
                                #(#seed_exprs)*
                                let __expected_id = spel_framework::pda::compute_private_pda(
                                    self_program_id, &[#(#seed_refs),*], #npk_param
                                );
                                if accounts[#idx].account_id != __expected_id {
                                    return Err(spel_framework::error::SpelError::PdaMismatch {
                                        account_name: #acc_name.to_string(),
                                        expected: format!("{:?}", __expected_id),
                                        actual: format!("{:?}", accounts[#idx].account_id),
                                    });
                                }
                            }
                        }
                    } else {
                        quote! {
                            {
                                #(#seed_exprs)*
                                let __expected_id = spel_framework::pda::compute_pda(
                                    self_program_id, &[#(#seed_refs),*]
                                );
                                if accounts[#idx].account_id != __expected_id {
                                    return Err(spel_framework::error::SpelError::PdaMismatch {
                                        account_name: #acc_name.to_string(),
                                        expected: format!("{:?}", __expected_id),
                                        actual: format!("{:?}", accounts[#idx].account_id),
                                    });
                                }
                            }
                        }
                    }
                })
                .collect();

            if signer_checks.is_empty() && init_checks.is_empty() && pda_checks.is_empty() && owner_checks.is_empty() {
                return quote! {};
            }

            // Combine all extra params: arg seeds first, then npk params
            let all_validate_params: Vec<TokenStream2> = arg_seed_params.into_iter()
                .chain(npk_params)
                .collect();

            quote! {
                #[allow(dead_code)]
                pub fn #fn_name(
                    accounts: &[nssa_core::account::AccountWithMetadata],
                    self_program_id: &nssa_core::program::ProgramId,
                    // Retained for future use (e.g. instruction-level replay protection or
                    // content-based dispatch). Not used in validation logic today.
                    _instruction_words: &nssa_core::program::InstructionData,
                    #(#all_validate_params),*
                ) -> Result<(), spel_framework::error::SpelError> {
                    // Owner checks first — fail fast if account isn't owned by this program.
                    #(#owner_checks)*
                    #(#signer_checks)*
                    #(#init_checks)*
                    #(#pda_checks)*
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

/// Convert a Rust `syn::Type` to a `TokenStream` that constructs the correct `IdlType` variant.
/// Used by `generate_idl_fn` to emit structured types (Array, Vec, Option) instead of
/// flattening everything to `IdlType::Primitive(string)`.
fn rust_type_to_idl_type_tokens(ty: &Type) -> proc_macro2::TokenStream {
    match ty {
        Type::Path(type_path) => {
            let segment = type_path.path.segments.last().unwrap();
            let ident = segment.ident.to_string();
            match ident.as_str() {
                "u8" | "u16" | "u32" | "u64" | "u128" | "i8" | "i16" | "i32" | "i64"
                | "i128" | "bool" | "String" => {
                    let name = ident.to_lowercase();
                    quote! { spel_framework::idl::IdlType::Primitive(#name.to_string()) }
                }
                "Vec" => {
                    if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                        if let Some(syn::GenericArgument::Type(inner)) = args.args.first() {
                            let inner_tokens = rust_type_to_idl_type_tokens(inner);
                            quote! {
                                spel_framework::idl::IdlType::Vec {
                                    vec: Box::new(#inner_tokens)
                                }
                            }
                        } else {
                            quote! { spel_framework::idl::IdlType::Primitive("vec<unknown>".to_string()) }
                        }
                    } else {
                        quote! { spel_framework::idl::IdlType::Primitive("vec<unknown>".to_string()) }
                    }
                }
                "Option" => {
                    if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                        if let Some(syn::GenericArgument::Type(inner)) = args.args.first() {
                            let inner_tokens = rust_type_to_idl_type_tokens(inner);
                            quote! {
                                spel_framework::idl::IdlType::Option {
                                    option: Box::new(#inner_tokens)
                                }
                            }
                        } else {
                            quote! { spel_framework::idl::IdlType::Primitive("option<unknown>".to_string()) }
                        }
                    } else {
                        quote! { spel_framework::idl::IdlType::Primitive("option<unknown>".to_string()) }
                    }
                }
                "ProgramId" => {
                    quote! { spel_framework::idl::IdlType::Primitive("program_id".to_string()) }
                }
                "AccountId" => {
                    quote! { spel_framework::idl::IdlType::Primitive("account_id".to_string()) }
                }
                other => {
                    let name = other.to_string();
                    quote! { spel_framework::idl::IdlType::Defined { defined: #name.to_string() } }
                }
            }
        }
        Type::Array(arr) => {
            let elem_tokens = rust_type_to_idl_type_tokens(&arr.elem);
            if let syn::Expr::Lit(lit) = &arr.len {
                if let syn::Lit::Int(n) = &lit.lit {
                    let size: usize = n.base10_parse().unwrap_or(0);
                    quote! {
                        spel_framework::idl::IdlType::Array {
                            array: (Box::new(#elem_tokens), #size)
                        }
                    }
                } else {
                    quote! { spel_framework::idl::IdlType::Primitive("unknown".to_string()) }
                }
            } else {
                quote! { spel_framework::idl::IdlType::Primitive("unknown".to_string()) }
            }
        }
        _ => {
            quote! { spel_framework::idl::IdlType::Primitive("unknown".to_string()) }
        }
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
                "AccountId" => "\"account_id\"".to_string(),
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

/// Compute SHA256("global:{name}")[..8] discriminator at macro expansion time.
fn compute_discriminator(name: &str) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(format!("global:{}", name).as_bytes());
    let result = hasher.finalize();
    result[..8].to_vec()
}

fn generate_idl_fn(mod_name: &Ident, instructions: &[InstructionInfo], external_instruction: Option<&str>, accounts: Vec<spel_framework_core::idl::IdlAccountType>, types: Vec<spel_framework_core::idl::IdlTypeDef>) -> TokenStream2 {
    let program_name = mod_name.to_string();

    // Serialize accounts and types to JSON for embedding in generated code
    let accounts_json = serde_json::to_string(&accounts).unwrap_or_else(|err| panic!("failed to serialize IDL accounts to JSON during macro expansion: {err}"));
    let types_json = serde_json::to_string(&types).unwrap_or_else(|err| panic!("failed to serialize IDL types to JSON during macro expansion: {err}"));

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
                                    spel_framework::idl::IdlSeed::Const { value: #val.to_string() }
                                },
                                PdaSeedDef::Account(name) => quote! {
                                    spel_framework::idl::IdlSeed::Account { path: #name.to_string() }
                                },
                                PdaSeedDef::Arg(name) => quote! {
                                    spel_framework::idl::IdlSeed::Arg { path: #name.to_string() }
                                },
                            })
                            .collect();
                        let is_private = acc.constraints.private_pda;

                        quote! {
                            Some(spel_framework::idl::IdlPda {
                                seeds: vec![#(#seed_literals),*],
                                private: #is_private,
                            })
                        }
                    };

                    let is_rest = acc.is_rest;
                    let visibility_tags: Vec<TokenStream2> = if acc.constraints.private_pda {
                        vec![quote! { "private".to_string() }]
                    } else {
                        vec![quote! { "public".to_string() }]
                    };
                    // Owner constraint in IDL.
                    let owner_literal = if let Some(ref owner) = acc.constraints.owner {
                        if let syn::Expr::Path(ep) = owner {
                            if let Some(seg) = ep.path.segments.last() {
                                if seg.ident == "self_program_id" {
                                    quote! { Some("self_program_id".to_string()) }
                                } else {
                                    let s = format!("{}", quote!(#owner));
                                    quote! { Some(#s.to_string()) }
                                }
                            } else {
                                quote! { None }
                            }
                        } else {
                            let s = format!("{}", quote!(#owner));
                            quote! { Some(#s.to_string()) }
                        }
                    } else {
                        quote! { None }
                    };

                    quote! {
                        spel_framework::idl::IdlAccountItem {
                            name: #acc_name.to_string(),
                            writable: #writable,
                            signer: #signer,
                            init: #init,
                            owner: #owner_literal,
                            pda: #pda_expr,
                            rest: #is_rest,
                            visibility: vec![#(#visibility_tags),*],
                        }
                    }
                })
                .collect();

            let arg_literals: Vec<TokenStream2> = ix
                .args
                .iter()
                .map(|arg| {
                    let arg_name = arg.name.to_string().trim_start_matches('_').to_string();
                    let type_tokens = rust_type_to_idl_type_tokens(&arg.ty);
                    quote! {
                        spel_framework::idl::IdlArg {
                            name: #arg_name.to_string(),
                            type_: #type_tokens,
                        }
                    }
                })
                .collect();

            let discriminator_bytes = compute_discriminator(&ix_name);
            let disc_bytes_lit: Vec<proc_macro2::TokenStream> = discriminator_bytes.iter()
                .map(|b| { let val = proc_macro2::Literal::u8_unsuffixed(*b); quote! { #val } })
                .collect();
            let variant_name_str = {
                let s = &ix_name;
                s.split('_')
                    .map(|w| {
                        let mut c = w.chars();
                        match c.next() {
                            None => String::new(),
                            Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                        }
                    })
                    .collect::<String>()
            };

            quote! {
                spel_framework::idl::IdlInstruction {
                    name: #ix_name.to_string(),
                    accounts: vec![#(#account_literals),*],
                    args: vec![#(#arg_literals),*],
                    discriminator: Some(vec![#(#disc_bytes_lit),*]),
                    execution: Some(spel_framework::idl::IdlExecution {
                        public: true,
                        private_owned: false,
                    }),
                    variant: Some(#variant_name_str.to_string()),
                }
            }
        })
        .collect();

    // Compute instruction_type at proc-macro expansion time
    let instruction_type_expr = if let Some(ext) = external_instruction {
        quote! { Some(#ext.to_string()) }
    } else {
        quote! { None }
    };

    quote! {
        #[allow(dead_code)]
        pub fn __program_idl() -> spel_framework::idl::SpelIdl {
            let accounts: Vec<spel_framework::idl::IdlAccountType> = spel_framework::serde_json::from_str(#accounts_json).expect("accounts JSON is valid");
            let types: Vec<spel_framework::idl::IdlTypeDef> = spel_framework::serde_json::from_str(#types_json).expect("types JSON is valid");
            spel_framework::idl::SpelIdl {
                version: "0.1.0".to_string(),
                name: #program_name.to_string(),
                instructions: vec![#(#instruction_literals),*],
                accounts,
                types,
                errors: vec![],
                spec: Some("0.1.0".to_string()),
                instruction_type: #instruction_type_expr,
                metadata: Some(spel_framework::idl::IdlMetadata {
                    name: #program_name.to_string(),
                    version: "0.1.0".to_string(),
                }),
            }
        }
    }
}

// ─── IDL generation (JSON string, for PROGRAM_IDL_JSON const) ────────────

fn generate_idl_json(mod_name: &Ident, instructions: &[InstructionInfo], external_instruction: Option<&str>, accounts: Vec<spel_framework_core::idl::IdlAccountType>, types: Vec<spel_framework_core::idl::IdlTypeDef>) -> String {
    let program_name = mod_name.to_string();

    // Serialize accounts and types to JSON
    let accounts_json_str = serde_json::to_string(&accounts).unwrap_or_else(|err| panic!("failed to serialize IDL accounts to JSON during macro expansion: {err}"));
    let types_json_str = serde_json::to_string(&types).unwrap_or_else(|err| panic!("failed to serialize IDL types to JSON during macro expansion: {err}"));

    let instructions_json: Vec<String> = instructions
        .iter()
        .map(|ix| {
            let ix_name = &ix.fn_name.to_string();

            let accounts_json: Vec<String> = ix
                .accounts
                .iter()
                .map(|acc| {
                    let name = acc.name.to_string().trim_start_matches('_').to_string();
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
                        if acc.constraints.private_pda {
                            format!(",\"pda\":{{\"seeds\":[{}],\"private\":true}}", seeds.join(","))
                        } else {
                            format!(",\"pda\":{{\"seeds\":[{}]}}", seeds.join(","))
                        }
                    };

                    let visibility_json = if acc.constraints.private_pda {
                        ",\"visibility\":[\"private\"]".to_string()
                    } else {
                        String::new()
                    };
                    let rest_json = if acc.is_rest { ",\"rest\":true".to_string() } else { String::new() };
                    format!(
                        "{{\"name\":\"{}\",\"writable\":{},\"signer\":{},\"init\":{}{}{}{}}}",
                        name, writable, signer, init, pda_json, rest_json, visibility_json
                    )
                })
                .collect();

            let args_json: Vec<String> = ix
                .args
                .iter()
                .map(|arg| {
                    let name = arg.name.to_string().trim_start_matches('_').to_string();
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

    let instruction_type_suffix = if let Some(ext) = external_instruction {
        format!(",\"instruction_type\":\"{}\"", ext)
    } else {
        String::new()
    };
    format!(
        "{{\"version\":\"0.1.0\",\"name\":\"{}\",\"instructions\":[{}],\"accounts\":{},\"types\":{},\"errors\":[]{}}}",
        program_name,
        instructions_json.join(","),
        accounts_json_str,
        types_json_str,
        instruction_type_suffix
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

    // Find the #[lez_program] module
    let mut program_mod: Option<&ItemMod> = None;
    for item in &file.items {
        if let syn::Item::Mod(m) = item {
            if m.attrs.iter().any(|a| a.path().is_ident("lez_program")) {
                program_mod = Some(m);
                break;
            }
        }
    }

    let program_mod = program_mod.ok_or_else(|| {
        syn::Error::new_spanned(
            span_token,
            format!(
                "No #[lez_program] module found in '{}'",
                file_path
            ),
        )
    })?;

    let mod_name = &program_mod.ident;

    let (_, items) = program_mod.content.as_ref().ok_or_else(|| {
        syn::Error::new_spanned(span_token, "lez_program module has no body")
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

    // Detect external instruction type from the #[lez_program(...)] attr
    let external_instruction_str: Option<String> = program_mod.attrs.iter()
        .find(|a| a.path().is_ident("lez_program"))
        .and_then(|attr| {
            // Try to parse as lez_program(instruction = "some::Path")
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

    // Collect #[account_type] annotated types: search both the file's top-level
    // items and the items inside the #[lez_program] module body, since user code
    // commonly defines account structs inside the program module.
    let mut all_items: Vec<syn::Item> = file.items.clone();
    all_items.extend(items.clone());

    // Also scan path-dependency crates for #[account_type] types.
    // This handles the common project structure where account types are defined
    // in a shared core crate (e.g. my_program_core) and the program binary
    // depends on it via `path = "..."`.
    let resolved_path_buf = std::path::Path::new(&resolved_path).to_path_buf();
    let dep_dirs =
        spel_framework_core::idl_gen::find_path_dep_dirs(&resolved_path_buf, |_| {});
    let (extra_items, dep_source_files) =
        spel_framework_core::idl_gen::collect_items_from_crate_dirs(&dep_dirs);
    all_items.extend(extra_items);

    let (accounts, types) = account_types::collect_account_types(&all_items);

    // Generate the IDL JSON
    let idl_json = generate_idl_json(mod_name, &instructions, external_instruction_str.as_deref(), accounts, types);

    // Embed the resolved path for cargo tracking
    let resolved = resolved_path.clone();

    // Emit include_str!() for every path-dep source file we read, so cargo
    // tracks them as dependencies.  Without this, changes in a path-dep crate
    // would not trigger macro re-expansion (stale IDL until cargo clean).
    let dep_tracking: Vec<proc_macro2::TokenStream> = dep_source_files
        .iter()
        .filter_map(|p| p.to_str().map(|s| s.to_string()))
        .map(|path| {
            let lit = syn::LitStr::new(&path, proc_macro2::Span::call_site());
            quote! { const _: &str = include_str!(#lit); }
        })
        .collect();

    // Generate a main() that pretty-prints the IDL
    Ok(quote! {
        pub fn main() {
            // Help cargo track source changes
            const _SOURCE: &str = include_str!(#resolved);
            #(#dep_tracking)*
            let json: spel_framework::serde_json::Value = spel_framework::serde_json::from_str(#idl_json)
                .expect("Generated IDL JSON is invalid");
            println!("{}", spel_framework::serde_json::to_string_pretty(&json).unwrap());
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Self-cleaning temporary directory.
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("spel-macro-test-{}-{}", label, n));
            std::fs::create_dir_all(&path).unwrap();
            TempDir(path)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }

        fn write(&self, rel: &str, content: &str) -> std::path::PathBuf {
            let p = self.0.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, content).unwrap();
            p
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    // ── has_account_type_attr (qualified form) ─────────────────────────────

    #[test]
    fn has_account_type_attr_matches_bare_form() {
        let file = syn::parse_file(
            "#[account_type]\npub struct Vault { pub balance: u64 }",
        )
        .unwrap();
        if let syn::Item::Struct(s) = &file.items[0] {
            assert!(account_types::has_account_type_attr(&s.attrs));
        } else {
            panic!("expected struct");
        }
    }

    #[test]
    fn has_account_type_attr_matches_qualified_form() {
        let file = syn::parse_file(
            "#[spel_framework_macros::account_type]\npub struct Vault { pub balance: u64 }",
        )
        .unwrap();
        if let syn::Item::Struct(s) = &file.items[0] {
            assert!(account_types::has_account_type_attr(&s.attrs));
        } else {
            panic!("expected struct");
        }
    }

    #[test]
    fn has_account_type_attr_matches_deeply_qualified_form() {
        let file = syn::parse_file(
            "#[foo::bar::account_type]\npub struct Vault { pub balance: u64 }",
        )
        .unwrap();
        if let syn::Item::Struct(s) = &file.items[0] {
            assert!(account_types::has_account_type_attr(&s.attrs));
        } else {
            panic!("expected struct");
        }
    }

    #[test]
    fn has_account_type_attr_rejects_other_attrs() {
        let file = syn::parse_file(
            "#[derive(Debug)]\npub struct Vault { pub balance: u64 }",
        )
        .unwrap();
        if let syn::Item::Struct(s) = &file.items[0] {
            assert!(!account_types::has_account_type_attr(&s.attrs));
        } else {
            panic!("expected struct");
        }
    }

    // ── find_path_dep_dirs (via spel-framework-core) ───────────────────────

    #[test]
    fn find_path_dep_dirs_returns_local_path_deps() {
        let tmp = TempDir::new("find-path-deps-macro");

        tmp.write(
            "core/Cargo.toml",
            "[package]\nname = \"token_core\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        tmp.write("core/src/lib.rs", "");

        tmp.write(
            "methods/guest/Cargo.toml",
            "[package]\nname = \"token-guest\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\ntoken_core = { path = \"../../core\" }\n",
        );
        let program = tmp.write("methods/guest/src/bin/token.rs", "");

        let dirs = spel_framework_core::idl_gen::find_path_dep_dirs(&program, |_| {});
        assert_eq!(dirs.len(), 1);
        assert!(dirs[0].ends_with("core"), "expected core dir, got {:?}", dirs[0]);
    }

    #[test]
    fn find_path_dep_dirs_ignores_registry_and_git_deps() {
        let tmp = TempDir::new("find-path-deps-filter-macro");

        tmp.write(
            "core/Cargo.toml",
            "[package]\nname = \"token_core\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        tmp.write("core/src/lib.rs", "");

        tmp.write(
            "methods/guest/Cargo.toml",
            "[package]\nname = \"token-guest\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\n\
             token_core = { path = \"../../core\" }\n\
             serde = { version = \"1.0\" }\n\
             nssa_core = { git = \"https://example.com/repo.git\", tag = \"v1.0\" }\n",
        );
        let program = tmp.write("methods/guest/src/bin/token.rs", "");

        let dirs = spel_framework_core::idl_gen::find_path_dep_dirs(&program, |_| {});
        assert_eq!(dirs.len(), 1);
        assert!(dirs[0].ends_with("core"));
    }

    #[test]
    fn find_path_dep_dirs_ignores_dev_and_build_deps() {
        let tmp = TempDir::new("find-path-deps-dev-build-macro");

        tmp.write(
            "core/Cargo.toml",
            "[package]\nname = \"token_core\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        tmp.write("core/src/lib.rs", "");
        tmp.write(
            "test_helpers/Cargo.toml",
            "[package]\nname = \"test_helpers\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        tmp.write("test_helpers/src/lib.rs", "");

        tmp.write(
            "methods/guest/Cargo.toml",
            "[package]\nname = \"token-guest\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\n\
             token_core = { path = \"../../core\" }\n\n\
             [dev-dependencies]\n\
             test_helpers = { path = \"../../test_helpers\" }\n",
        );
        let program = tmp.write("methods/guest/src/bin/token.rs", "");

        let dirs = spel_framework_core::idl_gen::find_path_dep_dirs(&program, |_| {});
        assert_eq!(dirs.len(), 1, "expected only core, got: {dirs:?}");
        assert!(dirs[0].ends_with("core"));
    }

    #[test]
    fn find_path_dep_dirs_resolves_transitive_deps() {
        let tmp = TempDir::new("transitive-deps-macro");

        // shared_types -> core -> guest
        tmp.write(
            "shared/Cargo.toml",
            "[package]\nname = \"shared_types\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        tmp.write("shared/src/lib.rs", "");

        tmp.write(
            "core/Cargo.toml",
            "[package]\nname = \"token_core\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\nshared_types = { path = \"../shared\" }\n",
        );
        tmp.write("core/src/lib.rs", "");

        tmp.write(
            "methods/guest/Cargo.toml",
            "[package]\nname = \"token-guest\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\ntoken_core = { path = \"../../core\" }\n",
        );
        let program = tmp.write("methods/guest/src/bin/token.rs", "");

        let dirs = spel_framework_core::idl_gen::find_path_dep_dirs(&program, |_| {});
        assert_eq!(dirs.len(), 2, "expected core and shared, got: {dirs:?}");
        let names: Vec<&str> = dirs.iter().map(|d| d.file_name().unwrap().to_str().unwrap()).collect();
        assert!(names.contains(&"core"));
        assert!(names.contains(&"shared"));
    }

    // ── expand_generate_idl with path deps ─────────────────────────────────

    /// End-to-end test: generate_idl! macro collects #[account_type] types from
    /// a path-dependency crate.
    #[test]
    fn generate_idl_collects_account_types_from_path_dep() {
        let tmp = TempDir::new("generate-idl-path-dep");

        // Core crate with account types
        tmp.write(
            "core/Cargo.toml",
            "[package]\nname = \"token_core\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        tmp.write(
            "core/src/lib.rs",
            r#"
#[account_type]
pub struct TokenHolding {
    pub balance: u128,
}

#[account_type]
pub enum TokenDefinition {
    Fungible { name: String },
}
"#,
        );

        // Guest crate depending on core
        tmp.write(
            "methods/guest/Cargo.toml",
            "[package]\nname = \"token-guest\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\ntoken_core = { path = \"../../core\" }\n",
        );
        let program = tmp.write(
            "methods/guest/src/bin/token.rs",
            r#"
#[lez_program]
pub mod token {
    #[instruction]
    pub fn transfer(
        sender: AccountWithMetadata,
        recipient: AccountWithMetadata,
        amount: u128,
    ) -> SpelResult { todo!() }
}
"#,
        );

        // Run expand_generate_idl
        let tokens = expand_generate_idl(
            program.to_str().unwrap(),
            &syn::LitStr::new("test", proc_macro2::Span::call_site()),
        )
        .unwrap();

        // The generated code should contain main() with IDL that includes the account types.
        let output = tokens.to_string();
        assert!(
            output.contains("TokenHolding"),
            "TokenHolding from path dep not found in generated IDL. Output: {}", output
        );
        assert!(
            output.contains("TokenDefinition"),
            "TokenDefinition from path dep not found in generated IDL. Output: {}", output
        );
    }

    /// Account types using the fully-qualified #[spel_framework_macros::account_type]
    /// form should also be collected from path-dep crates.
    #[test]
    fn generate_idl_collects_qualified_account_type_from_path_dep() {
        let tmp = TempDir::new("generate-idl-qualified");

        // Core crate with account types using fully-qualified attribute
        tmp.write(
            "core/Cargo.toml",
            "[package]\nname = \"token_core\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        tmp.write(
            "core/src/lib.rs",
            r#"
#[spel_framework_macros::account_type]
pub struct VaultConfig {
    pub owner: String,
}
"#,
        );

        // Guest crate depending on core
        tmp.write(
            "methods/guest/Cargo.toml",
            "[package]\nname = \"token-guest\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\ntoken_core = { path = \"../../core\" }\n",
        );
        let program = tmp.write(
            "methods/guest/src/bin/token.rs",
            r#"
#[lez_program]
pub mod token {
    #[instruction]
    pub fn init(acc: AccountWithMetadata) -> SpelResult { todo!() }
}
"#,
        );

        let tokens = expand_generate_idl(
            program.to_str().unwrap(),
            &syn::LitStr::new("test", proc_macro2::Span::call_site()),
        )
        .unwrap();

        let output = tokens.to_string();
        assert!(
            output.contains("VaultConfig"),
            "VaultConfig with qualified attribute not found in generated IDL. Output: {}", output
        );
    }
}
