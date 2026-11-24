# IDL Format (version 2)

An IDL describes what a client needs to talk to a program on LEZ's actor model: the messages
it accepts, the state an actor of it holds, and the events it emits. Who receives a message,
who signs and which actors a conversation reaches are the caller's business, so the IDL says
nothing about accounts.

Version 2 replaces the instruction-based version 1 of the retired `#[lez_program]` framework.
`spel` refuses a version 1 IDL with a message to regenerate it.

---

## Top level

```json
{
  "version": "2",
  "name": "token",
  "messages": [ … ],
  "state_types": ["TokenState"],
  "events": [ … ],
  "types": [ … ]
}
```

| field | meaning |
| --- | --- |
| `version` | always `"2"` |
| `name` | the program's name and event namespace (`token` for `token_core`) |
| `messages` | the external message enum's variants, in enum order |
| `state_types` | the types of an actor's state: usually one, several for untagged state types; omitted if none was given to the generator |
| `events` | the events the program emits; omitted if none |
| `types` | every struct and enum the messages, state and events reference, sorted by name |

## `messages`

One entry per variant of the message enum:

```json
{
  "name": "Transfer",
  "tag": 2,
  "docs": "Move `amount` from the receiving holding (authorized) to the holding at `to`,\nwhich must already hold the same token.",
  "fields": [
    { "name": "to", "type": "account_id" },
    { "name": "amount", "type": "u128" }
  ]
}
```

- `tag` is the variant's index: the leading Borsh byte of the encoded message. Because the tag
  is positional, message enums are append-only: inserting a variant changes the tag of every
  later one.
- `docs` comes from the variant's `///` comments; a field's `docs` from the field's.
- A unit variant has no `fields`. Tuple fields are named by position: `"0"`, `"1"`, ….
- The CLI names a message in kebab-case (`initialize-holding`); snake_case and the variant name
  also work.

The encoded message is the tag byte followed by each field's Borsh encoding in order, exactly
what `borsh::to_vec(&Message::…)` produces in the program's core crate.

## `state_types`

The types an actor of the program holds, as names of structs or enums in `types`. Usually
there is one, an enum over the actor kinds, and every actor decodes as it:

```json
"state_types": ["TokenState"]
```

`spel inspect` decodes actor state as this type unless `--type` names another.

A program can instead store different structs on different actors, with no tag saying which (a
straight port of `twap_oracle` keeps `PriceObservations`, `OraclePriceAccount` and
`CurrentTickAccount` apart). Its IDL lists all of them:

```json
"state_types": ["PriceObservations", "OraclePriceAccount", "CurrentTickAccount"]
```

The bytes don't say which type an actor holds, and two structs of the same size both decode
cleanly, so nothing guesses: with several state types, `spel inspect` prints the state as hex
and lists the types until `--type` picks one. Prefer one enum over the actor kinds where you
can; it costs one byte per actor and makes every actor decodable.

Early version 2 IDLs had a single `"state": { "defined": … }` field. `spel` refuses those with a
message to regenerate.

## `events`

```json
{ "name": "Transferred", "selector": "dc1cd384aa229a4b", "type": { "defined": "Transferred" } }
```

`selector` is `sha256("<name>::<event>")[..8]` in hex, with `<name>` the IDL's `name`: the LEZ
convention for event selectors. `type` is the event's payload.

## Types

A field type is one of:

| JSON | meaning | Borsh |
| --- | --- | --- |
| `"u8"` … `"u128"`, `"i8"` … `"i128"` | integers | little-endian |
| `"bool"` | | one byte |
| `"string"` | | `u32` length, UTF-8 bytes |
| `"account_id"` | `lee_core` `AccountId` | 32 raw bytes |
| `"program_id"` | `ProgramId` (`[u32; 8]`) | 32 bytes |
| `{ "vec": T }` | `Vec<T>` | `u32` length, elements |
| `{ "option": T }` | `Option<T>` | `0`, or `1` and the value |
| `{ "array": [T, N] }` | `[T; N]` | N elements, no length |
| `{ "defined": "Name" }` | a struct or enum in `types` | its fields; an enum's `u8` tag first |

`types` entries:

```json
{ "name": "TokenHolding", "kind": "struct",
  "fields": [ { "name": "definition_id", "type": "account_id" },
              { "name": "balance", "type": "u128" } ] }

{ "name": "TokenState", "kind": "enum",
  "variants": [ { "name": "Definition", "fields": [ { "name": "0", "type": { "defined": "TokenDefinition" } } ] },
                { "name": "Holding",    "fields": [ { "name": "0", "type": { "defined": "TokenHolding" } } ] } ] }
```

## Generation

```bash
spel generate-idl programs/token/core --message Message --state TokenState --events event
```

The generator (`spel_framework_core::idl_gen::generate_idl`, feature `idl-gen`) reads the
crate's source with `syn`; the program needs no annotations, though it can [mark](#markers)
what it would otherwise name. It reads:

