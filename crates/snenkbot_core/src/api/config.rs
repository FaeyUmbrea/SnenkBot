//! Persistent API configuration and credential administration.

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::watch;

use crate::migration::StoredConfig;
use crate::paths::AppPaths;
use crate::schema::ConfigurationCollectionDescriptor;
use crate::storage::{ConfigStore, SaveOutcome, StoreError};

const DEFINITION: &str = "snenkbot.api.settings";
const CONSUMERS_DEFINITION: &str = "snenkbot.api.consumers";
const SETTINGS_DEFINITION: &str = "snenkbot.api.consumer_settings";
const VERSION: u32 = 1;
const CONSUMERS_VERSION: u32 = 3;
const SERVICE: &str = "com.snenk.snenkbot.api";

/// Named consumers are administered beside the API server's own settings.
pub const CONSUMERS_COLLECTION: ConfigurationCollectionDescriptor =
    ConfigurationCollectionDescriptor {
        id: "api.consumers",
        title: "Consumers",
        item_label: "consumer",
        description: "Manage the overlays and applications that can access SnenkBot.",
    };

/// Channel, provider, and secret permissions granted to one consumer.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, crate::ConfigSchema)]
#[serde(deny_unknown_fields)]
#[config(
    id = "snenkbot.api.consumer_permissions",
    version = 1,
    title = "Consumer permissions"
)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub struct ConsumerPermissions {
    #[config(
        id = "channels",
        label = "Events",
        description = "Choose which live updates this consumer can receive.",
        introduced = 1,
        choice = "api.channels"
    )]
    pub channels: BTreeSet<String>,
    #[config(
        id = "providers",
        label = "Lookups",
        description = "Allow only the lookups this consumer needs.",
        introduced = 1,
        choice = "api.provider_operations"
    )]
    pub providers: BTreeMap<String, BTreeSet<String>>,
    #[config(
        id = "readable_secrets",
        label = "Read secrets",
        description = "Allow this consumer to read these named secrets from its own storage. Leave empty if it does not need secret access.",
        introduced = 1
    )]
    pub readable_secrets: BTreeSet<String>,
    #[config(
        id = "writable_secrets",
        label = "Write secrets",
        description = "Allow this consumer to save or replace these named secrets in its own storage. These are names, not secret values.",
        introduced = 1
    )]
    pub writable_secrets: BTreeSet<String>,
}

/// One named API consumer; only the SHA-256 token digest is persisted.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Consumer {
    pub id: String,
    pub name: String,
    pub token_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_slot: Option<String>,
    pub allowed_origins: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub static_folder: Option<String>,
    pub permissions: ConsumerPermissions,
}

/// Safe consumer administration view without the token digest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub struct ConsumerSummary {
    pub id: String,
    pub name: String,
    pub allowed_origins: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub static_folder: Option<String>,
    pub permissions: ConsumerPermissions,
}

/// API listener configuration stored as a versioned document.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[derive(crate::ConfigSchema)]
#[config(
    id = "snenkbot.api.settings",
    version = 1,
    title = "API server",
    collection = CONSUMERS_COLLECTION,
    category = "general",
    integration = "api"
)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub struct ApiSettings {
    #[config(id = "enabled", label = "Enabled", introduced = 1)]
    pub enabled: bool,
    #[config(id = "host", label = "Host", introduced = 1)]
    pub host: String,
    #[config(id = "port", label = "Port", introduced = 1)]
    pub port: u16,
    #[config(id = "record_support", label = "Record support events", introduced = 1)]
    pub record_support: bool,
    #[config(
        id = "support_retention",
        label = "Support history size",
        introduced = 1
    )]
    #[cfg_attr(feature = "desktop-contracts", specta(type = specta_typescript::Number))]
    pub support_retention: u32,
}

impl Default for ApiSettings {
    /// Keeps the local listener and recording disabled until configured.
    fn default() -> Self {
        Self {
            enabled: false,
            host: "127.0.0.1".into(),
            port: 38764,
            record_support: false,
            support_retention: 500,
        }
    }
}

/// Credential returned once when a consumer is created.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub struct CreatedConsumer {
    pub id: String,
    pub token: String,
}

/// Consumer-owned JSON value with an optimistic concurrency revision.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsumerSetting {
    pub value: serde_json::Value,
    pub revision: u64,
}

/// Failures from API storage or administrative validation.
#[derive(Debug, Error)]
pub enum ApiError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("API configuration is corrupt or has an unsupported version")]
    InvalidFormat,
    #[error("{0}")]
    Invalid(String),
    #[error("the value changed since it was read")]
    Conflict,
    #[error("credential manager is unavailable")]
    Credential,
    #[error("support history writer is full or stopped")]
    Busy,
    #[error("API storage is unavailable")]
    Io(#[from] std::io::Error),
}

/// Owns configuration, consumer settings, and keyring access.
pub struct ApiStore {
    config: ConfigStore,
    consumers: ConfigStore,
    settings: ConfigStore,
    settings_root: PathBuf,
    mutex: Mutex<()>,
    changes: watch::Sender<ApiSettings>,
    consumer_changes: watch::Sender<u64>,
    consumer_cache: RwLock<Vec<Consumer>>,
    startup_error: Option<String>,
    credentials: Arc<dyn super::credentials::ConsumerCredentialStore>,
}

impl ApiStore {
    /// Loads the configuration without repairing a corrupt document.
    pub fn new(paths: &AppPaths) -> Self {
        Self::with_credentials(
            paths,
            Arc::new(super::credentials::SystemConsumerCredentialStore),
        )
    }

