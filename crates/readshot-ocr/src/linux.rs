//! Linux OCR — pure-Rust `ocrs` engine.
//!
//! [`ocrs`](https://crates.io/crates/ocrs) is the only realistic free
//! offline OCR option on Linux: Tesseract is heavier, Apple Vision is
//! macOS-only, Windows.Media.Ocr is Windows-only. ocrs runs the same
//! pure-Rust ONNX-ish runtime (`rten`) on every architecture.
//!
//! ## Models
//!
//! ocrs needs two `.rten` model files (text-detection ≈ 17 MB,
//! text-recognition ≈ 13 MB). They are **not bundled** with this crate
//! or with Readshot's binaries — bundling 30 MB of model artefacts in
//! every install vehicle (DMG, MSI, AppImage, Flatpak) is disproportionate
//! when most macOS / Windows users don't need them. Instead this engine
//! looks for the two files in a configurable directory:
//!
//! 1. The path given to [`OcrsEngine::new`].
//! 2. The default — `<data_local_dir>/readshot/models/` — used by
//!    [`OcrsEngine::with_default_models_dir`] and the workspace-wide
//!    [`crate::default_engine`] factory.
//!
//! If the files are missing, the first call to [`recognise`] returns a
//! typed [`OCRError::Backend`] explaining that the models need to be
//! downloaded. The Linux package post-install hook (Task 17) handles
//! the download for end users.
//!
//! ## Languages
//!
//! ocrs's bundled models are English only. Multilingual support is
//! advertised through downloadable model packs which the user can drop
//! into the same directory. [`supported_languages`] returns `["en"]`
//! by default and the user can override via preferences.

use std::path::PathBuf;
use std::sync::OnceLock;

use async_trait::async_trait;
use ocrs::{ImageSource, OcrEngine as OcrsEngineInner, OcrEngineParams};
use readshot_core::error::OCRError;
use rten::Model;

use crate::{OCREngine, OCRRequest, OCRResult};

const DETECTION_FILENAME: &str = "text-detection.rten";
const RECOGNITION_FILENAME: &str = "text-recognition.rten";

/// Production Linux OCR engine. Lazily loads the two model files on the
/// first [`recognise`] call.
pub struct OcrsEngine {
    models_dir: PathBuf,
    inner: OnceLock<OcrsEngineInner>,
}

impl OcrsEngine {
    /// Construct an engine that loads its models from the given
    /// directory. The directory must contain `text-detection.rten` and
    /// `text-recognition.rten`.
    pub fn new(models_dir: PathBuf) -> Self {
        Self {
            models_dir,
            inner: OnceLock::new(),
        }
    }

    /// Construct an engine pointed at the default per-user models
    /// directory. Used by [`crate::default_engine`].
    pub fn with_default_models_dir() -> Self {
        Self::new(default_models_dir())
    }

    pub fn models_dir(&self) -> &std::path::Path {
        &self.models_dir
    }

    fn ensure_initialised(&self) -> Result<&OcrsEngineInner, OCRError> {
        if let Some(eng) = self.inner.get() {
            return Ok(eng);
        }

        let det_path = self.models_dir.join(DETECTION_FILENAME);
        let rec_path = self.models_dir.join(RECOGNITION_FILENAME);

        if !det_path.exists() || !rec_path.exists() {
            return Err(OCRError::Backend(format!(
                "ocrs models missing under {dir}; expected `{det}` and `{rec}` — \
                 download from https://github.com/robertknight/ocrs (see \
                 docs/AGENTS-CLI.md once Task 20 lands)",
                dir = self.models_dir.display(),
                det = DETECTION_FILENAME,
                rec = RECOGNITION_FILENAME,
            )));
        }

        let detection_model = Model::load_file(&det_path)
            .map_err(|e| OCRError::Backend(format!("ocrs detection model load: {e}")))?;
        let recognition_model = Model::load_file(&rec_path)
            .map_err(|e| OCRError::Backend(format!("ocrs recognition model load: {e}")))?;

        let engine = OcrsEngineInner::new(OcrEngineParams {
            detection_model: Some(detection_model),
            recognition_model: Some(recognition_model),
            ..Default::default()
        })
        .map_err(|e| OCRError::Backend(format!("ocrs engine init: {e}")))?;

        // OnceLock::set returns Err if a parallel call beat us; in that
        // case the value is already there and `.get()` returns Some.
        let _ = self.inner.set(engine);
        Ok(self
            .inner
            .get()
            .expect("inner just set or set by a parallel caller"))
    }
}

impl Default for OcrsEngine {
    fn default() -> Self {
        Self::with_default_models_dir()
    }
}

fn default_models_dir() -> PathBuf {
    directories::ProjectDirs::from("np.com", "pawanpaudel", "readshot")
        .map(|p| p.data_local_dir().join("models"))
        .unwrap_or_else(|| PathBuf::from("./models"))
}

#[async_trait]
impl OCREngine for OcrsEngine {
    fn supported_languages(&self) -> Vec<String> {
        // ocrs's default model is English; multilingual users supply
        // additional `.rten` files via the same directory. Reporting a
        // static list keeps `supported_languages` cheap (no model load).
        vec!["en".to_string()]
    }

    async fn recognise(&self, req: OCRRequest) -> Result<OCRResult, OCRError> {
        let engine = self.ensure_initialised()?;

        // ocrs's `ImageSource::from_bytes` expects RGB (3 channels). We
        // store captures as RGBA, so drop the alpha channel before
        // handing the bytes over.
        let rgb = drop_alpha_channel(&req.image);
        let dim = (req.image.width(), req.image.height());

        let img_source = ImageSource::from_bytes(&rgb, dim)
            .map_err(|e| OCRError::Backend(format!("ocrs ImageSource: {e}")))?;
        let input = engine
            .prepare_input(img_source)
            .map_err(|e| OCRError::Backend(format!("ocrs prepare_input: {e}")))?;
        let text = engine
            .get_text(&input)
            .map_err(|e| OCRError::Backend(format!("ocrs get_text: {e}")))?;

        let trimmed = text.trim();
        Ok(OCRResult {
            text: trimmed.to_string(),
            // ocrs 0.12 doesn't expose per-line confidence from get_text;
            // a future revision can swap to detect_words + recognize_text
            // and average those scores. 0.0 means "the engine returns no
            // signal", which agents reading MCP responses can ignore.
            average_confidence: 0.0,
        })
    }
}

/// RGBA → RGB by dropping the alpha channel. Equivalent to
/// `image::DynamicImage::ImageRgba8(rgba).into_rgb8().into_raw()` but
/// avoids one full-image allocation.
fn drop_alpha_channel(rgba: &image::RgbaImage) -> Vec<u8> {
    let pixels = rgba.as_raw();
    let mut rgb = Vec::with_capacity(pixels.len() / 4 * 3);
    for chunk in pixels.chunks_exact(4) {
        rgb.push(chunk[0]);
        rgb.push(chunk[1]);
        rgb.push(chunk[2]);
    }
    rgb
}
