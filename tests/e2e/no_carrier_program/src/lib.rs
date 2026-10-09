//! Fixture with a derivation and no carrier.
//!
//! The `mini_ext` marker omits the offset, so the framework must derive
//! it, but no struct carries the slot field marker. The build must
//! refuse with the missing-carrier error spelling out the fix.

#![allow(dead_code, unused_imports, unused_variables)]

use spel_framework::prelude::*;

use mini_ext::{mini_ext, require_mini};

#[account_type]
pub struct Cfg {
    pub v: u64,
    pub s: u8,
}

#[lez_program]
#[mini_ext(mini_config = config)]
mod no_carrier {
    #[allow(unused_imports)]
    use super::*;

    #[instruction]
    pub fn initialize(
        #[account(init, pda = literal("cfg"))]
        config: AccountWithMetadata,
        #[account(signer)]
        payer: AccountWithMetadata,
    ) -> SpelResult {
        Ok(SpelOutput::execute(vec![config, payer], vec![]))
    }
}
