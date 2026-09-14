# Extension discovery trust model

Status: accepted with PR #233. Records the decisions behind who can contribute instructions to a consumer's dispatcher and what happens when the mechanism fails.

## Problem

Extension discovery matches a metadata-declared attribute name (`extension_attr`) against attributes on the consumer's `#[lez_program]` module. Attribute names are plain strings: not namespaced, not bound to any crate, claimable by anyone. An early iteration walked the full transitive path-dependency graph, so any crate anywhere in the tree could declare `extension_attr = "admin_authority"` and have its `#[instruction]` fns merged into a consumer that had opted into the real admin-authority. The generated call path also derived from the dependency's directory name, which breaks for renamed checkouts, vendored copies, and `package =` renamed dependencies, and silently mislabels which crate a call resolves to.

## Decision

Activating an extension requires two explicit consumer actions, both in files the consumer authors:

1. The extension is listed in the consumer's own `[dependencies]`. Discovery walks direct dependencies only, never transitively.
2. The extension's declared marker attr appears on the consumer's `#[lez_program]` module.

Crate identity comes from the dependency's `[package].name`, never from its directory name. Generated cross-crate call paths use that name.

This is the same trust boundary cargo itself provides. A crate the consumer explicitly named can already run arbitrary code at build time (proc macros, build scripts), so extension discovery grants it nothing it did not have. What the direct-only rule removes is the ability of unchosen transitive crates to reach the dispatcher.

## Failure posture

Discovery failures split into two tiers:

- **Malformed metadata is a hard compile error.** A `[package.metadata.spel]` block that exists but has the wrong shape (non-string `extension_attr`, non-array `instruction_attrs`) fails the build in every producer. A broken extension declaration must never degrade into a program that builds, deploys, and silently lacks its extension surface, which for a security library means shipping ungated.
- **Environmental problems warn.** Unreadable manifests, path dependencies pointing at missing directories, and a matched extension that contributes neither instructions nor gate attrs are reported through the discovery `on_warning` channel, surfaced by the CLI. These indicate setup problems rather than broken declarations, and cargo itself fails the build for the fatal subset.

Duplicate instruction names across user fns and extensions (or two extensions) are a compile error naming both sources. Undetected they become colliding enum variants and IDL discriminators, or one instruction silently shadowing another.

## Consequences

- A consumer cannot be extended by anything they did not name in their own manifest. Reviewing a program's extension surface means reading its Cargo.toml and its module attrs, nothing else.
- Extensions must be direct dependencies. A meta-crate re-exporting a bundle of extensions does not activate them; consumers list each one.
- Registry and git dependencies are currently out of discovery scope (path dependencies only). When that widens, the same two-action rule and package-name identity apply to the new transports unchanged.
- Considered and rejected: an explicit allow-list on the module (`#[lez_program(extensions = [...])]`) duplicates information the dependency list already carries; binding to name-plus-version adds friction without adding trust, since the consumer's lockfile already pins versions.
