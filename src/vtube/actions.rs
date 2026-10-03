//! Typed workflow actions for the VTube Studio integration.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::ConfigSchema;
use crate::engine::{
    Capability, CapabilityError, Engine, Input, Values, validate_configured_inputs,
};
use crate::schema::{ActionDefinition, DescribeConfig};

use super::protocol::{Hotkey, Model, ModelHotkeys};

pub type VtubeFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, VtubeError>> + Send + 'a>>;

#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum VtubeError {
    #[error("{0}")]
    Unavailable(String),
    #[error("{0}")]
    Failed(String),
    #[error("{0}")]
    Uncertain(String),
}

/// Adapter boundary implemented by the VTube Studio integration's shared protocol session.
pub trait VtubeApi: Send + Sync {
    fn ready(&self) -> Result<(), String>;
    fn available_models(&self) -> VtubeFuture<'_, Vec<Model>>;
    fn current_model(&self) -> VtubeFuture<'_, Option<Model>>;
    fn hotkeys<'a>(&'a self, model_id: &'a str) -> VtubeFuture<'a, ModelHotkeys>;
    fn load_model<'a>(&'a self, model_id: &'a str) -> VtubeFuture<'a, String>;
    fn unload_model(&self) -> VtubeFuture<'_, ()>;
    fn trigger_hotkey<'a>(&'a self, hotkey_id: &'a str) -> VtubeFuture<'a, String>;
}

#[derive(Default, Deserialize, Serialize, ConfigSchema)]
#[serde(deny_unknown_fields)]
#[config(
    id = "vtube.load_model",
    version = 1,
    title = "Load VTube Studio model"
)]
struct LoadModelInputs {
    #[config(
        id = "model_id",
        label = "Model",
        introduced = 1,
        choice = "vtube.models"
    )]
    model_id: String,
}

#[derive(Default, Deserialize, Serialize, ConfigSchema)]
#[serde(deny_unknown_fields)]
#[config(
    id = "vtube.unload_model",
    version = 1,
    title = "Unload VTube Studio model"
)]
struct UnloadModelInputs {}

#[derive(Default, Deserialize, Serialize, ConfigSchema)]
#[serde(deny_unknown_fields)]
#[config(
    id = "vtube.trigger_hotkey",
    version = 1,
    title = "Trigger VTube Studio hotkey"
)]
struct TriggerHotkeyInputs {
    #[config(
        id = "model_id",
        label = "Model",
        introduced = 1,
        choice = "vtube.models"
    )]
    model_id: String,
    #[config(
        id = "hotkey_id",
        label = "Hotkey",
        introduced = 1,
        choice = "vtube.hotkeys",
        depends_on = "model_id"
    )]
    hotkey_id: String,
}

#[derive(Default, Deserialize, Serialize, ConfigSchema)]
#[serde(deny_unknown_fields)]
#[config(
    id = "vtube.get_current_model",
    version = 1,
    title = "Get current VTube Studio model",
    output("model_loaded", "Model loaded", "toggle"),
    output("model_id", "Model ID", "text", "optional"),
    output("model_name", "Model name", "text", "optional")
)]
struct CurrentModelInputs {}

#[derive(Clone, Copy)]
enum Action {
    LoadModel,
    UnloadModel,
    TriggerHotkey,
    CurrentModel,
}

pub fn register(engine: &mut Engine, api: Arc<dyn VtubeApi>) {
    for (definition, action) in [
        (ActionDefinition::of::<LoadModelInputs>(), Action::LoadModel),
        (
            ActionDefinition::of::<UnloadModelInputs>(),
            Action::UnloadModel,
        ),
        (
            ActionDefinition::of::<TriggerHotkeyInputs>(),
            Action::TriggerHotkey,
        ),
        (
            ActionDefinition::of::<CurrentModelInputs>(),
            Action::CurrentModel,
        ),
    ] {
        engine.register_lua_action_definition(
            definition,
            Arc::new(VtubeAction {
                kind: action,
                api: Arc::clone(&api),
            }),
        );
    }
}

struct VtubeAction {
    kind: Action,
    api: Arc<dyn VtubeApi>,
}

