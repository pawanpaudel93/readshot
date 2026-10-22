//! The MCP documentation (docs/MCP.md and site/mcp.html) checked against
//! the server: every tool is documented, every example request is one the
//! server accepts, and every host config launches `readshot-mcp`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use readshot_capture::fake::FakeCapturer;
use readshot_mcp::McpServer;
use readshot_ocr::fake::FakeOcrEngine;
use serde_json::Value;

fn repo_file(rel: &str) -> String {
    let root: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// Fenced blocks of a markdown file whose info string is one of `langs`.
fn markdown_blocks(md: &str, langs: &[&str]) -> Vec<String> {
    let mut blocks = Vec::new();
    // `Some(Some(_))` collects a wanted block, `Some(None)` skips one.
    let mut open: Option<Option<String>> = None;
    for line in md.lines() {
        if let Some(info) = line.strip_prefix("```") {
            match open.take() {
                Some(Some(block)) => blocks.push(block),
                Some(None) => {}
                None => {
                    open = Some(langs.contains(&info.trim()).then(String::new));
                }
            }
            continue;
        }
        if let Some(Some(block)) = open.as_mut() {
            block.push_str(line);
            block.push('\n');
        }
    }
    blocks
}

fn unescape_html(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// The text of every `<pre><code …>…</code></pre>` block.
fn html_code_blocks(html: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut rest = html;
    while let Some(start) = rest.find("<pre><code") {
        let after = &rest[start..];
        let open_end = after.find('>').unwrap() + 1;
        let close = after.find("</code></pre>").unwrap();
        blocks.push(unescape_html(&after[open_end..close]));
        rest = &after[close..];
    }
    blocks
}

/// Every JSON value in the documents' code blocks (blocks that are not
/// JSON, such as shell or TOML, are skipped).
fn json_examples() -> Vec<(String, Value)> {
    let mut found = Vec::new();
    let md = repo_file("docs/MCP.md");
    for block in markdown_blocks(&md, &["json", "jsonc"]) {
        let value = serde_json::from_str(&block)
            .unwrap_or_else(|e| panic!("docs/MCP.md block is not JSON ({e}):\n{block}"));
        found.push(("docs/MCP.md".to_string(), value));
    }
    for block in html_code_blocks(&repo_file("site/mcp.html")) {
        let trimmed = block.trim_start();
        if trimmed.starts_with('{') || trimmed.starts_with('[') {
            let value = serde_json::from_str(&block)
                .unwrap_or_else(|e| panic!("site/mcp.html block is not JSON ({e}):\n{block}"));
            found.push(("site/mcp.html".to_string(), value));
        }
    }
    found
}

/// Request bodies passed to `printf '…\n'` in shell examples.
fn printf_requests(text: &str) -> Vec<Value> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("printf '") {
        let after = &rest[start + "printf '".len()..];
        let end = after.find('\'').unwrap();
        let body = after[..end].trim_end_matches("\\n");
        found.push(
            serde_json::from_str(body)
                .unwrap_or_else(|e| panic!("printf example is not JSON ({e}): {body}")),
        );
        rest = &after[end..];
    }
    found
}

fn server() -> McpServer {
    McpServer::new(
        Arc::new(FakeCapturer::new()),
        Arc::new(FakeOcrEngine::with_text("docs")),
    )
}

async fn tool_names() -> Vec<String> {
    let reply = server()
        .handle(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
        .await
        .unwrap();
    let value: Value = serde_json::from_str(&reply).unwrap();
    value["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect()
}

/// MCP-06, WEB-09: every tool the server lists is documented in both
/// places, and both say how many there are.
#[tokio::test]
async fn every_tool_is_documented_on_both_pages() {
    let tools = tool_names().await;
    assert_eq!(tools.len(), 10);
    for (name, doc) in [
        ("docs/MCP.md", repo_file("docs/MCP.md")),
        ("site/mcp.html", repo_file("site/mcp.html")),
    ] {
        for tool in &tools {
            assert!(
                doc.contains(tool.as_str()),
                "{name} never mentions `{tool}`"
            );
        }
    }
}

/// MCP-40, WEB-09: every documented example request is accepted (any
/// tool failure with the fake backends is a tool result, never a
/// protocol or argument error).
#[tokio::test]
async fn documented_requests_are_accepted() {
    let mut requests: Vec<(String, Value)> = json_examples()
        .into_iter()
        .filter(|(_, value)| value.get("method").is_some())
        .collect();
    for (name, text) in [
        ("docs/MCP.md", repo_file("docs/MCP.md")),
        ("site/mcp.html", unescape_html(&repo_file("site/mcp.html"))),
    ] {
        for value in printf_requests(&text) {
            requests.push((name.to_string(), value));
        }
    }
    assert!(
        requests.len() >= 8,
        "found only {} requests",
        requests.len()
    );

    let server = server();
    for (source, request) in requests {
        let reply = server.handle(&request.to_string()).await;
        let reply: Value = serde_json::from_str(&reply.expect("requests have ids")).unwrap();
        assert!(
            reply.get("error").is_none(),
            "{source}: documented request {request} was refused: {reply}"
        );
        assert!(reply.get("result").is_some(), "{source}: {reply}");
    }
}

/// MCP-40: every documented host config starts the `readshot` server
/// with a `readshot-mcp` command, in whichever shape that host uses.
#[test]
fn documented_host_configs_launch_readshot_mcp() {
    let mut configs = 0;
    for (source, value) in json_examples() {
        // Other JSON blocks are requests, results and argument shapes.
        let Some(servers) = value.get("mcpServers").or_else(|| value.get("servers")) else {
            continue;
        };
        let readshot = match servers {
            Value::Object(map) => map.get("readshot"),
            Value::Array(list) => list.iter().find(|s| s["name"] == "readshot"),
            _ => None,
        }
        .unwrap_or_else(|| panic!("{source}: no `readshot` server in {value}"));
        let command = readshot["command"].as_str().unwrap();
        assert!(
            command == "readshot-mcp" || command.ends_with("/readshot-mcp"),
            "{source}: {command}"
        );
        if let Some(transport) = readshot.get("type").or_else(|| readshot.get("transport")) {
            assert_eq!(transport, "stdio", "{source}");
        }
        configs += 1;
    }
    assert!(configs >= 4, "found only {configs} host configs");
}
