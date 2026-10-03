//! Twitch's public-client device authorization flow.
//!
//! A successful exchange yields unaccepted tokens. The caller must validate the
//! access token and confirm the resulting account before storing either token.

use std::fmt;
use std::future::Future;
use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio_util::sync::CancellationToken;

pub const CLIENT_ID: &str = "cz0oehy4mmuoqdropk5m12du03lhxv";
const DEVICE_URL: &str = "https://id.twitch.tv/oauth2/device";
const TOKEN_URL: &str = "https://id.twitch.tv/oauth2/token";
const VALIDATE_URL: &str = "https://id.twitch.tv/oauth2/validate";
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// The authorization attempt to show in the UI. Keep this value for both Open
/// and Copy actions; requesting another session creates a different code.
#[derive(Clone)]
pub struct DeviceSession {
    pub verification_uri: String,
    pub user_code: String,
    pub interval: Duration,
    pub expires_at: Instant,
    device_code: String,
    scopes: String,
}

impl fmt::Debug for DeviceSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceSession")
            .field("verification_uri", &self.verification_uri)
            .field("user_code", &self.user_code)
            .field("interval", &self.interval)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

/// Credentials that have not yet been matched to the intended Twitch account.
pub struct UnacceptedTokens {
    access_token: String,
    refresh_token: String,
    pub expires_in: Duration,
    pub scopes: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ValidatedIdentity {
    pub client_id: String,
    pub user_id: String,
    pub login: String,
    pub scopes: Vec<String>,
    pub expires_in: Duration,
}

impl UnacceptedTokens {
    pub fn access_token(&self) -> &str {
        &self.access_token
    }

    pub fn refresh_token(&self) -> &str {
        &self.refresh_token
    }

    pub fn into_parts(self) -> (String, String, Duration, Vec<String>) {
        (
            self.access_token,
            self.refresh_token,
            self.expires_in,
            self.scopes,
        )
    }
}

impl fmt::Debug for UnacceptedTokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnacceptedTokens")
            .field("expires_in", &self.expires_in)
            .field("scopes", &self.scopes)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DeviceError {
    #[error("Twitch authorization was cancelled")]
    Cancelled,
    #[error("Twitch authorization code expired")]
    Expired,
    #[error("Twitch authorization was denied")]
    Denied,
    #[error("Twitch rejected the device code")]
    InvalidDeviceCode,
    #[error("Twitch rejected the refresh token")]
    InvalidRefreshToken,
    #[error("Twitch rejected the access token")]
    InvalidAccessToken,
    #[error("the Twitch token belongs to a different client or has no user identity")]
    InvalidIdentity,
    #[error("Twitch request failed")]
    Transport,
    #[error("Twitch returned HTTP {0}")]
    Http(u16),
    #[error("Twitch returned an invalid authorization response")]
    InvalidResponse,
    #[error("at least one valid Twitch scope is required")]
    InvalidScopes,
}

/// A deliberately small transport seam for deterministic protocol tests.
pub trait FormTransport {
    fn post_form(
        &self,
        url: &str,
        fields: &[(&str, &str)],
    ) -> impl Future<Output = Result<FormResponse, DeviceError>> + Send;

    fn get_bearer(
        &self,
        url: &str,
        access_token: &str,
    ) -> impl Future<Output = Result<FormResponse, DeviceError>> + Send;
}

pub struct FormResponse {
    pub status: u16,
    pub body: Vec<u8>,
    pub retry_after: Option<Duration>,
}

#[derive(Clone)]
pub struct ReqwestTransport(reqwest::Client);

impl Default for ReqwestTransport {
    fn default() -> Self {
        Self(
            reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("static Twitch HTTP client configuration"),
        )
    }
}

impl FormTransport for ReqwestTransport {
    async fn post_form(
        &self,
        url: &str,
        fields: &[(&str, &str)],
    ) -> Result<FormResponse, DeviceError> {
        let response = self
            .0
            .post(url)
            .form(fields)
            .send()
            .await
            .map_err(|_| DeviceError::Transport)?;
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .map(Duration::from_secs);
        let body = response
            .bytes()
            .await
            .map_err(|_| DeviceError::Transport)?
            .to_vec();
        Ok(FormResponse {
            status,
            body,
            retry_after,
        })
    }

    async fn get_bearer(&self, url: &str, access_token: &str) -> Result<FormResponse, DeviceError> {
        let response = self
            .0
            .get(url)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|_| DeviceError::Transport)?;
        let status = response.status().as_u16();
        let body = response
            .bytes()
            .await
            .map_err(|_| DeviceError::Transport)?
            .to_vec();
        Ok(FormResponse {
            status,
            body,
            retry_after: None,
        })
    }
}

