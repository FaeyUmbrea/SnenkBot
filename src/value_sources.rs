//! Values available at one workflow step, without exposing future or sibling-branch outputs.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::engine::{FailurePolicy, Input, Step, StepKind};
use crate::schema::{ConfigOutput, ConfigOutputKind, ConfigSchema};
use crate::workflows::{TriggerKind, WorkflowDefinition};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum ValueSourceKind {
    Trigger,
    Variable,
    Step,
}

impl ValueSourceKind {
    pub fn id(self) -> &'static str {
        match self {
            Self::Trigger => "trigger",
            Self::Variable => "variable",
            Self::Step => "step",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub struct ValueSource {
    pub source: ValueSourceKind,
    pub source_id: String,
    pub output_id: String,
    pub label: String,
    pub detail: String,
    pub optional: bool,
    pub fallback_kind: &'static str,
}

impl ValueSource {
    pub fn input(&self, fallback: Option<Input>) -> Result<Input, String> {
        if self.optional && fallback.is_none() {
            return Err("Choose a fallback for this value".into());
        }
        let fallback = fallback.map(Box::new);
        Ok(match self.source {
            ValueSourceKind::Trigger => Input::Trigger {
                name: self.source_id.clone(),
                fallback,
            },
            ValueSourceKind::Variable => Input::Variable {
                name: self.source_id.clone(),
                fallback,
            },
            ValueSourceKind::Step => Input::Reference {
                step_id: self.source_id.clone(),
                output_id: self.output_id.clone(),
                fallback,
            },
        })
    }
}

#[derive(Clone)]
struct ScopedStep<'a> {
    step: &'a Step,
    uncertain: bool,
}

pub fn available_sources(
    definition: &WorkflowDefinition,
    target_step: Option<&str>,
    schemas: &[&ConfigSchema],
    trigger_outputs: impl Fn(&TriggerKind) -> Option<Vec<ConfigOutput>>,
) -> Result<Vec<ValueSource>, String> {
    let mut before = Vec::new();
    if let Some(target_step) = target_step {
        if !collect_before(&definition.workflow.steps, target_step, false, &mut before) {
            return Err("This control step is unavailable".into());
        }
    } else {
        before.extend(definition.workflow.steps.iter().map(|step| ScopedStep {
            step,
            uncertain: false,
        }));
    }
    let mut sources = trigger_sources(definition, trigger_outputs);
    let mut variables = Vec::new();
    for scoped in before {
        let optional = scoped.uncertain || scoped.step.on_failure == FailurePolicy::Continue;
        match &scoped.step.kind {
            StepKind::Action {
                capability,
                version,
                ..
            } => {
                let Some(schema) = schemas
                    .iter()
                    .find(|schema| schema.id == capability && schema.version == *version)
                else {
                    continue;
                };
                for output in schema.outputs {
                    sources.push(ValueSource {
                        source: ValueSourceKind::Step,
                        source_id: scoped.step.id.clone(),
                        output_id: output.id.into(),
                        label: output.label.into(),
                        detail: format!("{} · {}", schema.title, output.description),
                        optional: optional || !output.required,
                        fallback_kind: fallback_kind(output.kind),
                    });
                }
            }
            StepKind::RequestInput { title, fields } => {
                for field in fields {
                    sources.push(ValueSource {
                        source: ValueSourceKind::Step,
                        source_id: scoped.step.id.clone(),
                        output_id: field.id.clone(),
                        label: field.label.clone().unwrap_or_else(|| field.id.clone()),
                        detail: title.clone().unwrap_or_else(|| "Ask for input".into()),
                        optional: optional || !field.required,
                        fallback_kind: "text",
                    });
                }
            }
            StepKind::SetVariable { name, value } if !name.is_empty() => {
                let kind = input_kind(value, &sources, &variables);
                variables.retain(|source: &ValueSource| source.source_id != *name);
                variables.push(ValueSource {
                    source: ValueSourceKind::Variable,
                    source_id: name.clone(),
                    output_id: String::new(),
                    label: name.clone(),
                    detail: "Variable".into(),
                    optional: optional || kind.is_empty(),
                    fallback_kind: kind,
                });
            }
            _ => {}
        }
    }
    sources.extend(variables);
    Ok(sources)
}

fn collect_before<'a>(
    steps: &'a [Step],
    target: &str,
    uncertain: bool,
    before: &mut Vec<ScopedStep<'a>>,
) -> bool {
    for step in steps {
        if step.id == target {
            return true;
        }
        match &step.kind {
            StepKind::If {
                then_steps,
                else_steps,
                ..
            } => {
                let mut path = before.clone();
                if collect_before(then_steps, target, uncertain, &mut path) {
                    *before = path;
                    return true;
                }
                let mut path = before.clone();
                if collect_before(else_steps, target, uncertain, &mut path) {
                    *before = path;
                    return true;
                }
            }
            StepKind::While { steps, .. } => {
                let mut path = before.clone();
                if collect_before(steps, target, uncertain, &mut path) {
                    *before = path;
                    return true;
                }
            }
            StepKind::OneOrMore { steps } => {
                let mut path = before.clone();
                if collect_before(steps, target, true, &mut path) {
                    *before = path;
                    return true;
                }
            }
            _ => {}
        }
        before.push(ScopedStep { step, uncertain });
    }
    false
}

