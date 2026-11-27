//! Public transactions that several wallets sign: `send --export`, `spel sign`, `spel submit`.
//!
//! A public transaction carries one signature per signer over its message, and the message
//! already holds what every signature commits to: each signer's nonce, the authorized accounts
//! and the fee. So the exporting wallet builds the whole message (fetching the co-signers'
//! nonces, measuring the fee with every signer authorized), signs for the keys it holds, and
//! writes a file instead of sending. Each co-signer runs `spel sign FILE`, which checks the
//! signatures already there, shows what the bytes say, and adds this wallet's. Once every signer
//! has signed, anyone runs `spel submit FILE`.
//!
//! The file is JSON:
//! - `version`: 2 (version 1 was the instruction-era format);
//! - `summary`: what the exporter meant to send, rendered by it. `sign` shows it, but decodes
//!   what the signature covers from `message` itself;
//! - `message`: the Borsh-encoded `public_transaction::Message`, hex. Signatures cover its hash;
//!   its signers are its nonces' accounts;
//! - `witnesses`: `{public_key, signature}` pairs, in the order they were added. The order
//!   doesn't matter to LEZ.
//!
//! A file goes stale when a signer's nonce moves (it sends something else) or the fee no longer
//! covers the base fees: submitting then fails, and the flow starts over with a fresh export.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::process;

use common::transaction::LeeTransaction;
use lee::privacy_preserving_transaction::circuit::{ProgramCatalog, Simulation};
use lee::public_transaction::{Message, WitnessSet};
use lee::{
    AccountId, FeeDeclaration, PublicAccountEvidence, PublicKey, PublicTransaction, Signature,
};
use lee_core::account::Actor;
use lee_core::native_token::NATIVE_TOKEN_PROGRAM_ID;
use lee_core::program::PROGRAM_LOADER_ACCOUNT_ID;
use sequencer_service_rpc::RpcClient as _;
use serde::{Deserialize, Serialize};
use spel_framework_core::decode::decode_message;
use spel_framework_core::idl::SpelIdl;

use crate::hex::{hex_decode, hex_encode};
use crate::send::{Names, Wallet, EXIT_NO_CHANGE};

/// The file format this spel reads and writes.
pub const VERSION: u32 = 2;

pub const SIGN_USAGE: &str = "Usage: spel sign <FILE> [--yes]";
pub const SUBMIT_USAGE: &str = "Usage: spel submit <FILE>";

/// One signature: the key that made it, and the signature over the message's hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Witness {
    pub public_key: String,
    pub signature: String,
}

/// A public transaction collecting its signatures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxFile {
    pub version: u32,
    pub summary: String,
    pub message: String,
    #[serde(default)]
    pub witnesses: Vec<Witness>,
}

impl TxFile {
    /// A file for `message`, with no signatures yet.
    #[must_use]
    pub fn new(message: &Message, summary: String) -> Self {
        Self {
            version: VERSION,
            summary,
            message: hex_encode(&message.to_bytes()),
            witnesses: Vec::new(),
        }
    }

    /// # Errors
    /// If the file can't be read, isn't a transaction file, or is another version.
    pub fn load(path: &str) -> Result<Self, String> {
        let text = fs::read_to_string(path).map_err(|e| format!("reading {path}: {e}"))?;
        let file: Self = serde_json::from_str(&text)
            .map_err(|e| format!("{path}: not a transaction file: {e}"))?;
        if file.version != VERSION {
            return Err(format!(
                "{path}: format version {}, this spel reads version {VERSION}",
                file.version
            ));
        }
        Ok(file)
    }

    /// # Errors
    /// If the file can't be written.
    pub fn save(&self, path: &str) -> Result<(), String> {
        let mut text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        text.push('\n');
        fs::write(path, text).map_err(|e| format!("writing {path}: {e}"))
    }

    /// The message the signatures cover. Its bytes must be its canonical encoding, so what a
    /// signer is shown is exactly what they sign.
    ///
    /// # Errors
    /// If `message` isn't the hex of an encoded message.
    pub fn message(&self) -> Result<Message, String> {
        let bytes = hex_decode(&self.message).map_err(|e| format!("message: {e}"))?;
        let message: Message =
            borsh::from_slice(&bytes).map_err(|e| format!("message: not a public message: {e}"))?;
        if message.to_bytes() != bytes {
            return Err("message: not canonically encoded".into());
        }
        Ok(message)
    }

