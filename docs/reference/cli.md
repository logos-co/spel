# CLI

The `lez-cli` crate provides a generic, IDL-driven command-line interface for any LEZ program. Programs get a complete CLI by writing a three-line wrapper.

For a guided walkthrough, see the [Tutorial](../tutorial.md). For other reference topics, see the [Reference Index](README.md).

---

## Quick Start

```rust
#[tokio::main]
async fn main() {
    lez_cli::run().await;
}
```

---

## Global Options

| Option | Short | Description |
|--------|-------|-------------|
| `--idl <FILE>` | `-i` | Path to the IDL JSON file. Required for most commands. |
| `--program <FILE>` | `-p` | Path to the program ELF binary. Used to compute ProgramId and for deployment. Defaults to `program.bin`. |
| `--program-id <HEX>` | | 64-character hex string of the program ID. Overrides `--program` for ProgramId resolution (faster, no binary loading). |
| `--dry-run` | | Print parsed/serialized data without submitting the transaction. |
| `--bin-<NAME> <FILE>` | | Additional program binary. Auto-fills `--<NAME>-program-id` from the binary's image ID. Useful for cross-program references. |

---

## `init`

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

## `inspect`

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

## `idl` (command)

Print the loaded IDL as pretty-printed JSON.

```bash
lez-cli --idl <IDL_FILE> idl
```

**Example:**

```bash
lez-cli -i my_program-idl.json idl
```

---

## `pda` (IDL mode)

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

## `pda` (raw mode)

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

## Instruction Execution

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

## Type Format Table

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
