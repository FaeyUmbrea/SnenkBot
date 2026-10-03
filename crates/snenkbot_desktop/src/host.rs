//! Native lifecycle ownership; integration behavior remains in application services.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::{Emitter, EventTarget, Manager, RunEvent, Runtime};

use snenk_bot::app::AppServices;
use snenk_bot::paths::AppPaths;
use snenk_bot::runtime::{AppRuntime, RuntimeEvent};
use snenk_bot::workflows::WorkflowRepository;

use crate::commands::{DesktopBackend, DesktopServices, register_commands};
use crate::hub::{DesktopEvent, DesktopHub, StartupState, next_batch};
use crate::input::DesktopInputProvider;

const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

struct ShutdownOwner {
    runtime: Mutex<Option<AppRuntime>>,
    closing: AtomicBool,
    done: AtomicBool,
    input: Arc<DesktopInputProvider>,
    hub: Arc<DesktopHub>,
}

impl ShutdownOwner {
    fn begin(self: &Arc<Self>, completed: impl FnOnce(bool) + Send + 'static) {
        if self.closing.swap(true, Ordering::AcqRel) {
            return;
        }
        publish(&self.hub, DesktopEvent::Lifecycle(StartupState::Closing));
        if let Err(error) = self.input.shutdown() {
            tracing::error!(%error, "desktop input shutdown failed");
        }
        let owner = Arc::clone(self);
        // Runtime shutdown must execute outside Tokio and must not block the UI thread.
        let completion = Arc::new(Mutex::new(Some(completed)));
        let worker_completion = Arc::clone(&completion);
        let spawned = std::thread::Builder::new()
            .name("snenkbot-shutdown".into())
            .spawn(move || {
                let success = owner.finish();
                owner.done.store(true, Ordering::Release);
                if let Some(completed) = worker_completion
                    .lock()
                    .expect("shutdown callback lock poisoned")
                    .take()
                {
                    completed(success);
                }
            });
        if let Err(error) = spawned {
            tracing::error!(%error, "could not start desktop shutdown worker");
            // Dropping the owner cancels the runtime in the background if no worker can start.
            self.runtime
                .lock()
                .expect("runtime ownership lock poisoned")
                .take();
            self.done.store(true, Ordering::Release);
            if let Some(completed) = completion
                .lock()
                .expect("shutdown callback lock poisoned")
                .take()
            {
                completed(false);
            }
        }
    }

    fn finish(&self) -> bool {
        let Some(mut runtime) = self
            .runtime
            .lock()
            .expect("runtime ownership lock poisoned")
            .take()
        else {
            return true;
        };
        match runtime.shutdown(SHUTDOWN_GRACE) {
            Ok(()) => true,
            Err(error) => {
                tracing::error!(%error, "desktop runtime shutdown failed");
                false
            }
        }
    }
}

fn publish(hub: &DesktopHub, event: DesktopEvent) {
    if let Err(error) = hub.publish(event) {
        tracing::error!(%error, "desktop state publication failed");
    }
}

fn startup_failed(hub: &DesktopHub, error: impl std::fmt::Display) {
    tracing::error!(%error, "desktop startup failed");
    publish(
        hub,
        DesktopEvent::Lifecycle(StartupState::Failed(
            "SnenkBot could not start. Check the application log for details.".into(),
        )),
    );
}

fn observe_connections(
    runtime: &AppRuntime,
    hub: Arc<DesktopHub>,
    services: Arc<AppServices>,
) -> Result<(), snenk_bot::runtime::RuntimeError> {
    // Subscribe before reading state. One coordinator recomputes snapshots serially,
    // so parallel module notifications cannot publish an older snapshot after a newer one.
    let subscriptions = services.subscribe_configuration_changes();
    runtime.spawn_task("desktop connection status", move |cancel| async move {
        let mut waiting = tokio::task::JoinSet::new();
        for receiver in subscriptions {
            wait_for_change(&mut waiting, receiver);
        }
        publish_connections(&hub, &services);
        while !waiting.is_empty() {
            let changed = tokio::select! {
                biased;
                _ = cancel.cancelled() => break,
                changed = waiting.join_next() => changed,
            };
            match changed {
                Some(Ok((receiver, true))) => {
                    publish_connections(&hub, &services);
                    wait_for_change(&mut waiting, receiver);
                }
                Some(Ok((_, false))) | None => {}
                Some(Err(error)) => return Err(error.to_string()),
            }
        }
        Ok::<(), String>(())
    })
}

