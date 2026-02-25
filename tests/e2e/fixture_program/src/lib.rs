//! Fixture program for e2e tests.
//!
//! Uses #[lez_program] to exercise the full macro expansion,
//! IDL generation, and handler invocation on the host.

#![allow(dead_code, unused_imports, unused_variables)]

use lez_framework::prelude::*;

#[lez_program]
mod treasury {
    #[allow(unused_imports)]
    use super::*;

    /// Initialize the treasury state.
    #[instruction]
    pub fn initialize(
        #[account(init, pda = literal("treasury_state"))]
        state: AccountWithMetadata,
        #[account(signer)]
        authority: AccountWithMetadata,
        threshold: u64,
    ) -> LezResult {
        Ok(LezOutput::states_only(vec![]))
    }

    /// Transfer funds.
    #[instruction]
    pub fn transfer(
        #[account(mut)]
        from: AccountWithMetadata,
        #[account(mut)]
        to: AccountWithMetadata,
        #[account(signer)]
        signer: AccountWithMetadata,
        amount: u64,
        memo: String,
    ) -> LezResult {
        Ok(LezOutput::states_only(vec![]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_account(authorized: bool) -> AccountWithMetadata {
        AccountWithMetadata {
            account_id: nssa_core::account::AccountId::new([0u8; 32]),
            account: nssa_core::account::Account::default(),
            is_authorized: authorized,
        }
    }

    #[test]
    fn idl_has_expected_instructions() {
        let idl = __program_idl();
        assert_eq!(idl.name, "treasury");
        assert_eq!(idl.version, "0.1.0");
        assert_eq!(idl.instructions.len(), 2);
        assert_eq!(idl.instructions[0].name, "initialize");
        assert_eq!(idl.instructions[1].name, "transfer");
    }

    #[test]
    fn idl_json_round_trip() {
        let idl: lez_framework::idl::LezIdl =
            serde_json::from_str(PROGRAM_IDL_JSON).expect("PROGRAM_IDL_JSON should parse");
        assert_eq!(idl.name, "treasury");
        assert_eq!(idl.instructions.len(), 2);
    }

    #[test]
    fn initialize_instruction_metadata() {
        let idl = __program_idl();
        let ix = &idl.instructions[0];
        assert_eq!(ix.name, "initialize");
        assert_eq!(ix.accounts.len(), 2);
        // First account: init + PDA
        assert!(ix.accounts[0].init);
        assert!(ix.accounts[0].writable); // init implies writable
        assert!(ix.accounts[0].pda.is_some());
        // Second account: signer
        assert!(ix.accounts[1].signer);
        // Args
        assert_eq!(ix.args.len(), 1);
        assert_eq!(ix.args[0].name, "threshold");
    }

    #[test]
    fn transfer_instruction_metadata() {
        let idl = __program_idl();
        let ix = &idl.instructions[1];
        assert_eq!(ix.name, "transfer");
        assert_eq!(ix.accounts.len(), 3);
        assert!(ix.accounts[0].writable); // from: mut
        assert!(ix.accounts[1].writable); // to: mut
        assert!(ix.accounts[2].signer);   // signer
        assert_eq!(ix.args.len(), 2);
        assert_eq!(ix.args[0].name, "amount");
        assert_eq!(ix.args[1].name, "memo");
    }

    /// Validates the cfg-gate fix: handler functions are directly callable
    /// from host-side tests without triggering zkVM syscalls.
    #[test]
    fn handler_initialize_callable() {
        let acc = make_account(true);
        let result = treasury::initialize(acc.clone(), acc.clone(), 5);
        assert!(result.is_ok());
    }

    #[test]
    fn handler_transfer_callable() {
        let acc = make_account(true);
        let result = treasury::transfer(
            acc.clone(),
            acc.clone(),
            acc.clone(),
            100,
            "test memo".to_string(),
        );
        assert!(result.is_ok());
    }
}
