//! Native workflow and Lua actions for transient, consumer-targeted API events.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::ConfigSchema;
use crate::choices::{ChoiceCatalog, ChoiceFuture, ChoiceProvider, ConfigChoice};
use crate::engine::{
    Capability, CapabilityError, Engine, Input, Values, validate_configured_inputs,
};
use crate::integration::ConnectionState;
use crate::schema::{ActionDefinition, ConfigSchema as Schema, DescribeConfig};

use super::{ApiError, ApiService};

const EVENT_LIMIT: usize = 256 * 1024;

/// Chat text and viewer identity rendered by selected API consumers.
#[derive(Deserialize, Serialize, ConfigSchema)]
#[serde(deny_unknown_fields)]
#[config(
    id = "api.send_chat",
    version = 1,
    title = "Send synthetic chat message"
)]
struct ChatInputs {
    #[config(
        id = "consumers",
        label = "Consumers",
        choice = "api.consumers",
        introduced = 1
    )]
    consumers: BTreeSet<String>,
    #[config(id = "user_id", label = "User ID", introduced = 1)]
    user_id: String,
    #[config(id = "login", label = "Login", introduced = 1)]
    login: String,
    #[config(id = "user", label = "Display name", introduced = 1)]
    user: String,
    #[config(
        id = "color",
        label = "Name color",
        description = "A hexadecimal color such as #fffd00.",
        introduced = 1
    )]
    color: String,
    #[config(id = "message", label = "Message", introduced = 1)]
    message: String,
}

/// Normalized support metadata for a synthetic subscription, gift or cheer.
#[derive(Deserialize, Serialize, ConfigSchema)]
#[serde(deny_unknown_fields)]
#[config(
    id = "api.send_support",
    version = 1,
    title = "Send synthetic support event"
)]
struct SupportInputs {
    #[config(
        id = "consumers",
        label = "Consumers",
        choice = "api.consumers",
        introduced = 1
    )]
    consumers: BTreeSet<String>,
    #[config(
        id = "kind",
        label = "Support kind",
        description = "Choose a subscription, resubscription, gift or cheer.",
        choice = "api.support_kinds",
        introduced = 1
    )]
    kind: String,
    #[config(id = "channel_id", label = "Broadcaster ID", introduced = 1)]
    channel_id: String,
    #[config(id = "user", label = "Display name", introduced = 1)]
    user: String,
    #[config(
        id = "tier",
        label = "Subscription tier",
        description = "For example 1000, 2000 or 3000.",
        introduced = 1
    )]
    tier: Option<String>,
    #[config(
        id = "amount",
        label = "Amount",
        description = "Bits, gifted subscriptions or cumulative months, depending on kind.",
        introduced = 1
    )]
    amount: Option<u64>,
    #[config(id = "message", label = "Message", introduced = 1)]
    message: Option<String>,
}

/// Category identity sent only to selected consumers without changing the live stream.
#[derive(Deserialize, Serialize, ConfigSchema)]
#[serde(deny_unknown_fields)]
#[config(
    id = "api.send_game",
    version = 1,
    title = "Send synthetic game change"
)]
struct GameInputs {
    #[config(
        id = "consumers",
        label = "Consumers",
        choice = "api.consumers",
        introduced = 1
    )]
    consumers: BTreeSet<String>,
    #[config(id = "category_id", label = "Category ID", introduced = 1)]
    category_id: String,
    #[config(id = "name", label = "Game name", introduced = 1)]
    name: String,
    #[config(
        id = "broadcaster_id",
        label = "Broadcaster ID",
        description = "Leave empty to keep the current live broadcaster identity.",
        introduced = 1
    )]
    broadcaster_id: Option<String>,
}

/// An application-defined event name and JSON payload on the fixed custom channel.
#[derive(Deserialize, Serialize, ConfigSchema)]
#[serde(deny_unknown_fields)]
#[config(id = "api.send_custom", version = 1, title = "Send custom API event")]
struct CustomInputs {
    #[config(
        id = "consumers",
        label = "Consumers",
        choice = "api.consumers",
        introduced = 1
    )]
    consumers: BTreeSet<String>,
    #[config(id = "name", label = "Event name", introduced = 1)]
    name: String,
    #[config(id = "payload", label = "JSON payload", introduced = 1)]
    payload: serde_json::Value,
}

