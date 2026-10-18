//! Twitch OAuth credentials stored in the operating system credential manager.
//!
//! The serialized configuration format must never contain values from this module.

use std::fmt;

use thiserror::Error;

const SERVICE: &str = "com.snenk.snenkbot.twitch";

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub enum TwitchRole {
    Broadcaster,
    Bot,
}

impl TwitchRole {
    fn key(self) -> &'static str {
        match self {
            Self::Broadcaster => "broadcaster",
            Self::Bot => "bot",
        }
    }
}

/// An OAuth access/refresh token pair. Debug output deliberately omits both values.
#[derive(Clone)]
pub struct OAuthTokens {
    access_token: String,
    refresh_token: String,
}

impl OAuthTokens {
    pub fn new(access_token: String, refresh_token: String) -> Self {
        Self {
            access_token,
            refresh_token,
        }
    }

    pub fn access_token(&self) -> &str {
        &self.access_token
    }

    pub fn refresh_token(&self) -> &str {
        &self.refresh_token
    }
}

impl fmt::Debug for OAuthTokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OAuthTokens([REDACTED])")
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum CredentialError {
    #[error("credential backend failed")]
    Backend,
    #[error("stored credential is malformed")]
    Malformed,
}

/// Backend interface kept small so credential behavior can be tested deterministically.
pub trait CredentialBackend: Send + Sync {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, CredentialError>;
    fn set(&self, service: &str, account: &str, value: &str) -> Result<(), CredentialError>;
    fn delete(&self, service: &str, account: &str) -> Result<(), CredentialError>;
}

/// Stores each role's complete pair in one credential entry. A replacement is a single
/// backend write, so callers can persist it before making use of the refreshed access token.
pub struct TwitchCredentialStore<B = SystemCredentialBackend> {
    backend: B,
    client_id: String,
}

impl TwitchCredentialStore<SystemCredentialBackend> {
    pub fn system(client_id: impl Into<String>) -> Self {
        Self::new(SystemCredentialBackend, client_id)
    }
}

impl<B: CredentialBackend> TwitchCredentialStore<B> {
    pub fn new(backend: B, client_id: impl Into<String>) -> Self {
        Self {
            backend,
            client_id: client_id.into(),
        }
    }

    pub fn load(
        &self,
        role: TwitchRole,
        slot_id: &str,
    ) -> Result<Option<OAuthTokens>, CredentialError> {
        let Some(value) = self.backend.get(SERVICE, &self.account(role, slot_id))? else {
            return Ok(None);
        };
        decode_pair(&value).map(Some)
    }

    /// Atomically replaces the stored pair from the caller's perspective.
    pub fn replace(
        &self,
        role: TwitchRole,
        slot_id: &str,
        tokens: &OAuthTokens,
    ) -> Result<(), CredentialError> {
        let value = encode_pair(tokens);
        self.backend
            .set(SERVICE, &self.account(role, slot_id), &value)
    }

    pub fn delete(&self, role: TwitchRole, slot_id: &str) -> Result<(), CredentialError> {
        self.backend.delete(SERVICE, &self.account(role, slot_id))
    }

    fn account(&self, role: TwitchRole, slot_id: &str) -> String {
        format!("{}:{}:{}:oauth", self.client_id, role.key(), slot_id)
    }
}

/// Uses the platform's native keychain/credential manager through `keyring`.
pub struct SystemCredentialBackend;

impl CredentialBackend for SystemCredentialBackend {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, CredentialError> {
        let entry = keyring::Entry::new(service, account).map_err(|_| CredentialError::Backend)?;
        match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(CredentialError::Backend),
        }
    }

    fn set(&self, service: &str, account: &str, value: &str) -> Result<(), CredentialError> {
        let entry = keyring::Entry::new(service, account).map_err(|_| CredentialError::Backend)?;
        entry
            .set_password(value)
            .map_err(|_| CredentialError::Backend)
    }

    fn delete(&self, service: &str, account: &str) -> Result<(), CredentialError> {
        let entry = keyring::Entry::new(service, account).map_err(|_| CredentialError::Backend)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(CredentialError::Backend),
        }
    }
}

// Length-prefixing preserves arbitrary token bytes represented as UTF-8 without exposing
// token contents through JSON, formatting, or a delimiter convention.
fn encode_pair(tokens: &OAuthTokens) -> String {
    format!(
        "{}:{}{}:{}",
        tokens.access_token.len(),
        tokens.access_token,
        tokens.refresh_token.len(),
        tokens.refresh_token
    )
}

