fn main() {
    println!("cargo::rerun-if-changed=csrc/stretch.cpp");
    println!("cargo::rerun-if-changed=vendor");
    let mut b = cc::Build::new();
    b.cpp(true).std("c++14").file("csrc/stretch.cpp").include("vendor").include("vendor/signalsmith-stretch");
    // always optimise the stretcher, even in debug builds (10x slower otherwise)
    b.opt_level(3).debug(false);
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        b.define("SIGNALSMITH_USE_ACCELERATE", None);
        println!("cargo:rustc-link-lib=framework=Accelerate");
    }
    b.compile("clubcraft_stretch");
}
