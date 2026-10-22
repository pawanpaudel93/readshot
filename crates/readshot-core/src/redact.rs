//! Smart redact: find emails, phone numbers, API keys /
//! tokens and card numbers in recognized text, and build the redaction
//! annotations that cover them. Pure and local: input is the OCR text
//! and line boxes, output is char spans and ordinary annotations.
//!
//! The detectors are deliberately simple, deterministic scanners (no
//! regex crate, no floating-point entropy) — see the plan's decision
//! list for the exact rules.

use std::collections::BTreeSet;
use std::ops::Range;

use crate::annotation::{Annotation, FillMode, Rgba, ShapeStyle};
use crate::geom::RectLike;
use crate::text_lines::TextDocument;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RedactKind {
    Email,
    Phone,
    ApiKey,
    Card,
}

impl RedactKind {
    /// Chip order on the redact card.
    pub const ALL: [Self; 4] = [Self::Email, Self::Phone, Self::ApiKey, Self::Card];

    /// "2 emails", "1 API key".
    pub fn chip(self, count: usize) -> String {
        let (one, many) = match self {
            Self::Email => ("email", "emails"),
            Self::Phone => ("phone", "phones"),
            Self::ApiKey => ("API key", "API keys"),
            Self::Card => ("card number", "card numbers"),
        };
        format!("{count} {}", if count == 1 { one } else { many })
    }
}

/// A sensitive span: chars `span` of `TextDocument::lines[line]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedactMatch {
    pub line: usize,
    pub span: Range<usize>,
    pub kind: RedactKind,
}

/// How a redaction covers its span. Solid is the default because blur
/// can sometimes be partly reversed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RedactStyle {
    #[default]
    Solid,
    Pixelate,
    Blur,
}

impl RedactStyle {
    pub const ALL: [Self; 3] = [Self::Solid, Self::Pixelate, Self::Blur];

    pub fn label(self) -> &'static str {
        match self {
            Self::Solid => "Solid block",
            Self::Pixelate => "Pixelate",
            Self::Blur => "Blur",
        }
    }
}

impl std::fmt::Display for RedactStyle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Padding around a span, as a fraction of its height, so ascenders and
/// descenders are covered too.
const REDACT_PAD_FRACTION: f32 = 0.15;

/// Every match in every line of `doc`.
pub fn scan(doc: &TextDocument) -> Vec<RedactMatch> {
    doc.lines
        .iter()
        .enumerate()
        .flat_map(|(line, l)| {
            detect_line(&l.text)
                .into_iter()
                .map(move |(span, kind)| RedactMatch { line, span, kind })
        })
        .collect()
}

/// Count per kind, in chip order, skipping kinds with no match.
pub fn summary<'a>(matches: impl IntoIterator<Item = &'a RedactMatch>) -> Vec<(RedactKind, usize)> {
    let mut counts = [0usize; 4];
    for m in matches {
        let i = RedactKind::ALL
            .iter()
            .position(|k| *k == m.kind)
            .unwrap_or(0);
        counts[i] += 1;
    }
    RedactKind::ALL
        .iter()
        .zip(counts)
        .filter(|(_, n)| *n > 0)
        .map(|(k, n)| (*k, n))
        .collect()
}

/// Detection priority when matches overlap: Card > Email > API key >
/// Phone.
fn priority(k: RedactKind) -> u8 {
    match k {
        RedactKind::Card => 3,
        RedactKind::Email => 2,
        RedactKind::ApiKey => 1,
        RedactKind::Phone => 0,
    }
}

/// Sensitive spans in one line: char ranges, sorted, non-overlapping.
///
/// Over-cover, never under-cover: matches that overlap or touch merge
/// into their **union**, labelled with the highest-priority kind among
/// them (Card > Email > API key > Phone). Keeping only one of two
/// overlapping matches would leave the other's protruding part visible
/// (`alice@example.com/<key>`: the key token overlaps the email's domain).
pub fn detect_line(text: &str) -> Vec<(Range<usize>, RedactKind)> {
    let cs: Vec<char> = text.chars().collect();
    let mut all: Vec<(Range<usize>, RedactKind)> = find_cards(&cs)
        .into_iter()
        .map(|r| (r, RedactKind::Card))
        .chain(find_emails(&cs).into_iter().map(|r| (r, RedactKind::Email)))
        .chain(
            find_api_keys(&cs)
                .into_iter()
                .map(|r| (r, RedactKind::ApiKey)),
        )
        .chain(find_phones(&cs).into_iter().map(|r| (r, RedactKind::Phone)))
        .filter(|(r, _)| r.start < r.end)
        .collect();
    all.sort_by_key(|(r, _)| r.start);
    let mut merged: Vec<(Range<usize>, RedactKind)> = Vec::new();
    for (r, k) in all {
        match merged.last_mut() {
            Some((last, lk)) if r.start <= last.end => {
                last.end = last.end.max(r.end);
                if priority(k) > priority(*lk) {
                    *lk = k;
                }
            }
            _ => merged.push((r, k)),
        }
    }
    merged
}

// --- Emails ----------------------------------------------------------

fn is_local(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '.' | '_' | '%' | '+' | '-')
}

fn is_domain(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '.' | '-')
}

fn find_emails(cs: &[char]) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    for (at, &c) in cs.iter().enumerate() {
        if c != '@' {
            continue;
        }
        let mut s = at;
        while s > 0 && is_local(cs[s - 1]) {
            s -= 1;
        }
        while s < at && cs[s] == '.' {
            s += 1;
        }
        if s == at {
            continue;
        }
        let mut e = at + 1;
        while e < cs.len() && is_domain(cs[e]) {
            e += 1;
        }
        while e > at + 1 && matches!(cs[e - 1], '.' | '-') {
            e -= 1;
        }
        let domain: String = cs[at + 1..e].iter().collect();
        let Some(dot) = domain.rfind('.') else {
            continue;
        };
        let tld = &domain[dot + 1..];
        if dot == 0 || tld.chars().count() < 2 || !tld.chars().all(char::is_alphabetic) {
            continue;
        }
        if out.last().is_some_and(|r| r.end > s) {
            continue;
        }
        out.push(s..e);
    }
    out
}

// --- Cards -----------------------------------------------------------

/// Luhn checksum over decimal digits (most significant first).
pub fn luhn_valid(digits: &[u8]) -> bool {
    if digits.len() < 2 {
        return false;
    }
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(k, &d)| {
            let d = u32::from(d);
            if k % 2 == 1 {
                let x = d * 2;
                if x > 9 {
                    x - 9
                } else {
                    x
                }
            } else {
                d
            }
        })
        .sum();
    sum.is_multiple_of(10)
}

/// Card numbers have 12–19 digits (Maestro is the shortest).
const CARD_DIGITS: Range<usize> = 12..20;

