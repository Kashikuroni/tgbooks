mod backend;
mod platform;
mod ui;

use std::{io::IsTerminal, process::ExitCode, time::Duration};

use anyhow::{Context as _, Result};
use tracing_subscriber::{Layer as _, layer::SubscriberExt as _, util::SubscriberInitExt as _};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // Only contextual application errors reach here; no credentials are read.
            eprintln!("Telegram Books: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    // Deliberately do not accept RUST_LOG=trace for third-party libraries:
    // MTProto/session internals must never accidentally enter application logs.
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(std::io::stderr().is_terminal())
                .with_writer(std::io::stderr)
                .with_filter(tracing_subscriber::filter::filter_fn(|metadata| {
                    application_log_enabled(metadata.target(), *metadata.level())
                })),
        )
        .try_init()
        .context("could not initialize logging")?;

    #[cfg(feature = "gpui-tests")]
    if std::env::args().any(|argument| argument == "--render-ui-preview") {
        return ui::preview::render();
    }

    // GPUI owns the main thread. Tokio lives for the entire native event loop.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("tgbooks-worker")
        .build()
        .context("could not start the background runtime")?;
    let (ui_connection, service_connection) = tgbooks_core::connect();
    let commands = ui_connection.commands.clone();
    let backend = runtime.spawn(backend::run(service_connection));

    // AppKit can terminate the process without returning from the event loop.
    // Await cleanup inside GPUI's quit hook, not only after ui::run returns.
    let shutdown = async move {
        commands.close();
        if backend.await.is_err() {
            tracing::error!("background service terminated unexpectedly");
        }
    };
    let result = ui::run(ui_connection, shutdown);
    runtime.shutdown_timeout(Duration::from_secs(2));
    result
}

fn application_log_enabled(target: &str, level: tracing::Level) -> bool {
    // The binary's crate name is `tgbooks`, while the library crates use `_`.
    (target == "tgbooks" || target.starts_with("tgbooks::") || target.starts_with("tgbooks_"))
        && level <= tracing::Level::INFO
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logs_include_the_binary_but_exclude_third_party_and_verbose_events() {
        assert!(application_log_enabled("tgbooks", tracing::Level::ERROR));
        assert!(application_log_enabled(
            "tgbooks::backend",
            tracing::Level::INFO
        ));
        assert!(application_log_enabled(
            "tgbooks_storage",
            tracing::Level::WARN
        ));
        assert!(!application_log_enabled(
            "tgbooks::backend",
            tracing::Level::DEBUG
        ));
        assert!(!application_log_enabled(
            "tgbooks::backend",
            tracing::Level::TRACE
        ));
        assert!(!application_log_enabled(
            "grammers_client",
            tracing::Level::ERROR
        ));
        assert!(!application_log_enabled("sqlx", tracing::Level::INFO));
        assert!(!application_log_enabled(
            "tgbooks-other",
            tracing::Level::INFO
        ));
    }
}
