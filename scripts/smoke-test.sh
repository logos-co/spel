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

# shellcheck source=lib/e2e.sh
source "$(dirname "$0")/lib/e2e.sh"
e2e_init SMOKE "${TMPDIR:-/tmp}/spel-smoke" "$@"
e2e_scaffold smoke-counter
e2e_chain
e2e_deploy

# ─── Public ───────────────────────────────────────────────────────────────

log "Public sends..."
step new-alice "$WALLET_BIN" account new public --label alice
send 0 public-increment send --root alice --signer alice --fee-payer "$FEE_PAYER" -- increment --by 2
send 0 public-increment-2 send --root alice --signer alice --fee-payer "$FEE_PAYER" -- increment --by 3
count_is "alice@$PROGRAM" 5
# A dry run prints the plan and sends nothing.
send 0 public-dry-run send --root alice --signer alice --fee-payer "$FEE_PAYER" --dry-run -- increment --by 1
grep -q "Dry run: nothing sent." "$LOG_DIR/public-dry-run.log" || fail "public-dry-run: no dry-run notice"
send 0 public-dry-run-json send --root alice --signer alice --fee-payer "$FEE_PAYER" --dry-run=json -- increment --by 1
step encode "$SPEL_BIN" encode increment --by 1
python3 - "$LOG_DIR/public-dry-run-json.log" "$(tail -1 "$LOG_DIR/encode.log")" <<'PY' \
    || fail "unexpected dry-run plan (see $LOG_DIR/public-dry-run-json.log)"
import json, sys
plan = json.load(open(sys.argv[1]))
assert plan["message"]["name"] == "Increment", plan["message"]
assert plan["message"]["hex"] == sys.argv[2].strip().removeprefix("0x"), (plan["message"], sys.argv[2])
assert len(plan["signers"]) == 1 and plan["fee_payer"], plan
assert not plan["private"], plan
PY
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
