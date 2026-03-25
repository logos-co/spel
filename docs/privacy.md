# Privacy-Preserving Programs with SPEL

SPEL programs are **privacy-agnostic** — the same program code works identically with both public and private accounts. Privacy is handled at the transaction layer, not the program layer.

## How LEZ Privacy Works

LEZ uses a commitment/nullifier scheme:

- **Private accounts** are owned by the `auth-transfer` program and encrypted on-chain
- **Commitments** hide account state in a Merkle tree
- **Nullifiers** prove an account was spent without revealing which one
- **ZK proofs** (RISC0) verify execution correctness without revealing private data

The sequencer never sees plaintext private account state — only commitments, nullifiers, and ZK proofs.

## Using Private Accounts with SPEL

### 1. Create a private account

```bash
wallet account new private
# → Private/5jH7h9CfRDcbfZxCs7h93PcuL1ESW5EJxWbntBup2tJ8

wallet auth-transfer init --account-id Private/<id>
wallet account sync-private
```

### 2. Call any SPEL instruction with a private account

Simply pass the `Private/` prefixed account ID — `spel` detects it automatically and builds a `PrivacyPreservingTransaction`:

```bash
spel --idl my-program-idl.json -p my-program.bin \
  my_instruction \
  --owner Private/5jH7h9Cf...
```

That's it. The program logic doesn't change.

### 3. Verify the data was written

```bash
wallet account sync-private
wallet account get --account-id Private/<id>
# → {"balance": 0, "data_b64": "SGVsbG8h", ...}
```

The `data_b64` field contains the base64-encoded private data, decrypted by your wallet.

## What the Sequencer Sees

For a `PrivacyPreservingTransaction`:

| Field | Value |
|-------|-------|
| Account states | Encrypted ciphertext |
| New commitments | Merkle tree insertions |
| Spent nullifiers | Prevents replay |
| ZK proof | RISC0 receipt |

The sequencer verifies the proof but never sees plaintext account data.

## Privacy Transaction Types

| Account prefix | Transaction type | ZK proof |
|---------------|-----------------|----------|
| `Public/` | `PublicTransaction` | Signature |
| `Private/` | `PrivacyPreservingTransaction` | RISC0 receipt |
| Mixed | `PrivacyPreservingTransaction` | RISC0 receipt |

## Writing Privacy-Compatible SPEL Programs

No special annotations needed. A simple program works with both:

```rust
#[lez_program]
mod my_program {
    #[instruction]
    pub fn store_data(
        #[account(mut)]
        target: AccountWithMetadata,   // works as Public/ or Private/
        data: Vec<u8>,
    ) -> LezResult {
        let mut account = target.account.clone();
        account.data = data.try_into()?;
        Ok(LezOutput::states_only(vec![
            AccountPostState::new(account),
        ]))
    }
}
```

## Private Account Lifecycle

```
wallet account new private          # create keypair, derive NPK/NSK
wallet auth-transfer init           # register commitment on-chain
wallet account sync-private         # sync Merkle tree state
spel ... --account Private/<id>     # use in any SPEL instruction
wallet account sync-private         # sync updated state
wallet account get --account-id ... # read decrypted data
```

## IDL Privacy Metadata (optional)

You can mark accounts as intended for private use in the IDL:

```rust
#[instruction(execution = { private_owned: true })]
pub fn private_only_instruction(...) -> LezResult
```

This is informational — it signals to tooling that this instruction expects private accounts. The program logic remains the same.

## Related

- [LEZ Privacy Technical Deep Dive](lez/lez-privacy-technical-deep-dive.md)
- [Private Multisig (LP-0002)](lez/lp-0002-rfc.md)
- [SPEL PR #83](https://github.com/logos-co/spel/pull/83) — `Private/` prefix auto-detection
