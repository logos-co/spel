#!/usr/bin/env bash
# SPEL Privacy Smoke Test
# Verifies both public and Private/ prefixed transactions work end-to-end
# including auth-transfer init for the private account.
#
# Usage: ./smoke-test-privacy.sh [WORK_DIR]

set -euo pipefail

export RISC0_DEV_MODE=1
export RISC0_SKIP_BUILD=1

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WORK_DIR="${1:-/tmp/spel-privacy-smoke}"
SEQUENCER_PORT="${SEQUENCER_PORT:-3040}"
SEQUENCER_URL="http://127.0.0.1:${SEQUENCER_PORT}"
PROJECT_NAME="privacy_test"
LOG_DIR="${WORK_DIR}/logs"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

log()  { echo -e "${GREEN}[PRIVACY]${NC} $*"; }
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

command -v spel >/dev/null 2>&1 || fail "spel not found"
command -v cargo >/dev/null 2>&1 || fail "cargo not found"

LSSA_DIR="${LSSA_DIR:-$HOME/lssa}"
SEQUENCER_BIN=""
for candidate in sequencer_service "$HOME/bin/sequencer_service" "$LSSA_DIR/target/release/sequencer_service"; do
    if command -v "$candidate" >/dev/null 2>&1 || [ -x "$candidate" ]; then
        SEQUENCER_BIN="$candidate"; break
    fi
done
[ -n "$SEQUENCER_BIN" ] || fail "sequencer_service not found"

WALLET_BIN=""
for candidate in wallet "$HOME/bin/wallet" "$LSSA_DIR/target/release/wallet"; do
    if command -v "$candidate" >/dev/null 2>&1 || [ -x "$candidate" ]; then
        WALLET_BIN="$candidate"; break
    fi
done
[ -n "$WALLET_BIN" ] || fail "wallet not found"

export NSSA_WALLET_HOME_DIR="${NSSA_WALLET_HOME_DIR:-${LSSA_DIR}/wallet/configs/debug}"
WALLET_PASSWORD="${WALLET_PASSWORD:-test}"

# ─── Setup ─────────────────────────────────────────────────────────────────

log "Setting up in ${WORK_DIR}..."
rm -rf "$WORK_DIR"
mkdir -p "$WORK_DIR" "$LOG_DIR"
cd "$WORK_DIR"

# ─── Step 1: Scaffold project ──────────────────────────────────────────────

log "Step 1: Creating SPEL project..."
spel init "$PROJECT_NAME" > "$LOG_DIR/init.log" 2>&1 || fail "spel init failed"
cd "$PROJECT_NAME"
log "  ✅ Project scaffolded"

# ─── Step 2: Modify guest program for privacy test ────────────────────────

log "Step 2: Setting up test program..."

# Replace the default scaffold with a simple greet instruction
cat > "methods/guest/src/bin/${PROJECT_NAME}.rs" << 'RUSTEOF'
#![no_main]
use spel_framework::prelude::*;
use nssa_core::account::Data;

risc0_zkvm::guest::entry!(main);

#[lez_program]
mod privacy_test {
    use super::*;

    /// Greet: appends greeting bytes to account data.
    /// For default (unclaimed) accounts: claims and writes data.
    /// For already-owned accounts: returns unchanged (privacy TX compatible).
    #[instruction]
    pub fn greet(
        #[account(mut)]
        account: AccountWithMetadata,
        greeting: Vec<u8>,
    ) -> SpelResult {
        let acc = account.account.clone();

        let post = if acc.program_owner == nssa_core::program::DEFAULT_PROGRAM_ID {
            // Unclaimed account: claim it and write greeting
            let mut acc = acc;
            let mut data: Vec<u8> = acc.data.into();
            data.extend_from_slice(&greeting);
            acc.data = Data::try_from(data)
                .map_err(|_| SpelError::custom(999, "data too big"))?;
            AccountPostState::new_claimed(acc)
        } else {
            // Already owned (e.g. by auth-transfer): return unchanged
            AccountPostState::new(acc)
        };

        Ok(SpelOutput::states_only(vec![post]))
    }
}
RUSTEOF

log "  ✅ Guest program configured"

# ─── Step 3: Build guest binary ───────────────────────────────────────────

log "Step 3: Building guest binary (RISC0_DEV_MODE=1)..."
RISC0_SKIP_BUILD= make build > "$LOG_DIR/build.log" 2>&1 || { cat "$LOG_DIR/build.log"; fail "Build failed"; }
GUEST_BIN=$(find . -name "*.bin" -path "*/riscv32im*" | head -1)
[ -n "$GUEST_BIN" ] || fail "No guest binary found"
GUEST_BIN_ABS="$(realpath "$GUEST_BIN")"
log "  ✅ Built: $(basename "$GUEST_BIN")"

# ─── Step 4: Generate IDL ─────────────────────────────────────────────────

log "Step 4: Generating IDL..."
make idl > "$LOG_DIR/idl.log" 2>&1 || fail "IDL generation failed"
IDL_FILE=$(find . -name "*-idl.json" | head -1)
[ -n "$IDL_FILE" ] || fail "No IDL found"
IDL_ABS="$(realpath "$IDL_FILE")"
log "  ✅ IDL: $(basename "$IDL_FILE")"

