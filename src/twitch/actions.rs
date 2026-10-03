//! Workflow capabilities supplied by the compiled Twitch module.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::ConfigSchema;
use crate::engine::{
    Capability, CapabilityError, Engine, FormValidator, Input, Values, validate_configured_inputs,
};
use crate::schema::{ActionDefinition, DescribeConfig};

use super::credentials::CredentialBackend;
use super::device::FormTransport;
use super::echoes::ChatEchoes;
use super::helix::{ChannelUpdate, Helix, HelixError, HelixTransport};
use super::session::{TwitchCapability, TwitchSession};

#[derive(Default, Deserialize, Serialize, ConfigSchema)]
#[serde(deny_unknown_fields)]
#[config(
    id = "twitch.send_chat",
    version = 1,
    title = "Send chat message",
    output("message_id", "Message ID", "text")
)]
struct SendChatInputs {
    #[config(
        id = "message",
        label = "Message",
        description = "Text sent to the broadcaster's chat",
        introduced = 1
    )]
    message: String,
    #[config(
        id = "broadcaster_fallback",
        label = "Use broadcaster if bot authorization fails",
        description = "Send as the broadcaster only when bot authorization fails before sending",
        introduced = 1
    )]
    #[serde(default)]
    broadcaster_fallback: Option<bool>,
}

#[derive(Default, Deserialize, Serialize, ConfigSchema)]
#[serde(deny_unknown_fields)]
#[config(
    id = "twitch.get_target_info",
    version = 1,
    title = "Get target channel information",
    output("id", "User ID", "text"),
    output("login", "Login", "text"),
    output("display_name", "Display name", "text"),
    output("game_id", "Game ID", "text"),
    output("game_name", "Game", "text")
)]
struct GetTargetInfoInputs {
    #[config(id = "login", label = "User login", introduced = 1)]
    login: String,
}

#[derive(Default, Deserialize, Serialize, ConfigSchema)]
#[serde(deny_unknown_fields)]
#[config(
    id = "twitch.get_channel",
    version = 1,
    title = "Get channel information",
    output("title", "Title", "text"),
    output("game_id", "Game ID", "text"),
    output("game_name", "Game", "text")
)]
struct GetChannelInputs {}

#[derive(Default, Deserialize, Serialize, ConfigSchema)]
#[serde(deny_unknown_fields)]
#[config(
    id = "twitch.set_channel",
    version = 1,
    title = "Set channel information"
)]
struct SetChannelInputs {
    #[config(id = "title", label = "Title", introduced = 1)]
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[config(id = "game_id", label = "Game ID", introduced = 1)]
    #[serde(skip_serializing_if = "Option::is_none")]
    game_id: Option<String>,
}

#[derive(Default, Deserialize, Serialize, ConfigSchema)]
#[serde(deny_unknown_fields)]
#[config(
    id = "twitch.find_game",
    version = 1,
    title = "Find game",
    output("id", "Game ID", "text"),
    output("name", "Game", "text")
)]
struct FindGameInputs {
    #[config(id = "name", label = "Game name", introduced = 1)]
    name: String,
}

#[derive(Default, Deserialize, Serialize, ConfigSchema)]
#[serde(deny_unknown_fields)]
#[config(
    id = "twitch.find_user",
    version = 1,
    title = "Find user",
    output("id", "User ID", "text"),
    output("login", "Login", "text"),
    output("display_name", "Display name", "text"),
    output("description", "Description", "text")
)]
struct FindUserInputs {
    #[config(id = "login", label = "User login", introduced = 1)]
    login: String,
}

#[derive(Clone, Copy)]
enum Action {
    SendChat,
    GetChannel,
    SetChannel,
    FindGame,
    FindUser,
    GetTargetInfo,
}

