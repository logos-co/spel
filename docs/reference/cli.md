# CLI

The `spel-cli` crate provides a generic, IDL-driven command-line interface for any SPEL program. Programs get a complete CLI by writing a three-line wrapper — the binary is named `spel`.

For a guided walkthrough, see the [Tutorial](../tutorial.md). For other reference topics, see the [Reference Index](README.md).

---

## Quick Start

```rust
#[tokio::main]
async fn main() {
    spel::run().await;
}
```

---

## Invocation Syntax

```
spel <COMMAND> [ARGS]                  (with spel.toml)
spel [OPTIONS] -- <COMMAND> [ARGS]     (without spel.toml)
```

The `--` separator is required whenever you pass global `OPTIONS` (like `--idl` or `--program`) together with a command that also takes its own `--`-flags. Without it, the first `--foo` after the command name would be parsed as a global flag, not an instruction argument.

When a `spel.toml` is present in the current directory (or any ancestor), `--idl` and `--program` are resolved from it automatically — the `--` separator is not needed in that case.

---

## Configuration: `spel.toml`

A `spel.toml` file in your project root lets you drop `--idl`/`--program` flags and makes multi-program projects ergonomic. `spel` walks up from the current directory until it finds one.

**Single-program projects:**

```toml
[program]
idl    = "my-program-idl.json"
binary = "target/riscv32im-risc0-zkvm-elf/docker/my_program.bin"
```

**Multi-program projects:**

```toml
[programs.game]
idl    = "game-idl.json"
binary = "target/game.bin"

[programs.nft]
idl    = "nft-idl.json"
binary = "target/nft.bin"
```

With a multi-program config, pick one with `--program <name>`:

```bash
spel --program game create --name "first-game"
```

When only one `[programs.<name>]` entry exists it is auto-selected; with multiple entries and no `--program <name>`, the CLI errors out listing the available names.

`[program]` and `[programs]` are mutually exclusive. Paths inside the config are resolved relative to the `spel.toml` file, so invocations from subdirectories work.

---

## Global Options

| Option | Short | Description |
|--------|-------|-------------|
| `--idl <FILE>` | `-i` | Path to the IDL JSON file. Required if not set in `spel.toml`. |
| `--program <NAME\|HEX\|FILE>` | `-p` | Accepts one of three things: (a) a program **name** from `spel.toml` → resolves both IDL and binary; (b) a 64-char **hex program ID** → skips binary loading; (c) a **file path** to the program ELF binary. |
| `--dry-run[=text\|json]` | | Resolve everything (PDAs, accounts, signer nonces, serialized data) and print without submitting. `--dry-run` and `--dry-run=text` produce a human-readable report; `--dry-run=json` emits a machine-readable document on stdout. |
| `--bin-<NAME> <FILE>` | | Additional program binary. Auto-fills `--<NAME>-program-id` from the binary's image ID. Useful for cross-program references. |
| `--program-id <HEX>` | | **Deprecated** — prefer `--program <HEX>`. Still accepted. |

---

## `init`

Scaffold a new SPEL project. This is the command that creates everything described below — "scaffolding" and `init` refer to the same operation.

```bash
spel init [OPTIONS] <project-name>
```

**Does not require `--idl`.**

> Options must come **before** the project name — the argument loop stops at the
> first non-flag token, so anything after the name is ignored.

Available options: `--lez-tag <TAG>`, `--lez-rev <REV>`, `--spel-tag <TAG>`,
`--spel-rev <REV>`, and `--spel-git <URL>` (the git URL the scaffolded project
pulls the framework from; defaults to `https://github.com/logos-co/spel.git`).
A fork's CI passes its own URL so the scaffolded project tests the fork's code.

Creates a complete project structure with:
- Workspace `Cargo.toml`
- `{name}_core/` crate for shared types
- `methods/guest/` with a skeleton `#[lez_program]` guest binary
- `examples/` with `generate_idl.rs` and `{name}_cli.rs`
- `Makefile` with `build`, `idl`, `cli`, `deploy`, `inspect`, `setup`, `status`, `clean` targets
- `spel.toml` (so you can run `spel` without `--idl`/`--program`)
- `README.md` with quick start guide
- `.gitignore`

**Options:**

| Flag | Purpose |
|------|---------|
| `--lez-tag <TAG>` | LEZ version tag to pin (e.g. `v0.2.4`). |
| `--spel-tag <TAG>` | SPEL version tag to pin. |
| `--lez-rev <REV>` | LEZ git revision (alternative to `--lez-tag`). |
| `--spel-rev <REV>` | SPEL git revision (alternative to `--spel-tag`). |

**Example:**

```bash
spel init my-token
cd my-token
# Edit methods/guest/src/bin/my_token.rs with your program logic
make idl
make cli ARGS="--help"
```

---

## `inspect`

Two modes — the one you get depends on whether `--idl`/`--type` are set.

### Mode 1: Print ProgramId for ELF binaries

```bash
spel inspect <FILE> [FILE...]
```

**Does not require `--idl`.**

**Output for each binary:**

