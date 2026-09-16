use proc_macro::TokenStream;
use quote::quote;

/// Module marker. Per the README contract 3, it must be a real proc-macro that
/// expands to nothing — the framework strips nothing.
#[proc_macro_attribute]
pub fn pause_ext(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Gate: refuse to run while the embedded flag is set. Expands on the emitted
/// handler after the #[lez_program] rewrite.
#[proc_macro_attribute]
pub fn require_not_paused(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut func = syn::parse_macro_input!(item as syn::ItemFn);
    let body = &func.block;
    let guard: syn::Block = syn::parse_quote!({
        {
            let __cfg = ::pause_ext::PauseConfig::read(&pause_config)?;
            if __cfg.paused {
                return Err(::spel_framework::error::SpelError::custom(
                    1001,
                    "program is paused",
                ));
            }
        }
        #body
    });
    func.block = Box::new(guard);
    quote!(#func).into()
}
