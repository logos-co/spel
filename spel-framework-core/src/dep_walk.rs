//! Dependency-graph walks shared by IDL generation and extension discovery.
//!
//! Two entry points with deliberately different reach:
//!
//! - [`find_path_dep_dirs`] walks **transitively**: types referenced by a
//!   program's instructions may come through any runtime dependency, so
//!   IDL type collection follows the whole graph.
//! - [`direct_path_dep_dirs`] walks **depth-1 only**: extension discovery
//!   must never pick up a dependency of a dependency (trust model's
//!   two-action rule), so it stops at the consumer's own `Cargo.toml`.
//!
//! Both merge two sources: a manifest walk for path dependencies (fast,
//! no subprocess) and a `cargo metadata` call for git and registry
//! dependencies, filtered to normal-kind resolve edges so dev- and
//! build-deps stay out. All failures here are environmental and go
//! through the `on_warning` channel; `cargo metadata` being unavailable
//! degrades to path-only results so expansion stays deterministic.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Return the crate-root directories of every runtime dependency of the
/// `Cargo.toml` nearest to `source_path`.
///
/// Path dependencies are resolved transitively via a direct manifest walk
/// (fast, no subprocess). Git and registry dependencies are additionally
/// enumerated via a single `cargo metadata --format-version 1` call so that
/// extensions shipped through git URLs or crates.io remain discoverable by
/// name-based lookups (extension attrs, wrap specs, inject specs). Results
/// from both sources are merged and deduplicated by canonical path.
///
/// **Transitive path-dependencies** are resolved: if a discovered path
/// dependency itself declares further path-based dependencies, those are
/// included as well (with cycle detection).
///
/// `[dev-dependencies]` and `[build-dependencies]` are deliberately
/// excluded: types defined in those crates are not part of the program's
/// on-chain interface and must not appear in the generated IDL. The
/// `cargo metadata` merge only reads the runtime resolve tree, so dev and
/// build deps are naturally filtered out.
///
/// In workspace projects the function detects when the nearest `Cargo.toml`
/// is a workspace root manifest and searches for the actual crate manifest
/// containing `[dependencies]`; `cargo metadata` is invoked against that
/// member manifest so its dep graph is what gets enumerated.
///
/// `on_warning` is called for non-fatal issues (missing dep directories,
/// unparseable manifests, `cargo metadata` failures, etc.). Pass `|_| {}`
/// to ignore warnings. If `cargo metadata` fails or is unavailable the
/// function silently returns only the path-dep results so downstream
/// expansion stays deterministic.
pub fn find_path_dep_dirs<F: FnMut(String)>(source_path: &Path, mut on_warning: F) -> Vec<PathBuf> {
    let manifest = match find_crate_manifest(source_path, &mut on_warning) {
        Some(m) => m,
        None => return vec![],
    };

    let content = match std::fs::read_to_string(&manifest) {
        Ok(c) => c,
        Err(e) => {
            on_warning(format!(
                "⚠️  could not read manifest '{}': {}",
                manifest.display(),
                e
            ));
            return vec![];
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
            return vec![];
        },
    };

    let manifest_dir = match manifest.parent() {
        Some(d) => d.to_path_buf(),
        None => return vec![],
    };

    // Check if this is a workspace root — if so, it has no [dependencies] of its
    // own.  We need to find the actual crate manifest for the program binary.
    let is_workspace = value.get("workspace").is_some() && value.get("package").is_none();

    let mut dirs = Vec::new();
    let mut visited = HashSet::new();
    let metadata_manifest: Option<PathBuf> = if is_workspace {
        let member = find_member_manifest(&manifest_dir, &value, source_path, &mut on_warning);
        if let Some(m) = &member {
            resolve_path_deps_recursive(m, &mut dirs, &mut visited, &mut on_warning);
        }
        member
    } else {
        resolve_path_deps_recursive(&manifest, &mut dirs, &mut visited, &mut on_warning);
        Some(manifest.clone())
    };

    // Merge in git and registry dep dirs via `cargo metadata`. Best-effort:
    // any failure falls back to path-only results.

    if let Some(m) = &metadata_manifest {
        for dir in find_dep_dirs_via_cargo_metadata(m, &mut on_warning) {
            let canonical = dir.canonicalize().unwrap_or_else(|_| dir.clone());
            if visited.insert(canonical) {
                dirs.push(dir);
            }
        }
    }

    dirs
}

