#!/usr/bin/env bash
# SPEL FFI Call Test
# Scaffolds a project via `spel init`, builds it, deploys to a live sequencer,
# generates FFI code from the IDL, and verifies the generated code structure.
# Note: does not currently call the generated fetch_* functions — that is planned.
#
# Usage: ./ffi-call-test.sh [WORK_DIR]
#
# Required Environment Variables:
#   LEZ_TAG     - LEZ revision/tag to test against
#   LSSA_DIR    - Path to logos-execution-zone directory with sequencer built

set -euo pipefail

export RISC0_DEV_MODE=1


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
SPEL_TAG="${SPEL_TAG:-local}"

# ─── Setup ─────────────────────────────────────────────────────────────────

log "Setting up in ${WORK_DIR}..."
rm -rf "$WORK_DIR"
mkdir -p "$WORK_DIR"
cd "$WORK_DIR"

# Use spel-cli from shared artifacts
SPEL_BIN="/tmp/lssa/target/release/spel"

    
# Build spel-client-gen CLI
log "Building spel-client-gen..."
cargo build --manifest-path "$SPEL_DIR/Cargo.toml" -p spel-client-gen --release \
    > "$WORK_DIR/client-gen-build.log" 2>&1 || fail "Failed to build spel-client-gen"
CLIENT_GEN_BIN="$SPEL_DIR/target/release/spel-client-gen"

# ─── Step 1: Scaffold project ──────────────────────────────────────────────

log "Step 1: Creating SPEL project (LEZ=${LEZ_TAG})..."
"$SPEL_BIN" init --lez-tag "$LEZ_TAG" --spel-rev "$SPEL_TAG" "$PROJECT_NAME" 2>&1 | tee "$WORK_DIR/init.log" || { echo ''; echo '=== INIT LOG ==='; cat "$WORK_DIR/init.log"; echo '================='; fail "spel init failed"; }
cd "$PROJECT_NAME"

# Regenerate lockfiles so the patch takes effect
(cd methods/guest && cargo generate-lockfile > "$WORK_DIR/guest-lockfile.log" 2>&1) \
    || warn "Guest lockfile regeneration failed"
cargo generate-lockfile > "$WORK_DIR/root-lockfile.log" 2>&1 \
    || warn "Root lockfile regeneration failed"

log "  ✓ Project scaffolded"

# ─── Step 2: Build guest binary ───────────────────────────────────────────

log "Step 2: Building guest binary..."
RISC0_SKIP_BUILD= make build 2>&1 | tee "$WORK_DIR/build.log" || { echo ''; echo '=== BUILD LOG ==='; cat "$WORK_DIR/build.log"; echo '================='; fail 'Guest binary build failed'; }
GUEST_BIN=$(find . -name "*.bin" -path "*/riscv32im*" | head -1)
[ -n "$GUEST_BIN" ] || fail "No guest binary found"
GUEST_BIN_ABS="$(realpath "$GUEST_BIN")"
log "  ✓ Built: $(basename "$GUEST_BIN")"

# ─── Step 3: Generate IDL ─────────────────────────────────────────────────

log "Step 3: Generating IDL..."
make idl 2>&1 | tee "$WORK_DIR/idl.log" > /dev/null || { echo ''; echo '=== IDL LOG ==='; cat "$WORK_DIR/idl.log"; echo '================='; fail 'IDL generation failed'; }
IDL_FILE=$(find . -name "*-idl.json" | head -1)
[ -n "$IDL_FILE" ] || fail "No IDL found"
log "  ✓ IDL: $(basename "$IDL_FILE")"

# ─── Step 4: Start sequencer ──────────────────────────────────────────────

log "Step 4: Starting sequencer on port ${SEQUENCER_PORT}..."
pgrep -f 'sequencer_service.*configs' | xargs -r kill 2>/dev/null || true
sleep 1
rm -rf "${LSSA_DIR}/rocksdb-${SEQUENCER_PORT}"

SEQ_CONFIGS="${LSSA_DIR}/sequencer/service/configs/debug/sequencer_config.json"
if [ ! -f "$SEQ_CONFIGS" ]; then
    SEQ_CONFIGS=$(find "$LSSA_DIR" -name "sequencer_config.json" 2>/dev/null | head -1)
fi
[ -n "$SEQ_CONFIGS" ] || fail "Sequencer config not found"

cd "$LSSA_DIR"
RUST_LOG=info $SEQUENCER_BIN --port "$SEQUENCER_PORT" "$SEQ_CONFIGS" > "$WORK_DIR/sequencer.log" 2>&1 &
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

# ─── Step 5: Update wallet config for correct port ────────────────────────

log "Step 5: Updating wallet config for port ${SEQUENCER_PORT}..."
WALLET_CONFIG="${NSSA_WALLET_HOME_DIR}/wallet_config.json"
if [ -f "$WALLET_CONFIG" ]; then
    python3 -c "
import json
with open('$WALLET_CONFIG', 'r') as f:
    config = json.load(f)
config['sequencer_addr'] = '$SEQUENCER_URL'
with open('$WALLET_CONFIG', 'w') as f:
    json.dump(config, f, indent=4)
print('  ✓ Updated wallet config to use $SEQUENCER_URL')
" || warn "Failed to update wallet config"
else
    warn "Wallet config not found at $WALLET_CONFIG"
fi

# ─── Step 6: Deploy program ───────────────────────────────────────────────

log "Step 6: Deploying program..."
printf '%s\n' "$WALLET_PASSWORD" | $WALLET_BIN deploy-program "$GUEST_BIN_ABS" 2>&1 | tee "$WORK_DIR/deploy.log" || { echo ''; echo '=== DEPLOY LOG ==='; cat "$WORK_DIR/deploy.log"; echo '==================='; fail 'Deploy failed'; }
log "  ✓ Program deployed"

# ─── Step 6: Generate FFI code ────────────────────────────────────────────

log "Step 6: Generating FFI code from IDL..."
FFI_OUT="$WORK_DIR/ffi_generated"
mkdir -p "$FFI_OUT"

"$CLIENT_GEN_BIN" --idl "$IDL_FILE" --out-dir "$FFI_OUT" \
    > "$WORK_DIR/client-gen.log" 2>&1 || fail "FFI generation failed (see $WORK_DIR/client-gen.log)"
log "  ✓ Generated client + FFI code"

# ─── Step 7: Verify generated FFI code structure ──────────────────────────

log "Step 7: Verifying generated FFI code..."

FFI_FILE=$(find "$FFI_OUT" -name "*_ffi.rs" | head -1)
HEADER_FILE=$(find "$FFI_OUT" -name "*.h" | head -1)

if [ -z "$FFI_FILE" ] || [ ! -f "$FFI_FILE" ]; then
    fail "Generated FFI file not found (looked in $FFI_OUT)"
fi

if [ -z "$HEADER_FILE" ] || [ ! -f "$HEADER_FILE" ]; then
    fail "Generated header file not found (looked in $FFI_OUT)"
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
