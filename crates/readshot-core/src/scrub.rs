//! Text scrubbing after a redaction (History scrub, masked Copy N Lines).
//!
//! Once the editor's annotations hide text on the image, every text copy
//! of that capture must hide the same words: the History record's
//! `ocr_text` (which the MCP server serves) and the Text panel's Copy N
//! Lines. Hidden characters become `█`, one per character, so line
//! structure and word lengths survive.
//!
//! Which words are hidden:
//! - **With line positions** (Apple Vision): a recognized word whose box
//!   is at least half covered by redactions — an opaque, square-cornered
//!   filled rectangle (the Solid redaction), a Blur or a Pixelate. Blur
//!   and pixelate count as hiding here: the user meant to hide the word.
//!   Coverage adds up over every redaction, so two adjacent blocks that
//!   each cover part of a word can hide it together (this can only over-
//!   count, never miss).
//! - **Without positions** (Windows, Linux engines): nothing ties a word
//!   to a place on the image, so when any redaction exists every
//!   sensitive item the smart-redact detector finds ([`crate::redact::scan`])
//!   is masked, in every line. Words a user hid by hand that the detector
//!   doesn't recognise stay readable — the limit of position-less OCR.
//!
//! Pure; no I/O.

use std::collections::BTreeMap;
use std::ops::Range;

use crate::annotation::{Annotation, FillMode};
use crate::geom::RectLike;
use crate::text_lines::{TextDocument, TextLine};

/// The mask character.
pub const MASK: char = '█';

/// Fraction of a word box that redactions must cover to hide the word.
const HIDDEN_FRACTION: f32 = 0.5;

/// A hidden run: chars `span` of `TextDocument::lines[line]`.
pub type HiddenSpan = (usize, Range<usize>);

/// Whether `a` hides what is under it: an opaque, square-cornered filled
/// rectangle (the Solid redaction shape), a Blur or a Pixelate.
pub fn is_redaction(a: &Annotation) -> bool {
    match a {
        Annotation::Rectangle { color, style, .. } => {
            style.fill == FillMode::Filled && style.corner_radius == 0.0 && color.a >= 1.0
        }
        Annotation::Blur { .. } | Annotation::Pixelate { .. } => true,
        _ => false,
    }
}

/// Whether any annotation is a redaction ([`is_redaction`]).
pub fn has_redaction(annotations: &[Annotation]) -> bool {
    annotations.iter().any(is_redaction)
}

fn redaction_rects(annotations: &[Annotation]) -> Vec<RectLike> {
    annotations
        .iter()
        .filter(|a| is_redaction(a))
        .filter_map(|a| match a {
            Annotation::Rectangle { rect, .. }
            | Annotation::Blur { rect, .. }
            | Annotation::Pixelate { rect, .. } => Some(*rect),
            _ => None,
        })
        .collect()
}

fn area(r: RectLike) -> f32 {
    r.width.max(0.0) * r.height.max(0.0)
}

fn intersection_area(a: RectLike, b: RectLike) -> f32 {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = (a.x + a.width).min(b.x + b.width);
    let y1 = (a.y + a.height).min(b.y + b.height);
    (x1 - x0).max(0.0) * (y1 - y0).max(0.0)
}

fn is_hidden(word: RectLike, redactions: &[RectLike]) -> bool {
    let a = area(word);
    if a <= 0.0 {
        return false;
    }
    let covered: f32 = redactions.iter().map(|r| intersection_area(word, *r)).sum();
    covered >= a * HIDDEN_FRACTION
}

/// Whitespace-delimited token ranges (char indices) of `chars`.
fn tokens(chars: &[char]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        let s = i;
        while i < chars.len() && !chars[i].is_whitespace() {
            i += 1;
        }
        out.push(s..i);
    }
    out
}

