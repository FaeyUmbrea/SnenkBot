use std::error::Error;
use std::sync::Arc;
use std::time::Duration;

use slint::{ComponentHandle, ModelRc, Timer, TimerMode, VecModel};
use tracing_subscriber::EnvFilter;

use snenk_bot::app::{AppServices, WorkflowCategory, WorkflowStatus};
use snenk_bot::engine::Event;
use snenk_bot::obs::ObsHealth;
use snenk_bot::obs::settings::ObsSettings;
use snenk_bot::paths::AppPaths;
use snenk_bot::runtime::AppRuntime;
use snenk_bot::twitch::credentials::TwitchRole;
use snenk_bot::twitch::login::LoginEvent;
use snenk_bot::ui::{
    AppWindow, AutomationRow, UiInputProvider, connect_editor, connect_run_history, connect_vtube,
    connect_workflow_creation, create_window, refresh_connections, report_runtime_event,
};
use snenk_bot::workflows::WorkflowRepository;

fn show_inventory(window: &slint::Weak<AppWindow>, statuses: Vec<WorkflowStatus>) {
    let usage = |prefix: &str| -> Vec<AutomationRow> {
        statuses
            .iter()
            .filter_map(|status| {
                let labels: Vec<_> = status
                    .capabilities
                    .iter()
                    .filter(|capability| capability.starts_with(prefix))
                    .map(|capability| {
                        status
                            .capability_titles
                            .get(capability)
                            .cloned()
                            .unwrap_or_else(|| capability.clone())
                    })
                    .collect();
                if labels.is_empty() {
                    return None;
                }
                Some(AutomationRow {
                    enabled: true,
                    id: status.id.clone().into(),
                    title: status.title.clone().into(),
                    revision: status.revision.unwrap_or_default() as i32,
                    trigger_summary: labels.join(" · ").into(),
                    ..Default::default()
                })
            })
            .collect()
    };
    let obs_usage = usage("obs.");
    let vtube_usage = usage("vtube.");
    let rows: Vec<_> = statuses
        .into_iter()
        .map(|status| AutomationRow {
            enabled: status.enabled,
            id: status.id.into(),
            title: status.title.into(),
            revision: status.revision.unwrap_or_default() as i32,
            has_steps: status.has_steps,
            trigger_summary: status.trigger_summary.into(),
            step_count: status.step_count.min(i32::MAX as usize) as i32,
            icon_kind: match status.category {
                WorkflowCategory::Chat => 0,
                WorkflowCategory::Broadcast => 1,
                WorkflowCategory::Automation => 2,
            },
            error: status.error.unwrap_or_default().into(),
        })
        .collect();
    if let Err(error) =
        window.upgrade_in_event_loop(move |window| {
            if window.get_selected_automation().is_empty()
                && let Some(first) = rows.first()
            {
                window.set_selected_automation(first.id.clone());
            }
            let selected = window.get_selected_automation();
            window.set_editor_can_run(rows.iter().any(|row| {
                row.id == selected && row.enabled && row.error.is_empty() && row.has_steps
            }));
            window.set_automations(ModelRc::new(VecModel::from(rows)));
            window.set_obs_usage(ModelRc::new(VecModel::from(obs_usage)));
            window.set_vtube_usage(ModelRc::new(VecModel::from(vtube_usage)));
        })
    {
        tracing::debug!(%error, "window no longer accepts workflow inventory");
    }
}

fn obs_health_text(health: ObsHealth) -> String {
    match health {
        ObsHealth::Disabled => "Disabled".into(),
        ObsHealth::Disconnected => "Disconnected".into(),
        ObsHealth::Connecting => "Connecting…".into(),
        ObsHealth::Connected => "Connected".into(),
        ObsHealth::Retrying(message) => format!("Reconnecting: {message}"),
        ObsHealth::Error(message) => message,
    }
}

