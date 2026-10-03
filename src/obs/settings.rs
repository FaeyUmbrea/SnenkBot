use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::migration::StoredConfig;
use crate::paths::AppPaths;
use crate::storage::{ConfigSnapshot, ConfigStore, SaveOutcome, StoreError};

const DOCUMENT_ID: &str = "connection";
const DEFINITION: &str = "snenkbot.obs.connection";
const VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, crate::ConfigSchema)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
#[serde(deny_unknown_fields)]
#[config(id = "snenkbot.obs.connection", version = 1, title = "OBS Studio")]
pub struct ObsSettings {
    #[config(id = "enabled", label = "Enabled", introduced = 1)]
    pub enabled: bool,
    #[config(id = "host", label = "Host", introduced = 1)]
    pub host: String,
    #[config(id = "port", label = "Port", introduced = 1)]
    pub port: u16,
    #[config(id = "tls", label = "Secure connection", introduced = 1)]
    pub tls: bool,
}

impl Default for ObsSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            host: "127.0.0.1".into(),
            port: 4455,
            tls: false,
        }
    }
}

#[derive(Debug, Error)]
pub enum ObsSettingsError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("OBS settings have an unsupported or invalid format")]
    InvalidFormat,
}

#[derive(Clone)]
pub struct LoadedObsSettings {
    pub value: ObsSettings,
    snapshot: Option<ConfigSnapshot>,
}

impl LoadedObsSettings {
    pub(crate) fn is_configured(&self) -> bool {
        self.snapshot.is_some()
    }
}

#[derive(Clone)]
pub struct ObsSettingsStore {
    store: ConfigStore,
}

impl ObsSettingsStore {
    pub fn new(paths: &AppPaths) -> Self {
        Self {
            store: ConfigStore::new(paths.integrations_dir().join("obs")),
        }
    }

    pub fn load(&self) -> Result<LoadedObsSettings, ObsSettingsError> {
        match self.store.load(DOCUMENT_ID) {
            Ok(snapshot) => Ok(LoadedObsSettings {
                value: decode(snapshot.config())?,
                snapshot: Some(snapshot),
            }),
            Err(StoreError::NotFound(_)) => Ok(LoadedObsSettings {
                value: ObsSettings::default(),
                snapshot: None,
            }),
            Err(error) => Err(error.into()),
        }
    }

    pub fn save(
        &self,
        loaded: &LoadedObsSettings,
        value: ObsSettings,
    ) -> Result<SaveOutcome, ObsSettingsError> {
        let config = StoredConfig {
            definition: DEFINITION.into(),
            version: VERSION,
            data: serde_json::to_value(value).expect("OBS settings serialize"),
        };
        match loaded.snapshot.as_ref() {
            Some(snapshot) => Ok(self.store.save(DOCUMENT_ID, snapshot, &config, validate)?),
            None => Ok(self.store.create(DOCUMENT_ID, &config, validate)?),
        }
    }
}

fn decode(config: &StoredConfig) -> Result<ObsSettings, ObsSettingsError> {
    if config.definition != DEFINITION || config.version != VERSION {
        return Err(ObsSettingsError::InvalidFormat);
    }
    let value: ObsSettings =
        serde_json::from_value(config.data.clone()).map_err(|_| ObsSettingsError::InvalidFormat)?;
    validate_value(&value).map_err(|_| ObsSettingsError::InvalidFormat)?;
    Ok(value)
}

fn validate(config: &StoredConfig) -> Result<(), String> {
    if config.definition != DEFINITION || config.version != VERSION {
        return Err("unsupported OBS settings version".into());
    }
    let value: ObsSettings = serde_json::from_value(config.data.clone())
        .map_err(|error| format!("invalid OBS settings: {error}"))?;
    validate_value(&value)
}

fn validate_value(value: &ObsSettings) -> Result<(), String> {
    if value.host.is_empty() {
        return Err("OBS host is required".into());
    }
    if value.host.len() > 253
        || value.host.chars().any(|character| {
            !(character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | ':' | '[' | ']'))
        })
    {
        return Err("OBS host must be a hostname or IP address, without a scheme or path".into());
    }
    if value.port == 0 {
        return Err("OBS port must be between 1 and 65535".into());
    }
    if !value.tls && !is_local_address(&value.host) {
        return Err(
            "Plain OBS connections require localhost or a private network address; use Secure connection for public hosts".into(),
        );
    }
    Ok(())
}

fn is_local_address(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let ip = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);
    match ip.parse::<IpAddr>() {
        Ok(IpAddr::V4(address)) => {
            address.is_loopback() || address.is_private() || address.is_link_local()
        }
        Ok(IpAddr::V6(address)) => {
            address.is_loopback() || address.is_unique_local() || address.is_unicast_link_local()
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn defaults_and_saves_without_credentials() {
        let root = tempdir().unwrap();
        let paths = AppPaths {
            config: root.path().join("config"),
            data: root.path().join("data"),
            state: root.path().join("state"),
        };
        let store = ObsSettingsStore::new(&paths);
        let initial = store.load().unwrap();
        assert_eq!(initial.value, ObsSettings::default());
        assert!(!initial.is_configured());
        store
            .save(
                &initial,
                ObsSettings {
                    enabled: true,
                    host: "localhost".into(),
                    port: 4456,
                    tls: false,
                },
            )
            .unwrap();
        let saved = store.load().unwrap();
        assert!(saved.is_configured());
        assert!(saved.value.enabled);
        assert_eq!(saved.value.port, 4456);
        assert_eq!(saved.value.host, "localhost");
    }

    #[test]
    fn rejects_invalid_host_without_publishing() {
        let root = tempdir().unwrap();
        let paths = AppPaths {
            config: root.path().join("config"),
            data: root.path().join("data"),
            state: root.path().join("state"),
        };
        let store = ObsSettingsStore::new(&paths);
        let initial = store.load().unwrap();
        assert!(
            store
                .save(
                    &initial,
                    ObsSettings {
                        host: "localhost/path".into(),
                        ..ObsSettings::default()
                    },
                )
                .is_err()
        );
        assert_eq!(store.load().unwrap().value, ObsSettings::default());
    }

    #[test]
    fn permits_private_lan_address_without_tls_and_rejects_public_address() {
        let root = tempdir().unwrap();
        let paths = AppPaths {
            config: root.path().join("config"),
            data: root.path().join("data"),
            state: root.path().join("state"),
        };
        let store = ObsSettingsStore::new(&paths);
        let initial = store.load().unwrap();
        let local = ObsSettings {
            enabled: true,
            host: "192.168.178.44".into(),
            port: 4455,
            tls: false,
        };
        store.save(&initial, local.clone()).unwrap();
        assert_eq!(store.load().unwrap().value, local);

        let saved = store.load().unwrap();
        let public = ObsSettings {
            host: "8.8.8.8".into(),
            ..local
        };
        let error = store.save(&saved, public).unwrap_err().to_string();
        assert!(error.contains("private network address"), "{error}");
        assert_eq!(store.load().unwrap().value.host, "192.168.178.44");
    }

    #[test]
    fn plain_connection_boundary_accepts_only_local_ip_ranges() {
        for host in [
            "localhost",
            "LOCALHOST",
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "[::1]",
            "fd12::1",
        ] {
            assert!(is_local_address(host), "{host}");
        }
        for host in [
            "8.8.8.8",
            "1.1.1.1",
            "2001:4860:4860::8888",
            "obs.example.com",
            "[[::1]]",
        ] {
            assert!(!is_local_address(host), "{host}");
        }
    }
}
