use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tokio_util::sync::CancellationToken;

use snenk_bot::engine::{
    Capability, CapabilityError, Engine, FormValidator, FormValidators, InputField, InputProvider,
    InputResponse, Outcome, Values,
};
use snenk_bot::lua::LuaAction;
use snenk_bot::workflows::{WorkflowDefinition, WorkflowRepository};

const CLEAN_TITLE: &str = include_str!("fixtures/stream_details/clean_title.lua");
const DEFINITIONS: [&str; 4] = [
    include_str!("fixtures/stream_details/game-marker.json"),
    include_str!("fixtures/stream_details/title-marker.json"),
    include_str!("fixtures/stream_details/edit-details.json"),
    include_str!("fixtures/stream_details/recording-details.json"),
];

#[derive(Default)]
struct ServiceState {
    calls: Vec<(&'static str, Values, tokio::time::Instant)>,
    title: String,
    game: String,
    fail_title: bool,
    recording: bool,
}

struct ServiceAction {
    id: &'static str,
    state: Arc<Mutex<ServiceState>>,
}

impl Capability for ServiceAction {
    fn default_deadline(&self) -> Duration {
        Duration::from_secs(5)
    }
    fn execute<'a>(
        &'a self,
        inputs: Values,
        _: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<Values, CapabilityError>> + Send + 'a>> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            state
                .calls
                .push((self.id, inputs.clone(), tokio::time::Instant::now()));
            match self.id {
                "twitch.get_channel" => Ok(Values::from([
                    ("title".into(), json!(state.title)),
                    ("game_name".into(), json!(state.game)),
                    ("game_id".into(), json!("7")),
                ])),
                "twitch.find_game" => {
                    assert_eq!(inputs["name"], "Good Game");
                    Ok(Values::from([
                        ("id".into(), json!("42")),
                        ("name".into(), json!("Good Game")),
                    ]))
                }
                "twitch.set_channel" => {
                    if let Some(title) = inputs.get("title") {
                        if state.fail_title {
                            return Err(CapabilityError::Failed("title rejected".into()));
                        }
                        state.title = title.as_str().unwrap().into();
                    } else {
                        assert_eq!(inputs["game_id"], "42");
                        state.game = "Good Game".into();
                    }
                    Ok(Values::new())
                }
                "obs.get_recording_status" => {
                    Ok(Values::from([("active".into(), json!(state.recording))]))
                }
                "obs.create_record_chapter" => Ok(Values::new()),
                _ => unreachable!(),
            }
        })
    }
}

struct GameValidator;
impl FormValidator for GameValidator {
    fn validate<'a>(
        &'a self,
        value: &'a str,
        _: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>> {
        Box::pin(async move {
            if value.eq_ignore_ascii_case("Good Game") {
                Ok("Good Game".into())
            } else {
                Err("game was not found".into())
            }
        })
    }
}

#[derive(Default)]
struct FormState {
    attempts: VecDeque<InputResponse>,
    defaults: Vec<Values>,
    errors: Vec<String>,
}
struct Form(Arc<Mutex<FormState>>);
impl InputProvider for Form {
    fn request<'a>(
        &'a self,
        title: String,
        fields: Vec<InputField>,
        defaults: Values,
        validators: &'a FormValidators,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<InputResponse, String>> + Send + 'a>> {
        Box::pin(async move {
            assert_eq!(title, "Edit stream details");
            self.0.lock().unwrap().defaults.push(defaults);
            loop {
                let candidate = self
                    .0
                    .lock()
                    .unwrap()
                    .attempts
                    .pop_front()
                    .expect("unexpected form request");
                match candidate {
                    InputResponse::Cancelled => return Ok(InputResponse::Cancelled),
                    InputResponse::Applied(values) => match validators
                        .validate(&fields, &values, cancel.child_token())
                        .await
                    {
                        Ok(values) => return Ok(InputResponse::Applied(values)),
                        Err(error) => self.0.lock().unwrap().errors.push(error),
                    },
                }
            }
        })
    }
}