- **the message enum** (`--message`, else the enum marked `#[message]`): variants in order,
  their fields and doc comments. A program's internal message enum (messages only its own
  actors send) is left out: nobody outside sends it.
- **the state types** (`--state`, repeatable, else the types marked `#[state]`), written to
  `state_types` in the order given, or for markers in declaration order (a module's items
  where its `mod` is declared).
- **the events** (`--events <module>`, else the impls of the trait marked `#[event]`): every
  `impl <Trait> for <Struct> { const NAME: &str = "…"; }` in that module (whatever the trait),
  or every impl of the marked trait anywhere in the crate, in declaration order. The impl's
  `NAME` names the event; the struct is its payload. With neither, the IDL has no events: the
  generator doesn't guess from `const NAME`.
- **every type those reference**, across the crate's path dependencies (`Cargo.toml`
  `path = …` entries, transitively). The crate's own definitions win over a dependency's.

### Markers

Instead of naming them, a crate can mark its message enum, state types and event trait with
the inert attributes of the `spel-attrs` crate:

```rust
use spel_attrs::{event, message, state};   // spel-attrs = { git = "…/spel" }, a build-time dependency only

#[message]
pub enum Message { Transfer { to: AccountId, amount: u128 }, Burn { amount: u128 } }

#[state]
pub enum TokenState { Definition(TokenDefinition), Holding(TokenHolding) }

#[event]
pub trait TokenEvent: BorshSerialize { const NAME: &'static str; }

impl TokenEvent for Transferred { const NAME: &'static str = "Transferred"; }
```

With all three, `spel generate-idl programs/token/core` needs no flags.

The markers change nothing about the item: they return it unchanged and are never linked into
the guest. They take no arguments; `#[message]` goes only on an enum, `#[state]` on a struct
or an enum, `#[event]` on a trait, and anything else fails to compile. The generator
recognizes each marker bare (`#[event]`) or qualified (`#[spel_attrs::event]`), not a renamed
import, and looks only in the crate itself. It matches an impl to a marked trait by the
trait's last path segment, so `impl event::TokenEvent for …` counts.

Names win over markers: a command-line flag first, then `spel.toml`'s `[idl-gen]`, then the
markers. When a name and the markers disagree, the generator uses the name and warns. Mark
one message enum: several fail generation. Several marked event traits are fine: their impls
merge.

Type rules: `String` → `string`, `AccountId` → `account_id`, `ProgramId` → `program_id`;
`Box<T>` is transparent; type aliases resolve to their target; array lengths must be integer
literals. Items under `#[cfg(test)]` or `#[cfg(feature = "…")]` are skipped, so test-only
types never reach an IDL.

It fails, naming the cause, when:

- the message enum, state type or events module doesn't exist (a rename breaks generation
  instead of silently dropping a type);
- no message enum is named or marked, or several enums are marked `#[message]`;
- `--message` names a struct, or an enum with more than 256 variants;
- the events module has no event impls, a marked event trait has no impls, or an impl of one
  has no string-literal `NAME`;
- two events share a name (their selectors would collide);
- a referenced type is defined neither in the crate nor in its path dependencies (the error
  names the field that uses it);
- a type can't be described (tuples, references, non-literal array lengths).

Output is deterministic: `types` is sorted by name and fields keep declaration order, so a
regenerated IDL diffs cleanly.

## Full example

The token program's IDL, abbreviated:

```json
{
  "version": "2",
  "name": "token",
  "messages": [
    { "name": "NewDefinition", "tag": 0, "docs": "Create a fungible token. …",
      "fields": [ { "name": "name", "type": "string" },
                  { "name": "total_supply", "type": "u128" },
                  { "name": "mint_authority", "type": { "option": "account_id" } },
                  { "name": "holding", "type": "account_id" } ] },
    { "name": "InitializeHolding", "tag": 1, "fields": [ { "name": "definition", "type": "account_id" } ] },
    { "name": "Transfer", "tag": 2, "fields": [ { "name": "to", "type": "account_id" },
                                                { "name": "amount", "type": "u128" } ] },
    { "name": "Burn", "tag": 3, "fields": [ { "name": "amount", "type": "u128" } ] },
    { "name": "ReadState", "tag": 6 }
  ],
  "state_types": ["TokenState"],
  "events": [
    { "name": "Transferred", "selector": "dc1cd384aa229a4b", "type": { "defined": "Transferred" } }
  ],
  "types": [
    { "name": "TokenDefinition", "kind": "struct", "fields": [ … ] },
    { "name": "TokenHolding", "kind": "struct", "fields": [ … ] },
    { "name": "TokenState", "kind": "enum", "variants": [ … ] },
    { "name": "Transferred", "kind": "struct", "fields": [ … ] }
  ]
}
```

The complete token and NFT IDLs are the golden files in
`spel-framework-core/tests/fixtures/golden/`.