/// Issuer prefix and length of a card number (`digits.len()` ≥ 12):
/// Visa, Mastercard, Amex, Discover, JCB, Diners, UnionPay, Maestro.
/// Every `6…` prefix is some network (Discover 6011/644–649/65,
/// UnionPay 62, Maestro 56–69), so it only needs the length.
fn card_network_ok(d: &[u8]) -> bool {
    let n = d.len();
    let prefix = |k: usize| d[..k].iter().fold(0u32, |a, &x| a * 10 + u32::from(x));
    match d[0] {
        4 => matches!(n, 13 | 16 | 19),
        5 => match prefix(2) {
            51..=55 => n == 16,
            50 | 56..=59 => CARD_DIGITS.contains(&n),
            _ => false,
        },
        2 => (2221..=2720).contains(&prefix(4)) && n == 16,
        3 => match prefix(2) {
            34 | 37 => n == 15,
            35 => (16..=19).contains(&n),
            30 | 36 | 38 => (14..=19).contains(&n),
            _ => false,
        },
        6 => CARD_DIGITS.contains(&n),
        _ => false,
    }
}

/// Length of a card group separator at `j`: one or two spaces, a dash or
/// a dot. 0 if there is none.
fn card_sep(cs: &[char], j: usize) -> usize {
    match cs.get(j) {
        Some('-' | '.') => 1,
        Some(' ') if cs.get(j + 1) == Some(&' ') => 2,
        Some(' ') => 1,
        _ => 0,
    }
}

/// Digit-group runs, longest valid prefix first. Linear: each start reads
/// at most `CARD_DIGITS.end` digits, and a failed start resumes at the
/// next group rather than the next char.
fn find_cards(cs: &[char]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        if !cs[i].is_ascii_digit() || (i > 0 && cs[i - 1].is_alphanumeric()) {
            i += 1;
            continue;
        }
        let mut digits: Vec<u8> = Vec::new();
        // (group start, char index just past the group, digits so far)
        let mut groups: Vec<(usize, usize, usize)> = Vec::new();
        let mut j = i;
        'run: loop {
            let gs = j;
            while j < cs.len() && cs[j].is_ascii_digit() {
                digits.push(cs[j] as u8 - b'0');
                j += 1;
                if digits.len() >= CARD_DIGITS.end {
                    break 'run;
                }
            }
            groups.push((gs, j, digits.len()));
            let k = card_sep(cs, j);
            if k > 0 && cs.get(j + k).is_some_and(char::is_ascii_digit) {
                j += k;
            } else {
                break;
            }
        }
        let hit = (0..groups.len()).rev().find_map(|g| {
            let (_, end, n) = groups[g];
            let bounded = end >= cs.len() || !cs[end].is_alphanumeric();
            // Short (12–15 digit) numbers overlap phone numbers, so they
            // must be printed the way cards are (4-6-5, 4-6-4, 4-4-4):
            // groups of 4+, only the last shorter. 16–19 digits pass on
            // network + Luhn alone, so OCR-split groups still count.
            let shaped = n >= 16 || groups[..g].iter().all(|&(gs, ge, _)| ge - gs >= 4);
            (bounded
                && shaped
                && CARD_DIGITS.contains(&n)
                && card_network_ok(&digits[..n])
                && luhn_valid(&digits[..n]))
            .then_some(i..end)
        });
        match hit {
            Some(r) => {
                i = r.end;
                out.push(r);
            }
            None => i = groups.get(1).map_or(j.max(i + 1), |&(gs, _, _)| gs),
        }
    }
    out
}

// --- Phones ----------------------------------------------------------

fn is_phone_char(c: char) -> bool {
    c.is_ascii_digit() || matches!(c, ' ' | '-' | '.' | '(' | ')')
}

/// The most digits (E.164) and chars a phone candidate may span.
const MAX_PHONE_DIGITS: usize = 15;
const MAX_PHONE_CHARS: usize = 32;

/// Plausible phone groupings only:
/// - `+CC` international: 8–15 digits, either one block (`+14155550100`)
///   or a 1–3 digit country code then groups of 1–8 (incl. `(0)`);
/// - NANP 3-3-4 or 1-3-3-4;
/// - national with a leading trunk `0`: 10–11 digits in groups of 2–6.
///
/// Parentheses must balance and dots only appear in 3-3-4.
fn phone_like(span: &[char]) -> bool {
    // Allocation-free: this runs for every group end of every start.
    let (mut opens, mut closes, mut dots) = (0usize, 0usize, false);
    let mut lens = [0usize; MAX_PHONE_DIGITS];
    let mut groups = 0;
    let mut digits = 0;
    let mut prev_digit = false;
    for &c in span {
        match c {
            '(' => opens += 1,
            ')' => closes += 1,
            '.' => dots = true,
            _ => {}
        }
        let digit = c.is_ascii_digit();
        if digit {
            if !prev_digit {
                if groups == lens.len() {
                    return false;
                }
                groups += 1;
            }
            lens[groups - 1] += 1;
            digits += 1;
        }
        prev_digit = digit;
    }
    let lens = &lens[..groups];
    if opens != closes || lens.is_empty() {
        return false;
    }
    if span.first() == Some(&'+') {
        return (8..=MAX_PHONE_DIGITS).contains(&digits)
            && (lens.len() == 1
                || (lens[0] <= 3 && lens[1..].iter().all(|n| (1..=8).contains(n))));
    }
    if dots {
        return lens == [3, 3, 4];
    }
    let first = span.iter().find(|c| c.is_ascii_digit()).copied();
    lens == [3, 3, 4]
        || (lens == [1, 3, 3, 4] && first == Some('1'))
        || (first == Some('0')
            && (10..=11).contains(&digits)
            && lens.len() >= 2
            && lens.iter().all(|n| (2..=6).contains(n)))
}

/// For every group start, the longest plausible phone that starts there.
/// Candidates may overlap each other or a card; `detect_line` keeps the
/// first by priority and start, so a rejected prefix ("2026-10-16 …",
/// "Qty 3 (415) …") never hides the number after it. Linear: each start
/// reads at most `MAX_PHONE_CHARS`.
fn find_phones(cs: &[char]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    for i in 0..cs.len() {
        let c = cs[i];
        let prev = i.checked_sub(1).map(|p| cs[p]);
        let starts = match c {
            '+' | '(' => true,
            // A digit right after `+` or `(` belongs to that start.
            d if d.is_ascii_digit() => !matches!(prev, Some('+' | '(')),
            _ => false,
        };
        if !starts || prev.is_some_and(char::is_alphanumeric) {
            continue;
        }
        let mut best = None;
        let mut digits = 0;
        let mut j = i;
        while j < cs.len() && j - i < MAX_PHONE_CHARS && (j == i || is_phone_char(cs[j])) {
            if cs[j].is_ascii_digit() {
                digits += 1;
                if digits > MAX_PHONE_DIGITS {
                    break;
                }
                let end = j + 1;
                let group_end = end >= cs.len() || !cs[end].is_ascii_digit();
                let bounded = end >= cs.len() || !cs[end].is_alphanumeric();
                if group_end && bounded && phone_like(&cs[i..end]) {
                    best = Some(i..end);
                }
            }
            j += 1;
        }
        out.extend(best);
    }
    out
}

