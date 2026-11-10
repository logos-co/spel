//! Reads a crate's source for IDL generation: every item, with the module it lives in.
//!
//! Follows `mod` declarations to their files and leaves out items a default build doesn't
//! compile (`#[cfg(test)]`, `#[cfg(feature = "...")]`), so test-only types never reach an IDL.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use syn::Attribute;

/// An item and the path of the module it is declared in, relative to the crate root
/// (`[]` for `lib.rs`, `["event"]` for `src/event.rs`).
#[derive(Debug, Clone)]
pub struct SourceItem {
    pub module: Vec<String>,
    pub item: syn::Item,
}

/// Every item of the crate rooted at `crate_dir` (`src/lib.rs`), or an error naming the file
/// that could not be read or parsed.
///
/// # Errors
/// If `src/lib.rs` is missing, or a reachable source file can't be read or parsed.
pub fn crate_items(crate_dir: &Path) -> Result<Vec<SourceItem>, String> {
    let lib_rs = crate_dir.join("src").join("lib.rs");
    if !lib_rs.exists() {
        return Err(format!("{} has no src/lib.rs", crate_dir.display()));
    }
    let mut out = Vec::new();
    let mut visited = HashSet::new();
    collect_file(&lib_rs, &[], &mut out, &mut visited)?;
    Ok(out)
}

fn collect_file(
    path: &Path,
    module: &[String],
    out: &mut Vec<SourceItem>,
    visited: &mut HashSet<PathBuf>,
) -> Result<(), String> {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if !visited.insert(canonical) {
        return Ok(());
    }
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let file =
        syn::parse_file(&content).map_err(|e| format!("cannot parse {}: {e}", path.display()))?;

    // Sub-module files for `lib.rs` / `mod.rs` live alongside the file; for `foo.rs` they
    // live in a `foo/` directory next to it.
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let sub_base = if matches!(file_name, "lib.rs" | "mod.rs" | "main.rs") {
        path.parent().map(Path::to_path_buf)
    } else {
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        path.parent().map(|p| p.join(stem))
    };
    collect_items(&file.items, sub_base.as_deref(), module, out, visited)
}

fn collect_items(
    items: &[syn::Item],
    base_dir: Option<&Path>,
    module: &[String],
    out: &mut Vec<SourceItem>,
    visited: &mut HashSet<PathBuf>,
) -> Result<(), String> {
    for item in items {
        if is_cfg_excluded(item_attrs(item)) {
            continue;
        }
        if let syn::Item::Mod(m) = item {
            let mut inner_module = module.to_vec();
            inner_module.push(m.ident.to_string());
            if let Some((_, inner)) = &m.content {
                let inner_base = base_dir.map(|d| d.join(m.ident.to_string()));
                collect_items(inner, inner_base.as_deref(), &inner_module, out, visited)?;
            } else if let Some(path) = base_dir.and_then(|dir| mod_file_path(m, dir)) {
                collect_file(&path, &inner_module, out, visited)?;
            }
            continue;
        }
        out.push(SourceItem {
            module: module.to_vec(),
            item: item.clone(),
        });
    }
    Ok(())
}

fn item_attrs(item: &syn::Item) -> &[Attribute] {
    match item {
        syn::Item::Struct(s) => &s.attrs,
        syn::Item::Enum(e) => &e.attrs,
        syn::Item::Fn(f) => &f.attrs,
        syn::Item::Trait(t) => &t.attrs,
        syn::Item::Impl(i) => &i.attrs,
        syn::Item::Type(t) => &t.attrs,
        syn::Item::Static(s) => &s.attrs,
        syn::Item::Const(c) => &c.attrs,
        syn::Item::Mod(m) => &m.attrs,
        syn::Item::Use(u) => &u.attrs,
        _ => &[],
    }
}

/// Resolve the on-disk path for an external `mod` declaration.
///
/// Resolution order:
/// 1. `#[path = "..."]` attribute on the mod item — resolved relative to `base_dir`.
/// 2. `<base_dir>/<mod_name>.rs`
/// 3. `<base_dir>/<mod_name>/mod.rs`
///
/// Returns `None` if no candidate file exists.
fn mod_file_path(m: &syn::ItemMod, base_dir: &Path) -> Option<PathBuf> {
    // Check for explicit #[path = "..."] override first.
    for attr in &m.attrs {
        if attr.path().is_ident("path") {
            if let Ok(syn::MetaNameValue {
                value:
                    syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(s),
                        ..
                    }),
                ..
            }) = attr.meta.require_name_value()
            {
                let p = base_dir.join(s.value());
                if p.exists() {
                    return Some(p);
                }
            }
        }
    }

    // Standard two-candidate resolution.
    let mod_name = m.ident.to_string();
    let flat = base_dir.join(format!("{mod_name}.rs"));
    if flat.exists() {
        return Some(flat);
    }
    let nested = base_dir.join(&mod_name).join("mod.rs");
    if nested.exists() {
        return Some(nested);
    }
    None
}

