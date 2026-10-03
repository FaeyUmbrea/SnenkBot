//! Portable VTube Studio connection settings. Plugin tokens stay in the OS credential store.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::migration::StoredConfig;
use crate::paths::AppPaths;
use crate::storage::{ConfigSnapshot, ConfigStore, SaveOutcome, StoreError};

const DOCUMENT_ID: &str = "connection";
const DEFINITION: &str = "snenkbot.vtube.connection";
const VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, crate::ConfigSchema)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
#[serde(deny_unknown_fields)]
#[config(id = "snenkbot.vtube.connection", version = 1, title = "VTube Studio")]
pub struct VtubeSettings {
    #[config(id = "enabled", label = "Enabled", introduced = 1)]
    pub enabled: bool,
    #[config(id = "host", label = "Host", introduced = 1)]
    pub host: String,
    #[config(id = "port", label = "Port", introduced = 1)]
    pub port: u16,
}

impl Default for VtubeSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            host: "127.0.0.1".into(),
            port: 8001,
        }
    }
}

impl VtubeSettings {
    pub fn endpoint(&self) -> String {
        let host = match self.host.parse::<IpAddr>() {
            Ok(IpAddr::V6(_)) => format!("[{}]", self.host),
            _ => self.host.clone(),
        };
        format!("ws://{host}:{}", self.port)
    }
}

#[derive(Debug, Error)]
pub enum VtubeSettingsError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("VTube Studio settings have an unsupported or invalid format")]
    InvalidFormat,
}

#[derive(Clone)]
pub struct LoadedVtubeSettings {
    pub value: VtubeSettings,
    snapshot: Option<ConfigSnapshot>,
}

impl LoadedVtubeSettings {
    pub(crate) fn is_configured(&self) -> bool {
        self.snapshot.is_some()
    }
}

#[derive(Clone)]
pub struct VtubeSettingsStore {
    store: ConfigStore,
}

impl VtubeSettingsStore {
    pub fn new(paths: &AppPaths) -> Self {
        Self {
            store: ConfigStore::new(paths.integrations_dir().join("vtube-studio")),
        }
    }

    pub fn load(&self) -> Result<LoadedVtubeSettings, VtubeSettingsError> {
        match self.store.load(DOCUMENT_ID) {
            Ok(snapshot) => Ok(LoadedVtubeSettings {
                value: decode(snapshot.config())?,
                snapshot: Some(snapshot),
            }),
            Err(StoreError::NotFound(_)) => Ok(LoadedVtubeSettings {
                value: VtubeSettings::default(),
                snapshot: None,
            }),
            Err(error) => Err(error.into()),
        }
    }

    pub fn save(
        &self,
        loaded: &LoadedVtubeSettings,
        value: VtubeSettings,
    ) -> Result<SaveOutcome, VtubeSettingsError> {
        let config = StoredConfig {
            definition: DEFINITION.into(),
            version: VERSION,
            data: serde_json::to_value(value).expect("VTube Studio settings serialize"),
        };
        match loaded.snapshot.as_ref() {
            Some(snapshot) => Ok(self.store.save(DOCUMENT_ID, snapshot, &config, validate)?),
            None => Ok(self.store.create(DOCUMENT_ID, &config, validate)?),
        }
    }
}

fn decode(config: &StoredConfig) -> Result<VtubeSettings, VtubeSettingsError> {
    if config.definition != DEFINITION || config.version != VERSION {
        return Err(VtubeSettingsError::InvalidFormat);
    }
    let value: VtubeSettings = serde_json::from_value(config.data.clone())
        .map_err(|_| VtubeSettingsError::InvalidFormat)?;
    validate_value(&value).map_err(|_| VtubeSettingsError::InvalidFormat)?;
    Ok(value)
}

fn validate(config: &StoredConfig) -> Result<(), String> {
    if config.definition != DEFINITION || config.version != VERSION {
        return Err("unsupported VTube Studio settings version".into());
    }
    let value: VtubeSettings = serde_json::from_value(config.data.clone())
        .map_err(|error| format!("invalid VTube Studio settings: {error}"))?;
    validate_value(&value)
}

fn validate_value(value: &VtubeSettings) -> Result<(), String> {
    if value.port == 0 {
        return Err("VTube Studio port must be between 1 and 65535".into());
    }
    if value.host.eq_ignore_ascii_case("localhost") {
        return Ok(());
    }
    let address = value
        .host
        .parse::<IpAddr>()
        .map_err(|_| "VTube Studio host must be localhost or a loopback IP address".to_owned())?;
    if !address.is_loopback() {
        return Err("VTube Studio host must be a loopback address".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn saves_local_endpoint_without_token_and_rejects_remote_hosts() {
        let root = tempdir().unwrap();
        let paths = AppPaths {
            config: root.path().join("config"),
            data: root.path().join("data"),
            state: root.path().join("state"),
        };
        let store = VtubeSettingsStore::new(&paths);
        let loaded = store.load().unwrap();
        assert_eq!(loaded.value.endpoint(), "ws://127.0.0.1:8001");
        assert!(!loaded.is_configured());
        store
            .save(
                &loaded,
                VtubeSettings {
                    enabled: true,
                    host: "::1".into(),
                    port: 9000,
                },
            )
            .unwrap();
        let saved = store.load().unwrap();
        assert!(saved.is_configured());
        assert_eq!(saved.value.endpoint(), "ws://[::1]:9000");
        let current = store.load().unwrap();
        assert!(
            store
                .save(
                    &current,
                    VtubeSettings {
                        enabled: true,
                        host: "192.168.1.10".into(),
                        port: 8001,
                    }
                )
                .is_err()
        );
        assert_eq!(store.load().unwrap().value.host, "::1");
    }
}
