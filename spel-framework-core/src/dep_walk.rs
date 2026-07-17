//! Dependency-graph resolution shared by IDL generation and extension
//! discovery.
//!
//! [`resolve_dep_graph`] resolves a crate's dependencies in one pass and
//! returns a [`DepGraph`] with two lists of deliberately different reach:
//!
//! - `transitive_dirs`: types referenced by a program's instructions may
//!   come through any runtime dependency, so IDL type collection follows
//!   the whole graph.
//! - `direct_dirs`: extension discovery must never pick up a dependency
//!   of a dependency (trust model's two-action rule), so it stops at the
//!   consumer's own `Cargo.toml`.
//!
//! Both lists merge two sources: a manifest walk for path dependencies
//! (fast, no subprocess) and a single shared `cargo metadata` call for
//! git and registry dependencies, filtered to normal-kind resolve edges
//! so dev- and build-deps stay out. All failures here are environmental
//! and go through the `on_warning` channel; `cargo metadata` being
//! unavailable degrades to path-only results so expansion stays
//! deterministic.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Parsed and validated `Cargo.toml`, or `None` after warning.
fn read_manifest_toml<F: FnMut(String)>(
    manifest: &Path,
    on_warning: &mut F,
) -> Option<toml::Value> {
    let content = match std::fs::read_to_string(manifest) {
        Ok(c) => c,
        Err(e) => {
            on_warning(format!(
                "⚠️  could not read manifest '{}': {}",
                manifest.display(),
                e
            ));
            return None;
        },
    };
    match toml::from_str(&content) {
        Ok(v) => Some(v),
        Err(e) => {
            on_warning(format!(
                "⚠️  failed to parse manifest '{}': {}",
                manifest.display(),
                e
            ));
            None
        },
    }
}

/// Everything the framework needs to know about a crate's dependency
/// graph, resolved in one pass with one `cargo metadata` invocation.
pub struct DepGraph {
    /// Transitive runtime dependency dirs. Feeds IDL type collection:
    /// instruction-arg types may come through any runtime dependency.
    pub transitive_dirs: Vec<PathBuf>,
    /// Depth-1 dependency dirs only. Feeds extension discovery: the trust
    /// model's two-action rule forbids transitive discovery.
    pub direct_dirs: Vec<PathBuf>,
}

/// Resolve the dependency graph of the crate owning `start` (a source
/// file or a crate directory).
///
/// Resolution: nearest `Cargo.toml`, with workspace roots resolved to the
/// member manifest containing `start`. Path dependencies come from a
/// manifest walk (no subprocess); git and registry dependencies from a
/// single `cargo metadata --offline` call shared by both result lists.
/// If `cargo metadata` fails, both lists degrade to path-only results.
pub fn resolve_dep_graph<F: FnMut(String)>(start: &Path, on_warning: &mut F) -> DepGraph {
    let empty = DepGraph {
        transitive_dirs: Vec::new(),
        direct_dirs: Vec::new(),
    };

    let Some(manifest) = find_crate_manifest(start, on_warning) else {
        return empty;
    };
    let Some(value) = read_manifest_toml(&manifest, on_warning) else {
        return empty;
    };
    let Some(manifest_dir) = manifest.parent().map(Path::to_path_buf) else {
        return empty;
    };

    // Workspace roots have no [dependencies] of their own: resolve to the
    // member manifest that contains `start`.
    let is_workspace = value.get("workspace").is_some() && value.get("package").is_none();
    let (manifest, value) = if is_workspace {
        let Some(member) = find_member_manifest(&manifest_dir, &value, start, on_warning) else {
            return empty;
        };
        let Some(member_value) = read_manifest_toml(&member, on_warning) else {
            return empty;
        };
        (member, member_value)
    } else {
        (manifest, value)
    };
    let Some(manifest_dir) = manifest.parent().map(Path::to_path_buf) else {
        return empty;
    };

    // Transitive path walk. `visited` also excludes the crate itself from
    // the metadata merge below.
    let mut transitive_dirs = Vec::new();
    let mut visited = HashSet::new();
    resolve_path_deps_recursive(&manifest, &mut transitive_dirs, &mut visited, on_warning);

    // Direct path deps straight from the [dependencies] table.
    let mut direct_dirs = Vec::new();
    if let Some(table) = value.get("dependencies").and_then(|v| v.as_table()) {
        for (name, dep) in table {
            if let Some(rel) = dep.get("path").and_then(|v| v.as_str()) {
                let dir = manifest_dir.join(rel);
                if dir.is_dir() {
                    direct_dirs.push(dir);
                } else {
                    on_warning(format!(
                        "path dependency '{}' points to non-existent directory: {}",
                        name,
                        dir.display()
                    ));
                }
            }
        }
    }

    // One subprocess feeds both merges.
    if let Some(meta) = cargo_metadata_json(&manifest, on_warning) {
        for dir in find_dep_dirs_via_cargo_metadata(&meta, &manifest) {
            let canonical = dir.canonicalize().unwrap_or_else(|_| dir.clone());
            if visited.insert(canonical) {
                transitive_dirs.push(dir);
            }
        }
        let mut seen: HashSet<PathBuf> = direct_dirs
            .iter()
            .map(|d| d.canonicalize().unwrap_or_else(|_| d.clone()))
            .collect();
        for dir in direct_normal_dep_dirs(&meta, &manifest) {
            let canonical = dir.canonicalize().unwrap_or_else(|_| dir.clone());
            if seen.insert(canonical) {
                direct_dirs.push(dir);
            }
        }
    }

    DepGraph {
        transitive_dirs,
        direct_dirs,
    }
}