impl Capability for VtubeAction {
    fn validate_inputs(&self, inputs: &BTreeMap<String, Input>) -> Result<(), String> {
        let (schema, ids): (_, &[(&str, &str)]) = match self.kind {
            Action::LoadModel => (LoadModelInputs::SCHEMA, &[("model_id", "model")]),
            Action::UnloadModel => (UnloadModelInputs::SCHEMA, &[]),
            Action::TriggerHotkey => (
                TriggerHotkeyInputs::SCHEMA,
                &[("model_id", "model"), ("hotkey_id", "hotkey")],
            ),
            Action::CurrentModel => (CurrentModelInputs::SCHEMA, &[]),
        };
        validate_configured_inputs(schema, inputs)?;
        for (field, kind) in ids {
            if let Some(Input::Literal(Value::String(id))) = inputs.get(*field) {
                validate_id(id, kind)?;
            }
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
            self.api
                .ready()
                .map_err(CapabilityError::ConnectorUnavailable)?;
            match self.kind {
                Action::LoadModel => {
                    let config: LoadModelInputs = parse_inputs(inputs)?;
                    validate_id(&config.model_id, "model").map_err(CapabilityError::Failed)?;
                    let models = self.api.available_models().await.map_err(map_error)?;
                    ensure_available_model(&models, &config.model_id)?;
                    ensure_active(&cancel)?;
                    let loaded = self
                        .api
                        .load_model(&config.model_id)
                        .await
                        .map_err(map_error)?;
                    if !same_id(&loaded, &config.model_id) {
                        return Err(CapabilityError::Uncertain(format!(
                            "VTube Studio reported model `{loaded}` after loading `{}`",
                            config.model_id
                        )));
                    }
                    Ok(Values::new())
                }
                Action::UnloadModel => {
                    ensure_no_inputs(&inputs)?;
                    if self.api.current_model().await.map_err(map_error)?.is_some() {
                        ensure_active(&cancel)?;
                        self.api.unload_model().await.map_err(map_error)?;
                    }
                    Ok(Values::new())
                }
                Action::TriggerHotkey => {
                    let config: TriggerHotkeyInputs = parse_inputs(inputs)?;
                    validate_id(&config.model_id, "model").map_err(CapabilityError::Failed)?;
                    validate_id(&config.hotkey_id, "hotkey").map_err(CapabilityError::Failed)?;
                    let current = self.api.current_model().await.map_err(map_error)?;
                    let Some(current) = current else {
                        return Err(CapabilityError::Failed(
                            "VTube Studio has no model loaded".into(),
                        ));
                    };
                    if !same_id(&current.id, &config.model_id) {
                        return Err(CapabilityError::Failed(format!(
                            "VTube Studio model changed; expected `{}`, found `{}`",
                            config.model_id, current.id
                        )));
                    }
                    let hotkeys = self
                        .api
                        .hotkeys(&config.model_id)
                        .await
                        .map_err(map_error)?;
                    if !same_id(&hotkeys.model_id, &config.model_id) {
                        return Err(CapabilityError::Failed(
                            "VTube Studio returned hotkeys for a different model".into(),
                        ));
                    }
                    ensure_hotkey(&hotkeys.hotkeys, &config.hotkey_id)?;
                    ensure_active(&cancel)?;
                    let triggered = self
                        .api
                        .trigger_hotkey(&config.hotkey_id)
                        .await
                        .map_err(map_error)?;
                    if !same_id(&triggered, &config.hotkey_id) {
                        return Err(CapabilityError::Uncertain(format!(
                            "VTube Studio reported hotkey `{triggered}` after triggering `{}`",
                            config.hotkey_id
                        )));
                    }
                    Ok(Values::new())
                }
                Action::CurrentModel => {
                    ensure_no_inputs(&inputs)?;
                    let model = self.api.current_model().await.map_err(map_error)?;
                    Ok(current_model_values(model))
                }
            }
        })
    }
}

fn parse_inputs<T: DeserializeOwned>(inputs: Values) -> Result<T, CapabilityError> {
    serde_json::from_value(Value::Object(inputs.into_iter().collect())).map_err(|error| {
        CapabilityError::Failed(format!("invalid VTube Studio action inputs: {error}"))
    })
}

