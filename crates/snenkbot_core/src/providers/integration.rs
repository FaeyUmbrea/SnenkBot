//! Provider configuration, credential, and workflow lifecycle ownership.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use thiserror::Error;
use tokio::sync::watch;

use crate::engine::Engine;
use crate::integration::{
    ConfigurationStatus, ConnectionState, ConnectionStatus, ReconfigurationRequest,
};
use crate::paths::AppPaths;
use crate::storage::{SaveOutcome, StoreError};

use super::{
    ProviderError, ProviderId, ProviderSecrets, ProviderServices, ProviderSettings,
    ProviderSettingsError, ProviderSettingsStore,
};

const KEYRING_SERVICE: &str = "com.snenk.snenkbot.providers";
const KEYRING_ACCOUNT: &str = "steamgriddb-api-key";

/// Failures while configuring the compiled provider integration.
#[derive(Debug, Error)]
pub enum ProviderIntegrationError {
    #[error(transparent)]
    Settings(#[from] ProviderSettingsError),
    #[error(transparent)]
    Service(#[from] ProviderError),
    #[error("provider settings changed since they were read")]
    Conflict,
    #[error("provider credential manager is unavailable")]
    Credential,
}

/// Cached provider key state read only during startup or explicit administration.
struct CredentialCache {
    key: RwLock<Option<String>>,
    available: AtomicBool,
}

impl CredentialCache {
    /// Creates an empty cache that does not access the operating system keyring.
    fn new() -> Self {
        Self {
            key: RwLock::new(None),
            available: AtomicBool::new(true),
        }
    }

    /// Stores a successful keyring read for later provider requests.
    fn set(&self, key: Option<String>) -> bool {
        let mut current = self.key.write().expect("provider credential lock");
        let was_available = self.available.load(Ordering::SeqCst);
        let changed = !was_available || *current != key;
        *current = key;
        self.available.store(true, Ordering::SeqCst);
        changed
    }

    /// Records a keyring read failure without retaining an old credential.
    fn fail(&self) -> bool {
        let mut current = self.key.write().expect("provider credential lock");
        let was_available = self.available.load(Ordering::SeqCst);
        let changed = was_available || current.is_some();
        *current = None;
        self.available.store(false, Ordering::SeqCst);
        changed
    }
}

impl ProviderSecrets for CredentialCache {
    /// Serves provider calls from memory without opening the system keyring.
    fn steamgriddb_key(&self) -> Result<Option<String>, ProviderError> {
        if !self.available.load(Ordering::SeqCst) {
            return Err(ProviderError::CredentialUnavailable);
        }
        Ok(self.key.read().expect("provider credential lock").clone())
    }
}

/// Owns provider settings, native credentials, lookup services, and action registration.
pub struct ProviderIntegration {
    settings_store: ProviderSettingsStore,
    settings_lock: Mutex<()>,
    settings_faults: RwLock<[Option<String>; 3]>,
    key_lock: Mutex<()>,
    credentials: Arc<CredentialCache>,
    services: Arc<ProviderServices>,
    changed: watch::Sender<u64>,
}

impl ProviderIntegration {
    /// Loads provider settings, warms enabled credentials, and registers workflow and Lua actions.
    pub fn register(
        paths: &AppPaths,
        engine: &mut Engine,
    ) -> Result<Arc<Self>, ProviderIntegrationError> {
        let settings_store = ProviderSettingsStore::new(paths);
        let (settings, settings_faults) = read_available(&settings_store);
        let credentials = Arc::new(CredentialCache::new());
        if settings.steamgriddb_enabled {
            match read_keyring_key() {
                Ok(key) => {
                    credentials.set(key);
                }
                Err(_) => {
                    credentials.fail();
                }
            }
        }
        let services = Arc::new(ProviderServices::new(
            settings,
            Arc::clone(&credentials) as Arc<dyn ProviderSecrets>,
        )?);
        super::actions::register(engine, Arc::clone(&services));
        Ok(Arc::new(Self {
            settings_store,
            settings_lock: Mutex::new(()),
            settings_faults: RwLock::new(settings_faults),
            key_lock: Mutex::new(()),
            credentials,
            services,
            changed: watch::channel(0).0,
        }))
    }

    /// Returns the persisted enabled switches without opening the keyring.
    pub fn settings(&self) -> Result<ProviderSettings, ProviderIntegrationError> {
        Ok(self.settings_store.load()?.value)
    }

    /// Reports whether this provider has its own saved setup or a legacy combined setup.
    pub fn configured(&self, provider: ProviderId) -> Result<bool, ProviderIntegrationError> {
        Ok(self.settings_store.load_provider(provider)?.configured)
    }

    /// Reads one provider's saved switch without requiring healthy sibling documents.
    pub fn enabled(&self, provider: ProviderId) -> Result<bool, ProviderIntegrationError> {
        Ok(self.settings_store.load_provider(provider)?.enabled)
    }

    /// Saves one provider switch while preserving concurrently edited sibling providers.
    pub fn save_enabled(
        &self,
        provider: ProviderId,
        expected: bool,
        enabled: bool,
    ) -> Result<SaveOutcome, ProviderIntegrationError> {
        let _guard = self.settings_lock.lock().expect("provider settings lock");
        let loaded = self.settings_store.load_provider(provider)?;
        if loaded.enabled != expected {
            return Err(ProviderIntegrationError::Conflict);
        }
        if provider == ProviderId::SteamGridDb && enabled {
            self.refresh_key()?;
        }
        let outcome = self
            .settings_store
            .save_provider(&loaded, provider, enabled)
            .map_err(map_settings_conflict)?;
        // A sibling may have been saved by another process since this save began.
        // Rebuild the runtime view from the persisted documents after our write.
        self.refresh_runtime_settings(provider == ProviderId::SteamGridDb && enabled);
        self.notify_changed();
        Ok(outcome)
    }

    /// Publishes a fresh persisted view after saves, including sibling writes by other processes.
    fn refresh_runtime_settings(&self, steam_key_warmed: bool) {
        let (value, faults) = read_available(&self.settings_store);
        if !steam_key_warmed
            && !self.services.settings().steamgriddb_enabled
            && value.steamgriddb_enabled
        {
            // A different process can enable SteamGridDB while this process edits a sibling.
            // Keep the saved switch and expose keyring failure through readiness status.
            let _ = self.refresh_key();
        }
        self.services.set_settings(value);
        *self.settings_faults.write().expect("provider fault lock") = faults;
    }

    /// Returns whether the SteamGridDB key exists after an explicit keyring read.
    pub fn key_present(&self) -> Result<bool, ProviderIntegrationError> {
        self.refresh_key()?;
        Ok(self.credentials.steamgriddb_key()?.is_some())
    }

    /// Saves or removes the SteamGridDB key, then invalidates previous lookup results.
    pub fn set_key(&self, key: Option<&str>) -> Result<(), ProviderIntegrationError> {
        let _guard = self.key_lock.lock().expect("provider key lock");
        if key.is_some_and(|key| {
            key.trim().is_empty() || key.len() > 512 || key.chars().any(char::is_control)
        }) {
            return Err(ProviderIntegrationError::Service(
                ProviderError::InvalidArguments("invalid SteamGridDB API key".into()),
            ));
        }
        let entry = keyring_entry()?;
        match key {
            Some(key) => entry
                .set_password(key)
                .map_err(|_| ProviderIntegrationError::Credential)?,
            None => match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => {}
                Err(_) => return Err(ProviderIntegrationError::Credential),
            },
        }
        if self.credentials.set(key.map(str::to_owned)) {
            self.services.clear_cache();
            self.notify_changed();
        }
        Ok(())
    }

    /// Returns the lookup service shared with the authenticated overlay API.
    pub fn services(&self) -> Arc<ProviderServices> {
        Arc::clone(&self.services)
    }

    /// Refreshes the memory cache only during explicit configuration work.
    fn refresh_key(&self) -> Result<(), ProviderIntegrationError> {
        let _guard = self.key_lock.lock().expect("provider key lock");
        match read_keyring_key() {
            Ok(key) => {
                if self.credentials.set(key) {
                    self.services.clear_cache();
                    self.notify_changed();
                }
                Ok(())
            }
            Err(error) => {
                if self.credentials.fail() {
                    self.services.clear_cache();
                    self.notify_changed();
                }
                Err(error)
            }
        }
    }

    /// Wakes desktop status observers after a committed settings or credential change.
    fn notify_changed(&self) {
        self.changed
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }
}

impl ConfigurationStatus for ProviderIntegration {
    /// Asks for setup only when saved settings are invalid or an enabled key is missing.
    fn reconfiguration_requests(&self) -> Vec<ReconfigurationRequest> {
        let faults = self.settings_faults.read().expect("provider fault lock");
        let mut requests: Vec<_> = ProviderId::ALL
            .into_iter()
            .filter(|provider| faults[provider.index()].is_some())
            .map(|provider| ReconfigurationRequest {
                integration: provider.document_id(),
                connection: "settings",
                reason: format!(
                    "{} settings are invalid; repair the saved configuration",
                    provider_title(provider)
                ),
                affected_features: vec![provider_title(provider).into()],
            })
            .collect();
        let settings = self.services.settings();
        if settings.steamgriddb_enabled
            && let Err(error) = self.services.ready("steamgriddb")
        {
            let reason = match error {
                ProviderError::CredentialUnavailable => {
                    "The credential manager is unavailable; check SteamGridDB setup"
                }
                _ => "Set a SteamGridDB API key to resolve game artwork",
            };
            requests.push(ReconfigurationRequest {
                integration: "steamgriddb",
                connection: "api_key",
                reason: reason.into(),
                affected_features: vec!["SteamGridDB game search and icons".into()],
            });
        }
        requests
    }

