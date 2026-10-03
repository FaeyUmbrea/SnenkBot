//! Durable storage for editable workflow definitions.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::engine::{self, Workflow};
use crate::migration::StoredConfig;
use crate::paths::AppPaths;
use crate::storage::{ConfigSnapshot, ConfigStore, SaveOutcome, StoreError};

const DEFINITION: &str = "snenkbot.workflow";
const VERSION: u32 = 4;

/// The editable definition stored on disk. Execution remains independent of triggers.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
#[serde(deny_unknown_fields)]
pub struct WorkflowDefinition {
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    pub workflow: Workflow,
    pub triggers: Vec<TriggerDefinition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

fn enabled_by_default() -> bool {
    true
}

impl WorkflowDefinition {
    pub fn manual(workflow: Workflow) -> Self {
        Self {
            enabled: true,
            workflow,
            triggers: vec![TriggerDefinition {
                id: "manual".to_owned(),
                enabled: true,
                kind: TriggerKind::Manual,
            }],
            name: None,
        }
    }

    pub fn title(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.workflow.id)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
#[serde(deny_unknown_fields)]
pub struct TriggerDefinition {
    pub id: String,
    pub enabled: bool,
    pub kind: TriggerKind,
}

/// Event names are stable storage values, independent of connector runtime types.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum TriggerKind {
    #[serde(rename = "manual")]
    Manual,
    #[serde(rename = "obs.recording_started")]
    ObsRecordingStarted,
    #[serde(rename = "obs.current_scene")]
    ObsCurrentScene { scene: String },
    #[serde(rename = "integration_event")]
    IntegrationEvent {
        integration: String,
        event: String,
        #[serde(default)]
        #[cfg_attr(
            feature = "desktop-contracts",
            specta(type = BTreeMap<String, specta_typescript::Unknown>)
        )]
        filters: BTreeMap<String, Value>,
    },
}

#[derive(Debug, Error)]
pub enum WorkflowRepositoryError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("could not read workflow directory `{path}`: {source}")]
    Directory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("workflow directory entry could not be read: {0}")]
    Entry(#[source] std::io::Error),
}

#[derive(Debug)]
pub enum WorkflowEntry {
    Available { id: String, workflow: Workflow },
    Unavailable { id: String, error: String },
}

#[derive(Debug)]
pub enum WorkflowDefinitionEntry {
    Available {
        id: String,
        definition: WorkflowDefinition,
    },
    Unavailable {
        id: String,
        error: String,
    },
}

impl WorkflowEntry {
    pub fn id(&self) -> &str {
        match self {
            Self::Available { id, .. } | Self::Unavailable { id, .. } => id,
        }
    }
}

/// A validated workflow together with the exact on-disk snapshot it was loaded from.
#[derive(Clone, Debug)]
pub struct EditableWorkflow {
    definition: WorkflowDefinition,
    snapshot: ConfigSnapshot,
}

impl EditableWorkflow {
    pub fn workflow(&self) -> &Workflow {
        &self.definition.workflow
    }

    pub fn definition(&self) -> &WorkflowDefinition {
        &self.definition
    }

    pub fn triggers(&self) -> &[TriggerDefinition] {
        &self.definition.triggers
    }
}

#[derive(Debug, Clone)]
pub struct WorkflowRepository {
    root: PathBuf,
    store: ConfigStore,
}

impl WorkflowRepository {
    pub fn new(paths: &AppPaths) -> Self {
        Self::at(paths.workflows_dir())
    }

