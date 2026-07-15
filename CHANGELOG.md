## v0.6.0 (2026-07-15)

### 💥 Breaking Changes

- **Upgrade to LEZ v0.2.0.** The Logos Execution Zone dependency moves to `v0.2.0`: the
  `nssa` / `nssa_core` crates are now `lee` / `lee_core`, and public transactions are signed
  over the `/LEE/v0.3` preimage the live testnet accepts. The wallet-home environment
  variable is **renamed `NSSA_WALLET_HOME_DIR` → `LEE_WALLET_HOME_DIR`** — update any
  scripts, CI, or FFI callers. (#238)

### ✨ Features

- **`Vec<String>` instruction arguments** — repeat a flag (`--tag a --tag b`) to build a
  `Vec<String>` argument for an instruction. (#189)

### 🐛 Fixes

- **LEZ v0.2.0 compatibility.** SPEL now builds against LEZ v0.2.0 and its public
  transactions are accepted by the live testnet — fixing both the `nssa_core`-not-found
  build break and the `InvalidSignature` rejection on public transactions. Verified
  end-to-end against `https://testnet.lez.logos.co/`. (#238; closes #234, #237, #247)
- **Deterministic IDL output.** Account helper types are emitted in a stable, name-sorted
  order, so the generated IDL is byte-identical across runs (previously randomized by
  `HashSet` iteration order — a source of spurious diffs and flaky committed-IDL checks).
  (#232)
- **Hardened ProgramId parsing.** `parse_program_id` now rejects `Public/`/`Private/`
  account ids and non-canonical base58 rather than silently reinterpreting them as a
  ProgramId, so a mistyped or wrong-encoding value errors instead of building a transaction
  against the wrong program. (#250; closes #243)

### ✅ Also resolved (fixed by the LEZ v0.2.0 upgrade)

- Wallet storage schema now matches the `wallet` CLI — SPEL reads `storage.json` created by
  the wallet without a deserialization error. (closes #235)
- Guest builds no longer pull `bonsai-sdk → reqwest → rustls → ring`; riscv32
  cross-compilation of programs using `spel-framework` is clean. (closes #165)

---

## v0.5.0 (2026-06-01)

### ✨ Features

- **`spel program-id`**: rename `inspect <FILE>` to `program-id` for clarity (1dc4dd0)
- **Rest-arg matching**: extend suffix matching beyond `_accounts`; derive rest-account args by name (#211, 276ae37, 9215bd2)
- **`--format hex|json`**: add output format flag to `decode`; unify `parse_bytes32` / `compute_pda_raw` (1dd5a0e)
- **`decode` feature gate**: make the `decode` module opt-in behind a Cargo feature; `base58` dep is only pulled in when enabled (#215, 2209515)
- **Workspace lint policy**: add rustfmt baseline, `clippy.toml`, and a `quality.yml` CI gate with per-crate lint configuration (dd4e505)

### 🐛 Fixes

- Fix `--format` flag missing from `program-id` subcommand after rename (ae84174)

### 📦 Maintenance

- Pin `logos-blockchain-circuits` setup script to a known-good commit in all CI workflows (25cd1f8)
- Retag LEZ dependency from `v0.2.0-rc3` to the equivalent published release `v0.1.2` (84f6cea)
- Normalize indentation in `logos_module_codegen.rs` (#216)

# Changelog

## v0.4.0 (2026-05-22)

### ✨ Features

**`spel-client-gen`: `--target logos-module` — Qt/QML Basecamp module scaffold from IDL (#209)**

Adds a new code generation target that emits a complete, compilable Qt/QML
Basecamp plugin directly from a SPEL IDL. Run once with `make ui-gen`,
customise the generated QML, then use `make ui-regen` on subsequent IDL
changes to preserve hand-written UI while regenerating the C++ backend.

Generated output (9 files):

- `src/{Class}Backend.h/cpp` — `QObject` with `Q_INVOKABLE` per instruction and `Q_PROPERTY` per state account; async FFI dispatch via `QFutureWatcher + QThreadPool`
- `src/{Class}Plugin.h/cpp` — Basecamp `IComponent` plugin with `Q_INIT_RESOURCE` for embedded QML
- `src/main.cpp` — standalone preview app entry point
- `qml/Main.qml` — sidebar + `StackLayout` UI: ACCOUNTS / INSTRUCTIONS / WALLET / SETTINGS sections
- `module.yaml` + `manifest.json` — logos-module-builder and Basecamp runtime metadata
- `CMakeLists.txt` — Qt6 CMake build wiring FFI `.so` and plugin + preview app targets

Key capabilities:

- **QSettings persistence** — `walletPath`, `sequencerUrl`, `programIdHex` persist across restarts; priority chain: QSettings → env var → compiled-in FFI constant (`{module}_program_id()`) → default
- **Account picker dropdown** — account-typed instruction fields show a RECENT (field history) + WALLET (live accounts) picker; non-account fields get a RECENT-only history dropdown
- **Field history** — per-field input history backed by `QSettings`, capped at 10 entries, deduplicated on save
- **TxPoller confirmation** — FFI waits for block inclusion via `TxPoller` before returning success; busy indicator stays active until then
- **`[u8; 32]` arg unification** — accepts base58 (`Public/`/`Private/` prefix), hex (`0x` prefix), or raw hex for any 32-byte instruction argument
- **`--module-name`** — overrides class/file/env-var names independently of the IDL `name` field (e.g. `--module-name lez_multisig` from a `multisig_program` IDL)
- **`--skip-ui`** — skips `qml/Main.qml` on re-generation; `make ui-regen` uses this automatically
- **`--ffi-lib-path`** — auto-wires `CMakeLists.txt` to the compiled FFI `.so`
- **Wallet page** — connection ping, account listing, account creation, on-demand Borsh account inspector
- **E2E test** — `e2e_logos_module_codegen` added to the framework test suite

**`spel pda`: resolve account seeds from CLI args (#194)**

`spel pda` now resolves seed values that reference instruction arguments
directly from the CLI, removing the need to pre-compute seed bytes manually.

**`generate_idl!`: scan path-dependency crates + qualified attribute form (#180)**

`#[account_type]` structs defined in path-dependency crates (common in
multi-crate workspaces) are now picked up by the macro. The qualified form
`#[spel_framework::account_type]` is also recognised.

### 🐛 Fixes

- Suppress spurious `r0vm` ImageID error in `make build` (#205)
- LEZ compat workflow: fix Cargo.lock extraction and sed escaping (#201)
- `spel init` E2E test: use `--owner` flag (was `--account`) (#206)
- Validity window test coverage and doc improvements (#203)

### 🧪 Tests & Docs

- Integration test for macro validity window pass-through (#202)
- Init E2E CI test exercising `spel init` with default flags (#185)
- README: troubleshooting section for `ring`/`riscv32` guest build failure (#181)

---

## v0.3.0 (2026-05-13)

### ✨ Features
expose execution context to instruction handlers (issue #172) (#182) (c4b7b0b)
extend SPEL macros to support private PDAs (#171) (529bf2a)
re-export nssa_core/nssa types from spel-framework prelude (#153) (e7135b8)
generate C FFI fetch functions + CI workflow cleanup (#156) (cd9d81a)
generate C FFI fetch functions for PDA account types (#154) (3cd5102)
support LEZ validity windows in program output (#139) (9e7f275)
--dry-run with full tx summary and JSON output (1dc31bf)
show seed inputs in PDA derivation output (665c5b8)
SpelOutput::execute() with auto-claim support (#126) (2384881)
add #[account_type] annotation for IDL-driven account inspection (#106) (62f91e2)

### 🐛 Fixes
exclude ProgramContext from runtime-generated IDL (#191) (582b452)
use branch=main for spel-framework default (issue #183) (#184) (ea2f998)
API stability for SpelOutput (issue #158) (#177) (ba6e87d)
harden path-dep account_type scanning (issue #173) (#175) (338129a)
collect #[account_type] types from path-dependency crates (#169) (577b802)
wrap generated extern "C" FFI functions with catch_unwind (#150) (5e943cc)
collect #[account_type] structs defined inside #[lez_program] module (#162) (d4e34f0)
unify IDL generation paths to include #[account_type] annotated types (#146) (82204ab)
strip Public/Private prefix in generated parse_account_id (#149) (3242f4a)
map lowercase 'string' IDL type to Rust 'String' (#148) (40dc8ed)
resolve config paths relative to spel.toml and clean up post-merge issues (fce4a0c)
clean up serializer after risc0 serde refactor (f36cfdf)
separate CLI flags from instruction args to avoid parsing conflicts (67af1e6)
parse PDA seed args through IDL type system (#129) (9005e9f)

## v0.2.0 (2026-04-01)

### 📦 Other
- fix(release): create issue with PR link instead of PR directly (#100) (8a67c6b)
- fix(release): delete stale remote branch before push (#99) (dc933d9)
- fix(release): fix broken YAML in gh pr create body (#98) (05ec85b)
- fix(release): use gh pr create instead of peter-evans action (#97) (93c9aff)
- fix(release): add logos-blockchain-circuits to release workflow (#96) (d3ccd60)
- ci(release): PR-based flow with categorized changelog (#95) (8f059c2)
- feat(spel-cli): detect Private/ prefix, build PrivacyPreservingTransaction (#92) (57201f6)
- feat: update to latest LEZ (ffcbc159) and fix spel-client-gen API (3621a26)
- rename: lez-* crates to spel-*, binary as spel (fixes #57) (034a39b)
- fix(e2e): update instruction count after adding PDA fixtures (600ea8a)
- test(fixture): add arg and multi-seed PDA examples to fixture program (9d2cd3c)
- fix(client-gen): use lez_framework_core::pda::compute_pda for correct PDA derivation (eb05263)
- feat(client-gen): generate PDA compute and state query helpers (2785438)
- fix(init): extract project name from path to support absolute paths (68e5f6a)
- feat(lez-cli): add `generate-idl` subcommand for runtime IDL generation (f4370bf)
- fix(cli)!: remove `-account` suffix (021041d)
- fix(init): fix scaffolded projects failing cargo risczero build (#73) (54fc4f4)
- feat: expose generic compute_pda() utility in lez-framework-core (bebe8c2)
- chore: add PR template with README checklist (b488a91)
- chore: add MIT and Apache-2.0 license files (aa7d5a1)
- chore: add PR template with README checklist (6dd72f6)
- feat: add `inspect` subcommand for account data decoding (#60) (c117260)
- chore: add PR template with README checklist (7cd8189)
- docs: add pda subcommand, Vec types, and --program-id flag to README (976d103)
- chore: update URLs for logos-co org transfer (3276fa8)
- docs: fix SPEL acronym — Smart Program Engine for Logos (233a066)
- docs: rename to SPEL, update README with acronym and ecosystem table (eefd20d)
