# shellcheck shell=bash
# What the sequencer e2e scripts share: prerequisites, a scaffolded and deployed counter, a
# standalone sequencer and a wallet. Source it, then:
#
#     e2e_init <tag> <default work dir> "$@"   # checks, recreates WORK_DIR, builds spel
#     e2e_scaffold <project>                    # spel init, host tests, guest, IDL
#     e2e_chain                                 # sequencer, wallet, funded FEE_PAYER
#     e2e_deploy                                # make deploy; the address lands in spel.toml
#
# Environment (see smoke-test.sh): LEZ_DIR (required), SEQUENCER_BIN, WALLET_BIN, SPEL_BIN,
# SPEL_INIT_ARGS, WORK_DIR. Everything stays in WORK_DIR; the sequencer listens on
# 127.0.0.1:3040, where LEZ's debug wallet config points.

E2E_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SPEL_DIR="$(cd "$E2E_LIB_DIR/../.." && pwd)"

# Proofs are faked and not verified: the sequencer and spel's prover both read this.
export RISC0_DEV_MODE=1

E2E_TAG=E2E
log()  { echo -e "\033[0;32m[$E2E_TAG]\033[0m $*"; }
fail() { echo -e "\033[0;31m[FAIL]\033[0m $*" >&2; exit 1; }

# Run a step, logging to $LOG_DIR/<name>.log; print the log's tail on failure.
step() {
    local name="$1"; shift
    "$@" > "$LOG_DIR/$name.log" 2>&1 || { tail -40 "$LOG_DIR/$name.log" >&2; fail "$name failed (see $LOG_DIR/$name.log)"; }
}

# Expect the command after $1 (a log name) and $2 (an exit code) to exit with that code.
expect_exit() {
    local name="$1" want="$2"; shift 2
    local got=0
    "$@" > "$LOG_DIR/$name.log" 2>&1 || got=$?
    [ "$got" = "$want" ] || { cat "$LOG_DIR/$name.log" >&2; fail "$name: exit $got, expected $want"; }
}

# Expect `spel send` with the given arguments to exit with $1, logged as $2.
send() {
    local want="$1" name="$2"; shift 2
    expect_exit "$name" "$want" "$SPEL_BIN" "$@"
}

# Expect ACTOR's count to be $2.
count_is() {
    local actor="$1" want="$2" state
    state="$("$SPEL_BIN" inspect "$actor" 2> "$LOG_DIR/inspect.log")" \
        || { cat "$LOG_DIR/inspect.log" >&2; fail "inspect $actor failed"; }
    python3 -c 'import json, sys; sys.exit(int(json.loads(sys.argv[1])["count"]) != int(sys.argv[2]))' \
        "$state" "$want" || fail "$actor: expected count $want, got $state"
    log "  $actor: count $want"
}

e2e_cleanup() {
    if [ -n "${SEQ_PID:-}" ] && kill -0 "$SEQ_PID" 2>/dev/null; then
        kill "$SEQ_PID" 2>/dev/null || true
        wait "$SEQ_PID" 2>/dev/null || true
    fi
}

e2e_init() {
    E2E_TAG="$1"
    WORK_DIR="${3:-${WORK_DIR:-$2}}"
    WORK_DIR="${WORK_DIR%/}"
    LOG_DIR="$WORK_DIR/logs"
    trap e2e_cleanup EXIT

    [ -n "${LEZ_DIR:-}" ] || fail "set LEZ_DIR to a logos-execution-zone checkout (see the header)"
    LEZ_DIR="$(cd "$LEZ_DIR" && pwd)"
    SEQUENCER_BIN="${SEQUENCER_BIN:-$LEZ_DIR/target/release/sequencer_service}"
    [ -x "$SEQUENCER_BIN" ] || fail "no sequencer at $SEQUENCER_BIN (cargo build --release --features standalone -p sequencer_service)"
    if [ -z "${WALLET_BIN:-}" ]; then
        if [ -x "$LEZ_DIR/target/release/wallet" ]; then WALLET_BIN="$LEZ_DIR/target/release/wallet"
        else WALLET_BIN="$(command -v wallet || true)"; fi
    fi
    [ -n "$WALLET_BIN" ] && [ -x "$WALLET_BIN" ] || fail "no wallet (cargo build --release -p wallet in LEZ_DIR)"
    local tool
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
}

