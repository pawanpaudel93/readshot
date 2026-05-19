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
//! Permission gating differs from the GUI: headless captures inherit
//! the invoking user's TCC / portal grants, so the call simply fails
//! with `CaptureError::PermissionDenied` if the host hasn't been
//! granted Screen Recording. Interactive CLI captures are explicit
//! user-selection flows.
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
//! * `capture` writes an image to `--output` (or `stdout` by default / `-`).
//!   Add `--interactive` to pick a region with the Readshot overlay.
//! * `capture-window` captures one window by id and writes an image. Add
//!   `--rect x,y,w,h` to crop a region relative to the window's top-left.
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

use std::borrow::Cow;
use std::io::{Cursor, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use image::RgbaImage;
use readshot_capture::{
    crop_window_relative_rect, CaptureRequest, Capturer, DisplayInfo, WindowCaptureRequest,
    WindowId, WindowInfo,
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
    about = "Cross-platform screenshot + offline OCR. Run with no args for the GUI.",
    long_about = None
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ImageFormatChoice {
    Png,
    #[value(alias = "jpeg")]
    Jpg,
    Tiff,
    Webp,
}

impl ImageFormatChoice {
    fn image_format(self) -> image::ImageFormat {
        match self {
            Self::Png => image::ImageFormat::Png,
            Self::Jpg => image::ImageFormat::Jpeg,
            Self::Tiff => image::ImageFormat::Tiff,
            Self::Webp => image::ImageFormat::WebP,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpg => "jpg",
            Self::Tiff => "tiff",
            Self::Webp => "webp",
        }
    }
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

    /// Capture a region (or full display) and write the image to a file.
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

        /// Use the Readshot overlay to select a region interactively.
        #[arg(long, conflicts_with_all = ["display", "rect", "scale"])]
        interactive: bool,

        /// Hide the cursor during capture. Default: true.
        #[arg(long, default_value_t = true)]
        hide_cursor: bool,

        /// Include the cursor in the captured image.
        #[arg(long, conflicts_with = "hide_cursor")]
        show_cursor: bool,

        /// Wait before capture, in seconds.
        #[arg(long, default_value_t = 0.0, value_parser = parse_delay)]
        delay: f64,

        /// Output image format.
        #[arg(long, value_enum, default_value_t = ImageFormatChoice::Png)]
        format: ImageFormatChoice,

        /// Copy the captured image to the clipboard instead of writing output.
        #[arg(long)]
        clipboard: bool,

        /// Emit capture metadata as JSON to stdout. Requires `--output PATH` or `--clipboard`.
        #[arg(long)]
        json: bool,

        /// Output image path. Use `-` for stdout.
        #[arg(long, short = 'o', default_value = "-", conflicts_with = "clipboard")]
        output: PathBuf,
    },

    /// Capture one window and write the image to a file.
    CaptureWindow {
        /// Target window id, as printed by `list-windows`.
        #[arg(long)]
        window: String,

        /// Window-relative region in logical pixels: `x,y,width,height`.
        /// Default: the full window.
        #[arg(long, value_parser = parse_rect)]
        rect: Option<Rect>,

        /// Wait before capture, in seconds.
        #[arg(long, default_value_t = 0.0, value_parser = parse_delay)]
        delay: f64,

        /// Output image format.
        #[arg(long, value_enum, default_value_t = ImageFormatChoice::Png)]
        format: ImageFormatChoice,

        /// Copy the captured image to the clipboard instead of writing output.
        #[arg(long)]
        clipboard: bool,

        /// Emit capture metadata as JSON to stdout. Requires `--output PATH` or `--clipboard`.
        #[arg(long)]
        json: bool,

        /// Include the native window shadow when the platform supports it.
        #[arg(long, conflicts_with = "no_window_shadow")]
        window_shadow: bool,

        /// Omit the native window shadow when the platform supports it.
        #[arg(long)]
        no_window_shadow: bool,

        /// Output image path. Use `-` for stdout.
        #[arg(long, short = 'o', default_value = "-", conflicts_with = "clipboard")]
        output: PathBuf,
    },

    /// Capture a region, run OCR, and write recognised text to stdout.
    CaptureText {
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

        /// Use the Readshot overlay to select a region interactively.
        #[arg(long, conflicts_with_all = ["display", "rect", "scale"])]
        interactive: bool,

        /// Hide the cursor during capture. Default: true.
        #[arg(long, default_value_t = true)]
        hide_cursor: bool,

        /// Include the cursor in the captured image.
        #[arg(long, conflicts_with = "hide_cursor")]
        show_cursor: bool,

        /// Wait before capture, in seconds.
        #[arg(long, default_value_t = 0.0, value_parser = parse_delay)]
        delay: f64,

        /// Copy recognised text to the clipboard instead of writing output.
        #[arg(long)]
        clipboard: bool,

        /// Emit OCR result and capture metadata as JSON.
        #[arg(long, conflicts_with = "clipboard")]
        json: bool,

        /// Output text path. Use `-` for stdout (the default).
        #[arg(long, short = 'o', default_value = "-", conflicts_with = "clipboard")]
        output: PathBuf,

        /// BCP-47 language hints, comma-separated. Empty → automatic detection.
        #[arg(long, value_delimiter = ',')]
        languages: Vec<String>,

        /// Apply post-recognition language correction when supported.
        #[arg(long, default_value_t = true)]
        language_correction: bool,
    },

    /// Recognise text in an existing image.
    Ocr {
        /// Input image path. Use `-` for stdin.
        #[arg(long, short = 'i')]
        input: PathBuf,

        /// Output text path. Use `-` for stdout (the default).
        #[arg(long, short = 'o', default_value = "-", conflicts_with = "clipboard")]
        output: PathBuf,

        /// Copy recognised text to the clipboard instead of writing output.
        #[arg(long)]
        clipboard: bool,

        /// Emit OCR result metadata as JSON.
        #[arg(long, conflicts_with = "clipboard")]
        json: bool,

        /// BCP-47 language hints, comma-separated. Empty → engine default.
        #[arg(long, value_delimiter = ',')]
        languages: Vec<String>,

        /// Apply post-recognition language correction when supported.
        #[arg(long, default_value_t = true)]
        language_correction: bool,
    },

    /// Capture a region and recognise text in one shot.
    CaptureAndOcr {
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

        /// Use the Readshot overlay to select a region interactively.
        #[arg(long, conflicts_with_all = ["display", "rect", "scale"])]
        interactive: bool,

        /// Hide the cursor during capture. Default: true.
        #[arg(long, default_value_t = true)]
        hide_cursor: bool,

        /// Include the cursor in the captured image.
        #[arg(long, conflicts_with = "hide_cursor")]
        show_cursor: bool,

        /// Wait before capture, in seconds.
        #[arg(long, default_value_t = 0.0, value_parser = parse_delay)]
        delay: f64,

        /// Output text path. Use `-` for stdout (the default).
        #[arg(long, short = 'o', default_value = "-")]
        output: PathBuf,

        /// Emit OCR result and capture metadata as JSON.
        #[arg(long)]
        json: bool,

        /// Optionally also write the captured PNG to this path.
        #[arg(long)]
        also_image: Option<PathBuf>,

        /// BCP-47 language hints, comma-separated. Empty → automatic detection.
        #[arg(long, value_delimiter = ',')]
        languages: Vec<String>,

        /// Apply post-recognition language correction when supported.
        #[arg(long, default_value_t = true)]
        language_correction: bool,
    },

    /// Print a ready-to-paste MCP stdio configuration snippet.
    McpConfig {
        /// Server name to use in the host config.
        #[arg(long, default_value = "readshot")]
        name: String,

        /// Command used by the host to launch the MCP server.
        #[arg(long, default_value = "readshot-mcp")]
        command: String,
    },

    /// Generate shell completion scripts.
    Completions {
        /// Shell to generate completions for.
        #[arg(value_enum)]
        shell: clap_complete::Shell,
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
    #[error("clipboard failed: {0}")]
    Clipboard(String),
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
        CliError::Capture(CaptureError::InvalidRegion(_)) => 64, // EX_USAGE
        CliError::Capture(_) => 71,                              // EX_OSERR
        CliError::Ocr(_) => 70,                                  // EX_SOFTWARE
        CliError::DisplayNotFound(_) | CliError::NoDisplays => 66, // EX_NOINPUT
        CliError::InvalidInput(_) | CliError::InvalidRect { .. } => 64, // EX_USAGE
        CliError::Io(_) | CliError::Image(_) | CliError::Clipboard(_) => 74, // EX_IOERR
    }
}

/// Largest `--delay` we accept, in seconds. Beyond this the value is
/// almost certainly user error; capping also avoids passing absurd
/// values to `Duration::from_secs_f64`, which panics near
/// `Duration::MAX`.
const MAX_DELAY_SECS: f64 = 3600.0;

fn parse_delay(s: &str) -> Result<f64, String> {
    let delay = s
        .parse::<f64>()
        .map_err(|e| format!("delay `{s}` is not a number: {e}"))?;
    if !delay.is_finite() || delay < 0.0 {
        return Err("--delay must be a non-negative finite number".into());
    }
    if delay > MAX_DELAY_SECS {
        return Err(format!("--delay must be <= {MAX_DELAY_SECS} seconds"));
    }
    Ok(delay)
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
        self.run_with_clipboard(capturer, ocr, stdout, &SystemClipboard)
            .await
    }

    async fn run_with_clipboard(
        self,
        capturer: Arc<dyn Capturer>,
        ocr: Arc<dyn OCREngine>,
        stdout: &mut dyn Write,
        clipboard: &dyn ClipboardSink,
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
                interactive,
                hide_cursor,
                show_cursor,
                delay,
                format,
                clipboard: use_clipboard,
                json,
                output,
            } => {
                ensure_image_json_has_destination(json, use_clipboard, &output)?;
                maybe_delay(delay).await;
                let (img, source) = if interactive {
                    (
                        interactive_capture(show_cursor).await?,
                        serde_json::json!({
                            "type": "interactive",
                            "show_cursor": show_cursor,
                        }),
                    )
                } else {
                    let req = build_capture_request(
                        &*capturer,
                        display.as_deref(),
                        rect,
                        scale,
                        effective_hide_cursor(hide_cursor, show_cursor),
                    )
                    .await?;
                    let source = capture_request_json(&req);
                    (capturer.capture_region(req).await?, source)
                };
                if use_clipboard {
                    clipboard.copy_image(&img)?;
                } else {
                    write_image(&img, &output, stdout, format)?;
                }
                if json {
                    write_json_value(
                        stdout,
                        capture_json("capture", &img, format, &source, use_clipboard, &output),
                    )?;
                }
            }
            Command::CaptureWindow {
                window,
                rect,
                delay,
                format,
                clipboard: use_clipboard,
                json,
                window_shadow: _,
                no_window_shadow,
                output,
            } => {
                ensure_image_json_has_destination(json, use_clipboard, &output)?;
                maybe_delay(delay).await;
                let window_id = WindowId(window);
                let mut img = capturer
                    .capture_window(WindowCaptureRequest {
                        window_id: window_id.clone(),
                        ignore_shadows: no_window_shadow,
                    })
                    .await?;
                if let Some(rect) = rect {
                    let window_bounds = lookup_window_bounds(&*capturer, &window_id).await?;
                    img = crop_window_relative_rect(img, window_bounds, rect)?;
                }
                if use_clipboard {
                    clipboard.copy_image(&img)?;
                } else {
                    write_image(&img, &output, stdout, format)?;
                }
                if json {
                    write_json_value(
                        stdout,
                        capture_json(
                            "capture-window",
                            &img,
                            format,
                            &serde_json::json!({
                                "type": "window",
                                "window_id": window_id.0,
                                "rect": rect.map(rect_json),
                                "ignore_shadows": no_window_shadow,
                            }),
                            use_clipboard,
                            &output,
                        ),
                    )?;
                }
            }
            Command::CaptureText {
                display,
                rect,
                scale,
                interactive,
                hide_cursor,
                show_cursor,
                delay,
                clipboard: use_clipboard,
                json,
                output,
                languages,
                language_correction,
            } => {
                maybe_delay(delay).await;
                let (img, source) = if interactive {
                    (
                        interactive_capture(show_cursor).await?,
                        serde_json::json!({
                            "type": "interactive",
                            "show_cursor": show_cursor,
                        }),
                    )
                } else {
                    let req = build_capture_request(
                        &*capturer,
                        display.as_deref(),
                        rect,
                        scale,
                        effective_hide_cursor(hide_cursor, show_cursor),
                    )
                    .await?;
                    let source = capture_request_json(&req);
                    (capturer.capture_region(req).await?, source)
                };
                let image_size = (img.width(), img.height());
                let result = ocr
                    .recognise(OCRRequest {
                        image: img,
                        languages: languages.clone(),
                        use_language_correction: language_correction,
                    })
                    .await?;
                if use_clipboard {
                    clipboard.copy_text(&result.text)?;
                } else if json {
                    write_json(
                        &output,
                        stdout,
                        ocr_json(
                            "capture-text",
                            &result,
                            Some(image_size),
                            Some(&source),
                            None,
                            &languages,
                            language_correction,
                        ),
                    )?;
                } else {
                    write_text(&result.text, &output, stdout)?;
                }
            }
            Command::Ocr {
                input,
                output,
                clipboard: use_clipboard,
                json,
                languages,
                language_correction,
            } => {
                let img = read_image(&input)?;
                let image_size = (img.width(), img.height());
                let result = ocr
                    .recognise(OCRRequest {
                        image: img,
                        languages: languages.clone(),
                        use_language_correction: language_correction,
                    })
                    .await?;
                if use_clipboard {
                    clipboard.copy_text(&result.text)?;
                } else if json {
                    write_json(
                        &output,
                        stdout,
                        ocr_json(
                            "ocr",
                            &result,
                            Some(image_size),
                            None,
                            Some(&input),
                            &languages,
                            language_correction,
                        ),
                    )?;
                } else {
                    write_text(&result.text, &output, stdout)?;
                }
            }
            Command::CaptureAndOcr {
                display,
                rect,
                scale,
                interactive,
                hide_cursor,
                show_cursor,
                delay,
                output,
                json,
                also_image,
                languages,
                language_correction,
            } => {
                ensure_combined_json_has_clean_stdout(json, also_image.as_deref())?;
                maybe_delay(delay).await;
                let (img, source) = if interactive {
                    (
                        interactive_capture(show_cursor).await?,
                        serde_json::json!({
                            "type": "interactive",
                            "show_cursor": show_cursor,
                        }),
                    )
                } else {
                    let req = build_capture_request(
                        &*capturer,
                        display.as_deref(),
                        rect,
                        scale,
                        effective_hide_cursor(hide_cursor, show_cursor),
                    )
                    .await?;
                    let source = capture_request_json(&req);
                    (capturer.capture_region(req).await?, source)
                };
                let image_size = (img.width(), img.height());
                if let Some(path) = also_image.as_deref() {
                    write_image(&img, path, stdout, ImageFormatChoice::Png)?;
                }
                let result = ocr
                    .recognise(OCRRequest {
                        image: img,
                        languages: languages.clone(),
                        use_language_correction: language_correction,
                    })
                    .await?;
                if json {
                    write_json(
                        &output,
                        stdout,
                        ocr_json(
                            "capture-and-ocr",
                            &result,
                            Some(image_size),
                            Some(&source),
                            None,
                            &languages,
                            language_correction,
                        ),
                    )?;
                } else {
                    write_text(&result.text, &output, stdout)?;
                }
            }
            Command::McpConfig { name, command } => {
                write_mcp_config(stdout, &name, &command)?;
            }
            Command::Completions { shell } => {
                write_completions(stdout, shell)?;
            }
        }
        Ok(())
    }
}

