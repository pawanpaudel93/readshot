//! User preferences — the TOML on-disk schema described in spec §3.11.
//!
//! [`Preferences`] is the typed Rust mirror of the file at
//! `<config_dir>/readshot/preferences.toml`. The file is pretty-printed
//! TOML so users (and reviewers) can edit it by hand without learning a
//! schema language.
//!
//! ## Schema versioning
//!
//! [`PREFERENCES_SCHEMA_VERSION`] is bumped whenever a non-additive change
//! lands. [`Preferences::load`] silently upgrades older files to the
//! current version on read; the upgrade fills in defaults via
//! `#[serde(default)]` on every field, so adding a *new* field is always
//! safe and never requires a migration.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::PreferencesError;
use crate::fs_atomic::write_atomic;

/// Bumped on every breaking schema change.
pub const PREFERENCES_SCHEMA_VERSION: u32 = 3;

/// Image format chosen when the user invokes Save.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    #[default]
    Png,
    Jpeg,
    Webp,
}

/// User-facing labels for the export format, so the settings `pick_list`
/// renders human names ("PNG", "JPEG", "WebP") rather than debug output.
impl fmt::Display for ExportFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            ExportFormat::Png => "PNG",
            ExportFormat::Jpeg => "JPEG",
            ExportFormat::Webp => "WebP",
        };
        f.write_str(label)
    }
}

/// Capture-history retention policy. `Off` is the default, matching the
/// spec's privacy-first stance — the user has to opt in explicitly.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryRetention {
    #[default]
    Off,
    Last50,
    Last30Days,
    Unlimited,
}

/// User-facing labels for the retention policy. The settings UI's
/// `pick_list` and the about box both render via this `Display` impl
/// so the strings stay consistent.
impl fmt::Display for HistoryRetention {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            HistoryRetention::Off => "Off",
            HistoryRetention::Last50 => "Last 50 captures",
            HistoryRetention::Last30Days => "Last 30 days",
            HistoryRetention::Unlimited => "Unlimited",
        };
        f.write_str(label)
    }
}

/// Which OCR engine to use. `Native` picks the per-platform default
/// (Apple Vision / Windows.Media.Ocr / ocrs); `Tesseract` opts the user
/// into a separately-installed Tesseract binary as the fallback.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OcrEngineChoice {
    #[default]
    Native,
    Tesseract,
}

/// User-facing labels for the OCR engine choice.
impl fmt::Display for OcrEngineChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            OcrEngineChoice::Native => "System default",
            OcrEngineChoice::Tesseract => "Tesseract",
        };
        f.write_str(label)
    }
}

/// Sparkle / self-check / Flatpak update channel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateChannel {
    #[default]
    Stable,
    Beta,
}

/// User-facing labels for the update channel.
impl fmt::Display for UpdateChannel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            UpdateChannel::Stable => "Stable",
            UpdateChannel::Beta => "Beta",
        };
        f.write_str(label)
    }
}

/// How many custom colors the editor's Recent row keeps.
pub const RECENT_COLORS_MAX: usize = 6;

