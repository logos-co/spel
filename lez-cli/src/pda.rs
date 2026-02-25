//! PDA (Program Derived Address) computation from IDL seed definitions.

use std::collections::HashMap;
use nssa::AccountId;
use nssa_core::program::{PdaSeed, ProgramId};
use lez_framework_core::idl::IdlSeed;
use crate::parse::ParsedValue;

/// Resolve a single seed to 32 bytes.
fn resolve_seed(
    seed: &IdlSeed,
    program_id: &ProgramId,
    account_map: &HashMap<String, AccountId>,
    parsed_args: &HashMap<String, ParsedValue>,
) -> Result<[u8; 32], String> {
    match seed {
        IdlSeed::Const { value } => {
            let mut bytes = [0u8; 32];
            let src = value.as_bytes();
            if src.len() > 32 {
                return Err(format!("Const seed '{}' exceeds 32 bytes", value));
            }
            bytes[..src.len()].copy_from_slice(src);
            Ok(bytes)
        }
        IdlSeed::Account { path } => {
            let account_id = account_map
                .get(path)
                .ok_or_else(|| {
                    format!(
                        "PDA seed references account '{}' which hasn't been resolved yet",
                        path
                    )
                })?;
            Ok(*account_id.value())
        }
        IdlSeed::Arg { path } => {
            let val = parsed_args
                .get(path)
                .ok_or_else(|| {
                    format!(
                        "PDA seed references arg '{}' which wasn't provided",
                        path
                    )
                })?;
            // Convert ParsedValue to 32 bytes
            match val {
                ParsedValue::ByteArray(b) => {
                    if b.len() != 32 {
                        return Err(format!("Arg '{}' is {} bytes, expected 32", path, b.len()));
                    }
                    let mut bytes = [0u8; 32];
                    bytes.copy_from_slice(b);
                    Ok(bytes)
                }
                ParsedValue::U64(n) => {
                    let mut bytes = [0u8; 32];
                    bytes[24..32].copy_from_slice(&n.to_be_bytes());
                    Ok(bytes)
                }
                ParsedValue::U128(n) => {
                    let mut bytes = [0u8; 32];
                    bytes[16..32].copy_from_slice(&n.to_be_bytes());
                    Ok(bytes)
                }
                ParsedValue::Str(s) => {
                    let mut bytes = [0u8; 32];
                    let src = s.as_bytes();
                    if src.len() > 32 {
                        return Err(format!("String arg '{}' exceeds 32 bytes", path));
                    }
                    bytes[..src.len()].copy_from_slice(src);
                    Ok(bytes)
                }
                _ => Err(format!(
                    "Arg '{}' has unsupported type for PDA seed. Expected bytes32, u64, u128, or string.",
                    path
                )),
            }
        }
    }
}

/// Hash multiple 32-byte seeds via SHA-256(seed1 || seed2 || ...).
///
/// Uses concatenation + SHA-256 (not XOR) to avoid commutativity and
/// self-cancellation issues. Matches the on-chain nssa derivation pattern.
fn hash_seeds(seeds: &[[u8; 32]]) -> [u8; 32] {
    use risc0_zkvm::sha::{Impl, Sha256};
    let mut bytes = Vec::with_capacity(seeds.len() * 32);
    for seed in seeds {
        bytes.extend_from_slice(seed);
    }
    Impl::hash_bytes(&bytes)
        .as_bytes()
        .try_into()
        .expect("SHA-256 output must be exactly 32 bytes")
}

