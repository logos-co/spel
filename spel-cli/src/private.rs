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

use lee::{
    privacy_preserving_transaction::circuit::{Dependency, ProgramCatalog, ProgramKind},
    program::Program,
    AccountId,
};
use lee_core::account::Actor;
use lee_core::execution_state::ExecutionError;
use lee_core::native_token::NATIVE_TOKEN_PROGRAM_ID;
use lee_core::program::{PdaSeed, ProgramHeader, ProgramId, PROGRAM_LOADER_ACCOUNT_ID};
use lee_core::{NullifierPublicKey, PrivateAccountKind};
use wallet::account::{AccountIdWithPrivacy, Label};
use wallet::cli::{account::AccountSubcommand, execute_subcommand, Command};
use wallet::program_facades::program_loader::ProgramLoader;
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
    let guest = read_program(binary)?;
    Ok((ProgramCatalog::from([(program, guest.clone())]), guest))
}

/// The programs a privacy-preserving transaction runs. The wallet simulates the whole call, public
/// parts included, and proves its private transitions, so every program it reaches must be here
/// (the native token program is the protocol's own).
///
/// A program is loaded from a local binary whose ImageID matches the deployed header's, else from
/// the chain (its bytecode segments). A binary that isn't the deployed code is never used: its
/// proof would claim the wrong ImageID.
pub struct Programs {
    pub catalog: ProgramCatalog,
    /// `--binary` files, which must exist, then spel.toml binaries, which may not be built yet.
    explicit: Vec<String>,
    configured: Vec<String>,
    /// The local binaries, read on first need: path, ImageID, program, and whether it was used.
    local: Option<Vec<LocalBinary>>,
}

struct LocalBinary {
    path: String,
    image: ProgramId,
    program: Program,
    used: bool,
}

impl Programs {
    #[must_use]
    pub fn new(explicit: Vec<String>, configured: Vec<String>) -> Self {
        Self {
            catalog: ProgramCatalog::default(),
            explicit,
            configured,
            local: None,
        }
    }

    /// Whether `program` is ready: the native token program, or one already loaded.
    #[must_use]
    pub fn has(&self, program: AccountId) -> bool {
        program == NATIVE_TOKEN_PROGRAM_ID || self.catalog.programs.contains_key(&program)
    }

    /// Load `program`, and say where it came from: a local binary's path or "the chain".
    ///
    /// # Errors
    /// If a `--binary` can't be read, or the program isn't deployed through the program loader.
    pub async fn load(&mut self, wallet: &Wallet, program: AccountId) -> Result<String, String> {
        let header = wallet
            .get_account_view(Actor::new(program, PROGRAM_LOADER_ACCOUNT_ID))
            .await
            .map_err(|e| format!("reading program {program}'s header: {e:#}"))?
            .and_then(|account| {
                ProgramHeader::from_loader_actor_state(
                    account.data.actor_state(PROGRAM_LOADER_ACCOUNT_ID),
                )
            })
            .ok_or_else(|| {
                format!(
                    "{program} has no program header: it isn't a program deployed through the \
                     program loader, so spel can't load it"
                )
            })?;
        if let Some((path, guest)) = self.local_match(header.image_id)? {
            self.insert(program, guest);
            return Ok(path);
        }
        let deployed = ProgramLoader(wallet)
            .program(program)
            .await
            .map_err(|e| format!("loading program {program} from the chain: {e:#}"))?
            .ok_or_else(|| format!("program {program}'s bytecode isn't on chain"))?;
        self.insert(program, deployed);
        Ok("the chain".to_owned())
    }

    /// The `--binary` files no program of the call matched: stale builds, or another program's.
    #[must_use]
    pub fn unused(&self) -> Vec<&str> {
        self.local
            .iter()
            .flatten()
            .filter(|b| !b.used && self.explicit.contains(&b.path))
            .map(|b| b.path.as_str())
            .collect()
    }

    /// The local binary whose ImageID is `image`, marked used.
    fn local_match(&mut self, image: ProgramId) -> Result<Option<(String, Program)>, String> {
        Ok(self
            .local()?
            .iter_mut()
            .find(|binary| binary.image == image)
            .map(|binary| {
                binary.used = true;
                (binary.path.clone(), binary.program.clone())
            }))
    }

    fn insert(&mut self, address: AccountId, program: Program) {
        self.catalog.programs.insert(
            address,
            Dependency {
                program,
                kind: ProgramKind::Disclosed,
            },
        );
    }

