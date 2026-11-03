//! Capture to disk, CLI, hotkeys, preferences, boot and settings.

use super::*;

#[tokio::test]
async fn capture_region_to_image_preserves_physical_pixel_dimensions() {
    struct HiDpiCapturer;

    #[async_trait]
    impl Capturer for HiDpiCapturer {
        async fn list_displays(&self) -> Result<Vec<DisplayInfo>, CaptureError> {
            Ok(vec![DisplayInfo {
                id: "retina".into(),
                bounds: readshot_core::geom::Rect::from_xywh(0.0, 0.0, 128.0, 128.0).unwrap(),
                scale: 2.0,
                name: "Retina".into(),
                is_primary: true,
            }])
        }

        async fn capture_region(
            &self,
            req: CaptureRequest,
        ) -> Result<image::RgbaImage, CaptureError> {
            let w = (req.rect.width() * req.scale).round() as u32;
            let h = (req.rect.height() * req.scale).round() as u32;
            Ok(solid(w, h))
        }
    }

    let perms = Arc::new(FakePermissions::granted());
    let coord = CaptureCoordinator::new(
        Arc::new(HiDpiCapturer),
        Arc::new(FakeOcrEngine::with_text("hi")),
        perms,
        None,
    );
    let img = capture_region_to_image(
        coord,
        "retina".into(),
        readshot_core::geom::Rect::from_xywh(0.0, 0.0, 128.0, 128.0).unwrap(),
        true,
    )
    .await
    .unwrap();
    assert_eq!(img.width(), 256);
    assert_eq!(img.height(), 256);
}

#[tokio::test]
async fn capture_region_to_image_forwards_cursor_visibility() {
    struct CursorFlagCapturer {
        seen_hide_cursor: Arc<Mutex<Option<bool>>>,
    }

    #[async_trait]
    impl Capturer for CursorFlagCapturer {
        async fn list_displays(&self) -> Result<Vec<DisplayInfo>, CaptureError> {
            Ok(vec![DisplayInfo {
                id: "primary".into(),
                bounds: readshot_core::geom::Rect::from_xywh(0.0, 0.0, 64.0, 64.0).unwrap(),
                scale: 1.0,
                name: "Primary".into(),
                is_primary: true,
            }])
        }

        async fn capture_region(
            &self,
            req: CaptureRequest,
        ) -> Result<image::RgbaImage, CaptureError> {
            *self.seen_hide_cursor.lock().unwrap() = Some(req.hide_cursor);
            Ok(solid(64, 64))
        }
    }

    let seen_hide_cursor = Arc::new(Mutex::new(None));
    let perms = Arc::new(FakePermissions::granted());
    let coord = CaptureCoordinator::new(
        Arc::new(CursorFlagCapturer {
            seen_hide_cursor: Arc::clone(&seen_hide_cursor),
        }),
        Arc::new(FakeOcrEngine::with_text("hi")),
        perms,
        None,
    );

    capture_region_to_image(
        coord,
        "primary".into(),
        readshot_core::geom::Rect::from_xywh(0.0, 0.0, 32.0, 32.0).unwrap(),
        false,
    )
    .await
    .unwrap();

    assert_eq!(*seen_hide_cursor.lock().unwrap(), Some(false));
}

#[tokio::test]
async fn capture_region_with_known_scale_skips_display_listing() {
    struct KnownScaleCapturer {
        seen_scale: Arc<Mutex<Option<f32>>>,
    }

    #[async_trait]
    impl Capturer for KnownScaleCapturer {
        async fn list_displays(&self) -> Result<Vec<DisplayInfo>, CaptureError> {
            panic!("known-scale capture should not list displays");
        }

        async fn capture_region(
            &self,
            req: CaptureRequest,
        ) -> Result<image::RgbaImage, CaptureError> {
            *self.seen_scale.lock().unwrap() = Some(req.scale);
            Ok(solid(64, 64))
        }
    }

    let seen_scale = Arc::new(Mutex::new(None));
    let perms = Arc::new(FakePermissions::granted());
    let coord = CaptureCoordinator::new(
        Arc::new(KnownScaleCapturer {
            seen_scale: Arc::clone(&seen_scale),
        }),
        Arc::new(FakeOcrEngine::with_text("hi")),
        perms,
        None,
    );

    capture_region_to_image_with_scale(
        coord,
        "secondary".into(),
        readshot_core::geom::Rect::from_xywh(0.0, 0.0, 32.0, 32.0).unwrap(),
        2.0,
        true,
    )
    .await
    .unwrap();

    assert_eq!(*seen_scale.lock().unwrap(), Some(2.0));
}

#[tokio::test]
async fn cli_interactive_write_removes_existing_tmp_file() {
    let dir = tempfile::TempDir::new().unwrap();
    let output = dir.path().join("capture.png");
    let tmp = dir.path().join("capture.png.tmp");
    std::fs::write(&tmp, b"stale").unwrap();

    write_cli_interactive_capture(solid(8, 8), output.clone())
        .await
        .unwrap();

    assert!(output.exists());
    assert!(!tmp.exists());
    let saved = image::open(output).unwrap();
    assert_eq!(saved.width(), 8);
    assert_eq!(saved.height(), 8);
}

#[cfg(target_os = "macos")]
#[test]
fn pretty_hotkey_renders_macos_glyphs() {
    assert_eq!(pretty_hotkey("cmd+shift+x"), "\u{21E7}\u{2318}X");
    // Apple order ⌃⌥⇧⌘: control then alt here.
    assert_eq!(pretty_hotkey("ctrl+alt+y"), "\u{2303}\u{2325}Y");
    assert_eq!(pretty_hotkey("CMD+Shift+Z"), "\u{21E7}\u{2318}Z");
}

#[cfg(not(target_os = "macos"))]
#[test]
fn pretty_hotkey_renders_pc_words() {
    // PC order: Ctrl, Shift, Alt, Super.
    assert_eq!(pretty_hotkey("ctrl+shift+x"), "Ctrl+Shift+X");
    assert_eq!(pretty_hotkey("ctrl+alt+y"), "Ctrl+Alt+Y");
}

#[test]
fn pretty_hotkey_falls_back_to_input_when_unparseable() {
    // Empty stays empty; nonsense single-token is preserved.
    assert_eq!(pretty_hotkey(""), "");
    assert_eq!(pretty_hotkey("???"), "???");
}

