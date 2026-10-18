//! Local authenticated API for overlays and other named consumers.

mod actions;
mod config;
mod credentials;
mod events;
mod hosting;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{DefaultBodyLimit, Path, Query, State, rejection::JsonRejection};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{
    IntoResponse, Response, Sse,
    sse::{Event, KeepAlive},
};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::stream;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};

use crate::integration::{
    ConfigurationStatus, ConnectionState, ConnectionStatus, ReconfigurationRequest, StatusCell,
};
use crate::providers::ProviderIntegration;
use crate::runtime::{AppRuntime, RuntimeError};
use crate::storage::SaveOutcome;

use config::ApiStore;
use config::Consumer;
pub use config::{
    ApiError, ApiSettings, ConsumerPermissions, ConsumerSetting, ConsumerSummary, CreatedConsumer,
};
pub use credentials::{ConsumerCredentialStore, SystemConsumerCredentialStore};
use events::EventSubscription;
pub use events::{EventFrame, EventHub};

const ALLOWED_CHANNELS: [&str; 4] = [
    "twitch.chat",
    "twitch.support",
    "twitch.channel",
    "api.custom",
];

/// Returns the compiled channel IDs available to consumer permissions.
pub fn channel_ids() -> &'static [&'static str] {
    &ALLOWED_CHANNELS
}

/// Human-facing channel choices for permission editors.
pub fn channel_choices() -> Vec<crate::choices::ConfigChoice> {
    [
        (
            "api.custom",
            "Custom events",
            "Receive named JSON events sent by API workflow actions.",
        ),
        (
            "twitch.chat",
            "Chat messages",
            "Receive chat messages, including resolved emotes and pronouns.",
        ),
        (
            "twitch.support",
            "Subscriptions and bits",
            "Receive subscriptions, resubscriptions, gifts and cheers.",
        ),
        (
            "twitch.channel",
            "Current game and stream",
            "Receive the current category and stream details when they change.",
        ),
    ]
    .into_iter()
    .map(|(value, label, detail)| crate::choices::ConfigChoice {
        value: value.into(),
        label: label.into(),
        detail: Some(detail.into()),
    })
    .collect()
}

/// API service shared between the desktop settings and HTTP handlers.
pub struct ApiService {
    store: Arc<ApiStore>,
    events: Arc<EventHub>,
    providers: Arc<ProviderIntegration>,
    connection_slots: Arc<Semaphore>,
    storage_slots: Arc<Semaphore>,
    configuration_lock: Mutex<()>,
    status: StatusCell<ConnectionStatus>,
    hosting_shutdown: tokio_util::sync::CancellationToken,
    listener_address: Mutex<Option<SocketAddr>>,
}

/// Clears the advertised listener address whenever its serving future stops or is dropped.
struct ListenerAddressGuard<'a> {
    address: &'a Mutex<Option<SocketAddr>>,
}

impl Drop for ListenerAddressGuard<'_> {
    /// Prevents copying a stale destination after shutdown, reconfiguration or a task failure.
    fn drop(&mut self) {
        *self
            .address
            .lock()
            .expect("API listener address lock poisoned") = None;
    }
}

impl ApiService {
    /// Registers the API's native actions and resource choices alongside its service.
    pub fn register(
        paths: &crate::paths::AppPaths,
        providers: Arc<ProviderIntegration>,
        engine: &mut crate::engine::Engine,
        choices: &mut crate::choices::ChoiceCatalog,
    ) -> Arc<Self> {
        let service = Self::new(paths, providers);
        actions::register(engine, choices, Arc::clone(&service));
        service
    }
    /// Loads API configuration and initializes the bounded live event hub.
    pub fn new(paths: &crate::paths::AppPaths, providers: Arc<ProviderIntegration>) -> Arc<Self> {
        Self::with_store(paths, providers, Arc::new(ApiStore::new(paths)))
    }

    /// Initializes API services with a supplied administrative credential backend and no keyring reads.
    pub fn new_with_credentials(
        paths: &crate::paths::AppPaths,
        providers: Arc<ProviderIntegration>,
        credentials: Arc<dyn ConsumerCredentialStore>,
    ) -> Arc<Self> {
        Self::with_store(
            paths,
            providers,
            Arc::new(ApiStore::with_credentials(paths, credentials)),
        )
    }

    /// Builds the live API around an initialized store without additional credential operations.
    fn with_store(
        paths: &crate::paths::AppPaths,
        providers: Arc<ProviderIntegration>,
        store: Arc<ApiStore>,
    ) -> Arc<Self> {
        let settings = store.subscribe().borrow().clone();
        let status = if let Some(error) = store.startup_error() {
            api_status(ConnectionState::Error, error)
        } else if settings.enabled {
            api_status(ConnectionState::Connecting, "Starting")
        } else {
            api_status(ConnectionState::Inactive, "Disabled")
        };
        Arc::new(Self {
            store,
            events: EventHub::new(
                paths,
                settings.enabled && settings.record_support,
                settings.support_retention as usize,
            ),
            providers,
            connection_slots: Arc::new(Semaphore::new(64)),
            storage_slots: Arc::new(Semaphore::new(16)),
            configuration_lock: Mutex::new(()),
            status: StatusCell::new(status),
            hosting_shutdown: tokio_util::sync::CancellationToken::new(),
            listener_address: Mutex::new(None),
        })
    }

