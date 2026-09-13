fn main() {
    // The CrispASR runtime libraries ship as bundle resources, beside the executable on
    // Windows and in the resource directory elsewhere. CrispASR's own build script adds
    // rpaths for its build tree; these add the installed locations.
    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("macos") => {
            println!("cargo:rustc-link-arg-bins=-Wl,-rpath,@executable_path/../Resources");
        }
        Ok("linux") => {
            println!("cargo:rustc-link-arg-bins=-Wl,-rpath,$ORIGIN/../lib/lathe");
        }
        _ => {}
    }
    tauri_build::build()
}
