//! Extension discovery for SPEL programs.
//!
//! Scans the consuming program's **direct** dependencies (path, git, or
//! registry, resolved by [`crate::dep_walk`]) for crates that declare
//! `[package.metadata.spel]` in their `Cargo.toml`. Each qualifying
//! crate contributes through one entry point:
//!
//! - [`discover_extensions`] returns an [`ExtensionDiscoveries`]: the
//!   cross-crate `#[instruction]` fns to be merged into the consumer's
//!   dispatcher and IDL, the gate param inject specs applied by
//!   [`inject_gate_params`], and the wrap configs. Gate and marker
//!   attrs stay on emitted handler fns and expand there as ordinary
//!   proc-macros: a gate rewrites the handler body, a marker expands
//!   to nothing. Nothing is stripped.
//! - [`check_duplicate_instruction_names`] rejects name collisions
//!   between user fns and discovered extensions (or two extensions)
//!   before they become colliding enum variants, match arms, or IDL
//!   discriminators. All producers run it after assembly.
//!
//! # Trust model
//!
//! Activating an extension takes two explicit consumer actions: the
//! dependency listed in the consumer's own `Cargo.toml` and the marker
//! attr on the `#[lez_program]` module. Discovery is deliberately not
//! transitive, so a dependency of a dependency can never contribute
//! instructions by claiming a matching `extension_attr`. Generated
//! cross-crate call paths use the dependency's `[package].name`, never
//! its directory name.
//!
//! # Failure tiers
//!
//! Malformed `[package.metadata.spel]` (a key with the wrong shape) is a
//! hard `Err`: callers surface it as a compile error, a broken extension
//! declaration must never degrade to a program silently missing its
//! extension surface. Environmental issues (unreadable manifest, path
//! dep pointing at a missing directory, a matched extension contributing
//! nothing) are reported through the `on_warning` channel, following the
//! `find_path_dep_dirs` precedent.
//!
//! Feature-gated identically to [`crate::idl_gen`]
//! (`#[cfg(feature = "idl-gen")]`) since it depends on `syn` and `toml`.
//! Internal helpers (`read_spel_extension_attr`,
//! `read_spel_inject_specs`, `collect_instruction_fns`) are
//! module-private; producers go through [`resolve_program_deps`],
//! and [`discover_extensions`] stays public for callers that already
//! hold a resolved graph.

use std::path::{Path, PathBuf};

use syn::{parse_quote, Attribute, FnArg, ItemFn};

use crate::idl_gen::{collect_items_from_crate_dirs, has_instruction_attr};

/// What the consumer's direct dependencies contribute to its program:
/// cross-crate instruction fns, gate param inject specs, and
/// library-owned gate attribute names, collected in one pass per
/// dependency.
#[derive(Debug, Default)]
pub struct ExtensionDiscoveries {
    /// One entry per discovered `#[instruction]` fn, with the absolute
    /// crate path to call it from the consumer (e.g. `::admin_authority`),
    /// derived from the dependency's `[package].name`.
    pub instructions: Vec<(ItemFn, syn::Path)>,
    /// Gate param injection specs the libraries declare. Instructions
    /// carrying a spec's bare wrapper attr get missing params
    /// synthesized by [`inject_gate_params`].
    pub inject_specs: Vec<InjectSpec>,
    /// Active wrap configs, paired with the consumer marker attr's arg
    /// (`""` for a bare marker) so callers can honor `skip`.
    pub wraps: Vec<(String, WrapInstructions)>,
}
#[derive(Debug, Default)]
/// Everything a producer needs from the dependency side, resolved in
/// one call by [`resolve_program_deps`].
pub struct ProgramDeps {
    /// The dependency graph, one `cargo metadata` invocation at most.
    pub graph: crate::dep_walk::DepGraph,
    /// What matched extensions contribute to the program.
    pub extensions: ExtensionDiscoveries,
}

/// One component of an injected account's PDA seed.
#[derive(Debug, PartialEq)]
pub enum InjectSeed {
    /// Literal string seed, emitted as `pda = literal("...")`.
    Const(String),
    /// Seed derived from another account's `AccountId`, emitted as
    /// `pda = account("...")`. The string names a param of the same
    /// gated instructions,
    Account(String),
}

/// One account a gate wrapper needs injected.
#[derive(Debug, PartialEq)]
pub struct InjectAccount {
    /// Param name the gate matches by.
    pub name: String,
    /// Ordered PDA seed components. Empty = plain account (no PDA),
    /// one = single-seed PDA, multiple = compound PDA.
    pub seeds: Vec<InjectSeed>,
    /// Whether the param carries `#[account(signer)]`.
    pub signer: bool,
}

/// One `[[package.metadata.spel.inject]]` block: which wrapper attr it
/// serves and the accounts to inject when a gated fn omits them.
#[derive(Debug)]
pub struct InjectSpec {
    /// Wrapper attr name (e.g. `require_admin`) that activates this spec.
    pub wrapper: String,
    /// Accounts to synthesize, in declaration order.
    pub accounts: Vec<InjectAccount>,
    // Crate name of the extension that declared this spec. Names the
    // offender when two extensions inject conflicting params.
    pub source: String,
}

/// Parsed `[package.metadata.spel.wrap_instructions]` for an extension
/// lib. Declares the per-instruction wrap the extension wants applied
/// to every instruction the consumer's dispatcher ships, the
/// consumer's own and discovered ones alike. Consumer fns opt out per
/// fn via `self_exempt_marker`; discovered fns have no source site to
/// annotate, so cross-crate carve-outs go in `exempt` by qualified
/// name.
#[derive(Debug, Clone)]
pub struct WrapInstructions {
    /// Proc-macro attribute the framework prepends to each non-exempt fn.
    pub wrapper: String,
    /// Marker attr arg that disables wrap (e.g. `"manual"`). `None` when
    /// the extension offers no opt-out word: wrap is then always active
    /// for consumers that carry the marker.
    pub skip: Option<String>,
    /// Per-fn opt-out attribute name (e.g. `"freeze_exempt`).
    pub self_exempt_marker: String,
    /// Fully-qualified instructions from other crates to skip
    /// unconditionally.
    pub exempt: Vec<String>,
}

struct MatchedExtension {
    marker_pos: usize,
    instructions: Vec<(ItemFn, syn::Path)>,
    inject_specs: Vec<InjectSpec>,
    wraps: Vec<(String, WrapInstructions)>,
}

/// Parsed Cargo.toml of a dependency dir, or `None` when unreadable or
/// unparseable. Silent: cargo itself fails the build for those cases.
fn read_manifest_value(crate_dir: &Path) -> Option<toml::Value> {
    let content = std::fs::read_to_string(crate_dir.join("Cargo.toml")).ok()?;
    toml::from_str(&content).ok()
}

/// Producer entry point: marker pre-check, graph resolution, and
/// extension discovery in one call. Modules without candidate markers
/// skip the cargo metadata walk and discovery entirely.
///
/// # Errors
///
/// `Err` on malformed spel metadata or a marker placed above
/// `#[lez_program]`; callers surface it as a compile error.
pub fn resolve_program_deps<F: FnMut(String)>(
    start: &Path,
    mod_attrs: &[Attribute],
    on_warning: &mut F,
) -> Result<ProgramDeps, String> {
    let with_metadata = has_extension_marker_candidates(mod_attrs);
    let graph = crate::dep_walk::resolve_dep_graph(start, with_metadata, on_warning);
    let extensions = if with_metadata {
        discover_extensions(&graph.direct_dirs, mod_attrs, on_warning)?
    } else {
        ExtensionDiscoveries::default()
    };
    Ok(ProgramDeps { graph, extensions })
}

