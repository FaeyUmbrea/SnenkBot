//! Shared OBS connection, request handling, and health state.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use obws::client::{ConnectConfig, DEFAULT_BROADCAST_CAPACITY};
use obws::requests::EventSubscription;
use serde_json::json;
use tokio::sync::{Mutex, watch};

use crate::engine::Values;
use crate::integration::StatusCell;

use super::ObsHealth;
use super::actions::{
    INPUT_TOKEN_PREFIX, ITEM_TOKEN_PREFIX, SCENE_TOKEN_PREFIX, parse_scene_item_token,
    parse_uuid_token,
};
use super::actions::{
    ObsApi, ObsError, ObsFuture, ObsRecordingStatus, ObsResource, ObsResourceSnapshot,
    ObsSceneItemResource, ObsStreamingStatus, SceneItem,
};
use super::credentials::ObsCredentialStore;
use super::settings::ObsSettings;

pub(super) struct ObsConnection {
    pub(super) settings: RwLock<ObsSettings>,
    pub(super) credentials: Arc<dyn ObsCredentialStore>,
    pub(super) client: Mutex<Option<Arc<obws::Client>>>,
    pub(super) health: StatusCell<ObsHealth>,
    pub(super) changed: watch::Sender<u64>,
}

impl ObsConnection {
    async fn finish_request<T>(
        &self,
        result: Result<T, obws::error::Error>,
        remote_effect: bool,
    ) -> Result<T, ObsError> {
        match result {
            Ok(value) => Ok(value),
            Err(error) => Err(self.call_failed(error, remote_effect).await),
        }
    }

    pub(super) async fn connected(&self) -> Result<Arc<obws::Client>, ObsError> {
        let settings = self
            .settings
            .read()
            .expect("OBS settings lock poisoned")
            .clone();
        if !settings.enabled {
            return Err(ObsError::Unavailable("OBS is disabled".into()));
        }
        let mut slot = self.client.lock().await;
        if let Some(client) = slot.as_ref() {
            return Ok(Arc::clone(client));
        }
        let credentials = Arc::clone(&self.credentials);
        self.health
            .set(ObsHealth::Connecting)
            .expect("OBS health lock poisoned");
        let password = tokio::task::spawn_blocking(move || credentials.load())
            .await
            .map_err(|_| ObsError::Unavailable("OBS credential worker stopped".into()))?
            .map_err(|error| ObsError::Unavailable(error.to_string()))?;
        let config = ConnectConfig {
            host: settings.host.as_str(),
            port: settings.port,
            password: password.as_deref(),
            event_subscriptions: Some(EventSubscription::NONE),
            tls: settings.tls,
            broadcast_capacity: DEFAULT_BROADCAST_CAPACITY,
            connect_timeout: Duration::from_secs(10),
            dangerous: None,
        };
        let client = obws::Client::connect_with_config(config)
            .await
            .map_err(|error| {
                let message = format!("OBS connection failed: {error}");
                self.health
                    .set(ObsHealth::Error(message.clone()))
                    .expect("OBS health lock poisoned");
                ObsError::Unavailable(message)
            })?;
        let client = Arc::new(client);
        *slot = Some(Arc::clone(&client));
        self.health
            .set(ObsHealth::Connected)
            .expect("OBS health lock poisoned");
        Ok(client)
    }

    pub(super) async fn probe(&self) -> Result<(), ObsError> {
        let client = self.connected().await?;
        match client.general().version().await {
            Ok(_) => {
                self.health
                    .set(ObsHealth::Connected)
                    .expect("OBS health lock poisoned");
                Ok(())
            }
            Err(error) => Err(self.call_failed(error, false).await),
        }
    }

