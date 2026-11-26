#!/usr/bin/env bash
# End-to-end smoke test: `spel init` → guest build → IDL → deploy → public and private sends,
# against a local standalone sequencer.
#
# Usage: LEZ_DIR=<logos-execution-zone checkout> scripts/smoke-test.sh [WORK_DIR]
#
# Needs Docker and cargo-risczero (the guest builds in Docker), python3 and curl, and in LEZ_DIR,
# checked out at the tag spel links (v0.4.0-rc3), a standalone sequencer and the wallet:
#
#     cargo build --release --features standalone -p sequencer_service
#     cargo build --release -p wallet
#
# Environment:
#   LEZ_DIR         the LEZ checkout (required)
#   SEQUENCER_BIN   default: $LEZ_DIR/target/release/sequencer_service
#   WALLET_BIN      default: $LEZ_DIR/target/release/wallet, else `wallet` on PATH
#   SPEL_BIN        default: built from this checkout (release)
#   SPEL_INIT_ARGS  `spel init` flags for spel-attrs; default: --spel-path <this checkout>
#   WORK_DIR        default: ${TMPDIR:-/tmp}/spel-smoke
#
# Everything it writes (project, sequencer home, wallet home, logs) stays in WORK_DIR, which
# it recreates. The sequencer listens on 127.0.0.1:3040, where LEZ's debug wallet config points.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SPEL_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WORK_DIR="${1:-${WORK_DIR:-${TMPDIR:-/tmp}/spel-smoke}}"
WORK_DIR="${WORK_DIR%/}"
LOG_DIR="$WORK_DIR/logs"
PROJECT=smoke-counter
PROGRAM=smoke_counter

# Proofs are faked and not verified: the sequencer and spel's prover both read this.
export RISC0_DEV_MODE=1

RED='\033[0;31m'
GREEN='\033[0;32m'
NC='\033[0m'
log()  { echo -e "${GREEN}[SMOKE]${NC} $*"; }
fail() { echo -e "${RED}[FAIL]${NC} $*" >&2; exit 1; }

# Run a step, logging to $LOG_DIR/<name>.log; print the log's tail on failure.
step() {
    local name="$1"; shift
    "$@" > "$LOG_DIR/$name.log" 2>&1 || { tail -40 "$LOG_DIR/$name.log" >&2; fail "$name failed (see $LOG_DIR/$name.log)"; }
}

