//! `spel encode` and offline `spel inspect --data` against the real binary, with the golden
//! token IDL from spel-framework-core's fixtures.

use std::process::Command;

fn token_idl() -> String {
    format!(
        "{}/../spel-framework-core/tests/fixtures/golden/token.json",
        env!("CARGO_MANIFEST_DIR")
    )
}

fn spel(args: &[&str]) -> std::process::Output {
    let dir = tempfile::tempdir().unwrap();
    Command::new(env!("CARGO_BIN_EXE_spel"))
        .current_dir(dir.path())
        .env("LEE_WALLET_HOME_DIR", dir.path())
        .args(args)
        .output()
        .expect("failed to run spel binary")
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn encode_prints_tag_then_fields() {
    let to = "ab".repeat(32);
    let out = spel(&[
        "--idl",
        &token_idl(),
        "encode",
        "transfer",
        "--to",
        &to,
        "--amount",
        "300",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    let expected = format!("02{to}{}", hex::encode(300u128.to_le_bytes()));
    assert_eq!(stdout(&out), expected);
}

#[test]
fn encode_rejects_an_unknown_field() {
    let out = spel(&["--idl", &token_idl(), "encode", "burn", "--amont", "1"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("--amont"), "{}", stderr(&out));
}

#[test]
fn inspect_decodes_data_as_the_state_type() {
    let data = format!(
        "01{}{}",
        "07".repeat(32),
        hex::encode(690u128.to_le_bytes())
    );
    let out = spel(&["--idl", &token_idl(), "inspect", "x", "--data", &data]);
    assert!(out.status.success(), "{}", stderr(&out));
    let value: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(value["Holding"]["balance"], "690");
}

#[test]
fn inspect_refuses_leftover_bytes() {
    let data = format!(
        "01{}{}00",
        "07".repeat(32),
        hex::encode(690u128.to_le_bytes())
    );
    let out = spel(&["--idl", &token_idl(), "inspect", "x", "--data", &data]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("remaining"), "{}", stderr(&out));
}

#[test]
fn a_version_one_idl_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let idl = dir.path().join("old.json");
    std::fs::write(&idl, r#"{"version":"0.1.0","name":"x","instructions":[]}"#).unwrap();
    let out = spel(&["--idl", idl.to_str().unwrap(), "idl"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("not supported"), "{}", stderr(&out));
}
