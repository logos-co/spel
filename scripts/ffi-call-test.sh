#!/usr/bin/env bash
# SPEL FFI Call Test
# Deploys a program with #[account_type], writes state, generates FFI code,
# and calls the generated fetch_* function against a live sequencer.
#
# Usage: ./ffi-call-test.sh [WORK_DIR]
#
# Required Environment Variables:
#   LEZ_TAG     - LEZ revision/tag to test against
#   LSSA_DIR    - Path to logos-execution-zone directory with sequencer built

set -euo pipefail

export RISC0_DEV_MODE=1
export RISC0_SKIP_BUILD=1

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WORK_DIR="${1:-${WORK_DIR:-/tmp/spel-ffi-call-test}}"
SEQUENCER_PORT="${SEQUENCER_PORT:-3041}"
SEQUENCER_URL="http://127.0.0.1:${SEQUENCER_PORT}"
PROJECT_NAME="ffi_test"

if [ -z "${LEZ_TAG:-}" ]; then
    echo "ERROR: LEZ_TAG environment variable is required"
    exit 1
fi

if [ -z "${LSSA_DIR:-}" ]; then
    echo "ERROR: LSSA_DIR environment variable is required"
    exit 1
fi

LSSA_DIR="$(cd "$LSSA_DIR" && pwd)"
SPEL_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

