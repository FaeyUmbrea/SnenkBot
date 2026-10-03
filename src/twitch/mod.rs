pub mod accounts;
pub mod actions;
mod channel_changes;
pub mod credentials;
pub mod device;
mod echoes;
pub mod events;
pub mod eventsub;
pub mod helix;
pub mod listener;
pub mod login;
pub mod session;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::watch;

use crate::engine::Engine;
use crate::integration::{ConnectionState, ConnectionStatus, StatusCell};
use crate::paths::AppPaths;
use crate::runtime::{AppRuntime, RuntimeError};
use crate::storage::ConfigStore;

use self::accounts::TwitchAccounts;
use self::credentials::{SystemCredentialBackend, TwitchCredentialStore};
use self::device::{DeviceAuth, ReqwestTransport};
use self::echoes::ChatEchoes;
use self::helix::Helix;
use self::listener::{TwitchHealth, TwitchListener};
use self::login::TwitchLogin;
use self::session::TwitchSession;
use crate::workflows::WorkflowDefinition;

pub use self::listener::TwitchActivation;
pub use events::trigger_value_schema;

const AUTHORIZATION_RETRY: Duration = Duration::from_secs(30);

#[derive(Default)]
struct AuthorizationRetry {
    broadcaster: Option<Instant>,
    bot: Option<Instant>,
}

impl AuthorizationRetry {
    fn deadline(&mut self, role: credentials::TwitchRole) -> &mut Option<Instant> {
        match role {
            credentials::TwitchRole::Broadcaster => &mut self.broadcaster,
            credentials::TwitchRole::Bot => &mut self.bot,
        }
    }

    fn should_authorize(
        &mut self,
        role: credentials::TwitchRole,
        health: &session::TwitchAuthorizationHealth,
        now: Instant,
    ) -> bool {
        let deadline = self.deadline(role);
        match health {
            session::TwitchAuthorizationHealth::Checking => {
                *deadline = None;
                true
            }
            session::TwitchAuthorizationHealth::Error(_) => {
                deadline.is_none_or(|deadline| now >= deadline)
            }
            session::TwitchAuthorizationHealth::Unconfigured
            | session::TwitchAuthorizationHealth::Connected => {
                *deadline = None;
                false
            }
        }
    }

    fn attempted(&mut self, role: credentials::TwitchRole, now: Instant) {
        *self.deadline(role) = Some(now + AUTHORIZATION_RETRY);
    }
}

/// Owns Twitch accounts, actions, EventSub subscriptions, and listener health.
pub struct TwitchIntegration {
    accounts: Arc<TwitchAccounts<SystemCredentialBackend>>,
    login: TwitchLogin,
    session: Arc<TwitchSession<SystemCredentialBackend, ReqwestTransport>>,
    helix: Arc<Helix>,
    echoes: Arc<ChatEchoes>,
    routes: watch::Sender<Vec<WorkflowDefinition>>,
    health: Arc<StatusCell<TwitchHealth>>,
    required_events: StatusCell<Result<BTreeSet<events::EventSubKind>, String>>,
}

impl TwitchIntegration {
    pub fn register(paths: &AppPaths, engine: &mut Engine) -> Self {
        let accounts = Arc::new(TwitchAccounts::new(
            ConfigStore::new(paths.integrations_dir().join("twitch")),
            TwitchCredentialStore::system(device::CLIENT_ID),
        ));
        let session = Arc::new(TwitchSession::new(
            Arc::clone(&accounts),
            DeviceAuth::new(ReqwestTransport::default()),
        ));
        let helix = Arc::new(Helix::default());
        let echoes = Arc::new(ChatEchoes::default());
        actions::register(
            engine,
            Arc::clone(&session),
            Arc::clone(&helix),
            Arc::clone(&echoes),
        );
        let login = TwitchLogin::new(Arc::clone(&accounts), DeviceAuth::default());
        let (routes, _) = watch::channel(Vec::new());
        Self {
            accounts,
            login,
            session,
            helix,
            echoes,
            routes,
            health: Arc::new(StatusCell::new(TwitchHealth::Disabled)),
            required_events: StatusCell::new(Ok(BTreeSet::new())),
        }
    }

