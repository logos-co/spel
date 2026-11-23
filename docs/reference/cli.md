# CLI

`spel` is an IDL-driven command-line client for programs on LEZ's actor model. It generates
IDLs from a program's core crate, sends messages to actors through LEZ's wallet, reads and
decodes actor state, and deploys programs.

For a guided walkthrough, see the [Tutorial](../tutorial.md). The IDL format is in
[idl.md](idl.md).

---

## Invocation

```bash
spel [GLOBAL OPTIONS] <COMMAND> [ARGS]
spel [GLOBAL OPTIONS] -- <COMMAND> [ARGS]
```

Global options may appear anywhere before `--`. Everything after `--` belongs to the command
(for `send`, the message and its fields), so a message field never collides with a global
option.

### Global options

| option | meaning |
| --- | --- |
| `-i, --idl <FILE>` | the IDL (v2). Overrides a `spel.toml` IDL. |
| `-p, --program <NAME\|ADDRESS>` | a `spel.toml` program name (also loads its IDL), or a program address in base58 or hex |
| `--fee-payer <ACCOUNT>` | the account that pays the fee (`send`, `deploy`); for a private call, an owned private account |
| `--binary <FILE>` | the program's binary (the R0BF `.bin`). Overrides a `spel.toml` `binary`. A private call is proved with it, and `pda --group` derives from its ImageID |
| `-t, --type <TYPE>` | `inspect`: decode as this IDL type instead of the state type |
| `-d, --data <HEX>` | `inspect`: decode these bytes offline instead of fetching |
| `--format <text\|hex\|json>` | `program-id` output format |

### Wallet

Commands that touch the chain open LEZ's wallet from `LEE_WALLET_HOME_DIR`
(`LEE_WALLET_PASSWORD` for encrypted storage). The wallet's config names the sequencer.

### Accounts and programs

