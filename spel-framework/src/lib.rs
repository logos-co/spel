//! # SPEL Framework
//!
//! Developer framework for building programs on SPEL,
//! similar to Anchor for Solana.

// Re-export the proc macros
pub use spel_framework_macros::{lez_program, instruction, account_type, generate_idl};

// Re-export core types
pub use spel_framework_core::*;

pub mod prelude {
    pub use crate::lez_program;
    pub use crate::instruction;
    pub use crate::account_type;
    pub use spel_framework_core::prelude::*;
    pub use spel_framework_core::types::SpelOutput;
    pub use spel_framework_core::error::{SpelError, SpelResult};
    pub use borsh::{BorshSerialize, BorshDeserialize};
}
