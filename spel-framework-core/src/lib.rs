//! # SPEL Framework Core
//!
//! The IDL types, the IDL generator and the helpers the SPEL CLI shares: PDA derivation and
//! IDL-driven decoding.
//!
//! Programs on LEZ's actor model are written against `lee_core` directly; the guest framework
//! (`spel-framework`, its macros and runtime types) is retired.

#[cfg(feature = "decode")]
pub mod decode;
pub mod idl;
pub mod pda;

#[cfg(feature = "idl-gen")]
pub mod account_types;
#[cfg(feature = "idl-gen")]
pub mod extension;
#[cfg(feature = "idl-gen")]
pub mod idl_gen;

#[cfg(feature = "idl-gen")]
pub mod dep_walk;

#[cfg(test)]
mod test_utils;
