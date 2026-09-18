//! `spel inspect <account-id>` against the real binary: fetching a public
//! account must need a sequencer address and nothing else from the wallet.
//!
//! It used to go through `WalletCore::from_env()`, which loads and validates
//! the whole wallet storage before the first request, so a read-only public
//! lookup failed whenever `storage.json` was written by a wallet build whose
//! schema differs from the one compiled into `spel` (#236).

use nssa_core::account::{Account, Data};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;

/// One account type, `Counter { value: u64 }`.
fn write_fixture_idl(dir: &Path) -> std::path::PathBuf {
    let idl = serde_json::json!({
        "version": "0.1.0",
        "name": "fixture",
        "instructions": [],
        "accounts": [{
            "name": "Counter",
            "type": { "kind": "struct", "fields": [{ "name": "value", "type": "u64" }] }
        }]
    });
    let path = dir.join("fixture-idl.json");
    std::fs::write(&path, idl.to_string()).unwrap_or_else(|e| panic!("writing IDL: {e}"));
    path
}

const ACCOUNT_ID: &str = "G8psqsYkhmsTzifFrL73AJuczh9iUERnsb51CJNsELw2";
const COUNTER_VALUE: u64 = 42;

/// A sequencer that answers every JSON-RPC request with the same account and
/// records the request bodies it saw.
struct FakeSequencer {
    url: String,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl FakeSequencer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .unwrap_or_else(|e| panic!("binding a local port: {e}"));
        let addr = listener
            .local_addr()
            .unwrap_or_else(|e| panic!("reading the bound port: {e}"));
        let url = format!("http://{addr}");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        let account = Account {
            data: Data::try_from(
                borsh::to_vec(&COUNTER_VALUE).unwrap_or_else(|e| panic!("borsh: {e}")),
            )
            .unwrap_or_else(|e| panic!("account data: {e}")),
            ..Default::default()
        };
        let account = serde_json::to_value(account).unwrap_or_else(|e| panic!("account json: {e}"));
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(
                    stream
                        .try_clone()
                        .unwrap_or_else(|e| panic!("cloning the socket: {e}")),
                );
                let mut content_length = 0;
                loop {
                    let mut line = String::new();
                    let read = reader
                        .read_line(&mut line)
                        .unwrap_or_else(|e| panic!("reading headers: {e}"));
                    if read == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        content_length = v
                            .trim()
                            .parse()
                            .unwrap_or_else(|e| panic!("content-length: {e}"));
                    }
                }
                let mut body = vec![0; content_length];
                reader
                    .read_exact(&mut body)
                    .unwrap_or_else(|e| panic!("reading the body: {e}"));
                let request: serde_json::Value = serde_json::from_slice(&body)
                    .unwrap_or_else(|e| panic!("request is not JSON: {e}"));
                let reply = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": request.get("id"),
                    "result": account,
                })
                .to_string();
                seen.lock()
                    .unwrap_or_else(|e| panic!("request log poisoned: {e}"))
                    .push(request);
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    reply.len(),
                    reply
                )
                .unwrap_or_else(|e| panic!("writing the response: {e}"));
            }
        });
        Self { url, requests }
    }

    fn requests(&self) -> Vec<serde_json::Value> {
        self.requests
            .lock()
            .unwrap_or_else(|e| panic!("request log poisoned: {e}"))
            .clone()
    }
}

/// `spel inspect` with `LEE_WALLET_HOME_DIR` pointing at `wallet_home`.
fn inspect(wallet_home: &Path, idl: &Path, extra: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_spel"))
        .env("LEE_WALLET_HOME_DIR", wallet_home)
        .args(["inspect", ACCOUNT_ID, "--idl"])
        .arg(idl)
        .args(["--type", "Counter"])
        .args(extra)
        .output()
        .unwrap_or_else(|e| panic!("failed to run spel binary: {e}"))
}

fn stdout_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The storage file the issue reports: written by a newer wallet, unreadable
/// by the schema compiled into `spel`.
fn write_unreadable_storage(wallet_home: &Path) {
    std::fs::write(
        wallet_home.join("storage.json"),
        r#"{ "key_chain": { "unknown_shape": {} } }"#,
    )
    .unwrap_or_else(|e| panic!("writing storage.json: {e}"));
}

