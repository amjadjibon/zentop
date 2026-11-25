fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rerun-if-changed=src/attribution.c");
        cc::Build::new()
            .file("src/attribution.c")
            .compile("zentop_attribution");
    }
}
