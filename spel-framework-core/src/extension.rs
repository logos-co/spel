//! Path-dep extension discovery for SPEL programs.
//!
//! Scans the consuming program's **direct** path-dependencies for crates
//! that declare `[package.metadata.spel]` in their `Cargo.toml`. Each
//! qualifying crate exposes its `#[instruction]` fns (and optional gate
//! attributes) through this module's discovery API:
//!
//! - [`discover_extension_instructions`] returns the cross-crate
//!   `#[instruction]` fns to be merged into the consumer's dispatcher
//!   and IDL.
//! - [`discover_extension_instruction_attrs`] returns the library-owned
//!   gate attribute names that the framework strips from emitted
//!   handler fns. Attribute macros on items inside a module only expand
//!   after the outer `#[lez_program]` rewrite, so this strip removes the
//!   first (and only possible) expansion: a gate attr on a
//!   consumer-authored instruction contributes no code, no validation,
//!   and no diagnostics. Library gates apply exclusively by re-expanding
//!   on the handlers the library itself emits.
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
//! `read_spel_instruction_attrs`, `collect_instruction_fns`) are
//! module-private; consumers go through the two `discover_*` entry
//! points.

use std::path::{Path, PathBuf};

use syn::Attribute;

use crate::idl_gen::{collect_items_from_crate_dirs, has_instruction_attr};

/// Filter `#[instruction]`-annotated fns from a flat item list.
///
/// Used by framework codegen to pull instruction definitions out of
/// extension libraries (e.g. admin-authority) that ship pre-defined
/// instructions to be merged into a consuming program's IDL + dispatcher.
fn collect_instruction_fns(items: &[syn::Item]) -> Vec<syn::ItemFn> {
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
fn read_spel_extension_attr(crate_dir: &Path) -> Result<Option<String>, String> {
    let manifest = crate_dir.join("Cargo.toml");
    let Ok(content) = std::fs::read_to_string(&manifest) else {
        return Ok(None); // unreadable dep manifest: cargo fails the build anyway
    };
    let Ok(value) = toml::from_str::<toml::Value>(&content) else {
        return Ok(None); // same
    };
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
                manifest.display()
            )),
        },
    }
}

/// Read `[package.metadata.spel.instruction_attrs]` from a crate's Cargo.toml.
/// Returns the list of instruction-level marker attribute names the library
/// declares (e.g. `["require_admin"]`). Framework strips these from emitted
/// handler fns to prevent re-expansion.
fn read_spel_instruction_attrs(crate_dir: &Path) -> Result<Vec<String>, String> {
    let manifest = crate_dir.join("Cargo.toml");
    let Ok(content) = std::fs::read_to_string(&manifest) else {
        return Ok(vec![]);
    };
    let Ok(value) = toml::from_str::<toml::Value>(&content) else {
        return Ok(vec![]);
    };
    let Some(attrs) = value
        .get("package")
        .and_then(|p| p.get("metadata"))
        .and_then(|m| m.get("spel"))
        .and_then(|s| s.get("instruction_attrs"))
    else {
        return Ok(vec![]);
    };
    let Some(arr) = attrs.as_array() else {
        return Err(format!(
            "malformed [package.metadata.spel] in {}: instruction_attrs must be an array of strings",
            manifest.display()
        ));
    };
    arr.iter()
        .map(|x| {
            x.as_str().map(String::from).ok_or_else(|| {
                format!(
                    "malformed [package.metadata.spel] in {}: instruction_attrs must be an array of strings",
                    manifest.display()
                )
            })
        })
        .collect()
}

