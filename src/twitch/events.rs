//! Typed decoding and workflow routing for Twitch EventSub notifications.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

use crate::WorkflowValues;
use crate::engine::Values;
use crate::schema::{ConfigOutput, DescribeValues};
use crate::workflows::{TriggerKind, WorkflowDefinition};

const TWITCH_INTEGRATION: &str = "twitch";
const MESSAGE_EVENT: &str = "chat.message";
const COMMAND_EVENT: &str = "chat.command";
const NOTIFICATION_EVENT: &str = "chat.notification";
const AD_BREAK_EVENT: &str = "ad_break.begin";
const CHANNEL_UPDATE_EVENT: &str = "channel.updated";
pub const DETAILS_CHANGED_EVENT: &str = "channel.details_changed";

pub fn trigger_title(kind: &TriggerKind) -> Option<&'static str> {
    let TriggerKind::IntegrationEvent {
        integration, event, ..
    } = kind
    else {
        return None;
    };
    if integration != TWITCH_INTEGRATION {
        return None;
    }
    Some(match event.as_str() {
        MESSAGE_EVENT => "Twitch chat message",
        COMMAND_EVENT => "Twitch chat command",
        NOTIFICATION_EVENT => "Twitch chat notification",
        AD_BREAK_EVENT => "Twitch ad break",
        CHANNEL_UPDATE_EVENT => "Twitch channel updated",
        DETAILS_CHANGED_EVENT => "Twitch title or game changed",
        _ => return None,
    })
}

pub fn trigger_value_schema(kind: &TriggerKind) -> Option<Vec<ConfigOutput>> {
    let TriggerKind::IntegrationEvent {
        integration, event, ..
    } = kind
    else {
        return None;
    };
    if integration != TWITCH_INTEGRATION {
        return None;
    }
    let outputs = match event.as_str() {
        MESSAGE_EVENT => ChatMessage::OUTPUTS.to_vec(),
        COMMAND_EVENT => {
            let mut outputs = ChatMessage::OUTPUTS.to_vec();
            outputs.extend(
                ChatCommandValues::OUTPUTS
                    .iter()
                    .filter(|output| output.id != "event")
                    .copied(),
            );
            outputs
        }
        NOTIFICATION_EVENT => ChatNotification::OUTPUTS.to_vec(),
        AD_BREAK_EVENT => AdBreakBegin::OUTPUTS.to_vec(),
        CHANNEL_UPDATE_EVENT | DETAILS_CHANGED_EVENT => ChannelUpdated::OUTPUTS.to_vec(),
        _ => return None,
    };
    Some(outputs)
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum EventSubKind {
    ChannelChatMessage,
    ChannelChatNotification,
    ChannelAdBreakBegin,
    ChannelUpdate,
}

impl EventSubKind {
    pub const fn event_type(self) -> &'static str {
        match self {
            Self::ChannelChatMessage => "channel.chat.message",
            Self::ChannelChatNotification => "channel.chat.notification",
            Self::ChannelAdBreakBegin => "channel.ad_break.begin",
            Self::ChannelUpdate => "channel.update",
        }
    }

    pub const fn event_name(self) -> &'static str {
        match self {
            Self::ChannelChatMessage => MESSAGE_EVENT,
            Self::ChannelChatNotification => NOTIFICATION_EVENT,
            Self::ChannelAdBreakBegin => AD_BREAK_EVENT,
            Self::ChannelUpdate => CHANNEL_UPDATE_EVENT,
        }
    }

    pub const fn version(self) -> &'static str {
        match self {
            Self::ChannelUpdate => "2",
            _ => "1",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TwitchEvent {
    Message(ChatMessage),
    Notification(ChatNotification),
    AdBreakBegin(AdBreakBegin),
    ChannelUpdated(ChannelUpdated),
    DetailsChanged(ChannelUpdated),
}

impl TwitchEvent {
    pub const fn kind(&self) -> EventSubKind {
        match self {
            Self::Message(_) => EventSubKind::ChannelChatMessage,
            Self::Notification(_) => EventSubKind::ChannelChatNotification,
            Self::AdBreakBegin(_) => EventSubKind::ChannelAdBreakBegin,
            Self::ChannelUpdated(_) | Self::DetailsChanged(_) => EventSubKind::ChannelUpdate,
        }
    }

    fn values(&self) -> Values {
        match self {
            Self::Message(message) => message.values(),
            Self::Notification(notification) => notification.values(),
            Self::AdBreakBegin(ad_break) => ad_break.values(),
            Self::ChannelUpdated(channel) => channel.values(),
            Self::DetailsChanged(channel) => {
                let mut values = channel.values();
                values.insert("event".into(), Value::String(DETAILS_CHANGED_EVENT.into()));
                values
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, WorkflowValues)]
#[workflow_values(event = "chat.message")]
pub struct ChatMessage {
    #[value(label = "Broadcaster user ID")]
    pub broadcaster_user_id: String,
    #[value(label = "Broadcaster login")]
    pub broadcaster_user_login: String,
    #[value(label = "Broadcaster name")]
    pub broadcaster_user_name: String,
    #[value(label = "Chatter user ID")]
    pub chatter_user_id: String,
    #[value(label = "Chatter login")]
    pub chatter_user_login: String,
    #[value(label = "Chatter name")]
    pub chatter_user_name: String,
    #[value(label = "Message ID")]
    pub message_id: String,
    #[value(label = "Message text")]
    pub text: String,
    #[value(label = "Source-only message")]
    pub is_source_only: bool,
    #[value(label = "Bot message")]
    pub is_bot: bool,
    #[value(label = "Broadcaster")]
    pub is_broadcaster: bool,
    #[value(label = "Moderator")]
    pub is_moderator: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, WorkflowValues)]
#[workflow_values(event = "chat.notification")]
pub struct ChatNotification {
    #[value(label = "Broadcaster user ID")]
    pub broadcaster_user_id: String,
    #[value(label = "Broadcaster login")]
    pub broadcaster_user_login: String,
    #[value(label = "Broadcaster name")]
    pub broadcaster_user_name: String,
    #[value(label = "Chatter user ID")]
    pub chatter_user_id: String,
    #[value(label = "Chatter login")]
    pub chatter_user_login: String,
    #[value(label = "Chatter name")]
    pub chatter_user_name: String,
    #[value(label = "Notification type")]
    pub notification_type: String,
    #[value(label = "Message ID")]
    pub message_id: String,
    #[value(label = "Notification text")]
    pub message_text: Option<String>,
    #[value(label = "Source-only message")]
    pub is_source_only: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, WorkflowValues)]
#[workflow_values(event = "ad_break.begin")]
pub struct AdBreakBegin {
    #[value(label = "Broadcaster user ID")]
    pub broadcaster_user_id: String,
    #[value(label = "Broadcaster login")]
    pub broadcaster_user_login: String,
    #[value(label = "Broadcaster name")]
    pub broadcaster_user_name: String,
    #[value(label = "Duration (seconds)")]
    pub duration_seconds: u64,
    #[value(label = "Started at")]
    pub started_at: String,
    #[value(label = "Automatic")]
    pub is_automatic: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, WorkflowValues)]
