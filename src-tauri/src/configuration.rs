//! Connection configuration sessions retain the original atomic-storage snapshot.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use specta::Type;
use uuid::Uuid;

use crate::api_admin::CreateApiConsumer;
use crate::commands::SaveNotice;
use snenk_bot::api::{ApiError, ApiSettings, ConsumerSummary};
use snenk_bot::app::AppServices;
use snenk_bot::engine::Values;
use snenk_bot::obs::{
    ObsConfigurationError,
    settings::{LoadedObsSettings, ObsSettings, ObsSettingsError},
};
use snenk_bot::providers::{
    BttvSettings, FfzSettings, PronounsSettings, ProviderId, ProviderIntegrationError,
    ProviderSettingsError, SteamGridDbSettings,
};
pub use snenk_bot::schema::ConfigurationCategory;
use snenk_bot::schema::{ConfigSchema, DescribeConfig};
use snenk_bot::storage::{SaveOutcome, SaveWarning, StoreError};
use snenk_bot::vtube::{
    VtubeConfigurationError,
    settings::{LoadedVtubeSettings, VtubeSettings, VtubeSettingsError},
};

const SESSION_CAPACITY: usize = 64;

/// Native configuration owner selected by the desktop settings directory.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionModule {
    /// OBS Studio connection settings.
    Obs,
    /// VTube Studio connection settings.
    VtubeStudio,
    /// Overlay API listener or one consumer record.
    Api,
    /// SteamGridDB artwork lookup settings.
    Steamgriddb,
    /// FrankerFaceZ emote lookup settings.
    Ffz,
    /// BetterTTV emote lookup settings.
    Bttv,
    /// Pronouns lookup settings.
    Pronouns,
}

/// One available native configuration target and whether it already has saved setup.
#[derive(Clone, Debug, Serialize, Type)]
pub struct ConfigurationCatalogEntry {
    /// Stable target identifier used by the settings directory.
    pub id: String,
    /// Name supplied by the compiled integration.
    pub title: String,
    /// Settings directory section.
    pub category: ConfigurationCategory,
    /// Native form module, or none for an account with its own login flow.
    pub module: Option<ConnectionModule>,
    /// Existing record identifier or account role when the target has records.
    pub record_id: Option<String>,
    /// Whether this target has saved configuration or a configured account.
    pub configured: bool,
    /// Safe explanation when this target's saved state cannot be read.
    pub error: Option<String>,
}

/// Builds the desktop settings directory from registered integrations and their stored state.
pub fn configuration_catalog(services: &AppServices) -> Vec<ConfigurationCatalogEntry> {
    let accounts = services.twitch().accounts().configured_accounts();
    let (obs_configured, obs_error) = catalog_state(
        services
            .obs()
            .settings()
            .load()
            .map(|settings| settings.is_configured())
            .map_err(|error| obs_error(error.into())),
    );
    let (vtube_configured, vtube_error) = catalog_state(
        services
            .vtube()
            .settings()
            .load()
            .map(|settings| settings.is_configured())
            .map_err(|error| vtube_error(error.into())),
    );
    let (api_configured, api_error) =
        catalog_state(services.api().is_configured().map_err(api_error));
    let twitch_error = accounts
        .as_ref()
        .err()
        .map(|_| ConfigurationError::unavailable().message);
    let mut entries = vec![
        form_entry::<ObsSettings>(ConnectionModule::Obs, obs_configured, obs_error),
        ConfigurationCatalogEntry {
            id: snenk_bot::twitch::accounts::ACCOUNT_INTEGRATION_ID.into(),
            title: snenk_bot::twitch::accounts::ACCOUNT_TITLE.into(),
            category: snenk_bot::twitch::accounts::ACCOUNT_CATEGORY,
            module: None,
            record_id: None,
            configured: accounts
                .as_ref()
                .map(|accounts| accounts.broadcaster.is_some() || accounts.bot.is_some())
                .unwrap_or(true),
            error: twitch_error,
        },
        form_entry::<VtubeSettings>(ConnectionModule::VtubeStudio, vtube_configured, vtube_error),
        form_entry::<ApiSettings>(ConnectionModule::Api, api_configured, api_error),
    ];
    for provider in snenk_bot::providers::catalog() {
        let Some(configuration) = provider.configuration else {
            continue;
        };
        let (configured, error) = catalog_state(
            services
                .providers()
                .configured(configuration)
                .map_err(provider_error),
        );
        entries.push(match configuration {
            ProviderId::SteamGridDb => {
                form_entry::<SteamGridDbSettings>(ConnectionModule::Steamgriddb, configured, error)
            }
            ProviderId::Ffz => form_entry::<FfzSettings>(ConnectionModule::Ffz, configured, error),
            ProviderId::Bttv => {
                form_entry::<BttvSettings>(ConnectionModule::Bttv, configured, error)
            }
            ProviderId::Pronouns => {
                form_entry::<PronounsSettings>(ConnectionModule::Pronouns, configured, error)
            }
        });
    }
    entries
}

