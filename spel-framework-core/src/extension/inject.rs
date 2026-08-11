//! Wrap application and gate-param injection: the shared pass that
//! prepends wrapper attrs and synthesizes missing gate accounts, used
//! identically by the dispatcher expansion and both IDL paths.

use std::collections::HashMap;

use syn::{parse_quote, Attribute, FnArg, ItemFn};

use super::{marker::attr_is, Embed, InjectAccount, InjectSeed, InjectSpec, WrapInstructions};

/// Filter `deps.extensions.wraps` down to the wraps whose extension
/// marker carries no skip-word arg matching `WrapInstructions::skip`.
///
/// A wrap with `skip = Some("manual")` is dropped for a marker written
/// `#[freeze_authority(manual)]`, kept for `#[freeze_authority]`. A wrap
/// with `skip = None` is always kept. Each survivor carries its
/// attribute path parsed, see [`ActiveWrap`].
///
/// # Errors
///
/// `Err` when a kept wrap declares a wrapper that is not a valid Rust
/// attribute path. Callers surface it as a compile error.
pub fn active_wraps(wraps: &[(String, WrapInstructions)]) -> Result<Vec<ActiveWrap>, String> {
    wraps
        .iter()
        .filter(|(arg, wrap)| match &wrap.skip {
            Some(s) => arg != s,
            None => true,
        })
        .map(|(_, wrap)| ActiveWrap::new(wrap.clone()))
        .collect()
}

/// A wrap the consumer's marker kept, with its attribute path parsed.
///
/// The path is the same for every instruction the wrap gates, so it is
/// parsed once per program and the gate pass reads it. A declared path
/// that is not a Rust attribute path is the extension author's mistake
/// and is reported against the program, not against each fn.
pub struct ActiveWrap {
    /// What the extension declared: wrapper name, skip word, exemptions.
    pub config: WrapInstructions,
    /// The wrapper as an attribute path, ready to prepend.
    pub path: syn::Path,
    /// Last segment of that path, the name inject specs match by. The
    /// framework prepends fully qualified attrs, so matching on the last
    /// segment is what lets those activate a spec.
    pub last: String,
}

impl ActiveWrap {
    /// # Errors
    ///
    /// `Err` when the declared wrapper is not a valid attribute path.
    fn new(config: WrapInstructions) -> Result<Self, String> {
        let path: syn::Path = syn::parse_str(&config.wrapper)
            .map_err(|e| format!("invalid wrapper path {:?}: {e}", config.wrapper))?;
        let last = path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_default();
        Ok(Self { config, path, last })
    }
}

