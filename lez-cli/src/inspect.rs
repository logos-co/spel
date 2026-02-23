//! Binary inspection — extract ProgramId from ELF binaries.

use nssa::program::Program;
use crate::hex::hex_encode;
use std::fs;

/// Inspect one or more ELF binary files and print their ProgramIds.
pub fn inspect_binaries(paths: &[String]) {
    if paths.is_empty() {
        eprintln!("Usage: lez-cli inspect <FILE> [FILE...]");
        eprintln!("  Prints the ProgramId ([u32; 8]) for each ELF binary.");
        std::process::exit(1);
    }
    for path in paths {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("❌ {}: {}", path, e);
                continue;
            }
        };
        match Program::new(bytes) {
            Ok(program) => {
                let id = program.id();
                let id_strs: Vec<String> = id.iter().map(|w| w.to_string()).collect();
                let id_hex: Vec<String> = id.iter().map(|w| format!("{:08x}", w)).collect();
                println!("📦 {}", path);
                println!("   ProgramId (decimal): {}", id_strs.join(","));
                println!("   ProgramId (hex):     {}", id_hex.join(","));
                let id_bytes: Vec<u8> = id.iter().flat_map(|w| w.to_le_bytes()).collect();
                println!("   ImageID (hex bytes): {}", hex_encode(&id_bytes));
                println!();
            }
            Err(e) => {
                eprintln!("❌ {}: failed to load as program: {:?}", path, e);
            }
        }
    }
}
