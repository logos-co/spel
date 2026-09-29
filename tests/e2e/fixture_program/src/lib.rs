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

    /// Delegate a mutation to another program instead of performing it directly.
    ///
    /// This is the shape a program must use for accounts it does not own — a
    /// balance decrease needs the account authorized, and a chained call runs with
    /// the callee as the executing program. Since LEZ v0.2.5 the call names its
    /// pre-states by `AccountId` rather than carrying them: the state machine
    /// resolves them, and the callee inherits authority from the caller's own
    /// authorized set rather than from a flag on a cloned pre-state.
    #[instruction]
    pub fn delegate_to_program(
        #[account(mut, pda = literal("treasury_state"))]
        state: AccountWithMetadata,
        #[account(signer)]
        authority: AccountWithMetadata,
        target: AccountWithMetadata,
        target_program_account_id: AccountId,
    ) -> SpelResult {
        let call = nssa_core::program::ChainedCall {
            program_account_id: target_program_account_id,
            instruction_data: vec![],
            pre_state_ids: vec![target.account_id],
            pda_seeds: vec![],
        };

        // `target` is returned unchanged: this program never mutates it, but every
        // declared account must still appear in the output.
        Ok(SpelOutput::execute(
            vec![state, authority, target],
            vec![call],
        ))
    }

    /// Initialize a private PDA — address is unique per (seed, npk, vpk) tuple.
    #[instruction]
    pub fn init_private_account(
        #[account(init, private_pda, pda = literal("private_vault"), npk = arg("user_npk"), vpk = arg("user_vpk"))]
        account: AccountWithMetadata,
        #[account(signer)]
        authority: AccountWithMetadata,
        user_npk: nssa_core::NullifierPublicKey,
        user_vpk: nssa_core::encryption::ViewingPublicKey,
    ) -> SpelResult {
        Ok(SpelOutput::execute(vec![account, authority], vec![]))
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

    /// Initialize a holding account, validating the definition is owned by this program.
    /// Exercises ProgramContext injection and #[account(owner = self_account_id)].
    #[instruction]
    pub fn initialize_holding(
        ctx: ProgramContext,
        #[account(owner = self_account_id)]
        definition: AccountWithMetadata,
        #[account(init, signer)]
        holding: AccountWithMetadata,
    ) -> SpelResult {
        // ctx.self_account_id and ctx.caller_account_id are available here
        let _ = ctx; // suppress unused warning in this example
        Ok(SpelOutput::execute(vec![definition, holding], vec![]))
    }

    /// Demonstrate a validity window on the program output.
    /// The returned SpelOutput restricts the transaction to a specific block range.
    #[instruction]
    pub fn emit_with_window(
        #[account(signer)]
        authority: AccountWithMetadata,
    ) -> SpelResult {
        Ok(SpelOutput::execute(vec![authority], vec![])
            .try_with_block_validity_window(100u64..200)
            .expect("100..200 is a valid range"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The chained-call path through the macro: a program that delegates a
    /// mutation instead of performing it. Nothing else in the repo exercises a
    /// non-empty `calls` vec, so this covers `SpelOutputParts` plumbing for that
    /// shape.
    #[test]
    fn delegate_emits_chained_call_to_the_target_program() {
        let target_program = AccountId::new([42u8; 32]);
        let out = treasury::delegate_to_program(
            make_account(false),
            make_account(true),
            make_account(false),
            target_program,
        )
        .expect("handler should succeed");

        assert_eq!(out.chained_calls.len(), 1, "one chained call expected");
        assert_eq!(
            out.chained_calls[0].program_account_id, target_program,
            "the call must target the program the caller named"
        );
    }

    #[test]
    fn delegate_names_the_target_as_a_pre_state_of_the_call() {
        // Since v0.2.5 a chained call carries account *ids*, not pre-states: the
        // state machine resolves them, and the callee's authority comes from the
        // caller's own authorized set rather than a flag on a cloned pre-state.
        // Naming the target here is what puts it in the callee's reach at all.
        let target = make_account_with_id([7u8; 32], false);
        let out = treasury::delegate_to_program(
            make_account(false),
            make_account(true),
            target.clone(),
            AccountId::new([42u8; 32]),
        )
        .unwrap();
        assert_eq!(
            out.chained_calls[0].pre_state_ids,
            vec![target.account_id],
            "the call must name the target it delegates to"
        );
    }

    #[test]
    fn delegate_returns_the_target_unmodified() {
        // The whole point of delegating: the caller must not mutate an account it
        // does not own. It must still *report* it — v0.2.5 fails a transaction
        // whose output omits a declared account.
        let target = make_account_with_id([7u8; 32], false);
        let out = treasury::delegate_to_program(
            make_account(false),
            make_account(true),
            target.clone(),
            AccountId::new([42u8; 32]),
        )
        .unwrap();
        assert_eq!(out.state_diffs.len(), 3, "every declared account is reported");
        let target_diff = &out.state_diffs[2];
        assert_eq!(target_diff.pre_state.account_id, target.account_id);
        assert_eq!(
            target_diff.post_data, None,
            "target must come back with no data change"
        );
        assert_eq!(
            target_diff.post_balance_diff,
            nssa_core::account::BalanceDiff::Add(0),
            "target must come back with no balance change"
        );
    }

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
        assert_eq!(idl.instructions.len(), 12);
        assert_eq!(idl.instructions[0].name, "initialize");
    }

    #[test]
    fn idl_json_round_trip() {
        let idl: spel_framework::idl::SpelIdl =
            serde_json::from_str(PROGRAM_IDL_JSON).expect("PROGRAM_IDL_JSON should parse");
        assert_eq!(idl.name, "treasury");
        assert_eq!(idl.instructions.len(), 12);
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

    fn test_program_id() -> AccountId {
        AccountId::new([1u8; 32])
    }

    fn empty_ix_data() -> Vec<u8> {
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
        assert_eq!(result.unwrap().state_diffs.len(), 1); // only authority
    }

    #[test]
    fn idl_has_batch_update_instruction() {
        let idl = __program_idl();
        let ix = idl.instructions.iter().find(|i| i.name == "batch_update")
            .expect("batch_update instruction should be in IDL");
        assert_eq!(ix.args.len(), 1);
        assert_eq!(ix.args[0].name, "value");
    }

    /// The rest-accounts branch must report a diff per account, confirming the
    /// accounts expression is evaluated and extracted correctly.
    #[test]
    fn batch_update_state_diffs_match_account_count() {
        let authority = make_account(true);
        let targets = vec![make_account(false), make_account(false)];
        let result = treasury::batch_update(authority, targets, 99).unwrap();
        assert_eq!(result.state_diffs.len(), 3); // authority + 2 targets
    }

    // ── init_private_account (private PDA) ──────────────────────────────────

    fn make_npk(byte: u8) -> nssa_core::NullifierPublicKey {
        nssa_core::NullifierPublicKey([byte; 32])
    }

    fn make_vpk(d: u8, z: u8) -> nssa_core::encryption::ViewingPublicKey {
        nssa_core::encryption::ViewingPublicKey::from_seed(&[d; 32], &[z; 32])
    }

    #[test]
    fn validate_init_private_account_accepts_correct_address() {
        let program_id = test_program_id();
        let npk = make_npk(0xAB);
        let vpk = make_vpk(0x01, 0x02);
        let correct_id = spel_framework::pda::compute_private_pda(
            &program_id,
            &[&spel_framework::pda::seed_from_str("private_vault")],
            &npk,
            &vpk,
            spel_framework::pda::DEFAULT_PRIVATE_PDA_IDENTIFIER,
        );
        let accounts = vec![
            make_account_with_id(*correct_id.value(), false), // account — correct private PDA
            make_account_with_id([2u8; 32], true),             // authority — signer
        ];
        let result = treasury::__validate_init_private_account(
            &accounts,
            &program_id,
            &empty_ix_data(),
            &npk,
            &vpk,
        );
        assert!(result.is_ok(), "correct private PDA should pass: {result:?}");
    }

    #[test]
    fn validate_init_private_account_rejects_wrong_npk() {
        let program_id = test_program_id();
        let correct_npk = make_npk(0xAB);
        let wrong_npk = make_npk(0xCD);
        let vpk = make_vpk(0x01, 0x02);
        let correct_id = spel_framework::pda::compute_private_pda(
            &program_id,
            &[&spel_framework::pda::seed_from_str("private_vault")],
            &correct_npk,
            &vpk,
            spel_framework::pda::DEFAULT_PRIVATE_PDA_IDENTIFIER,
        );
        // Supply the address for correct_npk but validate with wrong_npk
        let accounts = vec![
            make_account_with_id(*correct_id.value(), false),
            make_account_with_id([2u8; 32], true),
        ];
        let result = treasury::__validate_init_private_account(
            &accounts,
            &program_id,
            &empty_ix_data(),
            &wrong_npk,
            &vpk,
        );
        let err = result.expect_err("wrong npk should fail");
        assert!(
            matches!(err, spel_framework::error::SpelError::PdaMismatch { .. }),
            "expected PdaMismatch, got: {err:?}"
        );
    }

    #[test]
    fn validate_init_private_account_rejects_public_pda_address() {
        let program_id = test_program_id();
        let npk = make_npk(0xAB);
        let vpk = make_vpk(0x01, 0x02);
        // Supply the PUBLIC PDA address — should be rejected
        let public_id = spel_framework::pda::compute_pda(
            &program_id,
            &[&spel_framework::pda::seed_from_str("private_vault")],
        );
        let accounts = vec![
            make_account_with_id(*public_id.value(), false),
            make_account_with_id([2u8; 32], true),
        ];
        let result = treasury::__validate_init_private_account(
            &accounts,
            &program_id,
            &empty_ix_data(),
            &npk,
            &vpk,
        );
        assert!(
            result.is_err(),
            "public PDA address should be rejected for a private PDA account"
        );
    }

    #[test]
    fn idl_init_private_account_marks_pda_as_private() {
        let idl = __program_idl();
        let ix = idl.instructions.iter()
            .find(|i| i.name == "init_private_account")
            .expect("init_private_account must be in IDL");
        let acc = &ix.accounts[0];
        let pda = acc.pda.as_ref().expect("account must have PDA definition");
        assert!(pda.private, "IDL PDA must be marked private");
        assert!(acc.visibility.iter().any(|v| v == "private"), "visibility must include 'private'");
    }

    // ── output filtering ─────────────────────────────────────────────────────
    // The non-owned account filter runs inside the generated `pub fn main()` which is
    // `#[cfg(not(test))]`. It cannot be unit-tested here without a full zkVM harness.
    // The filter logic (pre_states_clone.zip(post_states).filter(...)) is covered by
    // integration/e2e tests that invoke the guest binary end-to-end.

    // ── ProgramContext + owner constraint tests ──────────────────────────────

    fn make_account_with_owner(id: [u8; 32], owner: AccountId, authorized: bool) -> AccountWithMetadata {
        let mut account = nssa_core::account::Account::default();
        account.program_owner = owner;
        AccountWithMetadata {
            account_id: nssa_core::account::AccountId::new(id),
            account,
            is_authorized: authorized,
        }
    }

    #[test]
    fn idl_excludes_context_from_initialize_holding() {
        let idl = __program_idl();
        let ix = idl.instructions.iter().find(|i| i.name == "initialize_holding")
            .expect("initialize_holding must be in IDL");
        // Context must NOT appear in IDL accounts or args
        assert!(!ix.accounts.iter().any(|a| a.name == "ctx"));
        assert_eq!(ix.accounts.len(), 2); // only definition + holding
        assert_eq!(ix.args.len(), 0);
    }

    #[test]
    fn idl_owner_constraint_reflected() {
        let idl = __program_idl();
        let ix = idl.instructions.iter().find(|i| i.name == "initialize_holding")
            .expect("initialize_holding must be in IDL");
        // definition account has #[account(owner = self_account_id)]
        assert_eq!(ix.accounts[0].owner, Some("self_account_id".to_string()));
    }

    #[test]
    fn handler_emit_with_window_callable() {
        let acc = make_account(true);
        let result = treasury::emit_with_window(acc);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.block_validity_window.start(), Some(100));
        assert_eq!(output.block_validity_window.end(), Some(200));
    }

    #[test]
    fn handler_initialize_holding_callable_with_context() {
        let program_id = AccountId::new([1u8; 32]);
        let ctx = ProgramContext::new(program_id, AccountId::new([2u8; 32]));
        let definition = make_account_with_owner([3u8; 32], program_id, false);
        let holding = make_account(true); // init + signer
        let result = treasury::initialize_holding(ctx, definition, holding);
        assert!(result.is_ok(), "handler should succeed with valid context and owner");
    }

    #[test]
    fn validate_initialize_holding_rejects_wrong_owner() {
        let program_id = AccountId::new([1u8; 32]);
        let other_program = AccountId::new([99u8; 32]);
        let accounts = vec![
            make_account_with_owner([3u8; 32], other_program, false), // definition — wrong owner
            make_account_with_id([4u8; 32], true),                     // holding: init + signer (empty)
        ];
        let result = treasury::__validate_initialize_holding(&accounts, &program_id, &empty_ix_data());
        let err = result.expect_err("should reject account with wrong owner");
        assert!(
            matches!(err, spel_framework::error::SpelError::AccountOwnerMismatch { .. }),
            "expected AccountOwnerMismatch, got: {:?}", err
        );
    }

    #[test]
    fn validate_initialize_holding_owner_check_runs_first() {
        // Owner check must fire before init/signer checks.
        // Even if holding is not empty and not authorized, owner error should surface first.
        let program_id = AccountId::new([1u8; 32]);
        let other_program = AccountId::new([99u8; 32]);
        let mut bad_account = nssa_core::account::Account::default();
        bad_account.data = vec![1u8; 32].try_into().unwrap(); // not empty → init violation
        let accounts = vec![
            make_account_with_owner([3u8; 32], other_program, false), // definition — wrong owner
            AccountWithMetadata {
                account_id: nssa_core::account::AccountId::new([4u8; 32]),
                account: bad_account,
                is_authorized: false, // not authorized → signer violation
            },
        ];
        let result = treasury::__validate_initialize_holding(&accounts, &program_id, &empty_ix_data());
        match result.unwrap_err() {
            spel_framework::error::SpelError::AccountOwnerMismatch { account_name } => {
                assert_eq!(account_name, "definition");
            }
            other => panic!("expected AccountOwnerMismatch (owner check runs first), got: {:?}", other),
        }
    }

    #[test]
    fn validate_initialize_holding_accepts_correct_owner() {
        let program_id = AccountId::new([1u8; 32]);
        let accounts = vec![
            make_account_with_owner([3u8; 32], program_id, false), // definition — correct owner
            make_account_with_id([4u8; 32], true),                 // holding: init + signer (empty)
        ];
        let result = treasury::__validate_initialize_holding(&accounts, &program_id, &empty_ix_data());
        assert!(result.is_ok(), "correct owner should pass: {:?}", result);
    }
}