fn decode_pair(value: &str) -> Result<OAuthTokens, CredentialError> {
    fn field(input: &str) -> Result<(&str, &str), CredentialError> {
        let (length, rest) = input.split_once(':').ok_or(CredentialError::Malformed)?;
        let length = length
            .parse::<usize>()
            .map_err(|_| CredentialError::Malformed)?;
        if !rest.is_char_boundary(length) {
            return Err(CredentialError::Malformed);
        }
        Ok(rest.split_at(length))
    }

    let (access_token, rest) = field(value)?;
    let (refresh_token, tail) = field(rest)?;
    if !tail.is_empty() {
        return Err(CredentialError::Malformed);
    }
    Ok(OAuthTokens::new(
        access_token.to_owned(),
        refresh_token.to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Mutex};

    use super::*;

    #[derive(Default)]
    struct MemoryBackend {
        values: Mutex<HashMap<(String, String), String>>,
        fail_reads: bool,
        fail_writes: bool,
        fail_deletes: bool,
    }

    impl CredentialBackend for MemoryBackend {
        fn get(&self, service: &str, account: &str) -> Result<Option<String>, CredentialError> {
            if self.fail_reads {
                return Err(CredentialError::Backend);
            }
            Ok(self
                .values
                .lock()
                .unwrap()
                .get(&(service.into(), account.into()))
                .cloned())
        }

        fn set(&self, service: &str, account: &str, value: &str) -> Result<(), CredentialError> {
            if self.fail_writes {
                return Err(CredentialError::Backend);
            }
            self.values
                .lock()
                .unwrap()
                .insert((service.into(), account.into()), value.into());
            Ok(())
        }

        fn delete(&self, service: &str, account: &str) -> Result<(), CredentialError> {
            if self.fail_deletes {
                return Err(CredentialError::Backend);
            }
            self.values
                .lock()
                .unwrap()
                .remove(&(service.into(), account.into()));
            Ok(())
        }
    }

    #[test]
    fn roles_are_isolated_and_replacement_stores_complete_pair() {
        let store = TwitchCredentialStore::new(MemoryBackend::default(), "client");
        store
            .replace(
                TwitchRole::Broadcaster,
                "account-1",
                &OAuthTokens::new("old".into(), "refresh-1".into()),
            )
            .unwrap();
        store
            .replace(
                TwitchRole::Bot,
                "account-1",
                &OAuthTokens::new("bot".into(), "bot-refresh".into()),
            )
            .unwrap();
        store
            .replace(
                TwitchRole::Broadcaster,
                "account-2",
                &OAuthTokens::new("new".into(), "refresh-2".into()),
            )
            .unwrap();
        let broadcaster = store
            .load(TwitchRole::Broadcaster, "account-2")
            .unwrap()
            .unwrap();
        assert_eq!(broadcaster.access_token(), "new");
        assert_eq!(broadcaster.refresh_token(), "refresh-2");
        let old = store
            .load(TwitchRole::Broadcaster, "account-1")
            .unwrap()
            .unwrap();
        assert_eq!(old.access_token(), "old");
        let bot = store.load(TwitchRole::Bot, "account-1").unwrap().unwrap();
        assert_eq!(bot.access_token(), "bot");
        assert_eq!(bot.refresh_token(), "bot-refresh");
    }

    #[test]
    fn missing_and_backend_failure_are_distinct_and_delete_clears_entry() {
        let store = TwitchCredentialStore::new(MemoryBackend::default(), "client");
        assert!(store.load(TwitchRole::Bot, "slot").unwrap().is_none());
        store
            .replace(
                TwitchRole::Bot,
                "slot",
                &OAuthTokens::new("a".into(), "r".into()),
            )
            .unwrap();
        store.delete(TwitchRole::Bot, "slot").unwrap();
        assert!(store.load(TwitchRole::Bot, "slot").unwrap().is_none());

        let broken = TwitchCredentialStore::new(
            MemoryBackend {
                fail_reads: true,
                ..Default::default()
            },
            "client",
        );
        assert_eq!(
            broken.load(TwitchRole::Bot, "slot").unwrap_err(),
            CredentialError::Backend
        );
        let broken = TwitchCredentialStore::new(
            MemoryBackend {
                fail_writes: true,
                ..Default::default()
            },
            "client",
        );
        assert_eq!(
            broken
                .replace(
                    TwitchRole::Bot,
                    "slot",
                    &OAuthTokens::new("a".into(), "r".into())
                )
                .unwrap_err(),
            CredentialError::Backend
        );
        let broken = TwitchCredentialStore::new(
            MemoryBackend {
                fail_deletes: true,
                ..Default::default()
            },
            "client",
        );
        assert_eq!(
            broken.delete(TwitchRole::Bot, "slot").unwrap_err(),
            CredentialError::Backend
        );
    }

    #[test]
    fn tokens_are_redacted_in_debug_and_pair_parser_handles_delimiters() {
        let tokens = OAuthTokens::new("a:b".into(), "r:🙂".into());
        assert_eq!(format!("{tokens:?}"), "OAuthTokens([REDACTED])");
        let decoded = decode_pair(&encode_pair(&tokens)).unwrap();
        assert_eq!(decoded.access_token(), "a:b");
        assert_eq!(decoded.refresh_token(), "r:🙂");
    }
}
