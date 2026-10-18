//! Workflow and Lua bindings for the compiled provider operations.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::ConfigSchema;
use crate::engine::{
    Capability, CapabilityError, Engine, Input, Values, validate_configured_inputs,
};
use crate::schema::{ActionDefinition, DescribeConfig};

use super::{ProviderDescriptor, ProviderError, ProviderId, ProviderOperation, ProviderServices};

/// Search term for SteamGridDB game matches.
#[derive(Default, Deserialize, Serialize, ConfigSchema)]
#[serde(deny_unknown_fields)]
#[config(
    id = "steamgriddb.search",
    version = 1,
    title = "Search SteamGridDB games",
    output("results", "Games", "json")
)]
struct SearchInputs {
    #[config(id = "name", label = "Game name", introduced = 1)]
    name: String,
}

/// Stable SteamGridDB game ID for icon retrieval.
#[derive(Default, Deserialize, Serialize, ConfigSchema)]
#[serde(deny_unknown_fields)]
#[config(
    id = "steamgriddb.icons",
    version = 1,
    title = "Get SteamGridDB icons",
    output("icons", "Icons", "json")
)]
struct IconsInputs {
    #[config(id = "game_id", label = "SteamGridDB game ID", introduced = 1)]
    game_id: u64,
}

/// Explicit game ID or exact game name for icon matching.
#[derive(Default, Deserialize, Serialize, ConfigSchema)]
#[serde(deny_unknown_fields)]
#[config(
    id = "steamgriddb.resolve",
    version = 1,
    title = "Resolve SteamGridDB icon",
    output("id", "Game ID", "number", "optional"),
    output("name", "Game", "text"),
    output("icon", "Icon URL", "text", "optional")
)]
struct ResolveInputs {
    #[config(id = "name", label = "Game name", introduced = 1)]
    name: String,
    #[config(id = "game_id", label = "SteamGridDB game ID", introduced = 1)]
    #[serde(default)]
    game_id: Option<u64>,
}

/// Twitch channel whose FFZ and BTTV emotes should be resolved.
#[derive(Default, Deserialize, Serialize, ConfigSchema)]
#[serde(deny_unknown_fields)]
#[config(
    id = "emotes.resolve",
    version = 1,
    title = "Resolve channel emotes",
    output("emotes", "Emotes", "json")
)]
struct EmotesInputs {
    #[config(id = "channel_id", label = "Twitch channel ID", introduced = 1)]
    channel_id: String,
}

/// Twitch login whose public pronouns should be resolved.
#[derive(Default, Deserialize, Serialize, ConfigSchema)]
#[serde(deny_unknown_fields)]
#[config(
    id = "pronouns.resolve",
    version = 1,
    title = "Resolve public pronouns",
    output("pronouns", "Pronouns", "text", "optional")
)]
struct PronounsInputs {
    #[config(id = "login", label = "Twitch login", introduced = 1)]
    login: String,
}

/// One of the compiled provider workflow operations.
#[derive(Clone, Copy)]
enum Action {
    Search,
    Icons,
    ResolveIcon,
    Emotes,
    Pronouns,
}

impl Action {
    /// Returns the provider whose enabled state controls this action.
    fn provider(self) -> &'static str {
        match self {
            Self::Search | Self::Icons | Self::ResolveIcon => "steamgriddb",
            Self::Emotes => "emotes",
            Self::Pronouns => "pronouns",
        }
    }

    /// Returns the API operation name for this action.
    fn operation(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Icons => "icons",
            Self::ResolveIcon | Self::Emotes | Self::Pronouns => "resolve",
        }
    }

    /// Returns the compiled input schema for validation and editor metadata.
    fn schema(self) -> &'static crate::schema::ConfigSchema {
        match self {
            Self::Search => SearchInputs::SCHEMA,
            Self::Icons => IconsInputs::SCHEMA,
            Self::ResolveIcon => ResolveInputs::SCHEMA,
            Self::Emotes => EmotesInputs::SCHEMA,
            Self::Pronouns => PronounsInputs::SCHEMA,
        }
    }
}

/// Keeps action metadata and provider operation metadata in one declaration list.
fn definitions() -> [(ActionDefinition, Action); 5] {
    [
        (ActionDefinition::of::<SearchInputs>(), Action::Search),
        (ActionDefinition::of::<IconsInputs>(), Action::Icons),
        (ActionDefinition::of::<ResolveInputs>(), Action::ResolveIcon),
        (ActionDefinition::of::<EmotesInputs>(), Action::Emotes),
        (ActionDefinition::of::<PronounsInputs>(), Action::Pronouns),
    ]
}

