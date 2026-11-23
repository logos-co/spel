# Gotchas and Common Mistakes

Lessons from using SPEL against LEZ's actor model. Read before scripting or debugging.

---

## Outcomes

### Check the exit code

The wallet simulates every send. A transaction that would be refused, by the program or by
LEZ's rules, isn't sent (it would still pay its fee): `spel send` prints `Refused in
simulation, so not sent: <reason>` and exits `2`. A transaction that is sent and included but
changes no declared actor also exits `2`: a legitimate no-op (a second `initialize-holding`),
or a refusal the simulation didn't foresee because state changed before inclusion. Scripts must
check the exit code; "included in block N" alone means nothing.

### Why was it refused?

The reason after `Refused in simulation` says. The usual causes:

1. an actor the conversation reaches isn't declared (`… neither a declared public actor nor
   private`; see below);
2. a needed signature is missing (`--signer`);
3. the program's own checks (`Guest panicked: insufficient balance`, wrong authority, wrong
   state);
4. a message to a fresh account that neither signs nor is otherwise admitted.

`--dry-run` shows exactly what was declared and who signs.

## Access lists

### Actors reached through state need `--actor`

`send` declares the root, each `--actor`, an actor on the root's program for each
`account_id` / `Option<account_id>` message field, and each signer. It can't see accounts a
program finds in state. Token `burn` reaches the holding's definition: without
`--actor <definition>` it is refused and burns nothing.

### Actors on another program need `@PROGRAM`

Field defaults are always on the root's program. An AMM message that reaches a token holding
needs `--actor <holding>@token`.

### Ids inside structs or vectors add no actors

Only top-level account fields do. Declare the rest with `--actor`.

### Over-declaring is harmless

An actor nothing reaches is accepted. When in doubt, declare it.

## Signers

### A signature authorizes an account, not an actor

Every actor of a signing account in the access list is authorized, under every program, for
every message in the transaction. The guardrail `send` prints lists them; read it when an
account has actors under several programs.

### Signers are never inferred

A missing signer fails in the program (silently, exit `2`); an extra one lends authority. `send`
refuses a `--signer` whose key the wallet lacks before sending.

### The fee payer can be anyone in the wallet

`--fee-payer` may be outside the access list. Accounts created by `spel deploy` (headers,
segments) are in the wallet but hold nothing.

## Programs and IDLs

### A program is its address, not its ImageID

`spel deploy` prints the header account; that is the program id for `--program`, `@PROGRAM`,
spel.toml and PDA derivation. `spel program-id` prints the ImageID, which only identifies the
code. Every deploy gets a new address: update spel.toml, and don't mix ids across deployments.

### Use the IDL of the program you address

Different programs' state layouts can decode into each other (an NFT master "decodes" as a
token holding). `send` and `inspect` refuse an IDL whose `name` differs from the program's
spel.toml name; with a raw program id SPEL can't check, so pass the right `--idl`.
`inspect ACCOUNT@name` loads that program's IDL from spel.toml unless `--idl` is given.

### Never edit an IDL by hand

Regenerate it with `spel generate-idl`. Message tags are variant positions, so message enums are
append-only: inserting a variant changes every later tag, and deployed programs keep decoding
the old ones.

### Version 1 IDLs are refused

IDLs from the retired `#[lez_program]` framework (with `instructions`) are not readable;
regenerate from the core crate.

### Generation fails loudly on purpose

A `--message`, `--state` or `--events` name that doesn't exist, or a referenced type outside
the crate and its path dependencies, fails the run. Fix the name rather than dropping the flag.

## Private calls

### A private PDA must be registered before it's used

The wallet can't find a private PDA from what it stores. Until `spel pda … --owner | --group …
--register` records it, `send` refuses it ("isn't an account this wallet can witness") and
`inspect` can't read it. For a PDA that already has history (joining an existing group vault),
add `--rescan`.

### Inference finds sync messages, not async credits

A private call's undeclared actors come from the wallet's simulation, one per retry. A sync
message to an undeclared actor stops the simulation, so it's found. An async credit to an
undeclared account isn't: it's left for its recipient. Declare `ACCOUNT@native` when the credit
should apply in the transaction (the caller's own balance, or a successor vault's), and name a
foreign private recipient with `--seal` / `--recover`.

### Seal from private execution, recover from public execution

A credit sent by a private actor (a private vault's balance) is sealed: `--seal DEST`. A credit
sent by a public actor can't be sealed, so it needs a recovery binding: `--recover DEST`, which
publishes its amount and sender. Using `--seal` for a public sender makes the transaction
revert.

### A public payee must exist on chain

Paying a brand-new public account from a private call is refused in simulation with
`UnadmittedPublicActor`: no transaction vouches for it. Fund it once first.

### A credit to your own private account waits

`--seal <your own private account>` addresses it by its keys, so the credit is pending
(`wallet pending receive`), not applied in the transaction. To apply it in place, declare
`ACCOUNT@native` instead.

## Values

### Wide integers are strings in `inspect` output

`"balance": "700"`, not `700`: u64/u128 keep their precision in any JSON reader.

### Message fields: labels, `none`, JSON

Account fields take wallet labels in `send` (not in `encode`). `Option` fields take `none`.
Structs and enum payloads take JSON objects; quote them for the shell.

### Global options vs message fields

Options like `--type`, `--data`, `--program` and `--idl` are read anywhere before `--`. Put the
message after `--` so a field with one of those names isn't taken as a global option.
