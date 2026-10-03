//! VTube Studio connection, workflow actions, and event routing.

pub mod actions;
mod connection;
pub mod credentials;
pub mod protocol;
pub mod routes;
pub mod settings;

pub use routes::{VtubeActivation, VtubeRoute, routes_for, trigger_title, trigger_value_schema};

use std::collections::BTreeSet;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use thiserror::Error;
use tokio::sync::{Mutex, watch};

use crate::choices::{ChoiceCatalog, ChoiceFuture, ChoiceProvider, ConfigChoice};
use crate::engine::Engine;
use crate::integration::{
    ConfigurationStatus, ConnectionState, ConnectionStatus, ReconfigurationRequest, StatusCell,
};
use crate::paths::AppPaths;
use crate::runtime::{AppRuntime, RuntimeError};
use crate::storage::SaveOutcome;

use self::actions::{VtubeApi, VtubeError};
use self::connection::VtubeConnection;
use self::credentials::{
    CachedVtubeCredentialStore, SystemVtubeCredentialStore, VtubeCredentialError,
    VtubeCredentialStore,
};
use self::protocol::{EventKind, Model, ModelHotkeys, Protocol, ProtocolError, ProtocolEvent};
use self::routes::{VtubeEventKind, activations, required_kinds};
use self::settings::{LoadedVtubeSettings, VtubeSettings, VtubeSettingsError, VtubeSettingsStore};

const PLUGIN_NAME: &str = "SnenkBot";
const PLUGIN_DEVELOPER: &str = "Faey Umbrea";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VtubeHealth {
    Disabled,
    Disconnected,
    Connecting,
    Connected,
    NeedsAuthorization,
    Retrying(String),
    Error(String),
}

#[derive(Debug, Error)]
pub enum VtubeConfigurationError {
    #[error(transparent)]
    Settings(#[from] VtubeSettingsError),
    #[error(transparent)]
    Credential(#[from] VtubeCredentialError),
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error("VTube Studio configuration worker stopped")]
    WorkerStopped,
    #[error("VTube Studio authorization or configuration is already in progress")]
    Busy,
}

pub struct VtubeIntegration {
    connection: Arc<VtubeConnection>,
    settings: VtubeSettingsStore,
    routes: watch::Sender<Vec<VtubeRoute>>,
    configuration: Mutex<()>,
    configured: StatusCell<bool>,
}

impl VtubeIntegration {
    pub fn register(
        paths: &AppPaths,
        engine: &mut Engine,
        choices: &mut ChoiceCatalog,
    ) -> Arc<Self> {
        let settings = VtubeSettingsStore::new(paths);
        let (value, mut health, is_configured) = match settings.load() {
            Ok(loaded) => {
                let health = if loaded.value.enabled {
                    VtubeHealth::Disconnected
                } else {
                    VtubeHealth::Disabled
                };
                let is_configured = loaded.is_configured();
                (loaded.value, health, is_configured)
            }
            Err(error) => (
                VtubeSettings::default(),
                VtubeHealth::Error(format!("VTube Studio settings are unavailable: {error}")),
                false,
            ),
        };
        let backend: Arc<dyn VtubeCredentialStore> = Arc::new(SystemVtubeCredentialStore);
        let credentials: Arc<dyn VtubeCredentialStore> = Arc::new(if is_configured {
            CachedVtubeCredentialStore::new(backend)
        } else {
            CachedVtubeCredentialStore::without_preload(backend)
        });
        if let Err(error) = credentials.load() {
            tracing::warn!(%error, "VTube Studio credential unavailable at startup");
            if value.enabled && !matches!(&health, VtubeHealth::Error(_)) {
                health = VtubeHealth::Error(error.to_string());
            }
        }
        let connection = Arc::new(VtubeConnection {
            settings: RwLock::new(value),
            credentials,
            client: Mutex::new(None),
            health: StatusCell::new(health),
            changed: watch::channel(0u64).0,
        });
        actions::register(engine, Arc::clone(&connection) as Arc<dyn VtubeApi>);
        let integration = Arc::new(Self {
            connection,
            settings,
            routes: watch::channel(Vec::new()).0,
            configuration: Mutex::new(()),
            configured: StatusCell::new(is_configured),
        });
        for source in ["vtube.models", "vtube.hotkeys"] {
            choices.register(source, Arc::clone(&integration) as Arc<dyn ChoiceProvider>);
        }
        integration
    }