    pub fn accounts(&self) -> &Arc<TwitchAccounts<SystemCredentialBackend>> {
        &self.accounts
    }

    pub fn login(&self) -> &TwitchLogin {
        &self.login
    }

    pub fn health(&self) -> TwitchHealth {
        self.health
            .read()
            .expect("Twitch health lock poisoned")
            .clone()
    }

    pub fn set_routes(&self, definitions: Vec<WorkflowDefinition>) {
        let required = events::required_kinds(&definitions);
        self.routes.send_replace(definitions);
        self.required_events
            .set(required)
            .expect("Twitch event demand lock poisoned");
    }

    pub fn authorization_health(
        &self,
        role: credentials::TwitchRole,
    ) -> session::TwitchAuthorizationHealth {
        self.session.authorization_health(role)
    }

    pub fn start_authorization(&self, runtime: &AppRuntime) -> Result<(), RuntimeError> {
        let session = Arc::clone(&self.session);
        runtime.spawn_task("Twitch authorization", move |shutdown| async move {
            let mut retry = AuthorizationRetry::default();
            loop {
                for (role, capability) in [
                    (credentials::TwitchRole::Broadcaster, session::TwitchCapability::ChannelOwner),
                    (credentials::TwitchRole::Bot, session::TwitchCapability::BotFeature),
                ] {
                    if retry.should_authorize(role, &session.authorization_health(role), Instant::now()) {
                        // Authorization updates the role's health and never reads Keychain here.
                        let _ = tokio::select! {
                            _ = shutdown.cancelled() => return Ok::<(), std::convert::Infallible>(()),
                            result = session.authorize(capability) => result,
                        };
                        retry.attempted(role, Instant::now());
                    }
                }
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_secs(2)) => {},
                }
            }
            Ok::<(), std::convert::Infallible>(())
        })
    }

    pub fn start_listener(
        &self,
        runtime: &AppRuntime,
        on_activation: impl Fn(TwitchActivation) + Send + Sync + 'static,
        on_error: impl Fn(String) + Send + Sync + 'static,
    ) -> Result<(), RuntimeError> {
        let listener = TwitchListener::new(
            Arc::clone(&self.accounts),
            Arc::clone(&self.session),
            Arc::clone(&self.helix),
            Arc::clone(&self.echoes),
            self.routes.subscribe(),
            Arc::clone(&self.health),
        );
        runtime.spawn_task("Twitch events", move |shutdown| async move {
            listener.run(shutdown, on_activation, on_error).await;
            Ok::<(), std::convert::Infallible>(())
        })
    }
}

impl crate::integration::ConfigurationStatus for TwitchIntegration {
    fn subscribe_configuration_changes(&self) -> Vec<watch::Receiver<u64>> {
        let mut subscriptions = vec![
            self.health.subscribe(),
            self.accounts.subscribe_configuration_changes(),
            self.required_events.subscribe(),
        ];
        subscriptions.extend(self.session.subscribe_health_changes());
        subscriptions
    }

    fn connection_statuses(&self) -> Vec<ConnectionStatus> {
        let requests = self.reconfiguration_requests();
        let accounts = self.accounts.configured_accounts().unwrap_or_default();
        let mut statuses = Vec::new();
        for (role, connection, title) in [
            (
                credentials::TwitchRole::Broadcaster,
                "broadcaster",
                "Twitch broadcaster account",
            ),
            (
                credentials::TwitchRole::Bot,
                "bot",
                "Twitch bot account (optional)",
            ),
        ] {
            if accounts.get(role).is_none() {
                continue;
            }
            let (mut state, mut detail) = match self.authorization_health(role) {
                session::TwitchAuthorizationHealth::Unconfigured => {
                    (ConnectionState::Inactive, "Not configured".into())
                }
                session::TwitchAuthorizationHealth::Checking => (
                    ConnectionState::Connecting,
                    "Checking authorization…".into(),
                ),
                session::TwitchAuthorizationHealth::Connected => {
                    (ConnectionState::Connected, "Signed in".into())
                }
                session::TwitchAuthorizationHealth::Error(error) => (ConnectionState::Error, error),
            };
            if let Some(request) = requests
                .iter()
                .find(|request| request.connection == connection)
            {
                state = ConnectionState::Error;
                detail = format!(
                    "Reconnect required · {}\n{}",
                    request.affected_features.join(", "),
                    request.reason
                );
            }
            statuses.push(ConnectionStatus {
                integration: "twitch".into(),
                connection: connection.into(),
                title: title.into(),
                state,
                detail,
            });
        }
        let demanded = self
            .required_events
            .read()
            .expect("Twitch event demand lock poisoned")
            .as_ref()
            .is_ok_and(|required| !required.is_empty());
        let health = self.health();
        if demanded && health != TwitchHealth::Disabled {
            let (state, detail) = match health {
                TwitchHealth::Disabled => unreachable!(),
                TwitchHealth::Connecting => (ConnectionState::Connecting, "Connecting…".into()),
                TwitchHealth::Connected => (ConnectionState::Connected, "Connected".into()),
                TwitchHealth::Degraded(error)
                | TwitchHealth::Retrying(error)
                | TwitchHealth::Error(error) => (ConnectionState::Error, error),
            };
            statuses.push(ConnectionStatus {
                integration: "twitch".into(),
                connection: "events".into(),
                title: "Twitch events".into(),
                state,
                detail,
            });
        }
        statuses
    }

