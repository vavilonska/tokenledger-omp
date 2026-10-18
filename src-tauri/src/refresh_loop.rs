use std::sync::{atomic::AtomicU64, Arc};

use tauri::{AppHandle, Emitter};

use crate::{
    commands::settings::emit_settings_if_account_changed, notifications::finish_refresh,
    pacing::NotificationEvaluator, policy::REFRESH_INTERVAL, service::ProviderService,
    settings::SettingsService,
};

pub fn spawn(
    app: AppHandle,
    service: Arc<ProviderService>,
    settings: Arc<SettingsService>,
    notifications: Arc<NotificationEvaluator>,
) {
    tauri::async_runtime::spawn(async move {
        let mut schedule_changed = service.subscribe_refresh_schedule();
        loop {
            schedule_changed.borrow_and_update();
            let provider_ids = settings.enabled_provider_ids();
            let delay = service
                .next_refresh_delay(&provider_ids)
                .unwrap_or(REFRESH_INTERVAL);
            if !delay.is_zero() {
                let _ = tokio::time::timeout(delay, schedule_changed.changed()).await;
                continue;
            }
            if !provider_ids.is_empty() {
                let progress_app = app.clone();
                let progress_settings = settings.clone();
                let observed_account_revision =
                    Arc::new(AtomicU64::new(settings.account_revision()));
                let progress_account_revision = observed_account_revision.clone();
                let state = service
                    .refresh_all_with_progress(&provider_ids, false, move |state| {
                        emit_settings_if_account_changed(
                            &progress_app,
                            &progress_settings,
                            &progress_account_revision,
                        );
                        let _ = progress_app.emit("usage-state", state);
                    })
                    .await;
                emit_settings_if_account_changed(&app, &settings, &observed_account_revision);
                let _ = app.emit("usage-state", &state);
                finish_refresh(&app, &state, &settings, &notifications);
            }
        }
    });
}
