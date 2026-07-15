use std::collections::BTreeMap;

use nssa_core::program::ProgramId;
use spel_framework_core::{
    idl::{IdlAccountItem, IdlInstruction, IdlSeed},
    pda::{compute_pda, compute_private_pda_with_identifier},
};
use wallet::AccountIdentity;

use super::{resolution::SpelTxError, value::ArgumentValue};

enum PdaState {
    Visiting,
    Done(AccountIdentity),
}

pub(crate) fn account_seed_target<'a>(
    instruction: &'a IdlInstruction,
    path: &str,
) -> Option<&'a IdlAccountItem> {
    instruction
        .accounts
        .iter()
        .find(|account| account.name == path)
        .or_else(|| {
            path.strip_suffix(".id")
                .and_then(|name| (!name.is_empty()).then_some(name))
                .and_then(|name| {
                    instruction
                        .accounts
                        .iter()
                        .find(|account| account.name == name)
                })
        })
}

pub(crate) fn resolve_pdas(
    instruction: &IdlInstruction,
    inputs: &BTreeMap<String, Vec<AccountIdentity>>,
    arguments: &BTreeMap<String, ArgumentValue>,
    program_id: ProgramId,
    private_path: bool,
) -> Result<BTreeMap<String, AccountIdentity>, SpelTxError> {
    let pda_names: Vec<_> = instruction
        .accounts
        .iter()
        .filter(|account| account.pda.is_some())
        .map(|account| account.name.clone())
        .collect();
    let mut resolver = PdaResolver {
        instruction,
        inputs,
        arguments,
        program_id,
        private_path,
        states: BTreeMap::new(),
    };

    for name in &pda_names {
        resolver.resolve_pda(name)?;
    }

    Ok(resolver
        .states
        .into_iter()
        .filter_map(|(name, state)| match state {
            PdaState::Done(identity) => Some((name, identity)),
            PdaState::Visiting => None,
        })
        .collect())
}

struct PdaResolver<'a> {
    instruction: &'a IdlInstruction,
    inputs: &'a BTreeMap<String, Vec<AccountIdentity>>,
    arguments: &'a BTreeMap<String, ArgumentValue>,
    program_id: ProgramId,
    private_path: bool,
    states: BTreeMap<String, PdaState>,
}

