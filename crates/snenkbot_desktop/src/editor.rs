use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use specta::Type;
use uuid::Uuid;

use snenk_bot::editor::{
    EditError, StepDestination, StepPosition, TriggerPosition, WorkflowEditSession,
};
use snenk_bot::engine::{Input, StepKind};
use snenk_bot::schema::{ConfigOutput, ConfigSchema};
use snenk_bot::value_sources::{ValueSource, available_sources};
use snenk_bot::workflows::{EditableWorkflow, TriggerKind, WorkflowDefinition, WorkflowRepository};

#[derive(Clone, Debug, Serialize, Type)]
pub struct EditorSnapshot {
    pub session_id: String,
    #[specta(type = specta_typescript::Number)]
    pub revision: u64,
    pub draft: WorkflowDefinition,
    pub dirty: bool,
    pub can_undo: bool,
    pub can_redo: bool,
    #[specta(type = specta_typescript::Number)]
    pub saved_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Type)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum EditorOperation {
    Rename {
        name: String,
    },
    Enabled {
        enabled: bool,
    },
    SetInput {
        step_id: String,
        field_id: String,
        value: Input,
    },
    InsertAt {
        kind: StepKind,
        destination: StepDestination,
        position: StepPosition,
    },
    SetKind {
        step_id: String,
        kind: StepKind,
    },
    Remove {
        step_id: String,
    },
    MoveTo {
        step_id: String,
        destination: StepDestination,
        position: StepPosition,
    },
    TriggerEnabled {
        trigger_id: String,
        enabled: bool,
    },
    TriggerKind {
        trigger_id: String,
        kind: TriggerKind,
    },
    AppendTrigger {
        kind: TriggerKind,
    },
    InsertTriggerAt {
        kind: TriggerKind,
        position: TriggerPosition,
    },
    RemoveTrigger {
        trigger_id: String,
    },
    MoveTriggerTo {
        trigger_id: String,
        position: TriggerPosition,
    },
    Undo,
    Redo,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum EditorErrorCode {
    Unavailable,
    Busy,
    MissingSession,
    StaleRevision,
    RevisionExhausted,
    InvalidEdit,
    WorkflowUnavailable,
    SaveConflict,
    SaveFailed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Type)]
pub struct EditorError {
    pub code: EditorErrorCode,
    pub message: String,
}

