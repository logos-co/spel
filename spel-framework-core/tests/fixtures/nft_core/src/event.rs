//! Events, emitted by the actor whose state changed. The indexer records the emitting
//! program, not the account, so every event names its accounts.

use borsh::{BorshDeserialize, BorshSerialize};
use lee_core::{account::AccountId, program::ProgramEvent};
use risc0_zkvm::sha::{Impl, Sha256 as _};

use crate::NftValue;

pub trait NftEvent: BorshSerialize {
    const NAME: &'static str;

    #[must_use]
    fn to_program_event(&self) -> ProgramEvent {
        ProgramEvent {
            selector: selector(Self::NAME),
            data: borsh::to_vec(self).expect("borsh serialization is infallible"),
        }
    }
}

/// `sha256("nft::<name>")[..8]`, the LEZ selector convention.
#[must_use]
pub fn selector(name: &str) -> [u8; 8] {
    let digest = Impl::hash_bytes(format!("nft::{name}").as_bytes());
    *digest
        .as_bytes()
        .first_chunk::<8>()
        .expect("a sha256 digest has 32 bytes")
}

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct NewDefinition {
    pub definition: AccountId,
    pub master: AccountId,
    pub name: String,
    pub printable_supply: u128,
}

impl NftEvent for NewDefinition {
    const NAME: &'static str = "NewDefinition";
}

/// Emitted by the recipient holding once credited.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Transferred {
    pub definition: AccountId,
    pub from: AccountId,
    pub to: AccountId,
    pub value: NftValue,
}

impl NftEvent for Transferred {
    const NAME: &'static str = "Transferred";
}

/// Emitted by the recipient holding once it holds the new copy.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Printed {
    pub definition: AccountId,
    pub master: AccountId,
    pub to: AccountId,
}

impl NftEvent for Printed {
    const NAME: &'static str = "Printed";
}

/// Emitted by the definition as it lowers its supply.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Burned {
    pub definition: AccountId,
    pub holding: AccountId,
    pub amount: u128,
}

impl NftEvent for Burned {
    const NAME: &'static str = "Burned";
}
