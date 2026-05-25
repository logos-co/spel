# spel-freeze-authority

Freeze authority library for LEZ programs — RFP-002 implementation.

Provides a standardised emergency circuit breaker for SPEL programs.
A designated freeze authority can disable all program interactions or
freeze specific accounts, while an admin authority controls who holds
the freeze authority.

## Authority Hierarchy
AdminAuthority (spel-admin-authority)
└── FreezeAuthority (spel-freeze-authority)
├── freeze_program() / unfreeze_program()
├── freeze_account(account_id)
└── unfreeze_account(account_id)

## Quick Start

```toml
[dependencies]
spel-freeze-authority = { path = "../spel-freeze-authority" }
spel-admin-authority = { path = "../spel-admin-authority" }
```

## Usage

### Gate interactions with check_interaction()

```rust
use spel_freeze_authority::FreezeState;

// In any instruction that should respect the freeze:
let state = FreezeState::try_from_slice(&config.account.data)?;
let user_key = *user.account_id.value();
state.check_interaction(&user_key)?;  // Err if program or account frozen
```

### Full lifecycle

```rust
// Initialize with freeze authority
let freeze = FreezeState::new(freeze_authority_key);

// Freeze entire program (freeze authority only)
freeze.freeze_program(&freeze_key)?;

// Freeze specific account (freeze authority only)
freeze.freeze_account(&freeze_key, target_account_id)?;

// Unfreeze
freeze.unfreeze_program(&freeze_key)?;
freeze.unfreeze_account(&freeze_key, target_account_id)?;

// Rotate freeze authority (admin only)
freeze.set_freeze_authority(&admin_key, Some(new_freeze_key))?;

// Revoke freeze authority permanently (admin only)
freeze.set_freeze_authority(&admin_key, None)?;
```

## API

### `FreezeState`

```rust
pub struct FreezeState {
    pub freeze_authority: AdminState,  // None = permanently revoked
    pub program_frozen: bool,
    pub frozen_accounts: Vec<[u8; 32]>,
}

impl FreezeState {
    pub fn new(freeze_key: [u8; 32]) -> Self;
    pub fn is_revoked(&self) -> bool;
    pub fn is_account_frozen(&self, account_id: &[u8; 32]) -> bool;
    pub fn check_interaction(&self, account_id: &[u8; 32]) -> Result<(), FreezeError>;
    pub fn freeze_program(&mut self, signer: &[u8; 32]) -> Result<(), FreezeError>;
    pub fn unfreeze_program(&mut self, signer: &[u8; 32]) -> Result<(), FreezeError>;
    pub fn freeze_account(&mut self, signer: &[u8; 32], account: [u8; 32]) -> Result<(), FreezeError>;
    pub fn unfreeze_account(&mut self, signer: &[u8; 32], account: [u8; 32]) -> Result<(), FreezeError>;
    pub fn set_freeze_authority(&mut self, admin: &[u8; 32], new: Option<[u8; 32]>) -> Result<(), FreezeError>;
}
```

## Error Codes

| Error | Meaning |
|---|---|
| `FreezeError::Unauthorized` | Signer is not the freeze/admin authority |
| `FreezeError::Revoked` | Freeze authority permanently revoked |
| `FreezeError::ProgramFrozen` | Program is frozen — all interactions blocked |
| `FreezeError::AccountFrozen` | Specific account is frozen |
| `FreezeError::AlreadyRevoked` | Cannot revoke — already revoked |

## Tests

```bash
cargo test -p spel-freeze-authority         # 11 unit tests
cargo test -p spel-freeze-authority-sample  # 9 integration tests
```

## Related

- [RFP-002 proposal](https://github.com/logos-co/rfp/issues/56)
- [spel-admin-authority](../spel-admin-authority/README.md) — admin authority library
