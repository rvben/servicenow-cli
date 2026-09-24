//! Reserves an 8 MiB main-thread stack on Windows, matching the Linux and
//! macOS default. Windows reserves only 1 MiB, and parsing the full command
//! tree with clap needs close to that in unoptimized builds.

const STACK_BYTES: u32 = 8 * 1024 * 1024;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    match std::env::var("CARGO_CFG_TARGET_ENV").as_deref() {
        Ok("msvc") => println!("cargo:rustc-link-arg-bins=/STACK:{STACK_BYTES}"),
        Ok("gnu") => println!("cargo:rustc-link-arg-bins=-Wl,--stack,{STACK_BYTES}"),
        _ => {}
    }
}
