//! Ergonomic runtime-IDL transaction builder selection.
//!
//! The public types in this module are re-exported from [`crate::tx`]. They
//! select and validate an IDL instruction without invoking CLI code or owning
//! Wallet transaction submission.

use std::collections::BTreeMap;

use nssa::{
    privacy_preserving_transaction::circuit::ProgramWithDependencies, AccountId,
    PrivacyPreservingTransaction, PublicTransaction,
};
use nssa_core::{program::ProgramId, NullifierPublicKey, SharedSecretKey};
use serde_json::Value;
use spel_framework_core::idl::{IdlAccountItem, IdlInstruction, SpelIdl};
use thiserror::Error;
use wallet::{AccountIdentity, ExecutionFailureKind, WalletCore};

use crate::hex::hex_encode;

use super::resolution::{
    resolve_private_instruction, resolve_private_instruction_parts, resolve_public_instruction,
    select_and_validate_instruction, ResolvedPrivateInstruction, ResolvedPublicInstruction,
    SpelInstructionRequest, SpelTxError,
};

/// Runtime-IDL entry point for binding a program and selecting instructions.
///
/// ```no_run
/// use nssa_core::program::ProgramId;
/// use spel::tx::SpelProgram;
/// use spel_framework_core::idl::SpelIdl;
///
/// fn select(idl: &SpelIdl, program_id: ProgramId) {
///     let _builder = SpelProgram::new(idl).program(program_id).public("transfer");
/// }
/// ```
#[must_use = "bind a program and select an instruction to create a transaction builder"]
pub struct SpelProgram<'idl> {
    idl: &'idl SpelIdl,
}

impl<'idl> SpelProgram<'idl> {
    /// Creates an entry point for instructions declared in `idl`.
    pub const fn new(idl: &'idl SpelIdl) -> Self {
        Self { idl }
    }

    /// Binds a public program ID or borrowed private program for reusable instruction selection.
    pub fn program<'program>(
        &self,
        program: impl Into<SpelProgramBinding<'program>>,
    ) -> BoundSpelProgram<'idl, 'program> {
        BoundSpelProgram {
            idl: self.idl,
            binding: program.into(),
        }
    }
}

/// Public or private program material bound to a [`SpelProgram`].
#[non_exhaustive]
pub enum SpelProgramBinding<'program> {
    /// A public program ID.
    Public(ProgramId),
    /// A privacy-preserving program and its dependency binaries.
    Private(&'program ProgramWithDependencies),
}

impl From<ProgramId> for SpelProgramBinding<'_> {
    fn from(program_id: ProgramId) -> Self {
        Self::Public(program_id)
    }
}

impl<'program> From<&'program ProgramWithDependencies> for SpelProgramBinding<'program> {
    fn from(program: &'program ProgramWithDependencies) -> Self {
        Self::Private(program)
    }
}

