//! Durable history of received integration events, separate from workflow runs.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

const DEFAULT_RETENTION: usize = 1_000;
static NEXT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// The integration that delivered an event to SnenkBot.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum EventSource {
    Twitch,
    Obs,
    VtubeStudio,
}

/// One received event. Payloads exist only where the integration can replay safely.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub struct EventRecord {
    pub id: String,
    #[cfg_attr(feature = "desktop-contracts", specta(type = specta_typescript::Number))]
    pub received_at_ms: u64,
    pub source: EventSource,
    pub name: String,
    /// A Twitch account boundary; absent for local integrations.
    pub broadcaster_id: Option<String>,
    /// Null for events that can be displayed but cannot be replayed.
    #[cfg_attr(feature = "desktop-contracts", specta(type = Option<specta_typescript::Unknown>))]
    pub payload: Option<Value>,
}

/// A compact list entry that never sends replay payloads to the view.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub struct EventSummary {
    pub id: String,
    #[cfg_attr(feature = "desktop-contracts", specta(type = specta_typescript::Number))]
    pub received_at_ms: u64,
    pub source: EventSource,
    pub name: String,
    pub replayable: bool,
}

/// Keeps malformed files visible without trusting their contents.
#[derive(Clone, Debug)]
pub enum EventEntry {
    Record(EventRecord),
    Corrupt { file_name: String, message: String },
}

/// Errors reading or writing the received-event journal.
#[derive(Debug, Error)]
pub enum EventHistoryError {
    #[error("invalid event ID")]
    InvalidId,
    #[error("event is unavailable")]
    NotFound,
    #[error("event JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("event history I/O failed at `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("system clock is before the Unix epoch")]
    Clock,
    #[error("event history worker stopped")]
    WorkerStopped,
}

/// Stores each received event in arrival order with bounded retention.
#[derive(Clone, Debug)]
pub struct EventHistory {
    directory: PathBuf,
    retention: usize,
    write_gate: Arc<Mutex<()>>,
}

impl EventHistory {
    /// Uses the default event retention under the application state directory.
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self::with_retention(directory, DEFAULT_RETENTION)
    }

    /// Creates a journal with a caller-selected record limit.
    pub fn with_retention(directory: impl Into<PathBuf>, retention: usize) -> Self {
        Self {
            directory: directory.into(),
            retention: retention.max(1),
            write_gate: Arc::new(Mutex::new(())),
        }
    }

    /// Persists one event before the listener continues to workflow routing.
    pub async fn record(
        &self,
        source: EventSource,
        name: impl Into<String>,
        broadcaster_id: Option<String>,
        payload: Option<Value>,
    ) -> Result<EventRecord, EventHistoryError> {
        let journal = self.clone();
        let name = name.into();
        tokio::task::spawn_blocking(move || {
            journal.record_blocking(source, name, broadcaster_id, payload)
        })
        .await
        .map_err(|_| EventHistoryError::WorkerStopped)?
    }

    /// Reads all retained records and reports malformed files as separate rows.
    pub fn entries(&self) -> Result<Vec<EventEntry>, EventHistoryError> {
        let mut entries = Vec::new();
        for path in self.record_files()? {
            let file_name = path.file_name().unwrap().to_string_lossy().into_owned();
            let bytes = match fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(source) => {
                    return Err(EventHistoryError::Io {
                        path: path.clone(),
                        source,
                    });
                }
            };
            match serde_json::from_slice::<EventRecord>(&bytes) {
                Ok(record) if format!("{}.json", record.id) == file_name => {
                    entries.push(EventEntry::Record(record))
                }
                Ok(_) => entries.push(EventEntry::Corrupt {
                    file_name,
                    message: "Event identity does not match its file".into(),
                }),
                Err(error) => entries.push(EventEntry::Corrupt {
                    file_name,
                    message: error.to_string(),
                }),
            }
        }
        Ok(entries)
    }

    /// Loads one validated record by its opaque ID.
    pub fn load(&self, id: &str) -> Result<EventRecord, EventHistoryError> {
        if !valid_id(id) {
            return Err(EventHistoryError::InvalidId);
        }
        let path = self.directory.join(format!("{id}.json"));
        let metadata = fs::symlink_metadata(&path).map_err(|source| {
            if source.kind() == io::ErrorKind::NotFound {
                EventHistoryError::NotFound
            } else {
                EventHistoryError::Io {
                    path: path.clone(),
                    source,
                }
            }
        })?;
        if !metadata.file_type().is_file() {
            return Err(EventHistoryError::NotFound);
        }
        let bytes = fs::read(&path).map_err(|source| {
            if source.kind() == io::ErrorKind::NotFound {
                EventHistoryError::NotFound
            } else {
                EventHistoryError::Io { path, source }
            }
        })?;
        let record: EventRecord = serde_json::from_slice(&bytes)?;
        if record.id != id {
            return Err(EventHistoryError::InvalidId);
        }
        Ok(record)
    }

    /// Persists a record and then trims the oldest files under one writer gate.
    fn record_blocking(
        &self,
        source: EventSource,
        name: String,
        broadcaster_id: Option<String>,
        payload: Option<Value>,
    ) -> Result<EventRecord, EventHistoryError> {
        let _gate = self.write_gate.lock().expect("event history lock poisoned");
        let received_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| EventHistoryError::Clock)?
            .as_millis() as u64;
        let sequence = NEXT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let id = format!(
            "{received_at_ms:013}-{sequence:016}-{}",
            Uuid::new_v4().simple()
        );
        let record = EventRecord {
            id,
            received_at_ms,
            source,
            name,
            broadcaster_id,
            payload,
        };
        fs::create_dir_all(&self.directory).map_err(|source| EventHistoryError::Io {
            path: self.directory.clone(),
            source,
        })?;
        #[cfg(unix)]
        fs::set_permissions(&self.directory, fs::Permissions::from_mode(0o700)).map_err(
            |source| EventHistoryError::Io {
                path: self.directory.clone(),
                source,
            },
        )?;
        let path = self.directory.join(format!("{}.json", record.id));
        let temp = self
            .directory
            .join(format!(".{}.tmp", Uuid::new_v4().simple()));
        let bytes = serde_json::to_vec_pretty(&record)?;
        let result = (|| -> io::Result<()> {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);
            let mut file = options.open(&temp)?;
            file.write_all(&bytes)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            fs::hard_link(&temp, &path)?;
            fs::remove_file(&temp)?;
            sync_directory(&self.directory)?;
            Ok(())
        })();
        if let Err(source) = result {
            let _ = fs::remove_file(&temp);
            return Err(EventHistoryError::Io { path, source });
        }
        self.prune()?;
        Ok(record)
    }

    /// Removes only files created with the journal's timestamped ID shape.
    fn prune(&self) -> Result<(), EventHistoryError> {
        let files = self.record_files()?;
        let remove_count = files.len().saturating_sub(self.retention);
        for path in files.into_iter().take(remove_count) {
            fs::remove_file(&path).map_err(|source| EventHistoryError::Io { path, source })?;
        }
        if remove_count > 0 {
            sync_directory(&self.directory).map_err(|source| EventHistoryError::Io {
                path: self.directory.clone(),
                source,
            })?;
        }
        Ok(())
    }

    /// Returns journal files in ID order, excluding staged writes and foreign files.
    fn record_files(&self) -> Result<Vec<PathBuf>, EventHistoryError> {
        let directory = match fs::read_dir(&self.directory) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(EventHistoryError::Io {
                    path: self.directory.clone(),
                    source,
                });
            }
        };
        let mut files = Vec::new();
        for entry in directory {
            let entry = entry.map_err(|source| EventHistoryError::Io {
                path: self.directory.clone(),
                source,
            })?;
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();
            if let Some(id) = name.strip_suffix(".json")
                && valid_id(id)
                && entry
                    .file_type()
                    .map_err(|source| EventHistoryError::Io {
                        path: entry.path(),
                        source,
                    })?
                    .is_file()
            {
                files.push(entry.path());
            }
        }
        files.sort();
        Ok(files)
    }
}

