//! Ordered workflow execution independent of UI and service connectors.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Mutex, Notify};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use crate::schema::{ConfigFieldKind, ConfigOutputKind, ConfigSchema};

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
pub type Values = BTreeMap<String, Value>;

pub const MAX_LOOP_ITERATIONS: usize = 1_000;
pub const MAX_STEP_STARTS: usize = 10_000;
pub const MAX_CALL_DEPTH: usize = 16;
pub const MAX_PENDING_PER_WORKFLOW: usize = 1_024;
pub const MAX_PENDING_GLOBAL: usize = 8_192;
pub const MAX_ACTIVE_PER_WORKFLOW: usize = 64;
pub const MAX_ACTIVE_GLOBAL: usize = 128;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub struct Workflow {
    pub id: String,
    // The desktop transport carries Serde JSON integers as JavaScript numbers.
    #[cfg_attr(feature = "desktop-contracts", specta(type = specta_typescript::Number))]
    pub revision: u64,
    pub overlap: bool,
    pub steps: Vec<Step>,
    pub outputs: BTreeMap<String, Input>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub struct Step {
    pub id: String,
    pub on_failure: FailurePolicy,
    pub kind: StepKind,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub enum FailurePolicy {
    #[default]
    Stop,
    Continue,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub enum StepKind {
    Action {
        capability: String,
        version: u32,
        inputs: BTreeMap<String, Input>,
        #[cfg_attr(feature = "desktop-contracts", specta(type = Option<specta_typescript::Number>))]
        deadline_ms: Option<u64>,
    },
    SetVariable {
        name: String,
        value: Input,
    },
    If {
        condition: Condition,
        then_steps: Vec<Step>,
        else_steps: Vec<Step>,
    },
    While {
        condition: Condition,
        steps: Vec<Step>,
    },
    OneOrMore {
        steps: Vec<Step>,
    },
    Delay {
        #[cfg_attr(feature = "desktop-contracts", specta(type = specta_typescript::Number))]
        millis: u64,
    },
    RequestInput {
        #[serde(default)]
        title: Option<String>,
        fields: Vec<InputField>,
    },
    Call {
        workflow_id: String,
        binding: CallBinding,
    },
    Stop,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub struct InputField {
    pub id: String,
    #[serde(default)]
    pub label: Option<String>,
    pub default: Option<Input>,
    pub required: bool,
    /// Integration-owned validator key; its result becomes this field's output.
    #[serde(default)]
    pub validator: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub enum CallBinding {
    Explicit { inputs: BTreeMap<String, Input> },
    AllNamedArguments,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub enum Input {
    Literal(
        #[cfg_attr(feature = "desktop-contracts", specta(type = specta_typescript::Unknown))] Value,
    ),
    Object(BTreeMap<String, Input>),
    Array(Vec<Input>),
    Reference {
        step_id: String,
        output_id: String,
        fallback: Option<Box<Input>>,
    },
    Trigger {
        name: String,
        fallback: Option<Box<Input>>,
    },
    Variable {
        name: String,
        fallback: Option<Box<Input>>,
    },
    Text(Vec<TextPart>),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub enum TextPart {
    Literal(String),
    Value(Input),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub enum Condition {
    Equal(Input, Input),
    NotEqual(Input, Input),
    Exists(Input),
    NullOrEmpty(Input),
    Contains(Input, Input),
    Less(Input, Input),
    Greater(Input, Input),
    All(Vec<Condition>),
    Any(Vec<Condition>),
    Not(Box<Condition>),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    InvalidDefinition,
    MissingValue,
    InvalidType,
    CapabilityUnavailable,
    ConnectorUnavailable,
    Action,
    Timeout,
    Input,
    LoopLimit,
    StepLimit,
    CallDepth,
    CalledWorkflow,
    NoChildSucceeded,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub struct Failure {
    pub step_id: String,
    pub kind: FailureKind,
    pub message: String,
    pub remote_effect_uncertain: bool,
}

impl Failure {
    fn new(step_id: &str, kind: FailureKind, message: impl Into<String>) -> Self {
        Self {
            step_id: step_id.to_owned(),
            kind,
            message: message.into(),
            remote_effect_uncertain: false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub enum Outcome {
    Success,
    Stopped,
    Cancelled,
    Failed(Failure),
    Rejected(String),
    Interrupted,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub enum Event {
    StepStarted {
        workflow_id: String,
        step_id: String,
    },
    StepFinished {
        workflow_id: String,
        step_id: String,
        outcome: Outcome,
    },
    VariableReplaced {
        workflow_id: String,
        step_id: String,
        name: String,
    },
    ActivationRejected {
        workflow_id: String,
        #[cfg_attr(feature = "desktop-contracts", specta(type = specta_typescript::Number))]
        revision: u64,
        reason: String,
    },
}

pub trait EventSink: Send + Sync {
    fn record(&self, event: Event);
}

impl<F: Fn(Event) + Send + Sync> EventSink for F {
    fn record(&self, event: Event) {
        self(event);
    }
}

#[derive(Clone, Default)]
pub struct Environment {
    pub trigger: Values,
    pub variables: Values,
    pub outputs: BTreeMap<String, Values>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CapabilityError {
    ConnectorUnavailable(String),
    Failed(String),
    /// The remote service may have applied the action before the connection failed.
    Uncertain(String),
}

/// Registered implementations are compiled into the application. They receive a private input snapshot.
pub trait Capability: Send + Sync {
    fn default_deadline(&self) -> Duration;
    fn validate_inputs(&self, _inputs: &BTreeMap<String, Input>) -> Result<(), String> {
        Ok(())
    }
    fn ready(&self) -> Result<(), String> {
        Ok(())
    }
    fn execute<'a>(
        &'a self,
        inputs: Values,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<Values, CapabilityError>>;
}

pub fn validate_configured_inputs(
    schema: &ConfigSchema,
    inputs: &BTreeMap<String, Input>,
) -> Result<(), String> {
    for field in schema.fields {
        let Some(input) = inputs.get(field.id) else {
            if field.required {
                return Err(format!("missing `{}` input", field.label));
            }
            continue;
        };
        let matches_kind = match input {
            Input::Literal(value) if value.is_null() && !field.required => true,
            Input::Literal(value) => match field.kind {
                ConfigFieldKind::Text | ConfigFieldKind::Secret => value.is_string(),
                ConfigFieldKind::Integer => value.as_i64().is_some() || value.as_u64().is_some(),
                ConfigFieldKind::Toggle => value.is_boolean(),
            },
            Input::Text(_) => matches!(field.kind, ConfigFieldKind::Text | ConfigFieldKind::Secret),
            Input::Object(_) | Input::Array(_) => false,
            Input::Reference { .. } | Input::Trigger { .. } | Input::Variable { .. } => true,
        };
        if !matches_kind {
            return Err(format!("`{}` input has the wrong type", field.label));
        }
    }
    if let Some(unexpected) = inputs
        .keys()
        .find(|key| !schema.fields.iter().any(|field| field.id == key.as_str()))
    {
        return Err(format!("unexpected `{unexpected}` input"));
    }
    Ok(())
}

fn validate_declared_outputs(schema: &ConfigSchema, outputs: &Values) -> Result<(), String> {
    for output in schema.outputs {
        let Some(value) = outputs.get(output.id) else {
            if output.required {
                return Err(format!("missing declared output `{}`", output.id));
            }
            continue;
        };
        let valid = match output.kind {
            ConfigOutputKind::Text => value.is_string(),
            ConfigOutputKind::Number => value.is_number(),
            ConfigOutputKind::Toggle => value.is_boolean(),
        };
        if !valid {
            return Err(format!(
                "declared output `{}` has the wrong type",
                output.id
            ));
        }
    }
    if let Some(unexpected) = outputs
        .keys()
        .find(|id| !schema.outputs.iter().any(|output| output.id == id.as_str()))
    {
        return Err(format!("undeclared output `{unexpected}`"));
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputResponse {
    Applied(Values),
    Cancelled,
}

pub trait InputProvider: Send + Sync {
    fn request<'a>(
        &'a self,
        title: String,
        fields: Vec<InputField>,
        defaults: Values,
        validators: &'a FormValidators,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<InputResponse, String>>;
}

pub trait FormValidator: Send + Sync {
    fn validate<'a>(
        &'a self,
        value: &'a str,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<String, String>>;
}

#[derive(Default)]
pub struct FormValidators {
    providers: BTreeMap<String, Arc<dyn FormValidator>>,
}

impl FormValidators {
    pub fn register(&mut self, id: impl Into<String>, provider: Arc<dyn FormValidator>) {
        let id = id.into();
        assert!(
            self.providers.insert(id.clone(), provider).is_none(),
            "duplicate form validator: {id}"
        );
    }

    pub async fn validate(
        &self,
        fields: &[InputField],
        values: &Values,
        cancel: CancellationToken,
    ) -> Result<Values, String> {
        if let Some(id) = values
            .keys()
            .find(|id| !fields.iter().any(|field| &field.id == *id))
        {
            return Err(format!("unknown form field `{id}`"));
        }
        let mut result = Values::new();
        for field in fields {
            let value = match values.get(&field.id) {
                Some(Value::String(value)) => value.as_str(),
                None | Some(Value::Null) => "",
                Some(_) => {
                    return Err(format!(
                        "{} must be text",
                        field.label.as_deref().unwrap_or(&field.id)
                    ));
                }
            };
            if field.required && value.trim().is_empty() {
                return Err(format!(
                    "{} is required",
                    field.label.as_deref().unwrap_or(&field.id)
                ));
            }
            let validated = if let Some(key) = &field.validator {
                let provider = self
                    .providers
                    .get(key)
                    .ok_or_else(|| format!("form validator `{key}` is unavailable"))?;
                provider
                    .validate(value, cancel.child_token())
                    .await
                    .map_err(|error| {
                        format!("{}: {error}", field.label.as_deref().unwrap_or(&field.id))
                    })?
            } else {
                value.to_owned()
            };
            result.insert(field.id.clone(), Value::String(validated));
        }
        Ok(result)
    }

    fn contains(&self, id: &str) -> bool {
        self.providers.contains_key(id)
    }
}

pub struct Engine {
    workflows: RwLock<HashMap<String, Arc<Workflow>>>,
    capabilities: HashMap<(String, u32), Arc<dyn Capability>>,
    action_schemas: BTreeMap<(String, u32), &'static ConfigSchema>,
    action_definitions: BTreeMap<(String, u32), crate::schema::ActionDefinition>,
    lua_bindings: BTreeMap<String, Arc<dyn Capability>>,
    input: Arc<dyn InputProvider>,
    form_validators: FormValidators,
    events: Arc<dyn EventSink>,
}

#[derive(Clone)]
pub struct ExecutionPlan {
    pub workflow: Arc<Workflow>,
    workflows: HashMap<String, Arc<Workflow>>,
}

pub struct RunResult {
    pub outcome: Outcome,
    pub environment: Environment,
    pub declared_outputs: Values,
    pub failures: Vec<Failure>,
    pub script_activity: Vec<ScriptActivity>,
    pub step_trace: Vec<StepTrace>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub struct StepTrace {
    /// Monotonic within this run. Repeated loop executions get distinct numbers.
    #[cfg_attr(feature = "desktop-contracts", specta(type = specta_typescript::Number))]
    pub sequence: u64,
    #[cfg_attr(feature = "desktop-contracts", specta(type = Option<specta_typescript::Number>))]
    pub parent_sequence: Option<u64>,
    pub workflow_id: String,
    #[cfg_attr(feature = "desktop-contracts", specta(type = specta_typescript::Number))]
    pub workflow_revision: u64,
    pub step_id: String,
    pub kind: StepTraceKind,
    #[cfg_attr(feature = "desktop-contracts", specta(type = specta_typescript::Number))]
    pub started_at_ms: u64,
    #[cfg_attr(feature = "desktop-contracts", specta(type = specta_typescript::Number))]
    pub finished_at_ms: u64,
    #[cfg_attr(feature = "desktop-contracts", specta(type = specta_typescript::Number))]
    pub duration_ms: u64,
    pub outcome: StepTraceOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StepTraceKind {
    Action { capability: String, version: u32 },
    SetVariable,
    If,
    While,
    OneOrMore,
    Delay,
    RequestInput,
    Call { workflow_id: String },
    Stop,
}

impl From<&StepKind> for StepTraceKind {
    fn from(kind: &StepKind) -> Self {
        match kind {
            StepKind::Action {
                capability,
                version,
                ..
            } => Self::Action {
                capability: capability.clone(),
                version: *version,
            },
            StepKind::SetVariable { .. } => Self::SetVariable,
            StepKind::If { .. } => Self::If,
            StepKind::While { .. } => Self::While,
            StepKind::OneOrMore { .. } => Self::OneOrMore,
            StepKind::Delay { .. } => Self::Delay,
            StepKind::RequestInput { .. } => Self::RequestInput,
            StepKind::Call { workflow_id, .. } => Self::Call {
                workflow_id: workflow_id.clone(),
            },
            StepKind::Stop => Self::Stop,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum StepTraceOutcome {
    Succeeded,
    Stopped,
    Cancelled,
    Failed {
        kind: FailureKind,
        remote_effect_uncertain: bool,
        continued_by_policy: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub struct ScriptActivity {
    pub workflow_id: String,
    pub step_id: String,
    pub outcome: ScriptOutcome,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum ScriptOutcome {
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy)]
struct ExecutionContext<'a> {
    workflow: &'a Workflow,
    workflows: &'a HashMap<String, Arc<Workflow>>,
    call_depth: usize,
    parent_sequence: Option<u64>,
}

impl<'a> ExecutionContext<'a> {
    fn nested(self, parent_sequence: u64) -> Self {
        Self {
            parent_sequence: Some(parent_sequence),
            ..self
        }
    }

    fn called(self, workflow: &'a Workflow, parent_sequence: u64) -> Self {
        Self {
            workflow,
            call_depth: self.call_depth + 1,
            parent_sequence: Some(parent_sequence),
            ..self
        }
    }
}

struct RunState {
    starts: usize,
    failures: Vec<Failure>,
    script_activity: Vec<ScriptActivity>,
    step_trace: Vec<StepTrace>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Flow {
    Continue,
    Stop,
    Cancel,
    Fail(Failure),
}

impl Engine {
    pub fn new(input: Arc<dyn InputProvider>, events: Arc<dyn EventSink>) -> Self {
        Self {
            workflows: RwLock::new(HashMap::new()),
            capabilities: HashMap::new(),
            action_schemas: BTreeMap::new(),
            action_definitions: BTreeMap::new(),
            lua_bindings: BTreeMap::new(),
            input,
            form_validators: FormValidators::default(),
            events,
        }
    }

    pub fn register_form_validator(
        &mut self,
        id: impl Into<String>,
        validator: Arc<dyn FormValidator>,
    ) {
        self.form_validators.register(id, validator);
    }

    pub fn register_capability(
        &mut self,
        id: impl Into<String>,
        version: u32,
        capability: Arc<dyn Capability>,
    ) {
        self.capabilities.insert((id.into(), version), capability);
    }

    /// Integration modules opt actions into the Lua API when registering them.
    pub fn register_lua_capability(
        &mut self,
        id: impl Into<String>,
        version: u32,
        capability: Arc<dyn Capability>,
    ) {
        let id = id.into();
        self.lua_bindings
            .insert(id.clone(), Arc::clone(&capability));
        self.register_capability(id, version, capability);
    }

    pub fn register_lua_action(
        &mut self,
        schema: &'static ConfigSchema,
        capability: Arc<dyn Capability>,
    ) {
        let key = (schema.id.to_owned(), schema.version);
        assert!(
            !self.capabilities.contains_key(&key),
            "duplicate action registration: {} version {}",
            schema.id,
            schema.version
        );
        self.register_lua_capability(schema.id, schema.version, capability);
        self.action_schemas.insert(key, schema);
    }

    /// Register module-owned input defaults alongside the existing schema and execution handler.
    pub fn register_lua_action_definition(
        &mut self,
        definition: crate::schema::ActionDefinition,
        capability: Arc<dyn Capability>,
    ) {
        let schema = definition.schema;
        self.register_lua_action(schema, capability);
        self.action_definitions
            .insert((schema.id.to_owned(), schema.version), definition);
    }

    pub fn action_definitions(&self) -> Vec<crate::schema::ActionDefinition> {
        self.action_definitions.values().cloned().collect()
    }

    pub fn action_schemas(&self) -> Vec<&'static ConfigSchema> {
        self.action_schemas.values().copied().collect()
    }

    pub fn lua_bindings(&self) -> BTreeMap<String, Arc<dyn Capability>> {
        self.lua_bindings.clone()
    }

    pub fn register_workflow(&self, workflow: Workflow) -> Result<(), String> {
        self.validate_registered_workflow(&workflow)?;
        self.workflows
            .write()
            .expect("workflow registry lock was poisoned")
            .insert(workflow.id.clone(), Arc::new(workflow));
        Ok(())
    }

    pub fn validate_registered_workflow(&self, workflow: &Workflow) -> Result<(), String> {
        validate_workflow_with_schemas(workflow, &self.action_schemas)?;
        self.validate_form_validators(&workflow.steps)?;
        self.validate_capability_inputs(&workflow.steps, false)
    }

    pub fn validate_available_workflow(&self, workflow: &Workflow) -> Result<(), String> {
        validate_workflow_with_schemas(workflow, &self.action_schemas)?;
        self.validate_form_validators(&workflow.steps)?;
        self.validate_capability_inputs(&workflow.steps, true)
    }

    /// Stops future admissions from using this definition. Already admitted runs
    /// keep their pinned snapshot.
    pub fn unregister_workflow(&self, id: &str) {
        self.workflows
            .write()
            .expect("workflow registry lock was poisoned")
            .remove(id);
    }

    pub fn workflow(&self, id: &str) -> Option<Arc<Workflow>> {
        self.workflows
            .read()
            .expect("workflow registry lock was poisoned")
            .get(id)
            .cloned()
    }

    pub fn snapshot(&self, workflow: Arc<Workflow>) -> Result<ExecutionPlan, String> {
        validate_workflow_with_schemas(&workflow, &self.action_schemas)?;
        self.validate_form_validators(&workflow.steps)?;
        self.validate_capability_inputs(&workflow.steps, false)?;
        // Keep every called definition pinned to this activation, including calls reached later.
        let workflows = self
            .workflows
            .read()
            .expect("workflow registry lock was poisoned")
            .clone();
        Ok(ExecutionPlan {
            workflow,
            workflows,
        })
    }

    fn validate_form_validators(&self, steps: &[Step]) -> Result<(), String> {
        for step in steps {
            match &step.kind {
                StepKind::RequestInput { fields, .. } => {
                    for field in fields {
                        if let Some(id) = &field.validator
                            && !self.form_validators.contains(id)
                        {
                            return Err(format!(
                                "step `{}` field `{}`: form validator `{id}` is unavailable",
                                step.id, field.id
                            ));
                        }
                    }
                }
                StepKind::If {
                    then_steps,
                    else_steps,
                    ..
                } => {
                    self.validate_form_validators(then_steps)?;
                    self.validate_form_validators(else_steps)?;
                }
                StepKind::While { steps, .. } | StepKind::OneOrMore { steps } => {
                    self.validate_form_validators(steps)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn validate_capability_inputs(
        &self,
        steps: &[Step],
        require_available: bool,
    ) -> Result<(), String> {
        for step in steps {
            match &step.kind {
                StepKind::Action {
                    capability,
                    version,
                    inputs,
                    ..
                } => {
                    if let Some(implementation) =
                        self.capabilities.get(&(capability.clone(), *version))
                    {
                        implementation
                            .validate_inputs(inputs)
                            .map_err(|error| format!("step `{}`: {error}", step.id))?;
                    } else if require_available {
                        return Err(format!(
                            "step `{}`: capability `{capability}` version {version} is unavailable",
                            step.id
                        ));
                    }
                }
                StepKind::If {
                    then_steps,
                    else_steps,
                    ..
                } => {
                    self.validate_capability_inputs(then_steps, require_available)?;
                    self.validate_capability_inputs(else_steps, require_available)?;
                }
                StepKind::While { steps, .. } | StepKind::OneOrMore { steps } => {
                    self.validate_capability_inputs(steps, require_available)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub async fn run(
        &self,
        workflow: Arc<Workflow>,
        trigger: Values,
        cancel: CancellationToken,
    ) -> RunResult {
        match self.snapshot(workflow) {
            Ok(plan) => self.run_plan(plan, trigger, cancel).await,
            Err(message) => {
                let failure = Failure::new("", FailureKind::InvalidDefinition, message);
                RunResult {
                    outcome: Outcome::Failed(failure.clone()),
                    environment: Environment {
                        trigger,
                        ..Environment::default()
                    },
                    declared_outputs: Values::new(),
                    failures: vec![failure],
                    script_activity: Vec::new(),
                    step_trace: Vec::new(),
                }
            }
        }
    }

    pub async fn run_plan(
        &self,
        plan: ExecutionPlan,
        trigger: Values,
        cancel: CancellationToken,
    ) -> RunResult {
        let workflow = plan.workflow;
        let mut environment = Environment {
            trigger,
            ..Environment::default()
        };
        let mut state = RunState {
            starts: 0,
            failures: Vec::new(),
            script_activity: Vec::new(),
            step_trace: Vec::new(),
        };
        let flow = self
            .execute_workflow(
                ExecutionContext {
                    workflow: &workflow,
                    workflows: &plan.workflows,
                    call_depth: 1,
                    parent_sequence: None,
                },
                &mut environment,
                &mut state,
                &cancel,
            )
            .await;
        let outcome = match flow {
            Flow::Continue => Outcome::Success,
            Flow::Stop => Outcome::Stopped,
            Flow::Cancel => Outcome::Cancelled,
            Flow::Fail(failure) => Outcome::Failed(failure),
        };
        let mut declared_outputs = Values::new();
        if matches!(outcome, Outcome::Success | Outcome::Stopped) {
            for (name, expression) in &workflow.outputs {
                match evaluate(expression, &environment) {
                    Ok(value) => {
                        declared_outputs.insert(name.clone(), value);
                    }
                    Err(message) => {
                        let failure = Failure::new("", FailureKind::MissingValue, message);
                        state.failures.push(failure);
                        return RunResult {
                            outcome: Outcome::Failed(state.failures.last().unwrap().clone()),
                            environment,
                            declared_outputs: Values::new(),
                            failures: state.failures,
                            script_activity: state.script_activity,
                            step_trace: state.step_trace,
                        };
                    }
                }
            }
        }
        RunResult {
            outcome,
            environment,
            declared_outputs,
            failures: state.failures,
            script_activity: state.script_activity,
            step_trace: state.step_trace,
        }
    }

    fn execute_workflow<'a>(
        &'a self,
        context: ExecutionContext<'a>,
        environment: &'a mut Environment,
        state: &'a mut RunState,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, Flow> {
        Box::pin(async move {
            self.execute_steps(context, &context.workflow.steps, environment, state, cancel)
                .await
        })
    }

    fn execute_steps<'a>(
        &'a self,
        context: ExecutionContext<'a>,
        steps: &'a [Step],
        environment: &'a mut Environment,
        state: &'a mut RunState,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, Flow> {
        Box::pin(async move {
            let workflow = context.workflow;
            if cancel.is_cancelled() {
                return Flow::Cancel;
            }
            for step in steps {
                if cancel.is_cancelled() {
                    return Flow::Cancel;
                }
                if state.starts >= MAX_STEP_STARTS {
                    let failure =
                        Failure::new(&step.id, FailureKind::StepLimit, "step start limit reached");
                    state.failures.push(failure.clone());
                    return Flow::Fail(failure);
                }
                state.starts += 1;
                let sequence = state.starts as u64;
                let started_at_ms = trace_now_ms();
                let started = Instant::now();
                let trace_index = state.step_trace.len();
                state.step_trace.push(StepTrace {
                    sequence,
                    parent_sequence: context.parent_sequence,
                    workflow_id: workflow.id.clone(),
                    workflow_revision: workflow.revision,
                    step_id: step.id.clone(),
                    kind: StepTraceKind::from(&step.kind),
                    started_at_ms,
                    finished_at_ms: started_at_ms,
                    duration_ms: 0,
                    outcome: StepTraceOutcome::Cancelled,
                });
                self.events.record(Event::StepStarted {
                    workflow_id: workflow.id.clone(),
                    step_id: step.id.clone(),
                });
                let flow = self
                    .execute_step(context.nested(sequence), step, environment, state, cancel)
                    .await;
                let trace = &mut state.step_trace[trace_index];
                trace.finished_at_ms = trace_now_ms().max(started_at_ms);
                trace.duration_ms =
                    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                trace.outcome = match &flow {
                    Flow::Continue => StepTraceOutcome::Succeeded,
                    Flow::Stop => StepTraceOutcome::Stopped,
                    Flow::Cancel => StepTraceOutcome::Cancelled,
                    Flow::Fail(failure) => StepTraceOutcome::Failed {
                        kind: failure.kind.clone(),
                        remote_effect_uncertain: failure.remote_effect_uncertain,
                        continued_by_policy: step.on_failure == FailurePolicy::Continue,
                    },
                };
                let outcome = match &flow {
                    Flow::Continue => Outcome::Success,
                    Flow::Stop => Outcome::Stopped,
                    Flow::Cancel => Outcome::Cancelled,
                    Flow::Fail(failure) => Outcome::Failed(failure.clone()),
                };
                if matches!(&step.kind, StepKind::Action { capability, .. } if capability == "lua.run")
                {
                    state.script_activity.push(ScriptActivity {
                        workflow_id: workflow.id.clone(),
                        step_id: step.id.clone(),
                        outcome: match &flow {
                            Flow::Continue | Flow::Stop => ScriptOutcome::Succeeded,
                            Flow::Cancel => ScriptOutcome::Cancelled,
                            Flow::Fail(_) => ScriptOutcome::Failed,
                        },
                    });
                }
                self.events.record(Event::StepFinished {
                    workflow_id: workflow.id.clone(),
                    step_id: step.id.clone(),
                    outcome,
                });
                match flow {
                    Flow::Continue => {}
                    Flow::Fail(failure) => {
                        state.failures.push(failure.clone());
                        if step.on_failure != FailurePolicy::Continue {
                            return Flow::Fail(failure);
                        }
                    }
                    other => return other,
                }
            }
            Flow::Continue
        })
    }

    fn execute_step<'a>(
        &'a self,
        context: ExecutionContext<'a>,
        step: &'a Step,
        environment: &'a mut Environment,
        state: &'a mut RunState,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, Flow> {
        Box::pin(async move {
            let workflow = context.workflow;
            let workflows = context.workflows;
            let sequence = context.parent_sequence.expect("step has a trace sequence");
            match &step.kind {
                StepKind::Action {
                    capability,
                    version,
                    inputs,
                    deadline_ms,
                } => {
                    let Some(implementation) =
                        self.capabilities.get(&(capability.clone(), *version))
                    else {
                        return fail(
                            step,
                            FailureKind::CapabilityUnavailable,
                            "capability is not registered",
                        );
                    };
                    if let Err(message) = implementation.ready() {
                        return fail(step, FailureKind::ConnectorUnavailable, message);
                    }
                    let mut arguments = Values::new();
                    for (name, value) in inputs {
                        match evaluate(value, environment) {
                            Ok(value) => {
                                arguments.insert(name.clone(), value);
                            }
                            Err(message) => return fail(step, FailureKind::MissingValue, message),
                        }
                    }
                    if cancel.is_cancelled() {
                        return Flow::Cancel;
                    }
                    let deadline = deadline_ms
                        .map(Duration::from_millis)
                        .unwrap_or_else(|| implementation.default_deadline());
                    let action_cancel = cancel.child_token();
                    let result = tokio::select! {
                        _ = cancel.cancelled() => { action_cancel.cancel(); return Flow::Cancel; },
                        result = timeout(deadline, implementation.execute(arguments, action_cancel.clone())) => result,
                    };
                    match result {
                        Ok(Ok(outputs)) => {
                            if let Some(schema) =
                                self.action_schemas.get(&(capability.clone(), *version))
                                && let Err(message) = validate_declared_outputs(schema, &outputs)
                            {
                                return fail(step, FailureKind::Action, message);
                            }
                            environment.outputs.insert(step.id.clone(), outputs);
                            Flow::Continue
                        }
                        Ok(Err(CapabilityError::ConnectorUnavailable(message))) => {
                            fail(step, FailureKind::ConnectorUnavailable, message)
                        }
                        Ok(Err(CapabilityError::Failed(message))) => {
                            fail(step, FailureKind::Action, message)
                        }
                        Ok(Err(CapabilityError::Uncertain(message))) => Flow::Fail(Failure {
                            remote_effect_uncertain: true,
                            ..Failure::new(&step.id, FailureKind::Action, message)
                        }),
                        Err(_) => {
                            action_cancel.cancel();
                            Flow::Fail(Failure {
                                remote_effect_uncertain: true,
                                ..Failure::new(
                                    &step.id,
                                    FailureKind::Timeout,
                                    "action deadline expired",
                                )
                            })
                        }
                    }
                }
                StepKind::SetVariable { name, value } => match evaluate(value, environment) {
                    Ok(value) => {
                        if environment.variables.insert(name.clone(), value).is_some() {
                            self.events.record(Event::VariableReplaced {
                                workflow_id: workflow.id.clone(),
                                step_id: step.id.clone(),
                                name: name.clone(),
                            });
                        }
                        Flow::Continue
                    }
                    Err(message) => fail(step, FailureKind::MissingValue, message),
                },
                StepKind::If {
                    condition,
                    then_steps,
                    else_steps,
                } => match evaluate_condition(condition, environment) {
                    Ok(true) => {
                        self.execute_steps(context, then_steps, environment, state, cancel)
                            .await
                    }
                    Ok(false) => {
                        self.execute_steps(context, else_steps, environment, state, cancel)
                            .await
                    }
                    Err(message) => fail(step, FailureKind::InvalidType, message),
                },
                StepKind::While { condition, steps } => {
                    for iteration in 0..=MAX_LOOP_ITERATIONS {
                        if cancel.is_cancelled() {
                            return Flow::Cancel;
                        }
                        match evaluate_condition(condition, environment) {
                            Ok(false) => return Flow::Continue,
                            Ok(true) => {}
                            Err(message) => return fail(step, FailureKind::InvalidType, message),
                        }
                        if iteration == MAX_LOOP_ITERATIONS {
                            return fail(
                                step,
                                FailureKind::LoopLimit,
                                "loop iteration limit reached",
                            );
                        }
                        match self
                            .execute_steps(context, steps, environment, state, cancel)
                            .await
                        {
                            Flow::Continue => {}
                            other => return other,
                        }
                    }
                    unreachable!("the final loop check returns")
                }
                StepKind::OneOrMore { steps } => {
                    let mut succeeded = false;
                    let mut failed_children = 0;
                    let mut uncertain_child = false;
                    for child in steps {
                        let failures_before = state.failures.len();
                        match self
                            .execute_steps(
                                context,
                                std::slice::from_ref(child),
                                environment,
                                state,
                                cancel,
                            )
                            .await
                        {
                            Flow::Continue => {
                                // A child's Continue policy still records its failure.
                                if state.failures.len() == failures_before {
                                    succeeded = true;
                                } else {
                                    failed_children += 1;
                                    uncertain_child |= state.failures[failures_before..]
                                        .iter()
                                        .any(|failure| failure.remote_effect_uncertain);
                                }
                            }
                            Flow::Fail(failure) => {
                                failed_children += 1;
                                uncertain_child |= failure.remote_effect_uncertain;
                            }
                            Flow::Stop => return Flow::Stop,
                            Flow::Cancel => return Flow::Cancel,
                        }
                    }
                    if succeeded {
                        Flow::Continue
                    } else {
                        Flow::Fail(Failure {
                            remote_effect_uncertain: uncertain_child,
                            ..Failure::new(
                                &step.id,
                                FailureKind::NoChildSucceeded,
                                format!("all {failed_children} children failed"),
                            )
                        })
                    }
                }
                StepKind::Delay { millis } => {
                    tokio::select! { _ = cancel.cancelled() => Flow::Cancel, _ = tokio::time::sleep(Duration::from_millis(*millis)) => Flow::Continue }
                }
                StepKind::RequestInput { title, fields } => {
                    let mut defaults = Values::new();
                    for field in fields {
                        if let Some(value) = &field.default {
                            match evaluate(value, environment) {
                                Ok(value) => {
                                    defaults.insert(field.id.clone(), value);
                                }
                                Err(message) => {
                                    return fail(step, FailureKind::MissingValue, message);
                                }
                            }
                        }
                    }
                    let response = tokio::select! {
                        _ = cancel.cancelled() => return Flow::Cancel,
                        result = self.input.request(title.clone().unwrap_or_else(|| "Input needed".into()), fields.clone(), defaults, &self.form_validators, cancel.child_token()) => result,
                    };
                    match response {
                        Ok(InputResponse::Cancelled) => Flow::Cancel,
                        Ok(InputResponse::Applied(values)) => {
                            if fields.iter().any(|field| {
                                field.required
                                    && values
                                        .get(&field.id)
                                        .and_then(Value::as_str)
                                        .is_none_or(|value| value.trim().is_empty())
                            }) {
                                fail(step, FailureKind::Input, "required field missing")
                            } else if values.values().any(|value| !value.is_string()) {
                                fail(step, FailureKind::Input, "form fields must be text")
                            } else if values
                                .keys()
                                .any(|id| !fields.iter().any(|field| &field.id == id))
                            {
                                fail(step, FailureKind::Input, "unknown form field")
                            } else {
                                environment.outputs.insert(step.id.clone(), values);
                                Flow::Continue
                            }
                        }
                        Err(message) => fail(step, FailureKind::Input, message),
                    }
                }
                StepKind::Call {
                    workflow_id,
                    binding,
                } => {
                    if context.call_depth >= MAX_CALL_DEPTH {
                        return fail(step, FailureKind::CallDepth, "workflow call depth reached");
                    }
                    let Some(child) = workflows.get(workflow_id) else {
                        return fail(
                            step,
                            FailureKind::CalledWorkflow,
                            "called workflow is unavailable",
                        );
                    };
                    let trigger = match binding {
                        CallBinding::Explicit { inputs } => {
                            let mut values = Values::new();
                            for (name, expression) in inputs {
                                match evaluate(expression, environment) {
                                    Ok(value) => {
                                        values.insert(name.clone(), value);
                                    }
                                    Err(message) => {
                                        return fail(step, FailureKind::MissingValue, message);
                                    }
                                }
                            }
                            values
                        }
                        CallBinding::AllNamedArguments => environment.variables.clone(),
                    };
                    let mut child_environment = Environment {
                        trigger: trigger.clone(),
                        variables: if matches!(binding, CallBinding::AllNamedArguments) {
                            trigger
                        } else {
                            Values::new()
                        },
                        outputs: BTreeMap::new(),
                    };
                    let result = self
                        .execute_workflow(
                            context.called(child, sequence),
                            &mut child_environment,
                            state,
                            cancel,
                        )
                        .await;
                    match result {
                        Flow::Continue | Flow::Stop => {
                            let mut outputs = Values::new();
                            for (name, expression) in &child.outputs {
                                match evaluate(expression, &child_environment) {
                                    Ok(value) => {
                                        outputs.insert(name.clone(), value);
                                    }
                                    Err(message) => {
                                        return fail(step, FailureKind::MissingValue, message);
                                    }
                                }
                            }
                            if matches!(binding, CallBinding::AllNamedArguments) {
                                environment.variables.extend(child_environment.variables);
                            }
                            environment.outputs.insert(step.id.clone(), outputs);
                            Flow::Continue
                        }
                        Flow::Cancel => Flow::Cancel,
                        Flow::Fail(failure) => Flow::Fail(Failure {
                            remote_effect_uncertain: failure.remote_effect_uncertain,
                            ..Failure::new(
                                &step.id,
                                FailureKind::CalledWorkflow,
                                format!(
                                    "child step {} failed: {}",
                                    failure.step_id, failure.message
                                ),
                            )
                        }),
                    }
                }
                StepKind::Stop => Flow::Stop,
            }
        })
    }
}

fn fail(step: &Step, kind: FailureKind, message: impl Into<String>) -> Flow {
    Flow::Fail(Failure::new(&step.id, kind, message))
}

fn trace_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}

pub fn evaluate(input: &Input, environment: &Environment) -> Result<Value, String> {
    match input {
        Input::Literal(value) => Ok(value.clone()),
        Input::Object(fields) => fields
            .iter()
            .map(|(name, input)| evaluate(input, environment).map(|value| (name.clone(), value)))
            .collect::<Result<serde_json::Map<_, _>, _>>()
            .map(Value::Object),
        Input::Array(items) => items
            .iter()
            .map(|input| evaluate(input, environment))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Input::Reference {
            step_id,
            output_id,
            fallback,
        } => lookup(
            environment
                .outputs
                .get(step_id)
                .and_then(|values| values.get(output_id)),
            fallback,
            environment,
            format!("missing output {step_id}.{output_id}"),
        ),
        Input::Trigger { name, fallback } => lookup(
            environment.trigger.get(name),
            fallback,
            environment,
            format!("missing trigger input {name}"),
        ),
        Input::Variable { name, fallback } => lookup(
            environment.variables.get(name),
            fallback,
            environment,
            format!("missing variable {name}"),
        ),
        Input::Text(parts) => {
            let mut text = String::new();
            for part in parts {
                match part {
                    TextPart::Literal(value) => text.push_str(value),
                    TextPart::Value(value) => match evaluate(value, environment)? {
                        Value::String(value) => text.push_str(&value),
                        _ => return Err("text segment requires a string".to_owned()),
                    },
                }
            }
            Ok(Value::String(text))
        }
    }
}

fn lookup(
    value: Option<&Value>,
    fallback: &Option<Box<Input>>,
    environment: &Environment,
    message: String,
) -> Result<Value, String> {
    match (value, fallback) {
        (Some(value), _) => Ok(value.clone()),
        (None, Some(input)) => evaluate(input, environment),
        (None, None) => Err(message),
    }
}

fn optional(input: &Input, environment: &Environment) -> Result<Option<Value>, String> {
    let absent = match input {
        Input::Reference {
            step_id,
            output_id,
            fallback: None,
        } => environment
            .outputs
            .get(step_id)
            .and_then(|values| values.get(output_id))
            .is_none(),
        Input::Trigger {
            name,
            fallback: None,
        } => !environment.trigger.contains_key(name),
        Input::Variable {
            name,
            fallback: None,
        } => !environment.variables.contains_key(name),
        _ => false,
    };
    if absent {
        Ok(None)
    } else {
        evaluate(input, environment).map(Some)
    }
}

pub fn evaluate_condition(
    condition: &Condition,
    environment: &Environment,
) -> Result<bool, String> {
    match condition {
        Condition::Equal(a, b) | Condition::NotEqual(a, b) => {
            let a = evaluate(a, environment)?;
            let b = evaluate(b, environment)?;
            if std::mem::discriminant(&a) != std::mem::discriminant(&b) {
                return Err("equality requires matching types".to_owned());
            }
            Ok(if matches!(condition, Condition::Equal(_, _)) {
                a == b
            } else {
                a != b
            })
        }
        Condition::Exists(value) => Ok(optional(value, environment)?.is_some()),
        Condition::NullOrEmpty(value) => Ok(match optional(value, environment)? {
            None | Some(Value::Null) => true,
            Some(Value::String(value)) => value.is_empty(),
            Some(Value::Array(value)) => value.is_empty(),
            Some(Value::Object(value)) => value.is_empty(),
            _ => false,
        }),
        Condition::Contains(a, b) => {
            let a = evaluate(a, environment)?;
            let b = evaluate(b, environment)?;
            match (a.as_str(), b.as_str()) {
                (Some(a), Some(b)) => Ok(a.contains(b)),
                _ => Err("contains requires strings".to_owned()),
            }
        }
        Condition::Less(a, b) | Condition::Greater(a, b) => {
            let a = evaluate(a, environment)?;
            let b = evaluate(b, environment)?;
            match (a.as_f64(), b.as_f64()) {
                (Some(a), Some(b)) => Ok(if matches!(condition, Condition::Less(_, _)) {
                    a < b
                } else {
                    a > b
                }),
                _ => Err("numeric comparison requires numbers".to_owned()),
            }
        }
        Condition::All(conditions) => {
            for condition in conditions {
                if !evaluate_condition(condition, environment)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        Condition::Any(conditions) => {
            for condition in conditions {
                if evaluate_condition(condition, environment)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Condition::Not(condition) => Ok(!evaluate_condition(condition, environment)?),
    }
}

pub fn validate_workflow(workflow: &Workflow) -> Result<(), String> {
    validate_workflow_with_schemas(workflow, &BTreeMap::new())
}

fn validate_workflow_with_schemas(
    workflow: &Workflow,
    schemas: &BTreeMap<(String, u32), &'static ConfigSchema>,
) -> Result<(), String> {
    if workflow.id.is_empty() {
        return Err("workflow ID is empty".to_owned());
    }
    let mut ids = std::collections::HashSet::new();
    fn visit(steps: &[Step], ids: &mut std::collections::HashSet<String>) -> Result<(), String> {
        for step in steps {
            if step.id.is_empty() || !ids.insert(step.id.clone()) {
                return Err(format!("duplicate or empty step ID: {}", step.id));
            }
            match &step.kind {
                StepKind::If {
                    then_steps,
                    else_steps,
                    ..
                } => {
                    visit(then_steps, ids)?;
                    visit(else_steps, ids)?;
                }
                StepKind::While { steps, .. } | StepKind::OneOrMore { steps } => visit(steps, ids)?,
                StepKind::RequestInput { fields, .. } => {
                    let mut field_ids = std::collections::HashSet::new();
                    for field in fields {
                        if field.id.is_empty() || !field_ids.insert(&field.id) {
                            return Err(format!("duplicate or empty field ID in {}", step.id));
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
    visit(&workflow.steps, &mut ids)?;
    let mut producers = std::collections::HashSet::new();
    fn collect_producers(steps: &[Step], producers: &mut std::collections::HashSet<String>) {
        for step in steps {
            match &step.kind {
                StepKind::Action { .. } | StepKind::RequestInput { .. } | StepKind::Call { .. } => {
                    producers.insert(step.id.clone());
                }
                StepKind::If {
                    then_steps,
                    else_steps,
                    ..
                } => {
                    collect_producers(then_steps, producers);
                    collect_producers(else_steps, producers);
                }
                StepKind::While { steps, .. } | StepKind::OneOrMore { steps } => {
                    collect_producers(steps, producers)
                }
                _ => {}
            }
        }
    }
    collect_producers(&workflow.steps, &mut producers);
    let mut contracts = HashMap::new();
    fn collect_contracts(
        steps: &[Step],
        schemas: &BTreeMap<(String, u32), &'static ConfigSchema>,
        contracts: &mut HashMap<String, HashMap<String, bool>>,
    ) {
        for step in steps {
            match &step.kind {
                StepKind::Action {
                    capability,
                    version,
                    ..
                } => {
                    if let Some(schema) = schemas.get(&(capability.clone(), *version)) {
                        contracts.insert(
                            step.id.clone(),
                            schema
                                .outputs
                                .iter()
                                .map(|output| (output.id.to_owned(), output.required))
                                .collect(),
                        );
                    }
                }
                StepKind::RequestInput { fields, .. } => {
                    contracts.insert(
                        step.id.clone(),
                        fields
                            .iter()
                            .map(|field| (field.id.clone(), field.required))
                            .collect(),
                    );
                }
                StepKind::If {
                    then_steps,
                    else_steps,
                    ..
                } => {
                    collect_contracts(then_steps, schemas, contracts);
                    collect_contracts(else_steps, schemas, contracts);
                }
                StepKind::While { steps, .. } | StepKind::OneOrMore { steps } => {
                    collect_contracts(steps, schemas, contracts);
                }
                _ => {}
            }
        }
    }
    collect_contracts(&workflow.steps, schemas, &mut contracts);
    let mut scope = ValidationScope {
        contracts: Arc::new(contracts),
        ..ValidationScope::default()
    };
    validate_steps(&workflow.steps, &ids, &producers, &mut scope)?;
    for input in workflow.outputs.values() {
        validate_input(input, &ids, &producers, &scope, false)?;
    }
    Ok(())
}

#[derive(Clone, Default)]
struct ValidationScope {
    seen: std::collections::HashSet<String>,
    definite: std::collections::HashSet<String>,
    guarded: std::collections::HashSet<(String, String)>,
    contracts: Arc<HashMap<String, HashMap<String, bool>>>,
}

fn validate_input(
    input: &Input,
    ids: &std::collections::HashSet<String>,
    producers: &std::collections::HashSet<String>,
    scope: &ValidationScope,
    allows_optional: bool,
) -> Result<(), String> {
    match input {
        Input::Object(fields) => {
            for input in fields.values() {
                validate_input(input, ids, producers, scope, false)?;
            }
        }
        Input::Array(items) => {
            for input in items {
                validate_input(input, ids, producers, scope, false)?;
            }
        }
        Input::Reference {
            step_id,
            output_id,
            fallback,
        } => {
            if !ids.contains(step_id) {
                return Err(format!("unknown step reference: {step_id}"));
            }
            if !producers.contains(step_id) {
                return Err(format!("step {step_id} has no outputs"));
            }
            if !scope.seen.contains(step_id) {
                return Err(format!("step {step_id} is not earlier in this path"));
            }
            let required = if let Some(outputs) = scope.contracts.get(step_id) {
                *outputs
                    .get(output_id)
                    .ok_or_else(|| format!("step {step_id} has no output `{output_id}`"))?
            } else {
                true
            };
            if (!scope.definite.contains(step_id) || !required)
                && !scope
                    .guarded
                    .contains(&(step_id.clone(), output_id.clone()))
                && fallback.is_none()
                && !allows_optional
            {
                return Err(format!(
                    "output {step_id}.{output_id} may be absent; add a fallback or existence guard"
                ));
            }
            if let Some(fallback) = fallback {
                validate_input(fallback, ids, producers, scope, false)?;
            }
        }
        Input::Trigger { fallback, .. } | Input::Variable { fallback, .. } => {
            if let Some(fallback) = fallback {
                validate_input(fallback, ids, producers, scope, false)?;
            }
        }
        Input::Text(parts) => {
            for part in parts {
                if let TextPart::Value(value) = part {
                    validate_input(value, ids, producers, scope, false)?;
                }
            }
        }
        Input::Literal(_) => {}
    }
    Ok(())
}

fn validate_condition(
    condition: &Condition,
    ids: &std::collections::HashSet<String>,
    producers: &std::collections::HashSet<String>,
    scope: &ValidationScope,
) -> Result<(), String> {
    match condition {
        Condition::Equal(a, b)
        | Condition::NotEqual(a, b)
        | Condition::Contains(a, b)
        | Condition::Less(a, b)
        | Condition::Greater(a, b) => {
            validate_input(a, ids, producers, scope, false)?;
            validate_input(b, ids, producers, scope, false)?;
        }
        Condition::Exists(input) | Condition::NullOrEmpty(input) => {
            validate_input(input, ids, producers, scope, true)?
        }
        Condition::All(conditions) | Condition::Any(conditions) => {
            for condition in conditions {
                validate_condition(condition, ids, producers, scope)?;
            }
        }
        Condition::Not(condition) => validate_condition(condition, ids, producers, scope)?,
    }
    Ok(())
}

fn add_exists_guards(condition: &Condition, scope: &mut ValidationScope) {
    match condition {
        Condition::Exists(Input::Reference {
            step_id,
            output_id,
            fallback: None,
        }) => {
            scope.guarded.insert((step_id.clone(), output_id.clone()));
        }
        Condition::All(conditions) => {
            for condition in conditions {
                add_exists_guards(condition, scope);
            }
        }
        _ => {}
    }
}

fn validate_steps(
    steps: &[Step],
    ids: &std::collections::HashSet<String>,
    producers: &std::collections::HashSet<String>,
    scope: &mut ValidationScope,
) -> Result<(), String> {
    for step in steps {
        match &step.kind {
            StepKind::Action { inputs, .. } => {
                for input in inputs.values() {
                    validate_input(input, ids, producers, scope, false)?;
                }
            }
            StepKind::SetVariable { value, .. } => {
                validate_input(value, ids, producers, scope, false)?
            }
            StepKind::RequestInput { fields, .. } => {
                for field in fields {
                    if let Some(input) = &field.default {
                        validate_input(input, ids, producers, scope, false)?;
                    }
                }
            }
            StepKind::Call {
                binding: CallBinding::Explicit { inputs },
                ..
            } => {
                for input in inputs.values() {
                    validate_input(input, ids, producers, scope, false)?;
                }
            }
            StepKind::If {
                condition,
                then_steps,
                else_steps,
            } => {
                validate_condition(condition, ids, producers, scope)?;
                let mut yes = scope.clone();
                add_exists_guards(condition, &mut yes);
                validate_steps(then_steps, ids, producers, &mut yes)?;
                let mut no = scope.clone();
                validate_steps(else_steps, ids, producers, &mut no)?;
                scope.seen.extend(yes.seen.iter().cloned());
                scope.seen.extend(no.seen.iter().cloned());
                scope.definite = yes.definite.intersection(&no.definite).cloned().collect();
            }
            StepKind::While { condition, steps } => {
                validate_condition(condition, ids, producers, scope)?;
                let mut body = scope.clone();
                validate_steps(steps, ids, producers, &mut body)?;
                scope.seen.extend(body.seen);
            }
            StepKind::OneOrMore { steps } => {
                for child in steps {
                    let mut child_scope = scope.clone();
                    validate_steps(
                        std::slice::from_ref(child),
                        ids,
                        producers,
                        &mut child_scope,
                    )?;
                    scope.seen.extend(child_scope.seen);
                }
            }
            StepKind::Call {
                binding: CallBinding::AllNamedArguments,
                ..
            }
            | StepKind::Delay { .. }
            | StepKind::Stop => {}
        }
        scope.seen.insert(step.id.clone());
        if producers.contains(&step.id) && step.on_failure == FailurePolicy::Stop {
            scope.definite.insert(step.id.clone());
        }
    }
    Ok(())
}

/// Admission owns a revision snapshot. A caller takes ready activations and runs them on its Tokio runtime.
pub struct ActivationQueue {
    inner: Mutex<QueueState>,
    changed: Notify,
    events: Arc<dyn EventSink>,
}

struct QueueState {
    pending: VecDeque<Activation>,
    active: HashMap<String, usize>,
    active_total: usize,
}

pub struct Activation {
    pub plan: ExecutionPlan,
    pub trigger: Values,
    pub cancel: CancellationToken,
    pub origin: ActivationOrigin,
    pub admission_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActivationOrigin {
    Manual,
    Schedule(String),
    Event(String),
    Webhook(String),
}

impl ActivationQueue {
    pub fn new(events: Arc<dyn EventSink>) -> Self {
        Self {
            inner: Mutex::new(QueueState {
                pending: VecDeque::new(),
                active: HashMap::new(),
                active_total: 0,
            }),
            changed: Notify::new(),
            events,
        }
    }

    pub async fn admit(
        &self,
        plan: ExecutionPlan,
        trigger: Values,
        cancel: CancellationToken,
    ) -> Result<(), String> {
        self.admit_with_origin(plan, trigger, cancel, ActivationOrigin::Manual)
            .await
    }

    pub async fn admit_with_origin(
        &self,
        plan: ExecutionPlan,
        trigger: Values,
        cancel: CancellationToken,
        origin: ActivationOrigin,
    ) -> Result<(), String> {
        self.admit_recorded(plan, trigger, cancel, origin, None)
            .await
    }

    pub async fn admit_recorded(
        &self,
        plan: ExecutionPlan,
        trigger: Values,
        cancel: CancellationToken,
        origin: ActivationOrigin,
        admission_id: Option<String>,
    ) -> Result<(), String> {
        let workflow = &plan.workflow;
        let mut queue = self.inner.lock().await;
        let per_workflow = queue
            .pending
            .iter()
            .filter(|item| item.plan.workflow.id == workflow.id)
            .count();
        let reason = if per_workflow >= MAX_PENDING_PER_WORKFLOW {
            Some("workflow pending limit reached")
        } else if queue.pending.len() >= MAX_PENDING_GLOBAL {
            Some("global pending limit reached")
        } else {
            None
        };
        if let Some(reason) = reason {
            self.events.record(Event::ActivationRejected {
                workflow_id: workflow.id.clone(),
                revision: workflow.revision,
                reason: reason.to_owned(),
            });
            return Err(reason.to_owned());
        }
        queue.pending.push_back(Activation {
            plan,
            trigger,
            cancel,
            origin,
            admission_id,
        });
        self.changed.notify_waiters();
        Ok(())
    }

    pub async fn take_ready(&self) -> Option<Activation> {
        let mut queue = self.inner.lock().await;
        if queue.active_total >= MAX_ACTIVE_GLOBAL {
            return None;
        }
        let index = queue.pending.iter().position(|item| {
            let active = queue
                .active
                .get(&item.plan.workflow.id)
                .copied()
                .unwrap_or(0);
            active
                < if item.plan.workflow.overlap {
                    MAX_ACTIVE_PER_WORKFLOW
                } else {
                    1
                }
        })?;
        let activation = queue.pending.remove(index)?;
        *queue
            .active
            .entry(activation.plan.workflow.id.clone())
            .or_default() += 1;
        queue.active_total += 1;
        Some(activation)
    }

    pub async fn finished(&self, workflow_id: &str) {
        let mut queue = self.inner.lock().await;
        if let Some(active) = queue.active.get_mut(workflow_id)
            && *active > 0
        {
            *active -= 1;
            queue.active_total -= 1;
            self.changed.notify_waiters();
        }
    }

    pub async fn next_ready(&self, cancel: &CancellationToken) -> Option<Activation> {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(activation) = self.take_ready().await {
                return Some(activation);
            }
            tokio::select! {
                _ = cancel.cancelled() => return None,
                _ = &mut notified => {}
            }
        }
    }

    pub async fn pending_count(&self) -> usize {
        self.inner.lock().await.pending.len()
    }

    pub async fn drain_pending(&self) -> Vec<Activation> {
        self.inner.lock().await.pending.drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lua::LuaAction;
    use crate::schema::ConfigField;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct GameValidator;

    impl FormValidator for GameValidator {
        fn validate<'a>(
            &'a self,
            value: &'a str,
            _: CancellationToken,
        ) -> BoxFuture<'a, Result<String, String>> {
            Box::pin(async move {
                if value == "Good Game" {
                    Ok("42".into())
                } else {
                    Err("game was not found".into())
                }
            })
        }
    }

    #[tokio::test]
    async fn form_validator_resolves_values_without_publishing_invalid_input() {
        let mut validators = FormValidators::default();
        validators.register("test.game", Arc::new(GameValidator));
        let fields = [InputField {
            id: "game_id".into(),
            label: Some("Game".into()),
            default: None,
            required: true,
            validator: Some("test.game".into()),
        }];
        let invalid = Values::from([("game_id".into(), json!("Unknown"))]);
        assert_eq!(
            validators
                .validate(&fields, &invalid, CancellationToken::new())
                .await,
            Err("Game: game was not found".into())
        );
        let valid = Values::from([("game_id".into(), json!("Good Game"))]);
        assert_eq!(
            validators
                .validate(&fields, &valid, CancellationToken::new())
                .await
                .unwrap(),
            Values::from([("game_id".into(), json!("42"))])
        );
        assert!(
            validators
                .validate(&fields, &Values::new(), CancellationToken::new())
                .await
                .is_err()
        );
    }

    #[test]
    fn unavailable_form_validator_rejects_workflow_before_run() {
        let engine = engine();
        let flow = workflow(
            "form",
            vec![step(
                "edit",
                StepKind::RequestInput {
                    title: Some("Edit stream details".into()),
                    fields: vec![InputField {
                        id: "game_id".into(),
                        label: Some("Game".into()),
                        default: None,
                        required: true,
                        validator: Some("missing.game".into()),
                    }],
                },
            )],
        );
        assert!(
            engine
                .validate_registered_workflow(&flow)
                .unwrap_err()
                .contains("missing.game")
        );
    }

    #[derive(Default, Serialize, Deserialize, crate::ConfigSchema)]
    #[config(id = "optional_test", version = 1, title = "Optional test")]
    struct OptionalConfig {
        #[config(id = "required", label = "Required", introduced = 1)]
        required: String,
        #[config(id = "text", label = "Text", introduced = 1)]
        text: Option<String>,
        #[config(id = "secret", label = "Secret", introduced = 1, secret)]
        secret: Option<String>,
        #[config(id = "enabled", label = "Enabled", introduced = 1)]
        enabled: Option<bool>,
        #[config(id = "count", label = "Count", introduced = 1)]
        count: Option<u32>,
    }

    #[derive(Default, Serialize, Deserialize, crate::ConfigSchema)]
    #[config(
        id = "output_test",
        version = 1,
        title = "Output test",
        output("value", "Value", "text"),
        output("maybe", "Maybe", "text", "optional")
    )]
    struct OutputConfig {}

    #[test]
    fn config_schema_marks_supported_option_fields_optional() {
        use crate::schema::DescribeConfig;

        let fields = OptionalConfig::SCHEMA.fields;
        assert_eq!(fields.len(), 5);
        assert!(fields[0].required);
        assert_eq!(fields[0].kind, ConfigFieldKind::Text);
        for field in &fields[1..] {
            assert!(!field.required);
        }
        assert_eq!(fields[1].kind, ConfigFieldKind::Text);
        assert_eq!(fields[2].kind, ConfigFieldKind::Secret);
        assert_eq!(fields[3].kind, ConfigFieldKind::Toggle);
        assert_eq!(fields[4].kind, ConfigFieldKind::Integer);
    }

    #[test]
    fn configured_inputs_allow_missing_and_null_optional_values() {
        const FIELDS: &[ConfigField] = &[
            ConfigField {
                id: "message",
                label: "Message",
                description: "",
                introduced_in: 1,
                kind: ConfigFieldKind::Text,
                required: true,
                choice_source: None,
            },
            ConfigField {
                id: "reply",
                label: "Reply",
                description: "",
                introduced_in: 1,
                kind: ConfigFieldKind::Text,
                required: false,
                choice_source: None,
            },
        ];
        const SCHEMA: ConfigSchema = ConfigSchema {
            id: "test",
            version: 1,
            title: "Test",
            fields: FIELDS,
            outputs: &[],
        };

        let required = Input::Literal(json!("hello"));
        assert!(
            validate_configured_inputs(
                &SCHEMA,
                &BTreeMap::from([("message".into(), required.clone())])
            )
            .is_ok()
        );
        assert!(validate_configured_inputs(&SCHEMA, &BTreeMap::new()).is_err());
        assert!(
            validate_configured_inputs(
                &SCHEMA,
                &BTreeMap::from([
                    ("message".into(), required.clone()),
                    ("reply".into(), Input::Literal(Value::Null)),
                ])
            )
            .is_ok()
        );
        assert!(
            validate_configured_inputs(
                &SCHEMA,
                &BTreeMap::from([
                    ("message".into(), required),
                    ("reply".into(), Input::Literal(json!(42))),
                ])
            )
            .is_err()
        );
    }

    struct Form {
        response: InputResponse,
    }
    impl InputProvider for Form {
        fn request<'a>(
            &'a self,
            _: String,
            _: Vec<InputField>,
            _: Values,
            _: &'a FormValidators,
            _: CancellationToken,
        ) -> BoxFuture<'a, Result<InputResponse, String>> {
            Box::pin(async { Ok(self.response.clone()) })
        }
    }

    struct Action {
        calls: Arc<AtomicUsize>,
        result: Result<Values, CapabilityError>,
        ready: bool,
    }
    impl Capability for Action {
        fn default_deadline(&self) -> Duration {
            Duration::from_secs(1)
        }
        fn ready(&self) -> Result<(), String> {
            if self.ready {
                Ok(())
            } else {
                Err("offline".to_owned())
            }
        }
        fn execute<'a>(
            &'a self,
            _: Values,
            _: CancellationToken,
        ) -> BoxFuture<'a, Result<Values, CapabilityError>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { self.result.clone() })
        }
    }

    fn engine() -> Engine {
        Engine::new(
            Arc::new(Form {
                response: InputResponse::Cancelled,
            }),
            Arc::new(|_: Event| {}),
        )
    }
    fn workflow(id: &str, steps: Vec<Step>) -> Workflow {
        Workflow {
            id: id.to_owned(),
            revision: 1,
            overlap: false,
            steps,
            outputs: BTreeMap::new(),
        }
    }
    fn step(id: &str, kind: StepKind) -> Step {
        Step {
            id: id.to_owned(),
            on_failure: FailurePolicy::Stop,
            kind,
        }
    }
    fn literal(value: Value) -> Input {
        Input::Literal(value)
    }
    fn reference(step_id: &str, output_id: &str) -> Input {
        Input::Reference {
            step_id: step_id.to_owned(),
            output_id: output_id.to_owned(),
            fallback: None,
        }
    }
    fn action(id: &str, capability: &str) -> Step {
        step(
            id,
            StepKind::Action {
                capability: capability.to_owned(),
                version: 1,
                inputs: BTreeMap::new(),
                deadline_ms: None,
            },
        )
    }
    fn register(
        engine: &mut Engine,
        id: &str,
        result: Result<Values, CapabilityError>,
        ready: bool,
    ) -> Arc<AtomicUsize> {
        let calls = Arc::new(AtomicUsize::new(0));
        engine.register_capability(
            id,
            1,
            Arc::new(Action {
                calls: calls.clone(),
                result,
                ready,
            }),
        );
        calls
    }

    #[tokio::test]
    async fn ordered_outputs_and_failed_actions_do_not_publish() {
        let mut engine = engine();
        register(
            &mut engine,
            "produce",
            Ok(Values::from([("value".into(), json!("done"))])),
            true,
        );
        register(
            &mut engine,
            "fail",
            Err(CapabilityError::Failed("broken".into())),
            true,
        );
        let mut failed = action("bad", "fail");
        failed.on_failure = FailurePolicy::Continue;
        let workflow = Arc::new(workflow(
            "flow",
            vec![
                action("first", "produce"),
                failed,
                step(
                    "saved",
                    StepKind::SetVariable {
                        name: "result".into(),
                        value: reference("first", "value"),
                    },
                ),
            ],
        ));
        let result = engine
            .run(workflow, Values::new(), CancellationToken::new())
            .await;
        assert_eq!(result.outcome, Outcome::Success);
        assert_eq!(
            result.environment.variables.get("result"),
            Some(&json!("done"))
        );
        assert!(!result.environment.outputs.contains_key("bad"));
        assert_eq!(result.failures[0].kind, FailureKind::Action);
    }

    #[tokio::test]
    async fn branch_optional_output_and_typed_conditions() {
        let mut engine = engine();
        register(
            &mut engine,
            "produce",
            Ok(Values::from([("value".into(), json!(1))])),
            true,
        );
        let condition = Condition::Equal(literal(json!(false)), literal(json!(false)));
        let branch = step(
            "branch",
            StepKind::If {
                condition,
                then_steps: vec![action("inside", "produce")],
                else_steps: vec![],
            },
        );
        let workflow = Arc::new(workflow("flow", vec![branch]));
        let result = engine
            .run(workflow, Values::new(), CancellationToken::new())
            .await;
        assert_eq!(result.outcome, Outcome::Success);
        assert_eq!(result.environment.outputs["inside"]["value"], json!(1));
        let missing = reference("absent", "value");
        assert_eq!(
            evaluate_condition(&Condition::Exists(missing.clone()), &result.environment),
            Ok(false)
        );
        assert_eq!(
            evaluate_condition(&Condition::NullOrEmpty(missing), &result.environment),
            Ok(true)
        );
        assert!(
            evaluate_condition(
                &Condition::Equal(literal(json!(1)), literal(json!("1"))),
                &result.environment
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn loop_zero_iterations_and_limit() {
        let engine = engine();
        let zero = Arc::new(workflow(
            "zero",
            vec![step(
                "loop",
                StepKind::While {
                    condition: Condition::Equal(literal(json!(1)), literal(json!(2))),
                    steps: vec![step("never", StepKind::Stop)],
                },
            )],
        ));
        assert_eq!(
            engine
                .run(zero, Values::new(), CancellationToken::new())
                .await
                .outcome,
            Outcome::Success
        );
        let repeated = Arc::new(workflow(
            "repeated",
            vec![step(
                "loop",
                StepKind::While {
                    condition: Condition::Equal(literal(json!(1)), literal(json!(1))),
                    steps: vec![],
                },
            )],
        ));
        assert!(matches!(
            engine
                .run(repeated, Values::new(), CancellationToken::new())
                .await
                .outcome,
            Outcome::Failed(Failure {
                kind: FailureKind::LoopLimit,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn loop_may_finish_on_its_last_allowed_iteration() {
        struct CountUntilLimit(AtomicUsize);

        impl Capability for CountUntilLimit {
            fn default_deadline(&self) -> Duration {
                Duration::from_secs(1)
            }

            fn execute<'a>(
                &'a self,
                _: Values,
                _: CancellationToken,
            ) -> BoxFuture<'a, Result<Values, CapabilityError>> {
                Box::pin(async move {
                    let iteration = self.0.fetch_add(1, Ordering::SeqCst) + 1;
                    Ok(Values::from([(
                        "again".to_owned(),
                        json!(iteration < MAX_LOOP_ITERATIONS),
                    )]))
                })
            }
        }

        let mut engine = engine();
        let counter = Arc::new(CountUntilLimit(AtomicUsize::new(0)));
        engine.register_capability("count", 1, counter.clone());
        let loop_step = step(
            "loop",
            StepKind::While {
                condition: Condition::Equal(
                    Input::Variable {
                        name: "again".into(),
                        fallback: None,
                    },
                    literal(json!(true)),
                ),
                steps: vec![
                    action("count", "count"),
                    step(
                        "update",
                        StepKind::SetVariable {
                            name: "again".into(),
                            value: reference("count", "again"),
                        },
                    ),
                ],
            },
        );
        let flow = Arc::new(workflow(
            "at_limit",
            vec![
                step(
                    "initial",
                    StepKind::SetVariable {
                        name: "again".into(),
                        value: literal(json!(true)),
                    },
                ),
                loop_step,
            ],
        ));
        let result = engine
            .run(flow, Values::new(), CancellationToken::new())
            .await;
        assert_eq!(result.outcome, Outcome::Success);
        assert_eq!(counter.0.load(Ordering::SeqCst), MAX_LOOP_ITERATIONS);
    }

    #[tokio::test]
    async fn one_or_more_runs_every_child_and_cancellation_stops_it() {
        let mut engine = engine();
        let first = register(
            &mut engine,
            "fail",
            Err(CapabilityError::Failed("bad".into())),
            true,
        );
        let second = register(&mut engine, "ok", Ok(Values::new()), true);
        let flow = Arc::new(workflow(
            "flow",
            vec![step(
                "any",
                StepKind::OneOrMore {
                    steps: vec![action("bad", "fail"), action("good", "ok")],
                },
            )],
        ));
        let result = engine
            .run(flow, Values::new(), CancellationToken::new())
            .await;
        assert_eq!(result.outcome, Outcome::Success);
        assert_eq!(first.load(Ordering::SeqCst), 1);
        assert_eq!(second.load(Ordering::SeqCst), 1);
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let flow = Arc::new(workflow(
            "cancel",
            vec![step(
                "any",
                StepKind::OneOrMore {
                    steps: vec![action("later", "ok")],
                },
            )],
        ));
        assert_eq!(
            engine.run(flow, Values::new(), cancelled).await.outcome,
            Outcome::Cancelled
        );
        assert_eq!(second.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn inline_call_returns_variables_and_failed_child_does_not_merge() {
        let mut engine = engine();
        register(
            &mut engine,
            "fail",
            Err(CapabilityError::Failed("bad".into())),
            true,
        );
        engine
            .register_workflow(workflow(
                "child",
                vec![
                    step(
                        "update",
                        StepKind::SetVariable {
                            name: "name".into(),
                            value: literal(json!("new")),
                        },
                    ),
                    step("stop", StepKind::Stop),
                ],
            ))
            .unwrap();
        let call = step(
            "call",
            StepKind::Call {
                workflow_id: "child".into(),
                binding: CallBinding::AllNamedArguments,
            },
        );
        let flow = Arc::new(workflow(
            "parent",
            vec![
                step(
                    "initial",
                    StepKind::SetVariable {
                        name: "name".into(),
                        value: literal(json!("old")),
                    },
                ),
                call,
            ],
        ));
        let result = engine
            .run(flow, Values::new(), CancellationToken::new())
            .await;
        assert_eq!(result.outcome, Outcome::Success);
        assert_eq!(result.environment.variables["name"], json!("new"));
        engine
            .register_workflow(workflow(
                "child",
                vec![
                    step(
                        "update",
                        StepKind::SetVariable {
                            name: "name".into(),
                            value: literal(json!("new")),
                        },
                    ),
                    action("bad", "fail"),
                ],
            ))
            .unwrap();
        let flow = Arc::new(workflow(
            "parent",
            vec![
                step(
                    "initial",
                    StepKind::SetVariable {
                        name: "name".into(),
                        value: literal(json!("old")),
                    },
                ),
                step(
                    "call",
                    StepKind::Call {
                        workflow_id: "child".into(),
                        binding: CallBinding::AllNamedArguments,
                    },
                ),
            ],
        ));
        let result = engine
            .run(flow, Values::new(), CancellationToken::new())
            .await;
        assert!(matches!(result.outcome, Outcome::Failed(_)));
        assert_eq!(result.environment.variables["name"], json!("old"));
    }

    #[tokio::test]
    async fn form_cancel_and_delay_cancel_stop_later_effects() {
        let mut engine = engine();
        let calls = register(&mut engine, "effect", Ok(Values::new()), true);
        let form = Arc::new(workflow(
            "form",
            vec![
                step(
                    "input",
                    StepKind::RequestInput {
                        title: None,
                        fields: vec![InputField {
                            id: "title".into(),
                            label: None,
                            default: None,
                            required: true,
                            validator: None,
                        }],
                    },
                ),
                action("effect", "effect"),
            ],
        ));
        assert_eq!(
            engine
                .run(form, Values::new(), CancellationToken::new())
                .await
                .outcome,
            Outcome::Cancelled
        );
        let cancel = CancellationToken::new();
        let delayed = Arc::new(workflow(
            "delay",
            vec![
                step("wait", StepKind::Delay { millis: 10_000 }),
                action("effect", "effect"),
            ],
        ));
        let future = engine.run(delayed, Values::new(), cancel.clone());
        tokio::pin!(future);
        tokio::select! { _ = &mut future => panic!("delay ended early"), _ = tokio::time::sleep(Duration::from_millis(5)) => {} }
        cancel.cancel();
        assert_eq!(future.await.outcome, Outcome::Cancelled);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn connector_failure_is_immediate() {
        let mut engine = engine();
        let calls = register(&mut engine, "offline", Ok(Values::new()), false);
        let result = engine
            .run(
                Arc::new(workflow("flow", vec![action("action", "offline")])),
                Values::new(),
                CancellationToken::new(),
            )
            .await;
        assert!(matches!(
            result.outcome,
            Outcome::Failed(Failure {
                kind: FailureKind::ConnectorUnavailable,
                ..
            })
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn validation_checks_order_and_optional_branch_outputs() {
        let read = |id: &str| {
            step(
                id,
                StepKind::SetVariable {
                    name: "result".into(),
                    value: reference("producer", "value"),
                },
            )
        };
        assert!(
            validate_workflow(&workflow(
                "missing",
                vec![step(
                    "read",
                    StepKind::SetVariable {
                        name: "x".into(),
                        value: reference("absent", "value")
                    }
                )]
            ))
            .unwrap_err()
            .contains("unknown step")
        );
        assert!(
            validate_workflow(&workflow(
                "forward",
                vec![read("read"), action("producer", "produce")]
            ))
            .unwrap_err()
            .contains("not earlier")
        );
        let branch = step(
            "branch",
            StepKind::If {
                condition: Condition::Equal(literal(json!(true)), literal(json!(true))),
                then_steps: vec![action("producer", "produce")],
                else_steps: vec![],
            },
        );
        assert!(
            validate_workflow(&workflow("optional", vec![branch.clone(), read("read")]))
                .unwrap_err()
                .contains("may be absent")
        );
        let guarded = step(
            "guard",
            StepKind::If {
                condition: Condition::Exists(reference("producer", "value")),
                then_steps: vec![read("read")],
                else_steps: vec![],
            },
        );
        assert!(validate_workflow(&workflow("guarded", vec![branch.clone(), guarded])).is_ok());
        let fallback = step(
            "read",
            StepKind::SetVariable {
                name: "result".into(),
                value: Input::Reference {
                    step_id: "producer".into(),
                    output_id: "value".into(),
                    fallback: Some(Box::new(literal(json!("fallback")))),
                },
            },
        );
        assert!(validate_workflow(&workflow("fallback", vec![branch, fallback])).is_ok());
    }

    #[test]
    fn registered_output_contract_rejects_unknown_and_unguarded_optional_references() {
        use crate::schema::DescribeConfig;

        let mut engine = engine();
        engine.register_lua_action(
            OutputConfig::SCHEMA,
            Arc::new(Action {
                calls: Arc::new(AtomicUsize::new(0)),
                result: Ok(Values::new()),
                ready: true,
            }),
        );
        let producer = action("producer", "output_test");
        let read = |output_id| {
            step(
                "read",
                StepKind::SetVariable {
                    name: "result".into(),
                    value: reference("producer", output_id),
                },
            )
        };
        assert!(
            engine
                .validate_registered_workflow(&workflow(
                    "valid",
                    vec![producer.clone(), read("value")]
                ))
                .is_ok()
        );
        assert!(
            engine
                .validate_registered_workflow(&workflow(
                    "unknown",
                    vec![producer.clone(), read("typo")]
                ))
                .unwrap_err()
                .contains("has no output `typo`")
        );
        assert!(
            engine
                .validate_registered_workflow(&workflow(
                    "optional",
                    vec![producer.clone(), read("maybe")]
                ))
                .unwrap_err()
                .contains("may be absent")
        );
        let guarded = step(
            "guard",
            StepKind::If {
                condition: Condition::Exists(reference("producer", "maybe")),
                then_steps: vec![read("maybe")],
                else_steps: vec![],
            },
        );
        assert!(
            engine
                .validate_registered_workflow(&workflow("guarded", vec![producer.clone(), guarded]))
                .is_ok()
        );
        let misleading_guard = step(
            "guard",
            StepKind::If {
                condition: Condition::Exists(Input::Reference {
                    step_id: "producer".into(),
                    output_id: "maybe".into(),
                    fallback: Some(Box::new(literal(json!("default")))),
                }),
                then_steps: vec![read("maybe")],
                else_steps: vec![],
            },
        );
        assert!(
            engine
                .validate_registered_workflow(&workflow(
                    "misleading_guard",
                    vec![producer.clone(), misleading_guard],
                ))
                .unwrap_err()
                .contains("may be absent")
        );
        let fallback = step(
            "read",
            StepKind::SetVariable {
                name: "result".into(),
                value: Input::Reference {
                    step_id: "producer".into(),
                    output_id: "maybe".into(),
                    fallback: Some(Box::new(literal(json!("default")))),
                },
            },
        );
        assert!(
            engine
                .validate_registered_workflow(&workflow("fallback", vec![producer, fallback]))
                .is_ok()
        );
    }

    #[test]
    fn declared_outputs_are_checked_before_becoming_workflow_values() {
        use crate::schema::DescribeConfig;

        let schema = OutputConfig::SCHEMA;
        assert!(
            validate_declared_outputs(schema, &Values::from([("value".into(), json!("ok"))]))
                .is_ok()
        );
        assert!(
            validate_declared_outputs(schema, &Values::new())
                .unwrap_err()
                .contains("missing declared output")
        );
        assert!(
            validate_declared_outputs(schema, &Values::from([("value".into(), json!(2))]))
                .unwrap_err()
                .contains("wrong type")
        );
        assert!(
            validate_declared_outputs(
                schema,
                &Values::from([
                    ("value".into(), json!("ok")),
                    ("surprise".into(), json!(true))
                ]),
            )
            .unwrap_err()
            .contains("undeclared output")
        );
    }

    #[test]
    fn nested_lua_steps_are_validated_before_registration() {
        let mut engine = engine();
        engine.register_capability("lua.run", 1, Arc::new(LuaAction::new()));
        let script = step(
            "script",
            StepKind::Action {
                capability: "lua.run".into(),
                version: 1,
                inputs: BTreeMap::from([(
                    "source".into(),
                    literal(json!("return snenkbot.call('missing', {})")),
                )]),
                deadline_ms: None,
            },
        );
        let nested = step(
            "branch",
            StepKind::If {
                condition: Condition::Equal(literal(json!(true)), literal(json!(true))),
                then_steps: vec![script],
                else_steps: Vec::new(),
            },
        );
        let error = engine
            .register_workflow(workflow("invalid-lua", vec![nested]))
            .unwrap_err();
        assert!(error.contains("step `script`"));
        assert!(error.contains("unknown native function `missing`"));
        assert!(engine.workflow("invalid-lua").is_none());
    }

    #[test]
    fn structured_inputs_resolve_nested_references() {
        let environment = Environment {
            trigger: Values::from([("title".into(), json!("Hello"))]),
            variables: Values::from([("count".into(), json!(3))]),
            ..Environment::default()
        };
        let input = Input::Object(BTreeMap::from([
            (
                "title".into(),
                Input::Trigger {
                    name: "title".into(),
                    fallback: None,
                },
            ),
            (
                "items".into(),
                Input::Array(vec![Input::Variable {
                    name: "count".into(),
                    fallback: None,
                }]),
            ),
        ]));
        assert_eq!(
            evaluate(&input, &environment).unwrap(),
            json!({"title":"Hello","items":[3]})
        );
    }

    #[tokio::test]
    async fn failed_lua_step_records_failed_script_activity() {
        let mut engine = engine();
        engine.register_capability("lua.run", 1, Arc::new(LuaAction::new()));
        let workflow = workflow(
            "lua-error",
            vec![step(
                "script",
                StepKind::Action {
                    capability: "lua.run".into(),
                    version: 1,
                    inputs: BTreeMap::from([(
                        "source".into(),
                        literal(json!("error('bad script')")),
                    )]),
                    deadline_ms: None,
                },
            )],
        );
        engine.register_workflow(workflow.clone()).unwrap();
        let result = engine
            .run(Arc::new(workflow), Values::new(), CancellationToken::new())
            .await;
        assert!(matches!(result.outcome, Outcome::Failed(_)));
        assert_eq!(
            result.script_activity,
            vec![ScriptActivity {
                workflow_id: "lua-error".into(),
                step_id: "script".into(),
                outcome: ScriptOutcome::Failed,
            }]
        );
    }

    #[tokio::test]
    async fn plan_pins_called_workflow_revision() {
        let engine = engine();
        engine
            .register_workflow(workflow(
                "child",
                vec![step(
                    "set",
                    StepKind::SetVariable {
                        name: "answer".into(),
                        value: literal(json!("old")),
                    },
                )],
            ))
            .unwrap();
        let parent = Arc::new(workflow(
            "parent",
            vec![step(
                "call",
                StepKind::Call {
                    workflow_id: "child".into(),
                    binding: CallBinding::AllNamedArguments,
                },
            )],
        ));
        let plan = engine.snapshot(parent).unwrap();
        engine
            .register_workflow(workflow(
                "child",
                vec![step(
                    "set",
                    StepKind::SetVariable {
                        name: "answer".into(),
                        value: literal(json!("new")),
                    },
                )],
            ))
            .unwrap();
        let result = engine
            .run_plan(plan, Values::new(), CancellationToken::new())
            .await;
        assert_eq!(result.environment.variables["answer"], json!("old"));
    }

    #[tokio::test]
    async fn form_apply_publishes_fields_atomically() {
        let mut engine = Engine::new(
            Arc::new(Form {
                response: InputResponse::Applied(Values::from([
                    ("title".into(), json!("Ready")),
                    ("game".into(), json!("Chess")),
                ])),
            }),
            Arc::new(|_: Event| {}),
        );
        let fields = vec![
            InputField {
                id: "title".into(),
                label: None,
                default: None,
                required: true,
                validator: None,
            },
            InputField {
                id: "game".into(),
                label: None,
                default: None,
                required: true,
                validator: None,
            },
        ];
        let flow = Arc::new(workflow(
            "flow",
            vec![step(
                "form",
                StepKind::RequestInput {
                    title: None,
                    fields: fields.clone(),
                },
            )],
        ));
        let result = engine
            .run(flow, Values::new(), CancellationToken::new())
            .await;
        assert_eq!(result.environment.outputs["form"].len(), 2);
        engine.input = Arc::new(Form {
            response: InputResponse::Applied(Values::from([("title".into(), json!("Ready"))])),
        });
        let flow = Arc::new(workflow(
            "flow",
            vec![step(
                "form",
                StepKind::RequestInput {
                    title: None,
                    fields,
                },
            )],
        ));
        let result = engine
            .run(flow, Values::new(), CancellationToken::new())
            .await;
        assert!(matches!(
            result.outcome,
            Outcome::Failed(Failure {
                kind: FailureKind::Input,
                ..
            })
        ));
        assert!(!result.environment.outputs.contains_key("form"));
    }

    struct SlowAction;
    impl Capability for SlowAction {
        fn default_deadline(&self) -> Duration {
            Duration::from_millis(1)
        }
        fn execute<'a>(
            &'a self,
            _: Values,
            _: CancellationToken,
        ) -> BoxFuture<'a, Result<Values, CapabilityError>> {
            Box::pin(async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                Ok(Values::new())
            })
        }
    }

    #[tokio::test]
    async fn timeout_records_uncertain_effect_and_version_is_checked() {
        let mut engine = engine();
        engine.register_capability("slow", 1, Arc::new(SlowAction));
        let result = engine
            .run(
                Arc::new(workflow("flow", vec![action("slow", "slow")])),
                Values::new(),
                CancellationToken::new(),
            )
            .await;
        assert!(matches!(
            result.outcome,
            Outcome::Failed(Failure {
                kind: FailureKind::Timeout,
                remote_effect_uncertain: true,
                ..
            })
        ));
        let mut newer = action("unknown", "slow");
        if let StepKind::Action { version, .. } = &mut newer.kind {
            *version = 2;
        }
        let result = engine
            .run(
                Arc::new(workflow("flow", vec![newer])),
                Values::new(),
                CancellationToken::new(),
            )
            .await;
        assert!(matches!(
            result.outcome,
            Outcome::Failed(Failure {
                kind: FailureKind::CapabilityUnavailable,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn connector_can_report_an_uncertain_remote_effect() {
        let mut engine = engine();
        engine.register_capability(
            "chapter",
            1,
            Arc::new(Action {
                calls: Arc::new(AtomicUsize::new(0)),
                result: Err(CapabilityError::Uncertain("response lost".into())),
                ready: true,
            }),
        );
        let result = engine
            .run(
                Arc::new(workflow("flow", vec![action("chapter", "chapter")])),
                Values::new(),
                CancellationToken::new(),
            )
            .await;
        assert!(matches!(
            result.outcome,
            Outcome::Failed(Failure {
                kind: FailureKind::Action,
                remote_effect_uncertain: true,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn queue_overflow_is_recorded() {
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = {
            let events = events.clone();
            Arc::new(move |event: Event| events.lock().unwrap().push(event)) as Arc<dyn EventSink>
        };
        let queue = ActivationQueue::new(sink);
        let plan = engine()
            .snapshot(Arc::new(workflow("flow", vec![])))
            .unwrap();
        for _ in 0..MAX_PENDING_PER_WORKFLOW {
            queue
                .admit(plan.clone(), Values::new(), CancellationToken::new())
                .await
                .unwrap();
        }
        assert!(
            queue
                .admit(plan, Values::new(), CancellationToken::new())
                .await
                .is_err()
        );
        assert_eq!(queue.pending_count().await, MAX_PENDING_PER_WORKFLOW);
        assert!(
            matches!(events.lock().unwrap().last(), Some(Event::ActivationRejected { workflow_id, .. }) if workflow_id == "flow")
        );
    }

    #[tokio::test]
    async fn next_ready_wakes_when_capacity_is_released() {
        let queue = Arc::new(ActivationQueue::new(Arc::new(|_: Event| {})));
        let plan = engine()
            .snapshot(Arc::new(workflow("serial", vec![])))
            .unwrap();
        queue
            .admit(plan.clone(), Values::new(), CancellationToken::new())
            .await
            .unwrap();
        queue
            .admit(plan, Values::new(), CancellationToken::new())
            .await
            .unwrap();
        assert!(queue.take_ready().await.is_some());
        let cancel = CancellationToken::new();
        let waiting = {
            let queue = queue.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move { queue.next_ready(&cancel).await })
        };
        tokio::task::yield_now().await;
        queue.finished("serial").await;
        let ready = tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(ready.unwrap().plan.workflow.id, "serial");
    }

    #[tokio::test]
    async fn queue_keeps_revision_and_serial_or_overlap_policy() {
        let queue = ActivationQueue::new(Arc::new(|_: Event| {}));
        let first = Arc::new(workflow("flow", vec![]));
        let mut updated = workflow("flow", vec![]);
        updated.revision = 2;
        queue
            .admit(
                engine().snapshot(first).unwrap(),
                Values::new(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        queue
            .admit(
                engine().snapshot(Arc::new(updated)).unwrap(),
                Values::new(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(queue.take_ready().await.unwrap().plan.workflow.revision, 1);
        assert!(queue.take_ready().await.is_none());
        queue.finished("flow").await;
        assert_eq!(queue.take_ready().await.unwrap().plan.workflow.revision, 2);
        let mut overlap = workflow("parallel", vec![]);
        overlap.overlap = true;
        let overlap = Arc::new(overlap);
        for _ in 0..2 {
            queue
                .admit(
                    engine().snapshot(overlap.clone()).unwrap(),
                    Values::new(),
                    CancellationToken::new(),
                )
                .await
                .unwrap();
        }
        assert!(queue.take_ready().await.is_some());
        assert!(queue.take_ready().await.is_some());
    }

    #[tokio::test]
    async fn step_trace_links_nested_steps_and_called_workflow() {
        let engine = engine();
        engine
            .register_workflow(workflow(
                "child",
                vec![step("child-delay", StepKind::Delay { millis: 0 })],
            ))
            .unwrap();
        let parent = Arc::new(workflow(
            "parent",
            vec![
                step(
                    "branch",
                    StepKind::If {
                        condition: Condition::Exists(literal(json!(true))),
                        then_steps: vec![step(
                            "call",
                            StepKind::Call {
                                workflow_id: "child".into(),
                                binding: CallBinding::AllNamedArguments,
                            },
                        )],
                        else_steps: vec![],
                    },
                ),
                step("stop", StepKind::Stop),
            ],
        ));
        let result = engine
            .run(parent, Values::new(), CancellationToken::new())
            .await;
        assert_eq!(result.outcome, Outcome::Stopped);
        let trace = &result.step_trace;
        assert_eq!(trace.len(), 4);
        assert_eq!(
            trace.iter().map(|entry| entry.sequence).collect::<Vec<_>>(),
            [1, 2, 3, 4]
        );
        assert_eq!(
            trace
                .iter()
                .map(|entry| entry.parent_sequence)
                .collect::<Vec<_>>(),
            [None, Some(1), Some(2), None]
        );
        assert_eq!(
            trace
                .iter()
                .map(|entry| entry.step_id.as_str())
                .collect::<Vec<_>>(),
            ["branch", "call", "child-delay", "stop"]
        );
        assert_eq!(trace[2].workflow_id, "child");
        assert_eq!(
            trace[1].kind,
            StepTraceKind::Call {
                workflow_id: "child".into()
            }
        );
        assert_eq!(trace[3].outcome, StepTraceOutcome::Stopped);
        assert!(
            trace
                .iter()
                .all(|entry| entry.finished_at_ms >= entry.started_at_ms)
        );
    }

    #[tokio::test]
    async fn trace_repeated_loop_steps_have_distinct_sequences_under_one_parent() {
        let engine = engine();
        let flow = Arc::new(workflow(
            "loop",
            vec![step(
                "while",
                StepKind::While {
                    condition: Condition::Exists(literal(json!(true))),
                    steps: vec![step("again", StepKind::Delay { millis: 0 })],
                },
            )],
        ));
        let result = engine
            .run(flow, Values::new(), CancellationToken::new())
            .await;
        assert!(matches!(
            result.outcome,
            Outcome::Failed(Failure {
                kind: FailureKind::LoopLimit,
                ..
            })
        ));
        assert_eq!(result.step_trace.len(), MAX_LOOP_ITERATIONS + 1);
        for (index, entry) in result.step_trace.iter().enumerate().skip(1) {
            assert_eq!(entry.sequence, (index + 1) as u64);
            assert_eq!(entry.parent_sequence, Some(1));
            assert_eq!(entry.step_id, "again");
        }
        assert!(matches!(
            result.step_trace[0].outcome,
            StepTraceOutcome::Failed {
                kind: FailureKind::LoopLimit,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn trace_handled_failures_and_one_or_more_keep_typed_private_results() {
        let mut engine = engine();
        register(
            &mut engine,
            "uncertain",
            Err(CapabilityError::Uncertain("secret remote response".into())),
            true,
        );
        let mut continued = action("continued", "uncertain");
        continued.on_failure = FailurePolicy::Continue;
        let flow = Arc::new(workflow(
            "failure",
            vec![
                continued,
                step(
                    "choices",
                    StepKind::OneOrMore {
                        steps: vec![
                            action("failed-choice", "uncertain"),
                            step("good-choice", StepKind::Delay { millis: 0 }),
                        ],
                    },
                ),
            ],
        ));
        let result = engine
            .run(flow, Values::new(), CancellationToken::new())
            .await;
        assert_eq!(result.outcome, Outcome::Success);
        assert_eq!(result.step_trace.len(), 4);
        assert_eq!(
            result.step_trace[0].outcome,
            StepTraceOutcome::Failed {
                kind: FailureKind::Action,
                remote_effect_uncertain: true,
                continued_by_policy: true,
            }
        );
        assert_eq!(
            result.step_trace[2].outcome,
            StepTraceOutcome::Failed {
                kind: FailureKind::Action,
                remote_effect_uncertain: true,
                continued_by_policy: false,
            }
        );
        assert_eq!(result.step_trace[1].outcome, StepTraceOutcome::Succeeded);
        assert!(
            !serde_json::to_string(&result.step_trace)
                .unwrap()
                .contains("secret remote response")
        );
    }

    #[tokio::test]
    async fn trace_concurrent_runs_keep_independent_sequences() {
        let engine = engine();
        let first = Arc::new(workflow(
            "first",
            vec![step("one", StepKind::Delay { millis: 1 })],
        ));
        let second = Arc::new(workflow(
            "second",
            vec![step("two", StepKind::Delay { millis: 1 })],
        ));
        let (first, second) = tokio::join!(
            engine.run(first, Values::new(), CancellationToken::new()),
            engine.run(second, Values::new(), CancellationToken::new()),
        );
        assert_eq!(first.step_trace.len(), 1);
        assert_eq!(second.step_trace.len(), 1);
        assert_eq!(first.step_trace[0].sequence, 1);
        assert_eq!(second.step_trace[0].sequence, 1);
        assert_eq!(first.step_trace[0].workflow_id, "first");
        assert_eq!(second.step_trace[0].workflow_id, "second");
    }

    #[tokio::test]
    async fn trace_called_workflow_preserves_remote_effect_uncertainty() {
        let mut engine = engine();
        register(
            &mut engine,
            "remote",
            Err(CapabilityError::Uncertain(
                "private service response".into(),
            )),
            true,
        );
        engine
            .register_workflow(workflow("child", vec![action("remote-step", "remote")]))
            .unwrap();
        let parent = Arc::new(workflow(
            "parent",
            vec![step(
                "call",
                StepKind::Call {
                    workflow_id: "child".into(),
                    binding: CallBinding::AllNamedArguments,
                },
            )],
        ));
        let result = engine
            .run(parent, Values::new(), CancellationToken::new())
            .await;
        assert!(matches!(
            result.outcome,
            Outcome::Failed(Failure {
                remote_effect_uncertain: true,
                ..
            })
        ));
        assert_eq!(result.step_trace.len(), 2);
        assert_eq!(result.step_trace[1].parent_sequence, Some(1));
        assert!(matches!(
            result.step_trace[0].outcome,
            StepTraceOutcome::Failed {
                kind: FailureKind::CalledWorkflow,
                remote_effect_uncertain: true,
                ..
            }
        ));
        assert!(
            !serde_json::to_string(&result.step_trace)
                .unwrap()
                .contains("private service response")
        );
    }
}