#[workflow_values(event = "channel.updated")]
pub struct ChannelUpdated {
    #[value(label = "Broadcaster user ID")]
    pub broadcaster_user_id: String,
    #[value(label = "Broadcaster login")]
    pub broadcaster_user_login: String,
    #[value(label = "Broadcaster name")]
    pub broadcaster_user_name: String,
    #[value(label = "Stream title")]
    pub title: String,
    #[value(label = "Game ID")]
    pub category_id: String,
    #[value(label = "Game")]
    pub category_name: String,
    #[value(label = "Stream language")]
    pub language: String,
}

#[derive(Clone, Debug, Eq, PartialEq, WorkflowValues)]
#[workflow_values(event = "chat.command")]
struct ChatCommandValues {
    #[value(label = "Command")]
    command: String,
    #[value(label = "Arguments")]
    arguments: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Route {
    pub workflow_id: String,
    pub trigger_id: String,
    pub values: Values,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RouteOptions<'a> {
    /// Messages authored by the connected bot account are ignored.
    pub bot_user_id: Option<&'a str>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DecodeError {
    Missing(&'static str),
    WrongType(&'static str),
    UnsupportedEvent { event_type: String, version: String },
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing(field) => write!(f, "missing EventSub field `{field}`"),
            Self::WrongType(field) => write!(f, "invalid EventSub field `{field}`"),
            Self::UnsupportedEvent {
                event_type,
                version,
            } => {
                write!(
                    f,
                    "unsupported EventSub event `{event_type}` version `{version}`"
                )
            }
        }
    }
}

impl std::error::Error for DecodeError {}

/// Decode a Twitch EventSub notification envelope for the supported v1 events.
pub fn decode_event(envelope: &Value) -> Result<TwitchEvent, DecodeError> {
    let subscription = object_at(envelope, "payload.subscription")?;
    let event = object_at(envelope, "payload.event")?;
    let event_type = string_at(subscription, "type", "payload.subscription.type")?;
    let version = string_at(subscription, "version", "payload.subscription.version")?;
    let kind = match (event_type.as_str(), version.as_str()) {
        ("channel.chat.message", "1") => EventSubKind::ChannelChatMessage,
        ("channel.chat.notification", "1") => EventSubKind::ChannelChatNotification,
        ("channel.ad_break.begin", "1") => EventSubKind::ChannelAdBreakBegin,
        ("channel.update", "2") => EventSubKind::ChannelUpdate,
        _ => {
            return Err(DecodeError::UnsupportedEvent {
                event_type,
                version,
            });
        }
    };
    match kind {
        EventSubKind::ChannelChatMessage => decode_message(event).map(TwitchEvent::Message),
        EventSubKind::ChannelChatNotification => {
            decode_notification(event).map(TwitchEvent::Notification)
        }
        EventSubKind::ChannelAdBreakBegin => decode_ad_break(event).map(TwitchEvent::AdBreakBegin),
        EventSubKind::ChannelUpdate => {
            decode_channel_update(event).map(TwitchEvent::ChannelUpdated)
        }
    }
}

/// Route a decoded event to enabled Twitch integration triggers.
pub fn route_event(
    event: &TwitchEvent,
    workflows: &[WorkflowDefinition],
    options: RouteOptions<'_>,
) -> Vec<Route> {
    let mut routes = Vec::new();
    for definition in workflows.iter().filter(|definition| definition.enabled) {
        for trigger in &definition.triggers {
            if !trigger.enabled {
                continue;
            }
            let TriggerKind::IntegrationEvent {
                integration,
                event: trigger_event,
                filters,
            } = &trigger.kind
            else {
                continue;
            };
            if integration != TWITCH_INTEGRATION {
                continue;
            }
            let is_command = trigger_event == COMMAND_EVENT;
            let event_matches = trigger_event
                == if matches!(event, TwitchEvent::DetailsChanged(_)) {
                    DETAILS_CHANGED_EVENT
                } else {
                    event.kind().event_name()
                }
                || (is_command && matches!(event, TwitchEvent::Message(_)));
            if !event_matches {
                continue;
            }
            if validate_trigger(trigger).is_err() {
                continue;
            }
            if source_only_or_bot(event, options.bot_user_id) {
                continue;
            }
            if let TwitchEvent::Notification(notification) = event
                && let Some(expected) = filters.get("notice_type").and_then(Value::as_str)
                && notification.notification_type != expected
            {
                continue;
            }
            if let (TwitchEvent::Message(message), true) = (event, is_command) {
                if filters
                    .get("broadcaster_or_moderator")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                    && !(message.is_broadcaster || message.is_moderator)
                {
                    continue;
                }
                let Some((command, arguments)) = matches_command(message, filters) else {
                    continue;
                };
                let mut values = message.values();
                values.extend(ChatCommandValues { command, arguments }.values());
                routes.push(Route {
                    workflow_id: definition.workflow.id.clone(),
                    trigger_id: trigger.id.clone(),
                    values,
                });
                continue;
            }
            routes.push(Route {
                workflow_id: definition.workflow.id.clone(),
                trigger_id: trigger.id.clone(),
                values: event.values(),
            });
        }
    }
    routes
}

fn source_only_or_bot(event: &TwitchEvent, bot_user_id: Option<&str>) -> bool {
    match event {
        TwitchEvent::Message(message) => {
            message.is_source_only
                || message.is_bot
                || bot_user_id.is_some_and(|bot_id| message.chatter_user_id == bot_id)
        }
        TwitchEvent::Notification(notification) => {
            notification.is_source_only
                || bot_user_id.is_some_and(|bot_id| notification.chatter_user_id == bot_id)
        }
        TwitchEvent::AdBreakBegin(_)
        | TwitchEvent::ChannelUpdated(_)
        | TwitchEvent::DetailsChanged(_) => false,
    }
}

/// Validate one Twitch trigger's stored filter schema. Other integrations are unaffected.
pub fn validate_trigger(trigger: &crate::workflows::TriggerDefinition) -> Result<(), String> {
    let TriggerKind::IntegrationEvent {
        integration,
        event,
        filters,
    } = &trigger.kind
    else {
        return Ok(());
    };
    if integration != TWITCH_INTEGRATION {
        return Ok(());
    }
    let allowed: &[&str] = match event.as_str() {
        MESSAGE_EVENT | AD_BREAK_EVENT | CHANNEL_UPDATE_EVENT | DETAILS_CHANGED_EVENT => &[],
        NOTIFICATION_EVENT => &["notice_type"],
        COMMAND_EVENT => &["command", "aliases", "broadcaster_or_moderator"],
        _ => return Err(format!("unsupported Twitch event `{event}`")),
    };
    if let Some(key) = filters.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(format!(
            "unsupported filter `{key}` for Twitch event `{event}`"
        ));
    }
    if event == COMMAND_EVENT {
        let Some(command) = filters.get("command").and_then(Value::as_str) else {
            return Err("Twitch chat.command requires a string `command` filter".into());
        };
        validate_command_name(command, "command")?;
        if let Some(aliases) = filters.get("aliases") {
            let Some(aliases) = aliases.as_array() else {
                return Err(
                    "Twitch chat.command `aliases` filter must be an array of strings".into(),
                );
            };
            for alias in aliases {
                let Some(alias) = alias.as_str() else {
                    return Err(
                        "Twitch chat.command `aliases` filter must be an array of strings".into(),
                    );
                };
                validate_command_name(alias, "alias")?;
            }
        }
        if let Some(permission) = filters.get("broadcaster_or_moderator")
            && !permission.is_boolean()
        {
            return Err(
                "Twitch chat.command `broadcaster_or_moderator` filter must be a boolean".into(),
            );
        }
    }
    if event == NOTIFICATION_EVENT
        && let Some(value) = filters.get("notice_type")
        && !value.as_str().is_some_and(|value| !value.is_empty())
    {
        return Err(
            "Twitch chat.notification `notice_type` filter must be a non-empty string".into(),
        );
    }
    Ok(())
}