    /// Shows enabled providers' configured readiness without making a network request.
    fn connection_statuses(&self) -> Vec<ConnectionStatus> {
        let faults = self.settings_faults.read().expect("provider fault lock");
        let mut statuses: Vec<_> = ProviderId::ALL
            .into_iter()
            .filter(|provider| faults[provider.index()].is_some())
            .map(|provider| ConnectionStatus {
                integration: provider.document_id().into(),
                connection: "settings".into(),
                title: provider_title(provider).into(),
                state: ConnectionState::Error,
                detail: "Provider settings are invalid".into(),
            })
            .collect();
        let settings = self.services.settings();
        statuses.extend(
            [
                ("steamgriddb", "SteamGridDB", settings.steamgriddb_enabled),
                ("emotes", "FFZ and BTTV emotes", settings.emotes_enabled),
                ("pronouns", "Pronouns", settings.pronouns_enabled),
            ]
            .into_iter()
            .filter(|(_, _, enabled)| *enabled)
            .map(|(id, title, _)| {
                let (state, detail) = if let Err(error) = self.services.ready(id) {
                    (ConnectionState::Error, error.to_string())
                } else {
                    (ConnectionState::Connected, "Ready".to_owned())
                };
                ConnectionStatus {
                    integration: id.into(),
                    connection: "lookup".into(),
                    title: title.into(),
                    state,
                    detail,
                }
            }),
        );
        statuses
    }

