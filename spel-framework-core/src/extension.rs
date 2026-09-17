//! Extension discovery for SPEL programs.
//!
//! Scans the consuming program's **direct** dependencies (path, git, or
//! registry, resolved by [`crate::dep_walk`]) for crates that declare
//! `[package.metadata.spel]` in their `Cargo.toml`. Each qualifying
//! crate contributes through one entry point:
//!
//! - `discover_extensions` returns an [`ExtensionDiscoveries`]: the
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
//! `find_path_dep_dirs` precedent — with one exception. A candidate
//! marker that matched no discovered extension makes
//! [`resolve_program_deps`] hard-error regardless of why: a typo or a
//! transitive-only dependency under healthy resolution, or a git or
//! registry extension that cannot be located once dependency resolution
//! loses the cargo metadata layer. Compiling a program that may be
//! silently missing its extension surface is the one failure this
//! mechanism cannot afford. When every candidate marker matched a path
//! dependency, resolution degradation stays a warning and the build
//! proceeds.
//!
//! Feature-gated identically to [`crate::idl_gen`]
//! (`#[cfg(feature = "idl-gen")]`) since it depends on `syn` and `toml`.
//! Internal helpers (`read_spel_extension_attr`,
//! `read_spel_inject_specs`, `collect_instruction_fns`,
//! `discover_extensions`) are module-private; producers go through
//! [`resolve_program_deps`].

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use syn::{Attribute, ItemFn};

use crate::account_types::has_account_type_attr;
use crate::idl_gen::{collect_items_from_crate_dirs, has_instruction_attr};

mod inject;
mod marker;
mod metadata;
mod slots;

pub use inject::{
    active_wraps, apply_wrap_and_inject, rewrite_embedded_roles, ActiveWrap, GateLocations,
};
pub use marker::{
    candidate_marker_names, has_extension_marker_candidates, parse_marker_args, BoundValue,
    EmbedDecl, MarkerArgs, OffsetSpec, INITIALIZE_SHORTHAND,
};
pub use slots::{find_slot_carrier, resolve_derived_offsets, slot_offset_const_name, SlotCarrier};