/// Return the EventSub subscriptions needed by enabled Twitch workflow triggers.
pub fn required_kinds(
    definitions: &[WorkflowDefinition],
) -> Result<BTreeSet<EventSubKind>, String> {
    let mut required = BTreeSet::new();
    for definition in definitions.iter().filter(|definition| definition.enabled) {
        for trigger in &definition.triggers {
            if !trigger.enabled {
                continue;
            }
            let TriggerKind::IntegrationEvent {
                integration, event, ..
            } = &trigger.kind
            else {
                continue;
            };
            if integration != TWITCH_INTEGRATION {
                continue;
            }
            validate_trigger(trigger)?;
            required.insert(match event.as_str() {
                MESSAGE_EVENT | COMMAND_EVENT => EventSubKind::ChannelChatMessage,
                NOTIFICATION_EVENT => EventSubKind::ChannelChatNotification,
                AD_BREAK_EVENT => EventSubKind::ChannelAdBreakBegin,
                CHANNEL_UPDATE_EVENT | DETAILS_CHANGED_EVENT => EventSubKind::ChannelUpdate,
                _ => return Err(format!("unsupported Twitch event `{event}`")),
            });
        }
    }
    Ok(required)
}

fn validate_command_name(command: &str, label: &str) -> Result<(), String> {
    if command.is_empty()
        || command.chars().any(char::is_whitespace)
        || command.chars().any(char::is_control)
    {
        return Err(format!(
            "Twitch chat.command {label} must be a non-empty token without whitespace or control characters"
        ));
    }
    Ok(())
}

