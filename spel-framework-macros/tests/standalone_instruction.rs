//! Standalone `#[instruction]` expansion.
//!
//! An extension library's instruction functions live outside `#[lez_program]`,
//! so the attribute expands for real there instead of being consumed by the
//! module macro. This file compiling is the guard: with a pass-through
//! `#[instruction]` the `#[account(...)]` parameter attributes reach rustc,
//! which rejects them with "cannot find attribute `account` in this scope".

use spel_framework_macros::instruction;

#[instruction]
pub fn set_paused(
    #[account(mut, pda = const("pause"))] state: u32,
    #[account(signer)] admin: u32,
    paused: bool,
) -> u32 {
    state + admin + u32::from(paused)
}

#[instruction]
pub fn no_accounts(value: u32) -> u32 {
    value
}

#[test]
fn stripping_leaves_the_function_callable() {
    assert_eq!(set_paused(1, 2, true), 4);
    assert_eq!(set_paused(1, 2, false), 3);
}

#[test]
fn a_function_without_account_attrs_passes_through() {
    assert_eq!(no_accounts(7), 7);
}
