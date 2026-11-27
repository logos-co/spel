//! Core types for the NFT Program: actor state, messages and events.
//!
//! State and messages outlive code (state stays on accounts, async messages in mailboxes),
//! and Borsh encodes an enum variant as a leading tag byte. Every enum here is therefore
//! append-only: a changed layout is a new variant, never an edit of an existing one.

use borsh::{BorshDeserialize, BorshSerialize};
use lee_core::account::AccountId;
use serde::{Deserialize, Serialize};

pub mod event;

/// Longest NFT name, in bytes of UTF-8. Every later message to a definition loads it.
pub const MAX_NAME_LEN: usize = 64;
/// Longest metadata `uri`, in bytes of UTF-8.
pub const MAX_URI_LEN: usize = 256;
/// Longest metadata `creators`, in bytes of UTF-8.
pub const MAX_CREATORS_LEN: usize = 256;

/// The NFT program's state on one account: a definition, its master, or a printed copy.
///
/// An empty actor state means "nothing here yet" and is not an `NftState`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub enum NftState {
    Definition(NftDefinition),
    Master(NftMaster),
    Copy(NftCopy),
}

impl NftState {
    /// Decodes an actor state; `None` for an empty one.
    ///
    /// # Panics
    /// If the bytes are not an `NftState`. Only NFT code writes NFT actor state.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.is_empty() {
            return None;
        }
        Some(borsh::from_slice(bytes).expect("nft actor state must decode"))
    }

    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        borsh::to_vec(self).expect("borsh serialization is infallible")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct NftDefinition {
    pub name: String,
    /// The master's print rights plus the owned copies. Fixed at creation; only burns lower it.
    pub printable_supply: u128,
    pub metadata: NftMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct NftMetadata {
    pub standard: MetadataStandard,
    pub uri: String,
    pub creators: String,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize,
)]
pub enum MetadataStandard {
    Simple,
    Expanded,
}

/// The holding that carries the print rights. `print_balance` counts its own unit, so it
/// prints while above 1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct NftMaster {
    pub definition_id: AccountId,
    pub print_balance: u128,
}

/// A printed edition. `owned: false` is a holding that holds nothing, as initialization
/// writes it and as a transfer or burn leaves it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct NftCopy {
    pub definition_id: AccountId,
    pub owned: bool,
}

/// What a credit carries: a master's whole print balance, or one copy.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize,
)]
pub enum NftValue {
    Master(u128),
    Copy,
}

/// Messages anyone may send: a transaction author or another program.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum Message {
    /// Create an NFT with `printable_supply` units, all on the master.
    ///
    /// Sent to the empty definition actor, which must be authorized. The master must be
    /// empty and authorized too.
    NewDefinition {
        name: String,
        printable_supply: u128,
        metadata: NftMetadata,
        master: AccountId,
    },

    /// Create a holding of `definition` that holds nothing; it can then receive a master or
    /// a copy.
    ///
    /// Sent to the empty, authorized holding actor, which asks the definition to
    /// initialize it. A holding of the same definition makes this a no-op.
    InitializeHolding { definition: AccountId },

    /// Move the receiving holding's whole value (authorized) to the holding at `to`, which
    /// must be a holding of the same definition that holds nothing.
    Transfer { to: AccountId },

    /// Print one copy from the receiving master (authorized) to the holding at `to`, which
    /// must be a holding of the same definition that holds nothing.
    PrintNft { to: AccountId },

    /// Destroy `amount` from the receiving holding (authorized): up to the print balance of
    /// a master, exactly 1 of an owned copy.
    Burn { amount: u128 },

    /// Reply to the sender with this actor's state, as a `lee_core::program::StateReply`.
    ReadState,
}

/// Messages only NFT actors send each other, named for what they do at the receiver.
/// The guest decodes these only when the sender is the NFT program itself, so each is
/// trusted because only NFT code can have produced it.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum Internal {
    /// To the empty master, from its definition at creation: write a master of
    /// `print_balance`.
    InitMaster {
        definition: AccountId,
        print_balance: u128,
    },
    /// To the definition an empty holding names: reply with `InitHolding`.
    RequestHolding,
    /// To an empty holding, from its definition: write a holding that holds nothing.
    InitHolding { definition: AccountId },
    /// To a holding of `definition` that holds nothing: take `value`. Sent async by a
    /// holding that has just given up the same value.
    Receive {
        definition: AccountId,
        value: NftValue,
    },
    /// To a holding of `definition` that holds nothing: take a new copy. Sent async by
    /// the master after lowering its print balance by one.
    ReceivePrint { definition: AccountId },
    /// To a holding's own definition, after the holding burned `amount`.
    DecreaseSupply { amount: u128 },
}
