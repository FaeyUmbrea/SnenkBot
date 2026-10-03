//! In-memory editing sessions for loaded workflow definitions.

use thiserror::Error;

use crate::engine::{FailurePolicy, Input, Step, StepKind, validate_configured_inputs};
use crate::schema::ConfigSchema;
use crate::workflows::{
    EditableWorkflow, TriggerDefinition, TriggerKind, WorkflowDefinition, validate_display_name,
};

/// A focused edit session keeps the loaded snapshot available for optimistic saves while
/// edits, undo, and redo operate only on an in-memory definition.
#[derive(Clone, Debug)]
pub struct WorkflowEditSession {
    opened: EditableWorkflow,
    draft: WorkflowDefinition,
    edits: Vec<Edit>,
    cursor: usize,
}

#[derive(Clone, Debug)]
struct InputEdit {
    step_id: String,
    field_id: String,
    before: Option<Input>,
    after: Option<Input>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StepLocation {
    path: Vec<StepPathSegment>,
    index: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StepPathSegment {
    index: usize,
    branch: NestedSteps,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NestedSteps {
    Then,
    Else,
    Body,
}

#[derive(Clone, Debug)]
enum Edit {
    WorkflowEnabled {
        before: bool,
        after: bool,
    },
    Input(InputEdit),
    InsertStep {
        location: StepLocation,
        step: Step,
    },
    SetStepKind {
        location: StepLocation,
        step_id: String,
        before: StepKind,
        after: StepKind,
    },
    RemoveStep {
        location: StepLocation,
        step: Step,
    },
    MoveStep {
        location: StepLocation,
        to: usize,
        step_id: String,
    },
    RelocateStep {
        before: StepLocation,
        after: StepLocation,
        step_id: String,
    },
    InsertTrigger {
        index: usize,
        trigger: TriggerDefinition,
    },
    RemoveTrigger {
        index: usize,
        trigger: TriggerDefinition,
    },
    MoveTrigger {
        before: usize,
        after: usize,
        id: String,
    },
    TriggerEnabled {
        id: String,
        before: bool,
        after: bool,
    },
    TriggerKind {
        id: String,
        before: TriggerKind,
        after: TriggerKind,
    },
    Name {
        before: Option<String>,
        after: String,
    },
}

impl Edit {
    fn apply(&self, draft: &mut WorkflowDefinition, undo: bool) {
        match self {
            Self::WorkflowEnabled { before, after } => {
                draft.enabled = if undo { *before } else { *after }
            }
            Self::TriggerEnabled { id, before, after } => {
                if let Some(trigger) = draft.triggers.iter_mut().find(|trigger| &trigger.id == id) {
                    trigger.enabled = if undo { *before } else { *after };
                }
            }
            Self::TriggerKind { id, before, after } => {
                if let Some(trigger) = draft.triggers.iter_mut().find(|trigger| &trigger.id == id) {
                    trigger.kind = if undo { before.clone() } else { after.clone() };
                }
            }
            Self::Input(edit) => replace_input(
                &mut draft.workflow.steps,
                &edit.step_id,
                &edit.field_id,
                if undo { &edit.before } else { &edit.after },
            ),
            Self::Name { before, after } => {
                draft.name = if undo {
                    before.clone()
                } else {
                    Some(after.clone())
                };
            }
            Self::InsertStep { location, step } => {
                let Some(siblings) = step_siblings_mut(&mut draft.workflow.steps, &location.path)
                else {
                    return;
                };
                if undo {
                    if siblings
                        .get(location.index)
                        .is_some_and(|candidate| candidate.id == step.id)
                    {
                        siblings.remove(location.index);
                    }
                } else if location.index <= siblings.len() {
                    siblings.insert(location.index, step.clone());
                }
            }
            Self::SetStepKind {
                location,
                step_id,
                before,
                after,
            } => {
                let Some(siblings) = step_siblings_mut(&mut draft.workflow.steps, &location.path)
                else {
                    return;
                };
                if let Some(step) = siblings
                    .get_mut(location.index)
                    .filter(|candidate| candidate.id == *step_id)
                {
                    step.kind = if undo { before.clone() } else { after.clone() };
                }
            }
            Self::RemoveStep { location, step } => {
                let Some(siblings) = step_siblings_mut(&mut draft.workflow.steps, &location.path)
                else {
                    return;
                };
                if undo {
                    if location.index <= siblings.len() {
                        siblings.insert(location.index, step.clone());
                    }
                } else if siblings
                    .get(location.index)
                    .is_some_and(|candidate| candidate.id == step.id)
                {
                    siblings.remove(location.index);
                }
            }
            Self::MoveStep {
                location,
                to,
                step_id,
            } => {
                let Some(siblings) = step_siblings_mut(&mut draft.workflow.steps, &location.path)
                else {
                    return;
                };
                let (source, destination) = if undo {
                    (*to, location.index)
                } else {
                    (location.index, *to)
                };
                if destination <= siblings.len()
                    && siblings
                        .get(source)
                        .is_some_and(|candidate| candidate.id == *step_id)
                {
                    let step = siblings.remove(source);
                    siblings.insert(destination, step);
                }
            }
            Self::RelocateStep {
                before,
                after,
                step_id,
            } => {
                let source = locate_step(&draft.workflow.steps, step_id)
                    .expect("recorded moved step remains unique");
                let step = step_siblings_mut(&mut draft.workflow.steps, &source.path)
                    .expect("recorded source path remains valid")
                    .remove(source.index);
                let destination = if undo { before } else { after };
                step_siblings_mut(&mut draft.workflow.steps, &destination.path)
                    .expect("recorded destination path remains valid after removal")
                    .insert(destination.index, step);
            }
            Self::InsertTrigger { index, trigger } | Self::RemoveTrigger { index, trigger } => {
                let remove = matches!(self, Self::InsertTrigger { .. }) == undo;
                if remove {
                    let position = draft
                        .triggers
                        .iter()
                        .position(|item| item.id == trigger.id)
                        .expect("recorded trigger remains unique");
                    draft.triggers.remove(position);
                } else {
                    draft.triggers.insert(*index, trigger.clone());
                }
            }
            Self::MoveTrigger { before, after, id } => {
                let position = draft
                    .triggers
                    .iter()
                    .position(|item| &item.id == id)
                    .expect("recorded trigger remains unique");
                let trigger = draft.triggers.remove(position);
                draft
                    .triggers
                    .insert(if undo { *before } else { *after }, trigger);
            }
        }
    }
}

/// Trigger ordering is presentation order; every enabled matching trigger remains independent.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub enum TriggerPosition {
    Append,
    Before { trigger_id: String },
}

/// Direction for moving a step among its current siblings.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub enum StepMoveDirection {
    Up,
    Down,
}

/// The child list that receives a new step.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub enum StepBranch {
    Then,
    Else,
    Body,
}

/// The sibling list at the root or inside one named container.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub enum StepDestination {
    Root,
    Branch {
        parent_id: String,
        branch: StepBranch,
    },
}

/// Placement within a destination list, independent of changing array indices.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub enum StepPosition {
    Append,
    /// Inserts immediately before this existing sibling. The ID must be unique
    /// across the workflow and belong to the chosen destination list.
    Before {
        step_id: String,
    },
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum EditError {
    #[error("step `{0}` does not exist")]
    MissingStep(String),
    #[error("step ID `{0}` is ambiguous")]
    AmbiguousStep(String),
    #[error("step `{step_id}` has no {branch:?} branch")]
    InvalidStepBranch { step_id: String, branch: StepBranch },
    #[error("step `{0}` is not a sibling in the destination list")]
    InvalidStepPosition(String),
    #[error("step `{step_id}` cannot move into itself or descendant `{parent_id}`")]
    StepMoveCycle { step_id: String, parent_id: String },
    #[error("step ID `{0}` already exists or occurs more than once in the new subtree")]
    DuplicateStepId(String),
    #[error("replacing step `{step_id}` would discard nested step `{nested_id}`")]
    DiscardedNestedStep { step_id: String, nested_id: String },
    #[error("replacing step `{step_id}` would change nested step `{nested_id}`")]
    ChangedNestedStep { step_id: String, nested_id: String },
    #[error("step `{step_id}` is not an action step")]
    NotAction { step_id: String },
    #[error("action step `{step_id}` has no configured input `{field_id}`")]
    MissingField { step_id: String, field_id: String },
    #[error("action step `{step_id}` already has configured input `{field_id}`")]
    FieldAlreadyPresent { step_id: String, field_id: String },
    #[error(
        "workflow name must be 1–80 characters without surrounding spaces or control characters"
    )]
    InvalidName,
    #[error("workflow revision overflow")]
    RevisionOverflow,
    #[error("saved workflow does not match the current draft")]
    SavedDefinitionMismatch,
    #[error("invalid inputs for action `{action_id}`: {message}")]
    InvalidActionInputs { action_id: String, message: String },
    #[error("this trigger is already configured")]
    DuplicateTrigger,
    #[error("this trigger requires a scene name")]
    MissingScene,
    #[error("this trigger type is not available")]
    UnavailableTrigger,
    #[error("trigger `{0}` does not exist")]
    MissingTrigger(String),
    #[error("{0}")]
    InvalidTrigger(String),
}

impl WorkflowEditSession {
    pub fn new(opened: EditableWorkflow) -> Self {
        Self {
            draft: opened.definition().clone(),
            opened,
            edits: Vec::new(),
            cursor: 0,
        }
    }

    /// The original loaded value, including the snapshot used by `AppServices::save_workflow`.
    pub fn opened(&self) -> &EditableWorkflow {
        &self.opened
    }

    /// The current editable draft. Borrowing it leaves the session usable after save failures.
    pub fn draft(&self) -> &WorkflowDefinition {
        &self.draft
    }

    /// Refreshes the optimistic-save snapshot after persistence succeeds. The edit
    /// history remains intact, so Undo can save an inverse edit as a new revision.
    pub fn accept_saved(&mut self, saved: EditableWorkflow) -> Result<(), EditError> {
        if saved.workflow().revision != self.draft.workflow.revision
            || definition_data(saved.definition()) != definition_data(&self.draft)
        {
            return Err(EditError::SavedDefinitionMismatch);
        }
        self.opened = saved;
        Ok(())
    }

    /// Whether the current definition differs from the opened definition.
    pub fn is_dirty(&self) -> bool {
        definition_data(&self.draft) != definition_data(self.opened.definition())
    }

    pub fn can_undo(&self) -> bool {
        self.cursor > 0
    }

    pub fn can_redo(&self) -> bool {
        self.cursor < self.edits.len()
    }

    pub fn rename(&mut self, name: String) -> Result<(), EditError> {
        validate_display_name(&name).map_err(|_| EditError::InvalidName)?;
        if self.draft.title() == name {
            return Ok(());
        }
        self.record_edit(Edit::Name {
            before: self.draft.name.clone(),
            after: name,
        })
    }