fn assert_decoded_counter(out: &std::process::Output, sequencer: &FakeSequencer) {
    assert!(out.status.success(), "inspect failed:\n{}", stderr_of(out));
    let decoded: serde_json::Value = serde_json::from_str(stdout_of(out).trim())
        .unwrap_or_else(|e| panic!("stdout is not JSON: {e}\n{}", stdout_of(out)));
    // The decoder renders u64 and u128 as strings to avoid JSON precision loss.
    assert_eq!(
        decoded.get("value"),
        Some(&serde_json::json!(COUNTER_VALUE.to_string())),
        "decoded account: {decoded}"
    );

    let requests = sequencer.requests();
    let [request] = requests.as_slice() else {
        panic!("expected one RPC call, saw {requests:?}");
    };
    assert_eq!(
        request.get("method"),
        Some(&serde_json::json!("getAccount"))
    );
}

#[test]
fn sequencer_flag_needs_no_wallet_home_at_all() {
    let dir = tempfile::tempdir().unwrap();
    let idl = write_fixture_idl(dir.path());
    let sequencer = FakeSequencer::start();

    // No wallet_config.json, and a storage.json nothing must open.
    let wallet_home = dir.path().join("wallet");
    std::fs::create_dir(&wallet_home).unwrap();
    write_unreadable_storage(&wallet_home);

    let out = inspect(&wallet_home, &idl, &["--sequencer", &sequencer.url]);
    assert_decoded_counter(&out, &sequencer);
}

#[test]
fn wallet_home_contributes_only_the_sequencer_address() {
    let dir = tempfile::tempdir().unwrap();
    let idl = write_fixture_idl(dir.path());
    let sequencer = FakeSequencer::start();

    let wallet_home = dir.path().join("wallet");
    std::fs::create_dir(&wallet_home).unwrap();
    std::fs::write(
        wallet_home.join("wallet_config.json"),
        serde_json::json!({ "sequencers": [{ "sequencer_addr": sequencer.url }] }).to_string(),
    )
    .unwrap();
    write_unreadable_storage(&wallet_home);

    let out = inspect(&wallet_home, &idl, &[]);
    assert_decoded_counter(&out, &sequencer);
}

#[test]
fn without_a_sequencer_source_the_error_names_every_way_to_give_one() {
    let dir = tempfile::tempdir().unwrap();
    let idl = write_fixture_idl(dir.path());
    let wallet_home = dir.path().join("wallet");
    std::fs::create_dir(&wallet_home).unwrap();

    let out = inspect(&wallet_home, &idl, &[]);
    assert!(
        !out.status.success(),
        "inspect succeeded with nowhere to fetch from"
    );
    let stderr = stderr_of(&out);
    for hint in ["--sequencer", "wallet_config.json", "--data"] {
        assert!(
            stderr.contains(hint),
            "stderr does not mention `{hint}`:\n{stderr}"
        );
    }
}

#[test]
fn data_flag_decodes_without_any_request() {
    let dir = tempfile::tempdir().unwrap();
    let idl = write_fixture_idl(dir.path());
    let sequencer = FakeSequencer::start();
    let wallet_home = dir.path().join("wallet");
    std::fs::create_dir(&wallet_home).unwrap();

    let data = hex::encode(borsh::to_vec(&COUNTER_VALUE).unwrap_or_else(|e| panic!("borsh: {e}")));
    let out = inspect(
        &wallet_home,
        &idl,
        &["--sequencer", &sequencer.url, "--data", &data],
    );
    assert!(out.status.success(), "inspect failed:\n{}", stderr_of(&out));
    let decoded: serde_json::Value = serde_json::from_str(stdout_of(&out).trim())
        .unwrap_or_else(|e| panic!("stdout is not JSON: {e}\n{}", stdout_of(&out)));
    assert_eq!(
        decoded.get("value"),
        Some(&serde_json::json!(COUNTER_VALUE.to_string()))
    );
    assert!(
        sequencer.requests().is_empty(),
        "--data must not touch the network, saw {:?}",
        sequencer.requests()
    );
}

#[test]
fn sequencer_flag_without_a_value_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let idl = write_fixture_idl(dir.path());
    let wallet_home = dir.path().join("wallet");
    std::fs::create_dir(&wallet_home).unwrap();

    for args in [
        &["--sequencer"][..],
        &["--sequencer="][..],
        &["--sequencer", "--data"][..],
    ] {
        let out = inspect(&wallet_home, &idl, args);
        assert!(!out.status.success(), "{args:?} was accepted");
        let stderr = stderr_of(&out);
        assert!(
            stderr.contains("--sequencer requires a value"),
            "{args:?}: stderr does not say the value is missing:\n{stderr}"
        );
    }
}
