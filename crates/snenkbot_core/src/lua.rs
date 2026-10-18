//! Isolated Lua action for workflow data transformations and explicitly bound capabilities.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use full_moon::ast::{Call, Expression, FunctionArgs, FunctionCall, Index, Prefix, Suffix};
use full_moon::node::Node;
use full_moon::tokenizer::{TokenReference, TokenType};
use full_moon::visitors::Visitor;
use mlua::chunk::ChunkMode;
use mlua::{HookTriggers, Lua, LuaOptions, LuaSerdeExt, StdLib, VmState};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::engine::{Capability, CapabilityError, Values};

const MEMORY_LIMIT: usize = 16 * 1024 * 1024;
const INSTRUCTION_LIMIT: usize = 1_000_000;
const HOOK_INTERVAL: u32 = 1_000;
const EXECUTION_LIMIT: Duration = Duration::from_secs(5);
const SOURCE_LIMIT: usize = 256 * 1024;

/// The `lua.run` action accepts `source` and an optional object of `values`.
/// Scripts read `values`, call selected native actions with
/// `snenkbot.call(name, arguments)`, and return an object of step outputs.
/// A new Lua state is created for each run; only bound names can call Rust code.
#[derive(Default)]
pub struct LuaAction {
    bindings: BTreeMap<String, Arc<dyn Capability>>,
}

impl LuaAction {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_bindings(bindings: BTreeMap<String, Arc<dyn Capability>>) -> Self {
        Self { bindings }
    }

    pub fn bind(&mut self, name: impl Into<String>, capability: Arc<dyn Capability>) {
        self.bindings.insert(name.into(), capability);
    }

    fn validate_source(&self, source: &str) -> Result<(), String> {
        if source.len() > SOURCE_LIMIT {
            return Err("Lua source exceeds 256 KiB".into());
        }
        let ast = full_moon::parse(source).map_err(|errors| {
            errors
                .iter()
                .map(|error| format!("line {}: {}", error.range().0.line(), error.error_message()))
                .collect::<Vec<_>>()
                .join("; ")
        })?;
        let lua = Lua::new_with(StdLib::NONE, LuaOptions::default())
            .map_err(|error| error.to_string())?;
        lua.set_memory_limit(MEMORY_LIMIT)
            .map_err(|error| error.to_string())?;
        lua.load(source)
            .set_mode(ChunkMode::Text)
            .set_name("workflow script")
            .into_function()
            .map_err(|error| error.to_string())?;
        let mut visitor = NativeCallValidator {
            lua: &lua,
            bindings: &self.bindings,
            errors: Vec::new(),
        };
        visitor.visit_ast(&ast);
        if visitor.errors.is_empty() {
            Ok(())
        } else {
            Err(visitor.errors.join("; "))
        }
    }

