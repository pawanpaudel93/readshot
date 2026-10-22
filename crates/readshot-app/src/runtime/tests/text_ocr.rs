//! Text recognition, smart redaction and word masking driven through
//! the real paths (W5c slice C: TX-05 … TX-34).
//!
//! Every History here is a tempdir `FsHistoryStore`; OCR is the
//! `FakeOcrEngine` (wrapped to record its requests); no returned Task is
//! run, so nothing reaches the clipboard, the screen or the user's data.

use super::*;
use readshot_core::error::OCRError;
use readshot_core::ocr_layout::RecognizedLine;
use readshot_ocr::{OCREngine, OCRRequest, OCRResult};

/// What one OCR request asked for: languages, language correction.
type Asked = (Vec<String>, bool);

/// A `FakeOcrEngine` that also records what each request asked for.
#[derive(Clone)]
struct RecordingOcr {
    inner: FakeOcrEngine,
    asked: Arc<std::sync::Mutex<Vec<Asked>>>,
}

impl RecordingOcr {
    fn new(inner: FakeOcrEngine) -> Self {
        Self {
            inner,
            asked: Arc::default(),
        }
    }

    fn asked(&self) -> Vec<Asked> {
        self.asked.lock().unwrap().clone()
    }
}

#[async_trait]
impl OCREngine for RecordingOcr {
    fn supported_languages(&self) -> Vec<String> {
        self.inner.supported_languages()
    }

    fn reports_positions(&self) -> bool {
        self.inner.reports_positions()
    }

    async fn recognise(&self, req: OCRRequest) -> Result<OCRResult, OCRError> {
        self.asked
            .lock()
            .unwrap()
            .push((req.languages.clone(), req.use_language_correction));
        self.inner.recognise(req).await
    }
}

/// `persist_history` (the capture's History save + OCR) to completion.
async fn persist(coord: &CaptureCoordinator, prefs: &Preferences) -> Vec<Message> {
    use iced::futures::StreamExt;
    let record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 8, 8, "d");
    let (tx, rx) = iced::futures::channel::mpsc::channel(4);
    persist_history(
        coord.clone(),
        solid(8, 8),
        Some(record),
        readshot_core::HistoryRetention::Last50,
        prefs.clone(),
        None,
        tx,
    )
    .await;
    rx.collect().await
}

fn settings_key(app: &mut App, id: iced::window::Id, key: iced::keyboard::key::Named) {
    let _ = update(
        app,
        Message::Settings(SettingsMsg::KeyPressed {
            window: id,
            key: iced::keyboard::Key::Named(key),
            modifiers: iced::keyboard::Modifiers::empty(),
            captured: false,
        }),
    );
}

/// TX-05, TX-06: languages ticked with the keyboard in Settings › Text
/// Recognition are what the next capture's OCR (History and Quick Copy
/// Text) asks for, always with language correction on; none ticked asks
/// for automatic detection.
#[tokio::test]
async fn ticked_languages_reach_the_next_capture_with_language_correction() {
    use iced::keyboard::key::Named;
    let ocr = RecordingOcr::new(FakeOcrEngine::no_text().with_supported_languages(vec![
        "en-US".into(),
        "fr-FR".into(),
        "ja-JP".into(),
    ]));
    let dir = tempfile::TempDir::new().unwrap();
    let store: Arc<dyn HistoryStore> = Arc::new(FsHistoryStore::new(dir.path().join("history")));
    let perms = Arc::new(FakePermissions::granted());
    let coord = CaptureCoordinator::new(
        Arc::new(FakeCapturer::new()),
        Arc::new(ocr.clone()),
        perms.clone(),
        Some(store),
    );
    let mut app = App::new(coord, perms, Preferences::default());
    let id = iced::window::Id::unique();
    app.settings_window_id = Some(id);
    let _ = update(
        &mut app,
        Message::Settings(SettingsMsg::PaneSelected(SettingsPane::TextRecognition)),
    );

    // Tab onto the first language, ↓ to French, Space ticks it.
    settings_key(&mut app, id, Named::Tab);
    settings_key(&mut app, id, Named::ArrowDown);
    settings_key(&mut app, id, Named::Space);
    assert_eq!(app.preferences.ocr_languages, ["fr-FR"]);
    assert_eq!(app.settings_key_feedback.as_deref(), Some("French: On"));
    // ↓ to Japanese and tick it too: priority is the order ticked.
    settings_key(&mut app, id, Named::ArrowDown);
    settings_key(&mut app, id, Named::Space);
    assert_eq!(app.preferences.ocr_languages, ["fr-FR", "ja-JP"]);

    let _ = persist(&app.coordinator, &app.preferences).await;
    let text = ocr_then_copy(
        app.coordinator.clone(),
        solid(8, 8),
        app.preferences.clone(),
        false,
        editor_ops::CopyAllMask::None,
    )
    .await
    .unwrap();
    assert_eq!(
        text, "",
        "nothing recognized: the clipboard is never written"
    );
    let ticked = (vec!["fr-FR".to_string(), "ja-JP".to_string()], true);
    assert_eq!(ocr.asked(), [ticked.clone(), ticked]);

    // Untick both: an empty list is automatic detection.
    settings_key(&mut app, id, Named::Space);
    settings_key(&mut app, id, Named::ArrowUp);
    settings_key(&mut app, id, Named::Space);
    assert_eq!(app.settings_key_feedback.as_deref(), Some("French: Off"));
    assert!(app.preferences.ocr_languages.is_empty());
    let _ = persist(&app.coordinator, &app.preferences).await;
    assert_eq!(ocr.asked().last(), Some(&(Vec::new(), true)));
}

