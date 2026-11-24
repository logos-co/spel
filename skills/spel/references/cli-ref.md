# CLI Reference

Condensed cheatsheet for `spel`. The full reference is `docs/reference/cli.md`.

## Invocation

```bash
spel [GLOBAL OPTIONS] <COMMAND> [ARGS]
spel [GLOBAL OPTIONS] -- <COMMAND> [ARGS]     # `--` keeps message fields apart from global options
```

| global option | meaning |
| --- | --- |
| `-i, --idl <FILE>` | IDL v2 file (overrides spel.toml) |
| `-p, --program <NAME\|ADDRESS>` | spel.toml program (also loads its IDL) or a program address |
| `--fee-payer <ACCOUNT>` | pays fees (`send`, `deploy`); may be outside the access list; the wallet must hold its key; a private call's payer is an owned private account |
| `-t, --type <TYPE>` | `inspect`: decode as this type |
| `-d, --data <HEX>` | `inspect`: decode offline |
| `--format text\|hex\|json` | `program-id` output |
| `--binary <FILE>` | the program's `.bin`: proves private calls, gives `pda --group` its ImageID |

Accounts: base58, hex, `Public/<id>`, `Private/<id>`, or a wallet label. Programs: an address,
a spel.toml name, or `native` (an account's balance). Actors: `ACCOUNT@PROGRAM`.

## spel.toml

```toml
[programs.token]
address = "<header account from spel deploy>"
idl     = "artifacts/token-idl.json"
binary  = "target/guest/token.bin"      # optional: default for `spel deploy`
core    = "programs/token/core"         # optional: the crate `generate-idl` reads

[programs.token.idl-gen]                # optional: generate-idl names; flags win
message = "Message"
state   = "TokenState"                  # or a list for several untagged state types
events  = "event"
```

One `[programs.x]` (or an unnamed `[program]`) is selected automatically; with several, pass
`--program <name>`. `inspect ACCOUNT@<name>` loads that program's IDL unless `--idl` is given.

## Commands

### send

```bash
spel --program token send --root alice --signer alice -- transfer --to bob --amount 300
spel --program token send --root bob --signer bob --actor usdc -- burn --amount 20
spel --program token send --root alice --signer alice --dry-run=json -- transfer --to bob --amount 1
```

| flag | meaning |
| --- | --- |
| `--root A[@P]` | receiving actor (required). Program: `@P`, else `--program`, else the spel.toml program named like the IDL |
| `--actor A[@P]` | declare another actor (repeatable); replaces the field default for that account |
| `--signer A` | authorize an account (repeatable): public signs, private uses the wallet's authorization key; the wallet must hold the key |
| `--seal D` / `--recover D` | a credit to private recipient `D` (a wallet account or `keys:FILE`), sealed (from private execution) or with a recovery binding (from public execution); makes the call private. A public recipient is refused: declare it, `--actor bob@token` |
| `--no-auto-actors` | no actors from message fields (public) or from simulation (private) |
| `--gas-limit N` | gas limit for this call |
| `--dry-run[=json]` | print, don't send (works without a wallet on raw ids) |

Message names: kebab-case, snake_case or the variant name. Fields: `--<field>` in kebab-case;
unknown fields are refused. `spel --idl X -- <message> --help` shows a message.

Private calls (a private actor, `--seal` or `--recover`): proved with the binary; only
`--signer` authorizes, as in public calls (a private root or `--actor` alone can be credited,
not debited; a private PDA only through its program's seed grant, so `--signer` refuses it);
actors reached by sync messages are inferred from simulation and added without authority
(`+ … reached in simulation`), except on a signer's account, which stops the send (exit 2)
with the declaring command, as does a ninth one; an async credit's destination isn't inferred (declare
`ACCOUNT@native`, or `--seal`/`--recover`, one per recipient). `keys:FILE` in an account field
gives the program the id only; seal it explicitly.

```bash
spel --program payroll send --root seat --signer seat -- open --treasury treasury --period-ms 60000
spel --program payroll send --root seat --signer seat --seal keys:emp1.keys -- pay --employee keys:emp1.keys
spel --program payroll send --root seat --signer seat --actor seat@native -- withdraw --amount 500
```

Exit codes: `0` included and changed; `2` nothing changed (refused in simulation and not
sent, reason printed; reverted; or included as a no-op); `1` error or not included.
stdout: the transaction hash only.

### inspect

```bash
spel inspect alice@token                       # decoded state, or null
spel --program token inspect alice             # same
spel inspect alice                             # every program with state on alice, hex
spel --idl token.json inspect x --data <hex>   # offline
spel inspect usdc@token --type TokenDefinition
spel --program payroll inspect treasury           # a private account: synced, then decrypted
spel inspect seat@native                         # {"balance": "…"}
```

### encode

```bash
spel --idl token.json encode transfer --to <id> --amount 300    # Borsh hex, no chain
```

### generate-idl

```bash
spel generate-idl [<CRATE_DIR>] [--message <ENUM>] [--state <TYPE>]... [--events <MODULE>] [--namespace <NAME>]
spel generate-idl --program token > artifacts/token-idl.json   # everything from spel.toml
```

Values not given come from the `--program` entry's `core` and `[idl-gen]` (or the default
program when no `<CRATE_DIR>` is given). The IDL lists the state types in `state_types`;
repeat `--state` (or give a list in `[idl-gen]`) for several untagged ones, and `inspect` then
prints hex and the candidates until `--type` picks one. Without a named message enum, state
type or events module, the ones the core crate marks are used (crate `spel-attrs`, inert
attributes): `#[message]` on the enum, `#[state]` on the state types, `#[event]` on the trait
the events implement. Names win, with a warning if they differ. No events module and no
marked trait means no events.

### deploy, program-id, pda, idl

```bash
ADDR=$(spel deploy target/guest/token.bin --fee-payer <funded> [--immutable])
spel program-id target/guest/token.bin --format hex        # ImageID, not the address
spel --program <ADDRESS> pda vault <64-hex-seed>           # public PDA from raw seeds
spel -p <P> pda account:seat --owner seat --register --label treasury       # private PDA, owner's keys
spel -p <P> --binary <BIN> pda 0x<seed> --group g1 --register --label vault # private PDA, a group's keys
spel blind <BASE> <FACTOR> [--hex]                         # AccountId::blinded; factor may be le64:N
spel transfer --from seat --to treasury --amount 1000      # native balance, public or private
spel --idl token.json idl                                  # print the IDL
spel --idl token.json                                      # help with the IDL's messages
```

## Value formats

| type | value |
| --- | --- |
| integers | decimal |
| `bool` | `true`/`false`; a bare flag is `true` |
| `account_id` | base58/hex; label or `keys:FILE` in `send` |
| `Option<T>` | value or `none` |
| `[u8; N]` | hex (2N chars) or a string ≤ N bytes |
| `Vec<u8\|u32\|u64\|u128\|bool>` | `1,2,3` |
| `Vec<[u8; 32]>`, `Vec<account_id>` | comma-separated ids (labels, `keys:FILE` in `send`) |
| `Vec<string>` | repeat the flag |
| struct | JSON object: `'{"standard":"Simple","uri":"ipfs://x","creators":"me"}'` |
| enum | `Variant` or `{"Variant": {…}}` |

`inspect` JSON: u64/u128 as strings, account ids base58, byte arrays hex, enums `{"Variant": …}`.
