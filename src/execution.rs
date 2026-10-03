//! Dispatches admitted workflows on the application-owned runtime.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use thiserror::Error;
use tokio::sync::Mutex;
use tokio::task::{Id, JoinSet};
use tokio_util::sync::CancellationToken;

use crate::engine::{
    ActivationOrigin, ActivationQueue, Engine, EventSink, FailureKind, Outcome, Values,
};
use crate::history::{FailureCategory, RunHistory, RunOutcome, TriggerIdentity, TriggerKind};
use crate::runtime::{AppRuntime, RuntimeError};

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub struct CompletedRun {
    pub run_id: String,
    pub workflow_id: String,
    #[cfg_attr(feature = "desktop-contracts", specta(type = specta_typescript::Number))]
    pub revision: u64,
    pub outcome: Outcome,
}

#[derive(Debug, Error)]
pub enum ExecutionError {
    #[error("workflow `{0}` is unavailable")]
    UnknownWorkflow(String),
    #[error("workflow definition is invalid: {0}")]
    InvalidWorkflow(String),
    #[error("activation rejected: {0}")]
    Rejected(String),
    #[error("run history is unavailable: {0}")]
    History(String),
    #[error("workflow dispatcher is shutting down")]
    Closed,
    #[error("admission task stopped: {0}")]
    AdmissionTask(String),
}

#[derive(Debug, Error)]
pub enum ExecutionStartError {
    #[error("workflow dispatcher has already started")]
    AlreadyStarted,
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
}

#[derive(Clone)]
pub struct WorkflowExecutor {
    engine: Arc<Engine>,
    queue: Arc<ActivationQueue>,
    history: RunHistory,
    gate: Arc<Mutex<()>>,
    closed: Arc<AtomicBool>,
    started: Arc<AtomicBool>,
    on_complete: Arc<dyn Fn(CompletedRun) + Send + Sync>,
    on_error: Arc<dyn Fn(String) + Send + Sync>,
}

struct RunCompletion {
    run_id: String,
    outcome: Outcome,
    history_error: Option<String>,
}

struct ActiveRun {
    run_id: String,
    workflow_id: String,
    revision: u64,
    cancel: CancellationToken,
}

struct CancelIfDropped(CancellationToken, bool);

impl CancelIfDropped {
    fn disarm(mut self) {
        self.1 = false;
    }
}

impl Drop for CancelIfDropped {
    fn drop(&mut self) {
        if self.1 {
            self.0.cancel();
        }
    }
}

impl WorkflowExecutor {
    pub fn new(
        engine: Arc<Engine>,
        events: Arc<dyn EventSink>,
        history: RunHistory,
        on_complete: impl Fn(CompletedRun) + Send + Sync + 'static,
        on_error: impl Fn(String) + Send + Sync + 'static,
    ) -> Self {
        Self {
            engine,
            queue: Arc::new(ActivationQueue::new(events)),
            history,
            gate: Arc::new(Mutex::new(())),
            closed: Arc::new(AtomicBool::new(false)),
            started: Arc::new(AtomicBool::new(false)),
            on_complete: Arc::new(on_complete),
            on_error: Arc::new(on_error),
        }
    }

    /// Admission snapshots the selected workflow and all workflows it may call.
    pub async fn admit(
        &self,
        workflow_id: &str,
        trigger: Values,
        cancel: CancellationToken,
        origin: ActivationOrigin,
    ) -> Result<(), ExecutionError> {
        let guard = CancelIfDropped(cancel.clone(), true);
        let executor = self.clone();
        let workflow_id = workflow_id.to_owned();
        let result = tokio::spawn(async move {
            executor
                .admit_inner(&workflow_id, trigger, cancel, origin)
                .await
        })
        .await
        .map_err(|error| ExecutionError::AdmissionTask(error.to_string()))?;
        guard.disarm();
        result
    }

