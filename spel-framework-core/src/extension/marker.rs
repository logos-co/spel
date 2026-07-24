//! Module-marker attribute handling: the pre-check that gates the
//! metadata walk and the marker argument grammar.

use syn::{punctuated::Punctuated, Attribute};

/// Embedded-mode declaration parsed from a module marker's kwargs,
/// `#[admin_authority(admin_config = prog_config, offset = 32)]`:
/// the inject role `admin_config` lives inside the consumer account
/// `prog_config` at byte offset 32.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedDecl {
    /// Inject-spec account name being relocated (the role).
    pub role: String,
    /// Consumer account the role's slot lives in.
    pub account: String,
    /// Byte offset of the slot window inside the account's data.
    pub offset: usize,
}

/// Everything a module marker's argument list can carry: an optional
/// bare mode word (`manual`) and an optional embedded declaration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MarkerArgs {
    /// Bare ident arg, the wrap-skip word. `None` when absent.
    pub word: Option<String>,
    /// Embedded-mode declaration. `None` in dedicated mode.
    pub embed: Option<EmbedDecl>,
}

/// Parse a module marker attr's arguments. Returns `Ok(None)` when the
/// attr is not the extension's marker. `Ok(Some(...))` otherwise.
///
/// Grammar: zero or more comma-separated items, each either a bare
/// ident (mode word) or `key = value`. `offset = <int>` is reserved;
/// exactly one other `role = account` pair may accompany it. A role
/// without `offset`, an `offset` without a role, a second role pair,
/// or a non-ident account value are hard errors.
///
/// # Errors
///
/// `Err` on malformed arguments: two mode words, a role without an
/// offset or an offset without a role, duplicate kwargs, a non-ident
/// account value, or a non-integer offset. Callers surface it as a
/// compile error.
pub fn parse_marker_args(attr: &Attribute, ext_attr: &str) -> Result<Option<MarkerArgs>, String> {
    if !attr.path().is_ident(ext_attr) {
        return Ok(None);
    }
    if matches!(attr.meta, syn::Meta::Path(_)) {
        return Ok(Some(MarkerArgs::default()));
    }
    let metas = attr
        .parse_args_with(Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated)
        .map_err(|e| format!("malformed `#[{ext_attr}(...)]` arguments: {e}"))?;

    let mut args = MarkerArgs::default();
    let mut role: Option<(String, String)> = None;
    let mut offset: Option<usize> = None;

    for meta in metas {
        match meta {
            syn::Meta::Path(p) => {
                let word = p
                    .get_ident()
                    .ok_or_else(|| format!("`#[{ext_attr}]`: expected a bare word"))?
                    .to_string();
                if args.word.replace(word).is_some() {
                    return Err(format!("`#[{ext_attr}]`: more than one bare mode word"));
                }
            },
            syn::Meta::NameValue(nv) => {
                let key = nv
                    .path
                    .get_ident()
                    .ok_or_else(|| format!("`#[{ext_attr}]`: expected `key = value`"))?
                    .to_string();
                if key == "offset" {
                    let lit = match &nv.value {
                        syn::Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Int(i),
                            ..
                        }) => i,
                        _ => {
                            return Err(format!(
                                "`#[{ext_attr}]`: `offset` must be an integer literal"
                            ));
                        },
                    };
                    let value = lit
                        .base10_parse::<usize>()
                        .map_err(|e| format!("`#[{ext_attr}]`: bad offset: {e}"))?;
                    if offset.replace(value).is_some() {
                        return Err(format!("`#[{ext_attr}]`: more than one `offset`"));
                    }
                } else {
                    let account = match &nv.value {
                        syn::Expr::Path(p) => match p.path.get_ident() {
                            Some(ident) => ident.to_string(),
                            None => {
                                return Err(format!(
                                    "`#[{ext_attr}]`: `{key}` must name a consumer account param"
                                ));
                            },
                        },
                        _ => {
                            return Err(format!(
                                "`#[{ext_attr}]`: `{key}` must name a consumer account param"
                            ));
                        },
                    };
                    if role.replace((key, account)).is_some() {
                        return Err(format!(
                            "`#[{ext_attr}]`: more than one embedded role declared"
                        ));
                    }
                }
            },
            syn::Meta::List(l) => {
                return Err(format!(
                    "`#[{ext_attr}]`: unexpected `{}(...)` argument",
                    l.path
                        .get_ident()
                        .map(|i| i.to_string())
                        .unwrap_or_default()
                ));
            },
        }
    }

    args.embed = match (role, offset) {
        (Some((role, account)), Some(offset)) => Some(EmbedDecl {
            role,
            account,
            offset,
        }),
        (None, None) => None,
        (Some((role, _)), None) => {
            return Err(format!(
                "`#[{ext_attr}]`: `{role} = ...` requires an `offset = <bytes>` kwarg"
            ));
        },
        (None, Some(_)) => {
            return Err(format!(
                "`#[{ext_attr}]`: `offset` requires a `<role> = <account>` kwarg"
            ));
        },
    };
    Ok(Some(args))
}

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

    // Parse the first attribute off a module written as tokens.
    fn marker(tokens: &str) -> Attribute {
        let attrs = syn::parse_str::<syn::ItemMod>(&format!("{tokens} mod m {{}}"))
            .unwrap()
            .attrs;
        attrs.into_iter().next().unwrap()
    }

    fn parsed(tokens: &str) -> MarkerArgs {
        parse_marker_args(&marker(tokens), "my_gate")
            .unwrap()
            .unwrap()
    }

    fn parse_err(tokens: &str) -> String {
        parse_marker_args(&marker(tokens), "my_gate").unwrap_err()
    }

    #[test]
    fn marker_args_other_attr_is_none() {
        let attr = marker("#[something_else]");
        assert_eq!(parse_marker_args(&attr, "my_gate").unwrap(), None);
    }

    #[test]
    fn marker_args_bare_is_default() {
        assert_eq!(parsed("#[my_gate]"), MarkerArgs::default());
    }

    #[test]
    fn marker_args_mode_word() {
        let args = parsed("#[my_gate(manual)]");
        assert_eq!(args.word.as_deref(), Some("manual"));
        assert_eq!(args.embed, None);
    }

    #[test]
    fn marker_args_embed_pair() {
        let args = parsed("#[my_gate(gate_config = prog_config, offset = 32)]");
        assert_eq!(args.word, None);
        assert_eq!(
            args.embed,
            Some(EmbedDecl {
                role: "gate_config".to_string(),
                account: "prog_config".to_string(),
                offset: 32,
            })
        );
    }

    #[test]
    fn marker_args_word_and_embed_coexist() {
        let args = parsed("#[my_gate(manual, gate_config = cfg, offset = 8)]");
        assert_eq!(args.word.as_deref(), Some("manual"));
        assert_eq!(
            args.embed,
            Some(EmbedDecl {
                role: "gate_config".to_string(),
                account: "cfg".to_string(),
                offset: 8,
            })
        );
    }

    #[test]
    fn marker_args_kwarg_order_does_not_matter() {
        assert_eq!(
            parsed("#[my_gate(offset = 32, gate_config = prog_config)]"),
            parsed("#[my_gate(gate_config = prog_config, offset = 32)]"),
        );
    }

    #[test]
    fn marker_args_role_without_offset_is_error() {
        let err = parse_err("#[my_gate(gate_config = prog_config)]");
        assert!(err.contains("requires an `offset"), "got: {err}");
    }

    #[test]
    fn marker_args_offset_without_role_is_error() {
        let err = parse_err("#[my_gate(offset = 32)]");
        assert!(err.contains("requires a `<role>"), "got: {err}");
    }

    #[test]
    fn marker_args_two_roles_is_error() {
        let err = parse_err("#[my_gate(gate_config = a, other_config = b, offset = 4)]");
        assert!(err.contains("more than one embedded role"), "got: {err}");
    }

    #[test]
    fn marker_args_two_offsets_is_error() {
        let err = parse_err("#[my_gate(gate_config = a, offset = 4, offset = 8)]");
        assert!(err.contains("more than one `offset`"), "got: {err}");
    }

    #[test]
    fn marker_args_two_mode_words_is_error() {
        let err = parse_err("#[my_gate(manual, strict)]");
        assert!(err.contains("more than one bare mode word"), "got: {err}");
    }

    #[test]
    fn marker_args_non_ident_account_is_error() {
        let err = parse_err(r#"#[my_gate(gate_config = "prog_config", offset = 4)]"#);
        assert!(
            err.contains("must name a consumer account param"),
            "got: {err}"
        );
    }

    #[test]
    fn marker_args_non_int_offset_is_error() {
        let err = parse_err("#[my_gate(gate_config = a, offset = away)]");
        assert!(err.contains("must be an integer literal"), "got: {err}");
    }

    #[test]
    fn marker_args_list_argument_is_error() {
        let err = parse_err("#[my_gate(nested(thing))]");
        assert!(err.contains("unexpected `nested(...)`"), "got: {err}");
    }

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
