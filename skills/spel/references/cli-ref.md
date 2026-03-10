# CLI Reference

Condensed cheatsheet for `lez-cli` and `lez-client-gen`.

---

## lez-cli Global Options

| Option | Short | Description |
|--------|-------|-------------|
| `--idl <FILE>` | `-i` | IDL JSON file path (required for most commands) |
| `--program <FILE>` | `-p` | Program ELF binary (computes ProgramId) |
| `--program-id <HEX>` | | 64-char hex ProgramId (overrides `--program`) |
| `--dry-run` | | Print parsed data without submitting transaction |
| `--bin-<NAME> <FILE>` | | Additional binary; auto-fills `--<NAME>-program-id` |

---

## Commands

### init — Scaffold New Project

```bash
lez-cli init <project-name>
```

No `--idl` required. Creates full workspace with Makefile, core crate, guest binary, IDL generator, and CLI wrapper.

### inspect — Print ProgramId

```bash
lez-cli inspect <FILE> [FILE...]
```

No `--idl` required. Outputs decimal, hex, and ImageID formats for each binary.

```
ProgramId (decimal): 12345,67890,...
ProgramId (hex):     00003039,000109b2,...
ImageID (hex bytes): 393000009b210100...    ← use this for --program-id
```

### idl — Print IDL

```bash
lez-cli -i <IDL_FILE> idl
```

Pretty-prints the loaded IDL JSON.

### pda (IDL mode) — Compute PDA from IDL Seeds

```bash
lez-cli -i <IDL> [-p <BIN> | --program-id <HEX>] pda <ACCOUNT_NAME> [--<seed-arg> <value>]
```

Looks up account in IDL, resolves seeds, prints base58 address.

```bash
# Const-only seeds
lez-cli -i idl.json --program-id abc...def pda counter

# With arg seed
lez-cli -i idl.json --program-id abc...def pda multisig_state --create-key 0a1b2c...

# With account seed
lez-cli -i idl.json --program-id abc...def pda vault --user-account EjR7...

# List all PDAs
lez-cli -i idl.json pda
```

### pda (raw mode) — Compute PDA Without IDL

```bash
lez-cli --program-id <64-CHAR-HEX> pda <SEED1> [SEED2] ...
```

No `--idl` required. Each seed: 64-char hex → 32 raw bytes; otherwise UTF-8 zero-padded to 32 bytes. Multi-seed: `SHA-256(seed1 || seed2 || ...)`.

```bash
lez-cli --program-id abc...def pda my_state
lez-cli --program-id abc...def pda multisig_vault__ 0a1b2c3d...
```

### Instruction Execution

```bash
lez-cli -i <IDL> [-p <BIN> | --program-id <HEX>] <INSTRUCTION> [--<arg> <val>] [--<account>-account <id>]
```

- Instruction names: `snake_case` → `kebab-case` (`create_proposal` → `create-proposal`)
- PDA accounts: auto-computed, not passed as arguments
- Account IDs: base58 or 64-char hex (with optional `0x` prefix)
- Rest accounts: comma-separated list

```bash
# Execute instruction
lez-cli -i idl.json -p prog.bin create \
  --create-key 0a1b... --threshold 2 \
  --members "aa...00,bb...00" \
  --creator-account EjR7...

# Dry run
lez-cli -i idl.json --program-id abc...def --dry-run approve \
  --proposal-id 5 --member-account cc...00

# Cross-program binary reference
lez-cli -i treasury-idl.json -p treasury.bin --bin-token token.bin \
  transfer --amount 100 --from-account aa...00

# Per-instruction help
lez-cli -i idl.json <INSTRUCTION> --help
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

## lez-client-gen

Generate typed Rust client + C FFI + C header from IDL:

```bash
lez-client-gen --idl <IDL_FILE> --out-dir <DIR>
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
