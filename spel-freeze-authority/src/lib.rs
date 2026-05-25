//! # SPEL Freeze Authority Library (RFP-002)
//!
//! Provides a standardised freeze mechanism for LEZ programs — an emergency
//! circuit breaker that allows an authorised account to disable all (or selected)
//! interactions with a program.
//!
//! ## Authority Hierarchy
//!
//! ```text
//! AdminAuthority (spel-admin-authority)
//!     └── FreezeAuthority (spel-freeze-authority)
//!             ├── freeze_program() / unfreeze_program()
//!             ├── freeze_account(account_id)
//!             └── unfreeze_account(account_id)
//! ```
//!
//! ## Usage
//!
//! ```rust,ignore
//! use spel_freeze_authority::FreezeState;
//!
//! // Check before any interaction:
//! state.check_interaction(caller_account_id)?;
//!
//! // Freeze the entire program (freeze authority only):
//! state.freeze_program(&freeze_authority_key)?;
//!
//! // Freeze a specific account:
//! state.freeze_account(&freeze_authority_key, account_id)?;
//! ```

use borsh::{BorshDeserialize, BorshSerialize};
use nssa_core::account::AccountId;
use serde::{Deserialize, Serialize};
use spel_admin_authority::{AdminError, AdminState};
use std::collections::HashSet;

/// Errors returned by freeze authority operations.
#[derive(Debug, Clone, PartialEq)]
pub enum FreezeError {
    /// The signer is not the current freeze authority.
    Unauthorized,
    /// The freeze authority has been permanently revoked by admin.
    Revoked,
    /// The program is frozen — all interactions are rejected.
    ProgramFrozen,
    /// The specific account is frozen.
    AccountFrozen,
    /// The freeze authority is already revoked.
    AlreadyRevoked,
}

impl std::fmt::Display for FreezeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FreezeError::Unauthorized => write!(f, "Unauthorized: signer is not the freeze authority"),
            FreezeError::Revoked => write!(f, "Freeze authority has been permanently revoked"),
            FreezeError::ProgramFrozen => write!(f, "Program is frozen — all interactions are rejected"),
            FreezeError::AccountFrozen => write!(f, "Account is frozen — interactions are rejected"),
            FreezeError::AlreadyRevoked => write!(f, "Freeze authority is already revoked"),
        }
    }
}

impl From<AdminError> for FreezeError {
    fn from(e: AdminError) -> Self {
        match e {
            AdminError::Unauthorized => FreezeError::Unauthorized,
            AdminError::Revoked => FreezeError::Revoked,
            AdminError::AlreadyRevoked => FreezeError::AlreadyRevoked,
        }
    }
}

/// The freeze authority state stored in a program's config PDA.
///
/// Controlled by the admin authority — only admin can set or revoke
/// the freeze authority. The freeze authority itself can freeze/unfreeze
/// the program or individual accounts.
#[derive(Debug, Clone, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub struct FreezeState {
    /// The freeze authority key. None = freeze authority revoked by admin.
    /// Only admin can change this.
    pub freeze_authority: AdminState,
    /// Whether the entire program is frozen.
    pub program_frozen: bool,
    /// Set of frozen account IDs.
    /// Accounts in this set cannot interact with the program.
    pub frozen_accounts: Vec<[u8; 32]>,
}

impl FreezeState {
    /// Create a new FreezeState with the given freeze authority key.
    pub fn new(freeze_authority_key: [u8; 32]) -> Self {
        Self {
            freeze_authority: AdminState::new(freeze_authority_key),
            program_frozen: false,
            frozen_accounts: Vec::new(),
        }
    }

    /// Check if freeze authority has been revoked.
    pub fn is_revoked(&self) -> bool {
        self.freeze_authority.is_revoked()
    }

    /// Check if a specific account is frozen.
    pub fn is_account_frozen(&self, account_id: &[u8; 32]) -> bool {
        self.frozen_accounts.contains(account_id)
    }

    /// Check if an interaction should be allowed.
    ///
    /// Returns Err if:
    /// - The program is frozen (`FreezeError::ProgramFrozen`)
    /// - The specific account is frozen (`FreezeError::AccountFrozen`)
    pub fn check_interaction(&self, account_id: &[u8; 32]) -> Result<(), FreezeError> {
        if self.program_frozen {
            return Err(FreezeError::ProgramFrozen);
        }
        if self.is_account_frozen(account_id) {
            return Err(FreezeError::AccountFrozen);
        }
        Ok(())
    }

    /// Freeze the entire program. Only freeze authority can call this.
    pub fn freeze_program(&mut self, signer_key: &[u8; 32]) -> Result<(), FreezeError> {
        self.freeze_authority.assert_admin(signer_key)?;
        self.program_frozen = true;
        Ok(())
    }