    /// Reports the startup/session cache without accessing the operating system store.
    pub fn authorization_present(&self) -> Result<bool, VtubeCredentialError> {
        Ok(self.connection.credentials.load()?.is_some())
    }

    pub fn settings(&self) -> &VtubeSettingsStore {
        &self.settings
    }

    pub async fn available_models(&self) -> Result<Vec<Model>, VtubeError> {
        self.connection.available_models().await
    }

    pub async fn current_model(&self) -> Result<Option<Model>, VtubeError> {
        self.connection.current_model().await
    }

    pub async fn hotkeys(&self, model_id: &str) -> Result<ModelHotkeys, VtubeError> {
        self.connection.hotkeys(model_id).await
    }

    pub fn health(&self) -> VtubeHealth {
        self.connection
            .health
            .read()
            .expect("VTube Studio health lock poisoned")
            .clone()
    }

    pub fn set_routes(&self, routes: Vec<VtubeRoute>) {
        self.routes.send_replace(routes);
    }

    pub fn start_listener(
        &self,
        runtime: &AppRuntime,
        on_activation: impl Fn(VtubeActivation) + Send + Sync + 'static,
        on_error: impl Fn(String) + Send + Sync + 'static,
    ) -> Result<(), RuntimeError> {
        let connection = Arc::clone(&self.connection);
        let mut routes = self.routes.subscribe();
        let mut changed = connection.changed.subscribe();
        runtime.spawn_task("VTube Studio", move |shutdown| async move {
            let mut retry = Duration::from_secs(1);
            loop {
                let settings = connection
                    .settings
                    .read()
                    .expect("VTube Studio settings lock poisoned")
                    .clone();
                if !settings.enabled {
                    if !matches!(
                        connection
                            .health
                            .read()
                            .expect("VTube Studio health lock poisoned")
                            .clone(),
                        VtubeHealth::Error(_)
                    ) {
                        set_health(&connection, VtubeHealth::Disabled);
                    }
                    tokio::select! {
                        _ = shutdown.cancelled() => break,
                        update = changed.changed() => if update.is_err() { break; },
                    }
                    continue;
                }

                let credentials = Arc::clone(&connection.credentials);
                let token = tokio::task::spawn_blocking(move || credentials.load()).await;
                let token = match token {
                    Ok(Ok(Some(token))) => token,
                    Ok(Ok(None)) => {
                        set_health(&connection, VtubeHealth::NeedsAuthorization);
                        tokio::select! {
                            _ = shutdown.cancelled() => break,
                            update = changed.changed() => if update.is_err() { break; },
                        }
                        continue;
                    }
                    Ok(Err(error)) => {
                        set_health(&connection, VtubeHealth::Error(error.to_string()));
                        on_error(error.to_string());
                        tokio::select! {
                            _ = shutdown.cancelled() => break,
                            update = changed.changed() => if update.is_err() { break; },
                        }
                        continue;
                    }
                    Err(_) => {
                        set_health(&connection, VtubeHealth::Error("VTube Studio credential worker stopped".into()));
                        on_error("VTube Studio credential worker stopped".into());
                        break;
                    }
                };

                set_health(&connection, VtubeHealth::Connecting);
                let (mut protocol, mut events) = Protocol::new(
                    &settings.endpoint(),
                    PLUGIN_NAME,
                    PLUGIN_DEVELOPER,
                );
                let authenticated = tokio::select! {
                    _ = shutdown.cancelled() => break,
                    update = changed.changed() => {
                        if update.is_err() { break; }
                        continue;
                    },
                    result = tokio::time::timeout(REQUEST_TIMEOUT, protocol.authenticate(&token)) => result,
                };
                match authenticated {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) if matches!(&error, ProtocolError::AuthenticationRejected(_))
                        || matches!(&error, ProtocolError::Api(api) if api.is_unauthenticated_error()) => {
                        set_health(&connection, VtubeHealth::NeedsAuthorization);
                        on_error("VTube Studio authorization is no longer valid".into());
                        tokio::select! {
                            _ = shutdown.cancelled() => break,
                            update = changed.changed() => if update.is_err() { break; },
                        }
                        continue;
                    }
                    result => {
                        let message = match result {
                            Ok(Err(error)) => format!("VTube Studio connection failed: {error}"),
                            Err(_) => "VTube Studio connection timed out".into(),
                            Ok(Ok(())) => unreachable!(),
                        };
                        set_health(&connection, VtubeHealth::Retrying(message.clone()));
                        on_error(message);
                        tokio::select! {
                            _ = shutdown.cancelled() => break,
                            update = changed.changed() => { if update.is_err() { break; } retry = Duration::from_secs(1); },
                            _ = tokio::time::sleep(retry) => { retry = (retry * 2).min(Duration::from_secs(30)); },
                        }
                        continue;
                    }
                }

                let client = Arc::new(Mutex::new(protocol));
                let mut subscriptions = BTreeSet::new();
                let desired = required_kinds(&routes.borrow());
                if let Err(error) = reconcile_subscriptions(&client, &mut subscriptions, &desired).await {
                    let message = format!("VTube Studio event subscription failed: {error}");
                    set_health(&connection, VtubeHealth::Retrying(message.clone()));
                    on_error(message);
                    tokio::select! {
                        _ = shutdown.cancelled() => break,
                        update = changed.changed() => { if update.is_err() { break; } retry = Duration::from_secs(1); },
                        _ = tokio::time::sleep(retry) => { retry = (retry * 2).min(Duration::from_secs(30)); },
                    }
                    continue;
                }
                *connection.client.lock().await = Some(Arc::clone(&client));
                set_health(&connection, VtubeHealth::Connected);
                retry = Duration::from_secs(1);

                let mut saw_connected = false;
                let lost = loop {
                    tokio::select! {
                        _ = shutdown.cancelled() => break None,
                        update = changed.changed() => {
                            if update.is_err() { break None; }
                            break None;
                        },
                        update = routes.changed() => {
                            if update.is_err() { break None; }
                            let desired = required_kinds(&routes.borrow());
                            if let Err(error) = reconcile_subscriptions(&client, &mut subscriptions, &desired).await {
                                break Some(format!("VTube Studio event subscription failed: {error}"));
                            }
                        },
                        event = events.next() => match event {
                            Some(ProtocolEvent::Connected) => saw_connected = true,
                            Some(ProtocolEvent::Disconnected) if !saw_connected => {},
                            Some(ProtocolEvent::Disconnected) | None => break Some("VTube Studio disconnected".into()),
                            Some(ProtocolEvent::Error(error)) => break Some(format!("VTube Studio event stream failed: {error}")),
                            Some(ProtocolEvent::Api(event)) => {
                                for activation in activations(&routes.borrow(), &event) {
                                    on_activation(activation);
                                }
                            }
                        },
                    }
                };
                connection.clear_if_current(&client).await;
                changed.borrow_and_update();
                if shutdown.is_cancelled() {
                    break;
                }
                if let Some(message) = lost {
                    set_health(&connection, VtubeHealth::Retrying(message.clone()));
                    on_error(message);
                    tokio::select! {
                        _ = shutdown.cancelled() => break,
                        update = changed.changed() => { if update.is_err() { break; } retry = Duration::from_secs(1); },
                        _ = tokio::time::sleep(retry) => { retry = (retry * 2).min(Duration::from_secs(30)); },
                    }
                }
            }
            connection.client.lock().await.take();
            Ok::<(), std::convert::Infallible>(())
        })
    }

