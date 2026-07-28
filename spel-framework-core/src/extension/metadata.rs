//! TOML readers for `[package.metadata.spel]` sections.
//!
//! Absent sections are `Ok(None)` / `Ok(empty)`: a normal crate. A
//! present but malformed section is a hard `Err`; a broken extension
//! declaration must never degrade to a program silently missing its
//! extension surface.

use std::path::Path;

use super::{InjectAccount, InjectSeed, InjectSpec, WrapInstructions};

/// One `[[package.metadata.spel.bound_args]]` entry: a trailing fn
/// param the framework fills at the dispatch call site from a module
/// marker kwarg, never from the transaction. Excluded from the IDL by
/// construction: discovery strips the param from the collected fn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundArg {
    /// Trailing fn param name to strip and fill.
    pub arg: String,
    /// Module marker kwarg the value comes from. `"offset"` is the
    /// only value-carrying kwarg today; a `from` naming anything else
    /// resolves to `default` until the marker grammar grows.
    pub from: String,
    /// Value when the marker does not carry the kwarg (dedicated mode).
    pub default: usize,
}

pub(super) fn read_spel_bound_args(
    value: &toml::Value,
    crate_dir: &Path,
) -> Result<Vec<BoundArg>, String> {
    let malformed = |what: &str| malformed_metadata(crate_dir, what);

    let Some(bounds) = value
        .get("package")
        .and_then(|p| p.get("metadata"))
        .and_then(|m| m.get("spel"))
        .and_then(|s| s.get("bound_args"))
    else {
        return Ok(vec![]);
    };
    let Some(arr) = bounds.as_array() else {
        return Err(malformed("bound_args must be an array of tables"));
    };
    arr.iter()
        .map(|b| {
            let Some(arg) = b.get("arg").and_then(|v| v.as_str()) else {
                return Err(malformed("bound_args.arg must be a string"));
            };
            let Some(from) = b.get("from").and_then(|v| v.as_str()) else {
                return Err(malformed("bound_args.from must be a string"));
            };
            let default = match b.get("default") {
                None => 0,
                Some(v) => usize::try_from(
                    v.as_integer()
                        .ok_or_else(|| malformed("bound_args.default muts be an integer"))?,
                )
                .map_err(|_| malformed("bound_args.default must not be negative"))?,
            };
            Ok(BoundArg {
                arg: arg.to_string(),
                from: from.to_string(),
                default,
            })
        })
        .collect()
}

/// Parsed Cargo.toml of a dependency dir, or `None` when unreadable or
/// unparseable. Silent: cargo itself fails the build for those cases.
pub(super) fn read_manifest_value(crate_dir: &Path) -> Option<toml::Value> {
    let content = std::fs::read_to_string(crate_dir.join("Cargo.toml")).ok()?;
    toml::from_str(&content).ok()
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
pub(super) fn read_spel_extension_attr(
    value: &toml::Value,
    crate_dir: &Path,
) -> Result<Option<String>, String> {
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
            None => Err(malformed_metadata(
                crate_dir,
                "extension_attr must be a string",
            )),
        },
    }
}