// ── Manifest location ────────────────────────────────────────────────────

/// Walk up from `start` to find the nearest `Cargo.toml`.
fn find_crate_manifest<F: FnMut(String)>(start: &Path, on_warning: &mut F) -> Option<PathBuf> {
    let mut dir: &Path = if start.is_file() {
        start.parent()?
    } else {
        start
    };
    loop {
        let candidate = dir.join("Cargo.toml");
        if candidate.exists() {
            return Some(candidate);
        }
        dir = match dir.parent() {
            Some(p) => p,
            None => {
                on_warning(format!(
                    "⚠️  no Cargo.toml found walking up from '{}'",
                    start.display()
                ));
                return None;
            },
        };
    }
}

/// Given a workspace root directory, try to locate the member crate manifest
/// that contains `source_path`.
fn find_member_manifest<F: FnMut(String)>(
    workspace_root: &Path,
    workspace_value: &toml::Value,
    source_path: &Path,
    on_warning: &mut F,
) -> Option<PathBuf> {
    // Try to get the explicit member list from [workspace.members].
    let members: Vec<String> = workspace_value
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();

    // Expand glob patterns (e.g. "crates/*") into concrete directories.
    let concrete_members: Vec<String> = if members.iter().any(|m| m.contains('*')) {
        let mut expanded = Vec::new();
        for pattern in &members {
            if pattern.contains('*') {
                // Simple glob expansion: replace * with readdir.
                let prefix = pattern.split_once('*').map(|(p, _)| p).unwrap_or("");
                let dir = workspace_root.join(prefix);
                if let Ok(entries) = std::fs::read_dir(&dir) {
                    for entry in entries.flatten() {
                        if entry.file_type().map_or(true, |ft| ft.is_dir()) {
                            expanded.push(format!(
                                "{}/{}",
                                prefix,
                                entry.file_name().to_string_lossy()
                            ));
                        }
                    }
                }
            } else {
                expanded.push(pattern.clone());
            }
        }
        expanded
    } else {
        members.clone()
    };

    // Find the member whose directory contains source_path.
    let source_dir = source_path.parent().unwrap_or(source_path);
    for member in &concrete_members {
        let member_dir = workspace_root.join(member.as_str());
        if member_dir.is_dir() && source_dir.starts_with(&member_dir) {
            let manifest = member_dir.join("Cargo.toml");
            if manifest.exists() {
                return Some(manifest);
            }
        }
    }

    // Fallback: recursively search all subdirectories for a Cargo.toml that
    // contains source_path.  This handles nested workspace members (e.g.
    // `methods/guest`) when the explicit `members` list is absent/mismatched.
    on_warning(format!(
        "⚠️  workspace at '{}' has no matching member for '{}'; searching all subdirectories",
        workspace_root.display(),
        source_path.display()
    ));

    fn search_recursive(dir: &Path, target_dir: &Path) -> Option<PathBuf> {
        // Search children FIRST (depth-first), then check current dir.
        // This ensures we find the deepest matching member manifest rather
        // than returning the workspace root immediately.
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                if entry.file_type().is_ok_and(|ft| ft.is_dir()) {
                    if let Some(found) = search_recursive(&entry.path(), target_dir) {
                        return Some(found);
                    }
                }
            }
        }
        // Check current dir — but skip virtual workspace manifests (no [package]).
        let manifest = dir.join("Cargo.toml");
        if manifest.exists() && target_dir.starts_with(dir) {
            // Skip virtual workspace manifests that have [workspace] but no [package].
            let is_virtual_workspace = std::fs::read_to_string(&manifest)
                .ok()
                .and_then(|content| content.parse::<toml::Value>().ok())
                .map(|v| v.get("workspace").is_some() && v.get("package").is_none())
                .unwrap_or(false);
            if !is_virtual_workspace {
                return Some(manifest);
            }
        }
        None
    }

    search_recursive(workspace_root, source_dir)
}

