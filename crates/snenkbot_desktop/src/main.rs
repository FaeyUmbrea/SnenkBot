use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use snenk_bot::paths::AppPaths;
use tauri::Listener;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let smoke = std::env::args().any(|argument| argument == "--smoke-test");
    let fixture = smoke.then(|| tempfile::tempdir().expect("create isolated desktop fixture"));
    let paths = match &fixture {
        Some(directory) => Ok(AppPaths {
            config: directory.path().join("legacy/config"),
            data: directory.path().join("legacy/data"),
            state: directory.path().join("legacy/state"),
        }),
        None => AppPaths::resolve(),
    };
    if fixture.is_some() {
        let paths = paths.as_ref().expect("isolated desktop paths");
        let workflow = snenk_bot::engine::Workflow {
            id: "migration-check".into(),
            revision: 1,
            overlap: false,
            steps: Vec::new(),
            outputs: Default::default(),
        };
        let mut definition = snenk_bot::workflows::WorkflowDefinition::manual(workflow);
        definition.enabled = false;
        snenk_bot::workflows::WorkflowRepository::new(paths)
            .create_definition(&definition)
            .expect("create isolated migration fixture");
    }
    let mut context = tauri::generate_context!();
    if let Some(directory) = &fixture {
        context.config_mut().app.app_directories_override = Some(
            tauri::utils::config::AppDirectoriesOverride::Root(directory.path().join("desktop")),
        );
    }
    if smoke {
        for window in &mut context.config_mut().app.windows {
            window.visible = false;
            window.focus = false;
        }
    }
    let builder = tauri::Builder::default().setup(move |app| {
        if smoke {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Prohibited);
            let received = Arc::new(AtomicBool::new(false));
            let event_received = Arc::clone(&received);
            let handle = app.handle().clone();
            app.listen("frontend-ready", move |event| {
                event_received.store(true, Ordering::Release);
                let payload: serde_json::Value =
                    serde_json::from_str(event.payload()).unwrap_or_default();
                let ready = payload["startup"] == "ready" && payload["error"].is_null();
                eprintln!(
                    "Desktop frontend startup: {}",
                    if ready { "ready" } else { "failed" }
                );
                handle.exit(if ready { 0 } else { 1 });
            });
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(30));
                if !received.load(Ordering::Acquire) {
                    eprintln!("Desktop frontend startup timed out");
                    handle.exit(1);
                }
            });
        }
        Ok(())
    });
    let mut code = match snenkbot_desktop::host::run_with_legacy_paths(builder, context, paths) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("SnenkBot could not start: {error}");
            1
        }
    };
    if let Some(directory) = &fixture {
        let source = directory
            .path()
            .join("legacy/data/workflows/migration-check.json");
        let imported = directory
            .path()
            .join("desktop/workflows/migration-check.json");
        match (std::fs::read(source), std::fs::read(imported)) {
            (Ok(source), Ok(imported)) if source == imported => {
                eprintln!("Desktop data import: verified");
            }
            _ => {
                eprintln!("Desktop data import: failed");
                code = 1;
            }
        }
    }
    drop(fixture);
    std::process::exit(code);
}
