---
name: spel
description: "Build, deploy, and interact with LEZ on-chain programs using the SPEL framework (logos-co/spel). Use when: (1) creating a new LEZ program or SPEL project, (2) writing #[lez_program] instructions with account constraints, PDA derivation, or signer checks, (3) generating IDL from program source, (4) using lez-cli to deploy, inspect, call instructions, or compute PDAs, (5) generating typed Rust/C FFI client code with lez-client-gen, (6) debugging SPEL macro output, account validation, or PDA mismatches, (7) registering a program in SPELbook, or (8) any mention of lez-cli, lez_framework, lez-client-gen, #[lez_program], #[instruction], LezOutput, LezError, LezResult, generate_idl!, AccountPostState, PdaSeed, or RISC Zero zkVM guest programs in the LEZ/NSSA ecosystem."
---

# SPEL Framework

SPEL is a Rust framework for building on-chain programs that run on LEZ (the NSSA execution layer). It provides attribute macros (`#[lez_program]`, `#[instruction]`) that generate zkVM guest binaries, instruction dispatch, account validation, IDL, and CLI tooling from annotated Rust modules.

Programs compile to RISC Zero zkVM guests. The framework auto-generates an `Instruction` enum, `main()` dispatch, validation functions, and a full IDL (JSON). The `lez-cli` reads the IDL at runtime to provide a complete CLI for any program.

## References

Read these files as needed:

- **[references/quickstart.md](references/quickstart.md)** — Full scaffold-to-deploy workflow with real commands. Read when building a new program or recalling the build/deploy/call sequence.
- **[references/gotchas.md](references/gotchas.md)** — Hard-won lessons and common mistakes. Read before writing or debugging any SPEL program.
- **[references/cli-ref.md](references/cli-ref.md)** — CLI cheatsheet for `lez-cli` and `lez-client-gen`. Read when constructing CLI commands or checking flag names.

## Core Workflow

1. **Scaffold** — `lez-cli init <name>` creates workspace with guest binary, core crate, IDL generator, CLI wrapper, and Makefile.
2. **Define state** — Put shared types in `{name}_core/src/lib.rs` (must derive `Serialize`, `Deserialize`).
3. **Write instructions** — In `methods/guest/src/bin/{name}.rs`, use `#[lez_program]` + `#[instruction]` with account constraints (`signer`, `init`, `mut`, `pda`, `owner`).
4. **Build** — `make build` compiles the RISC Zero zkVM guest binary.
5. **Generate IDL** — `make idl` runs `generate_idl!` macro to emit `{name}-idl.json`.
6. **Deploy** — `make setup && make deploy` creates signer account and deploys binary.
7. **Call instructions** — `make cli ARGS="..."` with IDL-driven subcommands.
8. **Generate client code** — `lez-client-gen --idl <file> --out-dir <dir>` for Rust client + C FFI + C header.

## Key Instruction Patterns

### Account constraints

```rust
#[account(signer)]              // must sign transaction
#[account(init)]                // new account, must be default — implies mut
#[account(mut)]                 // writable
#[account(pda = literal("x"))] // PDA from constant seed
#[account(pda = account("u"))] // PDA from another account's ID
#[account(pda = arg("key"))]   // PDA from instruction argument
#[account(pda = [literal("vault"), account("user")])]  // multi-seed PDA
#[account(owner = PROGRAM_ID)] // ownership check
```

### Return values

```rust
// New account (init)
AccountPostState::new_claimed(account)

// Updated existing account
AccountPostState::new(account)

// Return with no chained calls (most common)
Ok(LezOutput::states_only(vec![...]))

// Return with cross-program calls
Ok(LezOutput::with_chained_calls(vec![...], vec![chained_call]))
```

### Variable-length accounts

```rust
#[account(signer)]
members: Vec<AccountWithMetadata>  // variadic trailing accounts
```

### External instruction enum (for shared FFI types)

```rust
#[lez_program(instruction = "my_core::Instruction")]
mod my_program { ... }
```

## Critical Rules

1. **Never edit IDL JSON by hand** — always regenerate via `generate_idl!` / `make idl`.
2. **PDA accounts are auto-computed** — never pass them as CLI arguments; the CLI derives them from seeds + program ID.
3. **`init` implies `mut`** — do not add both; use `AccountPostState::new_claimed()` for init accounts.
4. **Account parameters must come before instruction arguments** in function signatures.
5. **Return ALL accounts** in `post_states` — every account passed to the instruction must appear in the output (even unchanged ones).
6. **Only the owning program can decrease an account's balance** — this is enforced by the runtime.
7. **`generate_idl!` path is relative to `CARGO_MANIFEST_DIR`** — typically `"../methods/guest/src/bin/{name}.rs"`.
8. **Instruction names**: `snake_case` in Rust, `kebab-case` in CLI, `PascalCase` in enum variants.
9. **Program ID is the RISC Zero ImageID** — a `[u32; 8]` derived from the compiled guest binary.
10. **State types need `Serialize` + `Deserialize`** (serde) for storage; instruction enum variants derive these automatically.