/// Path dependencies declared directly in this crate's own Cargo.toml.
/// One level, deliberately not transitive: a dependency of a dependency
/// can never contribute instructions the consumer did not opt into.
pub(crate) fn direct_path_dep_dirs<F: FnMut(String)>(
    manifest_dir: &Path,
    on_warning: &mut F,
) -> Vec<PathBuf> {
    let Some(manifest) = find_crate_manifest(manifest_dir, &mut |w| on_warning(w)) else {
        on_warning(format!(
            "could not locate a crate manifest from '{}'",
            manifest_dir.display()
        ));
        return vec![];
    };
    let manifest_dir = match manifest.parent() {
        Some(d) => d.to_path_buf(),
        None => return vec![],
    };
    let content = match std::fs::read_to_string(&manifest) {
        Ok(c) => c,
        Err(e) => {
            on_warning(format!(
                "could not read manifest '{}': {}",
                manifest.display(),
                e
            ));
            return vec![];
        },
    };
    let value: toml::Value = match toml::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            on_warning(format!(
                "failed to parse manifest '{}': {}",
                manifest.display(),
                e
            ));
            return vec![];
        },
    };

    let Some(table) = value.get("dependencies").and_then(|v| v.as_table()) else {
        return vec![];
    };
    let mut dirs = Vec::new();
    for (name, dep) in table {
        if let Some(rel) = dep.get("path").and_then(|v| v.as_str()) {
            let dir = manifest_dir.join(rel);
            if dir.is_dir() {
                dirs.push(dir);
            } else {
                on_warning(format!(
                    "path dependency '{}' points to non-existent directory: {}",
                    name,
                    dir.display()
                ));
            }
        }
    }
    // Merge depth-1 git/registry deps from `cargo metadata` so extensions
    // delivered as git or crates.io libraries are discoverable. Still
    // direct-only: the trust model's two-action rule is unchanged.
    let mut seen: HashSet<PathBuf> = dirs
        .iter()
        .map(|d| d.canonicalize().unwrap_or_else(|_| d.clone()))
        .collect();
    for dir in direct_normal_dep_dirs(&manifest, on_warning) {
        let canonical = dir.canonicalize().unwrap_or_else(|_| dir.clone());
        if seen.insert(canonical) {
            dirs.push(dir);
        }
    }

    dirs
}

// ── Manifest location ────────────────────────────────────────────────────

/// Walk up from `start` to find the nearest `Cargo.toml`.
fn find_crate_manifest<F: FnMut(String)>(
    start: &Path,
    on_warning: &mut F,
) -> Option<PathBuf> {
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

/// Transitive runtime (normal-kind) dependency dirs of `manifest` via
/// `cargo metadata`, excluding workspace members and the crate itself.
/// Feeds the IDL type-collection walk in [`find_path_dep_dirs`].
fn find_dep_dirs_via_cargo_metadata<F: FnMut(String)>(
    manifest: &Path,
    on_warning: &mut F,
) -> Vec<PathBuf> {
    let Some(meta) = cargo_metadata_json(manifest, on_warning) else {
        return Vec::new();
    };

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
    let normal_reachable = collect_normal_reachable(&meta, manifest);

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

/// Depth-1 normal-kind dependency dirs of `manifest` via `cargo metadata`.
/// Lets extension discovery see git- and registry-delivered libraries
/// without widening discovery to transitive deps. Path deps appear here
/// too. Callers dedup against the manifest walk. Environmental failures
/// warn and return empty so discovery degrades to path-only.
fn direct_normal_dep_dirs<F: FnMut(String)>(
    manifest: &Path,
    on_warning: &mut F,
) -> Vec<PathBuf> {
    let Some(meta) = cargo_metadata_json(manifest, on_warning) else {
        return Vec::new();
    };
    let Some(root_id) = root_package_id(&meta, manifest) else {
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
fn collect_normal_reachable(
    meta: &serde_json::Value,
    manifest: &Path,
) -> Option<HashSet<String>> {
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
    fn find_path_dep_dirs_returns_local_path_deps() {
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

        let dirs = find_path_dep_dirs(&program, |_| {});
        assert_eq!(dirs.len(), 1);
        assert!(
            dirs[0].ends_with("core"),
            "expected core dir, got {:?}",
            dirs[0]
        );
    }

    #[test]
    fn find_path_dep_dirs_falls_back_to_path_only_when_metadata_fails() {
        // The fake `https://example.com/repo.git` URL makes `cargo metadata`
        // fail (cannot resolve the git dep). The registry version dep on
        // `serde` also fails because the temporary workspace has no
        // Cargo.lock. `find_path_dep_dirs` should degrade gracefully and
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

        let dirs = find_path_dep_dirs(&program, |_| {});
        assert_eq!(dirs.len(), 1);
        assert!(dirs[0].ends_with("core"));
    }

    #[test]
    fn find_path_dep_dirs_ignores_dev_and_build_deps() {
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

        let dirs = find_path_dep_dirs(&program, |_| {});
        assert_eq!(dirs.len(), 1, "expected only core, got: {dirs:?}");
        assert!(dirs[0].ends_with("core"));
    }

    #[test]
    fn find_path_dep_dirs_resolves_transitive_deps() {
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

        let dirs = find_path_dep_dirs(&program, |_| {});
        assert_eq!(dirs.len(), 2, "expected core and shared, got: {dirs:?}");
        let names: Vec<&str> = dirs
            .iter()
            .map(|d| d.file_name().unwrap().to_str().unwrap())
            .collect();
        assert!(names.contains(&"core"));
        assert!(names.contains(&"shared"));
    }

    #[test]
    fn find_path_dep_dirs_dedups_diamond_graph() {
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

        let dirs = find_path_dep_dirs(&tmp.path().join("sample"), |_| {});

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
            "find_path_dep_dirs returned duplicate canonical paths: {:?}",
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