pub fn register<B, T, H>(
    engine: &mut Engine,
    session: Arc<TwitchSession<B, T>>,
    helix: Arc<Helix<H>>,
    echoes: Arc<ChatEchoes>,
) where
    B: CredentialBackend + 'static,
    T: FormTransport + Send + Sync + 'static,
    H: HelixTransport + Send + Sync + 'static,
{
    for (definition, action) in [
        (ActionDefinition::of::<SendChatInputs>(), Action::SendChat),
        (
            ActionDefinition::of::<GetChannelInputs>(),
            Action::GetChannel,
        ),
        (
            ActionDefinition::of::<SetChannelInputs>(),
            Action::SetChannel,
        ),
        (ActionDefinition::of::<FindGameInputs>(), Action::FindGame),
        (ActionDefinition::of::<FindUserInputs>(), Action::FindUser),
        (
            ActionDefinition::of::<GetTargetInfoInputs>(),
            Action::GetTargetInfo,
        ),
    ] {
        let capability = Arc::new(TwitchAction {
            kind: action,
            session: Arc::clone(&session),
            helix: Arc::clone(&helix),
            echoes: Arc::clone(&echoes),
        });
        if matches!(action, Action::FindGame) {
            engine.register_form_validator(
                "twitch.game",
                Arc::new(GameFormValidator {
                    action: capability.clone(),
                }),
            );
        }
        engine.register_lua_action_definition(definition, capability);
    }
}

struct TwitchAction<B, T, H> {
    kind: Action,
    session: Arc<TwitchSession<B, T>>,
    helix: Arc<Helix<H>>,
    echoes: Arc<ChatEchoes>,
}

struct GameFormValidator<B, T, H> {
    action: Arc<TwitchAction<B, T, H>>,
}

impl<B, T, H> FormValidator for GameFormValidator<B, T, H>
where
    B: CredentialBackend + 'static,
    T: FormTransport + Send + Sync + 'static,
    H: HelixTransport + Send + Sync + 'static,
{
    fn validate<'a>(
        &'a self,
        value: &'a str,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>> {
        Box::pin(async move {
            let outputs = self
                .action
                .find_game(
                    Values::from([("name".into(), Value::String(value.trim().into()))]),
                    cancel,
                )
                .await
                .map_err(|error| match error {
                    CapabilityError::Failed(message)
                    | CapabilityError::ConnectorUnavailable(message)
                    | CapabilityError::Uncertain(message) => message,
                })?;
            outputs
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| "Twitch did not return a game name".into())
        })
    }
}

impl<B, T, H> Capability for TwitchAction<B, T, H>
where
    B: CredentialBackend + 'static,
    T: FormTransport + Send + Sync + 'static,
    H: HelixTransport + Send + Sync + 'static,
{
    fn validate_inputs(&self, inputs: &BTreeMap<String, Input>) -> Result<(), String> {
        let schema = match self.kind {
            Action::SendChat => SendChatInputs::SCHEMA,
            Action::GetChannel => GetChannelInputs::SCHEMA,
            Action::SetChannel => SetChannelInputs::SCHEMA,
            Action::FindGame => FindGameInputs::SCHEMA,
            Action::FindUser => FindUserInputs::SCHEMA,
            Action::GetTargetInfo => GetTargetInfoInputs::SCHEMA,
        };
        validate_configured_inputs(schema, inputs)?;
        match self.kind {
            Action::SendChat => match inputs.get("message") {
                Some(Input::Literal(Value::String(message))) => SendChatInputs {
                    message: message.clone(),
                    broadcaster_fallback: None,
                }
                .validate(),
                _ => Ok(()),
            },
            Action::SetChannel => {
                if ["title", "game_id"].iter().any(|name| {
                    inputs
                        .get(*name)
                        .is_some_and(|input| !matches!(input, Input::Literal(Value::Null)))
                }) {
                    Ok(())
                } else {
                    Err("set channel requires a title or game ID".into())
                }
            }
            _ => Ok(()),
        }
    }

    fn default_deadline(&self) -> Duration {
        Duration::from_secs(60)
    }

    fn execute<'a>(
        &'a self,
        inputs: Values,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<Values, CapabilityError>> + Send + 'a>> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(CapabilityError::Failed("action cancelled".into()));
            }
            match self.kind {
                Action::SendChat => self.send_chat(inputs, cancel).await,
                Action::GetChannel => self.get_channel(inputs, cancel).await,
                Action::SetChannel => self.set_channel(inputs, cancel).await,
                Action::FindGame => self.find_game(inputs, cancel).await,
                Action::FindUser => self.find_user(inputs, cancel).await,
                Action::GetTargetInfo => self.get_target_info(inputs, cancel).await,
            }
        })
    }
}