    pub(super) async fn call_failed(
        &self,
        error: obws::error::Error,
        remote_effect: bool,
    ) -> ObsError {
        if matches!(
            error,
            obws::error::Error::Disconnected
                | obws::error::Error::Send(_)
                | obws::error::Error::ReceiveMessage(_)
        ) {
            self.client.lock().await.take();
            self.health
                .set(ObsHealth::Disconnected)
                .expect("OBS health lock poisoned");
        }
        match error {
            obws::error::Error::Api { code, message } => ObsError::Failed(match message {
                Some(message) => format!("OBS rejected the request ({code:?}): {message}"),
                None => format!("OBS rejected the request ({code:?})"),
            }),
            _ if remote_effect => ObsError::Uncertain(
                "OBS disconnected or returned an invalid response after the action was sent; it may have taken effect".into(),
            ),
            _ => ObsError::Unavailable("OBS connection was lost".into()),
        }
    }
}

impl ObsApi for ObsConnection {
    fn ready(&self) -> Result<(), String> {
        if !self
            .settings
            .read()
            .expect("OBS settings lock poisoned")
            .enabled
        {
            return Err("OBS is disabled".into());
        }
        match self
            .health
            .read()
            .expect("OBS health lock poisoned")
            .clone()
        {
            ObsHealth::Error(message) | ObsHealth::Retrying(message) => Err(message),
            ObsHealth::Connected if self.client.try_lock().is_ok_and(|client| client.is_some()) => {
                Ok(())
            }
            _ => Err("OBS is not connected yet".into()),
        }
    }

