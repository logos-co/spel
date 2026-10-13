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
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use sha2::{Digest, Sha256};
use syn::{
    parse::Parser, parse_macro_input, Attribute, FnArg, Ident, ItemFn, ItemMod, Pat, PatType, Type,
};

mod account_types;
mod slot_offsets;

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
                    if let syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(s),
                        ..
                    }) = &nv.value
                    {
                        config.external_instruction = Some(s.parse()?);
                    } else {
                        return Err(syn::Error::new_spanned(
                            &nv.value,
                            "expected string literal",
                        ));
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

/// Main entry point: `#[lez_program]` on a module.
///
/// This macro:
/// 1. Finds all `#[instruction]` functions in the module
/// 2. Generates a Borsh-serializable `Instruction` enum
/// 3. Generates the `fn main()` with read/dispatch/write boilerplate
/// 4. Generates account validation code per instruction
/// 5. Generates `PROGRAM_IDL_JSON` const with complete IDL (including PDA seeds)
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

/// Marker attribute for instruction functions.
///
/// Inside an `#[lez_program]` module this never expands. The module macro
/// consumes the whole module first and emits the handler functions itself.
///
/// Standalone it does expand, which is the case an extension library hits:
/// its instruction functions live outside `#[lez_program]`. Here the macro
/// strips the `#[account(...)]` helper attributes off the parameters, so
/// rustc accepts a function the framework has not rewritten. Extension
/// discovery is unaffected, because the scanner parses the library source
/// rather than this expansion and still sees the attributes.
#[proc_macro_attribute]
pub fn instruction(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut func = parse_macro_input!(item as ItemFn);
    strip_account_attrs(&mut func);
    quote!(#func).into()
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
/// For the IDL the attribute is a pass-through, consumed by the
/// generator. Structs with a `*_slot` field attribute (e.g.
/// `#[admin_slot]`) additionally gain a derived `<NAME>_OFFSET` const
/// and an emitted layout test. See `slot_offsets`.
#[proc_macro_attribute]
pub fn account_type(_attr: TokenStream, item: TokenStream) -> TokenStream {
    slot_offsets::expand(item)
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
    external_call_path: Option<syn::Path>,
    /// The original function item (with #[instruction] stripped)
    func: ItemFn,
    injected: Vec<String>,
    /// Account param ident in original signature order, duplicates kept.
    /// The dispatch call site needs every position; `accounts` below is
    /// the transaction view, deduped by ident.
    pub call_accounts: Vec<Ident>,
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
    /// True when `private_pda` keyword is present — address includes the caller's npk + vpk.
    private_pda: bool,
    /// Name of the instruction arg supplying the `NullifierPublicKey` for derivation.
    npk_arg: Option<String>,
    /// Name of the instruction arg supplying the `ViewingPublicKey` for derivation.
    vpk_arg: Option<String>,
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

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").map_err(|e| {
        syn::Error::new_spanned(&input.ident, format!("CARGO_MANIFEST_DIR not set: {e}"))
    })?;
    let manifest_dir = std::path::PathBuf::from(manifest_dir);
    let mut deps = spel_framework_core::extension::resolve_program_deps(
        &manifest_dir,
        &input.attrs,
        &mut |_| {},
    )
    .map_err(|msg| syn::Error::new(proc_macro2::Span::call_site(), msg))?;
    let mut slot_assert = proc_macro2::TokenStream::new();
    let bound_calls = deps.extensions.bound_calls.clone();
    let consumer_fns: Vec<ItemFn> = items
        .iter()
        .filter_map(|i| match i {
            syn::Item::Fn(f) if has_instruction_attr(&f.attrs) => Some(f.clone()),
            _ => None,
        })
        .collect();
    spel_framework_core::extension::rewrite_embedded_roles(
        &mut deps.extensions.inject_specs,
        &deps.extensions.embeds,
        &consumer_fns,
    )
    .map_err(|msg| syn::Error::new(proc_macro2::Span::call_site(), msg))?;

    let active_wraps: Vec<_> = spel_framework_core::extension::active_wraps(&deps.extensions.wraps);

    // Collect instruction functions and other items
    let mut instructions: Vec<InstructionInfo> = Vec::new();
    let mut other_items: Vec<TokenStream2> = Vec::new();

    for item in items {
        match item {
            syn::Item::Fn(func) => {
                if has_instruction_attr(&func.attrs) {
                    let mut func = func.clone();
                    let injected = spel_framework_core::extension::apply_wrap_and_inject(
                        &mut func,
                        &active_wraps,
                        &deps.extensions.inject_specs,
                        &deps.extensions.embeds,
                        None,
                    )
                    .map_err(|msg| syn::Error::new(proc_macro2::Span::call_site(), msg))?;
                    let mut info = parse_instruction(func)?;
                    info.injected = injected;
                    instructions.push(info);
                } else {
                    other_items.push(quote! { #func });
                }
            },
            other => {
                other_items.push(quote! { #other });
            },
        }
    }

    if instructions.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "lez_program must contain at least one #[instruction] function",
        ));
    }

    for (func, crate_path) in deps.extensions.instructions {
        let mut func = func;
        let qualified = format!(
            "{}::{}",
            crate_path
                .segments
                .first()
                .map(|s| s.ident.to_string())
                .unwrap_or_default(),
            func.sig.ident
        );
        spel_framework_core::extension::apply_wrap_and_inject(
            &mut func,
            &active_wraps,
            &deps.extensions.inject_specs,
            &deps.extensions.embeds,
            Some(&qualified),
        )
        .map_err(|msg| syn::Error::new(proc_macro2::Span::call_site(), msg))?;
        let mut info = parse_instruction(func)?;
        let name = &info.fn_name;
        info.external_call_path = Some(syn::parse_quote!(#crate_path::#name));
        instructions.push(info);
    }
    spel_framework_core::extension::check_duplicate_instruction_names(instructions.iter().map(
        |i| {
            (
                i.fn_name.to_string(),
                spel_framework_core::extension::instruction_source_label(
                    i.external_call_path.as_ref(),
                ),
            )
        },
    ))
    .map_err(|msg| syn::Error::new(proc_macro2::Span::call_site(), msg))?;

    // Generate the Instruction enum (or use external one)
    let enum_def = if let Some(path) = config.external_instruction.as_ref() {
        // External instruction: import it as `Instruction` if it's not already named that
        quote! {
            use #path as Instruction;
        }
    } else {
        let enum_variants = generate_enum_variants(&instructions);
        quote! {
            // Borsh is the instruction wire format since LEZ v0.2.5
            // (`read_lee_call` deserializes it), and deliberately the only encoding
            // derived here. Deriving serde as well would make this a type that accepts
            // either encoder, so a stale `risc0_zkvm::serde::to_vec(&instruction)` still
            // compiles and still produces bytes the runtime rejects -- silently, since
            // nothing in the signature distinguishes the wire format from tooling. With
            // no serde impl that is a compile error. Nothing consumed it: `spel-cli`
            // encodes Borsh straight from the IDL (`serialize_to_borsh`), IDL generation
            // parses guest source, and the runtime IDL path deserializes the framework's
            // own `IdlAccountType`/`IdlTypeDef`. Dropping it also means a guest no longer
            // needs a direct `serde` dependency just to satisfy a derive.
            //
            // Borsh encodes the variant as a leading tag byte, so variants are
            // append-only: inserting one shifts every existing encoding.
            #[derive(
                Debug,
                Clone,
                spel_framework::borsh::BorshSerialize,
                spel_framework::borsh::BorshDeserialize,
            )]
            // borsh's derive resolves `borsh` from the consumer's Cargo.toml, so point it
            // at the framework's re-export — a program needs no borsh dependency of its own,
            // and cannot drift to a version lee_core doesn't speak.
            #[borsh(crate = "spel_framework::borsh")]
            pub enum Instruction {
                #(#enum_variants),*
            }
        }
    };

    // Generate match arms for dispatch
    let match_arms = generate_match_arms(mod_name, &instructions, &bound_calls);

    // Generate the handler functions (with #[instruction] stripped, account attrs stripped)
    let handler_fns = generate_handler_fns(&instructions);

    // Generate validation functions
    let validation_fns = generate_validation(&instructions);

    // Generate main function.
    // `pub fn main` (not just `fn main`) is required so the zkVM linker can find the entry point
    // when this crate is compiled as a guest binary dependency.
    let main_fn = quote! {
        pub fn main() {
            // Read this invocation from the zkVM host.
            //
            // A call kind this build doesn't implement is a no-op, not a rejection:
            // `respond_unsupported_call` emits the diagnostic event and exits, so a
            // program deployed today survives a call kind introduced later.
            let __call = nssa_core::program::read_lee_call::<Instruction>();
            let nssa_core::program::ProgramCall::Execute(
                nssa_core::program::ProgramInput {
                    self_account_id,
                    caller_account_id,
                    pre_states,
                    instruction,
                },
                instruction_data,
            ) = __call else {
                nssa_core::program::respond_unsupported_call(__call);
            };

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
            let state_diffs = parts.state_diffs;
            let chained_calls = parts.chained_calls;
            let block_validity_window = parts.block_validity_window;
            let timestamp_validity_window = parts.timestamp_validity_window;

            // Every declared account is reported, untouched ones included.
            //
            // Up to v0.2.4 this dropped any (pre, post) pair for an account the program
            // neither owned nor claimed, to satisfy a `validate_execution` rule that
            // rejected a non-default account returned still default-owned. v0.2.5 removed
            // that rule and added its opposite: an account the transaction declared but
            // the output omits fails with `DeclaredAccountMissingFromOutput`. So the
            // handler's diffs go out as-is — an unchanged account belongs in the output as
            // `AccountStateDiff::unchanged`, never by being filtered away.

            // Write outputs to zkVM host
            nssa_core::program::ProgramOutput::new(
                self_account_id,
                caller_account_id,
                instruction_data,
                state_diffs,
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

    // The guest file's own items, kept to look up an external instruction enum.
    let mut local_items: Vec<syn::Item> = Vec::new();

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
                parsed_file.items.iter().any(
                    |item| matches!(item, syn::Item::Mod(item_mod) if item_mod.ident == *mod_name),
                )
            };

            let mut candidate_paths: Vec<std::path::PathBuf> = vec![
                manifest.join("src/bin").join(format!("{module_path}.rs")), // src/bin/{name}.rs
                manifest.join("src/bin").join(&module_path).join("main.rs"), // src/bin/{name}/main.rs
                manifest.join("src").join(format!("{module_path}.rs")),      // src/{name}.rs
                manifest.join("src").join(&module_path).join("mod.rs"),      // src/{name}/mod.rs
                manifest.join("src").join("lib.rs"),                         // src/lib.rs
                manifest.join("src").join("main.rs"),                        // src/main.rs
            ];

            // Scan src/bin/ for additional files and subdirectories
            let src_bin = manifest.join("src/bin");
            if let Ok(entries) = std::fs::read_dir(&src_bin) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("rs")
                    {
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
                    if path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("rs")
                    {
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

            // Manifest 'declared bin paths: entry files outside src/
            // (custom [[bin]] path, test harnesses) must not be missed,
            // a missed entry file silently skips the slot assert.
            for bin_path in spel_framework_core::idl_gen::manifest_bin_paths(manifest) {
                if bin_path.is_file() && !candidate_paths.iter().any(|p| p == &bin_path) {
                    candidate_paths.push(bin_path);
                }
            }

            let mut module_source_found = false;
            for guest_path in &candidate_paths {
                if let Ok(content_str) = std::fs::read_to_string(guest_path) {
                    if let Ok(parsed_file) = syn::parse_file(&content_str) {
                        if file_matches_module(&parsed_file) {
                            module_source_found = true;
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
                            local_items = all_items.clone();
                            // Also include items from path-dependency crates, so types defined in
                            // extension libraries (account types, instruction-arg types) reach the IDL.
                            let (extra_items, _) =
                                spel_framework_core::idl_gen::collect_items_from_crate_dirs(
                                    &deps.graph.transitive_dirs,
                                    |w| eprintln!("warning: {w}"),
                                );
                            all_items.extend(extra_items);
                            slot_assert.extend(slot_offsets::emit_agreement_asserts(
                                guest_path,
                                &deps.extensions.embeds,
                            )?);
                            slot_assert.extend(slot_offsets::embed_window_collision_asserts(
                                &deps.extensions.embeds,
                                &deps.extensions.embed_state_types,
                            )?);
                            result = account_types::collect_account_types(&all_items);
                            break;
                        }
                    }
                }
            }
            if !module_source_found && !deps.extensions.embeds.is_empty() {
                return Err(syn::Error::new(
                    proc_macro2::Span::call_site(),
                    "embedded markers are declared but the module's source \
                    file could not be located, so the slot agreement checks \
                    cannot be emitted; refusing to compile rather than skip \
                    them silently",
                ));
            }
        }
        result
    };

    let idl_instructions = idl_instruction_order(
        &instructions,
        ext_instr_str.as_deref(),
        &local_items,
        &deps.graph.transitive_dirs,
    )?;
    let idl_fn = generate_idl_fn(
        mod_name,
        &idl_instructions,
        ext_instr_str.as_deref(),
        accounts.clone(),
        types.clone(),
    );
    let idl_json = generate_idl_json(
        mod_name,
        &idl_instructions,
        ext_instr_str.as_deref(),
        accounts,
        types,
    );

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

            #slot_assert

            #(#other_items)*

            #(#handler_fns)*

            #(#validation_fns)*
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
            },
            FnArg::Receiver(_) => {
                return Err(syn::Error::new_spanned(
                    input,
                    "instruction functions cannot have self parameter",
                ));
            },
        }
    }

    let call_accounts: Vec<Ident> = accounts.iter().map(|a| a.name.clone()).collect();
    let mut deduped: Vec<AccountParam> = Vec::new();
    for a in accounts {
        match deduped.iter_mut().find(|d| d.name == a.name) {
            Some(kept) => {
                kept.constraints.mutable |= a.constraints.mutable;
                kept.constraints.signer |= a.constraints.signer;
            },
            None => deduped.push(a),
        }
    }
    let accounts = deduped;

    Ok(InstructionInfo {
        fn_name,
        accounts,
        args,
        has_context,
        external_call_path: None,
        func,
        injected: vec![],
        call_accounts,
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
                } else if meta.path.is_ident("vpk") {
                    // vpk = arg("arg_name") — instruction arg supplying the ViewingPublicKey
                    let value = meta.value()?;
                    let expr: syn::Expr = value.parse()?;
                    if let syn::Expr::Call(call) = &expr {
                        if let syn::Expr::Path(path) = &*call.func {
                            if path.path.is_ident("arg") {
                                if let Some(syn::Expr::Lit(lit)) = call.args.first() {
                                    if let syn::Lit::Str(s) = &lit.lit {
                                        constraints.vpk_arg = Some(s.value());
                                        return Ok(());
                                    }
                                }
                            }
                        }
                    }
                    Err(meta.error("vpk must be vpk = arg(\"arg_name\")"))
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
        let Some(npk_name) = constraints.npk_arg.as_deref() else {
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                "`private_pda` requires `npk = arg(\"arg_name\")`",
            ));
        };
        let Some(vpk_name) = constraints.vpk_arg.as_deref() else {
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                "`private_pda` requires `vpk = arg(\"arg_name\")` (LEZ v0.2.1+)",
            ));
        };
        // Validate the npk arg name is a valid Rust identifier
        if syn::parse_str::<Ident>(npk_name).is_err() {
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                format!("`npk` arg name `{npk_name}` is not a valid Rust identifier"),
            ));
        }
        // Validate the vpk arg name is a valid Rust identifier
        if syn::parse_str::<Ident>(vpk_name).is_err() {
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                format!("`vpk` arg name `{vpk_name}` is not a valid Rust identifier"),
            ));
        }
    } else if constraints.npk_arg.is_some() {
        return Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            "`npk` can only be used together with `private_pda`",
        ));
    } else if constraints.vpk_arg.is_some() {
        return Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            "`vpk` can only be used together with `private_pda`",
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
        },
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
        },
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

    let mut args_iter = call.args.iter();
    let (Some(arg), None) = (args_iter.next(), args_iter.next()) else {
        return Err(syn::Error::new_spanned(
            call,
            "PDA seed function takes exactly one string argument",
        ));
    };
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
                "Unknown PDA seed type '{func_name}'. Use const(\"...\"), account(\"...\"), or arg(\"...\")"
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

