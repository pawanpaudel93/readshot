//! Headless CLI surface (Task 18).
//!
//! Spec §6 (API Contracts) lists three external entry points:
//!
//! 1. `readshot://` URL scheme → opens the *interactive* overlay
//!    (handled by [`crate::url_scheme`]).
//! 2. `readshot capture | capture-window | ocr | capture-and-ocr |
//!    list-displays | list-windows` → silent, scriptable subcommands
//!    (this module).
//! 3. The MCP server (Task 19) — wraps the same coordinator.
//!
//! The CLI deliberately bypasses the iced GUI: it constructs the
//! coordinator directly, runs one operation, prints the result to
//! stdout, and exits. That makes Readshot scriptable from shell,
//! AI agents, and CI.
//!
//! Permission gating differs from the GUI: the CLI inherits the
//! invoking user's TCC / portal grants, so the call simply fails
//! with `CaptureError::PermissionDenied` if the host hasn't been
//! granted Screen Recording. We surface that as an exit code (see
//! [`Cli::run`]) rather than prompting interactively.
//!
//! ## Region selection
//!
//! The region-capture subcommands take `--display`, `--rect` (a
//! `x,y,w,h` quadruple in logical pixels) and `--scale` (default 1.0).
//! When `--display` is omitted, the first display reported by
//! [`readshot_capture::Capturer::list_displays`] is used. When `--rect`
//! is omitted, the full bounds of the chosen display are captured.
//!
//! ## Output formats
//!
//! * `capture` writes a PNG to `--output` (or `stdout` by default / `-`).
//! * `capture-window` captures one window by id and writes a PNG.
//! * `capture-text` captures a region, runs OCR, and writes recognised text
//!   to stdout. In the GUI, the equivalent no-editor flow is the overlay
//!   toolbar's Copy Text button.
//! * `ocr` reads a PNG from `--input` and writes the recognised text
//!   to `--output` or `stdout`.
//! * `capture-and-ocr` writes the recognised text to `--output` or
//!   `stdout`. Add `--also-image PATH` to additionally save the PNG.
//! * `list-displays` and `list-windows` print human-readable tables by
//!   default, or JSON when `--json` is set — useful for scripts and MCP
//!   wiring.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use image::RgbaImage;
use readshot_capture::{
    CaptureRequest, Capturer, DisplayInfo, WindowCaptureRequest, WindowId, WindowInfo,
};
use readshot_core::error::{CaptureError, OCRError};
use readshot_core::geom::Rect;
use readshot_ocr::{OCREngine, OCRRequest};

