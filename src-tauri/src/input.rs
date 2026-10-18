use std::collections::BTreeMap;
use std::fmt;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use specta::Type;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use snenk_bot::engine::{FormValidators, InputField, InputProvider, InputResponse, Values};

#[derive(Clone, Debug, Serialize, Type)]
pub struct InputRequest {
    pub request_id: String,
    pub title: String,
    pub fields: Vec<InputField>,
    #[specta(type = BTreeMap<String, specta_typescript::Unknown>)]
    pub defaults: Values,
    #[specta(type = BTreeMap<String, specta_typescript::Unknown>)]
    pub values: Values,
    pub validating: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Type)]
#[serde(tag = "event", content = "payload", rename_all = "snake_case")]
pub enum InputEvent {
    Requested(InputRequest),
    Changed(InputRequest),
    Closed { request_id: String },
}

#[derive(Clone, Debug, Serialize, Type, PartialEq, Eq)]
#[serde(tag = "kind", content = "message", rename_all = "snake_case")]
pub enum InputBridgeError {
    NoActiveRequest,
    StaleRequest,
    Busy,
    EmissionFailed(String),
}

impl fmt::Display for InputBridgeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoActiveRequest => formatter.write_str("no input request is active"),
            Self::StaleRequest => formatter.write_str("the input request has changed"),
            Self::Busy => formatter.write_str("input validation is already in progress"),
            Self::EmissionFailed(error) => {
                write!(formatter, "input event delivery failed: {error}")
            }
        }
    }
}

impl std::error::Error for InputBridgeError {}

type EventSink = dyn Fn(InputEvent) -> Result<(), String> + Send + Sync;

pub struct DesktopInputProvider {
    state: Arc<InputState>,
    serial: tokio::sync::Mutex<()>,
    shutdown: CancellationToken,
}

struct InputState {
    pending: Mutex<Option<PendingRequest>>,
    emit: Box<EventSink>,
}

struct PendingRequest {
    request: InputRequest,
    sender: mpsc::Sender<Values>,
    lifetime: Arc<RequestLifetime>,
}

#[derive(Default)]
struct RequestLifetime {
    cancel: CancellationToken,
    emission_error: Mutex<Option<String>>,
}

impl RequestLifetime {
    fn emit(&self, sink: &EventSink, event: InputEvent) -> Result<(), InputBridgeError> {
        sink(event).map_err(|error| {
            *self
                .emission_error
                .lock()
                .expect("input error lock poisoned") = Some(error.clone());
            self.cancel.cancel();
            InputBridgeError::EmissionFailed(error)
        })
    }

    fn result(&self, response: InputResponse) -> Result<InputResponse, String> {
        match self
            .emission_error
            .lock()
            .expect("input error lock poisoned")
            .take()
        {
            Some(error) => Err(InputBridgeError::EmissionFailed(error).to_string()),
            None => Ok(response),
        }
    }
}

struct RequestGuard {
    state: Arc<InputState>,
    request_id: String,
}

impl RequestGuard {
    fn close(&self) -> Result<(), InputBridgeError> {
        let mut pending = self.state.pending.lock().expect("input lock poisoned");
        if !pending
            .as_ref()
            .is_some_and(|form| form.request.request_id == self.request_id)
        {
            return Ok(());
        }
        let form = pending.take().expect("current input request exists");
        form.lifetime.cancel.cancel();
        form.lifetime.emit(
            self.state.emit.as_ref(),
            InputEvent::Closed {
                request_id: self.request_id.clone(),
            },
        )
    }
}

impl Drop for RequestGuard {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

impl DesktopInputProvider {
    /// The callback runs synchronously in event order and must not reenter this provider.
    pub fn new(emit: impl Fn(InputEvent) -> Result<(), String> + Send + Sync + 'static) -> Self {
        Self {
            state: Arc::new(InputState {
                pending: Mutex::new(None),
                emit: Box::new(emit),
            }),
            serial: tokio::sync::Mutex::new(()),
            shutdown: CancellationToken::new(),
        }
    }

