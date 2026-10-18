//! OBS capabilities accept legacy resource names and stable IDs from current resource choices.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::ConfigSchema;
use crate::engine::{
    Capability, CapabilityError, Engine, Input, Values, validate_configured_inputs,
};
use crate::schema::{ActionDefinition, DescribeConfig};

pub type ObsFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ObsError>> + Send + 'a>>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SceneItem {
    pub id: i64,
    pub name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObsResource {
    pub name: String,
    pub uuid: uuid::Uuid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObsSceneItemResource {
    pub scene: ObsResource,
    pub group_path: Vec<String>,
    pub item_id: i64,
    pub source_name: String,
}

pub(super) const SCENE_TOKEN_PREFIX: &str = "obs:scene:v1:";
pub(super) const INPUT_TOKEN_PREFIX: &str = "obs:input:v1:";
pub(super) const ITEM_TOKEN_PREFIX: &str = "obs:item:v1:";

pub(crate) fn scene_token(resource: &ObsResource) -> String {
    format!("{SCENE_TOKEN_PREFIX}{}", resource.uuid)
}

pub(crate) fn input_token(resource: &ObsResource) -> String {
    format!("{INPUT_TOKEN_PREFIX}{}", resource.uuid)
}

pub(crate) fn scene_item_token(resource: &ObsSceneItemResource) -> String {
    let identity = serde_json::to_vec(&(&resource.group_path, &resource.source_name))
        .expect("scene item token identity is serializable");
    format!(
        "{ITEM_TOKEN_PREFIX}{}:{}:{}",
        resource.scene.uuid,
        resource.item_id,
        URL_SAFE_NO_PAD.encode(identity)
    )
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SceneItemToken {
    pub(super) scene_uuid: uuid::Uuid,
    pub(super) item_id: i64,
    pub(super) group_path: Vec<String>,
    pub(super) source_name: String,
}

pub(super) fn parse_uuid_token(value: &str, prefix: &str) -> Option<uuid::Uuid> {
    value.strip_prefix(prefix)?.parse().ok()
}

pub(super) fn parse_scene_item_token(value: &str) -> Option<SceneItemToken> {
    let rest = value.strip_prefix(ITEM_TOKEN_PREFIX)?;
    let (scene_uuid, rest) = rest.split_once(':')?;
    let (item_id, identity) = rest.split_once(':')?;
    let scene_uuid = scene_uuid.parse().ok()?;
    let item_id = item_id.parse().ok()?;
    let identity = URL_SAFE_NO_PAD.decode(identity).ok()?;
    let (group_path, source_name) = serde_json::from_slice(&identity).ok()?;
    Some(SceneItemToken {
        scene_uuid,
        item_id,
        group_path,
        source_name,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObsResourceSnapshot {
    pub scenes: Vec<ObsResource>,
    pub inputs: Vec<ObsResource>,
    pub scene_items: Vec<ObsSceneItemResource>,
    pub current_program_scene: Option<ObsResource>,
    pub current_preview_scene: Option<ObsResource>,
    pub recording: ObsRecordingStatus,
    pub streaming: ObsStreamingStatus,
    pub replay_buffer_active: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ObsRecordingStatus {
    pub active: bool,
    pub paused: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ObsStreamingStatus {
    pub active: bool,
    pub reconnecting: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ObsError {
    #[error("{0}")]
    Unavailable(String),
    #[error("{0}")]
    Failed(String),
    #[error("{0}")]
    Uncertain(String),
}

pub trait ObsApi: Send + Sync {
    fn ready(&self) -> Result<(), String>;
    fn scenes(&self) -> ObsFuture<'_, Vec<String>>;
    fn inputs(&self) -> ObsFuture<'_, Vec<String>>;
    fn items<'a>(&'a self, scene: &'a str) -> ObsFuture<'a, Vec<SceneItem>>;
    fn set_scene<'a>(&'a self, scene: &'a str) -> ObsFuture<'a, ()>;
    fn set_preview_scene<'a>(&'a self, scene: &'a str) -> ObsFuture<'a, ()>;
    fn set_item_enabled<'a>(&'a self, scene: &'a str, id: i64, enabled: bool) -> ObsFuture<'a, ()>;
    fn set_input_muted<'a>(&'a self, input: &'a str, muted: bool) -> ObsFuture<'a, ()>;
    fn set_input_volume<'a>(&'a self, input: &'a str, volume: f32) -> ObsFuture<'a, ()>;
    fn create_chapter<'a>(&'a self, name: &'a str) -> ObsFuture<'a, ()>;
    fn recording_start(&self) -> ObsFuture<'_, ()>;
    fn recording_stop(&self) -> ObsFuture<'_, ()>;
    fn recording_pause(&self) -> ObsFuture<'_, ()>;
    fn recording_resume(&self) -> ObsFuture<'_, ()>;
    fn recording_status(&self) -> ObsFuture<'_, Values>;
    fn streaming_start(&self) -> ObsFuture<'_, ()>;
    fn streaming_stop(&self) -> ObsFuture<'_, ()>;
    fn streaming_status(&self) -> ObsFuture<'_, Values>;
    fn replay_start(&self) -> ObsFuture<'_, ()>;
    fn replay_stop(&self) -> ObsFuture<'_, ()>;
    fn replay_save(&self) -> ObsFuture<'_, ()>;
    fn replay_status(&self) -> ObsFuture<'_, Values>;
    fn resource_snapshot(&self) -> ObsFuture<'_, ObsResourceSnapshot>;
}

#[derive(Default, Deserialize, Serialize, ConfigSchema)]
#[serde(deny_unknown_fields)]
#[config(
    id = "obs.set_current_program_scene",
    version = 1,
    title = "Set current program scene"
)]
struct SetSceneInputs {
    #[config(id = "scene", label = "Scene", introduced = 1, choice = "obs.scenes")]
    scene: String,
}

#[derive(Default, Deserialize, Serialize, ConfigSchema)]
#[serde(deny_unknown_fields)]
#[config(
    id = "obs.set_scene_item_enabled",
    version = 1,
    title = "Set scene item visibility"
)]
struct SetItemEnabledInputs {
    #[config(id = "scene", label = "Scene", introduced = 1, choice = "obs.scenes")]
    scene: String,
    #[config(
        id = "item",
        label = "Item",
        introduced = 1,
        choice = "obs.scene_items",
        depends_on = "scene"
    )]
    item: String,
    #[config(id = "enabled", label = "Visible", introduced = 1)]
    enabled: bool,
}

#[derive(Default, Deserialize, Serialize, ConfigSchema)]
#[serde(deny_unknown_fields)]
#[config(
    id = "obs.set_current_preview_scene",
    version = 1,
    title = "Set current preview scene"
)]
struct SetPreviewSceneInputs {
    #[config(id = "scene", label = "Scene", introduced = 1, choice = "obs.scenes")]
    scene: String,
}

