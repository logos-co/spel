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
| `--fee-payer <ACCOUNT>` | pays fees (`send`, `deploy`); may be outside the access list |
| `-t, --type <TYPE>` | `inspect`: decode as this type |
| `-d, --data <HEX>` | `inspect`: decode offline |
| `--format text\|hex\|json` | `program-id` output |

Accounts: base58, hex, `Public/<id>`, or a wallet label. Programs: an address or a spel.toml
name. Actors: `ACCOUNT@PROGRAM`.

## spel.toml

```toml
[programs.token]
address = "<header account from spel deploy>"
idl     = "artifacts/token-idl.json"
binary  = "target/guest/token.bin"      # optional: default for `spel deploy`
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
| `--signer A` | sign (repeatable); the wallet must hold the key |
| `--no-auto-actors` | no actors from message fields |
| `--gas-limit N` | gas limit for this call |
| `--dry-run[=json]` | print, don't send (works without a wallet on raw ids) |

Message names: kebab-case, snake_case or the variant name. Fields: `--<field>` in kebab-case;
unknown fields are refused. `spel --idl X -- <message> --help` shows a message.

Exit codes: `0` included and changed; `2` included, nothing changed (refused or no-op); `1`
error or not included. stdout: the transaction hash only.

### inspect

```bash
spel inspect alice@token                       # decoded state, or null
spel --program token inspect alice             # same
spel inspect alice                             # every program with state on alice, hex
spel --idl token.json inspect x --data <hex>   # offline
spel inspect usdc@token --type TokenDefinition
```

### encode

```bash
spel --idl token.json encode transfer --to <id> --amount 300    # Borsh hex, no chain
```

### generate-idl

```bash
spel generate-idl <CRATE_DIR> --message <ENUM> [--state <TYPE>] [--events <MODULE>] [--namespace <NAME>]
```

### deploy, program-id, pda, idl

```bash
ADDR=$(spel deploy target/guest/token.bin --fee-payer <funded> [--immutable])
spel program-id target/guest/token.bin --format hex        # ImageID, not the address
spel --program <ADDRESS> pda vault <64-hex-seed>           # public PDA from raw seeds
spel --idl token.json idl                                  # print the IDL
spel --idl token.json                                      # help with the IDL's messages
```

## Value formats

| type | value |
| --- | --- |
| integers | decimal |
| `bool` | `true`/`false`; a bare flag is `true` |
| `account_id` | base58/hex; label in `send` |
| `Option<T>` | value or `none` |
| `[u8; N]` | hex (2N chars) or a string ≤ N bytes |
| `Vec<u8\|u32\|u64\|u128\|bool>` | `1,2,3` |
| `Vec<[u8; 32]>` | comma-separated ids |
| `Vec<string>` | repeat the flag |
| struct | JSON object: `'{"standard":"Simple","uri":"ipfs://x","creators":"me"}'` |
| enum | `Variant` or `{"Variant": {…}}` |

`inspect` JSON: u64/u128 as strings, account ids base58, byte arrays hex, enums `{"Variant": …}`.
