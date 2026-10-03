use std::collections::BTreeMap;

use slint::{ModelRc, VecModel};

use crate::engine::{CallBinding, Condition, FailurePolicy, Input, Step, StepKind, TextPart};
use crate::schema::{ConfigFieldKind, ConfigSchema};
use crate::workflows::{TriggerKind, WorkflowDefinition};

use super::{EditorField, EditorFieldKind, StepIcon, ValueSegment, WorkflowStep};

pub struct WorkflowPreview {
    pub title: String,
    pub revision: String,
    pub triggers: Vec<WorkflowStep>,
    pub steps: Vec<WorkflowStep>,
}

impl WorkflowPreview {
    pub fn from_definition(definition: &WorkflowDefinition) -> Self {
        Self::from_definition_with_schemas(definition, &[])
    }

    pub fn from_definition_with_schemas(
        definition: &WorkflowDefinition,
        schemas: &[&'static ConfigSchema],
    ) -> Self {
        Self::from_definition_with_titles(definition, schemas, &BTreeMap::new())
    }

    pub fn from_definition_with_titles(
        definition: &WorkflowDefinition,
        schemas: &[&'static ConfigSchema],
        workflow_titles: &BTreeMap<String, String>,
    ) -> Self {
        let triggers = definition
            .triggers
            .iter()
            .map(|trigger| {
                let (label, detail, icon) = match &trigger.kind {
                    TriggerKind::Manual => (
                        "Manual".to_owned(),
                        "Run from the app".to_owned(),
                        StepIcon::Automation,
                    ),
                    TriggerKind::ObsRecordingStarted => (
                        "OBS recording started".to_owned(),
                        "Runs when OBS starts recording".to_owned(),
                        StepIcon::Broadcast,
                    ),
                    TriggerKind::ObsCurrentScene { scene } => (
                        "OBS current scene".to_owned(),
                        format!("Scene: {scene}"),
                        StepIcon::Broadcast,
                    ),
                    TriggerKind::IntegrationEvent {
                        integration,
                        event,
                        filters,
                    } => {
                        let (label, detail) = match (integration.as_str(), event.as_str()) {
                            ("twitch", "chat.command") => (
                                "Twitch chat command".to_owned(),
                                format!(
                                    "Command: {} · Aliases: {} · {}",
                                    filters
                                        .get("command")
                                        .and_then(serde_json::Value::as_str)
                                        .unwrap_or("Missing command"),
                                    filters
                                        .get("aliases")
                                        .and_then(serde_json::Value::as_array)
                                        .map_or(0, Vec::len),
                                    if filters
                                        .get("broadcaster_or_moderator")
                                        .and_then(serde_json::Value::as_bool)
                                        .unwrap_or(false)
                                    {
                                        "Moderators only"
                                    } else {
                                        "Anyone"
                                    },
                                ),
                            ),
                            ("twitch", "chat.message") => (
                                "Twitch chat message".to_owned(),
                                "Any chat message".to_owned(),
                            ),
                            ("twitch", "chat.notification") => (
                                "Twitch chat notification".to_owned(),
                                format!(
                                    "Notice type: {}",
                                    filters
                                        .get("notice_type")
                                        .and_then(serde_json::Value::as_str)
                                        .unwrap_or("Any")
                                ),
                            ),
                            ("twitch", "ad_break.begin") => {
                                ("Twitch ad break".to_owned(), "Ad break begins".to_owned())
                            }
                            ("twitch", "channel.details_changed") => (
                                "Twitch title or game changed".to_owned(),
                                "Groups changes for 2 seconds".to_owned(),
                            ),
                            ("twitch", "channel.updated") => (
                                "Twitch channel updated".to_owned(),
                                "Channel details change".to_owned(),
                            ),
                            _ => (
                                format!("{integration} · {event}"),
                                format!("{} filter(s)", filters.len()),
                            ),
                        };
                        (label, detail, StepIcon::Automation)
                    }
                };
                WorkflowStep {
                    id: format!("trigger:{}", trigger.id).into(),
                    label: label.clone().into(),
                    display_label: label.into(),
                    detail: format!(
                        "{} · {}",
                        if trigger.enabled {
                            "Enabled"
                        } else {
                            "Disabled"
                        },
                        detail
                    )
                    .into(),
                    icon,
                    value: empty_segments(),
                    condition: empty_segments(),
                    suffix: "".into(),
                    control_kind: "".into(),
                    depth: 0,
                    group_end: false,
                    branch_header: false,
                    branch_empty: false,
                    parent_id: "".into(),
                    draft_child: false,
                }
            })
            .collect();
        let mut steps = Vec::new();
        for (index, step) in definition.workflow.steps.iter().enumerate() {
            add_step(
                &mut steps,
                step,
                &(index + 1).to_string(),
                0,
                definition,
                schemas,
                workflow_titles,
            );
        }
        Self {
            title: definition.title().to_owned(),
            revision: format!("Saved revision {}", definition.workflow.revision),
            triggers,
            steps,
        }
    }

    pub fn fields_for_step(
        definition: &WorkflowDefinition,
        step_id: &str,
        schemas: &[&'static ConfigSchema],
    ) -> Vec<EditorField> {
        let Some(step_id) = step_id.strip_prefix("step:") else {
            return Vec::new();
        };
        let mut matches = Vec::new();
        collect_steps(&definition.workflow.steps, step_id, &mut matches);
        let [step] = matches.as_slice() else {
            return Vec::new();
        };
        let StepKind::Action {
            capability,
            version,
            inputs,
            ..
        } = &step.kind
        else {
            return Vec::new();
        };
        let Some(schema) = schemas
            .iter()
            .find(|schema| schema.id == capability && schema.version == *version)
        else {
            return Vec::new();
        };
        schema
            .fields
            .iter()
            .map(|field| {
                let input = inputs.get(field.id);
                let editable = !matches!(field.kind, ConfigFieldKind::Secret)
                    && (matches!(input, Some(Input::Literal(_)))
                        || (input.is_none() && !field.required));
                let value = match (field.kind, input) {
                    (_, None) => "Not set".to_owned(),
                    (ConfigFieldKind::Secret, Some(_)) => "Credential reference".to_owned(),
                    (_, Some(Input::Literal(serde_json::Value::String(value)))) => value.clone(),
                    (_, Some(Input::Literal(serde_json::Value::Null))) => String::new(),
                    (_, Some(input)) => input_text(input, definition, schemas),
                };
                EditorField {
                    id: field.id.into(),
                    label: field.label.into(),
                    description: field.description.into(),
                    value: value.into(),
                    kind: match field.kind {
                        ConfigFieldKind::Text => EditorFieldKind::Text,
                        ConfigFieldKind::Integer => EditorFieldKind::Integer,
                        ConfigFieldKind::Toggle => EditorFieldKind::Toggle,
                        ConfigFieldKind::Secret => EditorFieldKind::Secret,
                    },
                    editable,
                    has_choices: editable && field.choice_source.is_some(),
                    is_output: input.is_some_and(|input| !matches!(input, Input::Literal(_))),
                    configured: input.is_some(),
                    optional: !field.required,
                }
            })
            .collect()
    }
}

fn collect_steps<'a>(steps: &'a [Step], id: &str, found: &mut Vec<&'a Step>) {
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
                collect_steps(then_steps, id, found);
                collect_steps(else_steps, id, found);
            }
            StepKind::While { steps, .. } | StepKind::OneOrMore { steps } => {
                collect_steps(steps, id, found);
            }
            _ => {}
        }
    }
}

