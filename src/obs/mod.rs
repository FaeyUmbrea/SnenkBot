//! OBS WebSocket integration. Transport and credentials stay inside this module.

pub mod actions;
mod connection;
pub mod credentials;
mod event_stream;
pub(crate) mod events;
pub mod settings;

pub use events::{
    ObsActivation, ObsRoute, ObsRouteKind, routes_for, trigger_title, trigger_value_schema,
};
use events::{activations, subscriptions};

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

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

use self::actions::{ObsApi, ObsError, ObsResourceSnapshot};
use self::connection::ObsConnection;
use self::credentials::{
    CachedObsCredentialStore, ObsCredentialError, ObsCredentialStore, SystemObsCredentialStore,
};
use self::event_stream::{EventStreamError, ObsEventStream};
use self::settings::{LoadedObsSettings, ObsSettings, ObsSettingsError, ObsSettingsStore};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ObsHealth {
    Disabled,
    Disconnected,
    Connecting,
    Connected,
    Retrying(String),
    Error(String),
}

pub struct ObsIntegration {
    connection: Arc<ObsConnection>,
    event_health: Arc<StatusCell<ObsHealth>>,
    settings: ObsSettingsStore,
    routes: watch::Sender<Vec<ObsRoute>>,
    configured: StatusCell<bool>,
}