    /// Unfreeze the entire program. Only freeze authority can call this.
    pub fn unfreeze_program(&mut self, signer_key: &[u8; 32]) -> Result<(), FreezeError> {
        self.freeze_authority.assert_admin(signer_key)?;
        self.program_frozen = false;
        Ok(())
    }

    /// Freeze a specific account. Only freeze authority can call this.
    pub fn freeze_account(
        &mut self,
        signer_key: &[u8; 32],
        account_id: [u8; 32],
    ) -> Result<(), FreezeError> {
        self.freeze_authority.assert_admin(signer_key)?;
        if !self.frozen_accounts.contains(&account_id) {
            self.frozen_accounts.push(account_id);
        }
        Ok(())
    }

    /// Unfreeze a specific account. Only freeze authority can call this.
    pub fn unfreeze_account(
        &mut self,
        signer_key: &[u8; 32],
        account_id: [u8; 32],
    ) -> Result<(), FreezeError> {
        self.freeze_authority.assert_admin(signer_key)?;
        self.frozen_accounts.retain(|a| a != &account_id);
        Ok(())
    }

    /// Change the freeze authority. Only admin can call this.
    ///
    /// Pass `None` to permanently revoke freeze authority.
    pub fn set_freeze_authority(
        &mut self,
        admin_key: &[u8; 32],
        new_authority: Option<[u8; 32]>,
    ) -> Result<(), FreezeError> {
        match new_authority {
            Some(new_key) => {
                self.freeze_authority.transfer_admin(admin_key, new_key)?;
            }
            None => {
                self.freeze_authority.revoke_admin(admin_key)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> [u8; 32] { [byte; 32] }

    #[test]
    fn new_sets_freeze_authority() {
        let state = FreezeState::new(key(1));
        assert!(!state.is_revoked());
        assert!(!state.program_frozen);
        assert!(state.frozen_accounts.is_empty());
    }

    #[test]
    fn check_interaction_allows_when_not_frozen() {
        let state = FreezeState::new(key(1));
        assert!(state.check_interaction(&key(2)).is_ok());
    }

    #[test]
    fn freeze_program_blocks_all_interactions() {
        let mut state = FreezeState::new(key(1));
        state.freeze_program(&key(1)).unwrap();
        assert!(state.program_frozen);
        assert_eq!(state.check_interaction(&key(2)), Err(FreezeError::ProgramFrozen));
        assert_eq!(state.check_interaction(&key(3)), Err(FreezeError::ProgramFrozen));
    }

    #[test]
    fn unfreeze_program_restores_interactions() {
        let mut state = FreezeState::new(key(1));
        state.freeze_program(&key(1)).unwrap();
        state.unfreeze_program(&key(1)).unwrap();
        assert!(!state.program_frozen);
        assert!(state.check_interaction(&key(2)).is_ok());
    }

    #[test]
    fn freeze_account_blocks_specific_account() {
        let mut state = FreezeState::new(key(1));
        state.freeze_account(&key(1), key(2)).unwrap();
        assert_eq!(state.check_interaction(&key(2)), Err(FreezeError::AccountFrozen));
        assert!(state.check_interaction(&key(3)).is_ok());
    }

    #[test]
    fn unfreeze_account_restores_specific_account() {
        let mut state = FreezeState::new(key(1));
        state.freeze_account(&key(1), key(2)).unwrap();
        state.unfreeze_account(&key(1), key(2)).unwrap();
        assert!(state.check_interaction(&key(2)).is_ok());
    }

    #[test]
    fn freeze_program_rejects_wrong_authority() {
        let mut state = FreezeState::new(key(1));
        assert_eq!(state.freeze_program(&key(2)), Err(FreezeError::Unauthorized));
        assert!(!state.program_frozen);
    }

    #[test]
    fn freeze_account_rejects_wrong_authority() {
        let mut state = FreezeState::new(key(1));
        assert_eq!(state.freeze_account(&key(2), key(3)), Err(FreezeError::Unauthorized));
        assert!(state.frozen_accounts.is_empty());
    }

    #[test]
    fn set_freeze_authority_rotates_key() {
        let mut state = FreezeState::new(key(1));
        state.set_freeze_authority(&key(1), Some(key(2))).unwrap();
        // old key no longer works
        assert_eq!(state.freeze_program(&key(1)), Err(FreezeError::Unauthorized));
        // new key works
        assert!(state.freeze_program(&key(2)).is_ok());
    }

    #[test]
    fn set_freeze_authority_none_revokes_permanently() {
        let mut state = FreezeState::new(key(1));
        state.set_freeze_authority(&key(1), None).unwrap();
        assert!(state.is_revoked());
        assert_eq!(state.freeze_program(&key(1)), Err(FreezeError::Revoked));
    }

    #[test]
    fn set_freeze_authority_rejects_wrong_admin() {
        let mut state = FreezeState::new(key(1));
        assert_eq!(
            state.set_freeze_authority(&key(2), Some(key(3))),
            Err(FreezeError::Unauthorized)
        );
    }
}
