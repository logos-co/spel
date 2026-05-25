//! # Freeze Authority Sample Program (RFP-002)
//!
//! Demonstrates how to use `spel-freeze-authority` in a SPEL program.
//!
//! ## Instructions
//!
//! - `initialize` — creates config PDA, sets admin + freeze authority
//! - `interact` — normal program interaction (blocked when frozen)
//! - `freeze_program` — freeze authority only: freeze entire program
//! - `unfreeze_program` — freeze authority only: unfreeze program
//! - `freeze_account` — freeze authority only: freeze specific account
//! - `unfreeze_account` — freeze authority only: unfreeze specific account
//! - `set_freeze_authority` — admin only: rotate or revoke freeze authority

use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};
use spel_admin_authority::AdminConfig;
use spel_freeze_authority::{FreezeError, FreezeState};
use spel_framework::prelude::*;

/// The config PDA storing both admin and freeze state.
#[account_type]
#[derive(Debug, Clone, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub struct FreezeConfig {
    /// Admin authority — controls who can change the freeze authority.
    pub admin: AdminConfig,
    /// Freeze authority — controls program and account freezing.
    pub freeze: FreezeState,
}

impl FreezeConfig {
    pub fn new(admin_key: [u8; 32], freeze_key: [u8; 32]) -> Self {
        Self {
            admin: AdminConfig::new(admin_key),
            freeze: FreezeState::new(freeze_key),
        }
    }
}

fn freeze_err(e: FreezeError) -> spel_framework::error::SpelError {
    spel_framework::error::SpelError::Unauthorized {
        message: e.to_string(),
    }
}

#[lez_program]
mod freeze_authority_sample {
    use super::*;

    /// Initialize config PDA with admin and freeze authority.
    #[instruction]
    pub fn initialize(
        #[account(init, pda = literal("config"))]
        config: AccountWithMetadata,
        #[account(signer)]
        admin: AccountWithMetadata,
        freeze_authority: [u8; 32],
    ) -> SpelResult {
        let admin_key = *admin.account_id.value();
        let state = FreezeConfig::new(admin_key, freeze_authority);
        let data = borsh::to_vec(&state).expect("FreezeConfig serializes");
        let mut post_config = config.account.clone();
        post_config.data = data.try_into().expect("data fits");
        Ok(SpelOutput::execute(vec![config, admin], vec![]))
    }

    /// Normal program interaction — blocked when program or account is frozen.
    #[instruction]
    pub fn interact(
        #[account(pda = literal("config"))]
        config: AccountWithMetadata,
        #[account(signer)]
        user: AccountWithMetadata,
        value: u64,
    ) -> SpelResult {
        let state = FreezeConfig::try_from_slice(&config.account.data)
            .map_err(|_| spel_framework::error::SpelError::Unauthorized {
                message: "Failed to deserialize FreezeConfig".to_string(),
            })?;

        let user_key = *user.account_id.value();
        state.freeze.check_interaction(&user_key).map_err(freeze_err)?;

        Ok(SpelOutput::execute(vec![config, user], vec![]))
    }

    /// Freeze the entire program. Freeze authority only.
    #[instruction]
    pub fn freeze_program(
        #[account(mut, pda = literal("config"))]
        config: AccountWithMetadata,
        #[account(signer)]
        freeze_auth: AccountWithMetadata,
    ) -> SpelResult {
        let mut state = FreezeConfig::try_from_slice(&config.account.data)
            .map_err(|_| spel_framework::error::SpelError::Unauthorized {
                message: "Failed to deserialize FreezeConfig".to_string(),
            })?;

        let signer_key = *freeze_auth.account_id.value();
        state.freeze.freeze_program(&signer_key).map_err(freeze_err)?;

        let data = borsh::to_vec(&state).expect("serializes");
        let mut post = config.account.clone();
        post.data = data.try_into().expect("fits");
        Ok(SpelOutput::execute(vec![config, freeze_auth], vec![]))
    }

