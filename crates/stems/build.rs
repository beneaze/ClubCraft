//! The prebuilt ONNX Runtime static library references `___isPlatformVersionAtLeast` (clang's
//! `@available` helper), which lives in `libclang_rt.osx.a`. rustc links with `-nodefaultlibs`,
//! so link the clang runtime explicitly on macOS. The directive propagates to every binary that
//! depends on this crate.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let out = std::process::Command::new("xcrun").args(["clang", "--print-resource-dir"]).output();
    let dir = match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => {
            println!("cargo:warning=xcrun clang not found; linking onnxruntime may fail");
            return;
        }
    };
    let lib = std::path::Path::new(&dir).join("lib/darwin");
    if lib.join("libclang_rt.osx.a").exists() {
        println!("cargo:rustc-link-search=native={}", lib.display());
        println!("cargo:rustc-link-lib=static=clang_rt.osx");
    }
}