    fn scenes(&self) -> ObsFuture<'_, Vec<String>> {
        Box::pin(async move {
            let client = self.connected().await?;
            match client.scenes().list().await {
                Ok(list) => Ok(list.scenes.into_iter().map(|scene| scene.id.name).collect()),
                Err(error) => Err(self.call_failed(error, false).await),
            }
        })
    }

    fn inputs(&self) -> ObsFuture<'_, Vec<String>> {
        Box::pin(async move {
            let client = self.connected().await?;
            match client.inputs().list(None).await {
                Ok(inputs) => Ok(inputs.into_iter().map(|input| input.id.name).collect()),
                Err(error) => Err(self.call_failed(error, false).await),
            }
        })
    }

    fn items<'a>(&'a self, scene: &'a str) -> ObsFuture<'a, Vec<SceneItem>> {
        Box::pin(async move {
            let client = self.connected().await?;
            match client.scene_items().list(scene.into()).await {
                Ok(items) => Ok(items
                    .into_iter()
                    .map(|item| SceneItem {
                        id: item.id,
                        name: item.source_name,
                    })
                    .collect()),
                Err(error) => Err(self.call_failed(error, false).await),
            }
        })
    }

    fn set_scene<'a>(&'a self, scene: &'a str) -> ObsFuture<'a, ()> {
        Box::pin(async move {
            let client = self.connected().await?;
            let scene_id = parse_uuid_token(scene, SCENE_TOKEN_PREFIX)
                .map(obws::requests::scenes::SceneId::Uuid)
                .unwrap_or_else(|| obws::requests::scenes::SceneId::Name(scene));
            match client.scenes().set_current_program_scene(scene_id).await {
                Ok(()) => Ok(()),
                Err(error) => Err(self.call_failed(error, true).await),
            }
        })
    }

    fn set_preview_scene<'a>(&'a self, scene: &'a str) -> ObsFuture<'a, ()> {
        Box::pin(async move {
            let client = self.connected().await?;
            let scene_id = parse_uuid_token(scene, SCENE_TOKEN_PREFIX)
                .map(obws::requests::scenes::SceneId::Uuid)
                .unwrap_or_else(|| obws::requests::scenes::SceneId::Name(scene));
            match client.scenes().set_current_preview_scene(scene_id).await {
                Ok(()) => Ok(()),
                Err(error) => Err(self.call_failed(error, true).await),
            }
        })
    }

    fn set_item_enabled<'a>(&'a self, scene: &'a str, id: i64, enabled: bool) -> ObsFuture<'a, ()> {
        Box::pin(async move {
            let client = self.connected().await?;
            if scene.starts_with(ITEM_TOKEN_PREFIX) {
                let item = parse_scene_item_token(scene)
                    .ok_or_else(|| ObsError::Failed("invalid OBS scene item token".into()))?;
                let snapshot = self.resource_snapshot().await?;
                let matches: Vec<_> = snapshot
                    .scene_items
                    .iter()
                    .filter(|entry| {
                        entry.scene.uuid == item.scene_uuid
                            && entry.group_path == item.group_path
                            && entry.item_id == item.item_id
                            && entry.source_name == item.source_name
                    })
                    .collect();
                match matches.as_slice() {
                    [_] => (),
                    [] => {
                        return Err(ObsError::Failed(
                            "OBS scene item token is stale or no longer available".into(),
                        ));
                    }
                    _ => return Err(ObsError::Failed("OBS scene item token is ambiguous".into())),
                }
                if let Some(group) = item.group_path.last() {
                    let matching_groups: std::collections::BTreeSet<_> = snapshot
                        .scene_items
                        .iter()
                        .filter(|entry| entry.group_path.last().is_some_and(|name| name == group))
                        .map(|entry| (entry.scene.uuid, entry.group_path.clone()))
                        .collect();
                    let matching_names: std::collections::BTreeSet<_> = snapshot
                        .scene_items
                        .iter()
                        .filter(|entry| entry.source_name == *group)
                        .map(|entry| (entry.scene.uuid, entry.group_path.clone(), entry.item_id))
                        .collect();
                    if matching_groups.len() != 1
                        || matching_names.len() != 1
                        || snapshot
                            .scenes
                            .iter()
                            .any(|scene| scene.name == group.as_str())
                    {
                        return Err(ObsError::Failed("OBS scene item group is ambiguous".into()));
                    }
                    let result = client
                        .scene_items()
                        .set_enabled(obws::requests::scene_items::SetEnabled {
                            scene: obws::requests::scenes::SceneId::Name(group),
                            item_id: item.item_id,
                            enabled,
                        })
                        .await;
                    return match result {
                        Ok(()) => Ok(()),
                        Err(error) => Err(self.call_failed(error, true).await),
                    };
                } else {
                    let result = client
                        .scene_items()
                        .set_enabled(obws::requests::scene_items::SetEnabled {
                            scene: obws::requests::scenes::SceneId::Uuid(item.scene_uuid),
                            item_id: item.item_id,
                            enabled,
                        })
                        .await;
                    return match result {
                        Ok(()) => Ok(()),
                        Err(error) => Err(self.call_failed(error, true).await),
                    };
                }
            }
            match client
                .scene_items()
                .set_enabled(obws::requests::scene_items::SetEnabled {
                    scene: obws::requests::scenes::SceneId::Name(scene),
                    item_id: id,
                    enabled,
                })
                .await
            {
                Ok(()) => Ok(()),
                Err(error) => Err(self.call_failed(error, true).await),
            }
        })
    }

    fn create_chapter<'a>(&'a self, name: &'a str) -> ObsFuture<'a, ()> {
        Box::pin(async move {
            let client = self.connected().await?;
            match client.recording().create_chapter(Some(name)).await {
                Ok(()) => Ok(()),
                Err(error) => Err(self.call_failed(error, true).await),
            }
        })
    }

    fn set_input_muted<'a>(&'a self, input: &'a str, muted: bool) -> ObsFuture<'a, ()> {
        Box::pin(async move {
            let client = self.connected().await?;
            let input_id = parse_uuid_token(input, INPUT_TOKEN_PREFIX)
                .map(obws::requests::inputs::InputId::Uuid)
                .unwrap_or_else(|| obws::requests::inputs::InputId::Name(input));
            match client.inputs().set_muted(input_id, muted).await {
                Ok(()) => Ok(()),
                Err(error) => Err(self.call_failed(error, true).await),
            }
        })
    }

    fn set_input_volume<'a>(&'a self, input: &'a str, volume: f32) -> ObsFuture<'a, ()> {
        Box::pin(async move {
            let client = self.connected().await?;
            match client
                .inputs()
                .set_volume(
                    parse_uuid_token(input, INPUT_TOKEN_PREFIX)
                        .map(obws::requests::inputs::InputId::Uuid)
                        .unwrap_or_else(|| obws::requests::inputs::InputId::Name(input)),
                    obws::requests::inputs::Volume::Mul(volume),
                )
                .await
            {
                Ok(()) => Ok(()),
                Err(error) => Err(self.call_failed(error, true).await),
            }
        })
    }

    fn recording_start(&self) -> ObsFuture<'_, ()> {
        Box::pin(async move {
            let client = self.connected().await?;
            self.finish_request(client.recording().start().await, true)
                .await
        })
    }

    fn recording_stop(&self) -> ObsFuture<'_, ()> {
        Box::pin(async move {
            let client = self.connected().await?;
            let result = client.recording().stop().await.map(|_| ());
            self.finish_request(result, true).await
        })
    }

    fn recording_pause(&self) -> ObsFuture<'_, ()> {
        Box::pin(async move {
            let client = self.connected().await?;
            self.finish_request(client.recording().pause().await, true)
                .await
        })
    }

    fn recording_resume(&self) -> ObsFuture<'_, ()> {
        Box::pin(async move {
            let client = self.connected().await?;
            self.finish_request(client.recording().resume().await, true)
                .await
        })
    }

    fn recording_status(&self) -> ObsFuture<'_, Values> {
        Box::pin(async move {
            let client = self.connected().await?;
            let status = self
                .finish_request(client.recording().status().await, false)
                .await?;
            Ok(Values::from([
                ("active".into(), json!(status.active)),
                ("paused".into(), json!(status.paused)),
                (
                    "timecode".into(),
                    json!(format_timecode(
                        status.timecode.whole_seconds(),
                        status.timecode.subsec_milliseconds()
                    )),
                ),
                (
                    "duration_ms".into(),
                    json!(status.duration.whole_milliseconds()),
                ),
                ("bytes".into(), json!(status.bytes)),
            ]))
        })
    }

    fn streaming_start(&self) -> ObsFuture<'_, ()> {
        Box::pin(async move {
            let client = self.connected().await?;
            self.finish_request(client.streaming().start().await, true)
                .await
        })
    }

    fn streaming_stop(&self) -> ObsFuture<'_, ()> {
        Box::pin(async move {
            let client = self.connected().await?;
            self.finish_request(client.streaming().stop().await, true)
                .await
        })
    }

    fn streaming_status(&self) -> ObsFuture<'_, Values> {
        Box::pin(async move {
            let client = self.connected().await?;
            let status = self
                .finish_request(client.streaming().status().await, false)
                .await?;
            Ok(Values::from([
                ("active".into(), json!(status.active)),
                ("reconnecting".into(), json!(status.reconnecting)),
                (
                    "timecode".into(),
                    json!(format_timecode(
                        status.timecode.whole_seconds(),
                        status.timecode.subsec_milliseconds()
                    )),
                ),
                (
                    "duration_ms".into(),
                    json!(status.duration.whole_milliseconds()),
                ),
                ("congestion".into(), json!(status.congestion)),
                ("bytes".into(), json!(status.bytes)),
                ("skipped_frames".into(), json!(status.skipped_frames)),
                ("total_frames".into(), json!(status.total_frames)),
            ]))
        })
    }

    fn replay_start(&self) -> ObsFuture<'_, ()> {
        Box::pin(async move {
            let client = self.connected().await?;
            self.finish_request(client.replay_buffer().start().await, true)
                .await
        })
    }

    fn replay_stop(&self) -> ObsFuture<'_, ()> {
        Box::pin(async move {
            let client = self.connected().await?;
            self.finish_request(client.replay_buffer().stop().await, true)
                .await
        })
    }

    fn replay_save(&self) -> ObsFuture<'_, ()> {
        Box::pin(async move {
            let client = self.connected().await?;
            self.finish_request(client.replay_buffer().save().await, true)
                .await
        })
    }

    fn replay_status(&self) -> ObsFuture<'_, Values> {
        Box::pin(async move {
            let client = self.connected().await?;
            let active = self
                .finish_request(client.replay_buffer().status().await, false)
                .await?;
            Ok(Values::from([(
                "active".into(),
                serde_json::Value::Bool(active),
            )]))
        })
    }

    fn resource_snapshot(&self) -> ObsFuture<'_, ObsResourceSnapshot> {
        Box::pin(async move {
            let client = self.connected().await?;
            let scene_list = self
                .finish_request(client.scenes().list().await, false)
                .await?;
            let input_list = self
                .finish_request(client.inputs().list(None).await, false)
                .await?;
            let mut scene_items = Vec::new();
            for scene in &scene_list.scenes {
                let root = ObsResource {
                    name: scene.id.name.clone(),
                    uuid: scene.id.uuid,
                };
                let mut pending = vec![(root.clone(), root.name.clone(), Vec::<String>::new())];
                while let Some((root, container, group_path)) = pending.pop() {
                    let scene_id = obws::requests::scenes::SceneId::Name(container.as_str());
                    let items = if group_path.is_empty() {
                        self.finish_request(client.scene_items().list(scene_id).await, false)
                            .await?
                    } else {
                        self.finish_request(client.scene_items().list_group(scene_id).await, false)
                            .await?
                    };
                    for item in items {
                        if item.is_group == Some(true)
                            && !group_path.iter().any(|name| name == &item.source_name)
                        {
                            let mut child_path = group_path.clone();
                            child_path.push(item.source_name.clone());
                            pending.push((root.clone(), item.source_name.clone(), child_path));
                        }
                        scene_items.push(ObsSceneItemResource {
                            scene: root.clone(),
                            group_path: group_path.clone(),
                            item_id: item.id,
                            source_name: item.source_name,
                        });
                    }
                }
            }
            let recording = self
                .finish_request(client.recording().status().await, false)
                .await?;
            let streaming = self
                .finish_request(client.streaming().status().await, false)
                .await?;
            let replay_buffer_active = self
                .finish_request(client.replay_buffer().status().await, false)
                .await?;
            let resource = |name: String, uuid: uuid::Uuid| ObsResource { name, uuid };
            Ok(ObsResourceSnapshot {
                scenes: scene_list
                    .scenes
                    .into_iter()
                    .map(|scene| resource(scene.id.name, scene.id.uuid))
                    .collect(),
                inputs: input_list
                    .into_iter()
                    .map(|input| resource(input.id.name, input.id.uuid))
                    .collect(),
                scene_items,
                current_program_scene: scene_list
                    .current_program_scene
                    .map(|scene| resource(scene.name, scene.uuid)),
                current_preview_scene: scene_list
                    .current_preview_scene
                    .map(|scene| resource(scene.name, scene.uuid)),
                recording: ObsRecordingStatus {
                    active: recording.active,
                    paused: recording.paused,
                },
                streaming: ObsStreamingStatus {
                    active: streaming.active,
                    reconnecting: streaming.reconnecting,
                },
                replay_buffer_active,
            })
        })
    }
}

