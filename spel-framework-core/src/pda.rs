//! Generic PDA (Program Derived Address) computation utilities.

use base58::FromBase58;
use nssa_core::account::AccountId;
use nssa_core::encryption::ViewingPublicKey;
use nssa_core::program::{PdaSeed, ProgramId};
use nssa_core::NullifierPublicKey;
use sha2::{Digest, Sha256};

/// Trait for converting a value into a 32-byte PDA seed.
///
/// Provides type-specific conversions that are more predictable than
/// generic Borsh serialization. Each type uses its natural byte
/// representation, zero-padded to 32 bytes.
pub trait ToSeed {
    /// Convert this value into a zero-padded 32-byte seed.
    fn to_seed(&self) -> [u8; 32];
}

impl ToSeed for [u8; 32] {
    fn to_seed(&self) -> [u8; 32] {
        *self
    }
}

impl ToSeed for u64 {
    fn to_seed(&self) -> [u8; 32] {
        let mut seed = [0u8; 32];
        seed[..8].copy_from_slice(&self.to_le_bytes());
        seed
    }
}

impl ToSeed for u32 {
    fn to_seed(&self) -> [u8; 32] {
        let mut seed = [0u8; 32];
        seed[..4].copy_from_slice(&self.to_le_bytes());
        seed
    }
}

impl ToSeed for String {
    fn to_seed(&self) -> [u8; 32] {
        seed_from_str(self)
    }
}

impl ToSeed for &str {
    fn to_seed(&self) -> [u8; 32] {
        seed_from_str(self)
    }
}

/// Convert a string to a zero-padded 32-byte seed.
///
/// # Panics
///
/// Panics if the string is longer than 32 bytes.
pub fn seed_from_str(s: &str) -> [u8; 32] {
    let src = s.as_bytes();
    assert!(src.len() <= 32, "seed string '{s}' exceeds 32 bytes");
    let mut bytes = [0u8; 32];
    bytes[..src.len()].copy_from_slice(src);
    bytes
}

/// Derive a **public** PDA `AccountId` from a program ID and one or more 32-byte seeds.
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
    AccountId::for_public_pda(program_id, &pda_seed)
}

/// Derive a **private** PDA `AccountId` from a program ID, one or more 32-byte seeds,
/// a `NullifierPublicKey`, and a `ViewingPublicKey`.
///
/// The seed combining logic mirrors [`compute_pda`]; the difference is the final
/// derivation calls `AccountId::for_private_pda`, which includes both the `npk` and
/// the `vpk` in the hash so each controller group gets a unique address for the same
/// seed.
///
/// # Panics
///
/// Panics if `seeds` is empty.
pub fn compute_private_pda(
    program_id: &ProgramId,
    seeds: &[&[u8; 32]],
    npk: &NullifierPublicKey,
    vpk: &ViewingPublicKey,
) -> AccountId {
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
    // lez-core-v0.2.0 added a u128 `identifier` to private-PDA derivation; default
    // to 0 (the common case) until callers need to pass a specific identifier.
    AccountId::for_private_pda(program_id, &pda_seed, npk, vpk, 0)
}

/// Compute a PDA from a program ID and multiple [`ToSeed`] values.
///
/// This is a convenience wrapper around [`compute_pda`] that accepts any
/// mix of types implementing `ToSeed` (e.g. `u64`, `u32`, `String`, `[u8; 32]`).
///
/// # Panics
///
/// Panics if `seeds` is empty.
pub fn compute_pda_multi(program_id: &ProgramId, seeds: &[&dyn ToSeed]) -> AccountId {
    let converted: Vec<[u8; 32]> = seeds.iter().map(|s| s.to_seed()).collect();
    let refs: Vec<&[u8; 32]> = converted.iter().collect();
    compute_pda(program_id, &refs)
}

