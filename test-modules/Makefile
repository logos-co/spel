# test-modules — SPEL Program
#
# Quick start:
#   make all        # full build (guest binary → IDL → FFI → UI)
#   make deploy     # deploy to sequencer
#   make setup      # create accounts
#   make cli ARGS="<command> --arg1 value1"
#   make install    # install plugin to Basecamp


SHELL := /bin/bash
STATE_FILE := .test_modules-state
IDL_FILE := test-modules-idl.json
PROGRAMS_DIR := methods/guest/target/riscv32im-risc0-zkvm-elf/docker
PROGRAM_BIN := $(PROGRAMS_DIR)/test_modules.bin
SPEL_CLIENT_GEN ?= $(shell which spel-client-gen 2>/dev/null || echo ../target/release/spel-client-gen)
UI_OUT_DIR := ui/test-modules
FFI_LIB := target/debug/libtest_modules_ffi.so
FFI_LIB_REL := ../../$(FFI_LIB)
LGX_FILE := $(UI_OUT_DIR)/test-modules.lgx
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

.PHONY: help all build idl cli deploy setup inspect status clean ffi-gen ffi ui-gen ui-build ui-run ui-package lgx lgx-sign install

help: ## Show this help
	@echo "test-modules — SPEL Program"
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
	@echo "  make ui-gen      Generate Qt/QML Basecamp module scaffold from IDL"
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
	cargo risczero build --manifest-path methods/guest/Cargo.toml
	@echo ""
	@echo "✅ Guest binary built: $(PROGRAM_BIN)"
	@ls -la $(PROGRAM_BIN) 2>/dev/null || true

idl: ## Generate IDL JSON from program source
	cargo run --bin generate_idl > $(IDL_FILE)
	@echo "✅ IDL written to $(IDL_FILE)"

cli: ## Run the IDL-driven CLI (ARGS="...")
	cargo run --bin test_modules_cli -- $(ARGS)

deploy: ## Deploy program to sequencer
	@test -f "$(PROGRAM_BIN)" || (echo "ERROR: Binary not found. Run 'make build' first."; exit 1)
	wallet deploy-program $(PROGRAM_BIN)
	@echo "✅ Program deployed"

inspect: ## Show ProgramId for built binary
	cargo run --bin test_modules_cli -- inspect $(PROGRAM_BIN)

setup: ## Create accounts needed for the program
	@echo "Creating signer account..."
	$(eval SIGNER_ID := $(shell wallet account new public 2>&1 | sed -n 's/.*Public\/\([A-Za-z0-9]*\).*/\1/p'))
	@echo "Signer: $(SIGNER_ID)"
	$(call save_var,SIGNER_ID,$(SIGNER_ID))
	@echo ""
	@echo "✅ Account saved to $(STATE_FILE)"

status: ## Show saved state and binary info
	@echo "test-modules Status"
	@echo "──────────────────────────────────────"
	@if [ -f "$(STATE_FILE)" ]; then cat $(STATE_FILE); else echo "(no state — run 'make setup')"; fi
	@echo ""
	@echo "Binaries:"
	@ls -la $(PROGRAM_BIN) 2>/dev/null || echo "  test_modules.bin: NOT BUILT (run 'make build')"
	@ls -la $(FFI_LIB) 2>/dev/null || echo "  test_modules_ffi.so: NOT BUILT (run 'make ffi')"
	@echo ""
	@echo "IDL:"
	@ls -la $(IDL_FILE) 2>/dev/null || echo "  $(IDL_FILE): NOT GENERATED (run 'make idl')"

clean: ## Remove saved state
	rm -f $(STATE_FILE) $(STATE_FILE).tmp
	@echo "✅ State cleaned"

ffi-gen: idl ## Generate FFI Rust source from IDL
	$(SPEL_CLIENT_GEN) --idl $(IDL_FILE) --out-dir test_modules_ffi/generated --target rust+ffi
	@echo "✅ FFI source generated in test_modules_ffi/generated/"

ffi: ffi-gen ## Build the FFI shared library (.so)
	cargo build -p test_modules_ffi
	@echo ""
	@echo "✅ FFI library: $(FFI_LIB)"

ui-gen: idl ffi ## Generate Qt/QML Basecamp module scaffold from IDL
	$(SPEL_CLIENT_GEN) --idl $(IDL_FILE) --out-dir $(UI_OUT_DIR) --target logos-module \
	    --ffi-lib-path $(FFI_LIB_REL)
	@echo ""
	@echo "✅ UI scaffold generated in $(UI_OUT_DIR)/"
	@echo "   Next: make ui-build  (or make install)"

ui-build: ffi ## Build the Qt/QML standalone preview app (needs Qt6 + CMake)
	@test -d "$(UI_OUT_DIR)" || (echo "ERROR: UI scaffold not found. Run 'make ui-gen' first."; exit 1)
	cmake -B $(UI_OUT_DIR)/build $(UI_OUT_DIR)
	cmake --build $(UI_OUT_DIR)/build --parallel
	@echo ""
	@echo "✅ Preview app built: $(UI_OUT_DIR)/build/TestModulesApp"
	@echo "   Run with: make ui-run  or install with: make install"

ui-run: ui-build ## Run the Qt/QML standalone preview app
	$(UI_OUT_DIR)/build/TestModulesApp

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
	cp $(UI_OUT_DIR)/build/libtest_modules_plugin.so $(LGX_STAGING)/
	cp $(FFI_LIB) $(LGX_STAGING)/
	cp $(UI_OUT_DIR)/qml/Main.qml $(LGX_STAGING)/
	cd $(UI_OUT_DIR) && lgx create test-modules
	lgx add $(LGX_FILE) -v $(VARIANT) -f $(LGX_STAGING) -m libtest_modules_plugin.so -y
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
	$(eval INSTALL_DIR := $(HOME)/.local/share/Logos/LogosBasecamp/plugins/test_modules)
	mkdir -p $(INSTALL_DIR)
	cp $(UI_OUT_DIR)/build/libtest_modules_plugin.so $(INSTALL_DIR)/
	cp $(UI_OUT_DIR)/qml/Main.qml $(INSTALL_DIR)/
	cp $(UI_OUT_DIR)/manifest.json $(INSTALL_DIR)/
	@printf '%s' '$(VARIANT)' > $(INSTALL_DIR)/variant
	@echo ""
	@echo "✅ Installed to $(INSTALL_DIR)"
	@echo "   Restart Basecamp to load the module"