    pub fn at(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            store: ConfigStore::new(&root),
            root,
        }
    }

    pub fn list(&self) -> Result<Vec<WorkflowEntry>, WorkflowRepositoryError> {
        Ok(self
            .list_definitions()?
            .into_iter()
            .map(|entry| match entry {
                WorkflowDefinitionEntry::Available { id, definition } => WorkflowEntry::Available {
                    id,
                    workflow: definition.workflow,
                },
                WorkflowDefinitionEntry::Unavailable { id, error } => {
                    WorkflowEntry::Unavailable { id, error }
                }
            })
            .collect())
    }

    pub fn list_definitions(
        &self,
    ) -> Result<Vec<WorkflowDefinitionEntry>, WorkflowRepositoryError> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(WorkflowRepositoryError::Directory {
                    path: self.root.clone(),
                    source,
                });
            }
        };
        let mut files = Vec::new();
        for entry in entries {
            let entry = entry.map_err(WorkflowRepositoryError::Entry)?;
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "json")
                && path.is_file()
                && let Some(stem) = path.file_stem().and_then(|stem| stem.to_str())
            {
                files.push((stem.to_owned(), path));
            }
        }
        files.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(files
            .into_iter()
            .map(|(id, _)| self.definition_entry(&id))
            .collect())
    }

    pub fn load(&self, id: &str) -> Result<EditableWorkflow, String> {
        let snapshot = self.store.load(id).map_err(|error| error.to_string())?;
        let definition = decode(snapshot.config()).map_err(|error| error.to_string())?;
        if definition.workflow.id != id {
            return Err(format!(
                "workflow ID `{}` does not match file ID `{id}`",
                definition.workflow.id
            ));
        }
        Ok(EditableWorkflow {
            definition,
            snapshot,
        })
    }

    pub fn create(&self, workflow: &Workflow) -> Result<SaveOutcome, WorkflowRepositoryError> {
        self.create_definition(&WorkflowDefinition::manual(workflow.clone()))
    }

    pub fn create_definition(
        &self,
        definition: &WorkflowDefinition,
    ) -> Result<SaveOutcome, WorkflowRepositoryError> {
        if definition.workflow.revision == 0 {
            return Err(validation_error(
                &definition.workflow.id,
                "workflow revision must be positive",
            ));
        }
        Ok(self.store.create(
            &definition.workflow.id,
            &config(definition),
            validate_workflow_config,
        )?)
    }

    pub fn save(
        &self,
        loaded: &EditableWorkflow,
        workflow: &Workflow,
    ) -> Result<SaveOutcome, WorkflowRepositoryError> {
        let mut definition = loaded.definition.clone();
        definition.workflow = workflow.clone();
        self.save_definition(loaded, &definition)
    }

    pub fn save_definition(
        &self,
        loaded: &EditableWorkflow,
        definition: &WorkflowDefinition,
    ) -> Result<SaveOutcome, WorkflowRepositoryError> {
        let workflow = &definition.workflow;
        if loaded.workflow().id != workflow.id {
            return Err(validation_error(
                &workflow.id,
                "workflow ID cannot change during an edit",
            ));
        }
        let new_config = config(definition);
        if definition_data(&loaded.definition) != definition_data(definition)
            || loaded.workflow().revision != workflow.revision
        {
            let expected = loaded
                .workflow()
                .revision
                .checked_add(1)
                .ok_or_else(|| validation_error(&workflow.id, "workflow revision overflow"))?;
            if workflow.revision != expected {
                return Err(validation_error(
                    &workflow.id,
                    format!("changed workflow must advance revision to {expected}"),
                ));
            }
        }
        Ok(self.store.save(
            &loaded.workflow().id,
            &loaded.snapshot,
            &new_config,
            validate_workflow_config,
        )?)
    }

    fn definition_entry(&self, id: &str) -> WorkflowDefinitionEntry {
        match self.load(id) {
            Ok(loaded) => WorkflowDefinitionEntry::Available {
                id: id.to_owned(),
                definition: loaded.definition,
            },
            Err(error) => WorkflowDefinitionEntry::Unavailable {
                id: id.to_owned(),
                error,
            },
        }
    }
}

fn validation_error(id: &str, message: impl Into<String>) -> WorkflowRepositoryError {
    StoreError::Validation {
        id: id.to_owned(),
        message: message.into(),
    }
    .into()
}

fn config(definition: &WorkflowDefinition) -> StoredConfig {
    StoredConfig {
        definition: DEFINITION.to_owned(),
        version: VERSION,
        data: serde_json::to_value(definition).expect("Workflow definition serializes to JSON"),
    }
}

fn definition_data(definition: &WorkflowDefinition) -> Value {
    let mut data =
        serde_json::to_value(definition).expect("Workflow definition serializes to JSON");
    if let Some(workflow) = data.get_mut("workflow").and_then(Value::as_object_mut) {
        workflow.remove("revision");
    }
    data
}

fn decode(stored: &StoredConfig) -> Result<WorkflowDefinition, String> {
    if stored.definition != DEFINITION {
        return Err(format!(
            "unknown workflow definition `{}`",
            stored.definition
        ));
    }
    if !matches!(stored.version, 1 | 2 | 3 | VERSION) {
        return Err(format!(
            "unsupported workflow version {}; this build supports versions 1 through {VERSION}",
            stored.version
        ));
    }
    let definition = if stored.version == 1 {
        let workflow = serde_json::from_value(stored.data.clone())
            .map_err(|error| format!("invalid workflow data: {error}"))?;
        WorkflowDefinition::manual(workflow)
    } else {
        serde_json::from_value(stored.data.clone())
            .map_err(|error| format!("invalid workflow data: {error}"))?
    };
    validate_definition(&definition)?;
    Ok(definition)
}