impl Default for ChatInputs {
    /// Supplies a complete synthetic viewer and message for immediate manual testing.
    fn default() -> Self {
        Self {
            consumers: BTreeSet::new(),
            user_id: "synthetic-user".into(),
            login: "testviewer".into(),
            user: "Test Viewer".into(),
            color: "#fffd00".into(),
            message: "Hello from SnenkBot!".into(),
        }
    }
}

impl Default for SupportInputs {
    /// Provides synthetic support metadata without requiring an actual Twitch identity.
    fn default() -> Self {
        Self {
            consumers: BTreeSet::new(),
            kind: "sub".into(),
            channel_id: "0".into(),
            user: "Test Viewer".into(),
            tier: Some("1000".into()),
            amount: Some(1),
            message: Some("Thanks for the stream!".into()),
        }
    }
}

impl Default for GameInputs {
    /// Provides a synthetic category without changing or looking up a live Twitch channel.
    fn default() -> Self {
        Self {
            consumers: BTreeSet::new(),
            category_id: "0".into(),
            name: "Test game".into(),
            broadcaster_id: None,
        }
    }
}

impl Default for CustomInputs {
    /// Starts custom delivery with a named event and editable JSON object.
    fn default() -> Self {
        Self {
            consumers: BTreeSet::new(),
            name: "test".into(),
            payload: json!({}),
        }
    }
}

/// Identifies one compiled API event adapter and its input schema.
#[derive(Clone, Copy)]
enum Action {
    Chat,
    Support,
    Game,
    Custom,
}

impl Action {
    /// Returns the macro-derived inputs used by validation and the workflow editor.
    fn schema(self) -> &'static Schema {
        match self {
            Self::Chat => ChatInputs::SCHEMA,
            Self::Support => SupportInputs::SCHEMA,
            Self::Game => GameInputs::SCHEMA,
            Self::Custom => CustomInputs::SCHEMA,
        }
    }
}

/// Registers API-owned event actions and current named-consumer choices at startup.
pub(super) fn register(engine: &mut Engine, choices: &mut ChoiceCatalog, service: Arc<ApiService>) {
    choices.register(
        "api.support_kinds",
        Arc::clone(&service) as Arc<dyn ChoiceProvider>,
    );
    choices.register(
        "api.consumers",
        Arc::clone(&service) as Arc<dyn ChoiceProvider>,
    );
    for (definition, kind, channel) in [
        (
            ActionDefinition::of::<ChatInputs>(),
            Action::Chat,
            "twitch.chat",
        ),
        (
            ActionDefinition::of::<SupportInputs>(),
            Action::Support,
            "twitch.support",
        ),
        (
            ActionDefinition::of::<GameInputs>(),
            Action::Game,
            "twitch.channel",
        ),
        (
            ActionDefinition::of::<CustomInputs>(),
            Action::Custom,
            "api.custom",
        ),
    ] {
        engine.register_event_test_action(
            definition,
            "consumers",
            channel,
            Arc::new(ApiAction {
                kind,
                service: Arc::clone(&service),
            }),
        );
    }
}

impl ChoiceProvider for ApiService {
    /// Resolves mutable display names to stable consumer UUIDs without reading tokens.
    fn choices<'a>(&'a self, source: &'a str, _depends_on: Option<&'a str>) -> ChoiceFuture<'a> {
        Box::pin(async move {
            if source == "api.support_kinds" {
                return Ok([
                    ("sub", "Subscription"),
                    ("resub", "Resubscription"),
                    ("gift", "Gift subscriptions"),
                    ("bits", "Bits cheer"),
                ]
                .into_iter()
                .map(|(value, label)| ConfigChoice {
                    value: value.into(),
                    label: label.into(),
                    detail: None,
                })
                .collect());
            }
            if source != "api.consumers" {
                return Err(format!("unknown API choice source `{source}`"));
            }
            self.list_consumers()
                .map(|consumers| {
                    consumers
                        .into_iter()
                        .map(|consumer| ConfigChoice {
                            value: consumer.id,
                            label: consumer.name,
                            detail: None,
                        })
                        .collect()
                })
                .map_err(|error| error.to_string())
        })
    }
}

