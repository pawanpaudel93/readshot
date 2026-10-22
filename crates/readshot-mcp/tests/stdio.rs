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

// `--check` writes a probe file into the history folder, which on
// Windows is the real %LOCALAPPDATA% (known folders ignore HOME).
#[cfg(not(windows))]
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

// ── W5c: more of the protocol, the options, and the history tools on
// the real binary ──

/// A throwaway home folder (and XDG data/config folders) for one run,
/// removed on drop. History seeded here is what the binary reads on
/// macOS and Linux; Windows reads its known folders from the system,
/// so tests that touch history are Unix-only.
struct Sandbox {
    home: std::path::PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let home =
            std::env::temp_dir().join(format!("readshot-mcp-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        Self { home }
    }

    fn command(&self) -> Command {
        let mut command = readshot_mcp();
        command
            .env("HOME", &self.home)
            .env("XDG_DATA_HOME", self.home.join(".local/share"))
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env_remove("RUST_LOG");
        command
    }

    /// Run the stdio server over `input` and return its reply lines.
    fn serve(&self, input: Vec<u8>) -> (Vec<Value>, std::process::Output) {
        let mut child = self
            .command()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        // Write from another thread so a large input can't deadlock
        // against replies filling the stdout pipe.
        let writer = std::thread::spawn(move || stdin.write_all(&input));
        let output = child.wait_with_output().unwrap();
        writer.join().unwrap().unwrap();
        let replies = String::from_utf8(output.stdout.clone())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        (replies, output)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn lines(requests: &[&str]) -> Vec<u8> {
    let mut input = String::new();
    for request in requests {
        input.push_str(request);
        input.push('\n');
    }
    input.into_bytes()
}

/// MCP-05, MCP-01: `shutdown` is answered but doesn't stop the server;
/// only end of input does, with exit 0.
#[test]
fn shutdown_is_answered_and_the_server_runs_until_eof() {
    let sandbox = Sandbox::new("shutdown");
    let (replies, output) = sandbox.serve(lines(&[
        r#"{"jsonrpc":"2.0","id":1,"method":"shutdown"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#,
    ]));
    assert!(output.status.success());
    assert_eq!(replies.len(), 2, "{replies:?}");
    assert_eq!(replies[0]["id"], 1);
    assert_eq!(replies[0]["result"], serde_json::json!({}));
    assert_eq!(replies[1]["id"], 2);
    assert_eq!(replies[1]["result"], serde_json::json!({}));
}

/// MCP-07, MCP-08, MCP-10: malformed input over the real pipe — parse
/// errors, bad versions, unknown methods and tools — each gets one
/// error reply; blank lines and notifications get none; CRLF works.
#[test]
fn protocol_errors_get_one_reply_each() {
    let sandbox = Sandbox::new("protocol");
    let mut input = lines(&[
        "nope",
        "",
        r#"{"jsonrpc":"1.0","id":2,"method":"ping"}"#,
        r#"{"jsonrpc":"2.0","id":[1],"method":"ping"}"#,
        r#"{"jsonrpc":"2.0","id":3,"method":"resources/list"}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","method":"no/such/notification"}"#,
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{}}"#,
        r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"nope"}}"#,
    ]);
    input.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":6,\"method\":\"ping\"}\r\n");
    let (replies, output) = sandbox.serve(input);
    assert!(output.status.success());

    let expected: &[(Value, i64)] = &[
        (Value::Null, -32700),
        (serde_json::json!(2), -32600),
        (Value::Null, -32600),
        (serde_json::json!(3), -32601),
        (serde_json::json!(4), -32602),
        (serde_json::json!(5), -32601),
    ];
    assert_eq!(replies.len(), expected.len() + 1, "{replies:#?}");
    for (reply, (id, code)) in replies.iter().zip(expected) {
        assert_eq!(&reply["id"], id, "{reply}");
        assert_eq!(reply["error"]["code"], *code, "{reply}");
        assert_eq!(reply["jsonrpc"], "2.0");
    }
    assert_eq!(replies[6]["id"], 6, "a CRLF line is still a request");
    assert_eq!(replies[6]["result"], serde_json::json!({}));
}

