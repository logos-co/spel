//! CLI tool for generating client/FFI code from SPEL program IDL.
//!
//! Usage:
//!   spel-client-gen --idl path/to/idl.json --out-dir generated/

use spel_client_gen::pascal_case;
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
    let mut target: Option<String> = None;
    let mut module_name: Option<String> = None;
    let mut ffi_lib_path: Option<String> = None;
    let mut skip_ui = false;

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
            "--target" => {
                target = Some(args.get(i + 1).ok_or("--target requires value")?.clone());
                i += 2;
            }
            "--module-name" => {
                module_name = Some(args.get(i + 1).ok_or("--module-name requires value")?.clone());
                i += 2;
            }
            "--ffi-lib-path" => {
                ffi_lib_path = Some(args.get(i + 1).ok_or("--ffi-lib-path requires value")?.clone());
                i += 2;
            }
            "--skip-ui" => {
                skip_ui = true;
                i += 1;
            }
            "--help" | "-h" => {
                println!("spel-client-gen - Generate typed Rust client and C FFI from SPEL IDL");
                println!();
                println!("Usage:");
                println!("  spel-client-gen --idl <path> --out-dir <dir> [--target <target>] [--module-name <name>] [--ffi-lib-path <path>]");
                println!();
                println!("Options:");
                println!("  --idl <path>           Path to IDL JSON file");
                println!("  --out-dir <dir>        Output directory for generated files");
                println!("  --target <target>      Output target: rust+ffi (default) | logos-module");
                println!("  --module-name <name>   Override class/file name for logos-module target");
                println!("  --ffi-lib-path <path>  Path to compiled FFI .so, relative to --out-dir");
                println!("                         (logos-module only). Wires up CMakeLists.txt automatically.");
                println!("  --skip-ui              Skip qml/Main.qml — preserves hand-written QML while still");
                println!("                         regenerating the C++ backend, plugin, and build files.");
                return Ok(());
            }
            other => return Err(format!("unknown argument: {other}").into()),
        }
    }

    let idl_path = idl_path.ok_or("missing --idl")?;
    let out_dir = out_dir.ok_or("missing --out-dir")?;

    let json = std::fs::read_to_string(&idl_path)
        .map_err(|e| format!("failed to read {}: {}", idl_path.display(), e))?;

    let program_name = {
        let idl: serde_json::Value = serde_json::from_str(&json)?;
        idl["name"].as_str().unwrap_or("program").to_string()
    };
    let prog = program_name.replace('-', "_");

    match target.as_deref() {
        Some("logos-module") => {
            let output = spel_client_gen::generate_logos_module_from_idl_json(
                &json,
                module_name.as_deref(),
                ffi_lib_path.as_deref(),
            )?;

            std::fs::create_dir_all(out_dir.join("src"))
                .map_err(|e| format!("failed to create src dir: {e}"))?;
            std::fs::create_dir_all(out_dir.join("qml"))
                .map_err(|e| format!("failed to create qml dir: {e}"))?;

            // Use --module-name if given, otherwise fall back to IDL name.
            let effective = module_name.as_deref().unwrap_or(&prog);
            let class = pascal_case(effective);

            let files: &[(&str, &str)] = &[
                (&format!("src/{}Backend.h", class),   &output.backend_h),
                (&format!("src/{}Backend.cpp", class), &output.backend_cpp),
                (&format!("src/{}Plugin.h", class),    &output.plugin_h),
                (&format!("src/{}Plugin.cpp", class),  &output.plugin_cpp),
                ("src/main.cpp",                        &output.main_cpp),
                ("qml/Main.qml",                        &output.main_qml),
                ("module.yaml",                         &output.module_yaml),
                ("manifest.json",                        &output.manifest_json),
                ("CMakeLists.txt",                      &output.cmake_lists),
            ];

            println!("Generated (--target logos-module):");
            for (rel, content) in files {
                if skip_ui && *rel == "qml/Main.qml" {
                    println!("  (skipped) qml/Main.qml  [--skip-ui]");
                    continue;
                }
                let path = out_dir.join(rel);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&path, content)?;
                println!("  {}", path.display());
            }
        }
        None | Some("rust+ffi") | Some("default") => {
            let output = spel_client_gen::generate_from_idl_json(&json)?;

            std::fs::create_dir_all(&out_dir)
                .map_err(|e| format!("failed to create {}: {}", out_dir.display(), e))?;

            let client_path = out_dir.join(format!("{prog}_client.rs"));
            let ffi_path    = out_dir.join(format!("{prog}_ffi.rs"));
            let header_path = out_dir.join(format!("{prog}.h"));

            std::fs::write(&client_path, &output.client_code)?;
            std::fs::write(&ffi_path,    &output.ffi_code)?;
            std::fs::write(&header_path, &output.header)?;

            println!("Generated:");
            println!("  Client: {}", client_path.display());
            println!("  FFI:    {}", ffi_path.display());
            println!("  Header: {}", header_path.display());
        }
        Some(other) => {
            return Err(format!("unknown target: {other} (known: rust+ffi, logos-module)").into());
        }
    }

    Ok(())
}

