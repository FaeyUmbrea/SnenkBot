//! Typed access to the Twitch Helix operations used by workflows.

use std::{future::Future, time::Duration};

use reqwest::{Method, Url, header};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

const API_ROOT: &str = "https://api.twitch.tv/helix/";
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HelixMethod {
    Get,
    Post,
    Patch,
    Delete,
}

/// HTTP request passed to the transport. Debug deliberately omits credentials and body.
pub struct HelixRequest {
    pub method: HelixMethod,
    pub url: Url,
    access_token: String,
    body: Option<Vec<u8>>,
}

impl HelixRequest {
    pub fn access_token(&self) -> &str {
        &self.access_token
    }

    pub fn body(&self) -> Option<&[u8]> {
        self.body.as_deref()
    }
}

impl std::fmt::Debug for HelixRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HelixRequest")
            .field("method", &self.method)
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

pub struct HelixResponse {
    pub status: u16,
    pub body: Vec<u8>,
    pub retry_after: Option<Duration>,
}

pub trait HelixTransport: Send + Sync {
    fn send(
        &self,
        request: HelixRequest,
    ) -> impl Future<Output = Result<HelixResponse, HelixError>> + Send;
}

#[derive(Clone)]
pub struct ReqwestHelixTransport(reqwest::Client);

impl Default for ReqwestHelixTransport {
    fn default() -> Self {
        Self(
            reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("static Twitch HTTP client configuration"),
        )
    }
}

