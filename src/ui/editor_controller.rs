use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::app::AppServices;
use crate::editor::{StepBranch, StepDestination, StepMoveDirection, WorkflowEditSession};
use crate::engine::{Input, Step, StepKind, validate_configured_inputs};
use crate::runtime::RuntimeSpawner;
use crate::schema::{ConfigChoiceSource, ConfigFieldKind, ConfigSchema};
use crate::storage::SaveWarning;
use crate::value_sources::{ValueSource, available_sources};
use crate::workflows::{EditableWorkflow, TriggerKind, WorkflowDefinition, WorkflowRepository};

use super::control_flow::{
    ConditionDraft, ConditionOperator, ControlDraft, ControlKind, LiteralType, TypedInput,
};
use super::{
    ActionChoice, ActionDraftField, AppWindow, ControlChild, ControlChoice, ControlField,
    EditorFieldKind, ResourceChoice, StepIcon, TriggerChoice, ValueChoice, WorkflowPreview,
};

#[derive(Default)]
struct EditorState {
    request: u64,
    choice_request: u64,
    active_choice: Option<ChoiceContext>,
    session: Option<WorkflowEditSession>,
    pending: bool,
    reopen_required: bool,
    selected_step: String,
    selected_action: Option<&'static ConfigSchema>,
    action_destination: Option<StepDestination>,
    action_saved_step: Option<String>,
    control: Option<ControlDraft>,
    control_frames: Vec<ControlFrame>,
    control_target: Option<String>,
    control_saved_step: Option<String>,
    staged_child_branch: Option<String>,
    value_picker: Option<ValuePicker>,
    literal_overrides: HashSet<(String, String)>,
    selected_trigger: Option<&'static str>,
    inspector_drafts: HashMap<String, (String, String)>,
    workflow_titles: BTreeMap<String, String>,
}

#[derive(Clone)]
struct ControlFrame {
    draft: ControlDraft,
    target: Option<String>,
    destination: Option<StepDestination>,
    branch: String,
}

struct ValuePicker {
    session_request: u64,
    workflow_id: String,
    target: ValueTarget,
    sources: Vec<ValueSource>,
    selected: Option<usize>,
    fallback_touched: bool,
    expected_kind: Option<&'static str>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ValueTarget {
    Control {
        control_target: Option<String>,
        slot: String,
    },
    SavedAction {
        step_id: String,
        field_id: String,
    },
}