impl<B, T, H> TwitchAction<B, T, H>
where
    B: CredentialBackend + 'static,
    T: FormTransport + Send + Sync + 'static,
    H: HelixTransport + Send + Sync + 'static,
{
    async fn send_chat(
        &self,
        inputs: Values,
        cancel: CancellationToken,
    ) -> Result<Values, CapabilityError> {
        let config: SendChatInputs = parse_inputs(inputs)?;
        config.validate().map_err(CapabilityError::Failed)?;
        let broadcaster = self
            .session
            .authorize(TwitchCapability::ChannelOwner)
            .await
            .map_err(connector)?;
        let sender = self.session.authorize(TwitchCapability::BotFeature).await;
        let (sender_token, sender_id, broadcaster_id) = match sender {
            Ok(sender) => (
                sender.access_token().to_owned(),
                sender.user_id().to_owned(),
                broadcaster.user_id().to_owned(),
            ),
            Err(_error) if config.broadcaster_fallback == Some(true) => {
                let broadcaster = self
                    .session
                    .authorize(TwitchCapability::ChannelOwner)
                    .await
                    .map_err(connector)?;
                (
                    broadcaster.access_token().to_owned(),
                    broadcaster.user_id().to_owned(),
                    broadcaster.user_id().to_owned(),
                )
            }
            Err(error) => return Err(connector(error)),
        };
        ensure_active(&cancel)?;
        let result = self
            .helix
            .send_chat_message(&sender_token, &broadcaster_id, &sender_id, &config.message)
            .await
            .map_err(helix_error)?;
        if !result.is_sent {
            let reason = result
                .drop_reason
                .map(|reason| format!("{}: {}", reason.code, reason.message))
                .unwrap_or_else(|| "Twitch did not send the message".into());
            return Err(CapabilityError::Failed(reason));
        }
        self.echoes.record(result.message_id.clone());
        Ok(Values::from([(
            "message_id".into(),
            Value::String(result.message_id),
        )]))
    }

    async fn get_channel(
        &self,
        inputs: Values,
        cancel: CancellationToken,
    ) -> Result<Values, CapabilityError> {
        let _: GetChannelInputs = parse_inputs(inputs)?;
        let broadcaster = self
            .session
            .authorize(TwitchCapability::ChannelOwner)
            .await
            .map_err(connector)?;
        ensure_active(&cancel)?;
        let channel = self
            .helix
            .get_channel_information(broadcaster.access_token(), broadcaster.user_id())
            .await
            .map_err(helix_error)?
            .ok_or_else(|| CapabilityError::Failed("Twitch channel was not found".into()))?;
        if channel.broadcaster_id != broadcaster.user_id() {
            return Err(CapabilityError::Failed(
                "Twitch returned a different channel".into(),
            ));
        }
        Ok(Values::from([
            ("title".into(), Value::String(channel.title)),
            ("game_id".into(), Value::String(channel.game_id)),
            ("game_name".into(), Value::String(channel.game_name)),
        ]))
    }

    async fn set_channel(
        &self,
        inputs: Values,
        cancel: CancellationToken,
    ) -> Result<Values, CapabilityError> {
        let config: SetChannelInputs = parse_inputs(inputs)?;
        let update = ChannelUpdate {
            title: config.title,
            game_id: config.game_id,
        };
        if update.title.is_none() && update.game_id.is_none() {
            return Err(CapabilityError::Failed(
                "set channel requires a title or game ID".into(),
            ));
        }
        let broadcaster = self
            .session
            .authorize(TwitchCapability::ChannelOwner)
            .await
            .map_err(connector)?;
        ensure_active(&cancel)?;
        self.helix
            .modify_channel_information(broadcaster.access_token(), broadcaster.user_id(), &update)
            .await
            .map_err(helix_error)?;
        Ok(Values::new())
    }

    async fn find_game(
        &self,
        inputs: Values,
        cancel: CancellationToken,
    ) -> Result<Values, CapabilityError> {
        let config: FindGameInputs = parse_inputs(inputs)?;
        let broadcaster = self
            .session
            .authorize(TwitchCapability::ChannelOwner)
            .await
            .map_err(connector)?;
        ensure_active(&cancel)?;
        let mut games = self
            .helix
            .get_games_by_name(broadcaster.access_token(), &config.name)
            .await
            .map_err(helix_error)?;
        if games.len() != 1 {
            return Err(CapabilityError::Failed(if games.is_empty() {
                "Twitch game was not found".into()
            } else {
                "Twitch game name is ambiguous".into()
            }));
        }
        let game = games.remove(0);
        Ok(Values::from([
            ("id".into(), Value::String(game.id)),
            ("name".into(), Value::String(game.name)),
        ]))
    }

    async fn find_user(
        &self,
        inputs: Values,
        cancel: CancellationToken,
    ) -> Result<Values, CapabilityError> {
        let config: FindUserInputs = parse_inputs(inputs)?;
        let broadcaster = self
            .session
            .authorize(TwitchCapability::ChannelOwner)
            .await
            .map_err(connector)?;
        ensure_active(&cancel)?;
        let user = self
            .helix
            .get_user_by_login(broadcaster.access_token(), &config.login)
            .await
            .map_err(helix_error)?
            .ok_or_else(|| CapabilityError::Failed("Twitch user was not found".into()))?;
        Ok(Values::from([
            ("id".into(), Value::String(user.id)),
            ("login".into(), Value::String(user.login)),
            ("display_name".into(), Value::String(user.display_name)),
            ("description".into(), Value::String(user.description)),
        ]))
    }

    async fn get_target_info(
        &self,
        inputs: Values,
        cancel: CancellationToken,
    ) -> Result<Values, CapabilityError> {
        let config: GetTargetInfoInputs = parse_inputs(inputs)?;
        let login = config.login.trim();
        let login = login.strip_prefix('@').unwrap_or(login).trim();
        if login.is_empty() {
            return Err(CapabilityError::Failed(
                "Twitch user login must not be empty".into(),
            ));
        }
        let broadcaster = self
            .session
            .authorize(TwitchCapability::ChannelOwner)
            .await
            .map_err(connector)?;
        ensure_active(&cancel)?;
        let user = self
            .helix
            .get_user_by_login(broadcaster.access_token(), login)
            .await
            .map_err(helix_error)?
            .ok_or_else(|| CapabilityError::Failed("Twitch user was not found".into()))?;
        ensure_active(&cancel)?;
        let channel = self
            .helix
            .get_channel_information(broadcaster.access_token(), &user.id)
            .await
            .map_err(helix_error)?
            .ok_or_else(|| CapabilityError::Failed("Twitch channel was not found".into()))?;
        if channel.broadcaster_id != user.id {
            return Err(CapabilityError::Failed(
                "Twitch returned a different channel".into(),
            ));
        }
        Ok(Values::from([
            ("id".into(), Value::String(user.id)),
            ("login".into(), Value::String(user.login)),
            ("display_name".into(), Value::String(user.display_name)),
            ("game_id".into(), Value::String(channel.game_id)),
            ("game_name".into(), Value::String(channel.game_name)),
        ]))
    }
}

