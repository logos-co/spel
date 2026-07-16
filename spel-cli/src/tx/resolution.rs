//! Fallible, resolver-only IDL transaction inputs.
//!
//! This module resolves canonical IDL names, account identities, PDA accounts, and
//! instruction data. It does not read files, initialize a wallet, build a
//! transaction, submit a transaction, poll, print, or terminate the process.

use std::collections::{BTreeMap, BTreeSet};

use crate::serialize::SerializeError;
use nssa::privacy_preserving_transaction::circuit::ProgramWithDependencies;
use nssa_core::program::{InstructionData, ProgramId};
use spel_framework_core::idl::{IdlInstruction, IdlSeed, SpelIdl};
use thiserror::Error;
use wallet::AccountIdentity;

use super::{
    pda_resolution,
    value::{self, ArgumentValue, ValueParseError},
};

/// Raw caller input for resolving one IDL instruction.
///
/// Construct this type with exact IDL account and argument names. The resolver
/// consumes it synchronously and follows IDL declaration order rather than map
/// order.
pub struct SpelInstructionRequest<'a> {
    /// IDL containing the instruction to resolve.
    pub idl: &'a SpelIdl,
    /// Exact IDL instruction name.
    pub instruction: &'a str,
    /// Exact IDL account names mapped to fixed or rest account identities.
    pub accounts: BTreeMap<String, Vec<AccountIdentity>>,
    /// Exact IDL argument names mapped to canonical or CLI-compatible input text.
    ///
    /// Canonical JSON is preferred for containers; established valid CLI forms
    /// remain accepted.
    pub arguments: BTreeMap<String, String>,
}

/// Resolved inputs for a public Wallet transaction build.
pub struct ResolvedPublicInstruction {
    program_id: ProgramId,
    accounts: Vec<AccountIdentity>,
    instruction_data: InstructionData,
}

impl ResolvedPublicInstruction {
    /// Returns the target program ID.
    #[must_use]
    pub const fn program_id(&self) -> ProgramId {
        self.program_id
    }

    /// Returns ordered Wallet account identities.
    #[must_use]
    pub fn accounts(&self) -> &[AccountIdentity] {
        &self.accounts
    }

    /// Returns the RISC0-serialized instruction words.
    #[must_use]
    pub fn instruction_data(&self) -> &[u32] {
        &self.instruction_data
    }

    /// Consumes the resolved instruction into Wallet build inputs.
    #[must_use]
    pub fn into_parts(self) -> (ProgramId, Vec<AccountIdentity>, InstructionData) {
        (self.program_id, self.accounts, self.instruction_data)
    }
}

/// Resolved inputs for a privacy-preserving Wallet transaction build.
pub struct ResolvedPrivateInstruction {
    program: ProgramWithDependencies,
    accounts: Vec<AccountIdentity>,
    instruction_data: InstructionData,
}

impl ResolvedPrivateInstruction {
    /// Returns the caller-supplied program and dependency binaries.
    #[must_use]
    pub const fn program(&self) -> &ProgramWithDependencies {
        &self.program
    }

    /// Returns ordered Wallet account identities.
    #[must_use]
    pub fn accounts(&self) -> &[AccountIdentity] {
        &self.accounts
    }

    /// Returns the RISC0-serialized instruction words.
    #[must_use]
    pub fn instruction_data(&self) -> &[u32] {
        &self.instruction_data
    }

    /// Consumes the resolved instruction into Wallet build inputs.
    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        ProgramWithDependencies,
        Vec<AccountIdentity>,
        InstructionData,
    ) {
        (self.program, self.accounts, self.instruction_data)
    }
}

/// Structured failure returned by the resolver-only transaction API.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum SpelTxError {
    /// No IDL instruction has the requested exact name.
    #[error("unknown instruction `{instruction}`")]
    UnknownInstruction {
        /// Requested instruction name.
        instruction: String,
    },
    /// More than one IDL instruction has the requested exact name.
    #[error("ambiguous instruction `{instruction}`")]
    AmbiguousInstruction {
        /// Requested instruction name.
        instruction: String,
        /// Number of matching instruction declarations.
        matches: usize,
    },
    /// The selected IDL instruction has an unsupported or malformed shape.
    #[error("invalid IDL for instruction `{instruction}` at `{path}`: {reason}")]
    InvalidIdl {
        /// Selected instruction name.
        instruction: String,
        /// IDL-relative path of the invalid declaration.
        path: String,
        /// Redacted, human-readable explanation.
        reason: String,
    },
    /// A required account input is absent.
    #[error("missing account `{name}`")]
    MissingAccount {
        /// Exact IDL account name.
        name: String,
    },
    /// An account input is unknown or forbidden for the IDL account shape.
    #[error("unexpected account `{name}`")]
    UnexpectedAccount {
        /// Exact caller-supplied account name.
        name: String,
    },
    /// An account input has the wrong number of identities.
    #[error("invalid account count for `{name}`: expected {expected}, got {actual}")]
    InvalidAccountCount {
        /// Exact IDL account name.
        name: String,
        /// Required identity count.
        expected: usize,
        /// Caller-supplied identity count.
        actual: usize,
    },
    /// An identity contradicts the selected resolver path or IDL account flags.
    #[error("invalid account `{account}` at index {index}: {reason}")]
    InvalidAccount {
        /// Exact IDL account name.
        account: String,
        /// Zero-based final account position.
        index: usize,
        /// Redacted, human-readable explanation.
        reason: String,
    },
    /// A required argument input is absent.
    #[error("missing argument `{name}`")]
    MissingArgument {
        /// Exact IDL argument name.
        name: String,
    },
    /// An argument input does not exist on the selected instruction.
    #[error("unexpected argument `{name}`")]
    UnexpectedArgument {
        /// Exact caller-supplied argument name.
        name: String,
    },
    /// An argument cannot be parsed as its selected IDL type.
    #[error("failed to parse argument `{name}` at {path:?}: {reason}")]
    ArgumentParse {
        /// Exact IDL argument name.
        name: String,
        /// Zero-based nested array/vector positions from outermost to innermost.
        path: Vec<usize>,
        /// Redacted, human-readable explanation.
        reason: String,
    },
    /// A PDA definition, dependency, or seed cannot be resolved.
    #[error("failed to resolve PDA `{account}`: {reason}")]
    PdaResolution {
        /// Exact IDL PDA account name.
        account: String,
        /// Zero-based seed position, or `None` for a whole-PDA failure.
        seed_index: Option<usize>,
        /// Redacted, human-readable explanation.
        reason: String,
    },
    /// Two final account positions resolve to the same account ID.
    #[error("duplicate resolved account `{account}` at index {index}")]
    DuplicateResolvedAccount {
        /// First IDL account name that resolved to this ID.
        first_account: String,
        /// First zero-based final account position.
        first_index: usize,
        /// Duplicate IDL account name.
        account: String,
        /// Duplicate zero-based final account position.
        index: usize,
    },
    /// RISC0 serialization of resolved instruction fields failed.
    #[error("failed to serialize instruction `{instruction}`")]
    InstructionSerialization {
        /// Selected instruction name.
        instruction: String,
        /// Concrete RISC0 serialization failure.
        #[source]
        source: SerializeError,
    },
}

