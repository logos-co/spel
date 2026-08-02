//! Wrap application and gate-param injection: the shared pass that
//! prepends wrapper attrs and synthesizes missing gate accounts, used
//! identically by the dispatcher expansion and both IDL paths.

use std::collections::HashMap;

use syn::{parse_quote, Attribute, FnArg, ItemFn};

use super::{InjectAccount, InjectSeed, InjectSpec, WrapInstructions};

/// Filter `deps.extensions.wraps` down to the wraps whose extension
/// marker carries no skip-word arg matching `WrapInstructions::skip`.
///
/// A wrap with `skip = Some("manual")` is dropped for a marker written
/// `#[freeze_authority(manual)]`, kept for `#[freeze_authority]`. A wrap
/// with `skip = None` is always kept.
pub fn active_wraps(wraps: &[(String, WrapInstructions)]) -> Vec<WrapInstructions> {
    wraps
        .iter()
        .filter(|(arg, wrap)| match &wrap.skip {
            Some(s) => arg != s,
            None => true,
        })
        .map(|(_, wrap)| wrap.clone())
        .collect()
}

/// The shared gate pass, four phases in order: substitute embedded
/// role params on discovered fns, prepend each active wrap's
/// attribute, inject missing gate params, then stamp authored bare
/// gates with the framework's location kwargs. Shared between the
/// program-macro dispatcher and both IDL paths so all three see the
/// same accounts. Injection reads the authored attr before stamping,
/// so consumer-authored args disable injection and framework-stamped
/// args never do.
///
/// `qualified = None` for consumer-authored fns: only the per-fn
/// `self_exempt_marker` opts out. `Some("crate::fn_name")` for
/// extension-provided fns: the `exempt` qualified-name list is also
/// consulted so a wrap can carve out an extension it depends on, and
/// embedded role substitution applies only on this path.
///
/// Returns the names of the params `inject_gate_params` synthesized.
///
/// # Errors
///
/// Propagates `inject_gate_params` errors and fails when a declared
/// wrapper path is not a valid Rust attribute path and when a
/// consumer-authored gate attr carries a location kwarg in embedded
/// mode.
pub fn apply_wrap_and_inject(
    func: &mut ItemFn,
    active_wraps: &[WrapInstructions],
    inject_specs: &[InjectSpec],
    embeds: &[(String, super::EmbedDecl)],
    qualified: Option<&str>,
) -> Result<Vec<String>, String> {
    if qualified.is_some() {
        substitute_embedded_params(func, inject_specs);
    }

    let remap = build_remap(inject_specs, func);
    let offset_by_source: HashMap<&str, usize> = embeds
        .iter()
        .map(|(source, e)| (source.as_str(), e.offset))
        .collect();
    check_authored_location_kwargs(func, inject_specs, &offset_by_source)?;

    for wrap in active_wraps {
        let exempt = func
            .attrs
            .iter()
            .any(|a| a.path().is_ident(&wrap.self_exempt_marker))
            || qualified
                .map(|q| wrap.exempt.iter().any(|e| e == q))
                .unwrap_or(false);
        if exempt {
            continue;
        }
        let wrapper_path: syn::Path = syn::parse_str(&wrap.wrapper)
            .map_err(|e| format!("invalid wrapper path {:?}: {e}", wrap.wrapper))?;
        let wrapper_last = wrapper_path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_default();

        let mut args: Vec<syn::MetaNameValue> = Vec::new();
        for spec in inject_specs {
            if spec.wrapper != wrapper_last {
                continue;
            }
            args.extend(spec_gate_args(
                spec,
                &remap,
                offset_by_source.get(spec.source.as_str()).copied(),
            ));
        }
        let attr: Attribute = if args.is_empty() {
            parse_quote! { #[#wrapper_path] }
        } else {
            parse_quote! { #[#wrapper_path(#(#args), *)] }
        };
        func.attrs.insert(0, attr);
    }

    let injected = inject_gate_params(func, inject_specs)?;

    stamp_authored_gates(func, inject_specs, &remap, &offset_by_source);

    Ok(injected)
}

/// Resolve the canonical PDA constraint for an embedded-mode account.
///
/// The canonical declaration is the consumer's account-creating one,
/// `#[account(init, pda = ...)]` on a param named account. Every
/// other declaration of that name carrying a `pda` constraint must
/// agree with it structurally.
///
/// # Errors
///
/// `Err` when no declaration carries `init` plus a `pda` constraint,
/// or when two declarations disagree, naming both fns. Callers
/// surface it as a compile error.
pub fn resolve_canonical_constraint(fns: &[ItemFn], account: &str) -> Result<syn::Expr, String> {
    struct Decl {
        fn_name: String,
        has_init: bool,
        pda: syn::Expr,
    }
    let mut decls: Vec<Decl> = Vec::new();

    for func in fns {
        for (pi, pt) in typed_params(func) {
            if pi.ident != account {
                continue;
            }
            for attr in &pt.attrs {
                if !attr.path().is_ident("account") {
                    continue;
                }
                let mut has_init = false;
                let mut pda: Option<syn::Expr> = None;
                attr.parse_nested_meta(|meta| {
                    if meta.path.is_ident("init") {
                        has_init = true;
                    } else if meta.path.is_ident("pda") {
                        pda = Some(meta.value()?.parse()?);
                    }
                    Ok(())
                })
                .ok();
                if let Some(pda) = pda {
                    decls.push(Decl {
                        fn_name: func.sig.ident.to_string(),
                        has_init,
                        pda,
                    });
                }
            }
        }
    }

    let Some(canonical) = decls.iter().find(|d| d.has_init) else {
        return Err(format!(
            "embedded account `{account}` has no canonical declaration: the \
            account-creating instruction must declare it with \
            `#[account(init, pda = ...)]`"
        ));
    };
    for d in &decls {
        if d.pda != canonical.pda {
            return Err(format!(
                "embedded account `{account}` is declared with conflicting pda \
                constraints: in `{}` and in `{}`",
                canonical.fn_name, d.fn_name
            ));
        }
    }

    Ok(canonical.pda.clone())
}

/// Rewrite each embedded role's inject entry: the role's account is
/// replaced by the consumer's embedding account, name and canonical
/// constraint. Runs before any wrap or inject pass so all three
/// producers see the rewritten specs.
///
/// # Errors
///
/// `Err` when a declared role matches no inject account of the
/// declaring extension, when the canonical constraint is missing or
/// conflicting ([`resolve_canonical_constraint`]), or when the
/// constraint is not `literal()`/`account()` seed shaped. Callers
/// surface it as a compile error.
pub fn rewrite_embedded_roles(
    specs: &mut [InjectSpec],
    embeds: &[(String, super::EmbedDecl)],
    consumer_fns: &[ItemFn],
) -> Result<(), String> {
    for (source, embed) in embeds {
        let canonical = resolve_canonical_constraint(consumer_fns, &embed.account)?;
        let seeds = expr_to_seeds(&canonical).ok_or_else(|| {
            format!(
                "embedded account `{}`: canonical constraint is not a \
                literal()/account() seed expression",
                embed.account
            )
        })?;

        check_initializer_coverage(specs, source, embed, consumer_fns)?;

        let mut hit = false;
        for spec in specs.iter_mut().filter(|s| &s.source == source) {
            for acc in spec.accounts.iter_mut().filter(|a| a.role == embed.role) {
                acc.name = embed.account.clone();
                acc.seeds = seeds.clone();
                acc.embedded = true;
                hit = true;
            }
        }
        if !hit {
            return Err(format!(
                "extension `{source}` declares no inject account named \
                `{}`; the marker kwarg must name one of the extension's \
                inject roles",
                embed.role
            ));
        }
    }

    check_embed_window_collisions(embeds)?;
    Ok(())
}

/// An extension that declares an initializer wrapper makes it
/// mandatory: every instruction that creates the embedding account must
/// carry it, or the program ships born renounced. Extensions without
/// one (freeze is born vacant by design) are untouched.
fn check_initializer_coverage(
    specs: &[InjectSpec],
    source: &str,
    embed: &super::EmbedDecl,
    consumer_fns: &[ItemFn],
) -> Result<(), String> {
    let prefix = embed.role.strip_suffix("_config").unwrap_or(&embed.role);
    let init_attr = format!("{prefix}_initialize");
    let declares_initializer = specs
        .iter()
        .any(|s| s.source == source && s.wrapper == init_attr);
    if !declares_initializer {
        return Ok(());
    }
    for func in consumer_fns {
        let creates = typed_params(func)
            .any(|(pi, pt)| pi.ident == embed.account.as_str() && param_has_init(pt));
        let annotated = func.attrs.iter().any(|a| {
            a.path()
                .segments
                .last()
                .is_some_and(|s| s.ident == init_attr.as_str())
        });

        if !creates && !annotated {
            continue;
        }
        if creates && !annotated {
            return Err(format!(
                "`{}` creates the embedding account `{}` and must carry \
                #[{init_attr}]; without it the program ships born renounced",
                func.sig.ident, embed.account
            ));
        }
        if annotated && !creates {
            return Err(format!(
                "`{}` carries #[{init_attr}] but does not declare `{}` with \
                 #[account(init, ...)]; the bootstrap must only ever run on \
                 a freshly created account",
                func.sig.ident, embed.account
            ));
        }
    }
    Ok(())
}

/// Reject two embeds sharing an account at the same offset: identical
/// windows cannot both hold state. Distinct offsets on one account are
/// the intended shared-account layout.
fn check_embed_window_collisions(embeds: &[(String, super::EmbedDecl)]) -> Result<(), String> {
    for (i, (source_a, a)) in embeds.iter().enumerate() {
        for (source_b, b) in &embeds[i + 1..] {
            if a.account == b.account && a.offset == b.offset {
                return Err(format!(
                    "extensions `{source_a}` and `{source_b}` both embed into \
                    account `{}` at offset {}; overlapping windows cannot both \
                    hold state, declare distinct offsets",
                    a.account, a.offset
                ));
            }
        }
    }
    Ok(())
}

/// Inject a wrapper's missing gate params into an instruction fn.
///
/// Skip-if-declared: a param that exists is never touched, and the
/// role remap reuses consumer params (signer, literal PDA, compound
/// PDA) under their declared names. Bare and args forms of the gate
/// attr both activate injection: kwargs rename the gate's targets but
/// never disable synthesis of params the fn lacks, per the ADR-0010
/// kwarg contract that superseded the old args-form-is-manual rule.
/// The wrapper is matched by the attr path's last segment, so the
/// fully qualified attrs prepended by auto-wrap activate specs too.
/// Returns the names actually injected.
///
/// When two specs want the same param name: identical constraints share
/// one account at the first injector's position, the cheap shared-signer
/// ABI. Conflicting constraints are a hard error naming both extensions.
///
/// # Errors
///
/// `Err` when two extensions inject the same param name with different
/// constraints; callers surface it as a compile error.
fn inject_gate_params(func: &mut ItemFn, specs: &[InjectSpec]) -> Result<Vec<String>, String> {
    let remap = build_remap(specs, func);
    let mut injected = Vec::new();
    let mut injected_by: HashMap<String, (&InjectAccount, &str)> = HashMap::new();
    let mut pos = insert_position(func);

    for spec in specs {
        if !spec_activates(spec, func) {
            continue;
        }

        for acc in &spec.accounts {
            let effective = remap
                .get(&acc.name)
                .cloned()
                .unwrap_or_else(|| acc.name.clone());
            if let Some((existing, source)) = injected_by.get(effective.as_str()) {
                if *existing == acc {
                    continue; // identical constraints: one shared account
                }
                return Err(format!(
                    "extension '{source}' and '{}' both inject param '{}' with \
                    conflicting constraints",
                    spec.source, effective
                ));
            }
            if has_param_named(func, &effective) {
                continue; // consumer declared it: declared win
            }
            func.sig.inputs.insert(pos, build_inject_param(acc, &remap));
            pos += 1;
            injected.push(acc.name.clone());
            injected_by.insert(effective.clone(), (acc, spec.source.as_str()));
        }
    }
    Ok(injected)
}

fn build_remap(specs: &[InjectSpec], func: &ItemFn) -> HashMap<String, String> {
    let existing_signer = find_signer_param(func);
    let mut remap: HashMap<String, String> = HashMap::new();

    for spec in specs {
        for acc in &spec.accounts {
            if acc.signer {
                if let Some(existing) = &existing_signer {
                    if existing != &acc.name {
                        remap.insert(acc.name.clone(), existing.clone());
                    }
                }
            } else if let [InjectSeed::Const(literal)] = acc.seeds.as_slice() {
                if let Some(existing) = find_pda_literal_param(func, literal) {
                    if existing != acc.name {
                        remap.insert(acc.name.clone(), existing.clone());
                    }
                }
            }
        }
    }

    for spec in specs {
        for acc in &spec.accounts {
            if acc.seeds.len() >= 2 {
                if let Some(existing) = find_pda_compound_param(func, &acc.seeds, &remap) {
                    if existing != acc.name {
                        remap.insert(acc.name.clone(), existing);
                    }
                }
            }
        }
    }

    remap
}

fn spec_activates(spec: &InjectSpec, func: &ItemFn) -> bool {
    func.attrs.iter().any(|a| {
        a.path()
            .segments
            .last()
            .is_some_and(|s| s.ident == spec.wrapper)
            && matches!(a.meta, syn::Meta::Path(_) | syn::Meta::List(_))
    })
}

/// True if the fn already declares a param with this name.
fn has_param_named(func: &ItemFn, name: &str) -> bool {
    func.sig.inputs.iter().any(|input| {
        matches!(input, FnArg::Typed(pt)
            if matches!(&*pt.pat, syn::Pat::Ident(pi) if pi.ident == name))
    })
}

fn find_signer_param(func: &ItemFn) -> Option<String> {
    let mut found: Option<String> = None;
    for (_, pt) in typed_params(func) {
        let is_signer = pt.attrs.iter().any(|a| {
            if !a.path().is_ident("account") {
                return false;
            }
            let mut hit = false;
            a.parse_nested_meta(|meta| {
                if meta.path.is_ident("signer") {
                    hit = true;
                }
                Ok(())
            })
            .ok();
            hit
        });
        if !is_signer {
            continue;
        }
        let syn::Pat::Ident(pi) = &*pt.pat else {
            continue;
        };
        if found.is_some() {
            return None;
        }
        found = Some(pi.ident.to_string());
    }
    found
}

fn find_pda_literal_param(func: &ItemFn, literal: &str) -> Option<String> {
    let target = vec![InjectSeed::Const(literal.to_string())];
    typed_params(func)
        .find(|(_, pt)| param_pda_seeds(pt).is_some_and(|s| s == target))
        .map(|(pi, _)| pi.ident.to_string())
}

fn find_pda_compound_param(
    func: &ItemFn,
    seeds: &[InjectSeed],
    remap: &HashMap<String, String>,
) -> Option<String> {
    let target: Vec<InjectSeed> = seeds
        .iter()
        .map(|s| match s {
            InjectSeed::Const(v) => InjectSeed::Const(v.clone()),
            InjectSeed::Account(v) => {
                InjectSeed::Account(remap.get(v).cloned().unwrap_or_else(|| v.clone()))
            },
        })
        .collect();
    typed_params(func)
        .find(|(_, pt)| param_pda_seeds(pt).is_some_and(|s| s == target))
        .map(|(pi, _)| pi.ident.to_string())
}

// One `literal("x")` / `account("y")` call as a structured seed.
fn seed_call_to_seed(expr: &syn::Expr) -> Option<InjectSeed> {
    let syn::Expr::Call(call) = expr else {
        return None;
    };
    let syn::Expr::Path(p) = &*call.func else {
        return None;
    };
    let arg = call.args.first()?;
    let syn::Expr::Lit(lit_expr) = arg else {
        return None;
    };
    let syn::Lit::Str(s) = &lit_expr.lit else {
        return None;
    };
    if p.path.is_ident("literal") {
        Some(InjectSeed::Const(s.value()))
    } else if p.path.is_ident("account") {
        Some(InjectSeed::Account(s.value()))
    } else {
        None
    }
}

/// Injected params go after a leading ProgramContext, else at the front.
fn insert_position(func: &ItemFn) -> usize {
    if let Some(FnArg::Typed(pt)) = func.sig.inputs.first() {
        if let syn::Type::Path(p) = &*pt.ty {
            if p.path
                .segments
                .last()
                .is_some_and(|s| s.ident == "ProgramContext")
            {
                return 1;
            }
        }
    }
    0
}

// Render one injected account as a typed fn param carrying its
// `#[account(...)]` constraint.
fn build_inject_param(acc: &InjectAccount, remap: &HashMap<String, String>) -> FnArg {
    let ident = syn::Ident::new(&acc.name, proc_macro2::Span::call_site());
    let seed_exprs: Vec<syn::Expr> = acc
        .seeds
        .iter()
        .map(|s| match s {
            InjectSeed::Const(v) => {
                let lit = syn::LitStr::new(v, proc_macro2::Span::call_site());
                parse_quote! { literal(#lit) }
            },
            InjectSeed::Account(v) => {
                let name = remap.get(v).unwrap_or(v);
                let lit = syn::LitStr::new(name, proc_macro2::Span::call_site());
                parse_quote! { account(#lit) }
            },
        })
        .collect();
    match (&seed_exprs[..], acc.signer) {
        ([], true) => parse_quote! { #[account(signer)] #ident: AccountWithMetadata },
        ([], false) => parse_quote! { #ident: AccountWithMetadata },
        ([single], _) => parse_quote! { #[account(pda = #single)] #ident: AccountWithMetadata },
        (multi, _) => parse_quote! { #[account(pda = [#(#multi),*])] #ident: AccountWithMetadata },
    }
}

/// Parse a consumer-declared `pda` expression back into structured
/// seeds. `literal("x")` and `account("y")` calls, alone or in an
/// array. `None` for any other shape.
fn expr_to_seeds(expr: &syn::Expr) -> Option<Vec<InjectSeed>> {
    let elems: Vec<&syn::Expr> = match expr {
        syn::Expr::Array(arr) => arr.elems.iter().collect(),
        single => vec![single],
    };
    elems.into_iter().map(seed_call_to_seed).collect()
}

/// Kwargs a gate attr carries for  `spec`: `role = resolved_name` per
/// account, plus the extension's embedded offset when one is declared
fn spec_gate_args(
    spec: &InjectSpec,
    remap: &HashMap<String, String>,
    offset: Option<usize>,
) -> Vec<syn::MetaNameValue> {
    let mut args = Vec::new();
    for acc in &spec.accounts {
        let resolved = remap
            .get(&acc.name)
            .cloned()
            .unwrap_or_else(|| acc.name.clone());
        let key = syn::Ident::new(&acc.role, proc_macro2::Span::call_site());
        let val = syn::Ident::new(&resolved, proc_macro2::Span::call_site());
        args.push(parse_quote! { #key = #val});
    }
    if let Some(off) = offset {
        let lit = syn::LitInt::new(&off.to_string(), proc_macro2::Span::call_site());
        args.push(parse_quote! { offset = #lit });
    }
    args
}

// Rewrite consumer-authored bare gate attrs to carry the framework's
// location kwargs. Embedded mode is the only writer of those kwargs
// and stamps every gate. Runs after the injection pass so the
// bare-attr-activates-injection rule saw the authored form.
fn stamp_authored_gates(
    func: &mut ItemFn,
    inject_specs: &[InjectSpec],
    remap: &HashMap<String, String>,
    offset_by_source: &HashMap<&str, usize>,
) {
    for attr in func.attrs.iter_mut() {
        if !matches!(attr.meta, syn::Meta::Path(_)) {
            continue;
        }
        let Some(last) = attr.path().segments.last().map(|s| s.ident.to_string()) else {
            continue;
        };
        for spec in inject_specs {
            if spec.wrapper != last {
                continue;
            }
            let Some(off) = offset_by_source.get(spec.source.as_str()) else {
                continue;
            };
            let args = spec_gate_args(spec, remap, Some(*off));
            let path = attr.path().clone();
            *attr = parse_quote! { #[#path(#(#args),*)] };
        }
    }
}

// Named, typed params of a fn: the only shape gate machinery reads.
fn typed_params(func: &ItemFn) -> impl Iterator<Item = (&syn::PatIdent, &syn::PatType)> {
    func.sig.inputs.iter().filter_map(|input| match input {
        FnArg::Typed(pt) => match &*pt.pat {
            syn::Pat::Ident(pi) => Some((pi, pt)),
            _ => None,
        },
        _ => None,
    })
}

// The declared pda seeds of a param, when its `#[account]` attr
// carries a pda constraint in the literal()/account() grammar.
fn param_pda_seeds(pt: &syn::PatType) -> Option<Vec<InjectSeed>> {
    for attr in &pt.attrs {
        if !attr.path().is_ident("account") {
            continue;
        }
        let mut pda: Option<syn::Expr> = None;
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("pda") {
                pda = Some(meta.value()?.parse()?);
            }
            Ok(())
        })
        .ok();
        if let Some(expr) = pda {
            return expr_to_seeds(&expr);
        }
    }
    None
}

// True when the param's `#[account]` attr carries `init`.
fn param_has_init(pt: &syn::PatType) -> bool {
    pt.attrs.iter().any(|attr| {
        if !attr.path().is_ident("account") {
            return false;
        }
        let mut has_init = false;
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("init") {
                has_init = true;
            }
            Ok(())
        })
        .ok();
        has_init
    })
}

// Render structured seeds back into a `pda = ...` expression:
// a bare call for one seed, an array for a compound.
fn seeds_to_pda_expr(seeds: &[InjectSeed]) -> Option<syn::Expr> {
    let exprs: Vec<syn::Expr> = seeds
        .iter()
        .map(|s| match s {
            InjectSeed::Const(v) => {
                let lit = syn::LitStr::new(v, proc_macro2::Span::call_site());
                parse_quote! { literal(#lit) }
            },
            InjectSeed::Account(v) => {
                let lit = syn::LitStr::new(v, proc_macro2::Span::call_site());
                parse_quote! { account(#lit) }
            },
        })
        .collect();
    match &exprs[..] {
        [] => None,
        [single] => Some(single.clone()),
        multi => Some(parse_quote! { [#(#multi),*] }),
    }
}

/// Retarget a discovered fn's embedded role params: a param named
/// after a rewritten role (role differs from name only after an
/// embedded rewrite) takes the consumer account's name and canonical
/// constraint. `mut` on the account attr is preserved; other flags
/// are not carried yet, the embedded role grammar is `mut` + `pda`.
fn substitute_embedded_params(func: &mut ItemFn, inject_specs: &[InjectSpec]) {
    for spec in inject_specs {
        for acc in &spec.accounts {
            if !acc.embedded {
                continue;
            }
            let Some(pda_expr) = seeds_to_pda_expr(&acc.seeds) else {
                continue;
            };
            for input in func.sig.inputs.iter_mut() {
                let FnArg::Typed(pt) = input else {
                    continue;
                };
                let syn::Pat::Ident(pi) = &mut *pt.pat else {
                    continue;
                };
                if pi.ident != acc.role {
                    continue;
                }
                pi.ident = syn::Ident::new(&acc.name, pi.ident.span());
                for attr in pt.attrs.iter_mut() {
                    if !attr.path().is_ident("account") {
                        continue;
                    }
                    let mut is_mut = false;
                    attr.parse_nested_meta(|meta| {
                        if meta.path.is_ident("mut") {
                            is_mut = true;
                        } else if meta.path.is_ident("pda") {
                            let _: syn::Expr = meta.value()?.parse()?;
                        }
                        Ok(())
                    })
                    .ok();
                    *attr = if is_mut {
                        parse_quote! { #[account(mut, pda = #pda_expr)] }
                    } else {
                        parse_quote! { #[account(pda = #pda_expr)] }
                    };
                }
            }
        }
    }
}

/// Embedded mode: the framework is the only writer of location
/// kwargs. A consumer-authored gate attr naming the embedded role or
/// `offset` could only contradict the program-wide marker
/// declaration, so it is rejected. Signer-role kwargs stay allowed,
/// and dedicated-mode manual kwargs are untouched
fn check_authored_location_kwargs(
    func: &ItemFn,
    inject_specs: &[InjectSpec],
    offset_by_source: &HashMap<&str, usize>,
) -> Result<(), String> {
    for attr in &func.attrs {
        if !matches!(attr.meta, syn::Meta::List(_)) {
            continue;
        }
        let Some(last) = attr.path().segments.last().map(|s| s.ident.to_string()) else {
            continue;
        };
        for spec in inject_specs {
            if spec.wrapper != last || !offset_by_source.contains_key(spec.source.as_str()) {
                continue;
            }
            let mut offending: Option<String> = None;
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("offset") {
                    offending = Some("offset".to_string());
                } else {
                    for acc in &spec.accounts {
                        if !acc.signer && meta.path.is_ident(&acc.role) {
                            offending = Some(acc.role.clone());
                        }
                    }
                }
                if meta.input.peek(syn::Token![=]) {
                    let _: syn::Expr = meta.value()?.parse()?;
                }
                Ok(())
            })
            .ok();
            if let Some(key) = offending {
                return Err(format!(
                    "`{}` on `{}`: the `{key}` kwarg is framework-written in \
                    embedded mode, remove it; the module marker declares the \
                    slot location",
                    spec.wrapper, func.sig.ident
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Parse the fn items out of a source string.
    fn fns(src: &str) -> Vec<ItemFn> {
        syn::parse_file(src)
            .unwrap()
            .items
            .into_iter()
            .filter_map(|i| match i {
                syn::Item::Fn(f) => Some(f),
                _ => None,
            })
            .collect()
    }

    fn expr(src: &str) -> syn::Expr {
        syn::parse_str(src).unwrap()
    }

    // A rewritten spec plus its embed decl: embedded mode for `my_ext`.
    fn embedded_fixture() -> (Vec<InjectSpec>, Vec<(String, crate::extension::EmbedDecl)>) {
        let specs = vec![InjectSpec {
            wrapper: "my_gate".to_string(),
            accounts: vec![
                InjectAccount {
                    name: "prog_config".to_string(),
                    role: "gate_config".to_string(),
                    seeds: vec![InjectSeed::Const("prog_config".to_string())],
                    signer: false,
                    embedded: false,
                },
                InjectAccount {
                    name: "caller".to_string(),
                    role: "caller".to_string(),
                    seeds: vec![],
                    signer: true,
                    embedded: false,
                },
            ],
            source: "my_ext".to_string(),
        }];
        let embeds = vec![(
            "my_ext".to_string(),
            crate::extension::EmbedDecl {
                role: "gate_config".to_string(),
                account: "prog_config".to_string(),
                offset: 32,
            },
        )];
        (specs, embeds)
    }

    #[test]
    fn embedded_role_reuses_same_spec_under_any_name() {
        // Recognition is by seed spec, not by name: a gated fn declaring
        // the embedding account's constraint under its own name must be
        // reused, never duplicated by injection.
        let (mut specs, embeds) = embedded_fixture();
        let create: ItemFn = syn::parse_quote!(
            pub fn create(
                #[account(init, pda = literal("prog_config"))] prog_config: AccountWithMetadata,
                #[account(signer)] payer: AccountWithMetadata,
            ) -> SpelResult {
                todo!()
            }
        );
        rewrite_embedded_roles(&mut specs, &embeds, &[create]).expect("rewrite succeeds");

        let mut gated: ItemFn = syn::parse_quote!(
            #[my_gate]
            pub fn update(
                #[account(pda = literal("prog_config"))] renamed: AccountWithMetadata,
                #[account(signer)] sender: AccountWithMetadata,
                value: u64,
            ) -> SpelResult {
                todo!()
            }
        );
        let before = gated.sig.inputs.len();
        let injected = inject_gate_params(&mut gated, &specs).expect("inject succeeds");
        assert!(
            injected.is_empty(),
            "same-spec params must be reused under their own names, injected: {injected:?}"
        );
        assert_eq!(
            gated.sig.inputs.len(),
            before,
            "no duplicate account params"
        );
    }

    // Specs of embedded_fixture plus an initializer wrapper, which
    // makes the coverage check mandatory for `my_ext`.
    fn initializer_specs() -> (Vec<InjectSpec>, Vec<(String, crate::extension::EmbedDecl)>) {
        let (mut specs, embeds) = embedded_fixture();
        specs.push(InjectSpec {
            wrapper: "gate_initialize".to_string(),
            accounts: vec![InjectAccount {
                name: "caller".to_string(),
                role: "caller".to_string(),
                seeds: vec![],
                signer: true,
                embedded: false,
            }],
            source: "my_ext".to_string(),
        });
        (specs, embeds)
    }

    #[test]
    fn embedding_creator_without_initializer_attr_is_refused() {
        let (mut specs, embeds) = initializer_specs();
        let create: ItemFn = syn::parse_quote!(
            pub fn create(
                #[account(init, pda = literal("prog_config"))] prog_config: AccountWithMetadata,
            ) -> SpelResult {
                todo!()
            }
        );
        let err = rewrite_embedded_roles(&mut specs, &embeds, &[create]).unwrap_err();
        assert!(
            err.contains("born renounced") && err.contains("create"),
            "got: {err}"
        );
    }

    #[test]
    fn annotated_creator_passes_the_coverage_check() {
        let (mut specs, embeds) = initializer_specs();
        let create: ItemFn = syn::parse_quote!(
            #[gate_initialize]
            pub fn create(
                #[account(init, pda = literal("prog_config"))] prog_config: AccountWithMetadata,
            ) -> SpelResult {
                todo!()
            }
        );
        rewrite_embedded_roles(&mut specs, &embeds, &[create]).expect("annotated creator passes");
    }

    #[test]
    fn initializer_attr_without_init_param_is_refused() {
        let (mut specs, embeds) = initializer_specs();
        let create: ItemFn = syn::parse_quote!(
            #[gate_initialize]
            pub fn create(
                #[account(init, pda = literal("prog_config"))] prog_config: AccountWithMetadata,
            ) -> SpelResult {
                todo!()
            }
        );
        let wrong: ItemFn = syn::parse_quote!(
            #[gate_initialize]
            pub fn poke(value: u64) -> SpelResult {
                todo!()
            }
        );
        let err = rewrite_embedded_roles(&mut specs, &embeds, &[create, wrong]).unwrap_err();
        assert!(
            err.contains("poke") && err.contains("freshly created"),
            "got: {err}"
        );
    }

    #[test]
    fn extension_without_initializer_spec_skips_the_check() {
        let (mut specs, embeds) = embedded_fixture();
        let create: ItemFn = syn::parse_quote!(
            pub fn create(
                #[account(init, pda = literal("prog_config"))] prog_config: AccountWithMetadata,
            ) -> SpelResult {
                todo!()
            }
        );
        rewrite_embedded_roles(&mut specs, &embeds, &[create])
            .expect("born-vacant extensions are untouched");
    }

    #[test]
    fn consumer_location_kwarg_on_embedded_gate_is_error() {
        let (specs, embeds) = embedded_fixture();
        let mut func: ItemFn = syn::parse_quote!(
            #[my_gate(gate_config = my_own)]
            pub fn update(value: u64) -> SpelResult {
                todo!()
            }
        );
        let err = apply_wrap_and_inject(&mut func, &[], &specs, &embeds, None).unwrap_err();
        assert!(
            err.contains("gate_config") && err.contains("update"),
            "got: {err}"
        );
    }

    #[test]
    fn consumer_offset_kwarg_on_embedded_gate_is_error() {
        let (specs, embeds) = embedded_fixture();
        let mut func: ItemFn = syn::parse_quote!(
            #[my_gate(offset = 64)]
            pub fn update(value: u64) -> SpelResult {
                todo!()
            }
        );
        let err = apply_wrap_and_inject(&mut func, &[], &specs, &embeds, None).unwrap_err();
        assert!(err.contains("offset"), "got: {err}");
    }

    #[test]
    fn signer_kwarg_stays_allowed_in_embedded_mode() {
        let (specs, embeds) = embedded_fixture();
        let mut func: ItemFn = syn::parse_quote!(
            #[my_gate(caller = my_signer)]
            pub fn update(
                #[account(signer)] my_signer: AccountWithMetadata,
                value: u64,
            ) -> SpelResult {
                todo!()
            }
        );
        apply_wrap_and_inject(&mut func, &[], &specs, &embeds, None)
            .expect("signer naming is orthogonal to slot location");
    }

    #[test]
    fn dedicated_manual_kwargs_stay_allowed() {
        let (specs, _) = embedded_fixture();
        let mut func: ItemFn = syn::parse_quote!(
            #[my_gate(gate_config = my_own, offset = 64)]
            pub fn update(value: u64) -> SpelResult {
                todo!()
            }
        );
        apply_wrap_and_inject(&mut func, &[], &specs, &[], None)
            .expect("without an embed decl the lockdown must not fire");
    }

    #[test]
    fn wrap_stamped_attr_carries_embedded_offset() {
        let specs = vec![InjectSpec {
            wrapper: "my_gate".to_string(),
            accounts: vec![InjectAccount {
                name: "prog_config".to_string(),
                role: "gate_config".to_string(),
                seeds: vec![InjectSeed::Const("prog_config".to_string())],
                signer: false,
                embedded: false,
            }],
            source: "my_ext".to_string(),
        }];
        let embeds = vec![(
            "my_ext".to_string(),
            crate::extension::EmbedDecl {
                role: "gate_config".to_string(),
                account: "prog_config".to_string(),
                offset: 32,
            },
        )];
        let wraps = vec![WrapInstructions {
            wrapper: "my_gate".to_string(),
            skip: None,
            self_exempt_marker: "my_exempt".to_string(),
            exempt: vec![],
        }];
        let mut func: ItemFn = syn::parse_quote!(
            pub fn update(value: u64) -> SpelResult {
                todo!()
            }
        );
        apply_wrap_and_inject(&mut func, &wraps, &specs, &embeds, None).unwrap();
        let expected: Attribute =
            syn::parse_quote!(#[my_gate(gate_config = prog_config, offset = 32)]);
        assert_eq!(
            func.attrs.first(),
            Some(&expected),
            "wrap-stamped gate must carry the extension's offset"
        );
    }

    #[test]
    fn substituted_role_param_takes_consumer_name_and_constraint() {
        let specs = vec![InjectSpec {
            wrapper: "my_gate".to_string(),
            accounts: vec![InjectAccount {
                name: "prog_config".to_string(),
                role: "gate_config".to_string(),
                seeds: vec![InjectSeed::Const("prog_config".to_string())],
                signer: false,
                embedded: true,
            }],
            source: "my_ext".to_string(),
        }];
        let mut func: ItemFn = syn::parse_quote!(
            pub fn ext_transfer(
                #[account(mut, pda = literal("gate_config"))] mut gate_config: AccountWithMetadata,
                #[account(signer)] caller: AccountWithMetadata,
            ) -> SpelResult {
                todo!()
            }
        );
        apply_wrap_and_inject(&mut func, &[], &specs, &[], Some("my_ext::ext_transfer")).unwrap();

        let FnArg::Typed(pt) = &func.sig.inputs[0] else {
            panic!("expected typed param");
        };
        let Pat::Ident(pi) = &*pt.pat else {
            panic!("expected ident pattern");
        };
        assert_eq!(
            pi.ident, "prog_config",
            "param renamed to the consumer account"
        );
        let expected: Attribute = syn::parse_quote!(#[account(mut, pda = literal("prog_config"))]);
        assert_eq!(
            pt.attrs.first(),
            Some(&expected),
            "mut preserved, constraint swapped"
        );
    }

    #[test]
    fn substitution_fires_when_embedding_account_shares_role_name() {
        let specs = vec![InjectSpec {
            wrapper: "my_gate".to_string(),
            accounts: vec![InjectAccount {
                name: "gate_config".to_string(),
                role: "gate_config".to_string(),
                seeds: vec![InjectSeed::Const("my_state".to_string())],
                signer: false,
                embedded: true,
            }],
            source: "my_ext".to_string(),
        }];
        let mut func: ItemFn = syn::parse_quote!(
            pub fn ext_transfer(
                #[account(mut, pda = literal("gate_config"))] mut gate_config: AccountWithMetadata,
            ) -> SpelResult {
                todo!()
            }
        );
        apply_wrap_and_inject(&mut func, &[], &specs, &[], Some("my_ext::ext_transfer")).unwrap();
        let FnArg::Typed(pt) = &func.sig.inputs[0] else {
            panic!("expected typed param");
        };
        let expected: Attribute = syn::parse_quote!(#[account(mut, pda = literal("my_state"))]);
        assert_eq!(
            pt.attrs.first(),
            Some(&expected),
            "same-name embed must still retarget"
        );
    }

    #[test]
    fn unrewritten_role_leaves_discovered_fn_untouched() {
        let specs = gate_specs();
        let mut func: ItemFn = syn::parse_quote!(
            pub fn ext_transfer(
                #[account(mut, pda = literal("gate_config"))] mut gate_config: AccountWithMetadata,
                #[account(signer)] caller: AccountWithMetadata,
            ) -> SpelResult {
                todo!()
            }
        );
        let before = func.clone();
        apply_wrap_and_inject(&mut func, &[], &specs, &[], Some("my_ext::ext_transfer")).unwrap();
        assert_eq!(
            func, before,
            "dedicated mode must not rewrite discovered fns"
        );
    }

    #[test]
    fn expr_to_seeds_handles_compound_arrays() {
        let seeds = expr_to_seeds(&expr(r#"[literal("cfg"), account("owner")]"#)).unwrap();
        assert_eq!(
            seeds,
            vec![
                InjectSeed::Const("cfg".to_string()),
                InjectSeed::Account("owner".to_string())
            ]
        );
    }

    #[test]
    fn canonical_constraint_resolves_from_init_declaration() {
        let fns = fns(r#"
            pub fn initialize(
                #[account(init, pda = literal("prog_config"))] mut prog_config: AccountWithMetadata,
            ) -> SpelResult { todo!() }
            pub fn update(
                #[account(mut, pda = literal("prog_config"))] mut prog_config: AccountWithMetadata,
            ) -> SpelResult { todo!() }
        "#);
        assert_eq!(
            resolve_canonical_constraint(&fns, "prog_config").unwrap(),
            expr(r#"literal("prog_config")"#)
        );
    }

    #[test]
    fn canonical_constraint_resolves_compound_pda() {
        let fns = fns(r#"
            pub fn initialize(
                #[account(init, pda = [literal("cfg"), account("owner")])] mut cfg: AccountWithMetadata,
            ) -> SpelResult { todo!() }
        "#);
        assert_eq!(
            resolve_canonical_constraint(&fns, "cfg").unwrap(),
            expr(r#"[literal("cfg"), account("owner")]"#)
        );
    }

    #[test]
    fn canonical_constraint_missing_init_is_error() {
        let fns = fns(r#"
            pub fn update(
                #[account(mut, pda = literal("prog_config"))] mut prog_config: AccountWithMetadata,
            ) -> SpelResult { todo!() }
        "#);
        let err = resolve_canonical_constraint(&fns, "prog_config").unwrap_err();
        assert!(err.contains("no canonical declaration"), "got: {err}");
    }

    #[test]
    fn canonical_constraint_never_declared_is_error() {
        let fns = fns("pub fn other(x: u64) -> u64 { x }");
        let err = resolve_canonical_constraint(&fns, "prog_config").unwrap_err();
        assert!(err.contains("no canonical declaration"), "got: {err}");
    }

    #[test]
    fn canonical_constraint_init_without_pda_is_not_canonical() {
        let fns = fns(r#"
            pub fn initialize(
                #[account(init)] mut prog_config: AccountWithMetadata,
            ) -> SpelResult { todo!() }
        "#);
        let err = resolve_canonical_constraint(&fns, "prog_config").unwrap_err();
        assert!(err.contains("no canonical declaration"), "got: {err}");
    }

    #[test]
    fn canonical_constraint_conflict_names_both_fns() {
        let fns = fns(r#"
            pub fn initialize(
                #[account(init, pda = literal("prog_config"))] mut prog_config: AccountWithMetadata,
            ) -> SpelResult { todo!() }
            pub fn update(
                #[account(mut, pda = literal("other_seed"))] mut prog_config: AccountWithMetadata,
            ) -> SpelResult { todo!() }
        "#);
        let err = resolve_canonical_constraint(&fns, "prog_config").unwrap_err();
        assert!(
            err.contains("initialize") && err.contains("update"),
            "got: {err}"
        );
    }

    #[test]
    fn canonical_constraint_ignores_other_params_and_fns() {
        let fns = fns(r#"
            pub fn initialize(
                #[account(init, pda = literal("prog_config"))] mut prog_config: AccountWithMetadata,
                #[account(init, pda = literal("unrelated"))] mut other: AccountWithMetadata,
            ) -> SpelResult { todo!() }
        "#);
        assert_eq!(
            resolve_canonical_constraint(&fns, "prog_config").unwrap(),
            expr(r#"literal("prog_config")"#)
        );
    }
    use syn::Pat;
    fn gate_specs() -> Vec<InjectSpec> {
        vec![InjectSpec {
            wrapper: "my_gate".to_string(),
            source: "ext-a".into(),
            accounts: vec![
                InjectAccount {
                    name: "gate_config".into(),
                    role: "gate_config".into(),
                    seeds: vec![InjectSeed::Const("gate_config".into())],
                    signer: false,
                    embedded: false,
                },
                InjectAccount {
                    name: "caller".into(),
                    role: "caller".into(),
                    seeds: vec![],
                    signer: true,
                    embedded: false,
                },
            ],
        }]
    }

    #[test]
    fn inject_gate_params_injects_missing_and_skips_declared() {
        let specs = gate_specs();

        // Gated fn missing both params: inject both, in order, at the front.
        let mut func: ItemFn = parse_quote! {
            #[instruction]
            #[my_gate]
            pub fn update_value(new_value: u64) -> SpelResult { todo!() }
        };
        let injected = inject_gate_params(&mut func, &specs).unwrap();
        assert_eq!(
            injected,
            vec!["gate_config".to_string(), "caller".to_string()]
        );
        assert_eq!(func.sig.inputs.len(), 3);

        // Second run: everything declared now, nothing injected, fn untouched.
        let before = func.sig.inputs.len();
        assert!(inject_gate_params(&mut func, &specs).unwrap().is_empty());
        assert_eq!(func.sig.inputs.len(), before);

        // Ungated fn: untouched.
        let mut plain: ItemFn = parse_quote! {
            #[instruction]
            pub fn other(x: u64) -> SpelResult { todo!() }
        };
        assert!(inject_gate_params(&mut plain, &specs).unwrap().is_empty());
    }

    #[test]
    fn inject_matches_qualified_wrapper_by_last_segment() {
        // Auto-wrap prepends fully qualified attrs; the spec names only
        // the final segment.
        let specs = gate_specs();
        let mut func: ItemFn = parse_quote! {
            #[instruction]
            #[my_ext_macros::my_gate]
            pub fn update_value(new_value: u64) -> SpelResult { todo!() }
        };
        let injected = inject_gate_params(&mut func, &specs).unwrap();
        assert_eq!(
            injected,
            vec!["gate_config".to_string(), "caller".to_string()]
        );
    }

    #[test]
    fn inject_emits_compound_pda_attr() {
        let specs = vec![InjectSpec {
            wrapper: "other_gate".to_string(),
            source: "ext-b".into(),
            accounts: vec![InjectAccount {
                name: "marker_account".into(),
                role: "marker_account".into(),
                seeds: vec![
                    InjectSeed::Const("marker".into()),
                    InjectSeed::Account("caller".into()),
                ],
                signer: false,
                embedded: false,
            }],
        }];
        let mut func: ItemFn = parse_quote! {
            #[instruction]
            #[other_gate]
            pub fn transfer(caller: AccountWithMetadata) -> SpelResult { todo!() }
        };
        assert_eq!(
            inject_gate_params(&mut func, &specs).unwrap(),
            vec!["marker_account"]
        );

        let FnArg::Typed(pt) = &func.sig.inputs[0] else {
            panic!("injected param must be typed");
        };
        let syn::Meta::List(list) = &pt.attrs[0].meta else {
            panic!("injected param must carry #[account(...)]");
        };
        let tokens = list.tokens.to_string();
        assert!(
            tokens.contains(r#"literal ("marker")"#) && tokens.contains(r#"account ("caller")"#),
            "compound pda attr not emitted: {tokens}"
        );
    }

    #[test]
    fn role_matched_params_skip_injection() {
        // Consumer declares both roles the spec would inject: a PDA
        // literal("gate_config") param and a signer. Injection detects
        // both via the role remap and skips them regardless of the
        // consumer's chosen names. ADR-0010 supersedes ADR-0009's
        // "args-form disables injection" — activation now runs for both
        // Path and List forms.
        let mut func: ItemFn = parse_quote! {
            #[instruction]
            #[my_gate(gate_config = my_cfg, caller = owner)]
            pub fn update_value(
                #[account(pda = literal("gate_config"))] my_cfg: AccountWithMetadata,
                #[account(signer)] owner: AccountWithMetadata,
                new_value: u64,
            ) -> SpelResult { todo!() }
        };
        assert!(inject_gate_params(&mut func, &gate_specs())
            .unwrap()
            .is_empty());
        assert_eq!(func.sig.inputs.len(), 3);
    }

    #[test]
    fn role_matched_compound_pda_skips_injection() {
        // Compound-seed reuse: consumer names their signer `sender` and
        // declares a per-account PDA with the same shape as the spec's
        // compound seed, resolved through the signer remap. Phase-1
        // remap resolves `caller` -> `sender`; phase-2 sees the
        // consumer's `[literal("frozen"), account("sender")]` matches
        // the spec's `[literal("frozen"), account("caller")]` after
        // resolution, so `marker_account` remaps to `my_frozen` and no
        // injection happens.
        let specs = vec![InjectSpec {
            wrapper: "my_gate".to_string(),
            source: "ext-c".into(),
            accounts: vec![
                InjectAccount {
                    name: "marker_account".into(),
                    role: "marker_account".into(),
                    seeds: vec![
                        InjectSeed::Const("frozen".into()),
                        InjectSeed::Account("caller".into()),
                    ],
                    signer: false,
                    embedded: false,
                },
                InjectAccount {
                    name: "caller".into(),
                    role: "caller".into(),
                    seeds: vec![],
                    signer: true,
                    embedded: false,
                },
            ],
        }];
        let mut func: ItemFn = parse_quote! {
            #[instruction]
            #[my_gate]
            pub fn withdraw(
                #[account(pda = [literal("frozen"), account("sender")])] my_frozen: AccountWithMetadata,
                #[account(signer)] sender: AccountWithMetadata,
                amount: u64,
            ) -> SpelResult { todo!() }
        };
        assert!(inject_gate_params(&mut func, &specs).unwrap().is_empty());
        assert_eq!(func.sig.inputs.len(), 3);
    }

    #[test]
    fn two_specs_append_in_order_with_running_cursor() {
        let mut specs = gate_specs();
        specs.push(InjectSpec {
            wrapper: "my_gate".to_string(),
            source: "ext-b".into(),
            accounts: vec![InjectAccount {
                name: "other_config".into(),
                role: "other_config".into(),
                seeds: vec![InjectSeed::Const("other_config".into())],
                signer: false,
                embedded: false,
            }],
        });
        let mut func: ItemFn = parse_quote! {
            #[instruction]
            #[my_gate]
            pub fn update_value(new_value: u64) -> SpelResult { todo!() }
        };
        let injected = inject_gate_params(&mut func, &specs).unwrap();
        assert_eq!(injected, vec!["gate_config", "caller", "other_config"]);
        let names: Vec<String> = func
            .sig
            .inputs
            .iter()
            .filter_map(|i| match i {
                FnArg::Typed(pt) => match &*pt.pat {
                    Pat::Ident(pi) => Some(pi.ident.to_string()),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        assert_eq!(
            names,
            vec!["gate_config", "caller", "other_config", "new_value"]
        );
    }

    #[test]
    fn identical_shared_param_dedups_to_first_position() {
        let mut specs = gate_specs();
        specs.push(InjectSpec {
            wrapper: "my_gate".to_string(),
            source: "ext-b".into(),
            accounts: vec![InjectAccount {
                name: "caller".into(),
                role: "caller".into(),
                seeds: vec![],
                signer: true,
                embedded: false,
            }],
        });
        let mut func: ItemFn = parse_quote! {
            #[instruction]
            #[my_gate]
            pub fn update_value(new_value: u64) -> SpelResult { todo!() }
        };
        let injected = inject_gate_params(&mut func, &specs).unwrap();
        assert_eq!(injected, vec!["gate_config", "caller"]);
        assert_eq!(func.sig.inputs.len(), 3);
    }

    #[test]
    fn conflicting_shared_param_is_a_hard_error() {
        let mut specs = gate_specs();
        specs.push(InjectSpec {
            wrapper: "my_gate".to_string(),
            source: "ext-b".into(),
            accounts: vec![InjectAccount {
                name: "caller".into(),
                role: "caller".into(),
                seeds: vec![InjectSeed::Const("caller_pda".into())],
                signer: false,
                embedded: false,
            }],
        });
        let mut func: ItemFn = parse_quote! {
            #[instruction]
            #[my_gate]
            pub fn update_value(new_value: u64) -> SpelResult { todo!() }
        };
        let err = inject_gate_params(&mut func, &specs)
            .expect_err("conflicting constraints must be rejected");
        assert!(
            err.contains("ext-a") && err.contains("ext-b"),
            "must name both extensions: {err}"
        );
        assert!(err.contains("caller"), "must name the param: {err}");
    }

    #[test]
    fn embedded_role_substitutes_on_peer_extension_fns() {
        // One extension's embedded entry must retarget a PEER extension's
        // authored role param: substitution is global across extensions.
        // This is the admin-embedded cell of freeze's renounce.
        let specs = vec![InjectSpec {
            wrapper: "require_admin".to_string(),
            accounts: vec![InjectAccount {
                name: "prog_config".to_string(),
                role: "admin_config".to_string(),
                seeds: vec![InjectSeed::Const("prog_config".to_string())],
                signer: false,
                embedded: true,
            }],
            source: "admin_authority".to_string(),
        }];
        let mut func: ItemFn = syn::parse_quote!(
            pub fn freeze_authority_renounce(
                #[account(pda = literal("admin_config"))] admin_config: AccountWithMetadata,
                #[account(mut, pda = literal("freeze_config"))]
                mut freeze_config: AccountWithMetadata,
            ) -> SpelResult {
                todo!()
            }
        );
        apply_wrap_and_inject(
            &mut func,
            &[],
            &specs,
            &[],
            Some("freeze_authority::freeze_authority_renounce"),
        )
        .unwrap();
        let FnArg::Typed(pt) = &func.sig.inputs[0] else {
            panic!("expected typed param");
        };
        let Pat::Ident(pi) = &*pt.pat else {
            panic!("expected ident pattern");
        };
        assert_eq!(pi.ident, "prog_config", "peer fn's role param renamed");
        let expected: Attribute = syn::parse_quote!(#[account(pda = literal("prog_config"))]);
        assert_eq!(pt.attrs.first(), Some(&expected));
    }

    #[test]
    fn same_account_same_offset_embeds_are_rejected() {
        let mut specs = vec![
            InjectSpec {
                wrapper: "gate_a".to_string(),
                accounts: vec![InjectAccount {
                    name: "cfg_a".to_string(),
                    role: "cfg_a".to_string(),
                    seeds: vec![InjectSeed::Const("cfg_a".to_string())],
                    signer: false,
                    embedded: false,
                }],
                source: "ext_a".to_string(),
            },
            InjectSpec {
                wrapper: "gate_b".to_string(),
                accounts: vec![InjectAccount {
                    name: "cfg_b".to_string(),
                    role: "cfg_b".to_string(),
                    seeds: vec![InjectSeed::Const("cfg_b".to_string())],
                    signer: false,
                    embedded: false,
                }],
                source: "ext_b".to_string(),
            },
        ];
        let consumer: ItemFn = syn::parse_quote!(
            pub fn initialize(
                #[account(init, pda = literal("shared"))] mut shared: AccountWithMetadata,
            ) -> SpelResult {
                todo!()
            }
        );
        let embeds = |off_b: usize| {
            vec![
                (
                    "ext_a".to_string(),
                    crate::extension::EmbedDecl {
                        role: "cfg_a".to_string(),
                        account: "shared".to_string(),
                        offset: 32,
                    },
                ),
                (
                    "ext_b".to_string(),
                    crate::extension::EmbedDecl {
                        role: "cfg_b".to_string(),
                        account: "shared".to_string(),
                        offset: off_b,
                    },
                ),
            ]
        };
        let err = rewrite_embedded_roles(&mut specs, &embeds(32), std::slice::from_ref(&consumer))
            .expect_err("equal offsets on one account must be rejected");
        assert!(err.contains("both embed into"), "unexpected error: {err}");

        // Distinct offsets on the same account are the goal layout.
        rewrite_embedded_roles(&mut specs, &embeds(64), std::slice::from_ref(&consumer))
            .expect("distinct offsets must pass");
    }
}