#[derive(Debug, Error)]
pub enum ObsConfigurationError {
    #[error(transparent)]
    Settings(#[from] ObsSettingsError),
    #[error(transparent)]
    Credential(#[from] ObsCredentialError),
    #[error("OBS configuration worker stopped")]
    WorkerStopped,
}

impl ObsIntegration {
    pub fn register(
        paths: &AppPaths,
        engine: &mut Engine,
        choices: &mut ChoiceCatalog,
    ) -> Arc<Self> {
        let settings = ObsSettingsStore::new(paths);
        let (value, mut health, is_configured) = match settings.load() {
            Ok(loaded) => {
                let health = if loaded.value.enabled {
                    ObsHealth::Disconnected
                } else {
                    ObsHealth::Disabled
                };
                let is_configured = loaded.is_configured();
                (loaded.value, health, is_configured)
            }
            Err(error) => (
                ObsSettings::default(),
                ObsHealth::Error(format!("OBS settings are unavailable: {error}")),
                false,
            ),
        };
        let backend: Arc<dyn ObsCredentialStore> = Arc::new(SystemObsCredentialStore);
        let credentials: Arc<dyn ObsCredentialStore> = Arc::new(if is_configured {
            CachedObsCredentialStore::new(backend)
        } else {
            CachedObsCredentialStore::without_preload(backend)
        });
        if let Err(error) = credentials.load() {
            tracing::warn!(%error, "OBS credential unavailable at startup");
            if value.enabled && !matches!(&health, ObsHealth::Error(_)) {
                health = ObsHealth::Error(error.to_string());
            }
        }
        let connection = Arc::new(ObsConnection {
            settings: RwLock::new(value),
            credentials,
            client: Mutex::new(None),
            health: StatusCell::new(health),
            changed: watch::channel(0u64).0,
        });
        let (routes, _) = watch::channel(Vec::new());
        actions::register(engine, Arc::clone(&connection) as Arc<dyn ObsApi>);
        let integration = Arc::new(Self {
            connection,
            event_health: Arc::new(StatusCell::new(ObsHealth::Disabled)),
            settings,
            routes,
            configured: StatusCell::new(is_configured),
        });
        for source in ["obs.scenes", "obs.inputs", "obs.scene_items"] {
            choices.register(source, Arc::clone(&integration) as Arc<dyn ChoiceProvider>);
        }
        integration
    }

    pub fn settings(&self) -> &ObsSettingsStore {
        &self.settings
    }

    pub fn password_present(&self) -> Result<bool, ObsCredentialError> {
        Ok(self.connection.credentials.load()?.is_some())
    }

    pub fn health(&self) -> ObsHealth {
        let action = self
            .connection
            .health
            .read()
            .expect("OBS health lock poisoned")
            .clone();
        let event = self
            .event_health
            .read()
            .expect("OBS event health lock poisoned");
        match &*event {
            ObsHealth::Error(_) | ObsHealth::Retrying(_) => event.clone(),
            _ => action,
        }
    }

    pub fn action_health(&self) -> ObsHealth {
        self.connection
            .health
            .read()
            .expect("OBS health lock poisoned")
            .clone()
    }

    pub fn event_health(&self) -> ObsHealth {
        self.event_health
            .read()
            .expect("OBS event health lock poisoned")
            .clone()
    }

    pub async fn resource_snapshot(&self) -> Result<ObsResourceSnapshot, ObsError> {
        self.connection.resource_snapshot().await
    }

    pub fn start(&self, runtime: &AppRuntime) -> Result<(), RuntimeError> {
        let connection = Arc::clone(&self.connection);
        let mut changed = connection.changed.subscribe();
        runtime.spawn_task("OBS connection", move |shutdown| async move {
            let mut retry = Duration::from_secs(1);
            loop {
                if !connection
                    .settings
                    .read()
                    .expect("OBS settings lock poisoned")
                    .enabled
                {
                    tokio::select! {
                        _ = shutdown.cancelled() => break,
                        _ = changed.changed() => continue,
                    }
                }
                let result = tokio::select! {
                    _ = shutdown.cancelled() => break,
                    result = connection.probe() => result,
                };
                let wait = match result {
                    Ok(()) => {
                        retry = Duration::from_secs(1);
                        Duration::from_secs(10)
                    }
                    Err(error) => {
                        connection
                            .health
                            .set(ObsHealth::Retrying(error.to_string()))
                            .expect("OBS health lock poisoned");
                        let wait = retry;
                        retry = (retry * 2).min(Duration::from_secs(30));
                        wait
                    }
                };
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    _ = changed.changed() => retry = Duration::from_secs(1),
                    _ = tokio::time::sleep(wait) => {},
                }
            }
            connection.client.lock().await.take();
            Ok::<(), std::convert::Infallible>(())
        })
    }

    pub fn set_routes(&self, routes: Vec<ObsRoute>) {
        self.routes.send_replace(routes);
    }

    pub fn start_listener(
        &self,
        runtime: &AppRuntime,
        on_activation: impl Fn(ObsActivation) + Send + Sync + 'static,
    ) -> Result<(), RuntimeError> {
        let connection = Arc::clone(&self.connection);
        let event_health = Arc::clone(&self.event_health);
        let mut routes = self.routes.subscribe();
        let mut changed = connection.changed.subscribe();
        runtime.spawn_task("OBS events", move |shutdown| async move {
            let mut retry = Duration::from_secs(1);
            let mut invalidated = false;
            loop {
                if invalidated {
                    tokio::select! {
                        _ = shutdown.cancelled() => break,
                        _ = changed.changed() => invalidated = false,
                        updated = routes.changed() => if updated.is_err() { break; },
                    }
                    continue;
                }
                let active = routes.borrow().clone();
                let settings = connection
                    .settings
                    .read()
                    .expect("OBS settings lock poisoned")
                    .clone();
                if active.is_empty() || !settings.enabled {
                    event_health
                        .set(ObsHealth::Disabled)
                        .expect("OBS event health lock poisoned");
                    tokio::select! {
                        _ = shutdown.cancelled() => break,
                        _ = changed.changed() => continue,
                        changed = routes.changed() => if changed.is_err() { break; },
                    }
                    continue;
                }
                let subscription = subscriptions(&active);
                event_health
                    .set(ObsHealth::Connecting)
                    .expect("OBS event health lock poisoned");
                let credentials = Arc::clone(&connection.credentials);
                let connected = tokio::time::timeout(
                    Duration::from_secs(10),
                    ObsEventStream::connect(&settings, credentials, subscription),
                );
                let mut stream = match tokio::select! {
                    _ = shutdown.cancelled() => break,
                    _ = changed.changed() => continue,
                    updated = routes.changed() => {
                        if updated.is_err() { break; }
                        continue;
                    },
                    result = connected => result,
                } {
                    Ok(Ok(stream)) => {
                        event_health
                            .set(ObsHealth::Connected)
                            .expect("OBS event health lock poisoned");
                        stream
                    }
                    result => {
                        let error = match result {
                            Ok(Err(error)) => error,
                            Err(_) => EventStreamError::Disconnected,
                            Ok(Ok(_)) => unreachable!(),
                        };
                        if error.requires_configuration() {
                            event_health
                                .set(ObsHealth::Error(error.to_string()))
                                .expect("OBS event health lock poisoned");
                            invalidated = true;
                            retry = Duration::from_secs(1);
                            continue;
                        }
                        event_health
                            .set(ObsHealth::Retrying(error.to_string()))
                            .expect("OBS event health lock poisoned");
                        tokio::select! {
                            _ = shutdown.cancelled() => break,
                            _ = changed.changed() => {},
                            changed = routes.changed() => if changed.is_err() { break; },
                            _ = tokio::time::sleep(retry) => {},
                        }
                        retry = (retry * 2).min(Duration::from_secs(30));
                        continue;
                    }
                };
                let connected_at = Instant::now();
                let mut lost_connection = None;
                loop {
                    tokio::select! {
                        _ = shutdown.cancelled() => return Ok::<(), std::convert::Infallible>(()),
                        _ = changed.changed() => break,
                        changed = routes.changed() => {
                            if changed.is_err() { return Ok(()); }
                            break;
                        },
                        event = stream.next_event() => match event {
                            Ok(event) => {
                                for activation in activations(&active, &event) {
                                    on_activation(activation);
                                }
                            }
                            Err(error) => {
                                lost_connection = Some(error);
                                break;
                            }
                        },
                    }
                }
                if let Some(error) = lost_connection {
                    if error.requires_configuration() {
                        event_health
                            .set(ObsHealth::Error(error.to_string()))
                            .expect("OBS event health lock poisoned");
                        invalidated = true;
                        retry = Duration::from_secs(1);
                        continue;
                    }
                    event_health
                        .set(ObsHealth::Retrying(error.to_string()))
                        .expect("OBS event health lock poisoned");
                    if connected_at.elapsed() >= Duration::from_secs(30) {
                        retry = Duration::from_secs(1);
                    }
                    tokio::select! {
                        _ = shutdown.cancelled() => break,
                        _ = changed.changed() => {},
                        changed = routes.changed() => if changed.is_err() { break; },
                        _ = tokio::time::sleep(retry) => {},
                    }
                    retry = (retry * 2).min(Duration::from_secs(30));
                } else {
                    retry = Duration::from_secs(1);
                }
            }
            Ok::<(), std::convert::Infallible>(())
        })
    }

    pub async fn save_settings(
        &self,
        loaded: LoadedObsSettings,
        value: ObsSettings,
    ) -> Result<SaveOutcome, ObsConfigurationError> {
        let store = self.settings.clone();
        let saved = value.clone();
        let outcome = tokio::task::spawn_blocking(move || store.save(&loaded, saved))
            .await
            .map_err(|_| ObsConfigurationError::WorkerStopped)??;
        self.apply_settings(value).await;
        self.configured
            .set(true)
            .expect("OBS configuration status lock poisoned");
        Ok(outcome)
    }

    pub async fn save_password(
        &self,
        password: Option<String>,
    ) -> Result<(), ObsConfigurationError> {
        let credentials = Arc::clone(&self.connection.credentials);
        tokio::task::spawn_blocking(move || match password {
            Some(password) => credentials.save(&password),
            None => credentials.clear(),
        })
        .await
        .map_err(|_| ObsConfigurationError::WorkerStopped)??;
        self.connection.client.lock().await.take();
        self.connection
            .health
            .set(
                if self
                    .connection
                    .settings
                    .read()
                    .expect("OBS settings lock poisoned")
                    .enabled
                {
                    ObsHealth::Disconnected
                } else {
                    ObsHealth::Disabled
                },
            )
            .expect("OBS health lock poisoned");
        self.connection
            .changed
            .send_modify(|revision| *revision = revision.wrapping_add(1));
        Ok(())
    }

    async fn apply_settings(&self, value: ObsSettings) {
        *self
            .connection
            .settings
            .write()
            .expect("OBS settings lock poisoned") = value.clone();
        self.connection.client.lock().await.take();
        self.connection
            .health
            .set(if value.enabled {
                ObsHealth::Disconnected
            } else {
                ObsHealth::Disabled
            })
            .expect("OBS health lock poisoned");
        self.connection
            .changed
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }
}

impl ConfigurationStatus for ObsIntegration {
    fn reconfiguration_requests(&self) -> Vec<ReconfigurationRequest> {
        Vec::new()
    }

