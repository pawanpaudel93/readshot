//! The runtime's tests, by area. Shared helpers live here; each
//! submodule sees them (and the runtime) through `use super::*`.

use super::*;

use crate::app::SettingsPane;
use crate::permissions::fake::FakePermissions;
use crate::permissions::PermissionStatus;
use async_trait::async_trait;
use global_hotkey::HotKeyState;
use iced::widget::button;
use readshot_capture::fake::FakeCapturer;
use readshot_capture::{Capturer, DisplayInfo};
use readshot_core::beautify::BuiltinPreset;
use readshot_core::error::CaptureError;
use readshot_core::{Annotation, FsHistoryStore, HistoryStore, PointLike, RectLike, Rgba};
use readshot_ocr::fake::FakeOcrEngine;
use std::collections::HashMap;

fn build_app(perms: Arc<FakePermissions>) -> App {
    let coord = CaptureCoordinator::new(
        Arc::new(FakeCapturer::new()),
        Arc::new(FakeOcrEngine::with_text("hi")),
        perms.clone(),
        None,
    );
    App::new(coord, perms, Preferences::default())
}

fn build_app_with_history(perms: Arc<FakePermissions>, history: Arc<dyn HistoryStore>) -> App {
    let coord = CaptureCoordinator::new(
        Arc::new(FakeCapturer::new()),
        Arc::new(FakeOcrEngine::with_text("hi")),
        perms.clone(),
        Some(history),
    );
    App::new(coord, perms, Preferences::default())
}

/// A mouse drag on a Beautify slider: the press tick (iced sends it
/// before the button event reaches the subscription), the button,
/// more ticks, then release.
fn drag_padding(app: &mut App, values: &[u32]) {
    use readshot_core::beautify::BeautifyEdit;
    for (i, v) in values.iter().enumerate() {
        let _ = update(
            app,
            Message::EditorBeautifySliderPreview(BeautifyEdit::Padding(*v)),
        );
        if i == 0 {
            assert_eq!(
                app.editor.as_ref().unwrap().model.beautify().padding,
                *v,
                "the press tick previews"
            );
            let _ = update(app, Message::EditorPointerButton(true, None));
        }
    }
    let _ = update(app, Message::EditorSnapshotSliderCommit);
    let _ = update(app, Message::EditorPointerButton(false, None));
}

fn editor_with_draft_and_color_popover(app: &mut App) -> iced::window::Id {
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    let id = iced::window::Id::unique();
    ed.window_id = Some(id);
    ed.begin_text_edit(crate::editor::PendingText {
        origin: PointLike::new(10.0, 10.0),
        content: "hello".into(),
        edit_index: None,
    });
    app.editor = Some(ed);
    let _ = update(
        app,
        Message::EditorPopoverToggled(crate::editor::EditorPopover::Color),
    );
    id
}

/// One Esc press as the running editor delivers it. The inline text
/// editor lets Esc through (`editor_text_key_binding` → None) and the
/// annotation canvas captures every Esc (publishing `Cancelled`), so
/// the keyboard subscription always sees `status_ignored: false`.
fn press_escape(app: &mut App, id: iced::window::Id) {
    assert!(editor_text_key_binding(escape_key_press()).is_none());
    let _ = update(
        app,
        Message::EditorCanvas(readshot_ui::CanvasMessage::Escaped),
    );
    let _ = update(
        app,
        Message::EditorKeyPressed {
            window: id,
            key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
            modifiers: iced::keyboard::Modifiers::empty(),
            status_ignored: false,
        },
    );
}

fn escape_key_press() -> iced::widget::text_editor::KeyPress {
    iced::widget::text_editor::KeyPress {
        key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
        modified_key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
        physical_key: iced::keyboard::key::Physical::Code(iced::keyboard::key::Code::Escape),
        modifiers: iced::keyboard::Modifiers::empty(),
        text: None,
        status: iced::widget::text_editor::Status::Focused { is_hovered: false },
    }
}

fn overlay_display(id: &str) -> crate::app::OverlayDisplay {
    crate::app::OverlayDisplay {
        display_id: id.to_string(),
        scale: 1.0,
        origin_x: 0.0,
        origin_y: 0.0,
        width: 800.0,
        height: 600.0,
    }
}

fn solid(w: u32, h: u32) -> image::RgbaImage {
    let mut img = image::RgbaImage::new(w, h);
    for px in img.pixels_mut() {
        *px = image::Rgba([255, 255, 255, 255]);
    }
    img
}

fn command_modifiers() -> iced::keyboard::Modifiers {
    let mut modifiers = iced::keyboard::Modifiers::default();
    #[cfg(target_os = "macos")]
    modifiers.insert(iced::keyboard::Modifiers::LOGO);
    #[cfg(not(target_os = "macos"))]
    modifiers.insert(iced::keyboard::Modifiers::CTRL);
    modifiers
}

