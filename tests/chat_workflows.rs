use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tokio_util::sync::CancellationToken;

use snenk_bot::engine::{
    Capability, CapabilityError, Engine, FormValidators, InputField, InputProvider, InputResponse,
    Outcome, Values,
};
use snenk_bot::lua::LuaAction;
use snenk_bot::twitch::events::{
    AdBreakBegin, ChatMessage, RouteOptions, TwitchEvent, decode_event, required_kinds, route_event,
};
use snenk_bot::workflows::{WorkflowDefinition, WorkflowRepository};

const DEFINITIONS: [&str; 8] = [
    include_str!("fixtures/chat_workflows/pronouns.json"),
    include_str!("fixtures/chat_workflows/discord.json"),
    include_str!("fixtures/chat_workflows/shoutout.json"),
    include_str!("fixtures/chat_workflows/lurk.json"),
    include_str!("fixtures/chat_workflows/unlurk.json"),
    include_str!("fixtures/chat_workflows/undertale.json"),
    include_str!("fixtures/chat_workflows/why.json"),
    include_str!("fixtures/chat_workflows/ads.json"),
];

struct NoInput;
impl InputProvider for NoInput {
    fn request<'a>(
        &'a self,
        _: String,
        _: Vec<InputField>,
        _: Values,
        _: &'a FormValidators,
        _: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<InputResponse, String>> + Send + 'a>> {
        Box::pin(async { panic!("chat workflows must not request input") })
    }
}

#[derive(Default)]
struct Effects {
    calls: Vec<(&'static str, Values)>,
    target_missing: bool,
    send_uncertain: bool,
}
struct Action {
    id: &'static str,
    effects: Arc<Mutex<Effects>>,
}
impl Capability for Action {
    fn default_deadline(&self) -> Duration {
        Duration::from_secs(5)
    }
    fn execute<'a>(
        &'a self,
        inputs: Values,
        _: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<Values, CapabilityError>> + Send + 'a>> {
        Box::pin(async move {
            let mut effects = self.effects.lock().unwrap();
            effects.calls.push((self.id, inputs.clone()));
            match self.id {
                "twitch.get_target_info" => {
                    if effects.target_missing || inputs["login"].as_str().unwrap().is_empty() {
                        return Err(CapabilityError::Failed("Twitch user was not found".into()));
                    }
                    assert_eq!(inputs["login"], "@Target");
                    Ok(Values::from([
                        ("id".into(), json!("42")),
                        ("login".into(), json!("target")),
                        ("display_name".into(), json!("Target")),
                        ("game_id".into(), json!("123")),
                        ("game_name".into(), json!("A Game 😄")),
                    ]))
                }
                "twitch.send_chat" => {
                    assert_eq!(inputs["broadcaster_fallback"], true);
                    if effects.send_uncertain {
                        return Err(CapabilityError::Uncertain("response lost".into()));
                    }
                    Ok(Values::from([("message_id".into(), json!("sent"))]))
                }
                _ => unreachable!(),
            }
        })
    }
}

fn setup() -> (Engine, Vec<WorkflowDefinition>, Arc<Mutex<Effects>>) {
    let directory = tempfile::tempdir().unwrap();
    let repository = WorkflowRepository::at(directory.path());
    let effects = Arc::new(Mutex::new(Effects::default()));
    let mut engine = Engine::new(Arc::new(NoInput), Arc::new(|_| {}));
    for id in ["twitch.get_target_info", "twitch.send_chat"] {
        engine.register_capability(
            id,
            1,
            Arc::new(Action {
                id,
                effects: effects.clone(),
            }),
        );
    }
    engine.register_capability("lua.run", 1, Arc::new(LuaAction::new()));
    let definitions = DEFINITIONS
        .iter()
        .map(|source| {
            let definition: WorkflowDefinition = serde_json::from_str(source).unwrap();
            repository.create_definition(&definition).unwrap();
            let definition = repository
                .load(&definition.workflow.id)
                .unwrap()
                .definition()
                .clone();
            engine
                .register_workflow(definition.workflow.clone())
                .unwrap();
            definition
        })
        .collect();
    (engine, definitions, effects)
}

