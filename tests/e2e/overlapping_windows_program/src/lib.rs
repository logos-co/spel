//! Fixture with two extensions embedding into one account at
//! overlapping offsets.
//!
//! `mini_a` claims bytes 0..16 and `mini_b` claims 8..24 of `config`,
//! each window sized by its declared state type. Discovery cannot see
//! the overlap, the offsets differ, so the build must refuse through
//! the emitted window collision assert instead of compiling a program
//! whose extensions cross-corrupt.

#![allow(dead_code, unused_imports, unused_variables)]

use spel_framework::prelude::*;

use mini_a::mini_a;
use mini_b::mini_b;

#[lez_program]
#[mini_a(a_config = config, offset = 0)]
#[mini_b(b_config = config, offset = 8)]
mod overlapping_windows {
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
