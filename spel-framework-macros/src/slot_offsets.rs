//! Embedded-slot support behind `#[account_type]` and `#[lez_program]`.
//! A field carrying a `*_slot` attribute (e.g. `#[admin_slot]`) makes
//! the struct gain a derived `<NAME>_OFFSET` const, computed as a sum
//! of `FixedBorshSize::SIZE` terms that rustc evaluates, plus a layout
//! test emitted into the consumer crate that serializes a probe value
//! in the slot field and asserts it lands at the derived offset.
//!
//! The const serves two masters. A marker that declares an explicit
//! offset gets an agreement assert, declared equals derived, so marker
//! drift is a compile error. A marker that omits the offset derives it:
//! `extension::slots` resolves the role to the carrier's const path and
//! this module lowers the resolved values back to tokens for the
//! dispatcher, the stamped gates, and the window-collision asserts,
//! with rustc evaluating what discovery-time code cannot. Structs
//! without slot attributes pass through unchanged.

use quote::quote;
use spel_framework_core::extension::{
    find_slot_carrier, BoundValue, EmbedDecl, OffsetSpec, SlotCarrier,
};

// ── account_type side: derive the offset ─────────────────────────────────

pub(crate) fn expand(item: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let mut st = match syn::parse::<syn::ItemStruct>(item.clone()) {
        Ok(s) => s,
        // Enums and anything non-struct keep the passthrough behavior.
        Err(_) => return item,
    };

    let slots = strip_slot_fields(&mut st);
    if slots.is_empty() {
        return quote::quote! {#st}.into();
    }

    let struct_ident = st.ident.clone();
    let named_types: Vec<syn::Type> = match &st.fields {
        syn::Fields::Named(f) => f.named.iter().map(|x| x.ty.clone()).collect(),
        _ => Vec::new(),
    };

    let mut consts = proc_macro2::TokenStream::new();
    let mut tests = proc_macro2::TokenStream::new();

    for slot in &slots {
        consts.extend(offset_const(
            &struct_ident,
            slot,
            &named_types[..slot.index],
        ));
        tests.extend(layout_test(&struct_ident, slot));
    }

    let checks_mod = quote::format_ident!(
        "__slot_layout_checks_{}",
        struct_ident.to_string().to_lowercase()
    );

    quote::quote! {
        #st
        #consts
        #[cfg(test)]
        mod #checks_mod {
            use super::*;
            #tests
        }
    }
    .into()
}

/// Const asserts refusing overlapping embedded windows in one account.
///
/// Discovery rejects identical literal offsets, but only rustc knows
/// window lengths and derived offsets, so range overlap is checked
/// here: one assert per embed pair sharing an account, each window's
/// length read from the extension's declared state type through
/// `FixedBorshSize::SIZE`, each offset lowered as its literal or its
/// carrier const path. Touching windows are legal.
pub(crate) fn embed_window_collision_asserts(
    embeds: &[(String, EmbedDecl)],
    state_types: &std::collections::HashMap<String, String>,
) -> syn::Result<proc_macro2::TokenStream> {
    let mut out = proc_macro2::TokenStream::new();
    for (i, (source_a, a)) in embeds.iter().enumerate() {
        for (source_b, b) in embeds.iter().skip(i + 1) {
            if a.account != b.account {
                continue;
            }
            let ty_a = state_type_path(state_types, source_a)?;
            let ty_b = state_type_path(state_types, source_b)?;
            let (off_a, off_b) = (offset_tokens(a)?, offset_tokens(b)?);
            let message = format!(
                "embedded windows of `{source_a}` (offset {off_a}) and \
                `{source_b}` (offset {off_b}) overlap in account `{}`",
                a.account
            );
            out.extend(quote! {
                const _: () = assert!(
                    #off_a + <#ty_a as ::spel_framework::FixedBorshSize>::SIZE <= #off_b
                        || #off_b + <#ty_b as ::spel_framework::FixedBorshSize>::SIZE <= #off_a,
                    #message
                );
            });
        }
    }
    Ok(out)
}

