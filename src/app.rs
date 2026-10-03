//! Application services shared by the desktop shell and background runtime.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use thiserror::Error;
use tokio::sync::{mpsc, oneshot, watch};

use crate::choices::{ChoiceCatalog, ConfigChoice};
use crate::engine::{ActivationOrigin, Engine, EventSink, InputProvider, Values, Workflow};
use crate::execution::{CompletedRun, ExecutionStartError, WorkflowExecutor};
use crate::history::{HistoryEntry, HistoryError, RunHistory};
use crate::integration::{ConfigurationStatus, ConnectionStatus, ReconfigurationRequest};
use crate::lua::LuaAction;
use crate::obs::settings::{LoadedObsSettings, ObsSettings, ObsSettingsError};
use crate::obs::{ObsConfigurationError, ObsIntegration, routes_for};
use crate::paths::AppPaths;
use crate::runtime::{AppRuntime, RuntimeError};
use crate::schema::{ConfigOutput, ConfigSchema};
use crate::storage::SaveOutcome;
use crate::twitch::{TwitchActivation, TwitchIntegration};
use crate::vtube::settings::{LoadedVtubeSettings, VtubeSettings, VtubeSettingsError};
use crate::vtube::{
    VtubeConfigurationError, VtubeIntegration, routes::routes_for as vtube_routes_for,
};
use crate::workflows::{
    EditableWorkflow, TriggerKind, WorkflowDefinition, WorkflowDefinitionEntry, WorkflowRepository,
    WorkflowRepositoryError,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub enum WorkflowCategory {
    Chat,
    Broadcast,
    Automation,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub struct WorkflowStatus {
    pub enabled: bool,
    pub trigger_summary: String,
    #[cfg_attr(feature = "desktop-contracts", specta(type = specta_typescript::Number))]
    pub step_count: usize,
    pub category: WorkflowCategory,
    pub capabilities: Vec<String>,
    pub capability_titles: BTreeMap<String, String>,
    /// Direct action and trigger references, including disabled definitions. Labels never contain capability IDs.
    pub integration_usage: BTreeMap<String, Vec<String>>,
    pub id: String,
    pub title: String,
    #[cfg_attr(feature = "desktop-contracts", specta(type = Option<specta_typescript::Number>))]
    pub revision: Option<u64>,
    pub has_steps: bool,
    pub error: Option<String>,
}

#[derive(Debug, Error)]
pub enum AppServiceError {
    #[error(transparent)]
    Workflows(#[from] WorkflowRepositoryError),
    #[error(transparent)]
    History(#[from] HistoryError),
    #[error(transparent)]
    Dispatcher(#[from] ExecutionStartError),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error("workflow `{0}` is unavailable")]
    Unavailable(String),
    #[error("workflow `{0}` is disabled")]
    Disabled(String),
    #[error("application services have stopped")]
    Stopped,
    #[error("too many application commands are pending")]
    Busy,
    #[error("invalid compiled configuration schema: {0}")]
    InvalidSchema(String),
}

/// Keeps persistence conflicts distinguishable from validation and activation failures.
#[derive(Debug, Error)]
pub enum WorkflowSaveError {
    #[error("{0}")]
    Validation(String),
    #[error(transparent)]
    Repository(#[from] WorkflowRepositoryError),
    #[error("workflow save worker stopped")]
    WorkerStopped,
    #[error("saved workflow could not be activated: {0}")]
    Activation(String),
}

enum WorkflowSaveReply {
    Text(oneshot::Sender<Result<SaveOutcome, String>>),
    Typed(oneshot::Sender<Result<SaveOutcome, WorkflowSaveError>>),
}

impl WorkflowSaveReply {
    fn send(self, result: Result<SaveOutcome, WorkflowSaveError>) {
        match self {
            Self::Text(reply) => {
                let _ = reply.send(result.map_err(|error| error.to_string()));
            }
            Self::Typed(reply) => {
                let _ = reply.send(result);
            }
        }
    }
}

enum SettingsSaveReply<E> {
    Text(oneshot::Sender<Result<(), String>>),
    Typed(oneshot::Sender<Result<SaveOutcome, E>>),
}

impl<E: std::fmt::Display> SettingsSaveReply<E> {
    fn send(self, result: Result<SaveOutcome, E>) {
        match self {
            Self::Text(reply) => {
                let _ = reply.send(result.map(|_| ()).map_err(|error| error.to_string()));
            }
            Self::Typed(reply) => {
                let _ = reply.send(result);
            }
        }
    }
}

enum Command {
    RunManual(String),
    RunTriggered(TriggerActivation),
    Reload(String),
    CreateWorkflow(String, oneshot::Sender<Result<String, String>>),
    SaveWorkflow {
        original: Box<EditableWorkflow>,
        definition: Box<WorkflowDefinition>,
        reply: WorkflowSaveReply,
    },
    SaveObsSettings {
        original: Option<Box<LoadedObsSettings>>,
        value: ObsSettings,
        reply: SettingsSaveReply<ObsConfigurationError>,
    },
    SaveObsPassword {
        password: Option<String>,
        reply: Option<oneshot::Sender<Result<(), ObsConfigurationError>>>,
    },
    SaveVtubeSettings {
        original: Option<Box<LoadedVtubeSettings>>,
        value: VtubeSettings,
        reply: SettingsSaveReply<VtubeConfigurationError>,
    },
    AuthorizeVtube(oneshot::Sender<Result<(), String>>),
    ForgetVtubeAuthorization(oneshot::Sender<Result<(), String>>),
}

struct TriggerActivation {
    workflow_id: String,
    trigger_id: String,
    values: Values,
    integration: &'static str,
}

/// Holds the current workflow inventory and a nonblocking command channel for UI callbacks.
pub struct AppServices {
    workflows: Arc<Mutex<Vec<WorkflowStatus>>>,
    commands: mpsc::Sender<Command>,
    twitch: Arc<TwitchIntegration>,
    obs: Arc<ObsIntegration>,
    vtube: Arc<VtubeIntegration>,
    action_schemas: Vec<&'static ConfigSchema>,
    action_definitions: Vec<crate::schema::ActionDefinition>,
    choices: ChoiceCatalog,
    configuration_sources: Vec<Arc<dyn ConfigurationStatus>>,
}

impl AppServices {
    pub fn start(
        paths: &AppPaths,
        runtime: &AppRuntime,
        input: Arc<dyn InputProvider>,
        events: Arc<dyn EventSink>,
        on_completed: impl Fn(CompletedRun) + Send + Sync + 'static,
        on_error: impl Fn(String) + Send + Sync + 'static,
        on_inventory: impl Fn(Vec<WorkflowStatus>) + Send + Sync + 'static,
    ) -> Result<Self, AppServiceError> {
        let on_error = Arc::new(on_error);
        let repository = WorkflowRepository::new(paths);
        let entries = repository.list_definitions()?;
        let history = RunHistory::new(paths.history_dir());
        for entry in history.recover()? {
            if let HistoryEntry::Corrupt { file_name, message } = entry {
                on_error(format!(
                    "History file `{file_name}` is unavailable: {message}"
                ));
            }
        }

        let mut engine = Engine::new(input, Arc::clone(&events));
        let mut choices = ChoiceCatalog::default();
        let twitch = Arc::new(TwitchIntegration::register(paths, &mut engine));
        let obs = ObsIntegration::register(paths, &mut engine, &mut choices);
        let vtube = VtubeIntegration::register(paths, &mut engine, &mut choices);
        engine.register_capability(
            "lua.run",
            1,
            Arc::new(LuaAction::with_bindings(engine.lua_bindings())),
        );
        let action_schemas = engine.action_schemas();
        let action_definitions = engine.action_definitions();
        choices
            .validate_schemas(&action_schemas)
            .map_err(AppServiceError::InvalidSchema)?;
        obs.start(runtime)?;
        twitch.start_authorization(runtime)?;

        let mut workflows = Vec::with_capacity(entries.len());
        let mut definitions: BTreeMap<String, WorkflowDefinition> = BTreeMap::new();
        for entry in entries {
            match entry {
                WorkflowDefinitionEntry::Available { id, definition } => {
                    let status = publish_definition(&engine, &mut definitions, &definition);
                    if let Some(error) = &status.error {
                        on_error(format!("Workflow `{id}` is unavailable: {error}"));
                    }
                    workflows.push(status);
                }
                WorkflowDefinitionEntry::Unavailable { id, error } => {
                    on_error(format!("Workflow `{id}` is unavailable: {error}"));
                    workflows.push(WorkflowStatus {
                        enabled: false,
                        title: id.clone(),
                        id,
                        revision: None,
                        has_steps: false,
                        trigger_summary: String::new(),
                        step_count: 0,
                        category: WorkflowCategory::Automation,
                        capabilities: Vec::new(),
                        capability_titles: BTreeMap::new(),
                        integration_usage: BTreeMap::new(),
                        error: Some(error),
                    });
                }
            }
        }
        refresh_routes(&obs, &twitch, &vtube, &definitions);

        let engine = Arc::new(engine);
        let workflows = Arc::new(Mutex::new(workflows));
        let error_sink = Arc::clone(&on_error);
        let executor = WorkflowExecutor::new(
            Arc::clone(&engine),
            events,
            history,
            on_completed,
            move |error| {
                error_sink(error);
            },
        );
        executor.start(runtime)?;

        let (commands, mut receiver) = mpsc::channel(256);
        let triggered_commands = commands.clone();
        let triggered_errors = Arc::clone(&on_error);
        obs.start_listener(runtime, move |activation| {
            let activation = TriggerActivation {
                workflow_id: activation.workflow_id,
                trigger_id: activation.trigger_id,
                values: activation.values,
                integration: "OBS",
            };
            if let Err(error) = triggered_commands.try_send(Command::RunTriggered(activation)) {
                triggered_errors(format!("OBS event could not be queued: {error}"));
            }
        })?;
        let twitch_commands = commands.clone();
        let twitch_errors = Arc::clone(&on_error);
        let twitch_listener_errors = Arc::clone(&on_error);
        twitch.start_listener(
            runtime,
            move |activation: TwitchActivation| {
                let activation = TriggerActivation {
                    workflow_id: activation.workflow_id,
                    trigger_id: activation.trigger_id,
                    values: activation.values,
                    integration: "Twitch",
                };
                if let Err(error) = twitch_commands.try_send(Command::RunTriggered(activation)) {
                    twitch_errors(format!("Twitch event could not be queued: {error}"));
                }
            },
            move |error| twitch_listener_errors(error),
        )?;
        let vtube_commands = commands.clone();
        let vtube_errors = Arc::clone(&on_error);
        let vtube_listener_errors = Arc::clone(&on_error);
        vtube.start_listener(
            runtime,
            move |activation| {
                let activation = TriggerActivation {
                    workflow_id: activation.workflow_id,
                    trigger_id: activation.trigger_id,
                    values: activation.values,
                    integration: "VTube Studio",
                };
                if let Err(error) = vtube_commands.try_send(Command::RunTriggered(activation)) {
                    vtube_errors(format!("VTube Studio event could not be queued: {error}"));
                }
            },
            move |error| vtube_listener_errors(error),
        )?;
        let worker_inventory = Arc::clone(&workflows);
        let worker_obs = Arc::clone(&obs);
        let worker_twitch = Arc::clone(&twitch);
        let worker_vtube = Arc::clone(&vtube);
        runtime.spawn_task("application commands", move |shutdown| async move {
            let mut obs_settings = worker_obs.settings().load().ok();
            let mut vtube_settings = worker_vtube.settings().load().ok();
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    command = receiver.recv() => {
                        let Some(command) = command else { break; };
                        match command {
                            Command::RunManual(id) => {
                                if let Err(error) = executor.admit(
                                    &id,
                                    Values::new(),
                                    shutdown.child_token(),
                                    ActivationOrigin::Manual,
                                ).await {
                                    on_error(format!("Could not run workflow `{id}`: {error}"));
                                }
                            }
                            Command::RunTriggered(activation) => {
                                if !definitions.get(&activation.workflow_id).is_some_and(|definition| definition.enabled && definition.triggers.iter().any(|trigger| trigger.id == activation.trigger_id && trigger.enabled)) { continue; }
                                if let Err(error) = executor.admit(
                                    &activation.workflow_id,
                                    activation.values,
                                    shutdown.child_token(),
                                    ActivationOrigin::Event(activation.trigger_id.clone()),
                                ).await {
                                    on_error(format!("{} trigger `{}` could not run workflow `{}`: {error}", activation.integration, activation.trigger_id, activation.workflow_id));
                                }
                            }
                            Command::CreateWorkflow(name, reply) => {
                                let create_repository = repository.clone();
                                let created = tokio::task::spawn_blocking(move || {
                                    let mut definition = WorkflowDefinition::manual(Workflow {
                                        id: uuid::Uuid::new_v4().to_string(),
                                        revision: 1,
                                        overlap: false,
                                        steps: Vec::new(),
                                        outputs: BTreeMap::new(),
                                    });
                                    definition.name = Some(name.trim().to_owned());
                                    create_repository.create_definition(&definition)
                                        .map_err(|error| error.to_string())?;
                                    Ok::<_, String>(definition)
                                }).await;
                                let result = match created {
                                    Ok(Ok(definition)) => {
                                        let id = definition.workflow.id.clone();
                                        let status = publish_definition(&engine, &mut definitions, &definition);
                                        if let Some(error) = &status.error {
                                            on_error(format!("Workflow `{id}` is unavailable: {error}"));
                                        }
                                        refresh_routes(&worker_obs, &worker_twitch, &worker_vtube, &definitions);
                                        let result = if let Some(error) = &status.error {
                                            Err(error.clone())
                                        } else {
                                            Ok(id)
                                        };
                                        let snapshot = update_inventory(&worker_inventory, status);
                                        on_inventory(snapshot);
                                        result
                                    }
                                    Ok(Err(error)) => Err(error),
                                    Err(error) => Err(format!("workflow creator stopped: {error}")),
                                };
                                let _ = reply.send(result);
                            }
                            Command::SaveWorkflow { original, definition, reply } => {
                                let result = runtime_trigger_error(&definition)
                                    .and_then(|_| engine.validate_available_workflow(&definition.workflow))
                                    .map_err(WorkflowSaveError::Validation);
                                let result = match result {
                                    Ok(()) => {
                                        let save_repository = repository.clone();
                                        let edited = definition.clone();
                                        match tokio::task::spawn_blocking(move || {
                                            save_repository.save_definition(&original, &edited)
                                                .map_err(WorkflowSaveError::Repository)
                                        }).await {
                                            Ok(result) => result,
                                            Err(error) => {
                                                tracing::error!(%error, "workflow save worker stopped");
                                                Err(WorkflowSaveError::WorkerStopped)
                                            }
                                        }
                                    }
                                    Err(error) => Err(error),
                                };
                                let result = match result {
                                    Ok(outcome) => {
                                        let status = publish_definition(&engine, &mut definitions, &definition);
                                        refresh_routes(&worker_obs, &worker_twitch, &worker_vtube, &definitions);
                                        let result = match &status.error {
                                            Some(error) => {
                                                on_error(format!("Saved workflow could not be activated: {error}"));
                                                Err(WorkflowSaveError::Activation(error.clone()))
                                            }
                                            None => Ok(outcome),
                                        };
                                        let snapshot = update_inventory(&worker_inventory, status);
                                        on_inventory(snapshot);
                                        result
                                    }
                                    Err(error) => Err(error),
                                };
                                reply.send(result);
                            }
                            Command::Reload(id) => {
                                let repository = repository.clone();
                                let reload_id = id.clone();
                                let loaded = tokio::task::spawn_blocking(move || repository.load(&reload_id)).await;
                                let status = match loaded {
                                    Ok(Ok(loaded)) => {
                                        let status = publish_definition(&engine, &mut definitions, loaded.definition());
                                        if let Some(error) = &status.error {
                                            on_error(format!("Workflow `{id}` is unavailable: {error}"));
                                        }
                                        status
                                    }
                                    result => {
                                        engine.unregister_workflow(&id);
                                        definitions.remove(&id);
                                        let error = match result {
                                            Ok(Err(error)) => error,
                                            Err(error) => format!("workflow reload worker stopped: {error}"),
                                            Ok(Ok(_)) => unreachable!(),
                                        };
                                        on_error(format!("Workflow `{id}` is unavailable: {error}"));
                                        WorkflowStatus {
                                            enabled: false,
                                            title: id.clone(),
                                            id,
                                            revision: None,
                                            has_steps: false,
                        trigger_summary: String::new(),
                        step_count: 0,
                        category: WorkflowCategory::Automation,
                        capabilities: Vec::new(),
                        capability_titles: BTreeMap::new(),
                        integration_usage: BTreeMap::new(),
                                            error: Some(error),
                                        }
                                    }
                                };
                                refresh_routes(&worker_obs, &worker_twitch, &worker_vtube, &definitions);
                                let snapshot = update_inventory(&worker_inventory, status);
                                on_inventory(snapshot);
                            }
                            Command::SaveObsSettings { original, value, reply } => {
                                let loaded = original.map(|loaded| *loaded).or_else(|| obs_settings.take());
                                let result = if let Some(loaded) = loaded {
                                    worker_obs.save_settings(loaded, value).await
                                } else {
                                    Err(ObsSettingsError::InvalidFormat.into())
                                };
                                obs_settings = worker_obs.settings().load().ok();
                                if let Err(error) = &result {
                                    on_error(format!("Could not save OBS settings: {error}"));
                                }
                                reply.send(result);
                            }
                            Command::SaveObsPassword { password, reply } => {
                                let result = worker_obs.save_password(password).await;
                                if let Err(error) = &result {
                                    on_error(format!("Could not save OBS password: {error}"));
                                }
                                if let Some(reply) = reply { let _ = reply.send(result); }
                            }
                            Command::SaveVtubeSettings { original, value, reply } => {
                                let loaded = original.map(|loaded| *loaded).or_else(|| vtube_settings.take());
                                let result = if let Some(loaded) = loaded {
                                    worker_vtube.save_settings(loaded, value).await
                                } else {
                                    Err(VtubeSettingsError::InvalidFormat.into())
                                };
                                vtube_settings = worker_vtube.settings().load().ok();
                                if let Err(error) = &result {
                                    on_error(format!("Could not save VTube Studio settings: {error}"));
                                }
                                reply.send(result);
                            }
                            Command::AuthorizeVtube(reply) => {
                                let vtube = Arc::clone(&worker_vtube);
                                let errors = Arc::clone(&on_error);
                                let cancel = shutdown.child_token();
                                tokio::spawn(async move {
                                    let result = tokio::select! {
                                        _ = cancel.cancelled() => Err("VTube Studio authorization was cancelled".into()),
                                        result = vtube.authorize() => result.map_err(|error| error.to_string()),
                                    };
                                    if let Err(error) = &result {
                                        errors(format!("Could not authorize VTube Studio: {error}"));
                                    }
                                    let _ = reply.send(result);
                                });
                            }
                            Command::ForgetVtubeAuthorization(reply) => {
                                let result = worker_vtube.forget_authorization().await.map_err(|error| error.to_string());
                                if let Err(error) = &result {
                                    on_error(format!("Could not remove VTube Studio authorization: {error}"));
                                }
                                let _ = reply.send(result);
                            }
                        }
                    }
                }
            }
            Ok::<(), std::convert::Infallible>(())
        })?;
        let configuration_sources: Vec<Arc<dyn ConfigurationStatus>> =
            vec![twitch.clone(), obs.clone(), vtube.clone()];
        Ok(Self {
            configuration_sources,
            workflows,
            commands,
            twitch,
            obs,
            vtube,
            action_schemas,
            action_definitions,
            choices,
        })
    }

    pub fn reconfiguration_requests(&self) -> Vec<ReconfigurationRequest> {
        self.configuration_sources
            .iter()
            .flat_map(|source| source.reconfiguration_requests())
            .collect()
    }

    pub fn connection_statuses(&self) -> Vec<ConnectionStatus> {
        self.configuration_sources
            .iter()
            .flat_map(|source| source.connection_statuses())
            .collect()
    }

    /// Subscribe before taking the initial snapshot to retain concurrent changes.
    pub fn subscribe_configuration_changes(&self) -> Vec<watch::Receiver<u64>> {
        self.configuration_sources
            .iter()
            .flat_map(|source| source.subscribe_configuration_changes())
            .collect()
    }

    pub fn twitch(&self) -> &TwitchIntegration {
        &self.twitch
    }

    pub fn obs(&self) -> &Arc<ObsIntegration> {
        &self.obs
    }

    pub fn vtube(&self) -> &Arc<VtubeIntegration> {
        &self.vtube
    }

    pub fn action_definitions(&self) -> &[crate::schema::ActionDefinition] {
        &self.action_definitions
    }

    pub fn action_schemas(&self) -> &[&'static ConfigSchema] {
        &self.action_schemas
    }

    pub fn trigger_value_schema(&self, kind: &TriggerKind) -> Option<Vec<ConfigOutput>> {
        crate::obs::trigger_value_schema(kind)
            .or_else(|| crate::twitch::trigger_value_schema(kind))
            .or_else(|| crate::vtube::trigger_value_schema(kind))
    }

    pub async fn choices(
        &self,
        source: &str,
        depends_on: Option<&str>,
    ) -> Result<Vec<ConfigChoice>, String> {
        self.choices.choices(source, depends_on).await
    }

    pub fn list_workflows(&self) -> Vec<WorkflowStatus> {
        self.workflows
            .lock()
            .expect("workflow inventory lock poisoned")
            .clone()
    }

    /// Queues a manual run without waiting for disk, credentials, or the network.
    pub fn run_manual(&self, id: &str) -> Result<(), AppServiceError> {
        if !self
            .list_workflows()
            .iter()
            .any(|entry| entry.id == id && entry.error.is_none())
        {
            return Err(AppServiceError::Unavailable(id.to_owned()));
        }
        if self
            .list_workflows()
            .iter()
            .any(|entry| entry.id == id && !entry.enabled)
        {
            return Err(AppServiceError::Disabled(id.to_owned()));
        }
        self.send(Command::RunManual(id.to_owned()))
    }

    /// Re-reads a saved workflow and updates the live execution registry.
    pub fn reload_workflow(&self, id: &str) -> Result<(), AppServiceError> {
        self.send(Command::Reload(id.to_owned()))
    }

    pub fn create_workflow(
        &self,
        name: &str,
    ) -> Result<oneshot::Receiver<Result<String, String>>, AppServiceError> {
        let (reply, receiver) = oneshot::channel();
        self.send(Command::CreateWorkflow(name.to_owned(), reply))?;
        Ok(receiver)
    }

    pub fn save_workflow(
        &self,
        original: &EditableWorkflow,
        definition: &WorkflowDefinition,
    ) -> Result<oneshot::Receiver<Result<SaveOutcome, String>>, AppServiceError> {
        let (reply, receiver) = oneshot::channel();
        self.send(Command::SaveWorkflow {
            original: Box::new(original.clone()),
            definition: Box::new(definition.clone()),
            reply: WorkflowSaveReply::Text(reply),
        })?;
        Ok(receiver)
    }

    /// The desktop adapter retains typed save failures without parsing diagnostic text.
    pub fn save_workflow_typed(
        &self,
        original: &EditableWorkflow,
        definition: &WorkflowDefinition,
    ) -> Result<oneshot::Receiver<Result<SaveOutcome, WorkflowSaveError>>, AppServiceError> {
        let (reply, receiver) = oneshot::channel();
        self.send(Command::SaveWorkflow {
            original: Box::new(original.clone()),
            definition: Box::new(definition.clone()),
            reply: WorkflowSaveReply::Typed(reply),
        })?;
        Ok(receiver)
    }

    pub fn save_obs_settings(
        &self,
        value: ObsSettings,
    ) -> Result<oneshot::Receiver<Result<(), String>>, AppServiceError> {
        let (reply, receiver) = oneshot::channel();
        self.send(Command::SaveObsSettings {
            original: None,
            value,
            reply: SettingsSaveReply::Text(reply),
        })?;
        Ok(receiver)
    }

    /// Saves against the caller's original snapshot, preserving external-write conflicts.
    pub fn save_obs_settings_typed(
        &self,
        original: LoadedObsSettings,
        value: ObsSettings,
    ) -> Result<oneshot::Receiver<Result<SaveOutcome, ObsConfigurationError>>, AppServiceError>
    {
        let (reply, receiver) = oneshot::channel();
        self.send(Command::SaveObsSettings {
            original: Some(Box::new(original)),
            value,
            reply: SettingsSaveReply::Typed(reply),
        })?;
        Ok(receiver)
    }

    pub fn save_vtube_settings_typed(
        &self,
        original: LoadedVtubeSettings,
        value: VtubeSettings,
    ) -> Result<oneshot::Receiver<Result<SaveOutcome, VtubeConfigurationError>>, AppServiceError>
    {
        let (reply, receiver) = oneshot::channel();
        self.send(Command::SaveVtubeSettings {
            original: Some(Box::new(original)),
            value,
            reply: SettingsSaveReply::Typed(reply),
        })?;
        Ok(receiver)
    }

    pub fn save_obs_password(&self, password: Option<String>) -> Result<(), AppServiceError> {
        self.send(Command::SaveObsPassword {
            password,
            reply: None,
        })
    }

    /// Reports credential persistence completion without returning credential material.
    pub fn save_obs_password_typed(
        &self,
        password: Option<String>,
    ) -> Result<oneshot::Receiver<Result<(), ObsConfigurationError>>, AppServiceError> {
        let (reply, receiver) = oneshot::channel();
        self.send(Command::SaveObsPassword {
            password,
            reply: Some(reply),
        })?;
        Ok(receiver)
    }

    pub fn save_vtube_settings(
        &self,
        value: VtubeSettings,
    ) -> Result<oneshot::Receiver<Result<(), String>>, AppServiceError> {
        let (reply, receiver) = oneshot::channel();
        self.send(Command::SaveVtubeSettings {
            original: None,
            value,
            reply: SettingsSaveReply::Text(reply),
        })?;
        Ok(receiver)
    }

    /// Call only from an explicit authorization control: VTube Studio displays a prompt.
    pub fn authorize_vtube(
        &self,
    ) -> Result<oneshot::Receiver<Result<(), String>>, AppServiceError> {
        let (reply, receiver) = oneshot::channel();
        self.send(Command::AuthorizeVtube(reply))?;
        Ok(receiver)
    }

    pub fn forget_vtube_authorization(
        &self,
    ) -> Result<oneshot::Receiver<Result<(), String>>, AppServiceError> {
        let (reply, receiver) = oneshot::channel();
        self.send(Command::ForgetVtubeAuthorization(reply))?;
        Ok(receiver)
    }

    fn send(&self, command: Command) -> Result<(), AppServiceError> {
        self.commands
            .try_send(command)
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => AppServiceError::Busy,
                mpsc::error::TrySendError::Closed(_) => AppServiceError::Stopped,
            })
    }
}

fn workflow_trigger_summary(definition: &WorkflowDefinition) -> String {
    let Some(trigger) = definition.triggers.first() else {
        return "No triggers".into();
    };
    let summary = match &trigger.kind {
        TriggerKind::Manual => "Manual".into(),
        TriggerKind::ObsRecordingStarted => "OBS recording started".into(),
        TriggerKind::ObsCurrentScene { scene } => scene.clone(),
        TriggerKind::IntegrationEvent {
            integration,
            event,
            filters,
        } if integration == "twitch" && event == "chat.command" => filters
            .get("command")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| "Twitch chat command".into()),
        kind => crate::obs::trigger_title(kind)
            .or_else(|| crate::twitch::events::trigger_title(kind))
            .or_else(|| crate::vtube::trigger_title(kind))
            .unwrap_or("Unavailable trigger")
            .into(),
    };
    if definition.triggers.len() > 1 {
        format!("{summary} +{}", definition.triggers.len() - 1)
    } else {
        summary
    }
}

fn workflow_category(definition: &WorkflowDefinition) -> WorkflowCategory {
    match definition.triggers.first().map(|trigger| &trigger.kind) {
        Some(TriggerKind::ObsRecordingStarted | TriggerKind::ObsCurrentScene { .. }) => {
            WorkflowCategory::Broadcast
        }
        Some(TriggerKind::IntegrationEvent { integration, .. }) if integration == "obs" => {
            WorkflowCategory::Broadcast
        }
        Some(TriggerKind::IntegrationEvent { integration, .. }) if integration == "twitch" => {
            WorkflowCategory::Chat
        }
        _ => WorkflowCategory::Automation,
    }
}

fn count_workflow_steps(steps: &[crate::engine::Step]) -> usize {
    use crate::engine::StepKind;
    steps
        .iter()
        .map(|step| {
            1 + match &step.kind {
                StepKind::If {
                    then_steps,
                    else_steps,
                    ..
                } => count_workflow_steps(then_steps) + count_workflow_steps(else_steps),
                StepKind::While { steps, .. } | StepKind::OneOrMore { steps } => {
                    count_workflow_steps(steps)
                }
                _ => 0,
            }
        })
        .sum()
}

fn workflow_capabilities(steps: &[crate::engine::Step]) -> Vec<String> {
    use crate::engine::StepKind;
    let mut capabilities = Vec::new();
    for step in steps {
        match &step.kind {
            StepKind::Action { capability, .. } => capabilities.push(capability.clone()),
            StepKind::If {
                then_steps,
                else_steps,
                ..
            } => {
                capabilities.extend(workflow_capabilities(then_steps));
                capabilities.extend(workflow_capabilities(else_steps));
            }
            StepKind::While { steps, .. } | StepKind::OneOrMore { steps } => {
                capabilities.extend(workflow_capabilities(steps))
            }
            _ => {}
        }
    }
    capabilities.sort();
    capabilities.dedup();
    capabilities
}

fn workflow_integration_usage(
    definition: &WorkflowDefinition,
    capabilities: &[String],
    titles: &BTreeMap<String, String>,
) -> BTreeMap<String, Vec<String>> {
    let mut usage: BTreeMap<String, Vec<String>> = BTreeMap::new();
    // These are the namespaces owned by the three compiled integrations. Lua bodies
    // and called workflows are opaque here; this reports references in this definition.
    for capability in capabilities {
        let integration = match capability.split_once('.').map(|(namespace, _)| namespace) {
            Some("obs") => "obs",
            Some("twitch") => "twitch",
            Some("vtube") => "vtube_studio",
            _ => continue,
        };
        usage.entry(integration.into()).or_default().push(
            titles
                .get(capability)
                .cloned()
                .unwrap_or_else(|| "Unavailable action".into()),
        );
    }
    for trigger in &definition.triggers {
        let (integration, label) = match &trigger.kind {
            TriggerKind::ObsRecordingStarted | TriggerKind::ObsCurrentScene { .. } => {
                ("obs", crate::obs::trigger_title(&trigger.kind))
            }
            TriggerKind::IntegrationEvent { integration, .. } => match integration.as_str() {
                "obs" => ("obs", crate::obs::trigger_title(&trigger.kind)),
                "twitch" => (
                    "twitch",
                    crate::twitch::events::trigger_title(&trigger.kind),
                ),
                "vtube_studio" => ("vtube_studio", crate::vtube::trigger_title(&trigger.kind)),
                _ => continue,
            },
            TriggerKind::Manual => continue,
        };
        usage
            .entry(integration.into())
            .or_default()
            .push(label.unwrap_or("Unavailable trigger").into());
    }
    for labels in usage.values_mut() {
        labels.sort();
        labels.dedup();
    }
    usage
}

fn publish_definition(
    engine: &Engine,
    definitions: &mut BTreeMap<String, WorkflowDefinition>,
    definition: &WorkflowDefinition,
) -> WorkflowStatus {
    let id = definition.workflow.id.clone();
    let result = runtime_trigger_error(definition)
        .and_then(|_| engine.validate_available_workflow(&definition.workflow))
        .and_then(|_| {
            if definition.enabled {
                engine.register_workflow(definition.workflow.clone())
            } else {
                engine.unregister_workflow(&id);
                Ok(())
            }
        });
    let capabilities = workflow_capabilities(&definition.workflow.steps);
    let capability_titles: BTreeMap<_, _> = engine
        .action_schemas()
        .into_iter()
        .filter(|schema| {
            capabilities
                .iter()
                .any(|capability| capability == schema.id)
        })
        .map(|schema| (schema.id.to_owned(), schema.title.to_owned()))
        .collect();
    let integration_usage =
        workflow_integration_usage(definition, &capabilities, &capability_titles);
    match result {
        Ok(()) => {
            definitions.insert(id.clone(), definition.clone());
            WorkflowStatus {
                enabled: definition.enabled,
                id,
                title: definition.title().to_owned(),
                revision: Some(definition.workflow.revision),
                has_steps: !definition.workflow.steps.is_empty(),
                trigger_summary: workflow_trigger_summary(definition),
                step_count: count_workflow_steps(&definition.workflow.steps),
                category: workflow_category(definition),
                capabilities,
                capability_titles,
                integration_usage,
                error: None,
            }
        }
        Err(error) => {
            engine.unregister_workflow(&id);
            definitions.remove(&id);
            WorkflowStatus {
                enabled: definition.enabled,
                id,
                title: definition.title().to_owned(),
                revision: Some(definition.workflow.revision),
                has_steps: !definition.workflow.steps.is_empty(),
                trigger_summary: workflow_trigger_summary(definition),
                step_count: count_workflow_steps(&definition.workflow.steps),
                category: workflow_category(definition),
                capabilities,
                capability_titles,
                integration_usage,
                error: Some(error),
            }
        }
    }
}

fn refresh_routes(
    obs: &ObsIntegration,
    twitch: &TwitchIntegration,
    vtube: &VtubeIntegration,
    definitions: &BTreeMap<String, WorkflowDefinition>,
) {
    obs.set_routes(
        definitions
            .values()
            .filter(|definition| definition.enabled)
            .flat_map(routes_for)
            .collect(),
    );
    twitch.set_routes(
        definitions
            .values()
            .filter(|definition| definition.enabled)
            .cloned()
            .collect(),
    );
    vtube.set_routes(
        definitions
            .values()
            .filter(|definition| definition.enabled)
            .flat_map(vtube_routes_for)
            .collect(),
    );
}

fn update_inventory(
    workflows: &Arc<Mutex<Vec<WorkflowStatus>>>,
    status: WorkflowStatus,
) -> Vec<WorkflowStatus> {
    let mut inventory = workflows.lock().expect("workflow inventory lock poisoned");
    match inventory.binary_search_by(|entry| entry.id.cmp(&status.id)) {
        Ok(index) => inventory[index] = status,
        Err(index) => inventory.insert(index, status),
    }
    inventory.clone()
}

fn runtime_trigger_error(definition: &WorkflowDefinition) -> Result<(), String> {
    for trigger in definition.triggers.iter().filter(|trigger| trigger.enabled) {
        if let TriggerKind::IntegrationEvent { integration, .. } = &trigger.kind {
            let validation = match integration.as_str() {
                "twitch" => crate::twitch::events::validate_trigger(trigger),
                "obs" => crate::obs::events::validate_trigger(trigger),
                "vtube_studio" => crate::vtube::routes::validate_trigger(trigger),
                _ => {
                    return Err(format!(
                        "trigger `{}` requires integration `{integration}` that is not implemented",
                        trigger.id
                    ));
                }
            };
            validation.map_err(|error| format!("trigger `{}`: {error}", trigger.id))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::sync::mpsc;
    use std::time::Duration;

    use tempfile::tempdir;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::engine::{
        Event, FailurePolicy, Input, InputField, InputResponse, Outcome, Step, StepKind, Workflow,
    };
    use crate::history::{RunOutcome, TriggerKind};
    use crate::obs::settings::ObsSettingsStore;
    use crate::workflows::{
        TriggerDefinition, TriggerKind as WorkflowTriggerKind, WorkflowDefinition,
    };

    struct TestInput;

    impl InputProvider for TestInput {
        fn request<'a>(
            &'a self,
            _title: String,
            _fields: Vec<InputField>,
            _defaults: Values,
            _validators: &'a crate::engine::FormValidators,
            _cancel: CancellationToken,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<InputResponse, String>> + Send + 'a>,
        > {
            Box::pin(async { Err("test did not expect an input request".to_owned()) })
        }
    }

    fn input() -> Arc<dyn InputProvider> {
        Arc::new(TestInput)
    }

    fn events() -> Arc<dyn EventSink> {
        Arc::new(|_: Event| {})
    }

    fn paths(root: &std::path::Path) -> AppPaths {
        AppPaths {
            config: root.join("config"),
            data: root.join("data"),
            state: root.join("state"),
        }
    }

    fn workflow(id: &str) -> Workflow {
        Workflow {
            id: id.to_owned(),
            revision: 1,
            overlap: false,
            steps: Vec::new(),
            outputs: BTreeMap::from([(
                "result".to_owned(),
                Input::Literal(serde_json::json!(true)),
            )]),
        }
    }

    #[test]
    fn library_summaries_use_module_labels_without_changing_trigger_identity() {
        let mut definition = WorkflowDefinition::manual(workflow("labels"));
        for (integration, event, title) in [
            ("twitch", "ad_break.begin", "Twitch ad break"),
            (
                "twitch",
                "channel.details_changed",
                "Twitch title or game changed",
            ),
            ("obs", "recording.stopped", "OBS recording stopped"),
            (
                "obs",
                "scene_item.enabled_changed",
                "OBS scene item visibility changed",
            ),
            ("vtube_studio", "model.loaded", "VTube Studio model loaded"),
            (
                "vtube_studio",
                "hotkey.triggered",
                "VTube Studio hotkey triggered",
            ),
            ("unknown", "private.event_id", "Unavailable trigger"),
        ] {
            definition.triggers[0].kind = WorkflowTriggerKind::IntegrationEvent {
                integration: integration.into(),
                event: event.into(),
                filters: BTreeMap::new(),
            };
            assert_eq!(workflow_trigger_summary(&definition), title);
            let WorkflowTriggerKind::IntegrationEvent { event: stored, .. } =
                &definition.triggers[0].kind
            else {
                panic!("identity changed")
            };
            assert_eq!(stored, event);
        }
        definition.triggers[0].kind = WorkflowTriggerKind::IntegrationEvent {
            integration: "twitch".into(),
            event: "chat.command".into(),
            filters: BTreeMap::from([("command".into(), "!hello".into())]),
        };
        definition.triggers.push(TriggerDefinition {
            id: "manual".into(),
            enabled: true,
            kind: WorkflowTriggerKind::Manual,
        });
        assert_eq!(workflow_trigger_summary(&definition), "!hello +1");
    }

    #[test]
    fn integration_usage_includes_disabled_trigger_only_references_and_friendly_actions() {
        let mut definition = WorkflowDefinition::manual(workflow("usage"));
        definition.enabled = false;
        definition.triggers[0] = TriggerDefinition {
            id: "record".into(),
            enabled: false,
            kind: WorkflowTriggerKind::ObsRecordingStarted,
        };
        definition.triggers.push(TriggerDefinition {
            id: "vtube".into(),
            enabled: false,
            kind: WorkflowTriggerKind::IntegrationEvent {
                integration: "vtube_studio".into(),
                event: "model.loaded".into(),
                filters: BTreeMap::new(),
            },
        });
        let usage = workflow_integration_usage(
            &definition,
            &[
                "twitch.chat.send".into(),
                "twitch.chat.send".into(),
                "obs.missing".into(),
                "lua.run".into(),
                "custom.private".into(),
            ],
            &BTreeMap::from([("twitch.chat.send".into(), "Send chat message".into())]),
        );
        assert_eq!(
            usage["obs"],
            ["OBS recording started", "Unavailable action"]
        );
        assert_eq!(usage["twitch"], ["Send chat message"]);
        assert_eq!(usage["vtube_studio"], ["VTube Studio model loaded"]);
        assert_eq!(usage.len(), 3);
        assert!(
            !serde_json::to_string(&usage)
                .unwrap()
                .contains("obs.missing")
        );
        assert!(!definition.enabled);
        assert!(!definition.triggers[0].enabled);
    }

    #[test]
    fn integration_usage_retains_unavailable_trigger_references_without_exposing_event_ids() {
        let mut definition = WorkflowDefinition::manual(workflow("usage"));
        definition.triggers[0].kind = WorkflowTriggerKind::IntegrationEvent {
            integration: "obs".into(),
            event: "private.event".into(),
            filters: BTreeMap::new(),
        };
        assert_eq!(
            workflow_integration_usage(&definition, &[], &BTreeMap::new())["obs"],
            ["Unavailable trigger"]
        );
        assert_eq!(workflow_category(&definition), WorkflowCategory::Broadcast);
        definition.triggers[0].kind = WorkflowTriggerKind::Manual;
        assert!(workflow_integration_usage(&definition, &[], &BTreeMap::new()).is_empty());
    }

    #[test]
    fn disabled_workflow_stays_editable_but_cannot_start() {
        let directory = tempdir().unwrap();
        let paths = paths(directory.path());
        let repository = WorkflowRepository::new(&paths);
        let mut definition = WorkflowDefinition::manual(workflow("disabled"));
        definition.enabled = false;
        repository.create_definition(&definition).unwrap();
        let mut runtime = AppRuntime::new(|_| {}).unwrap();
        let services =
            AppServices::start(&paths, &runtime, input(), events(), |_| {}, |_| {}, |_| {})
                .unwrap();
        let status = services.list_workflows().pop().unwrap();
        assert!(!status.enabled);
        assert!(status.error.is_none());
        assert!(matches!(
            services.run_manual("disabled"),
            Err(AppServiceError::Disabled(_))
        ));
        runtime.shutdown(Duration::from_secs(5)).unwrap();
    }

    #[test]
    fn loads_valid_files_preserves_invalid_files_and_runs_without_blocking_ui() {
        let directory = tempdir().unwrap();
        let paths = paths(directory.path());
        let repository = WorkflowRepository::new(&paths);
        repository.create(&workflow("alpha")).unwrap();
        let invalid_path = paths.workflows_dir().join("invalid.json");
        let invalid_bytes = b"not JSON";
        fs::write(&invalid_path, invalid_bytes).unwrap();

        let (completed_tx, completed_rx) = mpsc::channel();
        let (error_tx, error_rx) = mpsc::channel();
        let mut runtime = AppRuntime::new(|_| {}).unwrap();
        let services = AppServices::start(
            &paths,
            &runtime,
            input(),
            events(),
            move |run| completed_tx.send(run).unwrap(),
            move |error| error_tx.send(error).unwrap(),
            |_| {},
        )
        .unwrap();
        let schemas = services.action_schemas();
        for id in [
            "twitch.send_chat",
            "twitch.get_channel",
            "twitch.set_channel",
            "twitch.find_game",
            "twitch.find_user",
            "obs.set_current_program_scene",
            "obs.set_scene_item_enabled",
            "obs.create_record_chapter",
        ] {
            assert!(schemas.iter().any(|schema| schema.id == id), "missing {id}");
        }
        let chat = schemas
            .iter()
            .find(|schema| schema.id == "twitch.send_chat")
            .unwrap();
        assert_eq!(chat.version, 1);
        assert_eq!(chat.fields[0].id, "message");
        let definitions = services.action_definitions();
        assert_eq!(definitions.len(), schemas.len());
        for schema in schemas {
            assert!(
                definitions
                    .iter()
                    .any(|definition| definition.schema == *schema)
            );
        }
        let mute = definitions
            .iter()
            .find(|entry| entry.schema.id == "obs.set_input_mute")
            .unwrap();
        assert_eq!(mute.defaults.get("muted"), Some(&serde_json::json!(false)));
        let chat_defaults = definitions
            .iter()
            .find(|entry| entry.schema.id == "twitch.send_chat")
            .unwrap();
        assert_eq!(
            chat_defaults.defaults.get("message"),
            Some(&serde_json::json!(""))
        );
        assert!(!chat_defaults.defaults.contains_key("broadcaster_fallback"));
        let channel_defaults = definitions
            .iter()
            .find(|entry| entry.schema.id == "twitch.set_channel")
            .unwrap();
        assert!(channel_defaults.defaults.is_empty());

        assert_eq!(services.list_workflows().len(), 2);
        assert!(services.list_workflows().iter().any(|entry| {
            entry.id == "alpha" && entry.revision == Some(1) && entry.error.is_none()
        }));
        assert!(services.list_workflows().iter().any(|entry| {
            entry.id == "invalid" && entry.revision.is_none() && entry.error.is_some()
        }));
        assert!(error_rx.try_recv().unwrap().contains("invalid"));
        assert!(matches!(
            services.run_manual("invalid"),
            Err(AppServiceError::Unavailable(_))
        ));
        services.run_manual("alpha").unwrap();
        let completed = completed_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(completed.workflow_id, "alpha");
        assert_eq!(completed.outcome, Outcome::Success);
        let record = RunHistory::new(paths.history_dir())
            .load(&completed.run_id)
            .unwrap();
        assert_eq!(record.outcome, RunOutcome::Succeeded);
        assert_eq!(record.trigger.kind, TriggerKind::Manual);
        assert_eq!(fs::read(&invalid_path).unwrap(), invalid_bytes);

        drop(services);
        runtime.shutdown(Duration::from_secs(5)).unwrap();

        let mut runtime = AppRuntime::new(|_| {}).unwrap();
        let restarted =
            AppServices::start(&paths, &runtime, input(), events(), |_| {}, |_| {}, |_| {})
                .unwrap();
        assert_eq!(restarted.list_workflows().len(), 2);
        assert_eq!(
            RunHistory::new(paths.history_dir())
                .load(&completed.run_id)
                .unwrap()
                .outcome,
            RunOutcome::Succeeded
        );
        drop(restarted);
        runtime.shutdown(Duration::from_secs(5)).unwrap();
    }

    #[test]
    fn startup_recovers_interrupted_run() {
        let directory = tempdir().unwrap();
        let paths = paths(directory.path());
        let history = RunHistory::new(paths.history_dir());
        let running = history
            .begin(
                "alpha",
                1,
                crate::history::TriggerIdentity {
                    kind: TriggerKind::Manual,
                    id: None,
                },
            )
            .unwrap();
        let mut runtime = AppRuntime::new(|_| {}).unwrap();
        let services =
            AppServices::start(&paths, &runtime, input(), events(), |_| {}, |_| {}, |_| {})
                .unwrap();
        assert_eq!(
            history.load(&running.run_id).unwrap().outcome,
            RunOutcome::Interrupted
        );
        drop(services);
        runtime.shutdown(Duration::from_secs(5)).unwrap();
    }

    #[test]
    fn reload_registers_saved_edits_and_disables_invalidated_workflows() {
        let directory = tempdir().unwrap();
        let paths = paths(directory.path());
        let repository = WorkflowRepository::new(&paths);
        repository.create(&workflow("alpha")).unwrap();
        let (inventory_tx, inventory_rx) = mpsc::channel();
        let mut runtime = AppRuntime::new(|_| {}).unwrap();
        let services = AppServices::start(
            &paths,
            &runtime,
            input(),
            events(),
            |_| {},
            |_| {},
            move |inventory| inventory_tx.send(inventory).unwrap(),
        )
        .unwrap();

        let loaded = repository.load("alpha").unwrap();
        let mut updated = workflow("alpha");
        updated.revision = 2;
        repository.save(&loaded, &updated).unwrap();
        services.reload_workflow("alpha").unwrap();
        let inventory = inventory_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(inventory[0].revision, Some(2));
        assert_eq!(services.list_workflows()[0].revision, Some(2));

        let file = paths.workflows_dir().join("alpha.json");
        fs::write(&file, b"invalid JSON").unwrap();
        services.reload_workflow("alpha").unwrap();
        let inventory = inventory_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(inventory[0].error.is_some());
        assert!(matches!(
            services.run_manual("alpha"),
            Err(AppServiceError::Unavailable(_))
        ));
        assert_eq!(fs::read(file).unwrap(), b"invalid JSON");

        drop(services);
        runtime.shutdown(Duration::from_secs(5)).unwrap();
    }

    #[test]
    fn create_workflow_saves_a_named_draft_and_publishes_it_to_the_inventory() {
        let directory = tempdir().unwrap();
        let paths = paths(directory.path());
        let (inventory_tx, inventory_rx) = mpsc::channel();
        let mut runtime = AppRuntime::new(|_| {}).unwrap();
        let services = AppServices::start(
            &paths,
            &runtime,
            input(),
            events(),
            |_| {},
            |_| {},
            move |inventory| inventory_tx.send(inventory).unwrap(),
        )
        .unwrap();

        let id = services
            .create_workflow("  A lovely stream  ")
            .unwrap()
            .blocking_recv()
            .unwrap()
            .unwrap();
        let loaded = WorkflowRepository::new(&paths).load(&id).unwrap();
        assert_eq!(loaded.definition().title(), "A lovely stream");
        assert_eq!(loaded.workflow().revision, 1);
        assert!(loaded.workflow().steps.is_empty());
        assert!(matches!(
            loaded.triggers()[0].kind,
            WorkflowTriggerKind::Manual
        ));
        let inventory = inventory_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(inventory.len(), 1);
        assert_eq!(inventory[0].id, id);
        assert_eq!(inventory[0].title, "A lovely stream");
        assert!(!inventory[0].has_steps);
        assert!(inventory[0].error.is_none());

        let error = services
            .create_workflow("   ")
            .unwrap()
            .blocking_recv()
            .unwrap()
            .unwrap_err();
        assert!(error.contains("workflow name"), "{error}");
        assert_eq!(services.list_workflows().len(), 1);

        let repository = WorkflowRepository::new(&paths);
        let loaded = repository.load(&id).unwrap();
        let mut renamed = loaded.definition().clone();
        renamed.name = Some("A second title".into());
        renamed.workflow.revision = 2;
        repository.save_definition(&loaded, &renamed).unwrap();
        services.reload_workflow(&id).unwrap();
        let inventory = inventory_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(inventory[0].id, id);
        assert_eq!(inventory[0].title, "A second title");

        drop(services);
        runtime.shutdown(Duration::from_secs(5)).unwrap();
        let mut restarted = AppRuntime::new(|_| {}).unwrap();
        let services = AppServices::start(
            &paths,
            &restarted,
            input(),
            events(),
            |_| {},
            |_| {},
            |_| {},
        )
        .unwrap();
        assert_eq!(services.list_workflows()[0].title, "A second title");
        drop(services);
        restarted.shutdown(Duration::from_secs(5)).unwrap();
    }

    #[test]
    fn save_workflow_publishes_valid_edits_and_retains_stale_or_invalid_drafts() {
        let directory = tempdir().unwrap();
        let paths = paths(directory.path());
        let repository = WorkflowRepository::new(&paths);
        repository.create(&workflow("alpha")).unwrap();
        let (inventory_tx, inventory_rx) = mpsc::channel();
        let mut runtime = AppRuntime::new(|_| {}).unwrap();
        let services = AppServices::start(
            &paths,
            &runtime,
            input(),
            events(),
            |_| {},
            |_| {},
            move |inventory| inventory_tx.send(inventory).unwrap(),
        )
        .unwrap();

        let opened = repository.load("alpha").unwrap();
        let mut edit = opened.definition().clone();
        edit.name = Some("Chat greeting".into());
        edit.workflow.revision = 2;
        edit.workflow.steps.push(Step {
            id: "greeting".into(),
            on_failure: FailurePolicy::Stop,
            kind: StepKind::Action {
                capability: "twitch.send_chat".into(),
                version: 1,
                inputs: BTreeMap::from([(
                    "message".into(),
                    Input::Literal(serde_json::json!("hello")),
                )]),
                deadline_ms: None,
            },
        });

        let outcome = services
            .save_workflow_typed(&opened, &edit)
            .unwrap()
            .blocking_recv()
            .unwrap()
            .unwrap();
        assert!(outcome.published);
        assert!(outcome.warnings.is_empty());
        let inventory = inventory_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(inventory[0].title, "Chat greeting");
        assert_eq!(inventory[0].step_count, 1);
        assert_eq!(inventory[0].capabilities, vec!["twitch.send_chat"]);
        assert_eq!(inventory[0].revision, Some(2));
        assert!(inventory[0].has_steps);
        assert_eq!(repository.load("alpha").unwrap().workflow().steps.len(), 1);

        let mut stale = edit.clone();
        stale.name = Some("Stale title".into());
        stale.workflow.revision = 2;
        let error = services
            .save_workflow(&opened, &stale)
            .unwrap()
            .blocking_recv()
            .unwrap()
            .unwrap_err();
        assert!(error.contains("changed since it was loaded"), "{error}");

        let error = services
            .save_workflow_typed(&opened, &stale)
            .unwrap()
            .blocking_recv()
            .unwrap()
            .unwrap_err();
        assert!(matches!(
            error,
            WorkflowSaveError::Repository(WorkflowRepositoryError::Store(
                crate::storage::StoreError::Conflict(_)
            ))
        ));

        let current = repository.load("alpha").unwrap();
        let mut invalid = edit;
        invalid.workflow.revision = 3;
        if let StepKind::Action { inputs, .. } = &mut invalid.workflow.steps[0].kind {
            inputs.insert("message".into(), Input::Literal(serde_json::json!(false)));
        }
        let error = services
            .save_workflow(&current, &invalid)
            .unwrap()
            .blocking_recv()
            .unwrap()
            .unwrap_err();
        assert!(error.contains("Message"), "{error}");

        let mut unavailable = repository.load("alpha").unwrap().definition().clone();
        unavailable.workflow.revision = 3;
        if let StepKind::Action { capability, .. } = &mut unavailable.workflow.steps[0].kind {
            *capability = "future.unavailable".into();
        }
        let error = services
            .save_workflow(&current, &unavailable)
            .unwrap()
            .blocking_recv()
            .unwrap()
            .unwrap_err();
        assert!(error.contains("unavailable"), "{error}");
        assert_eq!(repository.load("alpha").unwrap().workflow().revision, 2);
        assert_eq!(services.list_workflows()[0].revision, Some(2));
        assert!(inventory_rx.try_recv().is_err());

        let path = paths.workflows_dir().join("alpha.json");
        let mut changed: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        changed["data"]["name"] = serde_json::json!("External title");
        fs::write(&path, serde_json::to_vec_pretty(&changed).unwrap()).unwrap();
        let mut overwrite = current.definition().clone();
        overwrite.name = Some("Overwrite".into());
        overwrite.workflow.revision = 3;
        let error = services
            .save_workflow(&current, &overwrite)
            .unwrap()
            .blocking_recv()
            .unwrap()
            .unwrap_err();
        assert!(error.contains("changed since it was loaded"), "{error}");
        assert_eq!(
            repository.load("alpha").unwrap().definition().title(),
            "External title"
        );

        drop(services);
        runtime.shutdown(Duration::from_secs(5)).unwrap();
    }

    #[test]
    fn a_saved_workflow_with_invalid_action_input_can_be_repaired() {
        let directory = tempdir().unwrap();
        let paths = paths(directory.path());
        let repository = WorkflowRepository::new(&paths);
        let mut invalid = workflow("repairable");
        invalid.steps.push(Step {
            id: "send".into(),
            on_failure: FailurePolicy::Stop,
            kind: StepKind::Action {
                capability: "twitch.send_chat".into(),
                version: 1,
                inputs: BTreeMap::from([(
                    "message".into(),
                    Input::Literal(serde_json::json!(false)),
                )]),
                deadline_ms: None,
            },
        });
        repository
            .create_definition(&WorkflowDefinition::manual(invalid))
            .unwrap();

        let mut runtime = AppRuntime::new(|_| {}).unwrap();
        let services =
            AppServices::start(&paths, &runtime, input(), events(), |_| {}, |_| {}, |_| {})
                .unwrap();
        let status = &services.list_workflows()[0];
        assert!(
            status
                .error
                .as_ref()
                .is_some_and(|error| error.contains("Message"))
        );
        assert!(matches!(
            services.run_manual("repairable"),
            Err(AppServiceError::Unavailable(_))
        ));

        let opened = repository.load("repairable").unwrap();
        let mut repaired = opened.definition().clone();
        repaired.workflow.revision += 1;
        let StepKind::Action { inputs, .. } = &mut repaired.workflow.steps[0].kind else {
            unreachable!();
        };
        inputs.insert("message".into(), Input::Literal(serde_json::json!("fixed")));
        let outcome = services
            .save_workflow(&opened, &repaired)
            .unwrap()
            .blocking_recv()
            .unwrap()
            .unwrap();
        assert!(outcome.published);
        assert!(services.list_workflows()[0].error.is_none());
        assert_eq!(
            repository.load("repairable").unwrap().workflow().revision,
            2
        );

        drop(services);
        runtime.shutdown(Duration::from_secs(5)).unwrap();
    }

    #[test]
    fn enabled_unimplemented_integration_trigger_is_unavailable() {
        let directory = tempdir().unwrap();
        let paths = paths(directory.path());
        WorkflowRepository::new(&paths)
            .create_definition(&WorkflowDefinition {
                enabled: true,
                workflow: workflow("future-event"),
                triggers: vec![TriggerDefinition {
                    id: "future".into(),
                    enabled: true,
                    kind: WorkflowTriggerKind::IntegrationEvent {
                        integration: "future".into(),
                        event: "something".into(),
                        filters: BTreeMap::new(),
                    },
                }],
                name: None,
            })
            .unwrap();
        let mut runtime = AppRuntime::new(|_| {}).unwrap();
        let services =
            AppServices::start(&paths, &runtime, input(), events(), |_| {}, |_| {}, |_| {})
                .unwrap();
        assert!(services.list_workflows()[0].error.is_some());
        assert!(matches!(
            services.run_manual("future-event"),
            Err(AppServiceError::Unavailable(_))
        ));
        drop(services);
        runtime.shutdown(Duration::from_secs(5)).unwrap();
    }

    #[test]
    fn twitch_trigger_validation_accepts_commands_and_rejects_bad_filters() {
        let mut definition = WorkflowDefinition {
            enabled: true,
            workflow: workflow("chat-command"),
            triggers: vec![TriggerDefinition {
                id: "command".into(),
                enabled: true,
                kind: WorkflowTriggerKind::IntegrationEvent {
                    integration: "twitch".into(),
                    event: "chat.command".into(),
                    filters: BTreeMap::from([("command".into(), serde_json::json!("!go"))]),
                },
            }],
            name: None,
        };
        assert!(runtime_trigger_error(&definition).is_ok());
        if let WorkflowTriggerKind::IntegrationEvent { filters, .. } =
            &mut definition.triggers[0].kind
        {
            filters.insert("command".into(), serde_json::json!(4));
        }
        assert!(
            runtime_trigger_error(&definition)
                .unwrap_err()
                .contains("command")
        );
    }

    #[test]
    fn obs_integration_events_are_validated_before_workflow_registration() {
        let mut definition = WorkflowDefinition {
            enabled: true,
            workflow: workflow("obs-state"),
            triggers: vec![TriggerDefinition {
                id: "mute".into(),
                enabled: true,
                kind: WorkflowTriggerKind::IntegrationEvent {
                    integration: "obs".into(),
                    event: "input.mute_changed".into(),
                    filters: BTreeMap::from([("input".into(), serde_json::json!("Mic"))]),
                },
            }],
            name: None,
        };
        assert!(runtime_trigger_error(&definition).is_ok());
        if let WorkflowTriggerKind::IntegrationEvent { filters, .. } =
            &mut definition.triggers[0].kind
        {
            filters.insert("item_id".into(), serde_json::json!(9));
        }
        assert!(
            runtime_trigger_error(&definition)
                .unwrap_err()
                .contains("item_id")
        );
    }

    #[test]
    fn vtube_integration_events_are_validated_before_workflow_registration() {
        let mut definition = WorkflowDefinition {
            enabled: true,
            workflow: workflow("avatar-state"),
            triggers: vec![TriggerDefinition {
                id: "hotkey".into(),
                enabled: true,
                kind: WorkflowTriggerKind::IntegrationEvent {
                    integration: "vtube_studio".into(),
                    event: "hotkey.triggered".into(),
                    filters: BTreeMap::from([(
                        "hotkey_id".into(),
                        serde_json::json!("0123456789abcdef0123456789abcdef"),
                    )]),
                },
            }],
            name: None,
        };
        assert!(runtime_trigger_error(&definition).is_ok());
        if let WorkflowTriggerKind::IntegrationEvent { filters, .. } =
            &mut definition.triggers[0].kind
        {
            filters.insert("hotkey_id".into(), serde_json::json!("stale"));
        }
        assert!(
            runtime_trigger_error(&definition)
                .unwrap_err()
                .contains("hotkey_id")
        );
    }

    #[test]
    fn saved_workflow_with_missing_action_is_preserved_and_unavailable() {
        let directory = tempdir().unwrap();
        let paths = paths(directory.path());
        let mut saved = workflow("missing-action");
        saved.steps.push(Step {
            id: "future".into(),
            on_failure: FailurePolicy::Stop,
            kind: StepKind::Action {
                capability: "future.action".into(),
                version: 1,
                inputs: BTreeMap::new(),
                deadline_ms: None,
            },
        });
        let repository = WorkflowRepository::new(&paths);
        repository.create(&saved).unwrap();
        let mut runtime = AppRuntime::new(|_| {}).unwrap();
        let services =
            AppServices::start(&paths, &runtime, input(), events(), |_| {}, |_| {}, |_| {})
                .unwrap();
        let status = &services.list_workflows()[0];
        assert_eq!(status.id, "missing-action");
        assert_eq!(status.step_count, 1);
        assert_eq!(status.capabilities, vec!["future.action"]);
        assert!(status.error.as_deref().unwrap().contains("future.action"));
        assert!(matches!(
            services.run_manual("missing-action"),
            Err(AppServiceError::Unavailable(_))
        ));
        assert_eq!(
            repository
                .load("missing-action")
                .unwrap()
                .workflow()
                .steps
                .len(),
            1
        );
        drop(services);
        runtime.shutdown(Duration::from_secs(5)).unwrap();
    }

    #[test]
    fn saves_obs_connection_settings_off_the_ui_thread() {
        let directory = tempdir().unwrap();
        let paths = paths(directory.path());
        let mut runtime = AppRuntime::new(|_| {}).unwrap();
        let services =
            AppServices::start(&paths, &runtime, input(), events(), |_| {}, |_| {}, |_| {})
                .unwrap();
        let value = ObsSettings {
            enabled: false,
            host: "192.168.178.44".into(),
            port: 4456,
            tls: false,
        };
        let reply = services.save_obs_settings(value.clone()).unwrap();
        assert_eq!(reply.blocking_recv().unwrap(), Ok(()));
        assert_eq!(ObsSettingsStore::new(&paths).load().unwrap().value, value);

        let rejected = ObsSettings {
            host: "8.8.8.8".into(),
            ..value.clone()
        };
        let reply = services.save_obs_settings(rejected).unwrap();
        let error = reply.blocking_recv().unwrap().unwrap_err();
        assert!(error.contains("private network address"), "{error}");
        assert_eq!(ObsSettingsStore::new(&paths).load().unwrap().value, value);
        let store = ObsSettingsStore::new(&paths);
        let original = store.load().unwrap();
        let changed = ObsSettings {
            port: 4460,
            ..value.clone()
        };
        store.save(&original, changed.clone()).unwrap();
        let error = services
            .save_obs_settings_typed(original, value)
            .unwrap()
            .blocking_recv()
            .unwrap()
            .unwrap_err();
        assert!(matches!(
            error,
            ObsConfigurationError::Settings(ObsSettingsError::Store(
                crate::storage::StoreError::Conflict(_)
            ))
        ));
        assert_eq!(store.load().unwrap().value, changed);
        services
            .save_obs_settings_typed(store.load().unwrap(), changed.clone())
            .unwrap()
            .blocking_recv()
            .unwrap()
            .unwrap();
        assert_eq!(store.load().unwrap().value, changed);
        drop(services);
        runtime.shutdown(Duration::from_secs(5)).unwrap();
    }
}