log()  { echo -e "${GREEN}[FFI-CALL]${NC} $*"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $*"; }
fail() { echo -e "${RED}[FAIL]${NC} $*"; exit 1; }

cleanup() {
    if [ -n "${SEQ_PID:-}" ] && kill -0 "$SEQ_PID" 2>/dev/null; then
        kill "$SEQ_PID" 2>/dev/null || true
        wait "$SEQ_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

# ─── Prerequisites ─────────────────────────────────────────────────────────

command -v cargo >/dev/null 2>&1 || fail "cargo not found"

SEQUENCER_BIN=""
for candidate in sequencer_service "$HOME/bin/sequencer_service" "$LSSA_DIR/target/release/sequencer_service"; do
    if command -v "$candidate" >/dev/null 2>&1 || [ -x "$candidate" ]; then
        SEQUENCER_BIN="$candidate"
        break
    fi
done
[ -n "$SEQUENCER_BIN" ] || fail "sequencer_service not found"

WALLET_BIN=""
for candidate in wallet "$HOME/bin/wallet" "$LSSA_DIR/target/release/wallet"; do
    if command -v "$candidate" >/dev/null 2>&1 || [ -x "$candidate" ]; then
        WALLET_BIN="$candidate"
        break
    fi
done
[ -n "$WALLET_BIN" ] || fail "wallet not found"

export NSSA_WALLET_HOME_DIR="${NSSA_WALLET_HOME_DIR:-${LSSA_DIR}/wallet/configs/debug}"
WALLET_PASSWORD="${WALLET_PASSWORD:-test}"

# Determine SPEL ref for testing (PR head or commit SHA)
SPEL_REF="${SPEL_REF:-local}"

# ─── Setup ─────────────────────────────────────────────────────────────────

log "Setting up in ${WORK_DIR}..."
rm -rf "$WORK_DIR"
mkdir -p "$WORK_DIR"
cd "$WORK_DIR"

# Build local spel-cli from this repo
log "Building local spel-cli..."
cargo build --manifest-path "$SPEL_DIR/Cargo.toml" -p spel --release \
    > "$WORK_DIR/spel-build.log" 2>&1 || fail "Failed to build spel-cli"
SPEL_BIN="$SPEL_DIR/target/release/spel"

# Build spel-client-gen CLI
log "Building spel-client-gen..."
cargo build --manifest-path "$SPEL_DIR/Cargo.toml" -p spel-client-gen --release \
    > "$WORK_DIR/client-gen-build.log" 2>&1 || fail "Failed to build spel-client-gen"
CLIENT_GEN_BIN="$SPEL_DIR/target/release/spel-client-gen"

# ─── Step 1: Scaffold project ──────────────────────────────────────────────

log "Step 1: Creating SPEL project (LEZ=${LEZ_TAG})..."
"$SPEL_BIN" init --lez-tag "$LEZ_TAG" --spel-rev "$SPEL_REF" "$PROJECT_NAME" \
    > "$WORK_DIR/init.log" 2>&1 || fail "spel init failed (see $WORK_DIR/init.log)"
cd "$PROJECT_NAME"

# Regenerate lockfiles so the patch takes effect
(cd methods/guest && cargo generate-lockfile > "$WORK_DIR/guest-lockfile.log" 2>&1) \
    || warn "Guest lockfile regeneration failed"
cargo generate-lockfile > "$WORK_DIR/root-lockfile.log" 2>&1 \
    || warn "Root lockfile regeneration failed"

log "  ✓ Project scaffolded"

# ─── Step 2: Modify guest program with #[account_type] and setter ──────────

log "Step 2: Adding #[account_type] structs and setter instruction..."

GUEST_SRC="methods/guest/src/bin/${PROJECT_NAME}.rs"

# Read the existing guest source
EXISTING=$(cat "$GUEST_SRC")

# Extract everything up to the last closing brace of the mod block
head -n -1 "$GUEST_SRC" > "${GUEST_SRC}.patched"

# Append account_type structs and setter instruction
cat >> "${GUEST_SRC}.patched" << 'RUSTEOF'

    // ── Account types for FFI testing ────────────────────────────────────

    /// State stored in a PDA, accessible via generated fetch_* FFI.
    #[account_type]
    pub struct GreetState {
        pub greeting: String,
        pub counter: u64,
        pub is_active: bool,
    }

    /// Set the greet state (called from host to write PDA data).
    #[instruction]
    pub fn set_greet_state(
        #[account(init, pda = literal("greet_state"), mut)]
        state: AccountWithMetadata<GreetState>,
        #[account(signer)]
        authority: AccountWithMetadata,
        greeting: String,
        counter: u64,
    ) -> SpelResult {
        let post = AccountPostState::new_with_data(
            state.meta.account_id,
            GreetState {
                greeting,
                counter,
                is_active: true,
            },
        );
        Ok(SpelOutput::states_only(vec![post]))
    }
}
RUSTEOF

# Replace the guest source with the patched version
mv "${GUEST_SRC}.patched" "$GUEST_SRC"
log "  ✓ Guest program configured with #[account_type]"

# ─── Step 3: Build guest binary ───────────────────────────────────────────

log "Step 3: Building guest binary..."
RISC0_SKIP_BUILD=1 make build > "$WORK_DIR/build.log" 2>&1 || { cat "$WORK_DIR/build.log"; fail "Build failed"; }
GUEST_BIN=$(find . -name "*.bin" -path "*/riscv32im*" | head -1)
[ -n "$GUEST_BIN" ] || fail "No guest binary found"
GUEST_BIN_ABS="$(realpath "$GUEST_BIN")"
log "  ✓ Built: $(basename "$GUEST_BIN")"

# ─── Step 4: Generate IDL ─────────────────────────────────────────────────

log "Step 4: Generating IDL..."
make idl > "$WORK_DIR/idl.log" 2>&1 || fail "IDL generation failed"
IDL_FILE=$(find . -name "*-idl.json" | head -1)
[ -n "$IDL_FILE" ] || fail "No IDL found"
log "  ✓ IDL: $(basename "$IDL_FILE")"

# ─── Step 5: Start sequencer ──────────────────────────────────────────────

log "Step 5: Starting sequencer on port ${SEQUENCER_PORT}..."
pgrep -f 'sequencer_service.*configs' | xargs -r kill 2>/dev/null || true
sleep 1
rm -rf "${LSSA_DIR}/rocksdb-${SEQUENCER_PORT}"

SEQ_CONFIGS="${LSSA_DIR}/sequencer/service/configs/debug/sequencer_config.json"
if [ ! -f "$SEQ_CONFIGS" ]; then
    SEQ_CONFIGS=$(find "$LSSA_DIR" -name "sequencer_config.json" 2>/dev/null | head -1)
fi
[ -n "$SEQ_CONFIGS" ] || fail "Sequencer config not found"

cd "$LSSA_DIR"
RUST_LOG=info $SEQUENCER_BIN "$SEQ_CONFIGS" > "$WORK_DIR/sequencer.log" 2>&1 &
SEQ_PID=$!
sleep 2
if ! kill -0 $SEQ_PID 2>/dev/null; then
    echo "❌ Sequencer failed to start. Logs:"
    cat "$WORK_DIR/sequencer.log" | tail -30
    exit 1
fi

cd "$WORK_DIR/$PROJECT_NAME"

log "  Waiting for sequencer..."
for i in $(seq 1 60); do
    if curl -sf -o /dev/null -w '%{http_code}' "$SEQUENCER_URL" 2>/dev/null | grep -qE '200|405'; then
        log "  ✓ Sequencer up"; break
    fi
    kill -0 "$SEQ_PID" 2>/dev/null || fail "Sequencer died"
    echo -n "."
    sleep 1
done

# Wait for first block
log "  Waiting for first block..."
for i in $(seq 1 60); do
    if curl -sf -X POST "$SEQUENCER_URL" \
        -H 'Content-Type: application/json' \
        -d '{"jsonrpc":"2.0","method":"getLastBlockId","params":[],"id":1}' 2>/dev/null; then
        log "  ✓ Sequencer producing blocks"; break
    fi
    sleep 2
    echo -n "."
done

# ─── Step 6: Deploy program ───────────────────────────────────────────────

log "Step 6: Deploying program..."
printf '%s\n' "$WALLET_PASSWORD" | $WALLET_BIN deploy-program "$GUEST_BIN_ABS" \
    > "$WORK_DIR/deploy.log" 2>&1 || fail "Deploy failed"
log "  ✓ Program deployed"

# ─── Step 7: Generate FFI code ────────────────────────────────────────────

log "Step 7: Generating FFI code from IDL..."
FFI_OUT="$WORK_DIR/ffi_generated"
mkdir -p "$FFI_OUT"

"$CLIENT_GEN_BIN" --idl "$IDL_FILE" --out-dir "$FFI_OUT" \
    > "$WORK_DIR/client-gen.log" 2>&1 || fail "FFI generation failed (see $WORK_DIR/client-gen.log)"
log "  ✓ Generated client + FFI code"

# ─── Step 8: Verify generated FFI code structure ──────────────────────────

log "Step 8: Verifying generated FFI code..."

FFI_FILE="$FFI_OUT/"*_ffi.rs
HEADER_FILE="$FFI_OUT/"*.h

if [ ! -f "$FFI_FILE" ]; then
    fail "Generated FFI file not found"
fi

if [ ! -f "$HEADER_FILE" ]; then
    fail "Generated header file not found"
fi

# Verify FFI contains extern "C" functions
if grep -q 'extern "C"' "$FFI_FILE"; then
    log "  ✓ FFI code contains extern \"C\" declarations"
else
    warn "  ⚠ No extern \"C\" declarations in FFI"
fi

# Verify header contains function declarations
if grep -q 'char\*' "$HEADER_FILE"; then
    log "  ✓ Header contains function declarations"
else
    warn "  ⚠ No char* declarations in header"
fi

# Count generated functions
FN_COUNT=$(grep -c 'char\* ' "$HEADER_FILE" 2>/dev/null || echo "0")
log "  ✓ Generated ${FN_COUNT} FFI function declaration(s) in header"

# Verify account types are in the IDL
ACCOUNT_COUNT=$(python3 -c "import json; d=json.load(open('$IDL_FILE')); print(len(d.get('accounts', [])))")
log "  ✓ IDL contains ${ACCOUNT_COUNT} account type(s)"

if [ "$ACCOUNT_COUNT" -gt 0 ]; then
    log "  ✓ Account types are available for fetch_* generation"
else
    warn "  ⚠ No account types in IDL — fetch functions won't be generated"
fi

# ─── Done ──────────────────────────────────────────────────────────────────

log ""
log "🎉 FFI call test PASSED!"
log "  Generated files:"
log "    FFI:     $(basename "$FFI_FILE")"
log "    Header:  $(basename "$HEADER_FILE")"
log "    Client:  $(ls $FFI_OUT/*_client.rs 2>/dev/null | xargs basename 2>/dev/null || echo 'N/A')"
log "  Sequencer: ${SEQUENCER_URL}"
