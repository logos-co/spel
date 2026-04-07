//! Account validation helpers.
//!
//! These functions are called by the macro-generated code to validate
//! accounts before passing them to instruction handlers.

use crate::error::SpelError;
use crate::types::AccountConstraint;

/// Validate that the correct number of accounts was provided.
pub fn validate_account_count(
    actual: usize,
    expected: usize,
) -> Result<(), SpelError> {
    if actual != expected {
        return Err(SpelError::AccountCountMismatch { expected, actual });
    }
    Ok(())
}

/// Validate a set of accounts against their constraints.
///
/// This is the main validation entry point called by generated code.
/// In a real implementation, `accounts` would be `&[AccountWithMetadata]`
/// from SPEL core.
///
/// # Generated usage
/// ```rust,ignore
/// // The proc-macro generates this call:
/// validate_accounts(&pre_states, &[
///     AccountConstraint { mutable: false, init: false, ..Default::default() },  // token_def
///     AccountConstraint { mutable: true, owner: Some(TOKEN_PROGRAM), ..Default::default() },  // from
///     AccountConstraint { mutable: true, ..Default::default() },  // to
/// ])?;
/// ```
pub fn validate_accounts(
    account_count: usize,
    constraints: &[AccountConstraint],
) -> Result<(), SpelError> {
    // First check count
    validate_account_count(account_count, constraints.len())?;
    
    // In a real implementation, we would also check:
    // - ownership constraints
    // - initialization state
    // - signer verification  
    // - PDA derivation
    //
    // These require access to the actual AccountWithMetadata data,
    // which the proc-macro would pass in.
    
    Ok(())
}

/// Check if an account is in default/uninitialized state.
/// Used for `#[account(init)]` constraint.
pub fn is_default_account(data: &[u8]) -> bool {
    data.is_empty() || data.iter().all(|&b| b == 0)
}

/// Verify that an account's owner matches the expected program.
/// Used for `#[account(owner = PROGRAM_ID)]` constraint.
pub fn verify_owner(
    account_owner: &[u8; 32],
    expected_owner: &[u8; 32],
    account_index: usize,
) -> Result<(), SpelError> {
    if account_owner != expected_owner {
        return Err(SpelError::InvalidAccountOwner {
            account_index,
            expected_owner: hex::encode(expected_owner),
        });
    }
    Ok(())
}

// Note: hex is used for error display only. In production, 
// consider base58 or the chain's preferred encoding.
mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{:02x}", b)).collect()
    }
}
