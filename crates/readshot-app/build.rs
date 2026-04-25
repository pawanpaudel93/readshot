//! Build script for `readshot-app`.
//!
//! Mirrors `readshot-capture`'s rpath additions because the
//! transitive `screencapturekit` dependency drags in a Swift
//! runtime that lives outside the standard linker search paths.
//! Cargo's `rustc-link-arg` is per-crate, so this file repeats the
//! incantation for the binary's own artefacts.

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
            for path in [
                format!("{xcode}/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift/macosx"),
                format!("{xcode}/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift-5.5/macosx"),
            ] {
                println!("cargo:rustc-link-arg=-Wl,-rpath,{path}");
            }
        }
    }
}
