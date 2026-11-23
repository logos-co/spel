//! `spel send`: deliver one message to an actor on LEZ's actor model.
//!
//! ```text
//! spel send --idl token.json --root alice@token --signer alice -- transfer --to bob --amount 300
//! ```
//!
//! The message is encoded from the IDL. The access list holds the root, each `--actor`, and
//! (unless `--no-auto-actors`) an actor on the root's program for every account-typed field.
//! A `--signer` authorizes every actor of its account in the transaction; the guardrail
//! printed before sending lists them.
//!
//! The wallet simulates the transaction first and doesn't send one that would be refused, by the
//! program or by LEZ's rules; `send` prints the reason and exits with [`EXIT_NO_CHANGE`]. Otherwise it reads every
//! declared actor before and after inclusion and reports what changed. Inclusion with no change
//! (a no-op, or a refusal the simulation didn't foresee) also exits with [`EXIT_NO_CHANGE`].
//!
//! A call that declares a private account, or credits one with `--seal` / `--recover`, is a
//! privacy-preserving transaction ([`crate::private`]). Its access list isn't filled from the
//! message's fields: the wallet's simulation names each actor the call reaches that isn't
//! declared, and `send` adds it and simulates again.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::process;

use lee::AccountId;
use lee_core::account::Actor;
use lee_core::native_token::NATIVE_TOKEN_PROGRAM_ID;
use serde_json::{json, Value};
use spel_framework_core::decode::decode_state;
use spel_framework_core::idl::{to_kebab, IdlMessage, IdlType, SpelIdl};
use wallet::account::{AccountIdWithPrivacy, Label};
use wallet::config::WalletConfigOverrides;
use wallet::{AccountIdentity, AccountMention, ExecutionFailureKind, WalletCore};

/// The wallet over the sequencer's RPC client.
pub type Wallet = WalletCore<sequencer_service_rpc::SequencerClient>;

use crate::cli::{encode_message, field_flag, parse_message_args};
use crate::deploy::StdoutToStderr;
use crate::hex::{decode_bytes_32, hex_encode};

/// Exit code when nothing changed: the message was refused in simulation (so it wasn't sent), or
/// it was included and changed no declared actor (a no-op, or a late refusal).
pub const EXIT_NO_CHANGE: i32 = 2;

pub const USAGE: &str =
    "Usage: spel send --idl <IDL> --root <ACCOUNT>[@<PROGRAM>] [--actor <ACCOUNT>[@<PROGRAM>]]... \
     [--signer <ACCOUNT>]... [--seal <DEST>]... [--recover <DEST>]... [--no-auto-actors] \
     [--fee-payer <ACCOUNT>] [--gas-limit <N>] [--dry-run[=json]] [--] <MESSAGE> [--field value]...";

/// How many undeclared actors a private send adds from simulation before giving up.
const MAX_INFERRED: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DryRun {
    Text,
    Json,
}

/// The flags after `send`, as given.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SendFlags {
    pub root: String,
    pub actors: Vec<String>,
    pub signers: Vec<String>,
    /// Credits sealed to their recipient: sent by private execution.
    pub seals: Vec<String>,
    /// Credits published with a recovery binding: sent by public execution.
    pub recovers: Vec<String>,
    pub no_auto_actors: bool,
    pub gas_limit: Option<u64>,
    pub dry_run: Option<DryRun>,
    pub message: String,
    pub message_args: Vec<String>,
}

/// Parse the arguments after `send`: send flags up to the message name, then the message's
/// own `--field value` pairs. A `--` before the message name is optional.
///
/// # Errors
/// A missing `--root` or message name, an unknown flag, or a flag without its value.
pub fn parse_send_args(args: &[String]) -> Result<SendFlags, String> {
    let mut flags = SendFlags::default();
    let mut root = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let mut value = |flag: &str| {
            iter.next()
                .filter(|v| !v.starts_with("--"))
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match arg.as_str() {
            "--" => {},
            "--root" => root = Some(value("--root")?),
            "--actor" => flags.actors.push(value("--actor")?),
            "--signer" => flags.signers.push(value("--signer")?),
            "--seal" => flags.seals.push(value("--seal")?),
            "--recover" => flags.recovers.push(value("--recover")?),
            "--no-auto-actors" => flags.no_auto_actors = true,
            "--gas-limit" => {
                let raw = value("--gas-limit")?;
                flags.gas_limit = Some(
                    raw.parse()
                        .map_err(|e| format!("--gas-limit '{raw}': {e}"))?,
                );
            },
            "--dry-run" | "--dry-run=text" => flags.dry_run = Some(DryRun::Text),
            "--dry-run=json" => flags.dry_run = Some(DryRun::Json),
            flag if flag.starts_with('-') => return Err(format!("unknown send option '{flag}'")),
            message => {
                flags.message = message.to_owned();
                flags.message_args = iter.cloned().collect();
                break;
            },
        }
    }
    flags.root = root.ok_or("missing --root <ACCOUNT>[@<PROGRAM>]")?;
    if flags.message.is_empty() {
        return Err("missing the message to send".into());
    }
    Ok(flags)
}

