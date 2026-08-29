#!/usr/bin/env bash
# Multisig E2E Test (witness exchange)
# Exercises the export → sign → submit round trip against a live sequencer,
# with two freshly created wallets standing in for the two signers:
#   - wallet A builds a partial transaction with --export and --co-signer
#   - wallet B signs it with `spel sign`
#   - wallet B submits it with `spel submit` and the TX must confirm
#
# Failing paths asserted along the way:
#   - `spel submit` while a witness is still missing is rejected
#   - `spel submit` of a blob tampered with after signing is rejected
#
# The sequencer is started by this script on its own port; it never touches
# an already-running stack. Both wallets are created from scratch in WORK_DIR
# so the flow proves key isolation: wallet B holds no key of wallet A.
#
# Usage: ./multisig-e2e-test.sh [WORK_DIR]
#
# Required Environment Variables:
#   LEZ_TAG     - LEZ revision/tag to test against
#   LSSA_DIR    - Path to logos-execution-zone directory with sequencer built
# Optional Environment Variables:
#   SPEL_TAG    - SPEL revision for init (e.g. refs/pull/XXX/head)
#   SPEL_GIT    - SPEL git URL for init (fork testing)
#   SPEL_BIN    - Path to the spel binary (default /tmp/lssa/target/release/spel)

set -euo pipefail

export RISC0_DEV_MODE=1

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WORK_DIR="${1:-${WORK_DIR:-/tmp/spel-multisig-e2e}}"
SEQUENCER_PORT="${SEQUENCER_PORT:-3044}"
SEQUENCER_URL="http://127.0.0.1:${SEQUENCER_PORT}"
PROJECT_NAME="multisig_e2e_test"

if [ -z "${LEZ_TAG:-}" ]; then
    echo "ERROR: LEZ_TAG environment variable is required"
    exit 1
fi

if [ -z "${LSSA_DIR:-}" ]; then
    echo "ERROR: LSSA_DIR environment variable is required"
    exit 1
fi

LSSA_DIR="$(cd "$LSSA_DIR" && pwd)"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

log()  { echo -e "${GREEN}[MULTISIG-E2E]${NC} $*"; }
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

SPEL_BIN="${SPEL_BIN:-/tmp/lssa/target/release/spel}"
[ -x "$SPEL_BIN" ] || fail "spel binary not found at $SPEL_BIN"

# Wallet config template. LEZ v0.2.0 moved the debug configs under a lez/
# subdirectory; prefer the new location, fall back to the pre-rc6 path.
if [ -f "${LSSA_DIR}/lez/wallet/configs/debug/wallet_config.json" ]; then
    WALLET_CONFIG_TEMPLATE="${LSSA_DIR}/lez/wallet/configs/debug/wallet_config.json"
else
    WALLET_CONFIG_TEMPLATE="${LSSA_DIR}/wallet/configs/debug/wallet_config.json"
fi
[ -f "$WALLET_CONFIG_TEMPLATE" ] || fail "wallet_config.json not found under $LSSA_DIR"

WALLET_PASSWORD="${WALLET_PASSWORD:-test}"
SPEL_TAG="${SPEL_TAG:-local}"

# ─── Setup ─────────────────────────────────────────────────────────────────

log "Setting up in ${WORK_DIR}..."
rm -rf "$WORK_DIR"
mkdir -p "$WORK_DIR"
cd "$WORK_DIR"

# ─── Step 1: Scaffold project ──────────────────────────────────────────────

log "Step 1: Creating SPEL project (LEZ=${LEZ_TAG}, SPEL=${SPEL_TAG})..."
SPEL_GIT_ARGS=()
if [ -n "${SPEL_GIT:-}" ]; then
    SPEL_GIT_ARGS=(--spel-git "$SPEL_GIT")