    fn run<'a>(
        &'a self,
        inputs: Values,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<Values, CapabilityError>> + Send + 'a>> {
        Box::pin(async move {
            let source = match inputs.get("source") {
                Some(Value::String(source)) if source.len() <= SOURCE_LIMIT => source,
                Some(Value::String(_)) => return Err(failed("Lua source exceeds 256 KiB")),
                _ => return Err(failed("Lua source must be a string")),
            };
            let values = match inputs.get("values") {
                Some(Value::Object(values)) => Value::Object(values.clone()),
                None => Value::Object(Default::default()),
                _ => return Err(failed("Lua values must be an object")),
            };
            if inputs.keys().any(|key| key != "source" && key != "values") {
                return Err(failed("Lua action accepts only source and values"));
            }
            if cancel.is_cancelled() {
                return Err(failed("Lua action cancelled"));
            }
            let libs = StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::UTF8;
            let lua = Lua::new_with(libs, LuaOptions::default()).map_err(lua_error)?;
            lua.set_memory_limit(MEMORY_LIMIT).map_err(lua_error)?;
            let globals = lua.globals();
            for name in ["dofile", "loadfile", "collectgarbage", "print"] {
                globals.set(name, mlua::Value::Nil).map_err(lua_error)?;
            }
            globals
                .set("values", lua.to_value(&values).map_err(lua_error)?)
                .map_err(lua_error)?;

            let api = lua.create_table().map_err(lua_error)?;
            let bindings = self.bindings.clone();
            let native_cancel = cancel.clone();
            let native_error = Arc::new(Mutex::new(None));
            let call_error = Arc::clone(&native_error);
            let call = lua
                .create_async_function(move |lua, (name, args): (String, mlua::Value)| {
                    let binding = bindings.get(&name).cloned();
                    let cancel = native_cancel.clone();
                    let error_slot = Arc::clone(&call_error);
                    async move {
                        let binding = binding.ok_or_else(|| {
                            mlua::Error::runtime(format!("unknown native function `{name}`"))
                        })?;
                        let args: Value = lua.from_value(args)?;
                        let Value::Object(args) = args else {
                            return Err(mlua::Error::runtime("native arguments must be an object"));
                        };
                        binding.ready().map_err(|message| {
                            *error_slot
                                .lock()
                                .expect("Lua native error lock was poisoned") =
                                Some(CapabilityError::ConnectorUnavailable(message.clone()));
                            mlua::Error::runtime(message)
                        })?;
                        if cancel.is_cancelled() {
                            return Err(mlua::Error::runtime("Lua action cancelled"));
                        }
                        let output = binding
                            .execute(args.into_iter().collect(), cancel)
                            .await
                            .map_err(|error| {
                                let message = match &error {
                                    CapabilityError::ConnectorUnavailable(message)
                                    | CapabilityError::Failed(message)
                                    | CapabilityError::Uncertain(message) => message.clone(),
                                };
                                *error_slot
                                    .lock()
                                    .expect("Lua native error lock was poisoned") = Some(error);
                                mlua::Error::runtime(message)
                            })?;
                        lua.to_value(&output)
                    }
                })
                .map_err(lua_error)?;
            api.set("call", call).map_err(lua_error)?;
            globals.set("snenkbot", api).map_err(lua_error)?;

            let started = Instant::now();
            let instructions = AtomicUsize::new(0);
            lua.set_global_hook(
                HookTriggers::new().every_nth_instruction(HOOK_INTERVAL),
                move |_, _| {
                    let count = instructions.fetch_add(HOOK_INTERVAL as usize, Ordering::Relaxed)
                        + HOOK_INTERVAL as usize;
                    if cancel.is_cancelled() {
                        return Err(mlua::Error::runtime("Lua action cancelled"));
                    }
                    if count >= INSTRUCTION_LIMIT || started.elapsed() >= EXECUTION_LIMIT {
                        return Err(mlua::Error::runtime("Lua execution limit exceeded"));
                    }
                    Ok(VmState::Continue)
                },
            )
            .map_err(lua_error)?;
            let result: mlua::Result<mlua::Value> = lua
                .load(source)
                .set_mode(ChunkMode::Text)
                .set_name("workflow script")
                .call_async(())
                .await;
            if let Some(error) = native_error
                .lock()
                .expect("Lua native error lock was poisoned")
                .take()
            {
                let context = result.err().map(|error| error.to_string());
                return Err(match error {
                    CapabilityError::ConnectorUnavailable(message) => {
                        CapabilityError::ConnectorUnavailable(with_context(message, context))
                    }
                    CapabilityError::Failed(message) => {
                        CapabilityError::Failed(with_context(message, context))
                    }
                    CapabilityError::Uncertain(message) => {
                        CapabilityError::Uncertain(with_context(message, context))
                    }
                });
            }
            let result = result.map_err(lua_error)?;
            let result: Value = lua.from_value(result).map_err(lua_error)?;
            let Value::Object(result) = result else {
                return Err(failed("Lua script must return an object"));
            };
            Ok(result.into_iter().collect())
        })
    }
}

impl Capability for LuaAction {
    fn default_deadline(&self) -> Duration {
        EXECUTION_LIMIT
    }

    fn validate_inputs(
        &self,
        inputs: &BTreeMap<String, crate::engine::Input>,
    ) -> Result<(), String> {
        use crate::engine::Input;

        if inputs.keys().any(|key| key != "source" && key != "values") {
            return Err("Lua action accepts only source and values".into());
        }
        let source = match inputs.get("source") {
            Some(Input::Literal(Value::String(source))) => source,
            _ => return Err("Lua source must be a literal string".into()),
        };
        if matches!(inputs.get("values"), Some(Input::Literal(value)) if !value.is_object()) {
            return Err("Lua values must be an object".into());
        }
        self.validate_source(source)
    }

    fn execute<'a>(
        &'a self,
        inputs: Values,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<Values, CapabilityError>> + Send + 'a>> {
        self.run(inputs, cancel)
    }
}

struct NativeCallValidator<'a> {
    lua: &'a Lua,
    bindings: &'a BTreeMap<String, Arc<dyn Capability>>,
    errors: Vec<String>,
}