/// Failure returned while resolving or building a runtime-IDL instruction.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum SpelBuildError {
    /// The selected instruction could not be resolved from its IDL inputs.
    #[error("failed to resolve instruction: {0}")]
    Resolution(#[from] SpelTxError),
    /// An inferred public signer or initialized account has no local Wallet signing key.
    #[error("missing public signing key for account `{account}`")]
    MissingPublicSigningKey {
        /// Exact IDL account name whose inferred public signing key is absent.
        account: String,
    },
    /// Wallet transaction construction failed.
    #[error("failed to build wallet transaction: {0}")]
    Wallet(#[from] ExecutionFailureKind),
}

/// One named runtime-IDL account or argument input.
///
/// Account variants are accepted only for IDL account names. Scalar values are
/// passed to the existing argument parser, while [`Self::Json`] provides
/// canonical JSON for container arguments.
pub enum SpelInput {
    /// One account ID whose public signing intent is inferred from the IDL account flags.
    AccountId(AccountId),
    /// One explicit Wallet account identity.
    AccountIdentity(AccountIdentity),
    /// Ordered account IDs for one trailing rest account.
    AccountIds(Vec<AccountId>),
    /// Ordered explicit Wallet identities for one trailing rest account.
    AccountIdentities(Vec<AccountIdentity>),
    /// CLI-compatible scalar or container argument text.
    ArgumentText(String),
    /// Canonical JSON for an IDL argument.
    Json(Value),
}

impl From<AccountId> for SpelInput {
    fn from(account_id: AccountId) -> Self {
        Self::AccountId(account_id)
    }
}

impl From<AccountIdentity> for SpelInput {
    fn from(identity: AccountIdentity) -> Self {
        Self::AccountIdentity(identity)
    }
}

impl From<Vec<AccountId>> for SpelInput {
    fn from(account_ids: Vec<AccountId>) -> Self {
        Self::AccountIds(account_ids)
    }
}

impl From<Vec<AccountIdentity>> for SpelInput {
    fn from(identities: Vec<AccountIdentity>) -> Self {
        Self::AccountIdentities(identities)
    }
}

impl From<String> for SpelInput {
    fn from(value: String) -> Self {
        Self::ArgumentText(value)
    }
}

impl From<&str> for SpelInput {
    fn from(value: &str) -> Self {
        Self::ArgumentText(value.to_owned())
    }
}

impl From<Value> for SpelInput {
    fn from(value: Value) -> Self {
        Self::Json(value)
    }
}

impl From<ProgramId> for SpelInput {
    fn from(program_id: ProgramId) -> Self {
        Self::ArgumentText(
            program_id
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(","),
        )
    }
}

impl From<NullifierPublicKey> for SpelInput {
    fn from(public_key: NullifierPublicKey) -> Self {
        Self::ArgumentText(hex_encode(&public_key.0))
    }
}

macro_rules! argument_text_from {
    ($($type:ty),+ $(,)?) => {
        $(
            impl From<$type> for SpelInput {
                fn from(value: $type) -> Self {
                    Self::ArgumentText(value.to_string())
                }
            }
        )+
    };
}

argument_text_from!(bool, u8, u16, u32, u64, u128, i8, i16, i32, i64, i128);

/// A reusable IDL and program binding that creates public or private builders.
#[must_use = "select a public or private instruction from this bound program"]
pub struct BoundSpelProgram<'idl, 'program> {
    idl: &'idl SpelIdl,
    binding: SpelProgramBinding<'program>,
}

impl<'idl, 'program> BoundSpelProgram<'idl, 'program> {
    /// Selects a public instruction and validates its static IDL shape.
    ///
    /// # Errors
    ///
    /// Returns [`SpelTxError::PublicProgramRequired`] when this binding is
    /// private, or a selection/IDL error when `instruction` is invalid.
    pub fn public(&self, instruction: &str) -> Result<PublicInstructionBuilder<'idl>, SpelTxError> {
        let SpelProgramBinding::Public(program_id) = self.binding else {
            return Err(SpelTxError::PublicProgramRequired);
        };
        let (_, selected) = select_and_validate_instruction(self.idl, instruction, false)?;
        Ok(PublicInstructionBuilder {
            idl: self.idl,
            instruction: selected,
            program_id,
            inputs: InstructionInputs::default(),
        })
    }

    /// Selects a privacy-preserving instruction and validates its static IDL shape.
    ///
    /// # Errors
    ///
    /// Returns [`SpelTxError::PrivateProgramRequired`] when this binding is
    /// public, or a selection/IDL error when `instruction` is invalid.
    pub fn private(
        &self,
        instruction: &str,
    ) -> Result<PrivateInstructionBuilder<'idl, 'program>, SpelTxError> {
        let SpelProgramBinding::Private(program) = self.binding else {
            return Err(SpelTxError::PrivateProgramRequired);
        };
        let (_, selected) = select_and_validate_instruction(self.idl, instruction, true)?;
        Ok(PrivateInstructionBuilder {
            idl: self.idl,
            instruction: selected,
            program,
            inputs: InstructionInputs::default(),
        })
    }
}

#[derive(Default)]
struct InstructionInputs {
    accounts: BTreeMap<String, Vec<AccountIdentity>>,
    arguments: BTreeMap<String, String>,
    inferred_public_signers: Vec<InferredPublicSigner>,
}

struct InferredPublicSigner {
    account: String,
    account_id: AccountId,
}