fi
"$SPEL_BIN" init --lez-tag "$LEZ_TAG" --spel-rev "$SPEL_TAG" "${SPEL_GIT_ARGS[@]}" "$PROJECT_NAME" \
    > "$WORK_DIR/init.log" 2>&1 || { cat "$WORK_DIR/init.log"; fail "spel init failed"; }
cd "$PROJECT_NAME"

# Regenerate lockfiles so the pinned revisions take effect
(cd methods/guest && cargo generate-lockfile > "$WORK_DIR/guest-lockfile.log" 2>&1) \
    || warn "Guest lockfile regeneration failed"
cargo generate-lockfile > "$WORK_DIR/root-lockfile.log" 2>&1 \
    || warn "Root lockfile regeneration failed"

# Re-pin the enum-ordinalize crates to LEZ's version (4.3.2) in the guest lockfile.
# generate-lockfile above re-resolves educe's `^4` to 4.4.1, which requires
# rustc 1.89, but the guest builds with the risc0 toolchain (rustc 1.88).
# spel init already pins these, but the regeneration above undoes it, so redo it.
# Both enum-ordinalize and enum-ordinalize-derive resolve independently.
(cd methods/guest \
    && cargo update -p enum-ordinalize --precise 4.3.2 >> "$WORK_DIR/guest-lockfile.log" 2>&1 \
    && cargo update -p enum-ordinalize-derive --precise 4.3.2 >> "$WORK_DIR/guest-lockfile.log" 2>&1) \
    || warn "enum-ordinalize pin failed (guest build may hit rustc 1.89)"

log "  ✓ Project scaffolded"

# ─── Step 2: Build guest binary ───────────────────────────────────────────

log "Step 2: Building guest binary..."
RISC0_SKIP_BUILD= make build > "$WORK_DIR/build.log" 2>&1 || { cat "$WORK_DIR/build.log"; fail "Build failed"; }
GUEST_BIN=$(find . -name "*.bin" -path "*/riscv32im*" | head -1)
[ -n "$GUEST_BIN" ] || fail "No guest binary found"
GUEST_BIN_ABS="$(realpath "$GUEST_BIN")"
log "  ✓ Built: $(basename "$GUEST_BIN") ($(stat -c%s "$GUEST_BIN") bytes)"

# ─── Step 3: Generate IDL ─────────────────────────────────────────────────

log "Step 3: Generating IDL..."
make idl > "$WORK_DIR/idl.log" 2>&1 || fail "IDL generation failed (see $WORK_DIR/idl.log)"
IDL_FILE=$(find . -name "*-idl.json" | head -1)
[ -n "$IDL_FILE" ] || fail "No IDL file found"
IDL_ABS="$(realpath "$IDL_FILE")"
log "  ✓ IDL generated"

# ─── Step 4: Start sequencer ──────────────────────────────────────────────

log "Step 4: Starting sequencer on port ${SEQUENCER_PORT}..."
# Only kill a leftover sequencer from a previous run of THIS script (matched
# by port), never a developer's live stack on another port.
pgrep -f "sequencer_service --port ${SEQUENCER_PORT}" | xargs -r kill 2>/dev/null || true
sleep 1
rm -rf "${LSSA_DIR}/rocksdb-${SEQUENCER_PORT}"

# LEZ v0.2.0 moved this under lez/; prefer it, fall back to the old path.
SEQ_CONFIGS="${LSSA_DIR}/lez/sequencer/service/configs/debug/sequencer_config.json"
if [ ! -f "$SEQ_CONFIGS" ]; then
    SEQ_CONFIGS="${LSSA_DIR}/sequencer/service/configs/debug/sequencer_config.json"
fi
if [ ! -f "$SEQ_CONFIGS" ]; then
    SEQ_CONFIGS=$(find "$LSSA_DIR" -name "sequencer_config.json" 2>/dev/null | head -1)
fi
[ -n "$SEQ_CONFIGS" ] && [ -f "$SEQ_CONFIGS" ] || fail "Sequencer config not found"

