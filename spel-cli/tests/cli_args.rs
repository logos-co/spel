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
