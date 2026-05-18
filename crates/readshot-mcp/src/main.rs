//! readshot-mcp — Model Context Protocol stdio server entry point.
//!
//! Reads newline-delimited JSON-RPC 2.0 requests from stdin, calls
//! into [`readshot_mcp::McpServer`], and writes responses (also
//! one-per-line) to stdout. All logging goes to stderr — stdout is
//! reserved for protocol traffic.
//!
//! Agents launch this binary directly; see `docs/MCP.md` for the
//! per-host configuration snippets (Claude Desktop, Cursor, OpenAI).

use std::sync::Arc;

use readshot_capture::default_capturer;
use readshot_core::{FsHistoryStore, HistoryStore};
use readshot_mcp::McpServer;
use readshot_ocr::default_engine;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::Level;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => {}
        [arg] if arg == "--help" || arg == "-h" => {
            print_help();
            return Ok(());
        }
        [arg] if arg == "--check" => {
            return run_check().await;
        }
        _ => {
            eprintln!("readshot-mcp: unknown argument. Use --help.");
            std::process::exit(64);
        }
    }

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(Level::INFO)
        .with_target(true)
        .init();

    let server = build_server();
    serve_stdio(server).await
}

fn print_help() {
    println!(
        "\
Readshot MCP stdio server.

Usage: readshot-mcp [OPTIONS]

Options:
      --check    Print JSON diagnostics and exit
  -h, --help     Print help
"
    );
}

async fn run_check() -> std::io::Result<()> {
    let history = check_history_root();
    let server = build_server();
    let tools_reply = server
        .handle(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
        .await;
    let tools = tools_reply
        .as_deref()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .and_then(|value| value["result"]["tools"].as_array().map(|tools| tools.len()));
    let tools_ok = tools.is_some_and(|count| count > 0);
    let ok = history.writable && tools_ok;

    let payload = json!({
        "ok": ok,
        "server": "readshot-mcp",
        "version": env!("CARGO_PKG_VERSION"),
        "transport": "stdio",
        "tools": {
            "ok": tools_ok,
            "count": tools.unwrap_or(0),
        },
        "history": {
            "path": history.path,
            "writable": history.writable,
            "error": history.error,
        },
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&payload).expect("diagnostic JSON cannot fail")
    );

    if ok {
        Ok(())
    } else {
        std::process::exit(70);
    }
}

struct HistoryCheck {
    path: Option<String>,
    writable: bool,
    error: Option<String>,
}

fn check_history_root() -> HistoryCheck {
    let Some(root) = default_history_root() else {
        return HistoryCheck {
            path: None,
            writable: false,
            error: Some("could not resolve platform data directory".to_string()),
        };
    };

    let result = std::fs::create_dir_all(&root).and_then(|_| {
        let probe = root.join(".readshot-mcp-check");
        std::fs::write(&probe, b"ok")?;
        std::fs::remove_file(probe)
    });

    HistoryCheck {
        path: Some(root.display().to_string()),
        writable: result.is_ok(),
        error: result.err().map(|err| err.to_string()),
    }
}

fn build_server() -> McpServer {
    let capturer = Arc::from(default_capturer());
    let ocr = Arc::from(default_engine());
    match default_history_root() {
        Some(root) => McpServer::with_history_root(
            capturer,
            ocr,
            Arc::new(FsHistoryStore::new(&root)) as Arc<dyn HistoryStore>,
            root,
        ),
        None => McpServer::new(capturer, ocr),
    }
}

async fn serve_stdio(server: McpServer) -> std::io::Result<()> {
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();
    let mut stdout = tokio::io::stdout();

    tracing::info!(
        target: "readshot::mcp",
        "readshot-mcp {version} ready on stdio",
        version = env!("CARGO_PKG_VERSION"),
    );

    while let Some(line) = reader.next_line().await? {
        if let Some(reply) = server.handle(&line).await {
            stdout.write_all(reply.as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
    }

    Ok(())
}

fn default_history_root() -> Option<std::path::PathBuf> {
    directories::ProjectDirs::from("np.com", "pawanpaudel", "Readshot")
        .map(|d| d.data_local_dir().join("history"))
}
