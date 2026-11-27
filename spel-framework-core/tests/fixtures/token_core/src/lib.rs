//! Core types for the fungible Token Program: actor state, messages and events.
//!
//! State and messages outlive code (state stays on accounts, async messages in mailboxes),
//! and Borsh encodes an enum variant as a leading tag byte. Every enum here is therefore
//! append-only: a changed layout is a new variant, never an edit of an existing one.

use borsh::{BorshDeserialize, BorshSerialize};
use lee_core::account::AccountId;
use serde::{Deserialize, Serialize};

pub mod event;

/// Longest token name, in bytes of UTF-8. Every later message to a definition loads it.
pub const MAX_NAME_LEN: usize = 64;

/// The token program's state on one account: a definition or a holding.
///
/// An empty actor state means "nothing here yet" and is not a `TokenState`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub enum TokenState {
    Definition(TokenDefinition),
    Holding(TokenHolding),
}

impl TokenState {
    /// Decodes an actor state; `None` for an empty one.
    ///
    /// # Panics
    /// If the bytes are not a `TokenState`. Only token code writes token actor state.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.is_empty() {
            return None;
        }
        Some(borsh::from_slice(bytes).expect("token actor state must decode"))
    }

    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        borsh::to_vec(self).expect("borsh serialization is infallible")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct TokenDefinition {
    pub name: String,
    pub total_supply: u128,
    /// `Some(id)` may mint and rotate or renounce the authority; `None` fixes the supply.
    pub authority: Option<AccountId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct TokenHolding {
    pub definition_id: AccountId,
    pub balance: u128,
}

/// Messages anyone may send: a transaction author or another program.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum Message {
    /// Create a fungible token.
    ///
    /// Sent to the empty definition actor, which must be authorized. The initial supply
    /// goes to `holding`, which must be empty and authorized too.
    NewDefinition {
        name: String,
        total_supply: u128,
        /// `Some(id)` makes the token mintable by `id`; `None` fixes the supply.
        mint_authority: Option<AccountId>,
        holding: AccountId,
    },

    /// Create a zero holding of `definition`.
    ///
    /// Sent to the empty, authorized holding actor, which asks the definition to
    /// initialize it. A holding of the same definition makes this a no-op.
    InitializeHolding { definition: AccountId },

    /// Move `amount` from the receiving holding (authorized) to the holding at `to`,
    /// which must already hold the same token.
    Transfer { to: AccountId, amount: u128 },

    /// Destroy `amount` from the receiving holding (authorized).
    Burn { amount: u128 },

    /// Mint `amount` of `definition` into the holding at `to`.
    ///
    /// Sent to the definition when it is its own authority (and authorized), otherwise to
    /// the authority's actor (authorized), which forwards it.
    Mint {
        definition: AccountId,
        to: AccountId,
        amount: u128,
    },

    /// Rotate (`Some`) or renounce (`None`) the mint authority. Routed like `Mint`.
    SetAuthority {
        definition: AccountId,
        new_authority: Option<AccountId>,
    },

    /// Reply to the sender with this actor's state, as a `lee_core::program::StateReply`.
    ReadState,
}

/// Messages only token actors send each other, named for what they do at the receiver.
/// The guest decodes these only when the sender is the token program itself, so each is
/// trusted because only token code can have produced it.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum Internal {
    /// To a holding of `definition`: add `amount`. Sent async by a holding that debited
    /// the same amount, or by the definition after raising its supply.
    IncreaseBalance { definition: AccountId, amount: u128 },
    /// To the definition an empty holding names: reply with `InitHolding` (amount 0).
    RequestHolding,
    /// To an empty holding, from its definition: write a holding of `amount`, the initial
    /// supply at creation or 0 in reply to `RequestHolding`.
    InitHolding { definition: AccountId, amount: u128 },
    /// To a holding's own definition, after the holding burned `amount`.
    DecreaseSupply { amount: u128 },
    /// To the definition, from its authority's actor: raise the supply by `amount` and
    /// credit it to `to`.
    IncreaseSupply { to: AccountId, amount: u128 },
    /// To the definition, from its authority's actor: rotate or renounce the authority.
    ChangeAuthority { new_authority: Option<AccountId> },
}
