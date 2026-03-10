# Quickstart: Scaffold to Deploy

Full workflow for creating, building, deploying, and interacting with a LEZ program using SPEL.

---

## 1. Scaffold

```bash
lez-cli init my-program
cd my-program
```

Generated structure:

```
my-program/
├── Cargo.toml                          # workspace
├── Makefile                            # build, idl, cli, deploy, inspect, setup targets
├── my_program_core/src/lib.rs          # shared types
├── methods/guest/src/bin/my_program.rs # on-chain guest binary
├── examples/src/bin/
│   ├── generate_idl.rs                 # IDL generator (one-liner macro)
│   └── my_program_cli.rs              # CLI wrapper (three lines)
└── methods/build.rs
```

## 2. Define State

Edit `my_program_core/src/lib.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MyState {
    pub value: u64,
    pub owner: [u8; 32],
}
```

State structs live in the `_core` crate so they can be shared between on-chain and off-chain code.

## 3. Write Instructions

Edit `methods/guest/src/bin/my_program.rs`:

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
        let data = borsh::to_vec(&my_program_core::MyState {
            value: 0,
            owner: *owner.account_id.value(),
        }).map_err(|e| LezError::SerializationError { message: e.to_string() })?;

        let mut new_account = state.account.clone();
        new_account.data = data.try_into().unwrap();

        Ok(LezOutput::states_only(vec![
            AccountPostState::new_claimed(new_account),
            AccountPostState::new(owner.account.clone()),
        ]))
    }

    #[instruction]
    pub fn update(
        #[account(mut, pda = literal("state"))]
        state: AccountWithMetadata,
        #[account(signer)]
        owner: AccountWithMetadata,
        new_value: u64,
    ) -> LezResult {
        let mut current: my_program_core::MyState =
            borsh::from_slice(&state.account.data)
                .map_err(|e| LezError::DeserializationError {
                    account_index: 0, message: e.to_string(),
                })?;

        if *owner.account_id.value() != current.owner {
            return Err(LezError::Unauthorized {
                message: "Only the owner can update".to_string(),
            });
        }

        current.value = new_value;
        let data = borsh::to_vec(&current)
            .map_err(|e| LezError::SerializationError { message: e.to_string() })?;
        let mut updated = state.account.clone();
        updated.data = data.try_into().unwrap();

        Ok(LezOutput::states_only(vec![
            AccountPostState::new(updated),
            AccountPostState::new(owner.account.clone()),
        ]))
    }
}
```

## 4. Set Up IDL Generator

`examples/src/bin/generate_idl.rs` (scaffold creates this):

```rust
lez_framework::generate_idl!("../methods/guest/src/bin/my_program.rs");
```

Path is relative to `CARGO_MANIFEST_DIR` (the `examples/` crate).

## 5. Set Up CLI Wrapper

`examples/src/bin/my_program_cli.rs` (scaffold creates this):

```rust
#[tokio::main]
async fn main() {
    lez_cli::run().await;
}
```

## 6. Build

```bash
make build    # compiles RISC Zero zkVM guest binary
```

## 7. Generate IDL

```bash
make idl      # runs cargo run --bin generate_idl > my-program-idl.json
```

## 8. Deploy

```bash
make setup    # create signer account in wallet
make deploy   # deploy binary to sequencer
make inspect  # print ProgramId (decimal, hex, ImageID)
```

Save the 64-char hex ImageID from `make inspect` output.

## 9. Call Instructions

```bash
# See available commands
make cli ARGS="--help"

# Initialize (PDA accounts auto-computed, not passed as args)
make cli ARGS="-p methods/guest/target/riscv32im-risc0-zkvm-elf/docker/my_program.bin \
  initialize --owner-account <SIGNER_BASE58>"

# Update with argument
make cli ARGS="-p methods/guest/target/riscv32im-risc0-zkvm-elf/docker/my_program.bin \
  update --new-value 42 --owner-account <SIGNER_BASE58>"

# Use --program-id to skip binary loading
make cli ARGS="--program-id <64-CHAR-HEX> \
  update --new-value 100 --owner-account <SIGNER_BASE58>"

# Dry run (no submission)
make cli ARGS="--dry-run -p methods/guest/target/...bin update --new-value 5 --owner-account <ADDR>"

# Compute PDA manually
make cli ARGS="--program-id <64-CHAR-HEX> pda state"
```

## 10. Generate Client Code (optional)

```bash
lez-client-gen --idl my-program-idl.json --out-dir generated/
```

Produces:
- `my_program_client.rs` — typed async Rust client
- `my_program_ffi.rs` — C FFI (`extern "C"` functions accepting JSON)
- `my_program.h` — C header

Build as shared library:

```bash
cargo build --release --lib
# Produces libmy_program.so / libmy_program.dylib
```

## 11. Register in SPELbook (optional)

Register the deployed program in SPELbook to make it discoverable. (Process TBD.)

## Makefile Targets Reference

| Target | Description |
|--------|-------------|
| `make build` | Compile guest binary for RISC Zero zkVM |
| `make idl` | Generate IDL JSON from program source |
| `make cli ARGS="..."` | Run the IDL-driven CLI with given arguments |
| `make deploy` | Deploy program binary to sequencer |
| `make setup` | Create signer account in wallet |
| `make inspect` | Print ProgramId for the compiled binary |
| `make status` | Check deployment status |
| `make clean` | Clean build artifacts |
