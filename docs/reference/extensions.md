# Extensions

An extension is an ordinary Rust crate that a program depends on and activates
with one attribute. Once active it can contribute whole instructions to the
consumer's dispatcher, inject accounts into instructions the consumer marks,
and wrap every instruction with a runtime check.

For the security reasoning behind the design — what an extension can and cannot
do, and why activation is explicit — see
[the extension trust model](../extension-trust-model.md).

---

## Activating one

Two things, both on the consumer side. A dependency:

```toml
# methods/guest/Cargo.toml
pause-ext = { path = "../../pause_ext" }
```

and the extension's marker attribute, **below** `#[lez_program]`:

```rust
#[lez_program]
#[pause_ext]                    // marker: activates discovery
mod my_program {
    use pause_ext::require_not_paused;

    #[instruction]
    #[require_not_paused]       // gate: pause_config is injected here
    pub fn do_something(/* … */) -> SpelResult { /* … */ }
}
```

Order matters. Attributes above `#[lez_program]` are consumed before it runs,
so a marker placed above it is never seen; the framework detects this case and
fails with a message naming the attribute rather than silently ignoring it.

Both halves are required, and neither alone does anything. A marker with no
matching dependency is an error, not a warning:

```text
marker(s) ["pause_ext"] matched no discoverable extension and dependency
resolution failed: … refusing to compile a program that could be silently
missing its extension surface.
```

A dependency with no marker is inert — depending on a crate never activates it.

---

## Declaring an extension

Extensions are discovered by reading `[package.metadata.spel]` from the
dependency's own `Cargo.toml`. A crate without that table is an ordinary
dependency.

```toml
[package]
name = "mini-ext"

[package.metadata.spel]
extension_attr = "mini_ext"          # the marker attribute consumers write

[[package.metadata.spel.inject]]
wrapper = "require_mini"             # the gate attribute this block applies to

  [[package.metadata.spel.inject.account]]
  name = "mini_config"
  seed = [{ const = "mini_config" }]

[package.metadata.spel.embedded]
state_type = "mini_ext::MiniConfig"
```

| Key | Meaning |
|---|---|
| `extension_attr` | Marker attribute name. The crate must export a proc-macro with this name. |
| `[[inject]]` | One block per gate attribute. `wrapper` names the attribute; each `[[inject.account]]` is an account added to every instruction carrying it. |
| `[[inject.account]]` | `name` is the parameter name; `seed` is the PDA seed list, in the same form as `#[account(pda = …)]`. |
| `embedded.state_type` | Absolute path to the extension's state type. |
| `[wrap_instructions]` | Optional. Applies `wrapper` (a **qualified** path, e.g. `my_ext_macros::gate`) to **every** instruction the dispatcher ships — the consumer's own and other extensions' alike. `self_exempt_marker` gives consumers a per-function opt-out word; `exempt` carves out instructions by qualified name, for discovered functions that have no source site to annotate. |

Malformed metadata is a compile error, not a skipped extension.

---

## Writing one

Four requirements. The first three stop the build; the fourth does not, which
makes it the one to watch.

### 1. A proc-macro sub-crate

Proc-macro attributes must live in a `proc-macro = true` crate, which cannot
export anything else, so an extension is normally two crates: the runtime
library and its macros. The library re-exports the macros so consumers declare
one dependency.

The macros crate must export the marker attribute (pass-through — the framework
matches it by name and never expands it) and any gate attributes. It must also
export an `#[instruction]` shim that strips `#[account(…)]` parameter
attributes, because the framework's own `#[instruction]` is a bare pass-through
that leaves them for rustc:

```text
error: cannot find attribute `account` in this scope
```

The framework reads those attributes out of the source file during the
dependency scan, so they have to survive in the text while being removed from
what rustc sees. See [issue #271](https://github.com/logos-co/spel/issues/271)
for the proposal to remove this boilerplate.

### 2. `extern crate self as <crate>;`

```rust
extern crate self as pause_ext;
```

The framework emits cross-crate calls into the consumer's binary using absolute
paths like `::pause_ext::PauseConfig`, so that path has to resolve both in the
library's own compile and in the consumer's.

### 3. Explicit `(Account, AutoClaim)` tuples

`SpelOutput::execute(vec![acct, …])` works inside `#[lez_program]` because the
macro rewrites it to `execute_with_claims`, deriving each claim from the
account's `#[account(…)]` constraints. That rewrite does not run in an
extension crate, so claims are written by hand:

```rust
SpelOutput::execute_with_claims(
    &[config.account, caller.account],
    &[AutoClaim::ClaimedIfDefault(Claim::Authorized), AutoClaim::None],
    vec![],
)
```

The vec is homogeneous: if any account is claimed, every entry becomes a tuple,
with `AutoClaim::None` for the rest. Mixing shapes fails with
`expected Account, found (Account, AutoClaim)`.

Getting claims wrong surfaces at execution, not compile time — `#[account(init, …)]`
on a setter gives `AccountAlreadyInitialized` on the second call, and claiming
an already-owned account gives `ClaimedNonDefaultAccount`.

### 4. `#[account_type]` on the state struct

```rust
#[account_type]
#[derive(Debug, Clone, BorshSerialize, BorshDeserialize, Default)]
pub struct PauseConfig {
    pub paused: bool,
}
```

This is what puts the type's shape into the **consumer's** IDL, and that IDL is
embedded into the generated FFI for `decode_account` to read at runtime.

Without it everything still builds and runs. Instructions are contributed, the
injected account is wired up, `spel-client-gen` emits the FFI entry points and
PDA helpers, and the generated QML is byte-identical. The only difference is
the IDL string inside `<program>_ffi.rs` — so a generated UI fetches the
injected account and then cannot decode it, showing raw bytes at runtime rather
than failing at build time.

The framework scans dependency crates for `#[account_type]` items, so nothing
is needed on the consumer side once the attribute is present.

---

## What the consumer gets

Extension instructions go through the same `parse_instruction()` as native
ones, so everything downstream treats them identically. For a program with one
native instruction, one gated instruction and an extension contributing two:

```text
initialize:  accounts=[counter(pda), owner(signer)]
bump:        accounts=[pause_config(pda), counter(pda), owner(signer)]   ← injected
init_pause:  accounts=[pause_config(pda), caller(signer)]                ← contributed
set_paused:  accounts=[pause_config(pda), caller(signer)]                ← contributed
```

All four appear in the IDL, get FFI entry points
(`consumer_init_pause`, `consumer_set_paused`), get PDA helpers
(`compute_pause_config_pda`), and get forms in a generated Logos module UI.
Injected accounts are ordinary IDL accounts — injection happens before the
instruction is parsed.

Both IDL producers agree: `generate_idl!` and `spel generate-idl` emit identical
instruction lists, identical injected-account ordering, and identical account
types. (They differ on two empty fields — `errors` and `types` serialize as
`null` from the CLI and `[]` from the macro — but that predates extensions and
shows up on programs that use none.)

Instruction names must be unique across the consumer and every active
extension; a collision is a compile error naming both sources.

---

## Gotchas

- **Path-dependency extensions must live inside the consumer's project tree.**
  The guest build is docker-hermetic with the project directory as its context,
  so a path containing `..` that escapes it fails with `failed to load manifest`.
  Git dependencies are unaffected.
- **`generate_idl!` discards discovery warnings.** The CLI prints them; the
  macro path passes a no-op sink, so environmental problems that downgrade to
  warnings are silent during a normal `make idl`.
- **Two extensions injecting into the same instruction** is supported — see
  `tests/e2e/overlapping_windows_program`.
