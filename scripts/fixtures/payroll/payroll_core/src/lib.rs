//! Types shared by the payroll program and its clients. A seat pays employees from its treasury:
//! a private PDA of this program, keyed by the seat's own keys, with the seat's id as its seed.
//! Only this program can move its funds, by granting that seed.
//!
//! Borsh encodes an enum variant as a leading tag byte, so the message enum is append-only.

use borsh::{BorshDeserialize, BorshSerialize};
use lee_core::account::AccountId;

/// What the program accepts.
#[cfg_attr(feature = "idl", spel_attrs::message)]
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum Message {
    /// Open the receiving seat with `treasury`, the seat's private PDA of this program (seed: the
    /// seat's id). The seat must authorize, and must not be open yet.
    Open { treasury: AccountId },
    /// Pay `amount` of the treasury's native balance to `employee`. The seat must authorize.
    Pay { employee: AccountId, amount: u128 },
}

/// The program's state on an open seat.
#[cfg_attr(feature = "idl", spel_attrs::state)]
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Seat {
    pub treasury: AccountId,
    /// Everything paid out so far.
    pub paid: u128,
}