/// `alice@token` → (`alice`, `Some("token")`).
pub fn split_actor(raw: &str) -> (&str, Option<&str>) {
    match raw.rsplit_once('@') {
        Some((account, program)) => (account, Some(program)),
        None => (raw, None),
    }
}

/// Turns names into ids and back: base58 or hex ids, wallet labels for accounts, `spel.toml`
/// names for programs.
pub struct Names<'a> {
    pub wallet: Option<&'a Wallet>,
    pub programs: Vec<(String, AccountId)>,
}

impl Names<'_> {
    /// An account's id, public or private.
    ///
    /// # Errors
    /// If `raw` is neither an id nor a label in the wallet.
    pub fn account(&self, raw: &str) -> Result<AccountId, String> {
        Ok(match self.account_with_privacy(raw)? {
            AccountIdWithPrivacy::Public(id) | AccountIdWithPrivacy::Private(id) => id,
        })
    }

    /// An account and whether it's private: `Private/<id>`, `Public/<id>`, a wallet label, or a
    /// bare id (private when this wallet can witness it as a private account).
    ///
    /// # Errors
    /// If `raw` is neither an id nor a label in the wallet.
    pub fn account_with_privacy(&self, raw: &str) -> Result<AccountIdWithPrivacy, String> {
        if let Ok(bytes) = decode_bytes_32(raw) {
            let id = AccountId::new(bytes);
            return Ok(if raw.starts_with("Private/") {
                AccountIdWithPrivacy::Private(id)
            } else if raw.starts_with("Public/") || !self.is_private(id) {
                AccountIdWithPrivacy::Public(id)
            } else {
                AccountIdWithPrivacy::Private(id)
            });
        }
        let wallet = self.wallet.ok_or_else(|| {
            format!("'{raw}' is not an account id, and no wallet is open to resolve it as a label")
        })?;
        wallet
            .storage()
            .resolve_label(&Label::new(raw))
            .ok_or_else(|| format!("'{raw}' is not an account id or a wallet label"))
    }

    /// Whether this wallet witnesses `id` as a private account.
    pub fn is_private(&self, id: AccountId) -> bool {
        self.wallet
            .is_some_and(|w| w.resolve_private_account(id).is_some())
    }

    /// A program: a `spel.toml` name, an id, or `native` (the native token program, whose actor
    /// on an account is its balance).
    ///
    /// # Errors
    /// If `raw` is neither an id nor a program named in `spel.toml`.
    pub fn program(&self, raw: &str) -> Result<AccountId, String> {
        if let Some((_, id)) = self.programs.iter().find(|(name, _)| name == raw) {
            return Ok(*id);
        }
        if raw == "native" {
            return Ok(NATIVE_TOKEN_PROGRAM_ID);
        }
        decode_bytes_32(raw).map(AccountId::new).map_err(|e| {
            format!("'{raw}' is not a program named in spel.toml, nor a program id: {e}")
        })
    }

    pub fn account_name(&self, id: AccountId) -> String {
        let label = self.wallet.and_then(|w| {
            [
                AccountIdWithPrivacy::Public(id),
                AccountIdWithPrivacy::Private(id),
            ]
            .into_iter()
            .find_map(|account| {
                w.storage()
                    .labels_for_account(account)
                    .next()
                    .map(|l| l.as_ref().to_owned())
            })
        });
        match label {
            Some(label) => format!("{label} ({id})"),
            None => id.to_string(),
        }
    }

    pub fn program_name(&self, id: AccountId) -> String {
        if id == NATIVE_TOKEN_PROGRAM_ID {
            return "native".into();
        }
        self.programs
            .iter()
            .find(|(_, p)| *p == id)
            .map_or_else(|| id.to_string(), |(name, _)| name.clone())
    }
}

/// One declared actor and why it is in the access list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    pub account: AccountId,
    pub program: AccountId,
    pub signs: bool,
    /// Witnessed through this wallet's keys in a privacy-preserving transaction.
    pub private: bool,
    pub reason: String,
}

