//! Execution context exposed to SPEL instruction handlers.
//!
//! When an `#[instruction]` handler declares a parameter of type
//! [`ProgramContext`], the macro-generated dispatcher injects the trusted
//! values from [`nssa_core::program::ProgramInput`] at call time.
//! The context parameter is **never** part of the instruction ABI or IDL.

use crate::prelude::{BlockId, ProgramId, Timestamp};

/// Trusted execution metadata supplied by the SPEL guest entrypoint.
///
/// Use this as a parameter on `#[instruction]` functions to access
/// `self_program_id` and `caller_program_id` without adding them to
/// the instruction schema:
///
/// ```ignore
/// #[instruction]
/// pub fn initialize(
///     ctx: ProgramContext,
///     #[account(owner = self_program_id)]
///     definition: AccountWithMetadata,
/// ) -> SpelResult {
///     // ctx.self_program_id is the currently executing program
/// }
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProgramContext {
    /// The program ID of the currently executing program.
    pub self_program_id: ProgramId,
    /// The program ID of the caller (the program that invoked this one).
    /// If there is no explicit caller (e.g. top-level transaction),
    /// this is set to [`nssa_core::program::DEFAULT_PROGRAM_ID`] (all zeros).
    pub caller_program_id: ProgramId,
}

impl ProgramContext {
    /// Create a new context from program input values.
    #[must_use]
    pub const fn new(self_program_id: ProgramId, caller_program_id: ProgramId) -> Self {
        Self {
            self_program_id,
            caller_program_id,
        }
    }
}

/// Clock metadata injected by the SPEL dispatcher when an `#[instruction]` handler
/// declares a `ClockContext` parameter.
///
/// Use this to access the current block number and timestamp without adding
/// them to the instruction schema. Like [`ProgramContext`], it is **never**
/// part of the instruction ABI or IDL. The dispatcher reads it from the
/// LEZ clock account (`/LEZ/ClockProgramAccount/0000001`), which the
/// transaction builder automatically prepends to the account list.
///
/// ```ignore
/// #[instruction]
/// pub fn time_locked_transfer(
///     ctx: ProgramContext,
///     clock: ClockContext,
///     #[account(owner = self_program_id)]
///     vault: AccountWithMetadata,
///     unlock_timestamp: u64,
/// ) -> SpelResult {
///     if clock.timestamp < unlock_timestamp {
///         return Err(SpelError::new(1, "still locked"));
///     }
/// }
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, borsh::BorshDeserialize)]
pub struct ClockContext {
    /// The block number at the time of execution.
    pub block_id: BlockId,
    /// The block timestamp in milliseconds since the Unix epoch.
    pub timestamp: Timestamp,
}

impl ClockContext {
    #[must_use]
    pub const fn new(block_id: BlockId, timestamp: Timestamp) -> Self {
        Self { block_id, timestamp }
    }
}
