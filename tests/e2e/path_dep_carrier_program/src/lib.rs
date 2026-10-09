//! Fixture proving the binding scan reaches a path-dependency carrier,
//! and that IDL generation needs no carrier at all.
//!
//! The `#[mini_slot]` carrier lives in the `shared_core` path dep, not
//! here, and the marker declares no `offset`. The dispatcher expansion
//! must find `Cfg` through the path-dep arm of the binding scan to
//! resolve the derivation, so a clean build is that half of the proof.
//! The other half is [`e2e_path_dep_carrier_generates_idl`]: the IDL
//! carries no offset, so its producers run the gate pass under
//! `GateLocations::Omit` and generate without resolving anything.

#![allow(dead_code, unused_imports, unused_variables)]

use spel_framework::prelude::*;

use mini_ext::{mini_ext, require_mini};
pub use shared_core::Cfg;

#[lez_program]
#[mini_ext(mini_config = config)]
mod path_dep_carrier {
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

    /// Gated, so the embedded role is rewritten to `config` and injected
    /// here. The IDL producers must list that account without ever
    /// resolving the offset the gate reads.
    #[instruction]
    #[require_mini]
    pub fn bump(value: u64) -> SpelResult {
        Ok(SpelOutput::execute(vec![], vec![]))
    }
}