```
📦 path/to/program.bin
   ProgramId (decimal): 12345,67890,11111,22222,33333,44444,55555,66666
   ProgramId (hex):     00003039,000109b2,...
   ImageID (hex bytes): 393000009b210100...
```

- **Decimal**: comma-separated `[u32; 8]` values
- **Hex**: comma-separated hex `[u32; 8]` values
- **ImageID hex bytes**: 64-character hex string (little-endian byte representation). This is the value to pass to `--program <HEX>`.

**Example:**

```bash
spel inspect methods/guest/target/riscv32im-risc0-zkvm-elf/docker/my_program.bin
```

### Mode 2: Decode account data

```bash
spel inspect <ACCOUNT-ID> --idl <IDL_FILE> --type <TYPE> [--data <BORSH_HEX>]
```

Fetches the account (or decodes supplied borsh-hex bytes) and renders the data as JSON using the IDL-declared type.

```bash
spel inspect <account-id> --idl my_program-idl.json --type VaultState
spel inspect <account-id> --idl my_program-idl.json --type VaultState --data <borsh-hex>
```

---

## `idl` (command)

Print the loaded IDL as pretty-printed JSON.

```bash
spel --idl <IDL_FILE> idl
```

With `spel.toml`:

```bash
spel idl
```

---

## `generate-idl`

Generate IDL JSON directly from a program source file. Useful if you don't want a runtime `examples/generate_idl.rs` binary.

```bash
spel generate-idl <PATH>
```

**Does not require `--idl`.**

- `<PATH>` may be a single source file, or a project directory to search for `#[lez_program]` entry points.
- Single match → IDL JSON printed to stdout.
- Multiple matches → one `<name>-idl.json` file written per program.

---

## `pda` (IDL mode)

Compute a PDA address from the IDL-defined seeds.

```bash
spel --idl <IDL_FILE> --program <NAME|HEX|FILE> pda <ACCOUNT_NAME> [--<seed-arg> <value> ...]
```

Looks up the named account across all instructions in the IDL, finds its PDA seed definition, resolves all seeds, and prints the base58 AccountId.

