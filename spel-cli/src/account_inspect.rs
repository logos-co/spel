//! `spel inspect ACCOUNT[@PROGRAM]`: read an actor's state and decode it with the IDL.
//!
//! An account holds one state per program. With a program (`@PROGRAM`, `--program`, or the
//! `spel.toml` program named like the IDL) the actor's state is decoded as the IDL's state
//! type, or as `--type`. Without one, every program holding state on the account is listed.
//! `--data HEX` decodes offline.

use std::process;

use lee::AccountId;
use lee_core::account::Actor;
use serde_json::{json, Map, Value};
use spel_framework_core::decode;
use spel_framework_core::idl::SpelIdl;

use crate::hex::{hex_decode, hex_encode};
use crate::send::{open_wallet, split_actor, Names};

pub const USAGE: &str =
    "Usage: spel inspect <ACCOUNT>[@<PROGRAM>] [--idl <IDL>] [--type <TYPE>] [--data <HEX>]";

/// What to inspect, resolved from the command line.
pub struct InspectArgs<'a> {
    pub target: &'a str,
    pub idl: Option<&'a SpelIdl>,
    pub type_name: Option<&'a str>,
    pub data_hex: Option<&'a str>,
    pub default_program: Option<&'a str>,
    pub programs: Vec<(String, AccountId)>,
}

fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("❌ {msg}");
    process::exit(1);
}

/// Decode `data` as `type_name`, or as the IDL's state type.
fn decode(data: &[u8], idl: &SpelIdl, type_name: Option<&str>) -> Result<Value, String> {
    match type_name {
        Some(name) => decode::decode_named(data, name, idl).map_err(|e| {
            if idl.type_def(name).is_none() {
                let names: Vec<&str> = idl.types.iter().map(|t| t.name.as_str()).collect();
                format!("{e}. Types in the IDL: {}", names.join(", "))
            } else {
                e
            }
        }),
        None => decode::decode_state(data, idl),
    }
}

fn print_json(value: &Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_else(|e| fail(e))
    );
}

pub async fn inspect_command(args: InspectArgs<'_>) {
    if let Some(hex) = args.data_hex {
        let idl = args
            .idl
            .unwrap_or_else(|| fail("decoding --data needs --idl <IDL>"));
        let data = hex_decode(hex).unwrap_or_else(|e| fail(format!("--data: {e}")));
        let value = decode(&data, idl, args.type_name)
            .unwrap_or_else(|e| fail(format!("Borsh decode failed: {e}")));
        print_json(&value);
        return;
    }

    let wallet = open_wallet(None).unwrap_or_else(|e| {
        fail(format!(
            "Failed to open the wallet: {e}\n   Set LEE_WALLET_HOME_DIR"
        ))
    });
    let names = Names {
        wallet: Some(&wallet),
        programs: args.programs,
    };
    let (account, program) = split_actor(args.target);
    let account = names.account(account).unwrap_or_else(|e| fail(e));
    let program = program
        .or(args.default_program)
        .map(|p| names.program(p).unwrap_or_else(|e| fail(e)))
        .or_else(|| args.idl.and_then(|idl| names.program(&idl.name).ok()));

    let Some(program) = program else {
        list_actor_states(&wallet, &names, account).await;
        return;
    };

    let view = wallet
        .get_account_view(Actor::new(account, program))
        .await
        .unwrap_or_else(|e| fail(format!("reading {account}: {e:#}")));
    let state = view
        .map(|a| a.data.actor_state(program).to_vec())
        .unwrap_or_default();
    let actor = format!(
        "({}, {})",
        names.account_name(account),
        names.program_name(program)
    );
    if state.is_empty() {
        eprintln!("{actor}: no state");
        println!("null");
        return;
    }
    eprintln!("{actor}: {} bytes", state.len());
    match args.idl {
        Some(idl) => {
            check_idl_matches(idl, program, &names);
            let value = decode(&state, idl, args.type_name).unwrap_or_else(|e| {
                eprintln!("Hex: {}", hex_encode(&state));
                fail(format!("Borsh decode failed: {e}"))
            });
            print_json(&value);
        },
        None => println!("{}", hex_encode(&state)),
    }
}

/// Refuse to decode one program's state with another program's IDL: layouts can line up
/// and decode to plausible nonsense. Only checkable when spel.toml names the program.
fn check_idl_matches(idl: &SpelIdl, program: AccountId, names: &Names<'_>) {
    let program_name = names.program_name(program);
    let named = names.programs.iter().any(|(name, _)| *name == program_name);
    if named && program_name != idl.name {
        fail(format!(
            "the '{}' IDL doesn't describe the '{program_name}' program; pass the {program_name} IDL",
            idl.name
        ));
    }
}

/// Every program holding state on `account`, with the state as hex.
async fn list_actor_states(wallet: &wallet::WalletCore, names: &Names<'_>, account: AccountId) {
    let found = wallet
        .get_account_public(account)
        .await
        .unwrap_or_else(|e| fail(format!("reading {account}: {e:#}")));
    let mut states = Map::new();
    if let Some(found) = found {
        for (program, state) in &found.data.actor_states {
            if !state.is_empty() {
                states.insert(names.program_name(*program), json!(hex_encode(state)));
            }
        }
    }
    eprintln!(
        "{}: state under {} program(s). Name one (ACCOUNT@PROGRAM) to decode it.",
        names.account_name(account),
        states.len()
    );
    print_json(&Value::Object(states));
}
