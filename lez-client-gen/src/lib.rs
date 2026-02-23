//! # lez-client-gen
//!
//! Generates typed Rust client code and C FFI wrappers from LEZ program IDL JSON.
//!
//! ## Usage
//!
//! ```rust,ignore
//! use lez_client_gen::generate_from_idl_json;
//! use std::fs;
//!
//! let idl_json = fs::read_to_string("my_program_idl.json")?;
//! let output = generate_from_idl_json(&idl_json)?;
//! fs::write("src/generated_client.rs", &output.client_code)?;
//! fs::write("src/generated_ffi.rs", &output.ffi_code)?;
//! ```

use lez_framework_core::idl::*;

mod codegen;
mod ffi_codegen;
mod util;

#[cfg(test)]
mod tests;

/// Output of code generation.
#[derive(Debug, Clone)]
pub struct CodegenOutput {
    /// Typed Rust client module source code.
    pub client_code: String,
    /// C FFI wrapper source code.
    pub ffi_code: String,
    /// C header file content.
    pub header: String,
}

/// Generate client + FFI code from an IDL JSON string.
pub fn generate_from_idl_json(json: &str) -> Result<CodegenOutput, String> {
    let idl: LezIdl = serde_json::from_str(json)
        .map_err(|e| format!("failed to parse IDL JSON: {}", e))?;
    generate_from_idl(&idl)
}

/// Generate client + FFI code from a parsed IDL.
pub fn generate_from_idl(idl: &LezIdl) -> Result<CodegenOutput, String> {
    let client_code = codegen::generate_client(idl)?;
    let ffi_code = ffi_codegen::generate_ffi(idl)?;
    let header = ffi_codegen::generate_header(idl)?;
    Ok(CodegenOutput { client_code, ffi_code, header })
}
