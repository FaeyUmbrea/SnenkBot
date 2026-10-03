use std::fmt::Display;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio::runtime::{Builder, Handle, Runtime};
use tokio::task::{JoinError, JoinHandle};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// Events reported by application-owned background tasks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeEvent {
    TaskFailed { name: String, message: String },
    TaskPanicked { name: String, message: String },
}

/// A synchronous owner for the application's Tokio runtime and background tasks.
pub struct AppRuntime {
    runtime: Option<Runtime>,
    cancellation: CancellationToken,
    tasks: TaskTracker,
    event_sink: Arc<dyn Fn(RuntimeEvent) + Send + Sync>,
}

#[derive(Clone)]
pub struct RuntimeSpawner {
    handle: Handle,
    cancellation: CancellationToken,
    tasks: TaskTracker,
    event_sink: Arc<dyn Fn(RuntimeEvent) + Send + Sync>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RuntimeError {
    #[error("runtime shutdown cannot run inside an asynchronous runtime")]
    AsyncShutdown,
    #[error("runtime has already been shut down")]
    AlreadyShutDown,
    #[error("background tasks did not stop before the shutdown deadline")]
    ShutdownTimedOut,
}

impl AppRuntime {
    pub fn new(event_sink: impl Fn(RuntimeEvent) + Send + Sync + 'static) -> std::io::Result<Self> {
        let runtime = Builder::new_multi_thread()
            .enable_all()
            .thread_name("snenkbot-runtime")
            .build()?;

        Ok(Self {
            runtime: Some(runtime),
            cancellation: CancellationToken::new(),
            tasks: TaskTracker::new(),
            event_sink: Arc::new(event_sink),
        })
    }

    /// Returns a child token that tasks can select against for cooperative shutdown.
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.child_token()
    }

    /// Stops owned application work while keeping the runtime available to report failures.
    pub fn request_shutdown(&self) {
        self.cancellation.cancel();
    }

    pub fn spawner(&self) -> Result<RuntimeSpawner, RuntimeError> {
        let runtime = self.runtime.as_ref().ok_or(RuntimeError::AlreadyShutDown)?;
        Ok(RuntimeSpawner {
            handle: runtime.handle().clone(),
            cancellation: self.cancellation.clone(),
            tasks: self.tasks.clone(),
            event_sink: Arc::clone(&self.event_sink),
        })
    }

    /// Shares the owned runtime with the desktop framework; the application still owns shutdown.
    pub fn handle(&self) -> Result<Handle, RuntimeError> {
        self.runtime
            .as_ref()
            .map(|runtime| runtime.handle().clone())
            .ok_or(RuntimeError::AlreadyShutDown)
    }

    /// Starts a fallible task and reports its error or panic through the event sink.
    pub fn spawn_task<F, Fut, E>(
        &self,
        name: impl Into<String>,
        task: F,
    ) -> Result<(), RuntimeError>
    where
        F: FnOnce(CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), E>> + Send + 'static,
        E: Display + Send + 'static,
    {
        self.spawner()?.spawn_task(name, task)
    }
}

impl RuntimeSpawner {
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.child_token()
    }

    pub fn spawn_task<F, Fut, E>(
        &self,
        name: impl Into<String>,
        task: F,
    ) -> Result<(), RuntimeError>
    where
        F: FnOnce(CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), E>> + Send + 'static,
        E: Display + Send + 'static,
    {
        if self.cancellation.is_cancelled() || self.tasks.is_closed() {
            return Err(RuntimeError::AlreadyShutDown);
        }
        let name = name.into();
        let cancellation = self.cancellation.child_token();
        let worker: JoinHandle<Result<(), String>> = self
            .handle
            .spawn(async move { task(cancellation).await.map_err(|error| error.to_string()) });

        let event_sink = Arc::clone(&self.event_sink);
        self.tasks.spawn_on(
            async move {
                match worker.await {
                    Ok(Ok(())) => {}
                    Ok(Err(message)) => event_sink(RuntimeEvent::TaskFailed { name, message }),
                    Err(error) if error.is_panic() => {
                        event_sink(RuntimeEvent::TaskPanicked {
                            name,
                            message: panic_message(error),
                        });
                    }
                    Err(_) => {}
                }
            },
            &self.handle,
        );
        Ok(())
    }
}