fn show_twitch_event(window: &slint::Weak<AppWindow>, id: u64, event: LoginEvent) {
    if let Err(error) = window.upgrade_in_event_loop(move |window| {
        if id != 0 && window.get_twitch_attempt_id() != id as i32 {
            return;
        }
        match event {
            LoginEvent::Starting => {
                window.set_twitch_login_stage(1);
                window.set_twitch_url("".into());
                window.set_twitch_code("".into());
                window.set_twitch_status("Requesting a sign-in code…".into());
            }
            LoginEvent::Code { url, code } => {
                window.set_twitch_url(url.into());
                window.set_twitch_code(code.into());
                window.set_twitch_login_stage(2);
                window.set_twitch_status("".into());
            }
            LoginEvent::Review { login, user_id } => {
                window.set_twitch_review_login(login.into());
                window.set_twitch_review_id(user_id.into());
                window.set_twitch_login_stage(3);
                window.set_twitch_status("".into());
            }
            LoginEvent::Connected { role, login } => {
                match role {
                    TwitchRole::Broadcaster => window.set_twitch_broadcaster(login.into()),
                    TwitchRole::Bot => window.set_twitch_bot_account(login.into()),
                }
                window.set_twitch_login_stage(0);
                window.set_twitch_attempt_id(0);
                window.set_twitch_status("Account connected".into());
            }
            LoginEvent::Failed(message) => {
                window.set_twitch_login_stage(0);
                window.set_twitch_attempt_id(0);
                window.set_twitch_status(message.into());
            }
        }
    }) {
        tracing::debug!(%error, "window no longer accepts Twitch sign-in updates");
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    if let Err(error) = run() {
        tracing::error!(%error, "SnenkBot could not finish normally");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let paths = match AppPaths::resolve() {
        Ok(paths) => paths,
        Err(error) => {
            let window = AppWindow::new()?;
            window
                .set_error_message(format!("Could not locate application storage: {error}").into());
            window.run()?;
            return Err(error.into());
        }
    };
    let window = create_window(&paths)?;
    let weak = window.as_weak();
    let mut runtime = match AppRuntime::new(move |event| report_runtime_event(&weak, event)) {
        Ok(runtime) => runtime,
        Err(error) => {
            window
                .set_error_message(format!("Could not start background services: {error}").into());
            window.run()?;
            return Err(error.into());
        }
    };

    let completed_window = window.as_weak();
    let error_window = window.as_weak();
    let inventory_window = window.as_weak();
    let services = Arc::new(AppServices::start(
        &paths,
        &runtime,
        Arc::new(UiInputProvider::new(&window)),
        Arc::new(|event: Event| tracing::debug!(?event, "workflow event")),
        move |completed| {
            let message = format!("{}: {:?}", completed.workflow_id, completed.outcome);
            if let Err(error) = completed_window.upgrade_in_event_loop(move |window| {
                window.set_last_run(message.into());
                window.invoke_refresh_history();
            }) {
                tracing::debug!(%error, "window no longer accepts run results");
            }
        },
        move |message| {
            tracing::error!(%message, "application service error");
            if let Err(error) = error_window.upgrade_in_event_loop(move |window| {
                window.set_error_message(message.into());
            }) {
                tracing::debug!(%error, "window no longer accepts application errors");
            }
        },
        move |statuses| show_inventory(&inventory_window, statuses),
    )?);
    show_inventory(&window.as_weak(), services.list_workflows());
    let run_services = Arc::clone(&services);
    let run_window = window.as_weak();
    window.on_run_automation(move |id| {
        if let Err(error) = run_services.run_manual(&id)
            && let Some(window) = run_window.upgrade()
        {
            window.set_error_message(error.to_string().into());
        }
    });
    connect_editor(
        &window,
        Arc::clone(&services),
        WorkflowRepository::new(&paths),
        runtime.spawner()?,
    );
    connect_workflow_creation(&window, Arc::clone(&services), runtime.spawner()?);
    connect_run_history(
        &window,
        paths.clone(),
        runtime.spawner()?,
        services.action_schemas().to_vec(),
    );
    connect_vtube(&window, Arc::clone(&services), runtime.spawner()?);

    match services.obs().settings().load() {
        Ok(loaded) => {
            window.set_obs_host(loaded.value.host.into());
            window.set_obs_port(loaded.value.port.to_string().into());
            window.set_obs_enabled(loaded.value.enabled);
            window.set_obs_tls(loaded.value.tls);
        }
        Err(error) => {
            window.set_error_message(format!("OBS settings are unavailable: {error}").into())
        }
    }
    window.set_obs_health(obs_health_text(services.obs().health()).into());
    refresh_connections(&window, &services);
    let health_window = window.as_weak();
    let health_services = Arc::clone(&services);
    let health_timer = Timer::default();
    health_timer.start(TimerMode::Repeated, Duration::from_secs(2), move || {
        if let Some(window) = health_window.upgrade() {
            window.set_obs_health(obs_health_text(health_services.obs().health()).into());
            refresh_connections(&window, &health_services);
        }
    });
    let settings_services = Arc::clone(&services);
    let settings_window = window.as_weak();
    let spawner = runtime.spawner()?;
    let save_spawner = spawner.clone();
    window.on_save_obs_settings(move || {
        let Some(window) = settings_window.upgrade() else {
            return;
        };
        if window.get_obs_save_pending() {
            return;
        }
        window.set_obs_save_status("".into());
        let port = match window.get_obs_port().parse::<u16>() {
            Ok(port) if port != 0 => port,
            _ => {
                window.set_error_message("OBS port must be between 1 and 65535".into());
                return;
            }
        };
        let value = ObsSettings {
            enabled: window.get_obs_enabled(),
            host: window.get_obs_host().trim().to_owned(),
            port,
            tls: window.get_obs_tls(),
        };
        window.set_error_message("".into());
        match settings_services.save_obs_settings(value) {
            Ok(reply) => {
                window.set_obs_save_pending(true);
                window.set_obs_save_status("Saving connection…".into());
                let result_window = settings_window.clone();
                if let Err(error) =
                    save_spawner.spawn_task("OBS save result", move |_| async move {
                        let status = match reply.await {
                            Ok(Ok(())) => "Connection saved".to_owned(),
                            Ok(Err(error)) => format!("Could not save connection: {error}"),
                            Err(_) => "OBS settings worker stopped before saving".to_owned(),
                        };
                        let _ = result_window.upgrade_in_event_loop(move |window| {
                            window.set_obs_save_status(status.into());
                            window.set_obs_save_pending(false);
                        });
                        Ok::<(), std::convert::Infallible>(())
                    })
                {
                    window.set_obs_save_status(error.to_string().into());
                    window.set_obs_save_pending(false);
                }
            }
            Err(error) => window.set_obs_save_status(error.to_string().into()),
        }
    });
    let password_services = Arc::clone(&services);
    let password_window = window.as_weak();
    window.on_save_obs_password(move || {
        let Some(window) = password_window.upgrade() else {
            return;
        };
        let password = window.get_obs_password().to_string();
        if password.is_empty() {
            window.set_error_message("Enter an OBS password or choose Remove password".into());
            return;
        }
        if let Err(error) = password_services.save_obs_password(Some(password)) {
            window.set_error_message(error.to_string().into());
        } else {
            window.set_obs_password("".into());
        }
    });
    let clear_services = Arc::clone(&services);
    let clear_window = window.as_weak();
    window.on_clear_obs_password(move || {
        if let Some(window) = clear_window.upgrade() {
            if let Err(error) = clear_services.save_obs_password(None) {
                window.set_error_message(error.to_string().into());
            } else {
                window.set_obs_password("".into());
            }
        }
    });

    let accounts = Arc::clone(services.twitch().accounts());
    let accounts_window = window.as_weak();
    spawner.spawn_task("Twitch account status", move |_| async move {
        let connected = tokio::task::spawn_blocking(move || accounts.connected()).await;
        match connected {
            Ok(Ok(connected)) => {
                let _ = accounts_window.upgrade_in_event_loop(move |window| {
                    if let Some(account) = connected.broadcaster
                        && window.get_twitch_broadcaster().is_empty()
                    {
                        window.set_twitch_broadcaster(account.login.into());
                    }
                    if let Some(account) = connected.bot
                        && window.get_twitch_bot_account().is_empty()
                    {
                        window.set_twitch_bot_account(account.login.into());
                    }
                });
            }
            Ok(Err(error)) => {
                let message = error.to_string();
                let _ = accounts_window.upgrade_in_event_loop(move |window| {
                    window.set_twitch_status(message.into());
                });
            }
            Err(_) => {
                let _ = accounts_window.upgrade_in_event_loop(move |window| {
                    window.set_twitch_status("Twitch account worker stopped".into());
                });
            }
        }
        Ok::<(), std::convert::Infallible>(())
    })?;
    let login_services = Arc::clone(&services);
    let login_window = window.as_weak();
    let login_spawner = spawner.clone();
    window.on_start_twitch_login(move |is_bot| {
        let role = if is_bot {
            TwitchRole::Bot
        } else {
            TwitchRole::Broadcaster
        };
        if let Some(window) = login_window.upgrade() {
            window.set_twitch_login_bot(is_bot);
        }
        let report_window = login_window.clone();
        match login_services
            .twitch()
            .login()
            .start(&login_spawner, role, move |id, event| {
                show_twitch_event(&report_window, id, event);
            }) {
            Ok(id) => {
                if let Some(window) = login_window.upgrade() {
                    window.set_twitch_attempt_id(id as i32);
                }
            }
            Err(error) => {
                if let Some(window) = login_window.upgrade() {
                    window.set_twitch_status(error.into());
                }
            }
        }
    });
    snenk_bot::ui::connect_reconfiguration(&window);
    let approve_services = Arc::clone(&services);
    let approve_window = window.as_weak();
    let approve_spawner = spawner.clone();
    window.on_approve_twitch_login(move || {
        let Some(window) = approve_window.upgrade() else {
            return;
        };
        let user_id = window.get_twitch_review_id().to_string();
        let report_window = approve_window.clone();
        match approve_services.twitch().login().approve(
            &approve_spawner,
            &user_id,
            move |id, event| {
                show_twitch_event(&report_window, id, event);
            },
        ) {
            Ok(()) => {
                window.set_twitch_login_stage(4);
                window.set_twitch_status("Saving account…".into());
            }
            Err(error) => window.set_twitch_status(error.into()),
        }
    });
    let cancel_services = Arc::clone(&services);
    let cancel_window = window.as_weak();
    window.on_cancel_twitch_login(move || {
        if cancel_services.twitch().login().cancel()
            && let Some(window) = cancel_window.upgrade()
        {
            window.set_twitch_login_stage(0);
            window.set_twitch_attempt_id(0);
            window.set_twitch_status("Sign-in cancelled".into());
        }
    });
    let open_window = window.as_weak();
    window.on_open_twitch_url(move || {
        if let Some(window) = open_window.upgrade()
            && let Err(error) = webbrowser::open(&window.get_twitch_url())
        {
            window.set_twitch_status(format!("Could not open the sign-in link: {error}").into());
        }
    });
    let copy_link_window = window.as_weak();
    window.on_copy_twitch_url(move || {
        if let Some(window) = copy_link_window.upgrade() {
            copy_twitch_text(&window, &window.get_twitch_url());
        }
    });
    let copy_code_window = window.as_weak();
    window.on_copy_twitch_code(move || {
        if let Some(window) = copy_code_window.upgrade() {
            copy_twitch_text(&window, &window.get_twitch_code());
        }
    });

    let result = window.run();
    drop(health_timer);
    drop(services);
    let shutdown = runtime.shutdown(Duration::from_secs(5));
    if let Err(error) = &shutdown {
        tracing::error!(%error, "background services did not shut down cleanly");
    }
    result?;
    shutdown?;
    Ok(())
}

fn copy_twitch_text(window: &AppWindow, text: &str) {
    let result = arboard::Clipboard::new().and_then(|mut clipboard| clipboard.set_text(text));
    match result {
        Ok(()) => window.set_twitch_status("Copied to clipboard".into()),
        Err(error) => window.set_twitch_status(format!("Could not copy: {error}").into()),
    }
}
