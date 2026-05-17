//! Readshot binary — composition root.
//!
//! This file is intentionally thin: it constructs the per-OS
//! services, builds an [`App`], and hands control to the iced daemon.
//! All testable logic lives under `lib.rs`.
//!
//! The full iced runtime wiring (multi-window overlay spawning,
//! tray-icon thread, global-hotkey subscription, URL scheme handler)
//! is the next bit of work that lands once the GUI is exercised on
//! a real display. Until then the binary entry point is a small
//! placeholder that initialises tracing and reports its build status
//! — useful for verifying the crate links and runs through to a
//! controlled exit.

use std::sync::Arc;

use clap::Parser;
use readshot_capture::default_capturer;
use readshot_ocr::default_engine;

use readshot_app::cli::{exit_code, Cli};
use readshot_app::runtime;
use readshot_app::url_scheme;

fn main() -> iced::Result {
    // Phase D (argv path): if the binary is invoked with a
    // `readshot://...` URL as the first argument (e.g. `open
    // readshot://new` on macOS or a desktop-handler invocation on
    // Linux), parse it now and pass the result to the runtime so
    // start() can dispatch the corresponding action on boot.
    let argv: Vec<String> = std::env::args().collect();
    let url_arg = argv
        .get(1)
        .filter(|a| a.starts_with("readshot://"))
        .cloned();
    // Parse CLI second. If argv[1] is a readshot:// URL, skip clap so
    // the GUI path can handle it after logging is ready. Any
    // subcommand routes to the headless surface and exits with a
    // stable status code.
    let cli = if url_arg.is_some() {
        // Skip clap when argv[1] is a URL — clap would reject it.
        Cli { command: None }
    } else {
        Cli::parse()
    };
    if cli.command.is_some() {
        init_cli_logging();
        let capturer = Arc::from(default_capturer());
        let ocr = Arc::from(default_engine());
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let mut stdout = std::io::stdout().lock();
        let result = rt.block_on(cli.run(capturer, ocr, &mut stdout));
        if let Err(err) = result {
            eprintln!("readshot: {err}");
            std::process::exit(exit_code(&err));
        }
        return Ok(());
    }

    init_app_logging();
    if let Some(arg) = url_arg.as_deref() {
        match url_scheme::parse(arg) {
            Ok(action) => runtime::set_initial_url_action(action),
            Err(e) => eprintln!("readshot: ignoring URL `{arg}`: {e}"),
        }
    }

    tracing::info!(
        target: readshot_core::log::LOG_TARGET,
        "readshot {version} starting iced daemon",
        version = env!("CARGO_PKG_VERSION"),
    );

    iced::daemon(runtime::start, runtime::update, runtime::view)
        .title(runtime::title)
        .subscription(runtime::subscription)
        .theme(runtime::theme)
        // The style hook is what actually makes overlay windows
        // see-through: iced uses `Style::background_color` as the
        // wgpu surface clear color, so a transparent background lets
        // the desktop show through. See `runtime::style`.
        .style(runtime::style)
        .font(readshot_core::render::FONT_DATA)
        .run()
}

/// Initialise tracing. We always log to stderr, and additionally to
/// `~/Library/Logs/Readshot/readshot.log` on macOS / equivalent on
/// other platforms — Launch Services-launched apps lose stderr to
/// `/dev/null`, so the file fallback is what makes triage possible
/// when the user double-clicks the bundle from Finder.
fn init_app_logging() {
    init_logging(true);
}

/// Initialise quiet logging for headless CLI invocations. CLI output
/// should stay script-friendly: data goes to stdout, errors go to
/// stderr, and routine tracing stays in the log file when available.
fn init_cli_logging() {
    init_logging(false);
}

fn init_logging(log_to_stderr: bool) {
    use tracing_subscriber::fmt;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::EnvFilter;

    let filter = || EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let stderr_layer =
        log_to_stderr.then(|| fmt::layer().with_writer(std::io::stderr).with_target(true));

    let log_path = directories::ProjectDirs::from("dev", "pawanpaudel93", "Readshot")
        .map(|d| d.data_local_dir().join("readshot.log"))
        .or_else(|| std::env::temp_dir().join("readshot.log").into());

    let file_appender = log_path.as_ref().and_then(|p| {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .ok()
    });

    if let Some(file) = file_appender {
        let file_layer = fmt::layer()
            .with_writer(std::sync::Mutex::new(file))
            .with_ansi(false)
            .with_target(true);
        tracing_subscriber::registry()
            .with(filter())
            .with(stderr_layer)
            .with(file_layer)
            .init();
    } else {
        tracing_subscriber::registry()
            .with(filter())
            .with(stderr_layer)
            .init();
    }
}