impl AppRuntime {
    /// Cancels tasks and waits up to `grace_period` for tracked supervisors to finish.
    /// Call this from the synchronous UI shutdown path, outside any Tokio runtime.
    pub fn shutdown(&mut self, grace_period: Duration) -> Result<(), RuntimeError> {
        if tokio::runtime::Handle::try_current().is_ok() {
            return Err(RuntimeError::AsyncShutdown);
        }

        let Some(runtime) = self.runtime.take() else {
            return Err(RuntimeError::AlreadyShutDown);
        };

        self.cancellation.cancel();
        self.tasks.close();

        let drained =
            runtime.block_on(async { timeout(grace_period, self.tasks.wait()).await.is_ok() });

        if !drained {
            // Runtime shutdown cancels the tracked supervisors and their worker tasks.
            runtime.shutdown_background();
            return Err(RuntimeError::ShutdownTimedOut);
        }

        runtime.shutdown_timeout(grace_period);
        Ok(())
    }
}

impl Drop for AppRuntime {
    fn drop(&mut self) {
        self.cancellation.cancel();
        self.tasks.close();

        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

fn panic_message(error: JoinError) -> String {
    let payload = error.into_panic();
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    if let Some(message) = payload.downcast_ref::<&'static str>() {
        return (*message).to_owned();
    }
    "task panicked with a non-string payload".to_owned()
}

#[cfg(test)]
mod tests {
    use super::{AppRuntime, RuntimeError, RuntimeEvent};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    use tokio_util::sync::CancellationToken;

    #[test]
    fn cancellation_keeps_the_shared_runtime_available_until_final_shutdown() {
        let mut runtime = AppRuntime::new(|_| {}).unwrap();
        let handle = runtime.handle().unwrap();
        let cancel = runtime.cancellation_token();
        runtime.request_shutdown();
        assert!(cancel.is_cancelled());
        assert_eq!(handle.block_on(async { 42 }), 42);
        runtime.shutdown(Duration::from_secs(1)).unwrap();
        assert!(matches!(
            runtime.handle(),
            Err(RuntimeError::AlreadyShutDown)
        ));
    }

    #[test]
    fn associates_each_failure_with_its_task_name() {
        let (sender, receiver) = mpsc::channel();
        let mut runtime = AppRuntime::new(move |event| sender.send(event).unwrap()).unwrap();

        runtime
            .spawn_task("database", |_| async { Err::<(), _>("connection lost") })
            .unwrap();
        runtime
            .spawn_task("settings", |_| async { Err::<(), _>("invalid config") })
            .unwrap();
        runtime.shutdown(Duration::from_secs(1)).unwrap();

        let events: Vec<_> = receiver.try_iter().collect();
        assert!(events.contains(&RuntimeEvent::TaskFailed {
            name: "database".to_owned(),
            message: "connection lost".to_owned(),
        }));
        assert!(events.contains(&RuntimeEvent::TaskFailed {
            name: "settings".to_owned(),
            message: "invalid config".to_owned(),
        }));
    }

    #[test]
    fn reports_failure_and_panic() {
        let (sender, receiver) = mpsc::channel();
        let mut runtime = AppRuntime::new(move |event| sender.send(event).unwrap()).unwrap();

        runtime
            .spawn_task("fails", |_| async { Err::<(), _>("database unavailable") })
            .unwrap();
        let should_panic = true;
        runtime
            .spawn_task("panics", move |_| async move {
                if should_panic {
                    panic!("unexpected state")
                } else {
                    Ok::<(), String>(())
                }
            })
            .unwrap();
        runtime.shutdown(Duration::from_secs(1)).unwrap();

        let events: Vec<_> = receiver.try_iter().collect();
        assert!(events.contains(&RuntimeEvent::TaskFailed {
            name: "fails".to_owned(),
            message: "database unavailable".to_owned(),
        }));
        assert!(events.contains(&RuntimeEvent::TaskPanicked {
            name: "panics".to_owned(),
            message: "unexpected state".to_owned(),
        }));
    }

