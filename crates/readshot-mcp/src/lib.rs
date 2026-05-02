//! Readshot MCP server (Task 19).
//!
//! Implements a minimal Model Context Protocol stdio server so AI
//! agents (Claude Desktop, Cursor, OpenAI desktop, etc.) can ask
//! Readshot to list displays, capture screenshots, and run OCR.
//!
//! ## Wire protocol
//!
//! MCP stdio transport is newline-delimited JSON-RPC 2.0: each
//! message is one line of UTF-8 JSON terminated by `\n`. Servers
//! must not write anything else to stdout. Logging goes to stderr.
//!
//! ## Methods implemented
//!
//! | Method            | Purpose                                       |
//! |-------------------|-----------------------------------------------|
//! | `initialize`      | Negotiate protocol version + advertise tools  |
//! | `tools/list`      | Return the Readshot tool descriptors         |
//! | `tools/call`      | Dispatch a tool by name                       |
//! | `ping`            | Health-check (returns `{}`)                   |
//! | `shutdown`        | Acknowledge an orderly shutdown               |
//!
//! Other JSON-RPC methods return method-not-found per spec so
//! agents can probe the server safely.
//!
//! ## Tools
//!
//! | Tool                       | Inputs                       | Output                          |
//! |----------------------------|------------------------------|---------------------------------|
//! | `list_displays`            | (none)                       | `[{ id, name, bounds, ... }]`   |
//! | `capture_region`           | display?, rect?, scale?      | `{ image_base64 }` (PNG)        |
//! | `capture_text`             | display?, rect?, scale?, ... | `{ text }`                      |
//! | `capture_region_and_text`  | display?, rect?, scale?, ... | `{ image_base64, text }`        |
//! | `recent_captures`          | limit?                       | recent history records          |
//! | `search_captures`          | query, limit?                | matching history records        |
//!
//! All `capture_*` tools accept an optional `display` (id from
//! `list_displays`) and an optional `rect = {x,y,width,height}`. If
//! either is omitted, the server defaults to the primary display
//! and its full bounds — same defaults as the CLI.
//!
//! ## Permissions
//!
//! The MCP server inherits the host process's TCC / portal grants.
//! On macOS, agents must launch `readshot-mcp` from a Terminal /
//! launcher that already has Screen Recording permission, or grant
//! the agent's parent application directly. Failed captures surface
//! as JSON-RPC errors with `code = -32001` (server error) and a
//! human-readable `message` so the agent can prompt the user.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use image::RgbaImage;
use readshot_capture::{CaptureRequest, Capturer, DisplayInfo};
use readshot_core::error::{CaptureError, HistoryError, OCRError};
use readshot_core::geom::Rect;
use readshot_core::{CaptureRecord, HistoryStore};
use readshot_ocr::{OCREngine, OCRRequest};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// MCP protocol version we advertise. The agent picks the highest
/// version both sides accept; older versions are still permitted.
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// JSON-RPC 2.0 request envelope. We only deserialize the fields we
/// care about; extra members are ignored.
#[derive(Debug, Deserialize)]
struct RpcRequest {
    jsonrpc: Option<String>,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

/// JSON-RPC 2.0 response envelope.
#[derive(Debug, Serialize)]
#[serde(untagged)]
enum RpcResponseBody {
    Result {
        jsonrpc: &'static str,
        id: Value,
        result: Value,
    },
    Error {
        jsonrpc: &'static str,
        id: Value,
        error: RpcError,
    },
}

#[derive(Debug, Serialize)]
struct RpcError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

/// Standard JSON-RPC 2.0 error codes plus an MCP-specific one for
/// server errors raised inside a tool call.
mod codes {
    pub const PARSE_ERROR: i32 = -32700;
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const SERVER_ERROR: i32 = -32001;
}

/// The MCP server holds the same service handles the GUI and CLI use
/// — just bound to JSON-RPC instead of clap. Cloneable via `Arc` so
/// the stdio runner can spawn per-message tasks if it wants to.
#[derive(Clone)]
pub struct McpServer {
    capturer: Arc<dyn Capturer>,
    ocr: Arc<dyn OCREngine>,
    history: Option<HistoryArchive>,
}

#[derive(Clone)]
struct HistoryArchive {
    store: Arc<dyn HistoryStore>,
    root: Option<PathBuf>,
}

impl McpServer {
    pub fn new(capturer: Arc<dyn Capturer>, ocr: Arc<dyn OCREngine>) -> Self {
        Self {
            capturer,
            ocr,
            history: None,
        }
    }

