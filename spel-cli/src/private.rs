//! The privacy-preserving path: private accounts, sealed and recoverable credits, and the private
//! PDAs a wallet operates.
//!
//! A call that names a private account, or a credit with `--seal` / `--recover`, is sent as a
//! privacy-preserving transaction. The wallet runs it locally, against the decrypted states it
//! holds, and proves it with the program's binary. Private accounts reach the transaction through
//! the wallet's keys:
//! - an account of the wallet's own key tree;
//! - a private PDA keyed by one of them (`spel pda --owner`);
//! - a private PDA keyed by a wallet group's secret (`spel pda --group`).
//!
//! The PDAs aren't derivable from what the wallet stores, so `spel pda --register` records them.

use std::path::Path;

use lee::{privacy_preserving_transaction::circuit::ProgramCatalog, program::Program, AccountId};
use lee_core::account::Actor;
use lee_core::execution_state::ExecutionError;
use lee_core::program::PdaSeed;
use lee_core::{NullifierPublicKey, PrivateAccountKind};
use wallet::account::{AccountIdWithPrivacy, Label};
use wallet::cli::{account::AccountSubcommand, execute_subcommand, Command};
use wallet::storage::key_chain::{SharedAccountDerivation, SharedAccountEntry};
use wallet::{AccountIdentity, AccountMention, AsyncDelivery, ExecutionFailureKind};

use crate::deploy::StdoutToStderr;
use crate::send::{Names, Wallet};

/// The program catalog a privacy-preserving transaction proves against: the program at its
/// deployed address, run from `binary`.
///
/// # Errors
/// If the binary can't be read or isn't a program binary.
pub fn catalog(binary: &Path, program: AccountId) -> Result<(ProgramCatalog, Program), String> {
    let bytes = std::fs::read(binary)
        .map_err(|e| format!("reading the program binary {}: {e}", binary.display()))?;
    let guest = Program::new(bytes.into())
        .map_err(|e| format!("{} is not a program binary: {e:?}", binary.display()))?;
    Ok((ProgramCatalog::from([(program, guest.clone())]), guest))
}

/// How a declared private account takes part: through the keys this wallet holds for it.
///
/// # Errors
/// If the wallet can't witness the account.
pub fn private_identity(wallet: &Wallet, account: AccountId) -> Result<AccountIdentity, String> {
    wallet.resolve_private_account(account).ok_or_else(|| {
        format!(
            "Private/{account} isn't an account this wallet can witness. A private PDA must be \
             registered first: spel pda <SEED> --owner <ACCOUNT> | --group <GROUP> --register"
        )
    })
}

/// A credit's destination, for `--seal` or `--recover`.
///
/// `keys:<FILE>` is a private account known by its keys (`wallet account show-keys` output). A
/// private account of this wallet is addressed by its keys too, so its credit waits for it rather
/// than applying in the transaction. A public account is declared.
///
/// # Errors
/// If `raw` names nothing usable.
pub fn destination(names: &Names<'_>, raw: &str) -> Result<AccountMention, String> {
    if let Some(path) = raw.strip_prefix("keys:") {
        let (npk, vpk) = wallet::cli::read_keys_file(path).map_err(|e| format!("{raw}: {e:#}"))?;
        return Ok(foreign(npk, vpk));
    }
    match names.account_with_privacy(raw)? {
        AccountIdWithPrivacy::Public(id) => Ok(AccountIdentity::PublicNoSign(id).balance()),
        AccountIdWithPrivacy::Private(id) => {
            let wallet = names.wallet.ok_or("no wallet is open")?;
            let found = wallet.storage().key_chain().private_account(id).ok_or_else(|| {
                format!(
                    "{raw}: this wallet holds no keys for Private/{id}; pass keys:<FILE> \
                     (`wallet account show-keys` output from its owner)"
                )
            })?;
            Ok(foreign(
                found.key_chain.nullifier_public_key,
                found.key_chain.viewing_public_key.clone(),
            ))
        },
    }
}

fn foreign(npk: NullifierPublicKey, vpk: lee_core::encryption::ViewingPublicKey) -> AccountMention {
    AccountIdentity::PrivateForeign {
        npk,
        vpk,
        kind: PrivateAccountKind::Regular,
    }
    .balance()
}

/// The credits of a call: `recover` with a recovery binding (an async message from public
/// execution), `seal` sealed to the recipient (from private execution). A public destination is
/// declared instead, and returned to add to the transaction.
///
/// Without credits, every async message to an account the transaction witnesses (a declared
/// private balance, say) is delivered in the transaction. With them, only the listed ones are, so
/// a credit to a witnessed account isn't applied in place of its seal.
///
/// # Errors
/// If a destination's keys can't be found.
pub fn delivery(
    wallet: &Wallet,
    recover: Vec<AccountMention>,
    seal: Vec<AccountMention>,
) -> Result<(Vec<AccountMention>, AsyncDelivery), String> {
    if recover.is_empty() && seal.is_empty() {
        return Ok((Vec::new(), AsyncDelivery::default()));
    }
    let (declared, mut delivery) = wallet
        .async_destinations(recover, seal)
        .map_err(|e| format!("preparing the credits: {e:#}"))?;
    delivery.promotions.listed_only = true;
    Ok((declared, delivery))
}