#[test]
fn only_the_capture_chord_is_registered_system_wide() {
    // History (⌘Y) and Settings (⌘,) are app shortcuts: registering them
    // with the OS would take ⌘Y / ⌘, (Redo on Windows, every app's
    // Preferences) away from every other app while Readshot runs.
    let prefs = Preferences {
        capture_hotkey: "ctrl+shift+x".into(),
        ..Preferences::default()
    };
    let mut handed_to_os = Vec::new();
    let HotkeyActions {
        actions,
        capture_registered,
        ..
    } = register_hotkey_actions(&prefs, |h| {
        handed_to_os.push(h);
        Ok(())
    });

    assert!(capture_registered);
    assert_eq!(handed_to_os.len(), 1);
    assert_eq!(
        actions.values().copied().collect::<Vec<_>>(),
        vec![GlobalHotkeyAction::Capture]
    );
    for fixed in [history_hotkey(), settings_hotkey()] {
        let id = readshot_ui::hotkey::parse(fixed)
            .unwrap()
            .to_global_hotkey()
            .id();
        assert!(!handed_to_os.iter().any(|h| h.id() == id), "{fixed}");
    }
}

#[test]
fn an_invalid_or_refused_capture_chord_registers_nothing() {
    let prefs = Preferences {
        capture_hotkey: "not a hotkey at all".into(),
        ..Preferences::default()
    };
    let HotkeyActions {
        actions,
        capture_registered,
        ..
    } = register_hotkey_actions(&prefs, |_| Ok(()));
    assert!(!capture_registered);
    assert!(actions.is_empty());

    let prefs = Preferences {
        capture_hotkey: "ctrl+shift+x".into(),
        ..Preferences::default()
    };
    let HotkeyActions {
        actions,
        capture_registered,
        ..
    } = register_hotkey_actions(&prefs, |_| Err("already owned".into()));
    assert!(!capture_registered);
    assert!(actions.is_empty());
}

#[test]
fn a_capture_chord_equal_to_an_app_shortcut_is_refused() {
    // ⌘, would capture from every window instead of opening Settings.
    let prefs = Preferences {
        capture_hotkey: settings_hotkey().into(),
        ..Preferences::default()
    };
    let attempts = std::cell::Cell::new(0);
    let HotkeyActions {
        actions,
        capture_registered,
        ..
    } = register_hotkey_actions(&prefs, |_| {
        attempts.set(attempts.get() + 1);
        Ok(())
    });

    assert!(!capture_registered, "conflicting capture must not register");
    assert!(actions.is_empty());
    assert_eq!(attempts.get(), 0);
    assert_eq!(capture_hotkey_conflict(settings_hotkey()), Some("Settings"));
    assert_eq!(capture_hotkey_conflict(history_hotkey()), Some("History"));
}

#[test]
fn ocr_request_uses_preferred_languages_from_preferences() {
    let prefs = Preferences {
        ocr_languages: vec!["ja-JP".into(), "en-US".into()],
        ..Preferences::default()
    };
    let img = image::RgbaImage::new(1, 1);

    let req = ocr_request_for_image(img, &prefs);

    assert_eq!(req.languages, vec!["ja-JP", "en-US"]);
    assert!(req.use_language_correction);
}

#[test]
fn ocr_request_keeps_empty_languages_for_automatic_detection() {
    let prefs = Preferences::default();
    let img = image::RgbaImage::new(1, 1);

    let req = ocr_request_for_image(img, &prefs);

    assert!(req.languages.is_empty());
}

#[test]
fn default_save_filename_expands_template_as_png() {
    let when = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();

    let filename = default_save_filename(
        "Capture {YYYY}-{timestamp}",
        when,
        readshot_core::ExportFormat::Png,
    );

    assert_eq!(filename, "Capture 2023-1700000000.png");
}

#[test]
fn default_save_filename_takes_the_default_format_extension() {
    let when = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
    for (format, ext) in [
        (readshot_core::ExportFormat::Png, "png"),
        (readshot_core::ExportFormat::Jpeg, "jpg"),
        (readshot_core::ExportFormat::Webp, "webp"),
    ] {
        assert_eq!(
            default_save_filename("Shot", when, format),
            format!("Shot.{ext}")
        );
    }
}

#[test]
fn default_save_filename_is_local_wall_clock_time() {
    // 2023-11-14 22:13:20 UTC is 2023-11-15 03:58:20 at UTC+05:45.
    let utc = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
    let nepal = chrono::FixedOffset::east_opt(5 * 3600 + 45 * 60).unwrap();
    assert_eq!(
        default_save_filename(
            "Screenshot {YYYY-MM-DD at HH.mm.ss}",
            utc.with_timezone(&nepal),
            readshot_core::ExportFormat::Png,
        ),
        "Screenshot 2023-11-15 at 03.58.20.png"
    );
}

#[test]
fn a_picked_save_path_keeps_a_known_extension_and_gains_the_default_otherwise() {
    use readshot_core::ExportFormat as F;
    let jpeg = F::Jpeg;
    // No extension (dots inside the name are not one): the default's.
    assert_eq!(
        resolve_save_path(PathBuf::from("/t/Screenshot at 10.20.30"), jpeg),
        (PathBuf::from("/t/Screenshot at 10.20.30.jpg"), F::Jpeg)
    );
    // A typed extension of another format is the user's choice.
    assert_eq!(
        resolve_save_path(PathBuf::from("/t/a.PNG"), jpeg),
        (PathBuf::from("/t/a.PNG"), F::Png)
    );
    assert_eq!(
        resolve_save_path(PathBuf::from("/t/a.jpeg"), F::Webp),
        (PathBuf::from("/t/a.jpeg"), F::Jpeg)
    );
    // An unknown extension stays in the name; the default's is added.
    assert_eq!(
        resolve_save_path(PathBuf::from("/t/a.txt"), F::Webp),
        (PathBuf::from("/t/a.txt.webp"), F::Webp)
    );
}

#[test]
fn a_picked_save_writes_the_resolved_format() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = write_picked_save(
        &solid(8, 8),
        dir.path().join("shot"),
        readshot_core::ExportFormat::Webp,
    )
    .unwrap();
    assert_eq!(path, dir.path().join("shot.webp"));
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(
        image::guess_format(&bytes).unwrap(),
        image::ImageFormat::WebP
    );
}

#[test]
fn editor_output_image_bakes_annotations_and_selected_frame() {
    let mut ed = crate::editor::EditorSession::new(solid(32, 32));
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(6.0, 6.0, 20.0, 20.0),
        color: Rgba::new(1.0, 0.0, 0.0, 1.0),
        line_width: 4.0,
        style: Default::default(),
    });
    ed.model
        .set_beautify(readshot_core::beautify::BuiltinPreset::Minimal.settings());

    let output = editor_output_image(&mut ed).unwrap();

    assert_eq!((output.width(), output.height()), (80, 80));
    assert_eq!(*output.get_pixel(0, 0), image::Rgba([255, 255, 255, 255]));
    let annotated = *output.get_pixel(24 + 6, 24 + 6);
    assert!(
        annotated[0] > 180 && annotated[1] < 80 && annotated[2] < 80,
        "expected visible red annotation pixel, got {annotated:?}"
    );
}