#[derive(Default, Deserialize, Serialize, ConfigSchema)]
#[serde(deny_unknown_fields)]
#[config(id = "obs.set_input_mute", version = 1, title = "Set input mute")]
struct SetInputMuteInputs {
    #[config(id = "input", label = "Input", introduced = 1, choice = "obs.inputs")]
    input: String,
    #[config(id = "muted", label = "Muted", introduced = 1)]
    muted: bool,
}

#[derive(Default, Deserialize, Serialize, ConfigSchema)]
#[serde(deny_unknown_fields)]
#[config(id = "obs.set_input_volume", version = 1, title = "Set input volume")]
struct SetInputVolumeInputs {
    #[config(id = "input", label = "Input", introduced = 1, choice = "obs.inputs")]
    input: String,
    #[config(id = "volume_percent", label = "Volume percent", introduced = 1)]
    volume_percent: u8,
}

macro_rules! empty_action_config {
    ($name:ident, $id:literal, $title:literal) => {
        #[derive(Default, Deserialize, Serialize, ConfigSchema)]
        #[serde(deny_unknown_fields)]
        #[config(id = $id, version = 1, title = $title)]
        struct $name {}
    };
}

empty_action_config!(
    StartRecordingInputs,
    "obs.start_recording",
    "Start recording"
);
empty_action_config!(StopRecordingInputs, "obs.stop_recording", "Stop recording");
empty_action_config!(
    PauseRecordingInputs,
    "obs.pause_recording",
    "Pause recording"
);
empty_action_config!(
    ResumeRecordingInputs,
    "obs.resume_recording",
    "Resume recording"
);
#[derive(Default, Deserialize, Serialize, ConfigSchema)]
#[serde(deny_unknown_fields)]
#[config(
    id = "obs.get_recording_status",
    version = 1,
    title = "Get recording status",
    output("active", "Recording", "toggle"),
    output("paused", "Paused", "toggle"),
    output("timecode", "Timecode", "text"),
    output("duration_ms", "Duration (ms)", "number"),
    output("bytes", "Bytes", "number")
)]
struct RecordingStatusInputs {}
empty_action_config!(
    StartStreamingInputs,
    "obs.start_streaming",
    "Start streaming"
);
empty_action_config!(StopStreamingInputs, "obs.stop_streaming", "Stop streaming");
#[derive(Default, Deserialize, Serialize, ConfigSchema)]
#[serde(deny_unknown_fields)]
#[config(
    id = "obs.get_streaming_status",
    version = 1,
    title = "Get streaming status",
    output("active", "Streaming", "toggle"),
    output("reconnecting", "Reconnecting", "toggle"),
    output("timecode", "Timecode", "text"),
    output("duration_ms", "Duration (ms)", "number"),
    output("congestion", "Congestion", "number"),
    output("bytes", "Bytes", "number"),
    output("skipped_frames", "Skipped frames", "number"),
    output("total_frames", "Total frames", "number")
)]
struct StreamingStatusInputs {}
empty_action_config!(
    StartReplayInputs,
    "obs.start_replay_buffer",
    "Start replay buffer"
);
empty_action_config!(
    StopReplayInputs,
    "obs.stop_replay_buffer",
    "Stop replay buffer"
);
empty_action_config!(
    SaveReplayInputs,
    "obs.save_replay_buffer",
    "Save replay buffer"
);
#[derive(Default, Deserialize, Serialize, ConfigSchema)]
#[serde(deny_unknown_fields)]
#[config(
    id = "obs.get_replay_buffer_status",
    version = 1,
    title = "Get replay buffer status",
    output("active", "Replay buffer", "toggle")
)]
struct ReplayStatusInputs {}

