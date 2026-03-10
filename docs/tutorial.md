# Zero to Hero: Building Your First LEZ Program with SPEL

This tutorial walks you through building a **counter program** from scratch using the SPEL framework. By the end, you'll have a deployed on-chain program with increment and get_count instructions, and understand the full build-deploy-transact lifecycle.

We reference [logos-co/lez-multisig](https://github.com/logos-co/lez-multisig) as a real-world example throughout.

## Table of Contents

- [Prerequisites](#prerequisites)
- [Step 1: Scaffold the Project](#step-1-scaffold-the-project)
- [Step 2: Define Your State](#step-2-define-your-state)
- [Step 3: Write Instructions](#step-3-write-instructions)
- [Step 4: Set Up IDL Generation](#step-4-set-up-idl-generation)
- [Step 5: Set Up the CLI Wrapper](#step-5-set-up-the-cli-wrapper)
- [Step 6: Build and Generate IDL](#step-6-build-and-generate-idl)
- [Step 7: Deploy](#step-7-deploy)
- [Step 8: Interact with Your Program](#step-8-interact-with-your-program)
- [Step 9: Register in SPELbook](#step-9-register-in-spelbook)
- [Concepts Deep Dive](#concepts-deep-dive)
  - [How the Macro Works](#how-the-macro-works)
  - [Account Validation](#account-validation)
  - [PDA Derivation](#pda-derivation)
  - [External Instruction Enums](#external-instruction-enums)
  - [Variable-Length Accounts](#variable-length-accounts)
  - [Chained Calls](#chained-calls)
  - [Client Code Generation](#client-code-generation)
- [Next Steps](#next-steps)

---

## Prerequisites

Before you begin, make sure you have:

- **Rust** with the nightly toolchain (`rustup install nightly`)
- **RISC Zero toolchain** — [install instructions](https://dev.risczero.com/api/zkvm/install)
- **NSSA wallet CLI** (`wallet` binary) — for account creation and transaction signing
- A **running sequencer** — the network node that accepts transactions
- **lez-cli** installed:

```bash
# From the SPEL repo
cargo install --path lez-cli
```

---

## Step 1: Scaffold the Project

Use `lez-cli init` to create a new project:

```bash
lez-cli init my-counter
cd my-counter
```

This generates:

```
my-counter/
├── Cargo.toml                      # Workspace
├── Makefile                        # build, idl, cli, deploy targets
├── .gitignore
├── README.md
├── my_counter_core/                # Shared types
│   ├── Cargo.toml
│   └── src/lib.rs
├── methods/
│   ├── Cargo.toml
│   ├── build.rs
│   ├── src/lib.rs
│   └── guest/                      # On-chain program
│       ├── Cargo.toml
│       └── src/bin/my_counter.rs   # ← Your program logic goes here
└── examples/
    ├── Cargo.toml
    └── src/bin/
        ├── generate_idl.rs         # IDL generator (one-liner)
        └── my_counter_cli.rs       # CLI wrapper (three lines)
```

The scaffold includes a working example with `initialize` and `do_something` instructions. We'll replace these with our counter logic.

> **Real-world example:** The [lez-multisig](https://github.com/logos-co/lez-multisig) program follows this exact structure, with a `multisig_core` crate for shared types and a guest binary for the on-chain program.

---

## Step 2: Define Your State

Edit `my_counter_core/src/lib.rs` to define your counter state:

```rust
use serde::{Deserialize, Serialize};

/// The counter state stored on-chain.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CounterState {
    /// The current count value.
    pub count: u64,
    /// The owner who can increment.
    pub owner: [u8; 32],
}
```

This state struct lives in the `_core` crate so it can be shared between the on-chain guest program and any off-chain tools. It needs to be serializable since it's stored in account data.

> **Real-world example:** In lez-multisig, the `multisig_core` crate defines the `MultisigState` struct with fields like `threshold`, `members`, and `proposal_index`.

---

## Step 3: Write Instructions

This is the core of your program. Edit `methods/guest/src/bin/my_counter.rs`:

```rust
#![no_main]

use nssa_core::account::AccountWithMetadata;
use nssa_core::program::AccountPostState;
use lez_framework::prelude::*;

risc0_zkvm::guest::entry!(main);

#[lez_program]
mod my_counter {
    #[allow(unused_imports)]
    use super::*;

    /// Initialize the counter with an owner.
    ///
    /// Creates a new PDA account derived from the literal seed "counter".
    /// The owner is the signer who can later increment the counter.
    #[instruction]
    pub fn initialize(
        #[account(init, pda = literal("counter"))]
        counter: AccountWithMetadata,
        #[account(signer)]
        owner: AccountWithMetadata,
    ) -> LezResult {
        // Serialize initial state into the account data
        let state = my_counter_core::CounterState {
            count: 0,
            owner: *owner.account_id.value(),
        };
        let data = borsh::to_vec(&state).map_err(|e| LezError::SerializationError {
            message: e.to_string(),
        })?;

        let mut new_account = counter.account.clone();
        new_account.data = data.try_into().unwrap();

        Ok(LezOutput::states_only(vec![
            AccountPostState::new_claimed(new_account),
            AccountPostState::new(owner.account.clone()),
        ]))
    }

    /// Increment the counter by a given amount.
    ///
    /// Only the owner can increment. The counter account is a PDA
    /// derived from the literal seed "counter".
    #[instruction]
    pub fn increment(
        #[account(mut, pda = literal("counter"))]
        counter: AccountWithMetadata,
        #[account(signer)]
        owner: AccountWithMetadata,
        amount: u64,
    ) -> LezResult {
        // Deserialize current state
        let mut state: my_counter_core::CounterState =
            borsh::from_slice(&counter.account.data).map_err(|e| {
                LezError::DeserializationError {
                    account_index: 0,
                    message: e.to_string(),
                }
            })?;

        // Verify the signer is the owner
        if *owner.account_id.value() != state.owner {
            return Err(LezError::Unauthorized {
                message: "Only the owner can increment".to_string(),
            });
        }

        // Increment with overflow check
        state.count = state.count.checked_add(amount).ok_or(LezError::Overflow {
            operation: "counter increment".to_string(),
        })?;

        // Serialize updated state
        let data = borsh::to_vec(&state).map_err(|e| LezError::SerializationError {
            message: e.to_string(),
        })?;
        let mut updated = counter.account.clone();
        updated.data = data.try_into().unwrap();

        Ok(LezOutput::states_only(vec![
            AccountPostState::new(updated),
            AccountPostState::new(owner.account.clone()),
        ]))
    }

    /// Get the current count value.
    ///
    /// This is a read-only instruction — it returns the state unchanged.
    /// The count is embedded in the transaction output for off-chain reading.
    #[instruction]
    pub fn get_count(
        #[account(pda = literal("counter"))]
        counter: AccountWithMetadata,
    ) -> LezResult {
        let state: my_counter_core::CounterState =
            borsh::from_slice(&counter.account.data).map_err(|e| {
                LezError::DeserializationError {
                    account_index: 0,
                    message: e.to_string(),
                }
            })?;

        // Return account unchanged (read-only)
        Ok(LezOutput::states_only(vec![
            AccountPostState::new(counter.account.clone()),
        ]))
    }
}
```

Let's break down what's happening:

### Key concepts

1. **`#[lez_program]`** — wraps your module and generates the guest `main()`, instruction dispatch, and IDL.

2. **`#[instruction]`** — marks each function as an on-chain instruction. The function name becomes a CLI subcommand (e.g., `increment` → `lez-cli increment`).

3. **`#[account(init, pda = literal("counter"))]`** — the counter account is a PDA (Program Derived Address) derived from the string `"counter"` and the program ID. The `init` constraint means this account must not already exist.

4. **`#[account(signer)]`** — the owner must sign the transaction. The framework automatically checks `is_authorized` before your handler runs.

5. **`#[account(mut, pda = literal("counter"))]`** — the counter account is writable (its state will change) and is a PDA.

6. **`AccountPostState::new_claimed(account)`** — claims a new account (used with `init`).

7. **`AccountPostState::new(account)`** — updates an existing account.

> **Real-world example:** The lez-multisig program has instructions like `create` (with `init` + multi-seed PDA), `create_proposal`, and `approve` (with signer checks for members).

---

## Step 4: Set Up IDL Generation

The scaffold already created `examples/src/bin/generate_idl.rs`. Make sure it points to your program:

```rust
/// Generate IDL JSON for the my-counter program.
lez_framework::generate_idl!("../methods/guest/src/bin/my_counter.rs");
```

This reads your program source at compile time and generates a `main()` that prints the complete IDL as JSON. The IDL describes all instructions, their accounts, arguments, PDA seeds, and types.

---

## Step 5: Set Up the CLI Wrapper

The scaffold already created `examples/src/bin/my_counter_cli.rs`:

```rust
#[tokio::main]
async fn main() {
    lez_cli::run().await;
}
```

That's it — three lines. The CLI reads the IDL at runtime and auto-generates subcommands for every instruction in your program.

---

## Step 6: Build and Generate IDL

```bash
# Build the guest binary (compiles for RISC Zero zkVM)
make build

# Generate the IDL from your program annotations
make idl
```

The `make idl` command runs `cargo run --bin generate_idl` and writes `my-counter-idl.json`. Let's look at what it generates:

```json
{
  "version": "0.1.0",
  "name": "my_counter",
  "instructions": [
    {
      "name": "initialize",
      "accounts": [
        {
          "name": "counter",
          "writable": true,
          "signer": false,
          "init": true,
          "pda": {
            "seeds": [
              { "kind": "const", "value": "counter" }
            ]
          }
        },
        {
          "name": "owner",
          "writable": false,
          "signer": true,
          "init": false
        }
      ],
      "args": []
    },
    {
      "name": "increment",
      "accounts": [
        {
          "name": "counter",
          "writable": true,
          "signer": false,
          "init": false,
          "pda": {
            "seeds": [
              { "kind": "const", "value": "counter" }
            ]
          }
        },
        {
          "name": "owner",
          "writable": false,
          "signer": true,
          "init": false
        }
      ],
      "args": [
        { "name": "amount", "type": "u64" }
      ]
    },
    {
      "name": "get_count",
      "accounts": [
        {
          "name": "counter",
          "writable": false,
          "signer": false,
          "init": false,
          "pda": {
            "seeds": [
              { "kind": "const", "value": "counter" }
            ]
          }
        }
      ],
      "args": []
    }
  ]
}
```

Notice:
- The `counter` account has `"pda"` with a `"const"` seed — the CLI will compute this automatically
- The `owner` account has `"signer": true` — the CLI will handle wallet signing
- `init: true` on the first instruction's counter account — the CLI knows this is a new account
- `amount` is the only instruction argument — everything else is an account

---

## Step 7: Deploy

First, set up your accounts and deploy the program:

```bash
# Create a signer account in your wallet
make setup

# Deploy the program binary to the sequencer
make deploy

# Verify the deployment — prints the ProgramId
make inspect
```

The `make inspect` command shows your program's ID:

```
📦 methods/guest/target/riscv32im-risc0-zkvm-elf/docker/my_counter.bin
   ProgramId (decimal): 12345,67890,...
   ProgramId (hex):     00003039,00010932,...
   ImageID (hex bytes): 3930000032920100...
```

Save the hex ImageID — you'll need it for CLI commands.

---

## Step 8: Interact with Your Program

### See available commands

```bash
make cli ARGS="--help"
```

Output:

```
🔧 my_counter v0.1.0 — IDL-driven CLI

USAGE:
  my_counter_cli [OPTIONS] <COMMAND> [ARGS]

COMMANDS:
  inspect <FILE> [FILE...]   Print ProgramId for ELF binary(ies)
  idl                        Print IDL information
  initialize                 --owner-account <BASE58|HEX>
  increment                  --amount <NUMBER> --owner-account <BASE58|HEX>
  get-count
```

Notice how the CLI auto-generated commands from your IDL:
- PDA accounts (`counter`) are not listed as arguments — they're computed automatically
- Instruction arguments (`amount`) are typed
- Account arguments (`owner`) expect base58 or hex

### Initialize the counter

```bash
make cli ARGS="-p methods/guest/target/riscv32im-risc0-zkvm-elf/docker/my_counter.bin \
  initialize --owner-account <YOUR_SIGNER_BASE58>"
```

The CLI will:
1. Load the program binary to get the ProgramId
2. Compute the `counter` PDA from the seed `"counter"` + ProgramId
3. Fetch the nonce for the signer account from the wallet
4. Build and sign the transaction
5. Submit to the sequencer
6. Wait for confirmation

### Increment the counter

```bash
make cli ARGS="-p methods/guest/target/riscv32im-risc0-zkvm-elf/docker/my_counter.bin \
  increment --amount 5 --owner-account <YOUR_SIGNER_BASE58>"
```

### Use `--program-id` to skip loading the binary

If you know the program ID, you can skip loading the binary:

```bash
make cli ARGS="--program-id <64-CHAR-HEX> \
  increment --amount 10 --owner-account <YOUR_SIGNER_BASE58>"
```

### Dry run (no submission)

Add `--dry-run` to see what would be submitted without actually sending:

```bash
make cli ARGS="--dry-run -p methods/guest/target/riscv32im-risc0-zkvm-elf/docker/my_counter.bin \
  increment --amount 5 --owner-account <YOUR_SIGNER_BASE58>"
```

This prints the parsed arguments, serialized instruction data, and account IDs without submitting.

### Compute the counter PDA manually

```bash
make cli ARGS="--program-id <64-CHAR-HEX> pda counter"
```

This prints the base58 AccountId of the counter PDA.

---

## Step 9: Register in SPELbook

TODO: verify — SPELbook registration process is not yet documented in the codebase.

Once your program is deployed and working, you can register it in SPELbook to make it discoverable by other developers and programs.

---

## Concepts Deep Dive

### How the Macro Works

The `#[lez_program]` macro transforms your module at compile time. Here's what it generates for our counter program:

**1. Instruction Enum**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Instruction {
    Initialize,
    Increment { amount: u64 },
    GetCount,
}
```

Variant names are PascalCase conversions of function names. Only non-account parameters become fields.

**2. Main Function** (cfg-gated: only in zkVM guest builds, not in tests)

```rust
fn main() {
    // Read inputs from zkVM host
    let (ProgramInput { pre_states, instruction }, instruction_words)
        = read_nssa_inputs::<Instruction>();

    // Dispatch to handler
    let result = match instruction {
        Instruction::Initialize => {
            let [counter, owner] = ...;  // destructure pre_states
            my_counter::__validate_initialize(&[counter.clone(), owner.clone()])?;
            my_counter::initialize(counter, owner)
        }
        Instruction::Increment { amount } => {
            let [counter, owner] = ...;
            my_counter::__validate_increment(&[counter.clone(), owner.clone()])?;
            my_counter::increment(counter, owner, amount)
        }
        Instruction::GetCount => {
            let [counter] = ...;
            my_counter::get_count(counter)
        }
    };

    // Write outputs
    write_nssa_outputs_with_chained_call(...);
}
```

**3. Validation Functions**

```rust
pub fn __validate_initialize(accounts: &[AccountWithMetadata]) -> Result<(), LezError> {
    // init check: counter must be default
    if accounts[0].account != Account::default() {
        return Err(LezError::AccountAlreadyInitialized { account_index: 0 });
    }
    // signer check: owner must be authorized
    if !accounts[1].is_authorized {
        return Err(LezError::Unauthorized {
            message: "Account 'owner' (index 1) must be a signer".to_string(),
        });
    }
    Ok(())
}
```

**4. IDL Constants**

```rust
pub const PROGRAM_IDL_JSON: &str = r#"{"version":"0.1.0","name":"my_counter",...}"#;
pub fn __program_idl() -> LezIdl { ... }
```

### Account Validation

The framework generates automatic validation checks that run before your handler:

| Attribute | Check | Error |
|-----------|-------|-------|
| `signer` | `is_authorized == true` | `LezError::Unauthorized` |
| `init` | `account == Account::default()` | `LezError::AccountAlreadyInitialized` |

These checks are generated per-instruction. If an instruction has no `signer` or `init` accounts, no validation function is generated.

Validation runs in declaration order: if both `init` and `signer` checks fail, the `init` check (which comes first in the generated code) will be the reported error.

### PDA Derivation

PDAs (Program Derived Addresses) are deterministic account addresses computed from a program ID and seeds. They allow programs to "own" accounts without needing a private key.

**How it works:**

1. Each seed is converted to 32 bytes (zero-padded for strings)
2. Single seed: used directly as `PdaSeed`
3. Multiple seeds: combined via `SHA-256(seed1_32 || seed2_32 || ...)`
4. Final address: `AccountId::from((program_id, &PdaSeed::new(combined)))`

**Seed types:**

```rust
// Constant string — always the same
#[account(pda = literal("counter"))]

// Another account's ID — PDA depends on which account is passed
#[account(pda = account("user"))]

// Instruction argument — PDA depends on the argument value
#[account(pda = arg("create_key"))]

// Multiple seeds — combined via SHA-256
#[account(pda = [literal("vault"), account("user")])]
#[account(pda = [literal("proposal"), arg("proposal_index")])]
```

> **Real-world example:** In lez-multisig, the multisig state PDA uses two seeds:
> ```rust
> #[account(init, pda = [literal("multisig_state__"), arg("create_key")])]
> ```
> This allows multiple independent multisig instances, each with a unique `create_key`.

### External Instruction Enums

For programs where the `Instruction` enum needs to be shared between the on-chain guest and off-chain tools (e.g., for FFI code generation with correct borsh serialization), you can define it in a shared core crate:

```rust
// In multisig_core/src/lib.rs
#[derive(Debug, Clone, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub enum Instruction {
    Create { create_key: [u8; 32], threshold: u64, members: Vec<[u8; 32]> },
    Approve { proposal_id: u64 },
    // ...
}
```

Then reference it in the program:

```rust
#[lez_program(instruction = "multisig_core::Instruction")]
mod multisig {
    // The macro uses multisig_core::Instruction instead of generating one
    // ...
}
```

The IDL will include `"instruction_type": "multisig_core::Instruction"`, which tells `lez-client-gen` to import and use the shared type in generated FFI code.

### Variable-Length Accounts

Some instructions need a variable number of accounts. Use `Vec<AccountWithMetadata>`:

```rust
#[instruction]
pub fn multi_approve(
    #[account(mut, pda = literal("state"))]
    state: AccountWithMetadata,
    #[account(signer)]
    members: Vec<AccountWithMetadata>,
) -> LezResult {
    // members can contain 0, 1, 2, ... accounts
    for member in &members {
        // validate each member
    }
    // ...
}
```

In the CLI, pass rest accounts as a comma-separated list:

```bash
lez-cli ... multi-approve --members-account "addr1,addr2,addr3"
```

Rest accounts are always optional (0 entries is valid). The macro splits `pre_states` into fixed accounts (before the rest) and the variadic tail.

### Chained Calls

Instructions can trigger calls to other programs by returning `ChainedCall`s:

```rust
#[instruction]
pub fn transfer_and_notify(
    // ... accounts ...
    amount: u64,
) -> LezResult {
    // ... transfer logic ...

    let chained_call = ChainedCall {
        // ... target program and instruction data ...
    };

    Ok(LezOutput::with_chained_calls(
        vec![/* post states */],
        vec![chained_call],
    ))
}
```

### Client Code Generation

For integrating LEZ programs into applications (e.g., a C++/Qt desktop app), use `lez-client-gen` to generate typed bindings:

```bash
lez-client-gen --idl my-counter-idl.json --out-dir generated/
```

This produces three files:

1. **`my_counter_client.rs`** — Async Rust client with typed methods
2. **`my_counter_ffi.rs`** — C FFI (`extern "C"` functions accepting JSON)
3. **`my_counter.h`** — C header file

**Using the C FFI from C++/Qt:**

```cpp
#include "my_counter.h"
#include <QJsonDocument>
#include <QJsonObject>

// Call the increment instruction
QJsonObject args;
args["wallet_path"] = "/path/to/wallet";
args["program_id_hex"] = "abc123...";
args["amount"] = 5;
args["owner"] = "base58-account-id";

QByteArray json = QJsonDocument(args).toJson();
char* result = my_counter_increment(json.constData());

// Parse result
QJsonDocument resultDoc = QJsonDocument::fromJson(result);
bool success = resultDoc.object()["success"].toBool();
QString txHash = resultDoc.object()["tx_hash"].toString();

my_counter_free_string(result);
```

Build the FFI as a shared library:

```bash
cargo build --release --lib
# Produces libmy_counter.so / libmy_counter.dylib
```

---

## Next Steps

- **Read the [Reference](reference/README.md)** for complete API documentation
- **Study [lez-multisig](https://github.com/logos-co/lez-multisig)** for a production-quality example with multi-seed PDAs, variable-length accounts, and external instruction enums
- **Generate client code** with `lez-client-gen` for integrating your program into applications
- **Write tests** — the `#[cfg(not(test))]` gate on `main()` means your handlers are directly callable in host-side tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initialize() {
        let acc = AccountWithMetadata {
            account_id: AccountId::new([0u8; 32]),
            account: Account::default(),
            is_authorized: true,
        };
        let result = my_counter::initialize(acc.clone(), acc.clone());
        assert!(result.is_ok());
    }
}
```
