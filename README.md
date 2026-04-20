# spel-framework

[![CI](https://github.com/logos-co/spel/actions/workflows/ci.yml/badge.svg)](https://github.com/logos-co/spel/actions/workflows/ci.yml)

Developer framework for building SPEL programs — inspired by [Anchor](https://www.anchor-lang.com/) for Solana.

Write your program logic with proc macros. Get IDL generation, a full CLI with TX submission, and project scaffolding for free.

## Quick Start

### Scaffold a new project

```bash
cargo install --path spel-cli  # installs as "spel"
spel init my-program
cd my-program
```

This generates a complete project:

```
my-program/
├── Cargo.toml                 # Workspace
├── Makefile                   # build, idl, cli, deploy, inspect, setup
├── README.md
├── my_program_core/           # Shared types (guest + host)
│   └── src/lib.rs
├── methods/
│   └── guest/                 # RISC Zero guest (runs on-chain)
│       └── src/bin/my_program.rs
└── examples/
    └── src/bin/
        ├── generate_idl.rs    # One-liner IDL generator
        └── my_program_cli.rs  # Three-line CLI wrapper
```

### Build → Deploy → Transact

```bash
make build        # Build the guest binary (risc0)
make idl          # Generate IDL from #[lez_program] annotations
make deploy       # Deploy to sequencer
make cli ARGS="--help"   # See auto-generated commands
make cli ARGS="-p <binary> initialize --owner-account <BASE58>"
```

## Writing Programs

```rust
#![no_main]

use nssa_core::account::AccountWithMetadata;
use nssa_core::program::AccountPostState;
use spel_framework::prelude::*;

risc0_zkvm::guest::entry!(main);

#[lez_program]
mod my_program {
    #[allow(unused_imports)]
    use super::*;

    #[instruction]
    pub fn initialize(
        #[account(init, pda = literal("state"))]
        state: AccountWithMetadata,
        #[account(signer)]
        owner: AccountWithMetadata,
    ) -> SpelResult {
        // Your logic here
        Ok(SpelOutput::states_only(vec![
            AccountPostState::new_claimed(state.account.clone(), Claim::Authorized),
            AccountPostState::new(owner.account.clone()),
        ]))
    }

    #[instruction]
    pub fn transfer(
        #[account(mut, pda = literal("state"))]
        state: AccountWithMetadata,
        recipient: AccountWithMetadata,
        #[account(signer)]
        sender: AccountWithMetadata,
        amount: u128,
    ) -> SpelResult {
        // Your logic here
        Ok(SpelOutput::states_only(vec![
            AccountPostState::new(state.account.clone()),
            AccountPostState::new(recipient.account.clone()),
            AccountPostState::new(sender.account.clone()),
        ]))
    }
}
```

### Account Attributes

| Attribute | Description |
|-----------|-------------|
| `#[account(mut)]` | Account is writable |
| `#[account(init)]` | Account is being created (use `new_claimed`) |
| `#[account(signer)]` | Account must sign the transaction |
| `#[account(pda = literal("seed"))]` | PDA derived from a constant string |
| `#[account(pda = account("other"))]` | PDA derived from another account's ID |
| `#[account(pda = arg("create_key"))]` | PDA derived from an instruction argument |
| `members: Vec<AccountWithMetadata>` | Variable-length trailing account list |

### PDA Seed Display

When the CLI derives PDA accounts during transaction execution, it prints the seed inputs used for each derivation:

```
  PDA vault → 4Lp3gkH...
    seeds: [program_id, "state"]
  PDA token_account → 7xQ2m...
    seeds: [program_id, Account(owner), Arg(create_key)]
```

Seeds always start with `program_id`, followed by the seeds declared in the account attribute. Constant strings appear quoted, account references as `Account(name)`, and instruction arguments as `Arg(name)`.

### Runtime Validation

Accounts marked with `#[account(signer)]` or `#[account(init)]` get **automatic runtime checks** before your handler runs:

- **Signer**: Verifies `is_authorized` is true, returns `SpelError::Unauthorized` if not
- **Init**: Verifies account is in default state, returns `SpelError::AccountAlreadyInitialized` if not

No manual checking needed in your instruction handlers.

### External Instruction Enum

If your `Instruction` enum lives in a shared core crate (used by both on-chain program and CLI), you can tell the macro to use it instead of generating one:

```rust
#[lez_program(instruction = "my_core::Instruction")]
mod my_program {
    // ...
}
```

### The CLI Wrapper

Every program gets a full CLI for free. The wrapper is just:

```rust
#[tokio::main]
async fn main() {
    spel_cli::run().await;
}
```

This provides:
- Auto-generated subcommands from IDL instructions
- Type-aware argument parsing (u128, [u8; N], base58 accounts, ProgramId, etc.)
- Automatic PDA computation from IDL seeds
- risc0-compatible serialization
- Transaction building and submission with wallet integration
- `--dry-run` mode for testing
- `inspect` subcommand to extract ProgramId from binaries

### Account Types

Types that represent on-chain account data can be annotated with `#[account_type]`. This causes them to appear in the generated IDL so `spel inspect` can decode raw account bytes into readable JSON.

```rust
use spel_framework::prelude::*;

#[account_type]
#[derive(BorshSerialize, BorshDeserialize)]
pub struct VaultState {
    pub owner: AccountId,
    pub balance: u128,
    pub locked: bool,
}

#[account_type]
#[derive(BorshSerialize, BorshDeserialize)]
pub enum TokenHolding {
    Fungible { definition_id: AccountId, balance: u128 },
    NftMaster { definition_id: AccountId, print_balance: u128 },
}
```

Types referenced by an `#[account_type]` (such as helper enums or nested structs) are collected automatically — they do not need their own annotation:

```rust
// No annotation needed — picked up automatically because VaultState references it
#[derive(BorshSerialize, BorshDeserialize)]
pub enum VaultStatus { Active, Frozen }
```

The IDL generator embeds all annotated types in the `accounts` array and all transitively referenced helper types in the `types` array of the generated JSON. No file paths or external references — the IDL is fully self-contained.

### IDL Generation

The IDL generator is also a one-liner:

```rust
spel_framework::generate_idl!("../methods/guest/src/bin/my_program.rs");
```

It reads the `#[lez_program]` annotations at compile time and generates a complete JSON IDL describing instructions, arguments, accounts, and PDA seeds.

#### LSSA-lang compatible fields

The generated IDL is a superset of the lssa-lang IDL spec. In addition to our core fields, each instruction includes:

- **discriminator** -- SHA256 of global:name, first 8 bytes, matching lssa-lang convention
- **execution** -- public/private_owned flags (default: public execution)
- **variant** -- PascalCase variant name

Each account field includes:

- **visibility** -- list of visibility tags (default: public)

These fields are optional and backward-compatible -- existing IDL consumers that do not know about them will simply ignore them.

## CLI Usage

```bash
# Scaffold a new project (no --idl needed)
spel init my-program

# Inspect program binaries (no --idl needed)
spel inspect program.bin

# Generate IDL from a program source file (includes all #[account_type] definitions)
spel generate-idl methods/guest/src/bin/my_program.rs > my_program-idl.json

# Decode on-chain account data using a type from the IDL
spel inspect <account-id> --idl my_program-idl.json --type VaultState

# Same, but supply raw borsh bytes directly instead of fetching from the network
spel inspect <account-id> --idl my_program-idl.json --type VaultState --data <borsh-hex>

# Show available commands
spel --idl program-idl.json --help

# Dry run an instruction — resolve everything (PDAs, accounts, serialized data,
# signer nonces) and print without submitting. Accepts --dry-run (text default),
# --dry-run=text, or --dry-run=json.
spel --idl program-idl.json --dry-run -p program.bin -- \
  create-vault --token-name "MYTKN" --initial-supply 1000000

# Machine-readable dry run for scripting / golden tests
spel --idl program-idl.json --dry-run=json -p program.bin -- \
  create-vault --token-name "MYTKN" --initial-supply 1000000 | jq .

# Submit a transaction
spel --idl program-idl.json -p program.bin -- \
  create-vault --token-name "MYTKN" --initial-supply 1000000

# Use --program-id instead of binary (skips loading the file)
spel --idl program-idl.json --program-id <64-char-hex>   create-vault --token-name "MYTKN" --initial-supply 1000000

# Compute a PDA from the IDL
spel --idl program-idl.json --program-id <64-char-hex> pda vault --create-key my-multisig

# PDA derivation output shows seed inputs:
#   PDA vault → 4Lp3gkH...
#     seeds: [program_id, "state"]

# Auto-fill program IDs from binaries
spel --idl program-idl.json -p treasury.bin --bin-token token.bin \
  create-vault --token-name "MYTKN" --initial-supply 1000000

# Get help for a specific instruction
spel --idl program-idl.json create-vault --help
```

### Type Formats

| IDL Type | CLI Format |
|----------|------------|
| `u8`, `u32`, `u64`, `u128` | Decimal number |
| `[u8; N]` | Hex string (2×N chars) or UTF-8 string (≤N chars, right-padded) |
| `[u32; 8]` / `program_id` | Comma-separated u32s: `"0,0,0,0,0,0,0,0"` |
| `Vec<u8>` | Comma-separated decimal bytes: `"0,1,2"` |
| `Vec<u32>` | Comma-separated decimal u32s: `"0,200,0,0,0"` |
| `Vec<[u8; 32]>` | Comma-separated hex or base58: `"addr1,addr2"` |
| `rest` accounts | Comma-separated base58/hex: `--foo-account "addr1,addr2"` |
| `Option<T>` | Value or `"none"` |
| Account IDs | Base58 or 64-char hex |

### Inspecting Account Data

Once types are annotated with `#[account_type]` and the IDL is generated, you can decode any on-chain account into JSON:

```bash
# Generate the IDL (embeds all annotated account types)
spel generate-idl methods/guest/src/bin/token.rs > token-idl.json

# Fetch and decode a live account from the network
spel inspect 3f2a...bc01 --idl token-idl.json --type TokenHolding
```

```
Account: 3f2a...bc01
Data:    33 bytes
Hex:     01aabbccdd...

{
  "NftMaster": {
    "definition_id": "aabbccddee...",
    "print_balance": "99"
  }
}
```

For accounts with nested types (e.g. `TokenMetadata` referencing `MetadataStandard`), the IDL contains both and decoding works transparently:

```bash
spel inspect 9d1c...f4 --idl token-idl.json --type TokenMetadata
```

```json
{
  "definition_id": "aabbccddee...",
  "standard": "Simple",
  "uri": "https://example.com/metadata.json",
  "creators": "Alice",
  "primary_sale_date": "1720000000"
}
```

You can also pass raw borsh bytes directly with `--data` to decode without a network connection — useful during development and testing:

```bash
spel inspect 0000...0000 \
  --idl token-idl.json \
  --type TokenHolding \
  --data 00<32-byte-definition-id-hex>00000000000000000000000000000064
```

## Crates

| Crate | Description |
|-------|-------------|
| `spel-framework` | Umbrella crate — re-exports macros + core with a prelude |
| `spel-framework-core` | IDL types, error types, `SpelOutput` |
| `spel-framework-macros` | Proc macros: `#[lez_program]`, `#[instruction]`, `generate_idl!` |
| `spel` | Generic IDL-driven CLI with TX submission + project scaffolding |
| `spel-client-gen` | Code generator — produces typed Rust FFI clients from IDL JSON |

## License

MIT