fn matches_command(
    message: &ChatMessage,
    filters: &BTreeMap<String, Value>,
) -> Option<(String, String)> {
    let mut commands = BTreeSet::new();
    if let Some(value) = filters.get("command").and_then(Value::as_str) {
        commands.insert(value.to_lowercase());
    }
    if let Some(aliases) = filters.get("aliases").and_then(Value::as_array) {
        commands.extend(
            aliases
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_lowercase),
        );
    }
    let text = message.text.as_str();
    commands.into_iter().find_map(|command| {
        let end = text
            .char_indices()
            .nth(command.chars().count())
            .map_or(text.len(), |(index, _)| index);
        let (candidate, suffix) = text.split_at(end);
        (candidate.to_lowercase() == command
            && (suffix.is_empty() || suffix.starts_with(char::is_whitespace)))
        .then(|| (candidate.to_owned(), suffix.trim_start().to_owned()))
    })
}

fn decode_message(event: &Map<String, Value>) -> Result<ChatMessage, DecodeError> {
    let message = object_at_value(event.get("message"), "payload.event.message")?;
    let message_text = string_at(message, "text", "payload.event.message.text")?;
    let chatter_id = string_at(event, "chatter_user_id", "payload.event.chatter_user_id")?;
    let broadcaster_id = string_at(
        event,
        "broadcaster_user_id",
        "payload.event.broadcaster_user_id",
    )?;
    let badges = event.get("badges").and_then(Value::as_array);
    let is_broadcaster = chatter_id == broadcaster_id || has_badge(badges, "broadcaster");
    let is_moderator = has_badge(badges, "moderator");
    Ok(ChatMessage {
        broadcaster_user_id: broadcaster_id,
        broadcaster_user_login: string_at(
            event,
            "broadcaster_user_login",
            "payload.event.broadcaster_user_login",
        )?,
        broadcaster_user_name: string_at(
            event,
            "broadcaster_user_name",
            "payload.event.broadcaster_user_name",
        )?,
        chatter_user_id: chatter_id,
        chatter_user_login: string_at(
            event,
            "chatter_user_login",
            "payload.event.chatter_user_login",
        )?,
        chatter_user_name: string_at(
            event,
            "chatter_user_name",
            "payload.event.chatter_user_name",
        )?,
        message_id: string_at(event, "message_id", "payload.event.message_id")?,
        text: message_text,
        is_source_only: optional_bool_at(event, "is_source_only", "payload.event.is_source_only")?,
        is_bot: event.get("message_type").and_then(Value::as_str) == Some("bot_message"),
        is_broadcaster,
        is_moderator,
    })
}

fn decode_channel_update(event: &Map<String, Value>) -> Result<ChannelUpdated, DecodeError> {
    Ok(ChannelUpdated {
        broadcaster_user_id: string_at(
            event,
            "broadcaster_user_id",
            "payload.event.broadcaster_user_id",
        )?,
        broadcaster_user_login: string_at(
            event,
            "broadcaster_user_login",
            "payload.event.broadcaster_user_login",
        )?,
        broadcaster_user_name: string_at(
            event,
            "broadcaster_user_name",
            "payload.event.broadcaster_user_name",
        )?,
        title: string_at(event, "title", "payload.event.title")?,
        category_id: string_at(event, "category_id", "payload.event.category_id")?,
        category_name: string_at(event, "category_name", "payload.event.category_name")?,
        language: string_at(event, "language", "payload.event.language")?,
    })
}

fn decode_notification(event: &Map<String, Value>) -> Result<ChatNotification, DecodeError> {
    let message = optional_object(event, "message", "payload.event.message")?;
    let chatter_id = string_at(event, "chatter_user_id", "payload.event.chatter_user_id")?;
    Ok(ChatNotification {
        broadcaster_user_id: string_at(
            event,
            "broadcaster_user_id",
            "payload.event.broadcaster_user_id",
        )?,
        broadcaster_user_login: string_at(
            event,
            "broadcaster_user_login",
            "payload.event.broadcaster_user_login",
        )?,
        broadcaster_user_name: string_at(
            event,
            "broadcaster_user_name",
            "payload.event.broadcaster_user_name",
        )?,
        chatter_user_id: chatter_id,
        chatter_user_login: string_at(
            event,
            "chatter_user_login",
            "payload.event.chatter_user_login",
        )?,
        chatter_user_name: string_at(
            event,
            "chatter_user_name",
            "payload.event.chatter_user_name",
        )?,
        notification_type: string_at(event, "notice_type", "payload.event.notice_type")?,
        message_id: string_at(event, "message_id", "payload.event.message_id")?,
        message_text: optional_string(message, "text", "payload.event.message.text")?,
        is_source_only: optional_bool_at(event, "is_source_only", "payload.event.is_source_only")?,
    })
}