fn message(text: &str) -> TwitchEvent {
    decode_event(&json!({
        "payload": {
            "subscription": {"type":"channel.chat.message", "version":"1"},
            "event": {
                "broadcaster_user_id":"100", "broadcaster_user_login":"channel", "broadcaster_user_name":"Channel",
                "chatter_user_id":"200", "chatter_user_login":"viewer", "chatter_user_name":"Náme",
                "message_id":"message", "message":{"text":text}, "badges":[],
                "source_broadcaster_user_id":null
            }
        }
    })).unwrap()
}

fn message_mut(event: &mut TwitchEvent) -> &mut ChatMessage {
    let TwitchEvent::Message(message) = event else {
        panic!("expected message")
    };
    message
}

#[tokio::test]
async fn every_public_command_alias_runs_only_its_saved_response() {
    let (engine, definitions, effects) = setup();
    for (command, id, expected) in [
        ("!PRONOUNS extra", "pronouns", "Pronoun information."),
        ("!discord", "discord", "Community information."),
        ("!lurk", "lurk", "Enjoy lurking, Náme!"),
        ("!UNLURK trailing", "unlurk", "Welcome back, Náme!"),
        ("!blind", "undertale", "No spoilers or backseating, please."),
        ("!FIRST", "undertale", "No spoilers or backseating, please."),
        (
            "!backseating",
            "undertale",
            "No spoilers or backseating, please.",
        ),
        ("!cocksize", "why", "A short response."),
    ] {
        let routes = route_event(&message(command), &definitions, RouteOptions::default());
        assert_eq!(routes.len(), 1, "{command}");
        assert_eq!(routes[0].workflow_id, id);
        let result = engine
            .run(
                engine.workflow(id).unwrap(),
                routes[0].values.clone(),
                CancellationToken::new(),
            )
            .await;
        assert_eq!(result.outcome, Outcome::Success);
        let mut effects = effects.lock().unwrap();
        assert_eq!(effects.calls.len(), 1);
        assert_eq!(effects.calls[0].0, "twitch.send_chat");
        assert_eq!(effects.calls[0].1["message"], expected);
        effects.calls.clear();
    }
    for text in [
        " !lurk",
        "hello !lurk",
        "!lurkish",
        "!unknown",
        "!so @Target",
    ] {
        assert!(
            route_event(&message(text), &definitions, RouteOptions::default()).is_empty(),
            "{text}"
        );
    }
}

#[tokio::test]
async fn shoutout_requires_moderator_or_broadcaster_and_resolves_first_argument() {
    let (engine, definitions, effects) = setup();
    for broadcaster in [false, true] {
        let mut event = message("!SO @Target ignored words");
        message_mut(&mut event).is_broadcaster = broadcaster;
        message_mut(&mut event).is_moderator = !broadcaster;
        let routes = route_event(&event, &definitions, RouteOptions::default());
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].values["arguments"], "@Target ignored words");
        let result = engine
            .run(
                engine.workflow("shoutout").unwrap(),
                routes[0].values.clone(),
                CancellationToken::new(),
            )
            .await;
        assert_eq!(result.outcome, Outcome::Success);
        let mut effects = effects.lock().unwrap();
        assert_eq!(effects.calls.len(), 2);
        assert_eq!(effects.calls[0].0, "twitch.get_target_info");
        assert_eq!(effects.calls[1].0, "twitch.send_chat");
        assert_eq!(
            effects.calls[1].1["message"],
            "Visit @Target playing A Game 😄: https://twitch.tv/@Target"
        );
        effects.calls.clear();
    }
}

#[test]
fn commands_exclude_bot_internal_and_disabled_triggers() {
    let (_, mut definitions, _) = setup();
    for text in [
        "!pronouns",
        "!discord",
        "!so @Target",
        "!lurk",
        "!unlurk",
        "!blind",
        "!first",
        "!backseating",
        "!cocksize",
    ] {
        for mode in 0..3 {
            let mut event = message(text);
            let message = message_mut(&mut event);
            message.is_moderator = true;
            message.is_bot = mode == 0;
            message.is_source_only = mode == 1;
            if mode == 2 {
                message.chatter_user_id = "bot".into();
            }
            assert!(
                route_event(
                    &event,
                    &definitions,
                    RouteOptions {
                        bot_user_id: Some("bot")
                    }
                )
                .is_empty()
            );
        }
    }
    for definition in &mut definitions {
        definition.triggers[0].enabled = false;
    }
    assert!(route_event(&message("!discord"), &definitions, RouteOptions::default()).is_empty());
}

