# Wiring `readshot-mcp` into AI agent hosts

The `readshot-mcp` binary speaks the
[Model Context Protocol](https://modelcontextprotocol.io) over stdio.
Any agent that supports MCP stdio servers can use it to ask Readshot
for screenshots and OCR text on the user's behalf.

This document gives copy-paste configuration snippets for the most
common hosts. Every snippet uses the absolute installed binary path
— adjust to wherever Readshot landed on your machine.

## Locate the binary

| OS       | Default install path                                   |
|----------|--------------------------------------------------------|
| macOS    | `/Applications/Readshot.app/Contents/MacOS/readshot-mcp` |
| Windows  | Not packaged in official releases yet; build from source. |
| Linux    | Not packaged in official releases yet; build from source. |

If you built from source, it's at
`./target/release/readshot-mcp` after `cargo build --release -p readshot-mcp`.

## Tools exposed

| Tool                       | Inputs                                          | Output                            |
|----------------------------|-------------------------------------------------|-----------------------------------|
| `list_displays`            | (none)                                          | `{ displays: [...] }`             |
| `list_windows`             | (none)                                          | `{ windows: [...] }`              |
| `capture_region`           | `display?`, `rect?`, `scale?`, `hide_cursor?`   | `{ image_base64 }` (PNG)          |
| `capture_window`           | `window`, `rect?`                               | `{ image_base64 }` (PNG)          |
| `capture_text`             | …same plus `languages`, `language_correction`   | `{ text, average_confidence }`    |
| `capture_region_and_text`  | …same                                           | `{ image_base64, text, … }`       |
| `recent_captures`          | `limit?`                                        | `{ captures: [...] }`             |
| `latest_capture`           | (none)                                          | `{ capture: {...} }`              |
| `get_capture`              | `id`                                            | `{ capture: {...} }`              |
| `search_captures`          | `query`, `limit?`                               | `{ captures: [...] }`             |

When `display` is omitted, the primary display is used. When `rect`
is omitted, the chosen display's full bounds are captured.

Use `list_windows` to find capturable app windows. Pass the returned
`id` as `window` to `capture_window`. A `capture_window.rect` is
relative to that window's top-left, not to the full display.

The history tools read the same local archive as the GUI. Each capture
result includes metadata, OCR text when available, and the absolute
PNG path.

For agents that only need text, use `capture_text`: it captures the
requested region, runs OCR, and returns text without an image payload.
For agents that need both pixels and OCR, use `capture_region_and_text`.
The GUI equivalent of text-only capture is the overlay toolbar's
**Copy Text** button.

Use `latest_capture` when an agent needs the newest saved capture
without guessing a limit. Use `get_capture` after `recent_captures` or
`search_captures` when the agent already has a capture id and needs
the full metadata again.

A full example tool call from the agent's side:

```jsonc
{
  "jsonrpc": "2.0",
  "id": 7,
  "method": "tools/call",
  "params": {
    "name": "capture_region_and_text",
    "arguments": {
      "rect": { "x": 100, "y": 100, "width": 800, "height": 600 },
      "languages": ["en-US"]
    }
  }
}
```

Capture one window:

```jsonc
{
  "jsonrpc": "2.0",
  "id": 8,
  "method": "tools/call",
  "params": {
    "name": "capture_window",
    "arguments": {
      "window": "11474",
      "rect": { "x": 100, "y": 100, "width": 800, "height": 500 }
    }
  }
}
```

## Permissions reminder

`readshot-mcp` does **not** prompt for Screen Recording itself — it
inherits the launching agent's TCC / portal grants.

* **macOS:** the agent app (Claude Desktop, Cursor, …) must have
  Screen Recording permission in *System Settings → Privacy &
  Security → Screen Recording*.
* **Linux/Wayland:** the first capture pops the
  `xdg-desktop-portal-screencast` consent dialog inside the agent's
  process. Click *Allow*.
* **Windows:** no permission prompt — captures work as soon as the
  agent launches the binary.

If a capture fails for permission reasons, the server returns a
JSON-RPC error with `code = -32001` and a `message` you can show to
the user.

---

## Claude Desktop

Claude Desktop reads `~/Library/Application Support/Claude/claude_desktop_config.json`
on macOS, `%APPDATA%\Claude\claude_desktop_config.json` on Windows.

Add a `mcpServers` entry:

```json
{
  "mcpServers": {
    "readshot": {
      "command": "/Applications/Readshot.app/Contents/MacOS/readshot-mcp"
    }
  }
}
```

Restart Claude Desktop. The `readshot` MCP server should appear in
the tool list with the tools above.

## Cursor

Cursor reads `~/.cursor/mcp.json` (project-level overrides live at
`.cursor/mcp.json` next to the project root).

```json
{
  "mcpServers": {
    "readshot": {
      "command": "/Applications/Readshot.app/Contents/MacOS/readshot-mcp",
      "type": "stdio"
    }
  }
}
```

After saving, run *Cursor → Settings → MCP → Reload* (or restart
Cursor). The tools become available inside the agent panel.

## OpenAI desktop / Codex CLI

OpenAI's desktop app and the `codex` CLI both honour an MCP config
in `~/.codex/config.toml`:

```toml
[mcp_servers.readshot]
command = "/Applications/Readshot.app/Contents/MacOS/readshot-mcp"
```

…or as JSON in `~/.openai/mcp.json`:

```json
{
  "servers": {
    "readshot": {
      "command": "/Applications/Readshot.app/Contents/MacOS/readshot-mcp",
      "transport": "stdio"
    }
  }
}
```

The CLI logs the server's stderr to `~/.codex/log/readshot.log` —
useful for debugging permission issues.

## Continue.dev

Add to `~/.continue/config.json`:

```json
{
  "mcpServers": [
    {
      "name": "readshot",
      "command": "/Applications/Readshot.app/Contents/MacOS/readshot-mcp"
    }
  ]
}
```

## Generic MCP client (any other host)

Any MCP-compatible host needs three pieces:

1. **Command:** absolute path to `readshot-mcp`.
2. **Transport:** stdio (newline-delimited JSON-RPC 2.0).
3. **Working directory** *(optional)*: not required — the server keeps
   no state on disk.

The server advertises protocol version `2024-11-05` and a single
`tools` capability. No prompts, resources, or sampling.

## Verifying it works

A one-shot smoke test that doesn't need an agent:

```bash
printf '{"jsonrpc":"2.0","id":1,"method":"tools/list"}\n' \
  | /Applications/Readshot.app/Contents/MacOS/readshot-mcp \
  | python3 -m json.tool
```

You should see the tool descriptors with their JSON Schemas. If
the binary errors out with `Library not loaded: @rpath/libswift_Concurrency.dylib`,
it was built against a Swift toolchain that isn't installed; reinstall
Readshot from the official DMG or rebuild from source on the target
machine.

## Troubleshooting

| Symptom                                         | Likely cause                              |
|-------------------------------------------------|-------------------------------------------|
| `command not found`                             | Wrong path; check the install location.   |
| Captures return `code = -32001 capture: permission denied` | Host process lacks Screen Recording grant. |
| Empty `displays` array                          | Wayland portal denied or no monitors.     |
| `tools/list` shows no tools                     | Agent connected to a different binary.    |
| OCR returns empty text                          | Region too small or no text rendered.     |

When in doubt, run the smoke test above first — if that returns the
tool list, the binary is fine and the issue is in the agent host's
configuration.