trait ClipboardSink {
    fn copy_image(&self, img: &RgbaImage) -> Result<(), CliError>;
    fn copy_text(&self, text: &str) -> Result<(), CliError>;
}

struct SystemClipboard;

impl ClipboardSink for SystemClipboard {
    fn copy_image(&self, img: &RgbaImage) -> Result<(), CliError> {
        let mut ctx = arboard::Clipboard::new().map_err(clipboard_error)?;
        ctx.set_image(arboard::ImageData {
            width: img.width() as usize,
            height: img.height() as usize,
            bytes: Cow::Borrowed(img.as_raw()),
        })
        .map_err(clipboard_error)
    }

    fn copy_text(&self, text: &str) -> Result<(), CliError> {
        let mut ctx = arboard::Clipboard::new().map_err(clipboard_error)?;
        ctx.set_text(text.to_string()).map_err(clipboard_error)
    }
}

fn clipboard_error(err: arboard::Error) -> CliError {
    CliError::Clipboard(err.to_string())
}

pub fn parse_internal_interactive_command(
    argv: &[String],
) -> Result<Option<(PathBuf, bool)>, CliError> {
    if argv.get(1).map(String::as_str) != Some("__interactive-capture") {
        return Ok(None);
    }

    let mut output = None;
    let mut show_cursor = false;
    let mut args = argv.iter().skip(2);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--output" => {
                let Some(value) = args.next() else {
                    return Err(CliError::InvalidInput(
                        "__interactive-capture requires --output PATH".into(),
                    ));
                };
                output = Some(PathBuf::from(value));
            }
            "--show-cursor" => show_cursor = true,
            other => {
                return Err(CliError::InvalidInput(format!(
                    "unknown __interactive-capture argument: {other}"
                )));
            }
        }
    }

    let Some(output) = output else {
        return Err(CliError::InvalidInput(
            "__interactive-capture requires --output PATH".into(),
        ));
    };
    Ok(Some((output, show_cursor)))
}