    /// Loads consumer data with an injectable native credential backend and no ambient keyring access.
    pub(super) fn with_credentials(
        paths: &AppPaths,
        credentials: Arc<dyn super::credentials::ConsumerCredentialStore>,
    ) -> Self {
        let config = ConfigStore::new(paths.integrations_dir().join("api"));
        let (initial, mut startup_error) = match load_config(&config) {
            Ok(value) => (value, None),
            Err(error) => (ApiSettings::default(), Some(error.to_string())),
        };
        let (changes, _) = watch::channel(initial);
        let (consumer_changes, _) = watch::channel(0);
        let settings_root = paths.data.join("api/settings");
        let mut store = Self {
            config,
            consumers: ConfigStore::new(paths.integrations_dir().join("api/consumers")),
            settings: ConfigStore::new(&settings_root),
            settings_root,
            mutex: Mutex::new(()),
            changes,
            consumer_changes,
            consumer_cache: RwLock::new(Vec::new()),
            startup_error: None,
            credentials,
        };
        match store.load_consumers() {
            Ok(consumers) => {
                *store
                    .consumer_cache
                    .write()
                    .expect("consumer cache lock poisoned") = consumers;
            }
            Err(error) => startup_error = Some(error.to_string()),
        }
        if startup_error.is_some() {
            store.changes.send_replace(ApiSettings::default());
        }
        store.startup_error = startup_error;
        store
    }

    /// Reports a startup storage fault without replacing its source document.
    pub fn startup_error(&self) -> Option<&str> {
        self.startup_error.as_deref()
    }

    /// Watches consumer creation, grant changes, and revocation.
    pub fn subscribe_consumer_changes(&self) -> watch::Receiver<u64> {
        self.consumer_changes.subscribe()
    }

    /// Returns the current persisted configuration.
    pub fn configuration(&self) -> Result<ApiSettings, ApiError> {
        load_config(&self.config)
    }