fn add_step(
    rows: &mut Vec<WorkflowStep>,
    step: &Step,
    position: &str,
    depth: i32,
    definition: &WorkflowDefinition,
    schemas: &[&'static ConfigSchema],
    workflow_titles: &BTreeMap<String, String>,
) {
    let (name, detail, icon, preview) = match &step.kind {
        StepKind::Action {
            capability,
            version,
            inputs,
            deadline_ms,
        } => {
            let schema = schemas
                .iter()
                .find(|schema| schema.id == capability && schema.version == *version);
            let name = schema.map_or_else(
                || {
                    capability
                        .rsplit('.')
                        .next()
                        .unwrap_or(capability)
                        .replace('_', " ")
                },
                |schema| schema.title.to_owned(),
            );
            let mut detail = if schema.is_some() {
                String::new()
            } else {
                "Action metadata is unavailable".to_owned()
            };
            if let Some(schema) = schema {
                for (key, value) in inputs {
                    let Some(field) = schema.fields.iter().find(|field| field.id == key) else {
                        continue;
                    };
                    if !detail.is_empty() {
                        detail.push('\n');
                    }
                    let display = if field.kind == ConfigFieldKind::Secret {
                        "Credential reference".to_owned()
                    } else {
                        input_text(value, definition, schemas)
                    };
                    detail.push_str(&format!("{}: {display}", field.label));
                }
            }
            if detail.is_empty() {
                detail = "No configuration".to_owned();
            }
            if let Some(deadline) = deadline_ms {
                detail.push_str(&format!("\nDeadline: {deadline} ms"));
            }
            (
                name,
                detail,
                if capability.starts_with("twitch.") {
                    StepIcon::Message
                } else if capability.starts_with("obs.") {
                    StepIcon::Broadcast
                } else {
                    StepIcon::Control
                },
                schema.and_then(|schema| {
                    schema.fields.iter().find_map(|field| {
                        (field.kind != ConfigFieldKind::Secret)
                            .then(|| inputs.get(field.id))
                            .flatten()
                    })
                }),
            )
        }
        StepKind::SetVariable { name, value } => (
            format!("Set {name}"),
            format!("Value: {}", input_text(value, definition, schemas)),
            StepIcon::Control,
            Some(value),
        ),
        StepKind::If { condition, .. } => (
            "If".to_owned(),
            format!(
                "Condition: {}",
                condition_text(condition, definition, schemas)
            ),
            StepIcon::Control,
            None,
        ),
        StepKind::While { condition, .. } => (
            "While".to_owned(),
            format!(
                "Condition: {}",
                condition_text(condition, definition, schemas)
            ),
            StepIcon::Control,
            None,
        ),
        StepKind::OneOrMore { .. } => (
            "One or More".to_owned(),
            "Runs every child; succeeds if at least one succeeds".to_owned(),
            StepIcon::Control,
            None,
        ),
        StepKind::Delay { millis } => (
            "Delay".to_owned(),
            format!("Wait {millis} ms"),
            StepIcon::Control,
            None,
        ),
        StepKind::RequestInput { title, fields } => (
            title.clone().unwrap_or_else(|| "Ask for input".to_owned()),
            fields
                .iter()
                .map(|field| field.id.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            StepIcon::Control,
            None,
        ),
        StepKind::Call {
            workflow_id,
            binding,
        } => (
            workflow_titles.get(workflow_id).map_or_else(
                || "Run missing workflow".to_owned(),
                |title| format!("Run {title}"),
            ),
            match binding {
                CallBinding::AllNamedArguments => "Pass named arguments".to_owned(),
                CallBinding::Explicit { inputs } if inputs.is_empty() => "No arguments".to_owned(),
                CallBinding::Explicit { inputs } => format!(
                    "Inputs:\n{}",
                    inputs
                        .iter()
                        .map(|(name, value)| format!(
                            "{name}: {}",
                            input_text(value, definition, schemas)
                        ))
                        .collect::<Vec<_>>()
                        .join("\n")
                ),
            },
            StepIcon::Control,
            None,
        ),
        StepKind::Stop => (
            "Stop".to_owned(),
            "End this workflow".to_owned(),
            StepIcon::Control,
            None,
        ),
    };
    let value = preview
        .filter(|input| input_text(input, definition, schemas).chars().count() <= 64)
        .map(|input| input_segments(input, definition, schemas))
        .unwrap_or_default();
    let suffix = match &step.kind {
        StepKind::Action { capability, .. } => match capability.split('.').next() {
            Some("twitch") => "Twitch".to_owned(),
            Some("obs") => "OBS".to_owned(),
            Some("vtube") => "VTube Studio".to_owned(),
            Some("lua") => "Lua".to_owned(),
            _ => String::new(),
        },
        _ => String::new(),
    };
    let condition = match &step.kind {
        StepKind::If { condition, .. } | StepKind::While { condition, .. } => {
            condition_segments(condition, definition, schemas)
        }
        _ => Vec::new(),
    };
    rows.push(WorkflowStep {
        id: format!("step:{}", step.id).into(),
        label: format!("{position} · {name}").into(),
        display_label: name.into(),
        detail: format!(
            "{detail}\nOn failure: {}",
            match step.on_failure {
                FailurePolicy::Stop => "Stop workflow",
                FailurePolicy::Continue => "Continue workflow",
            }
        )
        .into(),
        icon,
        value: ModelRc::new(VecModel::from(value)),
        condition: ModelRc::new(VecModel::from(condition)),
        suffix: suffix.into(),
        depth,
        group_end: false,
        branch_header: false,
        branch_empty: false,
        parent_id: "".into(),
        draft_child: false,
        control_kind: match &step.kind {
            StepKind::Delay { .. } => "wait",
            StepKind::If { .. } => "if",
            StepKind::While { .. } => "while",
            StepKind::OneOrMore { .. } => "one_or_more",
            StepKind::SetVariable { .. } => "set_variable",
            StepKind::Stop => "stop",
            _ => "",
        }
        .into(),
    });
    match &step.kind {
        StepKind::If {
            then_steps,
            else_steps,
            ..
        } => {
            add_branch_header(rows, step, depth, "then", "Then", then_steps.is_empty());
            for (index, child) in then_steps.iter().enumerate() {
                add_step(
                    rows,
                    child,
                    &format!("{position} › Then {}", index + 1),
                    depth + 1,
                    definition,
                    schemas,
                    workflow_titles,
                );
            }
            add_branch_header(rows, step, depth, "else", "Else", else_steps.is_empty());
            for (index, child) in else_steps.iter().enumerate() {
                add_step(
                    rows,
                    child,
                    &format!("{position} › Else {}", index + 1),
                    depth + 1,
                    definition,
                    schemas,
                    workflow_titles,
                );
            }
        }
        StepKind::While { steps, .. } | StepKind::OneOrMore { steps } => {
            let title = if matches!(step.kind, StepKind::While { .. }) {
                "Do"
            } else {
                "Try each"
            };
            add_branch_header(rows, step, depth, "body", title, steps.is_empty());
            for (index, child) in steps.iter().enumerate() {
                add_step(
                    rows,
                    child,
                    &format!("{position} › {}", index + 1),
                    depth + 1,
                    definition,
                    schemas,
                    workflow_titles,
                );
            }
        }
        _ => {}
    }
    if matches!(
        step.kind,
        StepKind::If { .. } | StepKind::While { .. } | StepKind::OneOrMore { .. }
    ) {
        rows.push(WorkflowStep {
            id: format!("end:{}", step.id).into(),
            parent_id: format!("step:{}", step.id).into(),
            depth,
            group_end: true,
            control_kind: if matches!(step.kind, StepKind::If { .. }) {
                "if"
            } else {
                "body"
            }
            .into(),
            ..Default::default()
        });
    }
}

fn add_branch_header(
    rows: &mut Vec<WorkflowStep>,
    parent: &Step,
    depth: i32,
    branch: &str,
    title: &str,
    empty: bool,
) {
    rows.push(WorkflowStep {
        id: format!("branch:{}:{branch}", parent.id).into(),
        label: title.into(),
        display_label: title.into(),
        depth: depth + 1,
        branch_header: true,
        branch_empty: empty,
        control_kind: branch.into(),
        parent_id: format!("step:{}", parent.id).into(),
        ..Default::default()
    });
}

fn empty_segments() -> ModelRc<ValueSegment> {
    ModelRc::new(VecModel::from(Vec::<ValueSegment>::new()))
}

fn input_segments(
    input: &Input,
    definition: &WorkflowDefinition,
    schemas: &[&ConfigSchema],
) -> Vec<ValueSegment> {
    let parts = match input {
        Input::Text(parts) => parts
            .iter()
            .map(|part| match part {
                TextPart::Literal(value) => (value.clone(), false),
                TextPart::Value(value) => (input_text(value, definition, schemas), true),
            })
            .collect(),
        Input::Literal(value) => vec![(literal_text(value), false)],
        value => vec![(input_text(value, definition, schemas), true)],
    };
    parts
        .into_iter()
        .map(|(text, is_output)| ValueSegment {
            text: text.into(),
            is_output,
            is_operator: false,
            source_icon: Default::default(),
        })
        .collect()
}

fn condition_segments(
    condition: &Condition,
    definition: &WorkflowDefinition,
    schemas: &[&ConfigSchema],
) -> Vec<ValueSegment> {
    let (left, operator, right) = match condition {
        Condition::Equal(left, right) => (left, "equals", Some(right)),
        Condition::NotEqual(left, right) => (left, "does not equal", Some(right)),
        Condition::Contains(left, right) => (left, "contains", Some(right)),
        Condition::Less(left, right) => (left, "is less than", Some(right)),
        Condition::Greater(left, right) => (left, "is greater than", Some(right)),
        Condition::Exists(value) => (value, "exists", None),
        Condition::NullOrEmpty(value) => (value, "is empty", None),
        Condition::All(_) | Condition::Any(_) | Condition::Not(_) => {
            return vec![ValueSegment {
                text: condition_text(condition, definition, schemas).into(),
                is_operator: true,
                ..Default::default()
            }];
        }
    };
    let mut segments = input_segments(left, definition, schemas);
    segments.push(ValueSegment {
        text: operator.into(),
        is_operator: true,
        ..Default::default()
    });
    if let Some(right) = right {
        segments.extend(input_segments(right, definition, schemas));
    }
    segments
}

fn input_text(input: &Input, definition: &WorkflowDefinition, schemas: &[&ConfigSchema]) -> String {
    match input {
        Input::Literal(value) => literal_text(value),
        Input::Object(fields) => format!(
            "{{{}}}",
            fields
                .iter()
                .map(|(key, value)| format!("{key}: {}", input_text(value, definition, schemas)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Input::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(|item| input_text(item, definition, schemas))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Input::Reference {
            step_id, output_id, ..
        } => reference_text(definition, schemas, step_id, output_id),
        Input::Trigger { name, .. } => {
            let output = definition.triggers.iter().find_map(|trigger| {
                crate::obs::trigger_value_schema(&trigger.kind)
                    .or_else(|| crate::twitch::trigger_value_schema(&trigger.kind))
                    .or_else(|| crate::vtube::trigger_value_schema(&trigger.kind))
                    .and_then(|outputs| outputs.into_iter().find(|output| output.id == name))
            });
            output.map_or_else(
                || format!("Trigger value: {name}"),
                |output| output.label.to_owned(),
            )
        }
        Input::Variable { name, .. } => format!("Variable: {name}"),
        Input::Text(parts) => parts
            .iter()
            .map(|part| match part {
                TextPart::Literal(value) => value.clone(),
                TextPart::Value(value) => format!("[{}]", input_text(value, definition, schemas)),
            })
            .collect(),
    }
}

fn reference_text(
    definition: &WorkflowDefinition,
    schemas: &[&ConfigSchema],
    step_id: &str,
    output_id: &str,
) -> String {
    let mut steps = Vec::new();
    collect_steps(&definition.workflow.steps, step_id, &mut steps);
    let [step] = steps.as_slice() else {
        return format!("Missing step {step_id}, output {output_id}");
    };
    match &step.kind {
        StepKind::Action {
            capability,
            version,
            ..
        } => {
            if let Some(schema) = schemas
                .iter()
                .find(|schema| schema.id == capability && schema.version == *version)
            {
                if let Some(output) = schema.outputs.iter().find(|output| output.id == output_id) {
                    return format!("{}: {}", schema.title, output.label);
                }
                return format!("{} output {output_id} (label unavailable)", schema.title);
            }
            format!("Action {step_id} output {output_id} (metadata unavailable)")
        }
        StepKind::RequestInput { fields, .. } => fields
            .iter()
            .find(|field| field.id == output_id)
            .and_then(|field| field.label.clone())
            .unwrap_or_else(|| format!("Input {output_id} from {step_id}")),
        _ => format!("Missing output {output_id} from {step_id}"),
    }
}

fn condition_text(
    condition: &Condition,
    definition: &WorkflowDefinition,
    schemas: &[&ConfigSchema],
) -> String {
    match condition {
        Condition::Equal(left, right) => {
            format!(
                "{} equals {}",
                input_text(left, definition, schemas),
                input_text(right, definition, schemas)
            )
        }
        Condition::NotEqual(left, right) => {
            format!(
                "{} does not equal {}",
                input_text(left, definition, schemas),
                input_text(right, definition, schemas)
            )
        }
        Condition::Exists(value) => format!("{} exists", input_text(value, definition, schemas)),
        Condition::NullOrEmpty(value) => {
            format!("{} is empty", input_text(value, definition, schemas))
        }
        Condition::Contains(left, right) => {
            format!(
                "{} contains {}",
                input_text(left, definition, schemas),
                input_text(right, definition, schemas)
            )
        }
        Condition::Less(left, right) => {
            format!(
                "{} is less than {}",
                input_text(left, definition, schemas),
                input_text(right, definition, schemas)
            )
        }
        Condition::Greater(left, right) => {
            format!(
                "{} is greater than {}",
                input_text(left, definition, schemas),
                input_text(right, definition, schemas)
            )
        }
        Condition::All(conditions) => conditions
            .iter()
            .map(|condition| condition_text(condition, definition, schemas))
            .collect::<Vec<_>>()
            .join(" and "),
        Condition::Any(conditions) => conditions
            .iter()
            .map(|condition| condition_text(condition, definition, schemas))
            .collect::<Vec<_>>()
            .join(" or "),
        Condition::Not(condition) => {
            format!("not ({})", condition_text(condition, definition, schemas))
        }
    }
}

fn literal_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(value) => value.clone(),
        value => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use slint::Model;

    use super::*;
    use crate::engine::{FailurePolicy, Workflow};
    use crate::schema::ConfigField;
    use crate::workflows::WorkflowDefinition;

    #[test]
    fn preview_preserves_nested_order_and_distinguishes_outputs() {
        static FIELDS: [ConfigField; 1] = [ConfigField {
            id: "message",
            label: "Message",
            description: "",
            introduced_in: 1,
            kind: ConfigFieldKind::Text,
            required: true,
            choice_source: None,
        }];
        static SCHEMA: ConfigSchema = ConfigSchema {
            id: "twitch.send_message",
            version: 1,
            title: "Send message",
            fields: &FIELDS,
            outputs: &[],
        };
        let definition = WorkflowDefinition::manual(Workflow {
            id: "sample".into(),
            revision: 3,
            overlap: false,
            steps: vec![Step {
                id: "branch".into(),
                on_failure: FailurePolicy::Stop,
                kind: StepKind::If {
                    condition: crate::engine::Condition::Exists(Input::Trigger {
                        name: "name".into(),
                        fallback: None,
                    }),
                    then_steps: vec![Step {
                        id: "message".into(),
                        on_failure: FailurePolicy::Stop,
                        kind: StepKind::Action {
                            capability: "twitch.send_message".into(),
                            version: 1,
                            inputs: BTreeMap::from([(
                                "message".into(),
                                Input::Text(vec![
                                    TextPart::Literal("Hello ".into()),
                                    TextPart::Value(Input::Trigger {
                                        name: "name".into(),
                                        fallback: None,
                                    }),
                                ]),
                            )]),
                            deadline_ms: None,
                        },
                    }],
                    else_steps: vec![],
                },
            }],
            outputs: BTreeMap::new(),
        });
        let preview = WorkflowPreview::from_definition_with_schemas(&definition, &[&SCHEMA]);
        assert_eq!(preview.revision, "Saved revision 3");
        assert_eq!(preview.triggers.len(), 1);
        assert_eq!(preview.steps[0].id.as_str(), "step:branch");
        assert_eq!(preview.steps[0].display_label, "If");
        assert!(preview.steps[0].condition.row_data(0).unwrap().is_output);
        assert!(preview.steps[0].condition.row_data(1).unwrap().is_operator);
        assert!(preview.steps[1].branch_header);
        assert_eq!(preview.steps[1].display_label, "Then");
        assert_eq!(preview.steps[2].label.as_str(), "1 › Then 1 · Send message");
        assert_eq!(preview.steps[2].depth, 1);
        assert!(preview.steps[3].branch_header);
        assert!(preview.steps[3].branch_empty);
        assert_eq!(preview.steps[3].display_label, "Else");
        assert!(preview.steps[4].group_end);
        assert_eq!(preview.steps[4].parent_id, "step:branch");
        let values = &preview.steps[2].value;
        assert!(!values.row_data(0).unwrap().is_output);
        assert!(values.row_data(1).unwrap().is_output);
    }

    #[test]
    fn preview_resolves_workflow_titles_and_marks_missing_references() {
        let definition = WorkflowDefinition::manual(Workflow {
            id: "parent".into(),
            revision: 1,
            overlap: false,
            steps: vec![
                Step {
                    id: "call".into(),
                    on_failure: FailurePolicy::Stop,
                    kind: StepKind::Call {
                        workflow_id: "child-id".into(),
                        binding: CallBinding::Explicit {
                            inputs: BTreeMap::new(),
                        },
                    },
                },
                Step {
                    id: "broken".into(),
                    on_failure: FailurePolicy::Stop,
                    kind: StepKind::SetVariable {
                        name: "target".into(),
                        value: Input::Reference {
                            step_id: "deleted".into(),
                            output_id: "login".into(),
                            fallback: None,
                        },
                    },
                },
            ],
            outputs: BTreeMap::new(),
        });
        let titles = BTreeMap::from([("child-id".into(), "Shoutout".into())]);
        let preview = WorkflowPreview::from_definition_with_titles(&definition, &[], &titles);
        assert_eq!(preview.steps[0].label, "1 · Run Shoutout");
        assert_eq!(
            preview.steps[1].value.row_data(0).unwrap().text,
            "Missing step deleted, output login"
        );
    }

    #[test]
    fn inspector_fields_follow_schema_and_hide_secret_values() {
        static FIELDS: [ConfigField; 4] = [
            ConfigField {
                id: "message",
                label: "Message",
                description: "Chat text",
                introduced_in: 1,
                kind: ConfigFieldKind::Text,
                required: true,
                choice_source: None,
            },
            ConfigField {
                id: "target",
                label: "Target",
                description: "",
                introduced_in: 1,
                kind: ConfigFieldKind::Text,
                required: true,
                choice_source: None,
            },
            ConfigField {
                id: "token",
                label: "Token",
                description: "",
                introduced_in: 1,
                kind: ConfigFieldKind::Secret,
                required: true,
                choice_source: None,
            },
            ConfigField {
                id: "optional",
                label: "Optional",
                description: "May be omitted",
                introduced_in: 1,
                kind: ConfigFieldKind::Text,
                required: false,
                choice_source: None,
            },
        ];
        static SCHEMA: ConfigSchema = ConfigSchema {
            id: "sample.action",
            version: 1,
            title: "Sample",
            fields: &FIELDS,
            outputs: &[],
        };
        let definition = WorkflowDefinition::manual(Workflow {
            id: "sample".into(),
            revision: 1,
            overlap: false,
            steps: vec![Step {
                id: "action".into(),
                on_failure: FailurePolicy::Stop,
                kind: StepKind::Action {
                    capability: "sample.action".into(),
                    version: 1,
                    inputs: BTreeMap::from([
                        ("message".into(), Input::Literal("Hello".into())),
                        (
                            "target".into(),
                            Input::Trigger {
                                name: "viewer".into(),
                                fallback: None,
                            },
                        ),
                        ("token".into(), Input::Literal("private value".into())),
                    ]),
                    deadline_ms: None,
                },
            }],
            outputs: BTreeMap::new(),
        });
        let fields = WorkflowPreview::fields_for_step(&definition, "step:action", &[&SCHEMA]);
        assert_eq!(fields.len(), 4);
        assert_eq!(fields[0].label, "Message");
        assert_eq!(fields[0].value, "Hello");
        assert!(fields[0].editable);
        assert!(fields[1].is_output);
        assert!(!fields[1].editable);
        assert_eq!(fields[2].value, "Credential reference");
        assert!(!fields[2].editable);
        assert_eq!(fields[3].value, "Not set");
        assert!(fields[3].editable);
        assert!(fields[3].optional);
        assert!(!fields[3].configured);
        let preview = WorkflowPreview::from_definition_with_schemas(&definition, &[&SCHEMA]);
        assert!(!preview.steps[0].detail.contains("private value"));
        assert_eq!(preview.steps[0].value.row_data(0).unwrap().text, "Hello");
        assert!(preview.steps[0].suffix.is_empty());
    }
}
