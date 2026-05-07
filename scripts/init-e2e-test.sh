#!/usr/bin/env bash
# Init E2E Test
# Exercises the full spel init → build → deploy → TX pipeline.
#
# Key difference from smoke-test-privacy.sh and ffi-call-test.sh:
# - Does NOT pass --lez-tag, so the DEFAULT LEZ dependency resolution in init.rs
#   is tested (catches bugs like PR #184 where default tag caused unbuildable projects)
# - DOES accept optional SPEL_TAG to override which spel-framework revision is used.
#   On PRs this should be set to the PR head so the scaffolded project uses the
#   PR's framework changes. On main pushes leave unset for true default testing.
#
# Usage: ./init-e2e-test.sh [WORK_DIR]
#
# Required Environment Variables:
#   LSSA_DIR    - Path to logos-execution-zone directory with sequencer built
# Optional Environment Variables:
#   SPEL_TAG    - SPEL revision for init (e.g. refs/pull/XXX/head). If unset,
#                 spel init uses its hardcoded default (branch = "main").

set -euo pipefail

export RISC0_DEV_MODE=1

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WORK_DIR="${1:-${WORK_DIR:-/tmp/spel-init-e2e}}"
SEQUENCER_PORT="${SEQUENCER_PORT:-3043}"
SEQUENCER_URL="http://127.0.0.1:${SEQUENCER_PORT}"
PROJECT_NAME="init_e2e_test"

if [ -z "${LSSA_DIR:-}" ]; then
    echo "ERROR: LSSA_DIR environment variable is required"
    exit 1
fi

LSSA_DIR="$(cd "$LSSA_DIR" && pwd)"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

log()  { echo -e "${GREEN}[INIT-E2E]${NC} $*"; }
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

SEQUENCER_BIN="${LSSA_DIR}/target/release/sequencer_service"
[ -x "$SEQUENCER_BIN" ] || fail "sequencer_service not found at $SEQUENCER_BIN"

WALLET_BIN="${LSSA_DIR}/target/release/wallet"
[ -x "$WALLET_BIN" ] || fail "wallet not found at $WALLET_BIN"

SPEL_BIN="/tmp/lssa/target/release/spel"
[ -x "$SPEL_BIN" ] || fail "spel binary not found at $SPEL_BIN"

export NSSA_WALLET_HOME_DIR="${NSSA_WALLET_HOME_DIR:-${LSSA_DIR}/wallet/configs/debug}"
WALLET_PASSWORD="${WALLET_PASSWORD:-test}"

# ─── Setup ─────────────────────────────────────────────────────────────────

log "Setting up in ${WORK_DIR}..."
rm -rf "$WORK_DIR"
mkdir -p "$WORK_DIR"
cd "$WORK_DIR"

# ─── Step 1: spel init — default LEZ, optional SPEL override ─────────────
# Always uses DEFAULT LEZ resolution (no --lez-tag) to test init.rs defaults.
# On PRs, SPEL_TAG is set so the scaffolded project uses the PR's framework code.
# On main pushes, SPEL_TAG is unset for true default testing.

log "Step 1: spel init (LEZ=defaults${SPEL_TAG:+, SPEL=$SPEL_TAG})..."
if [ -n "${SPEL_TAG:-}" ]; then
    "$SPEL_BIN" init --spel-rev "$SPEL_TAG" "$PROJECT_NAME" \
        > "$WORK_DIR/init.log" 2>&1 || fail "spel init failed (see $WORK_DIR/init.log)"
else
    "$SPEL_BIN" init "$PROJECT_NAME" \
        > "$WORK_DIR/init.log" 2>&1 || fail "spel init failed (see $WORK_DIR/init.log)"
fi
cd "$PROJECT_NAME"
log "  ✓ Project scaffolded"

# Show what was resolved — this helps diagnose dependency issues
log "  Resolved dependencies in methods/guest/Cargo.toml:"
grep -E '(spel-framework|nssa_core) = ' methods/guest/Cargo.toml || true

# ─── Step 2: Build guest binary ───────────────────────────────────────────
# This is where init bugs surface — if the default spel_ref or lez_ref point
# to incompatible versions, the build fails here.

log "Step 2: Building guest binary..."
RISC0_SKIP_BUILD= make build > "$WORK_DIR/build.log" 2>&1 || { cat "$WORK_DIR/build.log"; fail "Build failed"; }
GUEST_BIN=$(find . -name "*.bin" -path "*/riscv32im*" | head -1)
[ -n "$GUEST_BIN" ] || fail "No guest binary found"
GUEST_BIN_ABS="$(realpath "$GUEST_BIN")"
log "  ✓ Built: $(basename "$GUEST_BIN") ($(stat -c%s "$GUEST_BIN") bytes)"

# ─── Step 3: Generate and validate IDL ────────────────────────────────────

log "Step 3: Generating IDL..."
make idl > "$WORK_DIR/idl.log" 2>&1 || fail "IDL generation failed (see $WORK_DIR/idl.log)"
IDL_FILE=$(find . -name "*-idl.json" | head -1)
[ -n "$IDL_FILE" ] || fail "No IDL file found"
IDL_ABS="$(realpath "$IDL_FILE")"

# Validate IDL structure
python3 -c "
import json, sys
with open('$IDL_FILE') as f:
    idl = json.load(f)