/// Builds directory copy from the same type that defines and serializes the native form.
fn form_entry<T: DescribeConfig>(
    module: ConnectionModule,
    configured: bool,
    error: Option<String>,
) -> ConfigurationCatalogEntry {
    ConfigurationCatalogEntry {
        id: T::INTEGRATION
            .expect("catalog form must declare integration")
            .into(),
        title: T::SCHEMA.title.into(),
        category: T::CATEGORY.expect("catalog form must declare category"),
        module: Some(module),
        record_id: None,
        configured,
        error,
    }
}

/// Keeps a failed configuration visible without hiding unrelated catalog entries.
fn catalog_state(result: Result<bool, ConfigurationError>) -> (bool, Option<String>) {
    match result {
        Ok(configured) => (configured, None),
        Err(error) => (true, Some(error.message)),
    }
}

#[derive(Debug, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct OpenConfiguration {
    pub module: ConnectionModule,
    pub record_id: Option<String>,
}

#[derive(Debug, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationSessionRequest {
    pub session_id: String,
}

/// Selects the credential declared by a live configuration session.
#[derive(Debug, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationCredentialIdentity {
    pub session_id: String,
    pub credential_id: String,
}

/// Saves a credential separately from the portable configuration values.
#[derive(Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct SaveConfigurationCredential {
    pub session_id: String,
    pub credential_id: String,
    pub value: Option<String>,
}