# LEZ v0.2.0 writes a bedrock_signing_key (and rocksdb) under config.home,
# which defaults to "." — i.e. the sequencer's cwd. The LEZ checkout is
# read-only in CI, so launching from there fails with "Permission denied".
# Copy the config into the writable work dir with home rewritten to it.
SEQ_HOME="$WORK_DIR/seq-home"
mkdir -p "$SEQ_HOME"
SEQ_CONFIG_PATCHED="$WORK_DIR/sequencer_config.json"
# Paths are passed as argv (not interpolated into the source) so a path
# containing quotes or other special characters can't break the script.
python3 -c '
import json, sys
src, home, dst = sys.argv[1], sys.argv[2], sys.argv[3]
cfg = json.load(open(src))
cfg["home"] = home
json.dump(cfg, open(dst, "w"))
' "$SEQ_CONFIGS" "$SEQ_HOME" "$SEQ_CONFIG_PATCHED" || fail "Failed to patch sequencer config home"
SEQ_CONFIGS="$SEQ_CONFIG_PATCHED"

cd "$SEQ_HOME"
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

# ─── Step 5: Create two isolated wallets ──────────────────────────────────
# Each wallet home is a fresh directory holding only a wallet_config.json
# pointed at our sequencer; `wallet account new` bootstraps its storage.json.
# Wallet A is the exporter, wallet B the co-signer. Neither holds the
# other's keys, which is the whole point of the exercise.

log "Step 5: Creating two wallets..."
WALLET_A="$WORK_DIR/wallet-a"
WALLET_B="$WORK_DIR/wallet-b"

for home in "$WALLET_A" "$WALLET_B"; do
    mkdir -p "$home"
    # The sequencer address schema changed in LEZ v0.2.1 (flat sequencer_addr
    # became a sequencers array). Patch whichever schema the template uses —
    # writing the wrong one is silently ignored by serde and the wallet then
    # talks to the default port (spel/co #256).
    python3 -c '
import json, sys
src, dst, url = sys.argv[1], sys.argv[2], sys.argv[3]
with open(src) as f:
    config = json.load(f)
if isinstance(config.get("sequencers"), list):
    entry = config["sequencers"][0] if config["sequencers"] else {}
    entry["sequencer_addr"] = url
    config["sequencers"] = [entry]
    config.pop("sequencer_addr", None)
else:
    config["sequencer_addr"] = url
with open(dst, "w") as f:
    json.dump(config, f, indent=4)
' "$WALLET_CONFIG_TEMPLATE" "$home/wallet_config.json" "$SEQUENCER_URL" \
        || fail "Failed to write wallet config for $home"
done

SIGNER_A=$(printf '%s\n' "$WALLET_PASSWORD" \
    | NSSA_WALLET_HOME_DIR="$WALLET_A" LEE_WALLET_HOME_DIR="$WALLET_A" \
      "$WALLET_BIN" account new public 2>&1 \
    | sed -n 's/.*Public\/\([A-Za-z0-9]*\).*/\1/p' | tail -1)
[ -n "$SIGNER_A" ] || fail "Could not create account in wallet A"
log "  Wallet A signer: ${SIGNER_A:0:20}..."

SIGNER_B=$(printf '%s\n' "$WALLET_PASSWORD" \
    | NSSA_WALLET_HOME_DIR="$WALLET_B" LEE_WALLET_HOME_DIR="$WALLET_B" \
      "$WALLET_BIN" account new public 2>&1 \
    | sed -n 's/.*Public\/\([A-Za-z0-9]*\).*/\1/p' | tail -1)
[ -n "$SIGNER_B" ] || fail "Could not create account in wallet B"
log "  Wallet B signer: ${SIGNER_B:0:20}..."

[ "$SIGNER_A" != "$SIGNER_B" ] || fail "Wallet A and B produced the same account id"

# ─── Step 6: Deploy program (wallet A) ────────────────────────────────────

