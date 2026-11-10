//! Events, emitted by the actor whose state changed. The indexer records the emitting
//! program, not the account, so every event names its accounts.

use borsh::{BorshDeserialize, BorshSerialize};
use lee_core::{account::AccountId, program::ProgramEvent};
use risc0_zkvm::sha::{Impl, Sha256 as _};

pub trait TokenEvent: BorshSerialize {
    const NAME: &'static str;

    #[must_use]
    fn to_program_event(&self) -> ProgramEvent {
        ProgramEvent {
            selector: selector(Self::NAME),
            data: borsh::to_vec(self).expect("borsh serialization is infallible"),
        }
    }
}

/// `sha256("token::<name>")[..8]`, the LEZ selector convention.
#[must_use]
pub fn selector(name: &str) -> [u8; 8] {
    let digest = Impl::hash_bytes(format!("token::{name}").as_bytes());
    *digest
        .as_bytes()
        .first_chunk::<8>()
        .expect("a sha256 digest has 32 bytes")
}

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct NewDefinition {
    pub definition: AccountId,
    pub holding: AccountId,
    pub name: String,
    pub total_supply: u128,
    pub authority: Option<AccountId>,
}

impl TokenEvent for NewDefinition {
    const NAME: &'static str = "NewDefinition";
}

/// Emitted by the recipient holding once credited.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Transferred {
    pub definition: AccountId,
    pub from: AccountId,
    pub to: AccountId,
    pub amount: u128,
}

impl TokenEvent for Transferred {
    const NAME: &'static str = "Transferred";
}

/// Emitted by the definition as it raises its supply.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Minted {
    pub definition: AccountId,
    pub to: AccountId,
    pub amount: u128,
}

impl TokenEvent for Minted {
    const NAME: &'static str = "Minted";
}

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Burned {
    pub definition: AccountId,
    pub holding: AccountId,
    pub amount: u128,
}

impl TokenEvent for Burned {
    const NAME: &'static str = "Burned";
}

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct AuthorityChanged {
    pub definition: AccountId,
    pub old: Option<AccountId>,
    pub new: Option<AccountId>,
}

impl TokenEvent for AuthorityChanged {
    const NAME: &'static str = "AuthorityChanged";
}
