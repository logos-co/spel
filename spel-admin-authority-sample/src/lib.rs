//! # Admin Authority Sample Program (RFP-001)
//!
//! Demonstrates how to use `spel-admin-authority` in a SPEL program.
//!
//! ## Instructions
//!
//! - `initialize` — creates the config PDA and sets the admin authority
//! - `set_config_value` — admin-only: update the config value (gated instruction)
//! - `transfer_admin` — admin-only: transfer authority to a new key
//! - `revoke_admin` — admin-only: permanently revoke admin control
//!
//! ## Usage pattern
//!
//! ```rust,ignore
//! #[lez_program]
//! mod my_program {
//!     #[instruction]
//!     pub fn initialize(
//!         #[account(init, pda = literal("config"))]
//!         config: AccountWithMetadata,
//!         #[account(signer)]
//!         admin: AccountWithMetadata,
//!     ) -> SpelResult {
//!         // Store AdminState in config PDA
//!         let state = AdminState::new(*admin.account_id.value());
//!         // ... write state to config account
//!     }
//! }
//! ```

use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};
use spel_admin_authority::{AdminError, AdminState};
use spel_framework::prelude::*;

// Re-export AdminConfig from the library
pub use spel_admin_authority::AdminConfig;

/// Convert AdminError to SpelError for use in instruction handlers.
fn admin_err(e: AdminError) -> spel_framework::error::SpelError {
    spel_framework::error::SpelError::Unauthorized {
        message: e.to_string(),
    }
}

#[lez_program]
mod admin_authority_sample {
    use super::*;

    /// Initialize the config PDA and set the admin authority.
    ///
    /// The signer of this transaction becomes the admin authority.
    /// Re-initialization is rejected automatically by `#[account(init)]`.
    #[instruction]
    pub fn initialize(
        #[account(init, pda = literal("config"))]
        config: AccountWithMetadata,
        #[account(signer)]
        admin: AccountWithMetadata,
    ) -> SpelResult {
        let admin_key = *admin.account_id.value();
        let state = AdminConfig::new(admin_key);
        let data = borsh::to_vec(&state).expect("AdminConfig serializes");

        let mut post_config = config.account.clone();
        post_config.data = data.try_into().expect("data fits");

        Ok(SpelOutput::execute(
            vec![config, admin],
            vec![],
        ))
    }

    /// Update the config value. Admin-only.
    ///
    /// The #[require_admin(config)] annotation automatically injects
    /// the admin authority check before the handler body runs.
    #[instruction]
    #[require_admin(config)]
    pub fn set_config_value(
        #[account(mut, pda = literal("config"))]
        config: AccountWithMetadata,
        #[account(signer)]
        admin: AccountWithMetadata,
        new_value: u64,
    ) -> SpelResult {
        let mut state = AdminConfig::try_from_slice(&config.account.data)
            .map_err(|_| spel_framework::error::SpelError::Unauthorized {
                message: "Failed to deserialize AdminConfig".to_string(),
            })?;

        // Assert admin authority
        let admin_key = *admin.account_id.value();
        state.admin_state.assert_admin(&admin_key).map_err(admin_err)?;

        // Update value
        state.config_value = new_value;
        let data = borsh::to_vec(&state).expect("AdminConfig serializes");

        let mut post_config = config.account.clone();
        post_config.data = data.try_into().expect("data fits");

        Ok(SpelOutput::execute(vec![config, admin], vec![]))
    }

    /// Transfer admin authority to a new signer. Admin-only.
    ///
    /// After this call, only the new admin can call privileged instructions.
    #[instruction]
    pub fn transfer_admin(
        #[account(mut, pda = literal("config"))]
        config: AccountWithMetadata,
        #[account(signer)]
        admin: AccountWithMetadata,
        new_admin: [u8; 32],
    ) -> SpelResult {
        let mut state = AdminConfig::try_from_slice(&config.account.data)
            .map_err(|_| spel_framework::error::SpelError::Unauthorized {
                message: "Failed to deserialize AdminConfig".to_string(),
            })?;

        let admin_key = *admin.account_id.value();
        state.admin_state.transfer_admin(&admin_key, new_admin).map_err(admin_err)?;

        let data = borsh::to_vec(&state).expect("AdminConfig serializes");
        let mut post_config = config.account.clone();
        post_config.data = data.try_into().expect("data fits");

        Ok(SpelOutput::execute(vec![config, admin], vec![]))
    }