fn effective_hide_cursor(hide_cursor: bool, show_cursor: bool) -> bool {
    hide_cursor && !show_cursor
}

async fn maybe_delay(delay: f64) {
    if delay > 0.0 {
        tokio::time::sleep(Duration::from_secs_f64(delay)).await;
    }
}

async fn interactive_capture(show_cursor: bool) -> Result<RgbaImage, CliError> {
    tokio::task::spawn_blocking(move || interactive_capture_blocking(show_cursor))
        .await
        .map_err(|e| CliError::Capture(CaptureError::Backend(e.to_string())))?
}

fn interactive_capture_blocking(show_cursor: bool) -> Result<RgbaImage, CliError> {
    cleanup_stale_interactive_temp_files();
    let path = unique_temp_png_path();
    let _ = std::fs::remove_file(&path);
    let exe = std::env::current_exe()?;
    let mut command = std::process::Command::new(exe);
    for arg in interactive_capture_child_args(&path, show_cursor) {
        command.arg(arg);
    }
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    let status = command.status()?;
    if !status.success() {
        let _ = std::fs::remove_file(&path);
        return Err(interactive_capture_cancelled());
    }

    let result = read_interactive_capture_file(&path);
    let _ = std::fs::remove_file(&path);
    result
}

fn interactive_capture_child_args(
    path: &std::path::Path,
    show_cursor: bool,
) -> Vec<std::ffi::OsString> {
    let mut args = vec![
        std::ffi::OsString::from("__interactive-capture"),
        std::ffi::OsString::from("--output"),
        path.as_os_str().to_owned(),
    ];
    if show_cursor {
        args.push(std::ffi::OsString::from("--show-cursor"));
    }
    args
}