/// Hidden char ranges of one positioned line. Each word box is tested on
/// its own; a token no usable word box accounts for is tested with its
/// proportional slice of the line box.
fn hidden_in_line(line: &TextLine, redactions: &[RectLike]) -> Vec<Range<usize>> {
    let Some(line_rect) = line.rect else {
        return Vec::new();
    };
    let chars: Vec<char> = line.text.chars().collect();
    let n = chars.len();
    let usable: Vec<_> = line
        .words
        .iter()
        .filter(|w| w.rect.width > 0.0 && w.rect.height > 0.0 && w.start < w.end)
        .collect();
    let mut out = Vec::new();
    for tok in tokens(&chars) {
        let boxed: Vec<_> = usable
            .iter()
            .filter(|w| tok.start < w.end && w.start < tok.end)
            .collect();
        if boxed.is_empty() {
            let nf = n as f32;
            let x0 = line_rect.x + line_rect.width * tok.start as f32 / nf;
            let x1 = line_rect.x + line_rect.width * tok.end as f32 / nf;
            let slice = RectLike::new(x0, line_rect.y, x1 - x0, line_rect.height);
            if is_hidden(slice, redactions) {
                out.push(tok);
            }
            continue;
        }
        for w in boxed {
            if is_hidden(w.rect, redactions) {
                out.push(tok.start.max(w.start)..tok.end.min(w.end).min(n));
            }
        }
    }
    out
}

/// Every hidden span of `doc` under `annotations` (see the module docs
/// for the rule with and without positions). Empty when no annotation is
/// a redaction.
pub fn hidden_spans(doc: &TextDocument, annotations: &[Annotation]) -> Vec<HiddenSpan> {
    if !has_redaction(annotations) {
        return Vec::new();
    }
    if !doc.positioned {
        return crate::redact::scan(doc)
            .into_iter()
            .map(|m| (m.line, m.span))
            .collect();
    }
    let redactions = redaction_rects(annotations);
    doc.lines
        .iter()
        .enumerate()
        .flat_map(|(i, line)| {
            hidden_in_line(line, &redactions)
                .into_iter()
                .map(move |r| (i, r))
        })
        .collect()
}

fn by_line(spans: &[HiddenSpan]) -> BTreeMap<usize, Vec<Range<usize>>> {
    let mut map: BTreeMap<usize, Vec<Range<usize>>> = BTreeMap::new();
    for (line, r) in spans {
        map.entry(*line).or_default().push(r.clone());
    }
    map
}

/// `text` with the chars in `ranges` replaced by [`MASK`] (whitespace is
/// kept, so word and line structure survive).
pub fn mask_text(text: &str, ranges: &[Range<usize>]) -> String {
    text.chars()
        .enumerate()
        .map(|(i, c)| {
            if !c.is_whitespace() && ranges.iter().any(|r| r.contains(&i)) {
                MASK
            } else {
                c
            }
        })
        .collect()
}

/// `doc` with every hidden span of its lines masked. Positions are kept
/// (masking never changes a char count). `layout_text` is masked with
/// [`masked_layout_text`].
pub fn mask_document(doc: &TextDocument, spans: &[HiddenSpan]) -> TextDocument {
    let map = by_line(spans);
    TextDocument {
        lines: doc
            .lines
            .iter()
            .enumerate()
            .map(|(i, l)| match map.get(&i) {
                Some(ranges) => TextLine {
                    text: mask_text(&l.text, ranges),
                    ..l.clone()
                },
                None => l.clone(),
            })
            .collect(),
        positioned: doc.positioned,
        layout_text: masked_layout_text(doc, spans),
    }
}

fn find_from(hay: &[char], needle: &[char], from: usize) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|&p| hay[p..p + needle.len()] == *needle)
}

fn mark_all(hay: &[char], needle: &[char], mask: &mut [bool]) {
    let mut from = 0;
    while let Some(p) = find_from(hay, needle, from) {
        mask[p..p + needle.len()].iter_mut().for_each(|m| *m = true);
        from = p + 1;
    }
}

