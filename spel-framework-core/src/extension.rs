//! Path-dep extension discovery for SPEL programs.
//!
//! Scans the consuming program's local path-dependencies for crates that
//! declare `[package.metadata.spel]` in their `Cargo.toml`. Each
//! qualifying crate exposes its `#[instruction]` fns (and optional gate
//! attributes) through this module's discovery API:
//!
//! - [`discover_extension_instructions`] returns the cross-crate
//!   `#[instruction]` fns to be merged into the consumer's dispatcher
//!   and IDL.
//! - [`discover_extension_instruction_attrs`] returns the library-owned
//!   gate attribute names that the framework strips from emitted
//!   handler fns to prevent re-expansion.
//!
//! Feature-gated identically to [`crate::idl_gen`]
//! (`#[cfg(feature = "idl-gen")]`) since it depends on `syn` and `toml`.
//! Internal helpers (`read_spel_extension_attr`,
//! `read_spel_instruction_attrs`, `collect_instruction_fns`) are
//! module-private; consumers go through the two `discover_*` entry
//! points.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use syn::Attribute;

use crate::idl_gen::{collect_items_from_crate_dirs, find_path_dep_dirs, has_instruction_attr};

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
/// Returns `None` if the manifest is missing the metadata key.
fn read_spel_extension_attr(crate_dir: &Path) -> Option<String> {
    let manifest = crate_dir.join("Cargo.toml");
    let content = std::fs::read_to_string(&manifest).ok()?;
    let value: toml::Value = toml::from_str(&content).ok()?;
    value
        .get("package")?
        .get("metadata")?
        .get("spel")?
        .get("extension_attr")?
        .as_str()
        .map(String::from)
}

/// Read `[package.metadata.spel.instruction_attrs]` from a crate's Cargo.toml.
/// Returns the list of instruction-level marker attribute names the library
/// declares (e.g. `["require_admin"]`). Framework strips these from emitted
/// handler fns to prevent re-expansion.
fn read_spel_instruction_attrs(crate_dir: &Path) -> Vec<String> {
    fn inner(crate_dir: &Path) -> Option<Vec<String>> {
        let manifest = crate_dir.join("Cargo.toml");
        let content = std::fs::read_to_string(&manifest).ok()?;
        let value: toml::Value = toml::from_str(&content).ok()?;
        let arr = value
            .get("package")?
            .get("metadata")?
            .get("spel")?
            .get("instruction_attrs")?
            .as_array()?;
        Some(arr.iter().filter_map(|x| x.as_str().map(String::from)).collect())
    }
    inner(crate_dir).unwrap_or_default()
}