fn decode_ad_break(event: &Map<String, Value>) -> Result<AdBreakBegin, DecodeError> {
    Ok(AdBreakBegin {
        broadcaster_user_id: string_at(
            event,
            "broadcaster_user_id",
            "payload.event.broadcaster_user_id",
        )?,
        broadcaster_user_login: string_at(
            event,
            "broadcaster_user_login",
            "payload.event.broadcaster_user_login",
        )?,
        broadcaster_user_name: string_at(
            event,
            "broadcaster_user_name",
            "payload.event.broadcaster_user_name",
        )?,
        duration_seconds: u64_at(event, "duration_seconds", "payload.event.duration_seconds")?,
        started_at: string_at(event, "started_at", "payload.event.started_at")?,
        is_automatic: bool_or_string_at(event, "is_automatic", "payload.event.is_automatic")?,
    })
}

fn has_badge(badges: Option<&Vec<Value>>, badge: &str) -> bool {
    badges.is_some_and(|badges| {
        badges
            .iter()
            .any(|item| item.get("set_id").and_then(Value::as_str) == Some(badge))
    })
}

fn object_at<'a>(
    value: &'a Value,
    path: &'static str,
) -> Result<&'a Map<String, Value>, DecodeError> {
    let mut current = value;
    for component in path.split('.') {
        current = current.get(component).ok_or(DecodeError::Missing(path))?;
    }
    current.as_object().ok_or(DecodeError::WrongType(path))
}

fn object_at_value<'a>(
    value: Option<&'a Value>,
    path: &'static str,
) -> Result<&'a Map<String, Value>, DecodeError> {
    value
        .ok_or(DecodeError::Missing(path))?
        .as_object()
        .ok_or(DecodeError::WrongType(path))
}

fn string_at(
    object: &Map<String, Value>,
    key: &'static str,
    path: &'static str,
) -> Result<String, DecodeError> {
    object
        .get(key)
        .ok_or(DecodeError::Missing(path))?
        .as_str()
        .map(str::to_owned)
        .ok_or(DecodeError::WrongType(path))
}

fn optional_string(
    object: Option<&Map<String, Value>>,
    key: &'static str,
    path: &'static str,
) -> Result<Option<String>, DecodeError> {
    match object.and_then(|object| object.get(key)) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(DecodeError::WrongType(path)),
    }
}

fn optional_object<'a>(
    object: &'a Map<String, Value>,
    key: &'static str,
    path: &'static str,
) -> Result<Option<&'a Map<String, Value>>, DecodeError> {
    match object.get(key) {
        Some(Value::Object(value)) => Ok(Some(value)),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(DecodeError::WrongType(path)),
    }
}

fn optional_bool_at(
    object: &Map<String, Value>,
    key: &'static str,
    path: &'static str,
) -> Result<bool, DecodeError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(DecodeError::WrongType(path)),
    }
}

fn bool_or_string_at(
    object: &Map<String, Value>,
    key: &'static str,
    path: &'static str,
) -> Result<bool, DecodeError> {
    match object.get(key).ok_or(DecodeError::Missing(path))? {
        Value::Bool(value) => Ok(*value),
        Value::String(value) if value == "true" => Ok(true),
        Value::String(value) if value == "false" => Ok(false),
        _ => Err(DecodeError::WrongType(path)),
    }
}

