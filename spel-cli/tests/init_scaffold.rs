//! `spel init` must pin every crate it writes to the same framework and LEZ
//! revisions.
//!
//! CI always passes an explicit `--spel-rev`, so the defaults are never built
//! there. The FFI crate's framework default drifted to a three-release-old tag
//! that way, and `make ffi` on a fresh project failed with
//! `cannot find function compute_pda_raw in module spel_framework_core::pda`.

use std::path::Path;
use std::process::Command;

const NAME: &str = "scaffold_check";

fn init(dir: &Path, extra: &[&str]) {
    let out = Command::new(env!("CARGO_BIN_EXE_spel"))
        .arg("init")
        .args(extra)
        .arg(NAME)
        .current_dir(dir)
        // `init` finishes by running `cargo generate-lockfile` on the new
        // project, which resolves the whole dependency graph over the network
        // (minutes, and it fails for the fake revisions used below). This test
        // only reads the manifests, so hide cargo: `init` treats a missing
        // cargo as a warning and still writes every file.
        .env("PATH", "")
        .output()
        .expect("failed to run spel binary");
    assert!(
        out.status.success(),
        "spel init failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The git selector (`tag = "…"`, `rev = "…"` or `branch = "…"`) of `dep`
/// in the manifest at `rel`.
fn selector(root: &Path, rel: &str, dep: &str) -> String {
    let manifest = std::fs::read_to_string(root.join(NAME).join(rel))
        .unwrap_or_else(|e| panic!("reading {rel}: {e}"));
    let line = manifest
        .lines()
        .find(|l| l.split('=').next().map(str::trim) == Some(dep))
        .unwrap_or_else(|| panic!("{rel} has no `{dep}` dependency"));
    ["tag", "rev", "branch"]
        .iter()
        .find_map(|key| {
            let start = line.find(&format!("{key} = \""))?;
            let rest = &line[start..];
            let end = rest[key.len() + 4..].find('"')? + key.len() + 5;
            Some(rest[..end].to_string())
        })
        .unwrap_or_else(|| panic!("{rel}: `{dep}` has no git selector: {line}"))
}

fn framework_selectors(root: &Path) -> Vec<(String, String)> {
    let ffi = format!("{NAME}_ffi/Cargo.toml");
    vec![
        (
            "methods/guest spel-framework".into(),
            selector(root, "methods/guest/Cargo.toml", "spel-framework"),
        ),
        (
            "examples spel-framework".into(),
            selector(root, "examples/Cargo.toml", "spel-framework"),
        ),
        (
            "examples spel".into(),
            selector(root, "examples/Cargo.toml", "spel"),
        ),
        (
            "ffi spel-framework-core".into(),
            selector(root, &ffi, "spel-framework-core"),
        ),
    ]
}

fn lez_selectors(root: &Path) -> Vec<(String, String)> {
    let ffi = format!("{NAME}_ffi/Cargo.toml");
    vec![
        (
            "methods/guest nssa_core".into(),
            selector(root, "methods/guest/Cargo.toml", "nssa_core"),
        ),
        (
            "examples nssa_core".into(),
            selector(root, "examples/Cargo.toml", "nssa_core"),
        ),
        ("ffi nssa_core".into(), selector(root, &ffi, "nssa_core")),
        ("ffi wallet".into(), selector(root, &ffi, "wallet")),
    ]
}

fn assert_all(selectors: &[(String, String)], expected: &str) {
    for (site, got) in selectors {
        assert_eq!(got, expected, "{site} pins `{got}`, expected `{expected}`");
    }
}

#[test]
fn defaults_pin_every_crate_to_the_same_revisions() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path(), &[]);

    let default_spel = format!("branch = \"{}\"", spel::init::DEFAULT_SPEL_BRANCH);
    let default_lez = format!("tag = \"{}\"", spel::init::DEFAULT_LEZ_TAG);
    assert_all(&framework_selectors(dir.path()), &default_spel);
    assert_all(&lez_selectors(dir.path()), &default_lez);
}

#[test]
fn explicit_revisions_reach_every_crate() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path(), &["--spel-tag", "v9.9.9", "--lez-rev", "abc123"]);

    assert_all(&framework_selectors(dir.path()), "tag = \"v9.9.9\"");
    assert_all(&lez_selectors(dir.path()), "rev = \"abc123\"");
}

/// Runs `spel init <args…>` verbatim, with cargo hidden for the reason given
/// on [`init`].
fn run_init(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_spel"))
        .arg("init")
        .args(args)
        .current_dir(dir)
        .env("PATH", "")
        .output()
        .expect("failed to run spel binary")
}

/// Flags written after the project name used to be dropped without a word, and
/// the project was created pinned to the defaults instead.
#[test]
fn flags_after_the_project_name_are_honoured() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_init(
        dir.path(),
        &[NAME, "--spel-tag", "v9.9.9", "--lez-rev", "abc123"],
    );
    assert!(
        out.status.success(),
        "spel init failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert_all(&framework_selectors(dir.path()), "tag = \"v9.9.9\"");
    assert_all(&lez_selectors(dir.path()), "rev = \"abc123\"");
}

/// An argument `init` does not understand must fail loudly, not be discarded,
/// and must not leave a half-configured project behind.
#[test]
fn unexpected_arguments_are_an_error() {
    for extra in [
        &[NAME, "--no-such-flag"][..],
        &[NAME, "another-name"][..],
        &["--no-such-flag", NAME][..],
    ] {
        let dir = tempfile::tempdir().unwrap();
        let out = run_init(dir.path(), extra);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{extra:?} was accepted:\n{stderr}");
        assert!(
            stderr.contains("unexpected argument"),
            "{extra:?}: no `unexpected argument` error:\n{stderr}"
        );
        assert!(
            !dir.path().join(NAME).exists(),
            "{extra:?} still created the project"
        );
    }
}

/// A flag with nothing after it used to be ignored the same way.
#[test]
fn a_flag_without_a_value_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_init(dir.path(), &[NAME, "--spel-tag"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "accepted:\n{stderr}");
    assert!(stderr.contains("--spel-tag"), "{stderr}");
    assert!(!dir.path().join(NAME).exists());
}
