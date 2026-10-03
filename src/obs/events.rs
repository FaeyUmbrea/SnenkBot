//! Demand-driven OBS event routing for saved workflow triggers.

use obws::events::{Event, OutputState};
use obws::requests::EventSubscription;
use serde_json::Value;
use uuid::Uuid;

use crate::WorkflowValues;
use crate::engine::Values;
use crate::schema::{ConfigOutput, DescribeValues};
use crate::workflows::{TriggerDefinition, TriggerKind, WorkflowDefinition};

#[derive(WorkflowValues)]
struct SceneNameValues {
    #[value(label = "Scene name")]
    scene_name: String,
}

#[derive(WorkflowValues)]
struct SceneValues {
    #[value(label = "Scene name")]
    scene_name: String,
    #[value(label = "Scene UUID")]
    scene_uuid: String,
}

#[derive(WorkflowValues)]
struct SceneItemValues {
    #[value(label = "Scene name")]
    scene_name: String,
    #[value(label = "Scene UUID")]
    scene_uuid: String,
    #[value(label = "Item ID")]
    item_id: u64,
    #[value(label = "Enabled")]
    enabled: bool,
}

#[derive(WorkflowValues)]
struct InputMuteValues {
    #[value(label = "Input name")]
    input_name: String,
    #[value(label = "Input UUID")]
    input_uuid: String,
    #[value(label = "Muted")]
    muted: bool,
}

#[derive(WorkflowValues)]
struct RecordingValues {
    #[value(label = "Recording active")]
    recording_active: bool,
}

#[derive(WorkflowValues)]
struct RecordingStoppedValues {
    #[value(label = "Recording active")]
    recording_active: bool,
    #[value(label = "Path")]
    path: Option<String>,
}

#[derive(WorkflowValues)]
struct RecordingPausedValues {
    #[value(label = "Recording active")]
    recording_active: bool,
    #[value(label = "Recording paused")]
    recording_paused: bool,
}

#[derive(WorkflowValues)]
struct StreamingValues {
    #[value(label = "Streaming active")]
    streaming_active: bool,
}

#[derive(WorkflowValues)]
struct ReplayBufferValues {
    #[value(label = "Path")]
    path: String,
}

pub fn trigger_title(kind: &TriggerKind) -> Option<&'static str> {
    let event = match kind {
        TriggerKind::ObsRecordingStarted => ObsEventKind::RecordingStarted,
        TriggerKind::ObsCurrentScene { .. } => return Some("OBS scene changed"),
        TriggerKind::IntegrationEvent {
            integration, event, ..
        } if integration == "obs" => ObsEventKind::parse(event)?,
        _ => return None,
    };
    Some(match event {
        ObsEventKind::ProgramSceneChanged => "OBS program scene changed",
        ObsEventKind::PreviewSceneChanged => "OBS preview scene changed",
        ObsEventKind::SceneItemEnabledChanged => "OBS scene item visibility changed",
        ObsEventKind::InputMuteChanged => "OBS input mute changed",
        ObsEventKind::RecordingStarted => "OBS recording started",
        ObsEventKind::RecordingStopped => "OBS recording stopped",
        ObsEventKind::RecordingPaused => "OBS recording paused",
        ObsEventKind::RecordingResumed => "OBS recording resumed",
        ObsEventKind::StreamingStarted => "OBS streaming started",
        ObsEventKind::StreamingStopped => "OBS streaming stopped",
        ObsEventKind::ReplayBufferSaved => "OBS replay buffer saved",
    })
}

