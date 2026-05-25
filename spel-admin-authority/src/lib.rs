//! # SPEL Admin Authority Library (RFP-001)
//!
//! Provides standardised admin authority for LEZ programs.
//!
//! ## Usage
//!
//! ```rust,ignore
//! use spel_admin_authority::{AdminState, AdminError};
//!
//! // In initialize instruction:
//! let mut state = AdminState::initialize(admin_account.account_id.value());
//!
//! // Gate a privileged instruction:
//! state.assert_admin(&signer_account)?;
//!
//! // Transfer authority:
//! state.transfer_admin(new_admin_key)?;
//!
//! // Revoke permanently:
//! state.revoke_admin()?;
//! ```

use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};

/// The admin authority state stored in a PDA account.
///
/// This struct is stored on-chain in the program's config PDA.
/// Use `#[account_type]` when embedding in your program.
#[derive(Debug, Clone, PartialEq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub struct AdminState {
    /// The current admin authority public key.
    /// `None` means admin has been permanently revoked.
    pub admin: Option<[u8; 32]>,
}

/// Errors returned by admin authority operations.
#[derive(Debug, Clone, PartialEq)]
pub enum AdminError {
    /// The signer is not the current admin authority.
    Unauthorized,
    /// Admin authority has been permanently revoked.
    Revoked,
    /// Admin authority is already revoked — cannot revoke twice.
    AlreadyRevoked,
}

impl std::fmt::Display for AdminError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdminError::Unauthorized => {
                write!(f, "Unauthorized: signer is not the admin authority")
            }
            AdminError::Revoked => write!(f, "Admin authority has been permanently revoked"),
            AdminError::AlreadyRevoked => write!(f, "Admin authority is already revoked"),
        }
    }
}

impl AdminState {
    /// Initialize a new AdminState with the given admin key.
    pub fn new(admin_key: [u8; 32]) -> Self {
        Self {
            admin: Some(admin_key),
        }
    }

    /// Check if admin authority has been revoked.
    pub fn is_revoked(&self) -> bool {
        self.admin.is_none()
    }

    /// Assert that the given account is the current admin authority.
    ///
    /// Returns `Err(AdminError::Revoked)` if authority has been revoked.
    /// Returns `Err(AdminError::Unauthorized)` if the signer doesn't match.
    pub fn assert_admin(&self, signer_key: &[u8; 32]) -> Result<(), AdminError> {
        match &self.admin {
            None => Err(AdminError::Revoked),
            Some(key) => {
                if key == signer_key {
                    Ok(())
                } else {
                    Err(AdminError::Unauthorized)
                }
            }
        }
    }

    /// Transfer admin authority to a new key.
    ///
    /// Only the current admin can call this.
    pub fn transfer_admin(
        &mut self,
        signer_key: &[u8; 32],
        new_admin: [u8; 32],
    ) -> Result<(), AdminError> {
        self.assert_admin(signer_key)?;
        self.admin = Some(new_admin);
        Ok(())
    }

    /// Permanently revoke admin authority.
    ///
    /// Only the current admin can call this. This is irreversible.
    pub fn revoke_admin(&mut self, signer_key: &[u8; 32]) -> Result<(), AdminError> {
        self.assert_admin(signer_key)?;
        self.admin = None;
        Ok(())
    }
}

/// The standard config PDA account data for admin-authority programs.
///
/// Store this in your program's config PDA (pda = literal("config")).
/// The `admin_state` field controls access to privileged instructions.
///
/// ```rust,ignore
/// #[account_type]
/// #[derive(BorshSerialize, BorshDeserialize)]
/// pub struct MyProgramConfig {
///     pub admin_state: AdminState,
///     pub my_value: u64,
/// }
/// ```
///
/// Or use `AdminConfig` directly if you only need the admin state + a u64 value.
#[derive(Debug, Clone, PartialEq, borsh::BorshSerialize, borsh::BorshDeserialize,
         serde::Serialize, serde::Deserialize)]
pub struct AdminConfig {
    /// The admin authority state.
    pub admin_state: AdminState,
    /// A configurable value gated behind admin authority.
    pub config_value: u64,
}

impl AdminConfig {
    /// Create a new AdminConfig with the given admin key and zero config value.
    pub fn new(admin_key: [u8; 32]) -> Self {
        Self {
            admin_state: AdminState::new(admin_key),
            config_value: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    #[test]
    fn new_sets_admin() {
        let state = AdminState::new(key(1));
        assert_eq!(state.admin, Some(key(1)));
        assert!(!state.is_revoked());
    }

    #[test]
    fn assert_admin_accepts_correct_signer() {
        let state = AdminState::new(key(1));
        assert!(state.assert_admin(&key(1)).is_ok());
    }

    #[test]
    fn assert_admin_rejects_wrong_signer() {
        let state = AdminState::new(key(1));
        let err = state.assert_admin(&key(2)).unwrap_err();
        assert_eq!(err, AdminError::Unauthorized);
    }

    #[test]
    fn assert_admin_rejects_after_revocation() {
        let mut state = AdminState::new(key(1));
        state.revoke_admin(&key(1)).unwrap();
        let err = state.assert_admin(&key(1)).unwrap_err();
        assert_eq!(err, AdminError::Revoked);
    }

    #[test]
    fn transfer_admin_works() {
        let mut state = AdminState::new(key(1));
        state.transfer_admin(&key(1), key(2)).unwrap();
        assert_eq!(state.admin, Some(key(2)));
        // old key no longer works
        assert_eq!(state.assert_admin(&key(1)), Err(AdminError::Unauthorized));
        // new key works
        assert!(state.assert_admin(&key(2)).is_ok());
    }

    #[test]
    fn transfer_admin_rejects_wrong_signer() {
        let mut state = AdminState::new(key(1));
        let err = state.transfer_admin(&key(2), key(3)).unwrap_err();
        assert_eq!(err, AdminError::Unauthorized);
        // state unchanged
        assert_eq!(state.admin, Some(key(1)));
    }

    #[test]
    fn revoke_admin_works() {
        let mut state = AdminState::new(key(1));
        state.revoke_admin(&key(1)).unwrap();
        assert!(state.is_revoked());
        assert_eq!(state.admin, None);
    }

    #[test]
    fn revoke_admin_rejects_wrong_signer() {
        let mut state = AdminState::new(key(1));
        let err = state.revoke_admin(&key(2)).unwrap_err();
        assert_eq!(err, AdminError::Unauthorized);
        // state unchanged
        assert!(!state.is_revoked());
    }

    #[test]
    fn revoke_admin_rejects_already_revoked() {
        let mut state = AdminState::new(key(1));
        state.revoke_admin(&key(1)).unwrap();
        // try to revoke again with any key
        let err = state.revoke_admin(&key(1)).unwrap_err();
        assert_eq!(err, AdminError::Revoked);
    }
}
