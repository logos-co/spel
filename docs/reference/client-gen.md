# Client Code Generation

`lez-client-gen` generates typed Rust client code and C FFI wrappers from LEZ program IDL JSON. Useful for integrating LEZ programs into applications (e.g., C++/Qt desktop apps).

For a guided walkthrough, see the [Tutorial](../tutorial.md). For other reference topics, see the [Reference Index](README.md).

---

## lez-client-gen CLI Usage

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

## Library API

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

## Generated Rust Client

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

## Generated C FFI

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

## Generated C Header

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

## Using from C++/Qt

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