impl InstructionInputs {
    fn into_request<'idl>(
        self,
        idl: &'idl SpelIdl,
        instruction: &'idl IdlInstruction,
    ) -> (SpelInstructionRequest<'idl>, Vec<InferredPublicSigner>) {
        (
            SpelInstructionRequest {
                idl,
                instruction: &instruction.name,
                accounts: self.accounts,
                arguments: self.arguments,
            },
            self.inferred_public_signers,
        )
    }

    fn insert(
        &mut self,
        instruction: &IdlInstruction,
        name: String,
        input: SpelInput,
    ) -> Result<(), SpelTxError> {
        if self.accounts.contains_key(&name) || self.arguments.contains_key(&name) {
            return Err(SpelTxError::DuplicateInput { name });
        }

        let account = instruction
            .accounts
            .iter()
            .find(|account| account.name == name);
        let argument = instruction
            .args
            .iter()
            .find(|argument| argument.name == name);

        match (account, argument) {
            (None, None) => Err(SpelTxError::UnknownInput { name }),
            (Some(_), Some(_)) => Err(SpelTxError::AmbiguousInput { name }),
            (Some(account), None) => self.insert_account(name, account, input),
            (None, Some(_)) => self.insert_argument(name, input),
        }
    }

    fn insert_all<I>(&mut self, instruction: &IdlInstruction, inputs: I) -> Result<(), SpelTxError>
    where
        I: IntoIterator<Item = (String, SpelInput)>,
    {
        for (name, input) in inputs {
            self.insert(instruction, name, input)?;
        }
        Ok(())
    }

    fn insert_account(
        &mut self,
        name: String,
        account: &IdlAccountItem,
        input: SpelInput,
    ) -> Result<(), SpelTxError> {
        match &account.pda {
            Some(pda) if !pda.private => {
                Err(invalid_input(name, "public PDA is derived from the IDL"))
            },
            Some(_) => {
                let SpelInput::AccountIdentity(identity) = input else {
                    return Err(invalid_input(
                        name,
                        "private PDA requires an explicit account identity",
                    ));
                };
                self.accounts.insert(name, vec![identity]);
                Ok(())
            },
            None if account.rest => {
                let identities = match input {
                    SpelInput::AccountIds(account_ids) => account_ids
                        .into_iter()
                        .map(|account_id| self.infer_account_identity(account, account_id))
                        .collect(),
                    SpelInput::AccountIdentities(identities) => identities,
                    _ => {
                        return Err(invalid_input(
                            name,
                            "rest account requires an account ID or identity vector",
                        ));
                    },
                };
                self.accounts.insert(name, identities);
                Ok(())
            },
            None => {
                let identity = match input {
                    SpelInput::AccountId(account_id) => {
                        self.infer_account_identity(account, account_id)
                    },
                    SpelInput::AccountIdentity(identity) => identity,
                    _ => {
                        return Err(invalid_input(
                            name,
                            "fixed account requires an account ID or account identity",
                        ));
                    },
                };
                self.accounts.insert(name, vec![identity]);
                Ok(())
            },
        }
    }

    fn insert_argument(&mut self, name: String, input: SpelInput) -> Result<(), SpelTxError> {
        let value = match input {
            SpelInput::ArgumentText(value) => value,
            SpelInput::Json(value) => serde_json::to_string(&value).map_err(|_error| {
                invalid_input(name.clone(), "json input could not be serialized")
            })?,
            _ => {
                return Err(invalid_input(name, "argument requires text or JSON input"));
            },
        };
        self.arguments.insert(name, value);
        Ok(())
    }

    fn infer_account_identity(
        &mut self,
        account: &IdlAccountItem,
        account_id: AccountId,
    ) -> AccountIdentity {
        if requires_public_signing(account) {
            self.inferred_public_signers.push(InferredPublicSigner {
                account: account.name.clone(),
                account_id,
            });
            AccountIdentity::Public(account_id)
        } else {
            AccountIdentity::PublicNoSign(account_id)
        }
    }
}

fn requires_public_signing(account: &IdlAccountItem) -> bool {
    account.signer || account.init
}

fn invalid_input(name: String, reason: &'static str) -> SpelTxError {
    SpelTxError::InvalidInput {
        name,
        reason: reason.to_owned(),
    }
}

fn preflight_inferred_public_signers(
    wallet: &WalletCore,
    inferred_public_signers: &[InferredPublicSigner],
) -> Result<(), SpelBuildError> {
    for signer in inferred_public_signers {
        if wallet
            .get_account_public_signing_key(signer.account_id)
            .is_none()
        {
            return Err(SpelBuildError::MissingPublicSigningKey {
                account: signer.account.clone(),
            });
        }
    }
    Ok(())
}

/// A selected public instruction awaiting named inputs.
#[must_use = "provide inputs, resolve, or build the selected public instruction"]
pub struct PublicInstructionBuilder<'idl> {
    idl: &'idl SpelIdl,
    instruction: &'idl IdlInstruction,
    program_id: ProgramId,
    inputs: InstructionInputs,
}