/// The access list, root first. Rules:
/// - the root, then each explicit actor;
/// - unless `auto` is off, an actor on the root's program for each account field, except an
///   account that already has an actor in the list;
/// - an actor on the root's program for each signer whose account has none yet;
/// - an actor is declared once; it signs when its account is a signer.
pub fn plan_access_list(
    root: Actor,
    explicit: &[Actor],
    field_accounts: &[(String, AccountId)],
    signers: &[AccountId],
    auto: bool,
) -> Vec<Planned> {
    let mut list: Vec<Planned> = Vec::new();
    let add = |list: &mut Vec<Planned>, account: AccountId, program: AccountId, reason: String| {
        if !list
            .iter()
            .any(|p| p.account == account && p.program == program)
        {
            list.push(Planned {
                account,
                program,
                signs: signers.contains(&account),
                private: false,
                reason,
            });
        }
    };
    add(
        &mut list,
        root.account_id,
        root.program_account_id,
        "root".into(),
    );
    for actor in explicit {
        add(
            &mut list,
            actor.account_id,
            actor.program_account_id,
            "--actor".into(),
        );
    }
    if auto {
        for (field, account) in field_accounts {
            if !list.iter().any(|p| p.account == *account) {
                add(
                    &mut list,
                    *account,
                    root.program_account_id,
                    format!("field `{field}`"),
                );
            }
        }
    }
    for signer in signers {
        if !list.iter().any(|p| p.account == *signer) {
            add(
                &mut list,
                *signer,
                root.program_account_id,
                "--signer".into(),
            );
        }
    }
    list
}

/// The account-typed field values of `message` (`account_id`, or `Option<account_id>` when not
/// `none`), as given in `args` after label resolution.
fn account_fields(
    message: &IdlMessage,
    args: &HashMap<String, Vec<String>>,
) -> Vec<(String, AccountId)> {
    message
        .fields
        .iter()
        .filter(|f| is_account_field(&f.type_))
        .filter_map(|f| {
            let raw = args.get(&field_flag(&f.name))?.last()?;
            let bytes = decode_bytes_32(raw).ok()?;
            Some((f.name.clone(), AccountId::new(bytes)))
        })
        .collect()
}

fn is_account_field(ty: &IdlType) -> bool {
    match ty {
        IdlType::Primitive(p) => p == "account_id",
        IdlType::Option { option } => {
            matches!(option.as_ref(), IdlType::Primitive(p) if p == "account_id")
        },
        _ => false,
    }
}

/// Replace wallet labels in account-typed fields with their ids, so `--to bob` works.
fn resolve_account_fields(
    message: &IdlMessage,
    args: &mut HashMap<String, Vec<String>>,
    names: &Names<'_>,
) -> Result<(), String> {
    for field in message.fields.iter().filter(|f| is_account_field(&f.type_)) {
        let flag = field_flag(&field.name);
        let Some(values) = args.get_mut(&flag) else {
            continue;
        };
        for value in values.iter_mut() {
            if matches!(value.as_str(), "none" | "null" | "") {
                continue;
            }
            *value = names
                .account(value)
                .map_err(|e| format!("--{flag}: {e}"))?
                .to_string();
        }
    }
    Ok(())
}

/// Everything `send` would do, resolved.
struct Prepared {
    message_name: String,
    tag: u8,
    bytes: Vec<u8>,
    list: Vec<Planned>,
    signers: Vec<AccountId>,
    fee_payer: Option<AccountId>,
    /// Sent as a privacy-preserving transaction: an actor is private, or a credit is sealed or
    /// recoverable.
    private: bool,
}

