#!/usr/bin/env bash
# Multi-wallet signing e2e: `send --export` → `spel sign` → `spel submit`, against a local
# standalone sequencer, with two wallets that share no key.
#
# Wallet A holds the fee payer; wallet B holds alice, the counter's account. A exports an
# increment of alice's counter (alice co-signs, A pays), B checks and signs it, and B submits.
# Along the way it checks the refusals:
#   - submitting while alice hasn't signed;
#   - signing in a wallet that holds no missing signer's key;
#   - signing or submitting a file whose message changed after it was signed;
# and that submitting it again changes nothing: the sequencer already holds that transaction, so
# spel finds its first inclusion.
#
# Usage: LEZ_DIR=<logos-execution-zone checkout> scripts/multisig-e2e-test.sh [WORK_DIR]
#
# Needs what smoke-test.sh needs; takes the same environment. WORK_DIR defaults to
# ${TMPDIR:-/tmp}/spel-multisig.

set -euo pipefail

# shellcheck source=lib/e2e.sh
source "$(dirname "$0")/lib/e2e.sh"
e2e_init MULTISIG "${TMPDIR:-/tmp}/spel-multisig" "$@"
e2e_scaffold multisig-counter
e2e_chain
e2e_deploy

WALLET_A="$WORK_DIR/wallet"
WALLET_B="$WORK_DIR/wallet-b"
TX="$WORK_DIR/increment.json"

# ─── Wallet B: alice ──────────────────────────────────────────────────────

e2e_wallet "$WALLET_B"
step new-alice "$WALLET_BIN" account new public --label alice
ALICE="$(sed -n 's/.*account_id \(Public\/[A-Za-z0-9]*\).*/\1/p' "$LOG_DIR/new-alice.log" | head -1)"
[ -n "$ALICE" ] || fail "no account id in $LOG_DIR/new-alice.log"
log "alice (wallet B): $ALICE"

# ─── Wallet A: export ─────────────────────────────────────────────────────

e2e_wallet "$WALLET_A"
log "Exporting from wallet A (alice co-signs)..."
send 0 export send --root "$ALICE" --co-signer "$ALICE" --fee-payer "$FEE_PAYER" \
    --export "$TX" -- increment --by 4
python3 - "$TX" <<'PY' || fail "unexpected export (see $TX)"
import json, sys
tx = json.load(open(sys.argv[1]))
assert tx["version"] == 2, tx["version"]
assert len(tx["witnesses"]) == 1, "only the fee payer has signed"
PY
# The same call with another amount, for a message swapped under the signatures below.
send 0 export-other send --root "$ALICE" --co-signer "$ALICE" --fee-payer "$FEE_PAYER" \
    --export "$WORK_DIR/other.json" -- increment --by 5

expect_exit submit-unsigned 1 "$SPEL_BIN" submit "$TX"
grep -q "not signed by" "$LOG_DIR/submit-unsigned.log" || fail "submit-unsigned: wrong reason"
expect_exit sign-without-key 1 "$SPEL_BIN" sign "$TX" --yes
grep -q "holds the key of no missing signer" "$LOG_DIR/sign-without-key.log" \
    || fail "sign-without-key: wrong reason"

# swap FILE: FILE's witnesses over other.json's message.
swap() {
    python3 - "$1" "$WORK_DIR/other.json" "$2" <<'PY'
import json, sys
signed, other, out = sys.argv[1:]
tx = json.load(open(signed))
tx["message"] = json.load(open(other))["message"]
json.dump(tx, open(out, "w"))
PY
}

# ─── Wallet B: sign, submit ───────────────────────────────────────────────

e2e_wallet "$WALLET_B"
log "Signing in wallet B..."
swap "$TX" "$WORK_DIR/tampered.json"
expect_exit sign-tampered 1 "$SPEL_BIN" sign "$WORK_DIR/tampered.json" --yes
grep -q "doesn't verify" "$LOG_DIR/sign-tampered.log" || fail "sign-tampered: wrong reason"

step sign "$SPEL_BIN" sign "$TX" --yes
grep -q 'Increment {"by":"4"}' "$LOG_DIR/sign.log" || fail "sign didn't decode the message (see $LOG_DIR/sign.log)"

swap "$TX" "$WORK_DIR/tampered-signed.json"
expect_exit submit-tampered 1 "$SPEL_BIN" submit "$WORK_DIR/tampered-signed.json"
grep -q "doesn't verify" "$LOG_DIR/submit-tampered.log" || fail "submit-tampered: wrong reason"

log "Submitting from wallet B..."
step submit "$SPEL_BIN" submit "$TX"
count_is "$ALICE@$PROGRAM" 4

step submit-again "$SPEL_BIN" submit "$TX"
[ "$(tail -1 "$LOG_DIR/submit.log")" = "$(tail -1 "$LOG_DIR/submit-again.log")" ] \
    || fail "submit-again: not the first inclusion (see $LOG_DIR/submit-again.log)"
count_is "$ALICE@$PROGRAM" 4

log "🎉 Multisig e2e passed. Logs: $LOG_DIR"