impl EditorError {
    pub(crate) fn new(code: EditorErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl From<EditError> for EditorError {
    fn from(error: EditError) -> Self {
        let message = match error {
            EditError::MissingTrigger(_) => {
                "This trigger is no longer available. Refresh the editor and try again.".into()
            }
            error => error.to_string(),
        };
        Self::new(EditorErrorCode::InvalidEdit, message)
    }
}

struct Session {
    revision: u64,
    editor: WorkflowEditSession,
}

impl Session {
    fn snapshot(&self, session_id: &str) -> EditorSnapshot {
        EditorSnapshot {
            session_id: session_id.to_owned(),
            revision: self.revision,
            draft: self.editor.draft().clone(),
            dirty: self.editor.is_dirty(),
            can_undo: self.editor.can_undo(),
            can_redo: self.editor.can_redo(),
            saved_revision: self.editor.opened().workflow().revision,
        }
    }

    fn next_revision(&self, expected_revision: u64) -> Result<u64, EditorError> {
        if self.revision != expected_revision {
            return Err(EditorError::new(
                EditorErrorCode::StaleRevision,
                "The editor changed before this request arrived. Refresh the editor and try again.",
            ));
        }
        self.revision.checked_add(1).ok_or_else(|| {
            EditorError::new(
                EditorErrorCode::RevisionExhausted,
                "Reopen this editor to continue editing.",
            )
        })
    }
}

/// The desktop command layer holds this store behind a mutex across edits and saves.
pub struct EditorSessions {
    repository: WorkflowRepository,
    sessions: HashMap<String, Session>,
}

impl EditorSessions {
    pub fn new(repository: WorkflowRepository) -> Self {
        Self {
            repository,
            sessions: HashMap::new(),
        }
    }

    pub fn open(&mut self, workflow_id: &str) -> Result<EditorSnapshot, EditorError> {
        let opened = self.repository.load(workflow_id).map_err(|_| {
            EditorError::new(
                EditorErrorCode::WorkflowUnavailable,
                "This workflow could not be opened.",
            )
        })?;
        let session_id = Uuid::new_v4().to_string();
        let session = Session {
            revision: 0,
            editor: WorkflowEditSession::new(opened),
        };
        let snapshot = session.snapshot(&session_id);
        self.sessions.insert(session_id, session);
        Ok(snapshot)
    }

    pub fn snapshot(&self, session_id: &str) -> Result<EditorSnapshot, EditorError> {
        Ok(self.session(session_id)?.snapshot(session_id))
    }

    /// Read values from this draft and position without adding an undo entry.
    pub fn value_sources(
        &self,
        session_id: &str,
        expected_revision: u64,
        step_id: &str,
        schemas: &[&ConfigSchema],
        trigger_outputs: impl Fn(&TriggerKind) -> Option<Vec<ConfigOutput>>,
    ) -> Result<Vec<ValueSource>, EditorError> {
        let session = self.session(session_id)?;
        if session.revision != expected_revision {
            return Err(EditorError::new(
                EditorErrorCode::StaleRevision,
                "The editor changed. Open the value picker again.",
            ));
        }
        available_sources(
            session.editor.draft(),
            Some(step_id),
            schemas,
            trigger_outputs,
        )
        .map_err(|_| EditorError::new(EditorErrorCode::InvalidEdit, "This step is unavailable."))
    }

    pub fn close(&mut self, session_id: &str) -> Result<(), EditorError> {
        self.sessions
            .remove(session_id)
            .ok_or_else(missing_session)?;
        Ok(())
    }

    pub fn apply(
        &mut self,
        session_id: &str,
        expected_revision: u64,
        operation: EditorOperation,
    ) -> Result<EditorSnapshot, EditorError> {
        let session = self.session(session_id)?;
        let revision = session.next_revision(expected_revision)?;
        // Core edits may update more than one field; publish the draft only after validation succeeds.
        let mut editor = session.editor.clone();
        match operation {
            EditorOperation::Rename { name } => editor.rename(name)?,
            EditorOperation::Enabled { enabled } => editor.set_enabled(enabled)?,
            EditorOperation::SetInput {
                step_id,
                field_id,
                value,
            } => editor.set_action_input(&step_id, &field_id, value)?,
            EditorOperation::InsertAt {
                kind,
                destination,
                position,
            } => {
                editor.insert_step_at(kind, destination, position)?;
            }
            EditorOperation::SetKind { step_id, kind } => editor.set_step_kind(&step_id, kind)?,
            EditorOperation::Remove { step_id } => editor.remove_step(&step_id)?,
            EditorOperation::MoveTo {
                step_id,
                destination,
                position,
            } => editor.move_step_to(&step_id, destination, position)?,
            EditorOperation::TriggerEnabled {
                trigger_id,
                enabled,
            } => editor.set_trigger_enabled(&trigger_id, enabled)?,
            EditorOperation::TriggerKind { trigger_id, kind } => {
                editor.set_trigger_kind(&trigger_id, kind)?
            }
            EditorOperation::AppendTrigger { kind } => {
                editor.append_trigger(kind)?;
            }
            EditorOperation::InsertTriggerAt { kind, position } => {
                editor.insert_trigger_at(kind, position)?;
            }
            EditorOperation::RemoveTrigger { trigger_id } => editor.remove_trigger(&trigger_id)?,
            EditorOperation::MoveTriggerTo {
                trigger_id,
                position,
            } => editor.move_trigger_to(&trigger_id, position)?,
            EditorOperation::Undo => {
                editor.undo();
            }
            EditorOperation::Redo => {
                editor.redo();
            }
        }
        let session = Session { revision, editor };
        let snapshot = session.snapshot(session_id);
        self.sessions.insert(session_id.to_owned(), session);
        Ok(snapshot)
    }

    /// The caller validates and publishes through application services, then reloads the saved value.
    /// Persistence failures and mismatched saved definitions leave the session untouched.
    pub fn save_with(
        &mut self,
        session_id: &str,
        expected_revision: u64,
        persist: impl FnOnce(
            &EditableWorkflow,
            &WorkflowDefinition,
        ) -> Result<EditableWorkflow, EditorError>,
    ) -> Result<EditorSnapshot, EditorError> {
        let session = self.session(session_id)?;
        let revision = session.next_revision(expected_revision)?;
        let mut editor = session.editor.clone();
        let saved = persist(editor.opened(), editor.draft())?;
        editor.accept_saved(saved).map_err(|_| {
            EditorError::new(
                EditorErrorCode::SaveConflict,
                "The saved workflow changed. Your draft is still open.",
            )
        })?;
        let session = Session { revision, editor };
        let snapshot = session.snapshot(session_id);
        self.sessions.insert(session_id.to_owned(), session);
        Ok(snapshot)
    }

    fn session(&self, session_id: &str) -> Result<&Session, EditorError> {
        self.sessions.get(session_id).ok_or_else(missing_session)
    }
}

fn missing_session() -> EditorError {
    EditorError::new(
        EditorErrorCode::MissingSession,
        "This editor session is closed. Open the workflow again.",
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use snenk_bot::engine::{FailurePolicy, Step, Workflow};
    use snenk_bot::storage::StoreError;
    use snenk_bot::workflows::WorkflowRepositoryError;

    use super::*;

    #[test]
    fn trigger_drops_removal_and_stale_requests_are_atomic_and_persistable() {
        let (_directory, mut sessions) = sessions();
        let opened = sessions.open("test-workflow").unwrap();
        let manual = opened.draft.triggers[0].id.clone();
        let inserted = sessions
            .apply(
                &opened.session_id,
                opened.revision,
                EditorOperation::InsertTriggerAt {
                    kind: TriggerKind::ObsRecordingStarted,
                    position: TriggerPosition::Before {
                        trigger_id: manual.clone(),
                    },
                },
            )
            .unwrap();
        let recording = inserted.draft.triggers[0].id.clone();
        let stale = sessions
            .apply(
                &opened.session_id,
                opened.revision,
                EditorOperation::RemoveTrigger {
                    trigger_id: manual.clone(),
                },
            )
            .unwrap_err();
        assert_eq!(stale.code, EditorErrorCode::StaleRevision);
        let moved = sessions
            .apply(
                &opened.session_id,
                inserted.revision,
                EditorOperation::MoveTriggerTo {
                    trigger_id: recording.clone(),
                    position: TriggerPosition::Append,
                },
            )
            .unwrap();
        assert_eq!(moved.draft.triggers[1].id, recording);
        let invalid = sessions
            .apply(
                &opened.session_id,
                moved.revision,
                EditorOperation::MoveTriggerTo {
                    trigger_id: recording.clone(),
                    position: TriggerPosition::Before {
                        trigger_id: "private-identity".into(),
                    },
                },
            )
            .unwrap_err();
        assert!(!invalid.message.contains("private-identity"));
        let after_failure = sessions.snapshot(&opened.session_id).unwrap();
        assert_eq!(after_failure.revision, moved.revision);
        assert_eq!(
            serde_json::to_value(&after_failure.draft).unwrap(),
            serde_json::to_value(&moved.draft).unwrap()
        );
        let removed = sessions
            .apply(
                &opened.session_id,
                moved.revision,
                EditorOperation::RemoveTrigger {
                    trigger_id: manual.clone(),
                },
            )
            .unwrap();
        assert_eq!(removed.draft.triggers.len(), 1);
        let undone = sessions
            .apply(&opened.session_id, removed.revision, EditorOperation::Undo)
            .unwrap();
        assert_eq!(undone.draft.triggers[0].id, manual);
        let saved = save(&mut sessions, &undone).unwrap();
        let reopened = sessions.open("test-workflow").unwrap();
        assert_eq!(
            serde_json::to_value(&saved.draft.triggers).unwrap(),
            serde_json::to_value(&reopened.draft.triggers).unwrap()
        );
    }

    fn sessions() -> (tempfile::TempDir, EditorSessions) {
        let directory = tempfile::tempdir().unwrap();
        let repository = WorkflowRepository::at(directory.path());
        repository
            .create(&Workflow {
                id: "test-workflow".into(),
                revision: 7,
                overlap: false,
                steps: vec![delay("first"), delay("second")],
                outputs: BTreeMap::new(),
            })
            .unwrap();
        (directory, EditorSessions::new(repository))
    }

    fn delay(id: &str) -> Step {
        Step {
            id: id.into(),
            on_failure: FailurePolicy::Stop,
            kind: StepKind::Delay { millis: 10 },
        }
    }

    fn rename(name: &str) -> EditorOperation {
        EditorOperation::Rename { name: name.into() }
    }

    fn save(
        sessions: &mut EditorSessions,
        snapshot: &EditorSnapshot,
    ) -> Result<EditorSnapshot, EditorError> {
        let repository = sessions.repository.clone();
        sessions.save_with(&snapshot.session_id, snapshot.revision, |opened, draft| {
            repository.save_definition(opened, draft).map_err(|error| {
                let code = if matches!(
                    error,
                    WorkflowRepositoryError::Store(StoreError::Conflict(_))
                ) {
                    EditorErrorCode::SaveConflict
                } else {
                    EditorErrorCode::SaveFailed
                };
                EditorError::new(code, "The fixture could not be saved.")
            })?;
            repository.load(&draft.workflow.id).map_err(|_| {
                EditorError::new(
                    EditorErrorCode::SaveFailed,
                    "The fixture could not be reloaded.",
                )
            })
        })
    }

    fn assert_same(left: &EditorSnapshot, right: &EditorSnapshot) {
        assert_eq!(
            serde_json::to_value(left).unwrap(),
            serde_json::to_value(right).unwrap()
        );
    }

    #[test]
    fn value_picker_reads_only_the_requested_draft_revision_without_editing_it() {
        let (_directory, mut sessions) = sessions();
        let first = sessions.open("test-workflow").unwrap();
        let independent = sessions.open("test-workflow").unwrap();
        let edited = sessions
            .apply(
                &first.session_id,
                0,
                EditorOperation::SetKind {
                    step_id: "first".into(),
                    kind: StepKind::SetVariable {
                        name: "recording".into(),
                        value: Input::Literal(true.into()),
                    },
                },
            )
            .unwrap();
        let sources = sessions
            .value_sources(&first.session_id, edited.revision, "second", &[], |_| None)
            .unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].label, "recording");
        assert!(
            sessions
                .value_sources(&independent.session_id, 0, "second", &[], |_| None)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            sessions
                .value_sources(&first.session_id, 0, "second", &[], |_| None)
                .unwrap_err()
                .code,
            EditorErrorCode::StaleRevision
        );
        assert_eq!(
            sessions
                .value_sources(&first.session_id, edited.revision, "missing", &[], |_| None)
                .unwrap_err()
                .code,
            EditorErrorCode::InvalidEdit
        );
        assert_same(&edited, &sessions.snapshot(&first.session_id).unwrap());
        sessions.close(&first.session_id).unwrap();
        assert_eq!(
            sessions
                .value_sources(&first.session_id, edited.revision, "second", &[], |_| None)
                .unwrap_err()
                .code,
            EditorErrorCode::MissingSession
        );
    }

    #[test]
    fn stale_and_invalid_edits_leave_draft_and_history_unchanged() {
        let (_directory, mut sessions) = sessions();
        let opened = sessions.open("test-workflow").unwrap();
        let changed = sessions
            .apply(&opened.session_id, 0, rename("Edited"))
            .unwrap();
        assert_eq!(changed.revision, 1);
        assert!(changed.dirty);
        assert!(changed.can_undo);
        assert_eq!(
            sessions
                .apply(&opened.session_id, 0, rename("Stale"))
                .unwrap_err()
                .code,
            EditorErrorCode::StaleRevision
        );
        assert_eq!(
            sessions
                .apply(
                    &opened.session_id,
                    1,
                    EditorOperation::Remove {
                        step_id: "missing".into()
                    }
                )
                .unwrap_err()
                .code,
            EditorErrorCode::InvalidEdit
        );
        assert_same(&changed, &sessions.snapshot(&opened.session_id).unwrap());
    }

    #[test]
    fn reordering_undo_and_redo_use_the_core_history() {
        let (_directory, mut sessions) = sessions();
        let opened = sessions.open("test-workflow").unwrap();
        let moved = sessions
            .apply(
                &opened.session_id,
                0,
                EditorOperation::MoveTo {
                    step_id: "second".into(),
                    destination: StepDestination::Root,
                    position: StepPosition::Before {
                        step_id: "first".into(),
                    },
                },
            )
            .unwrap();
        assert_eq!(moved.draft.workflow.steps[0].id, "second");
        let undone = sessions
            .apply(&opened.session_id, 1, EditorOperation::Undo)
            .unwrap();
        assert_eq!(undone.draft.workflow.steps[0].id, "first");
        assert!(!undone.dirty);
        assert!(undone.can_redo);
        let redone = sessions
            .apply(&opened.session_id, 2, EditorOperation::Redo)
            .unwrap();
        assert_eq!(redone.draft.workflow.steps[0].id, "second");
        assert_eq!(redone.revision, 3);
    }

    #[test]
    fn sessions_for_the_same_workflow_keep_independent_drafts() {
        let (_directory, mut sessions) = sessions();
        let first = sessions.open("test-workflow").unwrap();
        let second = sessions.open("test-workflow").unwrap();
        assert_ne!(first.session_id, second.session_id);
        sessions
            .apply(&first.session_id, 0, rename("First editor"))
            .unwrap();
        assert_same(&second, &sessions.snapshot(&second.session_id).unwrap());
    }

    #[test]
    fn save_conflict_retains_the_draft_and_undo_state() {
        let (_directory, mut sessions) = sessions();
        let opened = sessions.open("test-workflow").unwrap();
        let concurrent = sessions.open("test-workflow").unwrap();
        let draft = sessions
            .apply(&opened.session_id, 0, rename("My edit"))
            .unwrap();
        let concurrent = sessions
            .apply(&concurrent.session_id, 0, rename("Other edit"))
            .unwrap();
        save(&mut sessions, &concurrent).unwrap();
        assert_eq!(
            save(&mut sessions, &draft).unwrap_err().code,
            EditorErrorCode::SaveConflict
        );
        assert_same(&draft, &sessions.snapshot(&opened.session_id).unwrap());
    }

    #[test]
    fn successful_save_keeps_identity_and_refreshes_the_saved_revision() {
        let (_directory, mut sessions) = sessions();
        let opened = sessions.open("test-workflow").unwrap();
        assert_eq!(opened.saved_revision, 7);
        let draft = sessions
            .apply(&opened.session_id, 0, rename("Saved edit"))
            .unwrap();
        let saved = save(&mut sessions, &draft).unwrap();
        assert_eq!(saved.session_id, opened.session_id);
        assert_eq!(saved.saved_revision, 8);
        assert_eq!(saved.revision, 2);
        assert!(!saved.dirty);
        assert!(saved.can_undo);
        let undone = sessions
            .apply(&saved.session_id, 2, EditorOperation::Undo)
            .unwrap();
        assert!(undone.dirty);
        let inverse = save(&mut sessions, &undone).unwrap();
        assert_eq!(inverse.saved_revision, 9);
        assert!(!inverse.dirty);
    }

    #[test]
    fn stale_save_never_calls_persistence_and_mismatched_save_retains_draft() {
        let (_directory, mut sessions) = sessions();
        let opened = sessions.open("test-workflow").unwrap();
        let draft = sessions
            .apply(&opened.session_id, 0, rename("Edited"))
            .unwrap();
        let error = sessions
            .save_with(&opened.session_id, 0, |_, _| {
                panic!("stale request reached persistence")
            })
            .unwrap_err();
        assert_eq!(error.code, EditorErrorCode::StaleRevision);
        let different = sessions.repository.load("test-workflow").unwrap();
        let error = sessions
            .save_with(&opened.session_id, 1, |_, _| Ok(different))
            .unwrap_err();
        assert_eq!(error.code, EditorErrorCode::SaveConflict);
        assert_same(&draft, &sessions.snapshot(&opened.session_id).unwrap());
    }

    #[test]
    fn exhausted_revision_rejects_edits_and_persistence_before_work() {
        let (_directory, mut sessions) = sessions();
        let opened = sessions.open("test-workflow").unwrap();
        sessions
            .sessions
            .get_mut(&opened.session_id)
            .unwrap()
            .revision = u64::MAX;
        let before = sessions.snapshot(&opened.session_id).unwrap();
        assert_eq!(
            sessions
                .apply(&opened.session_id, u64::MAX, rename("Overflow"))
                .unwrap_err()
                .code,
            EditorErrorCode::RevisionExhausted
        );
        assert_eq!(
            sessions
                .save_with(&opened.session_id, u64::MAX, |_, _| panic!(
                    "exhausted revision reached persistence"
                ))
                .unwrap_err()
                .code,
            EditorErrorCode::RevisionExhausted
        );
        assert_same(&before, &sessions.snapshot(&opened.session_id).unwrap());
    }

    #[test]
    fn closed_sessions_reject_reads_and_edits() {
        let (_directory, mut sessions) = sessions();
        let opened = sessions.open("test-workflow").unwrap();
        sessions.close(&opened.session_id).unwrap();
        assert_eq!(
            sessions.snapshot(&opened.session_id).unwrap_err().code,
            EditorErrorCode::MissingSession
        );
        assert_eq!(
            sessions
                .apply(&opened.session_id, 0, rename("Closed"))
                .unwrap_err()
                .code,
            EditorErrorCode::MissingSession
        );
        assert_eq!(
            sessions.close(&opened.session_id).unwrap_err().code,
            EditorErrorCode::MissingSession
        );
    }
}