    fn reconfiguration_requests(&self) -> Vec<crate::integration::ReconfigurationRequest> {
        let Ok(accounts) = self.accounts.configured_accounts() else {
            return Vec::new();
        };
        let Ok(required) = self
            .required_events
            .read()
            .expect("Twitch event demand lock poisoned")
            .clone()
        else {
            return Vec::new();
        };
        configuration_request(&accounts, &required)
            .into_iter()
            .collect()
    }
}

fn configuration_request(
    accounts: &accounts::ConnectedAccounts,
    required: &std::collections::BTreeSet<events::EventSubKind>,
) -> Option<crate::integration::ReconfigurationRequest> {
    let broadcaster = accounts.get(credentials::TwitchRole::Broadcaster)?;
    if required.contains(&events::EventSubKind::ChannelAdBreakBegin)
        && !broadcaster
            .scopes
            .iter()
            .any(|scope| scope == "channel:read:ads")
    {
        Some(crate::integration::ReconfigurationRequest {
            integration: "twitch",
            connection: "broadcaster",
            reason: "Reconnect your broadcaster account to allow ad events. Chat and other authorized features remain available.".into(),
            affected_features: vec!["Ad triggers".into()],
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use credentials::TwitchRole;
    use session::TwitchAuthorizationHealth as Health;

    #[test]
    fn transient_authorization_errors_retry_after_cooldown_per_role() {
        let mut retry = AuthorizationRetry::default();
        let start = Instant::now();
        assert!(retry.should_authorize(TwitchRole::Broadcaster, &Health::Checking, start));
        retry.attempted(TwitchRole::Broadcaster, start);

        let error = Health::Error("Twitch request failed".into());
        assert!(!retry.should_authorize(
            TwitchRole::Broadcaster,
            &error,
            start + AUTHORIZATION_RETRY - Duration::from_millis(1)
        ));
        assert!(retry.should_authorize(
            TwitchRole::Broadcaster,
            &error,
            start + AUTHORIZATION_RETRY
        ));
        assert!(!retry.should_authorize(TwitchRole::Bot, &Health::Unconfigured, start));
        assert!(retry.should_authorize(TwitchRole::Bot, &Health::Checking, start));
    }

    #[test]
    fn changed_account_checks_immediately_after_a_failed_attempt() {
        let mut retry = AuthorizationRetry::default();
        let start = Instant::now();
        retry.attempted(TwitchRole::Bot, start);
        assert!(!retry.should_authorize(
            TwitchRole::Bot,
            &Health::Error("Twitch request failed".into()),
            start + Duration::from_secs(1)
        ));
        assert!(retry.should_authorize(
            TwitchRole::Bot,
            &Health::Checking,
            start + Duration::from_secs(1)
        ));
        retry.attempted(TwitchRole::Bot, start + Duration::from_secs(1));
        assert!(!retry.should_authorize(
            TwitchRole::Bot,
            &Health::Error("Twitch request failed".into()),
            start + Duration::from_secs(2)
        ));
    }
}
