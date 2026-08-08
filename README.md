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

### Extension Libraries

Third-party libraries can ship `#[instruction]` fns that are auto-discovered by the framework and merged into a consuming program's dispatcher and IDL. Discovery is driven by metadata in the library's `Cargo.toml`:

```toml
[package.metadata.spel]
extension_attr = "admin_authority"
```

When a consumer's `#[lez_program]` module carries the declared `extension_attr` (e.g. `#[admin_authority]`), the framework scans the library's `src/lib.rs` for `#[instruction]` fns and merges them with cross-crate dispatcher calls. Per-instruction attrs the library owns (e.g. `#[require_admin]`) stay on the emitted handlers and expand there as the library's own proc-macros.

Trust model: activating an extension takes two explicit consumer actions, the dependency in the consumer's own `Cargo.toml` and the marker attr on the module. Discovery covers direct dependencies only, so a transitive crate can never contribute instructions by claiming a matching `extension_attr`. Generated call paths derive from the dependency's `[package].name`, never its directory name. Malformed `[package.metadata.spel]` fails the build rather than silently deactivating the extension. Duplicate instruction names across user fns and extensions are a compile error naming both sources.

Contracts an extension author must hold:

1. **Instruction fns are re-exported at the crate root.** The generated dispatcher calls `::your_crate::your_instruction(...)`; a fn nested in a private module does not resolve.
2. **Signature types resolve at the consumer's expansion site.** Extension instruction signatures are copied verbatim into consumer-side codegen, so reference your own types by absolute path (`::your_crate::YourType`) rather than relying on imports.
3. **Gate and marker attrs are self-consuming proc-macros.** Attrs on items inside a module expand once, after the outer `#[lez_program]` rewrite, on the emitted handlers. Ship every instruction-level attr as a real proc-macro that handles that expansion: a gate rewrites the handler body, a marker expands to nothing. The framework strips nothing.

An extension whose gate needs specific accounts can additionally declare an inject block:

```toml
[[package.metadata.spel.inject]]
wrapper = "require_admin"

  [[package.metadata.spel.inject.account]]
  name = "admin_config"
  seed = { const = "admin_config" }

  [[package.metadata.spel.inject.account]]
  name = "caller"
  signer = true
```

Any consumer instruction carrying the named wrapper attribute (bare, without arguments) gets the listed account params synthesized at expansion time unless it already declares them (skip-if-declared). A seed can also be a compound list, `seed = [{ const = "frozen" }, { account = "caller" }]`, emitted as a compound PDA constraint. Injection runs identically in the compile-time expansion and in `spel generate-idl`, so the IDL producers cannot diverge. Injected params are prepended after a leading `ProgramContext`, in the block's declaration order, and are part of the instruction's ABI as shown in the IDL. When multiple extensions inject on one instruction, the order of their marker attrs on the module decides which extension's params come first.