    pub fn with_history(
        capturer: Arc<dyn Capturer>,
        ocr: Arc<dyn OCREngine>,
        history: Arc<dyn HistoryStore>,
    ) -> Self {
        Self {
            capturer,
            ocr,
            history: Some(HistoryArchive {
                store: history,
                root: None,
            }),
        }
    }

    pub fn with_history_root(
        capturer: Arc<dyn Capturer>,
        ocr: Arc<dyn OCREngine>,
        history: Arc<dyn HistoryStore>,
        history_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            capturer,
            ocr,
            history: Some(HistoryArchive {
                store: history,
                root: Some(history_root.into()),
            }),
        }
    }

    /// Handle one JSON-RPC frame. Returns `Some(response_line)` for
    /// every request that has an `id`, and `None` for notifications
    /// (no `id`). The returned string does **not** include a trailing
    /// newline — callers add the framing.
    pub async fn handle(&self, line: &str) -> Option<String> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }

        let req: RpcRequest = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(e) => {
                return Some(error_response(
                    Value::Null,
                    codes::PARSE_ERROR,
                    e.to_string(),
                ))
            }
        };

        // Notifications carry no `id`; we still process them but
        // never emit a reply (per JSON-RPC 2.0 §4.1).
        let id = req.id.clone();

        if req.jsonrpc.as_deref() != Some("2.0") {
            return id.map(|id| {
                error_response(id, codes::INVALID_REQUEST, "jsonrpc must be \"2.0\"".into())
            });
        }

        let response = self.dispatch(&req.method, req.params).await;

        match response {
            Ok(value) => id.map(|id| ok_response(id, value)),
            Err(err) => id.map(|id| error_response(id, err.code, err.message)),
        }
    }

    async fn dispatch(&self, method: &str, params: Value) -> Result<Value, RpcErr> {
        match method {
            "initialize" => Ok(self.initialize_response()),
            "ping" | "shutdown" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tool_descriptors() })),
            "tools/call" => self.tool_call(params).await,
            _ => Err(RpcErr {
                code: codes::METHOD_NOT_FOUND,
                message: format!("method `{method}` not found"),
            }),
        }
    }

    fn initialize_response(&self) -> Value {
        json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "serverInfo": {
                "name": "readshot-mcp",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "capabilities": {
                "tools": { "listChanged": false },
            },
        })
    }

    async fn tool_call(&self, params: Value) -> Result<Value, RpcErr> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcErr {
                code: codes::INVALID_PARAMS,
                message: "tools/call missing `name`".into(),
            })?;
        let args = params.get("arguments").cloned().unwrap_or(Value::Null);

        let payload = match name {
            "list_displays" => self.tool_list_displays().await?,
            "capture_region" => self.tool_capture_region(&args).await?,
            "capture_text" => self.tool_capture_text(&args).await?,
            "capture_region_and_text" => self.tool_capture_region_and_text(&args).await?,
            "recent_captures" => self.tool_recent_captures(&args)?,
            "search_captures" => self.tool_search_captures(&args)?,
            other => {
                return Err(RpcErr {
                    code: codes::METHOD_NOT_FOUND,
                    message: format!("unknown tool `{other}`"),
                });
            }
        };

        // MCP requires tool responses to wrap their content in a
        // `content` array of typed parts. We always emit a single
        // `text` part containing the JSON payload — clients parse
        // it back when they want the structured result.
        Ok(json!({
            "content": [
                { "type": "text", "text": payload.to_string() }
            ],
            "isError": false,
            // Convenience: also surface the structured payload at
            // the top level for clients that prefer it. Optional per
            // MCP spec; agents that ignore unknown fields are safe.
            "structuredContent": payload,
        }))
    }

    fn history(&self) -> Result<&HistoryArchive, RpcErr> {
        self.history.as_ref().ok_or_else(|| RpcErr {
            code: codes::SERVER_ERROR,
            message: "history store unavailable".into(),
        })
    }

    fn tool_recent_captures(&self, args: &Value) -> Result<Value, RpcErr> {
        let limit = parse_limit(args, 20)?;
        let history = self.history()?;
        let records = history.store.list().map_err(history_to_rpc)?;
        Ok(json!({
            "captures": records
                .into_iter()
                .take(limit)
                .map(|record| record_to_json(&record, history.root.as_deref()))
                .collect::<Vec<_>>()
        }))
    }

    fn tool_search_captures(&self, args: &Value) -> Result<Value, RpcErr> {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .ok_or_else(|| RpcErr {
                code: codes::INVALID_PARAMS,
                message: "search_captures requires a non-empty `query`".into(),
            })?;
        let needle = query.to_lowercase();
        let limit = parse_limit(args, 20)?;
        let history = self.history()?;
        let records = history.store.list().map_err(history_to_rpc)?;
        Ok(json!({
            "captures": records
                .into_iter()
                .filter(|record| record_matches(record, &needle))
                .take(limit)
                .map(|record| record_to_json(&record, history.root.as_deref()))
                .collect::<Vec<_>>()
        }))
    }

    async fn tool_list_displays(&self) -> Result<Value, RpcErr> {
        let displays = self
            .capturer
            .list_displays()
            .await
            .map_err(capture_to_rpc)?;
        Ok(json!({ "displays": displays.iter().map(display_to_json).collect::<Vec<_>>() }))
    }

    async fn tool_capture_region(&self, args: &Value) -> Result<Value, RpcErr> {
        let req = self.build_request(args).await?;
        let img = self
            .capturer
            .capture_region(req)
            .await
            .map_err(capture_to_rpc)?;
        Ok(json!({ "image_base64": encode_png(&img)? }))
    }

    async fn tool_capture_text(&self, args: &Value) -> Result<Value, RpcErr> {
        let req = self.build_request(args).await?;
        let img = self
            .capturer
            .capture_region(req)
            .await
            .map_err(capture_to_rpc)?;
        let result = self
            .ocr
            .recognise(ocr_request_from(args, img))
            .await
            .map_err(ocr_to_rpc)?;
        Ok(json!({
            "text": result.text,
            "average_confidence": result.average_confidence,
        }))
    }

    async fn tool_capture_region_and_text(&self, args: &Value) -> Result<Value, RpcErr> {
        let req = self.build_request(args).await?;
        let img = self
            .capturer
            .capture_region(req)
            .await
            .map_err(capture_to_rpc)?;
        let png = encode_png(&img)?;
        let result = self
            .ocr
            .recognise(ocr_request_from(args, img))
            .await
            .map_err(ocr_to_rpc)?;
        Ok(json!({
            "image_base64": png,
            "text": result.text,
            "average_confidence": result.average_confidence,
        }))
    }

    async fn build_request(&self, args: &Value) -> Result<CaptureRequest, RpcErr> {
        let displays = self
            .capturer
            .list_displays()
            .await
            .map_err(capture_to_rpc)?;
        if displays.is_empty() {
            return Err(RpcErr {
                code: codes::SERVER_ERROR,
                message: "no displays available".into(),
            });
        }
        let chosen = match args.get("display").and_then(Value::as_str) {
            Some(id) => displays.iter().find(|d| d.id == id).ok_or_else(|| RpcErr {
                code: codes::INVALID_PARAMS,
                message: format!("display `{id}` not found"),
            })?,
            None => displays
                .iter()
                .find(|d| d.is_primary)
                .unwrap_or(&displays[0]),
        };

        let rect = match args.get("rect") {
            Some(v) => parse_rect_value(v)?,
            None => chosen.bounds,
        };
        let scale = args
            .get("scale")
            .and_then(Value::as_f64)
            .map(|f| f as f32)
            .unwrap_or(chosen.scale);
        let hide_cursor = args
            .get("hide_cursor")
            .and_then(Value::as_bool)
            .unwrap_or(true);

        Ok(CaptureRequest {
            display_id: chosen.id.clone(),
            rect,
            scale,
            hide_cursor,
        })
    }
}

