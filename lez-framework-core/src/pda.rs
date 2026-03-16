//! Generic PDA (Program Derived Address) computation utilities.

use nssa_core::account::AccountId;
use nssa_core::program::{PdaSeed, ProgramId};
use sha2::{Sha256, Digest};

/// Convert a string to a zero-padded 32-byte seed.
///
/// # Panics
///
/// Panics if the string is longer than 32 bytes.
pub fn seed_from_str(s: &str) -> [u8; 32] {
    let src = s.as_bytes();
    assert!(src.len() <= 32, "seed string '{}' exceeds 32 bytes", s);
    let mut bytes = [0u8; 32];
    bytes[..src.len()].copy_from_slice(src);
    bytes
}

/// Compute a PDA `AccountId` from a program ID and one or more 32-byte seeds.
///
/// - Single seed: used directly as the PDA seed.
/// - Multiple seeds: combined via SHA-256(seed1 || seed2 || ...) into a single
///   32-byte seed. This avoids XOR commutativity and self-cancellation issues.
///
/// # Panics
///
/// Panics if `seeds` is empty.
pub fn compute_pda(program_id: &ProgramId, seeds: &[&[u8; 32]]) -> AccountId {
    assert!(!seeds.is_empty(), "PDA requires at least one seed");

    let combined = if seeds.len() == 1 {
        *seeds[0]
    } else {
        let mut hasher = Sha256::new();
        for seed in seeds {
            hasher.update(seed);
        }
        hasher.finalize().into()
    };

    let pda_seed = PdaSeed::new(combined);
    AccountId::from((program_id, &pda_seed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_seed_from_str_basic() {
        let seed = seed_from_str("hello");
        assert_eq!(&seed[..5], b"hello");
        assert_eq!(&seed[5..], &[0u8; 27]);
    }

    #[test]
    fn test_seed_from_str_exact_32() {
        let s = "abcdefghijklmnopqrstuvwxyz012345"; // 32 bytes
        let seed = seed_from_str(s);
        assert_eq!(&seed, s.as_bytes());
    }

    #[test]
    #[should_panic(expected = "exceeds 32 bytes")]
    fn test_seed_from_str_too_long() {
        seed_from_str("abcdefghijklmnopqrstuvwxyz0123456"); // 33 bytes
    }

    #[test]
    fn test_seed_from_str_empty() {
        let seed = seed_from_str("");
        assert_eq!(seed, [0u8; 32]);
    }

    #[test]
    fn test_compute_pda_single_seed() {
        let program_id: ProgramId = [1u32; 8];
        let seed = seed_from_str("test_seed");
        let account = compute_pda(&program_id, &[&seed]);

        // Same input must always produce the same output
        let account2 = compute_pda(&program_id, &[&seed]);
        assert_eq!(account, account2);
    }

    #[test]
    fn test_compute_pda_multi_seed() {
        let program_id: ProgramId = [1u32; 8];
        let seed1 = seed_from_str("prefix");
        let seed2 = [42u8; 32];
        let account = compute_pda(&program_id, &[&seed1, &seed2]);

        let account2 = compute_pda(&program_id, &[&seed1, &seed2]);
        assert_eq!(account, account2);
    }

    #[test]
    fn test_compute_pda_different_programs() {
        let prog_a: ProgramId = [1u32; 8];
        let prog_b: ProgramId = [2u32; 8];
        let seed = seed_from_str("same_seed");

        let a = compute_pda(&prog_a, &[&seed]);
        let b = compute_pda(&prog_b, &[&seed]);
        assert_ne!(a, b);
    }

    #[test]
    fn test_compute_pda_seed_order_matters() {
        let program_id: ProgramId = [1u32; 8];
        let a = [0x01u8; 32];
        let b = [0x02u8; 32];

        let ab = compute_pda(&program_id, &[&a, &b]);
        let ba = compute_pda(&program_id, &[&b, &a]);
        assert_ne!(ab, ba, "seed order must matter (non-commutative)");
    }

    #[test]
    fn test_compute_pda_no_self_cancellation() {
        let program_id: ProgramId = [1u32; 8];
        let a = [0xFFu8; 32];

        let single = compute_pda(&program_id, &[&a]);
        let double = compute_pda(&program_id, &[&a, &a]);
        assert_ne!(single, double, "identical seeds must not cancel out");
    }

    #[test]
    fn test_compute_pda_multi_vs_single() {
        let program_id: ProgramId = [1u32; 8];
        let seed = seed_from_str("test");

        let single = compute_pda(&program_id, &[&seed]);
        let multi = compute_pda(&program_id, &[&seed, &[0u8; 32]]);
        assert_ne!(single, multi);
    }

    #[test]
    #[should_panic(expected = "at least one seed")]
    fn test_compute_pda_empty_seeds() {
        let program_id: ProgramId = [1u32; 8];
        compute_pda(&program_id, &[]);
    }
}
