//! Core types for the SPEL framework.
//!
//! These are thin wrappers/adapters that bridge framework ergonomics
//! with real SPEL core types.

use nssa_core::program::{AccountPostState, BlockValidityWindow, ChainedCall, InvalidWindow, TimestampValidityWindow, ValidityWindow};

/// Trait for types that can be converted into an [`AccountPostState`].
///
/// Implemented for `(Account, AutoClaim)`, `(Account, &AutoClaim)`, and
/// `AccountPostState` itself, so [`SpelOutput::execute`] accepts any of these.
pub trait IntoPostState {
    fn into_post_state(self) -> AccountPostState;
}

/// Output from an instruction handler.
#[derive(Debug, Clone)]
pub struct SpelOutput {
    pub post_states: Vec<AccountPostState>,
    pub chained_calls: Vec<ChainedCall>,
    pub block_validity_window: BlockValidityWindow,
    pub timestamp_validity_window: TimestampValidityWindow,
}

impl SpelOutput {
    /// Create output with only post-states and no chained calls.
    #[deprecated(note = "Use SpelOutput::execute() for auto-claim support")]
    pub fn states_only(post_states: Vec<AccountPostState>) -> Self {
        Self {
            post_states,
            chained_calls: vec![],
            block_validity_window: ValidityWindow::new_unbounded(),
            timestamp_validity_window: ValidityWindow::new_unbounded(),
        }
    }

    /// Create output with post-states and chained calls.
    #[deprecated(note = "Use SpelOutput::execute() for auto-claim support")]
    pub fn with_chained_calls(
        post_states: Vec<AccountPostState>,
        chained_calls: Vec<ChainedCall>,
    ) -> Self {
        Self {
            post_states,
            chained_calls,
            block_validity_window: ValidityWindow::new_unbounded(),
            timestamp_validity_window: ValidityWindow::new_unbounded(),
        }
    }

    /// Create an empty output.
    pub fn empty() -> Self {
        Self {
            post_states: vec![],
            chained_calls: vec![],
            block_validity_window: ValidityWindow::new_unbounded(),
            timestamp_validity_window: ValidityWindow::new_unbounded(),
        }
    }

    /// Restrict the block range in which the transaction is valid.
    ///
    /// Accepts any infallible range conversion: `1..`, `..100`, or `..` (unbounded).
    pub fn with_block_validity_window<W: Into<BlockValidityWindow>>(mut self, window: W) -> Self {
        self.block_validity_window = window.into();
        self
    }

    /// Restrict the block range in which the transaction is valid.
    ///
    /// Returns `Err` if `window` is an empty range (e.g. `5..5` or `10..5`).
    pub fn try_with_block_validity_window<W: TryInto<BlockValidityWindow, Error = InvalidWindow>>(
        mut self,
        window: W,
    ) -> Result<Self, InvalidWindow> {
        self.block_validity_window = window.try_into()?;
        Ok(self)
    }

    /// Restrict the timestamp range in which the transaction is valid.
    ///
    /// Accepts any infallible range conversion: `1..`, `..100`, or `..` (unbounded).
    pub fn with_timestamp_validity_window<W: Into<TimestampValidityWindow>>(
        mut self,
        window: W,
    ) -> Self {
        self.timestamp_validity_window = window.into();
        self
    }

    /// Restrict the timestamp range in which the transaction is valid.
    ///
    /// Returns `Err` if `window` is an empty range (e.g. `5..5` or `10..5`).
    pub fn try_with_timestamp_validity_window<
        W: TryInto<TimestampValidityWindow, Error = InvalidWindow>,
    >(
        mut self,
        window: W,
    ) -> Result<Self, InvalidWindow> {
        self.timestamp_validity_window = window.try_into()?;
        Ok(self)
    }

    /// Convert to the original tuple form of post-states and chained calls.
    #[deprecated(note = "Use SpelOutput::into_parts_with_windows() to also retrieve validity windows")]
    pub fn into_parts(self) -> (Vec<AccountPostState>, Vec<ChainedCall>) {
        (self.post_states, self.chained_calls)
    }

    /// Convert to the tuple form including validity windows.
    pub fn into_parts_with_windows(
        self,
    ) -> (
        Vec<AccountPostState>,
        Vec<ChainedCall>,
        BlockValidityWindow,
        TimestampValidityWindow,
    ) {
        (
            self.post_states,
            self.chained_calls,
            self.block_validity_window,
            self.timestamp_validity_window,
        )
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