/// The editor's second-level chord modifiers (see `editor_alt_chord`):
/// ⌥⌘ on macOS, Ctrl+Shift elsewhere.
fn editor_alt_modifiers() -> iced::keyboard::Modifiers {
    let mut modifiers = command_modifiers();
    if cfg!(target_os = "macos") {
        modifiers.insert(iced::keyboard::Modifiers::ALT);
    } else {
        modifiers.insert(iced::keyboard::Modifiers::SHIFT);
    }
    modifiers
}

fn scrolling_texture(w: u32, h: u32, offset: u32) -> image::RgbaImage {
    let mut img = image::RgbaImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let content_y = y + offset;
            let r = ((content_y * 3 + x * 5) % 251) as u8;
            let g = ((content_y * 7 + x * 11) % 253) as u8;
            let b = ((content_y * 13 + x * 17) % 247) as u8;
            img.put_pixel(x, y, image::Rgba([r, g, b, 255]));
        }
    }
    img
}

fn test_session() -> crate::app::ScrollSession {
    crate::app::ScrollSession::new(
        "display-a".into(),
        readshot_core::geom::Rect::from_xywh(0.0, 0.0, 80.0, 120.0).unwrap(),
        1.0,
    )
}

/// A scroll session with live HUD + region windows registered.
fn app_with_scroll_windows(stopping: bool) -> (App, window::Id, window::Id) {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let hud = window::Id::unique();
    let region = window::Id::unique();
    app.windows.register(hud, WindowKind::ScrollHud);
    app.windows.register(region, WindowKind::ScrollRegion);
    let mut s = test_session();
    s.hud_window_id = Some(hud);
    s.region_window_id = Some(region);
    s.stopping = stopping;
    app.scroll_session = Some(s);
    (app, hud, region)
}

// ---- #6: single-capture delete needs a confirm ----

fn app_with_two_saved_records() -> (
    tempfile::TempDir,
    Arc<FsHistoryStore>,
    App,
    readshot_core::CaptureRecord,
    readshot_core::CaptureRecord,
) {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(FsHistoryStore::new(dir.path().join("history")));
    let older =
        readshot_core::CaptureRecord::new(chrono::Utc::now(), 100, 100, "primary".to_string());
    let newer =
        readshot_core::CaptureRecord::new(chrono::Utc::now(), 120, 90, "primary".to_string());
    store.save(&older, b"png").unwrap();
    store.save(&newer, b"png").unwrap();
    let history: Arc<dyn HistoryStore> = store.clone();
    let mut app = build_app_with_history(Arc::new(FakePermissions::granted()), history);
    app.history_records = vec![newer.clone(), older.clone()];
    app.history_selected_id = Some(newer.id);
    (dir, store, app, newer, older)
}

fn magnifier_app(selected: bool) -> App {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(300, 200));
    // Source (40, 40) r 20, bubble (200, 40) r 40: the leader runs
    // from x 60 to x 160 along y 40.
    ed.model.commit_annotation(Annotation::Magnifier {
        source: PointLike::new(40.0, 40.0),
        radius: 20.0,
        bubble: PointLike::new(200.0, 40.0),
        zoom: 2.0,
        shape: readshot_core::MagnifierShape::Circle,
        color: Rgba::OPAQUE_BLACK,
    });
    if !selected {
        ed.model.clear_selection();
    }
    app.editor = Some(ed);
    app
}

fn select_drag(app: &mut App, from: (f32, f32), to: (f32, f32)) {
    for msg in [
        readshot_ui::CanvasMessage::SelectPressed(PointLike::new(from.0, from.1)),
        readshot_ui::CanvasMessage::SelectDragged(PointLike::new(to.0, to.1)),
        readshot_ui::CanvasMessage::SelectReleased,
    ] {
        let _ = update(app, Message::EditorCanvas(msg));
    }
}

fn magnifier_parts(app: &App) -> (PointLike, f32, PointLike) {
    match &app.editor.as_ref().unwrap().model.annotations()[0] {
        Annotation::Magnifier {
            source,
            radius,
            bubble,
            ..
        } => (*source, *radius, *bubble),
        other => panic!("expected magnifier, got {other:?}"),
    }
}

/// Open-settings app plus a helper that presses `key` with ⌘ (and ⇧
/// when `shift`) in the settings window.
fn settings_app() -> (App, iced::window::Id) {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let id = iced::window::Id::unique();
    app.settings_window_id = Some(id);
    (app, id)
}