use metadata::{
    read_manifest_value, read_package_ident, read_spel_bound_args, read_spel_embedded,
    read_spel_extension_attr, read_spel_inject_specs, read_spel_wrap_instructions, BoundArg,
    EmbeddedMeta,
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
    /// Embedded-mode declarations from the module markers, each naming
    /// the declaring extension so a role only ever rewrites its own
    /// extensions' inject entries.
    pub embeds: Vec<Embed>,
    /// Dispatch-only trailing args per discovered fn, resolved from
    /// `bound_args` metadata and the marker's kwargs. The dispatcher
    /// appends each value at the call site as a literal or, for a
    /// derived offset, as the carrier's const path; the params were
    /// stripped at discovery so no IDL or validation path sees them.
    pub bound_calls: HashMap<String, Vec<BoundValue>>,
    /// Marker names that matched a discovered extension, in marker
    /// order. Lets producers tell an unmatched candidate attr from a
    /// matched one when dependency resolution degrades.
    pub matched_markers: Vec<String>,
    /// Crate dir of each activated extension, in marker order. Their
    /// instructions ship in the consumer's binary, so the accounts those
    /// instructions write are the program's own.
    pub activated_dirs: Vec<PathBuf>,
    pub dormant_anchors: Vec<DormantAnchor>,
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

/// Whether a producer resolves derived offsets, and against what.
///
/// The two halves of that question always had one answer and were
/// asked separately: a producer that resolves carriers is the one that
/// writes location kwargs, and a producer that does not resolve must
/// not write them. Passing this to [`ProgramDeps::prepare`] settles
/// both at once.
pub enum Carriers<'a> {
    /// Resolve every derivation against these items, then write
    /// locations onto the gate attrs. The dispatcher's answer.
    Resolve(&'a [syn::Item]),
    /// Leave derivations unresolved and write no locations. The IDL
    /// producers' answer: the IDL has no offset field, so a resolved
    /// one could not reach their output.
    Skip,
}

/// An extension-provided instruction fn, ready for the gate pass.
pub struct PreparedInstruction {
    pub func: ItemFn,
    /// Absolute path to the declaring crate, e.g. `::admin_authority`.
    pub crate_path: syn::Path,
    /// `crate::fn_name`, the form a wrap's `exempt` list matches.
    pub qualified: String,
}

/// The dependency side of a program, in the state the gate pass wants.
///
/// [`ProgramDeps::prepare`] is the only way to build one, so the passes
/// that must precede the gate pass cannot be run out of order, skipped,
/// or applied with mismatched arguments: [`PreparedProgram::gate`]
/// supplies the specs, embeds, wraps, and location mode together.
#[derive(Default)]
pub struct PreparedProgram {
    /// The dependency graph, for callers that scan dependency sources.
    pub graph: crate::dep_walk::DepGraph,
    /// Gate param inject specs, embedded roles already rewritten.
    pub inject_specs: Vec<InjectSpec>,
    /// Embedded-mode declarations, offsets resolved under
    /// [`Carriers::Resolve`].
    pub embeds: Vec<Embed>,
    /// Wrap configs the consumer's marker args did not skip.
    pub active_wraps: Vec<ActiveWrap>,
    /// Dispatch-only trailing args per discovered fn.
    pub bound_calls: HashMap<String, Vec<BoundValue>>,
    /// Instruction fns the extensions contribute.
    pub instructions: Vec<PreparedInstruction>,
    /// Whether the gate pass writes location kwargs, decided with the
    /// carriers rather than separately at each call site.
    pub locations: GateLocations,
    /// Crates whose `#[account_type]` declares one of this program's
    /// account layouts: owned sources plus activated extensions, deduped
    /// by canonical path. Captured at prepare time, when both inputs
    /// still exist.
    pub connected_dirs: Vec<PathBuf>,
}

impl PreparedProgram {
    /// Run the gate pass over one instruction fn.
    ///
    /// `qualified = None` for a consumer-authored fn, `Some` for an
    /// extension-provided one (see [`apply_wrap_and_inject`]).
    ///
    /// # Errors
    ///
    /// Propagates [`apply_wrap_and_inject`].
    pub fn gate(&self, func: &mut ItemFn, qualified: Option<&str>) -> Result<Vec<String>, String> {
        marker::replace_initialize_shorthand(func, &self.embeds)?;
        apply_wrap_and_inject(
            func,
            &self.active_wraps,
            &self.inject_specs,
            self.locations,
            qualified,
        )
    }

    /// The item set whose `#[account_type]` declares this program's
    /// account layouts: the consumer's own items, then each connected
    /// crate's, checked for colliding names while the declaring crate
    /// is still known.
    ///
    /// # Errors
    ///
    /// `Err` when two connected crates declare an account layout of the
    /// same name, naming the type and both paths. Two layouts of one
    /// name make the IDL ambiguous, and both declarations are code the
    /// author owns or activated, so it is theirs to resolve. Callers
    /// surface it as a compile error.
    pub fn layout_items(
        &self,
        consumer_source: &Path,
        mut consumer_items: Vec<syn::Item>,
    ) -> Result<(Vec<syn::Item>, Vec<PathBuf>), String> {
        // The connected walk screens cfg-excluded items; the consumer's
        // items arrive raw from the macro input, so screen them here. A
        // default build never compiles them, so they are not the
        // program's layouts.
        consumer_items.retain(|i| !crate::idl_gen::cfg_excluded_item(i));
        let (mut connected, files_read) = self.connected_groups();
        self.demote_embedded_state(&mut connected);

        let mut groups = vec![(consumer_source.to_path_buf(), consumer_items)];
        groups.extend(connected);
        check_layout_collisions(&groups)?;

        // Consumer first: its declarations shadow same-named dependency
        // items in the flattened set.
        Ok((
            groups.into_iter().flat_map(|(_, items)| items).collect(),
            files_read,
        ))
    }

    /// Each connected crate's items under its dir, plus every file
    /// read, for callers that register cargo dependencies.
    fn connected_groups(&self) -> (Vec<(PathBuf, Vec<syn::Item>)>, Vec<PathBuf>) {
        let mut groups = Vec::new();
        let mut files_read = Vec::new();
        for dir in &self.connected_dirs {
            let (items, files) = collect_items_from_crate_dirs(std::slice::from_ref(dir), |w| {
                eprintln!("warning: {w}")
            });
            files_read.extend(files);
            groups.push((dir.clone(), items));
        }
        (groups, files_read)
    }

    /// An embedded extension's state type is a window inside the
    /// consumer's account, not an account of its own: strip its
    /// annotation so it reaches the IDL as a referenced type. The
    /// consumer's items are not in `groups`, so a same-named consumer
    /// struct, which is a different type, is untouched. Dedicated mode
    /// has no embed entry and nothing to strip.
    fn demote_embedded_state(&self, groups: &mut [(PathBuf, Vec<syn::Item>)]) {
        let embedded_state: HashSet<&str> = self
            .embeds
            .iter()
            .filter_map(|e| e.state_type.rsplit("::").next())
            .collect();
        for (_, items) in groups.iter_mut() {
            for item in items.iter_mut() {
                let (attrs, name) = match item {
                    syn::Item::Struct(s) => (&mut s.attrs, s.ident.to_string()),
                    syn::Item::Enum(e) => (&mut e.attrs, e.ident.to_string()),
                    _ => continue,
                };
                if embedded_state.contains(name.as_str()) {
                    attrs.retain(|a| !has_account_type_attr(std::slice::from_ref(a)));
                }
            }
        }
    }

    /// The rest of the dependency graph, as the source referenced types
    /// are resolved from on demand.
    pub fn unowned_defs(&self) -> crate::idl_gen::UnownedTypeDefs {
        use crate::dep_walk::canonical_key;
        let connected: HashSet<PathBuf> = self
            .connected_dirs
            .iter()
            .map(|d| canonical_key(d))
            .collect();
        crate::idl_gen::UnownedTypeDefs::new(
            self.graph
                .transitive_dirs
                .iter()
                .filter(|d| !connected.contains(&canonical_key(d)))
                .cloned()
                .collect(),
        )
    }
}

impl ProgramDeps {
    /// Run every pass the gate pass depends on, in the one order that
    /// works: resolve derivations, rewrite embedded roles against the
    /// consumer's own instructions, then filter the wraps the marker
    /// skipped.
    ///
    /// # Errors
    ///
    /// `Err` when a derivation has no slot carrier, when an embedded
    /// role matches no inject account, when the embedding account has
    /// no canonical declaration, or when two embeds collide. Callers
    /// surface it as a compile error.
    pub fn prepare(
        mut self,
        mod_items: &[syn::Item],
        carriers: Carriers<'_>,
    ) -> Result<PreparedProgram, String> {
        let connected_dirs = self.connected_dirs();
        let locations = match carriers {
            Carriers::Resolve(items) => {
                resolve_derived_offsets(&mut self.extensions, items)?;
                check_dormant_anchors(&self.extensions.dormant_anchors, items)?;
                GateLocations::Emit
            },
            Carriers::Skip => GateLocations::Omit,
        };

        let consumer_fns = collect_instruction_fns(mod_items);
        rewrite_embedded_roles(
            &mut self.extensions.inject_specs,
            &self.extensions.embeds,
            &consumer_fns,
        )?;

        let instructions = self
            .extensions
            .instructions
            .into_iter()
            .map(|(func, crate_path)| PreparedInstruction {
                qualified: qualified_instruction_name(&crate_path, &func.sig.ident),
                func,
                crate_path,
            })
            .collect();

        Ok(PreparedProgram {
            graph: self.graph,
            active_wraps: active_wraps(&self.extensions.wraps)?,
            inject_specs: self.extensions.inject_specs,
            embeds: self.extensions.embeds,
            bound_calls: self.extensions.bound_calls,
            instructions,
            locations,
            connected_dirs,
        })
    }
}

/// Two layouts of one name make the IDL ambiguous. Both declarations
/// sit in code the author owns or activated, so the error names the
/// type and both crates and leaves the rename to them.
fn check_layout_collisions(groups: &[(PathBuf, Vec<syn::Item>)]) -> Result<(), String> {
    let mut seen: HashMap<String, &Path> = HashMap::new();
    for (dir, items) in groups {
        for item in items {
            let name = match item {
                syn::Item::Struct(s) if has_account_type_attr(&s.attrs) => s.ident.to_string(),
                syn::Item::Enum(e) if has_account_type_attr(&e.attrs) => e.ident.to_string(),
                _ => continue,
            };
            if let Some(first) = seen.insert(name.clone(), dir) {
                return Err(format!(
                    "account layout `{name}` is declared in both `{}` and `{}`; \
                    two layouts of one name make the IDL ambiguous, rename one",
                    first.display(),
                    dir.display()
                ));
            }
        }
    }
    Ok(())
}

/// A slot field marker with no anchored fn is a silent mode
/// disagreement: the struct declares embedded intent and the extension
/// resolved dedicated. For a bootstrap anchor the window would ship
/// born renounced, for a no-op anchor it is dead bytes. Refused rather
/// than silently compiled as dedicated mode. Runs only with carriers
/// in scope. The IDL producers skip it, the consumer's build is the
/// gate.
fn check_dormant_anchors(dormant: &[DormantAnchor], items: &[syn::Item]) -> Result<(), String> {
    for anchor in dormant {
        if let Some(carrier) = find_slot_carrier(items, &anchor.role)? {
            return Err(format!(
                "struct `{}` carries a #[{}] field but no fn carries \
                #[{}]; the marked field declares embedded mode and \
                nothing anchors it, so the program would compile as \
                dedicated mode with a dead slot window. Anchor the \
                account-creating instruction with #[{}], or remove the \
                #[{}] marker for dedicated mode",
                carrier.struct_name, carrier.attr_name, anchor.attr, anchor.attr, carrier.attr_name
            ));
        }
    }
    Ok(())
}

/// The `crate::fn_name` form a wrap's `exempt` list matches, built from
/// the declaring crate's path and the fn's own name.
fn qualified_instruction_name(crate_path: &syn::Path, fn_name: &syn::Ident) -> String {
    format!(
        "{}::{fn_name}",
        crate_path
            .segments
            .first()
            .map(|s| s.ident.to_string())
            .unwrap_or_default()
    )
}

impl ProgramDeps {
    /// Crates whose `#[account_type]` declares one of this program's
    /// account layouts: the code its author owns, plus the extensions
    /// they activated. Derived from the two lists rather than stored
    /// beside them, so it cannot fall out of step with either.
    ///
    /// An extension linked by path appears in both, so the result is
    /// deduplicated by canonical path.
    pub fn connected_dirs(&self) -> Vec<PathBuf> {
        let mut seen = HashSet::new();
        self.graph
            .owned_dirs
            .iter()
            .chain(&self.extensions.activated_dirs)
            .filter(|dir| seen.insert(crate::dep_walk::canonical_key(dir)))
            .cloned()
            .collect()
    }
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
    /// Where the declaring extension's state sits inside the consumer's
    /// account, set by [`rewrite_embedded_roles`]. `Some` exactly when
    /// this extension is in embedded mode, which is what makes the
    /// framework the only writer of the gate's location kwargs.
    pub embedded_offset: Option<OffsetSpec>,
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
    embeds: Vec<Embed>,
    bound_calls: HashMap<String, Vec<BoundValue>>,
    marker: String,
    dir: PathBuf,
    dormant_anchor: Option<DormantAnchor>,
}

/// One extension's embedded-mode declaration: which extension declared
/// it, where its window sits, and the type occupying that window.
///
/// The three travel together because embedded mode requires all three:
/// discovery refuses an embed whose extension declares no
/// `embedded.state_type`, so a window always knows its own length and
/// the collision asserts can be emitted from the embed alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Embed {
    /// Crate name of the declaring extension.
    pub source: String,
    /// Role, embedding account, and offset from the module marker.
    pub decl: EmbedDecl,
    /// Type occupying the window, from `embedded.state_type` metadata.
    /// Window collision asserts read its `FixedBorshSize::SIZE`.
    pub state_type: String,
    /// The consumer struct carrying this role's `*_slot` field, bound by
    /// [`resolve_derived_offsets`]. `None` under [`Carriers::Skip`], and
    /// for a literal offset whose role no struct marks.
    pub carrier: Option<SlotCarrier>,
}

