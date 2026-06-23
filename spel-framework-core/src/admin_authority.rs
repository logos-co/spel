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
                let resolved = new_admin.validate_with_account(&new_admin_account)?;
                let state = ::admin_authority::AdminConfig::initialize(resolved)?;
                state.write_to(&mut config)?;
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
                let mut state = ::admin_authority::AdminConfig::from_account(&config)?;
                state.transfer(&caller, new_admin, &new_admin_account)?;
                state.write_to(&mut config)?;
                Ok(SpelOutput::execute(vec![config, caller, new_admin_account], vec![]))
            }
        },
        syn::parse_quote! {
            #[instruction]
            pub fn admin_renounce(
                #[account(mut, pda = literal("admin_config"))] mut config: AccountWithMetadata,
                #[account(signer)] caller: AccountWithMetadata,
            ) -> SpelResult {
                let mut state = ::admin_authority::AdminConfig::from_account(&config)?;
                state.renounce(&caller)?;
                state.write_to(&mut config)?;
                Ok(SpelOutput::execute(vec![config, caller], vec![]))
            }
        },
    ]
}