/// Compute PDA AccountId from IDL seed definitions.
///
/// Supports single and multi-seed PDAs:
/// - Single seed: used directly as PDA seed
/// - Multi-seed: SHA-256(seed1 || seed2 || ...) combined into a single 32-byte seed
///
/// Supports all seed types: `const`, `account`, and `arg`.
pub fn compute_pda_from_seeds(
    seeds: &[IdlSeed],
    program_id: &ProgramId,
    account_map: &HashMap<String, AccountId>,
    parsed_args: &HashMap<String, ParsedValue>,
) -> Result<AccountId, String> {
    if seeds.is_empty() {
        return Err("PDA requires at least one seed".to_string());
    }

    // Resolve all seeds to bytes
    let resolved: Vec<[u8; 32]> = seeds
        .iter()
        .map(|s| resolve_seed(s, program_id, account_map, parsed_args))
        .collect::<Result<Vec<_>, _>>()?;

    // Single seed: use directly. Multi-seed: SHA-256(seed1 || seed2 || ...)
    // This avoids XOR commutativity and self-cancellation issues.
    let combined = if resolved.len() == 1 {
        resolved[0]
    } else {
        hash_seeds(&resolved)
    };

    let pda_seed = PdaSeed::new(combined);
    Ok(AccountId::from((program_id, &pda_seed)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_const_seed() {
        let seeds = vec![IdlSeed::Const { value: "test_seed".to_string() }];
        let program_id: ProgramId = [1u32; 8];
        let result = compute_pda_from_seeds(&seeds, &program_id, &HashMap::new(), &HashMap::new());
        assert!(result.is_ok());
    }

    #[test]
    fn test_arg_seed_bytes32() {
        let seeds = vec![
            IdlSeed::Const { value: "multisig_state__".to_string() },
            IdlSeed::Arg { path: "create_key".to_string() },
        ];
        let program_id: ProgramId = [1u32; 8];
        let mut args = HashMap::new();
        args.insert("create_key".to_string(), ParsedValue::ByteArray(vec![42u8; 32]));
        let result = compute_pda_from_seeds(&seeds, &program_id, &HashMap::new(), &args);
        assert!(result.is_ok());
    }

    #[test]
    fn test_arg_seed_u64() {
        let seeds = vec![
            IdlSeed::Const { value: "proposal".to_string() },
            IdlSeed::Arg { path: "index".to_string() },
        ];
        let program_id: ProgramId = [1u32; 8];
        let mut args = HashMap::new();
        args.insert("index".to_string(), ParsedValue::U64(5));
        let result = compute_pda_from_seeds(&seeds, &program_id, &HashMap::new(), &args);
        assert!(result.is_ok());
    }

    #[test]
    fn test_missing_arg_errors() {
        let seeds = vec![IdlSeed::Arg { path: "missing".to_string() }];
        let program_id: ProgramId = [1u32; 8];
        let result = compute_pda_from_seeds(&seeds, &program_id, &HashMap::new(), &HashMap::new());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("missing"));
    }

    #[test]
    fn test_hash_seeds_not_commutative() {
        use risc0_zkvm::sha::{Impl, Sha256};
        // SHA-256(A || B) != SHA-256(B || A) for A != B
        let a = [0x01u8; 32];
        let b = [0x02u8; 32];
        let ab = hash_seeds(&[a, b]);
        let ba = hash_seeds(&[b, a]);
        assert_ne!(ab, ba, "seed order must matter (non-commutative)");
    }

    #[test]
    fn test_hash_seeds_no_self_cancellation() {
        // SHA-256(A || A) != zero
        let a = [0xFFu8; 32];
        let result = hash_seeds(&[a, a]);
        assert_ne!(result, [0u8; 32], "identical seeds must not cancel out");
    }

    #[test]
    fn test_multi_seed_differs_from_single() {
        let seeds_multi = vec![
            IdlSeed::Const { value: "test".to_string() },
            IdlSeed::Arg { path: "key".to_string() },
        ];
        let seeds_single = vec![
            IdlSeed::Const { value: "test".to_string() },
        ];
        let program_id: ProgramId = [1u32; 8];
        let mut args = HashMap::new();
        args.insert("key".to_string(), ParsedValue::ByteArray(vec![0u8; 32]));

        let multi = compute_pda_from_seeds(&seeds_multi, &program_id, &HashMap::new(), &args).unwrap();
        let single = compute_pda_from_seeds(&seeds_single, &program_id, &HashMap::new(), &HashMap::new()).unwrap();

        // Multi-seed SHA-256 must differ from single seed (no zero-cancellation)
        assert_ne!(multi, single);
    }
}
