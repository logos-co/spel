//! Slot-carrier resolution: binds an embedded role to the consumer
//! struct carrying its `*_slot` field, and lowers derived offsets to
//! the carrier's const path.
//!
//! The dispatcher expansion runs this; the IDL producers do not. An
//! offset reaches consumer code only through a gate attr, which the
//! IDL producers discard, so they run the gate pass under
//! `GateLocations::Omit` and never need a carrier. Keeping the scan on
//! one side keeps one binding scan set: a carrier living in a local
//! path dependency resolves for the party that reads it, and no second
//! producer can disagree about which items were in scope.

use super::{marker::BoundValue, ExtensionDiscoveries, OffsetSpec};

/// A role's slot carrier: the consumer struct holding the `*_slot`
/// field and the offset const `#[account_type]` derived for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotCarrier {
    pub struct_name: String,
    pub const_name: String,
    pub attr_name: String,
}

/// The field-marker attribute a role binds: `my_config` binds
/// `#[my_slot]`.
fn slot_attr_name(role: &str) -> String {
    format!("{}_slot", role.strip_suffix("_config").unwrap_or(role))
}

/// The offset const a slot attribute derives: `#[my_slot]` carries
/// `MY_SLOT_OFFSET`. The macro emits the const under this name and the
/// resolver builds the path that reads it, so the convention has one
/// definition rather than one per side.
pub fn slot_offset_const_name(attr_name: &str) -> String {
    format!("{}_OFFSET", attr_name.to_uppercase())
}

/// Binds a role to the one consumer struct carrying its slot
/// attribute. Dependency items never participate, a dep crate must not
/// satisfy or steal the consumer's binding. Two carriers is an error,
/// none means the derive is not adopted.
///
/// # Errors
///
/// `Err` when two structs carry the role's slot attribute, naming
/// both. Callers surface it as a compile error.
pub fn find_slot_carrier(items: &[syn::Item], role: &str) -> Result<Option<SlotCarrier>, String> {
    let attr_name = slot_attr_name(role);
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
                return Err(format!(
                    "both `{first}` and `{}` carry a #[{attr_name}] \
                    field; only one struct may embed this slot",
                    st.ident
                ));
            }
            found = Some(st.ident.clone());
        }
    }

    Ok(found.map(|struct_ident| SlotCarrier {
        struct_name: struct_ident.to_string(),
        const_name: slot_offset_const_name(&attr_name),
        attr_name,
    }))
}

/// The const path a derived offset resolves to.
pub fn carrier_path(c: &SlotCarrier) -> String {
    format!("{}::{}", c.struct_name, c.const_name)
}

/// The carrier for a role that must have one. `initializer` is the
/// anchor attr when the embed was inferred: the fix differs, because an
/// anchored extension has no offset kwarg to fall back to.
fn require_carrier(
    items: &[syn::Item],
    role: &str,
    initializer: Option<&str>,
) -> Result<SlotCarrier, String> {
    find_slot_carrier(items, role)?.ok_or_else(|| {
        let slot = slot_attr_name(role);
        match initializer {
            Some(anchor) => format!(
                "`{role}` derives its offset but no struct carries a #[{slot}] \
                field; the fn carrying #[{anchor}] puts this extension in \
                embedded mode, so mark the embedding field with #[{slot}], or \
                remove #[{anchor}] for dedicated mode"
            ),
            None => format!(
                "`{role}` derives its offset but no struct carries a #[{slot}] \
                field; mark the embedded field or declare `offset = <bytes>` \
                on the marker"
            ),
        }
    })
}

