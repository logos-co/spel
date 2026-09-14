//! Minimal embeddable extension A for the overlapping-windows fixture:
//! one gate inject spec, no instructions, a 16-byte window.

pub use mini_macros::{mini_a, require_a};

/// The embedded window's state type named by `embedded.state_type`.
pub struct AConfig;

impl spel_framework::FixedBorshSize for AConfig {
    const SIZE: usize = 16;
}
