//! # spel-client-gen
//!
//! Generates typed Rust client code and C FFI wrappers from SPEL program IDL JSON.
//!
//! ## Usage
//!
//! ```rust,ignore
//! use spel_client_gen::generate_from_idl_json;
//! use std::fs;
//!
//! let idl_json = fs::read_to_string("my_program_idl.json")?;
//! let output = generate_from_idl_json(&idl_json)?;
//! fs::write("src/generated_client.rs", &output.client_code)?;
//! fs::write("src/generated_ffi.rs", &output.ffi_code)?;
//! ```

use spel_framework_core::idl::*;

mod codegen;
mod ffi_codegen;
mod logos_module_codegen;
mod util;

#[cfg(test)]
mod tests;

pub use logos_module_codegen::LogosModuleOutput;
pub use util::pascal_case;

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
    let idl: SpelIdl = serde_json::from_str(json)
        .map_err(|e| format!("failed to parse IDL JSON: {}", e))?;
    let client_code = codegen::generate_client(&idl)?;
    let ffi_code = ffi_codegen::generate_ffi(&idl, json)?;
    let header = ffi_codegen::generate_header(&idl)?;
    Ok(CodegenOutput { client_code, ffi_code, header })
}

/// Generate a Logos Basecamp module scaffold from an IDL JSON string.
///
/// - `module_name` overrides the name derived from the IDL (e.g. `--module-name lez_multisig`).
/// - `ffi_lib_path` is the path to the compiled FFI `.so`, relative to the generated
///   `CMakeLists.txt` output directory. When provided, the CMakeLists.txt is emitted with a
///   proper IMPORTED target and RPATH so `make ui-build` links and runs without manual edits.
///   Example: `"../../target/debug/libmy_program_ffi.so"`.
pub fn generate_logos_module_from_idl_json(
    json: &str,
    module_name: Option<&str>,
    ffi_lib_path: Option<&str>,
) -> Result<LogosModuleOutput, String> {
    let idl: SpelIdl = serde_json::from_str(json)
        .map_err(|e| format!("failed to parse IDL JSON: {}", e))?;
    logos_module_codegen::generate_logos_module(&idl, module_name, ffi_lib_path)
}

/// Generate client + FFI code from a parsed IDL.
/// Use `generate_from_idl_json` if you have the raw JSON — it embeds the JSON
/// in the generated FFI for runtime decode support.
pub fn generate_from_idl(idl: &SpelIdl) -> Result<CodegenOutput, String> {
    let json = serde_json::to_string(idl).map_err(|e| format!("failed to serialise IDL: {}", e))?;
    let client_code = codegen::generate_client(idl)?;
    let ffi_code = ffi_codegen::generate_ffi(idl, &json)?;
    let header = ffi_codegen::generate_header(idl)?;
    Ok(CodegenOutput { client_code, ffi_code, header })
}
