//! Bounded desktop updates and a coherent snapshot for reconnecting views.

use std::collections::VecDeque;
use std::sync::Mutex;

use serde::Serialize;
use specta::Type;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use snenk_bot::app::WorkflowStatus;
use snenk_bot::engine::Event;
use snenk_bot::execution::CompletedRun;
use snenk_bot::integration::{ConnectionStatus, ReconfigurationRequest};

use crate::authentication::TwitchAuthentication;
use crate::input::{InputEvent, InputRequest};

const UPDATE_CAPACITY: usize = 256;
const ERROR_CAPACITY: usize = 32;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Type)]
#[serde(tag = "state", content = "message", rename_all = "snake_case")]
pub enum StartupState {
    Starting,
    Ready,
    Closing,
    Failed(String),
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct ErrorNotice {
    pub id: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct DesktopSnapshot {
    #[specta(type = specta_typescript::Number)]
    pub sequence: u64,
    pub startup: StartupState,
    pub workflows: Vec<WorkflowStatus>,
    pub connections: Vec<ConnectionStatus>,
    pub reconfiguration_requests: Vec<ReconfigurationRequest>,
    pub errors: Vec<ErrorNotice>,
    pub last_completed: Option<CompletedRun>,
    pub pending_input: Option<InputRequest>,
    pub twitch_authentication: Option<TwitchAuthentication>,
}

#[derive(Clone, Debug, Serialize, Type)]
#[serde(tag = "event", content = "payload", rename_all = "snake_case")]
pub enum DesktopEvent {
    Lifecycle(StartupState),
    Inventory(Vec<WorkflowStatus>),
    Connections {
        connections: Vec<ConnectionStatus>,
        requests: Vec<ReconfigurationRequest>,
    },
    Engine(Event),
    RunCompleted(CompletedRun),
    Input(InputEvent),
    TwitchAuthentication(TwitchAuthentication),
    ErrorAdded(ErrorNotice),
    ErrorDismissed {
        id: String,
    },
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct DesktopUpdate {
    #[specta(type = specta_typescript::Number)]
    pub sequence: u64,
    pub event: DesktopEvent,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct DesktopBatch {
    pub updates: Vec<DesktopUpdate>,
    /// A slow consumer must refresh its snapshot before applying further updates.
    pub resync_required: bool,
}

/// Waits while idle and batches a bounded burst, without periodically polling the engine.
pub async fn next_batch(
    receiver: &mut broadcast::Receiver<DesktopUpdate>,
    cancel: &CancellationToken,
) -> Option<DesktopBatch> {
    let first = tokio::select! {
        biased;
        _ = cancel.cancelled() => return None,
        result = receiver.recv() => result,
    };
    let mut batch = DesktopBatch {
        updates: Vec::new(),
        resync_required: false,
    };
    match first {
        Ok(update) => batch.updates.push(update),
        Err(broadcast::error::RecvError::Lagged(_)) => batch.resync_required = true,
        Err(broadcast::error::RecvError::Closed) => return None,
    }
    tokio::select! {
        biased;
        _ = cancel.cancelled() => return None,
        _ = tokio::time::sleep(std::time::Duration::from_millis(16)) => {}
    }
    while batch.updates.len() < 128 {
        match receiver.try_recv() {
            Ok(update) => batch.updates.push(update),
            Err(broadcast::error::TryRecvError::Lagged(_)) => batch.resync_required = true,
            Err(broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed) => {
                break;
            }
        }
    }
    Some(batch)
}

struct HubState {
    snapshot: DesktopSnapshot,
    errors: VecDeque<ErrorNotice>,
}

pub struct DesktopHub {
    state: Mutex<HubState>,
    updates: broadcast::Sender<DesktopUpdate>,
}

impl Default for DesktopHub {
    fn default() -> Self {
        Self::new()
    }
}

impl DesktopHub {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(HubState {
                snapshot: DesktopSnapshot {
                    sequence: 0,
                    startup: StartupState::Starting,
                    workflows: Vec::new(),
                    connections: Vec::new(),
                    reconfiguration_requests: Vec::new(),
                    errors: Vec::new(),
                    last_completed: None,
                    pending_input: None,
                    twitch_authentication: None,
                },
                errors: VecDeque::new(),
            }),
            updates: broadcast::channel(UPDATE_CAPACITY).0,
        }
    }

    /// Subscribe before taking a snapshot; discard updates already represented by its sequence.
    pub fn subscribe(&self) -> broadcast::Receiver<DesktopUpdate> {
        self.updates.subscribe()
    }

    pub fn snapshot(&self) -> Result<DesktopSnapshot, String> {
        self.state
            .lock()
            .map(|state| state.snapshot.clone())
            .map_err(|_| "Desktop state is unavailable.".to_owned())
    }

    pub fn startup_state(&self) -> Result<StartupState, String> {
        self.state
            .lock()
            .map(|state| state.snapshot.startup.clone())
            .map_err(|_| "Desktop state is unavailable.".to_owned())
    }

    pub fn report_error(&self, message: String) -> Result<(), String> {
        self.publish(DesktopEvent::ErrorAdded(ErrorNotice {
            id: Uuid::new_v4().to_string(),
            message,
        }))
    }

    pub fn dismiss_error(&self, id: String) -> Result<(), String> {
        self.publish(DesktopEvent::ErrorDismissed { id })
    }

    /// Snapshot mutation and publication share a lock so observers receive sequence order.
    /// This never calls application code, allowing input callbacks to publish synchronously.
    pub fn publish(&self, event: DesktopEvent) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Desktop state is unavailable.".to_owned())?;
        let next = state
            .snapshot
            .sequence
            .checked_add(1)
            .ok_or_else(|| "Desktop update sequence is exhausted.".to_owned())?;
        match &event {
            DesktopEvent::Lifecycle(startup) => {
                if state.snapshot.startup == *startup {
                    return Ok(());
                }
                state.snapshot.startup = startup.clone();
            }
            DesktopEvent::Inventory(workflows) => state.snapshot.workflows = workflows.clone(),
            DesktopEvent::Connections {
                connections,
                requests,
            } => {
                if state.snapshot.connections == *connections
                    && state.snapshot.reconfiguration_requests == *requests
                {
                    return Ok(());
                }
                state.snapshot.connections = connections.clone();
                state.snapshot.reconfiguration_requests = requests.clone();
            }
            DesktopEvent::TwitchAuthentication(attempt) => {
                state.snapshot.twitch_authentication = Some(attempt.clone());
            }
            DesktopEvent::Engine(_) => {}
            DesktopEvent::RunCompleted(run) => state.snapshot.last_completed = Some(run.clone()),
            DesktopEvent::Input(InputEvent::Requested(request) | InputEvent::Changed(request)) => {
                state.snapshot.pending_input = Some(request.clone());
            }
            DesktopEvent::Input(InputEvent::Closed { request_id }) => {
                if state
                    .snapshot
                    .pending_input
                    .as_ref()
                    .is_some_and(|request| request.request_id == *request_id)
                {
                    state.snapshot.pending_input = None;
                }
            }
            DesktopEvent::ErrorAdded(notice) => {
                if state.errors.len() == ERROR_CAPACITY {
                    state.errors.pop_front();
                }
                state.errors.push_back(notice.clone());
                state.snapshot.errors = state.errors.iter().cloned().collect();
            }
            DesktopEvent::ErrorDismissed { id } => {
                let previous = state.errors.len();
                state.errors.retain(|notice| notice.id != *id);
                if state.errors.len() == previous {
                    return Ok(());
                }
                state.snapshot.errors = state.errors.iter().cloned().collect();
            }
        }
        state.snapshot.sequence = next;
        // No subscribers is normal before the native view has attached.
        let _ = self.updates.send(DesktopUpdate {
            sequence: next,
            event,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test(start_paused = true)]
    async fn update_batches_are_bounded_and_shutdown_interrupts_idle_waits() {
        let hub = DesktopHub::new();
        let mut updates = hub.subscribe();
        let cancel = CancellationToken::new();
        for _ in 0..200 {
            hub.report_error("failure".into()).unwrap();
        }
        let batch = next_batch(&mut updates, &cancel).await.unwrap();
        assert_eq!(batch.updates.len(), 128);
        assert!(!batch.resync_required);
        let batch = next_batch(&mut updates, &cancel).await.unwrap();
        assert_eq!(batch.updates.len(), 72);
        cancel.cancel();
        assert!(next_batch(&mut updates, &cancel).await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn lagged_batches_request_snapshot_recovery() {
        let hub = DesktopHub::new();
        let mut updates = hub.subscribe();
        for _ in 0..UPDATE_CAPACITY + 10 {
            hub.report_error("failure".into()).unwrap();
        }
        let batch = next_batch(&mut updates, &CancellationToken::new())
            .await
            .unwrap();
        assert!(batch.resync_required);
        assert_eq!(batch.updates.len(), 128);
    }

    #[test]
    fn subscribe_then_snapshot_preserves_updates_without_duplicates() {
        let hub = DesktopHub::new();
        let mut updates = hub.subscribe();
        hub.publish(DesktopEvent::Lifecycle(StartupState::Ready))
            .unwrap();
        let initial = hub.snapshot().unwrap();
        assert_eq!(initial.sequence, 1);
        assert_eq!(updates.try_recv().unwrap().sequence, initial.sequence);
        hub.publish(DesktopEvent::Lifecycle(StartupState::Closing))
            .unwrap();
        assert_eq!(updates.try_recv().unwrap().sequence, 2);
    }

    #[test]
    fn errors_are_bounded_and_dismissible_from_any_view() {
        let hub = DesktopHub::new();
        for i in 0..40 {
            hub.report_error(format!("error {i}")).unwrap();
        }
        let snapshot = hub.snapshot().unwrap();
        assert_eq!(snapshot.errors.len(), ERROR_CAPACITY);
        assert_eq!(snapshot.errors[0].message, "error 8");
        hub.dismiss_error(snapshot.errors[0].id.clone()).unwrap();
        let dismissed = hub.snapshot().unwrap();
        assert_eq!(dismissed.errors.len(), ERROR_CAPACITY - 1);
        hub.dismiss_error("missing".into()).unwrap();
        assert_eq!(hub.snapshot().unwrap().sequence, dismissed.sequence);
    }

    #[test]
    fn concurrent_publishers_deliver_monotonic_sequences() {
        let hub = Arc::new(DesktopHub::new());
        let mut updates = hub.subscribe();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let hub = &hub;
                scope.spawn(move || {
                    for _ in 0..20 {
                        hub.report_error("failure".into()).unwrap();
                    }
                });
            }
        });
        for sequence in 1..=80 {
            assert_eq!(updates.try_recv().unwrap().sequence, sequence);
        }
        assert_eq!(hub.snapshot().unwrap().sequence, 80);
    }

    #[test]
    fn lag_is_reported_and_latest_snapshot_is_available() {
        let hub = DesktopHub::new();
        let mut updates = hub.subscribe();
        for _ in 0..UPDATE_CAPACITY + 10 {
            hub.report_error("failure".into()).unwrap();
        }
        assert!(matches!(
            updates.try_recv(),
            Err(broadcast::error::TryRecvError::Lagged(_))
        ));
        assert_eq!(
            hub.snapshot().unwrap().sequence,
            (UPDATE_CAPACITY + 10) as u64
        );
    }

    #[test]
    fn closing_an_old_input_does_not_clear_the_current_request() {
        let hub = DesktopHub::new();
        let request = InputRequest {
            request_id: "current".into(),
            title: "Title".into(),
            fields: Vec::new(),
            defaults: Default::default(),
            values: Default::default(),
            validating: false,
            error: None,
        };
        hub.publish(DesktopEvent::Input(InputEvent::Requested(request)))
            .unwrap();
        hub.publish(DesktopEvent::Input(InputEvent::Closed {
            request_id: "previous".into(),
        }))
        .unwrap();
        assert_eq!(
            hub.snapshot().unwrap().pending_input.unwrap().request_id,
            "current"
        );
        hub.publish(DesktopEvent::Input(InputEvent::Closed {
            request_id: "current".into(),
        }))
        .unwrap();
        assert!(hub.snapshot().unwrap().pending_input.is_none());
    }
}
