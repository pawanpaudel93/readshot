//! Recognized text the way the editor's Text panel uses it:
//! lines in reading order with boxes in **base-image pixels** when the
//! engine reports positions, plus the copy and smart-highlighter
//! geometry built on them. Pure; no UI.

use std::ops::Range;

use crate::geom::{PointLike, RectLike};
use crate::ocr_layout::{reading_order, reconstruct, RecognizedLine};
use crate::ocr_text::clean;

/// A word box in base-image pixels. `start..end` are char indices.
#[derive(Clone, Debug, PartialEq)]
pub struct WordRect {
    pub start: usize,
    pub end: usize,
    pub rect: RectLike,
}

/// One row of the Text panel.
#[derive(Clone, Debug, PartialEq)]
pub struct TextLine {
    pub text: String,
    /// `None` when the engine reports no positions (Windows, Linux).
    pub rect: Option<RectLike>,
    pub words: Vec<WordRect>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextDocument {
    pub lines: Vec<TextLine>,
    /// Every line has a box. Gates hover-to-locate, smart redact and
    /// the smart highlighter.
    pub positioned: bool,
    /// Layout-aware text (`reconstruct` + `clean`), exactly what Copy
    /// Text produced before the panel existed.
    pub layout_text: String,
}

/// Normalised box → base pixels, clamped to the image: engines can report
/// boxes that overhang the edge by a hair.
fn to_pixels(x: f32, y: f32, w: f32, h: f32, iw: f32, ih: f32) -> RectLike {
    let x0 = (x * iw).clamp(0.0, iw);
    let y0 = (y * ih).clamp(0.0, ih);
    let x1 = ((x + w) * iw).clamp(0.0, iw);
    let y1 = ((y + h) * ih).clamp(0.0, ih);
    RectLike::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
}

fn union(a: RectLike, b: RectLike) -> RectLike {
    let x0 = a.x.min(b.x);
    let y0 = a.y.min(b.y);
    let x1 = (a.x + a.width).max(b.x + b.width);
    let y1 = (a.y + a.height).max(b.y + b.height);
    RectLike::new(x0, y0, x1 - x0, y1 - y0)
}

impl TextDocument {
    /// Positioned engine output (normalised, top-left) → base pixels.
    pub fn from_recognized(lines: &[RecognizedLine], image_w: u32, image_h: u32) -> Self {
        let (iw, ih) = (image_w as f32, image_h as f32);
        let out: Vec<TextLine> = reading_order(lines)
            .into_iter()
            .map(|i| {
                let l = &lines[i];
                TextLine {
                    text: l.text.clone(),
                    rect: Some(to_pixels(l.x, l.y, l.w, l.h, iw, ih)),
                    words: l
                        .words
                        .iter()
                        .filter(|w| w.end > w.start)
                        .map(|w| WordRect {
                            start: w.start,
                            end: w.end,
                            rect: to_pixels(w.x, w.y, w.w, w.h, iw, ih),
                        })
                        .collect(),
                }
            })
            .collect();
        Self {
            positioned: !out.is_empty(),
            lines: out,
            layout_text: clean(&reconstruct(lines)),
        }
    }