    pub async fn save_settings(
        &self,
        loaded: LoadedVtubeSettings,
        value: VtubeSettings,
    ) -> Result<SaveOutcome, VtubeConfigurationError> {
        let _configuration = self
            .configuration
            .try_lock()
            .map_err(|_| VtubeConfigurationError::Busy)?;
        let store = self.settings.clone();
        let saved = value.clone();
        let result = tokio::task::spawn_blocking(move || store.save(&loaded, saved))
            .await
            .map_err(|_| VtubeConfigurationError::WorkerStopped)??;
        *self
            .connection
            .settings
            .write()
            .expect("VTube Studio settings lock poisoned") = value.clone();
        self.connection.client.lock().await.take();
        set_health(
            &self.connection,
            if value.enabled {
                VtubeHealth::Disconnected
            } else {
                VtubeHealth::Disabled
            },
        );
        self.connection
            .changed
            .send_modify(|revision| *revision = revision.wrapping_add(1));
        self.configured
            .set(true)
            .expect("VTube Studio configuration status lock poisoned");
        Ok(result)
    }

    /// Called only by the user-initiated authorization UI action.
    pub async fn authorize(&self) -> Result<(), VtubeConfigurationError> {
        let _configuration = self
            .configuration
            .try_lock()
            .map_err(|_| VtubeConfigurationError::Busy)?;
        let settings = self
            .connection
            .settings
            .read()
            .expect("VTube Studio settings lock poisoned")
            .clone();
        let (mut protocol, _events) =
            Protocol::new(&settings.endpoint(), PLUGIN_NAME, PLUGIN_DEVELOPER);
        let token = protocol.request_token().await?;
        protocol.authenticate(&token).await?;
        let credentials = Arc::clone(&self.connection.credentials);
        tokio::task::spawn_blocking(move || credentials.save(&token))
            .await
            .map_err(|_| VtubeConfigurationError::WorkerStopped)??;
        self.connection.client.lock().await.take();
        set_health(
            &self.connection,
            if settings.enabled {
                VtubeHealth::Disconnected
            } else {
                VtubeHealth::Disabled
            },
        );
        self.connection
            .changed
            .send_modify(|revision| *revision = revision.wrapping_add(1));
        Ok(())
    }

