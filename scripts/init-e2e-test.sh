#!/usr/bin/env bash
# `spel init` with its default dependencies: the scaffold must build as `spel init <PROJECT>`
# writes it. The smoke test (smoke-test.sh) points spel-attrs at this checkout and goes on to
# deploy and send; this one checks the defaults resolve: LEZ at the tag spel links, spel-attrs
# from git at the commit spel was built from (so that commit must be pushed).
#
# Usage: scripts/init-e2e-test.sh [WORK_DIR]
#
# Needs Docker and cargo-risczero (the guest builds in Docker) and python3. No sequencer.
#
# Environment:
#   SPEL_BIN        default: built from this checkout (release)
#   SPEL_INIT_ARGS  extra `spel init` flags, e.g. `--spel-git <fork URL>`; default: none
#   WORK_DIR        default: ${TMPDIR:-/tmp}/spel-init-e2e

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SPEL_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WORK_DIR="${1:-${WORK_DIR:-${TMPDIR:-/tmp}/spel-init-e2e}}"
WORK_DIR="${WORK_DIR%/}"
LOG_DIR="$WORK_DIR/logs"
PROJECT=init-e2e
PROGRAM=init_e2e

RED='\033[0;31m'
GREEN='\033[0;32m'
NC='\033[0m'
log()  { echo -e "${GREEN}[INIT]${NC} $*"; }
fail() { echo -e "${RED}[FAIL]${NC} $*" >&2; exit 1; }

# Run a step, logging to $LOG_DIR/<name>.log; print the log's tail on failure.
step() {
    local name="$1"; shift
    "$@" > "$LOG_DIR/$name.log" 2>&1 || { tail -40 "$LOG_DIR/$name.log" >&2; fail "$name failed (see $LOG_DIR/$name.log)"; }
}

for tool in cargo cargo-risczero docker python3; do
    command -v "$tool" >/dev/null || fail "$tool not found"
done

rm -rf "$WORK_DIR"
mkdir -p "$WORK_DIR" "$LOG_DIR"

if [ -z "${SPEL_BIN:-}" ]; then
    log "Building spel..."
    step spel-build cargo build --manifest-path "$SPEL_DIR/Cargo.toml" -p spel --release
    SPEL_BIN="$SPEL_DIR/target/release/spel"
fi
SPEL_BIN="$(cd "$(dirname "$SPEL_BIN")" && pwd)/$(basename "$SPEL_BIN")"

log "spel init $PROJECT ${SPEL_INIT_ARGS:-(defaults)}"
cd "$WORK_DIR"
# shellcheck disable=SC2086  # SPEL_INIT_ARGS is a list of flags
step init "$SPEL_BIN" init "$PROJECT" ${SPEL_INIT_ARGS:-}
cd "$PROJECT"

# What the defaults resolved to, for diagnosing a failure below.
grep -h -E '^(spel-attrs|lee_core|lee|risc0-zkvm) ' "${PROGRAM}_core/Cargo.toml" \
    "${PROGRAM}_program/Cargo.toml" guest/Cargo.toml | sed 's/^/  /'
! grep -q 'path = ' "${PROGRAM}_core/Cargo.toml" || fail "the default spel-attrs is a path, not git"

log "Host tests..."
step host-tests cargo test

# The guest never builds spel-attrs; this builds it from the default git revision.
log "Markers (--features idl)..."
step markers cargo check -p "${PROGRAM}_core" --features idl

log "Building the guest (Docker)..."
step guest-build make build
[ -f "guest/target/riscv32im-risc0-zkvm-elf/docker/$PROGRAM.bin" ] || fail "no guest binary"
step program-id "$SPEL_BIN" program-id "guest/target/riscv32im-risc0-zkvm-elf/docker/$PROGRAM.bin"

log "Generating the IDL..."
step idl make idl SPEL="$SPEL_BIN"
python3 - "$PROGRAM-idl.json" "$PROGRAM" <<'PY' || fail "unexpected IDL (see $PWD/$PROGRAM-idl.json)"
import json, sys
idl = json.load(open(sys.argv[1]))
assert idl["name"] == sys.argv[2], idl["name"]
assert [m["name"] for m in idl["messages"]] == ["Increment", "Reset"], idl["messages"]
assert idl["state_types"] == ["CounterState"], idl["state_types"]
PY

log "🎉 Init e2e passed. Logs: $LOG_DIR"
