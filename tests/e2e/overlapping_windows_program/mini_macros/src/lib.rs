//! Pass-through attributes for the overlapping-windows fixture
//! extensions. The framework detects the markers by name; the macros
//! exist so rustc accepts the attributes syntactically.

use proc_macro::TokenStream;

/// Marker attribute of extension A, pass-through.
#[proc_macro_attribute]
pub fn mini_a(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Gate attribute of extension A, pass-through.
#[proc_macro_attribute]
pub fn require_a(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Marker attribute of extension B, pass-through.
#[proc_macro_attribute]
pub fn mini_b(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Gate attribute of extension B, pass-through.
#[proc_macro_attribute]
pub fn require_b(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}