fn validate_definition(definition: &WorkflowDefinition) -> Result<(), String> {
    let workflow = &definition.workflow;
    engine::validate_workflow(workflow)?;
    if let Some(name) = &definition.name {
        validate_display_name(name).map_err(str::to_owned)?;
    }
    if workflow.revision == 0 {
        return Err("workflow revision must be positive".to_owned());
    }
    let mut ids = HashSet::new();
    for trigger in &definition.triggers {
        if trigger.id.is_empty()
            || !trigger
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            || !ids.insert(&trigger.id)
        {
            return Err(format!("duplicate or invalid trigger ID `{}`", trigger.id));
        }
        match &trigger.kind {
            TriggerKind::Manual | TriggerKind::ObsRecordingStarted => {}
            TriggerKind::ObsCurrentScene { scene } if !scene.trim().is_empty() => {}
            TriggerKind::IntegrationEvent {
                integration,
                event,
                filters,
            } if !integration.trim().is_empty()
                && !event.trim().is_empty()
                && filters.keys().all(|key| !key.trim().is_empty()) => {}
            _ => {
                return Err(format!(
                    "invalid configuration for trigger `{}`",
                    trigger.id
                ));
            }
        }
    }
    Ok(())
}

pub fn validate_display_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty()
        || name != name.trim()
        || name.chars().count() > 80
        || name.chars().any(char::is_control)
    {
        return Err(
            "workflow name must be 1–80 characters without surrounding spaces or control characters",
        );
    }
    Ok(())
}