fn prepare(
    flags: &SendFlags,
    idl: &SpelIdl,
    default_program: Option<&str>,
    fee_payer: Option<&str>,
    names: &Names<'_>,
) -> Result<Prepared, String> {
    let message = idl.message(&flags.message).ok_or_else(|| {
        let known: Vec<String> = idl.messages.iter().map(|m| to_kebab(&m.name)).collect();
        format!(
            "no message '{}' in the {} IDL (messages: {})",
            flags.message,
            idl.name,
            known.join(", ")
        )
    })?;
    let mut args = parse_message_args(&flags.message_args, message);
    resolve_account_fields(message, &mut args, names)?;
    let bytes = encode_message(idl, message, &args)?;

    let (root_account, root_program) = split_actor(&flags.root);
    let root_program = match root_program.or(default_program) {
        Some(program) => names.program(program)?,
        None => names.program(&idl.name).ok().ok_or_else(|| {
            format!(
                "--root {}: no program. Write {}@<PROGRAM>, pass --program, or name a \
                 [programs.{}] in spel.toml",
                flags.root, root_account, idl.name
            )
        })?,
    };
    let program_name = names.program_name(root_program);
    if names.programs.iter().any(|(name, _)| *name == program_name) && program_name != idl.name {
        return Err(format!(
            "the root is on the '{program_name}' program but the IDL is '{}'; pass the \
             {program_name} IDL",
            idl.name
        ));
    }
    // The accounts the caller declares, and which of them are private.
    let mut private_accounts = BTreeSet::new();
    let mut declared = |raw: &str| -> Result<AccountId, String> {
        Ok(match names.account_with_privacy(raw)? {
            AccountIdWithPrivacy::Public(id) => id,
            AccountIdWithPrivacy::Private(id) => {
                private_accounts.insert(id);
                id
            },
        })
    };
    let root = Actor::new(declared(root_account)?, root_program);
    let explicit = flags
        .actors
        .iter()
        .map(|raw| {
            let (account, program) = split_actor(raw);
            let program = match program {
                Some(p) => names.program(p)?,
                None => root_program,
            };
            Ok(Actor::new(declared(account)?, program))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let signers = flags
        .signers
        .iter()
        .map(|raw| match names.account_with_privacy(raw)? {
            AccountIdWithPrivacy::Public(id) => Ok(id),
            AccountIdWithPrivacy::Private(_) => Err(format!(
                "--signer {raw}: a private account authorizes with this wallet's keys, not a \
                 signature; drop --signer"
            )),
        })
        .collect::<Result<Vec<_>, String>>()?;
    let fee_payer = fee_payer.map(|raw| names.account(raw)).transpose()?;
    let private =
        !private_accounts.is_empty() || !flags.seals.is_empty() || !flags.recovers.is_empty();

    // A private call's undeclared actors come from simulation, not from the message's fields: a
    // field may name a private account no one in this transaction can witness.
    let mut list = plan_access_list(
        root,
        &explicit,
        &account_fields(message, &args),
        &signers,
        !flags.no_auto_actors && !private,
    );
    for planned in &mut list {
        planned.private = private_accounts.contains(&planned.account);
    }
    Ok(Prepared {
        message_name: message.name.clone(),
        tag: message.tag,
        bytes,
        list,
        signers,
        fee_payer,
        private,
    })
}

fn actor_name(names: &Names<'_>, account: AccountId, program: AccountId) -> String {
    format!(
        "({}, {})",
        names.account_name(account),
        names.program_name(program)
    )
}

/// The access list, then per signer every actor its signature authorizes.
fn print_guardrail(prepared: &Prepared, flags: &SendFlags, names: &Names<'_>) {
    eprintln!(
        "{} (tag {}, {} bytes) to {}{}",
        to_kebab(&prepared.message_name),
        prepared.tag,
        prepared.bytes.len(),
        actor_name(names, prepared.list[0].account, prepared.list[0].program),
        if prepared.private {
            ", as a privacy-preserving transaction"
        } else {
            ""
        }
    );
    eprintln!("Access list:");
    for planned in &prepared.list {
        eprintln!(
            "  {:<60} {}{}{}",
            actor_name(names, planned.account, planned.program),
            planned.reason,
            if planned.signs { ", signs" } else { "" },
            if planned.private {
                ", private: authorized by this wallet's keys"
            } else {
                ""
            }
        );
    }
    for seal in &flags.seals {
        eprintln!("Credit sealed to {seal}");
    }
    for recover in &flags.recovers {
        eprintln!("Credit to {recover}, with a recovery binding");
    }
    if prepared.private {
        // Private accounts authorize through the wallet's keys, not signatures.
    } else if prepared.signers.is_empty() {
        eprintln!("No signers: no actor is authorized.");
    }
    for signer in &prepared.signers {
        eprintln!(
            "{} signs. In this transaction that authorizes:",
            names.account_name(*signer)
        );
        for planned in prepared.list.iter().filter(|p| p.account == *signer) {
            let role = if planned.reason == "root" {
                "root"
            } else {
                "reachable: any program in the conversation can send it owner messages"
            };
            eprintln!(
                "  {:<60} {role}",
                actor_name(names, planned.account, planned.program)
            );
        }
    }
    if let Some(payer) = prepared.fee_payer {
        eprintln!("Fee payer: {}", names.account_name(payer));
    }
}

fn dry_run_json(prepared: &Prepared, flags: &SendFlags) -> Value {
    json!({
        "message": { "name": prepared.message_name, "tag": prepared.tag, "hex": hex_encode(&prepared.bytes) },
        "root": { "account": prepared.list[0].account.to_string(), "program": prepared.list[0].program.to_string() },
        "actors": prepared.list.iter().map(|p| json!({
            "account": p.account.to_string(),
            "program": p.program.to_string(),
            "authorizes": p.signs || p.private,
            "private": p.private,
            "reason": p.reason,
        })).collect::<Vec<_>>(),
        "signers": prepared.signers.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "fee_payer": prepared.fee_payer.map(|p| p.to_string()),
        "private": prepared.private,
        "seal": flags.seals,
        "recover": flags.recovers,
    })
}

/// Open the wallet `LEE_WALLET_HOME_DIR` names, with its password from `LEE_WALLET_PASSWORD` or a
/// prompt.
///
/// # Errors
/// If the wallet can't be found or opened.
pub fn open_wallet(gas_limit: Option<u64>) -> Result<Wallet, String> {
    let overrides = gas_limit.map(|gas_limit| WalletConfigOverrides {
        gas_limit: Some(gas_limit),
        ..Default::default()
    });
    let config_path = wallet::utils::fetch_config_path().map_err(|e| format!("{e:#}"))?;
    let storage_path =
        wallet::utils::fetch_persistent_storage_path().map_err(|e| format!("{e:#}"))?;
    let password = wallet::utils::read_password(&storage_path).map_err(|e| format!("{e:#}"))?;
    Wallet::new_update_chain(config_path, storage_path, overrides, &password)
        .map_err(|e| format!("{e:#}"))
}

async fn read_states(wallet: &Wallet, list: &[Planned]) -> Result<Vec<Vec<u8>>, String> {
    let mut states = Vec::with_capacity(list.len());
    for planned in list {
        let view = wallet
            .get_account_view(Actor::new(planned.account, planned.program))
            .await
            .map_err(|e| format!("reading {}: {e:#}", planned.account))?;
        states.push(
            view.map(|account| account.data.actor_state(planned.program).to_vec())
                .unwrap_or_default(),
        );
    }
    Ok(states)
}

/// An actor state as JSON when the IDL describes its program, else its size.
fn render_state(bytes: &[u8], program: AccountId, idl_program: AccountId, idl: &SpelIdl) -> String {
    if program == NATIVE_TOKEN_PROGRAM_ID {
        return lee_core::native_token::decode_balance(bytes).map_or_else(
            |_| format!("{} bytes", bytes.len()),
            |balance| format!("balance {balance}"),
        );
    }
    if bytes.is_empty() {
        return "none".into();
    }
    if program == idl_program {
        if let Ok(value) = decode_state(bytes, idl) {
            return value.to_string();
        }
    }
    format!("{} bytes", bytes.len())
}

/// Run `spel send`. Exits non-zero on any error, and with [`EXIT_NO_CHANGE`] when the message
/// was refused in simulation, reverted, or the included transaction changed no declared actor.
///
/// `binary` is the program's binary, which a privacy-preserving transaction is proved with.
pub async fn send_command(
    args: &[String],
    idl: &SpelIdl,
    default_program: Option<&str>,
    fee_payer: Option<&str>,
    programs: Vec<(String, AccountId)>,
    binary: Option<&str>,
) {
    let flags = parse_send_args(args).unwrap_or_else(|e| {
        eprintln!("❌ {e}");
        eprintln!("{USAGE}");
        process::exit(1);
    });

    // The wallet resolves labels and signs. A dry run works without one, on raw ids.
    let mut wallet = match open_wallet(flags.gas_limit) {
        Ok(wallet) => Some(wallet),
        Err(e) if flags.dry_run.is_some() => {
            eprintln!("⚠️  No wallet ({e}): labels won't resolve and signing keys aren't checked.");
            None
        },
        Err(e) => fail(format!(
            "Failed to open the wallet: {e}\n   Set LEE_WALLET_HOME_DIR"
        )),
    };
    let prepared = {
        let names = Names {
            wallet: wallet.as_ref(),
            programs: programs.clone(),
        };
        let prepared =
            prepare(&flags, idl, default_program, fee_payer, &names).unwrap_or_else(|e| fail(e));
        if let Some(wallet) = &wallet {
            for signer in &prepared.signers {
                if wallet.get_account_public_signing_key(*signer).is_none() {
                    fail(format!(
                        "--signer {}: this wallet doesn't hold its signing key",
                        names.account_name(*signer)
                    ));
                }
            }
        }
        match flags.dry_run {
            Some(DryRun::Json) => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&dry_run_json(&prepared, &flags))
                        .unwrap_or_default()
                );
                return;
            },
            Some(DryRun::Text) => {
                print_guardrail(&prepared, &flags, &names);
                eprintln!("Message: {}", hex_encode(&prepared.bytes));
                if prepared.private && !flags.no_auto_actors {
                    eprintln!(
                        "Private: the actors the call reaches beyond these are found when it's \
                         sent (the wallet simulates before proving)."
                    );
                }
                eprintln!("Dry run: nothing sent.");
                return;
            },
            None => print_guardrail(&prepared, &flags, &names),
        }
        prepared
    };
    let Some(wallet) = wallet.as_mut() else {
        unreachable!("only a dry run proceeds without a wallet")
    };
    if prepared.private {
        send_private(wallet, prepared, &flags, idl, programs, binary).await;
    } else {
        send_public(wallet, &prepared, idl, programs).await;
    }
}

fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("❌ {msg}");
    process::exit(1);
}

async fn send_public(
    wallet: &Wallet,
    prepared: &Prepared,
    idl: &SpelIdl,
    programs: Vec<(String, AccountId)>,
) {
    let names = Names {
        wallet: Some(wallet),
        programs,
    };
    let before = read_states(wallet, &prepared.list)
        .await
        .unwrap_or_else(|e| fail(e));
    let mentions: Vec<AccountMention> = prepared
        .list
        .iter()
        .map(|p| AccountMention {
            identity: if p.signs {
                AccountIdentity::Public(p.account)
            } else {
                AccountIdentity::PublicNoSign(p.account)
            },
            program_account_id: p.program,
            authorizes: p.signs,
            address_openings: BTreeSet::new(),
        })
        .collect();

    let (hash, block) = {
        // The wallet prints on stdout; keep stdout for the transaction hash.
        let _quiet = StdoutToStderr::new();
        let hash = match wallet
            .send_pub_tx_paid_by(mentions, 0, prepared.bytes.clone(), prepared.fee_payer)
            .await
        {
            Ok(hash) => hash,
            // The wallet simulates first and won't send a transaction that would be refused
            // (by the program or LEZ's rules), since it would still pay its fee.
            Err(ExecutionFailureKind::SimulatedRevert(reason)) => {
                eprintln!("⚠️  Refused in simulation, so not sent: {reason}");
                process::exit(EXIT_NO_CHANGE);
            },
            Err(e) => fail(format!("send refused by the wallet: {e:#}")),
        };
        eprintln!("Submitted {hash}, waiting for a block...");
        let (_, block) = wallet
            .poll_transaction(hash)
            .await
            .unwrap_or_else(|e| fail(format!("{hash} was not included: {e:#}")));
        (hash, block)
    };
    println!("{hash}");

    let after = read_states(wallet, &prepared.list)
        .await
        .unwrap_or_else(|e| fail(e));
    report_changes(&names, &prepared.list, &before, &after, block, idl);
}

