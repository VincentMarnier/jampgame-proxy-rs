fn main() {
    let mut build = cc::Build::new();
    // syscall_shim.c: variadic trap receive shim handed to the original game
    // module as its syscall pointer (R-008/U-006). -mstackrealign: the game
    // enters it with a 4-byte-aligned stack, while gcc i386 assumes 16.
    build.file("csrc/syscall_shim.c").flag("-mstackrealign");

    let target = std::env::var("TARGET").unwrap_or_default();
    let host = std::env::var("HOST").unwrap_or_default();
    if target == "i686-unknown-linux-gnu" && host.starts_with("x86_64") {
        // Cross-compiling down to i386: gcc-multilib provides -m32.
        build.flag("-m32");
    }

    build.compile("jampgame_syscall_shim");
    println!("cargo:rerun-if-changed=csrc/syscall_shim.c");
}
