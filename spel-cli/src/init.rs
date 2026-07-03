//! Project scaffolding: `spel init <name>`

use std::fs;
use std::path::Path;

pub fn init_project(
    name: &str,
    lez_tag: Option<&str>,
    spel_tag: Option<&str>,
    lez_rev: Option<&str>,
    spel_rev: Option<&str>,
) {
    let root = Path::new(name);
    if root.exists() {
        eprintln!("❌ Directory '{}' already exists", name);
        std::process::exit(1);
    }

    // Extract just the directory name for use as the project name,
    // so absolute paths like "/tmp/my-project" yield "my-project".
    let project_name = root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_else(|| {
            eprintln!("❌ Could not extract project name from '{}'", name);
            std::process::exit(1);
        });

    println!("🚀 Creating SPEL project '{}'...", project_name);

    let snake_name = project_name.replace('-', "_");

    // Detect spel-client-gen: check the directory next to the running binary first
    // (covers `cargo run`, installed builds, and CI). Fall back to bare name (PATH lookup).
    let spel_client_gen = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join("spel-client-gen")))
        .filter(|p| p.exists())
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "spel-client-gen".to_string());

    // Create directories
    let dirs = [
        "",
        &format!("{}_core/src", snake_name),
        "methods/src",
        &format!("methods/guest/src/bin"),
        "examples/src/bin",
        &format!("{}_ffi/src", snake_name),
        &format!("{}_ffi/generated", snake_name),
        "scripts",
    ];
    for dir in &dirs {
        let p = root.join(dir);
        fs::create_dir_all(&p).unwrap_or_else(|e| {
            eprintln!("❌ Failed to create {}: {}", p.display(), e);
            std::process::exit(1);
        });
    }

    // Root Cargo.toml (workspace)
    write_file(
        root,
        "Cargo.toml",
        &format!(
            r#"[workspace]
members = [
    "{snake_name}_core",
    "{snake_name}_ffi",
    "methods",
    "examples",
]
exclude = [
    "methods/guest",
]
resolver = "2"
"#
        ),
    );

    // .gitignore
    write_file(
        root,
        ".gitignore",
        &format!(
            r#"target/
methods/guest/target/
*.bin
.{snake_name}-state
.{snake_name}-state.tmp
ui/
"#
        ),
    );

    // FFI crate: Cargo.toml (cdylib — compiled into a .so for Qt to link against)
    let lez_ref_ffi = match (lez_tag, lez_rev) {
        (Some(t), _) => format!("tag = \"{}\"", t),
        (_, Some(r)) => format!("rev = \"{}\"", r),
        _ => "tag = \"v0.2.0\"".to_string(),
    };
    let spel_ref_ffi = match (spel_tag, spel_rev) {
        (Some(t), _) => format!("tag = \"{}\"", t),
        (_, Some(r)) => format!("rev = \"{}\"", r),
        _ => "tag = \"v0.4.0\"".to_string(),
    };
    write_file(
        root,
        &format!("{snake_name}_ffi/Cargo.toml"),
        &format!(
            r#"[package]
name = "{snake_name}_ffi"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
nssa        = {{ git = "https://github.com/logos-blockchain/logos-execution-zone.git", {lez_ref_ffi}, package = "lee" }}
nssa_core   = {{ git = "https://github.com/logos-blockchain/logos-execution-zone.git", {lez_ref_ffi}, package = "lee_core" }}
common      = {{ git = "https://github.com/logos-blockchain/logos-execution-zone.git", {lez_ref_ffi} }}
sequencer_service_rpc = {{ git = "https://github.com/logos-blockchain/logos-execution-zone.git", {lez_ref_ffi}, features = ["client"] }}
wallet      = {{ git = "https://github.com/logos-blockchain/logos-execution-zone.git", {lez_ref_ffi} }}
spel-framework-core = {{ git = "https://github.com/logos-co/spel.git", {spel_ref_ffi} }}
serde_json  = "1"
serde       = {{ version = "1", features = ["derive"] }}
borsh       = "1.5"
tokio       = {{ version = "1", features = ["rt-multi-thread"] }}
hex         = "0.4"
sha2        = "0.10"
{snake_name}_core = {{ path = "../{snake_name}_core" }}
"#
        ),
    );

    // FFI crate: src/lib.rs — includes generated code produced by `make ffi-gen`
    write_file(
        root,
        &format!("{snake_name}_ffi/src/lib.rs"),
        &format!(
            r#"// Auto-generated FFI for the {project_name} program.
// Run `make ffi-gen` to regenerate from the IDL, then `make ffi` to build the .so.
#![allow(dead_code, unused_imports, unused_variables)]
include!(concat!(env!("CARGO_MANIFEST_DIR"), "/generated/{snake_name}_ffi.rs"));
"#
        ),
    );

    // FFI crate: generated stub — replaced by `make ffi-gen`
    write_file(
        root,
        &format!("{snake_name}_ffi/generated/{snake_name}_ffi.rs"),
        r#"// Stub — run `make ffi-gen` to populate this file with the real FFI implementation.
"#,
    );

    // Standalone LGX installer — works on any machine, no lgx sign needed.
    write_file(
        root,
        "scripts/install_lgx.py",
        r#"#!/usr/bin/env python3
"""Install a Basecamp LGX module directly to the plugins directory.

Installs by extracting the package variant directly into the Basecamp plugins
directory, bypassing the in-app UI entirely.  Use this as a fallback when
the Basecamp 'Install Plugin' button is unavailable or inconvenient.

Note: Basecamp accepts unsigned LGX packages with a warning when using its
'Install Plugin' button — no signing step is required for normal distribution.

Usage:
    python3 scripts/install_lgx.py <module.lgx>

After running, restart Basecamp to load the new module.
"""
import gzip, io, json, os, pathlib, platform, sys, tarfile

def detect_variant(members):
    arch = "amd64" if platform.machine() == "x86_64" else "arm64"
    system = platform.system().lower()
    # Try variants in priority order
    for v in [f"{system}-{arch}", f"{system}-{arch}-dev", f"{system}-x86_64-dev"]:
        prefix = f"variants/{v}/"
        if any(m.name.startswith(prefix) for m in members):
            return v, prefix, f"{system}-{arch}"
    available = sorted({m.name.split("/")[1] for m in members if m.name.startswith("variants/") and "/" in m.name[9:]})
    raise SystemExit(f"No compatible variant found.\nAvailable variants: {available}")

def main():
    if len(sys.argv) != 2 or sys.argv[1] in ("-h", "--help"):
        print(__doc__)
        sys.exit(0 if sys.argv[1:] else 1)

    lgx_path = sys.argv[1]
    with gzip.open(lgx_path, "rb") as gz:
        raw = gz.read()

    with tarfile.open(fileobj=io.BytesIO(raw)) as tf:
        members = tf.getmembers()
        manifest = json.loads(tf.extractfile("manifest.json").read())
        name = manifest["name"]
        variant, prefix, install_variant = detect_variant(members)

        install_dir = (
            pathlib.Path.home()
            / ".local/share/Logos/LogosBasecamp/plugins"
            / name
        )
        install_dir.mkdir(parents=True, exist_ok=True)

        for m in members:
            if not m.name.startswith(prefix) or not m.isfile():
                continue
            rel = m.name[len(prefix):]
            dest = install_dir / rel
            dest.parent.mkdir(parents=True, exist_ok=True)
            data = tf.extractfile(m).read()
            dest.write_bytes(data)
            if rel.endswith(".so"):
                os.chmod(dest, 0o755)

    (install_dir / "manifest.json").write_text(json.dumps(manifest, indent=2))
    (install_dir / "variant").write_text(install_variant)

    print(f"Installed '{name}' to {install_dir}")
    print("Restart Basecamp to load the module.")

if __name__ == "__main__":
    main()
"#,
    );

    // LGX packaging helper — replaces the root manifest.json in the .lgx tarball
    // with the pre-generated manifest.json produced by spel-client-gen (0.2.0 format).
    write_file(
        root,
        "scripts/patch_lgx_manifest.py",
        r#"#!/usr/bin/env python3
"""Replace manifest.json in an LGX archive with a pre-generated manifest file.
Usage: python3 scripts/patch_lgx_manifest.py <pkg.lgx> <manifest.json>
"""
import sys, tarfile, gzip, json, io

def main():
    if len(sys.argv) != 3:
        print(f"Usage: {sys.argv[0]} <pkg.lgx> <manifest.json>", file=sys.stderr)
        sys.exit(1)
    lgx_path, manifest_path = sys.argv[1], sys.argv[2]
    manifest = json.load(open(manifest_path))

    with gzip.open(lgx_path, "rb") as gz:
        raw = gz.read()
    with tarfile.open(fileobj=io.BytesIO(raw)) as tf:
        members = [
            (m, tf.extractfile(m).read() if m.isfile() else None)
            for m in tf.getmembers()
        ]

    # Keep only manifest "main" entries whose variant directory actually exists in the archive.
    archive_variants = {
        m.name.split("/")[1]
        for m, _ in members
        if m.name.startswith("variants/") and m.name.count("/") >= 1 and m.name.split("/")[1]
    }
    if "main" in manifest and archive_variants:
        manifest["main"] = {k: v for k, v in manifest["main"].items() if k in archive_variants}

    buf = io.BytesIO()
    with gzip.GzipFile(fileobj=buf, mode="wb", mtime=0) as gz_out:
        with tarfile.open(fileobj=gz_out, mode="w") as tf_out:
            mb = json.dumps(manifest, indent=2, sort_keys=True).encode()
            info = tarfile.TarInfo("manifest.json")
            info.size = len(mb)
            info.mtime = info.uid = info.gid = 0
            info.mode = 0o644
            tf_out.addfile(info, io.BytesIO(mb))
            for m, data in members:
                if m.name == "manifest.json":
                    continue
                tf_out.addfile(m, io.BytesIO(data)) if data is not None else tf_out.addfile(m)

    with open(lgx_path, "wb") as f:
        f.write(buf.getvalue())
    print(f"Patched {lgx_path}: manifest.json replaced from {manifest_path}")

if __name__ == "__main__":
    main()
"#,
    );

    // spel.toml
    write_file(
        root,
        "spel.toml",
        &format!(
            r#"[program]
idl = "{project_name}-idl.json"
binary = "methods/guest/target/riscv32im-risc0-zkvm-elf/docker/{snake_name}.bin"
"#
        ),
    );

    // Makefile
    write_file(
        root,
        "Makefile",
        &format!(
            r#"# {project_name} — SPEL Program
#
# Quick start:
#   make all        # full build (guest binary → IDL → FFI → UI)
#   make deploy     # deploy to sequencer
#   make setup      # create accounts
#   make cli ARGS="<command> --arg1 value1"
#   make install    # install plugin to Basecamp


SHELL := /bin/bash
STATE_FILE := .{snake_name}-state
IDL_FILE := {project_name}-idl.json
PROGRAMS_DIR := methods/guest/target/riscv32im-risc0-zkvm-elf/docker
PROGRAM_BIN := $(PROGRAMS_DIR)/{snake_name}.bin
SPEL_CLIENT_GEN ?= {spel_client_gen}
UI_OUT_DIR := ui/{project_name}
FFI_LIB := target/debug/lib{snake_name}_ffi.so
FFI_LIB_REL := ../../$(FFI_LIB)
LGX_FILE := $(UI_OUT_DIR)/{project_name}.lgx
LGX_STAGING := $(UI_OUT_DIR)/.lgx-staging
ARCH := $(shell uname -m | sed 's/x86_64/amd64/;s/aarch64/arm64/')
OS := $(shell uname -s | tr 'A-Z' 'a-z')
VARIANT := linux-$(ARCH)

# Load saved state if it exists
-include $(STATE_FILE)

define save_var
	@grep -v '^$(1)=' $(STATE_FILE) 2>/dev/null > $(STATE_FILE).tmp || true
	@echo '$(1)=$(2)' >> $(STATE_FILE).tmp
	@mv $(STATE_FILE).tmp $(STATE_FILE)
endef

.PHONY: help all build idl cli deploy setup inspect status clean ffi-gen ffi ui-gen ui-regen ui-build ui-run ui-package lgx lgx-sign install

help: ## Show this help
	@echo "{project_name} — SPEL Program"
	@echo ""
	@echo "  make all         Full build: guest binary → IDL → FFI → UI scaffold → UI app"
	@echo "  make build       Build the guest binary (needs risc0 toolchain)"
	@echo "  make idl         Generate IDL from program source"
	@echo "  make cli ARGS=   Run the IDL-driven CLI (reads spel.toml for config)"
	@echo "  make deploy      Deploy program to sequencer"
	@echo "  make setup       Create accounts needed for the program"
	@echo "  make inspect     Show ProgramId for built binary"
	@echo "  make status      Show saved state and binary info"
	@echo "  make clean       Remove saved state"
	@echo ""
	@echo "  make ffi-gen     Generate FFI Rust source from IDL"
	@echo "  make ffi         Build FFI shared library (.so)"
	@echo "  make ui-gen      Generate Qt/QML Basecamp module scaffold from IDL (full, first run)"
	@echo "  make ui-regen    Regenerate C++ backend only; keep hand-written qml/Main.qml"
	@echo "  make ui-build    Build the Qt/QML standalone preview app (needs Qt6 + CMake)"
	@echo "  make ui-run      Run the standalone preview app"
	@echo "  make install     Install plugin directly to Basecamp plugins directory"
	@echo "  make lgx         Build a portable LGX archive for distribution"
	@echo "  make lgx-sign    Sign LGX with a dev key (run lgx keygen --name devkey first)"
	@echo ""
	@echo "  Distribution workflow:"
	@echo "    lgx keygen --name devkey    # one-time key generation"
	@echo "    make lgx && make lgx-sign   # build + sign"
	@echo "    # Share $(LGX_FILE) — recipients install via Basecamp 'Install Plugin'"
	@echo "  Dev install (direct, no signing needed):"
	@echo "    make install"
	@echo ""
	@echo "Example:"
	@echo "  make all         # full build from scratch"
	@echo "  make all deploy  # full build + deploy to sequencer"
	@echo "  make cli ARGS=\"--help\""
	@echo "  make cli ARGS=\"<command> --arg1 value1\""

all: build idl ffi ui-gen ui-build ## Full build: guest binary → IDL → FFI → UI scaffold → UI app
	@echo ""
	@echo "✅ Full build complete!"
	@echo "   Run with: make ui-run"
	@echo "   Install:  make install"
	@echo "   Deploy:   make deploy  (then make setup)"

build: ## Build the guest binary
	cargo risczero build --manifest-path methods/guest/Cargo.toml \
		2> >(grep -Ev "Falling back to slow ImageID|No such file or directory \(os error 2\)" >&2)
	@echo ""
	@echo "✅ Guest binary built: $(PROGRAM_BIN)"
	@ls -la $(PROGRAM_BIN) 2>/dev/null || true

idl: ## Generate IDL JSON from program source
	cargo run --bin generate_idl > $(IDL_FILE)
	@echo "✅ IDL written to $(IDL_FILE)"

cli: ## Run the IDL-driven CLI (ARGS="...")
	cargo run --bin {snake_name}_cli -- $(ARGS)

deploy: ## Deploy program to sequencer
	@test -f "$(PROGRAM_BIN)" || (echo "ERROR: Binary not found. Run 'make build' first."; exit 1)
	wallet deploy-program $(PROGRAM_BIN)
	@echo "✅ Program deployed"

inspect: ## Show ProgramId for built binary
	cargo run --bin {snake_name}_cli -- inspect $(PROGRAM_BIN)

setup: ## Create accounts needed for the program
	@echo "Creating signer account..."
	$(eval SIGNER_ID := $(shell wallet account new public 2>&1 | sed -n 's/.*Public\/\([A-Za-z0-9]*\).*/\1/p'))
	@echo "Signer: $(SIGNER_ID)"
	$(call save_var,SIGNER_ID,$(SIGNER_ID))
	@echo ""
	@echo "✅ Account saved to $(STATE_FILE)"

status: ## Show saved state and binary info
	@echo "{project_name} Status"
	@echo "──────────────────────────────────────"
	@if [ -f "$(STATE_FILE)" ]; then cat $(STATE_FILE); else echo "(no state — run 'make setup')"; fi
	@echo ""
	@echo "Binaries:"
	@ls -la $(PROGRAM_BIN) 2>/dev/null || echo "  {snake_name}.bin: NOT BUILT (run 'make build')"
	@ls -la $(FFI_LIB) 2>/dev/null || echo "  {snake_name}_ffi.so: NOT BUILT (run 'make ffi')"
	@echo ""
	@echo "IDL:"
	@ls -la $(IDL_FILE) 2>/dev/null || echo "  $(IDL_FILE): NOT GENERATED (run 'make idl')"

clean: ## Remove saved state
	rm -f $(STATE_FILE) $(STATE_FILE).tmp
	@echo "✅ State cleaned"

ffi-gen: idl ## Generate FFI Rust source from IDL
	$(SPEL_CLIENT_GEN) --idl $(IDL_FILE) --out-dir {snake_name}_ffi/generated --target rust+ffi
	@echo "✅ FFI source generated in {snake_name}_ffi/generated/"

ffi: ffi-gen ## Build the FFI shared library (.so)
	cargo build -p {snake_name}_ffi
	@echo ""
	@echo "✅ FFI library: $(FFI_LIB)"

ui-gen: idl ffi ## Generate Qt/QML Basecamp module scaffold from IDL (overwrites all files)
	$(SPEL_CLIENT_GEN) --idl $(IDL_FILE) --out-dir $(UI_OUT_DIR) --target logos-module \
	    --ffi-lib-path $(FFI_LIB_REL)
	@echo ""
	@echo "✅ UI scaffold generated in $(UI_OUT_DIR)/"
	@echo "   Next: make ui-build  (or make install)"
	@echo "   Tip:  use 'make ui-regen' after the first run to keep hand-written qml/Main.qml"

ui-regen: idl ffi ## Regenerate C++ backend + build files; preserve hand-written qml/Main.qml
	@test -d "$(UI_OUT_DIR)" || (echo "ERROR: UI scaffold not found. Run 'make ui-gen' first."; exit 1)
	$(SPEL_CLIENT_GEN) --idl $(IDL_FILE) --out-dir $(UI_OUT_DIR) --target logos-module \
	    --ffi-lib-path $(FFI_LIB_REL) --skip-ui
	@echo ""
	@echo "✅ C++ backend regenerated in $(UI_OUT_DIR)/ (qml/Main.qml preserved)"
	@echo "   Next: make ui-build"

ui-build: ffi ## Build the Qt/QML standalone preview app (needs Qt6 + CMake)
	@test -d "$(UI_OUT_DIR)" || (echo "ERROR: UI scaffold not found. Run 'make ui-gen' first."; exit 1)
	cmake -B $(UI_OUT_DIR)/build $(UI_OUT_DIR)
	cmake --build $(UI_OUT_DIR)/build --parallel
	@echo ""
	@echo "✅ Preview app built in $(UI_OUT_DIR)/build/"
	@echo "   Run with: make ui-run  or install with: make install"

ui-run: ui-build ## Run the Qt/QML standalone preview app
	@APP=$$(find $(UI_OUT_DIR)/build -maxdepth 1 -name '*App' -type f | head -1); \
	test -n "$$APP" || (echo "ERROR: no *App binary found in $(UI_OUT_DIR)/build/"; exit 1); \
	exec "$$APP"

ui-package: ui-build ## Package plugin + FFI .so for loading in Basecamp
	mkdir -p $(UI_OUT_DIR)/lib
	cp $(FFI_LIB) $(UI_OUT_DIR)/lib/
	@echo ""
	@echo "✅ Module packaged: $(UI_OUT_DIR)/"
	@echo "   Load in Basecamp by pointing to $(UI_OUT_DIR)/"

lgx: ui-build ## Build a portable LGX archive for distribution
	@command -v lgx >/dev/null 2>&1 || \
	    (echo "ERROR: lgx not found. Get it from https://github.com/logos-co/logos-package"; exit 1)
	@test -f "$(FFI_LIB)" || (echo "ERROR: FFI library not built. Run 'make ffi' first."; exit 1)
	rm -rf $(LGX_STAGING) $(LGX_FILE)
	mkdir -p $(LGX_STAGING)
	cp $(UI_OUT_DIR)/build/lib{snake_name}_plugin.so $(LGX_STAGING)/
	cp $(FFI_LIB) $(LGX_STAGING)/
	cp $(UI_OUT_DIR)/qml/Main.qml $(LGX_STAGING)/
	cd $(UI_OUT_DIR) && lgx create {project_name}
	lgx add $(LGX_FILE) -v $(VARIANT) -f $(LGX_STAGING) -m lib{snake_name}_plugin.so -y
	python3 scripts/patch_lgx_manifest.py $(LGX_FILE) $(UI_OUT_DIR)/manifest.json
	lgx verify $(LGX_FILE)
	rm -rf $(LGX_STAGING)
	@echo ""
	@echo "✅ LGX package: $(LGX_FILE)"
	@echo "   Dev install:  make install"
	@echo "   To distribute: make lgx-sign  then share $(LGX_FILE)"

lgx-sign: ## Sign LGX with a dev key (run 'lgx keygen --name devkey' first)
	@test -f "$(LGX_FILE)" || (echo "ERROR: LGX not built. Run 'make lgx' first."; exit 1)
	@command -v lgx >/dev/null 2>&1 || \
	    (echo "ERROR: lgx not found. Get it from https://github.com/logos-co/logos-package"; exit 1)
	lgx sign $(LGX_FILE) --key devkey
	lgx verify $(LGX_FILE)
	@echo ""
	@echo "✅ Signed: $(LGX_FILE)"
	@echo "   Share the file — recipients install via Basecamp 'Install Plugin'"

install: ui-build ## Install plugin directly to Basecamp plugins directory (no LGX/signature needed)
	$(eval INSTALL_DIR := $(HOME)/.local/share/Logos/LogosBasecamp/plugins/{snake_name})
	mkdir -p $(INSTALL_DIR)
	cp $(UI_OUT_DIR)/build/lib{snake_name}_plugin.so $(INSTALL_DIR)/
	cp $(UI_OUT_DIR)/qml/Main.qml $(INSTALL_DIR)/
	cp $(UI_OUT_DIR)/manifest.json $(INSTALL_DIR)/
	@printf '%s' '$(VARIANT)' > $(INSTALL_DIR)/variant
	@echo ""
	@echo "✅ Installed to $(INSTALL_DIR)"
	@echo "   Restart Basecamp to load the module"
"#
        ),
    );

    // README
    write_file(
        root,
        "README.md",
        &format!(
            r#"# {project_name}

A SPEL program built with [spel-framework](https://github.com/logos-co/spel).

## Prerequisites

- Rust + [risc0 toolchain](https://dev.risczero.com/api/zkvm/install)
- [LSSA wallet CLI](https://github.com/logos-blockchain/lssa) (`wallet` binary)
- A running sequencer

## Quick Start

```bash
# 1. Build the guest binary
make build

# 2. Generate the IDL (auto-extracts from #[lez_program] annotations)
make idl

# 3. Deploy to sequencer
make deploy

# 4. See available commands (auto-generated from your program)
make cli ARGS="--help"

# 5. Run an instruction (spel.toml provides IDL and binary paths)
make cli ARGS="<command> --arg1 value1 --arg2 value2"

# Dry run (no submission):
make cli ARGS="--dry-run -- <command> --arg1 value1"
```

## Make Targets

| Target | Description |
|--------|-------------|
| `make all` | Full build: guest binary → IDL → FFI → UI scaffold → UI app |
| `make build` | Build the guest binary (risc0) |
| `make idl` | Generate IDL JSON from program source |
| `make cli ARGS="..."` | Run the IDL-driven CLI |
| `make deploy` | Deploy program to sequencer |
| `make inspect` | Show ProgramId for built binary |
| `make setup` | Create accounts via wallet |
| `make status` | Show saved state and binary info |
| `make clean` | Remove saved state |
| `make ffi-gen` | Generate FFI Rust source from IDL |
| `make ffi` | Build FFI shared library (.so) |
| `make ui-gen` | Generate Qt/QML Basecamp module scaffold (first run, overwrites all) |
| `make ui-regen` | Regenerate C++ backend + build files; keep hand-written `qml/Main.qml` |
| `make ui-build` | Build the Qt/QML standalone preview app |
| `make ui-run` | Run the standalone preview app |
| `make install` | Install plugin to Basecamp plugins directory |
| `make lgx` | Build a portable LGX archive for distribution |
| `make lgx-sign` | Sign LGX with a dev key (`lgx keygen --name devkey` first) |
| `python3 scripts/install_lgx.py <f.lgx>` | Direct install (bypasses Basecamp UI) |

## Project Structure

```
{project_name}/
├── {snake_name}_core/    # Shared types (used by guest + host)
│   └── src/lib.rs
├── {snake_name}_ffi/     # C FFI cdylib (compiled to .so for Qt)
│   ├── src/lib.rs        # includes generated/ at build time
│   └── generated/        # populated by `make ffi-gen` (git-ignored)
├── methods/
│   └── guest/            # RISC Zero guest program (runs on-chain)
│       └── src/bin/{snake_name}.rs
├── examples/             # CLI tools
│   └── src/bin/
│       ├── generate_idl.rs    # One-liner IDL generator
│       └── {snake_name}_cli.rs # Three-line CLI wrapper
├── spel.toml                         # SPEL CLI config (IDL and binary paths)
├── Makefile
└── {project_name}-idl.json       # Auto-generated IDL
```

## How It Works

The `#[lez_program]` macro in your guest binary defines your on-chain program.
The framework automatically:

1. **Generates an `Instruction` enum** from your function signatures
2. **Generates an IDL** (Interface Description Language) describing your program
3. **Provides a full CLI** for building, inspecting, and submitting transactions

You write the program logic. The framework handles the rest.
"#
        ),
    );

    // program_core
    write_file(
        root,
        &format!("{}_core/Cargo.toml", snake_name),
        &format!(
            r#"[package]
name = "{snake_name}_core"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = {{ version = "1.0", features = ["derive"] }}
borsh = "1.5"

"#
        ),
    );

    write_file(
        root,
        &format!("{}_core/src/lib.rs", snake_name),
        r#"use serde::{Deserialize, Serialize};

/// Example state struct — customize for your program.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgramState {
    pub initialized: bool,
    pub owner: [u8; 32],
}
"#,
    );

    // methods/Cargo.toml
    write_file(
        root,
        "methods/Cargo.toml",
        &format!(
            r#"[package]
name = "{snake_name}-methods"
version = "0.1.0"
edition = "2021"

[build-dependencies]
risc0-build = "=3.0.5"

[dependencies]
risc0-zkvm = {{ version = "=3.0.5", features = ["std"] }}
{snake_name}_core = {{ path = "../{snake_name}_core" }}
"#
        ),
    );

    // methods/build.rs
    write_file(
        root,
        "methods/build.rs",
        r#"fn main() {
    risc0_build::embed_methods();
}
"#,
    );

    // methods/src/lib.rs
    write_file(
        root,
        "methods/src/lib.rs",
        r#"include!(concat!(env!("OUT_DIR"), "/methods.rs"));
"#,
    );

    let lez_ref = match (lez_tag, lez_rev) {
        (Some(t), _) => format!("tag = \"{}\"", t),
        (_, Some(r)) => format!("rev = \"{}\"", r),
        _ => "tag = \"v0.2.0\"".to_string(),
    };
    let spel_ref = match (spel_tag, spel_rev) {
        (Some(t), _) => format!("tag = \"{}\"", t),
        (_, Some(r)) => format!("rev = \"{}\"", r),
        _ => "branch = \"main\"".to_string(),
    };
    // methods/guest/Cargo.toml
    write_file(
        root,
        "methods/guest/Cargo.toml",
        &format!(
            r#"[package]
name = "{snake_name}-guest"
version = "0.1.0"
edition = "2021"

[workspace]

[[bin]]
name = "{snake_name}"
path = "src/bin/{snake_name}.rs"

[dependencies]
spel-framework = {{ git = "https://github.com/logos-co/spel.git", {spel_ref} }}
nssa_core = {{ git = "https://github.com/logos-blockchain/logos-execution-zone.git", {lez_ref}, package = "lee_core" }}
risc0-zkvm = {{ version = "=3.0.5", features = ["std"] }}
{snake_name}_core = {{ path = "../../{snake_name}_core" }}
serde = {{ version = "1.0", features = ["derive"] }}
borsh = "1.5"
ruint = "=1.17.0"

"#
        ),
    );

    // Guest program skeleton
    write_file(
        root,
        &format!("methods/guest/src/bin/{}.rs", snake_name),
        &format!(
            r#"#![no_main]

use spel_framework::prelude::*;
use nssa_core::account::Data;

risc0_zkvm::guest::entry!(main);

#[lez_program]
mod {snake_name} {{
    #[allow(unused_imports)]
    use super::*;

    /// Program state stored in a PDA account.
    #[derive(BorshSerialize, BorshDeserialize)]
    #[account_type]
    pub struct ProgramState {{
        pub initialized: bool,
        pub owner: [u8; 32],
    }}

    /// Initialize the program state.
    #[instruction]
    pub fn initialize(
        _ctx: ProgramContext,
        #[account(init, pda = literal("state"))]
        mut state: AccountWithMetadata,
        #[account(signer)]
        owner: AccountWithMetadata,
    ) -> SpelResult {{
        let ps = ProgramState {{
            initialized: true,
            owner: *owner.account_id.value(),
        }};
        let bytes = borsh::to_vec(&ps).map_err(|e| SpelError::custom(999, format!("borsh error: {{e}}")))?;
        state.account.data = Data::try_from(bytes).map_err(|_| SpelError::custom(999, "data too big"))?;
        Ok(SpelOutput::execute(vec![state, owner], vec![]))
    }}

    /// Example instruction — replace with your own.
    #[instruction]
    pub fn do_something(
        #[account(mut, pda = literal("state"))]
        state: AccountWithMetadata,
        #[account(signer)]
        owner: AccountWithMetadata,
        _amount: u64,
    ) -> SpelResult {{
        // TODO: implement your logic
        Ok(SpelOutput::execute(vec![state, owner], vec![]))
    }}
}}
"#
        ),
    );

    // examples/Cargo.toml
    write_file(
        root,
        "examples/Cargo.toml",
        &format!(
            r#"[package]
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
spel-framework = {{ git = "https://github.com/logos-co/spel.git", {spel_ref} }}
nssa_core = {{ git = "https://github.com/logos-blockchain/logos-execution-zone.git", {lez_ref}, package = "lee_core" }}
spel = {{ git = "https://github.com/logos-co/spel.git", {spel_ref} }}
{snake_name}_core = {{ path = "../{snake_name}_core" }}
serde_json = "1.0"
tokio = {{ version = "1.28.2", features = ["net", "rt-multi-thread", "sync", "macros"] }}
"#
        ),
    );

    // generate_idl.rs
    write_file(
        root,
        "examples/src/bin/generate_idl.rs",
        &format!(
            r#"/// Generate IDL JSON for the {project_name} program.
///
/// Usage:
///   cargo run --bin generate_idl > {project_name}-idl.json

spel_framework::generate_idl!("../methods/guest/src/bin/{snake_name}.rs");
"#
        ),
    );

    // CLI wrapper
    write_file(
        root,
        &format!("examples/src/bin/{}_cli.rs", snake_name),
        r#"#[tokio::main]
async fn main() {
    spel::run().await;
}
"#,
    );

    println!();
    // Generate Cargo.lock for the guest to pin dependency versions
    // (prevents getrandom 0.3.x breakage in Docker builds)
    let guest_dir = root.join("methods/guest");
    let status = std::process::Command::new("cargo")
        .arg("generate-lockfile")
        .current_dir(&guest_dir)
        .status();
    match status {
        Ok(s) if s.success() => {},
        Ok(s) => eprintln!("⚠️  cargo generate-lockfile exited with: {}", s),
        Err(e) => eprintln!(
            "⚠️  Failed to generate Cargo.lock (cargo not found?): {}",
            e
        ),
    }

    // Pin the enum-ordinalize crates to the version LEZ uses (4.3.2).
    // `educe`'s `^4` range otherwise resolves to 4.4.1, which requires rustc
    // 1.89, but the guest is built with the risc0 zkVM toolchain (rustc 1.88).
    // Both enum-ordinalize and its companion proc-macro enum-ordinalize-derive
    // resolve independently, so each needs its own pin. LEZ's own lockfile
    // pins 4.3.2, so this keeps the guest in sync and buildable under risc0.
    // Best-effort: init still succeeds if a pin fails (e.g. a future LEZ drops
    // the dep, or raises educe's floor above 4.3.2), in which case cargo prints
    // the reason on stderr just above this warning.
    for pkg in ["enum-ordinalize", "enum-ordinalize-derive"] {
        let status = std::process::Command::new("cargo")
            .args(["update", "-p", pkg, "--precise", "4.3.2"])
            .current_dir(&guest_dir)
            .status();
        match status {
            Ok(s) if s.success() => {},
            Ok(s) => eprintln!(
                "⚠️  Could not pin {} to 4.3.2 (cargo exited {}); see cargo output above. If the guest fails to build under risc0, check the resolved enum-ordinalize versions.",
                pkg, s
            ),
            Err(e) => {
                eprintln!("⚠️  Failed to pin {} (cargo not found?): {}", pkg, e)
            },
        }
    }

    println!("✅ Project '{}' created!", project_name);
    println!();
    println!("Next steps:");
    println!("  cd {}", name);
    println!(
        "  # Edit methods/guest/src/bin/{}.rs with your program logic",
        snake_name
    );
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