fn ocr_request_from(args: &Value, image: RgbaImage) -> OCRRequest {
    let languages = args
        .get("languages")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let use_language_correction = args
        .get("language_correction")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    OCRRequest {
        image,
        languages,
        use_language_correction,
    }
}

fn parse_rect_value(v: &Value) -> Result<Rect, RpcErr> {
    let obj = v.as_object().ok_or_else(|| RpcErr {
        code: codes::INVALID_PARAMS,
        message: "rect must be an object {x,y,width,height}".into(),
    })?;
    let field = |k: &str| -> Result<f32, RpcErr> {
        obj.get(k)
            .and_then(Value::as_f64)
            .map(|f| f as f32)
            .ok_or_else(|| RpcErr {
                code: codes::INVALID_PARAMS,
                message: format!("rect.{k} missing or not a number"),
            })
    };
    let (x, y, w, h) = (field("x")?, field("y")?, field("width")?, field("height")?);
    Rect::from_xywh(x, y, w, h).ok_or_else(|| RpcErr {
        code: codes::INVALID_PARAMS,
        message: "rect has non-positive or non-finite dimensions".into(),
    })
}

fn encode_png(img: &RgbaImage) -> Result<String, RpcErr> {
    let mut buf = Vec::with_capacity(img.as_raw().len() / 4);
    let mut cursor = std::io::Cursor::new(&mut buf);
    img.write_to(&mut cursor, image::ImageFormat::Png)
        .map_err(|e| RpcErr {
            code: codes::SERVER_ERROR,
            message: format!("PNG encode failed: {e}"),
        })?;
    Ok(B64.encode(&buf))
}