impl ApiService {
    /// Requires an enabled, successfully bound API listener before sending native events.
    pub fn event_delivery_ready(&self) -> Result<(), ApiError> {
        if !self.configuration()?.enabled {
            return Err(ApiError::Invalid("API server is disabled".into()));
        }
        let status = self.status.read().expect("API status lock poisoned");
        if status.state != ConnectionState::Connected {
            return Err(ApiError::Invalid(format!(
                "API server is unavailable: {}",
                status.detail
            )));
        }
        Ok(())
    }

    /// Validates the complete recipient grant before emitting transient native API data.
    pub fn send_event_to_consumers(
        &self,
        channel: &str,
        consumers: &BTreeSet<String>,
        data: Value,
        cancel: &CancellationToken,
    ) -> Result<(), ApiError> {
        let _guard = self
            .configuration_lock
            .lock()
            .expect("API configuration lock poisoned");
        self.event_delivery_ready()?;
        if !super::channel_ids().contains(&channel) {
            return Err(ApiError::Invalid("unknown API event channel".into()));
        }
        if consumers.is_empty() || consumers.len() > 64 {
            return Err(ApiError::Invalid(
                "select between 1 and 64 consumers".into(),
            ));
        }
        if serde_json::to_vec(&data)
            .map_err(|_| ApiError::InvalidFormat)?
            .len()
            > EVENT_LIMIT
        {
            return Err(ApiError::Invalid("API event exceeds 256 KiB".into()));
        }
        let current = self.list_consumers()?;
        for id in consumers {
            let consumer = current
                .iter()
                .find(|consumer| &consumer.id == id)
                .ok_or_else(|| {
                    ApiError::Invalid("selected consumer is no longer available".into())
                })?;
            if !consumer.permissions.channels.contains(channel) {
                return Err(ApiError::Invalid(format!(
                    "consumer `{}` cannot receive `{channel}`",
                    consumer.name
                )));
            }
        }
        if cancel.is_cancelled() {
            return Err(ApiError::Invalid("action cancelled".into()));
        }
        self.events.publish_targeted(channel, data, consumers)
    }
}

/// Adapts a typed synthetic event to the ordinary workflow capability contract.
struct ApiAction {
    kind: Action,
    service: Arc<ApiService>,
}

impl Capability for ApiAction {
    /// Rejects mismatched literal field shapes while preserving dynamic workflow inputs.
    fn validate_inputs(&self, inputs: &BTreeMap<String, Input>) -> Result<(), String> {
        validate_configured_inputs(self.kind.schema(), inputs)
    }

    /// Reports disabled or unavailable API service before evaluating workflow inputs.
    fn ready(&self) -> Result<(), String> {
        self.service
            .event_delivery_ready()
            .map_err(|error| error.to_string())
    }

    /// Bounds the local delivery step using the ordinary engine deadline.
    fn default_deadline(&self) -> Duration {
        Duration::from_secs(5)
    }