#[derive(Default, Deserialize, Serialize, ConfigSchema)]
#[serde(deny_unknown_fields)]
#[config(
    id = "obs.create_record_chapter",
    version = 1,
    title = "Create recording chapter"
)]
struct CreateChapterInputs {
    #[config(id = "name", label = "Chapter name", introduced = 1)]
    name: String,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Action {
    SetScene,
    SetPreviewScene,
    SetItemEnabled,
    SetInputMute,
    SetInputVolume,
    CreateChapter,
    RecordingStart,
    RecordingStop,
    RecordingPause,
    RecordingResume,
    RecordingStatus,
    StreamingStart,
    StreamingStop,
    StreamingStatus,
    ReplayStart,
    ReplayStop,
    ReplaySave,
    ReplayStatus,
}

pub fn register(engine: &mut Engine, api: Arc<dyn ObsApi>) {
    for (definition, action) in [
        (ActionDefinition::of::<SetSceneInputs>(), Action::SetScene),
        (
            ActionDefinition::of::<SetPreviewSceneInputs>(),
            Action::SetPreviewScene,
        ),
        (
            ActionDefinition::of::<SetItemEnabledInputs>(),
            Action::SetItemEnabled,
        ),
        (
            ActionDefinition::of::<SetInputMuteInputs>(),
            Action::SetInputMute,
        ),
        (
            ActionDefinition::of::<SetInputVolumeInputs>(),
            Action::SetInputVolume,
        ),
        (
            ActionDefinition::of::<CreateChapterInputs>(),
            Action::CreateChapter,
        ),
        (
            ActionDefinition::of::<StartRecordingInputs>(),
            Action::RecordingStart,
        ),
        (
            ActionDefinition::of::<StopRecordingInputs>(),
            Action::RecordingStop,
        ),
        (
            ActionDefinition::of::<PauseRecordingInputs>(),
            Action::RecordingPause,
        ),
        (
            ActionDefinition::of::<ResumeRecordingInputs>(),
            Action::RecordingResume,
        ),
        (
            ActionDefinition::of::<RecordingStatusInputs>(),
            Action::RecordingStatus,
        ),
        (
            ActionDefinition::of::<StartStreamingInputs>(),
            Action::StreamingStart,
        ),
        (
            ActionDefinition::of::<StopStreamingInputs>(),
            Action::StreamingStop,
        ),
        (
            ActionDefinition::of::<StreamingStatusInputs>(),
            Action::StreamingStatus,
        ),
        (
            ActionDefinition::of::<StartReplayInputs>(),
            Action::ReplayStart,
        ),
        (
            ActionDefinition::of::<StopReplayInputs>(),
            Action::ReplayStop,
        ),
        (
            ActionDefinition::of::<SaveReplayInputs>(),
            Action::ReplaySave,
        ),
        (
            ActionDefinition::of::<ReplayStatusInputs>(),
            Action::ReplayStatus,
        ),
    ] {
        engine.register_lua_action_definition(
            definition,
            Arc::new(ObsAction {
                kind: action,
                api: Arc::clone(&api),
            }),
        );
    }
}

struct ObsAction {
    kind: Action,
    api: Arc<dyn ObsApi>,
}

impl Capability for ObsAction {
    fn validate_inputs(&self, inputs: &BTreeMap<String, Input>) -> Result<(), String> {
        let (schema, nonempty) = match self.kind {
            Action::SetScene => (SetSceneInputs::SCHEMA, &["scene"][..]),
            Action::SetPreviewScene => (SetPreviewSceneInputs::SCHEMA, &["scene"][..]),
            Action::SetItemEnabled => (SetItemEnabledInputs::SCHEMA, &["scene", "item"][..]),
            Action::SetInputMute => (SetInputMuteInputs::SCHEMA, &["input"][..]),
            Action::SetInputVolume => (SetInputVolumeInputs::SCHEMA, &["input"][..]),
            Action::CreateChapter => (CreateChapterInputs::SCHEMA, &["name"][..]),
            Action::RecordingStart => (StartRecordingInputs::SCHEMA, &[] as &[&str]),
            Action::RecordingStop => (StopRecordingInputs::SCHEMA, &[] as &[&str]),
            Action::RecordingPause => (PauseRecordingInputs::SCHEMA, &[] as &[&str]),
            Action::RecordingResume => (ResumeRecordingInputs::SCHEMA, &[] as &[&str]),
            Action::RecordingStatus => (RecordingStatusInputs::SCHEMA, &[] as &[&str]),
            Action::StreamingStart => (StartStreamingInputs::SCHEMA, &[] as &[&str]),
            Action::StreamingStop => (StopStreamingInputs::SCHEMA, &[] as &[&str]),
            Action::StreamingStatus => (StreamingStatusInputs::SCHEMA, &[] as &[&str]),
            Action::ReplayStart => (StartReplayInputs::SCHEMA, &[] as &[&str]),
            Action::ReplayStop => (StopReplayInputs::SCHEMA, &[] as &[&str]),
            Action::ReplaySave => (SaveReplayInputs::SCHEMA, &[] as &[&str]),
            Action::ReplayStatus => (ReplayStatusInputs::SCHEMA, &[] as &[&str]),
        };
        validate_configured_inputs(schema, inputs)?;
        for key in nonempty {
            if let Some(Input::Literal(Value::String(value))) = inputs.get(*key) {
                nonempty_text(value, key)?;
            }
        }
        if self.kind == Action::SetInputVolume
            && let Some(Input::Literal(Value::Number(number))) = inputs.get("volume_percent")
            && number.as_u64().is_none_or(|value| value > 100)
        {
            return Err("`volume_percent` must be an integer from 0 to 100".into());
        }
        Ok(())
    }

    fn default_deadline(&self) -> Duration {
        Duration::from_secs(30)
    }