fn display_to_json(d: &DisplayInfo) -> Value {
    json!({
        "id": d.id,
        "name": d.name,
        "is_primary": d.is_primary,
        "scale": d.scale,
        "bounds": {
            "x": d.bounds.x(),
            "y": d.bounds.y(),
            "width": d.bounds.width(),
            "height": d.bounds.height(),
        }
    })
}

fn parse_limit(args: &Value, default: usize) -> Result<usize, RpcErr> {
    match args.get("limit") {
        None => Ok(default),
        Some(v) => {
            let n = v.as_u64().ok_or_else(|| RpcErr {
                code: codes::INVALID_PARAMS,
                message: "limit must be a positive integer".into(),
            })?;
            if n == 0 {
                return Err(RpcErr {
                    code: codes::INVALID_PARAMS,
                    message: "limit must be a positive integer".into(),
                });
            }
            Ok((n as usize).min(100))
        }
    }
}

fn record_matches(record: &CaptureRecord, needle: &str) -> bool {
    if let Some(ocr) = record.ocr_text.as_deref() {
        if ocr.to_lowercase().contains(needle) {
            return true;
        }
    }
    record
        .captured_at
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
        .to_lowercase()
        .contains(needle)
}

fn record_to_json(record: &CaptureRecord, history_root: Option<&Path>) -> Value {
    let image_path = history_root.map(|root| history_png_path(root, record));
    json!({
        "id": record.id.to_string(),
        "captured_at": record.captured_at.to_rfc3339(),
        "width_px": record.width_px,
        "height_px": record.height_px,
        "display_id": record.display_id,
        "ocr_text": record.ocr_text,
        "image_path": image_path.map(|p| p.to_string_lossy().to_string()),
    })
}

fn history_png_path(root: &Path, record: &CaptureRecord) -> PathBuf {
    root.join(record.captured_at.format("%Y").to_string())
        .join(record.captured_at.format("%m").to_string())
        .join(format!("{}.png", record.id))
}

fn capture_to_rpc(e: CaptureError) -> RpcErr {
    let code = match e {
        CaptureError::PermissionDenied => codes::SERVER_ERROR,
        _ => codes::SERVER_ERROR,
    };
    RpcErr {
        code,
        message: format!("capture: {e}"),
    }
}

fn ocr_to_rpc(e: OCRError) -> RpcErr {
    RpcErr {
        code: codes::SERVER_ERROR,
        message: format!("ocr: {e}"),
    }
}

fn history_to_rpc(e: HistoryError) -> RpcErr {
    RpcErr {
        code: codes::SERVER_ERROR,
        message: format!("history: {e}"),
    }
}