// ── Path-dependency walk (no subprocess) ─────────────────────────────────

/// Recursively extract path-based dependencies from a manifest, following
/// transitive path deps.  `visited` tracks canonicalised directories to avoid
/// infinite loops.
fn resolve_path_deps_recursive<F: FnMut(String)>(
    manifest: &Path,
    dirs: &mut Vec<PathBuf>,
    visited: &mut HashSet<PathBuf>,
    on_warning: &mut F,
) {
    let manifest_dir = match manifest.parent() {
        Some(d) => d.to_path_buf(),
        None => return,
    };

    // Deduplicate by canonical path.
    let canonical = match &manifest_dir.canonicalize() {
        Ok(c) => c.clone(),
        Err(_) => manifest_dir.clone(),
    };
    if !visited.insert(canonical) {
        return; // already processed — cycle or duplicate
    }

    let content = match std::fs::read_to_string(manifest) {
        Ok(c) => c,
        Err(e) => {
            on_warning(format!(
                "⚠️  could not read manifest '{}': {}",
                manifest.display(),
                e
            ));
            return;
        },
    };
    let value: toml::Value = match toml::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            on_warning(format!(
                "⚠️  failed to parse manifest '{}': {}",
                manifest.display(),
                e
            ));
            return;
        },
    };

    // Skip workspace roots — they have no [dependencies].
    if value.get("workspace").is_some() && value.get("package").is_none() {
        return;
    }

    if let Some(table) = value.get("dependencies").and_then(|v| v.as_table()) {
        for (name, dep) in table {
            if let Some(rel) = dep.get("path").and_then(|v| v.as_str()) {
                let dep_dir = manifest_dir.join(rel);
                if !dep_dir.is_dir() {
                    on_warning(format!(
                        "⚠️  path dependency '{}' points to non-existent directory: {}",
                        name,
                        dep_dir.display()
                    ));
                    continue;
                }
                // Deduplicate by canonical path.
                let canonical = match &dep_dir.canonicalize() {
                    Ok(c) => c.clone(),
                    Err(_) => dep_dir.clone(),
                };
                if visited.contains(&canonical) {
                    continue;
                }
                dirs.push(dep_dir.clone());

                // Recurse into the dependency's own Cargo.toml for transitive deps.
                let dep_manifest = dep_dir.join("Cargo.toml");
                if dep_manifest.exists() {
                    resolve_path_deps_recursive(&dep_manifest, dirs, visited, on_warning);
                }
            }
        }
    }
}

// ── Cargo metadata layer (git/registry deps) ─────────────────────────────