fn line(text: &str, x: f32, y: f32, w: f32, h: f32) -> RecognizedLine {
    ocr_line(text, x, y, w, h)
}

/// TX-07 … TX-11: each layout rule reaches the capture's two consumers
/// through the real pipeline (`persist_history` → coordinator →
/// `TextDocument`): the Text panel's document and the History text.
/// The CLI and MCP equality tests (`cli_ocr_text_equals_the_gui_pipeline`,
/// `capture_text_tools_return_the_gui_layout_text`) tie those surfaces
/// to the same pass.
#[tokio::test]
async fn every_layout_rule_reaches_the_text_panel_and_history() {
    let cases: Vec<(&str, FakeOcrEngine, &str)> = vec![
        (
            "TX-07 indentation",
            FakeOcrEngine::with_lines(vec![
                line("root", 0.00, 0.10, 0.05, 0.04),
                line("child", 0.04, 0.20, 0.05, 0.04),
                line("grandchild", 0.08, 0.30, 0.10, 0.04),
            ]),
            "root\n    child\n        grandchild",
        ),
        (
            "TX-08 table",
            FakeOcrEngine::with_lines(vec![
                line("Status", 0.30, 0.10, 0.06, 0.04),
                line("Name", 0.00, 0.10, 0.04, 0.04),
                line("a | b", 0.00, 0.20, 0.05, 0.04),
                line("Shipped", 0.30, 0.20, 0.07, 0.04),
            ]),
            "| Name | Status |\n| --- | --- |\n| a \\| b | Shipped |",
        ),
        (
            "TX-09 two columns",
            FakeOcrEngine::with_lines(vec![
                line("Left column starts here", 0.00, 0.10, 0.24, 0.04),
                line("Right column starts here", 0.58, 0.10, 0.25, 0.04),
                line("Left column continues", 0.00, 0.20, 0.22, 0.04),
                line("Right column continues", 0.58, 0.20, 0.24, 0.04),
            ]),
            "Left column starts here\nLeft column continues\n\n\
             Right column starts here\nRight column continues",
        ),
        (
            "TX-10 braced code",
            FakeOcrEngine::with_lines(vec![
                line("fn main() {", 0.00, 0.10, 0.11, 0.04),
                line("println!(\"hi\");", 0.04, 0.20, 0.16, 0.04),
                line("}", 0.00, 0.30, 0.01, 0.04),
            ]),
            "```\nfn main() {\n    println!(\"hi\");\n}\n```",
        ),
        (
            "TX-10 shell output",
            FakeOcrEngine::with_lines(vec![
                line("$ cargo test", 0.00, 0.10, 0.12, 0.04),
                line("test result: ok. 42 passed", 0.00, 0.20, 0.27, 0.04),
            ]),
            "```\n$ cargo test\ntest result: ok. 42 passed\n```",
        ),
        (
            "TX-11 cleanup of flat text",
            FakeOcrEngine::with_text("\r\n\r\nfirst line   \r\n\r\n\r\n\r\nsecond\t\rthird\n\n"),
            "first line\n\nsecond\nthird",
        ),
    ];
    for (what, ocr, want) in cases {
        let (msgs, coord, record, _dir) = run_persist_history(ocr, solid(100, 100), Some(9)).await;
        match &msgs[0] {
            Message::Editor(EditorMsg::TextPanel(TextPanelMsg::Recognized {
                result: Ok(doc),
                ..
            })) => assert_eq!(doc.layout_text, want, "{what}: panel"),
            other => panic!("{what}: expected the panel's document first, got {other:?}"),
        }
        let stored = coord.history_list().unwrap();
        let stored = stored.iter().find(|r| r.id == record.id).unwrap();
        assert_eq!(stored.ocr_text.as_deref(), Some(want), "{what}: History");
    }
}