    /// Engines without positions: one line per non-blank text line.
    pub fn from_plain(text: &str) -> Self {
        let layout_text = clean(text);
        let lines = layout_text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| TextLine {
                text: l.to_string(),
                rect: None,
                words: Vec::new(),
            })
            .collect();
        Self {
            lines,
            positioned: false,
            layout_text,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

impl TextLine {
    /// Box around chars `span` of this line, in base pixels. Safety over
    /// precision, because this feeds redaction:
    /// - every word box the span touches is covered **whole** (cutting a
    ///   box by character count can leak a glyph or two);
    /// - a touched word without a usable box (none reported, or one that
    ///   clamped to zero area) has no trusted place inside its line: the
    ///   line box is axis-aligned, and after a flip or a turn its text runs
    ///   right to left or along y. So the **whole line box** is covered
    ///   (`scrub` masks such words by the same rule);
    /// - `None` without a line box, for an empty or out-of-range span, or
    ///   when the result would have no area (callers can warn).
    pub fn span_rect(&self, span: Range<usize>) -> Option<RectLike> {
        let line = self.rect?;
        let chars: Vec<char> = self.text.chars().collect();
        let n = chars.len();
        if span.start >= span.end || span.start >= n {
            return None;
        }
        let usable = |w: &&WordRect| w.rect.width > 0.0 && w.rect.height > 0.0;
        let mut acc = self
            .words
            .iter()
            .filter(usable)
            .filter(|w| span.start < w.end && w.start < span.end)
            .map(|w| w.rect)
            .reduce(union);
        // Widen the span to whitespace; any token inside it that usable
        // word boxes don't wholly account for covers the line.
        let mut s = span.start;
        let mut e = span.end.min(n);
        while s > 0 && !chars[s - 1].is_whitespace() {
            s -= 1;
        }
        while e < n && !chars[e].is_whitespace() {
            e += 1;
        }
        let mut i = s;
        while i < e {
            if chars[i].is_whitespace() {
                i += 1;
                continue;
            }
            let ts = i;
            while i < e && !chars[i].is_whitespace() {
                i += 1;
            }
            // Every char of the token in some usable box (an engine may box
            // only part of a glued token): else its rest has no place.
            let boxed = (ts..i).all(|c| {
                self.words
                    .iter()
                    .filter(usable)
                    .any(|w| w.start <= c && c < w.end)
            });
            if boxed {
                continue;
            }
            acc = Some(acc.map_or(line, |a| union(a, line)));
            break;
        }
        acc.filter(|r| r.width > 0.0 && r.height > 0.0)
    }
}

/// "Remove line breaks when copying": trim each line, drop blank lines,
/// join with single spaces. It does not de-hyphenate (`foo-` / `bar`
/// becomes `foo- bar`) and blank lines are dropped, so paragraph breaks
/// are lost by design.
pub fn remove_line_breaks(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Copy N Lines: the lines at `indices`, always in reading order and each
/// once whatever order the caller selected them in, joined by newlines or,
/// with `remove_breaks`, by spaces. Unknown indices are skipped.
pub fn copy_lines(doc: &TextDocument, indices: &[usize], remove_breaks: bool) -> String {
    let mut sorted = indices.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let joined = sorted
        .iter()
        .filter_map(|&i| doc.lines.get(i))
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if remove_breaks {
        remove_line_breaks(&joined)
    } else {
        joined
    }
}

/// Copy All: the layout-aware text, or every line joined by spaces.
pub fn copy_all(doc: &TextDocument, remove_breaks: bool) -> String {
    if remove_breaks {
        let all: Vec<usize> = (0..doc.lines.len()).collect();
        copy_lines(doc, &all, true)
    } else {
        doc.layout_text.clone()
    }
}

/// Smart highlighter. When a freehand stroke runs along a
/// recognized line (its mean Y inside the line box, overlapping it
/// horizontally), return a straight two-point stroke across the words
/// it touched, as tall as the line: `(points, line_width)`. `None` keeps
/// the freehand mark.
pub fn snap_highlight(doc: &TextDocument, points: &[PointLike]) -> Option<(Vec<PointLike>, f32)> {
    if !doc.positioned || points.is_empty() {
        return None;
    }
    let sx0 = points.iter().map(|p| p.x).fold(f32::INFINITY, f32::min);
    let sx1 = points.iter().map(|p| p.x).fold(f32::NEG_INFINITY, f32::max);
    let cy = points.iter().map(|p| p.y).sum::<f32>() / points.len() as f32;
    let (line, rect) = doc
        .lines
        .iter()
        .filter_map(|l| l.rect.map(|r| (l, r)))
        .filter(|(_, r)| cy >= r.y && cy <= r.y + r.height)
        .map(|(l, r)| (l, r, sx1.min(r.x + r.width) - sx0.max(r.x)))
        .filter(|(_, _, overlap)| *overlap > 0.0)
        .max_by(|a, b| a.2.total_cmp(&b.2))
        .map(|(l, r, _)| (l, r))?;
    let words = line
        .words
        .iter()
        .filter(|w| w.rect.x < sx1 && w.rect.x + w.rect.width > sx0)
        .fold(None, |acc: Option<(f32, f32)>, w| {
            let (a, b) = (w.rect.x, w.rect.x + w.rect.width);
            Some(match acc {
                Some((p, q)) => (p.min(a), q.max(b)),
                None => (a, b),
            })
        });
    let (x0, x1) = words.unwrap_or((sx0.max(rect.x), sx1.min(rect.x + rect.width)));
    let y = rect.y + rect.height / 2.0;
    Some((
        vec![PointLike::new(x0, y), PointLike::new(x1, y)],
        rect.height,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr_layout::WordBox;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    fn rect_close(a: RectLike, b: RectLike) -> bool {
        close(a.x, b.x) && close(a.y, b.y) && close(a.width, b.width) && close(a.height, b.height)
    }

    /// 1000 × 500 image. Line 0 is "Email priya@example.com" at
    /// (100, 100, 500, 20) px with two word boxes; line 1 sits below.
    fn recognized() -> Vec<RecognizedLine> {
        vec![
            RecognizedLine {
                text: "second line".into(),
                x: 0.1,
                y: 0.4,
                w: 0.3,
                h: 0.04,
                words: Vec::new(),
            },
            RecognizedLine {
                text: "Email priya@example.com".into(),
                x: 0.1,
                y: 0.2,
                w: 0.5,
                h: 0.04,
                words: vec![
                    WordBox {
                        start: 0,
                        end: 5,
                        x: 0.1,
                        y: 0.2,
                        w: 0.1,
                        h: 0.04,
                    },
                    WordBox {
                        start: 6,
                        end: 23,
                        x: 0.22,
                        y: 0.2,
                        w: 0.38,
                        h: 0.04,
                    },
                ],
            },
        ]
    }

    fn doc() -> TextDocument {
        TextDocument::from_recognized(&recognized(), 1000, 500)
    }

    #[test]
    fn from_recognized_orders_lines_and_scales_to_pixels() {
        let d = doc();
        assert!(d.positioned);
        assert_eq!(d.lines[0].text, "Email priya@example.com");
        assert_eq!(d.lines[1].text, "second line");
        assert!(rect_close(
            d.lines[0].rect.unwrap(),
            RectLike::new(100.0, 100.0, 500.0, 20.0)
        ));
        assert_eq!(d.lines[0].words.len(), 2);
        assert!(rect_close(
            d.lines[0].words[1].rect,
            RectLike::new(220.0, 100.0, 380.0, 20.0)
        ));
        // Same string Copy Text produced before the panel existed.
        assert_eq!(d.layout_text, clean(&reconstruct(&recognized())));
    }

    #[test]
    fn from_plain_lists_lines_without_positions() {
        let d = TextDocument::from_plain("one  \r\n\n\ntwo\n");
        assert!(!d.positioned);
        let texts: Vec<&str> = d.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["one", "two"]);
        assert!(d.lines.iter().all(|l| l.rect.is_none()));
        assert_eq!(d.layout_text, "one\n\ntwo");
        assert!(TextDocument::from_plain("  \n").is_empty());
    }

    #[test]
    fn span_rect_covers_every_touched_word_whole() {
        let d = doc();
        let line = &d.lines[0];
        let address = RectLike::new(220.0, 100.0, 380.0, 20.0);
        assert!(rect_close(line.span_rect(6..23).unwrap(), address));
        // "example.com" is only part of the word "priya@example.com":
        // the whole word is covered, so no glyph can leak at a cut.
        assert!(rect_close(line.span_rect(12..23).unwrap(), address));
        // A span touching both words covers both.
        let both = RectLike::new(100.0, 100.0, 500.0, 20.0);
        assert!(rect_close(line.span_rect(0..23).unwrap(), both));
        assert!(rect_close(line.span_rect(4..7).unwrap(), both));
        assert!(line.span_rect(3..3).is_none());
    }

    #[test]
    fn partial_token_without_a_space_is_covered_whole() {
        let line = TextLine {
            text: "token=Zx9Qw7Lm3Rt5Vb1Nc8Kd4Hf2".into(),
            rect: Some(RectLike::new(0.0, 0.0, 300.0, 20.0)),
            words: vec![WordRect {
                start: 0,
                end: 30,
                rect: RectLike::new(0.0, 0.0, 300.0, 20.0),
            }],
        };
        assert_eq!(
            line.span_rect(6..30),
            Some(RectLike::new(0.0, 0.0, 300.0, 20.0))
        );
        // Same without word boxes: the span widens to whitespace first.
        let bare = TextLine {
            words: Vec::new(),
            ..line.clone()
        };
        assert_eq!(
            bare.span_rect(6..30),
            Some(RectLike::new(0.0, 0.0, 300.0, 20.0))
        );
    }

    #[test]
    fn span_rect_without_words_covers_the_whole_line() {
        let d = doc();
        let line = &d.lines[1]; // "second line", (100, 200, 300, 20)
        assert!(rect_close(
            line.span_rect(8..11).unwrap(),
            RectLike::new(100.0, 200.0, 300.0, 20.0)
        ));
        let plain = TextDocument::from_plain("abc");
        assert!(plain.lines[0].span_rect(0..1).is_none());
    }

    #[test]
    fn copy_helpers_join_lines_or_spaces() {
        let d = doc();
        assert_eq!(
            copy_lines(&d, &[1, 0], false),
            "Email priya@example.com\nsecond line"
        );
        assert_eq!(
            copy_lines(&d, &[0, 1], true),
            "Email priya@example.com second line"
        );
        assert_eq!(copy_lines(&d, &[9], false), "");
        assert_eq!(copy_all(&d, false), d.layout_text);
        assert_eq!(copy_all(&d, true), "Email priya@example.com second line");
        assert_eq!(remove_line_breaks("  a \n\n b\n"), "a b");
    }

    #[test]
    fn snap_highlight_covers_the_touched_words_at_line_height() {
        let d = doc();
        // A wobbly stroke over "priya@example.com" only.
        let stroke = [
            PointLike::new(300.0, 108.0),
            PointLike::new(350.0, 112.0),
            PointLike::new(420.0, 109.0),
        ];
        let (points, width) = snap_highlight(&d, &stroke).expect("snaps");
        assert!(close(width, 20.0));
        assert_eq!(points.len(), 2);
        assert!(
            close(points[0].x, 220.0) && close(points[1].x, 600.0),
            "{points:?}"
        );
        assert!(close(points[0].y, 110.0) && close(points[1].y, 110.0));
    }

    #[test]
    fn snap_highlight_without_word_boxes_clamps_to_the_stroke() {
        let d = doc();
        let stroke = [PointLike::new(50.0, 210.0), PointLike::new(250.0, 211.0)];
        let (points, _) = snap_highlight(&d, &stroke).expect("snaps");
        assert!(
            close(points[0].x, 100.0) && close(points[1].x, 250.0),
            "{points:?}"
        );
    }

    #[test]
    fn snap_highlight_leaves_strokes_off_text_alone() {
        let d = doc();
        assert!(snap_highlight(
            &d,
            &[PointLike::new(300.0, 400.0), PointLike::new(400.0, 400.0)]
        )
        .is_none());
        assert!(snap_highlight(&d, &[]).is_none());
        let plain = TextDocument::from_plain("abc");
        assert!(snap_highlight(
            &plain,
            &[PointLike::new(0.0, 0.0), PointLike::new(9.0, 0.0)]
        )
        .is_none());
    }

    #[test]
    fn boxes_are_clamped_to_the_image() {
        let lines = [RecognizedLine {
            text: "edge".into(),
            x: 0.9,
            y: -0.02,
            w: 0.2,
            h: 0.1,
            words: vec![WordBox {
                start: 0,
                end: 4,
                x: 0.95,
                y: 0.95,
                w: 0.1,
                h: 0.1,
            }],
        }];
        let d = TextDocument::from_recognized(&lines, 1000, 500);
        assert!(rect_close(
            d.lines[0].rect.unwrap(),
            RectLike::new(900.0, 0.0, 100.0, 40.0)
        ));
        assert!(rect_close(
            d.lines[0].words[0].rect,
            RectLike::new(950.0, 475.0, 50.0, 25.0)
        ));
    }

    fn line_with(text: &str, words: Vec<WordRect>) -> TextLine {
        TextLine {
            text: text.into(),
            rect: Some(RectLike::new(0.0, 0.0, 100.0, 10.0)),
            words,
        }
    }

    #[test]
    fn a_partly_boxed_glued_token_covers_the_line() {
        // "key:Zx9Qw7": the engine boxed "key:" only (x 0–40).
        let line = line_with(
            "key:Zx9Qw7 next",
            vec![WordRect {
                start: 0,
                end: 4,
                rect: RectLike::new(0.0, 0.0, 40.0, 10.0),
            }],
        );
        let r = line.span_rect(4..10).unwrap();
        assert!(rect_close(r, RectLike::new(0.0, 0.0, 100.0, 10.0)), "{r:?}");
    }

    #[test]
    fn span_rect_counts_chars_not_bytes() {
        // "Zürich ✓ token": 14 chars (17 bytes); "token" is chars 9..14,
        // in range, so it is placed (on the whole line: no word boxes).
        let line = line_with("Zürich ✓ token", Vec::new());
        let r = line.span_rect(9..14).unwrap();
        assert!(rect_close(r, RectLike::new(0.0, 0.0, 100.0, 10.0)), "{r:?}");
        assert!(line.span_rect(14..17).is_none());
    }

    #[test]
    fn out_of_range_span_is_none() {
        let line = line_with("abc def", Vec::new());
        assert!(line.span_rect(7..9).is_none());
        assert!(line.span_rect(20..30).is_none());
    }

    #[test]
    fn partially_boxed_span_covers_every_touched_word() {
        // "aa bb cc": only "aa" has a box; "bb" and "cc" fall back.
        let line = line_with(
            "aa bb cc",
            vec![WordRect {
                start: 0,
                end: 2,
                rect: RectLike::new(0.0, 0.0, 20.0, 10.0),
            }],
        );
        let r = line.span_rect(0..8).unwrap();
        assert!(rect_close(r, RectLike::new(0.0, 0.0, 100.0, 10.0)), "{r:?}");
        // Just "bb": no box, so the whole line box.
        let bb = line.span_rect(3..5).unwrap();
        assert!(
            rect_close(bb, RectLike::new(0.0, 0.0, 100.0, 10.0)),
            "{bb:?}"
        );
    }

    #[test]
    fn word_box_outside_the_image_is_ignored() {
        let lines = [RecognizedLine {
            text: "ghost real".into(),
            x: 0.0,
            y: 0.2,
            w: 0.5,
            h: 0.04,
            words: vec![
                WordBox {
                    start: 0,
                    end: 5,
                    x: 1.2,
                    y: 0.2,
                    w: 0.1,
                    h: 0.04,
                },
                WordBox {
                    start: 6,
                    end: 10,
                    x: 0.3,
                    y: 0.2,
                    w: 0.2,
                    h: 0.04,
                },
            ],
        }];
        let d = TextDocument::from_recognized(&lines, 1000, 500);
        let line = &d.lines[0];
        // The degenerate box is skipped, so "ghost" falls back to the
        // line slice instead of vanishing.
        let r = line.span_rect(0..5).unwrap();
        assert!(r.width > 0.0 && r.x < 100.0, "{r:?}");
        let both = line.span_rect(0..10).unwrap();
        assert!(
            both.x < 100.0 && close(both.x + both.width, 500.0),
            "{both:?}"
        );
    }

    #[test]
    fn zero_area_result_is_none() {
        let mut line = line_with("abc", Vec::new());
        line.rect = Some(RectLike::new(0.0, 0.0, 0.0, 10.0));
        assert!(line.span_rect(0..3).is_none());
    }

    fn contains(outer: RectLike, inner: RectLike) -> bool {
        let e = 0.01;
        outer.x <= inner.x + e
            && outer.y <= inner.y + e
            && outer.x + outer.width + e >= inner.x + inner.width
            && outer.y + outer.height + e >= inner.y + inner.height
    }

    #[test]
    fn a_boxless_span_is_covered_whatever_the_orientation() {
        use crate::document::{map_text_document, FlipAxis, Transform};
        // "SECRET1234 and some other words here", line x 10–370 of a
        // 400 × 50 image, no word boxes; the real glyphs of "SECRET1234"
        // are x 10–110.
        let d = TextDocument {
            lines: vec![TextLine {
                text: "SECRET1234 and some other words here".into(),
                rect: Some(RectLike::new(10.0, 10.0, 360.0, 20.0)),
                words: Vec::new(),
            }],
            positioned: true,
            layout_text: String::new(),
        };
        let t = Transform::default();
        for (name, t) in [
            ("identity", t),
            ("flip h", t.flipped(FlipAxis::Horizontal)),
            ("flip v", t.flipped(FlipAxis::Vertical)),
            ("turn 90", t.rotated_cw()),
            ("turn 180", t.rotated_cw().rotated_cw()),
            ("turn 270", t.rotated_ccw()),
        ] {
            let (m, _) = t.affine(400, 50);
            let moved = map_text_document(&d, &m);
            let real = m.map_rect(RectLike::new(10.0, 10.0, 100.0, 20.0));
            let cover = moved.lines[0].span_rect(0..10).expect("placed");
            assert!(contains(cover, real), "{name}: {cover:?} misses {real:?}");
        }
    }
}
