use std::pin::Pin;
use std::sync::{Arc, Mutex};

use slint::{ComponentHandle, Model, ModelRc, VecModel};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::engine::{FormValidators, InputField, InputProvider, InputResponse, Values};
use crate::paths::AppPaths;
use crate::runtime::RuntimeEvent;

slint::include_modules!();

mod connections;
pub use connections::{connect_reconfiguration, refresh_connections};
mod library;
mod workflow;
pub use library::connect_library;
pub use workflow::WorkflowPreview;
mod control_flow;
mod editor_controller;
pub use editor_controller::connect_editor;
mod workflow_creation;
pub use workflow_creation::connect_workflow_creation;
mod run_history;
pub use run_history::connect_run_history;
mod vtube;
pub use vtube::connect_vtube;

pub struct UiInputProvider {
    window: slint::Weak<AppWindow>,
    pending: Arc<Mutex<Option<PendingForm>>>,
    serial: tokio::sync::Mutex<()>,
}

struct PendingForm {
    id: Arc<()>,
    sender: mpsc::UnboundedSender<InputResponse>,
    cancel: CancellationToken,
}

struct FormGuard {
    id: Arc<()>,
    window: slint::Weak<AppWindow>,
    pending: Arc<Mutex<Option<PendingForm>>>,
}

fn is_current_form(pending: &Mutex<Option<PendingForm>>, id: &Arc<()>) -> bool {
    pending
        .lock()
        .expect("input lock poisoned")
        .as_ref()
        .is_some_and(|form| Arc::ptr_eq(&form.id, id))
}

impl Drop for FormGuard {
    fn drop(&mut self) {
        let mut pending = self.pending.lock().expect("input lock poisoned");
        if pending
            .as_ref()
            .is_some_and(|form| Arc::ptr_eq(&form.id, &self.id))
            && let Some(form) = pending.take()
        {
            form.cancel.cancel();
        }
        drop(pending);
        let pending = Arc::clone(&self.pending);
        let _ = self.window.upgrade_in_event_loop(move |window| {
            if pending.lock().expect("input lock poisoned").is_none() {
                window.set_input_visible(false);
                window.set_input_validating(false);
            }
        });
    }
}

impl UiInputProvider {
    pub fn new(window: &AppWindow) -> Self {
        let pending: Arc<Mutex<Option<PendingForm>>> = Arc::new(Mutex::new(None));
        let weak = window.as_weak();
        window.on_input_changed(move |index, value| {
            if let Some(window) = weak.upgrade() {
                let model = window.get_input_fields();
                if let Some(mut field) = model.row_data(index as usize) {
                    field.value = value;
                    model.set_row_data(index as usize, field);
                    window.set_input_error("".into());
                }
            }
        });
        let weak = window.as_weak();
        let submit_pending = Arc::clone(&pending);
        window.on_submit_input(move || {
            let Some(window) = weak.upgrade() else {
                return;
            };
            if window.get_input_validating() {
                return;
            }
            let model = window.get_input_fields();
            let mut values = Values::new();
            for index in 0..model.row_count() {
                let Some(field) = model.row_data(index) else {
                    continue;
                };
                if field.required && field.value.trim().is_empty() {
                    window.set_input_error(format!("{} is required", field.label).into());
                    return;
                }
                values.insert(
                    field.id.to_string(),
                    serde_json::Value::String(field.value.to_string()),
                );
            }
            if let Some(pending) = submit_pending.lock().expect("input lock poisoned").as_ref() {
                window.set_input_validating(true);
                let _ = pending.sender.send(InputResponse::Applied(values));
            } else {
                window.set_input_visible(false);
            }
        });
        let weak = window.as_weak();
        let cancel_pending = Arc::clone(&pending);
        window.on_cancel_input(move || {
            if let Some(window) = weak.upgrade() {
                window.set_input_visible(false);
            }
            if let Some(pending) = cancel_pending.lock().expect("input lock poisoned").take() {
                pending.cancel.cancel();
                let _ = pending.sender.send(InputResponse::Cancelled);
            }
        });
        Self {
            window: window.as_weak(),
            pending,
            serial: tokio::sync::Mutex::new(()),
        }
    }
}