pub fn trigger_value_schema(kind: &TriggerKind) -> Option<Vec<ConfigOutput>> {
    let outputs = match kind {
        TriggerKind::ObsRecordingStarted => RecordingValues::OUTPUTS,
        TriggerKind::ObsCurrentScene { .. } => SceneNameValues::OUTPUTS,
        TriggerKind::IntegrationEvent {
            integration, event, ..
        } if integration == "obs" => match ObsEventKind::parse(event)? {
            ObsEventKind::ProgramSceneChanged | ObsEventKind::PreviewSceneChanged => {
                SceneValues::OUTPUTS
            }
            ObsEventKind::SceneItemEnabledChanged => SceneItemValues::OUTPUTS,
            ObsEventKind::InputMuteChanged => InputMuteValues::OUTPUTS,
            ObsEventKind::RecordingStarted => RecordingValues::OUTPUTS,
            ObsEventKind::RecordingStopped => RecordingStoppedValues::OUTPUTS,
            ObsEventKind::RecordingPaused | ObsEventKind::RecordingResumed => {
                RecordingPausedValues::OUTPUTS
            }
            ObsEventKind::StreamingStarted | ObsEventKind::StreamingStopped => {
                StreamingValues::OUTPUTS
            }
            ObsEventKind::ReplayBufferSaved => ReplayBufferValues::OUTPUTS,
        },
        _ => return None,
    };
    Some(outputs.to_vec())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ObsRouteKind {
    RecordingStarted,
    CurrentScene(String),
    Event {
        kind: ObsEventKind,
        filter: Option<String>,
        resource_uuid: Option<Uuid>,
        item_id: Option<u64>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObsEventKind {
    ProgramSceneChanged,
    PreviewSceneChanged,
    SceneItemEnabledChanged,
    InputMuteChanged,
    RecordingStarted,
    RecordingStopped,
    RecordingPaused,
    RecordingResumed,
    StreamingStarted,
    StreamingStopped,
    ReplayBufferSaved,
}

impl ObsEventKind {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "program_scene.changed" => Self::ProgramSceneChanged,
            "preview_scene.changed" => Self::PreviewSceneChanged,
            "scene_item.enabled_changed" => Self::SceneItemEnabledChanged,
            "input.mute_changed" => Self::InputMuteChanged,
            "recording.started" => Self::RecordingStarted,
            "recording.stopped" => Self::RecordingStopped,
            "recording.paused" => Self::RecordingPaused,
            "recording.resumed" => Self::RecordingResumed,
            "streaming.started" => Self::StreamingStarted,
            "streaming.stopped" => Self::StreamingStopped,
            "replay_buffer.saved" => Self::ReplayBufferSaved,
            _ => return None,
        })
    }

    fn filter_key(self) -> Option<&'static str> {
        match self {
            Self::ProgramSceneChanged
            | Self::PreviewSceneChanged
            | Self::SceneItemEnabledChanged => Some("scene"),
            Self::InputMuteChanged => Some("input"),
            _ => None,
        }
    }

    fn uuid_filter_key(self) -> Option<&'static str> {
        match self.filter_key() {
            Some("scene") => Some("scene_uuid"),
            Some("input") => Some("input_uuid"),
            _ => None,
        }
    }

    fn subscription(self) -> EventSubscription {
        match self {
            Self::ProgramSceneChanged | Self::PreviewSceneChanged => EventSubscription::SCENES,
            Self::SceneItemEnabledChanged => EventSubscription::SCENE_ITEMS,
            Self::InputMuteChanged => EventSubscription::INPUTS,
            _ => EventSubscription::OUTPUTS,
        }
    }
}

