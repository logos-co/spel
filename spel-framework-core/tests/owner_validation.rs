//! Test that #[account(owner = self_program_id)] generates runtime validation checks.
//!
//! This is an expansion test — we simulate the validation functions that the macro
//! would generate for owner constraints.

use nssa_core::account::{Account, AccountId, AccountWithMetadata};
use nssa_core::program::ProgramId;
use spel_framework_core::error::SpelError;

/// Simulate the validation function that the macro would generate for:
/// ```
/// #[instruction]
/// pub fn initialize_holding(
///     ctx: ProgramContext,
///     #[account(owner = self_program_id)]
///     definition_account: AccountWithMetadata,
///     #[account(init, signer)]
///     holding_account: AccountWithMetadata,
/// ) -> SpelResult { ... }
/// ```
fn __validate_initialize_holding(
    accounts: &[AccountWithMetadata],
    self_program_id: &ProgramId,
) -> Result<(), SpelError> {
    // Account index 0 has #[account(owner = self_program_id)]
    if accounts[0].account.program_owner != *self_program_id {
        return Err(SpelError::AccountOwnerMismatch {
            account_name: "definition_account".to_string(),
        });
    }
    // Account index 1 has #[account(init)]
    if accounts[1].account != Account::default() {
        return Err(SpelError::AccountAlreadyInitialized { account_index: 1 });
    }
    // Account index 1 has #[account(signer)]
    if !accounts[1].is_authorized {
        return Err(SpelError::Unauthorized {
            message: format!(
                "Account {} (index {}) must be a signer",
                "holding_account", 1
            ),
        });
    }
    Ok(())
}

/// ProgramId is `[u32; 8]` (64 bytes = 32 u8s).
fn make_program_id(bytes: [u8; 32]) -> ProgramId {
    let mut id = [0u32; 8];
    for i in 0..8 {
        id[i] = u32::from_le_bytes([
            bytes[i * 4],
            bytes[i * 4 + 1],
            bytes[i * 4 + 2],
            bytes[i * 4 + 3],
        ]);
    }
    id
}

fn make_account_with_owner(
    id: [u8; 32],
    owner: ProgramId,
    authorized: bool,
) -> AccountWithMetadata {
    let mut account = Account::default();
    account.program_owner = owner;
    AccountWithMetadata {
        account_id: AccountId::new(id),
        account,
        is_authorized: authorized,
    }
}

fn make_initialized_account_with_owner(
    id: [u8; 32],
    owner: ProgramId,
    data: Vec<u8>,
    authorized: bool,
) -> AccountWithMetadata {
    let mut account = Account::default();
    account.program_owner = owner;
    account.data = data.try_into().unwrap();
    AccountWithMetadata {
        account_id: AccountId::new(id),
        account,
        is_authorized: authorized,
    }
}

#[test]
fn test_owner_matches_self_program_id() {
    let program_id = make_program_id([1u8; 32]);
    let accounts = vec![
        // definition_account owned by this program ✓
        make_account_with_owner([2u8; 32], program_id, false),
        // holding_account: init + signer (empty, authorized)
        make_account_with_owner([3u8; 32], ProgramId::default(), true),
    ];
    assert!(__validate_initialize_holding(&accounts, &program_id).is_ok());
}

#[test]
fn test_owner_mismatch_returns_error() {
    let program_id = make_program_id([1u8; 32]);
    let other_program = make_program_id([99u8; 32]);
    let accounts = vec![
        // definition_account owned by DIFFERENT program ✗
        make_account_with_owner([2u8; 32], other_program, false),
        make_account_with_owner([3u8; 32], ProgramId::default(), true),
    ];
    let result = __validate_initialize_holding(&accounts, &program_id);
    assert!(result.is_err());
    match result.unwrap_err() {
        SpelError::AccountOwnerMismatch { account_name } => {
            assert_eq!(account_name, "definition_account");
        },
        other => panic!("expected AccountOwnerMismatch, got: {other:?}"),
    }
}

#[test]
fn test_owner_check_runs_before_init_and_signer() {
    // Owner check is first in the validation chain.
    // Even if init and signer are also wrong, owner error should surface first.
    let program_id = make_program_id([1u8; 32]);
    let other_program = make_program_id([99u8; 32]);
    let accounts = vec![
        // definition_account owned by DIFFERENT program ✗
        make_account_with_owner([2u8; 32], other_program, false),
        // holding_account: NOT empty (init violated) and NOT authorized (signer violated)
        make_initialized_account_with_owner([3u8; 32], ProgramId::default(), vec![1u8; 32], false),
    ];
    let result = __validate_initialize_holding(&accounts, &program_id);
    // Owner check runs first, so we should get AccountOwnerMismatch, not init/signer errors.
    match result.unwrap_err() {
        SpelError::AccountOwnerMismatch { account_name } => {
            assert_eq!(account_name, "definition_account");
        },
        other => panic!("expected AccountOwnerMismatch (owner check runs first), got: {other:?}"),
    }
}

#[test]
fn test_owner_error_code() {
    let err = SpelError::AccountOwnerMismatch {
        account_name: "test_account".to_string(),
    };
    assert_eq!(err.error_code(), 1010);
}
