use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use i_slint_backend_testing::ElementHandle;
use serde_json::json;
use slint::{ComponentHandle, Model};
use tokio_util::sync::CancellationToken;

use snenk_bot::engine::{
    FormValidator, FormValidators, InputField, InputProvider, InputResponse, Values,
};
use snenk_bot::ui::{AppWindow, UiInputProvider};

struct GameValidator;

impl FormValidator for GameValidator {
    fn validate<'a>(
        &'a self,
        value: &'a str,
        _: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>> {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            match value {
                "Good Game" => Ok("42".into()),
                _ => Err("game was not found".into()),
            }
        })
    }
}

#[test]
fn form_validation_retains_values_and_dropped_request_closes_overlay() {
    i_slint_backend_testing::init_integration_test_with_system_time();
    let window = AppWindow::new().unwrap();
    let provider = UiInputProvider::new(&window);
    let cancel = CancellationToken::new();
    let worker_cancel = cancel.clone();
    let (sender, receiver) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async move {
                let mut validators = FormValidators::default();
                validators.register("test.game", Arc::new(GameValidator));
                let fields = vec![InputField {
                    id: "game_id".into(),
                    label: Some("Game".into()),
                    default: None,
                    required: true,
                    validator: Some("test.game".into()),
                }];
                let response = provider
                    .request(
                        "Edit stream details".into(),
                        fields,
                        Values::from([("game_id".into(), json!("Current Game"))]),
                        &validators,
                        CancellationToken::new(),
                    )
                    .await
                    .unwrap();
                sender.send(response).unwrap();
                // The engine can cancel by dropping the request rather than awaiting its response.
                tokio::select! {
                    _ = worker_cancel.cancelled() => {},
                    _ = provider.request("Cancel run".into(), Vec::new(), Values::new(), &validators, worker_cancel.child_token()) => {},
                }
                sender.send(InputResponse::Cancelled).unwrap();
            });
    });

    let weak = window.as_weak();
    let started = Instant::now();
    let mut stage = 0;
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(5),
        move || {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "form test timed out"
            );
            let window = weak.upgrade().unwrap();
            match stage {
                0 if window.get_input_visible() => {
                    assert_eq!(window.get_input_title(), "Edit stream details");
                    assert_eq!(
                        window.get_input_fields().row_data(0).unwrap().value,
                        "Current Game"
                    );
                    ElementHandle::find_by_accessible_label(&window, "Game")
                        .next()
                        .unwrap()
                        .set_accessible_value("Invalid Game");
                    window.invoke_submit_input();
                    assert!(window.get_input_validating());
                    assert_eq!(
                        ElementHandle::find_by_accessible_label(&window, "Game")
                            .next()
                            .unwrap()
                            .accessible_enabled(),
                        Some(false)
                    );
                    assert_eq!(
                        ElementHandle::find_by_accessible_label(&window, "Cancel")
                            .next()
                            .unwrap()
                            .accessible_enabled(),
                        Some(true)
                    );
                    stage = 1;
                }
                1 if !window.get_input_error().is_empty() => {
                    assert!(window.get_input_visible());
                    assert!(!window.get_input_validating());
                    assert_eq!(
                        window.get_input_fields().row_data(0).unwrap().value,
                        "Invalid Game"
                    );
                    ElementHandle::find_by_accessible_label(&window, "Game")
                        .next()
                        .unwrap()
                        .set_accessible_value("Good Game");
                    window.invoke_submit_input();
                    stage = 2;
                }
                2 => {
                    if let Ok(response) = receiver.try_recv() {
                        assert_eq!(
                            response,
                            InputResponse::Applied(Values::from([("game_id".into(), json!("42"))]))
                        );
                        stage = 3;
                    }
                }
                3 if window.get_input_visible() && window.get_input_title() == "Cancel run" => {
                    cancel.cancel();
                    stage = 4;
                }
                4 if !window.get_input_visible() => {
                    if let Ok(response) = receiver.try_recv() {
                        assert_eq!(response, InputResponse::Cancelled);
                        slint::quit_event_loop().unwrap();
                        stage = 5;
                    }
                }
                _ => {}
            }
        },
    );
    slint::run_event_loop().unwrap();
    worker.join().unwrap();
}