/// Lower ever `Derived` offset in the discoveries to the carrier's
/// const path. Runs once, immediately after discovery, in every
/// producer. After this pass a surviving `Derived` is a framework bug.
///
/// # Errors
///
/// `Err` when a derivation has no carrier, spelling out the fix, or
/// when the carrier binding is ambiguous. Callers surface it as a
/// compile error.
pub fn resolve_derived_offsets(
    ext: &mut ExtensionDiscoveries,
    items: &[syn::Item],
) -> Result<(), String> {
    for embed in &mut ext.embeds {
        // Every embed binds its carrier here, whatever its offset came
        // from, so one pass owns the binding and the emission side reads
        // it. A derivation must have one, since it has no other source
        // for the offset. A literal takes one when the consumer marked
        // the field, and the agreement assert compares the two; with no
        // marked field there is nothing to compare and nothing to emit.
        embed.carrier = if embed.decl.offset == OffsetSpec::Derived {
            let carrier =
                require_carrier(items, &embed.decl.role, embed.decl.initializer.as_deref())?;
            embed.decl.offset = OffsetSpec::Path(carrier_path(&carrier));
            Some(carrier)
        } else {
            find_slot_carrier(items, &embed.decl.role)?
        };
    }
    for values in ext.bound_calls.values_mut() {
        for v in values {
            if let BoundValue::Derived { role } = v {
                let initializer = ext
                    .embeds
                    .iter()
                    .find(|e| e.decl.role == *role)
                    .and_then(|e| e.decl.initializer.as_deref());
                *v = BoundValue::Path(carrier_path(&require_carrier(items, role, initializer)?));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension::{Embed, EmbedDecl};

    fn items(src: &str) -> Vec<syn::Item> {
        syn::parse_file(src).expect("fixture parses").items
    }

    fn discoveries(offset: OffsetSpec, bound: BoundValue) -> ExtensionDiscoveries {
        let mut ext = ExtensionDiscoveries::default();
        ext.embeds.push(Embed {
            source: "my-ext".into(),
            carrier: None,
            state_type: "my_ext::Cfg".into(),
            decl: EmbedDecl {
                role: "gate_config".into(),
                account: "cfg".into(),
                offset,
                initializer: None,
            },
        });
        ext.bound_calls.insert("action".into(), vec![bound]);
        ext
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
    fn no_carrier_binds_nothing() {
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

    // The pass lowers both carriers of a derivation: the embed's offset
    // and the bound value, each to the same carrier const path.
    #[test]
    fn resolution_lowers_derived_to_the_carrier_path() {
        let its = items("pub struct Cfg { #[gate_slot] pub s: u8 }");
        let mut ext = discoveries(
            OffsetSpec::Derived,
            BoundValue::Derived {
                role: "gate_config".into(),
            },
        );
        resolve_derived_offsets(&mut ext, &its).expect("resolves");
        assert_eq!(
            ext.embeds[0].decl.offset,
            OffsetSpec::Path("Cfg::GATE_SLOT_OFFSET".into())
        );
        assert_eq!(
            ext.bound_calls["action"][0],
            BoundValue::Path("Cfg::GATE_SLOT_OFFSET".into())
        );
    }

    // Explicit offsets pass through untouched: the pass owns
    // derivations and nothing else.
    #[test]
    fn resolution_leaves_literals_alone() {
        let its = items("pub struct Cfg { #[gate_slot] pub s: u8 }");
        let mut ext = discoveries(OffsetSpec::Literal(32), BoundValue::Literal(32));
        resolve_derived_offsets(&mut ext, &its).expect("resolves");
        assert_eq!(ext.embeds[0].decl.offset, OffsetSpec::Literal(32));
        assert_eq!(ext.bound_calls["action"][0], BoundValue::Literal(32));
    }

    // An anchored extension cannot fall back to an offset kwarg, so its
    // missing-carrier error offers the two fixes that exist: mark the
    // field, or drop the anchor for dedicated mode.
    #[test]
    fn anchored_missing_carrier_names_the_anchor() {
        let its = items("pub struct Cfg { pub s: u8 }");
        let mut ext = discoveries(OffsetSpec::Derived, BoundValue::Literal(0));
        ext.embeds[0].decl.initializer = Some("gate_initialize".into());
        let Err(msg) = resolve_derived_offsets(&mut ext, &its) else {
            panic!("expected the missing-carrier error");
        };
        assert!(
            msg.contains("#[gate_initialize]") && msg.contains("dedicated mode"),
            "message: {msg}"
        );
        assert!(
            !msg.contains("offset = <bytes>"),
            "an anchored extension has no offset kwarg to suggest: {msg}"
        );
    }

    // A derivation with no carrier struct is a consumer mistake and the
    // error says what to do about it.
    #[test]
    fn missing_carrier_names_the_fix() {
        let its = items("pub struct Cfg { pub s: u8 }");
        let mut ext = discoveries(OffsetSpec::Derived, BoundValue::Literal(0));
        let Err(msg) = resolve_derived_offsets(&mut ext, &its) else {
            panic!("expected the missing-carrier error");
        };
        assert!(
            msg.contains("#[gate_slot]") && msg.contains("mark the embedded field"),
            "message: {msg}"
        );
    }
}