impl InputProvider for UiInputProvider {
    fn request<'a>(
        &'a self,
        title: String,
        fields: Vec<InputField>,
        defaults: Values,
        validators: &'a FormValidators,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<InputResponse, String>> + Send + 'a>> {
        Box::pin(async move {
            let _guard = self.serial.lock().await;
            let rows: Vec<PromptField> = fields
                .iter()
                .map(|field| PromptField {
                    value: defaults
                        .get(&field.id)
                        .map(|value| match value {
                            serde_json::Value::String(text) => text.clone(),
                            serde_json::Value::Null => String::new(),
                            value => value.to_string(),
                        })
                        .unwrap_or_default()
                        .into(),
                    id: field.id.clone().into(),
                    label: field
                        .label
                        .clone()
                        .unwrap_or_else(|| field.id.clone())
                        .into(),
                    required: field.required,
                })
                .collect();
            let (sender, mut receiver) = mpsc::unbounded_channel();
            let form_cancel = CancellationToken::new();
            let id = Arc::new(());
            *self.pending.lock().expect("input lock poisoned") = Some(PendingForm {
                id: Arc::clone(&id),
                sender,
                cancel: form_cancel.clone(),
            });
            let _form = FormGuard {
                id: Arc::clone(&id),
                window: self.window.clone(),
                pending: Arc::clone(&self.pending),
            };
            let show_pending = Arc::clone(&self.pending);
            let show_id = Arc::clone(&id);
            if let Err(error) = self.window.upgrade_in_event_loop(move |window| {
                if !is_current_form(&show_pending, &show_id) {
                    return;
                }
                window.set_input_title(title.into());
                window.set_input_error("".into());
                window.set_input_validating(false);
                window.set_input_fields(ModelRc::new(VecModel::from(rows)));
                window.set_input_visible(true);
            }) {
                return Err(format!("input window is unavailable: {error}"));
            }
            loop {
                tokio::select! {
                    response = receiver.recv() => match response {
                        Some(InputResponse::Applied(values)) => {
                            let validation = tokio::select! {
                                result = validators.validate(&fields, &values, cancel.child_token()) => result,
                                _ = form_cancel.cancelled() => return Ok(InputResponse::Cancelled),
                                _ = cancel.cancelled() => return Ok(InputResponse::Cancelled),
                            };
                            match validation {
                                Ok(values) => {
                                    return Ok(InputResponse::Applied(values));
                                }
                                Err(error) => {
                                    let error_pending = Arc::clone(&self.pending);
                                    let error_id = Arc::clone(&id);
                                    let _ = self.window.upgrade_in_event_loop(move |window| {
                                        if !is_current_form(&error_pending, &error_id) {
                                            return;
                                        }
                                        window.set_input_error(error.into());
                                        window.set_input_validating(false);
                                    });
                                }
                            }
                        }
                        Some(InputResponse::Cancelled) => return Ok(InputResponse::Cancelled),
                        None => return Err("input window closed before a response".to_owned()),
                    },
                    _ = cancel.cancelled() => return Ok(InputResponse::Cancelled),
                }
            }
        })
    }
}

pub fn create_window(paths: &AppPaths) -> Result<AppWindow, slint::PlatformError> {
    let window = AppWindow::new()?;
    connect_library(&window);
    window.on_filter_history(|rows, failed, query| {
        let query = query.trim().to_lowercase();
        ModelRc::new(VecModel::from(
            rows.iter()
                .filter(|row| {
                    (!failed || row.outcome == "Failed" || row.is_corrupt)
                        && (row.title.to_lowercase().contains(&query)
                            || row.trigger.to_lowercase().contains(&query)
                            || row.error.to_lowercase().contains(&query))
                })
                .collect::<Vec<_>>(),
        ))
    });
    window.set_data_path(paths.workflows_dir().to_string_lossy().into_owned().into());
    window.set_config_path(
        paths
            .integrations_dir()
            .to_string_lossy()
            .into_owned()
            .into(),
    );
    window.set_history_path(paths.history_dir().to_string_lossy().into_owned().into());

    let weak = window.as_weak();
    window.on_dismiss_error(move || {
        if let Some(window) = weak.upgrade() {
            window.set_error_message("".into());
        }
    });
    Ok(window)
}

/// Background tasks pass owned events, never UI model or component handles.
pub fn report_runtime_event(window: &slint::Weak<AppWindow>, event: RuntimeEvent) {
    let message = match event {
        RuntimeEvent::TaskFailed { name, message } => format!("{name}: {message}"),
        RuntimeEvent::TaskPanicked { name, message } => {
            format!("{name} stopped unexpectedly: {message}")
        }
    };
    tracing::error!(%message, "background task failed");
    if let Err(error) = window.upgrade_in_event_loop(move |window| {
        window.set_error_message(message.into());
    }) {
        tracing::debug!(%error, "window no longer accepts runtime events");
    }
}