    /// Reports startup corruption without replacing its source file.
    pub fn startup_error(&self) -> Option<&str> {
        self.store.startup_error()
    }

    /// Returns the currently persisted listener settings.
    pub fn configuration(&self) -> Result<ApiSettings, ApiError> {
        self.store.configuration()
    }

    /// Reports saved listener configuration independently of its current enabled state.
    pub fn is_configured(&self) -> Result<bool, ApiError> {
        self.store.is_configured()
    }

    /// Saves settings and applies demand and listener binding changes.
    pub fn save_configuration(
        &self,
        expected: ApiSettings,
        settings: ApiSettings,
    ) -> Result<SaveOutcome, ApiError> {
        let _guard = self
            .configuration_lock
            .lock()
            .expect("API configuration lock poisoned");
        let outcome = self.store.save_configuration(&expected, settings.clone())?;
        self.events.configure(
            settings.enabled && settings.record_support,
            settings.support_retention as usize,
        );
        self.status
            .set(if settings.enabled {
                api_status(ConnectionState::Connecting, "Restarting listener")
            } else {
                api_status(ConnectionState::Inactive, "Disabled")
            })
            .expect("API status lock poisoned");
        Ok(outcome)
    }

    /// Lists consumer records for administration.
    pub fn list_consumers(&self) -> Result<Vec<ConsumerSummary>, ApiError> {
        self.store.list_consumers()
    }

    /// Reads a consumer's current editable grant from storage.
    pub fn consumer_configuration(&self, id: &str) -> Result<ConsumerSummary, ApiError> {
        self.store.consumer_configuration(id)
    }

    /// Creates a consumer token that is only returned by this call.
    pub fn create_consumer(
        &self,
        name: String,
        permissions: ConsumerPermissions,
        allowed_origins: BTreeSet<String>,
        static_folder: Option<String>,
    ) -> Result<CreatedConsumer, ApiError> {
        let _guard = self
            .configuration_lock
            .lock()
            .expect("API configuration lock poisoned");
        self.store
            .create_consumer(name, permissions, allowed_origins, static_folder)
    }

    /// Creates a consumer whose token can later be recovered through native administration.
    pub fn create_consumer_with_stored_token(
        &self,
        name: String,
        permissions: ConsumerPermissions,
        allowed_origins: BTreeSet<String>,
        static_folder: Option<String>,
    ) -> Result<CreatedConsumer, ApiError> {
        let _guard = self
            .configuration_lock
            .lock()
            .expect("API configuration lock poisoned");
        self.store.create_consumer_with_stored_token(
            name,
            permissions,
            allowed_origins,
            static_folder,
        )
    }

    /// Replaces a lost token without changing the consumer UUID, settings, folder or grants.
    pub fn regenerate_consumer_token(&self, id: &str) -> Result<CreatedConsumer, ApiError> {
        let _guard = self
            .configuration_lock
            .lock()
            .expect("API configuration lock poisoned");
        self.store.regenerate_consumer_token(id)
    }

    /// Builds a current hosting link from the bound listener and an already retained token.
    pub fn consumer_hosting_url(&self, id: &str) -> Result<String, ApiError> {
        let _guard = self
            .configuration_lock
            .lock()
            .expect("API configuration lock poisoned");
        if !self.configuration()?.enabled {
            return Err(ApiError::Invalid(
                "Enable the API server before copying its hosting URL.".into(),
            ));
        }
        let address = self
            .listener_address
            .lock()
            .expect("API listener address lock poisoned")
            .ok_or_else(|| {
                ApiError::Invalid(
                    "The API listener is not running. Check its connection status and try again."
                        .into(),
                )
            })?;
        let token = self.store.consumer_hosting_token(id)?;
        credentials::hosting_url(address, &token)
    }

    /// Updates a named consumer without changing its credential.
    pub fn update_consumer(
        &self,
        id: &str,
        name: String,
        permissions: ConsumerPermissions,
        allowed_origins: BTreeSet<String>,
        static_folder: Option<String>,
    ) -> Result<(), ApiError> {
        let _guard = self
            .configuration_lock
            .lock()
            .expect("API configuration lock poisoned");
        self.store
            .update_consumer(id, name, permissions, allowed_origins, static_folder)
    }

    /// Saves a consumer edit after comparing the original grant under the API lock.
    pub fn save_consumer_configuration(
        &self,
        expected: &ConsumerSummary,
        name: String,
        permissions: ConsumerPermissions,
        allowed_origins: BTreeSet<String>,
        static_folder: Option<String>,
    ) -> Result<(ConsumerSummary, SaveOutcome), ApiError> {
        let _guard = self
            .configuration_lock
            .lock()
            .expect("API configuration lock poisoned");
        self.store.save_consumer_configuration(
            expected,
            name,
            permissions,
            allowed_origins,
            static_folder,
        )
    }

    /// Revokes a consumer immediately for new requests.
    pub fn revoke_consumer(&self, id: &str) -> Result<(), ApiError> {
        let _guard = self
            .configuration_lock
            .lock()
            .expect("API configuration lock poisoned");
        self.store.revoke_consumer(id)?;
        self.events.forget_consumer(id);
        Ok(())
    }

    /// Shares overlay demand with Twitch's existing EventSub listener.
    pub fn subscribe_demand(&self) -> watch::Receiver<BTreeSet<String>> {
        self.events.subscribe_demand()
    }

    /// Shares event state with the Twitch listener.
    pub fn event_hub(&self) -> Arc<EventHub> {
        Arc::clone(&self.events)
    }