/// A 16:9 canvas on a tall document: 100 × 20000 with 200 px padding
/// would be 36267 × 20400, over the 32767 px side limit.
fn app_with_oversized_beautify() -> (App, iced::window::Id) {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(100, 20_000));
    let id = iced::window::Id::unique();
    ed.window_id = Some(id);
    app.windows.register(id, WindowKind::Editor);
    let mut s = readshot_core::beautify::BuiltinPreset::Minimal.settings();
    s.padding = readshot_core::beautify::MAX_PADDING;
    s.canvas = readshot_core::beautify::CanvasSize::Wide;
    ed.model.set_beautify(s);
    app.editor = Some(ed);
    (app, id)
}

#[test]
fn a_beautify_canvas_over_the_limits_is_refused_not_rendered() {
    let (mut app, id) = app_with_oversized_beautify();
    let ed = app.editor.as_ref().unwrap();
    let reason = super::super::beautify_panel::beautify_output_refusal(ed)
        .expect("the panel says why the canvas can't be made");
    assert!(reason.contains("36,267"), "{reason}");
    assert!(reason.contains("32,767"), "{reason}");
    assert!(super::super::view::editor_size_label(ed).contains("too large"));
    assert!(editor_output_image(app.editor.as_mut().unwrap()).is_err());

    for (message, verb) in [
        (Message::Editor(EditorMsg::SaveRequested), "save"),
        (Message::Editor(EditorMsg::CopyImageRequested), "copy"),
        (Message::Editor(EditorMsg::PinRequested), "pin"),
    ] {
        let _ = update(&mut app, message);
        let ed = app.editor.as_ref().expect("the editor stays open");
        assert!(!ed.busy, "{verb}: nothing started");
        let status = ed.status_text().map(str::to_owned).unwrap_or_default();
        assert!(
            status.starts_with(&format!("Couldn’t {verb}.")) && status.contains("32,767"),
            "{verb}: {status}"
        );
    }
    assert!(app.pins.is_empty(), "no pin opened");
    assert_eq!(app.windows.kind(id), Some(WindowKind::Editor));

    // Back to Auto: the same document outputs normally.
    let ed = app.editor.as_mut().unwrap();
    let mut s = ed.model.beautify();
    s.canvas = readshot_core::beautify::CanvasSize::Auto;
    ed.model.set_beautify(s);
    assert!(super::super::beautify_panel::beautify_output_refusal(ed).is_none());
    let out = editor_output_image(ed).unwrap();
    assert_eq!(out.dimensions(), (500, 20_400));
}

#[test]
fn preferred_save_seed_uses_recent_directory_before_saved_preference() {
    let prefs = Preferences {
        save_folder: PathBuf::from("/configured"),
        ..Preferences::default()
    };
    let last = Some(PathBuf::from("/recent"));

    let seed = preferred_save_seed_dir(&prefs, &last);

    assert_eq!(seed, Some(PathBuf::from("/recent")));
}

#[test]
fn preferred_save_seed_falls_back_to_configured_folder() {
    let prefs = Preferences {
        save_folder: PathBuf::from("/configured"),
        ..Preferences::default()
    };

    let seed = preferred_save_seed_dir(&prefs, &None);

    assert_eq!(seed, Some(PathBuf::from("/configured")));
}

#[test]
fn preferred_save_seed_ignores_empty_configured_folder() {
    let prefs = Preferences::default();

    let seed = preferred_save_seed_dir(&prefs, &None);

    assert_eq!(seed, None);
}

#[test]
fn settings_save_folder_to_open_prefers_configured_folder() {
    let prefs = Preferences {
        save_folder: PathBuf::from("/configured"),
        ..Preferences::default()
    };

    assert_eq!(
        settings_save_folder_to_open(&prefs),
        PathBuf::from("/configured")
    );
}

#[test]
fn shortcut_recorder_requires_a_modifier() {
    let shortcut = shortcut_string_from_keypress(
        &iced::keyboard::Key::Character("x".into()),
        iced::keyboard::Modifiers::default(),
    );

    assert_eq!(shortcut, None);
}

// LOGO is ⌘ on macOS (recorded "cmd"); off macOS see the tests below.
#[cfg(target_os = "macos")]
#[test]
fn shortcut_recorder_formats_command_shift_character() {
    let mut modifiers = iced::keyboard::Modifiers::default();
    modifiers.insert(iced::keyboard::Modifiers::SHIFT);
    modifiers.insert(iced::keyboard::Modifiers::LOGO);

    let shortcut =
        shortcut_string_from_keypress(&iced::keyboard::Key::Character("X".into()), modifiers);

    assert_eq!(shortcut.as_deref(), Some("cmd+shift+x"));
}

/// The recorder saves exactly the keys pressed, on every OS: Ctrl is
/// "ctrl" (never "cmd+ctrl", which registers Super+Ctrl), and the
/// Windows / Super key is recorded and registered as itself.
#[test]
fn shortcut_recorder_saves_exactly_the_modifiers_pressed() {
    use iced::keyboard::Modifiers as M;
    let x = iced::keyboard::Key::Character("X".into());
    let recorded = |m: M| shortcut_string_from_keypress(&x, m).unwrap();
    let ctrl_shift = recorded(M::CTRL | M::SHIFT);
    assert_eq!(ctrl_shift, "ctrl+shift+x");
    let spec = readshot_ui::hotkey::parse(&ctrl_shift).unwrap();
    assert!(spec.modifiers.control && spec.modifiers.shift);
    assert!(!spec.modifiers.meta, "no Super/⌘ the user didn't press");
    let logo = recorded(M::LOGO | M::SHIFT);
    let spec = readshot_ui::hotkey::parse(&logo).unwrap();
    assert!(spec.modifiers.meta && spec.modifiers.shift && !spec.modifiers.control);
    assert_eq!(recorded(M::ALT | M::CTRL), "ctrl+alt+x");
}

