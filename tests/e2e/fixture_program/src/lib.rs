//! Fixture program for e2e tests.
//!
//! Uses #[lez_program] to exercise the full macro expansion,
//! IDL generation, and handler invocation on the host.

#![allow(dead_code, unused_imports, unused_variables)]

use spel_framework::prelude::*;

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
    ) -> SpelResult {
        Ok(SpelOutput::execute(vec![state, authority], vec![]))
    }

    /// Create a user vault (PDA from arg seed).
    #[instruction]
    pub fn create_vault(
        #[account(init, pda = arg("owner_key"))]
        vault: AccountWithMetadata,
        #[account(signer)]
        owner: AccountWithMetadata,
        owner_key: [u8; 32],
    ) -> SpelResult {
        Ok(SpelOutput::execute(vec![vault, owner], vec![]))
    }

    /// Create a user config (PDA from literal + arg multi-seed).
    #[instruction]
    pub fn create_config(
        #[account(init, pda = [literal("config"), arg("user_id")])]
        config: AccountWithMetadata,
        #[account(signer)]
        admin: AccountWithMetadata,
        user_id: [u8; 32],
    ) -> SpelResult {
        Ok(SpelOutput::execute(vec![config, admin], vec![]))
    }

    /// Create a ledger entry (PDA from literal + u64 arg + u32 arg).
    #[instruction]
    pub fn create_ledger(
        #[account(init, pda = [literal("ledger"), arg("user_id"), arg("seq")])]
        ledger: AccountWithMetadata,
        #[account(signer)]
        authority: AccountWithMetadata,
        user_id: u64,
        seq: u32,
    ) -> SpelResult {
        Ok(SpelOutput::execute(vec![ledger, authority], vec![]))
    }

    /// Register a named entity (PDA from arg + arg with String type).
    #[instruction]
    pub fn register_entity(
        #[account(init, pda = [arg("domain"), arg("name")])]
        entity: AccountWithMetadata,
        #[account(signer)]
        registrar: AccountWithMetadata,
        domain: String,
        name: String,
    ) -> SpelResult {
        Ok(SpelOutput::execute(vec![entity, registrar], vec![]))
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
    ) -> SpelResult {
        Ok(SpelOutput::execute(vec![from, to, signer], vec![]))
    }

    /// Create a record whose PDA is derived from the owner's account ID.
    /// Exercises the `account("owner")` PDA seed variant in both claim generation
    /// and validation.
    #[instruction]
    pub fn create_record(
        #[account(init, pda = account("owner"))]
        record: AccountWithMetadata,
        #[account(signer)]
        owner: AccountWithMetadata,
    ) -> SpelResult {
        Ok(SpelOutput::execute(vec![record, owner], vec![]))
    }

    /// Batch update: one fixed authority + variable-length list of target accounts.
    #[instruction]
    pub fn batch_update(
        #[account(signer)]
        authority: AccountWithMetadata,
        #[account(mut)]
        targets: Vec<AccountWithMetadata>,
        value: u64,
    ) -> SpelResult {
        let mut accounts = vec![authority];
        accounts.extend(targets);
        Ok(SpelOutput::execute(accounts, vec![]))
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
        assert_eq!(idl.instructions.len(), 8);
        assert_eq!(idl.instructions[0].name, "initialize");
    }

    #[test]
    fn idl_json_round_trip() {
        let idl: spel_framework::idl::SpelIdl =
            serde_json::from_str(PROGRAM_IDL_JSON).expect("PROGRAM_IDL_JSON should parse");
        assert_eq!(idl.name, "treasury");
        assert_eq!(idl.instructions.len(), 8);
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
        let ix = &idl.instructions[5];
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

    #[test]
    fn create_vault_instruction_metadata() {
        let idl = __program_idl();
        let ix = &idl.instructions[1]; // create_vault is second
        assert_eq!(ix.name, "create_vault");
        assert_eq!(ix.accounts.len(), 2);
        assert!(ix.accounts[0].init);
        assert!(ix.accounts[0].pda.is_some());
        let pda = ix.accounts[0].pda.as_ref().unwrap();
        assert_eq!(pda.seeds.len(), 1); // arg seed
        assert_eq!(ix.args.len(), 1);
        assert_eq!(ix.args[0].name, "owner_key");
    }

    #[test]
    fn create_config_instruction_metadata() {
        let idl = __program_idl();
        let ix = &idl.instructions[2]; // create_config is third
        assert_eq!(ix.name, "create_config");
        assert_eq!(ix.accounts.len(), 2);
        assert!(ix.accounts[0].init);
        assert!(ix.accounts[0].pda.is_some());
        let pda = ix.accounts[0].pda.as_ref().unwrap();
        assert_eq!(pda.seeds.len(), 2); // literal + arg
    }

    #[test]
    fn handler_create_vault_callable() {
        let acc = make_account(true);
        let result = treasury::create_vault(acc.clone(), acc.clone(), [42u8; 32]);
        assert!(result.is_ok());
    }

    #[test]
    fn handler_create_config_callable() {
        let acc = make_account(true);
        let result = treasury::create_config(acc.clone(), acc.clone(), [99u8; 32]);
        assert!(result.is_ok());
    }

    // ── PDA validation tests ─────────────────────────────────────────

    fn make_account_with_id(id: [u8; 32], authorized: bool) -> AccountWithMetadata {
        AccountWithMetadata {
            account_id: nssa_core::account::AccountId::new(id),
            account: nssa_core::account::Account::default(),
            is_authorized: authorized,
        }
    }

    fn test_program_id() -> nssa_core::program::ProgramId {
        [1u32; 8]
    }

    fn empty_ix_data() -> Vec<u32> {
        vec![]
    }

    // ── create_vault (single arg seed) ───────────────────────────────

    #[test]
    fn validate_create_vault_rejects_wrong_pda() {
        let program_id = test_program_id();
        let owner_key = [42u8; 32];

        // Compute the correct PDA so we can supply a *different* one
        let correct_id = spel_framework::pda::compute_pda(&program_id, &[&owner_key]);
        let wrong_id = [0xFFu8; 32]; // definitely not the correct PDA
        assert_ne!(
            nssa_core::account::AccountId::new(wrong_id),
            correct_id,
            "test precondition: wrong_id must differ from correct PDA"
        );

        let accounts = vec![
            make_account_with_id(wrong_id, false), // vault — wrong address
            make_account_with_id([2u8; 32], true),  // owner — signer
        ];

        let result = treasury::__validate_create_vault(
            &accounts,
            &program_id,
            &empty_ix_data(),
            &owner_key,
        );
        let err = result.expect_err("should reject wrong PDA");
        assert!(
            matches!(err, spel_framework::error::SpelError::PdaMismatch { .. }),
            "expected PdaMismatch, got: {err:?}"
        );
    }

    #[test]
    fn validate_create_vault_accepts_correct_pda() {
        let program_id = test_program_id();
        let owner_key = [42u8; 32];
        let correct_id = spel_framework::pda::compute_pda(&program_id, &[&owner_key]);

        let accounts = vec![
            make_account_with_id(*correct_id.value(), false), // vault — correct PDA
            make_account_with_id([2u8; 32], true),             // owner — signer
        ];

        let result = treasury::__validate_create_vault(
            &accounts,
            &program_id,
            &empty_ix_data(),
            &owner_key,
        );
        assert!(result.is_ok(), "correct PDA should pass: {result:?}");
    }

    // ── create_config (multi-seed: literal + arg) ────────────────────

    #[test]
    fn validate_create_config_rejects_wrong_pda() {
        let program_id = test_program_id();
        let user_id = [99u8; 32];
        let config_seed = spel_framework::pda::seed_from_str("config");

        let correct_id =
            spel_framework::pda::compute_pda(&program_id, &[&config_seed, &user_id]);
        let wrong_id = [0xAAu8; 32];
        assert_ne!(
            nssa_core::account::AccountId::new(wrong_id),
            correct_id,
            "test precondition: wrong_id must differ from correct PDA"
        );

        let accounts = vec![
            make_account_with_id(wrong_id, false), // config — wrong address
            make_account_with_id([2u8; 32], true),  // admin — signer
        ];

        let result = treasury::__validate_create_config(
            &accounts,
            &program_id,
            &empty_ix_data(),
            &user_id,
        );
        let err = result.expect_err("should reject wrong PDA");
        assert!(
            matches!(err, spel_framework::error::SpelError::PdaMismatch { .. }),
            "expected PdaMismatch, got: {err:?}"
        );
    }

    #[test]
    fn validate_create_config_accepts_correct_pda() {
        let program_id = test_program_id();
        let user_id = [99u8; 32];
        let config_seed = spel_framework::pda::seed_from_str("config");

        let correct_id =
            spel_framework::pda::compute_pda(&program_id, &[&config_seed, &user_id]);

        let accounts = vec![
            make_account_with_id(*correct_id.value(), false), // config — correct PDA
            make_account_with_id([2u8; 32], true),             // admin — signer
        ];

        let result = treasury::__validate_create_config(
            &accounts,
            &program_id,
            &empty_ix_data(),
            &user_id,
        );
        assert!(result.is_ok(), "correct PDA should pass: {result:?}");
    }

    // ── create_ledger (literal + u64 arg + u32 arg) ─────────────────

    #[test]
    fn validate_create_ledger_rejects_wrong_pda() {
        use spel_framework::pda::ToSeed;

        let program_id = test_program_id();
        let user_id: u64 = 42;
        let seq: u32 = 7;

        let correct_id = spel_framework::pda::compute_pda_multi(
            &program_id,
            &[&"ledger", &user_id, &seq],
        );
        let wrong_id = [0xBBu8; 32];
        assert_ne!(
            nssa_core::account::AccountId::new(wrong_id),
            correct_id,
        );

        let accounts = vec![
            make_account_with_id(wrong_id, false),
            make_account_with_id([2u8; 32], true),
        ];

        let result = treasury::__validate_create_ledger(
            &accounts,
            &program_id,
            &empty_ix_data(),
            &user_id,
            &seq,
        );
        let err = result.expect_err("should reject wrong PDA");
        assert!(
            matches!(err, spel_framework::error::SpelError::PdaMismatch { .. }),
            "expected PdaMismatch, got: {err:?}"
        );
    }

    #[test]
    fn validate_create_ledger_accepts_correct_pda() {
        use spel_framework::pda::ToSeed;

        let program_id = test_program_id();
        let user_id: u64 = 42;
        let seq: u32 = 7;

        let correct_id = spel_framework::pda::compute_pda_multi(
            &program_id,
            &[&"ledger", &user_id, &seq],
        );

        let accounts = vec![
            make_account_with_id(*correct_id.value(), false),
            make_account_with_id([2u8; 32], true),
        ];

        let result = treasury::__validate_create_ledger(
            &accounts,
            &program_id,
            &empty_ix_data(),
            &user_id,
            &seq,
        );
        assert!(result.is_ok(), "correct PDA should pass: {result:?}");
    }

    // ── register_entity (String arg + String arg) ───────────────────

    #[test]
    fn validate_register_entity_rejects_wrong_pda() {
        use spel_framework::pda::ToSeed;

        let program_id = test_program_id();
        let domain = String::from("gaming");
        let name = String::from("player1");

        let correct_id = spel_framework::pda::compute_pda_multi(
            &program_id,
            &[&domain, &name],
        );
        let wrong_id = [0xCCu8; 32];
        assert_ne!(
            nssa_core::account::AccountId::new(wrong_id),
            correct_id,
        );

        let accounts = vec![
            make_account_with_id(wrong_id, false),
            make_account_with_id([2u8; 32], true),
        ];

        let result = treasury::__validate_register_entity(
            &accounts,
            &program_id,
            &empty_ix_data(),
            &domain,
            &name,
        );
        let err = result.expect_err("should reject wrong PDA");
        assert!(
            matches!(err, spel_framework::error::SpelError::PdaMismatch { .. }),
            "expected PdaMismatch, got: {err:?}"
        );
    }

    #[test]
    fn validate_register_entity_accepts_correct_pda() {
        use spel_framework::pda::ToSeed;

        let program_id = test_program_id();
        let domain = String::from("gaming");
        let name = String::from("player1");

        let correct_id = spel_framework::pda::compute_pda_multi(
            &program_id,
            &[&domain, &name],
        );

        let accounts = vec![
            make_account_with_id(*correct_id.value(), false),
            make_account_with_id([2u8; 32], true),
        ];

        let result = treasury::__validate_register_entity(
            &accounts,
            &program_id,
            &empty_ix_data(),
            &domain,
            &name,
        );
        assert!(result.is_ok(), "correct PDA should pass: {result:?}");
    }

    // ── create_record (account(...) PDA seed) ────────────────────────────────

    #[test]
    fn handler_create_record_callable() {
        let acc = make_account(true);
        let result = treasury::create_record(acc.clone(), acc.clone());
        assert!(result.is_ok());
    }

    /// Critical regression test: __claims_create_record must encode the *owner's account ID*
    /// as the PDA seed, not a hash of the string "owner". Before the fix, Account PDA seeds
    /// used seed_from_str(account_name) which is always wrong.
    #[test]
    fn claims_create_record_encodes_owner_account_id_as_seed() {
        let owner_id = [42u8; 32];
        let claims = treasury::__claims_create_record(&owner_id);

        assert_eq!(claims.len(), 2);

        // record (index 0): must be a PDA claim — not None, not Authorized
        assert!(
            matches!(&claims[0], spel_framework::spel_output::AutoClaim::Claimed(_)),
            "record claim should be Claimed(Pda(...)), got: {:?}",
            &claims[0]
        );

        // owner (index 1): signer only, not init → no claim
        assert!(
            matches!(&claims[1], spel_framework::spel_output::AutoClaim::None),
            "owner claim should be None, got: {:?}",
            &claims[1]
        );

        // The encoded seed must be the owner_id bytes, not seed_from_str("owner").
        let wrong_seed = spel_framework::pda::seed_from_str("owner");
        let wrong_claim = spel_framework::spel_output::AutoClaim::Claimed(
            nssa_core::program::Claim::Pda(nssa_core::program::PdaSeed::new(wrong_seed))
        );
        assert_ne!(
            claims[0], wrong_claim,
            "claim must use the runtime account ID, not seed_from_str(\"owner\")"
        );

        // It must match the claim built from the actual owner_id bytes.
        let correct_claim = spel_framework::spel_output::AutoClaim::Claimed(
            nssa_core::program::Claim::Pda(nssa_core::program::PdaSeed::new(owner_id))
        );
        assert_eq!(claims[0], correct_claim);
    }

    #[test]
    fn validate_create_record_accepts_correct_pda() {
        let program_id = test_program_id();
        let owner_id = [42u8; 32];
        let correct_pda = spel_framework::pda::compute_pda(&program_id, &[&owner_id]);

        let accounts = vec![
            make_account_with_id(*correct_pda.value(), false), // record — correct PDA
            make_account_with_id(owner_id, true),               // owner — signer
        ];

        let result = treasury::__validate_create_record(&accounts, &program_id, &empty_ix_data());
        assert!(result.is_ok(), "correct PDA should pass: {result:?}");
    }

    #[test]
    fn validate_create_record_rejects_wrong_pda() {
        let program_id = test_program_id();
        let owner_id = [42u8; 32];

        let accounts = vec![
            make_account_with_id([0xFFu8; 32], false), // record — wrong address
            make_account_with_id(owner_id, true),       // owner — signer
        ];

        let result = treasury::__validate_create_record(&accounts, &program_id, &empty_ix_data());
        let err = result.expect_err("wrong PDA should fail");
        assert!(
            matches!(err, spel_framework::error::SpelError::PdaMismatch { .. }),
            "expected PdaMismatch, got: {err:?}"
        );
    }

    // ── batch_update (rest accounts / ExecuteTransformer arbitrary expression) ──

    #[test]
    fn handler_batch_update_callable() {
        let acc = make_account(true);
        let targets = vec![make_account(false), make_account(false), make_account(false)];
        let result = treasury::batch_update(acc, targets, 42);
        assert!(result.is_ok());
    }

    #[test]
    fn handler_batch_update_empty_targets() {
        let acc = make_account(true);
        let result = treasury::batch_update(acc, vec![], 0);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().post_states.len(), 1); // only authority
    }

    #[test]
    fn idl_has_batch_update_instruction() {
        let idl = __program_idl();
        let ix = idl.instructions.iter().find(|i| i.name == "batch_update")
            .expect("batch_update instruction should be in IDL");
        assert_eq!(ix.args.len(), 1);
        assert_eq!(ix.args[0].name, "value");
    }

    /// Tests the ExecuteTransformer rest-branch (arbitrary accounts expression):
    /// __claims_batch_update(rest_count) must return 1 + rest_count claims.
    #[test]
    fn claims_batch_update_rest_count() {
        let claims = treasury::__claims_batch_update(3);
        assert_eq!(claims.len(), 4); // 1 fixed (authority) + 3 rest (targets)
        assert!(matches!(&claims[0], spel_framework::spel_output::AutoClaim::None)); // authority
        for claim in &claims[1..] {
            assert!(matches!(claim, spel_framework::spel_output::AutoClaim::None)); // targets
        }
    }

    /// Tests that the rest-branch ExecuteTransformer produces the correct number of
    /// post_states, confirming the accounts expression is evaluated and extracted correctly.
    #[test]
    fn batch_update_post_states_match_account_count() {
        let authority = make_account(true);
        let targets = vec![make_account(false), make_account(false)];
        let result = treasury::batch_update(authority, targets, 99).unwrap();
        assert_eq!(result.post_states.len(), 3); // authority + 2 targets
    }

    // ── output filtering ─────────────────────────────────────────────────────
    // The non-owned account filter runs inside the generated `pub fn main()` which is
    // `#[cfg(not(test))]`. It cannot be unit-tested here without a full zkVM harness.
    // The filter logic (pre_states_clone.zip(post_states).filter(...)) is covered by
    // integration/e2e tests that invoke the guest binary end-to-end.

}