log "Step 6: Deploying program..."
printf '%s\n' "$WALLET_PASSWORD" \
    | NSSA_WALLET_HOME_DIR="$WALLET_A" LEE_WALLET_HOME_DIR="$WALLET_A" \
      "$WALLET_BIN" deploy-program "$GUEST_BIN_ABS" \
    > "$WORK_DIR/deploy.log" 2>&1 || { cat "$WORK_DIR/deploy.log"; fail "Deploy failed"; }
log "  ✓ Program deployed"

# ─── Step 7: Initialize (single-signer, wallet A) ─────────────────────────

log "Step 7: Sending initialize transaction..."
SEQUENCER_URL="$SEQUENCER_URL" \
NSSA_WALLET_HOME_DIR="$WALLET_A" LEE_WALLET_HOME_DIR="$WALLET_A" \
    "$SPEL_BIN" --idl "$IDL_ABS" -p "$GUEST_BIN_ABS" \
    initialize \
    --owner "$SIGNER_A" \
    > "$WORK_DIR/initialize-tx.log" 2>&1 || { cat "$WORK_DIR/initialize-tx.log"; fail "Initialize TX failed"; }
log "  ✓ Initialize TX submitted and confirmed"

# ─── Step 8: Export partial transaction (wallet A + co-signer B) ──────────

log "Step 8: Exporting partial transaction (--export / --co-signer)..."
BLOB="$WORK_DIR/multisig-tx.json"
SEQUENCER_URL="$SEQUENCER_URL" \
NSSA_WALLET_HOME_DIR="$WALLET_A" LEE_WALLET_HOME_DIR="$WALLET_A" \
    "$SPEL_BIN" --idl "$IDL_ABS" -p "$GUEST_BIN_ABS" \
    --export "$BLOB" --co-signer "$SIGNER_B" \
    do_something \
    --owner "$SIGNER_A" \
    --amount 42 \
    > "$WORK_DIR/export.log" 2>&1 || { cat "$WORK_DIR/export.log"; fail "Export failed"; }

grep -q "Partial transaction written" "$WORK_DIR/export.log" \
    || { cat "$WORK_DIR/export.log"; fail "Export did not report writing the blob"; }
[ -f "$BLOB" ] || fail "Blob file not written: $BLOB"

# Exporter wallet holds only signer A's key, so the blob must carry exactly
# one witness and still list two required signers.
python3 -c '
import json, sys
blob = json.load(open(sys.argv[1]))
version = blob["version"]
signers = blob["signers"]
witnesses = blob["witnesses"]
assert version == 1, f"unexpected version {version}"
assert len(signers) == 2, f"expected 2 signers, got {len(signers)}"
assert len(witnesses) == 1, f"expected 1 witness, got {len(witnesses)}"
missing = [s for s in signers if s not in witnesses]
assert len(missing) == 1, f"expected 1 missing signer, got {missing}"
print("  ✓ Blob: 2 signers, 1 witness, co-signer still missing")
' "$BLOB" || fail "Blob content check failed"
log "  ✓ Partial transaction exported"

# ─── Step 9: Premature submit must be rejected ────────────────────────────

log "Step 9: Submitting with a witness still missing (must fail)..."
if NSSA_WALLET_HOME_DIR="$WALLET_B" LEE_WALLET_HOME_DIR="$WALLET_B" \
    "$SPEL_BIN" submit "$BLOB" > "$WORK_DIR/premature-submit.log" 2>&1; then
    cat "$WORK_DIR/premature-submit.log"
    fail "submit succeeded although a witness is missing"
fi
grep -q "missing signers" "$WORK_DIR/premature-submit.log" \
    || { cat "$WORK_DIR/premature-submit.log"; fail "submit failed for the wrong reason"; }
log "  ✓ Premature submit rejected (missing signers)"

# ─── Step 10: Co-sign from wallet B ───────────────────────────────────────

