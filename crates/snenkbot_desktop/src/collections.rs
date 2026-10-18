//! Schema-backed named records owned by a live integration settings session.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::State;

use snenk_bot::{
    choices::ConfigChoice,
    engine::Values,
    schema::{ActionDefinition, ConfigurationActionDescriptor, DescribeConfig},
};

use crate::{
    api_admin::CreateApiConsumer,
    commands::DesktopBackend,
    configuration::{ConfigurationError, ConfigurationErrorCode, ConnectionModule},
};

/// Identifies a collection declared by the open settings form.
#[derive(Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct CollectionIdentity {
    pub session_id: String,
    pub collection_id: String,
}

/// Creates a record using the collection's generated input schema.
#[derive(Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct CreateCollectionRecord {
    pub session_id: String,
    pub collection_id: String,
    #[specta(type = BTreeMap<String, specta_typescript::Unknown>)]
    pub values: Values,
}

/// Selects one record for removal without affecting its siblings.
#[derive(Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct CollectionRecordIdentity {
    pub session_id: String,
    pub collection_id: String,
    pub record_id: String,
}

/// Selects a compiled operation on one record in the open owning collection.
#[derive(Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct CollectionActionRequest {
    pub session_id: String,
    pub collection_id: String,
    pub record_id: String,
    pub action_id: String,
}

/// Public editable values for a named record, never its stored secret.
#[derive(Serialize, Type)]
pub struct CollectionRecord {
    pub id: String,
    pub title: String,
    #[specta(type = BTreeMap<String, specta_typescript::Unknown>)]
    pub values: Values,
}

/// A labelled group of choices for a schema-backed set map.
#[derive(Serialize, Type)]
pub struct ChoiceGroup {
    pub id: String,
    pub label: String,
    pub items: Vec<ConfigChoice>,
}

/// Native choices consumed directly by the shared field controls.
#[derive(Serialize, Type)]
pub struct CollectionChoices {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<ConfigChoice>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub groups: Option<Vec<ChoiceGroup>>,
}

/// Generated record form, public inventory and integration-provided choices.
#[derive(Serialize, Type)]
pub struct CollectionSnapshot {
    pub definition: ActionDefinition,
    pub actions: Vec<ConfigurationActionDescriptor>,
    pub records: Vec<CollectionRecord>,
    pub choices: BTreeMap<String, CollectionChoices>,
}

/// Reads a declared collection without returning credentials.
#[tauri::command]
pub async fn configuration_collection_load(
    state: State<'_, DesktopBackend>,
    request: CollectionIdentity,
) -> Result<CollectionSnapshot, ConfigurationError> {
    let module = state
        .collection_module(request.session_id, request.collection_id)
        .await?;
    match module {
        ConnectionModule::Api => {
            state
                .with_api(|api| {
                    let records = api
                        .list_consumers()?
                        .into_iter()
                        .map(|consumer| CollectionRecord {
                            id: consumer.id,
                            title: consumer.name.clone(),
                            values: Values::from([
                                (
                                    "static_folder".into(),
                                    serde_json::json!(consumer.static_folder),
                                ),
                                ("name".into(), serde_json::json!(consumer.name)),
                                (
                                    "permissions".into(),
                                    serde_json::json!(consumer.permissions),
                                ),
                                (
                                    "allowed_origins".into(),
                                    serde_json::json!(consumer.allowed_origins),
                                ),
                            ]),
                        })
                        .collect();
                    let groups = snenk_bot::providers::catalog()
                        .into_iter()
                        .map(|provider| ChoiceGroup {
                            id: provider.id.into(),
                            label: provider.title.into(),
                            items: provider
                                .operations
                                .into_iter()
                                .map(|operation| ConfigChoice {
                                    value: operation.id.into(),
                                    label: operation.title.into(),
                                    detail: None,
                                })
                                .collect(),
                        })
                        .collect();
                    Ok(CollectionSnapshot {
                        definition: ActionDefinition::of::<CreateApiConsumer>(),
                        actions: CreateApiConsumer::ACTIONS.to_vec(),
                        records,
                        choices: BTreeMap::from([
                            (
                                "api.channels".into(),
                                CollectionChoices {
                                    items: Some(snenk_bot::api::channel_choices()),
                                    groups: None,
                                },
                            ),
                            (
                                "api.provider_operations".into(),
                                CollectionChoices {
                                    items: None,
                                    groups: Some(groups),
                                },
                            ),
                        ]),
                    })
                })
                .await
        }
        _ => Err(unsupported_collection()),
    }
}