    fn subscribe_configuration_changes(&self) -> Vec<watch::Receiver<u64>> {
        vec![
            self.connection.health.subscribe(),
            self.event_health.subscribe(),
            self.configured.subscribe(),
            self.connection.changed.subscribe(),
        ]
    }

    fn connection_statuses(&self) -> Vec<ConnectionStatus> {
        if !*self
            .configured
            .read()
            .expect("OBS configuration status lock poisoned")
        {
            return Vec::new();
        }
        let (state, detail) = match self.health() {
            ObsHealth::Disabled => (ConnectionState::Inactive, "Disabled".into()),
            ObsHealth::Disconnected => (
                ConnectionState::Connecting,
                "Disconnected — waiting to connect".into(),
            ),
            ObsHealth::Connecting => (ConnectionState::Connecting, "Connecting…".into()),
            ObsHealth::Connected => (ConnectionState::Connected, "Connected".into()),
            ObsHealth::Retrying(error) => (ConnectionState::Error, format!("Retrying: {error}")),
            ObsHealth::Error(error) => (ConnectionState::Error, error),
        };
        vec![ConnectionStatus {
            integration: "obs".into(),
            connection: "connection".into(),
            title: "OBS Studio".into(),
            state,
            detail,
        }]
    }
}

impl ChoiceProvider for ObsIntegration {
    fn choices<'a>(&'a self, source: &'a str, depends_on: Option<&'a str>) -> ChoiceFuture<'a> {
        Box::pin(async move {
            let snapshot = self
                .resource_snapshot()
                .await
                .map_err(|error| error.to_string())?;
            let mut choices: Vec<ConfigChoice> = match source {
                "obs.scenes" => {
                    if depends_on.is_some() {
                        return Err("scene choices do not take a dependency".into());
                    }
                    snapshot
                        .scenes
                        .iter()
                        .map(|scene| ConfigChoice {
                            value: actions::scene_token(scene),
                            label: scene.name.clone(),
                            detail: None,
                        })
                        .collect()
                }
                "obs.inputs" => {
                    if depends_on.is_some() {
                        return Err("input choices do not take a dependency".into());
                    }
                    snapshot
                        .inputs
                        .iter()
                        .map(|input| ConfigChoice {
                            value: actions::input_token(input),
                            label: input.name.clone(),
                            detail: None,
                        })
                        .collect()
                }
                "obs.scene_items" => {
                    let scene_value = depends_on
                        .filter(|value| !value.trim().is_empty())
                        .ok_or("select a scene before choosing an item")?;
                    let matching: Vec<_> = snapshot
                        .scenes
                        .iter()
                        .filter(|scene| {
                            actions::scene_token(scene) == scene_value || scene.name == scene_value
                        })
                        .collect();
                    let scene = match matching.as_slice() {
                        [scene] => scene,
                        [] => return Err("the selected OBS scene is no longer available".into()),
                        _ => return Err("the selected OBS scene is ambiguous".into()),
                    };
                    snapshot
                        .scene_items
                        .iter()
                        .filter(|item| item.scene.uuid == scene.uuid)
                        .map(|item| ConfigChoice {
                            value: actions::scene_item_token(item),
                            label: item.source_name.clone(),
                            detail: Some(if item.group_path.is_empty() {
                                format!("{} · #{}", scene.name, item.item_id)
                            } else {
                                format!(
                                    "{} / {} · #{}",
                                    scene.name,
                                    item.group_path.join(" / "),
                                    item.item_id
                                )
                            }),
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

#[cfg(test)]
mod tests {
    use super::*;

    struct NoPassword;

    impl ObsCredentialStore for NoPassword {
        fn load(&self) -> Result<Option<String>, ObsCredentialError> {
            Ok(None)
        }

        fn save(&self, _: &str) -> Result<(), ObsCredentialError> {
            Ok(())
        }

        fn clear(&self) -> Result<(), ObsCredentialError> {
            Ok(())
        }
    }

    fn integration(dir: &tempfile::TempDir) -> ObsIntegration {
        let paths = AppPaths {
            config: dir.path().join("config"),
            data: dir.path().join("data"),
            state: dir.path().join("state"),
        };
        ObsIntegration {
            connection: Arc::new(ObsConnection {
                settings: RwLock::new(ObsSettings::default()),
                credentials: Arc::new(NoPassword),
                client: Mutex::new(None),
                health: StatusCell::new(ObsHealth::Disabled),
                changed: watch::channel(0).0,
            }),
            event_health: Arc::new(StatusCell::new(ObsHealth::Disabled)),
            settings: ObsSettingsStore::new(&paths),
            routes: watch::channel(Vec::new()).0,
            configured: StatusCell::new(false),
        }
    }

    #[test]
    fn event_health_does_not_hide_unready_actions() {
        let dir = tempfile::tempdir().unwrap();
        let integration = integration(&dir);
        integration.event_health.set(ObsHealth::Connected).unwrap();
        assert_eq!(integration.health(), ObsHealth::Disabled);
        integration
            .event_health
            .set(ObsHealth::Error("events failed".into()))
            .unwrap();
        assert_eq!(
            integration.health(),
            ObsHealth::Error("events failed".into())
        );
    }

    #[tokio::test]
    async fn configuration_and_both_health_sources_notify_without_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let integration = integration(&dir);
        let mut changes = integration.subscribe_configuration_changes();
        assert!(integration.connection_statuses().is_empty());
        integration
            .save_settings(integration.settings.load().unwrap(), ObsSettings::default())
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
        integration
            .connection
            .health
            .set(ObsHealth::Connecting)
            .unwrap();
        assert!(changes[0].has_changed().unwrap());
        assert_eq!(
            integration.connection_statuses()[0].state,
            ConnectionState::Connecting
        );
        changes[0].borrow_and_update();
        integration
            .connection
            .health
            .set(ObsHealth::Connecting)
            .unwrap();
        assert!(!changes[0].has_changed().unwrap());
        integration
            .event_health
            .set(ObsHealth::Retrying("event transport failed".into()))
            .unwrap();
        assert!(changes[1].has_changed().unwrap());
        assert_eq!(
            integration.connection_statuses()[0].detail,
            "Retrying: event transport failed"
        );
    }

    #[tokio::test]
    async fn rejected_settings_do_not_publish_configuration_changes() {
        let dir = tempfile::tempdir().unwrap();
        let integration = integration(&dir);
        let changes = integration.subscribe_configuration_changes();
        let invalid = ObsSettings {
            port: 0,
            ..ObsSettings::default()
        };
        assert!(
            integration
                .save_settings(integration.settings.load().unwrap(), invalid)
                .await
                .is_err()
        );
        assert!(integration.connection_statuses().is_empty());
        assert!(changes.iter().all(|change| !change.has_changed().unwrap()));
    }
}