impl Visitor for NativeCallValidator<'_> {
    fn visit_function_call(&mut self, call: &FunctionCall) {
        let Prefix::Name(prefix) = call.prefix() else {
            return;
        };
        if prefix.token().to_string() != "snenkbot" {
            return;
        }
        let mut suffixes = call.suffixes();
        let is_call = match suffixes.next() {
            Some(Suffix::Index(Index::Dot { name, .. })) => name.token().to_string() == "call",
            Some(Suffix::Index(Index::Brackets {
                expression: Expression::String(name),
                ..
            })) => self.literal_string(name).as_deref() == Some("call"),
            _ => false,
        };
        if !is_call {
            return;
        }
        let name = match suffixes.next() {
            Some(Suffix::Call(Call::AnonymousCall(FunctionArgs::Parentheses {
                arguments,
                ..
            }))) => match arguments.iter().next() {
                Some(Expression::String(name)) => Some(name),
                _ => None,
            },
            Some(Suffix::Call(Call::AnonymousCall(FunctionArgs::String(name)))) => Some(name),
            _ => None,
        };
        let Some(name) = name else {
            return;
        };
        let Some(binding_name) = self.literal_string(name) else {
            return;
        };
        if !self.bindings.contains_key(&binding_name) {
            let line = call.start_position().map_or(1, |position| position.line());
            self.errors.push(format!(
                "line {line}: unknown native function `{binding_name}`"
            ));
        }
    }
}

impl NativeCallValidator<'_> {
    fn literal_string(&self, token: &TokenReference) -> Option<String> {
        if !matches!(token.token().token_type(), TokenType::StringLiteral { .. }) {
            return None;
        }
        let literal = format!("return {}", token.token());
        self.lua
            .load(&literal)
            .set_mode(ChunkMode::Text)
            .eval::<String>()
            .ok()
    }
}

fn failed(message: &str) -> CapabilityError {
    CapabilityError::Failed(message.to_owned())
}

fn lua_error(error: mlua::Error) -> CapabilityError {
    CapabilityError::Failed(error.to_string())
}

