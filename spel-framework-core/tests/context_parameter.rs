//! Test that ProgramContext and ClockContext are properly handled by the framework.
//!
//! This tests the contract: instruction handlers can declare ProgramContext and/or
//! ClockContext parameters and receive trusted execution metadata without polluting the IDL.

use nssa_core::program::ProgramId;
use spel_framework_core::context::{ClockContext, ProgramContext};

/// Verify ProgramContext can be constructed and accessed.
#[test]
fn test_program_context_construction() {
    let self_id: ProgramId = [1u32; 8];
    let caller_id: ProgramId = [2u32; 8];

    let ctx = ProgramContext::new(self_id, caller_id);

    assert_eq!(ctx.self_program_id, self_id);
    assert_eq!(ctx.caller_program_id, caller_id);
}

/// Verify ProgramContext implements Copy (original remains usable after assignment).
#[test]
fn test_program_context_copy() {
    let ctx = ProgramContext::new([1u32; 8], [2u32; 8]);
    let ctx2 = ctx; // Copy — ctx is still valid after this
    assert_eq!(ctx.self_program_id, ctx2.self_program_id);
    assert_eq!(ctx.caller_program_id, ctx2.caller_program_id);
}

/// Verify ProgramContext implements Clone (explicit trait call, independent of Copy).
#[test]
fn test_program_context_clone() {
    let ctx = ProgramContext::new([1u32; 8], [2u32; 8]);
    let cloned = ctx.clone();
    assert_eq!(ctx.self_program_id, cloned.self_program_id);
    assert_eq!(ctx.caller_program_id, cloned.caller_program_id);
}

/// Verify ProgramContext implements Debug.
#[test]
fn test_program_context_debug() {
    let ctx = ProgramContext::new([1u32; 8], [2u32; 8]);
    let debug_str = format!("{ctx:?}");
    assert!(debug_str.contains("ProgramContext"));
}

/// Verify ProgramContext implements PartialEq and Eq.
#[test]
fn test_program_context_equality() {
    let ctx1 = ProgramContext::new([1u32; 8], [2u32; 8]);
    let ctx2 = ProgramContext::new([1u32; 8], [2u32; 8]);
    let ctx3 = ProgramContext::new([3u32; 8], [4u32; 8]);

    assert_eq!(ctx1, ctx2);
    assert_ne!(ctx1, ctx3);
}

/// Simulate a handler that uses context for owner validation.
/// This mirrors what the macro-generated dispatch would call:
/// ```
/// mod_name::initialize(
///     ProgramContext::new(self_program_id, caller_program_id),
///     definition_account,
///     holding_account,
/// )
/// ```
#[test]
fn test_handler_uses_context_for_owner_check() {
    let self_program: ProgramId = [1u32; 8];
    let other_program: ProgramId = [99u32; 8];

    // Simulated account with program_owner field
    #[derive(Clone, Debug)]
    struct MockAccount {
        program_owner: ProgramId,
    }

    // Handler that validates owner using context (as the macro would generate)
    fn validate_owner(ctx: &ProgramContext, account: &MockAccount) -> Result<(), String> {
        if account.program_owner != ctx.self_program_id {
            return Err(format!(
                "Account owner mismatch: expected {:?}, got {:?}",
                ctx.self_program_id, account.program_owner
            ));
        }
        Ok(())
    }

    // Case 1: Account owned by this program — passes
    let owned_account = MockAccount {
        program_owner: self_program,
    };
    let ctx = ProgramContext::new(self_program, [0u32; 8]);
    assert!(validate_owner(&ctx, &owned_account).is_ok());

    // Case 2: Account owned by different program — fails
    let foreign_account = MockAccount {
        program_owner: other_program,
    };
    assert!(validate_owner(&ctx, &foreign_account).is_err());
}

/// Verify that context can be used to access caller_program_id.
#[test]
fn test_handler_uses_caller_program_id() {
    let self_program: ProgramId = [1u32; 8];
    let caller_program: ProgramId = [42u32; 8];

    let ctx = ProgramContext::new(self_program, caller_program);

    // Simulated handler logic that checks caller
    fn is_authorized_caller(ctx: &ProgramContext, expected_caller: ProgramId) -> bool {
        ctx.caller_program_id == expected_caller
    }

    assert!(is_authorized_caller(&ctx, caller_program));
    assert!(!is_authorized_caller(&ctx, [99u32; 8]));
}

// ── ClockContext tests ────────────────────────────────────────────────────────

#[test]
fn test_clock_context_construction() {
    let clock = ClockContext::new(42, 1_700_000_000_000);
    assert_eq!(clock.block_id, 42);
    assert_eq!(clock.timestamp, 1_700_000_000_000);
}

#[test]
fn test_clock_context_clone_copy() {
    let clock = ClockContext::new(10, 999);
    let clock2 = clock;
    let clock3 = clock.clone();
    assert_eq!(clock.block_id, clock2.block_id);
    assert_eq!(clock.block_id, clock3.block_id);
}

#[test]
fn test_clock_context_equality() {
    let a = ClockContext::new(1, 100);
    let b = ClockContext::new(1, 100);
    let c = ClockContext::new(2, 200);
    assert_eq!(a, b);
    assert_ne!(a, c);
}

#[test]
fn test_clock_context_debug() {
    let clock = ClockContext::new(7, 42);
    let s = format!("{:?}", clock);
    assert!(s.contains("ClockContext"));
}

#[test]
fn test_clock_context_borsh_roundtrip() {
    use borsh::{BorshDeserialize, BorshSerialize};

    // ClockAccountData layout: block_id (u64 LE) then timestamp (u64 LE)
    let mut bytes = Vec::new();
    99u64.serialize(&mut bytes).unwrap();
    1_234_567_890_000u64.serialize(&mut bytes).unwrap();

    let clock = ClockContext::try_from_slice(&bytes).expect("borsh decode should succeed");
    assert_eq!(clock.block_id, 99);
    assert_eq!(clock.timestamp, 1_234_567_890_000);
}

#[test]
fn test_clock_context_borsh_wrong_length_fails() {
    use borsh::BorshDeserialize;
    let result = ClockContext::try_from_slice(&[0u8; 4]);
    assert!(result.is_err(), "decoding 4 bytes should fail (need 16)");
}