    pub fn set_enabled(&mut self, enabled: bool) -> Result<(), EditError> {
        if self.draft.enabled == enabled {
            return Ok(());
        }
        self.record_edit(Edit::WorkflowEnabled {
            before: self.draft.enabled,
            after: enabled,
        })
    }

    pub fn set_trigger_enabled(&mut self, id: &str, enabled: bool) -> Result<(), EditError> {
        let trigger = self
            .draft
            .triggers
            .iter()
            .find(|trigger| trigger.id == id)
            .ok_or_else(|| EditError::MissingTrigger(id.to_owned()))?;
        if trigger.enabled == enabled {
            return Ok(());
        }
        self.record_edit(Edit::TriggerEnabled {
            id: id.to_owned(),
            before: trigger.enabled,
            after: enabled,
        })
    }

    pub fn set_trigger_kind(&mut self, id: &str, kind: TriggerKind) -> Result<(), EditError> {
        let trigger = self
            .draft
            .triggers
            .iter()
            .find(|trigger| trigger.id == id)
            .ok_or_else(|| EditError::MissingTrigger(id.to_owned()))?;
        let supported = match (&trigger.kind, &kind) {
            (TriggerKind::ObsCurrentScene { .. }, TriggerKind::ObsCurrentScene { scene }) => {
                if scene.trim().is_empty() {
                    return Err(EditError::MissingScene);
                }
                if self.draft.triggers.iter().any(|other| {
                    other.id != id && matches!(&other.kind, TriggerKind::ObsCurrentScene { scene: other_scene } if other_scene == scene)
                }) {
                    return Err(EditError::DuplicateTrigger);
                }
                true
            }
            (
                TriggerKind::IntegrationEvent {
                    integration: old_integration,
                    event: old_event,
                    ..
                },
                TriggerKind::IntegrationEvent {
                    integration, event, ..
                },
            ) => old_integration == integration && old_event == event,
            _ => false,
        };
        if !supported {
            return Err(EditError::UnavailableTrigger);
        }
        let candidate = TriggerDefinition {
            id: id.to_owned(),
            enabled: trigger.enabled,
            kind: kind.clone(),
        };
        if let TriggerKind::IntegrationEvent { integration, .. } = &kind {
            let validation = match integration.as_str() {
                "twitch" => crate::twitch::events::validate_trigger(&candidate),
                "obs" => crate::obs::events::validate_trigger(&candidate),
                "vtube_studio" => crate::vtube::routes::validate_trigger(&candidate),
                _ => Err(format!("Unknown integration {integration}")),
            };
            validation.map_err(EditError::InvalidTrigger)?;
        }
        if serde_json::to_value(&trigger.kind).ok() == serde_json::to_value(&kind).ok() {
            return Ok(());
        }
        self.record_edit(Edit::TriggerKind {
            id: id.to_owned(),
            before: trigger.kind.clone(),
            after: kind,
        })
    }

    /// Appends a registered action as one undoable edit. Required inputs must be
    /// configured before insertion so the resulting workflow can be saved.
    pub fn append_action(
        &mut self,
        schema: &'static ConfigSchema,
        inputs: std::collections::BTreeMap<String, Input>,
    ) -> Result<String, EditError> {
        validate_configured_inputs(schema, &inputs).map_err(|message| {
            EditError::InvalidActionInputs {
                action_id: schema.id.to_owned(),
                message,
            }
        })?;

        self.insert_step(
            StepKind::Action {
                capability: schema.id.to_owned(),
                version: schema.version,
                inputs,
                deadline_ms: None,
            },
            StepDestination::Root,
        )
    }

    /// Appends a new step to the chosen sibling list as one undoable edit.
    /// The new step receives a stable ID and stops execution on failure by default.
    pub fn insert_step(
        &mut self,
        kind: StepKind,
        destination: StepDestination,
    ) -> Result<String, EditError> {
        self.insert_step_at(kind, destination, StepPosition::Append)
    }

    /// Inserts a new step at a stable sibling position as one undoable edit.
    /// The step receives a new ID; supplied nested steps retain their IDs.
    pub fn insert_step_at(
        &mut self,
        kind: StepKind,
        destination: StepDestination,
        position: StepPosition,
    ) -> Result<String, EditError> {
        let location =
            resolve_step_destination(&self.draft.workflow.steps, &destination, &position)?;
        let mut ids = std::collections::HashSet::new();
        collect_step_ids(&self.draft.workflow.steps, &mut ids);
        ensure_new_kind_ids(&kind, &mut ids)?;
        let id = loop {
            let candidate = uuid::Uuid::new_v4().to_string();
            if ids.insert(candidate.clone()) {
                break candidate;
            }
        };
        self.record_edit(Edit::InsertStep {
            location,
            step: Step {
                id: id.clone(),
                on_failure: FailurePolicy::Stop,
                kind,
            },
        })?;
        Ok(id)
    }

    /// Replaces a step's kind as one undoable edit, retaining its ID and failure policy.
    /// All existing nested steps must remain present in the replacement kind.
    pub fn set_step_kind(&mut self, step_id: &str, kind: StepKind) -> Result<(), EditError> {
        let location = locate_step(&self.draft.workflow.steps, step_id)?;
        let siblings = step_siblings(&self.draft.workflow.steps, &location.path)
            .expect("located step path remains valid");
        let before = &siblings[location.index].kind;
        if kind_data(before) == kind_data(&kind) {
            return Ok(());
        }

        let mut previous_ids = std::collections::HashSet::new();
        collect_kind_step_ids(before, &mut previous_ids);
        let mut replacement_ids = std::collections::HashSet::new();
        ensure_new_kind_ids(&kind, &mut replacement_ids)?;
        if replacement_ids.contains(step_id) {
            return Err(EditError::DuplicateStepId(step_id.to_owned()));
        }
        if let Some(nested_id) = previous_ids.difference(&replacement_ids).next() {
            return Err(EditError::DiscardedNestedStep {
                step_id: step_id.to_owned(),
                nested_id: nested_id.clone(),
            });
        }
        for nested_id in &previous_ids {
            let original = find_kind_steps(before, nested_id);
            let replacement = find_kind_steps(&kind, nested_id);
            if original.len() != 1
                || replacement.len() != 1
                || serde_json::to_value(original[0]).expect("workflow step serializes to JSON")
                    != serde_json::to_value(replacement[0])
                        .expect("workflow step serializes to JSON")
            {
                return Err(EditError::ChangedNestedStep {
                    step_id: step_id.to_owned(),
                    nested_id: nested_id.clone(),
                });
            }
        }
        let mut existing_ids = std::collections::HashSet::new();
        collect_step_ids(&self.draft.workflow.steps, &mut existing_ids);
        existing_ids.remove(step_id);
        for previous_id in previous_ids {
            existing_ids.remove(&previous_id);
        }
        if let Some(duplicate) = replacement_ids.intersection(&existing_ids).next() {
            return Err(EditError::DuplicateStepId(duplicate.clone()));
        }
        self.record_edit(Edit::SetStepKind {
            location,
            step_id: step_id.to_owned(),
            before: before.clone(),
            after: kind,
        })
    }

    /// Removes a step and its complete nested subtree as one undoable edit.
    /// References to the removed step remain in the draft for validation to report.
    pub fn remove_step(&mut self, step_id: &str) -> Result<(), EditError> {
        let location = locate_step(&self.draft.workflow.steps, step_id)?;
        let siblings = step_siblings(&self.draft.workflow.steps, &location.path)
            .expect("located step path remains valid");
        let step = siblings[location.index].clone();
        self.record_edit(Edit::RemoveStep { location, step })
    }

    /// Moves a step one place among its current siblings as one undoable edit.
    /// Steps in different branches or containers cannot cross their boundaries.
    pub fn move_step(
        &mut self,
        step_id: &str,
        direction: StepMoveDirection,
    ) -> Result<(), EditError> {
        let location = locate_step(&self.draft.workflow.steps, step_id)?;
        let siblings = step_siblings(&self.draft.workflow.steps, &location.path)
            .expect("located step path remains valid");
        let to = match direction {
            StepMoveDirection::Up => location.index.checked_sub(1),
            StepMoveDirection::Down => {
                (location.index + 1 < siblings.len()).then_some(location.index + 1)
            }
        };
        let Some(to) = to else {
            return Ok(());
        };
        self.record_edit(Edit::MoveStep {
            to,
            location,
            step_id: step_id.to_owned(),
        })
    }

    /// Moves an existing step and its complete subtree as one undoable edit.
    /// IDs and failure policies remain unchanged. The destination parent and
    /// sibling anchor use stable IDs; their indices are resolved after removal.
    /// Moving before itself, before the next sibling, or appending the last
    /// sibling is a no-op and preserves undo/redo history and revision.
    pub fn move_step_to(
        &mut self,
        step_id: &str,
        destination: StepDestination,
        position: StepPosition,
    ) -> Result<(), EditError> {
        let before = locate_step(&self.draft.workflow.steps, step_id)?;
        let target = resolve_step_destination(&self.draft.workflow.steps, &destination, &position)?;
        let siblings = step_siblings(&self.draft.workflow.steps, &before.path)
            .expect("located source path remains valid");
        let source = &siblings[before.index];
        if let StepDestination::Branch { parent_id, .. } = &destination
            && (source.id == *parent_id || !find_kind_steps(&source.kind, parent_id).is_empty())
        {
            return Err(EditError::StepMoveCycle {
                step_id: step_id.to_owned(),
                parent_id: parent_id.clone(),
            });
        }
        if before.path == target.path
            && (before.index == target.index || before.index + 1 == target.index)
        {
            return Ok(());
        }

        // Resolve again without the source so root and ancestor indices cannot
        // shift the destination to another container during a cross-list move.
        let mut remaining = self.draft.workflow.steps.clone();
        step_siblings_mut(&mut remaining, &before.path)
            .expect("located source path remains valid")
            .remove(before.index);
        let after = resolve_step_destination(&remaining, &destination, &position)?;
        self.record_edit(Edit::RelocateStep {
            before,
            after,
            step_id: step_id.to_owned(),
        })
    }

    /// Appends one available trigger as an undoable edit. A trigger's stable ID
    /// remains unchanged when its display label or configuration changes.
    pub fn append_trigger(&mut self, kind: TriggerKind) -> Result<String, EditError> {
        self.insert_trigger_at(kind, TriggerPosition::Append)
    }

    /// Removal retains the complete trigger for undo, including unavailable kinds and disabled state.
    pub fn remove_trigger(&mut self, id: &str) -> Result<(), EditError> {
        let index = self.trigger_index(id)?;
        self.record_edit(Edit::RemoveTrigger {
            index,
            trigger: self.draft.triggers[index].clone(),
        })
    }

    /// A drop changes only presentation order and produces one undo entry.
    pub fn move_trigger_to(
        &mut self,
        id: &str,
        position: TriggerPosition,
    ) -> Result<(), EditError> {
        let before = self.trigger_index(id)?;
        let target = self.trigger_position(&position)?;
        let after = if before < target { target - 1 } else { target };
        if before == after {
            return Ok(());
        }
        self.record_edit(Edit::MoveTrigger {
            before,
            after,
            id: id.to_owned(),
        })
    }

