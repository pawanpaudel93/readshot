//! readshot-mcp — Model Context Protocol stdio server entry point.
//!
//! Reads newline-delimited JSON-RPC 2.0 requests from stdin, calls
//! into [`readshot_mcp::McpServer`], and writes responses (also
//! one-per-line) to stdout. All logging goes to stderr — stdout is
//! reserved for protocol traffic.
//!
//! Agents launch this binary directly; see `docs/AGENTS.md` for the
//! per-host configuration snippets (Claude Desktop, Cursor, OpenAI).

use std::sync::Arc;

use readshot_capture::default_capturer;
use readshot_mcp::McpServer;
use readshot_ocr::default_engine;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::Level;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(Level::INFO)
        .with_target(true)
        .init();

    let server = McpServer::new(
        Arc::from(default_capturer()),
        Arc::from(default_engine()),
    );

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
