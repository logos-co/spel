//! Module-marker attribute handling: the pre-check that gates the
//! metadata walk and the marker argument grammar.

use syn::{punctuated::Punctuated, Attribute};

/// The consumer-facing shorthand for every activated extension's
/// anchor attribute. Inference and the coverage gate accept it, the
/// dispatcher swaps it for the extensions' real attrs.
pub const INITIALIZE_SHORTHAND: &str = "initialize";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OffsetSpec {
    /// Explicit `offset = N` on the marker.
    Literal(usize),
    /// No offset kwarg: derived from the role's `*_slot` field marker.
    Derived,
    /// A resolved derivation: the carrier struct's offset const as a
    /// path, e.g. `ProgConfig::MY_SLOT_OFFSET`. Written only by
    /// `resolve_derived_offsets`, parsed back to tokens at emission.
    Path(String),
}

/// A bound-arg value resolved at discovery time: either a concrete
/// number from an explicit offset kwarg or the declared default, or a
/// derivation the dispatcher lowers to the carrier struct's
/// `<ROLE>_SLOT_OFFSET` const path at emission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundValue {
    Literal(usize),
    Derived { role: String },
    Path(String),
}

impl OffsetSpec {
    /// The offset as an expression: the declared literal, or the
    /// carrier's const path once a derivation is resolved. `subject`
    /// names the role or extension for the unresolved-offset message.
    ///
    /// # Errors
    ///
    /// `Err` when the spec is still `Derived`, which means the
    /// resolution pass never ran, or when a resolved path does not
    /// parse as an expression.
    pub fn to_expr(&self, subject: &str) -> Result<syn::Expr, String> {
        match self {
            OffsetSpec::Literal(n) => Ok(literal_offset_expr(*n)),
            OffsetSpec::Path(p) => const_path_expr(p),
            OffsetSpec::Derived => Err(unresolved_offset(subject)),
        }
    }
}

impl BoundValue {
    /// The bound value as an expression, by the same lowering embedded
    /// offsets take: the dispatcher's call argument and the marker's
    /// gate kwarg can never render one derivation two ways.
    ///
    /// # Errors
    ///
    /// `Err` when the value is still `Derived`, which means the
    /// resolution pass never ran, or when a resolved path does not
    /// parse as an expression.
    pub fn to_expr(&self) -> Result<syn::Expr, String> {
        match self {
            BoundValue::Literal(n) => Ok(literal_offset_expr(*n)),
            BoundValue::Path(p) => const_path_expr(p),
            BoundValue::Derived { role } => Err(unresolved_offset(role)),
        }
    }
}

/// A byte count as an unsuffixed integer expression.
fn literal_offset_expr(n: usize) -> syn::Expr {
    let lit = syn::LitInt::new(&n.to_string(), proc_macro2::Span::call_site());
    syn::parse_quote!(#lit)
}

/// A resolved carrier path (`Cfg::MY_SLOT_OFFSET`) as an expression.
fn const_path_expr(path: &str) -> Result<syn::Expr, String> {
    syn::parse_str(path)
        .map_err(|e| format!("resolved carrier path {path:?} is not an expression: {e}"))
}

fn unresolved_offset(subject: &str) -> String {
    format!(
        "`{subject}` reached emission with an unresolved derived \
        offset; `resolve_derived_offsets` must run after discovery"
    )
}

/// Embedded-mode declaration parsed from a module marker's kwargs,
/// `#[admin_authority(admin_config = prog_config, offset = 32)]`:
/// the inject role `admin_config` lives inside the consumer account
/// `prog_config` at byte offset 32. The offset is `Derived` when the
/// marker omits the kwarg, resolved later from the account struct's
/// `*_slot` field marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedDecl {
    /// Inject-spec account name being relocated (the role).
    pub role: String,
    /// Consumer account the role's slot lives in.
    pub account: String,
    pub offset: OffsetSpec,
    /// The extension's declared initializer attr, `embedded.anchor_attr`.
    /// `Some` only for anchored extensions; the declaration is what
    /// makes the initializer coverage gate mandatory. `None` on the
    /// marker-kwarg path: an anchorless extension declares no
    /// initializer and gets no gate.
    pub initializer: Option<String>,
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
/// without `offset` derives the offset from the account struct's
/// `*_slot` field marker. An `offset` without a role, a second role
/// pair, or a non-ident account value are hard errors.
///
/// # Errors
///
/// `Err` on malformed arguments: two mode words, an offset without a
/// role, duplicate kwargs, a non-ident account value, or a
/// non-integer offset. Callers surface it as a compile error.
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
            offset: OffsetSpec::Literal(offset),
            initializer: None,
        }),
        (Some((role, account)), None) => Some(EmbedDecl {
            role,
            account,
            offset: OffsetSpec::Derived,
            initializer: None,
        }),
        (None, Some(_)) => {
            return Err(format!(
                "`#[{ext_attr}]`: `offset` requires a `<role> = <account>` kwarg"
            ));
        },
        (None, None) => None,
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
        is_marker_candidate(ident.to_string().as_str())
    })
}

