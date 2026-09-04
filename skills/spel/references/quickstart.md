# Quickstart: Scaffold to Deploy

Full workflow for creating, building, deploying, and interacting with a LEZ program using SPEL.

---

## 1. Scaffold

```bash
spel init my-program
cd my-program
```

Generated structure:

```
my-program/
├── Cargo.toml                          # workspace
├── Makefile                            # build, idl, cli, deploy, inspect, setup targets
├── spel.toml                           # [program] config — `spel` auto-discovers idl/binary
├── my_program_core/src/lib.rs          # shared types
├── methods/guest/src/bin/my_program.rs # on-chain guest binary
├── examples/src/bin/
│   ├── generate_idl.rs                 # IDL generator (one-liner macro)
│   └── my_program_cli.rs              # CLI wrapper (three lines)
└── methods/build.rs
```

## 2. Define State

Put state structs in `methods/guest/src/bin/my_program.rs` directly, annotated with `#[account_type]` at file top level — see the next step. The `_core` crate is optional and only needed when a type must be consumed by off-chain code (e.g. an external `Instruction` enum for an FFI client).

## 3. Write Instructions

Edit `methods/guest/src/bin/my_program.rs`:

```rust
#![no_main]

use spel_framework::prelude::*;

risc0_zkvm::guest::entry!(main);

/// State stored on-chain. `#[account_type]` MUST live at file top-level
/// (not inside the #[lez_program] module) so the IDL generator picks it up.
#[account_type]
#[derive(Debug, Clone, Default, BorshSerialize, BorshDeserialize)]
pub struct MyState {
    pub value: u64,
    pub owner: [u8; 32],
}

#[lez_program]
mod my_program {
    #[allow(unused_imports)]
    use super::*;

    #[instruction]
    pub fn initialize(
        #[account(init, pda = literal("state"))]
        mut state: AccountWithMetadata,
        #[account(signer)]
        owner: AccountWithMetadata,
    ) -> SpelResult {
        let data = borsh::to_vec(&MyState {
            value: 0,
            owner: *owner.account_id.value(),
        })
        .map_err(|e| SpelError::SerializationError { message: e.to_string() })?;
        state.account.data = data.try_into().unwrap();

        Ok(SpelOutput::execute(vec![state, owner], vec![]))
    }

    #[instruction]
    pub fn update(
        #[account(mut, pda = literal("state"))]
        mut state: AccountWithMetadata,
        #[account(signer)]
        owner: AccountWithMetadata,
        new_value: u64,
    ) -> SpelResult {
        let data: Vec<u8> = state.account.data.clone().into();
        let mut current: MyState = borsh::from_slice(&data)
            .map_err(|e| SpelError::DeserializationError {
                account_index: 0,
                message: e.to_string(),
            })?;

        if *owner.account_id.value() != current.owner {
            return Err(SpelError::Unauthorized {
                message: "Only the owner can update".to_string(),
            });
        }

        current.value = new_value;
        let data = borsh::to_vec(&current)
            .map_err(|e| SpelError::SerializationError { message: e.to_string() })?;
        state.account.data = data.try_into().unwrap();

        Ok(SpelOutput::execute(vec![state, owner], vec![]))
    }
}
```

The `#[lez_program]` macro reads the `#[account(…)]` attributes on each handler's parameters and generates the correct `AutoClaim` for every entry in the `vec![…]` you pass to `SpelOutput::execute(…)`. You never construct `AccountPostState` values by hand.

## 4. Set Up IDL Generator

`examples/src/bin/generate_idl.rs` (scaffold creates this):

```rust
spel_framework::generate_idl!("../methods/guest/src/bin/my_program.rs");
```

Path is relative to `CARGO_MANIFEST_DIR` (the `examples/` crate).

## 5. Set Up CLI Wrapper

`examples/src/bin/my_program_cli.rs` (scaffold creates this):

```rust
#[tokio::main]
async fn main() {
    spel::run().await;
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

With the scaffold-generated `spel.toml` in the project root, `spel` discovers the IDL and binary automatically — no `-i`/`-p` or `--` separator needed.

```bash
# See available commands
spel --help

# Initialize (PDA accounts auto-computed, not passed as args)
spel initialize --owner <SIGNER_BASE58>

# Update with argument
spel update --new-value 42 --owner <SIGNER_BASE58>

# Use a raw 64-char hex program ID to skip binary loading
spel --program <64-CHAR-HEX> -- update --new-value 100 --owner <SIGNER_BASE58>

# Dry run (text-default; add =json for machine-readable output)
spel --dry-run update --new-value 5 --owner <ADDR>
spel --dry-run=json update --new-value 5 --owner <ADDR> | jq .

# Compute PDA manually — prints base58 address only.
# (Dry-run / transaction output additionally echoes `seeds: [program_id, "state"]`.)
spel pda state

# Decode a stored account's data via the IDL (requires #[account_type] on the struct
# AND an IDL generated with `spel generate-idl`, not `make idl`)
spel inspect "$(spel pda state)" --type MyState
```

When running without a `spel.toml`, pass `--idl`/`--program` before a `--` separator:

```bash
spel -i my-program-idl.json -p methods/guest/target/.../my_program.bin -- \
  update --new-value 42 --owner <SIGNER_BASE58>
```

## 10. Generate Client Code (optional)

```bash
spel-client-gen --idl my-program-idl.json --out-dir generated/
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