pub struct DeviceAuth<T = ReqwestTransport> {
    transport: T,
}

impl Default for DeviceAuth<ReqwestTransport> {
    fn default() -> Self {
        Self::new(ReqwestTransport::default())
    }
}

impl<T: FormTransport> DeviceAuth<T> {
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    pub async fn begin(
        &self,
        scopes: &[&str],
        cancel: &CancellationToken,
    ) -> Result<DeviceSession, DeviceError> {
        if scopes.is_empty() || scopes.iter().any(|scope| !valid_scope(scope)) {
            return Err(DeviceError::InvalidScopes);
        }
        let scopes = scopes.join(" ");
        let response = self
            .request(
                DEVICE_URL,
                &[("client_id", CLIENT_ID), ("scopes", &scopes)],
                cancel,
            )
            .await?;
        if response.status != 200 {
            return Err(DeviceError::Http(response.status));
        }
        let response: DeviceResponse =
            serde_json::from_slice(&response.body).map_err(|_| DeviceError::InvalidResponse)?;
        if response.device_code.is_empty()
            || response.user_code.is_empty()
            || !response
                .verification_uri
                .starts_with("https://www.twitch.tv/")
            || response.interval == 0
            || response.expires_in == 0
        {
            return Err(DeviceError::InvalidResponse);
        }
        Ok(DeviceSession {
            verification_uri: response.verification_uri,
            user_code: response.user_code,
            interval: Duration::from_secs(response.interval),
            expires_at: Instant::now() + Duration::from_secs(response.expires_in),
            device_code: response.device_code,
            scopes,
        })
    }

    pub async fn poll(
        &self,
        session: &DeviceSession,
        cancel: &CancellationToken,
    ) -> Result<UnacceptedTokens, DeviceError> {
        let mut interval = session.interval;
        loop {
            let remaining = session
                .expires_at
                .checked_duration_since(Instant::now())
                .ok_or(DeviceError::Expired)?;
            if interval >= remaining {
                return self.wait_expiry(remaining, cancel).await;
            }
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(DeviceError::Cancelled),
                _ = tokio::time::sleep(interval) => {}
            }
            if Instant::now() >= session.expires_at {
                return Err(DeviceError::Expired);
            }
            let response = match self
                .request(
                    TOKEN_URL,
                    &[
                        ("client_id", CLIENT_ID),
                        ("scopes", &session.scopes),
                        ("device_code", &session.device_code),
                        ("grant_type", DEVICE_GRANT),
                    ],
                    cancel,
                )
                .await
            {
                Ok(response) => response,
                Err(DeviceError::Transport) => continue,
                Err(error) => return Err(error),
            };
            if Instant::now() >= session.expires_at {
                return Err(DeviceError::Expired);
            }
            if response.status == 200 {
                return parse_tokens(&response.body);
            }
            if response.status == 408 || (500..=599).contains(&response.status) {
                continue;
            }
            let message = error_message(&response.body);
            match (response.status, message.as_deref()) {
                (400, Some("authorization_pending")) => {}
                (400, Some("slow_down")) => interval += Duration::from_secs(5),
                (400, Some("access_denied" | "authorization_declined")) => {
                    return Err(DeviceError::Denied);
                }
                (400, Some("expired_token")) => return Err(DeviceError::Expired),
                (400, Some("invalid device code")) => {
                    return Err(DeviceError::InvalidDeviceCode);
                }
                (429, _) => {
                    interval = interval.max(response.retry_after.unwrap_or(Duration::from_secs(5)));
                }
                _ => return Err(DeviceError::Http(response.status)),
            }
        }
    }

    /// Public-client refresh. The caller must replace the old refresh token
    /// atomically with the returned one; Twitch makes the old token single use.
    pub async fn refresh(
        &self,
        refresh_token: &str,
        cancel: &CancellationToken,
    ) -> Result<UnacceptedTokens, DeviceError> {
        if refresh_token.is_empty() {
            return Err(DeviceError::InvalidRefreshToken);
        }
        let response = self
            .request(
                TOKEN_URL,
                &[
                    ("client_id", CLIENT_ID),
                    ("grant_type", "refresh_token"),
                    ("refresh_token", refresh_token),
                ],
                cancel,
            )
            .await?;
        if response.status == 200 {
            return parse_tokens(&response.body);
        }
        if matches!(response.status, 400 | 401)
            && error_message(&response.body)
                .is_some_and(|message| message.eq_ignore_ascii_case("invalid refresh token"))
        {
            return Err(DeviceError::InvalidRefreshToken);
        }
        Err(DeviceError::Http(response.status))
    }

    /// Resolve the account behind an access token before accepting credentials.
    /// Also call this on startup and at least hourly while using Twitch APIs.
    pub async fn validate(
        &self,
        access_token: &str,
        cancel: &CancellationToken,
    ) -> Result<ValidatedIdentity, DeviceError> {
        if access_token.is_empty() {
            return Err(DeviceError::InvalidAccessToken);
        }
        let response = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(DeviceError::Cancelled),
            result = self.transport.get_bearer(VALIDATE_URL, access_token) => result?,
        };
        if response.status == 401 {
            return Err(DeviceError::InvalidAccessToken);
        }
        if response.status != 200 {
            return Err(DeviceError::Http(response.status));
        }
        let identity: ValidateResponse =
            serde_json::from_slice(&response.body).map_err(|_| DeviceError::InvalidResponse)?;
        let (Some(user_id), Some(login)) = (identity.user_id, identity.login) else {
            return Err(DeviceError::InvalidIdentity);
        };
        if identity.client_id != CLIENT_ID || user_id.is_empty() || login.is_empty() {
            return Err(DeviceError::InvalidIdentity);
        }
        Ok(ValidatedIdentity {
            client_id: identity.client_id,
            user_id,
            login,
            scopes: identity.scopes,
            expires_in: Duration::from_secs(identity.expires_in),
        })
    }

    async fn request(
        &self,
        url: &str,
        fields: &[(&str, &str)],
        cancel: &CancellationToken,
    ) -> Result<FormResponse, DeviceError> {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(DeviceError::Cancelled),
            result = self.transport.post_form(url, fields) => result,
        }
    }

    async fn wait_expiry<R>(
        &self,
        remaining: Duration,
        cancel: &CancellationToken,
    ) -> Result<R, DeviceError> {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(DeviceError::Cancelled),
            _ = tokio::time::sleep(remaining) => Err(DeviceError::Expired),
        }
    }
}

