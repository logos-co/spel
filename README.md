# SPEL — Smart Program Execution Layer

[![CI](https://github.com/logos-co/spel/actions/workflows/ci.yml/badge.svg)](https://github.com/logos-co/spel/actions/workflows/ci.yml)

The client toolchain for programs on [LEZ](https://github.com/logos-blockchain/logos-execution-zone)'s
actor model: generate an IDL from a program's core crate, send messages to actors, read and
decode their state, and deploy programs.

On LEZ's actor model a program is written against `lee_core` directly: an actor is an
(account, program) pair, and a handler receives one message for one actor. SPEL no longer
provides a guest framework; the `#[lez_program]` macros, `spel init` scaffolding and
instruction-style transactions were retired with the move to actors (they remain in git
history).

## Documentation

- **[Tutorial](docs/tutorial.md)** — from a program's core crate to messages on a local sequencer
- **[CLI reference](docs/reference/cli.md)** — every command and flag
- **[IDL format](docs/reference/idl.md)** — IDL v2: messages, state, events, types

## Install

```bash
cargo install --path spel-cli   # installs the `spel` binary
```

SPEL talks to the chain through LEZ's wallet: set `LEE_WALLET_HOME_DIR` to a wallet directory
(and `LEE_WALLET_PASSWORD` if its storage is encrypted).

## At a glance

```bash
# 1. Generate the IDL from the program's core crate (no annotations needed)
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
| `generate-idl <CRATE_DIR> --message <ENUM> [--state <TYPE>] [--events <MODULE>]` | IDL v2 from a core crate |
| `send --root <ACCOUNT>[@<PROGRAM>] … -- <MESSAGE> [--field value]…` | send one message to an actor; reports what changed |
| `inspect <ACCOUNT>[@<PROGRAM>]` | read and decode an actor's state, or list every program holding state on an account |
| `encode <MESSAGE> [--field value]…` | print a message's Borsh bytes as hex |
| `deploy [BINARY] --fee-payer <ACCOUNT>` | deploy a program through LEZ's program loader, print its address |
| `program-id <FILE>…` | print a binary's ImageID (not its address) |
| `--program <ADDRESS> pda <SEED>…` | derive a public PDA from raw seeds |
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
- **Outcome:** LEZ includes a refused transaction in a block without reporting an outcome, so
  SPEL reads every declared actor before and after and prints what changed. It never says
  "confirmed".

| exit | meaning |
| --- | --- |
| `0` | included, and at least one declared actor changed |
| `2` | included, but nothing changed: probably refused (or a no-op) |
| `1` | error: bad arguments, a signer whose key the wallet lacks, an IDL that doesn't match the program, not included |

The transaction hash goes to stdout, everything else to stderr, so `TX=$(spel … send …)`
works. `--dry-run` (or `--dry-run=json`) prints the transaction without sending.

## Crates

| crate | contents |
| --- | --- |
| `spel-cli` (binary `spel`) | the CLI |
| `spel-framework-core` | IDL v2 types, the IDL generator (feature `idl-gen`), the IDL-driven Borsh decoder, PDA helpers |
| `spel-client-gen` | **parked**: generates clients from v1 IDLs; excluded from the workspace until it is ported to actor programs |

## Not yet ported

Private (privacy-preserving) sends, multi-party signing (`sign` / `submit`), events in `send`
output, project scaffolding for actor programs, and client generation.

## License

Dual-licensed under [MIT](LICENSE-MIT) and [Apache-2.0](LICENSE-APACHE-v2).