log "Step 10: Signing from wallet B (spel sign)..."
printf 'y\n' | \
NSSA_WALLET_HOME_DIR="$WALLET_B" LEE_WALLET_HOME_DIR="$WALLET_B" \
    "$SPEL_BIN" sign "$BLOB" \
    > "$WORK_DIR/sign.log" 2>&1 || { cat "$WORK_DIR/sign.log"; fail "spel sign failed"; }

grep -q "Witnesses added" "$WORK_DIR/sign.log" \
    || { cat "$WORK_DIR/sign.log"; fail "sign did not report adding a witness"; }
grep -q "All witnesses collected" "$WORK_DIR/sign.log" \
    || { cat "$WORK_DIR/sign.log"; fail "sign did not report the blob complete"; }

python3 -c '
import json, sys
blob = json.load(open(sys.argv[1]))
witnesses = blob["witnesses"]
assert len(witnesses) == 2, f"expected 2 witnesses, got {len(witnesses)}"
print("  ✓ Blob now fully signed (2 of 2 witnesses)")
' "$BLOB" || fail "Blob witness count check failed"
log "  ✓ Co-signer witness appended"

# ─── Step 11: Tampered blob must be rejected ──────────────────────────────
# Flip one bit in the last byte of message_hex — the tail of the borsh
# message is instruction data, so the blob stays decodable but its hash no
# longer matches what either signer signed.

log "Step 11: Submitting a tampered blob (must fail)..."
TAMPERED="$WORK_DIR/multisig-tx-tampered.json"
python3 -c '
import json, sys
src, dst = sys.argv[1], sys.argv[2]
blob = json.load(open(src))
raw = bytearray.fromhex(blob["message_hex"])
raw[-1] ^= 0xFF
blob["message_hex"] = raw.hex()
json.dump(blob, open(dst, "w"), indent=2)
' "$BLOB" "$TAMPERED" || fail "Failed to write tampered blob"

if NSSA_WALLET_HOME_DIR="$WALLET_B" LEE_WALLET_HOME_DIR="$WALLET_B" \
    "$SPEL_BIN" submit "$TAMPERED" > "$WORK_DIR/tampered-submit.log" 2>&1; then
    cat "$WORK_DIR/tampered-submit.log"
    fail "submit accepted a tampered blob"
fi
grep -q "does not verify against the message" "$WORK_DIR/tampered-submit.log" \
    || { cat "$WORK_DIR/tampered-submit.log"; fail "tampered submit failed for the wrong reason"; }
log "  ✓ Tampered blob rejected"

# ─── Step 12: Submit the real blob (wallet B) ─────────────────────────────

log "Step 12: Submitting the fully signed blob..."
NSSA_WALLET_HOME_DIR="$WALLET_B" LEE_WALLET_HOME_DIR="$WALLET_B" \
    "$SPEL_BIN" submit "$BLOB" \
    > "$WORK_DIR/submit.log" 2>&1 || { cat "$WORK_DIR/submit.log"; fail "spel submit failed"; }

grep -q "Transaction confirmed" "$WORK_DIR/submit.log" \
    || { cat "$WORK_DIR/submit.log"; fail "submit did not confirm the transaction"; }
log "  ✓ Multisig transaction submitted and confirmed"

# ─── Done ─────────────────────────────────────────────────────────────────

log ""
log "🎉 Multisig E2E test PASSED!"
log "  All steps completed successfully:"
log "    ✅ spel init + build + IDL — project ready"
log "    ✅ two isolated wallets created (A = exporter, B = co-signer)"
log "    ✅ deploy + initialize — program live"
log "    ✅ --export / --co-signer — partial TX written, 1 of 2 witnesses"
log "    ✅ premature submit rejected — missing signers"
log "    ✅ spel sign — co-signer witness appended, blob complete"
log "    ✅ tampered submit rejected — signature no longer verifies"
log "    ✅ spel submit — multisig TX confirmed in a block"
