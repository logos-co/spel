//! Slot-carrier resolution: binds an embedded role to the consumer
//! struct carrying its `*_slot` field, and lowers derived offsets to
//! the carrier's const path. Shared by the dispatcher expansion and
//! the CLI IDL path so both fail identically on a missing carrier.

use super::{marker::BoundValue, ExtensionDiscoveries, OffsetSpec};

/// A role's slot carrier: the consumer struct holding the `*_slot`
/// field and the offset const `#[account_type]` derived for it.
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

/// The carrier for a role that must have one.
fn require_carrier(items: &[syn::Item], role: &str) -> Result<SlotCarrier, String> {
    find_slot_carrier(items, role)?.ok_or_else(|| {
        format!(
            "`{role}` derives its offset but no struct carries a #[{}] \
            field; mark the embedded field or declare `offset = <bytes> \
            on the marker",
            slot_attr_name(role)
        )
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
    for (_, embed) in &mut ext.embeds {
        if embed.offset == OffsetSpec::Derived {
            embed.offset = OffsetSpec::Path(carrier_path(&require_carrier(items, &embed.role)?));
        }
    }
    for values in ext.bound_calls.values_mut() {
        for v in values {
            if let BoundValue::Derived { role } = v {
                *v = BoundValue::Path(carrier_path(&require_carrier(items, role)?));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension::EmbedDecl;

    fn items(src: &str) -> Vec<syn::Item> {
        syn::parse_file(src).expect("fixture parses").items
    }

    fn discoveries(offset: OffsetSpec, bound: BoundValue) -> ExtensionDiscoveries {
        let mut ext = ExtensionDiscoveries::default();
        ext.embeds.push((
            "my-ext".into(),
            EmbedDecl {
                role: "gate_config".into(),
                account: "cfg".into(),
                offset,
                initializer: None,
            },
        ));
        ext.bound_calls.insert("action".into(), vec![bound]);
        ext
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
            ext.embeds[0].1.offset,
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
        assert_eq!(ext.embeds[0].1.offset, OffsetSpec::Literal(32));
        assert_eq!(ext.bound_calls["action"][0], BoundValue::Literal(32));
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
