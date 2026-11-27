#!/usr/bin/env bash
# Private-call e2e: a payroll program whose payments reach actors no field names, against a local
# standalone sequencer, with two wallets.
#
# Wallet A holds the seat, a private account, and its treasury, a private PDA of the program keyed
# by the seat (seed: the seat's id). Wallet B holds the employee. A pays B from the treasury: the
# payment is a custody transfer out of the treasury's native balance, which checks
#   - a private PDA, registered with `spel pda --register` and funded with `spel transfer`;
#   - inference: the treasury's balance is reached only in simulation, and `send` adds it;
#   - a sealed credit (`--seal keys:FILE`) to a private account of another wallet, refused
#     without it, with the `--seal` named;
#   - loading the program from the chain, for a call made outside the project (no local binary);
# Then a public seat (boss) pays from a public treasury, a credit from public execution, so it
# goes with a recovery binding (`--recover`):
#   - while the employee has no binding, the call is private, publishes it, and a private
#     `--fee-payer` pays;
#   - once bound, the same call is public: a private `--fee-payer` is refused before sending, a
#     public one pays (boss has no native balance, so nothing else could).
# B collects every credit with `wallet pending receive`.
#
# Usage: LEZ_DIR=<logos-execution-zone checkout> scripts/payroll-e2e-test.sh [WORK_DIR]
#
# Needs what smoke-test.sh needs; takes the same environment. WORK_DIR defaults to
# ${TMPDIR:-/tmp}/spel-payroll.

set -euo pipefail

# shellcheck source=lib/e2e.sh
source "$(dirname "$0")/lib/e2e.sh"
e2e_init PAYROLL "${TMPDIR:-/tmp}/spel-payroll" "$@"
e2e_scaffold payroll payroll
python3 - "$PROGRAM-idl.json" <<'PY' || fail "unexpected IDL (see $PWD/$PROGRAM-idl.json)"
import json, sys
idl = json.load(open(sys.argv[1]))
assert [m["name"] for m in idl["messages"]] == ["Open", "Pay"], idl["messages"]
assert idl["state_types"] == ["Seat"], idl["state_types"]
PY
e2e_chain
e2e_deploy

PROJECT_DIR="$PWD"
ADDRESS="$(sed -n 's/^address = "\(.*\)"/\1/p' spel.toml)"
WALLET_A="$WORK_DIR/wallet"
WALLET_B="$WORK_DIR/wallet-b"
KEYS="$WORK_DIR/employee.keys"

# Expect seat $1's total paid to be $2. Extra arguments go before `inspect` (global options).
paid_is() {
    local seat="$1" want="$2" state; shift 2
    state="$("$SPEL_BIN" "$@" inspect "$seat@$ADDRESS" 2> "$LOG_DIR/inspect.log")" \
        || { cat "$LOG_DIR/inspect.log" >&2; fail "inspect $seat failed"; }
    python3 -c 'import json, sys; sys.exit(int(json.loads(sys.argv[1])["paid"]) != int(sys.argv[2]))' \
        "$state" "$want" || fail "$seat: expected paid $want, got $state"
    log "  $seat: paid $want"
}

# ─── Wallet B: the employee ───────────────────────────────────────────────

e2e_wallet "$WALLET_B"
step new-employee "$WALLET_BIN" account new private --label employee
"$WALLET_BIN" account show-keys --account-id employee > "$KEYS" 2> "$LOG_DIR/employee-keys.log" \
    || { cat "$LOG_DIR/employee-keys.log" >&2; fail "show-keys failed"; }
[ "$(wc -l < "$KEYS")" -eq 2 ] || fail "unexpected keys file $KEYS"
log "employee (wallet B): keys in $KEYS"

# ─── Wallet A: the seat and its treasury ──────────────────────────────────

e2e_wallet "$WALLET_A"
log "Seat and treasury..."
step new-seat "$WALLET_BIN" account new private --label seat
# The seat pays its calls' fees from its private balance.
step fund-seat "$SPEL_BIN" transfer --from "$FEE_PAYER" --to seat --amount 1000000000
step treasury "$SPEL_BIN" pda account:seat --owner seat --register --label treasury
log "  treasury: $(tail -1 "$LOG_DIR/treasury.log")"
step fund-treasury "$SPEL_BIN" transfer --from "$FEE_PAYER" --to treasury --amount 1000

send 0 open send --root seat --signer seat -- open --treasury treasury
paid_is seat 0

log "Paying..."
# The employee's credit comes from private execution: without its seal the wallet refuses the
# call, and send names the --seal to add (a missing argument: exit 1).
send 1 pay-unsealed send --root seat --signer seat -- pay --employee "keys:$KEYS" --amount 300
grep -q -- "--seal keys:$KEYS" "$LOG_DIR/pay-unsealed.log" \
    || fail "pay-unsealed: no --seal hint (see $LOG_DIR/pay-unsealed.log)"
paid_is seat 0

send 0 pay send --root seat --signer seat --seal "keys:$KEYS" -- pay --employee "keys:$KEYS" --amount 300
grep -q "reached in simulation, private PDA" "$LOG_DIR/pay.log" \
    || fail "pay: the treasury wasn't inferred as a private PDA (see $LOG_DIR/pay.log)"