// --- API keys / tokens -------------------------------------------------

/// Known key prefixes and the minimum number of characters after them.
const KEY_PREFIXES: &[(&str, usize)] = &[
    ("sk_live_", 10),
    ("sk_test_", 10),
    ("pk_live_", 10),
    ("pk_test_", 10),
    ("rk_live_", 10),
    ("rk_test_", 10),
    ("github_pat_", 20),
    ("ghp_", 20),
    ("gho_", 20),
    ("ghu_", 20),
    ("ghs_", 20),
    ("ghr_", 20),
    ("glpat-", 16),
    ("xoxb-", 10),
    ("xoxp-", 10),
    ("xoxa-", 10),
    ("xoxs-", 10),
    ("AKIA", 16),
    ("ASIA", 16),
    ("AIza", 30),
    ("sk-", 20),
    ("SG.", 20),
    ("eyJ", 20),
    ("npm_", 36),
    ("hf_", 30),
    ("shpat_", 32),
    ("shpss_", 32),
    ("dop_v1_", 64),
    ("xapp-", 10),
];

/// Minimum length of an unprefixed high-entropy token.
pub const MIN_SECRET_LEN: usize = 20;

fn is_key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '=' | '.' | '/')
}

fn class(c: char) -> u8 {
    if c.is_ascii_uppercase() {
        0
    } else if c.is_ascii_lowercase() {
        1
    } else if c.is_ascii_digit() {
        2
    } else {
        3
    }
}

/// A known key prefix (checked on the whole token), or a token with a
/// random-looking segment when split on `.` and `-` (file names,
/// versions and kebab-case identifiers split into short segments).
pub fn looks_like_secret(token: &str) -> bool {
    if has_key_prefix(token) {
        return true;
    }
    if !token.chars().all(is_key_char) {
        return false;
    }
    token.split(['.', '-']).any(random_segment)
}

/// ≥ 20 chars with upper, lower and digits, ≥ 12 distinct alphanumerics,
/// and random-looking: the alphanumerics change class (upper / lower /
/// digit) on at least 30 % of steps, and under 75 % of them sit in
/// CamelCase words (a capital then 2+ lowercase). Random base62/base64
/// changes class ~62 % of the time and has few such words; identifiers
/// like `SomeJavaClassNameFactoryBean2` are ≥ 90 % words, and
/// lowercase paths change class rarely. Integer-only.
fn random_segment(seg: &str) -> bool {
    if seg.chars().count() < MIN_SECRET_LEN {
        return false;
    }
    let alnum: Vec<char> = seg.chars().filter(char::is_ascii_alphanumeric).collect();
    let has = |k: u8| alnum.iter().any(|&c| class(c) == k);
    if !(has(0) && has(1) && has(2)) {
        return false;
    }
    let distinct = alnum.iter().collect::<BTreeSet<_>>().len();
    let changes = alnum
        .windows(2)
        .filter(|w| class(w[0]) != class(w[1]))
        .count();
    // CamelCase: chars inside "words" (a capital then 2+ lowercase).
    let mut word_chars = 0;
    let mut k = 0;
    while k < alnum.len() {
        let mut e = k + 1;
        if alnum[k].is_ascii_uppercase() {
            while e < alnum.len() && alnum[e].is_ascii_lowercase() {
                e += 1;
            }
            if e - k >= 3 {
                word_chars += e - k;
            }
        }
        k = e;
    }
    distinct >= 12 && changes * 10 >= alnum.len() * 3 && word_chars * 4 < alnum.len() * 3
}

fn has_key_prefix(token: &str) -> bool {
    KEY_PREFIXES
        .iter()
        .any(|(p, body)| token.starts_with(p) && token.chars().count() >= p.chars().count() + body)
}

fn is_data_header_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-' | '/' | ';' | '=')
}

/// Whether `cs[..s]` ends in a well-formed data-URI header:
/// `data:<type>/<subtype>` (chars `[A-Za-z0-9.+-]`), optional `;k=v`
/// parameters, then `;base64,`. A bare `base64,` is not enough.
fn after_data_uri_header(cs: &[char], s: usize) -> bool {
    const MARK: &str = ";base64,";
    const MAX_HEADER: usize = 128;
    let m = MARK.len();
    if s < m || !cs[s - m..s].iter().copied().eq(MARK.chars()) {
        return false;
    }
    let end = s - m;
    let mut b = end;
    while b > 0 && end - b < MAX_HEADER && is_data_header_char(cs[b - 1]) {
        b -= 1;
    }
    if b < 5 || !cs[b - 5..b].iter().copied().eq("data:".chars()) {
        return false;
    }
    // `data:` must start its own token: the line start, whitespace, or an
    // opening quote/bracket/separator. `metadata:…`, `secret=data:…` or
    // `x-data:…` is a word or a value, not a data URI.
    if b > 5
        && !(cs[b - 6].is_whitespace()
            || matches!(cs[b - 6], '"' | '\'' | '(' | '<' | '[' | ',' | ';'))
    {
        return false;
    }
    let header: String = cs[b..end].iter().collect();
    let word = |x: &str| {
        !x.is_empty()
            && x.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-'))
    };
    let mut parts = header.split(';');
    let mime_ok = parts
        .next()
        .and_then(|mime| mime.split_once('/'))
        .is_some_and(|(t, st)| word(t) && word(st));
    mime_ok && parts.all(|p| p.split_once('=').is_some_and(|(k, v)| word(k) && word(v)))
}

fn find_api_keys(cs: &[char]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        if !is_key_char(cs[i]) {
            i += 1;
            continue;
        }
        let mut e = i;
        while e < cs.len() && is_key_char(cs[e]) {
            e += 1;
        }
        let next = e;
        let mut s = i;
        while s < e && matches!(cs[s], '.' | '-' | '=' | '+') {
            s += 1;
        }
        while e > s && matches!(cs[e - 1], '.' | '-') {
            e -= 1;
        }
        let token: String = cs[s..e].iter().collect();
        // The payload of a real `data:<mime>;base64,` URI is content, not
        // a secret — unless it carries a known key prefix.
        let content = !has_key_prefix(&token) && after_data_uri_header(cs, s);
        if !content && looks_like_secret(&token) {
            out.push(s..e);
        }
        i = next;
    }
    out
}

// --- Redaction annotations -------------------------------------------

