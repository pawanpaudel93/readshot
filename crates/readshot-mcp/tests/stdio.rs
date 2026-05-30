use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::Value;

fn readshot_mcp() -> Command {
    Command::new(env!("CARGO_BIN_EXE_readshot-mcp"))
}

#[test]
fn stdio_server_handles_core_json_rpc_methods() {
    let mut child = readshot_mcp()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    {
        let stdin = child.stdin.as_mut().unwrap();
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{}}}}"#
        )
        .unwrap();
        writeln!(stdin, r#"{{"jsonrpc":"2.0","id":2,"method":"ping"}}"#).unwrap();
        writeln!(stdin, r#"{{"jsonrpc":"2.0","id":3,"method":"tools/list"}}"#).unwrap();
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{{"name":"capture_region","arguments":{{"scale":"2"}}}}}}"#
        )
        .unwrap();
        writeln!(stdin, r#"{{"jsonrpc":"2.0","method":"ping"}}"#).unwrap();
        writeln!(stdin, r#"{{"jsonrpc":"2.0","id":5}}"#).unwrap();
    }
    drop(child.stdin.take());

    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());

    let stdout = String::from_utf8(output.stdout).unwrap();
    let responses: Vec<Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        responses.len(),
        5,
        "notifications must not produce response lines: {stdout}"
    );

    assert_eq!(responses[0]["id"], 1);
    assert_eq!(responses[0]["result"]["serverInfo"]["name"], "readshot-mcp");
    assert!(responses[0]["result"]["capabilities"]["tools"].is_object());

    assert_eq!(responses[1]["id"], 2);
    assert_eq!(responses[1]["result"], serde_json::json!({}));

    assert_eq!(responses[2]["id"], 3);
    let tools = responses[2]["result"]["tools"].as_array().unwrap();
    assert!(tools.iter().any(|tool| tool["name"] == "capture_region"));
    assert!(tools.iter().any(|tool| tool["name"] == "search_captures"));

    assert_eq!(responses[3]["id"], 4);
    assert_eq!(responses[3]["error"]["code"], -32602);
    assert!(responses[3]["error"]["message"]
        .as_str()
        .unwrap()
        .contains("scale must be a number"));

    assert_eq!(responses[4]["id"], 5);
    assert_eq!(responses[4]["error"]["code"], -32600);

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        !stderr.contains('{'),
        "MCP stderr should not contain protocol JSON: {stderr}"
    );
}

#[test]
fn check_prints_json_diagnostics_without_stderr() {
    let home = std::env::temp_dir().join(format!(
        "readshot-mcp-check-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&home).unwrap();
    let output = readshot_mcp()
        .arg("--check")
        .env("HOME", &home)
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(
        output.stderr.is_empty(),
        "stderr should be empty for --check, got: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["ok"], true);
    assert_eq!(value["server"], "readshot-mcp");
    assert_eq!(value["transport"], "stdio");
    assert!(value["tools"]["count"].as_u64().unwrap() > 0);
    assert_eq!(value["history"]["writable"], true);
}

#[test]
fn help_prints_usage_without_stderr() {
    let output = readshot_mcp().arg("--help").output().unwrap();

    assert!(output.status.success());
    assert!(
        output.stderr.is_empty(),
        "stderr should be empty for --help, got: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("Usage: readshot-mcp"));
}
