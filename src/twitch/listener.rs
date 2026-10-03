//! Demand-driven Twitch EventSub subscriptions and workflow activation.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::time::{Instant, timeout_at};
use tokio_util::sync::CancellationToken;

use crate::engine::Values;
use crate::integration::{ReconfigurationRequest, StatusCell};
use crate::workflows::WorkflowDefinition;

use super::accounts::{ConnectedAccounts, TwitchAccounts};
use super::channel_changes::ChannelChanges;
use super::credentials::{CredentialBackend, TwitchRole};
use super::device::FormTransport;
use super::echoes::ChatEchoes;
use super::events::{EventSubKind, RouteOptions, decode_event, required_kinds, route_event};
use super::eventsub::{EventSubError, EventSubFrame, EventSubSocket, valid_twitch_url};
use super::helix::{Helix, HelixError, HelixTransport};
use super::session::{TwitchCapability, TwitchSession};

const EVENTSUB_URL: &str = "wss://eventsub.wss.twitch.tv/ws";
const MAX_RECENT_IDS: usize = 2_048;
const MAX_RETRY: Duration = Duration::from_secs(30);
type Reconnect = Pin<Box<dyn Future<Output = Result<EventSubSocket, EventSubError>> + Send>>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TwitchHealth {
    Disabled,
    Connecting,
    Connected,
    Degraded(String),
    Retrying(String),
    Error(String),
}

#[derive(Clone, Debug)]
pub struct TwitchActivation {
    pub workflow_id: String,
    pub trigger_id: String,
    pub values: Values,
}

enum SubscriptionFailure {
    Configuration(ReconfigurationRequest),
    Service(String),
}

impl SubscriptionFailure {
    fn message(&self) -> &str {
        match self {
            Self::Configuration(request) => &request.reason,
            Self::Service(message) => message,
        }
    }
}

pub struct TwitchListener<B, T, H> {
    accounts: Arc<TwitchAccounts<B>>,
    session: Arc<TwitchSession<B, T>>,
    helix: Arc<Helix<H>>,
    echoes: Arc<ChatEchoes>,
    routes: watch::Receiver<Vec<WorkflowDefinition>>,
    health: Arc<StatusCell<TwitchHealth>>,
    endpoint: String,
    changes: ChannelChanges,
}