/// The rect a redaction covers: `span` padded by 15 % of its height and
/// snapped **outward** to whole pixels, clamped to the image. Whole-pixel
/// edges mean the filled block has no partly covered (anti-aliased)
/// pixels. `None` if nothing is left inside the image.
pub fn redaction_rect(span: RectLike, image_w: u32, image_h: u32) -> Option<RectLike> {
    let pad = (span.height * REDACT_PAD_FRACTION).max(1.0);
    let x0 = (span.x - pad).floor().max(0.0);
    let y0 = (span.y - pad).floor().max(0.0);
    let x1 = (span.x + span.width + pad).ceil().min(image_w as f32);
    let y1 = (span.y + span.height + pad).ceil().min(image_h as f32);
    (x1 > x0 && y1 > y0).then(|| RectLike::new(x0, y0, x1 - x0, y1 - y0))
}

/// Where a set of matches go on the base image.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Located {
    /// Each placed match with its redaction rect (from [`redaction_rect`]).
    pub rects: Vec<(RedactMatch, RectLike)>,
    /// Matches that were detected but cannot be placed: the document has
    /// no positions, the line or span no longer exists, or the box lies
    /// outside the image. Never silently dropped: the caller must tell
    /// the user that these were found but not covered.
    pub unlocatable: Vec<RedactMatch>,
}

/// Redaction rects for `matches` in `doc`, in base-image pixels.
pub fn locate<'a>(
    doc: &TextDocument,
    matches: impl IntoIterator<Item = &'a RedactMatch>,
    image_w: u32,
    image_h: u32,
) -> Located {
    let mut out = Located::default();
    for m in matches {
        let rect = doc
            .lines
            .get(m.line)
            .and_then(|l| l.span_rect(m.span.clone()))
            .and_then(|span| redaction_rect(span, image_w, image_h));
        match rect {
            Some(r) => out.rects.push((m.clone(), r)),
            None => out.unlocatable.push(m.clone()),
        }
    }
    out
}

/// An ordinary, editable annotation that covers `rect`.
pub fn redaction_annotation(rect: RectLike, style: RedactStyle) -> Annotation {
    match style {
        RedactStyle::Solid => Annotation::Rectangle {
            rect,
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: ShapeStyle {
                fill: FillMode::Filled,
                ..ShapeStyle::default()
            },
        },
        RedactStyle::Pixelate => Annotation::Pixelate {
            rect,
            block_size: (rect.height / 2.0).max(6.0).round(),
        },
        RedactStyle::Blur => Annotation::Blur {
            rect,
            radius: (rect.height * 0.6).max(6.0).round(),
        },
    }
}

/// Whether the panel's recognized text (OCR of the base image) is still
/// what the output shows. A whitelist: true only when every annotation
/// neither adds nor hides text — Highlighter, non-Filled
/// Rectangle/Ellipse, Spotlight, Line, Arrow. Anything else (Text, Pen,
/// Stamp, NumberedPin, Filled shapes, Blur, Pixelate, Crop, and any
/// future variant) makes Copy All re-OCR the flattened output, so typed
/// text is never dropped and covered text never leaks.
pub fn panel_text_matches_output(annotations: &[Annotation]) -> bool {
    annotations.iter().all(|a| match a {
        Annotation::Rectangle { style, .. } | Annotation::Ellipse { style, .. } => {
            style.fill != FillMode::Filled
        }
        Annotation::Highlighter { .. }
        | Annotation::Spotlight { .. }
        | Annotation::Line { .. }
        | Annotation::Arrow { .. } => true,
        _ => false,
    })
}