    #[test]
    fn shutdown_cancels_cooperative_task() {
        let (sender, receiver) = mpsc::channel();
        let mut runtime = AppRuntime::new(move |event| sender.send(event).unwrap()).unwrap();
        let (started_sender, started_receiver) = mpsc::channel();

        runtime
            .spawn_task("cooperative", move |token: CancellationToken| async move {
                started_sender.send(()).unwrap();
                token.cancelled().await;
                Ok::<(), String>(())
            })
            .unwrap();
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        runtime.shutdown(Duration::from_secs(1)).unwrap();

        assert!(receiver.try_iter().next().is_none());
    }

    #[test]
    fn cloned_spawner_tracks_tasks_started_after_setup() {
        let mut runtime = AppRuntime::new(|_| {}).unwrap();
        let spawner = runtime.spawner().unwrap();
        let (started_sender, started_receiver) = mpsc::channel();
        spawner
            .spawn_task("sign-in", move |cancel| async move {
                started_sender.send(()).unwrap();
                cancel.cancelled().await;
                Ok::<(), String>(())
            })
            .unwrap();
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        runtime.shutdown(Duration::from_secs(1)).unwrap();
        assert_eq!(
            spawner.spawn_task("late", |_| async { Ok::<(), String>(()) }),
            Err(RuntimeError::AlreadyShutDown)
        );
    }

    #[test]
    fn shutdown_reports_timeout_and_aborts_unresponsive_task() {
        let mut runtime = AppRuntime::new(|_| {}).unwrap();
        let (started_sender, started_receiver) = mpsc::channel();
        runtime
            .spawn_task("unresponsive", move |_| async move {
                started_sender.send(()).unwrap();
                std::future::pending::<()>().await;
                Ok::<(), String>(())
            })
            .unwrap();
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();

        let started = Instant::now();
        assert_eq!(
            runtime.shutdown(Duration::from_millis(30)),
            Err(RuntimeError::ShutdownTimedOut)
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn shutdown_timeout_drops_unresponsive_task_future() {
        struct NotifyOnDrop(mpsc::Sender<()>);

        impl Drop for NotifyOnDrop {
            fn drop(&mut self) {
                let _ = self.0.send(());
            }
        }

        let mut runtime = AppRuntime::new(|_| {}).unwrap();
        let (started_sender, started_receiver) = mpsc::channel();
        let (dropped_sender, dropped_receiver) = mpsc::channel();
        runtime
            .spawn_task("unresponsive", move |_| async move {
                let _guard = NotifyOnDrop(dropped_sender);
                started_sender.send(()).unwrap();
                std::future::pending::<()>().await;
                Ok::<(), String>(())
            })
            .unwrap();
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();

        assert_eq!(
            runtime.shutdown(Duration::from_millis(30)),
            Err(RuntimeError::ShutdownTimedOut)
        );
        dropped_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
    }

    #[test]
    fn spawn_after_shutdown_returns_error() {
        let mut runtime = AppRuntime::new(|_| {}).unwrap();
        runtime.shutdown(Duration::from_secs(1)).unwrap();

        assert_eq!(
            runtime.spawn_task("late", |_| async { Ok::<(), String>(()) }),
            Err(RuntimeError::AlreadyShutDown)
        );
    }

    #[tokio::test]
    async fn shutdown_rejects_async_context_without_consuming_runtime() {
        let mut runtime = AppRuntime::new(|_| {}).unwrap();

        assert_eq!(
            runtime.shutdown(Duration::from_millis(10)),
            Err(RuntimeError::AsyncShutdown)
        );

        std::thread::spawn(move || {
            let mut runtime = runtime;
            runtime.shutdown(Duration::from_secs(1))
        })
        .join()
        .unwrap()
        .unwrap();
    }
}