    /// Permanently revoke admin authority. Admin-only. Irreversible.
    ///
    /// After this call, no one can call privileged instructions ever again.
    #[instruction]
    pub fn revoke_admin(
        #[account(mut, pda = literal("config"))]
        config: AccountWithMetadata,
        #[account(signer)]
        admin: AccountWithMetadata,
    ) -> SpelResult {
        let mut state = AdminConfig::try_from_slice(&config.account.data)
            .map_err(|_| spel_framework::error::SpelError::Unauthorized {
                message: "Failed to deserialize AdminConfig".to_string(),
            })?;

        let admin_key = *admin.account_id.value();
        state.admin_state.revoke_admin(&admin_key).map_err(admin_err)?;

        let data = borsh::to_vec(&state).expect("AdminConfig serializes");
        let mut post_config = config.account.clone();
        post_config.data = data.try_into().expect("data fits");

        Ok(SpelOutput::execute(vec![config, admin], vec![]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nssa_core::account::{Account, AccountId, AccountWithMetadata};

    fn make_account(id: [u8; 32], authorized: bool) -> AccountWithMetadata {
        AccountWithMetadata {
            account_id: AccountId::new(id),
            account: Account::default(),
            is_authorized: authorized,
        }
    }

    fn make_config_account(state: &AdminConfig) -> AccountWithMetadata {
        let data = borsh::to_vec(state).unwrap();
        let mut account = Account::default();
        account.data = data.try_into().unwrap();
        AccountWithMetadata {
            account_id: AccountId::new([0u8; 32]),
            account,
            is_authorized: false,
        }
    }

    fn admin_key() -> [u8; 32] { [1u8; 32] }
    fn other_key() -> [u8; 32] { [2u8; 32] }
    fn new_admin_key() -> [u8; 32] { [3u8; 32] }

    #[test]
    fn initialize_sets_admin() {
        let config = make_account([0u8; 32], false);
        let admin = make_account(admin_key(), true);
        let result = admin_authority_sample::initialize(config, admin);
        assert!(result.is_ok());
    }

    #[test]
    fn set_config_value_succeeds_for_admin() {
        let state = AdminConfig::new(admin_key());
        let config = make_config_account(&state);
        let admin = make_account(admin_key(), true);
        let result = admin_authority_sample::set_config_value(config, admin, 42);
        assert!(result.is_ok());
    }

    #[test]
    fn set_config_value_rejected_for_non_admin() {
        let state = AdminConfig::new(admin_key());
        let config = make_config_account(&state);
        let non_admin = make_account(other_key(), true);
        let result = admin_authority_sample::set_config_value(config, non_admin, 42);
        assert!(result.is_err());
    }

    #[test]
    fn transfer_admin_works() {
        let state = AdminConfig::new(admin_key());
        let config = make_config_account(&state);
        let admin = make_account(admin_key(), true);
        let result = admin_authority_sample::transfer_admin(config, admin, new_admin_key());
        assert!(result.is_ok());
    }

    #[test]
    fn transfer_admin_rejected_for_non_admin() {
        let state = AdminConfig::new(admin_key());
        let config = make_config_account(&state);
        let non_admin = make_account(other_key(), true);
        let result = admin_authority_sample::transfer_admin(config, non_admin, new_admin_key());
        assert!(result.is_err());
    }

    #[test]
    fn revoke_admin_works() {
        let state = AdminConfig::new(admin_key());
        let config = make_config_account(&state);
        let admin = make_account(admin_key(), true);
        let result = admin_authority_sample::revoke_admin(config, admin);
        assert!(result.is_ok());
    }

    #[test]
    fn revoke_admin_rejected_for_non_admin() {
        let state = AdminConfig::new(admin_key());
        let config = make_config_account(&state);
        let non_admin = make_account(other_key(), true);
        let result = admin_authority_sample::revoke_admin(config, non_admin);
        assert!(result.is_err());
    }

    #[test]
    fn set_config_rejected_after_revocation() {
        let mut state = AdminConfig::new(admin_key());
        state.admin_state.revoke_admin(&admin_key()).unwrap();
        let config = make_config_account(&state);
        let admin = make_account(admin_key(), true);
        let result = admin_authority_sample::set_config_value(config, admin, 99);
        assert!(result.is_err());
    }
}
