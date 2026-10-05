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
    // This helper is only ever called from #[test] fns; a failure to launch
    // the test binary itself is a setup error the test should panic on.
    #[allow(clippy::expect_used)]
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
