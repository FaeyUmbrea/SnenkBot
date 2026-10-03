use std::sync::Arc;

use slint::ComponentHandle;

use crate::app::AppServices;
use crate::runtime::RuntimeSpawner;
use crate::workflows::validate_display_name;

use super::AppWindow;

/// Connects the library's creation dialog to the persisted workflow service.
pub fn connect_workflow_creation(
    window: &AppWindow,
    services: Arc<AppServices>,
    spawner: RuntimeSpawner,
) {
    let weak = window.as_weak();
    window.on_cancel_create_workflow(move || {
        if let Some(window) = weak.upgrade()
            && !window.get_create_pending()
        {
            window.set_create_visible(false);
            window.set_create_name("".into());
            window.set_create_error("".into());
        }
    });

    let weak = window.as_weak();
    window.on_create_workflow(move |name| {
        let Some(window) = weak.upgrade() else {
            return;
        };
        if window.get_create_pending() || !window.get_create_visible() || window.get_page() != 1 {
            return;
        }
        let name = name.trim().to_owned();
        if let Err(error) = validate_display_name(&name) {
            window.set_create_error(error.into());
            return;
        }
        let receiver = match services.create_workflow(&name) {
            Ok(receiver) => receiver,
            Err(error) => {
                window.set_create_error(error.to_string().into());
                return;
            }
        };
        window.set_create_pending(true);
        window.set_create_error("".into());
        let result_window = weak.clone();
        let task = spawner.spawn_task("create workflow", move |_| async move {
            let result = receiver
                .await
                .unwrap_or_else(|_| Err("workflow creator stopped before saving".into()));
            let _ = result_window.upgrade_in_event_loop(move |window| {
                window.set_create_pending(false);
                match result {
                    Ok(id) => {
                        window.set_create_visible(false);
                        window.set_create_name("".into());
                        window.set_create_error("".into());
                        window.set_selected_automation(id.clone().into());
                        if window.get_page() == 1 {
                            window.invoke_open_automation(id.into());
                        }
                    }
                    Err(error) => window.set_create_error(error.into()),
                }
            });
            Ok::<(), std::convert::Infallible>(())
        });
        if let Err(error) = task {
            window.set_create_pending(false);
            window.set_create_error(error.to_string().into());
        }
    });
}
