//! Native administration for API consumers; bearer clients never receive these commands.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::State;

use snenk_bot::api::ConsumerPermissions;

use crate::commands::DesktopBackend;
use crate::configuration::ConfigurationError;

/// A named client grant with explicit browser origins and capability permissions.
#[derive(Default, Deserialize, Serialize, Type, snenk_bot::ConfigSchema)]
#[serde(deny_unknown_fields)]
#[config(id = "snenkbot.api.consumer", version = 2, title = "Consumer access", actions = CONSUMER_ACTIONS)]
pub struct CreateApiConsumer {
    #[config(
        id = "name",
        label = "Name",
        description = "A name you will recognize, such as Main overlay or Stream Deck.",
        introduced = 1
    )]
    pub name: String,
    #[config(id = "permissions", label = "Permissions", introduced = 1)]
    pub permissions: ConsumerPermissions,
    #[config(
        id = "allowed_origins",
        label = "Allowed browser origins",
        description = "For OBS Local file sources, add http://absolute. For files opened directly in a browser, add null. For hosted pages, add the exact address, such as http://localhost:47831, with no page path. All browser clients still need this consumer’s token. Leave empty for non-browser clients.",
        introduced = 1
    )]
    pub allowed_origins: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[config(
        id = "static_folder",
        label = "Static folder",
        description = "Optional absolute folder to serve read-only through this consumer’s token URL. Leave unset to disable file hosting.",
        directory,
        introduced = 2
    )]
    pub static_folder: Option<String>,
}

/// Native consumer operations displayed by the shared record editor.
pub const CONSUMER_ACTIONS: &[snenk_bot::schema::ConfigurationActionDescriptor] = &[
    snenk_bot::schema::ConfigurationActionDescriptor {
        id: "api.copy_hosting_url",
        label: "Copy hosting URL",
        description: "Copies the saved hosting address for OBS. Save a static folder first.",
        confirmation: None,
        success_message: "Hosting URL copied.",
    },
    snenk_bot::schema::ConfigurationActionDescriptor {
        id: "api.regenerate_token",
        label: "Regenerate token",
        description: "Replace a lost token while keeping this consumer’s settings and permissions.",
        confirmation: Some(
            "Regenerate this consumer’s token? Its old token and hosting URL will stop working. Update clients with the replacement token; the consumer’s settings and permissions will stay intact.",
        ),
        success_message: "Token regenerated. Update clients that use this consumer.",
    },
];

/// The prior overlay history envelope selected explicitly by the user for import.
#[derive(Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct ImportSupportHistory {
    pub version: u32,
    #[specta(type = Vec<specta_typescript::Unknown>)]
    pub events: Vec<serde_json::Value>,
}

/// Imports validated support events without touching the original overlay files.
#[tauri::command]
pub async fn api_history_import(
    state: State<'_, DesktopBackend>,
    request: ImportSupportHistory,
) -> Result<u32, ConfigurationError> {
    if request.version != 1 {
        return Err(ConfigurationError::new(
            crate::configuration::ConfigurationErrorCode::InvalidValues,
            "Unsupported overlay history version.",
        ));
    }
    state
        .with_api(move |api| {
            api.import_support_history(request.events)
                .map(|count| count as u32)
        })
        .await
}