/// Selects the pairing operation declared by a live configuration session.
#[derive(Debug, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationAuthorizationIdentity {
    pub session_id: String,
    pub authorization_id: String,
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
    pub credential: Option<snenk_bot::schema::CredentialDescriptor>,
    pub authorization: Option<snenk_bot::schema::AuthorizationDescriptor>,
    pub collection: Option<snenk_bot::schema::ConfigurationCollectionDescriptor>,
    /// Whether the saved configuration meets the declared pairing prerequisite.
    pub authorization_ready: bool,
    /// Tools declared by the owner of this settings form.
    pub actions: Vec<snenk_bot::schema::ConfigurationActionDescriptor>,
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
    Api(ApiSettings),
    ApiConsumer(ConsumerSummary),
    Steamgriddb(SteamGridDbSettings),
    Ffz(FfzSettings),
    Bttv(BttvSettings),
    Pronouns(PronounsSettings),
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

    /// Resolves a credential only from its live settings session and declared descriptor.
    pub fn credential_module(
        &self,
        session_id: &str,
        credential_id: &str,
    ) -> Result<ConnectionModule, ConfigurationError> {
        let session = self.sessions.get(session_id).ok_or_else(missing_session)?;
        let snapshot = session.snapshot(session_id)?;
        if snapshot
            .credential
            .is_some_and(|credential| credential.id == credential_id)
        {
            Ok(snapshot.module)
        } else {
            Err(ConfigurationError::new(
                ConfigurationErrorCode::InvalidValues,
                "This credential does not belong to the settings view.",
            ))
        }
    }

    /// Resolves pairing against its declared owner and, when requested, saved prerequisite.
    pub fn authorization_module(
        &self,
        session_id: &str,
        authorization_id: &str,
        require_ready: bool,
    ) -> Result<ConnectionModule, ConfigurationError> {
        let session = self.sessions.get(session_id).ok_or_else(missing_session)?;
        let snapshot = session.snapshot(session_id)?;
        let Some(descriptor) = snapshot.authorization else {
            return Err(unsupported_authorization());
        };
        if descriptor.id != authorization_id {
            return Err(unsupported_authorization());
        }
        if require_ready && !snapshot.authorization_ready {
            return Err(ConfigurationError::new(
                ConfigurationErrorCode::InvalidValues,
                descriptor
                    .prerequisite_message
                    .unwrap_or("Save these settings before authorizing."),
            ));
        }
        Ok(snapshot.module)
    }

    /// Resolves a named-record collection only from its live owning settings session.
    pub fn collection_module(
        &self,
        session_id: &str,
        collection_id: &str,
    ) -> Result<ConnectionModule, ConfigurationError> {
        let session = self.sessions.get(session_id).ok_or_else(missing_session)?;
        let snapshot = session.snapshot(session_id)?;
        if snapshot
            .collection
            .is_some_and(|collection| collection.id == collection_id)
        {
            Ok(snapshot.module)
        } else {
            Err(ConfigurationError::new(
                ConfigurationErrorCode::InvalidValues,
                "This collection does not belong to the settings view.",
            ))
        }
    }

    /// Resolves a directory control only from the native schema owned by the open form.
    pub fn directory_field(
        &self,
        session_id: &str,
        collection_id: Option<&str>,
        field_path: &[String],
    ) -> Result<String, ConfigurationError> {
        let session = self.sessions.get(session_id).ok_or_else(missing_session)?;
        let snapshot = session.snapshot(session_id)?;
        let schema = if let Some(collection_id) = collection_id {
            match self.collection_module(session_id, collection_id)? {
                ConnectionModule::Api => CreateApiConsumer::SCHEMA,
                _ => return Err(invalid_directory_field()),
            }
        } else {
            &snapshot.schema
        };
        directory_label(schema.fields, field_path)
    }

    pub fn save(
        &mut self,
        request: SaveConfiguration,
        services: &AppServices,
    ) -> Result<ConfigurationSaveResult, ConfigurationError> {
        self.save_with(request, |original, values| match original {
            LoadedConnectionSettings::Api(original) => {
                let value = parse_values::<ApiSettings>(values)?;
                let api = services.api();
                let outcome = api
                    .save_configuration(original.clone(), value.clone())
                    .map_err(api_error)?;
                let saved = api.configuration().map_err(api_error)?;
                if saved != value {
                    return Err(readback_conflict());
                }
                Ok((LoadedConnectionSettings::Api(saved), outcome))
            }
            LoadedConnectionSettings::ApiConsumer(original) => {
                let value = parse_consumer_values(values)?;
                let (saved, outcome) = services
                    .api()
                    .save_consumer_configuration(
                        original,
                        value.name,
                        value.permissions,
                        value.allowed_origins,
                        value.static_folder,
                    )
                    .map_err(api_error)?;
                Ok((LoadedConnectionSettings::ApiConsumer(saved), outcome))
            }
            LoadedConnectionSettings::Steamgriddb(original) => {
                let value = parse_values::<SteamGridDbSettings>(values)?;
                let providers = services.providers();
                let outcome = providers
                    .save_enabled(ProviderId::SteamGridDb, original.enabled, value.enabled)
                    .map_err(provider_error)?;
                let saved = providers
                    .enabled(ProviderId::SteamGridDb)
                    .map_err(provider_error)?;
                if saved != value.enabled {
                    return Err(readback_conflict());
                }
                Ok((LoadedConnectionSettings::Steamgriddb(value), outcome))
            }
            LoadedConnectionSettings::Ffz(original) => {
                let value = parse_values::<FfzSettings>(values)?;
                let providers = services.providers();
                let outcome = providers
                    .save_enabled(ProviderId::Ffz, original.enabled, value.enabled)
                    .map_err(provider_error)?;
                let saved = providers.enabled(ProviderId::Ffz).map_err(provider_error)?;
                if saved != value.enabled {
                    return Err(readback_conflict());
                }
                Ok((LoadedConnectionSettings::Ffz(value), outcome))
            }
            LoadedConnectionSettings::Bttv(original) => {
                let value = parse_values::<BttvSettings>(values)?;
                let providers = services.providers();
                let outcome = providers
                    .save_enabled(ProviderId::Bttv, original.enabled, value.enabled)
                    .map_err(provider_error)?;
                let saved = providers
                    .enabled(ProviderId::Bttv)
                    .map_err(provider_error)?;
                if saved != value.enabled {
                    return Err(readback_conflict());
                }
                Ok((LoadedConnectionSettings::Bttv(value), outcome))
            }
            LoadedConnectionSettings::Pronouns(original) => {
                let value = parse_values::<PronounsSettings>(values)?;
                let providers = services.providers();
                let outcome = providers
                    .save_enabled(ProviderId::Pronouns, original.enabled, value.enabled)
                    .map_err(provider_error)?;
                let saved = providers
                    .enabled(ProviderId::Pronouns)
                    .map_err(provider_error)?;
                if saved != value.enabled {
                    return Err(readback_conflict());
                }
                Ok((LoadedConnectionSettings::Pronouns(value), outcome))
            }

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

/// Follows nested native fields without permitting arbitrary picker requests from the frontend.
fn directory_label(
    fields: &[snenk_bot::schema::ConfigField],
    path: &[String],
) -> Result<String, ConfigurationError> {
    let Some((id, remaining)) = path.split_first() else {
        return Err(invalid_directory_field());
    };
    if path.len() > 16 {
        return Err(invalid_directory_field());
    }
    let field = fields
        .iter()
        .find(|field| field.id == id)
        .ok_or_else(invalid_directory_field)?;
    match field.kind {
        snenk_bot::schema::ConfigFieldKind::Directory if remaining.is_empty() => {
            Ok(field.label.to_owned())
        }
        snenk_bot::schema::ConfigFieldKind::Object(fields) if !remaining.is_empty() => {
            directory_label(fields, remaining)
        }
        _ => Err(invalid_directory_field()),
    }
}

/// Returns a safe error for an absent or incorrectly typed filesystem control.
fn invalid_directory_field() -> ConfigurationError {
    ConfigurationError::new(
        ConfigurationErrorCode::InvalidValues,
        "This settings field does not support folder selection.",
    )
}

impl ConfigurationSession {
    fn snapshot(&self, id: &str) -> Result<ConfigurationSnapshot, ConfigurationError> {
        let (module, schema, defaults, values, credential, authorization, collection, actions) =
            match &self.loaded {
                LoadedConnectionSettings::Api(value) => (
                    ConnectionModule::Api,
                    *ApiSettings::SCHEMA,
                    to_values(&ApiSettings::default())?,
                    to_values(value)?,
                    ApiSettings::CREDENTIAL,
                    ApiSettings::AUTHORIZATION,
                    ApiSettings::COLLECTION,
                    ApiSettings::ACTIONS.to_vec(),
                ),
                LoadedConnectionSettings::ApiConsumer(value) => (
                    ConnectionModule::Api,
                    *CreateApiConsumer::SCHEMA,
                    to_values(&CreateApiConsumer::default())?,
                    consumer_values(value)?,
                    CreateApiConsumer::CREDENTIAL,
                    CreateApiConsumer::AUTHORIZATION,
                    CreateApiConsumer::COLLECTION,
                    CreateApiConsumer::ACTIONS.to_vec(),
                ),
                LoadedConnectionSettings::Steamgriddb(value) => (
                    ConnectionModule::Steamgriddb,
                    *SteamGridDbSettings::SCHEMA,
                    to_values(&SteamGridDbSettings::default())?,
                    to_values(value)?,
                    SteamGridDbSettings::CREDENTIAL,
                    SteamGridDbSettings::AUTHORIZATION,
                    SteamGridDbSettings::COLLECTION,
                    SteamGridDbSettings::ACTIONS.to_vec(),
                ),
                LoadedConnectionSettings::Ffz(value) => (
                    ConnectionModule::Ffz,
                    *FfzSettings::SCHEMA,
                    to_values(&FfzSettings::default())?,
                    to_values(value)?,
                    FfzSettings::CREDENTIAL,
                    FfzSettings::AUTHORIZATION,
                    FfzSettings::COLLECTION,
                    FfzSettings::ACTIONS.to_vec(),
                ),
                LoadedConnectionSettings::Bttv(value) => (
                    ConnectionModule::Bttv,
                    *BttvSettings::SCHEMA,
                    to_values(&BttvSettings::default())?,
                    to_values(value)?,
                    BttvSettings::CREDENTIAL,
                    BttvSettings::AUTHORIZATION,
                    BttvSettings::COLLECTION,
                    BttvSettings::ACTIONS.to_vec(),
                ),
                LoadedConnectionSettings::Pronouns(value) => (
                    ConnectionModule::Pronouns,
                    *PronounsSettings::SCHEMA,
                    to_values(&PronounsSettings::default())?,
                    to_values(value)?,
                    PronounsSettings::CREDENTIAL,
                    PronounsSettings::AUTHORIZATION,
                    PronounsSettings::COLLECTION,
                    PronounsSettings::ACTIONS.to_vec(),
                ),

                LoadedConnectionSettings::Obs(loaded) => (
                    ConnectionModule::Obs,
                    *ObsSettings::SCHEMA,
                    to_values(&ObsSettings::default())?,
                    to_values(&loaded.value)?,
                    ObsSettings::CREDENTIAL,
                    ObsSettings::AUTHORIZATION,
                    ObsSettings::COLLECTION,
                    ObsSettings::ACTIONS.to_vec(),
                ),
                LoadedConnectionSettings::VtubeStudio(loaded) => (
                    ConnectionModule::VtubeStudio,
                    *VtubeSettings::SCHEMA,
                    to_values(&VtubeSettings::default())?,
                    to_values(&loaded.value)?,
                    VtubeSettings::CREDENTIAL,
                    VtubeSettings::AUTHORIZATION,
                    VtubeSettings::COLLECTION,
                    VtubeSettings::ACTIONS.to_vec(),
                ),
            };
        let authorization_ready = authorization
            .map(|descriptor| {
                descriptor.prerequisite_field.is_none_or(|field| {
                    values
                        .get(field)
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);
        Ok(ConfigurationSnapshot {
            session_id: id.into(),
            revision: self.revision,
            module,
            schema,
            defaults,
            values,
            credential,
            authorization,
            collection,
            authorization_ready,
            actions,
        })
    }
}

/// Keeps the consumer identifier and token outside the portable editor values.
fn consumer_values(value: &ConsumerSummary) -> Result<Values, ConfigurationError> {
    to_values(&CreateApiConsumer {
        name: value.name.clone(),
        permissions: value.permissions.clone(),
        allowed_origins: value.allowed_origins.clone(),
        static_folder: value.static_folder.clone(),
    })
}

/// Parses only the editable consumer grant fields from a session save.
fn parse_consumer_values(values: Values) -> Result<CreateApiConsumer, ConfigurationError> {
    serde_json::from_value(serde_json::Value::Object(values.into_iter().collect())).map_err(|_| {
        ConfigurationError::new(
            ConfigurationErrorCode::InvalidValues,
            "Check the consumer name, origins, and permissions before saving.",
        )
    })
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

fn unsupported_authorization() -> ConfigurationError {
    ConfigurationError::new(
        ConfigurationErrorCode::InvalidValues,
        "This authorization does not belong to the settings view.",
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

/// Converts API storage and validation failures without exposing private paths or keys.
pub(crate) fn api_error(error: ApiError) -> ConfigurationError {
    match error {
        ApiError::Store(error) => store_error(error),
        ApiError::Conflict => readback_conflict(),
        ApiError::Invalid(message) => {
            ConfigurationError::new(ConfigurationErrorCode::InvalidValues, message)
        }
        _ => ConfigurationError::new(
            ConfigurationErrorCode::SaveFailed,
            "The API operation could not be completed. Check its configuration and credential access.",
        ),
    }
}

/// Maps provider configuration failures into the existing secret-free settings error contract.
pub(crate) fn provider_error(error: ProviderIntegrationError) -> ConfigurationError {
    match error {
        ProviderIntegrationError::Settings(ProviderSettingsError::Store(error)) => {
            store_error(error)
        }
        ProviderIntegrationError::Conflict => readback_conflict(),
        ProviderIntegrationError::Service(
            snenk_bot::providers::ProviderError::InvalidArguments(message),
        ) => ConfigurationError::new(ConfigurationErrorCode::InvalidValues, message),
        _ => ConfigurationError::new(
            ConfigurationErrorCode::SaveFailed,
            "Provider settings or credentials could not be accessed. Your entries are still available.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snenk_bot::api::ConsumerPermissions;
    use snenk_bot::obs::settings::ObsSettingsStore;
    use snenk_bot::paths::AppPaths;
    use snenk_bot::vtube::settings::VtubeSettingsStore;

    #[test]
    fn credential_identity_is_bound_to_a_live_declared_session() {
        let mut sessions = ConfigurationSessions::default();
        let directory = tempfile::tempdir().unwrap();
        let paths = AppPaths {
            config: directory.path().into(),
            data: directory.path().into(),
            state: directory.path().into(),
        };
        let obs = sessions
            .open(LoadedConnectionSettings::Obs(
                ObsSettingsStore::new(&paths).load().unwrap(),
            ))
            .unwrap();
        let steam = sessions
            .open(LoadedConnectionSettings::Steamgriddb(
                SteamGridDbSettings::default(),
            ))
            .unwrap();
        let emotes = sessions
            .open(LoadedConnectionSettings::Ffz(FfzSettings::default()))
            .unwrap();
        assert!(matches!(
            sessions.credential_module(&obs.session_id, "obs-password"),
            Ok(ConnectionModule::Obs)
        ));
        assert!(matches!(
            sessions.credential_module(&steam.session_id, "steamgriddb-key"),
            Ok(ConnectionModule::Steamgriddb)
        ));
        for (session_id, credential_id) in [
            (&obs.session_id, "steamgriddb-key"),
            (&steam.session_id, "obs-password"),
            (&emotes.session_id, "steamgriddb-key"),
        ] {
            assert_eq!(
                sessions
                    .credential_module(session_id, credential_id)
                    .unwrap_err()
                    .code,
                ConfigurationErrorCode::InvalidValues
            );
        }
        sessions.close(&steam.session_id).unwrap();
        assert_eq!(
            sessions
                .credential_module(&steam.session_id, "steamgriddb-key")
                .unwrap_err()
                .code,
            ConfigurationErrorCode::MissingSession
        );
    }

    /// Pairing and named records resolve only through their owning live settings session.
    #[test]
    fn authorization_and_collection_identity_use_declared_sessions() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AppPaths {
            config: directory.path().into(),
            data: directory.path().into(),
            state: directory.path().into(),
        };
        let vtube_store = VtubeSettingsStore::new(&paths);
        let mut sessions = ConfigurationSessions::default();
        let api = sessions
            .open(LoadedConnectionSettings::Api(ApiSettings::default()))
            .unwrap();
        let vtube = sessions
            .open(LoadedConnectionSettings::VtubeStudio(
                vtube_store.load().unwrap(),
            ))
            .unwrap();
        assert_eq!(api.collection.unwrap().id, "api.consumers");
        assert_eq!(vtube.authorization.unwrap().id, "vtube-authorization");
        assert!(!vtube.authorization_ready);
        assert!(matches!(
            sessions.collection_module(&api.session_id, "api.consumers"),
            Ok(ConnectionModule::Api)
        ));
        assert!(matches!(
            sessions.authorization_module(&vtube.session_id, "vtube-authorization", false),
            Ok(ConnectionModule::VtubeStudio)
        ));
        for result in [
            sessions.collection_module(&vtube.session_id, "api.consumers"),
            sessions.collection_module(&api.session_id, "wrong"),
            sessions.authorization_module(&api.session_id, "vtube-authorization", false),
            sessions.authorization_module(&vtube.session_id, "wrong", false),
            sessions.authorization_module(&vtube.session_id, "vtube-authorization", true),
        ] {
            assert_eq!(
                result.unwrap_err().code,
                ConfigurationErrorCode::InvalidValues
            );
        }
        let loaded = vtube_store.load().unwrap();
        vtube_store
            .save(
                &loaded,
                VtubeSettings {
                    enabled: true,
                    ..VtubeSettings::default()
                },
            )
            .unwrap();
        let enabled = sessions
            .open(LoadedConnectionSettings::VtubeStudio(
                vtube_store.load().unwrap(),
            ))
            .unwrap();
        assert!(enabled.authorization_ready);
        assert!(matches!(
            sessions.authorization_module(&enabled.session_id, "vtube-authorization", true),
            Ok(ConnectionModule::VtubeStudio)
        ));
        sessions.close(&api.session_id).unwrap();
        assert_eq!(
            sessions
                .collection_module(&api.session_id, "api.consumers")
                .unwrap_err()
                .code,
            ConfigurationErrorCode::MissingSession
        );
    }

    /// Directory pickers accept only declared directory fields in a live owning form.
    #[test]
    fn directory_picker_requires_a_declared_field_and_live_owner() {
        let mut sessions = ConfigurationSessions::default();
        let api = sessions
            .open(LoadedConnectionSettings::Api(ApiSettings::default()))
            .unwrap();
        let collection = Some("api.consumers");
        assert_eq!(
            sessions
                .directory_field(&api.session_id, collection, &["static_folder".into()])
                .unwrap(),
            "Static folder"
        );
        for (owner, path) in [
            (None, vec!["static_folder".into()]),
            (collection, vec!["name".into()]),
            (Some("other.collection"), vec!["static_folder".into()]),
            (collection, vec![]),
            (
                collection,
                vec!["permissions".into(), "static_folder".into()],
            ),
        ] {
            assert_eq!(
                sessions
                    .directory_field(&api.session_id, owner, &path)
                    .unwrap_err()
                    .code,
                ConfigurationErrorCode::InvalidValues
            );
        }
        sessions.close(&api.session_id).unwrap();
        assert_eq!(
            sessions
                .directory_field(&api.session_id, collection, &["static_folder".into()])
                .unwrap_err()
                .code,
            ConfigurationErrorCode::MissingSession
        );
    }

    /// Consumer edits reuse generated fields and retain their baseline after a conflicting save.
    #[test]
    fn consumer_session_uses_edit_schema_and_keeps_original_after_conflict() {
        let original = ConsumerSummary {
            id: "consumer-id".into(),
            name: "original".into(),
            permissions: ConsumerPermissions::default(),
            allowed_origins: Default::default(),
            static_folder: None,
        };
        let mut sessions = ConfigurationSessions::default();
        let snapshot = sessions
            .open(LoadedConnectionSettings::ApiConsumer(original.clone()))
            .unwrap();
        assert!(matches!(snapshot.module, ConnectionModule::Api));
        assert_eq!(snapshot.schema.id, "snenkbot.api.consumer");
        assert_eq!(snapshot.values["name"], "original");
        assert!(!snapshot.values.contains_key("id"));
        assert!(!snapshot.values.contains_key("token_hash"));
        let edited = CreateApiConsumer {
            name: "edited".into(),
            permissions: ConsumerPermissions::default(),
            allowed_origins: Default::default(),
            static_folder: None,
        };
        let error = sessions
            .save_with(
                SaveConfiguration {
                    session_id: snapshot.session_id.clone(),
                    expected_revision: 0,
                    values: to_values(&edited).unwrap(),
                },
                |loaded, values| {
                    assert!(matches!(loaded, LoadedConnectionSettings::ApiConsumer(value) if value == &original));
                    assert_eq!(parse_consumer_values(values)?.name, "edited");
                    Err(readback_conflict())
                },
            )
            .unwrap_err();
        assert_eq!(error.code, ConfigurationErrorCode::SaveConflict);
        let still_open = sessions.sessions[&snapshot.session_id]
            .snapshot(&snapshot.session_id)
            .unwrap();
        assert_eq!(still_open.revision, 0);
        assert_eq!(still_open.values, snapshot.values);
    }

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
        assert_eq!(snapshot.credential.unwrap().id, "obs-password");
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

    #[test]
    fn api_session_exposes_schema_defaults_and_retains_baseline_after_failed_save() {
        let mut sessions = ConfigurationSessions::default();
        let snapshot = sessions
            .open(LoadedConnectionSettings::Api(ApiSettings::default()))
            .unwrap();
        assert!(matches!(snapshot.module, ConnectionModule::Api));
        assert_eq!(snapshot.schema.id, "snenkbot.api.settings");
        assert_eq!(snapshot.schema.version, 1);
        assert_eq!(
            snapshot.defaults,
            to_values(&ApiSettings::default()).unwrap()
        );
        assert_eq!(snapshot.values, snapshot.defaults);
        assert_eq!(snapshot.schema.fields.len(), snapshot.defaults.len());
        for field in snapshot.schema.fields {
            assert!(snapshot.defaults.contains_key(field.id));
        }

        let saved = ApiSettings {
            port: 38765,
            ..ApiSettings::default()
        };
        let result = sessions
            .save_with(
                SaveConfiguration {
                    session_id: snapshot.session_id.clone(),
                    expected_revision: 0,
                    values: to_values(&saved).unwrap(),
                },
                |original, values| {
                    assert!(matches!(original, LoadedConnectionSettings::Api(value) if value == &ApiSettings::default()));
                    let value = parse_values::<ApiSettings>(values)?;
                    Ok((
                        LoadedConnectionSettings::Api(value),
                        SaveOutcome {
                            published: true,
                            warnings: Vec::new(),
                        },
                    ))
                },
            )
            .unwrap();
        assert_eq!(result.snapshot.revision, 1);
        assert_eq!(result.snapshot.values, to_values(&saved).unwrap());

        let draft = ApiSettings {
            port: 38766,
            ..saved.clone()
        };
        let draft_values = to_values(&draft).unwrap();
        let error = sessions
            .save_with(
                SaveConfiguration {
                    session_id: snapshot.session_id.clone(),
                    expected_revision: 1,
                    values: draft_values.clone(),
                },
                |original, values| {
                    assert!(
                        matches!(original, LoadedConnectionSettings::Api(value) if value == &saved)
                    );
                    assert_eq!(values, draft_values);
                    Err(readback_conflict())
                },
            )
            .unwrap_err();
        assert_eq!(error.code, ConfigurationErrorCode::SaveConflict);
        let current = sessions.sessions[&snapshot.session_id]
            .snapshot(&snapshot.session_id)
            .unwrap();
        assert_eq!(current.revision, 1);
        assert_eq!(current.values, result.snapshot.values);
        assert_eq!(draft_values, to_values(&draft).unwrap());
    }

    /// Each provider has its own native schema and only SteamGridDB owns a credential.
    #[test]
    fn provider_sessions_are_independent() {
        let mut sessions = ConfigurationSessions::default();
        let steam = sessions
            .open(LoadedConnectionSettings::Steamgriddb(
                SteamGridDbSettings::default(),
            ))
            .unwrap();
        let emotes = sessions
            .open(LoadedConnectionSettings::Ffz(FfzSettings::default()))
            .unwrap();
        let bttv = sessions
            .open(LoadedConnectionSettings::Bttv(BttvSettings::default()))
            .unwrap();
        let pronouns = sessions
            .open(LoadedConnectionSettings::Pronouns(
                PronounsSettings::default(),
            ))
            .unwrap();
        for (snapshot, id) in [
            (&steam, "snenkbot.provider.steamgriddb"),
            (&emotes, "snenkbot.provider.ffz"),
            (&bttv, "snenkbot.provider.bttv"),
            (&pronouns, "snenkbot.provider.pronouns"),
        ] {
            assert_eq!(snapshot.schema.id, id);
            assert_eq!(snapshot.schema.fields.len(), 1);
            assert_eq!(snapshot.defaults["enabled"], false);
            assert_eq!(snapshot.values, snapshot.defaults);
        }
        assert_eq!(steam.credential.unwrap().id, "steamgriddb-key");
        assert!(emotes.credential.is_none());
        assert!(bttv.credential.is_none());
        assert!(pronouns.credential.is_none());
        let mut invalid = emotes.values.clone();
        invalid.insert("steamgriddb_enabled".into(), serde_json::json!(true));
        assert_eq!(
            parse_values::<FfzSettings>(invalid).unwrap_err().code,
            ConfigurationErrorCode::InvalidValues
        );
    }
}
