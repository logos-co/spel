# spel-admin-authority

Admin authority library for LEZ programs — RFP-001 implementation.

Provides a standardised access control primitive for SPEL programs where
privileged functions can only be called by a designated admin authority.
The authority can transfer control to a new key or permanently revoke it.

## Quick Start

Add to your program's `Cargo.toml`:

```toml
[dependencies]
spel-admin-authority = { path = "../spel-admin-authority" }
```

## Usage

### 1. Store AdminConfig in your config PDA

```rust
use spel_admin_authority::AdminConfig;

// In your initialize instruction:
let admin_key = *admin.account_id.value();
let config = AdminConfig::new(admin_key);
let data = borsh::to_vec(&config).unwrap();
```

### 2. Gate privileged instructions with #[require_admin]

```rust
use spel_framework::prelude::*;

#[lez_program]
mod my_program {
    #[instruction]
    pub fn initialize(
        #[account(init, pda = literal("config"))]
        config: AccountWithMetadata,
        #[account(signer)]
        admin: AccountWithMetadata,
    ) -> SpelResult {
        // Store AdminConfig in config PDA
        Ok(SpelOutput::execute(vec![config, admin], vec![]))
    }

    /// Admin-only: update config value.
    /// The #[require_admin] annotation injects the authority check automatically.
    #[instruction]
    #[require_admin(config)]
    pub fn set_value(
        #[account(mut, pda = literal("config"))]
        config: AccountWithMetadata,
        #[account(signer)]
        admin: AccountWithMetadata,
        new_value: u64,
    ) -> SpelResult {
        // Admin check injected automatically — no boilerplate needed
        Ok(SpelOutput::execute(vec![config, admin], vec![]))
    }

    /// Transfer admin authority to a new key.
    #[instruction]
    pub fn transfer_admin(
        #[account(mut, pda = literal("config"))]
        config: AccountWithMetadata,
        #[account(signer)]
        admin: AccountWithMetadata,
        new_admin: [u8; 32],
    ) -> SpelResult {
        let mut state = AdminConfig::try_from_slice(&config.account.data).unwrap();
        let admin_key = *admin.account_id.value();
        state.admin_state.transfer_admin(&admin_key, new_admin).unwrap();
        Ok(SpelOutput::execute(vec![config, admin], vec![]))
    }

    /// Permanently revoke admin authority — irreversible.
    #[instruction]
    pub fn revoke_admin(
        #[account(mut, pda = literal("config"))]
        config: AccountWithMetadata,
        #[account(signer)]
        admin: AccountWithMetadata,
    ) -> SpelResult {
        let mut state = AdminConfig::try_from_slice(&config.account.data).unwrap();
        let admin_key = *admin.account_id.value();
        state.admin_state.revoke_admin(&admin_key).unwrap();
        Ok(SpelOutput::execute(vec![config, admin], vec![]))
    }
}
```

## API

### `AdminState`

Core authority primitive. Store inside your program's config account.

```rust
pub struct AdminState {
    pub admin: Option<[u8; 32]>,
}

impl AdminState {
    pub fn new(admin_key: [u8; 32]) -> Self;
    pub fn is_revoked(&self) -> bool;
    pub fn assert_admin(&self, signer_key: &[u8; 32]) -> Result<(), AdminError>;
    pub fn transfer_admin(&mut self, signer_key: &[u8; 32], new_admin: [u8; 32]) -> Result<(), AdminError>;
    pub fn revoke_admin(&mut self, signer_key: &[u8; 32]) -> Result<(), AdminError>;
}
```

### `AdminConfig`

Standard config PDA type bundling `AdminState` with a `u64` config value.
Use this directly or embed `AdminState` in your own config struct.

```rust
pub struct AdminConfig {
    pub admin_state: AdminState,
    pub config_value: u64,
}
```

### `#[require_admin(config)]` macro

Add to any `#[instruction]` function to automatically inject an admin
authority check before the handler body runs. The argument names the
account parameter that holds the `AdminConfig` PDA.

```rust
#[instruction]
#[require_admin(config)]    // ← single annotation, no boilerplate
pub fn privileged_action(
    #[account(mut, pda = literal("config"))]
    config: AccountWithMetadata,
    #[account(signer)]
    admin: AccountWithMetadata,
) -> SpelResult { ... }
```

The macro expands to verify `AdminConfig::assert_admin()` against the
signer before the handler body executes. An unauthorized call returns
`SpelError::Unauthorized` and the transaction is rejected.

## Error Codes

| Error | Meaning |
|---|---|
| `AdminError::Unauthorized` | Signer is not the current admin authority |
| `AdminError::Revoked` | Admin authority has been permanently revoked |
| `AdminError::AlreadyRevoked` | Cannot revoke — already revoked |

## Authority Lifecycle
initialize(admin_key)
│
▼
AdminState { admin: Some(key) }
│
├── assert_admin(key) ──► Ok — privileged call allowed
├── assert_admin(other) ─► Err(Unauthorized)
│
├── transfer_admin(key, new_key)
│       └──► AdminState { admin: Some(new_key) }
│
└── revoke_admin(key)
└──► AdminState { admin: None } (permanent)
│
└── assert_admin(any) ──► Err(Revoked)

## Atomicity

All mutations in `AdminState` only modify state after all checks pass.
An unauthorized call returns `Err` before any write — the prior authority
is preserved on failure. This is enforced structurally, not by convention.

## Tests

```bash
cargo test -p spel-admin-authority    # 9 unit tests
cargo test -p spel-admin-authority-sample  # 8 integration tests
```

## Reference Implementation

See `spel-admin-authority-sample/` for a complete SPEL program demonstrating
all four instructions: `initialize`, `set_config_value`, `transfer_admin`,
and `revoke_admin`.

## Related

- [RFP-001 proposal](https://github.com/logos-co/rfp/issues/55)
- [LP-0013 implementation](https://github.com/bristinWild/logos-execution-zone)
  — production use of `AdminState` pattern for token mint authority