/// A privacy-preserving transaction: proved with the program's binary, with private actors
/// witnessed through this wallet's keys. Undeclared actors the simulation reaches are added, one
/// at a time.
async fn send_private(
    wallet: &mut Wallet,
    mut prepared: Prepared,
    flags: &SendFlags,
    idl: &SpelIdl,
    programs: Vec<(String, AccountId)>,
    binary: Option<&str>,
) {
    let binary = binary.unwrap_or_else(|| {
        fail(
            "a private call is proved with the program's binary: pass --binary <FILE>, or name a \
             spel.toml program with a `binary`",
        )
    });
    let (catalog, _) = crate::private::catalog(Path::new(binary), prepared.list[0].program)
        .unwrap_or_else(|e| fail(e));

    let mut before = Vec::with_capacity(prepared.list.len());
    for planned in &prepared.list {
        before.push(
            crate::private::actor_state(wallet, planned.account, planned.program, planned.private)
                .await
                .unwrap_or_else(|e| fail(e)),
        );
    }
    let mut inferred = 0;
    let hash = loop {
        let names = Names {
            wallet: Some(&*wallet),
            programs: programs.clone(),
        };
        let mut mentions = prepared
            .list
            .iter()
            .map(|planned| mention(wallet, planned))
            .collect::<Result<Vec<_>, String>>()
            .unwrap_or_else(|e| fail(e));
        let destinations = |raws: &[String]| {
            raws.iter()
                .map(|raw| crate::private::destination(&names, raw))
                .collect::<Result<Vec<_>, String>>()
                .unwrap_or_else(|e| fail(e))
        };
        let (declared, delivery) = crate::private::delivery(
            wallet,
            destinations(&flags.recovers),
            destinations(&flags.seals),
        )
        .unwrap_or_else(|e| fail(e));
        mentions.extend(declared);

        let sent = {
            let _quiet = StdoutToStderr::new();
            wallet
                .send_tx_with_pre_check(
                    mentions,
                    0,
                    prepared.bytes.clone(),
                    &catalog,
                    None,
                    delivery,
                    prepared.fee_payer,
                    |_| Ok(()),
                )
                .await
        };
        let error = match sent {
            Ok((hash, _)) => break hash,
            Err(error) => error,
        };
        let reached = (!flags.no_auto_actors && inferred < MAX_INFERRED)
            .then(|| crate::private::undeclared_actor(&error))
            .flatten();
        let Some(actor) = reached else {
            if let Some(reason) = crate::private::refusal(&error) {
                eprintln!("⚠️  Refused in simulation, so not sent: {reason}");
                process::exit(EXIT_NO_CHANGE);
            }
            fail(format!("send refused by the wallet: {error:#}"))
        };
        let (account, program) = (actor.account_id, actor.program_account_id);
        if prepared
            .list
            .iter()
            .any(|p| p.account == account && p.program == program)
        {
            fail(format!(
                "the simulation reaches {}, which is declared: {error:#}",
                actor_name(&names, account, program)
            ));
        }
        let private = names.is_private(account);
        eprintln!(
            "  + {:<58} reached in simulation{}",
            actor_name(&names, account, program),
            if private { ", private" } else { "" }
        );
        before.push(
            crate::private::actor_state(wallet, account, program, private)
                .await
                .unwrap_or_else(|e| fail(e)),
        );
        prepared.list.push(Planned {
            account,
            program,
            signs: false,
            private,
            reason: "reached in simulation".into(),
        });
        inferred += 1;
    };
    eprintln!("Submitted {hash}, waiting for a block...");
    let (block, reverted) = crate::private::settle(wallet, hash)
        .await
        .unwrap_or_else(|e| fail(e));
    println!("{hash}");

    let names = Names {
        wallet: Some(&*wallet),
        programs,
    };
    if inferred > 0 {
        eprintln!(
            "The same call, declared: {}",
            explicit_command(&prepared, flags, &names)
        );
    }
    if reverted {
        eprintln!(
            "⚠️  Included in block {block}, but it reverted: its fee was charged and nothing else \
             applied."
        );
        process::exit(EXIT_NO_CHANGE);
    }
    let mut after = Vec::with_capacity(prepared.list.len());
    for planned in &prepared.list {
        after.push(
            crate::private::actor_state(wallet, planned.account, planned.program, planned.private)
                .await
                .unwrap_or_else(|e| fail(e)),
        );
    }
    report_changes(&names, &prepared.list, &before, &after, block, idl);
}