cleanup() {
    if [ -n "${SEQ_PID:-}" ] && kill -0 "$SEQ_PID" 2>/dev/null; then
        kill "$SEQ_PID" 2>/dev/null || true
        wait "$SEQ_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

# ─── Prerequisites ────────────────────────────────────────────────────────

[ -n "${LEZ_DIR:-}" ] || fail "set LEZ_DIR to a logos-execution-zone checkout (see the header)"
LEZ_DIR="$(cd "$LEZ_DIR" && pwd)"
SEQUENCER_BIN="${SEQUENCER_BIN:-$LEZ_DIR/target/release/sequencer_service}"
[ -x "$SEQUENCER_BIN" ] || fail "no sequencer at $SEQUENCER_BIN (cargo build --release --features standalone -p sequencer_service)"
if [ -z "${WALLET_BIN:-}" ]; then
    if [ -x "$LEZ_DIR/target/release/wallet" ]; then WALLET_BIN="$LEZ_DIR/target/release/wallet"
    else WALLET_BIN="$(command -v wallet || true)"; fi
fi
[ -n "$WALLET_BIN" ] && [ -x "$WALLET_BIN" ] || fail "no wallet (cargo build --release -p wallet in LEZ_DIR)"
for tool in cargo cargo-risczero docker python3 curl; do
    command -v "$tool" >/dev/null || fail "$tool not found"
done
SEQ_TEMPLATE="$LEZ_DIR/lez/sequencer/service/networks/local.json"
WALLET_TEMPLATE="$LEZ_DIR/lez/wallet/configs/debug/wallet_config.json"
[ -f "$SEQ_TEMPLATE" ] || fail "no sequencer config at $SEQ_TEMPLATE"
[ -f "$WALLET_TEMPLATE" ] || fail "no wallet config at $WALLET_TEMPLATE"
log "LEZ $(git -C "$LEZ_DIR" describe --tags --always 2>/dev/null || echo '?') at $LEZ_DIR"

rm -rf "$WORK_DIR"
mkdir -p "$WORK_DIR" "$LOG_DIR"

if [ -z "${SPEL_BIN:-}" ]; then
    log "Building spel..."
    step spel-build cargo build --manifest-path "$SPEL_DIR/Cargo.toml" -p spel --release
    SPEL_BIN="$SPEL_DIR/target/release/spel"
fi
SPEL_BIN="$(cd "$(dirname "$SPEL_BIN")" && pwd)/$(basename "$SPEL_BIN")"

# ─── Scaffold, test, build, IDL ───────────────────────────────────────────

log "Scaffolding $PROJECT..."
cd "$WORK_DIR"
# shellcheck disable=SC2086  # SPEL_INIT_ARGS is a list of flags
step init "$SPEL_BIN" init "$PROJECT" ${SPEL_INIT_ARGS:---spel-path "$SPEL_DIR"}
cd "$PROJECT"

log "Host tests..."
step host-tests cargo test

log "Building the guest (Docker)..."
step guest-build make build
BINARY="guest/target/riscv32im-risc0-zkvm-elf/docker/$PROGRAM.bin"
[ -f "$BINARY" ] || fail "no guest binary at $BINARY"

log "Generating the IDL..."
step idl make idl SPEL="$SPEL_BIN"
python3 - "$PROGRAM-idl.json" <<'PY' || fail "unexpected IDL (see $PWD/$PROGRAM-idl.json)"
import json, sys
idl = json.load(open(sys.argv[1]))
assert [m["name"] for m in idl["messages"]] == ["Increment", "Reset"], idl["messages"]
assert idl["state_types"] == ["CounterState"], idl["state_types"]
PY

# ─── Sequencer and wallet ─────────────────────────────────────────────────

log "Starting the sequencer..."
SEQ_HOME="$WORK_DIR/sequencer"
# The funding key pays Bedrock inscriptions; standalone mode mocks Bedrock, so any key does.
step sequencer-setup "$SEQUENCER_BIN" setup "$SEQ_HOME" --config "$SEQ_TEMPLATE" \
    --funding-public-key 2e03b2eff5a45478e7e79668d2a146cf2c5c7925bce927f2b1c67f2ab4fc0d26 \
    --create-channel
(cd "$SEQ_HOME" && RUST_LOG=info,risc0_zkvm=warn exec "$SEQUENCER_BIN" start "$SEQ_HOME") \
    > "$LOG_DIR/sequencer.log" 2>&1 &
SEQ_PID=$!

export LEE_WALLET_HOME_DIR="$WORK_DIR/wallet"
export LEE_WALLET_PASSWORD=smoke
mkdir -p "$LEE_WALLET_HOME_DIR"
cp "$WALLET_TEMPLATE" "$LEE_WALLET_HOME_DIR/"

for _ in $(seq 1 60); do
    kill -0 "$SEQ_PID" 2>/dev/null || { tail -30 "$LOG_DIR/sequencer.log" >&2; fail "the sequencer exited"; }
    "$WALLET_BIN" check-health > "$LOG_DIR/health.log" 2>&1 && break
    sleep 2
done
"$WALLET_BIN" check-health > "$LOG_DIR/health.log" 2>&1 || { cat "$LOG_DIR/health.log" >&2; fail "the sequencer isn't healthy"; }

# shellcheck source=lib/fee-payer.sh
source "$SCRIPT_DIR/lib/fee-payer.sh"
FEE_PAYER="$(lez_fee_payer "$WALLET_BIN")"
[ -n "$FEE_PAYER" ] || fail "couldn't import the funded debug account"
log "Fee payer: $FEE_PAYER"

# ─── Deploy ───────────────────────────────────────────────────────────────

log "Deploying..."
step deploy make deploy SPEL="$SPEL_BIN" FEE_PAYER="$FEE_PAYER"
grep -q '^address = "' spel.toml || fail "make deploy didn't record the address in spel.toml"
log "  $(grep '^address' spel.toml)"

# Expect `spel send` to exit with $1; the rest are its arguments.
send() {
    local want="$1" name="$2"; shift 2
    local got=0
    "$SPEL_BIN" "$@" > "$LOG_DIR/$name.log" 2>&1 || got=$?
    [ "$got" = "$want" ] || { cat "$LOG_DIR/$name.log" >&2; fail "$name: exit $got, expected $want"; }
}

# Expect ACTOR's count to be $2.
count_is() {
    local actor="$1" want="$2" state
    state="$("$SPEL_BIN" inspect "$actor" 2> "$LOG_DIR/inspect.log")" \
        || { cat "$LOG_DIR/inspect.log" >&2; fail "inspect $actor failed"; }
    python3 -c 'import json, sys; sys.exit(int(json.loads(sys.argv[1])["count"]) != int(sys.argv[2]))' \
        "$state" "$want" || fail "$actor: expected count $want, got $state"
    log "  $actor: $state"
}

# ─── Public ───────────────────────────────────────────────────────────────

log "Public sends..."
step new-alice "$WALLET_BIN" account new public --label alice
send 0 public-increment send --root alice --signer alice --fee-payer "$FEE_PAYER" -- increment --by 2
send 0 public-increment-2 send --root alice --signer alice --fee-payer "$FEE_PAYER" -- increment --by 3
count_is "alice@$PROGRAM" 5
# Without --signer the counter refuses (it asserts authorization): refused in simulation, exit 2.
send 2 public-unsigned send --root alice --fee-payer "$FEE_PAYER" -- increment --by 1
count_is "alice@$PROGRAM" 5

# ─── Private ──────────────────────────────────────────────────────────────

log "Private sends..."
step new-carol "$WALLET_BIN" account new private --label carol
# A private call pays its fee from a private account, so fund carol first.
step fund-carol "$SPEL_BIN" transfer --from "$FEE_PAYER" --to carol --amount 1000000000
send 0 private-increment send --root carol --signer carol -- increment --by 7
count_is "carol@$PROGRAM" 7
send 2 private-unsigned send --root carol -- increment --by 1
count_is "carol@$PROGRAM" 7

log "🎉 Smoke test passed. Logs: $LOG_DIR"