    async fn admit_inner(
        &self,
        workflow_id: &str,
        trigger: Values,
        cancel: CancellationToken,
        origin: ActivationOrigin,
    ) -> Result<(), ExecutionError> {
        let _gate = self.gate.lock().await;
        if self.closed.load(Ordering::Acquire) {
            return Err(ExecutionError::Closed);
        }
        let workflow = self
            .engine
            .workflow(workflow_id)
            .ok_or_else(|| ExecutionError::UnknownWorkflow(workflow_id.to_owned()))?;
        let plan = self
            .engine
            .snapshot(workflow)
            .map_err(ExecutionError::InvalidWorkflow)?;
        let history = self.history.clone();
        let id = plan.workflow.id.clone();
        let revision = plan.workflow.revision;
        let identity = trigger_identity(origin.clone());
        let started = tokio::task::spawn_blocking(move || history.begin(id, revision, identity))
            .await
            .map_err(|error| ExecutionError::History(error.to_string()))?
            .map_err(|error| ExecutionError::History(error.to_string()))?;
        let run_id = started.run_id;
        if let Err(reason) = self
            .queue
            .admit_recorded(plan, trigger, cancel, origin, Some(run_id.clone()))
            .await
        {
            let history = self.history.clone();
            let finish_id = run_id.clone();
            tokio::task::spawn_blocking(move || history.finish(&finish_id, RunOutcome::Rejected))
                .await
                .map_err(|error| ExecutionError::History(error.to_string()))?
                .map_err(|error| ExecutionError::History(error.to_string()))?;
            return Err(ExecutionError::Rejected(reason));
        }
        Ok(())
    }

    pub fn start(&self, runtime: &AppRuntime) -> Result<(), ExecutionStartError> {
        if self.started.swap(true, Ordering::AcqRel) {
            return Err(ExecutionStartError::AlreadyStarted);
        }
        let worker = self.clone();
        let result = runtime.spawn_task("workflow dispatcher", move |shutdown| async move {
            worker.dispatch(shutdown).await;
            Ok::<(), std::convert::Infallible>(())
        });
        if result.is_err() {
            self.started.store(false, Ordering::Release);
        }
        result.map_err(ExecutionStartError::Runtime)
    }

    async fn dispatch(&self, shutdown: CancellationToken) {
        let mut runs = JoinSet::new();
        let mut active = HashMap::<Id, ActiveRun>::new();

        loop {
            tokio::select! {
                biased;
                result = runs.join_next_with_id(), if !active.is_empty() => {
                    if let Some(result) = result {
                        self.finish_run(result, &mut active).await;
                    }
                }
                _ = shutdown.cancelled() => break,
                activation = self.queue.next_ready(&shutdown) => {
                    let Some(activation) = activation else { break; };
                    let workflow_id = activation.plan.workflow.id.clone();
                    let revision = activation.plan.workflow.revision;
                    let run_id = activation.admission_id.clone().unwrap_or_default();
                    let cancel = activation.cancel.clone();
                    let engine = Arc::clone(&self.engine);
                    let history = self.history.clone();
                    let run_shutdown = shutdown.clone();
                    let handle = runs.spawn(async move {
                        execute_recorded(engine, history, activation, run_shutdown).await
                    });
                    active.insert(handle.id(), ActiveRun { run_id, workflow_id, revision, cancel });
                }
            }
        }

        let _gate = self.gate.lock().await;
        self.closed.store(true, Ordering::Release);
        drop(_gate);
        for run in active.values() {
            run.cancel.cancel();
        }
        while let Some(result) = runs.join_next_with_id().await {
            self.finish_run(result, &mut active).await;
        }
        for pending in self.queue.drain_pending().await {
            self.interrupt_pending(pending).await;
        }
    }

    async fn finish_run(
        &self,
        result: Result<(Id, RunCompletion), tokio::task::JoinError>,
        active: &mut HashMap<Id, ActiveRun>,
    ) {
        let (id, completion) = match result {
            Ok((id, completion)) => (id, completion),
            Err(error) => {
                tracing::error!(%error, "workflow run task stopped unexpectedly");
                (
                    error.id(),
                    RunCompletion {
                        run_id: String::new(),
                        outcome: Outcome::Interrupted,
                        history_error: Some(error.to_string()),
                    },
                )
            }
        };
        if let Some(run) = active.remove(&id) {
            self.queue.finished(&run.workflow_id).await;
            if let Some(message) = completion.history_error {
                (self.on_error)(message);
            }
            (self.on_complete)(CompletedRun {
                run_id: if completion.run_id.is_empty() {
                    run.run_id
                } else {
                    completion.run_id
                },
                workflow_id: run.workflow_id,
                revision: run.revision,
                outcome: completion.outcome,
            });
        }
    }