/// TX-12: an engine that finds nothing returns an empty result, never an
/// error: the Text panel shows its no-text state (no Try Again), the
/// History record is saved, and Copy All says "No text recognized.".
#[tokio::test]
async fn an_empty_recognition_is_a_result_not_an_error() {
    let (msgs, coord, record, _dir) =
        run_persist_history(FakeOcrEngine::no_text(), solid(16, 16), Some(4)).await;
    let doc = match &msgs[0] {
        Message::Editor(EditorMsg::TextPanel(TextPanelMsg::Recognized {
            result: Ok(doc), ..
        })) => doc.clone(),
        other => panic!("expected an empty document, got {other:?}"),
    };
    assert!(doc.is_empty() && doc.layout_text.is_empty());
    assert!(matches!(
        msgs.last(),
        Some(Message::History(HistoryMsg::RecordPersisted(Ok(_))))
    ));
    let stored = coord.history_list().unwrap();
    let stored = stored.iter().find(|r| r.id == record.id).unwrap();
    assert!(stored.ocr_text.as_deref().unwrap_or("").is_empty());
    assert!(!super::super::history_ops::history_text_hidden(stored));

    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(16, 16)));
    let session = app.editor.as_ref().unwrap().session_id;
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::TextPanel(TextPanelMsg::Recognized {
            session,
            part: 0,
            result: Ok(doc),
        })),
    );
    let ed = app.editor.as_ref().unwrap();
    assert!(matches!(
        &ed.text_panel.recognition,
        crate::editor::TextRecognition::Ready(d) if d.is_empty()
    ));
    assert_eq!(ed.toast_action(), None, "no Try Again for an empty result");
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::CopyTextDone {
            session,
            result: Ok(String::new()),
        }),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.status_text(), Some("No text recognized."));
    assert_eq!(ed.status_tone(), Some(StatusTone::Notice));
}

/// One Text-panel line as the panel draws it: pending matches marked,
/// covered ones muted (the same split `lines_view` makes per line).
fn drawn_line(app: &App, i: usize) -> Vec<(String, super::super::text_panel::Run)> {
    let ed = app.editor.as_ref().unwrap();
    let rs = super::super::text_panel::redact_state(ed);
    let marks: Vec<_> = rs
        .pending
        .iter()
        .filter(|(m, _)| m.line == i)
        .map(|(m, _)| m.span.clone())
        .collect();
    let muted: Vec<_> = rs
        .covered
        .iter()
        .filter(|m| m.line == i)
        .map(|m| m.span.clone())
        .collect();
    let doc = ed.text_panel.document().unwrap();
    super::super::text_panel::line_spans(&doc.lines[i].text, &marks, &muted)
}

/// TX-29: a detected item is highlighted until a redaction covers it,
/// then muted. With Solid chosen, an item only blurred stays pending
/// (highlighted); with Blur chosen the blur counts.
#[test]
fn pending_items_are_highlighted_and_covered_ones_muted() {
    use super::super::text_panel::Run::{Marked, Plain, Redacted};
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    ready_editor(&mut app);
    let phone = "+1 (415) 555-0142";
    let s = |t: &str| t.to_string();
    assert_eq!(drawn_line(&app, 0), [(s("Name Priya"), Plain)]);
    assert_eq!(
        drawn_line(&app, 1),
        [(s("Email "), Plain), (s(EMAIL), Marked)]
    );
    assert_eq!(
        drawn_line(&app, 2),
        [(s("Phone "), Plain), (s(phone), Marked)]
    );

    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::TextPanel(TextPanelMsg::RedactLine(1))),
    );
    assert_eq!(
        drawn_line(&app, 1),
        [(s("Email "), Plain), (s(EMAIL), Redacted)]
    );
    assert_eq!(
        drawn_line(&app, 2),
        [(s("Phone "), Plain), (s(phone), Marked)],
        "only that line was redacted"
    );

    // A blur over the phone: covered, but Solid is chosen, so the mark
    // wins (still to redact)…
    let ed = app.editor.as_mut().unwrap();
    ed.model
        .commit_annotation(word_blur(RectLike::new(0.0, 30.0, 64.0, 10.0)));
    assert_eq!(
        drawn_line(&app, 2),
        [(s("Phone "), Plain), (s(phone), Marked)]
    );
    // …and with Blur chosen the blur is enough: muted.
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::TextPanel(TextPanelMsg::RedactStyleChanged(
            readshot_core::redact::RedactStyle::Blur,
        ))),
    );
    assert_eq!(
        drawn_line(&app, 2),
        [(s("Phone "), Plain), (s(phone), Redacted)]
    );
}

