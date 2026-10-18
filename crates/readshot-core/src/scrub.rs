//! Text scrubbing after a redaction (History scrub, masked Copy N Lines).
//!
//! Once the editor's annotations hide text on the image, every text copy
//! of that capture must hide the same words: the History record's
//! `ocr_text` (which the MCP server serves) and the Text panel's Copy N
//! Lines. Hidden characters become `█`, one per character, so line
//! structure and word lengths survive.
//!
//! Which words are hidden (privacy over precision: over-masking is
//! acceptable, under-masking never):
//! - **With line positions** (Apple Vision): a recognized word whose box
//!   any redaction overlaps — an opaque, square-cornered filled rectangle
//!   (the Solid redaction), a Blur or a Pixelate — is hidden **in full**,
//!   however little of it is covered: a cover over only `hunter2` in
//!   `password=hunter2` hides the whole token. Blur and pixelate count as
//!   hiding: the user meant to hide the word. Only hairline contact is
//!   ignored, so a cover that grazes the neighbouring word's edge leaves
//!   it readable: an overlap under [`HAIRLINE_PX`] deep (its narrow side)
//!   **and** under [`HAIRLINE_FRACTION`] of the word box's area. Overlaps
//!   add up, so several hairlines that together pass the area limit hide
//!   the word.
//! - A token no usable word box accounts for has no reliable position
//!   inside its line: the line box is axis-aligned, and after a flip or a
//!   turn its text runs right to left or along y. So any redaction
//!   overlapping the line box (same hairline rule) hides every such token
//!   of that line.
//! - **Without positions** (Windows, Linux engines), or a line without a
//!   box: nothing ties a word to a place on the image, so once any
//!   redaction exists nothing can tell which words it hides.
//!   [`hidden_spans`] returns `None` and callers withhold the text
//!   entirely (History stores none, Copy Lines refuses) — never text with
//!   only the detected items masked.
//!
//! Pure; no I/O.

use std::collections::BTreeMap;
use std::ops::Range;

use crate::annotation::Annotation;
use crate::geom::RectLike;
use crate::text_lines::{TextDocument, TextLine};

/// The mask character.
pub const MASK: char = '█';

/// Hairline tolerance: an overlap shallower than this (px, its narrow
/// side) **and** smaller than [`HAIRLINE_FRACTION`] of the word box is
/// contact, not cover, and hides nothing.
pub const HAIRLINE_PX: f32 = 1.0;

/// See [`HAIRLINE_PX`]: the area limit, as a fraction of the word box.
pub const HAIRLINE_FRACTION: f32 = 0.01;

/// A hidden run: chars `span` of `TextDocument::lines[line]`.
pub type HiddenSpan = (usize, Range<usize>);

/// Whether `a` hides what is under it ([`Annotation::redaction_rect`]:
/// an opaque, square-cornered filled rectangle, a Blur or a Pixelate).
pub fn is_redaction(a: &Annotation) -> bool {
    a.is_redaction()
}

/// Whether any annotation is a redaction ([`is_redaction`]).
pub fn has_redaction(annotations: &[Annotation]) -> bool {
    annotations.iter().any(is_redaction)
}

/// Every redaction's rect, normalised to non-negative sizes.
fn redaction_rects(annotations: &[Annotation]) -> Vec<RectLike> {
    annotations
        .iter()
        .filter_map(Annotation::redaction_rect)
        .map(|r| {
            let (x0, x1) = (r.x.min(r.x + r.width), r.x.max(r.x + r.width));
            let (y0, y1) = (r.y.min(r.y + r.height), r.y.max(r.y + r.height));
            RectLike::new(x0, y0, x1 - x0, y1 - y0)
        })
        .collect()
}

/// Margin (px) around a magnifier bubble's square that
/// [`magnified_redactions`] also covers: the bubble's border and its
/// anti-aliased edge.
const BUBBLE_PAD: f32 = 2.0;

/// Solid redactions over every magnifier bubble that shows something a
/// redaction hides. A bubble copies the composed pixels around its source
/// (the renderer draws magnifiers last), so a bubble whose source touches
/// a Blur or Pixelate shows a magnified copy of that weakly hidden text,
/// which recognition of the output can read. Magnifiers are taken in
/// drawing order, and a bubble covered here counts as a redaction for the
/// ones drawn after it (a bubble magnifying such a bubble). For masking
/// recognized text of a rendered output, never for drawing.
pub fn magnified_redactions(annotations: &[Annotation]) -> Vec<Annotation> {
    let mut covers = redaction_rects(annotations);
    let mut out = Vec::new();
    for a in annotations {
        let Annotation::Magnifier {
            source,
            radius,
            bubble,
            zoom,
            ..
        } = a
        else {
            continue;
        };
        let r = radius.abs();
        let src = RectLike::new(source.x - r, source.y - r, 2.0 * r, 2.0 * r);
        let shows_hidden = covers.iter().any(|c| {
            let (w, h) = intersection(src, *c);
            w > 0.0 && h > 0.0
        }) || !src.x.is_finite()
            || !src.y.is_finite()
            || !r.is_finite();
        if !shows_hidden {
            continue;
        }
        let br = crate::magnifier::bubble_radius(r, *zoom) + BUBBLE_PAD;
        let rect = RectLike::new(bubble.x - br, bubble.y - br, 2.0 * br, 2.0 * br);
        covers.push(rect);
        out.push(crate::redact::redaction_annotation(
            rect,
            crate::redact::RedactStyle::Solid,
        ));
    }
    out
}

/// `redactions` (base px) moved into the pixels of an output cropped to
/// `crop` ([`crate::render::output_crop`]): translated by minus the crop
/// origin and clipped to the output, dropping any left with no area.
/// What a scrub that bakes a Crop stores, so the redactions line up with
/// the stored (cropped) image. Only redactions ([`is_redaction`]) are kept.
pub fn redactions_in_crop(
    redactions: &[Annotation],
    crop: crate::render::OutputCrop,
) -> Vec<Annotation> {
    redactions
        .iter()
        .filter_map(|a| a.with_redaction_rect(rect_in_crop(a.redaction_rect()?, crop)?))
        .collect()
}

