//! Inert markers that `spel generate-idl` reads from a program's core crate.
//!
//! The markers change nothing about the code they're on: each returns its item unchanged. They
//! exist so the IDL generator, which reads the crate's source with `syn`, can find what the
//! types alone don't say, without the program naming it on the command line or in `spel.toml`.
//!
//! ```ignore
//! use spel_attrs::{event, message, state};
//!
//! #[message]
//! pub enum Message {
//!     Transfer { to: AccountId, amount: u128 },
//! }
//!
//! #[state]
//! pub enum TokenState {
//!     Definition(TokenDefinition),
//!     Holding(TokenHolding),
//! }
//!
//! #[event]
//! pub trait TokenEvent: BorshSerialize {
//!     const NAME: &'static str;
//! }
//!
//! impl TokenEvent for Transferred {
//!     const NAME: &'static str = "Transferred";
//! }
//! ```

use proc_macro::{Delimiter, TokenStream, TokenTree};

/// Marks the program's external message enum, so `spel generate-idl` reads it without a
/// `--message` flag. Its variant order gives the messages' Borsh tags.
///
/// Mark exactly one enum per crate: the one others send. A program's internal message enum
/// (messages only its own actors send) stays unmarked.
///
/// It takes no arguments and goes on an enum; anything else is a compile error.
#[proc_macro_attribute]
pub fn message(attr: TokenStream, item: TokenStream) -> TokenStream {
    marker("message", &["enum"], &attr, item)
}

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
    marker("state", &["struct", "enum"], &attr, item)
}

/// Marks the trait a program's events implement, so `spel generate-idl` lists every impl of it
/// as an event without an `--events` flag. Each impl names its event with a string literal
/// `const NAME: &str`, which gives the event's selector; the implementing struct is its payload.
///
/// Mark the trait, not the event structs. Without a marked trait (or `--events`), the IDL has
/// no events. Marking several traits merges their events.
///
/// It takes no arguments and goes on a trait; anything else is a compile error.
#[proc_macro_attribute]
pub fn event(attr: TokenStream, item: TokenStream) -> TokenStream {
    marker("event", &["trait"], &attr, item)
}

/// `item` unchanged if the marker has no arguments and `item` is one of `kinds`; else a
/// compile error.
fn marker(name: &str, kinds: &[&str], attr: &TokenStream, item: TokenStream) -> TokenStream {
    if !attr.is_empty() {
        return compile_error(&format!("#[{name}] takes no arguments"));
    }
    if !item_kind(&item).is_some_and(|kind| kinds.contains(&kind.as_str())) {
        return compile_error(&format!("#[{name}] goes on {}", describe(kinds)));
    }
    item
}

/// `"an enum"`, `"a struct or an enum"`, `"a trait"`.
fn describe(kinds: &[&str]) -> String {
    kinds
        .iter()
        .map(|kind| format!("{} {kind}", if kind.starts_with('e') { "an" } else { "a" }))
        .collect::<Vec<_>>()
        .join(" or ")
}

/// The item's keyword (`struct`, `enum`, `fn`, …): the first identifier outside attributes and
/// visibility.
fn item_kind(item: &TokenStream) -> Option<String> {
    for token in item.clone() {
        match token {
            TokenTree::Ident(ident) => match ident.to_string().as_str() {
                // Visibility (`pub`, `pub(crate)`) and `unsafe` come before the keyword.
                "pub" | "crate" | "super" | "self" | "in" | "unsafe" => {},
                keyword => return Some(keyword.to_owned()),
            },
            // `#`, the attribute's `[...]`, and a visibility's `(...)`.
            TokenTree::Punct(_) => {},
            TokenTree::Group(group)
                if matches!(
                    group.delimiter(),
                    Delimiter::Bracket | Delimiter::Parenthesis
                ) => {},
            _ => return None,
        }
    }
    None
}

fn compile_error(message: &str) -> TokenStream {
    format!("::core::compile_error!({message:?});")
        .parse()
        .unwrap_or_default()
}
