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

/// `redactions` (base px) moved into the pixels of an output cropped to
/// `crop` ([`crate::render::output_crop`]): translated by minus the crop
/// origin and clipped to the output, dropping any left with no area.
/// What a scrub that bakes a Crop stores, so the redactions line up with
/// the stored (cropped) image. Only redactions ([`is_redaction`]) are kept.
pub fn redactions_in_crop(
    redactions: &[Annotation],
    crop: crate::render::OutputCrop,
) -> Vec<Annotation> {
    let clip = |r: RectLike| rect_in_crop(r, crop);
    redactions
        .iter()
        .filter(|a| is_redaction(a))
        .filter_map(|a| {
            let mut out = a.clone();
            match &mut out {
                Annotation::Rectangle { rect, .. }
                | Annotation::Blur { rect, .. }
                | Annotation::Pixelate { rect, .. } => *rect = clip(*rect)?,
                _ => return None,
            }
            Some(out)
        })
        .collect()
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

/// `doc` (positions in base px) as it reads in an output cropped to
/// `crop` ([`crate::render::output_crop`]): only characters estimated to
/// lie **wholly** inside the crop, with their boxes moved into the cropped
/// output's pixels (the frame [`redactions_in_crop`] stores redactions
/// in). What a History record whose stored image is that crop may say:
/// never a character from the cut-away area, at the cost of a few edge
/// characters.
///
/// - A word box wholly inside the crop keeps all its characters.
/// - A box not wholly inside vertically, or wholly outside, keeps none.
/// - A box straddling a vertical crop edge is split into equal slots, one
///   per grapheme cluster (box width / cluster count); a cluster is kept
///   only when its slot lies entirely inside the crop (rounded inward).
///   This covers a long token or URL cut by the edge, and a no-space
///   script line (CJK, Thai) the engine boxed as one word.
/// - Characters no word box accounts for get the same slots across their
///   line box (clusters of the whole line).
/// - A box whose slots can't be computed (zero, negative or non-finite
///   size) keeps nothing; a slot is never widened.
///
/// A line's kept tokens are joined by single spaces; lines with nothing
/// kept are dropped; `layout_text` is the kept lines joined by newlines
/// (the original layout can't be cut to the crop).
///
/// `None` without positions, or when a line has no box: nothing tells
/// which text the crop keeps, so callers store no text (fail closed).
pub fn document_in_crop(
    doc: &TextDocument,
    crop: crate::render::OutputCrop,
) -> Option<TextDocument> {
    use crate::text_lines::WordRect;
    if !doc.positioned {
        return None;
    }
    let mut lines = Vec::new();
    for line in &doc.lines {
        let line_rect = line.rect?;
        let chars: Vec<char> = line.text.chars().collect();
        let n = chars.len();
        let usable: Vec<_> = line
            .words
            .iter()
            .filter(|w| w.start < w.end.min(n))
            .collect();
        let covered = |i: usize| usable.iter().any(|w| w.start <= i && i < w.end);
        // Kept char ranges, each with its box in cropped-output px.
        let mut kept: Vec<(Range<usize>, RectLike)> = Vec::new();
        for w in &usable {
            let span = w.start..w.end.min(n);
            let clusters: Vec<Range<usize>> = graphemes(&chars[span.clone()])
                .into_iter()
                .map(|c| c.start + span.start..c.end + span.start)
                .collect();
            if let Some(k) = slots_in_crop(w.rect, &clusters, 0..clusters.len(), crop) {
                kept.push(k);
            }
        }
        // Chars no word box accounts for: slots across the line box.
        let line_clusters = graphemes(&chars);
        let mut c = 0;
        while c < line_clusters.len() {
            let unboxed = |c: usize| {
                let r = &line_clusters[c];
                !chars[r.clone()].iter().all(|ch| ch.is_whitespace()) && !r.clone().any(covered)
            };
            if !unboxed(c) {
                c += 1;
                continue;
            }
            let run_start = c;
            while c < line_clusters.len() && unboxed(c) {
                c += 1;
            }
            if let Some(k) = slots_in_crop(line_rect, &line_clusters, run_start..c, crop) {
                kept.push(k);
            }
        }
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
        if len == 0 {
            continue;
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
        lines.push(TextLine { text, rect, words });
    }
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

/// Of `clusters` laid out in equal slots across `rect` (cluster `i` in
/// slot `i`), those at `pick` whose slot is wholly inside `crop`: their
/// char range and box (cropped-output px). `None` when none is, when
/// `rect` isn't wholly inside the crop vertically, or when the slots
/// can't be computed. The kept clusters are contiguous (one crop rect).
fn slots_in_crop(
    rect: RectLike,
    clusters: &[Range<usize>],
    pick: Range<usize>,
    crop: crate::render::OutputCrop,
) -> Option<(Range<usize>, RectLike)> {
    let count = clusters.len();
    let finite = [rect.x, rect.y, rect.width, rect.height, crop.x, crop.y]
        .iter()
        .all(|v| v.is_finite());
    if !finite || rect.width <= 0.0 || rect.height <= 0.0 || count == 0 {
        return None;
    }
    let (cx0, cy0) = (crop.x, crop.y);
    let (cx1, cy1) = (cx0 + crop.width as f32, cy0 + crop.height as f32);
    if rect.y < cy0 || rect.y + rect.height > cy1 {
        return None;
    }
    let slot = rect.width / count as f32;
    if !slot.is_finite() || slot <= 0.0 {
        return None;
    }
    let inside = |i: usize| {
        let x0 = rect.x + slot * i as f32;
        let x1 = rect.x + slot * (i + 1) as f32;
        x0 >= cx0 && x1 <= cx1
    };
    let first = pick.clone().find(|&i| inside(i))?;
    let last = pick.rev().find(|&i| inside(i))?;
    let x0 = rect.x + slot * first as f32;
    let x1 = rect.x + slot * (last + 1) as f32;
    Some((
        clusters[first].start..clusters[last].end,
        RectLike::new(x0 - cx0, rect.y - cy0, x1 - x0, rect.height),
    ))
}

/// Grapheme clusters of `chars` (char ranges), approximated without a
/// Unicode segmentation table: combining marks, variation selectors,
/// emoji modifiers and tags join the preceding cluster, and a zero-width
/// joiner joins its neighbours. Close enough for slot estimates; a mark
/// split off can only shrink what is kept, never widen a slot.
fn graphemes(chars: &[char]) -> Vec<Range<usize>> {
    fn extends(c: char) -> bool {
        matches!(c as u32,
            0x0300..=0x036F
            | 0x0483..=0x0489
            | 0x0591..=0x05BD
            | 0x0610..=0x061A
            | 0x064B..=0x065F
            | 0x0900..=0x0903
            | 0x093A..=0x094F
            | 0x0E31 | 0x0E34..=0x0E3A | 0x0E47..=0x0E4E
            | 0x1AB0..=0x1AFF
            | 0x1DC0..=0x1DFF
            | 0x200C..=0x200D
            | 0x20D0..=0x20FF
            | 0x3099..=0x309A
            | 0xFE00..=0xFE0F
            | 0xFE20..=0xFE2F
            | 0x1F3FB..=0x1F3FF
            | 0xE0020..=0xE007F
            | 0xE0100..=0xE01EF)
    }
    let mut out: Vec<Range<usize>> = Vec::new();
    for (i, &c) in chars.iter().enumerate() {
        let joins = extends(c) || (i > 0 && chars[i - 1] == '\u{200D}');
        match out.last_mut() {
            Some(last) if joins => last.end = i + 1,
            _ => out.push(i..i + 1),
        }
    }
    out
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
            if is_hidden(slice(line_rect, n, tok.clone()), redactions) {
                out.push(tok);
            }
            continue;
        }
        for w in &boxed {
            if is_hidden(w.rect, redactions) {
                out.push(tok.start.max(w.start)..tok.end.min(w.end).min(n));
            }
        }
        // Chars of the token no word box accounts for (an engine that
        // boxed only part of a glued token) are tested on their own, by
        // their slice of the line box — never skipped.
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
            if is_hidden(slice(line_rect, n, rs..i), redactions) {
                out.push(rs..i);
            }
        }
    }
    out
}