impl SendChatInputs {
    fn validate(&self) -> Result<(), String> {
        if self.message.trim().is_empty() {
            return Err("chat message must not be empty".into());
        }
        Ok(())
    }
}

fn parse_inputs<T: DeserializeOwned>(inputs: Values) -> Result<T, CapabilityError> {
    serde_json::from_value(Value::Object(inputs.into_iter().collect()))
        .map_err(|error| CapabilityError::Failed(format!("invalid Twitch action inputs: {error}")))
}

fn ensure_active(cancel: &CancellationToken) -> Result<(), CapabilityError> {
    if cancel.is_cancelled() {
        Err(CapabilityError::Failed("action cancelled".into()))
    } else {
        Ok(())
    }
}

fn connector(error: impl std::fmt::Display) -> CapabilityError {
    CapabilityError::ConnectorUnavailable(error.to_string())
}

fn helix_error(error: HelixError) -> CapabilityError {
    match error {
        HelixError::Unauthorized => CapabilityError::ConnectorUnavailable(
            "Twitch authorization was rejected; reconnect the account".into(),
        ),
        HelixError::Forbidden => CapabilityError::ConnectorUnavailable(
            "Twitch account cannot perform this operation in the channel".into(),
        ),
        HelixError::RateLimited { retry_after } => CapabilityError::Failed(match retry_after {
            Some(delay) => format!(
                "Twitch rate limit reached; retry after {} seconds",
                delay.as_secs()
            ),
            None => "Twitch rate limit reached".into(),
        }),
        other => CapabilityError::Failed(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::migration::StoredConfig;
    use crate::storage::ConfigStore;
    use crate::twitch::credentials::{
        CredentialError, OAuthTokens, TwitchCredentialStore, TwitchRole,
    };
    use crate::twitch::device::{DeviceAuth, DeviceError, FormResponse};
    use crate::twitch::helix::{HelixMethod, HelixRequest, HelixResponse};

    #[test]
    fn action_output_schemas_match_helix_results() {
        let cases: &[(&crate::schema::ConfigSchema, &[&str])] = &[
            (SendChatInputs::SCHEMA, &["message_id"]),
            (
                GetTargetInfoInputs::SCHEMA,
                &["id", "login", "display_name", "game_id", "game_name"],
            ),
            (GetChannelInputs::SCHEMA, &["title", "game_id", "game_name"]),
            (SetChannelInputs::SCHEMA, &[]),
            (FindGameInputs::SCHEMA, &["id", "name"]),
            (
                FindUserInputs::SCHEMA,
                &["id", "login", "display_name", "description"],
            ),
        ];
        for (schema, expected) in cases {
            assert_eq!(
                schema
                    .outputs
                    .iter()
                    .map(|output| output.id)
                    .collect::<Vec<_>>(),
                *expected
            );
        }
    }
    const BROADCASTER_SLOT: &str = "b63e12df-d389-46e5-892c-779036286c8e";
    const BOT_SLOT: &str = "d304c860-d5d1-4a6b-9768-7519cb83de83";

    #[derive(Default)]
    struct MemoryCredentials(Mutex<HashMap<String, String>>);

    impl CredentialBackend for Arc<MemoryCredentials> {
        fn get(&self, _service: &str, account: &str) -> Result<Option<String>, CredentialError> {
            Ok(self.0.lock().unwrap().get(account).cloned())
        }

        fn set(&self, _service: &str, account: &str, value: &str) -> Result<(), CredentialError> {
            self.0.lock().unwrap().insert(account.into(), value.into());
            Ok(())
        }

        fn delete(&self, _service: &str, account: &str) -> Result<(), CredentialError> {
            self.0.lock().unwrap().remove(account);
            Ok(())
        }
    }

    struct Auth {
        reject_bot: bool,
    }

    impl FormTransport for Auth {
        async fn post_form(
            &self,
            _url: &str,
            _fields: &[(&str, &str)],
        ) -> Result<FormResponse, DeviceError> {
            Err(DeviceError::InvalidResponse)
        }

        async fn get_bearer(&self, _url: &str, token: &str) -> Result<FormResponse, DeviceError> {
            if self.reject_bot && token == "bot-token" {
                return Err(DeviceError::InvalidAccessToken);
            }
            let (user_id, scopes) = if token == "bot-token" {
                ("456", json!(["user:write:chat"]))
            } else {
                (
                    "123",
                    json!([
                        "channel:manage:broadcast",
                        "user:read:chat",
                        "user:write:chat"
                    ]),
                )
            };
            Ok(FormResponse {
                status: 200,
                body: json!({
                    "client_id": super::super::device::CLIENT_ID,
                    "user_id": user_id,
                    "login": "login",
                    "scopes": scopes,
                    "expires_in": 3600
                })
                .to_string()
                .into_bytes(),
                retry_after: None,
            })
        }
    }

    type Request = (HelixMethod, String, String, Value);

    struct FakeHelix {
        requests: Mutex<Vec<Request>>,
        send_ok: bool,
    }

    impl HelixTransport for Arc<FakeHelix> {
        async fn send(&self, request: HelixRequest) -> Result<HelixResponse, HelixError> {
            let body = request
                .body()
                .map(|body| serde_json::from_slice(body).unwrap())
                .unwrap_or(Value::Null);
            let path = request.url.path().to_owned();
            let method = request.method;
            self.requests.lock().unwrap().push((
                method,
                path.clone(),
                request.access_token().into(),
                body,
            ));
            let (status, body) = match path.as_str() {
                "/helix/chat/messages" => (
                    200,
                    json!({"data":[{
                        "message_id": if self.send_ok { "m1" } else { "" },
                        "is_sent": self.send_ok,
                        "drop_reason": if self.send_ok { Value::Null } else { json!({"code":"automod_held","message":"Held for review"}) }
                    }]}),
                ),
                "/helix/channels" if method == HelixMethod::Get => (
                    200,
                    json!({"data":[{
                        "broadcaster_id":"789",
                        "broadcaster_login":"target",
                        "broadcaster_name":"Target",
                        "game_id":"42",
                        "game_name":"Example Game",
                        "title":"Target stream"
                    }]}),
                ),
                "/helix/channels" => (204, Value::Null),
                "/helix/users" => (
                    200,
                    json!({"data":[{
                        "id":"789",
                        "login":"target",
                        "display_name":"Target",
                        "description":"A target channel"
                    }]}),
                ),
                "/helix/games" => (
                    200,
                    json!({"data":[{"id":"42","name":"Example Game","box_art_url":""}]}),
                ),
                _ => return Err(HelixError::NotFound),
            };
            Ok(HelixResponse {
                status,
                body: if status == 204 {
                    Vec::new()
                } else {
                    body.to_string().into_bytes()
                },
                retry_after: None,
            })
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        session: Arc<TwitchSession<Arc<MemoryCredentials>, Auth>>,
        helix: Arc<Helix<Arc<FakeHelix>>>,
        transport: Arc<FakeHelix>,
    }

    impl Fixture {
        fn new(send_ok: bool) -> Self {
            Self::with_auth(send_ok, Auth { reject_bot: false })
        }

        fn with_auth(send_ok: bool, auth: Auth) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let memory = Arc::new(MemoryCredentials::default());
            let credentials = TwitchCredentialStore::new(memory, super::super::device::CLIENT_ID);
            credentials
                .replace(
                    TwitchRole::Broadcaster,
                    BROADCASTER_SLOT,
                    &OAuthTokens::new("broadcaster-token".into(), "refresh".into()),
                )
                .unwrap();
            credentials
                .replace(
                    TwitchRole::Bot,
                    BOT_SLOT,
                    &OAuthTokens::new("bot-token".into(), "refresh".into()),
                )
                .unwrap();
            let store = ConfigStore::new(dir.path());
            store
                .create(
                    "twitch-accounts",
                    &StoredConfig {
                        definition: "snenkbot.twitch.accounts".into(),
                        version: 1,
                        data: json!({
                            "broadcaster": {"user_id":"123","login":"streamer","scopes":["channel:manage:broadcast","user:read:chat","user:write:chat"],"credential_slot":BROADCASTER_SLOT},
                            "bot": {"user_id":"456","login":"bot","scopes":["user:write:chat"],"credential_slot":BOT_SLOT}
                        }),
                    },
                    |_| Ok(()),
                )
                .unwrap();
            let accounts = Arc::new(super::super::accounts::TwitchAccounts::new(
                store,
                credentials,
            ));
            let session = Arc::new(TwitchSession::new(accounts, DeviceAuth::new(auth)));
            let transport = Arc::new(FakeHelix {
                requests: Mutex::new(Vec::new()),
                send_ok,
            });
            let helix = Arc::new(Helix::new(Arc::clone(&transport)));
            Self {
                _dir: dir,
                session,
                helix,
                transport,
            }
        }

        fn action(
            &self,
            kind: Action,
        ) -> TwitchAction<Arc<MemoryCredentials>, Auth, Arc<FakeHelix>> {
            TwitchAction {
                kind,
                session: Arc::clone(&self.session),
                helix: Arc::clone(&self.helix),
                echoes: Arc::new(ChatEchoes::default()),
            }
        }
    }

    #[tokio::test]
    async fn send_chat_uses_broadcaster_channel_and_bot_sender() {
        let fixture = Fixture::new(true);
        let action = fixture.action(Action::SendChat);
        let outputs = action
            .execute(
                Values::from([("message".into(), json!("hello"))]),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(outputs["message_id"], "m1");
        assert!(action.echoes.contains("m1"));
        let requests = fixture.transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].2, "bot-token");
        assert_eq!(requests[0].3["broadcaster_id"], "123");
        assert_eq!(requests[0].3["sender_id"], "456");
        assert_eq!(requests[0].3["message"], "hello");
    }

    #[tokio::test]
    async fn send_chat_can_opt_into_broadcaster_when_bot_authorization_fails() {
        let fixture = Fixture::with_auth(true, Auth { reject_bot: true });
        let output = fixture
            .action(Action::SendChat)
            .execute(
                Values::from([
                    ("message".into(), json!("hello")),
                    ("broadcaster_fallback".into(), json!(true)),
                ]),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(output["message_id"], "m1");
        let requests = fixture.transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].2, "broadcaster-token");
        assert_eq!(requests[0].3["sender_id"], "123");
    }

    #[tokio::test]
    async fn send_chat_does_not_fallback_without_explicit_opt_in() {
        let fixture = Fixture::with_auth(true, Auth { reject_bot: true });
        let result = fixture
            .action(Action::SendChat)
            .execute(
                Values::from([("message".into(), json!("hello"))]),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
        assert!(fixture.transport.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn target_info_resolves_user_then_returns_channel_game() {
        let fixture = Fixture::new(true);
        let output = fixture
            .action(Action::GetTargetInfo)
            .execute(
                Values::from([("login".into(), json!("@target"))]),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(output["id"], "789");
        assert_eq!(output["login"], "target");
        assert_eq!(output["display_name"], "Target");
        assert_eq!(output["game_id"], "42");
        assert_eq!(output["game_name"], "Example Game");
        let requests = fixture.transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].1, "/helix/users");
        assert_eq!(requests[1].1, "/helix/channels");
    }

    #[tokio::test]
    async fn target_info_rejects_empty_login_before_contacting_twitch() {
        let fixture = Fixture::new(true);
        let result = fixture
            .action(Action::GetTargetInfo)
            .execute(
                Values::from([("login".into(), json!(" @ "))]),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
        assert!(fixture.transport.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn send_chat_rejects_invalid_inputs_before_contacting_twitch() {
        let fixture = Fixture::new(true);
        let action = fixture.action(Action::SendChat);
        for inputs in [
            Values::new(),
            Values::from([("message".into(), json!(42))]),
            Values::from([("message".into(), json!("  "))]),
            Values::from([
                ("message".into(), json!("hello")),
                ("unexpected".into(), json!(true)),
            ]),
        ] {
            assert!(
                action
                    .execute(inputs, CancellationToken::new())
                    .await
                    .is_err()
            );
        }
        assert!(fixture.transport.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn send_chat_schema_validates_saved_inputs() {
        let fixture = Fixture::new(true);
        let action = fixture.action(Action::SendChat);
        assert_eq!(SendChatInputs::SCHEMA.id, "twitch.send_chat");
        assert_eq!(SendChatInputs::SCHEMA.version, 1);
        assert_eq!(SendChatInputs::SCHEMA.fields.len(), 2);
        assert_eq!(SendChatInputs::SCHEMA.fields[0].id, "message");
        assert_eq!(SendChatInputs::SCHEMA.fields[1].id, "broadcaster_fallback");
        assert!(!SendChatInputs::SCHEMA.fields[1].required);
        for input in [
            Input::Literal(json!("hello")),
            Input::Reference {
                step_id: "previous".into(),
                output_id: "text".into(),
                fallback: None,
            },
        ] {
            assert!(
                action
                    .validate_inputs(&BTreeMap::from([("message".into(), input)]))
                    .is_ok()
            );
        }
        for input in [Input::Literal(json!(42)), Input::Literal(json!("  "))] {
            assert!(
                action
                    .validate_inputs(&BTreeMap::from([("message".into(), input)]))
                    .is_err()
            );
        }
        assert!(action.validate_inputs(&BTreeMap::new()).is_err());
    }

    #[tokio::test]
    async fn typed_twitch_actions_reject_bad_inputs_before_contacting_twitch() {
        let fixture = Fixture::new(true);
        for (kind, inputs) in [
            (
                Action::GetChannel,
                Values::from([("extra".into(), json!(1))]),
            ),
            (Action::SetChannel, Values::new()),
            (
                Action::SetChannel,
                Values::from([("title".into(), Value::Null)]),
            ),
            (
                Action::SetChannel,
                Values::from([("title".into(), json!(5))]),
            ),
            (Action::FindGame, Values::new()),
            (Action::FindGame, Values::from([("name".into(), json!(7))])),
            (Action::FindUser, Values::new()),
            (
                Action::FindUser,
                Values::from([("login".into(), json!(false))]),
            ),
        ] {
            let action = fixture.action(kind);
            assert!(
                action
                    .execute(inputs, CancellationToken::new())
                    .await
                    .is_err()
            );
        }
        assert!(fixture.transport.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn optional_channel_fields_accept_one_value_but_require_a_change() {
        let fixture = Fixture::new(true);
        let action = fixture.action(Action::SetChannel);
        assert!(!SetChannelInputs::SCHEMA.fields[0].required);
        assert!(!SetChannelInputs::SCHEMA.fields[1].required);
        for inputs in [
            BTreeMap::from([("title".into(), Input::Literal(json!("New title")))]),
            BTreeMap::from([("game_id".into(), Input::Literal(json!("123")))]),
        ] {
            assert!(action.validate_inputs(&inputs).is_ok());
        }
        assert!(action.validate_inputs(&BTreeMap::new()).is_err());
        assert!(
            action
                .validate_inputs(&BTreeMap::from([(
                    "title".into(),
                    Input::Literal(Value::Null),
                )]))
                .is_err()
        );
    }

    #[tokio::test]
    async fn game_form_validator_returns_canonical_name_for_later_actions() {
        let fixture = Fixture::new(true);
        let validator = GameFormValidator {
            action: Arc::new(fixture.action(Action::FindGame)),
        };
        assert_eq!(
            validator
                .validate("Example Game", CancellationToken::new())
                .await,
            Ok("Example Game".into())
        );
        assert_eq!(
            validator
                .validate("Other Game", CancellationToken::new())
                .await,
            Err("Twitch game was not found".into())
        );
        assert_eq!(fixture.transport.requests.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn dropped_chat_message_is_an_action_failure() {
        let fixture = Fixture::new(false);
        let result = fixture
            .action(Action::SendChat)
            .execute(
                Values::from([("message".into(), json!("hello"))]),
                CancellationToken::new(),
            )
            .await;
        assert_eq!(
            result,
            Err(CapabilityError::Failed(
                "automod_held: Held for review".into()
            ))
        );
    }

    #[tokio::test]
    async fn channel_update_uses_broadcaster_and_only_requested_field() {
        let fixture = Fixture::new(true);
        fixture
            .action(Action::SetChannel)
            .execute(
                Values::from([("title".into(), json!("New title"))]),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        fixture
            .action(Action::SetChannel)
            .execute(
                Values::from([("game_id".into(), json!("42"))]),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let requests = fixture.transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].2, "broadcaster-token");
        assert_eq!(requests[0].3, json!({"title":"New title"}));
        assert_eq!(requests[1].2, "broadcaster-token");
        assert_eq!(requests[1].3, json!({"game_id":"42"}));
    }
}