    /// Unfreeze the entire program. Freeze authority only.
    #[instruction]
    pub fn unfreeze_program(
        #[account(mut, pda = literal("config"))]
        config: AccountWithMetadata,
        #[account(signer)]
        freeze_auth: AccountWithMetadata,
    ) -> SpelResult {
        let mut state = FreezeConfig::try_from_slice(&config.account.data)
            .map_err(|_| spel_framework::error::SpelError::Unauthorized {
                message: "Failed to deserialize FreezeConfig".to_string(),
            })?;

        let signer_key = *freeze_auth.account_id.value();
        state.freeze.unfreeze_program(&signer_key).map_err(freeze_err)?;

        let data = borsh::to_vec(&state).expect("serializes");
        let mut post = config.account.clone();
        post.data = data.try_into().expect("fits");
        Ok(SpelOutput::execute(vec![config, freeze_auth], vec![]))
    }

    /// Freeze a specific account. Freeze authority only.
    #[instruction]
    pub fn freeze_account(
        #[account(mut, pda = literal("config"))]
        config: AccountWithMetadata,
        #[account(signer)]
        freeze_auth: AccountWithMetadata,
        target_account: [u8; 32],
    ) -> SpelResult {
        let mut state = FreezeConfig::try_from_slice(&config.account.data)
            .map_err(|_| spel_framework::error::SpelError::Unauthorized {
                message: "Failed to deserialize FreezeConfig".to_string(),
            })?;

        let signer_key = *freeze_auth.account_id.value();
        state.freeze.freeze_account(&signer_key, target_account).map_err(freeze_err)?;

        let data = borsh::to_vec(&state).expect("serializes");
        let mut post = config.account.clone();
        post.data = data.try_into().expect("fits");
        Ok(SpelOutput::execute(vec![config, freeze_auth], vec![]))
    }

    /// Unfreeze a specific account. Freeze authority only.
    #[instruction]
    pub fn unfreeze_account(
        #[account(mut, pda = literal("config"))]
        config: AccountWithMetadata,
        #[account(signer)]
        freeze_auth: AccountWithMetadata,
        target_account: [u8; 32],
    ) -> SpelResult {
        let mut state = FreezeConfig::try_from_slice(&config.account.data)
            .map_err(|_| spel_framework::error::SpelError::Unauthorized {
                message: "Failed to deserialize FreezeConfig".to_string(),
            })?;

        let signer_key = *freeze_auth.account_id.value();
        state.freeze.unfreeze_account(&signer_key, target_account).map_err(freeze_err)?;

        let data = borsh::to_vec(&state).expect("serializes");
        let mut post = config.account.clone();
        post.data = data.try_into().expect("fits");
        Ok(SpelOutput::execute(vec![config, freeze_auth], vec![]))
    }

