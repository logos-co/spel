# Types

Core types provided by `lez-framework-core` for building LEZ programs. These include return types, error types, account constraint metadata, and re-exported types from `nssa_core`.

For a guided walkthrough, see the [Tutorial](../tutorial.md). For other reference topics, see the [Reference Index](README.md).

---

## `LezOutput`

Return value from instruction handlers. Contains post-states and optional chained calls.

```rust
pub struct LezOutput {
    pub post_states: Vec<AccountPostState>,
    pub chained_calls: Vec<ChainedCall>,
}
```

### Methods

| Method | Signature | Description |
|--------|-----------|-------------|
| `states_only` | `fn states_only(post_states: Vec<AccountPostState>) -> Self` | Create output with only post-states and no chained calls. Most common case. |
| `with_chained_calls` | `fn with_chained_calls(post_states: Vec<AccountPostState>, chained_calls: Vec<ChainedCall>) -> Self` | Create output with both post-states and chained calls (cross-program invocation). |
| `empty` | `fn empty() -> Self` | Create an empty output (no states, no calls). |
| `into_parts` | `fn into_parts(self) -> (Vec<AccountPostState>, Vec<ChainedCall>)` | Destructure into the tuple form expected by `write_nssa_outputs_with_chained_call`. Used by generated code. |

### Example

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

## `LezResult`

Type alias for instruction handler return types:

```rust
pub type LezResult = Result<LezOutput, LezError>;
```

All `#[instruction]` functions must return `LezResult`.

---

## `LezError`

Structured error type for LEZ programs. Borsh-serializable for on-chain representation.

```rust
#[derive(Error, Debug, BorshSerialize, BorshDeserialize)]
pub enum LezError { /* ... */ }
```

### Variants

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

### Methods

| Method | Signature | Description |
|--------|-----------|-------------|
| `custom` | `fn custom(code: u32, message: impl Into<String>) -> Self` | Create a custom error. Numeric code starts at 6000. |
| `error_code` | `fn error_code(&self) -> u32` | Get the numeric error code for client-side handling. |

### Example

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

## `AccountConstraint`

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

## `InstructionMeta` / `AccountMeta` / `ArgMeta`

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

## Re-exported nssa_core types

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
