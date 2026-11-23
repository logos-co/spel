//! Inert markers that `spel generate-idl` reads from a program's core crate.
//!
//! The markers change nothing about the code they're on: each returns its item unchanged. They
//! exist so the IDL generator, which reads the crate's source with `syn`, can find what the
//! types alone don't say, without the program naming it on the command line or in `spel.toml`.
//!
//! ```ignore
//! use spel_attrs::state;
//!
//! #[state]
//! pub enum TokenState {
//!     Definition(TokenDefinition),
//!     Holding(TokenHolding),
//! }
//! ```

use proc_macro::{Delimiter, TokenStream, TokenTree};

/// Marks the type of an actor's state, so `spel generate-idl` lists it in the IDL's
/// `state_types` without a `--state` flag.
///
/// Mark one type, usually an enum over the program's actor kinds. Mark several only when the
/// program stores different untagged structs on different actors; a reader then has to pick
/// the type for each actor.
///
/// It takes no arguments and goes on a struct or an enum; anything else is a compile error, so
/// a misplaced marker can't silently drop a type from the IDL.
#[proc_macro_attribute]
pub fn state(attr: TokenStream, item: TokenStream) -> TokenStream {
    if !attr.is_empty() {
        return compile_error("#[state] takes no arguments");
    }
    if !is_struct_or_enum(&item) {
        return compile_error("#[state] goes on a struct or an enum");
    }
    item
}

/// Whether `item` declares a struct or an enum: the first `struct` or `enum` keyword outside
/// attributes and visibility, before any other item keyword.
fn is_struct_or_enum(item: &TokenStream) -> bool {
    for token in item.clone() {
        match token {
            TokenTree::Ident(ident) => match ident.to_string().as_str() {
                "struct" | "enum" => return true,
                // Visibility (`pub`, `pub(crate)`) comes before the keyword.
                "pub" | "crate" | "super" | "self" | "in" => {},
                _ => return false,
            },
            // `#`, the attribute's `[...]`, and a visibility's `(...)`.
            TokenTree::Punct(_) => {},
            TokenTree::Group(group)
                if matches!(group.delimiter(), Delimiter::Bracket | Delimiter::Parenthesis) => {},
            _ => return false,
        }
    }
    false
}

fn compile_error(message: &str) -> TokenStream {
    format!("::core::compile_error!({message:?});")
        .parse()
        .unwrap_or_default()
}