    fn ready(&self) -> Result<(), String> {
        self.api.ready()
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
            let result = match self.kind {
                Action::SetScene => {
                    let config: SetSceneInputs = parse_inputs(inputs)?;
                    nonempty_text(&config.scene, "scene").map_err(CapabilityError::Failed)?;
                    if let Some(uuid) = parse_uuid_token(&config.scene, SCENE_TOKEN_PREFIX) {
                        let snapshot = self.api.resource_snapshot().await.map_err(map_error)?;
                        ensure_resource_uuid(&snapshot.scenes, uuid, "scene")?;
                    } else if config.scene.starts_with(SCENE_TOKEN_PREFIX) {
                        return Err(CapabilityError::Failed("invalid OBS scene token".into()));
                    } else {
                        let choices = self.api.scenes().await.map_err(map_error)?;
                        ensure_unique(&choices, &config.scene, "scene")?;
                    }
                    ensure_active(&cancel)?;
                    self.api.set_scene(&config.scene).await
                }
                Action::SetPreviewScene => {
                    let config: SetPreviewSceneInputs = parse_inputs(inputs)?;
                    nonempty_text(&config.scene, "scene").map_err(CapabilityError::Failed)?;
                    if let Some(uuid) = parse_uuid_token(&config.scene, SCENE_TOKEN_PREFIX) {
                        let snapshot = self.api.resource_snapshot().await.map_err(map_error)?;
                        ensure_resource_uuid(&snapshot.scenes, uuid, "scene")?;
                    } else if config.scene.starts_with(SCENE_TOKEN_PREFIX) {
                        return Err(CapabilityError::Failed("invalid OBS scene token".into()));
                    } else {
                        let choices = self.api.scenes().await.map_err(map_error)?;
                        ensure_unique(&choices, &config.scene, "scene")?;
                    }
                    ensure_active(&cancel)?;
                    self.api.set_preview_scene(&config.scene).await
                }
                Action::SetItemEnabled => {
                    let config: SetItemEnabledInputs = parse_inputs(inputs)?;
                    nonempty_text(&config.scene, "scene").map_err(CapabilityError::Failed)?;
                    nonempty_text(&config.item, "item").map_err(CapabilityError::Failed)?;
                    let item_token = parse_scene_item_token(&config.item);
                    if config.item.starts_with(ITEM_TOKEN_PREFIX) && item_token.is_none() {
                        return Err(CapabilityError::Failed(
                            "invalid OBS scene item token".into(),
                        ));
                    }
                    let scene_is_token = config.scene.starts_with(SCENE_TOKEN_PREFIX);
                    if item_token.is_some() || scene_is_token {
                        let snapshot = self.api.resource_snapshot().await.map_err(map_error)?;
                        let scene_uuid = if let Some(uuid) =
                            parse_uuid_token(&config.scene, SCENE_TOKEN_PREFIX)
                        {
                            ensure_resource_uuid(&snapshot.scenes, uuid, "scene")?;
                            uuid
                        } else if scene_is_token {
                            return Err(CapabilityError::Failed("invalid OBS scene token".into()));
                        } else {
                            unique_resource_name(&snapshot.scenes, &config.scene, "scene")?.uuid
                        };
                        if let Some(item) = item_token {
                            if item.scene_uuid != scene_uuid {
                                return Err(CapabilityError::Failed(
                                    "OBS scene item does not belong to the selected scene".into(),
                                ));
                            }
                            ensure_scene_item_present(&snapshot, &item)?;
                            ensure_active(&cancel)?;
                            self.api
                                .set_item_enabled(&config.item, item.item_id, config.enabled)
                                .await
                        } else {
                            let matching: Vec<_> = snapshot
                                .scene_items
                                .iter()
                                .filter(|entry| {
                                    entry.scene.uuid == scene_uuid
                                        && entry.source_name == config.item
                                })
                                .collect();
                            let entry = match matching.as_slice() {
                                [entry] => *entry,
                                [] => {
                                    return Err(CapabilityError::Failed(format!(
                                        "OBS item `{}` is no longer in scene `{}`",
                                        config.item, config.scene
                                    )));
                                }
                                _ => {
                                    return Err(CapabilityError::Failed(format!(
                                        "OBS scene `{}` contains multiple items named `{}`",
                                        config.scene, config.item
                                    )));
                                }
                            };
                            let stable_item_token = scene_item_token(entry);
                            ensure_active(&cancel)?;
                            self.api
                                .set_item_enabled(&stable_item_token, entry.item_id, config.enabled)
                                .await
                        }
                    } else {
                        let scenes = self.api.scenes().await.map_err(map_error)?;
                        ensure_unique(&scenes, &config.scene, "scene")?;
                        let items = self.api.items(&config.scene).await.map_err(map_error)?;
                        let matching: Vec<_> = items
                            .iter()
                            .filter(|entry| entry.name == config.item)
                            .collect();
                        let id = match matching.as_slice() {
                            [entry] => entry.id,
                            [] => {
                                return Err(CapabilityError::Failed(format!(
                                    "OBS item `{}` is no longer in scene `{}`",
                                    config.item, config.scene
                                )));
                            }
                            _ => {
                                return Err(CapabilityError::Failed(format!(
                                    "OBS scene `{}` contains multiple items named `{}`",
                                    config.scene, config.item
                                )));
                            }
                        };
                        ensure_active(&cancel)?;
                        self.api
                            .set_item_enabled(&config.scene, id, config.enabled)
                            .await
                    }
                }
                Action::CreateChapter => {
                    let config: CreateChapterInputs = parse_inputs(inputs)?;
                    nonempty_text(&config.name, "name").map_err(CapabilityError::Failed)?;
                    ensure_active(&cancel)?;
                    self.api.create_chapter(&config.name).await
                }
                Action::SetInputMute => {
                    let config: SetInputMuteInputs = parse_inputs(inputs)?;
                    nonempty_text(&config.input, "input").map_err(CapabilityError::Failed)?;
                    if let Some(uuid) = parse_uuid_token(&config.input, INPUT_TOKEN_PREFIX) {
                        let snapshot = self.api.resource_snapshot().await.map_err(map_error)?;
                        ensure_resource_uuid(&snapshot.inputs, uuid, "input")?;
                    } else if config.input.starts_with(INPUT_TOKEN_PREFIX) {
                        return Err(CapabilityError::Failed("invalid OBS input token".into()));
                    } else {
                        let choices = self.api.inputs().await.map_err(map_error)?;
                        ensure_unique(&choices, &config.input, "input")?;
                    }
                    ensure_active(&cancel)?;
                    self.api.set_input_muted(&config.input, config.muted).await
                }
                Action::SetInputVolume => {
                    let config: SetInputVolumeInputs = parse_inputs(inputs)?;
                    nonempty_text(&config.input, "input").map_err(CapabilityError::Failed)?;
                    if config.volume_percent > 100 {
                        return Err(CapabilityError::Failed(
                            "`volume_percent` must be an integer from 0 to 100".into(),
                        ));
                    }
                    if let Some(uuid) = parse_uuid_token(&config.input, INPUT_TOKEN_PREFIX) {
                        let snapshot = self.api.resource_snapshot().await.map_err(map_error)?;
                        ensure_resource_uuid(&snapshot.inputs, uuid, "input")?;
                    } else if config.input.starts_with(INPUT_TOKEN_PREFIX) {
                        return Err(CapabilityError::Failed("invalid OBS input token".into()));
                    } else {
                        let choices = self.api.inputs().await.map_err(map_error)?;
                        ensure_unique(&choices, &config.input, "input")?;
                    }
                    ensure_active(&cancel)?;
                    self.api
                        .set_input_volume(&config.input, config.volume_percent as f32 / 100.0)
                        .await
                }
                Action::RecordingStart => {
                    ensure_empty_inputs(inputs)?;
                    ensure_active(&cancel)?;
                    self.api.recording_start().await
                }
                Action::RecordingStop => {
                    ensure_empty_inputs(inputs)?;
                    ensure_active(&cancel)?;
                    self.api.recording_stop().await
                }
                Action::RecordingPause => {
                    ensure_empty_inputs(inputs)?;
                    ensure_active(&cancel)?;
                    self.api.recording_pause().await
                }
                Action::RecordingResume => {
                    ensure_empty_inputs(inputs)?;
                    ensure_active(&cancel)?;
                    self.api.recording_resume().await
                }
                Action::RecordingStatus => {
                    ensure_empty_inputs(inputs)?;
                    ensure_active(&cancel)?;
                    return self.api.recording_status().await.map_err(map_error);
                }
                Action::StreamingStart => {
                    ensure_empty_inputs(inputs)?;
                    ensure_active(&cancel)?;
                    self.api.streaming_start().await
                }
                Action::StreamingStop => {
                    ensure_empty_inputs(inputs)?;
                    ensure_active(&cancel)?;
                    self.api.streaming_stop().await
                }
                Action::StreamingStatus => {
                    ensure_empty_inputs(inputs)?;
                    ensure_active(&cancel)?;
                    return self.api.streaming_status().await.map_err(map_error);
                }
                Action::ReplayStart => {
                    ensure_empty_inputs(inputs)?;
                    ensure_active(&cancel)?;
                    self.api.replay_start().await
                }
                Action::ReplayStop => {
                    ensure_empty_inputs(inputs)?;
                    ensure_active(&cancel)?;
                    self.api.replay_stop().await
                }
                Action::ReplaySave => {
                    ensure_empty_inputs(inputs)?;
                    ensure_active(&cancel)?;
                    self.api.replay_save().await
                }
                Action::ReplayStatus => {
                    ensure_empty_inputs(inputs)?;
                    ensure_active(&cancel)?;
                    return self.api.replay_status().await.map_err(map_error);
                }
            };
            result.map_err(map_error)?;
            Ok(Values::new())
        })
    }
}