/// MCP-09: a line over 8 MiB is refused with a parse error and the
/// server goes on to answer the next request.
#[test]
fn an_oversized_line_is_refused_and_the_server_keeps_serving() {
    let sandbox = Sandbox::new("oversized");
    let mut input = vec![b'x'; 8 * 1024 * 1024 + 1];
    input.push(b'\n');
    input.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"ping\"}\n");
    let (replies, output) = sandbox.serve(input);
    assert!(output.status.success());
    assert_eq!(replies.len(), 2, "{replies:?}");
    assert_eq!(replies[0]["id"], Value::Null);
    assert_eq!(replies[0]["error"]["code"], -32700);
    assert!(replies[0]["error"]["message"]
        .as_str()
        .unwrap()
        .contains("exceeds maximum size"));
    assert_eq!(replies[1]["id"], 9);
    assert_eq!(replies[1]["result"], serde_json::json!({}));
}

/// MCP-02: an unknown option exits 64 with a message on stderr only.
#[test]
fn an_unknown_option_exits_64() {
    let sandbox = Sandbox::new("badarg");
    for args in [&["--bogus"][..], &["--check", "--help"][..], &["-x"][..]] {
        let output = sandbox.command().args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(64), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("unknown argument"));
    }
    let output = sandbox.command().arg("-h").output().unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("--check"));
}

/// MCP-03: `--check` reports an unwritable history folder and exits 70.
#[cfg(unix)]
#[test]
fn check_exits_70_when_history_is_not_writable() {
    let sandbox = Sandbox::new("check-fail");
    // A regular file where the data folder should be: nothing can be
    // created under it.
    let blocker = sandbox.home.join("blocker");
    std::fs::write(&blocker, b"not a folder").unwrap();
    let output = readshot_mcp()
        .arg("--check")
        .env("HOME", &blocker)
        .env("XDG_DATA_HOME", blocker.join("share"))
        .env("XDG_CONFIG_HOME", blocker.join("config"))
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(70));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["ok"], false);
    assert_eq!(value["history"]["writable"], false);
    assert!(value["history"]["error"].is_string());
    assert!(value["history"]["path"]
        .as_str()
        .unwrap()
        .starts_with(blocker.to_str().unwrap()));
    assert_eq!(value["tools"]["ok"], true, "only the history check failed");
}

/// Where the GUI keeps history for this home (runtime.rs
/// `default_history_root`; readshot-app pins the same path).
#[cfg(unix)]
fn gui_history_root(home: &std::path::Path) -> std::path::PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/np.com.pawanpaudel.Readshot/history")
    } else {
        home.join(".local/share/readshot/history")
    }
}

#[cfg(unix)]
fn seed_record(
    store: &readshot_core::FsHistoryStore,
    id: &str,
    captured_at: &str,
    ocr_text: &str,
) -> readshot_core::CaptureRecord {
    use readshot_core::HistoryStore;
    let record: readshot_core::CaptureRecord = serde_json::from_value(serde_json::json!({
        "id": id,
        "captured_at": captured_at,
        "width_px": 40,
        "height_px": 20,
        "display_id": "seeded-display",
        "ocr_text": ocr_text,
        "annotation_model": [],
    }))
    .unwrap();
    store.save(&record, b"seeded png").unwrap();
    record
}

