//! Persisted switches for the compiled public data providers.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::migration::StoredConfig;
use crate::paths::AppPaths;
use crate::schema::CredentialDescriptor;
use crate::schema::DescribeConfig;
use crate::storage::{ConfigSnapshot, ConfigStore, SaveOutcome, StoreError};

const DOCUMENT_ID: &str = "providers";
const DEFINITION: &str = "snenkbot.providers";
const VERSION: u32 = 1;

/// Native key editor copy for the SteamGridDB provider.
pub const STEAMGRIDDB_KEY_CREDENTIAL: CredentialDescriptor = CredentialDescriptor {
    id: "steamgriddb-key",
    label: "SteamGridDB API key",
    empty_description: "Add your SteamGridDB key to enable artwork requests.",
    stored_description: "A key is stored. Enter a new key to replace it.",
    unavailable_description: "Credential status is unavailable.",
    save_label: "Save key",
    saved_message: "Key saved.",
    save_error: "The key could not be stored.",
    remove_label: Some("Remove key"),
    removed_message: Some("Key removed."),
};

/// Enabled state for the compiled provider services and their actions.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, crate::ConfigSchema)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
#[serde(deny_unknown_fields)]
#[config(id = "snenkbot.providers", version = 1, title = "Data providers")]
pub struct ProviderSettings {
    #[config(id = "steamgriddb_enabled", label = "SteamGridDB", introduced = 1)]
    pub steamgriddb_enabled: bool,
    #[config(id = "emotes_enabled", label = "FFZ and BTTV emotes", introduced = 1)]
    pub emotes_enabled: bool,
    #[serde(default)]
    #[config(id = "ffz_enabled", label = "FrankerFaceZ", introduced = 1)]
    pub ffz_enabled: bool,
    #[serde(default)]
    #[config(id = "bttv_enabled", label = "BetterTTV", introduced = 1)]
    pub bttv_enabled: bool,
    #[config(id = "pronouns_enabled", label = "Pronouns", introduced = 1)]
    pub pronouns_enabled: bool,
}

/// The provider whose enabled switch a configuration session owns.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderId {
    SteamGridDb,
    Ffz,
    Bttv,
    Pronouns,
}

impl ProviderId {
    /// All compiled providers in stable display order.
    pub const ALL: [Self; 4] = [Self::SteamGridDb, Self::Ffz, Self::Bttv, Self::Pronouns];

    /// Stable document identifier for this independently configured provider.
    pub fn document_id(self) -> &'static str {
        match self {
            Self::SteamGridDb => SteamGridDbSettings::INTEGRATION.expect("provider integration ID"),
            Self::Ffz => FfzSettings::INTEGRATION.expect("provider integration ID"),
            Self::Bttv => BttvSettings::INTEGRATION.expect("provider integration ID"),
            Self::Pronouns => PronounsSettings::INTEGRATION.expect("provider integration ID"),
        }
    }

    /// Public title from this provider's portable settings schema.
    pub fn title(self) -> &'static str {
        match self {
            Self::SteamGridDb => SteamGridDbSettings::SCHEMA.title,
            Self::Ffz => FfzSettings::SCHEMA.title,
            Self::Bttv => BttvSettings::SCHEMA.title,
            Self::Pronouns => PronounsSettings::SCHEMA.title,
        }
    }

    /// Versioned schema identifier for this provider's document.
    fn definition(self) -> &'static str {
        match self {
            Self::SteamGridDb => "snenkbot.provider.steamgriddb",
            Self::Ffz => "snenkbot.provider.ffz",
            Self::Bttv => "snenkbot.provider.bttv",
            Self::Pronouns => "snenkbot.provider.pronouns",
        }
    }

    /// Index into the three fixed provider document snapshots.
    pub(crate) fn index(self) -> usize {
        match self {
            Self::SteamGridDb => 0,
            Self::Ffz => 1,
            Self::Bttv => 2,
            Self::Pronouns => 3,
        }
    }

    /// Reads only this provider's enabled switch from the shared persisted document.
    pub fn enabled(self, settings: &ProviderSettings) -> bool {
        match self {
            Self::SteamGridDb => settings.steamgriddb_enabled,
            Self::Ffz => settings.ffz_enabled || settings.emotes_enabled,
            Self::Bttv => settings.bttv_enabled || settings.emotes_enabled,
            Self::Pronouns => settings.pronouns_enabled,
        }
    }

    /// Changes only this provider's enabled switch in the shared persisted document.
    pub fn set_enabled(self, settings: &mut ProviderSettings, enabled: bool) {
        match self {
            Self::SteamGridDb => settings.steamgriddb_enabled = enabled,
            Self::Ffz => settings.ffz_enabled = enabled,
            Self::Bttv => settings.bttv_enabled = enabled,
            Self::Pronouns => settings.pronouns_enabled = enabled,
        }
    }
}