impl<B, T, H> TwitchListener<B, T, H>
where
    B: CredentialBackend + 'static,
    T: FormTransport + Send + Sync + 'static,
    H: HelixTransport + Send + Sync + 'static,
{
    pub fn new(
        accounts: Arc<TwitchAccounts<B>>,
        session: Arc<TwitchSession<B, T>>,
        helix: Arc<Helix<H>>,
        echoes: Arc<ChatEchoes>,
        routes: watch::Receiver<Vec<WorkflowDefinition>>,
        health: Arc<StatusCell<TwitchHealth>>,
    ) -> Self {
        Self {
            accounts,
            session,
            helix,
            echoes,
            routes,
            health,
            endpoint: EVENTSUB_URL.to_owned(),
            changes: ChannelChanges::default(),
        }
    }

    pub async fn run(
        mut self,
        shutdown: CancellationToken,
        on_activation: impl Fn(TwitchActivation) + Send + Sync + 'static,
        on_error: impl Fn(String) + Send + Sync + 'static,
    ) {
        let mut backoff = Duration::from_secs(1);
        let mut seen = RecentIds::default();
        loop {
            let definitions = self.routes.borrow().clone();
            if !details_demand(&definitions) {
                self.changes.reset();
            }
            let desired = match required_kinds(&definitions) {
                Ok(kinds) => kinds,
                Err(error) => {
                    self.set_health(TwitchHealth::Error(error));
                    if !self.wait_for_change(&shutdown).await {
                        break;
                    }
                    continue;
                }
            };
            if desired.is_empty() {
                self.set_health(TwitchHealth::Disabled);
                if !self.wait_for_change(&shutdown).await {
                    break;
                }
                continue;
            }
            // With only unavailable ad listeners, wait without opening a socket
            // that cannot meet Twitch's first-subscription deadline.
            if desired.len() == 1
                && let Ok(accounts) = self.connected_accounts().await
                && let Some(request) = super::configuration_request(&accounts, &desired)
            {
                self.set_health(TwitchHealth::Degraded(request.reason));
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    changed = self.routes.changed() => if changed.is_err() { break; },
                    _ = tokio::time::sleep(Duration::from_secs(2)) => {},
                }
                continue;
            }
            self.set_health(TwitchHealth::Connecting);
            let result = self
                .connected_loop(&shutdown, &on_activation, &on_error, &mut seen)
                .await;
            if shutdown.is_cancelled() {
                break;
            }
            match result {
                ConnectionExit::RoutesChanged => {
                    backoff = Duration::from_secs(1);
                }
                ConnectionExit::Lost(error) => {
                    self.set_health(TwitchHealth::Retrying(error.clone()));
                    on_error(format!("Twitch events unavailable: {error}"));
                    tokio::select! {
                        _ = shutdown.cancelled() => break,
                        changed = self.routes.changed() => if changed.is_err() { break; },
                        _ = tokio::time::sleep(backoff) => {}
                    }
                    backoff = (backoff * 2).min(MAX_RETRY);
                }
            }
        }
        self.set_health(TwitchHealth::Disabled);
    }

    async fn connected_loop(
        &mut self,
        shutdown: &CancellationToken,
        on_activation: &(impl Fn(TwitchActivation) + Send + Sync),
        on_error: &(impl Fn(String) + Send + Sync),
        seen: &mut RecentIds,
    ) -> ConnectionExit {
        let authorized = match self.session.authorize(TwitchCapability::ChannelOwner).await {
            Ok(authorized) => authorized,
            Err(error) => return ConnectionExit::Lost(error.to_string()),
        };
        let broadcaster_id = authorized.user_id().to_owned();
        let connected_accounts = match self.connected_accounts().await {
            Ok(accounts) => accounts,
            Err(error) => return ConnectionExit::Lost(error),
        };
        if connected_accounts
            .get(TwitchRole::Broadcaster)
            .map(|account| account.user_id.as_str())
            != Some(broadcaster_id.as_str())
        {
            return ConnectionExit::RoutesChanged;
        }
        let bot_user_id = connected_accounts
            .get(TwitchRole::Bot)
            .map(|account| account.user_id.clone());
        if let Err(error) = self
            .prepare_channel_changes(&broadcaster_id, authorized.access_token(), true)
            .await
        {
            return ConnectionExit::Lost(error);
        }
        let mut socket = match EventSubSocket::connect(&self.endpoint).await {
            Ok(socket) => socket,
            Err(error) => return ConnectionExit::Lost(error.to_string()),
        };
        let mut subscriptions = BTreeMap::new();
        let first_subscription_deadline = Instant::now() + Duration::from_secs(8);
        let mut desired = match required_kinds(&self.routes.borrow().clone()) {
            Ok(kinds) => kinds,
            Err(error) => return ConnectionExit::Lost(error),
        };
        if desired.is_empty() {
            return ConnectionExit::RoutesChanged;
        }
        let failures = match self
            .reconcile(
                &mut subscriptions,
                &desired,
                &broadcaster_id,
                socket.session_id(),
                authorized.access_token(),
                Some(first_subscription_deadline),
            )
            .await
        {
            Ok(failures) => failures,
            Err(error) => return ConnectionExit::Lost(error),
        };
        if let Some(error) = self.set_subscription_health(failures) {
            on_error(error);
        }
        let mut identity_check = tokio::time::interval(Duration::from_secs(10));
        identity_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        identity_check.tick().await;
        let mut subscription_retry = tokio::time::interval(Duration::from_secs(30));
        subscription_retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        subscription_retry.tick().await;
        let mut reconnect: Option<Reconnect> = None;
        loop {
            let mut needs_reconcile = false;
            let details_deadline = self.changes.deadline();
            tokio::select! {
                _ = async { tokio::time::sleep_until(details_deadline.expect("guarded deadline")).await }, if details_deadline.is_some() => {
                    if let Some(event) = self.changes.take_ready(Instant::now()) {
                        for route in route_event(&super::events::TwitchEvent::DetailsChanged(event), &self.routes.borrow(), RouteOptions { bot_user_id: bot_user_id.as_deref() }) {
                            on_activation(TwitchActivation { workflow_id: route.workflow_id, trigger_id: route.trigger_id, values: route.values });
                        }
                    }
                }
                _ = shutdown.cancelled() => return ConnectionExit::RoutesChanged,
                changed = self.routes.changed() => {
                    if changed.is_err() {
                        return ConnectionExit::RoutesChanged;
                    }
                    desired = match required_kinds(&self.routes.borrow().clone()) {
                        Ok(kinds) => kinds,
                        Err(error) => return ConnectionExit::Lost(error),
                    };
                    if desired.is_empty() {
                        return ConnectionExit::RoutesChanged;
                    }
                    if reconnect.is_some() {
                        continue;
                    }
                    let authorized = match self.session.authorize(TwitchCapability::ChannelOwner).await {
                        Ok(authorized) if authorized.user_id() == broadcaster_id => authorized,
                        Ok(_) => return ConnectionExit::RoutesChanged,
                        Err(error) => return ConnectionExit::Lost(error.to_string()),
                    };
                    if let Err(error) = self.prepare_channel_changes(&broadcaster_id, authorized.access_token(), false).await {
                        return ConnectionExit::Lost(error);
                    }
                    match self
                        .reconcile(
                            &mut subscriptions,
                            &desired,
                            &broadcaster_id,
                            socket.session_id(),
                            authorized.access_token(),
                            None,
                        )
                        .await
                    {
                        Ok(failures) => {
                            if let Some(error) = self.set_subscription_health(failures) {
                                on_error(error);
                            }
                        }
                        Err(error) => return ConnectionExit::Lost(error),
                    }
                }
                _ = subscription_retry.tick() => {
                    if reconnect.is_none() && subscriptions.len() < desired.len() {
                        let authorized = match self.session.authorize(TwitchCapability::ChannelOwner).await {
                            Ok(authorized) if authorized.user_id() == broadcaster_id => authorized,
                            Ok(_) => return ConnectionExit::RoutesChanged,
                            Err(error) => return ConnectionExit::Lost(error.to_string()),
                        };
                        match self.reconcile(
                            &mut subscriptions,
                            &desired,
                            &broadcaster_id,
                            socket.session_id(),
                            authorized.access_token(),
                            None,
                        ).await {
                            Ok(failures) => {
                                if let Some(error) = self.set_subscription_health(failures) {
                                    on_error(error);
                                }
                            }
                            Err(error) => return ConnectionExit::Lost(error),
                        }
                    }
                }
                _ = identity_check.tick() => {
                    match self.connected_accounts().await {
                        Ok(current) if current == connected_accounts => {}
                        Ok(_) => return ConnectionExit::RoutesChanged,
                        Err(error) => return ConnectionExit::Lost(error),
                    }
                }
                connected = async { reconnect.as_mut().expect("guarded reconnect").await }, if reconnect.is_some() => {
                    match connected {
                        Ok(next) => {
                            socket = next;
                            reconnect = None;
                            needs_reconcile = true;
                        }
                        Err(error) => return ConnectionExit::Lost(error.to_string()),
                    }
                }
                frame = socket.next_frame() => {
                    match frame {
                        Ok(EventSubFrame::Keepalive) => {}
                        Ok(EventSubFrame::Notification { id, subscription_type, envelope }) => {
                            if !seen.insert(id) {
                                continue;
                            }
                            match decode_event(&envelope) {
                                Ok(event) if event.kind().event_type() == subscription_type => {
                                    if let super::events::TwitchEvent::Message(message) = &event
                                        && self.echoes.contains(&message.message_id)
                                    {
                                        continue;
                                    }
                                    let definitions = self.routes.borrow().clone();
                                    if let super::events::TwitchEvent::ChannelUpdated(channel) = &event
                                        && details_demand(&definitions)
                                        && channel.broadcaster_user_id == broadcaster_id
                                    {
                                        self.changes.observe(channel.clone(), Instant::now());
                                    }
                                    for route in route_event(&event, &definitions, RouteOptions {
                                        bot_user_id: bot_user_id.as_deref(),
                                    }) {
                                        on_activation(TwitchActivation {
                                            workflow_id: route.workflow_id,
                                            trigger_id: route.trigger_id,
                                            values: route.values,
                                        });
                                    }
                                }
                                Ok(_) => on_error("Twitch event type disagreed with its subscription".to_owned()),
                                Err(error) => on_error(format!("Twitch event could not be decoded: {error}")),
                            }
                        }
                        Ok(EventSubFrame::Revocation { subscription_type, status }) => {
                            return ConnectionExit::Lost(format!(
                                "Twitch revoked {subscription_type}: {status}"
                            ));
                        }
                        Ok(EventSubFrame::Reconnect { url }) => {
                            if !valid_twitch_url(&url) && url != self.endpoint {
                                return ConnectionExit::Lost("Twitch sent an invalid reconnect address".into());
                            }
                            if reconnect.is_none() {
                                reconnect = Some(Box::pin(async move {
                                    EventSubSocket::connect(&url).await
                                }));
                            }
                        }
                        Err(error) => {
                            if let Some(waiting) = reconnect.take() {
                                match waiting.await {
                                    Ok(next) => {
                                        socket = next;
                                        needs_reconcile = true;
                                    }
                                    Err(_) => return ConnectionExit::Lost(error.to_string()),
                                }
                            } else {
                                return ConnectionExit::Lost(error.to_string());
                            }
                        }
                    }
                }
            }
            if needs_reconcile {
                let authorized = match self.session.authorize(TwitchCapability::ChannelOwner).await
                {
                    Ok(authorized) if authorized.user_id() == broadcaster_id => authorized,
                    Ok(_) => return ConnectionExit::RoutesChanged,
                    Err(error) => return ConnectionExit::Lost(error.to_string()),
                };
                match self
                    .reconcile(
                        &mut subscriptions,
                        &desired,
                        &broadcaster_id,
                        socket.session_id(),
                        authorized.access_token(),
                        None,
                    )
                    .await
                {
                    Ok(failures) => {
                        if let Some(error) = self.set_subscription_health(failures) {
                            on_error(error);
                        }
                    }
                    Err(error) => return ConnectionExit::Lost(error),
                }
            }
        }
    }

    async fn prepare_channel_changes(
        &mut self,
        broadcaster_id: &str,
        token: &str,
        refresh: bool,
    ) -> Result<(), String> {
        if !details_demand(&self.routes.borrow()) {
            self.changes.reset();
        } else if refresh || !self.changes.initialized_for(broadcaster_id) {
            let channel = self
                .helix
                .get_channel_information(token, broadcaster_id)
                .await
                .map_err(|error| format!("Could not read current stream details: {error}"))?
                .ok_or("Twitch returned no current stream details")?;
            if channel.broadcaster_id != broadcaster_id {
                return Err("Twitch returned stream details for another broadcaster".into());
            }
            self.changes.observe(
                super::events::ChannelUpdated {
                    broadcaster_user_id: channel.broadcaster_id,
                    broadcaster_user_login: channel.broadcaster_login,
                    broadcaster_user_name: channel.broadcaster_name,
                    title: channel.title,
                    category_id: channel.game_id,
                    category_name: channel.game_name,
                    language: String::new(),
                },
                Instant::now(),
            );
        }
        Ok(())
    }

    async fn reconcile(
        &self,
        subscriptions: &mut BTreeMap<EventSubKind, String>,
        desired: &BTreeSet<EventSubKind>,
        broadcaster_id: &str,
        session_id: &str,
        access_token: &str,
        first_subscription_deadline: Option<Instant>,
    ) -> Result<Vec<SubscriptionFailure>, String> {
        let accounts = self.connected_accounts().await?;
        let configuration = super::configuration_request(&accounts, desired);
        let mut failures = Vec::new();
        for kind in desired {
            if *kind == EventSubKind::ChannelAdBreakBegin
                && let Some(request) = &configuration
            {
                failures.push(SubscriptionFailure::Configuration(request.clone()));
                continue;
            }
            if subscriptions.contains_key(kind) {
                continue;
            }
            let create = self.helix.create_eventsub_subscription(
                access_token,
                kind.event_type(),
                kind.version(),
                broadcaster_id,
                session_id,
            );
            let subscription = if let Some(deadline) = first_subscription_deadline
                && subscriptions.is_empty()
            {
                match timeout_at(deadline, create).await {
                    Ok(result) => result,
                    Err(_) => {
                        failures.push(SubscriptionFailure::Service(format!(
                            "{} subscription missed Twitch's welcome deadline",
                            kind.event_type()
                        )));
                        break;
                    }
                }
            } else {
                create.await
            };
            match subscription {
                Ok(subscription) => {
                    subscriptions.insert(*kind, subscription.id);
                }
                Err(error) => failures.push(SubscriptionFailure::Service(subscription_error(
                    *kind, error,
                ))),
            }
        }
        for kind in subscriptions.keys().copied().collect::<Vec<_>>() {
            if desired.contains(&kind) {
                continue;
            }
            let id = &subscriptions[&kind];
            self.helix
                .delete_eventsub_subscription(access_token, id)
                .await
                .map_err(|error| format!("{} unsubscription failed: {error}", kind.event_type()))?;
            subscriptions.remove(&kind);
        }
        if subscriptions.is_empty() {
            return Err(failures
                .iter()
                .map(SubscriptionFailure::message)
                .collect::<Vec<_>>()
                .join("; "));
        }
        Ok(failures)
    }

    async fn connected_accounts(&self) -> Result<ConnectedAccounts, String> {
        let accounts = Arc::clone(&self.accounts);
        tokio::task::spawn_blocking(move || accounts.connected())
            .await
            .map_err(|_| "Twitch account reader stopped".to_owned())?
            .map_err(|error| error.to_string())
    }

    async fn wait_for_change(&mut self, shutdown: &CancellationToken) -> bool {
        tokio::select! {
            _ = shutdown.cancelled() => false,
            changed = self.routes.changed() => changed.is_ok(),
        }
    }

    fn set_health(&self, health: TwitchHealth) {
        self.health
            .set(health)
            .expect("Twitch health lock poisoned");
    }

    fn set_subscription_health(&self, failures: Vec<SubscriptionFailure>) -> Option<String> {
        if failures.is_empty() {
            self.set_health(TwitchHealth::Connected);
            None
        } else {
            let message = failures
                .iter()
                .map(SubscriptionFailure::message)
                .collect::<Vec<_>>()
                .join("; ");
            let errors: Vec<_> = failures
                .iter()
                .filter_map(|failure| match failure {
                    SubscriptionFailure::Service(error) => Some(error.as_str()),
                    SubscriptionFailure::Configuration(_) => None,
                })
                .collect();
            let next = TwitchHealth::Degraded(message.clone());
            if !self.health.set(next).expect("Twitch health lock poisoned") {
                None
            } else {
                (!errors.is_empty())
                    .then(|| format!("Twitch events partially unavailable: {}", errors.join("; ")))
            }
        }
    }
}