fn press_settings_key(app: &mut App, id: iced::window::Id, key: &str, shift: bool) {
    let mut modifiers = command_modifiers();
    if shift {
        modifiers.insert(iced::keyboard::Modifiers::SHIFT);
    }
    let _ = update(
        app,
        Message::SettingsKeyPressed {
            window: id,
            key: iced::keyboard::Key::Character(key.into()),
            modifiers,
            captured: false,
        },
    );
}

fn editor_with_selected_rect() -> crate::editor::EditorSession {
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 40.0, 40.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    assert_eq!(ed.model.select_at(PointLike::new(5.0, 5.0)), Some(0));
    ed
}

fn two_rects_first_selected() -> App {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    for x in [0.0, 30.0] {
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(x, x, 20.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
    }
    assert_eq!(ed.model.select_at(PointLike::new(5.0, 5.0)), Some(0));
    app.editor = Some(ed);
    app
}

fn app_with_text(size: f32) -> App {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(200, 200));
    ed.model.commit_annotation(Annotation::Text {
        content: "hi".into(),
        origin: PointLike::new(20.0, 60.0),
        color: Rgba::OPAQUE_BLACK,
        font_family: "system-ui".into(),
        size,
        style: Default::default(),
    });
    app.editor = Some(ed);
    app
}

fn pick(app: &mut App, x: f32, y: f32) {
    let _ = update(app, Message::EditorEyedropperToggled);
    let _ = update(
        app,
        Message::EditorCanvas(readshot_ui::CanvasMessage::EyedropperPick(PointLike::new(
            x, y,
        ))),
    );
}

/// 64 × 64 capture: "Name Priya" / "Email priya@example.com" (two
/// word boxes) / "Phone +1 (415) 555-0142". Two sensitive items.
fn sample_doc() -> readshot_core::text_lines::TextDocument {
    use readshot_core::ocr_layout::{RecognizedLine, WordBox};
    let line = |text: &str, y: f32, words: Vec<WordBox>| RecognizedLine {
        text: text.into(),
        x: 0.1,
        y,
        w: 0.8,
        h: 0.1,
        words,
    };
    readshot_core::text_lines::TextDocument::from_recognized(
        &[
            line("Name Priya", 0.1, Vec::new()),
            line(
                "Email priya@example.com",
                0.3,
                vec![
                    WordBox {
                        start: 0,
                        end: 5,
                        x: 0.1,
                        y: 0.3,
                        w: 0.2,
                        h: 0.1,
                    },
                    WordBox {
                        start: 6,
                        end: 23,
                        x: 0.35,
                        y: 0.3,
                        w: 0.55,
                        h: 0.1,
                    },
                ],
            ),
            line("Phone +1 (415) 555-0142", 0.5, Vec::new()),
        ],
        64,
        64,
    )
}

/// An editor whose Text panel finished recognizing `sample_doc()`.
fn ready_editor(app: &mut App) {
    app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
    let _ = update(app, Message::EditorTextRecognize);
    let session = app.editor.as_ref().unwrap().session_id;
    let _ = update(
        app,
        Message::EditorTextRecognized {
            session,
            part: 0,
            result: Ok(sample_doc()),
        },
    );
}

fn highlight_stroke() -> Annotation {
    // Over "priya@example.com" (line 1: y 19.2–25.6, word x 22.4–57.6).
    Annotation::Highlighter {
        points: vec![PointLike::new(25.0, 22.0), PointLike::new(40.0, 23.0)],
        color: Rgba::new(1.0, 1.0, 0.0, 0.45),
        line_width: 12.0,
    }
}

fn build_app_with_positions(perms: Arc<FakePermissions>) -> App {
    let coord = CaptureCoordinator::new(
        Arc::new(FakeCapturer::new()),
        Arc::new(FakeOcrEngine::with_lines(Vec::new())),
        perms.clone(),
        None,
    );
    App::new(coord, perms, Preferences::default())
}

fn select_highlighter(app: &mut App) {
    let _ = update(
        app,
        Message::EditorToolbar(readshot_ui::ToolbarMessage::SelectTool(
            readshot_ui::editor::ToolState::Highlighter,
        )),
    );
}

/// Run `persist_history` to completion and collect what it sent.
async fn run_persist_history(
    ocr: FakeOcrEngine,
    img: image::RgbaImage,
    session: Option<u64>,
) -> (
    Vec<Message>,
    CaptureCoordinator,
    readshot_core::CaptureRecord,
    tempfile::TempDir,
) {
    use iced::futures::StreamExt;
    let dir = tempfile::TempDir::new().unwrap();
    let store: Arc<dyn HistoryStore> = Arc::new(FsHistoryStore::new(dir.path().join("history")));
    let perms = Arc::new(FakePermissions::granted());
    let coord = CaptureCoordinator::new(
        Arc::new(FakeCapturer::new()),
        Arc::new(ocr),
        perms,
        Some(store),
    );
    let record =
        readshot_core::CaptureRecord::new(chrono::Utc::now(), img.width(), img.height(), "fake-0");
    let (tx, rx) = iced::futures::channel::mpsc::channel(4);
    persist_history(
        coord.clone(),
        img,
        Some(record.clone()),
        readshot_core::HistoryRetention::Last50,
        Preferences::default(),
        session,
        tx,
    )
    .await;
    let msgs: Vec<Message> = rx.collect().await;
    (msgs, coord, record, dir)
}

