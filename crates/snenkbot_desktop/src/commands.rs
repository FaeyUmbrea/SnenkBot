use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{Runtime, State};
use tokio::sync::Semaphore;

use snenk_bot::app::{AppServiceError, AppServices, WorkflowSaveError};
use snenk_bot::engine::Values;
use std::collections::BTreeMap;

use crate::authentication::{
    ApproveTwitchLogin, AuthenticationError, AuthenticationErrorCode, AuthenticationIdentity,
    DesktopAuthentication, StartTwitchLogin, TwitchAuthentication, TwitchAuthenticationPhase,
};
use crate::history::{DesktopHistory, HistoryPage, HistoryQuery, HistoryQueryError};
use crate::hub::{DesktopHub, DesktopSnapshot, StartupState};
use crate::input::{DesktopInputProvider, InputBridgeError};
use snenk_bot::history::{RunHistory, RunRecord};
use snenk_bot::runtime::RuntimeSpawner;
use snenk_bot::schema::ConfigSchema;
use snenk_bot::storage::{SaveWarning, StoreError};
use snenk_bot::workflows::{
    EditableWorkflow, WorkflowDefinition, WorkflowRepository, WorkflowRepositoryError,
};

use crate::configuration::{
    ConfigurationAuthorizationIdentity, ConfigurationCredentialIdentity, ConfigurationError,
    ConfigurationErrorCode, ConfigurationSaveResult, ConfigurationSessionRequest,
    ConfigurationSessions, ConfigurationSnapshot, ConnectionModule, LoadedConnectionSettings,
    OpenConfiguration, SaveConfiguration, SaveConfigurationCredential, obs_error, vtube_error,
};
use crate::editor::{
    EditorError, EditorErrorCode, EditorOperation, EditorSessions, EditorSnapshot,
};
use crate::events::{
    EventCommandError, EventCommandErrorCode, EventIdentity, EventPage, ReplayEvent,
};

#[derive(Debug, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct CreateWorkflow {
    pub name: String,
}

#[derive(Debug, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct OpenEditor {
    pub workflow_id: String,
}

#[derive(Debug, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct EditorValueSources {
    pub session_id: String,
    #[specta(type = specta_typescript::Number)]
    pub expected_revision: u64,
    pub step_id: String,
}

#[derive(Debug, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct EditorSessionRequest {
    pub session_id: String,
}

#[derive(Debug, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct ApplyEditorEdit {
    pub session_id: String,
    #[specta(type = specta_typescript::Number)]
    pub expected_revision: u64,
    pub edit: EditorOperation,
}

#[derive(Debug, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct SaveEditor {
    pub session_id: String,
    #[specta(type = specta_typescript::Number)]
    pub expected_revision: u64,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SaveNotice {
    DirectorySync,
    BackupRetention,
}

#[derive(Debug, Serialize, Type)]
pub struct EditorSaveResult {
    pub snapshot: EditorSnapshot,
    pub notices: Vec<SaveNotice>,
}

#[derive(Debug, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct SubmitInput {
    pub request_id: String,
    #[specta(type = BTreeMap<String, specta_typescript::Unknown>)]
    pub values: Values,
}

#[derive(Debug, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct InputRequestIdentity {
    pub request_id: String,
}

#[derive(Debug, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct DismissError {
    pub id: String,
}

#[derive(Debug, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct RunWorkflow {
    pub workflow_id: String,
}

#[derive(Debug, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct InspectHistory {
    pub run_id: String,
}

#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum CredentialPresence {
    Empty,
    Stored,
    Unavailable,
}

/// Only the host supplies running services; bridges can be tested without starting integrations.
pub struct DesktopServices {
    pub services: Arc<AppServices>,
    pub repository: WorkflowRepository,
    pub history: RunHistory,
    pub runtime: RuntimeSpawner,
}

/// Startup failures remain inspectable even when application services could not start.
pub struct DesktopBackend {
    ready: Option<DesktopCommands>,
    pub(crate) hub: Arc<DesktopHub>,
    pub(crate) input: Arc<DesktopInputProvider>,
    authentication: Arc<DesktopAuthentication>,
}

impl DesktopBackend {
    pub fn new(
        ready: Option<DesktopServices>,
        hub: Arc<DesktopHub>,
        input: Arc<DesktopInputProvider>,
    ) -> Self {
        let authentication_hub = Arc::clone(&hub);
        let authentication = Arc::new(DesktopAuthentication::new(move |attempt| {
            authentication_hub.publish(crate::hub::DesktopEvent::TwitchAuthentication(attempt))
        }));
        Self {
            ready: ready.map(|ready| DesktopCommands {
                history: DesktopHistory::new(ready.history),
                sessions: Arc::new(Mutex::new(EditorSessions::new(ready.repository.clone()))),
                configuration: Arc::new(Mutex::new(ConfigurationSessions::default())),
                repository: ready.repository,
                services: ready.services,
                workers: Arc::new(Semaphore::new(2)),
                runtime: ready.runtime,
            }),
            hub,
            input,
            authentication,
        }
    }

    /// Resolves an owned collection from an existing configuration session.
    pub(crate) async fn collection_module(
        &self,
        session_id: String,
        collection_id: String,
    ) -> Result<ConnectionModule, ConfigurationError> {
        self.ready()
            .map_err(|_| ConfigurationError::unavailable())?
            .configure(move |sessions| sessions.collection_module(&session_id, &collection_id))
            .await
    }

