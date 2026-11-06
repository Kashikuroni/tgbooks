#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

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
    if std::env::args().any(|argument| argument == "--render-app-icon") {
        return ui::preview::render_app_icon();
    }

    #[cfg(feature = "gpui-tests")]
    if std::env::args().any(|argument| argument == "--render-icon-concepts") {
        return ui::preview::render_icon_concepts();
    }

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
    let cancellation = ui_connection.shutdown.clone();
    let (start, ready) = async_channel::bounded(1);
    let backend = runtime.spawn(async move {
        if wait_for_start(ready, service_connection.shutdown.cancelled()).await {
            backend::run(service_connection).await;
        }
    });

    // AppKit can terminate the process without returning from the event loop.
    // Await cleanup inside GPUI's quit hook, not only after ui::run returns.
    let shutdown = async move {
        cancellation.cancel();
        commands.close();
        if backend.await.is_err() {
            tracing::error!("background service terminated unexpectedly");
        }
    };
    let result = ui::run(ui_connection, shutdown, start, runtime.handle().clone());
    runtime.shutdown_timeout(Duration::from_secs(2));
    result
}

async fn wait_for_start(
    ready: async_channel::Receiver<()>,
    shutdown: impl std::future::Future<Output = ()>,
) -> bool {
    tokio::select! {
        biased;
        _ = shutdown => false,
        result = ready.recv() => result.is_ok(),
    }
}

fn application_log_enabled(target: &str, level: tracing::Level) -> bool {
    // The binary's crate name is `tgbooks`, while the library crates use `_`.
    (target == "tgbooks" || target.starts_with("tgbooks::") || target.starts_with("tgbooks_"))
        && level <= tracing::Level::INFO
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn startup_gate_waits_for_consent_and_shutdown_never_starts_the_backend() {
        let (_ui, backend) = tgbooks_core::connect();
        let (start, ready) = async_channel::bounded(1);
        let waiting = wait_for_start(ready, backend.shutdown.cancelled());
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(Duration::from_millis(5), &mut waiting)
                .await
                .is_err()
        );
        assert!(start.try_send(()).is_ok());
        assert!(waiting.await);

        let (start, ready) = async_channel::bounded(1);
        assert!(start.try_send(()).is_ok());
        backend.shutdown.cancel();
        assert!(!wait_for_start(ready, backend.shutdown.cancelled()).await);
        let (start, ready) = async_channel::bounded(1);
        drop(start);
        assert!(!wait_for_start(ready, std::future::pending()).await);
    }

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