#[cfg(not(target_os = "macos"))]
#[test]
fn shortcut_recorder_off_macos_records_ctrl_and_super_by_name() {
    use iced::keyboard::Modifiers as M;
    let x = iced::keyboard::Key::Character("X".into());
    assert_eq!(
        shortcut_string_from_keypress(&x, M::CTRL | M::SHIFT).as_deref(),
        Some("ctrl+shift+x")
    );
    assert_eq!(
        shortcut_string_from_keypress(&x, M::LOGO | M::SHIFT).as_deref(),
        Some("super+shift+x")
    );
    assert_eq!(
        shortcut_string_from_keypress(&x, M::LOGO | M::CTRL).as_deref(),
        Some("super+ctrl+x")
    );
}

#[test]
fn shortcut_recorder_cancels_on_plain_escape_only() {
    let escape = iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape);
    assert!(shortcut_cancelled_by_keypress(
        &escape,
        iced::keyboard::Modifiers::default()
    ));

    let mut modifiers = iced::keyboard::Modifiers::default();
    modifiers.insert(iced::keyboard::Modifiers::SHIFT);
    assert!(!shortcut_cancelled_by_keypress(&escape, modifiers));
}

#[test]
fn app_window_shortcuts_open_history_and_settings() {
    let mut modifiers = iced::keyboard::Modifiers::default();
    modifiers.insert(iced::keyboard::Modifiers::COMMAND);

    assert!(matches!(
        app_window_shortcut_message(iced::keyboard::Key::Character("y".into()), modifiers),
        Some(Message::History(HistoryMsg::OpenRequested))
    ));
    assert!(matches!(
        app_window_shortcut_message(iced::keyboard::Key::Character(",".into()), modifiers),
        Some(Message::Settings(SettingsMsg::OpenRequested))
    ));
}

#[test]
fn app_window_shortcuts_ignore_extra_modifiers() {
    let mut modifiers = iced::keyboard::Modifiers::default();
    modifiers.insert(iced::keyboard::Modifiers::COMMAND);
    modifiers.insert(iced::keyboard::Modifiers::SHIFT);

    assert!(
        app_window_shortcut_message(iced::keyboard::Key::Character("y".into()), modifiers)
            .is_none()
    );
}

#[test]
fn global_hotkey_action_ignores_release_events() {
    let mut actions = HashMap::new();
    actions.insert(7, GlobalHotkeyAction::Capture);
    let event = GlobalHotKeyEvent {
        id: 7,
        state: HotKeyState::Released,
    };

    assert_eq!(global_hotkey_action(&event, &actions), None);
}

#[test]
fn global_hotkey_action_maps_pressed_ids() {
    let mut actions = HashMap::new();
    actions.insert(7, GlobalHotkeyAction::Capture);
    let event = GlobalHotKeyEvent {
        id: 7,
        state: HotKeyState::Pressed,
    };

    assert_eq!(
        global_hotkey_action(&event, &actions),
        Some(GlobalHotkeyAction::Capture)
    );
}

#[test]
fn hotkey_tick_no_events_is_noop() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    // No manager registered → tick does nothing meaningful.
    let _ = update(&mut app, Message::Tick(TickMsg::Hotkey));
    assert!(!app.capture_in_flight);
}

#[test]
fn url_tick_drains_delivered_url_actions() {
    let _lock = crate::url_events::lock_for_tests();
    crate::url_events::clear_for_tests();
    crate::url_events::deliver_url_string("readshot://new").unwrap();
    let mut app = build_app(Arc::new(FakePermissions::granted()));

    let _ = update(&mut app, Message::Tick(TickMsg::Url));

    assert_eq!(crate::url_events::drain_actions(), Vec::new());
}

#[test]
fn load_preferences_first_launch_when_path_is_none() {
    let prefs = load_preferences(None);
    // First-launch override: history on (Last50) so the wedge
    // works the first time the user takes a screenshot.
    assert_eq!(
        prefs.history_retention,
        readshot_core::HistoryRetention::Last50,
    );
    assert_eq!(prefs.capture_hotkey, default_capture_hotkey());
    assert!(!prefs.onboarding_completed);
}

#[test]
fn load_preferences_first_launch_when_file_missing() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("preferences.toml");
    assert!(!path.exists());
    let prefs = load_preferences(Some(&path));
    assert_eq!(
        prefs.history_retention,
        readshot_core::HistoryRetention::Last50,
    );
    assert!(!prefs.onboarding_completed);
}

#[test]
fn load_preferences_round_trips_an_existing_file() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("preferences.toml");
    // User explicitly turned history off — that must win over the
    // first-launch override.
    let on_disk = Preferences {
        history_retention: readshot_core::HistoryRetention::Off,
        capture_hotkey: "ctrl+alt+9".into(),
        ..Preferences::default()
    };
    on_disk.save(&path).unwrap();

    let loaded = load_preferences(Some(&path));
    assert_eq!(
        loaded.history_retention,
        readshot_core::HistoryRetention::Off,
    );
    assert_eq!(loaded.capture_hotkey, "ctrl+alt+9");
}

#[test]
fn load_preferences_migrates_existing_users_as_onboarded() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("preferences.toml");
    std::fs::write(
        &path,
        "schema_version = 2\ncapture_hotkey = \"ctrl+alt+9\"\n",
    )
    .unwrap();

    let loaded = load_preferences(Some(&path));

    assert!(loaded.onboarding_completed);
    assert_eq!(loaded.capture_hotkey, "ctrl+alt+9");
}

#[test]
fn boot_opens_ready_window_for_manual_granted_launch() {
    assert!(needs_welcome_window_on_boot(
        WelcomeState::Granted,
        false,
        false
    ));
}

#[test]
fn boot_skips_ready_window_for_login_launch_when_granted() {
    // The whole point of the fix: a login launch settles into the
    // menu bar instead of popping the ready window every boot.
    assert!(!needs_welcome_window_on_boot(
        WelcomeState::Granted,
        false,
        true
    ));
}

#[test]
fn boot_still_opens_permission_window_for_login_launch_when_blocked() {
    // Permission still missing → the gate must show even at login,
    // otherwise the user has no way to grant access.
    assert!(needs_welcome_window_on_boot(
        WelcomeState::Pending,
        false,
        true
    ));
}

#[test]
fn boot_skips_ready_window_for_url_launch_when_granted() {
    assert!(!needs_welcome_window_on_boot(
        WelcomeState::Granted,
        true,
        false
    ));
}

#[test]
fn boot_still_opens_permission_window_for_url_launch_when_blocked() {
    assert!(needs_welcome_window_on_boot(
        WelcomeState::Pending,
        true,
        false
    ));
}