paid_is seat 300

# Outside the project there is no spel.toml and no local binary: the program comes from the chain.
cd "$WORK_DIR"
send 0 pay-from-chain --program "$ADDRESS" --idl "$PROJECT_DIR/$PROGRAM-idl.json" \
    send --root seat --signer seat --seal "keys:$KEYS" -- pay --employee "keys:$KEYS" --amount 200
grep -q "from the chain" "$LOG_DIR/pay-from-chain.log" \
    || fail "pay-from-chain: the program wasn't loaded from the chain (see $LOG_DIR/pay-from-chain.log)"
paid_is seat 500 --program "$ADDRESS" --idl "$PROJECT_DIR/$PROGRAM-idl.json"
cd "$PROJECT_DIR"

# ─── Wallet A: a public seat, credits with a recovery binding ─────────────

log "Recoverable credits..."
step new-boss "$WALLET_BIN" account new public --label boss
step pub-treasury "$SPEL_BIN" pda account:boss
PUB_TREASURY="$(tail -1 "$LOG_DIR/pub-treasury.log")"
# A fresh public PDA takes its first deposit only by program and seed (its admission evidence).
expect_exit fund-pub-treasury-by-id 1 "$SPEL_BIN" transfer --from "$FEE_PAYER" --to "$PUB_TREASURY" --amount 1000
grep -q "not admitted" "$LOG_DIR/fund-pub-treasury-by-id.log" \
    || fail "fund-pub-treasury-by-id: wrong reason (see $LOG_DIR/fund-pub-treasury-by-id.log)"
step fund-pub-treasury "$SPEL_BIN" transfer --from "$FEE_PAYER" --to pda:account:boss --amount 1000
send 0 open-boss send --root boss --signer boss --fee-payer "$FEE_PAYER" -- open --treasury "$PUB_TREASURY"
paid_is boss 0

# boss pays from public execution: the treasury's native balance, declared (a public call infers
# nothing).
pay_boss() {
    local want="$1" name="$2"; shift 2
    send "$want" "$name" send --root boss --signer boss --actor "$PUB_TREASURY@native" \
        --recover "keys:$KEYS" "$@" -- pay --employee "keys:$KEYS" --amount 100
}
# No binding yet: private, the binding is published, the private payer pays.
pay_boss 0 recover-unbound --fee-payer seat
grep -q "privacy-preserving" "$LOG_DIR/recover-unbound.log" \
    || fail "recover-unbound: not private (see $LOG_DIR/recover-unbound.log)"
paid_is boss 100
# Bound now: the same call is public. A private payer is refused before sending.
pay_boss 1 recover-bound-private-payer --fee-payer seat
grep -q "recovery binding is already on chain" "$LOG_DIR/recover-bound-private-payer.log" \
    || fail "recover-bound-private-payer: wrong reason (see $LOG_DIR/recover-bound-private-payer.log)"
pay_boss 0 recover-bound --fee-payer "$FEE_PAYER"
grep -q "published by a public transaction" "$LOG_DIR/recover-bound.log" \
    || fail "recover-bound: not planned public (see $LOG_DIR/recover-bound.log)"
! grep -q "privacy-preserving" "$LOG_DIR/recover-bound.log" \
    || fail "recover-bound: planned private (see $LOG_DIR/recover-bound.log)"
paid_is boss 200

# ─── Wallet B: collect ────────────────────────────────────────────────────

e2e_wallet "$WALLET_B"
log "Collecting..."
# Receiving is a private transaction too, paid from a private account. A collector pays, so the
# employee's balance is only what it was paid.
FEE_PAYER_B="$(lez_fee_payer "$WALLET_BIN")"
[ -n "$FEE_PAYER_B" ] || fail "couldn't import the funded debug account into wallet B"
step new-collector "$WALLET_BIN" account new private --label collector
COLLECTOR="$(sed -n 's/.*account_id \(Private\/[A-Za-z0-9]*\).*/\1/p' "$LOG_DIR/new-collector.log" | head -1)"
[ -n "$COLLECTOR" ] || fail "no account id in $LOG_DIR/new-collector.log"
step fund-collector "$SPEL_BIN" transfer --from "$FEE_PAYER_B" --to collector --amount 1000000000
step pending "$WALLET_BIN" pending list
# One line per credit: `Message <position>: to … from …`.
POSITIONS="$(sed -n 's/^Message \([0-9][0-9]*\):.*/\1/p' "$LOG_DIR/pending.log")"
[ "$(echo "$POSITIONS" | grep -c .)" -eq 4 ] \
    || fail "expected 4 pending credits, 2 sealed and 2 recoverable (see $LOG_DIR/pending.log)"
for position in $POSITIONS; do
    step "receive-$position" "$WALLET_BIN" pending receive --position "$position" --fee-payer "${COLLECTOR#Private/}"
done
step balance "$WALLET_BIN" account get --account-id employee
grep -q '^Balance 700,' "$LOG_DIR/balance.log" || fail "employee: expected a balance of 700 (see $LOG_DIR/balance.log)"
log "  employee: balance 700"

log "🎉 Payroll e2e passed. Logs: $LOG_DIR"