impl PublicInstructionBuilder<'_> {
    /// Adds one named account or argument input.
    ///
    /// Bare account IDs infer public signing intent from IDL `signer` and
    /// non-PDA `init` flags. Use [`AccountIdentity`] when that inference is not
    /// appropriate, and [`SpelInput::Json`] for canonical container arguments.
    ///
    /// # Errors
    ///
    /// Returns [`SpelTxError`] when `name` is unknown, ambiguous, duplicated,
    /// or incompatible with the selected IDL position.
    pub fn input(
        mut self,
        name: impl Into<String>,
        input: impl Into<SpelInput>,
    ) -> Result<Self, SpelTxError> {
        self.inputs
            .insert(self.instruction, name.into(), input.into())?;
        Ok(self)
    }

    /// Adds a dynamic collection of named inputs.
    ///
    /// This is equivalent to repeated [`Self::input`] calls and accepts owned
    /// entries from standard maps or vectors.
    ///
    /// # Errors
    ///
    /// Returns the first [`SpelTxError`] produced by the equivalent
    /// [`Self::input`] call.
    pub fn inputs<I>(mut self, inputs: I) -> Result<Self, SpelTxError>
    where
        I: IntoIterator<Item = (String, SpelInput)>,
    {
        self.inputs.insert_all(self.instruction, inputs)?;
        Ok(self)
    }

    /// Resolves named inputs into direct public Wallet build inputs.
    ///
    /// # Errors
    ///
    /// Returns [`SpelTxError`] from the existing resolver when required inputs,
    /// argument parsing, account identities, PDAs, duplicate accounts, or
    /// instruction serialization are invalid.
    pub fn resolve(self) -> Result<ResolvedPublicInstruction, SpelTxError> {
        let Self {
            idl,
            instruction,
            program_id,
            inputs,
        } = self;
        let (request, _) = inputs.into_request(idl, instruction);
        resolve_public_instruction(request, program_id)
    }

    /// Resolves and builds a native public Wallet transaction without submitting it.
    ///
    /// Bare account IDs inferred as public signers or initialized accounts must
    /// have a local Wallet signing key. Explicit [`AccountIdentity`] inputs
    /// remain a direct Wallet contract.
    ///
    /// # Errors
    ///
    /// Returns [`SpelBuildError::Resolution`] for invalid selected-IDL inputs,
    /// [`SpelBuildError::MissingPublicSigningKey`] for a missing inferred key,
    /// or [`SpelBuildError::Wallet`] when Wallet construction fails.
    pub async fn build(self, wallet: &WalletCore) -> Result<PublicTransaction, SpelBuildError> {
        let Self {
            idl,
            instruction,
            program_id,
            inputs,
        } = self;
        let (request, inferred_public_signers) = inputs.into_request(idl, instruction);
        let resolved = resolve_public_instruction(request, program_id)?;
        preflight_inferred_public_signers(wallet, &inferred_public_signers)?;
        let (program_id, accounts, instruction_data) = resolved.into_parts();

        Ok(wallet
            .build_pub_tx(accounts, instruction_data, program_id)
            .await?)
    }
}

/// A selected privacy-preserving instruction awaiting named inputs.
#[must_use = "provide inputs, resolve, or build the selected private instruction"]
pub struct PrivateInstructionBuilder<'idl, 'program> {
    idl: &'idl SpelIdl,
    instruction: &'idl IdlInstruction,
    program: &'program ProgramWithDependencies,
    inputs: InstructionInputs,
}

impl PrivateInstructionBuilder<'_, '_> {
    /// Adds one named account or argument input.
    ///
    /// Uses the same input classification rules as
    /// [`PublicInstructionBuilder::input`].
    ///
    /// # Errors
    ///
    /// Returns [`SpelTxError`] when `name` is unknown, ambiguous, duplicated,
    /// or incompatible with the selected IDL position.
    pub fn input(
        mut self,
        name: impl Into<String>,
        input: impl Into<SpelInput>,
    ) -> Result<Self, SpelTxError> {
        self.inputs
            .insert(self.instruction, name.into(), input.into())?;
        Ok(self)
    }

    /// Adds a dynamic collection of named inputs.
    ///
    /// This is equivalent to repeated [`Self::input`] calls and accepts owned
    /// entries from standard maps or vectors.
    ///
    /// # Errors
    ///
    /// Returns the first [`SpelTxError`] produced by the equivalent
    /// [`Self::input`] call.
    pub fn inputs<I>(mut self, inputs: I) -> Result<Self, SpelTxError>
    where
        I: IntoIterator<Item = (String, SpelInput)>,
    {
        self.inputs.insert_all(self.instruction, inputs)?;
        Ok(self)
    }

    /// Resolves named inputs into direct privacy-preserving Wallet build inputs.
    ///
    /// This clones the caller-provided program only because
    /// [`ResolvedPrivateInstruction`] preserves the established owning result
    /// contract. The later `build` path uses the original borrow instead.
    ///
    /// # Errors
    ///
    /// Returns [`SpelTxError`] from the existing resolver when required inputs,
    /// argument parsing, account identities, PDAs, duplicate accounts, or
    /// instruction serialization are invalid.
    pub fn resolve(self) -> Result<ResolvedPrivateInstruction, SpelTxError> {
        let Self {
            idl,
            instruction,
            program,
            inputs,
        } = self;
        let (request, _) = inputs.into_request(idl, instruction);
        resolve_private_instruction(request, program.to_owned())
    }

