//! Headless CLI surface.
//!
//! Readshot has three external entry points:
//!
//! 1. `readshot://` URL scheme → opens the *interactive* overlay
//!    (handled by [`crate::url_scheme`]).
//! 2. `readshot capture | capture-window | ocr | capture-and-ocr |
//!    list-displays | list-windows` → silent, scriptable subcommands
//!    (this module).
//! 3. The MCP server — wraps the same coordinator.
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
    clip_rect_to_display, crop_window_relative_rect, display_local_bounds, CaptureRequest,
    Capturer, DisplayInfo, WindowCaptureRequest, WindowId, WindowInfo,
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
        /// to the primary display.
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

        /// Wait before capture, in seconds (at most 3600).
        #[arg(long, default_value_t = 0.0, value_parser = parse_delay)]
        delay: f64,

        /// Output image format. Not used with `--clipboard`.
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

        /// Wait before capture, in seconds (at most 3600).
        #[arg(long, default_value_t = 0.0, value_parser = parse_delay)]
        delay: f64,

        /// Output image format. Not used with `--clipboard`.
        #[arg(long, value_enum, default_value_t = ImageFormatChoice::Png)]
        format: ImageFormatChoice,

        /// Copy the captured image to the clipboard instead of writing output.
        #[arg(long)]
        clipboard: bool,

        /// Emit capture metadata as JSON to stdout. Requires `--output PATH` or `--clipboard`.
        #[arg(long)]
        json: bool,

        /// Include the native window shadow (the default; this flag only
        /// says so explicitly).
        #[arg(long, conflicts_with = "no_window_shadow")]
        window_shadow: bool,

        /// Omit the native window shadow when the platform supports it.
        #[arg(long)]
        no_window_shadow: bool,

        /// Output image path. Use `-` for stdout.
        #[arg(long, short = 'o', default_value = "-", conflicts_with = "clipboard")]
        output: PathBuf,
    },

    /// Capture a region, run OCR, and write recognized text to stdout.
    CaptureText {
        /// Target display id (as printed by `list-displays`). Defaults
        /// to the primary display.
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

        /// Wait before capture, in seconds (at most 3600).
        #[arg(long, default_value_t = 0.0, value_parser = parse_delay)]
        delay: f64,

        /// Copy recognized text to the clipboard instead of writing output.
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

        /// Apply post-recognition language correction when supported
        /// (on by default; `--language-correction=false` or
        /// `--no-language-correction` turns it off).
        #[arg(
            long,
            num_args = 0..=1,
            default_value_t = true,
            default_missing_value = "true",
            action = clap::ArgAction::Set,
            overrides_with = "no_language_correction"
        )]
        language_correction: bool,

        /// Turn post-recognition language correction off.
        #[arg(long, overrides_with = "language_correction")]
        no_language_correction: bool,
    },

    /// Recognize text in an existing image.
    Ocr {
        /// Input image path. Use `-` for stdin.
        #[arg(long, short = 'i')]
        input: PathBuf,

        /// Output text path. Use `-` for stdout (the default).
        #[arg(long, short = 'o', default_value = "-", conflicts_with = "clipboard")]
        output: PathBuf,

        /// Copy recognized text to the clipboard instead of writing output.
        #[arg(long)]
        clipboard: bool,

        /// Emit OCR result metadata as JSON.
        #[arg(long, conflicts_with = "clipboard")]
        json: bool,

        /// BCP-47 language hints, comma-separated. Empty → automatic detection.
        #[arg(long, value_delimiter = ',')]
        languages: Vec<String>,

        /// Apply post-recognition language correction when supported
        /// (on by default; `--language-correction=false` or
        /// `--no-language-correction` turns it off).
        #[arg(
            long,
            num_args = 0..=1,
            default_value_t = true,
            default_missing_value = "true",
            action = clap::ArgAction::Set,
            overrides_with = "no_language_correction"
        )]
        language_correction: bool,

        /// Turn post-recognition language correction off.
        #[arg(long, overrides_with = "language_correction")]
        no_language_correction: bool,
    },

    /// Capture a region and recognize text in one shot.
    CaptureAndOcr {
        /// Target display id (as printed by `list-displays`). Defaults
        /// to the primary display.
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

        /// Wait before capture, in seconds (at most 3600).
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

        /// Apply post-recognition language correction when supported
        /// (on by default; `--language-correction=false` or
        /// `--no-language-correction` turns it off).
        #[arg(
            long,
            num_args = 0..=1,
            default_value_t = true,
            default_missing_value = "true",
            action = clap::ArgAction::Set,
            overrides_with = "no_language_correction"
        )]
        language_correction: bool,

        /// Turn post-recognition language correction off.
        #[arg(long, overrides_with = "language_correction")]
        no_language_correction: bool,
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

    /// Uninstall Readshot for this user: login entry, command-line links,
    /// permission and (with --purge) data; on macOS the app too.
    ///
    /// Removes the launch-at-login item, the `readshot` / `readshot-mcp`
    /// command-line symlinks (only when they point into a Readshot.app
    /// bundle), resets the Screen Recording permission, and moves the
    /// app bundle to the Trash. Add `--purge` to also delete history,
    /// logs, and preferences.
    Uninstall {
        /// Also delete history, logs, and preferences.
        #[arg(long)]
        purge: bool,

        /// Print what would be removed and exit without changing anything.
        #[arg(short = 'n', long)]
        dry_run: bool,

        /// Assume yes: skip the confirmation prompt (required when not
        /// running in a terminal).
        #[arg(short = 'y', long)]
        yes: bool,
    },
}

impl Command {
    /// The subcommand as typed, for the log.
    fn name(&self) -> &'static str {
        match self {
            Command::ListDisplays { .. } => "list-displays",
            Command::ListWindows { .. } => "list-windows",
            Command::Capture { .. } => "capture",
            Command::CaptureWindow { .. } => "capture-window",
            Command::CaptureText { .. } => "capture-text",
            Command::Ocr { .. } => "ocr",
            Command::CaptureAndOcr { .. } => "capture-and-ocr",
            Command::McpConfig { .. } => "mcp-config",
            Command::Completions { .. } => "completions",
            Command::Uninstall { .. } => "uninstall",
        }
    }
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
    #[error("uninstall failed: {0}")]
    Uninstall(String),
    #[error(
        "Readshot is running. Quit Readshot (its menu-bar or tray menu, Quit Readshot), \
         then run the uninstall again"
    )]
    AppRunning,
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
        CliError::Uninstall(_) => 74,                            // EX_IOERR
        CliError::AppRunning => 75,                              // EX_TEMPFAIL
    }
}