/// An anchor-capable extension that resolved to dedicated mode: it
/// declares `embedded.anchor_attr` and no fn carries the attr. Kept so
/// the dispatcher can refuse a slot carrier with no anchor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DormantAnchor {
    /// Crate name of the declaring extension.
    pub source: String,
    /// The anchor attr no fn carries.
    pub attr: String,
    /// The role whose `*_slot` field marker declares embedded intent.
    pub role: String,
}

/// Producer entry point: marker pre-check, graph resolution, and
/// extension discovery in one call. Modules without candidate markers
/// skip the cargo metadata walk and discovery entirely.
///
/// # Errors
///
/// `Err` on malformed spel metadata, a marker placed above
/// `#[lez_program]`, or a candidate marker matching no discovered
/// extension, whatever the reason it did not match; callers surface it
/// as a compile error.
pub fn resolve_program_deps<F: FnMut(String)>(
    start: &Path,
    mod_attrs: &[Attribute],
    mod_items: &[syn::Item],
    on_warning: &mut F,
) -> Result<ProgramDeps, String> {
    let with_metadata = has_extension_marker_candidates(mod_attrs);
    let graph = crate::dep_walk::resolve_dep_graph(start, with_metadata, on_warning);
    resolve_program_deps_with_graph(graph, mod_attrs, mod_items, on_warning)
}

