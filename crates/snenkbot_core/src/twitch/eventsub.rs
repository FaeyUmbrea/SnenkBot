//! Twitch EventSub WebSocket framing. Subscription ownership belongs to the Twitch integration.

use std::time::Duration;

use futures_util::StreamExt;
use serde_json::Value;
use thiserror::Error;
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_websockets::{ClientBuilder, MaybeTlsStream, WebSocketStream};

const MAX_FRAME_BYTES: usize = 1024 * 1024;
const CONNECT_DEADLINE: Duration = Duration::from_secs(10);
const MAX_KEEPALIVE: Duration = Duration::from_secs(600);

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EventSubError {
    #[error("Twitch EventSub connection was lost")]
    Disconnected,
    #[error("Twitch EventSub stopped sending keepalives")]
    KeepaliveExpired,
    #[error("Twitch EventSub sent an invalid protocol message")]
    Protocol,
    #[error("Twitch EventSub sent an oversized message")]
    FrameTooLarge,
}

#[derive(Debug)]
pub enum EventSubFrame {
    Notification {
        id: String,
        subscription_type: String,
        envelope: Value,
    },
    Reconnect {
        url: String,
    },
    Revocation {
        subscription_type: String,
        status: String,
    },
    Keepalive,
}

pub struct EventSubSocket {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    session_id: String,
    keepalive: Duration,
}