#[test]
fn load_preferences_falls_back_when_file_is_corrupt() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("preferences.toml");
    let bytes = b"this is not [valid toml";
    std::fs::write(&path, bytes).unwrap();
    let prefs = load_preferences(Some(&path));
    // Corrupt file → no panic, and never a pruning retention: the
    // user's real setting is unknown, so History must not be cut to 50.
    assert_eq!(
        prefs.history_retention,
        readshot_core::HistoryRetention::Off
    );
    // The original bytes were moved aside, intact, not overwritten.
    assert!(!path.exists());
    let quarantined: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("preferences.toml.corrupt-")
        })
        .collect();
    assert_eq!(quarantined.len(), 1);
    assert_eq!(std::fs::read(quarantined[0].path()).unwrap(), bytes);
}

#[test]
fn update_settings_hotkey_change_persists_value_when_reregister_fails() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    assert!(app.hotkey_manager.is_none());
    // Garbage input — register_default_hotkey can't parse it,
    // capture re-registration fails, but the user's preference
    // still gets saved in-memory so they can fix the typo and try
    // again. Fixed app hotkeys may still be registered.
    let _ = update(
        &mut app,
        Message::Settings(SettingsMsg::Edit(SettingsMessage::SetCaptureHotkey(
            "not-a-real-chord".into(),
        ))),
    );
    assert_eq!(app.preferences.capture_hotkey, "not-a-real-chord");
    assert!(!app.capture_hotkey_registered);
    assert_eq!(
        app.settings_hotkey_error.as_deref(),
        Some("Couldn’t read that shortcut. Press Record and try another."),
    );
}

#[test]
fn update_settings_non_hotkey_does_not_re_register() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    // Non-hotkey settings (e.g. debug logging) must NOT churn the
    // hotkey registration — that path only fires for hotkey
    // changes specifically.
    let _ = update(
        &mut app,
        Message::Settings(SettingsMsg::Edit(SettingsMessage::SetDebugLogging(true))),
    );
    assert!(app.preferences.debug_logging);
    // Manager stays None because we never tried to register, and
    // the test build had nothing registered to begin with.
    assert!(app.hotkey_manager.is_none());
}

#[test]
fn window_closed_clears_settings_window_id() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let id = iced::window::Id::unique();
    app.windows.register(id, WindowKind::Settings);
    app.settings_window_id = Some(id);
    app.settings_recording_hotkey = true;
    app.settings_hotkey_error = Some("bad shortcut".into());
    let _ = update(&mut app, Message::Window(WindowMsg::Closed(id)));
    assert!(app.settings_window_id.is_none());
    assert!(!app.settings_recording_hotkey);
    assert!(app.settings_hotkey_error.is_none());
    assert!(app.windows.kind(id).is_none());
}

#[test]
fn window_closed_clears_editor_session() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let id = iced::window::Id::unique();
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.window_id = Some(id);
    app.windows.register(id, WindowKind::Editor);
    app.editor = Some(ed);

    let _ = update(&mut app, Message::Window(WindowMsg::Closed(id)));

    assert!(app.editor.is_none());
    assert!(app.windows.kind(id).is_none());
}

#[test]
fn window_closed_commits_pending_history_text_before_dropping_editor() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(FsHistoryStore::new(dir.path().join("history")));
    let record =
        readshot_core::CaptureRecord::new(chrono::Utc::now(), 100, 100, "primary".to_string());
    store.save(&record, b"png").unwrap();

    let history: Arc<dyn HistoryStore> = store.clone();
    let mut app = build_app_with_history(Arc::new(FakePermissions::granted()), history);
    let id = iced::window::Id::unique();
    let mut ed = crate::editor::EditorSession::from_history(solid(100, 100), record);
    ed.window_id = Some(id);
    ed.pending_text = Some(crate::editor::PendingText {
        origin: PointLike::new(12.0, 18.0),
        content: "window close draft".into(),
        edit_index: None,
    });
    app.windows.register(id, WindowKind::Editor);
    app.editor = Some(ed);

    let _ = update(&mut app, Message::Window(WindowMsg::Closed(id)));

    assert!(app.editor.is_none());
    assert!(app.windows.kind(id).is_none());
    // The sidecar write runs on the coordinator's background
    // writer; wait for it before reading the store.
    app.coordinator.flush_history_updates();
    let from_disk = store.list().unwrap();
    assert_eq!(from_disk[0].annotation_model.len(), 1);
    match &from_disk[0].annotation_model[0] {
        Annotation::Text {
            content, origin, ..
        } => {
            assert_eq!(content, "window close draft");
            assert_eq!(*origin, PointLike::new(12.0, 18.0));
        }
        other => panic!("expected text annotation, got {other:?}"),
    }
}

#[test]
fn hotkey_problems_are_flagged_as_errors_not_help() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.capture_hotkey_registered = true;
    app.settings_hotkey_error = None;
    app.settings_hotkey_status = None;
    assert!(!capture_hotkey_hint_is_error(&app));
    app.capture_hotkey_registered = false;
    assert!(capture_hotkey_hint_is_error(&app));
    app.capture_hotkey_registered = true;
    app.settings_hotkey_error = Some("reserved".into());
    assert!(capture_hotkey_hint_is_error(&app));
    app.settings_hotkey_error = None;
    app.settings_hotkey_status = Some("⇧⌘X is ready.".into());
    assert!(!capture_hotkey_hint_is_error(&app));
}

#[test]
fn shortcut_hints_name_a_way_back_and_recording_says_it_once() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.settings_recording_hotkey = true;
    let hint = crate::runtime::view::capture_hotkey_hint(&app);
    // The recording instruction lives under the control only.
    assert!(!hint.to_lowercase().contains("esc"), "{hint}");
    assert!(!hint.to_lowercase().contains("press"), "{hint}");
    app.settings_recording_hotkey = false;
    app.preferences.capture_hotkey = "cmd+shift+x".into();
    app.capture_hotkey_registered = false;
    let hint = crate::runtime::view::capture_hotkey_hint(&app);
    assert!(hint.ends_with("Choose a different shortcut."), "{hint}");
    assert!(
        !hint.contains("chord") && !hint.contains("hotkey"),
        "{hint}"
    );
    app.preferences.capture_hotkey = "nonsense".into();
    let hint = crate::runtime::view::capture_hotkey_hint(&app);
    assert!(!hint.contains('`'), "{hint}");
    assert!(
        hint.ends_with("Press Record to choose a new one."),
        "{hint}"
    );
}

