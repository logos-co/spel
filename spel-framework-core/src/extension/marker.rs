//! Module-marker attribute handling: the pre-check that gates the
//! metadata walk and the marker argument grammar.

use syn::Attribute;

/// True when the module carries at least one attribute that could be an
/// extension marker. Built-in and framework attrs are excluded. Unknown
/// or path-qualified attrs count as candidates: the guard may only skip
/// work when skipping is provably free.
pub fn has_extension_marker_candidates(mod_attrs: &[Attribute]) -> bool {
    mod_attrs.iter().any(|a| {
        let Some(ident) = a.path().get_ident() else {
            return true;
        };
        !matches!(
            ident.to_string().as_str(),
            "lez_program"
                | "doc"
                | "cfg"
                | "cfg_attr"
                | "allow"
                | "deny"
                | "warn"
                | "expect"
                | "forbid"
                | "deprecated"
        )
    })
}

/// Extract the single-ident arg from an attribute whose path matches
/// `ext_attr`. Returns `Some("")` for bare attribute, `Some(ident)`
/// for `#[name(ident)]`, and `None` if the path doesn't match or the
/// args aren't a single ident.
pub(super) fn extract_attr_arg(attr: &Attribute, ext_attr: &str) -> Option<String> {
    if !attr.path().is_ident(ext_attr) {
        return None;
    }
    if matches!(attr.meta, syn::Meta::Path(_)) {
        return Some(String::new());
    }
    attr.parse_args::<syn::Ident>().ok().map(|i| i.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn marker_candidates_false_for_builtin_attrs_only() {
        let m: syn::ItemMod = syn::parse_quote! {
            #[doc = "hi"]
            #[cfg(test)]
            #[allow(dead_code)]
            mod program {}
        };
        assert!(!has_extension_marker_candidates(&m.attrs));
        assert!(!has_extension_marker_candidates(&[]));
    }

    #[test]
    fn marker_candidates_true_for_unknown_and_qualified_attrs() {
        let m: syn::ItemMod = syn::parse_quote! {
            #[my_ext]
            mod program {}
        };
        assert!(has_extension_marker_candidates(&m.attrs));

        let q: syn::ItemMod = syn::parse_quote! {
            #[some::qualified]
            mod program {}
        };
        assert!(has_extension_marker_candidates(&q.attrs));
    }
}