- **Accounts** are base58 or hex ids, with an optional `Public/` or `Private/` prefix, or
  **wallet labels** (`wallet account new public --label alice`). Labels resolve in `--root`,
  `--actor`, `--signer`, `--fee-payer`, `inspect`, and in account-typed message fields.
  An account is **private** when it has the `Private/` prefix, when its label names a private
  account, or when it's a bare id the wallet witnesses as private. That makes `send` use a
  [private call](#private-calls).
- **Programs** are addressed by their header account (the address `spel deploy` prints), not
  their ImageID. A `spel.toml` name works wherever a program is expected. `native` is the native
  token program: an account's actor under it is its balance.
- **Actors** are written `ACCOUNT@PROGRAM`: `alice@token`, `vault@native`.

---

## Configuration: `spel.toml`

`spel` walks up from the current directory to the first `spel.toml`. Paths in it are relative
to the file.

```toml
[programs.token]
address = "B6Es5QgFbbB47PLmrBW7qnFzATpXeRrBG2P3XdKQs86C"   # the deployed header account
idl     = "artifacts/token-idl.json"
binary  = "target/guest/token.bin"                          # default for `spel deploy`

[programs.nft]
address = "9Afg4tSzEQJAiWF2WoQXonY8Chqg4k2njXsy8ywyz69b"
idl     = "artifacts/nft-idl.json"
```

- `--program <name>` selects an entry: its `address` becomes the default program and its
  `idl` the IDL.
- `ACCOUNT@<name>` names an entry's program; `inspect ACCOUNT@<name>` also loads that entry's
  IDL (an explicit `--idl` wins).
- With exactly one `[programs.<name>]`, or a single unnamed `[program]` table, it is selected
  without `--program`. With several, pass `--program`.
- `[program]` and `[programs]` are mutually exclusive.
- Each deployment gets a fresh address: update `address` after every deploy.

**IDL/program check:** when a program has a `spel.toml` name, `send` and `inspect` refuse an
IDL whose `name` differs from it. Layouts of different programs can line up, so a token IDL
would otherwise "decode" an NFT actor into plausible nonsense. With a raw program id the IDL
you pass is trusted.

---

## `send`

Send one message to an actor.

```bash
spel [GLOBAL OPTIONS] send --root <ACCOUNT>[@<PROGRAM>] [SEND OPTIONS] [--] <MESSAGE> [--<field> <value>]...
```

| option | meaning |
| --- | --- |
| `--root <ACCOUNT>[@<PROGRAM>]` | **required.** The actor that receives the message. |
| `--actor <ACCOUNT>[@<PROGRAM>]` | declare one more actor (repeatable). `PROGRAM` defaults to the root's. |
| `--signer <ACCOUNT>` | sign with this public account (repeatable). The wallet must hold its key. |
| `--seal <DEST>` | a credit the call sends from private execution, sealed to its private recipient (repeatable). Makes the call private |
| `--recover <DEST>` | a credit the call sends from public execution to a private recipient, published with a recovery binding (repeatable). Makes the call private |
| `--no-auto-actors` | declare only the root, the `--actor`s and the signers; a private call infers nothing |
| `--gas-limit <N>` | execution gas limit (a wallet config override for this call) |
| `--dry-run`, `--dry-run=json` | print the transaction and send nothing |

The message name is the IDL variant in kebab-case, snake_case or as written (`new-definition`,
`new_definition`, `NewDefinition`). Fields are `--<field>` in kebab-case; unknown fields are
refused. `spel --idl <IDL> -- <message> --help` lists a message's fields and documentation.

### The root's program

From `@PROGRAM` on `--root`; else `--program`; else the `spel.toml` program named like the IDL
(`token` for the token IDL). Without any of these `send` stops.

### The access list

LEZ loads only the actors a transaction declares; a message to an undeclared actor fails the
whole transaction. `send` declares, in order:

1. the root;
2. each `--actor`;
3. unless `--no-auto-actors`: an actor on the root's program for each message field of type
   `account_id` or `Option<account_id>` (a `none` adds nothing), except for accounts that
   already have an actor in the list (so an explicit `--actor` replaces the default);
4. an actor on the root's program for each signer whose account has none yet.

Each actor is declared once. Ids nested in structs or vectors don't add actors, and neither
do actors a conversation reaches through state alone; declare those with `--actor`. Example:
a token `burn` reaches the holding's definition, which is not a message field:

```bash
spel --program token send --root bob --signer bob --actor usdc -- burn --amount 20
```

Declaring an actor that nothing reaches is harmless.

### Signers

Signers are never inferred. A signature authorizes an **account**, not an actor: every actor
of a signing account in the access list is authorized, under every program, for every message
in the transaction. Before sending, `send` prints the access list and what each signature
authorizes:

```text
transfer (tag 2, 49 bytes) to (alice (J3q2…), token)
Access list:
  (alice (J3q2…), token)   root, signs
  (bob (rdey…), token)     field `to`
alice (J3q2…) signs. In this transaction that authorizes:
  (alice (J3q2…), token)   root
```

A signer whose key the wallet doesn't hold is refused before anything is sent.

### Fees

`--fee-payer` names the paying account; it may be outside the access list (it co-signs). Without
it the wallet picks a funded signer.

### Private calls

A call is sent as a **privacy-preserving transaction** when an actor's account is private, or
when it has a `--seal` or `--recover` credit. The wallet runs the call locally, against the
decrypted states it holds, and proves it with the program's binary (`--binary`, or the
`spel.toml` program's `binary`). Only the proof, nullifiers, encrypted notes and whatever
crosses into public execution are published.

**Private accounts take part through the wallet's keys**, not signatures:
- an account of the wallet's own key tree;
- a private PDA keyed by one of them, or by a wallet group's secret, which must be registered
  first with [`pda --register`](#pda).

A private account authorizes by being witnessed, so `--signer` names only public accounts. A
public actor in a private call is declared as in a public one.

**The access list is inferred from simulation**, not from the message's fields: a field may name
a private account no one in the transaction can witness. The wallet simulates the call before
proving it. When the simulation reaches an actor that isn't declared, `send` adds it (resolved
through the wallet: private if registered, else public) and simulates again, up to 8 times.
Each one is printed:

```text
pay (tag 3, 33 bytes) to (seat (9Wj…), payroll), as a privacy-preserving transaction
Access list:
  (seat (9Wj…), payroll) root, private: authorized by this wallet's keys
Credit sealed to emp1
  + (treasury (8bw…), payroll) reached in simulation, private
```

When anything was inferred, `send` also prints the same call with every actor declared
(`--no-auto-actors`). The simulation finds the actors a call reaches by sync messages. It
doesn't find the destination of an **async** credit, so declare a witnessed account's balance
(`--actor seat@native`) for its credit to apply in the transaction.

**Credits to a private recipient** need the recipient's keys:

| `DEST` | |
| --- | --- |
| a private account of this wallet (label, `Private/<id>`) | its keys; the credit still waits for it, and isn't applied in the transaction |
| `keys:<FILE>` | keys from `wallet account show-keys` output (two lines: npk, then vpk), for someone else's account |
| a public account | declared in the transaction, so its credit applies there |

A credit from private execution is **sealed** (`--seal`): only the recipient reads it, and it
comes from a blinded alias. A credit from a public actor can't be sealed, so it goes with a
**recovery binding** (`--recover`): its amount and sender are public. The recipient collects
either with `wallet pending receive`. A public recipient must already exist on chain, or the call
is refused (`UnadmittedPublicActor`).

With credits, only the listed ones are delivered in the transaction. Without them, every async
message to a witnessed account is.

After inclusion, `send` waits for the block, fails with exit `2` if the transaction reverted
there (its fee is still charged), syncs the wallet's private accounts, and prints what changed,
with private states read from the wallet's decrypted copies. A dry run of a private call shows
the declared actors only: inference needs the wallet's simulation, which runs only when sending.

```bash
# Payroll: the treasury is a private PDA keyed by the seat; the payee's credit is sealed.
spel --program payroll pda account:seat --owner seat --register --label treasury
spel --program payroll send --root seat -- open --treasury treasury --period-ms 60000
spel --program payroll send --root seat --seal emp1 -- pay --employee emp1
spel --program payroll send --root seat --actor seat@native -- withdraw --amount 500000
```

### Outcome and exit codes

The wallet simulates the transaction before sending it. If it would be refused, by the program
or by LEZ's rules (an undeclared actor, say), the wallet doesn't send it, since a refused
transaction still pays its fee, and `send` prints the reason and exits `2`. Otherwise `send` reads every declared actor before and after the
transaction and prints the ones that changed, decoded through the IDL where it describes their
program:

```text
Submitted af5b…, waiting for a block...
Included in block 356. Changed:
  (alice (J3q2…), token)
    before: {"Holding":{"balance":"1000","definition_id":"2hiv…"}}
    after:  {"Holding":{"balance":"700","definition_id":"2hiv…"}}
  (bob (rdey…), token)
    before: {"Holding":{"balance":"0","definition_id":"2hiv…"}}
    after:  {"Holding":{"balance":"300","definition_id":"2hiv…"}}
```

| exit | meaning |
| --- | --- |
| `0` | included; at least one declared actor changed |
| `2` | nothing changed: refused in simulation (not sent; the reason is printed), reverted after inclusion (private calls; the fee is charged), or it was included and changed no declared actor (a no-op, like a second `initialize-holding`) |
| `1` | not sent or not included: bad arguments, unknown message or field, unresolvable account or program, a signer without a key in the wallet, an IDL that doesn't match the root's program, a wallet error, or a timeout |

stdout carries only the transaction hash; the guardrail, progress and diff go to stderr.

### Dry run

`--dry-run` prints the guardrail and the encoded message and sends nothing. `--dry-run=json`
prints a machine-readable plan:

```json
{
  "message": { "name": "Transfer", "tag": 2, "hex": "02…" },
  "root": { "account": "J3q2…", "program": "B6Es…" },
  "actors": [
    { "account": "J3q2…", "program": "B6Es…", "authorizes": true,  "private": false, "reason": "root" },
    { "account": "rdey…", "program": "B6Es…", "authorizes": false, "private": false, "reason": "field `to`" }
  ],
  "signers": ["J3q2…"],
  "fee_payer": null,
  "private": false,
  "seal": [],
  "recover": []
}
```

A dry run works without a wallet (raw ids only; labels don't resolve and keys aren't checked).

### Examples

```bash
# Token: create, initialize, transfer, mint (definition is its own authority)
spel --program token send --root usdc --signer usdc --signer alice -- \
  new-definition --name USDC --total-supply 1000 --mint-authority usdc --holding alice
spel --program token send --root bob --signer bob -- initialize-holding --definition usdc
spel --program token send --root alice --signer alice -- transfer --to bob --amount 300
spel --program token send --root usdc --signer usdc -- mint --definition usdc --to bob --amount 50

# NFT: a struct field as a JSON object
spel --program nft send --root art --signer art --signer alice -- new-definition \
  --name Art --printable-supply 3 \
  --metadata '{"standard":"Simple","uri":"ipfs://…","creators":"me"}' --master alice

# Script-friendly: the hash on stdout, the outcome in the exit code
if TX=$(spel --program token send --root alice --signer alice -- transfer --to bob --amount 1); then
  echo "changed state in $TX"
elif [ $? -eq 2 ]; then
  echo "refused or a no-op: nothing changed"
fi
```

---

## `inspect`

Read an actor's state and decode it through the IDL.

```bash
spel [--idl <IDL>] [--program <P>] inspect <ACCOUNT>[@<PROGRAM>] [--type <TYPE>]
spel --idl <IDL> inspect <anything> --data <HEX> [--type <TYPE>]
```

- **With a program** (`@PROGRAM`, else `--program`, else the `spel.toml` program named like the
  IDL): prints the actor's state decoded as the IDL's state type, or as `--type`. An actor with
  no state prints `null` (and "no state" on stderr). Without an IDL the state is printed as hex.
- **Without a program:** lists every program holding state on the account, keyed by `spel.toml`
  name or program id, as hex:

  ```bash
  $ spel inspect alice
  {
    "nft": "01a25f…",
    "token": "01194a…"
  }
  ```

- **`--data <HEX>`** decodes bytes offline; no wallet or sequencer needed.
- **A private account** is read from the wallet's decrypted copy, after syncing the wallet's
  private accounts. Only a wallet that witnesses it can read it.
- **`ACCOUNT@native`** prints the account's balance: `{"balance": "1000"}`.

Decoding consumes every byte: leftover bytes are an error, so a wrong type doesn't decode
silently. The JSON follows the core types' serde form, with two differences: `u64`/`u128`
(and `i64`/`i128`) are strings so JSON readers keep their precision, and fixed byte arrays
are hex. Account ids are base58. Enums are `{"Variant": value}` (a unit variant is just
`"Variant"`); a tuple variant with one field holds that value, with several an array.

```bash
$ spel --program token inspect alice
{
  "Holding": {
    "balance": "700",
    "definition_id": "2hivK471mcXpSnfRiKDhKjEKCqsD4wQ88BpBLoZyy3Xn"
  }
}
```

---

## `encode`

Print a message's Borsh encoding as hex: the variant tag, then each field in declaration order.

```bash
spel --idl token.json encode transfer --to <ACCOUNT_ID> --amount 300
# 02<32-byte to><u128 LE amount>
```

Fields take the same values as in `send`, except that wallet labels don't resolve.

---

## `generate-idl`

Generate an IDL v2 from a program's core crate. The program needs no annotations.

```bash
spel generate-idl <CRATE_DIR> --message <ENUM> [--state <TYPE>] [--events <MODULE>] [--namespace <NAME>]
```

| option | meaning |
| --- | --- |
| `<CRATE_DIR>` | the core crate (with `Cargo.toml` and `src/lib.rs`) |
| `--message <ENUM>` | **required.** The external message enum; its variant order gives the Borsh tags |
| `--state <TYPE>` | the actor state type |
| `--events <MODULE>` | the module holding the events (`event`, or nested `a::b`) |
| `--namespace <NAME>` | the IDL name and event namespace; defaults to the crate name without `_core` |

```bash
spel generate-idl programs/token/core --message Message --state TokenState --events event > token-idl.json
```

What it reads, and the failures it reports, are described in [idl.md](idl.md#generation).
The IDL goes to stdout.

---

## `idl`

```bash
spel --idl <IDL> idl
```

Prints the loaded IDL. `spel --idl <IDL>` with no command prints help with the IDL's messages
and their flags. IDLs of version 1 (instruction-based) are refused with a message to
regenerate.

---

## `deploy`

```bash
spel deploy [BINARY] --fee-payer <ACCOUNT> [--immutable]
```

Deploys a program through LEZ's `program_loader`: creates a fresh header account and the
segment accounts the binary needs in the wallet, uploads the binary, and prints the program's
address (the header account) on stdout; progress goes to stderr, so
`ADDR=$(spel deploy … )` works. `BINARY` defaults to the `spel.toml` program's `binary`.
`--fee-payer` must be a funded account in the wallet, since the new accounts hold nothing.
`--immutable` marks the program as one that can't be upgraded later.

Each deploy produces a new address. The header's key stays in the wallet.

---

## `program-id`

```bash
spel program-id <BINARY>... [--format text|hex|json]
```

Prints the ImageID of a packaged guest binary (the R0BF `.bin`, not the raw ELF beside it; a
raw ELF is recognised and reported). The ImageID identifies the code; it is **not** the
program's address, which `spel deploy` prints.

---

## `pda`

```bash
spel --program <ADDRESS> pda <SEED> [SEED]...
```

Derives a public PDA of the program: `AccountId::for_public_pda(program, seed)`. Each seed is
64 hex characters (32 bytes) or a string of at most 32 bytes, zero-padded. Several seeds are
combined as `SHA-256(seed1 ‖ seed2 ‖ …)`. Programs document their own seeds; the IDL has none.

### Private PDAs

```bash
spel --program <P> pda <SEED>... --owner <ACCOUNT> [--register [--label <LABEL>] [--rescan]]
spel --program <P> --binary <BIN> pda <SEED>... --group <GROUP> [--register [--label <LABEL>] [--rescan]]
```

A private PDA, `AccountId::for_private_pda(program, seed, npk, vpk)`, with the keys of:
- **`--owner`**: a private account of this wallet's key tree. Only its owner can put the PDA in a
  transaction.
- **`--group`**: a wallet group's secret (`wallet group new | join`), derived for the program's
  ImageID, which comes from its binary. Every holder of the secret derives the same PDA and can
  put it in a transaction.

The wallet can't find a PDA from what it stores, so `--register` records it, labelled `--label`
if that label is free; `send` can then witness it, and syncs follow it. `--rescan` then
rediscovers private accounts from genesis, for a PDA that already has history on chain. The
address goes to stdout either way.

A seed here may also be 64 hex characters with `0x`, or `account:<ACCOUNT>`: that account's id
(a label resolves through the wallet).

---

## `blind`

```bash
spel blind <BASE> <FACTOR> [--hex]
```

Prints `AccountId::blinded(base, factor)` in base58 (or `0x` hex with `--hex`): the
domain-separated SHA-256 LEZ uses for blinded addresses, which programs also use to derive
seeds and pseudonyms. `BASE` and `FACTOR` are 64 hex characters (optionally `0x`), base58 ids,
wallet labels, or `le64:N` (N as 8 little-endian bytes, zero-padded to 32). Blinds compose:

```bash
SEED=$(spel blind 0x$CREATE_KEY le64:1 --hex)                          # a generation's seed
PSEUDONYM=$(spel blind seat $(spel blind 0x$SECRET 0x$CREATE_KEY))     # a self-named member
```

---

## `transfer`

```bash
spel transfer --from <ACCOUNT> --to <ACCOUNT | keys:FILE> --amount <N>
```

Moves native balance between any two accounts the wallet can act for (public, private, or a
registered private PDA, including a group's), or to a private account known by its keys, whose
credit waits for its owner. Private sides make it a privacy-preserving transaction. It prints the
hash on stdout and waits for the block; a revert exits `2`.

---

## Value formats

| IDL type | CLI value |
| --- | --- |
| `u8` … `u128`, `i8` … `i128` | decimal |
| `bool` | `true` / `false` (a bare flag means `true`) |
| `string` | as is |
| `account_id` | base58 or hex; in `send` also a wallet label |
| `program_id` | eight comma-separated `u32`s |
| `[u8; N]` | 2N hex characters, or a string of at most N bytes (right-padded) |
| `Vec<u8>`, `Vec<u32>`, `Vec<u64>`, `Vec<u128>`, `Vec<bool>` | comma-separated: `1,2,3` |
| `Vec<[u8; 32]>`, `Vec<account_id>` | comma-separated base58 or hex ids |
| `Vec<string>` | repeat the flag: `--tag a --tag b` |
| `Option<T>` | the value, or `none` |
| struct | JSON object keyed by field name; a tuple struct takes a JSON array |
| enum | the variant name for a unit variant, or `{"Variant": {…fields}}` |

Inside JSON, strings are given as JSON strings and everything else in its JSON form:
`'{"kind":"B","uri":"ipfs://x","owner":"none"}'`.