/// Creates a record and returns a newly issued secret once, when applicable.
#[tauri::command]
pub async fn configuration_collection_create(
    state: State<'_, DesktopBackend>,
    request: CreateCollectionRecord,
) -> Result<Option<String>, ConfigurationError> {
    let module = state
        .collection_module(request.session_id, request.collection_id)
        .await?;
    match module {
        ConnectionModule::Api => {
            let values: CreateApiConsumer =
                serde_json::from_value(serde_json::json!(request.values)).map_err(|_| {
                    ConfigurationError::new(
                        ConfigurationErrorCode::InvalidValues,
                        "The record contains invalid values.",
                    )
                })?;
            state
                .with_api(move |api| {
                    api.create_consumer_with_stored_token(
                        values.name,
                        values.permissions,
                        values.allowed_origins,
                        values.static_folder,
                    )
                    .map(|created| Some(created.token))
                })
                .await
        }
        _ => Err(unsupported_collection()),
    }
}

/// Executes only declared native record actions; copied credentials never enter public snapshots.
#[tauri::command]
pub async fn configuration_collection_action(
    state: State<'_, DesktopBackend>,
    request: CollectionActionRequest,
) -> Result<Option<String>, ConfigurationError> {
    let module = state
        .collection_module(request.session_id, request.collection_id)
        .await?;
    let action = record_action(module, &request.action_id)?;
    match action {
        "api.copy_hosting_url" => {
            let text = state
                .with_api(move |api| api.consumer_hosting_url(&request.record_id))
                .await?;
            tauri::async_runtime::spawn_blocking(move || {
                arboard::Clipboard::new().and_then(|mut clipboard| clipboard.set_text(text))
            })
            .await
            .map_err(|_| ConfigurationError::unavailable())?
            .map_err(|_| {
                ConfigurationError::new(
                    ConfigurationErrorCode::Unavailable,
                    "The hosting URL could not be copied. Try again.",
                )
            })?;
            Ok(None)
        }
        "api.regenerate_token" => {
            state
                .with_api(move |api| {
                    api.regenerate_consumer_token(&request.record_id)
                        .map(|created| Some(created.token))
                })
                .await
        }
        _ => Err(unsupported_collection()),
    }
}

/// Rejects operations absent from the owning record type before any credential access occurs.
fn record_action(module: ConnectionModule, id: &str) -> Result<&'static str, ConfigurationError> {
    let actions = match module {
        ConnectionModule::Api => CreateApiConsumer::ACTIONS,
        _ => &[],
    };
    actions
        .iter()
        .find(|action| action.id == id)
        .map(|action| action.id)
        .ok_or_else(|| {
            ConfigurationError::new(
                ConfigurationErrorCode::InvalidValues,
                "This action does not belong to the settings view.",
            )
        })
}

/// Removes only the requested record from its declared collection.
#[tauri::command]
pub async fn configuration_collection_remove(
    state: State<'_, DesktopBackend>,
    request: CollectionRecordIdentity,
) -> Result<(), ConfigurationError> {
    let module = state
        .collection_module(request.session_id, request.collection_id)
        .await?;
    match module {
        ConnectionModule::Api => {
            state
                .with_api(move |api| api.revoke_consumer(&request.record_id))
                .await
        }
        _ => Err(unsupported_collection()),
    }
}

/// Rejects a collection that has no compiled persistence implementation.
fn unsupported_collection() -> ConfigurationError {
    ConfigurationError::new(
        ConfigurationErrorCode::InvalidValues,
        "This settings view has no supported collection.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Native action identity is checked before touching the clipboard or credential manager.
    #[test]
    fn record_actions_belong_only_to_their_declared_owner() {
        assert_eq!(
            record_action(ConnectionModule::Api, "api.copy_hosting_url").unwrap(),
            "api.copy_hosting_url"
        );
        assert_eq!(
            record_action(ConnectionModule::Api, "api.regenerate_token").unwrap(),
            "api.regenerate_token"
        );
        assert!(record_action(ConnectionModule::Api, "arbitrary.action").is_err());
        assert!(record_action(ConnectionModule::Obs, "api.regenerate_token").is_err());
        assert!(
            CreateApiConsumer::ACTIONS
                .iter()
                .find(|action| action.id == "api.regenerate_token")
                .unwrap()
                .confirmation
                .is_some()
        );
    }
}
