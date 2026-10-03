//! Connection configuration sessions retain the original atomic-storage snapshot.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use specta::Type;
use uuid::Uuid;

use crate::commands::SaveNotice;
use snenk_bot::app::AppServices;
use snenk_bot::engine::Values;
use snenk_bot::obs::{
    ObsConfigurationError,
    settings::{LoadedObsSettings, ObsSettings, ObsSettingsError},
};
use snenk_bot::schema::{ConfigSchema, DescribeConfig};
use snenk_bot::storage::{SaveOutcome, SaveWarning, StoreError};
use snenk_bot::vtube::{
    VtubeConfigurationError,
    settings::{LoadedVtubeSettings, VtubeSettings, VtubeSettingsError},
};

const SESSION_CAPACITY: usize = 64;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionModule {
    Obs,
    VtubeStudio,
}

#[derive(Debug, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct OpenConfiguration {
    pub module: ConnectionModule,
}

#[derive(Debug, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationSessionRequest {
    pub session_id: String,
}

#[derive(Debug, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct SaveConfiguration {
    pub session_id: String,
    #[specta(type = specta_typescript::Number)]
    pub expected_revision: u64,
    #[specta(type = BTreeMap<String, specta_typescript::Unknown>)]
    pub values: Values,
}

#[derive(Debug, Serialize, Type)]
pub struct ConfigurationSnapshot {
    pub session_id: String,
    #[specta(type = specta_typescript::Number)]
    pub revision: u64,
    pub module: ConnectionModule,
    pub schema: ConfigSchema,
    #[specta(type = BTreeMap<String, specta_typescript::Unknown>)]
    pub defaults: Values,
    #[specta(type = BTreeMap<String, specta_typescript::Unknown>)]
    pub values: Values,
}

#[derive(Debug, Serialize, Type)]
pub struct ConfigurationSaveResult {
    pub snapshot: ConfigurationSnapshot,
    pub notices: Vec<SaveNotice>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationErrorCode {
    Unavailable,
    Busy,
    MissingSession,
    StaleRevision,
    InvalidValues,
    SaveConflict,
    SaveFailed,
    RevisionExhausted,
}

#[derive(Debug, Serialize, Type)]
pub struct ConfigurationError {
    pub code: ConfigurationErrorCode,
    pub message: String,
}

impl ConfigurationError {
    pub(crate) fn new(code: ConfigurationErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    pub(crate) fn unavailable() -> Self {
        Self::new(
            ConfigurationErrorCode::Unavailable,
            "Connection settings are unavailable.",
        )
    }
}

pub(crate) enum LoadedConnectionSettings {
    Obs(LoadedObsSettings),
    VtubeStudio(LoadedVtubeSettings),
}

struct ConfigurationSession {
    revision: u64,
    loaded: LoadedConnectionSettings,
}

#[derive(Default)]
pub(crate) struct ConfigurationSessions {
    sessions: HashMap<String, ConfigurationSession>,
}

impl ConfigurationSessions {
    pub fn open(
        &mut self,
        loaded: LoadedConnectionSettings,
    ) -> Result<ConfigurationSnapshot, ConfigurationError> {
        if self.sessions.len() >= SESSION_CAPACITY {
            return Err(ConfigurationError::new(
                ConfigurationErrorCode::Busy,
                "Close an existing settings view before opening another.",
            ));
        }
        let id = Uuid::new_v4().to_string();
        let session = ConfigurationSession {
            revision: 0,
            loaded,
        };
        let snapshot = session.snapshot(&id)?;
        self.sessions.insert(id, session);
        Ok(snapshot)
    }

    pub fn close(&mut self, id: &str) -> Result<(), ConfigurationError> {
        self.sessions
            .remove(id)
            .map(|_| ())
            .ok_or_else(missing_session)
    }

    pub fn save(
        &mut self,
        request: SaveConfiguration,
        services: &AppServices,
    ) -> Result<ConfigurationSaveResult, ConfigurationError> {
        self.save_with(request, |original, values| match original {
            LoadedConnectionSettings::Obs(original) => {
                let value = parse_values::<ObsSettings>(values)?;
                let outcome = services
                    .save_obs_settings_typed(original.clone(), value.clone())
                    .map_err(|_| ConfigurationError::unavailable())?
                    .blocking_recv()
                    .map_err(|_| ConfigurationError::unavailable())?
                    .map_err(obs_error)?;
                let saved = services
                    .obs()
                    .settings()
                    .load()
                    .map_err(|error| obs_error(error.into()))?;
                if saved.value != value {
                    return Err(readback_conflict());
                }
                Ok((LoadedConnectionSettings::Obs(saved), outcome))
            }
            LoadedConnectionSettings::VtubeStudio(original) => {
                let value = parse_values::<VtubeSettings>(values)?;
                let outcome = services
                    .save_vtube_settings_typed(original.clone(), value.clone())
                    .map_err(|_| ConfigurationError::unavailable())?
                    .blocking_recv()
                    .map_err(|_| ConfigurationError::unavailable())?
                    .map_err(vtube_error)?;
                let saved = services
                    .vtube()
                    .settings()
                    .load()
                    .map_err(|error| vtube_error(error.into()))?;
                if saved.value != value {
                    return Err(readback_conflict());
                }
                Ok((LoadedConnectionSettings::VtubeStudio(saved), outcome))
            }
        })
    }

