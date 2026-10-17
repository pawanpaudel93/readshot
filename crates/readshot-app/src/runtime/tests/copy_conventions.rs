//! Copy conventions checked over the view sources: tooltips name their
//! control in title case (as buttons and menu items do), toggles and row
//! labels use sentence case.

/// Sources whose tooltips and toggles are checked.
const SOURCES: [(&str, &str); 7] = [
    ("view.rs", include_str!("../view.rs")),
    ("editor_options.rs", include_str!("../editor_options.rs")),
    ("beautify_panel.rs", include_str!("../beautify_panel.rs")),
    ("side_panel.rs", include_str!("../side_panel.rs")),
    ("text_panel.rs", include_str!("../text_panel.rs")),
    ("image_menu.rs", include_str!("../image_menu.rs")),
    ("ui_kit.rs", include_str!("../ui_kit.rs")),
];

/// The arguments of the call whose `(` is at `open`, split at top-level
/// commas (string contents are skipped).
fn call_args(src: &str, open: usize) -> Vec<&str> {
    let bytes = src.as_bytes();
    let (mut depth, mut start, mut in_str, mut i) = (0i32, open + 1, false, open);
    let mut out = Vec::new();
    while i < bytes.len() {
        let c = bytes[i];
        if in_str {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
        } else {
            match c {
                b'"' => in_str = true,
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        out.push(src[start..i].trim());
                        return out;
                    }
                }
                b',' if depth == 1 => {
                    out.push(src[start..i].trim());
                    start = i + 1;
                }
                _ => {}
            }
        }
        i += 1;
    }
    out
}

/// Every plain string literal passed as argument `index` of `callee`.
fn literal_args(callee: &str, index: usize) -> Vec<(String, &'static str)> {
    let mut found = Vec::new();
    for (file, src) in SOURCES {
        let pattern = format!("{callee}(");
        for (at, _) in src.match_indices(&pattern) {
            // Skip the definition and longer names ending in `callee`.
            let before = &src[..at];
            if before.ends_with("fn ") || before.ends_with('_') {
                continue;
            }
            let prev = before.chars().last().unwrap_or(' ');
            if prev.is_alphanumeric() {
                continue;
            }
            let args = call_args(src, at + pattern.len() - 1);
            if let Some(arg) = args.get(index) {
                if arg.len() >= 2 && arg.starts_with('"') && arg.ends_with('"') {
                    found.push((arg[1..arg.len() - 1].to_string(), file));
                }
            }
        }
    }
    found
}

const SMALL_WORDS: [&str; 14] = [
    "a", "an", "and", "as", "at", "by", "for", "from", "in", "of", "on", "or", "the", "to",
];

fn is_title_case(s: &str) -> bool {
    s.split_whitespace().enumerate().all(|(i, w)| {
        let first = w.chars().next().unwrap();
        !first.is_lowercase() || (i > 0 && SMALL_WORDS.contains(&w))
    })
}

fn is_sentence_case(s: &str) -> bool {
    let mut words = s.split_whitespace();
    let Some(first) = words.next() else {
        return true;
    };
    first.chars().next().is_some_and(|c| !c.is_lowercase())
        && words.all(|w| {
            // Proper names keep their capital.
            w.chars().next().is_some_and(|c| c.is_lowercase())
                || ["Readshot", "OCR", "Tab"].contains(&w)
                || w.starts_with('(')
        })
}

#[test]
fn tooltips_name_their_control_in_title_case() {
    let mut names = literal_args("icon_button", 2);
    names.extend(literal_args("icon_button_hinted", 2));
    names.extend(literal_args("tooltip_with_key", 1));
    assert!(names.len() > 40, "found only {}", names.len());
    for (name, file) in names {
        assert!(is_title_case(&name), "{file}: tooltip {name:?}");
    }
}

#[test]
fn toggles_use_sentence_case() {
    let labels: Vec<_> = SOURCES
        .iter()
        .flat_map(|(file, src)| {
            src.match_indices(".label(\"").map(move |(at, m)| {
                let rest = &src[at + m.len()..];
                (rest[..rest.find('"').unwrap()].to_string(), *file)
            })
        })
        .collect();
    assert!(!labels.is_empty());
    for (label, file) in labels {
        assert!(is_sentence_case(&label), "{file}: toggle {label:?}");
    }
}
