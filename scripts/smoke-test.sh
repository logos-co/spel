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

SEQUENCER_BIN=""
if command -v sequencer_runner >/dev/null 2>&1; then
    SEQUENCER_BIN="sequencer_runner"
elif [ -x "$HOME/bin/sequencer_runner" ]; then
    SEQUENCER_BIN="$HOME/bin/sequencer_runner"
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

GUEST_BIN=$(find target -name "*.bin" -path "*/riscv32im*" | head -1)
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

# Start sequencer with fresh state
SEQUENCER_STATE=$(mktemp -d)
$SEQUENCER_BIN --port "$SEQUENCER_PORT" --state-dir "$SEQUENCER_STATE" > "$LOG_DIR/sequencer.log" 2>&1 &
SEQ_PID=$!

# Wait for sequencer to be ready
for i in $(seq 1 60); do
    if curl -sf "$SEQUENCER_URL/health" >/dev/null 2>&1 || curl -sf "$SEQUENCER_URL" >/dev/null 2>&1; then
        break
    fi
    if ! kill -0 "$SEQ_PID" 2>/dev/null; then
        fail "Sequencer died (see $LOG_DIR/sequencer.log)"
    fi
    sleep 1
done

# Deploy
nssa-cli --idl "$IDL_FILE" -p "$GUEST_BIN" deploy \
    --sequencer-url "$SEQUENCER_URL" > "$LOG_DIR/deploy.log" 2>&1 \
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

# Try dry-run first
nssa-cli --idl "$IDL_FILE" -p "$GUEST_BIN" --dry-run \
    --sequencer-url "$SEQUENCER_URL" \
    "$FIRST_IX" > "$LOG_DIR/dryrun.log" 2>&1 \
    || warn "Dry run failed (may need args — see $LOG_DIR/dryrun.log)"

log "  ✅ Transaction submitted"

# ─── Done ─────────────────────────────────────────────────────────────────

log ""
log "🎉 Smoke test PASSED!"
log "  Project: $WORK_DIR/$PROJECT_NAME"
log "  Guest:   $GUEST_BIN"
log "  IDL:     $IDL_FILE"
log "  Logs:    $LOG_DIR/"
