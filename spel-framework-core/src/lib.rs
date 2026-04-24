//! # SPEL Framework Core
//!
//! Core types and traits for the SPEL program framework.

pub mod error;
pub mod types;
pub mod spel_output;
pub mod idl;
pub mod pda;
pub mod validation;

#[cfg(feature = "idl-gen")]
pub mod idl_gen;

pub mod prelude {
    pub use crate::error::{SpelError, SpelResult};
    pub use crate::pda::{compute_pda, compute_pda_multi, seed_from_str, ToSeed};
    pub use crate::spel_output::AutoClaim;
    pub use crate::types::{IntoPostState, SpelOutput, AccountConstraint};
    pub use nssa_core::account::{Account, AccountWithMetadata};
    pub use nssa_core::program::{
        AccountPostState, BlockValidityWindow, ChainedCall, Claim, InvalidWindow, PdaSeed,
        ProgramId, TimestampValidityWindow, ValidityWindow,
    };
    pub use nssa_core::{BlockId, Timestamp};
}