fn trigger_sources(
    definition: &WorkflowDefinition,
    trigger_outputs: impl Fn(&TriggerKind) -> Option<Vec<ConfigOutput>>,
) -> Vec<ValueSource> {
    let mut outputs: BTreeMap<&'static str, (ConfigOutput, bool)> = BTreeMap::new();
    for trigger in &definition.triggers {
        let metadata = trigger_outputs(&trigger.kind).unwrap_or_default();
        for output in metadata {
            let entry = outputs.entry(output.id).or_insert((output, false));
            entry.1 |= entry.0.kind != output.kind;
        }
    }
    outputs
        .into_iter()
        .filter(|(_, (_, conflict))| !conflict)
        .map(|(id, (output, _))| ValueSource {
            source: ValueSourceKind::Trigger,
            source_id: id.into(),
            output_id: String::new(),
            label: output.label.into(),
            detail: output.description.into(),
            // Manual activation and other configured triggers do not provide this value.
            optional: true,
            fallback_kind: fallback_kind(output.kind),
        })
        .collect()
}

fn fallback_kind(kind: ConfigOutputKind) -> &'static str {
    match kind {
        ConfigOutputKind::Text => "text",
        ConfigOutputKind::Number => "number",
        ConfigOutputKind::Toggle => "toggle",
    }
}

