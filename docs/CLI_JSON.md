# CLI JSON Output

Use `--json` when a script or agent needs stable machine-readable
output from Readshot.

## Image Capture Metadata

Commands:

```bash
readshot capture --output capture.png --json
readshot capture-window --window 11474 --output window.png --json
readshot capture --clipboard --json
```

Image capture commands cannot write raw image bytes and JSON to stdout
at the same time. With `--json`, pass `--output PATH` or `--clipboard`.

Shape:

```json
{
  "kind": "capture",
  "image": {
    "width": 256,
    "height": 256,
    "format": "png"
  },
  "source": {
    "type": "display",
    "display_id": "fake-0",
    "rect": { "x": 0.0, "y": 0.0, "width": 64.0, "height": 64.0 },
    "scale": 1.0,
    "hide_cursor": true
  },
  "output": "capture.png",
  "clipboard": false
}
```

`kind` is `capture` or `capture-window`.

`source.type` is one of:

| Type | Extra fields |
|------|--------------|
| `display` | `display_id`, `rect`, `scale`, `hide_cursor` |
| `window` | `window_id`, `rect`, `ignore_shadows` |
| `interactive` | `show_cursor` |

When `clipboard` is `true`, `output` is `null`.

## OCR Metadata

Commands:

```bash
readshot capture-text --json
readshot ocr --input capture.png --json
readshot capture-and-ocr --also-image capture.png --json
```

Shape:

```json
{
  "kind": "ocr",
  "text": "recognized text",
  "average_confidence": 0.95,
  "image": {
    "width": 800,
    "height": 600
  },
  "source": null,
  "input": "capture.png",
  "languages": [],
  "language_correction": true,
  "lines": [
    {
      "text": "recognized text",
      "bounds": { "x": 0.1, "y": 0.2, "width": 0.5, "height": 0.04 }
    }
  ]
}
```

`kind` is `ocr`, `capture-text`, or `capture-and-ocr`.

`image` is the captured or input image size when known. `source` is
present for capture-based OCR commands and follows the same source
shape as image capture metadata. `input` is present for `ocr`.

`languages` is the explicit BCP-47 hint list passed to the command. An
empty array means automatic/default language detection.

`lines` contains positioned OCR lines when the platform OCR engine
returns layout data. Bounds are normalized to `[0, 1]` with origin at
the top-left of the image. Engines without positioned output return an
empty array.

## Writing JSON to a File

Text-producing commands use `--output` for the text destination. When
combined with `--json`, that destination receives JSON instead of
plain text:

```bash
readshot ocr --input capture.png --json --output ocr.json
readshot capture-text --json --output capture-text.json
```

For `capture-and-ocr`, `--also-image -` is rejected when `--json` is
set because stdout is reserved for JSON.
