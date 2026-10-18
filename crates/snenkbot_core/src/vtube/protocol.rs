//! Typed access to one VTube Studio API session.

use vtubestudio::data::{
    ApiStateRequest, AuthenticationRequest, AuthenticationTokenRequest, AvailableModelsRequest,
    CurrentModelRequest, Event, EventSubscriptionRequest, HotkeyTriggerRequest,
    HotkeyTriggeredEvent, HotkeyTriggeredEventConfig, HotkeysInCurrentModelRequest,
    ModelLoadRequest, ModelLoadedEvent, ModelLoadedEventConfig,
};
use vtubestudio::{Client, ClientEvent, ClientEventStream};

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error(transparent)]
    Api(#[from] vtubestudio::Error),
    #[error(transparent)]
    EventConfig(#[from] serde_json::Error),
    #[error("VTube Studio rejected plugin authentication: {0}")]
    AuthenticationRejected(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Model {
    pub id: String,
    pub name: String,
    pub loaded: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub id: String,
    pub name: String,
    pub action: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelHotkeys {
    pub model_id: String,
    pub model_name: String,
    pub hotkeys: Vec<Hotkey>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostics {
    pub api_active: bool,
    pub authenticated: bool,
    pub version: String,
}

#[derive(Debug)]
pub enum ProtocolEvent {
    Connected,
    Disconnected,
    Api(Event),
    Error(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    ModelLoaded,
    HotkeyTriggered,
}

pub struct Protocol {
    client: Client,
    plugin_name: String,
    plugin_developer: String,
}

pub struct ProtocolEvents {
    stream: ClientEventStream,
}

impl Protocol {
    /// Constructs a lazy client. Calling this does not connect or request authorization.
    pub fn new(url: &str, plugin_name: &str, plugin_developer: &str) -> (Self, ProtocolEvents) {
        // Neither authentication middleware nor disconnect retries may replay a model/hotkey action.
        let (client, stream) = Client::builder()
            .url(url)
            .retry_on_disconnect(false)
            .build_tungstenite();
        (
            Self {
                client,
                plugin_name: plugin_name.to_owned(),
                plugin_developer: plugin_developer.to_owned(),
            },
            ProtocolEvents { stream },
        )
    }

    /// Authenticates this session with a token previously approved by the user.
    pub async fn authenticate(&mut self, token: &str) -> Result<(), ProtocolError> {
        let response = self
            .client
            .send(&AuthenticationRequest {
                plugin_name: self.plugin_name.clone().into(),
                plugin_developer: self.plugin_developer.clone().into(),
                authentication_token: token.to_owned(),
            })
            .await?;
        if !response.authenticated {
            return Err(ProtocolError::AuthenticationRejected(response.reason));
        }
        Ok(())
    }

    /// Requests a new token. This can display a permission prompt in VTube Studio.
    pub async fn request_token(&mut self) -> Result<String, ProtocolError> {
        let response = self
            .client
            .send(&AuthenticationTokenRequest {
                plugin_name: self.plugin_name.clone().into(),
                plugin_developer: self.plugin_developer.clone().into(),
                plugin_icon: None,
            })
            .await?;
        Ok(response.authentication_token)
    }

    pub async fn diagnostics(&mut self) -> Result<Diagnostics, ProtocolError> {
        let state = self.client.send(&ApiStateRequest {}).await?;
        Ok(Diagnostics {
            api_active: state.active,
            authenticated: state.current_session_authenticated,
            version: state.vtubestudio_version,
        })
    }

    pub async fn current_model(&mut self) -> Result<Option<Model>, ProtocolError> {
        let response = self.client.send(&CurrentModelRequest {}).await?;
        Ok(response.model_loaded.then_some(Model {
            id: response.model_id,
            name: response.model_name,
            loaded: true,
        }))
    }

    pub async fn available_models(&mut self) -> Result<Vec<Model>, ProtocolError> {
        let response = self.client.send(&AvailableModelsRequest {}).await?;
        Ok(response
            .available_models
            .into_iter()
            .map(|model| Model {
                id: model.model_id,
                name: model.model_name,
                loaded: model.model_loaded,
            })
            .collect())
    }

    /// Uses model IDs for lookup; `None` selects the currently loaded model.
    pub async fn hotkeys(&mut self, model_id: Option<&str>) -> Result<ModelHotkeys, ProtocolError> {
        let response = self
            .client
            .send(&HotkeysInCurrentModelRequest {
                model_id: model_id.map(str::to_owned),
                live2d_item_file_name: None,
            })
            .await?;
        Ok(ModelHotkeys {
            model_id: response.model_id,
            model_name: response.model_name,
            hotkeys: response
                .available_hotkeys
                .into_iter()
                .map(|hotkey| Hotkey {
                    id: hotkey.hotkey_id,
                    name: hotkey.name,
                    action: hotkey.type_.to_string(),
                })
                .collect(),
        })
    }

    pub async fn load_model(&mut self, model_id: &str) -> Result<String, ProtocolError> {
        Ok(self
            .client
            .send(&ModelLoadRequest {
                model_id: model_id.to_owned(),
            })
            .await?
            .model_id)
    }

    /// VTube Studio unloads the current model when `ModelLoadRequest.modelID` is empty.
    pub async fn unload_model(&mut self) -> Result<(), ProtocolError> {
        self.client
            .send(&ModelLoadRequest {
                model_id: String::new(),
            })
            .await?;
        Ok(())
    }

    /// Sends once. A disconnect may leave the outcome unknown; callers must not retry blindly.
    pub async fn trigger_hotkey(&mut self, hotkey_id: &str) -> Result<String, ProtocolError> {
        Ok(self
            .client
            .send(&HotkeyTriggerRequest {
                hotkey_id: hotkey_id.to_owned(),
                item_instance_id: None,
            })
            .await?
            .hotkey_id)
    }

    pub async fn subscribe(&mut self, kind: EventKind) -> Result<(), ProtocolError> {
        let request = match kind {
            EventKind::ModelLoaded => {
                EventSubscriptionRequest::subscribe(&ModelLoadedEventConfig::default())?
            }
            EventKind::HotkeyTriggered => {
                EventSubscriptionRequest::subscribe(&HotkeyTriggeredEventConfig::default())?
            }
        };
        self.client.send(&request).await?;
        Ok(())
    }

    pub async fn unsubscribe(&mut self, kind: EventKind) -> Result<(), ProtocolError> {
        let request = match kind {
            EventKind::ModelLoaded => EventSubscriptionRequest::unsubscribe::<ModelLoadedEvent>(),
            EventKind::HotkeyTriggered => {
                EventSubscriptionRequest::unsubscribe::<HotkeyTriggeredEvent>()
            }
        };
        self.client.send(&request).await?;
        Ok(())
    }
}

impl ProtocolEvents {
    pub async fn next(&mut self) -> Option<ProtocolEvent> {
        loop {
            let event = self.stream.next().await?;
            match event {
                ClientEvent::Connected => return Some(ProtocolEvent::Connected),
                ClientEvent::Disconnected => return Some(ProtocolEvent::Disconnected),
                ClientEvent::Error(error) => return Some(ProtocolEvent::Error(error.to_string())),
                ClientEvent::Api(event) => return Some(ProtocolEvent::Api(event)),
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use vtubestudio::data::{HotkeyTriggerRequest, ModelLoadRequest, RequestEnvelope};

    #[test]
    fn unload_uses_empty_model_id() {
        let request = RequestEnvelope::new(&ModelLoadRequest {
            model_id: String::new(),
        })
        .expect("model load request serializes");
        assert_eq!(
            request.data.deserialize::<serde_json::Value>().unwrap()["modelID"],
            ""
        );
    }

    #[test]
    fn hotkey_request_targets_current_model_by_id() {
        let request = RequestEnvelope::new(&HotkeyTriggerRequest {
            hotkey_id: "stable-id".to_owned(),
            item_instance_id: None,
        })
        .expect("hotkey request serializes");
        let data = request.data.deserialize::<serde_json::Value>().unwrap();
        assert_eq!(data["hotkeyID"], "stable-id");
        assert!(data.get("itemInstanceID").is_none());
    }
}