    /// Publishes a decoded Twitch event to live consumers.
    pub fn publish_twitch(
        &self,
        event: &crate::twitch::events::TwitchEvent,
        envelope: &Value,
    ) -> Result<(), ApiError> {
        self.events.publish_twitch(event, envelope)
    }

    /// Publishes the current channel before EventSub updates arrive.
    pub fn publish_channel(&self, id: &str, name: &str, broadcaster_id: &str) {
        self.events.publish_channel(id, name, broadcaster_id);
    }

    /// Imports a validated legacy support-history export through native administration.
    pub fn import_support_history(&self, events: Vec<Value>) -> Result<usize, ApiError> {
        self.events.import_support_history(events)
    }

    /// Runs the configurable HTTP listener and bounded history writer in one owned task.
    pub fn start(self: &Arc<Self>, runtime: &AppRuntime) -> Result<(), RuntimeError> {
        if self.store.startup_error().is_some() {
            return Ok(());
        }
        let service = Arc::clone(self);
        runtime.spawn_task("overlay API", move |shutdown| async move {
            let _hosting_shutdown = service.hosting_shutdown.clone().drop_guard();
            let server = service.run_server(shutdown.clone());
            let health = service.monitor_history(shutdown.clone());
            let writer = async {
                service
                    .events
                    .run_history(shutdown)
                    .await
                    .map_err(|error| error.to_string())
            };
            tokio::try_join!(server, writer, health).map(|_| ())
        })
    }

    /// Reflects durable history failure and recovery in the desktop connection status.
    async fn monitor_history(
        &self,
        shutdown: tokio_util::sync::CancellationToken,
    ) -> Result<(), String> {
        let mut history = self.events.subscribe_history_unavailable();
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                changed = history.changed() => if changed.is_err() { break; },
            }
            if *history.borrow() {
                self.status
                    .set(api_status(
                        ConnectionState::Error,
                        "Support history unavailable",
                    ))
                    .expect("API status lock poisoned");
            } else if self.store.subscribe().borrow().enabled
                && self.status.read().expect("API status lock poisoned").detail
                    == "Support history unavailable"
            {
                self.status
                    .set(api_status(ConnectionState::Connected, "Listening"))
                    .expect("API status lock poisoned");
            }
        }
        Ok(())
    }

    /// Rebinds the listener when settings change and retries transient bind failures.
    async fn run_server(
        self: &Arc<Self>,
        shutdown: tokio_util::sync::CancellationToken,
    ) -> Result<(), String> {
        let mut settings = self.store.subscribe();
        loop {
            let current = settings.borrow().clone();
            if !current.enabled {
                self.status
                    .set(api_status(ConnectionState::Inactive, "Disabled"))
                    .expect("API status lock poisoned");
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    changed = settings.changed() => if changed.is_err() { break; },
                }
                continue;
            }
            let host = current
                .host
                .parse()
                .map_err(|error| format!("invalid API host: {error}"))?;
            let address = SocketAddr::new(host, current.port);
            let listener = match tokio::net::TcpListener::bind(address).await {
                Ok(listener) => listener,
                Err(_) => {
                    self.status
                        .set(api_status(
                            ConnectionState::Error,
                            "Could not bind listener",
                        ))
                        .expect("API status lock poisoned");
                    tokio::select! {
                        _ = shutdown.cancelled() => break,
                        changed = settings.changed() => if changed.is_err() { break; },
                        _ = tokio::time::sleep(Duration::from_secs(5)) => {},
                    }
                    continue;
                }
            };
            *self
                .listener_address
                .lock()
                .expect("API listener address lock poisoned") = Some(
                listener
                    .local_addr()
                    .map_err(|error| format!("API listener address is unavailable: {error}"))?,
            );
            let _listener_address = ListenerAddressGuard {
                address: &self.listener_address,
            };
            self.status
                .set(if *self.events.subscribe_history_unavailable().borrow() {
                    api_status(ConnectionState::Error, "Support history unavailable")
                } else {
                    api_status(ConnectionState::Connected, "Listening")
                })
                .expect("API status lock poisoned");
            let app = router(Arc::clone(self));
            tokio::select! {
                _ = shutdown.cancelled() => break,
                changed = settings.changed() => if changed.is_err() { break; },
                _ = axum::serve(listener, app).with_graceful_shutdown(shutdown.child_token().cancelled_owned()) => {
                    self.status.set(api_status(ConnectionState::Error, "Listener stopped")).expect("API status lock poisoned");
                    tokio::select! {
                        _ = shutdown.cancelled() => break,
                        changed = settings.changed() => if changed.is_err() { break; },
                        _ = tokio::time::sleep(Duration::from_secs(5)) => {},
                    }
                }
            }
        }
        Ok(())
    }
}

/// Mounts only the versioned consumer routes and their body-size limit.
fn router(service: Arc<ApiService>) -> Router {
    let files = Router::new()
        .route("/files/{token}/", get(hosting::index))
        .route(
            "/files/{token}/.snenkbot/reload.js",
            get(hosting::reload_script),
        )
        .route(
            "/files/{token}/.snenkbot/events",
            get(hosting::reload_events),
        )
        .route("/files/{token}/{*path}", get(hosting::asset))
        .route_layer(axum::middleware::map_response(|response: Response| async {
            hosting::private_response(response)
        }));
    Router::new()
        .merge(files)
        .route("/api/v1/events", get(events).options(preflight))
        .route(
            "/api/v1/settings/{key}",
            get(read_setting).put(write_setting).options(preflight),
        )
        .route(
            "/api/v1/secrets/{key}",
            get(read_secret).put(write_secret).options(preflight),
        )
        .route(
            "/api/v1/providers/{provider}/{operation}",
            post(call_provider).options(preflight),
        )
        .layer(DefaultBodyLimit::max(256 * 1024))
        .with_state(service)
}

