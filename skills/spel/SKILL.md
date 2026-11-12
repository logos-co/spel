---
name: spel
description: "Use the SPEL CLI (logos-co/spel) with programs on LEZ's actor model. Use when: (1) generating an IDL from a program's core crate (`spel generate-idl`), (2) sending messages to actors (`spel send`: --root, --actor, --signer, access lists, dry runs), (3) reading or decoding actor state (`spel inspect ACCOUNT@PROGRAM`), (4) deploying a program (`spel deploy`) or computing PDAs, (5) writing spel.toml, scripting spel against a sequencer, or interpreting its exit codes, or (6) any mention of spel, spel.toml, IDL v2, or spel-framework-core. Not for writing the program itself: actor programs are written against lee_core directly."
---

# SPEL

SPEL is the client toolchain for programs on LEZ's actor model. An actor is an
(account, program) pair; a program's handler receives one message for one actor. SPEL:

- generates an **IDL v2** (messages, state, events, types) from a program's **core crate**, with
  no annotations in the program;
- **sends** one message to an actor, declaring the actors the transaction may touch and the
  accounts that sign, and reports what changed;
- **reads** an actor's state and decodes it through the IDL;
- **deploys** programs through LEZ's program loader.

The old `#[lez_program]` guest framework, `spel init`, instruction calls and `sign`/`submit`
are retired. Private sends and client generation (`spel-client-gen`) are not ported yet.

## References

- **[references/quickstart.md](references/quickstart.md)** — generate, deploy, send, inspect, end to end. Read when setting up a program or recalling the sequence.
- **[references/cli-ref.md](references/cli-ref.md)** — every command, flag, value format and exit code. Read when constructing commands.
- **[references/gotchas.md](references/gotchas.md)** — access lists, signers, refusals, addresses vs ImageIDs. Read before scripting or debugging.

## Core workflow

1. **IDL:** `spel generate-idl <core-crate> --message Message --state <StateType> --events event > <name>-idl.json`
2. **Deploy:** `ADDR=$(spel deploy <guest>.bin --fee-payer <funded-account>)`; the address is the program's id from then on.
3. **Name it:** `spel.toml` with `[programs.<name>] address = "$ADDR"`, `idl = "<name>-idl.json"`; wallet labels for accounts.
4. **Send:** `spel --program <name> send --root <account> --signer <account> -- <message> --<field> <value>`
5. **Read:** `spel inspect <account>@<name>`

## Critical rules

- **The access list is explicit.** LEZ refuses a message to an undeclared actor. `send` declares
  the root, each `--actor`, account-typed message fields (on the root's program) and signers.
  Actors reached only through state need `--actor` (a token `burn` needs `--actor <definition>`);
  actors on another program need `--actor ACCOUNT@PROGRAM`.
- **Signers are never inferred.** One signature authorizes every actor of that account in the
  transaction, under every program.
- **Inclusion is not success.** A refused transaction is included with nothing changed. Check
  `send`'s exit code: `0` changed, `2` included but nothing changed (refused), `1` error.
- **A program is its address,** the header account `spel deploy` prints, not its ImageID
  (`spel program-id`). Each deploy gets a new address; update `spel.toml`.
- **Regenerate, never hand-edit, the IDL.** Message tags are variant positions; message enums
  are append-only.
- **Use a matching IDL.** `send`/`inspect` refuse an IDL whose `name` differs from the program's
  `spel.toml` name; with a raw program id the IDL you pass is trusted.
- `spel` needs `LEE_WALLET_HOME_DIR` (and `LEE_WALLET_PASSWORD` for an encrypted wallet).