fn ensure_empty_inputs(inputs: Values) -> Result<(), CapabilityError> {
    if inputs.is_empty() {
        Ok(())
    } else {
        Err(CapabilityError::Failed("OBS action takes no inputs".into()))
    }
}

fn parse_inputs<T: DeserializeOwned>(inputs: Values) -> Result<T, CapabilityError> {
    serde_json::from_value(Value::Object(inputs.into_iter().collect()))
        .map_err(|error| CapabilityError::Failed(format!("invalid OBS action inputs: {error}")))
}

fn nonempty_text(value: &str, key: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("`{key}` must be nonempty text"));
    }
    Ok(())
}

fn ensure_unique(choices: &[String], requested: &str, kind: &str) -> Result<(), CapabilityError> {
    match choices
        .iter()
        .filter(|value| value.as_str() == requested)
        .count()
    {
        1 => Ok(()),
        0 => Err(CapabilityError::Failed(format!(
            "OBS {kind} `{requested}` is no longer available"
        ))),
        _ => Err(CapabilityError::Failed(format!(
            "OBS {kind} `{requested}` is ambiguous"
        ))),
    }
}

fn ensure_resource_uuid(
    resources: &[ObsResource],
    requested: uuid::Uuid,
    kind: &str,
) -> Result<(), CapabilityError> {
    match resources
        .iter()
        .filter(|resource| resource.uuid == requested)
        .count()
    {
        1 => Ok(()),
        0 => Err(CapabilityError::Failed(format!(
            "OBS {kind} token is stale or no longer available"
        ))),
        _ => Err(CapabilityError::Failed(format!(
            "OBS {kind} token is ambiguous"
        ))),
    }
}

fn unique_resource_name<'a>(
    resources: &'a [ObsResource],
    requested: &str,
    kind: &str,
) -> Result<&'a ObsResource, CapabilityError> {
    match resources
        .iter()
        .filter(|resource| resource.name == requested)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [resource] => Ok(*resource),
        [] => Err(CapabilityError::Failed(format!(
            "OBS {kind} `{requested}` is no longer available"
        ))),
        _ => Err(CapabilityError::Failed(format!(
            "OBS {kind} `{requested}` is ambiguous"
        ))),
    }
}

fn ensure_scene_item_present(
    snapshot: &ObsResourceSnapshot,
    requested: &SceneItemToken,
) -> Result<(), CapabilityError> {
    match snapshot
        .scene_items
        .iter()
        .filter(|entry| {
            entry.scene.uuid == requested.scene_uuid
                && entry.group_path == requested.group_path
                && entry.item_id == requested.item_id
                && entry.source_name == requested.source_name
        })
        .count()
    {
        1 => Ok(()),
        0 => Err(CapabilityError::Failed(
            "OBS scene item is stale or no longer available".into(),
        )),
        _ => Err(CapabilityError::Failed(
            "OBS scene item token is ambiguous".into(),
        )),
    }
}

