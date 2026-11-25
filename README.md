# SPEL — Smart Program Execution Layer

[![CI](https://github.com/logos-co/spel/actions/workflows/ci.yml/badge.svg)](https://github.com/logos-co/spel/actions/workflows/ci.yml)

The client toolchain for programs on [LEZ](https://github.com/logos-blockchain/logos-execution-zone)'s
actor model: generate an IDL from a program's core crate, send messages to actors (publicly or
as privacy-preserving transactions), read and decode their state, and deploy programs.

On LEZ's actor model a program is written against `lee_core` directly: an actor is an
(account, program) pair, and a handler receives one message for one actor. SPEL no longer
provides a guest framework; the `#[lez_program]` macros, `spel init` scaffolding and
instruction-style transactions were retired with the move to actors (they remain in git
history).

## Documentation

- **[Tutorial](docs/tutorial.md)** — from a program's core crate to messages on a local sequencer
- **[CLI reference](docs/reference/cli.md)** — every command and flag
- **[IDL format](docs/reference/idl.md)** — IDL v2: messages, state types, events, types

## Install

```bash
cargo install --path spel-cli   # installs the `spel` binary
```

SPEL talks to the chain through LEZ's wallet: set `LEE_WALLET_HOME_DIR` to a wallet directory
(and `LEE_WALLET_PASSWORD` if its storage is encrypted).

## At a glance

```bash
# 1. Generate the IDL from the program's core crate: name the message enum, state type and
#    events module, or mark them in the crate (#[message], #[state], #[event]) and pass nothing
spel generate-idl programs/token/core --message Message --state TokenState --events event > token.json

# 2. Deploy the guest; prints the program's address
spel deploy target/guest/token.bin --fee-payer <funded-account>

# 3. Send messages (accounts may be wallet labels)
spel --idl token.json --program <address> --fee-payer <funded-account> \
  send --root usdc --signer usdc --signer alice -- \
  new-definition --name USDC --total-supply 1000 --mint-authority none --holding alice
spel --idl token.json --program <address> send --root alice --signer alice -- \
  transfer --to bob --amount 300

# 4. Read an actor's state, decoded through the IDL
spel --idl token.json --program <address> inspect alice
# {"Holding": {"balance": "700", "definition_id": "…"}}
```

With a `spel.toml` naming the program, `--idl` and `--program` collapse to a name:

```toml
[programs.token]
address = "B6Es5QgFbbB47PLmrBW7qnFzATpXeRrBG2P3XdKQs86C"
idl = "artifacts/token-idl.json"
```

```bash
spel --program token send --root alice --signer alice -- transfer --to bob --amount 300
spel inspect alice@token
```

## Commands

| command | what it does |
| --- | --- |
| `generate-idl [<CRATE_DIR>] [--message <ENUM>] [--state <TYPE>]... [--events <MODULE>]` | IDL v2 from a core crate; missing values come from spel.toml (`core`, `[idl-gen]`), then from `spel-attrs` markers |
| `send --root <ACCOUNT>[@<PROGRAM>] … -- <MESSAGE> [--field value]…` | send one message to an actor, publicly or privately; reports what changed |
| `inspect <ACCOUNT>[@<PROGRAM>]` | read and decode an actor's state (a private one from the wallet's decrypted copy), or list every program holding state on an account |
| `encode <MESSAGE> [--field value]…` | print a message's Borsh bytes as hex |
| `deploy [BINARY] --fee-payer <ACCOUNT>` | deploy a program through LEZ's program loader, print its address |
| `program-id <FILE>…` | print a binary's ImageID (not its address) |
| `--program <ADDRESS> pda <SEED>…` | derive a public PDA from raw seeds |
| `pda <SEED> --owner <ACCOUNT> \| --group <GROUP> [--register]` | derive a private PDA under an owned account's keys or a wallet group's, and record it in the wallet |
| `blind <BASE> <FACTOR>` | `AccountId::blinded`, for derived seeds and pseudonyms |
| `transfer --from <ACCOUNT> --to <ACCOUNT \| keys:FILE> --amount <N>` | move native balance, public or private |
| `idl` | print the loaded IDL |

`spel --idl <IDL>` with no command lists the IDL's messages and their flags;
`spel --idl <IDL> -- <message> --help` shows one message with its documentation.

## How `send` builds a transaction

- **Root:** the actor that receives the message. Its program comes from `@PROGRAM`, else
  `--program`, else the `spel.toml` program named like the IDL.
- **Access list:** LEZ loads only declared actors. SPEL declares the root, each `--actor`, an
  actor on the root's program for every `account_id` / `Option<account_id>` message field
  (`none` adds nothing), and each signer. An explicit `--actor` for an account replaces its
  default; `--no-auto-actors` turns the defaults off. Actors named only in state (a token's
  definition, for `burn`) need `--actor`.
- **Signers:** never inferred. One signature authorizes *every* actor of that account in the
  transaction, under every program; SPEL prints, per signer, the actors it authorizes before
  sending.
- **Outcome:** the wallet simulates every send and doesn't send one that would be refused (it
  would still pay its fee); SPEL prints the reason. Otherwise SPEL reads every
  declared actor before and after inclusion and prints what changed. It never says "confirmed".

