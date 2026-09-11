//! Minimal embeddable extension for the two-carrier fixture: one gate
//! inject spec, no instructions, no initializer wrapper.

pub use mini_ext_macros::{mini_ext, require_mini};

/// The embedded window's state type named by `embedded.state_type`.
/// This fixture has a single embed, so no collision assert ever
/// references it, but embedded-mode discovery requires the declaration.
pub struct MiniConfig;