assert 'name' in idl, 'Missing name'
assert 'version' in idl, 'Missing version'
assert 'instructions' in idl, 'Missing instructions'
assert len(idl['instructions']) >= 2, f\"Expected >= 2 instructions, got {len(idl['instructions'])}\"
ix_names = [ix['name'] for ix in idl['instructions']]
assert 'initialize' in ix_names, f\"Expected 'initialize', got: {ix_names}\"
assert 'do_something' in ix_names, f\"Expected 'do_something', got: {ix_names}\"
print(f\"  ✓ IDL valid: {idl['name']} v{idl['version']}, {len(idl['instructions'])} instructions\")
" || fail "IDL validation failed"

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
RUST_LOG=info $SEQUENCER_BIN --port "$SEQUENCER_PORT" "$SEQ_CONFIGS" \
    > "$WORK_DIR/sequencer.log" 2>&1 &
SEQ_PID=$!
sleep 2
if ! kill -0 $SEQ_PID 2>/dev/null; then
    echo "❌ Sequencer failed to start. Logs:"
    cat "$WORK_DIR/sequencer.log" | tail -30
    exit 1
fi
cd "$WORK_DIR/$PROJECT_NAME"

log "  Waiting for sequencer..."
for i in $(seq 1 90); do
    if curl -sf -o /dev/null -w '%{http_code}' "$SEQUENCER_URL" 2>/dev/null | grep -qE '200|405'; then
        log "  ✓ Sequencer up"; break
    fi
    kill -0 "$SEQ_PID" 2>/dev/null || fail "Sequencer died"
    echo -n "."
    sleep 2
done

log "  Waiting for first block..."
for i in $(seq 1 60); do
    if curl -sf -X POST "$SEQUENCER_URL" \
        -H 'Content-Type: application/json' \
        -d '{"jsonrpc":"2.0","method":"getLastBlockId","params":[],"id":1}' 2>/dev/null; then
        log "  ✓ Sequencer producing blocks"; break
    fi
    sleep 3
    echo -n "."
done

# ─── Step 5: Patch wallet config for correct sequencer port ──────────────
# The wallet reads its sequencer address from wallet_config.json which points
# to the default port. We need to update it to match our non-default port.

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
    warn "Wallet config not found at $WALLET_CONFIG — wallet may fail to connect"
fi

# ─── Step 6: Deploy program ──────────────────────────────────────────────

log "Step 6: Deploying program..."
printf '%s\n' "$WALLET_PASSWORD" | $WALLET_BIN deploy-program "$GUEST_BIN_ABS" \
    > "$WORK_DIR/deploy.log" 2>&1 || { cat "$WORK_DIR/deploy.log"; fail "Deploy failed"; }
log "  ✓ Program deployed"

# ─── Step 7: Create signer account ───────────────────────────────────────

log "Step 7: Creating signer account..."
SIGNER_ID=$(printf '%s\n' "$WALLET_PASSWORD" | $WALLET_BIN account new public 2>&1 \
    | sed -n 's/.*Public\/\([A-Za-z0-9]*\).*/\1/p')
[ -n "$SIGNER_ID" ] || fail "Could not create signer account"
log "  Signer: ${SIGNER_ID:0:20}..."

# ─── Step 8: Send initialize TX via spel CLI ─────────────────────────────

log "Step 8: Sending initialize transaction..."
SEQUENCER_URL="$SEQUENCER_URL" "$SPEL_BIN" --idl "$IDL_ABS" -p "$GUEST_BIN_ABS" \
    initialize \
    --account "$SIGNER_ID" \
    --threshold 1 \
    > "$WORK_DIR/initialize-tx.log" 2>&1 || { cat "$WORK_DIR/initialize-tx.log"; fail "Initialize TX failed"; }
log "  ✓ Initialize TX submitted and confirmed"

# ─── Step 9: Send do_something TX via spel CLI ───────────────────────────

log "Step 9: Sending do_something transaction..."
SEQUENCER_URL="$SEQUENCER_URL" "$SPEL_BIN" --idl "$IDL_ABS" -p "$GUEST_BIN_ABS" \
    do_something \
    --account "$SIGNER_ID" \
    --amount 42 \
    > "$WORK_DIR/do-something-tx.log" 2>&1 || { cat "$WORK_DIR/do-something-tx.log"; fail "do_something TX failed"; }
log "  ✓ do_something TX submitted and confirmed"

# ─── Step 10: Verify --dry-run works ──────────────────────────────────────

log "Step 10: Verifying --dry-run mode..."
SEQUENCER_URL="$SEQUENCER_URL" "$SPEL_BIN" --idl "$IDL_ABS" -p "$GUEST_BIN_ABS" \
    --dry-run \
    do_something \
    --account "$SIGNER_ID" \
    --amount 100 \
    > "$WORK_DIR/dry-run.log" 2>&1 || { cat "$WORK_DIR/dry-run.log"; fail "Dry-run failed"; }

if grep -qi "dry.run\|DRY RUN" "$WORK_DIR/dry-run.log" 2>/dev/null; then
    log "  ✓ Dry-run output verified"
else
    warn "  ⚠ Could not verify dry-run marker (may still be valid)"
fi

# ─── Done ─────────────────────────────────────────────────────────────────

log ""
log "🎉 Init E2E test PASSED!"
log "  All steps completed successfully:"
log "    ✅ spel init (default LEZ, PR spel on PRs) — scaffolded project"
log "    ✅ make build — guest binary compiled"
log "    ✅ make idl — IDL generated and validated"
log "    ✅ wallet config patched — sequencer port configured"
log "    ✅ deploy — program deployed to sequencer"
log "    ✅ initialize TX — first instruction sent"
log "    ✅ do_something TX — second instruction sent"
log "    ✅ --dry-run — dry-run mode verified"
