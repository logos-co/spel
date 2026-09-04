//! Minimal pause-switch extension.

use spel_framework::prelude::*;
use nssa_core::account::Data;

pub use pause_ext_macros::{instruction, pause_ext, require_not_paused};

// Required for the absolute self-paths the framework copies into consumer codegen.
extern crate self as pause_ext;

#[derive(BorshSerialize, BorshDeserialize, Default)]
pub struct PauseConfig {
    pub paused: bool,
}

impl PauseConfig {
    pub fn read(acct: &AccountWithMetadata) -> Result<Self, SpelError> {
        let bytes = acct.account.data.to_vec();
        if bytes.is_empty() {
            return Ok(Self::default());
        }
        borsh::from_slice(&bytes).map_err(|e| SpelError::custom(1002, format!("pause cfg: {e}")))
    }
}

/// Create the pause PDA. Contributed to the consumer's dispatcher.
#[instruction]
pub fn init_pause(
    #[account(init, pda = literal("pause_config"))] mut pause_config: AccountWithMetadata,
    #[account(signer)] caller: AccountWithMetadata,
) -> SpelResult {
    let bytes = ::borsh::to_vec(&::pause_ext::PauseConfig { paused: false })
        .map_err(|e| SpelError::custom(1003, format!("{e}")))?;
    pause_config.account.data =
        Data::try_from(bytes).map_err(|_| SpelError::custom(1003, "too big"))?;
    Ok(SpelOutput::execute(
        vec![
            (
                pause_config.account,
                AutoClaim::Claimed(Claim::Pda(PdaSeed::new(seed_from_str("pause_config")))),
            ),
            (caller.account, AutoClaim::None),
        ],
        vec![],
    ))
}

/// Flip the pause flag. Contributed to the consumer's dispatcher.
#[instruction]
pub fn set_paused(
    #[account(mut, pda = literal("pause_config"))] mut pause_config: AccountWithMetadata,
    #[account(signer)] caller: AccountWithMetadata,
    paused: bool,
) -> SpelResult {
    let bytes = ::borsh::to_vec(&::pause_ext::PauseConfig { paused })
        .map_err(|e| SpelError::custom(1003, format!("{e}")))?;
    pause_config.account.data =
        Data::try_from(bytes).map_err(|_| SpelError::custom(1003, "too big"))?;
    Ok(SpelOutput::execute(
        vec![
            // already initialised and program-owned: claiming again is
            // ClaimedNonDefaultAccount
            (pause_config.account, AutoClaim::None),
            (caller.account, AutoClaim::None),
        ],
        vec![],
    ))
}