    fn trigger_index(&self, id: &str) -> Result<usize, EditError> {
        self.draft
            .triggers
            .iter()
            .position(|trigger| trigger.id == id)
            .ok_or_else(|| EditError::MissingTrigger(id.to_owned()))
    }

    fn trigger_position(&self, position: &TriggerPosition) -> Result<usize, EditError> {
        match position {
            TriggerPosition::Append => Ok(self.draft.triggers.len()),
            TriggerPosition::Before { trigger_id } => self.trigger_index(trigger_id),
        }
    }

    /// Insert one validated, enabled trigger as one undoable edit.
    pub fn insert_trigger_at(
        &mut self,
        kind: TriggerKind,
        position: TriggerPosition,
    ) -> Result<String, EditError> {
        let index = self.trigger_position(&position)?;
        match &kind {
            TriggerKind::Manual | TriggerKind::ObsRecordingStarted => {}
            TriggerKind::ObsCurrentScene { scene } if !scene.trim().is_empty() => {}
            TriggerKind::ObsCurrentScene { .. } => return Err(EditError::MissingScene),
            TriggerKind::IntegrationEvent { integration, .. } => {
                let candidate = TriggerDefinition {
                    id: "new".into(),
                    enabled: true,
                    kind: kind.clone(),
                };
                let validation = match integration.as_str() {
                    "twitch" if crate::twitch::trigger_value_schema(&kind).is_some() => {
                        crate::twitch::events::validate_trigger(&candidate)
                    }
                    "obs" if crate::obs::trigger_value_schema(&kind).is_some() => {
                        crate::obs::events::validate_trigger(&candidate)
                    }
                    "vtube_studio" if crate::vtube::trigger_value_schema(&kind).is_some() => {
                        crate::vtube::routes::validate_trigger(&candidate)
                    }
                    _ => return Err(EditError::UnavailableTrigger),
                };
                validation.map_err(EditError::InvalidTrigger)?;
            }
        }
        if self
            .draft
            .triggers
            .iter()
            .any(|trigger| match (&trigger.kind, &kind) {
                (TriggerKind::Manual, TriggerKind::Manual)
                | (TriggerKind::ObsRecordingStarted, TriggerKind::ObsRecordingStarted) => true,
                (
                    TriggerKind::ObsCurrentScene { scene: existing },
                    TriggerKind::ObsCurrentScene { scene },
                ) => existing == scene,
                (
                    TriggerKind::IntegrationEvent {
                        integration: existing_integration,
                        event: existing_event,
                        filters: existing_filters,
                    },
                    TriggerKind::IntegrationEvent {
                        integration,
                        event,
                        filters,
                    },
                ) => {
                    existing_integration == integration
                        && existing_event == event
                        && existing_filters == filters
                }
                _ => false,
            })
        {
            return Err(EditError::DuplicateTrigger);
        }
        let ids: std::collections::HashSet<_> = self
            .draft
            .triggers
            .iter()
            .map(|trigger| &trigger.id)
            .collect();
        let id = loop {
            let candidate = uuid::Uuid::new_v4().to_string();
            if !ids.contains(&candidate) {
                break candidate;
            }
        };
        self.record_edit(Edit::InsertTrigger {
            index,
            trigger: TriggerDefinition {
                id: id.clone(),
                enabled: true,
                kind,
            },
        })?;
        Ok(id)
    }

    /// Changes one existing action input. Each successful call is one undoable edit.
    pub fn set_action_input(
        &mut self,
        step_id: &str,
        field_id: &str,
        value: Input,
    ) -> Result<(), EditError> {
        self.change_action_input(step_id, field_id, Some(value), true)
    }

    /// Adds an omitted input. The caller must check the action schema permits the field.
    pub fn add_action_input(
        &mut self,
        step_id: &str,
        field_id: &str,
        value: Input,
    ) -> Result<(), EditError> {
        self.change_action_input(step_id, field_id, Some(value), false)
    }

    /// Removes a configured input. The caller must check the field is optional.
    pub fn remove_action_input(&mut self, step_id: &str, field_id: &str) -> Result<(), EditError> {
        self.change_action_input(step_id, field_id, None, true)
    }

    fn change_action_input(
        &mut self,
        step_id: &str,
        field_id: &str,
        value: Option<Input>,
        expected_presence: bool,
    ) -> Result<(), EditError> {
        let matches = find_steps(&self.draft.workflow.steps, step_id);
        let step = match matches.as_slice() {
            [] => return Err(EditError::MissingStep(step_id.to_owned())),
            [step] => *step,
            _ => return Err(EditError::AmbiguousStep(step_id.to_owned())),
        };
        let inputs = match &step.kind {
            StepKind::Action { inputs, .. } => inputs,
            _ => {
                return Err(EditError::NotAction {
                    step_id: step_id.to_owned(),
                });
            }
        };
        let before = inputs.get(field_id).cloned();
        if expected_presence && before.is_none() {
            return Err(EditError::MissingField {
                step_id: step_id.to_owned(),
                field_id: field_id.to_owned(),
            });
        }
        if !expected_presence && before.is_some() {
            return Err(EditError::FieldAlreadyPresent {
                step_id: step_id.to_owned(),
                field_id: field_id.to_owned(),
            });
        }
        if before.as_ref().map(input_data) == value.as_ref().map(input_data) {
            return Ok(());
        }
        self.record_edit(Edit::Input(InputEdit {
            step_id: step_id.to_owned(),
            field_id: field_id.to_owned(),
            before,
            after: value,
        }))
    }

    fn record_edit(&mut self, edit: Edit) -> Result<(), EditError> {
        self.opened
            .workflow()
            .revision
            .checked_add(1)
            .ok_or(EditError::RevisionOverflow)?;
        if self.cursor < self.edits.len() {
            self.edits.truncate(self.cursor);
        }
        edit.apply(&mut self.draft, false);
        self.edits.push(edit);
        self.cursor += 1;
        self.refresh_revision();
        Ok(())
    }

    pub fn undo(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        self.cursor -= 1;
        self.edits[self.cursor].apply(&mut self.draft, true);
        self.refresh_revision();
        true
    }

    pub fn redo(&mut self) -> bool {
        if self.cursor == self.edits.len() {
            return false;
        }
        self.edits[self.cursor].apply(&mut self.draft, false);
        self.cursor += 1;
        self.refresh_revision();
        true
    }

    fn refresh_revision(&mut self) {
        self.draft.workflow.revision = if self.is_dirty() {
            self.opened.workflow().revision + 1
        } else {
            self.opened.workflow().revision
        };
    }
}

fn find_steps<'a>(steps: &'a [Step], id: &str) -> Vec<&'a Step> {
    let mut found = Vec::new();
    for step in steps {
        if step.id == id {
            found.push(step);
        }
        match &step.kind {
            StepKind::If {
                then_steps,
                else_steps,
                ..
            } => {
                found.extend(find_steps(then_steps, id));
                found.extend(find_steps(else_steps, id));
            }
            StepKind::While { steps, .. } | StepKind::OneOrMore { steps } => {
                found.extend(find_steps(steps, id));
            }
            _ => {}
        }
    }
    found
}

fn locate_step(steps: &[Step], id: &str) -> Result<StepLocation, EditError> {
    let mut matches = Vec::new();
    collect_step_locations(steps, id, &mut Vec::new(), &mut matches);
    match matches.as_slice() {
        [] => Err(EditError::MissingStep(id.to_owned())),
        [location] => Ok(location.clone()),
        _ => Err(EditError::AmbiguousStep(id.to_owned())),
    }
}

fn resolve_step_destination(
    steps: &[Step],
    destination: &StepDestination,
    position: &StepPosition,
) -> Result<StepLocation, EditError> {
    let path = match destination {
        StepDestination::Root => Vec::new(),
        StepDestination::Branch { parent_id, branch } => {
            let mut parent_location = locate_step(steps, parent_id)?;
            let parent = &step_siblings(steps, &parent_location.path)
                .expect("located parent path remains valid")[parent_location.index];
            let nested_branch = match (&parent.kind, branch) {
                (StepKind::If { .. }, StepBranch::Then) => NestedSteps::Then,
                (StepKind::If { .. }, StepBranch::Else) => NestedSteps::Else,
                (StepKind::While { .. } | StepKind::OneOrMore { .. }, StepBranch::Body) => {
                    NestedSteps::Body
                }
                _ => {
                    return Err(EditError::InvalidStepBranch {
                        step_id: parent_id.clone(),
                        branch: *branch,
                    });
                }
            };
            parent_location.path.push(StepPathSegment {
                index: parent_location.index,
                branch: nested_branch,
            });
            parent_location.path
        }
    };
    let index = match position {
        StepPosition::Append => step_siblings(steps, &path)
            .expect("validated destination path remains valid")
            .len(),
        StepPosition::Before { step_id } => {
            let anchor = locate_step(steps, step_id)?;
            if anchor.path != path {
                return Err(EditError::InvalidStepPosition(step_id.clone()));
            }
            anchor.index
        }
    };
    Ok(StepLocation { path, index })
}

fn collect_step_locations(
    steps: &[Step],
    id: &str,
    path: &mut Vec<StepPathSegment>,
    matches: &mut Vec<StepLocation>,
) {
    for (index, step) in steps.iter().enumerate() {
        if step.id == id {
            matches.push(StepLocation {
                path: path.clone(),
                index,
            });
        }
        match &step.kind {
            StepKind::If {
                then_steps,
                else_steps,
                ..
            } => {
                path.push(StepPathSegment {
                    index,
                    branch: NestedSteps::Then,
                });
                collect_step_locations(then_steps, id, path, matches);
                path.pop();
                path.push(StepPathSegment {
                    index,
                    branch: NestedSteps::Else,
                });
                collect_step_locations(else_steps, id, path, matches);
                path.pop();
            }
            StepKind::While { steps, .. } | StepKind::OneOrMore { steps } => {
                path.push(StepPathSegment {
                    index,
                    branch: NestedSteps::Body,
                });
                collect_step_locations(steps, id, path, matches);
                path.pop();
            }
            _ => {}
        }
    }
}

fn step_siblings<'a>(steps: &'a [Step], path: &[StepPathSegment]) -> Option<&'a [Step]> {
    let Some((segment, rest)) = path.split_first() else {
        return Some(steps);
    };
    let container = steps.get(segment.index)?;
    let nested = match (&container.kind, segment.branch) {
        (StepKind::If { then_steps, .. }, NestedSteps::Then) => then_steps,
        (StepKind::If { else_steps, .. }, NestedSteps::Else) => else_steps,
        (StepKind::While { steps, .. } | StepKind::OneOrMore { steps }, NestedSteps::Body) => steps,
        _ => return None,
    };
    step_siblings(nested, rest)
}

