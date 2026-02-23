//! CLI tool for generating client/FFI code from LEZ program IDL.
//!
//! Usage:
//!   lez-client-gen --idl path/to/idl.json --out-dir generated/

use std::path::PathBuf;

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let mut idl_path: Option<PathBuf> = None;
    let mut out_dir: Option<PathBuf> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--idl" => {
                idl_path = Some(PathBuf::from(args.get(i + 1).ok_or("--idl requires value")?));
                i += 2;
            }
            "--out-dir" => {
                out_dir = Some(PathBuf::from(args.get(i + 1).ok_or("--out-dir requires value")?));
                i += 2;
            }
            "--help" | "-h" => {
                println!("lez-client-gen - Generate typed Rust client and C FFI from LEZ IDL");
                println!();
                println!("Usage:");
                println!("  lez-client-gen --idl <path> --out-dir <dir>");
                println!();
                println!("Options:");
                println!("  --idl <path>     Path to IDL JSON file");
                println!("  --out-dir <dir>  Output directory for generated files");
                return Ok(());
            }
            other => return Err(format!("unknown argument: {other}").into()),
        }
    }

    let idl_path = idl_path.ok_or("missing --idl")?;
    let out_dir = out_dir.ok_or("missing --out-dir")?;

    let json = std::fs::read_to_string(&idl_path)
        .map_err(|e| format!("failed to read {}: {}", idl_path.display(), e))?;

    let output = lez_client_gen::generate_from_idl_json(&json)?;

    std::fs::create_dir_all(&out_dir)
        .map_err(|e| format!("failed to create {}: {}", out_dir.display(), e))?;

    let program_name = {
        let idl: serde_json::Value = serde_json::from_str(&json)?;
        idl["name"].as_str().unwrap_or("program").to_string()
    };

    let client_path = out_dir.join(format!("{}_client.rs", program_name.replace('-', "_")));
    let ffi_path = out_dir.join(format!("{}_ffi.rs", program_name.replace('-', "_")));
    let header_path = out_dir.join(format!("{}.h", program_name.replace('-', "_")));

    std::fs::write(&client_path, &output.client_code)?;
    std::fs::write(&ffi_path, &output.ffi_code)?;
    std::fs::write(&header_path, &output.header)?;

    println!("Generated:");
    println!("  Client: {}", client_path.display());
    println!("  FFI:    {}", ffi_path.display());
    println!("  Header: {}", header_path.display());

    Ok(())
}