# Scaffold PROJECT in WORK_DIR, test, build and generate its IDL; cd into it. Sets PROGRAM.
e2e_scaffold() {
    local project="$1"
    PROGRAM="${project//-/_}"
    log "Scaffolding $project..."
    cd "$WORK_DIR"
    # shellcheck disable=SC2086  # SPEL_INIT_ARGS is a list of flags
    step init "$SPEL_BIN" init "$project" ${SPEL_INIT_ARGS:---spel-path "$SPEL_DIR"}
    cd "$project"

    log "Host tests..."
    step host-tests cargo test

    log "Building the guest (Docker)..."
    step guest-build make build
    [ -f "guest/target/riscv32im-risc0-zkvm-elf/docker/$PROGRAM.bin" ] || fail "no guest binary"

    log "Generating the IDL..."
    step idl make idl SPEL="$SPEL_BIN"
    python3 - "$PROGRAM-idl.json" <<'PY' || fail "unexpected IDL (see $PWD/$PROGRAM-idl.json)"
import json, sys
idl = json.load(open(sys.argv[1]))
assert [m["name"] for m in idl["messages"]] == ["Increment", "Reset"], idl["messages"]
assert idl["state_types"] == ["CounterState"], idl["state_types"]
PY
}

# Start the sequencer and set up a wallet at $WORK_DIR/wallet with the funded FEE_PAYER.
e2e_chain() {
    # Another sequencer on these ports would answer for ours, and the test would run on its chain.
    local port
    for port in 3040 9000; do
        if (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null; then
            fail "127.0.0.1:$port is in use (a sequencer already running?). These tests start \
their own on it; stop the other one first."
        fi
    done
    log "Starting the sequencer..."
    local home="$WORK_DIR/sequencer"
    # The funding key pays Bedrock inscriptions; standalone mode mocks Bedrock, so any key does.
    step sequencer-setup "$SEQUENCER_BIN" setup "$home" --config "$SEQ_TEMPLATE" \
        --funding-public-key 2e03b2eff5a45478e7e79668d2a146cf2c5c7925bce927f2b1c67f2ab4fc0d26 \
        --create-channel
    (cd "$home" && RUST_LOG=info,risc0_zkvm=warn exec "$SEQUENCER_BIN" start "$home") \
        > "$LOG_DIR/sequencer.log" 2>&1 &
    SEQ_PID=$!

    export LEE_WALLET_PASSWORD=smoke
    e2e_wallet "$WORK_DIR/wallet"

    local _
    for _ in $(seq 1 60); do
        kill -0 "$SEQ_PID" 2>/dev/null || { tail -30 "$LOG_DIR/sequencer.log" >&2; fail "the sequencer exited"; }
        "$WALLET_BIN" check-health > "$LOG_DIR/health.log" 2>&1 && break
        sleep 2
    done
    "$WALLET_BIN" check-health > "$LOG_DIR/health.log" 2>&1 || { cat "$LOG_DIR/health.log" >&2; fail "the sequencer isn't healthy"; }
    kill -0 "$SEQ_PID" 2>/dev/null || { tail -30 "$LOG_DIR/sequencer.log" >&2; fail "the sequencer exited"; }

    # shellcheck source=fee-payer.sh
    source "$E2E_LIB_DIR/fee-payer.sh"
    FEE_PAYER="$(lez_fee_payer "$WALLET_BIN")"
    [ -n "$FEE_PAYER" ] || fail "couldn't import the funded debug account"
    log "Fee payer: $FEE_PAYER"
}

# Point LEE_WALLET_HOME_DIR at a wallet in DIR (created from LEZ's debug config if new).
e2e_wallet() {
    export LEE_WALLET_HOME_DIR="$1"
    if [ ! -f "$1/wallet_config.json" ]; then
        mkdir -p "$1"
        cp "$WALLET_TEMPLATE" "$1/"
    fi
}

e2e_deploy() {
    log "Deploying..."
    step deploy make deploy SPEL="$SPEL_BIN" FEE_PAYER="$FEE_PAYER"
    grep -q '^address = "' spel.toml || fail "make deploy didn't record the address in spel.toml"
    log "  $(grep '^address' spel.toml)"
}
