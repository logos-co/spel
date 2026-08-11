//! The consumer's slot carrier, living in a shared path-dependency
//! crate instead of the program's entry file. This is the layout the
//! binding scan's path-dep arm exists for: a workspace that keeps its
//! account types in a `*_core` crate and its program in a thin bin.

use spel_framework::prelude::*;

#[account_type]
pub struct Cfg {
    pub v: u64,
    #[mini_slot]
    pub s: u8,
}
