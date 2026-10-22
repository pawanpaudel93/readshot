//! Static checks of the website (`site/`) and the CLI documentation.
//!
//! No browser and no network: the pages are read as text. This covers
//! what can go stale without anyone noticing — internal links and
//! anchors, image alt text, page titles, the install command and its
//! redirect, the sitemap — and checks every `readshot …` command the
//! site and docs/CLI.md show against the real argument parser, and the
//! exit-code table against the code. Copy-button behaviour, layout and
//! the live host need a browser (see the W5c report).

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use clap::Parser;
use readshot_app::cli::{exit_code, Cli, CliError};
use readshot_core::error::{CaptureError, OCRError};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

const PAGES: &[&str] = &["index.html", "cli.html", "mcp.html"];

fn pages() -> HashMap<&'static str, String> {
    PAGES
        .iter()
        .map(|page| (*page, read(&format!("site/{page}"))))
        .collect()
}

/// Values of every `name="…"` attribute in `html`.
fn attr_values<'a>(html: &'a str, name: &str) -> Vec<&'a str> {
    let needle = format!(" {name}=\"");
    let mut values = Vec::new();
    let mut rest = html;
    while let Some(start) = rest.find(&needle) {
        let after = &rest[start + needle.len()..];
        let end = after.find('"').unwrap();
        values.push(&after[..end]);
        rest = &after[end..];
    }
    values
}

/// Every start tag `<tag …>` in `html`, attributes included.
fn tags<'a>(html: &'a str, tag: &str) -> Vec<&'a str> {
    let needle = format!("<{tag}");
    let mut found = Vec::new();
    let mut rest = html;
    while let Some(start) = rest.find(&needle) {
        let after = &rest[start..];
        let next = after.as_bytes().get(needle.len()).copied();
        let end = after.find('>').unwrap() + 1;
        if matches!(next, Some(b' ' | b'\n' | b'>' | b'/')) {
            found.push(&after[..end]);
        }
        rest = &after[end..];
    }
    found
}

fn unescape_html(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

/// The text inside every `<pre><code …>…</code></pre>`.
fn code_blocks(html: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut rest = html;
    while let Some(start) = rest.find("<pre") {
        let after = &rest[start..];
        let code = after.find("<code").unwrap();
        let open_end = code + after[code..].find('>').unwrap() + 1;
        let close = after.find("</code>").unwrap();
        blocks.push(unescape_html(&after[open_end..close]));
        rest = &after[close..];
    }
    blocks
}

/// The text of the first element with `class="…{class}…"`, tags removed.
fn element_text(html: &str, class: &str) -> String {
    let start = html
        .find(&format!("class=\"{class}\""))
        .unwrap_or_else(|| panic!("no .{class}"));
    let after = &html[start..];
    let open_end = after.find('>').unwrap() + 1;
    let close = after.find("</").unwrap();
    let mut text = String::new();
    let mut in_tag = false;
    for ch in after[open_end..close].chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => text.push(c),
            _ => {}
        }
    }
    text.trim().to_string()
}

/// WEB-14: every page has a title, a description, a language, a skip
/// link to `#main` as its first link, one `<main id="main">`, and
/// unique ids.
#[test]
fn every_page_has_its_chrome() {
    for (page, html) in pages() {
        let title = html
            .split("<title>")
            .nth(1)
            .and_then(|rest| rest.split("</title>").next())
            .unwrap_or_default();
        assert!(title.starts_with("Readshot"), "{page}: title `{title}`");
        assert!(html.contains("<html lang=\"en\">"), "{page}");
        for meta in [
            "name=\"description\"",
            "name=\"viewport\"",
            "property=\"og:title\"",
            "property=\"og:description\"",
            "property=\"og:image\"",
        ] {
            assert!(html.contains(meta), "{page}: no {meta}");
        }
        let first_link = tags(&html, "a")[0];
        assert!(
            first_link.contains("class=\"skip-link\"") && first_link.contains("href=\"#main\""),
            "{page}: first link is {first_link}"
        );
        assert_eq!(html.matches("<main id=\"main\">").count(), 1, "{page}");
        let nav: &[&str] = if page == "index.html" {
            &["#install", "#features", "cli.html", "mcp.html"]
        } else {
            &["index.html", "index.html#features", "cli.html", "mcp.html"]
        };
        for target in nav {
            assert!(
                html.contains(&format!("href=\"{target}\"")),
                "{page}: no nav link to {target}"
            );
        }

        let ids = attr_values(&html, "id");
        let unique: BTreeSet<_> = ids.iter().collect();
        assert_eq!(ids.len(), unique.len(), "{page}: duplicate ids");
    }
}

/// WEB-14: every image has alt text (empty for decoration), and every
/// page links its stylesheet, icons and manifest.
#[test]
fn images_have_alt_text_and_assets_exist() {
    for (page, html) in pages() {
        for img in tags(&html, "img") {
            assert!(img.contains(" alt=\""), "{page}: {img} has no alt");
        }
        for asset in [
            "styles.css",
            "favicon.svg",
            "favicon.ico",
            "apple-touch-icon.png",
            "site.webmanifest",
            "script.js",
        ] {
            assert!(html.contains(&format!("\"{asset}\"")), "{page}: {asset}");
        }
    }
}

