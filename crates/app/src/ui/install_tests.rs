use super::*;
use crate::platform::install::{Destination, InstallError, Locations, Proposal};
use gpui::TestAppContext;
use std::{cell::Cell, path::PathBuf, rc::Rc};

fn plan() -> Proposal {
    Proposal {
        source: "/Downloads/Telegram Books.app".into(),
        locations: Locations {
            system: "/Applications".into(),
            user: Some("/Users/example/Applications".into()),
        },
    }
}

fn window(cx: &mut TestAppContext) -> WindowHandle<BookApp> {
    let (ui, _backend) = tgbooks_core::connect();
    cx.open_window(gpui::size(gpui::px(760.), gpui::px(520.)), |_, cx| {
        BookApp::new(ui, cx)
    })
}

#[test]
fn custom_data_profiles_and_debug_runs_skip_installation() {
    assert!(eligible(true, false));
    assert!(!eligible(false, false));
    assert!(!eligible(true, true));
    assert!(!eligible(false, true));
}

#[gpui::test]
fn declining_starts_backend_without_running_any_installation(cx: &mut TestAppContext) {
    let window = window(cx);
    let (start, ready) = async_channel::bounded(1);
    let called = Rc::new(Cell::new(false));
    let installed = called.clone();
    cx.update(|cx| {
        prompt(
            window,
            plan(),
            start,
            move |_, _| {
                installed.set(true);
                std::future::ready(Ok(PathBuf::from("/unused.app")))
            },
            cx,
        )
    });
    assert!(cx.has_pending_prompt());
    assert!(ready.try_recv().is_err());
    cx.simulate_prompt_answer("Не сейчас");
    cx.run_until_parked();
    assert_eq!(ready.try_recv(), Ok(()));
    assert!(!called.get());
}

#[gpui::test]
async fn successful_user_install_restarts_the_selected_bundle_without_starting_backend(
    cx: &mut TestAppContext,
) {
    let window = window(cx);
    let (start, ready) = async_channel::bounded(1);
    let restart = cx.expect_restart();
    cx.update(|cx| {
        prompt(
            window,
            plan(),
            start,
            move |_, destination| {
                assert_eq!(destination, Destination::User);
                std::future::ready(Ok(PathBuf::from(
                    "/Users/example/Applications/Telegram Books.app",
                )))
            },
            cx,
        )
    });
    assert!(ready.try_recv().is_err());
    cx.simulate_prompt_answer("Только для меня");
    cx.run_until_parked();
    let Ok((path, _)) = restart.await else {
        panic!("restart not requested");
    };
    assert_eq!(
        path,
        Some("/Users/example/Applications/Telegram Books.app".into())
    );
    assert!(ready.try_recv().is_err());
}

#[gpui::test]
fn installation_failure_explains_the_problem_before_starting_the_backend(cx: &mut TestAppContext) {
    let window = window(cx);
    let (start, ready) = async_channel::bounded(1);
    cx.update(|cx| {
        prompt(
            window,
            plan(),
            start,
            move |_, destination| {
                assert_eq!(destination, Destination::System);
                std::future::ready(Err(InstallError::Occupied))
            },
            cx,
        )
    });
    cx.simulate_prompt_answer("Установить");
    cx.run_until_parked();
    assert!(ready.try_recv().is_err());
    let Some((_, detail)) = cx.pending_prompt() else {
        panic!("error prompt missing");
    };
    assert!(detail.contains("Существующая копия не изменена"));
    cx.simulate_prompt_answer("Продолжить здесь");
    cx.run_until_parked();
    assert_eq!(ready.try_recv(), Ok(()));
}
