//! Desktop queries for received integration events.

use serde::{Deserialize, Serialize};
use specta::Type;

use snenk_bot::app::{AppServiceError, AppServices};
use snenk_bot::event_history::{EventEntry, EventRecord, EventSummary};

/// A bounded event list whose records are ordered newest first.
#[derive(Clone, Debug, Serialize, Type)]
pub struct EventPage {
    pub events: Vec<EventSummary>,
    pub errors: Vec<String>,
}

/// The identity of one event selected in the desktop view.
#[derive(Clone, Debug, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct EventIdentity {
    pub event_id: String,
}

/// Normal replay omits targets; selected replay supplies named recipient IDs.
#[derive(Clone, Debug, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct ReplayEvent {
    pub event_id: String,
    pub targets: Option<Vec<String>>,
}

/// Errors returned by event list, inspection, or replay commands.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum EventCommandErrorCode {
    Busy,
    InvalidRequest,
    Unavailable,
    EventUnavailable,
    CorruptRecord,
}

/// A safe desktop error with a stable code and visible message.
#[derive(Clone, Debug, Serialize, Type)]
pub struct EventCommandError {
    pub code: EventCommandErrorCode,
    pub message: String,
}

impl EventCommandError {
    /// Constructs one error at the native command boundary.
    pub fn new(code: EventCommandErrorCode, message: &str) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Reports unavailable event services without disclosing private file paths.
    pub fn unavailable() -> Self {
        Self::new(
            EventCommandErrorCode::Unavailable,
            "Events are unavailable. Try again shortly.",
        )
    }
}

/// Loads all retained event summaries without giving the view replay payloads.
pub fn list(services: &AppServices) -> Result<EventPage, EventCommandError> {
    let mut entries = services.event_entries().map_err(map_error)?;
    entries.reverse();
    let mut events = Vec::new();
    let mut errors = Vec::new();
    for entry in entries {
        match entry {
            EventEntry::Record(record) => events.push(EventSummary {
                id: record.id,
                received_at_ms: record.received_at_ms,
                source: record.source,
                name: record.name,
                replayable: record.payload.is_some(),
            }),
            EventEntry::Corrupt { file_name, message } => {
                errors.push(format!(
                    "Event file `{file_name}` is unavailable: {message}"
                ));
            }
        }
    }
    Ok(EventPage { events, errors })
}

/// Loads one selected event after the core account-boundary check.
pub fn inspect(services: &AppServices, id: &str) -> Result<EventRecord, EventCommandError> {
    services.event_record(id).map_err(map_error)
}

/// Converts filesystem and identity errors into safe desktop responses.
fn map_error(error: AppServiceError) -> EventCommandError {
    use snenk_bot::event_history::EventHistoryError;

    match error {
        AppServiceError::EventHistory(EventHistoryError::InvalidId) => EventCommandError::new(
            EventCommandErrorCode::InvalidRequest,
            "Choose a valid event ID.",
        ),
        AppServiceError::EventHistory(EventHistoryError::NotFound) => EventCommandError::new(
            EventCommandErrorCode::EventUnavailable,
            "This event is no longer available.",
        ),
        AppServiceError::EventHistory(EventHistoryError::Json(_)) => EventCommandError::new(
            EventCommandErrorCode::CorruptRecord,
            "This event record could not be read.",
        ),
        AppServiceError::InvalidReplay(_) => EventCommandError::new(
            EventCommandErrorCode::InvalidRequest,
            "This event cannot be replayed.",
        ),
        _ => EventCommandError::unavailable(),
    }
}