/// Top-level user preferences, persisted as `preferences.toml`.
///
/// All fields are `#[serde(default)]` so a partially-written file (or one
/// produced by an older version of Readshot that lacked the field) reads
/// cleanly and the missing values fall back to [`Default`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    /// Schema version of *this* document. Initialised by `Default` to
    /// [`PREFERENCES_SCHEMA_VERSION`] and bumped by `load()` when an
    /// older file is read.
    pub schema_version: u32,

    /// Stringified hotkey accepted by `global-hotkey` — e.g.
    /// `"ctrl+shift+x"` (Windows / Linux default) or
    /// `"cmd+shift+x"` (macOS suggestion). Stored as a string so the
    /// preferences format doesn't depend on the hotkey crate.
    pub capture_hotkey: String,

    /// Folder to save captures into. An empty path means "use the
    /// platform default" (`~/Desktop` on macOS, `Pictures\Screenshots`
    /// on Windows, `$XDG_PICTURES_DIR/Screenshots` on Linux). The
    /// resolution happens at the call site, not here.
    pub save_folder: PathBuf,

    /// Tokenised filename template — see [`crate::filename::expand`].
    pub filename_template: String,

    pub default_format: ExportFormat,

    pub history_retention: HistoryRetention,

    /// Preferred recognition languages, BCP-47 codes ordered by priority.
    /// Empty means "ask the OCR engine to use whatever it has". The
    /// engine intersects this with its own supported list at runtime.
    pub ocr_languages: Vec<String>,

    pub ocr_engine_choice: OcrEngineChoice,

    pub launch_at_login: bool,

    pub update_channel: UpdateChannel,

    /// When true, components log at `DEBUG` level instead of `INFO`.
    pub debug_logging: bool,

    /// True once the first-run permission flow has shown the user the
    /// ready state. This keeps normal launches tray-only while still
    /// making the first post-permission relaunch visibly confirm that
    /// Readshot came back.
    pub onboarding_completed: bool,

    /// New editor tools the user has opened at least once (stable ids
    /// from `ToolState::id`). Drives the "new" dot on the tool bar.
    pub editor_tools_seen: Vec<String>,

    /// Last custom annotation colors (`#RRGGBB`), most recent first, at
    /// most `RECENT_COLORS_MAX`. Presets are never stored here.
    pub editor_recent_colors: Vec<String>,

    /// Editor side panel shown (spec §2.3). Remembered across sessions.
    pub editor_panel_open: bool,

    /// Last side-panel tab id (`"text"`; Phase C adds `"beautify"`). A
    /// string, not an enum, so a newer build's tab id can never make an
    /// older build fail to read this file. Unknown ids mean Text.
    pub editor_panel_tab: String,

    /// Text panel: "Remove line breaks when copying" (spec §3.7).
    pub editor_copy_remove_line_breaks: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            schema_version: PREFERENCES_SCHEMA_VERSION,
            capture_hotkey: "ctrl+shift+x".to_string(),
            save_folder: PathBuf::new(),
            filename_template: "Screenshot {YYYY-MM-DD at HH.mm.ss}".to_string(),
            default_format: ExportFormat::Png,
            history_retention: HistoryRetention::Off,
            ocr_languages: Vec::new(),
            ocr_engine_choice: OcrEngineChoice::Native,
            launch_at_login: false,
            update_channel: UpdateChannel::Stable,
            debug_logging: false,
            onboarding_completed: false,
            editor_tools_seen: Vec::new(),
            editor_recent_colors: Vec::new(),
            editor_panel_open: true,
            editor_panel_tab: "text".to_string(),
            editor_copy_remove_line_breaks: false,
        }
    }
}

impl Preferences {
    /// Read preferences from `path`, applying schema migrations.
    ///
    /// A missing file is *not* an error — callers handle "first launch"
    /// by checking with `path.exists()` themselves before calling, or by
    /// using [`Preferences::load_or_default`].
    pub fn load(path: &Path) -> Result<Preferences, PreferencesError> {
        let content = std::fs::read_to_string(path)?;
        let mut prefs: Preferences = toml::from_str(&content)?;
        prefs.migrate();
        Ok(prefs)
    }

    /// Convenience: read or fall back to [`Default`] if the file does not
    /// exist. IO errors other than not-found still propagate.
    pub fn load_or_default(path: &Path) -> Result<Preferences, PreferencesError> {
        if !path.exists() {
            return Ok(Preferences::default());
        }
        Self::load(path)
    }