    /// Set or revoke freeze authority. Admin only.
    #[instruction]
    pub fn set_freeze_authority(
        #[account(mut, pda = literal("config"))]
        config: AccountWithMetadata,
        #[account(signer)]
        admin: AccountWithMetadata,
        new_freeze_authority: Option<[u8; 32]>,
    ) -> SpelResult {
        let mut state = FreezeConfig::try_from_slice(&config.account.data)
            .map_err(|_| spel_framework::error::SpelError::Unauthorized {
                message: "Failed to deserialize FreezeConfig".to_string(),
            })?;

        let admin_key = *admin.account_id.value();
        // Admin authorizes freeze authority changes via admin_state
        state.admin.admin_state.assert_admin(&admin_key)
            .map_err(|e| spel_framework::error::SpelError::Unauthorized {
                message: e.to_string(),
            })?;
        // Apply the freeze authority change using freeze_key as current authority
        let current_freeze_key = state.freeze.freeze_authority.admin
            .ok_or(spel_framework::error::SpelError::Unauthorized {
                message: "Freeze authority already revoked".to_string(),
            })?;
        state.freeze.set_freeze_authority(&current_freeze_key, new_freeze_authority)
            .map_err(freeze_err)?;

        let data = borsh::to_vec(&state).expect("serializes");
        let mut post = config.account.clone();
        post.data = data.try_into().expect("fits");
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

    fn make_config(admin: [u8; 32], freeze: [u8; 32]) -> AccountWithMetadata {
        let state = FreezeConfig::new(admin, freeze);
        let data = borsh::to_vec(&state).unwrap();
        let mut account = Account::default();
        account.data = data.try_into().unwrap();
        AccountWithMetadata {
            account_id: AccountId::new([0u8; 32]),
            account,
            is_authorized: false,
        }
    }

    fn admin_key() -> [u8; 32] { [1u8; 32] }
    fn freeze_key() -> [u8; 32] { [2u8; 32] }
    fn user_key() -> [u8; 32] { [3u8; 32] }
    fn other_key() -> [u8; 32] { [4u8; 32] }

    #[test]
    fn initialize_works() {
        let config = make_account([0u8; 32], false);
        let admin = make_account(admin_key(), true);
        let result = freeze_authority_sample::initialize(config, admin, freeze_key());
        assert!(result.is_ok());
    }

    #[test]
    fn interact_allowed_when_not_frozen() {
        let config = make_config(admin_key(), freeze_key());
        let user = make_account(user_key(), true);
        let result = freeze_authority_sample::interact(config, user, 42);
        assert!(result.is_ok());
    }

    #[test]
    fn interact_blocked_when_program_frozen() {
        let mut state = FreezeConfig::new(admin_key(), freeze_key());
        state.freeze.freeze_program(&freeze_key()).unwrap();
        let data = borsh::to_vec(&state).unwrap();
        let mut account = Account::default();
        account.data = data.try_into().unwrap();
        let config = AccountWithMetadata {
            account_id: AccountId::new([0u8; 32]),
            account,
            is_authorized: false,
        };
        let user = make_account(user_key(), true);
        let result = freeze_authority_sample::interact(config, user, 42);
        assert!(result.is_err());
    }

    #[test]
    fn freeze_program_works() {
        let config = make_config(admin_key(), freeze_key());
        let freeze_auth = make_account(freeze_key(), true);
        let result = freeze_authority_sample::freeze_program(config, freeze_auth);
        assert!(result.is_ok());
    }

    #[test]
    fn freeze_program_rejected_for_wrong_authority() {
        let config = make_config(admin_key(), freeze_key());
        let wrong = make_account(other_key(), true);
        let result = freeze_authority_sample::freeze_program(config, wrong);
        assert!(result.is_err());
    }

    #[test]
    fn freeze_account_blocks_specific_account() {
        let config = make_config(admin_key(), freeze_key());
        let freeze_auth = make_account(freeze_key(), true);
        let result = freeze_authority_sample::freeze_account(config, freeze_auth, user_key());
        assert!(result.is_ok());
    }

    #[test]
    fn set_freeze_authority_works_for_admin() {
        let config = make_config(admin_key(), freeze_key());
        let admin = make_account(admin_key(), true);
        let result = freeze_authority_sample::set_freeze_authority(config, admin, Some(other_key()));
        assert!(result.is_ok());
    }

    #[test]
    fn set_freeze_authority_revoke_works() {
        let config = make_config(admin_key(), freeze_key());
        let admin = make_account(admin_key(), true);
        let result = freeze_authority_sample::set_freeze_authority(config, admin, None);
        assert!(result.is_ok());
    }

    #[test]
    fn set_freeze_authority_rejected_for_non_admin() {
        let config = make_config(admin_key(), freeze_key());
        let non_admin = make_account(other_key(), true);
        let result = freeze_authority_sample::set_freeze_authority(config, non_admin, Some(other_key()));
        assert!(result.is_err());
    }
}