/// Largest `--delay` we accept, in seconds. Beyond this the value is
/// almost certainly user error; capping also avoids passing absurd
/// values to `Duration::from_secs_f64`, which panics near
/// `Duration::MAX`.
const MAX_DELAY_SECS: f64 = 3600.0;
// Request safety limits are shared with the MCP server — single source
// of truth in readshot-capture so the two surfaces can't drift.
use readshot_capture::MAX_RECT_DIM;

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
    for (name, v) in [
        ("x", nums[0]),
        ("y", nums[1]),
        ("width", nums[2]),
        ("height", nums[3]),
    ] {
        if !v.is_finite() {
            return Err(format!("rect.{name} must be finite"));
        }
        if v.abs() > MAX_RECT_DIM {
            return Err(format!(
                "rect.{name} must not exceed {MAX_RECT_DIM} logical pixels"
            ));
        }
    }
    if nums[2] <= 0.0 || nums[3] <= 0.0 {
        return Err("rect must have positive width and height".into());
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
        tracing::info!(
            target: readshot_core::log::cat::CLI,
            command = command.name(),
            version = env!("CARGO_PKG_VERSION"),
            "cli command"
        );
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
                    capture_region(&*capturer, req).await?
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
                no_language_correction,
            } => {
                let language_correction = language_correction && !no_language_correction;
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
                    capture_region(&*capturer, req).await?
                };
                let image_size = (img.width(), img.height());
                let result = ocr
                    .recognise(OCRRequest {
                        image: img,
                        languages: languages.clone(),
                        use_language_correction: language_correction,
                    })
                    .await?
                    .with_layout_text();
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
                no_language_correction,
            } => {
                let language_correction = language_correction && !no_language_correction;
                let img = read_image(&input)?;
                let image_size = (img.width(), img.height());
                let result = ocr
                    .recognise(OCRRequest {
                        image: img,
                        languages: languages.clone(),
                        use_language_correction: language_correction,
                    })
                    .await?
                    .with_layout_text();
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
                no_language_correction,
            } => {
                let language_correction = language_correction && !no_language_correction;
                ensure_combined_has_clean_stdout(json, also_image.as_deref(), &output)?;
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
                    capture_region(&*capturer, req).await?
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
                    .await?
                    .with_layout_text();
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
            Command::Uninstall {
                purge,
                dry_run,
                yes,
            } => {
                run_uninstall(purge, dry_run, yes, stdout)?;
            }
        }
        Ok(())
    }
}

/// Whether an uninstall must ask `Continue? [y/N]` first: not with
/// `--yes`, and without a terminal to ask on it refuses outright, so a
/// script never removes anything it didn't explicitly confirm.
fn must_prompt_before_uninstall(yes: bool, stdin_is_terminal: bool) -> Result<bool, CliError> {
    if yes {
        return Ok(false);
    }
    if !stdin_is_terminal {
        return Err(CliError::InvalidInput(
            "refusing to uninstall without confirmation; re-run with -y/--yes \
             (or -n/--dry-run to preview)"
                .into(),
        ));
    }
    Ok(true)
}

/// `y` / `yes` in any case confirms; anything else (including Enter) is No.
fn confirms_yes(answer: &str) -> bool {
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Drive the built-in uninstall from the CLI: print the plan in plain
/// language, confirm on a TTY (unless `--yes`), then execute and report
/// each step. `--dry-run` prints the plan and exits without changes.
fn run_uninstall(
    purge: bool,
    dry_run: bool,
    yes: bool,
    stdout: &mut dyn Write,
) -> Result<(), CliError> {
    use std::io::IsTerminal;

    let paths = crate::uninstall::default_uninstall_paths();
    let plan = crate::uninstall::plan_uninstall(&paths, purge);
    write!(stdout, "{}", crate::uninstall::render_plan(&plan))?;

    if dry_run {
        writeln!(stdout, "\nDry run — nothing was changed.")?;
        return Ok(());
    }
    if plan.is_empty() {
        return Ok(());
    }
    // A running app keeps its state in memory and would write
    // preferences and History again after the steps (a purge included).
    uninstall_allowed(app_is_running(), dry_run)?;

    if must_prompt_before_uninstall(yes, std::io::stdin().is_terminal())? {
        // Conventional prompt: default is No, `y` / `yes` confirms.
        write!(stdout, "\nContinue? [y/N] ")?;
        stdout.flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !confirms_yes(&answer) {
            writeln!(stdout, "Aborted. Nothing was changed.")?;
            return Ok(());
        }
    }

    let system = crate::uninstall::RealSystem;
    let reports = crate::uninstall::execute(&plan, &system);
    writeln!(stdout)?;
    // Kept preferences say Open at Login is off, so running the
    // left-over program again doesn't register it again at startup: only
    // once the entry is really gone (a purge deletes the file; nothing
    // is created).
    let prefs_path = crate::runtime::default_preferences_path();
    if let Err(e) =
        crate::uninstall::record_open_at_login_off_after(&reports, prefs_path.as_deref(), purge)
    {
        writeln!(
            stdout,
            "  note     couldn't record Open at Login as off: {e}"
        )?;
    }
    let mut failures = 0usize;
    for report in &reports {
        match &report.outcome {
            Ok(crate::uninstall::ActionStatus::Done) => {
                writeln!(stdout, "  ok       {}", report.action.describe())?;
            }
            Ok(crate::uninstall::ActionStatus::Skipped(why)) => {
                writeln!(stdout, "  skipped  {} ({why})", report.action.describe())?;
            }
            Err(e) => {
                failures += 1;
                writeln!(stdout, "  FAILED   {} — {e}", report.action.describe())?;
            }
        }
    }

    if failures > 0 {
        return Err(CliError::Uninstall(format!(
            "{failures} step(s) could not be completed; see the list above"
        )));
    }
    writeln!(
        stdout,
        "\n{}",
        crate::uninstall::completion_message(&plan, purge)
    )?;
    Ok(())
}

/// Whether an uninstall may run: not while the app runs (it could
/// recreate what the steps remove), except a dry run, which changes
/// nothing.
fn uninstall_allowed(app_running: bool, dry_run: bool) -> Result<(), CliError> {
    if app_running && !dry_run {
        Err(CliError::AppRunning)
    } else {
        Ok(())
    }
}

/// Whether the Readshot app is running for this user. macOS: Launch
/// Services knows it by bundle id (`lsappinfo`; this CLI process is not
/// a registered app, so it never counts). Windows and Linux: the app
/// holds the single-instance lock. Unknown counts as not running.
fn app_is_running() -> bool {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("/usr/bin/lsappinfo")
            .args(["find", &format!("bundleid={}", crate::uninstall::BUNDLE_ID)])
            .output()
            .map(|o| lsappinfo_finds_app(&String::from_utf8_lossy(&o.stdout)))
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "macos"))]
    {
        crate::single_instance::lock_dir()
            .filter(|d| d.exists())
            .and_then(|d| instance_lock_held(&d).ok())
            .unwrap_or(false)
    }
}

/// `lsappinfo find` prints one `ASN:…` line per matching running app,
/// nothing when there is none.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn lsappinfo_finds_app(output: &str) -> bool {
    output.lines().any(|l| l.trim_start().starts_with("ASN:"))
}

/// Whether another process holds the single-instance lock in `dir`.
/// Taking it briefly when free leaves no request (`None`).
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn instance_lock_held(dir: &std::path::Path) -> std::io::Result<bool> {
    use crate::single_instance::{acquire, Instance};
    Ok(matches!(acquire(dir, None)?, Instance::Secondary))
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
    let dir = private_handoff_dir()?;
    let path = dir.join("capture.png");
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
        let _ = std::fs::remove_dir_all(&dir);
        return Err(interactive_child_error(status.code()));
    }

    let result = read_interactive_capture_file(&path);
    let _ = std::fs::remove_dir_all(&dir);
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