/// Return `true` if the item's attributes contain a `#[cfg(...)]` that would
/// exclude it from a default (non-test, no-extra-features) build.
///
/// Handles:
/// - `#[cfg(test)]` — always excluded (test-only).
/// - `#[cfg(feature = "...")]` — excluded (unknown which features are enabled).
/// - `#[cfg(any(test, ...))]` / `#[cfg(any(feature = "...", ...))]` — excluded
///   if *any* alternative references `test` or `feature`.
///
/// Does **not** handle:
/// - `#[cfg(all(...))]` — compound expressions requiring all conditions.
/// - Target-triple cfgs (`target_os`, `windows`, etc.) — unresolvable at
///   IDL-gen time without knowing the build target.
///
/// Note: `#[cfg(not(test))]` is handled correctly — the token scanner skips
/// contents of `not(...)` groups, so production-only items are included.
///
/// Note: `#[cfg_attr(test, ...)]` is **not** treated as exclusion because it
/// only conditionally applies attributes — it does not remove the item from
/// compilation (e.g. `cfg_attr(test, derive(Debug))` is common and valid).
fn is_cfg_excluded(attrs: &[Attribute]) -> bool {
    for attr in attrs {
        if !attr.path().is_ident("cfg") {
            continue;
        }
        // Structural parse of the cfg expression.
        if cfg_meta_excludes(&attr.meta) {
            return true;
        }
    }
    false
}

/// Check a `cfg(...)` attribute for exclusion triggers by scanning its token stream.
/// Handles bare paths (`test`, `feature = "..."`) and `any(...)` wrappers.
/// Uses exact identifier matching to avoid false positives (e.g. `target_feature`).
fn cfg_meta_excludes(meta: &syn::Meta) -> bool {
    match meta {
        syn::Meta::Path(path) => {
            // Bare path: `#[cfg(test)]` (unlikely at top level, but handle it)
            path.is_ident("test") || path.is_ident("feature")
        },
        syn::Meta::NameValue(nv) => {
            // Name-value: `#[cfg(feature = "...")]`
            nv.path.is_ident("feature")
        },
        syn::Meta::List(list) if list.path.is_ident("cfg") => {
            // Scan the inner tokens of #[cfg(...)] for test/feature identifiers.
            // This handles all cases: bare `test`, `feature = "x"`, and
            // `any(test, feature = "x")` — because we recurse into nested groups.
            cfg_tokens_have_exclusion(&list.tokens, false)
        },
        _ => false,
    }
}

/// Recursively scan a token stream for `test` or `feature` identifiers.
/// Skips tokens inside `not(...)` groups so `#[cfg(not(test))]` is not excluded.
///
/// When called from an `any(...)` context (in_any = true), returns true only if
/// ALL alternatives contain exclusion triggers.  This prevents false exclusions
/// like `#[cfg(any(not(test), feature = "x"))]` which should be included in
/// default builds because the `not(test)` alternative satisfies it.
fn cfg_tokens_have_exclusion(tokens: &proc_macro2::TokenStream, in_any: bool) -> bool {
    let mut iter = tokens.clone().into_iter().peekable();
    let mut alternatives: Vec<bool> = Vec::new(); // for any() context

    while let Some(token) = iter.next() {
        match token {
            proc_macro2::TokenTree::Ident(ident) if ident == "not" => {
                // Skip the next group (parentheses) — `not(test)` should not exclude.
                if let Some(proc_macro2::TokenTree::Group(_)) = iter.peek() {
                    iter.next(); // consume the group
                }
            },
            proc_macro2::TokenTree::Ident(ident) if ident == "any" => {
                // Handle any(...) — check if ALL alternatives would exclude.
                let next = iter.peek().cloned();
                if let Some(proc_macro2::TokenTree::Group(group)) = next {
                    iter.next(); // consume the group
                    let mut all_exclude = true;
                    let mut alt_count = 0;
                    // Split alternatives by comma at this level.
                    for alt_token in group.stream().clone() {
                        match alt_token {
                            proc_macro2::TokenTree::Group(alt_group) => {
                                // Nested expression like any(not(test), feature = "x")
                                alt_count += 1;
                                if !cfg_tokens_have_exclusion(&alt_group.stream(), true) {
                                    all_exclude = false;
                                }
                            },
                            proc_macro2::TokenTree::Ident(alt_ident) => {
                                alt_count += 1;
                                if alt_ident != "test" && alt_ident != "feature" {
                                    all_exclude = false;
                                }
                            },
                            _ => {},
                        }
                    }
                    // If no alternatives found (parse issue), be conservative.
                    if alt_count == 0 {
                        return true;
                    }
                    if in_any {
                        alternatives.push(all_exclude);
                    } else if all_exclude {
                        return true; // top-level: all alternatives exclude → exclude
                    }
                }
            },
            proc_macro2::TokenTree::Ident(ident) => {
                if ident == "test" || ident == "feature" {
                    if in_any {
                        alternatives.push(true);
                    } else {
                        return true;
                    }
                }
            },
            proc_macro2::TokenTree::Group(group) => {
                // Recurse into groups (parentheses, braces, brackets).
                let result = cfg_tokens_have_exclusion(&group.stream(), in_any);
                if in_any {
                    alternatives.push(result);
                } else if result {
                    return true;
                }
            },
            _ => {},
        }
    }

    // In any() context: exclude only if ALL alternatives exclude.
    if in_any && !alternatives.is_empty() {
        alternatives.iter().all(|&b| b)
    } else if in_any {
        // No exclusion triggers found in any alternative → don't exclude.
        false
    } else {
        false
    }
}