    /// Wakes status observers after settings or credential administration.
    fn subscribe_configuration_changes(&self) -> Vec<watch::Receiver<u64>> {
        vec![self.changed.subscribe()]
    }
}

/// Loads each provider independently so one invalid document cannot disable siblings.
fn read_available(store: &ProviderSettingsStore) -> (ProviderSettings, [Option<String>; 3]) {
    let mut settings = ProviderSettings::default();
    let mut faults: [Option<String>; 3] = std::array::from_fn(|_| None);
    for provider in ProviderId::ALL {
        match store.load_provider(provider) {
            Ok(loaded) => provider.set_enabled(&mut settings, loaded.enabled),
            Err(error) => faults[provider.index()] = Some(error.to_string()),
        }
    }
    (settings, faults)
}

/// Human-facing provider name for a per-document configuration fault.
fn provider_title(provider: ProviderId) -> &'static str {
    match provider {
        ProviderId::SteamGridDb => "SteamGridDB",
        ProviderId::Emotes => "FFZ and BTTV emotes",
        ProviderId::Pronouns => "Pronouns",
    }
}

/// Opens the provider integration's native keyring account.
fn keyring_entry() -> Result<keyring::Entry, ProviderIntegrationError> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT)
        .map_err(|_| ProviderIntegrationError::Credential)
}

