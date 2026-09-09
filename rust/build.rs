fn main() {
    let mut build = cc::Build::new();
    build.file("csrc/syscall_shim.c");

    let target = std::env::var("TARGET").unwrap_or_default();
    let host = std::env::var("HOST").unwrap_or_default();
    if target == "i686-unknown-linux-gnu" && host.starts_with("x86_64") {
        // Cross-compiling down to i386: gcc-multilib provides -m32.
        build.flag("-m32");
    }

    build.compile("jampgame_syscall_shim");
    println!("cargo:rerun-if-changed=csrc/syscall_shim.c");
}