    /// Builds the stable overlay payload and delivers it without incoming-event dispatch.
    fn execute<'a>(
        &'a self,
        inputs: Values,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<Values, CapabilityError>> + Send + 'a>> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(CapabilityError::Failed("action cancelled".into()));
            }
            let (channel, consumers, data) = match self.kind {
                Action::Chat => {
                    let config: ChatInputs = parse(inputs)?;
                    if config.color.len() != 7
                        || !config.color.starts_with('#')
                        || !config.color.as_bytes()[1..]
                            .iter()
                            .all(u8::is_ascii_hexdigit)
                    {
                        return Err(CapabilityError::Failed(
                            "name color must use #RRGGBB".into(),
                        ));
                    }
                    (
                        "twitch.chat",
                        config.consumers,
                        json!({"id": event_id(), "userId": config.user_id,
                        "login": config.login, "user": config.user, "color": config.color,
                        "parts": [{"text": config.message}]}),
                    )
                }
                Action::Support => {
                    let config: SupportInputs = parse(inputs)?;
                    if !["sub", "resub", "gift", "bits"].contains(&config.kind.as_str()) {
                        return Err(CapabilityError::Failed(
                            "support kind must be sub, resub, gift or bits".into(),
                        ));
                    }
                    let timestamp: chrono::DateTime<chrono::Utc> = SystemTime::now().into();
                    let mut data = json!({"id": event_id(), "channelId": config.channel_id,
                        "kind": config.kind, "user": config.user, "timestamp": timestamp.to_rfc3339()});
                    if let Some(tier) = config.tier {
                        data["tier"] = json!(tier);
                    }
                    if let Some(amount) = config.amount {
                        data["amount"] = json!(amount);
                    }
                    if let Some(message) = config.message {
                        data["message"] = json!(message);
                    }
                    ("twitch.support", config.consumers, data)
                }
                Action::Game => {
                    let config: GameInputs = parse(inputs)?;
                    let mut data = json!({"id": config.category_id, "name": config.name});
                    if let Some(broadcaster_id) = config
                        .broadcaster_id
                        .or_else(|| self.service.events.broadcaster_id())
                    {
                        data["broadcasterId"] = json!(broadcaster_id);
                    }
                    ("twitch.channel", config.consumers, data)
                }
                Action::Custom => {
                    let config: CustomInputs = parse(inputs)?;
                    if config.name.trim().is_empty()
                        || config.name.len() > 100
                        || config.name.chars().any(char::is_control)
                    {
                        return Err(CapabilityError::Failed("event name must contain 1 to 100 characters without control characters".into()));
                    }
                    (
                        "api.custom",
                        config.consumers,
                        json!({"id": event_id(), "name": config.name, "payload": config.payload}),
                    )
                }
            };
            self.service
                .send_event_to_consumers(channel, &consumers, data, &cancel)
                .map_err(|error| CapabilityError::Failed(error.to_string()))?;
            Ok(Values::new())
        })
    }
}

/// Deserializes the engine's resolved values into a native event input type.
fn parse<T: serde::de::DeserializeOwned>(inputs: Values) -> Result<T, CapabilityError> {
    serde_json::from_value(json!(inputs))
        .map_err(|error| CapabilityError::Failed(format!("invalid API event inputs: {error}")))
}

