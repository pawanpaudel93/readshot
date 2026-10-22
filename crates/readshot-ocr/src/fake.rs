//! Deterministic fake OCR engine for tests.
//!
//! [`FakeOcrEngine::with_text`] returns the same string for every
//! request. [`FakeOcrEngine::no_text`] simulates an empty-result run
//! (the MCP server's "image had no text" path). [`FakeOcrEngine::failing`]
//! always errors with a typed [`OCRError::Backend`].
//! [`FakeOcrEngine::with_lines`] returns positioned lines (with optional
//! word boxes) like Apple Vision, and reports positions.
//!
//! All four are `Send + Sync` and synchronous internally; the async
//! signature on [`OCREngine::recognise`] is satisfied by an immediately-
//! resolving future.

use async_trait::async_trait;
use readshot_core::error::OCRError;
use readshot_core::ocr_layout::RecognizedLine;

use crate::{OCREngine, OCRRequest, OCRResult};

#[derive(Clone, Debug)]
enum Behaviour {
    Text(String, f32),
    Empty,
    Fail(String),
    Lines(Vec<RecognizedLine>),
}

#[derive(Clone, Debug)]
pub struct FakeOcrEngine {
    behaviour: Behaviour,
    languages: Vec<String>,
}

impl FakeOcrEngine {
    pub fn with_text(text: impl Into<String>) -> Self {
        Self {
            behaviour: Behaviour::Text(text.into(), 0.95),
            languages: vec!["en".to_string()],
        }
    }

    pub fn with_text_and_confidence(text: impl Into<String>, confidence: f32) -> Self {
        Self {
            behaviour: Behaviour::Text(text.into(), confidence),
            languages: vec!["en".to_string()],
        }
    }

    pub fn no_text() -> Self {
        Self {
            behaviour: Behaviour::Empty,
            languages: vec!["en".to_string()],
        }
    }

    pub fn failing(message: impl Into<String>) -> Self {
        Self {
            behaviour: Behaviour::Fail(message.into()),
            languages: vec!["en".to_string()],
        }
    }

    /// Positioned output, like Apple Vision: `text` is the line texts
    /// joined with `\n`, `lines` is returned as given.
    pub fn with_lines(lines: Vec<RecognizedLine>) -> Self {
        Self {
            behaviour: Behaviour::Lines(lines),
            languages: vec!["en".to_string()],
        }
    }

    /// A small two-column table reported column by column, the way
    /// Apple Vision often orders observations. The engine's flat `text`
    /// is therefore column-major; the layout pass puts it back in rows.
    /// Shared by the GUI, CLI and MCP tests that prove all three return
    /// the same layout text.
    pub fn layout_fixture_lines() -> Vec<RecognizedLine> {
        let cell = |text: &str, x: f32, y: f32| RecognizedLine {
            text: text.to_string(),
            x,
            y,
            w: 0.2,
            h: 0.05,
            words: Vec::new(),
        };
        vec![
            cell("Name", 0.1, 0.1),
            cell("apple", 0.1, 0.2),
            cell("pear", 0.1, 0.3),
            cell("Qty", 0.5, 0.1),
            cell("3", 0.5, 0.2),
            cell("12", 0.5, 0.3),
        ]
    }

    pub fn with_supported_languages(mut self, langs: Vec<String>) -> Self {
        self.languages = langs;
        self
    }
}

impl Default for FakeOcrEngine {
    fn default() -> Self {
        Self::with_text("Hello, Readshot")
    }
}

#[async_trait]
impl OCREngine for FakeOcrEngine {
    fn supported_languages(&self) -> Vec<String> {
        self.languages.clone()
    }

    fn reports_positions(&self) -> bool {
        matches!(self.behaviour, Behaviour::Lines(_))
    }

    async fn recognise(&self, _req: OCRRequest) -> Result<OCRResult, OCRError> {
        match &self.behaviour {
            Behaviour::Text(text, confidence) => Ok(OCRResult {
                text: text.clone(),
                average_confidence: *confidence,
                lines: Vec::new(),
            }),
            Behaviour::Empty => Ok(OCRResult {
                text: String::new(),
                average_confidence: 0.0,
                lines: Vec::new(),
            }),
            Behaviour::Fail(msg) => Err(OCRError::Backend(msg.clone())),
            Behaviour::Lines(lines) => Ok(OCRResult {
                text: lines
                    .iter()
                    .map(|l| l.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
                average_confidence: 0.95,
                lines: lines.clone(),
            }),
        }
    }
}