fn wait_for_change(
    waiting: &mut tokio::task::JoinSet<(tokio::sync::watch::Receiver<u64>, bool)>,
    mut receiver: tokio::sync::watch::Receiver<u64>,
) {
    waiting.spawn(async move {
        let changed = receiver.changed().await.is_ok();
        (receiver, changed)
    });
}

fn publish_connections(hub: &DesktopHub, services: &AppServices) {
    publish(
        hub,
        DesktopEvent::Connections {
            connections: services.connection_statuses(),
            requests: services.reconfiguration_requests(),
        },
    );
}

/// Runs the native event loop with the existing application's runtime and data locations.
/// Calling this starts real integrations; fixture tests exercise the bridges independently.
pub fn run<R: Runtime>(
    builder: tauri::Builder<R>,
    context: tauri::Context<R>,
) -> Result<i32, tauri::Error> {
    run_with_legacy_paths(builder, context, AppPaths::resolve())
}

/// Resolves Tauri storage and migrates legacy data before integrations or windows start.
pub fn run_with_legacy_paths<R: Runtime>(
    builder: tauri::Builder<R>,
    mut context: tauri::Context<R>,
    legacy: Result<AppPaths, snenk_bot::paths::PathError>,
) -> Result<i32, tauri::Error> {
    let hub = Arc::new(DesktopHub::new());
    let input_hub = Arc::clone(&hub);
    let input = Arc::new(DesktopInputProvider::new(move |event| {
        input_hub.publish(DesktopEvent::Input(event))
    }));
    let runtime_hub = Arc::clone(&hub);
    let runtime = match AppRuntime::new(move |event| {
        let (name, message) = match event {
            RuntimeEvent::TaskFailed { name, message }
            | RuntimeEvent::TaskPanicked { name, message } => (name, message),
        };
        tracing::error!(%name, %message, "application background task failed");
        if let Err(error) = runtime_hub.report_error(format!("Background task failed: {name}")) {
            tracing::error!(%error, "background failure publication failed");
        }
    }) {
        Ok(runtime) => Some(runtime),
        Err(error) => {
            startup_failed(&hub, error);
            None
        }
    };
    if let Some(runtime) = &runtime {
        tauri::async_runtime::set(
            runtime
                .handle()
                .expect("new application runtime has a handle"),
        );
    }
    // PathResolver needs the native app handle. Defer all windows until services and
    // managed command state exist, so the frontend cannot race backend initialization.
    let windows = context.config().app.windows.clone();
    for window in &mut context.config_mut().app.windows {
        window.create = false;
    }
    let app = match register_commands(builder).build(context) {
        Ok(app) => app,
        Err(error) => {
            input.shutdown().ok();
            if let Some(mut runtime) = runtime {
                runtime.shutdown(SHUTDOWN_GRACE).ok();
            }
            return Err(error);
        }
    };
    let paths = crate::paths::resolve(app.handle()).and_then(|paths| {
        let legacy =
            legacy.map_err(|error| crate::paths::DirectoryError::Legacy(error.to_string()))?;
        crate::paths::migrate_legacy(&legacy, &paths)?;
        Ok(paths)
    });
    let ready = runtime.as_ref().and_then(|runtime| {
        let paths = match paths {
            Ok(paths) => paths,
            Err(error) => {
                startup_failed(&hub, error);
                return None;
            }
        };
        let events_hub = Arc::clone(&hub);
        let completed_hub = Arc::clone(&hub);
        let error_hub = Arc::clone(&hub);
        let inventory_hub = Arc::clone(&hub);
        match AppServices::start(
            &paths,
            runtime,
            input.clone(),
            Arc::new(move |event| publish(&events_hub, DesktopEvent::Engine(event))),
            move |run| publish(&completed_hub, DesktopEvent::RunCompleted(run)),
            move |message| {
                if let Err(error) = error_hub.report_error(message) {
                    tracing::error!(%error, "application failure publication failed");
                }
            },
            move |workflows| publish(&inventory_hub, DesktopEvent::Inventory(workflows)),
        ) {
            Ok(services) => {
                let services = Arc::new(services);
                if let Err(error) =
                    observe_connections(runtime, Arc::clone(&hub), Arc::clone(&services))
                {
                    startup_failed(&hub, error);
                    return None;
                }
                publish(&hub, DesktopEvent::Inventory(services.list_workflows()));
                publish(&hub, DesktopEvent::Lifecycle(StartupState::Ready));
                Some(DesktopServices {
                    services,
                    repository: WorkflowRepository::new(&paths),
                    history: snenk_bot::history::RunHistory::new(paths.history_dir()),
                    runtime: runtime
                        .spawner()
                        .expect("started application runtime has a spawner"),
                })
            }
            Err(error) => {
                startup_failed(&hub, error);
                None
            }
        }
    });
    let owner = Arc::new(ShutdownOwner {
        runtime: Mutex::new(runtime),
        closing: AtomicBool::new(false),
        done: AtomicBool::new(false),
        input: Arc::clone(&input),
        hub: Arc::clone(&hub),
    });
    if ready.is_none() {
        if let Some(runtime) = owner
            .runtime
            .lock()
            .expect("runtime ownership lock poisoned")
            .as_ref()
        {
            runtime.request_shutdown();
        }
        owner.input.shutdown().ok();
    }
    let backend = DesktopBackend::new(ready, Arc::clone(&hub), input);
    app.manage(backend);
    for window in windows.into_iter().filter(|window| window.create) {
        let built = tauri::WebviewWindowBuilder::from_config(&app, &window)
            .and_then(|builder| builder.build());
        if let Err(error) = built {
            owner.input.shutdown().ok();
            owner.finish();
            return Err(error);
        }
    }
    let handle = app.handle().clone();
    let mut updates = hub.subscribe();
    if let Some(runtime) = owner
        .runtime
        .lock()
        .expect("runtime ownership lock poisoned")
        .as_ref()
    {
        runtime
            .spawn_task("desktop updates", move |cancel| async move {
                while let Some(batch) = next_batch(&mut updates, &cancel).await {
                    handle
                        .emit_to(EventTarget::webview_window("main"), "desktop-update", batch)
                        .map_err(|error| error.to_string())?;
                }
                Ok::<(), String>(())
            })
            .expect("desktop runtime remains active before its event loop starts");
    }
    let event_owner = Arc::clone(&owner);
    let exit_code = app.run_return(move |handle, event| {
        if let RunEvent::ExitRequested { api, code, .. } = event
            && !event_owner.done.load(Ordering::Acquire)
        {
            api.prevent_exit();
            let handle = handle.clone();
            event_owner
                .begin(move |success| handle.exit(if success { code.unwrap_or(0) } else { 1 }));
        }
    });
    // Also cover a runtime/window error that bypassed ExitRequested.
    owner.input.shutdown().ok();
    if owner.finish() { Ok(exit_code) } else { Ok(1) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_is_single_flight_and_cancels_owned_work() {
        let runtime = AppRuntime::new(|_| {}).unwrap();
        let cancel = runtime.cancellation_token();
        let hub = Arc::new(DesktopHub::new());
        let input = Arc::new(DesktopInputProvider::new(|_| Ok(())));
        let owner = Arc::new(ShutdownOwner {
            runtime: Mutex::new(Some(runtime)),
            closing: AtomicBool::new(false),
            done: AtomicBool::new(false),
            input,
            hub,
        });
        let (send, receive) = std::sync::mpsc::channel();
        owner.begin(move |success| send.send(success).unwrap());
        owner.begin(|_| panic!("duplicate shutdown callback"));
        assert!(receive.recv_timeout(Duration::from_secs(2)).unwrap());
        assert!(cancel.is_cancelled());
        assert!(owner.done.load(Ordering::Acquire));
        assert!(matches!(
            owner.hub.snapshot().unwrap().startup,
            StartupState::Closing
        ));
        assert!(owner.finish());
    }

    #[tokio::test]
    async fn changed_receivers_rearm_without_periodic_polling() {
        let (sender, receiver) = tokio::sync::watch::channel(0);
        let mut waiting = tokio::task::JoinSet::new();
        wait_for_change(&mut waiting, receiver);
        sender.send_replace(1);
        let (receiver, changed) = waiting.join_next().await.unwrap().unwrap();
        assert!(changed);
        wait_for_change(&mut waiting, receiver);
        sender.send_replace(2);
        let (_, changed) = waiting.join_next().await.unwrap().unwrap();
        assert!(changed);
    }
}