/// A declared actor of a privacy-preserving transaction.
fn mention(wallet: &Wallet, planned: &Planned) -> Result<AccountMention, String> {
    let identity = if planned.private {
        crate::private::private_identity(wallet, planned.account)?
    } else if planned.signs {
        AccountIdentity::Public(planned.account)
    } else {
        AccountIdentity::PublicNoSign(planned.account)
    };
    Ok(identity.select_program_actor_state(planned.program))
}

/// `spel send` with every actor declared and nothing inferred.
fn explicit_command(prepared: &Prepared, flags: &SendFlags, names: &Names<'_>) -> String {
    let actor = |p: &Planned| {
        let account = if p.private {
            format!("Private/{}", p.account)
        } else {
            p.account.to_string()
        };
        format!("{account}@{}", names.program_name(p.program))
    };
    let mut words = vec![
        "spel send".to_owned(),
        format!("--root {}", actor(&prepared.list[0])),
    ];
    for planned in &prepared.list[1..] {
        if planned.reason != "--signer" {
            words.push(format!("--actor {}", actor(planned)));
        }
    }
    words.extend(flags.signers.iter().map(|s| format!("--signer {s}")));
    words.extend(flags.seals.iter().map(|s| format!("--seal {s}")));
    words.extend(flags.recovers.iter().map(|s| format!("--recover {s}")));
    words.push("--no-auto-actors --".to_owned());
    words.push(flags.message.clone());
    words.extend(flags.message_args.iter().cloned());
    words.join(" ")
}

