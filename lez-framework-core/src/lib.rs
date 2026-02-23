//! # LEZ Framework Core
//!
//! Core types and traits for the LEZ program framework.

pub mod error;
pub mod types;
pub mod idl;
pub mod validation;

pub mod prelude {
    pub use crate::error::{LezError, LezResult};
    pub use crate::types::{LezOutput, AccountConstraint};
    pub use nssa_core::account::{Account, AccountWithMetadata};
    pub use nssa_core::program::{AccountPostState, ChainedCall, PdaSeed, ProgramId};
}