/// Whether a producer writes the framework's location kwargs onto the
/// gate attrs it prepends.
///
/// The dispatcher writes them: the gate attr expands in the consumer's
/// crate and reads `offset` to find its state. The IDL producers do
/// not: they read the fn signature and discard the attrs, so a written
/// offset would be dead tokens, and demanding one would make them
/// resolve a slot carrier no output of theirs can observe. Injection is
/// unaffected either way, the prepended attr activates specs in both
/// its bare and its kwarg-carrying form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GateLocations {
    /// Write `offset = ...`. An unresolved derivation reaching this
    /// producer is a framework bug and fails closed.
    Emit,
    /// Leave it off. Offsets may arrive unresolved; nothing reads them.
    /// The default: writing a location is the choice that needs making.
    #[default]
    Omit,
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
/// embedded role substitution applies only on this path. On both paths
/// a fn that creates a gate's embedded account is skipped by that gate
/// ([`creates_gated_embedded_account`]): the state the gate would
/// decode cannot exist before the fn runs.
///
/// Returns the names of the params `inject_gate_params` synthesized.
///
/// `locations` says whether this producer writes the offset kwarg; see
/// [`GateLocations`]. The accounts it injects are identical either way,
/// which is what keeps the IDL's account list and the dispatcher's in
/// agreement without the IDL producers resolving a carrier.
///
/// # Errors
///
/// Propagates `inject_gate_params` errors and fails when a declared
/// wrapper path is not a valid Rust attribute path, when a
/// consumer-authored gate attr carries a location kwarg in embedded
/// mode, and, under [`GateLocations::Emit`], when an offset reaches the
/// pass unresolved.
pub fn apply_wrap_and_inject(
    func: &mut ItemFn,
    active_wraps: &[ActiveWrap],
    inject_specs: &[InjectSpec],
    embeds: &[Embed],
    locations: GateLocations,
    qualified: Option<&str>,
) -> Result<Vec<String>, String> {
    if locations == GateLocations::Emit {
        if let Some(e) = embeds
            .iter()
            .find(|e| e.decl.offset == super::OffsetSpec::Derived)
        {
            return Err(format!(
                "extension `{}` reached the gate pass with an unresolved \
                derived offset; `resolve_derived_offsets` must run after discovery",
                e.source
            ));
        }
    }
    if qualified.is_some() {
        substitute_embedded_params(func, inject_specs);
    }

    let remap = build_remap(inject_specs, func);
    // Membership answers "is this extension embedded", which decides
    // the authored-kwarg rejection on every producer. The value is read
    // only when this one writes locations.
    let offset_by_source: HashMap<&str, &super::OffsetSpec> = embeds
        .iter()
        .map(|e| (e.source.as_str(), &e.decl.offset))
        .collect();
    let gate_offset = |spec: &InjectSpec| match locations {
        GateLocations::Emit => offset_by_source.get(spec.source.as_str()).copied(),
        GateLocations::Omit => None,
    };
    check_authored_location_kwargs(func, inject_specs, &offset_by_source)?;

    for wrap in active_wraps {
        let (wrapper_path, wrapper_last) = (&wrap.path, &wrap.last);
        let exempt = func
            .attrs
            .iter()
            .any(|a| a.path().is_ident(&wrap.config.self_exempt_marker))
            || qualified
                .map(|q| wrap.config.exempt.iter().any(|e| e == q))
                .unwrap_or(false)
            || creates_gated_embedded_account(func, inject_specs, wrapper_last);
        if exempt {
            continue;
        }

        let mut args: Vec<syn::MetaNameValue> = Vec::new();
        for spec in inject_specs {
            if &spec.wrapper != wrapper_last {
                continue;
            }
            args.extend(spec_gate_args(spec, &remap, gate_offset(spec))?);
        }
        let attr: Attribute = if args.is_empty() {
            parse_quote! { #[#wrapper_path] }
        } else {
            parse_quote! { #[#wrapper_path(#(#args), *)] }
        };
        func.attrs.insert(0, attr);
    }

    let injected = inject_gate_params(func, inject_specs, &remap)?;

    // Stamping only ever writes location kwargs, so a producer that
    // omits them has nothing to stamp. It runs after injection either
    // way, so the bare-attr-activates-injection rule already fired.
    if locations == GateLocations::Emit {
        stamp_authored_gates(func, inject_specs, &remap, &offset_by_source)?;
    }

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
fn resolve_canonical_constraint(fns: &[ItemFn], account: &str) -> Result<syn::Expr, String> {
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
    embeds: &[Embed],
    consumer_fns: &[ItemFn],
) -> Result<(), String> {
    for Embed {
        source,
        decl: embed,
        ..
    } in embeds
    {
        let canonical = resolve_canonical_constraint(consumer_fns, &embed.account)?;
        let seeds = expr_to_seeds(&canonical).ok_or_else(|| {
            format!(
                "embedded account `{}`: canonical constraint is not a \
                literal()/account() seed expression",
                embed.account
            )
        })?;

        check_initializer_coverage(embed, consumer_fns)?;

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

/// True when `func` creates an embedded account this wrap's gate would
/// decode. The account being created is fresh and its slot is born
/// vacant, so there is no state for the gate to enforce: gating the
/// creator only makes initialization impossible. Dedicated mode never
/// matches, no consumer fn declares the extension's own PDA with
/// `#[account(init)]`.
fn creates_gated_embedded_account(
    func: &ItemFn,
    inject_specs: &[InjectSpec],
    wrapper_last: &str,
) -> bool {
    inject_specs
        .iter()
        .filter(|s| s.wrapper == wrapper_last)
        .flat_map(|s| s.accounts.iter())
        .filter(|a| a.embedded)
        .any(|a| {
            typed_params(func).any(|(pi, pt)| pi.ident == a.name.as_str() && param_has_init(pt))
        })
}

/// An extension that declares an initializer, the embed anchor from
/// `embedded.anchor_attr`, makes it mandatory: every instruction that
/// creates the embedding account must carry it, or the program ships
/// born renounced. Extensions without one (freeze is born vacant by
/// design) are untouched; a wrapper whose name merely matches the
/// `<role>_initialize` shape implies nothing.
fn check_initializer_coverage(
    embed: &super::EmbedDecl,
    consumer_fns: &[ItemFn],
) -> Result<(), String> {
    let Some(init_attr) = &embed.initializer else {
        return Ok(());
    };
    for func in consumer_fns {
        let creates = typed_params(func)
            .any(|(pi, pt)| pi.ident == embed.account.as_str() && param_has_init(pt));
        let annotated = func.attrs.iter().any(|a| attr_is(a, init_attr));

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
///
/// Only literal pairs are decided here. A derived offset is not a
/// number at this point, so pairs involving one are checked by the
/// const assert `slot_offsets` emits over the carriers' offset consts,
/// which rustc evaluates once the layout is known.
fn check_embed_window_collisions(embeds: &[Embed]) -> Result<(), String> {
    for (i, a) in embeds.iter().enumerate() {
        for b in &embeds[i + 1..] {
            let (source_a, source_b) = (&a.source, &b.source);
            let (a, b) = (&a.decl, &b.decl);
            if a.account != b.account {
                continue;
            }
            let (super::OffsetSpec::Literal(off_a), super::OffsetSpec::Literal(off_b)) =
                (&a.offset, &b.offset)
            else {
                continue;
            };
            if off_a == off_b {
                return Err(format!(
                    "extensions `{source_a}` and `{source_b}` both embed into \
                    account `{}` at offset {off_a}; identical offsets cannot both \
                    hold state, declare distinct offsets",
                    a.account
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
/// Returns the names actually injected. `remap` is the one built by
/// [`apply_wrap_and_inject`]: nothing between there and here touches a
/// param signature, so recomputing it could only produce the same map.
///
/// When two specs want the same param name: identical constraints share
/// one account at the first injector's position, the cheap shared-signer
/// ABI. Conflicting constraints are a hard error naming both extensions.
///
/// # Errors
///
/// `Err` when two extensions inject the same param name with different
/// constraints; callers surface it as a compile error.
fn inject_gate_params(
    func: &mut ItemFn,
    specs: &[InjectSpec],
    remap: &HashMap<String, String>,
) -> Result<Vec<String>, String> {
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
            func.sig.inputs.insert(pos, build_inject_param(acc, remap));
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
        attr_is(a, &spec.wrapper) && matches!(a.meta, syn::Meta::Path(_) | syn::Meta::List(_))
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
        if !param_has_flag(pt, "signer") {
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
    let target = remap_seeds(seeds, remap);
    typed_params(func)
        .find(|(_, pt)| param_pda_seeds(pt).is_some_and(|s| s == target))
        .map(|(pi, _)| pi.ident.to_string())
}

// Account seeds under the consumer's param names; const seeds are
// names of nothing and pass through.
fn remap_seeds(seeds: &[InjectSeed], remap: &HashMap<String, String>) -> Vec<InjectSeed> {
    seeds
        .iter()
        .map(|s| match s {
            InjectSeed::Const(v) => InjectSeed::Const(v.clone()),
            InjectSeed::Account(v) => {
                InjectSeed::Account(remap.get(v).cloned().unwrap_or_else(|| v.clone()))
            },
        })
        .collect()
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
    match (
        seeds_to_pda_expr(&remap_seeds(&acc.seeds, remap)),
        acc.signer,
    ) {
        (None, true) => parse_quote! { #[account(signer)] #ident: AccountWithMetadata },
        (None, false) => parse_quote! { #ident: AccountWithMetadata },
        (Some(pda), _) => parse_quote! { #[account(pda = #pda)] #ident: AccountWithMetadata },
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
///
/// # Errors
///
/// Propagates the shared offset lowering's error: an unresolved
/// derivation or an unparsable carrier path.
fn spec_gate_args(
    spec: &InjectSpec,
    remap: &HashMap<String, String>,
    offset: Option<&super::OffsetSpec>,
) -> Result<Vec<syn::MetaNameValue>, String> {
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
        let value = off.to_expr(&spec.source)?;
        args.push(parse_quote! { offset = #value });
    }
    Ok(args)
}

// Rewrite consumer-authored bare gate attrs to carry the framework's
// location kwargs. Embedded mode is the only writer of those kwargs
// and stamps every gate. Runs after the injection pass so the
// bare-attr-activates-injection rule saw the authored form.
fn stamp_authored_gates(
    func: &mut ItemFn,
    inject_specs: &[InjectSpec],
    remap: &HashMap<String, String>,
    offset_by_source: &HashMap<&str, &super::OffsetSpec>,
) -> Result<(), String> {
    for attr in func.attrs.iter_mut() {
        if !matches!(attr.meta, syn::Meta::Path(_)) {
            continue;
        }
        for spec in inject_specs {
            if !attr_is(attr, &spec.wrapper) {
                continue;
            }
            let Some(off) = offset_by_source.get(spec.source.as_str()) else {
                continue;
            };
            let args = spec_gate_args(spec, remap, Some(*off))?;
            let path = attr.path().clone();
            *attr = parse_quote! { #[#path(#(#args),*)] };
        }
    }
    Ok(())
}

// Named, typed params of a fn: the only shape gate machinery reads.
pub(super) fn typed_params(func: &ItemFn) -> impl Iterator<Item = (&syn::PatIdent, &syn::PatType)> {
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
pub(super) fn param_has_init(pt: &syn::PatType) -> bool {
    param_has_flag(pt, "init")
}

// True when the param's `#[account]` attr carries a bare `flag`.
fn param_has_flag(pt: &syn::PatType, flag: &str) -> bool {
    pt.attrs.iter().any(|attr| {
        if !attr.path().is_ident("account") {
            return false;
        }
        let mut hit = false;
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident(flag) {
                hit = true;
            }
            Ok(())
        })
        .ok();
        hit
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
    offset_by_source: &HashMap<&str, &super::OffsetSpec>,
) -> Result<(), String> {
    for attr in &func.attrs {
        if !matches!(attr.meta, syn::Meta::List(_)) {
            continue;
        }
        for spec in inject_specs {
            if !attr_is(attr, &spec.wrapper) || !offset_by_source.contains_key(spec.source.as_str())
            {
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
    use crate::extension::OffsetSpec;

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

    // Wrap configs with their attribute paths parsed, as the gate pass
    // receives them from `active_wraps`.
    fn active(configs: Vec<WrapInstructions>) -> Vec<ActiveWrap> {
        configs
            .into_iter()
            .map(|c| ActiveWrap::new(c).expect("fixture wrapper paths parse"))
            .collect()
    }

    // The injection pass under the remap its production caller builds.
    fn inject(func: &mut ItemFn, specs: &[InjectSpec]) -> Result<Vec<String>, String> {
        let remap = build_remap(specs, func);
        inject_gate_params(func, specs, &remap)
    }

    // A rewritten spec plus its embed decl: embedded mode for `my_ext`.
    fn embedded_fixture() -> (Vec<InjectSpec>, Vec<Embed>) {
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
        let embeds = vec![Embed {
            source: "my_ext".to_string(),
            carrier: None,
            state_type: "my_ext::MyConfig".to_string(),
            decl: crate::extension::EmbedDecl {
                role: "gate_config".to_string(),
                account: "prog_config".to_string(),
                offset: OffsetSpec::Literal(32),
                initializer: None,
            },
        }];
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
        let injected = inject(&mut gated, &specs).expect("inject succeeds");
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

    // The embedded fixture with a declared initializer: the embed
    // carries the anchor attr, which makes the coverage check
    // mandatory for `my_ext`.
    fn initializer_fixture() -> (Vec<InjectSpec>, Vec<Embed>) {
        let (specs, mut embeds) = embedded_fixture();
        embeds[0].decl.initializer = Some("gate_initialize".to_string());
        (specs, embeds)
    }

    #[test]
    fn embedding_creator_without_initializer_attr_is_refused() {
        let (mut specs, embeds) = initializer_fixture();
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
        let (mut specs, embeds) = initializer_fixture();
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
        let (mut specs, embeds) = initializer_fixture();
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

    // The declaration is the whole trigger: an embed without an
    // initializer gets no coverage gate, even when a wrapper's name
    // matches the `<role>_initialize` shape.
    #[test]
    fn undeclared_initializer_means_no_coverage_gate() {
        let (mut specs, embeds) = embedded_fixture();
        specs.push(InjectSpec {
            wrapper: "gate_initialize".to_string(),
            accounts: vec![],
            source: "my_ext".to_string(),
        });
        let create: ItemFn = syn::parse_quote!(
            pub fn create(
                #[account(init, pda = literal("prog_config"))] prog_config: AccountWithMetadata,
            ) -> SpelResult {
                todo!()
            }
        );
        rewrite_embedded_roles(&mut specs, &embeds, &[create])
            .expect("no declaration, no gate; born-vacant extensions are untouched");
    }

    // The declared name is the entire rule: an initializer named
    // outside the `<role>_initialize` shape still gates every creator.
    #[test]
    fn unconventional_initializer_name_still_gates() {
        let (mut specs, mut embeds) = embedded_fixture();
        embeds[0].decl.initializer = Some("bootstrap_gate".to_string());
        let create: ItemFn = syn::parse_quote!(
            pub fn create(
                #[account(init, pda = literal("prog_config"))] prog_config: AccountWithMetadata,
            ) -> SpelResult {
                todo!()
            }
        );
        let err = rewrite_embedded_roles(&mut specs, &embeds, &[create]).unwrap_err();
        assert!(
            err.contains("bootstrap_gate") && err.contains("born renounced"),
            "got: {err}"
        );
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
        let err = apply_wrap_and_inject(&mut func, &[], &specs, &embeds, GateLocations::Emit, None)
            .unwrap_err();
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
        let err = apply_wrap_and_inject(&mut func, &[], &specs, &embeds, GateLocations::Emit, None)
            .unwrap_err();
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
        apply_wrap_and_inject(&mut func, &[], &specs, &embeds, GateLocations::Emit, None)
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
        apply_wrap_and_inject(&mut func, &[], &specs, &[], GateLocations::Emit, None)
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
        let embeds = vec![Embed {
            source: "my_ext".to_string(),
            carrier: None,
            state_type: "my_ext::MyConfig".to_string(),
            decl: crate::extension::EmbedDecl {
                role: "gate_config".to_string(),
                account: "prog_config".to_string(),
                offset: OffsetSpec::Literal(32),
                initializer: None,
            },
        }];
        let wraps = active(vec![WrapInstructions {
            wrapper: "my_gate".to_string(),
            skip: None,
            self_exempt_marker: "my_exempt".to_string(),
            exempt: vec![],
        }]);
        let mut func: ItemFn = syn::parse_quote!(
            pub fn update(value: u64) -> SpelResult {
                todo!()
            }
        );
        apply_wrap_and_inject(
            &mut func,
            &wraps,
            &specs,
            &embeds,
            GateLocations::Emit,
            None,
        )
        .unwrap();
        let expected: Attribute =
            syn::parse_quote!(#[my_gate(gate_config = prog_config, offset = 32)]);
        assert_eq!(
            func.attrs.first(),
            Some(&expected),
            "wrap-stamped gate must carry the extension's offset"
        );
    }

    // The IDL producers read the fn signature and drop the attrs, so
    // they never resolve a carrier: an unresolved derivation reaches
    // them legally, and the accounts they see are the dispatcher's.
    // Only the location kwarg differs, and only on tokens nobody reads.
    #[test]
    fn omitted_locations_inject_what_emitted_ones_do() {
        let (specs, embeds) = embedded_fixture();
        let mut resolved = embeds.clone();
        resolved[0].decl.offset = OffsetSpec::Path("Cfg::GATE_SLOT_OFFSET".to_string());
        let mut derived = embeds;
        derived[0].decl.offset = OffsetSpec::Derived;
        let authored: ItemFn = syn::parse_quote!(
            #[my_gate]
            pub fn update(value: u64) -> SpelResult {
                todo!()
            }
        );

        let mut dispatcher = authored.clone();
        let emitted = apply_wrap_and_inject(
            &mut dispatcher,
            &[],
            &specs,
            &resolved,
            GateLocations::Emit,
            None,
        )
        .expect("the dispatcher takes a resolved derivation");
        let mut idl = authored;
        let omitted =
            apply_wrap_and_inject(&mut idl, &[], &specs, &derived, GateLocations::Omit, None)
                .expect("an IDL producer takes an unresolved one");

        assert_eq!(emitted, omitted, "same params injected");
        assert_eq!(
            idl.sig.inputs, dispatcher.sig.inputs,
            "the signature the IDL is read from must match the dispatcher's"
        );
        let stamped: Attribute = syn::parse_quote!(
            #[my_gate(gate_config = prog_config, caller = caller, offset = Cfg::GATE_SLOT_OFFSET)]
        );
        assert_eq!(dispatcher.attrs.first(), Some(&stamped));
        assert_eq!(
            idl.attrs.first(),
            Some(&syn::parse_quote!(#[my_gate])),
            "nothing stamps a location an IDL producer would discard"
        );
    }

    // The tripwire survives where it means something: a derivation that
    // reaches the dispatcher unresolved is still a framework bug.
    #[test]
    fn emitted_locations_still_refuse_an_unresolved_derivation() {
        let (specs, mut embeds) = embedded_fixture();
        embeds[0].decl.offset = OffsetSpec::Derived;
        let mut func: ItemFn = syn::parse_quote!(
            #[my_gate]
            pub fn update(value: u64) -> SpelResult {
                todo!()
            }
        );
        let err = apply_wrap_and_inject(&mut func, &[], &specs, &embeds, GateLocations::Emit, None)
            .unwrap_err();
        assert!(err.contains("resolve_derived_offsets"), "got: {err}");
    }

    // Post-rewrite embedded state plus an active wrap: the shape every
    // auto-gate skip test below starts from.
    fn embedded_wrap_fixture() -> (Vec<InjectSpec>, Vec<Embed>, Vec<ActiveWrap>) {
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
        let embeds = vec![Embed {
            source: "my_ext".to_string(),
            carrier: None,
            state_type: "my_ext::MyConfig".to_string(),
            decl: crate::extension::EmbedDecl {
                role: "gate_config".to_string(),
                account: "prog_config".to_string(),
                offset: OffsetSpec::Literal(32),
                initializer: None,
            },
        }];
        let wraps = active(vec![WrapInstructions {
            wrapper: "my_gate".to_string(),
            skip: None,
            self_exempt_marker: "my_exempt".to_string(),
            exempt: vec![],
        }]);
        (specs, embeds, wraps)
    }

    // The fn creating the embedding account is never auto-gated: the
    // state the gate would decode cannot exist before this fn runs.
    #[test]
    fn embedding_creator_is_never_auto_gated() {
        let (specs, embeds, wraps) = embedded_wrap_fixture();
        let mut func: ItemFn = syn::parse_quote!(
            pub fn initialize(
                #[account(init, pda = literal("prog_config"))] mut prog_config: AccountWithMetadata,
            ) -> SpelResult {
                todo!()
            }
        );
        let injected = apply_wrap_and_inject(
            &mut func,
            &wraps,
            &specs,
            &embeds,
            GateLocations::Emit,
            None,
        )
        .unwrap();
        assert!(
            func.attrs.iter().all(|a| !a.path().is_ident("my_gate")),
            "the embedding account's creator must not carry the gate"
        );
        assert!(
            injected.is_empty(),
            "a skipped gate must not inject params, got: {injected:?}"
        );
    }

    // Creating some other account is a state change freeze exists to
    // block: only init on the gate's own account lifts the gate.
    #[test]
    fn creator_of_a_sibling_account_stays_gated() {
        let (specs, embeds, wraps) = embedded_wrap_fixture();
        let mut func: ItemFn = syn::parse_quote!(
            pub fn create_item(
                #[account(pda = literal("prog_config"))] prog_config: AccountWithMetadata,
                #[account(init, pda = literal("item"))] mut item: AccountWithMetadata,
            ) -> SpelResult {
                todo!()
            }
        );
        apply_wrap_and_inject(
            &mut func,
            &wraps,
            &specs,
            &embeds,
            GateLocations::Emit,
            None,
        )
        .unwrap();
        assert!(
            func.attrs
                .first()
                .is_some_and(|a| a.path().is_ident("my_gate")),
            "init on a sibling account must not lift the gate"
        );
    }

    // The authored bare gate in embedded mode: injection first fills
    // the missing caller, stamping then writes the location kwargs on
    // the authored attr. The order is structurally fixed inside
    // apply_wrap_and_inject, this pins its observable outcome as a
    // unit.
    #[test]
    fn authored_bare_gate_gets_injection_and_then_stamping() {
        let (mut specs, embeds, _) = embedded_wrap_fixture();
        specs[0].accounts.push(InjectAccount {
            name: "caller".to_string(),
            role: "caller".to_string(),
            seeds: vec![],
            signer: true,
            embedded: false,
        });
        let mut func: ItemFn = syn::parse_quote!(
            #[my_gate]
            pub fn update(
                #[account(pda = literal("prog_config"))] prog_config: AccountWithMetadata,
                value: u64,
            ) -> SpelResult {
                todo!()
            }
        );
        let injected =
            apply_wrap_and_inject(&mut func, &[], &specs, &embeds, GateLocations::Emit, None)
                .unwrap();
        assert_eq!(
            injected,
            vec!["caller".to_string()],
            "the authored gate's missing caller must be injected"
        );
        let attr = func.attrs.first().expect("the authored attr survives");
        assert!(attr.path().is_ident("my_gate"), "authored gate stays first");
        let syn::Meta::List(stamped) = &attr.meta else {
            panic!("the authored gate must have been stamped with args");
        };
        let stamped = stamped.tokens.to_string();
        assert!(
            stamped.contains("offset = 32"),
            "the authored gate must be stamped with the framework's \
            location kwargs: {stamped}"
        );
    }

    // Dedicated mode: the gate's account is the extension's own PDA
    // (embedded = false), so even a same-named init keeps the gate.
    // Pins that the `embedded` flag guards the skip.
    #[test]
    fn dedicated_mode_creator_stays_gated() {
        let (mut specs, _, wraps) = embedded_wrap_fixture();
        specs[0].accounts[0].embedded = false;
        let mut func: ItemFn = syn::parse_quote!(
            pub fn initialize(
                #[account(init, pda = literal("prog_config"))] mut prog_config: AccountWithMetadata,
            ) -> SpelResult {
                todo!()
            }
        );
        apply_wrap_and_inject(&mut func, &wraps, &specs, &[], GateLocations::Emit, None).unwrap();
        assert!(
            func.attrs
                .first()
                .is_some_and(|a| a.path().is_ident("my_gate")),
            "dedicated mode must keep the gate on every consumer fn"
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
        apply_wrap_and_inject(
            &mut func,
            &[],
            &specs,
            &[],
            GateLocations::Emit,
            Some("my_ext::ext_transfer"),
        )
        .unwrap();

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
        apply_wrap_and_inject(
            &mut func,
            &[],
            &specs,
            &[],
            GateLocations::Emit,
            Some("my_ext::ext_transfer"),
        )
        .unwrap();
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
        apply_wrap_and_inject(
            &mut func,
            &[],
            &specs,
            &[],
            GateLocations::Emit,
            Some("my_ext::ext_transfer"),
        )
        .unwrap();
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
        let injected = inject(&mut func, &specs).unwrap();
        assert_eq!(
            injected,
            vec!["gate_config".to_string(), "caller".to_string()]
        );
        assert_eq!(func.sig.inputs.len(), 3);

        // Second run: everything declared now, nothing injected, fn untouched.
        let before = func.sig.inputs.len();
        assert!(inject(&mut func, &specs).unwrap().is_empty());
        assert_eq!(func.sig.inputs.len(), before);

        // Ungated fn: untouched.
        let mut plain: ItemFn = parse_quote! {
            #[instruction]
            pub fn other(x: u64) -> SpelResult { todo!() }
        };
        assert!(inject(&mut plain, &specs).unwrap().is_empty());
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
        let injected = inject(&mut func, &specs).unwrap();
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
        assert_eq!(inject(&mut func, &specs).unwrap(), vec!["marker_account"]);

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
        assert!(inject(&mut func, &gate_specs()).unwrap().is_empty());
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
        assert!(inject(&mut func, &specs).unwrap().is_empty());
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
        let injected = inject(&mut func, &specs).unwrap();
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
        let injected = inject(&mut func, &specs).unwrap();
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
        let err = inject(&mut func, &specs).expect_err("conflicting constraints must be rejected");
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
            GateLocations::Emit,
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
                Embed {
                    source: "ext_a".to_string(),
                    carrier: None,
                    state_type: "ext_a::CfgA".to_string(),
                    decl: crate::extension::EmbedDecl {
                        role: "cfg_a".to_string(),
                        account: "shared".to_string(),
                        offset: OffsetSpec::Literal(32),
                        initializer: None,
                    },
                },
                Embed {
                    source: "ext_b".to_string(),
                    carrier: None,
                    state_type: "ext_b::CfgB".to_string(),
                    decl: crate::extension::EmbedDecl {
                        role: "cfg_b".to_string(),
                        account: "shared".to_string(),
                        offset: OffsetSpec::Literal(off_b),
                        initializer: None,
                    },
                },
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
