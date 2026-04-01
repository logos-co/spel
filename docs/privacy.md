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
| `0x...` (public) | `PublicTransaction` | Signature |
| `Private/...` | `PrivacyPreservingTransaction` | RISC0 receipt |
| Mixed | `PrivacyPreservingTransaction` | RISC0 receipt |

## Writing Privacy-Compatible SPEL Programs

No special annotations needed. A simple program works with both:

```rust
#[lez_program]
mod my_program {
    #[instruction]
    pub fn store_data(
        #[account(mut)]
        target: AccountWithMetadata,   // works with public or Private/ accounts
        data: Vec<u8>,
    ) -> SpelResult {
        let mut account = target.account.clone();
        account.data = Data::try_from(data)
            .map_err(|_| SpelError::custom(999, "data too large"))?;
        Ok(SpelOutput::states_only(vec![
            AccountPostState::new(account),
        ]))
    }
}
```

> **Note:** Programs can only write data to accounts they own. For auth-transfer owned accounts
> (freshly initialized private accounts), the program can read but not modify data until the
> account is claimed by the program.

## Private Account Lifecycle

```
wallet account new private          # create keypair, derive NPK/NSK
wallet auth-transfer init           # register commitment on-chain
wallet account sync-private         # sync Merkle tree state
spel ... --account Private/<id>     # use in any SPEL instruction
wallet account sync-private         # sync updated state
wallet account get --account-id ... # read decrypted data
```
