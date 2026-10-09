# Extensions — quick reference

Full reference: [`docs/reference/extensions.md`](../../../docs/reference/extensions.md).
Design rationale: [`docs/extension-trust-model.md`](../../../docs/extension-trust-model.md).

## Using one

Two things, both required, neither sufficient alone:

```toml
# methods/guest/Cargo.toml
pause-ext = { path = "../../pause_ext" }
```

```rust
#[lez_program]
#[pause_ext]                    // marker — must be BELOW #[lez_program]
mod my_program {
    #[instruction]
    #[require_not_paused]       // gate — injects pause_config into this instruction
    pub fn do_something(/* … */) -> SpelResult { /* … */ }
}
```

A marker above `#[lez_program]` is never seen (attributes above expand first);
the framework detects this and fails with a message naming the attribute.

A marker with no matching dependency is a compile error — *"refusing to compile
a program that could be silently missing its extension surface"*. A dependency
with no marker is inert.

## Writing one

Declare it in the extension crate's own `Cargo.toml`:

```toml
[package.metadata.spel]
extension_attr = "mini_ext"

[[package.metadata.spel.inject]]
wrapper = "require_mini"

  [[package.metadata.spel.inject.account]]
  name = "mini_config"
  seed = [{ const = "mini_config" }]

[package.metadata.spel.embedded]
state_type = "mini_ext::MiniConfig"
```

Four requirements. The first three stop the build:

1. **A `proc-macro = true` sub-crate** exporting the marker (pass-through) and
   any gate attributes — plus `pub use spel_framework::instruction;` in the
   library for instruction functions. Since #276 the framework's `#[instruction]`
   strips `#[account(…)]` param attrs outside `#[lez_program]`; against an older
   framework this fails with `cannot find attribute 'account' in this scope`.
2. **`extern crate self as <crate>;`** — the framework emits cross-crate calls
   using absolute `::<crate>::Type` paths.
3. **Explicit `(Account, AutoClaim)` tuples** — `execute`'s rewrite only runs
   inside `#[lez_program]`. The vec is homogeneous: one claimed account turns
   every entry into a tuple, `AutoClaim::None` for the rest.

The fourth fails **quietly**:

4. **`#[account_type]` on the state struct.** Without it the type's shape never
   reaches the consumer's IDL, so the generated FFI cannot decode the injected
   account. Everything still builds; the generated QML is byte-identical. It
   surfaces as raw bytes in a running UI.

## Downstream

Extension instructions are parsed by the same code as native ones, so they get
IDL entries, FFI entry points, PDA helpers and generated-UI forms exactly like
native instructions. Injected accounts are ordinary IDL accounts.

Instruction names must be unique across the consumer and all active extensions.

## Traps

- A path-dependency extension must live **inside** the consumer's project tree —
  the guest build is docker-hermetic and a path containing `..` that escapes the
  context fails with `failed to load manifest`. Git deps are fine.
- `generate_idl!` discards discovery warnings; the CLI prints them.
