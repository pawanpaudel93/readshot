//! Smart redact (spec §3.8): find emails, phone numbers, API keys /
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
/// can sometimes be partly reversed (spec §3.8).
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

/// Sensitive spans in one line: char ranges, sorted, non-overlapping.
/// Overlaps keep the higher priority: Card > Email > API key > Phone.
pub fn detect_line(text: &str) -> Vec<(Range<usize>, RedactKind)> {
    let cs: Vec<char> = text.chars().collect();
    let candidates = find_cards(&cs)
        .into_iter()
        .map(|r| (r, RedactKind::Card))
        .chain(find_emails(&cs).into_iter().map(|r| (r, RedactKind::Email)))
        .chain(
            find_api_keys(&cs)
                .into_iter()
                .map(|r| (r, RedactKind::ApiKey)),
        )
        .chain(find_phones(&cs).into_iter().map(|r| (r, RedactKind::Phone)));
    let mut accepted: Vec<(Range<usize>, RedactKind)> = Vec::new();
    for (r, k) in candidates {
        if accepted
            .iter()
            .all(|(a, _)| r.end <= a.start || r.start >= a.end)
        {
            accepted.push((r, k));
        }
    }
    accepted.sort_by_key(|(r, _)| r.start);
    accepted
}

// --- Emails ----------------------------------------------------------

fn is_local(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '%' | '+' | '-')
}

fn is_domain(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '-')
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
        if dot == 0 || tld.len() < 2 || !tld.chars().all(|c| c.is_ascii_alphabetic()) {
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

/// Issuer prefix and length of a card number (`digits.len()` ≥ 13):
/// Visa, Mastercard, Amex, Discover, JCB, Diners.
fn card_network_ok(d: &[u8]) -> bool {
    let n = d.len();
    let prefix = |k: usize| d[..k].iter().fold(0u32, |a, &x| a * 10 + u32::from(x));
    match d[0] {
        4 => matches!(n, 13 | 16 | 19),
        5 => (51..=55).contains(&prefix(2)) && n == 16,
        2 => (2221..=2720).contains(&prefix(4)) && n == 16,
        3 => match prefix(2) {
            34 | 37 => n == 15,
            35 => (16..=19).contains(&n),
            30 | 36 | 38 => (14..=19).contains(&n),
            _ => false,
        },
        6 => {
            (prefix(4) == 6011 || prefix(2) == 65 || (644..=649).contains(&prefix(3)))
                && (16..=19).contains(&n)
        }
        _ => false,
    }
}

fn find_cards(cs: &[char]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        if !cs[i].is_ascii_digit() || (i > 0 && cs[i - 1].is_alphanumeric()) {
            i += 1;
            continue;
        }
        let mut digits: Vec<u8> = Vec::new();
        // (char index just past a digit group, digits so far)
        let mut ends: Vec<(usize, usize)> = Vec::new();
        let mut j = i;
        loop {
            while j < cs.len() && cs[j].is_ascii_digit() {
                digits.push(cs[j] as u8 - b'0');
                j += 1;
            }
            ends.push((j, digits.len()));
            let sep = j < cs.len() && matches!(cs[j], ' ' | '-');
            if sep && j + 1 < cs.len() && cs[j + 1].is_ascii_digit() {
                j += 1;
            } else {
                break;
            }
        }
        let run_end = j;
        let run_bounded = run_end >= cs.len() || !cs[run_end].is_alphanumeric();
        let hit = ends
            .iter()
            .rev()
            .filter(|&&(end, _)| end < run_end || run_bounded)
            .find(|&&(_, n)| {
                (13..=19).contains(&n) && card_network_ok(&digits[..n]) && luhn_valid(&digits[..n])
            })
            .map(|&(end, _)| i..end);
        match hit {
            Some(r) => {
                i = r.end;
                out.push(r);
            }
            None => i += 1,
        }
    }
    out
}

// --- Phones ----------------------------------------------------------

fn is_phone_char(c: char) -> bool {
    c.is_ascii_digit() || matches!(c, ' ' | '-' | '.' | '(' | ')')
}

