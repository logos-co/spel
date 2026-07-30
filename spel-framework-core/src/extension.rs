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
//!   [`apply_wrap_and_inject`], the wrap configs, and any embedded-mode
//!   declarations from the module markers. Producers apply
//!   [`rewrite_embedded_roles`] before the wrap and inject passes so an
//!   embedded role resolves to the consumer's own account; discovered
//!   fns get their role params substituted and their `bound_args`
//!   trailing params stripped, with the dispatcher filling the values
//!   as literals invisible to the IDL. Gate and
//!   marker attrs stay on emitted handler fns and expand there as
//!   ordinary proc-macros: a gate rewrites the handler body, a marker
//!   expands to nothing. Nothing is stripped.
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
//! `find_path_dep_dirs` precedent — with one exception. When dependency
//! resolution loses the cargo metadata layer while a candidate marker
//! matched no discovered extension, [`resolve_program_deps`] hard-errors
//! instead: a git or registry extension cannot be located in that state,
//! and compiling a program that may be silently missing its extension
//! surface is the one failure this mechanism cannot afford. When every
//! candidate marker matched a path dependency, the degradation stays a
//! warning and the build proceeds.
//!
//! Feature-gated identically to [`crate::idl_gen`]
//! (`#[cfg(feature = "idl-gen")]`) since it depends on `syn` and `toml`.
//! Internal helpers (`read_spel_extension_attr`,
//! `read_spel_inject_specs`, `collect_instruction_fns`) are
//! module-private; producers go through [`resolve_program_deps`],
//! and [`discover_extensions`] stays public for callers that already
//! hold a resolved graph.

use std::path::{Path, PathBuf};

use syn::{Attribute, ItemFn};

use crate::idl_gen::{collect_items_from_crate_dirs, has_instruction_attr};

mod inject;
mod marker;
mod metadata;

pub use inject::{
    active_wraps, apply_wrap_and_inject, resolve_canonical_constraint, rewrite_embedded_roles,
};
pub use marker::{
    candidate_marker_names, has_extension_marker_candidates, parse_marker_args, EmbedDecl,
    MarkerArgs,
};

