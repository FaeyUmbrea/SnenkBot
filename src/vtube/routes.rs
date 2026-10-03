//! Demand-driven VTube Studio event routing. One subscription serves every matching workflow.

use std::collections::BTreeSet;

use serde_json::Value;
use vtubestudio::data::Event;

use crate::WorkflowValues;
use crate::engine::Values;
use crate::schema::{ConfigOutput, DescribeValues};
use crate::workflows::{TriggerDefinition, TriggerKind, WorkflowDefinition};

#[derive(WorkflowValues)]
struct ModelValues {
    #[value(label = "Model ID")]
    model_id: String,
    #[value(label = "Model name")]
    model_name: String,
    #[value(label = "Model loaded")]
    model_loaded: bool,
}

#[derive(WorkflowValues)]
struct HotkeyValues {
    #[value(label = "Model ID")]
    model_id: String,
    #[value(label = "Model name")]
    model_name: String,
    #[value(label = "Hotkey ID")]
    hotkey_id: String,
    #[value(label = "Hotkey name")]
    hotkey_name: String,
    #[value(label = "Hotkey action")]
    hotkey_action: String,
    #[value(label = "Triggered by API")]
    triggered_by_api: bool,
    #[value(label = "Live2D item")]
    is_live2d_item: bool,
}

