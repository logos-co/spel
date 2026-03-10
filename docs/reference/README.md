# SPEL Framework Reference

Comprehensive API reference for the SPEL framework (`logos-co/spel`). This document covers every macro, type, CLI command, IDL schema, and code generation feature.

For a guided walkthrough, see the [Tutorial](../tutorial.md).

---

## Reference Pages

- [**Macros**](macros.md) — `#[lez_program]`, `#[instruction]`, `generate_idl!`, and generated validation functions
- [**Types**](types.md) — Framework types: `LezOutput`, `LezError`, `AccountConstraint`, `ChainedCall`, `PdaSeed`, and the prelude
- [**CLI**](cli.md) — All `lez-cli` commands (`init`, `inspect`, `idl`, `pda`, instruction execution) with flags, examples, and type format table
- [**IDL Format**](idl.md) — IDL JSON schema, instruction/account/arg definitions, discriminators, and lssa-lang compatibility fields
- [**Client Code Generation**](client-gen.md) — `lez-client-gen` CLI, library API, generated Rust client, C FFI wrappers, C header, and C++/Qt integration example