use metadata::{
    read_manifest_value, read_package_ident, read_spel_bound_args, read_spel_embedded_skip,
    read_spel_extension_attr, read_spel_inject_specs, read_spel_wrap_instructions, BoundArg,
};

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
    /// synthesized by [`apply_wrap_and_inject`].
    pub inject_specs: Vec<InjectSpec>,
    /// Active wrap configs, paired with the consumer marker attr's arg
    /// (`""` for a bare marker) so callers can honor `skip`.
    pub wraps: Vec<(String, WrapInstructions)>,
    /// Embedded-mode declarations from the module markers, paired with
    /// the declaring extension's crate name so a role only ever
    /// rewrites its own extensions' inject entries.
    pub embeds: Vec<(String, EmbedDecl)>,
    /// Dispatch-only trailing args per discovered fn, resolved from
    /// `bound_args` metadata and the marker's kwargs. The dispatcher
    /// appends these literals at the call site; the params were
    /// stripped at discovery so no IDL or validation path sees them.
    pub bound_calls: std::collections::HashMap<String, Vec<usize>>,
    /// Marker names that matched a discovered extension, in marker
    /// order. Lets producers tell an unmatched candidate attr from a
    /// matched one when dependency resolution degrades.
    pub matched_markers: Vec<String>,
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
#[derive(Clone, Debug, PartialEq)]
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
    /// Inject-spec role name, the wrapper kwarg key. Equal to `name`
    /// unless an embedded rewrite retargeted the param.
    pub role: String,
    /// Ordered PDA seed components. Empty = plain account (no PDA),
    /// one = single-seed PDA, multiple = compound PDA.
    pub seeds: Vec<InjectSeed>,
    /// Whether the param carries `#[account(signer)]`.
    pub signer: bool,
    /// Set by the embedded rewrite: this entry was retargeted to the
    /// consumer's embedding account. Substitution keys on this, never
    /// on a name/role comparison, so an embedding account may share
    /// the role's name.
    pub embedded: bool,
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
    embeds: Vec<(String, EmbedDecl)>,
    bound_calls: std::collections::HashMap<String, Vec<usize>>,
    marker: String,
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
    if with_metadata {
        if let Some(reason) = &graph.metadata_failure {
            let unmatched: Vec<String> = candidate_marker_names(mod_attrs)
                .into_iter()
                .filter(|c| !extensions.matched_markers.contains(c))
                .collect();
            if !unmatched.is_empty() {
                return Err(format!(
                    "marker(s) {unmatched:?} matched no discoverable extension and \
                    dependency resolution failed: {reason}. A git or registry \
                    extension cannot be located in this state, refusing to compile \
                    a program that could be silently missing its extension surface."
                ));
            }
            on_warning(format!(
                "dependency resolution degraded ({reason}); every marker matched a \
                path dependency, continuing"
            ));
        }
    }
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
        let embedded_skip = read_spel_embedded_skip(&manifest_value, dep_dir)?;
        let has_wrap = wrap.is_some();
        let marker_args = mod_attrs
            .iter()
            .find_map(|a| parse_marker_args(a, &ext_attr).transpose())
            .transpose()?
            .unwrap_or_default();

        let mut wraps = Vec::new();
        if let Some(w) = wrap {
            wraps.push((marker_args.word.clone().unwrap_or_default(), w));
        }
        let is_embedded = marker_args.embed.is_some();
        let embed_offset = marker_args.embed.as_ref().map(|e| e.offset);
        let mut embeds = Vec::new();
        if let Some(embed) = marker_args.embed {
            embeds.push((crate_name.clone(), embed));
        }

        let crate_ident = syn::Ident::new(&crate_name, proc_macro2::Span::call_site());
        let crate_path: syn::Path = syn::parse_quote!(::#crate_ident);

        let (items, _) = collect_items_from_crate_dirs(std::slice::from_ref(dep_dir));
        let funcs = collect_instruction_fns(&items);
        let funcs: Vec<ItemFn> = if is_embedded {
            funcs
                .into_iter()
                .filter(|f| !embedded_skip.iter().any(|s| f.sig.ident == *s))
                .collect()
        } else {
            funcs
        };
        let bound_args = read_spel_bound_args(&manifest_value, dep_dir)?;
        for bound in &bound_args {
            let kwarg = bound
                .from
                .split_once("::")
                .map_or(bound.from.as_str(), |(_, k)| k);
            if kwarg != "offset" {
                return Err(format!(
                    "extension '{crate_name}': bound_args.from = \"{}\" names kwarg \
                    \"{kwarg}\", which is not a marker kwarg the framework knows; \
                    only \"offset\" carries a value",
                    bound.from
                ));
            }
        }
        let mut bound_calls = std::collections::HashMap::new();
        let mut stripped: Vec<ItemFn> = Vec::with_capacity(funcs.len());
        for mut f in funcs {
            let mut values = Vec::new();
            for bound in &bound_args {
                let Some(pos) = f.sig.inputs.iter().position(|input| {
                    matches!(input, syn::FnArg::Typed(pt)
                        if matches!(&*pt.pat, syn::Pat::Ident(pi) if pi.ident == bound.arg))
                }) else {
                    continue;
                };
                f.sig.inputs = f
                    .sig
                    .inputs
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != pos)
                    .map(|(_, input)| input.clone())
                    .collect();
                values.push(resolve_bound_value(
                    bound,
                    embed_offset,
                    mod_attrs,
                    &crate_name,
                )?);
            }
            if !values.is_empty() {
                bound_calls.insert(f.sig.ident.to_string(), values);
            }
            stripped.push(f);
        }
        let funcs = stripped;
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
            embeds,
            bound_calls,
            marker: ext_attr.clone(),
        });
    }

    Ok(flatten_in_marker_order(matched))
}

/// Resolve one bound arg to its compile-time value.
///
/// Self shape (`from = "offset"`) reads the extension's own marker's
/// offset kwarg. Cross shape (`from = "<marker>::offset"`) reads the
/// named peer marker's offset from the same module, so an extension
/// can depend on where a peer embedded its state (freeze ADR-0012:
/// freeze binding `admin_offset` from `admin_authority::offset`).
/// A missing marker or missing kwarg falls back to `default`; a bound
/// without a default makes both hard errors at the consumer's build.
fn resolve_bound_value(
    bound: &BoundArg,
    self_offset: Option<usize>,
    mod_attrs: &[Attribute],
    crate_name: &str,
) -> Result<usize, String> {
    let marker_offset = match bound.from.split_once("::") {
        None => self_offset,
        Some((marker, _)) => {
            let Some(args) = mod_attrs
                .iter()
                .find_map(|a| parse_marker_args(a, marker).transpose())
                .transpose()?
            else {
                return bound.default.ok_or_else(|| {
                    format!(
                        "extension '{crate_name}': bound_arg '{}' requires marker \
                        '#[{marker}]', which is not declared on this module, and \
                        declares no default",
                        bound.arg
                    )
                });
            };
            args.embed.map(|e| e.offset)
        },
    };
    marker_offset.or(bound.default).ok_or_else(|| {
        format!(
            "extension '{crate_name}': bound_arg '{}' reads '{}' but the marker \
            carries no offset kwarg and the bound_arg declares no default",
            bound.arg, bound.from
        )
    })
}