/// [`resolve_program_deps`] over a graph the caller already resolved.
///
/// A producer that needs the graph for its own work hands it over
/// rather than paying for a second `cargo metadata` of the same
/// manifest. Discovery reads `direct_dirs`, so a graph resolved with
/// metadata enabled satisfies every marker a self-resolved one would.
///
/// # Errors
///
/// The same as [`resolve_program_deps`].
pub fn resolve_program_deps_with_graph<F: FnMut(String)>(
    graph: crate::dep_walk::DepGraph,
    mod_attrs: &[Attribute],
    mod_items: &[syn::Item],
    on_warning: &mut F,
) -> Result<ProgramDeps, String> {
    let with_metadata = has_extension_marker_candidates(mod_attrs);
    let extensions = if with_metadata {
        discover_extensions(&graph.direct_dirs, mod_attrs, mod_items, on_warning)?
    } else {
        ExtensionDiscoveries::default()
    };
    if with_metadata {
        let unmatched: Vec<String> = candidate_marker_names(mod_attrs)
            .into_iter()
            .filter(|c| !extensions.matched_markers.contains(c))
            .collect();
        if let Some(reason) = &graph.metadata_failure {
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
        } else if !unmatched.is_empty() {
            return Err(format!(
                "marker(s) {unmatched:?} matched no extension in the program's \
                direct dependencies. A marker matches a direct dependency that \
                declares extension_attr = \"<marker>\" in its spel metadata; \
                check the marker for typos, and note that transitive \
                dependencies are never discovered."
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
/// `mod_items` are the program module's top-level items, the only
/// place a dispatched fn (and so an embed anchor) can live. File-backed
/// modules and path deps never participate, unlike the carrier scan
/// for derived offsets. Tests without anchors pass `&[]`.
///
/// # Errors
///
/// `Err` on malformed spel metadata (callers surface it as a compile
/// error). Environmental skips are reported via `on_warning`.
fn discover_extensions<F: FnMut(String)>(
    dep_dirs: &[PathBuf],
    mod_attrs: &[Attribute],
    mod_items: &[syn::Item],
    on_warning: &mut F,
) -> Result<ExtensionDiscoveries, String> {
    let lez_pos = mod_attrs
        .iter()
        .position(|a| a.path().is_ident("lez_program"));
    let mut matched: Vec<MatchedExtension> = Vec::new();
    for dep_dir in dep_dirs {
        if let Some(m) = match_extension(dep_dir, lez_pos, mod_attrs, mod_items, on_warning)? {
            matched.push(m);
        }
    }
    Ok(flatten_in_marker_order(matched))
}

/// One dependency dir against the module: `None` when it is not an
/// extension the consumer activated, `Some` with everything it
/// contributes when it is.
///
/// The opening reads stay in sequence rather than behind names: manifest,
/// `extension_attr`, marker match and package ident together are the
/// four conditions that must all hold before a crate counts as an
/// activated extension.
///
/// # Errors
///
/// `Err` on malformed spel metadata, a marker above `#[lez_program]`,
/// embedded mode without `embedded.state_type`, or misdeclared bound
/// args. Callers surface it as a compile error.
fn match_extension<F: FnMut(String)>(
    dep_dir: &Path,
    lez_pos: Option<usize>,
    mod_attrs: &[Attribute],
    mod_items: &[syn::Item],
    on_warning: &mut F,
) -> Result<Option<MatchedExtension>, String> {
    let dir = dep_dir.to_path_buf();
    let Some(manifest_value) = read_manifest_value(dep_dir) else {
        return Ok(None);
    };
    let Some(ext_attr) = read_spel_extension_attr(&manifest_value, dep_dir)? else {
        return Ok(None);
    };
    let Some(marker_pos) = mod_attrs.iter().position(|a| a.path().is_ident(&ext_attr)) else {
        return Ok(None);
    };
    check_marker_below_lez(&ext_attr, marker_pos, lez_pos)?;
    let Some(crate_name) = read_package_ident(&manifest_value) else {
        on_warning(format!(
            "extension at '{}' matched module attribute but has no [package].name, skipped",
            dep_dir.display()
        ));
        return Ok(None);
    };

    let mut injects = read_spel_inject_specs(&manifest_value, dep_dir)?;
    for spec in &mut injects {
        spec.source = crate_name.clone();
    }
    let wrap = read_spel_wrap_instructions(&manifest_value, dep_dir)?;
    let embedded = read_spel_embedded(&manifest_value, dep_dir)?;
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

    let embeds: Vec<Embed> = resolve_embed(
        marker_args.embed,
        &embedded,
        mod_items,
        &crate_name,
        &ext_attr,
    )?
    .into_iter()
    .collect();
    let is_embedded = !embeds.is_empty();

    // Anchor declared, no fn carries it: dedicated mode, unless the
    // consumer marked a slot field. prepare() refuses that shape.
    let dormant_anchor = if embeds.is_empty() {
        embedded.anchor.as_ref().map(|a| DormantAnchor {
            source: crate_name.clone(),
            attr: a.attr.clone(),
            role: a.role.clone(),
        })
    } else {
        None
    };

    let crate_ident = syn::Ident::new(&crate_name, proc_macro2::Span::call_site());
    let crate_path: syn::Path = syn::parse_quote!(::#crate_ident);

    let (items, _) = collect_items_from_crate_dirs(std::slice::from_ref(&dir), &mut *on_warning);
    let funcs = collect_instruction_fns(&items);
    let funcs: Vec<ItemFn> = if is_embedded {
        funcs
            .into_iter()
            .filter(|f| !embedded.skip.iter().any(|s| f.sig.ident == *s))
            .collect()
    } else {
        funcs
    };
    let bound_args = read_spel_bound_args(&manifest_value, dep_dir)?;
    let (funcs, bound_calls) = strip_bound_args(
        funcs,
        &bound_args,
        embeds.first().map(|e| &e.decl),
        mod_attrs,
        &crate_name,
    )?;

    if funcs.is_empty() && injects.is_empty() && !has_wrap {
        on_warning(format!(
            "extension '{crate_name}' matched #[{ext_attr}] but contributes no \
            #[instruction] fns, no inject specs, and no wrap config"
        ));
    }
    let instructions = funcs
        .into_iter()
        .map(|func| (func, crate_path.clone()))
        .collect();
    Ok(Some(MatchedExtension {
        marker_pos,
        instructions,
        inject_specs: injects,
        wraps,
        embeds,
        bound_calls,
        marker: ext_attr,
        dir,
        dormant_anchor,
    }))
}

/// A marker above `#[lez_program]` expands first and is invisible to
/// the compiled program: the extension would appear in the IDL but not
/// in the dispatcher, so the placement is refused.
fn check_marker_below_lez(
    ext_attr: &str,
    marker_pos: usize,
    lez_pos: Option<usize>,
) -> Result<(), String> {
    if lez_pos.is_some_and(|lez| marker_pos < lez) {
        return Err(format!(
            "extension marker #[{ext_attr}] is above #[lez_program]: attributes \
            above expand first and are invisible to the compiled program, so the \
            extension would appear in the IDL but not in the dispatcher. Move \
            #[{ext_attr}] below #[lez_program]."
        ));
    }
    Ok(())
}

/// Decide an extension's embed, requiring its window type with it.
///
/// Embedded mode needs `embedded.state_type`: the window collision
/// asserts read the window's size through it, so an embed without one
/// is refused at discovery rather than surfacing at emission.
fn resolve_embed(
    marker_embed: Option<EmbedDecl>,
    embedded: &EmbeddedMeta,
    mod_items: &[syn::Item],
    crate_name: &str,
    ext_attr: &str,
) -> Result<Option<Embed>, String> {
    let Some(decl) = resolve_embed_decl(marker_embed, embedded, mod_items, crate_name, ext_attr)?
    else {
        return Ok(None);
    };
    let Some(state_type) = embedded.state_type.clone() else {
        return Err(format!(
            "extension '{crate_name}' is used in embedded mode but its \
            metadata declares no `embedded.state_type`; name the type \
            occupying the embedded window (e.g. state_type = \
            \"{crate_name}::MyConfig\") so window collision asserts can \
            be emitted"
        ));
    };
    Ok(Some(Embed {
        source: crate_name.to_string(),
        decl,
        state_type,
        carrier: None,
    }))
}

/// A crate's discovered fns with their bound params stripped, paired
/// with the values the dispatcher appends per fn.
type StrippedFns = (Vec<ItemFn>, HashMap<String, Vec<BoundValue>>);

/// Resolve an extension's bound args against the module's markers and
/// strip the bound trailing params form it's discovered fns.
///
/// Each bound arg names a trailing fn param the dispatcher fills at the
/// call site as a compile-time literal; the params come off here so no
/// IDL or validation path ever sees them. Trailing is enforced in
/// bound_args block order, because the dispatcher appends the values
/// after the transaction args.
///
/// # Errors
///
/// `Err` when a bound arg references a kwarg the framework does not
/// know, when a referenced marker or kwarg is absent with no declared
/// default, or when the bound params are not the trailing params in
/// declaration order. Callers surface it as a compile error.
fn strip_bound_args(
    funcs: Vec<ItemFn>,
    bound_args: &[BoundArg],
    embed: Option<&EmbedDecl>,
    mod_attrs: &[Attribute],
    crate_name: &str,
) -> Result<StrippedFns, String> {
    for bound in bound_args {
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
    let mut bound_calls: HashMap<String, Vec<BoundValue>> = HashMap::new();
    let mut stripped: Vec<ItemFn> = Vec::with_capacity(funcs.len());
    for mut f in funcs {
        let mut values = Vec::new();
        let mut found: Vec<usize> = Vec::new();
        for bound in bound_args {
            let Some(pos) = f.sig.inputs.iter().position(|input| {
                matches!(input, syn::FnArg::Typed(pt)
                    if matches!(&*pt.pat, syn::Pat::Ident(pi) if pi.ident == bound.arg))
            }) else {
                continue;
            };
            found.push(pos);
            values.push(resolve_bound_value(bound, embed, mod_attrs, crate_name)?);
        }
        let n = f.sig.inputs.len();
        let k = found.len();
        let trailing_in_order = found.iter().enumerate().all(|(i, pos)| *pos == n - k + i);
        if !trailing_in_order {
            return Err(format!(
                "extension '{crate_name}': bound_args params of `{}` must be \
                the trailing params, in bound_args block order; the dispatcher \
                appends their values after the transaction args",
                f.sig.ident
            ));
        }
        f.sig.inputs = f.sig.inputs.iter().take(n - k).cloned().collect();
        if !values.is_empty() {
            bound_calls.insert(f.sig.ident.to_string(), values);
        }
        stripped.push(f);
    }
    Ok((stripped, bound_calls))
}

/// Resolve one bound arg to its dispatch-time value.
///
/// Self shape (`from = "offset"`) reads the extension's own marker's
/// embed declaration. Cross shape (`from = "<marker>::offset"`) reads
/// the named peer marker's, so an extension can depend on where a peer
/// embedded its state (freeze ADR-0012: freeze binding `admin_offset`
/// from `admin_authority::offset`).
///
/// An explicit offset resolves to its number, a marker without one
/// stays a derivation for `resolve_derived_offsets` to lower, and the
/// `default` applies only when there is no embed at all. Deriving is a
/// resolution, not an absence: the default must never swallow it. A
/// missing marker or missing embed without a default is a hard error
/// at the consumer's build.
fn resolve_bound_value(
    bound: &BoundArg,
    self_embed: Option<&EmbedDecl>,
    mod_attrs: &[Attribute],
    crate_name: &str,
) -> Result<BoundValue, String> {
    let embed = match bound.from.split_once("::") {
        None => self_embed.cloned(),
        Some((marker, _)) => {
            let Some(args) = mod_attrs
                .iter()
                .find_map(|a| parse_marker_args(a, marker).transpose())
                .transpose()?
            else {
                return bound.default.map(BoundValue::Literal).ok_or_else(|| {
                    format!(
                        "extension '{crate_name}': bound_arg '{}' requires marker \
                        '#[{marker}]', which is not declared on this module, and \
                        declares no default",
                        bound.arg
                    )
                });
            };
            args.embed
        },
    };
    match embed {
        Some(e) => Ok(match e.offset {
            OffsetSpec::Literal(n) => BoundValue::Literal(n),
            OffsetSpec::Path(p) => BoundValue::Path(p),
            OffsetSpec::Derived => BoundValue::Derived { role: e.role },
        }),
        None => bound.default.map(BoundValue::Literal).ok_or_else(|| {
            format!(
                "extension `{crate_name}`: bound_arg `{}` read `{}` but the marker \
                declares no embed and the bound_arg declares no default",
                bound.arg, bound.from
            )
        }),
    }
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
        return Err(format!(
            "unreadable Cargo.toml under {}",
            crate_dir.display()
        ));
    };
    read_spel_inject_specs(&manifest_value, crate_dir)
}

/// Filter `#[instruction]`-annotated fns from a flat item list.
///
/// Used by framework codegen to pull instruction definitions out of
/// extension libraries (e.g. admin-authority) that ship pre-defined
/// instructions to be merged into a consuming program's IDL + dispatcher.
pub fn collect_instruction_fns(items: &[syn::Item]) -> Vec<ItemFn> {
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
    let mut seen: HashMap<String, String> = HashMap::new();
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
        out.activated_dirs.push(m.dir);
        out.dormant_anchors.extend(m.dormant_anchor);
    }
    out
}

/// Decide an extension's embedded declaration: the marker's role kwarg
/// for anchorless extensions, anchor inference for anchored ones, and
/// a hard error when both speak.
fn resolve_embed_decl(
    marker_embed: Option<EmbedDecl>,
    embedded: &EmbeddedMeta,
    mod_items: &[syn::Item],
    crate_name: &str,
    ext_attr: &str,
) -> Result<Option<EmbedDecl>, String> {
    match (marker_embed, &embedded.anchor) {
        (Some(_), Some(_)) => Err(format!(
            "extension `{crate_name}` declares an embed anchor \
            (embedded.anchor_attr); the marker's role kwarg is retired for \
            it, the anchor fn names the embedding account. Drop the kwarg \
            from #[{ext_attr}]"
        )),
        (None, Some(a)) => marker::infer_anchor_embed(mod_items, &a.attr, &a.role, crate_name),
        (embed, None) => Ok(embed),
    }
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
        Ok(discover_extensions(&graph.direct_dirs, mod_attrs, &[], on_warning)?.instructions)
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

        let deps = resolve_program_deps(&tmp.path().join("user"), &mod_attrs, &[], &mut |_| {})
            .expect("discovery through the producer entry point");
        assert_eq!(deps.extensions.instructions.len(), 1);
        assert!(
            deps.graph.direct_dirs.iter().any(|d| d.ends_with("my-ext")),
            "graph must contain the extension dir: {:?}",
            deps.graph.direct_dirs
        );
    }

    // A marker that matches nothing while resolution is healthy must
    // refuse, not silently drop the extension surface: the typo and the
    // transitive-only dependency both land here.
    #[test]
    fn unmatched_marker_with_healthy_resolution_is_a_hard_error() {
        let tmp = TempDir::new("program-deps-unmatched");
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
            #[lez_program]
            #[my_ext]
            #[my_extt]
        );

        let err = resolve_program_deps(&tmp.path().join("user"), &mod_attrs, &[], &mut |_| {})
            .expect_err("an unmatched marker must refuse to compile");
        assert!(
            err.contains("my_extt") && err.contains("transitive"),
            "message must name the marker and the transitive rule: {err}"
        );
        assert!(
            !err.contains("resolution failed"),
            "healthy resolution must use the healthy-path message: {err}"
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
        let deps = resolve_program_deps(&tmp.path().join("user"), &mod_attrs, &[], &mut |w| {
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

        let err = resolve_program_deps(&tmp.path().join("user"), &mod_attrs, &[], &mut |_| {})
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
        let err = discover_extensions(&graph.direct_dirs, &mod_attrs, &[], &mut |_| {})
            .expect_err("an unknown bound_args.from must be rejected");
        assert!(
            err.contains("only \"offset\" carries a value"),
            "unexpected error: {err}"
        );
    }

    // A bound param anywhere but trailing would silently shift the
    // remaining args, the dispatcher appends bound values last. Refuse.
    #[test]
    fn non_trailing_bound_param_is_a_hard_error() {
        let metadata = r#"
[package.metadata.spel]
extension_attr = "my_ext"

[[package.metadata.spel.bound_args]]
arg = "offset"
from = "offset"
default = 0

[package.metadata.spel.embedded]
state_type = "my_ext::ExtConfig"
"#;
        let lib_rs = r#"
#[instruction]
pub fn ext_action(offset: usize, account: AccountWithMetadata) -> SpelResult { todo!() }
"#;
        let tmp = TempDir::new("bound-non-trailing");
        ext_fixture(&tmp, metadata, lib_rs);
        let mod_attrs: Vec<Attribute> = syn::parse_quote!(
            #[lez_program]
            #[my_ext(gate_config = prog_config, offset = 32)]
        );
        let graph = crate::dep_walk::resolve_dep_graph(&tmp.path().join("user"), true, &mut |_| {});
        let err = discover_extensions(&graph.direct_dirs, &mod_attrs, &[], &mut |_| {})
            .expect_err("a non-trailing bound param must be refused");
        assert!(
            err.contains("ext_action") && err.contains("trailing"),
            "message must name the fn and the rule: {err}"
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

[package.metadata.spel.embedded]
state_type = "my_ext::ExtConfig"
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
        let ext = discover_extensions(&graph.direct_dirs, &mod_attrs, &[], &mut |_| {})
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
        assert_eq!(
            ext.bound_calls.get("ext_action"),
            Some(&vec![BoundValue::Literal(32)])
        );

        // Bare marker: dedicated mode resolves the default.
        let tmp = TempDir::new("bound-strip-dedicated");
        let mod_attrs = ext_fixture(&tmp, metadata, lib_rs);
        let graph = crate::dep_walk::resolve_dep_graph(&tmp.path().join("user"), true, &mut |_| {});
        let ext = discover_extensions(&graph.direct_dirs, &mod_attrs, &[], &mut |_| {})
            .expect("dedicated discovery must succeed");
        assert_eq!(
            ext.bound_calls.get("ext_action"),
            Some(&vec![BoundValue::Literal(0)])
        );
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
        let ext = discover_extensions(&graph.direct_dirs, &mod_attrs, &[], &mut |_| {})
            .expect("cross-marker discovery must succeed");
        assert_eq!(
            ext.bound_calls.get("ext_action"),
            Some(&vec![BoundValue::Literal(16)])
        );

        // Peer marker absent: the declared default applies.
        let tmp = TempDir::new("bound-cross-dedicated");
        let mod_attrs = ext_fixture(&tmp, metadata, lib_rs);
        let graph = crate::dep_walk::resolve_dep_graph(&tmp.path().join("user"), true, &mut |_| {});
        let ext = discover_extensions(&graph.direct_dirs, &mod_attrs, &[], &mut |_| {})
            .expect("absent peer with default must succeed");
        assert_eq!(
            ext.bound_calls.get("ext_action"),
            Some(&vec![BoundValue::Literal(0)])
        );
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
        let err = discover_extensions(&graph.direct_dirs, &mod_attrs, &[], &mut |_| {})
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
        let err = resolve_program_deps(&tmp.path().join("user"), &mod_attrs, &[], &mut |_| {})
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
        let deps = resolve_program_deps(&tmp.path().join("user"), &mod_attrs, &[], &mut |w| {
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
state_type = "my_ext::ExtConfig"
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
        let ext = discover_extensions(&graph.direct_dirs, &mod_attrs, &[], &mut |_| {})
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
        let ext = discover_extensions(&graph.direct_dirs, &mod_attrs, &[], &mut |_| {})
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

[package.metadata.spel.embedded]
state_type = "my_ext::ExtConfig"
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
        let ext = discover_extensions(&graph.direct_dirs, &mod_attrs, &[], &mut |_| {})
            .expect("embedded marker must be collected");
        assert_eq!(
            ext.embeds,
            vec![Embed {
                source: "my_ext".to_string(),
                carrier: None,
                // The window state type travels with the embed, so an
                // embed can always report its own length.
                state_type: "my_ext::ExtConfig".to_string(),
                decl: EmbedDecl {
                    role: "gate_config".to_string(),
                    account: "prog_config".to_string(),
                    offset: OffsetSpec::Literal(32),
                    initializer: None,
                },
            }]
        );
    }

    // Embedded mode requires the extension to name its window type:
    // without it the window collision asserts cannot be emitted.
    // Dedicated mode's indifference to the field is pinned by
    // dedicated_mode_keeps_skipped_initializer, whose fixture has none.
    #[test]
    fn embedded_mode_without_state_type_is_a_hard_error() {
        let tmp = TempDir::new("embed-no-state-type");
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
        let err = discover_extensions(&graph.direct_dirs, &mod_attrs, &[], &mut |_| {})
            .expect_err("embedded mode without state_type must be refused");
        assert!(
            err.contains("my_ext") && err.contains("embedded.state_type"),
            "message must name the crate and the field: {err}"
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

[package.metadata.spel.embedded]
state_type = "my_ext::ExtConfig"
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
        let mut ext = discover_extensions(&graph.direct_dirs, &mod_attrs, &[], &mut |_| {})
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
            embedded_offset: None,
        }];
        let embeds = vec![Embed {
            source: "my_ext".to_string(),
            carrier: None,
            state_type: "my_ext::ExtConfig".to_string(),
            decl: EmbedDecl {
                role: "nonexistent".to_string(),
                account: "prog_config".to_string(),
                offset: OffsetSpec::Literal(8),
                initializer: None,
            },
        }];
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
            embedded_offset: None,
        }];
        let embeds = vec![Embed {
            source: "my_ext".to_string(),
            carrier: None,
            state_type: "my_ext::ExtConfig".to_string(),
            decl: EmbedDecl {
                role: "gate_config".to_string(),
                account: "prog_config".to_string(),
                offset: OffsetSpec::Literal(32),
                initializer: None,
            },
        }];
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
        let injected =
            apply_wrap_and_inject(&mut func, &[], &specs, GateLocations::Emit, None).unwrap();
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
        let ext = discover_extensions(&graph.direct_dirs, &mod_attrs, &[], &mut |_| {})
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
        let ext = discover_extensions(&graph.direct_dirs, &b_first, &[], &mut |_| {}).unwrap();
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
        let ext = discover_extensions(&graph.direct_dirs, &a_first, &[], &mut |_| {}).unwrap();
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
        let err = discover_extensions(&graph.direct_dirs, &mod_attrs, &[], &mut |_| {})
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
        let ext = discover_extensions(&graph.direct_dirs, &mod_attrs, &[], &mut |_| {})
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
        // The fixture's own marker attrs are discarded: this test is
        // about a module that carries no extension marker at all.
        ext_fixture(
            &tmp,
            r#"
[package.metadata.spel]
extension_attr = "my_ext"
"#,
            r#"#[instruction] pub fn ext_action() -> SpelResult { todo!() }"#,
        );
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

    // Two layouts of one name make the IDL ambiguous, and both sit in
    // code the author owns or activated, so the error names both crates
    // and leaves the rename to them.
    #[test]
    fn colliding_layouts_name_both_crates() {
        let tmp = TempDir::new("dup-layouts");
        tmp.write(
            "a/src/lib.rs",
            "#[account_type]\npub struct Same { pub v: u64 }",
        );
        tmp.write(
            "b/src/lib.rs",
            "#[account_type]\npub struct Same { pub v: u8 }",
        );
        let program = PreparedProgram {
            connected_dirs: vec![tmp.path().join("a"), tmp.path().join("b")],
            ..Default::default()
        };
        let err = program
            .layout_items(Path::new("user/src/main.rs"), vec![])
            .unwrap_err();
        assert!(
            err.contains("Same") && err.contains("/a") && err.contains("/b"),
            "the error names the type and both crates: {err}"
        );
    }

    // An embedded extension's state type is a window inside the
    // consumer's account: its annotation is stripped so it arrives by
    // reference, while the extension's other layouts stay.
    #[test]
    fn embedded_state_type_is_not_a_layout() {
        let tmp = TempDir::new("demote-embedded");
        tmp.write(
            "ext/src/lib.rs",
            "#[account_type]\npub struct ExtConfig { pub v: u64 }\n\
             #[account_type]\npub struct Keeper { pub v: u64 }",
        );
        let program = PreparedProgram {
            connected_dirs: vec![tmp.path().join("ext")],
            embeds: vec![Embed {
                source: "my_ext".to_string(),
                state_type: "my_ext::ExtConfig".to_string(),
                carrier: None,
                decl: EmbedDecl {
                    role: "ext_config".to_string(),
                    account: "cfg".to_string(),
                    offset: OffsetSpec::Literal(0),
                    initializer: None,
                },
            }],
            ..Default::default()
        };
        let (layout, _) = program
            .layout_items(Path::new("user/src/main.rs"), vec![])
            .expect("no collision");

        let annotated: Vec<String> = layout
            .iter()
            .filter_map(|i| match i {
                syn::Item::Struct(s) if has_account_type_attr(&s.attrs) => {
                    Some(s.ident.to_string())
                },
                _ => None,
            })
            .collect();
        assert_eq!(annotated, vec!["Keeper".to_string()]);
        assert!(
            layout
                .iter()
                .any(|i| matches!(i, syn::Item::Struct(s) if s.ident == "ExtConfig")),
            "the demoted type stays in the item set for reference resolution"
        );
    }

    // The consumer's group is exempt from the demotion: a same-named
    // consumer struct is a different type and keeps its annotation.
    #[test]
    fn consumer_layout_survives_a_same_named_embed() {
        let tmp = TempDir::new("demote-exempt");
        tmp.write("ext/src/lib.rs", "");
        let consumer_items =
            syn::parse_file("#[account_type]\npub struct ExtConfig { pub own: u64 }")
                .unwrap()
                .items;
        let program = PreparedProgram {
            connected_dirs: vec![tmp.path().join("ext")],
            embeds: vec![Embed {
                source: "my_ext".to_string(),
                state_type: "my_ext::ExtConfig".to_string(),
                carrier: None,
                decl: EmbedDecl {
                    role: "ext_config".to_string(),
                    account: "cfg".to_string(),
                    offset: OffsetSpec::Literal(0),
                    initializer: None,
                },
            }],
            ..Default::default()
        };
        let (layout, _) = program
            .layout_items(Path::new("user/src/main.rs"), consumer_items)
            .expect("no collision");
        assert!(
            layout.iter().any(|i| matches!(i, syn::Item::Struct(s)
                    if s.ident == "ExtConfig" && has_account_type_attr(&s.attrs))),
            "the consumer's own layout keeps its annotation"
        );
    }

    // A cfg-gated consumer item is never compiled into the program, so
    // it is not a layout and cannot collide with one.
    #[test]
    fn cfg_gated_consumer_item_is_not_a_layout() {
        let tmp = TempDir::new("cfg-gated-consumer");
        tmp.write(
            "ext/src/lib.rs",
            "#[account_type]\npub struct Same { pub v: u64 }",
        );
        let consumer_items =
            syn::parse_file("#[cfg(test)]\n#[account_type]\npub struct Same { pub fixture: u8 }")
                .unwrap()
                .items;
        let program = PreparedProgram {
            connected_dirs: vec![tmp.path().join("ext")],
            ..Default::default()
        };
        let (layout, _) = program
            .layout_items(Path::new("user/src/main.rs"), consumer_items)
            .expect("a test fixture cannot collide");
        let same: Vec<&syn::ItemStruct> = layout
            .iter()
            .filter_map(|i| match i {
                syn::Item::Struct(s) if s.ident == "Same" => Some(s),
                _ => None,
            })
            .collect();
        assert_eq!(same.len(), 1, "the gated fixture is screened out");
        assert!(
            same[0]
                .fields
                .iter()
                .any(|f| f.ident.as_ref().is_some_and(|id| id == "v")),
            "the connected crate's layout survives"
        );
    }

    // The whole pipeline over a fabricated graph: an unowned crate's
    // annotation is inert, while a type it declares is still described
    // once something connected references it.
    #[test]
    fn unowned_annotations_are_inert_and_references_resolve() {
        let tmp = TempDir::new("unowned-inert");
        tmp.write(
            "conn/src/lib.rs",
            "#[account_type]\npub struct Wrapper { pub e: ExtEnum }",
        );
        tmp.write(
            "un/src/lib.rs",
            "#[account_type]\npub struct Planted { pub v: u64 }\n\
             pub enum ExtEnum { A, B }",
        );
        let program = PreparedProgram {
            connected_dirs: vec![tmp.path().join("conn")],
            graph: crate::dep_walk::DepGraph {
                transitive_dirs: vec![tmp.path().join("conn"), tmp.path().join("un")],
                ..Default::default()
            },
            ..Default::default()
        };
        let (layout, _) = program
            .layout_items(Path::new("user/src/main.rs"), vec![])
            .expect("no collision");
        let mut defs = program.unowned_defs();
        let (accounts, types) =
            crate::account_types::collect_account_types_from(&layout, &mut defs);

        let account_names: Vec<&str> = accounts.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(
            account_names,
            vec!["Wrapper"],
            "the unowned annotation is inert"
        );
        assert!(
            types.iter().any(|t| t.name == "ExtEnum"),
            "the referenced type resolves on demand: {types:?}"
        );
        assert!(
            !types.iter().any(|t| t.name == "Planted"),
            "nothing references the planted type, so it appears nowhere"
        );
        assert!(
            defs.files_read()
                .iter()
                .any(|f| f.ends_with("un/src/lib.rs")),
            "the lookup reports the file it read for rebuild tracking"
        );
    }

    // Anchor declared, no fn carries it, nothing marked: dedicated mode,
    // and discovery records the dormancy for the carrier check.
    #[test]
    fn unanchored_extension_records_a_dormant_anchor() {
        let tmp = TempDir::new("dormant-anchor");
        let mod_attrs = ext_fixture(
            &tmp,
            r#"
[package.metadata.spel]
extension_attr = "my_ext"

[package.metadata.spel.embedded]
state_type = "my_ext::ExtConfig"
anchor_attr = "ext_init"
anchor_role = "ext_config"
"#,
            r#"
#[instruction]
pub fn ext_action(account: AccountWithMetadata) -> SpelResult { todo!() }
"#,
        );
        let deps = resolve_program_deps(&tmp.path().join("user"), &mod_attrs, &[], &mut |_| {})
            .expect("dedicated resolution succeeds");
        assert_eq!(
            deps.extensions.dormant_anchors,
            vec![DormantAnchor {
                source: "my_ext".to_string(),
                attr: "ext_init".to_string(),
                role: "ext_config".to_string(),
            }]
        );
        assert!(deps.extensions.embeds.is_empty());
    }

    // A slot field marker with no anchored fn is a mode disagreement,
    // so prepare refuses it, naming the struct and both attrs.
    #[test]
    fn slot_carrier_without_anchor_refuses() {
        let mut deps = ProgramDeps::default();
        deps.extensions.dormant_anchors.push(DormantAnchor {
            source: "my_ext".to_string(),
            attr: "ext_init".to_string(),
            role: "ext_config".to_string(),
        });
        let items: Vec<syn::Item> =
            syn::parse_file("pub struct ProgConfig { pub v: u64, #[ext_slot] pub s: u8 }")
                .unwrap()
                .items;
        let Err(err) = deps.prepare(&[], Carriers::Resolve(&items)) else {
            panic!("a marked field with no anchor must refuse");
        };
        assert!(
            err.contains("ProgConfig")
                && err.contains("#[ext_init]")
                && err.contains("dedicated mode with a dead slot window"),
            "got: {err}"
        );
    }

    // Without the marked field the same dormancy is plain dedicated
    // mode, which is what an anchor-capable extension looks like for
    // every consumer that does not embed it.
    #[test]
    fn dormant_anchor_without_carrier_is_dedicated_mode() {
        let mut deps = ProgramDeps::default();
        deps.extensions.dormant_anchors.push(DormantAnchor {
            source: "my_ext".to_string(),
            attr: "ext_init".to_string(),
            role: "ext_config".to_string(),
        });
        let items: Vec<syn::Item> = syn::parse_file("pub struct ProgConfig { pub v: u64 }")
            .unwrap()
            .items;
        deps.prepare(&[], Carriers::Resolve(&items))
            .expect("no marked field, dedicated mode stands");
    }

    // The connected set is the author's own code plus the extensions
    // they switched on. An extension linked by path is in both lists and
    // must still appear once.
    #[test]
    fn connected_dirs_are_owned_plus_activated_without_repeats() {
        let tmp = TempDir::new("connected-dirs");
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
        let deps = resolve_program_deps(&tmp.path().join("user"), &mod_attrs, &[], &mut |_| {})
            .expect("discovery succeeds");

        assert_eq!(
            deps.extensions.activated_dirs.len(),
            1,
            "the matched extension is recorded"
        );
        let connected = deps.connected_dirs();
        let canonical: HashSet<_> = connected
            .iter()
            .map(|d| crate::dep_walk::canonical_key(d))
            .collect();
        assert_eq!(
            canonical.len(),
            connected.len(),
            "a path-linked extension is owned and activated, and appears once: {connected:?}"
        );
        assert!(
            connected.iter().any(|d| d.ends_with("my-ext")),
            "the extension's crate is connected: {connected:?}"
        );
    }

    // Resolving carriers and writing location kwargs are one decision:
    // a producer that writes a location is exactly the one that resolved
    // the carrier it names.
    #[test]
    fn carriers_decide_whether_locations_are_written() {
        let prepared = ProgramDeps::default()
            .prepare(&[], Carriers::Resolve(&[]))
            .expect("nothing to resolve");
        assert_eq!(prepared.locations, GateLocations::Emit);

        let prepared = ProgramDeps::default()
            .prepare(&[], Carriers::Skip)
            .expect("nothing to resolve");
        assert_eq!(prepared.locations, GateLocations::Omit);
    }

    // A producer holding a resolved graph hands it to IDL generation
    // instead of paying for a second resolution of one manifest. The
    // document must not depend on which entry point produced it.
    #[test]
    fn graph_entry_point_matches_the_dep_dirs_one() {
        let tmp = TempDir::new("idl-graph-entry");
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

#[account_type]
pub struct ExtState { pub v: u64 }
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
        tmp.write(
            "user/src/main.rs",
            r#"
#[lez_program]
#[my_ext]
mod user_program {
    #[instruction]
    pub fn update_value(account: AccountWithMetadata, value: u64) -> SpelResult { todo!() }
}
"#,
        );

        let source = tmp.path().join("user/src/main.rs");
        let graph = crate::dep_walk::resolve_dep_graph(&source, true, &mut |_| {});
        let dirs = graph.transitive_dirs.clone();

        let from_graph = crate::idl_gen::generate_idl_from_file_with_graph(&source, graph)
            .expect("generation over a caller-supplied graph");
        let from_dirs =
            crate::idl_gen::generate_idl_from_file_with_deps(&source, &dirs, &mut |_| {})
                .expect("generation that resolves its own graph");

        assert_eq!(
            serde_json::to_value(&from_graph).unwrap(),
            serde_json::to_value(&from_dirs).unwrap(),
            "the graph the caller supplies must not change the document"
        );
        assert!(
            from_graph
                .instructions
                .iter()
                .any(|i| i.name == "ext_action"),
            "the extension's instruction must survive both paths: {:?}",
            from_graph.instructions
        );
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
            &mut |_| {},
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
        let ext = discover_extensions(&graph.direct_dirs, &bare, &[], &mut |_| {}).unwrap();
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
        let ext = discover_extensions(&graph.direct_dirs, &bare, &[], &mut |_| {}).unwrap();
        assert_eq!(ext.wraps.len(), 1);
        assert_eq!(ext.wraps[0].0, "");

        // Marker with ident arg: arg is the ident (skip matching happens
        // at the producer).
        let manual: Vec<Attribute> = syn::parse_quote!(
            #[lez_program]
            #[my_ext(manual)]
        );
        let ext = discover_extensions(&graph.direct_dirs, &manual, &[], &mut |_| {}).unwrap();
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
        let ext = discover_extensions(&graph.direct_dirs, &attrs, &[], &mut |_| {}).unwrap();
        assert!(ext.wraps.is_empty());
    }

    // The dedicated-mode default fills a missing embed, never a derived
    // one: deriving is a resolution, and a default of 0 silently
    // pointing every gate at offset 0 is the failure this pins against.
    #[test]
    fn derived_embed_never_falls_back_to_the_default() {
        let bound = BoundArg {
            arg: "offset".into(),
            from: "offset".into(),
            default: Some(0),
        };
        let embed = EmbedDecl {
            role: "gate_config".into(),
            account: "cfg".into(),
            offset: OffsetSpec::Derived,
            initializer: None,
        };
        let v = resolve_bound_value(&bound, Some(&embed), &[], "my-ext").expect("resolves");
        assert_eq!(
            v,
            BoundValue::Derived {
                role: "gate_config".into()
            }
        );
    }

    // No embed at all is what the default is for.
    #[test]
    fn absent_embed_falls_back_to_the_default() {
        let bound = BoundArg {
            arg: "offset".into(),
            from: "offset".into(),
            default: Some(0),
        };
        let v = resolve_bound_value(&bound, None, &[], "my-ext").expect("resolves");
        assert_eq!(v, BoundValue::Literal(0));
    }

    fn anchor_meta(anchor: Option<(&str, &str)>) -> EmbeddedMeta {
        EmbeddedMeta {
            anchor: anchor.map(|(attr, role)| metadata::EmbedAnchor {
                attr: attr.to_string(),
                role: role.to_string(),
            }),
            ..EmbeddedMeta::default()
        }
    }

    fn kwarg_embed() -> EmbedDecl {
        EmbedDecl {
            role: "ext_config".into(),
            account: "cfg".into(),
            offset: OffsetSpec::Literal(32),
            initializer: None,
        }
    }

    // The four arms of the embed decision, in one place.
    #[test]
    fn marker_kwarg_with_anchor_metadata_refuses() {
        let err = resolve_embed_decl(
            Some(kwarg_embed()),
            &anchor_meta(Some(("ext_init", "ext_config"))),
            &[],
            "my-ext",
            "my_ext",
        )
        .expect_err("two writers must refuse");
        assert!(err.contains("anchor") && err.contains("my_ext"), "{err}");
    }

    #[test]
    fn marker_kwarg_without_anchor_passes_through() {
        let embed = resolve_embed_decl(
            Some(kwarg_embed()),
            &anchor_meta(None),
            &[],
            "my-ext",
            "my_ext",
        )
        .unwrap()
        .expect("the kwarg decl survives");
        assert_eq!(embed.account, "cfg");
    }

    #[test]
    fn anchor_without_kwarg_infers_from_the_module() {
        let items: Vec<syn::Item> = syn::parse_file(
            "#[ext_init]\npub fn initialize(#[account(init)] cfg: A) -> R { todo!() }",
        )
        .unwrap()
        .items;
        let embed = resolve_embed_decl(
            None,
            &anchor_meta(Some(("ext_init", "ext_config"))),
            &items,
            "my-ext",
            "my_ext",
        )
        .unwrap()
        .expect("the anchor infers");
        assert_eq!(embed.account, "cfg");
        assert_eq!(embed.offset, OffsetSpec::Derived);
    }

    #[test]
    fn neither_kwarg_nor_anchor_is_dedicated() {
        let embed = resolve_embed_decl(None, &anchor_meta(None), &[], "my-ext", "my_ext").unwrap();
        assert!(embed.is_none());
    }

    // The reason inference lives in discovery: embedded.skip filters the
    // instruction set right there, so an inferred embed must drop the
    // extension's initializer exactly like a kwarg-declared one.
    #[test]
    fn anchored_extension_infers_embed_and_skips_initializer() {
        let tmp = TempDir::new("program-deps-anchored");
        let mod_attrs = ext_fixture(
            &tmp,
            r#"
[package.metadata.spel]
extension_attr = "my_ext"

[package.metadata.spel.embedded]
skip = ["ext_init"]
state_type = "my_ext::ExtConfig"
anchor_attr = "ext_init"
anchor_role = "ext_config"
"#,
            r#"
#[instruction]
pub fn ext_init(account: AccountWithMetadata) -> SpelResult { todo!() }
#[instruction]
pub fn ext_action(account: AccountWithMetadata) -> SpelResult { todo!() }
"#,
        );
        let items: Vec<syn::Item> = syn::parse_file(
            "#[ext_init]\npub fn initialize(#[account(init)] my_cfg: A) -> R { todo!() }",
        )
        .unwrap()
        .items;

        let deps = resolve_program_deps(&tmp.path().join("user"), &mod_attrs, &items, &mut |_| {})
            .expect("anchored discovery succeeds");
        assert_eq!(
            deps.extensions.embeds,
            vec![Embed {
                source: "my_ext".to_string(),
                carrier: None,
                state_type: "my_ext::ExtConfig".to_string(),
                decl: EmbedDecl {
                    role: "ext_config".into(),
                    account: "my_cfg".into(),
                    offset: OffsetSpec::Derived,
                    initializer: Some("ext_init".into()),
                },
            }]
        );
        let names: Vec<String> = deps
            .extensions
            .instructions
            .iter()
            .map(|(f, _)| f.sig.ident.to_string())
            .collect();
        assert!(
            !names.contains(&"ext_init".to_string()),
            "the skip filter must fire on an inferred embed: {names:?}"
        );
        assert!(names.contains(&"ext_action".to_string()), "{names:?}");
    }

    // The shorthand was swapped only inside the dispatcher macro, so an
    // IDL built from #[initialize] described an instruction without the
    // accounts the dispatcher injects, and a client generated from it
    // sent the wrong accounts. Both spellings must gate alike.
    #[test]
    fn the_shorthand_gates_exactly_as_the_explicit_anchor() {
        let program = || PreparedProgram {
            inject_specs: vec![InjectSpec {
                wrapper: "admin_initialize".to_string(),
                accounts: vec![InjectAccount {
                    name: "caller".to_string(),
                    role: "caller".to_string(),
                    seeds: Vec::new(),
                    signer: true,
                    embedded: false,
                }],
                source: "admin".to_string(),
                embedded_offset: None,
            }],
            embeds: vec![Embed {
                source: "admin".to_string(),
                carrier: None,
                state_type: "admin::Cfg".to_string(),
                decl: EmbedDecl {
                    role: "admin_config".to_string(),
                    account: "config".to_string(),
                    offset: OffsetSpec::Literal(32),
                    initializer: Some("admin_initialize".to_string()),
                },
            }],
            ..Default::default()
        };
        let params = |f: &ItemFn| -> Vec<String> {
            f.sig
                .inputs
                .iter()
                .filter_map(|a| match a {
                    syn::FnArg::Typed(pt) => match &*pt.pat {
                        syn::Pat::Ident(pi) => Some(pi.ident.to_string()),
                        _ => None,
                    },
                    _ => None,
                })
                .collect()
        };
        let mut shorthand: ItemFn = syn::parse_quote! {
            #[initialize]
            pub fn initialize(config: AccountWithMetadata) -> SpelResult { todo!() }
        };
        let mut explicit: ItemFn = syn::parse_quote! {
            #[admin_initialize]
            pub fn initialize(config: AccountWithMetadata) -> SpelResult { todo!() }
        };
        program()
            .gate(&mut shorthand, None)
            .expect("the shorthand gates");
        program()
            .gate(&mut explicit, None)
            .expect("the explicit anchor gates");
        assert_eq!(
            params(&shorthand),
            params(&explicit),
            "the shorthand must inject what the explicit anchor injects"
        );
        assert!(
            params(&shorthand).iter().any(|p| p == "caller"),
            "the bootstrap's injected caller must reach every producer: {:?}",
            params(&shorthand)
        );
    }
}