/// `redactions` clipped to a `width × height` image, dropping any left
/// with no area (zero-area rects, and rects that only touch the image
/// along an edge or at a corner, `x == width` say): what a scrub that
/// bakes no Crop stores, so every stored redaction covers some of the
/// stored image ([`touches_image`], the test History's legacy check
/// applies). Only redactions ([`is_redaction`]) are kept.
pub fn redactions_in_image(redactions: &[Annotation], width: u32, height: u32) -> Vec<Annotation> {
    let (w, h) = (width as f32, height as f32);
    redactions_in_crop(
        redactions,
        crate::render::OutputCrop {
            x: 0.0,
            y: 0.0,
            width,
            height,
        },
    )
    .into_iter()
    .filter(|a| a.redaction_rect().is_some_and(|r| touches_image(r, w, h)))
    .collect()
}

/// Whether `r` (either size may be negative) covers some area of a
/// `w × h` image: a rect that only meets it along an edge or at a
/// corner, has no area, or holds a NaN does not.
pub fn touches_image(r: RectLike, w: f32, h: f32) -> bool {
    let (x0, x1) = (r.x.min(r.x + r.width), r.x.max(r.x + r.width));
    let (y0, y1) = (r.y.min(r.y + r.height), r.y.max(r.y + r.height));
    // NaN compares false everywhere: never touches.
    x0.max(0.0) < x1.min(w) && y0.max(0.0) < y1.min(h) && x0 < w && y0 < h && x1 > 0.0 && y1 > 0.0
}

/// `r` (base px) moved into the pixels of an output cropped to `crop`
/// and clipped to it; `None` when nothing of it is inside.
fn rect_in_crop(r: RectLike, crop: crate::render::OutputCrop) -> Option<RectLike> {
    let (w, h) = (crop.width as f32, crop.height as f32);
    let (mut x0, mut x1) = (r.x - crop.x, r.x + r.width - crop.x);
    let (mut y0, mut y1) = (r.y - crop.y, r.y + r.height - crop.y);
    if x0 > x1 {
        std::mem::swap(&mut x0, &mut x1);
    }
    if y0 > y1 {
        std::mem::swap(&mut y0, &mut y1);
    }
    let (x0, x1) = (x0.max(0.0), x1.min(w));
    let (y0, y1) = (y0.max(0.0), y1.min(h));
    (x1 > x0 && y1 > y0).then(|| RectLike::new(x0, y0, x1 - x0, y1 - y0))
}

/// Float slack when testing a box against the crop: far below a pixel,
/// so it only absorbs rounding, never a visible sliver of a glyph.
const CROP_EPSILON: f32 = 1e-3;

/// `doc` (positions in base px) as it reads in an output cropped to
/// `crop` ([`crate::render::output_crop`]): only the words whose boxes lie
/// **wholly** inside the crop, their boxes moved into the cropped output's
/// pixels (the frame [`redactions_in_crop`] stores redactions in). What a
/// History record whose stored image is that crop may say: never a
/// character from the cut-away area.
///
/// A box is all a word's position is known by — glyph widths vary, and a
/// mirrored, rotated or right-to-left word runs the other way inside its
/// axis-aligned box — so a word is kept or dropped whole: a box that
/// straddles a crop edge (on either axis), lies outside, or has no usable
/// size drops its word, as does text no word box accounts for. A long
/// token cut by the edge, or a no-space line (CJK, Thai) boxed as one
/// word, is lost to search: the price of never storing cut-away text.
///
/// A line's kept words are joined by single spaces; lines with nothing
/// kept are dropped; `layout_text` is the kept lines joined by newlines
/// (the original layout can't be cut to the crop).
///
/// `None` without positions, or when a line has no box: callers then store
/// no text (fail closed).
pub fn document_in_crop(
    doc: &TextDocument,
    crop: crate::render::OutputCrop,
) -> Option<TextDocument> {
    use crate::text_lines::WordRect;
    if !doc.positioned {
        return None;
    }
    let lines = doc
        .lines
        .iter()
        .map(|line| {
            line.rect?;
            let chars: Vec<char> = line.text.chars().collect();
            let n = chars.len();
            // Kept words: char range and box (cropped-output px).
            let kept: Vec<(Range<usize>, RectLike)> = line
                .words
                .iter()
                .filter(|w| w.start < w.end.min(n))
                .filter_map(|w| Some((w.start..w.end.min(n), wholly_in_crop(w.rect, crop)?)))
                .collect();
            let mut keep = vec![false; n];
            for (r, _) in &kept {
                keep[r.clone()].iter_mut().for_each(|k| *k = true);
            }
            // The kept chars, token by token; `at[i]` is char i's new index.
            let mut text = String::new();
            let mut at = vec![None; n];
            let mut len = 0usize;
            for tok in tokens(&chars) {
                if !keep[tok.clone()].contains(&true) {
                    continue;
                }
                if len > 0 {
                    text.push(' ');
                    len += 1;
                }
                for i in tok.filter(|&i| keep[i]) {
                    at[i] = Some(len);
                    text.push(chars[i]);
                    len += 1;
                }
            }
            let words: Vec<WordRect> = kept
                .iter()
                .filter_map(|(r, b)| {
                    let first = r.clone().find_map(|i| at[i])?;
                    let last = r.clone().rev().find_map(|i| at[i])?;
                    Some(WordRect {
                        start: first,
                        end: last + 1,
                        rect: *b,
                    })
                })
                .collect();
            let rect = words.iter().map(|w| w.rect).reduce(|a, b| {
                let (x0, y0) = (a.x.min(b.x), a.y.min(b.y));
                let x1 = (a.x + a.width).max(b.x + b.width);
                let y1 = (a.y + a.height).max(b.y + b.height);
                RectLike::new(x0, y0, x1 - x0, y1 - y0)
            });
            Some((len > 0).then_some(TextLine { text, rect, words }))
        })
        .collect::<Option<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let layout_text = lines
        .iter()
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    Some(TextDocument {
        lines,
        positioned: true,
        layout_text,
    })
}