fn valid_scope(scope: &str) -> bool {
    !scope.is_empty() && scope.bytes().all(|byte| byte.is_ascii_graphic())
}

fn error_message(body: &[u8]) -> Option<String> {
    #[derive(Deserialize)]
    struct ErrorBody {
        message: String,
    }
    serde_json::from_slice::<ErrorBody>(body)
        .ok()
        .map(|body| body.message)
}

fn parse_tokens(body: &[u8]) -> Result<UnacceptedTokens, DeviceError> {
    let response: TokenResponse =
        serde_json::from_slice(body).map_err(|_| DeviceError::InvalidResponse)?;
    if response.access_token.is_empty()
        || response.refresh_token.is_empty()
        || response.expires_in == 0
        || !response.token_type.eq_ignore_ascii_case("bearer")
    {
        return Err(DeviceError::InvalidResponse);
    }
    Ok(UnacceptedTokens {
        access_token: response.access_token,
        refresh_token: response.refresh_token,
        expires_in: Duration::from_secs(response.expires_in),
        scopes: response.scope,
    })
}

#[derive(Deserialize)]
struct DeviceResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    interval: u64,
    expires_in: u64,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: u64,
    scope: Vec<String>,
    token_type: String,
}

#[derive(Deserialize)]
struct ValidateResponse {
    client_id: String,
    user_id: Option<String>,
    login: Option<String>,
    scopes: Vec<String>,
    expires_in: u64,
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Clone, Default)]
    struct MockTransport {
        state: Arc<Mutex<MockState>>,
    }

    #[derive(Default)]
    struct MockState {
        responses: VecDeque<FormResponse>,
        requests: Vec<(String, Vec<(String, String)>)>,
    }

    impl MockTransport {
        fn push(&self, status: u16, body: &str) {
            self.push_with_retry_after(status, body, None);
        }

        fn push_with_retry_after(&self, status: u16, body: &str, retry_after: Option<Duration>) {
            self.state
                .lock()
                .unwrap()
                .responses
                .push_back(FormResponse {
                    status,
                    body: body.as_bytes().to_vec(),
                    retry_after,
                });
        }

        fn requests(&self) -> Vec<(String, Vec<(String, String)>)> {
            self.state.lock().unwrap().requests.clone()
        }
    }

    impl FormTransport for MockTransport {
        async fn post_form(
            &self,
            url: &str,
            fields: &[(&str, &str)],
        ) -> Result<FormResponse, DeviceError> {
            let mut state = self.state.lock().unwrap();
            state.requests.push((
                url.into(),
                fields
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            ));
            Ok(state.responses.pop_front().expect("mock response"))
        }

        async fn get_bearer(
            &self,
            url: &str,
            access_token: &str,
        ) -> Result<FormResponse, DeviceError> {
            let mut state = self.state.lock().unwrap();
            state.requests.push((
                url.into(),
                vec![("authorization".into(), access_token.into())],
            ));
            Ok(state.responses.pop_front().expect("mock response"))
        }
    }

    const DEVICE: &str = r#"{"device_code":"secret-code","user_code":"ABCDEFGH","verification_uri":"https://www.twitch.tv/activate?public=true&device-code=ABCDEFGH","interval":1,"expires_in":30}"#;
    const TOKENS: &str = r#"{"access_token":"secret-access","refresh_token":"secret-refresh","expires_in":14400,"scope":["chat:read"],"token_type":"bearer"}"#;
    const VALIDATED: &str = r#"{"client_id":"cz0oehy4mmuoqdropk5m12du03lhxv","user_id":"123","login":"viewer","scopes":["chat:read"],"expires_in":1200}"#;

    #[tokio::test]
    async fn starts_session_with_same_user_code_as_verification_url() {
        let mock = MockTransport::default();
        mock.push(200, DEVICE);
        let session = DeviceAuth::new(mock.clone())
            .begin(&["chat:read", "chat:edit"], &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(session.user_code, "ABCDEFGH");
        assert!(session.verification_uri.contains(&session.user_code));
        assert_eq!(session.interval, Duration::from_secs(1));
        assert!(session.expires_at > Instant::now());
        let requests = mock.requests();
        assert_eq!(requests[0].0, DEVICE_URL);
        assert_eq!(requests[0].1[0], ("client_id".into(), CLIENT_ID.into()));
        assert_eq!(
            requests[0].1[1],
            ("scopes".into(), "chat:read chat:edit".into())
        );
        assert!(!format!("{session:?}").contains("secret-code"));
    }

    #[tokio::test(start_paused = true)]
    async fn polls_pending_and_then_returns_unaccepted_tokens() {
        let mock = MockTransport::default();
        mock.push(200, DEVICE);
        mock.push(400, r#"{"status":400,"message":"authorization_pending"}"#);
        mock.push(200, TOKENS);
        let auth = DeviceAuth::new(mock.clone());
        let session = auth
            .begin(&["chat:read"], &CancellationToken::new())
            .await
            .unwrap();
        let cancel = CancellationToken::new();
        let poll = tokio::spawn(async move { auth.poll(&session, &cancel).await });
        tokio::task::yield_now().await;
        assert_eq!(mock.requests().len(), 1);
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(mock.requests().len(), 2);
        tokio::time::advance(Duration::from_secs(1)).await;
        let tokens = poll.await.unwrap().unwrap();
        assert_eq!(tokens.access_token(), "secret-access");
        assert_eq!(tokens.refresh_token(), "secret-refresh");
        assert!(!format!("{tokens:?}").contains("secret-"));
        let requests = mock.requests();
        assert_eq!(requests[2].0, TOKEN_URL);
        assert!(
            requests[2]
                .1
                .contains(&("device_code".into(), "secret-code".into()))
        );
        assert!(
            requests[2]
                .1
                .contains(&("grant_type".into(), DEVICE_GRANT.into()))
        );
        assert!(!requests[2].1.iter().any(|(key, _)| key == "client_secret"));
    }

    #[tokio::test(start_paused = true)]
    async fn slows_polling_after_server_throttling() {
        let mock = MockTransport::default();
        mock.push(200, DEVICE);
        mock.push(400, r#"{"status":400,"message":"slow_down"}"#);
        mock.push(200, TOKENS);
        let auth = DeviceAuth::new(mock.clone());
        let session = auth
            .begin(&["chat:read"], &CancellationToken::new())
            .await
            .unwrap();
        let cancel = CancellationToken::new();
        let poll = tokio::spawn(async move { auth.poll(&session, &cancel).await });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(mock.requests().len(), 2);
        tokio::time::advance(Duration::from_secs(5)).await;
        tokio::task::yield_now().await;
        assert_eq!(mock.requests().len(), 2);
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(poll.await.unwrap().unwrap().access_token(), "secret-access");
    }

    #[tokio::test(start_paused = true)]
    async fn transient_poll_error_reuses_the_same_device_session() {
        let mock = MockTransport::default();
        mock.push(200, DEVICE);
        mock.push(503, "temporary outage");
        mock.push(200, TOKENS);
        let auth = DeviceAuth::new(mock.clone());
        let session = auth
            .begin(&["chat:read"], &CancellationToken::new())
            .await
            .unwrap();
        let cancel = CancellationToken::new();
        let poll = tokio::spawn(async move { auth.poll(&session, &cancel).await });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(poll.await.unwrap().unwrap().access_token(), "secret-access");
        let requests = mock.requests();
        assert_eq!(
            requests.iter().filter(|(url, _)| url == DEVICE_URL).count(),
            1
        );
        assert_eq!(
            requests.iter().filter(|(url, _)| url == TOKEN_URL).count(),
            2
        );
        assert_eq!(requests[1].1, requests[2].1);
    }

    #[tokio::test(start_paused = true)]
    async fn rate_limit_observes_retry_after() {
        let mock = MockTransport::default();
        mock.push(200, DEVICE);
        mock.push_with_retry_after(429, "", Some(Duration::from_secs(10)));
        mock.push(200, TOKENS);
        let auth = DeviceAuth::new(mock.clone());
        let session = auth
            .begin(&["chat:read"], &CancellationToken::new())
            .await
            .unwrap();
        let cancel = CancellationToken::new();
        let poll = tokio::spawn(async move { auth.poll(&session, &cancel).await });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(9)).await;
        tokio::task::yield_now().await;
        assert_eq!(mock.requests().len(), 2);
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(poll.await.unwrap().unwrap().access_token(), "secret-access");
    }

    #[tokio::test(start_paused = true)]
    async fn denial_stops_polling() {
        let mock = MockTransport::default();
        mock.push(200, DEVICE);
        mock.push(400, r#"{"status":400,"message":"access_denied"}"#);
        let auth = DeviceAuth::new(mock.clone());
        let session = auth
            .begin(&["chat:read"], &CancellationToken::new())
            .await
            .unwrap();
        let cancel = CancellationToken::new();
        let poll = tokio::spawn(async move { auth.poll(&session, &cancel).await });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(poll.await.unwrap().err(), Some(DeviceError::Denied));
        assert_eq!(mock.requests().len(), 2);
    }

    #[tokio::test]
    async fn cancellation_and_expiry_never_send_a_poll_request() {
        let mock = MockTransport::default();
        mock.push(200, DEVICE);
        let auth = DeviceAuth::new(mock.clone());
        let mut session = auth
            .begin(&["chat:read"], &CancellationToken::new())
            .await
            .unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert_eq!(
            auth.poll(&session, &cancel).await.err(),
            Some(DeviceError::Cancelled)
        );
        session.expires_at = Instant::now() - Duration::from_secs(1);
        assert_eq!(
            auth.poll(&session, &CancellationToken::new()).await.err(),
            Some(DeviceError::Expired)
        );
        assert_eq!(mock.requests().len(), 1);
    }

    #[tokio::test]
    async fn refresh_is_public_client_and_returns_rotated_credentials() {
        let mock = MockTransport::default();
        mock.push(200, TOKENS);
        let tokens = DeviceAuth::new(mock.clone())
            .refresh("old+refresh/token", &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(tokens.refresh_token(), "secret-refresh");
        let requests = mock.requests();
        assert_eq!(requests[0].0, TOKEN_URL);
        assert!(
            requests[0]
                .1
                .contains(&("refresh_token".into(), "old+refresh/token".into()))
        );
        assert!(!requests[0].1.iter().any(|(key, _)| key == "client_secret"));
    }

    #[tokio::test]
    async fn validation_requires_correct_client_and_user_identity() {
        let mock = MockTransport::default();
        mock.push(200, VALIDATED);
        mock.push(200, &VALIDATED.replace(CLIENT_ID, "other-client"));
        mock.push(
            200,
            &VALIDATED.replace("\"user_id\":\"123\"", "\"user_id\":null"),
        );
        let auth = DeviceAuth::new(mock.clone());
        let cancel = CancellationToken::new();
        let identity = auth.validate("access", &cancel).await.unwrap();
        assert_eq!(identity.user_id, "123");
        assert_eq!(identity.login, "viewer");
        assert_eq!(
            auth.validate("access", &cancel).await,
            Err(DeviceError::InvalidIdentity)
        );
        assert_eq!(
            auth.validate("access", &cancel).await,
            Err(DeviceError::InvalidIdentity)
        );
        assert_eq!(mock.requests()[0].0, VALIDATE_URL);
    }

    #[tokio::test]
    async fn invalid_responses_are_rejected_without_exposing_response_body() {
        let mock = MockTransport::default();
        mock.push(503, "secret-body");
        let result = DeviceAuth::new(mock)
            .begin(&["chat:read"], &CancellationToken::new())
            .await;
        let error = result.unwrap_err();
        assert_eq!(error, DeviceError::Http(503));
        assert!(!format!("{error:?}").contains("secret-body"));
    }
}