fn format_timecode(seconds: i64, milliseconds: i16) -> String {
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        seconds / 3600,
        seconds % 3600 / 60,
        seconds % 60,
        milliseconds
    )
}

#[cfg(test)]
mod tests {
    use futures_util::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    use tokio::net::TcpListener;
    use tokio_websockets::{Message, ServerBuilder};

    use super::*;
    use crate::obs::credentials::ObsCredentialError;

    struct NoPassword;

    impl ObsCredentialStore for NoPassword {
        fn load(&self) -> Result<Option<String>, ObsCredentialError> {
            Ok(None)
        }
        fn save(&self, _: &str) -> Result<(), ObsCredentialError> {
            Ok(())
        }
        fn clear(&self) -> Result<(), ObsCredentialError> {
            Ok(())
        }
    }

    fn connection(port: u16) -> ObsConnection {
        ObsConnection {
            settings: RwLock::new(ObsSettings {
                enabled: true,
                host: "127.0.0.1".into(),
                port,
                tls: false,
            }),
            credentials: Arc::new(NoPassword),
            client: Mutex::new(None),
            health: StatusCell::new(ObsHealth::Disconnected),
            changed: watch::channel(0).0,
        }
    }

    #[test]
    fn event_health_cannot_make_action_connection_ready() {
        let connection = connection(4455);
        connection.health.set(ObsHealth::Connected).unwrap();
        assert_eq!(connection.ready().unwrap_err(), "OBS is not connected yet");
    }

