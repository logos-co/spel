//! Shared admin-authority helpers used by the proc-macros and the runtime
//! IDL generator. Gated behind `idl-gen` because the IDL generator is gated
//! there; the macros opt into the same feature to reuse these helpers.

use syn::{Attribute, ItemFn};

pub fn has_admin_authority_attr(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| a.path().is_ident("admin_authority"))
}

pub fn has_require_admin_attr(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| a.path().is_ident("require_admin"))
}

pub fn admin_instruction_fns() -> Vec<ItemFn> {
    vec![
        syn::parse_quote! {
            #[instruction]
            pub fn admin_initialize(
                #[account(init, pda = literal("admin_config"))] mut config: AccountWithMetadata,
                #[account(signer)] caller: AccountWithMetadata,
                new_admin_account: AccountWithMetadata,
                new_admin: ::admin_authority::AdminCandidate,
            ) -> SpelResult {
                let resolved = new_admin.validate_with_account(&new_admin_account)
                    .map_err(|e| SpelError::Unauthorized { message: e.to_string()})?;
                let state = ::admin_authority::AdminConfig::initialize(resolved)
                    .map_err(|e| SpelError::Unauthorized { message: e.to_string()})?;
                let bytes = state.encode()
                    .map_err(|e| SpelError::Unauthorized { message: e.to_string()})?;
                config.account.data = bytes.try_into()
                    .map_err(|_| SpelError::SerializationError {
                        message: "AdminConfig too large for account data".to_string(),
                    })?;
                Ok(SpelOutput::execute(vec![config, caller, new_admin_account], vec![]))
            }
        },
        syn::parse_quote! {
            #[instruction]
            pub fn admin_transfer(
                #[account(mut, pda = literal("admin_config"))] mut config: AccountWithMetadata,
                #[account(signer)] caller: AccountWithMetadata,
                new_admin_account: AccountWithMetadata,
                new_admin: ::admin_authority::AdminCandidate,
            ) -> SpelResult {
                let mut state = ::admin_authority::AdminConfig::from_account(&config)
                    .map_err(|e| SpelError::Unauthorized { message: e.to_string()})?;
                state.transfer(&caller, new_admin, &new_admin_account)
                    .map_err(|e| SpelError::Unauthorized { message: e.to_string()})?;
                let bytes = state.encode()
                    .map_err(|e| SpelError::Unauthorized { message: e.to_string()})?;
                config.account.data = bytes.try_into()
                    .map_err(|_| SpelError::SerializationError {
                        message: "AdminConfig too large for account data".to_string(),
                    })?;
                Ok(SpelOutput::execute(vec![config, caller, new_admin_account], vec![]))
            }
        },
        syn::parse_quote! {
            #[instruction]
            pub fn admin_renounce(
                #[account(mut, pda = literal("admin_config"))] mut config: AccountWithMetadata,
                #[account(signer)] caller: AccountWithMetadata,
            ) -> SpelResult {
                let mut state = ::admin_authority::AdminConfig::from_account(&config)
                    .map_err(|e| SpelError::Unauthorized { message: e.to_string()})?;
                state.renounce(&caller)
                    .map_err(|e| SpelError::Unauthorized { message: e.to_string()})?;
                let bytes = state.encode()
                    .map_err(|e| SpelError::Unauthorized { message: e.to_string()})?;
                config.account.data = bytes.try_into()
                    .map_err(|_| SpelError::SerializationError {
                        message: "AdminConfig too large for account data".to_string(),
                    })?;
                Ok(SpelOutput::execute(vec![config, caller], vec![]))
            }
        },
    ]
}