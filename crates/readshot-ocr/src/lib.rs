//! Per-OS offline OCR for Readshot.
//!
//! The public API is the [`OCREngine`] trait plus three platform-specific
//! implementations selected at compile time:
//!
//! * [`macos::AppleVisionEngine`] — `objc2-vision`'s `VNRecognizeTextRequest`.
//! * `windows::WindowsMediaOcrEngine` — `windows` crate's
//!   `Windows::Media::Ocr`.
//! * `linux::OcrsEngine` — the pure-Rust `ocrs` engine over its two
//!   `.rten` model files.
//!
//! The optional `test-fixtures` feature exposes `fake::FakeOcrEngine`,
//! which returns deterministic preset text for tests.
//!
//! Errors are typed as [`readshot_core::error::OCRError`]; the CLI exit-code
//! mapper and MCP error mapper match on its
//! variants directly.

// Unsafe is only for platform FFI, and each block says why it is
// sound (AGENTS.md, "Things to avoid").
#![deny(clippy::undocumented_unsafe_blocks)]

use async_trait::async_trait;
use image::RgbaImage;
use readshot_core::error::OCRError;

#[cfg(any(test, feature = "test-fixtures"))]
pub mod fake;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "windows")]
pub mod windows;

/// One OCR request from coordinator to engine. The image is RGBA8 (the
/// renderer's output format and what the clipboard / capture stack
/// produce); per-engine wrappers handle any conversion to RGB / grayscale
/// the underlying API needs.
#[derive(Clone, Debug)]
pub struct OCRRequest {
    pub image: RgbaImage,
    /// BCP-47 language codes, ordered by priority. An empty list signals
    /// "use the engine's default" — typically the user's system locale.
    pub languages: Vec<String>,
    /// When true, ask the engine to apply post-recognition language
    /// correction (Apple Vision's `usesLanguageCorrection`, Windows.Media's
    /// equivalent dictionary check). Has no effect on engines that don't
    /// support it (currently ocrs).
    pub use_language_correction: bool,
}

/// One OCR response. `text` is line-joined with `\n`; per-line confidence
/// information (when the underlying engine exposes it) is averaged into
/// `average_confidence`. A successful call that found no text returns
/// `text: ""` and `average_confidence: 0.0` rather than an error — the
/// MCP server relies on this so empty results aren't surfaced as tool
/// failures.
///
/// `lines` carries the same recognised text with normalised bounding
/// boxes, when the engine exposes them (today: macOS Apple Vision).
/// Backends without positioned output leave it empty; downstream
/// callers fall back to `text` when the vector is empty.
#[derive(Clone, Debug)]
pub struct OCRResult {
    pub text: String,
    pub average_confidence: f32,
    pub lines: Vec<readshot_core::ocr_layout::RecognizedLine>,
}

/// The single trait every Readshot OCR backend implements. `Send + Sync`
/// because the capture coordinator holds a `Box<dyn OCREngine>`
/// inside a `tokio::task` that may move between runtime threads.
// `#[async_trait]` puts `#[must_use]` on methods that return its boxed
// (already must-use) future; newer clippy flags that as double_must_use.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait OCREngine: Send + Sync {
    /// Languages this engine can recognise, BCP-47 codes. Returned as
    /// owned strings (not borrows) because some engines (Windows.Media)
    /// build the list lazily from system state.
    fn supported_languages(&self) -> Vec<String>;

    async fn recognise(&self, req: OCRRequest) -> Result<OCRResult, OCRError>;

    /// Whether `recognise` fills `OCRResult::lines` with boxes (Apple
    /// Vision). Callers use it to skip OCR that could only reproduce
    /// text they already have.
    fn reports_positions(&self) -> bool {
        false
    }
}

/// Construct the platform-default OCR engine. The Linux variant uses a
/// fallback models directory (`<data_dir>/readshot/models/`); the user's
/// `Preferences` can override this in higher layers.
#[allow(clippy::needless_return)]
pub fn default_engine() -> Box<dyn OCREngine> {
    #[cfg(target_os = "macos")]
    {
        return Box::new(macos::AppleVisionEngine::new());
    }
    #[cfg(target_os = "windows")]
    {
        return Box::new(windows::WindowsMediaOcrEngine::new());
    }
    #[cfg(target_os = "linux")]
    {
        return Box::new(linux::OcrsEngine::with_default_models_dir());
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        compile_error!(
            "readshot-ocr: unsupported target_os; only macOS, Windows, and Linux are supported"
        );
    }
}