fn step_siblings_mut<'a>(
    steps: &'a mut Vec<Step>,
    path: &[StepPathSegment],
) -> Option<&'a mut Vec<Step>> {
    let Some((segment, rest)) = path.split_first() else {
        return Some(steps);
    };
    let container = steps.get_mut(segment.index)?;
    let nested = match (&mut container.kind, segment.branch) {
        (StepKind::If { then_steps, .. }, NestedSteps::Then) => then_steps,
        (StepKind::If { else_steps, .. }, NestedSteps::Else) => else_steps,
        (StepKind::While { steps, .. } | StepKind::OneOrMore { steps }, NestedSteps::Body) => steps,
        _ => return None,
    };
    step_siblings_mut(nested, rest)
}

fn collect_step_ids(steps: &[Step], ids: &mut std::collections::HashSet<String>) {
    for step in steps {
        ids.insert(step.id.clone());
        match &step.kind {
            StepKind::If {
                then_steps,
                else_steps,
                ..
            } => {
                collect_step_ids(then_steps, ids);
                collect_step_ids(else_steps, ids);
            }
            StepKind::While { steps, .. } | StepKind::OneOrMore { steps } => {
                collect_step_ids(steps, ids);
            }
            _ => {}
        }
    }
}

fn collect_kind_step_ids(kind: &StepKind, ids: &mut std::collections::HashSet<String>) {
    match kind {
        StepKind::If {
            then_steps,
            else_steps,
            ..
        } => {
            collect_step_ids(then_steps, ids);
            collect_step_ids(else_steps, ids);
        }
        StepKind::While { steps, .. } | StepKind::OneOrMore { steps } => {
            collect_step_ids(steps, ids);
        }
        _ => {}
    }
}

fn find_kind_steps<'a>(kind: &'a StepKind, id: &str) -> Vec<&'a Step> {
    match kind {
        StepKind::If {
            then_steps,
            else_steps,
            ..
        } => {
            let mut found = find_steps(then_steps, id);
            found.extend(find_steps(else_steps, id));
            found
        }
        StepKind::While { steps, .. } | StepKind::OneOrMore { steps } => find_steps(steps, id),
        _ => Vec::new(),
    }
}

fn ensure_new_kind_ids(
    kind: &StepKind,
    ids: &mut std::collections::HashSet<String>,
) -> Result<(), EditError> {
    fn check_steps(
        steps: &[Step],
        ids: &mut std::collections::HashSet<String>,
    ) -> Result<(), EditError> {
        for step in steps {
            if step.id.is_empty() || !ids.insert(step.id.clone()) {
                return Err(EditError::DuplicateStepId(step.id.clone()));
            }
            ensure_new_kind_ids(&step.kind, ids)?;
        }
        Ok(())
    }

    match kind {
        StepKind::If {
            then_steps,
            else_steps,
            ..
        } => {
            check_steps(then_steps, ids)?;
            check_steps(else_steps, ids)
        }
        StepKind::While { steps, .. } | StepKind::OneOrMore { steps } => check_steps(steps, ids),
        _ => Ok(()),
    }
}

fn kind_data(kind: &StepKind) -> serde_json::Value {
    serde_json::to_value(kind).expect("workflow step kind serializes to JSON")
}

fn replace_input(steps: &mut [Step], step_id: &str, field_id: &str, value: &Option<Input>) {
    for step in steps {
        if step.id == step_id {
            if let StepKind::Action { inputs, .. } = &mut step.kind {
                if let Some(value) = value {
                    inputs.insert(field_id.to_owned(), value.clone());
                } else {
                    inputs.remove(field_id);
                }
            }
            return;
        }
        match &mut step.kind {
            StepKind::If {
                then_steps,
                else_steps,
                ..
            } => {
                replace_input(then_steps, step_id, field_id, value);
                replace_input(else_steps, step_id, field_id, value);
            }
            StepKind::While { steps, .. } | StepKind::OneOrMore { steps } => {
                replace_input(steps, step_id, field_id, value);
            }
            _ => {}
        }
    }
}

fn input_data(input: &Input) -> serde_json::Value {
    serde_json::to_value(input).expect("workflow input serializes to JSON")
}

