# Macros

Attribute macros and proc macros provided by the SPEL framework for defining LEZ programs, instructions, and generating IDL.

For a guided walkthrough, see the [Tutorial](../tutorial.md). For other reference topics, see the [Reference Index](README.md).

---

## `#[lez_program]`

**Crate:** `lez-framework-macros` (re-exported by `lez-framework`)

Attribute macro applied to a module. Transforms a module of `#[instruction]` functions into a complete LEZ guest binary with dispatch, validation, and IDL generation.

### Syntax

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

### Attributes

| Attribute | Type | Description |
|-----------|------|-------------|
| `instruction` | `"path::to::Enum"` | Optional. External instruction enum path. When set, the macro imports this type instead of generating its own `Instruction` enum. Used when the enum must be shared between on-chain and off-chain code (e.g., for correct borsh serialization in FFI). |

### What It Generates

1. **`Instruction` enum** — a `#[derive(Debug, Clone, Serialize, Deserialize)]` enum with one variant per `#[instruction]` function. Variant names are PascalCase conversions of function names. Only non-account parameters become enum fields. Skipped if `instruction = "..."` attribute is set.

2. **`fn main()`** — the zkVM guest entry point (gated by `#[cfg(not(test))]`). Reads `ProgramInput` from the host, dispatches to the correct handler via `match`, and writes outputs via `write_nssa_outputs_with_chained_call`. Account destructuring from `pre_states` is generated per-instruction.

3. **Validation functions** — one `__validate_{fn_name}()` function per instruction that has `signer` or `init` constraints. These run before the handler and return `LezError` on failure.

4. **`PROGRAM_IDL_JSON`** — a `pub const &str` containing the complete IDL as JSON. Available at compile time in any build target.

5. **`__program_idl()`** — a function returning a constructed `LezIdl` struct (with discriminators, execution metadata, etc.).

### Constraints

- The module **must have a body** (not `mod foo;`).
- The module must contain **at least one** `#[instruction]` function.
- Instruction functions **cannot have `self`** parameters.
- Account parameters must be typed `AccountWithMetadata` or `Vec<AccountWithMetadata>`.

### Example

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

## `#[instruction]`

**Crate:** `lez-framework-macros` (re-exported by `lez-framework`)

Marker attribute for functions inside an `#[lez_program]` module. This attribute is processed by `#[lez_program]` — it is a no-op when used standalone.

### Function Signature Requirements

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

### Account Constraint Attributes

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

### PDA Seeds

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

## `generate_idl!`

**Crate:** `lez-framework-macros` (re-exported by `lez-framework`)

Proc macro that reads a Rust source file at compile time, finds the `#[lez_program]` module, and generates a `fn main()` that prints the complete IDL as JSON.

### Syntax

```rust
lez_framework::generate_idl!("path/to/program.rs");
```

The path is resolved relative to `CARGO_MANIFEST_DIR`. The file must contain exactly one `#[lez_program]` module.

### What It Generates

A `fn main()` that:
1. Includes the source file via `include_str!` for cargo dependency tracking
2. Parses the embedded IDL JSON string
3. Pretty-prints it to stdout

### Usage

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
