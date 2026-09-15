//! Binary inspection — extract ProgramId from program binaries.

use crate::hex::hex_encode;
use nssa::program::Program;
use std::fs;

/// Magic bytes of a raw RISC-V ELF as produced by
/// `cargo build --target riscv32im-risc0-zkvm-elf` for a guest crate.
const ELF_MAGIC: &[u8] = b"\x7fELF";

/// Magic bytes of the `risc0_binfmt::ProgramBinary` wrapper the wallet and
/// sequencer consume — the format `spel program-id` actually requires. The
/// guest build emits it as the `.bin` artefact beside the raw ELF (for
/// example `.../riscv32im-risc0-zkvm-elf/docker/<name>.bin`).
const R0BF_MAGIC: &[u8] = b"R0BF";

/// A one-line diagnosis for the common wrong-input case: a raw guest ELF
/// where the R0BF-wrapped `.bin` is expected. Returns `None` for anything
/// else, leaving the generic load error unexplained.
fn raw_elf_hint(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(R0BF_MAGIC) {
        return None;
    }
    if bytes.starts_with(ELF_MAGIC) {
        Some(
            "this is a raw RISC-V ELF; spel program-id expects the R0BF \
             ProgramBinary wrapper — point it at the .bin artefact the guest \
             build emits beside the ELF (e.g. \
             .../riscv32im-risc0-zkvm-elf/docker/<name>.bin)",
        )
    } else {
        None
    }
}

/// Extract and print ProgramIds from one or more program binary files.
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
        eprintln!("  Prints the ProgramId ([u32; 8]) for each program binary (R0BF).");
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
        let looks_like_raw_elf = raw_elf_hint(&bytes);
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
                if let Some(hint) = looks_like_raw_elf {
                    eprintln!("   Hint: {}", hint);
                }
            },
        }
    }
    if fmt == "json" && paths.len() > 1 {
        println!("\n]");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A raw guest ELF — the exact wrong input from issue #240 — must be
    /// recognised and produce a hint naming the R0BF `.bin` artefact; the
    /// `Program::new` failure alone said only `Malformed ProgramBinary`.
    #[test]
    fn hints_on_raw_guest_elf() {
        let raw_elf = b"\x7fELF\x02\x01\x01\x00rest-of-guest";
        let hint = raw_elf_hint(raw_elf).expect("raw ELF must be diagnosed");
        assert!(hint.contains("raw RISC-V ELF"));
        assert!(hint.contains("R0BF"));
        assert!(hint.contains(".bin"));
    }

    /// An already-wrapped R0BF binary is the expected input: never hinted at
    /// (if it fails to decode, that is a different problem than #240).
    #[test]
    fn no_hint_for_r0bf_wrapped_binary() {
        assert!(raw_elf_hint(b"R0BF\x00\x00\x00\x01wrapped").is_none());
    }

    /// Anything else (truncated files, text, empty input) keeps the generic
    /// error without a speculative hint.
    #[test]
    fn no_hint_for_unrecognized_bytes() {
        assert!(raw_elf_hint(b"").is_none());
        assert!(raw_elf_hint(b"not a binary at all").is_none());
    }
}