    async fn interrupt_pending(&self, pending: crate::engine::Activation) {
        let Some(run_id) = pending.admission_id else {
            return;
        };
        let workflow_id = pending.plan.workflow.id.clone();
        let revision = pending.plan.workflow.revision;
        let history = self.history.clone();
        let finish_id = run_id.clone();
        let finished = tokio::task::spawn_blocking(move || {
            history.finish(&finish_id, RunOutcome::Interrupted)
        })
        .await;
        if let Err(message) = finished
            .map_err(|error| error.to_string())
            .and_then(|result| result.map_err(|error| error.to_string()))
        {
            (self.on_error)(message);
        }
        (self.on_complete)(CompletedRun {
            run_id,
            workflow_id,
            revision,
            outcome: Outcome::Interrupted,
        });
    }
}

async fn execute_recorded(
    engine: Arc<Engine>,
    history: RunHistory,
    activation: crate::engine::Activation,
    shutdown: CancellationToken,
) -> RunCompletion {
    let Some(run_id) = activation.admission_id else {
        return RunCompletion {
            run_id: String::new(),
            outcome: Outcome::Rejected("activation has no history record".to_owned()),
            history_error: Some("activation has no history record".to_owned()),
        };
    };

    let result = engine
        .run_plan(activation.plan, activation.trigger, activation.cancel)
        .await;
    let script_activity = result.script_activity;
    let step_trace = result.step_trace;
    let outcome = if shutdown.is_cancelled() && result.outcome == Outcome::Cancelled {
        Outcome::Interrupted
    } else {
        result.outcome
    };
    let stored_outcome = history_outcome(&outcome);
    let finish_id = run_id.clone();
    let finished = tokio::task::spawn_blocking(move || {
        history.finish_with_trace(&finish_id, stored_outcome, script_activity, step_trace)
    })
    .await;
    let history_error = match finished {
        Ok(Ok(_)) => None,
        Ok(Err(error)) => Some(error.to_string()),
        Err(error) => Some(error.to_string()),
    };
    RunCompletion {
        run_id,
        outcome,
        history_error,
    }
}

fn trigger_identity(origin: ActivationOrigin) -> TriggerIdentity {
    match origin {
        ActivationOrigin::Manual => TriggerIdentity {
            kind: TriggerKind::Manual,
            id: None,
        },
        ActivationOrigin::Schedule(id) => TriggerIdentity {
            kind: TriggerKind::Schedule,
            id: Some(id),
        },
        ActivationOrigin::Event(id) => TriggerIdentity {
            kind: TriggerKind::Event,
            id: Some(id),
        },
        ActivationOrigin::Webhook(id) => TriggerIdentity {
            kind: TriggerKind::Webhook,
            id: Some(id),
        },
    }
}