fn ensure_active(cancel: &CancellationToken) -> Result<(), CapabilityError> {
    if cancel.is_cancelled() {
        Err(CapabilityError::Failed("action cancelled".into()))
    } else {
        Ok(())
    }
}

fn map_error(error: ObsError) -> CapabilityError {
    match error {
        ObsError::Unavailable(message) => CapabilityError::ConnectorUnavailable(message),
        ObsError::Failed(message) => CapabilityError::Failed(message),
        ObsError::Uncertain(message) => CapabilityError::Uncertain(message),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::schema::ConfigChoiceSource;

    #[test]
    fn status_outputs_and_resource_choice_sources_are_declared() {
        assert_eq!(
            RecordingStatusInputs::SCHEMA
                .outputs
                .iter()
                .map(|output| output.id)
                .collect::<Vec<_>>(),
            ["active", "paused", "timecode", "duration_ms", "bytes"]
        );
        assert_eq!(
            StreamingStatusInputs::SCHEMA
                .outputs
                .iter()
                .map(|output| output.id)
                .collect::<Vec<_>>(),
            [
                "active",
                "reconnecting",
                "timecode",
                "duration_ms",
                "congestion",
                "bytes",
                "skipped_frames",
                "total_frames"
            ]
        );
        assert_eq!(ReplayStatusInputs::SCHEMA.outputs[0].id, "active");
        assert_eq!(
            SetSceneInputs::SCHEMA.fields[0].choice_source,
            Some(ConfigChoiceSource {
                key: "obs.scenes",
                depends_on: None
            })
        );
        assert_eq!(
            SetInputMuteInputs::SCHEMA.fields[0].choice_source,
            Some(ConfigChoiceSource {
                key: "obs.inputs",
                depends_on: None
            })
        );
        assert_eq!(
            SetItemEnabledInputs::SCHEMA.fields[1].choice_source,
            Some(ConfigChoiceSource {
                key: "obs.scene_items",
                depends_on: Some("scene")
            })
        );
    }

    #[derive(Default)]
    struct FakeObs {
        calls: Mutex<Vec<String>>,
        failure: Mutex<Option<ObsError>>,
    }

    impl ObsApi for FakeObs {
        fn ready(&self) -> Result<(), String> {
            Ok(())
        }
        fn scenes(&self) -> ObsFuture<'_, Vec<String>> {
            Box::pin(async { Ok(vec!["Gameplay".into()]) })
        }
        fn inputs(&self) -> ObsFuture<'_, Vec<String>> {
            Box::pin(async { Ok(vec!["Mic/Aux".into()]) })
        }
        fn items<'a>(&'a self, _scene: &'a str) -> ObsFuture<'a, Vec<SceneItem>> {
            Box::pin(async {
                Ok(vec![SceneItem {
                    id: 27,
                    name: "Camera".into(),
                }])
            })
        }
        fn set_scene<'a>(&'a self, scene: &'a str) -> ObsFuture<'a, ()> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(format!("scene:{scene}"));
                Ok(())
            })
        }
        fn set_preview_scene<'a>(&'a self, scene: &'a str) -> ObsFuture<'a, ()> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(format!("preview:{scene}"));
                Ok(())
            })
        }
        fn set_item_enabled<'a>(
            &'a self,
            scene: &'a str,
            id: i64,
            enabled: bool,
        ) -> ObsFuture<'a, ()> {
            Box::pin(async move {
                self.calls
                    .lock()
                    .unwrap()
                    .push(format!("item:{scene}:{id}:{enabled}"));
                Ok(())
            })
        }
        fn create_chapter<'a>(&'a self, name: &'a str) -> ObsFuture<'a, ()> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(format!("chapter:{name}"));
                self.failure.lock().unwrap().take().map_or(Ok(()), Err)
            })
        }
        fn set_input_muted<'a>(&'a self, input: &'a str, muted: bool) -> ObsFuture<'a, ()> {
            Box::pin(async move {
                self.calls
                    .lock()
                    .unwrap()
                    .push(format!("mute:{input}:{muted}"));
                Ok(())
            })
        }
        fn set_input_volume<'a>(&'a self, input: &'a str, volume: f32) -> ObsFuture<'a, ()> {
            Box::pin(async move {
                self.calls
                    .lock()
                    .unwrap()
                    .push(format!("volume:{input}:{volume}"));
                Ok(())
            })
        }
        fn recording_start(&self) -> ObsFuture<'_, ()> {
            self.recording_command("record:start")
        }
        fn recording_stop(&self) -> ObsFuture<'_, ()> {
            self.recording_command("record:stop")
        }
        fn recording_pause(&self) -> ObsFuture<'_, ()> {
            self.recording_command("record:pause")
        }
        fn recording_resume(&self) -> ObsFuture<'_, ()> {
            self.recording_command("record:resume")
        }
        fn recording_status(&self) -> ObsFuture<'_, Values> {
            Box::pin(async {
                Ok(Values::from([
                    ("active".into(), Value::Bool(true)),
                    ("paused".into(), Value::Bool(false)),
                ]))
            })
        }
        fn streaming_start(&self) -> ObsFuture<'_, ()> {
            self.recording_command("stream:start")
        }
        fn streaming_stop(&self) -> ObsFuture<'_, ()> {
            self.recording_command("stream:stop")
        }
        fn streaming_status(&self) -> ObsFuture<'_, Values> {
            Box::pin(async { Ok(Values::from([("active".into(), Value::Bool(true))])) })
        }
        fn replay_start(&self) -> ObsFuture<'_, ()> {
            self.recording_command("replay:start")
        }
        fn replay_stop(&self) -> ObsFuture<'_, ()> {
            self.recording_command("replay:stop")
        }
        fn replay_save(&self) -> ObsFuture<'_, ()> {
            self.recording_command("replay:save")
        }
        fn replay_status(&self) -> ObsFuture<'_, Values> {
            Box::pin(async { Ok(Values::from([("active".into(), Value::Bool(true))])) })
        }
        fn resource_snapshot(&self) -> ObsFuture<'_, ObsResourceSnapshot> {
            Box::pin(async {
                Ok(ObsResourceSnapshot {
                    scenes: vec![ObsResource {
                        name: "Program".into(),
                        uuid: uuid::Uuid::nil(),
                    }],
                    inputs: vec![ObsResource {
                        name: "Mic/Aux".into(),
                        uuid: uuid::Uuid::nil(),
                    }],
                    scene_items: vec![ObsSceneItemResource {
                        scene: ObsResource {
                            name: "Program".into(),
                            uuid: uuid::Uuid::nil(),
                        },
                        group_path: vec!["Overlay".into()],
                        item_id: 12,
                        source_name: "Camera".into(),
                    }],
                    current_program_scene: Some(ObsResource {
                        name: "Program".into(),
                        uuid: uuid::Uuid::nil(),
                    }),
                    current_preview_scene: None,
                    recording: ObsRecordingStatus {
                        active: true,
                        paused: false,
                    },
                    streaming: ObsStreamingStatus {
                        active: false,
                        reconnecting: false,
                    },
                    replay_buffer_active: true,
                })
            })
        }
    }

    impl FakeObs {
        fn recording_command(&self, command: &'static str) -> ObsFuture<'_, ()> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(command.into());
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn resolves_current_scene_item_id_and_preserves_chapter_text() {
        let api = Arc::new(FakeObs::default());
        let item = ObsAction {
            kind: Action::SetItemEnabled,
            api: api.clone(),
        };
        item.execute(
            Values::from([
                ("scene".into(), Value::String("Gameplay".into())),
                ("item".into(), Value::String("Camera".into())),
                ("enabled".into(), Value::Bool(false)),
            ]),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let chapter = ObsAction {
            kind: Action::CreateChapter,
            api: api.clone(),
        };
        chapter
            .execute(
                Values::from([(
                    "name".into(),
                    Value::String("GAME CHANGE: Final Fantasy XIV".into()),
                )]),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            *api.calls.lock().unwrap(),
            [
                "item:Gameplay:27:false",
                "chapter:GAME CHANGE: Final Fantasy XIV"
            ]
        );
    }

    #[tokio::test]
    async fn failed_chapter_is_not_retried_and_uncertainty_is_preserved() {
        let api = Arc::new(FakeObs::default());
        *api.failure.lock().unwrap() =
            Some(ObsError::Uncertain("connection lost after send".into()));
        let chapter = ObsAction {
            kind: Action::CreateChapter,
            api: api.clone(),
        };
        let error = chapter
            .execute(
                Values::from([("name".into(), Value::String("TITLE CHANGE: Ready".into()))]),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, CapabilityError::Uncertain(_)));
        assert_eq!(api.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn stale_scene_is_reported_before_mutation() {
        let api = Arc::new(FakeObs::default());
        let action = ObsAction {
            kind: Action::SetScene,
            api: api.clone(),
        };
        let error = action
            .execute(
                Values::from([("scene".into(), Value::String("Gone".into()))]),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, CapabilityError::Failed(_)));
        assert!(api.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn uuid_scene_and_input_tokens_resolve_against_fresh_snapshot() {
        let api = Arc::new(FakeObs::default());
        let uuid = uuid::Uuid::nil();
        let scene = ObsAction {
            kind: Action::SetScene,
            api: api.clone(),
        };
        scene
            .execute(
                Values::from([(
                    "scene".into(),
                    Value::String(format!("{SCENE_TOKEN_PREFIX}{uuid}")),
                )]),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let mute = ObsAction {
            kind: Action::SetInputMute,
            api: api.clone(),
        };
        mute.execute(
            Values::from([
                (
                    "input".into(),
                    Value::String(format!("{INPUT_TOKEN_PREFIX}{uuid}")),
                ),
                ("muted".into(), Value::Bool(true)),
            ]),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            *api.calls.lock().unwrap(),
            [
                format!("scene:{SCENE_TOKEN_PREFIX}{uuid}"),
                format!("mute:{INPUT_TOKEN_PREFIX}{uuid}:true")
            ]
        );
    }

    #[tokio::test]
    async fn scene_item_token_binds_root_group_and_item_identity() {
        let api = Arc::new(FakeObs::default());
        let item_resource = ObsSceneItemResource {
            scene: ObsResource {
                name: "Program".into(),
                uuid: uuid::Uuid::nil(),
            },
            group_path: vec!["Overlay".into()],
            item_id: 12,
            source_name: "Camera".into(),
        };
        let action = ObsAction {
            kind: Action::SetItemEnabled,
            api: api.clone(),
        };
        let token = scene_item_token(&item_resource);
        action
            .execute(
                Values::from([
                    (
                        "scene".into(),
                        Value::String(scene_token(&item_resource.scene)),
                    ),
                    ("item".into(), Value::String(token.clone())),
                    ("enabled".into(), Value::Bool(false)),
                ]),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            *api.calls.lock().unwrap(),
            [format!("item:{token}:12:false")]
        );
    }

    #[tokio::test]
    async fn item_token_accepts_legacy_scene_and_uuid_scene_resolves_legacy_item() {
        let api = Arc::new(FakeObs::default());
        let root = ObsResource {
            name: "Program".into(),
            uuid: uuid::Uuid::nil(),
        };
        let item = ObsSceneItemResource {
            scene: root.clone(),
            group_path: vec!["Overlay".into()],
            item_id: 12,
            source_name: "Camera".into(),
        };
        let token = scene_item_token(&item);
        let action = ObsAction {
            kind: Action::SetItemEnabled,
            api: api.clone(),
        };
        action
            .execute(
                Values::from([
                    ("scene".into(), Value::String("Program".into())),
                    ("item".into(), Value::String(token.clone())),
                    ("enabled".into(), Value::Bool(true)),
                ]),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        action
            .execute(
                Values::from([
                    ("scene".into(), Value::String(scene_token(&root))),
                    ("item".into(), Value::String("Camera".into())),
                    ("enabled".into(), Value::Bool(false)),
                ]),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            *api.calls.lock().unwrap(),
            [
                format!("item:{token}:12:true"),
                format!("item:{token}:12:false")
            ]
        );
    }

    #[tokio::test]
    async fn input_volume_uses_percentage_and_status_actions_publish_outputs() {
        let api = Arc::new(FakeObs::default());
        let volume = ObsAction {
            kind: Action::SetInputVolume,
            api: api.clone(),
        };
        volume
            .execute(
                Values::from([
                    ("input".into(), Value::String("Mic/Aux".into())),
                    ("volume_percent".into(), Value::from(50)),
                ]),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(api.calls.lock().unwrap()[0], "volume:Mic/Aux:0.5");

        let status = ObsAction {
            kind: Action::RecordingStatus,
            api,
        };
        let result = status
            .execute(Values::new(), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result["active"], Value::Bool(true));
        assert_eq!(result["paused"], Value::Bool(false));
    }

    #[tokio::test]
    async fn missing_input_is_rejected_before_audio_changes() {
        let api = Arc::new(FakeObs::default());
        let action = ObsAction {
            kind: Action::SetInputMute,
            api: api.clone(),
        };
        let error = action
            .execute(
                Values::from([
                    ("input".into(), Value::String("Removed mic".into())),
                    ("muted".into(), Value::Bool(true)),
                ]),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, CapabilityError::Failed(_)));
        assert!(api.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn resource_snapshot_keeps_uuid_and_nested_group_path() {
        let api = Arc::new(FakeObs::default());
        let snapshot = api.resource_snapshot().await.unwrap();
        assert_eq!(snapshot.scenes[0].name, "Program");
        assert_eq!(
            snapshot.current_program_scene.as_ref().unwrap().uuid,
            uuid::Uuid::nil()
        );
        assert_eq!(snapshot.scene_items[0].group_path, ["Overlay"]);
        assert_eq!(snapshot.scene_items[0].item_id, 12);
        assert!(snapshot.recording.active);
        assert!(!snapshot.streaming.active);
        assert!(snapshot.replay_buffer_active);
    }

    #[test]
    fn input_volume_rejects_values_over_one_hundred_percent() {
        let action = ObsAction {
            kind: Action::SetInputVolume,
            api: Arc::new(FakeObs::default()),
        };
        let inputs = BTreeMap::from([
            (
                "input".into(),
                Input::Literal(Value::String("Mic/Aux".into())),
            ),
            ("volume_percent".into(), Input::Literal(Value::from(101))),
        ]);
        assert!(
            action
                .validate_inputs(&inputs)
                .unwrap_err()
                .contains("0 to 100")
        );
    }

    #[test]
    fn obs_action_schemas_validate_saved_inputs() {
        let api = Arc::new(FakeObs::default());
        let scene = ObsAction {
            kind: Action::SetScene,
            api: api.clone(),
        };
        assert_eq!(SetSceneInputs::SCHEMA.fields[0].id, "scene");
        assert!(scene.validate_inputs(&BTreeMap::new()).is_err());
        assert!(
            scene
                .validate_inputs(&BTreeMap::from([(
                    "scene".into(),
                    Input::Literal(Value::String("  ".into())),
                )]))
                .is_err()
        );
        assert!(
            scene
                .validate_inputs(&BTreeMap::from([(
                    "scene".into(),
                    Input::Literal(Value::String("Gameplay".into())),
                )]))
                .is_ok()
        );

        let item = ObsAction {
            kind: Action::SetItemEnabled,
            api,
        };
        assert_eq!(SetItemEnabledInputs::SCHEMA.fields.len(), 3);
        let valid = BTreeMap::from([
            (
                "scene".into(),
                Input::Literal(Value::String("Gameplay".into())),
            ),
            (
                "item".into(),
                Input::Literal(Value::String("Camera".into())),
            ),
            ("enabled".into(), Input::Literal(Value::Bool(false))),
        ]);
        assert!(item.validate_inputs(&valid).is_ok());
        let mut invalid = valid;
        invalid.insert(
            "enabled".into(),
            Input::Literal(Value::String("false".into())),
        );
        assert!(item.validate_inputs(&invalid).is_err());
    }

    #[tokio::test]
    async fn malformed_obs_inputs_are_rejected_before_mutation() {
        let api = Arc::new(FakeObs::default());
        for (kind, inputs) in [
            (Action::SetScene, Values::new()),
            (
                Action::SetScene,
                Values::from([("scene".into(), Value::String("  ".into()))]),
            ),
            (
                Action::SetItemEnabled,
                Values::from([("scene".into(), Value::String("Gameplay".into()))]),
            ),
            (
                Action::CreateChapter,
                Values::from([("name".into(), Value::String("".into()))]),
            ),
            (
                Action::CreateChapter,
                Values::from([
                    ("name".into(), Value::String("Chapter".into())),
                    ("extra".into(), Value::Bool(true)),
                ]),
            ),
        ] {
            let action = ObsAction {
                kind,
                api: api.clone(),
            };
            assert!(
                action
                    .execute(inputs, CancellationToken::new())
                    .await
                    .is_err()
            );
        }
        assert!(api.calls.lock().unwrap().is_empty());
    }
}