fn unique_temp_png_path() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!(
        "readshot-interactive-{}-{nanos}.png",
        std::process::id()
    ))
}

fn cleanup_stale_interactive_temp_files() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    let stale_before = SystemTime::now()
        .checked_sub(Duration::from_secs(24 * 60 * 60))
        .unwrap_or(SystemTime::UNIX_EPOCH);
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("readshot-interactive-") || !name.ends_with(".png") {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if modified < stale_before {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn read_interactive_capture_file(path: &std::path::Path) -> Result<RgbaImage, CliError> {
    let metadata = std::fs::metadata(path).map_err(|_| interactive_capture_cancelled())?;
    if metadata.len() == 0 {
        return Err(interactive_capture_cancelled());
    }
    image::open(path)
        .map(|img| img.to_rgba8())
        .map_err(CliError::from)
}

fn interactive_capture_cancelled() -> CliError {
    CliError::InvalidInput("interactive capture cancelled".into())
}

async fn build_capture_request(
    capturer: &dyn Capturer,
    display: Option<&str>,
    rect: Option<Rect>,
    scale: Option<f32>,
    hide_cursor: bool,
) -> Result<CaptureRequest, CliError> {
    if let Some(scale) = scale {
        validate_scale(scale)?;
    }
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
    validate_scale(scale)?;
    Ok(CaptureRequest {
        display_id: chosen.id.clone(),
        rect,
        scale,
        hide_cursor,
    })
}

fn validate_scale(scale: f32) -> Result<(), CliError> {
    if scale.is_finite() && scale > 0.0 {
        Ok(())
    } else {
        Err(CliError::InvalidInput(
            "--scale must be a positive finite number".into(),
        ))
    }
}

async fn lookup_window_bounds(
    capturer: &dyn Capturer,
    window_id: &WindowId,
) -> Result<Rect, CliError> {
    let windows = capturer.list_windows().await?;
    windows
        .into_iter()
        .find(|window| window.id == *window_id)
        .map(|window| window.bounds)
        .ok_or_else(|| CliError::Capture(CaptureError::WindowNotFound(window_id.0.clone())))
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

fn ensure_image_json_has_destination(
    json: bool,
    clipboard: bool,
    output: &std::path::Path,
) -> Result<(), CliError> {
    if json && !clipboard && output == std::path::Path::new("-") {
        return Err(CliError::InvalidInput(
            "--json with image capture requires --output PATH or --clipboard".into(),
        ));
    }
    Ok(())
}

fn ensure_combined_json_has_clean_stdout(
    json: bool,
    also_image: Option<&std::path::Path>,
) -> Result<(), CliError> {
    if json && also_image == Some(std::path::Path::new("-")) {
        return Err(CliError::InvalidInput(
            "--json cannot be combined with --also-image -".into(),
        ));
    }
    Ok(())
}

fn rect_json(rect: Rect) -> serde_json::Value {
    serde_json::json!({
        "x": rect.x(),
        "y": rect.y(),
        "width": rect.width(),
        "height": rect.height(),
    })
}

fn capture_request_json(req: &CaptureRequest) -> serde_json::Value {
    serde_json::json!({
        "type": "display",
        "display_id": req.display_id,
        "rect": rect_json(req.rect),
        "scale": req.scale,
        "hide_cursor": req.hide_cursor,
    })
}

fn capture_json(
    kind: &str,
    img: &RgbaImage,
    format: ImageFormatChoice,
    source: &serde_json::Value,
    clipboard: bool,
    output: &std::path::Path,
) -> serde_json::Value {
    serde_json::json!({
        "kind": kind,
        "image": {
            "width": img.width(),
            "height": img.height(),
            "format": format.as_str(),
        },
        "source": source,
        "output": if clipboard {
            serde_json::Value::Null
        } else {
            serde_json::Value::String(output.display().to_string())
        },
        "clipboard": clipboard,
    })
}

fn ocr_json(
    kind: &str,
    result: &readshot_ocr::OCRResult,
    image_size: Option<(u32, u32)>,
    source: Option<&serde_json::Value>,
    input: Option<&std::path::Path>,
    languages: &[String],
    language_correction: bool,
) -> serde_json::Value {
    let lines: Vec<_> = result
        .lines
        .iter()
        .map(|line| {
            serde_json::json!({
                "text": line.text,
                "bounds": {
                    "x": line.x,
                    "y": line.y,
                    "width": line.w,
                    "height": line.h,
                },
            })
        })
        .collect();

    serde_json::json!({
        "kind": kind,
        "text": result.text,
        "average_confidence": result.average_confidence,
        "image": image_size.map(|(width, height)| serde_json::json!({
            "width": width,
            "height": height,
        })),
        "source": source.cloned(),
        "input": input.map(|path| path.display().to_string()),
        "languages": languages,
        "language_correction": language_correction,
        "lines": lines,
    })
}

fn write_json(
    path: &std::path::Path,
    stdout: &mut dyn Write,
    value: serde_json::Value,
) -> Result<(), CliError> {
    if path == std::path::Path::new("-") {
        write_json_value(stdout, value)?;
    } else {
        let s = serde_json::to_string_pretty(&value).expect("serde_json Value cannot fail");
        std::fs::write(path, format!("{s}\n"))?;
    }
    Ok(())
}

fn write_json_value(out: &mut dyn Write, value: serde_json::Value) -> std::io::Result<()> {
    let s = serde_json::to_string_pretty(&value).expect("serde_json Value cannot fail");
    out.write_all(s.as_bytes())?;
    out.write_all(b"\n")?;
    Ok(())
}

fn write_mcp_config(out: &mut dyn Write, name: &str, command: &str) -> std::io::Result<()> {
    let mut servers = serde_json::Map::new();
    servers.insert(
        name.to_string(),
        serde_json::json!({
            "command": command,
            "args": [],
        }),
    );
    let payload = serde_json::json!({
        "mcpServers": servers,
    });
    write_json_value(out, payload)
}

fn write_completions(out: &mut dyn Write, shell: clap_complete::Shell) -> std::io::Result<()> {
    let mut command = Cli::command();
    clap_complete::generate(shell, &mut command, "readshot", out);
    Ok(())
}

fn write_image(
    img: &RgbaImage,
    path: &std::path::Path,
    stdout: &mut dyn Write,
    format: ImageFormatChoice,
) -> Result<(), CliError> {
    let bytes = encode_image(img, format)?;
    if path == std::path::Path::new("-") {
        stdout.write_all(&bytes)?;
    } else {
        std::fs::write(path, bytes)?;
    }
    Ok(())
}

fn encode_image(img: &RgbaImage, format: ImageFormatChoice) -> Result<Vec<u8>, CliError> {
    if format == ImageFormatChoice::Png {
        return Ok(readshot_core::encode_png(img)?);
    }

    let mut cursor = Cursor::new(Vec::new());
    if format == ImageFormatChoice::Jpg {
        let rgb = rgba_to_rgb(img);
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, 90);
        encoder.encode(
            &rgb,
            img.width(),
            img.height(),
            image::ColorType::Rgb8.into(),
        )?;
    } else {
        image::DynamicImage::ImageRgba8(img.clone())
            .write_to(&mut cursor, format.image_format())?;
    }
    Ok(cursor.into_inner())
}

fn rgba_to_rgb(img: &RgbaImage) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(img.width() as usize * img.height() as usize * 3);
    for pixel in img.pixels() {
        rgb.extend_from_slice(&pixel.0[..3]);
    }
    rgb
}

fn read_image(path: &std::path::Path) -> Result<RgbaImage, CliError> {
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
    use std::sync::Mutex;

    fn fakes() -> (Arc<dyn Capturer>, Arc<dyn OCREngine>) {
        (
            Arc::new(FakeCapturer::new()),
            Arc::new(FakeOcrEngine::with_text("hello world")),
        )
    }

    #[derive(Default)]
    struct FakeClipboard {
        image_size: Mutex<Option<(u32, u32)>>,
        text: Mutex<Option<String>>,
    }

    impl FakeClipboard {
        fn image_size(&self) -> Option<(u32, u32)> {
            *self.image_size.lock().unwrap()
        }

        fn text(&self) -> Option<String> {
            self.text.lock().unwrap().clone()
        }
    }

    impl ClipboardSink for FakeClipboard {
        fn copy_image(&self, img: &RgbaImage) -> Result<(), CliError> {
            *self.image_size.lock().unwrap() = Some((img.width(), img.height()));
            Ok(())
        }

        fn copy_text(&self, text: &str) -> Result<(), CliError> {
            *self.text.lock().unwrap() = Some(text.to_string());
            Ok(())
        }
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
    fn parses_capture_with_clipboard_delay_format_and_show_cursor() {
        let cli = Cli::try_parse_from([
            "readshot",
            "capture",
            "--clipboard",
            "--delay",
            "2",
            "--format",
            "jpg",
            "--show-cursor",
        ])
        .unwrap();
        let Some(Command::Capture {
            clipboard,
            delay,
            format,
            show_cursor,
            ..
        }) = cli.command
        else {
            panic!("wrong subcommand");
        };
        assert!(clipboard);
        assert_eq!(delay, 2.0);
        assert_eq!(format, ImageFormatChoice::Jpg);
        assert!(show_cursor);
    }

    #[test]
    fn parses_capture_json_flag() {
        let cli = Cli::try_parse_from(["readshot", "capture", "--json", "--output", "capture.png"])
            .unwrap();

        assert!(matches!(
            cli.command,
            Some(Command::Capture { json: true, .. })
        ));
    }

    #[test]
    fn parses_jpeg_as_jpg_format_alias() {
        let cli = Cli::try_parse_from(["readshot", "capture", "--format", "jpeg"]).unwrap();

        assert!(matches!(
            cli.command,
            Some(Command::Capture {
                format: ImageFormatChoice::Jpg,
                ..
            })
        ));
    }

    #[test]
    fn clipboard_output_rejects_file_output() {
        let err = Cli::try_parse_from([
            "readshot",
            "capture",
            "--clipboard",
            "--output",
            "capture.png",
        ])
        .unwrap_err();

        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn parses_interactive_capture_commands() {
        let cli = Cli::try_parse_from(["readshot", "capture", "--interactive"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Capture {
                interactive: true,
                ..
            })
        ));

        let cli = Cli::try_parse_from(["readshot", "capture-text", "--interactive"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::CaptureText {
                interactive: true,
                ..
            })
        ));

        let cli = Cli::try_parse_from(["readshot", "capture-and-ocr", "--interactive"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::CaptureAndOcr {
                interactive: true,
                ..
            })
        ));
    }

    #[test]
    fn parses_internal_interactive_child_command_outside_clap() {
        let output = std::path::PathBuf::from("/tmp/readshot-interactive.png");
        let parsed = parse_internal_interactive_command(&[
            "readshot".to_string(),
            "__interactive-capture".to_string(),
            "--output".to_string(),
            output.display().to_string(),
            "--show-cursor".to_string(),
        ])
        .unwrap();

        let Some((parsed, show_cursor)) = parsed else {
            panic!("internal command should parse");
        };
        assert_eq!(parsed, output);
        assert!(show_cursor);
    }

    #[test]
    fn interactive_child_args_include_temp_output_and_cursor_flag() {
        let output = std::path::Path::new("/tmp/readshot-interactive.png");
        let args = interactive_capture_child_args(output, true);

        assert_eq!(args[0], std::ffi::OsString::from("__interactive-capture"));
        assert!(args.iter().any(|arg| arg == "--output"));
        assert!(args.iter().any(|arg| arg == "--show-cursor"));
        assert!(args.iter().any(|arg| arg == output.as_os_str()));
    }

    #[test]
    fn unique_interactive_temp_path_uses_readshot_prefix() {
        let path = unique_temp_png_path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap();

        assert!(name.starts_with("readshot-interactive-"));
        assert!(name.ends_with(".png"));
        assert_eq!(path.parent(), Some(std::env::temp_dir().as_path()));
    }

    #[test]
    fn interactive_capture_rejects_explicit_geometry() {
        let err = Cli::try_parse_from([
            "readshot",
            "capture",
            "--interactive",
            "--rect",
            "0,0,10,10",
        ])
        .unwrap_err();

        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn interactive_capture_missing_file_is_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.png");

        let err = read_interactive_capture_file(&missing).unwrap_err();

        assert!(matches!(err, CliError::InvalidInput(ref msg) if msg.contains("cancelled")));
        assert_eq!(exit_code(&err), 64);
    }

    #[test]
    fn parses_capture_window_subcommand() {
        let cli = Cli::try_parse_from([
            "readshot",
            "capture-window",
            "--window",
            "fake-window-0",
            "--rect",
            "8,12,32,24",
            "-o",
            "window.png",
        ])
        .unwrap();
        let Some(Command::CaptureWindow {
            window,
            rect,
            output,
            ..
        }) = cli.command
        else {
            panic!("wrong subcommand");
        };
        assert_eq!(window, "fake-window-0");
        let rect = rect.expect("window-relative rect should parse");
        assert_eq!(rect.x(), 8.0);
        assert_eq!(rect.y(), 12.0);
        assert_eq!(rect.width(), 32.0);
        assert_eq!(rect.height(), 24.0);
        assert_eq!(output, std::path::PathBuf::from("window.png"));
    }

    #[test]
    fn parses_capture_window_with_clipboard_delay_and_format() {
        let cli = Cli::try_parse_from([
            "readshot",
            "capture-window",
            "--window",
            "fake-window-0",
            "--clipboard",
            "--delay",
            "1.5",
            "--format",
            "webp",
        ])
        .unwrap();
        let Some(Command::CaptureWindow {
            clipboard,
            delay,
            format,
            ..
        }) = cli.command
        else {
            panic!("wrong subcommand");
        };
        assert!(clipboard);
        assert_eq!(delay, 1.5);
        assert_eq!(format, ImageFormatChoice::Webp);
    }

    #[test]
    fn parses_capture_window_shadow_flags() {
        let cli = Cli::try_parse_from([
            "readshot",
            "capture-window",
            "--window",
            "fake-window-0",
            "--no-window-shadow",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::CaptureWindow {
                no_window_shadow: true,
                window_shadow: false,
                ..
            })
        ));

        let err = Cli::try_parse_from([
            "readshot",
            "capture-window",
            "--window",
            "fake-window-0",
            "--window-shadow",
            "--no-window-shadow",
        ])
        .unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn parses_capture_text_subcommand() {
        let cli = Cli::try_parse_from(["readshot", "capture-text"]).unwrap();
        assert!(matches!(cli.command, Some(Command::CaptureText { .. })));
    }

    #[test]
    fn parses_text_json_flags() {
        let cli = Cli::try_parse_from(["readshot", "capture-text", "--json"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::CaptureText { json: true, .. })
        ));

        let cli = Cli::try_parse_from(["readshot", "ocr", "-i", "in.png", "--json"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Ocr { json: true, .. })));
    }

    #[test]
    fn text_json_rejects_clipboard_output() {
        let err =
            Cli::try_parse_from(["readshot", "capture-text", "--json", "--clipboard"]).unwrap_err();

        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn parses_text_commands_with_clipboard_and_delay() {
        let cli =
            Cli::try_parse_from(["readshot", "capture-text", "--clipboard", "--delay", "0.25"])
                .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::CaptureText {
                clipboard: true,
                delay: 0.25,
                ..
            })
        ));

        let cli = Cli::try_parse_from(["readshot", "ocr", "-i", "in.png", "--clipboard"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Ocr {
                clipboard: true,
                ..
            })
        ));
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
    fn capture_help_does_not_include_gui_only_last_region_flag() {
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
    fn parses_mcp_config_command() {
        let cli = Cli::try_parse_from([
            "readshot",
            "mcp-config",
            "--name",
            "screen",
            "--command",
            "/usr/local/bin/readshot-mcp",
        ])
        .unwrap();

        assert!(matches!(
            cli.command,
            Some(Command::McpConfig { ref name, ref command })
                if name == "screen" && command == "/usr/local/bin/readshot-mcp"
        ));
    }

    #[test]
    fn parses_completions_command() {
        let cli = Cli::try_parse_from(["readshot", "completions", "zsh"]).unwrap();

        assert!(matches!(
            cli.command,
            Some(Command::Completions {
                shell: clap_complete::Shell::Zsh
            })
        ));
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
    async fn run_capture_json_requires_file_or_clipboard_destination() {
        let (cap, ocr) = fakes();
        let cli = Cli::try_parse_from(["readshot", "capture", "--json"]).unwrap();
        let mut out = Vec::new();
        let err = cli.run(cap, ocr, &mut out).await.unwrap_err();

        assert!(matches!(err, CliError::InvalidInput(ref msg) if msg.contains("--json")));
        assert_eq!(exit_code(&err), 64);
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn run_capture_json_writes_metadata_to_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let png_path = dir.path().join("out.png");
        let (cap, ocr) = fakes();
        let cli = Cli::try_parse_from([
            "readshot",
            "capture",
            "--rect",
            "0,0,64,64",
            "--json",
            "-o",
            png_path.to_str().unwrap(),
        ])
        .unwrap();
        let mut out = Vec::new();
        cli.run(cap, ocr, &mut out).await.unwrap();

        let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(value["kind"], "capture");
        assert_eq!(value["image"]["width"], 256);
        assert_eq!(value["image"]["format"], "png");
        assert_eq!(value["source"]["display_id"], "fake-0");
        assert_eq!(value["output"], png_path.display().to_string());
        assert!(png_path.exists());
    }

    #[tokio::test]
    async fn run_capture_clipboard_copies_image_without_stdout_png() {
        let (cap, ocr) = fakes();
        let clipboard = FakeClipboard::default();
        let cli = Cli::try_parse_from(["readshot", "capture", "--clipboard"]).unwrap();
        let mut out = Vec::new();
        cli.run_with_clipboard(cap, ocr, &mut out, &clipboard)
            .await
            .unwrap();

        assert!(out.is_empty());
        assert_eq!(clipboard.image_size(), Some((256, 256)));
    }

    #[tokio::test]
    async fn run_capture_writes_jpg_to_file() {
        let dir = tempfile::tempdir().unwrap();
        let jpg_path = dir.path().join("out.jpg");
        let (cap, ocr) = fakes();
        let cli = Cli::try_parse_from([
            "readshot",
            "capture",
            "--format",
            "jpg",
            "-o",
            jpg_path.to_str().unwrap(),
        ])
        .unwrap();
        let mut out = Vec::new();
        cli.run(cap, ocr, &mut out).await.unwrap();

        let bytes = std::fs::read(&jpg_path).unwrap();
        assert_eq!(&bytes[..3], &[0xff, 0xd8, 0xff]);
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
    async fn run_capture_window_with_rect_crops_relative_to_window() {
        let dir = tempfile::tempdir().unwrap();
        let png_path = dir.path().join("window-region.png");
        let (cap, ocr) = fakes();
        let cli = Cli::try_parse_from([
            "readshot",
            "capture-window",
            "--window",
            "fake-window-0",
            "--rect",
            "8,12,32,24",
            "-o",
            png_path.to_str().unwrap(),
        ])
        .unwrap();
        let mut out = Vec::new();
        cli.run(cap, ocr, &mut out).await.unwrap();

        let img = image::open(&png_path).unwrap().to_rgba8();
        assert_eq!(img.width(), 32);
        assert_eq!(img.height(), 24);
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
    async fn run_ocr_json_emits_text_confidence_and_image_size() {
        let dir = tempfile::tempdir().unwrap();
        let png_path = dir.path().join("in.png");
        let img = image::RgbaImage::new(8, 6);
        readshot_core::save_png(&img, &png_path).unwrap();

        let cap = Arc::new(FakeCapturer::new());
        let ocr = Arc::new(FakeOcrEngine::with_text_and_confidence("json text", 0.42));
        let cli = Cli::try_parse_from([
            "readshot",
            "ocr",
            "-i",
            png_path.to_str().unwrap(),
            "--json",
        ])
        .unwrap();
        let mut out = Vec::new();
        cli.run(cap, ocr, &mut out).await.unwrap();

        let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(value["kind"], "ocr");
        assert_eq!(value["text"], "json text");
        let confidence = value["average_confidence"].as_f64().unwrap();
        assert!((confidence - 0.42).abs() < 0.001);
        assert_eq!(value["image"]["width"], 8);
        assert_eq!(value["image"]["height"], 6);
        assert_eq!(value["input"], png_path.display().to_string());
    }

    #[tokio::test]
    async fn run_ocr_clipboard_copies_text_without_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let png_path = dir.path().join("in.png");
        let img = image::RgbaImage::new(8, 8);
        readshot_core::save_png(&img, &png_path).unwrap();

        let (cap, ocr) = fakes();
        let clipboard = FakeClipboard::default();
        let cli = Cli::try_parse_from([
            "readshot",
            "ocr",
            "-i",
            png_path.to_str().unwrap(),
            "--clipboard",
        ])
        .unwrap();
        let mut out = Vec::new();
        cli.run_with_clipboard(cap, ocr, &mut out, &clipboard)
            .await
            .unwrap();

        assert!(out.is_empty());
        assert_eq!(clipboard.text(), Some("hello world".to_string()));
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
    async fn run_capture_text_json_includes_capture_source() {
        let (cap, ocr) = fakes();
        let cli =
            Cli::try_parse_from(["readshot", "capture-text", "--rect", "0,0,64,64", "--json"])
                .unwrap();
        let mut out = Vec::new();
        cli.run(cap, ocr, &mut out).await.unwrap();

        let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(value["kind"], "capture-text");
        assert_eq!(value["text"], "hello world");
        assert_eq!(value["image"]["width"], 256);
        assert_eq!(value["source"]["type"], "display");
        assert_eq!(value["source"]["rect"]["width"], 64.0);
    }

    #[tokio::test]
    async fn run_capture_and_ocr_json_rejects_stdout_image_output() {
        let (cap, ocr) = fakes();
        let cli =
            Cli::try_parse_from(["readshot", "capture-and-ocr", "--json", "--also-image", "-"])
                .unwrap();
        let mut out = Vec::new();
        let err = cli.run(cap, ocr, &mut out).await.unwrap_err();

        assert!(matches!(err, CliError::InvalidInput(ref msg) if msg.contains("--also-image")));
        assert_eq!(exit_code(&err), 64);
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn run_mcp_config_emits_stdio_config() {
        let (cap, ocr) = fakes();
        let cli = Cli::try_parse_from(["readshot", "mcp-config"]).unwrap();
        let mut out = Vec::new();
        cli.run(cap, ocr, &mut out).await.unwrap();

        let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(value["mcpServers"]["readshot"]["command"], "readshot-mcp");
        assert_eq!(
            value["mcpServers"]["readshot"]["args"],
            serde_json::json!([])
        );
    }

    #[tokio::test]
    async fn run_completions_emits_shell_script() {
        let (cap, ocr) = fakes();
        let cli = Cli::try_parse_from(["readshot", "completions", "zsh"]).unwrap();
        let mut out = Vec::new();
        cli.run(cap, ocr, &mut out).await.unwrap();

        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("_readshot"));
        assert!(text.contains("capture-and-ocr"));
        assert!(text.contains("mcp-config"));
        assert!(!text.contains("__interactive-capture"));
        assert!(!text.contains("--last-region"));
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
    async fn run_capture_with_invalid_scale_returns_usage_error() {
        let (cap, ocr) = fakes();
        let cli = Cli::try_parse_from([
            "readshot",
            "capture",
            "--scale",
            "0",
            "-o",
            "/tmp/ignored.png",
        ])
        .unwrap();
        let mut out = Vec::new();
        let err = cli.run(cap, ocr, &mut out).await.unwrap_err();

        assert!(matches!(err, CliError::InvalidInput(ref s) if s.contains("--scale")));
        assert_eq!(exit_code(&err), 64);
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
