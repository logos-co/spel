//! Minimal embeddable extension B for the overlapping-windows fixture:
//! one gate inject spec, no instructions, a 16-byte window.

pub use mini_macros::{mini_b, require_b};

/// The embedded window's state type named by `embedded.state_type`.
pub struct BConfig;

impl spel_framework::FixedBorshSize for BConfig {
    const SIZE: usize = 16;
}
