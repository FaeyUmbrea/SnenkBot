//! Typed drafts for control steps; saved workflows never contain a partial draft.

use std::collections::{BTreeMap, HashSet};

use serde_json::Value;

use crate::engine::{Condition, FailurePolicy, Input, Step, StepKind, validate_configured_inputs};
use crate::schema::ConfigSchema;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LiteralType {
    Text,
    Number,
    Toggle,
}

impl LiteralType {
    pub fn id(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Number => "number",
            Self::Toggle => "toggle",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        match id {
            "text" => Some(Self::Text),
            "number" => Some(Self::Number),
            "toggle" => Some(Self::Toggle),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TypedInput {
    pub kind: LiteralType,
    pub value: String,
    original: Option<Input>,
}

impl TypedInput {
    pub fn new() -> Self {
        Self {
            kind: LiteralType::Text,
            value: String::new(),
            original: None,
        }
    }

    pub fn from_input(input: &Input) -> Self {
        match input {
            Input::Literal(Value::String(value)) => Self {
                kind: LiteralType::Text,
                value: value.clone(),
                original: None,
            },
            Input::Literal(Value::Number(value)) => Self {
                kind: LiteralType::Number,
                value: value.to_string(),
                original: None,
            },
            Input::Literal(Value::Bool(value)) => Self {
                kind: LiteralType::Toggle,
                value: value.to_string(),
                original: None,
            },
            Input::Variable { name, .. } => Self {
                kind: LiteralType::Text,
                value: format!("Variable: {name}"),
                original: Some(input.clone()),
            },
            Input::Trigger { name, .. } => Self {
                kind: LiteralType::Text,
                value: format!("Trigger: {name}"),
                original: Some(input.clone()),
            },
            Input::Reference {
                step_id, output_id, ..
            } => Self {
                kind: LiteralType::Text,
                value: format!("Output: {step_id} · {output_id}"),
                original: Some(input.clone()),
            },
            _ => Self {
                kind: LiteralType::Text,
                value: "Existing structured input".into(),
                original: Some(input.clone()),
            },
        }
    }

    pub fn editable(&self) -> bool {
        self.original.is_none()
    }

    pub fn set_type(&mut self, kind: &str) -> Result<(), String> {
        if !self.editable() {
            return Err("This input uses a reference or structured value".into());
        }
        self.kind = LiteralType::from_id(kind).ok_or("Choose a supported input type")?;
        self.value = match self.kind {
            LiteralType::Text => String::new(),
            LiteralType::Number => "0".into(),
            LiteralType::Toggle => "false".into(),
        };
        Ok(())
    }

    pub fn set_value(&mut self, value: String) -> Result<(), String> {
        if !self.editable() {
            return Err("This input uses a reference or structured value".into());
        }
        self.value = value;
        Ok(())
    }

    pub fn set_input(&mut self, input: Input) {
        *self = Self::from_input(&input);
    }

    pub fn build(&self) -> Result<Input, String> {
        if let Some(original) = &self.original {
            return Ok(original.clone());
        }
        let value = match self.kind {
            LiteralType::Text => Value::String(self.value.clone()),
            LiteralType::Number => self
                .value
                .trim()
                .parse::<serde_json::Number>()
                .map(Value::Number)
                .map_err(|_| "Enter a finite number".to_owned())?,
            LiteralType::Toggle => match self.value.as_str() {
                "true" => Value::Bool(true),
                "false" => Value::Bool(false),
                _ => return Err("Choose On or Off".into()),
            },
        };
        Ok(Input::Literal(value))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConditionOperator {
    Equal,
    NotEqual,
    Exists,
    NullOrEmpty,
    Contains,
    Less,
    Greater,
}

impl ConditionOperator {
    pub fn id(self) -> &'static str {
        match self {
            Self::Equal => "equal",
            Self::NotEqual => "not_equal",
            Self::Exists => "exists",
            Self::NullOrEmpty => "null_or_empty",
            Self::Contains => "contains",
            Self::Less => "less",
            Self::Greater => "greater",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Equal => "Equals",
            Self::NotEqual => "Does not equal",
            Self::Exists => "Exists",
            Self::NullOrEmpty => "Is empty or null",
            Self::Contains => "Contains",
            Self::Less => "Is less than",
            Self::Greater => "Is greater than",
        }
    }

    pub fn all() -> &'static [Self] {
        &[
            Self::Equal,
            Self::NotEqual,
            Self::Exists,
            Self::NullOrEmpty,
            Self::Contains,
            Self::Less,
            Self::Greater,
        ]
    }

    fn from_id(id: &str) -> Option<Self> {
        Self::all()
            .iter()
            .copied()
            .find(|operator| operator.id() == id)
    }

    fn needs_right(self) -> bool {
        !matches!(self, Self::Exists | Self::NullOrEmpty)
    }
}

#[derive(Clone, Debug)]
pub enum ConditionDraft {
    Simple {
        operator: ConditionOperator,
        left: TypedInput,
        right: TypedInput,
    },
    Compound(Condition),
}

impl ConditionDraft {
    pub fn new() -> Self {
        Self::Simple {
            operator: ConditionOperator::Exists,
            left: TypedInput::new(),
            right: TypedInput::new(),
        }
    }

    pub fn from_condition(condition: &Condition) -> Self {
        let (operator, left, right) = match condition {
            Condition::Equal(left, right) => (ConditionOperator::Equal, left, Some(right)),
            Condition::NotEqual(left, right) => (ConditionOperator::NotEqual, left, Some(right)),
            Condition::Exists(left) => (ConditionOperator::Exists, left, None),
            Condition::NullOrEmpty(left) => (ConditionOperator::NullOrEmpty, left, None),
            Condition::Contains(left, right) => (ConditionOperator::Contains, left, Some(right)),
            Condition::Less(left, right) => (ConditionOperator::Less, left, Some(right)),
            Condition::Greater(left, right) => (ConditionOperator::Greater, left, Some(right)),
            _ => return Self::Compound(condition.clone()),
        };
        Self::Simple {
            operator,
            left: TypedInput::from_input(left),
            right: right.map_or_else(TypedInput::new, TypedInput::from_input),
        }
    }

    pub fn build(&self) -> Result<Condition, String> {
        let Self::Simple {
            operator,
            left,
            right,
        } = self
        else {
            let Self::Compound(condition) = self else {
                unreachable!()
            };
            return Ok(condition.clone());
        };
        let left = left
            .build()
            .map_err(|error| format!("Left input: {error}"))?;
        if !operator.needs_right() {
            return Ok(match operator {
                ConditionOperator::Exists => Condition::Exists(left),
                ConditionOperator::NullOrEmpty => Condition::NullOrEmpty(left),
                _ => unreachable!(),
            });
        }
        let right = right
            .build()
            .map_err(|error| format!("Right input: {error}"))?;
        Ok(match operator {
            ConditionOperator::Equal => Condition::Equal(left, right),
            ConditionOperator::NotEqual => Condition::NotEqual(left, right),
            ConditionOperator::Contains => Condition::Contains(left, right),
            ConditionOperator::Less => Condition::Less(left, right),
            ConditionOperator::Greater => Condition::Greater(left, right),
            _ => unreachable!(),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlKind {
    Wait,
    If,
    While,
    OneOrMore,
    SetVariable,
    Stop,
}

impl ControlKind {
    pub fn id(self) -> &'static str {
        match self {
            Self::Wait => "wait",
            Self::If => "if",
            Self::While => "while",
            Self::OneOrMore => "one_or_more",
            Self::SetVariable => "set_variable",
            Self::Stop => "stop",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Wait => "Wait",
            Self::If => "If",
            Self::While => "While",
            Self::OneOrMore => "One or More",
            Self::SetVariable => "Set variable",
            Self::Stop => "Stop",
        }
    }

    pub fn all() -> &'static [Self] {
        &[
            Self::Wait,
            Self::If,
            Self::While,
            Self::OneOrMore,
            Self::SetVariable,
            Self::Stop,
        ]
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::all().iter().copied().find(|kind| kind.id() == id)
    }
}

#[derive(Clone, Debug)]
pub struct ControlDraft {
    pub kind: ControlKind,
    pub wait_ms: String,
    pub variable_name: String,
    pub variable_value: TypedInput,
    pub condition: ConditionDraft,
    pub then_steps: Vec<Step>,
    pub else_steps: Vec<Step>,
    pub body_steps: Vec<Step>,
    pub existing: bool,
    staged_child_ids: HashSet<String>,
}

impl ControlDraft {
    pub fn new(kind: ControlKind) -> Self {
        Self {
            kind,
            wait_ms: "1000".into(),
            variable_name: String::new(),
            variable_value: TypedInput::new(),
            condition: ConditionDraft::new(),
            then_steps: Vec::new(),
            else_steps: Vec::new(),
            body_steps: Vec::new(),
            existing: false,
            staged_child_ids: HashSet::new(),
        }
    }

    pub fn from_step(kind: &StepKind) -> Option<Self> {
        let mut draft = match kind {
            StepKind::Delay { millis } => {
                let mut draft = Self::new(ControlKind::Wait);
                draft.wait_ms = millis.to_string();
                draft
            }
            StepKind::SetVariable { name, value } => {
                let mut draft = Self::new(ControlKind::SetVariable);
                draft.variable_name = name.clone();
                draft.variable_value = TypedInput::from_input(value);
                draft
            }
            StepKind::If {
                condition,
                then_steps,
                else_steps,
            } => {
                let mut draft = Self::new(ControlKind::If);
                draft.condition = ConditionDraft::from_condition(condition);
                draft.then_steps = then_steps.clone();
                draft.else_steps = else_steps.clone();
                draft
            }
            StepKind::While { condition, steps } => {
                let mut draft = Self::new(ControlKind::While);
                draft.condition = ConditionDraft::from_condition(condition);
                draft.body_steps = steps.clone();
                draft
            }
            StepKind::OneOrMore { steps } => {
                let mut draft = Self::new(ControlKind::OneOrMore);
                draft.body_steps = steps.clone();
                draft
            }
            StepKind::Stop => Self::new(ControlKind::Stop),
            _ => return None,
        };
        draft.existing = true;
        Some(draft)
    }

    pub fn set_field(&mut self, id: &str, value: String) -> Result<(), String> {
        match (self.kind, id) {
            (ControlKind::Wait, "duration") => self.wait_ms = value,
            (ControlKind::SetVariable, "name") => self.variable_name = value,
            (ControlKind::SetVariable, "value") => self.variable_value.set_value(value)?,
            (ControlKind::SetVariable, "value_type") => self.variable_value.set_type(&value)?,
            (ControlKind::If | ControlKind::While, "operator") => {
                let ConditionDraft::Simple { operator, .. } = &mut self.condition else {
                    return Err("Compound conditions are read-only here".into());
                };
                *operator =
                    ConditionOperator::from_id(&value).ok_or("Choose a supported condition")?;
            }
            (ControlKind::If | ControlKind::While, "left") => {
                let ConditionDraft::Simple { left, .. } = &mut self.condition else {
                    return Err("Compound conditions are read-only here".into());
                };
                left.set_value(value)?;
            }
            (ControlKind::If | ControlKind::While, "left_type") => {
                let ConditionDraft::Simple { left, .. } = &mut self.condition else {
                    return Err("Compound conditions are read-only here".into());
                };
                left.set_type(&value)?;
            }
            (ControlKind::If | ControlKind::While, "right") => {
                let ConditionDraft::Simple {
                    operator, right, ..
                } = &mut self.condition
                else {
                    return Err("Compound conditions are read-only here".into());
                };
                if !operator.needs_right() {
                    return Err("This condition has one input".into());
                }
                right.set_value(value)?;
            }
            (ControlKind::If | ControlKind::While, "right_type") => {
                let ConditionDraft::Simple {
                    operator, right, ..
                } = &mut self.condition
                else {
                    return Err("Compound conditions are read-only here".into());
                };
                if !operator.needs_right() {
                    return Err("This condition has one input".into());
                }
                right.set_type(&value)?;
            }
            _ => return Err("This control field is unavailable".into()),
        }
        Ok(())
    }

    /// Replaces one complete operand. Reference selection never mutates text inside an input.
    pub fn set_input(&mut self, slot: &str, input: Input) -> Result<(), String> {
        let target = match (self.kind, slot) {
            (ControlKind::SetVariable, "value") => &mut self.variable_value,
            (ControlKind::If | ControlKind::While, "left") => {
                let ConditionDraft::Simple { left, .. } = &mut self.condition else {
                    return Err("Compound conditions are read-only here".into());
                };
                left
            }
            (ControlKind::If | ControlKind::While, "right") => {
                let ConditionDraft::Simple {
                    operator, right, ..
                } = &mut self.condition
                else {
                    return Err("Compound conditions are read-only here".into());
                };
                if !operator.needs_right() {
                    return Err("This condition has one input".into());
                }
                right
            }
            _ => return Err("This control input is unavailable".into()),
        };
        target.set_input(input);
        Ok(())
    }

    pub fn stage_action(
        &mut self,
        branch: &str,
        schema: &'static ConfigSchema,
        inputs: BTreeMap<String, Input>,
    ) -> Result<String, String> {
        validate_configured_inputs(schema, &inputs)?;
        self.stage_kind(
            branch,
            StepKind::Action {
                capability: schema.id.to_owned(),
                version: schema.version,
                inputs,
                deadline_ms: None,
            },
        )
    }

    /// Stages a typed child without saving the enclosing control.
    pub fn stage_kind(&mut self, branch: &str, kind: StepKind) -> Result<String, String> {
        let target = match (self.kind, branch) {
            (ControlKind::If, "then") => &mut self.then_steps,
            (ControlKind::If, "else") => &mut self.else_steps,
            (ControlKind::While | ControlKind::OneOrMore, "body") => &mut self.body_steps,
            _ => return Err("This container has no such branch".into()),
        };
        let id = uuid::Uuid::new_v4().to_string();
        target.push(Step {
            id: id.clone(),
            on_failure: FailurePolicy::Stop,
            kind,
        });
        self.staged_child_ids.insert(id.clone());
        Ok(id)
    }

    pub fn can_remove_child(&self, id: &str) -> bool {
        !self.existing || self.staged_child_ids.contains(id)
    }

    pub fn remove_staged_child(&mut self, id: &str) -> bool {
        if !self.can_remove_child(id) {
            return false;
        }
        for steps in [
            &mut self.then_steps,
            &mut self.else_steps,
            &mut self.body_steps,
        ] {
            if let Some(index) = steps.iter().position(|step| step.id == id) {
                steps.remove(index);
                self.staged_child_ids.remove(id);
                return true;
            }
        }
        false
    }

    pub fn build(&self) -> Result<StepKind, String> {
        match self.kind {
            ControlKind::Wait => {
                let millis = self
                    .wait_ms
                    .parse::<u64>()
                    .map_err(|_| "Wait duration must be a whole number of milliseconds")?;
                if millis == 0 {
                    return Err("Wait duration must be greater than zero".into());
                }
                Ok(StepKind::Delay { millis })
            }
            ControlKind::SetVariable => {
                let name = self.variable_name.trim();
                if name.is_empty() {
                    return Err("Variable name is required".into());
                }
                Ok(StepKind::SetVariable {
                    name: name.to_owned(),
                    value: self.variable_value.build()?,
                })
            }
            ControlKind::If => Ok(StepKind::If {
                condition: self.condition.build()?,
                then_steps: self.then_steps.clone(),
                else_steps: self.else_steps.clone(),
            }),
            ControlKind::While => Ok(StepKind::While {
                condition: self.condition.build()?,
                steps: self.body_steps.clone(),
            }),
            ControlKind::OneOrMore => {
                if self.body_steps.is_empty() {
                    return Err("Add a step before saving One or More".into());
                }
                Ok(StepKind::OneOrMore {
                    steps: self.body_steps.clone(),
                })
            }
            ControlKind::Stop => Ok(StepKind::Stop),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ConditionDraft, ControlDraft, ControlKind, LiteralType, TypedInput};
    use crate::engine::{Condition, FailurePolicy, Input, Step, StepKind};
    use crate::schema::ConfigSchema;
    use serde_json::json;
    use std::collections::BTreeMap;

    #[test]
    fn typed_literals_build_without_json_entry() {
        let mut input = TypedInput::new();
        input.set_type("number").unwrap();
        input.set_value("42".into()).unwrap();
        assert!(matches!(input.build(), Ok(Input::Literal(value)) if value == json!(42)));
        input.set_value("0.75".into()).unwrap();
        assert!(matches!(input.build(), Ok(Input::Literal(value)) if value == json!(0.75)));
        input.set_value("1e400".into()).unwrap();
        assert!(input.build().is_err());
        let original = TypedInput::from_input(&Input::Literal(json!(0.75)));
        assert!(matches!(original.build(), Ok(Input::Literal(value)) if value == json!(0.75)));
        input.set_type("toggle").unwrap();
        input.set_value("true".into()).unwrap();
        assert!(matches!(input.build(), Ok(Input::Literal(value)) if value == json!(true)));
        assert_eq!(input.kind, LiteralType::Toggle);
    }

    #[test]
    fn existing_reference_and_compound_condition_round_trip() {
        let reference = Input::Variable {
            name: "viewer".into(),
            fallback: None,
        };
        let condition = Condition::Not(Box::new(Condition::Exists(reference.clone())));
        let draft = ConditionDraft::from_condition(&condition);
        assert!(matches!(draft.build(), Ok(Condition::Not(_))));
        let input = TypedInput::from_input(&reference);
        assert!(!input.editable());
        assert!(matches!(input.build(), Ok(Input::Variable { name, .. }) if name == "viewer"));
    }

    #[test]
    fn one_or_more_requires_a_staged_child() {
        static SCHEMA: ConfigSchema = ConfigSchema {
            id: "test.action",
            version: 1,
            title: "Test action",
            fields: &[],
            outputs: &[],
        };
        let mut draft = ControlDraft::new(ControlKind::OneOrMore);
        assert!(draft.build().is_err());
        let id = draft
            .stage_action("body", &SCHEMA, BTreeMap::new())
            .unwrap();
        assert!(
            matches!(draft.build(), Ok(StepKind::OneOrMore { steps }) if steps.len() == 1 && steps[0].id == id)
        );
        assert!(draft.remove_staged_child(&id));
        assert!(draft.build().is_err());
        let existing = ControlDraft::from_step(&StepKind::Stop).unwrap();
        assert!(matches!(existing.build(), Ok(StepKind::Stop)));
    }

    #[test]
    fn existing_container_preserves_saved_children_and_can_remove_new_child() {
        let saved = Step {
            id: "saved-child".into(),
            on_failure: FailurePolicy::Stop,
            kind: StepKind::Stop,
        };
        let mut draft =
            ControlDraft::from_step(&StepKind::OneOrMore { steps: vec![saved] }).unwrap();
        let new_id = draft
            .stage_kind("body", StepKind::Delay { millis: 10 })
            .unwrap();
        assert!(!draft.can_remove_child("saved-child"));
        assert!(!draft.remove_staged_child("saved-child"));
        assert!(draft.can_remove_child(&new_id));
        assert!(draft.remove_staged_child(&new_id));
        assert!(
            matches!(draft.build(), Ok(StepKind::OneOrMore { steps }) if steps.len() == 1 && steps[0].id == "saved-child")
        );
    }

    #[test]
    fn editing_condition_preserves_existing_children_and_reference() {
        let child = Step {
            id: "child".into(),
            on_failure: FailurePolicy::Continue,
            kind: StepKind::Stop,
        };
        let kind = StepKind::If {
            condition: Condition::Exists(Input::Variable {
                name: "viewer".into(),
                fallback: None,
            }),
            then_steps: vec![child],
            else_steps: Vec::new(),
        };
        let mut draft = ControlDraft::from_step(&kind).unwrap();
        assert!(draft.set_input("right", Input::Literal(json!(1))).is_err());
        assert!(
            matches!(draft.build(), Ok(StepKind::If { condition: Condition::Exists(Input::Variable { name, .. }), then_steps, .. }) if name == "viewer" && then_steps[0].id == "child")
        );
    }
}
