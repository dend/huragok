// Compile the structured-exception guard (csrc/seh.c) with the MSVC toolchain.
// Rust has no __try/__except, so we keep the SEH frame in a tiny C shim and call
// into it from src/seh.rs to survive access violations from wrong reflected offsets.
fn main() {
    cc::Build::new()
        .file("csrc/seh.c")
        .compile("huragok_seh");
    println!("cargo:rerun-if-changed=csrc/seh.c");
}