/// Top-level CLI parser.
///
/// Most invocations of `readshot` will go to the GUI — the binary's
/// `main` only routes to [`Cli::run`] when `argv.len() > 1`. The
/// `arg_required_else_help = false` setting lets us keep the
/// subcommand optional (so bare `readshot` is still a GUI launch).
#[derive(Debug, Parser)]
#[command(
    name = "readshot",
    version,
    about = "Cross-platform screenshot + offline OCR. Run with no args for the GUI."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List attached displays.
    ListDisplays {
        /// Emit machine-readable JSON instead of a human-readable table.
        #[arg(long)]
        json: bool,
    },

    /// List capturable windows.
    ListWindows {
        /// Emit machine-readable JSON instead of a human-readable table.
        #[arg(long)]
        json: bool,
    },

    /// Capture a region (or full display) and write the PNG to a file.
    Capture {
        /// Target display id (as printed by `list-displays`). Defaults
        /// to the first reported display.
        #[arg(long)]
        display: Option<String>,

        /// Region in logical pixels: `x,y,width,height`. Default: the
        /// full bounds of the chosen display.
        #[arg(long, value_parser = parse_rect)]
        rect: Option<Rect>,

        /// HiDPI scale factor. Default: read from the chosen display's
        /// reported scale.
        #[arg(long)]
        scale: Option<f32>,

        /// Hide the cursor during capture. Default: true.
        #[arg(long, default_value_t = true)]
        hide_cursor: bool,

        /// Hidden compatibility flag for the GUI-only Retake Last
        /// Region workflow. The CLI process has no shared in-memory
        /// overlay state, so this always returns a usage error.
        #[arg(long, hide = true)]
        last_region: bool,

        /// Output PNG path. Use `-` for stdout.
        #[arg(long, short = 'o', default_value = "-")]
        output: PathBuf,
    },

    /// Capture one window and write the PNG to a file.
    CaptureWindow {
        /// Target window id, as printed by `list-windows`.
        #[arg(long)]
        window: String,

        /// Output PNG path. Use `-` for stdout.
        #[arg(long, short = 'o', default_value = "-")]
        output: PathBuf,
    },

    /// Capture a region, run OCR, and write recognised text to stdout.
    CaptureText {
        #[arg(long)]
        display: Option<String>,

        #[arg(long, value_parser = parse_rect)]
        rect: Option<Rect>,

        #[arg(long)]
        scale: Option<f32>,

        #[arg(long, default_value_t = true)]
        hide_cursor: bool,

        /// Output text path. Use `-` for stdout (the default).
        #[arg(long, short = 'o', default_value = "-")]
        output: PathBuf,

        #[arg(long, value_delimiter = ',')]
        languages: Vec<String>,

        #[arg(long, default_value_t = true)]
        language_correction: bool,
    },

    /// Recognise text in an existing PNG.
    Ocr {
        /// Input PNG path. Use `-` for stdin.
        #[arg(long, short = 'i')]
        input: PathBuf,

        /// Output text path. Use `-` for stdout (the default).
        #[arg(long, short = 'o', default_value = "-")]
        output: PathBuf,

        /// BCP-47 language hints, comma-separated. Empty → engine default.
        #[arg(long, value_delimiter = ',')]
        languages: Vec<String>,

        /// Apply post-recognition language correction when supported.
        #[arg(long, default_value_t = true)]
        language_correction: bool,
    },

    /// Capture a region and recognise text in one shot.
    CaptureAndOcr {
        #[arg(long)]
        display: Option<String>,

        #[arg(long, value_parser = parse_rect)]
        rect: Option<Rect>,

        #[arg(long)]
        scale: Option<f32>,

        #[arg(long, default_value_t = true)]
        hide_cursor: bool,

        /// Output text path. Use `-` for stdout (the default).
        #[arg(long, short = 'o', default_value = "-")]
        output: PathBuf,

        /// Optionally also write the captured PNG to this path.
        #[arg(long)]
        also_image: Option<PathBuf>,

        #[arg(long, value_delimiter = ',')]
        languages: Vec<String>,

        #[arg(long, default_value_t = true)]
        language_correction: bool,
    },
}

