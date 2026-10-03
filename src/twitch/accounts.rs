use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::integration::StatusCell;
use crate::migration::StoredConfig;
use crate::storage::{ConfigSnapshot, ConfigStore, SaveWarning, StoreError};

use super::credentials::{
    CredentialBackend, CredentialError, OAuthTokens, TwitchCredentialStore, TwitchRole,
};
use super::device::{DeviceAuth, DeviceError, FormTransport, UnacceptedTokens, ValidatedIdentity};

const DOCUMENT_ID: &str = "twitch-accounts";
const DEFINITION: &str = "snenkbot.twitch.accounts";
const VERSION: u32 = 1;

const BROADCASTER_SCOPES: &[&str] = &[
    "channel:manage:broadcast",
    "channel:read:ads",
    "user:read:chat",
    "user:write:chat",
];
const BASE_BROADCASTER_SCOPES: &[&str] = &[
    "channel:manage:broadcast",
    "user:read:chat",
    "user:write:chat",
];
const BOT_SCOPES: &[&str] = &["user:write:chat"];

pub fn scopes_for(role: TwitchRole) -> &'static [&'static str] {
    match role {
        TwitchRole::Broadcaster => BROADCASTER_SCOPES,
        TwitchRole::Bot => BOT_SCOPES,
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountIdentity {
    pub user_id: String,
    pub login: String,
    pub scopes: Vec<String>,
    credential_slot: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectedAccounts {
    pub broadcaster: Option<AccountIdentity>,
    pub bot: Option<AccountIdentity>,
}

impl ConnectedAccounts {
    pub fn get(&self, role: TwitchRole) -> Option<&AccountIdentity> {
        match role {
            TwitchRole::Broadcaster => self.broadcaster.as_ref(),
            TwitchRole::Bot => self.bot.as_ref(),
        }
    }

    fn set(&mut self, role: TwitchRole, account: Option<AccountIdentity>) {
        match role {
            TwitchRole::Broadcaster => self.broadcaster = account,
            TwitchRole::Bot => self.bot = account,
        }
    }
}

/// The token pair stays private until the reviewed Twitch identity is accepted.
pub struct PendingAccount {
    role: TwitchRole,
    identity: ValidatedIdentity,
    tokens: UnacceptedTokens,
}

impl PendingAccount {
    pub fn role(&self) -> TwitchRole {
        self.role
    }

    pub fn user_id(&self) -> &str {
        &self.identity.user_id
    }

    pub fn login(&self) -> &str {
        &self.identity.login
    }
}

#[derive(Debug, Error)]
pub enum AccountError {
    #[error(transparent)]
    Device(#[from] DeviceError),
    #[error(transparent)]
    Credentials(#[from] CredentialError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("Twitch account metadata has an unsupported or invalid format")]
    InvalidMetadata,
    #[error("the approved Twitch account does not match the signed-in account")]
    IdentityMismatch,
    #[error("the Twitch account is missing required permissions")]
    MissingScopes,
    #[error("no Twitch account is connected for this role")]
    NotConnected,
    #[error("the credential manager has no tokens for the connected account")]
    MissingCredentials,
}

#[derive(Debug, Default)]
pub struct AccountChange {
    pub warnings: Vec<SaveWarning>,
    pub orphaned_credential: bool,
}

/// Reviews the Twitch identity without writing a credential or account file.
pub async fn review_account<T: FormTransport>(
    auth: &DeviceAuth<T>,
    role: TwitchRole,
    tokens: UnacceptedTokens,
    cancel: &CancellationToken,
) -> Result<PendingAccount, AccountError> {
    let identity = auth.validate(tokens.access_token(), cancel).await?;
    if !scopes_for(role)
        .iter()
        .all(|required| identity.scopes.iter().any(|actual| actual == required))
    {
        return Err(AccountError::MissingScopes);
    }
    Ok(PendingAccount {
        role,
        identity,
        tokens,
    })
}

type CachedTokens = HashMap<(TwitchRole, String), Result<Option<OAuthTokens>, CredentialError>>;

/// Stores only account identity and a credential-slot reference in JSON.
/// Call these synchronous operations from a worker, never the Slint UI thread.
pub struct TwitchAccounts<B = super::credentials::SystemCredentialBackend> {
    store: ConfigStore,
    credentials: TwitchCredentialStore<B>,
    lock: Mutex<()>,
    cached_tokens: Mutex<CachedTokens>,
    configured: StatusCell<Result<ConnectedAccounts, String>>,
}

impl<B: CredentialBackend> TwitchAccounts<B> {
    pub fn new(store: ConfigStore, credentials: TwitchCredentialStore<B>) -> Self {
        let accounts = Self {
            store,
            credentials,
            lock: Mutex::new(()),
            cached_tokens: Mutex::new(HashMap::new()),
            configured: StatusCell::new(Ok(ConnectedAccounts::default())),
        };
        // Read each referenced credential once during startup, including failures.
        // Authorization must never prompt the system credential manager later.
        match accounts.load() {
            Ok((connected, _)) => {
                for role in [TwitchRole::Broadcaster, TwitchRole::Bot] {
                    if let Some(account) = connected.get(role) {
                        let loaded = accounts.credentials.load(role, &account.credential_slot);
                        if let Err(error) = &loaded {
                            tracing::warn!(?role, %error, "Twitch credential unavailable at startup");
                        } else if matches!(&loaded, Ok(None)) {
                            tracing::warn!(?role, "Twitch credential missing at startup");
                        }
                        accounts
                            .cached_tokens
                            .lock()
                            .expect("Twitch token cache lock poisoned")
                            .insert((role, account.credential_slot.clone()), loaded);
                    }
                }
                accounts
                    .configured
                    .set(Ok(connected))
                    .expect("Twitch account status lock poisoned");
            }
            Err(error) => {
                tracing::warn!(%error, "Twitch accounts unavailable at startup");
                accounts
                    .configured
                    .set(Err(error.to_string()))
                    .expect("Twitch account status lock poisoned");
            }
        }
        accounts
    }

    pub fn connected(&self) -> Result<ConnectedAccounts, AccountError> {
        let _guard = self.lock.lock().expect("account lock poisoned");
        Ok(self.load()?.0)
    }

    /// The last committed identities, without reading disk or credentials.
    pub fn configured_accounts(&self) -> Result<ConnectedAccounts, String> {
        self.configured
            .read()
            .expect("Twitch account status lock poisoned")
            .clone()
    }

    pub(crate) fn subscribe_configuration_changes(&self) -> watch::Receiver<u64> {
        self.configured.subscribe()
    }

    /// Reports whether the configured role has a usable cached token pair.
    /// This does not validate the token with Twitch.
    pub fn credential_status(&self, role: TwitchRole) -> Result<(), AccountError> {
        self.load_for_authorization(role).map(|_| ())
    }

    /// Publishes a newly reviewed account after the user approves its exact user ID.
    /// A failed file save leaves the previously connected account in place.
    pub fn accept(
        &self,
        pending: PendingAccount,
        approved_user_id: &str,
    ) -> Result<AccountChange, AccountError> {
        if pending.identity.user_id != approved_user_id {
            return Err(AccountError::IdentityMismatch);
        }
        let _guard = self.lock.lock().expect("account lock poisoned");
        let (mut accounts, snapshot) = self.load()?;
        let previous = accounts.get(pending.role).cloned();
        let slot = uuid::Uuid::new_v4().to_string();
        let (access, refresh, _, _) = pending.tokens.into_parts();
        let tokens = OAuthTokens::new(access, refresh);
        self.credentials.replace(pending.role, &slot, &tokens)?;
        accounts.set(
            pending.role,
            Some(AccountIdentity {
                user_id: pending.identity.user_id,
                login: pending.identity.login,
                scopes: pending.identity.scopes,
                credential_slot: slot.clone(),
            }),
        );
        let outcome = match self.publish(snapshot.as_ref(), &accounts) {
            Ok(outcome) => outcome,
            Err(error) => {
                let _ = self.credentials.delete(pending.role, &slot);
                return Err(error);
            }
        };
        let mut cache = self
            .cached_tokens
            .lock()
            .expect("Twitch token cache lock poisoned");
        cache.insert((pending.role, slot), Ok(Some(tokens)));
        if let Some(old) = &previous {
            cache.remove(&(pending.role, old.credential_slot.clone()));
        }
        drop(cache);
        let uncertain_durability = outcome
            .warnings
            .iter()
            .any(|warning| matches!(warning, SaveWarning::DirectorySync(_)));
        let orphaned_credential = previous.is_some_and(|old| {
            uncertain_durability
                || self
                    .credentials
                    .delete(pending.role, &old.credential_slot)
                    .is_err()
        });
        self.configured
            .set(Ok(accounts))
            .expect("Twitch account status lock poisoned");
        Ok(AccountChange {
            warnings: outcome.warnings,
            orphaned_credential,
        })
    }

    pub fn disconnect(&self, role: TwitchRole) -> Result<AccountChange, AccountError> {
        let _guard = self.lock.lock().expect("account lock poisoned");
        let (mut accounts, snapshot) = self.load()?;
        let old = accounts
            .get(role)
            .cloned()
            .ok_or(AccountError::NotConnected)?;
        accounts.set(role, None);
        let outcome = self.publish(snapshot.as_ref(), &accounts)?;
        self.cached_tokens
            .lock()
            .expect("Twitch token cache lock poisoned")
            .remove(&(role, old.credential_slot.clone()));
        let orphaned_credential = outcome
            .warnings
            .iter()
            .any(|warning| matches!(warning, SaveWarning::DirectorySync(_)))
            || self.credentials.delete(role, &old.credential_slot).is_err();
        self.configured
            .set(Ok(accounts))
            .expect("Twitch account status lock poisoned");
        Ok(AccountChange {
            warnings: outcome.warnings,
            orphaned_credential,
        })
    }

    pub(crate) fn load_for_authorization(
        &self,
        role: TwitchRole,
    ) -> Result<(AccountIdentity, OAuthTokens), AccountError> {
        let _guard = self.lock.lock().expect("account lock poisoned");
        let (accounts, _) = self.load()?;
        let account = accounts.get(role).ok_or(AccountError::NotConnected)?;
        let tokens = self
            .cached_tokens
            .lock()
            .expect("Twitch token cache lock poisoned")
            .get(&(role, account.credential_slot.clone()))
            .cloned()
            .unwrap_or(Ok(None))?
            .ok_or(AccountError::MissingCredentials)?;
        Ok((account.clone(), tokens))
    }

    pub(crate) fn is_current(
        &self,
        role: TwitchRole,
        expected: &AccountIdentity,
    ) -> Result<bool, AccountError> {
        let _guard = self.lock.lock().expect("account lock poisoned");
        let (accounts, _) = self.load()?;
        Ok(accounts.get(role) == Some(expected))
    }

    /// A refresh token is single use. Keep the identity and slot fixed, and
    /// publish the new pair to the credential manager before using its access token.
    pub(crate) fn replace_refreshed(
        &self,
        role: TwitchRole,
        expected: &AccountIdentity,
        tokens: &OAuthTokens,
    ) -> Result<bool, AccountError> {
        let _guard = self.lock.lock().expect("account lock poisoned");
        let (accounts, _) = self.load()?;
        if accounts.get(role) != Some(expected) {
            return Ok(false);
        }
        self.credentials
            .replace(role, &expected.credential_slot, tokens)?;
        self.cached_tokens
            .lock()
            .expect("Twitch token cache lock poisoned")
            .insert(
                (role, expected.credential_slot.clone()),
                Ok(Some(tokens.clone())),
            );
        Ok(true)
    }

    fn load(&self) -> Result<(ConnectedAccounts, Option<ConfigSnapshot>), AccountError> {
        let snapshot = match self.store.load(DOCUMENT_ID) {
            Ok(snapshot) => snapshot,
            Err(StoreError::NotFound(_)) => return Ok((ConnectedAccounts::default(), None)),
            Err(error) => return Err(error.into()),
        };
        let accounts = parse(snapshot.config())?;
        Ok((accounts, Some(snapshot)))
    }

    fn publish(
        &self,
        snapshot: Option<&ConfigSnapshot>,
        accounts: &ConnectedAccounts,
    ) -> Result<crate::storage::SaveOutcome, AccountError> {
        let config = StoredConfig {
            definition: DEFINITION.into(),
            version: VERSION,
            data: serde_json::to_value(accounts).map_err(|_| AccountError::InvalidMetadata)?,
        };
        let outcome = match snapshot {
            Some(snapshot) => self.store.save(DOCUMENT_ID, snapshot, &config, validate),
            None => self.store.create(DOCUMENT_ID, &config, validate),
        }?;
        Ok(outcome)
    }
}

fn has_scopes(role: TwitchRole, scopes: &[String]) -> bool {
    let required = match role {
        TwitchRole::Broadcaster => BASE_BROADCASTER_SCOPES,
        TwitchRole::Bot => BOT_SCOPES,
    };
    required
        .iter()
        .all(|required| scopes.iter().any(|actual| actual == required))
}

fn validate(config: &StoredConfig) -> Result<(), String> {
    parse(config).map(|_| ()).map_err(|error| error.to_string())
}

fn parse(config: &StoredConfig) -> Result<ConnectedAccounts, AccountError> {
    if config.definition != DEFINITION || config.version != VERSION {
        return Err(AccountError::InvalidMetadata);
    }
    let accounts: ConnectedAccounts =
        serde_json::from_value(config.data.clone()).map_err(|_| AccountError::InvalidMetadata)?;
    for (role, account) in [
        (TwitchRole::Broadcaster, accounts.broadcaster.as_ref()),
        (TwitchRole::Bot, accounts.bot.as_ref()),
    ] {
        if let Some(account) = account
            && (account.user_id.is_empty()
                || account.login.is_empty()
                || uuid::Uuid::parse_str(&account.credential_slot).is_err()
                || !has_scopes(role, &account.scopes))
        {
            return Err(AccountError::InvalidMetadata);
        }
    }
    Ok(accounts)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::twitch::device::FormResponse;

    #[derive(Default)]
    struct MemoryCredentials(Mutex<HashMap<String, String>>);

    impl CredentialBackend for Arc<MemoryCredentials> {
        fn get(&self, _service: &str, account: &str) -> Result<Option<String>, CredentialError> {
            Ok(self.0.lock().unwrap().get(account).cloned())
        }

        fn set(&self, _service: &str, account: &str, value: &str) -> Result<(), CredentialError> {
            self.0.lock().unwrap().insert(account.into(), value.into());
            Ok(())
        }

        fn delete(&self, _service: &str, account: &str) -> Result<(), CredentialError> {
            self.0.lock().unwrap().remove(account);
            Ok(())
        }
    }

    struct AuthTransport;

    impl FormTransport for AuthTransport {
        async fn post_form(
            &self,
            url: &str,
            fields: &[(&str, &str)],
        ) -> Result<FormResponse, DeviceError> {
            let body = if url.ends_with("/device") {
                r#"{"device_code":"device-secret","user_code":"ABCDEFGH","verification_uri":"https://www.twitch.tv/activate","interval":1,"expires_in":30}"#.to_owned()
            } else {
                let role = fields
                    .iter()
                    .find(|(name, _)| *name == "scopes")
                    .map(|(_, scopes)| {
                        if scopes.contains("channel:") {
                            "broadcaster"
                        } else {
                            "bot"
                        }
                    })
                    .unwrap();
                format!(
                    r#"{{"access_token":"{role}-access-secret","refresh_token":"{role}-refresh-secret","expires_in":3600,"scope":["user:write:chat"],"token_type":"bearer"}}"#
                )
            };
            Ok(FormResponse {
                status: 200,
                body: body.into_bytes(),
                retry_after: None,
            })
        }

        async fn get_bearer(
            &self,
            _url: &str,
            access_token: &str,
        ) -> Result<FormResponse, DeviceError> {
            let (id, login, scopes) = if access_token.starts_with("broadcaster") {
                (
                    "123",
                    "streamer",
                    r#"["channel:manage:broadcast","channel:read:ads","user:read:chat","user:write:chat"]"#,
                )
            } else {
                ("456", "streamerbot", r#"["user:write:chat"]"#)
            };
            Ok(FormResponse {
                status: 200,
                body: format!(r#"{{"client_id":"cz0oehy4mmuoqdropk5m12du03lhxv","user_id":"{id}","login":"{login}","scopes":{scopes},"expires_in":3600}}"#).into_bytes(),
                retry_after: None,
            })
        }
    }

    async fn pending(role: TwitchRole) -> PendingAccount {
        let auth = DeviceAuth::new(AuthTransport);
        let cancel = CancellationToken::new();
        let session = auth.begin(scopes_for(role), &cancel).await.unwrap();
        let tokens = auth.poll(&session, &cancel).await.unwrap();
        review_account(&auth, role, tokens, &cancel).await.unwrap()
    }

    #[tokio::test]
    async fn account_status_changes_publish_only_successful_identity_updates() {
        let dir = tempfile::tempdir().unwrap();
        let accounts = TwitchAccounts::new(
            ConfigStore::new(dir.path()),
            TwitchCredentialStore::new(Arc::new(MemoryCredentials::default()), "client"),
        );
        let mut changes = accounts.subscribe_configuration_changes();
        assert!(accounts.configured_accounts().unwrap().bot.is_none());
        assert!(
            accounts
                .accept(pending(TwitchRole::Bot).await, "wrong-id")
                .is_err()
        );
        assert!(!changes.has_changed().unwrap());
        accounts
            .accept(pending(TwitchRole::Bot).await, "456")
            .unwrap();
        assert!(changes.has_changed().unwrap());
        changes.borrow_and_update();
        assert_eq!(
            accounts.configured_accounts().unwrap().bot.unwrap().login,
            "streamerbot"
        );
        accounts.disconnect(TwitchRole::Bot).unwrap();
        assert!(changes.has_changed().unwrap());
        changes.borrow_and_update();
        assert!(accounts.configured_accounts().unwrap().bot.is_none());
        assert!(accounts.disconnect(TwitchRole::Bot).is_err());
        assert!(!changes.has_changed().unwrap());
    }

    #[tokio::test]
    async fn reviewed_accounts_keep_tokens_out_of_json_and_roles_independent() {
        let dir = tempfile::tempdir().unwrap();
        let backend = Arc::new(MemoryCredentials::default());
        let accounts = TwitchAccounts::new(
            ConfigStore::new(dir.path()),
            TwitchCredentialStore::new(backend.clone(), "cz0oehy4mmuoqdropk5m12du03lhxv"),
        );
        let broadcaster = pending(TwitchRole::Broadcaster).await;
        assert_eq!(broadcaster.login(), "streamer");
        assert!(matches!(
            accounts.accept(broadcaster, "456"),
            Err(AccountError::IdentityMismatch)
        ));
        assert!(backend.0.lock().unwrap().is_empty());

        accounts
            .accept(pending(TwitchRole::Broadcaster).await, "123")
            .unwrap();
        accounts
            .accept(pending(TwitchRole::Bot).await, "456")
            .unwrap();
        let connected = accounts.connected().unwrap();
        assert_eq!(connected.broadcaster.as_ref().unwrap().login, "streamer");
        assert_eq!(connected.bot.as_ref().unwrap().login, "streamerbot");
        assert_eq!(
            accounts
                .load_for_authorization(TwitchRole::Bot)
                .unwrap()
                .1
                .access_token(),
            "bot-access-secret"
        );
        let json = std::fs::read_to_string(dir.path().join("twitch-accounts.json")).unwrap();
        assert!(!json.contains("access-secret"));
        assert!(!json.contains("refresh-secret"));

        accounts.disconnect(TwitchRole::Bot).unwrap();
        assert!(accounts.connected().unwrap().bot.is_none());
        assert!(accounts.connected().unwrap().broadcaster.is_some());
        assert_eq!(backend.0.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn replacement_moves_reference_then_removes_old_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let backend = Arc::new(MemoryCredentials::default());
        let accounts = TwitchAccounts::new(
            ConfigStore::new(dir.path()),
            TwitchCredentialStore::new(backend.clone(), "client"),
        );
        accounts
            .accept(pending(TwitchRole::Bot).await, "456")
            .unwrap();
        let first = accounts.connected().unwrap().bot.unwrap().credential_slot;
        accounts
            .accept(pending(TwitchRole::Bot).await, "456")
            .unwrap();
        let second = accounts.connected().unwrap().bot.unwrap().credential_slot;
        assert_ne!(first, second);
        assert_eq!(backend.0.lock().unwrap().len(), 1);
        assert_eq!(
            accounts
                .load_for_authorization(TwitchRole::Bot)
                .unwrap()
                .1
                .refresh_token(),
            "bot-refresh-secret"
        );
    }

    #[tokio::test]
    async fn unsupported_metadata_stays_untouched_and_no_credential_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("twitch-accounts.json");
        let original = r#"{"definition":"snenkbot.twitch.accounts","version":2,"data":{"bot":{"future":"value"}}}"#;
        std::fs::write(&path, original).unwrap();
        let backend = Arc::new(MemoryCredentials::default());
        let accounts = TwitchAccounts::new(
            ConfigStore::new(dir.path()),
            TwitchCredentialStore::new(backend.clone(), "client"),
        );
        assert!(matches!(
            accounts.connected(),
            Err(AccountError::InvalidMetadata)
        ));
        assert!(matches!(
            accounts.accept(pending(TwitchRole::Bot).await, "456"),
            Err(AccountError::InvalidMetadata)
        ));
        assert_eq!(std::fs::read_to_string(path).unwrap(), original);
        assert!(backend.0.lock().unwrap().is_empty());
    }
}
