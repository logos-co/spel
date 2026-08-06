//! Pass-through attributes for the two-carrier fixture extension. The
//! framework detects the marker by name; the macros exist so rustc
//! accepts the attributes syntactically.

use proc_macro::TokenStream;

/// Marker attribute, pass-through.
#[proc_macro_attribute]
pub fn mini_ext(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Gate attribute, pass-through.
#[proc_macro_attribute]
pub fn require_mini(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}
