fn main() {
    napi_build::setup();

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        // Keep definitions from the embedded V8 archives out of the addon's
        // dynamic symbol table instead of exposing them beside Node's V8.
        println!("cargo:rustc-link-arg=-Wl,--exclude-libs,ALL");
    }
}