// --- History scrub after a redaction (B10) -------------------------

fn decode_png(bytes: &[u8]) -> image::RgbaImage {
    image::load_from_memory(bytes).unwrap().to_rgba8()
}

fn stored(store: &FsHistoryStore) -> (readshot_core::CaptureRecord, image::RgbaImage) {
    let record = store.list().unwrap().remove(0);
    let png = std::fs::read(FsHistoryStore::abs_png_path(store.root(), &record)).unwrap();
    (record, decode_png(&png))
}

/// An editor opened on a saved History record whose text is
/// `sample_doc()`'s, with the panel recognized. `save` false leaves
/// the record off disk (a fresh capture whose save hasn't landed).
fn scrub_app(save: bool) -> (tempfile::TempDir, Arc<FsHistoryStore>, App) {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(FsHistoryStore::new(dir.path().join("history")));
    let mut app = build_app_with_history(
        Arc::new(FakePermissions::granted()),
        store.clone() as Arc<dyn HistoryStore>,
    );
    let mut record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 64, 64, "d");
    record.ocr_text = Some(sample_doc().layout_text);
    if save {
        store
            .save(&record, &readshot_core::encode_png(&solid(64, 64)).unwrap())
            .unwrap();
    }
    app.editor = Some(crate::editor::EditorSession::from_history(
        solid(64, 64),
        record,
    ));
    let session = app.editor.as_ref().unwrap().session_id;
    let _ = update(
        &mut app,
        Message::EditorTextRecognized {
            session,
            part: 0,
            result: Ok(sample_doc()),
        },
    );
    (dir, store, app)
}

const EMAIL: &str = "priya@example.com";

const PHONE: &str = "555-0142";

/// An editor on a History record whose stored text is `text`, with a
/// positional engine and no recognized document yet.
fn positional_scrub_app(text: &str) -> (tempfile::TempDir, Arc<FsHistoryStore>, App) {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(FsHistoryStore::new(dir.path().join("history")));
    let perms = Arc::new(FakePermissions::granted());
    let coord = CaptureCoordinator::new(
        Arc::new(FakeCapturer::new()),
        Arc::new(FakeOcrEngine::with_lines(Vec::new())),
        perms.clone(),
        Some(store.clone() as Arc<dyn HistoryStore>),
    );
    let mut app = App::new(coord, perms, Preferences::default());
    let mut record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 64, 64, "d");
    record.ocr_text = Some(text.to_string());
    store
        .save(&record, &readshot_core::encode_png(&solid(64, 64)).unwrap())
        .unwrap();
    app.editor = Some(crate::editor::EditorSession::from_history(
        solid(64, 64),
        record,
    ));
    (dir, store, app)
}

fn draw_solid_block(app: &mut App) {
    let _ = update(
        app,
        Message::EditorCanvas(readshot_ui::CanvasMessage::CommitAnnotation(
            readshot_core::redact::redaction_annotation(
                RectLike::new(6.0, 6.0, 40.0, 8.0),
                readshot_core::redact::RedactStyle::Solid,
            ),
        )),
    );
}

fn positioned_doc(text: &str, rect: RectLike) -> readshot_core::text_lines::TextDocument {
    use readshot_core::text_lines::{TextDocument, TextLine, WordRect};
    TextDocument {
        lines: vec![TextLine {
            text: text.into(),
            rect: Some(rect),
            words: vec![WordRect {
                start: 0,
                end: text.chars().count(),
                rect,
            }],
        }],
        positioned: true,
        layout_text: text.into(),
    }
}

fn stored_png_dims(
    store: &readshot_core::FsHistoryStore,
    record: &readshot_core::CaptureRecord,
) -> (u32, u32) {
    image::image_dimensions(readshot_core::FsHistoryStore::abs_png_path(
        store.root(),
        record,
    ))
    .unwrap()
}