/// Idents on the module that could be extension markers, by the same
/// filter the pre-check uses.
pub fn candidate_marker_names(mod_attrs: &[Attribute]) -> Vec<String> {
    mod_attrs
        .iter()
        .filter_map(|a| a.path().get_ident().map(ToString::to_string))
        .filter(|i| is_marker_candidate(i))
        .collect()
}

/// Infer an anchored extension's embedded declaration from the module.
///
/// The anchor attr marks the consumer fn that creates the embedding
/// account. Its optional `<role> = <param>` kwarg names the account
/// among several `#[account(init)]` params; with exactly one init
/// param the kwarg may be omitted. No fn carrying the attr means
/// dedicated mode.
///
/// # Errors
///
/// `Err` when two fns carry the anchor, when the kwarg names anything
/// but an init param, or when several init params exist and no kwarg
/// picks one. Callers surface it as a compile error.
pub(super) fn infer_anchor_embed(
    mod_items: &[syn::Item],
    anchor_attr: &str,
    role: &str,
    crate_name: &str,
) -> Result<Option<EmbedDecl>, String> {
    let fail = |what: String| format!("extension `{crate_name}`: {what}");

    let mut anchors: Vec<(&syn::ItemFn, &Attribute)> = Vec::new();
    for item in mod_items {
        let syn::Item::Fn(f) = item else { continue };
        let explicit = f.attrs.iter().find(|a| attr_is(a, anchor_attr));
        let shorthand = f.attrs.iter().find(|a| attr_is(a, INITIALIZE_SHORTHAND));
        match (explicit, shorthand) {
            (Some(_), Some(_)) => {
                return Err(fail(format!(
                    "fn `{}` carries both #[{anchor_attr}] and \
                    #[{INITIALIZE_SHORTHAND}]; one spelling per fn",
                    f.sig.ident
                )));
            },
            (Some(a), None) | (None, Some(a)) => anchors.push((f, a)),
            (None, None) => {},
        }
    }

    let (func, attr) = match anchors.as_slice() {
        [] => return Ok(None),
        [one] => *one,
        many => {
            let names: Vec<String> = many.iter().map(|(f, _)| f.sig.ident.to_string()).collect();
            return Err(fail(format!(
                "#[{anchor_attr}] appears on {} fns ({}); exactly one fn may \
                anchor the embed",
                many.len(),
                names.join(", ")
            )));
        },
    };

    let is_shorthand = attr_is(attr, INITIALIZE_SHORTHAND);
    if is_shorthand && !matches!(attr.meta, syn::Meta::Path(_)) {
        return Err(fail(format!(
            "#[{INITIALIZE_SHORTHAND}] takes no arguments; to name the \
            embedding account use #[{anchor_attr}({role} = <param>)]"
        )));
    }

    let init_params: Vec<&syn::Ident> = super::inject::typed_params(func)
        .filter(|(_, pt)| super::inject::param_has_init(pt))
        .map(|(pi, _)| &pi.ident)
        .collect();

    let account = match (
        if is_shorthand {
            None
        } else {
            anchor_kwarg(attr, anchor_attr, role)?
        },
        init_params.as_slice(),
    ) {
        (Some(name), inits) if inits.iter().any(|i| **i == name) => name,
        (Some(name), _) => {
            return Err(fail(format!(
                "#[{anchor_attr}({role} = {name})] on fn `{}` names no \
                #[account(init)] param; the embedding account must be created \
                by this fn",
                func.sig.ident
            )));
        },
        (None, [one]) => one.to_string(),
        (None, []) => {
            return Err(fail(format!(
                "#[{anchor_attr}] on fn `{}` has no #[account(init)] param; \
                the anchor fn creates the embedding account",
                func.sig.ident
            )));
        },
        (None, several) => {
            let names: Vec<String> = several.iter().map(ToString::to_string).collect();
            return Err(fail(format!(
                "#[{anchor_attr}] on fn `{}` has several #[account(init)] \
                params ({}); name the embedding account with \
                #[{anchor_attr}({role} = <param>)]",
                func.sig.ident,
                names.join(", ")
            )));
        },
    };

    Ok(Some(EmbedDecl {
        role: role.to_string(),
        account,
        offset: OffsetSpec::Derived,
        initializer: Some(anchor_attr.to_string()),
    }))
}

