# SPEC: `spel --dry-run` Transaction Summary

## Context

When running `spel` commands with `--dry-run`, users should see the complete local transaction picture before any submission. This enables validation and scripting use cases.

The current arg-parsing base (#134) has fixed CLI flag handling. The `--dry-run` flag exists but does not yet produce a transaction summary.

## What

### CLI Flag

```bash
spel --dry-run [text|json]  # text is default
spel --dry-run=text          # equivalent
spel --dry-run=json          # JSON output
```

- `--dry-run` alone → text summary to stdout, no submission
- `--dry-run json` → JSON object to stdout, no submission
- `--dry-run=json` → same as above (equals syntax)
- Without `--dry-run` → normal submission

### Text Summary Format

```
=== Dry Run ===
Program ID:  <hex or base58>
Accounts:
  owner     → Nsxxxxx…  [signer, writable]
  vault     → 4Lp3gkH…  [writable]
  PDA vault → 4Lp3gkH…
    seeds: [program_id, "owner"]
Arguments:
  --target Owner/abc123...
  --amount 100
Instruction data: <hex>
Signers:
  owner: nonce=42
================
Dry run complete — not submitted.
```

### JSON Format

```json
{
  "program_id": "Nsxxxxx…",
  "accounts": [
    {"name": "owner", "id": "Nsxxxxx…", "flags": ["signer", "writable"]},
    {"name": "vault", "id": "4Lp3gkH…", "flags": ["writable"]},
    {"name": "vault", "id": "4Lp3gkH…", "is_pda": true, "seeds": ["program_id", "owner"]}
  ],
  "arguments": {"target": "Owner/abc123…", "amount": 100},
  "instruction_data": "dead…",
  "signers": {"owner": {"nonce": 42}}
}
```

## Constraints

1. **Zero new dependencies** — use only existing crates
2. **Format strings** verified with `rustc` before commit
3. **No duplicate program binary reads** — read once, reuse
4. **PDA seed display** — derived from IDL, show Const/account/Arg seeds per PDA
5. **Non-fatal if wallet absent** — nonce may be unknown, show `(unknown)`

## Files to Change

- `spel-cli/src/tx.rs` — main implementation
- `spel-cli/src/lib.rs` — `--dry-run[=text|json]` flag parsing
- `spel-cli/src/cli.rs` — help text update
- `README.md` — usage examples

## Out of Scope

- Privacy warnings (not applicable — local RPC)
- Submission retry logic
- Wallet key management