/// Responds to a browser preflight for an explicitly allowed origin.
async fn preflight(State(service): State<Arc<ApiService>>, headers: HeaderMap) -> Response {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return StatusCode::FORBIDDEN.into_response();
    };
    let Ok(source) = origin.to_str() else {
        return StatusCode::FORBIDDEN.into_response();
    };
    if !service.store.origin_allowed(source).unwrap_or(false) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin.clone());
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, PUT, POST, OPTIONS"),
    );
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("Authorization, Content-Type"),
    );
    response
        .headers_mut()
        .insert(header::VARY, HeaderValue::from_static("Origin"));
    response
}

/// Resolves a bearer credential and enforces that consumer's exact browser origin.
fn authorize(
    service: &ApiService,
    headers: &HeaderMap,
) -> Result<(Consumer, Option<HeaderValue>), StatusCode> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let consumer = service
        .store
        .authenticate(token)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let origin = match headers.get(header::ORIGIN) {
        Some(value) => {
            let source = value.to_str().map_err(|_| StatusCode::FORBIDDEN)?;
            if !consumer.allowed_origins.contains(source) {
                return Err(StatusCode::FORBIDDEN);
            }
            Some(value.clone())
        }
        None => None,
    };
    Ok((consumer, origin))
}

/// Echoes an authorized browser origin on both successful and failed responses.
fn cors(mut response: Response, origin: Option<HeaderValue>) -> Response {
    if let Some(origin) = origin {
        response
            .headers_mut()
            .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        response
            .headers_mut()
            .insert(header::VARY, HeaderValue::from_static("Origin"));
    }
    response
}

/// Exposes authentication errors to browsers from registered origins without granting access.
fn auth_error(service: &ApiService, headers: &HeaderMap, code: StatusCode) -> Response {
    let origin = headers.get(header::ORIGIN).and_then(|value| {
        value
            .to_str()
            .ok()
            .and_then(|origin| {
                service
                    .store
                    .origin_allowed(origin)
                    .ok()
                    .filter(|allowed| *allowed)
            })
            .map(|_| value.clone())
    });
    cors(code.into_response(), origin)
}

/// Requested channels and optional cursor map from the event-stream URL.
#[derive(Deserialize)]
struct EventQuery {
    channels: String,
    cursors: Option<String>,
}

/// Per-connection state retained while streaming authenticated events.
struct StreamState {
    subscription: EventSubscription,
    consumer_id: String,
    pending: VecDeque<EventFrame>,
    service: Arc<ApiService>,
    token: String,
    origin: Option<String>,
    consumer_changes: watch::Receiver<u64>,
    settings_changes: watch::Receiver<ApiSettings>,
    _permit: OwnedSemaphorePermit,
}

impl StreamState {
    /// Rechecks grants after an administrative change.
    fn permitted(&self) -> bool {
        if !self.settings_changes.borrow().enabled {
            return false;
        }
        let Ok(Some(consumer)) = self.service.store.authenticate(&self.token) else {
            return false;
        };
        if self
            .origin
            .as_ref()
            .is_some_and(|origin| !consumer.allowed_origins.contains(origin))
        {
            return false;
        }
        self.subscription
            .channels
            .iter()
            .all(|channel| consumer.permissions.channels.contains(channel))
            && consumer.id == self.consumer_id
    }
}

/// Opens a bounded event stream after channel-grant and cursor validation.
async fn events(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Query(query): Query<EventQuery>,
) -> Response {
    let (consumer, origin) = match authorize(&service, &headers) {
        Ok(auth) => auth,
        Err(code) => return auth_error(&service, &headers, code),
    };
    let channels: BTreeSet<String> = query.channels.split(',').map(str::to_owned).collect();
    if channels.is_empty()
        || channels.iter().any(|channel| {
            !ALLOWED_CHANNELS.contains(&channel.as_str())
                || !consumer.permissions.channels.contains(channel)
        })
    {
        return cors(StatusCode::FORBIDDEN.into_response(), origin);
    }
    let cursors: BTreeMap<String, String> = match query.cursors {
        Some(value) => match serde_json::from_str(&value) {
            Ok(map) => map,
            Err(_) => return cors(StatusCode::BAD_REQUEST.into_response(), origin),
        },
        None => BTreeMap::new(),
    };
    let permit = match Arc::clone(&service.connection_slots).try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => return cors(StatusCode::TOO_MANY_REQUESTS.into_response(), origin),
    };
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or_default()
        .to_owned();
    let state = StreamState {
        subscription: service
            .events
            .subscribe_for_consumer(Some(&consumer.id), channels, cursors),
        consumer_id: consumer.id,
        pending: VecDeque::new(),
        consumer_changes: service.store.subscribe_consumer_changes(),
        settings_changes: service.store.subscribe(),
        origin: origin
            .as_ref()
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
        token,
        service,
        _permit: permit,
    };
    let stream = stream::unfold(state, |mut state| async move {
        loop {
            if !state.permitted() {
                return None;
            }
            if state.pending.is_empty() {
                state.pending.extend(state.subscription.initial.drain(..));
            }
            if let Some(frame) = state.pending.pop_front() {
                if !frame.visible_to(&state.consumer_id) {
                    continue;
                }
                let event = Event::default()
                    .data(serde_json::to_string(&frame).expect("event frame serializes"));
                return Some((Ok::<_, Infallible>(event), state));
            }
            tokio::select! {
                changed = state.consumer_changes.changed() => if changed.is_err() { return None; },
                changed = state.settings_changes.changed() => if changed.is_err() { return None; },
                frame = state.subscription.receiver.recv() => match frame {
                    Ok(frame) if state.subscription.channels.contains(&frame.channel) && frame.visible_to(&state.consumer_id) => state.pending.push_back(frame),
                    Ok(_) => {},
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        let cursors = state.subscription.channels.iter().map(|channel| (channel.clone(), "expired".to_owned())).collect();
                        state.subscription = state.service.events.subscribe_for_consumer(Some(&state.consumer_id), state.subscription.channels.clone(), cursors);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                },
            }
        }
    });
    cors(
        Sse::new(stream)
            .keep_alive(
                KeepAlive::new()
                    .interval(Duration::from_secs(15))
                    .text("keep-alive"),
            )
            .into_response(),
        origin,
    )
}