/// Scan path-dep crates for SPEL extension libraries whose `extension_attr`
/// metadata matches an attribute on the consuming program's mod. Returns
/// one `(ItemFn, crate_path)` per discovered `#[instruction]` fn, where
/// `crate_path` is the absolute path to call the fn from the consumer
/// (e.g. `::admin_authority`).
///
/// Silent on env/IO errors — returns empty. Callers may emit their own
/// error if they expected results.
pub fn discover_extension_instructions(
    manifest_dir: &Path,
    mod_attrs: &[Attribute],
) -> Vec<(syn::ItemFn, syn::Path)> {
    let dep_dirs = direct_path_dep_dirs(&manifest_dir);
    
    let mut out = Vec::new();
    for dep_dir in dep_dirs {
        let Some(ext_attr) = read_spel_extension_attr(&dep_dir) else {
            continue;
        };
        if !mod_attrs.iter().any(|a| a.path().is_ident(&ext_attr)) {
            continue;
        }

        let (items, _) = collect_items_from_crate_dirs(&[dep_dir.clone()]);
        let Some(crate_name) = read_package_ident(&dep_dir) else { continue };
        let crate_name = crate_name.replace('-', "_");
        let crate_ident = syn::Ident::new(&crate_name, proc_macro2::Span::call_site());
        let crate_path: syn::Path = syn::parse_quote!(::#crate_ident);

        for func in collect_instruction_fns(&items) {
            out.push((func, crate_path.clone()));
        }
    }
    out
}

/// Collect all instruction-level marker attribute names declared by
/// path-dep extension libraries whose `extension_attr` matches an attribute
/// on the consuming program's mod. Framework strips these from emitted
/// handler fns so the lib's own gate macros don't re-expand on them.
pub fn discover_extension_instruction_attrs(
    manifest_dir: &Path,
    mod_attrs: &[syn::Attribute],
) -> Vec<String> {
    let dep_dirs = find_path_dep_dirs(manifest_dir, |_| {});
    let mut out = Vec::new();
    for dep_dir in dep_dirs {
        let Some(ext_attr) = read_spel_extension_attr(&dep_dir) else { continue };
        if !mod_attrs.iter().any(|a| a.path().is_ident(&ext_attr)) { continue };
        out.extend(read_spel_instruction_attrs(&dep_dir));
    }
    out
}

/// Path dependencies declared directly in this crate's own Cargo.toml.
/// One level, deliberately not transitive: a dependency of a dependency
/// can never contribute instructions the consumer did not opt into.
fn direct_path_dep_dirs(manifest_dir: &Path) -> Vec<PathBuf> {
    fn inner(manifest_dir: &Path) -> Option<Vec<PathBuf>> {
        let content = std::fs::read_to_string(manifest_dir.join("Cargo.toml")).ok()?;
        let value: toml::Value = toml::from_str(&content).ok()?;
        let table = value.get("dependencies")?.as_table()?;
        let mut dirs = Vec::new();
        for dep in table.values() {
            if let Some(rel) = dep.get("path").and_then(|v| v.as_str()) {
                let dir = manifest_dir.join(rel);
                if dir.is_dir() {
                    dirs.push(dir);
                }
            }
        }
        Some(dirs)
    }
    inner(manifest_dir).unwrap_or_default()
}

/// [package].name from crate dir's manifest, `-` mapped to `_`, for use
/// as the crate ident in generated cross-crate call paths. The directory
/// name is not the crate's identity: renamed checkouts, vendoring, and 
/// `package = ` aliases all diverge from it.
fn read_package_ident(crate_dir: &Path) -> Option<String> {
    let content  = std::fs::read_to_string(crate_dir.join("Cargo.toml")).ok()?;
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
            read_spel_extension_attr(tmp.path()),
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
        assert!(read_spel_extension_attr(tmp.path()).is_none());
    }

    #[test]
    fn read_spel_extension_attr_none_when_no_manifest() {
        let tmp = TempDir::new("ext-attr-no-manifest");
        assert!(read_spel_extension_attr(tmp.path()).is_none());
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

        let found = discover_extension_instructions(&tmp.path().join("user"), &mod_attrs);
        assert_eq!(found.len(), 1);
        let (func, crate_path) = &found[0];
        assert_eq!(func.sig.ident.to_string(), "ext_action");
        let segs: Vec<String> = crate_path.segments.iter().map(|s| s.ident.to_string()).collect();
        assert_eq!(segs, vec!["my_ext".to_string()]);
        assert!(crate_path.leading_colon.is_some(), "path must start with ::");
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

        let found = discover_extension_instructions(&tmp.path().join("user"), &mod_attrs);
        assert_eq!(found.len(), 1);
        let (_, crate_path) = &found[0];
        let segs: Vec<String> = crate_path.segments.iter().map(|s| s.ident.to_string()).collect();
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

        let found = discover_extension_instructions(&tmp.path().join("user"), &mod_attrs);
        assert!(
            found.is_empty(),
            "transitive dep must never contribute instructions, got: {:?}",
            found.iter().map(|(f, _)| f.sig.ident.to_string()).collect::<Vec<_>>()
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

        let found = discover_extension_instructions(&tmp.path().join("user"), &mod_attrs);
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

        let found = discover_extension_instructions(&tmp.path().join("user"), &mod_attrs);
        assert!(found.is_empty(), "non-extension deps must not be scanned");
    }
}