impl EventSubSocket {
    pub async fn connect(url: &str) -> Result<Self, EventSubError> {
        let uri = url.parse().map_err(|_| EventSubError::Protocol)?;
        let (socket, _) = timeout(CONNECT_DEADLINE, ClientBuilder::from_uri(uri).connect())
            .await
            .map_err(|_| EventSubError::Disconnected)?
            .map_err(|_| EventSubError::Disconnected)?;
        let mut stream = Self {
            socket,
            session_id: String::new(),
            keepalive: CONNECT_DEADLINE,
        };
        let welcome = stream.read_frame().await?;
        let (session_id, keepalive) = parse_welcome(&welcome)?;
        stream.session_id = session_id;
        stream.keepalive = keepalive;
        Ok(stream)
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub async fn next_frame(&mut self) -> Result<EventSubFrame, EventSubError> {
        let frame = self.read_frame().await?;
        parse_frame(&frame)
    }

    async fn read_frame(&mut self) -> Result<Value, EventSubError> {
        loop {
            let message = timeout(self.keepalive, self.socket.next())
                .await
                .map_err(|_| EventSubError::KeepaliveExpired)?
                .ok_or(EventSubError::Disconnected)?
                .map_err(|_| EventSubError::Disconnected)?;
            if message.as_close().is_some() {
                return Err(EventSubError::Disconnected);
            }
            let Some(text) = message.as_text() else {
                continue;
            };
            if text.len() > MAX_FRAME_BYTES {
                return Err(EventSubError::FrameTooLarge);
            }
            return serde_json::from_str(text).map_err(|_| EventSubError::Protocol);
        }
    }
}

fn parse_welcome(value: &Value) -> Result<(String, Duration), EventSubError> {
    if message_type(value)? != "session_welcome" {
        return Err(EventSubError::Protocol);
    }
    let session = value
        .pointer("/payload/session")
        .ok_or(EventSubError::Protocol)?;
    let id = session
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or(EventSubError::Protocol)?;
    let seconds = session
        .get("keepalive_timeout_seconds")
        .and_then(Value::as_u64)
        .filter(|seconds| (10..=600).contains(seconds))
        .ok_or(EventSubError::Protocol)?;
    Ok((
        id.to_owned(),
        Duration::from_secs(seconds).min(MAX_KEEPALIVE),
    ))
}

fn parse_frame(value: &Value) -> Result<EventSubFrame, EventSubError> {
    match message_type(value)? {
        "session_keepalive" => Ok(EventSubFrame::Keepalive),
        "session_reconnect" => {
            let url = value
                .pointer("/payload/session/reconnect_url")
                .and_then(Value::as_str)
                .filter(|url| !url.is_empty())
                .ok_or(EventSubError::Protocol)?;
            Ok(EventSubFrame::Reconnect {
                url: url.to_owned(),
            })
        }
        "notification" => {
            let id = value
                .pointer("/metadata/message_id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .ok_or(EventSubError::Protocol)?;
            let subscription_type = value
                .pointer("/payload/subscription/type")
                .and_then(Value::as_str)
                .filter(|kind| !kind.is_empty())
                .ok_or(EventSubError::Protocol)?;
            value
                .pointer("/payload/event")
                .filter(|event| event.is_object())
                .ok_or(EventSubError::Protocol)?;
            Ok(EventSubFrame::Notification {
                id: id.to_owned(),
                subscription_type: subscription_type.to_owned(),
                envelope: value.clone(),
            })
        }
        "revocation" => {
            let subscription_type = value
                .pointer("/payload/subscription/type")
                .and_then(Value::as_str)
                .ok_or(EventSubError::Protocol)?;
            let status = value
                .pointer("/payload/subscription/status")
                .and_then(Value::as_str)
                .ok_or(EventSubError::Protocol)?;
            Ok(EventSubFrame::Revocation {
                subscription_type: subscription_type.to_owned(),
                status: status.to_owned(),
            })
        }
        _ => Err(EventSubError::Protocol),
    }
}

fn message_type(value: &Value) -> Result<&str, EventSubError> {
    value
        .pointer("/metadata/message_type")
        .and_then(Value::as_str)
        .ok_or(EventSubError::Protocol)
}

pub fn valid_twitch_url(url: &str) -> bool {
    reqwest::Url::parse(url).is_ok_and(|parsed| {
        parsed.scheme() == "wss"
            && parsed.host_str() == Some("eventsub.wss.twitch.tv")
            && parsed.port().is_none()
            && matches!(parsed.path(), "/" | "/ws")
            && parsed.username().is_empty()
            && parsed.password().is_none()
            && parsed.fragment().is_none()
    })
}

#[cfg(test)]
mod tests {
    use futures_util::SinkExt;
    use serde_json::json;
    use tokio::net::TcpListener;
    use tokio_websockets::{Message, ServerBuilder};

    use super::*;

    #[test]
    fn parses_welcome_and_frames() {
        let welcome = json!({"metadata":{"message_type":"session_welcome"},"payload":{"session":{"id":"socket-1","keepalive_timeout_seconds":10}}});
        assert_eq!(
            parse_welcome(&welcome).unwrap(),
            ("socket-1".into(), Duration::from_secs(10))
        );
        let notification = json!({"metadata":{"message_type":"notification","message_id":"m1"},"payload":{"subscription":{"type":"channel.chat.message"},"event":{"message":{"text":"hi"}}}});
        assert!(
            matches!(parse_frame(&notification).unwrap(), EventSubFrame::Notification { id, subscription_type, .. } if id == "m1" && subscription_type == "channel.chat.message")
        );
        assert!(matches!(
            parse_frame(&json!({"metadata":{"message_type":"session_keepalive"}})).unwrap(),
            EventSubFrame::Keepalive
        ));
        assert!(matches!(parse_frame(&json!({"metadata":{"message_type":"session_reconnect"},"payload":{"session":{"reconnect_url":"wss://eventsub.wss.twitch.tv/ws?x=1"}}})).unwrap(), EventSubFrame::Reconnect { .. }));
        assert!(matches!(parse_frame(&json!({"metadata":{"message_type":"revocation"},"payload":{"subscription":{"type":"channel.chat.message","status":"authorization_revoked"}}})).unwrap(), EventSubFrame::Revocation { .. }));
    }

    #[test]
    fn rejects_malformed_frames_and_unsafe_reconnect_urls() {
        assert_eq!(
            parse_welcome(&json!({"metadata":{"message_type":"notification"}})),
            Err(EventSubError::Protocol)
        );
        assert!(
            parse_frame(&json!({"metadata":{"message_type":"notification"},"payload":{}})).is_err()
        );
        assert!(valid_twitch_url(
            "wss://eventsub.wss.twitch.tv/ws?keepalive_timeout_seconds=30"
        ));
        // Twitch's reconnect address can use the origin root instead of /ws.
        assert!(valid_twitch_url(
            "wss://eventsub.wss.twitch.tv?session=reconnect"
        ));
        assert!(valid_twitch_url(
            "wss://eventsub.wss.twitch.tv/?session=reconnect"
        ));
        assert!(!valid_twitch_url("ws://eventsub.wss.twitch.tv/ws"));
        assert!(!valid_twitch_url(
            "wss://eventsub.wss.twitch.tv.evil.test/ws"
        ));
        assert!(!valid_twitch_url("wss://eventsub.wss.twitch.tv:444/ws"));
        assert!(!valid_twitch_url("wss://eventsub.wss.twitch.tv/other"));
        assert!(!valid_twitch_url("wss://user@eventsub.wss.twitch.tv/"));
        assert!(!valid_twitch_url("wss://eventsub.wss.twitch.tv/#fragment"));
    }

    #[tokio::test]
    async fn socket_requires_welcome_and_reports_disconnection() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let (_, mut socket) = ServerBuilder::new().accept(tcp).await.unwrap();
            socket.send(Message::text(json!({"metadata":{"message_type":"session_welcome"},"payload":{"session":{"id":"s1","keepalive_timeout_seconds":10}}}).to_string())).await.unwrap();
            socket
                .send(Message::text(
                    json!({"metadata":{"message_type":"session_keepalive"},"payload":{}})
                        .to_string(),
                ))
                .await
                .unwrap();
        });
        let mut socket = EventSubSocket::connect(&url).await.unwrap();
        assert_eq!(socket.session_id(), "s1");
        assert!(matches!(
            socket.next_frame().await.unwrap(),
            EventSubFrame::Keepalive
        ));
        assert_eq!(
            socket.next_frame().await.err(),
            Some(EventSubError::Disconnected)
        );
        server.await.unwrap();
    }
}