fn ok_response(id: Value, result: Value) -> String {
    serde_json::to_string(&RpcResponseBody::Result {
        jsonrpc: "2.0",
        id,
        result,
    })
    .expect("serializing rpc result cannot fail")
}

fn error_response(id: Value, code: i32, message: String) -> String {
    serde_json::to_string(&RpcResponseBody::Error {
        jsonrpc: "2.0",
        id,
        error: RpcError {
            code,
            message,
            data: None,
        },
    })
    .expect("serializing rpc error cannot fail")
}

struct RpcErr {
    code: i32,
    message: String,
}

/// Static tool descriptors returned by `tools/list`. The schemas are
/// JSON Schema fragments; agents use them to validate arguments
/// before calling the tool.
fn tool_descriptors() -> Value {
    let rect_schema = json!({
        "type": "object",
        "properties": {
            "x": { "type": "number" },
            "y": { "type": "number" },
            "width": { "type": "number" },
            "height": { "type": "number" }
        },
        "required": ["x", "y", "width", "height"]
    });
    let capture_args = json!({
        "type": "object",
        "properties": {
            "display": { "type": "string", "description": "Display id from list_displays. Defaults to primary." },
            "rect": rect_schema.clone(),
            "scale": { "type": "number", "description": "HiDPI scale; default = display's reported scale." },
            "hide_cursor": { "type": "boolean", "default": true },
        },
        "additionalProperties": false
    });
    let capture_text_args = json!({
        "type": "object",
        "properties": {
            "display": { "type": "string" },
            "rect": rect_schema,
            "scale": { "type": "number" },
            "hide_cursor": { "type": "boolean", "default": true },
            "languages": { "type": "array", "items": { "type": "string" }, "description": "BCP-47 codes" },
            "language_correction": { "type": "boolean", "default": true },
        },
        "additionalProperties": false
    });
    json!([
        {
            "name": "list_displays",
            "description": "List attached displays with id, bounds, and HiDPI scale.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        },
        {
            "name": "capture_region",
            "description": "Capture a region (or full display) and return a base64-encoded PNG.",
            "inputSchema": capture_args
        },
        {
            "name": "capture_text",
            "description": "Capture a region and return the OCR-recognised text.",
            "inputSchema": capture_text_args.clone()
        },
        {
            "name": "capture_region_and_text",
            "description": "Capture a region and return both the PNG and the OCR text.",
            "inputSchema": capture_text_args
        },
        {
            "name": "recent_captures",
            "description": "Return recent saved captures from Readshot history, newest first.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 }
                },
                "additionalProperties": false
            }
        },
        {
            "name": "search_captures",
            "description": "Search saved captures by OCR text or capture timestamp.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "minLength": 1 },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 }
                },
                "required": ["query"],
                "additionalProperties": false
            }
        }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use readshot_capture::fake::FakeCapturer;
    use readshot_core::{CaptureRecord, FsHistoryStore, HistoryStore};
    use readshot_ocr::fake::FakeOcrEngine;
    use serde_json::Value;

    fn server() -> McpServer {
        McpServer::new(
            Arc::new(FakeCapturer::new()),
            Arc::new(FakeOcrEngine::with_text("hello mcp")),
        )
    }

    fn server_with_history(records: Vec<CaptureRecord>) -> McpServer {
        let root = unique_history_root();
        let store = FsHistoryStore::new(&root);
        for record in records {
            store.save(&record, b"png").unwrap();
        }
        McpServer::with_history_root(
            Arc::new(FakeCapturer::new()),
            Arc::new(FakeOcrEngine::with_text("hello mcp")),
            Arc::new(store),
            root,
        )
    }

    fn unique_history_root() -> std::path::PathBuf {
        let mut root = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        root.push(format!("readshot-mcp-test-{}-{nanos}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    fn record(
        id: &str,
        captured_at: &str,
        width_px: u32,
        height_px: u32,
        display_id: &str,
        ocr_text: Option<&str>,
    ) -> CaptureRecord {
        serde_json::from_value(json!({
            "id": id,
            "captured_at": captured_at,
            "width_px": width_px,
            "height_px": height_px,
            "display_id": display_id,
            "ocr_text": ocr_text,
            "annotation_model": [],
        }))
        .unwrap()
    }

    async fn call(server: &McpServer, line: &str) -> Value {
        let raw = server.handle(line).await.expect("expected response");
        serde_json::from_str(&raw).expect("response is JSON")
    }

    #[tokio::test]
    async fn initialize_advertises_protocol_and_tool_capability() {
        let s = server();
        let resp = call(
            &s,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        )
        .await;
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["result"]["protocolVersion"], MCP_PROTOCOL_VERSION);
        assert!(resp["result"]["capabilities"]["tools"].is_object());
        assert_eq!(resp["result"]["serverInfo"]["name"], "readshot-mcp");
    }

    #[tokio::test]
    async fn tools_list_returns_readshot_tools() {
        let s = server();
        let resp = call(&s, r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#).await;
        let tools = resp["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            vec![
                "list_displays",
                "capture_region",
                "capture_text",
                "capture_region_and_text",
                "recent_captures",
                "search_captures"
            ]
        );
    }

    #[tokio::test]
    async fn tools_call_recent_captures_returns_newest_first_limited_records() {
        let s = server_with_history(vec![
            record(
                "00000000-0000-0000-0000-000000000001",
                "2026-01-01T00:00:00Z",
                100,
                50,
                "display-a",
                Some("older receipt"),
            ),
            record(
                "00000000-0000-0000-0000-000000000002",
                "2026-02-01T00:00:00Z",
                300,
                200,
                "display-b",
                Some("newer invoice"),
            ),
        ]);
        let resp = call(
            &s,
            r#"{"jsonrpc":"2.0","id":14,"method":"tools/call","params":{"name":"recent_captures","arguments":{"limit":1}}}"#,
        )
        .await;

        let captures = resp["result"]["structuredContent"]["captures"]
            .as_array()
            .unwrap();
        assert_eq!(captures.len(), 1);
        assert_eq!(captures[0]["id"], "00000000-0000-0000-0000-000000000002");
        assert_eq!(captures[0]["ocr_text"], "newer invoice");
        assert_eq!(captures[0]["width_px"], 300);
        assert_eq!(captures[0]["height_px"], 200);
        assert_eq!(captures[0]["display_id"], "display-b");
        assert!(captures[0]["image_path"]
            .as_str()
            .unwrap()
            .ends_with("2026/02/00000000-0000-0000-0000-000000000002.png"));
    }

    #[tokio::test]
    async fn tools_call_search_captures_matches_ocr_text_case_insensitively() {
        let s = server_with_history(vec![
            record(
                "00000000-0000-0000-0000-000000000003",
                "2026-03-01T00:00:00Z",
                100,
                50,
                "display-a",
                Some("Quarterly Budget Notes"),
            ),
            record(
                "00000000-0000-0000-0000-000000000004",
                "2026-04-01T00:00:00Z",
                100,
                50,
                "display-a",
                Some("unrelated"),
            ),
        ]);
        let resp = call(
            &s,
            r#"{"jsonrpc":"2.0","id":15,"method":"tools/call","params":{"name":"search_captures","arguments":{"query":"budget","limit":10}}}"#,
        )
        .await;

        let captures = resp["result"]["structuredContent"]["captures"]
            .as_array()
            .unwrap();
        assert_eq!(captures.len(), 1);
        assert_eq!(captures[0]["id"], "00000000-0000-0000-0000-000000000003");
        assert_eq!(captures[0]["ocr_text"], "Quarterly Budget Notes");
    }

    #[tokio::test]
    async fn tools_call_history_tool_without_store_returns_server_error() {
        let s = server();
        let resp = call(
            &s,
            r#"{"jsonrpc":"2.0","id":16,"method":"tools/call","params":{"name":"recent_captures","arguments":{}}}"#,
        )
        .await;
        assert_eq!(resp["error"]["code"], codes::SERVER_ERROR);
        assert!(resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("history store unavailable"));
    }

    #[tokio::test]
    async fn tools_call_list_displays_returns_fake_display() {
        let s = server();
        let resp = call(
            &s,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_displays","arguments":{}}}"#,
        )
        .await;
        let structured = &resp["result"]["structuredContent"];
        let displays = structured["displays"].as_array().unwrap();
        assert_eq!(displays.len(), 1);
        assert_eq!(displays[0]["id"], "fake-0");
    }

    #[tokio::test]
    async fn tools_call_capture_region_returns_base64_png() {
        let s = server();
        let resp = call(
            &s,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"capture_region","arguments":{"rect":{"x":0,"y":0,"width":64,"height":64}}}}"#,
        )
        .await;
        let b64 = resp["result"]["structuredContent"]["image_base64"]
            .as_str()
            .unwrap();
        let bytes = B64.decode(b64).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[tokio::test]
    async fn tools_call_capture_text_returns_text() {
        let s = server();
        let resp = call(
            &s,
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"capture_text","arguments":{"rect":{"x":0,"y":0,"width":32,"height":32}}}}"#,
        )
        .await;
        assert_eq!(resp["result"]["structuredContent"]["text"], "hello mcp",);
    }

    #[tokio::test]
    async fn tools_call_capture_region_and_text_returns_both() {
        let s = server();
        let resp = call(
            &s,
            r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"capture_region_and_text","arguments":{"rect":{"x":0,"y":0,"width":32,"height":32}}}}"#,
        )
        .await;
        let payload = &resp["result"]["structuredContent"];
        assert!(payload["image_base64"].is_string());
        assert_eq!(payload["text"], "hello mcp");
    }

    #[tokio::test]
    async fn unknown_method_returns_method_not_found() {
        let s = server();
        let resp = call(&s, r#"{"jsonrpc":"2.0","id":7,"method":"nope/unknown"}"#).await;
        assert_eq!(resp["error"]["code"], codes::METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn unknown_tool_returns_method_not_found() {
        let s = server();
        let resp = call(
            &s,
            r#"{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"nope","arguments":{}}}"#,
        )
        .await;
        assert_eq!(resp["error"]["code"], codes::METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn unknown_display_returns_invalid_params() {
        let s = server();
        let resp = call(
            &s,
            r#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"capture_region","arguments":{"display":"missing"}}}"#,
        )
        .await;
        assert_eq!(resp["error"]["code"], codes::INVALID_PARAMS);
    }

    #[tokio::test]
    async fn malformed_rect_returns_invalid_params() {
        let s = server();
        // Negative dimensions are rejected by Rect::from_xywh, which
        // surfaces as INVALID_PARAMS through `parse_rect_value`.
        let resp = call(
            &s,
            r#"{"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"capture_region","arguments":{"rect":{"x":0,"y":0,"width":-10,"height":-10}}}}"#,
        )
        .await;
        assert_eq!(resp["error"]["code"], codes::INVALID_PARAMS);
    }

    #[tokio::test]
    async fn rect_missing_field_returns_invalid_params() {
        let s = server();
        let resp = call(
            &s,
            r#"{"jsonrpc":"2.0","id":13,"method":"tools/call","params":{"name":"capture_region","arguments":{"rect":{"x":0,"y":0}}}}"#,
        )
        .await;
        assert_eq!(resp["error"]["code"], codes::INVALID_PARAMS);
    }

    #[tokio::test]
    async fn parse_error_returns_minus_32700() {
        let s = server();
        let raw = s.handle("not json").await.unwrap();
        let resp: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(resp["error"]["code"], codes::PARSE_ERROR);
    }

    #[tokio::test]
    async fn missing_jsonrpc_field_returns_invalid_request() {
        let s = server();
        let raw = s.handle(r#"{"id":11,"method":"ping"}"#).await.unwrap();
        let resp: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(resp["error"]["code"], codes::INVALID_REQUEST);
    }

    #[tokio::test]
    async fn notification_yields_no_response() {
        let s = server();
        let resp = s.handle(r#"{"jsonrpc":"2.0","method":"ping"}"#).await;
        assert!(resp.is_none(), "notifications must not produce a reply");
    }

    #[tokio::test]
    async fn empty_line_yields_no_response() {
        let s = server();
        assert!(s.handle("   \n").await.is_none());
        assert!(s.handle("").await.is_none());
    }

    #[tokio::test]
    async fn ping_returns_empty_object() {
        let s = server();
        let resp = call(&s, r#"{"jsonrpc":"2.0","id":12,"method":"ping"}"#).await;
        assert_eq!(resp["result"], serde_json::json!({}));
    }
}
