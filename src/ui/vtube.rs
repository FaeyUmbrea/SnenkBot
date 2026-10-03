use std::sync::Arc;
use std::time::Duration;

use slint::ComponentHandle;

use crate::app::AppServices;
use crate::runtime::RuntimeSpawner;
use crate::vtube::VtubeHealth;
use crate::vtube::settings::VtubeSettings;

use super::AppWindow;

/// Connects the VTube Studio settings surface to the application services.
pub fn connect_vtube(window: &AppWindow, services: Arc<AppServices>, spawner: RuntimeSpawner) {
    window.set_vtube_pending(true);
    let load_window = window.as_weak();
    let settings = services.vtube().settings().clone();
    if let Err(error) = spawner.spawn_task("VTube Studio settings load", move |_| async move {
        let loaded = tokio::task::spawn_blocking(move || settings.load()).await;
        match loaded {
            Ok(Ok(loaded)) => {
                let _ = load_window.upgrade_in_event_loop(move |window| {
                    window.set_vtube_host(loaded.value.host.into());
                    window.set_vtube_port(loaded.value.port.to_string().into());
                    window.set_vtube_enabled(loaded.value.enabled);
                    window.set_vtube_pending(false);
                });
            }
            Ok(Err(error)) => {
                let message = format!("VTube Studio settings are unavailable: {error}");
                let _ = load_window.upgrade_in_event_loop(move |window| {
                    window.set_vtube_status(message.into());
                    window.set_vtube_pending(false);
                });
            }
            Err(error) => {
                let message = format!("VTube Studio settings worker stopped: {error}");
                let _ = load_window.upgrade_in_event_loop(move |window| {
                    window.set_vtube_status(message.into());
                    window.set_vtube_pending(false);
                });
            }
        }
        Ok::<(), std::convert::Infallible>(())
    }) {
        window.set_vtube_status(format!("Could not load VTube Studio settings: {error}").into());
        window.set_vtube_pending(false);
    }

    let weak = window.as_weak();
    let health_services = Arc::clone(&services);
    if let Err(error) =
        spawner.spawn_task("VTube Studio health display", move |shutdown| async move {
            loop {
                let health = health_text(health_services.vtube().health());
                let _ = weak.upgrade_in_event_loop(move |window| {
                    window.set_vtube_health(health.into());
                });
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                }
            }
            Ok::<(), std::convert::Infallible>(())
        })
    {
        window.set_vtube_health(format!("Health updates unavailable: {error}").into());
    }

    let save_services = Arc::clone(&services);
    let save_spawner = spawner.clone();
    let save_window = window.as_weak();
    window.on_save_vtube_settings(move || {
        let Some(window) = save_window.upgrade() else {
            return;
        };
        if window.get_vtube_pending() {
            return;
        }
        let settings = match settings_from_fields(
            &window.get_vtube_host(),
            &window.get_vtube_port(),
            window.get_vtube_enabled(),
        ) {
            Ok(settings) => settings,
            Err(error) => {
                window.set_vtube_status(error.into());
                return;
            }
        };
        window.set_vtube_pending(true);
        window.set_vtube_status("Saving connection settings…".into());
        let reply = match save_services.save_vtube_settings(settings) {
            Ok(reply) => reply,
            Err(error) => {
                window.set_vtube_pending(false);
                window.set_vtube_status(
                    format!("Could not save connection settings: {error}").into(),
                );
                return;
            }
        };
        track_result(&save_spawner, save_window.clone(), "save", reply);
    });

    let authorize_services = Arc::clone(&services);
    let authorize_spawner = spawner.clone();
    let authorize_window = window.as_weak();
    window.on_authorize_vtube(move || {
        let Some(window) = authorize_window.upgrade() else {
            return;
        };
        if window.get_vtube_pending() {
            return;
        }
        window.set_vtube_pending(true);
        window.set_vtube_status("Waiting for VTube Studio authorization…".into());
        let reply = match authorize_services.authorize_vtube() {
            Ok(reply) => reply,
            Err(error) => {
                window.set_vtube_pending(false);
                window.set_vtube_status(format!("Could not start authorization: {error}").into());
                return;
            }
        };
        track_result(
            &authorize_spawner,
            authorize_window.clone(),
            "authorize",
            reply,
        );
    });

    let forget_services = Arc::clone(&services);
    let forget_spawner = spawner;
    let forget_window = window.as_weak();
    window.on_forget_vtube_authorization(move || {
        let Some(window) = forget_window.upgrade() else {
            return;
        };
        if window.get_vtube_pending() {
            return;
        }
        window.set_vtube_pending(true);
        window.set_vtube_status("Removing saved authorization…".into());
        let reply = match forget_services.forget_vtube_authorization() {
            Ok(reply) => reply,
            Err(error) => {
                window.set_vtube_pending(false);
                window.set_vtube_status(format!("Could not remove authorization: {error}").into());
                return;
            }
        };
        track_result(
            &forget_spawner,
            forget_window.clone(),
            "forget authorization",
            reply,
        );
    });
}

