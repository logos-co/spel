# Pause extension — a worked example

A minimal but complete SPEL extension: a marker attr, a gate attr, two contributed
instructions and one injected account. Built while reviewing the extension
mechanism, and verified end to end against a live LEZ v0.2.4 sequencer:

| step | result |
|---|---|
| `init_pause` (extension instruction) | confirmed |
| `do_something` while unpaused | confirmed |
| `set_paused true` (extension instruction) | confirmed |
| `do_something` while **paused** | rejected — `Program error 1001: program is paused` |
| `set_paused false` | confirmed |
| `do_something` again | confirmed |

The gate reads the injected `pause_config` account, enforces state at execution
time, and reopens when the flag clears.

## Activating it

Two consumer actions, per the trust model:

```toml
# consumer's methods/guest/Cargo.toml
pause-ext = { path = "../../pause_ext" }
```

```rust
#[lez_program]
#[pause_ext]                       // marker: activates discovery
mod my_program {
    use pause_ext::{pause_ext, require_not_paused};

    #[instruction]
    #[require_not_paused]          // gate: pause_config is injected here
    pub fn do_something(/* ... */) -> SpelResult { /* ... */ }
}
```

## Three things that are easy to miss

These are load-bearing. Without them the extension crate does not compile, and
none of them are currently in the README:

1. **Ship your own `#[instruction]` proc-macro** that strips `#[account(...)]`
   param attrs (see `pause_ext_macros`). The framework's source scanner reads the
   raw file and still sees the attrs; the attribute only exists so the extension
   crate itself compiles.
2. **`extern crate self as pause_ext;`** — required for the absolute self-paths
   (`::pause_ext::PauseConfig`) that get copied into consumer-side codegen.
3. **Write `(Account, AutoClaim)` tuples explicitly.** The `ExecuteTransformer`
   rewrite only runs inside `#[lez_program]`, so the in-program
   `SpelOutput::execute(vec![acct, ...])` idiom does not compile here.

Claim semantics are hand-written for the same reason, and are easy to get wrong.
Two mistakes made while writing this example, both caught by LEZ at execution:

- `#[account(init, ...)]` on a setter — the second call fails with
  `AccountAlreadyInitialized`. Only the initializer should use `init`.
- claiming an already-initialised account — fails with `ClaimedNonDefaultAccount`.
  A `mut` instruction on an account the program already owns must emit
  `AutoClaim::None`.

## A fourth one that fails quietly

The three above stop the build. This one does not, which makes it worse.

**Mark the extension's state struct `#[account_type]`.** It is what puts the
type's shape into the *consumer's* IDL, and that IDL is embedded into the
generated FFI and read back by `decode_account` at runtime.

Leave it off and everything still works: the instructions are contributed, the
injected account is wired into the gated instruction, `spel-client-gen` emits
the FFI entry points and PDA helpers, and the generated QML is **byte-identical**
— 1548 lines either way. The only difference is the IDL string inside
`<program>_ffi.rs`. So the account viewer in the generated UI can fetch the
injected account but cannot decode its contents, and it shows up as raw bytes
at runtime rather than as a build error.

Verified by generating both ways from the same consumer:

```
# without #[account_type]
IDL account types: ['Counter']
# with
IDL account types: ['Counter', 'PauseConfig']
    PauseConfig fields: [('paused', 'bool')]
```

The framework finds it on its own once the attribute is there — dependency
crates are scanned for `#[account_type]` items, which is what
`spel-framework-core/src/{dep_walk,account_types}.rs` are for. Nothing else is
needed on the consumer side.

## Layout note

The guest build is docker-hermetic, so a path-dependency extension must live
inside the consumer's project tree; a path pointing outside it fails with
`failed to load manifest`.