/// Reads the authenticated consumer's own optimistic-concurrency setting.
async fn read_setting(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path(key): Path<String>,
) -> Response {
    let (consumer, origin) = match authorize(&service, &headers) {
        Ok(auth) => auth,
        Err(code) => return auth_error(&service, &headers, code),
    };
    let response = match storage_call(&service, move |store| {
        store.read_setting(&consumer.id, &key)
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(error),
    };
    cors(response, origin)
}

/// Setting replacement with the revision the client last observed.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettingWrite {
    value: Value,
    expected_revision: u64,
}

/// Replaces one consumer setting if its revision still matches.
async fn write_setting(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path(key): Path<String>,
    body: Result<Json<SettingWrite>, JsonRejection>,
) -> Response {
    let (consumer, origin) = match authorize(&service, &headers) {
        Ok(auth) => auth,
        Err(code) => return auth_error(&service, &headers, code),
    };
    let Json(body) = match body {
        Ok(body) => body,
        Err(_) => return cors(StatusCode::BAD_REQUEST.into_response(), origin),
    };
    let response = match storage_call(&service, move |store| {
        store.write_setting(&consumer.id, &key, body.value, body.expected_revision)
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(error),
    };
    cors(response, origin)
}

/// Reads a named OS-credential secret only with an explicit read grant.
async fn read_secret(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path(key): Path<String>,
) -> Response {
    let (consumer, origin) = match authorize(&service, &headers) {
        Ok(auth) => auth,
        Err(code) => return auth_error(&service, &headers, code),
    };
    if !consumer.permissions.readable_secrets.contains(&key) {
        return cors(StatusCode::FORBIDDEN.into_response(), origin);
    }
    let response =
        match storage_call(&service, move |store| store.read_secret(&consumer.id, &key)).await {
            Ok(value) => Json(json!({ "value": value })).into_response(),
            Err(error) => api_error(error),
        };
    cors(response, origin)
}

/// Optional secret replacement; null removes the credential.
#[derive(Deserialize)]
struct SecretWrite {
    value: Option<String>,
}

/// Writes a named OS-credential secret only with an explicit write grant.
async fn write_secret(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path(key): Path<String>,
    body: Result<Json<SecretWrite>, JsonRejection>,
) -> Response {
    let (consumer, origin) = match authorize(&service, &headers) {
        Ok(auth) => auth,
        Err(code) => return auth_error(&service, &headers, code),
    };
    let Json(body) = match body {
        Ok(body) => body,
        Err(_) => return cors(StatusCode::BAD_REQUEST.into_response(), origin),
    };
    if !consumer.permissions.writable_secrets.contains(&key) {
        return cors(StatusCode::FORBIDDEN.into_response(), origin);
    }
    let response = match storage_call(&service, move |store| {
        store.write_secret(&consumer.id, &key, body.value.as_deref())
    })
    .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => api_error(error),
    };
    cors(response, origin)
}

/// Provider operation arguments passed to the integration's typed dispatcher.
#[derive(Deserialize)]
struct ProviderCall {
    arguments: Value,
}

/// Calls one enabled provider operation granted to the authenticated consumer.
async fn call_provider(
    State(service): State<Arc<ApiService>>,
    headers: HeaderMap,
    Path((provider, operation)): Path<(String, String)>,
    body: Result<Json<ProviderCall>, JsonRejection>,
) -> Response {
    let (consumer, origin) = match authorize(&service, &headers) {
        Ok(auth) => auth,
        Err(code) => return auth_error(&service, &headers, code),
    };
    let Json(body) = match body {
        Ok(body) => body,
        Err(_) => return cors(StatusCode::BAD_REQUEST.into_response(), origin),
    };
    if !consumer
        .permissions
        .providers
        .get(&provider)
        .is_some_and(|operations| operations.contains(&operation))
    {
        return cors(StatusCode::FORBIDDEN.into_response(), origin);
    }
    let providers = service.providers.services();
    let response = match providers.call(&provider, &operation, body.arguments).await {
        Ok(value) => Json(value).into_response(),
        Err(
            crate::providers::ProviderError::UnknownOperation
            | crate::providers::ProviderError::InvalidArguments(_),
        ) => StatusCode::BAD_REQUEST.into_response(),
        Err(
            crate::providers::ProviderError::Disabled
            | crate::providers::ProviderError::MissingCredential
            | crate::providers::ProviderError::ConfigurationChanged,
        ) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(crate::providers::ProviderError::Busy) => StatusCode::TOO_MANY_REQUESTS.into_response(),
        Err(crate::providers::ProviderError::TimedOut) => {
            StatusCode::GATEWAY_TIMEOUT.into_response()
        }
        Err(_) => StatusCode::BAD_GATEWAY.into_response(),
    };
    cors(response, origin)
}

