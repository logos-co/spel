# Tutorial: A token program with SPEL

This walks through using SPEL with a program on LEZ's actor model, from its core crate to
messages on a local sequencer. The example is the token program from
[lez-programs](https://github.com/logos-co/lez-programs) (`programs/token`); any actor program
with a core crate works the same way.

SPEL is the client side. The program itself is written against `lee_core`; see the LEZ guide
(`docs/lez-guide` in the LEZ repository) for writing actor programs.

## Contents

1. [Prerequisites](#1-prerequisites)
2. [The core crate](#2-the-core-crate)
3. [Generate the IDL](#3-generate-the-idl)
4. [Deploy](#4-deploy)
5. [Name things: `spel.toml` and wallet labels](#5-name-things-speltoml-and-wallet-labels)
6. [Send messages](#6-send-messages)
7. [Read state](#7-read-state)
8. [When a message is refused](#8-when-a-message-is-refused)
9. [Actors the defaults miss](#9-actors-the-defaults-miss)

---

## 1. Prerequisites

- `spel`: `cargo install --path spel-cli` from this repository.
- A LEZ sequencer and wallet at the LEZ version SPEL is pinned to. In a LEZ checkout, run the
  standalone sequencer (`sequencer_service setup` once, then `start`) and set up a wallet.
- `LEE_WALLET_HOME_DIR` pointing at the wallet, with one funded account to pay fees. Below it
  is `$PAYER`.
- The program's guest binary, e.g. `target/guest/token.bin` from lez-programs'
  `make build-programs`.

## 2. The core crate

An actor program keeps its client-facing types in a core crate: the messages it accepts, the
state its actors hold, and its events. From `programs/token/core/src/lib.rs`, shortened:

```rust
/// External messages.
pub enum Message {
    /// Create a fungible token. …
    NewDefinition { name: String, total_supply: u128, mint_authority: Option<AccountId>, holding: AccountId },
    /// Create a zero holding of `definition`. …
    InitializeHolding { definition: AccountId },
    /// Move `amount` from the receiving holding (authorized) to the holding at `to`. …
    Transfer { to: AccountId, amount: u128 },
    Burn { amount: u128 },
    Mint { definition: AccountId, to: AccountId, amount: u128 },
    SetAuthority { definition: AccountId, new_authority: Option<AccountId> },
    ReadState,
}

/// What a token actor holds.
pub enum TokenState {
    Definition(TokenDefinition),
    Holding(TokenHolding),
}
```

and events in `src/event.rs`, each `impl TokenEvent for Transferred { const NAME: &str = "Transferred"; }`.

The guest decodes `Message` with Borsh, so the variant order is the wire format: the index is
the tag byte.

## 3. Generate the IDL

```bash
spel generate-idl programs/token/core --message Message --state TokenState --events event \
  > artifacts/token-idl.json
```

No annotations are needed: the generator reads the named enum, state type and events module,
plus every type they reference (following path dependencies), and takes doc comments as help
text. A name that doesn't exist fails the run, so a rename can't silently drop a type. The
format is described in [reference/idl.md](reference/idl.md).

The IDL already drives help:

```bash
$ spel --idl artifacts/token-idl.json
…
MESSAGES:
  new-definition       --name <STRING> --total-supply <NUMBER> --mint-authority <OPT<ACCOUNT_ID>> --holding <ACCOUNT_ID>
  initialize-holding   --definition <ACCOUNT_ID>
  transfer             --to <ACCOUNT_ID> --amount <NUMBER>
  …
$ spel --idl artifacts/token-idl.json -- transfer --help
```

and encoding, with no chain involved:

```bash
$ spel --idl artifacts/token-idl.json encode burn --amount 20
0314000000000000000000000000000000
```

## 4. Deploy

```bash
TOKEN=$(spel deploy target/guest/token.bin --fee-payer $PAYER)
echo $TOKEN
```

`spel deploy` creates a header account and the segment accounts the binary needs, uploads it
through LEZ's program loader, and prints the program's address: the header account. That
address, not the ImageID (`spel program-id`), is how the program is named from now on. Every
deploy gets a new one.

## 5. Name things: `spel.toml` and wallet labels

A `spel.toml` maps a name to the deployment and its IDL:

```toml
[programs.token]
address = "<the address from step 4>"
idl = "artifacts/token-idl.json"
```

Accounts can carry wallet labels:

```bash
for name in usdc alice bob; do wallet account new public --label $name; done
```

Now `--program token` selects the program and its IDL, `alice@token` names alice's token actor,
and labels work anywhere an account is expected, including message fields.

## 6. Send messages

Create a token whose definition lives on `usdc`, with the supply minted to `alice`:

```bash
spel --program token --fee-payer $PAYER send --root usdc --signer usdc --signer alice -- \
  new-definition --name USDC --total-supply 1000 --mint-authority usdc --holding alice
```

Before sending, SPEL prints the transaction:

```text
new-definition (tag 0, 90 bytes) to (usdc (2hiv…), token)
Access list:
  (usdc (2hiv…), token)    root, signs
  (alice (J3q2…), token)   field `holding`, signs
usdc (2hiv…) signs. In this transaction that authorizes:
  (usdc (2hiv…), token)    root
alice (J3q2…) signs. In this transaction that authorizes:
  (alice (J3q2…), token)   reachable: any program in the conversation can send it owner messages
```

- `--root` is the actor that receives the message.
- The **access list** is every actor the transaction may touch; LEZ refuses messages to anything
  undeclared. SPEL added alice because `holding` is an account field; `mint_authority` names
  usdc, which is already the root.
- **Signers** are explicit. A signature authorizes every actor of that account in the
  transaction, which is why SPEL lists them.

After inclusion it prints what changed:

```text
Included in block 352. Changed:
  (usdc (2hiv…), token)
    before: none
    after:  {"Definition":{"authority":"2hiv…","name":"USDC","total_supply":"1000"}}
  (alice (J3q2…), token)
    before: none
    after:  {"Holding":{"balance":"1000","definition_id":"2hiv…"}}
```

Give bob a holding, then transfer and mint to it:

```bash
spel --program token send --root bob --signer bob -- initialize-holding --definition usdc
spel --program token send --root alice --signer alice -- transfer --to bob --amount 300
spel --program token send --root usdc --signer usdc -- mint --definition usdc --to bob --amount 50
```

`--dry-run` prints the transaction without sending it; `--dry-run=json` does so as JSON.

## 7. Read state

```bash
$ spel inspect alice@token
{
  "Holding": {
    "balance": "700",
    "definition_id": "2hivK471mcXpSnfRiKDhKjEKCqsD4wQ88BpBLoZyy3Xn"
  }
}
```

An account holds one state per program. Without a program, `inspect` lists them:

```bash
$ spel inspect alice
{
  "token": "01194a…"
}
```

Wide integers are strings in the JSON so they survive any JSON reader.

## 8. When a message is refused

```bash
$ spel --program token send --root alice --signer alice -- transfer --to bob --amount 5000
…
⚠️  Included in block 362, but no declared actor changed: the program probably refused the message (LEZ reports no outcome), or it is a no-op.
$ echo $?
2
```

LEZ includes a refused transaction in a block and changes nothing, without reporting why. SPEL
compares every declared actor before and after: exit `0` means something changed, `2` means
nothing did, `1` means it wasn't sent or included. Scripts should check the exit code, not
inclusion.

## 9. Actors the defaults miss

`burn` reaches the holding's definition, but the definition isn't a message field, so SPEL
can't know to declare it:

```bash
$ spel --program token send --root bob --signer bob -- burn --amount 20
⚠️  Included in block 364, but no declared actor changed: …
```

Declare it with `--actor`:

```bash
$ spel --program token send --root bob --signer bob --actor usdc -- burn --amount 20
Included in block 366. Changed:
  (bob (rdey…), token)       … "balance":"330" …
  (usdc (2hiv…), token)      … "total_supply":"1030" …
```

`--actor` also declares actors on other programs: `--actor vault@token` while sending to an AMM.
`--no-auto-actors` turns the field-based defaults off entirely.

---

Next: the [CLI reference](reference/cli.md) for every command and flag.