fn arrow() -> Annotation {
    Annotation::Arrow {
        a: PointLike::new(1.0, 1.0),
        b: PointLike::new(3.0, 3.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 1.0,
        style: Default::default(),
    }
}

/// A 40 × 20 capture cropped to x 20–40, with a weak pixelate over a
/// word at x 30–36 (x 10–16 of the cropped image).
fn cropped_weak_pixelate_scrub(
    store: &Arc<readshot_core::FsHistoryStore>,
    app: &App,
) -> readshot_core::CaptureRecord {
    let img = solid(40, 20);
    let record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 40, 20, "d");
    store
        .save(&record, &readshot_core::encode_png(&img).unwrap())
        .unwrap();
    let mut ed = crate::editor::EditorSession::from_history(img, record.clone());
    ed.model.commit_annotation(Annotation::Crop {
        rect: RectLike::new(20.0, 0.0, 20.0, 20.0),
    });
    ed.model.commit_annotation(Annotation::Pixelate {
        rect: RectLike::new(30.0, 2.0, 6.0, 4.0),
        block_size: 4.0,
    });
    write_editor_history(&mut ed, &app.coordinator);
    app.coordinator.flush_history_updates();
    record
}

/// "outside inside <hidden word>" on a 40 × 20 capture: "outside" at
/// x 2–12, "inside" at x 22–28, the hidden word at x 30–36.
fn three_words() -> readshot_core::text_lines::TextDocument {
    use readshot_core::text_lines::{TextDocument, TextLine, WordRect};
    let text = format!("outside inside {}", hidden_word());
    let word = |start, end, x| WordRect {
        start,
        end,
        rect: RectLike::new(x, 2.0, if start == 0 { 10.0 } else { 6.0 }, 4.0),
    };
    TextDocument {
        lines: vec![TextLine {
            text: text.clone(),
            rect: Some(RectLike::new(2.0, 2.0, 34.0, 4.0)),
            words: vec![word(0, 7, 2.0), word(8, 14, 22.0), word(15, 21, 30.0)],
        }],
        positioned: true,
        layout_text: text,
    }
}

/// A saved 40 × 20 capture (its text already stored) open in the
/// editor, cropped to x 20–40 with the hidden word pixelated.
fn cropped_scrub_session(
    store: &readshot_core::FsHistoryStore,
    app: &mut App,
    doc: Option<readshot_core::text_lines::TextDocument>,
) -> readshot_core::CaptureRecord {
    let img = solid(40, 20);
    let mut record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 40, 20, "d");
    record.ocr_text = Some(three_words().layout_text);
    store
        .save(&record, &readshot_core::encode_png(&img).unwrap())
        .unwrap();
    app.editor = Some(crate::editor::EditorSession::from_history(
        img,
        record.clone(),
    ));
    if let Some(doc) = doc {
        recognized(app, readshot_core::document::ORIGINAL_PART, doc);
    }
    let ed = app.editor.as_mut().unwrap();
    ed.model.commit_annotation(Annotation::Crop {
        rect: RectLike::new(20.0, 0.0, 20.0, 20.0),
    });
    ed.model.commit_annotation(Annotation::Pixelate {
        rect: RectLike::new(30.0, 2.0, 6.0, 4.0),
        block_size: 4.0,
    });
    sync_editor_history(ed, &app.coordinator);
    app.coordinator.flush_history_updates();
    record
}

fn build_app_with_positions_and_history(history: Arc<dyn HistoryStore>) -> App {
    let perms = Arc::new(FakePermissions::granted());
    let coord = CaptureCoordinator::new(
        Arc::new(FakeCapturer::new()),
        Arc::new(FakeOcrEngine::with_lines(Vec::new())),
        perms.clone(),
        Some(history),
    );
    App::new(coord, perms, Preferences::default())
}

fn stored_record(
    store: &readshot_core::FsHistoryStore,
    id: readshot_core::Uuid,
) -> readshot_core::CaptureRecord {
    store
        .list()
        .unwrap()
        .into_iter()
        .find(|r| r.id == id)
        .unwrap()
}

fn recognized(
    app: &mut App,
    part: readshot_core::document::PartId,
    doc: readshot_core::text_lines::TextDocument,
) {
    let session = app.editor.as_ref().unwrap().session_id;
    let _ = update(
        app,
        Message::EditorTextRecognized {
            session,
            part,
            result: Ok(doc),
        },
    );
}

fn word_blur(rect: RectLike) -> Annotation {
    Annotation::Blur { rect, radius: 2.0 }
}

/// A History record scrubbed in an earlier session: the stored image
/// has `word_blur` baked in, its text isn't known yet, and a weak blur
/// leaves the word readable to OCR of the flattened image.
fn reopened_scrubbed(
    store: &readshot_core::FsHistoryStore,
) -> (image::RgbaImage, readshot_core::CaptureRecord) {
    let img = solid(30, 20);
    let mut record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 30, 20, "d");
    record.redacted = true;
    record.redactions = vec![word_blur(RectLike::new(2.0, 4.0, 10.0, 3.0))];
    record.redactions_complete = true;
    store
        .save(&record, &readshot_core::encode_png(&img).unwrap())
        .unwrap();
    (img, record)
}