/// SteamGridDB configuration presented independently from other data providers.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, crate::ConfigSchema)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
#[serde(deny_unknown_fields)]
#[config(
    id = "snenkbot.provider.steamgriddb",
    version = 1,
    title = "SteamGridDB",
    credential = STEAMGRIDDB_KEY_CREDENTIAL,
    category = "devices",
    integration = "steamgriddb"
)]
pub struct SteamGridDbSettings {
    #[config(id = "enabled", label = "Enabled", introduced = 1)]
    pub enabled: bool,
}

/// FrankerFaceZ and BetterTTV emote configuration.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, crate::ConfigSchema)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
#[serde(deny_unknown_fields)]
#[config(
    id = "snenkbot.provider.emotes",
    version = 1,
    title = "FFZ and BTTV emotes",
    category = "devices",
    integration = "emotes"
)]
pub struct EmotesSettings {
    #[config(id = "enabled", label = "Enabled", introduced = 1)]
    pub enabled: bool,
}

/// FrankerFaceZ emote configuration.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, crate::ConfigSchema)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
#[serde(deny_unknown_fields)]
#[config(
    id = "snenkbot.provider.ffz",
    version = 1,
    title = "FrankerFaceZ",
    category = "devices",
    integration = "ffz"
)]
pub struct FfzSettings {
    #[config(id = "enabled", label = "Enabled", introduced = 1)]
    pub enabled: bool,
}

/// BetterTTV emote configuration.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, crate::ConfigSchema)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
#[serde(deny_unknown_fields)]
#[config(
    id = "snenkbot.provider.bttv",
    version = 1,
    title = "BetterTTV",
    category = "devices",
    integration = "bttv"
)]
pub struct BttvSettings {
    #[config(id = "enabled", label = "Enabled", introduced = 1)]
    pub enabled: bool,
}

/// Pronouns lookup configuration.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, crate::ConfigSchema)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
#[serde(deny_unknown_fields)]
#[config(
    id = "snenkbot.provider.pronouns",
    version = 1,
    title = "Pronouns",
    category = "devices",
    integration = "pronouns"
)]
pub struct PronounsSettings {
    #[config(id = "enabled", label = "Enabled", introduced = 1)]
    pub enabled: bool,
}

/// Errors while reading or writing provider switches.
#[derive(Debug, Error)]
pub enum ProviderSettingsError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("provider settings have an unsupported or invalid format")]
    InvalidFormat,
}

/// Provider settings plus the snapshot needed for a conflict-safe save.
#[derive(Clone)]
pub struct LoadedProviderSettings {
    pub value: ProviderSettings,
    configured: [bool; 4],
    snapshots: [Option<ConfigSnapshot>; 4],
}

impl LoadedProviderSettings {
    /// Reports an individual saved document or a migrated legacy combined setup.
    pub fn configured(&self, provider: ProviderId) -> bool {
        self.configured[provider.index()]
    }
}

