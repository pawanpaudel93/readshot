# Readshot CLI

The `readshot` command is installed with the macOS app. Run `readshot`
with no arguments to launch the GUI, or use a subcommand for scripts.

## Common Commands

```bash
readshot list-displays
readshot list-displays --json
readshot list-windows
readshot list-windows --json
readshot capture --interactive --output capture.png
readshot capture --delay 2 --format jpg --output capture.jpg
readshot capture --interactive --output capture.png --json
readshot capture-window --window 11474 --clipboard
readshot capture-text --interactive --clipboard
readshot capture-text --interactive --json
readshot capture-and-ocr --interactive --also-image capture.png
readshot ocr --input capture.png --json
readshot mcp-config
readshot completions zsh
```

## Commands

| Command | What it does |
|---------|--------------|
| `readshot` | Launches the GUI. |
| `readshot list-displays` | Lists displays with ids, bounds, and scale. |
| `readshot list-windows` | Lists capturable windows with ids, app names, titles, display ids, and bounds. |
| `readshot capture` | Captures a display region or full display as an image. |
| `readshot capture-window` | Captures a window or window-relative region. |
| `readshot capture-text` | Captures a region and writes OCR text. |
| `readshot capture-and-ocr` | Captures a region, writes OCR text, and can also save the image. |
| `readshot ocr` | Runs OCR on an existing image. |
| `readshot mcp-config` | Prints a ready-to-paste MCP stdio config snippet. |
| `readshot completions` | Generates zsh, bash, fish, PowerShell, or Elvish completions. |

Use `--json` when scripts need stable machine-readable output.
The full JSON output reference is in [CLI_JSON.md](CLI_JSON.md).

## Interactive Capture

`--interactive` opens the same Readshot region selector used by the
app. Drag a region and release to capture immediately. Press `Enter`
for the full display or `Esc` to cancel.

Supported commands:

```bash
readshot capture --interactive --output capture.png
readshot capture-text --interactive
readshot capture-and-ocr --interactive
```

Interactive capture can also be combined with clipboard output,
format selection, delay timers, and cursor controls where those options
apply.

## Targeting Displays and Windows

For display captures, omit `--display` and `--rect` to capture the
primary display. Use `list-displays` to find ids and bounds:

```bash
readshot list-displays --json
readshot capture --display 1 --rect 100,100,800,600 --output region.png
```

Rectangles are logical pixels in `x,y,width,height` form. If you need
to override the display scale used by the backend, pass `--scale`.

For window captures, get the window id first:

```bash
readshot list-windows
readshot capture-window --window 11474 --output window.png
readshot capture-window --window 11474 --rect 40,40,900,600 --output panel.png
```

Window rectangles are relative to that window's top-left corner.

## Image Output

`capture` and `capture-window` write PNG by default. Use `--format`
for other image formats:

```bash
readshot capture --format png --output capture.png
readshot capture --format jpg --output capture.jpg
readshot capture --format tiff --output capture.tiff
readshot capture --format webp --output capture.webp
```

Use `--clipboard` to copy the image instead of writing a file:

```bash
readshot capture --interactive --clipboard
readshot capture-window --window 11474 --clipboard
```

Use `--output -` to write image bytes to stdout. This is the default
when neither `--output` nor `--clipboard` is supplied.

Use `--json` to print capture metadata after saving or copying the
image. Because stdout is used for JSON, image capture commands require
`--output PATH` or `--clipboard` when `--json` is set:

```bash
readshot capture --interactive --output capture.png --json
readshot capture-window --window 11474 --output window.png --json
readshot capture --interactive --clipboard --json
```

## Text Output

`capture-text` captures a region, runs OCR, and writes recognized text.
`ocr` runs OCR on an existing image.

```bash
readshot capture-text --interactive
readshot capture-text --rect 100,100,800,600 --clipboard
readshot ocr --input capture.png --output text.txt
readshot ocr --input capture.png --clipboard
```

OCR uses automatic language detection by default.

Use `--clipboard` to copy text instead of printing it, or `--output` to
write text to a file:

```bash
readshot capture-text --interactive --output text.txt
readshot ocr --input capture.png --clipboard
```

Use `--input -` with `ocr` to read image bytes from stdin:

```bash
cat capture.png | readshot ocr --input -
```

Use `--json` to include OCR metadata such as confidence, image size,
language hints, and positioned OCR lines when the platform engine
returns them:

```bash
readshot capture-text --interactive --json
readshot ocr --input capture.png --json
```

`--languages` accepts comma-separated BCP-47 hints for OCR engines that
support explicit language hints. Leave it empty for automatic detection:

```bash
readshot ocr --input capture.png --languages en-US,fr-FR
```

`--language-correction` is enabled by default where the platform OCR
engine supports it.

## Combined Image and OCR

`capture-and-ocr` prints or writes text and can save the captured image
at the same time:

```bash
readshot capture-and-ocr --interactive --also-image capture.png --output text.txt
readshot capture-and-ocr --rect 100,100,800,600 --also-image capture.png
readshot capture-and-ocr --interactive --also-image capture.png --json
```

## MCP Config

`mcp-config` prints a standard stdio JSON snippet that works with
Claude Desktop, Cursor, and other hosts that accept `mcpServers`
configuration:

```bash
readshot mcp-config
readshot mcp-config --command /Applications/Readshot.app/Contents/MacOS/readshot-mcp
```

The default uses `readshot-mcp`, which is correct when the host inherits
your shell `PATH`. Use the absolute app-bundle path for desktop hosts
that do not inherit `~/.local/bin`.

## Shell Completions

Generate completions from the current clap command tree:

```bash
readshot completions zsh > _readshot
readshot completions bash > readshot.bash
readshot completions fish > readshot.fish
```

Use `readshot completions --help` to see every supported shell.

## Timing, Cursor, and Window Options

Use `--delay SECONDS` when capturing menus, popovers, hover states, or
other transient UI.

```bash
readshot capture --delay 2 --interactive --output menu.png
```

The cursor is hidden by default. Use `--show-cursor` when documenting
pointer state:

```bash
readshot capture --interactive --show-cursor --output pointer.png
```

Window capture supports native shadow control when the backend supports
it:

```bash
readshot capture-window --window 11474 --window-shadow --output window.png
readshot capture-window --window 11474 --no-window-shadow --output tight.png
```

## Output Destinations

| Need | Use |
|------|-----|
| Save an image | `--output capture.png` |
| Print image bytes | `--output -` |
| Copy image | `--clipboard` |
| Save OCR text | `--output text.txt` |
| Print OCR text | `--output -` |
| Copy OCR text | `--clipboard` |
| Save image and OCR text together | `capture-and-ocr --also-image capture.png --output text.txt` |

Use `readshot --help` or `readshot <command> --help` for the complete
option list.
