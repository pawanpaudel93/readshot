//! Build script for `readshot-capture`.
//!
//! On macOS, the `screencapturekit` crate links a Swift bridge that
//! depends on the Swift Concurrency runtime (`libswift_Concurrency.dylib`).
//! Cargo's `rustc-link-arg` build-script directive only applies to the
//! crate that emits it, not to downstream artefacts — so `screencapturekit`'s
//! own rpath additions don't reach this crate's test binaries. We mirror
//! those rpaths here so `cargo nextest run -p readshot-capture` finds
//! the Swift runtime libraries on a stock macOS host.

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "macos" {
        return;
    }

    // System Swift runtime path. Present on macOS 12+.
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");

    // Xcode-installed Swift runtime — the only place
    // `libswift_Concurrency.dylib` lives on most developer machines.
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
