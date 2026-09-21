//! # SPEL Framework
//!
//! Developer framework for building programs on SPEL,
//! similar to Anchor for Solana.

// Re-export the proc macros
pub use spel_framework_macros::{account_type, generate_idl, instruction, lez_program};

// Re-export core types
pub use spel_framework_core::types::{SpelOutput, SpelOutputParts};
pub use spel_framework_core::*;

// Re-export serde_json and borsh for use in generated code.
//
// borsh is the instruction wire format LEZ deserializes, so re-exporting it here
// pins one version against lee_core's rather than leaving each program to declare
// its own — a mismatch would be a silent encoding bug, not a build error.
pub use borsh;
pub use serde_json;

pub mod prelude {
    pub use crate::account_type;
    pub use crate::instruction;
    pub use crate::lez_program;
    pub use borsh::{BorshDeserialize, BorshSerialize};
    pub use spel_framework_core::error::{SpelError, SpelResult};
    pub use spel_framework_core::prelude::*;
    pub use spel_framework_core::types::SpelOutput;

    // nssa::public_transaction (host-only)
    #[cfg(feature = "host")]
    pub use spel_framework_core::prelude::{Message, WitnessSet};
}