#[test]
fn recovery_actions_stack_rather_than_clip_at_the_window_minimums() {
    use crate::runtime::view::{
        permission_actions_stack, welcome_footer_stacks, PERMISSION_ACTIONS_ROW_MIN,
        WELCOME_FOOTER_ROW_MIN,
    };
    // Settings at its 520 px minimum: 520 − 200 sidebar − 2 × 22 pane
    // padding − 2 × 14 group padding ≈ 248 px for the permission row.
    let settings_min_pane = 520.0 - 200.0 - 44.0 - 28.0;
    assert!(permission_actions_stack(settings_min_pane));
    // Three actions ≈ 80 + 135 + 160 + 2 × 8 px.
    const { assert!(PERMISSION_ACTIONS_ROW_MIN >= 80.0 + 135.0 + 160.0 + 16.0) };
    assert!(!permission_actions_stack(PERMISSION_ACTIONS_ROW_MIN));
    // Welcome at its 420 px minimum leaves ≈ 340 px; the footer needs ≈ 414.
    assert!(welcome_footer_stacks(420.0 - 80.0));
    const { assert!(WELCOME_FOOTER_ROW_MIN >= 414.0) };
}

/// Pin with a Beautify canvas: the editor closes at once and the canvas
/// renders off the UI thread; the pin opens when it lands. Without
/// Beautify the pin opens at once.
#[test]
fn a_beautified_pin_renders_off_the_ui_thread() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(32, 32));
    ed.model
        .set_beautify(readshot_core::beautify::BuiltinPreset::Minimal.settings());
    app.editor = Some(ed);
    let _ = update(&mut app, Message::Editor(EditorMsg::PinRequested));
    assert!(app.editor.is_none(), "the editor closed");
    assert!(app.pins.is_empty(), "no pin until the render lands");
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Pin(PinMsg::Rendered {
            result: Ok(solid(80, 80)),
            display_bounds: None,
        })),
    );
    assert_eq!(app.pins.len(), 1);

    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(32, 32)));
    let _ = update(&mut app, Message::Editor(EditorMsg::PinRequested));
    assert_eq!(app.pins.len(), 1, "no Beautify: the pin opens at once");
}

// ---- Window-capture shortcut ----

#[test]
fn the_window_capture_shortcut_is_unassigned_by_default() {
    let prefs = Preferences::default();
    assert_eq!(prefs.window_capture_hotkey, "");
    let HotkeyActions {
        actions,
        window_registered,
        ..
    } = register_hotkey_actions(&prefs, |_| Ok(()));
    assert!(!window_registered);
    assert!(!actions
        .values()
        .any(|a| *a == GlobalHotkeyAction::CaptureWindow));
}

#[test]
fn a_set_window_capture_shortcut_is_registered_system_wide() {
    let prefs = Preferences {
        capture_hotkey: "ctrl+shift+x".into(),
        window_capture_hotkey: "ctrl+shift+w".into(),
        ..Preferences::default()
    };
    let mut handed = 0;
    let HotkeyActions {
        actions,
        capture_registered,
        window_registered,
    } = register_hotkey_actions(&prefs, |_| {
        handed += 1;
        Ok(())
    });
    // Only where window capture exists (the Settings row's condition):
    // elsewhere a chord left in the file must not take a key system-wide.
    let supported = crate::tray::WINDOW_CAPTURE_IN_MENU;
    assert!(capture_registered);
    assert_eq!(window_registered, supported);
    assert_eq!(handed, if supported { 2 } else { 1 });
    assert_eq!(
        actions
            .values()
            .any(|a| *a == GlobalHotkeyAction::CaptureWindow),
        supported
    );
}

#[test]
fn recording_the_window_chord_as_the_capture_shortcut_is_refused() {
    let (mut app, _) = settings_app();
    app.preferences.window_capture_hotkey = "ctrl+alt+w".into();
    let capture = app.preferences.capture_hotkey.clone();
    let _ = update(
        &mut app,
        Message::Settings(SettingsMsg::StartHotkeyRecording),
    );
    let _ = update(
        &mut app,
        Message::Settings(SettingsMsg::HotkeyRecorded("ctrl+alt+w".into())),
    );
    assert_eq!(app.preferences.capture_hotkey, capture, "kept");
    assert_eq!(app.preferences.window_capture_hotkey, "ctrl+alt+w");
    let error = app.settings_hotkey_error.clone().unwrap_or_default();
    assert!(error.contains("window capture shortcut"), "{error}");
}

#[test]
fn a_window_shortcut_equal_to_another_shortcut_is_refused() {
    for clash in ["ctrl+shift+x", settings_hotkey()] {
        let prefs = Preferences {
            capture_hotkey: "ctrl+shift+x".into(),
            window_capture_hotkey: clash.into(),
            ..Preferences::default()
        };
        let HotkeyActions {
            window_registered,
            capture_registered,
            ..
        } = register_hotkey_actions(&prefs, |_| Ok(()));
        assert!(!window_registered, "{clash}");
        assert!(capture_registered, "the capture chord keeps working");
        assert!(window_hotkey_conflict(&prefs).is_some(), "{clash}");
    }
}

#[test]
fn recording_the_window_shortcut_sets_it_and_leaves_the_capture_one() {
    let (mut app, _) = settings_app();
    let capture = app.preferences.capture_hotkey.clone();
    let _ = update(
        &mut app,
        Message::Settings(SettingsMsg::StartWindowHotkeyRecording),
    );
    assert!(app.settings_recording_hotkey);
    assert_eq!(
        app.settings_recording_target,
        crate::app::HotkeyTarget::WindowCapture
    );
    let _ = update(
        &mut app,
        Message::Settings(SettingsMsg::HotkeyRecorded("ctrl+alt+w".into())),
    );
    assert!(!app.settings_recording_hotkey);
    assert_eq!(app.preferences.window_capture_hotkey, "ctrl+alt+w");
    assert_eq!(app.preferences.capture_hotkey, capture);
}

#[test]
fn recording_the_capture_chord_as_the_window_shortcut_is_refused() {
    let (mut app, _) = settings_app();
    let capture = app.preferences.capture_hotkey.clone();
    let _ = update(
        &mut app,
        Message::Settings(SettingsMsg::StartWindowHotkeyRecording),
    );
    let _ = update(
        &mut app,
        Message::Settings(SettingsMsg::HotkeyRecorded(capture.clone())),
    );
    assert_eq!(app.preferences.window_capture_hotkey, "");
    let (hint, is_error) = crate::runtime::view::window_hotkey_hint(&app);
    assert!(is_error, "{hint}");
    assert!(hint.contains("capture shortcut"), "{hint}");
}