fn definition_data(definition: &WorkflowDefinition) -> serde_json::Value {
    let mut data =
        serde_json::to_value(definition).expect("workflow definition serializes to JSON");
    if let Some(workflow) = data
        .get_mut("workflow")
        .and_then(serde_json::Value::as_object_mut)
    {
        workflow.remove("revision");
    }
    data
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;
    use tempfile::{TempDir, tempdir};

    use crate::schema::{ConfigField, ConfigFieldKind, ConfigSchema};

    use super::{
        EditError, StepBranch, StepDestination, StepMoveDirection, StepPosition, TriggerPosition,
        WorkflowEditSession,
    };
    use crate::engine::{FailurePolicy, Input, InputField, Step, StepKind, Workflow};
    use crate::workflows::{TriggerKind, WorkflowDefinition, WorkflowRepository};

    static ACTION_SCHEMA: ConfigSchema = ConfigSchema {
        id: "sample.action",
        version: 3,
        title: "Sample action",
        fields: &[
            ConfigField {
                id: "message",
                label: "Message",
                description: "Text to send",
                introduced_in: 1,
                kind: ConfigFieldKind::Text,
                required: true,
                choice_source: None,
            },
            ConfigField {
                id: "count",
                label: "Count",
                description: "Optional count",
                introduced_in: 2,
                kind: ConfigFieldKind::Integer,
                required: false,
                choice_source: None,
            },
        ],
        outputs: &[],
    };

    fn action(id: &str, field: &str, value: serde_json::Value) -> Step {
        Step {
            id: id.to_owned(),
            on_failure: FailurePolicy::Stop,
            kind: StepKind::Action {
                capability: "sample.action".into(),
                version: 1,
                inputs: BTreeMap::from([(field.into(), Input::Literal(value))]),
                deadline_ms: None,
            },
        }
    }

    fn delay(id: &str, millis: u64) -> Step {
        Step {
            id: id.to_owned(),
            on_failure: FailurePolicy::Stop,
            kind: StepKind::Delay { millis },
        }
    }

    fn session() -> (TempDir, WorkflowEditSession) {
        session_with_steps(vec![Step {
            id: "group".into(),
            on_failure: FailurePolicy::Stop,
            kind: StepKind::If {
                condition: crate::engine::Condition::Exists(Input::Literal(json!(true))),
                then_steps: vec![action("nested", "message", json!("before"))],
                else_steps: vec![],
            },
        }])
    }

    fn session_with_steps(steps: Vec<Step>) -> (TempDir, WorkflowEditSession) {
        let directory = tempdir().unwrap();
        let repository = WorkflowRepository::at(directory.path());
        let workflow = Workflow {
            id: "edit-me".into(),
            revision: 7,
            overlap: false,
            steps,
            outputs: BTreeMap::new(),
        };
        repository
            .create_definition(&WorkflowDefinition::manual(workflow))
            .unwrap();
        let opened = repository.load("edit-me").unwrap();
        (directory, WorkflowEditSession::new(opened))
    }

    fn nested_value(session: &WorkflowEditSession) -> &Input {
        let StepKind::If { then_steps, .. } = &session.draft().workflow.steps[0].kind else {
            panic!("expected if step");
        };
        let StepKind::Action { inputs, .. } = &then_steps[0].kind else {
            panic!("expected action step");
        };
        &inputs["message"]
    }

    fn branch(parent_id: &str, branch: StepBranch) -> StepDestination {
        StepDestination::Branch {
            parent_id: parent_id.into(),
            branch,
        }
    }

    fn before(step_id: &str) -> StepPosition {
        StepPosition::Before {
            step_id: step_id.into(),
        }
    }

    fn drag_steps() -> Vec<Step> {
        vec![
            delay("first", 1),
            Step {
                id: "group".into(),
                on_failure: FailurePolicy::Stop,
                kind: StepKind::If {
                    condition: crate::engine::Condition::Exists(Input::Literal(json!(true))),
                    then_steps: vec![delay("then-a", 2), delay("then-b", 3)],
                    else_steps: vec![Step {
                        id: "loop".into(),
                        on_failure: FailurePolicy::Stop,
                        kind: StepKind::OneOrMore {
                            steps: vec![delay("leaf", 4)],
                        },
                    }],
                },
            },
            delay("last", 5),
        ]
    }

    fn sibling_ids(session: &WorkflowEditSession, destination: StepDestination) -> Vec<String> {
        let location = super::resolve_step_destination(
            &session.draft.workflow.steps,
            &destination,
            &StepPosition::Append,
        )
        .unwrap();
        super::step_siblings(&session.draft.workflow.steps, &location.path)
            .unwrap()
            .iter()
            .map(|step| step.id.clone())
            .collect()
    }

    #[test]
    fn positional_insert_uses_sibling_ids_and_undo_restores_the_original() {
        let (_directory, mut session) = session_with_steps(drag_steps());
        let original = super::definition_data(session.draft());
        let id = session
            .insert_step_at(
                StepKind::Delay { millis: 6 },
                branch("group", StepBranch::Then),
                before("then-b"),
            )
            .unwrap();
        assert_eq!(
            sibling_ids(&session, branch("group", StepBranch::Then)),
            ["then-a", id.as_str(), "then-b"]
        );
        assert_eq!(session.draft.workflow.revision, 8);
        let inserted = super::definition_data(session.draft());
        assert!(session.undo());
        assert_eq!(super::definition_data(session.draft()), original);
        assert_eq!(session.draft.workflow.revision, 7);
        assert!(!session.can_undo());
        assert!(session.redo());
        assert_eq!(super::definition_data(session.draft()), inserted);
        assert_eq!(session.draft.workflow.revision, 8);
    }

    #[test]
    fn positional_moves_preserve_subtrees_and_undo_as_one_edit() {
        let cases = [
            (
                "last",
                StepDestination::Root,
                before("first"),
                vec!["last", "first", "group"],
            ),
            (
                "first",
                StepDestination::Root,
                StepPosition::Append,
                vec!["group", "last", "first"],
            ),
            (
                "then-b",
                branch("group", StepBranch::Then),
                before("then-a"),
                vec!["then-b", "then-a"],
            ),
            (
                "then-a",
                branch("group", StepBranch::Else),
                before("loop"),
                vec!["then-a", "loop"],
            ),
            (
                "first",
                branch("group", StepBranch::Then),
                before("then-b"),
                vec!["then-a", "first", "then-b"],
            ),
            (
                "loop",
                StepDestination::Root,
                before("first"),
                vec!["loop", "first", "group", "last"],
            ),
            (
                "group",
                StepDestination::Root,
                StepPosition::Append,
                vec!["first", "last", "group"],
            ),
            (
                "then-a",
                branch("loop", StepBranch::Body),
                StepPosition::Append,
                vec!["leaf", "then-a"],
            ),
        ];
        for (id, destination, position, expected) in cases {
            let (directory, mut session) = session_with_steps(drag_steps());
            let original = super::definition_data(session.draft());
            let moved_step =
                serde_json::to_value(super::find_steps(&session.draft.workflow.steps, id)[0])
                    .unwrap();
            session
                .move_step_to(id, destination.clone(), position)
                .unwrap();
            assert_eq!(sibling_ids(&session, destination), expected);
            assert_eq!(
                serde_json::to_value(super::find_steps(&session.draft.workflow.steps, id)[0])
                    .unwrap(),
                moved_step
            );
            assert_eq!(session.draft.workflow.revision, 8);
            let moved = super::definition_data(session.draft());
            assert!(session.undo());
            assert_eq!(super::definition_data(session.draft()), original);
            assert_eq!(session.draft.workflow.revision, 7);
            assert!(!session.can_undo());
            assert!(session.redo());
            assert_eq!(super::definition_data(session.draft()), moved);
            assert_eq!(session.draft.workflow.revision, 8);

            let repository = WorkflowRepository::at(directory.path());
            repository
                .save_definition(session.opened(), session.draft())
                .unwrap();
            session
                .accept_saved(repository.load("edit-me").unwrap())
                .unwrap();
            assert!(session.undo());
            assert_eq!(super::definition_data(session.draft()), original);
            assert_eq!(session.draft.workflow.revision, 9);
            assert!(session.redo());
            assert_eq!(session.draft.workflow.revision, 8);
        }
    }

    #[test]
    fn positional_noops_keep_revision_and_redo_history() {
        let (_directory, mut session) = session_with_steps(drag_steps());
        session
            .move_step_to("last", StepDestination::Root, before("first"))
            .unwrap();
        assert!(session.undo());
        let original = super::definition_data(session.draft());
        for (id, destination, position) in [
            ("first", StepDestination::Root, before("first")),
            ("first", StepDestination::Root, before("group")),
            ("last", StepDestination::Root, StepPosition::Append),
            (
                "then-a",
                branch("group", StepBranch::Then),
                before("then-b"),
            ),
        ] {
            session.move_step_to(id, destination, position).unwrap();
            assert_eq!(super::definition_data(session.draft()), original);
            assert_eq!(session.draft.workflow.revision, 7);
            assert!(!session.can_undo());
            assert!(session.can_redo());
        }
        assert!(session.redo());
        assert_eq!(
            sibling_ids(&session, StepDestination::Root),
            ["last", "first", "group"]
        );
    }

    #[test]
    fn invalid_positional_edits_leave_draft_and_history_unchanged() {
        let (_directory, mut session) = session_with_steps(drag_steps());
        session
            .move_step_to("last", StepDestination::Root, before("first"))
            .unwrap();
        assert!(session.undo());
        let original = super::definition_data(session.draft());
        let cases = [
            (
                "group",
                branch("group", StepBranch::Then),
                StepPosition::Append,
                EditError::StepMoveCycle {
                    step_id: "group".into(),
                    parent_id: "group".into(),
                },
            ),
            (
                "group",
                branch("loop", StepBranch::Body),
                StepPosition::Append,
                EditError::StepMoveCycle {
                    step_id: "group".into(),
                    parent_id: "loop".into(),
                },
            ),
            (
                "first",
                branch("group", StepBranch::Body),
                StepPosition::Append,
                EditError::InvalidStepBranch {
                    step_id: "group".into(),
                    branch: StepBranch::Body,
                },
            ),
            (
                "first",
                branch("missing", StepBranch::Then),
                StepPosition::Append,
                EditError::MissingStep("missing".into()),
            ),
            (
                "first",
                StepDestination::Root,
                before("then-a"),
                EditError::InvalidStepPosition("then-a".into()),
            ),
            (
                "first",
                StepDestination::Root,
                before("missing"),
                EditError::MissingStep("missing".into()),
            ),
            (
                "missing",
                StepDestination::Root,
                StepPosition::Append,
                EditError::MissingStep("missing".into()),
            ),
        ];
        for (id, destination, position, error) in cases {
            assert_eq!(session.move_step_to(id, destination, position), Err(error));
            assert_eq!(super::definition_data(session.draft()), original);
            assert_eq!(session.draft.workflow.revision, 7);
            assert!(!session.can_undo());
            assert!(session.can_redo());
        }
        assert_eq!(
            session.insert_step_at(
                StepKind::Delay { millis: 6 },
                StepDestination::Root,
                before("leaf")
            ),
            Err(EditError::InvalidStepPosition("leaf".into()))
        );
        assert_eq!(super::definition_data(session.draft()), original);
        assert!(session.can_redo());
    }

    #[test]
    fn positional_edits_reject_revision_overflow_without_mutation() {
        let (_directory, mut session) = session_with_steps(drag_steps());
        session.draft.workflow.revision = u64::MAX;
        let mut definition = session.opened.definition().clone();
        definition.workflow.revision = u64::MAX;
        let directory = tempdir().unwrap();
        let repository = WorkflowRepository::at(directory.path());
        repository.create_definition(&definition).unwrap();
        session.opened = repository.load("edit-me").unwrap();
        let original = serde_json::to_value(session.draft()).unwrap();
        assert_eq!(
            session.move_step_to("last", StepDestination::Root, before("first")),
            Err(EditError::RevisionOverflow)
        );
        assert_eq!(
            session.insert_step_at(
                StepKind::Delay { millis: 6 },
                StepDestination::Root,
                before("first"),
            ),
            Err(EditError::RevisionOverflow)
        );
        assert_eq!(serde_json::to_value(session.draft()).unwrap(), original);
        assert!(!session.can_undo());
        assert!(!session.can_redo());
    }

    #[test]
    fn positional_edits_reject_ambiguous_parent_anchor_and_source_ids() {
        let (_directory, mut session) = session_with_steps(drag_steps());
        session
            .draft
            .workflow
            .steps
            .push(session.draft.workflow.steps[1].clone());
        let original = super::definition_data(session.draft());
        assert_eq!(
            session.move_step_to("group", StepDestination::Root, StepPosition::Append),
            Err(EditError::AmbiguousStep("group".into()))
        );
        assert_eq!(
            session.move_step_to(
                "first",
                branch("group", StepBranch::Then),
                StepPosition::Append
            ),
            Err(EditError::AmbiguousStep("group".into()))
        );
        assert_eq!(
            session.move_step_to("first", StepDestination::Root, before("leaf")),
            Err(EditError::AmbiguousStep("leaf".into()))
        );
        assert_eq!(
            session.insert_step_at(
                StepKind::Delay { millis: 6 },
                StepDestination::Root,
                before("group")
            ),
            Err(EditError::AmbiguousStep("group".into()))
        );
        assert_eq!(super::definition_data(session.draft()), original);
        assert_eq!(session.draft.workflow.revision, 7);
        assert!(!session.can_undo());
        assert!(!session.can_redo());
    }

    #[test]
    fn workflow_activation_preserves_triggers_and_is_undoable() {
        let (_directory, mut session) = session();
        let triggers = serde_json::to_value(&session.draft().triggers).unwrap();
        session.set_enabled(false).unwrap();
        assert!(!session.draft().enabled);
        assert_eq!(
            serde_json::to_value(&session.draft().triggers).unwrap(),
            triggers
        );
        assert!(session.undo());
        assert!(session.draft().enabled);
        assert!(session.redo());
        assert!(!session.draft().enabled);
    }

    #[test]
    fn trigger_activation_is_persisted_and_undoable_without_changing_its_kind() {
        let (directory, mut session) = session();
        let before = serde_json::to_value(&session.draft().triggers[0].kind).unwrap();
        assert_eq!(
            session.set_trigger_enabled("missing", true),
            Err(EditError::MissingTrigger("missing".into()))
        );
        assert!(!session.is_dirty());
        session.set_trigger_enabled("manual", false).unwrap();
        assert!(!session.draft().triggers[0].enabled);
        let repository = WorkflowRepository::at(directory.path());
        repository
            .save_definition(session.opened(), session.draft())
            .unwrap();
        let saved = repository.load("edit-me").unwrap();
        assert!(!saved.triggers()[0].enabled);
        session.accept_saved(saved).unwrap();
        assert!(session.undo());
        assert!(session.draft().triggers[0].enabled);
        assert_eq!(session.draft().workflow.revision, 9);
        assert_eq!(
            serde_json::to_value(&session.draft().triggers[0].kind).unwrap(),
            before
        );
        assert!(session.redo());
        assert!(!session.draft().triggers[0].enabled);
    }

    #[test]
    fn command_filter_edits_round_trip_without_changing_trigger_identity() {
        let (directory, mut session) = session();
        let kind = |command: &str, aliases: Vec<&str>| TriggerKind::IntegrationEvent {
            integration: "twitch".into(),
            event: "chat.command".into(),
            filters: BTreeMap::from([
                ("command".into(), json!(command)),
                ("aliases".into(), json!(aliases)),
                ("broadcaster_or_moderator".into(), json!(true)),
            ]),
        };
        let id = session.append_trigger(kind("!hello", vec![])).unwrap();
        assert_eq!(
            session.append_trigger(kind("!hello", vec![])),
            Err(EditError::DuplicateTrigger)
        );
        let changed = kind("!welcome", vec!["!hi"]);
        session.set_trigger_kind(&id, changed.clone()).unwrap();
        let repository = WorkflowRepository::at(directory.path());
        repository
            .save_definition(session.opened(), session.draft())
            .unwrap();
        let saved = repository.load("edit-me").unwrap();
        let trigger = saved
            .definition()
            .triggers
            .iter()
            .find(|trigger| trigger.id == id)
            .unwrap();
        assert!(trigger.enabled);
        assert_eq!(
            serde_json::to_value(&trigger.kind).unwrap(),
            serde_json::to_value(changed).unwrap()
        );
    }

    #[test]
    fn editing_a_scene_trigger_keeps_its_identity_and_can_be_undone() {
        let (_directory, mut session) = session();
        let id = session
            .append_trigger(TriggerKind::ObsCurrentScene {
                scene: "Starting Soon".into(),
            })
            .unwrap();
        session
            .set_trigger_kind(
                &id,
                TriggerKind::ObsCurrentScene {
                    scene: "Live".into(),
                },
            )
            .unwrap();
        assert!(matches!(
            &session.draft().triggers.iter().find(|trigger| trigger.id == id).unwrap().kind,
            TriggerKind::ObsCurrentScene { scene } if scene == "Live"
        ));
        assert!(session.undo());
        assert!(matches!(
            &session.draft().triggers.iter().find(|trigger| trigger.id == id).unwrap().kind,
            TriggerKind::ObsCurrentScene { scene } if scene == "Starting Soon"
        ));
    }

    #[test]
    fn nested_edits_are_undoable_and_revision_tracks_dirty_state_once() {
        let (_directory, mut session) = session();
        assert!(!session.is_dirty());
        session
            .set_action_input("nested", "message", Input::Literal(json!("after")))
            .unwrap();
        assert!(session.is_dirty());
        assert!(session.can_undo());
        assert!(!session.can_redo());
        assert_eq!(session.draft().workflow.revision, 8);
        assert_eq!(
            super::input_data(nested_value(&session)),
            super::input_data(&Input::Literal(json!("after")))
        );
        assert!(session.undo());
        assert!(!session.is_dirty());
        assert!(!session.can_undo());
        assert!(session.can_redo());
        assert_eq!(session.draft().workflow.revision, 7);
        assert!(session.redo());
        assert_eq!(session.draft().workflow.revision, 8);
        assert!(!session.redo());
        assert_eq!(session.opened().workflow().revision, 7);
    }

    #[test]
    fn move_step_only_reorders_siblings_in_each_nested_container() {
        let original_steps = vec![
            Step {
                id: "branch".into(),
                on_failure: FailurePolicy::Stop,
                kind: StepKind::If {
                    condition: crate::engine::Condition::Exists(Input::Literal(json!(true))),
                    then_steps: vec![delay("then-a", 1), delay("then-b", 2)],
                    else_steps: vec![delay("else-a", 3), delay("else-b", 4)],
                },
            },
            Step {
                id: "loop".into(),
                on_failure: FailurePolicy::Stop,
                kind: StepKind::While {
                    condition: crate::engine::Condition::Exists(Input::Literal(json!(true))),
                    steps: vec![delay("while-a", 5), delay("while-b", 6)],
                },
            },
            Step {
                id: "repeat".into(),
                on_failure: FailurePolicy::Stop,
                kind: StepKind::OneOrMore {
                    steps: vec![delay("repeat-a", 7), delay("repeat-b", 8)],
                },
            },
        ];
        let (directory, mut session) = session_with_steps(original_steps.clone());
        let original = serde_json::to_value(&session.draft().workflow.steps).unwrap();

        session.move_step("then-b", StepMoveDirection::Up).unwrap();
        session
            .move_step("else-a", StepMoveDirection::Down)
            .unwrap();
        session.move_step("while-b", StepMoveDirection::Up).unwrap();
        session
            .move_step("repeat-a", StepMoveDirection::Down)
            .unwrap();
        assert_eq!(session.draft().workflow.revision, 8);

        let root_steps = &session.draft().workflow.steps;
        assert_eq!(
            root_steps
                .iter()
                .map(|step| step.id.as_str())
                .collect::<Vec<_>>(),
            ["branch", "loop", "repeat"]
        );
        let StepKind::If {
            then_steps,
            else_steps,
            ..
        } = &root_steps[0].kind
        else {
            panic!("expected branch");
        };
        assert_eq!(
            then_steps
                .iter()
                .map(|step| step.id.as_str())
                .collect::<Vec<_>>(),
            ["then-b", "then-a"]
        );
        assert_eq!(
            else_steps
                .iter()
                .map(|step| step.id.as_str())
                .collect::<Vec<_>>(),
            ["else-b", "else-a"]
        );
        let StepKind::While { steps: body, .. } = &root_steps[1].kind else {
            panic!("expected loop");
        };
        assert_eq!(
            body.iter().map(|step| step.id.as_str()).collect::<Vec<_>>(),
            ["while-b", "while-a"]
        );
        let StepKind::OneOrMore { steps: body, .. } = &root_steps[2].kind else {
            panic!("expected repetition");
        };
        assert_eq!(
            body.iter().map(|step| step.id.as_str()).collect::<Vec<_>>(),
            ["repeat-b", "repeat-a"]
        );

        for _ in 0..4 {
            assert!(session.undo());
        }
        assert_eq!(
            serde_json::to_value(&session.draft().workflow.steps).unwrap(),
            original
        );
        for _ in 0..4 {
            assert!(session.redo());
        }
        assert_eq!(session.draft().workflow.revision, 8);

        let repository = WorkflowRepository::at(directory.path());
        repository
            .save_definition(session.opened(), session.draft())
            .unwrap();
        session
            .accept_saved(repository.load("edit-me").unwrap())
            .unwrap();
        assert!(session.undo());
        let mut expected_after_undo = original_steps.clone();
        let StepKind::If {
            then_steps,
            else_steps,
            ..
        } = &mut expected_after_undo[0].kind
        else {
            unreachable!();
        };
        then_steps.swap(0, 1);
        else_steps.swap(0, 1);
        let StepKind::While { steps, .. } = &mut expected_after_undo[1].kind else {
            unreachable!();
        };
        steps.swap(0, 1);
        assert_eq!(
            serde_json::to_value(&session.draft().workflow.steps).unwrap(),
            serde_json::to_value(expected_after_undo).unwrap()
        );
        assert_eq!(session.draft().workflow.revision, 9);
    }

    #[test]
    fn boundary_moves_are_noops_and_step_edits_reject_missing_or_ambiguous_ids() {
        let (_directory, mut session) = session();
        assert_eq!(session.move_step("group", StepMoveDirection::Up), Ok(()));
        assert_eq!(session.move_step("nested", StepMoveDirection::Down), Ok(()));
        assert_eq!(
            session.remove_step("missing"),
            Err(EditError::MissingStep("missing".into()))
        );
        assert_eq!(
            session.move_step("missing", StepMoveDirection::Up),
            Err(EditError::MissingStep("missing".into()))
        );
        assert!(!session.is_dirty());
        assert!(!session.can_undo());
        assert_eq!(session.draft().workflow.revision, 7);

        let duplicate = match &session.draft().workflow.steps[0].kind {
            StepKind::If { then_steps, .. } => then_steps[0].clone(),
            _ => unreachable!(),
        };
        let StepKind::If { else_steps, .. } = &mut session.draft.workflow.steps[0].kind else {
            unreachable!();
        };
        else_steps.push(duplicate);
        assert_eq!(
            session.remove_step("nested"),
            Err(EditError::AmbiguousStep("nested".into()))
        );
        assert_eq!(
            session.move_step("nested", StepMoveDirection::Up),
            Err(EditError::AmbiguousStep("nested".into()))
        );
        assert_eq!(session.draft().workflow.revision, 7);
        assert!(!session.can_undo());
    }

    #[test]
    fn removing_a_subtree_restores_exact_structure_and_undo_after_save_is_a_new_revision() {
        let subtree = Step {
            id: "outer".into(),
            on_failure: FailurePolicy::Continue,
            kind: StepKind::If {
                condition: crate::engine::Condition::Exists(Input::Literal(json!(true))),
                then_steps: vec![Step {
                    id: "while".into(),
                    on_failure: FailurePolicy::Stop,
                    kind: StepKind::While {
                        condition: crate::engine::Condition::Exists(Input::Literal(json!(false))),
                        steps: vec![action("nested", "message", json!("kept exactly"))],
                    },
                }],
                else_steps: vec![Step {
                    id: "repeat".into(),
                    on_failure: FailurePolicy::Continue,
                    kind: StepKind::OneOrMore {
                        steps: vec![delay("fallback", 123)],
                    },
                }],
            },
        };
        let original_steps = vec![subtree.clone(), delay("after", 50)];
        let original = serde_json::to_value(&original_steps).unwrap();
        let (directory, mut session) = session_with_steps(original_steps);

        session.remove_step("outer").unwrap();
        assert_eq!(
            session
                .draft()
                .workflow
                .steps
                .iter()
                .map(|step| step.id.as_str())
                .collect::<Vec<_>>(),
            ["after"]
        );
        assert_eq!(session.draft().workflow.revision, 8);
        assert!(session.undo());
        assert_eq!(
            serde_json::to_value(&session.draft().workflow.steps).unwrap(),
            original
        );
        assert!(session.redo());
        assert_eq!(session.draft().workflow.steps[0].id, "after");

        let repository = WorkflowRepository::at(directory.path());
        repository
            .save_definition(session.opened(), session.draft())
            .unwrap();
        session
            .accept_saved(repository.load("edit-me").unwrap())
            .unwrap();
        assert!(session.undo());
        assert_eq!(
            serde_json::to_value(&session.draft().workflow.steps).unwrap(),
            original
        );
        assert_eq!(session.draft().workflow.revision, 9);
        repository
            .save_definition(session.opened(), session.draft())
            .unwrap();
        assert_eq!(repository.load("edit-me").unwrap().workflow().revision, 9);
    }

    #[test]
    fn removing_a_referenced_producer_preserves_the_reference_and_save_rejects_the_candidate() {
        let producer = Step {
            id: "prompt".into(),
            on_failure: FailurePolicy::Stop,
            kind: StepKind::RequestInput {
                title: Some("Answer".into()),
                fields: vec![InputField {
                    id: "value".into(),
                    label: Some("Value".into()),
                    default: None,
                    required: true,
                    validator: None,
                }],
            },
        };
        let consumer = Step {
            id: "copy".into(),
            on_failure: FailurePolicy::Stop,
            kind: StepKind::SetVariable {
                name: "answer".into(),
                value: Input::Reference {
                    step_id: "prompt".into(),
                    output_id: "value".into(),
                    fallback: None,
                },
            },
        };
        let (directory, mut session) = session_with_steps(vec![producer, consumer]);
        session.remove_step("prompt").unwrap();
        let StepKind::SetVariable { value, .. } = &session.draft().workflow.steps[0].kind else {
            panic!("expected the reference consumer to remain");
        };
        assert!(
            matches!(value, Input::Reference { step_id, output_id, .. } if step_id == "prompt" && output_id == "value")
        );
        let repository = WorkflowRepository::at(directory.path());
        assert!(
            repository
                .save_definition(session.opened(), session.draft())
                .is_err()
        );
        assert_eq!(session.opened().workflow().revision, 7);
        assert_eq!(session.draft().workflow.revision, 8);
        assert_eq!(session.opened().workflow().steps.len(), 2);
        assert_eq!(session.draft().workflow.steps.len(), 1);
        assert!(session.is_dirty());

        assert!(session.undo());
        assert_eq!(session.draft().workflow.steps[0].id, "prompt");
        assert!(session.redo());
        assert_eq!(session.draft().workflow.steps[0].id, "copy");
        let StepKind::SetVariable { value, .. } = &session.draft().workflow.steps[0].kind else {
            unreachable!();
        };
        assert!(
            matches!(value, Input::Reference { step_id, output_id, .. } if step_id == "prompt" && output_id == "value")
        );
    }

    #[test]
    fn no_op_and_invalid_targets_do_not_change_the_draft() {
        let (_directory, mut session) = session();
        session
            .set_action_input("nested", "message", Input::Literal(json!("before")))
            .unwrap();
        assert!(!session.is_dirty());
        assert!(!session.undo());
        assert_eq!(
            session.set_action_input("missing", "message", Input::Literal(json!("x"))),
            Err(EditError::MissingStep("missing".into()))
        );
        assert_eq!(
            session.set_action_input("group", "message", Input::Literal(json!("x"))),
            Err(EditError::NotAction {
                step_id: "group".into(),
            })
        );
        assert_eq!(
            session.set_action_input("nested", "missing", Input::Literal(json!("x"))),
            Err(EditError::MissingField {
                step_id: "nested".into(),
                field_id: "missing".into(),
            })
        );
        assert!(!session.is_dirty());
        assert_eq!(session.draft().workflow.revision, 7);
    }

    #[test]
    fn optional_input_add_and_remove_are_undoable() {
        let (directory, mut session) = session();
        let inputs = |session: &WorkflowEditSession| {
            let StepKind::If { then_steps, .. } = &session.draft().workflow.steps[0].kind else {
                unreachable!();
            };
            let StepKind::Action { inputs, .. } = &then_steps[0].kind else {
                unreachable!();
            };
            inputs.clone()
        };
        assert_eq!(
            session.remove_action_input("nested", "optional"),
            Err(EditError::MissingField {
                step_id: "nested".into(),
                field_id: "optional".into(),
            })
        );
        session
            .add_action_input("nested", "optional", Input::Literal(json!("new")))
            .unwrap();
        assert_eq!(
            session.add_action_input("nested", "optional", Input::Literal(json!("other"))),
            Err(EditError::FieldAlreadyPresent {
                step_id: "nested".into(),
                field_id: "optional".into(),
            })
        );
        assert!(inputs(&session).contains_key("optional"));
        assert!(session.undo());
        assert!(!inputs(&session).contains_key("optional"));
        assert!(session.redo());
        assert!(inputs(&session).contains_key("optional"));

        let repository = WorkflowRepository::at(directory.path());
        repository
            .save_definition(session.opened(), session.draft())
            .unwrap();
        session
            .accept_saved(repository.load("edit-me").unwrap())
            .unwrap();
        session.remove_action_input("nested", "optional").unwrap();
        assert!(!inputs(&session).contains_key("optional"));
        assert_eq!(session.draft().workflow.revision, 9);
        assert!(session.undo());
        assert!(inputs(&session).contains_key("optional"));
        assert_eq!(session.draft().workflow.revision, 8);
    }

    #[test]
    fn renaming_uses_the_same_history_and_snapshot_revision() {
        let (directory, mut session) = session();
        assert_eq!(
            session.rename(" Bad name ".into()),
            Err(EditError::InvalidName)
        );
        assert!(!session.is_dirty());
        session.rename("A lovely stream".into()).unwrap();
        assert_eq!(session.draft().title(), "A lovely stream");
        assert_eq!(session.draft().workflow.id, "edit-me");
        assert_eq!(session.draft().workflow.revision, 8);
        let repository = WorkflowRepository::at(directory.path());
        repository
            .save_definition(session.opened(), session.draft())
            .unwrap();
        session
            .accept_saved(repository.load("edit-me").unwrap())
            .unwrap();
        session
            .set_action_input("nested", "message", Input::Literal(json!("edited")))
            .unwrap();
        assert!(session.undo());
        assert_eq!(session.draft().title(), "A lovely stream");
        assert!(session.undo());
        assert_eq!(session.draft().title(), "edit-me");
        assert_eq!(session.draft().workflow.revision, 9);
        assert!(session.redo());
        assert_eq!(session.draft().title(), "A lovely stream");
        assert!(session.redo());
        assert_eq!(session.draft().workflow.revision, 9);
    }

    #[test]
    fn session_keeps_opened_snapshot_and_draft_available_for_retry() {
        let (directory, mut session) = session();
        session
            .set_action_input("nested", "message", Input::Literal(json!("edited")))
            .unwrap();
        let repository = WorkflowRepository::at(directory.path());
        let current = repository.load("edit-me").unwrap();
        let mut concurrent = current.definition().clone();
        concurrent.name = Some("Changed elsewhere".into());
        concurrent.workflow.revision += 1;
        repository.save_definition(&current, &concurrent).unwrap();
        assert!(
            repository
                .save_definition(session.opened(), session.draft())
                .is_err()
        );
        let opened_revision = session.opened().workflow().revision;
        let draft_revision = session.draft().workflow.revision;
        assert_eq!(opened_revision, 7);
        assert_eq!(draft_revision, 8);
        assert!(session.is_dirty());
        assert_eq!(
            super::input_data(nested_value(&session)),
            super::input_data(&Input::Literal(json!("edited")))
        );
    }

    #[test]
    fn duplicate_step_ids_are_rejected_as_ambiguous() {
        let (_directory, mut session) = session();
        let duplicate = match &session.draft.workflow.steps[0].kind {
            StepKind::If { then_steps, .. } => then_steps[0].clone(),
            _ => unreachable!(),
        };
        let StepKind::If { else_steps, .. } = &mut session.draft.workflow.steps[0].kind else {
            unreachable!();
        };
        else_steps.push(duplicate);
        assert_eq!(
            session.set_action_input("nested", "message", Input::Literal(json!("x"))),
            Err(EditError::AmbiguousStep("nested".into()))
        );
    }

    #[test]
    fn undo_after_a_successful_save_becomes_a_new_revision() {
        let (directory, mut session) = session();
        session
            .set_action_input("nested", "message", Input::Literal(json!("after")))
            .unwrap();
        let repository = WorkflowRepository::at(directory.path());
        repository
            .save_definition(session.opened(), session.draft())
            .unwrap();
        let saved = repository.load("edit-me").unwrap();
        session.accept_saved(saved).unwrap();
        assert!(!session.is_dirty());
        assert!(session.undo());
        assert!(session.is_dirty());
        assert_eq!(session.draft().workflow.revision, 9);
        repository
            .save_definition(session.opened(), session.draft())
            .unwrap();
        assert_eq!(repository.load("edit-me").unwrap().workflow().revision, 9);
    }

    #[test]
    fn append_action_is_validated_and_undoable_with_a_stable_id() {
        let (_directory, mut session) = session();
        let inputs = BTreeMap::from([
            ("message".into(), Input::Literal(json!("hello"))),
            ("count".into(), Input::Literal(json!(2))),
        ]);
        let id = session
            .append_action(&ACTION_SCHEMA, inputs.clone())
            .unwrap();
        assert_eq!(session.draft().workflow.steps.len(), 2);
        let appended = session.draft().workflow.steps.last().unwrap();
        assert_eq!(appended.id, id);
        assert_eq!(appended.on_failure, crate::engine::FailurePolicy::Stop);
        assert!(uuid::Uuid::parse_str(&id).is_ok());
        assert_ne!(id, "nested");
        let StepKind::Action {
            capability,
            version,
            inputs: actual_inputs,
            deadline_ms,
        } = &appended.kind
        else {
            panic!("expected action step");
        };
        assert_eq!(capability, ACTION_SCHEMA.id);
        assert_eq!(*version, ACTION_SCHEMA.version);
        assert_eq!(
            serde_json::to_value(actual_inputs).unwrap(),
            serde_json::to_value(inputs).unwrap()
        );
        assert_eq!(*deadline_ms, None);
        assert_eq!(session.draft().workflow.revision, 8);

        assert!(session.undo());
        assert_eq!(session.draft().workflow.steps.len(), 1);
        assert!(
            !session
                .draft()
                .workflow
                .steps
                .iter()
                .any(|step| step.id == id)
        );
        assert_eq!(session.draft().workflow.revision, 7);

        assert!(session.redo());
        assert_eq!(session.draft().workflow.steps.last().unwrap().id, id);
        assert_eq!(session.draft().workflow.revision, 8);
    }

    #[test]
    fn inserting_in_nested_branches_keeps_ids_through_save_undo_and_redo() {
        let (directory, mut session) = session();
        let branch = StepDestination::Branch {
            parent_id: "group".into(),
            branch: StepBranch::Else,
        };
        let loop_id = session
            .insert_step(
                StepKind::While {
                    condition: crate::engine::Condition::Exists(Input::Literal(json!(true))),
                    steps: vec![],
                },
                branch,
            )
            .unwrap();
        let child_id = session
            .insert_step(
                StepKind::OneOrMore { steps: vec![] },
                StepDestination::Branch {
                    parent_id: loop_id.clone(),
                    branch: StepBranch::Body,
                },
            )
            .unwrap();
        let leaf_id = session
            .insert_step(
                StepKind::Delay { millis: 50 },
                StepDestination::Branch {
                    parent_id: child_id.clone(),
                    branch: StepBranch::Body,
                },
            )
            .unwrap();
        assert_eq!(session.draft().workflow.revision, 8);
        let inserted = serde_json::to_value(&session.draft().workflow.steps).unwrap();

        let repository = WorkflowRepository::at(directory.path());
        repository
            .save_definition(session.opened(), session.draft())
            .unwrap();
        session
            .accept_saved(repository.load("edit-me").unwrap())
            .unwrap();
        assert_eq!(session.draft().workflow.revision, 8);
        for _ in 0..3 {
            assert!(session.undo());
        }
        assert_eq!(session.draft().workflow.revision, 9);
        for _ in 0..3 {
            assert!(session.redo());
        }
        assert_eq!(
            serde_json::to_value(&session.draft().workflow.steps).unwrap(),
            inserted
        );
        assert_eq!(session.draft().workflow.revision, 8);
        assert_eq!(
            super::locate_step(&session.draft().workflow.steps, &loop_id)
                .unwrap()
                .path
                .len(),
            1
        );
        assert_eq!(
            super::locate_step(&session.draft().workflow.steps, &child_id)
                .unwrap()
                .path
                .len(),
            2
        );
        assert_eq!(
            super::locate_step(&session.draft().workflow.steps, &leaf_id)
                .unwrap()
                .path
                .len(),
            3
        );
    }

    #[test]
    fn insertion_rejects_missing_ambiguous_and_incompatible_destinations() {
        let (_directory, mut session) = session_with_steps(vec![delay("plain", 3)]);
        for (parent_id, branch, expected) in [
            (
                "missing",
                StepBranch::Then,
                EditError::MissingStep("missing".into()),
            ),
            (
                "plain",
                StepBranch::Else,
                EditError::InvalidStepBranch {
                    step_id: "plain".into(),
                    branch: StepBranch::Else,
                },
            ),
        ] {
            assert_eq!(
                session.insert_step(
                    StepKind::Stop,
                    StepDestination::Branch {
                        parent_id: parent_id.into(),
                        branch
                    },
                ),
                Err(expected)
            );
        }
        assert!(!session.is_dirty());
        assert!(!session.can_undo());

        session.draft.workflow.steps.push(delay("plain", 4));
        let before = serde_json::to_value(&session.draft().workflow.steps).unwrap();
        assert_eq!(
            session.insert_step(
                StepKind::Stop,
                StepDestination::Branch {
                    parent_id: "plain".into(),
                    branch: StepBranch::Body,
                },
            ),
            Err(EditError::AmbiguousStep("plain".into()))
        );
        assert_eq!(
            serde_json::to_value(&session.draft().workflow.steps).unwrap(),
            before
        );
        assert!(!session.can_undo());
    }

    #[test]
    fn insertion_rejects_duplicate_ids_in_supplied_subtrees() {
        let (_directory, mut session) = session();
        assert_eq!(
            session.insert_step(
                StepKind::OneOrMore {
                    steps: vec![delay("nested", 5)],
                },
                StepDestination::Root,
            ),
            Err(EditError::DuplicateStepId("nested".into()))
        );
        assert!(!session.is_dirty());
        assert!(!session.can_undo());
    }

    #[test]
    fn replacing_kind_keeps_step_identity_and_rejects_subtree_loss() {
        let (directory, mut session) = session();
        assert_eq!(
            session.set_step_kind("nested", StepKind::Delay { millis: 10 }),
            Ok(())
        );
        assert_eq!(session.draft().workflow.revision, 8);
        assert_eq!(session.draft().workflow.steps[0].id, "group");
        let StepKind::If { then_steps, .. } = &session.draft().workflow.steps[0].kind else {
            panic!("expected if step");
        };
        assert_eq!(then_steps[0].id, "nested");
        assert_eq!(then_steps[0].on_failure, FailurePolicy::Stop);
        assert_eq!(
            session.set_step_kind("nested", StepKind::Delay { millis: 10 }),
            Ok(())
        );
        assert_eq!(session.draft().workflow.revision, 8);
        assert_eq!(
            session.set_step_kind("group", StepKind::Stop),
            Err(EditError::DiscardedNestedStep {
                step_id: "group".into(),
                nested_id: "nested".into(),
            })
        );
        let mut modified = session.draft().workflow.steps[0].kind.clone();
        let StepKind::If { then_steps, .. } = &mut modified else {
            panic!("expected if step");
        };
        then_steps[0].kind = StepKind::Stop;
        assert_eq!(
            session.set_step_kind("group", modified),
            Err(EditError::ChangedNestedStep {
                step_id: "group".into(),
                nested_id: "nested".into(),
            })
        );
        assert_eq!(
            session.set_step_kind("absent", StepKind::Stop),
            Err(EditError::MissingStep("absent".into()))
        );
        assert_eq!(
            session.set_step_kind(
                "nested",
                StepKind::OneOrMore {
                    steps: vec![delay("nested", 5)],
                },
            ),
            Err(EditError::DuplicateStepId("nested".into()))
        );
        assert!(session.undo());
        assert_eq!(session.draft().workflow.revision, 7);
        assert!(session.redo());
        assert_eq!(session.draft().workflow.revision, 8);

        let repository = WorkflowRepository::at(directory.path());
        repository
            .save_definition(session.opened(), session.draft())
            .unwrap();
        session
            .accept_saved(repository.load("edit-me").unwrap())
            .unwrap();
        assert!(session.undo());
        assert_eq!(session.draft().workflow.revision, 9);
        assert!(session.redo());
        assert_eq!(session.draft().workflow.revision, 8);
    }

    #[test]
    fn append_action_rejects_missing_wrong_typed_and_unknown_inputs_without_editing() {
        let (_directory, mut session) = session();
        for inputs in [
            BTreeMap::new(),
            BTreeMap::from([("message".into(), Input::Literal(json!(42)))]),
            BTreeMap::from([
                ("message".into(), Input::Literal(json!("hello"))),
                ("unexpected".into(), Input::Literal(json!(true))),
            ]),
        ] {
            assert!(matches!(
                session.append_action(&ACTION_SCHEMA, inputs),
                Err(EditError::InvalidActionInputs { .. })
            ));
            assert_eq!(session.draft().workflow.steps.len(), 1);
            assert_eq!(session.draft().workflow.revision, 7);
            assert!(!session.is_dirty());
        }
    }

    #[test]
    fn trigger_insertion_and_removal_restore_order_identity_and_configuration() {
        let (_directory, mut session) = session();
        let manual = session.draft().triggers[0].id.clone();
        let recording = session
            .insert_trigger_at(
                TriggerKind::ObsRecordingStarted,
                TriggerPosition::Before {
                    trigger_id: manual.clone(),
                },
            )
            .unwrap();
        assert_eq!(session.draft().triggers[0].id, recording);
        assert!(session.draft().triggers[0].enabled);
        session.set_trigger_enabled(&recording, false).unwrap();
        let before = serde_json::to_value(session.draft()).unwrap();
        session.remove_trigger(&recording).unwrap();
        assert_eq!(session.draft().triggers.len(), 1);
        assert!(session.undo());
        assert_eq!(serde_json::to_value(session.draft()).unwrap(), before);
        assert!(session.redo());
        assert_eq!(session.draft().triggers[0].id, manual);
        session.remove_trigger(&manual).unwrap();
        assert!(session.draft().triggers.is_empty());
        assert!(session.undo());
        assert_eq!(session.draft().triggers[0].id, manual);
    }

    #[test]
    fn trigger_moves_support_both_directions_and_keep_each_drop_one_undo_entry() {
        let (_directory, mut session) = session();
        let manual = session.draft().triggers[0].id.clone();
        let recording = session
            .append_trigger(TriggerKind::ObsRecordingStarted)
            .unwrap();
        let scene = session
            .append_trigger(TriggerKind::ObsCurrentScene {
                scene: "Scene".into(),
            })
            .unwrap();
        let ids = |session: &WorkflowEditSession| {
            session
                .draft()
                .triggers
                .iter()
                .map(|item| item.id.clone())
                .collect::<Vec<_>>()
        };
        let original = ids(&session);
        for (id, position, expected) in [
            (
                manual.clone(),
                TriggerPosition::Append,
                vec![recording.clone(), scene.clone(), manual.clone()],
            ),
            (
                manual.clone(),
                TriggerPosition::Before {
                    trigger_id: scene.clone(),
                },
                vec![recording.clone(), manual.clone(), scene.clone()],
            ),
            (
                scene.clone(),
                TriggerPosition::Before {
                    trigger_id: manual.clone(),
                },
                vec![scene.clone(), manual.clone(), recording.clone()],
            ),
        ] {
            session.move_trigger_to(&id, position).unwrap();
            assert_eq!(ids(&session), expected);
            assert!(session.undo());
            assert_eq!(ids(&session), original);
            assert!(session.redo());
            assert_eq!(ids(&session), expected);
            assert!(session.undo());
        }
    }

    #[test]
    fn stale_trigger_targets_and_no_op_drops_preserve_drafts_and_redo() {
        let (_directory, mut session) = session();
        let manual = session.draft().triggers[0].id.clone();
        session
            .append_trigger(TriggerKind::ObsRecordingStarted)
            .unwrap();
        assert!(session.undo());
        let before = serde_json::to_value(session.draft()).unwrap();
        let cursor = session.cursor;
        for position in [
            TriggerPosition::Append,
            TriggerPosition::Before {
                trigger_id: manual.clone(),
            },
        ] {
            session.move_trigger_to(&manual, position).unwrap();
        }
        assert!(session.remove_trigger("missing").is_err());
        assert!(
            session
                .move_trigger_to(
                    &manual,
                    TriggerPosition::Before {
                        trigger_id: "missing".into()
                    }
                )
                .is_err()
        );
        assert!(
            session
                .insert_trigger_at(
                    TriggerKind::ObsRecordingStarted,
                    TriggerPosition::Before {
                        trigger_id: "missing".into()
                    }
                )
                .is_err()
        );
        assert_eq!(serde_json::to_value(session.draft()).unwrap(), before);
        assert_eq!(session.cursor, cursor);
        assert!(session.can_redo());
        assert!(session.redo());
        assert_eq!(session.draft().triggers.len(), 2);
    }

    #[test]
    fn unavailable_trigger_removal_is_recoverable() {
        let (_directory, mut session) = session();
        session.draft.triggers[0].enabled = false;
        session.draft.triggers[0].kind = TriggerKind::IntegrationEvent {
            integration: "unavailable".into(),
            event: "missing".into(),
            filters: BTreeMap::from([("keep".into(), json!(42))]),
        };
        let original = serde_json::to_value(&session.draft.triggers[0]).unwrap();
        let id = session.draft.triggers[0].id.clone();
        session.remove_trigger(&id).unwrap();
        assert!(session.undo());
        assert_eq!(
            serde_json::to_value(&session.draft.triggers[0]).unwrap(),
            original
        );
    }

    #[test]
    fn append_trigger_is_undoable_and_rejects_duplicate_or_unavailable_kinds() {
        use crate::workflows::TriggerKind;

        let (_directory, mut session) = session();
        assert_eq!(
            session.append_trigger(TriggerKind::Manual),
            Err(EditError::DuplicateTrigger)
        );
        assert_eq!(
            session.append_trigger(TriggerKind::ObsCurrentScene { scene: " ".into() }),
            Err(EditError::MissingScene)
        );
        assert_eq!(
            session.append_trigger(TriggerKind::IntegrationEvent {
                integration: "twitch".into(),
                event: "chat".into(),
                filters: BTreeMap::new()
            }),
            Err(EditError::UnavailableTrigger)
        );
        assert!(!session.is_dirty());

        let id = session
            .append_trigger(TriggerKind::ObsRecordingStarted)
            .unwrap();
        assert!(uuid::Uuid::parse_str(&id).is_ok());
        assert_eq!(session.draft().triggers.last().unwrap().id, id);
        assert!(session.draft().triggers.last().unwrap().enabled);
        assert_eq!(session.draft().workflow.revision, 8);
        assert_eq!(
            session.append_trigger(TriggerKind::ObsRecordingStarted),
            Err(EditError::DuplicateTrigger)
        );

        assert!(session.undo());
        assert_eq!(session.draft().triggers.len(), 1);
        assert_eq!(session.draft().workflow.revision, 7);
        assert!(session.redo());
        assert_eq!(session.draft().triggers.last().unwrap().id, id);

        let scene_id = session
            .append_trigger(TriggerKind::ObsCurrentScene {
                scene: "Just Chatting".into(),
            })
            .unwrap();
        assert_ne!(scene_id, id);
        assert_eq!(
            session.append_trigger(TriggerKind::ObsCurrentScene {
                scene: "Just Chatting".into()
            }),
            Err(EditError::DuplicateTrigger)
        );
    }

    #[test]
    fn appended_trigger_survives_save_and_undo_after_reopen() {
        use crate::workflows::TriggerKind;

        let (directory, mut session) = session();
        let id = session
            .append_trigger(TriggerKind::ObsCurrentScene {
                scene: "Starting Soon".into(),
            })
            .unwrap();
        let repository = WorkflowRepository::at(directory.path());
        repository
            .save_definition(session.opened(), session.draft())
            .unwrap();
        let saved = repository.load("edit-me").unwrap();
        assert_eq!(saved.workflow().revision, 8);
        assert!(saved.triggers().iter().any(|trigger| trigger.id == id && matches!(&trigger.kind, TriggerKind::ObsCurrentScene { scene } if scene == "Starting Soon")));

        session.accept_saved(saved).unwrap();
        assert!(session.undo());
        repository
            .save_definition(session.opened(), session.draft())
            .unwrap();
        let restored = repository.load("edit-me").unwrap();
        assert_eq!(restored.workflow().revision, 9);
        assert_eq!(restored.triggers().len(), 1);
        assert!(matches!(restored.triggers()[0].kind, TriggerKind::Manual));
    }
}