/// `r` (base px) in the cropped output's px when it lies wholly inside
/// `crop` on both axes (within [`CROP_EPSILON`]), clamped to it; `None`
/// when it straddles an edge, lies outside, or has no usable size.
fn wholly_in_crop(r: RectLike, crop: crate::render::OutputCrop) -> Option<RectLike> {
    let finite = [r.x, r.y, r.width, r.height, crop.x, crop.y]
        .iter()
        .all(|v| v.is_finite());
    if !finite || r.width <= 0.0 || r.height <= 0.0 {
        return None;
    }
    let (w, h) = (crop.width as f32, crop.height as f32);
    let (x0, y0) = (r.x - crop.x, r.y - crop.y);
    let (x1, y1) = (x0 + r.width, y0 + r.height);
    let inside = x0 >= -CROP_EPSILON
        && y0 >= -CROP_EPSILON
        && x1 <= w + CROP_EPSILON
        && y1 <= h + CROP_EPSILON;
    inside.then(|| {
        let (x0, y0) = (x0.max(0.0), y0.max(0.0));
        RectLike::new(x0, y0, x1.min(w) - x0, y1.min(h) - y0)
    })
}

fn area(r: RectLike) -> f32 {
    r.width.max(0.0) * r.height.max(0.0)
}

/// Width and height of `a ∩ b` (0 when they don't overlap).
fn intersection(a: RectLike, b: RectLike) -> (f32, f32) {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = (a.x + a.width).min(b.x + b.width);
    let y1 = (a.y + a.height).min(b.y + b.height);
    ((x1 - x0).max(0.0), (y1 - y0).max(0.0))
}

/// Whether redactions hide `word` (a box): any overlap that isn't
/// hairline contact (see the module docs), or hairlines that together
/// reach [`HAIRLINE_FRACTION`] of it.
fn is_hidden(word: RectLike, redactions: &[RectLike]) -> bool {
    let a = area(word);
    // NaN or no area: not a usable box.
    if a.is_nan() || a <= 0.0 {
        return false;
    }
    let mut hairlines = 0.0;
    for r in redactions {
        let (w, h) = intersection(word, *r);
        if w.is_nan() || h.is_nan() || w <= 0.0 || h <= 0.0 {
            continue;
        }
        let overlap = w * h;
        let hairline = w.min(h) < HAIRLINE_PX && overlap < a * HAIRLINE_FRACTION;
        if !hairline {
            return true;
        }
        hairlines += overlap;
    }
    hairlines >= a * HAIRLINE_FRACTION
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

/// Hidden char ranges of one positioned line; `None` when it has no box
/// (nothing can tell what is hidden). Each word box is tested on its own
/// ([`is_hidden`]) and hides its whole token part. Chars no usable word
/// box accounts for are hidden when any redaction hides the line box: a
/// position inside the line can't be trusted (flips and turns reverse or
/// rotate the text inside its axis-aligned box).
fn hidden_in_line(line: &TextLine, redactions: &[RectLike]) -> Option<Vec<Range<usize>>> {
    let line_rect = line.rect?;
    let chars: Vec<char> = line.text.chars().collect();
    let n = chars.len();
    // A line box with no usable area (NaN, or clamped to nothing) can't
    // be tested: its unboxed chars count as hidden (fail closed).
    let line_area = area(line_rect);
    let line_hit = line_area.is_nan() || line_area <= 0.0 || is_hidden(line_rect, redactions);
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
        for w in &boxed {
            if is_hidden(w.rect, redactions) {
                out.push(tok.start.max(w.start)..tok.end.min(w.end).min(n));
            }
        }
        if !line_hit {
            continue;
        }
        // Chars of the token no word box accounts for (no box at all, or
        // an engine that boxed only part of a glued token): never skipped.
        let mut i = tok.start;
        while i < tok.end {
            if boxed.iter().any(|w| w.start <= i && i < w.end) {
                i += 1;
                continue;
            }
            let rs = i;
            while i < tok.end && !boxed.iter().any(|w| w.start <= i && i < w.end) {
                i += 1;
            }
            out.push(rs..i);
        }
    }
    Some(out)
}

/// Every hidden span of `doc` under `annotations` (see the module docs
/// for the rule). `Some(empty)` when no annotation is a redaction.
/// `None` when there is a redaction but `doc` can't tell what it hides
/// (no positions, or a line without a box): the caller must then
/// withhold the text, never serve it.
pub fn hidden_spans(doc: &TextDocument, annotations: &[Annotation]) -> Option<Vec<HiddenSpan>> {
    if !has_redaction(annotations) {
        return Some(Vec::new());
    }
    if !doc.positioned {
        return None;
    }
    let redactions = redaction_rects(annotations);
    let mut out = Vec::new();
    for (i, line) in doc.lines.iter().enumerate() {
        out.extend(
            hidden_in_line(line, &redactions)?
                .into_iter()
                .map(|r| (i, r)),
        );
    }
    Some(out)
}