/// WEB-01, WEB-08, WEB-09, WEB-14: every internal link, image and
/// script points at a file in site/, and every `#anchor` (on this page
/// or another) is an id on that page.
#[test]
fn internal_links_and_anchors_resolve() {
    let pages = pages();
    let ids: HashMap<&str, BTreeSet<&str>> = pages
        .iter()
        .map(|(page, html)| (*page, attr_values(html, "id").into_iter().collect()))
        .collect();
    let mut checked = 0;
    for (page, html) in &pages {
        let mut targets = attr_values(html, "href");
        targets.extend(attr_values(html, "src"));
        for target in targets {
            if target.starts_with("https://") {
                assert!(
                    target.starts_with("https://github.com/pawanpaudel93")
                        || target.starts_with("https://readshot.pawanpaudel.com.np"),
                    "{page}: unexpected external link {target}"
                );
                continue;
            }
            assert!(!target.starts_with("http:"), "{page}: insecure {target}");
            let (file, anchor) = match target.split_once('#') {
                Some((file, anchor)) => (file, Some(anchor)),
                None => (target, None),
            };
            let file = if file.is_empty() { *page } else { file };
            assert!(
                repo().join("site").join(file).is_file(),
                "{page}: link to missing file {target}"
            );
            if let Some(anchor) = anchor {
                let page_ids = ids
                    .get(file)
                    .unwrap_or_else(|| panic!("{page}: anchor into non-page {target}"));
                assert!(
                    page_ids.contains(anchor),
                    "{page}: {target} has no matching id"
                );
            }
            checked += 1;
        }
    }
    assert!(checked > 50, "only {checked} links checked");

    let releases = "https://github.com/pawanpaudel93/readshot/releases/latest";
    assert!(
        pages["index.html"].contains(&format!("href=\"{releases}\"")),
        "WEB-01: the download button goes to the latest release"
    );
}

const INSTALL: &str = "curl -fsSL https://readshot.pawanpaudel.com.np/install.sh | bash";
const INSTALL_TARGET: &str =
    "https://github.com/pawanpaudel93/readshot/releases/latest/download/install.sh";

/// WEB-02, WEB-03, WEB-10, WEB-11, WEB-12: the copy button copies the
/// command that is shown (desktop and narrow-screen versions), the
/// README and llms.txt show the same one, both hosts redirect it
/// (temporarily) to the release asset, and the release ships that asset.
#[test]
fn install_command_is_consistent_and_its_redirect_resolves() {
    let index = read("site/index.html");
    let copied = attr_values(&index, "data-copy");
    assert_eq!(copied, [INSTALL]);
    assert_eq!(
        unescape_html(&element_text(&index, "desktop-command")),
        INSTALL
    );
    assert_eq!(
        unescape_html(&element_text(&index, "mobile-command")),
        INSTALL,
        "the narrow-screen command breaks only with <wbr>, so a manual copy still works"
    );
    assert!(read("site/llms.txt").contains(INSTALL));
    assert!(read("README.md").contains(INSTALL));

    let redirects = read("site/_redirects");
    let rule: Vec<&str> = redirects
        .lines()
        .find(|l| l.starts_with("/install.sh "))
        .expect("_redirects has /install.sh")
        .split_whitespace()
        .collect();
    assert_eq!(rule, ["/install.sh", INSTALL_TARGET, "302"]);

    let vercel: serde_json::Value = serde_json::from_str(&read("site/vercel.json")).unwrap();
    let rule = vercel["redirects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["source"] == "/install.sh")
        .expect("vercel.json has /install.sh");
    assert_eq!(rule["destination"], INSTALL_TARGET);
    assert_eq!(rule["permanent"], false);

    let release = read(".github/workflows/release.yml");
    assert!(release.contains("cp install.sh out/install.sh"));
    assert!(release.contains("out/install.sh"));
    assert!(repo().join("install.sh").is_file());
}

/// WEB-12, WEB-13: llms.txt, robots.txt and the sitemap point at pages
/// that exist.
#[test]
fn llms_robots_and_sitemap_point_at_real_pages() {
    let host = "https://readshot.pawanpaudel.com.np/";
    let sitemap = read("site/sitemap.xml");
    let locs: Vec<&str> = sitemap
        .split("<loc>")
        .skip(1)
        .map(|s| s.split("</loc>").next().unwrap())
        .collect();
    assert_eq!(locs.len(), PAGES.len(), "{locs:?}");
    for loc in &locs {
        let path = loc.strip_prefix(host).expect("sitemap on the site's host");
        let file = if path.is_empty() { "index.html" } else { path };
        assert!(PAGES.contains(&file), "sitemap lists {loc}");
    }
    assert!(read("site/robots.txt").contains(&format!("Sitemap: {host}sitemap.xml")));

    let llms = read("site/llms.txt");
    for page in ["", "cli.html", "mcp.html"] {
        assert!(
            llms.contains(&format!("({host}{page})")),
            "llms.txt: {page}"
        );
    }
    assert!(llms.contains("(https://github.com/pawanpaudel93/readshot/releases)"));
}

/// Every `readshot …` command line in `text`, with pipes into it,
/// redirections and trailing comments removed.
fn readshot_commands(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let line = line.split(" #").next().unwrap();
            let line = line.rsplit("| ").next().unwrap().trim();
            let line = line.split(" >").next().unwrap().trim();
            line.starts_with("readshot ").then(|| line.to_string())
        })
        .collect()
}

