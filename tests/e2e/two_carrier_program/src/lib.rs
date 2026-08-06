//! Fixture with two structs carrying the same slot attribute.
//!
//! The `mini_ext` marker embeds its config into `config` at an offset,
//! but `CfgA` and `CfgB` both carry `#[mini_slot]`, so the slot binding
//! is ambiguous. The build must refuse with the two-carrier error
//! instead of silently skipping the agreement asserts.

#![allow(dead_code, unused_imports, unused_variables)]

use spel_framework::prelude::*;

use mini_ext::{mini_ext, require_mini};

#[account_type]
pub struct CfgA {
    pub v: u64,
    #[mini_slot]
    pub s: u8,
}

#[account_type]
pub struct CfgB {
    pub v: u64,
    #[mini_slot]
    pub s: u8,
}

#[lez_program]
#[mini_ext(mini_config = config, offset = 8)]
mod two_carrier {
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