/// Shared `cargo metadata --format-version 1` invocation. Parsed JSON, or
/// `None` after warning when cargo is unavailable, fails, or emits
/// unparseable output.
///
/// Runs `--offline`: this executes inside macro expansion, which must
/// never hit the network. By the time rustc expands the consumer crate,
/// cargo has already fetched its dependencies, so offline resolution
/// succeeds; where it cannot, callers degrade to path-only results.
fn cargo_metadata_json<F: FnMut(String)>(
    manifest: &Path,
    on_warning: &mut F,
) -> Option<serde_json::Value> {
    let output = match std::process::Command::new("cargo")
        .args([
            "metadata",
            "--format-version",
            "1",
            "--offline",
            "--manifest-path",
        ])
        .arg(manifest)
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            on_warning(format!("could not run `cargo metadata`: {e}"));
            return None;
        },
    };

    if !output.status.success() {
        on_warning(format!(
            "`cargo metadata` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
        return None;
    }
    match serde_json::from_slice(&output.stdout) {
        Ok(v) => Some(v),
        Err(e) => {
            on_warning(format!("failed to parse metadata: {e}"));
            None
        },
    }
}

/// Transitive runtime (normal-kind) dependency dirs from parsed metadata,
/// excluding workspace members and the crate itself.
fn find_dep_dirs_via_cargo_metadata(meta: &serde_json::Value, manifest: &Path) -> Vec<PathBuf> {
    let workspace_members: HashSet<String> = meta
        .get("workspace_members")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // Walk resolve.nodes from the root, keeping only edges whose dep_kinds
    // include the normal kind (represented as `null` in cargo metadata's
    // JSON) so dev- and build-dependencies stay out of the returned set.
    let normal_reachable = collect_normal_reachable(meta, manifest);

    let source_canonical = manifest.canonicalize().ok();
    let mut dirs = Vec::new();

    let empty = Vec::new();
    let packages = meta
        .get("packages")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty);
    for pkg in packages {
        let id = pkg.get("id").and_then(|v| v.as_str()).unwrap_or_default();
        if workspace_members.contains(id) {
            continue;
        }
        if let Some(reachable) = &normal_reachable {
            if !reachable.contains(id) {
                continue;
            }
        }

        let Some(mp) = pkg.get("manifest_path").and_then(|v| v.as_str()) else {
            continue;
        };
        let pkg_manifest = PathBuf::from(mp);

        if let (Some(src), Ok(pkg_c)) = (&source_canonical, pkg_manifest.canonicalize()) {
            if src == &pkg_c {
                continue;
            }
        }
        if let Some(dir) = pkg_manifest.parent() {
            dirs.push(dir.to_path_buf());
        }
    }

    dirs
}

/// Depth-1 normal-kind dependency dirs from parsed metadata. Path deps
/// appear here too; the caller dedups against the manifest walk.
fn direct_normal_dep_dirs(meta: &serde_json::Value, manifest: &Path) -> Vec<PathBuf> {
    let Some(root_id) = root_package_id(meta, manifest) else {
        return Vec::new();
    };

    let mut manifest_by_id = std::collections::HashMap::new();
    if let Some(packages) = meta.get("packages").and_then(|v| v.as_array()) {
        for pkg in packages {
            if let (Some(id), Some(mp)) = (
                pkg.get("id").and_then(|v| v.as_str()),
                pkg.get("manifest_path").and_then(|v| v.as_str()),
            ) {
                manifest_by_id.insert(id, PathBuf::from(mp));
            }
        }
    }

    let Some(node) = meta
        .get("resolve")
        .and_then(|r| r.get("nodes"))
        .and_then(|v| v.as_array())
        .and_then(|nodes| {
            nodes
                .iter()
                .find(|n| n.get("id").and_then(|v| v.as_str()) == Some(root_id.as_str()))
        })
    else {
        return Vec::new();
    };

    let mut dirs = Vec::new();
    if let Some(deps) = node.get("deps").and_then(|v| v.as_array()) {
        for dep in deps {
            if !is_normal_dep_edge(dep) {
                continue;
            }
            let Some(pkg_id) = dep.get("pkg").and_then(|v| v.as_str()) else {
                continue;
            };
            if let Some(dir) = manifest_by_id.get(pkg_id).and_then(|mp| mp.parent()) {
                dirs.push(dir.to_path_buf());
            }
        }
    }
    dirs
}

// ── Resolve-tree helpers ─────────────────────────────────────────────────

/// Package id of the crate owning `manifest`, falling back to
/// `resolve.root` for the single-crate case.
fn root_package_id(meta: &serde_json::Value, manifest: &Path) -> Option<String> {
    let manifest_c = manifest.canonicalize().ok();
    let packages = meta.get("packages")?.as_array()?;
    packages
        .iter()
        .find(|p| {
            let Some(mp) = p.get("manifest_path").and_then(|v| v.as_str()) else {
                return false;
            };
            match (manifest_c.as_ref(), PathBuf::from(mp).canonicalize().ok()) {
                (Some(a), Some(b)) => a == &b,
                _ => false,
            }
        })
        .and_then(|p| p.get("id").and_then(|v| v.as_str()))
        .or_else(|| {
            meta.get("resolve")
                .and_then(|r| r.get("root"))
                .and_then(|v| v.as_str())
        })
        .map(String::from)
}