fn u64_at(
    object: &Map<String, Value>,
    key: &'static str,
    path: &'static str,
) -> Result<u64, DecodeError> {
    match object.get(key).ok_or(DecodeError::Missing(path))? {
        Value::Number(value) => value.as_u64().ok_or(DecodeError::WrongType(path)),
        Value::String(value) => value.parse().map_err(|_| DecodeError::WrongType(path)),
        _ => Err(DecodeError::WrongType(path)),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::json;

    use super::{
        DecodeError, EventSubKind, RouteOptions, TwitchEvent, decode_event, required_kinds,
        route_event, validate_trigger,
    };
    use crate::engine::Workflow;
    use crate::schema::ConfigOutputKind;
    use crate::workflows::{TriggerDefinition, TriggerKind, WorkflowDefinition};

    fn envelope(event_type: &str, event: serde_json::Value) -> serde_json::Value {
        json!({
            "metadata": {"message_type": "notification"},
            "payload": {
                "subscription": {"type": event_type, "version": "1"},
                "event": event
            }
        })
    }

    fn message(text: &str, chatter_id: &str, badges: serde_json::Value) -> serde_json::Value {
        envelope(
            "channel.chat.message",
            json!({
                "broadcaster_user_id": "100", "broadcaster_user_login": "owner",
                "broadcaster_user_name": "Owner", "chatter_user_id": chatter_id,
                "chatter_user_login": "viewer", "chatter_user_name": "Viewer",
                "message_id": "msg-1", "message": {"text": text, "fragments": []},
                "message_type": "text", "is_source_only": false, "badges": badges
            }),
        )
    }

    fn workflow(trigger_id: &str, event: &str, filters: serde_json::Value) -> WorkflowDefinition {
        WorkflowDefinition {
            enabled: true,
            workflow: Workflow {
                id: "flow".into(),
                revision: 0,
                overlap: false,
                steps: vec![],
                outputs: Default::default(),
            },
            triggers: vec![TriggerDefinition {
                id: trigger_id.into(),
                enabled: true,
                kind: TriggerKind::IntegrationEvent {
                    integration: "twitch".into(),
                    event: event.into(),
                    filters: filters.as_object().unwrap().clone().into_iter().collect(),
                },
            }],
            name: None,
        }
    }

    #[test]
    fn decodes_message_notification_and_ad_break() {
        let chat = decode_event(&message("hello", "200", json!([]))).unwrap();
        assert!(matches!(chat, TwitchEvent::Message(ref m) if m.text == "hello"));

        let notice = decode_event(&envelope("channel.chat.notification", json!({
            "broadcaster_user_id":"100", "broadcaster_user_login":"owner", "broadcaster_user_name":"Owner",
            "chatter_user_id":"200", "chatter_user_login":"viewer", "chatter_user_name":"Viewer",
            "notice_type":"sub", "message_id":"notice-1", "message":{"text":"Joined!"},
            "is_source_only":null
        }))).unwrap();
        assert!(
            matches!(notice, TwitchEvent::Notification(ref n) if n.notification_type == "sub" && n.message_text.as_deref() == Some("Joined!"))
        );

        let ad = decode_event(&envelope("channel.ad_break.begin", json!({
            "broadcaster_user_id":"100", "broadcaster_user_login":"owner", "broadcaster_user_name":"Owner",
            "duration_seconds":"90", "started_at":"2026-01-01T00:00:00Z", "is_automatic":"false"
        }))).unwrap();
        assert!(matches!(ad, TwitchEvent::AdBreakBegin(ref a) if a.duration_seconds == 90));
        assert_eq!(ad.kind(), EventSubKind::ChannelAdBreakBegin);
    }

    #[test]
    fn nullable_source_flag_preserves_messages_and_rejects_wrong_types() {
        let mut envelope = message("!hello", "200", json!([]));
        envelope["payload"]["event"]["is_source_only"] = serde_json::Value::Null;
        assert!(
            matches!(decode_event(&envelope).unwrap(), TwitchEvent::Message(message) if !message.is_source_only)
        );
        envelope["payload"]["event"]
            .as_object_mut()
            .unwrap()
            .remove("is_source_only");
        assert!(decode_event(&envelope).is_ok());
        envelope["payload"]["event"]["is_source_only"] = json!("false");
        assert_eq!(
            decode_event(&envelope),
            Err(DecodeError::WrongType("payload.event.is_source_only"))
        );
    }

    #[test]
    fn channel_update_v2_routes_typed_values_and_shares_one_subscription() {
        let mut envelope = envelope(
            "channel.update",
            json!({
                "broadcaster_user_id":"100", "broadcaster_user_login":"owner", "broadcaster_user_name":"Owner",
                "title":"Hello 🐉", "category_id":"1", "category_name":"Deadlock", "language":"en"
            }),
        );
        envelope["payload"]["subscription"]["version"] = json!("2");
        let event = decode_event(&envelope).unwrap();
        let definitions = [
            workflow("updated", "channel.updated", json!({})),
            workflow("second", "channel.updated", json!({})),
            workflow("settled", super::DETAILS_CHANGED_EVENT, json!({})),
        ];
        let routes = route_event(&event, &definitions, RouteOptions::default());
        assert_eq!(routes.len(), 2);
        assert_eq!(routes[0].values["title"], json!("Hello 🐉"));
        assert_eq!(routes[0].values["category_name"], json!("Deadlock"));
        assert_eq!(
            required_kinds(&definitions).unwrap(),
            BTreeSet::from([EventSubKind::ChannelUpdate])
        );
        let TwitchEvent::ChannelUpdated(channel) = event else {
            panic!("wrong event")
        };
        let settled = route_event(
            &TwitchEvent::DetailsChanged(channel),
            &definitions,
            RouteOptions::default(),
        );
        assert_eq!(settled.len(), 1);
        assert_eq!(settled[0].trigger_id, "settled");
        assert_eq!(settled[0].values["event"], super::DETAILS_CHANGED_EVENT);
        assert_eq!(EventSubKind::ChannelUpdate.version(), "2");
        envelope["payload"]["subscription"]["version"] = json!("1");
        assert!(matches!(
            decode_event(&envelope),
            Err(DecodeError::UnsupportedEvent { .. })
        ));
    }

    #[test]
    fn rejects_malformed_and_unsupported_payloads() {
        assert_eq!(
            decode_event(&json!({})),
            Err(DecodeError::Missing("payload.subscription"))
        );
        let missing_text = message("hello", "200", json!([]));
        let mut missing_text = missing_text;
        missing_text["payload"]["event"]["message"]["text"] = json!(5);
        assert_eq!(
            decode_event(&missing_text),
            Err(DecodeError::WrongType("payload.event.message.text"))
        );
        let unsupported = envelope("channel.chat.message", json!({}));
        let mut unsupported = unsupported;
        unsupported["payload"]["subscription"]["version"] = json!("2");
        assert!(matches!(
            decode_event(&unsupported),
            Err(DecodeError::UnsupportedEvent { .. })
        ));
    }

    #[test]
    fn routes_commands_with_case_insensitive_aliases_and_unicode_text() {
        let definition = workflow(
            "cmd",
            "chat.command",
            json!({"command":"!salut", "aliases":["!yo"]}),
        );
        for input in ["!SALUT hello 🌍", "!yo naïve café"] {
            let event = decode_event(&message(input, "200", json!([]))).unwrap();
            let routes = route_event(
                &event,
                std::slice::from_ref(&definition),
                RouteOptions::default(),
            );
            assert_eq!(routes.len(), 1);
            assert_eq!(routes[0].trigger_id, "cmd");
            assert_eq!(routes[0].values["text"], input);
            assert_eq!(routes[0].values["event"], "chat.command");
            assert_eq!(
                routes[0].values["command"],
                if input.starts_with("!SALUT") {
                    "!SALUT"
                } else {
                    "!yo"
                }
            );
            assert_eq!(
                routes[0].values["arguments"],
                if input.starts_with("!SALUT") {
                    "hello 🌍"
                } else {
                    "naïve café"
                }
            );
        }
        let indented = decode_event(&message(" !salut", "200", json!([]))).unwrap();
        assert!(
            route_event(
                &indented,
                std::slice::from_ref(&definition),
                RouteOptions::default()
            )
            .is_empty()
        );
        let exact = decode_event(&message("!salute", "200", json!([]))).unwrap();
        assert!(route_event(&exact, &[definition], RouteOptions::default()).is_empty());
    }

    #[test]
    fn value_descriptors_match_routed_message_and_command_values() {
        for (event_name, filters, text) in [
            ("chat.message", json!({}), "hello"),
            ("chat.command", json!({"command":"!go"}), "!go now"),
        ] {
            let definition = workflow("schema", event_name, filters);
            let event = decode_event(&message(text, "200", json!([]))).unwrap();
            let routes = route_event(
                &event,
                std::slice::from_ref(&definition),
                RouteOptions::default(),
            );
            let values = &routes[0].values;
            let outputs = super::trigger_value_schema(&definition.triggers[0].kind).unwrap();
            assert_eq!(
                outputs
                    .iter()
                    .map(|output| output.id)
                    .collect::<BTreeSet<_>>(),
                values.keys().map(String::as_str).collect::<BTreeSet<_>>()
            );
            for output in outputs {
                let value = &values[output.id];
                assert!(match output.kind {
                    ConfigOutputKind::Text => value.is_string(),
                    ConfigOutputKind::Number => value.is_number(),
                    ConfigOutputKind::Toggle => value.is_boolean(),
                });
            }
        }
        let notification = decode_event(&envelope("channel.chat.notification", json!({
            "broadcaster_user_id":"100", "broadcaster_user_login":"owner", "broadcaster_user_name":"Owner",
            "chatter_user_id":"200", "chatter_user_login":"viewer", "chatter_user_name":"Viewer",
            "notice_type":"sub", "message_id":"notice-1"
        }))).unwrap();
        let definition = workflow("notice", "chat.notification", json!({}));
        let route = route_event(
            &notification,
            std::slice::from_ref(&definition),
            RouteOptions::default(),
        );
        let outputs = super::trigger_value_schema(&definition.triggers[0].kind).unwrap();
        assert_eq!(route[0].values["message_text"], serde_json::Value::Null);
        assert!(
            !outputs
                .iter()
                .find(|output| output.id == "message_text")
                .unwrap()
                .required
        );
        assert_eq!(
            outputs
                .iter()
                .map(|output| output.id)
                .collect::<BTreeSet<_>>(),
            route[0]
                .values
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
        );
    }

    #[test]
    fn all_message_routes_filter_bot_internal_and_permissions() {
        let command = workflow(
            "cmd",
            "chat.command",
            json!({
                "command":"!go", "broadcaster_or_moderator":true
            }),
        );
        let general = workflow("all", "chat.message", json!({}));
        let workflows = [command, general];

        let ordinary = decode_event(&message("!go", "200", json!([]))).unwrap();
        let routes = route_event(
            &ordinary,
            &workflows,
            RouteOptions {
                bot_user_id: Some("bot-9"),
            },
        );
        assert_eq!(
            routes
                .iter()
                .map(|route| route.trigger_id.as_str())
                .collect::<Vec<_>>(),
            ["all"]
        );

        let moderator = decode_event(&message(
            "!go args",
            "200",
            json!([{"set_id":"moderator","id":"1"}]),
        ))
        .unwrap();
        let routes = route_event(
            &moderator,
            &workflows,
            RouteOptions {
                bot_user_id: Some("bot-9"),
            },
        );
        assert_eq!(
            routes
                .iter()
                .map(|route| route.trigger_id.as_str())
                .collect::<Vec<_>>(),
            ["cmd", "all"]
        );

        let bot = decode_event(&message("!go", "bot-9", json!([]))).unwrap();
        let routes = route_event(
            &bot,
            &workflows,
            RouteOptions {
                bot_user_id: Some("bot-9"),
            },
        );
        assert!(routes.is_empty());

        let mut internal = message("!go", "200", json!([]));
        internal["payload"]["event"]["is_source_only"] = json!(true);
        let internal = decode_event(&internal).unwrap();
        let routes = route_event(&internal, &workflows, RouteOptions::default());
        assert!(routes.is_empty());
    }

    #[test]
    fn duplicate_command_aliases_still_route_once_per_trigger() {
        let definition = workflow(
            "cmd",
            "chat.command",
            json!({"command":"!go", "aliases":["!GO", "!go"]}),
        );
        let event = decode_event(&message("!go do it", "200", json!([]))).unwrap();
        let routes = route_event(&event, &[definition], RouteOptions::default());
        assert_eq!(routes.len(), 1);
    }

    #[test]
    fn routes_skip_source_only_and_bot_messages_and_notices() {
        let message_trigger = workflow("message", "chat.message", json!({}));
        let notice_trigger = workflow("notice", "chat.notification", json!({}));
        let workflows = [message_trigger, notice_trigger];

        let message = decode_event(&message("hello", "bot-9", json!([]))).unwrap();
        assert!(
            route_event(
                &message,
                &workflows,
                RouteOptions {
                    bot_user_id: Some("bot-9")
                }
            )
            .is_empty()
        );

        let notice = decode_event(&envelope("channel.chat.notification", json!({
            "broadcaster_user_id":"100", "broadcaster_user_login":"owner", "broadcaster_user_name":"Owner",
            "chatter_user_id":"200", "chatter_user_login":"viewer", "chatter_user_name":"Viewer",
            "notice_type":"resub", "message_id":"notice-1", "message":{"text":""},
            "is_source_only":true
        }))).unwrap();
        assert!(route_event(&notice, &workflows, RouteOptions::default()).is_empty());

        let bot_notice = decode_event(&envelope("channel.chat.notification", json!({
            "broadcaster_user_id":"100", "broadcaster_user_login":"owner", "broadcaster_user_name":"Owner",
            "chatter_user_id":"bot-9", "chatter_user_login":"bot", "chatter_user_name":"Bot",
            "notice_type":"resub", "message_id":"notice-2", "message":{"text":""},
            "is_source_only":null
        }))).unwrap();
        assert!(
            route_event(
                &bot_notice,
                &workflows,
                RouteOptions {
                    bot_user_id: Some("bot-9")
                }
            )
            .is_empty()
        );
    }

    #[test]
    fn trigger_validation_rejects_unknown_and_ill_typed_filters() {
        let valid = workflow(
            "cmd",
            "chat.command",
            json!({
                "command":"!go", "aliases":["!start"], "broadcaster_or_moderator":true
            }),
        );
        assert!(validate_trigger(&valid.triggers[0]).is_ok());

        for filters in [
            json!({"command":"!go", "extra":true}),
            json!({"command":42}),
            json!({"command":"!go", "aliases":["!ok", 3]}),
            json!({"command":"!go", "broadcaster_or_moderator":"yes"}),
            json!({"command":"!bad command"}),
            json!({"aliases":["!go"]}),
        ] {
            let definition = workflow("bad", "chat.command", filters);
            assert!(validate_trigger(&definition.triggers[0]).is_err());
        }

        let unsupported = workflow("bad", "chat.unknown", json!({}));
        assert!(validate_trigger(&unsupported.triggers[0]).is_err());
        let malformed_general = workflow("bad", "chat.message", json!({"unused":true}));
        assert!(validate_trigger(&malformed_general.triggers[0]).is_err());
        let malformed_notice = workflow("bad", "chat.notification", json!({"notice_type": 4}));
        assert!(validate_trigger(&malformed_notice.triggers[0]).is_err());
    }

    #[test]
    fn notification_type_filter_routes_only_matching_notice() {
        let definition = workflow("notice", "chat.notification", json!({"notice_type":"sub"}));
        let event = decode_event(&envelope("channel.chat.notification", json!({
            "broadcaster_user_id":"100", "broadcaster_user_login":"owner", "broadcaster_user_name":"Owner",
            "chatter_user_id":"200", "chatter_user_login":"viewer", "chatter_user_name":"Viewer",
            "notice_type":"resub", "message_id":"notice-1", "message":{"text":""}
        }))).unwrap();
        assert!(route_event(&event, &[definition], RouteOptions::default()).is_empty());
    }

    #[test]
    fn required_kinds_deduplicates_supported_subscriptions_and_rejects_invalid_enabled_trigger() {
        let definitions = [
            workflow("message", "chat.message", json!({})),
            workflow("command", "chat.command", json!({"command":"!go"})),
            workflow("notice", "chat.notification", json!({})),
            workflow("ad", "ad_break.begin", json!({})),
        ];
        assert_eq!(required_kinds(&definitions).unwrap().len(), 3);

        let invalid = workflow("invalid", "chat.command", json!({"command":true}));
        assert!(required_kinds(&[invalid]).is_err());
    }
}
