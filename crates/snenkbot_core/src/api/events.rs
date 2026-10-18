//! Bounded overlay event state, replay, and Twitch subscription demand.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc, watch};
use tokio_util::sync::CancellationToken;

use super::ApiError;
use crate::migration::StoredConfig;
use crate::paths::AppPaths;
use crate::storage::{ConfigStore, StoreError};
use uuid::Uuid;

use crate::twitch::events::TwitchEvent;

const CHAT_LIMIT: usize = 40;
const LIVE_LIMIT: usize = 1024;
const TARGETED_LIMIT: usize = 1024;
const HISTORY_DEFINITION: &str = "snenkbot.api.support_history";
/// A broadcaster-scoped durable write; no event means retention-only trimming.
type HistoryWrite = (String, Option<Value>);

/// Wire frame sent through the event stream.
#[derive(Clone, Debug, Serialize)]
pub struct EventFrame {
    pub kind: &'static str,
    pub channel: String,
    pub cursor: String,
    pub data: Value,
    #[serde(skip)]
    recipient: Option<String>,
}

impl EventFrame {
    /// A targeted replay is visible only to its named authenticated consumer.
    pub fn visible_to(&self, consumer_id: &str) -> bool {
        self.recipient
            .as_deref()
            .is_none_or(|recipient| recipient == consumer_id)
    }
}

/// Snapshot and live receiver captured at the same event boundary.
pub struct EventSubscription {
    pub channels: BTreeSet<String>,
    pub initial: Vec<EventFrame>,
    pub receiver: broadcast::Receiver<EventFrame>,
    _lease: DemandLease,
}

/// Removes one stream's demand when its body is dropped.
struct DemandLease {
    hub: Arc<EventHub>,
    channels: BTreeSet<String>,
}

impl Drop for DemandLease {
    /// Releases only the channels held by this stream.
    fn drop(&mut self) {
        let mut state = self.hub.state.lock().expect("overlay event lock poisoned");
        for channel in &self.channels {
            if let Some(count) = state.demand.get_mut(channel) {
                *count -= 1;
                if *count == 0 {
                    state.demand.remove(channel);
                }
            }
        }
        self.hub.notify_demand(&state);
    }
}

/// Current snapshot and bounded replay window for one event channel.
struct ChannelState {
    sequence: u64,
    pruned_until: u64,
    snapshot_sequence: u64,
    history: VecDeque<EventFrame>,
    snapshot: Value,
}

impl ChannelState {
    /// Starts a channel with its documented empty snapshot shape.
    fn new(channel: &str) -> Self {
        Self {
            sequence: 0,
            pruned_until: 0,
            snapshot_sequence: 0,
            history: VecDeque::new(),
            snapshot: match channel {
                "twitch.channel" => Value::Null,
                _ => json!([]),
            },
        }
    }
}

/// Synchronizes broadcaster identity, cursors, and demand under one lock.
struct State {
    epoch: String,
    channels: BTreeMap<String, ChannelState>,
    targeted: VecDeque<EventFrame>,
    targeted_pruned_until: BTreeMap<(String, String), u64>,
    demand: BTreeMap<String, usize>,
    support_retention: usize,
    broadcaster_id: Option<String>,
    history_load_fault: bool,
}

/// Holds demanded live channels and automatically recorded support events.
pub struct EventHub {
    state: Mutex<State>,
    sender: broadcast::Sender<EventFrame>,
    demand: watch::Sender<BTreeSet<String>>,
    demand_revision: watch::Sender<u64>,
    history: ConfigStore,
    history_lock: Arc<Mutex<()>>,
    history_tx: mpsc::Sender<HistoryWrite>,
    history_rx: Mutex<Option<mpsc::Receiver<HistoryWrite>>>,
    pending_trims: Mutex<BTreeSet<String>>,
    history_unavailable: watch::Sender<bool>,
    writer_fault: AtomicBool,
}

impl EventHub {
    /// Creates an empty epoch with bounded replay history.
    pub fn new(paths: &AppPaths, support_retention: usize) -> Arc<Self> {
        let (sender, _) = broadcast::channel(LIVE_LIMIT);
        let (demand, _) = watch::channel(BTreeSet::from(["twitch.support".into()]));
        let (demand_revision, _) = watch::channel(0);
        let (history_tx, history_rx) = mpsc::channel(256);
        let (history_unavailable, _) = watch::channel(false);
        let history = ConfigStore::new(paths.state.join("api/support_history"));
        let channels = BTreeMap::new();
        Arc::new(Self {
            state: Mutex::new(State {
                epoch: Uuid::new_v4().simple().to_string(),
                channels,
                targeted: VecDeque::new(),
                targeted_pruned_until: BTreeMap::new(),
                demand: BTreeMap::new(),
                support_retention,
                broadcaster_id: None,
                history_load_fault: false,
            }),
            sender,
            demand,
            demand_revision,
            history,
            history_lock: Arc::new(Mutex::new(())),
            history_tx,
            history_rx: Mutex::new(Some(history_rx)),
            pending_trims: Mutex::new(BTreeSet::new()),
            history_unavailable,
            writer_fault: AtomicBool::new(false),
        })
    }

    /// Watches durable storage failures without exposing broadcaster data.
    pub fn subscribe_history_unavailable(&self) -> watch::Receiver<bool> {
        self.history_unavailable.subscribe()
    }

    /// Drops a revoked consumer's private replay buffer and gap markers.
    pub fn forget_consumer(&self, consumer_id: &str) {
        let mut state = self.state.lock().expect("overlay event lock poisoned");
        state
            .targeted
            .retain(|frame| frame.recipient.as_deref() != Some(consumer_id));
        state
            .targeted_pruned_until
            .retain(|(recipient, _), _| recipient != consumer_id);
    }

    /// Drops the prior broadcaster's in-memory snapshot when the configured account changes.
    pub fn clear_if_other_broadcaster(&self, configured_id: Option<&str>) {
        let mut state = self.state.lock().expect("overlay event lock poisoned");
        if state.broadcaster_id.is_none() || state.broadcaster_id.as_deref() == configured_id {
            return;
        }
        state.broadcaster_id = None;
        state.history_load_fault = false;
        state.epoch = Uuid::new_v4().simple().to_string();
        state.channels.clear();
        state.targeted.clear();
        state.targeted_pruned_until.clear();
        for channel in state.demand.keys() {
            let _ = self.sender.send(EventFrame {
                kind: "reset",
                channel: channel.clone(),
                cursor: format!("{}:0", state.epoch),
                data: ChannelState::new(channel).snapshot,
                recipient: None,
            });
        }
        drop(state);
        self.refresh_history_health();
    }

    /// Reports either a failed account-history load or a failed queued write.
    fn refresh_history_health(&self) {
        let load_fault = self
            .state
            .lock()
            .expect("overlay event lock poisoned")
            .history_load_fault;
        self.history_unavailable
            .send_replace(load_fault || self.writer_fault.load(Ordering::Relaxed));
    }