/// Each declared actor whose state changed, before and after. Exits with [`EXIT_NO_CHANGE`] when
/// none did.
fn report_changes(
    names: &Names<'_>,
    list: &[Planned],
    before: &[Vec<u8>],
    after: &[Vec<u8>],
    block: u64,
    idl: &SpelIdl,
) {
    let root_program = list[0].program;
    let changed: Vec<_> = list
        .iter()
        .zip(before.iter().zip(after))
        .filter(|(_, (b, a))| b != a)
        .collect();
    if changed.is_empty() {
        eprintln!(
            "⚠️  Included in block {block}, but no declared actor changed: a no-op, or the \
             program refused it after the simulation passed."
        );
        process::exit(EXIT_NO_CHANGE);
    }
    eprintln!("Included in block {block}. Changed:");
    for (planned, (b, a)) in changed {
        eprintln!("  {}", actor_name(names, planned.account, planned.program));
        eprintln!(
            "    before: {}",
            render_state(b, planned.program, root_program, idl)
        );
        eprintln!(
            "    after:  {}",
            render_state(a, planned.program, root_program, idl)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    fn id(byte: u8) -> AccountId {
        AccountId::new([byte; 32])
    }

    #[test]
    fn send_flags_end_at_the_message_name() {
        let flags = parse_send_args(&args(
            "--root alice@token --signer alice --actor usdc --dry-run=json -- transfer --to bob --amount 3",
        ))
        .unwrap();
        assert_eq!(flags.root, "alice@token");
        assert_eq!(flags.signers, ["alice"]);
        assert_eq!(flags.actors, ["usdc"]);
        assert_eq!(flags.dry_run, Some(DryRun::Json));
        assert_eq!(flags.message, "transfer");
        assert_eq!(flags.message_args, args("--to bob --amount 3"));
    }

    #[test]
    fn credits_repeat_and_stay_apart() {
        let flags = parse_send_args(&args(
            "--root seat@payroll --seal emp --seal keys:/tmp/b.keys --recover bob pay --employee emp",
        ))
        .unwrap();
        assert_eq!(flags.seals, ["emp", "keys:/tmp/b.keys"]);
        assert_eq!(flags.recovers, ["bob"]);
        assert_eq!(flags.message_args, args("--employee emp"));
    }

    #[test]
    fn native_names_the_balance_program() {
        let names = Names {
            wallet: None,
            programs: vec![],
        };
        assert_eq!(names.program("native").unwrap(), NATIVE_TOKEN_PROGRAM_ID);
        assert_eq!(names.program_name(NATIVE_TOKEN_PROGRAM_ID), "native");
        let id = id(4);
        assert_eq!(
            names
                .account_with_privacy(&format!("Private/{id}"))
                .unwrap(),
            AccountIdWithPrivacy::Private(id)
        );
        assert_eq!(
            names.account_with_privacy(&id.to_string()).unwrap(),
            AccountIdWithPrivacy::Public(id),
            "a bare id with no wallet to witness it privately is public"
        );
    }

    #[test]
    fn the_separator_is_optional() {
        let flags = parse_send_args(&args("--root a --no-auto-actors burn --amount 1")).unwrap();
        assert!(flags.no_auto_actors);
        assert_eq!(flags.message, "burn");
    }

    #[test]
    fn root_and_message_are_required() {
        assert!(parse_send_args(&args("transfer"))
            .unwrap_err()
            .contains("--root"));
        assert!(parse_send_args(&args("--root a"))
            .unwrap_err()
            .contains("message"));
        assert!(parse_send_args(&args("--root a --sign b x"))
            .unwrap_err()
            .contains("--sign"));
    }

    #[test]
    fn actor_splits_at_the_last_at() {
        assert_eq!(split_actor("alice@token"), ("alice", Some("token")));
        assert_eq!(split_actor("alice"), ("alice", None));
    }

    #[test]
    fn programs_resolve_by_config_name_or_id() {
        let names = Names {
            wallet: None,
            programs: vec![("token".into(), id(9))],
        };
        assert_eq!(names.program("token").unwrap(), id(9));
        assert_eq!(names.program(&id(3).to_string()).unwrap(), id(3));
        assert!(names.program("nft").is_err());
        assert!(names.account("alice").unwrap_err().contains("no wallet"));
    }

    /// Transfer from alice to bob, alice signing: root plus bob from the `to` field.
    #[test]
    fn fields_add_actors_on_the_roots_program() {
        let (alice, bob, token) = (id(1), id(2), id(9));
        let list = plan_access_list(
            Actor::new(alice, token),
            &[],
            &[("to".into(), bob)],
            &[alice],
            true,
        );
        assert_eq!(
            list,
            [
                Planned {
                    account: alice,
                    program: token,
                    signs: true,
                    private: false,
                    reason: "root".into()
                },
                Planned {
                    account: bob,
                    program: token,
                    signs: false,
                    private: false,
                    reason: "field `to`".into()
                },
            ]
        );
    }

    #[test]
    fn an_explicit_actor_replaces_the_field_default() {
        let (alice, bob, token, amm) = (id(1), id(2), id(9), id(8));
        let list = plan_access_list(
            Actor::new(alice, amm),
            &[Actor::new(bob, token)],
            &[("to".into(), bob)],
            &[],
            true,
        );
        assert_eq!(list.len(), 2);
        assert_eq!((list[1].account, list[1].program), (bob, token));
    }

    #[test]
    fn a_field_naming_the_root_adds_nothing() {
        let (definition, token) = (id(1), id(9));
        let list = plan_access_list(
            Actor::new(definition, token),
            &[],
            &[("mint_authority".into(), definition)],
            &[definition],
            true,
        );
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn no_auto_actors_keeps_only_root_explicit_and_signers() {
        let (alice, bob, carol, token) = (id(1), id(2), id(3), id(9));
        let list = plan_access_list(
            Actor::new(alice, token),
            &[],
            &[("to".into(), bob)],
            &[carol],
            false,
        );
        let accounts: Vec<_> = list
            .iter()
            .map(|p| (p.account, p.reason.as_str(), p.signs))
            .collect();
        assert_eq!(
            accounts,
            [(alice, "root", false), (carol, "--signer", true)]
        );
    }

    #[test]
    fn a_signer_authorizes_every_actor_of_its_account() {
        let (alice, token, amm) = (id(1), id(9), id(8));
        let list = plan_access_list(
            Actor::new(alice, amm),
            &[Actor::new(alice, token)],
            &[],
            &[alice],
            true,
        );
        assert!(list.iter().all(|p| p.signs));
        assert_eq!(list.len(), 2);
    }
}