    async fn receive_json(
        server: &mut tokio_websockets::WebSocketStream<tokio::net::TcpStream>,
    ) -> Value {
        let message = server.next().await.unwrap().unwrap();
        serde_json::from_str(message.as_text().unwrap()).unwrap()
    }

    async fn fake_server(
        listener: TcpListener,
        answer_chapter: bool,
        finished: Option<tokio::sync::oneshot::Receiver<()>>,
    ) -> Value {
        let (stream, _) = listener.accept().await.unwrap();
        let (_, mut server) = ServerBuilder::new().accept(stream).await.unwrap();
        server
            .send(Message::text(
                json!({"op":0,"d":{"obsWebSocketVersion":"5.5.0","rpcVersion":1}}).to_string(),
            ))
            .await
            .unwrap();
        let identify = receive_json(&mut server).await;
        assert_eq!(identify["op"], 1);
        assert_eq!(identify["d"]["eventSubscriptions"], 0);
        assert!(identify["d"].get("authentication").is_none());
        server
            .send(Message::text(
                json!({"op":2,"d":{"negotiatedRpcVersion":1}}).to_string(),
            ))
            .await
            .unwrap();

        let version = receive_json(&mut server).await;
        assert_eq!(version["d"]["requestType"], "GetVersion");
        let response = json!({
            "op": 7,
            "d": {
                "requestType": "GetVersion",
                "requestId": version["d"]["requestId"],
                "requestStatus": {"result": true, "code": 100},
                "responseData": {
                    "obsStudioVersion": "30.2.0",
                    "obsWebSocketVersion": "5.5.0",
                    "rpcVersion": 1,
                    "availableRequests": [],
                    "supportedImageFormats": [],
                    "platform": "macos",
                    "platformDescription": "test"
                }
            }
        });
        server
            .send(Message::text(response.to_string()))
            .await
            .unwrap();

        let chapter = receive_json(&mut server).await;
        assert_eq!(chapter["d"]["requestType"], "CreateRecordChapter");
        if answer_chapter {
            server
                .send(Message::text(
                    json!({
                        "op": 7,
                        "d": {
                            "requestType": "CreateRecordChapter",
                            "requestId": chapter["d"]["requestId"],
                            "requestStatus": {"result": true, "code": 100}
                        }
                    })
                    .to_string(),
                ))
                .await
                .unwrap();
            if let Some(finished) = finished {
                let _ = finished.await;
            }
        }
        chapter
    }