fn hidden_word() -> String {
    ["sec", "ret"].concat()
}

fn ocr_line(
    text: &str,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) -> readshot_core::ocr_layout::RecognizedLine {
    readshot_core::ocr_layout::RecognizedLine {
        text: text.into(),
        x,
        y,
        w,
        h,
        words: Vec::new(),
    }
}

/// An app whose OCR reads `lines` (normalized boxes) in any image.
fn build_app_reading(
    history: Arc<dyn HistoryStore>,
    lines: Vec<readshot_core::ocr_layout::RecognizedLine>,
) -> App {
    let perms = Arc::new(FakePermissions::granted());
    let coord = CaptureCoordinator::new(
        Arc::new(FakeCapturer::new()),
        Arc::new(FakeOcrEngine::with_lines(lines)),
        perms.clone(),
        Some(history),
    );
    App::new(coord, perms, Preferences::default())
}

/// A record scrubbed by a build that kept no redactions: its image has
/// a weak blur over [`hidden_word`] (x 2–12, y 4–7) that nothing
/// records, and its text is masked.
fn scrubbed_without_redactions(
    store: &readshot_core::FsHistoryStore,
) -> (image::RgbaImage, readshot_core::CaptureRecord) {
    let img = solid(30, 20);
    let mut record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 30, 20, "d");
    record.redacted = true;
    record.ocr_text = Some("██████ visible".into());
    store
        .save(&record, &readshot_core::encode_png(&img).unwrap())
        .unwrap();
    (img, record)
}

fn crop_app_with_rect_selected() -> App {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(100, 80));
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    assert_eq!(ed.model.select_at(PointLike::new(5.0, 5.0)), Some(0));
    ed.crop_draft = Some(RectLike::new(10.0, 10.0, 40.0, 30.0));
    app.editor = Some(ed);
    app
}

fn two_part_editor() -> crate::editor::EditorSession {
    let mut ed = crate::editor::EditorSession::new(solid(40, 10));
    ed.model
        .add_part(solid(20, 10), readshot_core::document::Join::Below)
        .unwrap();
    ed.refresh_image();
    ed
}

/// An editor whose every resize / alignment / gap preview composes
/// off the UI thread.
fn async_compose(mut ed: crate::editor::EditorSession) -> crate::editor::EditorSession {
    ed.async_compose_min_pixels = 0;
    ed
}

/// The running compose's request, run here as its worker would.
fn finish_compose(app: &App) -> Message {
    let ed = app.editor.as_ref().unwrap();
    let job = ed.compose_job.clone().expect("a compose is running");
    compose_result(ed.session_id, &job)
}

fn compose_result(session: u64, job: &crate::editor::ComposeRequest) -> Message {
    Message::EditorComposed {
        session,
        seq: job.seq,
        output: crate::editor::ComposedOutput::new(Some(super::compose_jobs::compose_with(
            job.next.clone(),
            job.base.clone(),
        ))),
    }
}

/// Run every compose the editor is waiting on, as their workers
/// would, until none is left.
fn finish_all(app: &mut App) {
    for _ in 0..16 {
        if app
            .editor
            .as_ref()
            .is_none_or(|ed| ed.compose_job.is_none())
        {
            return;
        }
        let landed = finish_compose(app);
        let _ = update(app, landed);
    }
    panic!("composes never settle");
}

fn handle_id(ed: &crate::editor::EditorSession) -> iced::widget::image::Handle {
    ed.image_handle.clone()
}

fn rect_at(x: f32) -> Vec<readshot_core::Annotation> {
    vec![readshot_core::Annotation::Rectangle {
        rect: RectLike::new(x, 2.0, 6.0, 4.0),
        color: readshot_core::Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    }]
}

fn apply_resize_width(app: &mut App, width: &str) {
    let _ = update(app, Message::EditorResizeOpen);
    let _ = update(app, Message::EditorResizeWidthChanged(width.into()));
    let _ = update(app, Message::EditorResizeApply);
}

fn undo_msg() -> Message {
    Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo)
}

/// History-backed 40×20 editor with a Blur redaction at (4, 4, 10, 10).
fn redacted_history_editor() -> (tempfile::TempDir, Arc<readshot_core::FsHistoryStore>, App) {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
    let mut app = build_app_with_history(Arc::new(FakePermissions::granted()), store.clone());
    let img = solid(40, 20);
    let record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 40, 20, "d");
    store
        .save(&record, &readshot_core::encode_png(&img).unwrap())
        .unwrap();
    let mut ed = async_compose(crate::editor::EditorSession::from_history(img, record));
    ed.model.commit_annotation(Annotation::Blur {
        rect: RectLike::new(4.0, 4.0, 10.0, 10.0),
        radius: 4.0,
    });
    ed.model.clear_selection();
    app.editor = Some(ed);
    (dir, store, app)
}

