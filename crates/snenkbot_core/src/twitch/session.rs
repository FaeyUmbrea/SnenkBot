//! Validated Twitch access for channel and bot operations.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;
use tokio::sync::{Mutex, watch};
use tokio_util::sync::CancellationToken;

use crate::integration::StatusCell;

use super::accounts::{AccountError, AccountIdentity, TwitchAccounts};
use super::credentials::{CredentialBackend, OAuthTokens, TwitchRole};
use super::device::{DeviceAuth, DeviceError, FormTransport, ValidatedIdentity};

const REFRESH_BEFORE: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TwitchCapability {
    ChannelOwner,
    BotFeature,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TwitchAuthorizationHealth {
    Unconfigured,
    Checking,
    Connected,
    Error(String),
}

#[derive(Clone, PartialEq)]
struct RoleHealth {
    identity: Option<AccountIdentity>,
    state: TwitchAuthorizationHealth,
}

impl Default for RoleHealth {
    fn default() -> Self {
        Self {
            identity: None,
            state: TwitchAuthorizationHealth::Unconfigured,
        }
    }
}

/// The access token is deliberately omitted from debug output.
pub struct Authorized {
    role: TwitchRole,
    user_id: String,
    access_token: String,
}

impl Authorized {
    pub fn role(&self) -> TwitchRole {
        self.role
    }

    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    pub fn access_token(&self) -> &str {
        &self.access_token
    }
}

impl fmt::Debug for Authorized {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Authorized")
            .field("role", &self.role)
            .field("user_id", &self.user_id)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error(transparent)]
    Account(#[from] AccountError),
    #[error(transparent)]
    Device(#[from] DeviceError),
    #[error("the connected Twitch account changed during authorization")]
    AccountChanged,
    #[error("the Twitch token belongs to a different account")]
    IdentityMismatch,
    #[error("the Twitch token no longer has the account's required permissions")]
    MissingScopes,
    #[error("the Twitch authorization worker stopped")]
    WorkerStopped,
}

/// Coordinates refresh-token rotation for both account roles. Keep this in an
/// `Arc`; each authorization runs to completion after its caller is dropped so
/// that a consumed refresh token can still be saved.
pub struct TwitchSession<B, T> {
    accounts: Arc<TwitchAccounts<B>>,
    auth: DeviceAuth<T>,
    broadcaster: Mutex<()>,
    bot: Mutex<()>,
    broadcaster_health: StatusCell<RoleHealth>,
    bot_health: StatusCell<RoleHealth>,
}

impl<B, T> TwitchSession<B, T>
where
    B: CredentialBackend + 'static,
    T: FormTransport + Send + Sync + 'static,
{
    pub fn new(accounts: Arc<TwitchAccounts<B>>, auth: DeviceAuth<T>) -> Self {
        Self {
            accounts,
            auth,
            broadcaster: Mutex::new(()),
            bot: Mutex::new(()),
            broadcaster_health: StatusCell::new(RoleHealth::default()),
            bot_health: StatusCell::new(RoleHealth::default()),
        }
    }

    pub fn authorization_health(&self, role: TwitchRole) -> TwitchAuthorizationHealth {
        let connected = match self.accounts.configured_accounts() {
            Ok(connected) => connected,
            Err(error) => return TwitchAuthorizationHealth::Error(error.to_string()),
        };
        let Some(identity) = connected.get(role) else {
            return TwitchAuthorizationHealth::Unconfigured;
        };
        let health = self
            .role_health(role)
            .read()
            .expect("Twitch health lock poisoned");
        if health.identity.as_ref() == Some(identity) {
            health.state.clone()
        } else {
            TwitchAuthorizationHealth::Checking
        }
    }

    pub(crate) fn subscribe_health_changes(&self) -> Vec<watch::Receiver<u64>> {
        vec![
            self.broadcaster_health.subscribe(),
            self.bot_health.subscribe(),
        ]
    }

    fn role_health(&self, role: TwitchRole) -> &StatusCell<RoleHealth> {
        match role {
            TwitchRole::Broadcaster => &self.broadcaster_health,
            TwitchRole::Bot => &self.bot_health,
        }
    }

    fn set_role_health(
        &self,
        role: TwitchRole,
        identity: Option<AccountIdentity>,
        state: TwitchAuthorizationHealth,
    ) {
        self.role_health(role)
            .set(RoleHealth { identity, state })
            .expect("Twitch health lock poisoned");
    }

    pub async fn authorize(
        self: &Arc<Self>,
        capability: TwitchCapability,
    ) -> Result<Authorized, SessionError> {
        let session = Arc::clone(self);
        tokio::spawn(async move { session.authorize_inner(capability).await })
            .await
            .map_err(|_| SessionError::WorkerStopped)?
    }

    async fn authorize_inner(
        &self,
        capability: TwitchCapability,
    ) -> Result<Authorized, SessionError> {
        let role = match capability {
            TwitchCapability::ChannelOwner => TwitchRole::Broadcaster,
            TwitchCapability::BotFeature => {
                if self.connected().await?.bot.is_some() {
                    TwitchRole::Bot
                } else {
                    TwitchRole::Broadcaster
                }
            }
        };
        let _guard = match role {
            TwitchRole::Broadcaster => self.broadcaster.lock().await,
            TwitchRole::Bot => self.bot.lock().await,
        };

        let identity = self.connected().await?.get(role).cloned();
        self.set_role_health(role, identity.clone(), TwitchAuthorizationHealth::Checking);
        let result = self.authorize_selected(capability, role).await;
        let health = match &result {
            Ok(_) => TwitchAuthorizationHealth::Connected,
            Err(error) => TwitchAuthorizationHealth::Error(error.to_string()),
        };
        self.set_role_health(role, identity, health);
        result
    }

    async fn authorize_selected(
        &self,
        capability: TwitchCapability,
        role: TwitchRole,
    ) -> Result<Authorized, SessionError> {
        let (expected, tokens) = self.load(role).await?;
        let cancel = CancellationToken::new();
        match self.auth.validate(tokens.access_token(), &cancel).await {
            Ok(identity) => {
                check_identity(&expected, &identity)?;
                if identity.expires_in > REFRESH_BEFORE {
                    if !self.is_current(role, expected.clone()).await? {
                        return Err(SessionError::AccountChanged);
                    }
                    self.ensure_routing_current(capability, role).await?;
                    return Ok(Authorized {
                        role,
                        user_id: expected.user_id,
                        access_token: tokens.access_token().to_owned(),
                    });
                }
            }
            Err(DeviceError::InvalidAccessToken) => {}
            Err(error) => return Err(error.into()),
        }

        // A valid token near expiry and a rejected token both need a refresh.
        // Other validation failures above must not rotate credentials.
        let refreshed = self.auth.refresh(tokens.refresh_token(), &cancel).await?;
        let (access, refresh, _, _) = refreshed.into_parts();
        let pair = OAuthTokens::new(access.clone(), refresh);
        if !self.replace_refreshed(role, expected.clone(), pair).await? {
            return Err(SessionError::AccountChanged);
        }
        // Twitch has consumed the old refresh token. Save its replacement
        // first, so a transient validation failure can be retried later.
        let identity = self.auth.validate(&access, &cancel).await?;
        check_identity(&expected, &identity)?;
        if !self.is_current(role, expected.clone()).await? {
            return Err(SessionError::AccountChanged);
        }
        self.ensure_routing_current(capability, role).await?;
        Ok(Authorized {
            role,
            user_id: expected.user_id,
            access_token: access,
        })
    }

    async fn connected(&self) -> Result<super::accounts::ConnectedAccounts, SessionError> {
        let accounts = Arc::clone(&self.accounts);
        tokio::task::spawn_blocking(move || accounts.connected())
            .await
            .map_err(|_| SessionError::WorkerStopped)?
            .map_err(Into::into)
    }

    async fn ensure_routing_current(
        &self,
        capability: TwitchCapability,
        role: TwitchRole,
    ) -> Result<(), SessionError> {
        if capability == TwitchCapability::BotFeature
            && role == TwitchRole::Broadcaster
            && self.connected().await?.bot.is_some()
        {
            return Err(SessionError::AccountChanged);
        }
        Ok(())
    }

    async fn load(&self, role: TwitchRole) -> Result<(AccountIdentity, OAuthTokens), SessionError> {
        let accounts = Arc::clone(&self.accounts);
        tokio::task::spawn_blocking(move || accounts.load_for_authorization(role))
            .await
            .map_err(|_| SessionError::WorkerStopped)?
            .map_err(Into::into)
    }

    async fn is_current(
        &self,
        role: TwitchRole,
        expected: AccountIdentity,
    ) -> Result<bool, SessionError> {
        let accounts = Arc::clone(&self.accounts);
        tokio::task::spawn_blocking(move || accounts.is_current(role, &expected))
            .await
            .map_err(|_| SessionError::WorkerStopped)?
            .map_err(Into::into)
    }

    async fn replace_refreshed(
        &self,
        role: TwitchRole,
        expected: AccountIdentity,
        tokens: OAuthTokens,
    ) -> Result<bool, SessionError> {
        let accounts = Arc::clone(&self.accounts);
        tokio::task::spawn_blocking(move || accounts.replace_refreshed(role, &expected, &tokens))
            .await
            .map_err(|_| SessionError::WorkerStopped)?
            .map_err(Into::into)
    }
}

fn check_identity(
    expected: &AccountIdentity,
    actual: &ValidatedIdentity,
) -> Result<(), SessionError> {
    if expected.user_id != actual.user_id {
        return Err(SessionError::IdentityMismatch);
    }
    if !expected
        .scopes
        .iter()
        .all(|scope| actual.scopes.iter().any(|actual| actual == scope))
    {
        return Err(SessionError::MissingScopes);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use serde_json::json;

    use super::*;
    use crate::migration::StoredConfig;
    use crate::storage::ConfigStore;
    use crate::twitch::credentials::{CredentialError, TwitchCredentialStore};
    use crate::twitch::device::FormResponse;

    const CLIENT_ID: &str = "cz0oehy4mmuoqdropk5m12du03lhxv";
    const BROADCASTER_SLOT: &str = "b63e12df-d389-46e5-892c-779036286c8e";
    const BOT_SLOT: &str = "d304c860-d5d1-4a6b-9768-7519cb83de83";

    #[derive(Default)]
    struct MemoryCredentials {
        values: StdMutex<HashMap<String, String>>,
        reject_writes: AtomicBool,
        reject_reads: AtomicBool,
        reads: AtomicUsize,
    }

    impl CredentialBackend for Arc<MemoryCredentials> {
        fn get(&self, _service: &str, account: &str) -> Result<Option<String>, CredentialError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            if self.reject_reads.load(Ordering::SeqCst) {
                return Err(CredentialError::Backend);
            }
            Ok(self.values.lock().unwrap().get(account).cloned())
        }

        fn set(&self, _service: &str, account: &str, value: &str) -> Result<(), CredentialError> {
            if self.reject_writes.load(Ordering::SeqCst) {
                return Err(CredentialError::Backend);
            }
            self.values
                .lock()
                .unwrap()
                .insert(account.to_owned(), value.to_owned());
            Ok(())
        }

        fn delete(&self, _service: &str, account: &str) -> Result<(), CredentialError> {
            self.values.lock().unwrap().remove(account);
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeTransport {
        refreshes: AtomicUsize,
        wrong_refreshed_identity: AtomicBool,
        missing_scopes: AtomicBool,
    }

    impl FormTransport for Arc<FakeTransport> {
        async fn post_form(
            &self,
            _url: &str,
            fields: &[(&str, &str)],
        ) -> Result<FormResponse, DeviceError> {
            let refresh = fields
                .iter()
                .find(|(name, _)| *name == "refresh_token")
                .map(|(_, value)| *value)
                .ok_or(DeviceError::InvalidResponse)?;
            self.refreshes.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(50)).await;
            let name = if refresh == "broadcaster-refresh" {
                "broadcaster"
            } else {
                "bot"
            };
            Ok(FormResponse {
                status: 200,
                body: json!({
                    "access_token": format!("{name}-new"),
                    "refresh_token": format!("{name}-refresh-new"),
                    "expires_in": 3600,
                    "scope": ["user:write:chat"],
                    "token_type": "bearer"
                })
                .to_string()
                .into_bytes(),
                retry_after: None,
            })
        }

        async fn get_bearer(
            &self,
            _url: &str,
            access_token: &str,
        ) -> Result<FormResponse, DeviceError> {
            let is_bot = access_token.starts_with("bot");
            let is_new = access_token.ends_with("-new");
            let user_id = if is_new && self.wrong_refreshed_identity.load(Ordering::SeqCst) {
                "999"
            } else if is_bot {
                "456"
            } else {
                "123"
            };
            let scopes = if self.missing_scopes.load(Ordering::SeqCst) || is_bot {
                vec!["user:write:chat"]
            } else {
                vec![
                    "channel:manage:broadcast",
                    "user:read:chat",
                    "user:write:chat",
                ]
            };
            Ok(FormResponse {
                status: 200,
                body: json!({
                    "client_id": CLIENT_ID,
                    "user_id": user_id,
                    "login": if is_bot {"bot"} else {"streamer"},
                    "scopes": scopes,
                    "expires_in": if is_new || is_bot {3600} else {60}
                })
                .to_string()
                .into_bytes(),
                retry_after: None,
            })
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        accounts: Arc<TwitchAccounts<Arc<MemoryCredentials>>>,
        credentials: Arc<MemoryCredentials>,
        transport: Arc<FakeTransport>,
    }

    impl Fixture {
        fn new(with_bot: bool) -> Self {
            Self::with_credentials(with_bot, true, false)
        }

        fn with_credentials(with_bot: bool, bot_credential: bool, reject_reads: bool) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let credentials = Arc::new(MemoryCredentials::default());
            let credential_store = TwitchCredentialStore::new(Arc::clone(&credentials), CLIENT_ID);
            credential_store
                .replace(
                    TwitchRole::Broadcaster,
                    BROADCASTER_SLOT,
                    &OAuthTokens::new("broadcaster-old".into(), "broadcaster-refresh".into()),
                )
                .unwrap();
            if with_bot && bot_credential {
                credential_store
                    .replace(
                        TwitchRole::Bot,
                        BOT_SLOT,
                        &OAuthTokens::new("bot-old".into(), "bot-refresh".into()),
                    )
                    .unwrap();
            }
            let config = StoredConfig {
                definition: "snenkbot.twitch.accounts".into(),
                version: 1,
                data: json!({
                    "broadcaster": {
                        "user_id": "123",
                        "login": "streamer",
                        "scopes": [
                            "channel:manage:broadcast",
                            "user:read:chat",
                            "user:write:chat"
                        ],
                        "credential_slot": BROADCASTER_SLOT
                    },
                    "bot": with_bot.then(|| json!({
                        "user_id": "456",
                        "login": "bot",
                        "scopes": ["user:write:chat"],
                        "credential_slot": BOT_SLOT
                    }))
                }),
            };
            let store = ConfigStore::new(dir.path());
            store
                .create("twitch-accounts", &config, |_| Ok(()))
                .unwrap();
            credentials
                .reject_reads
                .store(reject_reads, Ordering::SeqCst);
            let accounts = Arc::new(TwitchAccounts::new(store, credential_store));
            let transport = Arc::new(FakeTransport::default());
            Self {
                _dir: dir,
                accounts,
                credentials,
                transport,
            }
        }

        fn session(&self) -> Arc<TwitchSession<Arc<MemoryCredentials>, Arc<FakeTransport>>> {
            Arc::new(TwitchSession::new(
                Arc::clone(&self.accounts),
                DeviceAuth::new(Arc::clone(&self.transport)),
            ))
        }
    }

    #[tokio::test]
    async fn action_authorization_updates_are_observable_without_credential_reads() {
        let fixture = Fixture::new(true);
        let session = fixture.session();
        let mut changes = session.subscribe_health_changes();
        let startup_reads = fixture.credentials.reads.load(Ordering::SeqCst);
        session
            .authorize(TwitchCapability::BotFeature)
            .await
            .unwrap();
        assert!(!changes[0].has_changed().unwrap());
        assert!(changes[1].has_changed().unwrap());
        changes[1].borrow_and_update();
        assert_eq!(
            session.authorization_health(TwitchRole::Bot),
            TwitchAuthorizationHealth::Connected
        );
        assert_eq!(
            fixture.credentials.reads.load(Ordering::SeqCst),
            startup_reads
        );
        let identity = fixture.accounts.configured_accounts().unwrap().bot;
        session.set_role_health(
            TwitchRole::Bot,
            identity,
            TwitchAuthorizationHealth::Connected,
        );
        assert!(!changes[1].has_changed().unwrap());
        fixture.accounts.disconnect(TwitchRole::Bot).unwrap();
        assert_eq!(
            session.authorization_health(TwitchRole::Bot),
            TwitchAuthorizationHealth::Unconfigured
        );
    }

    #[tokio::test]
    async fn bot_feature_uses_configured_bot_or_broadcaster_fallback() {
        let fixture = Fixture::new(true);
        let session = fixture.session();
        let bot = session
            .authorize(TwitchCapability::BotFeature)
            .await
            .unwrap();
        assert_eq!(bot.role(), TwitchRole::Bot);
        assert_eq!(bot.user_id(), "456");
        assert_eq!(bot.access_token(), "bot-old");
        assert!(!format!("{bot:?}").contains("bot-old"));
        let owner = session
            .authorize(TwitchCapability::ChannelOwner)
            .await
            .unwrap();
        assert_eq!(owner.role(), TwitchRole::Broadcaster);
        assert_eq!(owner.access_token(), "broadcaster-new");

        let fixture = Fixture::new(false);
        let fallback = fixture
            .session()
            .authorize(TwitchCapability::BotFeature)
            .await
            .unwrap();
        assert_eq!(fallback.role(), TwitchRole::Broadcaster);
    }

    #[tokio::test]
    async fn configured_bot_with_missing_credentials_does_not_fall_back() {
        let fixture = Fixture::with_credentials(true, false, false);
        let result = fixture
            .session()
            .authorize(TwitchCapability::BotFeature)
            .await;
        assert!(matches!(
            result,
            Err(SessionError::Account(AccountError::MissingCredentials))
        ));
        assert_eq!(fixture.transport.refreshes.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.credentials.reads.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn concurrent_authorization_uses_only_startup_credential_reads() {
        let fixture = Fixture::new(true);
        assert_eq!(fixture.credentials.reads.load(Ordering::SeqCst), 2);
        let session = fixture.session();
        assert_eq!(
            session.authorization_health(TwitchRole::Broadcaster),
            TwitchAuthorizationHealth::Checking
        );
        assert_eq!(
            session.authorization_health(TwitchRole::Bot),
            TwitchAuthorizationHealth::Checking
        );
        let (owner, bot) = tokio::join!(
            session.authorize(TwitchCapability::ChannelOwner),
            session.authorize(TwitchCapability::BotFeature)
        );
        assert_eq!(owner.unwrap().role(), TwitchRole::Broadcaster);
        assert_eq!(bot.unwrap().role(), TwitchRole::Bot);
        assert_eq!(fixture.credentials.reads.load(Ordering::SeqCst), 2);
        assert_eq!(
            session.authorization_health(TwitchRole::Broadcaster),
            TwitchAuthorizationHealth::Connected
        );
        assert_eq!(
            session.authorization_health(TwitchRole::Bot),
            TwitchAuthorizationHealth::Connected
        );
        fixture.accounts.disconnect(TwitchRole::Bot).unwrap();
        assert_eq!(
            session.authorization_health(TwitchRole::Bot),
            TwitchAuthorizationHealth::Unconfigured
        );
    }

    #[tokio::test]
    async fn failed_startup_read_is_cached_until_explicit_write() {
        let fixture = Fixture::with_credentials(false, false, true);
        assert_eq!(fixture.credentials.reads.load(Ordering::SeqCst), 1);
        fixture
            .credentials
            .reject_reads
            .store(false, Ordering::SeqCst);
        let session = fixture.session();
        for _ in 0..2 {
            assert!(matches!(
                session.authorize(TwitchCapability::ChannelOwner).await,
                Err(SessionError::Account(AccountError::Credentials(
                    CredentialError::Backend
                )))
            ));
        }
        assert_eq!(fixture.credentials.reads.load(Ordering::SeqCst), 1);
        assert_eq!(
            session.authorization_health(TwitchRole::Broadcaster),
            TwitchAuthorizationHealth::Error(CredentialError::Backend.to_string())
        );
    }

    #[tokio::test]
    async fn simultaneous_requests_rotate_refresh_token_once() {
        let fixture = Fixture::new(false);
        let session = fixture.session();
        let (first, second) = tokio::join!(
            session.authorize(TwitchCapability::ChannelOwner),
            session.authorize(TwitchCapability::ChannelOwner)
        );
        assert_eq!(first.unwrap().access_token(), "broadcaster-new");
        assert_eq!(second.unwrap().access_token(), "broadcaster-new");
        assert_eq!(fixture.transport.refreshes.load(Ordering::SeqCst), 1);
        assert_eq!(
            fixture
                .accounts
                .load_for_authorization(TwitchRole::Broadcaster)
                .unwrap()
                .1
                .refresh_token(),
            "broadcaster-refresh-new"
        );
    }

    #[tokio::test]
    async fn dropped_caller_does_not_interrupt_refresh_persistence() {
        let fixture = Fixture::new(false);
        let session = fixture.session();
        let caller =
            tokio::spawn(async move { session.authorize(TwitchCapability::ChannelOwner).await });
        for _ in 0..100 {
            if fixture.transport.refreshes.load(Ordering::SeqCst) == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert_eq!(fixture.transport.refreshes.load(Ordering::SeqCst), 1);
        caller.abort();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            fixture
                .accounts
                .load_for_authorization(TwitchRole::Broadcaster)
                .unwrap()
                .1
                .refresh_token(),
            "broadcaster-refresh-new"
        );
    }

    #[tokio::test]
    async fn failed_persistence_never_exposes_refreshed_token() {
        let fixture = Fixture::new(false);
        fixture
            .credentials
            .reject_writes
            .store(true, Ordering::SeqCst);
        let result = fixture
            .session()
            .authorize(TwitchCapability::ChannelOwner)
            .await;
        assert!(matches!(
            result,
            Err(SessionError::Account(AccountError::Credentials(
                CredentialError::Backend
            )))
        ));
        assert_eq!(
            fixture
                .accounts
                .load_for_authorization(TwitchRole::Broadcaster)
                .unwrap()
                .1
                .refresh_token(),
            "broadcaster-refresh"
        );
    }

    #[tokio::test]
    async fn refreshed_token_for_another_user_is_rejected() {
        let fixture = Fixture::new(false);
        fixture
            .transport
            .wrong_refreshed_identity
            .store(true, Ordering::SeqCst);
        let result = fixture
            .session()
            .authorize(TwitchCapability::ChannelOwner)
            .await;
        assert!(matches!(result, Err(SessionError::IdentityMismatch)));
        assert_eq!(
            fixture
                .accounts
                .load_for_authorization(TwitchRole::Broadcaster)
                .unwrap()
                .1
                .refresh_token(),
            "broadcaster-refresh-new"
        );
    }

    #[tokio::test]
    async fn missing_permissions_fail_without_rotating_credentials() {
        let fixture = Fixture::new(false);
        fixture
            .transport
            .missing_scopes
            .store(true, Ordering::SeqCst);
        let result = fixture
            .session()
            .authorize(TwitchCapability::ChannelOwner)
            .await;
        assert!(matches!(result, Err(SessionError::MissingScopes)));
        assert_eq!(fixture.transport.refreshes.load(Ordering::SeqCst), 0);
    }
}