#[test]
fn clearing_and_resetting_unassign_the_window_shortcut() {
    let (mut app, _) = settings_app();
    app.preferences.window_capture_hotkey = "ctrl+alt+w".into();
    let _ = update(
        &mut app,
        Message::Settings(SettingsMsg::Edit(SettingsMessage::SetWindowCaptureHotkey(
            String::new(),
        ))),
    );
    assert_eq!(app.preferences.window_capture_hotkey, "");
    let (hint, is_error) = crate::runtime::view::window_hotkey_hint(&app);
    assert!(!is_error);
    assert!(hint.starts_with("Not set."), "{hint}");

    app.preferences.window_capture_hotkey = "ctrl+alt+w".into();
    let _ = update(&mut app, Message::Settings(SettingsMsg::ResetAllRequested));
    let _ = update(&mut app, Message::Settings(SettingsMsg::ResetAllConfirmed));
    assert_eq!(app.preferences.window_capture_hotkey, "");
}

/// An app left uninstalled (Windows, Linux, a plain binary: the
/// program stays) after a purge. Its data and preferences dirs are gone.
fn uninstalled_app(purge: bool) -> (tempfile::TempDir, App) {
    let dir = tempfile::TempDir::new().unwrap();
    let history_root = dir.path().join("data").join("history");
    let store: Arc<dyn HistoryStore> = Arc::new(FsHistoryStore::new(history_root));
    let mut app = build_app_with_history(Arc::new(FakePermissions::granted()), store);
    app.preferences_path = Some(dir.path().join("config").join("preferences.toml"));
    app.preferences.history_retention = readshot_core::HistoryRetention::Last50;
    app.preferences.launch_at_login = true;
    app.hotkey_actions.insert(7, GlobalHotkeyAction::Capture);
    app.settings_window_id = Some(iced::window::Id::unique());
    app.settings_uninstall_pending = true;
    let _ = update(
        &mut app,
        Message::Settings(SettingsMsg::UninstallFinished {
            program_removed: false,
            purge,
            failed: Vec::new(),
        }),
    );
    // The History writer's launch-time sweep has run (in the app it ran
    // at launch, long before any uninstall), then what the executor
    // removed under purge is gone.
    app.coordinator.flush_history_updates();
    if purge {
        let _ = std::fs::remove_dir_all(dir.path().join("data"));
        let _ = std::fs::remove_dir_all(dir.path().join("config"));
    }
    (dir, app)
}

/// W5a S1: a Settings uninstall whose steps failed says so, in the
/// error tone, naming each failed step, and never claims everything was
/// removed. Even when the bundle went to the Trash, Readshot stays up to
/// say it instead of quitting.
#[test]
fn a_settings_uninstall_with_failed_steps_names_them() {
    for program_removed in [false, true] {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.settings_window_id = Some(iced::window::Id::unique());
        let _ = update(
            &mut app,
            Message::Settings(SettingsMsg::UninstallFinished {
                program_removed,
                purge: false,
                failed: vec!["Remove the Open at Login entry, if there is one".into()],
            }),
        );
        let said = app.settings_uninstall_result.as_ref().expect("said");
        assert_eq!(said.tone, StatusTone::Error, "{program_removed}");
        assert!(said.text.contains("Open at Login entry"), "{}", said.text);
        assert!(said.text.contains("1 step"), "{}", said.text);
        assert!(!said.text.contains("were removed"), "{}", said.text);
        // W5d review m7: advice the inert pane can't follow is not
        // given; the next step is outside this process.
        assert!(!said.text.starts_with("Run Uninstall"), "{}", said.text);
        if program_removed {
            assert!(said.text.contains("Trash"), "{}", said.text);
            assert!(!said.text.contains("readshot uninstall"), "{}", said.text);
        } else {
            assert!(said.text.contains("Quit Readshot"), "{}", said.text);
            assert!(said.text.contains("readshot uninstall"), "{}", said.text);
        }
        assert!(app.uninstalled);
    }
}

/// The completion is said under the Uninstall row, toned, with a way
/// to quit, never as another row's help text.
#[test]
fn a_settings_uninstall_that_leaves_the_program_says_what_is_left() {
    let (_dir, app) = uninstalled_app(false);
    let said = app.settings_uninstall_result.as_ref().expect("said");
    assert!(said.text.contains("still installed"), "{}", said.text);
    assert_eq!(said.tone, StatusTone::Warning);
    assert!(app.settings_status.is_none(), "not under another row");
    assert!(!app.settings_uninstall_pending);
}

/// After it, Readshot is inert until it quits: no shortcut, no tray
/// action but Quit, and no capture, History or preferences write can
/// bring back what the uninstall removed.
#[test]
fn an_uninstalled_readshot_recreates_nothing_it_removed() {
    let (dir, mut app) = uninstalled_app(true);
    assert!(app.uninstalled);
    assert!(app.hotkey_actions.is_empty(), "shortcuts unregistered");
    assert!(app.hotkey_manager.is_none());
    assert!(!app.preferences.launch_at_login, "in memory only");
    assert!(
        !dir.path().join("config").exists(),
        "no preferences written"
    );

    // A capture landing now opens nothing and saves nothing.
    let task = update(
        &mut app,
        Message::Capture(CaptureMsg::RegionCaptured(Ok(solid(8, 8)))),
    );
    assert_eq!(task.units(), 0);
    assert!(app.editor.is_none());
    // Tray captures and shortcuts do nothing.
    for action in [
        crate::tray::TrayAction::Capture,
        crate::tray::TrayAction::CaptureWindow,
        crate::tray::TrayAction::ScrollCapture,
        crate::tray::TrayAction::History,
    ] {
        let _ = update(&mut app, Message::Tick(TickMsg::TrayAction(action)));
        assert!(!app.overlay_opening, "{action:?}");
        assert!(app.history_window_id.is_none(), "{action:?}");
    }
    let _ = crate::runtime::tick_ops::run_hotkey_actions(&mut app, &[GlobalHotkeyAction::Capture]);
    assert!(!app.overlay_opening);
    // Settings edits don't write preferences, and Open at Login can't
    // be turned back on (which would re-register the autostart entry).
    let _ = update(
        &mut app,
        Message::Settings(SettingsMsg::Edit(SettingsMessage::SetDebugLogging(true))),
    );
    let _ = update(
        &mut app,
        Message::Settings(SettingsMsg::Edit(SettingsMessage::SetLaunchAtLogin(true))),
    );
    assert!(!app.preferences.launch_at_login);
    assert!(!app.preferences.debug_logging);
    assert!(
        !dir.path().join("config").exists(),
        "preferences dir recreated"
    );
    assert!(!dir.path().join("data").exists(), "history dir recreated");
}

// ---- Uninstall, fix round 3 (R1–R4) ----

