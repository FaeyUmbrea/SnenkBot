use slint::{ComponentHandle, ModelRc, VecModel};

use crate::app::AppServices;
use crate::obs::ObsHealth;
use crate::twitch::credentials::TwitchRole;
use crate::twitch::listener::TwitchHealth;
use crate::twitch::session::TwitchAuthorizationHealth;
use crate::vtube::VtubeHealth;

use super::{AppWindow, ConnectionRow};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Inactive,
    Connecting,
    Connected,
    Error,
}

fn row(title: &str, state: State, detail: String) -> ConnectionRow {
    ConnectionRow {
        title: title.into(),
        state: match state {
            State::Inactive => 0,
            State::Connecting => 1,
            State::Connected => 2,
            State::Error => 3,
        },
        detail: detail.into(),
        ..Default::default()
    }
}

fn summarize(rows: &[ConnectionRow]) -> (i32, i32, i32) {
    let total = rows.iter().filter(|row| row.state != 0).count() as i32;
    let connected = rows.iter().filter(|row| row.state == 2).count() as i32;
    let state = if rows.iter().any(|row| row.state == 3) {
        3
    } else if connected < total {
        1
    } else if total == 0 {
        0
    } else {
        2
    };
    (connected, total, state)
}

pub fn refresh_connections(window: &AppWindow, services: &AppServices) {
    let requests = services.reconfiguration_requests();
    let mut rows = Vec::new();
    let (state, detail) = match services.obs().health() {
        ObsHealth::Disabled => (State::Inactive, "Disabled".into()),
        ObsHealth::Disconnected => (
            State::Connecting,
            "Disconnected — waiting to connect".into(),
        ),
        ObsHealth::Connecting => (State::Connecting, "Connecting…".into()),
        ObsHealth::Connected => (State::Connected, "Connected".into()),
        ObsHealth::Retrying(error) => (State::Error, format!("Retrying: {error}")),
        ObsHealth::Error(error) => (State::Error, error),
    };
    rows.push(row("OBS Studio", state, detail));
    for (role, title) in [
        (TwitchRole::Broadcaster, "Twitch broadcaster account"),
        (TwitchRole::Bot, "Twitch bot account (optional)"),
    ] {
        let (state, detail) = match services.twitch().authorization_health(role) {
            TwitchAuthorizationHealth::Unconfigured => (State::Inactive, "Not configured".into()),
            TwitchAuthorizationHealth::Checking => {
                (State::Connecting, "Checking authorization…".into())
            }
            TwitchAuthorizationHealth::Connected => (State::Connected, "Signed in".into()),
            TwitchAuthorizationHealth::Error(error) => (State::Error, error),
        };
        let mut account_row = row(title, state, detail);
        let connection = match role {
            TwitchRole::Broadcaster => "broadcaster",
            TwitchRole::Bot => "bot",
        };
        if let Some(request) = requests
            .iter()
            .find(|request| request.integration == "twitch" && request.connection == connection)
        {
            account_row.state = 3;
            account_row.detail = format!(
                "Reconnect required · {}\n{}",
                request.affected_features.join(", "),
                request.reason
            )
            .into();
            account_row.integration = request.integration.into();
            account_row.connection = request.connection.into();
        }
        rows.push(account_row);
    }
    let (state, detail) = match services.twitch().health() {
        TwitchHealth::Disabled => (State::Inactive, "No active listeners".into()),
        TwitchHealth::Connecting => (State::Connecting, "Connecting…".into()),
        TwitchHealth::Connected => (State::Connected, "Connected".into()),
        TwitchHealth::Degraded(error)
        | TwitchHealth::Retrying(error)
        | TwitchHealth::Error(error) => (State::Error, error),
    };
    rows.push(row("Twitch events", state, detail));
    let (state, detail) = match services.vtube().health() {
        VtubeHealth::Disabled => (State::Inactive, "Disabled".into()),
        VtubeHealth::Disconnected => (
            State::Connecting,
            "Disconnected — waiting to connect".into(),
        ),
        VtubeHealth::Connecting => (State::Connecting, "Connecting…".into()),
        VtubeHealth::NeedsAuthorization => (
            State::Error,
            "Authorization required in VTube Studio".into(),
        ),
        VtubeHealth::Connected => (State::Connected, "Connected".into()),
        VtubeHealth::Retrying(error) | VtubeHealth::Error(error) => (State::Error, error),
    };
    rows.push(row("VTube Studio", state, detail));
    let (connected, total, state) = summarize(&rows);
    window.set_connection_count(connected);
    window.set_connection_total(total);
    window.set_connection_state(state);
    window.set_connections(ModelRc::new(VecModel::from(rows)));
    match services.obs().password_present() {
        Ok(present) => {
            window.set_obs_password_stored(present);
            window.set_obs_password_status_error(false);
        }
        Err(_) => window.set_obs_password_status_error(true),
    }
}

/// Routes a module's configuration action without discarding the open workflow.
pub fn connect_reconfiguration(window: &AppWindow) {
    let weak = window.as_weak();
    window.on_reconfigure_connection(move |integration, connection| {
        if integration == "twitch"
            && (connection == "broadcaster" || connection == "bot")
            && let Some(window) = weak.upgrade()
        {
            if window.get_reconfigure_return_page() < 0 {
                window.set_reconfigure_return_page(window.get_page());
            }
            window.set_page(5);
            window.invoke_start_twitch_login(connection == "bot");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn errors_take_precedence_and_optional_connections_do_not_count() {
        let mut rows = vec![
            row("optional", State::Inactive, String::new()),
            row("connected", State::Connected, String::new()),
        ];
        assert_eq!(summarize(&rows), (1, 1, 2));
        rows.push(row("connecting", State::Connecting, String::new()));
        assert_eq!(summarize(&rows), (1, 2, 1));
        rows.push(row("error", State::Error, String::new()));
        assert_eq!(summarize(&rows), (1, 3, 3));
        assert_eq!(summarize(&[]), (0, 0, 0));
    }
}
