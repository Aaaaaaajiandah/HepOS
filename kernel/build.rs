fn main() {
    // Emit the linker script path relative to this crate's manifest directory.
    // This makes the build portable — no hardcoded absolute paths in config.toml.
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rustc-link-arg=-T{}/linker.ld", dir);
    println!("cargo:rerun-if-changed=linker.ld");
}