fn validate_workflow_config(stored: &StoredConfig) -> Result<(), String> {
    decode(stored).map(|_| ())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use tempfile::tempdir;

    use super::{
        TriggerDefinition, TriggerKind, WorkflowDefinition, WorkflowDefinitionEntry, WorkflowEntry,
        WorkflowRepository,
    };
    use crate::engine::{Input, Workflow};

    fn workflow(id: &str, revision: u64) -> Workflow {
        Workflow {
            id: id.to_owned(),
            revision,
            overlap: false,
            steps: Vec::new(),
            outputs: BTreeMap::from([("result".to_owned(), Input::Literal(serde_json::json!(1)))]),
        }
    }

    #[test]
    fn old_definitions_default_to_enabled_without_being_rewritten() {
        let directory = tempdir().unwrap();
        let repository = WorkflowRepository::at(directory.path());
        let mut data =
            serde_json::to_value(WorkflowDefinition::manual(workflow("legacy", 1))).unwrap();
        data.as_object_mut().unwrap().remove("enabled");
        let bytes = serde_json::to_vec(
            &serde_json::json!({"definition":"snenkbot.workflow", "version":3, "data":data}),
        )
        .unwrap();
        fs::write(directory.path().join("legacy.json"), &bytes).unwrap();
        let loaded = repository.load("legacy").unwrap();
        assert!(loaded.definition().enabled);
        assert_eq!(
            fs::read(directory.path().join("legacy.json")).unwrap(),
            bytes
        );
        let mut edited = loaded.definition().clone();
        edited.enabled = false;
        edited.workflow.revision += 1;
        repository.save_definition(&loaded, &edited).unwrap();
        assert!(!repository.load("legacy").unwrap().definition().enabled);
    }

    #[test]
    fn round_trips_and_lists_workflow() {
        let directory = tempdir().unwrap();
        let repository = WorkflowRepository::at(directory.path());
        repository.create(&workflow("alpha", 1)).unwrap();

        let loaded = repository.load("alpha").unwrap();
        assert_eq!(loaded.workflow().id, "alpha");
        assert_eq!(loaded.definition().title(), "alpha");
        assert_eq!(loaded.workflow().revision, 1);
        assert!(matches!(
            loaded.triggers(),
            [TriggerDefinition {
                enabled: true,
                kind: TriggerKind::Manual,
                ..
            }]
        ));
        let stored: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.path().join("alpha.json")).unwrap())
                .unwrap();
        assert_eq!(stored["version"], super::VERSION);
        assert!(
            matches!(repository.list().unwrap().as_slice(), [WorkflowEntry::Available { id, .. }] if id == "alpha")
        );
    }

    #[test]
    fn display_name_is_portable_and_validated_independently_of_the_stable_id() {
        let directory = tempdir().unwrap();
        let repository = WorkflowRepository::at(directory.path());
        let mut definition = WorkflowDefinition::manual(workflow("stable-id", 1));
        definition.name = Some("A lovely stream".into());
        repository.create_definition(&definition).unwrap();
        let loaded = repository.load("stable-id").unwrap();
        assert_eq!(loaded.definition().title(), "A lovely stream");
        assert_eq!(loaded.workflow().id, "stable-id");

        let mut invalid = WorkflowDefinition::manual(workflow("invalid-name", 1));
        invalid.name = Some(" Bad name ".into());
        assert!(repository.create_definition(&invalid).is_err());
        assert!(!directory.path().join("invalid-name.json").exists());
    }

    #[test]
    fn invalid_and_future_files_remain_unavailable_and_unchanged() {
        let directory = tempdir().unwrap();
        let repository = WorkflowRepository::at(directory.path());
        let invalid = br#"{"format":1,"definition":"snenkbot.workflow","version":1,"data":{"id":"bad","revision":0,"overlap":false,"steps":[{"id":"same","on_failure":"Stop","kind":{"SetVariable":{"name":"x","value":{"Literal":1}}}},{"id":"same","on_failure":"Stop","kind":"Stop"}],"outputs":{}}}"#;
        let future =
            br#"{"format":1,"definition":"snenkbot.workflow","version":99,"data":{"future":true}}"#;
        fs::write(directory.path().join("invalid.json"), invalid).unwrap();
        fs::write(directory.path().join("future.json"), future).unwrap();

        let entries = repository.list().unwrap();
        assert!(
            entries
                .iter()
                .all(|entry| matches!(entry, WorkflowEntry::Unavailable { .. }))
        );
        assert_eq!(
            fs::read(directory.path().join("invalid.json")).unwrap(),
            invalid
        );
        assert_eq!(
            fs::read(directory.path().join("future.json")).unwrap(),
            future
        );
        assert!(repository.create(&workflow("future", 1)).is_err());
    }

    #[test]
    fn stale_snapshot_conflicts_after_another_save() {
        let directory = tempdir().unwrap();
        let repository = WorkflowRepository::at(directory.path());
        repository.create(&workflow("alpha", 1)).unwrap();
        let first = repository.load("alpha").unwrap();
        let stale = repository.load("alpha").unwrap();
        repository.save(&first, &workflow("alpha", 2)).unwrap();

        assert!(
            matches!(repository.save(&stale, &workflow("alpha", 2)), Err(super::WorkflowRepositoryError::Store(crate::storage::StoreError::Conflict(id))) if id == "alpha")
        );
        assert_eq!(repository.load("alpha").unwrap().workflow().revision, 2);
    }

    #[test]
    fn creates_require_positive_revision_and_changes_advance_once() {
        let directory = tempdir().unwrap();
        let repository = WorkflowRepository::at(directory.path());
        assert!(repository.create(&workflow("zero", 0)).is_err());
        repository.create(&workflow("alpha", 1)).unwrap();
        let loaded = repository.load("alpha").unwrap();
        let mut unchanged_revision = workflow("alpha", 1);
        unchanged_revision.overlap = true;
        let mut decreased_revision = workflow("alpha", 0);
        decreased_revision.overlap = true;

        assert!(repository.save(&loaded, &unchanged_revision).is_err());
        assert!(repository.save(&loaded, &decreased_revision).is_err());
        assert!(repository.save(&loaded, &workflow("alpha", 3)).is_err());
        repository.save(&loaded, &workflow("alpha", 2)).unwrap();
        assert_eq!(repository.load("alpha").unwrap().workflow().revision, 2);
    }

    #[test]
    fn reads_v1_as_manual_without_rewriting_and_upgrades_on_save() {
        let directory = tempdir().unwrap();
        let repository = WorkflowRepository::at(directory.path());
        let original = serde_json::to_vec(&serde_json::json!({
            "format": 1,
            "definition": "snenkbot.workflow",
            "version": 1,
            "data": workflow("legacy", 1),
        }))
        .unwrap();
        let path = directory.path().join("legacy.json");
        fs::write(&path, &original).unwrap();

        let loaded = repository.load("legacy").unwrap();
        assert!(matches!(
            loaded.triggers(),
            [TriggerDefinition {
                enabled: true,
                kind: TriggerKind::Manual,
                ..
            }]
        ));
        assert_eq!(fs::read(&path).unwrap(), original);
        assert!(matches!(
            repository.list_definitions().unwrap().as_slice(),
            [WorkflowDefinitionEntry::Available { .. }]
        ));
        repository.save(&loaded, loaded.workflow()).unwrap();

        let stored: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(stored["version"], super::VERSION);
        assert_eq!(repository.load("legacy").unwrap().workflow().revision, 1);
    }

    #[test]
    fn reads_v2_without_a_display_name_and_upgrades_only_when_saved() {
        let directory = tempdir().unwrap();
        let repository = WorkflowRepository::at(directory.path());
        let raw = serde_json::to_vec(&serde_json::json!({
            "format": 1,
            "definition": "snenkbot.workflow",
            "version": 2,
            "data": WorkflowDefinition::manual(workflow("existing", 1)),
        }))
        .unwrap();
        let path = directory.path().join("existing.json");
        fs::write(&path, &raw).unwrap();

        let loaded = repository.load("existing").unwrap();
        assert_eq!(loaded.definition().title(), "existing");
        assert_eq!(fs::read(&path).unwrap(), raw);
        repository.save(&loaded, loaded.workflow()).unwrap();
        let stored: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(stored["version"], super::VERSION);
    }

    #[test]
    fn unknown_trigger_kind_keeps_file_unavailable_and_unchanged() {
        let directory = tempdir().unwrap();
        let repository = WorkflowRepository::at(directory.path());
        let raw = serde_json::to_vec(&serde_json::json!({
            "format": 1,
            "definition": "snenkbot.workflow",
            "version": 2,
            "data": {
                "workflow": workflow("unknown", 1),
                "triggers": [{ "id": "future", "enabled": true, "kind": { "kind": "unknown.future" } }],
            },
        }))
        .unwrap();
        let path = directory.path().join("unknown.json");
        fs::write(&path, &raw).unwrap();

        assert!(matches!(
            repository.list().unwrap().as_slice(),
            [WorkflowEntry::Unavailable { .. }]
        ));
        assert!(
            repository
                .load("unknown")
                .unwrap_err()
                .contains("unknown.future")
        );
        assert_eq!(fs::read(path).unwrap(), raw);
    }

    #[test]
    fn trigger_edits_advance_revision_and_workflow_only_saves_preserve_triggers() {
        let directory = tempdir().unwrap();
        let repository = WorkflowRepository::at(directory.path());
        let mut definition = WorkflowDefinition::manual(workflow("alpha", 1));
        definition.triggers.push(TriggerDefinition {
            id: "recording".to_owned(),
            enabled: false,
            kind: TriggerKind::ObsRecordingStarted,
        });
        repository.create_definition(&definition).unwrap();

        let loaded = repository.load("alpha").unwrap();
        definition.triggers[1].enabled = true;
        assert!(repository.save_definition(&loaded, &definition).is_err());
        definition.workflow.revision = 2;
        repository.save_definition(&loaded, &definition).unwrap();

        let loaded = repository.load("alpha").unwrap();
        let mut workflow = loaded.workflow().clone();
        workflow.revision = 3;
        workflow.overlap = true;
        repository.save(&loaded, &workflow).unwrap();
        let updated = repository.load("alpha").unwrap();
        assert!(matches!(
            updated.triggers()[1],
            TriggerDefinition {
                enabled: true,
                kind: TriggerKind::ObsRecordingStarted,
                ..
            }
        ));
        assert!(directory.path().join("history").read_dir().unwrap().count() >= 2);
    }

    #[test]
    fn rejects_invalid_trigger_ids_and_parameters() {
        let directory = tempdir().unwrap();
        let repository = WorkflowRepository::at(directory.path());
        let mut definition = WorkflowDefinition::manual(workflow("alpha", 1));
        definition.triggers.push(TriggerDefinition {
            id: "manual".to_owned(),
            enabled: true,
            kind: TriggerKind::ObsCurrentScene {
                scene: "Scene".to_owned(),
            },
        });
        assert!(repository.create_definition(&definition).is_err());
        definition.triggers[1].id = "bad/id".to_owned();
        assert!(repository.create_definition(&definition).is_err());
        definition.triggers[1].id = "scene".to_owned();
        definition.triggers[1].kind = TriggerKind::ObsCurrentScene {
            scene: " ".to_owned(),
        };
        assert!(repository.create_definition(&definition).is_err());
        assert!(!directory.path().join("alpha.json").exists());
    }
}