    /// Validates a native directory control through its live configuration session.
    pub(crate) async fn directory_field(
        &self,
        request: crate::directories::PickConfigurationDirectory,
    ) -> Result<String, ConfigurationError> {
        self.ready()
            .map_err(|_| ConfigurationError::unavailable())?
            .configure(move |sessions| {
                sessions.directory_field(
                    &request.session_id,
                    request.collection_id.as_deref(),
                    &request.field_path,
                )
            })
            .await
    }

    /// Runs API administration in the same bounded blocking worker pool as settings saves.
    pub(crate) async fn with_api<T: Send + 'static>(
        &self,
        operation: impl FnOnce(Arc<snenk_bot::api::ApiService>) -> Result<T, snenk_bot::api::ApiError>
        + Send
        + 'static,
    ) -> Result<T, ConfigurationError> {
        let ready = self
            .ready()
            .map_err(|_| ConfigurationError::unavailable())?;
        let permit = Arc::clone(&ready.workers)
            .try_acquire_owned()
            .map_err(|_| {
                ConfigurationError::new(
                    ConfigurationErrorCode::Busy,
                    "Settings are busy. Try again shortly.",
                )
            })?;
        let api = Arc::clone(ready.services.api());
        tauri::async_runtime::spawn_blocking(move || {
            let _permit = permit;
            operation(api).map_err(crate::configuration::api_error)
        })
        .await
        .map_err(|_| ConfigurationError::unavailable())?
    }

    /// Runs provider administration independently of the optional API service.
    pub(crate) async fn with_providers<T: Send + 'static>(
        &self,
        operation: impl FnOnce(
            Arc<snenk_bot::providers::ProviderIntegration>,
        ) -> Result<T, snenk_bot::providers::ProviderIntegrationError>
        + Send
        + 'static,
    ) -> Result<T, ConfigurationError> {
        let ready = self
            .ready()
            .map_err(|_| ConfigurationError::unavailable())?;
        let permit = Arc::clone(&ready.workers)
            .try_acquire_owned()
            .map_err(|_| {
                ConfigurationError::new(
                    ConfigurationErrorCode::Busy,
                    "Settings are busy. Try again shortly.",
                )
            })?;
        let providers = Arc::clone(ready.services.providers());
        tauri::async_runtime::spawn_blocking(move || {
            let _permit = permit;
            operation(providers).map_err(crate::configuration::provider_error)
        })
        .await
        .map_err(|_| ConfigurationError::unavailable())?
    }

    fn ready(&self) -> Result<&DesktopCommands, EditorError> {
        if !matches!(
            self.hub.startup_state().map_err(|_| unavailable())?,
            StartupState::Ready
        ) {
            return Err(unavailable());
        }
        self.ready.as_ref().ok_or_else(unavailable)
    }
}

struct DesktopCommands {
    history: DesktopHistory,
    sessions: Arc<Mutex<EditorSessions>>,
    configuration: Arc<Mutex<ConfigurationSessions>>,
    repository: WorkflowRepository,
    services: Arc<AppServices>,
    workers: Arc<Semaphore>,
    runtime: RuntimeSpawner,
}

impl DesktopCommands {
    async fn history<T: Send + 'static>(
        &self,
        operation: impl FnOnce(DesktopHistory) -> Result<T, HistoryQueryError> + Send + 'static,
    ) -> Result<T, HistoryQueryError> {
        let permit =
            Arc::clone(&self.workers)
                .try_acquire_owned()
                .map_err(|_| HistoryQueryError {
                    code: crate::history::HistoryErrorCode::Busy,
                    message: "Run history is busy. Try again shortly.".into(),
                })?;
        let history = self.history.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let _permit = permit;
            operation(history)
        })
        .await
        .map_err(|error| {
            tracing::error!(%error, "history query worker stopped");
            history_unavailable()
        })?
    }

    async fn configure<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut ConfigurationSessions) -> Result<T, ConfigurationError>
        + Send
        + 'static,
    ) -> Result<T, ConfigurationError> {
        let permit = Arc::clone(&self.workers).try_acquire_owned().map_err(|_| {
            ConfigurationError::new(
                ConfigurationErrorCode::Busy,
                "Connection settings are busy. Try again shortly.",
            )
        })?;
        let sessions = Arc::clone(&self.configuration);
        tauri::async_runtime::spawn_blocking(move || {
            let _permit = permit;
            let mut sessions = sessions
                .lock()
                .map_err(|_| ConfigurationError::unavailable())?;
            operation(&mut sessions)
        })
        .await
        .map_err(|error| {
            tracing::error!(%error, "connection settings worker stopped");
            ConfigurationError::unavailable()
        })?
    }

    async fn edit<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut EditorSessions) -> Result<T, EditorError> + Send + 'static,
    ) -> Result<T, EditorError> {
        // A flooded webview cannot queue an unbounded number of blocking disk/editor workers.
        let permit = Arc::clone(&self.workers).try_acquire_owned().map_err(|_| {
            EditorError::new(
                EditorErrorCode::Busy,
                "The editor is busy. Try again shortly.",
            )
        })?;
        let sessions = Arc::clone(&self.sessions);
        tauri::async_runtime::spawn_blocking(move || {
            let _permit = permit;
            let mut sessions = sessions.lock().map_err(|_| {
                tracing::error!("desktop editor lock poisoned");
                unavailable()
            })?;
            operation(&mut sessions)
        })
        .await
        .map_err(|error| {
            tracing::error!(%error, "desktop editor worker stopped");
            unavailable()
        })?
    }
}

/// Registers application commands without granting the webview filesystem or credential access.
/// The host owns service startup, runtime cancellation and shutdown.
pub fn configure<R: Runtime>(
    builder: tauri::Builder<R>,
    backend: DesktopBackend,
) -> tauri::Builder<R> {
    register_commands(builder.manage(backend))
}