#[tokio::test]
async fn missing_shoutout_target_stops_without_sending_or_retrying() {
    for text in ["!so", "!so @Target"] {
        let (engine, definitions, effects) = setup();
        effects.lock().unwrap().target_missing = true;
        let mut event = message(text);
        message_mut(&mut event).is_moderator = true;
        let routes = route_event(&event, &definitions, RouteOptions::default());
        assert_eq!(routes.len(), 1);
        let result = engine
            .run(
                engine.workflow("shoutout").unwrap(),
                routes[0].values.clone(),
                CancellationToken::new(),
            )
            .await;
        assert!(matches!(result.outcome, Outcome::Failed(_)));
        let effects = effects.lock().unwrap();
        assert_eq!(effects.calls.len(), 1);
        assert_eq!(effects.calls[0].0, "twitch.get_target_info");
    }
}

#[tokio::test]
async fn uncertain_chat_response_is_not_retried_or_replaced() {
    let (engine, definitions, effects) = setup();
    effects.lock().unwrap().send_uncertain = true;
    let routes = route_event(&message("!lurk"), &definitions, RouteOptions::default());
    let result = engine
        .run(
            engine.workflow("lurk").unwrap(),
            routes[0].values.clone(),
            CancellationToken::new(),
        )
        .await;
    assert!(matches!(result.outcome, Outcome::Failed(_)));
    assert_eq!(effects.lock().unwrap().calls.len(), 1);
}

#[tokio::test]
async fn ad_start_sends_only_the_ad_response_and_demand_is_deduplicated() {
    let (engine, definitions, effects) = setup();
    let kinds = required_kinds(&definitions).unwrap();
    assert_eq!(kinds.len(), 2);
    let event = TwitchEvent::AdBreakBegin(AdBreakBegin {
        broadcaster_user_id: "100".into(),
        broadcaster_user_login: "channel".into(),
        broadcaster_user_name: "Channel".into(),
        duration_seconds: 90,
        started_at: "2026-09-30T18:00:00Z".into(),
        is_automatic: true,
    });
    let routes = route_event(&event, &definitions, RouteOptions::default());
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].workflow_id, "ads");
    let result = engine
        .run(
            engine.workflow("ads").unwrap(),
            routes[0].values.clone(),
            CancellationToken::new(),
        )
        .await;
    assert_eq!(result.outcome, Outcome::Success);
    let effects = effects.lock().unwrap();
    assert_eq!(effects.calls.len(), 1);
    assert_eq!(effects.calls[0].1["message"], "An ad break has started.");
}

#[test]
fn all_twelve_saved_workflows_load_with_the_real_compiled_catalogs() {
    let directory = tempfile::tempdir().unwrap();
    let paths = snenk_bot::paths::AppPaths {
        config: directory.path().join("config"),
        data: directory.path().join("data"),
        state: directory.path().join("state"),
    };
    let repository = WorkflowRepository::new(&paths);
    for source in DEFINITIONS.into_iter().chain([
        include_str!("fixtures/stream_details/game-marker.json"),
        include_str!("fixtures/stream_details/title-marker.json"),
        include_str!("fixtures/stream_details/edit-details.json"),
        include_str!("fixtures/stream_details/recording-details.json"),
    ]) {
        repository
            .create_definition(&serde_json::from_str(source).unwrap())
            .unwrap();
    }
    let errors = Arc::new(Mutex::new(Vec::new()));
    let reported_errors = errors.clone();
    let mut runtime = snenk_bot::runtime::AppRuntime::new(|_| {}).unwrap();
    let services = snenk_bot::app::AppServices::start(
        &paths,
        &runtime,
        Arc::new(NoInput),
        Arc::new(|_| {}),
        |_| {},
        move |error| reported_errors.lock().unwrap().push(error),
        |_| {},
    )
    .unwrap();
    let workflows = services.list_workflows();
    assert_eq!(workflows.len(), 12);
    for workflow in workflows {
        assert!(
            workflow.error.is_none(),
            "{}: {:?}",
            workflow.id,
            workflow.error
        );
    }
    drop(services);
    runtime.shutdown(Duration::from_secs(5)).unwrap();
    assert!(errors.lock().unwrap().is_empty());
}