/// Errors surfaced by the CLI. We map these to non-zero exit codes
/// in `main`. Each variant wraps the underlying domain error so
/// scripts can pattern-match on the printed reason.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("capture failed: {0}")]
    Capture(#[from] CaptureError),
    #[error("OCR failed: {0}")]
    Ocr(#[from] OCRError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("image decode failed: {0}")]
    Image(#[from] image::ImageError),
    #[error("display `{0}` not found")]
    DisplayNotFound(String),
    #[error("no displays reported by capture backend")]
    NoDisplays,
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("invalid rect `{got}`: {reason}")]
    InvalidRect { got: String, reason: String },
}

/// Map [`CliError`] to a stable exit code so callers (CI scripts,
/// agents) can branch on classes of failure.
pub fn exit_code(err: &CliError) -> i32 {
    match err {
        CliError::Capture(CaptureError::PermissionDenied) => 77, // EX_NOPERM
        CliError::Capture(CaptureError::DisplayNotFound(_))
        | CliError::Capture(CaptureError::WindowNotFound(_)) => 66, // EX_NOINPUT
        CliError::Capture(_) => 71,                              // EX_OSERR
        CliError::Ocr(_) => 70,                                  // EX_SOFTWARE
        CliError::DisplayNotFound(_) | CliError::NoDisplays => 66, // EX_NOINPUT
        CliError::InvalidInput(_) | CliError::InvalidRect { .. } => 64, // EX_USAGE
        CliError::Io(_) | CliError::Image(_) => 74,              // EX_IOERR
    }
}

fn parse_rect(s: &str) -> Result<Rect, String> {
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() != 4 {
        return Err(format!("expected x,y,width,height — got `{s}`"));
    }
    let mut nums = [0f32; 4];
    for (i, p) in parts.iter().enumerate() {
        nums[i] = p
            .trim()
            .parse::<f32>()
            .map_err(|e| format!("component {i} (`{p}`) is not a number: {e}"))?;
    }
    Rect::from_xywh(nums[0], nums[1], nums[2], nums[3])
        .ok_or_else(|| format!("rect `{s}` has non-positive or non-finite dimensions"))
}

impl Cli {
    /// Execute the parsed command using the supplied capturer / engine.
    /// `stdout` is parameterised so tests can capture output.
    pub async fn run(
        self,
        capturer: Arc<dyn Capturer>,
        ocr: Arc<dyn OCREngine>,
        stdout: &mut dyn Write,
    ) -> Result<(), CliError> {
        let Some(command) = self.command else {
            return Ok(()); // no subcommand → caller falls back to GUI
        };
        match command {
            Command::ListDisplays { json } => {
                let displays = capturer.list_displays().await?;
                if json {
                    write_displays_json(stdout, &displays)?;
                } else {
                    write_displays_table(stdout, &displays)?;
                }
            }
            Command::ListWindows { json } => {
                let windows = capturer.list_windows().await?;
                if json {
                    write_windows_json(stdout, &windows)?;
                } else {
                    write_windows_table(stdout, &windows)?;
                }
            }
            Command::Capture {
                display,
                rect,
                scale,
                hide_cursor,
                last_region,
                output,
            } => {
                if last_region {
                    return Err(CliError::InvalidInput(
                        "--last-region is only available in the GUI session".into(),
                    ));
                }
                let req =
                    build_capture_request(&*capturer, display.as_deref(), rect, scale, hide_cursor)
                        .await?;
                let img = capturer.capture_region(req).await?;
                write_png(&img, &output, stdout)?;
            }
            Command::CaptureWindow { window, output } => {
                let img = capturer
                    .capture_window(WindowCaptureRequest {
                        window_id: WindowId(window),
                    })
                    .await?;
                write_png(&img, &output, stdout)?;
            }
            Command::CaptureText {
                display,
                rect,
                scale,
                hide_cursor,
                output,
                languages,
                language_correction,
            } => {
                let req =
                    build_capture_request(&*capturer, display.as_deref(), rect, scale, hide_cursor)
                        .await?;
                let img = capturer.capture_region(req).await?;
                let result = ocr
                    .recognise(OCRRequest {
                        image: img,
                        languages,
                        use_language_correction: language_correction,
                    })
                    .await?;
                write_text(&result.text, &output, stdout)?;
            }
            Command::Ocr {
                input,
                output,
                languages,
                language_correction,
            } => {
                let img = read_png(&input)?;
                let result = ocr
                    .recognise(OCRRequest {
                        image: img,
                        languages,
                        use_language_correction: language_correction,
                    })
                    .await?;
                write_text(&result.text, &output, stdout)?;
            }
            Command::CaptureAndOcr {
                display,
                rect,
                scale,
                hide_cursor,
                output,
                also_image,
                languages,
                language_correction,
            } => {
                let req =
                    build_capture_request(&*capturer, display.as_deref(), rect, scale, hide_cursor)
                        .await?;
                let img = capturer.capture_region(req).await?;
                if let Some(path) = also_image.as_deref() {
                    write_png(&img, path, stdout)?;
                }
                let result = ocr
                    .recognise(OCRRequest {
                        image: img,
                        languages,
                        use_language_correction: language_correction,
                    })
                    .await?;
                write_text(&result.text, &output, stdout)?;
            }
        }
        Ok(())
    }
}

async fn build_capture_request(
    capturer: &dyn Capturer,
    display: Option<&str>,
    rect: Option<Rect>,
    scale: Option<f32>,
    hide_cursor: bool,
) -> Result<CaptureRequest, CliError> {
    let displays = capturer.list_displays().await?;
    if displays.is_empty() {
        return Err(CliError::NoDisplays);
    }
    let chosen = match display {
        Some(id) => displays
            .iter()
            .find(|d| d.id == id)
            .ok_or_else(|| CliError::DisplayNotFound(id.to_string()))?,
        None => displays
            .iter()
            .find(|d| d.is_primary)
            .unwrap_or(&displays[0]),
    };
    let rect = rect.unwrap_or(chosen.bounds);
    let scale = scale.unwrap_or(chosen.scale);
    Ok(CaptureRequest {
        display_id: chosen.id.clone(),
        rect,
        scale,
        hide_cursor,
    })
}

fn write_displays_table(out: &mut dyn Write, displays: &[DisplayInfo]) -> std::io::Result<()> {
    writeln!(
        out,
        "{:<24}{:<8}{:<24}{:<6}name",
        "id", "scale", "bounds (x,y,w,h)", "prim"
    )?;
    for d in displays {
        writeln!(
            out,
            "{:<24}{:<8.2}{:<24}{:<6}{}",
            d.id,
            d.scale,
            format!(
                "{},{},{},{}",
                d.bounds.x() as i32,
                d.bounds.y() as i32,
                d.bounds.width() as i32,
                d.bounds.height() as i32,
            ),
            if d.is_primary { "yes" } else { "" },
            d.name,
        )?;
    }
    Ok(())
}

fn write_displays_json(out: &mut dyn Write, displays: &[DisplayInfo]) -> std::io::Result<()> {
    let payload: Vec<_> = displays
        .iter()
        .map(|d| {
            serde_json::json!({
                "id": d.id,
                "name": d.name,
                "is_primary": d.is_primary,
                "scale": d.scale,
                "bounds": {
                    "x": d.bounds.x(),
                    "y": d.bounds.y(),
                    "width": d.bounds.width(),
                    "height": d.bounds.height(),
                },
            })
        })
        .collect();
    let s = serde_json::to_string_pretty(&payload).expect("serde_json on a Vec cannot fail");
    out.write_all(s.as_bytes())?;
    out.write_all(b"\n")?;
    Ok(())
}

fn write_windows_table(out: &mut dyn Write, windows: &[WindowInfo]) -> std::io::Result<()> {
    writeln!(
        out,
        "{:<18}{:<18}{:<18}{:<22}title",
        "id", "display", "bounds (x,y,w,h)", "app"
    )?;
    for window in windows {
        writeln!(
            out,
            "{:<18}{:<18}{:<18}{:<22}{}",
            window.id.0,
            window.display_id,
            format!(
                "{},{},{},{}",
                window.bounds.x() as i32,
                window.bounds.y() as i32,
                window.bounds.width() as i32,
                window.bounds.height() as i32,
            ),
            window.app_name,
            window.title,
        )?;
    }
    Ok(())
}

fn write_windows_json(out: &mut dyn Write, windows: &[WindowInfo]) -> std::io::Result<()> {
    let payload: Vec<_> = windows
        .iter()
        .map(|window| {
            serde_json::json!({
                "id": window.id.0,
                "title": window.title,
                "app_name": window.app_name,
                "display_id": window.display_id,
                "bounds": {
                    "x": window.bounds.x(),
                    "y": window.bounds.y(),
                    "width": window.bounds.width(),
                    "height": window.bounds.height(),
                },
            })
        })
        .collect();
    let s = serde_json::to_string_pretty(&payload).expect("serde_json on a Vec cannot fail");
    out.write_all(s.as_bytes())?;
    out.write_all(b"\n")?;
    Ok(())
}

fn write_png(
    img: &RgbaImage,
    path: &std::path::Path,
    stdout: &mut dyn Write,
) -> Result<(), CliError> {
    if path == std::path::Path::new("-") {
        let buf = readshot_core::encode_png(img)?;
        stdout.write_all(&buf)?;
    } else {
        readshot_core::save_png(img, path)?;
    }
    Ok(())
}

fn read_png(path: &std::path::Path) -> Result<RgbaImage, CliError> {
    if path == std::path::Path::new("-") {
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut std::io::stdin(), &mut buf)?;
        let dyn_img = image::load_from_memory(&buf)?;
        Ok(dyn_img.to_rgba8())
    } else {
        let dyn_img = image::open(path)?;
        Ok(dyn_img.to_rgba8())
    }
}