fn with_context(message: String, context: Option<String>) -> String {
    match context {
        Some(context) => format!("{message} ({context})"),
        None => message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Input;

    struct Add;

    struct Uncertain;

    impl Capability for Add {
        fn default_deadline(&self) -> Duration {
            Duration::from_secs(1)
        }

        fn execute<'a>(
            &'a self,
            inputs: Values,
            _cancel: CancellationToken,
        ) -> Pin<Box<dyn Future<Output = Result<Values, CapabilityError>> + Send + 'a>> {
            Box::pin(async move {
                let a = inputs["a"].as_i64().unwrap();
                let b = inputs["b"].as_i64().unwrap();
                Ok(Values::from([("sum".into(), Value::from(a + b))]))
            })
        }
    }

    impl Capability for Uncertain {
        fn default_deadline(&self) -> Duration {
            Duration::from_secs(1)
        }

        fn execute<'a>(
            &'a self,
            _inputs: Values,
            _cancel: CancellationToken,
        ) -> Pin<Box<dyn Future<Output = Result<Values, CapabilityError>> + Send + 'a>> {
            Box::pin(async {
                Err(CapabilityError::Uncertain(
                    "remote effect may have happened".into(),
                ))
            })
        }
    }

    fn input(source: &str, values: Value) -> Values {
        Values::from([
            ("source".into(), Value::from(source)),
            ("values".into(), values),
        ])
    }

    async fn run(
        action: &LuaAction,
        source: &str,
        values: Value,
    ) -> Result<Values, CapabilityError> {
        action
            .execute(input(source, values), CancellationToken::new())
            .await
    }

    #[tokio::test]
    async fn transforms_values_and_calls_explicit_native_binding() {
        let mut action = LuaAction::new();
        action.bind("test.add", Arc::new(Add));
        let output = run(
            &action,
            "local result = snenkbot.call('test.add', { a = values.first, b = 3 }); return { total = result.sum, label = values.label }",
            serde_json::json!({"first": 4, "label": "hello"}),
        )
        .await
        .unwrap();
        assert_eq!(output["total"], 7);
        assert_eq!(output["label"], "hello");
    }

    #[tokio::test]
    async fn each_run_has_a_fresh_state() {
        let action = LuaAction::new();
        run(&action, "leak = 17; return {}", serde_json::json!({}))
            .await
            .unwrap();
        let output = run(
            &action,
            "return { clean = leak == nil }",
            serde_json::json!({}),
        )
        .await
        .unwrap();
        assert_eq!(output["clean"], true);
    }

    #[tokio::test]
    async fn denies_ambient_file_process_and_module_access() {
        let action = LuaAction::new();
        let output = run(
            &action,
            "return { io = io == nil, os = os == nil, package = package == nil, debug = debug == nil, require = require == nil, loadfile = loadfile == nil, dofile = dofile == nil }",
            serde_json::json!({}),
        )
        .await
        .unwrap();
        assert!(output.values().all(|value| value == true));
    }

    #[tokio::test]
    async fn rejects_unbound_calls_and_reports_script_line() {
        let action = LuaAction::new();
        let error = run(
            &action,
            "local x = 1\nlocal y = snenkbot.call('missing', {})\nreturn { x = x + y }",
            serde_json::json!({}),
        )
        .await
        .unwrap_err();
        let CapabilityError::Failed(message) = error else {
            panic!("expected script failure");
        };
        assert!(message.contains("missing"));
        assert!(message.contains(":2:"), "{message}");
    }

    #[tokio::test]
    async fn preserves_uncertain_native_effects() {
        let mut action = LuaAction::new();
        action.bind("test.uncertain", Arc::new(Uncertain));
        let error = run(
            &action,
            "snenkbot.call('test.uncertain', {}); return {}",
            serde_json::json!({}),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(error, CapabilityError::Uncertain(message) if message.contains("remote effect") && message.contains(":1:"))
        );
    }

    #[tokio::test]
    async fn cancelled_run_does_not_execute() {
        let action = LuaAction::new();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = action
            .execute(input("return {}", serde_json::json!({})), cancel)
            .await
            .unwrap_err();
        assert!(matches!(error, CapabilityError::Failed(message) if message.contains("cancelled")));
    }

    #[tokio::test]
    async fn stops_nonterminating_scripts() {
        let action = LuaAction::new();
        let error = run(&action, "while true do end", serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(matches!(error, CapabilityError::Failed(message) if message.contains("limit")));
    }

    #[tokio::test]
    async fn rejects_invalid_values_and_result_shape() {
        let action = LuaAction::new();
        let error = run(&action, "return {}", Value::Null).await.unwrap_err();
        assert!(matches!(error, CapabilityError::Failed(message) if message.contains("values")));
        let error = run(&action, "return 3", serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(
            matches!(error, CapabilityError::Failed(message) if message.contains("return an object"))
        );
    }

    fn definition(source: &str) -> BTreeMap<String, Input> {
        BTreeMap::from([("source".into(), Input::Literal(Value::from(source)))])
    }

    #[test]
    fn validation_rejects_parse_errors_and_missing_direct_bindings() {
        let action = LuaAction::new();
        let error = action
            .validate_inputs(&definition("local x = "))
            .unwrap_err();
        assert!(error.contains("line 1"), "{error}");
        let error = action
            .validate_inputs(&definition("return snenkbot.call('missing', {})"))
            .unwrap_err();
        assert!(error.contains("line 1: unknown native function `missing`"));
    }

    #[test]
    fn validation_uses_syntax_tree_and_decodes_literal_names() {
        let mut action = LuaAction::new();
        action.bind("test.add", Arc::new(Add));
        action
            .validate_inputs(&definition(
                "-- snenkbot.call('missing', {})\nreturn { result = snenkbot.call('test\\46add', {a=1,b=2}) }",
            ))
            .unwrap();
        let error = action
            .validate_inputs(&definition("return snenkbot['call']('missing', {})"))
            .unwrap_err();
        assert!(error.contains("unknown native function `missing`"));
        let error = action
            .validate_inputs(&definition("return snenkbot.call 'missing'"))
            .unwrap_err();
        assert!(error.contains("unknown native function `missing`"));
    }

    #[test]
    fn validation_requires_static_source_and_object_values() {
        let action = LuaAction::new();
        let error = action
            .validate_inputs(&BTreeMap::from([(
                "source".into(),
                Input::Variable {
                    name: "script".into(),
                    fallback: None,
                },
            )]))
            .unwrap_err();
        assert!(error.contains("literal string"));
        let mut inputs = definition("return {}");
        inputs.insert("values".into(), Input::Literal(Value::Null));
        assert!(action.validate_inputs(&inputs).is_err());
    }
}