/// Derive a PDA from a program ID and raw byte-slice seeds (variable length, ≤ 32 bytes each).
///
/// Pads each seed to 32 bytes and then delegates to [`compute_pda`]. This is the variant
/// used by generated FFI code where seeds arrive as `&[u8]` rather than `&[u8; 32]`.
pub fn compute_pda_raw(program_id: &ProgramId, seeds: &[&[u8]]) -> Result<AccountId, String> {
    if seeds.is_empty() {
        return Err("PDA requires at least one seed".into());
    }
    let mut arrays: Vec<[u8; 32]> = Vec::with_capacity(seeds.len());
    for seed in seeds {
        let len = seed.len();
        if len > 32 {
            return Err(format!("PDA seed exceeds 32 bytes ({len})"));
        }
        let mut padded = [0u8; 32];
        padded[..len].copy_from_slice(seed);
        arrays.push(padded);
    }
    let refs: Vec<&[u8; 32]> = arrays.iter().collect();
    Ok(compute_pda(program_id, &refs))
}

/// Decode a 32-byte value from a base58 or hex string.
///
/// Accepts (in priority order):
/// - `Public/<value>` or `Private/<value>` — strips the prefix, then processes `<value>`
/// - `0x<hex>` or `0X<hex>` — 64 hex characters (explicit hex marker, checked first)
/// - `<hex>` — exactly 64 hex characters (tried before base58 to avoid ambiguity)
/// - `<base58>` — base58-encoded 32-byte value (~44 chars)
///
/// Returns an error if the string does not match any of the above or decodes to a length other than 32.
pub fn parse_bytes32(input: &str) -> Result<[u8; 32], String> {
    let s = input
        .strip_prefix("Public/")
        .or_else(|| input.strip_prefix("Private/"))
        .unwrap_or(input);

    let hex_part = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    if hex_part.len() == 64 {
        let mut arr = [0u8; 32];
        let mut ok = true;
        for (i, chunk) in hex_part.as_bytes().chunks(2).enumerate() {
            if let Ok(byte_str) = std::str::from_utf8(chunk) {
                if let Ok(b) = u8::from_str_radix(byte_str, 16) {
                    arr[i] = b;
                    continue;
                }
            }
            ok = false;
            break;
        }
        if ok {
            return Ok(arr);
        }
    }

    if let Ok(bytes) = s.from_base58() {
        if bytes.len() == 32 {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes);
            return Ok(arr);
        }
        return Err(format!(
            "base58 decoded to {} bytes, expected 32",
            bytes.len()
        ));
    }

    Err(format!(
        "expected 32 bytes (64 hex chars or base58-encoded key), got: {input}"
    ))
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

    // ── ToSeed trait tests ──────────────────────────────────────────

    #[test]
    fn test_to_seed_u8_32_identity() {
        let val = [42u8; 32];
        assert_eq!(val.to_seed(), val);
    }

    #[test]
    fn test_to_seed_u64() {
        let val: u64 = 0x0102030405060708;
        let seed = val.to_seed();
        assert_eq!(&seed[..8], &val.to_le_bytes());
        assert_eq!(&seed[8..], &[0u8; 24]);
    }

    #[test]
    fn test_to_seed_u32() {
        let val: u32 = 0x01020304;
        let seed = val.to_seed();
        assert_eq!(&seed[..4], &val.to_le_bytes());
        assert_eq!(&seed[4..], &[0u8; 28]);
    }

    #[test]
    fn test_to_seed_string() {
        let val = String::from("hello");
        let seed = val.to_seed();
        assert_eq!(&seed[..5], b"hello");
        assert_eq!(&seed[5..], &[0u8; 27]);
    }

    #[test]
    fn test_to_seed_str() {
        let seed = "hello".to_seed();
        assert_eq!(&seed[..5], b"hello");
        assert_eq!(&seed[5..], &[0u8; 27]);
    }

    #[test]
    fn test_to_seed_string_matches_seed_from_str() {
        let s = "vault_prefix";
        assert_eq!(s.to_seed(), seed_from_str(s));
        assert_eq!(String::from(s).to_seed(), seed_from_str(s));
    }

    // ── compute_pda_multi tests ─────────────────────────────────────

    #[test]
    fn test_compute_pda_multi_matches_compute_pda() {
        let program_id: ProgramId = [1u32; 8];
        let seed1 = seed_from_str("config");
        let seed2 = [99u8; 32];

        let from_compute = compute_pda(&program_id, &[&seed1, &seed2]);
        let from_multi = compute_pda_multi(&program_id, &[&seed1, &seed2]);
        assert_eq!(from_compute, from_multi);
    }

    #[test]
    fn test_compute_pda_multi_mixed_types() {
        let program_id: ProgramId = [1u32; 8];
        let id: u64 = 42;
        let label = String::from("vault");

        let pda = compute_pda_multi(&program_id, &[&label, &id]);

        // Verify it matches manual computation
        let seed1 = label.to_seed();
        let seed2 = id.to_seed();
        let expected = compute_pda(&program_id, &[&seed1, &seed2]);
        assert_eq!(pda, expected);
    }

    #[test]
    fn test_compute_pda_multi_single_u64() {
        let program_id: ProgramId = [1u32; 8];
        let val: u64 = 1000;
        let pda = compute_pda_multi(&program_id, &[&val]);

        let seed = val.to_seed();
        let expected = compute_pda(&program_id, &[&seed]);
        assert_eq!(pda, expected);
    }

    #[test]
    fn test_compute_pda_multi_three_seeds() {
        let program_id: ProgramId = [1u32; 8];
        let prefix = "order";
        let user_id: u64 = 7;
        let seq: u32 = 100;

        let pda = compute_pda_multi(&program_id, &[&prefix, &user_id, &seq]);

        let s1 = prefix.to_seed();
        let s2 = user_id.to_seed();
        let s3 = seq.to_seed();
        let expected = compute_pda(&program_id, &[&s1, &s2, &s3]);
        assert_eq!(pda, expected);
    }

    // ── compute_pda_raw tests ───────────────────────────────────────

    #[test]
    fn test_compute_pda_raw_matches_compute_pda() {
        let program_id: ProgramId = [1u32; 8];
        let seed = seed_from_str("test");
        let expected = compute_pda(&program_id, &[&seed]);
        let raw = compute_pda_raw(&program_id, &[b"test"]).unwrap();
        assert_eq!(raw, expected);
    }

    #[test]
    fn test_compute_pda_raw_multi_seed() {
        let program_id: ProgramId = [3u32; 8];
        let s1 = seed_from_str("prefix");
        let s2 = [0xabu8; 32];
        let expected = compute_pda(&program_id, &[&s1, &s2]);
        let raw = compute_pda_raw(&program_id, &[b"prefix", &[0xabu8; 32]]).unwrap();
        assert_eq!(raw, expected);
    }

    #[test]
    fn test_compute_pda_raw_empty_returns_err() {
        let program_id: ProgramId = [1u32; 8];
        assert!(compute_pda_raw(&program_id, &[]).is_err());
    }

    #[test]
    fn test_compute_pda_raw_seed_too_long() {
        let program_id: ProgramId = [1u32; 8];
        let long = [0u8; 33];
        assert!(compute_pda_raw(&program_id, &[&long]).is_err());
    }

    // ── parse_bytes32 tests ─────────────────────────────────────────

    #[test]
    fn test_parse_bytes32_hex_with_0x() {
        let input = format!("0x{}", "ab".repeat(32));
        let result = parse_bytes32(&input).unwrap();
        assert_eq!(result, [0xab; 32]);
    }

    #[test]
    fn test_parse_bytes32_hex_without_0x() {
        let input = "ab".repeat(32);
        let result = parse_bytes32(&input).unwrap();
        assert_eq!(result, [0xab; 32]);
    }

    #[test]
    fn test_parse_bytes32_public_prefix_stripped() {
        let hex = format!("0x{}", "cd".repeat(32));
        let with_prefix = format!("Public/{}", hex);
        assert_eq!(parse_bytes32(&with_prefix).unwrap(), [0xcdu8; 32]);
    }

    #[test]
    fn test_parse_bytes32_private_prefix_stripped() {
        let hex = "ef".repeat(32);
        let with_prefix = format!("Private/{}", hex);
        assert_eq!(parse_bytes32(&with_prefix).unwrap(), [0xefu8; 32]);
    }

    #[test]
    fn test_parse_bytes32_wrong_length() {
        assert!(parse_bytes32("deadbeef").is_err());
    }
}