| exit | meaning |
| --- | --- |
| `0` | included, and at least one declared actor changed |
| `2` | nothing changed: refused in simulation (not sent, reason printed), reverted after inclusion (private calls; the fee is charged), or included as a no-op |
| `1` | error: bad arguments, a signer whose key the wallet lacks, an IDL that doesn't match the program, not included |

The transaction hash goes to stdout, everything else to stderr, so `TX=$(spel … send …)`
works. `--dry-run` (or `--dry-run=json`) prints the transaction without sending.

## Private calls

A call is a **privacy-preserving transaction** when an actor's account is private
(`Private/<id>`, a private wallet label), or when it sends a credit with `--seal` / `--recover`.
The wallet runs it locally against its decrypted states and proves it with the program's binary
(`--binary`, or the `spel.toml` program's `binary`); only the proof, nullifiers, encrypted notes
and what crosses into public execution are published.

```bash
# The seat pays an employee known only by their keys; the credit is sealed to them.
spel --program payroll send --root seat --signer seat \
  --seal keys:emp1.keys -- pay --employee keys:emp1.keys

# A private sender, a public recipient: declare the actor the credit goes to.
spel --program token send --root alice --signer alice --actor bob@token -- transfer --to bob --amount 300
```

- **Authority:** as in public calls, only `--signer` authorizes. A private signer uses the
  wallet's authorization key and covers every actor of its account; a private root or `--actor`
  alone can be credited, not debited. A private PDA is authorized only by its program's seed
  grant (register it first with `spel pda … --register`).
- **Access list:** not filled from message fields, which may name accounts no one in the
  transaction can witness. The wallet's simulation names each actor the call reaches; SPEL adds
  it without authority and simulates again. One on a signer's account stops the send (exit `2`)
  with the command that declares it.
- **Recipients:** a private recipient's async messages need its keys. `--seal DEST` for one sent
  from private execution, `--recover DEST` for one sent from public execution; `DEST` is a wallet
  account or `keys:FILE` (`wallet account show-keys` output). An account field may also take
  `keys:FILE`, which only fills in the derived id. A public recipient isn't sealed: declare it
  with `--actor ACCOUNT@PROGRAM`. The recipient collects a sealed or recovered credit with
  `wallet pending receive`.
- **Fees:** paid in the proof by a private account: a private signer, `--fee-payer`, or one the
  wallet picks.

The sender of a message that reaches public execution appears as a fresh blinded alias.

## Crates

| crate | contents |
| --- | --- |
| `spel-cli` (binary `spel`) | the CLI |
| `spel-framework-core` | IDL v2 types, the IDL generator (feature `idl-gen`), the IDL-driven Borsh decoder, PDA helpers |
| `spel-attrs` | inert `#[message]`, `#[state]` and `#[event]` markers for core crates, read by `generate-idl`; a build-time dependency, never linked into the guest |
| `spel-client-gen` | **parked**: generates clients from v1 IDLs; excluded from the workspace until it is ported to actor programs |

## Not yet ported

Multi-party signing (`sign` / `submit`), events in `send`
output, project scaffolding for actor programs, and client generation.

## License

Dual-licensed under [MIT](LICENSE-MIT) and [Apache-2.0](LICENSE-APACHE-v2).