fn setup(
    attempts: Vec<InputResponse>,
    fail_title: bool,
) -> (Engine, Arc<Mutex<ServiceState>>, Arc<Mutex<FormState>>) {
    let state = Arc::new(Mutex::new(ServiceState {
        title: " Hello 😄 world 🐉 old suffix ".into(),
        game: "Current Game".into(),
        fail_title,
        recording: true,
        ..ServiceState::default()
    }));
    let form = Arc::new(Mutex::new(FormState {
        attempts: attempts.into(),
        ..FormState::default()
    }));
    let mut engine = Engine::new(Arc::new(Form(form.clone())), Arc::new(|_| {}));
    for id in [
        "twitch.get_channel",
        "twitch.find_game",
        "twitch.set_channel",
        "obs.create_record_chapter",
        "obs.get_recording_status",
    ] {
        engine.register_capability(
            id,
            1,
            Arc::new(ServiceAction {
                id,
                state: state.clone(),
            }),
        );
    }
    engine.register_capability("lua.run", 1, Arc::new(LuaAction::new()));
    engine.register_form_validator("twitch.game", Arc::new(GameValidator));
    let directory = tempfile::tempdir().unwrap();
    let repository = WorkflowRepository::at(directory.path());
    for source in DEFINITIONS {
        let definition: WorkflowDefinition = serde_json::from_str(source).unwrap();
        for step in &definition.workflow.steps {
            if let snenk_bot::engine::StepKind::Action {
                capability, inputs, ..
            } = &step.kind
                && capability == "lua.run"
            {
                assert_eq!(
                    serde_json::to_value(&inputs["source"]).unwrap(),
                    json!({"Literal": CLEAN_TITLE})
                );
            }
        }
        repository.create_definition(&definition).unwrap();
        let loaded = repository.load(&definition.workflow.id).unwrap();
        engine.register_workflow(loaded.workflow().clone()).unwrap();
    }
    (engine, state, form)
}

fn apply(title: &str, game: &str) -> InputResponse {
    InputResponse::Applied(Values::from([
        ("customTitle".into(), json!(title)),
        ("customGame".into(), json!(game)),
    ]))
}

#[tokio::test]
async fn cleanup_preserves_other_emoji_and_trims_unicode_whitespace() {
    let action = LuaAction::new();
    for (title, expected) in [
        ("Hello 😄 world | suffix", "Hello 😄 world"),
        ("Hello 🐉 suffix", "Hello"),
        ("Hello 🪐 suffix", "Hello"),
        ("\u{a0}\u{202f}Hello 😄\u{3000}", "Hello 😄"),
        ("| suffix", ""),
        ("One | suffix\nTwo 🪐 suffix", "One\nTwo"),
        ("One | suffix\r\nTwo", "One\nTwo"),
    ] {
        let result = action
            .execute(
                Values::from([
                    ("source".into(), json!(CLEAN_TITLE)),
                    ("values".into(), json!({"targetChannelTitle":title})),
                ]),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result["cleanTitle"], expected, "{title:?}");
    }
    for values in [json!({}), json!({"targetChannelTitle":42})] {
        assert!(
            action
                .execute(
                    Values::from([
                        ("source".into(), json!(CLEAN_TITLE)),
                        ("values".into(), values)
                    ]),
                    CancellationToken::new()
                )
                .await
                .is_err()
        );
    }
}