    /// Removes the saved token and ends the current session.
    pub async fn forget_authorization(&self) -> Result<(), VtubeConfigurationError> {
        let _configuration = self
            .configuration
            .try_lock()
            .map_err(|_| VtubeConfigurationError::Busy)?;
        let credentials = Arc::clone(&self.connection.credentials);
        tokio::task::spawn_blocking(move || credentials.clear())
            .await
            .map_err(|_| VtubeConfigurationError::WorkerStopped)??;
        self.connection.client.lock().await.take();
        let enabled = self
            .connection
            .settings
            .read()
            .expect("VTube Studio settings lock poisoned")
            .enabled;
        set_health(
            &self.connection,
            if enabled {
                VtubeHealth::NeedsAuthorization
            } else {
                VtubeHealth::Disabled
            },
        );
        self.connection
            .changed
            .send_modify(|revision| *revision = revision.wrapping_add(1));
        Ok(())
    }
}

impl ConfigurationStatus for VtubeIntegration {
    fn reconfiguration_requests(&self) -> Vec<ReconfigurationRequest> {
        Vec::new()
    }

    fn subscribe_configuration_changes(&self) -> Vec<watch::Receiver<u64>> {
        vec![
            self.connection.health.subscribe(),
            self.configured.subscribe(),
            self.connection.changed.subscribe(),
        ]
    }

