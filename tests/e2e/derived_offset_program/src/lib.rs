//! Fixture proving derived offsets end to end.
//!
//! The `mini_ext` marker names its embedding account and nothing else:
//! no `offset` kwarg anywhere. The slot position comes entirely from
//! the `#[mini_slot]` field marker, resolved to `Cfg::MINI_SLOT_OFFSET`
//! and stamped through gate kwargs and dispatch. Building this crate is
//! the proof.

#![allow(dead_code, unused_imports, unused_variables)]

use spel_framework::prelude::*;

use mini_ext::{mini_ext, require_mini};

#[account_type]
pub struct Cfg {
    pub v: u64,
    #[mini_slot]
    pub s: u8,
}

#[lez_program]
#[mini_ext(mini_config = config)]
mod derived_offset {
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
