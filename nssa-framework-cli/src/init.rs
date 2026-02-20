//! Project scaffolding: `nssa-cli init <name>`

use std::fs;
use std::path::Path;

pub fn init_project(name: &str) {
    let root = Path::new(name);
    if root.exists() {
        eprintln!("❌ Directory '{}' already exists", name);
        std::process::exit(1);
    }

    println!("🚀 Creating NSSA project '{}'...", name);

    let snake_name = name.replace('-', "_");

    // Create directories
    let dirs = [
        "",
        &format!("{}_core/src", snake_name),
        "methods/src",
        &format!("methods/guest/src/bin"),
        "examples/src/bin",
    ];
    for dir in &dirs {
        let p = root.join(dir);
        fs::create_dir_all(&p).unwrap_or_else(|e| {
            eprintln!("❌ Failed to create {}: {}", p.display(), e);
            std::process::exit(1);
        });
    }

    // Root Cargo.toml (workspace)
    write_file(root, "Cargo.toml", &format!(r#"[workspace]
members = [
    "{snake_name}_core",
    "methods",
    "examples",
]
exclude = [
    "methods/guest",
]
resolver = "2"
"#));

    // .gitignore
    write_file(root, ".gitignore", &format!(r#"target/
methods/guest/target/
*.bin
.{snake_name}-state
.{snake_name}-state.tmp
"#));

    // Makefile
    write_file(root, "Makefile", &format!(r#"# {name} — NSSA Program
#
# Quick start:
#   make build idl deploy setup
#   make cli ARGS="<command> --arg1 value1"


SHELL := /bin/bash
STATE_FILE := .{snake_name}-state
IDL_FILE := {name}-idl.json
PROGRAMS_DIR := methods/guest/target/riscv32im-risc0-zkvm-elf/docker
PROGRAM_BIN := $(PROGRAMS_DIR)/{snake_name}.bin

# Load saved state if it exists
-include $(STATE_FILE)

define save_var
	@grep -v '^$(1)=' $(STATE_FILE) 2>/dev/null > $(STATE_FILE).tmp || true
	@echo '$(1)=$(2)' >> $(STATE_FILE).tmp
	@mv $(STATE_FILE).tmp $(STATE_FILE)
endef

.PHONY: help build idl cli deploy setup inspect status clean

help: ## Show this help
	@echo "{name} — NSSA Program"
	@echo ""
	@echo "  make build       Build the guest binary (needs risc0 toolchain)"
	@echo "  make idl         Generate IDL from program source"
	@echo "  make cli ARGS=   Run the IDL-driven CLI (pass args via ARGS=)"
	@echo "  make deploy      Deploy program to sequencer"
	@echo "  make setup       Create accounts needed for the program"
	@echo "  make inspect     Show ProgramId for built binary"
	@echo "  make status      Show saved state and binary info"
	@echo "  make clean       Remove saved state"
	@echo ""
	@echo "Example:"
	@echo "  make build idl deploy"
	@echo "  make cli ARGS=\"--help\""
	@echo "  make cli ARGS=\"-p $(PROGRAM_BIN) <command> --arg1 value1\""

build: ## Build the guest binary
	cargo risczero build --manifest-path methods/guest/Cargo.toml
	@echo ""
	@echo "✅ Guest binary built: $(PROGRAM_BIN)"
	@ls -la $(PROGRAM_BIN) 2>/dev/null || true

idl: ## Generate IDL JSON from program source
	cargo run --bin generate_idl > $(IDL_FILE)
	@echo "✅ IDL written to $(IDL_FILE)"

cli: ## Run the IDL-driven CLI (ARGS="...")
	cargo run --bin {snake_name}_cli -- -i $(IDL_FILE) $(ARGS)

deploy: ## Deploy program to sequencer
	@test -f "$(PROGRAM_BIN)" || (echo "ERROR: Binary not found. Run 'make build' first."; exit 1)
	wallet deploy-program $(PROGRAM_BIN)
	@echo "✅ Program deployed"

inspect: ## Show ProgramId for built binary
	cargo run --bin {snake_name}_cli -- -i $(IDL_FILE) inspect $(PROGRAM_BIN)

setup: ## Create accounts needed for the program
	@echo "Creating signer account..."
	$(eval SIGNER_ID := $(shell wallet account new public 2>&1 | sed -n 's/.*Public\/\([A-Za-z0-9]*\).*/\1/p'))
	@echo "Signer: $(SIGNER_ID)"
	$(call save_var,SIGNER_ID,$(SIGNER_ID))
	@echo ""
	@echo "✅ Account saved to $(STATE_FILE)"

status: ## Show saved state and binary info
	@echo "{name} Status"
	@echo "──────────────────────────────────────"
	@if [ -f "$(STATE_FILE)" ]; then cat $(STATE_FILE); else echo "(no state — run 'make setup')"; fi
	@echo ""
	@echo "Binaries:"
	@ls -la $(PROGRAM_BIN) 2>/dev/null || echo "  {snake_name}.bin: NOT BUILT (run 'make build')"
	@echo ""
	@echo "IDL:"
	@ls -la $(IDL_FILE) 2>/dev/null || echo "  $(IDL_FILE): NOT GENERATED (run 'make idl')"

clean: ## Remove saved state
	rm -f $(STATE_FILE) $(STATE_FILE).tmp
	@echo "✅ State cleaned"
"#));

    // README
    write_file(root, "README.md", &format!(r#"# {name}

An NSSA/LEZ program built with [nssa-framework](https://github.com/jimmy-claw/nssa-framework).

## Prerequisites

- Rust + [risc0 toolchain](https://dev.risczero.com/api/zkvm/install)
- [LSSA wallet CLI](https://github.com/logos-blockchain/lssa) (`wallet` binary)
- A running sequencer

## Quick Start

```bash
# 1. Build the guest binary
make build

# 2. Generate the IDL (auto-extracts from #[nssa_program] annotations)
make idl

# 3. Deploy to sequencer
make deploy

# 4. See available commands (auto-generated from your program)
make cli ARGS="--help"

# 5. Run an instruction
make cli ARGS="-p methods/guest/target/riscv32im-risc0-zkvm-elf/docker/{snake_name}.bin \\
  <command> --arg1 value1 --arg2 value2"

# Dry run (no submission):
make cli ARGS="--dry-run -p methods/guest/target/riscv32im-risc0-zkvm-elf/docker/{snake_name}.bin \\
  <command> --arg1 value1"
```

## Make Targets

| Target | Description |
|--------|-------------|
| `make build` | Build the guest binary (risc0) |
| `make idl` | Generate IDL JSON from program source |
| `make cli ARGS="..."` | Run the IDL-driven CLI |
| `make deploy` | Deploy program to sequencer |
| `make inspect` | Show ProgramId for built binary |
| `make setup` | Create accounts via wallet |
| `make status` | Show saved state and binary info |
| `make clean` | Remove saved state |

## Project Structure

```
{name}/
├── {snake_name}_core/    # Shared types (used by guest + host)
│   └── src/lib.rs
├── methods/
│   └── guest/            # RISC Zero guest program (runs on-chain)
│       └── src/bin/{snake_name}.rs
├── examples/             # CLI tools
│   └── src/bin/
│       ├── generate_idl.rs    # One-liner IDL generator
│       └── {snake_name}_cli.rs # Three-line CLI wrapper
├── Makefile
└── {name}-idl.json       # Auto-generated IDL
```

## How It Works

The `#[nssa_program]` macro in your guest binary defines your on-chain program.
The framework automatically:

1. **Generates an `Instruction` enum** from your function signatures
2. **Generates an IDL** (Interface Description Language) describing your program
3. **Provides a full CLI** for building, inspecting, and submitting transactions

You write the program logic. The framework handles the rest.
"#));

    // program_core
    write_file(root, &format!("{}_core/Cargo.toml", snake_name), &format!(r#"[package]
name = "{snake_name}_core"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = {{ version = "1.0", features = ["derive"] }}
borsh = "1.5"
"#));

    write_file(root, &format!("{}_core/src/lib.rs", snake_name), r#"use serde::{Deserialize, Serialize};

/// Example state struct — customize for your program.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgramState {
    pub initialized: bool,
    pub owner: [u8; 32],
}
"#);

    // methods/Cargo.toml
    write_file(root, "methods/Cargo.toml", &format!(r#"[package]
name = "{snake_name}-methods"
version = "0.1.0"
edition = "2021"

[build-dependencies]
risc0-build = "3.0"

[dependencies]
risc0-zkvm = {{ version = "3.0.3", features = ["std"] }}
{snake_name}_core = {{ path = "../{snake_name}_core" }}
"#));

    // methods/build.rs
    write_file(root, "methods/build.rs", r#"fn main() {
    risc0_build::embed_methods();
}
"#);

    // methods/src/lib.rs
    write_file(root, "methods/src/lib.rs", r#"include!(concat!(env!("OUT_DIR"), "/methods.rs"));
"#);

    // methods/guest/Cargo.toml
    write_file(root, "methods/guest/Cargo.toml", &format!(r#"[package]
name = "{snake_name}-guest"
version = "0.1.0"
edition = "2021"

[workspace]

[[bin]]
name = "{snake_name}"
path = "src/bin/{snake_name}.rs"

[dependencies]
nssa-framework = {{ git = "https://github.com/jimmy-claw/nssa-framework.git" }}
nssa-framework-core = {{ git = "https://github.com/jimmy-claw/nssa-framework.git" }}
nssa_core = {{ git = "https://github.com/logos-blockchain/lssa.git", branch = "main" }}
risc0-zkvm = {{ version = "3.0.3", default-features = false }}
{snake_name}_core = {{ path = "../../{snake_name}_core" }}
serde = {{ version = "1.0", features = ["derive"] }}
borsh = "1.5"
"#));

    // Guest program skeleton
    write_file(root, &format!("methods/guest/src/bin/{}.rs", snake_name), &format!(r#"#![no_main]

use nssa_core::account::AccountWithMetadata;
use nssa_core::program::AccountPostState;
use nssa_framework::prelude::*;

risc0_zkvm::guest::entry!(main);

#[nssa_program]
mod {snake_name} {{
    #[allow(unused_imports)]
    use super::*;

    /// Initialize the program state.
    #[instruction]
    pub fn initialize(
        #[account(init, pda = literal("state"))]
        state: AccountWithMetadata,
        #[account(signer)]
        owner: AccountWithMetadata,
    ) -> NssaResult {{
        // TODO: implement initialization logic
        Ok(NssaOutput::states_only(vec![
            AccountPostState::new_claimed(state.account.clone()),
            AccountPostState::new(owner.account.clone()),
        ]))
    }}

    /// Example instruction — replace with your own.
    #[instruction]
    pub fn do_something(
        #[account(mut, pda = literal("state"))]
        state: AccountWithMetadata,
        #[account(signer)]
        owner: AccountWithMetadata,
        amount: u64,
    ) -> NssaResult {{
        // TODO: implement your logic
        Ok(NssaOutput::states_only(vec![
            AccountPostState::new(state.account.clone()),
            AccountPostState::new(owner.account.clone()),
        ]))
    }}
}}
"#));

    // examples/Cargo.toml
    write_file(root, "examples/Cargo.toml", &format!(r#"[package]
name = "{snake_name}-examples"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "generate_idl"
path = "src/bin/generate_idl.rs"

[[bin]]
name = "{snake_name}_cli"
path = "src/bin/{snake_name}_cli.rs"

[dependencies]
nssa-framework = {{ git = "https://github.com/jimmy-claw/nssa-framework.git" }}
nssa-framework-core = {{ git = "https://github.com/jimmy-claw/nssa-framework.git" }}
nssa-framework-cli = {{ git = "https://github.com/jimmy-claw/nssa-framework.git" }}
{snake_name}_core = {{ path = "../{snake_name}_core" }}
serde_json = "1.0"
tokio = {{ version = "1.28.2", features = ["net", "rt-multi-thread", "sync", "macros"] }}
"#));

    // generate_idl.rs
    write_file(root, "examples/src/bin/generate_idl.rs", &format!(r#"/// Generate IDL JSON for the {name} program.
///
/// Usage:
///   cargo run --bin generate_idl > {name}-idl.json

nssa_framework::generate_idl!("../methods/guest/src/bin/{snake_name}.rs");
"#));

    // CLI wrapper
    write_file(root, &format!("examples/src/bin/{}_cli.rs", snake_name), r#"#[tokio::main]
async fn main() {
    nssa_framework_cli::run().await;
}
"#);

    println!();
    println!("✅ Project '{}' created!", name);
    println!();
    println!("Next steps:");
    println!("  cd {}", name);
    println!("  # Edit methods/guest/src/bin/{}.rs with your program logic", snake_name);
    println!("  # Edit {}_core/src/lib.rs with your types", snake_name);
    println!("  make idl        # Generate the IDL");
    println!("  make cli ARGS=\"--help\"  # See available commands");
}

fn write_file(root: &Path, rel_path: &str, content: &str) {
    let path = root.join(rel_path);
    fs::write(&path, content).unwrap_or_else(|e| {
        eprintln!("❌ Failed to write {}: {}", path.display(), e);
        std::process::exit(1);
    });
}