/// Whether an existing redaction ([`Annotation::redaction_rect`]: an
/// opaque square-cornered filled rectangle, a blur or a pixelate) already
/// covers `rect`.
pub fn is_covered(rect: RectLike, annotations: &[Annotation]) -> bool {
    annotations
        .iter()
        .any(|a| a.redaction_rect().is_some_and(|r| r.contains(rect)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr_layout::RecognizedLine;
    use crate::text_lines::TextDocument;

    /// Matched text for each detection, for readable assertions.
    fn found(text: &str) -> Vec<(String, RedactKind)> {
        let cs: Vec<char> = text.chars().collect();
        detect_line(text)
            .into_iter()
            .map(|(r, k)| (cs[r].iter().collect(), k))
            .collect()
    }

    // Synthetic, obviously fake key material, assembled at runtime so no
    // literal here resembles a real credential.
    fn fake_stripe_key() -> String {
        ["sk_", "live_", "EXAMPLE0example1EXAMPLE2"].concat()
    }
    fn fake_aws_key() -> String {
        ["AK", "IA", "EXAMPLEEXAMPLE00"].concat()
    }
    fn fake_random_token() -> String {
        ["Zx9Qw7Lm", "3Rt5Vb1N", "c8Kd4Hf2"].concat()
    }

    #[test]
    fn finds_emails() {
        assert_eq!(
            found("Questions? Write to support@example.com."),
            [("support@example.com".to_string(), RedactKind::Email)]
        );
        assert_eq!(
            found("priya.shah+cc@mail.example.org"),
            [(
                "priya.shah+cc@mail.example.org".to_string(),
                RedactKind::Email
            )]
        );
        assert!(found("user@localhost and a@b and x@y.c0m").is_empty());
    }

    #[test]
    fn overlapping_detections_redact_their_union() {
        // The API-key token `example.com/<key>` overlaps the email's
        // domain; dropping it would leave the key after the `/` visible.
        let key = ["aB3dE5fG7h", "J9kL1mN3pQ", "5rS7tU9vW1xY3z"].concat();
        for prefix in ["", "https://"] {
            let text = format!("{prefix}alice@example.com/{key}");
            let got = found(&text);
            assert_eq!(
                got,
                [(format!("alice@example.com/{key}"), RedactKind::Email)],
                "{text}"
            );
        }
        // Adjacent spans stay separate items.
        let email = ["bob", "@", "example.com"].concat();
        let text = format!("{email} {}", fake_stripe_key());
        assert_eq!(found(&text).len(), 2);
    }

    #[test]
    fn finds_phone_numbers() {
        assert_eq!(
            found("Phone +1 (415) 555-0142"),
            [("+1 (415) 555-0142".to_string(), RedactKind::Phone)]
        );
        assert_eq!(found("call 415.555.0142 now")[0].0, "415.555.0142");
        assert_eq!(found("(415) 555-0142")[0].0, "(415) 555-0142");
        assert_eq!(found("1-415-555-0142")[0].0, "1-415-555-0142");
        assert_eq!(found("020 7946 0958")[0].0, "020 7946 0958");
        assert_eq!(found("+44 20 7946 0958")[0].0, "+44 20 7946 0958");
        // Not phones: implausible groupings, dates, IPs, versions, counts.
        for s in [
            "12345 67890",
            "1 299 000 000",
            "on 2026-10-16 12:30",
            "host 192.168.100.200",
            "version 1.2.3 build 4521",
            "order 4155550142",
            "555-0142",
            "4242 4242 4242",
        ] {
            assert!(found(s).is_empty(), "{s}");
        }
    }

    #[test]
    fn finds_cards_by_network_length_and_luhn() {
        assert_eq!(
            found("Card 4242 4242 4242 4242 exp 12/28"),
            [("4242 4242 4242 4242".to_string(), RedactKind::Card)]
        );
        assert_eq!(found("4242-4242-4242-4242")[0].1, RedactKind::Card);
        assert_eq!(found("4242424242424242")[0].1, RedactKind::Card);
        // Public test numbers for each network (all Luhn-valid).
        for s in [
            "5555 5555 5555 4444", // Mastercard 51–55
            "2223 0031 2200 3222", // Mastercard 2221–2720
            "3782 822463 10005",   // Amex, 15 digits
            "6011 1111 1111 1117", // Discover
            "3530 1113 3330 0000", // JCB
            "3056 9309 0259 04",   // Diners, 14 digits
        ] {
            assert_eq!(found(s), [(s.to_string(), RedactKind::Card)], "{s}");
        }
        // A trailing group that breaks Luhn is left off.
        assert_eq!(found("4242 4242 4242 4242 12")[0].0, "4242 4242 4242 4242");
        // Luhn-valid but no card network (prefix 9): not a card.
        assert!(found("9000 0000 0000 0001").is_empty());
        // Luhn failure: no card (and not a phone grouping).
        assert!(found("4242 4242 4242 4241").is_empty());
        assert!(found("Visa ending 4242").is_empty());
        assert!(luhn_valid(&[7, 9, 9, 2, 7, 3, 9, 8, 7, 1, 3]));
        assert!(!luhn_valid(&[7, 9, 9, 2, 7, 3, 9, 8, 7, 1, 4]));
    }

    #[test]
    fn finds_prefixed_and_high_entropy_keys() {
        let stripe = fake_stripe_key();
        assert_eq!(
            found(&format!("API key  {stripe}.")),
            [(stripe.clone(), RedactKind::ApiKey)]
        );
        assert_eq!(found(&fake_aws_key())[0].1, RedactKind::ApiKey);
        for key in [
            ["sk", "-", "EXAMPLEexample0123456789ab"].concat(),
            ["gh", "p_", "EXAMPLEexample0123456789abcdefAB"].concat(),
            ["xo", "xb-", "1234567890-EXAMPLEexample"].concat(),
        ] {
            assert_eq!(found(&key), [(key.clone(), RedactKind::ApiKey)], "{key}");
        }
        let token = fake_random_token();
        assert_eq!(
            found(&format!("token={token}")),
            [(format!("token={token}"), RedactKind::ApiKey)]
        );
        assert!(looks_like_secret(&token));
    }

    #[test]
    fn ignores_identifiers_file_names_versions_hashes_and_uuids() {
        for s in [
            "internationalization2026",
            "ReadshotCaptureCoordinator2",
            "SomeJavaClassNameFactoryBean2",
            "readshot-capture-2026-v0.2",
            "Screenshot2026-10-16at12.30.45PM.png",
            "MyApp-v2.3.1-Release-arm64.dmg",
            "x86_64-Linux-GNU-Ubuntu22.04",
            "iPhone15Pro-Max-256GB-Blue",
            "Win10-x64-Pro-22H2-Build19045",
            "sha512-AbCdEf0123456789",
            "3f786850e387550fdab836ed7e6dc881de23001b",
            "550E8400-E29B-41D4-A716-446655440000",
            "550e8400-e29b-41d4-a716-446655440000",
            "sk-learn",
        ] {
            assert!(found(s).is_empty(), "{s}");
        }
    }

    #[test]
    fn overlaps_resolve_by_priority_and_results_are_sorted() {
        let line = format!(
            "{} then 4242 4242 4242 4242 then a@example.com",
            fake_stripe_key()
        );
        let kinds: Vec<RedactKind> = found(&line).into_iter().map(|(_, k)| k).collect();
        assert_eq!(
            kinds,
            [RedactKind::ApiKey, RedactKind::Card, RedactKind::Email]
        );
    }

    #[test]
    fn scan_and_summary_count_by_kind() {
        let lines = vec![
            RecognizedLine {
                text: "a@example.com b@example.com".into(),
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 0.1,
                words: Vec::new(),
            },
            RecognizedLine {
                text: "+1 (415) 555-0142".into(),
                x: 0.0,
                y: 0.5,
                w: 1.0,
                h: 0.1,
                words: Vec::new(),
            },
        ];
        let doc = TextDocument::from_recognized(&lines, 100, 100);
        let matches = scan(&doc);
        assert_eq!(matches.len(), 3);
        assert_eq!(
            matches[2],
            RedactMatch {
                line: 1,
                span: 0..17,
                kind: RedactKind::Phone
            }
        );
        let chips: Vec<String> = summary(&matches)
            .into_iter()
            .map(|(k, n)| k.chip(n))
            .collect();
        assert_eq!(chips, ["2 emails", "1 phone"]);
        assert_eq!(RedactKind::ApiKey.chip(1), "1 API key");
        assert_eq!(RedactKind::Card.chip(2), "2 card numbers");
    }

    #[test]
    fn redaction_rect_pads_and_snaps_outward_to_whole_pixels() {
        let r = redaction_rect(RectLike::new(10.3, 8.6, 20.2, 9.7), 60, 40).unwrap();
        assert_eq!(r, RectLike::new(8.0, 7.0, 24.0, 13.0));
        let edge = redaction_rect(RectLike::new(0.5, 0.5, 70.0, 50.0), 60, 40).unwrap();
        assert_eq!(edge, RectLike::new(0.0, 0.0, 60.0, 40.0));
        assert!(redaction_rect(RectLike::new(80.0, 80.0, 5.0, 5.0), 60, 40).is_none());
    }

    #[test]
    fn solid_redaction_paints_every_pixel_opaque_black() {
        let base = image::RgbaImage::from_pixel(60, 40, image::Rgba([255, 255, 255, 255]));
        let rect = redaction_rect(RectLike::new(10.3, 8.6, 20.2, 9.7), 60, 40).unwrap();
        let out = crate::render::render(&base, &[redaction_annotation(rect, RedactStyle::Solid)]);
        for y in 7..20 {
            for x in 8..32 {
                assert_eq!(out.get_pixel(x, y).0, [0, 0, 0, 255], "({x},{y})");
            }
        }
        assert_eq!(out.get_pixel(7, 7).0, [255, 255, 255, 255]);
    }

    #[test]
    fn styles_map_to_existing_annotation_kinds() {
        let rect = RectLike::new(0.0, 0.0, 40.0, 20.0);
        assert!(matches!(
            redaction_annotation(rect, RedactStyle::Solid),
            Annotation::Rectangle { style, color, .. } if style.fill == FillMode::Filled && color == Rgba::OPAQUE_BLACK
        ));
        assert_eq!(
            redaction_annotation(rect, RedactStyle::Pixelate),
            Annotation::Pixelate {
                rect,
                block_size: 10.0
            }
        );
        assert_eq!(
            redaction_annotation(rect, RedactStyle::Blur),
            Annotation::Blur { rect, radius: 12.0 }
        );
        assert_eq!(RedactStyle::default(), RedactStyle::Solid);
        assert_eq!(RedactStyle::Solid.to_string(), "Solid block");
    }

    fn outline() -> Annotation {
        Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 100.0, 100.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: ShapeStyle::default(),
        }
    }

    #[test]
    fn coverage_of_existing_redactions() {
        let rect = RectLike::new(10.0, 10.0, 20.0, 10.0);
        let block = redaction_annotation(RectLike::new(5.0, 5.0, 40.0, 40.0), RedactStyle::Solid);
        assert!(is_covered(rect, std::slice::from_ref(&block)));
        assert!(!is_covered(RectLike::new(40.0, 40.0, 20.0, 20.0), &[block]));
        assert!(!is_covered(rect, &[outline()]));
        assert!(!is_covered(rect, &[Annotation::Crop { rect }]));
    }

    #[test]
    fn panel_text_matches_output_only_for_text_neutral_annotations() {
        use crate::annotation::TextStyle;
        use crate::geom::PointLike;
        let rect = RectLike::new(10.0, 10.0, 20.0, 10.0);
        let highlight = Annotation::Highlighter {
            points: vec![PointLike::new(0.0, 0.0), PointLike::new(9.0, 0.0)],
            color: Rgba::new(1.0, 1.0, 0.0, 0.45),
            line_width: 12.0,
        };
        assert!(panel_text_matches_output(&[]));
        assert!(panel_text_matches_output(&[outline(), highlight]));
        let typed = Annotation::Text {
            content: "typed".into(),
            origin: PointLike::new(1.0, 1.0),
            color: Rgba::OPAQUE_BLACK,
            font_family: "system-ui".into(),
            size: 16.0,
            style: TextStyle::default(),
        };
        let scribble = Annotation::Pen {
            points: vec![PointLike::new(0.0, 0.0), PointLike::new(9.0, 9.0)],
            color: Rgba::OPAQUE_BLACK,
            line_width: 3.0,
        };
        for a in [
            redaction_annotation(rect, RedactStyle::Solid),
            typed,
            scribble,
            Annotation::Blur { rect, radius: 4.0 },
            Annotation::Crop { rect },
        ] {
            assert!(!panel_text_matches_output(&[outline(), a.clone()]), "{a:?}");
        }
    }

    #[test]
    fn locate_covers_each_located_match_with_a_whole_pixel_rect() {
        let lines = vec![RecognizedLine {
            text: "+1 (415) 555-0142".into(),
            x: 0.1,
            y: 0.5,
            w: 0.6,
            h: 0.1,
            words: Vec::new(),
        }];
        let doc = TextDocument::from_recognized(&lines, 100, 100);
        let matches = scan(&doc);
        let located = locate(&doc, &matches, 100, 100);
        assert!(located.unlocatable.is_empty());
        assert_eq!(located.rects.len(), 1);
        let (m, r) = &located.rects[0];
        assert_eq!(m, &matches[0]);
        let span = doc.lines[0].span_rect(m.span.clone()).unwrap();
        assert!(r.contains(span), "{r:?} vs {span:?}");
        for v in [r.x, r.y, r.width, r.height] {
            assert_eq!(v, v.round());
        }
    }

    #[test]
    fn locate_reports_matches_it_cannot_place_instead_of_dropping_them() {
        // No positions (non-Vision engine): every match is unlocatable.
        let doc = TextDocument::from_plain("mail a@example.com\ncall (415) 555-0142");
        let matches = scan(&doc);
        assert_eq!(matches.len(), 2);
        let located = locate(&doc, &matches, 100, 100);
        assert!(located.rects.is_empty());
        assert_eq!(located.unlocatable, matches);

        // A stale match whose line or span no longer exists.
        let lines = vec![RecognizedLine {
            text: "a@example.com".into(),
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 0.1,
            words: Vec::new(),
        }];
        let doc = TextDocument::from_recognized(&lines, 100, 100);
        let stale = [
            RedactMatch {
                line: 7,
                span: 0..3,
                kind: RedactKind::Email,
            },
            RedactMatch {
                line: 0,
                span: 40..50,
                kind: RedactKind::Email,
            },
        ];
        let located = locate(&doc, &stale, 100, 100);
        assert!(located.rects.is_empty());
        assert_eq!(located.unlocatable, stale);
    }

    // --- Review fix round 1 -------------------------------------------

    fn phones_in(text: &str) -> Vec<String> {
        found(text)
            .into_iter()
            .filter(|(_, k)| *k == RedactKind::Phone)
            .map(|(s, _)| s)
            .collect()
    }

    #[test]
    fn phones_after_rejected_prefixes_are_still_found() {
        for (line, want) in [
            ("2026-10-16 415-555-0100", vec!["415-555-0100"]),
            (
                // "0100 415-555" is a plausible trunk-0 number overlapping
                // both, so the three merge (over-cover, never under-cover).
                "415-555-0100 415-555-0101",
                vec!["415-555-0100 415-555-0101"],
            ),
            ("12:30 415 555 0100", vec!["415 555 0100"]),
            ("1. 415-555-0100", vec!["415-555-0100"]),
            ("1) 415-555-0100", vec!["415-555-0100"]),
            ("2 415-555-0100", vec!["415-555-0100"]),
            ("Qty 3 (415) 555-0100", vec!["(415) 555-0100"]),
            ("Apt 4 - 415-555-0100", vec!["415-555-0100"]),
            ("Ext 12 +44 20 7946 0958", vec!["+44 20 7946 0958"]),
        ] {
            assert_eq!(phones_in(line), want, "{line}");
        }
        assert_eq!(
            found("4242 4242 4242 4242 415 555 0100"),
            [
                ("4242 4242 4242 4242".to_string(), RedactKind::Card),
                ("415 555 0100".to_string(), RedactKind::Phone),
            ]
        );
        assert!(found("4242 4242 4242 4241").is_empty());
        assert_eq!(
            found("4242 4242 4242 4241 a@example.com"),
            [("a@example.com".to_string(), RedactKind::Email)]
        );
    }

    #[test]
    fn international_and_national_phones() {
        for s in [
            "+14155550100",
            "+33 1 23 45 67 89",
            "+81 3-1234-5678",
            "+61 2 9374 4000",
            "+353 1 234 5678",
            "+91 98765 43210",
            "+49 30 12345678",
            "+44 (0)20 7946 0958",
            "07700 900123",
        ] {
            assert_eq!(found(s), [(s.to_string(), RedactKind::Phone)], "{s}");
        }
        for s in [
            "12345 67890",
            "1 299 000 000",
            "on 2026-10-16 12:30",
            "at 12:30:45 on 16.10.2026",
            "Total $1,299.00",
            "Price 1 299,00",
            "+12.50",
        ] {
            assert!(found(s).is_empty(), "{s}");
        }
    }

    #[test]
    fn non_ascii_emails_span_the_whole_address() {
        assert_eq!(
            found("mail jöhn@example.com now"),
            [("jöhn@example.com".to_string(), RedactKind::Email)]
        );
        assert_eq!(
            found("José@exämple.com"),
            [("José@exämple.com".to_string(), RedactKind::Email)]
        );
        assert!(found("x@y.c0m").is_empty());
    }

    #[test]
    fn cards_with_dot_and_double_space_separators_and_more_networks() {
        for s in [
            "4242  4242  4242  4242",
            "4242.4242.4242.4242",
            "6200 0000 0000 0005", // UnionPay
            "6759 6498 2643 8453", // Maestro
        ] {
            assert_eq!(found(s), [(s.to_string(), RedactKind::Card)], "{s}");
        }
        // Group shapes no card uses: a phone, not a card.
        assert_eq!(found("+61 2 9374 4000")[0].1, RedactKind::Phone);
        assert!(found("9000 0000 0000 0001").is_empty());
    }

    #[test]
    fn long_digit_runs_scan_in_linear_time() {
        // Quadratic scanning of these takes minutes; linear takes ms.
        let start = std::time::Instant::now();
        for unit in ["12-", "4242 ", "1 ", "1.2-3 "] {
            let line = unit.repeat(50_000);
            let _ = detect_line(&line);
        }
        assert!(start.elapsed() < std::time::Duration::from_secs(10));
    }

    #[test]
    fn more_key_prefixes_and_base64_tokens() {
        for key in [
            ["np", "m_", "EXAMPLEexample0123456789abcdefABCDEF"].concat(),
            ["h", "f_", "EXAMPLEexample0123456789abcdef"].concat(),
            ["shp", "at_", "0123456789abcdef0123456789abcdef"].concat(),
            ["shp", "ss_", "0123456789abcdef0123456789abcdef"].concat(),
            ["do", "p_v1_", &"0123456789abcdef".repeat(4)].concat(),
            ["xa", "pp-", "1-A0EXAMPLE-0123456789"].concat(),
        ] {
            assert_eq!(found(&key), [(key.clone(), RedactKind::ApiKey)], "{key}");
        }
        let b64 = ["Qm9i/x7Zp+K3", "wL5tR8vN2yH4", "cD6jF1=="].concat();
        assert_eq!(
            found(&format!("secret: {b64}")),
            [(b64.clone(), RedactKind::ApiKey)]
        );
        // Prefix body length counts chars, not bytes.
        assert!(!looks_like_secret(&["gh", "p_", &"é".repeat(12)].concat()));
    }

    #[test]
    fn more_non_secrets_stay_unflagged() {
        for s in [
            "SomeJavaClassNameFactoryBean2",
            "AbstractSingletonProxyFactoryBean2",
            "HttpServerRequestHandlerV2Impl",
            "UserProfileSettingsViewController3",
            "01ARZ3NDEKTSV4RRFFQ8K5DSMV",
            "3f78685",
            "3F786850E387550FDAB836ED7E6DC881DE23001B",
            "v2.3.1-rc.4+build.20261016",
            "/Users/me/Projects2026/readshot/crates/Cargo2.toml",
            "src/components/UserProfile/ProfileCard2.tsx",
            "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==",
        ] {
            assert!(found(s).is_empty(), "{s}: {:?}", found(s));
        }
    }

    /// Deterministic xorshift64 so the rate check needs no rand crate.
    fn xorshift(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    fn flagged_share(alphabet: &str, len: usize, trials: usize, seed: u64) -> f64 {
        let set: Vec<char> = alphabet.chars().collect();
        let mut x = seed;
        let hits = (0..trials)
            .filter(|_| {
                let t: String = (0..len)
                    .map(|_| set[(xorshift(&mut x) % set.len() as u64) as usize])
                    .collect();
                !detect_line(&format!("key {t}")).is_empty()
            })
            .count();
        hits as f64 / trials as f64
    }

    #[test]
    fn random_tokens_are_flagged_at_a_high_rate() {
        const B62: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
        let b64 = format!("{B62}+/");
        let base62 = flagged_share(B62, 24, 2_000, 0x9E37_79B9_7F4A_7C15);
        let base64 = flagged_share(&b64, 32, 2_000, 0xD1B5_4A32_D192_ED03);
        assert!(base62 >= 0.95, "base62/24 flagged {base62}");
        assert!(base64 >= 0.90, "base64/32 flagged {base64}");
    }

    // --- Review fix round 2 -------------------------------------------

    /// Whether some detection of `kind` covers every char of `needle`
    /// (its last occurrence in `line`).
    fn covered(line: &str, needle: &str, kind: RedactKind) -> bool {
        let b = line.rfind(needle).expect("needle in line");
        let start = line[..b].chars().count();
        let end = start + needle.chars().count();
        detect_line(line)
            .iter()
            .any(|(r, k)| *k == kind && r.start <= start && r.end >= end)
    }

    #[test]
    fn junk_phone_prefix_never_leaves_part_of_a_number_visible() {
        for (line, number) in [
            ("Ticket 0042 415-555-0100", "415-555-0100"),
            ("0800 415 555 0100", "415 555 0100"),
            ("Ref 0100 415 555 0100", "415 555 0100"),
            ("0042 (415) 555-0100", "(415) 555-0100"),
            ("ID 012 3456 415 555 0100", "415 555 0100"),
            ("PIN 0000 020 7946 0958", "020 7946 0958"),
        ] {
            assert!(
                covered(line, number, RedactKind::Phone),
                "{line}: {:?}",
                found(line)
            );
        }
        // Merging never swallows a card next to a phone.
        assert_eq!(
            found("4242 4242 4242 4242 415 555 0100"),
            [
                ("4242 4242 4242 4242".to_string(), RedactKind::Card),
                ("415 555 0100".to_string(), RedactKind::Phone),
            ]
        );
    }

    #[test]
    fn base64_marker_without_a_data_uri_does_not_hide_secrets() {
        let tok = fake_random_token();
        let ghp = ["gh", "p_", "EXAMPLEexample0123456789abcdefAB"].concat();
        for (line, secret) in [
            (format!("Authorization: Basic base64,{tok}"), tok.clone()),
            (format!("key=base64,{tok}"), tok.clone()),
            (format!("data:;base64,{tok}"), tok.clone()),
            (format!("base64,{}", fake_stripe_key()), fake_stripe_key()),
            (
                format!("data:image/png;base64,{}", fake_stripe_key()),
                fake_stripe_key(),
            ),
            (format!("data:text/plain;base64,{ghp}"), ghp.clone()),
            (
                format!("data:image/png;base64,iVBORw0KGgo= key={tok}"),
                tok.clone(),
            ),
        ] {
            assert!(
                covered(&line, &secret, RedactKind::ApiKey),
                "{line}: {:?}",
                found(&line)
            );
        }
        // A real data URI payload (with a parameter) is still content.
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
        for line in [
            format!("data:image/png;base64,{png}"),
            format!("data:image/svg+xml;charset=utf-8;base64,{png}"),
        ] {
            assert!(found(&line).is_empty(), "{line}");
        }
    }

    #[test]
    fn data_marker_inside_a_word_or_value_does_not_hide_secrets() {
        // 40 chars of synthetic random-looking material.
        let tok = [fake_random_token().as_str(), "Pq2Ws6Ex", "Jy7UGn3K"].concat();
        assert_eq!(tok.chars().count(), 40);
        for line in [
            format!("metadata:text/plain;base64,{tok}"),
            format!("secret=data:a/b;base64,{tok}"),
            format!("my_data:a/b;base64,{tok}"),
            format!("x-data:a/b;base64,{tok}"),
            format!("x.data:a/b;base64,{tok}"),
            format!("x:data:a/b;base64,{tok}"),
            format!("x/data:a/b;base64,{tok}"),
        ] {
            assert!(
                covered(&line, &tok, RedactKind::ApiKey),
                "{line}: {:?}",
                found(&line)
            );
        }
        // A data URI that starts the line or follows punctuation is
        // still content.
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
        for line in [
            format!("data:image/png;base64,{png}"),
            format!("src=\"data:image/png;base64,{png}\""),
            format!("url(data:image/png;base64,{png})"),
            format!("img data:image/png;base64,{png}"),
            format!("<data:image/png;base64,{png}>"),
            format!("'data:image/png;base64,{png}'"),
            format!("[data:image/png;base64,{png}]"),
            format!("a,data:image/png;base64,{png}"),
        ] {
            assert!(found(&line).is_empty(), "{line}: {:?}", found(&line));
        }
    }

    #[test]
    fn plus_prefixed_and_ocr_split_cards_are_still_cards() {
        assert!(covered(
            "+4111 1111 1111 1111",
            "4111 1111 1111 1111",
            RedactKind::Card
        ));
        assert!(covered(
            "+4242424242424242",
            "4242424242424242",
            RedactKind::Card
        ));
        for s in [
            "4242 4242 4242 42 42",
            "4242 424 2 4242 4242",
            "42 42 4242 4242 4242",
        ] {
            assert_eq!(found(s), [(s.to_string(), RedactKind::Card)], "{s}");
        }
        assert_eq!(
            found("+61 2 9374 4000"),
            [("+61 2 9374 4000".to_string(), RedactKind::Phone)]
        );
    }

    #[test]
    fn lowercase_heavy_tokens_with_a_few_capitals_are_flagged() {
        let lower_digits: Vec<char> = "abcdefghijklmnopqrstuvwxyz0123456789".chars().collect();
        let upper: Vec<char> = ('A'..='Z').collect();
        let mut x = 0x2545_F491_4F6C_DD1D_u64;
        let trials = 2_000;
        let hits = (0..trials)
            .filter(|_| {
                let mut t: Vec<char> = (0..24)
                    .map(|_| lower_digits[(xorshift(&mut x) % 36) as usize])
                    .collect();
                for _ in 0..3 {
                    let at = (xorshift(&mut x) % 24) as usize;
                    t[at] = upper[(xorshift(&mut x) % 26) as usize];
                }
                let t: String = t.into_iter().collect();
                !detect_line(&format!("key {t}")).is_empty()
            })
            .count();
        let share = hits as f64 / trials as f64;
        assert!(share >= 0.95, "lowercase+digits+3 caps flagged {share}");
    }

    /// TX-23: every documented key prefix, each at exactly its minimum
    /// body length, is an API key; one char shorter it is not. The
    /// bodies are a single repeated letter, so only the prefix rule can
    /// flag them (the high-entropy rule never would).
    #[test]
    fn each_key_prefix_flags_from_its_minimum_body_length() {
        // (prefix, minimum body), split so no literal reads as a key.
        let table: [(&[&str], usize); 29] = [
            (&["sk_", "live_"], 10),
            (&["sk_", "test_"], 10),
            (&["pk_", "live_"], 10),
            (&["pk_", "test_"], 10),
            (&["rk_", "live_"], 10),
            (&["rk_", "test_"], 10),
            (&["github", "_pat_"], 20),
            (&["gh", "p_"], 20),
            (&["gh", "o_"], 20),
            (&["gh", "u_"], 20),
            (&["gh", "s_"], 20),
            (&["gh", "r_"], 20),
            (&["gl", "pat-"], 16),
            (&["xo", "xb-"], 10),
            (&["xo", "xp-"], 10),
            (&["xo", "xa-"], 10),
            (&["xo", "xs-"], 10),
            (&["AK", "IA"], 16),
            (&["AS", "IA"], 16),
            (&["AI", "za"], 30),
            (&["sk", "-"], 20),
            (&["S", "G."], 20),
            (&["ey", "J"], 20),
            (&["np", "m_"], 36),
            (&["h", "f_"], 30),
            (&["shp", "at_"], 32),
            (&["shp", "ss_"], 32),
            (&["do", "p_v1_"], 64),
            (&["xa", "pp-"], 10),
        ];
        for (parts, body) in table {
            let prefix = parts.concat();
            let key = format!("{prefix}{}", "x".repeat(body));
            assert_eq!(
                found(&format!("token {key} here")),
                [(key.clone(), RedactKind::ApiKey)],
                "{prefix} with {body}"
            );
            let short = format!("{prefix}{}", "x".repeat(body - 1));
            assert!(
                found(&format!("token {short} here")).is_empty(),
                "{prefix} with {} is too short",
                body - 1
            );
        }
    }

    /// TX-23: a phone spans at most 15 digits and 32 chars.
    #[test]
    fn phones_stop_at_fifteen_digits_and_thirty_two_chars() {
        // One international block: 15 digits is a phone, 16 is not.
        let fifteen = "+123456789012345";
        assert_eq!(found(fifteen), [(fifteen.to_string(), RedactKind::Phone)]);
        assert!(!found("+1234567890123457")
            .iter()
            .any(|(_, k)| *k == RedactKind::Phone));
        // Padded with separators: a 32-char number is found whole; at 33
        // chars the candidate stops at the cap, before the last group.
        let pad = |n: usize| format!("+44{}20 7946 0958", " ".repeat(n));
        assert_eq!(pad(17).chars().count(), 32);
        assert_eq!(found(&pad(17)), [(pad(17), RedactKind::Phone)]);
        let capped = format!("+44{}20 7946", " ".repeat(18));
        assert_eq!(found(&pad(18)), [(capped, RedactKind::Phone)]);
    }
}