    #[tokio::test]
    async fn live_adapter_handshakes_and_sends_exact_chapter_name() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (finished_tx, finished_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(fake_server(listener, true, Some(finished_rx)));
        let connection = connection(port);
        tokio::time::timeout(
            Duration::from_secs(5),
            connection.create_chapter("TITLE CHANGE: Ready"),
        )
        .await
        .unwrap()
        .unwrap();
        let _ = finished_tx.send(());
        let chapter = server.await.unwrap();
        assert_eq!(
            chapter["d"]["requestData"]["chapterName"],
            "TITLE CHANGE: Ready"
        );
        assert_eq!(*connection.health.read().unwrap(), ObsHealth::Connected);
    }

    #[tokio::test]
    async fn lost_chapter_response_is_uncertain_and_never_retried() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(fake_server(listener, false, None));
        let connection = connection(port);
        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            connection.create_chapter("GAME CHANGE: Test"),
        )
        .await
        .unwrap();
        assert!(matches!(outcome, Err(ObsError::Uncertain(_))));
        let chapter = server.await.unwrap();
        assert_eq!(
            chapter["d"]["requestData"]["chapterName"],
            "GAME CHANGE: Test"
        );
    }

    #[test]
    fn formats_obs_timecodes_with_millisecond_precision() {
        assert_eq!(format_timecode(3_723, 45), "01:02:03.045");
        assert_eq!(format_timecode(0, 0), "00:00:00.000");
    }
}
