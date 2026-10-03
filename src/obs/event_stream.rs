//! OBS event transport. Keep close codes visible so an invalidated session never reconnects.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::{SinkExt, StreamExt};
use obws::{events::Event, requests::EventSubscription};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::net::TcpStream;
use tokio_websockets::{ClientBuilder, MaybeTlsStream, Message, WebSocketStream};

use std::sync::Arc;

use super::{credentials::ObsCredentialStore, settings::ObsSettings};

#[derive(Debug, Error)]
pub(super) enum EventStreamError {
    #[error("OBS event connection was lost")]
    Disconnected,
    #[error("OBS rejected the event connection")]
    Rejected,
    #[error(
        "OBS event session was invalidated; change OBS connection settings before reconnecting"
    )]
    SessionInvalidated,
    #[error("OBS sent an invalid event protocol message")]
    Protocol,
    #[error("OBS event authentication is unavailable")]
    Credential,
}

impl EventStreamError {
    pub(super) fn requires_configuration(&self) -> bool {
        !matches!(self, Self::Disconnected)
    }
}

pub(super) struct ObsEventStream {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl ObsEventStream {
    pub(super) async fn connect(
        settings: &ObsSettings,
        credentials: Arc<dyn ObsCredentialStore>,
        subscription: EventSubscription,
    ) -> Result<Self, EventStreamError> {
        let scheme = if settings.tls { "wss" } else { "ws" };
        let url = format!("{scheme}://{}:{}", settings.host, settings.port);
        let uri = url.parse().map_err(|_| EventStreamError::Protocol)?;
        let (socket, _) = ClientBuilder::from_uri(uri)
            .connect()
            .await
            .map_err(|_| EventStreamError::Disconnected)?;
        let mut stream = Self { socket };
        let hello = stream.read_op(0).await?;
        let rpc_version = hello
            .get("rpcVersion")
            .and_then(Value::as_u64)
            .ok_or(EventStreamError::Protocol)?;
        if rpc_version < 1 {
            return Err(EventStreamError::Protocol);
        }
        let authentication = match hello.get("authentication") {
            Some(value) if !value.is_null() => {
                let challenge = value
                    .get("challenge")
                    .and_then(Value::as_str)
                    .ok_or(EventStreamError::Protocol)?;
                let salt = value
                    .get("salt")
                    .and_then(Value::as_str)
                    .ok_or(EventStreamError::Protocol)?;
                let password = tokio::task::spawn_blocking(move || credentials.load())
                    .await
                    .map_err(|_| EventStreamError::Credential)?
                    .map_err(|_| EventStreamError::Credential)?
                    .ok_or(EventStreamError::Credential)?;
                Some(authentication(&password, salt, challenge))
            }
            _ => None,
        };
        let mut identify = json!({
            "rpcVersion": rpc_version.min(1),
            "eventSubscriptions": subscription.bits(),
        });
        if let Some(authentication) = authentication {
            identify["authentication"] = Value::String(authentication);
        }
        stream
            .socket
            .send(Message::text(json!({"op": 1, "d": identify}).to_string()))
            .await
            .map_err(|_| EventStreamError::Disconnected)?;
        stream.read_op(2).await?;
        Ok(stream)
    }

    pub(super) async fn next_event(&mut self) -> Result<Event, EventStreamError> {
        let (op, data) = self.read_message().await?;
        if op == 5 {
            serde_json::from_value(data).map_err(|_| EventStreamError::Protocol)
        } else {
            Err(EventStreamError::Protocol)
        }
    }

    async fn read_op(&mut self, expected: u64) -> Result<Value, EventStreamError> {
        let (op, data) = self.read_message().await?;
        if op != expected {
            return Err(EventStreamError::Protocol);
        }
        Ok(data)
    }

    async fn read_message(&mut self) -> Result<(u64, Value), EventStreamError> {
        loop {
            let message = self
                .socket
                .next()
                .await
                .ok_or(EventStreamError::Disconnected)?
                .map_err(|_| EventStreamError::Disconnected)?;
            if let Some((code, _)) = message.as_close() {
                return Err(if u16::from(code) == 4011 {
                    EventStreamError::SessionInvalidated
                } else {
                    EventStreamError::Rejected
                });
            }
            if let Some(text) = message.as_text() {
                let value: Value =
                    serde_json::from_str(text).map_err(|_| EventStreamError::Protocol)?;
                let op = value
                    .get("op")
                    .and_then(Value::as_u64)
                    .ok_or(EventStreamError::Protocol)?;
                let data = value.get("d").cloned().ok_or(EventStreamError::Protocol)?;
                return Ok((op, data));
            }
        }
    }
}

fn authentication(password: &str, salt: &str, challenge: &str) -> String {
    let secret = STANDARD.encode(Sha256::digest(format!("{password}{salt}").as_bytes()));
    STANDARD.encode(Sha256::digest(format!("{secret}{challenge}").as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::obs::credentials::ObsCredentialError;
    use tokio::net::TcpListener;
    use tokio_websockets::{CloseCode, ServerBuilder};

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

    #[test]
    fn computes_obs_authentication_response() {
        assert_eq!(
            authentication("secret", "salt", "challenge"),
            "39cfhx7et2iyoMZvoQ6o3OPLNSKgtMmy48GQ7jnvsdE="
        );
    }

    #[tokio::test]
    async fn subscribes_to_requested_events_and_exposes_session_invalidation() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let settings = ObsSettings {
            enabled: true,
            host: "127.0.0.1".into(),
            port: listener.local_addr().unwrap().port(),
            tls: false,
        };
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let (_, mut socket) = ServerBuilder::new().accept(socket).await.unwrap();
            socket
                .send(Message::text(
                    json!({"op":0,"d":{"rpcVersion":1}}).to_string(),
                ))
                .await
                .unwrap();
            let identify = socket.next().await.unwrap().unwrap();
            let identify: Value = serde_json::from_str(identify.as_text().unwrap()).unwrap();
            assert_eq!(identify["op"], 1);
            assert_eq!(
                identify["d"]["eventSubscriptions"],
                EventSubscription::OUTPUTS.bits()
            );
            assert!(identify["d"].get("authentication").is_none());
            socket
                .send(Message::text(
                    json!({"op":2,"d":{"negotiatedRpcVersion":1}}).to_string(),
                ))
                .await
                .unwrap();
            socket
                .send(Message::text(json!({"op":5,"d":{
                    "eventType":"RecordStateChanged",
                    "eventData":{"outputActive":true,"outputState":"OBS_WEBSOCKET_OUTPUT_STARTED"}
                }}).to_string()))
                .await
                .unwrap();
            socket
                .send(Message::close(
                    Some(CloseCode::try_from(4011).unwrap()),
                    "invalidated",
                ))
                .await
                .unwrap();
        });
        let mut stream =
            ObsEventStream::connect(&settings, Arc::new(NoPassword), EventSubscription::OUTPUTS)
                .await
                .unwrap();
        assert!(matches!(
            stream.next_event().await.unwrap(),
            Event::RecordStateChanged { .. }
        ));
        assert!(matches!(
            stream.next_event().await,
            Err(EventStreamError::SessionInvalidated)
        ));
        server.await.unwrap();
    }
}
