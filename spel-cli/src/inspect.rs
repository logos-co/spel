//! Binary inspection — extract ProgramId from ELF binaries.

use crate::hex::hex_encode;
use nssa::program::Program;
use std::fs;

/// Extract and print ProgramIds from one or more ELF binary files.
///
/// `format`:
/// - `None` / `"text"` — human-readable multi-line output (default)
/// - `"hex"` — one 64-char ImageID hex string per file (machine-readable; useful for
///   `PROGRAM_ID=$(spel program-id prog.bin --format hex)`)
/// - `"json"` — JSON object per file with `path`, `program_id_decimal`,
///   `program_id_hex`, and `image_id_hex` fields
pub fn inspect_binaries(paths: &[String], format: Option<&str>) {
    if paths.is_empty() {
        eprintln!("Usage: spel program-id <FILE> [FILE...]");
        eprintln!("  Prints the ProgramId ([u32; 8]) for each ELF binary.");
        eprintln!();
        eprintln!("Options:");
        eprintln!("  --format <fmt>   Output format: text (default) | hex | json");
        std::process::exit(1);
    }
    let fmt = format.unwrap_or("text");
    if !matches!(fmt, "text" | "hex" | "json") {
        eprintln!(
            "❌ --format: unknown format '{}'. Expected: text, hex, json",
            fmt
        );
        std::process::exit(1);
    }
    if fmt == "json" && paths.len() > 1 {
        print!("[");
    }
    let mut first = true;
    for path in paths {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("❌ {}: {}", path, e);
                continue;
            },
        };
        match Program::new(bytes.into()) {
            Ok(program) => {
                let id = program.id();
                let id_bytes: Vec<u8> = id.iter().flat_map(|w| w.to_le_bytes()).collect();
                let image_id_hex = hex_encode(&id_bytes);
                match fmt {
                    "hex" => println!("{}", image_id_hex),
                    "json" => {
                        let id_strs: Vec<String> = id.iter().map(|w| w.to_string()).collect();
                        let id_hex: Vec<String> = id.iter().map(|w| format!("{:08x}", w)).collect();
                        if paths.len() > 1 {
                            if !first {
                                print!(",");
                            }
                            println!();
                        }
                        let obj = serde_json::json!({
                            "path": path,
                            "program_id_decimal": id_strs.join(","),
                            "program_id_hex": id_hex.join(","),
                            "image_id_hex": image_id_hex,
                        });
                        print!("{}", obj);
                        first = false;
                    },
                    _ => {
                        let id_strs: Vec<String> = id.iter().map(|w| w.to_string()).collect();
                        let id_hex: Vec<String> = id.iter().map(|w| format!("{:08x}", w)).collect();
                        println!("📦 {}", path);
                        println!("   ProgramId (decimal): {}", id_strs.join(","));
                        println!("   ProgramId (hex):     {}", id_hex.join(","));
                        println!("   ImageID (hex bytes): {}", image_id_hex);
                        println!();
                    },
                }
            },
            Err(e) => {
                eprintln!("❌ {}: failed to load as program: {:?}", path, e);
            },
        }
    }
    if fmt == "json" && paths.len() > 1 {
        println!("\n]");
    }
}