impl HelixTransport for ReqwestHelixTransport {
    async fn send(&self, request: HelixRequest) -> Result<HelixResponse, HelixError> {
        let method = match request.method {
            HelixMethod::Get => Method::GET,
            HelixMethod::Post => Method::POST,
            HelixMethod::Patch => Method::PATCH,
            HelixMethod::Delete => Method::DELETE,
        };
        let mut builder = self
            .0
            .request(method, request.url)
            .header("Client-Id", crate::twitch::device::CLIENT_ID)
            .bearer_auth(request.access_token);
        if let Some(body) = request.body {
            builder = builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(body);
        }
        let response = builder.send().await.map_err(|_| HelixError::Transport)?;
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .map(Duration::from_secs);
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err(HelixError::ResponseTooLarge);
        }
        let mut response = response;
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| HelixError::Transport)? {
            if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(HelixError::ResponseTooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(HelixResponse {
            status,
            body,
            retry_after,
        })
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HelixError {
    #[error("Twitch request failed")]
    Transport,
    #[error("Twitch rejected the access token")]
    Unauthorized,
    #[error("Twitch denied this operation")]
    Forbidden,
    #[error("Twitch rate limit reached")]
    RateLimited { retry_after: Option<Duration> },
    #[error("Twitch resource was not found")]
    NotFound,
    #[error("Twitch returned HTTP {0}")]
    Http(u16),
    #[error("Twitch response exceeded the size limit")]
    ResponseTooLarge,
    #[error("Twitch returned an invalid response")]
    InvalidResponse,
    #[error("request values are invalid")]
    InvalidInput,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatMessageResult {
    pub message_id: String,
    pub is_sent: bool,
    pub drop_reason: Option<ChatDropReason>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ChatDropReason {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ChannelInfo {
    pub broadcaster_id: String,
    pub broadcaster_login: String,
    pub broadcaster_name: String,
    pub game_id: String,
    pub game_name: String,
    pub title: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChannelUpdate {
    pub title: Option<String>,
    /// Empty string or "0" clears the current game, following Twitch's API.
    pub game_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Game {
    pub id: String,
    pub name: String,
    pub box_art_url: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct User {
    pub id: String,
    pub login: String,
    pub display_name: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventSubSubscription {
    pub id: String,
}

pub struct Helix<T = ReqwestHelixTransport> {
    transport: T,
}

impl Default for Helix<ReqwestHelixTransport> {
    fn default() -> Self {
        Self::new(ReqwestHelixTransport::default())
    }
}

impl<T: HelixTransport> Helix<T> {
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    pub async fn create_eventsub_subscription(
        &self,
        access_token: &str,
        event_type: &str,
        version: &str,
        broadcaster_id: &str,
        session_id: &str,
    ) -> Result<EventSubSubscription, HelixError> {
        if event_type.is_empty()
            || version.is_empty()
            || broadcaster_id.is_empty()
            || session_id.is_empty()
        {
            return Err(HelixError::InvalidInput);
        }
        #[derive(Serialize)]
        struct Condition<'a> {
            broadcaster_user_id: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            user_id: Option<&'a str>,
        }
        #[derive(Serialize)]
        struct Transport<'a> {
            method: &'static str,
            session_id: &'a str,
        }
        #[derive(Serialize)]
        struct Request<'a> {
            #[serde(rename = "type")]
            event_type: &'a str,
            version: &'a str,
            condition: Condition<'a>,
            transport: Transport<'a>,
        }
        #[derive(Deserialize)]
        struct Reply {
            data: Vec<Subscription>,
        }
        #[derive(Deserialize)]
        struct Subscription {
            id: String,
        }
        let body = serde_json::to_vec(&Request {
            event_type,
            version,
            condition: Condition {
                broadcaster_user_id: broadcaster_id,
                user_id: event_type
                    .starts_with("channel.chat.")
                    .then_some(broadcaster_id),
            },
            transport: Transport {
                method: "websocket",
                session_id,
            },
        })
        .map_err(|_| HelixError::InvalidInput)?;
        let reply: Reply = self
            .execute(
                HelixMethod::Post,
                "eventsub/subscriptions",
                &[],
                access_token,
                Some(body),
            )
            .await?;
        let id = reply
            .data
            .into_iter()
            .next()
            .map(|subscription| subscription.id)
            .filter(|id| !id.is_empty())
            .ok_or(HelixError::InvalidResponse)?;
        Ok(EventSubSubscription { id })
    }

    pub async fn delete_eventsub_subscription(
        &self,
        access_token: &str,
        id: &str,
    ) -> Result<(), HelixError> {
        if id.is_empty() {
            return Err(HelixError::InvalidInput);
        }
        self.execute_status(
            HelixMethod::Delete,
            "eventsub/subscriptions",
            &[("id", id)],
            access_token,
            None,
        )
        .await
    }

    pub async fn send_chat_message(
        &self,
        access_token: &str,
        broadcaster_id: &str,
        sender_id: &str,
        message: &str,
    ) -> Result<ChatMessageResult, HelixError> {
        if broadcaster_id.is_empty()
            || sender_id.is_empty()
            || message.trim().is_empty()
            || message.chars().count() > 500
        {
            return Err(HelixError::InvalidInput);
        }
        #[derive(Serialize)]
        struct Body<'a> {
            broadcaster_id: &'a str,
            sender_id: &'a str,
            message: &'a str,
        }
        #[derive(Deserialize)]
        struct Reply {
            data: Vec<ChatReply>,
        }
        #[derive(Deserialize)]
        struct ChatReply {
            message_id: String,
            is_sent: bool,
            drop_reason: Option<ChatDropReason>,
        }
        let reply: Reply = self
            .execute(
                HelixMethod::Post,
                "chat/messages",
                &[],
                access_token,
                Some(
                    serde_json::to_vec(&Body {
                        broadcaster_id,
                        sender_id,
                        message,
                    })
                    .map_err(|_| HelixError::InvalidInput)?,
                ),
            )
            .await?;
        let result = reply
            .data
            .into_iter()
            .next()
            .ok_or(HelixError::InvalidResponse)?;
        if result.is_sent && result.message_id.is_empty() {
            return Err(HelixError::InvalidResponse);
        }
        Ok(ChatMessageResult {
            message_id: result.message_id,
            is_sent: result.is_sent,
            drop_reason: result.drop_reason,
        })
    }

    pub async fn get_channel_information(
        &self,
        access_token: &str,
        broadcaster_id: &str,
    ) -> Result<Option<ChannelInfo>, HelixError> {
        if broadcaster_id.is_empty() {
            return Err(HelixError::InvalidInput);
        }
        #[derive(Deserialize)]
        struct Reply {
            data: Vec<ChannelInfo>,
        }
        let reply: Reply = self
            .execute(
                HelixMethod::Get,
                "channels",
                &[("broadcaster_id", broadcaster_id)],
                access_token,
                None,
            )
            .await?;
        Ok(reply.data.into_iter().next())
    }

    pub async fn modify_channel_information(
        &self,
        access_token: &str,
        broadcaster_id: &str,
        update: &ChannelUpdate,
    ) -> Result<(), HelixError> {
        if broadcaster_id.is_empty()
            || (update.title.is_none() && update.game_id.is_none())
            || update
                .title
                .as_ref()
                .is_some_and(|v| v.is_empty() || v.chars().count() > 140)
        {
            return Err(HelixError::InvalidInput);
        }
        #[derive(Serialize)]
        struct Body<'a> {
            #[serde(skip_serializing_if = "Option::is_none")]
            game_id: &'a Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            title: &'a Option<String>,
        }
        let body = serde_json::to_vec(&Body {
            game_id: &update.game_id,
            title: &update.title,
        })
        .map_err(|_| HelixError::InvalidInput)?;
        self.execute_status(
            HelixMethod::Patch,
            "channels",
            &[("broadcaster_id", broadcaster_id)],
            access_token,
            Some(body),
        )
        .await?;
        Ok(())
    }

    pub async fn get_games_by_name(
        &self,
        access_token: &str,
        name: &str,
    ) -> Result<Vec<Game>, HelixError> {
        if name.trim().is_empty() {
            return Err(HelixError::InvalidInput);
        }
        #[derive(Deserialize)]
        struct Reply {
            data: Vec<Game>,
        }
        let reply: Reply = self
            .execute(
                HelixMethod::Get,
                "games",
                &[("name", name)],
                access_token,
                None,
            )
            .await?;
        Ok(reply
            .data
            .into_iter()
            .filter(|game| game.name.eq_ignore_ascii_case(name))
            .collect())
    }

    pub async fn get_user_by_login(
        &self,
        access_token: &str,
        login: &str,
    ) -> Result<Option<User>, HelixError> {
        if login.trim().is_empty() {
            return Err(HelixError::InvalidInput);
        }
        #[derive(Deserialize)]
        struct Reply {
            data: Vec<User>,
        }
        let reply: Reply = self
            .execute(
                HelixMethod::Get,
                "users",
                &[("login", login)],
                access_token,
                None,
            )
            .await?;
        Ok(reply
            .data
            .into_iter()
            .find(|user| user.login.eq_ignore_ascii_case(login)))
    }

    async fn execute<R: DeserializeOwned>(
        &self,
        method: HelixMethod,
        path: &str,
        query: &[(&str, &str)],
        access_token: &str,
        body: Option<Vec<u8>>,
    ) -> Result<R, HelixError> {
        let response = self.send(method, path, query, access_token, body).await?;
        serde_json::from_slice(&response.body).map_err(|_| HelixError::InvalidResponse)
    }

    async fn execute_status(
        &self,
        method: HelixMethod,
        path: &str,
        query: &[(&str, &str)],
        access_token: &str,
        body: Option<Vec<u8>>,
    ) -> Result<(), HelixError> {
        self.send(method, path, query, access_token, body)
            .await
            .map(|_| ())
    }

    async fn send(
        &self,
        method: HelixMethod,
        path: &str,
        query: &[(&str, &str)],
        access_token: &str,
        body: Option<Vec<u8>>,
    ) -> Result<HelixResponse, HelixError> {
        if access_token.is_empty() {
            return Err(HelixError::InvalidInput);
        }
        let mut url = Url::parse(API_ROOT).map_err(|_| HelixError::InvalidInput)?;
        url.set_path(&format!("helix/{path}"));
        url.query_pairs_mut().extend_pairs(query.iter().copied());
        let response = self
            .transport
            .send(HelixRequest {
                method,
                url,
                access_token: access_token.to_owned(),
                body,
            })
            .await?;
        match response.status {
            200..=299 => Ok(response),
            401 => Err(HelixError::Unauthorized),
            403 => Err(HelixError::Forbidden),
            404 => Err(HelixError::NotFound),
            429 => Err(HelixError::RateLimited {
                retry_after: response.retry_after,
            }),
            status => Err(HelixError::Http(status)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct Fake(Arc<Mutex<Vec<(HelixRequest, HelixResponse)>>>);
    impl HelixTransport for Fake {
        async fn send(&self, request: HelixRequest) -> Result<HelixResponse, HelixError> {
            let (expected, response) = self.0.lock().unwrap().remove(0);
            assert_eq!(request.method, expected.method);
            assert_eq!(request.url, expected.url);
            assert_eq!(request.body, expected.body);
            assert_eq!(request.access_token, expected.access_token);
            Ok(response)
        }
    }
    fn response(status: u16, body: &str) -> HelixResponse {
        HelixResponse {
            status,
            body: body.as_bytes().to_vec(),
            retry_after: None,
        }
    }
    fn expected(
        method: HelixMethod,
        path: &str,
        pairs: &[(&str, &str)],
        body: Option<&str>,
    ) -> (HelixRequest, HelixResponse) {
        let mut url = Url::parse(API_ROOT).unwrap();
        url.set_path(&format!("helix/{path}"));
        url.query_pairs_mut().extend_pairs(pairs.iter().copied());
        (
            HelixRequest {
                method,
                url,
                access_token: "secret-token".into(),
                body: body.map(|s| s.as_bytes().to_vec()),
            },
            response(200, "{}"),
        )
    }
    fn client(reply: HelixResponse, req: HelixRequest) -> Helix<Fake> {
        Helix::new(Fake(Arc::new(Mutex::new(vec![(req, reply)]))))
    }

    #[tokio::test]
    async fn send_message_includes_both_ids_and_reports_drop() {
        let (req, _) = expected(
            HelixMethod::Post,
            "chat/messages",
            &[],
            Some(r#"{"broadcaster_id":"b1","sender_id":"s2","message":"hello"}"#),
        );
        let c = client(
            response(
                200,
                r#"{"data":[{"message_id":"m1","is_sent":false,"drop_reason":{"code":"rate_limited","message":"Slow down"}}]}"#,
            ),
            req,
        );
        let result = c
            .send_chat_message("secret-token", "b1", "s2", "hello")
            .await
            .unwrap();
        assert_eq!(result.message_id, "m1");
        assert!(!result.is_sent);
        assert_eq!(result.drop_reason.unwrap().code, "rate_limited");
    }

    #[tokio::test]
    async fn creates_and_deletes_eventsub_websocket_subscriptions() {
        let (chat_request, _) = expected(
            HelixMethod::Post,
            "eventsub/subscriptions",
            &[],
            Some(
                r#"{"type":"channel.chat.message","version":"1","condition":{"broadcaster_user_id":"b1","user_id":"b1"},"transport":{"method":"websocket","session_id":"socket-1"}}"#,
            ),
        );
        let (ad_request, _) = expected(
            HelixMethod::Post,
            "eventsub/subscriptions",
            &[],
            Some(
                r#"{"type":"channel.ad_break.begin","version":"1","condition":{"broadcaster_user_id":"b1"},"transport":{"method":"websocket","session_id":"socket-1"}}"#,
            ),
        );
        let (delete_request, _) = expected(
            HelixMethod::Delete,
            "eventsub/subscriptions",
            &[("id", "subscription-1")],
            None,
        );
        let client = Helix::new(Fake(Arc::new(Mutex::new(vec![
            (
                chat_request,
                response(202, r#"{"data":[{"id":"subscription-1"}]}"#),
            ),
            (
                ad_request,
                response(202, r#"{"data":[{"id":"subscription-2"}]}"#),
            ),
            (delete_request, response(204, "")),
        ]))));
        let chat = client
            .create_eventsub_subscription(
                "secret-token",
                "channel.chat.message",
                "1",
                "b1",
                "socket-1",
            )
            .await
            .unwrap();
        assert_eq!(chat.id, "subscription-1");
        let ad = client
            .create_eventsub_subscription(
                "secret-token",
                "channel.ad_break.begin",
                "1",
                "b1",
                "socket-1",
            )
            .await
            .unwrap();
        assert_eq!(ad.id, "subscription-2");
        client
            .delete_eventsub_subscription("secret-token", &chat.id)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn gets_exact_game_name_and_encodes_query() {
        let (req, _) = expected(HelixMethod::Get, "games", &[("name", "A & B")], None);
        let c = client(
            response(
                200,
                r#"{"data":[{"id":"1","name":"A & B","box_art_url":"x"},{"id":"2","name":"A & B Deluxe","box_art_url":"y"}]}"#,
            ),
            req,
        );
        let games = c.get_games_by_name("secret-token", "A & B").await.unwrap();
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].id, "1");
    }

    #[tokio::test]
    async fn gets_channel_and_user_by_requested_identity() {
        let (channel_request, _) = expected(
            HelixMethod::Get,
            "channels",
            &[("broadcaster_id", "123")],
            None,
        );
        let (user_request, _) = expected(HelixMethod::Get, "users", &[("login", "faey")], None);
        let fake = Fake(Arc::new(Mutex::new(vec![
            (
                channel_request,
                response(
                    200,
                    r#"{"data":[{"broadcaster_id":"123","broadcaster_login":"faey","broadcaster_name":"Faey","game_id":"42","game_name":"Example","title":"Live"}]}"#,
                ),
            ),
            (
                user_request,
                response(
                    200,
                    r#"{"data":[{"id":"123","login":"faey","display_name":"Faey","description":""}]}"#,
                ),
            ),
        ])));
        let api = Helix::new(fake);
        let channel = api
            .get_channel_information("secret-token", "123")
            .await
            .unwrap()
            .unwrap();
        let user = api
            .get_user_by_login("secret-token", "faey")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(channel.title, "Live");
        assert_eq!(user.id, "123");
    }

    #[tokio::test]
    async fn modify_only_serializes_requested_fields() {
        let (req, _) = expected(
            HelixMethod::Patch,
            "channels",
            &[("broadcaster_id", "123")],
            Some(r#"{"game_id":"42","title":"New title"}"#),
        );
        let c = client(response(204, ""), req);
        c.modify_channel_information(
            "secret-token",
            "123",
            &ChannelUpdate {
                title: Some("New title".into()),
                game_id: Some("42".into()),
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn auth_and_rate_limit_are_distinct_and_do_not_leak_body() {
        let (req, _) = expected(HelixMethod::Get, "users", &[("login", "faey")], None);
        let c = client(response(401, "secret response"), req);
        let error = c
            .get_user_by_login("secret-token", "faey")
            .await
            .unwrap_err();
        assert_eq!(error, HelixError::Unauthorized);
        assert!(!error.to_string().contains("secret"));
        let (req, _) = expected(
            HelixMethod::Get,
            "channels",
            &[("broadcaster_id", "1")],
            None,
        );
        let rate = HelixResponse {
            status: 429,
            body: Vec::new(),
            retry_after: Some(Duration::from_secs(7)),
        };
        let c = client(rate, req);
        assert_eq!(
            c.get_channel_information("secret-token", "1")
                .await
                .unwrap_err(),
            HelixError::RateLimited {
                retry_after: Some(Duration::from_secs(7))
            }
        );
    }

    #[tokio::test]
    async fn validates_input_before_sending() {
        let (req, _) = expected(HelixMethod::Post, "chat/messages", &[], None);
        let c = client(response(200, "{}"), req);
        assert_eq!(
            c.send_chat_message("secret-token", "b", "s", " ")
                .await
                .unwrap_err(),
            HelixError::InvalidInput
        );
    }
}
