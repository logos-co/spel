//! # LEZ Framework
//!
//! Developer framework for building programs on LEZ,
//! similar to Anchor for Solana.

// Re-export the proc macros
pub use lez_framework_macros::{lez_program, instruction, generate_idl};

// Re-export core types
pub use lez_framework_core::*;

pub mod prelude {
    pub use crate::lez_program;
    pub use crate::instruction;
    pub use lez_framework_core::prelude::*;
    pub use lez_framework_core::types::LezOutput;
    pub use lez_framework_core::error::{LezError, LezResult};
    pub use borsh::{BorshSerialize, BorshDeserialize};
}