/// `doc`'s layout text with every word `annotations` hide masked
/// ([`hidden_spans`], [`masked_layout_text`]); `None` when it can't be
/// masked and must be withheld.
pub fn masked_text(doc: &TextDocument, annotations: &[Annotation]) -> Option<String> {
    Some(masked_layout_text(doc, &hidden_spans(doc, annotations)?))
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

fn mark_all(hay: &[char], needle: &[char], mask: &mut [bool]) -> usize {
    let mut from = 0;
    let mut hits = 0;
    while let Some(p) = find_from(hay, needle, from) {
        mask[p..p + needle.len()].iter_mut().for_each(|m| *m = true);
        hits += 1;
        from = p + 1;
    }
    hits
}

/// Hidden words this long or longer are masked wherever they occur in
/// the layout text, not only at their line's position: a secret is long,
/// and the layout may repeat it where no line accounts for it. Shorter
/// hidden words ("a", "to") are masked in place only, so they don't
/// blank every match inside other words.
const GLOBAL_MASK_MIN_CHARS: usize = 4;

/// Masks `word` everywhere in `hay` (and its Markdown-table `\|` escaped
/// form). Returns the number of occurrences masked.
fn mark_word_everywhere(hay: &[char], word: &[char], mask: &mut [bool]) -> usize {
    let mut hits = mark_all(hay, word, mask);
    if word.contains(&'|') {
        let escaped: Vec<char> = word
            .iter()
            .flat_map(|&c| if c == '|' { vec!['\\', '|'] } else { vec![c] })
            .collect();
        hits += mark_all(hay, &escaped, mask);
    }
    hits
}

/// `doc.layout_text` with every hidden word masked.
///
/// The layout text is rebuilt from the lines (indents, table pipes, code
/// fences), so each line is found in it and its hidden spans are masked
/// at every place the line occurs. Fails closed:
/// - hidden words of [`GLOBAL_MASK_MIN_CHARS`]+ chars, and every hidden
///   word of a line that can't be found after the previous one (a
///   multi-column or table layout reorders or normalises it), are masked
///   at **every** occurrence — over-masking beats leaking;
/// - if a hidden word still can't be found anywhere in the layout text,
///   the layout can't be trusted to be masked, so the result is the
///   masked lines joined by newlines instead.
pub fn masked_layout_text(doc: &TextDocument, spans: &[HiddenSpan]) -> String {
    if spans.is_empty() {
        return doc.layout_text.clone();
    }
    let map = by_line(spans);
    let layout: Vec<char> = doc.layout_text.chars().collect();
    let mut mask = vec![false; layout.len()];
    let mut cursor = 0;
    let mut unlocated = false;
    for (i, line) in doc.lines.iter().enumerate() {
        let needle: Vec<char> = line.text.trim_end().chars().collect();
        let found = find_from(&layout, &needle, cursor);
        if let Some(p) = found {
            cursor = p + needle.len();
        }
        let Some(ranges) = map.get(&i) else {
            continue;
        };
        let chars: Vec<char> = line.text.chars().collect();
        // Every occurrence of the line, not only the one the forward
        // search found: a multi-column layout emits columns one after
        // another, so that one may be another line that merely contains
        // this one while the real one lies before the cursor. Over-masking
        // beats leaking.
        let occurrences: Vec<usize> = if found.is_some() {
            let mut at = Vec::new();
            let mut from = 0;
            while let Some(p) = find_from(&layout, &needle, from) {
                at.push(p);
                from = p + 1;
            }
            at
        } else {
            Vec::new()
        };
        for r in ranges {
            let end = r.end.min(chars.len());
            if r.start >= end || chars[r.start..end].iter().all(|c| c.is_whitespace()) {
                continue;
            }
            let word = &chars[r.start..end];
            let mut located = false;
            for &p in &occurrences {
                for k in r.start..end.min(needle.len()) {
                    mask[p + k] = true;
                }
                located = true;
            }
            if found.is_none() || word.len() >= GLOBAL_MASK_MIN_CHARS {
                located |= mark_word_everywhere(&layout, word, &mut mask) > 0;
            }
            unlocated |= !located;
        }
    }
    if unlocated {
        return mask_document_lines(doc, &map).join("\n");
    }
    layout
        .iter()
        .zip(mask)
        .map(|(&c, m)| if m && !c.is_whitespace() { MASK } else { c })
        .collect()
}

fn mask_document_lines(
    doc: &TextDocument,
    map: &BTreeMap<usize, Vec<Range<usize>>>,
) -> Vec<String> {
    doc.lines
        .iter()
        .enumerate()
        .map(|(i, l)| match map.get(&i) {
            Some(ranges) => mask_text(&l.text, ranges),
            None => l.text.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotation::{FillMode, Rgba, ShapeStyle};
    use crate::text_lines::WordRect;

    #[test]
    fn redactions_in_image_drop_zero_area_and_edge_only_rects() {
        let px = |rect| Annotation::Pixelate {
            rect,
            block_size: 4.0,
        };
        let inside = px(RectLike::new(2.0, 2.0, 6.0, 4.0));
        let out = redactions_in_image(
            &[
                // Only meets the right edge, the bottom edge, a corner.
                px(RectLike::new(40.0, 2.0, 5.0, 4.0)),
                solid(RectLike::new(2.0, 20.0, 5.0, 4.0)),
                px(RectLike::new(40.0, 20.0, 3.0, 3.0)),
                // Meets the left edge from outside (negative size).
                px(RectLike::new(0.0, 2.0, -5.0, 4.0)),
                // No area.
                px(RectLike::new(10.0, 5.0, 0.0, 4.0)),
                px(RectLike::new(10.0, 5.0, 4.0, 0.0)),
                inside.clone(),
            ],
            40,
            20,
        );
        assert_eq!(out, vec![inside]);
        for (r, touches) in [
            (RectLike::new(40.0, 2.0, 5.0, 4.0), false),
            (RectLike::new(39.5, 2.0, 5.0, 4.0), true),
            (RectLike::new(10.0, 5.0, 0.0, 4.0), false),
            (RectLike::new(f32::NAN, 5.0, 4.0, 4.0), false),
            (RectLike::new(5.0, 5.0, f32::NAN, 4.0), false),
            (RectLike::new(45.0, 2.0, -10.0, 4.0), true),
        ] {
            assert_eq!(touches_image(r, 40.0, 20.0), touches, "{r:?}");
        }
    }

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

    #[test]
    fn redactions_in_crop_translate_into_the_cropped_output_and_clip() {
        let crop = crate::render::output_crop(
            400,
            200,
            &[Annotation::Crop {
                rect: RectLike::new(200.0, 0.0, 200.0, 200.0),
            }],
        )
        .expect("a crop");
        let px = |rect| Annotation::Pixelate {
            rect,
            block_size: 4.0,
        };
        let out = redactions_in_crop(
            &[
                px(RectLike::new(300.0, 10.0, 60.0, 20.0)),
                // Straddles the crop's left edge: clipped.
                solid(RectLike::new(150.0, 50.0, 100.0, 10.0)),
                // Wholly outside: dropped.
                px(RectLike::new(10.0, 10.0, 50.0, 50.0)),
            ],
            crop,
        );
        assert_eq!(
            out,
            vec![
                px(RectLike::new(100.0, 10.0, 60.0, 20.0)),
                solid(RectLike::new(0.0, 50.0, 50.0, 10.0)),
            ]
        );
        assert_eq!(crate::render::output_crop(400, 200, &out), None);
    }

    fn crop_of(x: f32, y: f32, w: f32, h: f32) -> crate::render::OutputCrop {
        crate::render::output_crop(
            400,
            200,
            &[Annotation::Crop {
                rect: RectLike::new(x, y, w, h),
            }],
        )
        .expect("a crop")
    }

    #[test]
    fn document_in_crop_keeps_only_words_wholly_inside_the_crop() {
        // x 45–135, y 10–20: "Zx9Qw7" (50–110) is wholly inside; "next"
        // (120–160) straddles the edge and goes whole; "key:" (0–40) and
        // line one are out.
        let crop = crop_of(45.0, 10.0, 90.0, 10.0);
        let out = document_in_crop(&doc(), crop).unwrap();
        assert_eq!(out.lines.len(), 1);
        let line = &out.lines[0];
        assert_eq!(line.text, "Zx9Qw7");
        assert_eq!(out.layout_text, "Zx9Qw7");
        assert!(out.positioned);
        assert_eq!(
            line.words,
            vec![WordRect {
                start: 0,
                end: 6,
                rect: RectLike::new(5.0, 0.0, 60.0, 10.0),
            }]
        );
        assert_eq!(line.rect, Some(RectLike::new(5.0, 0.0, 60.0, 10.0)));
        // A box exactly on the edges is inside.
        let out = document_in_crop(&doc(), crop_of(50.0, 10.0, 110.0, 10.0)).unwrap();
        assert_eq!(out.layout_text, "Zx9Qw7 next");
        // Redactions moved the same way still hide their word.
        let hide = redactions_in_crop(&[solid(RectLike::new(50.0, 10.0, 60.0, 10.0))], crop);
        let out = document_in_crop(&doc(), crop).unwrap();
        assert_eq!(
            masked_layout_text(&out, &hidden_spans(&out, &hide).unwrap()),
            "██████"
        );
    }

    /// One line, one word box over all of `text` at x `x`, 10 px a char.
    fn one_box_line(text: &str, x: f32, y: f32) -> TextDocument {
        let n = text.chars().count();
        TextDocument {
            lines: vec![TextLine {
                text: text.into(),
                rect: Some(RectLike::new(x, y, 10.0 * n as f32, 10.0)),
                words: vec![word(0, n, x, y, 10.0 * n as f32)],
            }],
            positioned: true,
            layout_text: text.into(),
        }
    }

    #[test]
    fn document_in_crop_drops_a_token_cut_by_either_edge() {
        let url = "https://host/reset?token=abc123";
        let d = one_box_line(url, 0.0, 0.0);
        for crop in [
            crop_of(0.0, 0.0, 100.0, 20.0),
            crop_of(255.0, 0.0, 100.0, 20.0),
        ] {
            let out = document_in_crop(&d, crop).unwrap();
            assert_eq!(out.layout_text, "", "{crop:?}");
            assert!(out.lines.is_empty());
        }
        // Wholly inside: kept.
        let out = document_in_crop(&d, crop_of(0.0, 0.0, 320.0, 20.0)).unwrap();
        assert_eq!(out.layout_text, url);
    }

    #[test]
    fn document_in_crop_drops_a_no_space_line_straddling_the_edge() {
        // A CJK line Vision boxes as one word, x 100–190; the crop ends
        // at 150.
        let line = "日本語のテキスト行";
        let d = one_box_line(line, 100.0, 0.0);
        assert_eq!(
            document_in_crop(&d, crop_of(0.0, 0.0, 150.0, 20.0))
                .unwrap()
                .layout_text,
            ""
        );
        assert_eq!(
            document_in_crop(&d, crop_of(0.0, 0.0, 200.0, 20.0))
                .unwrap()
                .layout_text,
            line
        );
    }

    #[test]
    fn document_in_crop_drops_a_word_straddling_a_horizontal_edge() {
        // y 5–20: line one (y 0–10) straddles the top edge; line two
        // (y 10–20) is wholly inside.
        let out = document_in_crop(&doc(), crop_of(0.0, 5.0, 200.0, 15.0)).unwrap();
        assert_eq!(out.layout_text, "key: Zx9Qw7 next");
    }

    #[test]
    fn document_in_crop_follows_a_flipped_document() {
        use crate::document::{map_text_document, FlipAxis, Transform};
        // "abc123XYZ other" in a 200 px wide image: "abc123XYZ" x 0–90,
        // "other" x 100–150. Flipped, they sit at x 110–200 and 50–100,
        // with "abc123XYZ" reading right to left inside its box.
        let d = TextDocument {
            lines: vec![TextLine {
                text: "abc123XYZ other".into(),
                rect: Some(RectLike::new(0.0, 0.0, 150.0, 10.0)),
                words: vec![word(0, 9, 0.0, 0.0, 90.0), word(10, 15, 100.0, 0.0, 50.0)],
            }],
            positioned: true,
            layout_text: "abc123XYZ other".into(),
        };
        let (m, _) = Transform::default()
            .flipped(FlipAxis::Horizontal)
            .affine(200, 100);
        let flipped = map_text_document(&d, &m);
        assert_eq!(
            flipped.lines[0].words[0].rect,
            RectLike::new(110.0, 0.0, 90.0, 10.0)
        );
        // The crop's edge (150) runs through "abc123XYZ": none of it is
        // kept (its "23XYZ" would be the cut-away glyphs).
        let out = document_in_crop(&flipped, crop_of(50.0, 0.0, 100.0, 20.0)).unwrap();
        assert_eq!(out.layout_text, "other");
    }

    #[test]
    fn document_in_crop_follows_a_rotated_document() {
        use crate::document::{map_text_document, Transform};
        // "abc" (0, 0, 30, 10) and "xyz" (100, 40, 30, 10) of a 200 × 100
        // image, turned 90° clockwise: tall, narrow boxes.
        let d = TextDocument {
            lines: vec![TextLine {
                text: "abc xyz".into(),
                rect: Some(RectLike::new(0.0, 0.0, 130.0, 50.0)),
                words: vec![word(0, 3, 0.0, 0.0, 30.0), word(4, 7, 100.0, 40.0, 30.0)],
            }],
            positioned: true,
            layout_text: "abc xyz".into(),
        };
        let (m, _) = Transform::default().rotated_cw().affine(200, 100);
        let turned = map_text_document(&d, &m);
        let boxes: Vec<RectLike> = turned.lines[0].words.iter().map(|w| w.rect).collect();
        assert_eq!(
            boxes,
            vec![
                RectLike::new(90.0, 0.0, 10.0, 30.0),
                RectLike::new(50.0, 100.0, 10.0, 30.0),
            ]
        );
        // y 0–115: "abc" wholly inside, "xyz" (y 100–130) straddles.
        let out = document_in_crop(&turned, crop_of(40.0, 0.0, 60.0, 115.0)).unwrap();
        assert_eq!(out.layout_text, "abc");
        let out = document_in_crop(&turned, crop_of(40.0, 0.0, 60.0, 130.0)).unwrap();
        assert_eq!(out.layout_text, "abc xyz");
    }

    #[test]
    fn document_in_crop_drops_unboxed_text_and_unusable_boxes() {
        // No word boxes at all: nothing is kept, however wide the crop.
        let d = TextDocument {
            lines: vec![TextLine {
                text: "alpha beta".into(),
                rect: Some(RectLike::new(0.0, 0.0, 100.0, 10.0)),
                words: Vec::new(),
            }],
            positioned: true,
            layout_text: "alpha beta".into(),
        };
        let out = document_in_crop(&d, crop_of(0.0, 0.0, 400.0, 200.0)).unwrap();
        assert!(out.lines.is_empty());
        assert_eq!(out.layout_text, "");
        // Chars no word box accounts for, and a NaN box, are dropped.
        let mixed = TextDocument {
            lines: vec![TextLine {
                text: "alpha beta gamma".into(),
                rect: Some(RectLike::new(0.0, 0.0, 160.0, 10.0)),
                words: vec![
                    word(0, 5, 0.0, 0.0, 50.0),
                    WordRect {
                        start: 6,
                        end: 10,
                        rect: RectLike::new(60.0, 0.0, f32::NAN, 10.0),
                    },
                ],
            }],
            positioned: true,
            layout_text: "alpha beta gamma".into(),
        };
        let out = document_in_crop(&mixed, crop_of(0.0, 0.0, 400.0, 200.0)).unwrap();
        assert_eq!(out.layout_text, "alpha");
        // No line box at all: fail closed.
        let mut boxless = d.clone();
        boxless.lines[0].rect = None;
        assert_eq!(
            document_in_crop(&boxless, crop_of(0.0, 0.0, 100.0, 10.0)),
            None
        );
    }

    #[test]
    fn document_in_crop_fails_closed_without_positions() {
        let d = TextDocument::from_plain("alpha beta");
        assert_eq!(document_in_crop(&d, crop_of(0.0, 0.0, 10.0, 10.0)), None);
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
    fn a_word_covered_at_all_is_hidden() {
        let d = doc();
        // Half, then a sixth, of the 60 px of "Zx9Qw7": hidden either way.
        for w in [30.0, 10.0] {
            let anns = [solid(RectLike::new(50.0, 10.0, w, 10.0))];
            assert_eq!(hidden_spans(&d, &anns).unwrap(), vec![(1, 5..11)]);
        }
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
        assert_eq!(hidden_spans(&d, &anns).unwrap(), vec![(1, 5..11)]);
    }

    #[test]
    fn non_redaction_annotations_hide_nothing() {
        let d = doc();
        let anns = [Annotation::Highlighter {
            points: vec![],
            color: Rgba::OPAQUE_BLACK,
            line_width: 4.0,
        }];
        assert!(hidden_spans(&d, &anns).unwrap().is_empty());
        assert!(!has_redaction(&anns));
    }

    #[test]
    fn tokens_without_word_boxes_are_hidden_with_their_line() {
        let mut d = doc();
        d.lines[1].words.clear();
        let anns = [solid(RectLike::new(50.0, 10.0, 60.0, 10.0))];
        assert_eq!(
            hidden_spans(&d, &anns).unwrap(),
            vec![(1, 0..4), (1, 5..11), (1, 12..16)]
        );
        // Line one (y 0–10) is only touched: untouched.
        let masked = mask_document(&d, &hidden_spans(&d, &anns).unwrap());
        assert_eq!(masked.lines[0].text, "intro line");
    }

    #[test]
    fn masking_keeps_char_counts_and_line_structure() {
        let d = doc();
        let anns = [solid(RectLike::new(50.0, 10.0, 60.0, 10.0))];
        let spans = hidden_spans(&d, &anns).unwrap();
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
        let spans = hidden_spans(&d, &anns).unwrap();
        assert_eq!(
            masked_layout_text(&d, &spans),
            "| key: | ██████ | next |\nagain ██████"
        );
    }

    #[test]
    fn without_positions_any_redaction_withholds_the_text() {
        // Nothing ties a word to a place on the image, so nothing can say
        // which words a hand-drawn redaction hides: the text is withheld
        // (`None`), never stored with only the detected items masked.
        let email = ["someone", "@", "example.com"].concat();
        let d = TextDocument::from_plain(&format!("Patient: John Smith\nmail {email} now"));
        let anns = [solid(RectLike::new(0.0, 0.0, 4.0, 4.0))];
        assert_eq!(hidden_spans(&d, &anns), None);
        // No redaction: nothing is hidden, the text is fine as is.
        assert_eq!(hidden_spans(&d, &[]), Some(Vec::new()));
        // A positioned document with a line that lost its box can't be
        // placed either.
        let mut boxless = doc();
        boxless.lines[1].rect = None;
        assert_eq!(hidden_spans(&boxless, &anns), None);
        // A line box with no usable size: its unboxed words are hidden.
        let mut degenerate = doc();
        degenerate.lines[0].rect = Some(RectLike::new(f32::NAN, 0.0, 10.0, 10.0));
        degenerate.lines[0].words.clear();
        assert_eq!(
            hidden_spans(&degenerate, &anns).unwrap(),
            vec![(0, 0..5), (0, 6..10)]
        );
    }

    #[test]
    fn no_spans_returns_the_text_unchanged() {
        let d = doc();
        assert_eq!(masked_layout_text(&d, &[]), d.layout_text);
        assert_eq!(mask_document(&d, &[]), d);
    }

    #[test]
    fn chars_outside_every_word_box_are_hidden_with_their_line() {
        let mut d = doc();
        // Only "key:" has a box; "Zx9Qw7" and "next" have none, and the
        // engine boxed the first half of a glued token "key:Zx9Qw7".
        d.lines[1].text = "key:Zx9Qw7 next".into();
        d.lines[1].rect = Some(RectLike::new(0.0, 10.0, 150.0, 10.0));
        d.lines[1].words = vec![word(0, 4, 0.0, 10.0, 40.0)];
        d.layout_text = "intro line\nkey:Zx9Qw7 next".into();
        // Cover x 40..100. The unboxed chars have no trusted place in the
        // line, so the cover hides all of them; "key:" (x 0–40, only
        // touched) stays.
        let anns = [solid(RectLike::new(40.0, 10.0, 60.0, 10.0))];
        let spans = hidden_spans(&d, &anns).unwrap();
        assert_eq!(spans, vec![(1, 4..10), (1, 11..15)]);
        assert_eq!(
            masked_layout_text(&d, &spans),
            "intro line\nkey:██████ ████"
        );
    }

    #[test]
    fn a_short_hidden_word_is_masked_in_a_multi_column_layout() {
        // Lines in reading order (row-major), but the layout emits the
        // left column, then the right. The left "PIN is 042" also occurs
        // inside a later right-column line; the forward search used to
        // find only that one and leave the real "042" readable.
        let line = |text: &str| TextLine {
            text: text.into(),
            rect: None,
            words: Vec::new(),
        };
        let d = TextDocument {
            lines: vec![
                line("Left top"),
                line("Right top"),
                line("PIN is 042"),
                line("PIN is 042 was reset"),
            ],
            positioned: true,
            layout_text: "Left top\nPIN is 042\n\nRight top\nPIN is 042 was reset".into(),
        };
        let masked = masked_layout_text(&d, &[(2, 7..10)]);
        assert_eq!(masked.lines().nth(1), Some("PIN is ███"), "{masked}");
    }

    #[test]
    fn long_hidden_words_are_masked_wherever_they_appear_in_the_layout() {
        let mut d = doc();
        // The line is found, but the same secret also sits in a part of
        // the layout no line accounts for (e.g. a code fence header).
        d.layout_text = "Zx9Qw7\nintro line\n  key: Zx9Qw7 next".into();
        let anns = [solid(RectLike::new(50.0, 10.0, 60.0, 10.0))];
        let spans = hidden_spans(&d, &anns).unwrap();
        let masked = masked_layout_text(&d, &spans);
        assert!(!masked.contains("Zx9Qw7"), "{masked}");
    }

    #[test]
    fn a_hidden_word_missing_from_the_layout_falls_back_to_the_masked_lines() {
        let mut d = doc();
        // The layout spells the line differently (normalised), so
        // neither the line nor the word can be found in it.
        d.layout_text = "intro line\nkey: Zx9 Qw7 next".into();
        let anns = [solid(RectLike::new(50.0, 10.0, 60.0, 10.0))];
        let spans = hidden_spans(&d, &anns).unwrap();
        let masked = masked_layout_text(&d, &spans);
        assert_eq!(masked, "intro line\nkey: ██████ next");
    }

    /// "password=hunter2", one Vision box over the whole token, x 0–160.
    fn password_line() -> TextDocument {
        one_box_line("password=hunter2", 0.0, 0.0)
    }

    #[test]
    fn a_word_partly_covered_is_hidden_in_full() {
        // A Blur over "hunter2" only (x 90–160, 44 % of the box): the
        // image hides it, so the text must too — the whole token goes.
        let d = password_line();
        for a in [
            Annotation::Blur {
                rect: RectLike::new(90.0, 0.0, 70.0, 10.0),
                radius: 2.0,
            },
            solid(RectLike::new(150.0, 2.0, 10.0, 6.0)),
            Annotation::Pixelate {
                rect: RectLike::new(-20.0, -5.0, 22.0, 30.0),
                block_size: 4.0,
            },
        ] {
            let spans = hidden_spans(&d, std::slice::from_ref(&a)).unwrap();
            assert_eq!(spans, vec![(0, 0..16)], "{a:?}");
            let masked = masked_layout_text(&d, &spans);
            assert!(!masked.contains("hunter2"), "{masked}");
        }
    }

    #[test]
    fn hairline_contact_does_not_hide_a_neighbouring_word() {
        // "key: Zx9Qw7 next": "next" is x 120–160. A cover whose edge
        // meets it within the hairline tolerance (under 1 px deep and
        // under 1 % of the box) leaves it; 1 px deep hides it.
        let d = doc();
        let cover = |x1: f32| solid(RectLike::new(50.0, 10.0, x1 - 50.0, 10.0));
        // 0.9 px into "next": 9 px² of 400 (2.25 %) — over 1 % of the
        // box, so not hairline: hidden.
        assert_eq!(
            hidden_spans(&d, &[cover(120.9)]).unwrap(),
            vec![(1, 5..11), (1, 12..16)]
        );
        // 0.3 px: 3 px² (0.75 %) and under 1 px: hairline, ignored.
        assert_eq!(hidden_spans(&d, &[cover(120.3)]).unwrap(), vec![(1, 5..11)]);
        // Exactly 1 px deep is never hairline, even if tiny in area.
        let mut wide = one_box_line(&"w".repeat(100), 0.0, 0.0);
        wide.lines[0].words[0].rect = RectLike::new(0.0, 0.0, 1000.0, 10.0);
        let edge = |w: f32| solid(RectLike::new(1000.0 - w, 0.0, 50.0, 10.0));
        assert_eq!(hidden_spans(&wide, &[edge(1.0)]).unwrap().len(), 1);
        // 0.9 px deep, 9 px² of 10 000 (0.09 %): hairline.
        assert!(hidden_spans(&wide, &[edge(0.9)]).unwrap().is_empty());
        // Merely touching (0 px) is no overlap.
        assert!(hidden_spans(&wide, &[edge(0.0)]).unwrap().is_empty());
    }

    #[test]
    fn several_hairlines_that_add_up_hide_the_word() {
        let mut wide = one_box_line(&"w".repeat(100), 0.0, 0.0);
        wide.lines[0].words[0].rect = RectLike::new(0.0, 0.0, 100.0, 10.0);
        // Two 0.6 px slivers: 6 px² each (0.6 %), 1.2 % together.
        let anns = [
            solid(RectLike::new(-10.0, 0.0, 10.6, 10.0)),
            solid(RectLike::new(99.4, 0.0, 10.0, 10.0)),
        ];
        assert_eq!(hidden_spans(&wide, &anns).unwrap().len(), 1);
        assert!(hidden_spans(&wide, &anns[..1]).unwrap().is_empty());
    }

    /// "SECRET1234 and some other words here" (36 chars) with a line box
    /// x 10–370, y 10–30 of a 400 × 50 image and no word boxes; the real
    /// glyphs of "SECRET1234" are x 10–110.
    fn boxless_line() -> TextDocument {
        TextDocument {
            lines: vec![TextLine {
                text: "SECRET1234 and some other words here".into(),
                rect: Some(RectLike::new(10.0, 10.0, 360.0, 20.0)),
                words: Vec::new(),
            }],
            positioned: true,
            layout_text: "SECRET1234 and some other words here".into(),
        }
    }

    fn every_orientation() -> Vec<(&'static str, crate::document::Transform)> {
        use crate::document::{FlipAxis, Transform};
        let t = Transform::default();
        vec![
            ("identity", t),
            ("flip h", t.flipped(FlipAxis::Horizontal)),
            ("flip v", t.flipped(FlipAxis::Vertical)),
            ("turn 90", t.rotated_cw()),
            ("turn 180", t.rotated_cw().rotated_cw()),
            ("turn 270", t.rotated_ccw()),
            (
                "turn 90 + flip h",
                t.rotated_cw().flipped(FlipAxis::Horizontal),
            ),
        ]
    }

    #[test]
    fn a_boxless_token_is_hidden_whatever_the_orientation() {
        use crate::document::{map_annotation, map_text_document};
        let d = boxless_line();
        let secret = solid(RectLike::new(10.0, 10.0, 100.0, 20.0));
        for (name, t) in every_orientation() {
            let (m, _) = t.affine(400, 50);
            let moved = map_text_document(&d, &m);
            // The user covers where the secret really is now.
            let cover = map_annotation(&secret, &m);
            let spans = hidden_spans(&moved, std::slice::from_ref(&cover)).unwrap();
            let masked = masked_layout_text(&moved, &spans);
            assert!(!masked.contains("SECRET1234"), "{name}: {masked}");
        }
    }

    #[test]
    fn a_redaction_off_a_boxless_line_hides_none_of_it() {
        let d = boxless_line();
        let elsewhere = solid(RectLike::new(10.0, 35.0, 100.0, 10.0));
        assert!(hidden_spans(&d, &[elsewhere]).unwrap().is_empty());
    }

    fn magnifier(source: (f32, f32), bubble: (f32, f32)) -> Annotation {
        Annotation::Magnifier {
            source: crate::PointLike::new(source.0, source.1),
            radius: 10.0,
            bubble: crate::PointLike::new(bubble.0, bubble.1),
            zoom: 2.0,
            shape: Default::default(),
            color: Rgba::OPAQUE_BLACK,
        }
    }

    #[test]
    fn a_bubble_magnifying_a_redaction_is_covered() {
        let blur = Annotation::Blur {
            rect: RectLike::new(0.0, 0.0, 30.0, 10.0),
            radius: 2.0,
        };
        // Source (15, 5) r 10 sits on the blur; bubble at (100, 100), r 20.
        let over = magnifier((15.0, 5.0), (100.0, 100.0));
        let out = magnified_redactions(&[blur.clone(), over.clone()]);
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].redaction_rect(),
            Some(RectLike::new(78.0, 78.0, 44.0, 44.0))
        );
        // A word recognized inside the bubble is masked.
        let d = one_box_line("Zx9Qw7", 85.0, 95.0);
        let mut hide = vec![blur.clone(), over];
        hide.extend(out);
        assert_eq!(masked_text(&d, &hide).as_deref(), Some("██████"));
        // A bubble of plain pixels is left alone.
        let elsewhere = magnifier((200.0, 200.0), (300.0, 300.0));
        assert!(magnified_redactions(&[blur.clone(), elsewhere]).is_empty());
        // A bubble magnifying that bubble is covered too (drawing order).
        let chained = magnifier((100.0, 100.0), (400.0, 400.0));
        let first = magnifier((15.0, 5.0), (100.0, 100.0));
        assert_eq!(magnified_redactions(&[blur, first, chained]).len(), 2);
    }
}