/// Read a crate's `[[package.metadata.spe.inject]]` blocks from its
/// manifest on disk. Public for the extension author's alignment
/// self-test (freeze ADR-0010): a unit test inside the extension crate
/// reads its own declared inject-account names and asserts they match
/// the kwarg set its wrapper macro accepts, so metadata and macro
/// cannot drift apart silently.
/// 
/// # Errors
/// 
/// `Err` on an unreadable manifest or malformed inject metadata.
pub fn read_inject_specs(crate_dir: &Path) -> Result<Vec<InjectSpec>, String> {
    let Some(manifest_value) = read_manifest_value(crate_dir) else {
        return Err(format!("unreadable Cargo.toml under {}", crate_dir.display()));
    };
    read_spel_inject_specs(&manifest_value, crate_dir)
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
        out.embeds.extend(m.embeds);
        out.bound_calls.extend(m.bound_calls);
        out.matched_markers.push(m.marker);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::TempDir;

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
    fn unknown_bound_from_is_a_hard_error() {
        let tmp = TempDir::new("bound-from-unknown");
        let mod_attrs = ext_fixture(
            &tmp,
            r#"
[package.metadata.spel]
extension_attr = "my_ext"

[[package.metadata.spel.bound_args]]
arg = "offset"
from = "grace"
"#,
            r#"
#[instruction]
pub fn ext_action(account: AccountWithMetadata, offset: usize) -> SpelResult { todo!() }
"#,
        );
        let graph = crate::dep_walk::resolve_dep_graph(&tmp.path().join("user"), true, &mut |_| {});
        let err = discover_extensions(&graph.direct_dirs, &mod_attrs, &mut |_| {})
            .expect_err("an unknown bound_args.from must be rejected");
        assert!(
            err.contains("only \"offset\" carries a value"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn bound_param_stripped_and_value_resolved() {
        let metadata = r#"
[package.metadata.spel]
extension_attr = "my_ext"

[[package.metadata.spel.inject]]
wrapper = "my_gate"

  [[package.metadata.spel.inject.account]]
  name = "gate_config"
  seed = { const = "gate_config" }

[[package.metadata.spel.bound_args]]
arg = "offset"
from = "offset"
default = 0
"#;
        let lib_rs = r#"
#[instruction]
pub fn ext_action(account: AccountWithMetadata, offset: usize) -> SpelResult { todo!() }
"#;

        // Embedded marker: the value is the marker's offset.
        let tmp = TempDir::new("bound-strip-embedded");
        ext_fixture(&tmp, metadata, lib_rs);
        let mod_attrs: Vec<Attribute> = syn::parse_quote!(
            #[lez_program]
            #[my_ext(gate_config = prog_config, offset = 32)]
        );
        let graph = crate::dep_walk::resolve_dep_graph(&tmp.path().join("user"), true, &mut |_| {});
        let ext = discover_extensions(&graph.direct_dirs, &mod_attrs, &mut |_| {})
            .expect("embedded discovery must succeed");
        let (func, _) = &ext.instructions[0];
        let param_names: Vec<String> = func
            .sig
            .inputs
            .iter()
            .filter_map(|i| match i {
                syn::FnArg::Typed(pt) => match &*pt.pat {
                    syn::Pat::Ident(pi) => Some(pi.ident.to_string()),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        assert_eq!(
            param_names,
            vec!["account".to_string()],
            "offset must be stripped"
        );
        assert_eq!(ext.bound_calls.get("ext_action"), Some(&vec![32]));

        // Bare marker: dedicated mode resolves the default.
        let tmp = TempDir::new("bound-strip-dedicated");
        let mod_attrs = ext_fixture(&tmp, metadata, lib_rs);
        let graph = crate::dep_walk::resolve_dep_graph(&tmp.path().join("user"), true, &mut |_| {});
        let ext = discover_extensions(&graph.direct_dirs, &mod_attrs, &mut |_| {})
            .expect("dedicated discovery must succeed");
        assert_eq!(ext.bound_calls.get("ext_action"), Some(&vec![0]));
    }

    #[test]
    fn cross_marker_bound_resolves_peer_offset() {
        let metadata = r#"
[package.metadata.spel]
extension_attr = "my_ext"

[[package.metadata.spel.bound_args]]
arg = "admin_offset"
from = "peer_ext::offset"
default = 0
"#;
        let lib_rs = r#"
#[instruction]
pub fn ext_action(account: AccountWithMetadata, admin_offset: usize) -> SpelResult { todo!() }
"#;

        // Peer marker embedded: the value is the peer's offset kwarg.
        let tmp = TempDir::new("bound-cross-embedded");
        ext_fixture(&tmp, metadata, lib_rs);
        let mod_attrs: Vec<Attribute> = syn::parse_quote!(
            #[lez_program]
            #[my_ext]
            #[peer_ext(peer_config = prog_config, offset = 16)]
        );
        let graph = crate::dep_walk::resolve_dep_graph(&tmp.path().join("user"), true, &mut |_| {});
        let ext = discover_extensions(&graph.direct_dirs, &mod_attrs, &mut |_| {})
            .expect("cross-marker discovery must succeed");
        assert_eq!(ext.bound_calls.get("ext_action"), Some(&vec![16]));

        // Peer marker absent: the declared default applies.
        let tmp = TempDir::new("bound-cross-dedicated");
        let mod_attrs = ext_fixture(&tmp, metadata, lib_rs);
        let graph = crate::dep_walk::resolve_dep_graph(&tmp.path().join("user"), true, &mut |_| {});
        let ext = discover_extensions(&graph.direct_dirs, &mod_attrs, &mut |_| {})
            .expect("absent peer with default must succeed");
        assert_eq!(ext.bound_calls.get("ext_action"), Some(&vec![0]));
    }

    #[test]
    fn cross_marker_bound_without_default_requires_the_peer_marker() {
        let tmp = TempDir::new("bound-cross-no-default");
        let mod_attrs = ext_fixture(
            &tmp,
            r#"
[package.metadata.spel]
extension_attr = "my_ext"

[[package.metadata.spel.bound_args]]
arg = "admin_offset"
from = "peer_ext::offset"
"#,
            r#"
#[instruction]
pub fn ext_action(account: AccountWithMetadata, admin_offset: usize) -> SpelResult { todo!() }
"#,
        );
        let graph = crate::dep_walk::resolve_dep_graph(&tmp.path().join("user"), true, &mut |_| {});
        let err = discover_extensions(&graph.direct_dirs, &mod_attrs, &mut |_| {})
            .expect_err("absent peer without default must be rejected");
        assert!(
            err.contains("requires marker '#[peer_ext]'"),
            "unexpected error: {err}"
        );
    }

    // The fail-open closure: a metadata failure with a marker that
    // matched nothing must refuse to compile, never silently drop a
    // git or registry extension. The invalid version string makes
    // cargo metadata fail deterministically and offline.
    #[test]
    fn metadata_failure_with_unmatched_marker_is_a_hard_error() {
        let tmp = TempDir::new("fail-open-unmatched");
        tmp.write(
            "user/Cargo.toml",
            r#"
[package]
name = "user"
version = "not-a-version"
edition = "2021"
"#,
        );
        tmp.write("user/src/lib.rs", "");
        let mod_attrs: Vec<Attribute> = syn::parse_quote!(
            #[lez_program]
            #[ghost_ext]
        );
        let err = resolve_program_deps(&tmp.path().join("user"), &mod_attrs, &mut |_| {})
            .expect_err("unmatched marker with failed metadata must refuse to compile");
        assert!(
            err.contains("refusing to compile"),
            "unexpected error: {err}"
        );
        assert!(err.contains("ghost_ext"), "must name the marker: {err}");
    }

    // The counterpart: when every marker matched a path dependency,
    // the same metadata failure stays a warning and the build proceeds.
    #[test]
    fn metadata_failure_with_matched_path_marker_degrades_to_warning() {
        let tmp = TempDir::new("fail-open-matched");
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
version = "not-a-version"
edition = "2021"

[dependencies]
my-ext = { path = "../my-ext" }
"#,
        );
        tmp.write("user/src/lib.rs", "");
        let mod_attrs: Vec<Attribute> = syn::parse_quote!(
            #[lez_program]
            #[my_ext]
        );
        let mut warnings = Vec::new();
        let deps = resolve_program_deps(&tmp.path().join("user"), &mod_attrs, &mut |w| {
            warnings.push(w)
        })
        .expect("matched path marker must compile through a degraded resolution");
        assert_eq!(deps.extensions.matched_markers, vec!["my_ext".to_string()]);
        assert_eq!(deps.extensions.instructions.len(), 1);
        assert!(
            warnings.iter().any(|w| w.contains("degraded")),
            "must warn about the degradation: {warnings:?}"
        );
    }

    #[test]
    fn embedded_mode_skips_declared_initializer() {
        let tmp = TempDir::new("embed-skip-init");
        ext_fixture(
            &tmp,
            r#"
[package.metadata.spel]
extension_attr = "my_ext"

[[package.metadata.spel.inject]]
wrapper = "my_gate"

  [[package.metadata.spel.inject.account]]
  name = "gate_config"
  seed = { const = "gate_config" }

[package.metadata.spel.embedded]
skip = ["ext_init"]
"#,
            r#"
#[instruction]
pub fn ext_init(account: AccountWithMetadata) -> SpelResult { todo!() }

#[instruction]
pub fn ext_action(account: AccountWithMetadata) -> SpelResult { todo!() }
"#,
        );
        let mod_attrs: Vec<Attribute> = syn::parse_quote!(
            #[lez_program]
            #[my_ext(gate_config = prog_config, offset = 32)]
        );
        let graph = crate::dep_walk::resolve_dep_graph(&tmp.path().join("user"), true, &mut |_| {});
        let ext = discover_extensions(&graph.direct_dirs, &mod_attrs, &mut |_| {})
            .expect("embedded discovery must succeed");
        let names: Vec<String> = ext
            .instructions
            .iter()
            .map(|(f, _)| f.sig.ident.to_string())
            .collect();
        assert_eq!(names, vec!["ext_action".to_string()]);
    }

    #[test]
    fn dedicated_mode_keeps_skipped_initializer() {
        let tmp = TempDir::new("dedicated-keeps-init");
        let mod_attrs = ext_fixture(
            &tmp,
            r#"
[package.metadata.spel]
extension_attr = "my_ext"

[package.metadata.spel.embedded]
skip = ["ext_init"]
"#,
            r#"
#[instruction]
pub fn ext_init(account: AccountWithMetadata) -> SpelResult { todo!() }

#[instruction]
pub fn ext_action(account: AccountWithMetadata) -> SpelResult { todo!() }
"#,
        );
        let graph = crate::dep_walk::resolve_dep_graph(&tmp.path().join("user"), true, &mut |_| {});
        let ext = discover_extensions(&graph.direct_dirs, &mod_attrs, &mut |_| {})
            .expect("dedicated discovery must succeed");
        let names: Vec<String> = ext
            .instructions
            .iter()
            .map(|(f, _)| f.sig.ident.to_string())
            .collect();
        assert_eq!(
            names,
            vec!["ext_init".to_string(), "ext_action".to_string()]
        );
    }

    #[test]
    fn discovery_collects_embed_decl() {
        let tmp = TempDir::new("discover-embed");
        ext_fixture(
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
        let mod_attrs: Vec<Attribute> = syn::parse_quote!(
            #[lez_program]
            #[my_ext(gate_config = prog_config, offset = 32)]
        );
        let graph = crate::dep_walk::resolve_dep_graph(&tmp.path().join("user"), true, &mut |_| {});
        let ext = discover_extensions(&graph.direct_dirs, &mod_attrs, &mut |_| {})
            .expect("embedded marker must be collected");
        assert_eq!(
            ext.embeds,
            vec![(
                "my_ext".to_string(),
                EmbedDecl {
                    role: "gate_config".to_string(),
                    account: "prog_config".to_string(),
                    offset: 32,
                }
            )]
        );
    }

    #[test]
    fn embedded_role_rewrites_inject_entry_from_canonical_declaration() {
        let tmp = TempDir::new("embed-rewrite");
        ext_fixture(
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
        let mod_attrs: Vec<Attribute> = syn::parse_quote!(
            #[lez_program]
            #[my_ext(gate_config = prog_config, offset = 32)]
        );
        let consumer_fns: Vec<ItemFn> = vec![syn::parse_quote!(
            pub fn initialize(
                #[account(init, pda = literal("prog_config"))] mut prog_config: AccountWithMetadata,
            ) -> SpelResult {
                todo!()
            }
        )];
        let graph = crate::dep_walk::resolve_dep_graph(&tmp.path().join("user"), true, &mut |_| {});
        let mut ext = discover_extensions(&graph.direct_dirs, &mod_attrs, &mut |_| {})
            .expect("embedded marker must be collected");
        rewrite_embedded_roles(&mut ext.inject_specs, &ext.embeds, &consumer_fns)
            .expect("rewrite must succeed");
        let acc = &ext.inject_specs[0].accounts[0];
        assert_eq!(acc.name, "prog_config");
        assert_eq!(
            acc.seeds,
            vec![InjectSeed::Const("prog_config".to_string())]
        );
        assert!(!acc.signer);
    }

    #[test]
    fn embedded_role_unknown_in_spec_is_error() {
        let mut specs = vec![InjectSpec {
            wrapper: "my_gate".to_string(),
            accounts: vec![InjectAccount {
                name: "gate_config".to_string(),
                role: "gate_config".to_string(),
                seeds: vec![InjectSeed::Const("gate_config".to_string())],
                signer: false,
                embedded: false,
            }],
            source: "my_ext".to_string(),
        }];
        let embeds = vec![(
            "my_ext".to_string(),
            EmbedDecl {
                role: "nonexistent".to_string(),
                account: "prog_config".to_string(),
                offset: 8,
            },
        )];
        let consumer_fns: Vec<ItemFn> = vec![syn::parse_quote!(
            pub fn initialize(
                #[account(init, pda = literal("prog_config"))] mut prog_config: AccountWithMetadata,
            ) -> SpelResult {
                todo!()
            }
        )];
        let err = rewrite_embedded_roles(&mut specs, &embeds, &consumer_fns).unwrap_err();
        assert!(
            err.contains("my_ext") && err.contains("nonexistent"),
            "got: {err}"
        );
    }

    #[test]
    fn rewritten_role_keeps_kwarg_key_and_injects_consumer_name() {
        let mut specs = vec![InjectSpec {
            wrapper: "my_gate".to_string(),
            accounts: vec![InjectAccount {
                name: "gate_config".to_string(),
                role: "gate_config".to_string(),
                seeds: vec![InjectSeed::Const("gate_config".to_string())],
                signer: false,
                embedded: false,
            }],
            source: "my_ext".to_string(),
        }];
        let embeds = vec![(
            "my_ext".to_string(),
            EmbedDecl {
                role: "gate_config".to_string(),
                account: "prog_config".to_string(),
                offset: 32,
            },
        )];
        let consumer_fns: Vec<ItemFn> = vec![syn::parse_quote!(
            pub fn initialize(
                #[account(init, pda = literal("prog_config"))] mut prog_config: AccountWithMetadata,
            ) -> SpelResult {
                todo!()
            }
        )];
        rewrite_embedded_roles(&mut specs, &embeds, &consumer_fns).unwrap();
        let acc = &specs[0].accounts[0];
        assert_eq!(
            acc.name, "prog_config",
            "param name takes the consumer account"
        );
        assert_eq!(acc.role, "gate_config", "kwarg key keeps the role");

        // A gated fn that does not declare the embedding account gets it
        // injected under the consumer's name, PDA-verified.
        let mut func: ItemFn = syn::parse_quote!(
            #[my_gate]
            pub fn emergency(value: u64) -> SpelResult {
                todo!()
            }
        );
        let injected = apply_wrap_and_inject(&mut func, &[], &specs, &embeds, None).unwrap();
        assert_eq!(injected, vec!["prog_config".to_string()]);
        let expected: Attribute =
            syn::parse_quote!(#[my_gate(gate_config = prog_config, offset = 32)]);
        assert_eq!(func.attrs.first(), Some(&expected));
        let names: Vec<String> = func
            .sig
            .inputs
            .iter()
            .filter_map(|i| match i {
                syn::FnArg::Typed(pt) => match &*pt.pat {
                    syn::Pat::Ident(pi) => Some(pi.ident.to_string()),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        assert_eq!(names, vec!["prog_config".to_string(), "value".to_string()]);
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