fn write_text(text: &str, path: &std::path::Path, stdout: &mut dyn Write) -> Result<(), CliError> {
    if path == std::path::Path::new("-") {
        stdout.write_all(text.as_bytes())?;
        if !text.ends_with('\n') {
            stdout.write_all(b"\n")?;
        }
    } else {
        std::fs::write(path, text)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};
    use readshot_capture::fake::FakeCapturer;
    use readshot_ocr::fake::FakeOcrEngine;

    fn fakes() -> (Arc<dyn Capturer>, Arc<dyn OCREngine>) {
        (
            Arc::new(FakeCapturer::new()),
            Arc::new(FakeOcrEngine::with_text("hello world")),
        )
    }

    #[test]
    fn parse_rect_accepts_well_formed_input() {
        let r = parse_rect("0,0,800,600").unwrap();
        assert_eq!(r.width(), 800.0);
        assert_eq!(r.height(), 600.0);
    }

    #[test]
    fn parse_rect_rejects_wrong_arity() {
        let err = parse_rect("0,0,800").unwrap_err();
        assert!(err.contains("expected x,y,width,height"));
    }

    #[test]
    fn parse_rect_rejects_non_numeric_component() {
        let err = parse_rect("0,0,abc,600").unwrap_err();
        assert!(err.contains("not a number"));
    }

    #[test]
    fn parse_rect_rejects_negative_size() {
        let err = parse_rect("0,0,-10,600").unwrap_err();
        assert!(err.contains("non-positive") || err.contains("non-finite"));
    }

    #[test]
    fn parses_list_displays_subcommand() {
        let cli = Cli::try_parse_from(["readshot", "list-displays"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::ListDisplays { json: false })
        ));
    }

    #[test]
    fn parses_list_displays_json_flag() {
        let cli = Cli::try_parse_from(["readshot", "list-displays", "--json"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::ListDisplays { json: true })
        ));
    }

    #[test]
    fn parses_list_windows_subcommand() {
        let cli = Cli::try_parse_from(["readshot", "list-windows"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::ListWindows { json: false })
        ));
    }

    #[test]
    fn parses_capture_subcommand() {
        let cli = Cli::try_parse_from([
            "readshot",
            "capture",
            "--display",
            "fake-0",
            "--rect",
            "0,0,128,128",
            "-o",
            "out.png",
        ])
        .unwrap();
        let Some(Command::Capture {
            display,
            rect,
            output,
            ..
        }) = cli.command
        else {
            panic!("wrong subcommand");
        };
        assert_eq!(display.as_deref(), Some("fake-0"));
        assert_eq!(rect.unwrap().width(), 128.0);
        assert_eq!(output, std::path::PathBuf::from("out.png"));
    }

    #[test]
    fn parses_capture_window_subcommand() {
        let cli = Cli::try_parse_from([
            "readshot",
            "capture-window",
            "--window",
            "fake-window-0",
            "-o",
            "window.png",
        ])
        .unwrap();
        let Some(Command::CaptureWindow { window, output }) = cli.command else {
            panic!("wrong subcommand");
        };
        assert_eq!(window, "fake-window-0");
        assert_eq!(output, std::path::PathBuf::from("window.png"));
    }

    #[test]
    fn parses_capture_text_subcommand() {
        let cli = Cli::try_parse_from(["readshot", "capture-text"]).unwrap();
        assert!(matches!(cli.command, Some(Command::CaptureText { .. })));
    }

    #[test]
    fn capture_text_help_describes_cli_ocr_flow() {
        let mut command = Cli::command();
        let help = command
            .find_subcommand_mut("capture-text")
            .expect("capture-text subcommand exists")
            .render_long_help()
            .to_string();

        assert!(help.contains("Capture a region, run OCR"));
        assert!(help.contains("write recognised text to stdout"));
    }

    #[test]
    fn parses_capture_last_region_flag() {
        let cli = Cli::try_parse_from(["readshot", "capture", "--last-region"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Capture {
                last_region: true,
                ..
            })
        ));
    }

    #[test]
    fn capture_help_hides_gui_only_last_region_flag() {
        let mut command = Cli::command();
        let help = command
            .find_subcommand_mut("capture")
            .expect("capture subcommand exists")
            .render_long_help()
            .to_string();

        assert!(!help.contains("--last-region"));
    }

    #[test]
    fn parses_ocr_subcommand_with_languages() {
        let cli = Cli::try_parse_from([
            "readshot",
            "ocr",
            "-i",
            "in.png",
            "--languages",
            "en-US,fr-FR",
        ])
        .unwrap();
        let Some(Command::Ocr {
            input,
            languages,
            output,
            ..
        }) = cli.command
        else {
            panic!("wrong subcommand");
        };
        assert_eq!(input, std::path::PathBuf::from("in.png"));
        assert_eq!(languages, vec!["en-US".to_string(), "fr-FR".to_string()]);
        assert_eq!(output, std::path::PathBuf::from("-"));
    }

    #[test]
    fn parses_capture_and_ocr_with_also_image() {
        let cli = Cli::try_parse_from([
            "readshot",
            "capture-and-ocr",
            "--rect",
            "0,0,32,32",
            "--also-image",
            "shot.png",
        ])
        .unwrap();
        assert!(matches!(cli.command, Some(Command::CaptureAndOcr { .. })));
    }

    #[test]
    fn parses_no_subcommand_for_gui_fallback() {
        let cli = Cli::try_parse_from(["readshot"]).unwrap();
        assert!(cli.command.is_none());
    }

    #[tokio::test]
    async fn run_list_displays_table_writes_fake_display() {
        let (cap, ocr) = fakes();
        let cli = Cli::try_parse_from(["readshot", "list-displays"]).unwrap();
        let mut out = Vec::new();
        cli.run(cap, ocr, &mut out).await.unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("fake-0"));
        assert!(text.contains("id"));
    }

    #[tokio::test]
    async fn run_list_displays_json_emits_valid_json() {
        let (cap, ocr) = fakes();
        let cli = Cli::try_parse_from(["readshot", "list-displays", "--json"]).unwrap();
        let mut out = Vec::new();
        cli.run(cap, ocr, &mut out).await.unwrap();
        let text = String::from_utf8(out).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let arr = value.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["id"], "fake-0");
    }

    #[tokio::test]
    async fn run_list_windows_table_writes_fake_window() {
        let (cap, ocr) = fakes();
        let cli = Cli::try_parse_from(["readshot", "list-windows"]).unwrap();
        let mut out = Vec::new();
        cli.run(cap, ocr, &mut out).await.unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("fake-window-0"));
        assert!(text.contains("Fake Window"));
        assert!(text.contains("Readshot Test"));
    }

    #[tokio::test]
    async fn run_list_windows_json_emits_valid_json() {
        let (cap, ocr) = fakes();
        let cli = Cli::try_parse_from(["readshot", "list-windows", "--json"]).unwrap();
        let mut out = Vec::new();
        cli.run(cap, ocr, &mut out).await.unwrap();
        let text = String::from_utf8(out).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let arr = value.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["id"], "fake-window-0");
        assert_eq!(arr[0]["app_name"], "Readshot Test");
    }

    #[tokio::test]
    async fn run_capture_writes_png_to_file() {
        let dir = tempfile::tempdir().unwrap();
        let png_path = dir.path().join("out.png");
        let (cap, ocr) = fakes();
        let cli = Cli::try_parse_from([
            "readshot",
            "capture",
            "--rect",
            "0,0,64,64",
            "-o",
            png_path.to_str().unwrap(),
        ])
        .unwrap();
        let mut out = Vec::new();
        cli.run(cap, ocr, &mut out).await.unwrap();

        let bytes = std::fs::read(&png_path).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[tokio::test]
    async fn run_capture_window_writes_png_to_file() {
        let dir = tempfile::tempdir().unwrap();
        let png_path = dir.path().join("window.png");
        let (cap, ocr) = fakes();
        let cli = Cli::try_parse_from([
            "readshot",
            "capture-window",
            "--window",
            "fake-window-0",
            "-o",
            png_path.to_str().unwrap(),
        ])
        .unwrap();
        let mut out = Vec::new();
        cli.run(cap, ocr, &mut out).await.unwrap();

        let bytes = std::fs::read(&png_path).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[tokio::test]
    async fn run_ocr_reads_png_and_writes_recognised_text() {
        let dir = tempfile::tempdir().unwrap();
        let png_path = dir.path().join("in.png");
        let img = image::RgbaImage::new(8, 8);
        readshot_core::save_png(&img, &png_path).unwrap();

        let (cap, ocr) = fakes();
        let cli =
            Cli::try_parse_from(["readshot", "ocr", "-i", png_path.to_str().unwrap()]).unwrap();
        let mut out = Vec::new();
        cli.run(cap, ocr, &mut out).await.unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("hello world"));
    }

    #[tokio::test]
    async fn run_capture_and_ocr_emits_text_and_optional_png() {
        let dir = tempfile::tempdir().unwrap();
        let png_path = dir.path().join("shot.png");
        let (cap, ocr) = fakes();
        let cli = Cli::try_parse_from([
            "readshot",
            "capture-and-ocr",
            "--rect",
            "0,0,64,64",
            "--also-image",
            png_path.to_str().unwrap(),
        ])
        .unwrap();
        let mut out = Vec::new();
        cli.run(cap, ocr, &mut out).await.unwrap();

        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("hello world"));
        assert!(png_path.exists());
    }

    #[tokio::test]
    async fn run_capture_text_emits_recognised_text() {
        let (cap, ocr) = fakes();
        let cli = Cli::try_parse_from(["readshot", "capture-text", "--rect", "0,0,64,64"]).unwrap();
        let mut out = Vec::new();
        cli.run(cap, ocr, &mut out).await.unwrap();

        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("hello world"));
    }

    #[tokio::test]
    async fn run_capture_last_region_returns_invalid_input() {
        let (cap, ocr) = fakes();
        let cli = Cli::try_parse_from(["readshot", "capture", "--last-region"]).unwrap();
        let mut out = Vec::new();
        let err = cli.run(cap, ocr, &mut out).await.unwrap_err();

        assert!(matches!(err, CliError::InvalidInput(ref s) if s.contains("--last-region")));
        assert_eq!(exit_code(&err), 64);
    }

    #[tokio::test]
    async fn run_capture_with_unknown_display_returns_display_not_found() {
        let (cap, ocr) = fakes();
        let cli = Cli::try_parse_from([
            "readshot",
            "capture",
            "--display",
            "no-such-display",
            "-o",
            "/tmp/ignored.png",
        ])
        .unwrap();
        let mut out = Vec::new();
        let err = cli.run(cap, ocr, &mut out).await.unwrap_err();
        assert!(matches!(err, CliError::DisplayNotFound(ref s) if s == "no-such-display"));
        assert_eq!(exit_code(&err), 66);
    }

    #[tokio::test]
    async fn run_capture_window_with_unknown_window_returns_window_not_found() {
        let (cap, ocr) = fakes();
        let cli = Cli::try_parse_from([
            "readshot",
            "capture-window",
            "--window",
            "no-such-window",
            "-o",
            "/tmp/ignored.png",
        ])
        .unwrap();
        let mut out = Vec::new();
        let err = cli.run(cap, ocr, &mut out).await.unwrap_err();

        assert!(matches!(
            err,
            CliError::Capture(CaptureError::WindowNotFound(ref s)) if s == "no-such-window"
        ));
        assert_eq!(exit_code(&err), 66);
    }

    #[test]
    fn exit_code_maps_permission_to_77() {
        let err = CliError::Capture(CaptureError::PermissionDenied);
        assert_eq!(exit_code(&err), 77);
    }
}