    fn save_with(
        &mut self,
        request: SaveConfiguration,
        persist: impl FnOnce(
            &LoadedConnectionSettings,
            Values,
        )
            -> Result<(LoadedConnectionSettings, SaveOutcome), ConfigurationError>,
    ) -> Result<ConfigurationSaveResult, ConfigurationError> {
        let session = self
            .sessions
            .get_mut(&request.session_id)
            .ok_or_else(missing_session)?;
        if session.revision != request.expected_revision {
            return Err(ConfigurationError::new(
                ConfigurationErrorCode::StaleRevision,
                "This settings view has changed. Use its current values.",
            ));
        }
        let revision = session.revision.checked_add(1).ok_or_else(|| {
            ConfigurationError::new(
                ConfigurationErrorCode::RevisionExhausted,
                "Reopen this settings view before continuing.",
            )
        })?;
        let (saved, outcome) = persist(&session.loaded, request.values)?;
        // Validate the response before replacing the session's recovery snapshot.
        let candidate = ConfigurationSession {
            revision,
            loaded: saved,
        };
        let snapshot = candidate.snapshot(&request.session_id)?;
        *session = candidate;
        Ok(ConfigurationSaveResult {
            snapshot,
            notices: outcome
                .warnings
                .into_iter()
                .map(|warning| match warning {
                    SaveWarning::DirectorySync(_) => SaveNotice::DirectorySync,
                    SaveWarning::BackupRetention(_) => SaveNotice::BackupRetention,
                })
                .collect(),
        })
    }
}

impl ConfigurationSession {
    fn snapshot(&self, id: &str) -> Result<ConfigurationSnapshot, ConfigurationError> {
        let (module, schema, defaults, values) = match &self.loaded {
            LoadedConnectionSettings::Obs(loaded) => (
                ConnectionModule::Obs,
                *ObsSettings::SCHEMA,
                to_values(&ObsSettings::default())?,
                to_values(&loaded.value)?,
            ),
            LoadedConnectionSettings::VtubeStudio(loaded) => (
                ConnectionModule::VtubeStudio,
                *VtubeSettings::SCHEMA,
                to_values(&VtubeSettings::default())?,
                to_values(&loaded.value)?,
            ),
        };
        Ok(ConfigurationSnapshot {
            session_id: id.into(),
            revision: self.revision,
            module,
            schema,
            defaults,
            values,
        })
    }
}

pub(crate) fn to_values(value: &impl Serialize) -> Result<Values, ConfigurationError> {
    serde_json::to_value(value)
        .and_then(serde_json::from_value)
        .map_err(|_| ConfigurationError::unavailable())
}

fn parse_values<T: serde::de::DeserializeOwned>(values: Values) -> Result<T, ConfigurationError> {
    serde_json::from_value(serde_json::Value::Object(values.into_iter().collect())).map_err(|_| {
        ConfigurationError::new(
            ConfigurationErrorCode::InvalidValues,
            "Check the connection fields and enter a valid port number.",
        )
    })
}

fn missing_session() -> ConfigurationError {
    ConfigurationError::new(
        ConfigurationErrorCode::MissingSession,
        "This settings view has closed. Reopen it to continue.",
    )
}
fn readback_conflict() -> ConfigurationError {
    ConfigurationError::new(
        ConfigurationErrorCode::SaveConflict,
        "Connection settings changed while saving. Reload them before continuing.",
    )
}
fn store_error(error: StoreError) -> ConfigurationError {
    match error {
        StoreError::Conflict(_) | StoreError::AlreadyExists(_) => readback_conflict(),
        StoreError::Validation { message, .. } => {
            ConfigurationError::new(ConfigurationErrorCode::InvalidValues, message)
        }
        error => {
            tracing::error!(%error, "connection configuration storage failed");
            ConfigurationError::new(
                ConfigurationErrorCode::SaveFailed,
                "Connection settings could not be saved. Your entries are still available.",
            )
        }
    }
}
pub(crate) fn obs_error(error: ObsConfigurationError) -> ConfigurationError {
    match error {
        ObsConfigurationError::Settings(ObsSettingsError::Store(error)) => store_error(error),
        ObsConfigurationError::Credential(error) => {
            tracing::error!(%error, "OBS password persistence failed");
            ConfigurationError::new(
                ConfigurationErrorCode::SaveFailed,
                "The OBS password could not be stored. Your entry is still available.",
            )
        }
        error => {
            tracing::error!(%error, "OBS configuration failed");
            ConfigurationError::unavailable()
        }
    }
}
pub(crate) fn vtube_error(error: VtubeConfigurationError) -> ConfigurationError {
    match error {
        VtubeConfigurationError::Settings(VtubeSettingsError::Store(error)) => store_error(error),
        VtubeConfigurationError::Busy => ConfigurationError::new(
            ConfigurationErrorCode::Busy,
            "VTube Studio configuration is already in progress.",
        ),
        error => {
            tracing::error!(%error, "VTube Studio configuration failed");
            ConfigurationError::unavailable()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snenk_bot::obs::settings::ObsSettingsStore;
    use snenk_bot::paths::AppPaths;

    #[test]
    fn settings_saves_preserve_original_conflicts_and_do_not_replace_failed_sessions() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AppPaths {
            config: directory.path().into(),
            data: directory.path().into(),
            state: directory.path().into(),
        };
        let store = ObsSettingsStore::new(&paths);
        let mut sessions = ConfigurationSessions::default();
        let snapshot = sessions
            .open(LoadedConnectionSettings::Obs(store.load().unwrap()))
            .unwrap();
        let value = ObsSettings {
            host: "192.168.178.44".into(),
            ..ObsSettings::default()
        };
        let request = || SaveConfiguration {
            session_id: snapshot.session_id.clone(),
            expected_revision: 0,
            values: to_values(&value).unwrap(),
        };
        let original = store.load().unwrap();
        store.save(&original, ObsSettings::default()).unwrap();
        let error = sessions
            .save_with(request(), |original, values| {
                let LoadedConnectionSettings::Obs(original) = original else {
                    unreachable!()
                };
                let outcome = store
                    .save(original, parse_values(values)?)
                    .map_err(|error| obs_error(error.into()))?;
                Ok((
                    LoadedConnectionSettings::Obs(store.load().unwrap()),
                    outcome,
                ))
            })
            .unwrap_err();
        assert_eq!(error.code, ConfigurationErrorCode::SaveConflict);
        assert_eq!(sessions.sessions[&snapshot.session_id].revision, 0);
        assert_eq!(store.load().unwrap().value, ObsSettings::default());
        let error = sessions
            .save_with(
                SaveConfiguration {
                    expected_revision: 1,
                    ..request()
                },
                |_, _| panic!("stale request must not write"),
            )
            .unwrap_err();
        assert_eq!(error.code, ConfigurationErrorCode::StaleRevision);
    }

    #[test]
    fn successful_save_refreshes_baseline_and_unknown_values_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AppPaths {
            config: directory.path().into(),
            data: directory.path().into(),
            state: directory.path().into(),
        };
        let store = ObsSettingsStore::new(&paths);
        let mut sessions = ConfigurationSessions::default();
        let snapshot = sessions
            .open(LoadedConnectionSettings::Obs(store.load().unwrap()))
            .unwrap();
        let result = sessions
            .save_with(
                SaveConfiguration {
                    session_id: snapshot.session_id.clone(),
                    expected_revision: 0,
                    values: snapshot.values,
                },
                |original, values| {
                    let LoadedConnectionSettings::Obs(original) = original else {
                        unreachable!()
                    };
                    let outcome = store
                        .save(original, parse_values(values)?)
                        .map_err(|error| obs_error(error.into()))?;
                    Ok((
                        LoadedConnectionSettings::Obs(store.load().unwrap()),
                        outcome,
                    ))
                },
            )
            .unwrap();
        assert_eq!(result.snapshot.revision, 1);
        let mut values = result.snapshot.values;
        values.insert("password".into(), serde_json::json!("not portable"));
        assert_eq!(
            parse_values::<ObsSettings>(values).unwrap_err().code,
            ConfigurationErrorCode::InvalidValues
        );
        sessions.close(&snapshot.session_id).unwrap();
        assert_eq!(
            sessions.close(&snapshot.session_id).unwrap_err().code,
            ConfigurationErrorCode::MissingSession
        );
    }

    #[test]
    fn private_storage_diagnostics_stay_out_of_ipc() {
        let error = store_error(StoreError::Io {
            path: "/private/config".into(),
            source: std::io::Error::other("private details"),
        });
        assert_eq!(error.code, ConfigurationErrorCode::SaveFailed);
        assert!(!error.message.contains("/private"));
        assert!(!error.message.contains("private details"));
    }
}
