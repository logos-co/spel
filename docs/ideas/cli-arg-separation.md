# CLI Argument Separation: `spel.toml` + `--` Separator

## Problem Statement
**How might we** eliminate namespace collisions between spel CLI flags and IDL-driven instruction arguments, while keeping the daily UX clean for both newcomers and power users?

## Recommended Direction

Two complementary changes:

### 1. `spel.toml` — project config for per-project defaults

A config file at the project root holds values that don't change between invocations:

```toml
[program]
idl = "treasury-idl.json"
binary = "methods/guest/target/riscv32im-risc0-zkvm-elf/docker/treasury.bin"
```

This removes `--idl` and `--program` from the command line entirely for the common case:

```bash
# Before (today):
spel --idl treasury-idl.json -p methods/guest/target/.../treasury.bin create-vault --owner-key 0xAB...

# After:
spel create-vault --owner-key 0xAB...
```

`spel.toml` is auto-discovered by walking up from CWD. `spel init` generates it alongside existing scaffolding.

### 2. `--` separator — for CLI overrides

When a user needs to override config values on the fly, the standard `--` separator marks the boundary:

```bash
spel --idl other.json -- create-vault --owner-key 0xAB...
```

Everything before `--` is for spel. Everything after is the instruction subcommand + its arguments. This is the standard Unix convention (`cargo run`, `docker exec`, `ssh`).

### Precedence

CLI flag (`--idl`) > `spel.toml` > built-in default (`program.bin`)

### Backward compatibility

Current flat parsing (no `--`, no `spel.toml`) remains as a **deprecated fallback**. When the parser detects ambiguous flag usage without `--`, it emits a deprecation warning directing users to either `spel.toml` or the `--` separator.

## Key Assumptions to Validate
- [ ] Users run spel from project root (where `spel.toml` lives) — validate by checking `spel init` project structure
- [ ] `idl` and `binary` are sufficient config fields for v1 — audit which flags are per-project vs. per-invocation
- [ ] Adding a `toml` dep to `spel-cli` is acceptable — check transitive deps and compile time impact
- [ ] Early adopters can migrate with deprecation warnings over ~2 releases

## MVP Scope

**In scope:**
- `spel.toml` with `[program]` section (`idl`, `binary` fields), auto-discovered by walking up from CWD
- `spel init` generates `spel.toml`
- `--` separator support in the parser (`lib.rs`)
- Deprecation warning when flat-parsing without `--` or config
- Updated help output showing both modes

**Out of scope (v1):**
- Per-environment config (dev/testnet/mainnet profiles)
- Config fields beyond `idl` and `binary`
- `--bin-<NAME>` in config

## Not Doing (and Why)
- **Per-environment profiles** — YAGNI; one `spel.toml` per project is enough. Add `[env.testnet]` later if demand arises.
- **Removing flat parsing entirely** — early adopters have scripts/Makefiles. Deprecation period first.
- **Auto-detecting IDL without config** — implicit magic creates UX confusion. Explicit config is better.
- **Positional instruction args** — less self-documenting, breaks help generation.
- **Prefix namespacing (`--arg:name`)** — non-standard, more typing, solves the same problem worse.

## Open Questions
- Should `spel init` update existing Makefile `ARGS=` patterns to reflect the new config-based workflow?
- Deprecation timeline: warn for how many releases before removing flat parsing?
- Should `spel.toml` also support `program-id` (hex) as an alternative to `binary` (path)?
