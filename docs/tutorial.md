# Zero to Hero: Building Your First LEZ Program with SPEL

This tutorial walks you through building a **counter program** from scratch using the SPEL framework. By the end, you'll have a deployed on-chain program with increment and get_count instructions, and understand the full build-deploy-transact lifecycle.

We reference [logos-co/lez-multisig](https://github.com/logos-co/lez-multisig) as a real-world example throughout.

## Table of Contents

- [Prerequisites](#prerequisites)
- [Step 1: Scaffold the Project](#step-1-scaffold-the-project)
- [Step 2: Write the Program](#step-2-write-the-program)
- [Step 3: Set Up the CLI Wrapper](#step-3-set-up-the-cli-wrapper)
- [Step 4: Build and Generate IDL](#step-4-build-and-generate-idl)
- [Step 5: Deploy](#step-5-deploy)
- [Step 6: Interact with Your Program](#step-6-interact-with-your-program)
- [Step 7: Register in SPELbook](#step-7-register-in-spelbook)
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
- **spel** installed:

```bash
# From the SPEL repo
cargo install --path spel-cli   # installs as the `spel` binary
```

---

## Step 1: Scaffold the Project

Use `spel init` to create a new project:

```bash
spel init my-counter
cd my-counter
```

This generates:

```
my-counter/
├── Cargo.toml                      # Workspace
├── Makefile                        # build, idl, cli, deploy targets
├── spel.toml                       # [program] config — spel auto-discovers idl/binary
├── .gitignore
├── README.md
├── my_counter_core/                # (optional) shared host-side types
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

The scaffold includes a working example with placeholder `initialize` and `do_something` instructions. We'll replace these with our counter logic.

The `spel.toml` is the reason you'll be able to call `spel initialize …` later without `-i`/`-p` flags — `spel` walks up from the current directory until it finds one, then reads `[program].idl` and `[program].binary`. See the [CLI reference](reference/cli.md#configuration-speltoml) for the full format.

> **Real-world example:** The [lez-multisig](https://github.com/logos-co/lez-multisig) program follows this structure, with a `multisig_core` crate for genuinely-shared types (an instruction enum consumed by FFI clients) and a guest binary for the on-chain program.

---

## Step 2: Write the Program

The counter's state and instructions all live in one file: `methods/guest/src/bin/my_counter.rs`. The state struct carries `#[account_type]` so `spel inspect` can later decode it, and every handler returns `SpelOutput::execute(…)` — the macro reads the `#[account(…)]` constraints and generates the correct claim metadata for you.

Replace the scaffold's contents with:

```rust
#![no_main]

use spel_framework::prelude::*;

risc0_zkvm::guest::entry!(main);

/// The counter state stored on-chain.
///
/// `#[account_type]` registers this in the IDL so `spel inspect <PDA> --type CounterState`
/// can decode raw account bytes into readable JSON.
#[account_type]
#[derive(Debug, Clone, Default, BorshSerialize, BorshDeserialize)]
pub struct CounterState {
    /// The current count value.
    pub count: u64,
    /// The owner who can increment.
    pub owner: [u8; 32],
}

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
        mut counter: AccountWithMetadata,
        #[account(signer)]
        owner: AccountWithMetadata,
    ) -> SpelResult {
        let state = CounterState {
            count: 0,
            owner: *owner.account_id.value(),
        };
        let bytes = borsh::to_vec(&state).map_err(|e| SpelError::SerializationError {
            message: e.to_string(),
        })?;
        counter.account.data = bytes.try_into().unwrap();

        Ok(SpelOutput::execute(vec![counter, owner], vec![]))
    }

    /// Increment the counter by a given amount. Only the owner can increment.
    #[instruction]
    pub fn increment(
        #[account(mut, pda = literal("counter"))]
        mut counter: AccountWithMetadata,
        #[account(signer)]
        owner: AccountWithMetadata,
        amount: u64,
    ) -> SpelResult {
        let data: Vec<u8> = counter.account.data.clone().into();
        let mut state: CounterState = borsh::from_slice(&data).map_err(|e| {
            SpelError::DeserializationError {
                account_index: 0,
                message: e.to_string(),
            }
        })?;

        if *owner.account_id.value() != state.owner {
            return Err(SpelError::Unauthorized {
                message: "Only the owner can increment".to_string(),
            });
        }

        state.count = state.count.checked_add(amount).ok_or(SpelError::Overflow {
            operation: "counter increment".to_string(),
        })?;

        let bytes = borsh::to_vec(&state).map_err(|e| SpelError::SerializationError {
            message: e.to_string(),
        })?;
        counter.account.data = bytes.try_into().unwrap();

        Ok(SpelOutput::execute(vec![counter, owner], vec![]))
    }

    /// Get the current count value (read-only).
    ///
    /// The caller inspects the counter account after the transaction to read the count —
    /// see Step 6 for the `spel inspect … --type CounterState` flow.
    #[instruction]
    pub fn get_count(
        #[account(pda = literal("counter"))]
        counter: AccountWithMetadata,
    ) -> SpelResult {
        Ok(SpelOutput::execute(vec![counter], vec![]))
    }
}
```

A few things to note about this file:

- **`use spel_framework::prelude::*;`** is the only import. The prelude brings in `AccountWithMetadata`, `SpelResult`, `SpelError`, `SpelOutput`, `Claim`, `AccountPostState`, `AutoClaim`, and the Borsh derives. You don't need any `nssa_core::…` imports.
- **`#[account_type]` must live at file top level**, not inside `mod my_counter { … }`. The IDL generator only scans top-level items for this marker.
- **`mut counter: AccountWithMetadata`** — handlers that write to `counter.account.data` need `mut` on the parameter. Handlers that only read (like `get_count`) don't.
- **`SpelOutput::execute(vec![accounts], vec![chained_calls])`** is the idiomatic return. The macro inspects each account's `#[account(…)]` attributes and generates the correct claim — you never manually construct `AccountPostState::new_claimed(…, Claim::Authorized)` (that API is still available but deprecated, and it's noisier).
- The `my_counter_core/` crate ships empty in this tutorial. It exists for types that need to be *literally shared* across crates (e.g. an external `Instruction` enum consumed by an FFI client generated with `spel-client-gen`) — which our counter doesn't need.

Let's break down what's happening:

### Key concepts

1. **`#[lez_program]`** — wraps your module and generates the guest `main()`, instruction dispatch, validation helpers, and an IDL constant.

2. **`#[instruction]`** — marks each function as an on-chain instruction. The function name becomes a CLI subcommand (e.g., `increment` → `spel increment`).

3. **`#[account(init, pda = literal("counter"))]`** — the counter account is a PDA (Program Derived Address) derived from the string `"counter"` and the program ID. The `init` constraint means this account must not already exist yet, and implies writable.

4. **`#[account(signer)]`** — the owner must sign the transaction. The framework automatically checks `is_authorized` before your handler runs.

5. **`#[account(mut, pda = literal("counter"))]`** — the counter account is writable (its state will change) and is a PDA.

6. **`#[account_type]`** — placed on structs/enums stored in account `data`, this registers them in the IDL so `spel inspect --type …` can decode raw bytes. Must live at the **top level of the guest file**, not inside `mod my_counter { … }`.

7. **`SpelOutput::execute(vec![accounts], vec![chained_calls])`** — the idiomatic return from a handler. The macro derives the correct claim for each account from its `#[account(…)]` attributes, so you never write `AccountPostState::new_claimed(…, Claim::Authorized)` by hand.

> **Real-world example:** The lez-multisig program has instructions like `create` (with `init` + multi-seed PDA), `create_proposal`, and `approve` (with signer checks for members).

---

## Step 3: Set Up the CLI Wrapper

The scaffold already created `examples/src/bin/my_counter_cli.rs`:

```rust
#[tokio::main]
async fn main() {
    spel::run().await;
}
```

That's it — three lines. The CLI reads the IDL at runtime and auto-generates subcommands for every instruction in your program. (The crate/binary is named `spel`, so the lib module is `spel::`, not `spel_cli::`.)

---

## Step 4: Build and Generate IDL

```bash
# Build the guest binary (compiles for RISC Zero zkVM)
make build
```

Then generate the IDL. **Use `spel generate-idl`**, not `make idl` — the Makefile target goes through a proc-macro path that does not pick up `#[account_type]` markers, so `spel inspect --type CounterState` would silently have nothing to decode:

```bash
spel generate-idl methods/guest/src/bin/my_counter.rs > my-counter-idl.json
```

> **Why two paths exist:** the scaffold's `make idl` runs a host-side `generate_idl` binary built from a proc macro. The proc macro emits instruction metadata correctly but currently skips file-level `#[account_type]` structs. The `spel generate-idl` CLI subcommand uses a second, fuller generator that includes them. Track [this issue](https://github.com/logos-co/spel/issues) for when the two paths merge; until then, prefer the CLI.

Let's look at what the generator writes to `my-counter-idl.json`:

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
          "pda": { "seeds": [{ "kind": "const", "value": "counter" }] }
        },
        { "name": "owner", "writable": false, "signer": true, "init": false }
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
          "pda": { "seeds": [{ "kind": "const", "value": "counter" }] }
        },
        { "name": "owner", "writable": false, "signer": true, "init": false }
      ],
      "args": [{ "name": "amount", "type": "u64" }]
    },
    {
      "name": "get_count",
      "accounts": [
        {
          "name": "counter",
          "writable": false,
          "signer": false,
          "init": false,
          "pda": { "seeds": [{ "kind": "const", "value": "counter" }] }
        }
      ],
      "args": []
    }
  ],
  "accounts": [
    {
      "name": "CounterState",
      "type": {
        "kind": "struct",
        "fields": [
          { "name": "count", "type": "u64" },
          { "name": "owner", "type": { "array": ["u8", 32] } }
        ]
      }
    }
  ],
  "errors": [],
  "types": []
}
```

Notice:
- The `counter` account has `"pda"` with a `"const"` seed — the CLI will compute this automatically.
- The `owner` account has `"signer": true` — the CLI will handle wallet signing.
- `init: true` on the first instruction's counter account — the CLI knows this is a new account.
- `amount` is the only instruction argument — everything else is an account.
- `accounts` at the top level lists every `#[account_type]` struct with its field schema. `spel inspect --type <Name>` uses this to decode raw account bytes.