    /// Reports whether a listener configuration document has been saved, even when disabled.
    pub fn is_configured(&self) -> Result<bool, ApiError> {
        match self.config.load("configuration") {
            Ok(_) => Ok(true),
            Err(StoreError::NotFound(_)) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    /// Subscribes to config changes that affect listener demand and binding.
    pub fn subscribe(&self) -> watch::Receiver<ApiSettings> {
        self.changes.subscribe()
    }

    /// Replaces the configuration if it passes binding and consumer validation.
    pub fn save_configuration(
        &self,
        expected: &ApiSettings,
        value: ApiSettings,
    ) -> Result<SaveOutcome, ApiError> {
        if self.startup_error.is_some() {
            return Err(ApiError::InvalidFormat);
        }
        let _guard = self.mutex.lock().expect("API config lock poisoned");
        validate_config(&value).map_err(ApiError::Invalid)?;
        let stored = stored(DEFINITION, &value)?;
        let outcome = match self.config.load("configuration") {
            Ok(snapshot) => {
                let current = decode_config(snapshot.config())?;
                if &current != expected {
                    return Err(ApiError::Conflict);
                }
                self.config
                    .save("configuration", &snapshot, &stored, |_| Ok(()))
                    .map_err(map_conflict)?
            }
            Err(StoreError::NotFound(_)) if expected == &ApiSettings::default() => self
                .config
                .create("configuration", &stored, |_| Ok(()))
                .map_err(map_conflict)?,
            Err(StoreError::NotFound(_)) => return Err(ApiError::Conflict),
            Err(error) => return Err(error.into()),
        };
        self.changes.send_replace(value);
        Ok(outcome)
    }

    /// Lists consumer metadata without returning bearer credentials.
    pub fn list_consumers(&self) -> Result<Vec<ConsumerSummary>, ApiError> {
        if self.startup_error.is_some() {
            return Err(ApiError::InvalidFormat);
        }
        Ok(self
            .consumer_cache
            .read()
            .expect("consumer cache lock poisoned")
            .clone()
            .into_iter()
            .map(|consumer| ConsumerSummary {
                id: consumer.id,
                name: consumer.name,
                allowed_origins: consumer.allowed_origins,
                static_folder: consumer.static_folder,
                permissions: consumer.permissions,
            })
            .collect())
    }

    /// Reads one current grant from storage before opening an edit session.
    pub fn consumer_configuration(&self, id: &str) -> Result<ConsumerSummary, ApiError> {
        if self.startup_error.is_some() {
            return Err(ApiError::InvalidFormat);
        }
        let _guard = self.mutex.lock().expect("API consumer lock poisoned");
        self.load_consumers()?
            .into_iter()
            .find(|consumer| consumer.id == id)
            .map(|consumer| ConsumerSummary {
                id: consumer.id,
                name: consumer.name,
                permissions: consumer.permissions,
                allowed_origins: consumer.allowed_origins,
                static_folder: consumer.static_folder,
            })
            .ok_or_else(|| ApiError::Invalid("unknown consumer".into()))
    }

    /// Loads the separately versioned consumer credential document.
    fn load_consumers(&self) -> Result<Vec<Consumer>, ApiError> {
        match self.consumers.load("registry") {
            Ok(snapshot) => {
                let value = snapshot.config();
                if value.definition != CONSUMERS_DEFINITION
                    || !(1..=CONSUMERS_VERSION).contains(&value.version)
                {
                    return Err(ApiError::InvalidFormat);
                }
                let consumers: Vec<Consumer> = serde_json::from_value(value.data.clone())
                    .map_err(|_| ApiError::InvalidFormat)?;
                validate_consumers(&consumers).map_err(|_| ApiError::InvalidFormat)?;
                Ok(consumers)
            }
            Err(StoreError::NotFound(_)) => Ok(Vec::new()),
            Err(error) => Err(error.into()),
        }
    }

    /// Replaces the consumer registry after checking identifiers and origins.
    fn save_consumers(&self, consumers: Vec<Consumer>) -> Result<SaveOutcome, ApiError> {
        validate_consumers(&consumers).map_err(ApiError::Invalid)?;
        let mut stored = stored(CONSUMERS_DEFINITION, &consumers)?;
        stored.version = CONSUMERS_VERSION;
        let outcome = match self.consumers.load("registry") {
            Ok(snapshot) => self
                .consumers
                .save("registry", &snapshot, &stored, |_| Ok(()))
                .map_err(map_conflict)?,
            Err(StoreError::NotFound(_)) => self
                .consumers
                .create("registry", &stored, |_| Ok(()))
                .map_err(map_conflict)?,
            Err(error) => return Err(error.into()),
        };
        *self
            .consumer_cache
            .write()
            .expect("consumer cache lock poisoned") = consumers;
        let next = (*self.consumer_changes.borrow()).wrapping_add(1);
        self.consumer_changes.send_replace(next);
        Ok(outcome)
    }

    /// Adds a named consumer and returns its one-time plaintext bearer token.
    pub fn create_consumer(
        &self,
        name: String,
        permissions: ConsumerPermissions,
        allowed_origins: BTreeSet<String>,
        static_folder: Option<String>,
    ) -> Result<CreatedConsumer, ApiError> {
        self.create_consumer_token(name, permissions, allowed_origins, static_folder, false)
    }

    /// Creates a consumer while retaining its recoverable token in native administration storage.
    pub(super) fn create_consumer_with_stored_token(
        &self,
        name: String,
        permissions: ConsumerPermissions,
        allowed_origins: BTreeSet<String>,
        static_folder: Option<String>,
    ) -> Result<CreatedConsumer, ApiError> {
        self.create_consumer_token(name, permissions, allowed_origins, static_folder, true)
    }

    /// Persists a complete consumer only after its optional fresh credential slot is written.
    fn create_consumer_token(
        &self,
        name: String,
        permissions: ConsumerPermissions,
        allowed_origins: BTreeSet<String>,
        static_folder: Option<String>,
        retain_token: bool,
    ) -> Result<CreatedConsumer, ApiError> {
        validate_static_folder(static_folder.as_deref())?;
        if name.trim().is_empty() {
            return Err(ApiError::Invalid("consumer name is required".into()));
        }
        if self.list_consumers()?.len() >= 64 {
            return Err(ApiError::Invalid("consumer limit reached".into()));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let token = new_consumer_token();
        if self.startup_error.is_some() {
            return Err(ApiError::InvalidFormat);
        }
        let _guard = self.mutex.lock().expect("API consumer lock poisoned");
        let mut consumers = self.load_consumers()?;
        let credential_slot = retain_token.then(|| uuid::Uuid::new_v4().to_string());
        consumers.push(Consumer {
            id: id.clone(),
            credential_slot: credential_slot.clone(),
            name,
            token_hash: token_hash(&token),
            allowed_origins,
            static_folder,
            permissions,
        });
        validate_consumers(&consumers).map_err(ApiError::Invalid)?;
        if let Some(slot) = &credential_slot
            && let Err(error) = self.credentials.write(slot, &token)
        {
            self.remove_credential(slot);
            return Err(error);
        }
        if let Err(error) = self.save_consumers(consumers) {
            if let Some(slot) = &credential_slot {
                self.remove_credential(slot);
            }
            return Err(error);
        }
        Ok(CreatedConsumer { id, token })
    }

    /// Replaces one token while preserving the consumer identity, grants and stored data.
    pub(super) fn regenerate_consumer_token(&self, id: &str) -> Result<CreatedConsumer, ApiError> {
        if self.startup_error.is_some() {
            return Err(ApiError::InvalidFormat);
        }
        let _guard = self.mutex.lock().expect("API consumer lock poisoned");
        let mut consumers = self.load_consumers()?;
        let consumer = consumers
            .iter_mut()
            .find(|consumer| consumer.id == id)
            .ok_or_else(|| ApiError::Invalid("unknown consumer".into()))?;
        let old_slot = consumer.credential_slot.clone();
        let slot = uuid::Uuid::new_v4().to_string();
        let token = new_consumer_token();
        consumer.token_hash = token_hash(&token);
        consumer.credential_slot = Some(slot.clone());
        if let Err(error) = self.credentials.write(&slot, &token) {
            self.remove_credential(&slot);
            return Err(error);
        }
        if let Err(error) = self.save_consumers(consumers) {
            self.remove_credential(&slot);
            return Err(error);
        }
        if let Some(old_slot) = old_slot {
            self.remove_credential(&old_slot);
        }
        Ok(CreatedConsumer {
            id: id.into(),
            token,
        })
    }

    /// Retrieves an existing retained token only for a current consumer with enabled hosting.
    pub(super) fn consumer_hosting_token(&self, id: &str) -> Result<String, ApiError> {
        if self.startup_error.is_some() {
            return Err(ApiError::InvalidFormat);
        }
        let _guard = self.mutex.lock().expect("API consumer lock poisoned");
        if !self.configuration()?.enabled {
            return Err(ApiError::Invalid(
                "Enable the API server before copying its hosting URL.".into(),
            ));
        }
        let consumers = self.load_consumers()?;
        let consumer = consumers
            .iter()
            .find(|consumer| consumer.id == id)
            .ok_or_else(|| ApiError::Invalid("unknown consumer".into()))?;
        if consumer.static_folder.is_none() {
            return Err(ApiError::Invalid(
                "Choose and save a static folder for this consumer first.".into(),
            ));
        }
        validate_static_folder(consumer.static_folder.as_deref())?;
        let slot = consumer
            .credential_slot
            .as_ref()
            .ok_or_else(missing_consumer_token)?;
        let token = self
            .credentials
            .read(slot)?
            .ok_or_else(missing_consumer_token)?;
        if !constant_time_eq(
            token_hash(&token).as_bytes(),
            consumer.token_hash.as_bytes(),
        ) {
            return Err(missing_consumer_token());
        }
        Ok(token)
    }

    /// Cleans obsolete native accounts without hiding a registry failure or leaking credential data.
    fn remove_credential(&self, slot: &str) {
        if self.credentials.remove(slot).is_err() {
            tracing::warn!("Could not remove an obsolete API administration credential");
        }
    }

    /// Updates consumer metadata and grants without replacing its bearer token.
    pub fn update_consumer(
        &self,
        id: &str,
        name: String,
        permissions: ConsumerPermissions,
        allowed_origins: BTreeSet<String>,
        static_folder: Option<String>,
    ) -> Result<(), ApiError> {
        validate_static_folder(static_folder.as_deref())?;
        if self.startup_error.is_some() {
            return Err(ApiError::InvalidFormat);
        }
        let _guard = self.mutex.lock().expect("API consumer lock poisoned");
        let mut consumers = self.load_consumers()?;
        let consumer = consumers
            .iter_mut()
            .find(|consumer| consumer.id == id)
            .ok_or_else(|| ApiError::Invalid("unknown consumer".into()))?;
        consumer.name = name;
        consumer.permissions = permissions;
        consumer.allowed_origins = allowed_origins;
        consumer.static_folder = static_folder;
        self.save_consumers(consumers).map(|_| ())
    }

    /// Compares the original grant under the storage lock and updates it without changing its token.
    pub fn save_consumer_configuration(
        &self,
        expected: &ConsumerSummary,
        name: String,
        permissions: ConsumerPermissions,
        allowed_origins: BTreeSet<String>,
        static_folder: Option<String>,
    ) -> Result<(ConsumerSummary, SaveOutcome), ApiError> {
        validate_static_folder(static_folder.as_deref())?;
        if self.startup_error.is_some() {
            return Err(ApiError::InvalidFormat);
        }
        let _guard = self.mutex.lock().expect("API consumer lock poisoned");
        let mut consumers = self.load_consumers()?;
        let consumer = consumers
            .iter_mut()
            .find(|consumer| consumer.id == expected.id)
            .ok_or(ApiError::Conflict)?;
        if consumer.name != expected.name
            || consumer.permissions != expected.permissions
            || consumer.allowed_origins != expected.allowed_origins
            || consumer.static_folder != expected.static_folder
        {
            return Err(ApiError::Conflict);
        }
        consumer.name = name;
        consumer.permissions = permissions;
        consumer.allowed_origins = allowed_origins;
        consumer.static_folder = static_folder;
        let saved = ConsumerSummary {
            id: consumer.id.clone(),
            name: consumer.name.clone(),
            permissions: consumer.permissions.clone(),
            allowed_origins: consumer.allowed_origins.clone(),
            static_folder: consumer.static_folder.clone(),
        };
        let outcome = self.save_consumers(consumers)?;
        // The same lock covers the readback, so another desktop edit cannot replace it here.
        let confirmed = self
            .load_consumers()?
            .into_iter()
            .find(|consumer| consumer.id == saved.id)
            .ok_or(ApiError::Conflict)?;
        if confirmed.name != saved.name
            || confirmed.permissions != saved.permissions
            || confirmed.allowed_origins != saved.allowed_origins
            || confirmed.static_folder != saved.static_folder
        {
            return Err(ApiError::Conflict);
        }
        Ok((saved, outcome))
    }

    /// Removes a consumer and its access immediately.
    pub fn revoke_consumer(&self, id: &str) -> Result<(), ApiError> {
        if self.startup_error.is_some() {
            return Err(ApiError::InvalidFormat);
        }
        let _guard = self.mutex.lock().expect("API consumer lock poisoned");
        let mut consumers = self.load_consumers()?;
        let count = consumers.len();
        let credential_slot = consumers
            .iter()
            .find(|consumer| consumer.id == id)
            .and_then(|consumer| consumer.credential_slot.clone());
        consumers.retain(|consumer| consumer.id != id);
        if consumers.len() == count {
            return Err(ApiError::Invalid("unknown consumer".into()));
        }
        self.save_consumers(consumers)?;
        if let Some(slot) = credential_slot {
            self.remove_credential(&slot);
        }
        Ok(())
    }

    /// Allows browser preflight only for an explicitly registered origin.
    pub fn origin_allowed(&self, origin: &str) -> Result<bool, ApiError> {
        if self.startup_error.is_some() {
            return Err(ApiError::InvalidFormat);
        }
        Ok(self
            .consumer_cache
            .read()
            .expect("consumer cache lock poisoned")
            .iter()
            .any(|consumer| consumer.allowed_origins.contains(origin)))
    }

    /// Resolves one bearer token without exposing stored digests.
    pub(crate) fn authenticate(&self, token: &str) -> Result<Option<Consumer>, ApiError> {
        if self.startup_error.is_some() {
            return Err(ApiError::InvalidFormat);
        }
        let hash = token_hash(token);
        Ok(self
            .consumer_cache
            .read()
            .expect("consumer cache lock poisoned")
            .iter()
            .find(|consumer| constant_time_eq(consumer.token_hash.as_bytes(), hash.as_bytes()))
            .cloned())
    }

    /// Reads a setting in one consumer namespace; absent values have revision zero.
    pub fn read_setting(&self, consumer_id: &str, key: &str) -> Result<ConsumerSetting, ApiError> {
        let id = setting_id(consumer_id, key)?;
        match self.settings.load(&id) {
            Ok(snapshot) => decode_setting(snapshot.config()),
            Err(StoreError::NotFound(_)) => Ok(ConsumerSetting {
                value: serde_json::Value::Null,
                revision: 0,
            }),
            Err(error) => Err(error.into()),
        }
    }

    /// Writes a setting only when its expected revision matches the stored one.
    pub fn write_setting(
        &self,
        consumer_id: &str,
        key: &str,
        value: serde_json::Value,
        expected_revision: u64,
    ) -> Result<ConsumerSetting, ApiError> {
        let id = setting_id(consumer_id, key)?;
        if serde_json::to_vec(&value)
            .map_err(StoreError::Serialize)?
            .len()
            > 128 * 1024
        {
            return Err(ApiError::Invalid("setting exceeds 128 KiB".into()));
        }
        let next = ConsumerSetting {
            value,
            revision: expected_revision
                .checked_add(1)
                .ok_or_else(|| ApiError::Invalid("revision exhausted".into()))?,
        };
        let stored = stored(SETTINGS_DEFINITION, &next)?;
        let _guard = self.mutex.lock().expect("API settings lock poisoned");
        match self.settings.load(&id) {
            Ok(snapshot) => {
                let current = decode_setting(snapshot.config())?;
                if current.revision != expected_revision {
                    return Err(ApiError::Conflict);
                }
                self.settings
                    .save(&id, &snapshot, &stored, |_| Ok(()))
                    .map_err(map_conflict)?;
            }
            Err(StoreError::NotFound(_)) if expected_revision == 0 => {
                if self.setting_count(consumer_id)? >= 64 {
                    return Err(ApiError::Invalid("setting key limit reached".into()));
                }
                self.settings
                    .create(&id, &stored, |_| Ok(()))
                    .map_err(map_conflict)?;
            }
            Err(StoreError::NotFound(_)) => return Err(ApiError::Conflict),
            Err(error) => return Err(error.into()),
        }
        Ok(next)
    }

    /// Counts persisted setting keys in one consumer namespace.
    fn setting_count(&self, consumer_id: &str) -> Result<usize, ApiError> {
        let entries = match std::fs::read_dir(&self.settings_root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(ApiError::Io(error)),
        };
        let prefix = format!("{consumer_id}-");
        let mut count = 0;
        for entry in entries {
            let entry = entry.map_err(ApiError::Io)?;
            let name = entry.file_name();
            if name.to_string_lossy().starts_with(&prefix)
                && name.to_string_lossy().ends_with(".json")
            {
                count += 1;
            }
        }
        Ok(count)
    }

    /// Reads a secret from the operating system credential manager.
    pub fn read_secret(&self, consumer_id: &str, key: &str) -> Result<Option<String>, ApiError> {
        let account = secret_account(consumer_id, key)?;
        let entry = keyring::Entry::new(SERVICE, &account).map_err(|_| ApiError::Credential)?;
        match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(ApiError::Credential),
        }
    }

    /// Writes or deletes a secret in the operating system credential manager.
    pub fn write_secret(
        &self,
        consumer_id: &str,
        key: &str,
        value: Option<&str>,
    ) -> Result<(), ApiError> {
        let account = secret_account(consumer_id, key)?;
        if value.is_some_and(|value| value.len() > 16 * 1024) {
            return Err(ApiError::Invalid("secret exceeds 16 KiB".into()));
        }
        let entry = keyring::Entry::new(SERVICE, &account).map_err(|_| ApiError::Credential)?;
        match value {
            Some(value) => entry.set_password(value).map_err(|_| ApiError::Credential),
            None => match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(_) => Err(ApiError::Credential),
            },
        }
    }
}

/// Loads the versioned listener document or its disabled default.
fn load_config(store: &ConfigStore) -> Result<ApiSettings, ApiError> {
    match store.load("configuration") {
        Ok(snapshot) => decode_config(snapshot.config()),
        Err(StoreError::NotFound(_)) => Ok(ApiSettings::default()),
        Err(error) => Err(error.into()),
    }
}

/// Validates stored metadata before interpreting listener values.
fn decode_config(config: &StoredConfig) -> Result<ApiSettings, ApiError> {
    if config.definition != DEFINITION || config.version != VERSION {
        return Err(ApiError::InvalidFormat);
    }
    let value: ApiSettings =
        serde_json::from_value(config.data.clone()).map_err(|_| ApiError::InvalidFormat)?;
    validate_config(&value).map_err(|_| ApiError::InvalidFormat)?;
    Ok(value)
}

/// Restricts listener binding and durable retention to supported ranges.
fn validate_config(config: &ApiSettings) -> Result<(), String> {
    if config.host.parse::<IpAddr>().is_err() || config.port == 0 {
        return Err("API host must be an IP address and port must be nonzero".into());
    }
    if config.support_retention == 0 || config.support_retention > 10000 {
        return Err("support retention must be 1 to 10000".into());
    }
    Ok(())
}

/// Validates every persisted grant against compiled channels and provider operations.
fn validate_consumers(consumers: &[Consumer]) -> Result<(), String> {
    if consumers.len() > 64 {
        return Err("consumer limit reached".into());
    }
    let mut ids = BTreeSet::new();
    for consumer in consumers {
        if consumer.name.trim().is_empty()
            || consumer.name.len() > 100
            || consumer.name.chars().any(char::is_control)
            || !ids.insert(&consumer.id)
            || !consumer
                .id
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
            || consumer
                .credential_slot
                .as_ref()
                .is_some_and(|slot| uuid::Uuid::parse_str(slot).is_err())
            || consumer.token_hash.len() != 64
        {
            return Err("invalid consumer metadata".into());
        }
        if consumer
            .permissions
            .channels
            .iter()
            .any(|channel| !super::channel_ids().contains(&channel.as_str()))
        {
            return Err("unknown overlay channel permission".into());
        }
        if consumer.allowed_origins.len() > 32
            || consumer.permissions.channels.len()
                + consumer.permissions.readable_secrets.len()
                + consumer.permissions.writable_secrets.len()
                + consumer
                    .permissions
                    .providers
                    .values()
                    .map(BTreeSet::len)
                    .sum::<usize>()
                > 64
        {
            return Err("too many origins or permissions".into());
        }
        let catalog = crate::providers::catalog();
        for (provider, operations) in &consumer.permissions.providers {
            let Some(descriptor) = catalog.iter().find(|descriptor| descriptor.id == provider)
            else {
                return Err("unknown provider permission".into());
            };
            if operations.iter().any(|operation| {
                !descriptor
                    .operations
                    .iter()
                    .any(|available| available.id == operation)
            }) {
                return Err("unknown provider operation permission".into());
            }
        }
        for key in consumer
            .permissions
            .readable_secrets
            .iter()
            .chain(&consumer.permissions.writable_secrets)
        {
            setting_id(&consumer.id, key).map_err(|_| "invalid secret permission key")?;
        }
        for origin in &consumer.allowed_origins {
            // Files opened directly in a browser use an opaque origin serialized as `null`.
            // This opt-in still requires this consumer's bearer credential.
            if origin == "null" {
                continue;
            }
            let url = reqwest::Url::parse(origin).map_err(|_| "allowed origin is invalid")?;
            if !matches!(url.scheme(), "http" | "https")
                || url.username() != ""
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
                || url.path() != "/"
                || url.origin().ascii_serialization() != *origin
            {
                return Err(
                    "allowed origins must be exact HTTP origins or null for local files".into(),
                );
            }
        }
    }
    Ok(())
}

/// Requires an explicitly configured hosting root to be an existing absolute directory.
fn validate_static_folder(folder: Option<&str>) -> Result<(), ApiError> {
    let Some(folder) = folder else {
        return Ok(());
    };
    let path = std::path::Path::new(folder);
    if !path.is_absolute() || !path.is_dir() {
        return Err(ApiError::Invalid(
            "static folder must be an existing absolute directory".into(),
        ));
    }
    Ok(())
}

/// Generates an independent random token for a new or regenerated consumer credential.
fn new_consumer_token() -> String {
    format!(
        "sb_{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// Explains recovery for pre-existing hash-only consumers without rotating during a read.
fn missing_consumer_token() -> ApiError {
    ApiError::Invalid("This consumer's token is not available on this computer. Regenerate its token, then copy the hosting URL and update clients using the old token.".into())
}

/// Wraps a consumer or setting value in the repository's versioned document format.
fn stored<T: Serialize>(definition: &str, value: &T) -> Result<StoredConfig, ApiError> {
    Ok(StoredConfig {
        definition: definition.into(),
        version: VERSION,
        data: serde_json::to_value(value).map_err(StoreError::Serialize)?,
    })
}

/// Rejects setting documents from another definition or version.
fn decode_setting(config: &StoredConfig) -> Result<ConsumerSetting, ApiError> {
    if config.definition != SETTINGS_DEFINITION || config.version != VERSION {
        return Err(ApiError::InvalidFormat);
    }
    serde_json::from_value(config.data.clone()).map_err(|_| ApiError::InvalidFormat)
}

/// Builds a path-safe per-consumer setting identifier.
fn setting_id(consumer_id: &str, key: &str) -> Result<String, ApiError> {
    if consumer_id.is_empty()
        || key.is_empty()
        || key.len() > 128
        || !consumer_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
        || !key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.')
    {
        return Err(ApiError::Invalid("invalid setting or secret key".into()));
    }
    Ok(format!("{consumer_id}-{key}"))
}

/// Names a consumer secret separately inside the API keyring service.
fn secret_account(consumer_id: &str, key: &str) -> Result<String, ApiError> {
    Ok(format!("{}:{key}", setting_id(consumer_id, key)?))
}

/// Derives the digest persisted in the consumer registry.
fn token_hash(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

/// Compares equal-length token digests without a data-dependent early exit.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

/// Preserves optimistic-concurrency failures as HTTP conflicts.
fn map_conflict(error: StoreError) -> ApiError {
    match error {
        StoreError::Conflict(_) | StoreError::AlreadyExists(_) => ApiError::Conflict,
        other => other.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    use super::super::credentials::MemoryConsumerCredentialStore;
    use std::sync::atomic::Ordering;

    /// Enables hosted-link recovery on a temporary registry with injected credentials.
    fn retained_store(
        paths: &AppPaths,
        credentials: &Arc<MemoryConsumerCredentialStore>,
    ) -> ApiStore {
        let store = ApiStore::with_credentials(
            paths,
            Arc::clone(credentials) as Arc<dyn super::super::ConsumerCredentialStore>,
        );
        if !store.configuration().unwrap().enabled {
            let expected = store.configuration().unwrap();
            let mut settings = expected.clone();
            settings.enabled = true;
            store.save_configuration(&expected, settings).unwrap();
        }
        store
    }

    /// Retained credentials survive reopening while rotation changes only authentication state.
    #[test]
    fn retained_token_rotation_preserves_identity_grants_and_settings() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = paths(temporary.path());
        let credentials = Arc::new(MemoryConsumerCredentialStore::default());
        let store = retained_store(&paths, &credentials);
        let original = store
            .create_consumer_with_stored_token(
                "Overlay".into(),
                ConsumerPermissions {
                    channels: BTreeSet::from(["api.custom".into()]),
                    ..Default::default()
                },
                BTreeSet::from(["null".into()]),
                Some(temporary.path().to_string_lossy().into_owned()),
            )
            .unwrap();
        let before = store.consumer_configuration(&original.id).unwrap();
        store
            .write_setting(
                &original.id,
                "animations",
                serde_json::json!({"enabled":true}),
                0,
            )
            .unwrap();
        let settings_before = store.read_setting(&original.id, "animations").unwrap();
        let mut changed = store.subscribe_consumer_changes();
        changed.borrow_and_update();
        let rotated = store.regenerate_consumer_token(&original.id).unwrap();
        assert_eq!(rotated.id, original.id);
        assert_ne!(rotated.token, original.token);
        assert_eq!(store.consumer_configuration(&original.id).unwrap(), before);
        assert_eq!(
            store
                .read_setting(&original.id, "animations")
                .unwrap()
                .value,
            settings_before.value
        );
        assert!(store.authenticate(&original.token).unwrap().is_none());
        assert!(store.authenticate(&rotated.token).unwrap().is_some());
        assert!(changed.has_changed().unwrap());
        assert_eq!(credentials.tokens.lock().unwrap().len(), 1);
        let restarted = retained_store(&paths, &credentials);
        assert_eq!(
            restarted.consumer_hosting_token(&original.id).unwrap(),
            rotated.token
        );
        let encoded =
            std::fs::read_to_string(paths.integrations_dir().join("api/consumers/registry.json"))
                .unwrap();
        assert!(!encoded.contains(&rotated.token));
        restarted.revoke_consumer(&original.id).unwrap();
        assert!(restarted.authenticate(&rotated.token).unwrap().is_none());
        assert!(restarted.consumer_hosting_token(&original.id).is_err());
        assert!(credentials.tokens.lock().unwrap().is_empty());
    }

    /// Legacy records fail recovery without credential access or implicit rotation.
    #[test]
    fn hash_only_consumers_remain_unchanged_until_explicit_regeneration() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = paths(temporary.path());
        let credentials = Arc::new(MemoryConsumerCredentialStore::default());
        let store = retained_store(&paths, &credentials);
        let original = store
            .create_consumer(
                "Legacy".into(),
                ConsumerPermissions::default(),
                BTreeSet::new(),
                Some(temporary.path().to_string_lossy().into_owned()),
            )
            .unwrap();
        let registry = paths.integrations_dir().join("api/consumers/registry.json");
        let bytes = std::fs::read(&registry).unwrap();
        let reads = credentials.reads.load(Ordering::SeqCst);
        assert!(
            store
                .consumer_hosting_token(&original.id)
                .unwrap_err()
                .to_string()
                .contains("Regenerate")
        );
        assert_eq!(credentials.reads.load(Ordering::SeqCst), reads);
        assert_eq!(std::fs::read(&registry).unwrap(), bytes);
        assert!(store.authenticate(&original.token).unwrap().is_some());
        let rotated = store.regenerate_consumer_token(&original.id).unwrap();
        credentials.tokens.lock().unwrap().clear();
        assert!(
            store
                .consumer_hosting_token(&original.id)
                .unwrap_err()
                .to_string()
                .contains("Regenerate")
        );
        assert!(store.authenticate(&rotated.token).unwrap().is_some());
    }

    /// A failed credential write leaves the existing token and portable registry intact.
    #[test]
    fn credential_write_failure_keeps_old_authentication() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = paths(temporary.path());
        let credentials = Arc::new(MemoryConsumerCredentialStore::default());
        let store = retained_store(&paths, &credentials);
        let original = store
            .create_consumer_with_stored_token(
                "Overlay".into(),
                ConsumerPermissions::default(),
                BTreeSet::new(),
                None,
            )
            .unwrap();
        let registry = paths.integrations_dir().join("api/consumers/registry.json");
        let before = std::fs::read(&registry).unwrap();
        credentials.fail_writes.store(true, Ordering::SeqCst);
        assert!(matches!(
            store.regenerate_consumer_token(&original.id),
            Err(ApiError::Credential)
        ));
        assert!(store.authenticate(&original.token).unwrap().is_some());
        assert_eq!(std::fs::read(registry).unwrap(), before);
        assert_eq!(credentials.tokens.lock().unwrap().len(), 1);
    }

    /// Registry failure cleans only the new slot; failed cleanup cannot mask the save error.
    #[test]
    fn registry_failure_preserves_old_slot_and_original_error() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = paths(temporary.path());
        let credentials = Arc::new(MemoryConsumerCredentialStore::default());
        let store = retained_store(&paths, &credentials);
        let original = store
            .create_consumer_with_stored_token(
                "Overlay".into(),
                ConsumerPermissions::default(),
                BTreeSet::new(),
                None,
            )
            .unwrap();
        let old_slot = store.load_consumers().unwrap()[0]
            .credential_slot
            .clone()
            .unwrap();
        let registry = paths.integrations_dir().join("api/consumers/registry.json");
        let backup = registry.with_extension("original");
        for cleanup_fails in [false, true] {
            let fail_registry = registry.clone();
            let preserved = backup.clone();
            credentials
                .fail_removals
                .store(cleanup_fails, Ordering::SeqCst);
            *credentials.after_write.lock().unwrap() = Some(Box::new(move || {
                std::fs::rename(&fail_registry, preserved).unwrap();
                std::fs::create_dir(fail_registry).unwrap();
            }));
            let error = store.regenerate_consumer_token(&original.id).unwrap_err();
            assert!(!matches!(error, ApiError::Credential));
            assert!(store.authenticate(&original.token).unwrap().is_some());
            assert_eq!(
                credentials.tokens.lock().unwrap().len(),
                if cleanup_fails { 2 } else { 1 }
            );
            assert_eq!(
                credentials.tokens.lock().unwrap().get(&old_slot),
                Some(&original.token)
            );
            std::fs::remove_dir(&registry).unwrap();
            std::fs::rename(&backup, &registry).unwrap();
            credentials
                .tokens
                .lock()
                .unwrap()
                .retain(|slot, _| slot == &old_slot);
        }
        credentials.fail_removals.store(true, Ordering::SeqCst);
        let rotated = store.regenerate_consumer_token(&original.id).unwrap();
        assert!(store.authenticate(&rotated.token).unwrap().is_some());
        assert!(store.authenticate(&original.token).unwrap().is_none());
    }

    /// Existing version-one grants omit hosting and retain their original serialization.
    #[test]
    fn old_consumer_records_default_to_no_hosting() {
        let record = serde_json::json!({
            "id": "old-consumer",
            "name": "Existing overlay",
            "token_hash": "a".repeat(64),
            "allowed_origins": [],
            "permissions": {"channels": [], "providers": {}, "readable_secrets": [], "writable_secrets": []}
        });
        let consumer: Consumer = serde_json::from_value(record.clone()).unwrap();
        assert!(consumer.static_folder.is_none());
        assert_eq!(serde_json::to_value(consumer).unwrap(), record);
    }

    /// Version-one grants load unchanged and advance only on an ordinary successful save.
    #[test]
    fn consumer_registry_upgrade_preserves_existing_grants() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = AppPaths {
            config: temporary.path().join("config"),
            data: temporary.path().join("data"),
            state: temporary.path().join("state"),
        };
        let records = vec![Consumer {
            id: "existing".into(),
            name: "Existing overlay".into(),
            token_hash: token_hash("test-token"),
            credential_slot: None,
            allowed_origins: BTreeSet::from(["null".into()]),
            permissions: ConsumerPermissions::default(),
            static_folder: None,
        }];
        let registry = ConfigStore::new(paths.integrations_dir().join("api/consumers"));
        registry
            .create(
                "registry",
                &stored(CONSUMERS_DEFINITION, &records).unwrap(),
                |_| Ok(()),
            )
            .unwrap();
        let original = registry.load("registry").unwrap().original_bytes().to_vec();
        let store = ApiStore::new(&paths);
        assert!(store.startup_error().is_none());
        assert!(store.authenticate("test-token").unwrap().is_some());
        assert_eq!(
            registry.load("registry").unwrap().original_bytes(),
            original
        );
        store
            .update_consumer(
                "existing",
                "Renamed overlay".into(),
                ConsumerPermissions::default(),
                BTreeSet::from(["null".into()]),
                None,
            )
            .unwrap();
        assert_eq!(
            registry.load("registry").unwrap().config().version,
            CONSUMERS_VERSION
        );
        assert!(store.authenticate("test-token").unwrap().is_some());
    }