/// The declared state type of an embedded window, parsed to a path.
fn state_type_path(
    state_types: &std::collections::HashMap<String, String>,
    source: &str,
) -> syn::Result<syn::Path> {
    let Some(raw) = state_types.get(source) else {
        return Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            format!(
                "extension '{source}' declares no embedded.state_type; \
                discovery must have rejected this"
            ),
        ));
    };
    syn::parse_str(raw).map_err(|e| {
        syn::Error::new(
            proc_macro2::Span::call_site(),
            format!(
                "extension '{source}': embedded.state_type {raw:?} \
                is not a valid type path: {e}"
            ),
        )
    })
}

/// A field marked as an embedded slot, after its attribute was
/// stripped: what the emissions need to know and nothing else.
struct SlotField {
    index: usize,
    attr_name: String,
    ident: syn::Ident,
    ty: syn::Type,
}

/// Find fields carrying a `*_slot` attr and strip the attr so rustc
/// never sees it.
fn strip_slot_fields(st: &mut syn::ItemStruct) -> Vec<SlotField> {
    let mut slots = Vec::new();
    let syn::Fields::Named(fields) = &mut st.fields else {
        return slots;
    };
    for (index, field) in fields.named.iter_mut().enumerate() {
        let mut found: Option<String> = None;
        field.attrs.retain(|a| {
            let slot_name = a
                .path()
                .get_ident()
                .map(|id| id.to_string())
                .filter(|n| n.ends_with("_slot"));
            match slot_name {
                Some(n) => {
                    found = Some(n);
                    false
                },
                None => true,
            }
        });
        if let Some(attr_name) = found {
            slots.push(SlotField {
                index,
                attr_name,
                ident: field.ident.clone().expect("named field has an ident"),
                ty: field.ty.clone(),
            });
        }
    }
    slots
}

/// The derived offset: a sum of `FixedBorshSize::SIZE` terms rustc
/// evaluates, so aliases resolve and unsupported types fail to compile.
fn offset_const(
    struct_ident: &syn::Ident,
    slot: &SlotField,
    preceding: &[syn::Type],
) -> proc_macro2::TokenStream {
    let const_ident = quote::format_ident!("{}_OFFSET", slot.attr_name.to_uppercase());
    quote::quote! {
        impl #struct_ident {
            pub const #const_ident: usize =
                0 #( + <#preceding as ::spel_framework::FixedBorshSize>::SIZE )*;
        }
    }
}

