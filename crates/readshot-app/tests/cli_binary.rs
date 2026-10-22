//! Tests that spawn the built `readshot` binary.
//!
//! None of them needs Screen Recording or a window server: they stop
//! before any capture (argument and destination checks), read an image
//! file, or print a static document. Each run gets its own home, data
//! and config folders ([`Sandbox`]), so the CLI's log file and the
//! uninstall planner never see the real user's folders on macOS and
//! Linux. Windows reads its known folders from the system, not the
//! environment, and there is no override, so every test that gets as
//! far as starting the CLI's logging (any subcommand that parses) is
//! off on Windows; only clap-level runs (`--help`, `--version`,
//! argument errors) and the internal child's argument check run there.
//!
//! No test here can run a real uninstall: the only uninstall run is
//! `--dry-run` without `--yes` and without a terminal, so a regression
//! in the dry-run branch still stops at the no-terminal refusal (which
//! `cli::tests::uninstall_refuses_without_a_terminal_unless_yes` covers
//! at the function level).

#[cfg(not(windows))]
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// A throwaway home folder for one run of the binary.
struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn home(&self) -> &Path {
        self.dir.path()
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_readshot"));
        command
            .env("HOME", self.home())
            .env("XDG_DATA_HOME", self.home().join(".local/share"))
            .env("XDG_CONFIG_HOME", self.home().join(".config"))
            .env("XDG_CACHE_HOME", self.home().join(".cache"))
            .env_remove("RUST_LOG")
            .stdin(Stdio::null());
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    #[cfg(not(windows))]
    fn run_with_stdin(&self, args: &[&str], stdin: &[u8]) -> Output {
        let mut child = self
            .command()
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(stdin).unwrap();
        child.wait_with_output().unwrap()
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A failure after parsing: the exit code, nothing on stdout, and one
/// `readshot: <reason>` line on stderr (no log noise).
fn assert_cli_error(output: &Output, code: i32, reason: &str) {
    let err = stderr(output);
    assert_eq!(output.status.code(), Some(code), "stderr: {err}");
    assert!(
        output.stdout.is_empty(),
        "stdout must stay empty on error, got: {}",
        stdout(output)
    );
    assert!(
        err.starts_with("readshot: ") && err.contains(reason),
        "expected `readshot: …{reason}…` on stderr, got: {err}"
    );
    assert_eq!(
        err.lines().count(),
        1,
        "stderr has more than the error: {err}"
    );
}

#[cfg(all(target_os = "macos", feature = "integration"))]
fn png_bytes(width: u32, height: u32) -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(width, height, image::Rgba([255, 255, 255, 255]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    img.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    bytes.into_inner()
}

#[test]
fn help_prints_cli_usage_without_log_noise() {
    let output = Sandbox::new().run(&["--help"]);

    assert!(output.status.success());
    assert!(
        output.stderr.is_empty(),
        "stderr should be empty for --help, got: {}",
        stderr(&output)
    );

    let stdout = stdout(&output);
    assert!(stdout.contains("Usage: readshot [COMMAND]"));
    assert!(stdout.contains("capture-window"));
    assert!(stdout.contains("list-displays"));
}

/// CLI-03: `--version` / `-V` print the package version and nothing else.
#[test]
fn version_prints_the_package_version() {
    let sandbox = Sandbox::new();
    for flag in ["--version", "-V"] {
        let output = sandbox.run(&[flag]);
        assert!(output.status.success(), "{flag}: {}", stderr(&output));
        assert!(output.stderr.is_empty(), "{flag}: {}", stderr(&output));
        assert_eq!(
            stdout(&output),
            format!("readshot {}\n", env!("CARGO_PKG_VERSION")),
            "{flag}"
        );
    }
}

#[cfg(not(windows))]
#[test]
fn invalid_scale_exits_usage_error_without_capture_or_log_noise() {
    let sandbox = Sandbox::new();
    let target = sandbox.path("ignored.png");
    let output = sandbox.run(&["capture", "--scale", "0", "-o", target.to_str().unwrap()]);

    assert_cli_error(
        &output,
        64,
        "invalid input: scale must be a positive finite number",
    );
    assert!(!stderr(&output).contains("logging to"));
    assert!(!target.exists(), "nothing may be captured");
}

/// CLI-10, -12, -13, -14, -16, -21, -24, -25, -44 (code 2): argument
/// errors are clap's, exit 2, print nothing on stdout, and never get
/// as far as a capture or a file.
#[test]
fn argument_errors_exit_2_before_anything_runs() {
    let sandbox = Sandbox::new();
    let target = sandbox.path("never.png");
    let out = target.to_str().unwrap();
    let cases: &[(&[&str], &str)] = &[
        (
            &["capture", "--rect", "1,2,3", "-o", out],
            "expected x,y,width,height",
        ),
        (
            &["capture", "--rect", "0,0,-5,10", "-o", out],
            "positive width and height",
        ),
        // B4: a zero-sized rect is as bad as a negative one.
        (
            &["capture", "--rect", "0,0,0,10", "-o", out],
            "positive width and height",
        ),
        (
            &["capture-window", "--window", "1", "--rect", "0,0,10,0"],
            "positive width and height",
        ),
        (
            &["capture", "--rect", "0,0,99999,10", "-o", out],
            "must not exceed",
        ),
        (&["capture", "--delay=-1", "-o", out], "--delay must be"),
        (
            &["capture", "--delay", "3601", "-o", out],
            "--delay must be <=",
        ),
        (
            &["capture", "--show-cursor", "--hide-cursor", "-o", out],
            "cannot be used with",
        ),
        (
            &["capture", "--interactive", "--rect", "0,0,10,10", "-o", out],
            "cannot be used with",
        ),
        (
            &["capture-text", "--interactive", "--display", "1"],
            "cannot be used with",
        ),
        (
            &["capture-and-ocr", "--interactive", "--scale", "2"],
            "cannot be used with",
        ),
        (
            &["capture", "--clipboard", "-o", out],
            "cannot be used with",
        ),
        (
            &["capture-text", "--clipboard", "--json"],
            "cannot be used with",
        ),
        (
            &["ocr", "-i", out, "--clipboard", "--json"],
            "cannot be used with",
        ),
        (
            &[
                "capture-window",
                "--window",
                "1",
                "--window-shadow",
                "--no-window-shadow",
            ],
            "cannot be used with",
        ),
        (&["capture", "--format", "gif", "-o", out], "invalid value"),
        (&["capture", "--bogus"], "unexpected argument"),
        (&["nonsense"], "unrecognized subcommand"),
    ];
    for (args, reason) in cases {
        let output = sandbox.run(args);
        let err = stderr(&output);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {err}");
        assert!(output.stdout.is_empty(), "{args:?}: {}", stdout(&output));
        assert!(
            err.contains("error:") && err.contains(reason),
            "{args:?}: expected `{reason}` in: {err}"
        );
        assert!(!target.exists(), "{args:?} wrote a file");
    }
}

/// CLI-18, CLI-34: JSON metadata needs a clean stdout, so the image
/// commands refuse `--json` without a file or the clipboard before
/// they capture anything.
#[cfg(not(windows))]
#[test]
fn json_without_a_destination_exits_64_before_capturing() {
    let sandbox = Sandbox::new();
    assert_cli_error(
        &sandbox.run(&["capture", "--json"]),
        64,
        "--json with image capture requires --output PATH or --clipboard",
    );
    assert_cli_error(
        &sandbox.run(&["capture-window", "--window", "no-such-window", "--json"]),
        64,
        "--json with image capture requires --output PATH or --clipboard",
    );
    // B6: the image and the text (or JSON) can't share stdout.
    for args in [
        &["capture-and-ocr", "--json", "--also-image", "-"][..],
        &["capture-and-ocr", "--also-image", "-"][..],
    ] {
        assert_cli_error(&sandbox.run(args), 64, "--also-image - needs --output FILE");
    }
}

/// CLI-29: language correction can be turned off on the real binary
/// (all three spellings parse; the missing input file is what fails).
#[cfg(not(windows))]
#[test]
fn language_correction_can_be_turned_off() {
    let sandbox = Sandbox::new();
    let missing = sandbox.path("missing.png");
    let missing = missing.to_str().unwrap();
    for flags in [
        &["--language-correction", "false"][..],
        &["--language-correction=false"][..],
        &["--no-language-correction"][..],
        &["--language-correction"][..],
    ] {
        let mut args = vec!["ocr", "-i", missing];
        args.extend_from_slice(flags);
        let output = sandbox.run(&args);
        assert_cli_error(&output, 74, "image decode failed");
    }
}

/// CLI-30, CLI-31, CLI-44 (74): input problems exit 74, and `-i -`
/// really reads stdin (garbage there is a decode error, not a missing
/// file called `-`).
#[cfg(not(windows))]
#[test]
fn ocr_input_errors_exit_74() {
    let sandbox = Sandbox::new();
    let missing = sandbox.path("missing.png");
    assert_cli_error(
        &sandbox.run(&["ocr", "-i", missing.to_str().unwrap()]),
        74,
        "image decode failed",
    );

    let junk = sandbox.path("junk.png");
    std::fs::write(&junk, b"not an image").unwrap();
    assert_cli_error(
        &sandbox.run(&["ocr", "-i", junk.to_str().unwrap()]),
        74,
        "image decode failed",
    );

    let output = sandbox.run_with_stdin(&["ocr", "-i", "-"], b"not an image either");
    assert_cli_error(&output, 74, "image decode failed");
    let err = stderr(&output);
    assert!(
        !err.contains("No such file") && !err.contains("cannot find"),
        "`-` must mean stdin, not a file: {err}"
    );
}

/// CLI-31, CLI-26, CLI-27, CLI-28: `ocr -i -` decodes the image from
/// stdin and runs the real engine on it; `--json -o FILE` writes the
/// metadata to the file and keeps stdout empty; plain `-o FILE` writes
/// the text with no newline added. A blank image keeps the result
/// independent of what the engine reads.
#[cfg(all(target_os = "macos", feature = "integration"))]
#[test]
fn ocr_reads_stdin_with_the_real_engine_and_writes_to_files() {
    let sandbox = Sandbox::new();
    let json_path = sandbox.path("ocr.json");
    let output = sandbox.run_with_stdin(
        &[
            "ocr",
            "-i",
            "-",
            "--json",
            "--languages",
            "en-US,fr-FR",
            "--language-correction=false",
            "-o",
            json_path.to_str().unwrap(),
        ],
        &png_bytes(37, 23),
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(output.stdout.is_empty(), "{}", stdout(&output));
    assert!(output.stderr.is_empty(), "{}", stderr(&output));
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&json_path).unwrap()).unwrap();
    assert_eq!(value["kind"], "ocr");
    assert_eq!(value["input"], "-");
    assert_eq!(value["image"]["width"], 37);
    assert_eq!(value["image"]["height"], 23);
    assert_eq!(value["languages"], serde_json::json!(["en-US", "fr-FR"]));
    assert_eq!(value["language_correction"], false);
    assert!(value["lines"].is_array());

    let input = sandbox.path("blank.png");
    std::fs::write(&input, png_bytes(40, 20)).unwrap();
    let text_path = sandbox.path("text.txt");
    let output = sandbox.run(&[
        "ocr",
        "-i",
        input.to_str().unwrap(),
        "-o",
        text_path.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(output.stdout.is_empty(), "{}", stdout(&output));
    let text = std::fs::read_to_string(&text_path).unwrap();
    assert!(!text.ends_with('\n'), "-o FILE adds no newline: {text:?}");
}

/// CLI-48: the internal overlay child refuses bad arguments with 64
/// before any window opens.
#[test]
fn internal_interactive_child_rejects_bad_arguments() {
    let sandbox = Sandbox::new();
    assert_cli_error(
        &sandbox.run(&["__interactive-capture"]),
        64,
        "__interactive-capture requires --output PATH",
    );
    assert_cli_error(
        &sandbox.run(&["__interactive-capture", "--output"]),
        64,
        "__interactive-capture requires --output PATH",
    );
    assert_cli_error(
        &sandbox.run(&["__interactive-capture", "--output", "x.png", "--bogus"]),
        64,
        "unknown __interactive-capture argument: --bogus",
    );
}

#[cfg(not(windows))]
#[test]
fn mcp_config_prints_json_without_log_noise() {
    let output = Sandbox::new().run(&["mcp-config"]);

    assert!(output.status.success());
    assert!(
        output.stderr.is_empty(),
        "stderr should be empty for mcp-config, got: {}",
        stderr(&output)
    );

    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["mcpServers"]["readshot"]["command"], "readshot-mcp");
}

/// CLI-36: `--name` and `--command` replace the server key and command.
#[cfg(not(windows))]
#[test]
fn mcp_config_takes_a_name_and_a_command() {
    let output = Sandbox::new().run(&[
        "mcp-config",
        "--name",
        "screen",
        "--command",
        "/Applications/Readshot.app/Contents/MacOS/readshot-mcp",
    ]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(output.stderr.is_empty(), "{}", stderr(&output));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "mcpServers": {
                "screen": {
                    "command": "/Applications/Readshot.app/Contents/MacOS/readshot-mcp",
                    "args": [],
                }
            }
        })
    );
}

/// CLI-37: every supported shell gets a script on stdout.
#[cfg(not(windows))]
#[test]
fn completions_prints_shell_script_without_log_noise() {
    let sandbox = Sandbox::new();
    for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
        let output = sandbox.run(&["completions", shell]);
        assert!(output.status.success(), "{shell}: {}", stderr(&output));
        assert!(output.stderr.is_empty(), "{shell}: {}", stderr(&output));
        let script = stdout(&output);
        assert!(script.contains("capture-and-ocr"), "{shell}");
        assert!(script.contains("mcp-config"), "{shell}");
        assert!(!script.contains("__interactive-capture"), "{shell}");
    }
    assert!(stdout(&sandbox.run(&["completions", "zsh"])).contains("_readshot"));
}