    fn connection_statuses(&self) -> Vec<ConnectionStatus> {
        if !*self
            .configured
            .read()
            .expect("VTube Studio configuration status lock poisoned")
        {
            return Vec::new();
        }
        let (state, detail) = match self.health() {
            VtubeHealth::Disabled => (ConnectionState::Inactive, "Disabled".into()),
            VtubeHealth::Disconnected => (
                ConnectionState::Connecting,
                "Disconnected — waiting to connect".into(),
            ),
            VtubeHealth::Connecting => (ConnectionState::Connecting, "Connecting…".into()),
            VtubeHealth::Connected => (ConnectionState::Connected, "Connected".into()),
            VtubeHealth::NeedsAuthorization => (
                ConnectionState::Error,
                "Authorization required in VTube Studio".into(),
            ),
            VtubeHealth::Retrying(error) | VtubeHealth::Error(error) => {
                (ConnectionState::Error, error)
            }
        };
        vec![ConnectionStatus {
            integration: "vtube_studio".into(),
            connection: "connection".into(),
            title: "VTube Studio".into(),
            state,
            detail,
        }]
    }
}

impl ChoiceProvider for VtubeIntegration {
    fn choices<'a>(&'a self, source: &'a str, depends_on: Option<&'a str>) -> ChoiceFuture<'a> {
        Box::pin(async move {
            let mut choices: Vec<ConfigChoice> = match source {
                "vtube.models" => {
                    if depends_on.is_some() {
                        return Err("model choices do not take a dependency".into());
                    }
                    self.available_models()
                        .await
                        .map_err(|error| error.to_string())?
                        .into_iter()
                        .map(|model| ConfigChoice {
                            value: model.id,
                            label: model.name,
                            detail: model.loaded.then(|| "Loaded".into()),
                        })
                        .collect()
                }
                "vtube.hotkeys" => {
                    let model_id = depends_on
                        .filter(|value| !value.trim().is_empty())
                        .ok_or("select a model before choosing a hotkey")?;
                    self.hotkeys(model_id)
                        .await
                        .map_err(|error| error.to_string())?
                        .hotkeys
                        .into_iter()
                        .map(|hotkey| ConfigChoice {
                            value: hotkey.id,
                            label: hotkey.name,
                            detail: Some(hotkey.action),
                        })
                        .collect()
                }
                _ => return Err(format!("choice source `{source}` is unavailable")),
            };
            choices.sort_by(|left, right| {
                left.label
                    .to_lowercase()
                    .cmp(&right.label.to_lowercase())
                    .then_with(|| left.value.cmp(&right.value))
            });
            Ok(choices)
        })
    }
}

fn set_health(connection: &VtubeConnection, health: VtubeHealth) {
    connection
        .health
        .set(health)
        .expect("VTube Studio health lock poisoned");
}

fn protocol_kind(kind: VtubeEventKind) -> EventKind {
    match kind {
        VtubeEventKind::ModelLoaded => EventKind::ModelLoaded,
        VtubeEventKind::HotkeyTriggered => EventKind::HotkeyTriggered,
    }
}