/// One provider's enabled value and exact document revision, independent of siblings.
#[derive(Clone)]
pub struct LoadedProviderSetting {
    pub enabled: bool,
    pub configured: bool,
    snapshot: Option<ConfigSnapshot>,
}

/// Stores provider switches without storing API credentials in configuration files.
#[derive(Clone)]
pub struct ProviderSettingsStore {
    store: ConfigStore,
}

impl ProviderSettingsStore {
    /// Opens the provider settings document in the application's integration directory.
    pub fn new(paths: &AppPaths) -> Self {
        Self {
            store: ConfigStore::new(paths.integrations_dir().join("providers")),
        }
    }

    /// Loads one provider's document, consulting legacy settings only if it has no own document.
    pub fn load_provider(
        &self,
        provider: ProviderId,
    ) -> Result<LoadedProviderSetting, ProviderSettingsError> {
        match self.store.load(provider.document_id()) {
            Ok(snapshot) => Ok(LoadedProviderSetting {
                enabled: decode_scoped(provider, snapshot.config())?,
                configured: true,
                snapshot: Some(snapshot),
            }),
            Err(StoreError::NotFound(_)) => self.load_legacy(provider),
            Err(error) => Err(error.into()),
        }
    }

    /// Reads the previous shared emote document without changing its contents.
    fn load_legacy(
        &self,
        provider: ProviderId,
    ) -> Result<LoadedProviderSetting, ProviderSettingsError> {
        if matches!(provider, ProviderId::Ffz | ProviderId::Bttv) {
            match self.store.load("emotes") {
                Ok(snapshot) => {
                    let config = snapshot.config();
                    if config.definition != "snenkbot.provider.emotes" || config.version != VERSION
                    {
                        return Err(ProviderSettingsError::InvalidFormat);
                    }
                    let legacy: EmotesSettings = serde_json::from_value(config.data.clone())
                        .map_err(|_| ProviderSettingsError::InvalidFormat)?;
                    return Ok(LoadedProviderSetting {
                        enabled: legacy.enabled,
                        configured: true,
                        snapshot: None,
                    });
                }
                Err(StoreError::NotFound(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
        match self.store.load(DOCUMENT_ID) {
            Ok(snapshot) => {
                let config = snapshot.config();
                if config.definition != DEFINITION || config.version != VERSION {
                    return Err(ProviderSettingsError::InvalidFormat);
                }
                let legacy: ProviderSettings = serde_json::from_value(config.data.clone())
                    .map_err(|_| ProviderSettingsError::InvalidFormat)?;
                Ok(LoadedProviderSetting {
                    enabled: provider.enabled(&legacy),
                    configured: true,
                    snapshot: None,
                })
            }
            Err(StoreError::NotFound(_)) => Ok(LoadedProviderSetting {
                enabled: false,
                configured: false,
                snapshot: None,
            }),
            Err(error) => Err(error.into()),
        }
    }

    /// Collects all switches for callers that require an entirely healthy aggregate.
    pub fn load(&self) -> Result<LoadedProviderSettings, ProviderSettingsError> {
        let mut value = ProviderSettings::default();
        let mut configured = [false; 4];
        let mut snapshots: [Option<ConfigSnapshot>; 4] = std::array::from_fn(|_| None);
        for provider in ProviderId::ALL {
            let loaded = self.load_provider(provider)?;
            provider.set_enabled(&mut value, loaded.enabled);
            configured[provider.index()] = loaded.configured;
            snapshots[provider.index()] = loaded.snapshot;
        }
        Ok(LoadedProviderSettings {
            value,
            configured,
            snapshots,
        })
    }

    /// Saves only the selected provider's document using its loaded revision.
    pub fn save_enabled(
        &self,
        loaded: &LoadedProviderSettings,
        provider: ProviderId,
        enabled: bool,
    ) -> Result<SaveOutcome, ProviderSettingsError> {
        self.save_provider(
            &LoadedProviderSetting {
                enabled: provider.enabled(&loaded.value),
                configured: loaded.configured(provider),
                snapshot: loaded.snapshots[provider.index()].clone(),
            },
            provider,
            enabled,
        )
    }

    /// Writes only one provider's versioned document using its own revision.
    pub fn save_provider(
        &self,
        loaded: &LoadedProviderSetting,
        provider: ProviderId,
        enabled: bool,
    ) -> Result<SaveOutcome, ProviderSettingsError> {
        let config = StoredConfig {
            definition: provider.definition().into(),
            version: VERSION,
            data: encode_scoped(provider, enabled),
        };
        let validate = |config: &StoredConfig| {
            decode_scoped(provider, config)
                .map(|_| ())
                .map_err(|_| "invalid provider settings".into())
        };
        match loaded.snapshot.as_ref() {
            Some(snapshot) => {
                Ok(self
                    .store
                    .save(provider.document_id(), snapshot, &config, validate)?)
            }
            None => Ok(self
                .store
                .create(provider.document_id(), &config, validate)?),
        }
    }
}

/// Reads a provider's own versioned schema without accepting sibling fields.
fn decode_scoped(
    provider: ProviderId,
    config: &StoredConfig,
) -> Result<bool, ProviderSettingsError> {
    if config.definition != provider.definition() || config.version != VERSION {
        return Err(ProviderSettingsError::InvalidFormat);
    }
    let data = config.data.clone();
    match provider {
        ProviderId::SteamGridDb => {
            serde_json::from_value::<SteamGridDbSettings>(data).map(|value| value.enabled)
        }
        ProviderId::Ffz => serde_json::from_value::<FfzSettings>(data).map(|value| value.enabled),
        ProviderId::Bttv => serde_json::from_value::<BttvSettings>(data).map(|value| value.enabled),
        ProviderId::Pronouns => {
            serde_json::from_value::<PronounsSettings>(data).map(|value| value.enabled)
        }
    }
    .map_err(|_| ProviderSettingsError::InvalidFormat)
}

/// Serializes through the same Rust type that declares the native editor schema.
fn encode_scoped(provider: ProviderId, enabled: bool) -> serde_json::Value {
    match provider {
        ProviderId::SteamGridDb => serde_json::to_value(SteamGridDbSettings { enabled }),
        ProviderId::Ffz => serde_json::to_value(FfzSettings { enabled }),
        ProviderId::Bttv => serde_json::to_value(BttvSettings { enabled }),
        ProviderId::Pronouns => serde_json::to_value(PronounsSettings { enabled }),
    }
    .expect("provider settings serialize")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A disabled provider remains configured after saving its own document.
    #[test]
    fn scoped_documents_track_configuration_without_marking_siblings() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AppPaths {
            config: directory.path().join("config"),
            data: directory.path().join("data"),
            state: directory.path().join("state"),
        };
        let store = ProviderSettingsStore::new(&paths);
        let initial = store.load().unwrap();
        assert!(!initial.configured(ProviderId::Ffz));
        store
            .save_enabled(&initial, ProviderId::Ffz, false)
            .unwrap();
        let saved = store.load().unwrap();
        assert!(saved.configured(ProviderId::Ffz));
        assert!(!saved.configured(ProviderId::SteamGridDb));
        assert!(!saved.configured(ProviderId::Pronouns));
        assert!(!saved.value.ffz_enabled);
        store
            .save_enabled(&initial, ProviderId::Pronouns, true)
            .unwrap();
        let merged = store.load().unwrap();
        assert!(merged.configured(ProviderId::Pronouns));
        assert!(merged.value.pronouns_enabled);
        assert!(matches!(
            store.save_enabled(&initial, ProviderId::Ffz, true),
            Err(ProviderSettingsError::Store(StoreError::AlreadyExists(_)))
        ));
    }

    /// Existing combined settings remain readable and are never rewritten by scoped saves.
    #[test]
    fn legacy_combined_document_falls_back_until_each_provider_is_saved() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AppPaths {
            config: directory.path().join("config"),
            data: directory.path().join("data"),
            state: directory.path().join("state"),
        };
        let config_store = ConfigStore::new(paths.integrations_dir().join("providers"));
        let legacy = StoredConfig {
            definition: DEFINITION.into(),
            version: VERSION,
            data: serde_json::to_value(ProviderSettings {
                steamgriddb_enabled: true,
                emotes_enabled: false,
                pronouns_enabled: true,
                ..Default::default()
            })
            .unwrap(),
        };
        config_store
            .create(DOCUMENT_ID, &legacy, |_| Ok(()))
            .unwrap();
        let store = ProviderSettingsStore::new(&paths);
        let loaded = store.load().unwrap();
        for provider in [
            ProviderId::SteamGridDb,
            ProviderId::Ffz,
            ProviderId::Pronouns,
        ] {
            assert!(loaded.configured(provider));
        }
        assert!(loaded.value.steamgriddb_enabled);
        store.save_enabled(&loaded, ProviderId::Ffz, true).unwrap();
        let current = store.load().unwrap();
        assert!(current.value.ffz_enabled);
        assert!(current.value.steamgriddb_enabled);
        assert!(current.value.pronouns_enabled);
        assert_eq!(config_store.load(DOCUMENT_ID).unwrap().config(), &legacy);
    }

    /// A valid scoped document remains editable even when the legacy fallback is damaged.
    #[test]
    fn scoped_provider_ignores_corrupt_legacy_fallback() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AppPaths {
            config: directory.path().join("config"),
            data: directory.path().join("data"),
            state: directory.path().join("state"),
        };
        let store = ProviderSettingsStore::new(&paths);
        let emotes = store.load_provider(ProviderId::Ffz).unwrap();
        store.save_provider(&emotes, ProviderId::Ffz, true).unwrap();
        let legacy_file = paths.integrations_dir().join("providers/providers.json");
        std::fs::write(&legacy_file, "{invalid-json").unwrap();

        assert!(store.load_provider(ProviderId::Ffz).unwrap().enabled);
        assert!(store.load_provider(ProviderId::Pronouns).is_err());
        let emotes = store.load_provider(ProviderId::Ffz).unwrap();
        store
            .save_provider(&emotes, ProviderId::Ffz, false)
            .unwrap();
        assert!(!store.load_provider(ProviderId::Ffz).unwrap().enabled);
        assert_eq!(
            std::fs::read_to_string(legacy_file).unwrap(),
            "{invalid-json"
        );
    }
    /// Each emote provider inherits old setup until its own document overrides that switch.
    #[test]
    fn split_emotes_preserve_legacy_and_save_independently() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AppPaths {
            config: directory.path().join("config"),
            data: directory.path().join("data"),
            state: directory.path().join("state"),
        };
        let config_store = ConfigStore::new(paths.integrations_dir().join("providers"));
        let legacy = StoredConfig {
            definition: "snenkbot.provider.emotes".into(),
            version: 1,
            data: serde_json::json!({"enabled": true}),
        };
        config_store.create("emotes", &legacy, |_| Ok(())).unwrap();
        let store = ProviderSettingsStore::new(&paths);
        assert!(store.load_provider(ProviderId::Ffz).unwrap().enabled);
        assert!(store.load_provider(ProviderId::Bttv).unwrap().enabled);
        let ffz = store.load_provider(ProviderId::Ffz).unwrap();
        store.save_provider(&ffz, ProviderId::Ffz, false).unwrap();
        assert!(!store.load_provider(ProviderId::Ffz).unwrap().enabled);
        assert!(store.load_provider(ProviderId::Bttv).unwrap().enabled);
        let bttv = store.load_provider(ProviderId::Bttv).unwrap();
        store.save_provider(&bttv, ProviderId::Bttv, false).unwrap();
        assert!(!store.load_provider(ProviderId::Bttv).unwrap().enabled);
        assert_eq!(config_store.load("emotes").unwrap().config(), &legacy);
    }
}