fn is_marker_candidate(ident: &str) -> bool {
    !matches!(
        ident,
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
}

/// The attr's last path segment equals `name`, matching the bare
/// re-export form and qualified `admin_authority::admin_initialize`.
pub(super) fn attr_is(attr: &Attribute, name: &str) -> bool {
    attr.path().segments.last().is_some_and(|s| s.ident == name)
}

/// The located anchor attr's optional `<role> = <param>` kwarg. Other
/// kwargs pass through untouched, the gate machinery owns them.
fn anchor_kwarg(attr: &Attribute, anchor_attr: &str, role: &str) -> Result<Option<String>, String> {
    if matches!(attr.meta, syn::Meta::Path(_)) {
        return Ok(None);
    }
    let metas = attr
        .parse_args_with(Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated)
        .map_err(|e| format!("`#[{anchor_attr}]`: unparsable arguments: {e}"))?;
    for m in &metas {
        let syn::Meta::NameValue(nv) = m else {
            continue;
        };
        if !nv.path.is_ident(role) {
            continue;
        }
        let ident = match &nv.value {
            syn::Expr::Path(p) => p.path.get_ident(),
            _ => None,
        };
        return match ident {
            Some(id) => Ok(Some(id.to_string())),
            None => Err(format!(
                "#[{anchor_attr}({role} = ...)]: the value must be a \
                plain param name"
            )),
        };
    }
    Ok(None)
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
                offset: OffsetSpec::Literal(32),
                initializer: None,
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
                offset: OffsetSpec::Literal(8),
                initializer: None,
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
    fn marker_args_role_without_offset_derives() {
        let args = parsed("#[my_gate(gate_config = prog_config)]");
        assert_eq!(
            args.embed,
            Some(EmbedDecl {
                role: "gate_config".into(),
                account: "prog_config".into(),
                offset: OffsetSpec::Derived,
                initializer: None,
            })
        );
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

    #[test]
    fn candidate_names_keep_markers_and_drop_standard_attrs() {
        let m: syn::ItemMod = syn::parse_quote! {
            #[lez_program]
            #[doc = "x"]
            #[allow(dead_code)]
            #[my_ext]
            #[freeze_authority(manual)]
            mod program {}
        };
        assert_eq!(
            candidate_marker_names(&m.attrs),
            vec!["my_ext".to_string(), "freeze_authority".to_string()]
        );
    }

    fn mod_fns(src: &str) -> Vec<syn::Item> {
        syn::parse_file(src).expect("fixture parses").items
    }

    #[test]
    fn anchor_with_single_init_infers_the_account() {
        let items = mod_fns(
            "#[ext_init]\npub fn initialize(#[account(init, pda = literal(\"cfg\"))] cfg: A, #[account(signer)] s: A) -> R { todo!() }",
        );
        let embed = infer_anchor_embed(&items, "ext_init", "ext_config", "my-ext")
            .unwrap()
            .expect("one init param infers");
        assert_eq!(embed.role, "ext_config");
        assert_eq!(embed.account, "cfg");
        assert_eq!(embed.offset, OffsetSpec::Derived);
    }

    #[test]
    fn anchor_kwarg_picks_among_several_inits() {
        let items = mod_fns(
            "#[ext_init(ext_config = vault)]\npub fn initialize(#[account(init)] cfg: A, #[account(init)] vault: A) -> R { todo!() }",
        );
        let embed = infer_anchor_embed(&items, "ext_init", "ext_config", "my-ext")
            .unwrap()
            .expect("the kwarg picks");
        assert_eq!(embed.account, "vault");
    }

    #[test]
    fn anchor_kwarg_naming_non_init_param_refuses() {
        let items = mod_fns(
            "#[ext_init(ext_config = s)]\npub fn initialize(#[account(init)] cfg: A, #[account(signer)] s: A) -> R { todo!() }",
        );
        let err = infer_anchor_embed(&items, "ext_init", "ext_config", "my-ext")
            .expect_err("a non-init kwarg target must refuse");
        assert!(err.contains("names no #[account(init)]"), "{err}");
    }

    #[test]
    fn several_inits_without_kwarg_refuse_listing_candidates() {
        let items = mod_fns(
            "#[ext_init]\npub fn initialize(#[account(init)] cfg: A, #[account(init)] vault: A) -> R { todo!() }",
        );
        let err = infer_anchor_embed(&items, "ext_init", "ext_config", "my-ext")
            .expect_err("ambiguity must refuse");
        assert!(err.contains("cfg") && err.contains("vault"), "{err}");
        assert!(
            err.contains("ext_config = "),
            "must show the kwarg form: {err}"
        );
    }

    #[test]
    fn anchor_without_init_param_refuses() {
        let items =
            mod_fns("#[ext_init]\npub fn initialize(#[account(signer)] s: A) -> R { todo!() }");
        let err = infer_anchor_embed(&items, "ext_init", "ext_config", "my-ext")
            .expect_err("an anchor that creates nothing must refuse");
        assert!(err.contains("no #[account(init)] param"), "{err}");
    }

    #[test]
    fn two_anchor_fns_refuse_naming_both() {
        let items = mod_fns(
            "#[ext_init]\npub fn a(#[account(init)] cfg: A) -> R { todo!() }\n#[ext_init]\npub fn b(#[account(init)] cfg: A) -> R { todo!() }",
        );
        let err = infer_anchor_embed(&items, "ext_init", "ext_config", "my-ext")
            .expect_err("two anchors must refuse");
        assert!(err.contains('a') && err.contains('b'), "{err}");
    }

    #[test]
    fn no_anchor_fn_is_dedicated() {
        let items = mod_fns("pub fn plain(#[account(init)] cfg: A) -> R { todo!() }");
        let embed = infer_anchor_embed(&items, "ext_init", "ext_config", "my-ext").unwrap();
        assert!(embed.is_none(), "no anchor means dedicated mode");
    }

    // #[initialize] counts as the anchor of every anchored extension,
    // and the recorded initializer is the extension's real attr, so
    // downstream errors and the coverage gate speak real names.
    #[test]
    fn initialize_shorthand_anchors_the_embed() {
        let items = mod_fns(
            "#[initialize]\npub fn initialize(#[account(init, pda = literal(\"cfg\"))] cfg: A, #[account(signer)] s: A) -> R { todo!() }",
        );
        let embed = infer_anchor_embed(&items, "ext_init", "ext_config", "my-ext")
            .unwrap()
            .expect("the shorthand anchors");
        assert_eq!(embed.account, "cfg");
        assert_eq!(embed.initializer.as_deref(), Some("ext_init"));
    }

    #[test]
    fn shorthand_beside_the_explicit_anchor_refuses() {
        let items = mod_fns(
            "#[ext_init]\n#[initialize]\npub fn initialize(#[account(init)] cfg: A) -> R { todo!() }",
        );
        let err = infer_anchor_embed(&items, "ext_init", "ext_config", "my-ext")
            .expect_err("one spelling per fn");
        assert!(err.contains("both"), "{err}");
    }

    #[test]
    fn shorthand_with_arguments_refuses() {
        let items = mod_fns(
            "#[initialize(ext_config = cfg)]\npub fn initialize(#[account(init)] cfg: A) -> R { todo!() }",
        );
        let err = infer_anchor_embed(&items, "ext_init", "ext_config", "my-ext")
            .expect_err("the shorthand takes no arguments");
        assert!(
            err.contains("#[ext_init(ext_config = <param>)]"),
            "the fix is the explicit form: {err}"
        );
    }

    // Several init params under the shorthand fall into the existing
    // ambiguity error, whose fix is the explicit anchor kwarg.
    #[test]
    fn shorthand_with_several_inits_names_the_explicit_form() {
        let items = mod_fns(
            "#[initialize]\npub fn initialize(#[account(init)] cfg: A, #[account(init)] vault: A) -> R { todo!() }",
        );
        let err = infer_anchor_embed(&items, "ext_init", "ext_config", "my-ext")
            .expect_err("ambiguity must refuse");
        assert!(
            err.contains("ext_config = "),
            "must show the explicit kwarg form: {err}"
        );
    }
}
