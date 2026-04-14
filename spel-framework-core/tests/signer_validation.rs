//! Test that #[account(signer)] generates runtime validation checks.
//!
//! This is an expansion test — we cannot run the macro in a unit test directly,
//! so we test the validation functions that would be generated.

use nssa_core::account::{Account, AccountId, AccountWithMetadata};
use spel_framework_core::error::SpelError;

/// Simulate the validation function that the macro would generate for:
/// ```
/// #[instruction]
/// pub fn transfer(
///     #[account(mut)] from: AccountWithMetadata,
///     #[account(signer)] authority: AccountWithMetadata,
///     #[account(mut)] to: AccountWithMetadata,
/// ) -> SpelResult { ... }
/// ```
fn __validate_transfer(accounts: &[AccountWithMetadata]) -> Result<(), SpelError> {
    // Account index 1 has #[account(signer)]
    if !accounts[1].is_authorized {
        return Err(SpelError::Unauthorized {
            message: format!("Account {} (index {}) must be a signer", "authority", 1),
        });
    }
    Ok(())
}

/// Simulate validation for an init + signer instruction:
/// ```
/// #[instruction]
/// pub fn create_state(
///     #[account(init)] state: AccountWithMetadata,
///     #[account(signer)] creator: AccountWithMetadata,
/// ) -> SpelResult { ... }
/// ```
fn __validate_create_state(accounts: &[AccountWithMetadata]) -> Result<(), SpelError> {
    // Account index 0 has #[account(init)]
    if accounts[0].account != Account::default() {
        return Err(SpelError::AccountAlreadyInitialized {
            account_index: 0,
        });
    }
    // Account index 1 has #[account(signer)]
    if !accounts[1].is_authorized {
        return Err(SpelError::Unauthorized {
            message: format!("Account {} (index {}) must be a signer", "creator", 1),
        });
    }
    Ok(())
}

fn make_account(id: [u8; 32], authorized: bool) -> AccountWithMetadata {
    AccountWithMetadata {
        account_id: AccountId::new(id),
        account: Account::default(),
        is_authorized: authorized,
    }
}

fn make_account_with_data(id: [u8; 32], data: Vec<u8>, authorized: bool) -> AccountWithMetadata {
    let mut account = Account::default();
    account.data = data.try_into().unwrap();
    AccountWithMetadata {
        account_id: AccountId::new(id),
        account,
        is_authorized: authorized,
    }
}

#[test]
fn test_signer_authorized_passes() {
    let accounts = vec![
        make_account([1u8; 32], false),  // from (mut, not signer)
        make_account([2u8; 32], true),   // authority (signer) ← authorized
        make_account([3u8; 32], false),  // to (mut, not signer)
    ];
    assert!(__validate_transfer(&accounts).is_ok());
}

#[test]
fn test_signer_unauthorized_fails() {
    let accounts = vec![
        make_account([1u8; 32], false),
        make_account([2u8; 32], false),  // authority NOT authorized
        make_account([3u8; 32], false),
    ];
    let err = __validate_transfer(&accounts).unwrap_err();
    match err {
        SpelError::Unauthorized { message } => {
            assert!(message.contains("authority"));
            assert!(message.contains("index 1"));
        }
        _ => panic!("Expected Unauthorized error, got {:?}", err),
    }
}

#[test]
fn test_init_uninitialized_passes() {
    let accounts = vec![
        make_account([1u8; 32], false),  // state (init, default = uninitialized)
        make_account([2u8; 32], true),   // creator (signer, authorized)
    ];
    assert!(__validate_create_state(&accounts).is_ok());
}

#[test]
fn test_init_already_initialized_fails() {
    let accounts = vec![
        make_account_with_data([1u8; 32], vec![42], false),  // state already has data
        make_account([2u8; 32], true),
    ];
    let err = __validate_create_state(&accounts).unwrap_err();
    match err {
        SpelError::AccountAlreadyInitialized { account_index } => {
            assert_eq!(account_index, 0);
        }
        _ => panic!("Expected AccountAlreadyInitialized, got {:?}", err),
    }
}

#[test]
fn test_init_and_signer_both_checked() {
    // Both init account initialized AND signer not authorized
    let accounts = vec![
        make_account_with_data([1u8; 32], vec![42], false),  // already initialized
        make_account([2u8; 32], false),  // not authorized
    ];
    // Init check runs first, so we get AccountAlreadyInitialized
    let err = __validate_create_state(&accounts).unwrap_err();
    assert!(matches!(err, SpelError::AccountAlreadyInitialized { .. }));
}