    /// Flushes accepted support events through the versioned store in arrival order.
    pub async fn run_history(&self, shutdown: CancellationToken) -> Result<(), ApiError> {
        let mut receiver = self
            .history_rx
            .lock()
            .expect("history receiver lock poisoned")
            .take()
            .expect("history writer started once");
        loop {
            let pending_trim = self
                .pending_trims
                .lock()
                .expect("pending trim lock poisoned")
                .pop_first();
            let value = if let Some(broadcaster_id) = pending_trim {
                Some((broadcaster_id, None))
            } else {
                tokio::select! {
                    _ = shutdown.cancelled() => { receiver.close(); receiver.recv().await },
                    value = receiver.recv() => value,
                }
            };
            let Some((broadcaster_id, value)) = value else {
                break;
            };
            loop {
                let store = self.history.clone();
                let history_lock = Arc::clone(&self.history_lock);
                let id = broadcaster_id.clone();
                let event = value.clone();
                let retention = self
                    .state
                    .lock()
                    .expect("overlay event lock poisoned")
                    .support_retention;
                let result = tokio::task::spawn_blocking(move || {
                    let _guard = history_lock.lock().expect("support history lock poisoned");
                    persist_history(&store, &id, event, retention)
                })
                .await
                .map_err(|_| ApiError::Invalid("support history worker stopped".into()))
                .and_then(|result| result);
                match result {
                    Ok(()) => {
                        self.writer_fault.store(false, Ordering::Relaxed);
                        self.refresh_history_health();
                        break;
                    }
                    Err(error) => {
                        self.writer_fault.store(true, Ordering::Relaxed);
                        self.refresh_history_health();
                        tokio::select! {
                            _ = shutdown.cancelled() => return Err(error),
                            _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {},
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Switches the active broadcaster and resets all live state before replaying its history.
    pub fn set_broadcaster(&self, broadcaster_id: &str) -> Result<(), ApiError> {
        if broadcaster_id.is_empty() || !broadcaster_id.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(ApiError::Invalid("invalid broadcaster ID".into()));
        }
        {
            let mut state = self.state.lock().expect("overlay event lock poisoned");
            if state.broadcaster_id.as_deref() == Some(broadcaster_id) && !state.history_load_fault
            {
                return Ok(());
            }
            state.broadcaster_id = Some(broadcaster_id.to_owned());
            state.history_load_fault = false;
            state.epoch = Uuid::new_v4().simple().to_string();
            state.channels.clear();
            state.targeted.clear();
            state.targeted_pruned_until.clear();
            for channel in state.demand.keys() {
                let _ = self.sender.send(EventFrame {
                    kind: "reset",
                    channel: channel.clone(),
                    cursor: format!("{}:0", state.epoch),
                    data: ChannelState::new(channel).snapshot,
                    recipient: None,
                });
            }
        }
        let saved = {
            let _guard = self
                .history_lock
                .lock()
                .expect("support history lock poisoned");
            load_history(&self.history, broadcaster_id)
        };
        let saved = match saved {
            Ok(saved) => saved,
            Err(error) => {
                let mut state = self.state.lock().expect("overlay event lock poisoned");
                if state.broadcaster_id.as_deref() == Some(broadcaster_id) {
                    state.history_load_fault = true;
                }
                drop(state);
                self.refresh_history_health();
                return Err(error);
            }
        };
        let mut state = self.state.lock().expect("overlay event lock poisoned");
        if state.broadcaster_id.as_deref() != Some(broadcaster_id) {
            return Ok(());
        }
        state.history_load_fault = false;
        let mut support = ChannelState::new("twitch.support");
        support.snapshot = Value::Array(
            saved
                .into_iter()
                .rev()
                .take(state.support_retention)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect(),
        );
        state.channels.insert("twitch.support".into(), support);
        for channel in state.demand.keys() {
            let snapshot = state
                .channels
                .get(channel)
                .map(|entry| entry.snapshot.clone())
                .unwrap_or_else(|| ChannelState::new(channel).snapshot);
            let _ = self.sender.send(EventFrame {
                kind: "reset",
                channel: channel.clone(),
                cursor: format!("{}:0", state.epoch),
                data: snapshot,
                recipient: None,
            });
        }
        drop(state);
        self.refresh_history_health();
        Ok(())
    }

    /// Imports validated legacy support events without touching consumer secrets.
    pub fn import_support_history(&self, events: Vec<Value>) -> Result<usize, ApiError> {
        if events.len() > 10_000 {
            return Err(ApiError::Invalid("too many support events".into()));
        }
        let mut groups: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for event in events {
            let parsed: ImportedSupportEvent = serde_json::from_value(event.clone())
                .map_err(|_| ApiError::Invalid("invalid support event".into()))?;
            if parsed.id.is_empty()
                || parsed.id.len() > 128
                || parsed.channel_id.is_empty()
                || !parsed.channel_id.bytes().all(|byte| byte.is_ascii_digit())
                || parsed.user.len() > 128
                || parsed.tier.as_ref().is_some_and(|tier| tier.len() > 128)
                || parsed
                    .message
                    .as_ref()
                    .is_some_and(|message| message.len() > 4096)
                || chrono::DateTime::parse_from_rfc3339(&parsed.timestamp).is_err()
                || parsed
                    .amount
                    .is_some_and(|amount| !amount.is_finite() || amount < 0.0)
            {
                return Err(ApiError::Invalid("invalid support event".into()));
            }
            groups.entry(parsed.channel_id).or_default().push(event);
        }
        let _guard = self
            .history_lock
            .lock()
            .expect("support history lock poisoned");
        let mut additions = 0;
        let retention = self
            .state
            .lock()
            .expect("overlay event lock poisoned")
            .support_retention;
        let mut active_history = None;
        let active_id = self
            .state
            .lock()
            .expect("overlay event lock poisoned")
            .broadcaster_id
            .clone();
        for (broadcaster_id, group) in groups {
            let mut saved = load_history(&self.history, &broadcaster_id)?;
            let mut ids: BTreeSet<String> = saved
                .iter()
                .filter_map(|event| event.get("id").and_then(Value::as_str).map(str::to_owned))
                .collect();
            for value in group {
                let id = value
                    .get("id")
                    .and_then(Value::as_str)
                    .expect("validated event ID");
                if ids.insert(id.to_owned()) {
                    saved.push(value);
                    additions += 1;
                }
            }
            sort_support_by_timestamp(&mut saved);
            if saved.len() > retention {
                saved.drain(0..saved.len() - retention);
            }
            let config = StoredConfig {
                definition: HISTORY_DEFINITION.into(),
                version: 1,
                data: Value::Array(saved.clone()),
            };
            match self.history.load(&broadcaster_id) {
                Ok(snapshot) => {
                    self.history
                        .save(&broadcaster_id, &snapshot, &config, |_| Ok(()))?;
                }
                Err(StoreError::NotFound(_)) => {
                    self.history.create(&broadcaster_id, &config, |_| Ok(()))?;
                }
                Err(error) => return Err(error.into()),
            }
            if active_id.as_deref() == Some(&broadcaster_id) {
                active_history = Some((broadcaster_id, saved));
            }
        }
        drop(_guard);
        if let Some((broadcaster_id, saved)) = active_history {
            self.reset_active_support_after_import(&broadcaster_id, saved);
        }
        Ok(additions)
    }

    /// Resets the active snapshot only if the imported account is still selected.
    fn reset_active_support_after_import(&self, broadcaster_id: &str, saved: Vec<Value>) {
        let mut state = self.state.lock().expect("overlay event lock poisoned");
        if state.broadcaster_id.as_deref() != Some(broadcaster_id) {
            return;
        }
        let epoch = state.epoch.clone();
        let retention = state.support_retention;
        let support = state
            .channels
            .entry("twitch.support".into())
            .or_insert_with(|| ChannelState::new("twitch.support"));
        let mut merged = saved;
        let mut ids: BTreeSet<String> = merged
            .iter()
            .filter_map(|event| event.get("id").and_then(Value::as_str).map(str::to_owned))
            .collect();
        if let Some(live) = support.snapshot.as_array() {
            for event in live {
                if let Some(id) = event.get("id").and_then(Value::as_str)
                    && ids.insert(id.to_owned())
                {
                    merged.push(event.clone());
                }
            }
        }
        sort_support_by_timestamp(&mut merged);
        if merged.len() > retention {
            merged.drain(0..merged.len() - retention);
        }
        support.sequence += 1;
        support.snapshot = Value::Array(merged);
        support.snapshot_sequence = support.sequence;
        support.history.clear();
        let _ = self.sender.send(EventFrame {
            kind: "reset",
            channel: "twitch.support".into(),
            cursor: format!("{epoch}:{}", support.sequence),
            data: support.snapshot.clone(),
            recipient: None,
        });
    }

    /// Exposes active channel demand to the single Twitch listener.
    pub fn subscribe_demand(&self) -> watch::Receiver<BTreeSet<String>> {
        self.demand.subscribe()
    }

    /// Reports demand changes through the configuration-status notification type.
    pub fn subscribe_demand_changes(&self) -> watch::Receiver<u64> {
        self.demand_revision.subscribe()
    }

    /// Updates the listener demand and notifies desktop connection-status readers.
    fn notify_demand(&self, state: &State) {
        self.demand.send_replace(demand_set(state));
        let next = (*self.demand_revision.borrow()).wrapping_add(1);
        self.demand_revision.send_replace(next);
    }

    /// Changes the retention bound for automatically recorded support events.
    pub fn configure(&self, support_retention: usize) {
        let mut state = self.state.lock().expect("overlay event lock poisoned");
        let retention_changed = state.support_retention != support_retention;
        state.support_retention = support_retention;
        if let Some(support) = state.channels.get_mut("twitch.support") {
            while support.history.len() > support_retention {
                support.history.pop_front();
            }
            if let Some(events) = support.snapshot.as_array_mut()
                && events.len() > support_retention
            {
                events.drain(0..events.len() - support_retention);
            }
        }
        if retention_changed && let Some(broadcaster_id) = &state.broadcaster_id {
            match self.history_tx.try_send((broadcaster_id.clone(), None)) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    self.pending_trims
                        .lock()
                        .expect("pending trim lock poisoned")
                        .insert(broadcaster_id.clone());
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    self.writer_fault.store(true, Ordering::Relaxed);
                    // Avoid taking the event-state lock recursively while reporting the fault.
                    self.history_unavailable.send_replace(true);
                }
            }
        }
        self.notify_demand(&state);
    }

    /// Captures snapshots or replay after subscribing to live traffic under one lock.
    pub fn subscribe(
        self: &Arc<Self>,
        channels: BTreeSet<String>,
        cursors: BTreeMap<String, String>,
    ) -> EventSubscription {
        self.subscribe_for_consumer(None, channels, cursors)
    }

    /// Captures a named consumer's reconnect window without exposing other recipients' frames.
    pub fn subscribe_for_consumer(
        self: &Arc<Self>,
        consumer_id: Option<&str>,
        channels: BTreeSet<String>,
        cursors: BTreeMap<String, String>,
    ) -> EventSubscription {
        let mut state = self.state.lock().expect("overlay event lock poisoned");
        let receiver = self.sender.subscribe();
        let mut initial = Vec::new();
        for channel in &channels {
            *state.demand.entry(channel.clone()).or_insert(0) += 1;
            let epoch = state.epoch.clone();
            let target_pruned = consumer_id
                .and_then(|id| {
                    state
                        .targeted_pruned_until
                        .get(&(id.to_owned(), channel.clone()))
                })
                .copied()
                .unwrap_or(0);
            let from = cursors
                .get(channel)
                .and_then(|value| parse_cursor(value, &epoch));
            let private_frames: Vec<_> = consumer_id
                .map(|consumer_id| {
                    state
                        .targeted
                        .iter()
                        .filter(|frame| {
                            frame.channel == *channel
                                && frame.recipient.as_deref() == Some(consumer_id)
                                && from.is_none_or(|sequence| {
                                    parse_cursor(&frame.cursor, &epoch)
                                        .is_some_and(|value| value > sequence)
                                })
                        })
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            let entry = state
                .channels
                .entry(channel.clone())
                .or_insert_with(|| ChannelState::new(channel));
            let cursor = format!("{epoch}:{}", entry.sequence);
            let mut channel_initial = Vec::new();
            let normal_pruned = entry.pruned_until;
            let sequence = entry.sequence;
            let snapshot_sequence = entry.snapshot_sequence;
            let mut snapshot = entry.snapshot.clone();
            let can_replay = from.is_some_and(|from| {
                from <= sequence && from >= normal_pruned && from >= target_pruned
            });
            if can_replay {
                let from = from.expect("checked above");
                channel_initial.extend(
                    entry
                        .history
                        .iter()
                        .filter(|frame| {
                            parse_cursor(&frame.cursor, &epoch)
                                .is_some_and(|sequence| sequence > from)
                        })
                        .cloned(),
                );
                channel_initial.extend(private_frames);
                channel_initial
                    .sort_by_key(|frame| parse_cursor(&frame.cursor, &epoch).unwrap_or(0));
            } else {
                if channel == "twitch.channel" {
                    if let Some(private) = private_frames
                        .iter()
                        .max_by_key(|frame| parse_cursor(&frame.cursor, &epoch).unwrap_or(0))
                        && parse_cursor(&private.cursor, &epoch)
                            .is_some_and(|value| value > snapshot_sequence)
                    {
                        snapshot = private.data.clone();
                    }
                } else if let Some(array) = snapshot.as_array_mut() {
                    array.extend(private_frames.into_iter().map(|frame| frame.data));
                }
                channel_initial.push(EventFrame {
                    kind: if cursors.contains_key(channel) {
                        "reset"
                    } else {
                        "snapshot"
                    },
                    channel: channel.clone(),
                    cursor,
                    data: snapshot,
                    recipient: None,
                });
            }
            initial.extend(channel_initial);
        }
        self.notify_demand(&state);
        EventSubscription {
            channels: channels.clone(),
            initial,
            receiver,
            _lease: DemandLease {
                hub: Arc::clone(self),
                channels,
            },
        }
    }

    /// Publishes a Twitch event only to demanded channels or recording history.
    pub fn publish_twitch(&self, event: &TwitchEvent, envelope: &Value) -> Result<(), ApiError> {
        let broadcaster_id = match event {
            TwitchEvent::Message(message) => message.broadcaster_user_id.as_str(),
            TwitchEvent::Notification(notification) => notification.broadcaster_user_id.as_str(),
            TwitchEvent::Subscription(subscription)
            | TwitchEvent::SubscriptionMessage(subscription)
            | TwitchEvent::SubscriptionGift(subscription)
            | TwitchEvent::Cheer(subscription) => subscription
                .get("broadcaster_user_id")
                .and_then(Value::as_str)
                .unwrap_or(""),
            TwitchEvent::ChannelUpdated(channel) | TwitchEvent::DetailsChanged(channel) => {
                channel.broadcaster_user_id.as_str()
            }
            TwitchEvent::AdBreakBegin(ad_break) => ad_break.broadcaster_user_id.as_str(),
        };
        if self
            .state
            .lock()
            .expect("overlay event lock poisoned")
            .broadcaster_id
            .as_deref()
            != Some(broadcaster_id)
        {
            return Err(ApiError::Invalid(
                "event belongs to another broadcaster".into(),
            ));
        }
        match event {
            TwitchEvent::Message(message) => {
                let raw = envelope.pointer("/payload/event").unwrap_or(&Value::Null);
                let parts = raw.get("message").and_then(|message| message.get("fragments")).and_then(Value::as_array)
                    .map(|fragments| fragments.iter().map(|fragment| {
                        let mut part = json!({ "text": fragment.get("text").and_then(Value::as_str).unwrap_or("") });
                        if let Some(id) = fragment.pointer("/emote/id").and_then(Value::as_str).filter(|id| !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')) {
                            part["image"] = json!(format!("https://static-cdn.jtvnw.net/emoticons/v2/{id}/default/dark/2.0"));
                        }
                        part
                    }).collect::<Vec<_>>())
                    .filter(|parts| !parts.is_empty())
                    .unwrap_or_else(|| vec![json!({ "text": message.text })]);
                let color = raw
                    .get("color")
                    .and_then(Value::as_str)
                    .filter(|color| {
                        color.len() == 7
                            && color.starts_with('#')
                            && color.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
                    })
                    .unwrap_or("#fffd00");
                self.publish("twitch.chat", json!({ "id": message.message_id, "userId": message.chatter_user_id, "login": message.chatter_user_login, "user": message.chatter_user_name, "color": color, "parts": parts }), CHAT_LIMIT)?;
            }
            TwitchEvent::Subscription(raw)
                if raw.get("is_gift").and_then(Value::as_bool) == Some(true) => {}
            TwitchEvent::Subscription(raw)
            | TwitchEvent::SubscriptionMessage(raw)
            | TwitchEvent::SubscriptionGift(raw)
            | TwitchEvent::Cheer(raw) => {
                let kind = match event {
                    TwitchEvent::Subscription(_) => "sub",
                    TwitchEvent::SubscriptionMessage(_) => "resub",
                    TwitchEvent::SubscriptionGift(_) => "gift",
                    TwitchEvent::Cheer(_) => "bits",
                    _ => unreachable!(),
                };
                let value = normalize_support(kind, raw, envelope)?;
                self.publish("twitch.support", value, self.retention())?;
            }
            TwitchEvent::ChannelUpdated(channel) => self.publish_channel(
                &channel.category_id,
                &channel.category_name,
                &channel.broadcaster_user_id,
            ),
            _ => {}
        }
        Ok(())
    }

    /// The existing API channels that can carry this non-chat Twitch event.
    pub fn replay_channels(event: &TwitchEvent) -> &'static [&'static str] {
        match event {
            TwitchEvent::Subscription(raw)
                if raw.get("is_gift").and_then(Value::as_bool) == Some(true) =>
            {
                &[]
            }
            TwitchEvent::Subscription(_)
            | TwitchEvent::SubscriptionMessage(_)
            | TwitchEvent::SubscriptionGift(_)
            | TwitchEvent::Cheer(_) => &["twitch.support"],
            TwitchEvent::ChannelUpdated(_) => &["twitch.channel"],
            _ => &[],
        }
    }

    /// Reads the live broadcaster identity without changing the epoch, demand or snapshots.
    pub(super) fn broadcaster_id(&self) -> Option<String> {
        self.state
            .lock()
            .expect("overlay event lock poisoned")
            .broadcaster_id
            .clone()
    }

    /// Sends native synthetic data only to validated recipients without changing shared state.
    pub(super) fn publish_targeted(
        &self,
        channel: &str,
        data: Value,
        consumers: &BTreeSet<String>,
    ) -> Result<(), ApiError> {
        if !super::channel_ids().contains(&channel) {
            return Err(ApiError::Invalid("unknown API event channel".into()));
        }
        for consumer in consumers {
            self.publish_frame(channel, data.clone(), 0, false, true, Some(consumer))?;
        }
        Ok(())
    }

    /// Sends a replay through one existing API channel without recording it again.
    pub fn replay_twitch_channel(
        &self,
        event: &TwitchEvent,
        envelope: &Value,
        channel: &str,
    ) -> Result<(), ApiError> {
        self.replay_twitch_channel_for(event, envelope, channel, None)
    }

    /// Buffers one transient replay only for a named consumer's authorized stream.
    pub fn replay_twitch_consumer(
        &self,
        event: &TwitchEvent,
        envelope: &Value,
        channel: &str,
        consumer_id: &str,
    ) -> Result<(), ApiError> {
        self.replay_twitch_channel_for(event, envelope, channel, Some(consumer_id))
    }

    fn replay_twitch_channel_for(
        &self,
        event: &TwitchEvent,
        envelope: &Value,
        channel: &str,
        recipient: Option<&str>,
    ) -> Result<(), ApiError> {
        if !Self::replay_channels(event).contains(&channel) {
            return Err(ApiError::Invalid(
                "event cannot use this API channel".into(),
            ));
        }
        let broadcaster_id = envelope
            .pointer("/payload/event/broadcaster_user_id")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::Invalid("event has no broadcaster identity".into()))?;
        self.set_broadcaster(broadcaster_id)?;
        match channel {
            "twitch.support" => {
                let raw = envelope.pointer("/payload/event").unwrap_or(&Value::Null);
                let kind = match event {
                    TwitchEvent::Subscription(_) => "sub",
                    TwitchEvent::SubscriptionMessage(_) => "resub",
                    TwitchEvent::SubscriptionGift(_) => "gift",
                    TwitchEvent::Cheer(_) => "bits",
                    _ => unreachable!(),
                };
                let mut value = normalize_support(kind, raw, envelope)?;
                value["id"] = json!(format!("replay-{}", Uuid::new_v4().simple()));
                self.publish_frame(
                    "twitch.support",
                    value,
                    self.retention(),
                    false,
                    true,
                    recipient,
                )
            }
            "twitch.channel" => {
                let TwitchEvent::ChannelUpdated(updated) = event else {
                    unreachable!()
                };
                self.publish_frame(
                    "twitch.channel",
                    json!({ "id": updated.category_id, "name": updated.category_name, "broadcasterId": updated.broadcaster_user_id }),
                    1,
                    false,
                    true,
                    recipient,
                )
            }
            _ => unreachable!(),
        }
    }

    /// Publishes the current channel identity and game to channel subscribers.
    pub fn publish_channel(&self, id: &str, name: &str, broadcaster_id: &str) {
        if self
            .state
            .lock()
            .expect("overlay event lock poisoned")
            .broadcaster_id
            .as_deref()
            != Some(broadcaster_id)
        {
            return;
        }
        let _ = self.publish(
            "twitch.channel",
            json!({ "id": id, "name": name, "broadcasterId": broadcaster_id }),
            1,
        );
    }

    /// Reads the current support retention under the event-state lock.
    fn retention(&self) -> usize {
        self.state
            .lock()
            .expect("overlay event lock poisoned")
            .support_retention
    }

    /// Updates one demanded snapshot and replay window before broadcasting its frame.
    fn publish(&self, channel: &str, data: Value, limit: usize) -> Result<(), ApiError> {
        self.publish_frame(channel, data, limit, true, false, None)
    }

    /// Updates an API channel with explicit persistence and demand rules.
    fn publish_frame(
        &self,
        channel: &str,
        data: Value,
        limit: usize,
        persist_support: bool,
        force: bool,
        recipient: Option<&str>,
    ) -> Result<(), ApiError> {
        let mut state = self.state.lock().expect("overlay event lock poisoned");
        if !force && !state.demand.contains_key(channel) && channel != "twitch.support" {
            return Ok(());
        }
        if persist_support && channel == "twitch.support" {
            let broadcaster_id = data
                .get("channelId")
                .and_then(Value::as_str)
                .ok_or_else(|| ApiError::Invalid("support event has no broadcaster ID".into()))?;
            if state.broadcaster_id.as_deref() != Some(broadcaster_id) {
                return Err(ApiError::Invalid(
                    "support event belongs to another broadcaster".into(),
                ));
            }
            self.history_tx
                .try_send((broadcaster_id.to_owned(), Some(data.clone())))
                .map_err(|_| ApiError::Busy)?;
        }
        let epoch = state.epoch.clone();
        let entry = state
            .channels
            .entry(channel.into())
            .or_insert_with(|| ChannelState::new(channel));
        entry.sequence += 1;
        let frame = EventFrame {
            kind: "event",
            channel: channel.into(),
            cursor: format!("{epoch}:{}", entry.sequence),
            data: data.clone(),
            recipient: recipient.map(str::to_owned),
        };
        if recipient.is_some() {
            state.targeted.push_back(frame.clone());
            while state.targeted.len() > TARGETED_LIMIT {
                if let Some(evicted) = state.targeted.pop_front()
                    && let Some(sequence) = parse_cursor(&evicted.cursor, &epoch)
                {
                    state
                        .targeted_pruned_until
                        .entry((
                            evicted.recipient.expect("targeted frames have recipients"),
                            evicted.channel,
                        ))
                        .and_modify(|until| *until = (*until).max(sequence))
                        .or_insert(sequence);
                }
            }
        } else {
            if channel == "twitch.channel" {
                entry.snapshot = data;
            } else if let Some(array) = entry.snapshot.as_array_mut() {
                array.push(data);
                if array.len() > limit {
                    array.drain(0..array.len() - limit);
                }
            }
            entry.snapshot_sequence = entry.sequence;
            entry.history.push_back(frame.clone());
            while entry.history.len() > limit {
                if let Some(evicted) = entry.history.pop_front()
                    && let Some(sequence) = parse_cursor(&evicted.cursor, &epoch)
                {
                    entry.pruned_until = entry.pruned_until.max(sequence);
                }
            }
        }
        let _ = self.sender.send(frame);
        Ok(())
    }
}

/// Maps the four authoritative Twitch support events to the overlay's stable item shape.
fn normalize_support(kind: &str, raw: &Value, envelope: &Value) -> Result<Value, ApiError> {
    let field = |name: &str| raw.get(name).and_then(Value::as_str);
    let id = envelope
        .pointer("/metadata/message_id")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::Invalid("support event has no ID".into()))?;
    let timestamp = envelope
        .pointer("/metadata/message_timestamp")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::Invalid("support event has no timestamp".into()))?;
    let channel_id = field("broadcaster_user_id")
        .ok_or_else(|| ApiError::Invalid("support event has no broadcaster ID".into()))?;
    let user = if raw.get("is_anonymous").and_then(Value::as_bool) == Some(true) {
        "Anonymous"
    } else {
        field("user_name")
            .filter(|name| !name.is_empty())
            .or_else(|| field("user_login"))
            .unwrap_or("Anonymous")
    };
    let mut value = json!({"id": id, "channelId": channel_id, "kind": kind, "user": user, "timestamp": timestamp});
    if let Some(tier) = field("tier") {
        value["tier"] = json!(tier);
    }
    let amount_field = match kind {
        "bits" => "bits",
        "gift" => "total",
        "resub" => "cumulative_months",
        _ => "",
    };
    if !amount_field.is_empty()
        && let Some(amount) = raw.get(amount_field).and_then(Value::as_u64)
    {
        value["amount"] = json!(amount);
    }
    if let Some(message) = raw.get("message")
        && let Some(text) = message
            .as_str()
            .or_else(|| message.get("text").and_then(Value::as_str))
    {
        value["message"] = json!(text);
    }
    Ok(value)
}

/// Accepts a sequence only from this process and broadcaster epoch.
fn parse_cursor(cursor: &str, epoch: &str) -> Option<u64> {
    cursor
        .split_once(':')
        .and_then(|(prefix, sequence)| (prefix == epoch).then(|| sequence.parse().ok()).flatten())
}

/// Keeps support subscriptions active alongside channels requested by API clients.
fn demand_set(state: &State) -> BTreeSet<String> {
    let mut channels: BTreeSet<String> = state.demand.keys().cloned().collect();
    channels.insert("twitch.support".into());
    channels
}

/// Keeps the newest support events when an older legacy export is imported later.
fn sort_support_by_timestamp(events: &mut [Value]) {
    events.sort_by_key(|event| {
        event
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|timestamp| chrono::DateTime::parse_from_rfc3339(timestamp).ok())
    });
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
/// Validated fields accepted from the previous overlay's history export.
struct ImportedSupportEvent {
    id: String,
    channel_id: String,
    #[serde(rename = "kind")]
    _kind: ImportedSupportKind,
    user: String,
    timestamp: String,
    amount: Option<f64>,
    tier: Option<String>,
    message: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
/// Support kinds that the legacy history importer accepts.
enum ImportedSupportKind {
    Sub,
    Resub,
    Gift,
    Bits,
}

/// Reads a single broadcaster's durable history without repairing corrupt data.
fn load_history(store: &ConfigStore, broadcaster_id: &str) -> Result<Vec<Value>, ApiError> {
    match store.load(broadcaster_id) {
        Ok(snapshot) => {
            let config = snapshot.config();
            if config.definition != HISTORY_DEFINITION || config.version != 1 {
                return Err(ApiError::InvalidFormat);
            }
            serde_json::from_value(config.data.clone()).map_err(|_| ApiError::InvalidFormat)
        }
        Err(StoreError::NotFound(_)) => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

/// Appends or trims one broadcaster's history through the versioned store.
fn persist_history(
    store: &ConfigStore,
    broadcaster_id: &str,
    value: Option<Value>,
    retention: usize,
) -> Result<(), ApiError> {
    if broadcaster_id.is_empty() || !broadcaster_id.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ApiError::Invalid("invalid support broadcaster ID".into()));
    }
    let mut saved = load_history(store, broadcaster_id)?;
    if let Some(value) = value {
        let id = value.get("id").and_then(Value::as_str).unwrap_or("");
        if !id.is_empty()
            && saved
                .iter()
                .any(|event| event.get("id").and_then(Value::as_str) == Some(id))
        {
            return Ok(());
        }
        saved.push(value);
    }
    sort_support_by_timestamp(&mut saved);
    if saved.len() > retention {
        saved.drain(0..saved.len() - retention);
    }
    let config = StoredConfig {
        definition: HISTORY_DEFINITION.into(),
        version: 1,
        data: Value::Array(saved),
    };
    match store.load(broadcaster_id) {
        Ok(snapshot) => {
            store.save(broadcaster_id, &snapshot, &config, |_| Ok(()))?;
        }
        Err(StoreError::NotFound(_)) => {
            store.create(broadcaster_id, &config, |_| Ok(()))?;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    use super::*;

    /// Synthetic deliveries remain private and leave broadcaster snapshots and durable writes alone.
    #[test]
    fn synthetic_events_do_not_change_shared_channel_or_support_state() {
        let temporary = tempfile::tempdir().unwrap();
        let hub = EventHub::new(&paths(temporary.path()), 500);
        hub.set_broadcaster("100").unwrap();
        let epoch = hub.state.lock().unwrap().epoch.clone();
        let consumers = BTreeSet::from(["alice".into()]);
        for channel in [
            "twitch.chat",
            "twitch.support",
            "twitch.channel",
            "api.custom",
        ] {
            hub.publish_targeted(channel, json!({"id": "synthetic"}), &consumers)
                .unwrap();
        }
        let state = hub.state.lock().unwrap();
        assert_eq!(state.broadcaster_id.as_deref(), Some("100"));
        assert_eq!(state.epoch, epoch);
        assert_eq!(state.targeted.len(), 4);
        assert!(
            state
                .targeted
                .iter()
                .all(|frame| frame.visible_to("alice") && !frame.visible_to("bob"))
        );
        for (channel, value) in &state.channels {
            assert!(value.history.is_empty());
            if channel == "twitch.channel" {
                assert!(value.snapshot.is_null());
            } else {
                assert_eq!(value.snapshot, json!([]));
            }
        }
        drop(state);
        assert!(
            hub.history_rx
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .try_recv()
                .is_err()
        );
    }

    /// Gives event tests independent support-history files.
    fn paths(root: &std::path::Path) -> AppPaths {
        AppPaths {
            config: root.join("config"),
            data: root.join("data"),
            state: root.join("state"),
        }
    }

    /// Flushes recorded cheers before shutdown and restores the same account after restart.
    #[tokio::test]
    async fn cheer_recording_survives_restart_and_resets_on_account_change() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = paths(temporary.path());
        let hub = EventHub::new(&paths, 500);
        hub.set_broadcaster("100").unwrap();
        let shutdown = CancellationToken::new();
        let writer = tokio::spawn({
            let hub = Arc::clone(&hub);
            let shutdown = shutdown.clone();
            async move { hub.run_history(shutdown).await }
        });
        hub.publish_twitch(&TwitchEvent::Cheer(json!({"broadcaster_user_id":"100","user_name":"Viewer","bits":100,"message":"yay"})), &json!({"metadata":{"message_id":"one","message_timestamp":"2026-10-18T08:00:00Z"}})).unwrap();
        shutdown.cancel();
        writer.await.unwrap().unwrap();
        let restarted = EventHub::new(&paths, 500);
        restarted.set_broadcaster("100").unwrap();
        let channels = BTreeSet::from(["twitch.support".to_owned()]);
        let snapshot = restarted.subscribe(channels.clone(), BTreeMap::new());
        assert_eq!(snapshot.initial[0].data.as_array().unwrap().len(), 1);
        drop(snapshot);
        restarted.set_broadcaster("200").unwrap();
        let empty = restarted.subscribe(channels.clone(), BTreeMap::new());
        assert!(empty.initial[0].data.as_array().unwrap().is_empty());
        drop(empty);
        restarted.set_broadcaster("100").unwrap();
        let reset = restarted.subscribe(
            channels,
            BTreeMap::from([("twitch.support".into(), "expired".into())]),
        );
        assert_eq!(reset.initial[0].kind, "reset");
        assert_eq!(reset.initial[0].data.as_array().unwrap().len(), 1);
    }

    /// Counts aggregate gifts once and retains resubscription fields.
    #[test]
    fn support_uses_authoritative_events_and_omits_gift_deliveries() {
        let temporary = tempfile::tempdir().unwrap();
        let hub = EventHub::new(&paths(temporary.path()), 500);
        hub.set_broadcaster("100").unwrap();
        let channels = BTreeSet::from(["twitch.support".to_owned()]);
        let _subscription = hub.subscribe(channels.clone(), BTreeMap::new());
        let envelope = |id: &str| json!({"metadata":{"message_id":id,"message_timestamp":"2026-10-18T08:00:00Z"}});
        hub.publish_twitch(&TwitchEvent::Subscription(json!({"broadcaster_user_id":"100","is_gift":true,"user_name":"Recipient","tier":"1000"})), &envelope("delivery")).unwrap();
        hub.publish_twitch(
            &TwitchEvent::SubscriptionGift(
                json!({"broadcaster_user_id":"100","user_name":"Gifter","total":5,"tier":"1000"}),
            ),
            &envelope("gift"),
        )
        .unwrap();
        hub.publish_twitch(&TwitchEvent::SubscriptionMessage(json!({"broadcaster_user_id":"100","user_name":"Viewer","cumulative_months":14,"tier":"2000","message":{"text":"Still here"}})), &envelope("resub")).unwrap();
        let snapshot = hub.subscribe(channels, BTreeMap::new()).initial;
        let events = snapshot[0].data.as_array().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["kind"], "gift");
        assert_eq!(events[0]["amount"], 5);
        assert_eq!(events[1]["kind"], "resub");
        assert_eq!(events[1]["amount"], 14);
        assert_eq!(events[1]["message"], "Still here");
    }

    /// Replayed support reaches the live channel with a fresh ID and no durable write.
    #[test]
    fn replayed_support_is_transient_and_uses_unique_delivery_ids() {
        let temporary = tempfile::tempdir().unwrap();
        let hub = EventHub::new(&paths(temporary.path()), 500);
        let event = TwitchEvent::Cheer(
            json!({"broadcaster_user_id":"100","user_name":"Viewer","bits":100}),
        );
        let envelope = json!({
            "metadata":{"message_id":"original","message_timestamp":"2026-10-18T08:00:00Z"},
            "payload":{"event":{"broadcaster_user_id":"100"}}
        });
        hub.replay_twitch_channel(&event, &envelope, "twitch.support")
            .unwrap();
        hub.replay_twitch_channel(&event, &envelope, "twitch.support")
            .unwrap();
        let subscription =
            hub.subscribe(BTreeSet::from(["twitch.support".into()]), BTreeMap::new());
        let snapshot = subscription.initial[0].data.as_array().unwrap();
        assert_eq!(snapshot.len(), 2);
        assert_ne!(snapshot[0]["id"], snapshot[1]["id"]);
        assert_ne!(snapshot[0]["id"], "original");
        assert!(matches!(
            hub.history.load("100"),
            Err(StoreError::NotFound(_))
        ));
        assert!(
            hub.replay_twitch_channel(&event, &envelope, "twitch.channel")
                .is_err()
        );
    }

    /// Private replay stays out of other streams and global snapshots, including reconnects.
    #[test]
    fn targeted_replay_is_isolated_and_reconnects_in_cursor_order() {
        let temporary = tempfile::tempdir().unwrap();
        let hub = EventHub::new(&paths(temporary.path()), 500);
        hub.set_broadcaster("100").unwrap();
        let channels = BTreeSet::from(["twitch.support".to_owned()]);
        let mut alice =
            hub.subscribe_for_consumer(Some("alice"), channels.clone(), BTreeMap::new());
        let mut bob = hub.subscribe_for_consumer(Some("bob"), channels.clone(), BTreeMap::new());
        let cursor = alice.initial[0].cursor.clone();
        let event = TwitchEvent::Cheer(
            json!({"broadcaster_user_id":"100","user_name":"Viewer","bits":100}),
        );
        let envelope = |id: &str| {
            json!({
                "metadata":{"message_id":id,"message_timestamp":"2026-10-18T08:00:00Z"},
                "payload":{"event":{"broadcaster_user_id":"100"}}
            })
        };
        hub.publish_twitch(&event, &envelope("first")).unwrap();
        hub.replay_twitch_consumer(&event, &envelope("private"), "twitch.support", "alice")
            .unwrap();
        hub.publish_twitch(&event, &envelope("last")).unwrap();

        let live_alice: Vec<_> = (0..3).map(|_| alice.receiver.try_recv().unwrap()).collect();
        let live_bob: Vec<_> = (0..3)
            .map(|_| bob.receiver.try_recv().unwrap())
            .filter(|frame| frame.visible_to("bob"))
            .collect();
        assert_eq!(
            live_alice
                .iter()
                .filter(|frame| frame.visible_to("alice"))
                .count(),
            3
        );
        assert_eq!(live_bob.len(), 2);
        assert!(!live_alice[1].visible_to("bob"));
        assert!(
            serde_json::to_value(&live_alice[1])
                .unwrap()
                .get("recipient")
                .is_none()
        );

        let cursors = BTreeMap::from([("twitch.support".into(), cursor)]);
        let alice_reconnect =
            hub.subscribe_for_consumer(Some("alice"), channels.clone(), cursors.clone());
        let bob_reconnect = hub.subscribe_for_consumer(Some("bob"), channels.clone(), cursors);
        assert_eq!(
            alice_reconnect
                .initial
                .iter()
                .map(|frame| frame.cursor.as_str())
                .collect::<Vec<_>>(),
            live_alice
                .iter()
                .map(|frame| frame.cursor.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            bob_reconnect
                .initial
                .iter()
                .map(|frame| frame.cursor.as_str())
                .collect::<Vec<_>>(),
            vec![live_alice[0].cursor.as_str(), live_alice[2].cursor.as_str()]
        );
        let fresh_bob = hub.subscribe_for_consumer(Some("bob"), channels, BTreeMap::new());
        assert_eq!(fresh_bob.initial.len(), 1);
        assert_eq!(fresh_bob.initial[0].data.as_array().unwrap().len(), 2);
    }

    /// A private channel update cannot overwrite a later public state on a fresh connection.
    #[test]
    fn targeted_channel_snapshot_uses_latest_visible_state() {
        let temporary = tempfile::tempdir().unwrap();
        let hub = EventHub::new(&paths(temporary.path()), 500);
        hub.set_broadcaster("100").unwrap();
        let channels = BTreeSet::from(["twitch.channel".to_owned()]);
        let _demand = hub.subscribe_for_consumer(Some("alice"), channels.clone(), BTreeMap::new());
        let event = |name: &str| {
            TwitchEvent::ChannelUpdated(crate::twitch::events::ChannelUpdated {
                broadcaster_user_id: "100".into(),
                broadcaster_user_login: "owner".into(),
                broadcaster_user_name: "Owner".into(),
                title: name.into(),
                category_id: name.into(),
                category_name: name.into(),
                language: "en".into(),
            })
        };
        let envelope = json!({"payload":{"event":{"broadcaster_user_id":"100"}}});
        hub.replay_twitch_channel(&event("A"), &envelope, "twitch.channel")
            .unwrap();
        let cursor = hub
            .subscribe_for_consumer(Some("alice"), channels.clone(), BTreeMap::new())
            .initial[0]
            .cursor
            .clone();
        hub.replay_twitch_consumer(&event("B"), &envelope, "twitch.channel", "alice")
            .unwrap();
        hub.replay_twitch_channel(&event("C"), &envelope, "twitch.channel")
            .unwrap();
        let alice = hub.subscribe_for_consumer(Some("alice"), channels.clone(), BTreeMap::new());
        assert_eq!(alice.initial[0].data["name"], "C");
        let replay = hub.subscribe_for_consumer(
            Some("alice"),
            channels.clone(),
            BTreeMap::from([("twitch.channel".into(), cursor)]),
        );
        assert_eq!(
            replay
                .initial
                .iter()
                .map(|frame| frame.data["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["B", "C"]
        );
        hub.replay_twitch_consumer(&event("D"), &envelope, "twitch.channel", "alice")
            .unwrap();
        let alice = hub.subscribe_for_consumer(Some("alice"), channels.clone(), BTreeMap::new());
        let bob = hub.subscribe_for_consumer(Some("bob"), channels, BTreeMap::new());
        assert_eq!(alice.initial[0].data["name"], "D");
        assert_eq!(bob.initial[0].data["name"], "C");
    }

    /// Overflow in one recipient's private queue does not reset another's cursor.
    #[test]
    fn private_queue_eviction_is_scoped_to_recipient() {
        let temporary = tempfile::tempdir().unwrap();
        let hub = EventHub::new(&paths(temporary.path()), 500);
        hub.set_broadcaster("100").unwrap();
        let channels = BTreeSet::from(["twitch.support".to_owned()]);
        let bob_cursor = hub
            .subscribe_for_consumer(Some("bob"), channels.clone(), BTreeMap::new())
            .initial[0]
            .cursor
            .clone();
        let event =
            TwitchEvent::Cheer(json!({"broadcaster_user_id":"100","user_name":"Viewer","bits":1}));
        let envelope = json!({
            "metadata":{"message_id":"original","message_timestamp":"2026-10-18T08:00:00Z"},
            "payload":{"event":{"broadcaster_user_id":"100"}}
        });
        for _ in 0..=TARGETED_LIMIT {
            hub.replay_twitch_consumer(&event, &envelope, "twitch.support", "alice")
                .unwrap();
        }
        let bob = hub.subscribe_for_consumer(
            Some("bob"),
            channels.clone(),
            BTreeMap::from([("twitch.support".into(), bob_cursor.clone())]),
        );
        let alice = hub.subscribe_for_consumer(
            Some("alice"),
            channels,
            BTreeMap::from([("twitch.support".into(), bob_cursor)]),
        );
        assert!(bob.initial.is_empty());
        assert_eq!(alice.initial[0].kind, "reset");
    }

    /// Preserves Twitch emote images and substitutes an invalid name color.
    #[test]
    fn chat_uses_native_emotes_and_safe_color_fallback() {
        let temporary = tempfile::tempdir().unwrap();
        let hub = EventHub::new(&paths(temporary.path()), 500);
        hub.set_broadcaster("100").unwrap();
        let channels = BTreeSet::from(["twitch.chat".to_owned()]);
        let _subscription = hub.subscribe(channels.clone(), BTreeMap::new());
        let message = crate::twitch::events::ChatMessage {
            broadcaster_user_id: "100".into(),
            broadcaster_user_login: "owner".into(),
            broadcaster_user_name: "Owner".into(),
            chatter_user_id: "200".into(),
            chatter_user_login: "viewer".into(),
            chatter_user_name: "Viewer".into(),
            message_id: "message".into(),
            text: "Hi Kappa".into(),
            is_source_only: false,
            is_bot: false,
            is_broadcaster: false,
            is_moderator: false,
        };
        hub.publish_twitch(&TwitchEvent::Message(message), &json!({"payload":{"event":{"color":"invalid","message":{"fragments":[{"type":"text","text":"Hi "},{"type":"emote","text":"Kappa","emote":{"id":"25"}}]}}}})).unwrap();
        let snapshot = hub.subscribe(channels, BTreeMap::new()).initial;
        let chat = &snapshot[0].data[0];
        assert_eq!(chat["color"], "#fffd00");
        assert_eq!(chat["parts"][0]["text"], "Hi ");
        assert_eq!(
            chat["parts"][1]["image"],
            "https://static-cdn.jtvnw.net/emoticons/v2/25/default/dark/2.0"
        );
    }

    /// Validates an entire import and partitions accepted records by broadcaster.
    #[test]
    fn legacy_import_validates_before_writing_and_keeps_broadcasters_separate() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = paths(temporary.path());
        let hub = EventHub::new(&paths, 500);
        let valid = json!({"id":"one","channelId":"100","kind":"gift","user":"Gifter","timestamp":"2026-10-18T08:00:00Z","amount":5});
        let invalid = json!({"id":"two","channelId":"../other","kind":"gift","user":"Gifter","timestamp":"2026-10-18T08:00:00Z"});
        assert!(
            hub.import_support_history(vec![valid.clone(), invalid])
                .is_err()
        );
        assert!(load_history(&hub.history, "100").unwrap().is_empty());
        assert_eq!(
            hub.import_support_history(vec![valid.clone(), valid])
                .unwrap(),
            1
        );
        hub.set_broadcaster("100").unwrap();
        let history = hub
            .subscribe(BTreeSet::from(["twitch.support".into()]), BTreeMap::new())
            .initial;
        assert_eq!(history[0].data.as_array().unwrap().len(), 1);
        hub.clear_if_other_broadcaster(Some("200"));
        let cleared = hub
            .subscribe(BTreeSet::from(["twitch.support".into()]), BTreeMap::new())
            .initial;
        assert!(cleared[0].data.as_array().unwrap().is_empty());
        hub.set_broadcaster("200").unwrap();
        let other = hub
            .subscribe(BTreeSet::from(["twitch.support".into()]), BTreeMap::new())
            .initial;
        assert!(other[0].data.as_array().unwrap().is_empty());
    }

    /// Clears the prior broadcaster even when the new account's history cannot be read.
    #[test]
    fn corrupt_next_account_history_clears_previous_snapshot() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = paths(temporary.path());
        let hub = EventHub::new(&paths, 500);
        let event = json!({"id":"one","channelId":"100","kind":"sub","user":"Viewer","timestamp":"2026-10-18T08:00:00Z"});
        hub.import_support_history(vec![event]).unwrap();
        hub.set_broadcaster("100").unwrap();
        let next = paths.state.join("api/support_history/200.json");
        std::fs::create_dir_all(next.parent().unwrap()).unwrap();
        std::fs::write(&next, b"{corrupt").unwrap();
        assert!(hub.set_broadcaster("200").is_err());
        assert!(*hub.subscribe_history_unavailable().borrow());
        let snapshot = hub
            .subscribe(BTreeSet::from(["twitch.support".into()]), BTreeMap::new())
            .initial;
        assert!(snapshot[0].data.as_array().unwrap().is_empty());
        assert_eq!(std::fs::read(&next).unwrap(), b"{corrupt");
    }

    /// Retains newer recorded events when importing an older legacy file.
    #[test]
    fn older_import_does_not_evict_newer_recorded_support() {
        let temporary = tempfile::tempdir().unwrap();
        let hub = EventHub::new(&paths(temporary.path()), 1);
        let newer = json!({"id":"new","channelId":"100","kind":"bits","user":"Viewer","timestamp":"2026-10-18T08:00:00Z","amount":100});
        let older = json!({"id":"old","channelId":"100","kind":"sub","user":"Viewer","timestamp":"2025-10-18T08:00:00Z"});
        hub.import_support_history(vec![newer]).unwrap();
        hub.import_support_history(vec![older]).unwrap();
        let saved = load_history(&hub.history, "100").unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0]["id"], "new");
    }

    /// Ignores an import finishing after the active broadcaster changed.
    #[test]
    fn stale_import_completion_cannot_reset_a_new_broadcaster() {
        let temporary = tempfile::tempdir().unwrap();
        let hub = EventHub::new(&paths(temporary.path()), 500);
        hub.set_broadcaster("100").unwrap();
        hub.set_broadcaster("200").unwrap();
        hub.reset_active_support_after_import("100", vec![json!({"id":"old","channelId":"100","kind":"sub","user":"Viewer","timestamp":"2025-10-18T08:00:00Z"})]);
        let snapshot = hub
            .subscribe(BTreeSet::from(["twitch.support".into()]), BTreeMap::new())
            .initial;
        assert!(snapshot[0].data.as_array().unwrap().is_empty());
    }
}
