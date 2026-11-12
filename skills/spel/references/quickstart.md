# Quickstart: core crate to messages on chain

Uses lez-programs' token program; any actor program with a core crate works the same way.

## 0. Prerequisites

- `cargo install --path spel-cli` (from the spel repo)
- a running LEZ sequencer; `LEE_WALLET_HOME_DIR` → a wallet with a funded account (`$PAYER`)
- the guest binary (lez-programs: `make build-programs` → `target/guest/token.bin`)

## 1. Generate the IDL

```bash
spel generate-idl programs/token/core --message Message --state TokenState --events event \
  > artifacts/token-idl.json
```

The core crate needs no annotations. Doc comments become help text. A missing name fails.

## 2. Deploy

```bash
ADDR=$(spel deploy target/guest/token.bin --fee-payer $PAYER)
```

`$ADDR` is the program's id from now on (not the ImageID). New deploy → new address.

## 3. Configure

```toml
# spel.toml
[programs.token]
address = "<$ADDR>"
idl = "artifacts/token-idl.json"
```

```bash
for n in usdc alice bob; do wallet account new public --label $n; done
```

## 4. Send

```bash
# Create: the definition actor on usdc, the supply to alice; both sign
spel --program token --fee-payer $PAYER send --root usdc --signer usdc --signer alice -- \
  new-definition --name USDC --total-supply 1000 --mint-authority usdc --holding alice

# bob opts in, then receives
spel --program token send --root bob --signer bob -- initialize-holding --definition usdc
spel --program token send --root alice --signer alice -- transfer --to bob --amount 300

# the definition mints (it is its own authority)
spel --program token send --root usdc --signer usdc -- mint --definition usdc --to bob --amount 50

# burn reaches the definition, which no field names: declare it
spel --program token send --root bob --signer bob --actor usdc -- burn --amount 20
```

Each `send` prints the access list and what each signature authorizes, then the actors that
changed. Check the exit code: `0` changed, `2` refused, `1` error.

## 5. Read

```bash
spel inspect alice@token     # {"Holding": {"balance": "700", "definition_id": "…"}}
spel inspect usdc@token      # {"Definition": {"name": "USDC", "total_supply": "1030", …}}
spel inspect alice           # all programs with state on alice
```

## 6. Preview without sending

```bash
spel --program token send --root alice --signer alice --dry-run -- transfer --to bob --amount 1
spel --program token send --root alice --signer alice --dry-run=json -- transfer --to bob --amount 1 | jq .actors
spel --idl artifacts/token-idl.json encode transfer --to <id> --amount 1
```
