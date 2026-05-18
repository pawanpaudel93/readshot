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
readshot capture-window --window 11474 --clipboard
readshot capture-text --interactive --clipboard
readshot capture-and-ocr --interactive --also-image capture.png
readshot ocr --input capture.png --clipboard
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

Use `--json` with `list-displays` or `list-windows` when scripts need
stable machine-readable output.

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
```

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
