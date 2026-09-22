use std::thread;

use tauri::{AppHandle, Manager};
use tauri_plugin_notification::{NotificationExt, PermissionState};

use crate::{
    localization::uses_simplified_chinese,
    models::{LanguagePreference, ProviderSnapshot, ProviderViewState},
    pacing::{Milestone, NotificationEvaluator, PaceAlert},
    popup::PopupDismissGuard,
    service::UsageViewState,
    settings::SettingsService,
    tray_presentation,
    window::{show_main_window, MAIN_WINDOW},
};

pub fn permission(app: &AppHandle) -> &'static str {
    match app.notification().permission_state() {
        Ok(PermissionState::Granted) => "granted",
        Ok(PermissionState::Denied) => "denied",
        Ok(PermissionState::Prompt | PermissionState::PromptWithRationale) => "prompt",
        Err(_) => "unavailable",
    }
}

pub fn finish_refresh(
    app: &AppHandle,
    state: &UsageViewState,
    settings: &SettingsService,
    notifications: &NotificationEvaluator,
) {
    let preferences = settings.get();
    tray_presentation::update(app, state, &preferences, settings.registry());
    notifications.prune(&preferences);
    for snapshot in state.providers.values().filter_map(notification_snapshot) {
        let alerts = notifications.evaluate(
            snapshot,
            &preferences,
            settings.registry(),
            chrono::Utc::now(),
        );
        let failed = deliver(app, &alerts, preferences.language);
        if !failed.is_empty() {
            notifications.rollback(&failed);
        }
    }
}

fn notification_snapshot(state: &ProviderViewState) -> Option<&ProviderSnapshot> {
    // A refresh error can coexist with a retained last-good snapshot. The error is
    // shown to the user, but it must not suppress time-based pace evaluation.
    state.snapshot.as_ref()
}

fn deliver(app: &AppHandle, alerts: &[PaceAlert], language: LanguagePreference) -> Vec<PaceAlert> {
    if permission(app) != "granted" {
        if !alerts.is_empty() {
            crate::app_debug!(
                "notifications",
                "skipped {} alerts because permission is unavailable",
                alerts.len()
            );
        }
        return alerts.to_vec();
    }
    alerts
        .iter()
        .filter_map(|alert| {
            let (title, body) = localized_milestone(alert.milestone, language);
            let result = show(
                app,
                title,
                &format!("{} · {}\n{}", alert.provider, alert.metric, body),
                language,
            );
            if result.is_ok() {
                crate::app_info!("notifications", "pace alert delivered");
                None
            } else {
                crate::app_error!("notifications", "pace alert delivery failed");
                Some(alert.clone())
            }
        })
        .collect()
}

fn localized_milestone(
    milestone: Milestone,
    language: LanguagePreference,
) -> (&'static str, &'static str) {
    if !uses_simplified_chinese(language) {
        return (milestone.title(), milestone.body());
    }
    match milestone {
        Milestone::AlmostOut => ("即将用尽", "此周期的剩余用量已低于 10%。"),
        Milestone::CuttingItClose => ("接近用尽", "预计周期结束时将接近用量上限。"),
        Milestone::WillRunOut => ("预计用尽", "预计将在限额重置前用尽。"),
    }
}

fn show(
    app: &AppHandle,
    title: &str,
    body: &str,
    _language: LanguagePreference,
) -> Result<(), String> {
    let mut notification = notify_rust::Notification::new();
    notification
        .summary(title)
        .body(body)
        .appname("TokenLedger OMP");
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    notification.action(
        "default",
        if uses_simplified_chinese(_language) {
            "打开 TokenLedger OMP"
        } else {
            "Open TokenLedger OMP"
        },
    );
    #[cfg(target_os = "windows")]
    notification.app_id(&app.config().identifier);
    #[cfg(target_os = "macos")]
    let _ = notify_rust::set_application(if tauri::is_dev() {
        "com.apple.Terminal"
    } else {
        &app.config().identifier
    });

    let handle = notification
        .show()
        .map_err(|_| "The notification could not be delivered.".to_owned())?;
    let app = app.clone();
    thread::spawn(move || {
        let _ = handle.wait_for_response(move |response: &notify_rust::NotificationResponse| {
            if !response_opens_window(response) {
                return;
            }
            let app_for_window = app.clone();
            let _ = app.run_on_main_thread(move || {
                app_for_window.state::<PopupDismissGuard>().cancel_pending();
                if let Some(window) = app_for_window.get_webview_window(MAIN_WINDOW) {
                    show_main_window(&window);
                }
            });
        });
    });
    Ok(())
}

fn response_opens_window(response: &notify_rust::NotificationResponse) -> bool {
    matches!(
        response,
        notify_rust::NotificationResponse::Default | notify_rust::NotificationResponse::Action(_)
    )
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use notify_rust::{CloseReason, NotificationResponse};

    use crate::{
        models::{LanguagePreference, ProviderSnapshot, ProviderViewState, UsageHistory},
        pacing::Milestone,
    };

    use super::{localized_milestone, notification_snapshot, response_opens_window};

    #[test]
    fn explicit_chinese_preference_localizes_pacing_notifications() {
        assert_eq!(
            localized_milestone(Milestone::WillRunOut, LanguagePreference::SimplifiedChinese),
            ("预计用尽", "预计将在限额重置前用尽。")
        );
        assert_eq!(
            localized_milestone(Milestone::AlmostOut, LanguagePreference::English),
            ("Almost Out", "Under 10% usage remaining for this window.")
        );
    }

    #[test]
    fn notification_clicks_open_the_window_but_dismissals_do_not() {
        assert!(response_opens_window(&NotificationResponse::Default));
        assert!(response_opens_window(&NotificationResponse::Action(
            "open".into()
        )));
        assert!(!response_opens_window(&NotificationResponse::Closed(
            CloseReason::Dismissed
        )));
    }

    #[test]
    fn refresh_error_does_not_hide_the_retained_notification_snapshot() {
        let state = ProviderViewState {
            snapshot: Some(ProviderSnapshot {
                provider_id: "codex".into(),
                plan: None,
                quotas: Vec::new(),
                value_metrics: Vec::new(),
                status_metrics: Vec::new(),
                notices: Vec::new(),
                usage: UsageHistory::default(),
                warnings: Vec::new(),
                refreshed_at: Utc::now(),
            }),
            error: Some("The latest refresh failed.".into()),
            ..ProviderViewState::default()
        };

        assert_eq!(
            notification_snapshot(&state).map(|snapshot| snapshot.provider_id.as_str()),
            Some("codex")
        );
    }
}
