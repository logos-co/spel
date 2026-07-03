// On macOS, pyo3 (pulled in transitively via the LEZ `wallet` -> `keycard_wallet`
// dependency chain) links this binary against the system Python *framework* using
// an `@rpath/Python3.framework/Versions/<X.Y>/Python3` reference. pyo3-ffi tries to
// register the matching rpath, but it lives in a *library* dependency and Cargo does
// not propagate a dependency's link-args into a downstream *binary* crate. The result
// is a `spel` binary that references `@rpath/...` with no `LC_RPATH` to resolve it:
//
//   dyld: Library not loaded: @rpath/Python3.framework/Versions/3.9/Python3
//   Reason: no LC_RPATH's found
//
// This build script belongs to the binary crate, so its link-args *do* apply. We ask
// the same interpreter pyo3 uses (honoring PYO3_PYTHON, else `python3`) for the
// directory that contains the `.framework`, and add it as an rpath. Non-framework
// Python builds report an empty prefix, in which case we add nothing.
fn main() {
    println!("cargo:rerun-if-env-changed=PYO3_PYTHON");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }

    let python = std::env::var("PYO3_PYTHON").unwrap_or_else(|_| "python3".to_string());
    let Ok(output) = std::process::Command::new(&python)
        .args([
            "-c",
            "import sysconfig; print(sysconfig.get_config_var('PYTHONFRAMEWORKPREFIX') or '')",
        ])
        .output()
    else {
        return;
    };

    // Ignore the output unless the interpreter exited cleanly; a non-zero exit
    // that still wrote to stdout would otherwise be injected as an rpath arg.
    if !output.status.success() {
        return;
    }

    let prefix = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !prefix.is_empty() {
        println!("cargo:rustc-link-arg-bins=-Wl,-rpath,{prefix}");
    }
}