fn ensure_no_inputs(inputs: &Values) -> Result<(), CapabilityError> {
    if inputs.is_empty() {
        Ok(())
    } else {
        Err(CapabilityError::Failed(
            "VTube Studio action takes no inputs".into(),
        ))
    }
}

fn ensure_active(cancel: &CancellationToken) -> Result<(), CapabilityError> {
    if cancel.is_cancelled() {
        Err(CapabilityError::Failed("action cancelled".into()))
    } else {
        Ok(())
    }
}

fn validate_id(id: &str, kind: &str) -> Result<(), String> {
    if id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(format!(
            "VTube Studio {kind} ID must contain exactly 32 hexadecimal characters"
        ))
    }
}

fn same_id(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

fn ensure_available_model(models: &[Model], requested: &str) -> Result<(), CapabilityError> {
    match models
        .iter()
        .filter(|model| same_id(&model.id, requested))
        .count()
    {
        1 => Ok(()),
        0 => Err(CapabilityError::Failed(format!(
            "VTube Studio model `{requested}` is no longer available"
        ))),
        _ => Err(CapabilityError::Failed(format!(
            "VTube Studio model ID `{requested}` is ambiguous"
        ))),
    }
}

fn ensure_hotkey(hotkeys: &[Hotkey], requested: &str) -> Result<(), CapabilityError> {
    match hotkeys
        .iter()
        .filter(|hotkey| same_id(&hotkey.id, requested))
        .count()
    {
        1 => Ok(()),
        0 => Err(CapabilityError::Failed(format!(
            "VTube Studio hotkey `{requested}` is no longer available for the expected model"
        ))),
        _ => Err(CapabilityError::Failed(format!(
            "VTube Studio hotkey ID `{requested}` is ambiguous for the expected model"
        ))),
    }
}

fn current_model_values(model: Option<Model>) -> Values {
    match model {
        Some(model) => Values::from([
            ("model_loaded".into(), json!(true)),
            ("model_id".into(), json!(model.id)),
            ("model_name".into(), json!(model.name)),
        ]),
        None => Values::from([("model_loaded".into(), json!(false))]),
    }
}

fn map_error(error: VtubeError) -> CapabilityError {
    match error {
        VtubeError::Unavailable(message) => CapabilityError::ConnectorUnavailable(message),
        VtubeError::Failed(message) => CapabilityError::Failed(message),
        VtubeError::Uncertain(message) => CapabilityError::Uncertain(message),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::schema::ConfigChoiceSource;

    #[test]
    fn model_outputs_and_dependent_hotkey_choice_are_declared() {
        let outputs = CurrentModelInputs::SCHEMA.outputs;
        assert_eq!(
            outputs.iter().map(|output| output.id).collect::<Vec<_>>(),
            ["model_loaded", "model_id", "model_name"]
        );
        assert!(outputs[0].required);
        assert!(!outputs[1].required && !outputs[2].required);
        assert_eq!(
            LoadModelInputs::SCHEMA.fields[0].choice_source,
            Some(ConfigChoiceSource {
                key: "vtube.models",
                depends_on: None
            })
        );
        assert_eq!(
            TriggerHotkeyInputs::SCHEMA.fields[1].choice_source,
            Some(ConfigChoiceSource {
                key: "vtube.hotkeys",
                depends_on: Some("model_id")
            })
        );
    }

    #[derive(Default)]
    struct FakeVtube {
        calls: Mutex<Vec<String>>,
        loaded: Mutex<Option<Model>>,
        failure: Mutex<Option<VtubeError>>,
    }

    impl VtubeApi for FakeVtube {
        fn ready(&self) -> Result<(), String> {
            Ok(())
        }
        fn available_models(&self) -> VtubeFuture<'_, Vec<Model>> {
            Box::pin(async { Ok(vec![model("Model")]) })
        }
        fn current_model(&self) -> VtubeFuture<'_, Option<Model>> {
            Box::pin(async move { Ok(self.loaded.lock().unwrap().clone()) })
        }
        fn hotkeys<'a>(&'a self, model_id: &'a str) -> VtubeFuture<'a, ModelHotkeys> {
            Box::pin(async move {
                Ok(ModelHotkeys {
                    model_id: model_id.into(),
                    model_name: "Test".into(),
                    hotkeys: vec![hotkey()],
                })
            })
        }
        fn load_model<'a>(&'a self, model_id: &'a str) -> VtubeFuture<'a, String> {
            Box::pin(async move {
                self.calls.lock().unwrap().push("load".into());
                if let Some(error) = self.failure.lock().unwrap().take() {
                    return Err(error);
                }
                Ok(model_id.into())
            })
        }
        fn unload_model(&self) -> VtubeFuture<'_, ()> {
            Box::pin(async move {
                self.calls.lock().unwrap().push("unload".into());
                Ok(())
            })
        }
        fn trigger_hotkey<'a>(&'a self, hotkey_id: &'a str) -> VtubeFuture<'a, String> {
            Box::pin(async move {
                self.calls.lock().unwrap().push("trigger".into());
                if let Some(error) = self.failure.lock().unwrap().take() {
                    return Err(error);
                }
                Ok(hotkey_id.into())
            })
        }
    }

    fn model(name: &str) -> Model {
        Model {
            id: "0123456789abcdef0123456789abcdef".into(),
            name: name.into(),
            loaded: false,
        }
    }

    fn hotkey() -> Hotkey {
        Hotkey {
            id: "abcdef0123456789abcdef0123456789".into(),
            name: "Wave".into(),
            action: "TriggerAnimation".into(),
        }
    }

    #[tokio::test]
    async fn load_uses_selected_model_id_and_current_model_exposes_outputs() {
        let api = Arc::new(FakeVtube::default());
        let load = VtubeAction {
            kind: Action::LoadModel,
            api: api.clone(),
        };
        load.execute(
            Values::from([("model_id".into(), json!(model("Test").id))]),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(*api.calls.lock().unwrap(), ["load"]);
        *api.loaded.lock().unwrap() = Some(model("Test"));
        let status = VtubeAction {
            kind: Action::CurrentModel,
            api,
        };
        let result = status
            .execute(Values::new(), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result["model_loaded"], true);
        assert_eq!(result["model_id"], "0123456789abcdef0123456789abcdef");
    }

    #[tokio::test]
    async fn hotkey_rejects_stale_model_before_trigger_request() {
        let api = Arc::new(FakeVtube::default());
        *api.loaded.lock().unwrap() = Some(Model {
            id: "ffffffffffffffffffffffffffffffff".into(),
            name: "Other".into(),
            loaded: true,
        });
        let action = VtubeAction {
            kind: Action::TriggerHotkey,
            api: api.clone(),
        };
        let result = action
            .execute(
                Values::from([
                    ("model_id".into(), json!(model("Expected").id)),
                    ("hotkey_id".into(), json!(hotkey().id)),
                ]),
                CancellationToken::new(),
            )
            .await;
        assert!(matches!(result, Err(CapabilityError::Failed(_))));
        assert!(api.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn uncertain_hotkey_outcome_is_preserved_without_retry() {
        let api = Arc::new(FakeVtube::default());
        *api.loaded.lock().unwrap() = Some(model("Test"));
        *api.failure.lock().unwrap() =
            Some(VtubeError::Uncertain("connection lost after send".into()));
        let action = VtubeAction {
            kind: Action::TriggerHotkey,
            api: api.clone(),
        };
        let result = action
            .execute(
                Values::from([
                    ("model_id".into(), json!(model("Expected").id)),
                    ("hotkey_id".into(), json!(hotkey().id)),
                ]),
                CancellationToken::new(),
            )
            .await;
        assert!(matches!(result, Err(CapabilityError::Uncertain(_))));
        assert_eq!(*api.calls.lock().unwrap(), ["trigger"]);
    }

    #[test]
    fn schemas_reject_non_hex_or_wrong_length_ids() {
        let api = Arc::new(FakeVtube::default());
        let action = VtubeAction {
            kind: Action::TriggerHotkey,
            api,
        };
        let inputs = BTreeMap::from([
            ("model_id".into(), Input::Literal(json!("short"))),
            ("hotkey_id".into(), Input::Literal(json!(hotkey().id))),
        ]);
        assert!(
            action
                .validate_inputs(&inputs)
                .unwrap_err()
                .contains("32 hexadecimal")
        );
    }
}
