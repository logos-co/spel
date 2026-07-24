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

/// Prepend each active wrap's attribute to `func`, then inject any
/// gate params those wrappers need. Shared between the program-macro
/// dispatcher and both IDL paths so all three see the same accounts.
///
/// `qualified = None` for consumer-authored fns: only the per-fn
/// `self_exempt_marker` opts out. `Some("crate::fn_name")` for
/// extension-provided fns: the `exempt` qualified-name list is also
/// consulted so a wrap can carve out an extension it depends on.
///
/// Returns the names of the params `inject_gate_params` synthesized.
///
/// # Errors
///
/// Propagates `inject_gate_params` errors and fails when a declared
/// wrapper path is not a valid Rust attribute path.
pub fn apply_wrap_and_inject(
    func: &mut ItemFn,
    active_wraps: &[WrapInstructions],
    inject_specs: &[InjectSpec],
    qualified: Option<&str>,
) -> Result<Vec<String>, String> {
    let remap = build_remap(inject_specs, func);
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
            for acc in &spec.accounts {
                let resolved = remap
                    .get(&acc.name)
                    .cloned()
                    .unwrap_or_else(|| acc.name.clone());
                let key = syn::Ident::new(&acc.name, proc_macro2::Span::call_site());
                let val = syn::Ident::new(&resolved, proc_macro2::Span::call_site());
                args.push(parse_quote! { #key = # val});
            }
        }
        let attr: Attribute = if args.is_empty() {
            parse_quote! { #[#wrapper_path] }
        } else {
            parse_quote! { #[#wrapper_path(#(#args), *)] }
        };
        func.attrs.insert(0, attr);
    }
    inject_gate_params(func, inject_specs)
}

/// Inject a wrapper's missing gate params into an instruction fn.
///
/// Skip-if-declared: a param that exists is never touched. A wrapper
/// attr carrying arguments (custom target names) disables injection for
/// that fn: renaming is manual mode, the consumer declares its own
/// params. The wrapper is matched by the attr path's last segment, so
/// the fully qualified attrs prepended by auto-wrap activate specs too.
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
    for input in &func.sig.inputs {
        let FnArg::Typed(pt) = input else { continue };
        let is_signer = pt.attrs.iter().any(|a| {
            if !a.path().is_ident("account") {
                return false;
            }
            let mut hit = false;
            let _ = a.parse_nested_meta(|meta| {
                if meta.path.is_ident("signer") {
                    hit = true;
                }
                Ok(())
            });
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
    for input in &func.sig.inputs {
        let FnArg::Typed(pt) = input else { continue };
        let syn::Pat::Ident(pi) = &*pt.pat else {
            continue;
        };
        for attr in &pt.attrs {
            if !attr.path().is_ident("account") {
                continue;
            }
            let mut matched = false;
            let _ = attr.parse_nested_meta(|meta| {
                if !meta.path.is_ident("pda") {
                    return Ok(());
                }
                let expr: syn::Expr = meta.value()?.parse()?;
                if expr_is_literal_call(&expr, literal) {
                    matched = true;
                }
                Ok(())
            });
            if matched {
                return Some(pi.ident.to_string());
            }
        }
    }
    None
}

fn expr_is_literal_call(expr: &syn::Expr, target: &str) -> bool {
    let syn::Expr::Call(call) = expr else {
        return false;
    };
    let syn::Expr::Path(p) = &*call.func else {
        return false;
    };
    if !p.path.is_ident("literal") {
        return false;
    }
    let Some(syn::Expr::Lit(lit_expr)) = call.args.first() else {
        return false;
    };
    let syn::Lit::Str(s) = &lit_expr.lit else {
        return false;
    };
    s.value() == target
}

fn find_pda_compound_param(
    func: &ItemFn,
    seeds: &[InjectSeed],
    remap: &HashMap<String, String>,
) -> Option<String> {
    let target: Vec<String> = seeds
        .iter()
        .map(|s| match s {
            InjectSeed::Const(v) => format!("literal:{v}"),
            InjectSeed::Account(v) => {
                let resolved = remap.get(v).map(String::as_str).unwrap_or(v);
                format!("account:{resolved}")
            },
        })
        .collect();

    for input in &func.sig.inputs {
        let FnArg::Typed(pt) = input else { continue };
        let syn::Pat::Ident(pi) = &*pt.pat else {
            continue;
        };
        for attr in &pt.attrs {
            if !attr.path().is_ident("account") {
                continue;
            }
            let mut candidate: Vec<String> = Vec::new();
            let _ = attr.parse_nested_meta(|meta| {
                if !meta.path.is_ident("pda") {
                    return Ok(());
                }
                let expr: syn::Expr = meta.value()?.parse()?;
                let syn::Expr::Array(arr) = expr else {
                    return Ok(());
                };
                for elem in &arr.elems {
                    if let Some(tag) = seed_expr_to_tag(elem) {
                        candidate.push(tag);
                    }
                }
                Ok(())
            });
            if candidate == target {
                return Some(pi.ident.to_string());
            }
        }
    }
    None
}

fn seed_expr_to_tag(expr: &syn::Expr) -> Option<String> {
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
        Some(format!("literal:{}", s.value()))
    } else if p.path.is_ident("account") {
        Some(format!("account:{}", s.value()))
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

#[cfg(test)]
mod tests {
    use super::*;
    use syn::Pat;
    fn gate_specs() -> Vec<InjectSpec> {
        vec![InjectSpec {
            wrapper: "my_gate".to_string(),
            source: "ext-a".into(),
            accounts: vec![
                InjectAccount {
                    name: "gate_config".into(),
                    seeds: vec![InjectSeed::Const("gate_config".into())],
                    signer: false,
                },
                InjectAccount {
                    name: "caller".into(),
                    seeds: vec![],
                    signer: true,
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
                seeds: vec![
                    InjectSeed::Const("marker".into()),
                    InjectSeed::Account("caller".into()),
                ],
                signer: false,
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
                    seeds: vec![
                        InjectSeed::Const("frozen".into()),
                        InjectSeed::Account("caller".into()),
                    ],
                    signer: false,
                },
                InjectAccount {
                    name: "caller".into(),
                    seeds: vec![],
                    signer: true,
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
                seeds: vec![InjectSeed::Const("other_config".into())],
                signer: false,
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
                seeds: vec![],
                signer: true,
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
                seeds: vec![InjectSeed::Const("caller_pda".into())],
                signer: false,
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
}
