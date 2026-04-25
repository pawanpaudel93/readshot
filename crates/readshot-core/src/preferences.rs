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

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::PreferencesError;

/// Bumped on every breaking schema change.
pub const PREFERENCES_SCHEMA_VERSION: u32 = 1;

/// Image format chosen when the user invokes Save.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    #[default]
    Png,
    Jpeg,
    Webp,
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

/// Sparkle / self-check / Flatpak update channel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateChannel {
    #[default]
    Stable,
    Beta,
}

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
            ocr_languages: vec!["en".to_string()],
            ocr_engine_choice: OcrEngineChoice::Native,
            launch_at_login: false,
            update_channel: UpdateChannel::Stable,
            debug_logging: false,
        }
    }
}

impl Preferences {
    /// Read preferences from `path`, applying schema migrations.
    ///
    /// A missing file is *not* an error — callers handle "first launch"
    /// by checking with `path.exists()` themselves before calling, or by
    /// using [`load_or_default`].
    pub fn load(path: &Path) -> Result<Preferences, PreferencesError> {
        let content = std::fs::read_to_string(path)?;
        let mut prefs: Preferences = toml::from_str(&content)?;
        prefs.migrate();
        Ok(prefs)
    }

    /// Convenience: read or fall back to [`Default`] if the file does not
    /// exist. IO errors other than not-found still propagate.
    pub fn load_or_default(path: &Path) -> Result<Preferences, PreferencesError> {
        match Self::load(path) {
            Ok(p) => Ok(p),
            Err(PreferencesError::Io(msg)) if msg.contains("(os error 2)") || msg.contains("No such") => {
                Ok(Preferences::default())
            }
            Err(e) => Err(e),
        }
    }

    /// Write a pretty-printed TOML representation to `path`. Creates the
    /// parent directory as needed.
    pub fn save(&self, path: &Path) -> Result<(), PreferencesError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    /// Apply schema migrations in place. Currently a no-op beyond
    /// stamping the version forward; future migrations land here as
    /// `match` arms on the incoming `schema_version`.
    fn migrate(&mut self) {
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
        std::fs::write(&path, "schema_version = 1\n").unwrap();
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
        std::fs::write(
            &path,
            "schema_version = 0\ncapture_hotkey = \"f1\"\n",
        )
        .unwrap();
        let prefs = Preferences::load(&path).unwrap();
        assert_eq!(prefs.schema_version, PREFERENCES_SCHEMA_VERSION);
        assert_eq!(prefs.capture_hotkey, "f1");
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
}