pub fn validate_trigger(trigger: &TriggerDefinition) -> Result<(), String> {
    let TriggerKind::IntegrationEvent {
        integration,
        event,
        filters,
    } = &trigger.kind
    else {
        return Err("expected an OBS integration event".into());
    };
    if integration != "obs" {
        return Err("expected an OBS integration event".into());
    }
    let kind =
        ObsEventKind::parse(event).ok_or_else(|| format!("unsupported OBS event `{event}`"))?;
    let max_filters = usize::from(kind.filter_key().is_some())
        + usize::from(kind.uuid_filter_key().is_some())
        + usize::from(kind == ObsEventKind::SceneItemEnabledChanged);
    if filters.len() > max_filters {
        return Err(format!("unsupported filters for OBS event `{event}`"));
    }
    for (key, value) in filters {
        let valid = if key == "item_id" && kind == ObsEventKind::SceneItemEnabledChanged {
            value.as_u64().is_some_and(|id| id > 0)
        } else if Some(key.as_str()) == kind.uuid_filter_key() {
            value
                .as_str()
                .is_some_and(|text| Uuid::parse_str(text).is_ok())
        } else {
            Some(key.as_str()) == kind.filter_key()
                && matches!(value, Value::String(text) if !text.trim().is_empty() && text == text.trim())
        };
        if !valid {
            return Err(format!("invalid `{key}` filter for OBS event `{event}`"));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObsRoute {
    pub workflow_id: String,
    pub trigger_id: String,
    pub kind: ObsRouteKind,
}

pub struct ObsActivation {
    pub workflow_id: String,
    pub trigger_id: String,
    pub values: Values,
}

pub fn routes_for(definition: &WorkflowDefinition) -> Vec<ObsRoute> {
    if !definition.enabled {
        return Vec::new();
    }
    definition
        .triggers
        .iter()
        .filter(|trigger| trigger.enabled)
        .filter_map(|trigger| {
            let kind = match &trigger.kind {
                TriggerKind::ObsRecordingStarted => ObsRouteKind::RecordingStarted,
                TriggerKind::ObsCurrentScene { scene } => ObsRouteKind::CurrentScene(scene.clone()),
                TriggerKind::IntegrationEvent {
                    integration,
                    event,
                    filters,
                } if integration == "obs" => {
                    let kind = ObsEventKind::parse(event)?;
                    let filter = kind
                        .filter_key()
                        .and_then(|key| filters.get(key))
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    let resource_uuid = kind
                        .uuid_filter_key()
                        .and_then(|key| filters.get(key))
                        .and_then(Value::as_str)
                        .and_then(|text| Uuid::parse_str(text).ok());
                    let item_id = filters.get("item_id").and_then(Value::as_u64);
                    ObsRouteKind::Event {
                        kind,
                        filter,
                        resource_uuid,
                        item_id,
                    }
                }
                _ => return None,
            };
            Some(ObsRoute {
                workflow_id: definition.workflow.id.clone(),
                trigger_id: trigger.id.clone(),
                kind,
            })
        })
        .collect()
}

pub(super) fn subscriptions(routes: &[ObsRoute]) -> EventSubscription {
    routes.iter().fold(EventSubscription::NONE, |bits, route| {
        bits | match route.kind {
            ObsRouteKind::RecordingStarted => EventSubscription::OUTPUTS,
            ObsRouteKind::CurrentScene(_) => EventSubscription::SCENES,
            ObsRouteKind::Event { kind, .. } => kind.subscription(),
        }
    })
}

pub(super) fn activations(routes: &[ObsRoute], event: &Event) -> Vec<ObsActivation> {
    routes
        .iter()
        .filter_map(|route| {
            let values = match (&route.kind, event) {
                (
                    ObsRouteKind::RecordingStarted,
                    Event::RecordStateChanged {
                        active: true,
                        state: OutputState::Started,
                        ..
                    },
                ) => RecordingValues {
                    recording_active: true,
                }
                .values(),
                (
                    ObsRouteKind::CurrentScene(expected),
                    Event::CurrentProgramSceneChanged { id },
                ) if expected == &id.name => SceneNameValues {
                    scene_name: id.name.clone(),
                }
                .values(),
                (
                    ObsRouteKind::Event {
                        kind,
                        filter,
                        resource_uuid,
                        item_id,
                    },
                    event,
                ) => event_values(*kind, filter.as_deref(), *resource_uuid, *item_id, event)?,
                _ => return None,
            };
            Some(ObsActivation {
                workflow_id: route.workflow_id.clone(),
                trigger_id: route.trigger_id.clone(),
                values,
            })
        })
        .collect()
}

fn event_values(
    kind: ObsEventKind,
    filter: Option<&str>,
    resource_uuid: Option<Uuid>,
    expected_item_id: Option<u64>,
    event: &Event,
) -> Option<Values> {
    let values = match (kind, event) {
        (ObsEventKind::ProgramSceneChanged, Event::CurrentProgramSceneChanged { id })
        | (ObsEventKind::PreviewSceneChanged, Event::CurrentPreviewSceneChanged { id })
            if matches_resource(filter, resource_uuid, &id.name, id.uuid) =>
        {
            SceneValues {
                scene_name: id.name.clone(),
                scene_uuid: id.uuid.to_string(),
            }
            .values()
        }
        (
            ObsEventKind::SceneItemEnabledChanged,
            Event::SceneItemEnableStateChanged {
                scene,
                item_id,
                enabled,
            },
        ) if matches_resource(filter, resource_uuid, &scene.name, scene.uuid)
            && expected_item_id.is_none_or(|expected| expected == *item_id) =>
        {
            SceneItemValues {
                scene_name: scene.name.clone(),
                scene_uuid: scene.uuid.to_string(),
                item_id: *item_id,
                enabled: *enabled,
            }
            .values()
        }
        (ObsEventKind::InputMuteChanged, Event::InputMuteStateChanged { id, muted })
            if matches_resource(filter, resource_uuid, &id.name, id.uuid) =>
        {
            InputMuteValues {
                input_name: id.name.clone(),
                input_uuid: id.uuid.to_string(),
                muted: *muted,
            }
            .values()
        }
        (
            ObsEventKind::RecordingStarted,
            Event::RecordStateChanged {
                active: true,
                state: OutputState::Started,
                ..
            },
        ) => RecordingValues {
            recording_active: true,
        }
        .values(),
        (
            ObsEventKind::RecordingStopped,
            Event::RecordStateChanged {
                active: false,
                state: OutputState::Stopped,
                path,
            },
        ) => RecordingStoppedValues {
            recording_active: false,
            path: path.clone(),
        }
        .values(),
        (
            ObsEventKind::RecordingPaused,
            Event::RecordStateChanged {
                active: true,
                state: OutputState::Paused,
                ..
            },
        ) => RecordingPausedValues {
            recording_active: true,
            recording_paused: true,
        }
        .values(),
        (
            ObsEventKind::RecordingResumed,
            Event::RecordStateChanged {
                active: true,
                state: OutputState::Resumed,
                ..
            },
        ) => RecordingPausedValues {
            recording_active: true,
            recording_paused: false,
        }
        .values(),
        (
            ObsEventKind::StreamingStarted,
            Event::StreamStateChanged {
                active: true,
                state: OutputState::Started,
            },
        ) => StreamingValues {
            streaming_active: true,
        }
        .values(),
        (
            ObsEventKind::StreamingStopped,
            Event::StreamStateChanged {
                active: false,
                state: OutputState::Stopped,
            },
        ) => StreamingValues {
            streaming_active: false,
        }
        .values(),
        (ObsEventKind::ReplayBufferSaved, Event::ReplayBufferSaved { path }) => {
            ReplayBufferValues {
                path: path.to_string_lossy().into_owned(),
            }
            .values()
        }
        _ => return None,
    };
    Some(values)
}

fn matches_resource(
    filter: Option<&str>,
    uuid: Option<Uuid>,
    name: &str,
    actual_uuid: Uuid,
) -> bool {
    if let Some(uuid) = uuid {
        uuid == actual_uuid
    } else {
        filter.is_none_or(|expected| expected == name)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use obws::responses::inputs::InputId;
    use obws::responses::scenes::SceneId;
    use serde_json::json;

    use super::*;
    use crate::engine::Workflow;
    use crate::workflows::{TriggerDefinition, TriggerKind};

    fn assert_schema_matches(kind: &TriggerKind, values: &Values) {
        let outputs = trigger_value_schema(kind).unwrap();
        assert_eq!(
            outputs
                .iter()
                .map(|output| output.id)
                .collect::<BTreeSet<_>>(),
            values.keys().map(String::as_str).collect::<BTreeSet<_>>()
        );
    }

    #[test]
    fn enabled_triggers_share_only_required_subscriptions() {
        let definition = WorkflowDefinition {
            enabled: true,
            workflow: Workflow {
                id: "markers".into(),
                revision: 1,
                overlap: false,
                steps: Vec::new(),
                outputs: BTreeMap::new(),
            },
            triggers: vec![
                TriggerDefinition {
                    id: "record".into(),
                    enabled: true,
                    kind: TriggerKind::ObsRecordingStarted,
                },
                TriggerDefinition {
                    id: "scene".into(),
                    enabled: true,
                    kind: TriggerKind::ObsCurrentScene {
                        scene: "Gameplay".into(),
                    },
                },
                TriggerDefinition {
                    id: "off".into(),
                    enabled: false,
                    kind: TriggerKind::ObsRecordingStarted,
                },
            ],
            name: None,
        };
        let routes = routes_for(&definition);
        assert_eq!(routes.len(), 2);
        assert_eq!(
            subscriptions(&routes),
            EventSubscription::OUTPUTS | EventSubscription::SCENES
        );
        let recording = Event::RecordStateChanged {
            active: true,
            state: OutputState::Started,
            path: None,
        };
        let recording_activations = activations(&routes, &recording);
        assert_eq!(recording_activations.len(), 1);
        assert_eq!(recording_activations[0].trigger_id, "record");
        assert_schema_matches(
            &definition.triggers[0].kind,
            &recording_activations[0].values,
        );
        let starting = Event::RecordStateChanged {
            active: true,
            state: OutputState::Starting,
            path: None,
        };
        assert!(activations(&routes, &starting).is_empty());
        let scene = Event::CurrentProgramSceneChanged {
            id: SceneId {
                name: "Gameplay".into(),
                ..SceneId::default()
            },
        };
        let scene_activations = activations(&routes, &scene);
        assert_eq!(scene_activations.len(), 1);
        assert_eq!(scene_activations[0].values["scene_name"], "Gameplay");
        assert_schema_matches(&definition.triggers[1].kind, &scene_activations[0].values);
        let other = Event::CurrentProgramSceneChanged {
            id: SceneId {
                name: "Starting Soon".into(),
                ..SceneId::default()
            },
        };
        assert!(activations(&routes, &other).is_empty());
        let mut disabled = definition;
        disabled.enabled = false;
        assert!(routes_for(&disabled).is_empty());
    }

    fn integration_trigger(event: &str, filters: BTreeMap<String, Value>) -> TriggerDefinition {
        TriggerDefinition {
            id: event.replace('.', "-"),
            enabled: true,
            kind: TriggerKind::IntegrationEvent {
                integration: "obs".into(),
                event: event.into(),
                filters,
            },
        }
    }

    #[test]
    fn typed_obs_events_validate_filters_and_route_only_matching_resources() {
        let trigger = integration_trigger(
            "input.mute_changed",
            BTreeMap::from([("input".into(), json!("Microphone"))]),
        );
        assert!(validate_trigger(&trigger).is_ok());
        let definition = WorkflowDefinition {
            enabled: true,
            workflow: Workflow {
                id: "mute-flow".into(),
                revision: 1,
                overlap: false,
                steps: Vec::new(),
                outputs: BTreeMap::new(),
            },
            triggers: vec![trigger],
            name: None,
        };
        let routes = routes_for(&definition);
        assert_eq!(subscriptions(&routes), EventSubscription::INPUTS);
        let event = Event::InputMuteStateChanged {
            id: InputId {
                name: "Microphone".into(),
                ..InputId::default()
            },
            muted: true,
        };
        let matched = activations(&routes, &event);
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].values["muted"], true);
        assert_eq!(matched[0].values["input_name"], "Microphone");
        assert!(
            activations(
                &routes,
                &Event::InputMuteStateChanged {
                    id: InputId {
                        name: "Desktop Audio".into(),
                        ..InputId::default()
                    },
                    muted: true,
                }
            )
            .is_empty()
        );
        assert!(
            validate_trigger(&integration_trigger(
                "input.mute_changed",
                BTreeMap::from([("scene".into(), json!("Gameplay"))])
            ))
            .is_err()
        );
        assert!(
            validate_trigger(&integration_trigger(
                "streaming.started",
                BTreeMap::from([("input".into(), json!("Microphone"))])
            ))
            .is_err()
        );
        assert!(validate_trigger(&integration_trigger("unknown", BTreeMap::new())).is_err());
        let item_trigger = integration_trigger(
            "scene_item.enabled_changed",
            BTreeMap::from([
                ("scene".into(), json!("Gameplay")),
                ("scene_uuid".into(), json!(Uuid::nil().to_string())),
                ("item_id".into(), json!(7)),
            ]),
        );
        assert!(validate_trigger(&item_trigger).is_ok());
        assert!(
            validate_trigger(&integration_trigger(
                "scene_item.enabled_changed",
                BTreeMap::from([("item_id".into(), json!(0))])
            ))
            .is_err()
        );
        assert!(
            validate_trigger(&integration_trigger(
                "program_scene.changed",
                BTreeMap::from([("scene_uuid".into(), json!("not-a-uuid"))])
            ))
            .is_err()
        );
        let mut item_definition = definition.clone();
        item_definition.triggers = vec![item_trigger];
        let item_routes = routes_for(&item_definition);
        assert_eq!(subscriptions(&item_routes), EventSubscription::SCENE_ITEMS);
        let item_event = Event::SceneItemEnableStateChanged {
            scene: SceneId {
                name: "Gameplay".into(),
                ..SceneId::default()
            },
            item_id: 7,
            enabled: false,
        };
        assert_eq!(
            activations(&item_routes, &item_event)[0].values["enabled"],
            false
        );
        assert_eq!(
            activations(
                &item_routes,
                &Event::SceneItemEnableStateChanged {
                    scene: SceneId {
                        name: "Renamed Gameplay".into(),
                        ..SceneId::default()
                    },
                    item_id: 7,
                    enabled: true,
                }
            )[0]
            .values["enabled"],
            true
        );
        assert!(
            activations(
                &item_routes,
                &Event::SceneItemEnableStateChanged {
                    scene: SceneId {
                        name: "Gameplay".into(),
                        ..SceneId::default()
                    },
                    item_id: 8,
                    enabled: false,
                }
            )
            .is_empty()
        );
    }

    #[test]
    fn output_events_ignore_intermediate_states() {
        let definition = WorkflowDefinition {
            enabled: true,
            workflow: Workflow {
                id: "output-flow".into(),
                revision: 1,
                overlap: false,
                steps: Vec::new(),
                outputs: BTreeMap::new(),
            },
            triggers: vec![
                integration_trigger("recording.stopped", BTreeMap::new()),
                integration_trigger("streaming.started", BTreeMap::new()),
                integration_trigger("replay_buffer.saved", BTreeMap::new()),
            ],
            name: None,
        };
        let routes = routes_for(&definition);
        assert_eq!(subscriptions(&routes), EventSubscription::OUTPUTS);
        let stopping = Event::RecordStateChanged {
            active: true,
            state: OutputState::Stopping,
            path: None,
        };
        assert!(activations(&routes, &stopping).is_empty());
        let stopped = Event::RecordStateChanged {
            active: false,
            state: OutputState::Stopped,
            path: Some("recording.mkv".into()),
        };
        assert_eq!(
            activations(&routes, &stopped)[0].values["path"],
            "recording.mkv"
        );
        assert_schema_matches(
            &definition.triggers[0].kind,
            &activations(&routes, &stopped)[0].values,
        );
        assert!(
            !trigger_value_schema(&definition.triggers[0].kind)
                .unwrap()
                .iter()
                .find(|output| output.id == "path")
                .unwrap()
                .required
        );
        let starting = Event::StreamStateChanged {
            active: true,
            state: OutputState::Starting,
        };
        assert!(activations(&routes, &starting).is_empty());
        let started = Event::StreamStateChanged {
            active: true,
            state: OutputState::Started,
        };
        assert_eq!(
            activations(&routes, &started)[0].values["streaming_active"],
            true
        );
        assert_schema_matches(
            &definition.triggers[1].kind,
            &activations(&routes, &started)[0].values,
        );
    }
}