/// The program's refusal, when `error` is the guest panicking in the wallet's simulation: the
/// transaction was never sent.
#[must_use]
pub fn refusal(error: &ExecutionFailureKind) -> Option<String> {
    match error {
        ExecutionFailureKind::SimulatedRevert(reason) => Some(reason.to_string()),
        ExecutionFailureKind::TransactionBuildError(
            lee::error::LeeError::ProgramExecutionFailed(reason),
        ) => Some(reason.clone()),
        // A rule of LEZ the call breaks, such as moving more than a balance holds.
        ExecutionFailureKind::TransactionBuildError(
            lee::error::LeeError::InvalidProgramBehavior(cause),
        ) => Some(format!("{cause:?}")),
        _ => None,
    }
}

/// The actor a simulation reached without it being declared.
#[must_use]
pub fn undeclared_actor(error: &ExecutionFailureKind) -> Option<Actor> {
    let ExecutionFailureKind::TransactionBuildError(error) = error else {
        return None;
    };
    match error {
        lee::error::LeeError::InvalidProgramBehavior(
            lee::error::InvalidProgramBehaviorError::Execution(ExecutionError::UndeclaredActor {
                actor,
            }),
        ) => Some(*actor),
        _ => undeclared_actor_in_text(&format!("{error:?}")),
    }
}

/// `UndeclaredActor { actor: Actor { account_id: <id>, program_account_id: <id> } }`, read back from
/// an error that was turned into text on the way.
fn undeclared_actor_in_text(text: &str) -> Option<Actor> {
    let rest = &text[text.find("UndeclaredActor")?..];
    let field = |name: &str| -> Option<AccountId> {
        let start = rest.find(name)? + name.len();
        let value: String = rest[start..]
            .trim_start_matches([':', ' ', '"'])
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect();
        value.parse().ok()
    };
    Some(Actor::new(
        field("account_id")?,
        field("program_account_id")?,
    ))
}

/// Sync the wallet's private accounts with the chain, then save the wallet. `rescan` rediscovers
/// them from genesis (for an account registered after its history).
///
/// # Errors
/// If the sync or the save fails.
pub async fn sync(wallet: &mut Wallet, rescan: bool) -> Result<(), String> {
    let _quiet = StdoutToStderr::new();
    execute_subcommand(
        wallet,
        Command::Account(AccountSubcommand::SyncPrivate { rescan, yes: true }),
    )
    .await
    .map_err(|e| format!("syncing private accounts: {e:#}"))?;
    wallet
        .store_persistent_data()
        .map_err(|e| format!("saving the wallet: {e:#}"))
}

/// Waits for `hash`'s block, then syncs the private accounts. Returns the block and whether the
/// transaction reverted there (included and charged, with no other effect).
///
/// # Errors
/// If the transaction isn't included or the block can't be read.
pub async fn settle(wallet: &mut Wallet, hash: common::HashType) -> Result<(u64, bool), String> {
    let (_, block_id) = {
        let _quiet = StdoutToStderr::new();
        wallet
            .poll_transaction(hash)
            .await
            .map_err(|e| format!("{hash} was not included: {e:#}"))?
    };
    let block = wallet
        .get_block(block_id)
        .await
        .map_err(|e| format!("reading block {block_id}: {e:#}"))?
        .ok_or_else(|| format!("block {block_id} not found"))?;
    let reverted = block
        .body
        .transactions
        .iter()
        .position(|tx| tx.hash() == hash)
        .is_some_and(|position| block.body.is_reverted(position));
    sync(wallet, false).await?;
    Ok((block_id, reverted))
}

/// An actor's state: a private account's from the wallet's decrypted copy, a public one's from the
/// sequencer.
///
/// # Errors
/// If the sequencer can't be read.
pub async fn actor_state(
    wallet: &Wallet,
    account: AccountId,
    program: AccountId,
    private: bool,
) -> Result<Vec<u8>, String> {
    if private {
        return Ok(wallet
            .get_account_private(account)
            .map(|found| found.data.actor_state(program).to_vec())
            .unwrap_or_default());
    }
    let view = wallet
        .get_account_view(Actor::new(account, program))
        .await
        .map_err(|e| format!("reading {account}: {e:#}"))?;
    Ok(view
        .map(|found| found.data.actor_state(program).to_vec())
        .unwrap_or_default())
}

// ── spel pda, private ─────────────────────────────────────────────────────────