/// Generates a collision-resistant identity distinct from received Twitch event IDs.
fn event_id() -> String {
    format!("synthetic-{}", Uuid::new_v4().simple())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{
        FormValidators, InputField, InputProvider, InputResponse, Outcome, Step, StepKind, Workflow,
    };
    use crate::paths::AppPaths;
    use crate::providers::ProviderIntegration;

    /// Rejects forms because native event actions do not request interactive input.
    struct NoInput;

    impl InputProvider for NoInput {
        /// Rejects unexpected interactive form requests during headless execution.
        fn request<'a>(
            &'a self,
            _title: String,
            _fields: Vec<InputField>,
            _defaults: Values,
            _validators: &'a FormValidators,
            _cancel: CancellationToken,
        ) -> Pin<Box<dyn Future<Output = Result<InputResponse, String>> + Send + 'a>> {
            Box::pin(async { Err("input unavailable".into()) })
        }
    }

    /// Constructs an isolated API service without binding a listener or accessing browser UI.
    fn service(root: &std::path::Path) -> (Arc<ApiService>, Engine) {
        let paths = AppPaths {
            config: root.join("config"),
            data: root.join("data"),
            state: root.join("state"),
        };
        let mut engine = Engine::new(Arc::new(NoInput), Arc::new(|_| {}));
        let providers = ProviderIntegration::register(&paths, &mut engine).unwrap();
        (ApiService::new(&paths, providers), engine)
    }

    /// Registered test senders retain their defaults and deliver only to the selected output.
    #[tokio::test]
    async fn direct_test_catalog_reuses_registered_defaults_and_targeted_delivery() {
        let temporary = tempfile::tempdir().unwrap();
        let (service, mut engine) = service(temporary.path());
        enable(&service);
        let alice = consumer(&service, "Alice", "twitch.chat");
        let bob = consumer(&service, "Bob", "twitch.chat");
        let channels = BTreeSet::from(["twitch.chat".into()]);
        let mut alice_stream =
            service
                .events
                .subscribe_for_consumer(Some(&alice), channels.clone(), BTreeMap::new());
        let mut bob_stream =
            service
                .events
                .subscribe_for_consumer(Some(&bob), channels, BTreeMap::new());
        register(
            &mut engine,
            &mut ChoiceCatalog::default(),
            Arc::clone(&service),
        );
        let catalog = engine.event_tests();
        assert_eq!(catalog.senders().len(), 4);
        let sender = catalog
            .senders()
            .into_iter()
            .find(|sender| sender.definition.schema.id == "api.send_chat")
            .unwrap();
        let mut values = sender.definition.defaults;
        values.insert(sender.target_field, json!([alice]));
        catalog.send("api.send_chat", 1, values).await.unwrap();
        let frame = alice_stream.receiver.try_recv().unwrap();
        assert_eq!(frame.channel, "twitch.chat");
        assert_eq!(frame.data["parts"][0]["text"], "Hello from SnenkBot!");
        let private_frame = bob_stream.receiver.try_recv().unwrap();
        assert!(frame.visible_to(&alice));
        assert!(!private_frame.visible_to(&bob));
    }

    /// Marks the test listener as bound without creating any actual network service.
    fn enable(service: &ApiService) {
        let expected = service.configuration().unwrap();
        let mut settings = expected.clone();
        settings.enabled = true;
        service.save_configuration(expected, settings).unwrap();
        service
            .status
            .set(super::super::api_status(
                ConnectionState::Connected,
                "Test listener",
            ))
            .unwrap();
    }

    /// Creates an isolated current grant for one channel.
    fn consumer(service: &ApiService, name: &str, channel: &str) -> String {
        service
            .create_consumer(
                name.into(),
                super::super::ConsumerPermissions {
                    channels: BTreeSet::from([channel.into()]),
                    ..Default::default()
                },
                BTreeSet::new(),
                None,
            )
            .unwrap()
            .id
    }

    /// Invalid or revoked targets reject the entire selection before any stream receives data.
    #[test]
    fn all_targets_are_validated_before_delivery() {
        let temporary = tempfile::tempdir().unwrap();
        let (service, _) = service(temporary.path());
        enable(&service);
        let alice = consumer(&service, "Alice", "twitch.chat");
        let bob = consumer(&service, "Bob", "twitch.support");
        let mut stream = service.events.subscribe_for_consumer(
            Some(&alice),
            BTreeSet::from(["twitch.chat".into()]),
            BTreeMap::new(),
        );
        let recipients = BTreeSet::from([alice.clone(), bob.clone()]);
        assert!(
            service
                .send_event_to_consumers(
                    "twitch.chat",
                    &BTreeSet::new(),
                    json!({}),
                    &CancellationToken::new(),
                )
                .is_err()
        );
        assert!(
            service
                .send_event_to_consumers(
                    "twitch.chat",
                    &BTreeSet::from([alice.clone()]),
                    json!({"message": "x".repeat(EVENT_LIMIT)}),
                    &CancellationToken::new(),
                )
                .is_err()
        );
        assert!(
            service
                .send_event_to_consumers(
                    "twitch.chat",
                    &recipients,
                    json!({"id":"test"}),
                    &CancellationToken::new()
                )
                .is_err()
        );
        assert!(stream.receiver.try_recv().is_err());
        service.revoke_consumer(&bob).unwrap();
        assert!(
            service
                .send_event_to_consumers(
                    "twitch.chat",
                    &recipients,
                    json!({"id":"test"}),
                    &CancellationToken::new()
                )
                .is_err()
        );
        assert!(stream.receiver.try_recv().is_err());
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(
            service
                .send_event_to_consumers(
                    "twitch.chat",
                    &BTreeSet::from([alice]),
                    json!({}),
                    &cancelled
                )
                .is_err()
        );
        assert!(stream.receiver.try_recv().is_err());
    }

    /// Registration survives disabled listeners while execution readiness reports the missing service.
    #[test]
    fn disabled_api_still_registers_actions_and_consumer_choices() {
        let temporary = tempfile::tempdir().unwrap();
        let (service, mut engine) = service(temporary.path());
        let mut choices = ChoiceCatalog::default();
        register(&mut engine, &mut choices, Arc::clone(&service));
        choices.validate_schemas(&engine.action_schemas()).unwrap();
        assert!(
            engine
                .action_schemas()
                .iter()
                .any(|schema| schema.id == "api.send_custom")
        );
        assert!(service.event_delivery_ready().is_err());
    }

    /// Built-in adapters retain the same field shapes as normalized live overlay events.
    #[tokio::test]
    async fn synthetic_builtin_payloads_match_live_consumer_shapes() {
        let temporary = tempfile::tempdir().unwrap();
        let (service, _) = service(temporary.path());
        enable(&service);
        let id = service
            .create_consumer(
                "Overlay".into(),
                super::super::ConsumerPermissions {
                    channels: BTreeSet::from([
                        "twitch.chat".into(),
                        "twitch.support".into(),
                        "twitch.channel".into(),
                    ]),
                    ..Default::default()
                },
                BTreeSet::new(),
                None,
            )
            .unwrap()
            .id;
        let mut stream = service.events.subscribe_for_consumer(
            Some(&id),
            BTreeSet::from([
                "twitch.chat".into(),
                "twitch.support".into(),
                "twitch.channel".into(),
            ]),
            BTreeMap::new(),
        );
        let chat = ApiAction {
            kind: Action::Chat,
            service: Arc::clone(&service),
        };
        let inputs: Values = serde_json::from_value(json!({"consumers":[id], "user_id":"7", "login":"viewer", "user":"Viewer", "color":"#aabbcc", "message":"Hello"})).unwrap();
        chat.execute(inputs, CancellationToken::new())
            .await
            .unwrap();
        let frame = stream.receiver.try_recv().unwrap();
        assert_eq!(frame.channel, "twitch.chat");
        assert_eq!(frame.data["parts"], json!([{"text":"Hello"}]));
        assert_eq!(frame.data["userId"], "7");
        let mut ids = BTreeSet::from([frame.data["id"].as_str().unwrap().to_owned()]);
        for kind in ["sub", "resub", "gift", "bits"] {
            let action = ApiAction {
                kind: Action::Support,
                service: Arc::clone(&service),
            };
            let inputs = serde_json::from_value(json!({"consumers":[id], "kind":kind, "channel_id":"100", "user":"Viewer", "tier":"1000", "amount":5, "message":"Thanks"})).unwrap();
            action
                .execute(inputs, CancellationToken::new())
                .await
                .unwrap();
            let frame = stream.receiver.try_recv().unwrap();
            assert_eq!(frame.channel, "twitch.support");
            assert_eq!(frame.data["kind"], kind);
            assert_eq!(frame.data["channelId"], "100");
            assert_eq!(frame.data["amount"], 5);
            assert_eq!(frame.data["tier"], "1000");
            assert_eq!(frame.data["message"], "Thanks");
            assert!(
                chrono::DateTime::parse_from_rfc3339(frame.data["timestamp"].as_str().unwrap())
                    .is_ok()
            );
            assert!(ids.insert(frame.data["id"].as_str().unwrap().to_owned()));
        }
        let action = ApiAction {
            kind: Action::Game,
            service: Arc::clone(&service),
        };
        let inputs = serde_json::from_value(json!({"consumers":[id], "category_id":"42", "name":"Test game", "broadcaster_id":"100"})).unwrap();
        action
            .execute(inputs, CancellationToken::new())
            .await
            .unwrap();
        let frame = stream.receiver.try_recv().unwrap();
        assert_eq!(frame.channel, "twitch.channel");
        assert_eq!(
            frame.data,
            json!({"id":"42", "name":"Test game", "broadcasterId":"100"})
        );
    }

    /// Synthetic games reuse a known live identity without requiring or changing one.
    #[tokio::test]
    async fn synthetic_game_preserves_live_broadcaster_unless_explicitly_overridden() {
        let temporary = tempfile::tempdir().unwrap();
        let (service, _) = service(temporary.path());
        enable(&service);
        let recipient = consumer(&service, "Overlay", "twitch.channel");
        let action = ApiAction {
            kind: Action::Game,
            service: Arc::clone(&service),
        };
        assert!(GameInputs::default().broadcaster_id.is_none());
        let channels = BTreeSet::from(["twitch.channel".into()]);
        let mut stream = service.events.subscribe_for_consumer(
            Some(&recipient),
            channels.clone(),
            BTreeMap::new(),
        );
        let inputs: Values = serde_json::from_value(json!({
            "consumers": [recipient], "category_id": "42", "name": "Test game"
        }))
        .unwrap();
        action
            .execute(inputs.clone(), CancellationToken::new())
            .await
            .unwrap();
        assert!(
            stream
                .receiver
                .try_recv()
                .unwrap()
                .data
                .get("broadcasterId")
                .is_none()
        );
        assert!(service.events.broadcaster_id().is_none());

        service.events.set_broadcaster("100").unwrap();
        let mut stream =
            service
                .events
                .subscribe_for_consumer(Some(&recipient), channels, BTreeMap::new());
        action
            .execute(inputs.clone(), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            stream.receiver.try_recv().unwrap().data["broadcasterId"],
            "100"
        );
        let mut overridden = inputs;
        overridden.insert("broadcaster_id".into(), json!("200"));
        action
            .execute(overridden, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            stream.receiver.try_recv().unwrap().data["broadcasterId"],
            "200"
        );
        assert_eq!(service.events.broadcaster_id().as_deref(), Some("100"));
    }

    /// The ordinary workflow engine emits a private structured custom event through its registered action.
    #[tokio::test]
    async fn engine_executes_native_custom_action_with_json_payload() {
        let temporary = tempfile::tempdir().unwrap();
        let (service, mut engine) = service(temporary.path());
        enable(&service);
        let alice = consumer(&service, "Alice", "api.custom");
        let bob = consumer(&service, "Bob", "api.custom");
        let mut stream = service.events.subscribe_for_consumer(
            Some(&alice),
            BTreeSet::from(["api.custom".into()]),
            BTreeMap::new(),
        );
        register(
            &mut engine,
            &mut ChoiceCatalog::default(),
            Arc::clone(&service),
        );
        let inputs = BTreeMap::from([
            ("consumers".into(), Input::Literal(json!([alice]))),
            ("name".into(), Input::Literal(json!("confetti"))),
            (
                "payload".into(),
                Input::Literal(json!({"count": 5, "colors": ["pink"]})),
            ),
        ]);
        let workflow = Arc::new(Workflow {
            id: "native-api-test".into(),
            revision: 1,
            overlap: false,
            steps: vec![Step {
                id: "send".into(),
                on_failure: Default::default(),
                kind: StepKind::Action {
                    capability: "api.send_custom".into(),
                    version: 1,
                    inputs,
                    deadline_ms: None,
                },
            }],
            outputs: BTreeMap::new(),
        });
        let result = engine
            .run(workflow, Values::new(), CancellationToken::new())
            .await;
        assert!(
            matches!(result.outcome, Outcome::Success),
            "{:?}",
            result.outcome
        );
        let frame = stream.receiver.try_recv().unwrap();
        assert_eq!(frame.channel, "api.custom");
        assert_eq!(frame.data["payload"], json!({"count":5,"colors":["pink"]}));
        assert_eq!(frame.data["name"], "confetti");
        assert!(frame.data["id"].as_str().unwrap().starts_with("synthetic-"));
        assert!(!frame.visible_to(&bob));
    }
}