/// Scan `dep_dirs` (the consumer's direct dependencies) for SPEL
/// extension libraries whose `extension_attr` metadata matches an
/// attribute on the consuming program's mod.
///
/// Contributions are ordered by the marker attrs' positions on the
/// module, first marker first. That order is the cross-extension ABI:
/// it decides instruction order in the dispatcher and IDL, and the
/// account order of injected params.
///
/// # Errors
///
/// `Err` on malformed spel metadata (callers surface it as a compile
/// error). Environmental skips are reported via `on_warning`.
pub fn discover_extensions<F: FnMut(String)>(
    dep_dirs: &[PathBuf],
    mod_attrs: &[Attribute],
    on_warning: &mut F,
) -> Result<ExtensionDiscoveries, String> {
    let mut matched: Vec<MatchedExtension> = Vec::new();

    let lez_pos = mod_attrs
        .iter()
        .position(|a| a.path().is_ident("lez_program"));
    for dep_dir in dep_dirs {
        let Some(manifest_value) = read_manifest_value(dep_dir) else {
            continue;
        };
        let Some(ext_attr) = read_spel_extension_attr(&manifest_value, dep_dir)? else {
            continue;
        };
        if !mod_attrs.iter().any(|a| a.path().is_ident(&ext_attr)) {
            continue;
        }
        if let (Some(lez), Some(marker)) = (
            lez_pos,
            mod_attrs.iter().position(|a| a.path().is_ident(&ext_attr)),
        ) {
            if marker < lez {
                return Err(format!(
                    "extension marker #[{ext_attr}] is above #[lez_program]: attributes \
                    above expand first and are invisible to the compiled program, so the \
                    extension would appear in the IDL but not in the dispatcher. Move \
                    #[{ext_attr}] below #[lez_program]."
                ));
            }
        }
        let Some(crate_name) = read_package_ident(&manifest_value) else {
            on_warning(format!(
                "extension at '{}' matched module attribute but has no [package].name, skipped",
                dep_dir.display()
            ));
            continue;
        };

        let marker_pos = mod_attrs
            .iter()
            .position(|a| a.path().is_ident(&ext_attr))
            .unwrap_or(usize::MAX);
        let mut injects = read_spel_inject_specs(&manifest_value, dep_dir)?;
        for spec in &mut injects {
            spec.source = crate_name.clone();
        }
        let wrap = read_spel_wrap_instructions(&manifest_value, dep_dir)?;
        let has_wrap = wrap.is_some();
        let mut wraps = Vec::new();
        if let Some(w) = wrap {
            let arg = mod_attrs
                .iter()
                .find_map(|a| extract_attr_arg(a, &ext_attr))
                .unwrap_or_default();
            wraps.push((arg, w));
        }

        let crate_ident = syn::Ident::new(&crate_name, proc_macro2::Span::call_site());
        let crate_path: syn::Path = syn::parse_quote!(::#crate_ident);

        let (items, _) = collect_items_from_crate_dirs(std::slice::from_ref(dep_dir));
        let funcs = collect_instruction_fns(&items);
        if funcs.is_empty() && injects.is_empty() && !has_wrap {
            on_warning(format!(
                "extension '{crate_name}' matched #[{ext_attr}] but contributes no \
                #[instruction] fns, no inject specs, and no wrap config"
            ));
        }
        let mut instructions = Vec::new();
        for func in funcs {
            instructions.push((func, crate_path.clone()));
        }
        matched.push(MatchedExtension {
            marker_pos,
            instructions,
            inject_specs: injects,
            wraps,
        });
    }

    Ok(flatten_in_marker_order(matched))
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
pub fn inject_gate_params(func: &mut ItemFn, specs: &[InjectSpec]) -> Result<Vec<String>, String> {
    let mut injected = Vec::new();
    // name -> (account def, source extension) for params this call added
    let mut injected_by: std::collections::HashMap<String, (&InjectAccount, &str)> =
        std::collections::HashMap::new();
    let mut pos = insert_position(func);

    for spec in specs {
        let activates = func.attrs.iter().any(|a| {
            a.path()
                .segments
                .last()
                .is_some_and(|s| s.ident == spec.wrapper)
                && matches!(a.meta, syn::Meta::Path(_))
        });
        if !activates {
            continue;
        }

        for acc in &spec.accounts {
            if let Some((existing, source)) = injected_by.get(acc.name.as_str()) {
                if *existing == acc {
                    continue; // identical constraints: one shared account
                }
                return Err(format!(
                    "extension '{source}' and '{}' both inject param '{}' with \
                    conflicting constraints",
                    spec.source, acc.name
                ));
            }
            if has_param_named(func, &acc.name) {
                continue; // consumer declared it: declared win
            }
            func.sig.inputs.insert(pos, build_inject_param(acc));
            pos += 1;
            injected.push(acc.name.clone());
            injected_by.insert(acc.name.clone(), (acc, spec.source.as_str()));
        }
    }
    Ok(injected)
}

/// Filter `#[instruction]`-annotated fns from a flat item list.
///
/// Used by framework codegen to pull instruction definitions out of
/// extension libraries (e.g. admin-authority) that ship pre-defined
/// instructions to be merged into a consuming program's IDL + dispatcher.
fn collect_instruction_fns(items: &[syn::Item]) -> Vec<ItemFn> {
    items
        .iter()
        .filter_map(|it| match it {
            syn::Item::Fn(f) if has_instruction_attr(&f.attrs) => Some(f.clone()),
            _ => None,
        })
        .collect()
}

/// Read `[package.metadata.spel.extension_attr]` from a crate's Cargo.toml.
///
/// Used by framework codegen to discover libraries that opt into providing
/// instructions to consuming programs. The lib declares the marker attr
/// name (e.g. `"admin_authority"`); when the consuming program puts that
/// attr on its `#[lez_program]` mod, the framework includes the lib's
/// `#[instruction]` fns in the dispatcher + IDL.
///
/// Ok(None): no spel metadata or no extension_attr_key (a normal crate).
/// Ok(Some(name)): declared extension.
/// Err(msg): metadata present but malformed. Callers must surface this as
/// a hard error, a broken declaration must never degrade to "no extension".
fn read_spel_extension_attr(
    value: &toml::Value,
    crate_dir: &Path,
) -> Result<Option<String>, String> {
    let Some(spel) = value
        .get("package")
        .and_then(|p| p.get("metadata"))
        .and_then(|m| m.get("spel"))
    else {
        return Ok(None);
    };
    match spel.get("extension_attr") {
        None => Ok(None),
        Some(v) => match v.as_str() {
            Some(s) => Ok(Some(s.to_string())),
            None => Err(format!(
                "malformed [package.metadata.spel] in {}: extension_attr must be a string",
                crate_dir.join("Cargo.toml").display()
            )),
        },
    }
}

/// Read `[[package.metadata.spel.inject]]` blocks from a parsed manifest.
///
/// Absent inject key is `Ok(empty)`, a normal extension without param
/// injection. Malformed blocks are a hard `Err`: a broken inject
/// declaration must never degrade to a gate silently missing its
/// account constraints.
fn read_spel_inject_specs(
    value: &toml::Value,
    crate_dir: &Path,
) -> Result<Vec<InjectSpec>, String> {
    let manifest = crate_dir.join("Cargo.toml");
    let malformed = |what: &str| {
        format!(
            "malformed [package.metadata.spel] in {}: {what}",
            manifest.display()
        )
    };

    let Some(injects) = value
        .get("package")
        .and_then(|p| p.get("metadata"))
        .and_then(|m| m.get("spel"))
        .and_then(|s| s.get("inject"))
    else {
        return Ok(vec![]);
    };
    let Some(injects) = injects.as_array() else {
        return Err(malformed("inject must be an array of tables"));
    };

    let mut specs = Vec::new();
    for inject in injects {
        let Some(wrapper) = inject.get("wrapper").and_then(|w| w.as_str()) else {
            return Err(malformed("inject.wrapper must be a string"));
        };
        let Some(accs) = inject.get("account").and_then(|a| a.as_array()) else {
            return Err(malformed("inject.account must be an array of tables"));
        };
        let mut accounts = Vec::new();
        for acc in accs {
            let Some(name) = acc.get("name").and_then(|n| n.as_str()) else {
                return Err(malformed("inject.account.name must be a string"));
            };
            let seeds = match acc.get("seed") {
                None => vec![],
                Some(toml::Value::Array(entries)) => entries
                    .iter()
                    .map(|e| {
                        parse_seed_entry(e).ok_or_else(|| {
                            malformed(
                                "inject.account.seed entries must be { const = \"...\" } or { account = \"...\" }",
                            )
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                Some(single) => vec![parse_seed_entry(single).ok_or_else(|| {
                    malformed(
                        "inject.account.seed must be { const = \"...\" }, { account = \"...\" }, or an array of those",
                    )
                })?],
            };
            let signer = match acc.get("signer") {
                None => false,
                Some(b) => b
                    .as_bool()
                    .ok_or_else(|| malformed("inject.account.signer must be a boolean"))?,
            };
            accounts.push(InjectAccount {
                name: name.to_string(),
                seeds,
                signer,
            });
        }
        specs.push(InjectSpec {
            wrapper: wrapper.to_string(),
            accounts,
            source: String::new(),
        });
    }
    Ok(specs)
}

/// Read `[package.metadata.spel.wrap_instructions` from parsed
/// manifest.
///
/// Absent section in `Ok(None)`, the extension does not wrap. A present
/// but malformed section is a hard `Err`: a broken wrap declaration
/// must never degrade to a program silently shipping unwrapped
/// instructions.
fn read_spel_wrap_instructions(
    value: &toml::Value,
    crate_dir: &Path,
) -> Result<Option<WrapInstructions>, String> {
    let manifest = crate_dir.join("Cargo.toml");
    let malformed = |what: &str| {
        format!(
            "malformed [package.metadata.spel] in {}: {what}",
            manifest.display()
        )
    };

    let Some(wrap) = value
        .get("package")
        .and_then(|p| p.get("metadata"))
        .and_then(|m| m.get("spel"))
        .and_then(|s| s.get("wrap_instructions"))
    else {
        return Ok(None);
    };

    let Some(wrapper) = wrap.get("wrapper").and_then(|w| w.as_str()) else {
        return Err(malformed("wrap_instructions.wrapper must be a string"));
    };
    let Some(self_exempt_marker) = wrap.get("self_exempt_marker").and_then(|w| w.as_str()) else {
        return Err(malformed(
            "wrap_instructions.self_exempt_marker must be a string",
        ));
    };
    let skip = match wrap.get("skip") {
        None => None,
        Some(v) => {
            let Some(s) = v.as_str() else {
                return Err(malformed("wrap_instructions.skip must be a string"));
            };
            if s.is_empty() {
                return Err(malformed(
                    "wrap_instructions.skip must not be empty: an empty skip word \
                    matches the bare marker and turns wrap off for every consumer",
                ));
            }
            Some(s.to_string())
        },
    };
    let exempt = match wrap.get("exempt") {
        None => vec![],
        Some(v) => {
            let Some(arr) = v.as_array() else {
                return Err(malformed(
                    "wrap_instructions.exempt must be an array of string",
                ));
            };
            arr.iter()
                .map(|x| {
                    x.as_str().map(String::from).ok_or_else(|| {
                        malformed("wrap_instructions.exempt must be and array of strings")
                    })
                })
                .collect::<Result<Vec<_>, _>>()?
        },
    };

    Ok(Some(WrapInstructions {
        wrapper: wrapper.to_string(),
        skip,
        self_exempt_marker: self_exempt_marker.to_string(),
        exempt,
    }))
}

/// Reject duplicate instruction names across user fns and discovered
/// extensions. Duplicates would produce colliding enum variants, match
/// arms, and IDL discriminators, or silently shadow one another.
/// `instructions` yields `(fn name, source_label)`; first seen wins.
///
/// # Errors
///
/// `Err` on the second sighting of a name, naming both sources.
pub fn check_duplicate_instruction_names<I>(instructions: I) -> Result<(), String>
where
    I: IntoIterator<Item = (String, String)>,
{
    let mut seen: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (name, source) in instructions {
        if let Some(first) = seen.get(&name) {
            return Err(format!(
                "duplicate instruction name '{name}': defined in {first} and in {source}"
            ));
        }
        seen.insert(name, source);
    }
    Ok(())
}

/// Human label for a duplicate-name report: which side owns the fn.
pub fn instruction_source_label(external_call_path: Option<&syn::Path>) -> String {
    match external_call_path {
        Some(p) => match p.segments.first() {
            Some(seg) => format!("extension {}", seg.ident),
            None => "an extension".to_string(),
        },
        None => "this module".to_string(),
    }
}

/// [package].name from crate dir's manifest, `-` mapped to `_`, for use
/// as the crate ident in generated cross-crate call paths. The directory
/// name is not the crate's identity: renamed checkouts, vendoring, and
/// `package = ` aliases all diverge from it.
fn read_package_ident(value: &toml::Value) -> Option<String> {
    let name = value.get("package")?.get("name")?.as_str()?;
    Some(name.replace('-', "_"))
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

/// True if the fn already declares a param with this name.
fn has_param_named(func: &ItemFn, name: &str) -> bool {
    func.sig.inputs.iter().any(|input| {
        matches!(input, FnArg::Typed(pt)
            if matches!(&*pt.pat, syn::Pat::Ident(pi) if pi.ident == name))
    })
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

/// Extract the single-ident arg from an attribute whose path matches
/// `ext_attr`. Returns `Some("")` for bare attribute, `Some(ident)`
/// for `#[name(ident)]`, and `None` if the path doesn't match or the
/// args aren't a single ident.
fn extract_attr_arg(attr: &Attribute, ext_attr: &str) -> Option<String> {
    if !attr.path().is_ident(ext_attr) {
        return None;
    }
    if matches!(attr.meta, syn::Meta::Path(_)) {
        return Some(String::new());
    }
    attr.parse_args::<syn::Ident>().ok().map(|i| i.to_string())
}

/// Decode one TOML seed entry (`{ const = "..." }` or
/// `{ account "..." }`) into an [`InjectSeed`].
fn parse_seed_entry(value: &toml::Value) -> Option<InjectSeed> {
    let t = value.as_table()?;
    if let Some(s) = t.get("const").and_then(|c| c.as_str()) {
        return Some(InjectSeed::Const(s.to_string()));
    }
    if let Some(s) = t.get("account").and_then(|a| a.as_str()) {
        return Some(InjectSeed::Account(s.to_string()));
    }
    None
}

// Render one injected account as a typed fn param carrying its
// `#[account(...)]` constraint.
fn build_inject_param(acc: &InjectAccount) -> FnArg {
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
                let lit = syn::LitStr::new(v, proc_macro2::Span::call_site());
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

/// Flatten matched extensions in marker order. The first marker on the
/// module contributes first, everywhere downstream: dispatcher, IDL,
/// and injected params. Marker order is the cross-extension ABI order.
fn flatten_in_marker_order(mut matched: Vec<MatchedExtension>) -> ExtensionDiscoveries {
    matched.sort_by_key(|m| m.marker_pos);
    let mut out = ExtensionDiscoveries::default();
    for m in matched {
        out.instructions.extend(m.instructions);
        out.inject_specs.extend(m.inject_specs);
        out.wraps.extend(m.wraps);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::TempDir;
    use syn::Pat;

    /// Old two-call discovery shape, kept for the tests: resolve the
    /// graph, then split the discoveries.
    fn discover_instructions<F: FnMut(String)>(
        dir: &Path,
        mod_attrs: &[Attribute],
        on_warning: &mut F,
    ) -> Result<Vec<(ItemFn, syn::Path)>, String> {
        let graph = crate::dep_walk::resolve_dep_graph(dir, true, on_warning);
        Ok(discover_extensions(&graph.direct_dirs, mod_attrs, on_warning)?.instructions)
    }

    fn wrap_fixture(tmp: &TempDir, wrap_toml: &str) {
        ext_fixture(
            tmp,
            &format!(
                r#"
[package.metadata.spel]
extension_attr = "my_ext"

{wrap_toml}
"#
            ),
            r#"
#[instruction]
pub fn ext_action(account: AccountWithMetadata) -> SpelResult { todo!() }
"#,
        );
    }

    #[test]
    fn read_spel_extension_attr_returns_declared_name() {
        let tmp = TempDir::new("ext-attr-present");
        tmp.write(
            "Cargo.toml",
            r#"
[package]
name = "my-ext"
version = "0.1.0"
edition = "2021"

[package.metadata.spel]
extension_attr = "my_ext"
"#,
        );
        let value = read_manifest_value(tmp.path()).unwrap();
        assert_eq!(
            read_spel_extension_attr(&value, tmp.path()).unwrap(),
            Some("my_ext".to_string())
        );
    }

    #[test]
    fn read_spel_extension_attr_none_when_missing() {
        let tmp = TempDir::new("ext-attr-absent");
        tmp.write(
            "Cargo.toml",
            "[package]\nname = \"my-ext\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        let value = read_manifest_value(tmp.path()).unwrap();
        assert!(read_spel_extension_attr(&value, tmp.path())
            .unwrap()
            .is_none());
    }

    #[test]
    fn read_manifest_value_none_when_no_manifest() {
        let tmp = TempDir::new("ext-attr-no-manifest");
        assert!(read_manifest_value(tmp.path()).is_none());
    }

    #[test]
    fn discover_extension_instructions_picks_up_matching_ext() {
        let tmp = TempDir::new("discover-match");

        // Extension crate at <tmp>/my-ext/
        tmp.write(
            "my-ext/Cargo.toml",
            r#"
[package]
name = "my-ext"
version = "0.1.0"
edition = "2021"

[package.metadata.spel]
extension_attr = "my_ext"
"#,
        );
        tmp.write(
            "my-ext/src/lib.rs",
            r#"
#[instruction]
pub fn ext_action(account: AccountWithMetadata) -> SpelResult { todo!() }
"#,
        );

        // User crate at <tmp>/user/ depending on my-ext
        tmp.write(
            "user/Cargo.toml",
            r#"
[package]
name = "user"
version = "0.1.0"
edition = "2021"

[dependencies]
my-ext = { path = "../my-ext" }
"#,
        );
        tmp.write("user/src/lib.rs", "");

        // mod_attrs simulating: #[lez_program] #[my_ext] mod user { ... }
        let mod_attrs: Vec<Attribute> = syn::parse_quote!(
            #[lez_program]
            #[my_ext]
        );

        let found =
            discover_instructions(&tmp.path().join("user"), &mod_attrs, &mut |_| {}).unwrap();
        assert_eq!(found.len(), 1);
        let (func, crate_path) = &found[0];
        assert_eq!(func.sig.ident.to_string(), "ext_action");
        let segs: Vec<String> = crate_path
            .segments
            .iter()
            .map(|s| s.ident.to_string())
            .collect();
        assert_eq!(segs, vec!["my_ext".to_string()]);
        assert!(
            crate_path.leading_colon.is_some(),
            "path must start with ::"
        );
    }

    #[test]
    fn discovery_uses_package_name_not_dir_name() {
        let tmp = TempDir::new("discover-renamed-dir");

        // Extension checked out under a directory that does NOT match its
        // package name (renamed checkout / vendored copy).
        tmp.write(
            "renamed-checkout/Cargo.toml",
            r#"
[package]
name = "my-ext"
version = "0.1.0"
edition = "2021"

[package.metadata.spel]
extension_attr = "my_ext"
"#,
        );
        tmp.write(
            "renamed-checkout/src/lib.rs",
            r#"
#[instruction]
pub fn ext_action(account: AccountWithMetadata) -> SpelResult { todo!() }
"#,
        );
        tmp.write(
            "user/Cargo.toml",
            r#"
[package]
name = "user"
version = "0.1.0"
edition = "2021"

[dependencies]
my-ext = { path = "../renamed-checkout" }
"#,
        );
        tmp.write("user/src/lib.rs", "");

        let mod_attrs: Vec<Attribute> = syn::parse_quote!(
            #[lez_program]
            #[my_ext]
        );

        let found =
            discover_instructions(&tmp.path().join("user"), &mod_attrs, &mut |_| {}).unwrap();
        assert_eq!(found.len(), 1);
        let (_, crate_path) = &found[0];
        let segs: Vec<String> = crate_path
            .segments
            .iter()
            .map(|s| s.ident.to_string())
            .collect();
        // Identity comes from [package].name, never from the directory name:
        // ::my_ext, not ::renamed_checkout.
        assert_eq!(segs, vec!["my_ext".to_string()]);
    }

    #[test]
    fn transitive_extension_is_not_discovered() {
        let tmp = TempDir::new("discover-transitive");

        // Innocent direct dep with no extension metadata...
        tmp.write(
            "helper/Cargo.toml",
            r#"
[package]
name = "helper"
version = "0.1.0"
edition = "2021"

[dependencies]
evil-ext = { path = "../evil-ext" }
"#,
        );
        tmp.write("helper/src/lib.rs", "");

        // ...pulling in a transitive crate that claims the consumer's
        // marker attr and ships instructions.
        tmp.write(
            "evil-ext/Cargo.toml",
            r#"
[package]
name = "evil-ext"
version = "0.1.0"
edition = "2021"

[package.metadata.spel]
extension_attr = "my_ext"
"#,
        );
        tmp.write(
            "evil-ext/src/lib.rs",
            r#"
#[instruction]
pub fn smuggled(account: AccountWithMetadata) -> SpelResult { todo!() }
"#,
        );
        tmp.write(
            "user/Cargo.toml",
            r#"
[package]
name = "user"
version = "0.1.0"
edition = "2021"

[dependencies]
helper = { path = "../helper" }
"#,
        );
        tmp.write("user/src/lib.rs", "");

        // Consumer opted into #[my_ext], but no DIRECT dep declares it.
        let mod_attrs: Vec<Attribute> = syn::parse_quote!(
            #[lez_program]
            #[my_ext]
        );

        let found =
            discover_instructions(&tmp.path().join("user"), &mod_attrs, &mut |_| {}).unwrap();
        assert!(
            found.is_empty(),
            "transitive dep must never contribute instructions, got: {:?}",
            found
                .iter()
                .map(|(f, _)| f.sig.ident.to_string())
                .collect::<Vec<_>>()
        );
    }

    fn ext_fixture(tmp: &TempDir, metadata: &str, lib_rs: &str) -> Vec<Attribute> {
        tmp.write(
            "my-ext/Cargo.toml",
            &format!(
                r#"
[package]
name = "my-ext"
version = "0.1.0"
edition = "2021"

{metadata}
"#
            ),
        );
        tmp.write("my-ext/src/lib.rs", lib_rs);
        tmp.write(
            "user/Cargo.toml",
            r#"
[package]
name = "user"
version = "0.1.0"
edition = "2021"

[dependencies]
my-ext = { path = "../my-ext" }
"#,
        );
        tmp.write("user/src/lib.rs", "");
        syn::parse_quote!(
            #[lez_program]
            #[my_ext]
        )
    }

    #[test]
    fn resolve_program_deps_discovers_through_one_call() {
        let tmp = TempDir::new("program-deps-match");
        let mod_attrs = ext_fixture(
            &tmp,
            r#"
[package.metadata.spel]
extension_attr = "my_ext"
"#,
            r#"
#[instruction]
pub fn ext_action(account: AccountWithMetadata) -> SpelResult { todo!() }
"#,
        );

        let deps = resolve_program_deps(&tmp.path().join("user"), &mod_attrs, &mut |_| {})
            .expect("discovery through the producer entry point");
        assert_eq!(deps.extensions.instructions.len(), 1);
        assert!(
            deps.graph.direct_dirs.iter().any(|d| d.ends_with("my-ext")),
            "graph must contain the extension dir: {:?}",
            deps.graph.direct_dirs
        );
    }

    #[test]
    fn resolve_program_deps_without_markers_skips_discovery_and_metadata() {
        let tmp = TempDir::new("program-deps-no-marker");
        // Extension exists as a dependency, and the consumer manifest also
        // carries an unfetchable git dep: if `cargo metadata` ran it would
        // warn, and if discovery ran it would read the extension manifest.
        ext_fixture(
            &tmp,
            r#"
[package.metadata.spel]
extension_attr = "my_ext"
"#,
            r#"
#[instruction]
pub fn ext_action(account: AccountWithMetadata) -> SpelResult { todo!() }
"#,
        );
        tmp.write(
            "user/Cargo.toml",
            r#"
[package]
name = "user"
version = "0.1.0"
edition = "2021"

[dependencies]
my-ext = { path = "../my-ext" }
nssa_core = { git = "https://example.com/repo.git", tag = "v1.0" }
"#,
        );

        let mod_attrs: Vec<Attribute> = syn::parse_quote!(
            #[doc = "no markers here"]
        );
        let mut warnings = Vec::new();
        let deps = resolve_program_deps(&tmp.path().join("user"), &mod_attrs, &mut |w| {
            warnings.push(w)
        })
        .expect("no markers is not an error");
        assert!(warnings.is_empty(), "metadata must not run: {warnings:?}");
        assert!(deps.extensions.instructions.is_empty());
    }

    #[test]
    fn resolve_program_deps_propagates_marker_order_error() {
        let tmp = TempDir::new("program-deps-marker-above");
        ext_fixture(
            &tmp,
            r#"
[package.metadata.spel]
extension_attr = "my_ext"
"#,
            r#"
#[instruction]
pub fn ext_action(account: AccountWithMetadata) -> SpelResult { todo!() }
"#,
        );
        let mod_attrs: Vec<Attribute> = syn::parse_quote!(
            #[my_ext]
            #[lez_program]
        );

        let err = resolve_program_deps(&tmp.path().join("user"), &mod_attrs, &mut |_| {})
            .expect_err("misplaced marker must propagate");
        assert!(
            err.contains("above #[lez_program]"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn read_inject_specs_parses_admin_block() {
        let tmp = TempDir::new("inject-specs");
        tmp.write(
            "Cargo.toml",
            r#"
[package]
name = "x"
version = "0.1.0"
edition = "2021"

[package.metadata.spel]
extension_attr = "my_ext"

[[package.metadata.spel.inject]]
wrapper = "my_gate"

  [[package.metadata.spel.inject.account]]
  name = "gate_config"
  seed = { const = "gate_config" }

  [[package.metadata.spel.inject.account]]
  name = "caller"
  signer = true
"#,
        );
        let value = read_manifest_value(tmp.path()).unwrap();
        let specs = read_spel_inject_specs(&value, tmp.path()).unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].wrapper, "my_gate");
        assert_eq!(specs[0].accounts.len(), 2);
        assert!(matches!(
            &specs[0].accounts[0].seeds[..],
            [InjectSeed::Const(s)] if s == "gate_config"
        ));
        assert!(specs[0].accounts[1].signer);
        assert!(specs[0].accounts[1].seeds.is_empty());
    }

    #[test]
    fn malformed_inject_seed_is_a_hard_error() {
        // A bare-string seed used to be swallowed, injecting the account
        // unconstrained where a PDA-verified one was intended.
        let tmp = TempDir::new("inject-bad-seed");
        tmp.write(
            "Cargo.toml",
            r#"
[package]
name = "x"
version = "0.1.0"
edition = "2021"

[[package.metadata.spel.inject]]
wrapper = "my_gate"

  [[package.metadata.spel.inject.account]]
  name = "gate_config"
  seed = "gate_config"
"#,
        );
        let value = read_manifest_value(tmp.path()).unwrap();
        let err = read_spel_inject_specs(&value, tmp.path())
            .expect_err("bare-string seed must be rejected");
        assert!(err.contains("seed"), "unexpected error: {err}");
    }

    #[test]
    fn malformed_inject_wrapper_is_a_hard_error() {
        let tmp = TempDir::new("inject-bad-wrapper");
        tmp.write(
            "Cargo.toml",
            r#"
[package]
name = "x"
version = "0.1.0"
edition = "2021"

[[package.metadata.spel.inject]]
wrapper = 42
"#,
        );
        let value = read_manifest_value(tmp.path()).unwrap();
        let err = read_spel_inject_specs(&value, tmp.path())
            .expect_err("non-string wrapper must be rejected");
        assert!(err.contains("wrapper"), "unexpected error: {err}");
    }

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
    fn read_inject_specs_parses_compound_seed() {
        let tmp = TempDir::new("inject-compound");
        tmp.write(
            "Cargo.toml",
            r#"
[package]
name = "x"
version = "0.1.0"
edition = "2021"

[[package.metadata.spel.inject]]
wrapper = "other_gate"

  [[package.metadata.spel.inject.account]]
  name = "marker_account"
  seed = [{ const = "marker" }, { account = "caller" }]
"#,
        );
        let value = read_manifest_value(tmp.path()).unwrap();
        let specs = read_spel_inject_specs(&value, tmp.path()).unwrap();
        let seeds = &specs[0].accounts[0].seeds;
        assert_eq!(seeds.len(), 2);
        assert!(matches!(&seeds[0], InjectSeed::Const(s) if s == "marker"));
        assert!(matches!(&seeds[1], InjectSeed::Account(s) if s == "caller"));
    }

    #[test]
    fn malformed_compound_seed_entry_is_a_hard_error() {
        // An entry that is neither const nor account used to be silently
        // skipped, shortening the seed list and deriving a wrong PDA.
        let tmp = TempDir::new("inject-bad-compound");
        tmp.write(
            "Cargo.toml",
            r#"
[package]
name = "x"
version = "0.1.0"
edition = "2021"

[[package.metadata.spel.inject]]
wrapper = "other_gate"

  [[package.metadata.spel.inject.account]]
  name = "marker_account"
  seed = [{ neither = "x" }]
"#,
        );
        let value = read_manifest_value(tmp.path()).unwrap();
        let err = read_spel_inject_specs(&value, tmp.path())
            .expect_err("unknown seed entry must be rejected");
        assert!(err.contains("seed"), "unexpected error: {err}");
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
    fn wrapper_with_args_disables_injection() {
        // Custom target names are manual mode: whoever renames declares.
        let mut func: ItemFn = parse_quote! {
            #[instruction]
            #[my_gate(config = my_cfg, signer = owner)]
            pub fn update_value(new_value: u64) -> SpelResult { todo!() }
        };
        assert!(inject_gate_params(&mut func, &gate_specs())
            .unwrap()
            .is_empty());
        assert_eq!(func.sig.inputs.len(), 1);
    }

    #[test]
    fn discovery_collects_inject_specs() {
        let tmp = TempDir::new("discover-inject");
        let mod_attrs = ext_fixture(
            &tmp,
            r#"
[package.metadata.spel]
extension_attr = "my_ext"

[[package.metadata.spel.inject]]
wrapper = "my_gate"

  [[package.metadata.spel.inject.account]]
  name = "gate_config"
  seed = { const = "gate_config" }
"#,
            r#"
#[instruction]
pub fn ext_action(account: AccountWithMetadata) -> SpelResult { todo!() }
"#,
        );
        let graph = crate::dep_walk::resolve_dep_graph(&tmp.path().join("user"), true, &mut |_| {});
        let ext = discover_extensions(&graph.direct_dirs, &mod_attrs, &mut |_| {})
            .expect("inject block must be collected");
        assert_eq!(ext.inject_specs.len(), 1);
        assert_eq!(ext.inject_specs[0].wrapper, "my_gate");
    }

    #[test]
    fn contributions_follow_marker_order_not_dep_order() {
        // Two extensions, both matched. The module lists ext_b's marker
        // first, so ext_b contributes first, regardless of dep-walk order.
        let tmp = TempDir::new("marker-order");
        for name in ["ext-a", "ext-b"] {
            let ident = name.replace('-', "_");
            tmp.write(
                &format!("{name}/Cargo.toml"),
                &format!(
                    r#"
[package]
name = "{name}"
version = "0.1.0"
edition = "2021"

[package.metadata.spel]
extension_attr = "{ident}"

[[package.metadata.spel.inject]]
wrapper = "{ident}_gate"

  [[package.metadata.spel.inject.account]]
  name = "{ident}_config"
  seed = {{ const = "{ident}_config" }}
"#
                ),
            );
            tmp.write(
                &format!("{name}/src/lib.rs"),
                &format!(
                    r#"
#[instruction]
pub fn {ident}_action(account: AccountWithMetadata) -> SpelResult {{ todo!() }}
"#
                ),
            );
        }
        tmp.write(
            "user/Cargo.toml",
            r#"
[package]
name = "user"
version = "0.1.0"
edition = "2021"

[dependencies]
ext-a = { path = "../ext-a" }
ext-b = { path = "../ext-b" }
"#,
        );
        tmp.write("user/src/lib.rs", "");

        let graph = crate::dep_walk::resolve_dep_graph(&tmp.path().join("user"), true, &mut |_| {});

        let b_first: Vec<Attribute> = syn::parse_quote!(
            #[lez_program]
            #[ext_b]
            #[ext_a]
        );
        let ext = discover_extensions(&graph.direct_dirs, &b_first, &mut |_| {}).unwrap();
        assert_eq!(ext.inject_specs[0].wrapper, "ext_b_gate");
        assert_eq!(ext.inject_specs[1].wrapper, "ext_a_gate");
        assert_eq!(ext.instructions[0].0.sig.ident, "ext_b_action");
        assert_eq!(ext.instructions[1].0.sig.ident, "ext_a_action");

        // Flip the markers: order flips with them.
        let a_first: Vec<Attribute> = syn::parse_quote!(
            #[lez_program]
            #[ext_a]
            #[ext_b]
        );
        let ext = discover_extensions(&graph.direct_dirs, &a_first, &mut |_| {}).unwrap();
        assert_eq!(ext.inject_specs[0].wrapper, "ext_a_gate");
        assert_eq!(ext.inject_specs[1].wrapper, "ext_b_gate");
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

    #[test]
    fn marker_above_lez_program_is_a_hard_error() {
        let tmp = TempDir::new("marker-above-lez");
        ext_fixture(
            &tmp,
            r#"
[package.metadata.spel]
extension_attr = "my_ext"
"#,
            r#"
#[instruction]
pub fn ext_action(account: AccountWithMetadata) -> SpelResult { todo!() }
"#,
        );
        // Same fixture, but the marker sits above #[lez_program], the order
        // the compiled program cannot see.
        let mod_attrs: Vec<Attribute> = syn::parse_quote!(
            #[my_ext]
            #[lez_program]
        );

        let graph = crate::dep_walk::resolve_dep_graph(&tmp.path().join("user"), true, &mut |_| {});
        let err = discover_extensions(&graph.direct_dirs, &mod_attrs, &mut |_| {})
            .expect_err("marker above lez_program must fail discovery");
        assert!(
            err.contains("above #[lez_program]"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn marker_below_lez_program_is_accepted() {
        let tmp = TempDir::new("marker-below-lez");
        // ext_fixture returns #[lez_program] #[my_ext], the correct order.
        let mod_attrs = ext_fixture(
            &tmp,
            r#"
[package.metadata.spel]
extension_attr = "my_ext"
"#,
            r#"
#[instruction]
pub fn ext_action(account: AccountWithMetadata) -> SpelResult { todo!() }
"#,
        );

        let graph = crate::dep_walk::resolve_dep_graph(&tmp.path().join("user"), true, &mut |_| {});
        let ext = discover_extensions(&graph.direct_dirs, &mod_attrs, &mut |_| {})
            .expect("marker below lez_program must be accepted");
        assert_eq!(ext.instructions.len(), 1);
    }

    #[test]
    fn malformed_extension_attr_is_a_hard_error() {
        let tmp = TempDir::new("malformed-ext-attr");
        let mod_attrs = ext_fixture(
            &tmp,
            r#"
[package.metadata.spel]
extension_attr = 42
"#,
            "",
        );
        let err = discover_instructions(&tmp.path().join("user"), &mod_attrs, &mut |_| {})
            .expect_err("wrong-shaped extension_attr must fail, not degrade to no-extension");
        assert!(err.contains("extension_attr"), "unhelpful error: {err}");
    }

    #[test]
    fn unreadable_consumer_manifest_warns() {
        let tmp = TempDir::new("no-consumer-manifest");
        // consumer dir exists but has no Cargo.toml at all
        tmp.write("user/src/lib.rs", "");
        let mod_attrs: Vec<Attribute> = syn::parse_quote!(#[my_ext]);

        let mut warnings = Vec::new();
        let found = discover_instructions(&tmp.path().join("user"), &mod_attrs, &mut |w| {
            warnings.push(w)
        })
        .unwrap();
        assert!(found.is_empty());
        assert!(!warnings.is_empty(), "failure must be loud, got silence");
    }

    #[test]
    fn wrap_only_extension_does_not_warn() {
        // No #[instruction] fns and no inject specs: a library whose whole
        // contribution is the auto-wrap config is a valid extension.
        let tmp = TempDir::new("wrap-only-ext");
        let mod_attrs = ext_fixture(
            &tmp,
            r#"
[package.metadata.spel]
extension_attr = "my_ext"

[package.metadata.spel.wrap_instructions]
wrapper = "my_ext_macros::gate"
self_exempt_marker = "my_exempt"
"#,
            "",
        );
        let mut warnings = Vec::new();
        let found = discover_instructions(&tmp.path().join("user"), &mod_attrs, &mut |w| {
            warnings.push(w)
        })
        .unwrap();
        assert!(found.is_empty());
        assert!(
            warnings.is_empty(),
            "wrap-only extension is legitimate, got: {warnings:?}"
        );
    }

    #[test]
    fn inject_only_extension_does_not_warn() {
        // A library contributing only gate param inject specs is valid too.
        let tmp = TempDir::new("inject-only-ext");
        let mod_attrs = ext_fixture(
            &tmp,
            r#"
[package.metadata.spel]
extension_attr = "my_ext"

[[package.metadata.spel.inject]]
wrapper = "my_gate"

  [[package.metadata.spel.inject.account]]
  name = "gate_config"
  seed = { const = "gate_config" }
"#,
            "",
        );
        let mut warnings = Vec::new();
        let found = discover_instructions(&tmp.path().join("user"), &mod_attrs, &mut |w| {
            warnings.push(w)
        })
        .unwrap();
        assert!(found.is_empty());
        assert!(
            warnings.is_empty(),
            "inject-only extension is legitimate, got: {warnings:?}"
        );
    }

    #[test]
    fn extension_contributing_nothing_warns() {
        let tmp = TempDir::new("nothing-ext");
        let mod_attrs = ext_fixture(
            &tmp,
            r#"
[package.metadata.spel]
extension_attr = "my_ext"
"#,
            "", // no fns AND no gate attrs: almost certainly a broken layout
        );
        let mut warnings = Vec::new();
        let found = discover_instructions(&tmp.path().join("user"), &mod_attrs, &mut |w| {
            warnings.push(w)
        })
        .unwrap();
        assert!(found.is_empty());
        assert!(
            warnings.iter().any(|w| w.contains("my_ext")),
            "matched-but-empty extension must warn, got: {warnings:?}"
        );
    }

    #[test]
    fn discover_extension_instructions_skips_when_attr_absent_on_mod() {
        let tmp = TempDir::new("discover-skip-attr");

        tmp.write(
            "my-ext/Cargo.toml",
            r#"
[package]
name = "my-ext"
version = "0.1.0"
edition = "2021"

[package.metadata.spel]
extension_attr = "my_ext"
"#,
        );
        tmp.write(
            "my-ext/src/lib.rs",
            r#"#[instruction] pub fn ext_action() -> SpelResult { todo!() }"#,
        );
        tmp.write(
            "user/Cargo.toml",
            r#"
[package]
name = "user"
version = "0.1.0"
edition = "2021"

[dependencies]
my-ext = { path = "../my-ext" }
"#,
        );
        tmp.write("user/src/lib.rs", "");

        let mod_attrs: Vec<Attribute> = syn::parse_quote!(#[lez_program]);

        let found =
            discover_instructions(&tmp.path().join("user"), &mod_attrs, &mut |_| {}).unwrap();
        assert!(found.is_empty(), "should skip — no matching attr on mod");
    }

    #[test]
    fn discover_extension_instructions_skips_deps_without_metadata() {
        let tmp = TempDir::new("discover-skip-no-meta");

        tmp.write(
            "lib-no-meta/Cargo.toml",
            r#"
[package]
name = "lib-no-meta"
version = "0.1.0"
edition = "2021"
    "#,
        );
        tmp.write(
            "lib-no-meta/src/lib.rs",
            r#"#[instruction] pub fn whatever() -> SpelResult { todo!() }"#,
        );
        tmp.write(
            "user/Cargo.toml",
            r#"
[package]
name = "user"
version = "0.1.0"
edition = "2021"

[dependencies]
lib-no-meta = { path = "../lib-no-meta" }
"#,
        );
        tmp.write("user/src/lib.rs", "");

        let mod_attrs: Vec<Attribute> = syn::parse_quote!(#[lez_program] #[whatever]);

        let found =
            discover_instructions(&tmp.path().join("user"), &mod_attrs, &mut |_| {}).unwrap();
        assert!(found.is_empty(), "non-extension deps must not be scanned");
    }

    #[test]
    fn duplicate_names_error_names_both_sources() {
        let pairs = vec![
            ("update_value".to_string(), "this module".to_string()),
            (
                "admin_initialize".to_string(),
                "extension my_ext".to_string(),
            ),
            ("update_value".to_string(), "extension my_ext".to_string()),
        ];
        let err =
            check_duplicate_instruction_names(pairs).expect_err("colliding names must be rejected");
        assert!(
            err.contains("update_value"),
            "must name the instruction: {err}"
        );
        assert!(
            err.contains("this module"),
            "must name the first source: {err}"
        );
        assert!(err.contains("my_ext"), "must name the second source: {err}");
    }

    #[test]
    fn unique_names_pass_duplicate_check() {
        let pairs = vec![
            ("update_value".to_string(), "this module".to_string()),
            (
                "admin_initialize".to_string(),
                "extension my_ext".to_string(),
            ),
        ];
        assert!(check_duplicate_instruction_names(pairs).is_ok());
    }

    #[test]
    fn user_fn_colliding_with_extension_fails_idl_generation() {
        let tmp = TempDir::new("dup-user-vs-ext");
        tmp.write(
            "my-ext/Cargo.toml",
            r#"
[package]
name = "my-ext"
version = "0.1.0"
edition = "2021"

[package.metadata.spel]
extension_attr = "my_ext"
"#,
        );
        tmp.write(
            "my-ext/src/lib.rs",
            r#"
#[instruction]
pub fn ext_action(account: AccountWithMetadata) -> SpelResult { todo!() }
"#,
        );
        tmp.write(
            "user/Cargo.toml",
            r#"
[package]
name = "user"
version = "0.1.0"
edition = "2021"

[dependencies]
my-ext = { path = "../my-ext" }
"#,
        );
        // Consumer defines an instruction with the SAME name the
        // extension provides.
        tmp.write(
            "user/src/main.rs",
            r#"
#[lez_program]
#[my_ext]
mod user_program {
    #[instruction]
    pub fn ext_action(account: AccountWithMetadata) -> SpelResult { todo!() }
}
"#,
        );

        let err = crate::idl_gen::generate_idl_from_file_with_deps(
            &tmp.path().join("user/src/main.rs"),
            &[],
        )
        .expect_err("colliding user and extension instruction must fail IDL generation");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("ext_action"),
            "must name the instruction: {msg}"
        );
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
    fn read_wrap_instructions_returns_declared_config() {
        let tmp = TempDir::new("wrap-declared");
        tmp.write(
            "Cargo.toml",
            r#"
[package]
name = "ext-b"
version = "0.1.0"

[package.metadata.spel.wrap_instructions]
wrapper = "ext_b_macros::__inject_gate"
skip = "manual"
self_exempt_marker = "freeze_exempt"
exempt = ["ext_a::action_one", "ext_a::action_two"]
"#,
        );
        let value = read_manifest_value(tmp.path()).unwrap();
        let wrap = read_spel_wrap_instructions(&value, tmp.path())
            .unwrap()
            .expect("wrap config declared");
        assert_eq!(wrap.wrapper, "ext_b_macros::__inject_gate");
        assert_eq!(wrap.skip.as_deref(), Some("manual"));
        assert_eq!(wrap.self_exempt_marker, "freeze_exempt");
        assert_eq!(wrap.exempt, vec!["ext_a::action_one", "ext_a::action_two"]);
    }

    #[test]
    fn read_wrap_instructions_none_when_section_missing() {
        let tmp = TempDir::new("wrap-absent");
        tmp.write(
            "Cargo.toml",
            r#"
[package]
name = "no-wrap"
version = "0.1.0"

[package.metadata.spel]
extension_attr = "some_other"
"#,
        );
        let value = read_manifest_value(tmp.path()).unwrap();
        assert!(read_spel_wrap_instructions(&value, tmp.path())
            .unwrap()
            .is_none());
    }

    #[test]
    fn read_wrap_instructions_handles_omitted_optional_fields() {
        let tmp = TempDir::new("wrap-minimal");
        tmp.write(
            "Cargo.toml",
            r#"
[package]
name = "minimal-wrap"
version = "0.1.0"

[package.metadata.spel.wrap_instructions]
wrapper = "minimal::wrapper"
self_exempt_marker = "minimal_exempt"
"#,
        );
        let value = read_manifest_value(tmp.path()).unwrap();
        let wrap = read_spel_wrap_instructions(&value, tmp.path())
            .unwrap()
            .expect("minimal config");
        assert!(wrap.skip.is_none());
        assert!(wrap.exempt.is_empty());
    }

    #[test]
    fn malformed_wrap_missing_required_field_is_a_hard_error() {
        let tmp = TempDir::new("wrap-incomplete");
        tmp.write(
            "Cargo.toml",
            r#"
[package]
name = "incomplete"
version = "0.1.0"

[package.metadata.spel.wrap_instructions]
wrapper = "incomplete::wrapper"
"#,
        );
        let value = read_manifest_value(tmp.path()).unwrap();
        let err = read_spel_wrap_instructions(&value, tmp.path())
            .expect_err("missing self_exempt_marker must be rejected");
        assert!(
            err.contains("self_exempt_marker"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn omitted_skip_keeps_wrap_active() {
        // No skip word declared means no opt-out: a bare marker must not
        // accidentally match an empty-string default and turn wrap off.
        let tmp = TempDir::new("wrap-no-skip");
        wrap_fixture(
            &tmp,
            r#"
[package.metadata.spel.wrap_instructions]
wrapper = "my_ext_macros::gate"
self_exempt_marker = "my_exempt"
"#,
        );
        let bare: Vec<Attribute> = syn::parse_quote!(
            #[lez_program]
            #[my_ext]
        );
        let graph = crate::dep_walk::resolve_dep_graph(&tmp.path().join("user"), true, &mut |_| {});
        let ext = discover_extensions(&graph.direct_dirs, &bare, &mut |_| {}).unwrap();
        assert_eq!(ext.wraps.len(), 1);
        assert_eq!(ext.wraps[0].0, "");
        assert!(ext.wraps[0].1.skip.is_none());
    }

    #[test]
    fn empty_skip_is_a_hard_error() {
        let tmp = TempDir::new("wrap-empty-skip");
        tmp.write(
            "Cargo.toml",
            r#"
[package]
name = "empty-skip"
version = "0.1.0"

[package.metadata.spel.wrap_instructions]
wrapper = "x::gate"
skip = ""
self_exempt_marker = "x_exempt"
"#,
        );
        let value = read_manifest_value(tmp.path()).unwrap();
        let err = read_spel_wrap_instructions(&value, tmp.path())
            .expect_err("empty skip word must be rejected");
        assert!(err.contains("skip"), "unexpected error: {err}");
    }

    #[test]
    fn discovery_pairs_wrap_with_marker_arg() {
        let tmp = TempDir::new("wrap-discover-arg");
        wrap_fixture(
            &tmp,
            r#"
[package.metadata.spel.wrap_instructions]
wrapper = "my_ext_macros::gate"
skip = "manual"
self_exempt_marker = "my_exempt"
"#,
        );

        // Bare marker: arg is "".
        let bare: Vec<Attribute> = syn::parse_quote!(
            #[lez_program]
            #[my_ext]
        );
        let graph = crate::dep_walk::resolve_dep_graph(&tmp.path().join("user"), true, &mut |_| {});
        let ext = discover_extensions(&graph.direct_dirs, &bare, &mut |_| {}).unwrap();
        assert_eq!(ext.wraps.len(), 1);
        assert_eq!(ext.wraps[0].0, "");

        // Marker with ident arg: arg is the ident (skip matching happens
        // at the producer).
        let manual: Vec<Attribute> = syn::parse_quote!(
            #[lez_program]
            #[my_ext(manual)]
        );
        let ext = discover_extensions(&graph.direct_dirs, &manual, &mut |_| {}).unwrap();
        assert_eq!(ext.wraps[0].0, "manual");
    }

    #[test]
    fn discovery_skips_wrap_when_attr_absent_on_mod() {
        let tmp = TempDir::new("wrap-discover-absent");
        wrap_fixture(
            &tmp,
            r#"
[package.metadata.spel.wrap_instructions]
wrapper = "my_ext_macros::gate"
self_exempt_marker = "my_exempt"
"#,
        );
        let attrs: Vec<Attribute> = syn::parse_quote!(#[lez_program]);
        let graph = crate::dep_walk::resolve_dep_graph(&tmp.path().join("user"), true, &mut |_| {});
        let ext = discover_extensions(&graph.direct_dirs, &attrs, &mut |_| {}).unwrap();
        assert!(ext.wraps.is_empty());
    }
}
