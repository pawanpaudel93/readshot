fn main() {
    #[cfg(target_os = "macos")]
    {
        use std::env;
        use std::path::PathBuf;
        use std::process::Command;

        let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR should be set"));
        let lib_path = out_dir.join("libreadshot_macos_vision_swift.a");
        let status = Command::new("swiftc")
            .args(["-parse-as-library", "-emit-library", "-static", "-O", "-o"])
            .arg(&lib_path)
            .arg("src/macos_vision.swift")
            .status()
            .expect("failed to invoke swiftc for macOS Vision shim");
        if !status.success() {
            panic!("swiftc failed while compiling macOS Vision shim");
        }
        let swiftc = Command::new("xcrun")
            .args(["--find", "swiftc"])
            .output()
            .expect("failed to locate swiftc with xcrun");
        if !swiftc.status.success() {
            panic!("xcrun could not locate swiftc");
        }
        let swiftc_path = String::from_utf8(swiftc.stdout).expect("swiftc path should be UTF-8");
        let swiftc_path = PathBuf::from(swiftc_path.trim());
        let toolchain_swift_lib = swiftc_path
            .parent()
            .and_then(|bin| bin.parent())
            .map(|usr| usr.join("lib/swift/macosx"))
            .expect("swiftc path should include usr/bin");

        println!("cargo:rerun-if-changed=src/macos_vision.swift");
        println!("cargo:rustc-link-search=native={}", out_dir.display());
        println!(
            "cargo:rustc-link-search=native={}",
            toolchain_swift_lib.display()
        );
        println!("cargo:rustc-link-lib=static=readshot_macos_vision_swift");
        println!("cargo:rustc-link-lib=framework=Foundation");
        println!("cargo:rustc-link-lib=framework=Vision");
        println!("cargo:rustc-link-lib=dylib=swiftCore");
        println!("cargo:rustc-link-lib=dylib=swift_Concurrency");
        println!("cargo:rustc-link-lib=dylib=swift_StringProcessing");
        println!("cargo:rustc-link-lib=dylib=swift_RegexParser");
        println!(
            "cargo:rustc-link-arg=-Wl,-rpath,{}",
            toolchain_swift_lib.display()
        );
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
}