/// Scan the consumer's direct path-deps for SPEL extension libraries whose
/// `extension_attr` metadata matches an attribute on the consuming
/// program's mod. Returns one `(ItemFn, crate_path)` per discovered
/// `#[instruction]` fn, where `crate_path` is the absolute path to call
/// the fn from the consumer (e.g. `::admin_authority`), derived from the
/// dependency's `[package].name`.
///
/// `Err` on malformed spel metadata (callers surface it as a compile
/// error). Environmental skips are reported via `on_warning`.
pub fn discover_extension_instructions<F: FnMut(String)>(
    manifest_dir: &Path,
    mod_attrs: &[Attribute],
    on_warning: &mut F,
) -> Result<Vec<(syn::ItemFn, syn::Path)>, String> {
    let dep_dirs = direct_path_dep_dirs(&manifest_dir, on_warning);

    let mut out = Vec::new();
    for dep_dir in dep_dirs {
        let Some(ext_attr) = read_spel_extension_attr(&dep_dir)? else {
            continue;
        };
        if !mod_attrs.iter().any(|a| a.path().is_ident(&ext_attr)) {
            continue;
        }

        let (items, _) = collect_items_from_crate_dirs(&[dep_dir.clone()]);
        let Some(crate_name) = read_package_ident(&dep_dir) else {
            on_warning(format!(
                "extension at '{}' matched a module attribute but has no [package].name, skipped",
                dep_dir.display()
            ));
            continue;
        };
        let crate_ident = syn::Ident::new(&crate_name, proc_macro2::Span::call_site());
        let crate_path: syn::Path = syn::parse_quote!(::#crate_ident);

        let funcs = collect_instruction_fns(&items);
        if funcs.is_empty() && read_spel_instruction_attrs(&dep_dir)?.is_empty() {
            on_warning(format!(
                "extension '{crate_name}' matched #[{ext_attr}] but contributes no \
                #[instruction] fns and no instruction_attrs"
            ));
        }
        for func in funcs {
            out.push((func, crate_path.clone()));
        }
    }
    Ok(out)
}

/// Collect all instruction-level marker attribute names declared by the
/// consumer's direct path-dep extensions whose `extension_attr` matches an
/// attribute on the consuming program's mod. Framework strips these from
/// emitted handler fns.
///
/// Note the stripping semantics: attrs on items inside a module expand
/// only after the outer `#[lez_program]` rewrite, so the strip prevents
/// the first and only possible expansion. A gate attr a consumer writes
/// on their own instruction never runs; library gates take effect by
/// re-expansion on library-emitted handlers only.
///
/// `Err` on malformed spel metadata, same contract as
/// [`discover_extension_instructions`].
pub fn discover_extension_instruction_attrs<F: FnMut(String)>(
    manifest_dir: &Path,
    mod_attrs: &[Attribute],
    on_warning: &mut F,
) -> Result<Vec<String>, String> {
    let dep_dirs = direct_path_dep_dirs(manifest_dir, on_warning);
    let mut out = Vec::new();
    for dep_dir in dep_dirs {
        let Some(ext_attr) = read_spel_extension_attr(&dep_dir)? else {
            continue;
        };
        if !mod_attrs.iter().any(|a| a.path().is_ident(&ext_attr)) {
            continue;
        };
        out.extend(read_spel_instruction_attrs(&dep_dir)?);
    }
    Ok(out)
}

/// Reject duplicate instruction names across user fns and discovered
/// extensions. Duplicates would produce colliding enum variants, match
/// arms, and IDL discriminators, or silently shadow one another.
/// `instructions` yields `(fn name, source_label)`; first seen wins,
/// the second sighting reports both sources.
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

/// Path dependencies declared directly in this crate's own Cargo.toml.
/// One level, deliberately not transitive: a dependency of a dependency
/// can never contribute instructions the consumer did not opt into.
fn direct_path_dep_dirs<F: FnMut(String)>(manifest_dir: &Path, on_warning: &mut F) -> Vec<PathBuf> {
    let Some(manifest) = crate::idl_gen::_find_crate_manifest(manifest_dir, &mut |w| on_warning(w))
    else {
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
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for (name, dep) in table {
        if let Some(rel) = dep.get("path").and_then(|v| v.as_str()) {
            let dir = manifest_dir.join(rel);
            if dir.is_dir() {
                // Deduplicate by canonical path: `package =` aliases can list
                // the same directory under two dependency names, and scanning
                // it twice would trip the duplicate-instruction-name check.
                let canonical = dir.canonicalize().unwrap_or_else(|_| dir.clone());
                if seen.insert(canonical) {
                    dirs.push(dir);
                }
            } else {
                on_warning(format!(
                    "path dependency '{}' points to non-existent directory: {}",
                    name,
                    dir.display()
                ));
            }
        }
    }
    dirs
}

/// [package].name from crate dir's manifest, `-` mapped to `_`, for use
/// as the crate ident in generated cross-crate call paths. The directory
/// name is not the crate's identity: renamed checkouts, vendoring, and
/// `package = ` aliases all diverge from it.
fn read_package_ident(crate_dir: &Path) -> Option<String> {
    let content = std::fs::read_to_string(crate_dir.join("Cargo.toml")).ok()?;
    let value: toml::Value = toml::from_str(&content).ok()?;
    let name = value.get("package")?.get("name")?.as_str()?;
    Some(name.replace('-', "_"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::idl::{IdlSeed, IdlType, SpelIdl};
    use crate::test_utils::TempDir;

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
        assert_eq!(
            read_spel_extension_attr(tmp.path()).unwrap(),
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
        assert!(read_spel_extension_attr(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn read_spel_extension_attr_none_when_no_manifest() {
        let tmp = TempDir::new("ext-attr-no-manifest");
        assert!(read_spel_extension_attr(tmp.path()).unwrap().is_none());
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
            discover_extension_instructions(&tmp.path().join("user"), &mod_attrs, &mut |_| {})
                .unwrap();
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
    fn aliased_path_dep_scanned_once() {
        let tmp = TempDir::new("discover-aliased-dep");

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

        // Same directory listed under two dependency names via a
        // `package = ` alias. A double scan would surface ext_action twice
        // and fail the duplicate-instruction-name check downstream.
        tmp.write(
            "user/Cargo.toml",
            r#"
[package]
name = "user"
version = "0.1.0"
edition = "2021"

[dependencies]
my-ext = { path = "../my-ext" }
my-ext-alias = { package = "my-ext", path = "../my-ext" }
"#,
        );
        tmp.write("user/src/lib.rs", "");

        let mod_attrs: Vec<Attribute> = syn::parse_quote!(
            #[lez_program]
            #[my_ext]
        );

        let found =
            discover_extension_instructions(&tmp.path().join("user"), &mod_attrs, &mut |_| {})
                .unwrap();
        assert_eq!(found.len(), 1, "aliased dep must be scanned once");
        assert_eq!(found[0].0.sig.ident.to_string(), "ext_action");
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
            discover_extension_instructions(&tmp.path().join("user"), &mod_attrs, &mut |_| {})
                .unwrap();
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
            discover_extension_instructions(&tmp.path().join("user"), &mod_attrs, &mut |_| {})
                .unwrap();
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
        let err =
            discover_extension_instructions(&tmp.path().join("user"), &mod_attrs, &mut |_| {})
                .expect_err("wrong-shaped extension_attr must fail, not degrade to no-extension");
        assert!(err.contains("extension_attr"), "unhelpful error: {err}");
    }

    #[test]
    fn malformed_instruction_attrs_is_a_hard_error() {
        let tmp = TempDir::new("malformed-instr-attrs");
        let mod_attrs = ext_fixture(
            &tmp,
            r#"
[package.metadata.spel]
extension_attr = "my_ext"
instruction_attrs = "require_x"
"#,
            "",
        );
        let err =
            discover_extension_instruction_attrs(&tmp.path().join("user"), &mod_attrs, &mut |_| {})
                .expect_err("non-array instruction_attrs must fail");
        assert!(err.contains("instruction_attrs"), "unhelpful error: {err}");
    }

    #[test]
    fn unreadable_consumer_manifest_warns() {
        let tmp = TempDir::new("no-consumer-manifest");
        // consumer dir exists but has no Cargo.toml at all
        tmp.write("user/src/lib.rs", "");
        let mod_attrs: Vec<Attribute> = syn::parse_quote!(#[my_ext]);

        let mut warnings = Vec::new();
        let found =
            discover_extension_instructions(&tmp.path().join("user"), &mod_attrs, &mut |w| {
                warnings.push(w)
            })
            .unwrap();
        assert!(found.is_empty());
        assert!(!warnings.is_empty(), "failure must be loud, got silence");
    }

    #[test]
    fn gate_only_extension_does_not_warn() {
        let tmp = TempDir::new("gate-only-ext");
        let mod_attrs = ext_fixture(
            &tmp,
            r#"
[package.metadata.spel]
extension_attr = "my_ext"
instruction_attrs = ["require_x"]
"#,
            "", // no #[instruction] fns: a pure gate library is a valid extension
        );
        let mut warnings = Vec::new();
        let found =
            discover_extension_instructions(&tmp.path().join("user"), &mod_attrs, &mut |w| {
                warnings.push(w)
            })
            .unwrap();
        assert!(found.is_empty());
        assert!(
            warnings.is_empty(),
            "gate-only extension is legitimate, got: {warnings:?}"
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
        let found =
            discover_extension_instructions(&tmp.path().join("user"), &mod_attrs, &mut |w| {
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
            discover_extension_instructions(&tmp.path().join("user"), &mod_attrs, &mut |_| {})
                .unwrap();
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
            discover_extension_instructions(&tmp.path().join("user"), &mod_attrs, &mut |_| {})
                .unwrap();
        assert!(found.is_empty(), "non-extension deps must not be scanned");
    }

    #[test]
    fn duplicate_names_error_names_both_sources() {
        let pairs = vec![
            ("update_value".to_string(), "this module".to_string()),
            (
                "admin_initialize".to_string(),
                "extension admin_authority".to_string(),
            ),
            (
                "update_value".to_string(),
                "extension admin_authority".to_string(),
            ),
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
        assert!(
            err.contains("admin_authority"),
            "must name the second source: {err}"
        );
    }

    #[test]
    fn unique_names_pass_duplicate_check() {
        let pairs = vec![
            ("update_value".to_string(), "this module".to_string()),
            (
                "admin_initialize".to_string(),
                "extension admin_authority".to_string(),
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
}