/// Converts safe storage and validation failures to HTTP responses.
fn api_error(error: ApiError) -> Response {
    let code = match error {
        ApiError::Invalid(_) => StatusCode::BAD_REQUEST,
        ApiError::Conflict => StatusCode::CONFLICT,
        ApiError::Busy => StatusCode::TOO_MANY_REQUESTS,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    let message = if code.is_server_error() {
        "API storage is unavailable".to_owned()
    } else {
        error.to_string()
    };
    (code, Json(json!({ "error": message }))).into_response()
}

/// Runs bounded consumer storage and credential-manager work outside Tokio workers.
async fn storage_call<T: Send + 'static>(
    service: &ApiService,
    action: impl FnOnce(Arc<ApiStore>) -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError> {
    let permit = Arc::clone(&service.storage_slots)
        .try_acquire_owned()
        .map_err(|_| ApiError::Busy)?;
    let store = Arc::clone(&service.store);
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        action(store)
    })
    .await
    .map_err(|_| ApiError::Io(std::io::Error::other("API storage worker stopped")))?
}

/// Builds the public connection status without internal or credential data.
fn api_status(state: ConnectionState, detail: &str) -> ConnectionStatus {
    ConnectionStatus {
        integration: "api".into(),
        connection: "listener".into(),
        title: "Overlay API".into(),
        state,
        detail: detail.into(),
    }
}

impl ConfigurationStatus for ApiService {
    /// Requests repair when persisted API configuration cannot be loaded safely.
    fn reconfiguration_requests(&self) -> Vec<ReconfigurationRequest> {
        self.store
            .startup_error()
            .map(|_| ReconfigurationRequest {
                integration: "api",
                connection: "configuration",
                reason: "Overlay API configuration or consumer registry is unavailable".into(),
                affected_features: vec!["Overlay API".into()],
            })
            .into_iter()
            .collect()
    }

    /// Shows listener and durable-history health only while the API is configured to run.
    fn connection_statuses(&self) -> Vec<ConnectionStatus> {
        let status = self
            .status
            .read()
            .expect("API status lock poisoned")
            .clone();
        if status.state == ConnectionState::Inactive {
            Vec::new()
        } else {
            vec![status]
        }
    }