    pub fn current_request(&self) -> Option<InputRequest> {
        self.state
            .pending
            .lock()
            .expect("input lock poisoned")
            .as_ref()
            .map(|form| form.request.clone())
    }

    pub fn submit(&self, request_id: &str, values: Values) -> Result<(), InputBridgeError> {
        let mut pending = self.state.pending.lock().expect("input lock poisoned");
        let form = pending.as_mut().ok_or(InputBridgeError::NoActiveRequest)?;
        if form.request.request_id != request_id {
            return Err(InputBridgeError::StaleRequest);
        }
        if form.lifetime.cancel.is_cancelled() {
            return Err(InputBridgeError::NoActiveRequest);
        }
        if form.request.validating {
            return Err(InputBridgeError::Busy);
        }
        // Mark busy before admission so two commands cannot enqueue competing submissions.
        form.request.values = values.clone();
        form.request.validating = true;
        form.request.error = None;
        form.lifetime.emit(
            self.state.emit.as_ref(),
            InputEvent::Changed(form.request.clone()),
        )?;
        form.sender.try_send(values).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => InputBridgeError::Busy,
            mpsc::error::TrySendError::Closed(_) => InputBridgeError::NoActiveRequest,
        })
    }

    pub fn cancel(&self, request_id: &str) -> Result<(), InputBridgeError> {
        let pending = self.state.pending.lock().expect("input lock poisoned");
        let form = pending.as_ref().ok_or(InputBridgeError::NoActiveRequest)?;
        if form.request.request_id != request_id {
            return Err(InputBridgeError::StaleRequest);
        }
        form.lifetime.cancel.cancel();
        drop(pending);
        RequestGuard {
            state: Arc::clone(&self.state),
            request_id: request_id.to_owned(),
        }
        .close()
    }

    /// Stops the active form and all waiting requests when the host closes.
    pub fn shutdown(&self) -> Result<(), InputBridgeError> {
        self.shutdown.cancel();
        let mut pending = self.state.pending.lock().expect("input lock poisoned");
        let Some(form) = pending.take() else {
            return Ok(());
        };
        form.lifetime.cancel.cancel();
        form.lifetime.emit(
            self.state.emit.as_ref(),
            InputEvent::Closed {
                request_id: form.request.request_id,
            },
        )
    }
}

impl InputProvider for DesktopInputProvider {
    fn request<'a>(
        &'a self,
        title: String,
        fields: Vec<InputField>,
        defaults: Values,
        validators: &'a FormValidators,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<InputResponse, String>> + Send + 'a>> {
        Box::pin(async move {
            let _serial = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Ok(InputResponse::Cancelled),
                _ = self.shutdown.cancelled() => return Ok(InputResponse::Cancelled),
                guard = self.serial.lock() => guard,
            };
            let lifetime = Arc::new(RequestLifetime::default());
            let request_id = Uuid::new_v4().to_string();
            let (sender, mut receiver) = mpsc::channel(1);
            let values = fields
                .iter()
                .map(|field| {
                    let text = match defaults.get(&field.id) {
                        Some(serde_json::Value::String(text)) => text.clone(),
                        None | Some(serde_json::Value::Null) => String::new(),
                        Some(value) => value.to_string(),
                    };
                    (field.id.clone(), serde_json::Value::String(text))
                })
                .collect();
            let request = InputRequest {
                request_id: request_id.clone(),
                title,
                fields: fields.clone(),
                defaults,
                values,
                validating: false,
                error: None,
            };
            let guard = RequestGuard {
                state: Arc::clone(&self.state),
                request_id: request_id.clone(),
            };
            {
                let mut pending = self.state.pending.lock().expect("input lock poisoned");
                // Admission and shutdown can race before this lock is acquired.
                // Never publish a new form after shutdown has closed the pending slot.
                if self.shutdown.is_cancelled() || cancel.is_cancelled() {
                    return Ok(InputResponse::Cancelled);
                }
                *pending = Some(PendingRequest {
                    request: request.clone(),
                    sender,
                    lifetime: Arc::clone(&lifetime),
                });
                lifetime
                    .emit(self.state.emit.as_ref(), InputEvent::Requested(request))
                    .map_err(|error| error.to_string())?;
            }
            let response = loop {
                let values = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break InputResponse::Cancelled,
                    _ = self.shutdown.cancelled() => break InputResponse::Cancelled,
                    _ = lifetime.cancel.cancelled() => break InputResponse::Cancelled,
                    values = receiver.recv() => match values {
                        Some(values) => values,
                        None => break InputResponse::Cancelled,
                    },
                };
                let validation_cancel = lifetime.cancel.child_token();
                let validation = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => { validation_cancel.cancel(); break InputResponse::Cancelled; },
                    _ = self.shutdown.cancelled() => { validation_cancel.cancel(); break InputResponse::Cancelled; },
                    _ = lifetime.cancel.cancelled() => break InputResponse::Cancelled,
                    result = validators.validate(&fields, &values, validation_cancel.clone()) => result,
                };
                match validation {
                    Ok(values) => break InputResponse::Applied(values),
                    Err(error) => {
                        let mut pending = self.state.pending.lock().expect("input lock poisoned");
                        let Some(form) = pending
                            .as_mut()
                            .filter(|form| form.request.request_id == request_id)
                        else {
                            break InputResponse::Cancelled;
                        };
                        form.request.validating = false;
                        form.request.error = Some(error);
                        if lifetime
                            .emit(
                                self.state.emit.as_ref(),
                                InputEvent::Changed(form.request.clone()),
                            )
                            .is_err()
                        {
                            break InputResponse::Cancelled;
                        }
                    }
                }
            };
            let _ = guard.close();
            lifetime.result(response)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;
    use snenk_bot::engine::FormValidator;
    use tokio::task::JoinHandle;