/// Plausible phone groupings only (see the plan's decision list):
/// `+CC` international (8–15 digits, country code 1–3, groups 2–4),
/// NANP 3-3-4 or 1-3-3-4, or a national number with a leading trunk `0`
/// (10–11 digits, groups 2–4). Dots only in 3-3-4.
fn phone_like(span: &[char]) -> bool {
    let groups: Vec<&[char]> = span
        .split(|c| !c.is_ascii_digit())
        .filter(|g| !g.is_empty())
        .collect();
    if groups.is_empty() {
        return false;
    }
    let lens: Vec<usize> = groups.iter().map(|g| g.len()).collect();
    let digits: usize = lens.iter().sum();
    if span.first() == Some(&'+') {
        return (8..=15).contains(&digits)
            && lens.len() >= 2
            && lens[0] <= 3
            && lens[1..].iter().all(|n| (2..=4).contains(n));
    }
    if span.contains(&'.') {
        return lens == [3, 3, 4];
    }
    lens == [3, 3, 4]
        || (lens == [1, 3, 3, 4] && groups[0] == ['1'])
        || (groups[0][0] == '0'
            && (10..=11).contains(&digits)
            && lens.len() >= 2
            && lens.iter().all(|n| (2..=4).contains(n)))
}

fn find_phones(cs: &[char]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        let starts = cs[i] == '+' || cs[i] == '(' || cs[i].is_ascii_digit();
        if !starts || (i > 0 && cs[i - 1].is_alphanumeric()) {
            i += 1;
            continue;
        }
        let mut e = i + 1;
        while e < cs.len() && is_phone_char(cs[e]) {
            e += 1;
        }
        let next = e;
        while e > i && !(cs[e - 1].is_ascii_digit() || cs[e - 1] == ')') {
            e -= 1;
        }
        let opens = cs[i..e].iter().filter(|&&c| c == '(').count();
        let closes = cs[i..e].iter().filter(|&&c| c == ')').count();
        if e > i && cs[e - 1] == ')' && closes > opens {
            e -= 1;
        }
        let bounded = e >= cs.len() || !cs[e].is_alphanumeric();
        if e > i && bounded && phone_like(&cs[i..e]) {
            out.push(i..e);
        }
        // Never restart inside a rejected run: "4242 4242 4242 4241"
        // must not yield its last 12 digits as a phone.
        i = next.max(i + 1);
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
];

/// Minimum length of an unprefixed high-entropy token (spec §3.8).
pub const MIN_SECRET_LEN: usize = 20;

fn is_key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '=' | '.')
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
    if KEY_PREFIXES
        .iter()
        .any(|(p, body)| token.starts_with(p) && token.len() >= p.len() + body)
    {
        return true;
    }
    if !token.chars().all(is_key_char) {
        return false;
    }
    token.split(['.', '-']).any(random_segment)
}

/// ≥ 20 chars with upper, lower and digits, ≥ 12 distinct, and whose
/// alphanumerics change class (upper / lower / digit) at least every
/// other character. CamelCase changes about twice per word, so
/// `SomeJavaClassNameFactoryBean2` fails; random base62 changes ~62 % of
/// the time.
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
    distinct >= 12 && changes * 2 >= alnum.len()
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
        if looks_like_secret(&token) {
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

fn contains(outer: RectLike, inner: RectLike) -> bool {
    outer.x <= inner.x
        && outer.y <= inner.y
        && outer.x + outer.width >= inner.x + inner.width
        && outer.y + outer.height >= inner.y + inner.height
}

/// Whether an existing redaction-like annotation already covers `rect`
/// (an opaque square-cornered filled rectangle, a blur or a pixelate).
pub fn is_covered(rect: RectLike, annotations: &[Annotation]) -> bool {
    annotations.iter().any(|a| match a {
        Annotation::Rectangle {
            rect: r,
            color,
            style,
            ..
        } => {
            style.fill == FillMode::Filled
                && style.corner_radius == 0.0
                && color.a >= 1.0
                && contains(*r, rect)
        }
        Annotation::Blur { rect: r, .. } | Annotation::Pixelate { rect: r, .. } => {
            contains(*r, rect)
        }
        _ => false,
    })
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

    fn contains_rect(outer: RectLike, inner: RectLike) -> bool {
        outer.x <= inner.x
            && outer.y <= inner.y
            && outer.x + outer.width >= inner.x + inner.width
            && outer.y + outer.height >= inner.y + inner.height
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
        assert!(contains_rect(*r, span), "{r:?} vs {span:?}");
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
}