/// Builds the public provider catalog from the registered action definitions.
pub(super) fn catalog() -> Vec<ProviderDescriptor> {
    let definitions = definitions();
    ProviderId::ALL
        .into_iter()
        .map(|configuration| ProviderDescriptor {
            configuration,
            id: configuration.document_id(),
            title: configuration.title(),
            operations: definitions
                .iter()
                .filter(|(_, action)| action.provider() == configuration.document_id())
                .map(|(definition, action)| ProviderOperation {
                    id: action.operation(),
                    title: definition.schema.title,
                    action: definition.schema.id,
                })
                .collect(),
        })
        .collect()
}

/// Registers provider actions in workflows and the Lua native binding table.
pub(super) fn register(engine: &mut Engine, services: Arc<ProviderServices>) {
    for (definition, kind) in definitions() {
        engine.register_lua_action_definition(
            definition,
            Arc::new(ProviderAction {
                kind,
                services: Arc::clone(&services),
            }),
        );
    }
}

/// Adapts a typed provider operation to the workflow capability contract.
struct ProviderAction {
    kind: Action,
    services: Arc<ProviderServices>,
}

impl Capability for ProviderAction {
    /// Keeps disabled or unconfigured providers out of workflow execution.
    fn ready(&self) -> Result<(), String> {
        self.services
            .ready(self.kind.provider())
            .map_err(|error| error.to_string())
    }

    /// Uses the action's compiled schema for editor-side input checks.
    fn validate_inputs(&self, inputs: &BTreeMap<String, Input>) -> Result<(), String> {
        validate_configured_inputs(self.kind.schema(), inputs)
    }

    /// Gives the bounded provider call time to finish before the workflow deadline.
    fn default_deadline(&self) -> Duration {
        Duration::from_secs(25)
    }

    /// Converts workflow values to API arguments and returns structured outputs.
    fn execute<'a>(
        &'a self,
        inputs: Values,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<Values, CapabilityError>> + Send + 'a>> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(CapabilityError::Failed("action cancelled".into()));
            }
            let (operation, arguments) = match self.kind {
                Action::Search => {
                    let config: SearchInputs = parse(inputs)?;
                    ("search", json!({"name": config.name}))
                }
                Action::Icons => {
                    let config: IconsInputs = parse(inputs)?;
                    ("icons", json!({"gameId": config.game_id}))
                }
                Action::ResolveIcon => {
                    let config: ResolveInputs = parse(inputs)?;
                    (
                        "resolve",
                        json!({"name": config.name, "gameId": config.game_id}),
                    )
                }
                Action::Emotes => {
                    let config: EmotesInputs = parse(inputs)?;
                    ("resolve", json!({"channelId": config.channel_id}))
                }
                Action::Pronouns => {
                    let config: PronounsInputs = parse(inputs)?;
                    ("resolve", json!({"login": config.login}))
                }
            };
            let result = tokio::select! {
                result = self.services.call(self.kind.provider(), operation, arguments) => result.map_err(capability_error)?,
                _ = cancel.cancelled() => return Err(CapabilityError::Failed("action cancelled".into())),
            };
            let outputs = match self.kind {
                Action::Search => Values::from([("results".into(), result)]),
                Action::Icons => Values::from([("icons".into(), result)]),
                Action::ResolveIcon => {
                    let Value::Object(result) = result else {
                        return Err(CapabilityError::Failed("invalid icon result".into()));
                    };
                    result
                        .into_iter()
                        .filter(|(_, value)| !value.is_null())
                        .collect()
                }
                Action::Emotes => Values::from([("emotes".into(), result)]),
                Action::Pronouns => {
                    let label = result
                        .get("pronouns")
                        .filter(|value| !value.is_null())
                        .cloned();
                    label
                        .map(|label| Values::from([("pronouns".into(), label)]))
                        .unwrap_or_default()
                }
            };
            Ok(outputs)
        })
    }
}

/// Deserializes resolved workflow values into an operation's input type.
fn parse<T: for<'de> Deserialize<'de>>(inputs: Values) -> Result<T, CapabilityError> {
    serde_json::from_value(json!(inputs))
        .map_err(|_| CapabilityError::Failed("invalid provider action inputs".into()))
}

/// Maps provider readiness failures to unavailable capabilities for workflows.
fn capability_error(error: ProviderError) -> CapabilityError {
    match error {
        ProviderError::Disabled
        | ProviderError::MissingCredential
        | ProviderError::CredentialUnavailable => {
            CapabilityError::ConnectorUnavailable(error.to_string())
        }
        _ => CapabilityError::Failed(error.to_string()),
    }
}