/// Proportional slice of `line` for chars `r` of an `n`-char line.
fn slice(line: RectLike, n: usize, r: Range<usize>) -> RectLike {
    let nf = n as f32;
    let x0 = line.x + line.width * r.start as f32 / nf;
    let x1 = line.x + line.width * r.end as f32 / nf;
    RectLike::new(x0, line.y, x1 - x0, line.height)
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
/// fences), so each line is found in it in reading order and its hidden
/// spans are masked in place. Fails closed:
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
        for r in ranges {
            let end = r.end.min(chars.len());
            if r.start >= end || chars[r.start..end].iter().all(|c| c.is_whitespace()) {
                continue;
            }
            let word = &chars[r.start..end];
            let mut located = false;
            if let Some(p) = found {
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
    fn document_in_crop_keeps_only_text_inside_the_crop_in_cropped_pixels() {
        // x 45–135, y 10–20: "Zx9Qw7" (50–110) is wholly inside; "next"
        // (120–160, 10 px a char) straddles the edge: only "n" (120–130)
        // is wholly inside. "key:" (0–40) and line one are out.
        let crop = crop_of(45.0, 10.0, 90.0, 10.0);
        let out = document_in_crop(&doc(), crop).unwrap();
        assert_eq!(out.lines.len(), 1);
        let line = &out.lines[0];
        assert_eq!(line.text, "Zx9Qw7 n");
        assert_eq!(out.layout_text, "Zx9Qw7 n");
        assert!(out.positioned);
        assert_eq!(
            line.words,
            vec![
                WordRect {
                    start: 0,
                    end: 6,
                    rect: RectLike::new(5.0, 0.0, 60.0, 10.0),
                },
                WordRect {
                    start: 7,
                    end: 8,
                    rect: RectLike::new(75.0, 0.0, 10.0, 10.0),
                },
            ]
        );
        assert_eq!(line.rect, Some(RectLike::new(5.0, 0.0, 80.0, 10.0)));
        // Rounded inward: a slot even partly outside goes ("n" ends at
        // 130, past a crop ending at 125).
        let out = document_in_crop(&doc(), crop_of(45.0, 10.0, 80.0, 10.0)).unwrap();
        assert_eq!(out.layout_text, "Zx9Qw7");
        // Redactions moved the same way still hide their word.
        let hide = redactions_in_crop(&[solid(RectLike::new(50.0, 10.0, 60.0, 10.0))], crop);
        let out = document_in_crop(&doc(), crop).unwrap();
        assert_eq!(
            masked_layout_text(&out, &hidden_spans(&out, &hide)),
            "██████ n"
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
    fn document_in_crop_cuts_a_token_at_the_crop_edge() {
        let url = "https://host/reset?token=abc123";
        // Only x 0–100 is kept: the first ten characters.
        let out =
            document_in_crop(&one_box_line(url, 0.0, 0.0), crop_of(0.0, 0.0, 100.0, 20.0)).unwrap();
        assert_eq!(out.layout_text, "https://ho");
        assert!(!out.layout_text.contains("token"));
        assert_eq!(
            out.lines[0].words,
            vec![WordRect {
                start: 0,
                end: 10,
                rect: RectLike::new(0.0, 0.0, 100.0, 10.0),
            }]
        );
        // Cut on the left edge too.
        let out = document_in_crop(
            &one_box_line(url, 0.0, 0.0),
            crop_of(255.0, 0.0, 100.0, 20.0),
        )
        .unwrap();
        assert_eq!(out.layout_text, "bc123");
    }

    #[test]
    fn document_in_crop_cuts_a_no_space_line_boxed_as_one_word() {
        // A CJK line Vision boxes as one word, x 100–190; the crop ends
        // at 150.
        let line = "日本語のテキスト行";
        let out = document_in_crop(
            &one_box_line(line, 100.0, 0.0),
            crop_of(0.0, 0.0, 150.0, 20.0),
        )
        .unwrap();
        assert_eq!(out.layout_text, "日本語のテ");
        // A combining mark stays with its base: "é" (e + U+0301) is one
        // 10 px slot of a 2-cluster box.
        let decomposed = "e\u{301}x";
        let d = TextDocument {
            lines: vec![TextLine {
                text: decomposed.into(),
                rect: Some(RectLike::new(0.0, 0.0, 20.0, 10.0)),
                words: vec![word(0, 3, 0.0, 0.0, 20.0)],
            }],
            positioned: true,
            layout_text: decomposed.into(),
        };
        let out = document_in_crop(&d, crop_of(0.0, 0.0, 10.0, 20.0)).unwrap();
        assert_eq!(out.layout_text, "e\u{301}");
    }

    #[test]
    fn document_in_crop_drops_a_word_straddling_a_horizontal_edge() {
        // y 5–20: line one (y 0–10) straddles the top edge; line two
        // (y 10–20) is wholly inside.
        let out = document_in_crop(&doc(), crop_of(0.0, 5.0, 200.0, 15.0)).unwrap();
        assert_eq!(out.layout_text, "key: Zx9Qw7 next");
        assert!(!out.layout_text.contains("intro"));
    }

    #[test]
    fn document_in_crop_cuts_unboxed_text_by_its_line_slots() {
        // No word boxes: 10 px per char of a 100 px line.
        let d = TextDocument {
            lines: vec![TextLine {
                text: "alpha beta".into(),
                rect: Some(RectLike::new(0.0, 0.0, 100.0, 10.0)),
                words: Vec::new(),
            }],
            positioned: true,
            layout_text: "alpha beta".into(),
        };
        let cut = |x: f32| document_in_crop(&d, crop_of(x, 0.0, 100.0 - x, 10.0)).unwrap();
        assert_eq!(cut(60.0).layout_text, "beta");
        assert_eq!(cut(55.0).layout_text, "beta");
        // "b" (60–70) is partly outside x 65: dropped.
        assert_eq!(cut(65.0).layout_text, "eta");
        let out = document_in_crop(&d, crop_of(200.0, 0.0, 40.0, 10.0)).unwrap();
        assert!(out.lines.is_empty());
        assert_eq!(out.layout_text, "");
        // Characters a word box doesn't account for use the line's slots;
        // a box with no usable size keeps nothing.
        let mixed = TextDocument {
            lines: vec![TextLine {
                text: "alpha beta".into(),
                rect: Some(RectLike::new(0.0, 0.0, 100.0, 10.0)),
                words: vec![
                    word(0, 5, 0.0, 0.0, 50.0),
                    WordRect {
                        start: 6,
                        end: 8,
                        rect: RectLike::new(60.0, 0.0, f32::NAN, 10.0),
                    },
                ],
            }],
            positioned: true,
            layout_text: "alpha beta".into(),
        };
        let out = document_in_crop(&mixed, crop_of(0.0, 0.0, 100.0, 10.0)).unwrap();
        assert_eq!(out.layout_text, "alpha ta");
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

    #[test]
    fn chars_of_a_token_outside_every_word_box_are_tested_on_their_own() {
        let mut d = doc();
        // Only "key:" has a box; "Zx9Qw7" and "next" have none, and the
        // engine boxed the first half of a glued token "key:Zx9Qw7".
        d.lines[1].text = "key:Zx9Qw7 next".into();
        d.lines[1].rect = Some(RectLike::new(0.0, 10.0, 150.0, 10.0));
        d.lines[1].words = vec![word(0, 4, 0.0, 10.0, 40.0)];
        d.layout_text = "intro line\nkey:Zx9Qw7 next".into();
        // Cover x 40..100 — the unboxed "Zx9Qw7" only.
        let anns = [solid(RectLike::new(40.0, 10.0, 60.0, 10.0))];
        let spans = hidden_spans(&d, &anns);
        assert_eq!(spans, vec![(1, 4..10)]);
        assert_eq!(
            masked_layout_text(&d, &spans),
            "intro line\nkey:██████ next"
        );
    }

    #[test]
    fn long_hidden_words_are_masked_wherever_they_appear_in_the_layout() {
        let mut d = doc();
        // The line is found, but the same secret also sits in a part of
        // the layout no line accounts for (e.g. a code fence header).
        d.layout_text = "Zx9Qw7\nintro line\n  key: Zx9Qw7 next".into();
        let anns = [solid(RectLike::new(50.0, 10.0, 60.0, 10.0))];
        let spans = hidden_spans(&d, &anns);
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
        let spans = hidden_spans(&d, &anns);
        let masked = masked_layout_text(&d, &spans);
        assert_eq!(masked, "intro line\nkey: ██████ next");
    }
}