/// Read `[[package.metadata.spel.inject]]` blocks from a parsed manifest.
///
/// Absent inject key is `Ok(empty)`, a normal extension without param
/// injection. Malformed blocks are a hard `Err`: a broken inject
/// declaration must never degrade to a gate silently missing its
/// account constraints.
pub(super) fn read_spel_inject_specs(
    value: &toml::Value,
    crate_dir: &Path,
) -> Result<Vec<InjectSpec>, String> {
    let malformed = |what: &str| malformed_metadata(crate_dir, what);

    let Some(injects) = value
        .get("package")
        .and_then(|p| p.get("metadata"))
        .and_then(|m| m.get("spel"))
        .and_then(|s| s.get("inject"))
    else {
        return Ok(vec![]);
    };
    let Some(injects) = injects.as_array() else {
        return Err(malformed("inject must be an array of tables"));
    };

    let mut specs = Vec::new();
    for inject in injects {
        let Some(wrapper) = inject.get("wrapper").and_then(|w| w.as_str()) else {
            return Err(malformed("inject.wrapper must be a string"));
        };
        let Some(accs) = inject.get("account").and_then(|a| a.as_array()) else {
            return Err(malformed("inject.account must be an array of tables"));
        };
        let mut accounts = Vec::new();
        for acc in accs {
            let Some(name) = acc.get("name").and_then(|n| n.as_str()) else {
                return Err(malformed("inject.account.name must be a string"));
            };
            let seeds = match acc.get("seed") {
                None => vec![],
                Some(toml::Value::Array(entries)) => entries
                    .iter()
                    .map(|e| {
                        parse_seed_entry(e).ok_or_else(|| {
                            malformed(
                                "inject.account.seed entries must be { const = \"...\" } or { account = \"...\" }",
                            )
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                Some(single) => vec![parse_seed_entry(single).ok_or_else(|| {
                    malformed(
                        "inject.account.seed must be { const = \"...\" }, { account = \"...\" }, or an array of those",
                    )
                })?],
            };
            let signer = match acc.get("signer") {
                None => false,
                Some(b) => b
                    .as_bool()
                    .ok_or_else(|| malformed("inject.account.signer must be a boolean"))?,
            };
            accounts.push(InjectAccount {
                name: name.to_string(),
                role: name.to_string(),
                seeds,
                signer,
                embedded: false,
            });
        }
        specs.push(InjectSpec {
            wrapper: wrapper.to_string(),
            accounts,
            source: String::new(),
        });
    }
    Ok(specs)
}

/// Read `[package.metadata.spel.wrap_instructions` from parsed
/// manifest.
///
/// Absent section in `Ok(None)`, the extension does not wrap. A present
/// but malformed section is a hard `Err`: a broken wrap declaration
/// must never degrade to a program silently shipping unwrapped
/// instructions.
pub(super) fn read_spel_wrap_instructions(
    value: &toml::Value,
    crate_dir: &Path,
) -> Result<Option<WrapInstructions>, String> {
    let malformed = |what: &str| malformed_metadata(crate_dir, what);

    let Some(wrap) = value
        .get("package")
        .and_then(|p| p.get("metadata"))
        .and_then(|m| m.get("spel"))
        .and_then(|s| s.get("wrap_instructions"))
    else {
        return Ok(None);
    };

    let Some(wrapper) = wrap.get("wrapper").and_then(|w| w.as_str()) else {
        return Err(malformed("wrap_instructions.wrapper must be a string"));
    };
    let Some(self_exempt_marker) = wrap.get("self_exempt_marker").and_then(|w| w.as_str()) else {
        return Err(malformed(
            "wrap_instructions.self_exempt_marker must be a string",
        ));
    };
    let skip = match wrap.get("skip") {
        None => None,
        Some(v) => {
            let Some(s) = v.as_str() else {
                return Err(malformed("wrap_instructions.skip must be a string"));
            };
            if s.is_empty() {
                return Err(malformed(
                    "wrap_instructions.skip must not be empty: an empty skip word \
                    matches the bare marker and turns wrap off for every consumer",
                ));
            }
            Some(s.to_string())
        },
    };
    let exempt = match wrap.get("exempt") {
        None => vec![],
        Some(v) => {
            let Some(arr) = v.as_array() else {
                return Err(malformed(
                    "wrap_instructions.exempt must be an array of string",
                ));
            };
            arr.iter()
                .map(|x| {
                    x.as_str().map(String::from).ok_or_else(|| {
                        malformed("wrap_instructions.exempt must be and array of strings")
                    })
                })
                .collect::<Result<Vec<_>, _>>()?
        },
    };

    Ok(Some(WrapInstructions {
        wrapper: wrapper.to_string(),
        skip,
        self_exempt_marker: self_exempt_marker.to_string(),
        exempt,
    }))
}

/// [package].name from crate dir's manifest, `-` mapped to `_`, for use
/// as the crate ident in generated cross-crate call paths. The directory
/// name is not the crate's identity: renamed checkouts, vendoring, and
/// `package = ` aliases all diverge from it.
pub(super) fn read_package_ident(value: &toml::Value) -> Option<String> {
    let name = value.get("package")?.get("name")?.as_str()?;
    Some(name.replace('-', "_"))
}

/// Read `[package.metadata.spel.embedded]` from a parsed manifest.
///
/// `skip` names discovered instructions the framework mus not emit
/// when the extension is in embedded mode. The slot is born
/// initialized by the consumer's own account-creating instruction,
/// so the extension's initializer has no role to play there. Absent
/// section or absent `skip` is an empty list.
///
/// # Errors
///
/// `Err` when `skip` is present but not an array of strings. Callers
/// surface it as a compile error.
pub(super) fn read_spel_embedded_skip(
    value: &toml::Value,
    crate_dir: &Path,
) -> Result<Vec<String>, String> {
    let malformed = |what: &str| malformed_metadata(crate_dir, what);

    let Some(embedded) = value
        .get("package")
        .and_then(|p| p.get("metadata"))
        .and_then(|m| m.get("spel"))
        .and_then(|s| s.get("embedded"))
    else {
        return Ok(vec![]);
    };
    let Some(skip) = embedded.get("skip") else {
        return Ok(vec![]);
    };
    let Some(arr) = skip.as_array() else {
        return Err(malformed("embedded.skip must be an array of strings"));
    };
    arr.iter()
        .map(|x| {
            x.as_str()
                .map(String::from)
                .ok_or_else(|| malformed("embedded.skip must be an array of strings"))
        })
        .collect()
}

/// Decode one TOML seed entry (`{ const = "..." }` or
/// `{ account "..." }`) into an [`InjectSeed`].
fn parse_seed_entry(value: &toml::Value) -> Option<InjectSeed> {
    let t = value.as_table()?;
    if let Some(s) = t.get("const").and_then(|c| c.as_str()) {
        return Some(InjectSeed::Const(s.to_string()));
    }
    if let Some(s) = t.get("account").and_then(|a| a.as_str()) {
        return Some(InjectSeed::Account(s.to_string()));
    }
    None
}

/// The uniform malformed-metadata message: every reader's errors share
/// one prefix so callers and tests can rely on its shape.
fn malformed_metadata(crate_dir: &Path, what: &str) -> String {
    format!(
        "malformed [package.metadata.spel] in {}: {what}",
        crate_dir.join("Cargo.toml").display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let value = read_manifest_value(tmp.path()).unwrap();
        assert_eq!(
            read_spel_extension_attr(&value, tmp.path()).unwrap(),
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
        let value = read_manifest_value(tmp.path()).unwrap();
        assert!(read_spel_extension_attr(&value, tmp.path())
            .unwrap()
            .is_none());
    }

    #[test]
    fn read_manifest_value_none_when_no_manifest() {
        let tmp = TempDir::new("ext-attr-no-manifest");
        assert!(read_manifest_value(tmp.path()).is_none());
    }

    #[test]
    fn read_inject_specs_parses_admin_block() {
        let tmp = TempDir::new("inject-specs");
        tmp.write(
            "Cargo.toml",
            r#"
[package]
name = "x"
version = "0.1.0"
edition = "2021"

[package.metadata.spel]
extension_attr = "my_ext"

[[package.metadata.spel.inject]]
wrapper = "my_gate"

  [[package.metadata.spel.inject.account]]
  name = "gate_config"
  seed = { const = "gate_config" }

  [[package.metadata.spel.inject.account]]
  name = "caller"
  signer = true
"#,
        );
        let value = read_manifest_value(tmp.path()).unwrap();
        let specs = read_spel_inject_specs(&value, tmp.path()).unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].wrapper, "my_gate");
        assert_eq!(specs[0].accounts.len(), 2);
        assert!(matches!(
            &specs[0].accounts[0].seeds[..],
            [InjectSeed::Const(s)] if s == "gate_config"
        ));
        assert!(specs[0].accounts[1].signer);
        assert!(specs[0].accounts[1].seeds.is_empty());
    }

    #[test]
    fn malformed_inject_seed_is_a_hard_error() {
        // A bare-string seed used to be swallowed, injecting the account
        // unconstrained where a PDA-verified one was intended.
        let tmp = TempDir::new("inject-bad-seed");
        tmp.write(
            "Cargo.toml",
            r#"
[package]
name = "x"
version = "0.1.0"
edition = "2021"

[[package.metadata.spel.inject]]
wrapper = "my_gate"

  [[package.metadata.spel.inject.account]]
  name = "gate_config"
  seed = "gate_config"
"#,
        );
        let value = read_manifest_value(tmp.path()).unwrap();
        let err = read_spel_inject_specs(&value, tmp.path())
            .expect_err("bare-string seed must be rejected");
        assert!(err.contains("seed"), "unexpected error: {err}");
    }

    #[test]
    fn malformed_inject_wrapper_is_a_hard_error() {
        let tmp = TempDir::new("inject-bad-wrapper");
        tmp.write(
            "Cargo.toml",
            r#"
[package]
name = "x"
version = "0.1.0"
edition = "2021"

[[package.metadata.spel.inject]]
wrapper = 42
"#,
        );
        let value = read_manifest_value(tmp.path()).unwrap();
        let err = read_spel_inject_specs(&value, tmp.path())
            .expect_err("non-string wrapper must be rejected");
        assert!(err.contains("wrapper"), "unexpected error: {err}");
    }

    #[test]
    fn read_inject_specs_parses_compound_seed() {
        let tmp = TempDir::new("inject-compound");
        tmp.write(
            "Cargo.toml",
            r#"
[package]
name = "x"
version = "0.1.0"
edition = "2021"

[[package.metadata.spel.inject]]
wrapper = "other_gate"

  [[package.metadata.spel.inject.account]]
  name = "marker_account"
  seed = [{ const = "marker" }, { account = "caller" }]
"#,
        );
        let value = read_manifest_value(tmp.path()).unwrap();
        let specs = read_spel_inject_specs(&value, tmp.path()).unwrap();
        let seeds = &specs[0].accounts[0].seeds;
        assert_eq!(seeds.len(), 2);
        assert!(matches!(&seeds[0], InjectSeed::Const(s) if s == "marker"));
        assert!(matches!(&seeds[1], InjectSeed::Account(s) if s == "caller"));
    }

    #[test]
    fn malformed_compound_seed_entry_is_a_hard_error() {
        // An entry that is neither const nor account used to be silently
        // skipped, shortening the seed list and deriving a wrong PDA.
        let tmp = TempDir::new("inject-bad-compound");
        tmp.write(
            "Cargo.toml",
            r#"
[package]
name = "x"
version = "0.1.0"
edition = "2021"

[[package.metadata.spel.inject]]
wrapper = "other_gate"

  [[package.metadata.spel.inject.account]]
  name = "marker_account"
  seed = [{ neither = "x" }]
"#,
        );
        let value = read_manifest_value(tmp.path()).unwrap();
        let err = read_spel_inject_specs(&value, tmp.path())
            .expect_err("unknown seed entry must be rejected");
        assert!(err.contains("seed"), "unexpected error: {err}");
    }

    #[test]
    fn read_wrap_instructions_returns_declared_config() {
        let tmp = TempDir::new("wrap-declared");
        tmp.write(
            "Cargo.toml",
            r#"
[package]
name = "ext-b"
version = "0.1.0"

[package.metadata.spel.wrap_instructions]
wrapper = "ext_b_macros::__inject_gate"
skip = "manual"
self_exempt_marker = "freeze_exempt"
exempt = ["ext_a::action_one", "ext_a::action_two"]
"#,
        );
        let value = read_manifest_value(tmp.path()).unwrap();
        let wrap = read_spel_wrap_instructions(&value, tmp.path())
            .unwrap()
            .expect("wrap config declared");
        assert_eq!(wrap.wrapper, "ext_b_macros::__inject_gate");
        assert_eq!(wrap.skip.as_deref(), Some("manual"));
        assert_eq!(wrap.self_exempt_marker, "freeze_exempt");
        assert_eq!(wrap.exempt, vec!["ext_a::action_one", "ext_a::action_two"]);
    }

    #[test]
    fn read_wrap_instructions_none_when_section_missing() {
        let tmp = TempDir::new("wrap-absent");
        tmp.write(
            "Cargo.toml",
            r#"
[package]
name = "no-wrap"
version = "0.1.0"

[package.metadata.spel]
extension_attr = "some_other"
"#,
        );
        let value = read_manifest_value(tmp.path()).unwrap();
        assert!(read_spel_wrap_instructions(&value, tmp.path())
            .unwrap()
            .is_none());
    }

    #[test]
    fn read_wrap_instructions_handles_omitted_optional_fields() {
        let tmp = TempDir::new("wrap-minimal");
        tmp.write(
            "Cargo.toml",
            r#"
[package]
name = "minimal-wrap"
version = "0.1.0"

[package.metadata.spel.wrap_instructions]
wrapper = "minimal::wrapper"
self_exempt_marker = "minimal_exempt"
"#,
        );
        let value = read_manifest_value(tmp.path()).unwrap();
        let wrap = read_spel_wrap_instructions(&value, tmp.path())
            .unwrap()
            .expect("minimal config");
        assert!(wrap.skip.is_none());
        assert!(wrap.exempt.is_empty());
    }

    #[test]
    fn bound_args_reader_parses_entries() {
        let tmp = TempDir::new("bound-args-ok");
        tmp.write(
            "Cargo.toml",
            r#"
[package]
name = "my-ext"
version = "0.1.0"

[[package.metadata.spel.bound_args]]
arg = "offset"
from = "offset"
default = 0

[[package.metadata.spel.bound_args]]
arg = "window"
from = "offset"
"#,
        );
        let value = read_manifest_value(tmp.path()).unwrap();
        let bounds = read_spel_bound_args(&value, tmp.path()).unwrap();
        assert_eq!(
            bounds,
            vec![
                BoundArg {
                    arg: "offset".to_string(),
                    from: "offset".to_string(),
                    default: 0,
                },
                BoundArg {
                    arg: "window".to_string(),
                    from: "offset".to_string(),
                    default: 0,
                },
            ]
        );
    }

    #[test]
    fn malformed_bound_args_missing_arg_is_a_hard_error() {
        let tmp = TempDir::new("bound-args-missing-arg");
        tmp.write(
            "Cargo.toml",
            r#"
[package]
name = "my-ext"
version = "0.1.0"

[[package.metadata.spel.bound_args]]
from = "offset"
"#,
        );
        let value = read_manifest_value(tmp.path()).unwrap();
        let err = read_spel_bound_args(&value, tmp.path())
            .expect_err("a bound arg without `arg` must be rejected");
        assert!(err.contains("bound_args.arg"), "unexpected error: {err}");
    }

    #[test]
    fn malformed_embedded_skip_is_a_hard_error() {
        let tmp = TempDir::new("embedded-skip-malformed");
        tmp.write(
            "Cargo.toml",
            r#"
[package]
name = "my-ext"
version = "0.1.0"

[package.metadata.spel.embedded]
skip = "ext_init"
"#,
        );
        let value = read_manifest_value(tmp.path()).unwrap();
        let err = read_spel_embedded_skip(&value, tmp.path())
            .expect_err("a non-array skip must be rejected");
        assert!(err.contains("embedded.skip"), "unexpected error: {err}");
    }

    #[test]
    fn absent_embedded_section_is_empty_skip() {
        let tmp = TempDir::new("embedded-skip-absent");
        tmp.write(
            "Cargo.toml",
            r#"
[package]
name = "my-ext"
version = "0.1.0"

[package.metadata.spel]
extension_attr = "my_ext"
"#,
        );
        let value = read_manifest_value(tmp.path()).unwrap();
        assert_eq!(read_spel_embedded_skip(&value, tmp.path()), Ok(vec![]));
    }

    #[test]
    fn malformed_wrap_missing_required_field_is_a_hard_error() {
        let tmp = TempDir::new("wrap-incomplete");
        tmp.write(
            "Cargo.toml",
            r#"
[package]
name = "incomplete"
version = "0.1.0"

[package.metadata.spel.wrap_instructions]
wrapper = "incomplete::wrapper"
"#,
        );
        let value = read_manifest_value(tmp.path()).unwrap();
        let err = read_spel_wrap_instructions(&value, tmp.path())
            .expect_err("missing self_exempt_marker must be rejected");
        assert!(
            err.contains("self_exempt_marker"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn empty_skip_is_a_hard_error() {
        let tmp = TempDir::new("wrap-empty-skip");
        tmp.write(
            "Cargo.toml",
            r#"
[package]
name = "empty-skip"
version = "0.1.0"

[package.metadata.spel.wrap_instructions]
wrapper = "x::gate"
skip = ""
self_exempt_marker = "x_exempt"
"#,
        );
        let value = read_manifest_value(tmp.path()).unwrap();
        let err = read_spel_wrap_instructions(&value, tmp.path())
            .expect_err("empty skip word must be rejected");
        assert!(err.contains("skip"), "unexpected error: {err}");
    }
}