#[tokio::test(start_paused = true)]
async fn edit_retries_invalid_game_then_updates_without_direct_chapters() {
    let (engine, state, form) = setup(
        vec![
            apply("New title", "missing"),
            apply("New title", "good game"),
        ],
        false,
    );
    let result = engine
        .run(
            engine.workflow("edit-details").unwrap(),
            Values::new(),
            CancellationToken::new(),
        )
        .await;
    assert_eq!(result.outcome, Outcome::Success);
    let form = form.lock().unwrap();
    assert_eq!(
        form.defaults,
        vec![Values::from([
            ("customTitle".into(), json!("Hello 😄 world")),
            ("customGame".into(), json!("Current Game"))
        ])]
    );
    assert_eq!(form.errors, ["Game: game was not found"]);
    let state = state.lock().unwrap();
    assert_eq!(state.title, "New title | Example channel");
    assert_eq!(state.game, "Good Game");
    assert_eq!(
        state.calls.iter().map(|call| call.0).collect::<Vec<_>>(),
        [
            "twitch.get_channel",
            "twitch.find_game",
            "twitch.set_channel",
            "twitch.set_channel"
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn empty_title_keeps_cleaned_current_title() {
    let (engine, state, _) = setup(vec![apply("", "Good Game")], false);
    let result = engine
        .run(
            engine.workflow("edit-details").unwrap(),
            Values::new(),
            CancellationToken::new(),
        )
        .await;
    assert_eq!(result.outcome, Outcome::Success);
    assert_eq!(
        state.lock().unwrap().title,
        "Hello 😄 world | Example channel"
    );
}

#[tokio::test(start_paused = true)]
async fn cancel_before_or_after_invalid_game_stops_all_mutations() {
    for attempts in [
        vec![InputResponse::Cancelled],
        vec![apply("New", "missing"), InputResponse::Cancelled],
    ] {
        let (engine, state, _) = setup(attempts, false);
        let result = engine
            .run(
                engine.workflow("edit-details").unwrap(),
                Values::new(),
                CancellationToken::new(),
            )
            .await;
        assert_eq!(result.outcome, Outcome::Cancelled);
        let state = state.lock().unwrap();
        assert_eq!(state.calls.len(), 1);
        assert_eq!(state.title, " Hello 😄 world 🐉 old suffix ");
        assert_eq!(state.game, "Current Game");
    }
}

#[tokio::test(start_paused = true)]
async fn failed_title_does_not_repeat_game_change_or_write_chapters() {
    let (engine, state, _) = setup(vec![apply("New", "Good Game")], true);
    let result = engine
        .run(
            engine.workflow("edit-details").unwrap(),
            Values::new(),
            CancellationToken::new(),
        )
        .await;
    assert!(matches!(result.outcome, Outcome::Failed(_)));
    let state = state.lock().unwrap();
    assert_eq!(state.game, "Good Game");
    assert_eq!(state.title, " Hello 😄 world 🐉 old suffix ");
    assert_eq!(state.calls.len(), 4);
}

#[tokio::test(start_paused = true)]
async fn recording_refresh_waits_and_only_writes_current_details_to_chapters() {
    let (engine, state, form) = setup(Vec::new(), false);
    let started = tokio::time::Instant::now();
    let result = engine
        .run(
            engine.workflow("recording-details").unwrap(),
            Values::new(),
            CancellationToken::new(),
        )
        .await;
    assert_eq!(result.outcome, Outcome::Success);
    assert!(form.lock().unwrap().defaults.is_empty());
    let state = state.lock().unwrap();
    assert_eq!(state.calls.len(), 4);
    assert_eq!(
        state.calls[1].2.duration_since(started),
        Duration::from_secs(2)
    );
    assert_eq!(state.calls[2].1["name"], "GAME CHANGE: Current Game");
    assert_eq!(state.calls[3].1["name"], "TITLE CHANGE: Hello 😄 world");
    assert_eq!(
        state.calls[3].2.duration_since(state.calls[2].2),
        Duration::from_secs(1)
    );
}

#[tokio::test(start_paused = true)]
async fn settled_channel_change_writes_one_pair_only_while_recording() {
    for recording in [true, false] {
        let (engine, state, _) = setup(Vec::new(), false);
        state.lock().unwrap().recording = recording;
        let started = tokio::time::Instant::now();
        let result = engine
            .run(
                engine.workflow("recording-details").unwrap(),
                Values::from([("event".into(), json!("channel.details_changed"))]),
                CancellationToken::new(),
            )
            .await;
        assert_eq!(result.outcome, Outcome::Success);
        let state = state.lock().unwrap();
        if recording {
            assert_eq!(state.calls.len(), 4);
            assert_eq!(state.calls[2].1["name"], "GAME CHANGE: Current Game");
            assert_eq!(state.calls[3].1["name"], "TITLE CHANGE: Hello 😄 world");
            assert_eq!(state.calls[2].2.duration_since(started), Duration::ZERO);
            assert_eq!(
                state.calls[3].2.duration_since(state.calls[2].2),
                Duration::from_secs(1)
            );
        } else {
            assert_eq!(
                state.calls.iter().map(|call| call.0).collect::<Vec<_>>(),
                ["obs.get_recording_status"]
            );
        }
    }
}

#[test]
fn saved_workflows_are_accepted_by_the_compiled_application_catalogs() {
    let directory = tempfile::tempdir().unwrap();
    let paths = snenk_bot::paths::AppPaths {
        config: directory.path().join("config"),
        data: directory.path().join("data"),
        state: directory.path().join("state"),
    };
    let repository = WorkflowRepository::new(&paths);
    for source in DEFINITIONS {
        let mut definition: WorkflowDefinition = serde_json::from_str(source).unwrap();
        // Validate the catalog without starting real account-backed listeners.
        for trigger in &mut definition.triggers {
            if matches!(
                trigger.kind,
                snenk_bot::workflows::TriggerKind::IntegrationEvent { .. }
            ) {
                trigger.enabled = false;
            }
        }
        repository.create_definition(&definition).unwrap();
    }
    let errors = Arc::new(Mutex::new(Vec::new()));
    let reported_errors = errors.clone();
    let mut runtime = snenk_bot::runtime::AppRuntime::new(|_| {}).unwrap();
    let services = snenk_bot::app::AppServices::start(
        &paths,
        &runtime,
        Arc::new(Form(Arc::new(Mutex::new(FormState::default())))),
        Arc::new(|_| {}),
        |_| {},
        move |error| reported_errors.lock().unwrap().push(error),
        |_| {},
    )
    .unwrap();
    let workflows = services.list_workflows();
    assert_eq!(workflows.len(), 4);
    for workflow in workflows {
        assert!(
            workflow.error.is_none(),
            "{}: {:?}",
            workflow.id,
            workflow.error
        );
        assert!(workflow.has_steps);
    }
    drop(services);
    runtime.shutdown(Duration::from_secs(5)).unwrap();
    assert!(
        errors.lock().unwrap().is_empty(),
        "{:?}",
        errors.lock().unwrap()
    );
}
