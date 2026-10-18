# Roadmap

This roadmap tracks near-term screenshot workflow gaps that are useful
for Readshot. It is not a release promise.

## Prioritized

No prioritized screenshot workflow gaps are tracked right now.

## Not yet built

- **Drag out of the editor.** A drag handle in the editor's action bar
  that drags the output straight into another app (a chat, a document,
  Finder). It needs a native drag session (`NSDraggingSession` on
  macOS), which iced does not offer, so it starts with a spike to see
  whether a drag can be started from an iced window at all. Until then
  Copy Image and Save cover the same need. If the spike fails, Readshot
  ships without it.