fn confirming_app(purge: bool, prefs_on_disk: bool) -> (tempfile::TempDir, App) {
    let dir = tempfile::TempDir::new().unwrap();
    let store: Arc<dyn HistoryStore> =
        Arc::new(FsHistoryStore::new(dir.path().join("data").join("history")));
    let mut app = build_app_with_history(Arc::new(FakePermissions::granted()), store);
    let path = dir.path().join("config").join("preferences.toml");
    app.preferences.launch_at_login = true;
    if prefs_on_disk {
        app.preferences.save(&path).unwrap();
    }
    app.preferences_path = Some(path);
    app.hotkey_actions.insert(7, GlobalHotkeyAction::Capture);
    app.settings_window_id = Some(iced::window::Id::unique());
    app.settings_uninstall_pending = true;
    app.settings_uninstall_purge = purge;
    app.coordinator.flush_history_updates();
    (dir, app)
}

fn saved_launch_at_login(dir: &tempfile::TempDir) -> Option<bool> {
    let path = dir.path().join("config").join("preferences.toml");
    path.exists()
        .then(|| Preferences::load(&path).unwrap().launch_at_login)
}

/// W5a S3: a History write that lands after a purge removed the
/// History folder (stuck past the bounded flush) fails rather than make
/// the folder again. Without a purge nothing is sealed.
#[test]
fn a_late_history_write_never_brings_a_purged_folder_back() {
    let (dir, mut app) = confirming_app(true, true);
    let root = dir.path().join("data").join("history");
    let record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 8, 8, "d");
    let png = readshot_core::png::encode(&solid(8, 8)).unwrap();
    FsHistoryStore::new(&root).save(&record, &png).unwrap();
    let _ = update(&mut app, Message::Settings(SettingsMsg::UninstallConfirmed));
    std::fs::remove_dir_all(dir.path().join("data")).unwrap();
    assert!(app.coordinator.update_history(&record).is_err());
    assert!(!root.exists(), "the purged History folder came back");
}

/// R2: confirming goes inert before any step runs, so nothing done
/// meanwhile (a shortcut, a capture landing) writes into History.
#[test]
fn confirming_an_uninstall_goes_inert_before_it_runs() {
    let (_dir, mut app) = confirming_app(true, true);
    let _ = update(&mut app, Message::Settings(SettingsMsg::UninstallConfirmed));
    assert!(app.uninstalled);
    assert!(app.hotkey_actions.is_empty());
    let progress = app.settings_uninstall_result.as_ref().expect("said");
    assert_eq!(progress.tone, StatusTone::Progress);
    let task = update(
        &mut app,
        Message::Capture(CaptureMsg::RegionCaptured(Ok(solid(8, 8)))),
    );
    assert_eq!(task.units(), 0);
    assert!(app.editor.is_none());
    // The steps' outcome still arrives and is said.
    let _ = update(
        &mut app,
        Message::Settings(SettingsMsg::UninstallFinished {
            program_removed: false,
            purge: true,
            failed: Vec::new(),
        }),
    );
    assert_eq!(
        app.settings_uninstall_result.as_ref().unwrap().tone,
        StatusTone::Warning
    );
}

/// R1 and W5a S2: kept preferences end up saying Open at Login is off,
/// but only after the steps removed the entry
/// (`uninstall::record_open_at_login_off_after`, tested there), never at
/// confirm: if the removal then failed, the file would say off while
/// the entry stays. A purged or never-written file is not created.
#[test]
fn a_kept_preferences_file_is_not_written_before_the_steps_run() {
    let (dir, mut app) = confirming_app(false, true);
    let _ = update(&mut app, Message::Settings(SettingsMsg::UninstallConfirmed));
    assert_eq!(
        saved_launch_at_login(&dir),
        Some(true),
        "nothing recorded before the login entry is removed"
    );
    assert!(!app.preferences.launch_at_login);

    let (dir, mut app) = confirming_app(false, false);
    let _ = update(&mut app, Message::Settings(SettingsMsg::UninstallConfirmed));
    assert_eq!(saved_launch_at_login(&dir), None, "no file created");

    let (dir, mut app) = confirming_app(true, true);
    let _ = update(&mut app, Message::Settings(SettingsMsg::UninstallConfirmed));
    assert_eq!(
        saved_launch_at_login(&dir),
        Some(true),
        "a purge deletes the file; nothing is written into it first"
    );
}

/// R3: in the inert state Esc / ⌘W close Settings and ↵ quits; no other
/// Settings key does anything.
#[test]
fn an_uninstalled_settings_window_answers_esc_and_return_only() {
    use crate::runtime::settings_ops::{uninstalled_settings_key, InertSettingsKey};
    use iced::keyboard::{key::Named, Key, Modifiers};
    assert_eq!(
        uninstalled_settings_key(&Key::Named(Named::Escape), Modifiers::default()),
        Some(InertSettingsKey::Close)
    );
    assert_eq!(
        uninstalled_settings_key(&Key::Character("w".into()), Modifiers::COMMAND),
        Some(InertSettingsKey::Close)
    );
    assert_eq!(
        uninstalled_settings_key(&Key::Named(Named::Enter), Modifiers::default()),
        Some(InertSettingsKey::Quit)
    );
    assert_eq!(
        uninstalled_settings_key(&Key::Character("l".into()), Modifiers::COMMAND),
        None,
        "⌘L must not toggle Open at Login back on"
    );

    let (_dir, mut app) = uninstalled_app(false);
    let id = app.settings_window_id.unwrap();
    app.windows.register(id, WindowKind::Settings);
    let _ = update(
        &mut app,
        Message::Settings(SettingsMsg::KeyPressed {
            window: id,
            key: Key::Character("l".into()),
            modifiers: Modifiers::COMMAND,
            captured: false,
        }),
    );
    assert!(!app.preferences.launch_at_login);
    let _ = update(
        &mut app,
        Message::Settings(SettingsMsg::KeyPressed {
            window: id,
            key: Key::Named(Named::Escape),
            modifiers: Modifiers::default(),
            captured: false,
        }),
    );
    assert_eq!(app.windows.kind(id), None, "Esc closed Settings");
}

/// R4: the tray's Settings item reopens Settings on the uninstall result,
/// and Settings shows nothing else to change.
#[test]
fn the_tray_reopens_settings_on_the_uninstall_result() {
    let (_dir, mut app) = uninstalled_app(false);
    app.settings_window_id = None;
    app.settings_pane = SettingsPane::Capture;
    let _ = update(
        &mut app,
        Message::Tick(TickMsg::TrayAction(crate::tray::TrayAction::Settings)),
    );
    assert!(app.settings_window_id.is_some());
    assert_eq!(app.settings_pane, SettingsPane::App);
    assert!(!crate::runtime::view::settings_sidebar_shown(&app));
    assert!(app.settings_uninstall_result.is_some());
}
