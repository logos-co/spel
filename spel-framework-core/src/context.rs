//! Execution context exposed to SPEL instruction handlers.
//!
//! When an `#[instruction]` handler declares a parameter of type
//! [`ProgramContext`], the macro-generated dispatcher injects the trusted
//! values from [`lee_core::program::ProgramInput`] at call time.
//! The context parameter is **never** part of the instruction ABI or IDL.

use crate::prelude::ProgramId;

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
    /// this is set to [`lee_core::program::DEFAULT_PROGRAM_ID`] (all zeros).
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