**Seed resolution:**
- `const` seeds: resolved from the IDL definition
- `arg` seeds: must be provided via `--<arg-name> <value>` (parsed through the IDL type of the owning instruction's argument)
- `account` seeds: must be provided via `--<account-name> <hex|base58>`

**Private PDAs.** An account declared `#[account(private_pda, …)]` derives its
address from the caller's key material as well as the seeds, so computing it needs
two extra flags:

```bash
spel pda <ACCOUNT_NAME> --npk <64-char-hex> --vpk <2368-char-hex>
```

`--npk` is the `NullifierPublicKey` (32 bytes) and `--vpk` the `ViewingPublicKey`
(an ML-KEM-768 encapsulation key, 1184 bytes). Both are printed by
`wallet account show-keys` for a private account. Omitting either is an error —
the address cannot be derived without them.

Since LEZ v0.2.1 the derivation is
`SHA256(prefix || program_id || seed || npk || vpk || identifier)`, with
`identifier` currently fixed at 0.

**ProgramId resolution** (in priority order):
1. `--program <64-char-hex>`
2. `--program <name-from-spel.toml>` → resolved binary loaded
3. `--program <path>` → binary loaded

**Example:**

```bash
# Simple PDA with only const seeds
spel --idl my_program-idl.json --program abc123...def pda counter

# PDA with arg seed (with spel.toml set up)
spel pda multisig_state --create-key 0a1b2c...

# List available PDAs
spel --idl my_program-idl.json pda
```

**If no account name is given**, prints all PDA accounts found in the IDL.

---

## `pda` (raw mode)

Compute an arbitrary PDA from a program ID and raw seeds — no IDL required.

```bash
spel --program <64-CHAR-HEX> pda <SEED1> [SEED2] ...
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
spel --program abc123...def pda my_state_name

# Multiple seeds
spel --program abc123...def pda multisig_vault__ 0a1b2c3d...
```

---

## Instruction Execution

Execute any instruction defined in the IDL. The CLI auto-generates subcommands from the IDL.

```bash
spel --idl <IDL_FILE> --program <NAME|HEX|FILE> -- <INSTRUCTION> [--<arg> <value> ...] [--<account> <hex|base58> ...]
```

With `spel.toml`:

```bash
spel <INSTRUCTION> [--<arg> <value> ...] [--<account> <hex|base58> ...]
```

Instruction names are converted from `snake_case` to `kebab-case` in CLI commands (e.g., `create_proposal` → `create-proposal`).

**Arguments:**
- Instruction args: `--<arg-name> <value>` (type-aware parsing from IDL)
- Non-PDA accounts: `--<account-name> <base58|hex>` (64 hex chars or base58 string) — the flag is just the account's parameter name, no suffix
- PDA accounts: **auto-computed** from seeds — not passed as arguments
- Rest (variadic) accounts: optional, comma-separated list of account IDs

**Additional program binaries:** Use `--bin-<name> <file>` to auto-fill `--<name>-program-id` from the binary's image ID.

**Transaction flow:**
1. Parse and validate all arguments
2. Auto-fill program IDs from `--bin-*` flags
3. Serialize instruction data in risc0 serde format
4. Resolve PDA accounts from seeds (printing each seed input it used)
5. Initialize wallet from `NSSA_WALLET_HOME_DIR` environment variable
6. Fetch nonces for signer accounts
7. Build, sign, and submit the transaction
8. Poll for confirmation

**Per-instruction help:**

```bash
spel <INSTRUCTION> --help
```

Shows accounts (with flags like `[mut, signer, init]`), PDA status, and argument types.

**Example (no spel.toml):**

```bash
# Execute a create instruction
spel --idl multisig-idl.json --program multisig.bin -- create \
  --create-key 0a1b2c3d4e5f... \
  --threshold 2 \
  --members "aabb...00,ccdd...00" \
  --creator EjR7...base58

# Auto-fill cross-program reference
spel --idl treasury-idl.json --program treasury.bin \
  --bin-token token.bin -- \
  transfer --amount 100 \
  --from aabb...00 \
  --to ccdd...00
```

**Example (with spel.toml in the project root):**

```bash
spel create \
  --create-key 0a1b2c3d4e5f... \
  --threshold 2 \
  --members "aabb...00,ccdd...00" \
  --creator EjR7...base58
```

---

## Multi-signature: `--export`, `--co-signer`, `sign`, `submit`

When an instruction needs signatures from keys held in different wallets, build the
transaction once and pass it around as a blob instead of trying to gather every key
in one place.

```bash
# wallet A: build a partial transaction naming the other required signer
spel --export tx.json --co-signer <SIGNER_B> transfer --owner <SIGNER_A> --amount 10

# wallet B: add its witness (prompts, showing the decoded instruction)
spel sign tx.json

# anyone: submit once every witness is present
spel submit tx.json
```

`--export <PATH>` writes the partial transaction rather than submitting it, and
cannot be combined with `--dry-run`. `--co-signer <ACCOUNT_ID>` may be repeated to
name several additional signers.

`spel sign <PATH>` appends a witness for any required signer whose key this wallet
holds, and reports when the blob is complete. `spel submit <PATH>` assembles the
witness set and broadcasts. Submit refuses a blob with a missing witness, and
refuses one whose message no longer matches what the witnesses signed.

---

## `program-id`

```bash
spel program-id <BINARY> [--format text|hex|json]
```

Prints the `ProgramId` derived from a compiled guest binary. This is the current
name for what `inspect <FILE>` did; `inspect` still works and additionally decodes
account data (see above).

---

## Dry Run

`--dry-run` resolves the entire transaction — PDAs, non-PDA account IDs, signer nonces, serialized instruction bytes — and prints it without submitting. Useful for CI golden tests, for previewing a TX before signing, and for scripting.

```bash
spel --dry-run <INSTRUCTION> --arg1 value1           # text, human-readable (default)
spel --dry-run=text <INSTRUCTION> --arg1 value1      # same as above, explicit
spel --dry-run=json <INSTRUCTION> --arg1 value1      # machine-readable JSON
```

**Text output:**

```
=== Dry Run ===
Program ID: abc123...def
Instruction: transfer

Accounts:
  PDA vault → 4Lp3gkH... [writable]
    seeds: [program_id, "state"]
  recipient → 0xaabb...00
  sender → 0xccdd...00 [signer]

Arguments:
  --amount 1000

Instruction data: 0x01000000e803000000000000...

Signers:
  sender: nonce=42
================
Dry run complete — not submitted.
```

**JSON output (shape):**

```json
{
  "program_id": "abc123...def",
  "instruction": "transfer",
  "accounts": [
    {
      "name": "vault", "id": "4Lp3gkH...", "flags": ["writable"],
      "is_pda": true,
      "seeds": [{"kind": "const", "value": "state"}]
    },
    { "name": "sender", "id": "0xccdd...00", "flags": ["signer"] }
  ],
  "arguments": { "amount": 1000 },
  "instruction_data": "01000000e803000000000000...",
  "signers": { "sender": {"nonce": "42"} }
}
```

Numeric values that exceed JSON's 53-bit integer precision (`u128` args and nonces) are emitted as decimal strings to avoid silent truncation.

In JSON mode, all human-readable preamble is suppressed — only the JSON document goes to stdout — so it's safe to pipe through `jq`.

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
| `Vec<String>` | Repeat the flag once per element | `--tag a --tag b` |
| `Option<T>` | `none`/`null`/empty for None; otherwise same as inner type | `none` or `42` |
| Account IDs | Base58 string **or** 64 hex chars (with optional `0x` prefix) | `EjR7...` or `0xaabb...00` |

**Notes:**
- `[u8; N]` accepts both hex and string formats. Hex is detected by length (exactly `2*N` chars, all hex digits). Otherwise treated as UTF-8 and zero-padded.
- `0x` prefix is accepted and stripped for hex values.
- `program_id` values can also use `0x`-prefixed hex for individual u32 components.

---

## Serialization (spel-cli internals)

The CLI serializes instruction data using `risc0_zkvm::serde::to_vec` (risc0 serde format, `Vec<u32>`) for submission to the zkVM guest. The format is:

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
