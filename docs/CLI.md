# Readshot CLI

The `readshot` command is installed with the macOS app. Run `readshot`
with no arguments to launch the GUI, or use a subcommand for scripts.

## Common Commands

```bash
readshot list-displays
readshot list-windows
readshot capture --interactive --output capture.png
readshot capture --delay 2 --format jpg --output capture.jpg
readshot capture-window --window 11474 --clipboard
readshot capture-text --interactive --clipboard
readshot capture-and-ocr --interactive --also-image capture.png
readshot ocr --input capture.png --clipboard
```

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
format selection, and cursor controls where those options apply.

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

Use `readshot --help` or `readshot <command> --help` for the complete
option list.