    /// Resolves and builds a native privacy-preserving Wallet transaction without submitting it.
    ///
    /// Bare account IDs inferred as public signers or initialized accounts must
    /// have a local Wallet signing key. Explicit [`AccountIdentity`] inputs
    /// remain a direct Wallet contract.
    ///
    /// # Errors
    ///
    /// Returns [`SpelBuildError::Resolution`] for invalid selected-IDL inputs,
    /// [`SpelBuildError::MissingPublicSigningKey`] for a missing inferred key,
    /// or [`SpelBuildError::Wallet`] when Wallet construction fails.
    pub async fn build(
        self,
        wallet: &WalletCore,
    ) -> Result<(PrivacyPreservingTransaction, Vec<SharedSecretKey>), SpelBuildError> {
        let Self {
            idl,
            instruction,
            program,
            inputs,
        } = self;
        let (request, inferred_public_signers) = inputs.into_request(idl, instruction);
        let (accounts, instruction_data) =
            resolve_private_instruction_parts(request, program.program.id())?;
        preflight_inferred_public_signers(wallet, &inferred_public_signers)?;

        Ok(wallet
            .build_privacy_preserving_tx(accounts, instruction_data, program)
            .await?)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        borrow::Cow,
        collections::{BTreeMap, HashMap},
    };

    use nssa::{
        privacy_preserving_transaction::circuit::ProgramWithDependencies, program::Program,
        AccountId,
    };
    use nssa_core::{encryption::ViewingPublicKey, NullifierPublicKey};
    use serde_json::json;
    use spel_framework_core::idl::{
        IdlAccountItem, IdlArg, IdlInstruction, IdlPda, IdlSeed, IdlType,
    };

    use super::*;

    fn idl(instructions: Vec<IdlInstruction>) -> SpelIdl {
        let mut idl = SpelIdl::new("runtime-test");
        idl.instructions = instructions;
        idl
    }

    fn instruction(name: &str) -> IdlInstruction {
        instruction_with_inputs(name, vec![], vec![])
    }

