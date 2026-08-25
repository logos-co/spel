//! Core types for the SPEL framework.
//!
//! These are thin wrappers/adapters that bridge framework ergonomics
//! with real SPEL core types.

use lee_core::program::{
    AccountPostState, BlockValidityWindow, ChainedCall, InvalidWindow, TimestampValidityWindow,
    ValidityWindow,
};

/// Trait for types that can be converted into an [`AccountPostState`].
///
/// Implemented for `(Account, AutoClaim)`, `(Account, &AutoClaim)`, and
/// `AccountPostState` itself, so [`SpelOutput::execute`] accepts any of these.
pub trait IntoPostState {
    fn into_post_state(self) -> AccountPostState;
}

/// Output from an instruction handler.
///
/// This struct is `#[non_exhaustive]` to allow future field additions without
/// breaking external programs that construct it via struct literals.
/// Use the builder methods (`execute()`, `with_*()`) instead.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SpelOutput {
    pub post_states: Vec<AccountPostState>,
    pub chained_calls: Vec<ChainedCall>,
    pub block_validity_window: BlockValidityWindow,
    pub timestamp_validity_window: TimestampValidityWindow,
}

impl SpelOutput {
    /// Create an empty output.
    pub fn empty() -> Self {
        Self {
            post_states: vec![],
            chained_calls: vec![],
            block_validity_window: ValidityWindow::new_unbounded(),
            timestamp_validity_window: ValidityWindow::new_unbounded(),
        }
    }

    /// Restrict the block range in which the transaction is valid (infallible).
    ///
    /// Accepts any infallible range conversion: `1..`, `..100`, or `..` (unbounded).
    /// For the fallible variant (returns `Err` on empty/inverted ranges), see
    /// [`try_with_block_validity_window`](Self::try_with_block_validity_window).
    pub fn with_block_validity_window<W: Into<BlockValidityWindow>>(mut self, window: W) -> Self {
        self.block_validity_window = window.into();
        self
    }

    /// Restrict the block range in which the transaction is valid (fallible).
    ///
    /// Returns `Err(InvalidWindow)` if `window` is an empty range (e.g. `5..5` or `10..5`).
    /// For the infallible variant, see [`with_block_validity_window`](Self::with_block_validity_window).
    pub fn try_with_block_validity_window<
        W: TryInto<BlockValidityWindow, Error = InvalidWindow>,
    >(
        mut self,
        window: W,
    ) -> Result<Self, InvalidWindow> {
        self.block_validity_window = window.try_into()?;
        Ok(self)
    }

    /// Restrict the timestamp range in which the transaction is valid (infallible).
    ///
    /// Accepts any infallible range conversion: `1..`, `..100`, or `..` (unbounded).
    /// For the fallible variant (returns `Err` on empty/inverted ranges), see
    /// [`try_with_timestamp_validity_window`](Self::try_with_timestamp_validity_window).
    pub fn with_timestamp_validity_window<W: Into<TimestampValidityWindow>>(
        mut self,
        window: W,
    ) -> Self {
        self.timestamp_validity_window = window.into();
        self
    }

    /// Restrict the timestamp range in which the transaction is valid (fallible).
    ///
    /// Returns `Err(InvalidWindow)` if `window` is an empty range (e.g. `5..5` or `10..5`).
    /// For the infallible variant, see [`with_timestamp_validity_window`](Self::with_timestamp_validity_window).
    pub fn try_with_timestamp_validity_window<
        W: TryInto<TimestampValidityWindow, Error = InvalidWindow>,
    >(
        mut self,
        window: W,
    ) -> Result<Self, InvalidWindow> {
        self.timestamp_validity_window = window.try_into()?;
        Ok(self)
    }

    /// Destructure into individual components.
    ///
    /// Returns a [`SpelOutputParts`] struct instead of a raw tuple so that
    /// future field additions don't require updating every call site.
    pub fn into_parts(self) -> SpelOutputParts {
        SpelOutputParts {
            post_states: self.post_states,
            chained_calls: self.chained_calls,
            block_validity_window: self.block_validity_window,
            timestamp_validity_window: self.timestamp_validity_window,
        }
    }
}

/// Components of a [`SpelOutput`] after destructuring via [`SpelOutput::into_parts`].
///
/// This struct is `#[non_exhaustive]` so future field additions don't break
/// callers that pattern-match on it.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SpelOutputParts {
    /// Post-transaction account states (claims, mutability).
    pub post_states: Vec<AccountPostState>,
    /// Chained calls to other programs.
    pub chained_calls: Vec<ChainedCall>,
    /// Block range in which the transaction is valid.
    pub block_validity_window: BlockValidityWindow,
    /// Timestamp range in which the transaction is valid.
    pub timestamp_validity_window: TimestampValidityWindow,
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
