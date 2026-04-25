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
use readshot_core::Preferences;
use readshot_ocr::default_engine;
use tracing::Level;

use readshot_app::app::App;
use readshot_app::cli::{exit_code, Cli};
use readshot_app::coordinator::CaptureCoordinator;
use readshot_app::permissions::default_provider;

fn main() {
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .with_target(true)
        .init();

    // Parse CLI first. Bare `readshot` with no subcommand falls
    // through to the GUI bootstrap below; any subcommand routes to
    // the headless surface and exits with a stable status code.
    let cli = Cli::parse();
    if cli.command.is_some() {
        let capturer = Arc::from(default_capturer());
        let ocr = Arc::from(default_engine());
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        let mut stdout = std::io::stdout().lock();
        let result = runtime.block_on(cli.run(capturer, ocr, &mut stdout));
        if let Err(err) = result {
            eprintln!("readshot: {err}");
            std::process::exit(exit_code(&err));
        }
        return;
    }

    let permissions = Arc::from(default_provider());
    let coordinator = CaptureCoordinator::new(
        Arc::from(default_capturer()),
        Arc::from(default_engine()),
        Arc::clone(&permissions),
        None,
    );
    let preferences = Preferences::default();
    let _app = App::new(coordinator, permissions, preferences);

    tracing::info!(
        target: readshot_core::log::LOG_TARGET,
        "readshot {version} initialised; iced daemon wiring pending",
        version = env!("CARGO_PKG_VERSION"),
    );

    // Future work (kept here so the next runner can pick it up):
    //
    // iced::daemon(App::title, App::update, App::view)
    //     .subscription(App::subscription)
    //     .theme(App::theme)
    //     .run_with(App::start)
    //     .expect("iced daemon failed");
    //
    // The `App::title` / `App::update` / `App::view` /
    // `App::subscription` / `App::start` entry points need to be
    // added once the multi-window overlay spawning + tray + global
    // hotkey subscription are exercised on a real display. Until
    // then this binary exits cleanly after initialising tracing —
    // useful for verifying the workspace links and the per-OS
    // dependency tree resolves.
}