    /// Write a pretty-printed TOML representation to `path`. Creates the
    /// parent directory as needed. The write is atomic: contents are
    /// staged to a sibling `.tmp` file and renamed into place so a crash
    /// mid-write cannot leave a torn or truncated preferences file.
    pub fn save(&self, path: &Path) -> Result<(), PreferencesError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)?;
        write_atomic(path, content.as_bytes())?;
        Ok(())
    }

    /// Record that the editor tool `id` has been used. Returns `true`
    /// when this is the first time (the caller persists).
    pub fn mark_tool_seen(&mut self, id: &str) -> bool {
        if self.editor_tools_seen.iter().any(|s| s == id) {
            return false;
        }
        self.editor_tools_seen.push(id.to_string());
        true
    }

    /// Move `hex` to the front of the Recent row. Returns whether the
    /// list changed (the caller persists).
    pub fn push_recent_color(&mut self, hex: &str) -> bool {
        let hex = hex.trim().to_ascii_uppercase();
        if self.editor_recent_colors.first() == Some(&hex) {
            return false;
        }
        self.editor_recent_colors.retain(|h| *h != hex);
        self.editor_recent_colors.insert(0, hex);
        self.editor_recent_colors.truncate(RECENT_COLORS_MAX);
        true
    }

    /// Apply schema migrations in place.
    fn migrate(&mut self) {
        let original_version = self.schema_version;
        if original_version < 2 && self.ocr_languages == ["en"] {
            self.ocr_languages.clear();
        }
        // `onboarding_completed` was introduced in schema v3. `migrate`
        // only ever runs on a file that already exists on disk (a fresh
        // user has no file and gets `Default`, which is already v3), so
        // *any* pre-v3 file belongs to an existing user who has already
        // been onboarded — including v0 (a file whose `schema_version`
        // predates the field, deserialised to 0). The previous
        // `(1..3)` range excluded v0 and re-showed onboarding to those
        // users.
        if original_version < 3 {
            self.onboarding_completed = true;
        }
        if self.schema_version < PREFERENCES_SCHEMA_VERSION {
            self.schema_version = PREFERENCES_SCHEMA_VERSION;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn temp_path() -> (TempDir, PathBuf) {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("preferences.toml");
        (dir, path)
    }

    #[test]
    fn mark_tool_seen_is_idempotent_and_round_trips() {
        let (_dir, path) = temp_path();
        let mut p = Preferences::default();
        assert!(p.editor_tools_seen.is_empty());
        assert!(p.mark_tool_seen("spotlight"));
        assert!(!p.mark_tool_seen("spotlight"));
        p.save(&path).unwrap();
        let back = Preferences::load(&path).unwrap();
        assert_eq!(back.editor_tools_seen, vec!["spotlight".to_string()]);
    }

    #[test]
    fn recent_colors_are_mru_deduped_uppercase_and_capped() {
        let mut p = Preferences::default();
        for hex in [
            "#111111", "#222222", "#333333", "#444444", "#555555", "#666666", "#777777",
        ] {
            assert!(p.push_recent_color(hex));
        }
        assert_eq!(p.editor_recent_colors.len(), RECENT_COLORS_MAX);
        assert_eq!(p.editor_recent_colors[0], "#777777");
        assert!(p.push_recent_color("#333333"));
        assert_eq!(p.editor_recent_colors[0], "#333333");
        assert_eq!(p.editor_recent_colors.len(), RECENT_COLORS_MAX);
        assert!(!p.push_recent_color("#333333"), "already most recent");
        assert!(p.push_recent_color("#abcdef"));
        assert_eq!(p.editor_recent_colors[0], "#ABCDEF");
    }

    #[test]
    fn default_round_trips_unchanged() {
        let (_dir, path) = temp_path();
        let p = Preferences::default();
        p.save(&path).unwrap();
        let back = Preferences::load(&path).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        // Write a file with only the schema_version field; serde must
        // fill the rest from Default.
        let (_dir, path) = temp_path();
        std::fs::write(
            &path,
            format!("schema_version = {PREFERENCES_SCHEMA_VERSION}\n"),
        )
        .unwrap();
        let prefs = Preferences::load(&path).unwrap();
        let default = Preferences::default();
        assert_eq!(prefs, default);
    }

    #[test]
    fn missing_file_is_a_typed_io_error() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("does-not-exist.toml");
        let err = Preferences::load(&path).unwrap_err();
        assert!(matches!(err, PreferencesError::Io(_)));
    }

    #[test]
    fn load_or_default_handles_missing_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("does-not-exist.toml");
        let prefs = Preferences::load_or_default(&path).unwrap();
        assert_eq!(prefs, Preferences::default());
    }

    #[test]
    fn migration_stamps_version_forward() {
        let (_dir, path) = temp_path();
        std::fs::write(&path, "schema_version = 0\ncapture_hotkey = \"f1\"\n").unwrap();
        let prefs = Preferences::load(&path).unwrap();
        assert_eq!(prefs.schema_version, PREFERENCES_SCHEMA_VERSION);
        assert_eq!(prefs.capture_hotkey, "f1");
    }

    #[test]
    fn default_ocr_languages_uses_engine_automatic_detection() {
        let prefs = Preferences::default();
        assert!(prefs.ocr_languages.is_empty());
    }

    #[test]
    fn migration_clears_legacy_english_ocr_default() {
        let (_dir, path) = temp_path();
        std::fs::write(
            &path,
            r#"
schema_version = 1
ocr_languages = ["en"]
"#,
        )
        .unwrap();

        let prefs = Preferences::load(&path).unwrap();

        assert_eq!(prefs.schema_version, PREFERENCES_SCHEMA_VERSION);
        assert!(prefs.ocr_languages.is_empty());
    }

    #[test]
    fn migration_marks_existing_users_onboarded() {
        let (_dir, path) = temp_path();
        std::fs::write(&path, "schema_version = 2\n").unwrap();

        let prefs = Preferences::load(&path).unwrap();

        assert_eq!(prefs.schema_version, PREFERENCES_SCHEMA_VERSION);
        assert!(prefs.onboarding_completed);
    }

    #[test]
    fn migration_marks_schema_v0_users_onboarded() {
        // An explicit `schema_version = 0` file is still an existing
        // user's file (migrate only runs on a file that exists on disk),
        // so it must not re-show onboarding. The old `(1..3)` range
        // wrongly excluded v0; `onboarding_completed` was introduced in
        // v3 so every pre-v3 file should be marked onboarded.
        let (_dir, path) = temp_path();
        std::fs::write(&path, "schema_version = 0\ncapture_hotkey = \"f1\"\n").unwrap();

        let prefs = Preferences::load(&path).unwrap();

        // migrate stamps the version forward and marks the user onboarded.
        assert_eq!(prefs.schema_version, PREFERENCES_SCHEMA_VERSION);
        assert!(prefs.onboarding_completed);
    }

    #[test]
    fn save_creates_parent_directory() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nested/deep/preferences.toml");
        Preferences::default().save(&path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn enum_serialisation_matches_serde_rename() {
        let p = Preferences {
            history_retention: HistoryRetention::Last30Days,
            default_format: ExportFormat::Webp,
            update_channel: UpdateChannel::Beta,
            ..Preferences::default()
        };
        let toml_text = toml::to_string(&p).unwrap();
        assert!(toml_text.contains("history_retention = \"last30_days\""));
        assert!(toml_text.contains("default_format = \"webp\""));
        assert!(toml_text.contains("update_channel = \"beta\""));
    }

    #[test]
    fn panel_preferences_default_and_round_trip() {
        let p = Preferences::default();
        assert!(p.editor_panel_open);
        assert_eq!(p.editor_panel_tab, "text");
        assert!(!p.editor_copy_remove_line_breaks);

        let (_dir, path) = temp_path();
        let p = Preferences {
            editor_panel_open: false,
            editor_panel_tab: "beautify".into(),
            editor_copy_remove_line_breaks: true,
            ..Preferences::default()
        };
        p.save(&path).unwrap();
        assert_eq!(Preferences::load(&path).unwrap(), p);
    }

    #[test]
    fn files_without_panel_preferences_load_with_defaults() {
        let (_dir, path) = temp_path();
        std::fs::write(
            &path,
            "schema_version = 3\ncapture_hotkey = \"cmd+shift+x\"\n",
        )
        .unwrap();
        let p = Preferences::load(&path).unwrap();
        assert!(p.editor_panel_open);
        assert_eq!(p.editor_panel_tab, "text");
        assert!(!p.editor_copy_remove_line_breaks);
    }
}