The framework holds no library-specific knowledge. Multiple extensions stack on one program without coordination. First consumer of this mechanism is [`admin-authority`](https://github.com/mmlado/spel-admin-authority).

#### Auto-Wrap (Optional)

Extensions can additionally request that the framework automatically prepend a per-instruction attribute (e.g. a freeze gate) to every dispatched instruction the consumer ships. Activated by a second metadata table:

```toml
[package.metadata.spel.wrap_instructions]
wrapper = "freeze_authority::require_not_frozen"
skip = "manual"
self_exempt_marker = "freeze_exempt"
exempt = [
  "admin_authority::admin_initialize",
  "admin_authority::admin_transfer",
  "admin_authority::admin_renounce",
]
```

When the extension declares a `skip` word and the consumer's marker carries it as an arg (e.g. `#[freeze_authority(manual)]`), wrap is disabled and the consumer falls back to per-instruction opt-in. Omitting `skip` means the extension offers no opt-out word, and wrap is active for every consumer carrying the marker. Otherwise the framework walks every dispatched instruction and prepends `wrapper`, except those carrying the `self_exempt_marker` attribute or named in `exempt` (cross-crate carve-outs from other extensions).

First consumer of this mechanism is [`freeze-authority`](https://github.com/mmlado/spel-freeze-authority).

#### Embedded Mode (Optional)

An extension's config state can live inside one of the consumer's own accounts instead of a dedicated PDA. The consumer declares it program-wide on the module marker, role kwarg plus byte offset:

```rust
#[lez_program]
#[admin_authority(admin_config = config, offset = 32)]
mod my_program { ... }
```

The framework then rewrites the named role end to end. The role's inject entry retargets to the consumer account with the constraint copied from the consumer's account-creating declaration, the `#[account(init, pda = ...)]` one, minus `init` and `mut`. Gated instructions that declare the account use it, ones that do not get it injected PDA-verified. Every gate is stamped with the location kwargs and the offset by the framework itself, after the injection decision, so authored args keep disabling injection and a consumer-written location kwarg is a compile error, the marker is the only writer. Discovered instructions get the role param substituted to the consumer account, and instructions the extension names in its embedded metadata are not emitted at all, typically the initializer, because the slot is born initialized by the consumer's own account-creating instruction.

Two more metadata tables drive the extension side:

```toml
[package.metadata.spel.embedded]
skip = ["admin_initialize"]
state_type = "admin_authority::AdminConfig"

[[package.metadata.spel.bound_args]]
arg = "offset"
from = "offset"
default = 0
```

`embedded.skip` names discovered instructions dropped in embedded mode. `embedded.state_type` names the type occupying the embedded window and is mandatory in embedded mode: the program macro emits a window collision assert per embed pair sharing an account, each window's length read through `<state_type as FixedBorshSize>::SIZE`, so two extensions claiming overlapping byte ranges refuse to compile. Touching windows are legal. Discovery itself rejects identical offsets in every producer, the CLI included, and defers the range check to rustc, the only party that knows sizes. `bound_args` declares a trailing fn param the framework strips at discovery and fills at the dispatch call site as a compile-time literal, resolved from the marker kwarg or the default. The value never appears in the IDL or the transaction, a caller-supplied offset would be a caller-controlled write location. Dedicated mode is the degenerate case offset zero over the extension's own PDA, one code path.

`from` accepts two shapes. `"offset"` reads the extension's own marker. `"<marker>::offset"` reads a peer marker on the same module, so an extension can depend on where a peer embedded its state without depending on the peer's crate. `default` is optional. When the referenced marker or kwarg is absent the default applies, and a bound arg without a default makes both hard errors at the consumer's build, never a silent zero.

When two embedded roles resolve to the same consumer account, the framework merges them into one transaction account: listed once in the IDL, enum, and validation with the union of their `mut` and `signer` constraints, and cloned into every duplicated position of the precompiled call. Two embeds naming the same account at the same offset are a compile error.

One amendment to the wrapper-kwarg contract above: `offset` is the single non-role kwarg a stamped gate attr may carry, and framework-stamped args do not count as consumer-authored, only the authored form disables injection.

Embedded mode ships in [`admin-authority`](https://github.com/mmlado/spel-admin-authority) and [`freeze-authority`](https://github.com/mmlado/spel-freeze-authority), including both extensions sharing one consumer account.

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

## Troubleshooting

### Guest build fails with `ring` cross-compilation error on riscv32

If your guest build fails with:

```
riscv32-unknown-elf-gcc: error: unrecognized command-line option '-m64'
error: failed to run custom build command for `ring v0.17.14`
```

This is caused by the LEZ workspace enabling `risc0-zkvm` default features (`bonsai`, `client`), which pull in `reqwest → rustls → ring`. The `ring` crate cannot cross-compile for riscv32.

**Root cause:** [logos-blockchain/logos-execution-zone issue #468](https://github.com/logos-blockchain/logos-execution-zone/issues/468)

**Workaround:** fork the LEZ repo, apply this one-line change to `Cargo.toml`, and patch your workspace:

```diff
- risc0-zkvm = { version = "3.0.5", features = ["std"] }
+ risc0-zkvm = { version = "3.0.5", default-features = false, features = ["std"] }
```

Then in your workspace `Cargo.toml`:

```toml
[patch."https://github.com/logos-blockchain/logos-execution-zone.git"]
nssa_core = { git = "https://github.com/YOUR-USER/logos-execution-zone.git", branch = "fix-risc0-defaults" }
nssa = { git = "https://github.com/YOUR-USER/logos-execution-zone.git", branch = "fix-risc0-defaults" }
```

Once the upstream fix is merged, remove the `[patch]` section and update your LEZ dependency tag.

## License

MIT