pub fn trigger_title(kind: &TriggerKind) -> Option<&'static str> {
    let TriggerKind::IntegrationEvent {
        integration, event, ..
    } = kind
    else {
        return None;
    };
    if integration != "vtube_studio" {
        return None;
    }
    Some(match event.as_str() {
        "model.loaded" => "VTube Studio model loaded",
        "model.unloaded" => "VTube Studio model unloaded",
        "hotkey.triggered" => "VTube Studio hotkey triggered",
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
    if integration != "vtube_studio" {
        return None;
    }
    match event.as_str() {
        "model.loaded" | "model.unloaded" => Some(ModelValues::OUTPUTS.to_vec()),
        "hotkey.triggered" => Some(HotkeyValues::OUTPUTS.to_vec()),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum VtubeEventKind {
    ModelLoaded,
    HotkeyTriggered,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VtubeRouteKind {
    Model {
        loaded: bool,
        model_id: Option<String>,
    },
    Hotkey {
        model_id: Option<String>,
        hotkey_id: Option<String>,
        ignore_api: bool,
    },
}

impl VtubeRouteKind {
    pub fn event_kind(&self) -> VtubeEventKind {
        match self {
            Self::Model { .. } => VtubeEventKind::ModelLoaded,
            Self::Hotkey { .. } => VtubeEventKind::HotkeyTriggered,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VtubeRoute {
    pub workflow_id: String,
    pub trigger_id: String,
    pub kind: VtubeRouteKind,
}

pub struct VtubeActivation {
    pub workflow_id: String,
    pub trigger_id: String,
    pub values: Values,
}

pub fn validate_trigger(trigger: &TriggerDefinition) -> Result<(), String> {
    let TriggerKind::IntegrationEvent {
        integration,
        event,
        filters,
    } = &trigger.kind
    else {
        return Err("expected a VTube Studio integration event".into());
    };
    if integration != "vtube_studio" {
        return Err("expected a VTube Studio integration event".into());
    }
    let allowed: &[&str] = match event.as_str() {
        "model.loaded" | "model.unloaded" => &["model_id"],
        "hotkey.triggered" => &["model_id", "hotkey_id", "ignore_api"],
        _ => return Err(format!("unsupported VTube Studio event `{event}`")),
    };
    for (key, value) in filters {
        if !allowed.contains(&key.as_str()) {
            return Err(format!(
                "unsupported `{key}` filter for VTube Studio event `{event}`"
            ));
        }
        let valid = match key.as_str() {
            "model_id" | "hotkey_id" => value.as_str().is_some_and(is_resource_id),
            "ignore_api" => value.is_boolean(),
            _ => false,
        };
        if !valid {
            return Err(format!(
                "invalid `{key}` filter for VTube Studio event `{event}`"
            ));
        }
    }
    Ok(())
}

fn is_resource_id(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub fn routes_for(definition: &WorkflowDefinition) -> Vec<VtubeRoute> {
    if !definition.enabled {
        return Vec::new();
    }
    definition
        .triggers
        .iter()
        .filter(|trigger| trigger.enabled)
        .filter_map(|trigger| {
            let TriggerKind::IntegrationEvent {
                integration,
                event,
                filters,
            } = &trigger.kind
            else {
                return None;
            };
            if integration != "vtube_studio" {
                return None;
            }
            let model_id = filters
                .get("model_id")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let kind = match event.as_str() {
                "model.loaded" | "model.unloaded" => VtubeRouteKind::Model {
                    loaded: event == "model.loaded",
                    model_id,
                },
                "hotkey.triggered" => VtubeRouteKind::Hotkey {
                    model_id,
                    hotkey_id: filters
                        .get("hotkey_id")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    ignore_api: filters
                        .get("ignore_api")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                },
                _ => return None,
            };
            Some(VtubeRoute {
                workflow_id: definition.workflow.id.clone(),
                trigger_id: trigger.id.clone(),
                kind,
            })
        })
        .collect()
}

pub fn required_kinds(routes: &[VtubeRoute]) -> BTreeSet<VtubeEventKind> {
    routes.iter().map(|route| route.kind.event_kind()).collect()
}

pub fn activations(routes: &[VtubeRoute], event: &Event) -> Vec<VtubeActivation> {
    routes
        .iter()
        .filter_map(|route| {
            let values = match (&route.kind, event) {
                (VtubeRouteKind::Model { loaded, model_id }, Event::ModelLoaded(data))
                    if *loaded == data.model_loaded
                        && model_id
                            .as_ref()
                            .is_none_or(|id| id.eq_ignore_ascii_case(&data.model_id)) =>
                {
                    ModelValues {
                        model_id: data.model_id.clone(),
                        model_name: data.model_name.clone(),
                        model_loaded: data.model_loaded,
                    }
                    .values()
                }
                (
                    VtubeRouteKind::Hotkey {
                        model_id,
                        hotkey_id,
                        ignore_api,
                    },
                    Event::HotkeyTriggered(data),
                ) if model_id
                    .as_ref()
                    .is_none_or(|id| id.eq_ignore_ascii_case(&data.model_id))
                    && hotkey_id
                        .as_ref()
                        .is_none_or(|id| id.eq_ignore_ascii_case(&data.hotkey_id))
                    && (!ignore_api || !data.hotkey_triggered_by_api) =>
                {
                    HotkeyValues {
                        model_id: data.model_id.clone(),
                        model_name: data.model_name.clone(),
                        hotkey_id: data.hotkey_id.clone(),
                        hotkey_name: data.hotkey_name.clone(),
                        hotkey_action: data.hotkey_action.as_str().to_owned(),
                        triggered_by_api: data.hotkey_triggered_by_api,
                        is_live2d_item: data.is_live2d_item,
                    }
                    .values()
                }
                _ => return None,
            };
            Some(VtubeActivation {
                workflow_id: route.workflow_id.clone(),
                trigger_id: route.trigger_id.clone(),
                values,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use serde_json::json;

    use super::*;
    use crate::engine::Workflow;

    fn definition(triggers: Vec<TriggerDefinition>) -> WorkflowDefinition {
        WorkflowDefinition {
            enabled: true,
            workflow: Workflow {
                id: "vts-flow".into(),
                revision: 1,
                overlap: false,
                steps: vec![],
                outputs: BTreeMap::new(),
            },
            triggers,
            name: None,
        }
    }

    fn trigger(id: &str, event: &str, filters: BTreeMap<String, Value>) -> TriggerDefinition {
        TriggerDefinition {
            id: id.into(),
            enabled: true,
            kind: TriggerKind::IntegrationEvent {
                integration: "vtube_studio".into(),
                event: event.into(),
                filters,
            },
        }
    }

    #[test]
    fn validates_ids_and_aggregates_event_demand() {
        let model = "a".repeat(32);
        let good = trigger(
            "model",
            "model.loaded",
            BTreeMap::from([("model_id".into(), json!(model))]),
        );
        assert!(validate_trigger(&good).is_ok());
        assert!(
            validate_trigger(&trigger(
                "bad",
                "model.loaded",
                BTreeMap::from([("model_id".into(), json!("short"))])
            ))
            .is_err()
        );
        assert!(
            validate_trigger(&trigger(
                "bad",
                "hotkey.triggered",
                BTreeMap::from([("ignore_api".into(), json!("true"))])
            ))
            .is_err()
        );
        let configured = definition(vec![
            good,
            trigger("other", "model.unloaded", BTreeMap::new()),
            trigger("hotkey", "hotkey.triggered", BTreeMap::new()),
        ]);
        let routes = routes_for(&configured);
        assert_eq!(
            required_kinds(&routes),
            BTreeSet::from([VtubeEventKind::ModelLoaded, VtubeEventKind::HotkeyTriggered])
        );
        let mut disabled = configured;
        disabled.enabled = false;
        assert!(routes_for(&disabled).is_empty());
    }

    #[test]
    fn model_events_route_by_loaded_state_and_stable_id() {
        let id = "a".repeat(32);
        let routes = routes_for(&definition(vec![
            trigger(
                "load",
                "model.loaded",
                BTreeMap::from([("model_id".into(), json!(id.clone()))]),
            ),
            trigger("unload", "model.unloaded", BTreeMap::new()),
        ]));
        let loaded = Event::ModelLoaded(vtubestudio::data::ModelLoadedEvent {
            model_loaded: true,
            model_name: "Avatar".into(),
            model_id: id.to_uppercase(),
        });
        assert_eq!(activations(&routes, &loaded).len(), 1);
        assert_eq!(activations(&routes, &loaded)[0].trigger_id, "load");
        let outputs =
            trigger_value_schema(&trigger("load", "model.loaded", BTreeMap::new()).kind).unwrap();
        assert_eq!(
            outputs
                .iter()
                .map(|output| output.id)
                .collect::<BTreeSet<_>>(),
            activations(&routes, &loaded)[0]
                .values
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
        );
        let unloaded = Event::ModelLoaded(vtubestudio::data::ModelLoadedEvent {
            model_loaded: false,
            model_name: "Avatar".into(),
            model_id: "b".repeat(32),
        });
        assert_eq!(activations(&routes, &unloaded)[0].trigger_id, "unload");
    }

    #[test]
    fn hotkey_schema_matches_routed_values_without_private_event_fields() {
        let kind = trigger("hotkey", "hotkey.triggered", BTreeMap::new()).kind;
        let routes = routes_for(&definition(vec![TriggerDefinition {
            id: "hotkey".into(),
            enabled: true,
            kind: kind.clone(),
        }]));
        let event = Event::HotkeyTriggered(vtubestudio::data::HotkeyTriggeredEvent {
            hotkey_id: "hotkey-id".into(),
            hotkey_name: "Wave".into(),
            hotkey_action: vtubestudio::data::EnumString::new_from_str("ToggleExpression"),
            hotkey_file: "private.exp3.json".into(),
            hotkey_triggered_by_api: false,
            model_id: "model-id".into(),
            model_name: "Avatar".into(),
            is_live2d_item: false,
        });
        let values = &activations(&routes, &event)[0].values;
        let outputs = trigger_value_schema(&kind).unwrap();
        assert_eq!(
            outputs
                .iter()
                .map(|output| output.id)
                .collect::<BTreeSet<_>>(),
            values.keys().map(String::as_str).collect::<BTreeSet<_>>()
        );
        assert!(!values.contains_key("hotkey_file"));
        assert!(outputs.iter().all(|output| output.id != "hotkey_file"));
    }
}