/// Pseudo-random pixels: blur and pixelate change almost every one.
fn noisy(w: u32, h: u32) -> image::RgbaImage {
    image::RgbaImage::from_fn(w, h, |x, y| {
        let n =
            (x.wrapping_mul(374_761_393) ^ y.wrapping_mul(668_265_263)).wrapping_mul(1_274_126_177);
        image::Rgba([n as u8, (n >> 8) as u8, (n >> 16) as u8, 255])
    })
}

/// An editor on a saved History record over `img` whose text is
/// `doc`'s, recognized with positions.
fn history_editor_with(
    img: &image::RgbaImage,
    doc: readshot_core::text_lines::TextDocument,
) -> (tempfile::TempDir, Arc<FsHistoryStore>, App) {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(FsHistoryStore::new(dir.path().join("history")));
    let mut app = build_app_with_history(
        Arc::new(FakePermissions::granted()),
        store.clone() as Arc<dyn HistoryStore>,
    );
    let mut record =
        readshot_core::CaptureRecord::new(chrono::Utc::now(), img.width(), img.height(), "d");
    record.ocr_text = Some(doc.layout_text.clone());
    store
        .save(&record, &readshot_core::encode_png(img).unwrap())
        .unwrap();
    app.editor = Some(crate::editor::EditorSession::from_history(
        img.clone(),
        record,
    ));
    recognized(&mut app, 0, doc);
    (dir, store, app)
}

fn commit(app: &mut App, a: Annotation) {
    let _ = update(
        app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::CommitAnnotation(a),
        )),
    );
    app.coordinator.flush_history_updates();
}

/// The email's word box in `sample_doc()` (x 22.4–57.6, y 19.2–25.6),
/// in whole pixels.
fn email_pixels() -> impl Iterator<Item = (u32, u32)> {
    (23..57).flat_map(|x| (20..25).map(move |y| (x, y)))
}

/// TX-33: only an opaque, square-cornered filled rectangle, a Blur or a
/// Pixelate is a redaction. Each of those, drawn over the email, scrubs
/// History: the stored PNG and thumbnail show it (no pixel under it is
/// the original) and the stored text masks the email. Anything else —
/// translucent or rounded or outlined rectangles, a filled ellipse, a
/// highlighter, an arrow — hides nothing: the record keeps its original
/// image and full text, with the annotation kept beside it.
#[test]
fn only_solid_blur_and_pixelate_scrub_history() {
    use readshot_core::redact::{redaction_annotation, RedactStyle};
    use readshot_core::{FillMode, ShapeStyle};
    let over = RectLike::new(20.0, 18.0, 40.0, 9.0);
    let filled = ShapeStyle {
        fill: FillMode::Filled,
        ..ShapeStyle::default()
    };
    let rect = |color: Rgba, style: ShapeStyle| Annotation::Rectangle {
        rect: over,
        color,
        line_width: 2.0,
        style,
    };
    let redactions = [
        ("solid", redaction_annotation(over, RedactStyle::Solid)),
        ("blur", redaction_annotation(over, RedactStyle::Blur)),
        (
            "pixelate",
            redaction_annotation(over, RedactStyle::Pixelate),
        ),
    ];
    let others = [
        (
            "translucent fill",
            rect(Rgba::new(0.0, 0.0, 0.0, 0.5), filled),
        ),
        (
            "rounded fill",
            rect(
                Rgba::OPAQUE_BLACK,
                ShapeStyle {
                    corner_radius: 4.0,
                    ..filled
                },
            ),
        ),
        ("outline", rect(Rgba::OPAQUE_BLACK, ShapeStyle::default())),
        (
            "filled ellipse",
            Annotation::Ellipse {
                rect: over,
                color: Rgba::OPAQUE_BLACK,
                line_width: 2.0,
                style: filled,
            },
        ),
        (
            "highlighter",
            Annotation::Highlighter {
                points: vec![PointLike::new(22.0, 22.0), PointLike::new(58.0, 22.0)],
                color: Rgba::new(1.0, 1.0, 0.0, 0.45),
                line_width: 12.0,
            },
        ),
        ("arrow", arrow()),
    ];
    let original = noisy(64, 64);
    for (what, a) in redactions {
        let (_dir, store, mut app) = history_editor_with(&original, sample_doc());
        commit(&mut app, a);
        let (record, png) = stored(&store);
        assert!(record.redacted, "{what}");
        let text = record.ocr_text.clone().unwrap_or_default();
        assert!(!text.contains(EMAIL), "{what}: {text}");
        assert!(
            text.contains(&"█".repeat(EMAIL.chars().count())),
            "{what}: {text}"
        );
        assert!(record.annotation_model.is_empty(), "{what}: baked in");
        let ed = app.editor.as_mut().unwrap();
        assert_eq!(png, ed.model.flatten(), "{what}: the PNG is the output");
        let same = email_pixels()
            .filter(|&(x, y)| png.get_pixel(x, y) == original.get_pixel(x, y))
            .count();
        assert!(
            same * 20 < email_pixels().count(),
            "{what}: {same} pixels unchanged"
        );
        let thumb =
            std::fs::read(FsHistoryStore::abs_thumbnail_path(store.root(), &record)).unwrap();
        assert_eq!(
            thumb,
            readshot_core::history::thumbnail_png(&png).unwrap(),
            "{what}: the thumbnail is of the scrubbed image"
        );
    }
    for (what, a) in others {
        let (_dir, store, mut app) = history_editor_with(&original, sample_doc());
        commit(&mut app, a);
        let (record, png) = stored(&store);
        assert!(!record.redacted, "{what}");
        assert_eq!(
            record.ocr_text.as_deref(),
            Some(sample_doc().layout_text.as_str()),
            "{what}"
        );
        assert_eq!(png, original, "{what}: the original image is kept");
        assert_eq!(record.annotation_model.len(), 1, "{what}");
    }
}