/// Lay out an installed-looking home: a login agent, and (on Unix) the
/// two CLI links pointing into a `Readshot.app`.
#[cfg(not(windows))]
fn installed_home(sandbox: &Sandbox) -> Vec<PathBuf> {
    let agents = sandbox.home().join("Library/LaunchAgents");
    std::fs::create_dir_all(&agents).unwrap();
    let plist = agents.join("np.com.pawanpaudel.readshot.login.plist");
    std::fs::write(&plist, b"<plist/>").unwrap();
    #[allow(unused_mut)]
    let mut kept = vec![plist];
    #[cfg(unix)]
    {
        let macos = sandbox
            .home()
            .join("Applications/Readshot.app/Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        let bin = sandbox.home().join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        for name in ["readshot", "readshot-mcp"] {
            std::fs::write(macos.join(name), b"#!/bin/sh\n").unwrap();
            std::os::unix::fs::symlink(macos.join(name), bin.join(name)).unwrap();
            kept.push(bin.join(name));
        }
    }
    kept
}

/// CLI-38, CLI-39, CLI-42: `uninstall -n` prints the ordered plan
/// (including what `--purge` adds) and changes nothing.
#[cfg(not(windows))]
#[test]
fn uninstall_dry_run_prints_the_plan_and_changes_nothing() {
    let sandbox = Sandbox::new();
    let kept = installed_home(&sandbox);

    let output = sandbox.run(&["uninstall", "-n", "--purge"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(output.stderr.is_empty(), "{}", stderr(&output));
    let plan = stdout(&output);
    assert!(plan.starts_with("This will:\n"), "{plan}");
    assert!(
        plan.contains("Remove the launch-at-login item")
            && plan.contains("np.com.pawanpaudel.readshot.login.plist"),
        "{plan}"
    );
    #[cfg(unix)]
    assert!(
        plan.contains("Remove the command-line tool") && plan.contains(".local/bin/readshot-mcp"),
        "{plan}"
    );
    #[cfg(target_os = "macos")]
    assert!(
        plan.contains("Reset the Screen Recording permission"),
        "{plan}"
    );
    #[cfg(not(target_os = "macos"))]
    assert!(plan.contains("Remove the Open at Login entry"), "{plan}");
    assert!(
        plan.contains("Delete preferences"),
        "--purge adds data: {plan}"
    );
    assert!(
        plan.trim_end().ends_with("Dry run — nothing was changed."),
        "{plan}"
    );
    for path in &kept {
        assert!(
            std::fs::symlink_metadata(path).is_ok(),
            "{} was removed by a dry run",
            path.display()
        );
    }

    let without_purge = stdout(&sandbox.run(&["uninstall", "--dry-run"]));
    assert!(
        !without_purge.contains("Delete preferences"),
        "{without_purge}"
    );
}

/// CLI-45, CLI-46: the CLI logs to `readshot.log` in the data folder,
/// never to stderr, even with `RUST_LOG=trace`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn cli_logs_go_to_the_data_folder_file_not_stderr() {
    let sandbox = Sandbox::new();
    let output = sandbox
        .command()
        .env("RUST_LOG", "trace")
        .args(["capture", "--json"])
        .output()
        .unwrap();
    assert_cli_error(&output, 64, "--json with image capture requires");

    #[cfg(target_os = "macos")]
    let log = sandbox
        .home()
        .join("Library/Application Support/np.com.pawanpaudel.Readshot/readshot.log");
    #[cfg(target_os = "linux")]
    let log = sandbox.home().join(".local/share/readshot/readshot.log");
    assert!(log.is_file(), "expected the CLI log at {}", log.display());
    // The run is in it: the command, then its failure.
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(
        text.contains("cli command") && text.contains("command=\"capture\""),
        "{text}"
    );
    assert!(text.contains("cli command failed"), "{text}");
}