fn markdown_bash_blocks(md: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<String> = None;
    let mut skipping = false;
    for line in md.lines() {
        if let Some(info) = line.strip_prefix("```") {
            if let Some(block) = current.take() {
                blocks.push(block);
            } else if skipping {
                skipping = false;
            } else if info.trim() == "bash" {
                current = Some(String::new());
            } else {
                skipping = true;
            }
            continue;
        }
        if let Some(block) = current.as_mut() {
            block.push_str(line);
            block.push('\n');
        }
    }
    blocks
}

/// WEB-07, WEB-08, CLI docs: every command the site and docs/CLI.md
/// show parses with the real CLI definition.
#[test]
fn every_documented_command_parses() {
    let mut commands = Vec::new();
    for page in PAGES {
        for block in code_blocks(&read(&format!("site/{page}"))) {
            for cmd in readshot_commands(&block) {
                commands.push((format!("site/{page}"), cmd));
            }
        }
    }
    for block in markdown_bash_blocks(&read("docs/CLI.md")) {
        for cmd in readshot_commands(&block) {
            commands.push(("docs/CLI.md".to_string(), cmd));
        }
    }
    assert!(
        commands.len() > 60,
        "found only {} commands",
        commands.len()
    );
    for (source, cmd) in &commands {
        let argv = cmd.split_whitespace();
        if let Err(err) = Cli::try_parse_from(argv) {
            panic!("{source}: `{cmd}` does not parse:\n{err}");
        }
    }

    // The index page's automation samples, exactly as shown (WEB-07).
    let index = read("site/index.html");
    for sample in [
        "readshot capture --interactive -o capture.png",
        "readshot capture-text --interactive --clipboard",
        "readshot mcp-config",
        "readshot-mcp --check",
    ] {
        assert!(index.contains(sample), "index.html: {sample}");
    }
}

/// The exit codes a documented table lists.
fn table_codes(rows: impl Iterator<Item = String>) -> BTreeSet<i32> {
    rows.filter_map(|row| row.trim().parse().ok()).collect()
}

/// CLI-44, WEB-08: the exit-code tables in docs/CLI.md and cli.html
/// list exactly the codes the CLI can return (plus 0 and clap's 2).
#[test]
fn exit_code_tables_match_the_code() {
    let io = || std::io::Error::other("x");
    let errors = [
        CliError::Capture(CaptureError::PermissionDenied),
        CliError::Capture(CaptureError::DisplayNotFound("d".into())),
        CliError::Capture(CaptureError::WindowNotFound("w".into())),
        CliError::Capture(CaptureError::InvalidRegion("r".into())),
        CliError::Capture(CaptureError::Backend("b".into())),
        CliError::Capture(CaptureError::Unsupported("u".into())),
        CliError::Ocr(OCRError::NoText),
        CliError::Ocr(OCRError::Backend("o".into())),
        CliError::Io(io()),
        CliError::Image(image::ImageError::IoError(io())),
        CliError::Clipboard("c".into()),
        CliError::DisplayNotFound("d".into()),
        CliError::NoDisplays,
        CliError::InvalidInput("i".into()),
        CliError::InvalidRect {
            got: "g".into(),
            reason: "r".into(),
        },
        CliError::Uninstall("u".into()),
    ];
    let mut expected: BTreeSet<i32> = errors.iter().map(exit_code).collect();
    expected.extend([0, 2]);

    let md = read("docs/CLI.md");
    let section = md.split("## Exit Codes").nth(1).unwrap();
    let section = section.split("\n## ").next().unwrap();
    let md_codes = table_codes(
        section
            .lines()
            .filter_map(|l| l.strip_prefix('|'))
            .map(|l| l.split('|').next().unwrap().to_string()),
    );
    assert_eq!(md_codes, expected, "docs/CLI.md exit codes");

    let cli = read("site/cli.html");
    let section = cli.split("id=\"exit-codes\"").nth(1).unwrap();
    let section = section.split("</table>").next().unwrap();
    let html_codes = table_codes(
        section
            .split("<td><code translate=\"no\">")
            .skip(1)
            .map(|s| s.split('<').next().unwrap().to_string()),
    );
    assert_eq!(html_codes, expected, "site/cli.html exit codes");
}