/// Resolves one IDL instruction into direct public Wallet build inputs.
///
/// ```no_run
/// use std::collections::BTreeMap;
///
/// use nssa_core::program::ProgramId;
/// use spel::tx::{SpelInstructionRequest, resolve_public_instruction};
/// use spel_framework_core::idl::SpelIdl;
/// use wallet::WalletCore;
///
/// fn prepare(idl: &SpelIdl, program_id: ProgramId, wallet: &WalletCore) {
///     let request = SpelInstructionRequest {
///         idl,
///         instruction: "initialize",
///         accounts: BTreeMap::new(),
///         arguments: BTreeMap::new(),
///     };
///     let resolved = resolve_public_instruction(request, program_id).unwrap();
///     let (program_id, accounts, instruction_data) = resolved.into_parts();
///     let _build = wallet.build_pub_tx(accounts, instruction_data, program_id);
/// }
/// ```
///
/// # Errors
///
/// Returns [`SpelTxError`] when the selected IDL, caller input, account
/// identities, PDA dependencies, or instruction serialization is invalid.
pub fn resolve_public_instruction(
    request: SpelInstructionRequest<'_>,
    program_id: ProgramId,
) -> Result<ResolvedPublicInstruction, SpelTxError> {
    let resolved = resolve(request, program_id, ResolverPath::Public)?;
    Ok(ResolvedPublicInstruction {
        program_id,
        accounts: resolved.accounts,
        instruction_data: resolved.instruction_data,
    })
}

/// Resolves one IDL instruction into direct privacy-preserving Wallet build inputs.
///
/// ```no_run
/// use std::collections::BTreeMap;
///
/// use nssa::privacy_preserving_transaction::circuit::ProgramWithDependencies;
/// use spel::tx::{SpelInstructionRequest, resolve_private_instruction};
/// use spel_framework_core::idl::SpelIdl;
/// use wallet::WalletCore;
///
/// fn prepare(idl: &SpelIdl, program: ProgramWithDependencies, wallet: &WalletCore) {
///     let request = SpelInstructionRequest {
///         idl,
///         instruction: "initialize",
///         accounts: BTreeMap::new(),
///         arguments: BTreeMap::new(),
///     };
///     let resolved = resolve_private_instruction(request, program).unwrap();
///     let (program, accounts, instruction_data) = resolved.into_parts();
///     let _build = wallet.build_privacy_preserving_tx(accounts, instruction_data, &program);
/// }
/// ```
///
/// # Errors
///
/// Returns [`SpelTxError`] when the selected IDL, caller input, account
/// identities, PDA dependencies, or instruction serialization is invalid.
pub fn resolve_private_instruction(
    request: SpelInstructionRequest<'_>,
    program: ProgramWithDependencies,
) -> Result<ResolvedPrivateInstruction, SpelTxError> {
    let resolved = resolve(request, program.program.id(), ResolverPath::Private)?;
    Ok(ResolvedPrivateInstruction {
        program,
        accounts: resolved.accounts,
        instruction_data: resolved.instruction_data,
    })
}

enum ResolverPath {
    Public,
    Private,
}

impl ResolverPath {
    const fn is_private(&self) -> bool {
        matches!(self, Self::Private)
    }
}

struct ResolvedParts {
    accounts: Vec<AccountIdentity>,
    instruction_data: InstructionData,
}

fn resolve(
    request: SpelInstructionRequest<'_>,
    program_id: ProgramId,
    path: ResolverPath,
) -> Result<ResolvedParts, SpelTxError> {
    let SpelInstructionRequest {
        idl,
        instruction: instruction_name,
        accounts: inputs,
        arguments: raw_arguments,
    } = request;
    let (instruction_index, instruction) = select_instruction(idl, instruction_name)?;

    validate_selected_instruction(instruction, path.is_private())?;
    validate_account_inputs(instruction, &inputs)?;
    let arguments = parse_arguments(instruction, &raw_arguments)?;
    validate_identities(instruction, &inputs, path.is_private())?;
    let pdas = pda_resolution::resolve_pdas(
        instruction,
        &inputs,
        &arguments,
        program_id,
        path.is_private(),
    )?;
    let accounts = flatten_accounts(instruction, &inputs, &pdas)?;
    let variant_index = u32::try_from(instruction_index)
        .map_err(|_| invalid_idl(instruction, "instructions", "instruction index exceeds u32"))?;
    let fields: Vec<_> = instruction
        .args
        .iter()
        .map(|argument| {
            arguments
                .get(&argument.name)
                .ok_or_else(|| SpelTxError::MissingArgument {
                    name: argument.name.clone(),
                })
        })
        .collect::<Result<_, _>>()?;
    let instruction_data =
        value::serialize_instruction(variant_index, &fields).map_err(|source| {
            SpelTxError::InstructionSerialization {
                instruction: instruction.name.clone(),
                source,
            }
        })?;

    Ok(ResolvedParts {
        accounts,
        instruction_data,
    })
}

fn select_instruction<'a>(
    idl: &'a SpelIdl,
    instruction: &str,
) -> Result<(usize, &'a IdlInstruction), SpelTxError> {
    let matches: Vec<_> = idl
        .instructions
        .iter()
        .enumerate()
        .filter(|(_, candidate)| candidate.name == instruction)
        .collect();
    match matches.as_slice() {
        [] => Err(SpelTxError::UnknownInstruction {
            instruction: instruction.to_owned(),
        }),
        [(index, selected)] => Ok((*index, *selected)),
        _ => Err(SpelTxError::AmbiguousInstruction {
            instruction: instruction.to_owned(),
            matches: matches.len(),
        }),
    }
}

