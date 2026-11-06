//! Startup-only macOS prompt; Core remains asleep until installation is decided.
use async_channel::Sender;
use gpui::{App, WindowHandle};

use super::BookApp;

pub(super) fn offer(
    window: WindowHandle<BookApp>,
    start: Sender<()>,
    runtime: tokio::runtime::Handle,
    cx: &mut App,
) {
    #[cfg(target_os = "macos")]
    if eligible(
        !cfg!(debug_assertions),
        std::env::var_os("TGBOOKS_DATA_DIR").is_some(),
    ) {
        use crate::platform::install::{Locations, proposal};
        // GPUI's macOS app_path uses NSBundle.bundlePath, not the CWD or argv[0].
        if let Ok(bundle) = cx.app_path() {
            let detect =
                runtime.spawn_blocking(move || proposal(&bundle, Locations::system(), true));
            cx.spawn(async move |cx| {
                if let Ok(Some(plan)) = detect.await {
                    cx.update(|cx| {
                        prompt(
                            window,
                            plan,
                            start,
                            move |plan, destination| async move {
                                runtime
                                    .spawn_blocking(move || {
                                        crate::platform::install::install(plan, destination)
                                    })
                                    .await
                                    .unwrap_or(Err(crate::platform::install::InstallError::Copy))
                            },
                            cx,
                        )
                    });
                } else {
                    let _ = start.try_send(());
                }
            })
            .detach();
            return;
        }
    }
    let _ = (window, runtime, cx);
    let _ = start.try_send(());
}

#[cfg(target_os = "macos")]
fn eligible(release: bool, custom_data: bool) -> bool {
    // LaunchServices restart does not forward arbitrary environment variables.
    // Never redirect a custom-data/testing run to the normal profile.
    release && !custom_data
}

#[cfg(target_os = "macos")]
fn prompt<
    Fut: std::future::Future<
            Output = Result<std::path::PathBuf, crate::platform::install::InstallError>,
        > + 'static,
>(
    window: WindowHandle<BookApp>,
    plan: crate::platform::install::Proposal,
    start: Sender<()>,
    install: impl FnOnce(
        crate::platform::install::Proposal,
        crate::platform::install::Destination,
    ) -> Fut
    + 'static,
    cx: &mut App,
) {
    use crate::platform::install::Destination;
    use gpui::{PromptButton, PromptLevel};
    tracing::info!("offering application installation");
    let mut buttons = vec![
        PromptButton::ok("Установить"),
        PromptButton::cancel("Не сейчас"),
    ];
    let detail = if let Some(user) = &plan.locations.user {
        buttons.push(PromptButton::new("Только для меня"));
        format!(
            "Приложение будет скопировано в {} и перезапущено оттуда.\n\
             «Только для меня» — установка в {} без прав администратора.\n\
             Исходная копия останется на месте. Данные и Telegram-сессия не перемещаются.",
            plan.locations.system.display(),
            user.display(),
        )
    } else {
        format!(
            "Приложение будет скопировано в {} и перезапущено оттуда. Исходная копия и данные сохранятся.",
            plan.locations.system.display()
        )
    };
    let answer = window.update(cx, |_, window, cx| {
        window.prompt(
            PromptLevel::Info,
            "Установить Telegram Books в «Программы»?",
            Some(&detail),
            &buttons,
            cx,
        )
    });
    cx.spawn(async move |cx| {
        let destination = match answer {
            Ok(answer) => match answer.await {
                Ok(0) => Some(Destination::System),
                Ok(2) if plan.locations.user.is_some() => Some(Destination::User),
                _ => None,
            },
            Err(_) => None,
        };
        let Some(destination) = destination else {
            let _ = start.try_send(());
            return;
        };
        let directory = plan.directory(destination).map(std::path::Path::to_owned);
        match install(plan, destination).await {
            Ok(installed) => {
                tracing::info!("application installed; restarting");
                // The pinned GPUI implementation waits for this PID to exit
                // before `open` on the new bundle. Its quit hook releases locks.
                cx.update(|cx| {
                    cx.set_restart_path(installed);
                    cx.restart();
                });
            }
            Err(error) => {
                let answer = window.update(cx, |_, window, cx| {
                    window.prompt(
                        PromptLevel::Warning,
                        "Приложение не установлено",
                        Some(error.message()),
                        &[
                            PromptButton::cancel("Продолжить здесь"),
                            PromptButton::new("Открыть папку"),
                        ],
                        cx,
                    )
                });
                if let Ok(answer) = answer
                    && matches!(answer.await, Ok(1))
                    && let Some(directory) = directory
                {
                    cx.update(|cx| cx.reveal_path(&directory));
                }
                let _ = start.try_send(());
            }
        }
    })
    .detach();
}

#[cfg(all(test, feature = "gpui-tests", target_os = "macos"))]
#[path = "install_tests.rs"]
mod tests;