fn history_outcome(outcome: &Outcome) -> RunOutcome {
    match outcome {
        Outcome::Success => RunOutcome::Succeeded,
        Outcome::Stopped => RunOutcome::Stopped,
        Outcome::Cancelled => RunOutcome::Cancelled,
        Outcome::Failed(failure) => RunOutcome::Failed {
            category: match failure.kind {
                FailureKind::InvalidDefinition => FailureCategory::Definition,
                FailureKind::MissingValue => FailureCategory::MissingValue,
                FailureKind::InvalidType => FailureCategory::InvalidType,
                FailureKind::CapabilityUnavailable => FailureCategory::Capability,
                FailureKind::ConnectorUnavailable => FailureCategory::Connector,
                FailureKind::Action
                | FailureKind::CalledWorkflow
                | FailureKind::NoChildSucceeded => FailureCategory::Action,
                FailureKind::Timeout => FailureCategory::Timeout,
                FailureKind::Input => FailureCategory::Input,
                FailureKind::LoopLimit | FailureKind::StepLimit | FailureKind::CallDepth => {
                    FailureCategory::Limit
                }
            },
        },
        Outcome::Rejected(_) => RunOutcome::Rejected,
        Outcome::Interrupted => RunOutcome::Interrupted,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::future::Future;
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    use tempfile::tempdir;
    use tokio_util::sync::CancellationToken;

    use super::{CompletedRun, WorkflowExecutor};
    use crate::engine::{
        ActivationOrigin, Engine, Event, FailurePolicy, Input, InputField, InputProvider,
        InputResponse, Outcome, ScriptActivity, ScriptOutcome, Step, StepKind, Values, Workflow,
    };
    use crate::history::{RunHistory, RunOutcome};
    use crate::lua::LuaAction;
    use crate::runtime::AppRuntime;
    use crate::workflows::WorkflowRepository;

    struct NoInput;

    impl InputProvider for NoInput {
        fn request<'a>(
            &'a self,
            _: String,
            _: Vec<InputField>,
            _: Values,
            _: &'a crate::engine::FormValidators,
            _: CancellationToken,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<InputResponse, String>> + Send + 'a>,
        > {
            Box::pin(async { Err("no input provider".to_owned()) })
        }
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

    #[test]
    fn saved_workflow_runs_on_app_runtime_and_records_history() {
        let directory = tempdir().unwrap();
        let repository = WorkflowRepository::at(directory.path().join("workflows"));
        repository.create(&workflow("example", vec![])).unwrap();

        let events: Arc<dyn crate::engine::EventSink> = Arc::new(|_: Event| {});
        let engine = Arc::new(Engine::new(Arc::new(NoInput), events.clone()));
        for entry in repository.list().unwrap() {
            if let crate::workflows::WorkflowEntry::Available { workflow, .. } = entry {
                engine.register_workflow(workflow).unwrap();
            }
        }
        let history = RunHistory::new(directory.path().join("history"));
        let (sender, receiver) = mpsc::channel::<CompletedRun>();
        let executor = WorkflowExecutor::new(
            engine,
            events,
            history.clone(),
            move |run| sender.send(run).unwrap(),
            |error| panic!("history error: {error}"),
        );
        let mut app_runtime = AppRuntime::new(|event| panic!("runtime error: {event:?}")).unwrap();
        app_runtime
            .spawn_task("admit", {
                let executor = executor.clone();
                move |_| async move {
                    executor
                        .admit(
                            "example",
                            Values::new(),
                            CancellationToken::new(),
                            ActivationOrigin::Manual,
                        )
                        .await
                }
            })
            .unwrap();
        executor.start(&app_runtime).unwrap();

        let completed = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(completed.outcome, Outcome::Success);
        assert_eq!(completed.workflow_id, "example");
        let saved = history.load(&completed.run_id).unwrap();
        assert_eq!(saved.outcome, RunOutcome::Succeeded);
        app_runtime.shutdown(Duration::from_secs(2)).unwrap();
    }

    #[test]
    fn saved_lua_workflow_consumes_values_and_records_script_activity() {
        let directory = tempdir().unwrap();
        let repository = WorkflowRepository::at(directory.path().join("workflows"));
        let source =
            "assert(values.title == 'Hello'); return { result = values.title .. ' world' }";
        let mut definition = workflow(
            "lua-flow",
            vec![
                Step {
                    id: "initial".into(),
                    on_failure: FailurePolicy::Stop,
                    kind: StepKind::SetVariable {
                        name: "title".into(),
                        value: Input::Trigger {
                            name: "title".into(),
                            fallback: None,
                        },
                    },
                },
                Step {
                    id: "script".into(),
                    on_failure: FailurePolicy::Stop,
                    kind: StepKind::Action {
                        capability: "lua.run".into(),
                        version: 1,
                        inputs: BTreeMap::from([
                            ("source".into(), Input::Literal(source.into())),
                            (
                                "values".into(),
                                Input::Object(BTreeMap::from([(
                                    "title".into(),
                                    Input::Variable {
                                        name: "title".into(),
                                        fallback: None,
                                    },
                                )])),
                            ),
                        ]),
                        deadline_ms: None,
                    },
                },
                Step {
                    id: "save".into(),
                    on_failure: FailurePolicy::Stop,
                    kind: StepKind::SetVariable {
                        name: "result".into(),
                        value: Input::Reference {
                            step_id: "script".into(),
                            output_id: "result".into(),
                            fallback: None,
                        },
                    },
                },
            ],
        );
        definition.outputs.insert(
            "result".into(),
            Input::Variable {
                name: "result".into(),
                fallback: None,
            },
        );
        repository.create(&definition).unwrap();

        let events: Arc<dyn crate::engine::EventSink> = Arc::new(|_: Event| {});
        let mut engine = Engine::new(Arc::new(NoInput), events.clone());
        engine.register_capability("lua.run", 1, Arc::new(LuaAction::new()));
        for entry in repository.list().unwrap() {
            if let crate::workflows::WorkflowEntry::Available { workflow, .. } = entry {
                engine.register_workflow(workflow).unwrap();
            }
        }
        let history = RunHistory::new(directory.path().join("history"));
        let (sender, receiver) = mpsc::channel::<CompletedRun>();
        let executor = WorkflowExecutor::new(
            Arc::new(engine),
            events,
            history.clone(),
            move |run| sender.send(run).unwrap(),
            |error| panic!("history error: {error}"),
        );
        let mut app_runtime = AppRuntime::new(|event| panic!("runtime error: {event:?}")).unwrap();
        app_runtime
            .spawn_task("admit", {
                let executor = executor.clone();
                move |_| async move {
                    executor
                        .admit(
                            "lua-flow",
                            Values::from([("title".into(), "Hello".into())]),
                            CancellationToken::new(),
                            ActivationOrigin::Manual,
                        )
                        .await
                }
            })
            .unwrap();
        executor.start(&app_runtime).unwrap();

        let completed = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(completed.outcome, Outcome::Success);
        let record = history.load(&completed.run_id).unwrap();
        assert_eq!(record.outcome, RunOutcome::Succeeded);
        assert_eq!(
            record.script_activity,
            vec![ScriptActivity {
                workflow_id: "lua-flow".into(),
                step_id: "script".into(),
                outcome: ScriptOutcome::Succeeded,
            }]
        );
        assert_eq!(record.step_trace.len(), 3);
        assert_eq!(record.step_trace[1].step_id, "script");
        assert_eq!(record.step_trace[1].workflow_revision, 1);
        let file = std::fs::read_to_string(
            directory
                .path()
                .join("history")
                .join(format!("{}.json", completed.run_id)),
        )
        .unwrap();
        assert!(!file.contains("Hello"));
        assert!(!file.contains(source));
        assert!(!file.contains("values.title"));
        app_runtime.shutdown(Duration::from_secs(2)).unwrap();
    }

    #[test]
    fn shutdown_interrupts_active_work_and_persists_that_outcome() {
        let directory = tempdir().unwrap();
        let (step_sender, step_receiver) = mpsc::channel();
        let events: Arc<dyn crate::engine::EventSink> = Arc::new(move |event: Event| {
            if matches!(event, Event::StepStarted { .. }) {
                step_sender.send(()).unwrap();
            }
        });
        let engine = Arc::new(Engine::new(Arc::new(NoInput), events.clone()));
        engine
            .register_workflow(workflow(
                "wait",
                vec![Step {
                    id: "delay".to_owned(),
                    on_failure: crate::engine::FailurePolicy::Stop,
                    kind: StepKind::Delay { millis: 10_000 },
                }],
            ))
            .unwrap();
        let history = RunHistory::new(directory.path().join("history"));
        let (sender, receiver) = mpsc::channel::<CompletedRun>();
        let executor = WorkflowExecutor::new(
            engine,
            events,
            history.clone(),
            move |run| sender.send(run).unwrap(),
            |error| panic!("history error: {error}"),
        );
        let mut app_runtime = AppRuntime::new(|event| panic!("runtime error: {event:?}")).unwrap();
        app_runtime
            .spawn_task("admit", {
                let executor = executor.clone();
                move |_| async move {
                    executor
                        .admit(
                            "wait",
                            Values::new(),
                            CancellationToken::new(),
                            ActivationOrigin::Manual,
                        )
                        .await
                }
            })
            .unwrap();
        executor.start(&app_runtime).unwrap();
        step_receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        app_runtime.shutdown(Duration::from_secs(2)).unwrap();

        let completed = receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(completed.outcome, Outcome::Interrupted);
        assert_eq!(
            history.load(&completed.run_id).unwrap().outcome,
            RunOutcome::Interrupted
        );
        let trace = history.load(&completed.run_id).unwrap().step_trace;
        assert_eq!(trace.len(), 1);
        assert_eq!(trace[0].outcome, crate::engine::StepTraceOutcome::Cancelled);
    }

    #[test]
    fn shutdown_interrupts_queued_work_and_closes_admission() {
        let directory = tempdir().unwrap();
        let (step_sender, step_receiver) = mpsc::channel();
        let events: Arc<dyn crate::engine::EventSink> = Arc::new(move |event: Event| {
            if matches!(event, Event::StepStarted { .. }) {
                step_sender.send(()).unwrap();
            }
        });
        let engine = Arc::new(Engine::new(Arc::new(NoInput), events.clone()));
        engine
            .register_workflow(workflow(
                "wait",
                vec![Step {
                    id: "delay".to_owned(),
                    on_failure: crate::engine::FailurePolicy::Stop,
                    kind: StepKind::Delay { millis: 10_000 },
                }],
            ))
            .unwrap();
        let history = RunHistory::new(directory.path().join("history"));
        let (sender, receiver) = mpsc::channel::<CompletedRun>();
        let executor = WorkflowExecutor::new(
            engine,
            events,
            history.clone(),
            move |run| sender.send(run).unwrap(),
            |error| panic!("history error: {error}"),
        );
        let mut app_runtime = AppRuntime::new(|event| panic!("runtime error: {event:?}")).unwrap();
        executor.start(&app_runtime).unwrap();
        assert!(matches!(
            executor.start(&app_runtime),
            Err(super::ExecutionStartError::AlreadyStarted)
        ));
        let admitted = executor.clone();
        let (admitted_sender, admitted_receiver) = mpsc::channel();
        app_runtime
            .spawn_task("admit both", move |_| async move {
                for _ in 0..2 {
                    admitted
                        .admit(
                            "wait",
                            Values::new(),
                            CancellationToken::new(),
                            ActivationOrigin::Manual,
                        )
                        .await?;
                }
                admitted_sender.send(()).unwrap();
                Ok::<(), super::ExecutionError>(())
            })
            .unwrap();
        admitted_receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        step_receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        app_runtime.shutdown(Duration::from_secs(2)).unwrap();

        let completed: Vec<_> = receiver.try_iter().collect();
        assert_eq!(completed.len(), 2);
        for run in completed {
            assert_eq!(run.outcome, Outcome::Interrupted);
            assert_eq!(
                history.load(&run.run_id).unwrap().outcome,
                RunOutcome::Interrupted
            );
        }
        let checker = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert!(matches!(
            checker.block_on(executor.admit(
                "wait",
                Values::new(),
                CancellationToken::new(),
                ActivationOrigin::Manual,
            )),
            Err(super::ExecutionError::Closed)
        ));
    }

    #[tokio::test]
    async fn abandoning_admission_cancels_its_eventual_recorded_run() {
        let directory = tempdir().unwrap();
        let events: Arc<dyn crate::engine::EventSink> = Arc::new(|_: Event| {});
        let engine = Arc::new(Engine::new(Arc::new(NoInput), events.clone()));
        engine.register_workflow(workflow("flow", vec![])).unwrap();
        let history = RunHistory::new(directory.path().join("history"));
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let executor = WorkflowExecutor::new(
            engine,
            events,
            history.clone(),
            move |run| {
                sender.send(run).unwrap();
            },
            |error| panic!("history error: {error}"),
        );
        let gate = executor.gate.lock().await;
        let cancellation = CancellationToken::new();
        let mut admission = Box::pin(executor.admit(
            "flow",
            Values::new(),
            cancellation.clone(),
            ActivationOrigin::Manual,
        ));
        std::future::poll_fn(|context| {
            assert!(admission.as_mut().poll(context).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        drop(admission);
        assert!(cancellation.is_cancelled());
        drop(gate);

        let shutdown = CancellationToken::new();
        let worker = tokio::spawn({
            let executor = executor.clone();
            let shutdown = shutdown.clone();
            async move { executor.dispatch(shutdown).await }
        });
        let completed = tokio::time::timeout(Duration::from_secs(2), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(completed.outcome, Outcome::Cancelled);
        assert_eq!(
            history.load(&completed.run_id).unwrap().outcome,
            RunOutcome::Cancelled
        );
        shutdown.cancel();
        worker.await.unwrap();
    }
}
