//! Shared authenticated VTube Studio session for workflow actions.

use std::sync::{Arc, RwLock};

use tokio::sync::{Mutex, watch};

use crate::integration::StatusCell;

use super::VtubeHealth;
use super::actions::{VtubeApi, VtubeError, VtubeFuture};
use super::credentials::VtubeCredentialStore;
use super::protocol::{Model, ModelHotkeys, Protocol, ProtocolError};
use super::settings::VtubeSettings;

pub(super) struct VtubeConnection {
    pub(super) settings: RwLock<VtubeSettings>,
    pub(super) credentials: Arc<dyn VtubeCredentialStore>,
    pub(super) client: Mutex<Option<Arc<Mutex<Protocol>>>>,
    pub(super) health: StatusCell<VtubeHealth>,
    pub(super) changed: watch::Sender<u64>,
}

impl VtubeConnection {
    async fn connected(&self) -> Result<Arc<Mutex<Protocol>>, VtubeError> {
        self.ready().map_err(VtubeError::Unavailable)?;
        self.client
            .lock()
            .await
            .as_ref()
            .cloned()
            .ok_or_else(|| VtubeError::Unavailable("VTube Studio is not connected yet".into()))
    }

    pub(super) async fn clear_if_current(&self, client: &Arc<Mutex<Protocol>>) {
        let mut slot = self.client.lock().await;
        if slot
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, client))
        {
            slot.take();
            self.health
                .set(VtubeHealth::Disconnected)
                .expect("VTube Studio health lock poisoned");
            self.changed
                .send_modify(|revision| *revision = revision.wrapping_add(1));
        }
    }

    async fn finish<T>(
        &self,
        client: &Arc<Mutex<Protocol>>,
        result: Result<T, ProtocolError>,
        remote_effect: bool,
    ) -> Result<T, VtubeError> {
        match result {
            Ok(value) => Ok(value),
            Err(ProtocolError::Api(error)) if error.is_api_error() => {
                if error.is_unauthenticated_error() {
                    self.clear_if_current(client).await;
                    self.health
                        .set(VtubeHealth::NeedsAuthorization)
                        .expect("VTube Studio health lock poisoned");
                    Err(VtubeError::Unavailable(
                        "VTube Studio authorization is no longer valid".into(),
                    ))
                } else {
                    Err(VtubeError::Failed(format!(
                        "VTube Studio rejected the request: {error}"
                    )))
                }
            }
            Err(error) => {
                self.clear_if_current(client).await;
                if remote_effect {
                    Err(VtubeError::Uncertain(
                        "VTube Studio disconnected or returned an invalid response after the action was sent; it may have taken effect".into(),
                    ))
                } else {
                    Err(VtubeError::Unavailable(format!(
                        "VTube Studio request failed: {error}"
                    )))
                }
            }
        }
    }
}

impl VtubeApi for VtubeConnection {
    fn ready(&self) -> Result<(), String> {
        if let VtubeHealth::Error(message) = &*self
            .health
            .read()
            .expect("VTube Studio health lock poisoned")
        {
            return Err(message.clone());
        }
        if !self
            .settings
            .read()
            .expect("VTube Studio settings lock poisoned")
            .enabled
        {
            return Err("VTube Studio is disabled".into());
        }
        match &*self
            .health
            .read()
            .expect("VTube Studio health lock poisoned")
        {
            VtubeHealth::Connected => Ok(()),
            VtubeHealth::NeedsAuthorization => Err("VTube Studio needs authorization".into()),
            VtubeHealth::Error(message) | VtubeHealth::Retrying(message) => Err(message.clone()),
            _ => Err("VTube Studio is not connected yet".into()),
        }
    }

    fn available_models(&self) -> VtubeFuture<'_, Vec<Model>> {
        Box::pin(async move {
            let client = self.connected().await?;
            let result = client.lock().await.available_models().await;
            self.finish(&client, result, false).await
        })
    }

    fn current_model(&self) -> VtubeFuture<'_, Option<Model>> {
        Box::pin(async move {
            let client = self.connected().await?;
            let result = client.lock().await.current_model().await;
            self.finish(&client, result, false).await
        })
    }

    fn hotkeys<'a>(&'a self, model_id: &'a str) -> VtubeFuture<'a, ModelHotkeys> {
        Box::pin(async move {
            let client = self.connected().await?;
            let result = client.lock().await.hotkeys(Some(model_id)).await;
            self.finish(&client, result, false).await
        })
    }

    fn load_model<'a>(&'a self, model_id: &'a str) -> VtubeFuture<'a, String> {
        Box::pin(async move {
            let client = self.connected().await?;
            let result = client.lock().await.load_model(model_id).await;
            self.finish(&client, result, true).await
        })
    }

    fn unload_model(&self) -> VtubeFuture<'_, ()> {
        Box::pin(async move {
            let client = self.connected().await?;
            let result = client.lock().await.unload_model().await;
            self.finish(&client, result, true).await
        })
    }

    fn trigger_hotkey<'a>(&'a self, hotkey_id: &'a str) -> VtubeFuture<'a, String> {
        Box::pin(async move {
            let client = self.connected().await?;
            let result = client.lock().await.trigger_hotkey(hotkey_id).await;
            self.finish(&client, result, true).await
        })
    }
}
