//! Deterministic fake OCR engine for tests.
//!
//! [`FakeOcrEngine::with_text`] returns the same string for every
//! request. [`FakeOcrEngine::no_text`] simulates an empty-result run
//! (the MCP server's "image had no text" path). [`FakeOcrEngine::failing`]
//! always errors with a typed [`OCRError::Backend`].
//!
//! All three are `Send + Sync` and synchronous internally; the async
//! signature on [`OCREngine::recognise`] is satisfied by an immediately-
//! resolving future.

use async_trait::async_trait;
use readshot_core::error::OCRError;

use crate::{OCREngine, OCRRequest, OCRResult};

#[derive(Clone, Debug)]
enum Behaviour {
    Text(String, f32),
    Empty,
    Fail(String),
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

    async fn recognise(&self, _req: OCRRequest) -> Result<OCRResult, OCRError> {
        match &self.behaviour {
            Behaviour::Text(text, confidence) => Ok(OCRResult {
                text: text.clone(),
                average_confidence: *confidence,
            }),
            Behaviour::Empty => Ok(OCRResult {
                text: String::new(),
                average_confidence: 0.0,
            }),
            Behaviour::Fail(msg) => Err(OCRError::Backend(msg.clone())),
        }
    }
}