    /// Notifies desktop status readers when API health changes.
    fn subscribe_configuration_changes(&self) -> Vec<watch::Receiver<u64>> {
        vec![self.status.subscribe()]
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use futures_util::StreamExt;
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;

    use super::*;
    use crate::engine::{
        Engine, Event, FormValidators, InputField, InputProvider, InputResponse, Values,
    };

    /// Rejects UI input requests in HTTP-only tests.
    struct TestInput;

    impl InputProvider for TestInput {
        /// Rejects unexpected input requests during API route tests.
        fn request<'a>(
            &'a self,
            _title: String,
            _fields: Vec<InputField>,
            _defaults: Values,
            _validators: &'a FormValidators,
            _cancel: CancellationToken,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<InputResponse, String>> + Send + 'a>,
        > {
            Box::pin(async { Err("unexpected input request".into()) })
        }
    }

    /// Builds disabled provider and API services without accessing credentials or remote hosts.
    fn service(root: &std::path::Path) -> Arc<ApiService> {
        let paths = crate::paths::AppPaths {
            config: root.join("config"),
            data: root.join("data"),
            state: root.join("state"),
        };
        let mut engine = Engine::new(Arc::new(TestInput), Arc::new(|_: Event| {}));
        let providers = ProviderIntegration::register(&paths, &mut engine).unwrap();
        ApiService::new_with_credentials(
            &paths,
            providers,
            Arc::new(credentials::MemoryConsumerCredentialStore::default()),
        )
    }

    /// Copies only an existing retained token and uses the listener actually bound by the service.
    #[test]
    fn hosted_link_recovery_requires_saved_folder_enabled_api_and_listener() {
        let temporary = tempfile::tempdir().unwrap();
        let service = service(temporary.path());
        let created = service
            .create_consumer_with_stored_token(
                "Overlay".into(),
                ConsumerPermissions::default(),
                BTreeSet::new(),
                Some(temporary.path().to_string_lossy().into_owned()),
            )
            .unwrap();
        assert!(
            service
                .consumer_hosting_url(&created.id)
                .unwrap_err()
                .to_string()
                .contains("Enable")
        );
        let expected = service.configuration().unwrap();
        let mut settings = expected.clone();
        settings.enabled = true;
        service.save_configuration(expected, settings).unwrap();
        assert!(
            service
                .consumer_hosting_url(&created.id)
                .unwrap_err()
                .to_string()
                .contains("not running")
        );
        *service.listener_address.lock().unwrap() = Some("[::1]:45678".parse().unwrap());
        assert_eq!(
            service.consumer_hosting_url(&created.id).unwrap(),
            format!("http://[::1]:45678/files/{}/", created.token)
        );
        let legacy = service
            .create_consumer(
                "Legacy".into(),
                ConsumerPermissions::default(),
                BTreeSet::new(),
                Some(temporary.path().to_string_lossy().into_owned()),
            )
            .unwrap();
        assert!(
            service
                .consumer_hosting_url(&legacy.id)
                .unwrap_err()
                .to_string()
                .contains("Regenerate")
        );
        assert!(service.store.authenticate(&legacy.token).unwrap().is_some());
        let no_folder = service
            .create_consumer_with_stored_token(
                "No folder".into(),
                ConsumerPermissions::default(),
                BTreeSet::new(),
                None,
            )
            .unwrap();
        assert!(
            service
                .consumer_hosting_url(&no_folder.id)
                .unwrap_err()
                .to_string()
                .contains("static folder")
        );
        {
            let _guard = ListenerAddressGuard {
                address: &service.listener_address,
            };
        }
        assert!(service.listener_address.lock().unwrap().is_none());
    }

    /// Rotating a credential closes its old authenticated stream without discarding consumer identity.
    #[tokio::test]
    async fn regenerated_token_invalidates_open_event_stream() {
        let temporary = tempfile::tempdir().unwrap();
        let service = service(temporary.path());
        let expected = service.configuration().unwrap();
        let mut settings = expected.clone();
        settings.enabled = true;
        service.save_configuration(expected, settings).unwrap();
        let created = service
            .create_consumer_with_stored_token(
                "Overlay".into(),
                ConsumerPermissions {
                    channels: BTreeSet::from(["twitch.chat".into()]),
                    ..Default::default()
                },
                BTreeSet::new(),
                None,
            )
            .unwrap();
        let app = router(Arc::clone(&service));
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/events?channels=twitch.chat")
                    .header(header::AUTHORIZATION, format!("Bearer {}", created.token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.into_body().into_data_stream();
        body.next().await.unwrap().unwrap();
        let rotated = service.regenerate_consumer_token(&created.id).unwrap();
        assert_eq!(rotated.id, created.id);
        assert!(
            tokio::time::timeout(Duration::from_secs(1), body.next())
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            service
                .store
                .authenticate(&created.token)
                .unwrap()
                .is_none()
        );
        assert!(
            service
                .store
                .authenticate(&rotated.token)
                .unwrap()
                .is_some()
        );
    }

    /// A saved disabled listener remains configured without producing a status row.
    #[test]
    fn disabled_listener_is_configured_but_not_in_connections() {
        let temporary = tempfile::tempdir().unwrap();
        let service = service(temporary.path());
        assert!(!service.is_configured().unwrap());
        assert!(service.connection_statuses().is_empty());
        service
            .save_configuration(ApiSettings::default(), ApiSettings::default())
            .unwrap();
        assert!(service.is_configured().unwrap());
        assert!(service.connection_statuses().is_empty());
    }

    /// Checks protected static files, protocol semantics, live reload, and immediate revocation.
    #[tokio::test]
    async fn hosted_files_require_current_grants_and_keep_file_protocol_behavior() {
        let temporary = tempfile::tempdir().unwrap();
        let folder = temporary.path().join("site");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("index.html"), "<!doctype html><p>Overlay</p>").unwrap();
        std::fs::write(folder.join("main.css"), "body { color: red; }").unwrap();
        std::fs::create_dir(folder.join("empty")).unwrap();
        let service = service(temporary.path());
        service
            .save_configuration(
                ApiSettings::default(),
                ApiSettings {
                    enabled: true,
                    ..ApiSettings::default()
                },
            )
            .unwrap();
        let credential = service
            .create_consumer(
                "Hosted overlay".into(),
                ConsumerPermissions::default(),
                BTreeSet::new(),
                Some(folder.to_string_lossy().into_owned()),
            )
            .unwrap();
        let app = router(Arc::clone(&service));
        let prefix = format!("/files/{}/", credential.token);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("{prefix}main.css"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "text/css");
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(response.headers()[header::REFERRER_POLICY], "no-referrer");
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("HEAD")
                    .uri(format!("{prefix}main.css"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap()
                .is_empty()
        );
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("{prefix}main.css"))
                    .header(header::RANGE, "bytes=0-3")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap(),
            "body"
        );
        let response = app
            .clone()
            .oneshot(Request::builder().uri(&prefix).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let html = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        assert!(
            std::str::from_utf8(&html)
                .unwrap()
                .contains(".snenkbot/reload.js?revision=")
        );
        for path in [
            "/files/invalid/index.html".to_owned(),
            format!("{prefix}..%2Foutside"),
            format!("{prefix}empty/"),
        ] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        }
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(&prefix)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(response.headers()[header::REFERRER_POLICY], "no-referrer");
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("{prefix}.snenkbot/events"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut stream = response.into_body().into_data_stream();
        let initial = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            std::str::from_utf8(&initial)
                .unwrap()
                .contains("event: revision")
        );
        std::fs::write(folder.join("main.css"), "body { color: blue; }").unwrap();
        let update = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            std::str::from_utf8(&update)
                .unwrap()
                .contains("event: revision")
        );
        service.revoke_consumer(&credential.id).unwrap();
        let revoked = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            std::str::from_utf8(&revoked)
                .unwrap()
                .contains("event: revoked")
        );
        for path in [
            format!("{prefix}main.css"),
            format!("{prefix}.snenkbot/reload.js"),
            format!("{prefix}.snenkbot/events"),
        ] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
    }

    /// Verifies browser origin, bearer grant, secret denial, and live revocation.
    #[tokio::test]
    async fn http_authorization_origin_and_sse_grants_are_enforced() {
        let temporary = tempfile::tempdir().unwrap();
        let service = service(temporary.path());
        let mut settings = service.configuration().unwrap();
        settings.enabled = true;
        service
            .save_configuration(ApiSettings::default(), settings)
            .unwrap();
        let credential = service
            .create_consumer(
                "overlay".into(),
                ConsumerPermissions {
                    channels: BTreeSet::from(["twitch.chat".into()]),
                    ..ConsumerPermissions::default()
                },
                BTreeSet::from(["http://localhost:5173".into(), "null".into()]),
                None,
            )
            .unwrap();
        let app = router(Arc::clone(&service));
        let path = "/api/v1/events?channels=twitch.chat";
        let denied = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        assert!(service.subscribe_demand().borrow().is_empty());
        let denied_origin = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", credential.token),
                    )
                    .header(header::ORIGIN, "http://evil.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(denied_origin.status(), StatusCode::FORBIDDEN);
        let preflight = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("OPTIONS")
                    .uri(path)
                    .header(header::ORIGIN, "http://localhost:5173")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(preflight.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            preflight.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
            "http://localhost:5173"
        );
        // Files opened directly in a browser send Origin: null and still require a valid bearer.
        let local_preflight = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("OPTIONS")
                    .uri(path)
                    .header(header::ORIGIN, "null")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(local_preflight.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            local_preflight.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
            "null"
        );
        let local_request = || {
            Request::builder()
                .uri("/api/v1/settings/animations")
                .header(header::ORIGIN, "null")
        };
        let local_denied = app
            .clone()
            .oneshot(local_request().body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(local_denied.status(), StatusCode::UNAUTHORIZED);
        let local_allowed = app
            .clone()
            .oneshot(
                local_request()
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", credential.token),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(local_allowed.status(), StatusCode::OK);
        assert_eq!(
            local_allowed.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
            "null"
        );
        let denied_secret = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/secrets/private")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", credential.token),
                    )
                    .header(header::ORIGIN, "http://localhost:5173")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(denied_secret.status(), StatusCode::FORBIDDEN);
        let response = app
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", credential.token),
                    )
                    .header(header::ORIGIN, "http://localhost:5173")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.into_body().into_data_stream();
        let first = tokio::time::timeout(Duration::from_secs(1), body.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(std::str::from_utf8(&first).unwrap().contains("twitch.chat"));
        service.revoke_consumer(&credential.id).unwrap();
        let next = tokio::time::timeout(Duration::from_secs(1), body.next())
            .await
            .unwrap();
        assert!(next.is_none());
    }

    /// A targeted replay crosses the SSE boundary only for its authenticated recipient.
    #[tokio::test]
    async fn targeted_replay_reaches_only_its_named_consumer_stream() {
        let temporary = tempfile::tempdir().unwrap();
        let service = service(temporary.path());
        let mut settings = service.configuration().unwrap();
        settings.enabled = true;
        service
            .save_configuration(ApiSettings::default(), settings)
            .unwrap();
        let permissions = ConsumerPermissions {
            channels: BTreeSet::from(["twitch.support".into()]),
            ..ConsumerPermissions::default()
        };
        let alice = service
            .create_consumer("alice".into(), permissions.clone(), BTreeSet::new(), None)
            .unwrap();
        let bob = service
            .create_consumer("bob".into(), permissions, BTreeSet::new(), None)
            .unwrap();
        let hub = service.event_hub();
        hub.set_broadcaster("100").unwrap();
        let app = router(Arc::clone(&service));
        let open = |token: &str| {
            Request::builder()
                .uri("/api/v1/events?channels=twitch.support")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap()
        };
        let alice_response = app.clone().oneshot(open(&alice.token)).await.unwrap();
        let bob_response = app.oneshot(open(&bob.token)).await.unwrap();
        assert_eq!(alice_response.status(), StatusCode::OK);
        assert_eq!(bob_response.status(), StatusCode::OK);
        let mut alice_stream = alice_response.into_body().into_data_stream();
        let mut bob_stream = bob_response.into_body().into_data_stream();
        alice_stream.next().await.unwrap().unwrap();
        bob_stream.next().await.unwrap().unwrap();
        let event = crate::twitch::events::TwitchEvent::Cheer(serde_json::json!({
            "broadcaster_user_id":"100", "user_name":"Viewer", "bits":100
        }));
        let envelope = serde_json::json!({
            "metadata":{"message_id":"original","message_timestamp":"2026-10-18T08:00:00Z"},
            "payload":{"event":{"broadcaster_user_id":"100"}}
        });
        hub.replay_twitch_consumer(&event, &envelope, "twitch.support", &alice.id)
            .unwrap();
        let delivered = tokio::time::timeout(Duration::from_secs(1), alice_stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(std::str::from_utf8(&delivered).unwrap().contains("replay-"));
        assert!(
            tokio::time::timeout(Duration::from_millis(100), bob_stream.next())
                .await
                .is_err()
        );
    }
}
