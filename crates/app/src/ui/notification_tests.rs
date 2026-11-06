use super::*;

fn key(channel: i64, message: i32) -> DownloadKey {
    DownloadKey {
        channel_id: channel,
        message_id: message,
        document_id: i64::from(message),
    }
}

#[test]
fn batches_group_unique_findings_without_file_names_and_replays_are_ignored() {
    let mut notes = Notifications::default();
    notes.set_account(Some(1));
    let a = key(-1, 1);
    let b = key(-2, 2);
    assert!(notes.add(RecommendationBatch {
        account: 1,
        keys: vec![a, a]
    }));
    assert!(notes.add(RecommendationBatch {
        account: 1,
        keys: vec![b]
    }));
    let Some(notification) = notes.take(&[a, b].into(), false, true) else {
        panic!("batch missing");
    };
    assert!(notification.body.contains("2 книги") && notification.body.contains("2 канала"));
    assert_eq!(notification.tag, TAG);
    assert!(!notes.add(RecommendationBatch {
        account: 1,
        keys: vec![a, b]
    }));
    let response = SystemNotificationResponse {
        tag: TAG.into(),
        action_id: None,
    };
    assert!(notes.accepts(&response, Some(1)));
    assert!(!notes.accepts(&response, Some(2)));
    notes.set_account(Some(2));
    assert!(!notes.accepts(&response, Some(2)));
}

#[test]
fn foreground_disabled_and_no_longer_unread_findings_do_not_reappear_later() {
    let a = key(-1, 1);
    let mut notes = Notifications::default();
    notes.set_account(Some(1));
    for (active, enabled, eligible) in [
        (true, true, true),
        (false, false, true),
        (false, true, false),
    ] {
        notes.set_account(Some(2));
        notes.set_account(Some(1));
        notes.add(RecommendationBatch {
            account: 1,
            keys: vec![a],
        });
        let keys = if eligible { [a].into() } else { HashSet::new() };
        assert!(notes.take(&keys, active, enabled).is_none());
        assert!(!notes.add(RecommendationBatch {
            account: 1,
            keys: vec![a]
        }));
    }
    assert!(!notes.add(RecommendationBatch {
        account: 2,
        keys: vec![a]
    }));
}

#[cfg(feature = "gpui-tests")]
mod gpui_tests {
    use super::*;
    use crate::ui::{BookApp, Screen};
    use gpui::{TestAppContext, VisualTestContext, px, size};
    use tgbooks_core::{
        AppInfo, AppSettings, BookAttachment, BookFormat, CoreEvent, Recommendation,
        RecommendationState, RecommendationStatus,
    };

    #[gpui::test]
    fn inactive_window_gets_one_group_and_click_opens_the_inbox_without_downloading(
        cx: &mut TestAppContext,
    ) {
        let (ui, backend) = tgbooks_core::connect();
        cx.update(|cx| cx.set_app_identity(crate::platform::APP_ID, crate::platform::APP_NAME));
        let keys: Vec<_> = (1..=2).map(|id| key(-i64::from(id), id)).collect();
        let window = cx.open_window(size(px(1040.), px(700.)), |_, cx| {
            let mut app = BookApp::new(ui, cx);
            app.notification_available = true;
            let mut settings = AppSettings::new(std::env::temp_dir());
            settings.system_notifications = true;
            app.apply_event(
                CoreEvent::Ready(AppInfo {
                    settings,
                    data_directory: "data".into(),
                    download_directory: "books".into(),
                }),
                cx,
            );
            app.apply_event(
                CoreEvent::Recommendations(RecommendationState {
                    account: Some(1),
                    loaded: true,
                    items: keys
                        .iter()
                        .map(|&key| Recommendation {
                            book: BookAttachment {
                                key,
                                file_name: "private.epub".into(),
                                size_bytes: Some(3),
                                format: BookFormat::Epub,
                            },
                            source_title: "Private source".into(),
                            message_date: 1,
                            status: RecommendationStatus::Unread,
                        })
                        .collect(),
                    ..Default::default()
                }),
                cx,
            );
            app
        });
        cx.update(|cx| register(window, cx));
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, _| window.activate_window());
        visual.run_until_parked();
        visual.deactivate_window();
        let updated = window.update(&mut visual.cx, |app, _, cx| {
            app.apply_event(
                CoreEvent::RecommendationBatch(RecommendationBatch {
                    account: 1,
                    keys: vec![keys[0]],
                }),
                cx,
            );
            app.apply_event(
                CoreEvent::RecommendationBatch(RecommendationBatch {
                    account: 1,
                    keys: vec![keys[1]],
                }),
                cx,
            );
        });
        assert!(updated.is_ok());
        visual.run_until_parked();
        assert!(visual.cx.shown_system_notifications().is_empty());
        visual.cx.executor().advance_clock(GROUP_DELAY);
        visual.run_until_parked();
        let shown = visual.cx.shown_system_notifications();
        assert_eq!(shown.len(), 1);
        assert!(!shown[0].body.contains("private") && !shown[0].body.contains("Private"));
        visual
            .cx
            .simulate_system_notification_response(SystemNotificationResponse {
                tag: TAG.into(),
                action_id: Some(OPEN.into()),
            });
        visual.run_until_parked();
        let Ok(app) = window.root(&mut visual.cx) else {
            panic!("window missing");
        };
        app.read_with(&visual.cx, |app, _| {
            assert_eq!(app.screen, Screen::Channels);
            assert!(app.show_recommendations);
        });
        visual.update(|window, _| assert!(window.is_window_active()));
        assert!(backend.commands.try_recv().is_err());
    }
}
