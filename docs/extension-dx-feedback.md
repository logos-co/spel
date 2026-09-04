# PR #257 — extension mechanism: DX feedback

Written after building a working extension (a pause switch: marker attr, gate attr,
one contributed instruction, one injected account) from the README alone, then
activating it in a scaffolded consumer program.

## What works well

- **Discovery, merging and injection all function end to end.** The contributed
  instruction appears in the consumer's dispatcher and IDL, and the gated
  instruction gets the declared account injected and PDA-verified.
- **Both IDL producers agree.** `generate_idl!` and `spel generate-idl` emit the
  same instruction list and the same injected account order.
- **The marker-without-dependency error is excellent** — it names the marker,
  explains that dependency resolution failed, and states why it refuses to
  compile rather than silently shipping without the extension surface. This is
  the standard the other failure paths should meet.
- **The trust model doc is genuinely good**: names the threat, states the
  decision, justifies it against cargo's own boundary, records rejected
  alternatives.

## Highest-value fixes

### 1. `generate_idl!` swallows discovery warnings (one line)

`spel-framework-macros/src/lib.rs:235` and `:2360` pass `&mut |_| {}`, while the
CLI passes `|w| eprintln!("⚠️  {w}")`. The trust model promises environmental
problems "are reported through the discovery `on_warning` channel, surfaced by
the CLI" — in the macro path they are discarded. When something is wrong with
discovery during a normal `make idl`, the author gets silence.

### 2. Guest and examples can drift to different framework versions

`spel init` emits independent framework pins for `methods/guest` and `examples`.
If they resolve to different versions, `make idl` produces an IDL **missing the
entire extension surface** with no error and no warning — and that IDL is what
gets committed and fed to client-gen. Either pin them in lockstep at scaffold
time, or have the IDL producer refuse when its framework version does not match
the guest's.

### 3. Three authoring requirements are undocumented

Each one stops an author cold; all three were only discoverable by reading
`mmlado/spel-admin-authority`:

- **You must ship your own `#[instruction]` proc-macro** that strips
  `#[account(...)]` param attrs, or the extension crate does not compile. It is
  ~8 lines of pure boilerplate every author must reinvent. Consider exporting it
  from the framework so extensions can re-export it.
- **`extern crate self as <crate_name>;` is required** for README contract 2
  ("reference your own types by absolute path") to resolve inside your own crate.
- **`SpelOutput::execute` needs explicit `(Account, AutoClaim)` tuples** in an
  extension crate — the `ExecuteTransformer` rewrite only runs inside
  `#[lez_program]`, so the in-program idiom does not compile.

### 4. No in-repo example of an instruction-contributing extension

`mini_ext` deliberately contributes none (it exists for the compile-failure
fixtures), so the only working reference is an external repository. A minimal
example in-tree — marker, gate, one instruction, one injected account — would
carry the three contracts above by demonstration.

### 5. Path-dep extensions must live inside the project tree

The guest build is docker-hermetic, so a path dependency outside the project
directory fails with `failed to load manifest`. Worth a sentence in the docs.