impl PdaResolver<'_> {
    fn resolve_pda(&mut self, name: &str) -> Result<AccountIdentity, SpelTxError> {
        if let Some(state) = self.states.get(name) {
            return match state {
                PdaState::Done(identity) => Ok(identity.clone()),
                PdaState::Visiting => Err(SpelTxError::PdaResolution {
                    account: name.to_owned(),
                    seed_index: None,
                    reason: "PDA dependency cycle".to_string(),
                }),
            };
        }

        let account = self
            .account(name)
            .ok_or_else(|| SpelTxError::PdaResolution {
                account: name.to_owned(),
                seed_index: None,
                reason: "PDA account is not declared".to_string(),
            })?
            .clone();
        let pda = account
            .pda
            .clone()
            .ok_or_else(|| SpelTxError::PdaResolution {
                account: name.to_owned(),
                seed_index: None,
                reason: "account is not a PDA".to_string(),
            })?;

        self.states.insert(name.to_owned(), PdaState::Visiting);

        let mut seeds = Vec::with_capacity(pda.seeds.len());
        for (seed_index, seed) in pda.seeds.iter().enumerate() {
            seeds.push(self.resolve_seed(&account.name, seed_index, seed)?);
        }
        let seed_refs: Vec<_> = seeds.iter().collect();

        let identity = if pda.private {
            self.resolve_private_pda(&account, &seed_refs)?
        } else {
            AccountIdentity::PublicNoSign(compute_pda(&self.program_id, &seed_refs))
        };

        self.states
            .insert(account.name.clone(), PdaState::Done(identity.clone()));
        Ok(identity)
    }

    fn resolve_seed(
        &mut self,
        account: &str,
        seed_index: usize,
        seed: &IdlSeed,
    ) -> Result<[u8; 32], SpelTxError> {
        match seed {
            IdlSeed::Const { value } => constant_seed(value).ok_or_else(|| {
                self.pda_error(account, Some(seed_index), "constant seed exceeds 32 bytes")
            }),
            IdlSeed::Account { path } => {
                let target = account_seed_target(self.instruction, path)
                    .ok_or_else(|| {
                        self.pda_error(
                            account,
                            Some(seed_index),
                            "account seed references an undeclared account",
                        )
                    })?
                    .clone();
                if target.rest {
                    return Err(self.pda_error(
                        account,
                        Some(seed_index),
                        "account seed cannot reference a rest account",
                    ));
                }

                let identity = if target.pda.is_some() {
                    if matches!(self.states.get(&target.name), Some(PdaState::Visiting)) {
                        return Err(self.pda_error(
                            account,
                            Some(seed_index),
                            "PDA dependency cycle",
                        ));
                    }
                    self.resolve_pda(&target.name)?
                } else {
                    self.inputs
                        .get(&target.name)
                        .and_then(|identities| identities.first())
                        .cloned()
                        .ok_or_else(|| {
                            self.pda_error(
                                account,
                                Some(seed_index),
                                "account seed input is unavailable",
                            )
                        })?
                };
                Ok(*identity.account_id().value())
            },
            IdlSeed::Arg { path } => {
                let argument = self
                    .instruction
                    .args
                    .iter()
                    .find(|argument| argument.name == *path)
                    .ok_or_else(|| {
                        self.pda_error(
                            account,
                            Some(seed_index),
                            "argument seed references an undeclared argument",
                        )
                    })?;
                let value = self.arguments.get(path).ok_or_else(|| {
                    self.pda_error(
                        account,
                        Some(seed_index),
                        "argument seed input is unavailable",
                    )
                })?;
                value
                    .seed_bytes(&argument.type_)
                    .map_err(|reason| self.pda_error(account, Some(seed_index), reason))
            },
        }
    }

    fn resolve_private_pda(
        &self,
        account: &IdlAccountItem,
        seeds: &[&[u8; 32]],
    ) -> Result<AccountIdentity, SpelTxError> {
        if !self.private_path {
            return Err(SpelTxError::InvalidIdl {
                instruction: self.instruction.name.clone(),
                path: format!("accounts.{}", account.name),
                reason: "public resolution does not support private PDAs".to_string(),
            });
        }

        let identity = self
            .inputs
            .get(&account.name)
            .and_then(|identities| identities.first())
            .cloned()
            .ok_or_else(|| SpelTxError::MissingAccount {
                name: account.name.clone(),
            })?;
        let (npk, identifier) = match &identity {
            AccountIdentity::PrivatePdaForeign {
                npk, identifier, ..
            }
            | AccountIdentity::PrivatePdaShared {
                npk, identifier, ..
            } => (npk, *identifier),
            _ => {
                return Err(SpelTxError::InvalidAccount {
                    account: account.name.clone(),
                    index: self.account_index(&account.name),
                    reason: "private PDA initialization requires a foreign or shared private PDA identity"
                        .to_string(),
                });
            },
        };

        let expected =
            compute_private_pda_with_identifier(&self.program_id, seeds, npk, identifier);
        if identity.account_id() != expected {
            return Err(SpelTxError::InvalidAccount {
                account: account.name.clone(),
                index: self.account_index(&account.name),
                reason: "private PDA identity does not match the derived address".to_string(),
            });
        }

        Ok(identity)
    }

    fn account(&self, name: &str) -> Option<&IdlAccountItem> {
        self.instruction
            .accounts
            .iter()
            .find(|account| account.name == name)
    }

    fn account_index(&self, name: &str) -> usize {
        self.instruction
            .accounts
            .iter()
            .position(|account| account.name == name)
            .unwrap_or_default()
    }

    fn pda_error(
        &self,
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
}

fn constant_seed(value: &str) -> Option<[u8; 32]> {
    (value.len() <= 32).then(|| {
        let mut seed = [0; 32];
        seed[..value.len()].copy_from_slice(value.as_bytes());
        seed
    })
}