fn input_kind(input: &Input, sources: &[ValueSource], variables: &[ValueSource]) -> &'static str {
    match input {
        Input::Literal(serde_json::Value::Number(_)) => "number",
        Input::Literal(serde_json::Value::Bool(_)) => "toggle",
        Input::Literal(serde_json::Value::String(_)) | Input::Text(_) => "text",
        Input::Reference {
            step_id, output_id, ..
        } => sources
            .iter()
            .find(|source| {
                source.source == ValueSourceKind::Step
                    && source.source_id == *step_id
                    && source.output_id == *output_id
            })
            .map_or("", |source| source.fallback_kind),
        Input::Trigger { name, .. } => sources
            .iter()
            .find(|source| source.source == ValueSourceKind::Trigger && source.source_id == *name)
            .map_or("", |source| source.fallback_kind),
        Input::Variable { name, .. } => variables
            .iter()
            .find(|source| source.source_id == *name)
            .map_or("", |source| source.fallback_kind),
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::{ValueSource, ValueSourceKind, available_sources};
    use crate::engine::{FailurePolicy, Input, Step, StepKind, Workflow};
    use crate::schema::{ConfigOutput, ConfigOutputKind, ConfigSchema};
    use crate::workflows::{TriggerDefinition, TriggerKind, WorkflowDefinition};
    use serde_json::json;
    use std::collections::BTreeMap;

    fn step(id: &str, kind: StepKind) -> Step {
        Step {
            id: id.into(),
            on_failure: FailurePolicy::Stop,
            kind,
        }
    }

    #[test]
    fn path_scope_excludes_future_and_other_branch_variables() {
        let definition = WorkflowDefinition::manual(Workflow {
            id: "test".into(),
            revision: 1,
            overlap: false,
            steps: vec![
                step(
                    "first",
                    StepKind::SetVariable {
                        name: "before".into(),
                        value: Input::Literal(json!(1)),
                    },
                ),
                step(
                    "branch",
                    StepKind::If {
                        condition: crate::engine::Condition::Exists(Input::Literal(json!(true))),
                        then_steps: vec![step(
                            "then",
                            StepKind::SetVariable {
                                name: "then_only".into(),
                                value: Input::Literal(json!("yes")),
                            },
                        )],
                        else_steps: vec![step("target", StepKind::Stop)],
                    },
                ),
                step(
                    "future",
                    StepKind::SetVariable {
                        name: "later".into(),
                        value: Input::Literal(json!(2)),
                    },
                ),
            ],
            outputs: BTreeMap::new(),
        });
        let sources = available_sources(&definition, Some("target"), &[], |_| None).unwrap();
        assert!(sources.iter().any(|source| source.source_id == "before"));
        assert!(!sources.iter().any(|source| source.source_id == "then_only"));
        assert!(!sources.iter().any(|source| source.source_id == "later"));
    }

    #[test]
    fn preceding_action_uses_stable_output_ids_and_marks_optional_output() {
        static OUTPUTS: [ConfigOutput; 2] = [
            ConfigOutput {
                id: "viewer_id",
                label: "Viewer",
                description: "The viewer identifier",
                kind: ConfigOutputKind::Text,
                required: false,
            },
            ConfigOutput {
                id: "count",
                label: "Count",
                description: "A number",
                kind: ConfigOutputKind::Number,
                required: true,
            },
        ];
        static SCHEMA: ConfigSchema = ConfigSchema {
            id: "test.action",
            version: 1,
            title: "Test action",
            fields: &[],
            outputs: &OUTPUTS,
        };
        let definition = WorkflowDefinition::manual(Workflow {
            id: "test".into(),
            revision: 1,
            overlap: false,
            steps: vec![
                step(
                    "source-step",
                    StepKind::Action {
                        capability: "test.action".into(),
                        version: 1,
                        inputs: BTreeMap::new(),
                        deadline_ms: None,
                    },
                ),
                step(
                    "numeric-variable",
                    StepKind::SetVariable {
                        name: "count".into(),
                        value: Input::Reference {
                            step_id: "source-step".into(),
                            output_id: "count".into(),
                            fallback: Some(Box::new(Input::Literal(json!(0)))),
                        },
                    },
                ),
                step(
                    "unknown-variable",
                    StepKind::SetVariable {
                        name: "unknown".into(),
                        value: Input::Literal(json!(null)),
                    },
                ),
                step("target", StepKind::Stop),
            ],
            outputs: BTreeMap::new(),
        });
        let sources = available_sources(&definition, Some("target"), &[&SCHEMA], |_| None).unwrap();
        let source = sources
            .iter()
            .find(|source| source.source_id == "source-step")
            .unwrap();
        assert_eq!(source.output_id, "viewer_id");
        assert_eq!(source.label, "Viewer");
        assert!(source.optional);
        assert!(source.input(None).is_err());
        let numeric = sources
            .iter()
            .find(|source| source.source_id == "count")
            .unwrap();
        assert_eq!(numeric.fallback_kind, "number");
        let unknown = sources
            .iter()
            .find(|source| source.source_id == "unknown")
            .unwrap();
        assert_eq!(unknown.fallback_kind, "");
        assert!(unknown.optional);
    }

    #[test]
    fn configured_disabled_trigger_value_is_discoverable_with_fallback() {
        let mut definition = WorkflowDefinition::manual(Workflow {
            id: "test".into(),
            revision: 1,
            overlap: false,
            steps: vec![],
            outputs: BTreeMap::new(),
        });
        definition.triggers.push(TriggerDefinition {
            id: "disabled".into(),
            enabled: false,
            kind: TriggerKind::ObsRecordingStarted,
        });
        let sources = available_sources(&definition, None, &[], |kind| {
            matches!(kind, TriggerKind::ObsRecordingStarted).then_some(vec![ConfigOutput {
                id: "recording_name",
                label: "Recording name",
                description: "Current recording",
                kind: ConfigOutputKind::Text,
                required: true,
            }])
        })
        .unwrap();
        let source = sources
            .iter()
            .find(|source| source.source_id == "recording_name")
            .unwrap();
        assert!(source.optional);
        assert!(source.input(None).is_err());
    }

    #[test]
    fn optional_source_requires_explicit_fallback() {
        let source = ValueSource {
            source: ValueSourceKind::Step,
            source_id: "step-id".into(),
            output_id: "value-id".into(),
            label: "Value".into(),
            detail: String::new(),
            optional: true,
            fallback_kind: "text",
        };
        assert!(source.input(None).is_err());
        assert!(matches!(
            source.input(Some(Input::Literal(json!("fallback")))),
            Ok(Input::Reference { step_id, output_id, fallback: Some(_) })
                if step_id == "step-id" && output_id == "value-id"
        ));
    }
}
