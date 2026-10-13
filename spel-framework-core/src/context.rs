//! Execution context exposed to SPEL instruction handlers.
//!
//! When an `#[instruction]` handler declares a parameter of type
//! [`ProgramContext`], the macro-generated dispatcher injects the trusted
//! values from [`nssa_core::program::ProgramInput`] at call time.
//! The context parameter is **never** part of the instruction ABI or IDL.

use crate::prelude::AccountId;

/// Trusted execution metadata supplied by the SPEL guest entrypoint.
///
/// Use this as a parameter on `#[instruction]` functions to access
/// `self_account_id` and `caller_account_id` without adding them to
/// the instruction schema:
///
/// ```ignore
/// #[instruction]
/// pub fn initialize(
///     ctx: ProgramContext,
///     #[account(owner = self_account_id)]
///     definition: AccountWithMetadata,
/// ) -> SpelResult {
///     // ctx.self_account_id is the currently executing program
/// }
/// ```
///
/// Since LEZ v0.2.5 a program is identified by the `AccountId` of its on-chain
/// header account, not by its image id — see `nssa_core::program::ProgramHeader`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProgramContext {
    /// The account ID of the currently executing program.
    pub self_account_id: AccountId,
    /// The account ID of the caller (the program that invoked this one).
    /// If there is no explicit caller (e.g. top-level transaction),
    /// this is set to [`nssa_core::program::DEFAULT_PROGRAM_OWNER`] (all zeros).
    pub caller_account_id: AccountId,
}

impl ProgramContext {
    /// Create a new context from program input values.
    #[must_use]
    pub const fn new(self_account_id: AccountId, caller_account_id: AccountId) -> Self {
        Self {
            self_account_id,
            caller_account_id,
        }
    }
}