fn compose_stored_record(
    store: &readshot_core::FsHistoryStore,
    app: &App,
) -> readshot_core::CaptureRecord {
    app.coordinator.flush_history_updates();
    let id = app
        .editor
        .as_ref()
        .and_then(|ed| ed.source_record.as_ref())
        .map(|r| r.id);
    store
        .list()
        .unwrap()
        .into_iter()
        .find(|r| Some(r.id) == id || id.is_none())
        .unwrap()
}

/// What `view()` does to the canvas frame: points the canvas sends
/// from now on are in the present frame.
fn built_a_view(app: &App) {
    app.editor.as_ref().unwrap().note_canvas_built();
}

/// Two-part editor with a gap drag opened at gap 10 (composing).
fn gap_drag_composing(app: &mut App) -> (usize, u64) {
    app.editor = Some(async_compose(two_part_editor()));
    let ed = app.editor.as_ref().unwrap();
    let (depth, session) = (ed.model.undo_depth(), ed.session_id);
    let _ = update(app, Message::EditorLayoutGapPreview(10));
    assert!(matches!(
        app.editor
            .as_ref()
            .unwrap()
            .compose_job
            .as_ref()
            .map(|j| j.kind),
        Some(crate::editor::ComposeKind::GapPreview(10))
    ));
    (depth, session)
}

const NO_DOC: PartBudget = PartBudget {
    doc_width: 0,
    doc_height: 0,
    gap: 0,
};

// ---- Task 9b: the combine's own selection flag ----

/// Run a scroll capture through its real start (an overlay selection
/// with the scroll intent), then end its session as a stitch does.
fn app_after_a_scroll_capture() -> App {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let overlay = window::Id::unique();
    app.windows.register(overlay, WindowKind::Overlay);
    app.overlay_displays.insert(
        overlay,
        crate::app::OverlayDisplay {
            display_id: "display-a".into(),
            scale: 2.0,
            origin_x: 0.0,
            origin_y: 0.0,
            width: 800.0,
            height: 600.0,
        },
    );
    app.pending_intent = Some(crate::app::CaptureIntent::ScrollCapture);
    let _ = update(
        &mut app,
        Message::OverlaySelected {
            display_id: "display-a".into(),
            rect: readshot_core::geom::Rect::from_xywh(10.0, 10.0, 200.0, 100.0).unwrap(),
            intent: crate::app::CaptureIntent::ScrollCapture,
        },
    );
    let s = app.scroll_session.take().expect("scroll session started");
    for id in [s.hud_window_id, s.region_window_id].into_iter().flatten() {
        app.windows.forget(id);
    }
    assert!(!app.windows.kinds().any(|k| k == WindowKind::Overlay));
    app
}

fn start_add_capture(app: &mut App) {
    app.editor = Some(crate::editor::EditorSession::new(solid(64, 32)));
    let _ = update(
        app,
        Message::EditorCombineCaptureRequested(readshot_core::document::Join::Below),
    );
    assert!(app.combine_capture.is_some(), "Add Capture started");
}

/// An Add Capture whose overlay selection is made (its capture in
/// flight), with the overlay window still registered. Returns the
/// editor session and the overlay id.
fn app_with_a_selected_combine() -> (App, u64, window::Id) {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    start_add_capture(&mut app);
    let session = app.editor.as_ref().unwrap().session_id;
    app.overlay_opening = false;
    let id = window::Id::unique();
    app.windows.register(id, WindowKind::Overlay);
    let _ = update(
        &mut app,
        Message::OverlaySelected {
            display_id: Default::default(),
            rect: readshot_core::geom::Rect::from_xywh(0.0, 0.0, 10.0, 10.0).unwrap(),
            intent: crate::app::CaptureIntent::CombineIntoEditor,
        },
    );
    assert!(app.combine_capture.is_some_and(|cc| cc.selected));
    (app, session, id)
}

/// The in-flight capture still lands in the same editor.
fn assert_the_combine_capture_lands(mut app: App, session: u64) {
    let _ = update(&mut app, Message::RegionCaptureCompleted(Ok(solid(64, 16))));
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.session_id, session);
    assert_eq!(ed.model.part_ids().len(), 2);
    assert!(app.combine_capture.is_none());
}

