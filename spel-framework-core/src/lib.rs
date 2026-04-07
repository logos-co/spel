//! # SPEL Framework Core
//!
//! Core types and traits for the SPEL program framework.

pub mod error;
pub mod types;
pub mod idl;
pub mod pda;
pub mod validation;

#[cfg(feature = "idl-gen")]
pub mod idl_gen;

pub mod prelude {
    pub use crate::error::{SpelError, SpelResult};
    pub use crate::pda::{compute_pda, seed_from_str};
    pub use crate::types::{SpelOutput, AccountConstraint};
    pub use nssa_core::account::{Account, AccountWithMetadata};
    pub use nssa_core::program::{AccountPostState, ChainedCall, PdaSeed, ProgramId};
}