/// Whose keys a private PDA is derived with.
pub enum Keys<'a> {
    /// A private account of this wallet's key tree.
    Owner(&'a str),
    /// A wallet group: every holder of its secret derives the same PDA.
    Group(&'a str),
}

/// A private PDA, derived.
pub struct PrivatePda {
    pub account: AccountId,
    pub seed: PdaSeed,
    /// What the wallet records to witness it.
    entry: PdaEntry,
}

enum PdaEntry {
    Owned,
    Shared(Box<SharedAccountEntry>),
}

/// Derive `program`'s private PDA of `seed` under `keys`. A group's derivation needs the
/// program's image id, so its `binary`.
///
/// # Errors
/// If the owner or group isn't in the wallet, or the binary can't be read.
pub fn derive_private_pda(
    names: &Names<'_>,
    program: AccountId,
    seed: PdaSeed,
    keys: &Keys<'_>,
    binary: Option<&Path>,
) -> Result<PrivatePda, String> {
    let wallet = names.wallet.ok_or("no wallet is open")?;
    match keys {
        Keys::Owner(owner) => {
            let owner_id = match names.account_with_privacy(owner)? {
                AccountIdWithPrivacy::Private(id) => id,
                AccountIdWithPrivacy::Public(_) => {
                    return Err(format!("--owner {owner} is a public account"))
                },
            };
            let found = wallet
                .storage()
                .key_chain()
                .private_account(owner_id)
                .ok_or_else(|| format!("--owner {owner}: not a private account of this wallet"))?;
            let account = AccountId::for_private_pda(
                &program,
                &seed,
                &found.key_chain.nullifier_public_key,
                &found.key_chain.viewing_public_key,
            );
            Ok(PrivatePda {
                account,
                seed,
                entry: PdaEntry::Owned,
            })
        },
        Keys::Group(group) => {
            let binary = binary.ok_or(
                "--group derives from the program's image id: pass --binary, or name a spel.toml \
                 program with a `binary`",
            )?;
            let (_, guest) = catalog(binary, program)?;
            let image = guest.id();
            let label = Label::new(*group);
            let holder = wallet
                .storage()
                .key_chain()
                .group_key_holder(&label)
                .ok_or_else(|| format!("no wallet group `{group}` (wallet group new | join)"))?;
            let keys = holder.derive_keys_for_pda(&image, &seed);
            let account = AccountId::for_private_pda(
                &program,
                &seed,
                &keys.generate_nullifier_public_key(),
                &keys.generate_viewing_public_key(),
            );
            Ok(PrivatePda {
                account,
                seed,
                entry: PdaEntry::Shared(Box::new(SharedAccountEntry {
                    group_label: label,
                    derivation: SharedAccountDerivation::Pda {
                        seed,
                        program_id: image,
                    },
                    kind: PrivateAccountKind::Pda {
                        account_id: program,
                        seed,
                    },
                    account: lee::Account::default(),
                })),
            })
        },
    }
}

/// Record `pda` in the wallet, labelled `label` if the label is free, and save. Returns whether it
/// was new.
///
/// # Errors
/// If the wallet refuses the entry or can't be saved.
pub fn register(
    wallet: &mut Wallet,
    program: AccountId,
    pda: PrivatePda,
    label: Option<&str>,
) -> Result<bool, String> {
    let new = wallet.resolve_private_account(pda.account).is_none();
    if new {
        match pda.entry {
            PdaEntry::Owned => wallet
                .storage_mut()
                .key_chain_mut()
                .insert_private_account(
                    pda.account,
                    PrivateAccountKind::Pda {
                        account_id: program,
                        seed: pda.seed,
                    },
                    lee::Account::default(),
                )
                .map_err(|e| format!("registering Private/{}: {e:#}", pda.account))?,
            PdaEntry::Shared(entry) => wallet
                .storage_mut()
                .key_chain_mut()
                .insert_shared_private_account(pda.account, *entry),
        }
    }
    if let Some(label) = label {
        let label = Label::new(label);
        match wallet.storage().resolve_label(&label) {
            None => wallet
                .storage_mut()
                .add_label(label, AccountIdWithPrivacy::Private(pda.account))
                .map_err(|e| format!("labelling Private/{}: {e:#}", pda.account))?,
            Some(AccountIdWithPrivacy::Private(id)) if id == pda.account => {},
            Some(other) => eprintln!("⚠️  label `{label}` already names {other}; not relabelled"),
        }
    }
    wallet
        .store_persistent_data()
        .map_err(|e| format!("saving the wallet: {e:#}"))?;
    Ok(new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_undeclared_actor_is_read_back_from_text() {
        let (account, program) = (AccountId::new([1; 32]), AccountId::new([2; 32]));
        let text = format!(
            "InvalidProgramBehavior(Execution(UndeclaredActor {{ actor: Actor {{ account_id: \
             {account}, program_account_id: {program} }} }}))"
        );
        assert_eq!(
            undeclared_actor_in_text(&text),
            Some(Actor::new(account, program))
        );
        assert_eq!(undeclared_actor_in_text("ProgramExecutionFailed(\"x\")"), None);
    }

    #[test]
    fn a_typed_undeclared_actor_is_found() {
        let actor = Actor::new(AccountId::new([3; 32]), AccountId::new([4; 32]));
        let error = ExecutionFailureKind::TransactionBuildError(
            ExecutionError::UndeclaredActor { actor }.into(),
        );
        assert_eq!(undeclared_actor(&error), Some(actor));
        assert_eq!(
            undeclared_actor(&ExecutionFailureKind::KeyNotFoundError),
            None
        );
    }
}
