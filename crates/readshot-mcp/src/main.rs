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
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
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

/// Maximum bytes buffered for a single JSON-RPC request line. The input
/// source is an untrusted MCP host/agent, and every legitimate request to
/// this server is tiny (a display id, a rect, a short language list) —
/// base64 image data is only ever *output*. 8 MiB is far past any real
/// request and exists purely to bound memory: without it a client could
/// stream gigabytes on one newline-less line and OOM the server.
const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;

/// Outcome of reading one newline-delimited line with a size bound.
#[derive(Debug, PartialEq, Eq)]
enum LineRead {
    /// A complete line within the cap (trailing `\n`/`\r\n` stripped).
    Line,
    /// End of stream with no further bytes.
    Eof,
    /// The line exceeded the cap; its bytes were drained to the next
    /// newline (so the stream stays in sync) but never buffered.
    Overflow,
}

/// Read one `\n`-terminated line into `out`, buffering at most `cap`
/// bytes. Reading from a [`BufReader`] keeps this cheap despite the
/// byte-at-a-time loop (each read is served from the in-memory buffer,
/// not a syscall). Over-long lines are drained without being stored, so a
/// malicious client cannot exhaust memory and the stream resynchronises
/// to the next request.
async fn read_line_capped<R>(
    reader: &mut R,
    out: &mut Vec<u8>,
    cap: usize,
) -> std::io::Result<LineRead>
where
    R: tokio::io::AsyncRead + Unpin,
{
    out.clear();
    let mut byte = [0u8; 1];
    let mut overflow = false;
    let mut saw_any = false;
    loop {
        if reader.read(&mut byte).await? == 0 {
            if !saw_any {
                return Ok(LineRead::Eof);
            }
            return Ok(if overflow {
                LineRead::Overflow
            } else {
                LineRead::Line
            });
        }
        saw_any = true;
        if byte[0] == b'\n' {
            if overflow {
                return Ok(LineRead::Overflow);
            }
            if out.last() == Some(&b'\r') {
                out.pop();
            }
            return Ok(LineRead::Line);
        }
        if out.len() < cap {
            out.push(byte[0]);
        } else {
            // Past the cap: stop storing, keep draining to the newline.
            overflow = true;
        }
    }
}

async fn serve_stdio(server: McpServer) -> std::io::Result<()> {
    let mut reader = BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();

    tracing::info!(
        target: "readshot::mcp",
        "readshot-mcp {version} ready on stdio",
        version = env!("CARGO_PKG_VERSION"),
    );

    let mut buf: Vec<u8> = Vec::new();
    loop {
        match read_line_capped(&mut reader, &mut buf, MAX_LINE_BYTES).await? {
            LineRead::Eof => break,
            LineRead::Overflow => {
                tracing::warn!(
                    target: "readshot::mcp",
                    "rejected oversized request line (> {MAX_LINE_BYTES} bytes)"
                );
                // Reply with a JSON-RPC parse error but keep serving.
                let reply = r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error: request line exceeds maximum size"}}"#;
                stdout.write_all(reply.as_bytes()).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
            }
            LineRead::Line => {
                let line = String::from_utf8_lossy(&buf);
                if let Some(reply) = server.handle(&line).await {
                    stdout.write_all(reply.as_bytes()).await?;
                    stdout.write_all(b"\n").await?;
                    stdout.flush().await?;
                }
            }
        }
    }

    Ok(())
}

fn default_history_root() -> Option<std::path::PathBuf> {
    directories::ProjectDirs::from("np.com", "pawanpaudel", "Readshot")
        .map(|d| d.data_local_dir().join("history"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufReader;

    #[tokio::test]
    async fn reads_consecutive_lines_then_eof() {
        let data = b"hello\nworld\n";
        let mut reader = BufReader::new(&data[..]);
        let mut buf = Vec::new();

        assert_eq!(
            read_line_capped(&mut reader, &mut buf, 1024).await.unwrap(),
            LineRead::Line
        );
        assert_eq!(buf, b"hello");
        assert_eq!(
            read_line_capped(&mut reader, &mut buf, 1024).await.unwrap(),
            LineRead::Line
        );
        assert_eq!(buf, b"world");
        assert_eq!(
            read_line_capped(&mut reader, &mut buf, 1024).await.unwrap(),
            LineRead::Eof
        );
    }

    #[tokio::test]
    async fn strips_trailing_carriage_return() {
        let data = b"crlf\r\n";
        let mut reader = BufReader::new(&data[..]);
        let mut buf = Vec::new();
        assert_eq!(
            read_line_capped(&mut reader, &mut buf, 1024).await.unwrap(),
            LineRead::Line
        );
        assert_eq!(buf, b"crlf");
    }

    #[tokio::test]
    async fn rejects_oversized_line_without_buffering_and_resyncs() {
        // A 5000-byte line under a 1024 cap must not buffer past the cap,
        // must report Overflow, and must leave the stream positioned at
        // the next line.
        let mut data = vec![b'a'; 5000];
        data.push(b'\n');
        data.extend_from_slice(b"ok\n");
        let mut reader = BufReader::new(&data[..]);
        let mut buf = Vec::new();

        assert_eq!(
            read_line_capped(&mut reader, &mut buf, 1024).await.unwrap(),
            LineRead::Overflow
        );
        assert!(
            buf.len() <= 1024,
            "buffered {} bytes, cap was 1024",
            buf.len()
        );

        assert_eq!(
            read_line_capped(&mut reader, &mut buf, 1024).await.unwrap(),
            LineRead::Line
        );
        assert_eq!(buf, b"ok");
    }
}