    fn instruction_with_inputs(
        name: &str,
        accounts: Vec<IdlAccountItem>,
        args: Vec<IdlArg>,
    ) -> IdlInstruction {
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

    fn pda_account(name: &str, private: bool) -> IdlAccountItem {
        let mut account = account(name);
        account.pda = Some(IdlPda {
            seeds: vec![IdlSeed::Const {
                value: "state".to_string(),
            }],
            private,
        });
        account
    }

    fn argument(name: &str, type_: IdlType) -> IdlArg {
        IdlArg {
            name: name.to_string(),
            type_,
        }
    }

    fn private_program() -> ProgramWithDependencies {
        ProgramWithDependencies::new(
            Program::new_unchecked([9; 8], Cow::Borrowed(&[])),
            HashMap::new(),
        )
    }

    fn temporary_wallet() -> (WalletCore, tempfile::TempDir) {
        let wallet_home = tempfile::tempdir().expect("temporary Wallet directory must initialize");
        let (wallet, _) = WalletCore::new_init_storage(
            wallet_home.path().join("wallet-config.json"),
            wallet_home.path().join("storage.json"),
            None,
            "runtime-test-password",
        )
        .expect("temporary Wallet must initialize");
        (wallet, wallet_home)
    }

    #[test]
    fn bound_public_program_reuses_selection_for_multiple_instructions() {
        let idl = idl(vec![instruction("first"), instruction("second")]);
        let program_id: ProgramId = [1; 8];
        let bound = SpelProgram::new(&idl).program(program_id);

        assert!(bound.public("first").is_ok());
        assert!(bound.public("second").is_ok());
    }

    #[test]
    fn temporary_bound_private_program_keeps_original_program_borrow() {
        let idl = idl(vec![instruction("execute")]);
        let program = private_program();

        assert!(SpelProgram::new(&idl)
            .program(&program)
            .private("execute")
            .is_ok());
    }

    #[test]
    fn public_and_private_selection_reject_incompatible_program_bindings() {
        let idl = idl(vec![instruction("execute")]);
        let program = private_program();
        let public_program_id: ProgramId = [1; 8];

        assert!(matches!(
            SpelProgram::new(&idl).program(&program).public("execute"),
            Err(SpelTxError::PublicProgramRequired)
        ));
        assert!(matches!(
            SpelProgram::new(&idl)
                .program(public_program_id)
                .private("execute"),
            Err(SpelTxError::PrivateProgramRequired)
        ));
    }

    #[test]
    fn selection_rejects_invalid_static_idl_before_input_collection() {
        let mut invalid = instruction("execute");
        invalid.args.push(IdlArg {
            name: String::new(),
            type_: IdlType::Primitive("u8".to_string()),
        });
        let idl = idl(vec![invalid]);
        let program_id: ProgramId = [1; 8];

        assert!(matches!(
            SpelProgram::new(&idl).program(program_id).public("execute"),
            Err(SpelTxError::InvalidIdl { ref path, .. }) if path == "args[0]"
        ));
    }

    #[test]
    fn input_classifies_fixed_rest_and_argument_values() {
        let mut signer = account("signer");
        signer.signer = true;
        let mut initialized = account("initialized");
        initialized.init = true;
        initialized.writable = true;
        let reader = account("reader");
        let mut members = account("members");
        members.rest = true;
        let idl = idl(vec![instruction_with_inputs(
            "execute",
            vec![signer, initialized, reader, members],
            vec![
                argument("amount", IdlType::Primitive("u128".to_string())),
                argument(
                    "bytes",
                    IdlType::Vec {
                        vec: Box::new(IdlType::Primitive("u8".to_string())),
                    },
                ),
            ],
        )]);
        let explicit = AccountIdentity::PublicNoSign(AccountId::new([3; 32]));

        let builder = SpelProgram::new(&idl)
            .program([1; 8])
            .public("execute")
            .expect("test IDL must select")
            .input("signer", AccountId::new([1; 32]))
            .expect("signer input must classify")
            .input("initialized", AccountId::new([2; 32]))
            .expect("initialized input must classify")
            .input("reader", explicit.clone())
            .expect("explicit identity must classify")
            .input(
                "members",
                vec![AccountId::new([4; 32]), AccountId::new([5; 32])],
            )
            .expect("rest account IDs must classify")
            .input("amount", 42_u128)
            .expect("scalar argument must classify")
            .input("bytes", json!([1, 2, 3]))
            .expect("JSON argument must classify");

        assert_eq!(
            builder.inputs.accounts["signer"],
            vec![AccountIdentity::Public(AccountId::new([1; 32]))]
        );
        assert_eq!(
            builder.inputs.accounts["initialized"],
            vec![AccountIdentity::Public(AccountId::new([2; 32]))]
        );
        assert_eq!(builder.inputs.accounts["reader"], vec![explicit]);
        assert_eq!(
            builder.inputs.accounts["members"],
            vec![
                AccountIdentity::PublicNoSign(AccountId::new([4; 32])),
                AccountIdentity::PublicNoSign(AccountId::new([5; 32])),
            ]
        );
        assert_eq!(builder.inputs.arguments["amount"], "42");
        assert_eq!(builder.inputs.arguments["bytes"], "[1,2,3]");
        assert_eq!(
            builder
                .inputs
                .inferred_public_signers
                .iter()
                .map(|signer| signer.account.as_str())
                .collect::<Vec<_>>(),
            ["signer", "initialized"]
        );
    }

    #[test]
    fn input_conversions_preserve_existing_argument_text_formats() {
        fn assert_argument_text(input: SpelInput, expected: &str) {
            let SpelInput::ArgumentText(value) = input else {
                panic!("input must convert to argument text");
            };
            assert_eq!(value, expected);
        }

        assert_argument_text(SpelInput::from(true), "true");
        assert_argument_text(SpelInput::from(-42_i128), "-42");
        assert_argument_text(SpelInput::from("text"), "text");
        assert_argument_text(SpelInput::from([1, 2, 3, 4, 5, 6, 7, 8]), "1,2,3,4,5,6,7,8");
        assert_argument_text(
            SpelInput::from(NullifierPublicKey([0xab; 32])),
            &"ab".repeat(32),
        );
        assert!(matches!(
            SpelInput::from(json!([1, 2, 3])),
            SpelInput::Json(_)
        ));
    }

    #[test]
    fn input_reports_wrapper_name_and_shape_errors() {
        let fixed = account("fixed");
        let shared = account("shared");
        let public_pda = pda_account("state", false);
        let mut rest = account("rest");
        rest.rest = true;
        let idl = idl(vec![instruction_with_inputs(
            "execute",
            vec![fixed, shared, public_pda, rest],
            vec![
                argument("value", IdlType::Primitive("u8".to_string())),
                argument("shared", IdlType::Primitive("u8".to_string())),
            ],
        )]);

        let select = || {
            SpelProgram::new(&idl)
                .program([1; 8])
                .public("execute")
                .expect("test IDL must select")
        };

        assert!(matches!(
            select().input("missing", 1_u8),
            Err(SpelTxError::UnknownInput { ref name }) if name == "missing"
        ));
        assert!(matches!(
            select().input("shared", 1_u8),
            Err(SpelTxError::AmbiguousInput { ref name }) if name == "shared"
        ));
        assert!(matches!(
            select().input("fixed", vec![AccountId::new([1; 32])]),
            Err(SpelTxError::InvalidInput { ref name, .. }) if name == "fixed"
        ));
        assert!(matches!(
            select().input("rest", AccountId::new([1; 32])),
            Err(SpelTxError::InvalidInput { ref name, .. }) if name == "rest"
        ));
        assert!(matches!(
            select().input("state", AccountId::new([1; 32])),
            Err(SpelTxError::InvalidInput { ref name, .. }) if name == "state"
        ));
        assert!(matches!(
            select().input("value", AccountId::new([1; 32])),
            Err(SpelTxError::InvalidInput { ref name, .. }) if name == "value"
        ));

        let builder = select()
            .input("value", 1_u8)
            .expect("first input must classify");
        assert!(matches!(
            builder.input("value", 2_u8),
            Err(SpelTxError::DuplicateInput { ref name }) if name == "value"
        ));
    }

    #[test]
    fn inputs_matches_repeated_input_calls_and_rejects_duplicate_batch_names() {
        let payer = account("payer");
        let mut rest = account("rest");
        rest.rest = true;
        let idl = idl(vec![instruction_with_inputs(
            "execute",
            vec![payer, rest],
            vec![argument("value", IdlType::Primitive("u8".to_string()))],
        )]);
        let select = || {
            SpelProgram::new(&idl)
                .program([1; 8])
                .public("execute")
                .expect("test IDL must select")
        };

        let repeated = select()
            .input("payer", AccountId::new([1; 32]))
            .expect("payer must classify")
            .input("rest", vec![AccountId::new([2; 32])])
            .expect("rest must classify")
            .input("value", 3_u8)
            .expect("argument must classify");
        let batched = select()
            .inputs(BTreeMap::from([
                (
                    "payer".to_string(),
                    SpelInput::from(AccountId::new([1; 32])),
                ),
                (
                    "rest".to_string(),
                    SpelInput::from(vec![AccountId::new([2; 32])]),
                ),
                ("value".to_string(), SpelInput::from(3_u8)),
            ]))
            .expect("batch inputs must classify");

        assert_eq!(repeated.inputs.accounts, batched.inputs.accounts);
        assert_eq!(repeated.inputs.arguments, batched.inputs.arguments);
        assert!(matches!(
            select().inputs(vec![
                ("value".to_string(), SpelInput::from(1_u8)),
                ("value".to_string(), SpelInput::from(2_u8)),
            ]),
            Err(SpelTxError::DuplicateInput { ref name }) if name == "value"
        ));
    }

    #[test]
    fn private_inputs_require_an_explicit_identity_for_private_pdas() {
        let mut private_pda = pda_account("state", true);
        private_pda.init = true;
        private_pda.writable = true;
        let idl = idl(vec![instruction_with_inputs(
            "execute",
            vec![private_pda],
            vec![argument("value", IdlType::Primitive("u8".to_string()))],
        )]);
        let program = private_program();
        let identity = AccountIdentity::PrivatePdaForeign {
            account_id: AccountId::new([1; 32]),
            npk: NullifierPublicKey([2; 32]),
            vpk: ViewingPublicKey::from_seed(&[3; 32], &[4; 32]),
            identifier: 5,
        };
        let select = || {
            SpelProgram::new(&idl)
                .program(&program)
                .private("execute")
                .expect("test IDL must select")
        };

        assert!(matches!(
            select().input("state", AccountId::new([1; 32])),
            Err(SpelTxError::InvalidInput { ref name, .. }) if name == "state"
        ));
        let builder = select()
            .inputs(BTreeMap::from([
                ("state".to_string(), SpelInput::from(identity.clone())),
                ("value".to_string(), SpelInput::from(7_u8)),
            ]))
            .expect("explicit private PDA identity must classify");

        assert_eq!(builder.inputs.accounts["state"], vec![identity]);
        assert_eq!(builder.inputs.arguments["value"], "7");
    }

    #[test]
    fn public_resolve_matches_direct_resolution_and_defers_argument_parsing() {
        let mut payer = account("payer");
        payer.signer = true;
        let recipient = account("recipient");
        let idl = idl(vec![instruction_with_inputs(
            "transfer",
            vec![payer, recipient],
            vec![argument("amount", IdlType::Primitive("u8".to_string()))],
        )]);
        let program_id = [1; 8];
        let payer_id = AccountId::new([1; 32]);
        let recipient_identity = AccountIdentity::PublicNoSign(AccountId::new([2; 32]));
        let direct = resolve_public_instruction(
            SpelInstructionRequest {
                idl: &idl,
                instruction: "transfer",
                accounts: BTreeMap::from([
                    ("payer".to_string(), vec![AccountIdentity::Public(payer_id)]),
                    ("recipient".to_string(), vec![recipient_identity.clone()]),
                ]),
                arguments: BTreeMap::from([("amount".to_string(), "7".to_string())]),
            },
            program_id,
        )
        .expect("direct request must resolve");
        let fluent = SpelProgram::new(&idl)
            .program(program_id)
            .public("transfer")
            .expect("test IDL must select")
            .input("payer", payer_id)
            .expect("payer must classify")
            .input("recipient", recipient_identity)
            .expect("recipient must classify")
            .input("amount", 7_u8)
            .expect("amount must classify")
            .resolve()
            .expect("fluent request must resolve");

        assert_eq!(fluent.program_id(), direct.program_id());
        assert_eq!(fluent.accounts(), direct.accounts());
        assert_eq!(fluent.instruction_data(), direct.instruction_data());
        assert!(matches!(
            SpelProgram::new(&idl)
                .program(program_id)
                .public("transfer")
                .expect("test IDL must select")
                .input("payer", payer_id)
                .expect("payer must classify")
                .input("recipient", AccountIdentity::PublicNoSign(AccountId::new([2; 32])))
                .expect("recipient must classify")
                .input("amount", "not a number")
                .expect("argument text must classify")
                .resolve(),
            Err(SpelTxError::ArgumentParse { ref name, ref path, .. })
                if name == "amount" && path.is_empty()
        ));
    }

    #[test]
    fn private_resolve_matches_direct_resolution_and_preserves_program_borrow() {
        let idl = idl(vec![instruction_with_inputs(
            "transfer",
            vec![account("owner")],
            vec![argument("amount", IdlType::Primitive("u8".to_string()))],
        )]);
        let program = private_program();
        let identity = AccountIdentity::PrivateOwned(AccountId::new([3; 32]));
        let direct = resolve_private_instruction(
            SpelInstructionRequest {
                idl: &idl,
                instruction: "transfer",
                accounts: BTreeMap::from([("owner".to_string(), vec![identity.clone()])]),
                arguments: BTreeMap::from([("amount".to_string(), "9".to_string())]),
            },
            program.clone(),
        )
        .expect("direct request must resolve");
        let fluent = SpelProgram::new(&idl)
            .program(&program)
            .private("transfer")
            .expect("test IDL must select")
            .input("owner", identity)
            .expect("owner must classify")
            .input("amount", 9_u8)
            .expect("amount must classify")
            .resolve()
            .expect("fluent request must resolve");

        assert_eq!(fluent.program().program.id(), direct.program().program.id());
        assert_eq!(fluent.accounts(), direct.accounts());
        assert_eq!(fluent.instruction_data(), direct.instruction_data());
        assert_eq!(program.program.id(), fluent.program().program.id());
    }

    #[tokio::test]
    async fn public_build_returns_native_transaction_without_accounts() {
        let idl = idl(vec![instruction("initialize")]);
        let program_id: ProgramId = [1; 8];
        let (wallet, _wallet_home) = temporary_wallet();

        let transaction = SpelProgram::new(&idl)
            .program(program_id)
            .public("initialize")
            .expect("test IDL must select")
            .build(&wallet)
            .await
            .expect("empty public instruction must build without a sequencer");

        assert_eq!(transaction.message.program_id, program_id);
        assert!(transaction.message.account_ids.is_empty());
        assert!(transaction.message.nonces.is_empty());
    }

    #[tokio::test]
    async fn public_build_wraps_resolver_error_before_wallet_access() {
        let idl = idl(vec![instruction_with_inputs(
            "initialize",
            vec![account("owner")],
            vec![],
        )]);
        let (wallet, _wallet_home) = temporary_wallet();

        let result = SpelProgram::new(&idl)
            .program([1; 8])
            .public("initialize")
            .expect("test IDL must select")
            .build(&wallet)
            .await;

        assert!(matches!(
            result,
            Err(SpelBuildError::Resolution(SpelTxError::MissingAccount { ref name }))
                if name == "owner"
        ));
    }

    #[tokio::test]
    async fn public_build_reports_missing_key_for_inferred_signer() {
        let mut authority = account("authority");
        authority.signer = true;
        let idl = idl(vec![instruction_with_inputs(
            "initialize",
            vec![authority],
            vec![],
        )]);
        let (wallet, _wallet_home) = temporary_wallet();

        let result = SpelProgram::new(&idl)
            .program([1; 8])
            .public("initialize")
            .expect("test IDL must select")
            .input("authority", AccountId::new([1; 32]))
            .expect("bare signer must classify")
            .build(&wallet)
            .await;

        assert!(matches!(
            result,
            Err(SpelBuildError::MissingPublicSigningKey { ref account })
                if account == "authority"
        ));
    }

    #[tokio::test]
    async fn public_build_reports_missing_key_for_inferred_initialized_account() {
        let mut state = account("state");
        state.init = true;
        state.writable = true;
        let idl = idl(vec![instruction_with_inputs(
            "initialize",
            vec![state],
            vec![],
        )]);
        let (wallet, _wallet_home) = temporary_wallet();

        let result = SpelProgram::new(&idl)
            .program([1; 8])
            .public("initialize")
            .expect("test IDL must select")
            .input("state", AccountId::new([2; 32]))
            .expect("bare initialized account must classify")
            .build(&wallet)
            .await;

        assert!(matches!(
            result,
            Err(SpelBuildError::MissingPublicSigningKey { ref account }) if account == "state"
        ));
    }
}