fn track_result(
    spawner: &RuntimeSpawner,
    window: slint::Weak<AppWindow>,
    operation: &'static str,
    reply: tokio::sync::oneshot::Receiver<Result<(), String>>,
) {
    let result_window = window.clone();
    if let Err(error) = spawner.spawn_task(
        format!("VTube Studio {operation} result"),
        move |_| async move {
            let status = match reply.await {
                Ok(Ok(())) => match operation {
                    "save" => "Connection settings saved".to_owned(),
                    "authorize" => "VTube Studio authorization saved".to_owned(),
                    _ => "VTube Studio authorization removed".to_owned(),
                },
                Ok(Err(error)) => format!("Could not {operation}: {error}"),
                Err(_) => format!("VTube Studio {operation} worker stopped before replying"),
            };
            let _ = result_window.upgrade_in_event_loop(move |window| {
                window.set_vtube_status(status.into());
                window.set_vtube_pending(false);
            });
            Ok::<(), std::convert::Infallible>(())
        },
    ) && let Some(window) = window.upgrade()
    {
        window
            .set_vtube_status(format!("Could not track VTube Studio {operation}: {error}").into());
        window.set_vtube_pending(false);
    }
}

fn health_text(health: VtubeHealth) -> String {
    match health {
        VtubeHealth::Disabled => "Disabled".into(),
        VtubeHealth::Disconnected => "Disconnected".into(),
        VtubeHealth::Connecting => "Connecting…".into(),
        VtubeHealth::Connected => "Connected".into(),
        VtubeHealth::NeedsAuthorization => "Authorization required".into(),
        VtubeHealth::Retrying(message) => format!("Reconnecting: {message}"),
        VtubeHealth::Error(message) => message,
    }
}

fn settings_from_fields(
    host: &str,
    port: &str,
    enabled: bool,
) -> Result<VtubeSettings, &'static str> {
    let host = host.trim();
    if host.is_empty() {
        return Err("VTube Studio host is required");
    }
    let port = match port.parse::<u16>() {
        Ok(port) if port > 0 => port,
        _ => return Err("VTube Studio port must be between 1 and 65535"),
    };
    Ok(VtubeSettings {
        enabled,
        host: host.to_owned(),
        port,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_fields_trim_host_and_require_a_valid_port() {
        assert_eq!(
            settings_from_fields(" localhost ", "8001", true).unwrap(),
            VtubeSettings {
                enabled: true,
                host: "localhost".into(),
                port: 8001,
            }
        );
        assert_eq!(
            settings_from_fields("localhost", "0", false),
            Err("VTube Studio port must be between 1 and 65535")
        );
        assert_eq!(
            settings_from_fields("localhost", "65536", false),
            Err("VTube Studio port must be between 1 and 65535")
        );
        assert_eq!(
            settings_from_fields("   ", "8001", false),
            Err("VTube Studio host is required")
        );
    }
}