/// TX-34: the word-masking rule through the editor, for the History text
/// (also what MCP serves) and Copy Lines. A word covered at all is
/// masked in full, one █ per char; a hairline graze (under 1 px deep and
/// 1 % of the word box, measured after the cover is widened to the
/// whole pixels it paints) is ignored, but hairlines add up; a cover
/// over a line without word boxes masks every token of that line.
#[test]
fn a_redaction_masks_whole_words_in_history_and_copy_lines() {
    use readshot_core::redact::{redaction_annotation, RedactStyle};
    let block = |x: f32, y: f32, w: f32, h: f32| {
        redaction_annotation(RectLike::new(x, y, w, h), RedactStyle::Solid)
    };
    let mask = |s: &str| "█".repeat(s.chars().count());
    // One 80 × 40 word box at x 10.4–90.4 (3200 px²) of a 100 × 60
    // capture. A cover ending at x 10.6 paints up to x 11: 0.6 px deep,
    // 24 px² (0.75 %). One ending at x 90 paints from x 90: 0.4 px deep,
    // 16 px² (0.5 %).
    let wide = || positioned_doc("hunter2", RectLike::new(10.4, 10.0, 80.0, 40.0));
    let left_graze = || block(0.0, 0.0, 10.6, 60.0);
    let right_graze = || block(90.0, 0.0, 10.0, 60.0);
    let email_line = format!("Email {}", mask(EMAIL));
    let cases = vec![
        (
            "a quarter of the word",
            solid(64, 64),
            sample_doc(),
            vec![block(22.0, 18.0, 9.0, 9.0)],
            1,
            email_line,
        ),
        (
            "a hairline graze",
            solid(100, 60),
            wide(),
            vec![left_graze()],
            0,
            "hunter2".to_string(),
        ),
        (
            "two hairlines that add up (1.25 %)",
            solid(100, 60),
            wide(),
            vec![left_graze(), right_graze()],
            0,
            mask("hunter2"),
        ),
        (
            "a cover over a line without word boxes",
            solid(64, 64),
            sample_doc(),
            vec![block(8.0, 6.0, 4.0, 7.0)],
            0,
            format!("{} {}", mask("Name"), mask("Priya")),
        ),
    ];
    for (what, img, doc, covers, line, want) in cases {
        let (_dir, store, mut app) = history_editor_with(&img, doc);
        for c in covers {
            commit(&mut app, c);
        }
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(
            super::super::text_panel::copy_lines_text(ed, &[line], false).as_deref(),
            Some(want.as_str()),
            "{what}: Copy Lines"
        );
        let (record, _) = stored(&store);
        assert!(record.redacted, "{what}");
        let text = record.ocr_text.unwrap();
        assert_eq!(
            text.lines().nth(line),
            Some(want.as_str()),
            "{what}: History text {text:?}"
        );
    }
}