/// Reads the provider key from the operating system credential manager.
fn read_keyring_key() -> Result<Option<String>, ProviderIntegrationError> {
    match keyring_entry()?.get_password() {
        Ok(key) => Ok(Some(key)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(_) => Err(ProviderIntegrationError::Credential),
    }
}

/// Preserves conflict semantics when ConfigStore rejects a stale settings write.
fn map_settings_conflict(error: ProviderSettingsError) -> ProviderIntegrationError {
    match error {
        ProviderSettingsError::Store(StoreError::Conflict(_) | StoreError::AlreadyExists(_)) => {
            ProviderIntegrationError::Conflict
        }
        other => ProviderIntegrationError::Settings(other),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio_util::sync::CancellationToken;

    use crate::engine::{Engine, FormValidators, InputField, InputProvider, InputResponse, Values};
    use crate::integration::{ConfigurationStatus, ConnectionState};
    use crate::paths::AppPaths;

    use super::{
        CredentialCache, ProviderError, ProviderId, ProviderIntegration, ProviderSecrets,
        ProviderSettingsStore,
    };

    /// Unused form input for registering actions in an isolated startup test.
    struct NoInput;

    impl InputProvider for NoInput {
        /// Rejects form requests that the registration test should never make.
        fn request<'a>(
            &'a self,
            _title: String,
            _fields: Vec<InputField>,
            _defaults: Values,
            _validators: &'a FormValidators,
            _cancel: CancellationToken,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<InputResponse, String>> + Send + 'a>,
        > {
            Box::pin(async { Err("input unavailable".into()) })
        }
    }

    /// Confirms provider readiness never triggers an ambient keyring read.
    #[test]
    fn cached_credential_reads_never_contact_keyring() {
        let cache = CredentialCache::new();
        assert_eq!(cache.steamgriddb_key().unwrap(), None);
        assert!(cache.set(Some("test-key".into())));
        assert_eq!(cache.steamgriddb_key().unwrap(), Some("test-key".into()));
        assert!(!cache.set(Some("test-key".into())));
        assert!(cache.fail());
        assert!(matches!(
            cache.steamgriddb_key(),
            Err(ProviderError::CredentialUnavailable)
        ));
        assert!(cache.set(None));
        assert_eq!(cache.steamgriddb_key().unwrap(), None);
    }

    /// Sibling provider pages can save independently while stale edits of one switch conflict.
    #[test]
    fn scoped_provider_saves_preserve_sibling_switches() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AppPaths {
            config: directory.path().join("config"),
            data: directory.path().join("data"),
            state: directory.path().join("state"),
        };
        let mut engine = Engine::new(Arc::new(NoInput), Arc::new(|_| {}));
        let integration = ProviderIntegration::register(&paths, &mut engine).unwrap();
        assert!(integration.connection_statuses().is_empty());
        integration
            .save_enabled(ProviderId::Emotes, false, true)
            .unwrap();
        integration
            .save_enabled(ProviderId::Pronouns, false, true)
            .unwrap();
        let settings = integration.settings().unwrap();
        assert!(settings.emotes_enabled);
        assert!(settings.pronouns_enabled);
        assert!(!settings.steamgriddb_enabled);
        assert_eq!(integration.connection_statuses().len(), 2);
        assert!(matches!(
            integration.save_enabled(ProviderId::Emotes, false, false),
            Err(super::ProviderIntegrationError::Conflict)
        ));
        integration
            .save_enabled(ProviderId::Emotes, true, false)
            .unwrap();
        assert!(integration.configured(ProviderId::Emotes).unwrap());
        assert_eq!(integration.connection_statuses().len(), 1);
        assert!(integration.settings().unwrap().pronouns_enabled);
    }

    /// Keeps optional provider corruption from aborting workflow registration.
    #[test]
    fn corrupt_optional_settings_leave_actions_registered_and_file_untouched() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AppPaths {
            config: directory.path().join("config"),
            data: directory.path().join("data"),
            state: directory.path().join("state"),
        };
        let settings_dir = paths.integrations_dir().join("providers");
        std::fs::create_dir_all(&settings_dir).unwrap();
        let settings_file = settings_dir.join("providers.json");
        std::fs::write(&settings_file, "{invalid-json").unwrap();
        let mut engine = Engine::new(Arc::new(NoInput), Arc::new(|_| {}));

        let integration = ProviderIntegration::register(&paths, &mut engine).unwrap();

        assert_eq!(engine.action_schemas().len(), 5);
        assert_eq!(
            integration.connection_statuses()[0].state,
            ConnectionState::Error
        );
        assert!(integration.settings().is_err());
        assert_eq!(
            std::fs::read_to_string(settings_file).unwrap(),
            "{invalid-json"
        );
    }

    /// Invalid sibling settings do not prevent valid providers from running or saving.
    #[test]
    fn corrupt_sibling_leaves_healthy_provider_available() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AppPaths {
            config: directory.path().join("config"),
            data: directory.path().join("data"),
            state: directory.path().join("state"),
        };
        let store = ProviderSettingsStore::new(&paths);
        for provider in [ProviderId::Emotes, ProviderId::Pronouns] {
            let loaded = store.load_provider(provider).unwrap();
            store.save_provider(&loaded, provider, true).unwrap();
        }
        let steam_file = paths.integrations_dir().join("providers/steamgriddb.json");
        std::fs::write(&steam_file, "{invalid-json").unwrap();
        let mut engine = Engine::new(Arc::new(NoInput), Arc::new(|_| {}));
        let integration = ProviderIntegration::register(&paths, &mut engine).unwrap();

        assert_eq!(engine.action_schemas().len(), 5);
        assert!(integration.services.ready("emotes").is_ok());
        assert!(integration.services.ready("pronouns").is_ok());
        assert!(integration.enabled(ProviderId::Emotes).unwrap());
        assert!(integration.configured(ProviderId::Emotes).unwrap());
        assert!(integration.enabled(ProviderId::SteamGridDb).is_err());
        assert_eq!(integration.connection_statuses().len(), 3);
        integration
            .save_enabled(ProviderId::Emotes, true, false)
            .unwrap();
        assert!(!integration.enabled(ProviderId::Emotes).unwrap());
        assert!(integration.enabled(ProviderId::Pronouns).unwrap());
        assert_eq!(
            std::fs::read_to_string(steam_file).unwrap(),
            "{invalid-json"
        );
    }

    /// A sibling write by another store is reflected when this process saves its own provider.
    #[test]
    fn sibling_save_before_target_save_refreshes_runtime_view() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AppPaths {
            config: directory.path().join("config"),
            data: directory.path().join("data"),
            state: directory.path().join("state"),
        };
        let mut engine = Engine::new(Arc::new(NoInput), Arc::new(|_| {}));
        let integration = ProviderIntegration::register(&paths, &mut engine).unwrap();
        let other_store = ProviderSettingsStore::new(&paths);
        let sibling = other_store.load_provider(ProviderId::Pronouns).unwrap();
        other_store
            .save_provider(&sibling, ProviderId::Pronouns, true)
            .unwrap();
        assert!(!integration.services.settings().pronouns_enabled);
        integration
            .save_enabled(ProviderId::Emotes, false, true)
            .unwrap();
        let runtime = integration.services.settings();
        assert!(runtime.emotes_enabled);
        assert!(runtime.pronouns_enabled);
    }
}