fn png_header_only(w: u32, h: u32) -> Vec<u8> {
    fn crc32(bytes: &[u8]) -> u32 {
        let mut c = 0xFFFF_FFFFu32;
        for &b in bytes {
            c ^= u32::from(b);
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
        }
        !c
    }
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut chunk = b"IHDR".to_vec();
    chunk.extend_from_slice(&w.to_be_bytes());
    chunk.extend_from_slice(&h.to_be_bytes());
    chunk.extend_from_slice(&[8, 6, 0, 0, 0]);
    let mut push = |kind: &[u8], data: &[u8]| {
        let mut chunk = kind.to_vec();
        chunk.extend_from_slice(data);
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(&chunk);
        out.extend_from_slice(&crc32(&chunk).to_be_bytes());
    };
    push(b"IHDR", &chunk[4..]);
    // A few bytes of (bogus) data: the header is all that's read.
    push(b"IDAT", &[0x78, 0x9C, 0x03, 0x00]);
    push(b"IEND", &[]);
    out
}

// ===== D1: no UI-thread compose or render at large sizes =====

/// A non-uniform image, so a turned or flipped document differs.
fn ramp(w: u32, h: u32) -> image::RgbaImage {
    image::RgbaImage::from_fn(w, h, |x, y| {
        image::Rgba([(x * 6) as u8, (y * 11) as u8, (x ^ y) as u8, 255])
    })
}

/// An async editor over `ramp(40, 20)` with one selected rectangle.
fn large_editor(app: &mut App) -> usize {
    let mut ed = sync_editor();
    ed.refresh_image();
    app.editor = Some(async_compose(ed));
    app.editor.as_ref().unwrap().model.undo_depth()
}

/// The same document built synchronously, for comparing outputs.
fn sync_editor() -> crate::editor::EditorSession {
    let mut ed = crate::editor::EditorSession::new(ramp(40, 20));
    ed.model.commit_annotation(rect_at(5.0).remove(0));
    ed
}

fn redo_msg() -> Message {
    Message::EditorToolbar(readshot_ui::ToolbarMessage::Redo)
}

fn job_kind(app: &App) -> Option<crate::editor::ComposeKind> {
    app.editor
        .as_ref()
        .unwrap()
        .compose_job
        .as_ref()
        .map(|j| j.kind)
}

/// Nothing composed or rendered on the UI thread since `before`.
fn composes(app: &App) -> (u64, u64) {
    (
        super::compose_jobs::composes_on_this_thread(),
        app.editor.as_ref().unwrap().model.base_composes(),
    )
}

// ===== D1 fix round 1 =====

fn key_press(window: iced::window::Id, key: iced::keyboard::Key, cmd: bool) -> Message {
    Message::EditorKeyPressed {
        window,
        key,
        modifiers: if cmd {
            command_modifiers()
        } else {
            iced::keyboard::Modifiers::default()
        },
        status_ignored: true,
    }
}

/// A magnifier over (7, 6) whose 2× bubble sits at (30, 10).
fn magnifier_at_7_6() -> Annotation {
    Annotation::Magnifier {
        source: PointLike::new(7.0, 6.0),
        radius: 3.0,
        bubble: PointLike::new(30.0, 10.0),
        zoom: 2.0,
        shape: readshot_core::MagnifierShape::Circle,
        color: readshot_core::Rgba::OPAQUE_BLACK,
    }
}

/// The 2× bubble of [`magnifier_at_7_6`], as the editor covers it.
fn bubble_of_magnifier_at_7_6() -> RectLike {
    RectLike::new(23.0, 3.0, 14.0, 14.0)
}

/// Theme, popovers, Beautify sliders, Esc, zoom and status toasts.
mod app_shell;
/// Overlay, scroll capture, window placement, permission and pins.
mod capture_flow;
/// Combine: adding captures and files, bakes.
mod combine_capture;
/// Tooltip and toggle casing over the view sources.
mod copy_conventions;
/// Editor text drafts, discard, output and editor keys.
mod editor_drafts;
/// Rotate, flip, resize and crop.
mod geometry_and_crop;
/// Sync and async geometry steps land in the same editor state.
mod geometry_pipeline;
/// History browser, statuses, friendly errors and platform commands.
mod history_browser;
/// History scrub after a redaction (B10).
mod history_scrub;
/// Key maps of the scroll HUD, welcome, History, pins and Settings.
mod key_maps;
/// Editor keyboard focus zones, Tab selection, keyboard crop and resize.
mod keyboard_focus;
/// Off-thread composes and renders (D1) and pending steps.
mod large_documents;
/// Magnifier drags and keyboard width bumps.
mod magnifier_tools;
/// Capture to disk, CLI, hotkeys, preferences, boot and settings.
mod settings_and_boot;
/// Style sliders and the eyedropper.
mod styles_and_eyedropper;
/// Text panel: recognition, copy, redact lines.
mod text_recognition;
/// History, Settings and Welcome keyboard paths and the Settings focus ring.
mod window_keys;