---

## Step 5: Deploy

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

## Step 6: Interact with Your Program

All commands below assume you're inside the project directory, so `spel` picks up `spel.toml` and resolves the IDL and binary automatically. Without `spel.toml` you'd need to pass `-i <IDL> -p <BIN> --` before the instruction — see [Without `spel.toml`](#without-speltoml) below.

### See available commands

```bash
spel --help
```

Output:

```
🔧 my_counter v0.1.0 — IDL-driven CLI

USAGE:
  spel <COMMAND> [ARGS]                  (with spel.toml)
  spel [OPTIONS] -- <COMMAND> [ARGS]     (without spel.toml)

COMMANDS:
  inspect <FILE> [FILE...]   Print ProgramId for ELF binary(ies)
  generate-idl [PATH]        Generate IDL JSON
  idl                        Print the loaded IDL
  initialize           --owner <BASE58|HEX>
  increment            --amount <NUMBER> --owner <BASE58|HEX>
  get-count
```

Notice how the CLI auto-generated commands from your IDL:
- PDA accounts (`counter`) are not listed as arguments — they're computed automatically.
- Instruction arguments (`amount`) are typed.
- Account arguments get a flag named after the account itself (`owner` → `--owner`). Account flags expect base58 or 64-character hex.

### Initialize the counter

```bash
spel initialize --owner <YOUR_SIGNER_BASE58>
```

The CLI will:
1. Resolve the program binary from `spel.toml` and derive the ProgramId.
2. Compute the `counter` PDA from the seed `"counter"` + ProgramId.
3. Fetch the nonce for the signer account from your wallet.
4. Build, sign, and submit the transaction.
5. Wait for confirmation.

### Increment the counter

```bash
spel increment --amount 5 --owner <YOUR_SIGNER_BASE58>
```

### Read the count back

The counter's state lives in the PDA's account data. Compute the PDA address and decode it with `spel inspect --type`:

```bash
COUNTER_PDA=$(spel pda counter)
spel inspect "$COUNTER_PDA" --type CounterState
```

Typical output:

```
Account: DzEcGdM7RqkGpG6QtQhoVhMmiSoVrqB4pL3AzZCtoMvZ
Data:    40 bytes
Hex:     0500000000000000cdc32169...b905ded1c169a66aca040a277584bdbf13

{
  "count": "5",
  "owner": "cdc32169ea799edca123080eb858b4b905ded1c169a66aca040a277584bdbf13"
}
```

The decode works because `CounterState` is annotated with `#[account_type]` in your program source, which puts its field schema in the IDL. **Be sure to use `spel generate-idl` (not `make idl`) to produce the IDL** — `make idl` uses a proc-macro path that currently skips these types, and `spel inspect --type CounterState` would then fail with "type not found."

> **Note:** If `spel inspect` inside the project complains that `--type` is required even when inspecting an ELF binary, run it from a directory without a `spel.toml` (e.g. `cd /tmp && spel inspect /full/path/to/my_counter.bin`). The binary-vs-account mode selector is currently ambiguous when a spel.toml provides a default IDL.

### Dry run (no submission)

`--dry-run` resolves the whole transaction (PDAs, accounts, signer nonces, serialized data) and prints it without submitting:

```bash
spel --dry-run increment --amount 5 --owner <BASE58>
```

Typical text output:

```
=== Dry Run ===
Program ID: 3930000032920100...
Instruction: increment

Accounts:
  PDA counter → 4Lp3gkH... [writable]
    seeds: [program_id, "counter"]
  owner → 0xccdd...00 [signer]

Arguments:
  --amount 5

Instruction data: 0x010000000500000000000000

Signers:
  owner: nonce=42
================
Dry run complete — not submitted.
```

The `seeds: […]` line is rendered during PDA resolution and is only shown in dry-run and live-transaction output — not by the standalone `spel pda` subcommand, which prints only the address.

For machine-readable output (e.g. in CI golden tests or `jq` pipelines), use `--dry-run=json`:

```bash
spel --dry-run=json increment --amount 5 --owner <BASE58> | jq .
```

In JSON mode all human preamble is suppressed — only the JSON document goes to stdout.

### Pass a raw program ID instead of a binary

`--program` accepts three forms: a name from `spel.toml`, a 64-character hex program ID, or a file path to the ELF binary. Using the hex ID skips loading the binary and is faster:

```bash
spel --idl my-counter-idl.json --program <64-CHAR-HEX> -- \
  increment --amount 10 --owner <YOUR_SIGNER_BASE58>
```

### Compute the counter PDA manually

```bash
spel pda counter                                      # with spel.toml
spel --idl my-counter-idl.json --program <HEX> pda counter
```

This prints the base58 AccountId of the counter PDA and nothing else. If you want to see the seed inputs that were used, run the same instruction in `--dry-run` mode (see above).

### Without `spel.toml`

When invoking `spel` from a directory without a `spel.toml`, global options (`--idl`, `--program`, `--dry-run`) come **before** a `--` separator; the instruction and its `--arg` flags go after:

```bash
spel --idl my-counter-idl.json --program ./my_counter.bin -- \
  increment --amount 5 --owner <BASE58>
```

Without the `--`, the first `--amount` would be swallowed by the global-flag parser and the command would error out. The `spel.toml`-based invocations above don't need the separator because no global flags are in play.

---

## Step 7: Register in SPELbook

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
pub fn __validate_initialize(accounts: &[AccountWithMetadata]) -> Result<(), SpelError> {
    // init check: counter must be default
    if accounts[0].account != Account::default() {
        return Err(SpelError::AccountAlreadyInitialized { account_index: 0 });
    }
    // signer check: owner must be authorized
    if !accounts[1].is_authorized {
        return Err(SpelError::Unauthorized {
            message: "Account 'owner' (index 1) must be a signer".to_string(),
        });
    }
    Ok(())
}
```

**4. IDL Constants**

```rust
pub const PROGRAM_IDL_JSON: &str = r#"{"version":"0.1.0","name":"my_counter",...}"#;
pub fn __program_idl() -> SpelIdl { ... }
```

### Account Validation

The framework generates automatic validation checks that run before your handler:

| Attribute | Check | Error |
|-----------|-------|-------|
| `signer` | `is_authorized == true` | `SpelError::Unauthorized` |
| `init` | `account == Account::default()` | `SpelError::AccountAlreadyInitialized` |

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

The IDL will include `"instruction_type": "multisig_core::Instruction"`, which tells `spel-client-gen` to import and use the shared type in generated FFI code.

### Variable-Length Accounts

Some instructions need a variable number of accounts. Use `Vec<AccountWithMetadata>`:

```rust
#[instruction]
pub fn multi_approve(
    #[account(mut, pda = literal("state"))]
    state: AccountWithMetadata,
    #[account(signer)]
    members: Vec<AccountWithMetadata>,
) -> SpelResult {
    // members can contain 0, 1, 2, ... accounts
    for member in &members {
        // validate each member
    }
    // ...
}
```

In the CLI, pass rest accounts as a comma-separated list:

```bash
spel multi-approve --members "addr1,addr2,addr3"
```

Rest accounts are always optional (0 entries is valid). The macro splits `pre_states` into fixed accounts (before the rest) and the variadic tail.

### Chained Calls

Instructions can trigger calls to other programs by returning `ChainedCall`s. The second argument to `SpelOutput::execute(…)` is a `Vec<ChainedCall>`:

```rust
#[instruction]
pub fn transfer_and_notify(
    #[account(mut)]
    from: AccountWithMetadata,
    #[account(mut)]
    to: AccountWithMetadata,
    #[account(signer)]
    signer: AccountWithMetadata,
    amount: u64,
) -> SpelResult {
    // ... transfer logic (mutate from.account.data / to.account.data) ...

    let chained_call = ChainedCall {
        // ... target program and instruction data ...
    };

    Ok(SpelOutput::execute(vec![from, to, signer], vec![chained_call]))
}
```

### Client Code Generation

For integrating LEZ programs into applications (e.g., a C++/Qt desktop app), use `spel-client-gen` to generate typed bindings:

```bash
spel-client-gen --idl my-counter-idl.json --out-dir generated/
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
- **Generate client code** with `spel-client-gen` for integrating your program into applications
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