# ─── Step 5: Start sequencer ──────────────────────────────────────────────

log "Step 5: Starting sequencer..."
pgrep -f 'sequencer_service.*configs' | xargs -r kill 2>/dev/null || true
sleep 1
rm -rf "${LSSA_DIR}/rocksdb"

SEQ_CONFIGS="${LSSA_DIR}/sequencer/service/configs/debug/sequencer_config.json"
[ -f "$SEQ_CONFIGS" ] || fail "Sequencer config not found"

cd "$LSSA_DIR"
RUST_LOG=warn $SEQUENCER_BIN "$SEQ_CONFIGS" > "$LOG_DIR/sequencer.log" 2>&1 &
SEQ_PID=$!
cd "$WORK_DIR/$PROJECT_NAME"

log "  Waiting for sequencer..."
for i in $(seq 1 60); do
    if curl -sf -o /dev/null -w '%{http_code}' "$SEQUENCER_URL" 2>/dev/null | grep -qE '200|405'; then
        log "  ✅ Sequencer up"; break
    fi
    kill -0 "$SEQ_PID" 2>/dev/null || fail "Sequencer died"
    sleep 1
done

# Wait for first block to be produced before proceeding
log "  Waiting for first block..."
for i in $(seq 1 60); do
    LAST_BLOCK=$(curl -sf -X POST "$SEQUENCER_URL" \
        -H 'Content-Type: application/json' \
        -d '{"jsonrpc":"2.0","method":"getLastBlockId","params":[],"id":1}' 2>/dev/null \
        | python3 -c "import json,sys; r=json.load(sys.stdin); print(r.get('result',0))" 2>/dev/null || echo 0)
    if [ "${LAST_BLOCK:-0}" -gt 0 ] 2>/dev/null; then
        log "  ✅ First block produced (block $LAST_BLOCK)"
        break
    fi
    sleep 2
done

# ─── Step 6: Deploy ───────────────────────────────────────────────────────

log "Step 6: Deploying program..."
printf '%s\n' "$WALLET_PASSWORD" | $WALLET_BIN deploy-program "$GUEST_BIN_ABS" \
    > "$LOG_DIR/deploy.log" 2>&1 || fail "Deploy failed"
log "  ✅ Program deployed"

# ─── Step 7: Generate test accounts ───────────────────────────────────────

log "Step 7: Generating test accounts..."

# Create a public account (random)
PUBLIC_ACCOUNT="0x$(openssl rand -hex 32)"
log "  Public account: ${PUBLIC_ACCOUNT:0:20}..."

# Create a private account via wallet (wallet holds the ZK keys)
PRIVATE_ACCOUNT=$(echo "$WALLET_PASSWORD" | $WALLET_BIN account new private 2>&1 | grep -o "Private/[^ ]*" | head -1)
[ -n "$PRIVATE_ACCOUNT" ] || fail "Could not create private account from wallet"
log "  Private account: ${PRIVATE_ACCOUNT:0:30}..."

# ─── Step 8: Test PUBLIC transaction ────────────────────────────────────

log "Step 8: Testing PUBLIC transaction..."
FRESH_ACCOUNT="0x$(openssl rand -hex 32)"

SEQUENCER_URL="$SEQUENCER_URL" spel --idl "$IDL_ABS" -p "$GUEST_BIN_ABS" \
    greet \
    --account "$FRESH_ACCOUNT" \
    --greeting "72,101,108,108,111,32,80,117,98,108,105,99" \
    > "$LOG_DIR/public-tx.log" 2>&1 || fail "Public TX failed (see $LOG_DIR/public-tx.log)"

log "  ✅ Public TX submitted and confirmed"

# ─── Step 9: Init auth-transfer for private account ─────────────────────

log "Step 9: Initializing auth-transfer for private account..."
echo "$WALLET_PASSWORD" | $WALLET_BIN auth-transfer init --account-id "$PRIVATE_ACCOUNT" \
    > "$LOG_DIR/auth-transfer.log" 2>&1 || fail "auth-transfer init failed (see $LOG_DIR/auth-transfer.log)"
log "  ✅ auth-transfer initialized"

# Wait for auth-transfer TX to be included in a block
log "  Waiting for auth-transfer to be confirmed..."
sleep 20

# ─── Step 10: Test PRIVACY-PRESERVING transaction ───────────────────────

log "Step 10: Testing PRIVACY-PRESERVING transaction..."
SEQUENCER_URL="$SEQUENCER_URL" spel --idl "$IDL_ABS" -p "$GUEST_BIN_ABS" \
    greet \
    --account "$PRIVATE_ACCOUNT" \
    --greeting "72,101,108,108,111,32,80,114,105,118,97,116,101" \
    > "$LOG_DIR/private-tx.log" 2>&1 || { cat "$LOG_DIR/private-tx.log"; fail "Private TX failed"; }

log "  ✅ Privacy-preserving TX submitted and confirmed"

# ─── Done ─────────────────────────────────────────────────────────────────

log ""
log "🎉 Privacy smoke test PASSED!"
log "  Public TX:       $LOG_DIR/public-tx.log"
log "  Auth-transfer:   $LOG_DIR/auth-transfer.log"
log "  Private TX:      $LOG_DIR/private-tx.log"
log "  Sequencer:       $LOG_DIR/sequencer.log"