fn validate_selected_instruction(
    instruction: &IdlInstruction,
    private_path: bool,
) -> Result<(), SpelTxError> {
    let mut account_names = BTreeSet::new();
    let mut rest_seen = false;
    for (index, account) in instruction.accounts.iter().enumerate() {
        let path = format!("accounts[{index}]");
        if account.name.is_empty() {
            return Err(invalid_idl(instruction, &path, "account name is empty"));
        }
        if !account_names.insert(&account.name) {
            return Err(invalid_idl(instruction, &path, "duplicate account name"));
        }
        if account.rest {
            if rest_seen || index + 1 != instruction.accounts.len() {
                return Err(invalid_idl(
                    instruction,
                    &path,
                    "rest account must be the only final account",
                ));
            }
            if account.pda.is_some() {
                return Err(invalid_idl(
                    instruction,
                    &path,
                    "rest account cannot be a PDA",
                ));
            }
            rest_seen = true;
        }
        if account.pda.is_some() && account.signer {
            return Err(invalid_idl(
                instruction,
                &path,
                "PDA account cannot be a signer",
            ));
        }
        if account.init && !account.writable {
            return Err(invalid_idl(
                instruction,
                &path,
                "initialized account must be writable",
            ));
        }
        if let Some(pda) = &account.pda {
            if pda.private {
                if !private_path {
                    return Err(invalid_idl(
                        instruction,
                        &path,
                        "public resolution does not support private PDAs",
                    ));
                }
                if !account.init {
                    return Err(invalid_idl(
                        instruction,
                        &path,
                        "private PDA reuse is unsupported",
                    ));
                }
            }
        }
    }

    let mut argument_names = BTreeSet::new();
    for (index, argument) in instruction.args.iter().enumerate() {
        let path = format!("args[{index}]");
        if argument.name.is_empty() {
            return Err(invalid_idl(instruction, &path, "argument name is empty"));
        }
        if !argument_names.insert(&argument.name) {
            return Err(invalid_idl(instruction, &path, "duplicate argument name"));
        }
        value::validate_type(&argument.type_)
            .map_err(|reason| invalid_idl(instruction, &path, reason))?;
    }

    validate_pda_definitions(instruction)?;

    Ok(())
}

fn validate_pda_definitions(instruction: &IdlInstruction) -> Result<(), SpelTxError> {
    for account in &instruction.accounts {
        let Some(pda) = &account.pda else {
            continue;
        };
        if pda.seeds.is_empty() {
            return Err(pda_resolution_error(
                &account.name,
                None,
                "PDA requires at least one seed",
            ));
        }

        for (seed_index, seed) in pda.seeds.iter().enumerate() {
            match seed {
                IdlSeed::Const { value } if value.len() > 32 => {
                    return Err(pda_resolution_error(
                        &account.name,
                        Some(seed_index),
                        "constant seed exceeds 32 bytes",
                    ));
                },
                IdlSeed::Const { .. } => {},
                IdlSeed::Account { path } => {
                    let target = pda_resolution::account_seed_target(instruction, path)
                        .ok_or_else(|| {
                            pda_resolution_error(
                                &account.name,
                                Some(seed_index),
                                "account seed references an undeclared account",
                            )
                        })?;
                    if target.rest {
                        return Err(pda_resolution_error(
                            &account.name,
                            Some(seed_index),
                            "account seed cannot reference a rest account",
                        ));
                    }
                },
                IdlSeed::Arg { path } => {
                    let argument = instruction
                        .args
                        .iter()
                        .find(|argument| argument.name == *path)
                        .ok_or_else(|| {
                            pda_resolution_error(
                                &account.name,
                                Some(seed_index),
                                "argument seed references an undeclared argument",
                            )
                        })?;
                    value::validate_seed_type(&argument.type_).map_err(|reason| {
                        pda_resolution_error(&account.name, Some(seed_index), reason)
                    })?;
                },
            }
        }
    }
    Ok(())
}

fn validate_account_inputs(
    instruction: &IdlInstruction,
    inputs: &BTreeMap<String, Vec<AccountIdentity>>,
) -> Result<(), SpelTxError> {
    for name in inputs.keys() {
        let Some(account) = instruction
            .accounts
            .iter()
            .find(|account| account.name == *name)
        else {
            return Err(SpelTxError::UnexpectedAccount { name: name.clone() });
        };
        if matches!(&account.pda, Some(pda) if !pda.private) {
            return Err(SpelTxError::UnexpectedAccount { name: name.clone() });
        }
    }

    for account in &instruction.accounts {
        if account.rest || matches!(&account.pda, Some(pda) if !pda.private) {
            continue;
        }
        let Some(identities) = inputs.get(&account.name) else {
            return Err(SpelTxError::MissingAccount {
                name: account.name.clone(),
            });
        };
        if identities.len() != 1 {
            return Err(SpelTxError::InvalidAccountCount {
                name: account.name.clone(),
                expected: 1,
                actual: identities.len(),
            });
        }
    }

    Ok(())
}

