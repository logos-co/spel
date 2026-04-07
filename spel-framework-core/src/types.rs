//! Core types for the SPEL framework.
//!
//! These are thin wrappers/adapters that bridge framework ergonomics
//! with real SPEL core types.

use nssa_core::program::{AccountPostState, ChainedCall};

/// Output from an instruction handler.
#[derive(Debug, Clone)]
pub struct SpelOutput {
    pub post_states: Vec<AccountPostState>,
    pub chained_calls: Vec<ChainedCall>,
}

impl SpelOutput {
    /// Create output with only post-states and no chained calls.
    pub fn states_only(post_states: Vec<AccountPostState>) -> Self {
        Self {
            post_states,
            chained_calls: vec![],
        }
    }

    /// Create output with post-states and chained calls.
    pub fn with_chained_calls(
        post_states: Vec<AccountPostState>,
        chained_calls: Vec<ChainedCall>,
    ) -> Self {
        Self {
            post_states,
            chained_calls,
        }
    }

    /// Create an empty output.
    pub fn empty() -> Self {
        Self {
            post_states: vec![],
            chained_calls: vec![],
        }
    }

    /// Convert to the tuple form expected by `write_nssa_outputs_with_chained_call`.
    pub fn into_parts(self) -> (Vec<AccountPostState>, Vec<ChainedCall>) {
        (self.post_states, self.chained_calls)
    }
}

/// Account constraint flags used by the proc-macro.
#[derive(Debug, Clone, Default)]
pub struct AccountConstraint {
    pub mutable: bool,
    pub init: bool,
    pub owner: Option<[u8; 32]>,
    pub signer: bool,
    pub seeds: Option<Vec<Vec<u8>>>,
}

/// Metadata about an instruction, used for IDL generation.
#[derive(Debug, Clone)]
pub struct InstructionMeta {
    pub name: String,
    pub accounts: Vec<AccountMeta>,
    pub args: Vec<ArgMeta>,
}

/// Metadata about an account parameter.
#[derive(Debug, Clone)]
pub struct AccountMeta {
    pub name: String,
    pub writable: bool,
    pub init: bool,
    pub owner: Option<String>,
    pub signer: bool,
    pub pda_seeds: Option<Vec<String>>,
}

/// Metadata about an instruction argument.
#[derive(Debug, Clone)]
pub struct ArgMeta {
    pub name: String,
    pub type_name: String,
}
