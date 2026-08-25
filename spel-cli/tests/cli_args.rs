//! Argument-parsing regression tests against the real binary.

use std::process::Command;

fn spel(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_spel"))
        .args(args)
        .output()
        .expect("failed to run spel binary")
}

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn export_as_last_token_errors_cleanly() {
    let out = spel(&["--export"]);
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("--export requires"),
        "stderr: {}",
        stderr_of(&out)
    );
}

#[test]
fn co_signer_without_value_errors() {
    // Shape 1: flag is the last token, nothing after it.
    let out = spel(&["--co-signer"]);
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("--co-signer requires an account id"),
        "stderr: {}",
        stderr_of(&out)
    );

    // Shape 2: next token is another flag, not a value.
    let out = spel(&["--co-signer", "--dry-run"]);
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("--co-signer requires an account id"),
        "stderr: {}",
        stderr_of(&out)
    );
}

#[test]
fn export_twice_errors() {
    let out = spel(&["--export", "a.json", "--export", "b.json"]);
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("export given twice"),
        "stderr: {}",
        stderr_of(&out)
    )
}

#[test]
fn export_conflicts_with_dry_run() {
    let out = spel(&["--export", "a.json", "--dry-run"]);
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("--export and --dry-run cannot be combined"),
        "stderr: {}",
        stderr_of(&out)
    )
}

#[test]
fn dry_run_conflicts_with_export() {
    let out = spel(&["--dry-run", "--export", "a.json"]);
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("--export and --dry-run cannot be combined"),
        "stderr: {}",
        stderr_of(&out)
    )
}

/// Minimal one-instruction IDL for exercising instruction-arg parsing.
fn write_fixture_idl(dir: &std::path::Path) -> std::path::PathBuf {
    let idl = serde_json::json!({
        "version": "0.1.0",
        "name": "fixture",
        "instructions": [{
            "name": "do_thing",
            "accounts": [{ "name": "caller", "signer": true }],
            "args": [{ "name": "new_value", "type": "u64" }]
        }]
    });
    let path = dir.join("fixture-idl.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&idl).unwrap()).unwrap();
    path
}

// Live-round regression (2026-08-09): --export placed after the '--'
// separator was silently dropped and the transaction submitted to the
// sequencer instead of being written to the blob file.
#[test]
fn misplaced_export_refuses_before_building_a_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let idl = write_fixture_idl(dir.path());
    let blob = dir.path().join("never-written.json");

    let out = spel(&[
        "--idl",
        idl.to_str().unwrap(),
        "--program",
        &"ab".repeat(32),
        "--",
        "do-thing",
        "--new-value",
        "7",
        "--caller",
        &"11".repeat(32),
        "--export",
        blob.to_str().unwrap(),
    ]);

    assert!(!out.status.success(), "misplaced --export must refuse");
    let err = stderr_of(&out);
    assert!(
        err.contains("--export: unknown argument"),
        "stderr names the flag: {err}"
    );
    assert!(
        err.contains("before the '--' separator"),
        "stderr points at the fix: {err}"
    );
    assert!(!blob.exists(), "no blob may be written on refusal");
}

// Control: the same invocation with the flag in its global position
// parses and reaches transaction building (--dry-run, no network).
#[test]
fn well_placed_args_still_parse() {
    let dir = tempfile::tempdir().unwrap();
    let idl = write_fixture_idl(dir.path());

    let out = spel(&[
        "--idl",
        idl.to_str().unwrap(),
        "--program",
        &"ab".repeat(32),
        "--dry-run",
        "--",
        "do-thing",
        "--new-value",
        "7",
        "--caller",
        &"11".repeat(32),
    ]);

    let err = stderr_of(&out);
    assert!(
        out.status.success(),
        "dry-run with legal args must pass, stderr: {err}"
    );
    assert!(!err.contains("unknown argument"), "stderr: {err}");
}
