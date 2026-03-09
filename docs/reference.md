# SPEL Framework Reference

Comprehensive API reference for the SPEL framework (`logos-co/spel`). This document covers every macro, type, CLI command, IDL schema, and code generation feature.

For a guided walkthrough, see the [Tutorial](tutorial.md).

## Table of Contents

- [Macros](#macros)
  - [`#[lez_program]`](#lez_program)
  - [`#[instruction]`](#instruction)
  - [`generate_idl!`](#generate_idl)
- [Types (lez-framework-core)](#types-lez-framework-core)
  - [`LezOutput`](#lezoutput)
  - [`LezResult`](#lezresult)
  - [`LezError`](#lezerror)
  - [`AccountConstraint`](#accountconstraint)
  - [`InstructionMeta` / `AccountMeta` / `ArgMeta`](#instructionmeta--accountmeta--argmeta)
  - [Re-exported nssa_core types](#re-exported-nssa_core-types)
- [CLI (lez-cli)](#cli-lez-cli)
  - [Global Options](#global-options)
  - [`init`](#init)
  - [`inspect`](#inspect)
  - [`idl`](#idl-command)
  - [`pda` (IDL mode)](#pda-idl-mode)
  - [`pda` (raw mode)](#pda-raw-mode)
  - [Instruction Execution](#instruction-execution)
  - [Type Format Table](#type-format-table)
- [IDL Format](#idl-format)
  - [Top-Level Schema](#top-level-schema)
  - [`instructions`](#instructions)
  - [`accounts` (in instructions)](#accounts-in-instructions)
  - [`args`](#args)
  - [`pda`](#pda)
  - [IDL Types](#idl-types)
  - [`accounts` (top-level)](#accounts-top-level)
  - [`types`](#types-section)
  - [`errors`](#errors-section)
  - [Discriminators](#discriminators)
  - [`spec` and `metadata`](#spec-and-metadata)
  - [`instruction_type`](#instruction_type)
  - [Full Example IDL](#full-example-idl)
- [Client Code Generation (lez-client-gen)](#client-code-generation-lez-client-gen)
  - [CLI Usage](#lez-client-gen-cli-usage)
  - [Library API](#library-api)
  - [Generated Rust Client](#generated-rust-client)
  - [Generated C FFI](#generated-c-ffi)
  - [Generated C Header](#generated-c-header)
  - [Using from C++/Qt](#using-from-cqt)
- [Validation](#validation)
  - [Generated Validation Functions](#generated-validation-functions)
  - [Validation Helpers](#validation-helpers)

---

## Macros

### `#[lez_program]`

**Crate:** `lez-framework-macros` (re-exported by `lez-framework`)

Attribute macro applied to a module. Transforms a module of `#[instruction]` functions into a complete LEZ guest binary with dispatch, validation, and IDL generation.

#### Syntax

```rust
#[lez_program]
mod my_program {
    #[instruction]
    pub fn my_instruction(/* ... */) -> LezResult { /* ... */ }
}
```

With external instruction enum:

```rust
#[lez_program(instruction = "my_crate::Instruction")]
mod my_program {
    #[instruction]
    pub fn my_instruction(/* ... */) -> LezResult { /* ... */ }
}
```

#### Attributes

| Attribute | Type | Description |
|-----------|------|-------------|
| `instruction` | `"path::to::Enum"` | Optional. External instruction enum path. When set, the macro imports this type instead of generating its own `Instruction` enum. Used when the enum must be shared between on-chain and off-chain code (e.g., for correct borsh serialization in FFI). |

#### What It Generates

1. **`Instruction` enum** — a `#[derive(Debug, Clone, Serialize, Deserialize)]` enum with one variant per `#[instruction]` function. Variant names are PascalCase conversions of function names. Only non-account parameters become enum fields. Skipped if `instruction = "..."` attribute is set.

2. **`fn main()`** — the zkVM guest entry point (gated by `#[cfg(not(test))]`). Reads `ProgramInput` from the host, dispatches to the correct handler via `match`, and writes outputs via `write_nssa_outputs_with_chained_call`. Account destructuring from `pre_states` is generated per-instruction.

3. **Validation functions** — one `__validate_{fn_name}()` function per instruction that has `signer` or `init` constraints. These run before the handler and return `LezError` on failure.

4. **`PROGRAM_IDL_JSON`** — a `pub const &str` containing the complete IDL as JSON. Available at compile time in any build target.

5. **`__program_idl()`** — a function returning a constructed `LezIdl` struct (with discriminators, execution metadata, etc.).

#### Constraints

- The module **must have a body** (not `mod foo;`).
- The module must contain **at least one** `#[instruction]` function.
- Instruction functions **cannot have `self`** parameters.
- Account parameters must be typed `AccountWithMetadata` or `Vec<AccountWithMetadata>`.

#### Example

```rust
use lez_framework::prelude::*;
use nssa_core::account::AccountWithMetadata;
use nssa_core::program::AccountPostState;

#[lez_program]
mod treasury {
    use super::*;

    #[instruction]
    pub fn deposit(
        #[account(mut, pda = literal("vault"))]
        vault: AccountWithMetadata,
        #[account(signer)]
        depositor: AccountWithMetadata,
        amount: u128,
    ) -> LezResult {
        // ... business logic ...
        Ok(LezOutput::states_only(vec![
            AccountPostState::new(vault.account.clone()),
            AccountPostState::new(depositor.account.clone()),
        ]))
    }
}
```

This generates:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Instruction {
    Deposit { amount: u128 },
}
```

---

### `#[instruction]`

**Crate:** `lez-framework-macros` (re-exported by `lez-framework`)

Marker attribute for functions inside an `#[lez_program]` module. This attribute is processed by `#[lez_program]` — it is a no-op when used standalone.

#### Function Signature Requirements

```rust
#[instruction]
pub fn instruction_name(
    // Account parameters — must come first
    #[account(/* constraints */)]
    account_name: AccountWithMetadata,

    // Variable-length accounts (at most one, must be last account param)
    #[account(/* constraints */)]
    rest_accounts: Vec<AccountWithMetadata>,

    // Instruction arguments — after all account params
    arg1: u64,
    arg2: String,
) -> LezResult {
    // ...
}
```

- **Return type** must be `LezResult` (alias for `Result<LezOutput, LezError>`).
- **Account parameters** are identified by type: `AccountWithMetadata` for single accounts, `Vec<AccountWithMetadata>` for variable-length.
- **All other parameters** are instruction arguments and become fields in the generated `Instruction` enum variant.
- Function names use `snake_case`; generated enum variants use `PascalCase`.

#### Account Constraint Attributes

Applied via `#[account(...)]` on account parameters:

| Constraint | Syntax | Description |
|------------|--------|-------------|
| `signer` | `#[account(signer)]` | Requires `is_authorized == true`. Generates a runtime check before the handler runs. |
| `init` | `#[account(init)]` | Account must be uninitialized (`== Account::default()`). **Implies `mut`**. Used with `AccountPostState::new_claimed()`. |
| `mut` | `#[account(mut)]` | Account is writable. Sets `writable: true` in the IDL. |
| `owner` | `#[account(owner = EXPR)]` | Account must be owned by the given program ID. The expression should resolve to `[u8; 32]`. |
| `pda` | `#[account(pda = SEED)]` | Account address is a PDA derived from the program ID and seed(s). See [PDA Seeds](#pda-seeds) below. Sets the `pda` field in the IDL. |
| `rest` | _(implicit)_ | Not an explicit attribute. When the type is `Vec<AccountWithMetadata>`, the account is treated as variable-length (`rest: true` in the IDL). |

Constraints can be combined:

```rust
#[account(init, signer, pda = literal("state"))]
state: AccountWithMetadata,

#[account(mut, owner = TOKEN_PROGRAM_ID)]
token_account: AccountWithMetadata,
```

#### PDA Seeds

The `pda` attribute specifies how the account address is derived. Seed types:

| Seed Type | Syntax | Description |
|-----------|--------|-------------|
| `const` | `literal("string")` or `seed_const("string")` | Constant string, UTF-8 encoded and zero-padded to 32 bytes. Aliases: `const(...)`, `r#const(...)`, `seed_const(...)`, `literal(...)`. |
| `account` | `account("other_account_name")` | Uses another account's 32-byte ID as the seed. |
| `arg` | `arg("argument_name")` | Uses an instruction argument's value as the seed. |

**Single seed:**

```rust
#[account(pda = literal("my_state"))]
```

**Multiple seeds** (array syntax):

```rust
#[account(pda = [literal("multisig_state__"), arg("create_key")])]
#[account(pda = [literal("vault"), account("user")])]
#[account(pda = [literal("holding"), account("token"), account("user")])]
```

**PDA derivation logic:**
- Each seed is resolved to 32 bytes (strings are zero-padded)
- Single seed: used directly as `PdaSeed`
- Multiple seeds: combined via `SHA-256(seed1_32 || seed2_32 || ...)` into a single 32-byte seed
- Final address: `AccountId::from((program_id, &PdaSeed::new(combined)))`

---

### `generate_idl!`

**Crate:** `lez-framework-macros` (re-exported by `lez-framework`)

Proc macro that reads a Rust source file at compile time, finds the `#[lez_program]` module, and generates a `fn main()` that prints the complete IDL as JSON.

#### Syntax

```rust
lez_framework::generate_idl!("path/to/program.rs");
```

The path is resolved relative to `CARGO_MANIFEST_DIR`. The file must contain exactly one `#[lez_program]` module.

#### What It Generates

A `fn main()` that:
1. Includes the source file via `include_str!` for cargo dependency tracking
2. Parses the embedded IDL JSON string
3. Pretty-prints it to stdout

#### Usage

Create a binary crate (e.g., `examples/src/bin/generate_idl.rs`):

```rust
/// Generate IDL JSON for the my_program program.
///
/// Usage:
///   cargo run --bin generate_idl > my_program-idl.json
lez_framework::generate_idl!("../methods/guest/src/bin/my_program.rs");
```

Then run:

```bash
cargo run --bin generate_idl > my_program-idl.json
```

The macro detects the `instruction = "..."` attribute on `#[lez_program(...)]` and includes the `instruction_type` field in the generated IDL when present.

---

## Types (lez-framework-core)

### `LezOutput`

Return value from instruction handlers. Contains post-states and optional chained calls.

```rust
pub struct LezOutput {
    pub post_states: Vec<AccountPostState>,
    pub chained_calls: Vec<ChainedCall>,
}
```

#### Methods

| Method | Signature | Description |
|--------|-----------|-------------|
| `states_only` | `fn states_only(post_states: Vec<AccountPostState>) -> Self` | Create output with only post-states and no chained calls. Most common case. |
| `with_chained_calls` | `fn with_chained_calls(post_states: Vec<AccountPostState>, chained_calls: Vec<ChainedCall>) -> Self` | Create output with both post-states and chained calls (cross-program invocation). |
| `empty` | `fn empty() -> Self` | Create an empty output (no states, no calls). |
| `into_parts` | `fn into_parts(self) -> (Vec<AccountPostState>, Vec<ChainedCall>)` | Destructure into the tuple form expected by `write_nssa_outputs_with_chained_call`. Used by generated code. |

#### Example

```rust
// Most instructions: just return updated states
Ok(LezOutput::states_only(vec![
    AccountPostState::new_claimed(new_account),  // init
    AccountPostState::new(updated_account),       // update
]))

// Cross-program call
Ok(LezOutput::with_chained_calls(
    vec![AccountPostState::new(state.account.clone())],
    vec![chained_call],
))
```

---

### `LezResult`

Type alias for instruction handler return types:

```rust
pub type LezResult = Result<LezOutput, LezError>;
```

All `#[instruction]` functions must return `LezResult`.

---

### `LezError`

Structured error type for LEZ programs. Borsh-serializable for on-chain representation.

```rust
#[derive(Error, Debug, BorshSerialize, BorshDeserialize)]
pub enum LezError { /* ... */ }
```

#### Variants

| Variant | Error Code | Fields | Description |
|---------|-----------|--------|-------------|
| `AccountCountMismatch` | 1000 | `expected: usize, actual: usize` | Wrong number of accounts provided |
| `InvalidAccountOwner` | 1001 | `account_index: usize, expected_owner: String` | Account not owned by expected program |
| `AccountAlreadyInitialized` | 1002 | `account_index: usize` | `init` account already has data |
| `AccountNotInitialized` | 1003 | `account_index: usize` | Account expected to be initialized but is empty |
| `InsufficientBalance` | 1004 | `available: u128, requested: u128` | Insufficient balance for operation |
| `DeserializationError` | 1005 | `account_index: usize, message: String` | Failed to deserialize account data |
| `SerializationError` | 1006 | `message: String` | Failed to serialize data |
| `Overflow` | 1007 | `operation: String` | Arithmetic overflow |
| `Unauthorized` | 1008 | `message: String` | Authorization/signer check failed |
| `PdaMismatch` | 1009 | `account_index: usize` | PDA derivation did not match |
| `Custom` | 6000 + code | `code: u32, message: String` | Program-specific error |

#### Methods

| Method | Signature | Description |
|--------|-----------|-------------|
| `custom` | `fn custom(code: u32, message: impl Into<String>) -> Self` | Create a custom error. Numeric code starts at 6000. |
| `error_code` | `fn error_code(&self) -> u32` | Get the numeric error code for client-side handling. |

#### Example

```rust
// Using built-in variants
if balance < amount {
    return Err(LezError::InsufficientBalance {
        available: balance,
        requested: amount,
    });
}

// Using custom errors
return Err(LezError::custom(1, "Proposal already executed"));
// error_code() returns 6001
```

---

### `AccountConstraint`

Internal type used by the macro-generated validation code. Not typically used directly.

```rust
pub struct AccountConstraint {
    pub mutable: bool,
    pub init: bool,
    pub owner: Option<[u8; 32]>,
    pub signer: bool,
    pub seeds: Option<Vec<Vec<u8>>>,
}
```

---

### `InstructionMeta` / `AccountMeta` / `ArgMeta`

Metadata types used for IDL generation. Not typically used directly.

```rust
pub struct InstructionMeta {
    pub name: String,
    pub accounts: Vec<AccountMeta>,
    pub args: Vec<ArgMeta>,
}

pub struct AccountMeta {
    pub name: String,
    pub writable: bool,
    pub init: bool,
    pub owner: Option<String>,
    pub signer: bool,
    pub pda_seeds: Option<Vec<String>>,
}

pub struct ArgMeta {
    pub name: String,
    pub type_name: String,
}
```

---

### Re-exported nssa_core types

The following types are re-exported through `lez-framework-core::prelude`:

| Type | Origin | Description |
|------|--------|-------------|
| `Account<T>` | `nssa_core::account` | Generic account wrapper |
| `AccountWithMetadata` | `nssa_core::account` | Account data plus `account_id` and `is_authorized` flag. Primary type used in instruction handlers. |
| `AccountPostState` | `nssa_core::program` | Represents the state of an account after instruction execution. Use `new()` for existing accounts, `new_claimed()` for newly initialized accounts. |
| `ChainedCall` | `nssa_core::program` | Cross-program invocation data. Returned in `LezOutput::with_chained_calls()`. |
| `PdaSeed` | `nssa_core::program` | A 32-byte PDA seed. Constructed via `PdaSeed::new(bytes)`. |
| `ProgramId` | `nssa_core::program` | Type alias for `[u32; 8]`. Identifies a program by its RISC Zero image ID. |

---

## CLI (lez-cli)

The `lez-cli` crate provides a generic, IDL-driven CLI for any LEZ program. Programs get a complete CLI by writing a three-line wrapper:

```rust
#[tokio::main]
async fn main() {
    lez_cli::run().await;
}
```

### Global Options

| Option | Short | Description |
|--------|-------|-------------|
| `--idl <FILE>` | `-i` | Path to the IDL JSON file. Required for most commands. |
| `--program <FILE>` | `-p` | Path to the program ELF binary. Used to compute ProgramId and for deployment. Defaults to `program.bin`. |
| `--program-id <HEX>` | | 64-character hex string of the program ID. Overrides `--program` for ProgramId resolution (faster, no binary loading). |
| `--dry-run` | | Print parsed/serialized data without submitting the transaction. |
| `--bin-<NAME> <FILE>` | | Additional program binary. Auto-fills `--<NAME>-program-id` from the binary's image ID. Useful for cross-program references. |

---

### `init`

Scaffold a new LEZ project.

```bash
lez-cli init <project-name>
```

**Does not require `--idl`.**

Creates a complete project structure with:
- Workspace `Cargo.toml`
- `{name}_core/` crate for shared types
- `methods/guest/` with a skeleton `#[lez_program]` guest binary
- `examples/` with `generate_idl.rs` and `{name}_cli.rs`
- `Makefile` with `build`, `idl`, `cli`, `deploy`, `inspect`, `setup`, `status`, `clean` targets
- `README.md` with quick start guide
- `.gitignore`

**Example:**

```bash
lez-cli init my-token
cd my-token
# Edit methods/guest/src/bin/my_token.rs with your program logic
make idl
make cli ARGS="--help"
```

---

### `inspect`

Print the ProgramId for one or more ELF binaries.

```bash
lez-cli inspect <FILE> [FILE...]
```

**Does not require `--idl`.**

**Output for each binary:**

```
📦 path/to/program.bin
   ProgramId (decimal): 12345,67890,11111,22222,33333,44444,55555,66666
   ProgramId (hex):     00003039,000109b2,...
   ImageID (hex bytes): 393000009b210100...
```

The three formats:
- **Decimal**: comma-separated `[u32; 8]` values
- **Hex**: comma-separated hex `[u32; 8]` values
- **ImageID hex bytes**: 64-character hex string (little-endian byte representation)

**Example:**

```bash
lez-cli inspect methods/guest/target/riscv32im-risc0-zkvm-elf/docker/my_program.bin
```

---

### `idl` (command)

Print the loaded IDL as pretty-printed JSON.

```bash
lez-cli --idl <IDL_FILE> idl
```

**Example:**

```bash
lez-cli -i my_program-idl.json idl
```

---

### `pda` (IDL mode)

Compute a PDA address from the IDL-defined seeds.

```bash
lez-cli --idl <IDL_FILE> [-p <PROGRAM> | --program-id <HEX>] pda <ACCOUNT_NAME> [--<seed-arg> <value> ...]
```

Looks up the named account across all instructions in the IDL, finds its PDA seed definition, resolves all seeds, and prints the base58 AccountId.

**Seed resolution:**
- `const` seeds: resolved from the IDL definition
- `arg` seeds: must be provided via `--<arg-name> <value>`
- `account` seeds: must be provided via `--<account-name>-account <hex|base58>`

**ProgramId resolution** (in priority order):
1. `--program-id <hex>` flag
2. Load from `--program <path>` binary

**Example:**

```bash
# Simple PDA with only const seeds
lez-cli -i my_program-idl.json --program-id abc123...def pda counter

# PDA with arg seed
lez-cli -i multisig-idl.json --program-id abc123...def pda multisig_state \
  --create-key 0a1b2c...

# List available PDAs
lez-cli -i my_program-idl.json pda
```

**If no account name is given**, prints all PDA accounts found in the IDL.

---

### `pda` (raw mode)

Compute an arbitrary PDA from a program ID and raw seeds — no IDL required.

```bash
lez-cli --program-id <64-CHAR-HEX> pda <SEED1> [SEED2] ...
```

**Does not require `--idl`.**

Each seed can be:
- **64-character hex string**: interpreted as 32 raw bytes
- **Plain string**: UTF-8 encoded and zero-padded to 32 bytes (max 32 bytes)

**Multi-seed derivation:** `SHA-256(seed1_32 || seed2_32 || ...)`

**Output:** base58 AccountId

**Example:**

```bash
# Single seed
lez-cli --program-id abc123...def pda my_state_name

# Multiple seeds
lez-cli --program-id abc123...def pda multisig_vault__ 0a1b2c3d...
```

---

### Instruction Execution

Execute any instruction defined in the IDL. The CLI auto-generates subcommands from the IDL.

```bash
lez-cli --idl <IDL_FILE> [-p <PROGRAM> | --program-id <HEX>] <INSTRUCTION> [--<arg> <value> ...] [--<account>-account <hex|base58> ...]
```

Instruction names are converted from `snake_case` to `kebab-case` in CLI commands (e.g., `create_proposal` → `create-proposal`).

**Arguments:**
- Instruction args: `--<arg-name> <value>` (type-aware parsing from IDL)
- Non-PDA accounts: `--<account-name>-account <base58|hex>` (64 hex chars or base58 string)
- PDA accounts: **auto-computed** from seeds — not passed as arguments
- Rest (variadic) accounts: optional, comma-separated list of account IDs

**Additional program binaries:** Use `--bin-<name> <file>` to auto-fill `--<name>-program-id` from the binary's image ID.

**Transaction flow:**
1. Parse and validate all arguments
2. Auto-fill program IDs from `--bin-*` flags
3. Serialize instruction data in risc0 serde format
4. Resolve PDA accounts from seeds
5. Initialize wallet from `NSSA_WALLET_HOME_DIR` environment variable
6. Fetch nonces for signer accounts
7. Build, sign, and submit the transaction
8. Poll for confirmation

**Per-instruction help:**

```bash
lez-cli --idl <IDL_FILE> <INSTRUCTION> --help
```

Shows accounts (with flags like `[mut, signer, init]`), PDA status, and argument types.

**Example:**

```bash
# Execute a create instruction
lez-cli -i multisig-idl.json -p multisig.bin create \
  --create-key 0a1b2c3d4e5f... \
  --threshold 2 \
  --members "aabb...00,ccdd...00" \
  --creator-account EjR7...base58

# Dry run (no submission)
lez-cli -i multisig-idl.json --program-id abc123...def --dry-run approve \
  --proposal-id 5 \
  --proposal-account aabb...00 \
  --member-account ccdd...00

# Auto-fill cross-program reference
lez-cli -i treasury-idl.json -p treasury.bin \
  --bin-token token.bin \
  transfer --amount 100 \
  --from-account aabb...00 \
  --to-account ccdd...00
```

---

### Type Format Table

How to pass values for each IDL type on the command line:

| IDL Type | CLI Format | Example |
|----------|-----------|---------|
| `u8` | Decimal number | `255` |
| `u32` | Decimal number | `1000000` |
| `u64` | Decimal number | `1000000000` |
| `u128` | Decimal number | `340282366920938463463374607431768211455` |
| `bool` | `true`/`false`/`1`/`0`/`yes`/`no` | `true` |
| `string` / `String` | Plain text | `"hello world"` |
| `[u8; N]` | Hex string (`2*N` hex chars) **or** UTF-8 string (≤N chars, zero-padded) | `0a1b2c...` (64 chars for N=32) or `my_string` |
| `[u32; 8]` / `program_id` | 8 comma-separated u32 values, or 64 hex chars | `0,0,0,0,0,0,0,0` or `abc123...def` |
| `Vec<[u8; 32]>` | Comma-separated hex strings | `"aabb...00,ccdd...00"` |
| `Vec<u8>` | Comma-separated decimal bytes | `1,2,3,4,5` |
| `Vec<u32>` | Comma-separated u32 values | `100,200,300` |
| `Option<T>` | `none`/`null`/empty for None; otherwise same as inner type | `none` or `42` |
| Account IDs | Base58 string **or** 64 hex chars (with optional `0x` prefix) | `EjR7...` or `0xaabb...00` |

**Notes:**
- `[u8; N]` accepts both hex and string formats. Hex is detected by length (exactly `2*N` chars, all hex digits). Otherwise treated as UTF-8 and zero-padded.
- `0x` prefix is accepted and stripped for hex values.
- `program_id` values can also use `0x`-prefixed hex for individual u32 components.

---

## IDL Format

The IDL (Interface Definition Language) is a JSON file that describes a LEZ program's complete interface. It is generated by the `generate_idl!` macro or the `__program_idl()` function.

### Top-Level Schema

```json
{
  "version": "0.1.0",
  "name": "program_name",
  "instructions": [ /* ... */ ],
  "accounts": [],
  "types": [],
  "errors": [],
  "spec": "0.1.0",
  "metadata": {
    "name": "program_name",
    "version": "0.1.0"
  },
  "instruction_type": "my_crate::Instruction"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `version` | `string` | Yes | IDL version. Currently `"0.1.0"`. |
| `name` | `string` | Yes | Program name (from the module name). |
| `instructions` | `array` | Yes | List of instruction definitions. |
| `accounts` | `array` | Yes | Account type definitions (currently unused, always `[]`). |
| `types` | `array` | Yes | Custom type definitions (currently unused, always `[]`). |
| `errors` | `array` | Yes | Error definitions (currently unused, always `[]`). |
| `spec` | `string` | No | IDL spec version identifier (lssa-lang compat). |
| `metadata` | `object` | No | Program metadata with `name` and `version` (lssa-lang compat). |
| `instruction_type` | `string` | No | Fully-qualified Rust path to external instruction enum. When set, generated FFI imports this type. E.g., `"multisig_core::Instruction"`. |

---

### `instructions`

Each instruction object:

```json
{
  "name": "create_proposal",
  "accounts": [ /* ... */ ],
  "args": [ /* ... */ ],
  "discriminator": [72, 137, 94, 219, 188, 57, 3, 12],
  "execution": { "public": true, "private_owned": false },
  "variant": "CreateProposal"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | `string` | Yes | Instruction name in `snake_case`. |
| `accounts` | `array` | Yes | Accounts expected by this instruction (in order). |
| `args` | `array` | Yes | Instruction arguments. |
| `discriminator` | `array<u8>` | No | SHA-256("global:{name}")[..8] — 8-byte discriminator (lssa-lang compat). |
| `execution` | `object` | No | `{ "public": bool, "private_owned": bool }` — execution mode (lssa-lang compat). Defaults to public. |
| `variant` | `string` | No | PascalCase variant name in the `Instruction` enum (lssa-lang compat). |

---

### `accounts` (in instructions)

Each account object within an instruction:

```json
{
  "name": "multisig_state",
  "writable": true,
  "signer": false,
  "init": true,
  "owner": null,
  "pda": {
    "seeds": [
      { "kind": "const", "value": "multisig_state__" },
      { "kind": "arg", "path": "create_key" }
    ]
  },
  "rest": false,
  "visibility": ["public"]
}
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `name` | `string` | | Account name from the function parameter. |
| `writable` | `bool` | `false` | Whether the account is modified by this instruction. Set by `mut` or `init`. |
| `signer` | `bool` | `false` | Whether the account must sign the transaction. |
| `init` | `bool` | `false` | Whether this is a new account being initialized. |
| `owner` | `string?` | `null` | Expected owner program ID (hex string), or null. |
| `pda` | `object?` | `null` | PDA derivation specification, or null for non-PDA accounts. |
| `rest` | `bool` | `false` | If true, this account represents a variable-length trailing account list (`Vec<AccountWithMetadata>`). |
| `visibility` | `array<string>` | `[]` | Visibility tags (lssa-lang compat). Typically `["public"]`. |

---

### `args`

Each argument object:

```json
{
  "name": "threshold",
  "type": "u64"
}
```

| Field | Type | Description |
|-------|------|-------------|
| `name` | `string` | Argument name in `snake_case`. |
| `type` | `IdlType` | The argument type. See [IDL Types](#idl-types). |

---

### `pda`

PDA derivation specification:

```json
{
  "seeds": [
    { "kind": "const", "value": "multisig_state__" },
    { "kind": "account", "path": "user" },
    { "kind": "arg", "path": "create_key" }
  ]
}
```

**Seed kinds:**

| Kind | Fields | Description |
|------|--------|-------------|
| `const` | `value: string` | Constant string seed. UTF-8 bytes, zero-padded to 32 bytes. |
| `account` | `path: string` | References another account by name. Uses the account's 32-byte ID. |
| `arg` | `path: string` | References an instruction argument by name. Value is converted to 32 bytes (type-dependent). |

**Derivation:**
- Single seed: used directly as a 32-byte PDA seed
- Multiple seeds: `SHA-256(seed1_bytes || seed2_bytes || ...)` → 32-byte combined seed
- Final: `AccountId::from((program_id, &PdaSeed::new(combined_seed)))`

---

### IDL Types

Types in the IDL are represented as JSON using an untagged format:

| Rust Type | IDL JSON | Description |
|-----------|----------|-------------|
| `u8` | `"u8"` | Primitive string |
| `u16` | `"u16"` | Primitive string |
| `u32` | `"u32"` | Primitive string |
| `u64` | `"u64"` | Primitive string |
| `u128` | `"u128"` | Primitive string |
| `i8`–`i128` | `"i8"`–`"i128"` | Signed integer primitives |
| `bool` | `"bool"` | Primitive string |
| `String` | `"string"` | Primitive string |
| `ProgramId` | `"program_id"` | Alias for `[u32; 8]` |
| `AccountId` | `"account_id"` | Alias for `[u8; 32]` |
| `Vec<T>` | `{ "vec": <T> }` | Vector of inner type |
| `Option<T>` | `{ "option": <T> }` | Optional inner type |
| `[T; N]` | `{ "array": [<T>, N] }` | Fixed-size array |
| Custom type | `{ "defined": "TypeName" }` | Reference to a named type |

**Examples:**

```json
"u64"
{ "vec": "u8" }
{ "vec": { "array": ["u8", 32] } }
{ "option": "string" }
{ "array": ["u8", 32] }
{ "defined": "MyCustomStruct" }
```

---

### `accounts` (top-level)

Top-level account type definitions. Each entry describes a named account struct:

```json
{
  "name": "MultisigState",
  "type": {
    "kind": "struct",
    "fields": [
      { "name": "threshold", "type": "u64" },
      { "name": "members", "type": { "vec": { "array": ["u8", 32] } } }
    ]
  }
}
```

Currently generated as an empty array by the macro. Future versions may populate this from account struct definitions.

---

### `types` (section)

Custom type definitions (struct or enum):

```json
{
  "kind": "struct",
  "fields": [
    { "name": "field_name", "type": "u64" }
  ],
  "variants": []
}
```

For enums:

```json
{
  "kind": "enum",
  "fields": [],
  "variants": [
    { "name": "Active", "fields": [] },
    { "name": "WithData", "fields": [{ "name": "value", "type": "u64" }] }
  ]
}
```

Currently generated as an empty array by the macro.

---

### `errors` (section)

Error definitions:

```json
{
  "code": 1000,
  "name": "AccountCountMismatch",
  "msg": "Expected {expected} accounts, got {actual}"
}
```

Currently generated as an empty array by the macro. The built-in `LezError` variants have fixed codes (see [LezError](#lezerror)).

---

### Discriminators

Each instruction can have a `discriminator` field — an 8-byte array computed as:

```
SHA-256("global:{instruction_name}")[..8]
```

This matches the lssa-lang convention. The discriminator is computed at macro expansion time and included in the IDL generated by `__program_idl()`.

**Example:** For instruction `create`:
```
SHA-256("global:create")[0..8] = [72, 137, 94, 219, 188, 57, 3, 12]
```

The `compute_discriminator()` function in `lez-framework-core/src/idl.rs` and `lez-framework-macros/src/lib.rs` implements this.

---

### `spec` and `metadata`

lssa-lang compatibility fields:

```json
{
  "spec": "0.1.0",
  "metadata": {
    "name": "program_name",
    "version": "0.1.0"
  }
}
```

These are included in the `__program_idl()` output but omitted from the `PROGRAM_IDL_JSON` const string.

---

### `instruction_type`

When `#[lez_program(instruction = "my_crate::Instruction")]` is used, the IDL includes:

```json
{
  "instruction_type": "my_crate::Instruction"
}
```

This tells `lez-client-gen` to import and use the external type in generated FFI code (instead of generating a local `#[derive(Serialize, Deserialize)]` enum), ensuring correct serialization for the zkVM guest.

---

### Full Example IDL

```json
{
  "version": "0.1.0",
  "name": "my_multisig",
  "instructions": [
    {
      "name": "create",
      "accounts": [
        {
          "name": "multisig_state",
          "writable": true,
          "signer": false,
          "init": true,
          "pda": {
            "seeds": [
              { "kind": "const", "value": "multisig_state__" },
              { "kind": "arg", "path": "create_key" }
            ]
          }
        },
        {
          "name": "creator",
          "writable": false,
          "signer": true,
          "init": false
        }
      ],
      "args": [
        { "name": "create_key", "type": { "array": ["u8", 32] } },
        { "name": "threshold", "type": "u64" },
        { "name": "members", "type": { "vec": { "array": ["u8", 32] } } }
      ],
      "discriminator": [72, 137, 94, 219, 188, 57, 3, 12],
      "execution": { "public": true, "private_owned": false },
      "variant": "Create"
    },
    {
      "name": "approve",
      "accounts": [
        {
          "name": "multisig_state",
          "writable": false,
          "signer": false,
          "init": false,
          "pda": {
            "seeds": [
              { "kind": "const", "value": "multisig_state__" }
            ]
          }
        },
        {
          "name": "proposal",
          "writable": true,
          "signer": false,
          "init": false
        },
        {
          "name": "member",
          "writable": false,
          "signer": true,
          "init": false
        }
      ],
      "args": [
        { "name": "proposal_id", "type": "u64" }
      ],
      "discriminator": [18, 234, 43, 71, 52, 0, 12, 8],
      "execution": { "public": true, "private_owned": false },
      "variant": "Approve"
    }
  ],
  "accounts": [],
  "types": [],
  "errors": [],
  "instruction_type": "multisig_core::Instruction"
}
```

---

## Client Code Generation (lez-client-gen)

Generates typed Rust client code and C FFI wrappers from LEZ program IDL JSON. Useful for integrating LEZ programs into applications (e.g., C++/Qt desktop apps).

### lez-client-gen CLI Usage

```bash
lez-client-gen --idl <PATH> --out-dir <DIR>
```

| Option | Required | Description |
|--------|----------|-------------|
| `--idl <path>` | Yes | Path to the IDL JSON file. |
| `--out-dir <dir>` | Yes | Output directory for generated files. Created if it doesn't exist. |
| `--help`, `-h` | | Print help. |

**Output files** (named after the program):

```
<out-dir>/
├── <program_name>_client.rs    # Typed Rust client
├── <program_name>_ffi.rs       # C FFI wrappers
└── <program_name>.h            # C header file
```

**Example:**

```bash
lez-client-gen --idl multisig-idl.json --out-dir generated/

# Output:
# Generated:
#   Client: generated/my_multisig_client.rs
#   FFI:    generated/my_multisig_ffi.rs
#   Header: generated/my_multisig.h
```

### Library API

```rust
use lez_client_gen::{generate_from_idl_json, generate_from_idl, CodegenOutput};

// From JSON string
let json = std::fs::read_to_string("idl.json")?;
let output: CodegenOutput = generate_from_idl_json(&json)?;

// From parsed IDL
let idl: LezIdl = serde_json::from_str(&json)?;
let output: CodegenOutput = generate_from_idl(&idl)?;

// Write output files
std::fs::write("client.rs", &output.client_code)?;
std::fs::write("ffi.rs", &output.ffi_code)?;
std::fs::write("program.h", &output.header)?;
```

**`CodegenOutput` fields:**

| Field | Type | Description |
|-------|------|-------------|
| `client_code` | `String` | Typed Rust client module source code |
| `ffi_code` | `String` | C FFI wrapper source code |
| `header` | `String` | C header file content |

---

### Generated Rust Client

The generated client includes:

1. **Instruction enum** — `{ProgramName}Instruction` with all variants
2. **Account structs** — one `{InstructionName}Accounts` struct per instruction, with fields for each account (using `AccountId` for single accounts, `Vec<AccountId>` for rest accounts)
3. **Client struct** — `{ProgramName}Client<'w>` with:
   - Constructor: `new(wallet: &WalletCore, program_id: ProgramId)`
   - One async method per instruction that builds, signs, and submits the transaction
   - PDA helper methods: `compute_{account}_pda(...)` for each PDA account
4. **Helper functions** — `parse_program_id_hex()`, `compute_pda()`

**Example generated code** (for a multisig program):

```rust
pub enum MyMultisigInstruction {
    Create { create_key: [u8; 32], threshold: u64, members: Vec<[u8; 32]> },
    Approve { proposal_id: u64 },
}

pub struct CreateAccounts {
    pub multisig_state: AccountId,
    pub creator: AccountId,
}

pub struct MyMultisigClient<'w> {
    pub wallet: &'w WalletCore,
    pub program_id: ProgramId,
}

impl<'w> MyMultisigClient<'w> {
    pub fn new(wallet: &'w WalletCore, program_id: ProgramId) -> Self { /* ... */ }
    pub async fn create(&self, accounts: CreateAccounts, create_key: [u8; 32], ...) -> Result<String, String> { /* ... */ }
    pub async fn approve(&self, accounts: ApproveAccounts, proposal_id: u64) -> Result<String, String> { /* ... */ }
    pub fn compute_multisig_state_pda(create_key: &[u8; 32]) -> AccountId { /* ... */ }
}
```

---

### Generated C FFI

The FFI module generates `extern "C"` functions that accept and return JSON strings. Each instruction gets a function:

```rust
#[no_mangle]
pub extern "C" fn my_multisig_create(args_json: *const c_char) -> *mut c_char { /* ... */ }

#[no_mangle]
pub extern "C" fn my_multisig_approve(args_json: *const c_char) -> *mut c_char { /* ... */ }

#[no_mangle]
pub extern "C" fn my_multisig_free_string(s: *mut c_char) { /* ... */ }

#[no_mangle]
pub extern "C" fn my_multisig_version() -> *mut c_char { /* ... */ }
```

**Required JSON fields for every instruction call:**

| Field | Type | Description |
|-------|------|-------------|
| `wallet_path` | `string` | Path to the NSSA wallet directory |
| `sequencer_url` | `string` | Sequencer URL (e.g., `"http://127.0.0.1:3040"`) |
| `program_id_hex` | `string` | 64-character hex string of the program ID |

Plus instruction-specific fields for accounts and arguments.

**Return format:**

```json
// Success
{ "success": true, "tx_hash": "abc123..." }

// Error
{ "success": false, "error": "error message" }
```

**Instruction type handling:**
- If the IDL has `instruction_type` set, the FFI imports and uses that type directly (`use path::to::Instruction as ProgramInstruction;`)
- Otherwise, a local `#[derive(Serialize, Deserialize)]` enum is generated

**PDA helpers:** Standalone `compute_{account}_pda()` functions are generated for each unique PDA account. Single-seed PDAs use `PdaSeed` directly; multi-seed PDAs use SHA-256 combination.

---

### Generated C Header

```c
/* Auto-generated C header for my_multisig FFI. DO NOT EDIT. */
#ifndef MY_MULTISIG_FFI_H
#define MY_MULTISIG_FFI_H

#ifdef __cplusplus
extern "C" {
#endif

/* create instruction */
char* my_multisig_create(const char* args_json);

/* approve instruction */
char* my_multisig_approve(const char* args_json);

void my_multisig_free_string(char* s);
char* my_multisig_version(void);

#ifdef __cplusplus
}
#endif

#endif /* MY_MULTISIG_FFI_H */
```

---

### Using from C++/Qt

1. **Generate the FFI:**

```bash
lez-client-gen --idl my_program-idl.json --out-dir ffi/
```

2. **Build as a shared library** by adding to your `Cargo.toml`:

```toml
[lib]
name = "my_program_ffi"
crate-type = ["cdylib"]
```

Include the generated FFI code in your lib:

```rust
// src/lib.rs
include!("../ffi/my_program_ffi.rs");
```

3. **Build:**

```bash
cargo build --release --lib
# Produces: target/release/libmy_program_ffi.so (Linux)
#           target/release/libmy_program_ffi.dylib (macOS)
```

4. **Use from C++/Qt:**

```cpp
#include "my_program.h"
#include <QJsonDocument>
#include <QJsonObject>

// Build the JSON arguments
QJsonObject args;
args["wallet_path"] = "/path/to/wallet";
args["program_id_hex"] = "abc123...";
args["amount"] = 5;
args["owner"] = "base58-account-id";

QByteArray json = QJsonDocument(args).toJson(QJsonDocument::Compact);
char* result = my_program_increment(json.constData());

// Parse the result
QJsonDocument resultDoc = QJsonDocument::fromJson(result);
bool success = resultDoc.object()["success"].toBool();
QString txHash = resultDoc.object()["tx_hash"].toString();

// Free the result string
my_program_free_string(result);
```

---

## Validation

### Generated Validation Functions

The `#[lez_program]` macro generates validation functions for instructions that have `signer` or `init` constraints. These run automatically before the handler.

**Generated function signature:**

```rust
pub fn __validate_{instruction_name}(
    accounts: &[AccountWithMetadata]
) -> Result<(), LezError>
```

**Checks performed (in order):**

1. **Signer checks** — for each `#[account(signer)]`:
   ```rust
   if !accounts[idx].is_authorized {
       return Err(LezError::Unauthorized {
           message: "Account '{name}' (index {idx}) must be a signer",
       });
   }
   ```

2. **Init checks** — for each `#[account(init)]`:
   ```rust
   if accounts[idx].account != Account::default() {
       return Err(LezError::AccountAlreadyInitialized {
           account_index: idx,
       });
   }
   ```

If an instruction has no `signer` or `init` constraints, no validation function is generated.

---

### Validation Helpers

The `lez-framework-core::validation` module provides helper functions used by generated code:

| Function | Signature | Description |
|----------|-----------|-------------|
| `validate_account_count` | `fn(actual: usize, expected: usize) -> Result<(), LezError>` | Check that the correct number of accounts was provided. Returns `AccountCountMismatch` on failure. |
| `validate_accounts` | `fn(account_count: usize, constraints: &[AccountConstraint]) -> Result<(), LezError>` | Validate accounts against constraints. Currently checks count; ownership, init state, signer, and PDA checks are delegated to the macro-generated code. |
| `is_default_account` | `fn(data: &[u8]) -> bool` | Check if account data is empty or all zeros. Used for `init` constraint. |
| `verify_owner` | `fn(account_owner: &[u8; 32], expected_owner: &[u8; 32], account_index: usize) -> Result<(), LezError>` | Verify account ownership. Returns `InvalidAccountOwner` on mismatch. |

---

## Serialization (lez-cli internals)

The CLI serializes instruction data in **risc0 serde format** (`Vec<u32>`) for submission to the zkVM guest. The format is:

```
[variant_index: u32, field1_words..., field2_words..., ...]
```

**Per-type encoding:**

| Type | Encoding |
|------|----------|
| `bool` | 1 word: `0` or `1` |
| `u8` | 1 word (zero-extended) |
| `u32` | 1 word |
| `u64` | 2 words (little-endian) |
| `u128` | 4 words (little-endian) |
| `program_id` / `[u32; 8]` | 8 words |
| `[u8; N]` | N words (each byte zero-extended to u32) |
| `String` | `[length: u32, bytes...]` (bytes padded to u32 words) |
| `Vec<T>` | `[length: u32, elements...]` |
| `Option<T>` | `[0]` for None; `[1, value...]` for Some |

This matches `risc0_zkvm::serde::to_vec` for enum struct variants.

---

## Prelude

Import the prelude for convenient access to common types:

```rust
use lez_framework::prelude::*;
```

This imports:
- `lez_program` (macro)
- `instruction` (macro)
- `LezOutput`
- `LezError`, `LezResult`
- `AccountConstraint`
- `Account`, `AccountWithMetadata`
- `AccountPostState`, `ChainedCall`, `PdaSeed`, `ProgramId`
- `BorshSerialize`, `BorshDeserialize`
