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
  "state": { "defined": "TokenState" },
  "events": [ … ],
  "types": [ … ]
}
```

| field | meaning |
| --- | --- |
| `version` | always `"2"` |
| `name` | the program's name and event namespace (`token` for `token_core`) |
| `messages` | the external message enum's variants, in enum order |
| `state` | the type of an actor's state; omitted if not given to the generator |
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

## `state`

The type an actor of the program holds, usually an enum over the actor kinds:

```json
"state": { "defined": "TokenState" }
```

`spel inspect` decodes actor state as this type unless `--type` names another.

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
crate's source with `syn`; the program carries no annotations. It reads:

- **the message enum** (`--message`): variants in order, their fields and doc comments. A
  program's internal message enum (messages only its own actors send) is left out: nobody
  outside sends it.
- **the state type** (`--state`).
- **the events** (`--events <module>`): every `impl <Trait> for <Struct> { const NAME: &str = "…"; }`
  in that module, in source order. The trait's name doesn't matter.
- **every type those reference**, across the crate's path dependencies (`Cargo.toml`
  `path = …` entries, transitively). The crate's own definitions win over a dependency's.

Type rules: `String` → `string`, `AccountId` → `account_id`, `ProgramId` → `program_id`;
`Box<T>` is transparent; type aliases resolve to their target; array lengths must be integer
literals. Items under `#[cfg(test)]` or `#[cfg(feature = "…")]` are skipped, so test-only
types never reach an IDL.

It fails, naming the cause, when:

- the message enum, state type or events module doesn't exist (a rename breaks generation
  instead of silently dropping a type);
- `--message` names a struct, or an enum with more than 256 variants;
- the events module has no event impls;
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
  "state": { "defined": "TokenState" },
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
