//! Build script for `readshot-mcp`.
//!
//! Mirrors the Swift Concurrency rpath fixups in `readshot-capture`'s
//! and `readshot-app`'s build scripts. The `screencapturekit` crate
//! reaches us transitively via `readshot-capture`, but Cargo's
//! `rustc-link-arg` directive only applies to the emitting crate —
//! so this binary's test harness can't find `libswift_Concurrency.dylib`
//! unless we re-emit the same rpaths here.

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "macos" {
        return;
    }

    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");

    if let Ok(output) = std::process::Command::new("xcode-select")
        .arg("-p")
        .output()
    {
        if output.status.success() {
            let xcode = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let candidates = [
                format!("{xcode}/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift/macosx"),
                format!("{xcode}/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift-5.5/macosx"),
            ];
            for path in candidates {
                println!("cargo:rustc-link-arg=-Wl,-rpath,{path}");
            }
        }
    }
}