/// Traverse `resolve.nodes` starting from the node matching `manifest`,
/// following only edges whose `dep_kinds[].kind == null` (normal). Returns
/// the set of reachable package IDs (excluding the root itself), or `None`
/// if the resolve tree is missing so callers fall back to unfiltered
/// behaviour.
fn collect_normal_reachable(meta: &serde_json::Value, manifest: &Path) -> Option<HashSet<String>> {
    let resolve = meta.get("resolve")?;
    let nodes = resolve.get("nodes")?.as_array()?;

    let mut by_id: std::collections::HashMap<&str, &serde_json::Value> =
        std::collections::HashMap::new();
    for node in nodes {
        if let Some(id) = node.get("id").and_then(|v| v.as_str()) {
            by_id.insert(id, node);
        }
    }

    let root_id = root_package_id(meta, manifest)?;

    let mut reachable: HashSet<String> = HashSet::new();
    let mut stack: Vec<&str> = vec![&root_id];
    while let Some(id) = stack.pop() {
        let Some(node) = by_id.get(id) else {
            continue;
        };
        let Some(deps) = node.get("deps").and_then(|v| v.as_array()) else {
            continue;
        };
        for dep in deps {
            if !is_normal_dep_edge(dep) {
                continue;
            }
            let Some(pkg_id) = dep.get("pkg").and_then(|v| v.as_str()) else {
                continue;
            };
            if reachable.insert(pkg_id.to_string()) {
                stack.push(pkg_id);
            }
        }
    }
    Some(reachable)
}

