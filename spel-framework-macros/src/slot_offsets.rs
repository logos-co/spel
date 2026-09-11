//! Embedded-slot support behind `#[account_type]` and `#[lez_program]`.
//! A field carrying a `*_slot` attribute (e.g. `#[admin_slot]`) makes
//! the struct gain a derived `<NAME>_OFFSET` const, computed as a sum
//! of `FixedBorshSize::SIZE` terms that rustc evaluates, plus a layout
//! test emitted into the consumer crate that serializes a probe value
//! in the slot field and asserts it lands at the derived offset. For
//! every embedded marker, `lez_program` then emits a const assert that
//! the marker's declared offset equals the derived const, so marker
//! drift is a compile error. Structs without slot attributes pass
//! through unchanged.

use proc_macro::TokenStream;
use quote::quote;

// ── account_type side: derive the offset ─────────────────────────────────

pub(crate) fn expand(item: TokenStream) -> TokenStream {
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
/// Discovery rejects identical offsets, but only rustc knows window
/// lengths, so range overlap is checked here: one assert per embed
/// pair sharing an account, each window's length read from the
/// extension's declared state type through `FixedBorshSize::SIZE`.
/// Touching windows are legal.
pub(crate) fn embed_window_collision_asserts(
    embeds: &[(String, spel_framework_core::extension::EmbedDecl)],
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
            let off_a = a.offset;
            let off_b = b.offset;
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

// ── lez_program side: marker agreement ───────────────────────────────────

/// Emit every embedded marker's agreement assert. The scan set is the
/// consumer's own code: the entry file with its inline and file-backed
/// modules, plus local path-dependency crates for the shared-core
/// layout. Git and registry dependencies never participate, a foreign
/// crate must not satisfy or steal the consumer's slot binding.
pub(crate) fn emit_agreement_asserts(
    guest_path: &std::path::Path,
    embeds: &[(String, spel_framework_core::extension::EmbedDecl)],
) -> syn::Result<proc_macro2::TokenStream> {
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
    let mut out = proc_macro2::TokenStream::new();
    for (_, embed) in embeds {
        if let Some(a) = find_slot_assert(&scan_items, &embed.role, embed.offset)? {
            out.extend(agreement_assert(&a));
        }
    }
    Ok(out)
}

/// One embed's declared offset paired with the derived const that must
/// equal it. Disagreement means gates would read one location while the
/// struct's layout puts the slot at another, so the emitted check turns
/// that into a compile error instead of a wrong-slot read at runtime.
struct SlotAssert {
    pub struct_ident: syn::Ident,
    pub const_ident: syn::Ident,
    pub offset: usize,
    pub attr_name: String,
}

/// Binds an embed to the one consumer struct carrying its slot
/// attribute (`admin_config` binds `#[admin_slot]`). Dependency items
/// never participate, a dep crate must not satisfy or steal the
/// consumer's binding. Two carriers is an error, none means the derive
/// is not adopted and no check is emitted.
fn find_slot_assert(
    items: &[syn::Item],
    role: &str,
    offset: usize,
) -> syn::Result<Option<SlotAssert>> {
    let prefix = role.strip_suffix("_config").unwrap_or(role);
    let attr_name = format!("{prefix}_slot");
    let mut found: Option<syn::Ident> = None;
    for item in items {
        let syn::Item::Struct(st) = item else {
            continue;
        };
        let syn::Fields::Named(fields) = &st.fields else {
            continue;
        };
        let has = fields.named.iter().any(|f| {
            f.attrs.iter().any(|a| {
                a.path()
                    .get_ident()
                    .is_some_and(|id| *id == attr_name.as_str())
            })
        });
        if has {
            if let Some(first) = &found {
                return Err(syn::Error::new(
                    proc_macro2::Span::call_site(),
                    format!(
                        "both `{first}` and `{}` carry a #[{attr_name}] \
                        field; only one struct may embed this slot",
                        st.ident
                    ),
                ));
            }
            found = Some(st.ident.clone());
        }
    }

    Ok(found.map(|struct_ident| SlotAssert {
        struct_ident,
        const_ident: quote::format_ident!("{}_OFFSET", &attr_name.to_uppercase()),
        offset,
        attr_name,
    }))
}

/// Emit the compile-time agreement check.
fn agreement_assert(a: &SlotAssert) -> proc_macro2::TokenStream {
    let SlotAssert {
        struct_ident,
        const_ident,
        offset,
        attr_name,
    } = a;
    let msg = format!(
        "the marker offset {offset} disagrees with \
        {struct_ident}::{const_ident}; a field before the \
        #[{attr_name}] field changed without the marker following"
    );
    quote::quote! {
        const _: () = assert!(#struct_ident::#const_ident == #offset, #msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(src: &str) -> Vec<syn::Item> {
        syn::parse_file(src).expect("fixture parses").items
    }

    #[test]
    fn binds_the_roles_slot_attr() {
        let its = items("#[account_type]\npub struct Cfg { pub v: u64, #[admin_slot] pub a: u8 }");
        let a = find_slot_assert(&its, "admin_config", 8)
            .expect("unambiguous")
            .expect("binds");
        assert_eq!(a.struct_ident, "Cfg");
        assert_eq!(a.const_ident, "ADMIN_SLOT_OFFSET");
        assert_eq!(a.offset, 8);
    }

    #[test]
    fn role_without_config_suffix_uses_the_full_role() {
        let its = items("pub struct S { #[vault_slot] pub s: u8 }");
        let a = find_slot_assert(&its, "vault", 0).unwrap().expect("binds");
        assert_eq!(a.attr_name, "vault_slot");
    }

    #[test]
    fn no_carrier_emits_nothing() {
        let its = items("pub struct S { pub v: u64 }");
        assert!(find_slot_assert(&its, "admin_config", 32)
            .unwrap()
            .is_none());
    }

    #[test]
    fn two_carriers_is_an_error_naming_both() {
        let its = items(
            "pub struct Alpha { #[admin_slot] pub a: u8 }\npub struct Beta { #[admin_slot] pub b: u8 }",
        );
        let Err(e) = find_slot_assert(&its, "admin_config", 32) else {
            panic!("expected the two-carrier ambiguity error");
        };
        let msg = e.to_string();
        assert!(
            msg.contains("Alpha") && msg.contains("Beta"),
            "message: {msg}"
        );
    }

    #[test]
    fn agreement_assert_names_both_sides() {
        let its = items("pub struct Cfg { #[admin_slot] pub a: u8 }");
        let a = find_slot_assert(&its, "admin_config", 32).unwrap().unwrap();
        let ts = agreement_assert(&a).to_string();
        assert!(
            ts.contains("ADMIN_SLOT_OFFSET") && ts.contains("32"),
            "{ts}"
        );
    }

    fn embed(
        source: &str,
        account: &str,
        offset: usize,
    ) -> (String, spel_framework_core::extension::EmbedDecl) {
        (
            source.to_string(),
            spel_framework_core::extension::EmbedDecl {
                role: format!("{source}_config"),
                account: account.to_string(),
                offset,
            },
        )
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
        let embeds = vec![embed("admin", "cfg", 32), embed("freeze", "cfg", 40)];
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
        let embeds = vec![embed("admin", "cfg_a", 32), embed("freeze", "cfg_b", 32)];
        let types = state_types(&[("admin", "A"), ("freeze", "B")]);
        let ts = embed_window_collision_asserts(&embeds, &types).expect("ok");
        assert!(ts.is_empty(), "{ts}");
    }

    // A missing state type here is a framework bug, discovery fails
    // closed before emission. The error still names the source.
    #[test]
    fn missing_state_type_is_an_error_naming_the_source() {
        let embeds = vec![embed("admin", "cfg", 32), embed("freeze", "cfg", 64)];
        let types = state_types(&[("admin", "A")]);
        let e = embed_window_collision_asserts(&embeds, &types).expect_err("must fail");
        assert!(e.to_string().contains("freeze"), "{e}");
    }

    // A malformed declared path fails naming the offending string.
    #[test]
    fn invalid_state_type_path_is_an_error() {
        let embeds = vec![embed("admin", "cfg", 32), embed("freeze", "cfg", 64)];
        let types = state_types(&[("admin", "not a path!!"), ("freeze", "B")]);
        let e = embed_window_collision_asserts(&embeds, &types).expect_err("must fail");
        assert!(e.to_string().contains("not a path!!"), "{e}");
    }
}