fn generate_match_arms(
    mod_name: &Ident,
    instructions: &[InstructionInfo],
    bound_calls: &std::collections::HashMap<String, Vec<usize>>,
) -> Vec<TokenStream2> {
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

            let account_destructure = if let Some(rest_account) =
                ix.accounts.iter().find(|a| a.is_rest)
            {
                // Split into fixed accounts + rest
                let fixed_accounts: Vec<&AccountParam> = ix.accounts.iter().filter(|a| !a.is_rest).collect();
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
                // caller_account_id is Option<AccountId> from ProgramInput; a top-level call
                // has no caller, which reads as the default (all-zero) program owner.
                if ix.has_context {
                    args.push(quote! {
                        spel_framework::context::ProgramContext::new(
                            self_account_id,
                            caller_account_id.unwrap_or(nssa_core::program::DEFAULT_PROGRAM_OWNER)
                        )
                    });
                }
                args.extend(ix.call_accounts.iter().enumerate().map(|(i, name)| {
                    let repeats_later = ix
                        .call_accounts
                        .get(i + 1..)
                        .unwrap_or_default()
                        .iter()
                        .any(|n| n == name);
                    if repeats_later {
                        quote! { #name.clone() }
                    } else {
                        quote! { #name }
                    }
                }));
                args.extend(ix.args.iter().map(|a| {
                    let name = &a.name;
                    quote! { #name }
                }));
                if let Some(values) = bound_calls.get(&ix.fn_name.to_string()) {
                    args.extend(values.iter().map(|v| {
                        let lit = proc_macro2::Literal::usize_unsuffixed(*v);
                        quote! { #lit }
                    }));
                }
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

            // Collect vpk arg values for private PDA accounts
            let vpk_arg_values: Vec<TokenStream2> = {
                let mut names = Vec::new();
                for acc in &ix.accounts {
                    if let Some(ref vpk) = acc.constraints.vpk_arg {
                        if !names.contains(vpk) {
                            names.push(vpk.clone());
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
                .chain(vpk_arg_values.iter())
                .cloned()
                .collect();

            let validation_call = if has_validation {
                if let Some(rest_account) = ix.accounts.iter().find(|a| a.is_rest) {
                    // For instructions with Vec accounts, build the slice dynamically
                    let fixed_refs: Vec<TokenStream2> = ix.accounts.iter()
                        .filter(|a| !a.is_rest)
                        .map(|a| { let name = &a.name; quote! { #name.clone() } })
                        .collect();
                    let rest_ref = &rest_account.name;
                    quote! {
                        let mut __all_accounts = vec![#(#fixed_refs),*];
                        __all_accounts.extend(#rest_ref.clone());
                        #mod_name::#validate_fn_name(
                            &__all_accounts,
                            &self_account_id,
                            &instruction_data,
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
                            &self_account_id,
                            &instruction_data,
                            #(#all_extra_args),*
                        ).expect("account validation failed");
                    }
                }
            } else {
                quote! {}
            };
            let call_target = match &ix.external_call_path {
                Some(path) => quote! { #path },
                None => quote! { #mod_name::#fn_name },
            };
            quote! {
                #pattern => {
                    #account_destructure
                    #validation_call
                    #call_target(#(#call_args),*)
                        .map(|output| output.into_parts())
                }
            }
        })
        .collect()
}

/// Drop the `#[account(...)]` helper attributes from a function's parameters.
///
/// The attribute is inert syntax that only the framework reads, so whoever
/// emits the function has to remove it before rustc sees it. Both emitters
/// go through here: `#[lez_program]` when it generates handlers, and the
/// standalone `#[instruction]` expansion.
fn strip_account_attrs(func: &mut ItemFn) {
    for input in &mut func.sig.inputs {
        if let FnArg::Typed(pat_type) = input {
            pat_type.attrs.retain(|a| !a.path().is_ident("account"));
        }
    }
}

/// Emit each handler as an ordinary function.
///
/// Up to LEZ v0.2.4 this also rewrote every `SpelOutput::execute(...)` in the body
/// into `execute_with_claims(...)`, threading the `#[account(...)]`-derived
/// `AutoClaim` values in behind the author's back. v0.2.5 removed claims entirely,
/// so the call the author wrote is the call that runs.
fn generate_handler_fns(instructions: &[InstructionInfo]) -> Vec<TokenStream2> {
    instructions
        .iter()
        .filter(|ix| ix.external_call_path.is_none())
        .map(|ix| {
            let mut func = ix.func.clone();
            func.attrs.retain(|a| !a.path().is_ident("instruction"));
            strip_account_attrs(&mut func);
            quote! { #func }
        })
        .collect()
}

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
    names
        .iter()
        .map(|name| {
            let ident = format_ident!("__pda_arg_{}", name);
            let actual_type = ix
                .args
                .iter()
                .find(|a| a.name == name.as_str())
                .map(|a| &a.ty);
            if let Some(ty) = actual_type {
                quote! { #ident: &#ty }
            } else {
                quote! { #ident: &[u8; 32] }
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
                .filter_map(|(i, acc)| Some((i, acc, acc.constraints.owner.as_ref()?)))
                .map(|(i, acc, owner_expr)| {
                    let idx = i;
                    let acc_name = acc.name.to_string();
                    // self_account_id is passed as &AccountId; deref for comparison.
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

            // Extra parameters for private PDA vpk args: __vpk_arg_<name>: &nssa_core::encryption::ViewingPublicKey
            let vpk_params: Vec<TokenStream2> = {
                let mut seen: Vec<String> = Vec::new();
                let mut params = Vec::new();
                for acc in &ix.accounts {
                    if let Some(ref vpk) = acc.constraints.vpk_arg {
                        if !seen.contains(vpk) {
                            seen.push(vpk.clone());
                            let ident = format_ident!("__vpk_arg_{}", vpk);
                            params.push(quote! { #ident: &nssa_core::encryption::ViewingPublicKey });
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
                                            "PDA seed references unknown account '{account_name}'"
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
                        // Private PDA: address = for_private_pda(program_id, seed, npk, vpk, identifier).
                        // The attribute has no `identifier` constraint yet, so validation uses the
                        // framework default (0); non-zero identifiers need hand-written validation.
                        // parse_account_constraints() rejects private_pda without npk/vpk args
                        // before an AccountParam with private_pda: true can exist here.
                        #[allow(clippy::expect_used)]
                        let npk_name = acc.constraints.npk_arg.as_deref()
                            .expect("private_pda without npk_arg — should have been caught in parse_account_constraints");
                        let npk_param = format_ident!("__npk_arg_{}", npk_name);
                        #[allow(clippy::expect_used)]
                        let vpk_name = acc.constraints.vpk_arg.as_deref()
                            .expect("private_pda without vpk_arg — should have been caught in parse_account_constraints");
                        let vpk_param = format_ident!("__vpk_arg_{}", vpk_name);
                        quote! {
                            {
                                #(#seed_exprs)*
                                let __expected_id = spel_framework::pda::compute_private_pda(
                                    self_account_id, &[#(#seed_refs),*], #npk_param, #vpk_param,
                                    spel_framework::pda::DEFAULT_PRIVATE_PDA_IDENTIFIER,
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
                                    self_account_id, &[#(#seed_refs),*]
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

            // Combine all extra params: arg seeds, then npk params, then vpk params
            let all_validate_params: Vec<TokenStream2> = arg_seed_params.into_iter()
                .chain(npk_params)
                .chain(vpk_params)
                .collect();

            quote! {
                #[allow(dead_code)]
                pub fn #fn_name(
                    accounts: &[nssa_core::account::AccountWithMetadata],
                    self_account_id: &nssa_core::account::AccountId,
                    // Retained for future use (e.g. instruction-level replay protection or
                    // content-based dispatch). Not used in validation logic today.
                    _instruction_data: &nssa_core::program::InstructionData,
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
            // A parsed `syn::Path` always has at least one segment.
            let Some(segment) = type_path.path.segments.last() else {
                return quote! { spel_framework::idl::IdlType::Primitive("unknown".to_string()) };
            };
            let ident = segment.ident.to_string();
            match ident.as_str() {
                "u8" | "u16" | "u32" | "u64" | "u128" | "i8" | "i16" | "i32" | "i64" | "i128"
                | "bool" | "String" => {
                    let name = ident.to_lowercase();
                    quote! { spel_framework::idl::IdlType::Primitive(#name.to_string()) }
                },
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
                },
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
                },
                "ProgramId" => {
                    quote! { spel_framework::idl::IdlType::Primitive("program_id".to_string()) }
                },
                "AccountId" => {
                    quote! { spel_framework::idl::IdlType::Primitive("account_id".to_string()) }
                },
                other => {
                    let name = other.to_string();
                    quote! { spel_framework::idl::IdlType::Defined { defined: #name.to_string() } }
                },
            }
        },
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
        },
        _ => {
            quote! { spel_framework::idl::IdlType::Primitive("unknown".to_string()) }
        },
    }
}

/// Convert a Rust IDL type string to the JSON representation.
/// This produces a JSON value string for embedding in const IDL JSON.
fn rust_type_to_idl_json(ty: &Type) -> String {
    match ty {
        Type::Path(type_path) => {
            // A parsed `syn::Path` always has at least one segment.
            let Some(segment) = type_path.path.segments.last() else {
                return "\"unknown\"".to_string();
            };
            let ident = segment.ident.to_string();
            match ident.as_str() {
                "u8" | "u16" | "u32" | "u64" | "u128" | "i8" | "i16" | "i32" | "i64" | "i128"
                | "bool" | "String" => {
                    format!("\"{}\"", ident.to_lowercase())
                },
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
                },
                "ProgramId" => "\"program_id\"".to_string(),
                "AccountId" => "\"account_id\"".to_string(),
                other => format!("{{\"defined\":\"{other}\"}}"),
            }
        },
        Type::Array(arr) => {
            let elem = rust_type_to_idl_json(&arr.elem);
            if let syn::Expr::Lit(lit) = &arr.len {
                if let syn::Lit::Int(n) = &lit.lit {
                    return format!("{{\"array\":[{elem},{n}]}}");
                }
            }
            format!("{{\"array\":[{elem},0]}}")
        },
        _ => "\"unknown\"".to_string(),
    }
}

/// The instructions in the order the IDL lists them. Clients encode an
/// instruction's IDL position as its Borsh variant tag, so with an external
/// enum this is the enum's variant order; otherwise source order, which the
/// generated enum follows. Source order also when the enum cannot be found.
fn idl_instruction_order<'a>(
    instructions: &'a [InstructionInfo],
    external_instruction: Option<&str>,
    local_items: &[syn::Item],
    dep_dirs: &[std::path::PathBuf],
) -> syn::Result<Vec<&'a InstructionInfo>> {
    let in_source_order: Vec<&InstructionInfo> = instructions.iter().collect();
    let Some(enum_path) = external_instruction else {
        return Ok(in_source_order);
    };
    let Some(variants) =
        spel_framework_core::idl_gen::external_enum_variants(enum_path, local_items, dep_dirs)
    else {
        return Ok(in_source_order);
    };
    spel_framework_core::idl_gen::order_by_enum_variants(
        in_source_order,
        |ix| ix.fn_name.to_string(),
        &variants,
        enum_path,
    )
    .map_err(|msg| syn::Error::new(proc_macro2::Span::call_site(), msg))
}

// ─── IDL generation (code-based, for __program_idl()) ────────────────────

/// Compute SHA256("global:{name}")[..8] discriminator at macro expansion time.
fn compute_discriminator(name: &str) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(format!("global:{name}").as_bytes());
    let result = hasher.finalize();
    result.get(..8).unwrap_or_default().to_vec()
}

fn generate_idl_fn(
    mod_name: &Ident,
    instructions: &[&InstructionInfo],
    external_instruction: Option<&str>,
    accounts: Vec<spel_framework_core::idl::IdlAccountType>,
    types: Vec<spel_framework_core::idl::IdlTypeDef>,
) -> TokenStream2 {
    let program_name = mod_name.to_string();

    // Serialize accounts and types to JSON for embedding in generated code
    let accounts_json = serde_json::to_string(&accounts).unwrap_or_else(|err| {
        panic!("failed to serialize IDL accounts to JSON during macro expansion: {err}")
    });
    let types_json = serde_json::to_string(&types).unwrap_or_else(|err| {
        panic!("failed to serialize IDL types to JSON during macro expansion: {err}")
    });

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
                                if seg.ident == "self_account_id" {
                                    quote! { Some("self_account_id".to_string()) }
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

fn generate_idl_json(
    mod_name: &Ident,
    instructions: &[&InstructionInfo],
    external_instruction: Option<&str>,
    accounts: Vec<spel_framework_core::idl::IdlAccountType>,
    types: Vec<spel_framework_core::idl::IdlTypeDef>,
) -> String {
    let program_name = mod_name.to_string();

    // Serialize accounts and types to JSON
    let accounts_json_str = serde_json::to_string(&accounts).unwrap_or_else(|err| {
        panic!("failed to serialize IDL accounts to JSON during macro expansion: {err}")
    });
    let types_json_str = serde_json::to_string(&types).unwrap_or_else(|err| {
        panic!("failed to serialize IDL types to JSON during macro expansion: {err}")
    });

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
                                    format!("{{\"kind\":\"const\",\"value\":\"{val}\"}}")
                                },
                                PdaSeedDef::Account(name) => {
                                    format!("{{\"kind\":\"account\",\"path\":\"{name}\"}}")
                                },
                                PdaSeedDef::Arg(name) => {
                                    format!("{{\"kind\":\"arg\",\"path\":\"{name}\"}}")
                                },
                            })
                            .collect();
                        if acc.constraints.private_pda {
                            format!(
                                ",\"pda\":{{\"seeds\":[{}],\"private\":true}}",
                                seeds.join(",")
                            )
                        } else {
                            format!(",\"pda\":{{\"seeds\":[{}]}}", seeds.join(","))
                        }
                    };

                    let visibility_json = if acc.constraints.private_pda {
                        ",\"visibility\":[\"private\"]".to_string()
                    } else {
                        String::new()
                    };
                    let rest_json = if acc.is_rest {
                        ",\"rest\":true".to_string()
                    } else {
                        String::new()
                    };
                    format!(
                        "{{\"name\":\"{name}\",\"writable\":{writable},\"signer\":{signer},\"init\":{init}{pda_json}{rest_json}{visibility_json}}}"
                    )
                })
                .collect();

            let args_json: Vec<String> = ix
                .args
                .iter()
                .map(|arg| {
                    let name = arg.name.to_string().trim_start_matches('_').to_string();
                    let type_json = rust_type_to_idl_json(&arg.ty);
                    format!("{{\"name\":\"{name}\",\"type\":{type_json}}}")
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
        format!(",\"instruction_type\":\"{ext}\"")
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
            format!("Failed to read '{file_path}' (resolved: '{resolved_path}'): {e}"),
        )
    })?;

    // Parse as a Rust file
    let file = syn::parse_file(&content).map_err(|e| {
        syn::Error::new_spanned(span_token, format!("Failed to parse '{file_path}': {e}"))
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
            format!("No #[lez_program] module found in '{file_path}'"),
        )
    })?;

    let mod_name = &program_mod.ident;

    let (_, items) = program_mod
        .content
        .as_ref()
        .ok_or_else(|| syn::Error::new_spanned(span_token, "lez_program module has no body"))?;

    let manifest_dir = std::path::PathBuf::from(&resolved_path);
    let mut deps = spel_framework_core::extension::resolve_program_deps(
        &manifest_dir,
        &program_mod.attrs,
        &mut |_| {},
    )
    .map_err(|msg| syn::Error::new(proc_macro2::Span::call_site(), msg))?;
    let consumer_fns: Vec<ItemFn> = items
        .iter()
        .filter_map(|i| match i {
            syn::Item::Fn(f) if has_instruction_attr(&f.attrs) => Some(f.clone()),
            _ => None,
        })
        .collect();
    spel_framework_core::extension::rewrite_embedded_roles(
        &mut deps.extensions.inject_specs,
        &deps.extensions.embeds,
        &consumer_fns,
    )
    .map_err(|msg| syn::Error::new(proc_macro2::Span::call_site(), msg))?;
    let active_wraps = spel_framework_core::extension::active_wraps(&deps.extensions.wraps);

    // Parse instructions
    let mut instructions: Vec<InstructionInfo> = Vec::new();
    for item in items {
        if let syn::Item::Fn(func) = item {
            if has_instruction_attr(&func.attrs) {
                let mut func = func.clone();
                let injected = spel_framework_core::extension::apply_wrap_and_inject(
                    &mut func,
                    &active_wraps,
                    &deps.extensions.inject_specs,
                    &deps.extensions.embeds,
                    None,
                )
                .map_err(|msg| syn::Error::new(proc_macro2::Span::call_site(), msg))?;
                let mut info = parse_instruction(func)?;
                info.injected = injected;
                instructions.push(info);
            }
        }
    }

    if instructions.is_empty() {
        return Err(syn::Error::new_spanned(
            span_token,
            "No #[instruction] functions found in the program module",
        ));
    }

    for (func, crate_path) in deps.extensions.instructions {
        let mut func = func.clone();
        let qualified = format!(
            "{}::{}",
            crate_path
                .segments
                .first()
                .map(|s| s.ident.to_string())
                .unwrap_or_default(),
            func.sig.ident
        );
        spel_framework_core::extension::apply_wrap_and_inject(
            &mut func,
            &active_wraps,
            &deps.extensions.inject_specs,
            &deps.extensions.embeds,
            Some(&qualified),
        )
        .map_err(|msg| syn::Error::new(proc_macro2::Span::call_site(), msg))?;
        let mut info = parse_instruction(func)?;
        let name = &info.fn_name;
        info.external_call_path = Some(syn::parse_quote!(#crate_path::#name));
        instructions.push(info);
    }
    spel_framework_core::extension::check_duplicate_instruction_names(instructions.iter().map(
        |i| {
            (
                i.fn_name.to_string(),
                spel_framework_core::extension::instruction_source_label(
                    i.external_call_path.as_ref(),
                ),
            )
        },
    ))
    .map_err(|msg| syn::Error::new(proc_macro2::Span::call_site(), msg))?;

    // Detect external instruction type from the #[lez_program(...)] attr
    let external_instruction_str: Option<String> = program_mod
        .attrs
        .iter()
        .find(|a| a.path().is_ident("lez_program"))
        .and_then(|attr| {
            // Try to parse as lez_program(instruction = "some::Path")
            let mut ext: Option<String> = None;
            drop(attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("instruction") {
                    if let Ok(value) = meta.value() {
                        if let Ok(lit) = value.parse::<syn::LitStr>() {
                            ext = Some(lit.value());
                        }
                    }
                }
                Ok(())
            }));
            ext
        });

    // Collect #[account_type] annotated types: search both the file's top-level
    // items and the items inside the #[lez_program] module body, since user code
    // commonly defines account structs inside the program module.
    let mut all_items: Vec<syn::Item> = file.items.clone();
    all_items.extend(items.clone());

    let idl_instructions = idl_instruction_order(
        &instructions,
        external_instruction_str.as_deref(),
        &all_items,
        &deps.graph.transitive_dirs,
    )?;

    // Also scan path-dependency crates for #[account_type] types.
    // This handles the common project structure where account types are defined
    // in a shared core crate (e.g. my_program_core) and the program binary
    // depends on it via `path = "..."`.
    let (extra_items, dep_source_files) =
        spel_framework_core::idl_gen::collect_items_from_crate_dirs(
            &deps.graph.transitive_dirs,
            |w| eprintln!("warning: {w}"),
        );
    all_items.extend(extra_items);

    let (accounts, types) = account_types::collect_account_types(&all_items);

    // Generate the IDL JSON
    let idl_json = generate_idl_json(
        mod_name,
        &idl_instructions,
        external_instruction_str.as_deref(),
        accounts,
        types,
    );

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
            let path = std::env::temp_dir().join(format!("spel-macro-test-{label}-{n}"));
            std::fs::create_dir_all(&path).unwrap();
            TempDir(path)
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
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    // ── strip_account_attrs ────────────────────────────────────────────────

    fn param_attr_paths(func: &ItemFn) -> Vec<Vec<String>> {
        func.sig
            .inputs
            .iter()
            .map(|input| match input {
                FnArg::Typed(pat_type) => pat_type
                    .attrs
                    .iter()
                    .map(|a| quote!(#a).to_string())
                    .collect(),
                FnArg::Receiver(_) => Vec::new(),
            })
            .collect()
    }

    #[test]
    fn strip_account_attrs_clears_every_account_attr() {
        let mut func: ItemFn = syn::parse_quote! {
            pub fn set_paused(
                #[account(mut, pda = const("pause"))] state: u32,
                #[account(signer)] admin: u32,
                paused: bool,
            ) -> u32 { 0 }
        };
        strip_account_attrs(&mut func);
        assert_eq!(
            param_attr_paths(&func),
            vec![Vec::<String>::new(), Vec::new(), Vec::new()],
            "every #[account(...)] must be gone, on every parameter"
        );
    }

    #[test]
    fn strip_account_attrs_keeps_attrs_it_does_not_own() {
        // The framework owns `account` and nothing else. A parameter attr
        // belonging to an extension library has to survive to expand there.
        let mut func: ItemFn = syn::parse_quote! {
            pub fn gated(#[other] #[account(signer)] caller: u32) -> u32 { 0 }
        };
        strip_account_attrs(&mut func);
        assert_eq!(param_attr_paths(&func), vec![vec!["# [other]".to_string()]]);
    }

    #[test]
    fn strip_account_attrs_leaves_function_attrs_alone() {
        // `#[instruction]` sits on the function, not a parameter. Removing it
        // is `generate_handler_fns`' job, and standalone it is self-consuming.
        let mut func: ItemFn = syn::parse_quote! {
            #[instruction]
            #[inline]
            pub fn noop() -> u32 { 0 }
        };
        strip_account_attrs(&mut func);
        assert_eq!(
            func.attrs.len(),
            2,
            "function attrs are not this fn's business"
        );
    }

    #[test]
    fn strip_account_attrs_accepts_a_parameterless_fn() {
        let mut func: ItemFn = syn::parse_quote! { pub fn noop() {} };
        strip_account_attrs(&mut func);
        assert!(func.sig.inputs.is_empty());
    }

    // ── has_account_type_attr (qualified form) ─────────────────────────────

    #[test]
    fn has_account_type_attr_matches_bare_form() {
        let file =
            syn::parse_file("#[account_type]\npub struct Vault { pub balance: u64 }").unwrap();
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
        let file =
            syn::parse_file("#[foo::bar::account_type]\npub struct Vault { pub balance: u64 }")
                .unwrap();
        if let syn::Item::Struct(s) = &file.items[0] {
            assert!(account_types::has_account_type_attr(&s.attrs));
        } else {
            panic!("expected struct");
        }
    }

    #[test]
    fn has_account_type_attr_rejects_other_attrs() {
        let file =
            syn::parse_file("#[derive(Debug)]\npub struct Vault { pub balance: u64 }").unwrap();
        if let syn::Item::Struct(s) = &file.items[0] {
            assert!(!account_types::has_account_type_attr(&s.attrs));
        } else {
            panic!("expected struct");
        }
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
            "TokenHolding from path dep not found in generated IDL. Output: {output}"
        );
        assert!(
            output.contains("TokenDefinition"),
            "TokenDefinition from path dep not found in generated IDL. Output: {output}"
        );
    }

    /// With an external enum from a path dependency, the IDL lists instructions
    /// in its variant order (the Borsh tag order), not in source order.
    #[test]
    fn generate_idl_orders_instructions_by_external_enum() {
        let tmp = TempDir::new("generate-idl-external-enum");
        tmp.write(
            "core/Cargo.toml",
            "[package]\nname = \"vault_core\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        tmp.write(
            "core/src/lib.rs",
            "pub enum Instruction { OpenVault, SealVault { amount: u128 }, ZapVault }\n",
        );
        tmp.write(
            "methods/guest/Cargo.toml",
            "[package]\nname = \"vault-guest\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\nvault_core = { path = \"../../core\" }\n",
        );
        let program = tmp.write(
            "methods/guest/src/bin/vault.rs",
            r#"
#[lez_program(instruction = "vault_core::Instruction")]
pub mod vault {
    #[instruction]
    pub fn zap_vault(vault: AccountWithMetadata) -> SpelResult { todo!() }
    #[instruction]
    pub fn open_vault(vault: AccountWithMetadata) -> SpelResult { todo!() }
    #[instruction]
    pub fn seal_vault(vault: AccountWithMetadata, amount: u128) -> SpelResult { todo!() }
}
"#,
        );

        let output = expand_generate_idl(
            program.to_str().unwrap(),
            &syn::LitStr::new("test", proc_macro2::Span::call_site()),
        )
        .unwrap()
        .to_string();

        let position = |name: &str| {
            output
                .find(name)
                .unwrap_or_else(|| panic!("{name} missing from IDL: {output}"))
        };
        assert!(position("open_vault") < position("seal_vault"));
        assert!(position("seal_vault") < position("zap_vault"));
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
            "VaultConfig with qualified attribute not found in generated IDL. Output: {output}"
        );
    }
}