async fn reconcile_subscriptions(
    client: &Arc<Mutex<Protocol>>,
    current: &mut BTreeSet<VtubeEventKind>,
    desired: &BTreeSet<VtubeEventKind>,
) -> Result<(), ProtocolError> {
    let mut protocol = client.lock().await;
    for kind in current.difference(desired) {
        protocol.unsubscribe(protocol_kind(*kind)).await?;
    }
    for kind in desired.difference(current) {
        protocol.subscribe(protocol_kind(*kind)).await?;
    }
    *current = desired.clone();
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use futures_util::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;
    use tokio::time::timeout;
    use tokio_websockets::{Message, ServerBuilder};

    use super::*;
    use crate::vtube::credentials::VtubeCredentialStore;
    use crate::vtube::routes::VtubeRouteKind;

    struct MemoryCredentials {
        token: StdMutex<Option<String>>,
    }

    impl MemoryCredentials {
        fn new(token: Option<&str>) -> Self {
            Self {
                token: StdMutex::new(token.map(str::to_owned)),
            }
        }
    }

    impl VtubeCredentialStore for MemoryCredentials {
        fn load(&self) -> Result<Option<String>, VtubeCredentialError> {
            Ok(self.token.lock().unwrap().clone())
        }

        fn save(&self, token: &str) -> Result<(), VtubeCredentialError> {
            *self.token.lock().unwrap() = Some(token.to_owned());
            Ok(())
        }

        fn clear(&self) -> Result<(), VtubeCredentialError> {
            self.token.lock().unwrap().take();
            Ok(())
        }
    }

    struct FakeServer {
        port: u16,
        requests: mpsc::UnboundedReceiver<(usize, Value)>,
        disconnect: mpsc::UnboundedSender<()>,
        task: tokio::task::JoinHandle<()>,
    }

    impl FakeServer {
        async fn start(reject_auth: bool) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let (requests_tx, requests) = mpsc::unbounded_channel();
            let (disconnect, mut disconnect_rx) = mpsc::unbounded_channel();
            let task = tokio::spawn(async move {
                let mut connection = 0;
                while let Ok((socket, _)) = listener.accept().await {
                    connection += 1;
                    let (_, mut socket) = ServerBuilder::new().accept(socket).await.unwrap();
                    let mut subscribed = BTreeSet::<String>::new();
                    loop {
                        tokio::select! {
                            command = disconnect_rx.recv() => {
                                if command.is_some() { break; }
                            }
                            message = socket.next() => {
                                let Some(Ok(message)) = message else { break; };
                                let Some(text) = message.as_text() else { continue; };
                                let request: Value = serde_json::from_str(text).unwrap();
                                let kind = request["messageType"].as_str().unwrap();
                                let (response_type, data) = match kind {
                                    "AuthenticationRequest" => (
                                        "AuthenticationResponse",
                                        json!({"authenticated": !reject_auth, "reason": if reject_auth { "rejected" } else { "" }}),
                                    ),
                                    "AuthenticationTokenRequest" => (
                                        "AuthenticationTokenResponse",
                                        json!({"authenticationToken": "approved-token"}),
                                    ),
                                    "EventSubscriptionRequest" => {
                                        let event = request["data"]["eventName"].as_str().unwrap().to_owned();
                                        if request["data"]["subscribe"] == true {
                                            subscribed.insert(event);
                                        } else {
                                            subscribed.remove(&event);
                                        }
                                        (
                                            "EventSubscriptionResponse",
                                            json!({"subscribedEventCount": subscribed.len(), "subscribedEvents": subscribed}),
                                        )
                                    }
                                    other => panic!("unexpected VTube Studio request: {other}"),
                                };
                                let response = json!({
                                    "apiName": "VTubeStudioPublicAPI",
                                    "apiVersion": "1.0",
                                    "timestamp": 1,
                                    "requestID": request["requestID"],
                                    "messageType": response_type,
                                    "data": data,
                                });
                                requests_tx.send((connection, request)).unwrap();
                                socket.send(Message::text(response.to_string())).await.unwrap();
                            }
                        }
                    }
                }
            });
            Self {
                port,
                requests,
                disconnect,
                task,
            }
        }

        async fn next_request(&mut self) -> (usize, Value) {
            timeout(Duration::from_secs(5), self.requests.recv())
                .await
                .expect("VTube Studio request timed out")
                .expect("fake server stopped")
        }

        async fn no_request(&mut self) {
            assert!(
                timeout(Duration::from_millis(150), self.requests.recv())
                    .await
                    .is_err()
            );
        }
    }

    impl Drop for FakeServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    fn integration(
        port: u16,
        credentials: Arc<MemoryCredentials>,
        root: &tempfile::TempDir,
    ) -> VtubeIntegration {
        let paths = AppPaths {
            config: root.path().join("config"),
            data: root.path().join("data"),
            state: root.path().join("state"),
        };
        VtubeIntegration {
            connection: Arc::new(VtubeConnection {
                settings: RwLock::new(VtubeSettings {
                    enabled: true,
                    host: "127.0.0.1".into(),
                    port,
                }),
                credentials,
                client: Mutex::new(None),
                health: StatusCell::new(VtubeHealth::Disconnected),
                changed: watch::channel(0).0,
            }),
            settings: VtubeSettingsStore::new(&paths),
            routes: watch::channel(Vec::new()).0,
            configuration: Mutex::new(()),
            configured: StatusCell::new(true),
        }
    }

    fn route(kind: VtubeRouteKind) -> VtubeRoute {
        VtubeRoute {
            workflow_id: "workflow".into(),
            trigger_id: "trigger".into(),
            kind,
        }
    }

    #[tokio::test]
    async fn configuration_and_action_health_notify_without_connecting() {
        let root = tempfile::tempdir().unwrap();
        let integration = integration(8001, Arc::new(MemoryCredentials::new(None)), &root);
        integration.configured.set(false).unwrap();
        let mut changes = integration.subscribe_configuration_changes();
        assert!(integration.connection_statuses().is_empty());
        integration
            .save_settings(
                integration.settings.load().unwrap(),
                VtubeSettings::default(),
            )
            .await
            .unwrap();
        assert_eq!(
            integration.connection_statuses()[0].state,
            ConnectionState::Inactive
        );
        assert!(changes.iter().any(|change| change.has_changed().unwrap()));
        for change in &mut changes {
            change.borrow_and_update();
        }
        set_health(&integration.connection, VtubeHealth::NeedsAuthorization);
        assert!(changes[0].has_changed().unwrap());
        assert_eq!(
            integration.connection_statuses()[0].detail,
            "Authorization required in VTube Studio"
        );
        changes[0].borrow_and_update();
        set_health(&integration.connection, VtubeHealth::NeedsAuthorization);
        assert!(!changes[0].has_changed().unwrap());
        integration.forget_authorization().await.unwrap();
        assert!(changes.iter().any(|change| change.has_changed().unwrap()));
        assert_eq!(
            integration.connection_statuses()[0].state,
            ConnectionState::Inactive
        );
    }

    #[tokio::test]
    async fn rejected_settings_do_not_publish_status_changes() {
        let root = tempfile::tempdir().unwrap();
        let integration = integration(8001, Arc::new(MemoryCredentials::new(None)), &root);
        let changes = integration.subscribe_configuration_changes();
        let invalid = VtubeSettings {
            port: 0,
            ..VtubeSettings::default()
        };
        assert!(
            integration
                .save_settings(integration.settings.load().unwrap(), invalid)
                .await
                .is_err()
        );
        assert!(changes.iter().all(|change| !change.has_changed().unwrap()));
    }

    #[tokio::test]
    async fn saved_token_reconnects_and_subscribes_only_to_demanded_events() {
        let mut server = FakeServer::start(false).await;
        let credentials = Arc::new(MemoryCredentials::new(Some("saved-token")));
        let root = tempfile::tempdir().unwrap();
        let integration = integration(server.port, credentials, &root);
        let runtime = AppRuntime::new(|_| {}).unwrap();
        integration
            .start_listener(&runtime, |_| {}, |_| {})
            .unwrap();

        let (first, auth) = server.next_request().await;
        assert_eq!(first, 1);
        assert_eq!(auth["messageType"], "AuthenticationRequest");
        assert_eq!(auth["data"]["authenticationToken"], "saved-token");
        server.no_request().await;

        integration.set_routes(vec![route(VtubeRouteKind::Model {
            loaded: true,
            model_id: None,
        })]);
        let (_, model_subscription) = server.next_request().await;
        assert_eq!(model_subscription["data"]["eventName"], "ModelLoadedEvent");
        assert_eq!(model_subscription["data"]["subscribe"], true);

        integration.set_routes(vec![
            route(VtubeRouteKind::Model {
                loaded: true,
                model_id: None,
            }),
            route(VtubeRouteKind::Hotkey {
                model_id: None,
                hotkey_id: None,
                ignore_api: false,
            }),
        ]);
        let (_, hotkey_subscription) = server.next_request().await;
        assert_eq!(
            hotkey_subscription["data"]["eventName"],
            "HotkeyTriggeredEvent"
        );
        assert_eq!(hotkey_subscription["data"]["subscribe"], true);

        integration.set_routes(vec![route(VtubeRouteKind::Hotkey {
            model_id: None,
            hotkey_id: None,
            ignore_api: false,
        })]);
        let (_, model_unsubscribe) = server.next_request().await;
        assert_eq!(model_unsubscribe["data"]["eventName"], "ModelLoadedEvent");
        assert_eq!(model_unsubscribe["data"]["subscribe"], false);

        server.disconnect.send(()).unwrap();
        let (second, reauth) = server.next_request().await;
        assert_eq!(second, 2);
        assert_eq!(reauth["messageType"], "AuthenticationRequest");
        assert_eq!(reauth["data"]["authenticationToken"], "saved-token");
        let (second, resubscribe) = server.next_request().await;
        assert_eq!(second, 2);
        assert_eq!(resubscribe["data"]["eventName"], "HotkeyTriggeredEvent");
        server.no_request().await;
        tokio::task::spawn_blocking(move || drop(runtime))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn rejected_saved_token_waits_without_requesting_a_new_token() {
        let mut server = FakeServer::start(true).await;
        let credentials = Arc::new(MemoryCredentials::new(Some("rejected-token")));
        let root = tempfile::tempdir().unwrap();
        let integration = integration(server.port, credentials, &root);
        let runtime = AppRuntime::new(|_| {}).unwrap();
        integration
            .start_listener(&runtime, |_| {}, |_| {})
            .unwrap();
        let (_, auth) = server.next_request().await;
        assert_eq!(auth["messageType"], "AuthenticationRequest");
        assert_eq!(auth["data"]["authenticationToken"], "rejected-token");
        timeout(Duration::from_secs(2), async {
            while integration.health() != VtubeHealth::NeedsAuthorization {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        server.no_request().await;
        tokio::task::spawn_blocking(move || drop(runtime))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn token_request_occurs_only_during_explicit_authorize() {
        let mut server = FakeServer::start(false).await;
        let credentials = Arc::new(MemoryCredentials::new(None));
        let root = tempfile::tempdir().unwrap();
        let integration = integration(server.port, Arc::clone(&credentials), &root);
        assert!(!integration.authorization_present().unwrap());
        let runtime = AppRuntime::new(|_| {}).unwrap();
        integration
            .start_listener(&runtime, |_| {}, |_| {})
            .unwrap();
        timeout(Duration::from_secs(2), async {
            while integration.health() != VtubeHealth::NeedsAuthorization {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        server.no_request().await;

        let ((_, token_request), outcome) =
            tokio::join!(server.next_request(), integration.authorize());
        assert_eq!(token_request["messageType"], "AuthenticationTokenRequest");
        outcome.unwrap();
        let (_, authentication) = server.next_request().await;
        assert_eq!(authentication["messageType"], "AuthenticationRequest");
        assert_eq!(
            authentication["data"]["authenticationToken"],
            "approved-token"
        );
        assert_eq!(
            credentials.load().unwrap().as_deref(),
            Some("approved-token")
        );
        assert!(integration.authorization_present().unwrap());
        tokio::task::spawn_blocking(move || drop(runtime))
            .await
            .unwrap();
        integration.forget_authorization().await.unwrap();
        assert!(!integration.authorization_present().unwrap());
    }

    #[tokio::test]
    async fn disabled_listener_preserves_settings_load_error() {
        let server = FakeServer::start(false).await;
        let root = tempfile::tempdir().unwrap();
        let integration = integration(server.port, Arc::new(MemoryCredentials::new(None)), &root);
        integration.connection.settings.write().unwrap().enabled = false;
        integration
            .connection
            .health
            .set(VtubeHealth::Error(
                "VTube Studio settings are unavailable".into(),
            ))
            .unwrap();
        let runtime = AppRuntime::new(|_| {}).unwrap();
        integration
            .start_listener(&runtime, |_| {}, |_| {})
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(matches!(integration.health(), VtubeHealth::Error(_)));
        assert!(
            integration
                .connection
                .ready()
                .unwrap_err()
                .contains("settings are unavailable")
        );
        tokio::task::spawn_blocking(move || drop(runtime))
            .await
            .unwrap();
    }
}