    fn local(&mut self) -> Result<&mut Vec<LocalBinary>, String> {
        if self.local.is_none() {
            let mut local = Vec::new();
            let configured = self
                .configured
                .iter()
                .filter(|path| !self.explicit.contains(path) && Path::new(path).exists());
            for (path, required) in self
                .explicit
                .iter()
                .map(|path| (path, true))
                .chain(configured.map(|path| (path, false)))
            {
                match read_program(Path::new(path)) {
                    Ok(program) => local.push(LocalBinary {
                        path: path.clone(),
                        image: program.id(),
                        program,
                        used: false,
                    }),
                    Err(e) if required => return Err(e),
                    Err(e) => eprintln!("⚠️  spel.toml binary skipped: {e}"),
                }
            }
            self.local = Some(local);
        }
        Ok(self.local.get_or_insert_with(Vec::new))
    }
}

fn read_program(binary: &Path) -> Result<Program, String> {
    let bytes = std::fs::read(binary)
        .map_err(|e| format!("reading the program binary {}: {e}", binary.display()))?;
    Program::new(bytes.into())
        .map_err(|e| format!("{} is not a program binary: {e:?}", binary.display()))
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

/// A credit's private recipient, for `--seal` or `--recover`.
///
/// `keys:<FILE>` is a private account known by its keys (`wallet account show-keys` output). A
/// private account of this wallet is addressed by its keys too, so its credit waits for it rather
/// than applying in the transaction.
///
/// # Errors
/// If `raw` names nothing usable, or a public account: it has nothing to seal or recover, and is
/// declared as an actor instead (`--actor ACCOUNT@PROGRAM`, under the program its credit goes
/// to), so its credit applies in the transaction.
pub fn destination(names: &Names<'_>, raw: &str) -> Result<AccountMention, String> {
    if let Some(path) = raw.strip_prefix("keys:") {
        let (npk, vpk) = wallet::cli::read_keys_file(path).map_err(|e| format!("{raw}: {e:#}"))?;
        return Ok(foreign(npk, vpk));
    }
    match names.account_with_privacy(raw)? {
        AccountIdWithPrivacy::Public(_) => Err(public_destination(raw)),
        AccountIdWithPrivacy::Private(id) => {
            let wallet = names.wallet.ok_or("no wallet is open")?;
            let found = wallet
                .storage()
                .key_chain()
                .private_account(id)
                .ok_or_else(|| {
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

/// Why a public account can't be a `--seal` / `--recover` recipient, and what to do instead.
#[must_use]
pub fn public_destination(raw: &str) -> String {
    format!(
        "{raw} is a public account: its credit isn't sealed or recovered. Declare it under the \
         program the credit goes to, so the credit applies in the transaction: --actor \
         {raw}@<PROGRAM> (--actor {raw}@native for a native balance)"
    )
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
/// execution), `seal` sealed to the recipient (from private execution). Every destination is
/// private ([`destination`] refuses public ones), so the wallet declares no actor for them.
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
) -> Result<AsyncDelivery, String> {
    if recover.is_empty() && seal.is_empty() {
        return Ok(AsyncDelivery::default());
    }
    let (_, mut delivery) = wallet
        .async_destinations(recover, seal)
        .map_err(|e| format!("preparing the credits: {e:#}"))?;
    delivery.promotions.listed_only = true;
    Ok(delivery)
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

/// The program a simulation reached that isn't in the catalog. Typed, like [`undeclared_actor`].
#[must_use]
pub fn undeclared_program(error: &ExecutionFailureKind) -> Option<AccountId> {
    match error {
        ExecutionFailureKind::TransactionBuildError(
            lee::error::LeeError::InvalidProgramBehavior(
                lee::error::InvalidProgramBehaviorError::UndeclaredProgramDependency {
                    program_account_id,
                },
            ),
        ) => Some(*program_account_id),
        _ => None,
    }
}

/// The actor a simulation reached without it being declared. A private send's simulation returns
/// it typed: nothing turns it into text on the way.
#[must_use]
pub fn undeclared_actor(error: &ExecutionFailureKind) -> Option<Actor> {
    match error {
        ExecutionFailureKind::TransactionBuildError(
            lee::error::LeeError::InvalidProgramBehavior(
                lee::error::InvalidProgramBehaviorError::Execution(
                    ExecutionError::UndeclaredActor { actor },
                ),
            ),
        ) => Some(*actor),
        _ => None,
    }
}

/// The destination of an async message from private execution that nothing seals, from the
/// wallet's refusal: "No recipient material seals an async message to <id>". The wallet reports it
/// only as text (`InvalidInput`), so a reworded message just loses the hint.
#[must_use]
pub fn unsealed_destination(error: &str) -> Option<AccountId> {
    const MARKER: &str = "No recipient material seals an async message to ";
    let rest = &error[error.find(MARKER)? + MARKER.len()..];
    let id: String = rest
        .chars()
        .take_while(char::is_ascii_alphanumeric)
        .collect();
    id.parse().ok()
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