    use super::*;

    fn fields() -> Vec<InputField> {
        vec![InputField {
            id: "name".into(),
            label: Some("Name".into()),
            default: None,
            required: true,
            validator: None,
        }]
    }

    fn values(value: serde_json::Value) -> Values {
        Values::from([("name".into(), value)])
    }

    fn provider() -> (
        Arc<DesktopInputProvider>,
        mpsc::UnboundedReceiver<InputEvent>,
    ) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (
            Arc::new(DesktopInputProvider::new(move |event| {
                sender.send(event).map_err(|error| error.to_string())
            })),
            receiver,
        )
    }

    fn start(
        provider: Arc<DesktopInputProvider>,
        cancel: CancellationToken,
    ) -> JoinHandle<Result<InputResponse, String>> {
        tokio::spawn(async move {
            provider
                .request(
                    "Input".into(),
                    fields(),
                    Values::new(),
                    &FormValidators::default(),
                    cancel,
                )
                .await
        })
    }

    async fn next(events: &mut mpsc::UnboundedReceiver<InputEvent>) -> InputEvent {
        tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("input event timed out")
            .expect("input event channel closed")
    }

    async fn requested(events: &mut mpsc::UnboundedReceiver<InputEvent>) -> InputRequest {
        match next(events).await {
            InputEvent::Requested(request) => request,
            event => panic!("expected request, got {event:?}"),
        }
    }

    #[tokio::test]
    async fn queued_request_can_cancel_without_disturbing_active_form() {
        let (provider, mut events) = provider();
        let active = start(Arc::clone(&provider), CancellationToken::new());
        let first = requested(&mut events).await;
        let cancel = CancellationToken::new();
        let queued = start(Arc::clone(&provider), cancel.clone());
        tokio::task::yield_now().await;
        cancel.cancel();
        assert_eq!(queued.await.unwrap().unwrap(), InputResponse::Cancelled);
        assert_eq!(
            provider.current_request().unwrap().request_id,
            first.request_id
        );
        assert!(events.try_recv().is_err());
        provider.cancel(&first.request_id).unwrap();
        assert_eq!(active.await.unwrap().unwrap(), InputResponse::Cancelled);
    }

    #[tokio::test]
    async fn stale_commands_cannot_affect_replacement_form() {
        let (provider, mut events) = provider();
        let first = start(Arc::clone(&provider), CancellationToken::new());
        let old = requested(&mut events).await;
        provider.cancel(&old.request_id).unwrap();
        first.await.unwrap().unwrap();
        assert!(matches!(next(&mut events).await, InputEvent::Closed { .. }));
        let second = start(Arc::clone(&provider), CancellationToken::new());
        let current = requested(&mut events).await;
        assert_ne!(old.request_id, current.request_id);
        assert_eq!(
            provider.submit(&old.request_id, values(json!("old"))),
            Err(InputBridgeError::StaleRequest)
        );
        assert_eq!(
            provider.cancel(&old.request_id),
            Err(InputBridgeError::StaleRequest)
        );
        assert!(!provider.current_request().unwrap().validating);
        provider
            .submit(&current.request_id, values(json!("current")))
            .unwrap();
        assert_eq!(
            second.await.unwrap().unwrap(),
            InputResponse::Applied(values(json!("current")))
        );
    }

    #[tokio::test]
    async fn rejected_values_stay_visible_and_can_be_corrected() {
        let (provider, mut events) = provider();
        let run = start(Arc::clone(&provider), CancellationToken::new());
        let request = requested(&mut events).await;
        for invalid in [json!(42), json!(" ")] {
            provider
                .submit(&request.request_id, values(invalid.clone()))
                .unwrap();
            assert!(matches!(
                next(&mut events).await,
                InputEvent::Changed(InputRequest {
                    validating: true,
                    ..
                })
            ));
            let InputEvent::Changed(rejected) = next(&mut events).await else {
                panic!("expected rejection")
            };
            assert!(!rejected.validating);
            assert!(rejected.error.is_some());
            assert_eq!(rejected.values, values(invalid));
        }
        provider
            .submit(&request.request_id, values(json!("valid")))
            .unwrap();
        assert_eq!(
            run.await.unwrap().unwrap(),
            InputResponse::Applied(values(json!("valid")))
        );
        assert!(provider.current_request().is_none());
    }

    struct CanonicalValidator;

    impl FormValidator for CanonicalValidator {
        fn validate<'a>(
            &'a self,
            value: &'a str,
            _cancel: CancellationToken,
        ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>> {
            Box::pin(async move {
                if value.trim() == "reject" {
                    return Err("choose another name".into());
                }
                Ok(value.trim().to_uppercase())
            })
        }
    }

    #[tokio::test]
    async fn integration_validator_rejection_and_canonical_result_are_preserved() {
        let (provider, mut events) = provider();
        let mut validators = FormValidators::default();
        validators.register("canonical", Arc::new(CanonicalValidator));
        let mut validated_fields = fields();
        validated_fields[0].validator = Some("canonical".into());
        let run_provider = Arc::clone(&provider);
        let run = tokio::spawn(async move {
            run_provider
                .request(
                    "Input".into(),
                    validated_fields,
                    Values::new(),
                    &validators,
                    CancellationToken::new(),
                )
                .await
        });
        let request = requested(&mut events).await;
        provider
            .submit(&request.request_id, values(json!("reject")))
            .unwrap();
        next(&mut events).await;
        let InputEvent::Changed(rejected) = next(&mut events).await else {
            panic!("expected rejection")
        };
        assert_eq!(rejected.error.as_deref(), Some("Name: choose another name"));
        assert_eq!(rejected.values, values(json!("reject")));
        provider
            .submit(&request.request_id, values(json!(" valid ")))
            .unwrap();
        assert_eq!(
            run.await.unwrap().unwrap(),
            InputResponse::Applied(values(json!("VALID")))
        );
    }

    struct BlockingValidator {
        started: mpsc::UnboundedSender<CancellationToken>,
    }

    impl FormValidator for BlockingValidator {
        fn validate<'a>(
            &'a self,
            _value: &'a str,
            cancel: CancellationToken,
        ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>> {
            Box::pin(async move {
                self.started.send(cancel).unwrap();
                std::future::pending().await
            })
        }
    }

    #[tokio::test]
    async fn duplicate_submit_is_rejected_and_cancel_stops_validation() {
        let (provider, mut events) = provider();
        let (started, mut started_receiver) = mpsc::unbounded_channel();
        let mut validators = FormValidators::default();
        validators.register("blocking", Arc::new(BlockingValidator { started }));
        let mut validated_fields = fields();
        validated_fields[0].validator = Some("blocking".into());
        let run_provider = Arc::clone(&provider);
        let run = tokio::spawn(async move {
            run_provider
                .request(
                    "Input".into(),
                    validated_fields,
                    Values::new(),
                    &validators,
                    CancellationToken::new(),
                )
                .await
        });
        let request = requested(&mut events).await;
        provider
            .submit(&request.request_id, values(json!("first")))
            .unwrap();
        assert_eq!(
            provider.submit(&request.request_id, values(json!("second"))),
            Err(InputBridgeError::Busy)
        );
        let validation_cancel =
            tokio::time::timeout(Duration::from_secs(2), started_receiver.recv())
                .await
                .unwrap()
                .unwrap();
        provider.cancel(&request.request_id).unwrap();
        assert_eq!(run.await.unwrap().unwrap(), InputResponse::Cancelled);
        assert!(validation_cancel.is_cancelled());
        assert!(provider.current_request().is_none());
    }

    #[tokio::test]
    async fn dropping_request_closes_it_and_releases_admission() {
        let (provider, mut events) = provider();
        let run = start(Arc::clone(&provider), CancellationToken::new());
        let request = requested(&mut events).await;
        run.abort();
        assert!(run.await.unwrap_err().is_cancelled());
        let InputEvent::Closed { request_id } = next(&mut events).await else {
            panic!("expected closed event")
        };
        assert_eq!(request_id, request.request_id);
        assert!(provider.current_request().is_none());
        let next_run = start(Arc::clone(&provider), CancellationToken::new());
        let replacement = requested(&mut events).await;
        provider.cancel(&replacement.request_id).unwrap();
        next_run.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn failed_emission_fails_request_and_cleans_snapshot() {
        let provider = DesktopInputProvider::new(|_| Err("receiver unavailable".into()));
        let result = provider
            .request(
                "Input".into(),
                fields(),
                Values::new(),
                &FormValidators::default(),
                CancellationToken::new(),
            )
            .await;
        assert!(result.unwrap_err().contains("receiver unavailable"));
        assert!(provider.current_request().is_none());
    }

    #[tokio::test]
    async fn failed_changed_or_closed_event_cannot_apply_input() {
        for fail_changed in [true, false] {
            let (sender, mut events) = mpsc::unbounded_channel();
            let provider = Arc::new(DesktopInputProvider::new(move |event| {
                if matches!(&event, InputEvent::Changed(_)) && fail_changed
                    || matches!(&event, InputEvent::Closed { .. }) && !fail_changed
                {
                    return Err("delivery failed".into());
                }
                sender.send(event).map_err(|error| error.to_string())
            }));
            let run = start(Arc::clone(&provider), CancellationToken::new());
            let request = requested(&mut events).await;
            let submit = provider.submit(&request.request_id, values(json!("valid")));
            assert_eq!(submit.is_err(), fail_changed);
            assert!(run.await.unwrap().unwrap_err().contains("delivery failed"));
            assert!(provider.current_request().is_none());
        }
    }

    #[tokio::test]
    async fn shutdown_cancels_active_and_queued_requests() {
        let (provider, mut events) = provider();
        let active = start(Arc::clone(&provider), CancellationToken::new());
        requested(&mut events).await;
        let queued = start(Arc::clone(&provider), CancellationToken::new());
        provider.shutdown().unwrap();
        provider.shutdown().unwrap();
        assert_eq!(active.await.unwrap().unwrap(), InputResponse::Cancelled);
        assert_eq!(queued.await.unwrap().unwrap(), InputResponse::Cancelled);
        assert!(provider.current_request().is_none());
        provider.shutdown().unwrap();
        assert!(matches!(next(&mut events).await, InputEvent::Closed { .. }));
        assert!(events.try_recv().is_err());
    }
}
