use std::process::Command;

fn readshot() -> Command {
    Command::new(env!("CARGO_BIN_EXE_readshot"))
}

#[test]
fn help_prints_cli_usage_without_log_noise() {
    let output = readshot().arg("--help").output().unwrap();

    assert!(output.status.success());
    assert!(
        output.stderr.is_empty(),
        "stderr should be empty for --help, got: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Usage: readshot [COMMAND]"));
    assert!(stdout.contains("capture-window"));
    assert!(stdout.contains("list-displays"));
}

#[test]
fn invalid_scale_exits_usage_error_without_capture_or_log_noise() {
    let output = readshot()
        .args(["capture", "--scale", "0", "-o", "/tmp/readshot-ignored.png"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(64));
    assert!(
        output.stdout.is_empty(),
        "stdout should be empty on usage error, got: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("readshot: invalid input: --scale must be a positive finite number"));
    assert!(
        !stderr.contains("logging to"),
        "CLI stderr should not include routine logging: {stderr}"
    );
}

#[test]
fn mcp_config_prints_json_without_log_noise() {
    let output = readshot().arg("mcp-config").output().unwrap();

    assert!(output.status.success());
    assert!(
        output.stderr.is_empty(),
        "stderr should be empty for mcp-config, got: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["mcpServers"]["readshot"]["command"], "readshot-mcp");
}
