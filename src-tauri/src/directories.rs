//! Native folder selection for directory fields declared by an open configuration form.

use serde::Deserialize;
use specta::Type;
use tauri::{Runtime, State, Window};
use tokio::sync::{Semaphore, oneshot};

use crate::commands::DesktopBackend;
use crate::configuration::{ConfigurationError, ConfigurationErrorCode};

/// One picker request bound to a declared field in a live singleton or collection form.
#[derive(Clone, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct PickConfigurationDirectory {
    pub session_id: String,
    pub collection_id: Option<String>,
    pub field_path: Vec<String>,
    pub current_path: Option<String>,
}

/// Prevents concurrent native panels while leaving the application event loop responsive.
static PICKER: Semaphore = Semaphore::const_new(1);

/// Opens a folder panel on the UI thread and returns the selection without saving the form.
#[tauri::command]
pub async fn configuration_pick_directory<R: Runtime>(
    state: State<'_, DesktopBackend>,
    window: Window<R>,
    request: PickConfigurationDirectory,
) -> Result<Option<String>, ConfigurationError> {
    let label = state.directory_field(request.clone()).await?;
    let _permit = PICKER.try_acquire().map_err(|_| {
        ConfigurationError::new(
            ConfigurationErrorCode::Busy,
            "A folder picker is already open.",
        )
    })?;
    let initial = request
        .current_path
        .as_deref()
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_dir());
    let (sender, receiver) = oneshot::channel();
    let parent = window.clone();
    window
        .run_on_main_thread(move || {
            let mut dialog = rfd::AsyncFileDialog::new()
                .set_title(format!("Choose {label}"))
                .set_parent(&parent);
            if let Some(initial) = initial {
                dialog = dialog.set_directory(initial);
            }
            let selection = dialog.pick_folder();
            tauri::async_runtime::spawn(async move {
                let _ = sender.send(selection.await);
            });
        })
        .map_err(|_| ConfigurationError::unavailable())?;
    let selection = receiver
        .await
        .map_err(|_| ConfigurationError::unavailable())?;
    state.directory_field(request).await?;
    selection
        .map(|file| {
            file.path().to_str().map(str::to_owned).ok_or_else(|| {
                ConfigurationError::new(
                    ConfigurationErrorCode::InvalidValues,
                    "The selected folder path could not be represented as text.",
                )
            })
        })
        .transpose()
}