impl ValueTarget {
    fn slot(&self) -> &str {
        match self {
            Self::Control { slot, .. } => slot,
            Self::SavedAction { field_id, .. } => field_id,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ChoiceContext {
    session_request: u64,
    request: u64,
    workflow_id: String,
    step_id: String,
    field_id: String,
    title: String,
    new_action: bool,
    source: ConfigChoiceSource,
    dependency: Option<String>,
    values: Vec<String>,
}

#[derive(Clone)]
struct PersistenceContext {
    services: Arc<AppServices>,
    repository: WorkflowRepository,
    spawner: RuntimeSpawner,
}

/// Connects the workflow editor UI to persisted workflow snapshots and app services.
/// All disk and service waits run on the application runtime, outside Slint callbacks.
pub fn connect_editor(
    window: &AppWindow,
    services: Arc<AppServices>,
    repository: WorkflowRepository,
    spawner: RuntimeSpawner,
) {
    let state = Arc::new(Mutex::new(EditorState::default()));

    window.set_editor_control_input_types(ModelRc::new(VecModel::from(type_choices())));

    let weak = window.as_weak();
    let begin_control_state = Arc::clone(&state);
    let control_schemas = services.action_schemas().to_vec();
    window.on_begin_editor_control(move |kind| {
        let Some(window) = weak.upgrade() else { return };
        let Some(kind) = ControlKind::from_id(&kind) else {
            return;
        };
        let mut guard = begin_control_state
            .lock()
            .expect("editor state lock poisoned");
        if !can_begin_control(&window, &guard) || !window.get_editor_can_compose() {
            return;
        }
        let draft = if guard.staged_child_branch.is_some() {
            match begin_nested_control(&mut guard, kind) {
                Ok(draft) => draft,
                Err(error) => {
                    window.set_editor_action_error(error.into());
                    return;
                }
            }
        } else {
            if guard.control.is_some() || !guard.control_frames.is_empty() {
                window.set_editor_action_error("Finish the current control first".into());
                return;
            }
            let draft = ControlDraft::new(kind);
            guard.control = Some(draft.clone());
            guard.control_target = None;
            if guard.action_destination.is_none() {
                window.set_editor_control_insert_parent("".into());
                window.set_editor_control_insert_branch("".into());
            }
            draft
        };
        guard.selected_action = None;
        window.set_editor_action_mode(0);
        if !guard.control_frames.is_empty() {
            set_insert_destination(&window, None);
        }
        publish_control_parent_title(&window, &guard);
        publish_control(&window, &draft, None, &control_schemas);
    });

    let weak = window.as_weak();
    let edit_control_state = Arc::clone(&state);
    let control_schemas = services.action_schemas().to_vec();
    window.on_edit_editor_control(move |selected| {
        let Some(window) = weak.upgrade() else { return };
        let mut guard = edit_control_state
            .lock()
            .expect("editor state lock poisoned");
        if !can_begin_control(&window, &guard)
            || !window.get_editor_can_edit_fields()
            || !window.get_editor_active_field().is_empty()
            || window.get_editor_rename_active()
            || guard.control.is_some()
            || !guard.control_frames.is_empty()
        {
            return;
        }
        let Some(session) = guard.session.as_ref() else {
            return;
        };
        let Some(id) = configured_action_id(&selected) else {
            return;
        };
        let Some(kind) = find_step_kind(&session.draft().workflow.steps, id) else {
            window.set_error_message("This control step is unavailable".into());
            return;
        };
        let Some(draft) = ControlDraft::from_step(kind) else {
            window.set_error_message("This step is not a control step".into());
            return;
        };
        guard.control = Some(draft.clone());
        guard.control_target = Some(selected.to_string());
        guard.action_destination = None;
        guard.staged_child_branch = None;
        set_insert_destination(&window, None);
        publish_control_parent_title(&window, &guard);
        publish_control(&window, &draft, Some(&selected), &control_schemas);
    });

    let weak = window.as_weak();
    let update_control_state = Arc::clone(&state);
    let control_schemas = services.action_schemas().to_vec();
    window.on_update_editor_control_field(move |id, value| {
        if let Some(window) = weak.upgrade() {
            update_control_draft(
                &window,
                &update_control_state,
                &control_schemas,
                &id,
                value.to_string(),
            );
        }
    });

    let weak = window.as_weak();
    let type_control_state = Arc::clone(&state);
    let control_schemas = services.action_schemas().to_vec();
    window.on_set_editor_control_input_type(move |slot, kind| {
        if let Some(window) = weak.upgrade() {
            update_control_draft(
                &window,
                &type_control_state,
                &control_schemas,
                &format!("{slot}_type"),
                kind.to_string(),
            );
        }
    });

    let weak = window.as_weak();
    let value_state = Arc::clone(&state);
    let value_schemas = services.action_schemas().to_vec();
    let value_services = Arc::clone(&services);
    window.on_request_editor_control_values(move |slot| {
        let Some(window) = weak.upgrade() else { return };
        let mut guard = value_state.lock().expect("editor state lock poisoned");
        if !can_edit_control_input(&window, &guard, &slot) {
            return;
        }
        let Some(session) = guard.session.as_ref() else {
            return;
        };
        let (definition, target) = match scoped_control_definition(
            session,
            guard.control_target.as_deref().or_else(|| {
                guard
                    .control_frames
                    .iter()
                    .rev()
                    .find_map(|frame| frame.target.as_deref())
            }),
            guard.action_destination.as_ref(),
        ) {
            Ok(scoped) => scoped,
            Err(error) => {
                window.set_editor_control_error(error.into());
                return;
            }
        };
        let sources =
            match available_sources(&definition, target.as_deref(), &value_schemas, |kind| {
                value_services.trigger_value_schema(kind)
            }) {
                Ok(sources) => sources,
                Err(error) => {
                    window.set_editor_control_error(error.into());
                    return;
                }
            };
        let rows = sources.iter().map(value_choice).collect::<Vec<_>>();
        guard.value_picker = Some(ValuePicker {
            session_request: guard.request,
            workflow_id: session.opened().workflow().id.clone(),
            target: ValueTarget::Control {
                control_target: guard.control_target.clone(),
                slot: slot.to_string(),
            },
            sources,
            selected: None,
            fallback_touched: false,
            expected_kind: None,
        });
        window.set_editor_value_slot(slot);
        window.set_editor_value_choices(ModelRc::new(VecModel::from(rows)));
        window.set_editor_value_selected(-1);
        window.set_editor_value_fallback_kind("".into());
        window.set_editor_value_fallback_value("".into());
        window.set_editor_value_error("".into());
        window.set_editor_value_visible(true);
    });

    let weak = window.as_weak();
    let value_state = Arc::clone(&state);
    let value_schemas = services.action_schemas().to_vec();
    let value_services = Arc::clone(&services);
    window.on_request_editor_action_values(move |field_id| {
        let Some(window) = weak.upgrade() else { return };
        let mut guard = value_state.lock().expect("editor state lock poisoned");
        let Some((step_id, workflow_id)) =
            saved_action_field_context(&window, &guard, &field_id, &value_schemas)
        else {
            return;
        };
        let Some(session) = guard.session.as_ref() else {
            return;
        };
        let selected = format!("step:{step_id}");
        let fields = WorkflowPreview::fields_for_step(session.draft(), &selected, &value_schemas);
        let Some(field) = fields.iter().find(|field| field.id == field_id) else {
            return;
        };
        let sources =
            match available_sources(session.draft(), Some(&step_id), &value_schemas, |kind| {
                value_services.trigger_value_schema(kind)
            }) {
                Ok(sources) => sources
                    .into_iter()
                    .filter(|source| source_matches_field(source, field.kind))
                    .collect::<Vec<_>>(),
                Err(error) => {
                    window.set_editor_value_error(error.into());
                    return;
                }
            };
        let rows = sources.iter().map(value_choice).collect::<Vec<_>>();
        guard.value_picker = Some(ValuePicker {
            session_request: guard.request,
            workflow_id,
            target: ValueTarget::SavedAction {
                step_id: step_id.clone(),
                field_id: field_id.to_string(),
            },
            sources,
            selected: None,
            fallback_touched: false,
            expected_kind: Some(editor_kind_id(field.kind)),
        });
        window.set_editor_value_slot(field_id);
        window.set_editor_value_choices(ModelRc::new(VecModel::from(rows)));
        window.set_editor_value_selected(-1);
        window.set_editor_value_fallback_kind("".into());
        window.set_editor_value_fallback_value("".into());
        window.set_editor_value_error("".into());
        window.set_editor_value_visible(true);
    });

    let weak = window.as_weak();
    let value_state = Arc::clone(&state);
    window.on_select_editor_control_value(move |index| {
        let Some(window) = weak.upgrade() else { return };
        let mut guard = value_state.lock().expect("editor state lock poisoned");
        let Some(picker) = guard.value_picker.as_mut() else {
            return;
        };
        if !window.get_editor_value_visible() {
            return;
        }
        let Ok(index) = usize::try_from(index) else {
            return;
        };
        let Some(source) = picker.sources.get(index) else {
            return;
        };
        picker.selected = Some(index);
        picker.fallback_touched = false;
        window.set_editor_value_selected(index as i32);
        let fallback_kind = if source.fallback_kind.is_empty() {
            picker.expected_kind.unwrap_or("")
        } else {
            source.fallback_kind
        };
        window.set_editor_value_fallback_kind(fallback_kind.into());
        window.set_editor_value_fallback_value("".into());
        window.set_editor_value_error("".into());
    });

    let weak = window.as_weak();
    let value_state = Arc::clone(&state);
    window.on_set_editor_value_fallback(move |kind, value| {
        let Some(window) = weak.upgrade() else { return };
        let mut guard = value_state.lock().expect("editor state lock poisoned");
        let Some(picker) = guard.value_picker.as_mut() else {
            return;
        };
        if !window.get_editor_value_visible() {
            return;
        }
        if picker.selected.is_none() {
            return;
        }
        if !matches!(kind.as_str(), "text" | "number" | "toggle") {
            return;
        }
        picker.fallback_touched = true;
        window.set_editor_value_fallback_kind(kind);
        window.set_editor_value_fallback_value(value);
        window.set_editor_value_error("".into());
    });

    let weak = window.as_weak();
    let value_state = Arc::clone(&state);
    let value_schemas = services.action_schemas().to_vec();
    let value_context = PersistenceContext {
        services: Arc::clone(&services),
        repository: repository.clone(),
        spawner: spawner.clone(),
    };
    window.on_confirm_editor_control_value(move || {
        let Some(window) = weak.upgrade() else { return };
        let candidate = {
            let mut guard = value_state.lock().expect("editor state lock poisoned");
            let Some(picker) = guard.value_picker.as_ref() else {
                return;
            };
            if !value_picker_matches(&window, &guard, picker) {
                return;
            }
            let Some(source) = picker.selected.and_then(|index| picker.sources.get(index)) else {
                return;
            };
            let fallback = if source.optional {
                if !picker.fallback_touched {
                    window.set_editor_value_error("Choose a fallback for this value".into());
                    return;
                }
                let required_kind = if source.fallback_kind.is_empty() {
                    picker.expected_kind
                } else {
                    Some(source.fallback_kind)
                };
                if required_kind.is_some_and(|kind| window.get_editor_value_fallback_kind() != kind)
                {
                    window.set_editor_value_error(
                        "Use a fallback with the same type as this value".into(),
                    );
                    return;
                }
                let mut typed = TypedInput::new();
                let result = typed
                    .set_type(&window.get_editor_value_fallback_kind())
                    .and_then(|()| {
                        typed.set_value(window.get_editor_value_fallback_value().to_string())
                    })
                    .and_then(|()| typed.build());
                match result {
                    Ok(value) => Some(value),
                    Err(error) => {
                        window.set_editor_value_error(error.into());
                        return;
                    }
                }
            } else {
                None
            };
            let input = match source.input(fallback) {
                Ok(input) => input,
                Err(error) => {
                    window.set_editor_value_error(error.into());
                    return;
                }
            };
            let target = picker.target.clone();
            match target {
                ValueTarget::Control {
                    control_target,
                    slot,
                } => {
                    let Some(draft) = guard.control.as_mut() else {
                        return;
                    };
                    if let Err(error) = draft.set_input(&slot, input) {
                        window.set_editor_value_error(error.into());
                        return;
                    }
                    let draft = draft.clone();
                    clear_value_picker(&window, &mut guard);
                    publish_control(&window, &draft, control_target.as_deref(), &value_schemas);
                    return;
                }
                ValueTarget::SavedAction { step_id, field_id } => {
                    let Some(session) = guard.session.as_ref() else {
                        return;
                    };
                    let mut candidate = session.clone();
                    let selected = format!("step:{step_id}");
                    let fields = WorkflowPreview::fields_for_step(
                        candidate.draft(),
                        &selected,
                        &value_schemas,
                    );
                    let Some(field) = fields.iter().find(|field| {
                        field.id == field_id && field.kind != EditorFieldKind::Secret
                    }) else {
                        window.set_editor_value_error("This action field is unavailable".into());
                        return;
                    };
                    let result = if field.configured {
                        candidate.set_action_input(&step_id, &field_id, input)
                    } else {
                        candidate.add_action_input(&step_id, &field_id, input)
                    };
                    if let Err(error) = result {
                        window.set_editor_value_error(error.to_string().into());
                        return;
                    }
                    clear_value_picker(&window, &mut guard);
                    if !candidate.is_dirty() {
                        return;
                    }
                    candidate
                }
            }
        };
        persist_candidate(
            &window,
            Arc::clone(&value_state),
            value_context.clone(),
            candidate,
            SaveIntent::Other,
            "save workflow field",
        );
    });

    let weak = window.as_weak();
    let value_state = Arc::clone(&state);
    window.on_cancel_editor_control_values(move || {
        let Some(window) = weak.upgrade() else { return };
        let mut guard = value_state.lock().expect("editor state lock poisoned");
        clear_value_picker(&window, &mut guard);
    });

    let weak = window.as_weak();
    let value_state = Arc::clone(&state);
    let value_schemas = services.action_schemas().to_vec();
    window.on_request_editor_control_literal(move |slot| {
        let Some(window) = weak.upgrade() else { return };
        let mut guard = value_state.lock().expect("editor state lock poisoned");
        if !can_edit_control_input(&window, &guard, &slot) {
            return;
        }
        let target = guard.control_target.clone();
        let Some(draft) = guard.control.as_mut() else {
            return;
        };
        if let Err(error) = draft.set_input(&slot, Input::Literal(Value::String(String::new()))) {
            window.set_editor_control_error(error.into());
            return;
        }
        let draft = draft.clone();
        publish_control(&window, &draft, target.as_deref(), &value_schemas);
    });

    let weak = window.as_weak();
    let literal_state = Arc::clone(&state);
    let literal_schemas = services.action_schemas().to_vec();
    window.on_request_editor_action_literal(move |field_id| {
        let Some(window) = weak.upgrade() else { return };
        let mut guard = literal_state.lock().expect("editor state lock poisoned");
        let Some((step_id, _)) =
            saved_action_field_context(&window, &guard, &field_id, &literal_schemas)
        else {
            return;
        };
        let selected = format!("step:{step_id}");
        let Some(session) = guard.session.as_ref() else {
            return;
        };
        let fields = WorkflowPreview::fields_for_step(session.draft(), &selected, &literal_schemas);
        let Some(field) = fields
            .iter()
            .find(|field| field.id == field_id && field.is_output)
        else {
            return;
        };
        let value = match field.kind {
            EditorFieldKind::Integer => "0",
            EditorFieldKind::Toggle => "false",
            _ => "",
        };
        guard
            .literal_overrides
            .insert((selected, field_id.to_string()));
        window.set_editor_active_field(field_id);
        window.set_editor_draft_value(value.into());
        window.set_editor_edit_error("".into());
    });

    let weak = window.as_weak();
    let cancel_control_state = Arc::clone(&state);
    let cancel_control_schemas = services.action_schemas().to_vec();
    window.on_cancel_editor_control(move || {
        let Some(window) = weak.upgrade() else { return };
        let mut guard = cancel_control_state
            .lock()
            .expect("editor state lock poisoned");
        if !guard.pending && !window.get_editor_edit_pending() {
            if let Some(parent) = cancel_nested_control(&mut guard) {
                let target = guard.control_target.clone();
                set_insert_destination(&window, guard.action_destination.as_ref());
                publish_control_parent_title(&window, &guard);
                publish_control(&window, &parent, target.as_deref(), &cancel_control_schemas);
                preview_control_steps(&window, &guard, &cancel_control_schemas);
            } else {
                clear_control(&window, &mut guard);
                restore_saved_steps(&window, &guard, &cancel_control_schemas);
            }
        }
    });

    let weak = window.as_weak();
    let save_control_state = Arc::clone(&state);
    let save_control_context = PersistenceContext {
        services: Arc::clone(&services),
        repository: repository.clone(),
        spawner: spawner.clone(),
    };
    window.on_save_editor_control(move || {
        let Some(window) = weak.upgrade() else { return };
        let candidate = {
            let mut guard = save_control_state
                .lock()
                .expect("editor state lock poisoned");
            if guard.pending
                || guard.reopen_required
                || window.get_editor_control_mode() != 1
                || window.get_editor_choice_visible()
                || window.get_editor_value_visible()
                || window.get_editor_action_mode() != 0
                || (guard.control_target.is_some() && !window.get_editor_can_edit_fields())
                || (guard.control_target.is_none() && !window.get_editor_can_compose())
            {
                return;
            }
            let (Some(draft), Some(session)) = (guard.control.as_ref(), guard.session.as_ref())
            else {
                return;
            };
            if window.get_selected_automation() != session.opened().workflow().id {
                return;
            }
            let kind = match draft.build() {
                Ok(kind) => kind,
                Err(error) => {
                    window.set_editor_control_error(error.into());
                    return;
                }
            };
            if !guard.control_frames.is_empty() {
                let parent = match stage_nested_control(&mut guard, kind) {
                    Ok(parent) => parent,
                    Err(error) => {
                        window.set_editor_control_error(error.into());
                        return;
                    }
                };
                let target = guard.control_target.clone();
                set_insert_destination(&window, guard.action_destination.as_ref());
                publish_control_parent_title(&window, &guard);
                publish_control(
                    &window,
                    &parent,
                    target.as_deref(),
                    save_control_context.services.action_schemas(),
                );
                preview_control_steps(
                    &window,
                    &guard,
                    save_control_context.services.action_schemas(),
                );
                return;
            }
            let mut candidate = session.clone();
            let result = if let Some(selected) = guard.control_target.as_ref() {
                match configured_action_id(selected) {
                    Some(id) => candidate.set_step_kind(id, kind).map(|()| None),
                    None => {
                        window.set_editor_control_error("Select a control step".into());
                        return;
                    }
                }
            } else {
                let destination = guard
                    .action_destination
                    .clone()
                    .unwrap_or(StepDestination::Root);
                candidate.insert_step(kind, destination).map(Some)
            };
            match result {
                Ok(new_id) => guard.control_saved_step = new_id,
                Err(error) => {
                    window.set_editor_control_error(error.to_string().into());
                    return;
                }
            }
            if !candidate.is_dirty() {
                clear_control(&window, &mut guard);
                restore_saved_steps(
                    &window,
                    &guard,
                    save_control_context.services.action_schemas(),
                );
                return;
            }
            candidate
        };
        persist_candidate(
            &window,
            Arc::clone(&save_control_state),
            save_control_context.clone(),
            candidate,
            SaveIntent::ControlSaved,
            "save control step",
        );
    });

    let weak = window.as_weak();
    let begin_child_state = Arc::clone(&state);
    window.on_begin_editor_control_child(move |branch| {
        let Some(window) = weak.upgrade() else { return };
        let mut guard = begin_child_state
            .lock()
            .expect("editor state lock poisoned");
        if guard.pending
            || guard.reopen_required
            || window.get_editor_control_mode() != 1
            || window.get_editor_action_mode() != 0
            || !window.get_editor_can_compose()
        {
            return;
        }
        let Some(draft) = guard.control.as_ref() else {
            return;
        };
        if !control_accepts_branch(draft.kind, &branch) {
            return;
        }
        guard.staged_child_branch = Some(branch.to_string());
        guard.selected_action = None;
        window.set_editor_control_insert_parent("".into());
        window.set_editor_control_insert_branch(branch);
        window.set_editor_control_mode(0);
        window.set_editor_action_mode(1);
        window.set_editor_sidepanel_tab(0);
        window.set_editor_action_error("".into());
    });

    let weak = window.as_weak();
    let remove_child_state = Arc::clone(&state);
    let control_schemas = services.action_schemas().to_vec();
    window.on_remove_editor_control_child(move |id| {
        let Some(window) = weak.upgrade() else { return };
        let mut guard = remove_child_state
            .lock()
            .expect("editor state lock poisoned");
        if guard.pending || guard.reopen_required || window.get_editor_control_mode() != 1 {
            return;
        }
        let target = guard.control_target.clone();
        let Some(draft) = guard.control.as_mut() else {
            return;
        };
        let child_id = id.strip_prefix("step:").unwrap_or(&id);
        if draft.remove_staged_child(child_id) {
            publish_control(&window, draft, target.as_deref(), &control_schemas);
            preview_control_steps(&window, &guard, &control_schemas);
        }
    });

    let weak = window.as_weak();
    let add_child_state = Arc::clone(&state);
    window.on_add_editor_child(move |parent, branch| {
        let Some(window) = weak.upgrade() else { return };
        let mut guard = add_child_state.lock().expect("editor state lock poisoned");
        if !can_begin_control(&window, &guard)
            || !window.get_editor_can_compose()
            || window.get_editor_action_mode() != 0
            || window.get_editor_control_mode() != 0
            || guard.control.is_some()
            || !guard.control_frames.is_empty()
        {
            return;
        }
        let Some(session) = guard.session.as_ref() else {
            return;
        };
        let destination = match step_destination(&parent, &branch, &session.draft().workflow.steps)
        {
            Ok(destination) => destination,
            Err(error) => {
                window.set_error_message(error.into());
                return;
            }
        };
        guard.action_destination = Some(destination);
        guard.staged_child_branch = None;
        guard.selected_action = None;
        window.set_editor_control_insert_parent(parent);
        window.set_editor_control_insert_branch(branch);
        window.set_editor_action_mode(1);
        window.set_editor_sidepanel_tab(0);
    });

    let weak = window.as_weak();
    let choice_state = Arc::clone(&state);
    let choice_services = Arc::clone(&services);
    let choice_spawner = spawner.clone();
    window.on_request_editor_choices(move |field_id, new_action| {
        let Some(window) = weak.upgrade() else { return };
        let field_id = field_id.to_string();
        let request = {
            let mut guard = choice_state.lock().expect("editor state lock poisoned");
            invalidate_choices(&window, &mut guard);
            let result = choice_context(
                &window,
                &guard,
                choice_services.action_schemas(),
                &field_id,
                new_action,
            );
            match result {
                Ok(mut context) => {
                    context.request = guard.choice_request;
                    guard.active_choice = Some(context.clone());
                    Ok(context)
                }
                Err(error) => Err(error),
            }
        };
        window.set_editor_choice_visible(true);
        window.set_editor_choice_field(field_id.clone().into());
        window.set_editor_choice_new_action(new_action);
        window.set_editor_choice_title(format!("Choose {field_id}").into());
        window.set_editor_choices(ModelRc::new(VecModel::from(Vec::new())));
        let context = match request {
            Ok(context) => context,
            Err(error) => {
                window.set_editor_choice_loading(false);
                window.set_editor_choice_error(error.into());
                return;
            }
        };
        window.set_editor_choice_title(format!("Choose {}", context.title).into());
        window.set_editor_choice_error("".into());
        window.set_editor_choice_loading(true);

        let services = Arc::clone(&choice_services);
        let state = Arc::clone(&choice_state);
        let result_window = weak.clone();
        let task_context = context.clone();
        if let Err(error) = choice_spawner.spawn_task("load action choices", move |_| async move {
            let result = services
                .choices(task_context.source.key, task_context.dependency.as_deref())
                .await;
            let _ = result_window.upgrade_in_event_loop(move |window| {
                let mut guard = state.lock().expect("editor state lock poisoned");
                if !choice_is_current(&window, &guard, &task_context) {
                    return;
                }
                window.set_editor_choice_loading(false);
                match result {
                    Ok(choices) => {
                        if let Some(active) = guard.active_choice.as_mut() {
                            active.values =
                                choices.iter().map(|choice| choice.value.clone()).collect();
                        }
                        window.set_editor_choices(ModelRc::new(VecModel::from(
                            choices
                                .into_iter()
                                .map(|choice| ResourceChoice {
                                    value: choice.value.into(),
                                    label: choice.label.into(),
                                    detail: choice.detail.unwrap_or_default().into(),
                                })
                                .collect::<Vec<_>>(),
                        )));
                    }
                    Err(error) => window.set_editor_choice_error(error.into()),
                }
            });
            Ok::<(), std::convert::Infallible>(())
        }) {
            let mut guard = choice_state.lock().expect("editor state lock poisoned");
            invalidate_choices(&window, &mut guard);
            window.set_editor_choice_visible(true);
            window.set_editor_choice_loading(false);
            window.set_editor_choice_error(format!("Could not load choices: {error}").into());
        }
    });

    let weak = window.as_weak();
    let choose_state = Arc::clone(&state);
    window.on_choose_editor_choice(move |value| {
        let Some(window) = weak.upgrade() else { return };
        let context = {
            let mut guard = choose_state.lock().expect("editor state lock poisoned");
            let Some(context) = guard.active_choice.clone() else {
                return;
            };
            if window.get_editor_choice_loading()
                || !choice_contains_value(&context, value.as_str())
                || !choice_is_current(&window, &guard, &context)
            {
                return;
            }
            invalidate_choices(&window, &mut guard);
            context
        };
        if context.new_action {
            let model = window.get_editor_action_fields();
            let Some((index, mut field)) = model
                .iter()
                .enumerate()
                .find(|(_, field)| field.id == context.field_id)
            else {
                return;
            };
            field.value = value;
            field.configured = true;
            model.set_row_data(index, field);
        } else {
            window.set_editor_active_field(context.field_id.into());
            window.set_editor_draft_value(value);
            window.set_editor_edit_error("".into());
        }
    });

    let weak = window.as_weak();
    let cancel_choice_state = Arc::clone(&state);
    window.on_cancel_editor_choices(move || {
        if let Some(window) = weak.upgrade() {
            let mut guard = cancel_choice_state
                .lock()
                .expect("editor state lock poisoned");
            invalidate_choices(&window, &mut guard);
        }
    });

    let weak = window.as_weak();
    let trigger_state = Arc::clone(&state);
    let trigger_context = PersistenceContext {
        services: Arc::clone(&services),
        repository: repository.clone(),
        spawner: spawner.clone(),
    };
    window.on_set_editor_trigger_enabled(move |selected, enabled| {
        let Some(window) = weak.upgrade() else {
            return;
        };
        let Some(id) = selected.strip_prefix("trigger:") else {
            return;
        };
        let candidate = {
            let guard = trigger_state.lock().expect("editor state lock poisoned");
            if guard.pending || guard.reopen_required || guard.selected_step != selected.as_str() {
                return;
            }
            let Some(session) = guard.session.as_ref() else {
                return;
            };
            let mut candidate = session.clone();
            if let Err(error) = candidate.set_trigger_enabled(id, enabled) {
                window.set_error_message(error.to_string().into());
                return;
            }
            candidate
        };
        persist_candidate(
            &window,
            Arc::clone(&trigger_state),
            trigger_context.clone(),
            candidate,
            SaveIntent::Other,
            "change trigger activation",
        );
    });

    let weak = window.as_weak();
    let enabled_state = Arc::clone(&state);
    let enabled_context = PersistenceContext {
        services: Arc::clone(&services),
        repository: repository.clone(),
        spawner: spawner.clone(),
    };
    window.on_set_editor_workflow_enabled(move |enabled| {
        let Some(window) = weak.upgrade() else {
            return;
        };
        let candidate = {
            let guard = enabled_state.lock().expect("editor state lock poisoned");
            if guard.pending || guard.reopen_required {
                return;
            }
            let Some(session) = guard.session.as_ref() else {
                return;
            };
            let mut candidate = session.clone();
            if let Err(error) = candidate.set_enabled(enabled) {
                window.set_error_message(error.to_string().into());
                return;
            }
            candidate
        };
        persist_candidate(
            &window,
            Arc::clone(&enabled_state),
            enabled_context.clone(),
            candidate,
            SaveIntent::Other,
            "change workflow activation",
        );
    });

    let weak = window.as_weak();
    let edit_trigger_state = Arc::clone(&state);
    window.on_edit_editor_trigger(move |selected| {
        let Some(window) = weak.upgrade() else { return };
        let Some(id) = selected.strip_prefix("trigger:") else {
            return;
        };
        let guard = edit_trigger_state
            .lock()
            .expect("editor state lock poisoned");
        if guard.pending || guard.reopen_required || guard.selected_step != selected.as_str() {
            return;
        }
        let Some(trigger) = guard.session.as_ref().and_then(|session| {
            session
                .draft()
                .triggers
                .iter()
                .find(|trigger| trigger.id == id)
        }) else {
            return;
        };
        match &trigger.kind {
            TriggerKind::ObsCurrentScene { scene } => {
                window.set_editor_trigger_edit_kind("obs.current_scene".into());
                window.set_editor_trigger_edit_scene(scene.clone().into());
            }
            TriggerKind::IntegrationEvent {
                integration,
                event,
                filters,
            } if integration == "twitch" && event == "chat.command" => {
                window.set_editor_trigger_edit_kind("twitch.chat.command".into());
                window.set_editor_trigger_edit_command(
                    filters
                        .get("command")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into(),
                );
                window.set_editor_trigger_edit_aliases(
                    filters
                        .get("aliases")
                        .and_then(Value::as_array)
                        .map(|aliases| {
                            aliases
                                .iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                        .unwrap_or_default()
                        .into(),
                );
                window.set_editor_trigger_edit_moderator(
                    filters
                        .get("broadcaster_or_moderator")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                );
            }
            _ => return,
        }
        window.set_editor_trigger_error("".into());
        window.set_editor_trigger_editing(true);
    });

    let weak = window.as_weak();
    window.on_cancel_editor_trigger_edit(move || {
        if let Some(window) = weak.upgrade()
            && !window.get_editor_trigger_pending()
        {
            window.set_editor_trigger_editing(false);
            window.set_editor_trigger_error("".into());
        }
    });

    let weak = window.as_weak();
    let save_trigger_state = Arc::clone(&state);
    let save_trigger_context = PersistenceContext {
        services: Arc::clone(&services),
        repository: repository.clone(),
        spawner: spawner.clone(),
    };
    window.on_save_editor_trigger_edit(move || {
        let Some(window) = weak.upgrade() else { return };
        if !window.get_editor_trigger_editing() || window.get_editor_trigger_pending() {
            return;
        }
        let selected = window.get_editor_selected_step().to_string();
        let Some(id) = selected.strip_prefix("trigger:") else {
            return;
        };
        let candidate = {
            let guard = save_trigger_state
                .lock()
                .expect("editor state lock poisoned");
            if guard.pending || guard.reopen_required || guard.selected_step != selected {
                return;
            }
            let Some(session) = guard.session.as_ref() else {
                return;
            };
            let Some(trigger) = session
                .draft()
                .triggers
                .iter()
                .find(|trigger| trigger.id == id)
            else {
                return;
            };
            let kind = match &trigger.kind {
                TriggerKind::ObsCurrentScene { .. }
                    if window.get_editor_trigger_edit_kind() == "obs.current_scene" =>
                {
                    TriggerKind::ObsCurrentScene {
                        scene: window.get_editor_trigger_edit_scene().to_string(),
                    }
                }
                TriggerKind::IntegrationEvent {
                    integration,
                    event,
                    filters,
                } if integration == "twitch"
                    && event == "chat.command"
                    && window.get_editor_trigger_edit_kind() == "twitch.chat.command" =>
                {
                    let mut filters = filters.clone();
                    filters.insert(
                        "command".into(),
                        Value::String(window.get_editor_trigger_edit_command().to_string()),
                    );
                    let aliases: Vec<_> = window
                        .get_editor_trigger_edit_aliases()
                        .split(',')
                        .map(str::trim)
                        .filter(|alias| !alias.is_empty())
                        .map(|alias| Value::String(alias.to_owned()))
                        .collect();
                    if !aliases.is_empty() || filters.contains_key("aliases") {
                        filters.insert("aliases".into(), Value::Array(aliases));
                    }
                    let moderator = window.get_editor_trigger_edit_moderator();
                    if moderator || filters.contains_key("broadcaster_or_moderator") {
                        filters.insert("broadcaster_or_moderator".into(), Value::Bool(moderator));
                    }
                    TriggerKind::IntegrationEvent {
                        integration: integration.clone(),
                        event: event.clone(),
                        filters,
                    }
                }
                _ => return,
            };
            let mut candidate = session.clone();
            if let Err(error) = candidate.set_trigger_kind(id, kind) {
                window.set_editor_trigger_error(error.to_string().into());
                return;
            }
            candidate
        };
        if !candidate.is_dirty() {
            window.set_editor_trigger_editing(false);
            return;
        }
        persist_candidate(
            &window,
            Arc::clone(&save_trigger_state),
            save_trigger_context.clone(),
            candidate,
            SaveIntent::TriggerEdited,
            "edit workflow trigger",
        );
    });

    let weak = window.as_weak();
    let open_state = Arc::clone(&state);
    let open_services = Arc::clone(&services);
    let open_spawner = spawner.clone();
    let open_repository = repository.clone();
    window.on_open_automation(move |id| {
        let Some(window) = weak.upgrade() else {
            return;
        };
        let request = {
            let mut state = open_state.lock().expect("editor state lock poisoned");
            if state.pending {
                window.set_error_message("Wait for the workflow edit to finish saving".into());
                return;
            }
            invalidate_choices(&window, &mut state);
            state.request = state.request.wrapping_add(1);
            state.pending = false;
            state.reopen_required = false;
            state.selected_step.clear();
            state.selected_action = None;
            state.action_destination = None;
            state.action_saved_step = None;
            state.literal_overrides.clear();
            clear_control(&window, &mut state);
            state.selected_trigger = None;
            state.inspector_drafts.clear();
            state.session = None;
            state.request
        };
        window.set_editor_loading(true);
        window.set_editor_open(false);
        window.set_editor_selected_step("".into());
        window.set_editor_active_field("".into());
        window.set_editor_draft_value("".into());
        window.set_editor_edit_error("".into());
        window.set_editor_rename_active(false);
        window.set_editor_rename_draft("".into());
        window.set_editor_edit_pending(false);
        window.set_editor_can_edit_fields(false);
        window.set_editor_can_rename(false);
        window.set_editor_can_undo(false);
        window.set_editor_can_redo(false);
        window.set_editor_can_compose(false);
        window.set_editor_can_add_trigger(false);
        window.set_editor_action_mode(0);
        window.set_editor_trigger_mode(0);
        window.set_editor_trigger_editing(false);
        window.set_editor_trigger_kind("".into());
        window.set_editor_trigger_scene("".into());
        window.set_editor_trigger_error("".into());
        window.set_editor_action_query("".into());
        window.set_editor_action_category("all".into());
        window.set_editor_sidepanel_tab(0);
        set_action_choices(&window, open_services.action_schemas(), "");
        window.set_editor_action_error("".into());
        window.set_editor_action_fields(ModelRc::new(VecModel::from(Vec::new())));
        window.set_editor_fields(ModelRc::new(VecModel::from(Vec::new())));

        let repository = open_repository.clone();
        let services = Arc::clone(&open_services);
        let state = Arc::clone(&open_state);
        let result_window = weak.clone();
        let requested_id = id.to_string();
        let task = open_spawner.spawn_task("open automation", move |_| async move {
            let result = tokio::task::spawn_blocking(move || repository.load(&requested_id)).await;
            let loaded = match result {
                Ok(result) => result,
                Err(error) => Err(format!("workflow loader stopped: {error}")),
            };
            let _ = result_window.upgrade_in_event_loop(move |window| {
                let current = {
                    let state = state.lock().expect("editor state lock poisoned");
                    state.request == request
                };
                if !current {
                    return;
                }
                window.set_editor_loading(false);
                match loaded {
                    Ok(loaded)
                        if window.get_page() == 1
                            && window.get_selected_automation() == loaded.workflow().id =>
                    {
                        let definition = loaded.definition().clone();
                        let workflow_titles = services
                            .list_workflows()
                            .into_iter()
                            .map(|status| (status.id, status.title))
                            .collect::<BTreeMap<_, _>>();
                        let preview = WorkflowPreview::from_definition_with_titles(
                            &definition,
                            services.action_schemas(),
                            &workflow_titles,
                        );
                        let selected = String::new();
                        {
                            let mut state = state.lock().expect("editor state lock poisoned");
                            state.session = Some(WorkflowEditSession::new(loaded));
                            state.reopen_required = false;
                            state.selected_step = selected.clone();
                            state.workflow_titles = workflow_titles;
                            update_history_buttons(&window, &state);
                        }
                        update_availability(&window, &services, &definition);
                        set_trigger_choices(&window, &definition);
                        update_selected_trigger(&window, &definition);
                        window.set_editor_title(preview.title.into());
                        window.set_editor_workflow_enabled(definition.enabled);
                        window.set_editor_revision(preview.revision.into());
                        window.set_editor_triggers(ModelRc::new(VecModel::from(preview.triggers)));
                        window.set_editor_steps(ModelRc::new(VecModel::from(preview.steps)));
                        window.set_editor_selected_step(selected.clone().into());
                        window.set_editor_active_field("".into());
                        window.set_editor_draft_value("".into());
                        window.set_editor_edit_error("".into());
                        window.set_editor_open(true);
                        refresh_fields(&window, &state, services.action_schemas());
                    }
                    Ok(_) => {}
                    Err(error) => window
                        .set_error_message(format!("Could not open automation: {error}").into()),
                }
            });
            Ok::<(), std::convert::Infallible>(())
        });
        if let Err(error) = task {
            let mut state = open_state.lock().expect("editor state lock poisoned");
            if state.request == request {
                state.session = None;
                window.set_editor_loading(false);
                window.set_error_message(format!("Could not open automation: {error}").into());
            }
        }
    });

    let weak = window.as_weak();
    let select_state = Arc::clone(&state);
    let schemas = services.action_schemas().to_vec();
    window.on_select_editor_step(move |selected_id| {
        if let Some(window) = weak.upgrade() {
            let selected_id = selected_id.to_string();
            if window.get_editor_action_mode() == 2 {
                return;
            }
            if window.get_editor_control_mode() != 0 {
                return;
            }
            if window.get_editor_trigger_mode() == 2 {
                return;
            }
            let restore = {
                let mut state = select_state.lock().expect("editor state lock poisoned");
                if state.pending {
                    window.set_editor_selected_step(state.selected_step.clone().into());
                    return;
                }
                if state.control.is_some() || !state.control_frames.is_empty() {
                    window.set_editor_selected_step(state.selected_step.clone().into());
                    return;
                }
                invalidate_choices(&window, &mut state);
                let active_field = window.get_editor_active_field().to_string();
                let draft_value = window.get_editor_draft_value().to_string();
                switch_inspector_step(&mut state, &selected_id, &active_field, &draft_value)
            };
            window.set_editor_action_mode(0);
            window.set_editor_trigger_mode(0);
            window.set_editor_trigger_editing(false);
            if let Some((field_id, draft_value)) = restore {
                window.set_editor_active_field(field_id.into());
                window.set_editor_draft_value(draft_value.into());
            } else {
                window.set_editor_active_field("".into());
                window.set_editor_draft_value("".into());
            }
            window.set_editor_edit_error("".into());
            refresh_fields(&window, &select_state, &schemas);
            window.set_editor_sidepanel_tab(1);
        }
    });

    let weak = window.as_weak();
    let remove_step_state = Arc::clone(&state);
    let remove_step_context = PersistenceContext {
        services: Arc::clone(&services),
        repository: repository.clone(),
        spawner: spawner.clone(),
    };
    window.on_remove_editor_step(move |selected| {
        let Some(window) = weak.upgrade() else { return };
        match structural_step_candidate(&window, &remove_step_state, &selected, None) {
            Ok(Some(candidate)) => persist_candidate(
                &window,
                Arc::clone(&remove_step_state),
                remove_step_context.clone(),
                candidate,
                SaveIntent::Other,
                "remove workflow step",
            ),
            Ok(None) => {}
            Err(error) => window.set_error_message(error.into()),
        }
    });

    let weak = window.as_weak();
    let move_step_state = Arc::clone(&state);
    let move_step_context = PersistenceContext {
        services: Arc::clone(&services),
        repository: repository.clone(),
        spawner: spawner.clone(),
    };
    window.on_move_editor_step(move |selected, direction| {
        let Some(window) = weak.upgrade() else { return };
        let direction = match direction {
            -1 => StepMoveDirection::Up,
            1 => StepMoveDirection::Down,
            _ => return,
        };
        match structural_step_candidate(&window, &move_step_state, &selected, Some(direction)) {
            Ok(Some(candidate)) => persist_candidate(
                &window,
                Arc::clone(&move_step_state),
                move_step_context.clone(),
                candidate,
                SaveIntent::Other,
                "move workflow step",
            ),
            Ok(None) => {}
            Err(error) => window.set_error_message(error.into()),
        }
    });

    let weak = window.as_weak();
    let edit_state = Arc::clone(&state);
    let edit_services = Arc::clone(&services);
    let edit_repository = repository.clone();
    let edit_spawner = spawner.clone();
    let edit_context = PersistenceContext {
        services: Arc::clone(&edit_services),
        repository: edit_repository.clone(),
        spawner: edit_spawner.clone(),
    };
    window.on_apply_editor_field(move |step_id, field_id, value| {
        let Some(window) = weak.upgrade() else {
            return;
        };
        let step_id = step_id.to_string();
        let field_id = field_id.to_string();
        let value = value.to_string();
        if window.get_editor_edit_pending() || window.get_editor_rename_active() {
            return;
        }
        let candidate = {
            let guard = edit_state.lock().expect("editor state lock poisoned");
            if guard.pending || guard.reopen_required {
                return;
            }
            let Some(session) = guard.session.as_ref() else {
                window.set_editor_edit_error("Open a saved workflow before editing fields".into());
                return;
            };
            if window.get_selected_automation() != session.opened().workflow().id
                || window.get_editor_selected_step().as_str() != step_id
                || window.get_editor_active_field().as_str() != field_id
            {
                return;
            }
            let mut candidate = session.clone();
            let fields = WorkflowPreview::fields_for_step(
                candidate.draft(),
                &step_id,
                edit_services.action_schemas(),
            );
            let Some(field) = fields.iter().find(|field| {
                field.id == field_id
                    && (field.editable
                        || (field.is_output
                            && field.kind != EditorFieldKind::Secret
                            && guard
                                .literal_overrides
                                .contains(&(step_id.clone(), field_id.clone()))))
            }) else {
                window.set_editor_edit_error("This field cannot be edited".into());
                return;
            };
            let input = match parse_literal(field.kind, &value) {
                Ok(input) => input,
                Err(error) => {
                    window.set_editor_edit_error(error.into());
                    return;
                }
            };
            let Some(action_id) = configured_action_id(&step_id) else {
                window.set_editor_edit_error("Select an action step to edit".into());
                return;
            };
            let change = if field.configured {
                candidate.set_action_input(action_id, &field_id, input)
            } else {
                candidate.add_action_input(action_id, &field_id, input)
            };
            if let Err(error) = change {
                window.set_editor_edit_error(error.to_string().into());
                return;
            }
            candidate
        };
        if !candidate.is_dirty() {
            let mut guard = edit_state.lock().expect("editor state lock poisoned");
            let field = window.get_editor_active_field().to_string();
            guard.literal_overrides.remove(&(step_id.clone(), field));
            window.set_editor_active_field("".into());
            window.set_editor_draft_value("".into());
            window.set_editor_edit_error("".into());
            return;
        }
        persist_candidate(
            &window,
            Arc::clone(&edit_state),
            edit_context.clone(),
            candidate,
            SaveIntent::Field,
            "save workflow field",
        );
    });

    let weak = window.as_weak();
    let remove_state = Arc::clone(&state);
    let remove_context = PersistenceContext {
        services: Arc::clone(&services),
        repository: repository.clone(),
        spawner: spawner.clone(),
    };
    window.on_remove_editor_field(move |step_id, field_id| {
        let Some(window) = weak.upgrade() else {
            return;
        };
        if !window.get_editor_active_field().is_empty() || window.get_editor_rename_active() {
            return;
        }
        let step_id = step_id.to_string();
        let field_id = field_id.to_string();
        let candidate = {
            let guard = remove_state.lock().expect("editor state lock poisoned");
            if guard.pending || guard.reopen_required {
                return;
            }
            let Some(session) = guard.session.as_ref() else {
                return;
            };
            if window.get_selected_automation() != session.opened().workflow().id
                || window.get_editor_selected_step().as_str() != step_id
            {
                return;
            }
            let fields = WorkflowPreview::fields_for_step(
                session.draft(),
                &step_id,
                remove_context.services.action_schemas(),
            );
            if !fields.iter().any(|field| {
                field.id == field_id && field.optional && field.configured && field.editable
            }) {
                window.set_error_message("This field cannot be removed".into());
                return;
            }
            let Some(action_id) = configured_action_id(&step_id) else {
                return;
            };
            let mut candidate = session.clone();
            if let Err(error) = candidate.remove_action_input(action_id, &field_id) {
                window.set_error_message(error.to_string().into());
                return;
            }
            candidate
        };
        persist_candidate(
            &window,
            Arc::clone(&remove_state),
            remove_context.clone(),
            candidate,
            SaveIntent::Other,
            "remove workflow field",
        );
    });

    let weak = window.as_weak();
    let rename_state = Arc::clone(&state);
    let rename_context = PersistenceContext {
        services: Arc::clone(&services),
        repository: repository.clone(),
        spawner: spawner.clone(),
    };
    window.on_apply_editor_name(move |name| {
        let Some(window) = weak.upgrade() else {
            return;
        };
        if !window.get_editor_rename_active() || !window.get_editor_active_field().is_empty() {
            return;
        }
        let candidate = {
            let guard = rename_state.lock().expect("editor state lock poisoned");
            if guard.pending || guard.reopen_required || !window.get_editor_can_rename() {
                return;
            }
            let Some(session) = guard.session.as_ref() else {
                return;
            };
            if window.get_selected_automation() != session.opened().workflow().id {
                return;
            }
            let mut candidate = session.clone();
            if let Err(error) = candidate.rename(name.to_string()) {
                window.set_error_message(error.to_string().into());
                return;
            }
            candidate
        };
        if !candidate.is_dirty() {
            window.set_editor_rename_active(false);
            window.set_editor_rename_draft("".into());
            return;
        }
        persist_candidate(
            &window,
            Arc::clone(&rename_state),
            rename_context.clone(),
            candidate,
            SaveIntent::Name,
            "rename workflow",
        );
    });

    let weak = window.as_weak();
    let undo_state = Arc::clone(&state);
    let undo_context = PersistenceContext {
        services: Arc::clone(&services),
        repository: repository.clone(),
        spawner: spawner.clone(),
    };
    window.on_undo_editor(move || {
        let Some(window) = weak.upgrade() else {
            return;
        };
        let Some(candidate) = history_candidate(&window, &undo_state, false) else {
            return;
        };
        persist_candidate(
            &window,
            Arc::clone(&undo_state),
            undo_context.clone(),
            candidate,
            SaveIntent::Other,
            "undo workflow edit",
        );
    });

    let weak = window.as_weak();
    let redo_state = Arc::clone(&state);
    let redo_context = PersistenceContext {
        services: Arc::clone(&services),
        repository: repository.clone(),
        spawner: spawner.clone(),
    };
    window.on_redo_editor(move || {
        let Some(window) = weak.upgrade() else {
            return;
        };
        let Some(candidate) = history_candidate(&window, &redo_state, true) else {
            return;
        };
        persist_candidate(
            &window,
            Arc::clone(&redo_state),
            redo_context.clone(),
            candidate,
            SaveIntent::Other,
            "redo workflow edit",
        );
    });

    let weak = window.as_weak();
    let trigger_state = Arc::clone(&state);
    window.on_add_editor_trigger(move || {
        let Some(window) = weak.upgrade() else { return };
        if !window.get_editor_can_add_trigger()
            || window.get_editor_action_mode() != 0
            || window.get_editor_trigger_mode() != 0
            || window.get_editor_edit_pending()
            || !window.get_editor_active_field().is_empty()
            || window.get_editor_rename_active()
        {
            return;
        }
        let guard = trigger_state.lock().expect("editor state lock poisoned");
        if guard.pending || guard.reopen_required || guard.session.is_none() {
            return;
        }
        window.set_editor_trigger_error("".into());
        window.set_editor_trigger_editing(false);
        window.set_editor_trigger_mode(1);
        window.set_editor_sidepanel_tab(1);
    });

    let weak = window.as_weak();
    let trigger_state = Arc::clone(&state);
    window.on_choose_editor_trigger(move |kind| {
        let Some(window) = weak.upgrade() else { return };
        if window.get_editor_trigger_mode() != 1 || !window.get_editor_can_add_trigger() {
            return;
        }
        let mut guard = trigger_state.lock().expect("editor state lock poisoned");
        if guard.pending || guard.reopen_required {
            return;
        }
        let Some(session) = guard.session.as_ref() else {
            return;
        };
        let allowed = trigger_choices(session.draft())
            .iter()
            .any(|choice| choice.kind == kind);
        if !allowed {
            return;
        }
        let (kind, title) = match kind.as_str() {
            "obs.recording_started" => ("obs.recording_started", "OBS recording started"),
            "obs.current_scene" => ("obs.current_scene", "OBS scene changed"),
            "manual" => ("manual", "Manual run"),
            "twitch.channel.updated" => ("twitch.channel.updated", "Twitch channel updated"),
            "twitch.channel.details_changed" => (
                "twitch.channel.details_changed",
                "Twitch title or game changed",
            ),
            _ => return,
        };
        guard.selected_trigger = Some(kind);
        window.set_editor_trigger_kind(kind.into());
        window.set_editor_trigger_title(title.into());
        window.set_editor_trigger_scene("".into());
        window.set_editor_trigger_error("".into());
        window.set_editor_trigger_mode(2);
    });

    let weak = window.as_weak();
    let trigger_state = Arc::clone(&state);
    window.on_cancel_editor_trigger(move || {
        let Some(window) = weak.upgrade() else { return };
        if window.get_editor_edit_pending() {
            return;
        }
        let mut guard = trigger_state.lock().expect("editor state lock poisoned");
        guard.selected_trigger = None;
        if window.get_editor_trigger_mode() == 2 {
            window.set_editor_trigger_mode(1);
            window.set_editor_sidepanel_tab(1);
        } else {
            window.set_editor_trigger_mode(0);
        }
        window.set_editor_trigger_error("".into());
    });

    let weak = window.as_weak();
    let trigger_state = Arc::clone(&state);
    let trigger_context = PersistenceContext {
        services: Arc::clone(&services),
        repository: repository.clone(),
        spawner: spawner.clone(),
    };
    window.on_save_editor_trigger(move || {
        let Some(window) = weak.upgrade() else { return };
        if window.get_editor_trigger_mode() != 2 || !window.get_editor_can_add_trigger() {
            return;
        }
        let candidate = {
            let guard = trigger_state.lock().expect("editor state lock poisoned");
            if guard.pending || guard.reopen_required {
                return;
            }
            let (Some(kind), Some(session)) = (guard.selected_trigger, guard.session.as_ref())
            else {
                return;
            };
            if window.get_selected_automation() != session.opened().workflow().id {
                return;
            }
            let kind = match kind {
                "twitch.channel.details_changed" => TriggerKind::IntegrationEvent {
                    integration: "twitch".into(),
                    event: "channel.details_changed".into(),
                    filters: BTreeMap::new(),
                },
                "twitch.channel.updated" => TriggerKind::IntegrationEvent {
                    integration: "twitch".into(),
                    event: "channel.updated".into(),
                    filters: BTreeMap::new(),
                },
                "manual" => TriggerKind::Manual,
                "obs.recording_started" => TriggerKind::ObsRecordingStarted,
                "obs.current_scene" => TriggerKind::ObsCurrentScene {
                    scene: window.get_editor_trigger_scene().to_string(),
                },
                _ => return,
            };
            let mut candidate = session.clone();
            if let Err(error) = candidate.append_trigger(kind) {
                window.set_editor_trigger_error(error.to_string().into());
                return;
            }
            candidate
        };
        persist_candidate(
            &window,
            Arc::clone(&trigger_state),
            trigger_context.clone(),
            candidate,
            SaveIntent::TriggerAdded,
            "add workflow trigger",
        );
    });

    set_action_choices(window, services.action_schemas(), "");
    let weak = window.as_weak();
    let search_services = Arc::clone(&services);
    window.on_search_editor_actions(move |query| {
        if let Some(window) = weak.upgrade() {
            set_action_choices(&window, search_services.action_schemas(), &query);
        }
    });

    let weak = window.as_weak();
    let choose_services = Arc::clone(&services);
    let choose_state = Arc::clone(&state);
    window.on_choose_editor_action(move |capability, version| {
        let Some(window) = weak.upgrade() else { return };
        if window.get_editor_action_mode() != 1 || !window.get_editor_can_compose() {
            return;
        }
        let Some(schema) = choose_services
            .action_schemas()
            .iter()
            .copied()
            .find(|schema| {
                capability == schema.id && schema.version == version as u32 && composable(schema)
            })
        else {
            return;
        };
        let mut guard = choose_state.lock().expect("editor state lock poisoned");
        if guard.pending || guard.reopen_required || guard.session.is_none() {
            return;
        }
        invalidate_choices(&window, &mut guard);
        guard.selected_action = Some(schema);
        window.set_editor_action_title(schema.title.into());
        window.set_editor_action_fields(ModelRc::new(VecModel::from(
            schema
                .fields
                .iter()
                .map(|field| ActionDraftField {
                    id: field.id.into(),
                    label: field.label.into(),
                    description: field.description.into(),
                    value: if field.kind == ConfigFieldKind::Toggle {
                        "false"
                    } else {
                        ""
                    }
                    .into(),
                    kind: editor_field_kind(field.kind),
                    required: field.required,
                    configured: field.required,
                    has_choices: field.choice_source.is_some(),
                })
                .collect::<Vec<_>>(),
        )));
        window.set_editor_action_error("".into());
        window.set_editor_action_mode(2);
        window.set_editor_sidepanel_tab(1);
    });

    let weak = window.as_weak();
    let update_choice_state = Arc::clone(&state);
    window.on_update_editor_action_field(move |index, value, configured| {
        let Some(window) = weak.upgrade() else { return };
        if window.get_editor_action_mode() != 2 || window.get_editor_edit_pending() {
            return;
        }
        let model = window.get_editor_action_fields();
        let Ok(index) = usize::try_from(index) else {
            return;
        };
        let Some(mut field) = model.row_data(index) else {
            return;
        };
        if window.get_editor_choice_visible() {
            let mut guard = update_choice_state
                .lock()
                .expect("editor state lock poisoned");
            invalidate_choices(&window, &mut guard);
        }
        field.configured = field.required || configured;
        field.value = value;
        model.set_row_data(index, field);
        window.set_editor_action_error("".into());
    });

    let weak = window.as_weak();
    let cancel_state = Arc::clone(&state);
    window.on_cancel_editor_action(move || {
        let Some(window) = weak.upgrade() else { return };
        if window.get_editor_edit_pending() {
            return;
        }
        let mut guard = cancel_state.lock().expect("editor state lock poisoned");
        invalidate_choices(&window, &mut guard);
        if window.get_editor_action_mode() == 1 {
            guard.staged_child_branch = None;
            if guard.control.is_some() {
                window.set_editor_control_mode(1);
                window.set_editor_sidepanel_tab(1);
                set_insert_destination(&window, guard.action_destination.as_ref());
            } else {
                guard.action_destination = None;
                set_insert_destination(&window, None);
            }
            window.set_editor_action_mode(0);
            return;
        }
        guard.selected_action = None;
        drop(guard);
        window.set_editor_action_fields(ModelRc::new(VecModel::from(Vec::new())));
        window.set_editor_action_error("".into());
        window.set_editor_action_mode(1);
        window.set_editor_sidepanel_tab(0);
    });

    let weak = window.as_weak();
    let save_state = Arc::clone(&state);
    let save_context = PersistenceContext {
        services,
        repository,
        spawner,
    };
    window.on_save_editor_action(move || {
        let Some(window) = weak.upgrade() else { return };
        if window.get_editor_action_mode() != 2 || !window.get_editor_can_compose() {
            return;
        }
        let candidate = {
            let mut guard = save_state.lock().expect("editor state lock poisoned");
            if guard.pending || guard.reopen_required {
                return;
            }
            let (Some(schema), Some(session)) = (guard.selected_action, guard.session.clone())
            else {
                return;
            };
            if window.get_selected_automation() != session.opened().workflow().id {
                return;
            }
            let mut inputs = BTreeMap::new();
            for field in window
                .get_editor_action_fields()
                .iter()
                .filter(|field| field.configured)
            {
                let input = match parse_literal(field.kind, &field.value) {
                    Ok(input) => input,
                    Err(error) => {
                        window.set_editor_action_error(format!("{}: {error}", field.label).into());
                        return;
                    }
                };
                inputs.insert(field.id.to_string(), input);
            }
            if let Some(branch) = guard.staged_child_branch.clone() {
                let target = guard.control_target.clone();
                let destination = guard.action_destination.clone();
                let Some(draft) = guard.control.as_mut() else {
                    return;
                };
                if let Err(error) = draft.stage_action(&branch, schema, inputs) {
                    window.set_editor_action_error(error.into());
                    return;
                }
                set_insert_destination(&window, destination.as_ref());
                publish_control(
                    &window,
                    draft,
                    target.as_deref(),
                    save_context.services.action_schemas(),
                );
                preview_control_steps(&window, &guard, save_context.services.action_schemas());
                guard.staged_child_branch = None;
                guard.selected_action = None;
                window.set_editor_action_fields(ModelRc::new(VecModel::from(Vec::new())));
                window.set_editor_action_mode(0);
                return;
            }
            if let Err(error) = validate_configured_inputs(schema, &inputs) {
                window.set_editor_action_error(error.into());
                return;
            }
            let mut candidate = session;
            let destination = guard
                .action_destination
                .clone()
                .unwrap_or(StepDestination::Root);
            let result = candidate.insert_step(
                StepKind::Action {
                    capability: schema.id.to_owned(),
                    version: schema.version,
                    inputs,
                    deadline_ms: None,
                },
                destination,
            );
            match result {
                Ok(id) => guard.action_saved_step = Some(id),
                Err(error) => {
                    window.set_editor_action_error(error.to_string().into());
                    return;
                }
            }
            candidate
        };
        persist_candidate(
            &window,
            Arc::clone(&save_state),
            save_context.clone(),
            candidate,
            SaveIntent::ActionAdded,
            "add workflow action",
        );
    });
}

fn composable(schema: &ConfigSchema) -> bool {
    !schema
        .fields
        .iter()
        .any(|field| field.kind == ConfigFieldKind::Secret)
}

fn invalidate_choices(window: &AppWindow, state: &mut EditorState) {
    state.choice_request = state.choice_request.wrapping_add(1);
    state.active_choice = None;
    window.set_editor_choice_visible(false);
    window.set_editor_choice_loading(false);
    window.set_editor_choice_error("".into());
    window.set_editor_choices(ModelRc::new(VecModel::from(Vec::new())));
}

fn choice_context(
    window: &AppWindow,
    state: &EditorState,
    schemas: &[&'static ConfigSchema],
    field_id: &str,
    new_action: bool,
) -> Result<ChoiceContext, String> {
    if state.pending || state.reopen_required || window.get_page() != 1 || !window.get_editor_open()
    {
        return Err("Open an editable workflow to browse choices".into());
    }
    let session = state
        .session
        .as_ref()
        .ok_or("Open a workflow to browse choices")?;
    let workflow_id = session.opened().workflow().id.clone();
    if window.get_selected_automation() != workflow_id {
        return Err("This workflow is no longer selected".into());
    }
    let step_id = window.get_editor_selected_step().to_string();
    let (schema, dependency) = if new_action {
        if window.get_editor_action_mode() != 2 || !window.get_editor_can_compose() {
            return Err("Select an action before browsing choices".into());
        }
        let schema = state
            .selected_action
            .ok_or("Select an action before browsing choices")?;
        let field = schema
            .fields
            .iter()
            .find(|field| field.id == field_id)
            .ok_or("This field is unavailable")?;
        if field.kind == ConfigFieldKind::Secret {
            return Err("This field cannot use a choice list".into());
        }
        let source = field.choice_source.ok_or("This field has no choice list")?;
        let dependency = source
            .depends_on
            .map(|id| {
                window
                    .get_editor_action_fields()
                    .iter()
                    .find(|field| field.id == id && field.configured)
                    .map(|field| field.value.to_string())
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| dependency_error(schema, id))
            })
            .transpose()?;
        (schema, dependency)
    } else {
        if window.get_editor_action_mode() != 0
            || !window.get_editor_can_edit_fields()
            || state.selected_step != step_id
        {
            return Err("Select an editable action field to browse choices".into());
        }
        let action_id = configured_action_id(&step_id).ok_or("Select an action step")?;
        let (capability, version, inputs) =
            find_action_inputs(&session.draft().workflow.steps, action_id)
                .ok_or("This action step is unavailable")?;
        let schema = schemas
            .iter()
            .copied()
            .find(|schema| schema.id == capability && schema.version == version)
            .ok_or("Action metadata is unavailable")?;
        let editable = WorkflowPreview::fields_for_step(session.draft(), &step_id, schemas)
            .into_iter()
            .any(|field| field.id == field_id && field.editable && field.has_choices);
        if !editable {
            return Err("This field cannot use a choice list".into());
        }
        let source = schema
            .fields
            .iter()
            .find(|field| field.id == field_id)
            .and_then(|field| field.choice_source)
            .ok_or("This field has no choice list")?;
        let dependency = source
            .depends_on
            .map(|id| {
                inputs
                    .get(id)
                    .and_then(|input| match input {
                        Input::Literal(Value::String(value)) if !value.is_empty() => {
                            Some(value.clone())
                        }
                        _ => None,
                    })
                    .ok_or_else(|| dependency_error(schema, id))
            })
            .transpose()?;
        (schema, dependency)
    };
    let field = schema
        .fields
        .iter()
        .find(|field| field.id == field_id)
        .ok_or("This field is unavailable")?;
    let source = field.choice_source.ok_or("This field has no choice list")?;
    Ok(ChoiceContext {
        session_request: state.request,
        request: state.choice_request,
        workflow_id,
        step_id,
        field_id: field_id.to_owned(),
        title: field.label.to_owned(),
        new_action,
        source,
        dependency,
        values: Vec::new(),
    })
}

fn dependency_error(schema: &ConfigSchema, id: &str) -> String {
    let label = schema
        .fields
        .iter()
        .find(|field| field.id == id)
        .map_or(id, |field| field.label);
    format!("Set {label} before browsing choices")
}

fn find_action_inputs<'a>(
    steps: &'a [Step],
    id: &str,
) -> Option<(&'a str, u32, &'a BTreeMap<String, Input>)> {
    for step in steps {
        if step.id == id {
            if let StepKind::Action {
                capability,
                version,
                inputs,
                ..
            } = &step.kind
            {
                return Some((capability, *version, inputs));
            }
            return None;
        }
        let nested = match &step.kind {
            StepKind::If {
                then_steps,
                else_steps,
                ..
            } => find_action_inputs(then_steps, id).or_else(|| find_action_inputs(else_steps, id)),
            StepKind::While { steps, .. } | StepKind::OneOrMore { steps } => {
                find_action_inputs(steps, id)
            }
            _ => None,
        };
        if nested.is_some() {
            return nested;
        }
    }
    None
}

fn find_step_kind<'a>(steps: &'a [Step], id: &str) -> Option<&'a StepKind> {
    for step in steps {
        if step.id == id {
            return Some(&step.kind);
        }
        let nested = match &step.kind {
            StepKind::If {
                then_steps,
                else_steps,
                ..
            } => find_step_kind(then_steps, id).or_else(|| find_step_kind(else_steps, id)),
            StepKind::While { steps, .. } | StepKind::OneOrMore { steps } => {
                find_step_kind(steps, id)
            }
            _ => None,
        };
        if nested.is_some() {
            return nested;
        }
    }
    None
}

fn step_destination(parent: &str, branch: &str, steps: &[Step]) -> Result<StepDestination, String> {
    let id = configured_action_id(parent).ok_or("Select a container step")?;
    let kind = find_step_kind(steps, id).ok_or("This container step is unavailable")?;
    let branch = match (kind, branch) {
        (StepKind::If { .. }, "then") => StepBranch::Then,
        (StepKind::If { .. }, "else") => StepBranch::Else,
        (StepKind::While { .. } | StepKind::OneOrMore { .. }, "body") => StepBranch::Body,
        _ => return Err("This container has no such branch".into()),
    };
    Ok(StepDestination::Branch {
        parent_id: id.to_owned(),
        branch,
    })
}

fn choice_is_current(window: &AppWindow, state: &EditorState, context: &ChoiceContext) -> bool {
    if !window.get_editor_choice_visible()
        || state.pending
        || state.reopen_required
        || window.get_page() != 1
        || !window.get_editor_open()
        || state
            .active_choice
            .as_ref()
            .is_none_or(|active| active != context)
        || window.get_selected_automation() != context.workflow_id
        || window.get_editor_selected_step() != context.step_id
        || window.get_editor_choice_field() != context.field_id
        || window.get_editor_choice_new_action() != context.new_action
    {
        return false;
    }
    if context.new_action && window.get_editor_action_mode() != 2 {
        return false;
    }
    if !context.new_action && window.get_editor_action_mode() != 0 {
        return false;
    }
    let Some(session) = state.session.as_ref() else {
        return false;
    };
    if session.opened().workflow().id != context.workflow_id {
        return false;
    }
    let dependency = if context.new_action {
        context.source.depends_on.and_then(|id| {
            window
                .get_editor_action_fields()
                .iter()
                .find(|field| field.id == id && field.configured)
                .map(|field| field.value.to_string())
        })
    } else {
        configured_action_id(&context.step_id)
            .and_then(|id| find_action_inputs(&session.draft().workflow.steps, id))
            .and_then(|(_, _, inputs)| context.source.depends_on.and_then(|id| inputs.get(id)))
            .and_then(|input| match input {
                Input::Literal(Value::String(value)) => Some(value.clone()),
                _ => None,
            })
    };
    choice_still_matches(state, context, dependency.as_deref())
}

fn choice_still_matches(
    state: &EditorState,
    context: &ChoiceContext,
    dependency: Option<&str>,
) -> bool {
    state.request == context.session_request
        && state.choice_request == context.request
        && context.dependency.as_deref() == dependency
}

fn choice_contains_value(context: &ChoiceContext, value: &str) -> bool {
    context.values.iter().any(|candidate| candidate == value)
}

fn control_choice(kind: &str, title: &str) -> ControlChoice {
    ControlChoice {
        kind: kind.into(),
        title: title.into(),
    }
}

fn value_choice(source: &ValueSource) -> ValueChoice {
    ValueChoice {
        source: source.source.id().into(),
        source_id: source.source_id.clone().into(),
        output_id: source.output_id.clone().into(),
        label: source.label.clone().into(),
        detail: source.detail.clone().into(),
        optional: source.optional,
        fallback_kind: source.fallback_kind.into(),
    }
}

fn source_matches_field(source: &ValueSource, kind: EditorFieldKind) -> bool {
    kind != EditorFieldKind::Secret
        && (source.fallback_kind.is_empty() || source.fallback_kind == editor_kind_id(kind))
}

fn editor_kind_id(kind: EditorFieldKind) -> &'static str {
    match kind {
        EditorFieldKind::Text => "text",
        EditorFieldKind::Integer => "number",
        EditorFieldKind::Toggle => "toggle",
        EditorFieldKind::Secret => "",
    }
}

fn can_edit_control_input(window: &AppWindow, state: &EditorState, slot: &str) -> bool {
    !state.pending
        && !state.reopen_required
        && window.get_page() == 1
        && window.get_editor_open()
        && window.get_editor_control_mode() == 1
        && !window.get_editor_edit_pending()
        && !window.get_editor_choice_visible()
        && !window.get_editor_value_visible()
        && window.get_editor_action_mode() == 0
        && window.get_editor_trigger_mode() == 0
        && state.session.as_ref().is_some_and(|session| {
            window.get_selected_automation() == session.opened().workflow().id
        })
        && state.control.as_ref().is_some_and(|draft| {
            let mut probe = draft.clone();
            probe
                .set_input(slot, Input::Literal(Value::String(String::new())))
                .is_ok()
        })
}

fn scoped_control_definition(
    session: &WorkflowEditSession,
    control_target: Option<&str>,
    destination: Option<&StepDestination>,
) -> Result<(WorkflowDefinition, Option<String>), String> {
    if let Some(target) = control_target {
        let id = configured_action_id(target).ok_or("Select a control step")?;
        return Ok((session.draft().clone(), Some(id.to_owned())));
    }
    if let Some(destination) = destination {
        let mut candidate = session.clone();
        let marker = candidate
            .insert_step(StepKind::Stop, destination.clone())
            .map_err(|error| error.to_string())?;
        return Ok((candidate.draft().clone(), Some(marker)));
    }
    Ok((session.draft().clone(), None))
}

fn saved_action_field_context(
    window: &AppWindow,
    state: &EditorState,
    field_id: &str,
    schemas: &[&'static ConfigSchema],
) -> Option<(String, String)> {
    if state.pending
        || state.reopen_required
        || window.get_page() != 1
        || !window.get_editor_open()
        || !window.get_editor_can_edit_fields()
        || window.get_editor_edit_pending()
        || window.get_editor_choice_visible()
        || window.get_editor_value_visible()
        || window.get_editor_control_mode() != 0
        || window.get_editor_action_mode() != 0
        || window.get_editor_trigger_mode() != 0
        || !window.get_editor_active_field().is_empty()
        || window.get_editor_rename_active()
    {
        return None;
    }
    let session = state.session.as_ref()?;
    let workflow_id = session.opened().workflow().id.clone();
    if window.get_selected_automation() != workflow_id
        || window.get_editor_selected_step() != state.selected_step
    {
        return None;
    }
    let step_id = configured_action_id(&state.selected_step)?.to_owned();
    let fields = WorkflowPreview::fields_for_step(session.draft(), &state.selected_step, schemas);
    fields
        .iter()
        .any(|field| field.id == field_id && field.kind != EditorFieldKind::Secret)
        .then_some((step_id, workflow_id))
}

fn value_picker_matches(window: &AppWindow, state: &EditorState, picker: &ValuePicker) -> bool {
    !state.pending
        && !state.reopen_required
        && window.get_editor_value_visible()
        && window.get_page() == 1
        && window.get_editor_open()
        && !window.get_editor_edit_pending()
        && state.request == picker.session_request
        && state.session.as_ref().is_some_and(|session| {
            session.opened().workflow().id == picker.workflow_id
                && window.get_selected_automation() == picker.workflow_id
        })
        && window.get_editor_value_slot() == picker.target.slot()
        && match &picker.target {
            ValueTarget::Control { control_target, .. } => {
                window.get_editor_control_mode() == 1
                    && state.control_target == *control_target
                    && state.control.is_some()
            }
            ValueTarget::SavedAction { step_id, .. } => {
                let selected = format!("step:{step_id}");
                window.get_editor_control_mode() == 0
                    && window.get_editor_action_mode() == 0
                    && window.get_editor_trigger_mode() == 0
                    && window.get_editor_can_edit_fields()
                    && window.get_editor_active_field().is_empty()
                    && !window.get_editor_rename_active()
                    && state.selected_step == selected
                    && window.get_editor_selected_step() == selected
            }
        }
}

fn clear_value_picker(window: &AppWindow, state: &mut EditorState) {
    state.value_picker = None;
    window.set_editor_value_visible(false);
    window.set_editor_value_slot("".into());
    window.set_editor_value_choices(ModelRc::new(VecModel::from(Vec::new())));
    window.set_editor_value_selected(-1);
    window.set_editor_value_fallback_kind("".into());
    window.set_editor_value_fallback_value("".into());
    window.set_editor_value_error("".into());
}

fn can_begin_control(window: &AppWindow, state: &EditorState) -> bool {
    !state.pending
        && !state.reopen_required
        && state.session.as_ref().is_some_and(|session| {
            window.get_selected_automation() == session.opened().workflow().id
        })
        && window.get_page() == 1
        && window.get_editor_open()
        && !window.get_editor_edit_pending()
        && !window.get_editor_choice_visible()
        && window.get_editor_trigger_mode() == 0
        && window.get_editor_control_mode() == 0
}

fn control_accepts_branch(kind: ControlKind, branch: &str) -> bool {
    match kind {
        ControlKind::If => matches!(branch, "then" | "else"),
        ControlKind::While | ControlKind::OneOrMore => branch == "body",
        _ => false,
    }
}

fn begin_nested_control(
    state: &mut EditorState,
    kind: ControlKind,
) -> Result<ControlDraft, String> {
    let branch = state
        .staged_child_branch
        .as_ref()
        .ok_or("Select a branch for the child control")?
        .clone();
    let parent = state
        .control
        .as_ref()
        .ok_or("The parent control is unavailable")?;
    if !control_accepts_branch(parent.kind, &branch) {
        return Err("This parent cannot stage a child control".into());
    }
    let frame = ControlFrame {
        draft: state.control.take().expect("parent control checked above"),
        target: state.control_target.take(),
        destination: state.action_destination.clone(),
        branch,
    };
    state.control_frames.push(frame);
    state.staged_child_branch = None;
    let draft = ControlDraft::new(kind);
    state.control = Some(draft.clone());
    Ok(draft)
}

fn stage_nested_control(state: &mut EditorState, kind: StepKind) -> Result<ControlDraft, String> {
    let frame = state
        .control_frames
        .last()
        .ok_or("The parent control is unavailable")?;
    let mut parent = frame.draft.clone();
    parent.stage_kind(&frame.branch, kind)?;
    let frame = state
        .control_frames
        .pop()
        .expect("parent frame checked above");
    state.control = Some(parent.clone());
    state.control_target = frame.target;
    state.action_destination = frame.destination;
    state.staged_child_branch = None;
    Ok(parent)
}

fn cancel_nested_control(state: &mut EditorState) -> Option<ControlDraft> {
    let frame = state.control_frames.pop()?;
    state.control = Some(frame.draft.clone());
    state.control_target = frame.target;
    state.action_destination = frame.destination;
    state.staged_child_branch = None;
    state.control.clone()
}

fn update_control_draft(
    window: &AppWindow,
    state: &Mutex<EditorState>,
    schemas: &[&ConfigSchema],
    id: &str,
    value: String,
) {
    let mut guard = state.lock().expect("editor state lock poisoned");
    if guard.pending
        || guard.reopen_required
        || window.get_editor_control_mode() != 1
        || window.get_editor_edit_pending()
        || window.get_editor_value_visible()
    {
        return;
    }
    let target = guard.control_target.clone();
    let Some(draft) = guard.control.as_mut() else {
        return;
    };
    if let Err(error) = draft.set_field(id, value.clone()) {
        window.set_editor_control_error(error.into());
        return;
    }
    if id == "operator" || id.ends_with("_type") {
        publish_control(window, draft, target.as_deref(), schemas);
    } else {
        let fields = window.get_editor_control_fields();
        if let Some((index, mut field)) =
            fields.iter().enumerate().find(|(_, field)| field.id == id)
        {
            field.value = value.clone().into();
            field.display_value = value.into();
            fields.set_row_data(index, field);
        }
        window.set_editor_control_error("".into());
    }
}

fn control_field(
    id: &str,
    label: &str,
    value: &str,
    kind: EditorFieldKind,
    choices: Vec<ControlChoice>,
    editable: bool,
) -> ControlField {
    let display_value = choices
        .iter()
        .find(|choice| choice.kind == value)
        .map_or(value, |choice| choice.title.as_str());
    ControlField {
        id: id.into(),
        label: label.into(),
        value: value.into(),
        display_value: display_value.into(),
        kind,
        choices: ModelRc::new(VecModel::from(choices)),
        editable,
    }
}

fn type_choices() -> Vec<ControlChoice> {
    vec![
        control_choice("text", "Text"),
        control_choice("number", "Number"),
        control_choice("toggle", "On or Off"),
    ]
}

fn typed_fields(slot: &str, label: &str, input: &TypedInput) -> Vec<ControlField> {
    vec![
        control_field(
            &format!("{slot}_type"),
            &format!("{label} type"),
            input.kind.id(),
            EditorFieldKind::Text,
            if input.editable() {
                type_choices()
            } else {
                Vec::new()
            },
            input.editable(),
        ),
        control_field(
            slot,
            label,
            &input.value,
            match input.kind {
                LiteralType::Text => EditorFieldKind::Text,
                LiteralType::Number => EditorFieldKind::Integer,
                LiteralType::Toggle => EditorFieldKind::Toggle,
            },
            Vec::new(),
            input.editable(),
        ),
    ]
}

fn control_fields(draft: &ControlDraft) -> Vec<ControlField> {
    match draft.kind {
        ControlKind::Wait => vec![control_field(
            "duration",
            "Duration in milliseconds",
            &draft.wait_ms,
            EditorFieldKind::Integer,
            Vec::new(),
            true,
        )],
        ControlKind::SetVariable => {
            let mut fields = vec![control_field(
                "name",
                "Variable name",
                &draft.variable_name,
                EditorFieldKind::Text,
                Vec::new(),
                true,
            )];
            fields.extend(typed_fields("value", "Value", &draft.variable_value));
            fields
        }
        ControlKind::If | ControlKind::While => match &draft.condition {
            ConditionDraft::Simple {
                operator,
                left,
                right,
            } => {
                let mut fields = vec![control_field(
                    "operator",
                    "Condition",
                    operator.id(),
                    EditorFieldKind::Text,
                    ConditionOperator::all()
                        .iter()
                        .map(|operator| control_choice(operator.id(), operator.label()))
                        .collect(),
                    true,
                )];
                fields.extend(typed_fields("left", "Left input", left));
                if !matches!(
                    operator,
                    ConditionOperator::Exists | ConditionOperator::NullOrEmpty
                ) {
                    fields.extend(typed_fields("right", "Right input", right));
                }
                fields
            }
            ConditionDraft::Compound(_) => vec![control_field(
                "operator",
                "Condition",
                "Compound condition (read-only)",
                EditorFieldKind::Text,
                Vec::new(),
                false,
            )],
        },
        ControlKind::OneOrMore | ControlKind::Stop => Vec::new(),
    }
}

fn control_children(draft: &ControlDraft, schemas: &[&ConfigSchema]) -> Vec<ControlChild> {
    let mut rows = Vec::new();
    for (branch, steps) in [
        ("Then", &draft.then_steps),
        ("Else", &draft.else_steps),
        ("Body", &draft.body_steps),
    ] {
        for step in steps {
            let title = match &step.kind {
                StepKind::Action {
                    capability,
                    version,
                    ..
                } => schemas
                    .iter()
                    .find(|schema| schema.id == capability && schema.version == *version)
                    .map_or(capability.as_str(), |schema| schema.title),
                StepKind::Delay { .. } => "Wait",
                StepKind::If { .. } => "If",
                StepKind::While { .. } => "While",
                StepKind::OneOrMore { .. } => "One or More",
                StepKind::SetVariable { .. } => "Set variable",
                StepKind::Stop => "Stop",
                _ => "Step",
            };
            rows.push(ControlChild {
                kind: step.id.clone().into(),
                title: format!("{branch} · {title}").into(),
                removable: draft.can_remove_child(&step.id),
            });
        }
    }
    rows
}

fn publish_control(
    window: &AppWindow,
    draft: &ControlDraft,
    target: Option<&str>,
    schemas: &[&ConfigSchema],
) {
    window.set_editor_control_mode(1);
    window.set_editor_sidepanel_tab(1);
    window.set_editor_control_kind(draft.kind.id().into());
    window.set_editor_control_target(target.unwrap_or_default().into());
    window.set_editor_control_title(draft.kind.title().into());
    window.set_editor_control_fields(ModelRc::new(VecModel::from(control_fields(draft))));
    window.set_editor_control_children(ModelRc::new(VecModel::from(control_children(
        draft, schemas,
    ))));
    window.set_editor_control_error("".into());
    if window.get_editor_control_insert_branch().is_empty() {
        window.set_editor_control_insert_branch(
            if draft.kind == ControlKind::If {
                "then"
            } else {
                "body"
            }
            .into(),
        );
    }
}

fn set_insert_destination(window: &AppWindow, destination: Option<&StepDestination>) {
    match destination {
        Some(StepDestination::Branch { parent_id, branch }) => {
            window.set_editor_control_insert_parent(format!("step:{parent_id}").into());
            window.set_editor_control_insert_branch(
                match branch {
                    StepBranch::Then => "then",
                    StepBranch::Else => "else",
                    StepBranch::Body => "body",
                }
                .into(),
            );
        }
        _ => {
            window.set_editor_control_insert_parent("".into());
            window.set_editor_control_insert_branch("".into());
        }
    }
}

fn publish_control_parent_title(window: &AppWindow, state: &EditorState) {
    window.set_editor_control_parent_title(
        state
            .control_frames
            .last()
            .map_or("", |frame| frame.draft.kind.title())
            .into(),
    );
}

fn preview_control_steps(
    window: &AppWindow,
    state: &EditorState,
    schemas: &[&'static ConfigSchema],
) {
    let Some(session) = state.session.as_ref() else {
        return;
    };
    let (draft, target) = if let (Some(draft), Some(target)) =
        (state.control.as_ref(), state.control_target.as_deref())
    {
        (draft, target)
    } else {
        let Some(frame) = state
            .control_frames
            .iter()
            .rev()
            .find(|frame| frame.target.is_some())
        else {
            return;
        };
        let Some(target) = frame.target.as_deref() else {
            return;
        };
        (&frame.draft, target)
    };
    let (Some(id), Ok(kind)) = (configured_action_id(target), draft.build()) else {
        return;
    };
    let mut candidate = session.clone();
    if candidate.set_step_kind(id, kind).is_err() {
        return;
    }
    let mut preview = WorkflowPreview::from_definition_with_titles(
        candidate.draft(),
        schemas,
        &state.workflow_titles,
    );
    for row in &mut preview.steps {
        row.draft_child = state.control_target.as_deref() == Some(target)
            && row
                .id
                .strip_prefix("step:")
                .is_some_and(|id| draft.can_remove_child(id));
    }
    window.set_editor_steps(ModelRc::new(VecModel::from(preview.steps)));
}

fn restore_saved_steps(window: &AppWindow, state: &EditorState, schemas: &[&'static ConfigSchema]) {
    let Some(session) = state.session.as_ref() else {
        return;
    };
    let preview = WorkflowPreview::from_definition_with_titles(
        session.draft(),
        schemas,
        &state.workflow_titles,
    );
    window.set_editor_steps(ModelRc::new(VecModel::from(preview.steps)));
}

fn clear_control(window: &AppWindow, state: &mut EditorState) {
    clear_value_picker(window, state);
    state.control = None;
    state.control_frames.clear();
    state.control_target = None;
    state.control_saved_step = None;
    state.staged_child_branch = None;
    state.action_destination = None;
    window.set_editor_control_mode(0);
    window.set_editor_control_kind("".into());
    window.set_editor_control_target("".into());
    window.set_editor_control_parent_title("".into());
    window.set_editor_control_fields(ModelRc::new(VecModel::from(Vec::new())));
    window.set_editor_control_children(ModelRc::new(VecModel::from(Vec::new())));
    window.set_editor_control_error("".into());
    window.set_editor_control_insert_parent("".into());
    window.set_editor_control_insert_branch("".into());
}

fn editor_field_kind(kind: ConfigFieldKind) -> EditorFieldKind {
    match kind {
        ConfigFieldKind::Text => EditorFieldKind::Text,
        ConfigFieldKind::Integer => EditorFieldKind::Integer,
        ConfigFieldKind::Toggle => EditorFieldKind::Toggle,
        ConfigFieldKind::Secret => EditorFieldKind::Secret,
    }
}

fn set_action_choices(window: &AppWindow, schemas: &[&'static ConfigSchema], query: &str) {
    let query = query.trim().to_lowercase();
    let category = window.get_editor_action_category();
    let mut choices: Vec<_> = schemas
        .iter()
        .copied()
        .filter(|schema| {
            composable(schema)
                && action_category_matches(schema.id, &category)
                && (query.is_empty()
                    || schema.title.to_lowercase().contains(&query)
                    || schema.id.to_lowercase().contains(&query))
        })
        .map(|schema| ActionChoice {
            capability: schema.id.into(),
            version: schema.version as i32,
            title: schema.title.into(),
            group: action_group(schema.id).into(),
            icon: if schema.id.starts_with("twitch.") {
                StepIcon::Message
            } else if schema.id.starts_with("obs.") {
                StepIcon::Broadcast
            } else {
                StepIcon::Control
            },
        })
        .collect();
    choices.sort_by_key(|choice| (choice.group.to_string(), choice.title.to_lowercase()));
    window.set_editor_action_choices(ModelRc::new(VecModel::from(choices)));
    let controls = if category == "all" || category == "flow" {
        control_catalog(&query)
    } else {
        Vec::new()
    };
    window.set_editor_control_choices(ModelRc::new(VecModel::from(controls)));
}

fn action_group(capability: &str) -> &'static str {
    match capability.split('.').next().unwrap_or_default() {
        "twitch" => "Twitch",
        "obs" => "OBS",
        "vtube" => "VTube Studio",
        "lua" => "Lua",
        _ => "Flow",
    }
}

fn action_category_matches(capability: &str, category: &str) -> bool {
    category == "all"
        || matches!(
            (category, action_group(capability)),
            ("twitch", "Twitch")
                | ("obs", "OBS")
                | ("vtube", "VTube Studio")
                | ("lua", "Lua")
                | ("flow", "Flow")
        )
}

fn control_catalog(query: &str) -> Vec<ControlChoice> {
    ControlKind::all()
        .iter()
        .filter(|kind| {
            query.is_empty()
                || kind.title().to_lowercase().contains(query)
                || kind.id().contains(query)
        })
        .map(|kind| control_choice(kind.id(), kind.title()))
        .collect()
}

fn trigger_choices(definition: &WorkflowDefinition) -> Vec<TriggerChoice> {
    let has_manual = definition
        .triggers
        .iter()
        .any(|trigger| matches!(trigger.kind, TriggerKind::Manual));
    let has_recording = definition
        .triggers
        .iter()
        .any(|trigger| matches!(trigger.kind, TriggerKind::ObsRecordingStarted));
    let mut choices = Vec::new();
    if !has_manual {
        choices.push(TriggerChoice {
            kind: "manual".into(),
            title: "Manual run".into(),
            icon: StepIcon::Control,
        });
    }
    if !has_recording {
        choices.push(TriggerChoice {
            kind: "obs.recording_started".into(),
            title: "OBS recording started".into(),
            icon: StepIcon::Broadcast,
        });
    }
    choices.push(TriggerChoice {
        kind: "obs.current_scene".into(),
        title: "OBS scene changed".into(),
        icon: StepIcon::Broadcast,
    });
    if !definition.triggers.iter().any(|trigger| matches!(&trigger.kind, TriggerKind::IntegrationEvent { integration, event, .. } if integration == "twitch" && event == "channel.updated")) {
        choices.push(TriggerChoice { kind: "twitch.channel.updated".into(), title: "Twitch channel updated".into(), icon: StepIcon::Message });
    }
    if !definition.triggers.iter().any(|trigger| matches!(&trigger.kind, TriggerKind::IntegrationEvent { integration, event, .. } if integration == "twitch" && event == "channel.details_changed")) {
        choices.push(TriggerChoice { kind: "twitch.channel.details_changed".into(), title: "Twitch title or game changed".into(), icon: StepIcon::Message });
    }
    choices
}

fn set_trigger_choices(window: &AppWindow, definition: &WorkflowDefinition) {
    window.set_editor_trigger_choices(ModelRc::new(VecModel::from(trigger_choices(definition))));
}

fn structural_step_candidate(
    window: &AppWindow,
    state: &Mutex<EditorState>,
    selected: &str,
    direction: Option<StepMoveDirection>,
) -> Result<Option<WorkflowEditSession>, String> {
    if window.get_page() != 1
        || !window.get_editor_open()
        || !window.get_editor_can_edit_fields()
        || window.get_editor_edit_pending()
        || !window.get_editor_active_field().is_empty()
        || window.get_editor_rename_active()
        || window.get_editor_action_mode() != 0
        || window.get_editor_trigger_mode() != 0
        || window.get_editor_choice_visible()
    {
        return Ok(None);
    }
    let guard = state.lock().expect("editor state lock poisoned");
    if guard.pending || guard.reopen_required {
        return Ok(None);
    }
    let Some(session) = guard.session.as_ref() else {
        return Ok(None);
    };
    if window.get_selected_automation() != session.opened().workflow().id {
        return Ok(None);
    }
    let step_id = configured_action_id(selected).ok_or("Select a workflow step")?;
    let mut candidate = session.clone();
    match direction {
        Some(direction) => candidate.move_step(step_id, direction),
        None => candidate.remove_step(step_id),
    }
    .map_err(|error| error.to_string())?;
    Ok(candidate.is_dirty().then_some(candidate))
}

fn history_candidate(
    window: &AppWindow,
    state: &Mutex<EditorState>,
    redo: bool,
) -> Option<WorkflowEditSession> {
    if !window.get_editor_active_field().is_empty() || window.get_editor_rename_active() {
        return None;
    }
    let guard = state.lock().expect("editor state lock poisoned");
    if guard.pending || guard.reopen_required {
        return None;
    }
    let session = guard.session.as_ref()?;
    if window.get_selected_automation() != session.opened().workflow().id {
        return None;
    }
    history_candidate_session(session, redo)
}

fn history_candidate_session(
    session: &WorkflowEditSession,
    redo: bool,
) -> Option<WorkflowEditSession> {
    let mut candidate = session.clone();
    let changed = if redo {
        candidate.redo()
    } else {
        candidate.undo()
    };
    changed.then_some(candidate)
}

fn switch_inspector_step(
    state: &mut EditorState,
    next_step: &str,
    active_field: &str,
    draft_value: &str,
) -> Option<(String, String)> {
    let previous_step = state.selected_step.clone();
    if !active_field.is_empty() {
        state.inspector_drafts.insert(
            previous_step,
            (active_field.to_owned(), draft_value.to_owned()),
        );
    } else {
        state.inspector_drafts.remove(&previous_step);
    }
    state.selected_step = next_step.to_owned();
    state.inspector_drafts.remove(next_step)
}

enum SaveCompletion {
    Failed(String),
    RefreshFailed(String),
    Reloaded(Box<EditableWorkflow>, Vec<SaveWarning>),
}

#[derive(Clone, Copy)]
enum SaveIntent {
    Field,
    Name,
    ActionAdded,
    ControlSaved,
    TriggerAdded,
    TriggerEdited,
    Other,
}

fn persist_candidate(
    window: &AppWindow,
    state: Arc<Mutex<EditorState>>,
    context: PersistenceContext,
    candidate: WorkflowEditSession,
    intent: SaveIntent,
    task_name: &'static str,
) {
    let services = Arc::clone(&context.services);
    let (request, workflow_id) = {
        let mut guard = state.lock().expect("editor state lock poisoned");
        if guard.pending || guard.reopen_required {
            return;
        }
        if guard.session.as_ref().is_none_or(|session| {
            session.opened().workflow().id != candidate.opened().workflow().id
                || window.get_selected_automation() != session.opened().workflow().id
        }) {
            return;
        }
        invalidate_choices(window, &mut guard);
        guard.pending = true;
        update_history_buttons(window, &guard);
        (guard.request, candidate.opened().workflow().id.clone())
    };
    window.set_editor_edit_pending(true);
    window.set_editor_trigger_pending(true);
    window.set_editor_edit_error("".into());
    window.set_editor_revision("Saving…".into());

    let weak = window.as_weak();
    let completion_state = Arc::clone(&state);
    let worker_services = Arc::clone(&services);
    let worker_repository = context.repository.clone();
    let task_candidate = candidate.clone();
    let task = context.spawner.spawn_task(task_name, move |_| async move {
        let result = match worker_services
            .save_workflow(task_candidate.opened(), task_candidate.draft())
        {
            Err(error) => SaveCompletion::Failed(error.to_string()),
            Ok(receiver) => match receiver.await {
                Err(_) => SaveCompletion::Failed("workflow save worker stopped before saving".into()),
                Ok(Err(error)) => SaveCompletion::Failed(error),
                Ok(Ok(outcome)) => {
                    match tokio::task::spawn_blocking(move || worker_repository.load(&workflow_id))
                        .await
                    {
                        Ok(Ok(loaded)) => {
                            SaveCompletion::Reloaded(Box::new(loaded), outcome.warnings)
                        }
                        Ok(Err(error)) => SaveCompletion::RefreshFailed(error),
                        Err(error) => SaveCompletion::RefreshFailed(format!(
                            "workflow reload stopped: {error}"
                        )),
                    }
                }
            },
        };
        let _ = weak.upgrade_in_event_loop(move |window| {
            let mut guard = completion_state
                .lock()
                .expect("editor state lock poisoned");
            if guard.request != request {
                return;
            }
            guard.pending = false;
            window.set_editor_edit_pending(false);
            window.set_editor_trigger_pending(false);
            let selected_matches = guard.session.as_ref().is_some_and(|session| {
                window.get_selected_automation() == session.opened().workflow().id
            });
            if !selected_matches {
                update_history_buttons(&window, &guard);
                return;
            }
            match result {
                SaveCompletion::Failed(error) => {
                    restore_saved_revision(&window, &guard);
                    show_operation_message(&window, intent, error);
                    update_history_buttons(&window, &guard);
                }
                SaveCompletion::RefreshFailed(error) => {
                    let message = format!(
                        "The workflow was saved, but its updated snapshot could not be loaded. Reopen it before editing again: {error}"
                    );
                    require_reopen(&window, &mut guard, message);
                }
                SaveCompletion::Reloaded(saved, warnings) => {
                    let mut candidate = candidate;
                    if let Err(error) = candidate.accept_saved(*saved) {
                        let message = format!(
                            "The workflow was saved, but its updated snapshot could not be accepted. Reopen it before editing again: {error}"
                        );
                        require_reopen(&window, &mut guard, message);
                        return;
                    }
                    let definition = candidate.draft().clone();
                    if matches!(intent, SaveIntent::ActionAdded) {
                        if let Some(id) = guard.action_saved_step.take() {
                            guard.selected_step = format!("step:{id}");
                            window.set_editor_selected_step(guard.selected_step.clone().into());
                        }
                        guard.action_destination = None;
                        guard.selected_action = None;
                        window.set_editor_action_mode(0);
                        window.set_editor_action_error("".into());
                        window.set_editor_action_fields(ModelRc::new(VecModel::from(Vec::new())));
                    }
                    if matches!(intent, SaveIntent::ControlSaved) {
                        if let Some(id) = guard.control_saved_step.clone() {
                            guard.selected_step = format!("step:{id}");
                            window.set_editor_selected_step(guard.selected_step.clone().into());
                        }
                        clear_control(&window, &mut guard);
                    }
                    if matches!(intent, SaveIntent::TriggerAdded) {
                        if let Some(trigger) = definition.triggers.last() {
                            guard.selected_step = format!("trigger:{}", trigger.id);
                            window.set_editor_selected_step(guard.selected_step.clone().into());
                        }
                        guard.selected_trigger = None;
                        window.set_editor_trigger_mode(0);
                        window.set_editor_trigger_kind("".into());
                        window.set_editor_trigger_scene("".into());
                        window.set_editor_trigger_error("".into());
                    }
                    if matches!(intent, SaveIntent::TriggerEdited) {
                        window.set_editor_trigger_editing(false);
                        window.set_editor_trigger_error("".into());
                    }
                    guard.session = Some(candidate);
                    guard.reopen_required = false;
                    if matches!(intent, SaveIntent::Name) {
                        window.set_editor_rename_active(false);
                        window.set_editor_rename_draft("".into());
                    }
                    if matches!(intent, SaveIntent::Field) {
                        let selected_step = guard.selected_step.clone();
                        let field = window.get_editor_active_field().to_string();
                        guard.literal_overrides.remove(&(selected_step.clone(), field));
                        guard.inspector_drafts.remove(&selected_step);
                        window.set_editor_active_field("".into());
                        window.set_editor_draft_value("".into());
                        window.set_editor_edit_error("".into());
                        if !warnings.is_empty() {
                            window.set_error_message(save_warnings_text(&warnings).into());
                        }
                    } else {
                        window.set_editor_active_field("".into());
                        window.set_editor_draft_value("".into());
                        if warnings.is_empty() {
                            window.set_editor_edit_error("".into());
                        } else {
                            window.set_error_message(save_warnings_text(&warnings).into());
                        }
                    }
                    guard.workflow_titles = services
                        .list_workflows()
                        .into_iter()
                        .map(|status| (status.id, status.title))
                        .collect();
                    let preview = WorkflowPreview::from_definition_with_titles(
                        &definition,
                        services.action_schemas(),
                        &guard.workflow_titles,
                    );
                    if !selected_step_exists(&preview, &guard.selected_step) {
                        guard.selected_step.clear();
                        guard.inspector_drafts.clear();
                        window.set_editor_selected_step("".into());
                        window.set_editor_active_field("".into());
                        window.set_editor_draft_value("".into());
                    }
                    window.set_editor_title(preview.title.into());
                    window.set_editor_workflow_enabled(definition.enabled);
                    window.set_editor_revision(preview.revision.into());
                    window.set_editor_triggers(ModelRc::new(VecModel::from(preview.triggers)));
                    window.set_editor_steps(ModelRc::new(VecModel::from(preview.steps)));
                    update_availability(&window, &services, &definition);
                    set_trigger_choices(&window, &definition);
                    update_selected_trigger(&window, &definition);
                    update_history_buttons(&window, &guard);
                    let fields = WorkflowPreview::fields_for_step(
                        &definition,
                        window.get_editor_selected_step().as_str(),
                        services.action_schemas(),
                    );
                    window.set_editor_fields(ModelRc::new(VecModel::from(fields)));
                }
            }
        });
        Ok::<(), std::convert::Infallible>(())
    });
    if let Err(error) = task {
        let mut guard = state.lock().expect("editor state lock poisoned");
        if guard.request == request {
            guard.pending = false;
            window.set_editor_edit_pending(false);
            window.set_editor_trigger_pending(false);
            restore_saved_revision(window, &guard);
            show_operation_message(window, intent, error.to_string());
            update_history_buttons(window, &guard);
        }
    }
}

fn selected_step_exists(preview: &WorkflowPreview, selected: &str) -> bool {
    selected.is_empty()
        || preview
            .steps
            .iter()
            .any(|step| !step.group_end && step.id == selected)
        || preview
            .triggers
            .iter()
            .any(|trigger| trigger.id == selected)
}

fn require_reopen(window: &AppWindow, state: &mut EditorState, message: String) {
    invalidate_choices(window, state);
    clear_control(window, state);
    state.reopen_required = true;
    state.selected_action = None;
    state.selected_trigger = None;
    window.set_editor_can_edit_fields(false);
    window.set_editor_can_compose(false);
    window.set_editor_can_add_trigger(false);
    window.set_editor_can_rename(false);
    window.set_editor_can_run(false);
    window.set_editor_revision("Saved · reopen to continue".into());
    window.set_editor_can_undo(false);
    window.set_editor_can_redo(false);
    window.set_editor_active_field("".into());
    window.set_editor_draft_value("".into());
    window.set_editor_rename_active(false);
    window.set_editor_rename_draft("".into());
    window.set_editor_action_mode(0);
    window.set_editor_trigger_mode(0);
    window.set_editor_trigger_editing(false);
    window.set_error_message(message.into());
}

fn restore_saved_revision(window: &AppWindow, state: &EditorState) {
    if let Some(session) = &state.session {
        window.set_editor_revision(
            format!("Saved revision {}", session.opened().workflow().revision).into(),
        );
    }
}

fn show_operation_message(window: &AppWindow, intent: SaveIntent, message: String) {
    if matches!(intent, SaveIntent::TriggerAdded | SaveIntent::TriggerEdited) {
        window.set_editor_trigger_error(message.into());
    } else if matches!(intent, SaveIntent::ActionAdded) {
        window.set_editor_action_error(message.into());
    } else if matches!(intent, SaveIntent::ControlSaved) {
        window.set_editor_control_error(message.into());
    } else if matches!(intent, SaveIntent::Field) {
        window.set_editor_edit_error(message.into());
    } else {
        window.set_error_message(message.into());
    }
}

fn update_history_buttons(window: &AppWindow, state: &EditorState) {
    let selected_matches = state
        .session
        .as_ref()
        .is_some_and(|session| window.get_selected_automation() == session.opened().workflow().id);
    let enabled = !state.pending && !state.reopen_required && selected_matches;
    window.set_editor_can_undo(
        enabled
            && state
                .session
                .as_ref()
                .is_some_and(WorkflowEditSession::can_undo),
    );
    window.set_editor_can_redo(
        enabled
            && state
                .session
                .as_ref()
                .is_some_and(WorkflowEditSession::can_redo),
    );
}

fn update_availability(
    window: &AppWindow,
    services: &AppServices,
    definition: &crate::workflows::WorkflowDefinition,
) {
    let status = services
        .list_workflows()
        .into_iter()
        .find(|status| status.id == definition.workflow.id);
    let can_edit = status.as_ref().is_some_and(|status| {
        status.error.is_none()
            || action_schemas_known(&definition.workflow.steps, services.action_schemas())
    });
    window.set_editor_can_run(
        status
            .as_ref()
            .is_some_and(|status| status.enabled && status.error.is_none() && status.has_steps),
    );
    window.set_editor_can_edit_fields(can_edit);
    window.set_editor_can_compose(status.as_ref().is_some_and(|status| status.error.is_none()));
    window.set_editor_can_add_trigger(status.as_ref().is_some_and(|status| status.error.is_none()));
    window.set_editor_can_rename(status.is_some_and(|status| status.error.is_none()));
}

fn action_schemas_known(steps: &[Step], schemas: &[&ConfigSchema]) -> bool {
    steps.iter().all(|step| match &step.kind {
        StepKind::Action {
            capability,
            version,
            ..
        } => schemas
            .iter()
            .any(|schema| schema.id == capability && schema.version == *version),
        StepKind::If {
            then_steps,
            else_steps,
            ..
        } => action_schemas_known(then_steps, schemas) && action_schemas_known(else_steps, schemas),
        StepKind::While { steps, .. } | StepKind::OneOrMore { steps } => {
            action_schemas_known(steps, schemas)
        }
        _ => true,
    })
}

fn save_warnings_text(warnings: &[SaveWarning]) -> String {
    warnings
        .iter()
        .map(|warning| match warning {
            SaveWarning::DirectorySync(message) => {
                format!("Saved, but directory sync failed: {message}")
            }
            SaveWarning::BackupRetention(message) => {
                format!("Saved, but backup cleanup failed: {message}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn refresh_fields(
    window: &AppWindow,
    state: &Mutex<EditorState>,
    schemas: &[&'static crate::schema::ConfigSchema],
) {
    let guard = state.lock().expect("editor state lock poisoned");
    if let Some(session) = guard.session.as_ref() {
        update_selected_trigger(window, session.draft());
    }
    let fields = guard
        .session
        .as_ref()
        .map(|session| {
            WorkflowPreview::fields_for_step(
                session.draft(),
                window.get_editor_selected_step().as_str(),
                schemas,
            )
        })
        .unwrap_or_default();
    window.set_editor_fields(ModelRc::new(VecModel::from(fields)));
}

fn update_selected_trigger(window: &AppWindow, definition: &WorkflowDefinition) {
    let selected = window.get_editor_selected_step();
    let enabled = selected
        .strip_prefix("trigger:")
        .and_then(|id| definition.triggers.iter().find(|trigger| trigger.id == id))
        .is_some_and(|trigger| trigger.enabled);
    window.set_editor_trigger_enabled(enabled);
}

fn parse_literal(kind: EditorFieldKind, text: &str) -> Result<Input, String> {
    match kind {
        EditorFieldKind::Text => Ok(Input::Literal(Value::String(text.to_owned()))),
        EditorFieldKind::Integer => text
            .parse::<i64>()
            .map(|value| Input::Literal(Value::from(value)))
            .or_else(|_| {
                text.parse::<u64>()
                    .map(|value| Input::Literal(Value::from(value)))
            })
            .map_err(|_| "Enter a whole number".to_owned()),
        EditorFieldKind::Toggle => match text {
            "true" => Ok(Input::Literal(Value::Bool(true))),
            "false" => Ok(Input::Literal(Value::Bool(false))),
            _ => Err("Choose On or Off".to_owned()),
        },
        EditorFieldKind::Secret => Err("This field type cannot be edited".to_owned()),
    }
}

fn configured_action_id(selected_step: &str) -> Option<&str> {
    selected_step
        .strip_prefix("step:")
        .filter(|id| !id.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{
        ChoiceContext, ControlDraft, ControlKind, EditorFieldKind, EditorState, StepBranch,
        StepDestination, action_category_matches, action_group, action_schemas_known,
        begin_nested_control, cancel_nested_control, choice_contains_value, choice_still_matches,
        configured_action_id, control_catalog, find_action_inputs, history_candidate_session,
        parse_literal, scoped_control_definition, selected_step_exists, source_matches_field,
        stage_nested_control, switch_inspector_step, trigger_choices,
    };
    #[test]
    fn action_categories_keep_modules_separate_and_include_flow_actions() {
        for (capability, category, group) in [
            ("twitch.send_chat", "twitch", "Twitch"),
            ("obs.set_scene", "obs", "OBS"),
            ("vtube.play_hotkey", "vtube", "VTube Studio"),
            ("lua.run", "lua", "Lua"),
            ("workflow.run", "flow", "Flow"),
        ] {
            assert_eq!(action_group(capability), group);
            assert!(action_category_matches(capability, category));
            assert!(action_category_matches(capability, "all"));
            assert!(!action_category_matches(capability, "unknown"));
        }
        assert!(!action_category_matches("obs.set_scene", "twitch"));
        assert!(!action_category_matches("lua.run", "flow"));
    }
    use crate::editor::WorkflowEditSession;
    use crate::engine::{FailurePolicy, Input, Step, StepKind, Workflow};
    use crate::schema::{ConfigChoiceSource, ConfigSchema};
    use crate::ui::WorkflowPreview;
    use crate::value_sources::available_sources;
    use crate::value_sources::{ValueSource, ValueSourceKind};
    use crate::workflows::{TriggerKind, WorkflowDefinition, WorkflowRepository};
    use serde_json::json;
    use std::collections::BTreeMap;

    fn session() -> (tempfile::TempDir, WorkflowEditSession) {
        let directory = tempfile::tempdir().unwrap();
        let repository = WorkflowRepository::at(directory.path());
        let definition = WorkflowDefinition::manual(Workflow {
            id: "controller-test".into(),
            revision: 1,
            overlap: false,
            steps: vec![Step {
                id: "send".into(),
                on_failure: FailurePolicy::Stop,
                kind: StepKind::Action {
                    capability: "sample.action".into(),
                    version: 1,
                    inputs: BTreeMap::from([("message".into(), Input::Literal(json!("original")))]),
                    deadline_ms: None,
                },
            }],
            outputs: BTreeMap::new(),
        });
        repository.create_definition(&definition).unwrap();
        let session = WorkflowEditSession::new(repository.load("controller-test").unwrap());
        (directory, session)
    }

    fn message(session: &WorkflowEditSession) -> &serde_json::Value {
        let StepKind::Action { inputs, .. } = &session.draft().workflow.steps[0].kind else {
            panic!("expected action step");
        };
        let Input::Literal(value) = &inputs["message"] else {
            panic!("expected a literal message");
        };
        value
    }

    #[test]
    fn saved_action_reference_is_undoable_and_keeps_original_until_saved() {
        let (_directory, session) = session();
        let mut candidate = session.clone();
        candidate
            .set_action_input(
                "send",
                "message",
                Input::Variable {
                    name: "stable-variable-id".into(),
                    fallback: Some(Box::new(Input::Literal(json!("fallback")))),
                },
            )
            .unwrap();
        assert_eq!(message(&session), "original");
        let (_, _, inputs) = find_action_inputs(&candidate.draft().workflow.steps, "send").unwrap();
        assert!(matches!(
            &inputs["message"],
            Input::Variable { name, fallback: Some(_) } if name == "stable-variable-id"
        ));
        assert!(candidate.undo());
        assert_eq!(message(&candidate), "original");
    }

    #[test]
    fn saved_action_picker_filters_incompatible_value_types() {
        let source = ValueSource {
            source: ValueSourceKind::Step,
            source_id: "source".into(),
            output_id: "count".into(),
            label: "Count".into(),
            detail: String::new(),
            optional: false,
            fallback_kind: "number",
        };
        assert!(source_matches_field(&source, EditorFieldKind::Integer));
        assert!(!source_matches_field(&source, EditorFieldKind::Text));
        assert!(!source_matches_field(&source, EditorFieldKind::Secret));
        let unknown = ValueSource {
            fallback_kind: "",
            ..source
        };
        assert!(source_matches_field(&unknown, EditorFieldKind::Integer));
        assert!(source_matches_field(&unknown, EditorFieldKind::Text));
    }

    #[test]
    fn action_search_filters_control_choices_too() {
        let choices = control_catalog("while");
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0].kind, "while");
        assert!(control_catalog("missing-capability").is_empty());
        assert_eq!(control_catalog("").len(), ControlKind::all().len());
    }

    #[test]
    fn nested_control_cancel_preserves_parent_and_destination() {
        let destination = StepDestination::Branch {
            parent_id: "saved-parent".into(),
            branch: StepBranch::Else,
        };
        let mut state = EditorState {
            control: Some(ControlDraft::new(ControlKind::OneOrMore)),
            action_destination: Some(destination.clone()),
            staged_child_branch: Some("body".into()),
            ..Default::default()
        };
        begin_nested_control(&mut state, ControlKind::If).unwrap();
        assert_eq!(state.control_frames.len(), 1);
        assert_eq!(state.action_destination, Some(destination.clone()));
        cancel_nested_control(&mut state).unwrap();
        assert_eq!(state.control.as_ref().unwrap().kind, ControlKind::OneOrMore);
        assert!(state.control_frames.is_empty());
        assert_eq!(state.action_destination, Some(destination));
        assert!(state.control.as_ref().unwrap().body_steps.is_empty());
    }

    #[test]
    fn nested_control_done_stages_typed_child_before_outer_save() {
        let mut state = EditorState {
            control: Some(ControlDraft::new(ControlKind::OneOrMore)),
            staged_child_branch: Some("body".into()),
            ..Default::default()
        };
        begin_nested_control(&mut state, ControlKind::Wait).unwrap();
        let parent = stage_nested_control(&mut state, StepKind::Delay { millis: 25 }).unwrap();
        assert!(state.control_frames.is_empty());
        assert_eq!(parent.body_steps.len(), 1);
        assert!(!parent.body_steps[0].id.is_empty());
        assert!(matches!(
            parent.body_steps[0].kind,
            StepKind::Delay { millis: 25 }
        ));
        assert!(matches!(parent.build(), Ok(StepKind::OneOrMore { .. })));
        assert!(state.session.is_none());
    }

    #[test]
    fn nested_control_stack_can_stage_multiple_levels_atomically() {
        let mut state = EditorState {
            control: Some(ControlDraft::new(ControlKind::OneOrMore)),
            staged_child_branch: Some("body".into()),
            ..Default::default()
        };
        begin_nested_control(&mut state, ControlKind::If).unwrap();
        state.staged_child_branch = Some("then".into());
        begin_nested_control(&mut state, ControlKind::Wait).unwrap();
        let child = stage_nested_control(&mut state, StepKind::Delay { millis: 25 }).unwrap();
        let parent = stage_nested_control(&mut state, child.build().unwrap()).unwrap();
        let StepKind::OneOrMore { steps } = parent.build().unwrap() else {
            panic!("expected staged container");
        };
        let StepKind::If { then_steps, .. } = &steps[0].kind else {
            panic!("expected staged If");
        };
        assert!(matches!(then_steps[0].kind, StepKind::Delay { millis: 25 }));
        assert_ne!(steps[0].id, then_steps[0].id);
        assert!(state.session.is_none());
    }

    #[test]
    fn child_control_value_scope_stops_before_future_root_steps() {
        let (_directory, mut session) = session();
        session
            .insert_step(
                StepKind::SetVariable {
                    name: "before".into(),
                    value: Input::Literal(json!(1)),
                },
                StepDestination::Root,
            )
            .unwrap();
        let parent = session
            .insert_step(
                StepKind::If {
                    condition: crate::engine::Condition::Exists(Input::Literal(json!(true))),
                    then_steps: vec![],
                    else_steps: vec![],
                },
                StepDestination::Root,
            )
            .unwrap();
        session
            .insert_step(
                StepKind::SetVariable {
                    name: "future".into(),
                    value: Input::Literal(json!(2)),
                },
                StepDestination::Root,
            )
            .unwrap();
        let (definition, marker) = scoped_control_definition(
            &session,
            None,
            Some(&StepDestination::Branch {
                parent_id: parent,
                branch: StepBranch::Then,
            }),
        )
        .unwrap();
        let sources = available_sources(&definition, marker.as_deref(), &[], |_| None).unwrap();
        assert!(sources.iter().any(|source| source.source_id == "before"));
        assert!(!sources.iter().any(|source| source.source_id == "future"));
    }

    #[test]
    fn choice_result_requires_current_session_request_and_dependency() {
        let mut state = EditorState {
            request: 4,
            choice_request: 9,
            ..Default::default()
        };
        let context = ChoiceContext {
            session_request: 4,
            request: 9,
            workflow_id: "workflow".into(),
            step_id: "step:send".into(),
            field_id: "item".into(),
            title: "Item".into(),
            new_action: false,
            source: ConfigChoiceSource {
                key: "test.items",
                depends_on: Some("scene"),
            },
            dependency: Some("scene-id".into()),
            values: vec!["stable-item-id".into()],
        };
        assert!(choice_still_matches(&state, &context, Some("scene-id")));
        assert!(!choice_still_matches(
            &state,
            &context,
            Some("different-scene")
        ));
        assert!(choice_contains_value(&context, "stable-item-id"));
        assert!(!choice_contains_value(&context, "Visible item label"));
        state.choice_request += 1;
        assert!(!choice_still_matches(&state, &context, Some("scene-id")));
        state.choice_request -= 1;
        state.request += 1;
        assert!(!choice_still_matches(&state, &context, Some("scene-id")));
    }

    #[test]
    fn saved_action_dependency_comes_from_literal_input() {
        let (_directory, session) = session();
        let (capability, version, inputs) =
            find_action_inputs(&session.draft().workflow.steps, "send").unwrap();
        assert_eq!((capability, version), ("sample.action", 1));
        assert!(
            matches!(inputs.get("message"), Some(Input::Literal(value)) if value == &json!("original"))
        );
    }

    #[test]
    fn selection_clears_only_when_saved_step_is_absent() {
        let (_directory, session) = session();
        let before = WorkflowPreview::from_definition(session.draft());
        assert!(selected_step_exists(&before, "step:send"));
        assert!(selected_step_exists(&before, ""));
        let mut removed = session.clone();
        removed.remove_step("send").unwrap();
        let after = WorkflowPreview::from_definition(removed.draft());
        assert!(!selected_step_exists(&after, "step:send"));
        assert!(selected_step_exists(&after, &before.triggers[0].id));
    }

    #[test]
    fn trigger_picker_lists_only_available_kinds() {
        let (_directory, mut session) = session();
        let kinds: Vec<_> = trigger_choices(session.draft())
            .into_iter()
            .map(|choice| choice.kind.to_string())
            .collect();
        assert_eq!(
            kinds,
            [
                "obs.recording_started",
                "obs.current_scene",
                "twitch.channel.updated",
                "twitch.channel.details_changed"
            ]
        );

        session
            .append_trigger(TriggerKind::ObsRecordingStarted)
            .unwrap();
        let kinds: Vec<_> = trigger_choices(session.draft())
            .into_iter()
            .map(|choice| choice.kind.to_string())
            .collect();
        assert_eq!(
            kinds,
            [
                "obs.current_scene",
                "twitch.channel.updated",
                "twitch.channel.details_changed"
            ]
        );
    }

    #[test]
    fn known_action_schemas_allow_recovery_of_invalid_saved_inputs() {
        static SCHEMA: ConfigSchema = ConfigSchema {
            id: "sample.action",
            version: 1,
            title: "Sample",
            fields: &[],
            outputs: &[],
        };
        let (_directory, session) = session();
        assert!(action_schemas_known(
            &session.draft().workflow.steps,
            &[&SCHEMA]
        ));
        assert!(!action_schemas_known(&session.draft().workflow.steps, &[]));
    }

    #[test]
    fn parses_only_supported_scalar_literals() {
        assert_eq!(configured_action_id("step:send"), Some("send"));
        assert_eq!(configured_action_id("trigger:manual"), None);
        assert!(
            matches!(parse_literal(EditorFieldKind::Text, "hello"), Ok(Input::Literal(value)) if value == json!("hello"))
        );
        assert!(
            matches!(parse_literal(EditorFieldKind::Integer, "18446744073709551615"), Ok(Input::Literal(value)) if value == json!(u64::MAX))
        );
        assert!(
            matches!(parse_literal(EditorFieldKind::Toggle, "false"), Ok(Input::Literal(value)) if value == json!(false))
        );
        assert!(parse_literal(EditorFieldKind::Integer, "2.5").is_err());
        assert!(parse_literal(EditorFieldKind::Toggle, "yes").is_err());
        assert!(parse_literal(EditorFieldKind::Secret, "secret").is_err());
    }

    #[test]
    fn save_warnings_are_visible_as_actionable_messages() {
        assert_eq!(
            super::save_warnings_text(&[
                crate::storage::SaveWarning::DirectorySync("disk delayed".into()),
                crate::storage::SaveWarning::BackupRetention("permission denied".into()),
            ]),
            "Saved, but directory sync failed: disk delayed\nSaved, but backup cleanup failed: permission denied"
        );
    }

    #[test]
    fn undo_and_redo_persist_as_new_revisions_after_a_saved_edit() {
        let (directory, session) = session();
        let repository = WorkflowRepository::at(directory.path());

        let mut edited = session.clone();
        edited
            .set_action_input("send", "message", Input::Literal(json!("edited")))
            .unwrap();
        repository
            .save_definition(edited.opened(), edited.draft())
            .unwrap();
        edited
            .accept_saved(repository.load("controller-test").unwrap())
            .unwrap();
        assert!(edited.can_undo());
        assert!(!edited.can_redo());

        let mut undone = history_candidate_session(&edited, false).unwrap();
        repository
            .save_definition(undone.opened(), undone.draft())
            .unwrap();
        undone
            .accept_saved(repository.load("controller-test").unwrap())
            .unwrap();
        assert_eq!(message(&undone), &json!("original"));
        assert!(undone.can_redo());
        assert_eq!(undone.opened().workflow().revision, 3);

        let mut redone = history_candidate_session(&undone, true).unwrap();
        repository
            .save_definition(redone.opened(), redone.draft())
            .unwrap();
        redone
            .accept_saved(repository.load("controller-test").unwrap())
            .unwrap();
        assert_eq!(message(&redone), &json!("edited"));
        assert!(redone.can_undo());
        assert!(!redone.can_redo());
        assert_eq!(redone.opened().workflow().revision, 4);
    }

    #[test]
    fn failed_candidate_save_leaves_the_installed_session_unchanged() {
        let (directory, installed) = session();
        let repository = WorkflowRepository::at(directory.path());
        let mut candidate = installed.clone();
        candidate
            .set_action_input("send", "message", Input::Literal(json!("pending draft")))
            .unwrap();

        let current = repository.load("controller-test").unwrap();
        let mut concurrent = current.definition().clone();
        concurrent.name = Some("Changed elsewhere".into());
        concurrent.workflow.revision += 1;
        repository.save_definition(&current, &concurrent).unwrap();
        assert!(
            repository
                .save_definition(candidate.opened(), candidate.draft())
                .is_err()
        );

        assert!(!installed.is_dirty());
        assert!(!installed.can_undo());
        assert_eq!(message(&installed), &json!("original"));
        assert!(candidate.is_dirty());
        assert_eq!(message(&candidate), &json!("pending draft"));
    }

    #[test]
    fn inspector_draft_survives_switching_steps_and_cancel_discards_it() {
        let mut state = EditorState {
            selected_step: "step:a".into(),
            ..EditorState::default()
        };
        assert!(
            switch_inspector_step(&mut state, "step:b", "message", "unfinished text",).is_none()
        );
        assert_eq!(
            switch_inspector_step(&mut state, "step:a", "", ""),
            Some(("message".into(), "unfinished text".into()))
        );
        assert!(switch_inspector_step(&mut state, "step:b", "", "").is_none());
        assert!(state.inspector_drafts.is_empty());
    }
}
