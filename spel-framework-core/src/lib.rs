//! # SPEL Framework Core
//!
//! Core types and traits for the SPEL program framework.

pub mod context;
#[cfg(feature = "decode")]
pub mod decode;
pub mod error;
pub mod idl;
pub mod pda;
pub mod spel_output;
pub mod types;
pub mod validation;

#[cfg(feature = "idl-gen")]
pub mod account_types;
#[cfg(feature = "idl-gen")]
pub mod idl_gen;

pub mod prelude {
    pub use crate::error::{SpelError, SpelResult};
    pub use crate::pda::{compute_pda, compute_pda_multi, seed_from_str, ToSeed};
    pub use crate::spel_output::AutoClaim;
    pub use crate::types::{AccountConstraint, IntoPostState, SpelOutput, SpelOutputParts};

    // lee_core::account
    pub use lee_core::account::{Account, AccountId, AccountWithMetadata};

    // lee_core::program
    pub use lee_core::program::{
        AccountPostState, BlockValidityWindow, ChainedCall, Claim, InvalidWindow, PdaSeed,
        ProgramId, TimestampValidityWindow, ValidityWindow,
    };

    // lee_core extras
    pub use lee_core::{BlockId, Timestamp};

    // spel-framework additional re-exports
    pub use lee_core::program::{read_lee_inputs, InstructionData, ProgramInput, ProgramOutput};

    // Execution context for instruction handlers (issue #172)
    pub use crate::context::ProgramContext;

    // lee::public_transaction (host-only)
    #[cfg(feature = "host")]
    pub use lee::public_transaction::{Message, WitnessSet};
}
