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
//!   [`apply_wrap_and_inject`], and the wrap configs. Gate and marker
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

use syn::{Attribute, ItemFn};

use crate::idl_gen::{collect_items_from_crate_dirs, has_instruction_attr};

mod inject;
mod marker;
mod metadata;

pub use inject::{active_wraps, apply_wrap_and_inject};
pub use marker::has_extension_marker_candidates;

use marker::extract_attr_arg;
use metadata::{
    read_manifest_value, read_package_ident, read_spel_extension_attr, read_spel_inject_specs,
    read_spel_wrap_instructions,
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
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::TempDir;
    use syn::Pat;
}