/// MCP-29..33, MCP-38: the binary reads the history the GUI writes
/// (the same folder), newest first, through every history tool; bad
/// history arguments are invalid-params errors, not tool errors.
#[cfg(unix)]
#[test]
fn history_tools_read_the_gui_history_folder() {
    let sandbox = Sandbox::new("history");
    let root = gui_history_root(&sandbox.home);

    // Empty history first: a null capture, not an error.
    let (replies, _) = sandbox.serve(lines(&[
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"latest_capture","arguments":{}}}"#,
    ]));
    assert_eq!(replies[0]["result"]["isError"], false, "{}", replies[0]);
    assert_eq!(
        replies[0]["result"]["structuredContent"]["capture"],
        Value::Null
    );

    // `--check` reports that very folder.
    let check = sandbox.command().arg("--check").output().unwrap();
    let check: Value = serde_json::from_slice(&check.stdout).unwrap();
    assert_eq!(check["history"]["path"], root.display().to_string());

    let store = readshot_core::FsHistoryStore::new(&root);
    let older_id = "11111111-1111-4111-8111-111111111111";
    let newer_id = "22222222-2222-4222-8222-222222222222";
    seed_record(
        &store,
        older_id,
        "2026-01-01T10:00:00Z",
        "Quarterly INVOICE total",
    );
    seed_record(&store, newer_id, "2026-02-01T10:00:00Z", "meeting notes");

    let call = |id: u32, name: &str, arguments: Value| {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        })
        .to_string()
    };
    let requests = [
        call(1, "recent_captures", serde_json::json!({})),
        call(2, "recent_captures", serde_json::json!({"limit": 1})),
        call(3, "latest_capture", serde_json::json!({})),
        call(4, "get_capture", serde_json::json!({"id": older_id})),
        call(
            5,
            "search_captures",
            serde_json::json!({"query": "invoice"}),
        ),
        call(
            6,
            "get_capture",
            serde_json::json!({"id": "33333333-3333-4333-8333-333333333333"}),
        ),
        call(7, "recent_captures", serde_json::json!({"limit": 0})),
        call(8, "search_captures", serde_json::json!({"query": "  "})),
        call(9, "recent_captures", serde_json::json!({"bogus": 1})),
    ];
    let request_refs: Vec<&str> = requests.iter().map(String::as_str).collect();
    let (replies, output) = sandbox.serve(lines(&request_refs));
    assert!(output.status.success());
    assert_eq!(replies.len(), 9, "{replies:#?}");
    let payload = |i: usize| &replies[i]["result"]["structuredContent"];

    let all = payload(0)["captures"].as_array().unwrap();
    let ids: Vec<&str> = all.iter().map(|c| c["id"].as_str().unwrap()).collect();
    assert_eq!(ids, [newer_id, older_id], "newest first");
    let first = &all[0];
    assert_eq!(first["width_px"], 40);
    assert_eq!(first["height_px"], 20);
    assert_eq!(first["display_id"], "seeded-display");
    assert_eq!(first["ocr_text"], "meeting notes");
    assert_eq!(first["redacted"], false);
    assert!(first["captured_at"]
        .as_str()
        .unwrap()
        .starts_with("2026-02-01"));
    let image_path = first["image_path"].as_str().unwrap();
    assert!(
        image_path.starts_with(root.to_str().unwrap()),
        "{image_path}"
    );
    assert_eq!(std::fs::read(image_path).unwrap(), b"seeded png");
    // The text envelope carries the same payload as structuredContent.
    let text: Value =
        serde_json::from_str(replies[0]["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(&text, payload(0));

    assert_eq!(payload(1)["captures"].as_array().unwrap().len(), 1);
    assert_eq!(payload(1)["captures"][0]["id"], newer_id);
    assert_eq!(payload(2)["capture"]["id"], newer_id);
    assert_eq!(payload(3)["capture"]["id"], older_id);
    let found = payload(4)["captures"].as_array().unwrap();
    assert_eq!(found.len(), 1, "case-insensitive OCR text match");
    assert_eq!(found[0]["id"], older_id);

    for (i, id) in [(5, 6), (6, 7), (7, 8), (8, 9)] {
        assert_eq!(replies[i]["id"], id);
        assert_eq!(replies[i]["error"]["code"], -32602, "{}", replies[i]);
    }
}