/// Checks the timestamp and UUID shape before a supplied ID reaches the filesystem.
fn valid_id(id: &str) -> bool {
    id.len() == 63
        && id.as_bytes()[..13].iter().all(u8::is_ascii_digit)
        && id.as_bytes()[13] == b'-'
        && id.as_bytes()[14..30].iter().all(u8::is_ascii_digit)
        && id.as_bytes()[30] == b'-'
        && id.as_bytes()[31..].iter().all(u8::is_ascii_hexdigit)
}

/// Syncs directory metadata where the platform supports it.
fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{EventEntry, EventHistory, EventSource};

    /// Retention preserves the newest arrival and never displays replay payloads in summaries.
    #[tokio::test]
    async fn stores_events_in_order_and_trims_oldest() {
        let directory = tempfile::tempdir().unwrap();
        let history = EventHistory::with_retention(directory.path(), 2);
        let first = history
            .record(
                EventSource::Obs,
                "RecordStateChanged",
                None,
                Some(json!({"active":true})),
            )
            .await
            .unwrap();
        let second = history
            .record(
                EventSource::Twitch,
                "channel.cheer",
                Some("123".into()),
                Some(json!({"bits":100})),
            )
            .await
            .unwrap();
        let third = history
            .record(EventSource::VtubeStudio, "model.loaded", None, None)
            .await
            .unwrap();

        assert!(matches!(
            history.load(&first.id),
            Err(super::EventHistoryError::NotFound)
        ));
        let entries = history.entries().unwrap();
        assert_eq!(entries.len(), 2);
        assert!(matches!(&entries[0], EventEntry::Record(record) if record.id == second.id));
        assert!(matches!(&entries[1], EventEntry::Record(record) if record.id == third.id));
        assert_eq!(
            history.load(&second.id).unwrap().broadcaster_id.as_deref(),
            Some("123")
        );
    }

    /// Pruning an old file between listing and reading does not hide the remaining history.
    #[tokio::test]
    async fn listing_survives_concurrent_retention_pruning() {
        let directory = tempfile::tempdir().unwrap();
        let history = EventHistory::with_retention(directory.path(), 1);
        let writer = history.clone();
        let task = tokio::spawn(async move {
            for index in 0..100 {
                writer
                    .record(EventSource::Obs, format!("event-{index}"), None, None)
                    .await
                    .unwrap();
            }
        });
        for _ in 0..100 {
            history.entries().unwrap();
            tokio::task::yield_now().await;
        }
        task.await.unwrap();
        assert_eq!(history.entries().unwrap().len(), 1);
    }

    /// File IDs cannot traverse out of the journal directory.
    #[test]
    fn rejects_unsafe_event_ids() {
        let directory = tempfile::tempdir().unwrap();
        let history = EventHistory::new(directory.path());
        assert!(matches!(
            history.load("../secrets"),
            Err(super::EventHistoryError::InvalidId)
        ));
    }
}