/// True when a resolve-tree edge carries the normal dependency kind
/// (`kind == null` in cargo metadata's JSON). Edges without `dep_kinds`
/// count as normal so missing data widens rather than silently drops.
fn is_normal_dep_edge(dep: &serde_json::Value) -> bool {
    dep.get("dep_kinds")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .any(|k| k.get("kind").is_some_and(|x| x.is_null()))
        })
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::TempDir;

    #[test]
    fn resolve_dep_graph_returns_local_path_deps() {
        let tmp = TempDir::new("find-path-deps");

        tmp.write(
            "core/Cargo.toml",
            r#"
[package]
name = "token_core"
version = "0.1.0"
edition = "2021"
"#,
        );
        tmp.write("core/src/lib.rs", "");

        tmp.write(
            "methods/guest/Cargo.toml",
            r#"
[package]
name = "token-guest"
version = "0.1.0"
edition = "2021"

[dependencies]
token_core = { path = "../../core" }
"#,
        );
        let program = tmp.write("methods/guest/src/bin/token.rs", "");

        let dirs = resolve_dep_graph(&program, &mut |_| {}).transitive_dirs;
        assert_eq!(dirs.len(), 1);
        assert!(
            dirs[0].ends_with("core"),
            "expected core dir, got {:?}",
            dirs[0]
        );
    }

    #[test]
    fn resolve_dep_graph_falls_back_to_path_only_when_metadata_fails() {
        // The fake `https://example.com/repo.git` URL makes `cargo metadata`
        // fail (cannot resolve the git dep). The registry version dep on
        // `serde` also fails because the temporary workspace has no
        // Cargo.lock. `resolve_dep_graph` should degrade gracefully and
        // still return the path-dep, proving the fallback path works.
        let tmp = TempDir::new("find-path-deps-filter");

        tmp.write(
            "core/Cargo.toml",
            r#"
[package]
name = "token_core"
version = "0.1.0"
edition = "2021"
"#,
        );
        tmp.write("core/src/lib.rs", "");

        tmp.write(
            "methods/guest/Cargo.toml",
            r#"
[package]
name = "token-guest"
version = "0.1.0"
edition = "2021"

[dependencies]
token_core = { path = "../../core" }
serde = { version = "1.0" }
nssa_core = { git = "https://example.com/repo.git", tag = "v1.0" }
"#,
        );
        let program = tmp.write("methods/guest/src/bin/token.rs", "");

        let dirs = resolve_dep_graph(&program, &mut |_| {}).transitive_dirs;
        assert_eq!(dirs.len(), 1);
        assert!(dirs[0].ends_with("core"));
    }

    #[test]
    fn resolve_dep_graph_ignores_dev_and_build_deps() {
        let tmp = TempDir::new("find-path-deps-dev-build");

        tmp.write(
            "core/Cargo.toml",
            r#"
[package]
name = "token_core"
version = "0.1.0"
edition = "2021"
"#,
        );
        tmp.write("core/src/lib.rs", "");
        tmp.write(
            "test_helpers/Cargo.toml",
            r#"
[package]
name = "test_helpers"
version = "0.1.0"
edition = "2021"
"#,
        );
        tmp.write("test_helpers/src/lib.rs", "");

        tmp.write(
            "methods/guest/Cargo.toml",
            r#"
[package]
name = "token-guest"
version = "0.1.0"
edition = "2021"

[dependencies]
token_core = { path = "../../core" }

[dev-dependencies]
test_helpers = { path = "../../test_helpers" }
"#,
        );
        let program = tmp.write("methods/guest/src/bin/token.rs", "");

        let dirs = resolve_dep_graph(&program, &mut |_| {}).transitive_dirs;
        assert_eq!(dirs.len(), 1, "expected only core, got: {dirs:?}");
        assert!(dirs[0].ends_with("core"));
    }

    #[test]
    fn resolve_dep_graph_resolves_transitive_deps() {
        let tmp = TempDir::new("transitive-deps");

        // shared_types -> core -> guest
        tmp.write(
            "shared/Cargo.toml",
            r#"
[package]
name = "shared_types"
version = "0.1.0"
edition = "2021"
"#,
        );
        tmp.write("shared/src/lib.rs", "");

        tmp.write(
            "core/Cargo.toml",
            r#"
[package]
name = "token_core"
version = "0.1.0"
edition = "2021"

[dependencies]
shared_types = { path = "../shared" }
"#,
        );
        tmp.write("core/src/lib.rs", "");

        tmp.write(
            "methods/guest/Cargo.toml",
            r#"
[package]
name = "token-guest"
version = "0.1.0"
edition = "2021"

[dependencies]
token_core = { path = "../../core" }
"#,
        );
        let program = tmp.write("methods/guest/src/bin/token.rs", "");

        let dirs = resolve_dep_graph(&program, &mut |_| {}).transitive_dirs;
        assert_eq!(dirs.len(), 2, "expected core and shared, got: {dirs:?}");
        let names: Vec<&str> = dirs
            .iter()
            .map(|d| d.file_name().unwrap().to_str().unwrap())
            .collect();
        assert!(names.contains(&"core"));
        assert!(names.contains(&"shared"));
    }

    #[test]
    fn resolve_dep_graph_dedups_diamond_graph() {
        // Diamond dep graph: sample → ext_a (direct), sample → ext_b → ext_a (transitive).
        // The buggy version of `resolve_path_deps_recursive` pushes ext_a to the
        // returned Vec twice — once via the direct edge, once via ext_b's transitive
        // edge — because the push happens before the visited-set check in the
        // recursive call. Latent on `main` because downstream callers
        // (`collect_items_from_crate_dirs`) have a second dedup layer keyed by
        // canonical source-file path, but a caller that skips that layer sees the
        // duplicate dir and produces duplicate items.
        //
        // This test reproduces the diamond and asserts the returned Vec contains
        // no duplicates by canonical path. Fails on buggy code, passes on the fix.

        let tmp = TempDir::new("diamond");

        tmp.write(
            "ext-a/Cargo.toml",
            r#"
[package]
name = "ext-a"
version = "0.1.0"
edition = "2021"
"#,
        );
        tmp.write("ext-a/src/lib.rs", "");

        tmp.write(
            "ext-b/Cargo.toml",
            r#"
[package]
name = "ext-b"
version = "0.1.0"
edition = "2021"

[dependencies]
ext-a = { path = "../ext-a" }
"#,
        );
        tmp.write("ext-b/src/lib.rs", "");

        tmp.write(
            "sample/Cargo.toml",
            r#"
[package]
name = "sample"
version = "0.1.0"
edition = "2021"

[dependencies]
ext-a = { path = "../ext-a" }
ext-b = { path = "../ext-b" }
"#,
        );
        tmp.write("sample/src/lib.rs", "");

        let dirs = resolve_dep_graph(&tmp.path().join("sample"), &mut |_| {}).transitive_dirs;

        // Canonicalise every returned dir, count unique canonical paths.
        // The dirs Vec MUST have no duplicate canonical paths — including ext-a,
        // which is reachable via both the direct edge and ext-b's transitive edge.
        let unique: HashSet<PathBuf> = dirs
            .iter()
            .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
            .collect();

        assert_eq!(
            unique.len(),
            dirs.len(),
            "resolve_dep_graph returned duplicate canonical paths: {:?}",
            dirs
        );

        // Also assert ext-a is in the result (paranoia: the diamond graph
        // actually got walked, not just an empty result).
        let ext_a_canonical = tmp.path().join("ext-a").canonicalize().unwrap();
        assert!(
            unique.contains(&ext_a_canonical),
            "ext-a not in result; diamond walk failed: {:?}",
            dirs
        );
    }
}