    /// The signatures, each checked: it verifies over the message's hash, by a key whose account
    /// is a signer of it, at most one per signer.
    ///
    /// # Errors
    /// On the first signature that fails one of those checks.
    pub fn verified(&self, message: &Message) -> Result<BTreeMap<AccountId, Witness>, String> {
        let hash = message.hash();
        let mut verified = BTreeMap::new();
        for witness in &self.witnesses {
            let key: PublicKey = witness
                .public_key
                .parse()
                .map_err(|e| format!("a witness's public key '{}': {e}", witness.public_key))?;
            let account = AccountId::from(&key);
            let signature: Signature = witness
                .signature
                .parse()
                .map_err(|e| format!("{account}'s signature: {e}"))?;
            if !message.nonces.contains_key(&account) {
                return Err(format!(
                    "{account} signed, but isn't a signer of this transaction"
                ));
            }
            if !signature.is_valid_for(&hash, &key) {
                return Err(format!(
                    "{account}'s signature doesn't verify: the message changed after it was signed"
                ));
            }
            if verified.insert(account, witness.clone()).is_some() {
                return Err(format!("{account} signed twice"));
            }
        }
        Ok(verified)
    }

    /// Add this key's signature over `message`.
    pub fn sign(&mut self, message: &Message, key: &lee::PrivateKey) {
        self.witnesses.push(Witness {
            public_key: PublicKey::new_from_private_key(key).to_string(),
            signature: Signature::new(key, &message.hash()).to_string(),
        });
    }