/// `doc.layout_text` with every hidden word masked.
///
/// The layout text is rebuilt from the lines (indents, table pipes, code
/// fences), so each line is found in it in reading order and its hidden
/// spans are masked in place. A line that can't be found after the
/// previous one (a multi-column or table layout reorders or normalises
/// it) falls back to masking **every** occurrence of each of its hidden
/// words — over-masking a repeated word is preferred to leaking it.
pub fn masked_layout_text(doc: &TextDocument, spans: &[HiddenSpan]) -> String {
    if spans.is_empty() {
        return doc.layout_text.clone();
    }
    let map = by_line(spans);
    let layout: Vec<char> = doc.layout_text.chars().collect();
    let mut mask = vec![false; layout.len()];
    let mut cursor = 0;
    for (i, line) in doc.lines.iter().enumerate() {
        let needle: Vec<char> = line.text.trim_end().chars().collect();
        let found = find_from(&layout, &needle, cursor);
        if let Some(p) = found {
            cursor = p + needle.len();
        }
        let Some(ranges) = map.get(&i) else {
            continue;
        };
        match found {
            Some(p) => {
                for r in ranges {
                    for k in r.clone().filter(|&k| k < needle.len()) {
                        mask[p + k] = true;
                    }
                }
            }
            None => {
                let chars: Vec<char> = line.text.chars().collect();
                for r in ranges {
                    let end = r.end.min(chars.len());
                    if r.start >= end {
                        continue;
                    }
                    let word: Vec<char> = chars[r.start..end].to_vec();
                    mark_all(&layout, &word, &mut mask);
                    // Markdown table cells escape `|`.
                    if word.contains(&'|') {
                        let escaped: Vec<char> = word
                            .iter()
                            .flat_map(|&c| if c == '|' { vec!['\\', '|'] } else { vec![c] })
                            .collect();
                        mark_all(&layout, &escaped, &mut mask);
                    }
                }
            }
        }
    }
    layout
        .iter()
        .zip(mask)
        .map(|(&c, m)| if m && !c.is_whitespace() { MASK } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotation::{Rgba, ShapeStyle};
    use crate::text_lines::WordRect;

    fn solid(rect: RectLike) -> Annotation {
        Annotation::Rectangle {
            rect,
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: ShapeStyle {
                fill: FillMode::Filled,
                ..ShapeStyle::default()
            },
        }
    }

    fn word(start: usize, end: usize, x: f32, y: f32, w: f32) -> WordRect {
        WordRect {
            start,
            end,
            rect: RectLike::new(x, y, w, 10.0),
        }
    }

    /// "key: Zx9Qw7 next" with a box per word, 10 px per char.
    fn doc() -> TextDocument {
        let text = "key: Zx9Qw7 next".to_string();
        TextDocument {
            lines: vec![
                TextLine {
                    text: "intro line".into(),
                    rect: Some(RectLike::new(0.0, 0.0, 100.0, 10.0)),
                    words: vec![word(0, 5, 0.0, 0.0, 50.0), word(6, 10, 60.0, 0.0, 40.0)],
                },
                TextLine {
                    text: text.clone(),
                    rect: Some(RectLike::new(0.0, 10.0, 160.0, 10.0)),
                    words: vec![
                        word(0, 4, 0.0, 10.0, 40.0),
                        word(5, 11, 50.0, 10.0, 60.0),
                        word(12, 16, 120.0, 10.0, 40.0),
                    ],
                },
            ],
            positioned: true,
            layout_text: format!("intro line\n  {text}"),
        }
    }

    #[test]
    fn only_opaque_square_filled_rects_blur_and_pixelate_are_redactions() {
        let r = RectLike::new(0.0, 0.0, 4.0, 4.0);
        assert!(is_redaction(&solid(r)));
        assert!(is_redaction(&Annotation::Blur {
            rect: r,
            radius: 2.0
        }));
        assert!(is_redaction(&Annotation::Pixelate {
            rect: r,
            block_size: 2.0
        }));
        let Annotation::Rectangle {
            rect,
            line_width,
            style,
            ..
        } = solid(r)
        else {
            unreachable!()
        };
        let translucent = Annotation::Rectangle {
            rect,
            color: Rgba::new(0.0, 0.0, 0.0, 0.5),
            line_width,
            style,
        };
        assert!(!is_redaction(&translucent));
        let rounded = Annotation::Rectangle {
            rect,
            color: Rgba::OPAQUE_BLACK,
            line_width,
            style: ShapeStyle {
                corner_radius: 4.0,
                ..style
            },
        };
        assert!(!is_redaction(&rounded));
        let outline = Annotation::Rectangle {
            rect,
            color: Rgba::OPAQUE_BLACK,
            line_width,
            style: ShapeStyle::default(),
        };
        assert!(!is_redaction(&outline));
    }

    #[test]
    fn a_word_at_least_half_covered_is_hidden() {
        let d = doc();
        // Covers 30 of the 60 px of "Zx9Qw7" (exactly half).
        let anns = [solid(RectLike::new(50.0, 10.0, 30.0, 10.0))];
        assert_eq!(hidden_spans(&d, &anns), vec![(1, 5..11)]);
        // 29 px: under half.
        let anns = [solid(RectLike::new(50.0, 10.0, 29.0, 10.0))];
        assert!(hidden_spans(&d, &anns).is_empty());
    }

    #[test]
    fn coverage_adds_up_over_several_redactions_and_counts_blur() {
        let d = doc();
        let anns = [
            Annotation::Blur {
                rect: RectLike::new(50.0, 10.0, 20.0, 10.0),
                radius: 3.0,
            },
            Annotation::Pixelate {
                rect: RectLike::new(70.0, 10.0, 15.0, 10.0),
                block_size: 4.0,
            },
        ];
        assert_eq!(hidden_spans(&d, &anns), vec![(1, 5..11)]);
    }

    #[test]
    fn non_redaction_annotations_hide_nothing() {
        let d = doc();
        let anns = [Annotation::Highlighter {
            points: vec![],
            color: Rgba::OPAQUE_BLACK,
            line_width: 4.0,
        }];
        assert!(hidden_spans(&d, &anns).is_empty());
        assert!(!has_redaction(&anns));
    }

    #[test]
    fn tokens_without_word_boxes_use_their_slice_of_the_line() {
        let mut d = doc();
        d.lines[1].words.clear();
        // 16 chars over 160 px: "Zx9Qw7" is x 50..110.
        let anns = [solid(RectLike::new(50.0, 10.0, 60.0, 10.0))];
        assert_eq!(hidden_spans(&d, &anns), vec![(1, 5..11)]);
    }

    #[test]
    fn masking_keeps_char_counts_and_line_structure() {
        let d = doc();
        let anns = [solid(RectLike::new(50.0, 10.0, 60.0, 10.0))];
        let spans = hidden_spans(&d, &anns);
        let masked = mask_document(&d, &spans);
        assert_eq!(masked.lines[0].text, "intro line");
        assert_eq!(masked.lines[1].text, "key: ██████ next");
        assert_eq!(masked.layout_text, "intro line\n  key: ██████ next");
        assert_eq!(
            masked.layout_text.chars().count(),
            d.layout_text.chars().count()
        );
        assert!(!masked.layout_text.contains("Zx9Qw7"));
    }

    #[test]
    fn a_line_missing_from_the_layout_masks_every_occurrence_of_its_hidden_words() {
        let mut d = doc();
        // A table layout normalises the line; the word appears twice.
        d.layout_text = "| key: | Zx9Qw7 | next |\nagain Zx9Qw7".into();
        let anns = [solid(RectLike::new(50.0, 10.0, 60.0, 10.0))];
        let spans = hidden_spans(&d, &anns);
        assert_eq!(
            masked_layout_text(&d, &spans),
            "| key: | ██████ | next |\nagain ██████"
        );
    }

    #[test]
    fn without_positions_every_detected_item_is_masked_once_anything_is_redacted() {
        let email = ["someone", "@", "example.com"].concat();
        let d = TextDocument::from_plain(&format!("Contact\nmail {email} now"));
        let anns = [solid(RectLike::new(0.0, 0.0, 4.0, 4.0))];
        let spans = hidden_spans(&d, &anns);
        let masked = mask_document(&d, &spans);
        assert!(!masked.layout_text.contains(&email));
        assert_eq!(
            masked.layout_text,
            format!("Contact\nmail {} now", "█".repeat(email.chars().count()))
        );
        // No redaction: nothing is masked.
        assert!(hidden_spans(&d, &[]).is_empty());
    }

    #[test]
    fn no_spans_returns_the_text_unchanged() {
        let d = doc();
        assert_eq!(masked_layout_text(&d, &[]), d.layout_text);
        assert_eq!(mask_document(&d, &[]), d);
    }
}
