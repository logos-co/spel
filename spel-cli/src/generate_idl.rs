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

use toml::Value;

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
pub fn find_path_dep_dirs(source_path: &Path) -> Vec<PathBuf> {
    (|| -> Option<Vec<PathBuf>> {
        let manifest = find_crate_manifest(source_path)?;
        let content = fs::read_to_string(&manifest).ok()?;
        let value: Value = toml::from_str(&content).ok()?;
        let manifest_dir = manifest.parent()?;

        let mut dirs = Vec::new();
        if let Some(table) = value.get("dependencies").and_then(|v| v.as_table()) {
            for (_name, dep) in table {
                if let Some(rel) = dep.get("path").and_then(|v| v.as_str()) {
                    let dep_dir = manifest_dir.join(rel);
                    if dep_dir.is_dir() {
                        dirs.push(dep_dir);
                    }
                }
            }
        }
        Some(dirs)
    })()
    .unwrap_or_default()
}

/// Walk up from `start` to find the nearest `Cargo.toml`.
fn find_crate_manifest(start: &Path) -> Option<PathBuf> {
    let mut dir: &Path = if start.is_file() { start.parent()? } else { start };
    loop {
        let candidate = dir.join("Cargo.toml");
        if candidate.exists() {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
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

        let dirs = find_path_dep_dirs(&program);
        assert_eq!(dirs.len(), 1);
        assert!(dirs[0].ends_with("core"), "expected core dir, got {:?}", dirs[0]);
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

        let dirs = find_path_dep_dirs(&program);
        // Only the path dep (core) should be returned, not serde or nssa_core
        assert_eq!(dirs.len(), 1);
        assert!(dirs[0].ends_with("core"));
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

        let dirs = find_path_dep_dirs(&program);
        // Only runtime path dep (core) should be returned
        assert_eq!(dirs.len(), 1, "expected only core, got: {dirs:?}");
        assert!(dirs[0].ends_with("core"));
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

        let dep_dirs = find_path_dep_dirs(&program);
        assert_eq!(dep_dirs.len(), 1);

        let idl = generate_idl_from_file_with_deps(&program, &dep_dirs).unwrap();

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

        let dep_dirs = find_path_dep_dirs(&program);
        let idl = generate_idl_from_file_with_deps(&program, &dep_dirs).unwrap();

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

        let dep_dirs = find_path_dep_dirs(&program);
        let idl = generate_idl_from_file_with_deps(&program, &dep_dirs).unwrap();

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

        let dep_dirs = find_path_dep_dirs(&program);
        let idl = generate_idl_from_file_with_deps(&program, &dep_dirs).unwrap();

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
}
