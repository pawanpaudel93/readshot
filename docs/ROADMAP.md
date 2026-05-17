# Roadmap

This roadmap tracks near-term screenshot workflow gaps that are useful
for Readshot. It is not a release promise.

## Prioritized

1. **Interactive CLI selection**
   - Add a CLI flag that opens the selection overlay, then returns the
     selected capture to the command.
   - Target commands: `readshot capture --interactive`,
     `readshot capture-text --interactive`, and
     `readshot capture-and-ocr --interactive`.
   - Why: combines GUI selection with scriptable output.

2. **CLI clipboard output**
   - Add clipboard output for image and text commands.
   - Target commands: `readshot capture --clipboard`,
     `readshot capture-window --clipboard`, `readshot capture-text --clipboard`,
     and `readshot ocr --clipboard`.
   - Why: common screenshot workflow and faster sharing.

3. **CLI delay timer**
   - Add `--delay SECONDS` before capture.
   - Target commands: region, display, window, and text capture.
   - Why: captures menus, popovers, tooltips, hover states, and other
     transient UI.

4. **Output format selection**
   - Add `--format png|jpg|tiff|webp` where supported.
   - Keep PNG as the default.
   - Why: PNG is best for screenshots and OCR, but JPG/WebP can be
     useful for smaller files.

5. **Cursor and window shadow polish**
   - Make cursor inclusion explicit with `--show-cursor` /
     `--hide-cursor`.
   - Investigate window shadow controls for `capture-window`.
   - Why: useful for documentation screenshots and bug reports.