/// The emitted layout test: a probe value serialized in the slot field
/// must land at the derived offset, pinning the size arithmetic to real
/// serialization.
fn layout_test(struct_ident: &syn::Ident, slot: &SlotField) -> proc_macro2::TokenStream {
    let const_ident = quote::format_ident!("{}_OFFSET", slot.attr_name.to_uppercase());
    let test_ident = quote::format_ident!("__{}_offset_matches_layout", slot.attr_name);
    let field_ident = &slot.ident;
    let field_ty = &slot.ty;
    quote::quote! {
        #[test]
        fn #test_ident() {
            let mut v = <#struct_ident as ::core::default::Default>::default();
            v.#field_ident = <#field_ty as ::spel_framework::SlotLayoutProbe>::probe();
            let bytes = borsh::to_vec(&v).expect("serialize the struct");
            let probe = borsh::to_vec(&v.#field_ident).expect("serialize the probe");
            let at = #struct_ident::#const_ident;
            assert_eq!(
                &bytes[at..at + probe.len()],
                &probe[..],
                concat!(
                    "the `", stringify!(#field_ident),
                    "` field does not sit at the derived offset; a \
                     preceding field changed without the layout following"
                )
            );
        }
    }
}

// ── lez_program side: marker agreement and derived offsets ──────────────

/// Emit every embedded marker's agreement assert: declared literal
/// offset equals the carrier's derived const. Derived offsets need no
/// agreement assert, the carrier const is their single source of
/// truth. Window collisions are a separate emission,
/// [`embed_window_collision_asserts`].
pub(crate) fn emit_agreement_asserts(
    guest_path: &std::path::Path,
    embeds: &[(String, EmbedDecl)],
) -> syn::Result<proc_macro2::TokenStream> {
    let scan_items = consumer_scan_items(guest_path);
    let mut out = proc_macro2::TokenStream::new();
    for (_, embed) in embeds {
        if let OffsetSpec::Literal(off) = embed.offset {
            if let Some(c) = find_slot_carrier(&scan_items, &embed.role)
                .map_err(|m| syn::Error::new(proc_macro2::Span::call_site(), m))?
            {
                out.extend(agreement_assert(&c, off));
            }
        }
    }
    Ok(out)
}

/// The consumer's binding scan set: the entry file with its inline and
/// file-backed modules, plus local path-dependency crates for the
/// shared-core layout. Git and registry dependencies never participate,
/// a foreign crate must not satisfy or steal the consumer's slot
/// binding.
pub(crate) fn consumer_scan_items(guest_path: &std::path::Path) -> Vec<syn::Item> {
    let mut scan_items =
        spel_framework_core::idl_gen::collect_file_items_following_mods(guest_path);
    if let Ok(md) = std::env::var("CARGO_MANIFEST_DIR") {
        let manifest = std::path::Path::new(&md).join("Cargo.toml");
        let (path_dep_items, _) = spel_framework_core::idl_gen::collect_items_from_crate_dirs(
            &spel_framework_core::dep_walk::path_dep_dirs(&manifest),
            |w| eprintln!("warning: {w}"),
        );
        scan_items.extend(path_dep_items);
    }
    scan_items
}

/// Emit the compile-time agreement check for an explicit offset.
fn agreement_assert(c: &SlotCarrier, offset: usize) -> proc_macro2::TokenStream {
    let (path, attr_name) = (carrier_tokens(c), &c.attr_name);
    let msg = format!(
        "the marker offset {offset} disagrees with {}::{}; a field before \
        the #[{attr_name}] field changed without the marker following",
        c.struct_name, c.const_name
    );
    quote::quote! {
        const _: () = assert!(#path == #offset, #msg);
    }
}

// ── token lowering: resolved offsets back into consumer code ─────────────

/// The carrier's offset const as tokens: `Cfg::MY_SLOT_OFFSET`.
fn carrier_tokens(c: &SlotCarrier) -> proc_macro2::TokenStream {
    let (struct_ident, const_ident) = (
        quote::format_ident!("{}", &c.struct_name),
        quote::format_ident!("{}", c.const_name),
    );
    quote::quote! { #struct_ident::#const_ident }
}

/// One embed's offset as tokens: the declared literal, or the
/// carrier's const path when the marker derived it. A `Derived` offset
/// here means the resolution pass never ran, which is a framework bug
/// rather than a consumer mistake, so it names the missing step.
fn offset_tokens(embed: &EmbedDecl) -> syn::Result<proc_macro2::TokenStream> {
    match &embed.offset {
        OffsetSpec::Literal(n) => Ok(quote::quote! { #n }),
        OffsetSpec::Path(p) => {
            let path: syn::Expr = syn::parse_str(p)?;
            Ok(quote::quote! { #path })
        },
        OffsetSpec::Derived => Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            format!(
                "`{}` reached assert emission with an unresolved derived \
                offset; `resolve_derived_offsets` must run after discovery",
                embed.role
            ),
        )),
    }
}

/// One resolved bound value as a dispatch call argument: the literal,
/// or the carrier's const path. `offset_tokens`' twin for the
/// dispatcher, which appends these to the generated call. An unresolved
/// `Derived` here means the resolution pass never ran; a proc-macro
/// panic is a compile error, so the invariant fails loudly at the
/// consumer's build.
pub(crate) fn bound_value_tokens(v: &BoundValue) -> proc_macro2::TokenStream {
    match v {
        BoundValue::Literal(n) => {
            let lit = proc_macro2::Literal::usize_unsuffixed(*n);
            quote::quote! { #lit }
        },
        BoundValue::Path(p) => {
            let path: syn::Expr =
                syn::parse_str(p).expect("a resolved carrier path parses as an expression");
            quote::quote! { #path }
        },
        BoundValue::Derived { role } => panic!(
            "`{role}` reached dispatch emission with an unresolved \
            derived offset; resolve_derived_offsets must run after \
            discovery"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(src: &str) -> Vec<syn::Item> {
        syn::parse_file(src).expect("fixture parses").items
    }

    fn embed(role: &str, account: &str, offset: OffsetSpec) -> (String, EmbedDecl) {
        (
            role.to_string(),
            EmbedDecl {
                role: role.to_string(),
                account: account.to_string(),
                offset,
            },
        )
    }

    #[test]
    fn binds_the_roles_slot_attr() {
        let its = items("#[account_type]\npub struct Cfg { pub v: u64, #[admin_slot] pub a: u8 }");
        let c = find_slot_carrier(&its, "admin_config")
            .expect("unambiguous")
            .expect("binds");
        assert_eq!(c.struct_name, "Cfg");
        assert_eq!(c.const_name, "ADMIN_SLOT_OFFSET");
    }

    #[test]
    fn role_without_config_suffix_uses_the_full_role() {
        let its = items("pub struct S { #[vault_slot] pub s: u8 }");
        let c = find_slot_carrier(&its, "vault").unwrap().expect("binds");
        assert_eq!(c.attr_name, "vault_slot");
    }

    #[test]
    fn no_carrier_emits_nothing() {
        let its = items("pub struct S { pub v: u64 }");
        assert!(find_slot_carrier(&its, "admin_config").unwrap().is_none());
    }

    #[test]
    fn two_carriers_is_an_error_naming_both() {
        let its = items(
            "pub struct Alpha { #[admin_slot] pub a: u8 }\npub struct Beta { #[admin_slot] pub b: u8 }",
        );
        let Err(msg) = find_slot_carrier(&its, "admin_config") else {
            panic!("expected the two-carrier ambiguity error");
        };
        assert!(
            msg.contains("Alpha") && msg.contains("Beta"),
            "message: {msg}"
        );
    }

    #[test]
    fn agreement_assert_names_both_sides() {
        let its = items("pub struct Cfg { #[admin_slot] pub a: u8 }");
        let c = find_slot_carrier(&its, "admin_config").unwrap().unwrap();
        let ts = agreement_assert(&c, 32).to_string();
        assert!(
            ts.contains("ADMIN_SLOT_OFFSET") && ts.contains("32"),
            "{ts}"
        );
    }

    fn state_types(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    // A shared account's embed pair becomes one assert: each window's
    // length through its declared state type, offsets as literals.
    #[test]
    fn shared_account_pair_emits_a_range_assert() {
        let embeds = vec![
            embed("admin", "cfg", OffsetSpec::Literal(32)),
            embed("freeze", "cfg", OffsetSpec::Literal(40)),
        ];
        let types = state_types(&[
            ("admin", "admin_authority::AdminConfig"),
            ("freeze", "freeze_authority::FreezeConfig"),
        ]);
        let ts = embed_window_collision_asserts(&embeds, &types)
            .expect("emits")
            .to_string();
        assert!(
            ts.contains("AdminConfig") && ts.contains("FreezeConfig"),
            "{ts}"
        );
        assert!(ts.contains("FixedBorshSize"), "{ts}");
        assert!(ts.contains("overlap in account `cfg`"), "{ts}");
    }

    // Separate accounts have nothing to collide.
    #[test]
    fn separate_accounts_emit_no_collision_assert() {
        let embeds = vec![
            embed("admin", "cfg_a", OffsetSpec::Literal(32)),
            embed("freeze", "cfg_b", OffsetSpec::Literal(32)),
        ];
        let types = state_types(&[("admin", "A"), ("freeze", "B")]);
        let ts = embed_window_collision_asserts(&embeds, &types).expect("ok");
        assert!(ts.is_empty(), "{ts}");
    }

    // A missing state type here is a framework bug, discovery fails
    // closed before emission. The error still names the source.
    #[test]
    fn missing_state_type_is_an_error_naming_the_source() {
        let embeds = vec![
            embed("admin", "cfg", OffsetSpec::Literal(32)),
            embed("freeze", "cfg", OffsetSpec::Literal(64)),
        ];
        let types = state_types(&[("admin", "A")]);
        let e = embed_window_collision_asserts(&embeds, &types).expect_err("must fail");
        assert!(e.to_string().contains("freeze"), "{e}");
    }

    // A malformed declared path fails naming the offending string.
    #[test]
    fn invalid_state_type_path_is_an_error() {
        let embeds = vec![
            embed("admin", "cfg", OffsetSpec::Literal(32)),
            embed("freeze", "cfg", OffsetSpec::Literal(64)),
        ];
        let types = state_types(&[("admin", "not a path!!"), ("freeze", "B")]);
        let e = embed_window_collision_asserts(&embeds, &types).expect_err("must fail");
        assert!(e.to_string().contains("not a path!!"), "{e}");
    }

    // Two resolved derivations in one account land in the same range
    // assert as const paths: neither side is a number until rustc
    // evaluates it, so the check discovery could not make lands in the
    // consumer's crate.
    #[test]
    fn derived_pair_collides_through_the_const_paths() {
        let embeds = [
            embed(
                "admin_config",
                "config",
                OffsetSpec::Path("Cfg::ADMIN_SLOT_OFFSET".into()),
            ),
            embed(
                "freeze_config",
                "config",
                OffsetSpec::Path("Cfg::FREEZE_SLOT_OFFSET".into()),
            ),
        ];
        let types = state_types(&[("admin_config", "A"), ("freeze_config", "B")]);
        let ts = embed_window_collision_asserts(&embeds, &types)
            .expect("both sides resolve")
            .to_string();
        assert!(
            ts.contains("ADMIN_SLOT_OFFSET") && ts.contains("FREEZE_SLOT_OFFSET"),
            "{ts}"
        );
        assert!(ts.contains("FixedBorshSize"), "{ts}");
    }

    // An unresolved derivation reaching emission is a framework bug, so
    // it fails loudly naming the pass that should have run.
    #[test]
    fn unresolved_derived_offset_names_the_missing_pass() {
        let (_, e) = embed("admin_config", "config", OffsetSpec::Derived);
        let Err(err) = offset_tokens(&e) else {
            panic!("expected the unresolved-derived error");
        };
        assert!(
            err.to_string().contains("resolve_derived_offsets"),
            "message: {err}"
        );
    }

    // The dispatcher lowering renders each resolved shape and refuses
    // the unresolved one.
    #[test]
    fn bound_literal_renders_the_number() {
        assert_eq!(
            bound_value_tokens(&BoundValue::Literal(32)).to_string(),
            "32"
        );
    }

    #[test]
    fn bound_path_renders_the_const_path() {
        let ts = bound_value_tokens(&BoundValue::Path("Cfg::ADMIN_SLOT_OFFSET".into()));
        assert_eq!(ts.to_string(), "Cfg :: ADMIN_SLOT_OFFSET");
    }

    #[test]
    #[should_panic(expected = "resolve_derived_offsets")]
    fn bound_derived_panics_naming_the_missing_pass() {
        bound_value_tokens(&BoundValue::Derived {
            role: "admin_config".into(),
        });
    }
}