fn details_demand(definitions: &[WorkflowDefinition]) -> bool {
    definitions.iter().filter(|definition| definition.enabled).any(|definition| definition.triggers.iter().any(|trigger| trigger.enabled && matches!(&trigger.kind, crate::workflows::TriggerKind::IntegrationEvent { integration, event, .. } if integration == "twitch" && event == super::events::DETAILS_CHANGED_EVENT)))
}

fn subscription_error(kind: EventSubKind, error: HelixError) -> String {
    format!("{} subscription failed: {error}", kind.event_type())
}

enum ConnectionExit {
    RoutesChanged,
    Lost(String),
}

#[derive(Default)]
struct RecentIds {
    seen: HashSet<String>,
    order: VecDeque<String>,
}

impl RecentIds {
    fn insert(&mut self, id: String) -> bool {
        if !self.seen.insert(id.clone()) {
            return false;
        }
        self.order.push_back(id);
        if self.order.len() > MAX_RECENT_IDS
            && let Some(oldest) = self.order.pop_front()
        {
            self.seen.remove(&oldest);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    use futures_util::SinkExt;
    use serde_json::json;
    use tokio::net::TcpListener;
    use tokio::sync::{Notify, mpsc, watch};
    use tokio_websockets::{Message, ServerBuilder};

    use super::*;
    use crate::engine::Workflow;
    use crate::migration::StoredConfig;
    use crate::storage::ConfigStore;
    use crate::twitch::credentials::{CredentialError, OAuthTokens, TwitchCredentialStore};
    use crate::twitch::device::{CLIENT_ID, DeviceAuth, DeviceError, FormResponse};
    use crate::twitch::helix::{HelixMethod, HelixRequest, HelixResponse};
    use crate::workflows::{TriggerDefinition, TriggerKind};

    const SLOT: &str = "e103d06e-e18c-4f54-b31e-8a29c16690b0";
    type TestAccounts = Arc<TwitchAccounts<Arc<MemoryCredentials>>>;
    type TestSession = Arc<TwitchSession<Arc<MemoryCredentials>, FakeAuth>>;

    #[derive(Default)]
    struct MemoryCredentials(Mutex<HashMap<String, String>>);

    impl CredentialBackend for Arc<MemoryCredentials> {
        fn get(&self, _: &str, account: &str) -> Result<Option<String>, CredentialError> {
            Ok(self.0.lock().unwrap().get(account).cloned())
        }

        fn set(&self, _: &str, account: &str, value: &str) -> Result<(), CredentialError> {
            self.0.lock().unwrap().insert(account.into(), value.into());
            Ok(())
        }

        fn delete(&self, _: &str, account: &str) -> Result<(), CredentialError> {
            self.0.lock().unwrap().remove(account);
            Ok(())
        }
    }

    struct FakeAuth;

    impl FormTransport for FakeAuth {
        async fn post_form(
            &self,
            _: &str,
            _: &[(&str, &str)],
        ) -> Result<FormResponse, DeviceError> {
            Err(DeviceError::Transport)
        }

        async fn get_bearer(&self, _: &str, _: &str) -> Result<FormResponse, DeviceError> {
            Ok(FormResponse {
                status: 200,
                body: json!({
                    "client_id": CLIENT_ID,
                    "user_id": "owner-1",
                    "login": "owner",
                    "scopes": ["channel:manage:broadcast", "channel:read:ads", "user:read:chat", "user:write:chat"],
                    "expires_in": 3600
                }).to_string().into_bytes(),
                retry_after: None,
            })
        }
    }

    #[derive(Default)]
    struct FakeHelix {
        requests: Mutex<Vec<String>>,
        subscribed: Notify,
        creates: AtomicUsize,
        deletes: AtomicUsize,
        deny_ads: AtomicBool,
    }

    impl HelixTransport for Arc<FakeHelix> {
        async fn send(&self, request: HelixRequest) -> Result<HelixResponse, HelixError> {
            assert_eq!(request.access_token(), "access");
            let url = request.url.to_string();
            self.requests.lock().unwrap().push(url);
            if request.method == HelixMethod::Delete {
                self.deletes.fetch_add(1, Ordering::SeqCst);
                return Ok(HelixResponse {
                    status: 204,
                    body: Vec::new(),
                    retry_after: None,
                });
            }
            if request.method == HelixMethod::Get {
                return Ok(HelixResponse { status: 200, body: json!({"data":[{"broadcaster_id":"owner-1", "broadcaster_login":"owner", "broadcaster_name":"Owner", "game_id":"1", "game_name":"Old game", "title":"Old title"}]}).to_string().into_bytes(), retry_after: None });
            }
            let index = self.creates.fetch_add(1, Ordering::SeqCst);
            self.subscribed.notify_one();
            if self.deny_ads.load(Ordering::SeqCst)
                && request.body().is_some_and(|body| {
                    body.windows(b"channel.ad_break.begin".len())
                        .any(|window| window == b"channel.ad_break.begin")
                })
            {
                return Ok(HelixResponse {
                    status: 403,
                    body: Vec::new(),
                    retry_after: None,
                });
            }
            Ok(HelixResponse {
                status: 202,
                body: json!({"data":[{"id":format!("sub-{index}")}]})
                    .to_string()
                    .into_bytes(),
                retry_after: None,
            })
        }
    }

    fn command_definition() -> WorkflowDefinition {
        WorkflowDefinition {
            enabled: true,
            workflow: Workflow {
                id: "flow".into(),
                revision: 1,
                overlap: false,
                steps: vec![],
                outputs: BTreeMap::new(),
            },
            triggers: vec![TriggerDefinition {
                id: "command".into(),
                enabled: true,
                kind: TriggerKind::IntegrationEvent {
                    integration: "twitch".into(),
                    event: "chat.command".into(),
                    filters: BTreeMap::from([("command".into(), json!("!go"))]),
                },
            }],
            name: None,
        }
    }

    fn fixture(root: &std::path::Path) -> (TestAccounts, TestSession) {
        fixture_with_scopes(root, true)
    }

    fn fixture_with_scopes(root: &std::path::Path, ads: bool) -> (TestAccounts, TestSession) {
        let mut scopes = vec![
            "channel:manage:broadcast",
            "user:read:chat",
            "user:write:chat",
        ];
        if ads {
            scopes.push("channel:read:ads");
        }
        let credentials = Arc::new(MemoryCredentials::default());
        TwitchCredentialStore::new(Arc::clone(&credentials), CLIENT_ID)
            .replace(
                TwitchRole::Broadcaster,
                SLOT,
                &OAuthTokens::new("access".into(), "refresh".into()),
            )
            .unwrap();
        let store = ConfigStore::new(root);
        store
            .create(
                "twitch-accounts",
                &StoredConfig {
                    definition: "snenkbot.twitch.accounts".into(),
                    version: 1,
                    data: json!({
                        "broadcaster": {
                            "user_id": "owner-1",
                            "login": "owner",
                            "scopes": scopes,
                            "credential_slot": SLOT
                        },
                        "bot": null
                    }),
                },
                |_| Ok(()),
            )
            .unwrap();
        let accounts = Arc::new(TwitchAccounts::new(
            store,
            TwitchCredentialStore::new(credentials, CLIENT_ID),
        ));
        let session = Arc::new(TwitchSession::new(
            Arc::clone(&accounts),
            DeviceAuth::new(FakeAuth),
        ));
        (accounts, session)
    }

    fn notification(id: &str, text: &str) -> String {
        json!({
            "metadata": {"message_type":"notification","message_id":id},
            "payload": {
                "subscription": {"type":"channel.chat.message","version":"1"},
                "event": {
                    "broadcaster_user_id":"owner-1",
                    "broadcaster_user_login":"owner",
                    "broadcaster_user_name":"Owner",
                    "chatter_user_id":"viewer-1",
                    "chatter_user_login":"viewer",
                    "chatter_user_name":"Viewer",
                    "message_id":id,
                    "message":{"text":text,"fragments":[]},
                    "badges":[],
                    "message_type":"text"
                }
            }
        })
        .to_string()
    }

    #[tokio::test]
    async fn missing_ad_permission_is_actionable_without_failing_chat_or_notifying() {
        let root = tempfile::tempdir().unwrap();
        let (accounts, session) = fixture_with_scopes(root.path(), false);
        let required = BTreeSet::from([
            EventSubKind::ChannelChatMessage,
            EventSubKind::ChannelAdBreakBegin,
        ]);
        let connected = accounts.connected().unwrap();
        let request = super::super::configuration_request(&connected, &required).unwrap();
        assert_eq!(request.connection, "broadcaster");
        assert_eq!(request.affected_features, ["Ad triggers"]);
        assert!(
            super::super::configuration_request(
                &connected,
                &BTreeSet::from([EventSubKind::ChannelChatMessage])
            )
            .is_none()
        );
        let mut granted = connected.clone();
        granted
            .broadcaster
            .as_mut()
            .unwrap()
            .scopes
            .push("channel:read:ads".into());
        assert!(super::super::configuration_request(&granted, &required).is_none());
        assert!(
            super::super::configuration_request(&ConnectedAccounts::default(), &required).is_none()
        );
        let helix = Arc::new(FakeHelix::default());
        let (_sender, routes) = watch::channel(vec![]);
        let health = Arc::new(StatusCell::new(TwitchHealth::Disabled));
        let mut status_changes = health.subscribe();
        let source = TwitchListener::new(
            accounts,
            session,
            Arc::new(Helix::new(Arc::clone(&helix))),
            Arc::new(ChatEchoes::default()),
            routes,
            health.clone(),
        );
        let mut subscriptions = BTreeMap::new();
        let failures = source
            .reconcile(
                &mut subscriptions,
                &required,
                "owner-1",
                "socket-1",
                "access",
                None,
            )
            .await
            .unwrap();
        assert!(subscriptions.contains_key(&EventSubKind::ChannelChatMessage));
        assert!(!subscriptions.contains_key(&EventSubKind::ChannelAdBreakBegin));
        assert_eq!(helix.creates.load(Ordering::SeqCst), 1);
        assert!(source.set_subscription_health(failures).is_none());
        assert!(status_changes.has_changed().unwrap());
        status_changes.borrow_and_update();
        let unchanged = health.read().unwrap().clone();
        source.set_health(unchanged);
        assert!(!status_changes.has_changed().unwrap());
        assert!(matches!(*health.read().unwrap(), TwitchHealth::Degraded(_)));
    }

    #[test]
    fn notification_ids_are_deduplicated_with_bounded_memory() {
        let mut ids = RecentIds::default();
        assert!(ids.insert("first".into()));
        assert!(!ids.insert("first".into()));
        for index in 0..super::MAX_RECENT_IDS {
            assert!(ids.insert(format!("id-{index}")));
        }
        assert!(ids.insert("first".into()));
    }

    #[tokio::test]
    async fn route_changes_reuse_the_socket_and_reconcile_subscription_types() {
        let root = tempfile::tempdir().unwrap();
        let (accounts, session) = fixture(root.path());
        let helix = Arc::new(FakeHelix::default());
        let (_routes, receiver) = watch::channel(Vec::new());
        let source = TwitchListener::new(
            accounts,
            session,
            Arc::new(Helix::new(Arc::clone(&helix))),
            Arc::new(ChatEchoes::default()),
            receiver,
            Arc::new(StatusCell::new(TwitchHealth::Disabled)),
        );
        let mut subscriptions = BTreeMap::new();
        let chat = BTreeSet::from([EventSubKind::ChannelChatMessage]);
        assert!(
            source
                .reconcile(
                    &mut subscriptions,
                    &chat,
                    "owner-1",
                    "socket-1",
                    "access",
                    None
                )
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(subscriptions.len(), 1);
        let both = BTreeSet::from([
            EventSubKind::ChannelChatMessage,
            EventSubKind::ChannelChatNotification,
        ]);
        assert!(
            source
                .reconcile(
                    &mut subscriptions,
                    &both,
                    "owner-1",
                    "socket-1",
                    "access",
                    None
                )
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(helix.creates.load(Ordering::SeqCst), 2);
        let notice = BTreeSet::from([EventSubKind::ChannelChatNotification]);
        assert!(
            source
                .reconcile(
                    &mut subscriptions,
                    &notice,
                    "owner-1",
                    "socket-1",
                    "access",
                    None
                )
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(subscriptions.len(), 1);
        assert_eq!(helix.deletes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn demand_opens_one_socket_and_routes_each_notification_once() {
        let root = tempfile::tempdir().unwrap();
        let (accounts, session) = fixture(root.path());
        let helix = Arc::new(FakeHelix::default());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let subscribed = Arc::clone(&helix);
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let (_, mut socket) = ServerBuilder::new().accept(tcp).await.unwrap();
            socket
                .send(Message::text(
                    json!({
                        "metadata":{"message_type":"session_welcome"},
                        "payload":{"session":{"id":"socket-1","keepalive_timeout_seconds":10}}
                    })
                    .to_string(),
                ))
                .await
                .unwrap();
            subscribed.subscribed.notified().await;
            socket
                .send(Message::text(notification("own-event", "!go ignore")))
                .await
                .unwrap();
            let event = notification("event-1", "!go payload");
            socket.send(Message::text(event.clone())).await.unwrap();
            socket.send(Message::text(event)).await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        });
        let (routes, receiver) = watch::channel(vec![command_definition()]);
        let health = Arc::new(StatusCell::new(TwitchHealth::Disabled));
        let echoes = Arc::new(ChatEchoes::default());
        echoes.record("own-event".into());
        let mut source = TwitchListener::new(
            accounts,
            session,
            Arc::new(Helix::new(Arc::clone(&helix))),
            echoes,
            receiver,
            Arc::clone(&health),
        );
        source.endpoint = endpoint;
        let shutdown = CancellationToken::new();
        let (sender, mut activations) = mpsc::unbounded_channel();
        let worker = tokio::spawn({
            let shutdown = shutdown.clone();
            async move {
                source
                    .run(
                        shutdown,
                        move |activation| {
                            sender.send(activation).unwrap();
                        },
                        |_| {},
                    )
                    .await;
            }
        });
        let activation = tokio::time::timeout(Duration::from_secs(3), activations.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(activation.workflow_id, "flow");
        assert_eq!(activation.trigger_id, "command");
        assert_eq!(activation.values["arguments"], "payload");
        assert!(
            tokio::time::timeout(Duration::from_millis(150), activations.recv())
                .await
                .is_err()
        );
        assert_eq!(helix.creates.load(Ordering::SeqCst), 1);
        routes.send_replace(Vec::new());
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if *health.read().unwrap() == TwitchHealth::Disabled {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        shutdown.cancel();
        worker.await.unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn external_title_and_game_updates_emit_one_settled_activation() {
        let root = tempfile::tempdir().unwrap();
        let (accounts, session) = fixture(root.path());
        let helix = Arc::new(FakeHelix::default());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let subscribed = Arc::clone(&helix);
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let (_, mut socket) = ServerBuilder::new().accept(tcp).await.unwrap();
            socket.send(Message::text(json!({"metadata":{"message_type":"session_welcome"},"payload":{"session":{"id":"socket-1","keepalive_timeout_seconds":10}}}).to_string())).await.unwrap();
            subscribed.subscribed.notified().await;
            for (index, title, game, language) in [
                (0, "Old title", "1", "de"),
                (1, "Old title", "2", "de"),
                (2, "New title", "2", "de"),
                (3, "New title", "2", "en"),
            ] {
                socket.send(Message::text(json!({"metadata":{"message_type":"notification","message_id":format!("update-{index}")},"payload":{"subscription":{"type":"channel.update","version":"2"},"event":{"broadcaster_user_id":"owner-1","broadcaster_user_login":"owner","broadcaster_user_name":"Owner","title":title,"category_id":game,"category_name":if game == "1" {"Old game"} else {"New game"},"language":language}}}).to_string())).await.unwrap();
            }
            tokio::time::sleep(Duration::from_secs(4)).await;
        });
        let mut definition = command_definition();
        definition.triggers[0].kind = TriggerKind::IntegrationEvent {
            integration: "twitch".into(),
            event: super::super::events::DETAILS_CHANGED_EVENT.into(),
            filters: BTreeMap::new(),
        };
        let (_routes, receiver) = watch::channel(vec![definition]);
        let mut source = TwitchListener::new(
            accounts,
            session,
            Arc::new(Helix::new(helix)),
            Arc::new(ChatEchoes::default()),
            receiver,
            Arc::new(StatusCell::new(TwitchHealth::Disabled)),
        );
        source.endpoint = endpoint;
        let shutdown = CancellationToken::new();
        let (sender, mut activations) = mpsc::unbounded_channel();
        let worker_shutdown = shutdown.clone();
        let worker = tokio::spawn(async move {
            source
                .run(
                    worker_shutdown,
                    move |activation| {
                        sender.send(activation).unwrap();
                    },
                    |error| panic!("unexpected listener error: {error}"),
                )
                .await;
        });
        let activation = tokio::time::timeout(Duration::from_secs(3), activations.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(activation.values["title"], "New title");
        assert_eq!(activation.values["category_name"], "New game");
        assert_eq!(activation.values["event"], "channel.details_changed");
        assert!(
            tokio::time::timeout(Duration::from_millis(100), activations.recv())
                .await
                .is_err()
        );
        shutdown.cancel();
        worker.await.unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn denied_ad_subscription_keeps_chat_command_live() {
        let root = tempfile::tempdir().unwrap();
        let (accounts, session) = fixture(root.path());
        let helix = Arc::new(FakeHelix::default());
        helix.deny_ads.store(true, Ordering::SeqCst);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let subscribed = Arc::clone(&helix);
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let (_, mut socket) = ServerBuilder::new().accept(tcp).await.unwrap();
            socket
                .send(Message::text(
                    json!({
                        "metadata":{"message_type":"session_welcome"},
                        "payload":{"session":{"id":"socket-1","keepalive_timeout_seconds":10}}
                    })
                    .to_string(),
                ))
                .await
                .unwrap();
            subscribed.subscribed.notified().await;
            socket
                .send(Message::text(notification("event-1", "!go still works")))
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(300)).await;
        });
        let mut definition = command_definition();
        definition.triggers.push(TriggerDefinition {
            id: "ad".into(),
            enabled: true,
            kind: TriggerKind::IntegrationEvent {
                integration: "twitch".into(),
                event: "ad_break.begin".into(),
                filters: BTreeMap::new(),
            },
        });
        let (_routes, receiver) = watch::channel(vec![definition]);
        let health = Arc::new(StatusCell::new(TwitchHealth::Disabled));
        let mut source = TwitchListener::new(
            accounts,
            session,
            Arc::new(Helix::new(Arc::clone(&helix))),
            Arc::new(ChatEchoes::default()),
            receiver,
            Arc::clone(&health),
        );
        source.endpoint = endpoint;
        let shutdown = CancellationToken::new();
        let (sender, mut activations) = mpsc::unbounded_channel();
        let (error_sender, mut errors) = mpsc::unbounded_channel();
        let worker = tokio::spawn({
            let shutdown = shutdown.clone();
            async move {
                source
                    .run(
                        shutdown,
                        move |activation| {
                            sender.send(activation).unwrap();
                        },
                        move |error| {
                            error_sender.send(error).unwrap();
                        },
                    )
                    .await;
            }
        });
        let activation = tokio::time::timeout(Duration::from_secs(3), activations.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(activation.values["arguments"], "still works");
        let error = tokio::time::timeout(Duration::from_secs(3), errors.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(error.contains("channel.ad_break.begin subscription failed"));
        assert!(matches!(
            &*health.read().unwrap(),
            TwitchHealth::Degraded(_)
        ));
        shutdown.cancel();
        worker.await.unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn server_reconnect_keeps_old_events_and_reconciles_new_demand() {
        let root = tempfile::tempdir().unwrap();
        let (accounts, session) = fixture(root.path());
        let helix = Arc::new(FakeHelix::default());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let reconnect_url = endpoint.clone();
        let subscribed = Arc::clone(&helix);
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let (_, mut old) = ServerBuilder::new().accept(tcp).await.unwrap();
            old.send(Message::text(
                json!({
                    "metadata":{"message_type":"session_welcome"},
                    "payload":{"session":{"id":"socket-1","keepalive_timeout_seconds":10}}
                })
                .to_string(),
            ))
            .await
            .unwrap();
            subscribed.subscribed.notified().await;
            old.send(Message::text(
                json!({
                    "metadata":{"message_type":"session_reconnect"},
                    "payload":{"session":{"reconnect_url":reconnect_url}}
                })
                .to_string(),
            ))
            .await
            .unwrap();
            old.send(Message::text(notification("old-event", "!go old")))
                .await
                .unwrap();
            let (tcp, _) = listener.accept().await.unwrap();
            let (_, mut next) = ServerBuilder::new().accept(tcp).await.unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
            next.send(Message::text(
                json!({
                    "metadata":{"message_type":"session_welcome"},
                    "payload":{"session":{"id":"socket-2","keepalive_timeout_seconds":10}}
                })
                .to_string(),
            ))
            .await
            .unwrap();
            next.send(Message::text(notification("old-event", "!go old")))
                .await
                .unwrap();
            next.send(Message::text(notification("new-event", "!go new")))
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(250)).await;
        });
        let (routes, receiver) = watch::channel(vec![command_definition()]);
        let health = Arc::new(StatusCell::new(TwitchHealth::Disabled));
        let mut source = TwitchListener::new(
            accounts,
            session,
            Arc::new(Helix::new(Arc::clone(&helix))),
            Arc::new(ChatEchoes::default()),
            receiver,
            health,
        );
        source.endpoint = endpoint;
        let shutdown = CancellationToken::new();
        let (sender, mut activations) = mpsc::unbounded_channel();
        let worker = tokio::spawn({
            let shutdown = shutdown.clone();
            async move {
                source
                    .run(
                        shutdown,
                        move |activation| {
                            sender.send(activation).unwrap();
                        },
                        |_| {},
                    )
                    .await;
            }
        });
        let first = tokio::time::timeout(Duration::from_secs(3), activations.recv())
            .await
            .unwrap()
            .unwrap();
        let mut updated = command_definition();
        updated.triggers.push(TriggerDefinition {
            id: "notice".into(),
            enabled: true,
            kind: TriggerKind::IntegrationEvent {
                integration: "twitch".into(),
                event: "chat.notification".into(),
                filters: BTreeMap::new(),
            },
        });
        routes.send_replace(vec![updated]);
        let second = tokio::time::timeout(Duration::from_secs(3), activations.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.values["arguments"], "old");
        assert_eq!(second.values["arguments"], "new");
        assert!(
            tokio::time::timeout(Duration::from_millis(100), activations.recv())
                .await
                .is_err()
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            while helix.creates.load(Ordering::SeqCst) != 2 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        shutdown.cancel();
        worker.await.unwrap();
        server.await.unwrap();
    }
}
