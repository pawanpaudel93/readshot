//! Build script for `readshot-app`.
//!
//! A build script's own `cfg(target_os)` is the HOST, so everything here
//! gates on the TARGET (`CARGO_CFG_TARGET_OS`): cross-checking Windows or
//! Linux from a Mac must not compile Swift or link Apple frameworks.
//!
//! * macOS: compiles `src/macos_notify.swift` (Readshot's notifications,
//!   through UNUserNotificationCenter) into a static library, the same way
//!   `readshot-ocr` builds its Vision shim, and repeats `readshot-capture`'s
//!   Swift-runtime rpaths, which `screencapturekit` needs and Cargo's
//!   `rustc-link-arg` does not propagate across crates.
//! * Windows: links `packaging/windows/readshot.res` (the multi-size app
//!   icon) into the `readshot` executable.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    match env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("macos") => macos(),
        Ok("windows") => windows(),
        _ => {}
    }
}

fn windows() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let res = manifest.join("../../packaging/windows/readshot.res");
    println!("cargo:rerun-if-changed={}", res.display());
    if !res.is_file() {
        panic!(
            "{} is missing: run packaging/windows/render-icon.sh",
            res.display()
        );
    }
    match env::var("CARGO_CFG_TARGET_ENV").as_deref() {
        // link.exe (and lld-link) take a compiled .res as an input file and
        // embed its resources: the icon Explorer, the taskbar and shortcuts
        // show for readshot.exe. Only the binary gets it: a .res linked into
        // a library or a test harness would be dead weight.
        Ok("msvc") => println!("cargo:rustc-link-arg-bins={}", res.display()),
        // GNU ld cannot read a .res; it needs a COFF object from windres,
        // and no release or CI job builds the GNU target (test.yml's
        // windows-latest job and every packaging path use MSVC). A GNU
        // build therefore gets the window and tray icons (set at runtime)
        // but no file icon in Explorer.
        _ => println!(
            "cargo:warning=readshot.exe gets no file icon on this Windows target (only MSVC links packaging/windows/readshot.res)"
        ),
    }
}

fn macos() {
    rpaths_for_screencapturekit();
    compile_notify_shim();
}

fn rpaths_for_screencapturekit() {
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");

    if let Ok(output) = Command::new("xcode-select").arg("-p").output() {
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

fn compile_notify_shim() {
    const SOURCE: &str = "src/macos_notify.swift";
    println!("cargo:rerun-if-changed={SOURCE}");

    let arch = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86_64") => "x86_64",
        Ok(other) => panic!("unsupported macOS target architecture: {other}"),
        Err(error) => panic!("CARGO_CFG_TARGET_ARCH should be set: {error}"),
    };
    // Matches Info.plist's LSMinimumSystemVersion.
    let target = format!("{arch}-apple-macosx14.0");
    let sdk_path = command_stdout("xcrun", &["--sdk", "macosx", "--show-sdk-path"]);

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR should be set"));
    let lib_path = out_dir.join("libreadshot_macos_notify_swift.a");
    let status = Command::new("swiftc")
        .args([
            "-parse-as-library",
            "-emit-library",
            "-static",
            "-O",
            // A module name distinct from readshot-ocr's Vision shim: both
            // static libraries land in the same binary.
            "-module-name",
            "ReadshotNotify",
            "-target",
            &target,
            "-sdk",
            &sdk_path,
            "-o",
        ])
        .arg(&lib_path)
        .arg(SOURCE)
        .status()
        .expect("failed to invoke swiftc for the macOS notification shim");
    if !status.success() {
        panic!("swiftc failed while compiling the macOS notification shim");
    }

    let swiftc = PathBuf::from(command_stdout("xcrun", &["--find", "swiftc"]));
    let toolchain_swift_lib = swiftc
        .parent()
        .and_then(Path::parent)
        .map(|usr| usr.join("lib/swift/macosx"))
        .expect("swiftc path should include usr/bin");

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!(
        "cargo:rustc-link-search=native={}",
        toolchain_swift_lib.display()
    );
    println!("cargo:rustc-link-lib=static=readshot_macos_notify_swift");
    println!("cargo:rustc-link-lib=framework=Foundation");
    println!("cargo:rustc-link-lib=framework=UserNotifications");
    println!("cargo:rustc-link-lib=dylib=swiftCore");
    println!(
        "cargo:rustc-link-arg=-Wl,-rpath,{}",
        toolchain_swift_lib.display()
    );
}

fn command_stdout(program: &str, args: &[&str]) -> String {
    let output = Command::new(program)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("failed to run {program}: {e}"));
    if !output.status.success() {
        panic!("{program} {args:?} failed");
    }
    String::from_utf8(output.stdout)
        .unwrap_or_else(|e| panic!("{program} printed non-UTF-8 output: {e}"))
        .trim()
        .to_string()
}