/// Create a fresh owner-only directory under the system temp dir for
/// the parent↔child capture handoff.
///
/// The handoff path travels on the child's command line, where any
/// local user can read it (`/proc/<pid>/cmdline` on Linux), and the
/// child writes the PNG with default permissions. On systems with a
/// shared `/tmp` (Linux — macOS gives each user a private `$TMPDIR`)
/// a bare temp file would let another local user read the screenshot
/// or pre-plant a symlink at the predictable path. A `0700` directory
/// closes both: the path may be known, but nothing inside it is
/// reachable by other users. Anything already at the path (stale run
/// with a recycled pid, or a hostile pre-created entry) is deleted and
/// the directory recreated fresh; a create race fails closed.
fn private_handoff_dir() -> Result<PathBuf, CliError> {
    let dir = std::env::temp_dir().join(format!("readshot-interactive-{}", std::process::id()));
    match std::fs::symlink_metadata(&dir) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(&dir)?,
        Ok(_) => std::fs::remove_file(&dir)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700);
        builder
    };
    #[cfg(not(unix))]
    let builder = std::fs::DirBuilder::new();
    builder.create(&dir)?;
    Ok(dir)
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
        // Current layout: `readshot-interactive-<pid>/` directories.
        // The `.png` suffix matches files left behind by older builds.
        if !name.starts_with("readshot-interactive-") {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if modified < stale_before {
            if metadata.is_dir() {
                let _ = std::fs::remove_dir_all(path);
            } else {
                let _ = std::fs::remove_file(path);
            }
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

/// The `__interactive-capture` child's exit when the user cancelled
/// (Esc, or a selection too small to capture).
pub const INTERACTIVE_CHILD_CANCELLED: i32 = 64;
/// The child's exit when Screen Recording permission is missing.
pub const INTERACTIVE_CHILD_NO_PERMISSION: i32 = 77;
/// The child's exit for any other failure (no display, capture error).
pub const INTERACTIVE_CHILD_FAILED: i32 = 71;

/// How the child reports a failure it saw (`err` is the runtime's
/// message): permission problems get their own code, so the parent
/// exits 77 for them as a non-interactive capture would.
pub fn interactive_child_failure_code(err: &str) -> i32 {
    let denied = CaptureError::PermissionDenied.to_string();
    if err.contains(&denied) || err.contains("Screen Recording permission") {
        INTERACTIVE_CHILD_NO_PERMISSION
    } else {
        INTERACTIVE_CHILD_FAILED
    }
}

/// The parent's error for a child that wrote no image. Only a real
/// cancel is "cancelled" (64); a crash (no code) is a capture failure.
fn interactive_child_error(code: Option<i32>) -> CliError {
    match code {
        Some(INTERACTIVE_CHILD_CANCELLED) => interactive_capture_cancelled(),
        Some(INTERACTIVE_CHILD_NO_PERMISSION) => CliError::Capture(CaptureError::PermissionDenied),
        Some(code) => CliError::Capture(CaptureError::Backend(format!(
            "interactive capture failed (exit {code})"
        ))),
        None => CliError::Capture(CaptureError::Backend(
            "interactive capture stopped unexpectedly".into(),
        )),
    }
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
    let rect = rect.unwrap_or_else(|| display_local_bounds(chosen));
    let scale = scale.unwrap_or(chosen.scale);
    validate_scale(scale)?;
    validate_capture_pixels(rect, scale)?;
    // The capture is the part of the rect on the display; the request
    // (and so the JSON) says exactly that. Entirely off it is 64.
    let rect = clip_rect_to_display(rect, chosen).map_err(limit_err_to_cli)?;
    Ok(CaptureRequest {
        display_id: chosen.id.clone(),
        rect,
        scale,
        hide_cursor,
    })
}

/// Map the shared limit validators' typed rejection into the CLI's
/// usage-error class (exit code 64) — these are bad arguments, not
/// capture failures.
fn limit_err_to_cli(e: CaptureError) -> CliError {
    match e {
        CaptureError::InvalidRegion(msg) => CliError::InvalidInput(msg),
        other => CliError::Capture(other),
    }
}

fn validate_capture_pixels(rect: Rect, scale: f32) -> Result<(), CliError> {
    readshot_capture::validate_capture_pixels(rect, scale).map_err(limit_err_to_cli)
}

fn validate_scale(scale: f32) -> Result<(), CliError> {
    readshot_capture::validate_scale(scale).map_err(limit_err_to_cli)
}

async fn lookup_window_bounds(
    capturer: &dyn Capturer,
    window_id: &WindowId,
) -> Result<Rect, CliError> {
    readshot_capture::lookup_window_bounds(capturer, window_id)
        .await
        .map_err(CliError::Capture)
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

/// The display column for a window on no display.
const NO_DISPLAY: &str = "-";

fn write_windows_table(out: &mut dyn Write, windows: &[WindowInfo]) -> std::io::Result<()> {
    let rows: Vec<[String; 5]> = windows
        .iter()
        .map(|window| {
            [
                window.id.0.clone(),
                window
                    .display_id
                    .clone()
                    .unwrap_or_else(|| NO_DISPLAY.into()),
                format!(
                    "{},{},{},{}",
                    window.bounds.x() as i32,
                    window.bounds.y() as i32,
                    window.bounds.width() as i32,
                    window.bounds.height() as i32,
                ),
                window.app_name.clone(),
                window.title.clone(),
            ]
        })
        .collect();
    let header = ["id", "display", "bounds (x,y,w,h)", "app", "title"];
    // Each column is as wide as its longest cell plus two spaces, so a
    // long value never runs into the next column.
    let width = |col: usize| {
        rows.iter()
            .map(|row| row[col].chars().count())
            .chain([header[col].len()])
            .max()
            .unwrap_or(0)
            + 2
    };
    let widths = [width(0), width(1), width(2), width(3)];
    let mut line = |cells: [&str; 5]| {
        let mut text = String::new();
        for (cell, w) in cells[..4].iter().zip(widths) {
            text.push_str(&format!("{cell:<w$}"));
        }
        text.push_str(cells[4]);
        writeln!(out, "{}", text.trim_end())
    };
    line(header)?;
    for row in &rows {
        line([&row[0], &row[1], &row[2], &row[3], &row[4]])?;
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

/// `capture-and-ocr` writes the text (or JSON) to `--output` and the
/// image to `--also-image`; both default to or may name stdout, which
/// can carry only one of them.
fn ensure_combined_has_clean_stdout(
    json: bool,
    also_image: Option<&std::path::Path>,
    output: &std::path::Path,
) -> Result<(), CliError> {
    let stdout = std::path::Path::new("-");
    if also_image == Some(stdout) && output == stdout {
        let text = if json { "JSON" } else { "text" };
        return Err(CliError::InvalidInput(format!(
            "--also-image - needs --output FILE: stdout can't carry both the image and the {text}"
        )));
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

/// Capture `req` and describe it for the JSON `source`.
async fn capture_region(
    capturer: &dyn Capturer,
    req: CaptureRequest,
) -> Result<(RgbaImage, serde_json::Value), CliError> {
    tracing::info!(
        target: readshot_core::log::cat::CLI,
        display = %req.display_id,
        width = req.rect.width(),
        height = req.rect.height(),
        scale = req.scale,
        "cli region capture"
    );
    let img = capturer.capture_region(req.clone()).await?;
    let source = capture_request_json(&req, &img);
    Ok((img, source))
}

fn capture_request_json(req: &CaptureRequest, img: &RgbaImage) -> serde_json::Value {
    serde_json::json!({
        "type": "display",
        "display_id": req.display_id,
        "rect": rect_json(req.rect),
        "scale": output_scale(req, img),
        "hide_cursor": req.hide_cursor,
    })
}

/// The scale of the image a capture really produced: the requested one
/// when the image is that size (to a pixel), else its measured ratio.
fn output_scale(req: &CaptureRequest, img: &RgbaImage) -> f32 {
    let width = req.rect.width();
    let measured = img.width() as f32 / width;
    if (img.width() as f32 - width * req.scale).abs() <= 1.0 {
        req.scale
    } else {
        measured
    }
}

/// `image.format` in capture JSON when the image went to the clipboard.
const CLIPBOARD_FORMAT: &str = "rgba";

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
            // The clipboard gets the raw pixels whatever `--format`
            // says; the system stores them in its own image type.
            "format": if clipboard { CLIPBOARD_FORMAT } else { format.as_str() },
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
        // By content, not extension: `--also-image x.jpg` writes PNG.
        let dyn_img = image::ImageReader::open(path)
            .and_then(image::ImageReader::with_guessed_format)
            .map_err(image::ImageError::IoError)?
            .decode()?;
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
    fn uninstall_prompt_defaults_to_no() {
        assert!(confirms_yes("y\n"));
        assert!(confirms_yes("Yes\n"));
        assert!(!confirms_yes("\n"));
        assert!(!confirms_yes("no\n"));
        assert!(!confirms_yes("yeah\n"));
    }

    /// CLI-40: without `--yes` and without a terminal the uninstall
    /// refuses (exit 64) before any step; with a terminal it prompts;
    /// `--yes` skips the prompt.
    #[test]
    fn uninstall_refuses_without_a_terminal_unless_yes() {
        let err = must_prompt_before_uninstall(false, false).unwrap_err();
        assert!(
            matches!(err, CliError::InvalidInput(ref msg)
                if msg.contains("refusing to uninstall without confirmation")),
            "{err:?}"
        );
        assert_eq!(exit_code(&err), 64);
        assert!(must_prompt_before_uninstall(false, true).unwrap());
        assert!(!must_prompt_before_uninstall(true, false).unwrap());
        assert!(!must_prompt_before_uninstall(true, true).unwrap());
    }

    #[test]
    fn uninstall_accepts_short_flags() {
        let cli = Cli::try_parse_from(["readshot", "uninstall", "-n", "-y", "--purge"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Uninstall {
                purge: true,
                dry_run: true,
                yes: true
            })
        ));
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
        assert!(err.contains("positive width and height"), "{err}");
    }

    #[test]
    fn parse_rect_rejects_extreme_dimensions() {
        let err = parse_rect("0,0,999999,600").unwrap_err();
        assert!(err.contains("must not exceed"));
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
    fn parses_uninstall_subcommand_with_flags() {
        let cli = Cli::try_parse_from(["readshot", "uninstall", "--purge", "--dry-run", "--yes"])
            .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Uninstall {
                purge: true,
                dry_run: true,
                yes: true,
            })
        ));

        let cli = Cli::try_parse_from(["readshot", "uninstall"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Uninstall {
                purge: false,
                dry_run: false,
                yes: false,
            })
        ));
    }

    #[test]
    fn uninstall_dry_run_reports_plan_and_changes_nothing() {
        let paths = crate::uninstall::default_uninstall_paths();
        let plan = crate::uninstall::plan_uninstall(&paths, false);
        // Dry-run always includes the permission-reset step, so the plan
        // is never empty and the rendered text is stable to assert on.
        let rendered = crate::uninstall::render_plan(&plan);
        assert!(rendered.contains("Screen Recording"));
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
    fn private_handoff_dir_is_fresh_and_owner_only() {
        let dir = private_handoff_dir().unwrap();
        let name = dir.file_name().and_then(|n| n.to_str()).unwrap();
        assert!(name.starts_with("readshot-interactive-"));
        assert_eq!(dir.parent(), Some(std::env::temp_dir().as_path()));
        assert!(dir.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "handoff dir must be owner-only");
        }

        // A pre-existing entry at the path (here: a stale dir with
        // leftover contents) is replaced, not reused.
        std::fs::write(dir.join("stale.png"), b"old").unwrap();
        let fresh = private_handoff_dir().unwrap();
        assert_eq!(fresh, dir);
        assert!(!fresh.join("stale.png").exists());

        let _ = std::fs::remove_dir_all(&dir);
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

    fn language_correction_of(args: &[&str]) -> bool {
        let cli = Cli::try_parse_from(args.iter().copied()).unwrap();
        match cli.command {
            Some(
                Command::Ocr {
                    language_correction,
                    no_language_correction,
                    ..
                }
                | Command::CaptureText {
                    language_correction,
                    no_language_correction,
                    ..
                }
                | Command::CaptureAndOcr {
                    language_correction,
                    no_language_correction,
                    ..
                },
            ) => language_correction && !no_language_correction,
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn language_correction_is_on_by_default_and_can_be_turned_off() {
        for cmd in [
            &["readshot", "ocr", "-i", "a.png"][..],
            &["readshot", "capture-text"][..],
            &["readshot", "capture-and-ocr"][..],
        ] {
            assert!(language_correction_of(cmd), "{cmd:?}");
            let with = |extra: &[&'static str]| {
                let mut v = cmd.to_vec();
                v.extend_from_slice(extra);
                language_correction_of(&v)
            };
            assert!(with(&["--language-correction"]), "bare flag stays on");
            assert!(with(&["--language-correction=true"]));
            assert!(!with(&["--language-correction=false"]), "{cmd:?}");
            assert!(!with(&["--language-correction", "false"]), "{cmd:?}");
            assert!(!with(&["--no-language-correction"]), "{cmd:?}");
        }
    }

    #[test]
    fn display_help_says_the_primary_display_is_the_default() {
        use clap::CommandFactory;
        let mut cmd = Cli::command();
        for sub in ["capture", "capture-text", "capture-and-ocr"] {
            let help = cmd
                .find_subcommand_mut(sub)
                .unwrap()
                .render_long_help()
                .to_string();
            assert!(help.contains("primary display"), "{sub}: {help}");
            assert!(!help.contains("first reported"), "{sub}");
        }
    }

    #[test]
    fn uninstall_refuses_while_the_app_runs_except_a_dry_run() {
        assert!(uninstall_allowed(false, false).is_ok());
        assert!(
            uninstall_allowed(true, true).is_ok(),
            "a dry run changes nothing"
        );
        let err = uninstall_allowed(true, false).unwrap_err();
        assert!(matches!(err, CliError::AppRunning));
        assert_eq!(exit_code(&err), 75);
        assert!(err.to_string().contains("Quit Readshot"), "{err}");
    }

    #[test]
    fn lsappinfo_output_names_a_running_app() {
        assert!(lsappinfo_finds_app("ASN:0x0-0x24024-\"Readshot\":\n"));
        assert!(!lsappinfo_finds_app(""));
        assert!(!lsappinfo_finds_app("\n"));
    }

    #[test]
    fn a_held_instance_lock_means_the_app_runs() {
        use crate::single_instance::{acquire, Instance};
        let dir = tempfile::tempdir().unwrap();
        assert!(!instance_lock_held(dir.path()).unwrap());
        let Instance::Primary(guard) = acquire(dir.path(), None).unwrap() else {
            panic!("nobody held it");
        };
        assert!(instance_lock_held(dir.path()).unwrap());
        drop(guard);
        assert!(!instance_lock_held(dir.path()).unwrap());
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
        assert!(help.contains("write recognized text to stdout"));
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

    /// An interactive capture exits 64 only for a real cancel: a missing
    /// permission inside the overlay child exits 77, and any other child
    /// failure (or a crash) is a capture failure, 71.
    #[test]
    fn interactive_child_failures_keep_their_exit_codes() {
        let code = |c| exit_code(&interactive_child_error(c));
        assert_eq!(code(Some(INTERACTIVE_CHILD_CANCELLED)), 64);
        assert_eq!(code(Some(INTERACTIVE_CHILD_NO_PERMISSION)), 77);
        assert_eq!(code(Some(INTERACTIVE_CHILD_FAILED)), 71);
        assert_eq!(code(Some(101)), 71);
        assert_eq!(code(None), 71);
        assert_eq!(
            interactive_child_failure_code(
                "Screen Recording permission is required for interactive capture"
            ),
            INTERACTIVE_CHILD_NO_PERMISSION
        );
        assert_eq!(
            interactive_child_failure_code(
                "could not list displays: screen capture permission denied"
            ),
            INTERACTIVE_CHILD_NO_PERMISSION
        );
        assert_eq!(
            interactive_child_failure_code("no displays detected"),
            INTERACTIVE_CHILD_FAILED
        );
    }

    /// `--format` doesn't apply to `--clipboard` (the raw pixels go
    /// there), so the JSON reports `rgba`, not the requested format.
    #[tokio::test]
    async fn clipboard_json_reports_what_the_clipboard_got() {
        let (cap, ocr) = fakes();
        let clipboard = FakeClipboard::default();
        let cli = Cli::try_parse_from([
            "readshot",
            "capture",
            "--clipboard",
            "--format",
            "jpg",
            "--json",
        ])
        .unwrap();
        let mut out = Vec::new();
        cli.run_with_clipboard(cap, ocr, &mut out, &clipboard)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(value["image"]["format"], "rgba");
        assert_eq!(value["clipboard"], true);
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

    /// The CLI's `ocr` and `capture-text` return exactly what the GUI's
    /// `CaptureCoordinator::recognise` returns: the layout pass, not the
    /// engine's flat lines.
    #[tokio::test]
    async fn cli_ocr_text_equals_the_gui_pipeline() {
        use crate::coordinator::CaptureCoordinator;
        use crate::permissions::fake::FakePermissions;
        let dir = tempfile::tempdir().unwrap();
        let png_path = dir.path().join("in.png");
        let img = image::RgbaImage::new(40, 20);
        readshot_core::save_png(&img, &png_path).unwrap();
        let lines = FakeOcrEngine::layout_fixture_lines();
        let ocr: Arc<dyn OCREngine> = Arc::new(FakeOcrEngine::with_lines(lines.clone()));
        let coord = CaptureCoordinator::new(
            Arc::new(FakeCapturer::new()),
            Arc::clone(&ocr),
            Arc::new(FakePermissions::granted()),
            None,
        );
        let gui = coord
            .recognise(OCRRequest {
                image: img,
                languages: Vec::new(),
                use_language_correction: true,
            })
            .await
            .unwrap();
        let flat: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_ne!(
            gui,
            flat.join("\n"),
            "the fixture must exercise the layout pass"
        );

        let cli =
            Cli::try_parse_from(["readshot", "ocr", "-i", png_path.to_str().unwrap()]).unwrap();
        let mut out = Vec::new();
        cli.run(Arc::new(FakeCapturer::new()), Arc::clone(&ocr), &mut out)
            .await
            .unwrap();
        assert_eq!(String::from_utf8(out).unwrap().trim_end(), gui.trim_end());

        let cli = Cli::try_parse_from([
            "readshot",
            "ocr",
            "-i",
            png_path.to_str().unwrap(),
            "--json",
        ])
        .unwrap();
        let mut out = Vec::new();
        cli.run(Arc::new(FakeCapturer::new()), Arc::clone(&ocr), &mut out)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(value["text"], gui);

        let cli = Cli::try_parse_from(["readshot", "capture-text", "--rect", "0,0,40,20"]).unwrap();
        let mut out = Vec::new();
        cli.run(Arc::new(FakeCapturer::new()), Arc::clone(&ocr), &mut out)
            .await
            .unwrap();
        assert_eq!(String::from_utf8(out).unwrap().trim_end(), gui.trim_end());
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

        assert!(matches!(err, CliError::InvalidInput(ref s) if s.contains("scale")));
        assert_eq!(exit_code(&err), 64);
    }

    #[tokio::test]
    async fn run_capture_with_extreme_scale_returns_usage_error() {
        let (cap, ocr) = fakes();
        let cli = Cli::try_parse_from([
            "readshot",
            "capture",
            "--scale",
            "99",
            "-o",
            "/tmp/ignored.png",
        ])
        .unwrap();
        let mut out = Vec::new();
        let err = cli.run(cap, ocr, &mut out).await.unwrap_err();

        assert!(matches!(err, CliError::InvalidInput(ref s) if s.contains("scale")));
        assert_eq!(exit_code(&err), 64);
    }

    #[tokio::test]
    async fn run_capture_with_oversized_scaled_region_returns_usage_error() {
        let (cap, ocr) = fakes();
        let cli = Cli::try_parse_from([
            "readshot",
            "capture",
            "--rect",
            "0,0,10000,10000",
            "--scale",
            "2",
            "-o",
            "/tmp/ignored.png",
        ])
        .unwrap();
        let mut out = Vec::new();
        let err = cli.run(cap, ocr, &mut out).await.unwrap_err();

        assert!(matches!(err, CliError::InvalidInput(ref s) if s.contains("too large")));
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

    // ── W5c: what the dispatcher hands the capturer and the OCR engine ──

    /// A capturer that records each request it receives, over a set of
    /// displays the test chooses.
    struct RecordingCapturer {
        inner: FakeCapturer,
        displays: Vec<DisplayInfo>,
        regions: Mutex<Vec<(CaptureRequest, std::time::Instant)>>,
        windows: Mutex<Vec<WindowCaptureRequest>>,
    }

    impl RecordingCapturer {
        fn new() -> Self {
            let display =
                |id: &str, x: f32, w: f32, h: f32, scale: f32, primary: bool| DisplayInfo {
                    id: id.to_string(),
                    bounds: Rect::from_xywh(x, 0.0, w, h).unwrap(),
                    scale,
                    name: id.to_string(),
                    is_primary: primary,
                };
            // The primary display is deliberately not the first one listed.
            Self {
                inner: FakeCapturer::new(),
                displays: vec![
                    display("side", -800.0, 800.0, 600.0, 1.0, false),
                    display("main", 0.0, 1440.0, 900.0, 2.0, true),
                ],
                regions: Mutex::new(Vec::new()),
                windows: Mutex::new(Vec::new()),
            }
        }

        fn regions(&self) -> Vec<CaptureRequest> {
            self.regions
                .lock()
                .unwrap()
                .iter()
                .map(|(req, _)| req.clone())
                .collect()
        }

        fn windows(&self) -> Vec<WindowCaptureRequest> {
            self.windows.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl Capturer for RecordingCapturer {
        async fn list_displays(&self) -> Result<Vec<DisplayInfo>, CaptureError> {
            Ok(self.displays.clone())
        }

        async fn capture_region(&self, req: CaptureRequest) -> Result<RgbaImage, CaptureError> {
            self.regions
                .lock()
                .unwrap()
                .push((req.clone(), std::time::Instant::now()));
            self.inner.capture_region(req).await
        }

        async fn list_windows(&self) -> Result<Vec<WindowInfo>, CaptureError> {
            self.inner.list_windows().await
        }

        async fn capture_window(
            &self,
            req: WindowCaptureRequest,
        ) -> Result<RgbaImage, CaptureError> {
            self.windows.lock().unwrap().push(req.clone());
            self.inner.capture_window(req).await
        }
    }

    /// An OCR engine that records the languages and correction flag of
    /// each request.
    struct RecordingOcr {
        inner: FakeOcrEngine,
        requests: Mutex<Vec<(Vec<String>, bool)>>,
    }

    impl RecordingOcr {
        fn new() -> Self {
            Self {
                inner: FakeOcrEngine::with_text("hello world"),
                requests: Mutex::new(Vec::new()),
            }
        }

        fn requests(&self) -> Vec<(Vec<String>, bool)> {
            self.requests.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl OCREngine for RecordingOcr {
        fn supported_languages(&self) -> Vec<String> {
            self.inner.supported_languages()
        }

        async fn recognise(&self, req: OCRRequest) -> Result<readshot_ocr::OCRResult, OCRError> {
            self.requests
                .lock()
                .unwrap()
                .push((req.languages.clone(), req.use_language_correction));
            self.inner.recognise(req).await
        }
    }

    async fn run_recorded(
        args: &[&str],
    ) -> (
        Result<(), CliError>,
        Vec<u8>,
        Arc<RecordingCapturer>,
        Arc<RecordingOcr>,
        FakeClipboard,
    ) {
        let cap = Arc::new(RecordingCapturer::new());
        let ocr = Arc::new(RecordingOcr::new());
        let clipboard = FakeClipboard::default();
        let mut out = Vec::new();
        let result = Cli::try_parse_from(args.iter().copied())
            .unwrap()
            .run_with_clipboard(cap.clone(), ocr.clone(), &mut out, &clipboard)
            .await;
        (result, out, cap, ocr, clipboard)
    }

    /// CLI-08: no `--display`/`--rect`/`--scale` → the primary display
    /// (even when it is not listed first), its full display-local bounds,
    /// its scale, and the cursor hidden.
    #[tokio::test]
    async fn capture_defaults_to_the_primary_display_its_bounds_and_scale() {
        for cmd in ["capture", "capture-text", "capture-and-ocr"] {
            let args: &[&str] = if cmd == "capture" {
                &["readshot", "capture", "--clipboard"]
            } else {
                &["readshot", cmd]
            };
            let (result, _, cap, _, _) = run_recorded(args).await;
            result.unwrap();
            let reqs = cap.regions();
            assert_eq!(reqs.len(), 1, "{cmd}");
            let req = &reqs[0];
            assert_eq!(req.display_id, "main", "{cmd}");
            assert_eq!(
                (
                    req.rect.x(),
                    req.rect.y(),
                    req.rect.width(),
                    req.rect.height()
                ),
                (0.0, 0.0, 1440.0, 900.0),
                "{cmd}"
            );
            assert_eq!(req.scale, 2.0, "{cmd}");
            assert!(req.hide_cursor, "{cmd}: the cursor is hidden by default");
        }

        let (result, _, cap, _, _) = run_recorded(&[
            "readshot",
            "capture",
            "--display",
            "side",
            // A leading `-` needs the `=` form, or clap reads a flag.
            "--rect=-10,5,100,50",
            "--scale",
            "3",
            "--clipboard",
        ])
        .await;
        result.unwrap();
        let req = &cap.regions()[0];
        assert_eq!(req.display_id, "side");
        assert_eq!(
            (
                req.rect.x(),
                req.rect.y(),
                req.rect.width(),
                req.rect.height()
            ),
            (0.0, 5.0, 90.0, 50.0),
            "negative x is allowed and keeps only the part on the display"
        );
        assert_eq!(req.scale, 3.0);
    }

    /// CLI-13: `--show-cursor` reaches the capturer (and the JSON source)
    /// for every region command.
    #[tokio::test]
    async fn show_cursor_reaches_the_capture_request() {
        for cmd in ["capture", "capture-text", "capture-and-ocr"] {
            let mut args = vec!["readshot", cmd, "--show-cursor"];
            if cmd == "capture" {
                args.push("--clipboard");
            }
            let (result, _, cap, _, _) = run_recorded(&args).await;
            result.unwrap();
            assert!(!cap.regions()[0].hide_cursor, "{cmd}");
        }

        let (result, out, _, _, _) = run_recorded(&[
            "readshot",
            "capture",
            "--show-cursor",
            "--clipboard",
            "--json",
        ])
        .await;
        result.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(value["source"]["hide_cursor"], false);
    }

    /// CLI-21, CLI-22: the shadow flag reaches the window capture; the
    /// shared flags (clipboard + JSON, other formats) work for windows.
    #[tokio::test]
    async fn window_shadow_flags_reach_the_window_request() {
        for (flag, ignore) in [
            (None, false),
            (Some("--window-shadow"), false),
            (Some("--no-window-shadow"), true),
        ] {
            let mut args = vec![
                "readshot",
                "capture-window",
                "--window",
                "fake-window-0",
                "--clipboard",
                "--json",
            ];
            args.extend(flag);
            let (result, out, cap, _, clipboard) = run_recorded(&args).await;
            result.unwrap();
            let reqs = cap.windows();
            assert_eq!(reqs.len(), 1);
            assert_eq!(reqs[0].ignore_shadows, ignore, "{flag:?}");
            assert_eq!(clipboard.image_size(), Some((256, 256)));
            let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
            assert_eq!(value["kind"], "capture-window");
            assert_eq!(value["source"]["type"], "window");
            assert_eq!(value["source"]["window_id"], "fake-window-0");
            assert_eq!(value["source"]["ignore_shadows"], ignore);
            assert_eq!(value["output"], serde_json::Value::Null);
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("window.webp");
        let (result, out, _, _, _) = run_recorded(&[
            "readshot",
            "capture-window",
            "--window",
            "fake-window-0",
            "--format",
            "webp",
            "-o",
            path.to_str().unwrap(),
        ])
        .await;
        result.unwrap();
        assert!(out.is_empty());
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(
            image::guess_format(&bytes).unwrap(),
            image::ImageFormat::WebP
        );
    }

    /// CLI-28, CLI-29: `--languages` and the correction switch reach the
    /// OCR request for all three OCR commands.
    #[tokio::test]
    async fn language_hints_and_correction_reach_the_ocr_request() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("in.png");
        readshot_core::save_png(&image::RgbaImage::new(8, 8), &png).unwrap();
        let png = png.to_str().unwrap();

        for base in [
            &["readshot", "ocr", "-i", png][..],
            &["readshot", "capture-text"][..],
            &["readshot", "capture-and-ocr"][..],
        ] {
            let (result, _, _, ocr, _) = run_recorded(base).await;
            result.unwrap();
            assert_eq!(ocr.requests(), vec![(Vec::new(), true)], "{base:?}");

            for off in ["--no-language-correction", "--language-correction=false"] {
                let mut args = base.to_vec();
                args.extend(["--languages", "en-US,fr-FR", off]);
                let (result, _, _, ocr, _) = run_recorded(&args).await;
                result.unwrap();
                assert_eq!(
                    ocr.requests(),
                    vec![(vec!["en-US".to_string(), "fr-FR".to_string()], false)],
                    "{args:?}"
                );
            }
        }
    }

    /// CLI-14: `--delay` waits before the capture.
    #[tokio::test]
    async fn delay_waits_before_capturing() {
        let started = std::time::Instant::now();
        let (result, _, cap, _, _) =
            run_recorded(&["readshot", "capture", "--delay", "0.2", "--clipboard"]).await;
        result.unwrap();
        let captured_at = cap.regions.lock().unwrap()[0].1;
        assert!(
            captured_at.duration_since(started) >= Duration::from_millis(200),
            "captured after {:?}",
            captured_at.duration_since(started)
        );
    }

    /// CLI-15, CLI-17: TIFF and WebP are real files of that type; the
    /// default `-o -` writes PNG bytes to stdout; an unwritable path is an
    /// I/O error (74).
    #[tokio::test]
    async fn image_formats_destinations_and_write_errors() {
        let dir = tempfile::tempdir().unwrap();
        for (format, expected) in [
            ("tiff", image::ImageFormat::Tiff),
            ("webp", image::ImageFormat::WebP),
        ] {
            let path = dir.path().join(format!("out.{format}"));
            let (result, out, _, _, _) = run_recorded(&[
                "readshot",
                "capture",
                "--format",
                format,
                "-o",
                path.to_str().unwrap(),
            ])
            .await;
            result.unwrap();
            assert!(out.is_empty(), "{format}");
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(image::guess_format(&bytes).unwrap(), expected);
            let img = image::load_from_memory(&bytes).unwrap();
            assert_eq!((img.width(), img.height()), (256, 256), "{format}");
        }

        let (result, out, _, _, _) = run_recorded(&["readshot", "capture"]).await;
        result.unwrap();
        assert_eq!(&out[..8], b"\x89PNG\r\n\x1a\n", "default output is stdout");

        let unwritable = dir.path().join("missing-dir").join("x.png");
        let (result, out, _, _, _) =
            run_recorded(&["readshot", "capture", "-o", unwritable.to_str().unwrap()]).await;
        let err = result.unwrap_err();
        assert!(matches!(err, CliError::Io(_)), "{err:?}");
        assert_eq!(exit_code(&err), 74);
        assert!(out.is_empty());
    }

    /// CLI-25: `capture-text --clipboard` copies the text and prints nothing.
    #[tokio::test]
    async fn capture_text_clipboard_copies_text_without_stdout() {
        let (result, out, _, _, clipboard) =
            run_recorded(&["readshot", "capture-text", "--clipboard"]).await;
        result.unwrap();
        assert!(out.is_empty());
        assert_eq!(clipboard.text(), Some("hello world".to_string()));
        assert_eq!(clipboard.image_size(), None);
    }

    /// CLI-26, CLI-27: `-o FILE` gets the text exactly (no newline added,
    /// unlike stdout) or the JSON; stdout stays empty.
    #[tokio::test]
    async fn text_commands_write_text_or_json_to_the_output_file() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("in.png");
        readshot_core::save_png(&image::RgbaImage::new(8, 8), &png).unwrap();
        let png = png.to_str().unwrap();
        let text_path = dir.path().join("out.txt");
        let text_out = text_path.to_str().unwrap();

        for args in [
            &["readshot", "ocr", "-i", png, "-o", text_out][..],
            &["readshot", "capture-text", "-o", text_out][..],
            &["readshot", "capture-and-ocr", "-o", text_out][..],
        ] {
            let _ = std::fs::remove_file(&text_path);
            let (result, out, _, _, _) = run_recorded(args).await;
            result.unwrap();
            assert!(out.is_empty(), "{args:?}");
            assert_eq!(std::fs::read_to_string(&text_path).unwrap(), "hello world");
        }

        let (result, out, _, _, _) = run_recorded(&["readshot", "ocr", "-i", png]).await;
        result.unwrap();
        assert_eq!(out, b"hello world\n", "stdout gets a trailing newline");

        let json_path = dir.path().join("out.json");
        for (args, kind) in [
            (vec!["readshot", "ocr", "-i", png, "--json"], "ocr"),
            (vec!["readshot", "capture-text", "--json"], "capture-text"),
            (
                vec!["readshot", "capture-and-ocr", "--json"],
                "capture-and-ocr",
            ),
        ] {
            let mut args = args;
            args.extend(["-o", json_path.to_str().unwrap()]);
            let (result, out, _, _, _) = run_recorded(&args).await;
            result.unwrap();
            assert!(out.is_empty(), "{args:?}");
            let value: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&json_path).unwrap()).unwrap();
            assert_eq!(value["kind"], kind);
            assert_eq!(value["text"], "hello world");
        }
    }

    /// CLI-44: every error class maps to its documented exit code.
    #[test]
    fn every_error_maps_to_its_documented_exit_code() {
        let io = || std::io::Error::other("disk");
        let cases: Vec<(CliError, i32)> = vec![
            (CliError::Capture(CaptureError::PermissionDenied), 77),
            (
                CliError::Capture(CaptureError::DisplayNotFound("d".into())),
                66,
            ),
            (
                CliError::Capture(CaptureError::WindowNotFound("w".into())),
                66,
            ),
            (
                CliError::Capture(CaptureError::InvalidRegion("r".into())),
                64,
            ),
            (CliError::Capture(CaptureError::Backend("b".into())), 71),
            (CliError::Capture(CaptureError::Unsupported("u".into())), 71),
            (CliError::Ocr(OCRError::NoText), 70),
            (CliError::Ocr(OCRError::Backend("o".into())), 70),
            (CliError::Io(io()), 74),
            (CliError::Image(image::ImageError::IoError(io())), 74),
            (CliError::Clipboard("c".into()), 74),
            (CliError::DisplayNotFound("d".into()), 66),
            (CliError::NoDisplays, 66),
            (CliError::InvalidInput("i".into()), 64),
            (
                CliError::InvalidRect {
                    got: "1".into(),
                    reason: "r".into(),
                },
                64,
            ),
            (CliError::Uninstall("1 step(s)".into()), 74),
        ];
        for (err, code) in cases {
            assert_eq!(exit_code(&err), code, "{err:?}");
        }
    }

    /// CLI-47: handoff entries older than a day are removed; fresh ones,
    /// and entries that aren't Readshot's, stay.
    #[cfg(unix)]
    #[test]
    fn stale_interactive_handoff_entries_are_removed() {
        let tmp = std::env::temp_dir();
        let tag = format!(
            "w5c-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        )
        .replace(['(', ')'], "");
        let stale_dir = tmp.join(format!("readshot-interactive-stale-dir-{tag}"));
        let stale_file = tmp.join(format!("readshot-interactive-stale-{tag}.png"));
        let fresh_dir = tmp.join(format!("readshot-interactive-fresh-{tag}"));
        let foreign = tmp.join(format!("not-readshot-interactive-{tag}"));
        std::fs::create_dir_all(&stale_dir).unwrap();
        std::fs::write(stale_dir.join("capture.png"), b"old").unwrap();
        std::fs::write(&stale_file, b"old").unwrap();
        std::fs::create_dir_all(&fresh_dir).unwrap();
        std::fs::write(&foreign, b"keep").unwrap();
        let two_days_ago = SystemTime::now() - Duration::from_secs(2 * 24 * 60 * 60);
        for path in [&stale_dir, &stale_file, &foreign] {
            std::fs::File::open(path)
                .unwrap()
                .set_modified(two_days_ago)
                .unwrap();
        }

        cleanup_stale_interactive_temp_files();

        assert!(!stale_dir.exists(), "a stale handoff dir is removed");
        assert!(!stale_file.exists(), "a stale handoff file is removed");
        assert!(fresh_dir.exists(), "a fresh handoff dir stays");
        assert!(foreign.exists(), "other entries are never touched");
        let _ = std::fs::remove_dir_all(&fresh_dir);
        let _ = std::fs::remove_file(&foreign);
    }

    /// MCP-38: the GUI keeps history in the folder `readshot-mcp` reads
    /// (readshot-mcp's `history_tools_read_the_gui_history_folder` pins
    /// the same path from the server side).
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn gui_history_folder_is_the_one_readshot_mcp_reads() {
        let root = crate::runtime::default_history_root().unwrap();
        #[cfg(target_os = "macos")]
        let expected =
            std::path::Path::new("Library/Application Support/np.com.pawanpaudel.Readshot/history");
        #[cfg(target_os = "linux")]
        let expected = std::path::Path::new("readshot/history");
        assert!(root.ends_with(expected), "{}", root.display());
    }

    // ── W5f: end-to-end bugs B1–B9 ──

    /// A capturer that, like macOS before B1, always returns the region
    /// at a fixed 2x whatever scale was asked for.
    struct FixedRatioCapturer;

    #[async_trait::async_trait]
    impl Capturer for FixedRatioCapturer {
        async fn list_displays(&self) -> Result<Vec<DisplayInfo>, CaptureError> {
            Ok(vec![DisplayInfo {
                id: "main".into(),
                bounds: Rect::from_xywh(0.0, 0.0, 1440.0, 900.0).unwrap(),
                scale: 2.0,
                name: "main".into(),
                is_primary: true,
            }])
        }

        async fn capture_region(&self, req: CaptureRequest) -> Result<RgbaImage, CaptureError> {
            Ok(RgbaImage::new(
                (req.rect.width() * 2.0) as u32,
                (req.rect.height() * 2.0) as u32,
            ))
        }
    }

    /// B1: the JSON `scale` is the scale of the image really produced,
    /// not just the one requested.
    #[tokio::test]
    async fn capture_json_reports_the_scale_the_image_really_has() {
        let clipboard = FakeClipboard::default();
        let mut out = Vec::new();
        Cli::try_parse_from([
            "readshot",
            "capture",
            "--rect",
            "0,0,100,50",
            "--scale",
            "1",
            "--clipboard",
            "--json",
        ])
        .unwrap()
        .run_with_clipboard(
            Arc::new(FixedRatioCapturer),
            Arc::new(FakeOcrEngine::with_text("x")),
            &mut out,
            &clipboard,
        )
        .await
        .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(value["image"]["width"], 200);
        assert_eq!(value["source"]["scale"], 2.0);

        // When the image matches the request, the requested scale stands.
        let mut out = Vec::new();
        Cli::try_parse_from([
            "readshot",
            "capture",
            "--rect",
            "0,0,100,50",
            "--clipboard",
            "--json",
        ])
        .unwrap()
        .run_with_clipboard(
            Arc::new(FixedRatioCapturer),
            Arc::new(FakeOcrEngine::with_text("x")),
            &mut out,
            &clipboard,
        )
        .await
        .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(value["source"]["scale"], 2.0);
    }

    fn rect_tuple(r: Rect) -> (f32, f32, f32, f32) {
        (r.x(), r.y(), r.width(), r.height())
    }

    /// B2, B3: a rect partly off the display captures exactly the part
    /// on it, and the JSON reports that rect; one entirely off exits 64.
    #[tokio::test]
    async fn a_rect_partly_off_the_display_is_clipped_and_reported_truthfully() {
        for (rect, expected) in [
            ("--rect=-100,0,200,50", (0.0, 0.0, 100.0, 50.0)),
            ("--rect=1400,850,100,100", (1400.0, 850.0, 40.0, 50.0)),
        ] {
            for cmd in ["capture", "capture-text", "capture-and-ocr"] {
                let mut args = vec!["readshot", cmd, "--display", "main", rect, "--json"];
                if cmd == "capture" {
                    args.push("--clipboard");
                }
                let (result, out, cap, _, _) = run_recorded(&args).await;
                result.unwrap();
                assert_eq!(rect_tuple(cap.regions()[0].rect), expected, "{args:?}");
                let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
                let r = &value["source"]["rect"];
                assert_eq!(
                    (
                        r["x"].as_f64().unwrap() as f32,
                        r["y"].as_f64().unwrap() as f32,
                        r["width"].as_f64().unwrap() as f32,
                        r["height"].as_f64().unwrap() as f32,
                    ),
                    expected,
                    "{args:?}"
                );
            }
        }

        let (result, out, cap, _, _) = run_recorded(&[
            "readshot",
            "capture",
            "--rect",
            "5000,5000,100,100",
            "--clipboard",
        ])
        .await;
        let err = result.unwrap_err();
        assert_eq!(exit_code(&err), 64, "{err:?}");
        assert!(err.to_string().contains("outside"), "{err}");
        assert!(cap.regions().is_empty() && out.is_empty());
    }

    /// B4: zero-width and zero-height rects are argument errors.
    #[test]
    fn zero_sized_rects_are_rejected() {
        for rect in ["0,0,0,10", "0,0,10,0"] {
            for cmd in ["capture", "capture-text", "capture-and-ocr"] {
                assert!(
                    Cli::try_parse_from(["readshot", cmd, "--rect", rect]).is_err(),
                    "{cmd} {rect}"
                );
            }
            let err = Cli::try_parse_from([
                "readshot",
                "capture-window",
                "--window",
                "1",
                "--rect",
                rect,
            ])
            .unwrap_err();
            assert_eq!(err.exit_code(), 2);
            assert!(err.to_string().contains("width and height"), "{err}");
        }
    }

    /// B5: `ocr -i FILE` decodes by content, not by file extension.
    #[tokio::test]
    async fn ocr_input_is_decoded_by_content_not_extension() {
        let dir = tempfile::tempdir().unwrap();
        let png = readshot_core::encode_png(&image::RgbaImage::new(8, 6)).unwrap();
        for name in ["png-named.jpg", "png-noext"] {
            let path = dir.path().join(name);
            std::fs::write(&path, &png).unwrap();
            let (result, out, _, _, _) =
                run_recorded(&["readshot", "ocr", "-i", path.to_str().unwrap(), "--json"]).await;
            result.unwrap_or_else(|e| panic!("{name}: {e}"));
            let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
            assert_eq!(value["image"]["width"], 8, "{name}");
        }
    }

    /// B6: `--also-image -` needs the text (or JSON) to go to a file, so
    /// stdout never mixes PNG bytes and text.
    #[tokio::test]
    async fn also_image_to_stdout_needs_the_text_in_a_file() {
        for args in [
            &["readshot", "capture-and-ocr", "--also-image", "-"][..],
            &["readshot", "capture-and-ocr", "--also-image", "-", "--json"][..],
        ] {
            let (result, out, cap, _, _) = run_recorded(args).await;
            let err = result.unwrap_err();
            assert!(
                matches!(err, CliError::InvalidInput(ref m) if m.contains("--also-image -")),
                "{err:?}"
            );
            assert_eq!(exit_code(&err), 64);
            assert!(out.is_empty() && cap.regions().is_empty(), "{args:?}");
        }

        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().join("t.txt");
        for json in [false, true] {
            let mut args = vec![
                "readshot",
                "capture-and-ocr",
                "--also-image",
                "-",
                "-o",
                text.to_str().unwrap(),
            ];
            if json {
                args.push("--json");
            }
            let (result, out, _, _, _) = run_recorded(&args).await;
            result.unwrap();
            let img = image::load_from_memory(&out).expect("stdout is exactly one PNG");
            assert_eq!((img.width(), img.height()), (256, 256));
            let written = std::fs::read_to_string(&text).unwrap();
            if json {
                let value: serde_json::Value = serde_json::from_str(&written).unwrap();
                assert_eq!(value["text"], "hello world");
            } else {
                assert_eq!(written, "hello world");
            }
        }
    }

    fn window_on(display_id: Option<&str>, bounds: (f32, f32, f32, f32)) -> WindowInfo {
        WindowInfo {
            id: WindowId("6874".into()),
            title: "Title".into(),
            app_name: "cmux".into(),
            display_id: display_id.map(str::to_string),
            bounds: Rect::from_xywh(bounds.0, bounds.1, bounds.2, bounds.3).unwrap(),
        }
    }

    /// B9: a window on no display is listed with none (`-` in the table,
    /// `null` in JSON), and long bounds never run into the app column.
    #[test]
    fn list_windows_shows_no_display_and_keeps_columns_apart() {
        let windows = [
            window_on(None, (0.0, -72.0, 1512.0, 40.0)),
            window_on(Some("3"), (1512.0, -73.0, 1920.0, 1055.0)),
        ];
        let mut table = Vec::new();
        write_windows_table(&mut table, &windows).unwrap();
        let table = String::from_utf8(table).unwrap();
        let rows: Vec<&str> = table.lines().collect();
        assert!(
            rows[1].split_whitespace().nth(1) == Some("-"),
            "no display shows as -: {table}"
        );
        assert!(
            rows[2].contains("1512,-73,1920,1055 ") && !rows[2].contains("1055cmux"),
            "{table}"
        );
        // The app column starts at the same place on every row.
        let app_at = |row: &str| row.find("cmux").unwrap();
        assert_eq!(app_at(rows[1]), app_at(rows[2]), "{table}");
        assert_eq!(rows[0].find("app").unwrap(), app_at(rows[1]), "{table}");

        let mut json = Vec::new();
        write_windows_json(&mut json, &windows).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(value[0]["display_id"], serde_json::Value::Null);
        assert_eq!(value[1]["display_id"], "3");
    }
}