    /// The transaction, once every signer has signed.
    ///
    /// # Errors
    /// If a signature doesn't verify, or a signer hasn't signed.
    pub fn transaction(&self) -> Result<PublicTransaction, String> {
        let message = self.message()?;
        let verified = self.verified(&message)?;
        let missing = missing(&message, &verified);
        if !missing.is_empty() {
            return Err(format!(
                "not signed by {}",
                missing
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let mut witnesses = Vec::with_capacity(verified.len());
        for witness in verified.values() {
            let key: PublicKey = witness.public_key.parse().map_err(|e| format!("{e}"))?;
            let signature: Signature = witness.signature.parse().map_err(|e| format!("{e}"))?;
            witnesses.push((signature, key));
        }
        let tx = PublicTransaction::new(message, WitnessSet::from_raw_parts(witnesses));
        tx.check_stateless().map_err(|e| e.to_string())?;
        Ok(tx)
    }
}

/// The signers that haven't signed.
#[must_use]
pub fn missing(message: &Message, verified: &BTreeMap<AccountId, Witness>) -> Vec<AccountId> {
    message
        .nonces
        .keys()
        .filter(|account| !verified.contains_key(account))
        .copied()
        .collect()
}

/// Why a transaction can't be built.
#[derive(Debug)]
pub enum BuildError {
    /// The simulation reverts: sent, it would be charged and change nothing.
    Refused(String),
    Failed(String),
}

/// What [`build`] measured.
#[derive(Debug)]
pub struct Built {
    pub message: Message,
    pub cycles: u64,
}

/// Build the public message of a call signed by `signers` (which must include `payer`), with its
/// fee measured as the wallet measures its own sends: simulated with every signer authorized,
/// priced at the base fees the configured headroom of full blocks could reach.
///
/// # Errors
/// If the sequencer can't be read, a program can't be loaded, or the simulation reverts.
pub async fn build(
    wallet: &Wallet,
    root: Actor,
    bytes: Vec<u8>,
    actors: &[Actor],
    signers: &BTreeSet<AccountId>,
    payer: AccountId,
    programs: &mut crate::private::Programs,
    names: &Names<'_>,
) -> Result<Built, BuildError> {
    let failed = BuildError::Failed;
    let mut declared: Vec<Actor> = Vec::new();
    for actor in std::iter::once(&root).chain(actors) {
        if !declared.contains(actor) {
            declared.push(*actor);
        }
    }

    // The declared actors' states, and which accounts exist.
    let mut states = HashMap::new();
    let mut present = BTreeSet::new();
    for actor in &declared {
        let view = wallet.get_account_view(*actor).await.map_err(|e| {
            failed(format!(
                "reading {}: {e:#}",
                names.account_name(actor.account_id)
            ))
        })?;
        if view.is_some() {
            present.insert(actor.account_id);
        }
        let state = view
            .map(|account| account.data.actor_state(actor.program_account_id).clone())
            .unwrap_or_default();
        states.insert(*actor, state);
    }

    // A new account is admitted by its signature, or by evidence that it's public: its key, when
    // this wallet holds it.
    let mut evidence = Vec::new();
    let mut admitted: BTreeSet<AccountId> = present.union(signers).copied().collect();
    for account in declared
        .iter()
        .map(|a| a.account_id)
        .collect::<BTreeSet<_>>()
    {
        if admitted.contains(&account) {
            continue;
        }
        if let Some(key) = wallet.get_account_public_signing_key(account) {
            evidence.push(PublicAccountEvidence::Key(PublicKey::new_from_private_key(
                key,
            )));
            admitted.insert(account);
        }
    }

    let signer_ids: Vec<AccountId> = signers.iter().copied().collect();
    let nonces = wallet
        .get_accounts_nonces(&signer_ids)
        .await
        .map_err(|e| failed(format!("reading the signers' nonces: {e:#}")))?;
    let nonces: BTreeMap<_, _> = signer_ids.into_iter().zip(nonces).collect();

    for program in declared
        .iter()
        .map(|a| a.program_account_id)
        .collect::<BTreeSet<_>>()
    {
        if program == NATIVE_TOKEN_PROGRAM_ID || program == PROGRAM_LOADER_ACCOUNT_ID {
            continue;
        }
        if !programs.has(program) {
            let from = programs.load(wallet, program).await.map_err(failed)?;
            eprintln!("  program {}: from {from}", names.program_name(program));
        }
    }

    let config = wallet.config();
    let budget = config.gas_limit.min(fee_core::market::MAX_GAS_EXEC);
    let mut message = Message::new(
        root,
        bytes,
        declared,
        nonces,
        Some(FeeDeclaration::new(payer, budget, 0, 0)),
        evidence,
    );
    let simulation = Simulation {
        public_actor_states: states,
        admitted_accounts: Some(admitted),
    };

    // Storage gas is priced on the signed transaction's wire size.
    let unsigned = PublicTransaction::new(message.clone(), WitnessSet::from_raw_parts(Vec::new()));
    let witness_bytes = size_of::<Signature>() + size_of::<PublicKey>();
    let wire_bytes = borsh::to_vec(&LeeTransaction::Public(unsigned.clone()))
        .map_err(|e| failed(e.to_string()))?
        .len()
        + signers.len() * witness_bytes;
    let wire_bytes = u64::try_from(wire_bytes).unwrap_or(u64::MAX);

    let cycles = simulate(&unsigned, &simulation, &programs.catalog, budget, None)?;
    let quote = wallet
        .client_owned()
        .get_fee_state()
        .await
        .map_err(|e| failed(format!("reading the fee state: {e:#}")))?;
    let gas_limit = cycles
        .saturating_mul(config.public_gas_margin_percent.saturating_add(100))
        .div_ceil(100)
        .min(budget);
    let (exec_ceiling, stor_ceiling) = base_fee_ceilings(&quote, config.fee_headroom_blocks);
    let max_fee = fee_core::assess::fee_for(gas_limit, wire_bytes, 0, exec_ceiling, stor_ceiling);

    // Settlement holds the reserve from the payer before the call runs: a call that spends the
    // payer's balance runs on what's left.
    let reserve = fee_core::assess::fee_for(
        gas_limit,
        wire_bytes,
        0,
        quote.base_fee_exec,
        quote.base_fee_stor,
    );
    if simulation
        .public_actor_states
        .contains_key(&Actor::native_balance(payer))
    {
        simulate(
            &unsigned,
            &simulation,
            &programs.catalog,
            gas_limit,
            Some((payer, reserve)),
        )?;
    }

    message.execution.fee = Some(FeeDeclaration::new(payer, gas_limit, 0, max_fee));
    Ok(Built { message, cycles })
}

/// Simulate `tx` within `budget` cycles, with `held` taken from its payer's balance first.
fn simulate(
    tx: &PublicTransaction,
    simulation: &Simulation,
    programs: &ProgramCatalog,
    budget: u64,
    held: Option<(AccountId, u128)>,
) -> Result<u64, BuildError> {
    let mut simulation = simulation.clone();
    if let Some((payer, amount)) = held {
        if let Some(state) = simulation
            .public_actor_states
            .get_mut(&Actor::native_balance(payer))
        {
            let balance = lee_core::native_token::decode_balance(state).unwrap_or_default();
            *state = lee_core::native_token::encode_balance(balance.saturating_sub(amount));
        }
    }
    let (cycles, result) = tokio::task::block_in_place(|| {
        lee::simulate_public_transaction(tx, &simulation, programs, budget)
    });
    match result {
        Ok(()) => Ok(cycles),
        Err(error) if error.is_chargeable() => Err(BuildError::Refused(format!(
            "{:#}",
            anyhow::Error::from(error)
        ))),
        Err(error) => Err(BuildError::Failed(format!(
            "{:#}",
            anyhow::Error::from(error)
        ))),
    }
}

/// The execution and storage base fees after `blocks` full blocks past `quote`: the steepest
/// climb the fee market allows. As the wallet prices its reserves.
fn base_fee_ceilings(quote: &sequencer_service_rpc::FeeStateQuote, blocks: u64) -> (u64, u64) {
    use fee_core::market::{
        next_base_fee, BASE_FEE_EXEC_MAX, BASE_FEE_EXEC_MIN, BASE_FEE_STOR_MAX, BASE_FEE_STOR_MIN,
        D_EXEC, D_STOR, MAX_GAS_EXEC, MAX_GAS_STOR, TARGET_GAS_EXEC, TARGET_GAS_STOR,
    };
    let climb = |base, max_gas, target, damping, lo, hi| {
        (0..blocks).fold(base, |fee, _| {
            next_base_fee(fee, max_gas, target, damping, lo, hi)
        })
    };
    (
        climb(
            quote.base_fee_exec,
            MAX_GAS_EXEC,
            TARGET_GAS_EXEC,
            D_EXEC,
            BASE_FEE_EXEC_MIN,
            BASE_FEE_EXEC_MAX,
        ),
        climb(
            quote.base_fee_stor,
            MAX_GAS_STOR,
            TARGET_GAS_STOR,
            D_STOR,
            BASE_FEE_STOR_MIN,
            BASE_FEE_STOR_MAX,
        ),
    )
}

/// Sign `file` for every signer whose key this wallet holds, write it to `path`, and say who's
/// left.
///
/// # Errors
/// If the file can't be written.
pub fn export(
    wallet: &Wallet,
    mut file: TxFile,
    path: &str,
    names: &Names<'_>,
) -> Result<(), String> {
    let message = file.message()?;
    for signer in message.nonces.keys() {
        if let Some(key) = wallet.get_account_public_signing_key(*signer) {
            file.sign(&message, key);
        }
    }
    file.save(path)?;
    let verified = file.verified(&message)?;
    eprintln!("✅ Written to {path}, not sent.");
    print_signers(&message, &verified, names);
    if missing(&message, &verified).is_empty() {
        eprintln!("Every signer signed: spel submit {path}");
    } else {
        eprintln!("Each missing signer runs: spel sign {path}");
    }
    Ok(())
}

fn print_signers(message: &Message, verified: &BTreeMap<AccountId, Witness>, names: &Names<'_>) {
    eprintln!("Signers:");
    for (account, nonce) in &message.nonces {
        eprintln!(
            "  {} {} (nonce {})",
            if verified.contains_key(account) {
                "✅ signed "
            } else {
                "⏳ missing"
            },
            names.account_name(*account),
            nonce.0
        );
    }
}

/// What a file's message says, decoded from its bytes, for the signer to check.
fn print_message(message: &Message, idl: Option<&SpelIdl>, names: &Names<'_>) {
    let root = &message.execution.root;
    let to = names.program_name(root.to.program_account_id);
    eprintln!(
        "To:      ({}, {to})",
        names.account_name(root.to.account_id)
    );
    // An IDL decodes the message only when it's the root program's, as far as spel.toml knows.
    let idl = idl.filter(|idl| {
        names
            .programs
            .iter()
            .all(|(_, address)| *address != root.to.program_account_id)
            || idl.name == to
    });
    match idl.map(|idl| (idl, decode_message(&root.message, idl))) {
        Some((idl, Ok((name, fields)))) => {
            eprintln!(
                "Message: {name} {fields} (decoded with the {} IDL)",
                idl.name
            );
        },
        Some((idl, Err(e))) => {
            eprintln!(
                "Message: {} (not a {} message: {e})",
                hex_encode(&root.message),
                idl.name
            );
        },
        None => eprintln!(
            "Message: {} (pass the program's IDL to decode it)",
            hex_encode(&root.message)
        ),
    }
    eprintln!("Actors:");
    for actor in &message.public_context.actors {
        let authorized = message
            .public_context
            .authorized_accounts
            .contains(&actor.account_id);
        eprintln!(
            "  ({}, {}){}",
            names.account_name(actor.account_id),
            names.program_name(actor.program_account_id),
            if authorized { ", authorized" } else { "" }
        );
    }
    match &message.execution.fee {
        Some(fee) => eprintln!(
            "Fee:     paid by {}, at most {} (gas limit {}, tip {})",
            names.account_name(fee.payer),
            fee.max_fee,
            fee.gas_limit,
            fee.tip
        ),
        None => eprintln!("Fee:     none"),
    }
}

fn fail(message: impl std::fmt::Display) -> ! {
    eprintln!("❌ {message}");
    process::exit(1);
}

/// `spel sign FILE [--yes]`.
pub fn sign_command(args: &[String], wallet: &Wallet, idl: Option<&SpelIdl>, names: &Names<'_>) {
    let mut path = None;
    let mut yes = false;
    for arg in args {
        match arg.as_str() {
            "--yes" | "-y" => yes = true,
            flag if flag.starts_with('-') => {
                fail(format!("unknown sign option '{flag}'\n{SIGN_USAGE}"))
            },
            file if path.is_none() => path = Some(file.to_owned()),
            _ => fail(SIGN_USAGE),
        }
    }
    let path = path.unwrap_or_else(|| fail(SIGN_USAGE));
    let mut file = TxFile::load(&path).unwrap_or_else(|e| fail(e));
    let message = file.message().unwrap_or_else(|e| fail(e));
    let verified = file.verified(&message).unwrap_or_else(|e| fail(e));

    eprintln!("The exporter's summary (unchecked):");
    for line in file.summary.lines() {
        eprintln!("  {line}");
    }
    eprintln!();
    eprintln!("What the signature covers, decoded from the message:");
    print_message(&message, idl, names);
    print_signers(&message, &verified, names);

    let unsigned = missing(&message, &verified);
    if unsigned.is_empty() {
        eprintln!("Every signer signed: spel submit {path}");
        return;
    }
    let signable: Vec<(AccountId, &lee::PrivateKey)> = unsigned
        .into_iter()
        .filter_map(|account| {
            wallet
                .get_account_public_signing_key(account)
                .map(|key| (account, key))
        })
        .collect();
    if signable.is_empty() {
        fail("this wallet holds the key of no missing signer");
    }
    let accounts = signable
        .iter()
        .map(|(account, _)| names.account_name(*account))
        .collect::<Vec<_>>()
        .join(", ");
    if !yes {
        if !io::stdin().is_terminal() {
            fail(format!(
                "not signed: confirm with --yes to sign for {accounts}"
            ));
        }
        eprint!("Sign for {accounts}? [y/N] ");
        io::stderr().flush().ok();
        let mut answer = String::new();
        io::stdin().lock().read_line(&mut answer).ok();
        if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
            eprintln!("Not signed, nothing written.");
            process::exit(1);
        }
    }
    for (_, key) in &signable {
        file.sign(&message, key);
    }
    file.save(&path).unwrap_or_else(|e| fail(e));
    let verified = file.verified(&message).unwrap_or_else(|e| fail(e));
    eprintln!("✅ Signed for {accounts}: {path}");
    if missing(&message, &verified).is_empty() {
        eprintln!("Every signer signed: spel submit {path}");
    } else {
        eprintln!("Pass it on to the missing signers: spel sign {path}");
    }
}

/// `spel submit FILE`: exit `0` included, `2` included but reverted, `1` otherwise.
pub async fn submit_command(args: &[String], wallet: &mut Wallet) {
    let [path] = args else { fail(SUBMIT_USAGE) };
    if path.starts_with('-') {
        fail(SUBMIT_USAGE);
    }
    let file = TxFile::load(path).unwrap_or_else(|e| fail(e));
    let tx = file.transaction().unwrap_or_else(|e| {
        fail(format!(
            "{path}: {e}; collect the rest with spel sign {path}"
        ))
    });
    let hash = wallet
        .client_owned()
        .send_transaction(LeeTransaction::Public(tx))
        .await
        .unwrap_or_else(|e| {
            fail(format!(
                "the sequencer refused it: {e:#}\n   A signer's nonce may have moved since the \
                 export, or the fee no longer covers the base fees: export it again"
            ))
        });
    eprintln!("Submitted {hash}, waiting for a block...");
    let (block, reverted) = crate::private::settle(wallet, hash)
        .await
        .unwrap_or_else(|e| fail(e));
    println!("{hash}");
    if reverted {
        eprintln!(
            "⚠️  Included in block {block}, but it reverted: its fee was charged and nothing else \
             applied."
        );
        process::exit(EXIT_NO_CHANGE);
    }
    eprintln!("Included in block {block}.");
}
