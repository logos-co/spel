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
}