/// Installs command dispatch before the host resolves paths and starts services.
pub(crate) fn register_commands<R: Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder.invoke_handler(tauri::generate_handler![
        crate::api_admin::api_history_import,
        crate::collections::configuration_collection_load,
        crate::collections::configuration_collection_create,
        crate::collections::configuration_collection_remove,
        crate::collections::configuration_collection_action,
        crate::directories::configuration_pick_directory,
        desktop_snapshot,
        dismiss_error,
        submit_input,
        cancel_input,
        run_workflow,
        workflow_create,
        editor_open,
        editor_value_sources,
        editor_snapshot,
        editor_apply,
        editor_save,
        editor_close,
        action_schemas,
        action_definitions,
        action_choices,
        configuration_catalog,
        configuration_open,
        configuration_save,
        configuration_close,
        configuration_credential_status,
        configuration_credential_save,
        history_page,
        history_inspect,
        events_list,
        events_inspect,
        events_targets,
        events_replay,
        twitch_accounts,
        twitch_login_start,
        twitch_login_approve,
        twitch_login_cancel,
        twitch_login_open_browser,
        twitch_login_copy,
        twitch_login_snapshot,
        configuration_authorization_status,
        configuration_authorize,
        configuration_forget_authorization,
    ])
}

/// Lists native configuration targets and their persisted setup state.
#[tauri::command]
fn configuration_catalog(
    state: State<'_, DesktopBackend>,
) -> Result<Vec<crate::configuration::ConfigurationCatalogEntry>, ConfigurationError> {
    let ready = state
        .ready()
        .map_err(|_| ConfigurationError::unavailable())?;
    Ok(crate::configuration::configuration_catalog(&ready.services))
}

#[derive(Clone, Copy, Debug, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum TwitchCopyTarget {
    Link,
    Code,
}

#[derive(Debug, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct CopyTwitchLogin {
    pub attempt_id: String,
    pub target: TwitchCopyTarget,
}

#[tauri::command]
fn twitch_login_snapshot(
    state: State<'_, DesktopBackend>,
) -> Result<Option<TwitchAuthentication>, AuthenticationError> {
    state.authentication.snapshot()
}

fn twitch_copy_text(
    authentication: &DesktopAuthentication,
    request: CopyTwitchLogin,
) -> Result<String, AuthenticationError> {
    let attempt = authentication
        .snapshot()?
        .filter(|attempt| attempt.attempt_id == request.attempt_id)
        .ok_or_else(|| {
            AuthenticationError::new(
                AuthenticationErrorCode::StaleAttempt,
                "This Twitch sign-in has changed. Use the current sign-in.",
            )
        })?;
    match (attempt.phase, request.target) {
        (TwitchAuthenticationPhase::Code { code, .. }, TwitchCopyTarget::Code) => Ok(code),
        (TwitchAuthenticationPhase::Code { .. }, TwitchCopyTarget::Link) => authentication
            .verification_url(AuthenticationIdentity {
                attempt_id: request.attempt_id,
            }),
        _ => Err(AuthenticationError::new(
            AuthenticationErrorCode::NotReady,
            "This Twitch sign-in is not ready for copying.",
        )),
    }
}

#[tauri::command]
async fn twitch_login_copy(
    state: State<'_, DesktopBackend>,
    request: CopyTwitchLogin,
) -> Result<(), AuthenticationError> {
    let ready = state
        .ready()
        .map_err(|_| AuthenticationError::unavailable())?;
    let _permit = Arc::clone(&ready.workers)
        .try_acquire_owned()
        .map_err(|_| AuthenticationError::unavailable())?;
    let text = twitch_copy_text(&state.authentication, request)?;
    tauri::async_runtime::spawn_blocking(move || {
        arboard::Clipboard::new().and_then(|mut clipboard| clipboard.set_text(text))
    })
    .await
    .map_err(|_| AuthenticationError::unavailable())?
    .map_err(|_| {
        AuthenticationError::new(
            AuthenticationErrorCode::Unavailable,
            "Could not copy. Select the link or code and copy it directly.",
        )
    })
}

