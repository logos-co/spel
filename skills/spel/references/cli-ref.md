# CLI Reference

Condensed cheatsheet for `spel` and `spel-client-gen`.

---

## Invocation syntax

```
spel <COMMAND> [ARGS]                  (with spel.toml)
spel [OPTIONS] -- <COMMAND> [ARGS]     (without spel.toml)
```

The `--` separator is required whenever global `OPTIONS` are mixed with a command that has its own `--flags`.

## spel.toml (optional)

Place in project root to skip `--idl`/`--program` flags. Discovered by walking up from CWD.

```toml
[program]                 # single-program
idl    = "my-idl.json"
binary = "target/prog.bin"

# or

[programs.game]           # multi-program; select with `--program game`
idl    = "game-idl.json"
binary = "target/game.bin"
[programs.nft]
idl    = "nft-idl.json"
binary = "target/nft.bin"
```

Paths resolve relative to the `spel.toml` itself. `[program]` and `[programs]` are mutually exclusive.

## Global Options

| Option | Short | Description |
|--------|-------|-------------|
| `--idl <FILE>` | `-i` | IDL JSON file path (required if not in `spel.toml`) |
| `--program <NAME\|HEX\|FILE>` | `-p` | Name from `spel.toml`, 64-char hex program ID, or path to ELF binary |
| `--dry-run[=text\|json]` | | Resolve PDAs/accounts/nonces/ix-data and print without submitting (`text` default) |
| `--bin-<NAME> <FILE>` | | Additional binary; auto-fills `--<NAME>-program-id` |
| `--program-id <HEX>` | | Deprecated — use `--program <HEX>` |

---

## Commands

### init — Scaffold New Project

```bash
spel init <project-name>
```

No `--idl` required. Creates full workspace with Makefile, core crate, guest binary, IDL generator, and CLI wrapper.

### inspect — Print ProgramId

```bash
spel inspect <FILE> [FILE...]
```

No `--idl` required. Outputs decimal, hex, and ImageID formats for each binary.

```
ProgramId (decimal): 12345,67890,...
ProgramId (hex):     00003039,000109b2,...
ImageID (hex bytes): 393000009b210100...    ← pass to `--program <HEX>`
```

### idl — Print IDL

```bash
spel -i <IDL_FILE> idl
```

Pretty-prints the loaded IDL JSON.

### pda (IDL mode) — Compute PDA from IDL Seeds

```bash
spel -i <IDL> -p <NAME|HEX|BIN> pda <ACCOUNT_NAME> [--<seed-arg> <value>]
```

Looks up account in IDL, resolves seeds, prints base58 address. (Dry-run / transaction output echoes the seed inputs on separate lines; the standalone `pda` subcommand does not.)

```bash
# With spel.toml
spel pda counter
spel pda multisig_state --create-key 0a1b2c...

# Without spel.toml
spel -i idl.json -p abc...def pda counter
spel -i idl.json -p abc...def pda vault --user-account EjR7...

# List all PDAs
spel -i idl.json pda
```

### pda (raw mode) — Compute PDA Without IDL

```bash
spel --program <64-CHAR-HEX> pda <SEED1> [SEED2] ...
```

No `--idl` required. Each seed: 64-char hex → 32 raw bytes; otherwise UTF-8 zero-padded to 32 bytes. Multi-seed: `SHA-256(seed1 || seed2 || ...)`.

```bash
spel --program abc...def pda my_state
spel --program abc...def pda multisig_vault__ 0a1b2c3d...
```

### Instruction Execution

```bash
# With spel.toml
spel <INSTRUCTION> [--<arg> <val>] [--<account> <id>]

# Without spel.toml (`--` is REQUIRED when mixing global flags with instruction flags)
spel -i <IDL> -p <NAME|HEX|BIN> -- <INSTRUCTION> [--<arg> <val>] [--<account> <id>]
```

- Instruction names: `snake_case` → `kebab-case` (`create_proposal` → `create-proposal`)
- PDA accounts: auto-computed, not passed as arguments
- Account IDs: base58 or 64-char hex (with optional `0x` prefix)
- Rest accounts: comma-separated list

```bash
# Execute instruction (with spel.toml)
spel create --create-key 0a1b... --threshold 2 \
  --members "aa...00,bb...00" --creator EjR7...

# Same, without spel.toml
spel -i idl.json -p prog.bin -- create --create-key 0a1b... --threshold 2 \
  --members "aa...00,bb...00" --creator EjR7...

# Dry run (text, default)
spel --dry-run approve --proposal-id 5 --member cc...00

# Dry run (JSON — pipe through jq in CI)
spel --dry-run=json approve --proposal-id 5 --member cc...00 | jq .

# Cross-program binary reference
spel -i treasury-idl.json -p treasury.bin --bin-token token.bin -- \
  transfer --amount 100 --from aa...00 --to bb...00

# Per-instruction help
spel <INSTRUCTION> --help
```

---

## Type Format Table

| IDL Type | CLI Format | Example |
|----------|-----------|---------|
| `u8` | Decimal | `255` |
| `u32` | Decimal | `1000000` |
| `u64` | Decimal | `1000000000` |
| `u128` | Decimal | `340282366920938463...` |
| `bool` | `true`/`false`/`1`/`0`/`yes`/`no` | `true` |
| `string` | Plain text | `"hello"` |
| `[u8; N]` | Hex (`2*N` chars) or UTF-8 (≤N chars, zero-padded) | `0a1b2c...` or `my_str` |
| `[u32; 8]` / `program_id` | 8 comma-separated u32 or 64-char hex | `abc123...def` |
| `Vec<[u8; 32]>` | Comma-separated hex strings | `"aa...00,bb...00"` |
| `Vec<u8>` | Comma-separated decimal bytes | `1,2,3,4,5` |
| `Vec<u32>` | Comma-separated u32 values | `100,200,300` |
| `Option<T>` | `none`/`null` for None; otherwise inner type | `none` or `42` |
| Account IDs | Base58 or 64-char hex (optional `0x`) | `EjR7...` or `0xaa...00` |

---

## spel-client-gen

Generate typed Rust client + C FFI + C header from IDL:

```bash
spel-client-gen --idl <IDL_FILE> --out-dir <DIR>
```

| Option | Required | Description |
|--------|----------|-------------|
| `--idl <path>` | Yes | IDL JSON file |
| `--out-dir <dir>` | Yes | Output directory (created if needed) |

Output files:

```
<out-dir>/
├── <program>_client.rs    # typed async Rust client with PDA helpers
├── <program>_ffi.rs       # extern "C" functions accepting JSON
└── <program>.h            # C header
```

Build FFI as shared library:

```toml
# Cargo.toml
[lib]
name = "my_program_ffi"
crate-type = ["cdylib"]
```

```rust
// src/lib.rs
include!("../generated/my_program_ffi.rs");
```

```bash
cargo build --release --lib
# → target/release/libmy_program_ffi.so
```

FFI JSON fields (every call):

| Field | Type | Description |
|-------|------|-------------|
| `wallet_path` | `string` | Path to NSSA wallet directory |
| `sequencer_url` | `string` | Sequencer URL (e.g., `http://127.0.0.1:3040`) |
| `program_id_hex` | `string` | 64-char hex program ID |

Plus instruction-specific account and argument fields.

Return format: `{ "success": true, "tx_hash": "..." }` or `{ "success": false, "error": "..." }`.
