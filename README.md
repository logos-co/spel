# lez-framework

[![CI](https://github.com/jimmy-claw/lez-framework/actions/workflows/ci.yml/badge.svg)](https://github.com/jimmy-claw/lez-framework/actions/workflows/ci.yml)

Developer framework for building LEZ programs — inspired by [Anchor](https://www.anchor-lang.com/) for Solana.

Write your program logic with proc macros. Get IDL generation, a full CLI with TX submission, and project scaffolding for free.

## Quick Start

### Scaffold a new project

```bash
cargo install --path lez-cli
lez-cli init my-program
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
use lez_framework::prelude::*;

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
    ) -> LezResult {
        // Your logic here
        Ok(LezOutput::states_only(vec![
            AccountPostState::new_claimed(state.account.clone()),
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
    ) -> LezResult {
        // Your logic here
        Ok(LezOutput::states_only(vec![
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

### Runtime Validation

Accounts marked with `#[account(signer)]` or `#[account(init)]` get **automatic runtime checks** before your handler runs:

- **Signer**: Verifies `is_authorized` is true, returns `LezError::Unauthorized` if not
- **Init**: Verifies account is in default state, returns `LezError::AccountAlreadyInitialized` if not

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
    lez_cli::run().await;
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

### IDL Generation

The IDL generator is also a one-liner:

```rust
lez_framework::generate_idl!("../methods/guest/src/bin/my_program.rs");
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
lez-cli init my-program

# Inspect program binaries (no --idl needed)
lez-cli inspect program.bin

# Show available commands
lez-cli --idl program-idl.json --help

# Dry run an instruction
lez-cli --idl program-idl.json --dry-run -p program.bin \
  create-vault --token-name "MYTKN" --initial-supply 1000000

# Submit a transaction
lez-cli --idl program-idl.json -p program.bin \
  create-vault --token-name "MYTKN" --initial-supply 1000000

# Use --program-id instead of binary (skips loading the file)
lez-cli --idl program-idl.json --program-id <64-char-hex>   create-vault --token-name "MYTKN" --initial-supply 1000000

# Compute a PDA from the IDL
lez-cli --idl program-idl.json --program-id <64-char-hex> pda vault --create-key my-multisig

# Auto-fill program IDs from binaries
lez-cli --idl program-idl.json -p treasury.bin --bin-token token.bin \
  create-vault --token-name "MYTKN" --initial-supply 1000000

# Get help for a specific instruction
lez-cli --idl program-idl.json create-vault --help
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

## Crates

| Crate | Description |
|-------|-------------|
| `lez-framework` | Umbrella crate — re-exports macros + core with a prelude |
| `lez-framework-core` | IDL types, error types, `LezOutput` |
| `lez-framework-macros` | Proc macros: `#[lez_program]`, `#[instruction]`, `generate_idl!` |
| `lez-cli` | Generic IDL-driven CLI with TX submission + project scaffolding |
| `lez-client-gen` | Code generator — produces typed Rust FFI clients from IDL JSON |

## License

MIT
