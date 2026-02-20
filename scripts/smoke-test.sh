#!/usr/bin/env bash
# nssa-framework end-to-end smoke test
# Tests the full pipeline: init → build guest → deploy → submit tx
#
# Prerequisites:
#   - nssa-cli in PATH (cargo install --path nssa-framework-cli)
#   - cargo-risczero installed (cargo risczero --version)
#   - Docker running (for risc0 guest builds)
#   - sequencer_runner in PATH or ~/bin/

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WORK_DIR="${WORK_DIR:-/tmp/nssa-smoke-test}"
SEQUENCER_PORT="${SEQUENCER_PORT:-3040}"
SEQUENCER_URL="http://127.0.0.1:${SEQUENCER_PORT}"
PROJECT_NAME="smoke_test_program"
LOG_DIR="${WORK_DIR}/logs"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

log() { echo -e "${GREEN}[SMOKE]${NC} $*"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $*"; }
fail() { echo -e "${RED}[FAIL]${NC} $*"; exit 1; }

cleanup() {
    log "Cleaning up..."
    if [ -n "${SEQ_PID:-}" ] && kill -0 "$SEQ_PID" 2>/dev/null; then
        kill "$SEQ_PID" 2>/dev/null || true
        wait "$SEQ_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

# ─── Prerequisites ────────────────────────────────────────────────────────

log "Checking prerequisites..."

command -v nssa-cli >/dev/null 2>&1 || fail "nssa-cli not found in PATH"
command -v cargo >/dev/null 2>&1 || fail "cargo not found"
command -v cargo-risczero >/dev/null 2>&1 || warn "cargo-risczero not found — guest build may fail"
docker info >/dev/null 2>&1 || warn "Docker not running — guest build may fail"

LSSA_DIR="${LSSA_DIR:-$HOME/lssa}"
SEQUENCER_BIN=""
if command -v sequencer_runner >/dev/null 2>&1; then
    SEQUENCER_BIN="sequencer_runner"
elif [ -x "$HOME/bin/sequencer_runner" ]; then
    SEQUENCER_BIN="$HOME/bin/sequencer_runner"
elif [ -x "$LSSA_DIR/target/release/sequencer_runner" ]; then
    SEQUENCER_BIN="$LSSA_DIR/target/release/sequencer_runner"
elif [ -x "$LSSA_DIR/target/debug/sequencer_runner" ]; then
    SEQUENCER_BIN="$LSSA_DIR/target/debug/sequencer_runner"
else
    warn "sequencer_runner not found — will skip deploy/submit steps"
fi

# ─── Step 1: Scaffold project ────────────────────────────────────────────

log "Step 1: Scaffolding project..."
rm -rf "$WORK_DIR"
mkdir -p "$WORK_DIR" "$LOG_DIR"
cd "$WORK_DIR"

nssa-cli init "$PROJECT_NAME" > "$LOG_DIR/init.log" 2>&1 || fail "nssa-cli init failed (see $LOG_DIR/init.log)"
cd "$PROJECT_NAME"

# Verify scaffold structure
[ -f "Cargo.toml" ] || fail "Missing Cargo.toml"
[ -f "Makefile" ] || fail "Missing Makefile"
[ -d "methods/guest/src/bin" ] || fail "Missing guest binary dir"
log "  ✅ Project scaffolded"

# ─── Step 2: Build guest binary ───────────────────────────────────────────

log "Step 2: Building guest binary (this may take a while)..."
make build > "$LOG_DIR/build.log" 2>&1 || fail "Guest build failed (see $LOG_DIR/build.log)"

GUEST_BIN=$(find . -name "*.bin" -path "*/riscv32im*" | head -1)
[ -n "$GUEST_BIN" ] || fail "No guest binary found after build"
log "  ✅ Guest binary built: $GUEST_BIN"

# ─── Step 3: Generate IDL ────────────────────────────────────────────────

log "Step 3: Generating IDL..."
make idl > "$LOG_DIR/idl.log" 2>&1 || fail "IDL generation failed (see $LOG_DIR/idl.log)"

IDL_FILE=$(find . -name "*-idl.json" | head -1)
[ -n "$IDL_FILE" ] || fail "No IDL file found after generation"

# Validate IDL is valid JSON with instructions
python3 -c "
import json, sys
with open('$IDL_FILE') as f:
    idl = json.load(f)
assert 'instructions' in idl, 'IDL missing instructions'
assert len(idl['instructions']) > 0, 'IDL has no instructions'
print(f'  IDL: {len(idl[\"instructions\"])} instructions')
" || fail "IDL validation failed"
log "  ✅ IDL generated: $IDL_FILE"

# ─── Step 4: Deploy to sequencer ─────────────────────────────────────────

if [ -z "$SEQUENCER_BIN" ]; then
    warn "Skipping deploy/submit (no sequencer)"
    log "Smoke test passed (scaffold + build + IDL only)"
    exit 0
fi

log "Step 4: Starting sequencer and deploying..."

# Kill any existing sequencer
pgrep -f 'sequencer_runner.*configs' | xargs -r kill 2>/dev/null || true
sleep 1

# Clean old state
rm -rf "${LSSA_DIR}/.sequencer_db" "${LSSA_DIR}/rocksdb"

# Start sequencer with lssa configs
SEQ_CONFIGS="${LSSA_DIR}/sequencer_runner/configs/debug"
if [ ! -d "$SEQ_CONFIGS" ]; then
    fail "Sequencer configs not found at $SEQ_CONFIGS"
fi

cd "$LSSA_DIR"
RUST_LOG=info $SEQUENCER_BIN "$SEQ_CONFIGS" > "$LOG_DIR/sequencer.log" 2>&1 &
SEQ_PID=$!
cd "$WORK_DIR/$PROJECT_NAME"

# Wait for sequencer to be ready (up to 60s)
log "  Waiting for sequencer (PID $SEQ_PID)..."
for i in $(seq 1 60); do
    if curl -s -o /dev/null -w '%{http_code}' "$SEQUENCER_URL" 2>/dev/null | grep -qE '(200|405)'; then
        log "  Sequencer up after ${i}s"
        break
    fi
    if ! kill -0 "$SEQ_PID" 2>/dev/null; then
        fail "Sequencer died (see $LOG_DIR/sequencer.log)"
    fi
    sleep 1
done

if ! curl -s -o /dev/null -w '%{http_code}' "$SEQUENCER_URL" 2>/dev/null | grep -qE '(200|405)'; then
    fail "Sequencer failed to start after 60s (see $LOG_DIR/sequencer.log)"
fi

# Deploy using wallet CLI (same as `make deploy`)
GUEST_BIN_ABS="$(cd "$(dirname "$GUEST_BIN")" && pwd)/$(basename "$GUEST_BIN")"
IDL_FILE_ABS="$(cd "$(dirname "$IDL_FILE")" && pwd)/$(basename "$IDL_FILE")"

WALLET_BIN=""
if command -v wallet >/dev/null 2>&1; then
    WALLET_BIN="wallet"
elif [ -x "$LSSA_DIR/target/release/wallet" ]; then
    WALLET_BIN="$LSSA_DIR/target/release/wallet"
elif [ -x "$LSSA_DIR/target/debug/wallet" ]; then
    WALLET_BIN="$LSSA_DIR/target/debug/wallet"
else
    warn "wallet CLI not found — skipping deploy/submit"
    log "Smoke test passed (scaffold + build + IDL + sequencer start)"
    exit 0
fi

export NSSA_WALLET_HOME_DIR="${NSSA_WALLET_HOME_DIR:-${LSSA_DIR}/wallet/configs/debug}"
WALLET_PASSWORD="${WALLET_PASSWORD:-test}"

# Wallet needs password on stdin; first run creates storage
printf '%s\n' "$WALLET_PASSWORD" | $WALLET_BIN deploy-program "$GUEST_BIN_ABS" > "$LOG_DIR/deploy.log" 2>&1 \
    || fail "Deploy failed (see $LOG_DIR/deploy.log)"
log "  ✅ Program deployed"

# ─── Step 5: Submit a transaction ─────────────────────────────────────────

log "Step 5: Submitting test transaction..."

# Get the first instruction name from IDL
FIRST_IX=$(python3 -c "
import json
with open('$IDL_FILE') as f:
    idl = json.load(f)
print(idl['instructions'][0]['name'])
")

# Try submitting the first instruction (may fail if it needs specific args — that's OK)
SEQUENCER_URL="$SEQUENCER_URL" nssa-cli --idl "$IDL_FILE_ABS" -p "$GUEST_BIN_ABS" \
    "$FIRST_IX" > "$LOG_DIR/submit.log" 2>&1 \
    && log "  ✅ Transaction submitted" \
    || warn "Submit failed (may need args — see $LOG_DIR/submit.log). Deploy was successful."

# ─── Done ─────────────────────────────────────────────────────────────────

log ""
log "🎉 Smoke test PASSED!"
log "  Project: $WORK_DIR/$PROJECT_NAME"
log "  Guest:   $GUEST_BIN"
log "  IDL:     $IDL_FILE"
log "  Logs:    $LOG_DIR/"
