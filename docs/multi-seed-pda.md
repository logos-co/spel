# Multi-seed and arg-based PDA derivation

GitHub Issue: https://github.com/jimmy-claw/lez-framework/issues/1

## Problem

Currently PDAs support only a single 32-byte seed via:
- `pda = literal("string")` — constant UTF-8 string, right-padded to 32 bytes
- `pda = account("name")` — another account's 32-byte ID

This covers basic cases but many real programs need **dynamic, multi-part seeds** — e.g. a per-user vault, a per-token-per-user holding, etc.

## Anchor Comparison

Anchor supports arbitrary seed combinations:
```rust
#[account(seeds = [b"vault", user.key().as_ref(), &[vault_id]], bump)]
```

## Proposed Syntax

```rust
// Single seeds (current — keep working)
#[account(pda = literal("state"))]
#[account(pda = account("user"))]

// Multi-seed (new)
#[account(pda = [literal("vault"), account("user")])]
#[account(pda = [literal("holding"), account("token"), account("user")])]

// Arg-based seeds (new)
#[account(pda = arg("token_name"))]
#[account(pda = [literal("vault"), arg("vault_id")])]
```

## Implementation

Since LSSA's `PdaSeed` is a single `[u8; 32]`, multi-part seeds need to be combined:

```
combined_seed = sha256(part1 || part2 || ... || partN)  →  [u8; 32]
pda = AccountId::from((program_id, &PdaSeed::new(combined_seed)))
```

### Changes needed:

1. **Macro** (`lez-framework-macros`): Parse array syntax `pda = [...]`, support `arg("name")`
2. **IDL** (`lez-framework-core`): `IdlSeed` already has `Const`, `Account`, `Arg` variants — just need to handle multiple seeds
3. **CLI** (`lez-cli`): `compute_pda_from_seeds` already accepts `&[IdlSeed]` — implement multi-seed hashing
4. **Guest code generation**: Macro-generated `main()` needs to compute multi-seed PDAs at runtime

### Seed types:

| Seed | Source | Value |
|------|--------|-------|
| `literal("x")` | Constant | UTF-8 bytes of "x" |
| `account("name")` | Transaction account | 32-byte account ID |
| `arg("name")` | Instruction argument | Serialized bytes of the arg value |

## Backwards Compatibility

Single-seed syntax remains unchanged. Multi-seed is additive.