    /// Invalid roots fail explicitly while optional hosting performs no filesystem lookup.
    #[test]
    fn hosting_roots_require_existing_absolute_directories() {
        assert!(validate_static_folder(None).is_ok());
        assert!(validate_static_folder(Some("relative/path")).is_err());
        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("file.html");
        std::fs::write(&file, "hello").unwrap();
        assert!(validate_static_folder(temporary.path().to_str()).is_ok());
        assert!(validate_static_folder(file.to_str()).is_err());
        assert!(validate_static_folder(temporary.path().join("missing").to_str()).is_err());
    }

    /// Gives each test independent configuration and data roots.
    fn paths(root: &std::path::Path) -> AppPaths {
        AppPaths {
            config: root.join("config"),
            data: root.join("data"),
            state: root.join("state"),
        }
    }

    /// Keeps concurrent registry mutations and per-consumer settings isolated.
    #[test]
    fn consumers_are_isolated_and_concurrent_creations_keep_every_record() {
        let temporary = tempfile::tempdir().unwrap();
        let store = Arc::new(ApiStore::new(&paths(temporary.path())));
        let created = std::thread::scope(|scope| {
            (0..16)
                .map(|index| {
                    let store = Arc::clone(&store);
                    scope.spawn(move || {
                        store
                            .create_consumer(
                                format!("overlay {index}"),
                                ConsumerPermissions::default(),
                                BTreeSet::new(),
                                None,
                            )
                            .unwrap()
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(store.list_consumers().unwrap().len(), 16);
        for credential in &created {
            assert!(store.authenticate(&credential.token).unwrap().is_some());
            assert!(!format!("{:?}", store.list_consumers().unwrap()).contains(&credential.token));
        }
        let first = &created[0];
        let second = &created[1];
        store
            .write_setting(&first.id, "animations", serde_json::json!({"speed":2}), 0)
            .unwrap();
        assert_eq!(
            store.read_setting(&second.id, "animations").unwrap().value,
            serde_json::Value::Null
        );
        assert!(matches!(
            store.write_setting(&first.id, "animations", serde_json::json!(3), 0),
            Err(ApiError::Conflict)
        ));
        store.revoke_consumer(&first.id).unwrap();
        assert!(store.authenticate(&first.token).unwrap().is_none());
        assert!(store.authenticate(&second.token).unwrap().is_some());
    }

    /// A stale editor cannot replace another grant edit or rotate the original token.
    #[test]
    fn consumer_configuration_save_preserves_token_and_rejects_stale_values() {
        let temporary = tempfile::tempdir().unwrap();
        let store = ApiStore::new(&paths(temporary.path()));
        let created = store
            .create_consumer(
                "original".into(),
                ConsumerPermissions::default(),
                BTreeSet::new(),
                None,
            )
            .unwrap();
        let original = store
            .list_consumers()
            .unwrap()
            .into_iter()
            .find(|consumer| consumer.id == created.id)
            .unwrap();
        let (saved, outcome) = store
            .save_consumer_configuration(
                &original,
                "edited".into(),
                ConsumerPermissions::default(),
                BTreeSet::new(),
                None,
            )
            .unwrap();
        assert!(outcome.published);
        assert_eq!(saved.name, "edited");
        assert_eq!(
            store.authenticate(&created.token).unwrap().unwrap().name,
            "edited"
        );
        assert!(matches!(
            store.save_consumer_configuration(
                &original,
                "stale edit".into(),
                ConsumerPermissions::default(),
                BTreeSet::new(),
                None,
            ),
            Err(ApiError::Conflict)
        ));
        assert_eq!(store.list_consumers().unwrap()[0], saved);

        let parallel = ApiStore::new(&paths(temporary.path()));
        parallel
            .update_consumer(
                &created.id,
                "outside edit".into(),
                ConsumerPermissions::default(),
                BTreeSet::new(),
                None,
            )
            .unwrap();
        assert_eq!(
            store.consumer_configuration(&created.id).unwrap().name,
            "outside edit"
        );
    }

    /// Preserves a corrupt source document for explicit repair.
    #[test]
    fn corrupt_settings_are_reported_without_repair() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = paths(temporary.path());
        let store = ApiStore::new(&paths);
        store
            .save_configuration(&ApiSettings::default(), ApiSettings::default())
            .unwrap();
        let path = paths.integrations_dir().join("api/configuration.json");
        std::fs::write(&path, b"{broken").unwrap();
        assert!(matches!(
            store.configuration(),
            Err(ApiError::Store(StoreError::InvalidDocument { .. }))
        ));
        assert_eq!(std::fs::read(&path).unwrap(), b"{broken");
    }

    /// Loads only validated persisted grants and removes revoked grants immediately.
    #[test]
    fn cached_credentials_survive_restart_and_revoke_immediately() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = paths(temporary.path());
        let store = ApiStore::new(&paths);
        let created = store
            .create_consumer(
                "overlay".into(),
                ConsumerPermissions::default(),
                BTreeSet::from(["http://localhost:5173".into()]),
                None,
            )
            .unwrap();
        assert!(store.authenticate(&created.token).unwrap().is_some());
        drop(store);
        let restarted = ApiStore::new(&paths);
        assert!(restarted.authenticate(&created.token).unwrap().is_some());
        assert!(restarted.origin_allowed("http://localhost:5173").unwrap());
        restarted.revoke_consumer(&created.id).unwrap();
        assert!(restarted.authenticate(&created.token).unwrap().is_none());
        assert!(!restarted.origin_allowed("http://localhost:5173").unwrap());
        assert!(
            ApiStore::new(&paths)
                .authenticate(&created.token)
                .unwrap()
                .is_none()
        );
    }
}