fn parse_arguments(
    instruction: &IdlInstruction,
    raw_arguments: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, ArgumentValue>, SpelTxError> {
    for name in raw_arguments.keys() {
        if !instruction
            .args
            .iter()
            .any(|argument| argument.name == *name)
        {
            return Err(SpelTxError::UnexpectedArgument { name: name.clone() });
        }
    }

    let mut arguments = BTreeMap::new();
    for argument in &instruction.args {
        let raw =
            raw_arguments
                .get(&argument.name)
                .ok_or_else(|| SpelTxError::MissingArgument {
                    name: argument.name.clone(),
                })?;
        let value = value::parse_argument(raw, &argument.type_)
            .map_err(|error| argument_parse_error(&argument.name, error))?;
        arguments.insert(argument.name.clone(), value);
    }
    Ok(arguments)
}

fn validate_identities(
    instruction: &IdlInstruction,
    inputs: &BTreeMap<String, Vec<AccountIdentity>>,
    private_path: bool,
) -> Result<(), SpelTxError> {
    for (account_index, account) in instruction.accounts.iter().enumerate() {
        if account.pda.is_some() {
            continue;
        }
        let identities = inputs
            .get(&account.name)
            .map(Vec::as_slice)
            .unwrap_or_default();
        for (identity_index, identity) in identities.iter().enumerate() {
            let index = account_index + identity_index;
            if !private_path && !identity.is_public() {
                return Err(SpelTxError::InvalidAccount {
                    account: account.name.clone(),
                    index,
                    reason: "public resolution does not accept private identities".to_string(),
                });
            }
            if account.init && matches!(identity, AccountIdentity::PublicNoSign(_)) {
                return Err(SpelTxError::InvalidAccount {
                    account: account.name.clone(),
                    index,
                    reason: "initialized non-PDA account requires signing intent".to_string(),
                });
            }
            if account.signer && !identity_can_sign(identity) {
                return Err(SpelTxError::InvalidAccount {
                    account: account.name.clone(),
                    index,
                    reason: "signer account requires signing-capable identity intent".to_string(),
                });
            }
        }
    }
    Ok(())
}

fn flatten_accounts(
    instruction: &IdlInstruction,
    inputs: &BTreeMap<String, Vec<AccountIdentity>>,
    pdas: &BTreeMap<String, AccountIdentity>,
) -> Result<Vec<AccountIdentity>, SpelTxError> {
    let mut resolved = Vec::new();
    for account in &instruction.accounts {
        if account.pda.is_some() {
            let identity =
                pdas.get(&account.name)
                    .cloned()
                    .ok_or_else(|| SpelTxError::PdaResolution {
                        account: account.name.clone(),
                        seed_index: None,
                        reason: "PDA did not resolve".to_string(),
                    })?;
            resolved.push((account.name.clone(), identity));
        } else if account.rest {
            if let Some(identities) = inputs.get(&account.name) {
                resolved.extend(
                    identities
                        .iter()
                        .cloned()
                        .map(|identity| (account.name.clone(), identity)),
                );
            }
        } else {
            let identity = inputs
                .get(&account.name)
                .and_then(|identities| identities.first())
                .cloned()
                .ok_or_else(|| SpelTxError::MissingAccount {
                    name: account.name.clone(),
                })?;
            resolved.push((account.name.clone(), identity));
        }
    }

    let mut seen = Vec::<(String, usize, nssa::AccountId)>::new();
    for (index, (account, identity)) in resolved.iter().enumerate() {
        let account_id = identity.account_id();
        if let Some((first_account, first_index, _)) =
            seen.iter().find(|(_, _, seen_id)| *seen_id == account_id)
        {
            return Err(SpelTxError::DuplicateResolvedAccount {
                first_account: first_account.clone(),
                first_index: *first_index,
                account: account.clone(),
                index,
            });
        }
        seen.push((account.clone(), index, account_id));
    }

    Ok(resolved.into_iter().map(|(_, identity)| identity).collect())
}

fn identity_can_sign(identity: &AccountIdentity) -> bool {
    matches!(
        identity,
        AccountIdentity::Public(_)
            | AccountIdentity::PublicKeycard { .. }
            | AccountIdentity::PrivateOwned(_)
            | AccountIdentity::PrivatePdaOwned(_)
            | AccountIdentity::PrivateShared { .. }
            | AccountIdentity::PrivatePdaShared { .. }
    )
}

fn argument_parse_error(name: &str, error: ValueParseError) -> SpelTxError {
    SpelTxError::ArgumentParse {
        name: name.to_owned(),
        path: error.path,
        reason: error.reason,
    }
}

fn pda_resolution_error(
    account: &str,
    seed_index: Option<usize>,
    reason: impl Into<String>,
) -> SpelTxError {
    SpelTxError::PdaResolution {
        account: account.to_owned(),
        seed_index,
        reason: reason.into(),
    }
}

fn invalid_idl(
    instruction: &IdlInstruction,
    path: impl Into<String>,
    reason: impl Into<String>,
) -> SpelTxError {
    SpelTxError::InvalidIdl {
        instruction: instruction.name.clone(),
        path: path.into(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use nssa_core::{encryption::ViewingPublicKey, NullifierPublicKey};
    use risc0_zkvm::serde::Deserializer;
    use serde::Deserialize;
    use spel_framework_core::{
        idl::{IdlAccountItem, IdlArg, IdlPda, IdlSeed, IdlType},
        pda::{compute_pda, compute_private_pda_with_identifier, seed_from_str},
    };

    use super::*;

    fn program_id() -> ProgramId {
        [17; 8]
    }

    fn make_idl(instructions: Vec<IdlInstruction>) -> SpelIdl {
        let mut idl = SpelIdl::new("resolver-test");
        idl.instructions = instructions;
        idl
    }

    fn instruction(name: &str, accounts: Vec<IdlAccountItem>, args: Vec<IdlArg>) -> IdlInstruction {
        IdlInstruction {
            name: name.to_string(),
            accounts,
            args,
            discriminator: None,
            execution: None,
            variant: None,
        }
    }

    fn account(name: &str) -> IdlAccountItem {
        IdlAccountItem {
            name: name.to_string(),
            writable: false,
            signer: false,
            init: false,
            owner: None,
            pda: None,
            rest: false,
            visibility: vec![],
        }
    }

    fn pda_account(name: &str, private: bool, seeds: Vec<IdlSeed>) -> IdlAccountItem {
        let mut account = account(name);
        account.pda = Some(IdlPda { seeds, private });
        account
    }

    fn arg(name: &str, type_: IdlType) -> IdlArg {
        IdlArg {
            name: name.to_string(),
            type_,
        }
    }

    fn public_identity(byte: u8) -> AccountIdentity {
        AccountIdentity::Public(nssa::AccountId::new([byte; 32]))
    }

    fn identity_variants() -> Vec<AccountIdentity> {
        let vpk = ViewingPublicKey::from_seed(&[11; 32], &[12; 32]);
        vec![
            AccountIdentity::Public(nssa::AccountId::new([1; 32])),
            AccountIdentity::PublicNoSign(nssa::AccountId::new([2; 32])),
            AccountIdentity::PublicKeycard {
                account_id: nssa::AccountId::new([3; 32]),
                key_path: "m/44'/60'/0'/0/0".to_string(),
            },
            AccountIdentity::PrivateOwned(nssa::AccountId::new([4; 32])),
            AccountIdentity::PrivateForeign {
                npk: NullifierPublicKey([5; 32]),
                vpk: vpk.clone(),
                identifier: 5,
            },
            AccountIdentity::PrivatePdaOwned(nssa::AccountId::new([6; 32])),
            AccountIdentity::PrivatePdaForeign {
                account_id: nssa::AccountId::new([7; 32]),
                npk: NullifierPublicKey([7; 32]),
                vpk: vpk.clone(),
                identifier: 7,
            },
            AccountIdentity::PrivateShared {
                nsk: [8; 32],
                npk: NullifierPublicKey([8; 32]),
                vpk: vpk.clone(),
                identifier: 8,
            },
            AccountIdentity::PrivatePdaShared {
                account_id: nssa::AccountId::new([9; 32]),
                nsk: [9; 32],
                npk: NullifierPublicKey([9; 32]),
                vpk,
                identifier: 9,
            },
        ]
    }

    #[test]
    fn applies_public_and_private_identity_path_rules_to_all_wallet_variants() {
        let idl = make_idl(vec![instruction(
            "execute",
            vec![account("account")],
            vec![],
        )]);
        let identities = identity_variants();

        for identity in identities.iter().take(3) {
            assert!(resolve_public_instruction(
                SpelInstructionRequest {
                    idl: &idl,
                    instruction: "execute",
                    accounts: BTreeMap::from([("account".to_string(), vec![identity.clone()])]),
                    arguments: BTreeMap::new(),
                },
                program_id(),
            )
            .is_ok());
        }

        for identity in identities.iter().skip(3) {
            let error = resolve_public_instruction(
                SpelInstructionRequest {
                    idl: &idl,
                    instruction: "execute",
                    accounts: BTreeMap::from([("account".to_string(), vec![identity.clone()])]),
                    arguments: BTreeMap::new(),
                },
                program_id(),
            )
            .err()
            .expect("public resolution must reject a private identity");
            assert!(matches!(
                error,
                SpelTxError::InvalidAccount {
                    ref account,
                    index: 0,
                    ..
                } if account == "account"
            ));
        }

        for identity in identities {
            assert!(resolve(
                SpelInstructionRequest {
                    idl: &idl,
                    instruction: "execute",
                    accounts: BTreeMap::from([("account".to_string(), vec![identity])]),
                    arguments: BTreeMap::new(),
                },
                program_id(),
                ResolverPath::Private,
            )
            .is_ok());
        }
    }

    #[test]
    fn applies_the_signer_intent_matrix_to_all_wallet_variants() {
        let mut signer = account("signer");
        signer.signer = true;
        let idl = make_idl(vec![instruction("execute", vec![signer], vec![])]);

        for (index, identity) in identity_variants().into_iter().enumerate() {
            let result = resolve(
                SpelInstructionRequest {
                    idl: &idl,
                    instruction: "execute",
                    accounts: BTreeMap::from([("signer".to_string(), vec![identity])]),
                    arguments: BTreeMap::new(),
                },
                program_id(),
                ResolverPath::Private,
            );
            let accepted = matches!(index, 0 | 2 | 3 | 5 | 7 | 8);
            assert_eq!(result.is_ok(), accepted, "identity variant {index}");
            if !accepted {
                assert!(matches!(
                    result,
                    Err(SpelTxError::InvalidAccount {
                        ref account,
                        index: 0,
                        ..
                    }) if account == "signer"
                ));
            }
        }
    }

    #[test]
    fn resolves_in_idl_order_with_public_pda_and_rest_accounts() {
        let payer = public_identity(1);
        let member_one = public_identity(2);
        let member_two = public_identity(3);
        let state_seed = seed_from_str("state");
        let expected_state =
            AccountIdentity::PublicNoSign(compute_pda(&program_id(), &[&state_seed]));

        let mut rest = account("members");
        rest.rest = true;
        let idl = make_idl(vec![
            instruction("ignored", vec![], vec![]),
            instruction(
                "execute",
                vec![
                    account("payer"),
                    pda_account(
                        "state",
                        false,
                        vec![IdlSeed::Const {
                            value: "state".to_string(),
                        }],
                    ),
                    rest,
                ],
                vec![arg("amount", IdlType::Primitive("u32".to_string()))],
            ),
        ]);
        let accounts = BTreeMap::from([
            (
                "members".to_string(),
                vec![member_one.clone(), member_two.clone()],
            ),
            ("payer".to_string(), vec![payer.clone()]),
        ]);
        let arguments = BTreeMap::from([("amount".to_string(), "42".to_string())]);

        let resolved = resolve_public_instruction(
            SpelInstructionRequest {
                idl: &idl,
                instruction: "execute",
                accounts,
                arguments,
            },
            program_id(),
        )
        .unwrap();

        assert_eq!(resolved.program_id(), program_id());
        assert_eq!(
            resolved.accounts(),
            &[payer, expected_state, member_one, member_two]
        );
        assert_eq!(resolved.instruction_data(), &[1, 42]);
    }

    #[test]
    fn permits_empty_rest_inputs_and_rejects_malformed_rest_layouts() {
        let mut rest = account("remaining");
        rest.rest = true;
        let idl = make_idl(vec![instruction("execute", vec![rest], vec![])]);

        for accounts in [
            BTreeMap::new(),
            BTreeMap::from([("remaining".to_string(), vec![])]),
        ] {
            let resolved = resolve_public_instruction(
                SpelInstructionRequest {
                    idl: &idl,
                    instruction: "execute",
                    accounts,
                    arguments: BTreeMap::new(),
                },
                program_id(),
            )
            .unwrap();
            assert!(resolved.accounts().is_empty());
            assert_eq!(resolved.instruction_data(), &[0]);
        }

        let unknown = resolve_public_instruction(
            SpelInstructionRequest {
                idl: &idl,
                instruction: "execute",
                accounts: BTreeMap::from([("unknown".to_string(), vec![])]),
                arguments: BTreeMap::new(),
            },
            program_id(),
        )
        .err()
        .expect("resolver must reject unknown account input");
        assert!(matches!(
            unknown,
            SpelTxError::UnexpectedAccount { ref name } if name == "unknown"
        ));

        let mut malformed_rest = account("remaining");
        malformed_rest.rest = true;
        let malformed_idl = make_idl(vec![instruction(
            "malformed",
            vec![malformed_rest, account("after")],
            vec![],
        )]);
        let malformed = resolve_public_instruction(
            SpelInstructionRequest {
                idl: &malformed_idl,
                instruction: "malformed",
                accounts: BTreeMap::new(),
                arguments: BTreeMap::new(),
            },
            program_id(),
        )
        .err()
        .expect("resolver must reject non-final rest account");
        assert!(matches!(
            malformed,
            SpelTxError::InvalidIdl {
                ref instruction,
                ref path,
                ..
            } if instruction == "malformed" && path == "accounts[0]"
        ));
    }

    #[test]
    fn rejects_malformed_selected_idl_shapes_before_caller_input() {
        let mut signer_pda = pda_account(
            "state",
            false,
            vec![IdlSeed::Const {
                value: "state".to_string(),
            }],
        );
        signer_pda.signer = true;
        let signer_pda_idl = make_idl(vec![instruction("bad-pda", vec![signer_pda], vec![])]);
        let signer_pda_error = resolve_public_instruction(
            SpelInstructionRequest {
                idl: &signer_pda_idl,
                instruction: "bad-pda",
                accounts: BTreeMap::new(),
                arguments: BTreeMap::new(),
            },
            program_id(),
        )
        .err()
        .expect("resolver must reject a signer PDA");
        assert!(matches!(
            signer_pda_error,
            SpelTxError::InvalidIdl {
                ref instruction,
                ref path,
                ..
            } if instruction == "bad-pda" && path == "accounts[0]"
        ));

        let unsupported_type_idl = make_idl(vec![instruction(
            "bad-type",
            vec![],
            vec![arg(
                "value",
                IdlType::Defined {
                    defined: "Custom".to_string(),
                },
            )],
        )]);
        let unsupported_type = resolve_public_instruction(
            SpelInstructionRequest {
                idl: &unsupported_type_idl,
                instruction: "bad-type",
                accounts: BTreeMap::new(),
                arguments: BTreeMap::new(),
            },
            program_id(),
        )
        .err()
        .expect("resolver must reject defined instruction argument types");
        assert!(matches!(
            unsupported_type,
            SpelTxError::InvalidIdl {
                ref instruction,
                ref path,
                ..
            } if instruction == "bad-type" && path == "args[0]"
        ));

        let mut private_pda = pda_account(
            "state",
            true,
            vec![IdlSeed::Const {
                value: "state".to_string(),
            }],
        );
        private_pda.writable = true;
        let private_reuse_idl = make_idl(vec![instruction(
            "private-reuse",
            vec![private_pda],
            vec![],
        )]);
        let private_reuse = resolve(
            SpelInstructionRequest {
                idl: &private_reuse_idl,
                instruction: "private-reuse",
                accounts: BTreeMap::new(),
                arguments: BTreeMap::new(),
            },
            program_id(),
            ResolverPath::Private,
        )
        .err()
        .expect("top-level private PDA reuse is unsupported");
        assert!(matches!(
            private_reuse,
            SpelTxError::InvalidIdl {
                ref instruction,
                ref path,
                ..
            } if instruction == "private-reuse" && path == "accounts[0]"
        ));
    }

    #[test]
    fn resolves_exact_account_seed_name_before_id_alias() {
        let owner = public_identity(1);
        let literal_owner_id = public_identity(2);
        let idl = make_idl(vec![instruction(
            "derive",
            vec![
                account("owner"),
                account("owner.id"),
                pda_account(
                    "state",
                    false,
                    vec![IdlSeed::Account {
                        path: "owner.id".to_string(),
                    }],
                ),
            ],
            vec![],
        )]);
        let owner_id_seed = literal_owner_id.account_id().into_value();
        let expected = compute_pda(&program_id(), &[&owner_id_seed]);

        let resolved = resolve_public_instruction(
            SpelInstructionRequest {
                idl: &idl,
                instruction: "derive",
                accounts: BTreeMap::from([
                    ("owner".to_string(), vec![owner.clone()]),
                    ("owner.id".to_string(), vec![literal_owner_id.clone()]),
                ]),
                arguments: BTreeMap::new(),
            },
            program_id(),
        )
        .unwrap();

        assert_eq!(
            resolved.accounts(),
            &[
                owner,
                literal_owner_id,
                AccountIdentity::PublicNoSign(expected),
            ]
        );
    }

    #[test]
    fn ignores_malformed_unselected_instructions() {
        let idl = make_idl(vec![
            instruction("broken", vec![pda_account("bad", false, vec![])], vec![]),
            instruction("ready", vec![], vec![]),
        ]);

        let resolved = resolve_public_instruction(
            SpelInstructionRequest {
                idl: &idl,
                instruction: "ready",
                accounts: BTreeMap::new(),
                arguments: BTreeMap::new(),
            },
            program_id(),
        )
        .unwrap();

        assert_eq!(resolved.instruction_data(), &[1]);
    }

    #[test]
    fn reports_exact_instruction_selection_errors() {
        let duplicate_idl = make_idl(vec![
            instruction("same", vec![], vec![]),
            instruction("same", vec![], vec![]),
        ]);
        let ambiguous = resolve_public_instruction(
            SpelInstructionRequest {
                idl: &duplicate_idl,
                instruction: "same",
                accounts: BTreeMap::new(),
                arguments: BTreeMap::new(),
            },
            program_id(),
        )
        .err()
        .expect("resolver must reject invalid input");
        assert!(matches!(
            ambiguous,
            SpelTxError::AmbiguousInstruction {
                ref instruction,
                matches: 2,
            } if instruction == "same"
        ));

        let missing = resolve_public_instruction(
            SpelInstructionRequest {
                idl: &duplicate_idl,
                instruction: "other",
                accounts: BTreeMap::new(),
                arguments: BTreeMap::new(),
            },
            program_id(),
        )
        .err()
        .expect("resolver must reject invalid input");
        assert!(matches!(
            missing,
            SpelTxError::UnknownInstruction { ref instruction } if instruction == "other"
        ));
    }

    #[test]
    fn validates_account_input_keys_and_counts() {
        let idl = make_idl(vec![instruction(
            "execute",
            vec![
                account("payer"),
                pda_account(
                    "state",
                    false,
                    vec![IdlSeed::Const {
                        value: "state".to_string(),
                    }],
                ),
            ],
            vec![],
        )]);

        let missing = resolve_public_instruction(
            SpelInstructionRequest {
                idl: &idl,
                instruction: "execute",
                accounts: BTreeMap::new(),
                arguments: BTreeMap::new(),
            },
            program_id(),
        )
        .err()
        .expect("resolver must reject invalid input");
        assert!(matches!(missing, SpelTxError::MissingAccount { ref name } if name == "payer"));

        let public_pda_input = resolve_public_instruction(
            SpelInstructionRequest {
                idl: &idl,
                instruction: "execute",
                accounts: BTreeMap::from([("state".to_string(), vec![public_identity(1)])]),
                arguments: BTreeMap::new(),
            },
            program_id(),
        )
        .err()
        .expect("resolver must reject invalid input");
        assert!(matches!(
            public_pda_input,
            SpelTxError::UnexpectedAccount { ref name } if name == "state"
        ));

        let invalid_count = resolve_public_instruction(
            SpelInstructionRequest {
                idl: &idl,
                instruction: "execute",
                accounts: BTreeMap::from([("payer".to_string(), vec![])]),
                arguments: BTreeMap::new(),
            },
            program_id(),
        )
        .err()
        .expect("resolver must reject invalid input");
        assert!(matches!(
            invalid_count,
            SpelTxError::InvalidAccountCount {
                ref name,
                expected: 1,
                actual: 0,
            } if name == "payer"
        ));
    }

    #[test]
    fn validates_identity_intent_and_duplicate_resolved_accounts() {
        let private_identity = AccountIdentity::PrivateOwned(nssa::AccountId::new([1; 32]));
        let idl = make_idl(vec![instruction("execute", vec![account("payer")], vec![])]);
        let public_error = resolve_public_instruction(
            SpelInstructionRequest {
                idl: &idl,
                instruction: "execute",
                accounts: BTreeMap::from([("payer".to_string(), vec![private_identity])]),
                arguments: BTreeMap::new(),
            },
            program_id(),
        )
        .err()
        .expect("resolver must reject invalid input");
        assert!(matches!(
            public_error,
            SpelTxError::InvalidAccount {
                ref account,
                index: 0,
                ..
            } if account == "payer"
        ));

        let mut initialized = account("new_account");
        initialized.init = true;
        initialized.writable = true;
        let init_idl = make_idl(vec![instruction("init", vec![initialized], vec![])]);
        let init_error = resolve_public_instruction(
            SpelInstructionRequest {
                idl: &init_idl,
                instruction: "init",
                accounts: BTreeMap::from([(
                    "new_account".to_string(),
                    vec![AccountIdentity::PublicNoSign(nssa::AccountId::new([2; 32]))],
                )]),
                arguments: BTreeMap::new(),
            },
            program_id(),
        )
        .err()
        .expect("resolver must reject invalid input");
        assert!(matches!(
            init_error,
            SpelTxError::InvalidAccount {
                ref account,
                index: 0,
                ..
            } if account == "new_account"
        ));

        let mut rest = account("remaining");
        rest.rest = true;
        let duplicate_idl = make_idl(vec![instruction(
            "duplicate",
            vec![account("first"), rest],
            vec![],
        )]);
        let duplicate = public_identity(7);
        let duplicate_error = resolve_public_instruction(
            SpelInstructionRequest {
                idl: &duplicate_idl,
                instruction: "duplicate",
                accounts: BTreeMap::from([
                    ("first".to_string(), vec![duplicate.clone()]),
                    ("remaining".to_string(), vec![duplicate]),
                ]),
                arguments: BTreeMap::new(),
            },
            program_id(),
        )
        .err()
        .expect("resolver must reject invalid input");
        assert!(matches!(
            duplicate_error,
            SpelTxError::DuplicateResolvedAccount {
                ref first_account,
                first_index: 0,
                ref account,
                index: 1,
            } if first_account == "first" && account == "remaining"
        ));
    }

    #[test]
    fn private_resolution_accepts_private_non_pda_and_checks_private_pda_identity() {
        let owned = AccountIdentity::PrivateOwned(nssa::AccountId::new([4; 32]));
        let non_pda_idl = make_idl(vec![instruction("private", vec![account("owner")], vec![])]);
        let resolved = resolve(
            SpelInstructionRequest {
                idl: &non_pda_idl,
                instruction: "private",
                accounts: BTreeMap::from([("owner".to_string(), vec![owned.clone()])]),
                arguments: BTreeMap::new(),
            },
            program_id(),
            ResolverPath::Private,
        )
        .unwrap();
        assert_eq!(resolved.accounts, vec![owned]);

        let mut private_pda = pda_account(
            "vault",
            true,
            vec![IdlSeed::Const {
                value: "vault".to_string(),
            }],
        );
        private_pda.init = true;
        private_pda.writable = true;
        let private_pda_idl = make_idl(vec![instruction("create", vec![private_pda], vec![])]);
        let seed = seed_from_str("vault");
        let npk = NullifierPublicKey([5; 32]);
        let vpk = ViewingPublicKey::from_seed(&[6; 32], &[7; 32]);
        let identifier = 9;
        let account_id =
            compute_private_pda_with_identifier(&program_id(), &[&seed], &npk, &vpk, identifier);
        let identity = AccountIdentity::PrivatePdaForeign {
            account_id,
            npk,
            vpk,
            identifier,
        };

        let missing = resolve(
            SpelInstructionRequest {
                idl: &private_pda_idl,
                instruction: "create",
                accounts: BTreeMap::new(),
                arguments: BTreeMap::new(),
            },
            program_id(),
            ResolverPath::Private,
        )
        .err()
        .expect("private PDA input is required");
        assert!(matches!(missing, SpelTxError::MissingAccount { ref name } if name == "vault"));

        let invalid_count = resolve(
            SpelInstructionRequest {
                idl: &private_pda_idl,
                instruction: "create",
                accounts: BTreeMap::from([(
                    "vault".to_string(),
                    vec![identity.clone(), identity.clone()],
                )]),
                arguments: BTreeMap::new(),
            },
            program_id(),
            ResolverPath::Private,
        )
        .err()
        .expect("private PDA input must have exactly one identity");
        assert!(matches!(
            invalid_count,
            SpelTxError::InvalidAccountCount {
                ref name,
                expected: 1,
                actual: 2,
            } if name == "vault"
        ));

        let unsupported_identity = resolve(
            SpelInstructionRequest {
                idl: &private_pda_idl,
                instruction: "create",
                accounts: BTreeMap::from([(
                    "vault".to_string(),
                    vec![AccountIdentity::PrivatePdaOwned(account_id)],
                )]),
                arguments: BTreeMap::new(),
            },
            program_id(),
            ResolverPath::Private,
        )
        .err()
        .expect("owned private PDA initialization is unsupported");
        assert!(matches!(
            unsupported_identity,
            SpelTxError::InvalidAccount {
                ref account,
                index: 0,
                ..
            } if account == "vault"
        ));

        let resolved = resolve(
            SpelInstructionRequest {
                idl: &private_pda_idl,
                instruction: "create",
                accounts: BTreeMap::from([("vault".to_string(), vec![identity.clone()])]),
                arguments: BTreeMap::new(),
            },
            program_id(),
            ResolverPath::Private,
        )
        .unwrap();
        assert_eq!(resolved.accounts, vec![identity]);

        let mismatch = resolve(
            SpelInstructionRequest {
                idl: &private_pda_idl,
                instruction: "create",
                accounts: BTreeMap::from([(
                    "vault".to_string(),
                    vec![AccountIdentity::PrivatePdaForeign {
                        account_id: nssa::AccountId::new([8; 32]),
                        npk: NullifierPublicKey([5; 32]),
                        vpk: ViewingPublicKey::from_seed(&[6; 32], &[7; 32]),
                        identifier,
                    }],
                )]),
                arguments: BTreeMap::new(),
            },
            program_id(),
            ResolverPath::Private,
        )
        .err()
        .expect("resolver must reject invalid input");
        assert!(matches!(
            mismatch,
            SpelTxError::InvalidAccount {
                ref account,
                index: 0,
                ..
            } if account == "vault"
        ));
    }

    #[test]
    fn resolves_pda_dependencies_and_rejects_cycles_and_empty_seed_lists() {
        let idl = make_idl(vec![instruction(
            "derive",
            vec![
                account("payer"),
                pda_account(
                    "second",
                    false,
                    vec![
                        IdlSeed::Account {
                            path: "first".to_string(),
                        },
                        IdlSeed::Account {
                            path: "payer.id".to_string(),
                        },
                        IdlSeed::Arg {
                            path: "sequence".to_string(),
                        },
                    ],
                ),
                pda_account(
                    "first",
                    false,
                    vec![IdlSeed::Const {
                        value: "first".to_string(),
                    }],
                ),
            ],
            vec![arg("sequence", IdlType::Primitive("u16".to_string()))],
        )]);
        let payer = public_identity(3);
        let first_seed = seed_from_str("first");
        let first = compute_pda(&program_id(), &[&first_seed]);
        let first_value = first.into_value();
        let payer_value = payer.account_id().into_value();
        let mut sequence_seed = [0; 32];
        sequence_seed[..2].copy_from_slice(&513_u16.to_le_bytes());
        let second = compute_pda(&program_id(), &[&first_value, &payer_value, &sequence_seed]);

        let resolved = resolve_public_instruction(
            SpelInstructionRequest {
                idl: &idl,
                instruction: "derive",
                accounts: BTreeMap::from([("payer".to_string(), vec![payer.clone()])]),
                arguments: BTreeMap::from([("sequence".to_string(), "513".to_string())]),
            },
            program_id(),
        )
        .unwrap();
        assert_eq!(
            resolved.accounts(),
            &[
                payer,
                AccountIdentity::PublicNoSign(second),
                AccountIdentity::PublicNoSign(first),
            ]
        );

        let cycle_idl = make_idl(vec![instruction(
            "cycle",
            vec![
                pda_account(
                    "a",
                    false,
                    vec![IdlSeed::Account {
                        path: "b".to_string(),
                    }],
                ),
                pda_account(
                    "b",
                    false,
                    vec![IdlSeed::Account {
                        path: "a".to_string(),
                    }],
                ),
            ],
            vec![],
        )]);
        let cycle = resolve_public_instruction(
            SpelInstructionRequest {
                idl: &cycle_idl,
                instruction: "cycle",
                accounts: BTreeMap::new(),
                arguments: BTreeMap::new(),
            },
            program_id(),
        )
        .err()
        .expect("resolver must reject invalid input");
        assert!(matches!(
            cycle,
            SpelTxError::PdaResolution {
                ref account,
                seed_index: Some(0),
                ..
            } if account == "b"
        ));

        let malformed_idl = make_idl(vec![instruction(
            "empty",
            vec![pda_account("pda", false, vec![])],
            vec![],
        )]);
        let malformed = resolve_public_instruction(
            SpelInstructionRequest {
                idl: &malformed_idl,
                instruction: "empty",
                accounts: BTreeMap::new(),
                arguments: BTreeMap::new(),
            },
            program_id(),
        )
        .err()
        .expect("resolver must reject invalid input");
        assert!(matches!(
            malformed,
            SpelTxError::PdaResolution {
                ref account,
                seed_index: None,
                ..
            } if account == "pda"
        ));

        let unknown_seed_idl = make_idl(vec![instruction(
            "unknown-seed",
            vec![
                account("payer"),
                pda_account(
                    "state",
                    false,
                    vec![IdlSeed::Arg {
                        path: "missing".to_string(),
                    }],
                ),
            ],
            vec![],
        )]);
        let unknown_seed = resolve_public_instruction(
            SpelInstructionRequest {
                idl: &unknown_seed_idl,
                instruction: "unknown-seed",
                accounts: BTreeMap::new(),
                arguments: BTreeMap::new(),
            },
            program_id(),
        )
        .err()
        .expect("resolver must reject an unknown PDA seed argument");
        assert!(matches!(
            unknown_seed,
            SpelTxError::PdaResolution {
                ref account,
                seed_index: Some(0),
                ..
            } if account == "state"
        ));
    }

    #[test]
    fn serializes_canonical_arguments_in_idl_order() {
        #[derive(Debug, Deserialize, PartialEq)]
        enum TestInstruction {
            Ignored,
            Execute {
                bytes: [u8; 3],
                values: Vec<u32>,
                enabled: Option<bool>,
                maximum: u128,
            },
        }

        let idl = make_idl(vec![
            instruction("ignored", vec![], vec![]),
            instruction(
                "execute",
                vec![],
                vec![
                    arg(
                        "bytes",
                        IdlType::Array {
                            array: (Box::new(IdlType::Primitive("u8".to_string())), 3),
                        },
                    ),
                    arg(
                        "values",
                        IdlType::Vec {
                            vec: Box::new(IdlType::Primitive("u32".to_string())),
                        },
                    ),
                    arg(
                        "enabled",
                        IdlType::Option {
                            option: Box::new(IdlType::Primitive("bool".to_string())),
                        },
                    ),
                    arg("maximum", IdlType::Primitive("u128".to_string())),
                ],
            ),
        ]);
        let resolved = resolve_public_instruction(
            SpelInstructionRequest {
                idl: &idl,
                instruction: "execute",
                accounts: BTreeMap::new(),
                arguments: BTreeMap::from([
                    ("values".to_string(), "[4,5]".to_string()),
                    (
                        "maximum".to_string(),
                        "+340282366920938463463374607431768211455".to_string(),
                    ),
                    ("enabled".to_string(), "true".to_string()),
                    ("bytes".to_string(), "[1,2,3]".to_string()),
                ]),
            },
            program_id(),
        )
        .unwrap();
        let instruction =
            TestInstruction::deserialize(&mut Deserializer::new(resolved.instruction_data()))
                .unwrap();

        assert_eq!(
            instruction,
            TestInstruction::Execute {
                bytes: [1, 2, 3],
                values: vec![4, 5],
                enabled: Some(true),
                maximum: u128::MAX,
            }
        );
    }

    #[test]
    fn reports_nested_argument_parse_paths() {
        let idl = make_idl(vec![instruction(
            "execute",
            vec![],
            vec![arg(
                "values",
                IdlType::Vec {
                    vec: Box::new(IdlType::Primitive("u32".to_string())),
                },
            )],
        )]);
        let error = resolve_public_instruction(
            SpelInstructionRequest {
                idl: &idl,
                instruction: "execute",
                accounts: BTreeMap::new(),
                arguments: BTreeMap::from([("values".to_string(), "[1,\"bad\"]".to_string())]),
            },
            program_id(),
        )
        .err()
        .expect("resolver must reject invalid input");

        assert!(matches!(
            error,
            SpelTxError::ArgumentParse {
                ref name,
                path: ref actual_path,
                ..
            } if name == "values" && actual_path == &[1]
        ));
    }
}
