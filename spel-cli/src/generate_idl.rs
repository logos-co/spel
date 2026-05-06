//! Source file discovery for `generate-idl`.
//!
//! The CLI calls [`discover_sources`] to turn an optional path argument into
//! a concrete list of `.rs` files.  The library crate (`spel-framework-core`)
//! only ever receives a single resolved path.
//!
//! ## Resolution (no argument)
//! Searches `./methods/guest/src/bin/*.rs` in the current working directory.
//!
//! ## Resolution (argument given)
//! - `.rs` file  → used directly (backwards-compatible).
//! - directory   → `<dir>/methods/guest/src/bin/*.rs` is searched.

use std::fs;
use std::path::{Path, PathBuf};

/// Result of path-dependency discovery, including warnings for any issues
/// encountered during manifest parsing or directory resolution.
pub struct PathDepResult {
    /// Crate-root directories of all discovered path dependencies (including
    /// transitive ones).
    pub dirs: Vec<PathBuf>,
    /// Non-fatal warnings emitted during discovery (e.g. TOML parse failures,
    /// missing dep directories, no Cargo.toml found).
    pub warnings: Vec<String>,
}

impl PathDepResult {
    pub fn new() -> Self {
        Self {
            dirs: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

/// Resolve the list of SPEL program source files for IDL generation.
///
/// `arg` is the optional positional argument passed to `generate-idl`:
/// - `None`         → auto-detect from `./methods/guest/src/bin/`
/// - `Some("*.rs")` → that file only
/// - `Some(dir)`    → `<dir>/methods/guest/src/bin/*.rs`
///
/// Returns an error string when no sources can be found.
pub fn discover_sources(arg: Option<&str>) -> Result<Vec<PathBuf>, String> {
    match arg {
        Some(p) => {
            let path = PathBuf::from(p);
            if path.extension().map_or(false, |e| e == "rs") {
                if !path.exists() {
                    return Err(format!("File not found: {}", p));
                }
                Ok(vec![path])
            } else if path.is_dir() {
                let sources = search_methods_dir(&path)?;
                if sources.is_empty() {
                    Err(format!(
                        "No .rs files found in '{}/methods/guest/src/bin/'.\n\
                         Pass a .rs file directly instead.",
                        p
                    ))
                } else {
                    Ok(sources)
                }
            } else {
                Err(format!("'{}' is not a .rs file or a directory", p))
            }
        }
        None => {
            let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
            let sources = search_methods_dir(&cwd)?;
            if !sources.is_empty() {
                return Ok(sources);
            }
            Err(
                "No SPEL program sources found.\n\
                 Searched: ./methods/guest/src/bin/*.rs\n\
                 \n\
                 Options:\n\
                 - Run from your project root (where 'methods/' lives)\n\
                 - Pass a project directory: generate-idl <path-to-project>\n\
                 - Pass a source file:       generate-idl <path-to-program.rs>"
                    .to_string(),
            )
        }
    }
}

/// Return the crate-root directories of all `path = "..."` entries in the
/// `[dependencies]` table of the `Cargo.toml` nearest to `source_path`.
///
/// Only runtime dependencies are considered.  `[dev-dependencies]` and
/// `[build-dependencies]` are deliberately excluded: types defined in those
/// crates are not part of the program's on-chain interface and must not appear
/// in the generated IDL.  Registry (`version = "..."`) and git dependencies
/// are also excluded so that only project-local crates are scanned.
///
/// **Transitive path-dependencies** are resolved: if a discovered dependency
/// itself declares path-based dependencies, those are included as well (with
/// cycle detection).
///
/// In workspace projects the function detects when the nearest `Cargo.toml` is
/// a workspace root manifest and searches for the actual crate manifest
/// containing `[dependencies]`.
pub fn find_path_dep_dirs(source_path: &Path) -> PathDepResult {
    let mut warnings = Vec::new();
    let dirs = spel_framework_core::idl_gen::find_path_dep_dirs(source_path, |w| {
        warnings.push(w);
    });
    PathDepResult { dirs, warnings }
}

/// Scan `<root>/methods/guest/src/bin/*.rs`.  Returns an empty vec — not an
/// error — when the directory doesn't exist.
pub fn search_methods_dir(root: &Path) -> Result<Vec<PathBuf>, String> {
    let bin_dir = root.join("methods").join("guest").join("src").join("bin");
    if !bin_dir.exists() {
        return Ok(vec![]);
    }
    let entries = fs::read_dir(&bin_dir)
        .map_err(|e| format!("Cannot read {}: {}", bin_dir.display(), e))?;
    let mut sources: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map_or(false, |e| e == "rs"))
        .collect();
    sources.sort();
    Ok(sources)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Self-cleaning temporary directory.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("spel-idl-test-{}-{}", label, n));
            fs::create_dir_all(&path).unwrap();
            TempDir(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn write(&self, rel: &str, content: &str) -> PathBuf {
            let p = self.0.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, content).unwrap();
            p
        }

        /// Write a minimal valid SPEL program to `methods/guest/src/bin/<name>.rs`.
        fn write_program(&self, name: &str) -> PathBuf {
            self.write(
                &format!("methods/guest/src/bin/{}.rs", name),
                &format!(
                    "#[lez_program]\npub mod {name} {{\n  \
                     #[instruction]\n  \
                     pub fn init(acc: AccountWithMetadata) {{}}\n}}\n"
                ),
            )
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    // ── search_methods_dir ──────────────────────────────────────────────────

    #[test]
    fn methods_dir_absent_returns_empty() {
        let tmp = TempDir::new("absent");
        let sources = search_methods_dir(tmp.path()).unwrap();
        assert!(sources.is_empty());
    }

    #[test]
    fn methods_dir_single_program() {
        let tmp = TempDir::new("single");
        let expected = tmp.write_program("my_prog");
        let sources = search_methods_dir(tmp.path()).unwrap();
        assert_eq!(sources, vec![expected]);
    }

    #[test]
    fn methods_dir_multiple_programs_sorted() {
        let tmp = TempDir::new("multi");
        tmp.write_program("beta");
        tmp.write_program("alpha");
        let sources = search_methods_dir(tmp.path()).unwrap();
        assert_eq!(sources.len(), 2);
        assert!(sources[0].ends_with("alpha.rs"));
        assert!(sources[1].ends_with("beta.rs"));
    }

    #[test]
    fn methods_dir_ignores_non_rs_files() {
        let tmp = TempDir::new("non-rs");
        tmp.write_program("prog");
        tmp.write("methods/guest/src/bin/README.md", "# readme");
        tmp.write("methods/guest/src/bin/data.bin", "binary");
        let sources = search_methods_dir(tmp.path()).unwrap();
        assert_eq!(sources.len(), 1);
        assert!(sources[0].ends_with("prog.rs"));
    }

    // ── discover_sources — explicit .rs file ───────────────────────────────

    #[test]
    fn explicit_rs_file_accepted() {
        let tmp = TempDir::new("explicit-ok");
        let file = tmp.write("program.rs", "fn main() {}");
        let sources = discover_sources(Some(file.to_str().unwrap())).unwrap();
        assert_eq!(sources, vec![file]);
    }

    #[test]
    fn explicit_rs_file_missing_errors() {
        let tmp = TempDir::new("explicit-missing");
        let missing = tmp.path().join("does_not_exist.rs");
        let err = discover_sources(Some(missing.to_str().unwrap())).unwrap_err();
        assert!(err.contains("not found"), "unexpected error: {err}");
    }

    // ── discover_sources — directory argument ──────────────────────────────

    #[test]
    fn directory_with_methods_finds_program() {
        let tmp = TempDir::new("dir-ok");
        let expected = tmp.write_program("vault");
        let sources = discover_sources(Some(tmp.path().to_str().unwrap())).unwrap();
        assert_eq!(sources, vec![expected]);
    }

    #[test]
    fn directory_with_multiple_programs() {
        let tmp = TempDir::new("dir-multi");
        tmp.write_program("alpha");
        tmp.write_program("beta");
        let sources = discover_sources(Some(tmp.path().to_str().unwrap())).unwrap();
        assert_eq!(sources.len(), 2);
    }

    #[test]
    fn directory_without_methods_errors() {
        let tmp = TempDir::new("dir-empty");
        let err = discover_sources(Some(tmp.path().to_str().unwrap())).unwrap_err();
        assert!(err.contains("No .rs files found"), "unexpected error: {err}");
    }

    #[test]
    fn non_rs_non_dir_path_errors() {
        let tmp = TempDir::new("invalid");
        let file = tmp.write("archive.tar", "data");
        let err = discover_sources(Some(file.to_str().unwrap())).unwrap_err();
        assert!(
            err.contains("not a .rs file or a directory"),
            "unexpected error: {err}"
        );
    }

    // ── end-to-end round-trips ─────────────────────────────────────────────

    #[test]
    fn explicit_file_round_trip() {
        use spel_framework_core::idl_gen::generate_idl_from_file;

        let tmp = TempDir::new("roundtrip-file");
        let file = tmp.write(
            "token.rs",
            r#"
            #[lez_program]
            pub mod token {
                #[instruction]
                pub fn transfer(
                    #[account(signer)] sender: AccountWithMetadata,
                    recipient: AccountWithMetadata,
                    amount: u64,
                ) -> SpelResult { todo!() }
            }
            "#,
        );

        let sources = discover_sources(Some(file.to_str().unwrap())).unwrap();
        assert_eq!(sources.len(), 1);

        let idl = generate_idl_from_file(&sources[0]).unwrap();
        assert_eq!(idl.name, "token");
        assert_eq!(idl.instructions.len(), 1);
        assert_eq!(idl.instructions[0].name, "transfer");
        assert_eq!(idl.instructions[0].accounts.len(), 2);
        assert!(idl.instructions[0].accounts[0].signer);
        assert_eq!(idl.instructions[0].args.len(), 1);
        assert_eq!(idl.instructions[0].args[0].name, "amount");
    }

    #[test]
    fn directory_discovery_round_trip() {
        use spel_framework_core::idl_gen::generate_idl_from_file;

        let tmp = TempDir::new("roundtrip-dir");
        tmp.write(
            "methods/guest/src/bin/counter.rs",
            r#"
            #[lez_program]
            pub mod counter {
                #[instruction]
                pub fn increment(
                    #[account(mut, pda = literal("count"))]
                    state: AccountWithMetadata,
                    #[account(signer)]
                    owner: AccountWithMetadata,
                ) -> SpelResult { todo!() }
            }
            "#,
        );

        let sources = discover_sources(Some(tmp.path().to_str().unwrap())).unwrap();
        assert_eq!(sources.len(), 1);

        let idl = generate_idl_from_file(&sources[0]).unwrap();
        assert_eq!(idl.name, "counter");
        assert!(idl.instructions[0].accounts[0].writable);
        assert!(idl.instructions[0].accounts[0].pda.is_some());
        assert!(idl.instructions[0].accounts[1].signer);
    }

    // ── find_path_dep_dirs ─────────────────────────────────────────────────

    #[test]
    fn find_path_dep_dirs_returns_local_path_deps() {
        let tmp = TempDir::new("find-path-deps");

        tmp.write("core/Cargo.toml", "[package]\nname = \"token_core\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
        tmp.write("core/src/lib.rs", "");

        tmp.write(
            "methods/guest/Cargo.toml",
            "[package]\nname = \"token-guest\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\ntoken_core = { path = \"../../core\" }\n",
        );
        let program = tmp.write("methods/guest/src/bin/token.rs", "");

        let result = find_path_dep_dirs(&program);
        assert!(result.warnings.is_empty(), "unexpected warnings: {:?}", result.warnings);
        assert_eq!(result.dirs.len(), 1);
        assert!(result.dirs[0].ends_with("core"), "expected core dir, got {:?}", result.dirs[0]);
    }

    #[test]
    fn find_path_dep_dirs_ignores_registry_and_git_deps() {
        let tmp = TempDir::new("find-path-deps-filter");

        tmp.write("core/Cargo.toml", "[package]\nname = \"token_core\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
        tmp.write("core/src/lib.rs", "");

        tmp.write(
            "methods/guest/Cargo.toml",
            "[package]\nname = \"token-guest\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\n\
             token_core = { path = \"../../core\" }\n\
             serde = { version = \"1.0\" }\n\
             nssa_core = { git = \"https://example.com/repo.git\", tag = \"v1.0\" }\n",
        );
        let program = tmp.write("methods/guest/src/bin/token.rs", "");

        let result = find_path_dep_dirs(&program);
        assert!(result.warnings.is_empty(), "unexpected warnings: {:?}", result.warnings);
        // Only the path dep (core) should be returned, not serde or nssa_core
        assert_eq!(result.dirs.len(), 1);
        assert!(result.dirs[0].ends_with("core"));
    }

    #[test]
    fn find_path_dep_dirs_ignores_dev_and_build_deps() {
        let tmp = TempDir::new("find-path-deps-dev-build");

        tmp.write("core/Cargo.toml", "[package]\nname = \"token_core\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
        tmp.write("core/src/lib.rs", "");
        tmp.write("test_helpers/Cargo.toml", "[package]\nname = \"test_helpers\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
        tmp.write("test_helpers/src/lib.rs", "");
        tmp.write("build_support/Cargo.toml", "[package]\nname = \"build_support\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
        tmp.write("build_support/src/lib.rs", "");

        tmp.write(
            "methods/guest/Cargo.toml",
            "[package]\nname = \"token-guest\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\n\
             token_core = { path = \"../../core\" }\n\n\
             [dev-dependencies]\n\
             test_helpers = { path = \"../../test_helpers\" }\n\n\
             [build-dependencies]\n\
             build_support = { path = \"../../build_support\" }\n",
        );
        let program = tmp.write("methods/guest/src/bin/token.rs", "");

        let result = find_path_dep_dirs(&program);
        assert!(result.warnings.is_empty(), "unexpected warnings: {:?}", result.warnings);
        // Only runtime path dep (core) should be returned
        assert_eq!(result.dirs.len(), 1, "expected only core, got: {:?}", result.dirs);
        assert!(result.dirs[0].ends_with("core"));
    }

    // ── workspace detection ────────────────────────────────────────────────

    /// Standard case: program source is inside a member crate that has its own
    /// Cargo.toml.  `find_crate_manifest` finds the member manifest (not the
    /// workspace root), so the normal path-dependency resolution applies.
    #[test]
    fn find_path_dep_dirs_resolves_workspace_member_manifest() {
        let tmp = TempDir::new("workspace-member");

        // Workspace root manifest (no [package], no [dependencies])
        tmp.write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"core\", \"methods/guest\"]\n",
        );

        tmp.write("core/Cargo.toml", "[package]\nname = \"token_core\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
        tmp.write("core/src/lib.rs", "");

        tmp.write(
            "methods/guest/Cargo.toml",
            "[package]\nname = \"token-guest\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\ntoken_core = { path = \"../../core\" }\n",
        );
        let program = tmp.write("methods/guest/src/bin/token.rs", "");

        let result = find_path_dep_dirs(&program);
        assert!(result.warnings.is_empty(), "unexpected warnings: {:?}", result.warnings);
        assert_eq!(result.dirs.len(), 1);
        assert!(result.dirs[0].ends_with("core"), "expected core dir, got {:?}", result.dirs);
    }

    /// Workspace with glob patterns in members: the glob is expanded and the
    /// correct member manifest is found.
    #[test]
    fn find_path_dep_dirs_resolves_workspace_with_glob_members() {
        let tmp = TempDir::new("workspace-glob");

        // Workspace root with glob pattern in members.
        tmp.write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/*\", \"methods/guest\"]\n",
        );

        tmp.write("crates/core/Cargo.toml", "[package]\nname = \"token_core\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
        tmp.write("crates/core/src/lib.rs", "");

        // Path is relative to methods/guest/, so needs ../.. to reach workspace root.
        tmp.write(
            "methods/guest/Cargo.toml",
            "[package]\nname = \"token-guest\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\ntoken_core = { path = \"../../crates/core\" }\n",
        );
        let program = tmp.write("methods/guest/src/bin/token.rs", "");

        let result = find_path_dep_dirs(&program);
        assert!(result.warnings.is_empty(), "unexpected warnings: {:?}", result.warnings);
        assert_eq!(result.dirs.len(), 1);
        assert!(result.dirs[0].ends_with("crates/core"), "expected crates/core dir, got {:?}", result.dirs);
    }

    /// When the program source has no intermediate Cargo.toml (only a workspace
    /// root exists above it), the workspace member search kicks in.  This test
    /// places the source in a directory with NO crate manifest between it and
    /// the workspace root, forcing the full workspace resolution path.
    #[test]
    fn find_path_dep_dirs_fallback_search_in_workspace() {
        let tmp = TempDir::new("workspace-fallback");

        // Workspace root — no [package] section, so find_crate_manifest returns this.
        tmp.write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"core\", \"programs/guest\"]\n",
        );

        tmp.write("core/Cargo.toml", "[package]\nname = \"token_core\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
        tmp.write("core/src/lib.rs", "");

        // The guest crate has its own Cargo.toml with dependencies.
        tmp.write(
            "programs/guest/Cargo.toml",
            "[package]\nname = \"token-guest\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\ntoken_core = { path = \"../../core\" }\n",
        );
        // Source is inside the guest member crate, deeply nested.
        // find_crate_manifest finds programs/guest/Cargo.toml first (has [package]),
        // so normal resolution applies — NOT the workspace fallback.
        let program = tmp.write("programs/guest/src/bin/token.rs", "");

        let result = find_path_dep_dirs(&program);
        assert!(result.warnings.is_empty(), "unexpected warnings: {:?}", result.warnings);
        assert_eq!(result.dirs.len(), 1);
        assert!(result.dirs[0].ends_with("core"), "expected core dir, got {:?}", result.dirs);
    }

    /// Test that exercises the workspace-root resolution path: the nearest
    /// Cargo.toml is truly a virtual workspace root (no intermediate crate
    /// manifest).  This validates `find_member_manifest` and recursive search.
    #[test]
    fn find_path_dep_dirs_virtual_workspace_root() {
        let tmp = TempDir::new("virtual-workspace");

        // Virtual workspace root — no [package], just [workspace].
        tmp.write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"libs/*\", \"programs/*\"]\n",
        );

        tmp.write("libs/common/Cargo.toml", "[package]\nname = \"common\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
        tmp.write("libs/common/src/lib.rs", "");

        // The program crate is NOT a workspace member — it lives in a dir with
        // no Cargo.toml, so find_crate_manifest walks up to the workspace root.
        // The workspace resolution then finds libs/common via recursive search.
        tmp.write(
            "programs/myprog/Cargo.toml",
            "[package]\nname = \"myprog\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\ncommon = { path = \"../../libs/common\" }\n",
        );
        // Source file is in a deeply nested dir with no intermediate Cargo.toml.
        let program = tmp.write("programs/myprog/src/deep/nested/token.rs", "");

        let result = find_path_dep_dirs(&program);
        assert!(result.warnings.is_empty(), "unexpected warnings: {:?}", result.warnings);
        assert_eq!(result.dirs.len(), 1);
        assert!(
            result.dirs[0].ends_with("libs/common"),
            "expected libs/common dir, got {:?}",
            result.dirs
        );
    }

    // ── transitive dependencies ────────────────────────────────────────────

    #[test]
    fn find_path_dep_dirs_resolves_transitive_deps() {
        let tmp = TempDir::new("transitive-deps");

        // shared_types is a dependency of core, which is a dependency of guest
        tmp.write("shared_types/Cargo.toml", "[package]\nname = \"shared_types\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
        tmp.write("shared_types/src/lib.rs", "");

        tmp.write(
            "core/Cargo.toml",
            "[package]\nname = \"token_core\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\nshared_types = { path = \"../shared_types\" }\n",
        );
        tmp.write("core/src/lib.rs", "");

        tmp.write(
            "methods/guest/Cargo.toml",
            "[package]\nname = \"token-guest\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\ntoken_core = { path = \"../../core\" }\n",
        );
        let program = tmp.write("methods/guest/src/bin/token.rs", "");

        let result = find_path_dep_dirs(&program);
        assert!(result.warnings.is_empty(), "unexpected warnings: {:?}", result.warnings);
        // Should include both direct dep (core) and transitive dep (shared_types)
        assert_eq!(result.dirs.len(), 2, "expected 2 dirs, got {:?}", result.dirs);
        let names: Vec<&str> = result.dirs.iter().map(|d| d.file_name().unwrap().to_str().unwrap()).collect();
        assert!(names.contains(&"core"));
        assert!(names.contains(&"shared_types"));
    }

    // ── warnings on errors ─────────────────────────────────────────────────

    #[test]
    fn find_path_dep_dirs_warns_on_missing_dep_directory() {
        let tmp = TempDir::new("missing-dep-dir");

        tmp.write(
            "methods/guest/Cargo.toml",
            "[package]\nname = \"token-guest\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\ntoken_core = { path = \"../../nonexistent\" }\n",
        );
        let program = tmp.write("methods/guest/src/bin/token.rs", "");

        let result = find_path_dep_dirs(&program);
        assert!(result.dirs.is_empty());
        assert!(!result.warnings.is_empty(), "expected warning for missing dep dir");
        assert!(result.warnings[0].contains("non-existent"), "unexpected warning: {}", result.warnings[0]);
    }

    #[test]
    fn find_path_dep_dirs_warns_on_invalid_toml() {
        let tmp = TempDir::new("invalid-toml");

        tmp.write(
            "methods/guest/Cargo.toml",
            "this is not valid [[ toml !!!\n",
        );
        let program = tmp.write("methods/guest/src/bin/token.rs", "");

        let result = find_path_dep_dirs(&program);
        assert!(result.dirs.is_empty());
        assert!(!result.warnings.is_empty(), "expected warning for invalid TOML");
    }

    // ── account types from path-dep crates ────────────────────────────────

    /// Mirrors the real token program structure:
    ///   core/src/lib.rs          — TokenDefinition, TokenHolding, TokenMetadata (#[account_type])
    ///   methods/guest/Cargo.toml — depends on core via path = "../../core"
    ///   methods/guest/src/bin/token.rs — #[lez_program], no #[account_type] here
    #[test]
    fn account_types_from_path_dep_lib_appear_in_idl() {
        use spel_framework_core::idl_gen::generate_idl_from_file_with_deps;

        let tmp = TempDir::new("path-dep-lib");

        tmp.write(
            "core/Cargo.toml",
            "[package]\nname = \"token_core\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        tmp.write(
            "core/src/lib.rs",
            r#"
use nssa_core::account::AccountId;

#[account_type]
pub enum TokenDefinition {
    Fungible {
        name: String,
        total_supply: u128,
        metadata_id: Option<AccountId>,
    },
    NonFungible {
        name: String,
        printable_supply: u128,
        metadata_id: AccountId,
    },
}

#[account_type]
pub enum TokenHolding {
    Fungible {
        definition_id: AccountId,
        balance: u128,
    },
    NftMaster {
        definition_id: AccountId,
        print_balance: u128,
    },
    NftPrintedCopy {
        definition_id: AccountId,
        owned: bool,
    },
}

#[account_type]
pub struct TokenMetadata {
    pub definition_id: AccountId,
    pub uri: String,
    pub primary_sale_date: u64,
}
"#,
        );

        tmp.write(
            "methods/guest/Cargo.toml",
            "[package]\nname = \"token-guest\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\ntoken_core = { path = \"../../core\" }\n",
        );
        let program = tmp.write(
            "methods/guest/src/bin/token.rs",
            r#"
#[lez_program(instruction = "token_core::Instruction")]
pub mod token {
    use super::*;

    #[instruction]
    pub fn transfer(
        sender: AccountWithMetadata,
        recipient: AccountWithMetadata,
        amount_to_transfer: u128,
    ) -> SpelResult { todo!() }

    #[instruction]
    pub fn initialize_account(
        definition_account: AccountWithMetadata,
        account_to_initialize: AccountWithMetadata,
    ) -> SpelResult { todo!() }

    #[instruction]
    pub fn mint(
        definition_account: AccountWithMetadata,
        user_holding_account: AccountWithMetadata,
        amount_to_mint: u128,
    ) -> SpelResult { todo!() }

    #[instruction]
    pub fn burn(
        definition_account: AccountWithMetadata,
        user_holding_account: AccountWithMetadata,
        amount_to_burn: u128,
    ) -> SpelResult { todo!() }
}
"#,
        );

        let dep_result = find_path_dep_dirs(&program);
        assert!(dep_result.warnings.is_empty(), "unexpected warnings: {:?}", dep_result.warnings);
        assert_eq!(dep_result.dirs.len(), 1);

        let idl = generate_idl_from_file_with_deps(&program, &dep_result.dirs).unwrap();

        assert_eq!(idl.name, "token");
        assert_eq!(idl.instructions.len(), 4);

        let account_names: Vec<&str> = idl.accounts.iter().map(|a| a.name.as_str()).collect();
        assert!(
            account_names.contains(&"TokenDefinition"),
            "TokenDefinition missing; got {:?}",
            account_names
        );
        assert!(
            account_names.contains(&"TokenHolding"),
            "TokenHolding missing; got {:?}",
            account_names
        );
        assert!(
            account_names.contains(&"TokenMetadata"),
            "TokenMetadata missing; got {:?}",
            account_names
        );
    }

    /// Account types split across sub-modules of a path-dep crate are also found.
    /// lib.rs declares `pub mod types;` and the types live in types.rs.
    #[test]
    fn account_types_in_submodule_of_path_dep_appear_in_idl() {
        use spel_framework_core::idl_gen::generate_idl_from_file_with_deps;

        let tmp = TempDir::new("path-dep-submodule");

        tmp.write(
            "core/Cargo.toml",
            "[package]\nname = \"token_core\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        // lib.rs re-exports from a sub-module; account types live in types.rs
        tmp.write("core/src/lib.rs", "pub mod types;\n");
        tmp.write(
            "core/src/types.rs",
            r#"
use nssa_core::account::AccountId;

#[account_type]
pub enum TokenHolding {
    Fungible {
        definition_id: AccountId,
        balance: u128,
    },
}

#[account_type]
pub struct TokenMetadata {
    pub uri: String,
}
"#,
        );

        tmp.write(
            "methods/guest/Cargo.toml",
            "[package]\nname = \"token-guest\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\ntoken_core = { path = \"../../core\" }\n",
        );
        let program = tmp.write(
            "methods/guest/src/bin/token.rs",
            r#"
#[lez_program]
pub mod token {
    #[instruction]
    pub fn transfer(
        sender: AccountWithMetadata,
        recipient: AccountWithMetadata,
        amount: u128,
    ) -> SpelResult { todo!() }
}
"#,
        );

        let dep_result = find_path_dep_dirs(&program);
        assert!(dep_result.warnings.is_empty(), "unexpected warnings: {:?}", dep_result.warnings);
        let idl = generate_idl_from_file_with_deps(&program, &dep_result.dirs).unwrap();

        let account_names: Vec<&str> = idl.accounts.iter().map(|a| a.name.as_str()).collect();
        assert!(
            account_names.contains(&"TokenHolding"),
            "TokenHolding in sub-module not found; got {:?}",
            account_names
        );
        assert!(
            account_names.contains(&"TokenMetadata"),
            "TokenMetadata in sub-module not found; got {:?}",
            account_names
        );
    }

    /// Account type inside a file-backed module that is itself declared inside an
    /// inline module: `mod outer { mod inner; }` in lib.rs, where `inner` resolves
    /// to `outer/inner.rs`.  Verifies that base_dir is advanced when recursing into
    /// inline module bodies.
    #[test]
    fn account_type_in_nested_file_module_appears_in_idl() {
        use spel_framework_core::idl_gen::generate_idl_from_file_with_deps;

        let tmp = TempDir::new("nested-file-mod");

        tmp.write(
            "core/Cargo.toml",
            "[package]\nname = \"token_core\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        // lib.rs has an inline `mod outer` whose body declares an external `mod inner`
        tmp.write("core/src/lib.rs", "pub mod outer {\n    pub mod inner;\n}\n");
        // inner.rs lives at core/src/outer/inner.rs
        tmp.write(
            "core/src/outer/inner.rs",
            "#[account_type]\npub struct VaultAccount { pub balance: u128 }\n",
        );

        tmp.write(
            "methods/guest/Cargo.toml",
            "[package]\nname = \"token-guest\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\ntoken_core = { path = \"../../core\" }\n",
        );
        let program = tmp.write(
            "methods/guest/src/bin/token.rs",
            "#[lez_program]\npub mod token {\n  \
             #[instruction]\n  \
             pub fn deposit(acc: AccountWithMetadata) -> SpelResult { todo!() }\n}\n",
        );

        let dep_result = find_path_dep_dirs(&program);
        assert!(dep_result.warnings.is_empty(), "unexpected warnings: {:?}", dep_result.warnings);
        let idl = generate_idl_from_file_with_deps(&program, &dep_result.dirs).unwrap();

        let account_names: Vec<&str> = idl.accounts.iter().map(|a| a.name.as_str()).collect();
        assert!(
            account_names.contains(&"VaultAccount"),
            "VaultAccount in outer/inner.rs not found; got {:?}",
            account_names
        );
    }

    /// A `mod` declaration with a `#[path = "..."]` override is resolved to the
    /// explicitly given file rather than the default `<mod>.rs` / `<mod>/mod.rs`.
    #[test]
    fn account_type_behind_path_attribute_appears_in_idl() {
        use spel_framework_core::idl_gen::generate_idl_from_file_with_deps;

        let tmp = TempDir::new("path-attr-mod");

        tmp.write(
            "core/Cargo.toml",
            "[package]\nname = \"token_core\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        // lib.rs uses #[path] to point at a non-standard file name.
        tmp.write(
            "core/src/lib.rs",
            "#[path = \"account_types.rs\"]\npub mod types;\n",
        );
        tmp.write(
            "core/src/account_types.rs",
            "#[account_type]\npub struct StakeAccount { pub amount: u64 }\n",
        );

        tmp.write(
            "methods/guest/Cargo.toml",
            "[package]\nname = \"token-guest\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\ntoken_core = { path = \"../../core\" }\n",
        );
        let program = tmp.write(
            "methods/guest/src/bin/token.rs",
            "#[lez_program]\npub mod token {\n  \
             #[instruction]\n  \
             pub fn stake(acc: AccountWithMetadata) -> SpelResult { todo!() }\n}\n",
        );

        let dep_result = find_path_dep_dirs(&program);
        assert!(dep_result.warnings.is_empty(), "unexpected warnings: {:?}", dep_result.warnings);
        let idl = generate_idl_from_file_with_deps(&program, &dep_result.dirs).unwrap();

        let account_names: Vec<&str> = idl.accounts.iter().map(|a| a.name.as_str()).collect();
        assert!(
            account_names.contains(&"StakeAccount"),
            "StakeAccount behind #[path] not found; got {:?}",
            account_names
        );
    }

    /// Without dep dirs, #[account_type] types from external crates are absent.
    /// This is the regression guard — the old behaviour that the fix addresses.
    #[test]
    fn without_dep_dirs_external_account_types_are_missing() {
        use spel_framework_core::idl_gen::generate_idl_from_file_with_deps;

        let tmp = TempDir::new("path-dep-no-scan");

        tmp.write(
            "core/Cargo.toml",
            "[package]\nname = \"token_core\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        tmp.write(
            "core/src/lib.rs",
            "#[account_type]\npub struct TokenHolding { pub balance: u128 }\n",
        );

        tmp.write(
            "methods/guest/Cargo.toml",
            "[package]\nname = \"token-guest\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\ntoken_core = { path = \"../../core\" }\n",
        );
        let program = tmp.write(
            "methods/guest/src/bin/token.rs",
            "#[lez_program]\npub mod token {\n  \
             #[instruction]\n  \
             pub fn transfer(acc: AccountWithMetadata) -> SpelResult { todo!() }\n}\n",
        );

        // Passing no dep dirs replicates the old single-file behaviour
        let idl = generate_idl_from_file_with_deps(&program, &[]).unwrap();
        assert!(
            idl.accounts.is_empty(),
            "expected no accounts without dep scanning, got {:?}",
            idl.accounts.iter().map(|a| &a.name).collect::<Vec<_>>()
        );
    }

    // ── #[cfg(test)] modules are skipped ────────────────────────────────────

    /// Account types inside a #[cfg(test)] module in a dependency crate should
    /// NOT appear in the generated IDL — they are test-only and won't be compiled
    /// into the on-chain program.
    #[test]
    fn cfg_test_module_excluded_from_idl() {
        use spel_framework_core::idl_gen::generate_idl_from_file_with_deps;

        let tmp = TempDir::new("cfg-test-exclude");

        tmp.write(
            "core/Cargo.toml",
            "[package]\nname = \"token_core\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        // lib.rs has a regular account type and a test-only one.
        tmp.write(
            "core/src/lib.rs",
            r#"
#[account_type]
pub struct RealAccount { pub balance: u128 }

#[cfg(test)]
pub mod test_helpers {
    #[account_type]
    pub struct TestOnlyAccount { pub fake_balance: u64 }
}
"#,
        );

        tmp.write(
            "methods/guest/Cargo.toml",
            "[package]\nname = \"token-guest\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\ntoken_core = { path = \"../../core\" }\n",
        );
        let program = tmp.write(
            "methods/guest/src/bin/token.rs",
            "#[lez_program]\npub mod token {\n  \
             #[instruction]\n  \
             pub fn transfer(acc: AccountWithMetadata) -> SpelResult { todo!() }\n}\n",
        );

        let dep_result = find_path_dep_dirs(&program);
        assert!(dep_result.warnings.is_empty(), "unexpected warnings: {:?}", dep_result.warnings);
        let idl = generate_idl_from_file_with_deps(&program, &dep_result.dirs).unwrap();

        let account_names: Vec<&str> = idl.accounts.iter().map(|a| a.name.as_str()).collect();
        assert!(
            account_names.contains(&"RealAccount"),
            "RealAccount should be present; got {:?}",
            account_names
        );
        assert!(
            !account_names.contains(&"TestOnlyAccount"),
            "TestOnlyAccount in #[cfg(test)] module should be excluded; got {:?}",
            account_names
        );
    }

    /// Account types behind #[cfg(feature = "...")] are also excluded since we
    /// don't know which features are enabled for the on-chain build.
    #[test]
    fn cfg_feature_module_excluded_from_idl() {
        use spel_framework_core::idl_gen::generate_idl_from_file_with_deps;

        let tmp = TempDir::new("cfg-feature-exclude");

        tmp.write(
            "core/Cargo.toml",
            "[package]\nname = \"token_core\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        tmp.write(
            "core/src/lib.rs",
            r#"
#[account_type]
pub struct RealAccount { pub balance: u128 }

#[cfg(feature = "experimental")]
pub mod experimental {
    #[account_type]
    pub struct ExperimentalAccount { pub secret: String }
}
"#,
        );

        tmp.write(
            "methods/guest/Cargo.toml",
            "[package]\nname = \"token-guest\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\ntoken_core = { path = \"../../core\" }\n",
        );
        let program = tmp.write(
            "methods/guest/src/bin/token.rs",
            "#[lez_program]\npub mod token {\n  \
             #[instruction]\n  \
             pub fn transfer(acc: AccountWithMetadata) -> SpelResult { todo!() }\n}\n",
        );

        let dep_result = find_path_dep_dirs(&program);
        let idl = generate_idl_from_file_with_deps(&program, &dep_result.dirs).unwrap();

        let account_names: Vec<&str> = idl.accounts.iter().map(|a| a.name.as_str()).collect();
        assert!(
            account_names.contains(&"RealAccount"),
            "RealAccount should be present; got {:?}",
            account_names
        );
        assert!(
            !account_names.contains(&"ExperimentalAccount"),
            "ExperimentalAccount in #[cfg(feature)] module should be excluded; got {:?}",
            account_names
        );
    }

    /// `#[cfg(any(test, ...))]` wrappers are also detected — if any alternative
    /// references `test` or `feature`, the item is excluded.
    #[test]
    fn cfg_any_test_excluded_from_idl() {
        use spel_framework_core::idl_gen::generate_idl_from_file_with_deps;

        let tmp = TempDir::new("cfg-any-test");

        tmp.write(
            "core/Cargo.toml",
            "[package]\nname = \"token_core\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        tmp.write(
            "core/src/lib.rs",
            r#"
#[account_type]
pub struct RealAccount { pub balance: u128 }

#[cfg(any(test, feature = "debug-tools"))]
pub mod debug {
    #[account_type]
    pub struct DebugAccount { pub trace_id: String }
}
"#,
        );

        tmp.write(
            "methods/guest/Cargo.toml",
            "[package]\nname = \"token-guest\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\ntoken_core = { path = \"../../core\" }\n",
        );
        let program = tmp.write(
            "methods/guest/src/bin/token.rs",
            "#[lez_program]\npub mod token {\n  \
             #[instruction]\n  \
             pub fn transfer(acc: AccountWithMetadata) -> SpelResult { todo!() }\n}\n",
        );

        let dep_result = find_path_dep_dirs(&program);
        let idl = generate_idl_from_file_with_deps(&program, &dep_result.dirs).unwrap();

        let account_names: Vec<&str> = idl.accounts.iter().map(|a| a.name.as_str()).collect();
        assert!(
            account_names.contains(&"RealAccount"),
            "RealAccount should be present; got {:?}",
            account_names
        );
        assert!(
            !account_names.contains(&"DebugAccount"),
            "DebugAccount in #[cfg(any(test, ...))] module should be excluded; got {:?}",
            account_names
        );
    }

    /// Top-level (non-module) items with #[cfg(test)] are also filtered.
    /// e.g. `#[cfg(test)] #[account_type] struct TestAccount {}`
    #[test]
    fn cfg_test_top_level_struct_excluded_from_idl() {
        use spel_framework_core::idl_gen::generate_idl_from_file_with_deps;

        let tmp = TempDir::new("cfg-test-top-level");

        tmp.write(
            "core/Cargo.toml",
            "[package]\nname = \"token_core\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        tmp.write(
            "core/src/lib.rs",
            r#"
#[account_type]
pub struct RealAccount { pub balance: u128 }

#[cfg(test)]
#[account_type]
pub struct TestOnlyStruct { pub fake: u64 }
"#,
        );

        tmp.write(
            "methods/guest/Cargo.toml",
            "[package]\nname = \"token-guest\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\ntoken_core = { path = \"../../core\" }\n",
        );
        let program = tmp.write(
            "methods/guest/src/bin/token.rs",
            "#[lez_program]\npub mod token {\n  \
             #[instruction]\n  \
             pub fn transfer(acc: AccountWithMetadata) -> SpelResult { todo!() }\n}\n",
        );

        let dep_result = find_path_dep_dirs(&program);
        let idl = generate_idl_from_file_with_deps(&program, &dep_result.dirs).unwrap();

        let account_names: Vec<&str> = idl.accounts.iter().map(|a| a.name.as_str()).collect();
        assert!(
            account_names.contains(&"RealAccount"),
            "RealAccount should be present; got {:?}",
            account_names
        );
        assert!(
            !account_names.contains(&"TestOnlyStruct"),
            "Top-level #[cfg(test)] struct should be excluded; got {:?}",
            account_names
        );
    }

    /// `#[cfg(not(test))]` items are production-only and should NOT be excluded.
    /// This verifies the token scanner skips contents of `not(...)` groups.
    #[test]
    fn cfg_not_test_included_in_idl() {
        use spel_framework_core::idl_gen::generate_idl_from_file_with_deps;

        let tmp = TempDir::new("cfg-not-test");

        tmp.write(
            "core/Cargo.toml",
            "[package]\nname = \"token_core\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        tmp.write(
            "core/src/lib.rs",
            r#"
#[account_type]
pub struct RealAccount { pub balance: u128 }

#[cfg(not(test))]
#[account_type]
pub struct ProdOnlyStruct { pub secret: u64 }
"#,
        );

        tmp.write(
            "methods/guest/Cargo.toml",
            "[package]\nname = \"token-guest\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\ntoken_core = { path = \"../../core\" }\n",
        );
        let program = tmp.write(
            "methods/guest/src/bin/token.rs",
            "#[lez_program]\npub mod token {\n  \
             #[instruction]\n  \
             pub fn transfer(acc: AccountWithMetadata) -> SpelResult { todo!() }\n}\n",
        );

        let dep_result = find_path_dep_dirs(&program);
        let idl = generate_idl_from_file_with_deps(&program, &dep_result.dirs).unwrap();

        let account_names: Vec<&str> = idl.accounts.iter().map(|a| a.name.as_str()).collect();
        assert!(
            account_names.contains(&"RealAccount"),
            "RealAccount should be present; got {:?}",
            account_names
        );
        assert!(
            account_names.contains(&"ProdOnlyStruct"),
            "#[cfg(not(test))] struct should be INCLUDED (production-only); got {:?}",
            account_names
        );
    }

    // ── transitive deps with account types ──────────────────────────────────

    /// Account types from transitive path dependencies (A → B → C) are all
    /// collected and appear in the generated IDL.
    #[test]
    fn account_types_from_transitive_deps_appear_in_idl() {
        use spel_framework_core::idl_gen::generate_idl_from_file_with_deps;

        let tmp = TempDir::new("transitive-account-types");

        // shared_types is a transitive dependency (guest → core → shared_types)
        tmp.write(
            "shared_types/Cargo.toml",
            "[package]\nname = \"shared_types\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        tmp.write(
            "shared_types/src/lib.rs",
            r#"
#[account_type]
pub struct SharedAccount { pub data: String }
"#,
        );

        tmp.write(
            "core/Cargo.toml",
            "[package]\nname = \"token_core\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\nshared_types = { path = \"../shared_types\" }\n",
        );
        tmp.write(
            "core/src/lib.rs",
            r#"
#[account_type]
pub struct CoreAccount { pub balance: u128 }
"#,
        );

        tmp.write(
            "methods/guest/Cargo.toml",
            "[package]\nname = \"token-guest\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\ntoken_core = { path = \"../../core\" }\n",
        );
        let program = tmp.write(
            "methods/guest/src/bin/token.rs",
            "#[lez_program]\npub mod token {\n  \
             #[instruction]\n  \
             pub fn transfer(acc: AccountWithMetadata) -> SpelResult { todo!() }\n}\n",
        );

        let dep_result = find_path_dep_dirs(&program);
        // Should include both direct and transitive deps
        assert_eq!(dep_result.dirs.len(), 2, "expected 2 dep dirs, got {:?}", dep_result.dirs);

        let idl = generate_idl_from_file_with_deps(&program, &dep_result.dirs).unwrap();

        let account_names: Vec<&str> = idl.accounts.iter().map(|a| a.name.as_str()).collect();
        assert!(
            account_names.contains(&"CoreAccount"),
            "CoreAccount from direct dep should be present; got {:?}",
            account_names
        );
        assert!(
            account_names.contains(&"SharedAccount"),
            "SharedAccount from transitive dep should be present; got {:?}",
            account_names
        );
    }
}