/// Account settings need only public identity, never credential-slot references or tokens.
#[derive(Clone, Debug, Serialize, Type)]
pub struct TwitchAccountSummary {
    pub user_id: String,
    pub login: String,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct TwitchAccountsSnapshot {
    pub integration_id: String,
    pub title: String,
    pub account_selection_hint: String,
    pub roles: Vec<AccountRoleSnapshot>,
}

/// One declared account role with its public connected identity, if present.
#[derive(Clone, Debug, Serialize, Type)]
pub struct AccountRoleSnapshot {
    pub descriptor: snenk_bot::twitch::accounts::AccountRoleDescriptor,
    pub account: Option<TwitchAccountSummary>,
}

/// Reads public Twitch account identities alongside integration-owned role metadata.
#[tauri::command]
fn twitch_accounts(
    state: State<'_, DesktopBackend>,
) -> Result<TwitchAccountsSnapshot, AuthenticationError> {
    let accounts = state
        .ready()
        .map_err(|_| AuthenticationError::unavailable())?
        .services
        .twitch()
        .accounts()
        .configured_accounts()
        .map_err(|_| AuthenticationError::unavailable())?;
    let summary = |account: &snenk_bot::twitch::accounts::AccountIdentity| TwitchAccountSummary {
        user_id: account.user_id.clone(),
        login: account.login.clone(),
    };
    Ok(TwitchAccountsSnapshot {
        integration_id: "twitch".into(),
        title: snenk_bot::twitch::accounts::ACCOUNT_TITLE.into(),
        account_selection_hint: snenk_bot::twitch::accounts::ACCOUNT_SELECTION_HINT.into(),
        roles: snenk_bot::twitch::accounts::ACCOUNT_ROLES
            .iter()
            .map(|descriptor| AccountRoleSnapshot {
                descriptor: *descriptor,
                account: accounts.get(descriptor.id).map(summary),
            })
            .collect(),
    })
}

#[tauri::command]
fn twitch_login_start(
    state: State<'_, DesktopBackend>,
    request: StartTwitchLogin,
) -> Result<TwitchAuthentication, AuthenticationError> {
    let ready = state
        .ready()
        .map_err(|_| AuthenticationError::unavailable())?;
    state.authentication.start(
        ready.services.twitch().login(),
        &ready.runtime,
        request.role,
    )
}

#[tauri::command]
fn twitch_login_approve(
    state: State<'_, DesktopBackend>,
    request: ApproveTwitchLogin,
) -> Result<(), AuthenticationError> {
    let ready = state
        .ready()
        .map_err(|_| AuthenticationError::unavailable())?;
    state
        .authentication
        .approve(ready.services.twitch().login(), &ready.runtime, request)
}

#[tauri::command]
fn twitch_login_cancel(
    state: State<'_, DesktopBackend>,
    request: AuthenticationIdentity,
) -> Result<(), AuthenticationError> {
    let ready = state
        .ready()
        .map_err(|_| AuthenticationError::unavailable())?;
    state
        .authentication
        .cancel(ready.services.twitch().login(), request)
}

/// Only the stored current verification URL can leave Rust, and opening requires this command.
#[tauri::command]
async fn twitch_login_open_browser(
    state: State<'_, DesktopBackend>,
    request: AuthenticationIdentity,
) -> Result<(), AuthenticationError> {
    let ready = state
        .ready()
        .map_err(|_| AuthenticationError::unavailable())?;
    let _permit = Arc::clone(&ready.workers)
        .try_acquire_owned()
        .map_err(|_| AuthenticationError::unavailable())?;
    let url = state.authentication.verification_url(request)?;
    tauri::async_runtime::spawn_blocking(move || webbrowser::open(&url))
        .await
        .map_err(|_| AuthenticationError::unavailable())?
        .map_err(|_| AuthenticationError::unavailable())
}

#[tauri::command]
fn desktop_snapshot(state: State<'_, DesktopBackend>) -> Result<DesktopSnapshot, EditorError> {
    state.hub.snapshot().map_err(|_| unavailable())
}

#[tauri::command]
fn dismiss_error(
    state: State<'_, DesktopBackend>,
    request: DismissError,
) -> Result<(), EditorError> {
    state
        .hub
        .dismiss_error(request.id)
        .map_err(|_| unavailable())
}

#[tauri::command]
fn submit_input(
    state: State<'_, DesktopBackend>,
    request: SubmitInput,
) -> Result<(), InputBridgeError> {
    state.input.submit(&request.request_id, request.values)
}

#[tauri::command]
fn cancel_input(
    state: State<'_, DesktopBackend>,
    request: InputRequestIdentity,
) -> Result<(), InputBridgeError> {
    state.input.cancel(&request.request_id)
}

#[tauri::command]
fn run_workflow(state: State<'_, DesktopBackend>, request: RunWorkflow) -> Result<(), EditorError> {
    state
        .ready()?
        .services
        .run_manual(&request.workflow_id)
        .map_err(|error| match error {
            AppServiceError::Disabled(_) => EditorError::new(
                EditorErrorCode::InvalidEdit,
                "Enable this workflow before running it.",
            ),
            AppServiceError::Unavailable(_) => EditorError::new(
                EditorErrorCode::WorkflowUnavailable,
                "This workflow is unavailable. Check its configuration.",
            ),
            AppServiceError::Busy => EditorError::new(
                EditorErrorCode::Busy,
                "Too many runs are waiting. Try again shortly.",
            ),
            error => {
                tracing::error!(%error, "manual workflow request failed");
                unavailable()
            }
        })
}

fn history_unavailable() -> HistoryQueryError {
    HistoryQueryError {
        code: crate::history::HistoryErrorCode::Unavailable,
        message: "Run history is unavailable.".into(),
    }
}

#[tauri::command]
async fn history_page(
    state: State<'_, DesktopBackend>,
    request: HistoryQuery,
) -> Result<HistoryPage, HistoryQueryError> {
    state
        .ready()
        .map_err(|_| history_unavailable())?
        .history(move |history| history.page(request))
        .await
}

#[tauri::command]
async fn history_inspect(
    state: State<'_, DesktopBackend>,
    request: InspectHistory,
) -> Result<RunRecord, HistoryQueryError> {
    state
        .ready()
        .map_err(|_| history_unavailable())?
        .history(move |history| history.inspect(&request.run_id))
        .await
}

/// Reads retained event summaries on a bounded blocking worker.
#[tauri::command]
async fn events_list(state: State<'_, DesktopBackend>) -> Result<EventPage, EventCommandError> {
    let ready = state
        .ready()
        .map_err(|_| EventCommandError::unavailable())?;
    let permit = Arc::clone(&ready.workers)
        .try_acquire_owned()
        .map_err(|_| {
            EventCommandError::new(
                EventCommandErrorCode::Busy,
                "Events are busy. Try again shortly.",
            )
        })?;
    let services = Arc::clone(&ready.services);
    tauri::async_runtime::spawn_blocking(move || {
        let _permit = permit;
        crate::events::list(&services)
    })
    .await
    .map_err(|error| {
        tracing::error!(%error, "event list worker stopped");
        EventCommandError::unavailable()
    })?
}

/// Reads one account-scoped event on a bounded blocking worker.
#[tauri::command]
async fn events_inspect(
    state: State<'_, DesktopBackend>,
    request: EventIdentity,
) -> Result<snenk_bot::event_history::EventRecord, EventCommandError> {
    let ready = state
        .ready()
        .map_err(|_| EventCommandError::unavailable())?;
    let permit = Arc::clone(&ready.workers)
        .try_acquire_owned()
        .map_err(|_| {
            EventCommandError::new(
                EventCommandErrorCode::Busy,
                "Events are busy. Try again shortly.",
            )
        })?;
    let services = Arc::clone(&ready.services);
    tauri::async_runtime::spawn_blocking(move || {
        let _permit = permit;
        crate::events::inspect(&services, &request.event_id)
    })
    .await
    .map_err(|error| {
        tracing::error!(%error, "event inspection worker stopped");
        EventCommandError::unavailable()
    })?
}

/// Lists named consumers and automations applicable to the selected event.
#[tauri::command]
async fn events_targets(
    state: State<'_, DesktopBackend>,
    request: EventIdentity,
) -> Result<snenk_bot::app::EventTargetPage, EventCommandError> {
    let ready = state
        .ready()
        .map_err(|_| EventCommandError::unavailable())?;
    let permit = Arc::clone(&ready.workers)
        .try_acquire_owned()
        .map_err(|_| {
            EventCommandError::new(
                EventCommandErrorCode::Busy,
                "Events are busy. Try again shortly.",
            )
        })?;
    let _permit = permit;
    ready
        .services
        .event_replay_targets(&request.event_id)
        .map_err(|_| EventCommandError::unavailable())?
        .await
        .map_err(|_| EventCommandError::unavailable())?
        .map_err(|error| EventCommandError::new(EventCommandErrorCode::InvalidRequest, &error))
}

/// Replays through ordinary ingress or only to chosen named recipients.
#[tauri::command]
async fn events_replay(
    state: State<'_, DesktopBackend>,
    request: ReplayEvent,
) -> Result<snenk_bot::app::EventReplayResult, EventCommandError> {
    let ready = state
        .ready()
        .map_err(|_| EventCommandError::unavailable())?;
    let _permit = Arc::clone(&ready.workers)
        .try_acquire_owned()
        .map_err(|_| {
            EventCommandError::new(
                EventCommandErrorCode::Busy,
                "Events are busy. Try again shortly.",
            )
        })?;
    ready
        .services
        .replay_event(&request.event_id, request.targets)
        .map_err(|error| {
            tracing::warn!(%error, "event replay could not be queued");
            EventCommandError::new(
                EventCommandErrorCode::Busy,
                "Event replay is busy. Try again shortly.",
            )
        })?
        .await
        .map_err(|_| EventCommandError::unavailable())?
        .map_err(|error| {
            tracing::warn!(%error, "event replay failed");
            EventCommandError::new(EventCommandErrorCode::InvalidRequest, &error)
        })
}

/// Reads the pairing cache only for a live session declaring this authorization.
#[tauri::command]
async fn configuration_authorization_status(
    state: State<'_, DesktopBackend>,
    request: ConfigurationAuthorizationIdentity,
) -> Result<CredentialPresence, ConfigurationError> {
    let ready = state
        .ready()
        .map_err(|_| ConfigurationError::unavailable())?;
    let module = ready
        .configure(move |sessions| {
            sessions.authorization_module(&request.session_id, &request.authorization_id, false)
        })
        .await?;
    if !matches!(module, ConnectionModule::VtubeStudio) {
        return Err(unsupported_configuration_authorization());
    }
    Ok(match ready.services.vtube().authorization_present() {
        Ok(true) => CredentialPresence::Stored,
        Ok(false) => CredentialPresence::Empty,
        Err(_) => CredentialPresence::Unavailable,
    })
}

/// Starts the declared pairing operation only after its saved prerequisite is met.
#[tauri::command]
async fn configuration_authorize(
    state: State<'_, DesktopBackend>,
    request: ConfigurationAuthorizationIdentity,
) -> Result<(), ConfigurationError> {
    let ready = state
        .ready()
        .map_err(|_| ConfigurationError::unavailable())?;
    let module = ready
        .configure(move |sessions| {
            sessions.authorization_module(&request.session_id, &request.authorization_id, true)
        })
        .await?;
    if !matches!(module, ConnectionModule::VtubeStudio) {
        return Err(unsupported_configuration_authorization());
    }
    let _permit = Arc::clone(&ready.workers)
        .try_acquire_owned()
        .map_err(|_| {
            ConfigurationError::new(
                ConfigurationErrorCode::Busy,
                "Connection settings are busy. Try again shortly.",
            )
        })?;
    ready
        .services
        .authorize_vtube()
        .map_err(|_| ConfigurationError::unavailable())?
        .await
        .map_err(|_| ConfigurationError::unavailable())?
        .map_err(|_| {
            ConfigurationError::new(
                ConfigurationErrorCode::Unavailable,
                "VTube Studio could not be authorized. Check its API settings and try again.",
            )
        })
}

#[tauri::command]
async fn configuration_forget_authorization(
    state: State<'_, DesktopBackend>,
    request: ConfigurationAuthorizationIdentity,
) -> Result<(), ConfigurationError> {
    let ready = state
        .ready()
        .map_err(|_| ConfigurationError::unavailable())?;
    let module = ready
        .configure(move |sessions| {
            sessions.authorization_module(&request.session_id, &request.authorization_id, false)
        })
        .await?;
    if !matches!(module, ConnectionModule::VtubeStudio) {
        return Err(unsupported_configuration_authorization());
    }
    let _permit = Arc::clone(&ready.workers)
        .try_acquire_owned()
        .map_err(|_| {
            ConfigurationError::new(
                ConfigurationErrorCode::Busy,
                "Connection settings are busy. Try again shortly.",
            )
        })?;
    ready
        .services
        .forget_vtube_authorization()
        .map_err(|_| ConfigurationError::unavailable())?
        .await
        .map_err(|_| ConfigurationError::unavailable())?
        .map_err(|_| {
            ConfigurationError::new(
                ConfigurationErrorCode::Unavailable,
                "VTube Studio authorization could not be removed. Try again.",
            )
        })
}

fn unsupported_configuration_authorization() -> ConfigurationError {
    ConfigurationError::new(
        ConfigurationErrorCode::InvalidValues,
        "This settings view has no supported authorization.",
    )
}

/// Reads only the credential declared by an open settings session.
#[tauri::command]
async fn configuration_credential_status(
    state: State<'_, DesktopBackend>,
    request: ConfigurationCredentialIdentity,
) -> Result<CredentialPresence, ConfigurationError> {
    let ready = state
        .ready()
        .map_err(|_| ConfigurationError::unavailable())?;
    let module = ready
        .configure(move |sessions| {
            sessions.credential_module(&request.session_id, &request.credential_id)
        })
        .await?;
    match module {
        ConnectionModule::Obs => Ok(obs_credential_presence(ready)),
        ConnectionModule::Steamgriddb => {
            state
                .with_providers(|providers| {
                    Ok(if providers.key_present()? {
                        CredentialPresence::Stored
                    } else {
                        CredentialPresence::Empty
                    })
                })
                .await
        }
        _ => Err(unsupported_configuration_credential()),
    }
}

/// Saves only the credential declared by an open settings session.
#[tauri::command]
async fn configuration_credential_save(
    state: State<'_, DesktopBackend>,
    request: SaveConfigurationCredential,
) -> Result<CredentialPresence, ConfigurationError> {
    let ready = state
        .ready()
        .map_err(|_| ConfigurationError::unavailable())?;
    let session_id = request.session_id.clone();
    let credential_id = request.credential_id.clone();
    let module = ready
        .configure(move |sessions| sessions.credential_module(&session_id, &credential_id))
        .await?;
    match module {
        ConnectionModule::Obs => {
            let password = obs_password_for_save(request.value)?;
            let _permit = Arc::clone(&ready.workers)
                .try_acquire_owned()
                .map_err(|_| {
                    ConfigurationError::new(
                        ConfigurationErrorCode::Busy,
                        "Connection settings are busy. Try again shortly.",
                    )
                })?;
            ready
                .services
                .save_obs_password_typed(Some(password))
                .map_err(|_| ConfigurationError::unavailable())?
                .await
                .map_err(|_| ConfigurationError::unavailable())?
                .map_err(obs_error)?;
            Ok(obs_credential_presence(ready))
        }
        ConnectionModule::Steamgriddb => {
            state
                .with_providers(move |providers| {
                    providers.set_key(request.value.as_deref())?;
                    Ok(if request.value.is_some() {
                        CredentialPresence::Stored
                    } else {
                        CredentialPresence::Empty
                    })
                })
                .await
        }
        _ => Err(unsupported_configuration_credential()),
    }
}

/// OBS presence reads the startup credential cache, never the system keyring.
fn obs_credential_presence(ready: &DesktopCommands) -> CredentialPresence {
    match ready.services.obs().password_present() {
        Ok(true) => CredentialPresence::Stored,
        Ok(false) => CredentialPresence::Empty,
        Err(_) => CredentialPresence::Unavailable,
    }
}

/// No other configuration module currently declares a native credential.
fn unsupported_configuration_credential() -> ConfigurationError {
    ConfigurationError::new(
        ConfigurationErrorCode::InvalidValues,
        "This settings view has no supported credential.",
    )
}

/// The OBS editor intentionally offers replacement without a removal control.
fn obs_password_for_save(value: Option<String>) -> Result<String, ConfigurationError> {
    value.ok_or_else(|| {
        ConfigurationError::new(
            ConfigurationErrorCode::InvalidValues,
            "OBS password removal is unavailable.",
        )
    })
}

#[tauri::command]
async fn configuration_open(
    state: State<'_, DesktopBackend>,
    request: OpenConfiguration,
) -> Result<ConfigurationSnapshot, ConfigurationError> {
    let ready = state
        .ready()
        .map_err(|_| ConfigurationError::unavailable())?;
    let services = Arc::clone(&ready.services);
    ready
        .configure(move |sessions| {
            let loaded = match (request.module, request.record_id.as_deref()) {
                (ConnectionModule::Api, Some(id)) => LoadedConnectionSettings::ApiConsumer(
                    services
                        .api()
                        .consumer_configuration(id)
                        .map_err(crate::configuration::api_error)?,
                ),
                (ConnectionModule::Api, None) => LoadedConnectionSettings::Api(
                    services
                        .api()
                        .configuration()
                        .map_err(crate::configuration::api_error)?,
                ),
                (ConnectionModule::Steamgriddb, None) => LoadedConnectionSettings::Steamgriddb(
                    snenk_bot::providers::SteamGridDbSettings {
                        enabled: services
                            .providers()
                            .enabled(snenk_bot::providers::ProviderId::SteamGridDb)
                            .map_err(crate::configuration::provider_error)?,
                    },
                ),
                (ConnectionModule::Emotes, None) => {
                    LoadedConnectionSettings::Emotes(snenk_bot::providers::EmotesSettings {
                        enabled: services
                            .providers()
                            .enabled(snenk_bot::providers::ProviderId::Emotes)
                            .map_err(crate::configuration::provider_error)?,
                    })
                }
                (ConnectionModule::Pronouns, None) => {
                    LoadedConnectionSettings::Pronouns(snenk_bot::providers::PronounsSettings {
                        enabled: services
                            .providers()
                            .enabled(snenk_bot::providers::ProviderId::Pronouns)
                            .map_err(crate::configuration::provider_error)?,
                    })
                }
                (ConnectionModule::Obs, None) => LoadedConnectionSettings::Obs(
                    services
                        .obs()
                        .settings()
                        .load()
                        .map_err(|error| obs_error(error.into()))?,
                ),
                (ConnectionModule::VtubeStudio, None) => LoadedConnectionSettings::VtubeStudio(
                    services
                        .vtube()
                        .settings()
                        .load()
                        .map_err(|error| vtube_error(error.into()))?,
                ),
                (_, Some(_)) => {
                    return Err(ConfigurationError::new(
                        ConfigurationErrorCode::InvalidValues,
                        "This connection does not have editable records.",
                    ));
                }
            };
            sessions.open(loaded)
        })
        .await
}

#[tauri::command]
async fn configuration_save(
    state: State<'_, DesktopBackend>,
    request: SaveConfiguration,
) -> Result<ConfigurationSaveResult, ConfigurationError> {
    let ready = state
        .ready()
        .map_err(|_| ConfigurationError::unavailable())?;
    let services = Arc::clone(&ready.services);
    ready
        .configure(move |sessions| sessions.save(request, &services))
        .await
}

#[tauri::command]
async fn configuration_close(
    state: State<'_, DesktopBackend>,
    request: ConfigurationSessionRequest,
) -> Result<(), ConfigurationError> {
    state
        .ready()
        .map_err(|_| ConfigurationError::unavailable())?
        .configure(move |sessions| sessions.close(&request.session_id))
        .await
}

#[tauri::command]
async fn workflow_create(
    state: State<'_, DesktopBackend>,
    request: CreateWorkflow,
) -> Result<String, EditorError> {
    let name = request.name.trim();
    snenk_bot::workflows::validate_display_name(name)
        .map_err(|message| EditorError::new(EditorErrorCode::InvalidEdit, message))?;
    let ready = state.ready()?;
    let _permit = Arc::clone(&ready.workers)
        .try_acquire_owned()
        .map_err(|_| {
            EditorError::new(
                EditorErrorCode::Busy,
                "The editor is busy. Try again shortly.",
            )
        })?;
    ready
        .services
        .create_workflow(name)
        .map_err(|_| unavailable())?
        .await
        .map_err(|_| unavailable())?
        .map_err(|_| {
            EditorError::new(
                EditorErrorCode::SaveFailed,
                "This workflow could not be created. Try again.",
            )
        })
}

#[tauri::command]
async fn editor_open(
    state: State<'_, DesktopBackend>,
    request: OpenEditor,
) -> Result<EditorSnapshot, EditorError> {
    state
        .ready()?
        .edit(move |sessions| sessions.open(&request.workflow_id))
        .await
}

#[tauri::command]
async fn editor_value_sources(
    state: State<'_, DesktopBackend>,
    request: EditorValueSources,
) -> Result<Vec<snenk_bot::value_sources::ValueSource>, EditorError> {
    let ready = state.ready()?;
    let services = Arc::clone(&ready.services);
    ready
        .edit(move |sessions| {
            sessions.value_sources(
                &request.session_id,
                request.expected_revision,
                &request.step_id,
                services.action_schemas(),
                |kind| services.trigger_value_schema(kind),
            )
        })
        .await
}

#[tauri::command]
async fn editor_snapshot(
    state: State<'_, DesktopBackend>,
    request: EditorSessionRequest,
) -> Result<EditorSnapshot, EditorError> {
    state
        .ready()?
        .edit(move |sessions| sessions.snapshot(&request.session_id))
        .await
}

#[tauri::command]
async fn editor_apply(
    state: State<'_, DesktopBackend>,
    request: ApplyEditorEdit,
) -> Result<EditorSnapshot, EditorError> {
    state
        .ready()?
        .edit(move |sessions| {
            sessions.apply(&request.session_id, request.expected_revision, request.edit)
        })
        .await
}

#[tauri::command]
async fn editor_save(
    state: State<'_, DesktopBackend>,
    request: SaveEditor,
) -> Result<EditorSaveResult, EditorError> {
    let ready = state.ready()?;
    let repository = ready.repository.clone();
    let services = Arc::clone(&ready.services);
    state
        .ready()?
        .edit(move |sessions| {
            let mut notices = Vec::new();
            let snapshot = sessions.save_with(
                &request.session_id,
                request.expected_revision,
                |original, definition| {
                    let (saved, saved_notices) =
                        persist_editor(&services, &repository, original, definition)?;
                    notices = saved_notices;
                    Ok(saved)
                },
            )?;
            Ok(EditorSaveResult { snapshot, notices })
        })
        .await
}

#[tauri::command]
async fn editor_close(
    state: State<'_, DesktopBackend>,
    request: EditorSessionRequest,
) -> Result<(), EditorError> {
    state
        .ready()?
        .edit(move |sessions| sessions.close(&request.session_id))
        .await
}

/// Resource discovery is explicit and bounded; ordinary snapshots never query integrations.
#[tauri::command]
async fn action_choices(
    state: State<'_, DesktopBackend>,
    request: crate::choices::ActionChoices,
) -> Result<Vec<snenk_bot::choices::ConfigChoice>, EditorError> {
    let ready = state.ready()?;
    let source = crate::choices::resolve_source(ready.services.action_schemas(), &request)?;
    let _permit = Arc::clone(&ready.workers)
        .try_acquire_owned()
        .map_err(|_| {
            EditorError::new(
                EditorErrorCode::Busy,
                "Resource discovery is busy. Try again shortly.",
            )
        })?;
    ready
        .services
        .choices(source.key, request.depends_on.as_deref())
        .await
        .map_err(|error| {
            tracing::warn!(%error, "resource discovery failed");
            EditorError::new(
                EditorErrorCode::Unavailable,
                "Resources could not be loaded. Check the connection and try again.",
            )
        })
}

#[tauri::command]
fn action_definitions(
    state: State<'_, DesktopBackend>,
) -> Result<Vec<snenk_bot::schema::ActionDefinition>, EditorError> {
    Ok(state.ready()?.services.action_definitions().to_vec())
}

#[tauri::command]
fn action_schemas(state: State<'_, DesktopBackend>) -> Result<Vec<ConfigSchema>, EditorError> {
    Ok(state
        .ready()?
        .services
        .action_schemas()
        .iter()
        .map(|schema| **schema)
        .collect())
}

fn persist_editor(
    services: &AppServices,
    repository: &WorkflowRepository,
    original: &EditableWorkflow,
    definition: &WorkflowDefinition,
) -> Result<(EditableWorkflow, Vec<SaveNotice>), EditorError> {
    // Validation, persistence and route publication use the existing service path.
    let outcome = services
        .save_workflow_typed(original, definition)
        .map_err(|_| unavailable())?
        .blocking_recv()
        .map_err(|_| unavailable())?
        .map_err(save_error)?;
    let notices = outcome
        .warnings
        .into_iter()
        .map(|warning| match warning {
            SaveWarning::DirectorySync(_) => SaveNotice::DirectorySync,
            SaveWarning::BackupRetention(_) => SaveNotice::BackupRetention,
        })
        .collect();
    let saved = repository.load(&definition.workflow.id).map_err(|_| {
        EditorError::new(
            EditorErrorCode::SaveFailed,
            "The saved workflow could not be read back. Your draft is still open.",
        )
    })?;
    Ok((saved, notices))
}

fn unavailable() -> EditorError {
    EditorError::new(
        EditorErrorCode::Unavailable,
        "The application could not complete this request.",
    )
}

fn save_error(error: WorkflowSaveError) -> EditorError {
    match error {
        WorkflowSaveError::Validation(message) => {
            EditorError::new(EditorErrorCode::InvalidEdit, message)
        }
        WorkflowSaveError::Repository(WorkflowRepositoryError::Store(StoreError::Conflict(_))) => {
            EditorError::new(
                EditorErrorCode::SaveConflict,
                "This workflow changed on disk. Your draft is still open.",
            )
        }
        WorkflowSaveError::WorkerStopped => unavailable(),
        WorkflowSaveError::Activation(_) => EditorError::new(
            EditorErrorCode::SaveFailed,
            "The workflow was saved but could not be activated. Your draft is still open.",
        ),
        WorkflowSaveError::Repository(_) => EditorError::new(
            EditorErrorCode::SaveFailed,
            "The workflow could not be saved. Your draft is still open.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A missing OBS value cannot clear the stored password through this editor.
    #[test]
    fn obs_credential_cannot_be_removed_through_shared_save_command() {
        assert_eq!(
            obs_password_for_save(Some("saved".into())).unwrap(),
            "saved"
        );
        assert_eq!(
            obs_password_for_save(None).unwrap_err().code,
            ConfigurationErrorCode::InvalidValues
        );
    }

    #[test]
    fn startup_failure_remains_readable_without_admitting_application_commands() {
        let hub = Arc::new(DesktopHub::new());
        let input = Arc::new(DesktopInputProvider::new(|_| Ok(())));
        let backend = DesktopBackend::new(None, Arc::clone(&hub), input);
        hub.publish(crate::hub::DesktopEvent::Lifecycle(StartupState::Failed(
            "Could not start".into(),
        )))
        .unwrap();
        hub.report_error("Setup failed".into()).unwrap();
        assert!(matches!(
            backend.ready(),
            Err(EditorError {
                code: EditorErrorCode::Unavailable,
                ..
            })
        ));
        let snapshot = backend.hub.snapshot().unwrap();
        assert!(matches!(snapshot.startup, StartupState::Failed(_)));
        backend
            .hub
            .dismiss_error(snapshot.errors[0].id.clone())
            .unwrap();
        assert!(backend.hub.snapshot().unwrap().errors.is_empty());
    }

    #[test]
    fn persistence_errors_keep_conflict_identity_and_private_paths_out_of_ipc() {
        let conflict = save_error(WorkflowSaveError::Repository(
            StoreError::Conflict("workflow".into()).into(),
        ));
        assert_eq!(conflict.code, EditorErrorCode::SaveConflict);
        let io = save_error(WorkflowSaveError::Repository(
            StoreError::Io {
                path: "/private/user/data/workflow.json".into(),
                source: std::io::Error::other("private storage details"),
            }
            .into(),
        ));
        assert_eq!(io.code, EditorErrorCode::SaveFailed);
        assert!(!io.message.contains("/private/"));
        assert!(!io.message.contains("private storage details"));
    }
}
